//! Google Chat values as the Swift engine keeps them (`Backend/Chat.swift`, `Store/ChatStore.swift`):
//! accounts, users, messages and spaces, in the JSON mail.db holds (dates as seconds since 2001,
//! absent optionals left out), so either engine reads what the other wrote; and what the list and
//! the commands show of them: rows with their date labels and previews, and message text without
//! its markup ([`crate::chat_markup`]).
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::answers;
use crate::chat_markup;
pub use crate::display::DateLabels;
use crate::model::{DISTANT_PAST, Date, iso8601};

/// A Google account with Chat access; `name` and `email` are both its address (the account id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatAccount {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// One space of one chat account.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatSpaceKey {
    pub account: String,
    pub space: String,
}

impl ChatSpaceKey {
    pub fn new(account: &str, space: &str) -> Self {
        Self { account: account.into(), space: space.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUser {
    /// `users/<id>`, stable across spaces.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

/// Trimmed of spaces and tabs, as Swift's `trimmingCharacters(in: .whitespaces)`.
fn blank(text: &str) -> bool {
    text.chars().all(|character| character.is_whitespace() && character != '\n' && character != '\r')
}

impl ChatUser {
    /// Display name, else address, else a generic label (Google withholds both for some people).
    pub fn label(&self) -> String {
        if let Some(name) = self.display_name.as_deref().filter(|name| !blank(name)) {
            return name.into();
        }
        if let Some(email) = self.email.as_deref().filter(|email| !email.is_empty()) {
            return email.into();
        }
        if self.bot { "App" } else { "Unknown" }.into()
    }

    /// First name, for previews ("Brent: …").
    pub fn short_label(&self) -> String {
        match self.display_name.as_deref().filter(|name| !name.is_empty()) {
            Some(name) => name.split(' ').find(|word| !word.is_empty()).unwrap_or(name).into(),
            None => self.label(),
        }
    }

    /// What avatars and avatar colors key on: the lowercased address, else the resource name.
    pub fn avatar_key(&self) -> String {
        self.email.as_ref().map_or_else(|| self.name.clone(), |email| email.to_lowercase())
    }

    /// As commands answer a user.
    pub fn answer(&self) -> answers::ChatUser {
        answers::ChatUser {
            id: self.name.clone(),
            name: self.label(),
            email: self.email.clone(),
            avatar_key: self.avatar_key(),
            avatar_url: self.avatar_url.clone(),
            bot: self.bot,
            avatar: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The uploaded file's `attachmentDataRef`, which `chat.attachment` downloads; nil for Drive
    /// files and for messages cached before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// `spaces/x/messages/y`.
    pub name: String,
    pub sender: ChatUser,
    pub created: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<Date>,
    /// The text as Google Chat sent it, in Markdown ([`crate::chat_markup`]).
    #[serde(default)]
    pub text: String,
    /// `spaces/x/threads/z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// A reply inside a thread, not the thread's first message.
    #[serde(default)]
    pub thread_reply: bool,
    #[serde(default)]
    pub mine: bool,
    /// A bot card; `text` holds its fallback text or "".
    #[serde(default)]
    pub card: bool,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
}

impl ChatMessage {
    /// As `chat.messages` answers one.
    pub fn answer(&self) -> answers::ChatMessage {
        answers::ChatMessage {
            id: self.name.clone(),
            sender: self.sender.answer(),
            created: Some(iso8601(self.created)),
            edited: self.edited.map(iso8601),
            text: chat_markup::plain(&self.text),
            markup: self.text.clone(),
            thread: self.thread.clone(),
            thread_reply: self.thread_reply,
            mine: self.mine,
            card: self.card,
            attachments: self
                .attachments
                .iter()
                .map(|attachment| answers::ChatAttachment {
                    name: attachment.name.clone(),
                    url: attachment.url.clone(),
                    content_type: attachment.content_type.clone().filter(|kind| !kind.is_empty()),
                    resource: attachment.resource.clone(),
                })
                .collect(),
            body: Vec::new(),
            time: String::new(),
            divider: String::new(),
            continued: false,
        }
    }
}

/// What a space is: a direct message, a group chat, a named space, or a direct message with an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChatSpaceKind {
    Dm,
    Group,
    Space,
    Bot,
}

impl ChatSpaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dm => "dm",
            Self::Group => "group",
            Self::Space => "space",
            Self::Bot => "bot",
        }
    }
}

impl Serialize for ChatSpaceKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// An unknown kind reads as a space, as in the Swift engine.
impl<'de> Deserialize<'de> for ChatSpaceKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "dm" => Self::Dm,
            "group" => Self::Group,
            "bot" => Self::Bot,
            _ => Self::Space,
        })
    }
}

fn space_kind() -> ChatSpaceKind {
    ChatSpaceKind::Space
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSpace {
    /// `spaces/x`.
    pub name: String,
    #[serde(default = "space_kind")]
    pub kind: ChatSpaceKind,
    /// Never empty: the space's name, the DM peer, or the members' names.
    #[serde(default)]
    pub title: String,
    /// Other people (not the user) in a DM or group; empty for named spaces.
    #[serde(default)]
    pub members: Vec<ChatUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<Date>,
    /// Messages after `last_read` not sent by the user, capped at the summary's message count.
    #[serde(default)]
    pub unread: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<ChatMessage>,
}

impl ChatSpace {
    /// Newest activity: `last_active`, else the latest message's time.
    pub fn activity(&self) -> Option<Date> {
        self.last_active.or_else(|| self.latest.as_ref().map(|message| message.created))
    }
}

/// Newest first, undated last; stable, so equal dates keep their order.
pub fn sort_newest_first<T>(items: &mut [T], date: impl Fn(&T) -> Option<Date>) {
    items.sort_by(|a, b| {
        let (a, b) = (date(a).unwrap_or(DISTANT_PAST), date(b).unwrap_or(DISTANT_PAST));
        b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// One account's spaces; also its row in mail.db.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatSpaceList {
    /// The user's `users/<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    pub more: bool,
    pub spaces: Vec<ChatSpace>,
}

/// A page of messages, ascending; also a recent space's row in mail.db.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePage {
    pub messages: Vec<ChatMessage>,
    pub more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<Date>,
}

// MARK: - Rows

/// What one space's list row shows (`chat.spaces`), with when it was last active.
pub fn row(space: &ChatSpace, account: &str, labels: &DateLabels) -> (Option<Date>, answers::ChatSpace) {
    let title = if space.title.is_empty() {
        space.members.iter().map(ChatUser::label).collect::<Vec<_>>().join(", ")
    } else {
        space.title.clone()
    };
    let activity = space.activity();
    let (date, section) = labels.labels(activity);
    let latest = space.latest.as_ref();
    let incoming = latest.filter(|message| !message.mine).map(|message| &message.sender);
    let person = match space.kind {
        ChatSpaceKind::Dm | ChatSpaceKind::Bot => space.members.first().or(incoming),
        ChatSpaceKind::Group => incoming.or(space.members.first()),
        ChatSpaceKind::Space => incoming,
    };
    let (avatar_key, avatar_url, avatar_color_key, avatar_name) = match person {
        Some(person) => (person.avatar_key(), person.avatar_url.clone(), person.avatar_key(), person.label()),
        None => (String::new(), None, space.name.clone(), title.clone()),
    };
    let row = answers::ChatSpace {
        account: account.into(),
        space: space.name.clone(),
        kind: space.kind,
        preview: latest.map(preview).unwrap_or_default(),
        title,
        date,
        section,
        activity: activity.map(iso8601),
        unread: space.unread > 0,
        attachment: latest.is_some_and(|message| !message.attachments.is_empty()),
        avatar_key,
        avatar_name,
        avatar_color_key,
        avatar_url,
        avatar: Default::default(),
    };
    (activity, row)
}

/// A message as a row's preview.
pub fn preview(message: &ChatMessage) -> String {
    let mut body = chat_markup::plain(&message.text);
    if body.is_empty() {
        body = match message.attachments.first() {
            Some(attachment) => format!("Attachment: {}", attachment.name),
            None if message.card => "Card".into(),
            None => String::new(),
        };
    }
    let who = if message.mine { "You".to_string() } else { message.sender.short_label() };
    if body.is_empty() { who } else { format!("{who}: {body}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(display: Option<&str>, email: Option<&str>) -> ChatUser {
        ChatUser {
            name: "users/1".into(),
            display_name: display.map(String::from),
            email: email.map(String::from),
            avatar_url: None,
            bot: false,
        }
    }

    #[test]
    fn labels_users_and_previews() {
        assert_eq!(user(Some("Brent Smith"), None).short_label(), "Brent");
        assert_eq!(user(Some("  "), Some("a@x.com")).label(), "a@x.com");
        assert_eq!(user(None, Some("A@X.com")).avatar_key(), "a@x.com");
        assert_eq!(user(None, None).label(), "Unknown");
        let message = ChatMessage {
            name: "spaces/a/messages/1".into(),
            sender: user(Some("Brent Smith"), None),
            created: 0.0,
            edited: None,
            text: String::new(),
            thread: None,
            thread_reply: false,
            mine: false,
            card: false,
            attachments: vec![ChatAttachment { name: "a.png".into(), content_type: None, url: None, resource: None }],
        };
        assert_eq!(preview(&message), "Brent: Attachment: a.png");
    }

    #[test]
    fn reads_what_the_swift_engine_writes() {
        // ChatSpaceList as Swift's JSONEncoder writes it, with an unknown kind and a missing title.
        let swift = r#"{"me":"users\/9","more":true,"spaces":[{"name":"spaces\/A","kind":"future","members":[],"unread":2,
            "lastActive":815000000.5,"latest":{"name":"spaces\/A\/messages\/1","sender":{"name":"users\/2","bot":false},
            "created":815000000.5,"text":"hi","threadReply":false,"mine":false,"card":false,"attachments":[]}}]}"#;
        let list: ChatSpaceList = serde_json::from_str(swift).unwrap();
        assert_eq!(list.spaces[0].kind, ChatSpaceKind::Space);
        assert_eq!(list.spaces[0].title, "");
        assert_eq!(list.spaces[0].activity(), Some(815_000_000.5));
        let written = String::from_utf8(crate::json::encode(&list, false, true)).unwrap();
        assert!(written.contains(r#""kind":"space""#) && !written.contains("lastRead"), "{written}");
    }
}
