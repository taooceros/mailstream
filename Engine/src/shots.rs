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
//! has, so nothing it shows is replaced. Calendar and Chat changes are refused too.
use std::path::Path;
use std::sync::{LazyLock, Mutex, OnceLock};

use serde_json::{Value, json};

use crate::accounts::Accounts;
use crate::fake_server::{self, FakeRequest, FakeServer, Reply};
use crate::keychain::SignIns;
use crate::{Config, OAuthClient, push};

/// The sign-ins the engine reads while the screenshot mode runs: in memory only, never a
/// Keychain item or a file.
pub const SERVICE: &str = "dev.mailstream.Mailstream.shots";

static PROVIDER: OnceLock<FakeServer> = OnceLock::new();
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
    let mut lines = Vec::new();
    let reply = match (method, path) {
        ("POST", "/token") => Reply::json(json!({ "access_token": "shots", "expires_in": 3600 })),
        ("POST", "/upload/gmail/v1/users/me/messages/send") => Reply::json(json!({ "id": "shots-sent" })),
        ("POST", _) if path.starts_with("/gmail/v1/users/me/messages/") => Reply::json(json!({})),
        ("POST", "/v1.0/me/sendMail") => Reply::bytes(202, Vec::new()),
        ("POST", "/v1.0/$batch") => graph_batch(request, &mut lines),
        _ => refused(),
    };
    let mut log = LOG.lock().expect("log lock");
    log.push(format!("{} {method} {path}", reply.status));
    log.extend(lines);
    reply
}

/// Graph's `$batch`: changes to messages succeed, anything else is refused.
fn graph_batch(request: &FakeRequest, lines: &mut Vec<String>) -> Reply {
    let Ok(batch) = serde_json::from_slice::<Value>(&request.body) else { return refused() };
    let responses: Vec<Value> = batch["requests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|sub| {
            let (method, url) = (sub["method"].as_str().unwrap_or_default(), sub["url"].as_str().unwrap_or_default());
            let change = method != "GET" && url.starts_with("/me/messages/");
            let (status, body) = if change { (200, json!({})) } else { (400, refusal()) };
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
