//! Changing and sending Outlook mail, as the Swift engine's `GraphBackend` does: flags, moves and
//! deletions go through `$batch` twenty at a time, and sending is Graph's MIME `sendMail`.
use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use serde_json::{Value, json};

use super::{Graph, IMMUTABLE_IDS};
use crate::api::{self, ApiError, ErrorBody, Priority, Request};
use crate::backend::Flag;
use crate::mime::{self, OutgoingMessage};

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

    /// Sends and files the copy in Sent Items. Never repeated.
    pub async fn send(&self, message: &OutgoingMessage) -> Result<(), ApiError> {
        let raw = mime::render(message).map_err(|message| ApiError::new(0, message))?;
        let body = base64::engine::general_purpose::STANDARD.encode(raw).into_bytes();
        let request = Request::post(self.url("me/sendMail", &[]), "text/plain", body).header("Prefer", IMMUTABLE_IDS);
        self.api.send(request, Priority::Interactive, Some(false), 0).await?;
        Ok(())
    }

    /// Sends `method` to `/me/messages/{id}<path>` for every id through `$batch`, twenty at a
    /// time. Sub-requests Graph throttled (429) or left unanswered are sent again, after the
    /// longest Retry-After; any other failure fails the whole change.
    async fn batch(&self, method: &str, ids: &[String], path: &str, body: Option<Value>) -> Result<(), ApiError> {
        let mut headers = json!({ "Prefer": IMMUTABLE_IDS });
        if body.is_some() {
            headers["Content-Type"] = json!("application/json");
        }
        let requests: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut request = json!({
                    "id": (index + 1).to_string(), "method": method,
                    "url": format!("/me/messages/{}{path}", api::percent_encoded(id)), "headers": headers,
                });
                if let Some(body) = &body {
                    request["body"] = body.clone();
                }
                request
            })
            .collect();
        self.api
            .batched(requests, 20, "Microsoft Graph", |pending| async move {
                let payload = serde_json::to_vec(&json!({ "requests": &pending })).expect("batch encodes");
                let request = Request::post(self.url("$batch", &[]), "application/json", payload).header("Prefer", IMMUTABLE_IDS);
                let answer: Value = self.api.decode(request, Priority::Interactive, 0).await?;
                let mut retry: HashSet<String> =
                    pending.iter().filter_map(|request| request["id"].as_str().map(String::from)).collect();
                let mut retry_after: Option<f64> = None;
                let mut items: Vec<&Value> = answer["responses"].as_array().map(|items| items.iter().collect()).unwrap_or_default();
                items.sort_by_key(|item| item["id"].as_str().and_then(|id| id.parse::<i64>().ok()).unwrap_or(0));
                for item in items {
                    let Some(id) = item["id"].as_str().filter(|id| retry.contains(*id)) else { continue };
                    let status = item["status"].as_u64().unwrap_or(0) as u16;
                    match status {
                        200..=299 => {
                            retry.remove(id);
                        }
                        429 => {
                            let headers: HashMap<String, String> =
                                serde_json::from_value(item["headers"].clone()).unwrap_or_default();
                            let seconds = headers
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case("Retry-After"))
                                .and_then(|(_, value)| value.trim().parse::<f64>().ok());
                            if let Some(seconds) = seconds {
                                retry_after = Some(retry_after.unwrap_or(0.0).max(seconds));
                            }
                        }
                        _ => {
                            let message = serde_json::from_value::<ErrorBody>(item["body"].clone())
                                .ok()
                                .and_then(|body| body.message())
                                .unwrap_or_else(|| format!("Microsoft Graph answered {status}"));
                            return Err(ApiError::new(status, message));
                        }
                    }
                }
                let again: Vec<Value> = pending
                    .into_iter()
                    .filter(|request| request["id"].as_str().is_some_and(|id| retry.contains(id)))
                    .collect();
                Ok((again, retry_after))
            })
            .await
    }
}
