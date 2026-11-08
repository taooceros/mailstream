//! Push and polling end to end: an engine in a throwaway folder, signed in through a test
//! Keychain item, against a fake relay socket and fake Gmail and Graph servers. Nothing reaches a
//! provider or the real relay.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request as Handshake, Response as HandshakeResponse};

use crate::fake::{FakeRequest, FakeServer, Reply};
use crate::keychain::{Keychain, SignIns};
use crate::push::{PushConfig, PushTiming};
use crate::tests::{cleanup, config};
use crate::{Config, Engine, OAuthClient, OAuthConfig, backend, gmail};

/// A stand-in for `Push/worker.js`'s socket: answers `ping` with `pong` (unless `silent`), sends
/// what `events` broadcasts to every socket, and closes them all on `drop_all`.
struct FakeRelay {
    /// `http://127.0.0.1:<port>`, as `MSPushURL` names the relay.
    base: String,
    connections: Arc<AtomicUsize>,
    authorizations: Arc<Mutex<Vec<String>>>,
    silent: Arc<AtomicBool>,
    events: broadcast::Sender<String>,
    closes: broadcast::Sender<()>,
}

impl FakeRelay {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let authorizations = Arc::new(Mutex::new(Vec::new()));
        let silent = Arc::new(AtomicBool::new(false));
        let (events, _) = broadcast::channel(16);
        let (closes, _) = broadcast::channel(4);
        let relay = Self {
            base,
            connections: connections.clone(),
            authorizations: authorizations.clone(),
            silent: silent.clone(),
            events: events.clone(),
            closes: closes.clone(),
        };
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                while let Ok((stream, _)) = listener.accept().await {
                    let (connections, authorizations, silent) =
                        (connections.clone(), authorizations.clone(), silent.clone());
                    let (mut events, mut closes) = (events.subscribe(), closes.subscribe());
                    tokio::spawn(async move {
                        let seen = authorizations.clone();
                        // tungstenite's callback type names its own (large) error response.
                        #[allow(clippy::result_large_err)]
                        let check = move |request: &Handshake, response: HandshakeResponse| {
                            assert_eq!(request.uri().path(), "/socket");
                            let authorization = request.headers().get("Authorization").and_then(|value| value.to_str().ok());
                            seen.lock().unwrap().push(authorization.unwrap_or_default().to_string());
                            Ok(response)
                        };
                        let Ok(socket) = tokio_tungstenite::accept_hdr_async(stream, check).await else { return };
                        connections.fetch_add(1, Ordering::SeqCst);
                        let (mut write, mut read) = socket.split();
                        loop {
                            tokio::select! {
                                message = read.next() => match message {
                                    Some(Ok(Message::Text(text))) if text.as_str() == "ping" => {
                                        if !silent.load(Ordering::SeqCst) {
                                            let _ = write.send(Message::text("pong")).await;
                                        }
                                    }
                                    Some(Ok(_)) => {}
                                    _ => return,
                                },
                                event = events.recv() => {
                                    let Ok(event) = event else { return };
                                    let _ = write.send(Message::text(event)).await;
                                }
                                _ = closes.recv() => return,
                            }
                        }
                    });
                }
            });
        });
        relay
    }

    fn send(&self, event: Value) {
        self.events.send(event.to_string()).unwrap();
    }
}

const TOPIC: &str = "projects/p/topics/t";

/// A throwaway engine folder pointed at `server` and `relay`, signed in to `accounts` through a
/// test Keychain item; push.json may be written before the engine starts on it.
fn prepare(server: &FakeServer, relay: &FakeRelay, accounts: &[(&str, &str)]) -> Config {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = backend::Endpoints {
        gmail: gmail::GmailEndpoints {
            root: format!("{}/gmail/v1/users/me/", server.base),
            batch: format!("{}/batch/gmail/v1", server.base),
        },
        graph: format!("{}/v1.0/", server.base),
        chat: format!("{}/chat/v1/", server.base),
        people: format!("{}/people/v1/", server.base),
    };
    config.push = PushConfig {
        url: Some(relay.base.clone()),
        token: Some("relay-token".into()),
        graph_client_state: Some("client-state".into()),
        gmail_topic: Some(TOPIC.into()),
        timing: PushTiming {
            ping: Duration::from_millis(300),
            silence: Duration::from_millis(200),
            backoff: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            renewal: Duration::from_secs(3600),
            wake_check: Duration::from_secs(3600),
        },
    };
    std::fs::create_dir_all(&config.support_directory).unwrap();
    let records: Vec<Value> = accounts
        .iter()
        .map(|(id, provider)| json!({ "id": id, "provider": provider, "label": id, "scopes": ["openid", "mail"] }))
        .collect();
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(&records).unwrap()).unwrap();
    let mut sign_ins = SignIns::default();
    for (id, _) in accounts {
        sign_ins.refresh_tokens.insert(id.to_string(), format!("rt-{id}"));
        sign_ins.clients.insert(id.to_string(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    }
    Keychain::new(config.keychain_service.clone()).store(&sign_ins).unwrap();
    config
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Graph with an inbox and sent mail, subscriptions that answer like Graph's, and Gmail's watch
/// and stop (its mail reads answer 404: only the registration matters here).
fn provider_server() -> FakeServer {
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request: &FakeRequest| {
        let path = request.path.as_str();
        let method = request.method.as_str();
        match (method, path) {
            (_, "/token") => {
                let call = tokens.fetch_add(1, Ordering::SeqCst) + 1;
                Reply::json(json!({ "access_token": format!("at{call}"), "expires_in": 3600 }))
            }
            ("POST", "/gmail/v1/users/me/watch") => {
                Reply::json(json!({ "historyId": "9", "expiration": "1790000000000" }))
            }
            ("POST", "/gmail/v1/users/me/stop") => Reply::status(204, Value::Null),
            ("POST", "/v1.0/subscriptions") => {
                Reply::status(201, json!({ "id": "sub-1", "expirationDateTime": "2026-11-11T00:00:00.1234567Z" }))
            }
            ("PATCH", "/v1.0/subscriptions/sub-1") => {
                Reply::json(json!({ "id": "sub-1", "expirationDateTime": "2026-11-12T00:00:00Z" }))
            }
            ("DELETE", "/v1.0/subscriptions/sub-1") => Reply::status(204, Value::Null),
            ("GET", "/v1.0/me") => Reply::json(json!({ "displayName": "Me", "mail": "me@outlook.com" })),
            ("GET", "/v1.0/me/mailFolders") => Reply::json(json!({ "value": [
                { "id": "F-inbox", "displayName": "Inbox", "childFolderCount": 0, "unreadItemCount": 1, "totalItemCount": 1 },
                { "id": "F-sent", "displayName": "Sent Items", "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 0 },
            ]})),
            ("GET", "/v1.0/me/mailFolders/inbox") => Reply::json(json!({ "id": "F-inbox" })),
            ("GET", "/v1.0/me/mailFolders/sentitems") => Reply::json(json!({ "id": "F-sent" })),
            ("GET", other) if other.starts_with("/v1.0/me/mailFolders/") && other.ends_with("/messages") => {
                Reply::json(json!({ "value": [] }))
            }
            _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
        }
    })
}

fn requests(server: &FakeServer, method: &str, path: &str) -> Vec<FakeRequest> {
    server.requests.lock().unwrap().iter().filter(|request| request.method == method && request.path == path).cloned().collect()
}

fn registrations(config: &Config) -> Value {
    std::fs::read(config.support_directory.join("push.json"))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or(Value::Null)
}

#[test]
fn connects_registers_follows_notifications_and_stops_on_sign_out() {
    let server = provider_server();
    let relay = FakeRelay::start();
    let config = prepare(&server, &relay, &[("me@outlook.com", "microsoft"), ("me@gmail.com", "google")]);
    let engine = Engine::start(config.clone());
    let (sender, events) = mpsc::channel();
    engine.subscribe(move |event| {
        let _ = sender.send(event);
    });
    let push = engine.core.push.clone();

    // The socket connects with the relay's token; its first pong marks push connected.
    wait_until("push connected", || push.is_connected());
    assert_eq!(relay.authorizations.lock().unwrap()[0], "Bearer relay-token");
    wait_until("a sync event", || events.try_iter().any(|event| event == "sync"));

    // Both accounts registered, written to push.json as the Swift engine writes it.
    wait_until("registrations", || registrations(&config).as_object().is_some_and(|all| all.len() == 2));
    let watch = &requests(&server, "POST", "/gmail/v1/users/me/watch")[0];
    assert_eq!(serde_json::from_slice::<Value>(&watch.body).unwrap(), json!({ "topicName": TOPIC }));
    assert!(watch.header("Authorization").is_some_and(|value| value.starts_with("Bearer at")));
    let subscribe = &requests(&server, "POST", "/v1.0/subscriptions")[0];
    let body: Value = serde_json::from_slice(&subscribe.body).unwrap();
    let endpoint = format!("{}/graph", relay.base);
    assert_eq!(
        (&body["changeType"], &body["notificationUrl"], &body["lifecycleNotificationUrl"], &body["resource"], &body["clientState"]),
        (&json!("created,updated,deleted"), &json!(endpoint), &json!(endpoint), &json!("me/messages"), &json!("client-state"))
    );
    let expires = crate::model::parse_iso8601(body["expirationDateTime"].as_str().unwrap()).unwrap();
    assert!((expires - crate::model::now() - 4200.0 * 60.0).abs() < 60.0);
    let text = std::fs::read_to_string(config.support_directory.join("push.json")).unwrap();
    assert_eq!(
        text,
        "{\n  \"me@gmail.com\" : {\n    \"expires\" : \"2026-09-21T14:13:20Z\",\n    \"id\" : \"projects\\/p\\/topics\\/t\",\n    \"kind\" : \"gmail\"\n  },\n  \"me@outlook.com\" : {\n    \"expires\" : \"2026-11-11T00:00:00Z\",\n    \"id\" : \"sub-1\",\n    \"kind\" : \"graph\"\n  }\n}"
    );

    // A Graph change refreshes that account's inbox and counts, half a second later.
    let inbox = || requests(&server, "GET", "/v1.0/me/mailFolders/F-inbox/messages").len();
    let folders = || requests(&server, "GET", "/v1.0/me/mailFolders").len();
    wait_until("the first inbox fetch", || inbox() > 0);
    std::thread::sleep(Duration::from_millis(300));
    let (inbox_before, folders_before) = (inbox(), folders());
    relay.send(json!({ "source": "graph", "subscriptionId": "sub-1", "changeType": "created", "lifecycleEvent": null }));
    relay.send(json!({ "source": "graph", "subscriptionId": "sub-1", "changeType": "updated", "lifecycleEvent": null }));
    wait_until("the pushed inbox fetch", || inbox() > inbox_before && folders() > folders_before);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(inbox(), inbox_before + 1, "the two notifications came as one refresh");
    // An unknown subscription or address is ignored; a Gmail change refreshes that account.
    let labels = || requests(&server, "GET", "/gmail/v1/users/me/labels").len();
    let labels_before = labels();
    relay.send(json!({ "source": "graph", "subscriptionId": "other" }));
    relay.send(json!({ "source": "gmail", "email": "Someone@else.com", "historyId": "1" }));
    relay.send(json!({ "source": "gmail", "email": "Me@Gmail.com", "historyId": "10" }));
    wait_until("the pushed Gmail counts", || labels() > labels_before);
    assert_eq!(inbox(), inbox_before + 1);

    // Graph asking for reauthorization renews the subscription at once.
    relay.send(json!({ "source": "graph", "subscriptionId": "sub-1", "lifecycleEvent": "reauthorizationRequired" }));
    wait_until("the renewal", || !requests(&server, "PATCH", "/v1.0/subscriptions/sub-1").is_empty());
    let renewal: Value = serde_json::from_slice(&requests(&server, "PATCH", "/v1.0/subscriptions/sub-1")[0].body).unwrap();
    assert!(renewal["expirationDateTime"].as_str().is_some_and(|date| date.ends_with('Z')));
    wait_until("the renewed expiry", || registrations(&config)["me@outlook.com"]["expires"] == json!("2026-11-12T00:00:00Z"));

    // A relay that stops answering is dropped within the silence limit and reconnected.
    let connections = relay.connections.load(Ordering::SeqCst);
    relay.silent.store(true, Ordering::SeqCst);
    wait_until("push disconnected", || !push.is_connected());
    relay.silent.store(false, Ordering::SeqCst);
    wait_until("push reconnected", || push.is_connected() && relay.connections.load(Ordering::SeqCst) > connections);
    // So is one that closes the socket.
    let connections = relay.connections.load(Ordering::SeqCst);
    relay.closes.send(()).unwrap();
    wait_until("push back after a close", || push.is_connected() && relay.connections.load(Ordering::SeqCst) > connections);

    // `refresh` starts at once and reports loading through `sync`.
    while events.try_recv().is_ok() {}
    assert_eq!(block_on(engine.call("refresh", json!({}))).unwrap(), json!({ "started": true }));
    wait_until("loading ends", || !engine.core.mail.is_loading() && engine.core.mail.last_refresh().is_some());
    assert!(events.try_iter().any(|event| event == "sync"));

    // Signing out stops the account's registration with its still-valid token, then drops it.
    let error = block_on(engine.call("refresh", json!({ "nope": 1 }))).unwrap_err();
    assert_eq!(error.code, "usage");
    block_on(engine.call("accounts.remove", json!({ "account": "me@outlook.com", "yes": true }))).unwrap();
    let deleted = requests(&server, "DELETE", "/v1.0/subscriptions/sub-1");
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].header("Authorization").is_some_and(|value| value.starts_with("Bearer at")));
    assert!(registrations(&config).get("me@outlook.com").is_none());
    block_on(engine.call("accounts.remove", json!({ "account": "me@gmail.com", "yes": true }))).unwrap();
    assert_eq!(requests(&server, "POST", "/gmail/v1/users/me/stop").len(), 1);
    assert_eq!(registrations(&config), json!({}));
    assert!(Keychain::new(config.keychain_service.clone()).load().unwrap().refresh_tokens.is_empty());
    drop(engine);
    cleanup(&config);
}

#[test]
fn renews_only_what_is_due_and_replaces_a_subscription_graph_forgot() {
    // A Swift engine's push.json: the Gmail watch is fresh, the Graph subscription due and gone.
    let server = FakeServer::start(|request: &FakeRequest| match (request.method.as_str(), request.path.as_str()) {
        (_, "/token") => Reply::json(json!({ "access_token": "at", "expires_in": 3600 })),
        ("PATCH", "/v1.0/subscriptions/old") => Reply::status(404, json!({ "error": { "message": "gone" } })),
        ("POST", "/v1.0/subscriptions") => Reply::status(201, json!({ "id": "new", "expirationDateTime": "2026-11-11T00:00:00Z" })),
        _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
    });
    let relay = FakeRelay::start();
    let config = prepare(&server, &relay, &[("me@outlook.com", "microsoft"), ("me@gmail.com", "google")]);
    // Left by another process before this engine starts.
    let fresh = crate::model::iso8601(crate::model::now() + 3.0 * 86_400.0);
    let due = crate::model::iso8601(crate::model::now() + 3600.0);
    std::fs::write(
        config.support_directory.join("push.json"),
        format!(
            "{{\n  \"me@gmail.com\" : {{\n    \"expires\" : \"{fresh}\",\n    \"id\" : \"projects\\/p\\/topics\\/t\",\n    \"kind\" : \"gmail\"\n  }},\n  \"me@outlook.com\" : {{\n    \"expires\" : \"{due}\",\n    \"id\" : \"old\",\n    \"kind\" : \"graph\"\n  }}\n}}"
        ),
    )
    .unwrap();
    let engine = Engine::start(config.clone());
    wait_until("the replacement subscription", || registrations(&config)["me@outlook.com"]["id"] == json!("new"));
    assert_eq!(requests(&server, "PATCH", "/v1.0/subscriptions/old").len(), 1);
    assert!(requests(&server, "POST", "/gmail/v1/users/me/watch").is_empty(), "a fresh watch is left alone");
    assert_eq!(registrations(&config)["me@gmail.com"]["expires"], json!(fresh));
    drop(engine);
    cleanup(&config);
}

#[test]
fn stays_idle_without_a_relay() {
    let server = provider_server();
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints.graph = format!("{}/v1.0/", server.base);
    let engine = Engine::start(config.clone());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!engine.core.push.is_connected());
    assert!(!config.support_directory.join("push.json").exists());
    drop(engine);
    cleanup(&config);
}

#[tokio::test]
async fn the_relay_socket_speaks_tls() {
    // A server that accepts and hangs up: the client gets as far as its TLS handshake, which
    // needs a crypto provider, and fails rather than panicking.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let result = tokio_tungstenite::connect_async(format!("wss://127.0.0.1:{port}/socket")).await;
    assert!(result.is_err());
}
