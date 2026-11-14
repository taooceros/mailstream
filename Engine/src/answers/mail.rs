//! Mail answers: mailboxes, listings, threads, tags, auto-read and what was sent.
use serde::{Deserialize, Serialize};

use super::{Address, BatchFailure, Category, MailboxRole, MeetingInvitation, address_text};
use crate::display::Avatar;
pub use crate::mail::AutoReadReason;

/// One of an account's mailboxes; `mailboxes` answers them by account id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Mailbox {
    pub id: String,
    /// The role's title (`Inbox`, `All Mail`, …), or the folder's own name for `other`.
    pub name: String,
    pub role: MailboxRole,
    pub unread: Option<i64>,
    pub total: Option<i64>,
}

/// One conversation as lists show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Addresses the conversation in later calls while the engine runs.
    pub id: String,
    pub account: String,
    pub subject: String,
    /// ISO 8601, of the latest message.
    pub date: Option<String>,
    /// The latest message's sender, the user's own address when they wrote it.
    #[serde(with = "address_text::optional")]
    pub from: Option<Address>,
    /// The user wrote the latest message: lists name its sender `me`.
    #[serde(default)]
    pub from_me: bool,
    /// When `from_me` is true, whom the user's latest message went to, so lists show them (a
    /// group's own address, or the people of a group) rather than the user (MS-337, MS-284): its
    /// recipients but the user, else the last other person who wrote.
    #[serde(default, with = "address_text::list")]
    #[uniffi(default)]
    pub peers: Vec<Address>,
    /// Senders' names, `me` for the user's own addresses.
    pub participants: Vec<String>,
    pub message_count: u32,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    /// Roles of the mailboxes its messages are in.
    pub mailboxes: Vec<MailboxRole>,
    pub snippet: Option<String>,
    /// The AI tag's category.
    pub category: Option<Category>,
    /// Waits on the user's reply or action.
    pub waiting: bool,
    pub labels: Vec<String>,
    /// In the inbox only as a conversation the user started that nobody has answered yet (their
    /// sent mail alone): Archive takes it out of the inbox.
    #[serde(default)]
    pub started: bool,
    /// The subject as lists show it: leading `[list]` tags (empty when none) apart from the title,
    /// which drops reply and forward prefixes.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub tag: String,
    #[serde(skip)]
    #[uniffi(default = "")]
    pub title: String,
    /// `date` as the list shows it, and its date group.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub when: String,
    #[serde(skip)]
    #[uniffi(default = "")]
    pub section: String,
    /// The avatar of the first of `peers`, else of `from`, else of the first participant.
    #[serde(skip)]
    #[uniffi(default)]
    pub avatar: Avatar,
}

impl Conversation {
    /// Service mail the Smart Inbox folds into a bundle; anything waiting on the user stays out.
    pub fn bundle(&self) -> Option<Category> {
        self.category.filter(|category| *category != Category::People && !self.waiting)
    }

    /// Something sits in an inbox-like mailbox (`MailStore.canArchive`), or it is in the inbox
    /// as a conversation the user started.
    pub fn can_archive(&self) -> bool {
        self.started
            || self.mailboxes.iter().any(|role| {
                !matches!(role, MailboxRole::Sent | MailboxRole::All | MailboxRole::Archive | MailboxRole::Trash | MailboxRole::Drafts)
            })
    }

    /// Something sits in the trash or junk (`MailStore.canRestore`).
    pub fn can_restore(&self) -> bool {
        self.mailboxes.iter().any(|role| matches!(role, MailboxRole::Trash | MailboxRole::Junk))
    }

    /// "Waiting on you" can be set or cleared: tagged, and not a newsletter.
    pub fn can_mark_waiting(&self) -> bool {
        self.category.is_some_and(|category| category != Category::Newsletter)
    }

    /// A conversation between people reads as a chat log (MS-73): more than one message, and
    /// tagged People or not tagged at all.
    pub fn chat_log(&self) -> bool {
        self.message_count > 1 && self.category.is_none_or(|category| category == Category::People)
    }
}

/// A page of conversations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Listing {
    pub conversations: Vec<Conversation>,
    /// Providers that could not be reached; the conversations are the others'.
    pub failures: Vec<String>,
    /// The following page's cursor (`mail.list --cursor`); nil at the end, and absent from
    /// searches' JSON.
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: i64,
    /// Where the engine keeps the file once fetched.
    pub path: String,
}

/// One message of a thread. Its body's fields (`cc`, `text`, `truncated`, `attachments`, `html`,
/// `invitation`) are there when the body loaded, `error` when it did not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Message {
    pub id: String,
    pub mailbox: String,
    pub role: MailboxRole,
    #[serde(with = "address_text::list")]
    pub from: Vec<Address>,
    #[serde(with = "address_text::list")]
    pub to: Vec<Address>,
    #[serde(default, with = "address_text::list")]
    pub cc: Vec<Address>,
    pub date: Option<String>,
    pub subject: String,
    pub unread: bool,
    pub starred: bool,
    /// Plain text, or the HTML body as text; nil when the body could not load (`error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// The HTML body when asked for (`mail.show --html`); inline images point at files in the
    /// cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The meeting invitation it carries; `calendar.invitation` says how the account answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation: Option<MeetingInvitation>,
    /// `date` as the reader shows it, and its time of day.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub when: String,
    #[serde(skip)]
    #[uniffi(default = "")]
    pub time: String,
    /// The avatar of its sender.
    #[serde(skip)]
    #[uniffi(default)]
    pub avatar: Avatar,
    /// Its place when its thread reads as a chat log (`display::chat_runs`): the day divider
    /// above it (empty on the day of the message before), and whether it continues the run above
    /// (same sender, same day, within the hour) without a head.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub divider: String,
    #[serde(skip)]
    #[uniffi(default = false)]
    pub continued: bool,
}

impl Message {
    /// Who sent it; an unknown sender when the headers name none.
    pub fn sender(&self) -> Address {
        self.from.first().cloned().unwrap_or_else(|| Address::new(None, "(unknown)"))
    }

    /// The HTML body a reader draws instead of the text: whenever the message has one (MS-244).
    /// The text is no sign of a plain mail: the engine derives it from the HTML when the mail has
    /// no plain part, and GitHub's notifications carry both.
    pub fn html_body(&self) -> Option<&str> {
        self.html.as_deref().filter(|html| !html.trim().is_empty())
    }
}

/// A conversation with its messages and bodies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Thread {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub messages: Vec<Message>,
}

/// A conversation's AI tag after `mail.tag` corrected it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Tagged {
    pub id: String,
    pub category: Category,
    pub waiting: bool,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct TagResult {
    pub done: Vec<Tagged>,
    pub failed: Vec<BatchFailure>,
    /// The step `mail.undo` takes the correction back with, when it changed anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<u64>,
}

/// What a step `mail.undo` takes back had done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "lowercase")]
pub enum UndoAction {
    Read,
    Unread,
    Star,
    Unstar,
    Archive,
    Trash,
    Restore,
    /// A correction of the AI tag (`mail.tag`).
    Tag,
}

/// A step `mail.undo` took back: which one, what it had done, and the conversations it was taken
/// back for and those the provider refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Undone {
    pub step: u64,
    pub action: UndoAction,
    pub done: Vec<String>,
    pub failed: Vec<BatchFailure>,
}

/// A conversation auto-read marked read and nobody undid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct AutoReadEntry {
    pub id: String,
    pub account: String,
    pub sender: String,
    pub subject: String,
    /// A notification or a newsletter.
    pub category: Category,
    pub reason: AutoReadReason,
    /// ISO 8601.
    pub at: String,
    /// The pass that marked it read; `mail.autoread.undo` without ids undoes the latest.
    pub batch: u64,
}

/// What auto-read (`autoRead.*` settings) marked read, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct AutoReadLog {
    pub enabled: bool,
    pub days: i64,
    pub entries: Vec<AutoReadEntry>,
}

/// What was sent: the account it went from, its recipients (`Name <address>`; replies include
/// To, Cc and Bcc) and subject. The JSON adds `"sent": true`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Sent {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}
