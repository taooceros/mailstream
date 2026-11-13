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
mod display;
mod mail_html;
mod main_thread;
mod people;
#[cfg(feature = "swift-engine")]
mod swift;

#[cfg(feature = "blitz")]
pub use html_page::{HtmlPage, HtmlPart, HtmlSize, HtmlTheme, HtmlTile};
pub use display::{
    ChatRunEntry, Recipients, SubjectParts, avatar, chat_body, chat_draft_body, chat_runs, day_divider, forward_body,
    forward_subject, full_date, list_date, list_section, message_date, message_time, parse_recipients, reply_subject,
    split_recipients, split_subject,
};
pub use mail_html::{
    InlineParts, MailWebView, QUOTES, inline_parts_folder, is_mail_link, mail_web_document, mail_web_script, served_inline_part,
};
/// The engine's values, as commands answer them (MS-238): front ends get these types as they are.
pub use mailstream_engine::answers::*;
pub use mailstream_engine::chat_markup::{ChatBlock, ChatBlockKind, ChatSpan, ChatStyle};
/// For Rust hosts (GPUI): the stable color slot of a key, as avatars pick theirs.
pub use mailstream_engine::display::{AVATAR_SLOTS, Avatar, ChatRun, stable_slot};
pub use mailstream_engine::support_directory;

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
    /// Conversation id when forwarding a message, so the engine quotes its HTML body.
    #[uniffi(default = None)]
    pub forward: Option<String>,
    /// Message id within `forward` (`Message::id`); `None` quotes its latest.
    #[uniffi(default = None)]
    pub forward_message: Option<String>,
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
    /// The id of the conversation's message to answer (`Message::id`); `None` answers its latest.
    #[uniffi(default = None)]
    pub message: Option<String>,
}

#[derive(Deserialize)]
struct FilePath {
    path: String,
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

/// What changed in the engine; a front end re-reads what it shows of it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Event {
    Accounts,
    Mailboxes,
    /// The inbox list changed: new mail, flags, moves; sent as each mailbox's answer comes in.
    Conversations,
    Previews,
    Tags,
    Sync,
    /// `status` changed its failures: the last one, an account that can't connect
    /// (`AccountStatus::problem`) or connects again, the AI tagger's.
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
        let mut listing: Listing = call("mail.list", args).await?;
        listing.dress();
        Ok(listing)
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
            args["message"] = json!(edits.message);
        }
        call("mail.reply", args).await
    }

    /// An uploaded attachment (its `resource`) as a file in the cache, downloaded the first time;
    /// kept under `name`, so its extension says what it is. Answers the file's path.
    pub async fn chat_attachment(&self, account: String, resource: String, name: String) -> Result<String, MailError> {
        let file: FilePath =
            call("chat.attachment", json!({ "account": account, "resource": resource, "name": name })).await?;
        Ok(file.path)
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

/// Typed methods that each send one engine command, written `fn method(parameters) -> Answer =
/// "command" { "argument": value, … };`: without `-> Answer` when the method drops the answer,
/// with `dressed` before the `;` when the answer's display fields are filled in (`display.rs`).
/// The command and its arguments are checked against the engine's command table
/// (`mailstream_engine::commands::takes`) when this crate compiles. Methods whose arguments need
/// more than one expression each are written out in the `impl` above.
macro_rules! commands {
    ($(
        $(#[$doc:meta])*
        fn $method:ident($($parameter:ident: $type:ty),* $(,)?) $(-> $answer:ty)? = $command:literal {
            $($argument:literal: $value:expr),* $(,)?
        } $($dressed:ident)?;
    )*) => {
        $(command! {
            $(#[$doc])* fn $method($($parameter: $type),*) $(-> $answer)? = $command { $($argument: $value),* } $($dressed)?
        })*
    };
}

/// One method of [`commands!`], in an exported `impl` block of its own.
macro_rules! command {
    (
        $(#[$doc:meta])* fn $method:ident($($parameter:ident: $type:ty),*) -> $answer:ty = $command:literal {
            $($argument:literal: $value:expr),*
        } dressed
    ) => {
        #[uniffi::export]
        impl Mailstream {
            $(#[$doc])*
            pub async fn $method(&self, $($parameter: $type),*) -> Result<$answer, MailError> {
                checked!($command, $($argument),*);
                let mut answer: $answer = call($command, json!({ $($argument: $value),* })).await?;
                answer.dress();
                Ok(answer)
            }
        }
    };
    (
        $(#[$doc:meta])* fn $method:ident($($parameter:ident: $type:ty),*) -> $answer:ty = $command:literal {
            $($argument:literal: $value:expr),*
        }
    ) => {
        #[uniffi::export]
        impl Mailstream {
            $(#[$doc])*
            pub async fn $method(&self, $($parameter: $type),*) -> Result<$answer, MailError> {
                checked!($command, $($argument),*);
                call($command, json!({ $($argument: $value),* })).await
            }
        }
    };
    (
        $(#[$doc:meta])* fn $method:ident($($parameter:ident: $type:ty),*) = $command:literal {
            $($argument:literal: $value:expr),*
        }
    ) => {
        #[uniffi::export]
        impl Mailstream {
            $(#[$doc])*
            pub async fn $method(&self, $($parameter: $type),*) -> Result<(), MailError> {
                checked!($command, $($argument),*);
                call::<Value>($command, json!({ $($argument: $value),* })).await.map(drop)
            }
        }
    };
}

/// Fails the build when the engine's command table has no `command` taking these arguments.
macro_rules! checked {
    ($command:literal, $($argument:literal),*) => {
        const _: () = assert!(
            mailstream_engine::commands::takes($command, &[$($argument),*]),
            concat!("The engine's command table has no ", $command, " taking these arguments."),
        );
    };
}

commands! {
    fn status() -> Status = "status" {};

    fn accounts() -> Vec<Account> = "accounts.list" {};

    /// Changes the label an account shows; empty shows the address. `None` when the account went
    /// away meanwhile.
    fn rename_account(account: String, label: String) -> Option<Account> = "accounts.rename" {
        "account": account, "label": label
    };

    /// The OAuth client a sign-in with `provider` uses unless given another: the one used last,
    /// else the build's; `None` when there is neither. Its secret stays in the engine.
    fn suggested_client(provider: String) -> Option<OAuthClientInfo> = "accounts.client" { "provider": provider };

    /// Signs in a new account through the browser and waits (up to 5 minutes) for the person to
    /// finish. Without `client_id` it uses [`Mailstream::suggested_client`]; Google needs a
    /// secret, which for that client's id the engine supplies when `client_secret` is `None`.
    fn add_account(provider: String, client_id: Option<String>, client_secret: Option<String>) -> Account = "accounts.add" {
        "provider": provider, "client-id": client_id, "client-secret": client_secret
    };

    /// Lets `account` use `service` (`contacts`, `chat`, `calendar`): the provider's consent page
    /// for it opens in the browser, and this waits (up to 5 minutes) for the person to finish.
    /// The account as it is then; `not_granted` when the person did not allow it after all.
    fn connect_account(account: String, service: String) -> Account = "accounts.connect" {
        "account": account, "service": service
    };

    /// Signs the account out and deletes its saved sign-in; mail on the server stays. `confirm`
    /// is the person's go-ahead.
    fn remove_account(account: String, confirm: bool) = "accounts.remove" { "account": account, "yes": confirm };

    /// Every setting in settings.json, with defaults for those it does not set.
    fn settings() -> Settings = "config.list" {};

    /// Writes one setting; the engine applies it at once and sends [`Event::Settings`].
    fn set_setting(key: String, value: SettingValue) -> SettingEntry = "config.set" {
        "key": key, "value": value.typed()
    };

    /// Puts a setting back to its default.
    fn reset_setting(key: String) -> SettingEntry = "config.reset" { "key": key };

    /// Each account's mailboxes, by account id; only `account`'s when given.
    fn mailboxes(account: Option<String>) -> HashMap<String, Vec<Mailbox>> = "mailboxes" { "account": account };

    /// The providers' own search over every mailbox but trash and junk.
    fn search(query: String, account: Option<String>, limit: u32) -> Listing = "mail.search" {
        "query": query, "account": account, "limit": limit
    } dressed;

    /// A conversation from a listing, with text bodies of at most `max_chars` characters and,
    /// with `html`, the HTML bodies.
    fn show(id: String, max_chars: u32, html: bool) -> Thread = "mail.show" {
        "id": id, "max-chars": max_chars, "html": html
    } dressed;

    /// The preview line of each conversation's latest message by id, loading those not loaded yet.
    fn previews(ids: Vec<String>) -> HashMap<String, Option<String>> = "mail.previews" { "ids": ids };

    /// Each sender's picture as a `file:` URL of a PNG in the cache, by lowercased address; nil
    /// when there is none. `automated` names the addresses a site icon may stand in for.
    fn avatars(addresses: Vec<String>, automated: Vec<String>) -> HashMap<String, Option<String>> = "mail.avatars" {
        "addresses": addresses, "automated": automated
    };

    fn set_read(ids: Vec<String>, read: bool) -> BatchResult = "mail.read" { "ids": ids, "unread": !read };

    fn set_starred(ids: Vec<String>, starred: bool) -> BatchResult = "mail.star" { "ids": ids, "off": !starred };

    fn archive(ids: Vec<String>) -> BatchResult = "mail.archive" { "ids": ids };

    /// Conversations already in the trash are deleted for good, which needs `confirm`.
    fn trash(ids: Vec<String>, confirm: bool) -> BatchResult = "mail.trash" { "ids": ids, "yes": confirm };

    fn restore(ids: Vec<String>) -> BatchResult = "mail.restore" { "ids": ids };

    /// Corrects the AI tag; each of `category`, `waiting` and `labels` (at most two) changes
    /// only when given.
    fn tag(
        ids: Vec<String>,
        category: Option<String>,
        waiting: Option<bool>,
        labels: Option<Vec<String>>,
    ) -> TagResult = "mail.tag" {
        "ids": ids, "category": category, "waiting": waiting, "labels": labels
    };

    /// Forgets every AI tag and tags the current list again; `disabled` while tagging is off.
    fn retag() = "mail.retag" {};

    /// What auto-read marked read and nobody undid, newest first.
    fn auto_read_log() -> AutoReadLog = "mail.autoread" {};

    /// Marks auto-read conversations unread again: `ids`, else those of its latest pass. Auto-read
    /// then leaves them alone until a new message arrives.
    fn undo_auto_read(ids: Option<Vec<String>>) -> BatchResult = "mail.autoread.undo" { "ids": ids };

    /// Takes back step `step` (an action's `undo`), else the latest action on mail, on the server
    /// too; `nothing` when none is left.
    fn undo(step: Option<u64>) -> Undone = "mail.undo" { "step": step };

    /// Sends `draft` with its attachments. Sends at once: `confirm` is the person's
    /// go-ahead; without it the engine only answers what it would send (`confirm`).
    fn send(draft: Draft, confirm: bool) -> Sent = "mail.send" {
        "from": draft.from, "to": draft.to, "cc": draft.cc, "bcc": draft.bcc,
        "subject": draft.subject, "body": draft.body, "attachments": draft.attachments,
        "forward": draft.forward, "forward-message": draft.forward_message, "yes": confirm
    };

    /// Fetches new mail and counts now.
    fn refresh() = "refresh" {};

    /// Google Chat accounts and their spaces, from what the engine has loaded.
    fn chat_spaces(account: Option<String>) -> ChatSpaces = "chat.spaces" { "account": account } dressed;

    /// Loads another page of older spaces; [`Event::Chat`] follows.
    fn chat_more(account: Option<String>) = "chat.more" { "account": account };

    /// Fetches every chat account's spaces now.
    fn chat_refresh() = "chat.refresh" {};

    /// Opens a space: its cached messages now, the newest from Google after
    /// ([`Event::ChatMessages`]), kept current and read while it stays open.
    fn chat_open(account: String, space: String) -> ChatThread = "chat.open" {
        "account": account, "space": space
    } dressed;

    fn chat_close() = "chat.close" {};

    /// What is loaded of a space's history.
    fn chat_messages(account: String, space: String) -> ChatThread = "chat.messages" {
        "account": account, "space": space
    } dressed;

    /// Loads the page before the oldest loaded message.
    fn chat_earlier(account: String, space: String) -> ChatThread = "chat.earlier" {
        "account": account, "space": space
    } dressed;

    /// Sends `text` (shown pending at once) and returns once Google took it. `confirm` is the
    /// person's go-ahead.
    fn chat_send(account: String, space: String, text: String, thread: Option<String>, confirm: bool) = "chat.send" {
        "account": account, "space": space, "text": text, "thread": thread, "yes": confirm
    };

    /// People `account` chats with, most recently active first.
    fn chat_people(account: String) -> ChatPeople = "chat.people" { "account": account } dressed;

    /// The direct message with one person or the group chat with several, found or started.
    fn chat_start(account: String, people: Vec<String>) -> ChatSpaceRef = "chat.start" {
        "account": account, "people": people
    };

    /// Every account and the events of `days` days from `from` (`2026-11-08`; today when
    /// `None`), from what the engine last fetched (every five minutes); [`Event::Calendar`]
    /// follows each fetch.
    fn calendar_events(from: Option<String>, days: u32) -> CalendarAgenda = "calendar.events" {
        "from": from, "days": days
    };

    /// Fetches every calendar account's events now.
    fn calendar_refresh() = "calendar.refresh" {};

    /// Shows `calendar` of `account` (any in [`CalendarAccount::calendars`], others' shared or
    /// subscribed ones too) or hides it; the engine keeps the choice and fetches what it lacks.
    fn calendar_show(account: String, calendar: String, shown: bool) = "calendar.show" {
        "account": account, "calendar": calendar, "off": !shown
    };

    /// The invitation `message` of conversation `id` carries and the event it names in the
    /// account's calendar, with the account's answer; [`Event::Calendar`] follows changes.
    fn calendar_invitation(id: String, message: String) -> InvitationStatus = "calendar.invitation" {
        "id": id, "message": message
    };

    /// Answers an invitation (`accepted`, `tentative` or `declined`) through the calendar, which
    /// tells the organizer: `event` and `calendar` are [`InvitationStatus::target`] and
    /// [`InvitationStatus::calendar`]. `confirm` is the person's go-ahead.
    fn calendar_respond(
        account: String,
        calendar: Option<String>,
        event: String,
        response: String,
        confirm: bool,
    ) = "calendar.respond" {
        "account": account, "calendar": calendar, "event": event, "response": response, "yes": confirm
    };

    /// Deletes an [`CalendarItem::editable`] event, telling its guests it is cancelled.
    /// `confirm` is the person's go-ahead.
    fn calendar_delete(account: String, calendar: String, id: String, confirm: bool) = "calendar.delete" {
        "account": account, "calendar": calendar, "id": id, "yes": confirm
    };
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
