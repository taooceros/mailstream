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
//! never, so they list in the inbox. Calendar and Chat changes are refused too. Searches never
//! reach it: the engine finds the mail its data folder lists instead ([`matches`], MS-331).
//!
//! Sign-ins and connecting a service (`accounts.connect`, MS-306) go through [`consent`] instead
//! of a browser: it allows everything asked, as a person ticking every box would, and redirects
//! back at once, so the screens after a sign-in can be shot. No browser opens.
//!
//! [`set_outage`] makes one provider never answer, or fail, for screenshots of an account that
//! can't connect while the others go on (MS-278).
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::accounts::Accounts;
use crate::fake_server::{self, FakeRequest, FakeServer, Reply};
use crate::keychain::SignIns;
use crate::model::Envelope;
use crate::{Config, OAuthClient, Provider, push};

/// The sign-ins the engine reads while the screenshot mode runs: in memory only, never a
/// Keychain item or a file.
pub const SERVICE: &str = "dev.mailstream.Mailstream.shots";

static PROVIDER: OnceLock<FakeServer> = OnceLock::new();
/// The engine's data folder, whose accounts.json says what each account was granted so far.
static SUPPORT: OnceLock<PathBuf> = OnceLock::new();
/// The last sign-in [`consent`] allowed: the account (`login_hint`, else a new one) and the
/// scopes asked for.
static CONSENTED: Mutex<Option<(String, String)>> = Mutex::new(None);
/// The account a sign-in without `login_hint` (a new account) signs in to.
const NEW_ACCOUNT: &str = "new@example.com";
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
    SUPPORT.set(support.to_path_buf()).ok();
    PROVIDER.set(FakeServer::start(answer)).ok();
}

/// `config` on the provider once [`start`] ran; as it is before.
pub(crate) fn config(mut config: Config) -> Config {
    let Some(server) = PROVIDER.get() else { return config };
    config.keychain_service = SERVICE.into();
    config.oauth.token_endpoint = Some(format!("{}/token", server.base));
    config.endpoints = fake_server::endpoints(&server.base);
    config.push = push::PushConfig::default();
    config.oauth.open_url = Arc::new(consent);
    // A port of its own, so a run never meets the app's sign-in or another run's.
    config.oauth.redirect_port =
        std::net::TcpListener::bind(("127.0.0.1", 0)).and_then(|listener| listener.local_addr()).map_or(0, |address| address.port());
    config
}

/// [`start`] ran: the engine's providers are the stand-in.
pub(crate) fn running() -> bool {
    PROVIDER.get().is_some()
}

/// Whether a search for `query` finds `envelope`: its subject or a sender's or recipient's name
/// or address holds it, ignoring case, as a search for a person or a subject finds them.
pub(crate) fn matches(envelope: &Envelope, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    let people = envelope.from.iter().chain(&envelope.to);
    let mut fields = std::iter::once(envelope.subject.as_str())
        .chain(people.flat_map(|person| person.name.as_deref().into_iter().chain([person.email.as_str()])));
    !query.is_empty() && fields.any(|field| field.to_lowercase().contains(&query))
}

/// The screenshot mode's browser: allows the sign-in page at `url` everything it asks and
/// redirects to the engine's listener with a code, which the stand-in's token endpoint redeems
/// ([`token`]).
fn consent(url: &str) {
    let Ok(url) = reqwest::Url::parse(url) else { return };
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let account = query.get("login_hint").cloned().unwrap_or_else(|| NEW_ACCOUNT.into());
    *CONSENTED.lock().expect("consent lock") = Some((account, query.get("scope").cloned().unwrap_or_default()));
    let Some(port) = query.get("redirect_uri").and_then(|uri| reqwest::Url::parse(uri).ok()?.port()) else { return };
    let state = query.get("state").cloned().unwrap_or_default();
    std::thread::spawn(move || {
        if let Ok(mut connection) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let request = format!("GET /?code=shots&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let _ = connection.write_all(request.as_bytes());
            let _ = std::io::Read::read_to_end(&mut connection, &mut Vec::new());
        }
    });
}

/// The token endpoint: a refresh gets an access token; a sign-in's code what [`consent`]
/// allowed on top of what the account had (Google's `include_granted_scopes`), with an id token
/// naming the account.
fn token(request: &FakeRequest) -> Reply {
    if request.form().get("grant_type").map(String::as_str) != Some("authorization_code") {
        return Reply::json(json!({ "access_token": "shots", "expires_in": 3600 }));
    }
    let Some((account, asked)) = CONSENTED.lock().expect("consent lock").take() else {
        return Reply::status(400, json!({ "error": "invalid_grant", "error_description": "No sign-in to redeem." }));
    };
    let had = SUPPORT.get().and_then(|support| Accounts::new(support).record(&account)).map(|record| record.scopes);
    let mut scopes = had.unwrap_or_default();
    for scope in asked.split(' ').filter(|scope| !scope.is_empty()) {
        if !scopes.iter().any(|known| known == scope) {
            scopes.push(scope.to_string());
        }
    }
    // As a consent that asks for no profile: no name, so the account keeps its own.
    let claims = json!({ "email": account }).to_string();
    let id_token = format!("e30.{}.sig", base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, claims));
    Reply::json(json!({
        "access_token": "shots", "expires_in": 3600, "refresh_token": format!("shots-{account}"),
        "scope": scopes.join(" "), "id_token": id_token,
    }))
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
        ("POST", "/token") => token(request),
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
