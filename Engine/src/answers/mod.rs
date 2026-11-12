//! What commands answer, as values (MS-238): the engine builds each answer as one of these and
//! writes it as the command's JSON (the control socket's, the CLI's and agents' format), and the
//! FFI crate (`FFI/`) reads that JSON back into the same type and hands it to front ends as it is
//! (each is a UniFFI record or enum; Dart gets them from `mailstream_engine.dart`, GPUI through
//! the `mailstream` crate). A field added here reaches every front end.
//!
//! Fields marked `#[serde(skip)]` are display strings (`display.rs`, MS-235) the JSON leaves out:
//! the FFI fills them in once per answer with [`dress`](Listing::dress), so a front end never
//! asks per row while it scrolls.
//!
//! Dates are ISO 8601 text, as the JSON has them. Addresses in mail answers are
//! [`Address`](crate::model::Address) values written as `Name <address>` text ([`address_text`]).
mod calendar;
mod chat;
mod mail;

pub use calendar::*;
pub use chat::*;
pub use mail::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::model::{Address, MailboxRole};
pub use crate::oauth::Provider;
pub use crate::tagging::Category;

/// `value` as a command's JSON.
pub fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("answers encode")
}

/// An OAuth client the engine knows: its id, and whether a secret is saved with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInfo {
    pub id: String,
    pub has_secret: bool,
}

/// A signed-in account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Account {
    /// The address, which identifies the account everywhere.
    pub id: String,
    pub label: String,
    pub provider: Provider,
    /// The person's name from the provider.
    pub name: Option<String>,
    /// What the sign-in grants: `mail`, `contacts`, `chat`, `calendar`.
    pub services: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub settings_file: String,
    pub settings_issues: Vec<String>,
    pub accounts: Vec<AccountStatus>,
    pub sync: SyncStatus,
    pub ai_tagging: TaggingStatus,
}

/// The AI tagger: whether it is on, and what it is doing or why it stopped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct TaggingStatus {
    pub enabled: bool,
    /// A line for people (progress or the last failure); nil while idle.
    pub status: Option<String>,
    /// `status` is the last run's failure (omp or Apple Intelligence unavailable, a failed run);
    /// the `problems` event announces it showing up and going away.
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub id: String,
    pub label: String,
    pub provider: Provider,
    pub mailboxes_loaded: bool,
    /// Why the account's mail could not load just now (no answer, the provider failing, the
    /// sign-in refused), until it loads again; the `problems` event follows it (MS-278).
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub loading: bool,
    /// ISO 8601.
    pub last_refresh: Option<String>,
    pub last_error: Option<String>,
    pub push_connected: bool,
}

/// Someone from the accounts' Google and Outlook directories or recent correspondents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Person {
    pub name: Option<String>,
    pub email: String,
    #[serde(skip)]
    #[uniffi(default)]
    pub avatar: crate::display::Avatar,
}

// MARK: - Settings

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
    /// The setting's type in the JSON.
    fn kind(&self) -> &'static str {
        match self {
            Self::Flag { .. } => "boolean",
            Self::Number { .. } => "integer",
            Self::Text { .. } => "string",
            Self::Addresses { .. } => "array of email addresses",
        }
    }

    /// `value` read as a setting of `kind` holds it.
    pub fn of(kind: &str, value: &Value) -> Option<Self> {
        Some(match kind {
            "boolean" => Self::Flag { enabled: value.as_bool()? },
            "integer" => Self::Number { value: value.as_i64()? },
            "string" => Self::Text { text: value.as_str()?.into() },
            _ => Self::Addresses { addresses: serde_json::from_value(value.clone()).ok()? },
        })
    }

    pub fn json(&self) -> Value {
        match self {
            Self::Flag { enabled } => Value::from(*enabled),
            Self::Number { value } => Value::from(*value),
            Self::Text { text } => Value::from(text.as_str()),
            Self::Addresses { addresses } => Value::from(addresses.clone()),
        }
    }

    /// As `config.set` takes it: true/false, a number, text, or a JSON array.
    pub fn typed(&self) -> String {
        match self {
            Self::Flag { enabled } => enabled.to_string(),
            Self::Number { value } => value.to_string(),
            Self::Text { text } => text.clone(),
            Self::Addresses { addresses } => serde_json::to_string(addresses).expect("strings encode"),
        }
    }
}

/// One setting of settings.json, the file Mailstream.app and every front end share. In the JSON
/// the values are bare, with the setting's `type` beside them.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
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

#[derive(Serialize, Deserialize)]
struct SettingJson {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    value: Value,
    default: Value,
    changed: bool,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    choices: Vec<String>,
}

impl Serialize for SettingEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SettingJson {
            key: self.key.clone(),
            kind: self.default_value.kind().into(),
            value: self.value.json(),
            default: self.default_value.json(),
            changed: self.changed,
            summary: self.summary.clone(),
            minimum: self.minimum,
            maximum: self.maximum,
            choices: self.choices.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SettingEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = SettingJson::deserialize(deserializer)?;
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Settings {
    pub file: String,
    pub settings: Vec<SettingEntry>,
    pub issues: Vec<String>,
}

// MARK: - Batches

/// A conversation an action failed for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct BatchFailure {
    pub id: String,
    /// The engine's error code; none for a tag correction that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

impl BatchFailure {
    /// `id` failed with the engine's `error`.
    pub fn of(id: &str, error: &crate::EngineError) -> Self {
        Self { id: id.into(), code: Some(error.code.clone()), message: error.message.clone() }
    }
}

/// An action over several conversations: the ids it was done for and those it failed for, and for
/// an action on mail the step `mail.undo` takes it back with, when it changed anything.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct BatchResult {
    pub done: Vec<String>,
    pub failed: Vec<BatchFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<u64>,
}

/// `Name <address>` text for [`Address`] fields: `serde(with = "address_text")`, and its
/// [`optional`](address_text::optional) and [`list`](address_text::list) forms.
pub mod address_text {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::model::Address;

    /// `raw` as an address; text that is none keeps it as the address.
    pub fn read(raw: &str) -> Address {
        Address::parse(raw).unwrap_or_else(|| Address::new(None, raw.trim()))
    }

    pub fn serialize<S: Serializer>(address: &Address, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&address.text())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Address, D::Error> {
        Ok(read(&String::deserialize(deserializer)?))
    }

    pub mod optional {
        use super::*;

        pub fn serialize<S: Serializer>(address: &Option<Address>, serializer: S) -> Result<S::Ok, S::Error> {
            match address {
                Some(address) => serializer.serialize_some(&address.text()),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Address>, D::Error> {
            Ok(Option::<String>::deserialize(deserializer)?.as_deref().map(read))
        }
    }

    pub mod list {
        use serde::ser::SerializeSeq;

        use super::*;

        pub fn serialize<S: Serializer>(addresses: &[Address], serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(addresses.len()))?;
            for address in addresses {
                seq.serialize_element(&address.text())?;
            }
            seq.end()
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Address>, D::Error> {
            Ok(Vec::<String>::deserialize(deserializer)?.iter().map(|raw| read(raw)).collect())
        }
    }
}
