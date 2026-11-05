//! Google Chat (`chat.*`): accounts, spaces, message pages, sending, and starting chats.
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::json;

use crate::{MailError, Mailstream, call};

/// A Google account signed in to Chat.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatAccount {
    /// The account's address.
    pub id: String,
    pub label: String,
    /// Its sign-in may create direct messages and group chats.
    pub can_start: bool,
    /// How many of its spaces have unread messages.
    pub unread: u32,
}

/// The spaces list, and whether the store is still loading it.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct ChatSpaces {
    pub loading: bool,
    pub spaces: Vec<ChatSpace>,
}

/// A space, direct message, or group chat as a list row.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatSpace {
    pub account: String,
    /// `spaces/<id>`.
    pub id: String,
    /// `space`, `dm`, `group` or `bot`; empty when `chat.start` could not resolve the new space yet.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    /// The latest message's preview.
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub unread: u32,
    /// ISO 8601.
    #[serde(default)]
    pub last_active: Option<String>,
    /// ISO 8601.
    #[serde(default)]
    pub last_read: Option<String>,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub avatar: ChatAvatar,
    #[serde(default)]
    pub members: Vec<ChatUser>,
}

/// How a space's row avatar is drawn.
#[derive(Clone, Debug, Default, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatAvatar {
    /// The cache key of an avatar image, if any.
    pub key: Option<String>,
    pub name: String,
    pub initials: String,
    pub color_key: String,
}

/// A Chat member.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatUser {
    /// `users/<id>`.
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub label: String,
    #[serde(default)]
    pub bot: bool,
    pub avatar_url: Option<String>,
}

/// A page of a space's messages, oldest first.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePage {
    pub messages: Vec<ChatMessage>,
    /// Older messages exist.
    pub more: bool,
    /// ISO 8601.
    pub last_read: Option<String>,
    /// The account's own user (`users/<id>`), when known.
    pub me: Option<String>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// `spaces/<id>/messages/<id>`.
    pub id: String,
    pub sender: ChatUser,
    /// ISO 8601 with microseconds, so it round-trips as `before`.
    pub created: Option<String>,
    pub edited: Option<String>,
    pub text: String,
    pub thread: Option<String>,
    pub thread_reply: bool,
    pub mine: bool,
    /// The message is a card (an app's rich content) the text only summarizes.
    pub card: bool,
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Clone, Debug, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    pub content_type: Option<String>,
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct Sent {
    message: ChatMessage,
}

#[uniffi::export]
impl Mailstream {
    /// The accounts signed in to Google Chat.
    pub async fn chat_accounts(&self) -> Result<Vec<ChatAccount>, MailError> {
        call("chat.accounts", json!({})).await
    }

    /// Spaces newest activity first; only `account`'s when given.
    pub async fn chat_spaces(&self, account: Option<String>) -> Result<ChatSpaces, MailError> {
        call("chat.spaces", json!({ "account": account })).await
    }

    /// Up to `limit` (1-page size) messages of `space`, those before the ISO 8601 `before` when given.
    pub async fn chat_messages(
        &self,
        space: String,
        account: String,
        before: Option<String>,
        limit: u32,
    ) -> Result<ChatMessagePage, MailError> {
        call(
            "chat.messages",
            json!({ "space": space, "account": account, "before": before, "limit": limit }),
        )
        .await
    }

    /// Posts `body` to `space`, as a reply in `thread` when given; returns the sent message.
    /// Sends only with `confirm`.
    pub async fn chat_send(
        &self,
        space: String,
        account: String,
        body: String,
        thread: Option<String>,
        confirm: bool,
    ) -> Result<ChatMessage, MailError> {
        let sent: Sent = call(
            "chat.send",
            json!({ "space": space, "account": account, "body": body, "thread": thread, "yes": confirm }),
        )
        .await?;
        Ok(sent.message)
    }

    /// Marks `space` read up to now.
    pub async fn chat_read(&self, space: String, account: String) -> Result<(), MailError> {
        call::<IgnoredAny>("chat.read", json!({ "space": space, "account": account })).await?;
        Ok(())
    }

    /// Finds or creates the direct message (one address) or group chat with `emails`.
    pub async fn chat_start(&self, account: String, emails: Vec<String>) -> Result<ChatSpace, MailError> {
        call("chat.start", json!({ "account": account, "emails": emails })).await
    }

    /// Asks the store to poll Chat now.
    pub async fn chat_refresh(&self) -> Result<(), MailError> {
        call::<IgnoredAny>("chat.refresh", json!({})).await?;
        Ok(())
    }
}
