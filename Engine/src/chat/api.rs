//! # Chat API
//!
//! The Google Chat REST resources the engine reads and writes, and one authorized call that
//! renews the token once when Google rejects it.
//!
//! Reference: <https://developers.google.com/workspace/chat/api/reference/rest>.

use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use url::Url;

use super::{
    account,
    http::{self, ApiError, Effect},
};

const BASE: &str = "https://chat.googleapis.com/v1/";

/// `path` under the API root, with `query` encoded.
pub fn url(path: &str, query: &[(&str, &str)]) -> Result<Url> {
    let mut url = Url::parse(BASE)?.join(path)?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}

/// One call as the account's user. A 401 means the cached token went stale (revoked, or
/// refreshed elsewhere): it is fetched again and the call repeated, which is safe because the
/// server refused the first one.
pub fn call<T: DeserializeOwned>(account: &str, method: &str, url: &Url, body: Option<&impl Serialize>, effect: Effect) -> Result<T> {
    let body = body.map(serde_json::to_vec).transpose()?;
    let token = account::token(account, false)?;
    let bytes = match http::send(method, url, &token, body.clone(), effect) {
        Err(err) if err.downcast_ref::<ApiError>().is_some_and(|e| e.status == 401) => {
            let token = account::token(account, true)?;
            http::send(method, url, &token, body, effect)?
        }
        result => result?,
    };
    http::parse(&bytes)
}

pub fn get<T: DeserializeOwned>(account: &str, url: &Url) -> Result<T> {
    call(account, "GET", url, None::<&()>, Effect::Idempotent)
}

// MARK: - Resources

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Space {
    pub name: String,
    #[serde(default)]
    pub space_type: String,
    #[serde(default)]
    pub display_name: String,
    pub last_active_time: Option<String>,
    #[serde(default)]
    pub single_user_bot_dm: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpacesPage {
    #[serde(default)]
    pub spaces: Vec<Space>,
    pub next_page_token: Option<String>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub name: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default)]
    pub name: String,
    pub sender: Option<User>,
    pub create_time: Option<String>,
    pub last_update_time: Option<String>,
    pub text: Option<String>,
    pub formatted_text: Option<String>,
    pub fallback_text: Option<String>,
    pub thread: Option<Thread>,
    #[serde(default)]
    pub thread_reply: bool,
    #[serde(default)]
    pub attachment: Vec<Attachment>,
    pub cards_v2: Option<Value>,
    pub cards: Option<Value>,
}

#[derive(Deserialize, Serialize)]
pub struct Thread {
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub content_name: Option<String>,
    pub content_type: Option<String>,
    pub download_uri: Option<String>,
    pub drive_data_ref: Option<DriveRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveRef {
    pub drive_file_id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MessagesPage {
    #[serde(default)]
    pub messages: Vec<Message>,
    pub next_page_token: Option<String>,
}

#[derive(Deserialize)]
pub struct Membership {
    pub member: Option<User>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MembersPage {
    #[serde(default)]
    pub memberships: Vec<Membership>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadState {
    /// `users/{user}/spaces/{space}/spaceReadState`: the only place the API names the caller.
    pub name: String,
    pub last_read_time: Option<String>,
}

impl ReadState {
    /// The caller's own `users/{id}`.
    pub fn user(&self) -> Option<String> {
        let mut parts = self.name.split('/');
        match (parts.next(), parts.next()) {
            (Some("users"), Some(id)) if !id.is_empty() && id != "me" => Some(format!("users/{id}")),
            _ => None,
        }
    }
}

// MARK: - Calls

/// Every space the user is in. Pages are as large as the API allows, so this is one or two
/// calls.
pub fn spaces(account: &str) -> Result<Vec<Space>> {
    let mut spaces = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut query = vec![("pageSize", "1000")];
        if let Some(token) = token.as_deref() {
            query.push(("pageToken", token));
        }
        let page: SpacesPage = get(account, &url("spaces", &query)?)?;
        spaces.extend(page.spaces);
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(next) => token = Some(next),
            None => return Ok(spaces),
        }
    }
}

/// The newest `limit` messages created before `before`, newest first.
pub fn messages(account: &str, space: &str, limit: usize, before: Option<&str>) -> Result<MessagesPage> {
    let size = limit.clamp(1, 1000).to_string();
    let filter = before.map(|time| format!("createTime < \"{time}\""));
    let mut query = vec![("pageSize", size.as_str()), ("orderBy", "createTime desc")];
    if let Some(filter) = filter.as_deref() {
        query.push(("filter", filter));
    }
    get(account, &url(&format!("{space}/messages"), &query)?)
}

/// Members, invited ones included: a DM with someone outside the domain who hasn't accepted yet
/// has them only as an invitation.
pub fn members(account: &str, space: &str) -> Result<Vec<User>> {
    let query = [("pageSize", "100"), ("showInvited", "true")];
    let page: MembersPage = get(account, &url(&format!("{space}/members"), &query)?)?;
    Ok(page.memberships.into_iter().filter_map(|m| m.member).collect())
}

pub fn read_state(account: &str, space: &str) -> Result<ReadState> {
    get(account, &url(&format!("users/me/{space}/spaceReadState"), &[])?)
}

pub fn mark_read(account: &str, space: &str, time: &str) -> Result<ReadState> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Body<'a> {
        last_read_time: &'a str,
    }
    let url = url(&format!("users/me/{space}/spaceReadState"), &[("updateMask", "lastReadTime")])?;
    call(account, "PATCH", &url, Some(&Body { last_read_time: time }), Effect::Idempotent)
}

pub fn create_message(account: &str, space: &str, text: &str, thread: Option<&str>) -> Result<Message> {
    #[derive(Serialize)]
    struct Body<'a> {
        text: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread: Option<Thread>,
    }
    let query: &[(&str, &str)] = match thread {
        Some(_) => &[("messageReplyOption", "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD")],
        None => &[],
    };
    let body = Body { text, thread: thread.map(|name| Thread { name: name.to_owned() }) };
    call(account, "POST", &url(&format!("{space}/messages"), query)?, Some(&body), Effect::Creates)
}
