//! A stand-in Gmail IMAP server on loopback for tests (`mail_tests`), parallel to `fake_server.rs`.
//! Supports `XOAUTH2`, `ENABLE UTF8=ACCEPT`, `LIST` with `\All`, `EXAMINE`, `UID SEARCH X-GM-RAW`,
//! and `UID FETCH` with `X-GM-MSGID`, `X-GM-THRID`, `X-GM-LABELS`, `FLAGS`, `INTERNALDATE`,
//! `RFC822.SIZE`, `BODYSTRUCTURE`, and `BODY[HEADER.FIELDS (...)]`.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::gmail::ImapEndpoint;

#[derive(Clone, Debug)]
pub struct FakeImapMessage {
    pub uid: u32,
    pub msg_id: u64,
    pub thr_id: u64,
    pub labels: Vec<String>,
    pub flags: Vec<String>,
    pub internal_date: String,
    pub size: u32,
    pub bodystructure: String,
    pub headers: Vec<(String, String)>,
}

pub type ImapMatcher = Box<dyn Fn(&str, &FakeImapMessage) -> bool + Send + Sync>;

pub struct FakeImapConfig {
    pub all_mail_name: String,
    pub reject_auth: bool,
    pub reject_first_token: Option<String>,
    pub fail_search: bool,
    /// Drops the connection once when a `UID SEARCH` arrives, then succeeds on reconnect.
    pub drop_once_on_search: AtomicBool,
    pub messages: Vec<FakeImapMessage>,
    pub matches: ImapMatcher,
}

impl Default for FakeImapConfig {
    fn default() -> Self {
        Self {
            all_mail_name: "[Gmail]/所有邮件".into(),
            reject_auth: false,
            reject_first_token: None,
            fail_search: false,
            drop_once_on_search: AtomicBool::new(false),
            messages: Vec::new(),
            matches: Box::new(|_, _| true),
        }
    }
}

pub struct FakeImapServer {
    pub endpoint: ImapEndpoint,
    pub connections: AtomicUsize,
    pub searches: AtomicUsize,
    pub fetches: AtomicUsize,
    pub last_query: Mutex<Option<String>>,
    pub last_user: Mutex<Option<String>>,
    pub last_token: Mutex<Option<String>>,
    rejected_once: AtomicBool,
    pub config: FakeImapConfig,
}

impl FakeImapServer {
    pub fn start(config: FakeImapConfig) -> Arc<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = Arc::new(Self {
            endpoint: ImapEndpoint { host: "127.0.0.1".into(), port, tls: false },
            connections: AtomicUsize::new(0),
            searches: AtomicUsize::new(0),
            fetches: AtomicUsize::new(0),
            last_query: Mutex::new(None),
            last_user: Mutex::new(None),
            last_token: Mutex::new(None),
            rejected_once: AtomicBool::new(false),
            config,
        });
        let shared = server.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                while let Ok((stream, _)) = listener.accept().await {
                    let server = shared.clone();
                    tokio::spawn(async move {
                        server.serve(stream).await;
                    });
                }
            });
        });
        server
    }

    async fn serve(&self, stream: TcpStream) {
        self.connections.fetch_add(1, Ordering::SeqCst);
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        if write.write_all(b"* OK Gimap ready for requests from 127.0.0.1\r\n").await.is_err() {
            return;
        }
        let mut line = String::new();
        loop {
            line.clear();
            let Ok(n) = reader.read_line(&mut line).await else { return };
            if n == 0 {
                return;
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let Some((tag, rest)) = trimmed.split_once(' ') else { continue };
            let upper = rest.to_ascii_uppercase();

            if upper.starts_with("AUTHENTICATE XOAUTH2") {
                if write.write_all(b"+ \r\n").await.is_err() {
                    return;
                }
                let mut auth_line = String::new();
                if reader.read_line(&mut auth_line).await.unwrap_or(0) == 0 {
                    return;
                }
                let b64 = auth_line.trim_end_matches(['\r', '\n']);
                let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
                let text = String::from_utf8_lossy(&decoded);
                let (mut user, mut token) = (String::new(), String::new());
                for part in text.split('\x01') {
                    if let Some(u) = part.strip_prefix("user=") {
                        user = u.to_string();
                    } else if let Some(t) = part.strip_prefix("auth=Bearer ") {
                        token = t.to_string();
                    }
                }
                *self.last_user.lock().unwrap() = Some(user.clone());
                *self.last_token.lock().unwrap() = Some(token.clone());

                let reject_bad_token = self
                    .config
                    .reject_first_token
                    .as_deref()
                    .is_some_and(|bad| bad == token && !self.rejected_once.swap(true, Ordering::SeqCst));
                if self.config.reject_auth || reject_bad_token {
                    let err_b64 = base64::engine::general_purpose::STANDARD.encode(r#"{"status":"401","schemes":"Bearer"}"#);
                    let _ = write.write_all(format!("+ {err_b64}\r\n").as_bytes()).await;
                    let mut ack = String::new();
                    let _ = reader.read_line(&mut ack).await;
                    let _ = write.write_all(format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes()).await;
                    continue;
                }
                let _ = write.write_all(format!("{tag} OK {user} authenticated (Success)\r\n").as_bytes()).await;
            } else if upper == "CAPABILITY" {
                let _ = write
                    .write_all(
                        format!(
                            "* CAPABILITY IMAP4rev1 UNSELECT IDLE X-GM-EXT-1 UIDPLUS ENABLE UTF8=ACCEPT SPECIAL-USE\r\n{tag} OK Success\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
            } else if upper == "ENABLE UTF8=ACCEPT" {
                let _ = write.write_all(format!("* ENABLED UTF8=ACCEPT\r\n{tag} OK Success\r\n").as_bytes()).await;
            } else if upper.starts_with("LIST ") {
                let all_name = quote_imap(&self.config.all_mail_name);
                let resp = format!(
                    "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n\
                     * LIST (\\Noselect \\HasChildren) \"/\" \"[Gmail]\"\r\n\
                     * LIST (\\HasNoChildren \\All) \"/\" {all_name}\r\n\
                     * LIST (\\HasNoChildren \\Sent) \"/\" \"[Gmail]/Sent Mail\"\r\n\
                     {tag} OK Success\r\n"
                );
                let _ = write.write_all(resp.as_bytes()).await;
            } else if upper.starts_with("EXAMINE ") {
                let resp = format!(
                    "* FLAGS (\\Answered \\Flagged \\Draft \\Deleted \\Seen)\r\n\
                     * {} EXISTS\r\n\
                     {tag} OK [READ-ONLY] Selected (Success)\r\n",
                    self.config.messages.len()
                );
                let _ = write.write_all(resp.as_bytes()).await;
            } else if let Some(after) = strip_prefix_ignore_case(rest, "UID SEARCH X-GM-RAW ") {
                self.searches.fetch_add(1, Ordering::SeqCst);
                let query = unquote_imap(after);
                *self.last_query.lock().unwrap() = Some(query.clone());
                if self.config.drop_once_on_search.swap(false, Ordering::SeqCst) {
                    return;
                }
                if self.config.fail_search {
                    let _ = write.write_all(format!("{tag} NO [CANNOT] Search failed\r\n").as_bytes()).await;
                    continue;
                }
                let matched: Vec<String> = self
                    .config
                    .messages
                    .iter()
                    .filter(|msg| (self.config.matches)(&query, msg))
                    .map(|msg| msg.uid.to_string())
                    .collect();
                let line = if matched.is_empty() { "* SEARCH\r\n".into() } else { format!("* SEARCH {}\r\n", matched.join(" ")) };
                let _ = write.write_all(format!("{line}{tag} OK SEARCH completed (Success)\r\n").as_bytes()).await;
            } else if let Some(after) = strip_prefix_ignore_case(rest, "UID FETCH ") {
                self.fetches.fetch_add(1, Ordering::SeqCst);
                let uid_list = after.split_whitespace().next().unwrap_or("");
                let wanted: HashSet<u32> = uid_list.split(',').filter_map(|part| part.parse::<u32>().ok()).collect();
                let mut out = Vec::new();
                for (index, msg) in self.config.messages.iter().enumerate() {
                    if !wanted.contains(&msg.uid) {
                        continue;
                    }
                    let labels: Vec<String> = msg
                        .labels
                        .iter()
                        .map(|l| if l.starts_with('\\') { l.clone() } else { quote_imap(l) })
                        .collect();
                    let flags = msg.flags.join(" ");
                    let mut hdr_bytes = String::new();
                    for (k, v) in &msg.headers {
                        hdr_bytes.push_str(&format!("{k}: {v}\r\n"));
                    }
                    hdr_bytes.push_str("\r\n");
                    let head = format!(
                        "* {} FETCH (UID {} X-GM-MSGID {} X-GM-THRID {} X-GM-LABELS ({}) FLAGS ({}) INTERNALDATE \"{}\" RFC822.SIZE {} BODYSTRUCTURE {} BODY[HEADER.FIELDS (From To Subject Message-ID In-Reply-To References)] {{{}}}\r\n",
                        index + 1,
                        msg.uid,
                        msg.msg_id,
                        msg.thr_id,
                        labels.join(" "),
                        flags,
                        msg.internal_date,
                        msg.size,
                        msg.bodystructure,
                        hdr_bytes.len()
                    );
                    out.extend_from_slice(head.as_bytes());
                    out.extend_from_slice(hdr_bytes.as_bytes());
                    out.extend_from_slice(b")\r\n");
                }
                out.extend_from_slice(format!("{tag} OK Success\r\n").as_bytes());
                let _ = write.write_all(&out).await;
            } else if upper == "NOOP" || upper == "LOGOUT" {
                let _ = write.write_all(format!("{tag} OK Success\r\n").as_bytes()).await;
                if upper == "LOGOUT" {
                    return;
                }
            } else {
                let _ = write.write_all(format!("{tag} BAD Unsupported command\r\n").as_bytes()).await;
            }
        }
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
}

fn quote_imap(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote_imap(s: &str) -> String {
    let trimmed = s.trim();
    let Some(inner) = trimmed.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return trimmed.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.next()
        {
            out.push(next);
        } else {
            out.push(ch);
        }
    }
    out
}
