//! Conversations the user started (MS-270): the inbox lists a conversation once the user is in
//! it, so mail they wrote that answers nothing (its first message has no `In-Reply-To`) lists in
//! the inbox on its own until someone answers. Their replies join the conversation they answer
//! through the threader's context mail, as before.
//!
//! Sent mail alone can't tell whether a conversation was answered and archived since: the answer
//! is out of the inbox and out of the loaded listings. So before one lists, one request per
//! account asks the provider for the threads (Gmail's thread, Outlook's conversation) of every
//! such conversation not asked about yet, and lists only those holding nothing but the user's
//! own mail (Sent and Drafts); ones listed are asked about again after `RECHECK`. Archive takes a
//! conversation the user started out of the inbox for good, though the provider has nothing to
//! move, and keeps what the user sent in an archived conversation from listing as one they
//! started: that is remembered here. What is known lives in mail.db (`startedThreads`), so a launch
//! lists them before any request.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Mail, State};
use crate::model::{Conversation, Date, MailboxKey, MailboxRole, now};

/// How long a thread found unanswered lists before the provider is asked again.
const RECHECK: f64 = 10.0 * 60.0;
/// How long after a failed request the provider is asked again.
const RETRY: f64 = 60.0;

/// A conversation the user started, by account and provider thread.
pub(super) type ThreadKey = (String, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Status {
    /// Only the user's own mail: it lists in the inbox.
    Unanswered,
    /// Someone answered; the answer lists it when it is in the inbox.
    Answered,
    /// The user archived it.
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    account: String,
    thread: String,
    status: Status,
    checked: Date,
}

/// What is known about the conversations the user started.
#[derive(Debug, Default)]
pub(super) struct Started {
    pub(super) threads: HashMap<ThreadKey, (Status, Date)>,
    checking: bool,
    /// No request before this, after one failed.
    retry_at: Option<Date>,
}

impl Started {
    /// Whether the inbox lists the conversation of `key`; `due` gets it when the provider is to
    /// be asked about it, which it can't be while the thread is still this engine's own
    /// (`pending`).
    pub(super) fn lists(&self, key: &ThreadKey, due: &mut Vec<ThreadKey>) -> bool {
        let known = self.threads.get(key);
        if !super::pending::is_local(&key.1)
            && known.is_none_or(|(status, checked)| *status == Status::Unanswered && now() - checked > RECHECK)
        {
            due.push(key.clone());
        }
        known.is_some_and(|(status, _)| *status == Status::Unanswered)
    }
}

/// What was known of the threads archive marked, before it did (`Mail::archive_started`).
#[derive(Clone, Debug)]
pub(super) struct Marked(Vec<(ThreadKey, Option<(Status, Date)>)>);

/// The thread of a conversation of the user's own sent mail alone, the first of which answers
/// nothing.
pub(super) fn started_key(conversation: &Conversation) -> Option<ThreadKey> {
    let first = conversation.messages.first()?;
    if !conversation.messages.iter().all(|message| message.role == MailboxRole::Sent) || !first.envelope.in_reply_to.is_empty() {
        return None;
    }
    Some((first.account().to_string(), first.envelope.thread.clone()?))
}

/// Listed from mailboxes other than `primary` only as a conversation the user started.
pub(super) fn is_started(conversation: &Conversation, primary: &HashSet<MailboxKey>) -> bool {
    started_key(conversation).is_some() && !conversation.messages.iter().any(|message| primary.contains(&message.key))
}

impl Mail {
    /// Reads what mail.db knows.
    pub(super) async fn restore_started(&self) {
        let saved: Vec<Saved> = self.cache.load_started_threads().await.unwrap_or_default();
        let mut state = self.state();
        for item in saved {
            state.started.threads.entry((item.account, item.thread)).or_insert((item.status, item.checked));
        }
    }

    /// Asks the providers about `due` in the background, unless a request is running or failed
    /// a moment ago; the inbox lists again once one turns out unanswered or answered.
    pub(super) fn check_started(self: &Arc<Self>, due: Vec<ThreadKey>) {
        if due.is_empty() {
            return;
        }
        {
            let mut state = self.state();
            let started = &mut state.started;
            if started.checking || started.retry_at.is_some_and(|at| now() < at) {
                return;
            }
            started.checking = true;
        }
        let mail = self.clone();
        tokio::spawn(async move {
            let (answers, changed) = mail.ask_started(due).await;
            let mut state = mail.state();
            state.started.checking = false;
            if answers {
                mail.save_started(&state);
            }
            if changed {
                mail.conversations_changed();
            }
        });
    }

    /// One request per account; whether any answered, and whether a conversation's listing
    /// changed.
    async fn ask_started(self: &Arc<Self>, due: Vec<ThreadKey>) -> (bool, bool) {
        let mut by_account: HashMap<String, Vec<String>> = HashMap::new();
        for (account, thread) in due {
            let threads = by_account.entry(account).or_default();
            if !threads.contains(&thread) {
                threads.push(thread);
            }
        }
        let asks = by_account.into_iter().filter_map(|(account, threads)| {
            let backend = self.backend(&account)?;
            let own: Vec<String> = {
                let state = self.state();
                [MailboxRole::Sent, MailboxRole::Drafts]
                    .into_iter()
                    .filter_map(|role| Self::mailbox_with_role(&state, &account, role).map(|mailbox| mailbox.id.clone()))
                    .collect()
            };
            Some(async move {
                let answered = backend.answered(&threads, &own).await;
                (account, threads, answered)
            })
        });
        let results = futures::future::join_all(asks).await;
        let mut state = self.state();
        let (mut answers, mut changed) = (false, false);
        for (account, threads, answered) in results {
            let Ok(answered) = answered else {
                state.started.retry_at = Some(now() + RETRY);
                continue;
            };
            answers = true;
            for thread in threads {
                let status = if answered.contains(&thread) { Status::Answered } else { Status::Unanswered };
                let key = (account.clone(), thread);
                let before = state.started.threads.get(&key).map(|(status, _)| *status);
                if before == Some(Status::Archived) {
                    continue;
                }
                // Only unanswered ones list.
                changed |= (before == Some(Status::Unanswered)) != (status == Status::Unanswered);
                state.started.threads.insert(key, (status, now()));
            }
        }
        (answers, changed)
    }

    /// Archive: the user's own mail left of `conversation` (its threads) no longer lists in the
    /// inbox as a conversation they started. Answers what was known of those threads before, for
    /// undo (`unarchive_started`).
    pub(super) fn archive_started(&self, conversation: &Conversation) -> Marked {
        let mut state = self.state();
        let mut before = Vec::new();
        for message in &conversation.messages {
            if let Some(thread) = &message.envelope.thread {
                let key = (message.account().to_string(), thread.clone());
                let known = state.started.threads.insert(key.clone(), (Status::Archived, now()));
                if !before.iter().any(|(marked, _)| *marked == key) {
                    before.push((key, known));
                }
            }
        }
        self.save_started(&state);
        drop(state);
        self.conversations_changed();
        Marked(before)
    }

    /// Undoes `archive_started`: the threads it marked are known as they were before.
    pub(super) fn unarchive_started(&self, marked: &Marked) {
        let mut state = self.state();
        for (key, known) in &marked.0 {
            match known {
                Some(known) => state.started.threads.insert(key.clone(), *known),
                None => state.started.threads.remove(key),
            };
        }
        self.save_started(&state);
        drop(state);
        self.conversations_changed();
    }

    /// Writes what is known to mail.db: archived threads, and the others still in a loaded Sent
    /// listing.
    pub(super) fn save_started(&self, state: &State) {
        let loaded: HashSet<(&str, &str)> = state
            .listings
            .iter()
            .filter(|(key, _)| Self::role(state, key) == MailboxRole::Sent)
            .flat_map(|(key, listing)| {
                listing.envelopes.iter().filter_map(|envelope| Some((key.account.as_str(), envelope.thread.as_deref()?)))
            })
            .collect();
        let saved: Vec<Saved> = state
            .started
            .threads
            .iter()
            .filter(|((account, thread), (status, _))| {
                *status == Status::Archived || loaded.contains(&(account.as_str(), thread.as_str()))
            })
            .map(|((account, thread), (status, checked))| Saved {
                account: account.clone(),
                thread: thread.clone(),
                status: *status,
                checked: *checked,
            })
            .collect();
        self.cache.save_started_threads(&saved);
    }
}
