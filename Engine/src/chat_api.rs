//! Google Chat over its REST API, as the Swift engine's `Chat` and `ChatAPI`
//! (`Sources/MailstreamKit/Backend/Chat.swift`, `ChatAPI.swift`) read and write it.
//! Reference: <https://developers.google.com/workspace/chat/api/reference/rest>.
//!
//! The space list is polled, so it is kept cheap: each account's space summaries stay in memory
//! keyed by the space's last activity, and a poll only refetches the spaces whose activity
//! changed (plus the read state of the ones still unread, which another client may have read).
use std::collections::HashMap;
use std::sync::Mutex;

use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{self, Api, ApiError, Priority, Request};
use crate::chat_model::{ChatAttachment, ChatMessage, ChatMessagePage, ChatSpace, ChatSpaceKind, ChatSpaceList, ChatUser};
use crate::model::{DISTANT_PAST, Date, date_from_unix, iso8601, parse_iso8601, unix_seconds};

/// Google Chat's API root.
pub const ROOT: &str = "https://chat.googleapis.com/v1/";
/// Messages read per space for its summary: the latest one, and enough to count what is unread.
pub const SUMMARY_MESSAGES: usize = 25;
/// Space summaries fetched at once.
const FAN_OUT: usize = 6;

/// A request that could not be made, or an answer that could not be read; only its message is
/// shown.
pub fn chat_error(message: impl Into<String>) -> ApiError {
    ApiError::new(0, message)
}

/// Fails unless `name` is a resource name of the given shape (`spaces/…`, `spaces/…/threads/…`),
/// so a caller's value can't reach another path of the API.
pub fn check(name: &str, shape: &[&str]) -> Result<(), ApiError> {
    let parts: Vec<&str> = name.split('/').collect();
    let allowed = |character: char| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.');
    let ok = parts.len() == shape.len() * 2
        && shape.iter().enumerate().all(|(index, kind)| {
            let id = parts[index * 2 + 1];
            parts[index * 2] == *kind && !id.is_empty() && id.chars().all(allowed)
        });
    if ok { Ok(()) } else { Err(chat_error(format!("Invalid Google Chat resource name: {name}"))) }
}

/// Fails unless `name` is a space's resource name (`spaces/…`).
pub fn check_space(name: &str) -> Result<(), ApiError> {
    check(name, &["spaces"])
}

/// `users/<address>`, the name the API takes for a person by email; fails for anything that
/// isn't a plain address, so a typed one can't reach another path or query of the API.
pub fn user_name(email: &str) -> Result<String, ApiError> {
    let allowed = |character: char| character.is_ascii_alphanumeric() || "-_.+'@".contains(character);
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || !parts[1].contains('.') || !email.chars().all(allowed) {
        return Err(chat_error(format!("Not an email address: {email}")));
    }
    Ok(format!("users/{email}"))
}

/// RFC 3339 in UTC with microseconds, the precision Google reports, for the `createTime` filter.
pub fn format_chat_date(date: Date) -> String {
    let unix = unix_seconds(date);
    let mut seconds = unix.floor();
    let mut micros = ((unix - seconds) * 1_000_000.0).round() as i64;
    if micros >= 1_000_000 {
        micros -= 1_000_000;
        seconds += 1.0;
    }
    let base = iso8601(date_from_unix(seconds));
    format!("{}.{micros:06}Z", base.trim_end_matches('Z'))
}

fn parse_date(text: &str) -> Option<Date> {
    parse_iso8601(text)
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.is_empty())
}

// MARK: - Resources

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpacesPage {
    spaces: Option<Vec<Space>>,
    next_page_token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
struct Thread {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveRef {
    drive_file_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataRef {
    resource_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    content_name: Option<String>,
    content_type: Option<String>,
    download_uri: Option<String>,
    drive_data_ref: Option<DriveRef>,
    /// Set for files uploaded to Chat; `media` downloads them.
    attachment_data_ref: Option<DataRef>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default)]
    name: String,
    sender: Option<User>,
    create_time: Option<String>,
    last_update_time: Option<String>,
    text: Option<String>,
    formatted_text: Option<String>,
    fallback_text: Option<String>,
    thread: Option<Thread>,
    #[serde(default)]
    thread_reply: bool,
    #[serde(default)]
    attachment: Vec<Attachment>,
    /// Only their presence matters.
    cards_v2: Option<Value>,
    cards: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagesPage {
    messages: Option<Vec<Message>>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct Membership {
    member: Option<User>,
}

#[derive(Deserialize)]
struct MembersPage {
    memberships: Option<Vec<Membership>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadState {
    /// `users/{user}/spaces/{space}/spaceReadState`: the only place the API names the caller.
    name: Option<String>,
    last_read_time: Option<String>,
}

impl ReadState {
    /// The caller's own `users/{id}`.
    fn user(&self) -> Option<String> {
        let parts: Vec<&str> = self.name.as_deref().unwrap_or_default().split('/').collect();
        (parts.len() >= 2 && parts[0] == "users" && !parts[1].is_empty() && parts[1] != "me")
            .then(|| format!("users/{}", parts[1]))
    }

    fn last_read(&self) -> Option<Date> {
        self.last_read_time.as_deref().and_then(parse_date)
    }
}

impl ChatUser {
    fn from_api(user: Option<&User>) -> Self {
        Self {
            name: user.map(|user| user.name.clone()).unwrap_or_default(),
            display_name: user
                .and_then(|user| user.display_name.clone())
                .filter(|name| name.chars().any(|character| !character.is_whitespace() || character == '\n')),
            email: non_empty(user.and_then(|user| user.email.clone())),
            avatar_url: non_empty(user.and_then(|user| user.avatar_url.clone())),
            bot: user.and_then(|user| user.kind.as_deref()) == Some("BOT"),
        }
    }
}

impl ChatMessage {
    /// `mine` overrides the comparison of the sender with `me`.
    fn from_api(message: &Message, me: Option<&str>, mine: Option<bool>) -> Self {
        let sender = ChatUser::from_api(message.sender.as_ref());
        // `text` has the markup stripped; `formattedText` keeps it (`*bold*`, `<url|label>`,
        // `<users/…>` mentions), with a trailing newline.
        let formatted = message.formatted_text.as_deref().map(|text| text.trim_end_matches('\n'));
        let text = [formatted, message.text.as_deref()]
            .into_iter()
            .flatten()
            .find(|text| !text.is_empty())
            .map(String::from)
            .or_else(|| message.fallback_text.clone())
            .unwrap_or_default();
        let created_raw = message.create_time.as_deref().and_then(parse_date);
        // The API sets `lastUpdateTime` on every message; only a later one is an edit.
        let edited = message
            .last_update_time
            .as_deref()
            .and_then(parse_date)
            .filter(|updated| created_raw.is_none_or(|created| *updated > created));
        let mine = mine.unwrap_or_else(|| me.is_some_and(|me| sender.name == me));
        Self {
            name: message.name.clone(),
            created: created_raw.unwrap_or(DISTANT_PAST),
            edited,
            text,
            thread: message.thread.as_ref().map(|thread| thread.name.clone()),
            thread_reply: message.thread_reply,
            mine,
            card: message.cards_v2.is_some() || message.cards.is_some(),
            attachments: message
                .attachment
                .iter()
                .map(|attachment| {
                    let download = non_empty(attachment.download_uri.clone());
                    let drive = attachment
                        .drive_data_ref
                        .as_ref()
                        .and_then(|drive| drive.drive_file_id.as_ref())
                        .map(|id| format!("https://drive.google.com/open?id={id}"));
                    ChatAttachment {
                        name: attachment.content_name.clone().unwrap_or_else(|| "Attachment".into()),
                        content_type: Some(attachment.content_type.clone().unwrap_or_default()),
                        url: download.or(drive),
                        resource: non_empty(
                            attachment.attachment_data_ref.as_ref().and_then(|data| data.resource_name.clone()),
                        ),
                    }
                })
                .collect(),
            sender,
        }
    }
}

// MARK: - Calls

/// The REST calls, under `root`.
struct Calls<'a> {
    api: &'a Api,
    root: &'a str,
}

impl Calls<'_> {
    fn url(&self, path: &str, query: &[(&str, &str)]) -> String {
        api::url(self.root, path, query)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ApiError> {
        self.api.get(url, Priority::Interactive, 0).await
    }

    fn json_request(method: reqwest::Method, url: String, body: &Value) -> Request {
        Request {
            method,
            url,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(serde_json::to_vec(body).expect("JSON bodies encode")),
        }
    }

    /// Every space the user is in. Pages are as large as the API allows, so this is one or two calls.
    async fn spaces(&self) -> Result<Vec<Space>, ApiError> {
        let first = self.url("spaces", &[("pageSize", "1000")]);
        let found = api::pages(
            first,
            usize::MAX,
            |url| async move { self.get::<SpacesPage>(&url).await.map(Some) },
            |page: &SpacesPage| {
                page.next_page_token
                    .as_deref()
                    .filter(|token| !token.is_empty())
                    .map(|token| self.url("spaces", &[("pageSize", "1000"), ("pageToken", token)]))
            },
            |page| page.spaces.unwrap_or_default(),
        )
        .await?;
        Ok(found.unwrap_or_default())
    }

    /// The newest `limit` messages created before `before` (RFC 3339), newest first.
    async fn messages(&self, space: &str, limit: usize, before: Option<&str>) -> Result<MessagesPage, ApiError> {
        let size = api::page_size(limit, 1000);
        let filter = before.map(|before| format!("createTime < \"{before}\""));
        let mut query = vec![("pageSize", size.as_str()), ("orderBy", "createTime desc")];
        if let Some(filter) = &filter {
            query.push(("filter", filter));
        }
        self.get(&self.url(&format!("{space}/messages"), &query)).await
    }

    /// Members, invited ones included: a DM with someone outside the domain who hasn't accepted
    /// yet has them only as an invitation.
    async fn members(&self, space: &str) -> Result<Vec<User>, ApiError> {
        let url = self.url(&format!("{space}/members"), &[("pageSize", "100"), ("showInvited", "true")]);
        let page: MembersPage = self.get(&url).await?;
        Ok(page.memberships.unwrap_or_default().into_iter().filter_map(|membership| membership.member).collect())
    }

    async fn read_state(&self, space: &str) -> Result<ReadState, ApiError> {
        self.get(&self.url(&format!("users/me/{space}/spaceReadState"), &[])).await
    }

    async fn mark_read(&self, space: &str, time: &str) -> Result<ReadState, ApiError> {
        let url = self.url(&format!("users/me/{space}/spaceReadState"), &[("updateMask", "lastReadTime")]);
        let request = Self::json_request(reqwest::Method::PATCH, url, &json!({ "lastReadTime": time }));
        self.api.decode(request, Priority::Interactive, 0).await
    }

    /// An uploaded attachment's bytes, by its `attachmentDataRef.resourceName`.
    async fn media(&self, resource: &str) -> Result<Vec<u8>, ApiError> {
        let parts: Vec<&str> = resource.split('/').collect();
        if resource.is_empty() || parts.iter().any(|part| part.is_empty() || *part == "." || *part == "..") {
            return Err(chat_error(format!("Not an attachment: {resource}")));
        }
        let path: Vec<String> = parts.iter().map(|part| api::percent_encoded(part)).collect();
        let url = self.url(&format!("media/{}", path.join("/")), &[("alt", "media")]);
        Ok(self.api.send(Request::get(url), Priority::Interactive, None, 0).await?.body)
    }

    /// Creates a message; never repeated (a POST).
    async fn create_message(&self, space: &str, text: &str, thread: Option<&str>) -> Result<Message, ApiError> {
        let query: &[(&str, &str)] =
            if thread.is_some() { &[("messageReplyOption", "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD")] } else { &[] };
        let mut body = json!({ "text": text });
        if let Some(thread) = thread {
            body["thread"] = json!({ "name": thread });
        }
        let request = Self::json_request(reqwest::Method::POST, self.url(&format!("{space}/messages"), query), &body);
        self.api.decode(request, Priority::Interactive, 0).await
    }

    /// The user's direct message with `user` (`users/…`); nil when they have none yet.
    async fn find_direct_message(&self, user: &str) -> Result<Option<Space>, ApiError> {
        match self.get(&self.url("spaces:findDirectMessage", &[("name", user)])).await {
            Ok(space) => Ok(Some(space)),
            Err(error) if error.status == 404 => Ok(None),
            // What Google answers for an address no Chat user has ("malformed user resource name").
            Err(error) if error.status == 400 => Err(chat_error(format!(
                "No Google Chat user has the address {}.",
                user.trim_start_matches("users/")
            ))),
            Err(error) => Err(error),
        }
    }

    /// Creates a direct message with one of `users` (`users/…`) or a group chat with several;
    /// for a direct message that exists already Google returns it. `request` makes a repeat
    /// return the space the first one created, so this is safe to send again.
    async fn set_up_space(&self, users: &[String], request: &str) -> Result<Space, ApiError> {
        let body = json!({
            "space": { "spaceType": if users.len() == 1 { "DIRECT_MESSAGE" } else { "GROUP_CHAT" } },
            "requestId": request,
            "memberships": users.iter().map(|user| json!({ "member": { "name": user, "type": "HUMAN" } })).collect::<Vec<_>>(),
        });
        let url = self.url("spaces:setup", &[]);
        let host = reqwest::Url::parse(&url).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default();
        let request = Self::json_request(reqwest::Method::POST, url, &body);
        let response = self.api.send(request, Priority::Interactive, Some(true), 0).await?;
        serde_json::from_slice(&response.body).map_err(|error| ApiError::unexpected_answer(&host, error))
    }
}

// MARK: - Summaries

struct Summary {
    space: ChatSpace,
    /// `lastActiveTime` as Google sent it; a different one means the space changed.
    last_active_raw: String,
    /// Its unread count went out of date; the next poll recounts it.
    stale: bool,
}

#[derive(Default)]
struct Memory {
    /// The user's own `users/{id}`, learned from a read state.
    me: Option<String>,
    summaries: HashMap<String, Summary>,
    /// Members of DMs and unnamed group chats, which name them.
    members: HashMap<String, Vec<User>>,
}

/// Google Chat for every account, with each account's space summaries remembered.
pub struct ChatRemote {
    root: String,
    memory: Mutex<HashMap<String, Memory>>,
}

impl ChatRemote {
    pub fn new(root: String) -> Self {
        Self { root, memory: Mutex::new(HashMap::new()) }
    }

    fn calls<'a>(&'a self, api: &'a Api) -> Calls<'a> {
        Calls { api, root: &self.root }
    }

    fn memory(&self) -> std::sync::MutexGuard<'_, HashMap<String, Memory>> {
        self.memory.lock().expect("chat memory lock")
    }

    /// The `limit` most recently active spaces, newest first.
    pub async fn spaces(&self, api: &Api, account: &str, limit: usize) -> Result<ChatSpaceList, ApiError> {
        let calls = self.calls(api);
        let limit = limit.max(1);
        let mut all = calls.spaces().await?;
        let active = |space: &Space| space.last_active_time.as_deref().and_then(parse_date).unwrap_or(DISTANT_PAST);
        all.sort_by(|a, b| active(b).partial_cmp(&active(a)).unwrap_or(std::cmp::Ordering::Equal));
        let more = all.len() > limit;
        all.truncate(limit);

        // What memory can't answer: spaces new or active since, and unread ones (read elsewhere?).
        let (mut stale, mut recheck) = (Vec::new(), Vec::new());
        {
            let memory = self.memory();
            for space in &all {
                match memory.get(account).and_then(|memory| memory.summaries.get(&space.name)) {
                    Some(known)
                        if !known.stale && Some(known.last_active_raw.as_str()) == space.last_active_time.as_deref() =>
                    {
                        if known.space.unread > 0 {
                            recheck.push(space.clone());
                        }
                    }
                    _ => stale.push(space.clone()),
                }
            }
        }
        // The futures are made before they are streamed: a stream over a borrowing closure
        // would make the spawned poll's future not `Send` for every lifetime.
        let summaries: Vec<_> = stale.iter().map(|space| self.summarize(&calls, account, space)).collect();
        let states: Vec<_> = recheck.iter().map(|space| calls.read_state(&space.name)).collect();
        let fresh = futures::stream::iter(summaries).buffered(FAN_OUT).collect::<Vec<_>>();
        let rechecked = futures::stream::iter(states).buffered(FAN_OUT).collect::<Vec<_>>();
        let (fresh, rechecked) = tokio::join!(fresh, rechecked);

        let mut failures = Vec::new();
        let mut memory = self.memory();
        let entry = memory.entry(account.to_string()).or_default();
        for result in fresh {
            match result {
                Ok(summary) => {
                    entry.summaries.insert(summary.space.name.clone(), summary);
                }
                Err(error) => failures.push(error),
            }
        }
        for (space, result) in recheck.iter().zip(rechecked) {
            let Ok(state) = result else { continue };
            if state.last_read_time.is_none() {
                continue;
            }
            let Some(summary) = entry.summaries.get_mut(&space.name) else { continue };
            let last_read = state.last_read();
            if last_read == summary.space.last_read {
                continue;
            }
            match last_read {
                Some(last_read) if last_read >= parse_date(&summary.last_active_raw).unwrap_or(DISTANT_PAST) => {
                    summary.space.unread = 0;
                    summary.space.last_read = Some(last_read);
                }
                // Read part of the way elsewhere: shown as it was until the next poll recounts it.
                _ => summary.stale = true,
            }
        }
        let spaces: Vec<ChatSpace> =
            all.iter().filter_map(|space| entry.summaries.get(&space.name).map(|summary| summary.space.clone())).collect();
        // A partial list is better than none, unless nothing at all could be read.
        if spaces.is_empty()
            && let Some(failure) = failures.into_iter().next()
        {
            return Err(failure);
        }
        Ok(ChatSpaceList { me: entry.me.clone(), more, spaces })
    }

    /// The newest `limit` messages created before `before` (default: now), ascending.
    pub async fn messages(
        &self,
        api: &Api,
        account: &str,
        space: &str,
        limit: usize,
        before: Option<Date>,
    ) -> Result<ChatMessagePage, ApiError> {
        check_space(space)?;
        let calls = self.calls(api);
        let before = before.map(format_chat_date);
        let (state, page) = tokio::join!(calls.read_state(space), calls.messages(space, limit, before.as_deref()));
        let page = page?;
        let state = state.ok();
        let me = self.learn_me(state.as_ref().and_then(ReadState::user), account);
        Ok(ChatMessagePage {
            messages: page
                .messages
                .unwrap_or_default()
                .iter()
                .rev()
                .map(|message| ChatMessage::from_api(message, me.as_deref(), None))
                .collect(),
            more: page.next_page_token.is_some_and(|token| !token.is_empty()),
            last_read: state.and_then(|state| state.last_read()),
        })
    }

    /// Marks the space read up to now; returns the new read time.
    pub async fn mark_read(&self, api: &Api, account: &str, space: &str) -> Result<Date, ApiError> {
        check_space(space)?;
        let now = crate::model::now();
        let state = self.calls(api).mark_read(space, &format_chat_date(now)).await?;
        let last_read = state.last_read().unwrap_or(now);
        if let Some(summary) = self.memory().get_mut(account).and_then(|memory| memory.summaries.get_mut(space)) {
            summary.space.unread = 0;
            summary.space.last_read = Some(last_read);
        }
        Ok(last_read)
    }

    /// An uploaded attachment's bytes.
    pub async fn attachment(&self, api: &Api, resource: &str) -> Result<Vec<u8>, ApiError> {
        self.calls(api).media(resource).await
    }

    /// Sends as the user, in `thread` when given. Not idempotent: never retried here.
    pub async fn send(
        &self,
        api: &Api,
        account: &str,
        space: &str,
        text: &str,
        thread: Option<&str>,
    ) -> Result<ChatMessage, ApiError> {
        check_space(space)?;
        if let Some(thread) = thread {
            check(thread, &["spaces", "threads"])?;
            if !thread.starts_with(&format!("{space}/")) {
                return Err(chat_error(format!("Thread {thread} is not in {space}")));
            }
        }
        if text.trim().is_empty() {
            return Err(chat_error("Message is empty"));
        }
        let message = self.calls(api).create_message(space, text, thread).await?;
        let me = message.sender.as_ref().map(|sender| sender.name.clone());
        {
            let mut memory = self.memory();
            let entry = memory.entry(account.to_string()).or_default();
            // The space's activity moved: its summary is refetched on the next poll.
            if let Some(summary) = entry.summaries.get_mut(space) {
                summary.stale = true;
            }
            if entry.me.is_none() {
                entry.me = me.clone();
            }
        }
        Ok(ChatMessage::from_api(&message, me.as_deref(), Some(true)))
    }

    /// The user's direct message with the person at `email`, summarized; nil when they have none.
    pub async fn direct_message(&self, api: &Api, account: &str, email: &str) -> Result<Option<ChatSpace>, ApiError> {
        let user = user_name(email)?;
        let calls = self.calls(api);
        let Some(space) = calls.find_direct_message(&user).await? else { return Ok(None) };
        self.remember(&calls, account, &space).await.map(Some)
    }

    /// Creates a direct message with one of `emails` or a group chat with several, summarized.
    /// `request` makes a repeat return the space the first one created.
    pub async fn set_up(&self, api: &Api, account: &str, emails: &[String], request: &str) -> Result<ChatSpace, ApiError> {
        let users = emails.iter().map(|email| user_name(email)).collect::<Result<Vec<_>, _>>()?;
        if users.is_empty() {
            return Err(chat_error("Add someone to chat with"));
        }
        let calls = self.calls(api);
        let space = calls.set_up_space(&users, request).await?;
        self.remember(&calls, account, &space).await
    }

    /// Summarizes a space the user just found or made and keeps it like a polled one.
    async fn remember(&self, calls: &Calls<'_>, account: &str, space: &Space) -> Result<ChatSpace, ApiError> {
        check_space(&space.name)?;
        let summary = self.summarize(calls, account, space).await?;
        let found = summary.space.clone();
        self.memory().entry(account.to_string()).or_default().summaries.insert(space.name.clone(), summary);
        Ok(found)
    }

    /// Forgets an account's summaries (it was removed).
    pub fn forget(&self, account: &str) {
        self.memory().remove(account);
    }

    async fn summarize(&self, calls: &Calls<'_>, account: &str, space: &Space) -> Result<Summary, ApiError> {
        let state = calls.read_state(&space.name).await?;
        let me = self.learn_me(state.user(), account);
        let page = calls.messages(&space.name, SUMMARY_MESSAGES, None).await?;
        let messages = page.messages.unwrap_or_default();
        let last_read = state.last_read();
        let unread = messages
            .iter()
            .filter(|message| {
                if message.sender.as_ref().map(|sender| &sender.name) == me.as_ref() {
                    return false;
                }
                let Some(last_read) = last_read else { return true };
                message.create_time.as_deref().and_then(parse_date).unwrap_or(DISTANT_PAST) > last_read
            })
            .count();
        let kind = match space.space_type.as_str() {
            "DIRECT_MESSAGE" if space.single_user_bot_dm => ChatSpaceKind::Bot,
            "DIRECT_MESSAGE" => ChatSpaceKind::Dm,
            "GROUP_CHAT" => ChatSpaceKind::Group,
            _ => ChatSpaceKind::Space,
        };
        let display_name = space.display_name.trim();
        let named = !display_name.is_empty();
        let mut members = Vec::new();
        if !named {
            let known = self.memory().get(account).and_then(|memory| memory.members.get(&space.name).cloned());
            members = match known {
                Some(known) => known,
                None => {
                    let fetched: Vec<User> = calls
                        .members(&space.name)
                        .await?
                        .into_iter()
                        .filter(|member| Some(&member.name) != me.as_ref())
                        .collect();
                    self.memory()
                        .entry(account.to_string())
                        .or_default()
                        .members
                        .insert(space.name.clone(), fetched.clone());
                    fetched
                }
            };
        }
        // Google withholds names of some members (apps, people without "affinity"), but not of
        // a message's sender: fill them in from the messages at hand.
        let members: Vec<User> = members
            .into_iter()
            .map(|member| {
                if member.display_name.is_some() {
                    return member;
                }
                messages
                    .iter()
                    .filter_map(|message| message.sender.as_ref())
                    .find(|sender| sender.name == member.name && sender.display_name.is_some())
                    .cloned()
                    .unwrap_or(member)
            })
            .collect();
        let title = if named { display_name.to_string() } else { title(&members, kind) };
        let summary = ChatSpace {
            name: space.name.clone(),
            kind,
            title,
            members: members.iter().map(|member| ChatUser::from_api(Some(member))).collect(),
            last_active: space.last_active_time.as_deref().and_then(parse_date),
            last_read,
            unread,
            latest: messages.first().map(|message| ChatMessage::from_api(message, me.as_deref(), None)),
        };
        Ok(Summary { space: summary, last_active_raw: space.last_active_time.clone().unwrap_or_default(), stale: false })
    }

    /// Records the user's id when a read state names it; returns the best known one.
    fn learn_me(&self, user: Option<String>, account: &str) -> Option<String> {
        let mut memory = self.memory();
        let entry = memory.entry(account.to_string()).or_default();
        if let Some(user) = user {
            entry.me = Some(user);
        }
        entry.me.clone()
    }
}

/// An unnamed space is named after the people in it.
fn title(members: &[User], kind: ChatSpaceKind) -> String {
    let names: Vec<&str> = members
        .iter()
        .filter_map(|member| member.display_name.as_deref().or(member.email.as_deref()))
        .filter(|name| name.chars().any(|character| !character.is_whitespace() || character == '\n'))
        .collect();
    match (names.len(), kind) {
        (0, ChatSpaceKind::Bot) => "App".into(),
        (0, ChatSpaceKind::Dm) => "Direct message".into(),
        (0, _) => "Group chat".into(),
        (1..=4, _) => names.join(", "),
        (count, _) => format!("{}, +{}", names[..3].join(", "), count - 3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_names_and_formats_dates() {
        assert!(check_space("spaces/AAA-b_c.1").is_ok());
        assert!(check_space("spaces/a/messages/b").is_err());
        assert!(check_space("spaces/../x").is_err());
        assert!(check("spaces/a/threads/t", &["spaces", "threads"]).is_ok());
        assert_eq!(user_name("a+b@x.com").unwrap(), "users/a+b@x.com");
        assert_eq!(user_name("a@x/y.com").unwrap_err().message, "Not an email address: a@x/y.com");
        assert!(user_name("nobody@localhost").is_err());
        let date = parse_iso8601("2026-11-07T23:05:07.123456Z").unwrap();
        assert_eq!(format_chat_date(date), "2026-11-07T23:05:07.123456Z");
        assert_eq!(format_chat_date(parse_iso8601("2026-11-07T23:05:07Z").unwrap()), "2026-11-07T23:05:07.000000Z");
    }

    #[test]
    fn reads_messages_like_the_swift_engine() {
        let message: Message = serde_json::from_value(json!({
            "name": "spaces/A/messages/1", "sender": { "name": "users/2", "displayName": "Ann Lee", "type": "HUMAN" },
            "createTime": "2026-11-07T10:00:00.5Z", "lastUpdateTime": "2026-11-07T10:00:00.5Z",
            "text": "plain", "formattedText": "*plain*\n\n", "thread": { "name": "spaces/A/threads/t" },
            "attachment": [
                { "contentName": "a.png", "contentType": "image/png", "attachmentDataRef": { "resourceName": "r/1" } },
                { "driveDataRef": { "driveFileId": "D" } },
            ],
            "cardsV2": [],
        }))
        .unwrap();
        let read = ChatMessage::from_api(&message, Some("users/2"), None);
        assert_eq!(read.text, "*plain*");
        assert_eq!(read.edited, None);
        assert!(read.mine && read.card);
        assert_eq!(read.attachments[0].resource.as_deref(), Some("r/1"));
        assert_eq!(read.attachments[1].name, "Attachment");
        assert_eq!(read.attachments[1].url.as_deref(), Some("https://drive.google.com/open?id=D"));
        assert_eq!(read.attachments[1].content_type.as_deref(), Some(""));
    }

    #[test]
    fn titles_unnamed_spaces() {
        let user = |name: &str| User { display_name: Some(name.into()), ..User::default() };
        let people: Vec<User> = ["A", "B", "C", "D", "E"].map(user).into();
        assert_eq!(title(&people, ChatSpaceKind::Group), "A, B, C, +2");
        assert_eq!(title(&people[..2], ChatSpaceKind::Group), "A, B");
        assert_eq!(title(&[], ChatSpaceKind::Dm), "Direct message");
        assert_eq!(title(&[], ChatSpaceKind::Bot), "App");
    }
}
