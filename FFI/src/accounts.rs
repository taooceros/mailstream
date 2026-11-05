//! Signing accounts in and out, renaming them (`accounts.*`).
use serde_json::json;

use crate::{Account, MailError, Mailstream, call};

#[uniffi::export]
impl Mailstream {
    /// Sets the label of `account`; an empty label goes back to the address. `None` when the
    /// account went away meanwhile.
    pub async fn rename_account(&self, account: String, label: String) -> Result<Option<Account>, MailError> {
        call("accounts.rename", json!({ "account": account, "label": label })).await
    }

    /// Signs in to `provider` (`google` or `microsoft`) in the browser, waiting for the user (up
    /// to five minutes). Without a client the engine uses the one it knows for the provider;
    /// Google needs the secret too.
    pub async fn add_account(
        &self,
        provider: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Result<Account, MailError> {
        call("accounts.add", json!({ "provider": provider, "client-id": client_id, "client-secret": client_secret }))
            .await
    }

    /// Signs out of `account` and deletes its saved sign-in (mail on the server stays); needs
    /// `confirm`.
    pub async fn remove_account(&self, account: String, confirm: bool) -> Result<(), MailError> {
        call::<serde_json::Value>("accounts.remove", json!({ "account": account, "yes": confirm })).await.map(drop)
    }
}
