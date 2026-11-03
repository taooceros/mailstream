//! # Chat HTTP
//!
//! Requests to `chat.googleapis.com` over HTTP/1.1, on TLS connections kept open between
//! requests. A connection holds no credential (the token rides in each request), so every
//! account shares one pool. A request that fails on a reused connection runs once more on a fresh
//! one, unless it may have created something.

use std::{
    io::{Read, Write},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use io_http::{
    coroutine::{HttpCoroutine, HttpCoroutineState},
    rfc9110::{
        request::HttpRequest,
        send::{HttpSendOutput, HttpSendYield},
    },
    rfc9112::send::Http11Send,
};
use pimalaya_stream::stream::{Stream, TlsConnectOptions};
use serde::Deserialize;
use url::Url;

pub const HOST: &str = "chat.googleapis.com";

/// Google closes idle HTTP/1.1 connections after a few minutes; one idle longer than this is
/// replaced instead of reused.
const IDLE_LIMIT: Duration = Duration::from_secs(60);
/// Connections kept idle; the space refresh fans out to at most this many at once.
const IDLE_CAPACITY: usize = 8;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const READ_BUFFER_SIZE: usize = 16 * 1024;

static IDLE: Mutex<Vec<(Stream, Instant)>> = Mutex::new(Vec::new());

/// What a request may do to the server, which decides whether it can be repeated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Reads, or sets state to a value: running it twice is the same as once.
    Idempotent,
    /// Creates something (a message): never repeated, and always on a fresh connection, where a
    /// failure can't be an old socket the server already closed.
    Creates,
}

/// A non-2xx answer, with Google's error message.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Google Chat returned HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Sends one request and returns the 2xx body.
pub fn send(method: &str, url: &Url, token: &str, body: Option<Vec<u8>>, effect: Effect) -> Result<Vec<u8>> {
    let request = request(method, url, token, body);
    if effect == Effect::Idempotent
        && let Some(stream) = checkout()
        && let Ok(result) = exchange(stream, request.clone())
    {
        return result;
    }
    let stream = connect()?;
    exchange(stream, request).context("Google Chat request failed")?
}

fn request(method: &str, url: &Url, token: &str, body: Option<Vec<u8>>) -> HttpRequest {
    let mut request = HttpRequest::get(url.clone())
        .header("Host", HOST)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        request = request.header("Content-Type", "application/json; charset=utf-8").body(body);
    }
    request.method = method.into();
    request
}

fn connect() -> Result<Stream> {
    let stream = Stream::connect_tls(HOST, 443, TlsConnectOptions::default())
        .with_context(|| format!("Connect to {HOST} error"))?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    Ok(stream)
}

fn checkout() -> Option<Stream> {
    let mut idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
    while let Some((stream, since)) = idle.pop() {
        if since.elapsed() < IDLE_LIMIT {
            return Some(stream);
        }
    }
    None
}

fn checkin(stream: Stream) {
    let mut idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
    if idle.len() < IDLE_CAPACITY {
        idle.push((stream, Instant::now()));
    }
}

/// Runs the request over `stream`. The outer error is the transport's (the request may be
/// repeated on another connection); the inner result is the server's answer.
fn exchange(mut stream: Stream, request: HttpRequest) -> Result<Result<Vec<u8>>> {
    let mut send = Http11Send::new(request);
    let mut buf = vec![0u8; READ_BUFFER_SIZE];
    let mut read: Option<usize> = None;
    loop {
        match send.resume(read.take().map(|n| &buf[..n])) {
            HttpCoroutineState::Yielded(HttpSendYield::WantsRead) => read = Some(stream.read(&mut buf)?),
            HttpCoroutineState::Yielded(HttpSendYield::WantsWrite(bytes)) => stream.write_all(&bytes)?,
            HttpCoroutineState::Yielded(HttpSendYield::WantsRedirect { url, .. }) => {
                return Ok(Err(anyhow!("Google Chat redirected to {url}")));
            }
            HttpCoroutineState::Complete(Err(err)) => return Err(err.into()),
            HttpCoroutineState::Complete(Ok(HttpSendOutput { response, keep_alive, .. })) => {
                if keep_alive {
                    checkin(stream);
                }
                if response.status.is_success() {
                    return Ok(Ok(response.body));
                }
                return Ok(Err(api_error(*response.status, &response.body).into()));
            }
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    error: EnvelopeError,
}

#[derive(Deserialize)]
struct EnvelopeError {
    code: Option<u16>,
    message: Option<String>,
}

fn api_error(status: u16, body: &[u8]) -> ApiError {
    match serde_json::from_slice::<Envelope>(body) {
        Ok(Envelope { error }) => ApiError {
            status: error.code.unwrap_or(status),
            message: error.message.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "unknown error".into()),
        },
        Err(_) => ApiError { status, message: String::from_utf8_lossy(body).trim().chars().take(300).collect() },
    }
}

/// The 2xx body as `T`; an empty body reads as `{}`.
pub fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    let body = if body.is_empty() { b"{}".as_slice() } else { body };
    serde_json::from_slice(body).context("Parse Google Chat response error")
}

/// Fails unless `name` is a resource name of the given shape (`spaces/…`, `spaces/…/threads/…`),
/// so a caller's value can't reach another path of the API.
pub fn check_name(name: &str, shape: &[&str]) -> Result<()> {
    let parts: Vec<&str> = name.split('/').collect();
    let ok = parts.len() == shape.len() * 2
        && parts.chunks(2).zip(shape).all(|(pair, kind)| {
            pair[0] == *kind && !pair[1].is_empty() && pair[1].bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        });
    if !ok {
        bail!("Invalid Google Chat resource name: {name}");
    }
    Ok(())
}
