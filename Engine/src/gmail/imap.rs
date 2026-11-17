//! Gmail search and header fetch over IMAP (`imap.gmail.com:993`, MS-358). Gmail's REST API
//! charges per-minute quota units for `messages.list` + batched `messages.get`, whereas IMAP is
//! covered by the existing `https://mail.google.com/` scope and avoids those quota units. Each
//! account keeps up to `MAX_SESSIONS` long-lived connections open on `[Gmail]/All Mail` (found via
//! `\All` in `LIST`), so its searches and the listings' header fetches (MS-395) run side by side
//! (MS-371); dropped connections are replaced, and the REST API answers when IMAP is disabled,
//! unavailable or refuses a query.
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use async_imap::error::Error as ImapError;
use async_imap::imap_proto::{BodyContentCommon, BodyStructure, MailboxDatum, Response, Status};
use async_imap::types::{Fetch, Flag, Name, NameAttribute};
use async_imap::{Authenticator, Client, Session};
use base64::Engine as _;
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::ServerName;

use super::{GmailEndpoints, ImapEndpoint};
use crate::api::Api;
use crate::model::{Address, Date, date_from_unix};
use crate::text;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
/// A connection unused this long must answer a `NOOP` within `PROBE_TIMEOUT` before it carries a
/// search: one a sleep or a NAT dropped without a word would otherwise hold the search for all of
/// `SEARCH_TIMEOUT` (MS-371).
const PROBE_AFTER_IDLE: Duration = Duration::from_millis(if cfg!(test) { 500 } else { 15_000 });
const PROBE_TIMEOUT: Duration = Duration::from_millis(if cfg!(test) { 500 } else { 5_000 });
/// Connections an account's searches use at once. Gmail allows 15 per account across every
/// client, so this leaves room for the user's other ones.
const MAX_SESSIONS: usize = 4;
/// UIDs one `UID FETCH` asks for, keeping each command line short when `limit` reaches 500.
const FETCH_CHUNK: usize = 100;
/// How long a quick search's matches wait for the full search of the same query that follows it.
const HELD_FOR: Duration = Duration::from_secs(30);
/// How long IMAP steps aside after a connection, auth, or protocol failure (e.g. Workspace
/// admin disabled IMAP) before trying IMAP again.
const DISABLED_COOLDOWN: Duration = Duration::from_secs(300);

const FETCH_ITEMS: &str = "(UID X-GM-MSGID X-GM-THRID X-GM-LABELS FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (From To Subject Message-ID In-Reply-To References)])";

static TLS_CONFIG: LazyLock<Arc<rustls::ClientConfig>> = LazyLock::new(|| {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(loaded.certs);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
});

#[derive(Debug)]
enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Transport {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

struct XOAuth2 {
    user: String,
    token: String,
    sent: bool,
}

impl XOAuth2 {
    fn new(user: &str, token: &str) -> Self {
        Self { user: user.to_string(), token: token.to_string(), sent: false }
    }
}

impl Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        if self.sent {
            // Gmail sends a base64 error JSON challenge on XOAUTH2 failure and waits for an empty
            // line before returning NO [AUTHENTICATIONFAILED].
            return String::new();
        }
        self.sent = true;
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

#[derive(Clone)]
pub(super) struct ImapMessage {
    pub id: String,
    pub thread: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub date: Option<Date>,
    pub size: i64,
    pub has_attachment: Option<bool>,
    pub system_labels: BTreeSet<String>,
    pub user_labels: Vec<String>,
}

struct LiveSession {
    session: Session<Transport>,
    utf8: bool,
    uid_validity: Option<u32>,
    /// When it last answered, by the wall clock: unlike `Instant`, which stops while the machine
    /// sleeps, it counts the sleep that may have cost the connection.
    used: SystemTime,
}

impl LiveSession {
    fn needs_probe(&self) -> bool {
        needs_probe_since(self.used, SystemTime::now())
    }
}

fn needs_probe_since(used: SystemTime, now: SystemTime) -> bool {
    now.duration_since(used).map_or(true, |idle| idle >= PROBE_AFTER_IDLE)
}

/// What one query matched: every UID, newest first, and the messages of the first `fetched` of
/// them (less those gone before the fetch).
#[derive(Clone)]
struct Matches {
    uid_validity: Option<u32>,
    uids: Vec<u32>,
    messages: HashMap<u32, ImapMessage>,
    unkeyed: Vec<ImapMessage>,
    fetched: usize,
}

impl Matches {
    /// Whether the first `limit` matches are fetched already.
    fn covers(&self, limit: usize) -> bool {
        self.fetched >= limit.min(self.uids.len())
    }

    /// The first `limit` matches' messages, newest first.
    fn into_first(mut self, limit: usize) -> Vec<ImapMessage> {
        let mut out: Vec<ImapMessage> = self.uids.iter().take(limit).filter_map(|uid| self.messages.remove(uid)).collect();
        out.extend(self.unkeyed.into_iter().take(limit.saturating_sub(out.len())));
        out
    }
}

/// A quick search's matches, kept for the full search of the same query that follows it.
struct HeldSearch {
    query: String,
    at: Instant,
    matches: Matches,
}

#[derive(Default)]
struct ImapState {
    /// Open connections no search is using, the most recently used last.
    idle: Vec<LiveSession>,
    disabled_until: Option<Instant>,
    held: Vec<HeldSearch>,
}

pub(super) struct ImapClient {
    api: Arc<Api>,
    endpoints: Arc<GmailEndpoints>,
    /// Locked only to take or put back a connection, never across the network.
    state: Mutex<ImapState>,
    /// One per connection a search may hold: searches beyond `MAX_SESSIONS` wait for one.
    slots: Semaphore,
}

#[derive(Debug)]
pub(super) enum Fallback {
    Unconfigured,
    Disabled,
    Failed,
}

impl ImapClient {
    pub(super) fn new(api: Arc<Api>, endpoints: Arc<GmailEndpoints>) -> Self {
        Self { api, endpoints, state: Mutex::new(ImapState::default()), slots: Semaphore::new(MAX_SESSIONS) }
    }

    /// The newest `limit` messages in All Mail matching `query`, newest first. A `quick` search
    /// keeps its matches for the full search of the same query that follows it (MS-371), which
    /// then fetches only the messages the quick one left out instead of searching again.
    pub(super) async fn search(&self, query: &str, limit: usize, quick: bool) -> Result<Vec<ImapMessage>, Fallback> {
        self.ready()?;
        let known = if quick { None } else { self.take_held(query) };
        let known = match known {
            Some(known) if known.covers(limit) => return Ok(known.into_first(limit)),
            other => other,
        };
        let known = known.as_ref();
        let found = self
            .run(|mut live| async move {
                let found = search_on_session(&mut live, query, limit, known).await;
                (live, found)
            })
            .await?;
        if quick {
            self.hold(query, found.clone());
        }
        Ok(found.into_first(limit))
    }

    /// The messages among `ids` (Gmail message ids, which IMAP calls `X-GM-MSGID`) that All Mail
    /// holds, in no particular order, each with the inbox category Gmail put it in among its
    /// labels (MS-395). Messages in Spam or Trash, which All Mail leaves out, and messages gone
    /// are missing from the answer.
    pub(super) async fn messages(&self, ids: &[u64]) -> Result<Vec<ImapMessage>, Fallback> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.run(|mut live| async move {
            let found = messages_on_session(&mut live, ids).await;
            (live, found)
        })
        .await
    }

    /// The endpoint, unless IMAP is off for this setup or stepping aside after a failure.
    fn ready(&self) -> Result<&ImapEndpoint, Fallback> {
        let Some(endpoint) = self.endpoints.imap.as_ref() else {
            return Err(Fallback::Unconfigured);
        };
        if self.disabled() {
            return Err(Fallback::Disabled);
        }
        Ok(endpoint)
    }

    /// Runs `work` on the most recently used open connection, or on a new one when none is
    /// left or the idle one failed, holding one of the account's `MAX_SESSIONS` slots meanwhile.
    /// `work` takes the connection and hands it back with its answer.
    async fn run<T, F>(&self, work: impl Fn(LiveSession) -> F) -> Result<T, Fallback>
    where
        F: Future<Output = (LiveSession, Result<T, SearchError>)>,
    {
        let endpoint = self.ready()?;
        let _slot = self.slots.acquire().await.expect("IMAP slots are never closed");
        // Another search may have failed while this one waited for a slot.
        if self.disabled() {
            return Err(Fallback::Disabled);
        }

        if let Some(live) = self.idle_session().await {
            match tokio::time::timeout(SEARCH_TIMEOUT, work(live)).await {
                Ok((live, Ok(found))) => {
                    self.put_back(live);
                    return Ok(found);
                }
                Ok((live, Err(SearchError::Query))) => {
                    self.put_back(live);
                    return Err(Fallback::Failed);
                }
                Ok((_, Err(SearchError::Protocol))) => {}
                // It answered a moment ago, so the server is slow rather than gone: a new
                // connection would only make the fallback wait longer.
                Err(_) => return Err(Fallback::Failed),
            }
        }

        let live = match tokio::time::timeout(CONNECT_TIMEOUT, connect_and_examine(endpoint, &self.api)).await {
            Ok(Ok(live)) => live,
            Ok(Err(_)) | Err(_) => {
                self.disable();
                return Err(Fallback::Failed);
            }
        };
        match tokio::time::timeout(SEARCH_TIMEOUT, work(live)).await {
            Ok((live, Ok(found))) => {
                self.put_back(live);
                Ok(found)
            }
            Ok((live, Err(SearchError::Query))) => {
                self.put_back(live);
                Err(Fallback::Failed)
            }
            Ok((_, Err(SearchError::Protocol))) | Err(_) => {
                self.disable();
                Err(Fallback::Failed)
            }
        }
    }

    fn disabled(&self) -> bool {
        self.state.lock().expect("IMAP state").disabled_until.is_some_and(|until| Instant::now() < until)
    }

    fn disable(&self) {
        self.state.lock().expect("IMAP state").disabled_until = Some(Instant::now() + DISABLED_COOLDOWN);
    }

    /// The held quick search's matches when they are `query`'s and still fresh.
    fn take_held(&self, query: &str) -> Option<Matches> {
        let mut state = self.state.lock().expect("IMAP state");
        state.held.retain(|entry| entry.at.elapsed() < HELD_FOR);
        let pos = state.held.iter().rposition(|entry| entry.query == query)?;
        Some(state.held.remove(pos).matches)
    }

    /// The most recently used open connection. One unused for `PROBE_AFTER_IDLE` that doesn't
    /// answer `NOOP` lost the network, and so did the others idle as long: they all go.
    /// `run_command_and_check_ok` is used rather than `Session::noop`, whose `take_while`
    /// swallows `NO`, `BAD` and EOF.
    async fn idle_session(&self) -> Option<LiveSession> {
        loop {
            let mut live = self.state.lock().expect("IMAP state").idle.pop()?;
            if !live.needs_probe() {
                return Some(live);
            }
            if let Ok(Ok(())) = tokio::time::timeout(PROBE_TIMEOUT, live.session.run_command_and_check_ok("NOOP")).await {
                live.used = SystemTime::now();
                return Some(live);
            }
            self.state.lock().expect("IMAP state").idle.retain(|other| !other.needs_probe());
        }
    }

    fn put_back(&self, mut live: LiveSession) {
        live.used = SystemTime::now();
        self.state.lock().expect("IMAP state").idle.push(live);
    }

    /// Holds a quick search's matches for the full search of the same query that follows it.
    fn hold(&self, query: &str, found: Matches) {
        let mut state = self.state.lock().expect("IMAP state");
        state.held.retain(|entry| entry.at.elapsed() < HELD_FOR && entry.query != query);
        if state.held.len() >= MAX_SESSIONS {
            state.held.remove(0);
        }
        state.held.push(HeldSearch { query: query.to_string(), at: Instant::now(), matches: found });
    }
}

enum SearchError {
    /// IMAP can't answer this query (it needs UTF-8 the server didn't enable, or the server
    /// refused it with `NO`/`BAD`); the connection is fine and the REST API answers instead.
    Query,
    /// The connection failed or answered something unreadable.
    Protocol,
}

async fn open_client(endpoint: &ImapEndpoint) -> Result<Client<Transport>, String> {
    let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .await
        .map_err(|error| format!("IMAP connect: {error}"))?;
    let transport = if endpoint.tls {
        let domain = ServerName::try_from(endpoint.host.clone()).map_err(|error| format!("IMAP TLS name: {error}"))?;
        let connector = TlsConnector::from(TLS_CONFIG.clone());
        let tls = connector.connect(domain, tcp).await.map_err(|error| format!("IMAP TLS handshake: {error}"))?;
        Transport::Tls(Box::new(tls))
    } else {
        Transport::Plain(tcp)
    };
    let mut client = Client::new(transport);
    match client.read_response().await {
        Ok(Some(_)) => Ok(client),
        Ok(None) => Err("IMAP server closed before greeting".into()),
        Err(error) => Err(format!("IMAP greeting: {error}")),
    }
}

async fn connect_and_examine(endpoint: &ImapEndpoint, api: &Api) -> Result<LiveSession, String> {
    let token = api.token().await.map_err(|error| error.message)?;
    let client = open_client(endpoint).await?;
    let mut session = match client.authenticate("XOAUTH2", XOAuth2::new(&api.account, &token)).await {
        Ok(session) => session,
        Err((ImapError::No(reason), _)) => {
            api.reject_token(&token);
            let fresh = api.token().await.map_err(|error| error.message)?;
            let retry_client = open_client(endpoint).await?;
            retry_client
                .authenticate("XOAUTH2", XOAuth2::new(&api.account, &fresh))
                .await
                .map_err(|(error, _)| format!("IMAP auth ({reason}): {error}"))?
        }
        Err((error, _)) => return Err(format!("IMAP auth: {error}")),
    };

    let utf8 = match session.capabilities().await {
        Ok(caps) if caps.has_str("UTF8=ACCEPT") => session.run_command_and_check_ok("ENABLE UTF8=ACCEPT").await.is_ok(),
        _ => false,
    };

    let all_mail = find_all_mail(&mut session).await?;
    let mailbox = session.examine(&all_mail).await.map_err(|error| format!("IMAP EXAMINE {all_mail}: {error}"))?;
    Ok(LiveSession { session, utf8, uid_validity: mailbox.uid_validity, used: SystemTime::now() })
}

async fn find_all_mail(session: &mut Session<Transport>) -> Result<String, String> {
    let names: Vec<Name> = session
        .list(Some(""), Some("*"))
        .await
        .map_err(|error| format!("IMAP LIST: {error}"))?
        .try_collect()
        .await
        .map_err(|error| format!("IMAP LIST stream: {error}"))?;

    let is_selectable = |name: &Name| !name.attributes().iter().any(|attr| matches!(attr, NameAttribute::NoSelect));
    let has_all = |name: &Name| {
        name.attributes().iter().any(|attr| match attr {
            NameAttribute::All => true,
            NameAttribute::Extension(ext) => ext.eq_ignore_ascii_case("\\All"),
            _ => false,
        })
    };
    if let Some(found) = names.iter().find(|name| is_selectable(name) && has_all(name)) {
        return Ok(found.name().to_string());
    }
    if let Some(fallback) = names
        .iter()
        .find(|name| is_selectable(name) && name.name().eq_ignore_ascii_case("[Gmail]/All Mail"))
    {
        return Ok(fallback.name().to_string());
    }
    Err("IMAP LIST returned no \\All mailbox".into())
}

async fn search_on_session(
    live: &mut LiveSession,
    query: &str,
    limit: usize,
    known: Option<&Matches>,
) -> Result<Matches, SearchError> {
    let mut matches = match known.filter(|known| known.uid_validity == live.uid_validity) {
        Some(known) => known.clone(),
        None => {
            if !query.is_ascii() && !live.utf8 {
                return Err(SearchError::Query);
            }
            let escaped = quote_imap_string(query);
            let mut uids = uid_search_checked(&mut live.session, &format!("X-GM-RAW {escaped}")).await?;
            uids.sort_unstable_by(|a, b| b.cmp(a));
            Matches {
                uid_validity: live.uid_validity,
                uids,
                messages: HashMap::new(),
                unkeyed: Vec::new(),
                fetched: 0,
            }
        }
    };

    let target = limit.min(matches.uids.len());
    if target > matches.fetched {
        for (uid, message) in fetch_uids(live, &matches.uids[matches.fetched..target]).await? {
            if let Some(uid) = uid {
                matches.messages.insert(uid, message);
            } else {
                matches.unkeyed.push(message);
            }
        }
        matches.fetched = target;
    }
    Ok(matches)
}

/// `ImapClient::messages` on one connection: the UIDs of `ids` in one `UID SEARCH` of `OR`-ed
/// `X-GM-MSGID`s, their messages, then one search per inbox category among those UIDs, since
/// `X-GM-LABELS` leaves the categories out.
async fn messages_on_session(live: &mut LiveSession, ids: &[u64]) -> Result<Vec<ImapMessage>, SearchError> {
    let mut criteria = "OR ".repeat(ids.len().saturating_sub(1));
    for id in ids {
        let _ = write!(criteria, "X-GM-MSGID {id} ");
    }
    let uids = uid_search_checked(&mut live.session, criteria.trim_end()).await?;
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = HashMap::with_capacity(uids.len());
    for (uid, message) in fetch_uids(live, &uids).await? {
        found.insert(uid.ok_or(SearchError::Protocol)?, message);
    }
    let uids = uid_set(&uids);
    for (label, _, search) in super::CATEGORIES {
        for uid in uid_search_checked(&mut live.session, &format!("UID {uids} X-GM-RAW \"{search}\"")).await? {
            if let Some(message) = found.get_mut(&uid) {
                message.system_labels.insert(label.into());
            }
        }
    }
    Ok(found.into_values().collect())
}

/// The messages of `uids`, `FETCH_CHUNK` per `UID FETCH`, with the UID each came back under.
async fn fetch_uids(live: &mut LiveSession, uids: &[u32]) -> Result<Vec<(Option<u32>, ImapMessage)>, SearchError> {
    let mut messages = Vec::with_capacity(uids.len());
    for chunk in uids.chunks(FETCH_CHUNK) {
        let fetches: Vec<Fetch> = {
            let stream = live.session.uid_fetch(uid_set(chunk), FETCH_ITEMS).await.map_err(|_| SearchError::Protocol)?;
            stream.try_collect().await.map_err(|_| SearchError::Protocol)?
        };
        if fetches.is_empty() {
            return Err(SearchError::Protocol);
        }
        for fetch in fetches {
            let message = fetch_to_message(&fetch, live.utf8).ok_or(SearchError::Protocol)?;
            messages.push((fetch.uid, message));
        }
    }
    Ok(messages)
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// Runs `UID SEARCH <criteria>` and checks the tagged `OK`/`NO`/`BAD` completion rather than
/// relying on `Session::uid_search`, whose `take_while` swallows `NO`, `BAD`, and EOF.
async fn uid_search_checked(session: &mut Session<Transport>, criteria: &str) -> Result<Vec<u32>, SearchError> {
    let tag = session.run_command(&format!("UID SEARCH {criteria}")).await.map_err(|_| SearchError::Protocol)?;
    let mut uids = Vec::new();
    let mut bye = false;
    loop {
        let Some(resp) = session.read_response().await.map_err(|_| SearchError::Protocol)? else {
            return Err(SearchError::Protocol);
        };
        match resp.parsed() {
            Response::MailboxData(MailboxDatum::Search(found)) => {
                uids.extend_from_slice(found);
            }
            Response::Data { status: Status::Bye, .. } => {
                bye = true;
            }
            Response::Done { tag: done_tag, status, .. } if *done_tag == tag => match status {
                Status::Ok if !bye => return Ok(uids),
                Status::No | Status::Bad if !bye => return Err(SearchError::Query),
                _ => return Err(SearchError::Protocol),
            },
            _ => {}
        }
    }
}

fn quote_imap_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\r' | '\n' | '\0' => out.push(' '),
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn fetch_to_message(fetch: &Fetch, utf8_enabled: bool) -> Option<ImapMessage> {
    let msg_id = *fetch.gmail_msg_id()?;
    let id = format!("{msg_id:x}");
    let thread = fetch.gmail_thr_id().map(|thr| format!("{thr:x}"));

    let mut system_labels = BTreeSet::new();
    let mut user_labels = Vec::new();

    let mut seen = false;
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => seen = true,
            Flag::Flagged => {
                system_labels.insert("STARRED".into());
            }
            Flag::Draft => {
                system_labels.insert("DRAFT".into());
            }
            _ => {}
        }
    }
    if !seen {
        system_labels.insert("UNREAD".into());
    }

    if let Some(raw_labels) = fetch.gmail_labels() {
        for raw in raw_labels {
            let label = gmail_label(raw, utf8_enabled);
            if let Some(mapped) = map_gmail_system_label(&label) {
                system_labels.insert(mapped.into());
            } else if !label.starts_with('\\') && !label.is_empty() {
                user_labels.push(label);
            }
        }
    }

    let headers = parse_headers(fetch.header().unwrap_or(&[]));
    let header = |name: &str| headers.get(name).map(String::as_str);

    let in_reply_to = text::parse_message_id_list(header("in-reply-to"));
    let in_reply_to = if in_reply_to.is_empty() {
        text::parse_message_id_list(header("references")).into_iter().last().into_iter().collect()
    } else {
        in_reply_to
    };

    let date = fetch
        .internal_date()
        .map(|dt| date_from_unix(dt.timestamp_millis() as f64 / 1000.0));
    let has_attachment = fetch.bodystructure().map(bodystructure_has_attachment);

    Some(ImapMessage {
        id,
        thread,
        message_id: header("message-id").map(text::normalize_message_id).filter(|id| !id.is_empty()),
        in_reply_to,
        subject: text::decode_header(header("subject").unwrap_or("")),
        from: text::parse_address_list(header("from").unwrap_or("")),
        to: text::parse_address_list(header("to").unwrap_or("")),
        date,
        size: i64::from(fetch.size.unwrap_or(0)),
        has_attachment,
        system_labels,
        user_labels,
    })
}

/// A label as `X-GM-LABELS` lists it, as text. Gmail sends system labels and names with spaces
/// or specials as quoted strings (`"\\Inbox"`, `"Bills \"Q4\""`), and `imap-proto` hands quoted
/// strings over with their escapes still in (MS-366), so `\\` and `\"` are undone here. Atoms
/// (`\Inbox`, `Receipts`) can't hold either sequence, so the same pass leaves them as they are.
/// Without `UTF8=ACCEPT` the name is then modified UTF-7.
fn gmail_label(raw: &str, utf8_enabled: bool) -> String {
    let mut label = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        match (ch, chars.clone().next()) {
            ('\\', Some(next @ ('\\' | '"'))) => {
                label.push(next);
                chars.next();
            }
            _ => label.push(ch),
        }
    }
    if utf8_enabled { label } else { decode_mutf7(&label) }
}

fn map_gmail_system_label(raw: &str) -> Option<&'static str> {
    if !raw.starts_with('\\') {
        return None;
    }
    match raw.to_ascii_lowercase().as_str() {
        "\\inbox" => Some("INBOX"),
        "\\sent" => Some("SENT"),
        "\\draft" | "\\drafts" => Some("DRAFT"),
        "\\trash" => Some("TRASH"),
        "\\junk" | "\\spam" => Some("SPAM"),
        "\\starred" | "\\flagged" => Some("STARRED"),
        "\\important" => Some("IMPORTANT"),
        _ => None,
    }
}

fn bodystructure_has_attachment(bs: &BodyStructure<'_>) -> bool {
    fn part_has_file(common: &BodyContentCommon<'_>) -> bool {
        let disp_file = common.disposition.as_ref().is_some_and(|disp| {
            disp.ty.eq_ignore_ascii_case("attachment")
                || disp.params.iter().flatten().any(|(k, v)| {
                    (k.eq_ignore_ascii_case("filename") || k.eq_ignore_ascii_case("filename*")) && !v.is_empty()
                })
        });
        let type_name = common.ty.params.iter().flatten().any(|(k, v)| {
            (k.eq_ignore_ascii_case("name") || k.eq_ignore_ascii_case("name*")) && !v.is_empty()
        });
        disp_file || type_name
    }

    fn walk(bs: &BodyStructure<'_>, in_related: bool) -> bool {
        match bs {
            BodyStructure::Basic { common, .. } | BodyStructure::Text { common, .. } => {
                let is_inline = common.disposition.as_ref().is_some_and(|disp| disp.ty.eq_ignore_ascii_case("inline"));
                if in_related && is_inline {
                    return false;
                }
                part_has_file(common)
            }
            BodyStructure::Message { common, body, .. } => part_has_file(common) || walk(body, false),
            BodyStructure::Multipart { common, bodies, .. } => {
                let related = in_related || common.ty.subtype.eq_ignore_ascii_case("related");
                bodies.iter().any(|child| walk(child, related))
            }
        }
    }

    match bs {
        BodyStructure::Multipart { common, bodies, .. } => {
            let related = common.ty.subtype.eq_ignore_ascii_case("related");
            bodies.iter().any(|child| walk(child, related))
        }
        BodyStructure::Basic { common, .. } | BodyStructure::Text { common, .. } => part_has_file(common),
        BodyStructure::Message { .. } => true,
    }
}

fn parse_headers(raw: &[u8]) -> HashMap<String, String> {
    let text = text::decode_charset(raw, None);
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(key) = current.as_deref()
                && let Some(val) = headers.get_mut(key)
            {
                val.push(' ');
                val.push_str(line.trim_start_matches([' ', '\t']));
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let value = value.trim_start_matches([' ', '\t']).trim_end().to_string();
            headers.entry(key.clone()).or_insert(value);
            current = Some(key);
        }
    }
    headers
}

/// Decodes IMAP modified UTF-7 (RFC 3501 §5.1.3) for servers/sessions without `UTF8=ACCEPT`.
fn decode_mutf7(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(dash) = after.find('-') else {
            out.push('&');
            out.push_str(after);
            return out;
        };
        let encoded = &after[..dash];
        if encoded.is_empty() {
            out.push('&');
        } else {
            let b64 = encoded.replace(',', "/");
            if let Some(bytes) = decode_b64_unpadded(&b64) {
                let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&pair| u16::from_be_bytes(pair)).collect();
                out.push_str(&String::from_utf16_lossy(&units));
            } else {
                out.push('&');
                out.push_str(encoded);
                out.push('-');
            }
        }
        rest = &after[dash + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_b64_unpadded(b64: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(b64)
        .ok()
        .or_else(|| base64::engine::general_purpose::STANDARD.decode(b64).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_headers_folded_and_mutf7() {
        let raw = b"From: Ann <ann@x.com>\r\nSubject: =?UTF-8?B?5rWL6K+V?=\r\n  continued\r\nMessage-ID: <M1@X.COM>\r\n\r\n";
        let headers = parse_headers(raw);
        assert_eq!(headers.get("from").map(String::as_str), Some("Ann <ann@x.com>"));
        assert_eq!(headers.get("subject").map(String::as_str), Some("=?UTF-8?B?5rWL6K+V?= continued"));
        assert_eq!(text::decode_header(headers.get("subject").unwrap()), "测试 continued");
        assert_eq!(text::normalize_message_id(headers.get("message-id").unwrap()), "m1@x.com");

        assert_eq!(decode_mutf7("Receipts &- Bills"), "Receipts & Bills");
        assert_eq!(decode_mutf7("A&-B"), "A&B");
        assert_eq!(decode_mutf7("&ZeVnLIqe-"), "日本語");
    }

    #[test]
    fn quotes_imap_search_strings() {
        assert_eq!(quote_imap_string(r#"from:"a" \b"#), r#""from:\"a\" \\b""#);
        assert_eq!(quote_imap_string("line1\r\nline2"), "\"line1  line2\"");
    }

    #[test]
    fn reads_quoted_system_and_user_labels_as_gmail_sends_them() {
        use async_imap::imap_proto::AttributeValue;
        // Real Gmail quotes system labels and escapes their backslash (MS-366).
        let raw = br#"* 1 FETCH (X-GM-LABELS ("\\Inbox" "\\Important" \Sent Receipts "Bills \"Q4\"" "C:\\Temp" "&ZeVnLIqe-"))"#;
        let line = [&raw[..], b"\r\n"].concat();
        let (_, Response::Fetch(_, attrs)) = Response::parse(&line).unwrap() else { panic!("not a FETCH") };
        let [AttributeValue::GmailLabels(labels)] = attrs.as_slice() else { panic!("no X-GM-LABELS") };
        let labels: Vec<String> = labels.iter().map(|raw| gmail_label(raw, false)).collect();
        assert_eq!(labels, ["\\Inbox", "\\Important", "\\Sent", "Receipts", "Bills \"Q4\"", "C:\\Temp", "日本語"]);
        let system: Vec<_> = labels.iter().filter_map(|label| map_gmail_system_label(label)).collect();
        assert_eq!(system, ["INBOX", "IMPORTANT", "SENT"]);
        assert_eq!(gmail_label("&ZeVnLIqe-", true), "&ZeVnLIqe-");
    }

    #[test]
    fn probes_after_idle_or_clock_jump() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(!needs_probe_since(base, base + PROBE_AFTER_IDLE - Duration::from_millis(1)));
        assert!(needs_probe_since(base, base + PROBE_AFTER_IDLE));
        assert!(needs_probe_since(base, base - Duration::from_secs(1)));
    }
}
