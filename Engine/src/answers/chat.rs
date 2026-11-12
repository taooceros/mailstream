//! Google Chat answers: accounts, spaces, a space's messages and the people to start one with.
use serde::{Deserialize, Serialize};

use crate::chat_markup::ChatBlock;
use crate::chat_model::ChatSpaceKind;
use crate::display::Avatar;

/// A Google Chat account: a Google account signed in with Chat access.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatSpace {
    pub account: String,
    /// `spaces/…`.
    pub space: String,
    pub kind: ChatSpaceKind,
    pub title: String,
    /// "Brent: see you" / "You: thanks".
    pub preview: String,
    /// The date as lists show it, and its date group.
    pub date: String,
    pub section: String,
    /// ISO 8601.
    pub activity: Option<String>,
    pub unread: bool,
    pub attachment: bool,
    /// The person the row shows: their lowercased address, else their Chat user id; empty for none.
    pub avatar_key: String,
    pub avatar_name: String,
    /// What the row's avatar keys on: `avatar_key`, else the space.
    pub avatar_color_key: String,
    /// The picture Google Chat publishes for them.
    pub avatar_url: Option<String>,
    /// The row's avatar without a picture: a person's initial, or a space's.
    #[serde(skip)]
    #[uniffi(default)]
    pub avatar: Avatar,
}

/// The chat accounts and every space loaded, most recently active first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct ChatSpaces {
    pub accounts: Vec<ChatAccount>,
    pub loading: bool,
    /// Older spaces exist beyond these (`chat.more`).
    pub more: bool,
    pub spaces: Vec<ChatSpace>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatUser {
    /// `users/…`.
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub avatar_key: String,
    pub avatar_url: Option<String>,
    pub bot: bool,
    #[serde(skip)]
    #[uniffi(default)]
    pub avatar: Avatar,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    /// Where it opens in the browser (Chat's download link, or the Drive file); nil when the
    /// API gives none.
    pub url: Option<String>,
    /// `video/mp4`, `image/png`, …
    #[serde(default)]
    pub content_type: Option<String>,
    /// A file uploaded to Chat: what `chat.attachment` downloads. Nil for Drive files.
    #[serde(default)]
    pub resource: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub sender: ChatUser,
    /// ISO 8601.
    pub created: Option<String>,
    pub edited: Option<String>,
    /// Plain text.
    pub text: String,
    /// The text as Google Chat sent it, with its markup (Markdown, mentions; `chat_markup.rs`).
    pub markup: String,
    pub thread: Option<String>,
    pub thread_reply: bool,
    pub mine: bool,
    /// Carries a card the text does not show.
    pub card: bool,
    pub attachments: Vec<ChatAttachment>,
    /// `markup` laid out, mentions named.
    #[serde(skip)]
    #[uniffi(default = [])]
    pub body: Vec<ChatBlock>,
    /// `created`'s time of day.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub time: String,
    /// Its place in the loaded log (`display::chat_runs`): the day divider above it (empty on the
    /// day of the message before), and whether it continues the run above, without a head.
    #[serde(skip)]
    #[uniffi(default = "")]
    pub divider: String,
    #[serde(skip)]
    #[uniffi(default = false)]
    pub continued: bool,
}

/// A message sent from here that Google has not taken yet, or that it refused (`failure`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct ChatPending {
    pub id: String,
    pub text: String,
    /// ISO 8601.
    pub created: Option<String>,
    pub failure: Option<String>,
}

/// The loaded part of a space's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
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
    /// Older messages exist (`chat.earlier`).
    pub more: bool,
    /// Google answered this session; until then the messages are the cached ones.
    pub loaded: bool,
    /// Why fetching failed, while there is nothing to show.
    pub failure: Option<String>,
    pub loading_earlier: bool,
}

/// People a chat account talks with, and whether it may start new conversations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatPeople {
    pub can_start: bool,
    pub people: Vec<ChatUser>,
}

/// A space to open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct ChatSpaceRef {
    pub account: String,
    pub space: String,
}
