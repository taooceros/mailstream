//! What Mailstream.app (AppKit) asks of the mail store beyond the control socket's commands, for
//! when it moves to this engine (MS-120; only the FFI's `native.rs` calls it so far). Its list,
//! reader and composer work on messages the way the Swift engine's `MailStore` handed them out:
//! the lists in view are every loaded message of their mailboxes, threaded; mutations and
//! their Undo name the messages they move; replies and forwards are composed by the app and only
//! delivered here. Messages travel as the JSON mail.db keeps (`Envelope`, `MessageContent`), which
//! the app's Swift model decodes as it is.
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Mail, remember};
use crate::backend::Flag;
use crate::cache::CachedListing;
use crate::mime::OutgoingMessage;
use crate::model::{Address, Envelope, MailMessage, MailboxKey, MailboxRole, MessageRef};
use crate::EngineError;

/// A message as the app names it: where it lists, its envelope as listed, and the mailbox's role.
#[derive(Clone, Debug, Deserialize)]
pub struct NativeMessage {
    pub account: String,
    pub mailbox: String,
    pub role: MailboxRole,
    pub envelope: Envelope,
}

impl NativeMessage {
    fn message(self) -> MailMessage {
        MailMessage {
            key: Arc::new(MailboxKey::new(&self.account, &self.mailbox)),
            envelope: Arc::new(self.envelope),
            role: self.role,
        }
    }

    /// Messages from the app's JSON array of them.
    pub fn parse(json: &str) -> Result<Vec<MailMessage>, EngineError> {
        let messages: Vec<NativeMessage> =
            serde_json::from_str(json).map_err(|error| EngineError::usage(format!("Not a list of messages: {error}")))?;
        Ok(messages.into_iter().map(NativeMessage::message).collect())
    }
}

/// A message the app composed (a new one, a reply or a forward), with the source's threading.
#[derive(Clone, Debug, Default)]
pub struct NativeOutgoing {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<std::path::PathBuf>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub thread: Option<String>,
}

impl Mail {
    /// Every loaded message of `primary` (from memory, else mail.db) threaded with their accounts'
    /// inbox and sent mail, newest first, as the app's list shows them: each conversation with
    /// its messages (and their previews, where loaded) and AI tag. The conversations are
    /// remembered, so commands taking conversation ids find them; `more` says whether a mailbox
    /// of `primary` has a page after the mail kept.
    pub async fn native_list(self: &Arc<Self>, primary: Vec<MailboxKey>) -> Value {
        let context = Self::context_keys(&self.state(), &primary);
        let all: Vec<MailboxKey> = primary.iter().chain(&context).cloned().collect();
        self.load_cached_listings(&all).await;
        let mut state = self.state();
        let inboxes = !primary.is_empty() && primary.iter().all(|key| Self::role(&state, key) == MailboxRole::Inbox);
        let fetched: Vec<(MailboxKey, Vec<Arc<crate::model::Envelope>>)> =
            primary.iter().map(|key| (key.clone(), Self::loaded_envelopes(&state, key))).collect();
        let listed = Self::threads(&state, &fetched, &context, Vec::new(), inboxes);
        let more = primary.iter().any(|key| Self::has_next_page(&state, key));
        for conversation in &listed.conversations {
            remember(&mut state, conversation, &listed.primary);
        }
        drop(state);
        json!({ "conversations": self.native_conversations(&listed.conversations), "more": more })
    }

    /// The providers' search over every mailbox of `accounts` but trash and junk, threaded as
    /// `native_list` hands conversations out.
    pub async fn native_search(self: &Arc<Self>, accounts: Vec<String>, query: &str) -> Value {
        let listed = self.agent_search(&accounts, query, super::PAGE_SIZE).await;
        {
            let mut state = self.state();
            for conversation in &listed.conversations {
                remember(&mut state, conversation, &listed.primary);
            }
        }
        json!({ "conversations": self.native_conversations(&listed.conversations), "failures": listed.failures })
    }

    fn native_conversations(&self, conversations: &[crate::model::Conversation]) -> Vec<Value> {
        conversations
            .iter()
            .map(|conversation| {
                let messages: Vec<Value> = conversation
                    .messages
                    .iter()
                    .map(|message| {
                        let mut item = json!({
                            "account": message.account(), "mailbox": message.mailbox(), "role": message.role,
                            "envelope": &*message.envelope,
                        });
                        if let Some(snippet) = self.contents.snippet(&message.message_ref()) {
                            item["snippet"] = json!(snippet);
                        }
                        item
                    })
                    .collect();
                let mut item = json!({ "id": conversation.id, "account": conversation.account, "messages": messages });
                if let Some(tag) = self.tag(conversation) {
                    item["tag"] = json!(tag);
                }
                item
            })
            .collect()
    }

    /// The first pages of `keys`, fetched afresh: whether a listing changed, and each failed
    /// key's error (also reported as the engine's problem).
    pub async fn native_fetch_first_pages(self: &Arc<Self>, keys: Vec<MailboxKey>) -> Value {
        self.load_cached_listings(&keys).await;
        let (failures, changed) = self.fetch_first_pages(&keys).await;
        let failures: Vec<Value> = failures
            .into_iter()
            .map(|(key, error)| json!({ "account": key.account, "mailbox": key.mailbox, "message": error.message }))
            .collect();
        json!({ "changed": changed, "failures": failures })
    }

    /// The next page of each of `keys` that has one; whether a listing grew.
    pub async fn native_fetch_next_pages(self: &Arc<Self>, keys: Vec<MailboxKey>) -> bool {
        self.fetch_next_pages(&keys).await
    }

    /// One message's content (memory, mail.db, else the provider), as mail.db keeps it. A
    /// `background` load only fills free room in memory, like a preview's.
    pub async fn native_content(self: &Arc<Self>, message: MessageRef, background: bool) -> Result<Value, EngineError> {
        let backend = self.backend(&message.account);
        let content = if background {
            self.contents.load_background(&message, backend).await
        } else {
            self.contents.load(&message, backend).await
        };
        content.map(|content| json!(&*content)).map_err(EngineError::failed)
    }

    /// Sets or clears a flag of `targets`: shown in the listings (and unread counts) at once, then
    /// sent to the provider; a refusal is rolled back and reported.
    pub async fn native_set_flags(self: &Arc<Self>, targets: Vec<MailMessage>, flag: Flag, add: bool) -> Result<(), EngineError> {
        if targets.is_empty() {
            return Ok(());
        }
        if flag == Flag::Seen {
            self.adjust_unread(&targets, if add { -1 } else { 1 });
        }
        self.mutate_flags(targets, flag, add).await
    }

    /// Moves `targets` to `destination`: they leave their listings at once; a refusal fetches
    /// the lists afresh.
    pub async fn native_move(self: &Arc<Self>, targets: Vec<MailMessage>, destination: MailboxKey) -> Result<(), EngineError> {
        let Some(mailbox) = Self::mailbox(&self.state(), &destination).cloned() else {
            return Err(EngineError::new("unknown_mailbox", format!("No mailbox {} in {}.", destination.mailbox, destination.account)));
        };
        self.move_messages(targets, &mailbox).await
    }

    /// Deletes `targets` (in the trash) for good.
    pub async fn native_delete(self: &Arc<Self>, targets: Vec<MailMessage>) -> Result<(), EngineError> {
        let Some(first) = targets.first() else { return Ok(()) };
        let account = first.account().to_string();
        self.remove_envelopes(&targets);
        let ids: Vec<String> = targets.iter().map(|message| message.id().to_string()).collect();
        self.provider_request(account, true, async move |backend: Arc<crate::backend::Backend>| backend.delete(&ids).await, || {})
            .await
    }

    /// Undo of a move to `from`: `messages` (as they were listed before it) leave `from` for the
    /// mailboxes they came from and show there again at once.
    pub async fn native_put_back(self: &Arc<Self>, messages: Vec<MailMessage>, from: MailboxKey) -> Result<(), EngineError> {
        let role = Self::role(&self.state(), &from);
        let moved: Vec<MailMessage> = messages
            .iter()
            .map(|message| MailMessage { key: Arc::new(from.clone()), envelope: message.envelope.clone(), role })
            .collect();
        self.remove_envelopes(&moved);
        let groups = super::actions::by_mailbox(&messages);
        {
            let mut state = self.state();
            for message in &messages {
                let Some(listing) = state.listings.get_mut(&message.key) else { continue };
                if !listing.envelopes.iter().any(|envelope| envelope.id == message.envelope.id) {
                    listing.envelopes.push(message.envelope.clone());
                }
            }
            for (key, _) in &groups {
                let Some(listing) = state.listings.get_mut(key) else { continue };
                listing.envelopes.sort_by(|a, b| {
                    let (da, db) = (a.date.unwrap_or(crate::model::DISTANT_PAST), b.date.unwrap_or(crate::model::DISTANT_PAST));
                    db.total_cmp(&da)
                });
                self.cache.save_listing(CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() }, key);
            }
        }
        let unseen: Vec<MailMessage> = messages.iter().filter(|message| !message.envelope.is_seen()).cloned().collect();
        self.adjust_unread(&unseen, 1);
        self.events.emit_soon("conversations");
        let requests = groups.into_iter().map(|(key, ids)| {
            let from = from.mailbox.clone();
            self.provider_request(
                key.account.clone(),
                true,
                async move |backend: Arc<crate::backend::Backend>| backend.move_to(&ids, &from, &key.mailbox).await,
                || {},
            )
        });
        super::actions::first_failure(futures::future::join_all(requests).await)
    }

    /// Sends what the app composed from `account`'s identity, then picks up the sent copy.
    pub async fn native_deliver(self: &Arc<Self>, account: &str, outgoing: NativeOutgoing) -> Result<(), EngineError> {
        let from = self
            .state()
            .accounts
            .iter()
            .find(|known| known.name == account)
            .and_then(|known| known.identity.clone())
            .unwrap_or_else(|| Address::new(None, account));
        let message = OutgoingMessage {
            from,
            to: outgoing.to,
            cc: outgoing.cc,
            bcc: outgoing.bcc,
            subject: outgoing.subject,
            text: outgoing.text,
            html: outgoing.html,
            attachments: outgoing.attachments,
            in_reply_to: outgoing.in_reply_to,
            references: outgoing.references,
            thread: outgoing.thread,
        };
        self.deliver(&message, account).await
    }
}
