//! Sending mail (`mail.send`, `mail.reply`).
use serde::Deserialize;
use serde_json::json;

use crate::{MailError, Mailstream, call};

/// A new message. Addresses are `Name <address>` or bare addresses.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Draft {
    /// The sending account's id.
    pub from: String,
    /// At least one address.
    pub to: Vec<String>,
    #[uniffi(default = [])]
    pub cc: Vec<String>,
    #[uniffi(default = [])]
    pub bcc: Vec<String>,
    pub subject: String,
    /// Plain text.
    pub body: String,
}

/// What was sent.
#[derive(Clone, Debug, Deserialize, uniffi::Record)]
pub struct Sent {
    pub sent: bool,
    /// The sending account's id.
    pub from: String,
    /// `Name <address>` of each recipient (for replies, `to` and `cc`).
    pub to: Vec<String>,
    pub subject: String,
}

#[uniffi::export]
impl Mailstream {
    /// Sends `draft`; without `confirm` the engine only answers what it would send (`confirm`).
    pub async fn send(&self, draft: Draft, confirm: bool) -> Result<Sent, MailError> {
        call(
            "mail.send",
            json!({
                "from": draft.from, "to": draft.to, "cc": draft.cc, "bcc": draft.bcc,
                "subject": draft.subject, "body": draft.body, "yes": confirm,
            }),
        )
        .await
    }

    /// Replies to the latest message of conversation `id`, to everyone on it with `all`; needs
    /// `confirm` like [`Mailstream::send`].
    pub async fn reply(&self, id: String, body: String, all: bool, confirm: bool) -> Result<Sent, MailError> {
        call("mail.reply", json!({ "id": id, "body": body, "all": all, "yes": confirm })).await
    }
}
