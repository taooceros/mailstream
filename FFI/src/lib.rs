//! Mailstream's engine (accounts, sync, cache, threading, tagging, chat) as a Rust API, and
//! through UniFFI as a Dart, Kotlin or Python one.
//!
//! The engine is the Rust one in `Engine/`, linked into this crate: one engine per process,
//! started by [`Mailstream::new`]. It runs on its own threads, so its host needs no run loop of
//! its own; requests go to it as the control socket's commands and come back as the typed
//! values below. Futures here need no particular executor; listeners are called on the main
//! thread (a command-line host without a run loop calls [`run_main_loop`] on it; on Windows, the
//! thread that subscribes first, whose message loop runs them). Whichever
//! process starts its engine first serves the control socket the `mailstream` command line uses.
//!
//! With the `swift-engine` feature, a process started with `MAILSTREAM_ENGINE=swift` runs the
//! Swift engine Mailstream.app still has (`libMailstreamKit.dylib`, through its C API in
//! `Sources/MailstreamKit/FFI/Exports.swift`) instead, for `examples/bench.rs` to compare the
//! two. The front ends never build it (MS-119); it goes with the Swift engine (MS-120).
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

uniffi::setup_scaffolding!();

#[cfg(feature = "blitz")]
mod html_page;
mod main_thread;
mod people;
#[cfg(feature = "swift-engine")]
mod swift;

#[cfg(feature = "blitz")]
pub use html_page::{HtmlPage, HtmlPart, HtmlSize, HtmlTile};
pub use people::Person;

/// A failed request: the engine's stable `code` (`unknown_account`, `provider`, `confirm`, …)
/// and a message for people.
#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum MailError {
    #[error("{message}")]
    Failed { code: String, message: String },
}

impl MailError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self::Failed { code: code.into(), message: message.into() }
    }

    /// The engine's stable code: `unreachable`, `unknown_command`, `confirm`, `disabled`, …
    pub fn code(&self) -> &str {
        match self {
            Self::Failed { code, .. } => code,
        }
    }
}

/// An OAuth client the engine knows: its id, and whether a secret is saved with it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInfo {
    pub id: String,
    pub has_secret: bool,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Account {
    /// The address, which identifies the account everywhere.
    pub id: String,
    pub label: String,
    /// `google` or `microsoft`.
    pub provider: String,
    pub name: Option<String>,
    /// What the sign-in grants: `mail`, `contacts`, `chat`, `calendar`.
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
    pub ai_tagging: TaggingStatus,
}

/// The AI tagger: whether it is on, and what it is doing or why it stopped.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct TaggingStatus {
    pub enabled: bool,
    /// A line for people (progress or the last failure); nil while idle.
    pub status: Option<String>,
    /// `status` is the last run's failure (omp or Apple Intelligence unavailable, a failed run);
    /// [`Event::Problems`] announces it showing up and going away.
    pub failed: bool,
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
    /// Lists what the engine already holds without fetching the providers' first pages, for a
    /// front end reading again after a change event.
    #[uniffi(default = false)]
    pub cached: bool,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            mailboxes: vec![], account: None, unread: false, starred: false, category: None, waiting: false, limit: 25,
            cursor: None, cached: false,
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
    /// The meeting invitation it carries; [`Mailstream::calendar_invitation`] says how the
    /// account answered.
    #[serde(default)]
    pub invitation: Option<MeetingInvitation>,
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

/// A conversation auto-read marked read and nobody undid.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct AutoReadEntry {
    pub id: String,
    pub account: String,
    pub sender: String,
    pub subject: String,
    /// `notification` or `newsletter`.
    pub category: String,
    /// `age` (older than `autoRead.days`) or `superseded` (its sender sent a newer newsletter).
    pub reason: String,
    /// ISO 8601.
    pub at: String,
    /// The pass that marked it read; [`Mailstream::undo_auto_read`] without ids undoes the latest.
    pub batch: u64,
}

/// What auto-read (`autoRead.*` settings) marked read, newest first.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct AutoReadLog {
    pub enabled: bool,
    pub days: i64,
    pub entries: Vec<AutoReadEntry>,
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

/// A new message. Addresses are `Name <address>` or bare addresses.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Draft {
    /// The sending account's id (its address).
    pub from: String,
    /// At least one address.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    /// Plain text.
    pub body: String,
    /// Full paths of the files to attach.
    pub attachments: Vec<String>,
}

/// Optional edits to the engine's reply plan. `None` keeps the default; an empty value clears it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ReplyOverrides {
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub subject: Option<String>,
    /// Full paths of files to attach; source attachments are not forwarded automatically.
    pub attachments: Option<Vec<String>>,
}

/// What was sent: the account it went from, its recipients (`Name <address>`; replies include
/// To, Cc and Bcc) and subject.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Sent {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}

/// A setting's value, typed by what the setting holds.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SettingValue {
    Flag { enabled: bool },
    Number { value: i64 },
    Text { text: String },
    /// Lowercased email addresses, sorted and unique.
    Addresses { addresses: Vec<String> },
}

impl SettingValue {
    fn of(kind: &str, value: &Value) -> Option<Self> {
        Some(match kind {
            "boolean" => Self::Flag { enabled: value.as_bool()? },
            "integer" => Self::Number { value: value.as_i64()? },
            "string" => Self::Text { text: value.as_str()?.into() },
            _ => Self::Addresses { addresses: serde_json::from_value(value.clone()).ok()? },
        })
    }

    /// As `config.set` takes it: true/false, a number, text, or a JSON array.
    fn typed(&self) -> String {
        match self {
            Self::Flag { enabled } => enabled.to_string(),
            Self::Number { value } => value.to_string(),
            Self::Text { text } => text.clone(),
            Self::Addresses { addresses } => serde_json::to_string(addresses).expect("strings encode"),
        }
    }
}

/// One setting of settings.json, the file Mailstream.app and every front end share.
#[derive(Clone, Debug, uniffi::Record)]
pub struct SettingEntry {
    /// Dotted key, e.g. `sync.pollSeconds`.
    pub key: String,
    pub value: SettingValue,
    pub default_value: SettingValue,
    /// The file sets it (to something other than the default).
    pub changed: bool,
    pub summary: String,
    /// The range a number setting accepts.
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    /// The words a text setting accepts; empty when it takes any text.
    pub choices: Vec<String>,
}

impl<'de> Deserialize<'de> for SettingEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            key: String,
            #[serde(rename = "type")]
            kind: String,
            value: Value,
            default: Value,
            changed: bool,
            summary: String,
            minimum: Option<i64>,
            maximum: Option<i64>,
            #[serde(default)]
            choices: Vec<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let typed = |value: &Value| {
            SettingValue::of(&raw.kind, value)
                .ok_or_else(|| serde::de::Error::custom(format!("{} is not a {}", raw.key, raw.kind)))
        };
        Ok(Self {
            value: typed(&raw.value)?,
            default_value: typed(&raw.default)?,
            key: raw.key,
            changed: raw.changed,
            summary: raw.summary,
            minimum: raw.minimum,
            maximum: raw.maximum,
            choices: raw.choices,
        })
    }
}

/// Every setting, the file they live in and its problems (unknown keys, invalid values, broken
/// JSON; those fall back to the default).
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Settings {
    pub file: String,
    pub settings: Vec<SettingEntry>,
    pub issues: Vec<String>,
}

/// A Google Chat account: a Google account signed in with Chat access.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatAccount {
    pub id: String,
    /// The account's label, as the sidebar shows it.
    pub label: String,
    /// Whether its sign-in lets it create direct messages and group chats.
    pub can_start: bool,
    /// Spaces with unread messages.
    pub unread: u32,
}

/// One space as the chat list shows it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatSpace {
    pub account: String,
    /// `spaces/…`.
    pub space: String,
    /// `dm`, `group`, `space` or `bot`.
    pub kind: Option<String>,
    pub title: String,
    /// "Brent: see you" / "You: thanks".
    pub preview: String,
    /// The date as lists show it, and its date group.
    pub date: String,
    pub section: String,
    pub activity: Option<String>,
    pub unread: bool,
    pub attachment: bool,
    /// The person the row shows: their lowercased address, else their Chat user id; empty for none.
    pub avatar_key: String,
    pub avatar_name: String,
    /// What picks the row's avatar color, as the native list picks it.
    pub avatar_color_key: String,
    /// The picture Google Chat publishes for them.
    pub avatar_url: Option<String>,
}

/// The chat accounts and every space loaded, most recently active first.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct ChatSpaces {
    pub accounts: Vec<ChatAccount>,
    pub loading: bool,
    /// Older spaces exist beyond these ([`Mailstream::chat_more`]).
    pub more: bool,
    pub spaces: Vec<ChatSpace>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatUser {
    /// `users/…`.
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub avatar_key: String,
    pub avatar_url: Option<String>,
    pub bot: bool,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    /// Where it opens in the browser (Chat's download link, or the Drive file); nil when the
    /// API gives none.
    pub url: Option<String>,
    /// `video/mp4`, `image/png`, …
    #[serde(default)]
    pub content_type: Option<String>,
    /// A file uploaded to Chat: what [`Mailstream::chat_attachment`] downloads. Nil for Drive files.
    #[serde(default)]
    pub resource: Option<String>,
}

#[derive(Deserialize)]
struct FilePath {
    path: String,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub sender: ChatUser,
    pub created: Option<String>,
    pub edited: Option<String>,
    /// Plain text.
    pub text: String,
    /// The text as Google Chat sent it, with its markup (`*bold*`, `<users/…>` mentions, …).
    pub markup: String,
    pub thread: Option<String>,
    pub thread_reply: bool,
    pub mine: bool,
    /// Carries a card the text does not show.
    pub card: bool,
    pub attachments: Vec<ChatAttachment>,
}

/// A message sent from here that Google has not taken yet, or that it refused (`failure`).
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct ChatPending {
    pub id: String,
    pub text: String,
    pub created: Option<String>,
    pub failure: Option<String>,
}

/// The loaded part of a space's history.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatThread {
    pub account: String,
    pub space: String,
    pub title: Option<String>,
    /// The user's `users/…` in this account.
    pub me: Option<String>,
    /// The space's members, for naming mentions.
    pub members: Vec<ChatUser>,
    /// Messages up to this time (ISO 8601) were read.
    pub last_read: Option<String>,
    /// Oldest first.
    pub messages: Vec<ChatMessage>,
    pub pending: Vec<ChatPending>,
    /// Older messages exist ([`Mailstream::chat_earlier`]).
    pub more: bool,
    /// Google answered this session; until then the messages are the cached ones.
    pub loaded: bool,
    /// Why fetching failed, while there is nothing to show.
    pub failure: Option<String>,
    pub loading_earlier: bool,
}

/// People a chat account talks with, and whether it may start new conversations.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatPeople {
    pub can_start: bool,
    pub people: Vec<ChatUser>,
}

/// A space to open.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct ChatSpaceRef {
    pub account: String,
    pub space: String,
}

/// One of an account's calendars.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    /// `#rrggbb`, as the provider shows it.
    pub color: Option<String>,
    /// The account's own calendar.
    pub primary: bool,
    /// The account may add and change events on it.
    #[serde(default)]
    pub writable: bool,
}

/// An account as the calendar shows it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct CalendarAccount {
    pub id: String,
    pub label: String,
    /// `google` or `microsoft`.
    pub provider: String,
    /// Whether its sign-in grants calendar access; accounts signed in before Mailstream asked
    /// for it must sign in again.
    pub granted: bool,
    /// Whether its sign-in lets it add, change and delete events.
    #[serde(default)]
    pub writable: bool,
    pub loading: bool,
    /// Why its last fetch failed; its events are the ones fetched before.
    pub problem: Option<String>,
    /// When its events were fetched (RFC 3339).
    pub fetched: Option<String>,
    pub calendars: Vec<CalendarInfo>,
}

/// An organizer or guest.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct CalendarPerson {
    pub name: Option<String>,
    pub email: Option<String>,
    /// `accepted`, `tentative`, `declined` or `needsAction`.
    pub response: Option<String>,
}

/// One event, or one occurrence of a recurring event. (Not `CalendarEvent`: Dart names the
/// [`Event::Calendar`] variant that.)
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    pub account: String,
    pub calendar: String,
    pub calendar_name: Option<String>,
    /// The calendar's color, `#rrggbb`.
    pub color: Option<String>,
    pub id: String,
    pub title: String,
    /// RFC 3339 in UTC; for all-day events, local midnight of `start_day` and `end_day`.
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
    /// An all-day event's first day and the day after its last: `2026-11-08`.
    pub start_day: Option<String>,
    pub end_day: Option<String>,
    pub location: Option<String>,
    /// The video call to join.
    pub meeting_url: Option<String>,
    pub organizer: Option<CalendarPerson>,
    #[serde(default)]
    pub attendees: Vec<CalendarPerson>,
    /// The account's answer: `accepted`, `tentative`, `declined`, `needsAction`, or
    /// `organizer` for its own meeting; none for an event without guests.
    pub response: Option<String>,
    /// The description, as plain text.
    pub notes: Option<String>,
    /// `notes` is shorter than the provider's description, so editing them here would lose the
    /// rest.
    #[serde(default)]
    pub notes_cut: bool,
    /// The event in the provider's web calendar.
    pub link: Option<String>,
    /// For an occurrence of a recurring event, the series' id: a change or deletion touches
    /// that occurrence alone.
    pub series: Option<String>,
    /// The account organizes it on a calendar it may write to: [`Mailstream::calendar_update`]
    /// and [`Mailstream::calendar_delete`] take it.
    #[serde(default)]
    pub editable: bool,
}

/// Every account and the events of the days asked for, earliest first.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAgenda {
    pub accounts: Vec<CalendarAccount>,
    pub loading: bool,
    pub events: Vec<CalendarItem>,
    /// This computer's time zone (`America/Los_Angeles`), which [`CalendarDraft`] times are in.
    pub time_zone: String,
    /// The days the engine fetches; days outside them show no events because none are fetched.
    pub synced: CalendarDays,
}

/// Days from `from` up to, not including, `until`, as `2026-11-08`.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct CalendarDays {
    pub from: String,
    pub until: String,
}

/// A new event, or what a change sets: a field left `None` stays as it is; an empty location or
/// notes clears it. A time is `start` with `end`, both local wall-clock times
/// (`2026-11-08T10:00`) or, with `all_day`, the first and last days (`2026-11-08`).
#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct CalendarDraft {
    pub title: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
    /// The guests' addresses, the whole list.
    pub attendees: Option<Vec<String>>,
}

/// A meeting invitation (or its cancellation) as a message states it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct MeetingInvitation {
    /// `request`, or `cancel` for a cancelled meeting.
    pub method: String,
    pub uid: Option<String>,
    pub title: String,
    /// RFC 3339 in UTC; for all-day events, local midnight of `start_day` and `end_day`.
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
    pub start_day: Option<String>,
    pub end_day: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<CalendarPerson>,
    /// A recurring meeting, answered as a whole.
    #[serde(default)]
    pub recurring: bool,
    /// One occurrence of a recurring meeting.
    #[serde(default)]
    pub occurrence: bool,
}

/// A message's invitation and the event it names in its account's calendar.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct InvitationStatus {
    pub account: String,
    pub invitation: Option<MeetingInvitation>,
    /// Whether the account's sign-in grants calendar access.
    pub granted: bool,
    /// Whether it may answer (change events); accounts signed in before must sign in again.
    pub editable: bool,
    /// The event, with the account's answer (`response`); none when the calendar lacks it.
    pub event: Option<CalendarItem>,
    /// What [`Mailstream::calendar_respond`] takes: the event's calendar and the event an
    /// answer goes to (a recurring meeting's series).
    pub calendar: Option<String>,
    pub target: Option<String>,
    /// Why the calendar could not be asked.
    pub problem: Option<String>,
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
    /// settings.json changed, from any front end, the CLI or an editor.
    Settings,
    /// Chat accounts or their spaces changed.
    Chat,
    /// The open space's messages changed.
    ChatMessages,
    /// Calendar accounts or their events changed.
    Calendar,
    /// Auto-read marked conversations read, or some were put back ([`Mailstream::auto_read_log`]).
    AutoRead,
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
            "settings" => Self::Settings,
            "chat" => Self::Chat,
            "chatMessages" => Self::ChatMessages,
            "calendar" => Self::Calendar,
            "autoRead" => Self::AutoRead,
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
        #[cfg(feature = "swift-engine")]
        if *swift::USE_SWIFT_ENGINE {
            swift::start();
            return Arc::new(Self {});
        }
        LazyLock::force(&ENGINE);
        Arc::new(Self {})
    }

    pub async fn status(&self) -> Result<Status, MailError> {
        call("status", json!({})).await
    }

    pub async fn accounts(&self) -> Result<Vec<Account>, MailError> {
        call("accounts.list", json!({})).await
    }

    /// Changes the label an account shows; empty shows the address. `None` when the account went
    /// away meanwhile.
    pub async fn rename_account(&self, account: String, label: String) -> Result<Option<Account>, MailError> {
        call("accounts.rename", json!({ "account": account, "label": label })).await
    }

    /// The OAuth client a sign-in with `provider` uses unless given another: the one used last,
    /// else the build's; `None` when there is neither. Its secret stays in the engine.
    pub async fn suggested_client(&self, provider: String) -> Result<Option<OAuthClientInfo>, MailError> {
        call("accounts.client", json!({ "provider": provider })).await
    }

    /// Signs in a new account through the browser and waits (up to 5 minutes) for the person to
    /// finish. Without `client_id` it uses [`Mailstream::suggested_client`]; Google needs a
    /// secret, which for that client's id the engine supplies when `client_secret` is `None`.
    pub async fn add_account(
        &self,
        provider: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Result<Account, MailError> {
        call(
            "accounts.add",
            json!({ "provider": provider, "client-id": client_id, "client-secret": client_secret }),
        )
        .await
    }

    /// Signs the account out and deletes its saved sign-in; mail on the server stays. `confirm`
    /// is the person's go-ahead.
    pub async fn remove_account(&self, account: String, confirm: bool) -> Result<(), MailError> {
        call::<Value>("accounts.remove", json!({ "account": account, "yes": confirm })).await.map(drop)
    }

    /// Every setting in settings.json, with defaults for those it does not set.
    pub async fn settings(&self) -> Result<Settings, MailError> {
        call("config.list", json!({})).await
    }

    /// Writes one setting; the engine applies it at once and sends [`Event::Settings`].
    pub async fn set_setting(&self, key: String, value: SettingValue) -> Result<SettingEntry, MailError> {
        call("config.set", json!({ "key": key, "value": value.typed() })).await
    }

    /// Puts a setting back to its default.
    pub async fn reset_setting(&self, key: String) -> Result<SettingEntry, MailError> {
        call("config.reset", json!({ "key": key })).await
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
            "cached": query.cached,
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

    /// Forgets every AI tag and tags the current list again; `disabled` while tagging is off.
    pub async fn retag(&self) -> Result<(), MailError> {
        call::<Value>("mail.retag", json!({})).await.map(drop)
    }

    /// What auto-read marked read and nobody undid, newest first.
    pub async fn auto_read_log(&self) -> Result<AutoReadLog, MailError> {
        call("mail.autoread", json!({})).await
    }

    /// Marks auto-read conversations unread again: `ids`, else those of its latest pass. Auto-read
    /// then leaves them alone until a new message arrives.
    pub async fn undo_auto_read(&self, ids: Option<Vec<String>>) -> Result<BatchResult, MailError> {
        call("mail.autoread.undo", json!({ "ids": ids })).await
    }

    /// Sends `draft` as a new plain-text message with its attachments. Sends at once: `confirm` is the person's
    /// go-ahead; without it the engine only answers what it would send (`confirm`).
    pub async fn send(&self, draft: Draft, confirm: bool) -> Result<Sent, MailError> {
        call(
            "mail.send",
            json!({
                "from": draft.from, "to": draft.to, "cc": draft.cc, "bcc": draft.bcc,
                "subject": draft.subject, "body": draft.body, "attachments": draft.attachments, "yes": confirm,
            }),
        )
        .await
    }

    /// Replies with the original thread headers and quote. Missing overrides keep the reply plan.
    /// Sends at once: `confirm` is the person's go-ahead.
    pub async fn reply(
        &self, id: String, body: String, all: bool, overrides: Option<ReplyOverrides>, confirm: bool,
    ) -> Result<Sent, MailError> {
        let mut args = json!({ "id": id, "body": body, "all": all, "yes": confirm });
        if let Some(edits) = overrides {
            args["to"] = json!(edits.to);
            args["cc"] = json!(edits.cc);
            args["bcc"] = json!(edits.bcc);
            args["subject"] = json!(edits.subject);
            args["attachments"] = json!(edits.attachments);
        }
        call("mail.reply", args).await
    }

    /// Fetches new mail and counts now.
    pub async fn refresh(&self) -> Result<(), MailError> {
        call::<Value>("refresh", json!({})).await.map(drop)
    }

    /// Google Chat accounts and their spaces, from what the engine has loaded.
    pub async fn chat_spaces(&self, account: Option<String>) -> Result<ChatSpaces, MailError> {
        call("chat.spaces", json!({ "account": account })).await
    }

    /// Loads another page of older spaces; [`Event::Chat`] follows.
    pub async fn chat_more(&self, account: Option<String>) -> Result<(), MailError> {
        call::<Value>("chat.more", json!({ "account": account })).await.map(drop)
    }

    /// Fetches every chat account's spaces now.
    pub async fn chat_refresh(&self) -> Result<(), MailError> {
        call::<Value>("chat.refresh", json!({})).await.map(drop)
    }

    /// Opens a space: its cached messages now, the newest from Google after
    /// ([`Event::ChatMessages`]), kept current and read while it stays open.
    pub async fn chat_open(&self, account: String, space: String) -> Result<ChatThread, MailError> {
        call("chat.open", json!({ "account": account, "space": space })).await
    }

    pub async fn chat_close(&self) -> Result<(), MailError> {
        call::<Value>("chat.close", json!({})).await.map(drop)
    }

    /// What is loaded of a space's history.
    pub async fn chat_messages(&self, account: String, space: String) -> Result<ChatThread, MailError> {
        call("chat.messages", json!({ "account": account, "space": space })).await
    }

    /// Loads the page before the oldest loaded message.
    pub async fn chat_earlier(&self, account: String, space: String) -> Result<ChatThread, MailError> {
        call("chat.earlier", json!({ "account": account, "space": space })).await
    }

    /// Sends `text` (shown pending at once) and returns once Google took it. `confirm` is the
    /// person's go-ahead.
    pub async fn chat_send(
        &self,
        account: String,
        space: String,
        text: String,
        thread: Option<String>,
        confirm: bool,
    ) -> Result<(), MailError> {
        call::<Value>(
            "chat.send",
            json!({ "account": account, "space": space, "text": text, "thread": thread, "yes": confirm }),
        )
        .await
        .map(drop)
    }

    /// People `account` chats with, most recently active first.
    pub async fn chat_people(&self, account: String) -> Result<ChatPeople, MailError> {
        call("chat.people", json!({ "account": account })).await
    }

    /// The direct message with one person or the group chat with several, found or started.
    pub async fn chat_start(&self, account: String, people: Vec<String>) -> Result<ChatSpaceRef, MailError> {
        call("chat.start", json!({ "account": account, "people": people })).await
    }

    /// An uploaded attachment (its `resource`) as a file in the cache, downloaded the first time;
    /// kept under `name`, so its extension says what it is. Answers the file's path.
    pub async fn chat_attachment(&self, account: String, resource: String, name: String) -> Result<String, MailError> {
        let file: FilePath =
            call("chat.attachment", json!({ "account": account, "resource": resource, "name": name })).await?;
        Ok(file.path)
    }

    /// Every account and the events of `days` days from `from` (`2026-11-08`; today when
    /// `None`), from what the engine last fetched (every five minutes); [`Event::Calendar`]
    /// follows each fetch.
    pub async fn calendar_events(&self, from: Option<String>, days: u32) -> Result<CalendarAgenda, MailError> {
        call("calendar.events", json!({ "from": from, "days": days })).await
    }

    /// Fetches every calendar account's events now.
    pub async fn calendar_refresh(&self) -> Result<(), MailError> {
        call::<Value>("calendar.refresh", json!({})).await.map(drop)
    }

    /// The invitation `message` of conversation `id` carries and the event it names in the
    /// account's calendar, with the account's answer; [`Event::Calendar`] follows changes.
    pub async fn calendar_invitation(&self, id: String, message: String) -> Result<InvitationStatus, MailError> {
        call("calendar.invitation", json!({ "id": id, "message": message })).await
    }

    /// Answers an invitation (`accepted`, `tentative` or `declined`) through the calendar, which
    /// tells the organizer: `event` and `calendar` are [`InvitationStatus::target`] and
    /// [`InvitationStatus::calendar`]. `confirm` is the person's go-ahead.
    pub async fn calendar_respond(
        &self,
        account: String,
        calendar: Option<String>,
        event: String,
        response: String,
        confirm: bool,
    ) -> Result<(), MailError> {
        call::<Value>(
            "calendar.respond",
            json!({ "account": account, "calendar": calendar, "event": event, "response": response, "yes": confirm }),
        )
        .await
        .map(drop)
    }

    /// Adds an event to `calendar` of `account` (its own calendar when `None`) and invites the
    /// draft's guests; answers the event as the calendar lists it, which [`Event::Calendar`]
    /// also shows at once. The draft needs a title and a start.
    pub async fn calendar_create(
        &self,
        account: String,
        calendar: Option<String>,
        draft: CalendarDraft,
    ) -> Result<CalendarItem, MailError> {
        let mut args = draft_arguments(draft);
        args["account"] = json!(account);
        args["calendar"] = json!(calendar);
        call("calendar.create", args).await
    }

    /// Changes what `draft` gives of an [`CalendarItem::editable`] event, telling its guests.
    pub async fn calendar_update(
        &self,
        account: String,
        calendar: String,
        id: String,
        draft: CalendarDraft,
    ) -> Result<CalendarItem, MailError> {
        let mut args = draft_arguments(draft);
        args["account"] = json!(account);
        args["calendar"] = json!(calendar);
        args["id"] = json!(id);
        call("calendar.update", args).await
    }

    /// Deletes an [`CalendarItem::editable`] event, telling its guests it is cancelled.
    /// `confirm` is the person's go-ahead.
    pub async fn calendar_delete(&self, account: String, calendar: String, id: String, confirm: bool) -> Result<(), MailError> {
        call::<Value>("calendar.delete", json!({ "account": account, "calendar": calendar, "id": id, "yes": confirm }))
            .await
            .map(drop)
    }

    /// Calls `listener` on the main thread for every change from now on, while the process runs.
    pub fn subscribe(&self, listener: Arc<dyn EventListener>) {
        #[cfg(feature = "swift-engine")]
        if *swift::USE_SWIFT_ENGINE {
            swift::subscribe(listener);
            return;
        }
        main_thread::adopt_current_thread();
        ENGINE.subscribe(move |event| {
            let listener = listener.clone();
            main_thread::run(move || listener.on_event(Change { event: Event::named(event) }));
        });
    }
}

/// For Rust hosts that link this crate directly (GPUI) and run an event loop of their own.
impl Mailstream {
    /// Calls `listener` on an engine thread for every change from now on, while the process runs.
    /// For a host whose main thread is not the queue [`Mailstream::subscribe`] posts to (GPUI on
    /// Linux runs its own loop, not GLib's): `listener` hands the event over itself.
    pub fn subscribe_any_thread(&self, listener: Arc<dyn EventListener>) {
        #[cfg(feature = "swift-engine")]
        if *swift::USE_SWIFT_ENGINE {
            swift::subscribe(listener);
            return;
        }
        ENGINE.subscribe(move |event| listener.on_event(Change { event: Event::named(event) }));
    }
}

/// Runs the main thread's queue, where listeners are called, for a host without a run loop (a
/// command-line tool). Call it on the main thread; it never returns.
#[uniffi::export]
pub fn run_main_loop() {
    #[cfg(feature = "swift-engine")]
    if *swift::USE_SWIFT_ENGINE {
        swift::run_main_loop();
    }
    main_thread::run_loop()
}

/// Every setting at its default, with its range, without reading settings.json: what a front end
/// lays out its first frames with, before `Mailstream::settings` answers. The engine's
/// descriptions are the one copy of the defaults.
#[uniffi::export]
pub fn setting_defaults() -> Vec<SettingEntry> {
    mailstream_engine::settings::Setting::defaults()
        .into_iter()
        .map(|entry| serde_json::from_value(entry).expect("the engine describes every setting"))
        .collect()
}

/// The process's engine, started on first use.
static ENGINE: LazyLock<mailstream_engine::Engine> =
    LazyLock::new(|| mailstream_engine::Engine::start(mailstream_engine::Config::for_this_app()));

impl From<mailstream_engine::EngineError> for MailError {
    fn from(error: mailstream_engine::EngineError) -> Self {
        Self::Failed { code: error.code, message: error.message }
    }
}

/// Any engine command by name (the control socket's: `config.get`, `accounts.list`, …) with its
/// arguments, answered as `T`. The typed methods use it; Rust tools such as `examples/bench.rs`
/// call commands that have none.
pub async fn call<T: DeserializeOwned>(command: &str, args: Value) -> Result<T, MailError> {
    #[cfg(feature = "swift-engine")]
    let result = if *swift::USE_SWIFT_ENGINE {
        swift::call(command, args).await?
    } else {
        ENGINE.call(command, args).await?
    };
    #[cfg(not(feature = "swift-engine"))]
    let result = ENGINE.call(command, args).await?;
    serde_json::from_value(result)
        .map_err(|error| MailError::new("decode", format!("Unexpected result of {command}: {error}")))
}

/// `calendar.create` and `calendar.update`'s arguments for `draft`; `null` ones are left out.
fn draft_arguments(draft: CalendarDraft) -> Value {
    json!({
        "title": draft.title, "start": draft.start, "end": draft.end, "all-day": draft.all_day.then_some(true),
        "location": draft.location, "notes": draft.notes, "attendees": draft.attendees,
    })
}
