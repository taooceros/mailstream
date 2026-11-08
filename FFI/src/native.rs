//! Mailstream.app (AppKit)'s calls beyond the control socket's commands, for when it moves to
//! this engine (MS-120; nothing calls them yet). Its list, reader and composer work on messages
//! the way the Swift engine's `MailStore` handed them out (the engine's `mail/native.rs`), so
//! messages, conversations and message content cross as the JSON mail.db keeps, which its Swift
//! model decodes as it is.
use serde_json::Value;

use crate::{ENGINE, MailError, Mailstream, Person};
use mailstream_engine::backend::Flag;
use mailstream_engine::mail::{NativeMessage, NativeOutgoing};
use mailstream_engine::model::{Address, MailboxKey, MessageRef};

/// One mailbox of one account.
#[derive(Clone, Debug, uniffi::Record)]
pub struct MailboxRef {
    pub account: String,
    pub mailbox: String,
}

impl From<MailboxRef> for MailboxKey {
    fn from(key: MailboxRef) -> Self {
        MailboxKey::new(&key.account, &key.mailbox)
    }
}

/// Whether a conversation's remote images and fonts load on their own, and the senders that
/// keep them from it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RemoteContent {
    pub loads: bool,
    pub untrusted: Vec<Person>,
}

/// A message the app composed: new, a reply or a forward (with its source's threading).
#[derive(Clone, Debug, uniffi::Record)]
pub struct Outgoing {
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub bcc: Vec<Person>,
    pub subject: String,
    /// Plain text.
    pub text: String,
    /// An HTML alternative, when a reply quotes HTML.
    pub html: Option<String>,
    /// Full paths of the files to attach.
    pub attachments: Vec<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// The provider's thread to file it under.
    pub thread: Option<String>,
}

fn addresses(people: Vec<Person>) -> Vec<Address> {
    people.into_iter().map(|person| Address::new(person.name, person.email)).collect()
}

fn text(value: Value) -> String {
    value.to_string()
}

fn mailbox_keys(refs: Vec<MailboxRef>) -> Vec<MailboxKey> {
    refs.into_iter().map(MailboxKey::from).collect()
}

#[uniffi::export]
impl Mailstream {
    /// Every loaded message of `primary` threaded with their accounts' inbox and sent mail,
    /// newest first: `{"conversations": [{id, account, messages: [{account, mailbox, role,
    /// envelope, snippet?}], tag?}], "more": bool}`.
    pub async fn native_list(&self, primary: Vec<MailboxRef>) -> Result<String, MailError> {
        Ok(text(ENGINE.native_list(mailbox_keys(primary)).await?))
    }

    /// The providers' search over every mailbox of `accounts` but trash and junk, as
    /// [`Mailstream::native_list`] hands conversations out, with `"failures"`.
    pub async fn native_search(&self, accounts: Vec<String>, query: String) -> Result<String, MailError> {
        Ok(text(ENGINE.native_search(accounts, query).await?))
    }

    /// `keys`' first pages fetched afresh: `{"changed": bool, "failures": [{account, mailbox, message}]}`.
    pub async fn native_fetch_first_pages(&self, keys: Vec<MailboxRef>) -> Result<String, MailError> {
        Ok(text(ENGINE.native_fetch_first_pages(mailbox_keys(keys)).await?))
    }

    /// The next page of each of `keys` that has one; whether a listing grew.
    pub async fn native_fetch_next_pages(&self, keys: Vec<MailboxRef>) -> Result<bool, MailError> {
        Ok(ENGINE.native_fetch_next_pages(mailbox_keys(keys)).await?)
    }

    /// One message's content (`MessageContent`). A `background` load only fills free room in the
    /// engine's memory.
    pub async fn native_content(
        &self,
        account: String,
        mailbox: String,
        id: String,
        background: bool,
    ) -> Result<String, MailError> {
        Ok(text(ENGINE.native_content(MessageRef { account, mailbox, id }, background).await?))
    }

    /// Sets (`add`) or clears flag `seen` or `flagged` of `messages`, a JSON array of
    /// `{account, mailbox, role, envelope}`.
    pub async fn native_set_flags(&self, messages: String, flag: String, add: bool) -> Result<(), MailError> {
        let flag = match flag.as_str() {
            "seen" => Flag::Seen,
            "flagged" => Flag::Flagged,
            _ => return Err(MailError::new("usage", format!("No flag {flag}; seen or flagged."))),
        };
        Ok(ENGINE.native_set_flags(NativeMessage::parse(&messages)?, flag, add).await?)
    }

    /// Moves `messages` to `destination`.
    pub async fn native_move(&self, messages: String, destination: MailboxRef) -> Result<(), MailError> {
        Ok(ENGINE.native_move(NativeMessage::parse(&messages)?, destination.into()).await?)
    }

    /// Deletes `messages`, which are in the trash, for good.
    pub async fn native_delete(&self, messages: String) -> Result<(), MailError> {
        Ok(ENGINE.native_delete(NativeMessage::parse(&messages)?).await?)
    }

    /// Undoes a move of `messages` (as listed before it) to `from`.
    pub async fn native_put_back(&self, messages: String, from: MailboxRef) -> Result<(), MailError> {
        Ok(ENGINE.native_put_back(NativeMessage::parse(&messages)?, from.into()).await?)
    }

    /// Sends `message` from `account` at once; the app asked the person first.
    pub async fn native_deliver(&self, account: String, message: Outgoing) -> Result<(), MailError> {
        let outgoing = NativeOutgoing {
            to: addresses(message.to),
            cc: addresses(message.cc),
            bcc: addresses(message.bcc),
            subject: message.subject,
            text: message.text,
            html: message.html,
            attachments: message.attachments.into_iter().map(Into::into).collect(),
            in_reply_to: message.in_reply_to,
            references: message.references,
            thread: message.thread,
        };
        Ok(ENGINE.native_deliver(account, outgoing).await?)
    }

    /// Everyone compose completes to, best first: `[{"address": {name?, email}, "own": bool}]`.
    pub async fn native_contacts(&self) -> Result<String, MailError> {
        Ok(text(ENGINE.native_contacts().await?))
    }

    /// Whether conversation `id`'s remote content loads on its own (`untrusted` empty and not
    /// junk), and its senders the person has not written to or trusted.
    pub fn remote_content(&self, id: String) -> Result<RemoteContent, MailError> {
        let (loads, untrusted) = ENGINE.remote_content(&id)?;
        let untrusted = untrusted.into_iter().map(|address| Person { name: address.name, email: address.email }).collect();
        Ok(RemoteContent { loads, untrusted })
    }
}
