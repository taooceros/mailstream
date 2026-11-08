//! Authorized HTTPS requests for one account, as the Swift engine's `API`
//! (`Sources/MailstreamKit/Backend/API.swift`) sends them. A 401 drops the access token and
//! refreshes once; rate limiting (429, or Google's 403 rate-limit reasons, which never ran)
//! backs off and repeats, pausing the account's `QuotaBudget` meanwhile; 5xx and dropped
//! connections repeat only requests that may run twice. Concurrency is bounded per priority so
//! background prefetch never starves interactive reads, and a request with a `cost` spends it
//! from the account's budget on every attempt once it holds a permit.
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

/// One HTTP client for every account: connections and TLS sessions are shared. Made at the
/// first request, as loading the system's root certificates takes a while.
static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(8)
        .build()
        .expect("the HTTP client builds")
});

/// `name=value&…` with values percent-encoded (RFC 3986 unreserved characters kept), names as
/// they are (Graph's `$select` keeps its `$`).
pub fn form_encoded(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(name, value)| format!("{name}={}", percent_encoded(value))).collect::<Vec<_>>().join("&")
}

const UNRESERVED: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Everything but the unreserved characters as `%XX`: safe as a path segment or query value.
pub fn percent_encoded(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, UNRESERVED).to_string()
}

/// `base` + `path`, then `query` form-encoded.
pub fn url(base: &str, path: &str, query: &[(&str, &str)]) -> String {
    if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{}", form_encoded(query)) }
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
                Err(error) if repeatable && attempt < 3 && (error.is_timeout() || error.is_connect() || error.is_request()) => {
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

    /// Sends `items` `size` at a time, one chunk after another, through `send`, which answers
    /// the ones to send again (throttled or unanswered) and the longest Retry-After among them.
    /// Those repeat after a backoff that pauses the account's budget, until none are left.
    pub async fn batched<Item, Send, Fut>(&self, items: Vec<Item>, size: usize, service: &str, send: Send) -> Result<(), ApiError>
    where
        Item: Clone,
        Send: Fn(Vec<Item>) -> Fut,
        Fut: Future<Output = Result<(Vec<Item>, Option<f64>), ApiError>>,
    {
        for chunk in items.chunks(size) {
            let mut pending = chunk.to_vec();
            let mut attempt = 0;
            while !pending.is_empty() {
                attempt += 1;
                let (again, retry_after) = send(pending).await?;
                if again.is_empty() {
                    break;
                }
                if attempt >= RATE_LIMIT_ATTEMPTS {
                    return Err(ApiError::new(429, format!("{service} is limiting requests; try again shortly")));
                }
                self.pause(attempt, retry_after, true).await;
                pending = again;
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

/// Every page of a listing from `first`, at most `limit` pages: `fetch` reads one (nil: the
/// provider refused it, which ends the listing), `next` names the one after it, `items` takes
/// what it holds. Nil when the first page was refused.
pub async fn pages<Page, Item, Fetch, Fut>(
    first: String,
    limit: usize,
    fetch: Fetch,
    next: impl Fn(&Page) -> Option<String>,
    items: impl Fn(Page) -> Vec<Item>,
) -> Result<Option<Vec<Item>>, ApiError>
where
    Fetch: Fn(String) -> Fut,
    Fut: Future<Output = Result<Option<Page>, ApiError>>,
{
    let mut found = Vec::new();
    let mut url = first;
    for index in 0..limit {
        let Some(page) = fetch(url).await? else { return Ok((index > 0).then_some(found)) };
        let following = next(&page);
        found.extend(items(page));
        match following {
            Some(following) => url = following,
            None => break,
        }
    }
    Ok(Some(found))
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
        assert_eq!(form_encoded(&[("$select", "id,name"), ("q", "from:a b")]), "$select=id%2Cname&q=from%3Aa%20b");
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
}
