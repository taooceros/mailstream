//! What the engine's tests share: throwaway engine folders signed in through a test Keychain
//! item, calling and waiting on an engine, and stand-ins for the providers (an HTTP server on the
//! loopback interface, on a thread of its own, answering each request with a handler's reply and
//! keeping what it got). Nothing reaches a provider.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::keychain::{Keychain, SignIns};
use crate::{BuiltInClients, Config, Engine, EngineError, OAuthClient, OAuthConfig, avatars, backend, gmail, omp, push};

// MARK: - Engines

/// A number no other test in this process gets, for temporary names.
pub fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A throwaway engine folder and Keychain item whose providers are all unreachable.
pub fn config(oauth: OAuthConfig) -> Config {
    let name = format!("mailstream-engine-{}-{}", std::process::id(), unique());
    let root = std::env::temp_dir().join(name.clone());
    Config {
        support_directory: root.join("support"),
        cache_directory: root.join("caches"),
        keychain_service: format!("dev.mailstream.test.{name}"),
        built_in_clients: BuiltInClients::new(),
        oauth,
        // Nothing listens there: no test reaches a real provider.
        endpoints: endpoints("http://127.0.0.1:9"),
        // No relay: no test registers push with a provider unless it sets a fake one.
        push: push::PushConfig::default(),
        // A PATH without omp (the system's tools only): tagging runs only the fake omp a
        // test names.
        tool_environment: Some(HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])),
        // No Apple Intelligence: only a fake helper a test writes there runs.
        apple_tagger: root.join(omp::APPLE_TAGGER),
        avatar_web: Arc::new(avatars::tests::FakeWeb::default()),
        // The test of the socket turns it on.
        control_socket: false,
    }
}

/// Every provider's root under `base`, at the paths the fake servers answer.
fn endpoints(base: &str) -> backend::Endpoints {
    backend::Endpoints {
        gmail: gmail::GmailEndpoints {
            root: format!("{base}/gmail/v1/users/me/"),
            batch: format!("{base}/batch/gmail/v1"),
        },
        graph: format!("{base}/v1.0/"),
        chat: format!("{base}/chat/v1/"),
        people: format!("{base}/people/v1/"),
        calendar: format!("{base}/calendar/v3/"),
    }
}

/// `config` with every provider and the token endpoint (`/token`) on `server`.
pub fn server_config(server: &FakeServer) -> Config {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = endpoints(&server.base);
    config
}

/// A mail account's accounts.json record: labelled with its id, mail scopes only.
pub fn mail_account(id: &str, provider: &str) -> Value {
    json!({ "id": id, "provider": provider, "label": id, "scopes": ["openid", "mail"] })
}

/// Writes `records` as the folder's accounts.json.
pub fn write_accounts(config: &Config, records: &[Value]) {
    std::fs::create_dir_all(&config.support_directory).unwrap();
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(records).unwrap()).unwrap();
}

/// Signs `ids` in through the folder's test Keychain item: refresh token `rt-<id>`, a client of
/// the test's.
pub fn sign_in(config: &Config, ids: &[&str]) {
    let mut sign_ins = SignIns::default();
    for id in ids {
        sign_ins.refresh_tokens.insert(id.to_string(), format!("rt-{id}"));
        sign_ins.clients.insert(id.to_string(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    }
    Keychain::new(config.keychain_service.clone()).store(&sign_ins).unwrap();
}

/// `records` written and every one of them signed in.
pub fn sign_in_all(config: &Config, records: &[Value]) {
    write_accounts(config, records);
    let ids: Vec<&str> = records.iter().map(|record| record["id"].as_str().unwrap()).collect();
    sign_in(config, &ids);
}

/// An engine on `server`, signed in to `accounts` (`(id, provider)`) as mail accounts.
pub fn engine_for(server: &FakeServer, accounts: &[(&str, &str)]) -> (Engine, Config) {
    let config = server_config(server);
    let records: Vec<Value> = accounts.iter().map(|(id, provider)| mail_account(id, provider)).collect();
    sign_in_all(&config, &records);
    (Engine::start(config.clone()), config)
}

/// Empties the test Keychain item and deletes the folder.
pub fn cleanup(config: &Config) {
    let _ = Keychain::new(config.keychain_service.clone()).store(&SignIns::default());
    let _ = std::fs::remove_dir_all(config.support_directory.parent().unwrap());
}

pub fn call(engine: &Engine, command: &str, args: Value) -> Result<Value, EngineError> {
    block_on(engine.call(command, args))
}

/// Waits up to 10 s for `done`.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The token endpoint's answer: a fresh access token (`at1`, `at2`, …) per call.
pub fn token_reply(calls: &AtomicUsize) -> Reply {
    let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
    Reply::json(json!({ "access_token": format!("at{call}"), "expires_in": 3600 }))
}

// MARK: - Sign-in

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The browser's part: follows the sign-in page's redirect to `port` with `query` and the state
/// the page carries.
pub fn redirecting_browser(port: u16, query: &'static str) -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(move |url: &str| {
        let state = url.split('&').find_map(|pair| pair.strip_prefix("state=")).unwrap().to_string();
        tokio::spawn(async move {
            let mut connection = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let request = format!("GET /?{query}&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            connection.write_all(request.as_bytes()).await.unwrap();
            let mut page = String::new();
            connection.read_to_string(&mut page).await.unwrap();
            assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        });
    })
}

// MARK: - Fake servers

/// Serves each connection to a loopback listener with `serve`, on a thread and runtime of their
/// own (so a test's own runtime, or none, doesn't matter); answers `http://127.0.0.1:<port>`.
pub fn serve<Serve, Fut>(serve: Serve) -> String
where
    Serve: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            while let Ok((connection, _)) = listener.accept().await {
                tokio::spawn(serve(connection));
            }
        });
    });
    base
}

#[derive(Clone, Debug)]
pub struct FakeRequest {
    pub method: String,
    /// Decoded, without the query.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl FakeRequest {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as a form (`name=value&…`), decoded.
    pub fn form(&self) -> HashMap<String, String> {
        decoded_pairs(&self.text()).into_iter().collect()
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// How long the server waits before answering.
    pub delay: Duration,
}

impl Reply {
    pub fn json(value: Value) -> Self {
        Self::status(200, value)
    }

    pub fn status(status: u16, value: Value) -> Self {
        Self::bytes(status, serde_json::to_vec(&value).unwrap()).header("Content-Type", "application/json")
    }

    /// `body` as it is, without a content type.
    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self { status, headers: Vec::new(), body, delay: Duration::ZERO }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub struct FakeServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    pub requests: Arc<Mutex<Vec<FakeRequest>>>,
}

impl FakeServer {
    pub fn start(handler: impl Fn(&FakeRequest) -> Reply + Send + Sync + 'static) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (handler, kept) = (Arc::new(handler), requests.clone());
        let base = serve(move |mut connection| {
            let (handler, kept) = (handler.clone(), kept.clone());
            async move {
                let Some(request) = read_request(&mut connection).await else { return };
                kept.lock().unwrap().push(request.clone());
                let reply = handler(&request);
                tokio::time::sleep(reply.delay).await;
                let mut head = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
                for (name, value) in &reply.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = connection.write_all(head.as_bytes()).await;
                let _ = connection.write_all(&reply.body).await;
                let _ = connection.shutdown().await;
            }
        });
        Self { base, requests }
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The forms POSTed to `path`, in order.
    pub fn forms(&self, path: &str) -> Vec<HashMap<String, String>> {
        self.requests.lock().unwrap().iter().filter(|request| request.path == path).map(FakeRequest::form).collect()
    }
}

/// `name=value&…` decoded, pairs without `=` as empty values.
fn decoded_pairs(text: &str) -> Vec<(String, String)> {
    let decode = |text: &str| percent_decode_str(text).decode_utf8_lossy().into_owned();
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

async fn read_request(connection: &mut TcpStream) -> Option<FakeRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = connection.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..count]);
        let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_string(), value.trim().to_string())))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < end + 4 + length {
            let count = connection.read(&mut chunk).await.ok()?;
            if count == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..count]);
        }
        let body = buffer[end + 4..].to_vec();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let path = percent_decode_str(path).decode_utf8_lossy().into_owned();
        return Some(FakeRequest { method, path, query: decoded_pairs(query), headers, body });
    }
}
