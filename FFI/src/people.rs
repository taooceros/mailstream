//! The people compose completes addresses to (`contacts.search`).
use serde_json::json;

use crate::{MailError, Mailstream, Person, call};

#[uniffi::export]
impl Mailstream {
    /// People whose name or address starts with `query`, best first; at most `limit` (1-12).
    pub async fn search_contacts(&self, query: String, limit: u32) -> Result<Vec<Person>, MailError> {
        let mut people: Vec<Person> = call("contacts.search", json!({ "query": query, "limit": limit })).await?;
        for person in &mut people {
            person.dress();
        }
        Ok(people)
    }
}
