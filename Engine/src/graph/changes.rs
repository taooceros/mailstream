//! Changing and sending Outlook mail, as the Swift engine's `GraphBackend` does: flags, moves and
//! deletions go through `$batch` twenty at a time, and sending is Graph's MIME `sendMail`.
//! Categories (AI tags synced, `tagsync`) are read afresh and written back per message, since a
//! PATCH replaces the whole list.
use std::collections::HashMap;

use base64::Engine as _;
use serde_json::{Value, json};

use super::{Graph, IMMUTABLE_IDS};
use crate::api::{self, ApiError, ErrorBody, Priority, Request};
use crate::backend::{Flag, SentCopy};
use crate::mime::{self, OutgoingMessage};

/// One message's sub-request: its id, method, path after the message and body.
type Change<'a> = (String, &'a str, String, Option<Value>);

impl Graph {
    pub async fn set_flag(&self, flag: Flag, on: bool, ids: &[String]) -> Result<(), ApiError> {
        let change = match flag {
            Flag::Seen => json!({ "isRead": on }),
            Flag::Flagged => json!({ "flag": { "flagStatus": if on { "flagged" } else { "notFlagged" } } }),
        };
        self.batch("PATCH", ids, "", Some(change)).await
    }

    pub async fn move_to(&self, ids: &[String], to: &str) -> Result<(), ApiError> {
        self.batch("POST", ids, "/move", Some(json!({ "destinationId": to }))).await
    }

    /// Deletes for good; only called for messages already in the trash.
    pub async fn delete(&self, ids: &[String]) -> Result<(), ApiError> {
        self.batch("DELETE", ids, "", None).await
    }

    /// Adds the categories `add` and takes off `remove` (any case) on every message, keeping the
    /// rest of each message's categories as Outlook has them now. A category the message already
    /// carries in another case stays as it is.
    pub async fn set_tags(&self, ids: &[String], add: &[String], remove: &[String]) -> Result<(), ApiError> {
        let fetches = ids.iter().map(|id| (id.clone(), "GET", "?$select=categories".to_string(), None)).collect();
        let current = self.send_batch(fetches).await?;
        let updates: Vec<Change> = ids
            .iter()
            .filter_map(|id| {
                let before: Vec<String> = current
                    .get(id)
                    .and_then(|answer| serde_json::from_value(answer["categories"].clone()).ok())
                    .unwrap_or_default();
                let after = crate::tagsync::retagged(&before, add, remove);
                (after != before).then(|| (id.clone(), "PATCH", String::new(), Some(json!({ "categories": after }))))
            })
            .collect();
        self.send_batch(updates).await?;
        Ok(())
    }

    /// Sends and files the copy in Sent Items. Never repeated. Graph answers nothing of the copy.
    pub async fn send(&self, message: &OutgoingMessage) -> Result<SentCopy, ApiError> {
        let raw = mime::render(message).map_err(|message| ApiError::new(0, message))?;
        let body = base64::engine::general_purpose::STANDARD.encode(raw).into_bytes();
        let request = Request::post(self.url("me/sendMail", &[]), "text/plain", body).header("Prefer", IMMUTABLE_IDS);
        self.api.send(request, Priority::Interactive, Some(false), 0).await?;
        Ok(SentCopy::default())
    }

    /// Sends `method` to `/me/messages/{id}<path>` for every id through `$batch`, twenty at a
    /// time (`Api::batch`: throttled or unanswered sub-requests are sent again); any other
    /// failure fails the whole change.
    async fn batch(&self, method: &str, ids: &[String], path: &str, body: Option<Value>) -> Result<(), ApiError> {
        self.send_batch(ids.iter().map(|id| (id.clone(), method, path.to_string(), body.clone())).collect()).await?;
        Ok(())
    }

    /// `batch` with a method, path and body of its own per message (`(id, method, path, body)`);
    /// answers each message's successful sub-request's body, by message id.
    async fn send_batch<'a>(&self, items: Vec<Change<'a>>) -> Result<HashMap<String, Value>, ApiError> {
        let send = |pending: Vec<(usize, Change<'a>)>| async move {
            let requests: Vec<Value> = pending
                .into_iter()
                .map(|(number, (id, method, path, body))| {
                    let mut headers = json!({ "Prefer": IMMUTABLE_IDS });
                    if body.is_some() {
                        headers["Content-Type"] = json!("application/json");
                    }
                    let mut request = json!({
                        "id": number.to_string(), "method": method,
                        "url": format!("/me/messages/{}{path}", api::percent_encoded(&id)), "headers": headers,
                    });
                    if let Some(body) = body {
                        request["body"] = body;
                    }
                    request
                })
                .collect();
            let payload = serde_json::to_vec(&json!({ "requests": requests })).expect("batch encodes");
            let request = Request::post(self.url("$batch", &[]), "application/json", payload).header("Prefer", IMMUTABLE_IDS);
            let answer: Value = self.api.decode(request, Priority::Interactive, 0).await?;
            Ok(api::json_parts(&answer))
        };
        let mut found = HashMap::new();
        self.api
            .batch(items, 20, "Microsoft Graph", false, send, |(id, ..), part| {
                if !(200..300).contains(&part.status) {
                    let message = ErrorBody::parse(&part.body)
                        .and_then(|body| body.message())
                        .unwrap_or_else(|| format!("Microsoft Graph answered {}", part.status));
                    return Err(ApiError::new(part.status, message));
                }
                found.insert(id, serde_json::from_slice(&part.body).unwrap_or(Value::Null));
                Ok(())
            })
            .await?;
        Ok(found)
    }
}
