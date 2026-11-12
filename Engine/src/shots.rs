//! The screenshot mode's provider (MS-265; GPUI's `--shots`, `scripts/shots.py`), built only with
//! the `shots` feature, so installed apps carry none of it.
//!
//! The screenshot mode paints synthetic mail from a fresh data folder. [`start`] signs every
//! account of that folder in with fake sign-ins kept in memory ([`SERVICE`], `keychain.rs`) and
//! runs a stand-in provider on the loopback interface (`fake_server`); from then on
//! [`crate::Config::for_this_app`] points the engine's providers and token endpoint there, with no
//! push relay. The stand-in takes mail changes as a provider would (Gmail's message changes and
//! sending, Graph's message changes in `$batch` and `sendMail`), so marking read, starring,
//! archiving and sending succeed and show what follows them. It holds no mail: every read is
//! refused with a 400, which the engine treats as a failed refresh and keeps what the data folder
//! has, so nothing it shows is replaced. The one read it answers is whether conversations the
//! user started were answered (Gmail's threads, Graph's messages of a conversation, in batches):
//! never, so they list in the inbox. Calendar and Chat changes are refused too.
//!
//! [`set_outage`] makes one provider never answer, or fail, for screenshots of an account that
//! can't connect while the others go on (MS-278).
use std::path::Path;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::accounts::Accounts;
use crate::fake_server::{self, FakeRequest, FakeServer, Reply};
use crate::keychain::SignIns;
use crate::{Config, OAuthClient, Provider, push};

/// The sign-ins the engine reads while the screenshot mode runs: in memory only, never a
/// Keychain item or a file.
pub const SERVICE: &str = "dev.mailstream.Mailstream.shots";

static PROVIDER: OnceLock<FakeServer> = OnceLock::new();
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static OUTAGES: Mutex<Vec<(Provider, Outage)>> = Mutex::new(Vec::new());

/// How the stand-in answers a provider's requests (the token endpoint, which every provider
/// shares, answers whatever this says).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outage {
    /// As above.
    Answer,
    /// Never: the engine gives up on each request after its 30 s.
    Hang,
    /// 503, the provider failing.
    Fail,
}

/// From now on, `provider`'s requests get `outage`.
pub fn set_outage(provider: Provider, outage: Outage) {
    let mut outages = OUTAGES.lock().expect("outages lock");
    outages.retain(|(known, _)| *known != provider);
    outages.push((provider, outage));
}

/// The in-memory sign-ins under [`SERVICE`].
pub(crate) static SIGN_INS: LazyLock<Mutex<SignIns>> = LazyLock::new(Default::default);

/// Starts the provider and signs in every account of the accounts.json in `support` (the
/// engine's data folder). Call before the engine starts; a second call changes nothing.
pub fn start(support: &Path) {
    if PROVIDER.get().is_some() {
        return;
    }
    let mut signed_in = SignIns::default();
    for record in Accounts::new(support).list() {
        signed_in.refresh_tokens.insert(record.id.clone(), format!("shots-{}", record.id));
        signed_in.clients.insert(record.id, OAuthClient { id: "shots".into(), secret: None });
    }
    *SIGN_INS.lock().expect("sign-ins lock") = signed_in;
    PROVIDER.set(FakeServer::start(answer)).ok();
}

/// `config` on the provider once [`start`] ran; as it is before.
pub(crate) fn config(mut config: Config) -> Config {
    let Some(server) = PROVIDER.get() else { return config };
    config.keychain_service = SERVICE.into();
    config.oauth.token_endpoint = Some(format!("{}/token", server.base));
    config.endpoints = fake_server::endpoints(&server.base);
    config.push = push::PushConfig::default();
    config
}

/// What the provider answered so far, one line per request (`200 POST /gmail/v1/…`), a Graph
/// `$batch`'s sub-requests indented under it.
pub fn log() -> Vec<String> {
    LOG.lock().expect("log lock").clone()
}

fn answer(request: &FakeRequest) -> Reply {
    let (method, path) = (request.method.as_str(), request.path.as_str());
    let provider = if path.starts_with("/v1.0/") { Provider::Microsoft } else { Provider::Google };
    let outage = OUTAGES.lock().expect("outages lock").iter().find(|(known, _)| *known == provider).map(|(_, outage)| *outage);
    match outage.filter(|_| path != "/token") {
        Some(Outage::Hang) => {
            LOG.lock().expect("log lock").push(format!("--- {method} {path} (never answered)"));
            return Reply::bytes(200, Vec::new()).after(Duration::from_secs(24 * 3600));
        }
        Some(Outage::Fail) => {
            LOG.lock().expect("log lock").push(format!("503 {method} {path}"));
            return Reply::status(503, json!({ "error": { "message": "The service is unavailable." } }));
        }
        Some(Outage::Answer) | None => {}
    }
    let mut lines = Vec::new();
    let reply = match (method, path) {
        ("POST", "/token") => Reply::json(json!({ "access_token": "shots", "expires_in": 3600 })),
        ("POST", "/upload/gmail/v1/users/me/messages/send") => Reply::json(json!({ "id": "shots-sent" })),
        ("POST", _) if path.starts_with("/gmail/v1/users/me/messages/") => Reply::json(json!({})),
        ("POST", "/v1.0/me/sendMail") => Reply::bytes(202, Vec::new()),
        ("POST", "/v1.0/$batch") => graph_batch(request, &mut lines),
        ("POST", "/batch/gmail/v1") => gmail_batch(request, &mut lines),
        _ => refused(),
    };
    let mut log = LOG.lock().expect("log lock");
    log.push(format!("{} {method} {path}", reply.status));
    log.extend(lines);
    reply
}

/// Gmail's batch: a thread holds nothing but the user's mail; any other read is refused.
fn gmail_batch(request: &FakeRequest, lines: &mut Vec<String>) -> Reply {
    let text = String::from_utf8_lossy(&request.body);
    let mut parts = String::new();
    let mut number = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Content-ID: <m") {
            number = rest.trim_end_matches('>').parse::<usize>().ok();
        }
        let (Some(target), Some(number)) = (line.strip_prefix("GET "), number) else { continue };
        let (status, body) =
            if target.contains("/threads/") { ("200 OK", json!({ "messages": [] })) } else { ("400 Bad Request", refusal()) };
        lines.push(format!("  {status} GET {target}"));
        parts.push_str(&format!(
            "--shots\r\nContent-Type: application/http\r\nContent-ID: <response-m{number}>\r\n\r\nHTTP/1.1 {status}\r\nContent-Type: application/json\r\n\r\n{body}\r\n"
        ));
    }
    parts.push_str("--shots--\r\n");
    Reply::bytes(200, parts.into_bytes()).header("Content-Type", "multipart/mixed; boundary=shots")
}

/// Graph's `$batch`: changes to messages succeed, and a conversation's messages are none;
/// anything else is refused.
fn graph_batch(request: &FakeRequest, lines: &mut Vec<String>) -> Reply {
    let Ok(batch) = serde_json::from_slice::<Value>(&request.body) else { return refused() };
    let responses: Vec<Value> = batch["requests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|sub| {
            let (method, url) = (sub["method"].as_str().unwrap_or_default(), sub["url"].as_str().unwrap_or_default());
            let change = method != "GET" && url.starts_with("/me/messages/");
            let conversation = method == "GET" && url.starts_with("/me/messages?");
            let (status, body) = if change {
                (200, json!({}))
            } else if conversation {
                (200, json!({ "value": [] }))
            } else {
                (400, refusal())
            };
            lines.push(format!("  {status} {method} {url}"));
            json!({ "id": sub["id"], "status": status, "body": body })
        })
        .collect();
    Reply::json(json!({ "responses": responses }))
}

fn refusal() -> Value {
    json!({ "error": { "code": 400, "message": "The screenshot mode's provider holds no mail." } })
}

fn refused() -> Reply {
    Reply::status(400, refusal())
}
