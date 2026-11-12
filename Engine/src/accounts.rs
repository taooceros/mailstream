//! The signed-in accounts, in the order they were added, kept in accounts.json as the Swift
//! engine's `AccountStore` keeps them (`Sources/MailstreamKit/Backend/Accounts.swift`); their
//! refresh tokens live in the Keychain ([`crate::tokens`]).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::oauth::{GOOGLE_CALENDARS, GOOGLE_EVENTS, Grant, MICROSOFT_CALENDARS, MICROSOFT_CALENDARS_READ, Provider, has_scope};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRecord {
    /// Lowercased address: the key of everything.
    pub id: String,
    pub provider: Provider,
    /// Shown in the sidebar; the person may change it (defaults to the address).
    pub label: String,
    /// The person's name from the provider.
    #[serde(rename = "displayName", default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Scopes the provider granted.
    pub scopes: Vec<String>,
}

impl AccountRecord {
    /// The account as `accounts.*` commands answer it.
    pub fn json(&self) -> Value {
        let scopes = self.scopes.join(" ").to_lowercase();
        let mut services: Vec<&str> =
            ["mail", "contacts", "chat"].into_iter().filter(|service| scopes.contains(service)).collect();
        if self.has_calendars() {
            services.push("calendar");
        }
        json!({
            "id": self.id, "label": self.label, "provider": self.provider.as_str(), "name": self.display_name,
            "services": services,
        })
    }

    /// Whether its sign-in lets it read its calendars: those signed in before Mailstream asked
    /// for them must sign in again.
    pub fn has_calendars(&self) -> bool {
        match self.provider {
            Provider::Google => has_scope(&self.scopes, GOOGLE_CALENDARS),
            Provider::Microsoft => {
                has_scope(&self.scopes, MICROSOFT_CALENDARS) || has_scope(&self.scopes, MICROSOFT_CALENDARS_READ)
            }
        }
    }

    /// Whether its sign-in lets it change events: answer invitations, add and edit events.
    pub fn can_edit_calendars(&self) -> bool {
        match self.provider {
            Provider::Google => has_scope(&self.scopes, GOOGLE_EVENTS),
            Provider::Microsoft => has_scope(&self.scopes, MICROSOFT_CALENDARS),
        }
    }
}

pub struct Accounts {
    directory: PathBuf,
    file: PathBuf,
    records: Mutex<Vec<AccountRecord>>,
}

impl Accounts {
    /// The accounts in `directory`'s accounts.json; none when it is missing or does not decode.
    pub fn new(directory: &Path) -> Self {
        let file = directory.join("accounts.json");
        let records = std::fs::read(&file).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
        Self { directory: directory.to_path_buf(), file, records: Mutex::new(records) }
    }

    pub fn list(&self) -> Vec<AccountRecord> {
        self.records.lock().expect("accounts lock").clone()
    }

    pub fn record(&self, id: &str) -> Option<AccountRecord> {
        self.records.lock().expect("accounts lock").iter().find(|record| record.id == id).cloned()
    }

    /// Sets `id`'s label; an empty one shows the address again. The record after, `None` when there
    /// is no such account; `Err` when the file could not be written.
    pub fn rename(&self, id: &str, label: &str) -> Result<Option<AccountRecord>, String> {
        let mut records = self.records.lock().expect("accounts lock");
        let Some(record) = records.iter_mut().find(|record| record.id == id) else { return Ok(None) };
        let trimmed = label.trim();
        record.label = if trimmed.is_empty() { id.to_string() } else { trimmed.to_string() };
        let renamed = record.clone();
        self.persist(&records)?;
        Ok(Some(renamed))
    }

    /// Records a completed sign-in. Signing in to an account already here replaces its name and
    /// scopes and keeps its label.
    pub fn signed_in(&self, provider: Provider, grant: &Grant) -> Result<AccountRecord, String> {
        let mut records = self.records.lock().expect("accounts lock");
        let mut record = AccountRecord {
            id: grant.id.clone(),
            provider,
            label: grant.id.clone(),
            display_name: grant.name.clone(),
            scopes: grant.scopes.clone(),
        };
        match records.iter_mut().find(|existing| existing.id == record.id) {
            Some(existing) => {
                record.label = existing.label.clone();
                *existing = record.clone();
            }
            None => records.push(record.clone()),
        }
        self.persist(&records)?;
        Ok(record)
    }

    /// Deletes `id`'s record; false when there was none.
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let mut records = self.records.lock().expect("accounts lock");
        let before = records.len();
        records.retain(|record| record.id != id);
        if records.len() == before {
            return Ok(false);
        }
        self.persist(&records)?;
        Ok(true)
    }

    /// Pretty, key-sorted, slashes escaped and no final newline: what the Swift engine writes.
    fn persist(&self, records: &[AccountRecord]) -> Result<(), String> {
        let value = serde_json::to_value(records).expect("accounts encode");
        crate::files::replace(&self.directory, &self.file, &crate::json::encode(&value, true, true))
            .map_err(|error| format!("Could not save the account list: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_swift_format_and_labels() {
        let dir =
            std::env::temp_dir().join(format!("mailstream-accounts-{}-{}", std::process::id(), crate::testkit::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        let swift = "[\n  {\n    \"displayName\" : \"Me\",\n    \"id\" : \"me@x.com\",\n    \"label\" : \"Work\",\n    \"provider\" : \"google\",\n    \"scopes\" : [\n      \"https:\\/\\/mail.google.com\\/\",\n      \"https:\\/\\/www.googleapis.com\\/auth\\/chat.messages.readonly\"\n    ]\n  }\n]";
        std::fs::write(dir.join("accounts.json"), swift).unwrap();
        let accounts = Accounts::new(&dir);
        let me = accounts.record("me@x.com").unwrap();
        assert_eq!(me.json()["services"], json!(["mail", "chat"]));
        // A rename to the same label writes the same bytes back.
        accounts.rename("me@x.com", "  Work ").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("accounts.json")).unwrap(), swift);
        assert_eq!(accounts.rename("me@x.com", " ").unwrap().unwrap().label, "me@x.com");
        assert_eq!(accounts.rename("other@x.com", "x").unwrap(), None);
        accounts.rename("me@x.com", "Work").unwrap();
        // Signing in again keeps the label and takes the new name and scopes.
        let grant = Grant {
            id: "me@x.com".into(),
            name: None,
            scopes: vec!["openid".into()],
            refresh_token: "rt".into(),
            access_token: "at".into(),
            expires_in: 3600,
        };
        let record = accounts.signed_in(Provider::Google, &grant).unwrap();
        assert_eq!((record.label.as_str(), record.display_name.as_deref()), ("Work", None));
        assert_eq!(Accounts::new(&dir).list(), vec![record]);
        assert!(accounts.remove("me@x.com").unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("accounts.json")).unwrap(), "[\n\n]");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
