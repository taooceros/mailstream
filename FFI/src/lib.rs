//! Mailstream's Swift engine (accounts, sync, cache, threading, tagging) as a Rust API, and
//! through UniFFI as a Dart, Kotlin or Python one.
//!
//! The engine lives in `libMailstreamKit.dylib`, the Swift package's `MailstreamKit` product,
//! loaded into this process: one engine per process, started by [`Mailstream::new`]. Requests
//! cross its C API (`Sources/MailstreamKit/FFI/Exports.swift`) as the control socket's JSON, in
//! memory rather than over the socket, and come back as the typed values below.
//!
//! The engine runs on the main thread's run loop. App hosts (Flutter, GPUI, AppKit) already run
//! one; a command-line host calls [`run_main_loop`] on its main thread and works elsewhere.
//! Futures here need no particular executor; listeners are called on the main thread.
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use futures_channel::oneshot;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

uniffi::setup_scaffolding!();

type Callback = extern "C" fn(*mut c_void, *const u8, usize);

unsafe extern "C" {
    fn mailstream_start();
    fn mailstream_call(request: *const u8, length: usize, context: *mut c_void, reply: Callback);
    fn mailstream_subscribe(context: *mut c_void, event: Callback);
    fn mailstream_run_main_loop();
}

/// A failed request: the engine's stable `code` (`unknown_account`, `provider`, `confirm`, …)
/// and a message for people.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MailError {
    #[error("{message}")]
    Failed { code: String, message: String },
}

impl MailError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self::Failed { code: code.into(), message: message.into() }
    }
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Account {
    /// The address, which identifies the account everywhere.
    pub id: String,
    pub label: String,
    /// `google` or `microsoft`.
    pub provider: String,
    pub name: Option<String>,
    /// What the sign-in grants: `mail`, `contacts`, `chat`.
    pub services: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Mailbox {
    pub id: String,
    pub name: String,
    /// `inbox`, `sent`, `drafts`, `archive`, `all`, `junk`, `trash`, `starred`, `important` or `other`.
    pub role: String,
    pub unread: Option<i64>,
    pub total: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub version: String,
    pub pid: i64,
    pub settings_file: String,
    pub settings_issues: Vec<String>,
    pub accounts: Vec<AccountStatus>,
    pub sync: SyncStatus,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub mailboxes_loaded: bool,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub loading: bool,
    /// ISO 8601.
    pub last_refresh: Option<String>,
    pub last_error: Option<String>,
    pub push_connected: bool,
}

/// One conversation as lists show it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Addresses the conversation in later calls while the engine runs.
    pub id: String,
    pub account: String,
    pub subject: String,
    /// ISO 8601, of the latest message.
    pub date: Option<String>,
    /// `Name <address>` of the latest incoming message's sender.
    pub from: Option<String>,
    /// Senders, `me` for the user's own addresses.
    pub participants: Vec<String>,
    pub message_count: i64,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    /// Roles of the mailboxes its messages are in.
    pub mailboxes: Vec<String>,
    pub snippet: Option<String>,
    /// AI category: `people`, `newsletter`, `notification`, …
    pub category: Option<String>,
    /// Waits on the user's reply or action.
    pub waiting: bool,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Listing {
    pub conversations: Vec<Conversation>,
    /// Providers that could not be reached; the conversations are the others'.
    pub failures: Vec<String>,
    /// Passed as [`ListQuery::cursor`], the page after this one; nil at the end and for searches.
    #[serde(default)]
    pub next: Option<String>,
}

/// What [`Mailstream::list`] lists.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ListQuery {
    /// Roles (`inbox`, `sent`, …) or, with `account`, mailbox ids or names. Empty: the inboxes.
    #[uniffi(default = [])]
    pub mailboxes: Vec<String>,
    #[uniffi(default = None)]
    pub account: Option<String>,
    #[uniffi(default = false)]
    pub unread: bool,
    #[uniffi(default = false)]
    pub starred: bool,
    #[uniffi(default = None)]
    pub category: Option<String>,
    #[uniffi(default = false)]
    pub waiting: bool,
    /// 1 to 100.
    #[uniffi(default = 25)]
    pub limit: u32,
    /// The previous page's [`Listing::next`]: the conversations after it, with the same filters.
    #[uniffi(default = None)]
    pub cursor: Option<String>,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            mailboxes: vec![], account: None, unread: false, starred: false, category: None, waiting: false, limit: 25,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: i64,
    /// Where the engine keeps the file once fetched.
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Message {
    pub id: String,
    pub mailbox: String,
    pub role: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub date: Option<String>,
    pub subject: String,
    pub unread: bool,
    pub starred: bool,
    /// Plain text, or the HTML body as text; nil when the body could not load (`error`).
    pub text: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// The HTML body when asked for (`html` in [`Mailstream::show`]); inline images point at
    /// files in the cache.
    pub html: Option<String>,
    pub error: Option<String>,
}

/// A conversation with its messages and bodies.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Thread {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct BatchFailure {
    pub id: String,
    pub code: Option<String>,
    pub message: String,
}

/// An action over several conversations: the ids it was done for and those it failed for.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct BatchResult {
    pub done: Vec<String>,
    pub failed: Vec<BatchFailure>,
}

/// A conversation's AI tag after [`Mailstream::tag`] corrected it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Tagged {
    pub id: String,
    /// `people`, `newsletter`, `notification`, …
    pub category: String,
    pub waiting: bool,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct TagResult {
    pub done: Vec<Tagged>,
    pub failed: Vec<BatchFailure>,
}

/// A sent reply: the account it went from, its recipients (`Name <address>`) and subject.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Sent {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}

/// What changed in the engine; a front end re-reads what it shows of it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Event {
    Accounts,
    Mailboxes,
    /// The inbox list changed: new mail, flags, moves.
    Conversations,
    Previews,
    Tags,
    Sync,
    Problems,
    /// A kind this build does not know yet.
    Other { name: String },
}

impl Event {
    fn named(name: &str) -> Self {
        match name {
            "accounts" => Self::Accounts,
            "mailboxes" => Self::Mailboxes,
            "conversations" => Self::Conversations,
            "previews" => Self::Previews,
            "tags" => Self::Tags,
            "sync" => Self::Sync,
            "problems" => Self::Problems,
            _ => Self::Other { name: name.into() },
        }
    }
}

/// One change, as [`EventListener::on_event`] gets it. A record rather than the bare [`Event`]:
/// uniffi-dart passes an enum argument of a foreign callback as an integer, which does not match
/// the buffer Rust sends, and the Dart side fails to read it; records cross intact.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Change {
    pub event: Event,
}

/// Implemented by the front end (in Rust, Dart, …) to hear about changes.
#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, change: Change);
}

/// The engine of this process.
#[derive(uniffi::Object)]
pub struct Mailstream {}

#[uniffi::export]
impl Mailstream {
    /// Starts the engine (once per process; later calls share it): it paints from the cache and
    /// starts syncing every signed-in account.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        unsafe { mailstream_start() };
        Arc::new(Self {})
    }

    pub async fn status(&self) -> Result<Status, MailError> {
        call("status", json!({})).await
    }

    pub async fn accounts(&self) -> Result<Vec<Account>, MailError> {
        call("accounts.list", json!({})).await
    }

    /// Each account's mailboxes, by account id; only `account`'s when given.
    pub async fn mailboxes(&self, account: Option<String>) -> Result<HashMap<String, Vec<Mailbox>>, MailError> {
        call("mailboxes", json!({ "account": account })).await
    }

    /// Conversations newest first, fetched from the providers.
    pub async fn list(&self, query: ListQuery) -> Result<Listing, MailError> {
        let mut args = json!({
            "account": query.account, "unread": query.unread, "starred": query.starred,
            "category": query.category, "waiting": query.waiting, "limit": query.limit, "cursor": query.cursor,
        });
        if !query.mailboxes.is_empty() {
            args["mailbox"] = json!(query.mailboxes);
        }
        call("mail.list", args).await
    }

    /// The providers' own search over every mailbox but trash and junk.
    pub async fn search(&self, query: String, account: Option<String>, limit: u32) -> Result<Listing, MailError> {
        call("mail.search", json!({ "query": query, "account": account, "limit": limit })).await
    }

    /// A conversation from a listing, with text bodies of at most `max_chars` characters and,
    /// with `html`, the HTML bodies.
    pub async fn show(&self, id: String, max_chars: u32, html: bool) -> Result<Thread, MailError> {
        call("mail.show", json!({ "id": id, "max-chars": max_chars, "html": html })).await
    }

    /// The preview line of each conversation's latest message by id, loading those not loaded yet.
    pub async fn previews(&self, ids: Vec<String>) -> Result<HashMap<String, Option<String>>, MailError> {
        call("mail.previews", json!({ "ids": ids })).await
    }

    /// Each sender's picture as a `file:` URL of a PNG in the cache, by lowercased address; nil
    /// when there is none. `automated` names the addresses a site icon may stand in for.
    pub async fn avatars(
        &self,
        addresses: Vec<String>,
        automated: Vec<String>,
    ) -> Result<HashMap<String, Option<String>>, MailError> {
        call("mail.avatars", json!({ "addresses": addresses, "automated": automated })).await
    }

    pub async fn set_read(&self, ids: Vec<String>, read: bool) -> Result<BatchResult, MailError> {
        call("mail.read", json!({ "ids": ids, "unread": !read })).await
    }

    pub async fn set_starred(&self, ids: Vec<String>, starred: bool) -> Result<BatchResult, MailError> {
        call("mail.star", json!({ "ids": ids, "off": !starred })).await
    }

    pub async fn archive(&self, ids: Vec<String>) -> Result<BatchResult, MailError> {
        call("mail.archive", json!({ "ids": ids })).await
    }

    /// Conversations already in the trash are deleted for good, which needs `confirm`.
    pub async fn trash(&self, ids: Vec<String>, confirm: bool) -> Result<BatchResult, MailError> {
        call("mail.trash", json!({ "ids": ids, "yes": confirm })).await
    }

    pub async fn restore(&self, ids: Vec<String>) -> Result<BatchResult, MailError> {
        call("mail.restore", json!({ "ids": ids })).await
    }

    /// Corrects the AI tag; each of `category`, `waiting` and `labels` (at most two) changes
    /// only when given.
    pub async fn tag(
        &self,
        ids: Vec<String>,
        category: Option<String>,
        waiting: Option<bool>,
        labels: Option<Vec<String>>,
    ) -> Result<TagResult, MailError> {
        let waiting = waiting.map(|waiting| if waiting { "true" } else { "false" });
        call("mail.tag", json!({ "ids": ids, "category": category, "waiting": waiting, "labels": labels })).await
    }

    /// Replies to the conversation's latest message, quoting it, to its sender or with `all` to
    /// everyone. Sends at once: `confirm` is the person's go-ahead.
    pub async fn reply(&self, id: String, body: String, all: bool, confirm: bool) -> Result<Sent, MailError> {
        call("mail.reply", json!({ "id": id, "body": body, "all": all, "yes": confirm })).await
    }

    /// Fetches new mail and counts now.
    pub async fn refresh(&self) -> Result<(), MailError> {
        call::<Value>("refresh", json!({})).await.map(drop)
    }

    /// Calls `listener` on the main thread for every change from now on, while the process runs.
    pub fn subscribe(&self, listener: Arc<dyn EventListener>) {
        let context = Box::into_raw(Box::new(listener)).cast::<c_void>();
        unsafe { mailstream_subscribe(context, deliver_event) };
    }
}

/// Runs the main thread's run loop, which the engine works on, for a host without one (a
/// command-line tool). Call it on the main thread; it never returns.
#[uniffi::export]
pub fn run_main_loop() {
    unsafe { mailstream_run_main_loop() };
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
    result: Option<Value>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

async fn call<T: DeserializeOwned>(command: &str, args: Value) -> Result<T, MailError> {
    let request = serde_json::to_vec(&json!({ "command": command, "args": args })).expect("JSON values encode");
    let (sender, receiver) = oneshot::channel::<Vec<u8>>();
    let context = Box::into_raw(Box::new(sender)).cast::<c_void>();
    unsafe { mailstream_call(request.as_ptr(), request.len(), context, deliver_answer) };
    let bytes = receiver.await.map_err(|_| MailError::new("unreachable", "The engine dropped the request."))?;
    let answer: Answer = serde_json::from_slice(&bytes)
        .map_err(|error| MailError::new("decode", format!("Unexpected answer to {command}: {error}")))?;
    if !answer.ok {
        let error = answer.error.unwrap_or(ErrorBody { code: None, message: None });
        return Err(MailError::Failed {
            code: error.code.unwrap_or_else(|| "failed".into()),
            message: error.message.unwrap_or_else(|| format!("{command} failed.")),
        });
    }
    serde_json::from_value(answer.result.unwrap_or(Value::Null))
        .map_err(|error| MailError::new("decode", format!("Unexpected result of {command}: {error}")))
}

/// The engine's answer to one request: the context is that request's sender, used once.
extern "C" fn deliver_answer(context: *mut c_void, bytes: *const u8, length: usize) {
    let sender = unsafe { Box::from_raw(context.cast::<oneshot::Sender<Vec<u8>>>()) };
    let _ = sender.send(unsafe { bytes_of(bytes, length) }.to_vec());
}

/// One engine event: the context is a subscriber's listener, kept for the life of the process.
extern "C" fn deliver_event(context: *mut c_void, bytes: *const u8, length: usize) {
    let listener = unsafe { &*context.cast::<Arc<dyn EventListener>>() };
    #[derive(Deserialize)]
    struct Message {
        event: String,
    }
    if let Ok(message) = serde_json::from_slice::<Message>(unsafe { bytes_of(bytes, length) }) {
        listener.on_event(Change { event: Event::named(&message.event) });
    }
}

unsafe fn bytes_of<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    if bytes.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(bytes, length) } }
}
