//! Mail sent from here shows at once (MS-291): as Send is pressed, the message lists in its
//! account's Sent mailbox, so a reply joins its conversation (the threader's context mail) and a
//! new message lists in the inbox as a conversation the user started (`started`), the user's own,
//! before the provider has it. It is held in memory beside the listings, under a local id, its
//! body in `Contents`, and goes when:
//!
//! - the send fails: it leaves the lists and the command answers the error;
//! - the provider's copy lists in Sent: Gmail's by the id its send answered, Outlook's by the
//!   Message-ID, else (Outlook may write its own) by the same subject to the same people, dated
//!   since. The copy takes its place, so it never shows twice;
//! - no copy listed `LINGER` after it was sent: what the provider lists is the truth.
use std::collections::HashSet;
use std::sync::Arc;

use super::started::Status;
use super::{Mail, State};
use crate::backend::SentCopy;
use crate::mime::OutgoingMessage;
use crate::model::{Address, Date, Envelope, MailboxKey, MailboxRole, MessageContent, MessageRef, now};
use crate::text;

/// How the ids of messages held here start; no provider's do.
const LOCAL: &str = "local-";
/// How long a sent message lists without the provider's copy.
const LINGER: f64 = 10.0 * 60.0;
/// How much earlier than the send a copy matched by subject and people may be dated (the
/// provider's clock).
const SKEW: f64 = 2.0 * 60.0;

/// A message held here: an id the provider never gave.
pub(super) fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL)
}

/// A message sent from here whose copy Sent doesn't list yet.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    /// The account's Sent mailbox, where it lists.
    pub(super) key: MailboxKey,
    pub(super) envelope: Arc<Envelope>,
    sent_at: Date,
    /// The provider took it.
    delivered: bool,
    /// The copy's id, when the provider said (Gmail).
    copy: Option<String>,
}

impl Pending {
    fn message_ref(&self) -> MessageRef {
        MessageRef { account: self.key.account.clone(), mailbox: self.key.mailbox.clone(), id: self.envelope.id.clone() }
    }

    /// Answers nothing: a conversation of its own, which the inbox lists as one the user started.
    fn starts_conversation(&self) -> bool {
        self.envelope.in_reply_to.is_empty()
    }

    fn is_copy(&self, listed: &Envelope) -> bool {
        if let Some(copy) = &self.copy {
            return *copy == listed.id;
        }
        if listed.message_id.is_some() && listed.message_id == self.envelope.message_id {
            return true;
        }
        self.delivered
            && listed.subject == self.envelope.subject
            && people(&listed.to) == people(&self.envelope.to)
            && listed.date.is_some_and(|date| date >= self.sent_at - SKEW)
    }
}

fn people(list: &[Address]) -> HashSet<String> {
    list.iter().map(Address::normalized_email).collect()
}

impl Mail {
    /// Lists `message`, about to be sent from `account`, in its Sent mailbox; answers it to hand
    /// to `delivered` or `undelivered`. Nothing lists while the account's Sent mailbox is unknown.
    pub(super) fn sending(&self, message: &OutgoingMessage, account: &str) -> Option<MessageRef> {
        let mut state = self.state();
        let key = Self::mailbox_with_role(&state, account, MailboxRole::Sent)?.key();
        let id = format!("{LOCAL}{}", message.message_id);
        let in_reply_to: Vec<String> = message.in_reply_to.iter().cloned().collect();
        let envelope = Envelope {
            id,
            // A new conversation's thread is the provider's to name; until then its own.
            thread: message.thread.clone().or_else(|| in_reply_to.is_empty().then(|| format!("{LOCAL}{}", message.message_id))),
            message_id: Some(message.message_id.clone()),
            in_reply_to,
            flags: Envelope::flags(true, false, false),
            subject: message.subject.clone(),
            from: vec![message.from.clone()],
            to: message.to.clone(),
            date: Some(now()),
            size: 0,
            has_attachment: Some(!message.attachments.is_empty()),
            tags: Default::default(),
            provider_category: None,
            important: false,
        };
        let pending =
            Pending { key, envelope: Arc::new(envelope), sent_at: now(), delivered: false, copy: None };
        let reference = pending.message_ref();
        if pending.starts_conversation()
            && let Some(thread) = &pending.envelope.thread
        {
            state.started.threads.insert((account.to_string(), thread.clone()), (Status::Unanswered, now()));
        }
        state.pending.push(pending);
        self.contents.hold(
            reference.clone(),
            MessageContent {
                html: message.html.clone(),
                text: Some(message.text.clone()),
                cc: message.cc.clone(),
                reply_to: Vec::new(),
                references: message.references.clone(),
                attachments: Vec::new(),
                snippet: text::snippet(&message.text),
                invitation: None,
            },
        );
        drop(state);
        self.conversations_changed();
        Some(reference)
    }

    /// The provider took the message `sending` listed: Gmail's answer names the copy and its
    /// thread, which a new conversation goes on in.
    pub(super) fn delivered(&self, local: &MessageRef, copy: SentCopy) {
        let mut state = self.state();
        let Some(pending) = state.pending.iter_mut().find(|pending| pending.envelope.id == local.id) else { return };
        pending.delivered = true;
        pending.copy = copy.id;
        let Some(thread) = copy.thread.filter(|_| pending.starts_conversation()) else { return };
        let mut envelope = (*pending.envelope).clone();
        let before = envelope.thread.replace(thread.clone());
        pending.envelope = Arc::new(envelope);
        Self::move_started(&mut state, &local.account, before.as_deref(), Some(&thread));
    }

    /// The send failed: the message `sending` listed leaves the lists.
    pub(super) fn undelivered(&self, local: &MessageRef) {
        let mut state = self.state();
        let Some(index) = state.pending.iter().position(|pending| pending.envelope.id == local.id) else { return };
        let pending = state.pending.remove(index);
        if pending.starts_conversation() {
            Self::move_started(&mut state, &local.account, pending.envelope.thread.as_deref(), None);
        }
        self.contents.release(local, None);
        drop(state);
        self.conversations_changed();
    }

    /// Takes the messages held for `key` out once Sent lists their copies there, or they have
    /// lingered long enough without; whether any went. Called with Sent's listing updated.
    pub(super) fn settle_pending(&self, state: &mut State, key: &MailboxKey) -> bool {
        if !state.pending.iter().any(|pending| pending.key == *key) {
            return false;
        }
        let listed: Vec<Arc<Envelope>> = state.listings.get(key).map(|listing| listing.envelopes.clone()).unwrap_or_default();
        let mut taken: HashSet<&str> = HashSet::new();
        let mut settled: Vec<(Pending, Option<Arc<Envelope>>)> = Vec::new();
        let mut kept = Vec::with_capacity(state.pending.len());
        for pending in std::mem::take(&mut state.pending) {
            if pending.key != *key {
                kept.push(pending);
                continue;
            }
            match listed.iter().find(|envelope| !taken.contains(envelope.id.as_str()) && pending.is_copy(envelope)) {
                Some(copy) => {
                    taken.insert(copy.id.as_str());
                    settled.push((pending, Some(copy.clone())));
                }
                None if pending.delivered && now() - pending.sent_at > LINGER => settled.push((pending, None)),
                None => kept.push(pending),
            }
        }
        state.pending = kept;
        if settled.is_empty() {
            return false;
        }
        let mut moved = false;
        for (pending, copy) in &settled {
            let local = pending.message_ref();
            let copy_ref = copy.as_ref().map(|copy| MessageRef { id: copy.id.clone(), ..local.clone() });
            self.contents.release(&local, copy_ref.as_ref());
            if pending.starts_conversation() {
                let thread = copy.as_ref().and_then(|copy| copy.thread.as_deref());
                Self::move_started(state, &key.account, pending.envelope.thread.as_deref(), thread);
                moved |= thread.is_some();
            }
        }
        if moved {
            self.save_started(state);
        }
        true
    }

    /// What is known of the conversation the user started under thread `from` (its own until the
    /// provider named it) goes to the provider's `to`, unless that is known already; none drops it.
    fn move_started(state: &mut State, account: &str, from: Option<&str>, to: Option<&str>) {
        let Some(from) = from.filter(|from| is_local(from)) else { return };
        let Some(known) = state.started.threads.remove(&(account.to_string(), from.to_string())) else { return };
        if let Some(to) = to {
            state.started.threads.entry((account.to_string(), to.to_string())).or_insert(known);
        }
    }
}
