//! Read, star, archive, trash and restore, as the Swift engine's `MailStore` mutations and their
//! commands do them (`Store/MailStore.swift`, `Control/ControlService.swift`): each change shows
//! at once in the listings (and mail.db and the unread counts), then goes to the provider; a
//! refused flag change is rolled back, a refused move fetches the lists afresh, and either is
//! reported. Commands take conversation ids and list which ones failed rather than stopping.
//! However many conversations a command names (a multi-selection, MS-305), it asks the provider
//! once per mailbox and destination, not once per conversation: a refused request fails every
//! conversation that had messages in it. Each command that changed anything is one step
//! `mail.undo` can take back (`undo.rs`).
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::undo::Change;
use super::{Known, Mail, pending, started};
use crate::answers::{self, BatchFailure, BatchResult, UndoAction};
use crate::api::ApiError;
use crate::backend::{Backend, Flag};
use crate::cache::CachedListing;
use crate::commands::Arguments;
use crate::model::{Conversation, DISTANT_PAST, Envelope, MailMessage, Mailbox, MailboxKey, MailboxRole, MessageRef};
use crate::EngineError;

/// One conversation's part of an action, before anything is asked of the provider.
struct Plan {
    known: Known,
    /// The messages whose flag changes (read, star).
    flags: Vec<MailMessage>,
    /// The messages that move, and the id of the mailbox they go to.
    moves: Option<(Vec<MailMessage>, String)>,
    /// The messages deleted for good.
    deletes: Vec<MailMessage>,
}

/// `targets` not in `destination` yet, moving there; none when all are.
fn moving_to(targets: Vec<MailMessage>, destination: &Mailbox) -> Option<(Vec<MailMessage>, String)> {
    let targets: Vec<MailMessage> = targets.into_iter().filter(|message| message.mailbox() != destination.id).collect();
    (!targets.is_empty()).then(|| (targets, destination.id.clone()))
}

/// The request for mailbox `key`'s `ids` carries `message`.
fn carried(message: &MailMessage, key: &MailboxKey, ids: &[String]) -> bool {
    *message.key == *key && ids.iter().any(|id| id == message.id())
}

/// The requests of a change to many conversations at once (one per mailbox, and destination)
/// the provider refused, with the messages each carried, so each conversation learns how its
/// part went.
#[derive(Default)]
pub(super) struct Refusals(Vec<(MailboxKey, Vec<String>, EngineError)>);

impl Refusals {
    /// The refusal of the first request that carried one of `messages`.
    pub(super) fn of(&self, messages: &[MailMessage]) -> Result<(), EngineError> {
        let refused = self.0.iter().find(|(key, ids, _)| messages.iter().any(|message| carried(message, key, ids)));
        refused.map_or(Ok(()), |(_, _, error)| Err(error.clone()))
    }

    /// The first refusal.
    pub(super) fn first(self) -> Result<(), EngineError> {
        self.0.into_iter().next().map_or(Ok(()), |(_, _, error)| Err(error))
    }
}

impl Envelope {
    pub(super) fn set_flag(&mut self, flag: Flag, on: bool) {
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
pub(super) fn updating(conversation: &Conversation, refs: &HashSet<MessageRef>, change: impl Fn(&mut Envelope)) -> Conversation {
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

pub(super) fn refs(messages: &[MailMessage]) -> HashSet<MessageRef> {
    messages.iter().map(MailMessage::message_ref).collect()
}

/// Message ids by the mailbox they list under; mail still being sent from here (`pending`) is
/// not the provider's to change yet.
fn by_mailbox(messages: &[MailMessage]) -> Vec<(MailboxKey, Vec<String>)> {
    let mut groups: Vec<(MailboxKey, Vec<String>)> = Vec::new();
    for message in messages.iter().filter(|message| !pending::is_local(message.id())) {
        let key = (*message.key).clone();
        match groups.iter_mut().find(|(known, _)| *known == key) {
            Some((_, ids)) => ids.push(message.id().to_string()),
            None => groups.push((key, vec![message.id().to_string()])),
        }
    }
    groups
}

/// Of `targets`, those whose `flag` is not `on` yet: what setting it changes, for undo.
fn flag_change(targets: &[MailMessage], flag: Flag, on: bool) -> Vec<Change> {
    let messages: Vec<MailMessage> = targets
        .iter()
        .filter(|message| {
            let set = match flag {
                Flag::Seen => message.envelope.is_seen(),
                Flag::Flagged => message.envelope.is_flagged(),
            };
            set != on
        })
        .cloned()
        .collect();
    if messages.is_empty() { Vec::new() } else { vec![Change::Flag { messages, flag, on }] }
}

impl Mail {
    /// `mail.read`, `mail.star`, `mail.archive`, `mail.trash` and `mail.restore`:
    /// `{"done": [id…], "failed": [{id, code, message}], "undo": step}` over the request's ids,
    /// `undo` naming the step `mail.undo` takes the command back with when it changed anything.
    pub(super) async fn act(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        let ids = args.list("ids").unwrap_or_default();
        if ids.is_empty() {
            return Err(EngineError::usage("Give at least one conversation id."));
        }
        let action = match command {
            "mail.read" if args.bool("unread") => UndoAction::Unread,
            "mail.read" => UndoAction::Read,
            "mail.star" if args.bool("off") => UndoAction::Unstar,
            "mail.star" => UndoAction::Star,
            "mail.archive" => UndoAction::Archive,
            "mail.trash" => UndoAction::Trash,
            _ => UndoAction::Restore,
        };
        let plans: Vec<(String, Result<Plan, EngineError>)> = ids
            .into_iter()
            .map(|id| {
                let plan = self.known(&id).and_then(|known| self.plan(action, args, known));
                (id, plan)
            })
            .collect();

        // Every conversation's requests at once.
        let planned = || plans.iter().filter_map(|(_, plan)| plan.as_ref().ok());
        let flagged: Vec<MailMessage> = planned().flat_map(|plan| plan.flags.iter().cloned()).collect();
        let moves: Vec<(Vec<MailMessage>, String)> = planned().filter_map(|plan| plan.moves.clone()).collect();
        let deleted: Vec<MailMessage> = planned().flat_map(|plan| plan.deletes.iter().cloned()).collect();
        let flag = match action {
            UndoAction::Read => Some((Flag::Seen, true)),
            UndoAction::Unread => Some((Flag::Seen, false)),
            UndoAction::Star => Some((Flag::Flagged, true)),
            UndoAction::Unstar => Some((Flag::Flagged, false)),
            _ => None,
        };
        // What setting the flags changes, before they are set.
        let flag_changes: Vec<Vec<Change>> = plans
            .iter()
            .map(|(_, plan)| match (plan, flag) {
                (Ok(plan), Some((flag, on))) => flag_change(&plan.flags, flag, on),
                _ => Vec::new(),
            })
            .collect();
        let flags = async {
            let Some((flag, on)) = flag.filter(|_| !flagged.is_empty()) else { return Refusals::default() };
            if flag == Flag::Seen {
                self.adjust_unread(&flagged, if on { -1 } else { 1 });
            }
            self.set_flags(flagged.clone(), flag, on).await
        };
        let (flags, moved, deleted) = tokio::join!(flags, self.move_all(&moves, false), self.delete_all(&deleted));

        let mut done: Vec<String> = Vec::new();
        let mut failed: Vec<BatchFailure> = Vec::new();
        let mut acted: Vec<(Known, Vec<Change>)> = Vec::new();
        for ((id, plan), mut changes) in plans.into_iter().zip(flag_changes) {
            let plan = plan.and_then(|plan| {
                flags.of(&plan.flags)?;
                moved.of(plan.moves.as_ref().map_or(&[][..], |(messages, _)| messages))?;
                deleted.of(&plan.deletes)?;
                Ok(plan)
            });
            let plan = match plan {
                Ok(plan) => plan,
                Err(error) => {
                    failed.push(BatchFailure::of(&id, &error));
                    continue;
                }
            };
            let conversation = &plan.known.conversation;
            if let Some((messages, to)) = &plan.moves {
                changes.push(Change::Moved { messages: messages.clone(), to: to.clone() });
            }
            if action == UndoAction::Archive {
                changes.push(Change::Started(self.archive_started(conversation)));
            }
            let kept = match flag {
                Some((flag, on)) => {
                    // Read and unread show on the whole conversation; a star on the message starred.
                    let shown = if flag == Flag::Seen || !on { &conversation.messages[..] } else { &plan.flags[..] };
                    Some(updating(conversation, &refs(shown), |envelope| envelope.set_flag(flag, on)))
                }
                None => None,
            };
            let mut state = self.state();
            match kept {
                Some(conversation) => {
                    if let Some(known) = state.known.get_mut(&id) {
                        known.conversation = conversation;
                    }
                }
                None => {
                    if state.known.remove(&id).is_some() {
                        state.known_order.retain(|known| *known != id);
                    }
                }
            }
            drop(state);
            acted.push((plan.known, changes));
            done.push(id);
        }
        let undo = self.record(action, acted);
        Ok(answers::json(&BatchResult { done, failed, undo }))
    }

    /// What `action` asks of the provider for one conversation, or why it can't be done to it.
    fn plan(&self, action: UndoAction, args: &Arguments, known: Known) -> Result<Plan, EngineError> {
        let conversation = &known.conversation;
        let mut plan = Plan { flags: Vec::new(), moves: None, deletes: Vec::new(), known: known.clone() };
        match action {
            UndoAction::Read | UndoAction::Unread => plan.flags = Self::read_targets(conversation, action == UndoAction::Read),
            // Starring stars the latest message; unstarring every starred one.
            UndoAction::Star => plan.flags = vec![conversation.latest().clone()],
            UndoAction::Unstar => {
                plan.flags = conversation.messages.iter().filter(|message| message.envelope.is_flagged()).cloned().collect();
            }
            UndoAction::Archive => plan.moves = self.archive_plan(conversation, &known.primary)?,
            UndoAction::Trash => {
                let trash = Self::mailbox_with_role(&self.state(), &conversation.account, MailboxRole::Trash).cloned();
                let Some(trash) = trash else {
                    let error = ApiError::new(0, "No trash mailbox found.");
                    self.report(&error, Some(&conversation.account));
                    return Err(EngineError::failed(error.message));
                };
                let (in_trash, elsewhere): (Vec<MailMessage>, Vec<MailMessage>) =
                    conversation.messages.iter().cloned().partition(|message| message.mailbox() == trash.id);
                if !in_trash.is_empty() && !args.bool("yes") {
                    return Err(EngineError::new(
                        "confirm",
                        format!("{} is already in the trash, so it would be deleted for good. Re-run with --yes.", conversation.id),
                    ));
                }
                // Mail already in the trash is deleted for good, which undo cannot take back.
                plan.deletes = in_trash;
                let moving: Vec<MailMessage> = elsewhere.into_iter().filter(|message| message.role != MailboxRole::Drafts).collect();
                plan.moves = moving_to(moving, &trash);
            }
            UndoAction::Restore => {
                // The messages in the trash or junk go to the inbox.
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
                plan.moves = moving_to(targets, &inbox);
            }
            UndoAction::Tag => unreachable!("tags are not an action"),
        }
        Ok(plan)
    }

    /// The messages listed from `primary` that sit in an inbox-like mailbox, to the archive. A
    /// conversation the user started is only marked archived (`act`): the user's own sent mail
    /// left behind doesn't list in the inbox as a conversation they started, and one that is
    /// nothing but that leaves the inbox with nothing to move.
    fn archive_plan(
        &self,
        conversation: &Conversation,
        primary: &HashSet<MailboxKey>,
    ) -> Result<Option<(Vec<MailMessage>, String)>, EngineError> {
        if started::started_key(conversation).is_some() {
            return Ok(None);
        }
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
        Ok(moving_to(targets, &target))
    }

    // MARK: - Mutations

    /// The messages `mail.read` changes: every unread one, or the latest not sent by the user.
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

    /// `set_flags`, failing with its first refusal.
    pub(super) async fn mutate_flags(self: &Arc<Self>, targets: Vec<MailMessage>, flag: Flag, add: bool) -> Result<(), EngineError> {
        self.set_flags(targets, flag, add).await.first()
    }

    /// Sets `flag` on `targets` (`add`) or takes it off, shown at once, with one request per
    /// mailbox; a refused request's messages are set back.
    pub(super) async fn set_flags(self: &Arc<Self>, targets: Vec<MailMessage>, flag: Flag, add: bool) -> Refusals {
        self.update_envelopes(&refs(&targets), |envelope| envelope.set_flag(flag, add));
        let requests = by_mailbox(&targets).into_iter().map(|(key, ids)| {
            let sent: HashSet<MessageRef> =
                targets.iter().filter(|message| carried(message, &key, &ids)).map(MailMessage::message_ref).collect();
            let sending = ids.clone();
            async move {
                let result = self
                    .provider_request(
                        key.account.clone(),
                        false,
                        async move |backend: Arc<Backend>| backend.set_flag(flag, add, &sending).await,
                        || self.update_envelopes(&sent, |envelope| envelope.set_flag(flag, !add)),
                    )
                    .await;
                result.err().map(|error| (key, ids, error))
            }
        });
        Refusals(futures::future::join_all(requests).await.into_iter().flatten().collect())
    }

    /// Moves each of `moves` (messages, the id of the mailbox of their account they go to) with
    /// one request per mailbox and destination, taking them out of their listings at once.
    /// `back` (undo) moves them from that mailbox to the ones they were listed in instead,
    /// showing them there at once; a refused request takes its messages out again.
    pub(super) async fn move_all(self: &Arc<Self>, moves: &[(Vec<MailMessage>, String)], back: bool) -> Refusals {
        let all: Vec<MailMessage> = moves.iter().flat_map(|(messages, _)| messages.iter().cloned()).collect();
        if all.is_empty() {
            return Refusals::default();
        }
        if back {
            self.insert_envelopes(&all);
        } else {
            self.remove_envelopes(&all);
        }
        let mut groups: Vec<(MailboxKey, String, Vec<String>)> = Vec::new();
        for (messages, other) in moves {
            for (key, ids) in by_mailbox(messages) {
                match groups.iter_mut().find(|(known, mailbox, _)| *known == key && mailbox == other) {
                    Some((_, _, grouped)) => grouped.extend(ids),
                    None => groups.push((key, other.clone(), ids)),
                }
            }
        }
        let requests = groups.into_iter().map(|(key, other, ids)| {
            let returned: Vec<MailMessage> = all.iter().filter(|message| carried(message, &key, &ids)).cloned().collect();
            let (from, to) = if back { (other, key.mailbox.clone()) } else { (key.mailbox.clone(), other) };
            let sending = ids.clone();
            async move {
                let result = self
                    .provider_request(
                        key.account.clone(),
                        true,
                        async move |backend: Arc<Backend>| backend.move_to(&sending, &from, &to).await,
                        || {
                            if back {
                                self.remove_envelopes(&returned);
                            }
                        },
                    )
                    .await;
                result.err().map(|error| (key, ids, error))
            }
        });
        Refusals(futures::future::join_all(requests).await.into_iter().flatten().collect())
    }

    /// Deletes `messages` (in the trash) for good, with one request per mailbox.
    async fn delete_all(self: &Arc<Self>, messages: &[MailMessage]) -> Refusals {
        if messages.is_empty() {
            return Refusals::default();
        }
        self.remove_envelopes(messages);
        let requests = by_mailbox(messages).into_iter().map(|(key, ids)| {
            let sending = ids.clone();
            async move {
                let result = self
                    .provider_request(key.account.clone(), true, async move |backend: Arc<Backend>| backend.delete(&sending).await, || {})
                    .await;
                result.err().map(|error| (key, ids, error))
            }
        });
        Refusals(futures::future::join_all(requests).await.into_iter().flatten().collect())
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
    pub(super) fn remove_envelopes(&self, targets: &[MailMessage]) {
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

    /// Puts messages `remove_envelopes` dropped back in their listings, where their dates place
    /// them, and their unread ones back in their mailboxes' counts (undo).
    pub(super) fn insert_envelopes(&self, messages: &[MailMessage]) {
        let unseen: Vec<MailMessage> = messages.iter().filter(|message| !message.envelope.is_seen()).cloned().collect();
        self.adjust_unread(&unseen, 1);
        let mut state = self.state();
        for (key, _) in by_mailbox(messages) {
            let Some(listing) = state.listings.get_mut(&key) else { continue };
            for message in messages.iter().filter(|message| *message.key == key && !pending::is_local(message.id())) {
                if listing.envelopes.iter().any(|envelope| envelope.id == message.id()) {
                    continue;
                }
                let date = message.envelope.date.unwrap_or(DISTANT_PAST);
                let at = listing
                    .envelopes
                    .iter()
                    .position(|envelope| envelope.date.unwrap_or(DISTANT_PAST) < date)
                    .unwrap_or(listing.envelopes.len());
                listing.envelopes.insert(at, message.envelope.clone());
            }
            self.cache.save_listing(CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() }, &key);
        }
        drop(state);
        self.events.emit_soon("conversations");
    }

    pub(super) fn adjust_unread(&self, targets: &[MailMessage], delta: i64) {
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
