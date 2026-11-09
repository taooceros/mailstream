//! Read, star, archive, trash and restore, as the Swift engine's `MailStore` mutations and their
//! commands do them (`Store/MailStore.swift`, `Control/ControlService.swift`): each change shows
//! at once in the listings (and mail.db and the unread counts), then goes to the provider; a
//! refused flag change is rolled back, a refused move fetches the lists afresh, and either is
//! reported. Commands take conversation ids and list which ones failed rather than stopping.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{Known, Mail};
use crate::api::ApiError;
use crate::backend::{Backend, Flag};
use crate::cache::CachedListing;
use crate::commands::Arguments;
use crate::model::{Conversation, Envelope, MailMessage, Mailbox, MailboxKey, MailboxRole, MessageRef};
use crate::EngineError;

/// What a command keeps of a conversation it acted on.
enum Outcome {
    Remember(Conversation),
    Forget,
}

impl Envelope {
    fn set_flag(&mut self, flag: Flag, on: bool) {
        let name = match flag {
            Flag::Seen => "seen",
            Flag::Flagged => "flagged",
        };
        if on {
            self.flags.insert(name.into());
        } else {
            self.flags.remove(name);
        }
    }
}

/// The same thread with the envelopes of `refs` changed in place (flags).
fn updating(conversation: &Conversation, refs: &HashSet<MessageRef>, change: impl Fn(&mut Envelope)) -> Conversation {
    let messages = conversation
        .messages
        .iter()
        .map(|message| {
            if !refs.contains(&message.message_ref()) {
                return message.clone();
            }
            let mut envelope = (*message.envelope).clone();
            change(&mut envelope);
            MailMessage { key: message.key.clone(), envelope: Arc::new(envelope), role: message.role }
        })
        .collect();
    Conversation::new(conversation.id.clone(), conversation.account.clone(), messages)
}

fn refs(messages: &[MailMessage]) -> HashSet<MessageRef> {
    messages.iter().map(MailMessage::message_ref).collect()
}

/// Message ids by the mailbox they list under.
fn by_mailbox(messages: &[MailMessage]) -> Vec<(MailboxKey, Vec<String>)> {
    let mut groups: Vec<(MailboxKey, Vec<String>)> = Vec::new();
    for message in messages {
        let key = (*message.key).clone();
        match groups.iter_mut().find(|(known, _)| *known == key) {
            Some((_, ids)) => ids.push(message.id().to_string()),
            None => groups.push((key, vec![message.id().to_string()])),
        }
    }
    groups
}

/// The first failure of `results`, after all of them finished.
fn first_failure(results: Vec<Result<(), EngineError>>) -> Result<(), EngineError> {
    results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
}

impl Mail {
    /// `mail.read`, `mail.star`, `mail.archive`, `mail.trash` and `mail.restore`:
    /// `{"done": [id…], "failed": [{id, code, message}]}` over the request's ids.
    pub(super) async fn act(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        let ids = args.list("ids").unwrap_or_default();
        if ids.is_empty() {
            return Err(EngineError::usage("Give at least one conversation id."));
        }
        let mut done: Vec<Value> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        for id in ids {
            let outcome = match self.known(&id) {
                Ok(known) => self.act_on(command, args, known).await,
                Err(error) => Err(error),
            };
            match outcome {
                Ok(outcome) => {
                    let mut state = self.state();
                    match outcome {
                        Outcome::Remember(conversation) => {
                            if let Some(known) = state.known.get_mut(&id) {
                                known.conversation = conversation;
                            }
                        }
                        Outcome::Forget => {
                            if state.known.remove(&id).is_some() {
                                state.known_order.retain(|known| *known != id);
                            }
                        }
                    }
                    done.push(json!(id));
                }
                Err(error) => failed.push(json!({ "id": id, "code": error.code, "message": error.message })),
            }
        }
        Ok(json!({ "done": done, "failed": failed }))
    }

    async fn act_on(self: &Arc<Self>, command: &str, args: &Arguments, known: Known) -> Result<Outcome, EngineError> {
        let conversation = &known.conversation;
        match command {
            "mail.read" => {
                let unread = args.bool("unread");
                self.set_read(conversation, !unread).await?;
                let all = refs(&conversation.messages);
                Ok(Outcome::Remember(updating(conversation, &all, |envelope| envelope.set_flag(Flag::Seen, !unread))))
            }
            "mail.star" => {
                let off = args.bool("off");
                self.set_flagged(conversation, !off).await?;
                let changed =
                    if off { refs(&conversation.messages) } else { refs(std::slice::from_ref(conversation.latest())) };
                Ok(Outcome::Remember(updating(conversation, &changed, |envelope| envelope.set_flag(Flag::Flagged, !off))))
            }
            "mail.archive" => {
                self.archive(conversation, &known.primary).await?;
                Ok(Outcome::Forget)
            }
            "mail.trash" => {
                let trash = Self::mailbox_with_role(&self.state(), &conversation.account, MailboxRole::Trash).cloned();
                if let Some(trash) = trash
                    && conversation.messages.iter().any(|message| message.mailbox() == trash.id)
                    && !args.bool("yes")
                {
                    return Err(EngineError::new(
                        "confirm",
                        format!("{} is already in the trash, so it would be deleted for good. Re-run with --yes.", conversation.id),
                    ));
                }
                self.trash(conversation).await?;
                Ok(Outcome::Forget)
            }
            "mail.restore" => {
                self.restore(conversation).await?;
                Ok(Outcome::Forget)
            }
            _ => unreachable!("{command} is an action"),
        }
    }

    // MARK: - Mutations

    /// Marks every unread message read, or the latest message not sent by the user unread.
    async fn set_read(self: &Arc<Self>, conversation: &Conversation, read: bool) -> Result<(), EngineError> {
        let targets = Self::read_targets(conversation, read);
        if targets.is_empty() {
            return Ok(());
        }
        self.adjust_unread(&targets, if read { -1 } else { 1 });
        self.mutate_flags(targets, Flag::Seen, read).await
    }

    /// The messages `set_read` changes.
    fn read_targets(conversation: &Conversation, read: bool) -> Vec<MailMessage> {
        if read {
            conversation.messages.iter().filter(|message| !message.envelope.is_seen()).cloned().collect()
        } else {
            let latest = conversation.messages.iter().rev().find(|message| message.role != MailboxRole::Sent);
            vec![latest.unwrap_or_else(|| conversation.latest()).clone()]
        }
    }

    /// `mail.read` over several conversations with one request per mailbox (auto-read and its
    /// undo); the conversations commands handed out follow, as `act` keeps them.
    pub(super) async fn set_read_all(self: &Arc<Self>, conversations: &[Conversation], read: bool) -> Result<(), EngineError> {
        let targets: Vec<MailMessage> =
            conversations.iter().flat_map(|conversation| Self::read_targets(conversation, read)).collect();
        if !targets.is_empty() {
            self.adjust_unread(&targets, if read { -1 } else { 1 });
            self.mutate_flags(targets, Flag::Seen, read).await?;
        }
        let mut state = self.state();
        for conversation in conversations {
            if let Some(known) = state.known.get_mut(&conversation.id) {
                let all = refs(&conversation.messages);
                known.conversation = updating(conversation, &all, |envelope| envelope.set_flag(Flag::Seen, read));
            }
        }
        Ok(())
    }

    /// Stars the latest message, or unstars every starred one.
    async fn set_flagged(self: &Arc<Self>, conversation: &Conversation, flagged: bool) -> Result<(), EngineError> {
        let targets: Vec<MailMessage> = if flagged {
            vec![conversation.latest().clone()]
        } else {
            conversation.messages.iter().filter(|message| message.envelope.is_flagged()).cloned().collect()
        };
        self.mutate_flags(targets, Flag::Flagged, flagged).await
    }

    async fn mutate_flags(self: &Arc<Self>, targets: Vec<MailMessage>, flag: Flag, add: bool) -> Result<(), EngineError> {
        let changed = refs(&targets);
        self.update_envelopes(&changed, |envelope| envelope.set_flag(flag, add));
        let requests = by_mailbox(&targets).into_iter().map(|(key, ids)| {
            let changed = &changed;
            self.provider_request(
                key.account,
                false,
                async move |backend: Arc<Backend>| backend.set_flag(flag, add, &ids).await,
                move || self.update_envelopes(changed, |envelope| envelope.set_flag(flag, !add)),
            )
        });
        first_failure(futures::future::join_all(requests).await)
    }

    /// One request for a change already shown. A refusal (signing out included) is rolled back,
    /// reported, and with `resync` the lists and counts are fetched afresh.
    pub(super) async fn provider_request(
        self: &Arc<Self>,
        account: String,
        resync: bool,
        call: impl AsyncFnOnce(Arc<Backend>) -> Result<(), ApiError>,
        rollback: impl FnOnce(),
    ) -> Result<(), EngineError> {
        let result = match self.backend(&account) {
            Some(backend) => call(backend).await,
            None => Err(ApiError::new(0, "The account is no longer signed in.")),
        };
        let Err(error) = result else { return Ok(()) };
        rollback();
        self.report(&error, Some(&account));
        if resync {
            self.clone().refresh();
            self.refresh_counts();
        }
        Err(EngineError::failed(error.message))
    }

    /// Destination of "Archive": the archive folder, else Gmail's All Mail.
    fn archive_mailbox(&self, account: &str) -> Option<Mailbox> {
        let state = self.state();
        Self::mailbox_with_role(&state, account, MailboxRole::Archive)
            .or_else(|| Self::mailbox_with_role(&state, account, MailboxRole::All))
            .cloned()
    }

    /// Moves the messages listed from `primary` that sit in an inbox-like mailbox to the archive.
    async fn archive(self: &Arc<Self>, conversation: &Conversation, primary: &HashSet<MailboxKey>) -> Result<(), EngineError> {
        let Some(target) = self.archive_mailbox(&conversation.account) else {
            return Err(EngineError::failed(format!("{} has no archive mailbox.", conversation.account)));
        };
        let targets: Vec<MailMessage> = conversation
            .messages
            .iter()
            .filter(|message| {
                primary.contains(&*message.key)
                    && !matches!(
                        message.role,
                        MailboxRole::Sent | MailboxRole::All | MailboxRole::Archive | MailboxRole::Trash | MailboxRole::Drafts
                    )
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            return Err(EngineError::failed(format!("Nothing in {} is in a mailbox Archive moves from.", conversation.id)));
        }
        self.move_messages(targets, &target).await
    }

    /// Moves the conversation to the trash; messages already there are deleted for good.
    async fn trash(self: &Arc<Self>, conversation: &Conversation) -> Result<(), EngineError> {
        let trash = Self::mailbox_with_role(&self.state(), &conversation.account, MailboxRole::Trash).cloned();
        let Some(trash) = trash else {
            let error = ApiError::new(0, "No trash mailbox found.");
            self.report(&error, Some(&conversation.account));
            return Err(EngineError::failed(error.message));
        };
        let (in_trash, elsewhere): (Vec<MailMessage>, Vec<MailMessage>) =
            conversation.messages.iter().cloned().partition(|message| message.mailbox() == trash.id);
        let elsewhere: Vec<MailMessage> = elsewhere.into_iter().filter(|message| message.role != MailboxRole::Drafts).collect();
        let moving = self.move_messages(elsewhere, &trash);
        let deleting = async {
            if in_trash.is_empty() {
                return Ok(());
            }
            self.remove_envelopes(&in_trash);
            let ids: Vec<String> = in_trash.iter().map(|message| message.id().to_string()).collect();
            self.provider_request(trash.account.clone(), true, async move |backend: Arc<Backend>| backend.delete(&ids).await, || {})
                .await
        };
        let (moved, deleted) = tokio::join!(moving, deleting);
        first_failure(vec![moved, deleted])
    }

    /// Moves the conversation's messages in the trash or junk to the inbox.
    async fn restore(self: &Arc<Self>, conversation: &Conversation) -> Result<(), EngineError> {
        let inbox = Self::mailbox_with_role(&self.state(), &conversation.account, MailboxRole::Inbox).cloned();
        let Some(inbox) = inbox else {
            return Err(EngineError::failed(format!("{} has no inbox.", conversation.account)));
        };
        let targets: Vec<MailMessage> = conversation
            .messages
            .iter()
            .filter(|message| matches!(message.role, MailboxRole::Trash | MailboxRole::Junk))
            .cloned()
            .collect();
        if targets.is_empty() {
            return Err(EngineError::failed(format!("Nothing in {} is in the trash or junk.", conversation.id)));
        }
        self.move_messages(targets, &inbox).await
    }

    async fn move_messages(self: &Arc<Self>, targets: Vec<MailMessage>, destination: &Mailbox) -> Result<(), EngineError> {
        let targets: Vec<MailMessage> = targets.into_iter().filter(|message| message.mailbox() != destination.id).collect();
        if targets.is_empty() {
            return Ok(());
        }
        self.remove_envelopes(&targets);
        let requests = by_mailbox(&targets).into_iter().map(|(key, ids)| {
            let to = destination.id.clone();
            self.provider_request(
                key.account.clone(),
                true,
                async move |backend: Arc<Backend>| backend.move_to(&ids, &key.mailbox, &to).await,
                || {},
            )
        });
        first_failure(futures::future::join_all(requests).await)
    }

    // MARK: - Listings

    /// Flags and tags never change threading: the listed envelopes change in place.
    pub(super) fn update_envelopes(&self, refs: &HashSet<MessageRef>, change: impl Fn(&mut Envelope)) {
        let mut ids: HashMap<MailboxKey, HashSet<&str>> = HashMap::new();
        for reference in refs {
            ids.entry(reference.key()).or_default().insert(reference.id.as_str());
        }
        let mut state = self.state();
        for (key, ids) in ids {
            let Some(listing) = state.listings.get_mut(&key) else { continue };
            for envelope in listing.envelopes.iter_mut().filter(|envelope| ids.contains(envelope.id.as_str())) {
                let mut changed = (**envelope).clone();
                change(&mut changed);
                *envelope = Arc::new(changed);
            }
            self.cache.save_listing(CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() }, &key);
        }
        drop(state);
        self.events.emit_soon("conversations");
    }

    /// Drops the messages from their listings and their unread ones from their mailboxes' counts.
    fn remove_envelopes(&self, targets: &[MailMessage]) {
        let unseen: Vec<MailMessage> = targets.iter().filter(|message| !message.envelope.is_seen()).cloned().collect();
        self.adjust_unread(&unseen, -1);
        let mut state = self.state();
        for (key, ids) in by_mailbox(targets) {
            let Some(listing) = state.listings.get_mut(&key) else { continue };
            listing.envelopes.retain(|envelope| !ids.contains(&envelope.id));
            self.cache.save_listing(CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() }, &key);
        }
        drop(state);
        self.events.emit_soon("conversations");
    }

    fn adjust_unread(&self, targets: &[MailMessage], delta: i64) {
        let mut changed = false;
        let mut state = self.state();
        for (key, ids) in by_mailbox(targets) {
            let Some(mailbox) = state
                .mailboxes
                .get_mut(&key.account)
                .and_then(|list| list.iter_mut().find(|mailbox| mailbox.id == key.mailbox))
            else {
                continue;
            };
            if let Some(unread) = mailbox.unread {
                mailbox.unread = Some((unread + delta * ids.len() as i64).max(0));
                changed = true;
            }
        }
        drop(state);
        if changed {
            self.events.emit_soon("mailboxes");
        }
    }
}
