//! Undo (MS-280): each command that acts on mail (`mail.read`, `mail.star`, `mail.archive`,
//! `mail.trash`, `mail.restore`, `mail.tag`) and changed anything is one step, which its answer
//! names (`undo`), and `mail.undo` takes back the latest step or the one named. Taking back works
//! as the actions do (`actions.rs`): it shows at once in the listings, the counts and the tags, then
//! goes to the provider (a step over many conversations, one request per mailbox), and a refusal
//! is rolled back and reported. Moved mail goes back to the
//! mailbox it was listed in (Gmail gets that label back or leaves the trash; Outlook moves it back
//! to its folder), flags are set back on the messages the action changed, conversations the user
//! started list again once their archive is taken back, and corrected tags return to what they
//! were, synced to the server again where tags sync.
//!
//! Steps live in memory for this run, the newest `LIMIT` of them; mail deleted for good (`mail.trash
//! --yes` on mail in the trash) does not come back. Auto-read's own undo (`mail.autoread.undo`,
//! `autoread.rs`) is a different thing: a lasting log of its background passes, each conversation
//! of which can be marked unread again. Both change flags through `set_flags`.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::actions::{refs, updating};
use super::started::Marked;
use super::{Known, Mail, remember};
use crate::answers::{self, BatchFailure, UndoAction, Undone};
use crate::backend::Flag;
use crate::commands::Arguments;
use crate::model::{Conversation, MailMessage};
use crate::tagging::ConversationTag;
use crate::EngineError;

/// Steps kept; older ones can no longer be taken back.
const LIMIT: usize = 50;

/// One change an action made to one conversation, as undo needs it.
pub(super) enum Change {
    /// `flag` was set to `on` on `messages` (only those it changed).
    Flag { messages: Vec<MailMessage>, flag: Flag, on: bool },
    /// `messages` moved from the mailbox they were listed in (their key) to mailbox `to` of their
    /// account.
    Moved { messages: Vec<MailMessage>, to: String },
    /// Archive marked the threads of a conversation the user started.
    Started(Marked),
    /// A correction replaced the conversation's AI tag `before`.
    Tag { conversation: Conversation, before: ConversationTag },
}

/// What one command did, to take back.
struct Step {
    id: u64,
    action: UndoAction,
    /// Each conversation as the command found it, with what it changed there.
    conversations: Vec<(Known, Vec<Change>)>,
}

/// The steps that can be taken back, oldest first.
#[derive(Default)]
pub(super) struct Undo {
    steps: Mutex<VecDeque<Step>>,
    last: Mutex<u64>,
}

impl Mail {
    /// Keeps what a command changed as a step; answers its id, none when nothing changed.
    pub(super) fn record(&self, action: UndoAction, acted: Vec<(Known, Vec<Change>)>) -> Option<u64> {
        let conversations: Vec<(Known, Vec<Change>)> = acted.into_iter().filter(|(_, changes)| !changes.is_empty()).collect();
        if conversations.is_empty() {
            return None;
        }
        let id = {
            let mut last = self.undo.last.lock().expect("undo lock");
            *last += 1;
            *last
        };
        let mut steps = self.undo.steps.lock().expect("undo lock");
        steps.push_back(Step { id, action, conversations });
        while steps.len() > LIMIT {
            steps.pop_front();
        }
        Some(id)
    }

    /// The commands the table gives the tagger: `mail.tag`, kept as a step, and `mail.retag`.
    pub(crate) fn tagging_command(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        if command != "mail.tag" {
            return self.tagging.run(command);
        }
        let (mut result, replaced) = self.tagging.tag_command(args, |id| self.lookup(id))?;
        let acted = replaced
            .into_iter()
            .filter_map(|(conversation, before)| {
                let known = self.known(&conversation.id).ok()?;
                Some((known, vec![Change::Tag { conversation, before }]))
            })
            .collect();
        result.undo = self.record(UndoAction::Tag, acted);
        Ok(answers::json(&result))
    }

    /// `mail.undo`: takes back step `step`, else the latest, and forgets it.
    pub(super) async fn undo_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let wanted = args.int("step");
        let step = {
            let mut steps = self.undo.steps.lock().expect("undo lock");
            match wanted {
                Some(id) => steps.iter().position(|step| step.id as i64 == id).and_then(|at| steps.remove(at)),
                None => steps.pop_back(),
            }
        };
        let Some(step) = step else {
            return Err(match wanted {
                Some(id) => EngineError::new("not_found", format!("Step {id} can no longer be undone.")),
                None => EngineError::new("nothing", "Nothing to undo."),
            });
        };
        // Tags and the marks of conversations the user started, each conversation's own.
        let local: Vec<Result<(), EngineError>> = step.conversations.iter().map(|(_, changes)| self.take_back_here(changes)).collect();
        // The flags and moves of every conversation at once, one request per mailbox.
        let all = || step.conversations.iter().flat_map(|(_, changes)| changes);
        let mut flags: Vec<(Flag, bool)> = Vec::new();
        for change in all() {
            if let Change::Flag { flag, on, .. } = change
                && !flags.contains(&(*flag, *on))
            {
                flags.push((*flag, *on));
            }
        }
        let flagged = flags.into_iter().map(|(flag, on)| {
            let messages: Vec<MailMessage> = all()
                .filter_map(|change| match change {
                    Change::Flag { messages, flag: changed, on: was } if (*changed, *was) == (flag, on) => Some(messages),
                    _ => None,
                })
                .flatten()
                .cloned()
                .collect();
            async move {
                if flag == Flag::Seen {
                    self.adjust_unread(&messages, if on { 1 } else { -1 });
                }
                self.set_flags(messages, flag, !on).await
            }
        });
        let moves: Vec<(Vec<MailMessage>, String)> = all()
            .filter_map(|change| match change {
                Change::Moved { messages, to } => Some((messages.clone(), to.clone())),
                _ => None,
            })
            .collect();
        let (flagged, moved) = tokio::join!(futures::future::join_all(flagged), self.move_all(&moves, true));

        let mut done: Vec<String> = Vec::new();
        let mut failed: Vec<BatchFailure> = Vec::new();
        for ((known, changes), local) in step.conversations.iter().zip(local) {
            let id = &known.conversation.id;
            let result = local.and_then(|()| {
                for change in changes {
                    match change {
                        Change::Flag { messages, .. } => flagged.iter().try_for_each(|refusals| refusals.of(messages))?,
                        Change::Moved { messages, .. } => moved.of(messages)?,
                        Change::Started(_) | Change::Tag { .. } => {}
                    }
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    self.remember_taken_back(known, changes);
                    done.push(id.clone());
                }
                Err(error) => failed.push(BatchFailure::of(id, &error)),
            }
        }
        Ok(answers::json(&Undone { step: step.id, action: step.action, done, failed }))
    }

    /// Takes back what one conversation's changes did here alone: the marks of a conversation
    /// the user started, and tags.
    fn take_back_here(&self, changes: &[Change]) -> Result<(), EngineError> {
        for change in changes.iter().rev() {
            match change {
                Change::Started(marked) => self.unarchive_started(marked),
                Change::Tag { conversation, before } => {
                    self.tagging.put_back(conversation, before.clone()).map_err(EngineError::failed)?
                }
                Change::Flag { .. } | Change::Moved { .. } => {}
            }
        }
        Ok(())
    }

    /// What commands remember of a conversation taken back: its flags as they are again, and
    /// one a move took out of the lists (forgotten then) as it was.
    fn remember_taken_back(&self, known: &Known, changes: &[Change]) {
        let mut state = self.state();
        let Some(current) = state.known.get_mut(&known.conversation.id) else {
            if changes.iter().any(|change| matches!(change, Change::Moved { .. } | Change::Started(_))) {
                remember(&mut state, &known.conversation, &known.primary);
            }
            return;
        };
        for change in changes {
            if let Change::Flag { messages, flag, on } = change {
                current.conversation = updating(&current.conversation, &refs(messages), |envelope| envelope.set_flag(*flag, !on));
            }
        }
    }
}
