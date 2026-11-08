//! Gmail over its REST API, read as the Swift engine's `GmailBackend` reads it
//! (`Sources/MailstreamKit/Backend/GmailBackend.swift`). Mailbox ids are label ids plus the
//! virtual `ALL` (All Mail: everything but spam and trash). Envelopes' immutable fields are kept
//! by message id in the same per-account JSON file under Caches, their labels kept current
//! through the mailbox history, so a refresh or relaunch fetches metadata only for messages it
//! has never seen.
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::api::{self, Api, ApiError, Priority, Request};
use crate::assembly::{AttachedMessage, MessageAssembly};
use crate::backend::{EnvelopePage, SearchHit};
use crate::model::{Address, DISTANT_PAST, Envelope, Mailbox, MailboxRole, MessageContent, date_from_unix};
use crate::text;

mod changes;

pub const ALL_MAIL: &str = "ALL";

/// Where Gmail's REST API lives: its root, and its batch endpoint (whose sub-requests name
/// paths under the root).
#[derive(Clone, Debug)]
pub struct GmailEndpoints {
    pub root: String,
    pub batch: String,
}

impl Default for GmailEndpoints {
    fn default() -> Self {
        Self {
            root: "https://gmail.googleapis.com/gmail/v1/users/me/".into(),
            batch: "https://gmail.googleapis.com/batch/gmail/v1".into(),
        }
    }
}

impl GmailEndpoints {
    fn url(&self, path: &str, query: &[(&str, &str)]) -> String {
        api::url(&self.root, path, query)
    }

    /// The root's path, which batch sub-requests start with.
    fn path(&self) -> String {
        reqwest::Url::parse(&self.root).map(|url| url.path().to_string()).unwrap_or_else(|_| "/".into())
    }

    fn batch_host(&self) -> String {
        reqwest::Url::parse(&self.batch).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default()
    }
}

/// The signed-out `account`'s envelope cache file.
pub fn cache_file(root: &std::path::Path, account: &str) -> PathBuf {
    root.join("gmail-envelopes-v1").join(format!("{}.json", crate::cache::hash(&[account])))
}

pub struct Gmail {
    api: Arc<Api>,
    endpoints: Arc<GmailEndpoints>,
    cache: Arc<EnvelopeCache>,
}

const ROLES: [(&str, MailboxRole); 7] = [
    ("INBOX", MailboxRole::Inbox),
    ("SENT", MailboxRole::Sent),
    ("DRAFT", MailboxRole::Drafts),
    ("TRASH", MailboxRole::Trash),
    ("SPAM", MailboxRole::Junk),
    ("STARRED", MailboxRole::Starred),
    ("IMPORTANT", MailboxRole::Important),
];

/// `resultSizeEstimate` is always present, so a list with no matches still answers `{…}`.
const LIST_FIELDS: &str = "messages(id),nextPageToken,resultSizeEstimate";

#[derive(Deserialize)]
struct MessageList {
    messages: Option<Vec<MessageItem>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct MessageItem {
    id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GmailMessage {
    id: String,
    #[serde(rename = "threadId")]
    thread_id: Option<String>,
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
    #[serde(rename = "internalDate")]
    internal_date: Option<String>,
    #[serde(rename = "sizeEstimate")]
    size_estimate: Option<i64>,
    payload: Option<Part>,
}

#[derive(Clone, Debug, Deserialize)]
struct Part {
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    filename: Option<String>,
    headers: Option<Vec<Header>>,
    body: Option<Body>,
    parts: Option<Vec<Part>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Header {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Body {
    #[serde(rename = "attachmentId")]
    attachment_id: Option<String>,
    data: Option<String>,
}

impl Part {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.as_ref()?.iter().find(|header| header.name.eq_ignore_ascii_case(name)).map(|header| header.value.as_str())
    }

    /// Whether some part below this one is a named file.
    fn has_file(&self) -> bool {
        self.parts.iter().flatten().any(|part| part.filename.as_deref().is_some_and(|name| !name.is_empty()) || part.has_file())
    }
}

impl Gmail {
    pub fn new(api: Arc<Api>, endpoints: Arc<GmailEndpoints>, cache_root: &std::path::Path) -> Self {
        let cache = Arc::new(EnvelopeCache::new(api.clone(), endpoints.clone(), cache_file(cache_root, &api.account)));
        Self { api, endpoints, cache }
    }

    pub async fn identity(&self) -> Result<Address, ApiError> {
        #[derive(Deserialize)]
        struct Profile {
            #[serde(rename = "emailAddress")]
            email_address: String,
        }
        #[derive(Deserialize)]
        struct SendAsList {
            #[serde(rename = "sendAs")]
            send_as: Option<Vec<SendAs>>,
        }
        #[derive(Deserialize)]
        struct SendAs {
            #[serde(rename = "sendAsEmail")]
            send_as_email: String,
            #[serde(rename = "displayName")]
            display_name: Option<String>,
            #[serde(rename = "isPrimary")]
            is_primary: Option<bool>,
        }
        let profile_url = self.endpoints.url("profile", &[]);
        let send_as_url = self.endpoints.url("settings/sendAs", &[]);
        let (profile, send_as) = tokio::join!(
            self.api.get::<Profile>(&profile_url, Priority::Interactive, 1),
            self.api.get::<SendAsList>(&send_as_url, Priority::Interactive, 1),
        );
        let email = profile?.email_address;
        let entries = send_as?.send_as.unwrap_or_default();
        let primary = entries
            .iter()
            .find(|entry| entry.is_primary == Some(true))
            .or_else(|| entries.iter().find(|entry| entry.send_as_email.eq_ignore_ascii_case(&email)));
        let name = primary.and_then(|entry| entry.display_name.as_deref()).unwrap_or("").trim_matches(' ').to_string();
        Ok(Address::new((!name.is_empty()).then_some(name), email))
    }

    pub async fn mailboxes(&self, counts: bool) -> Result<Vec<Mailbox>, ApiError> {
        #[derive(Deserialize)]
        struct LabelList {
            labels: Option<Vec<Label>>,
        }
        #[derive(Deserialize)]
        struct Label {
            id: String,
            name: String,
            #[serde(rename = "type")]
            kind: Option<String>,
        }
        #[derive(Deserialize)]
        struct Tally {
            #[serde(rename = "messagesTotal")]
            messages_total: Option<i64>,
            #[serde(rename = "messagesUnread")]
            messages_unread: Option<i64>,
        }
        let account = &self.api.account;
        let url = self.endpoints.url("labels", &[("fields", "labels(id,name,type)")]);
        let labels = self.api.get::<LabelList>(&url, Priority::Interactive, 1).await?.labels.unwrap_or_default();
        let mut boxes: Vec<Mailbox> = labels
            .into_iter()
            .filter_map(|label| {
                if let Some((_, role)) = ROLES.iter().find(|(id, _)| *id == label.id) {
                    return Some(Mailbox {
                        account: account.clone(),
                        id: label.id,
                        name: role.title().into(),
                        role: *role,
                        unread: None,
                        total: None,
                    });
                }
                (label.kind.as_deref() == Some("user")).then(|| Mailbox {
                    account: account.clone(),
                    id: label.id,
                    name: label.name,
                    role: MailboxRole::Other,
                    unread: None,
                    total: None,
                })
            })
            .collect();
        if counts {
            let ids: Vec<String> = boxes.iter().map(|mailbox| mailbox.id.clone()).collect();
            let query = api::form_encoded(&[("fields", "messagesTotal,messagesUnread")]);
            let tallies: HashMap<String, Tally> =
                batch_get(&self.api, &self.endpoints, "labels", &ids, &query, 1, Priority::Background).await?;
            for mailbox in &mut boxes {
                mailbox.unread = tallies.get(&mailbox.id).and_then(|tally| tally.messages_unread);
                mailbox.total = tallies.get(&mailbox.id).and_then(|tally| tally.messages_total);
            }
        }
        boxes.push(Mailbox {
            account: account.clone(),
            id: ALL_MAIL.into(),
            name: MailboxRole::All.title().into(),
            role: MailboxRole::All,
            unread: None,
            total: None,
        });
        Ok(boxes)
    }

    pub async fn envelopes(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        page_size: usize,
        priority: Priority,
    ) -> Result<EnvelopePage, ApiError> {
        let size = api::page_size(page_size, 500);
        let mut query = scope(mailbox);
        query.push(("maxResults", &size));
        query.push(("fields", LIST_FIELDS));
        if let Some(cursor) = cursor {
            query.push(("pageToken", cursor));
        }
        let (ids, next) = self.listing(self.endpoints.url("messages", &query), priority).await?;
        Ok(EnvelopePage { envelopes: self.cache.envelopes(&ids, priority).await?, next })
    }

    /// No label filter: Gmail then searches everything but spam and trash, as its web search does.
    pub async fn search(&self, query: &str, page_size: usize) -> Result<Vec<SearchHit>, ApiError> {
        let size = api::page_size(page_size, 500);
        let url = self.endpoints.url("messages", &[("q", query), ("maxResults", &size), ("fields", LIST_FIELDS)]);
        let (ids, _) = self.listing(url, Priority::Interactive).await?;
        self.cache.hits(&ids, Priority::Interactive).await
    }

    /// Lists message ids, bringing cached labels up to date meanwhile.
    async fn listing(&self, url: String, priority: Priority) -> Result<(Vec<String>, Option<String>), ApiError> {
        let (synchronized, page) =
            tokio::join!(self.cache.clone().synchronize(priority), self.api.get::<MessageList>(&url, priority, 5));
        let page = page?;
        synchronized?;
        Ok((page.messages.unwrap_or_default().into_iter().map(|item| item.id).collect(), page.next_page_token))
    }

    pub async fn content(
        &self,
        id: &str,
        directory: PathBuf,
        url_base: String,
        priority: Priority,
    ) -> Result<MessageContent, ApiError> {
        let url = self.endpoints.url(&format!("messages/{}", api::percent_encoded(id)), &[("format", "full")]);
        let message: GmailMessage = self.api.get(&url, priority, 5).await?;
        let Some(payload) = message.payload else {
            return Err(ApiError::malformed(format!("Gmail returned message {id} without content")));
        };
        let fetched = self.attachment_data(&payload, id, priority).await?;
        let assembly = MessageAssembly::new(directory, url_base).map_err(|error| ApiError::new(0, error.to_string()))?;
        let mut renderer = Renderer { assembly, fetched, next_index: 0 };
        let body = renderer.render(&payload, false);
        let html = body.as_ref().filter(|body| body.primary).and_then(|body| body.html.clone());
        let text = body.and_then(|body| body.text);
        Ok(renderer.assembly.finish(
            html,
            text,
            text::parse_address_list(payload.header("Cc").unwrap_or("")),
            text::parse_address_list(payload.header("Reply-To").unwrap_or("")),
            text::parse_message_id_list(payload.header("References")),
        ))
    }

    /// Bodies Gmail left out of the message (`body.attachmentId`), by attachment id.
    async fn attachment_data(&self, payload: &Part, message: &str, priority: Priority) -> Result<HashMap<String, Vec<u8>>, ApiError> {
        fn collect(part: &Part, wanted: &mut Vec<String>) {
            match &part.parts {
                Some(parts) if !parts.is_empty() => parts.iter().for_each(|child| collect(child, wanted)),
                _ => {
                    if let Some(id) = part.body.as_ref().and_then(|body| body.attachment_id.clone()) {
                        wanted.push(id);
                    }
                }
            }
        }
        let mut wanted = Vec::new();
        collect(payload, &mut wanted);
        let fetches = wanted.into_iter().map(|attachment| async move {
            let path = format!("messages/{}/attachments/{}", api::percent_encoded(message), api::percent_encoded(&attachment));
            let body: Body = self.api.get(&self.endpoints.url(&path, &[]), priority, 5).await?;
            Ok::<_, ApiError>((attachment, body.data.and_then(|data| text::base64_url_decode(&data)).unwrap_or_default()))
        });
        Ok(futures::future::try_join_all(fetches).await?.into_iter().collect())
    }
}

/// Query parameters naming `mailbox`; All Mail is the unfiltered listing.
fn scope(mailbox: &str) -> Vec<(&'static str, &str)> {
    match mailbox {
        ALL_MAIL => vec![],
        "TRASH" | "SPAM" => vec![("labelIds", mailbox), ("includeSpamTrash", "true")],
        _ => vec![("labelIds", mailbox)],
    }
}

// MARK: - Batch requests

/// `GET <collection>/<id>?<query>` for every id through Gmail's batch endpoint, 50 per request
/// and one batch at a time (parallel batches were what Gmail refused). Sub-requests Gmail
/// throttles or fails transiently are sent again. Answers are keyed by id; ids that no longer
/// exist are left out. `unit_cost` is one sub-request's quota units.
async fn batch_get<T: serde::de::DeserializeOwned>(
    api: &Api,
    endpoints: &GmailEndpoints,
    collection: &str,
    ids: &[String],
    query: &str,
    unit_cost: u32,
    priority: Priority,
) -> Result<HashMap<String, T>, ApiError> {
    let numbered: Vec<(usize, String)> =
        ids.chunks(50).flat_map(|chunk| chunk.iter().cloned().enumerate()).collect();
    let found: std::sync::Mutex<HashMap<String, T>> = std::sync::Mutex::new(HashMap::new());
    let root_path = endpoints.path();
    api.batched(numbered, 50, "Gmail", |pending: Vec<(usize, String)>| {
        let (found, root_path) = (&found, &root_path);
        async move {
            let boundary = format!("batch_{}", random_hex(32, false));
            let mut body = String::new();
            for (number, id) in &pending {
                body.push_str(&format!(
                    "--{boundary}\r\nContent-Type: application/http\r\nContent-ID: <m{number}>\r\n\r\nGET {root_path}{collection}/{}?{query}\r\n\r\n",
                    api::percent_encoded(id)
                ));
            }
            body.push_str(&format!("--{boundary}--\r\n"));
            let request =
                Request::post(endpoints.batch.clone(), &format!("multipart/mixed; boundary={boundary}"), body.into_bytes());
            let response = api.send(request, priority, Some(true), unit_cost * pending.len() as u32).await?;
            let Some(response_boundary) = text::header_parameter("boundary", response.content_type.as_deref()) else {
                return Err(ApiError::new(response.status, "Gmail answered a batch request without parts"));
            };
            // Whatever stays in `retry` (throttled, failed transiently, unanswered) is sent again.
            let mut retry: HashMap<usize, String> = pending.iter().cloned().collect();
            for part in batch_parts(&response.body, &response_boundary) {
                let Some(number) = part.index else { continue };
                let Some(id) = retry.get(&number).cloned() else { continue };
                match part.status {
                    200 => {
                        let value = serde_json::from_slice::<T>(part.body.as_bytes())
                            .map_err(|error| ApiError::unexpected_answer(&endpoints.batch_host(), error))?;
                        found.lock().expect("batch answers").insert(id, value);
                    }
                    404 => {}
                    status => {
                        if api::retry(status, part.body.as_bytes()).is_none() {
                            return Err(ApiError::from_body(status, part.body.as_bytes()));
                        }
                        continue;
                    }
                }
                retry.remove(&number);
            }
            let again = pending.into_iter().filter(|(number, _)| retry.contains_key(number)).collect();
            Ok((again, None))
        }
    })
    .await?;
    Ok(found.into_inner().expect("batch answers"))
}

struct BatchPart {
    index: Option<usize>,
    status: u16,
    body: String,
}

/// Splits a multipart/mixed batch answer into its HTTP responses; `index` comes from the
/// `Content-ID` (`<response-m3>`) of the sub-request it answers.
fn batch_parts(data: &[u8], boundary: &str) -> Vec<BatchPart> {
    static CONTENT_ID: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)content-id:\s*<[^>]*?(\d+)>").unwrap());
    let text = String::from_utf8_lossy(data).replace("\r\n", "\n");
    let mut parts = Vec::new();
    for chunk in text.split(&format!("--{boundary}")).skip(1) {
        if chunk.starts_with("--") {
            break;
        }
        let Some((headers, response)) = chunk.split_once("\n\n") else { continue };
        let index = CONTENT_ID.captures(headers).and_then(|found| found[1].parse().ok());
        let status_line = response.split('\n').next().unwrap_or("");
        let Some(status) = status_line.split(' ').filter(|piece| !piece.is_empty()).nth(1).and_then(|code| code.parse().ok())
        else {
            continue;
        };
        let body = response.split_once("\n\n").map_or("", |(_, body)| body);
        parts.push(BatchPart { index, status, body: body.to_string() });
    }
    parts
}

/// `count` random hex digits, lowercase unless not `lowercase`.
pub fn random_hex(count: usize, lowercase: bool) -> String {
    let mut bytes = vec![0u8; count.div_ceil(2)];
    let _ = getrandom::fill(&mut bytes);
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let hex = &hex[..count];
    if lowercase { hex.to_string() } else { hex.to_uppercase() }
}

// MARK: - Envelope cache

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    envelope: Envelope,
    labels: BTreeSet<String>,
}

impl Entry {
    fn current(&self) -> Envelope {
        let mut envelope = self.envelope.clone();
        envelope.flags = Envelope::flags(
            !self.labels.contains("UNREAD"),
            self.labels.contains("STARRED"),
            self.labels.contains("DRAFT"),
        );
        envelope
    }
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    #[serde(rename = "historyID")]
    history_id: u64,
    entries: HashMap<String, Entry>,
}

/// Newest messages kept on disk; older ones are fetched again if a listing reaches them.
const STORED_LIMIT: usize = 10_000;

type Fetch = Shared<BoxFuture<'static, Result<Arc<HashMap<String, Entry>>, ApiError>>>;
type Pull = Shared<BoxFuture<'static, Result<(), ApiError>>>;

#[derive(Default)]
struct CacheState {
    loaded: bool,
    save_scheduled: bool,
    entries: HashMap<String, Entry>,
    /// Metadata fetches in flight, by the ids they fetch.
    loading: HashMap<String, Fetch>,
    history_id: Option<u64>,
    /// Ids each change to `entries` touched (nil: all), by generation, so metadata fetched while
    /// a change landed is not cached with labels that change missed.
    generation: u64,
    touched: VecDeque<(u64, Option<HashSet<String>>)>,
    /// Callers that asked for a history pull; the running pull answers those up to `covers`.
    tickets: u64,
    running: Option<(u64, Pull)>,
}

impl CacheState {
    fn advance(&mut self, ids: Option<HashSet<String>>) {
        self.generation += 1;
        self.touched.push_back((self.generation, ids));
        while self.touched.len() > 32 {
            self.touched.pop_front();
        }
    }

    /// Ids changed after generation `since`; nil when that is everything or no longer known.
    fn changed(&self, since: u64) -> Option<HashSet<String>> {
        if since >= self.generation {
            return Some(HashSet::new());
        }
        let first = self.touched.front()?;
        if first.0 > since + 1 {
            return None;
        }
        let mut ids = HashSet::new();
        for (generation, changed) in &self.touched {
            if *generation > since {
                ids.extend(changed.as_ref()?.iter().cloned());
            }
        }
        Some(ids)
    }

    fn reset(&mut self) {
        self.entries.clear();
        self.history_id = None;
        self.advance(None);
    }
}

struct EnvelopeCache {
    api: Arc<Api>,
    endpoints: Arc<GmailEndpoints>,
    file: PathBuf,
    state: Mutex<CacheState>,
}

impl EnvelopeCache {
    fn new(api: Arc<Api>, endpoints: Arc<GmailEndpoints>, file: PathBuf) -> Self {
        Self { api, endpoints, file, state: Mutex::new(CacheState::default()) }
    }

    /// Reads the disk snapshot once, before anything else touches the entries.
    fn load(&self, state: &mut CacheState) {
        if state.loaded {
            return;
        }
        state.loaded = true;
        let Some(snapshot) = std::fs::read(&self.file).ok().and_then(|data| serde_json::from_slice::<Snapshot>(&data).ok())
        else {
            return;
        };
        state.history_id = Some(snapshot.history_id);
        state.entries = snapshot.entries;
    }

    /// Writes the snapshot a moment after a change, so a burst of changes is one write.
    fn schedule_save(self: &Arc<Self>, state: &mut CacheState) {
        if state.save_scheduled {
            return;
        }
        state.save_scheduled = true;
        let cache = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Some(cache) = cache.upgrade() {
                cache.save().await;
            }
        });
    }

    async fn save(&self) {
        let (history_id, mut kept) = {
            let mut state = self.state.lock().await;
            state.save_scheduled = false;
            (state.history_id, state.entries.clone())
        };
        let file = self.file.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let Some(history_id) = history_id else {
                let _ = std::fs::remove_file(&file);
                return;
            };
            if kept.len() > STORED_LIMIT {
                let mut newest: Vec<(String, Entry)> = kept.into_iter().collect();
                newest.sort_by(|a, b| {
                    let date = |entry: &Entry| entry.envelope.date.unwrap_or(DISTANT_PAST);
                    date(&b.1).total_cmp(&date(&a.1))
                });
                newest.truncate(STORED_LIMIT);
                kept = newest.into_iter().collect();
            }
            let data = crate::json::encode(&Snapshot { history_id, entries: kept }, false, true);
            if let Some(directory) = file.parent() {
                let _ = crate::files::replace(directory, &file, &data);
            }
        })
        .await;
    }

    /// Applies every label change and deletion since the last pull. A pull already running
    /// answers callers that arrived before it started; later ones get a pull of their own.
    async fn synchronize(self: Arc<Self>, priority: Priority) -> Result<(), ApiError> {
        let pull = {
            let mut state = self.state.lock().await;
            self.load(&mut state);
            state.tickets += 1;
            let ticket = state.tickets;
            loop {
                let Some((covers, running)) = state.running.clone() else { break };
                drop(state);
                if covers >= ticket {
                    return running.await;
                }
                let _ = running.await;
                state = self.state.lock().await;
            }
            if state.history_id.is_none() {
                return Ok(());
            }
            let cache = self.clone();
            let pull: Pull = async move { cache.pull(priority).await }.boxed().shared();
            state.running = Some((state.tickets, pull.clone()));
            pull
        };
        pull.await
    }

    async fn pull(self: Arc<Self>, priority: Priority) -> Result<(), ApiError> {
        let result = self.pull_pages(priority).await;
        self.state.lock().await.running = None;
        result
    }

    async fn pull_pages(self: &Arc<Self>, priority: Priority) -> Result<(), ApiError> {
        #[derive(Deserialize)]
        struct Page {
            history: Option<Vec<Record>>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
            #[serde(rename = "historyId")]
            history_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct Record {
            #[serde(rename = "labelsAdded")]
            labels_added: Option<Vec<Change>>,
            #[serde(rename = "labelsRemoved")]
            labels_removed: Option<Vec<Change>>,
            #[serde(rename = "messagesDeleted")]
            messages_deleted: Option<Vec<Change>>,
        }
        #[derive(Deserialize)]
        struct Change {
            message: MessageItem,
            #[serde(rename = "labelIds")]
            label_ids: Option<Vec<String>>,
        }
        let Some(start) = self.state.lock().await.history_id else { return Ok(()) };
        let mut records: Vec<Record> = Vec::new();
        let mut latest = start;
        let mut page_token: Option<String> = None;
        let start_text = start.to_string();
        for _ in 0..10 {
            let mut query = vec![
                ("startHistoryId", start_text.as_str()),
                ("historyTypes", "labelAdded"),
                ("historyTypes", "labelRemoved"),
                ("historyTypes", "messageDeleted"),
                ("maxResults", "500"),
                (
                    "fields",
                    "history(labelsAdded(message(id),labelIds),labelsRemoved(message(id),labelIds),messagesDeleted(message(id))),nextPageToken,historyId",
                ),
            ];
            if let Some(token) = &page_token {
                query.push(("pageToken", token));
            }
            let page: Page = match self.api.get(&self.endpoints.url("history", &query), priority, 2).await {
                Ok(page) => page,
                Err(error) if error.status == 404 => {
                    let mut state = self.state.lock().await;
                    state.reset();
                    self.schedule_save(&mut state);
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            records.extend(page.history.unwrap_or_default());
            if let Some(id) = page.history_id.and_then(|id| id.parse::<u64>().ok()) {
                latest = latest.max(id);
            }
            let Some(next) = page.next_page_token else {
                let mut state = self.state.lock().await;
                let mut ids = HashSet::new();
                for record in records {
                    for change in record.labels_added.unwrap_or_default() {
                        if let Some(entry) = state.entries.get_mut(&change.message.id) {
                            entry.labels.extend(change.label_ids.unwrap_or_default());
                        }
                        ids.insert(change.message.id);
                    }
                    for change in record.labels_removed.unwrap_or_default() {
                        if let Some(entry) = state.entries.get_mut(&change.message.id) {
                            for label in change.label_ids.unwrap_or_default() {
                                entry.labels.remove(&label);
                            }
                        }
                        ids.insert(change.message.id);
                    }
                    for change in record.messages_deleted.unwrap_or_default() {
                        state.entries.remove(&change.message.id);
                        ids.insert(change.message.id);
                    }
                }
                state.history_id = Some(latest);
                // A pull that changed nothing leaves fetches in flight current.
                if !ids.is_empty() {
                    state.advance(Some(ids));
                    self.schedule_save(&mut state);
                }
                return Ok(());
            };
            page_token = Some(next);
        }
        // More change than a refetch costs.
        let mut state = self.state.lock().await;
        state.reset();
        self.schedule_save(&mut state);
        Ok(())
    }

    async fn envelopes(self: &Arc<Self>, ids: &[String], priority: Priority) -> Result<Vec<Envelope>, ApiError> {
        Ok(self.lookup(ids, priority).await?.iter().map(Entry::current).collect())
    }

    /// The envelopes placed in the mailbox their labels list them under: Inbox, Sent or Drafts
    /// when they carry that label, else All Mail.
    async fn hits(self: &Arc<Self>, ids: &[String], priority: Priority) -> Result<Vec<SearchHit>, ApiError> {
        Ok(self
            .lookup(ids, priority)
            .await?
            .iter()
            .map(|entry| {
                let mailbox = ["INBOX", "SENT", "DRAFT"].into_iter().find(|label| entry.labels.contains(*label));
                SearchHit { mailbox: mailbox.unwrap_or(ALL_MAIL).to_string(), envelope: entry.current() }
            })
            .collect())
    }

    /// Entries of `ids` in that order, fetching metadata for the ones not cached; messages that
    /// no longer exist are left out. Ids another caller is already fetching share that fetch.
    async fn lookup(self: &Arc<Self>, ids: &[String], priority: Priority) -> Result<Vec<Entry>, ApiError> {
        let waits: Vec<Fetch> = {
            let mut state = self.state.lock().await;
            self.load(&mut state);
            let unknown: Vec<&String> = ids.iter().filter(|id| !state.entries.contains_key(*id)).collect();
            let missing: Vec<String> =
                unknown.iter().filter(|id| !state.loading.contains_key(**id)).map(|id| (*id).clone()).collect();
            if !missing.is_empty() {
                let cache = self.clone();
                let ids = missing.clone();
                let fetch: Fetch = async move { cache.fetch(ids, priority).await.map(Arc::new) }.boxed().shared();
                for id in missing {
                    state.loading.insert(id, fetch.clone());
                }
            }
            let mut waits: Vec<Fetch> = Vec::new();
            for id in unknown {
                if let Some(fetch) = state.loading.get(id)
                    && !waits.iter().any(|known| known.ptr_eq(fetch))
                {
                    waits.push(fetch.clone());
                }
            }
            waits
        };
        let mut fetched: HashMap<String, Entry> = HashMap::new();
        for wait in waits {
            for (id, entry) in wait.await?.iter() {
                fetched.entry(id.clone()).or_insert_with(|| entry.clone());
            }
        }
        let state = self.state.lock().await;
        Ok(ids.iter().filter_map(|id| state.entries.get(id).or_else(|| fetched.get(id)).cloned()).collect())
    }

    async fn fetch(self: Arc<Self>, ids: Vec<String>, priority: Priority) -> Result<HashMap<String, Entry>, ApiError> {
        let result = self.fetch_chunks(&ids, priority).await;
        let mut state = self.state.lock().await;
        for id in &ids {
            state.loading.remove(id);
        }
        result
    }

    async fn fetch_chunks(self: &Arc<Self>, ids: &[String], priority: Priority) -> Result<HashMap<String, Entry>, ApiError> {
        // The baseline comes first: history from it replays any change racing the fetch.
        if self.state.lock().await.history_id.is_none() {
            let baseline = self.history_id(priority).await?;
            let mut state = self.state.lock().await;
            if state.history_id.is_none() {
                state.history_id = baseline;
            }
        }
        // Fifty at a time, each stored as it lands: a refusal or quit midway keeps what came before.
        let mut fetched = HashMap::new();
        for chunk in ids.chunks(50) {
            let since = self.state.lock().await.generation;
            let messages = self.metadata(chunk, &metadata_query(), priority).await?;
            // Metadata shows only the top MIME type; a multipart/mixed message has attachments
            // only if a part carries a file name.
            let mixed: Vec<String> = messages
                .iter()
                .filter(|message| {
                    message.payload.as_ref().and_then(|payload| payload.mime_type.as_deref()).map(str::to_lowercase).as_deref()
                        == Some("multipart/mixed")
                })
                .map(|message| message.id.clone())
                .collect();
            let with_files: HashSet<String> = if mixed.is_empty() {
                HashSet::new()
            } else {
                self.metadata(&mixed, &parts_query(), priority)
                    .await?
                    .into_iter()
                    .filter(|message| message.payload.as_ref().is_some_and(Part::has_file))
                    .map(|message| message.id)
                    .collect()
            };
            let mut state = self.state.lock().await;
            let stale = state.changed(since);
            for message in &messages {
                let entry = entry(message, with_files.contains(&message.id));
                fetched.insert(message.id.clone(), entry.clone());
                if let Some(stale) = &stale
                    && !stale.contains(&message.id)
                    && !state.entries.contains_key(&message.id)
                {
                    state.entries.insert(message.id.clone(), entry);
                }
            }
            self.schedule_save(&mut state);
        }
        Ok(fetched)
    }

    /// The mailbox's current history id.
    async fn history_id(&self, priority: Priority) -> Result<Option<u64>, ApiError> {
        #[derive(Deserialize)]
        struct Profile {
            #[serde(rename = "historyId")]
            history_id: Option<String>,
        }
        let url = self.endpoints.url("profile", &[("fields", "historyId")]);
        let profile: Profile = self.api.get(&url, priority, 1).await?;
        Ok(profile.history_id.and_then(|id| id.parse().ok()))
    }

    async fn metadata(&self, ids: &[String], query: &str, priority: Priority) -> Result<Vec<GmailMessage>, ApiError> {
        let found: HashMap<String, GmailMessage> =
            batch_get(&self.api, &self.endpoints, "messages", ids, query, 5, priority).await?;
        Ok(found.into_values().collect())
    }
}

fn metadata_query() -> String {
    let mut query: Vec<(&str, &str)> = vec![("format", "metadata")];
    for header in ["From", "To", "Subject", "Message-ID", "In-Reply-To", "References"] {
        query.push(("metadataHeaders", header));
    }
    query.push(("fields", "id,threadId,labelIds,historyId,internalDate,sizeEstimate,payload(mimeType,headers)"));
    api::form_encoded(&query)
}

/// The part tree's file names, two levels deep, without bodies or headers.
fn parts_query() -> String {
    api::form_encoded(&[("format", "full"), ("fields", "id,payload(parts(filename,parts(filename)))")])
}

fn entry(message: &GmailMessage, has_attachment: bool) -> Entry {
    let payload = message.payload.as_ref();
    let header = |name: &str| payload.and_then(|payload| payload.header(name));
    let in_reply_to = text::parse_message_id_list(header("In-Reply-To"));
    let in_reply_to = if in_reply_to.is_empty() {
        text::parse_message_id_list(header("References")).into_iter().last().into_iter().collect()
    } else {
        in_reply_to
    };
    let envelope = Envelope {
        id: message.id.clone(),
        thread: message.thread_id.clone(),
        message_id: header("Message-ID").map(text::normalize_message_id).filter(|id| !id.is_empty()),
        in_reply_to,
        flags: BTreeSet::new(),
        subject: text::decode_header(header("Subject").unwrap_or("")),
        from: text::parse_address_list(header("From").unwrap_or("")),
        to: text::parse_address_list(header("To").unwrap_or("")),
        date: message.internal_date.as_deref().and_then(|ms| ms.parse::<f64>().ok()).map(|ms| date_from_unix(ms / 1000.0)),
        size: message.size_estimate.unwrap_or(0),
        has_attachment: Some(has_attachment),
    };
    Entry { envelope, labels: message.label_ids.iter().flatten().cloned().collect() }
}

// MARK: - Content

/// What a part shows. `primary` when `html` is a real HTML body; other HTML (an inline image)
/// only joins when a sibling has one.
struct Segment {
    html: Option<String>,
    text: Option<String>,
    primary: bool,
}

/// Walks a `format=full` payload: picks the HTML and plain bodies, stores every other part
/// through `MessageAssembly` in part order, and renders forwarded messages inline.
struct Renderer {
    assembly: MessageAssembly,
    fetched: HashMap<String, Vec<u8>>,
    next_index: i64,
}

impl Renderer {
    fn render(&mut self, part: &Part, related: bool) -> Option<Segment> {
        let mime = part.mime_type.as_deref().unwrap_or("").to_lowercase();
        if mime.starts_with("multipart/") {
            let children = part.parts.as_deref().unwrap_or(&[]);
            if mime == "multipart/alternative" {
                let segments = self.render_all(children, false);
                let html = segments.iter().rev().find(|segment| segment.primary).and_then(|segment| segment.html.clone());
                let text = segments.iter().rev().find(|segment| segment.text.is_some()).and_then(|segment| segment.text.clone());
                return (html.is_some() || text.is_some()).then(|| Segment { primary: html.is_some(), html, text });
            }
            let is_related = mime == "multipart/related";
            let segments = self.render_all(children, is_related);
            return combine(segments);
        }
        if mime == "message/rfc822" {
            return self.attached_message(part);
        }
        let disposition = part.header("Content-Disposition").unwrap_or("").to_lowercase();
        let filename = part.filename.as_deref().unwrap_or("");
        if (mime == "text/plain" || mime == "text/html") && !disposition.starts_with("attachment") && filename.is_empty() {
            let charset = text::header_parameter("charset", part.header("Content-Type"));
            let decoded = text::decode_charset(&self.data(part).unwrap_or_default(), charset.as_deref());
            return Some(if mime == "text/html" {
                Segment { html: Some(decoded), text: None, primary: true }
            } else {
                Segment { html: None, text: Some(decoded), primary: false }
            });
        }
        let bytes = self.data(part)?;
        let inline = disposition.starts_with("inline");
        // Images placed inline between bodies (Apple Mail) rather than referenced from the HTML.
        let placed = inline && !related && mime.starts_with("image/");
        let index = self.next_index;
        self.next_index += 1;
        let content_id = part.header("Content-ID").map(String::from).or_else(|| placed.then(|| format!("ms-part-{index}")));
        let attachment = self.assembly.store(
            &bytes,
            index,
            (!filename.is_empty()).then_some(filename),
            &mime,
            content_id.as_deref(),
            inline,
        );
        let cid = attachment.content_id.filter(|cid| placed && text::escape_html(cid) == *cid)?;
        Some(Segment { html: Some(format!("<p><img src=\"cid:{cid}\" style=\"max-width:100%\"></p>")), text: None, primary: false })
    }

    fn render_all(&mut self, parts: &[Part], related: bool) -> Vec<Segment> {
        parts.iter().filter_map(|part| self.render(part, related)).collect()
    }

    /// A forwarded message: rendered below a headline when Gmail spelled out its parts, else
    /// offered as an `.eml` attachment.
    fn attached_message(&mut self, part: &Part) -> Option<Segment> {
        let nested = part.parts.as_deref().unwrap_or(&[]);
        if nested.is_empty() {
            let bytes = self.data(part)?;
            let filename = part.filename.as_deref().filter(|name| !name.is_empty()).unwrap_or("message.eml");
            self.assembly.store(&bytes, self.next_index, Some(filename), "message/rfc822", None, false);
            self.next_index += 1;
            return None;
        }
        let source = std::iter::once(part).chain(nested.iter()).find(|candidate| candidate.header("From").is_some());
        let inner = if nested.len() == 1 { self.render(&nested[0], false) } else { combine(self.render_all(nested, false)) };
        let inner_html = inner.as_ref().filter(|inner| inner.primary).and_then(|inner| inner.html.clone());
        let inner_text = inner
            .as_ref()
            .and_then(|inner| inner.text.clone())
            .or_else(|| inner_html.as_deref().map(text::strip_html))
            .unwrap_or_default();
        let from = text::parse_address_list(source.and_then(|source| source.header("From")).unwrap_or(""));
        let subject = text::decode_header(source.and_then(|source| source.header("Subject")).unwrap_or(""));
        let attached = AttachedMessage::new(from.first(), &subject, inner_html.as_deref(), &inner_text);
        Some(Segment { primary: attached.has_html, html: Some(attached.html), text: Some(attached.text) })
    }

    fn data(&self, part: &Part) -> Option<Vec<u8>> {
        let body = part.body.as_ref()?;
        if let Some(data) = &body.data {
            return text::base64_url_decode(data);
        }
        body.attachment_id.as_ref().and_then(|id| self.fetched.get(id).cloned())
    }
}

/// Siblings shown one after another (multipart/mixed, related and the rest).
fn combine(mut segments: Vec<Segment>) -> Option<Segment> {
    if segments.len() <= 1 {
        return segments.pop();
    }
    let primary = segments.iter().any(|segment| segment.primary);
    let html = primary.then(|| {
        segments
            .iter()
            .map(|segment| segment.html.clone().unwrap_or_else(|| text::plain_to_html(segment.text.as_deref().unwrap_or(""))))
            .collect::<String>()
    });
    let has_text = segments.iter().any(|segment| segment.text.is_some());
    let text = has_text.then(|| {
        segments
            .iter()
            .filter_map(|segment| {
                segment.text.clone().or_else(|| if segment.primary { segment.html.as_deref().map(text::strip_html) } else { None })
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    });
    Some(Segment { html, text, primary })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_batch_answers() {
        let body = "--b\r\nContent-Type: application/http\r\nContent-ID: <response-m1>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"id\":\"x\"}\r\n--b\r\nContent-ID: <response-m0>\r\n\r\nHTTP/1.1 404 Not Found\r\n\r\n{}\r\n--b--\r\n";
        let parts = batch_parts(body.as_bytes(), "b");
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].index, parts[0].status, parts[0].body.trim()), (Some(1), 200, "{\"id\":\"x\"}"));
        assert_eq!((parts[1].index, parts[1].status), (Some(0), 404));
    }

    #[test]
    fn renders_alternatives_inline_images_and_forwards() {
        let directory = std::env::temp_dir().join(format!("mailstream-gmail-render-{}", std::process::id()));
        let encode = |text: &str| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, text);
        let payload: Part = serde_json::from_value(serde_json::json!({
            "mimeType": "multipart/mixed",
            "parts": [
                { "mimeType": "multipart/alternative", "parts": [
                    { "mimeType": "text/plain", "headers": [{ "name": "Content-Type", "value": "text/plain; charset=utf-8" }], "body": { "data": encode("Hello") } },
                    { "mimeType": "text/html", "body": { "data": encode("<p>Hello</p>") } },
                ]},
                { "mimeType": "image/png", "filename": "a.png", "headers": [{ "name": "Content-Disposition", "value": "inline" }], "body": { "data": encode("png") } },
                { "mimeType": "message/rfc822", "parts": [
                    { "mimeType": "text/plain", "headers": [{ "name": "From", "value": "Bob <b@x.com>" }, { "name": "Subject", "value": "Old" }], "body": { "data": encode("Earlier") } },
                ]},
            ],
        }))
        .unwrap();
        let mut renderer = Renderer {
            assembly: MessageAssembly::new(directory.clone(), "mscid://k/".into()).unwrap(),
            fetched: HashMap::new(),
            next_index: 0,
        };
        let body = renderer.render(&payload, false).unwrap();
        assert!(body.primary);
        assert_eq!(
            body.html.as_deref(),
            Some(
                "<p>Hello</p><p><img src=\"cid:ms-part-0\" style=\"max-width:100%\"></p><div class=\"ms-attached-message\"><div class=\"ms-attached-headline\">Attached message from &quot;Bob&quot; &lt;b@x.com&gt;: Old</div><div class=\"ms-plain\">Earlier\n</div></div>"
            )
        );
        assert_eq!(body.text.as_deref(), Some("Hello\n\n---------- Attached message from \"Bob\" <b@x.com>: Old ----------\nEarlier"));
        let content = renderer.assembly.finish(body.html, body.text, vec![], vec![], vec![]);
        assert!(content.html.unwrap().contains("mscid://k/0-a.png"));
        assert!(content.attachments.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
