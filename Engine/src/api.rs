//! Authorized HTTPS requests for one account, as the Swift engine's `API`
//! (`Sources/MailstreamKit/Backend/API.swift`) sends them. A 401 drops the access token and
//! refreshes once; rate limiting (429, or Google's 403 rate-limit reasons, which never ran)
//! backs off and repeats, pausing the account's `QuotaBudget` meanwhile; 5xx and dropped
//! connections repeat only requests that may run twice. Every request gives up after `HTTP`'s
//! 30 s, and one that ran out of time is not sent again: a provider that stops answering costs
//! each request one wait, and the next refresh asks again (MS-278). Concurrency is bounded per
//! priority so background prefetch never starves interactive reads, and a request with a `cost`
//! spends it from the account's budget on every attempt once it holds a permit.
use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::tokens::{TokenError, Tokens};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Priority {
    Interactive,
    Background,
}

/// A request a provider refused (or that never got an answer): the HTTP status, 0 without one,
/// and the provider's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    /// A refusal from the error answer `body`: its message, else the start of the body.
    pub fn from_body(status: u16, body: &[u8]) -> Self {
        let message = ErrorBody::parse(body).and_then(|body| body.message()).unwrap_or_else(|| {
            let head = &body[..body.len().min(300)];
            let text = String::from_utf8_lossy(head).trim().to_string();
            if text.is_empty() { format!("The server answered {status}") } else { text }
        });
        Self { status, message }
    }

    /// An answer that came back but does not say what it should; the request itself succeeded.
    pub fn malformed(message: impl Into<String>) -> Self {
        Self::new(200, message)
    }

    pub fn unexpected_answer(host: &str, error: impl fmt::Display) -> Self {
        Self::malformed(format!("Unexpected answer from {host}: {error}"))
    }

    /// The account can't get its mail at all, rather than one request being refused: no answer
    /// (timed out, no connection), the provider failing or limiting it, or its sign-in refused.
    /// A 400 or 404 (a stale cursor, a folder gone) concerns that request alone.
    pub fn cuts_off(&self) -> bool {
        matches!(self.status, 0 | 401 | 403 | 429 | 500..=599)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<TokenError> for ApiError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::SignInAgain(message) => Self::new(401, message),
            TokenError::Failed(message) => Self::new(0, message),
        }
    }
}

/// The error answer Google and Graph share: `{"error": {"message": …}}`, plus Google's
/// `status` and `errors[].reason`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorBody {
    error: Option<ErrorDetail>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ErrorDetail {
    message: Option<String>,
    status: Option<String>,
    errors: Option<Vec<ErrorReason>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ErrorReason {
    reason: Option<String>,
}

impl ErrorBody {
    pub fn parse(body: &[u8]) -> Option<Self> {
        serde_json::from_slice(body).ok()
    }

    pub fn message(&self) -> Option<String> {
        self.error.as_ref()?.message.clone().filter(|message| !message.is_empty())
    }

    /// Google refuses over-quota requests with 403 and one of these reasons; nothing ran.
    fn is_rate_limit(&self) -> bool {
        let Some(error) = &self.error else { return false };
        error.status.as_deref() == Some("RESOURCE_EXHAUSTED")
            || error.errors.iter().flatten().any(|reason| {
                matches!(reason.reason.as_deref(), Some("rateLimitExceeded" | "userRateLimitExceeded" | "quotaExceeded"))
            })
    }
}

/// Why a refused request may be sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// 429, or Google's 403 rate-limit reasons: refused for load, nothing ran.
    Throttled,
    /// 5xx: the provider failed, perhaps after running it.
    ServerError,
}

/// How an answer of `status` with error answer `body` may be repeated; nil when it may not.
pub fn retry(status: u16, body: &[u8]) -> Option<Retry> {
    match status {
        429 => Some(Retry::Throttled),
        403 if ErrorBody::parse(body).is_some_and(|body| body.is_rate_limit()) => Some(Retry::Throttled),
        500..=599 => Some(Retry::ServerError),
        _ => None,
    }
}

/// Attempts at a request the provider keeps throttling before giving up.
const RATE_LIMIT_ATTEMPTS: u32 = 6;

/// One request: method, URL, headers and body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self { method: reqwest::Method::GET, url: url.into(), headers: Vec::new(), body: None }
    }

    pub fn post(url: impl Into<String>, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            headers: vec![("Content-Type".into(), content_type.into())],
            body: Some(body),
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn host(&self) -> String {
        reqwest::Url::parse(&self.url).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_else(|| "the server".into())
    }
}

/// An answer in the 200s.
pub struct Response {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// A client that times out every request after `timeout`, sending `user_agent` when given. Every
/// HTTP client of the engine is made here.
pub(crate) fn client(timeout: Duration, user_agent: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(timeout).pool_max_idle_per_host(8);
    if let Some(user_agent) = user_agent {
        builder = builder.user_agent(user_agent);
    }
    builder.build().expect("the HTTP client builds")
}

/// One HTTP client for every account and for OAuth token requests: connections and TLS sessions
/// are shared, and every request times out after 30 s. Made at the first request, as loading the
/// system's root certificates takes a while (~75 ms), which engine start should not wait for.
pub(crate) static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| client(Duration::from_secs(30), None));

/// `name=value&…` with values percent-encoded (RFC 3986 unreserved characters kept), names as
/// they are (Graph's `$select` keeps its `$`).
pub fn form_encoded<N: fmt::Display, V: AsRef<str>>(fields: impl IntoIterator<Item = (N, V)>) -> String {
    fields.into_iter().map(|(name, value)| format!("{name}={}", percent_encoded(value.as_ref()))).collect::<Vec<_>>().join("&")
}

const UNRESERVED: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Everything but the unreserved characters as `%XX`: safe as a path segment or query value.
pub fn percent_encoded(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, UNRESERVED).to_string()
}

/// `base` + `path`, then `query` form-encoded.
pub fn url(base: &str, path: &str, query: &[(&str, &str)]) -> String {
    if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{}", form_encoded(query.iter().copied())) }
}

/// A page size query value kept within the provider's 1…`max`.
pub fn page_size(requested: usize, max: usize) -> String {
    requested.clamp(1, max).to_string()
}

/// A provider's per-user quota as a token bucket: each request spends its cost, waiting while
/// the bucket is low. Background requests leave `reserve` units for interactive ones. A refusal
/// for load pauses and empties it.
pub struct QuotaBudget {
    capacity: f64,
    per_second: f64,
    reserve: f64,
    state: Mutex<BudgetState>,
}

struct BudgetState {
    available: f64,
    updated: Instant,
    paused_until: Instant,
}

impl QuotaBudget {
    pub fn new(per_minute: u32, burst: u32, reserve: u32) -> Self {
        let now = Instant::now();
        Self {
            capacity: f64::from(burst),
            per_second: f64::from(per_minute) / 60.0,
            reserve: f64::from(reserve),
            state: Mutex::new(BudgetState { available: f64::from(burst), updated: now, paused_until: now }),
        }
    }

    /// Gmail refused this app's mail well under its documented 15,000 units a minute, so the
    /// pace is a quarter of what it took and bursts stay small.
    pub fn gmail() -> Self {
        Self::new(3000, 300, 75)
    }

    /// Nothing is spent for `delay`, and the bucket refills from empty afterwards.
    pub async fn pause(&self, delay: Duration) {
        let mut state = self.state.lock().await;
        let until = Instant::now() + delay;
        if until > state.paused_until {
            state.paused_until = until;
        }
        state.available = 0.0;
        state.updated = state.paused_until;
    }

    pub async fn spend(&self, cost: u32, priority: Priority) {
        let cost = f64::from(cost).min(self.capacity);
        let floor = if priority == Priority::Background { self.reserve } else { 0.0 };
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                if now < state.paused_until {
                    state.paused_until - now
                } else {
                    let elapsed = now.saturating_duration_since(state.updated).as_secs_f64();
                    state.updated = now;
                    state.available = self.capacity.min(state.available + self.per_second * elapsed);
                    if state.available - cost >= floor
                        || (floor > 0.0 && cost > self.capacity - floor && state.available >= self.capacity)
                    {
                        state.available -= cost;
                        return;
                    }
                    let missing = cost + floor - state.available;
                    Duration::from_millis((missing / self.per_second * 1000.0) as u64 + 5)
                }
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// Authorized requests for one account.
pub struct Api {
    pub account: String,
    tokens: Arc<Tokens>,
    budget: Option<QuotaBudget>,
    interactive: Semaphore,
    background: Semaphore,
}

impl Api {
    /// Outlook refuses more than four requests at once per mailbox, so Graph accounts take 3 + 1;
    /// Gmail takes 6 + 3 with its quota budget.
    pub fn gmail(account: &str, tokens: Arc<Tokens>) -> Self {
        Self::new(account, tokens, Some(QuotaBudget::gmail()), 6, 3)
    }

    pub fn graph(account: &str, tokens: Arc<Tokens>) -> Self {
        Self::new(account, tokens, None, 3, 1)
    }

    fn new(account: &str, tokens: Arc<Tokens>, budget: Option<QuotaBudget>, interactive: usize, background: usize) -> Self {
        Self {
            account: account.into(),
            tokens,
            budget,
            interactive: Semaphore::new(interactive),
            background: Semaphore::new(background),
        }
    }

    /// GETs `url` and decodes its JSON answer.
    pub async fn get<T: DeserializeOwned>(&self, url: &str, priority: Priority, cost: u32) -> Result<T, ApiError> {
        self.decode(Request::get(url), priority, cost).await
    }

    pub async fn decode<T: DeserializeOwned>(
        &self,
        request: Request,
        priority: Priority,
        cost: u32,
    ) -> Result<T, ApiError> {
        let host = request.host();
        let response = self.send(request, priority, None, cost).await?;
        serde_json::from_slice(&response.body).map_err(|error| ApiError::unexpected_answer(&host, error))
    }

    /// Sends `request`; any status outside 200–299 fails. `idempotent` defaults to every
    /// method but POST. `cost` is the request's units of the provider's per-user quota.
    pub async fn send(
        &self,
        request: Request,
        priority: Priority,
        idempotent: Option<bool>,
        cost: u32,
    ) -> Result<Response, ApiError> {
        let repeatable = idempotent.unwrap_or(request.method != reqwest::Method::POST);
        let semaphore = if priority == Priority::Interactive { &self.interactive } else { &self.background };
        let _permit = semaphore.acquire().await.map_err(|_| ApiError::new(0, "The request was cancelled."))?;
        let mut refreshed = false;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            if let Some(budget) = &self.budget
                && cost > 0
            {
                budget.spend(cost, priority).await;
            }
            let token = self.tokens.token(&self.account).await.map_err(ApiError::from)?;
            let mut builder = HTTP.request(request.method.clone(), &request.url).bearer_auth(&token);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = &request.body {
                builder = builder.body(body.clone());
            }
            let response = match builder.send().await {
                Ok(response) => response,
                // A timeout already waited the client's whole 30 s: no second wait.
                Err(error)
                    if repeatable && attempt < 3 && !error.is_timeout() && (error.is_connect() || error.is_request()) =>
                {
                    tokio::time::sleep(Duration::from_millis(300 * u64::from(attempt))).await;
                    continue;
                }
                Err(error) => return Err(ApiError::new(0, network_message(&error))),
            };
            let status = response.status().as_u16();
            let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(String::from);
            let content_type = header("content-type");
            let retry_after = header("retry-after").and_then(|value| value.trim().parse::<f64>().ok());
            let body = response.bytes().await.map_err(|error| ApiError::new(0, network_message(&error)))?.to_vec();
            match status {
                200..=299 => return Ok(Response { status, content_type, body }),
                401 if !refreshed => {
                    refreshed = true;
                    self.tokens.reject(&token, &self.account);
                }
                _ => {
                    // A server error repeats only a request that may run twice.
                    let kind = retry(status, &body);
                    let again = (kind == Some(Retry::Throttled) && attempt < RATE_LIMIT_ATTEMPTS)
                        || (kind == Some(Retry::ServerError) && repeatable && attempt < 4);
                    if !again {
                        return Err(ApiError::from_body(status, &body));
                    }
                    // A server error is no sign of load: the budget keeps spending.
                    self.pause(attempt, retry_after, kind == Some(Retry::Throttled)).await;
                }
            }
        }
    }

    /// Sends `items` `size` at a time, one chunk after another: `send` sends one chunk, its items
    /// numbered from 1, as one batch request and answers what each sub-request got. Sub-requests
    /// refused for load, or left unanswered, are sent again after the longest Retry-After among
    /// them (else a backoff) that pauses the account's budget, until none are left; server errors
    /// too when the sub-requests are `repeatable`. `answered` takes every other answer, the
    /// successful ones included; its error fails the whole batch.
    pub async fn batch<Item, Send, Fut>(
        &self,
        items: Vec<Item>,
        size: usize,
        service: &str,
        repeatable: bool,
        send: Send,
        mut answered: impl FnMut(Item, Part) -> Result<(), ApiError>,
    ) -> Result<(), ApiError>
    where
        Item: Clone,
        Send: Fn(Vec<(usize, Item)>) -> Fut,
        Fut: Future<Output = Result<Vec<Part>, ApiError>>,
    {
        for chunk in items.chunks(size) {
            let mut pending: Vec<(usize, Item)> = chunk.iter().cloned().enumerate().map(|(index, item)| (index + 1, item)).collect();
            let mut attempt = 0;
            while !pending.is_empty() {
                attempt += 1;
                let mut retry_after: Option<f64> = None;
                for part in send(pending.clone()).await? {
                    let Some(index) = pending.iter().position(|(number, _)| *number == part.number) else { continue };
                    let again = match retry(part.status, &part.body) {
                        Some(Retry::Throttled) => true,
                        Some(Retry::ServerError) => repeatable,
                        None => false,
                    };
                    if again {
                        if let Some(seconds) = part.retry_after {
                            retry_after = Some(retry_after.unwrap_or(0.0).max(seconds));
                        }
                        continue;
                    }
                    let (_, item) = pending.remove(index);
                    answered(item, part)?;
                }
                if pending.is_empty() {
                    break;
                }
                if attempt >= RATE_LIMIT_ATTEMPTS {
                    return Err(ApiError::new(429, format!("{service} is limiting requests; try again shortly")));
                }
                self.pause(attempt, retry_after, true).await;
            }
        }
        Ok(())
    }

    /// Waits before repeating attempt `attempt`: Retry-After when given (seconds, at most 30),
    /// else exponential from half a second, capped at 8 s. `pausing_budget`: the provider
    /// refused for load, so no request of this account spends quota meanwhile.
    async fn pause(&self, attempt: u32, retry_after: Option<f64>, pausing_budget: bool) {
        let delay = match retry_after {
            Some(seconds) => Duration::from_millis((seconds.clamp(0.0, 30.0) * 1000.0) as u64),
            None => Duration::from_millis((500u64 << (attempt - 1).min(5)).min(8000)),
        };
        if pausing_budget && let Some(budget) = &self.budget {
            budget.pause(delay).await;
        }
        tokio::time::sleep(delay).await;
    }
}

fn network_message(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "The request timed out.".into()
    } else if error.is_connect() {
        "Could not connect to the server.".into()
    } else {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(inner) = source {
            message = format!("{message}: {inner}");
            source = inner.source();
        }
        message
    }
}

// MARK: - Listings

/// One page of a listing: what it holds and where the next page is.
pub trait Listing {
    type Item;
    /// The next page's URL, nil on the last page; `first` is the first page's.
    fn next(&self, first: &str) -> Option<String>;
    fn items(self) -> Vec<Self::Item>;
}

/// A page of a Google listing. Google names a listing's items after what they are, so `items`
/// answers to each of those names; the next page is the first one with the page's token.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GooglePage<T> {
    #[serde(
        alias = "messages",
        alias = "history",
        alias = "spaces",
        alias = "memberships",
        alias = "connections",
        alias = "otherContacts",
        alias = "people"
    )]
    pub items: Option<Vec<T>>,
    pub next_page_token: Option<String>,
}

impl<T> GooglePage<T> {
    /// The next page's token; Chat answers an empty one on the last page.
    pub fn token(&self) -> Option<&str> {
        self.next_page_token.as_deref().filter(|token| !token.is_empty())
    }
}

impl<T> Listing for GooglePage<T> {
    type Item = T;

    fn next(&self, first: &str) -> Option<String> {
        let separator = if first.contains('?') { '&' } else { '?' };
        self.token().map(|token| format!("{first}{separator}pageToken={}", percent_encoded(token)))
    }

    fn items(self) -> Vec<T> {
        self.items.unwrap_or_default()
    }
}

/// A page of a Graph listing: `value`, and the next page's whole URL.
#[derive(Deserialize)]
pub struct GraphPage<T> {
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub next: Option<String>,
}

impl<T> Listing for GraphPage<T> {
    type Item = T;

    fn next(&self, _first: &str) -> Option<String> {
        self.next.clone()
    }

    fn items(self) -> Vec<T> {
        self.value
    }
}

/// Every page of a listing from `first`, at most `limit` pages: `fetch` reads one (nil: the
/// provider refused it, which ends the listing). Nil when the first page was refused.
pub async fn pages<Page, Fetch, Fut>(first: String, limit: usize, fetch: Fetch) -> Result<Option<Vec<Page::Item>>, ApiError>
where
    Page: Listing,
    Fetch: Fn(String) -> Fut,
    Fut: Future<Output = Result<Option<Page>, ApiError>>,
{
    let mut found = Vec::new();
    let mut url = first.clone();
    for index in 0..limit {
        let Some(page) = fetch(url).await? else { return Ok((index > 0).then_some(found)) };
        let following = page.next(&first);
        found.extend(page.items());
        match following {
            Some(following) => url = following,
            None => break,
        }
    }
    Ok(Some(found))
}

// MARK: - Batch answers

/// What one sub-request of a batch got: its number in the batch, its status, how long it asks
/// to wait before the next attempt and its body.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub number: usize,
    pub status: u16,
    pub retry_after: Option<f64>,
    pub body: Vec<u8>,
}

/// Splits a multipart/mixed batch answer (Google's) into its HTTP responses; the number comes
/// from the `Content-ID` (`<response-m3>`) of the sub-request each answers.
pub fn multipart_parts(data: &[u8], boundary: &str) -> Vec<Part> {
    static CONTENT_ID: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)content-id:\s*<[^>]*?(\d+)>").unwrap());
    let text = String::from_utf8_lossy(data).replace("\r\n", "\n");
    let mut parts = Vec::new();
    for chunk in text.split(&format!("--{boundary}")).skip(1) {
        if chunk.starts_with("--") {
            break;
        }
        let Some((headers, response)) = chunk.split_once("\n\n") else { continue };
        let Some(number) = CONTENT_ID.captures(headers).and_then(|found| found[1].parse().ok()) else { continue };
        let status_line = response.split('\n').next().unwrap_or("");
        let Some(status) = status_line.split(' ').filter(|piece| !piece.is_empty()).nth(1).and_then(|code| code.parse().ok())
        else {
            continue;
        };
        let body = response.split_once("\n\n").map_or("", |(_, body)| body);
        parts.push(Part { number, status, retry_after: None, body: body.as_bytes().to_vec() });
    }
    parts
}

/// The sub-requests' answers of a JSON batch answer (Graph's `$batch`): `responses`, each with
/// its sub-request's `id`, `status`, `headers` and `body`.
pub fn json_parts(answer: &serde_json::Value) -> Vec<Part> {
    let Some(responses) = answer["responses"].as_array() else { return Vec::new() };
    responses
        .iter()
        .filter_map(|item| {
            let number = item["id"].as_str()?.parse().ok()?;
            let retry_after = item["headers"].as_object().and_then(|headers| {
                let (_, value) = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("Retry-After"))?;
                value.as_str()?.trim().parse::<f64>().ok()
            });
            let body = if item["body"].is_null() { Vec::new() } else { serde_json::to_vec(&item["body"]).expect("JSON encodes") };
            Some(Part { number, status: item["status"].as_u64().unwrap_or(0) as u16, retry_after, body })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_refusals_like_the_swift_engine() {
        assert_eq!(retry(429, b""), Some(Retry::Throttled));
        let quota = br#"{"error":{"code":403,"message":"Quota","errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        assert_eq!(retry(403, quota), Some(Retry::Throttled));
        assert_eq!(retry(403, br#"{"error":{"message":"Forbidden"}}"#), None);
        assert_eq!(retry(503, b""), Some(Retry::ServerError));
        assert_eq!(ApiError::from_body(404, br#"{"error":{"message":"Not Found"}}"#).message, "Not Found");
        assert_eq!(ApiError::from_body(502, b"").message, "The server answered 502");
        assert_eq!(form_encoded([("$select", "id,name"), ("q", "from:a b")]), "$select=id%2Cname&q=from%3Aa%20b");
        assert_eq!(form_encoded([("scope", "openid https://x/y"), ("a", "b~c-d.e_f*")]), "scope=openid%20https%3A%2F%2Fx%2Fy&a=b~c-d.e_f%2A");
    }

    #[test]
    fn reads_google_and_graph_listings() {
        let page: GooglePage<u32> = serde_json::from_str(r#"{"messages":[1,2],"nextPageToken":"t/1","resultSizeEstimate":2}"#).unwrap();
        assert_eq!(page.next("https://x/m?q=a"), Some("https://x/m?q=a&pageToken=t%2F1".into()));
        assert_eq!(page.items(), [1, 2]);
        let page: GooglePage<u32> = serde_json::from_str(r#"{"otherContacts":[3],"nextPageToken":""}"#).unwrap();
        assert_eq!((page.next("https://x/c"), page.items()), (None, vec![3]));
        // Gmail's history page carries its history id beside the listing.
        #[derive(Deserialize)]
        struct History {
            #[serde(flatten)]
            records: GooglePage<u32>,
            #[serde(rename = "historyId")]
            history_id: String,
        }
        let page: History = serde_json::from_str(r#"{"history":[4],"nextPageToken":"n","historyId":"9"}"#).unwrap();
        assert_eq!((page.history_id.as_str(), page.records.next("https://x/h?a=b")), ("9", Some("https://x/h?a=b&pageToken=n".into())));
        assert_eq!(page.records.items(), [4]);
        let page: GraphPage<u32> = serde_json::from_str(r#"{"value":[5],"@odata.nextLink":"https://g/next"}"#).unwrap();
        assert_eq!((page.next("https://g/first"), page.items()), (Some("https://g/next".into()), vec![5]));
    }

    #[test]
    fn splits_batch_answers() {
        let body = "--b\r\nContent-Type: application/http\r\nContent-ID: <response-m2>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"id\":\"x\"}\r\n--b\r\nContent-ID: <response-m1>\r\n\r\nHTTP/1.1 404 Not Found\r\n\r\n{}\r\n--b\r\n\r\nHTTP/1.1 200 OK\r\n\r\n{}\r\n--b--\r\n";
        let parts = multipart_parts(body.as_bytes(), "b");
        assert_eq!(parts.len(), 2, "the part without a Content-ID is left out");
        assert_eq!((parts[0].number, parts[0].status, String::from_utf8_lossy(&parts[0].body).trim()), (2, 200, "{\"id\":\"x\"}"));
        assert_eq!((parts[1].number, parts[1].status), (1, 404));
        let answer = serde_json::json!({ "responses": [
            { "id": "2", "status": 429, "headers": { "retry-after": "3" } },
            { "id": "1", "status": 200, "body": { "categories": ["A"] } },
        ]});
        let parts = json_parts(&answer);
        assert_eq!((parts[0].number, parts[0].status, parts[0].retry_after, parts[0].body.is_empty()), (2, 429, Some(3.0), true));
        assert_eq!((parts[1].number, parts[1].retry_after, parts[1].body.as_slice()), (1, None, br#"{"categories":["A"]}"#.as_slice()));
    }

    /// An `Api` whose account is never asked for a token: `batch` sends nothing itself.
    fn tokenless_api() -> Api {
        let keychain = crate::keychain::Keychain::new(format!("dev.mailstream.test.api.{}", crate::testkit::unique()));
        let oauth = Arc::new(crate::oauth::OAuth::new(crate::oauth::OAuthConfig::default()));
        let tokens = Tokens::new(keychain, oauth, crate::BuiltInClients::new(), std::collections::HashMap::new());
        Api::graph("me@x.com", Arc::new(tokens))
    }

    #[tokio::test(start_paused = true)]
    async fn batches_send_again_only_what_was_throttled_or_unanswered() {
        let api = tokenless_api();
        let sent: std::sync::Mutex<Vec<Vec<(usize, &str)>>> = Default::default();
        let send = |pending: Vec<(usize, &'static str)>| {
            let attempt = sent.lock().unwrap().len();
            sent.lock().unwrap().push(pending.clone());
            async move {
                let part = |number: usize, status: u16, retry_after: Option<f64>| Part { number, status, retry_after, body: b"{}".to_vec() };
                Ok(match attempt {
                    // a throttled for 2 s, b done, c unanswered.
                    0 => vec![part(1, 429, Some(2.0)), part(2, 200, None)],
                    // b answered again is no longer pending: left alone.
                    1 => vec![part(2, 200, None), part(1, 200, None), part(3, 200, None)],
                    // The next chunk: d fails for good, as servers errors do when not repeatable.
                    _ => vec![part(1, 503, None)],
                })
            }
        };
        let mut answered = Vec::new();
        let start = Instant::now();
        let error = api
            .batch(vec!["a", "b", "c", "d"], 3, "Microsoft Graph", false, send, |item, part| {
                answered.push((item, part.status));
                if part.status == 200 { Ok(()) } else { Err(ApiError::new(part.status, "failed")) }
            })
            .await
            .unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(answered, [("b", 200), ("a", 200), ("c", 200), ("d", 503)]);
        assert_eq!(*sent.lock().unwrap(), [vec![(1, "a"), (2, "b"), (3, "c")], vec![(1, "a"), (3, "c")], vec![(1, "d")]]);
        assert!(start.elapsed() >= Duration::from_secs(2), "waited the Retry-After");
    }

    #[tokio::test(start_paused = true)]
    async fn budgets_spend_and_pause() {
        let budget = QuotaBudget::new(60, 10, 2);
        let start = Instant::now();
        budget.spend(10, Priority::Interactive).await;
        assert!(start.elapsed() < Duration::from_millis(10));
        // Background spending leaves the reserve: 2 units back at 1 a second, plus the cost.
        budget.spend(1, Priority::Background).await;
        assert!(start.elapsed() >= Duration::from_secs(3));
        budget.pause(Duration::from_secs(5)).await;
        let paused = Instant::now();
        budget.spend(1, Priority::Interactive).await;
        assert!(paused.elapsed() >= Duration::from_secs(5));
    }

    /// Google's front end (`server: ESF`) follows full DATA frames with padding-only ones, which
    /// carry no data. h2 0.4.20 took at most 100 of those over a connection's life, then closed
    /// it with `too_many_data_frames`, failing what was in flight ("error decoding response body",
    /// MS-297); the patched copy in `Engine/vendor/h2` lets data earn them back. One connection
    /// here answers 150 requests, each with an empty frame between its data and its end.
    #[tokio::test]
    async fn http2_connections_outlast_empty_data_frames() {
        const BODY: usize = 1024;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            while let Some(Ok((_, mut respond))) = connection.accept().await {
                let mut body = respond.send_response(http::Response::new(()), false).unwrap();
                body.send_data(vec![b'x'; BODY].into(), false).unwrap();
                body.send_data(bytes::Bytes::new(), false).unwrap();
                body.send_data(bytes::Bytes::new(), true).unwrap();
            }
        });
        let client = reqwest::Client::builder().http2_prior_knowledge().build().unwrap();
        for request in 1..=150 {
            let response = client.get(format!("http://{address}/")).send().await.unwrap();
            let body = response.bytes().await.unwrap_or_else(|error| panic!("request {request}: {error:?}"));
            assert_eq!(body.len(), BODY);
        }
    }
}
