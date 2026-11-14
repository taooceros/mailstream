//! Gmail search and header fetch over IMAP (`imap.gmail.com:993`, MS-358). Gmail's REST API
//! charges per-minute quota units for `messages.list` + batched `messages.get`, whereas IMAP is
//! covered by the existing `https://mail.google.com/` scope and avoids those quota units. One
//! long-lived connection per account stays open on `[Gmail]/All Mail` (found via `\All` in
//! `LIST`), reconnecting when dropped and falling back to the REST API if IMAP is disabled or
//! unavailable.
use std::collections::{BTreeSet, HashMap};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_imap::error::Error as ImapError;
use async_imap::imap_proto::{BodyContentCommon, BodyStructure, MailboxDatum, Response, Status};
use async_imap::types::{Fetch, Flag, Name, NameAttribute};
use async_imap::{Authenticator, Client, Session};
use base64::Engine as _;
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
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
}

#[derive(Default)]
struct ImapState {
    session: Option<LiveSession>,
    disabled_until: Option<Instant>,
}

pub(super) struct ImapClient {
    api: Arc<Api>,
    endpoints: Arc<GmailEndpoints>,
    state: Mutex<ImapState>,
}

#[derive(Debug)]
pub(super) enum Fallback {
    Unconfigured,
    Disabled,
    Failed,
}

impl ImapClient {
    pub(super) fn new(api: Arc<Api>, endpoints: Arc<GmailEndpoints>) -> Self {
        Self { api, endpoints, state: Mutex::new(ImapState::default()) }
    }

    pub(super) async fn search(&self, query: &str, limit: usize) -> Result<Vec<ImapMessage>, Fallback> {
        let Some(endpoint) = self.endpoints.imap.as_ref() else {
            return Err(Fallback::Unconfigured);
        };
        let mut state = self.state.lock().await;
        if state.disabled_until.is_some_and(|until| Instant::now() < until) {
            return Err(Fallback::Disabled);
        }

        if let Some(live) = state.session.as_mut() {
            match tokio::time::timeout(SEARCH_TIMEOUT, search_on_session(live, query, limit)).await {
                Ok(Ok(messages)) => return Ok(messages),
                Ok(Err(SearchError::UnsupportedQuery)) => return Err(Fallback::Failed),
                Ok(Err(SearchError::Protocol)) | Err(_) => {
                    state.session = None;
                }
            }
        }

        let live = match tokio::time::timeout(CONNECT_TIMEOUT, connect_and_examine(endpoint, &self.api)).await {
            Ok(Ok(live)) => live,
            Ok(Err(_)) | Err(_) => {
                state.disabled_until = Some(Instant::now() + DISABLED_COOLDOWN);
                return Err(Fallback::Failed);
            }
        };
        state.session = Some(live);

        let live = state.session.as_mut().expect("session just stored");
        match tokio::time::timeout(SEARCH_TIMEOUT, search_on_session(live, query, limit)).await {
            Ok(Ok(messages)) => Ok(messages),
            Ok(Err(SearchError::UnsupportedQuery)) => Err(Fallback::Failed),
            Ok(Err(SearchError::Protocol)) | Err(_) => {
                state.session = None;
                state.disabled_until = Some(Instant::now() + DISABLED_COOLDOWN);
                Err(Fallback::Failed)
            }
        }
    }
}

enum SearchError {
    UnsupportedQuery,
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
    session.examine(&all_mail).await.map_err(|error| format!("IMAP EXAMINE {all_mail}: {error}"))?;
    Ok(LiveSession { session, utf8 })
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

async fn search_on_session(live: &mut LiveSession, query: &str, limit: usize) -> Result<Vec<ImapMessage>, SearchError> {
    if !query.is_ascii() && !live.utf8 {
        return Err(SearchError::UnsupportedQuery);
    }
    let escaped = quote_imap_string(query);
    let mut uids = uid_search_checked(&mut live.session, &format!("X-GM-RAW {escaped}")).await?;
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(limit);

    let uid_set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let fetches: Vec<Fetch> = {
        let stream = live.session.uid_fetch(&uid_set, FETCH_ITEMS).await.map_err(|_| SearchError::Protocol)?;
        stream.try_collect().await.map_err(|_| SearchError::Protocol)?
    };
    if fetches.is_empty() {
        return Err(SearchError::Protocol);
    }

    let mut by_uid: HashMap<u32, ImapMessage> = HashMap::with_capacity(fetches.len());
    let mut ordered = Vec::with_capacity(fetches.len());
    for fetch in fetches {
        let uid = fetch.uid;
        let Some(message) = fetch_to_message(&fetch, live.utf8) else {
            return Err(SearchError::Protocol);
        };
        if let Some(uid) = uid {
            by_uid.insert(uid, message);
        } else {
            ordered.push(message);
        }
    }
    let mut out = Vec::with_capacity(uids.len());
    for uid in uids {
        if let Some(message) = by_uid.remove(&uid) {
            out.push(message);
        }
    }
    out.extend(ordered);
    Ok(out)
}

/// Runs `UID SEARCH <criteria>` and checks the tagged `OK`/`NO`/`BAD` completion rather than
/// relying on `Session::uid_search`, whose `take_while` swallows `NO`, `BAD`, and EOF.
async fn uid_search_checked(session: &mut Session<Transport>, criteria: &str) -> Result<Vec<u32>, SearchError> {
    let tag = session.run_command(&format!("UID SEARCH {criteria}")).await.map_err(|_| SearchError::Protocol)?;
    let mut uids = Vec::new();
    loop {
        let Some(resp) = session.read_response().await.map_err(|_| SearchError::Protocol)? else {
            return Err(SearchError::Protocol);
        };
        match resp.parsed() {
            Response::MailboxData(MailboxDatum::Search(found)) => {
                uids.extend_from_slice(found);
            }
            Response::Done { tag: done_tag, status, .. } if *done_tag == tag => match status {
                Status::Ok => return Ok(uids),
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
            let label = if utf8_enabled { raw.as_ref().to_string() } else { decode_mutf7(raw.as_ref()) };
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
}
