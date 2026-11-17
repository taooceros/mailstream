//! Mail bodies between the user's devices (`peer_mail.rs`, MS-394): the protocol between two
//! paired support folders on the loopback interface, and two paired engines that take bodies
//! from each other before asking the fake Gmail and Graph.
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iroh::address_lookup::MemoryLookup;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use serde_json::{Value, json};

use crate::accounts::Accounts;
use crate::api::Priority;
use crate::cache::{CONTENT_VERSION, MailCache};
use crate::keychain::Keychain;
use crate::mail_tests::{GmailCounters, gmail_metadata, gmail_reply, graph_reply};
use crate::model::{Attachment, Mailbox, MessageContent, MessageRef};
use crate::oauth::OAuthClient;
use crate::peer_mail::{ALPN, PeerMail};
use crate::peer_sync::{Network, PeerSync, Service};
use crate::testkit::{FakeServer, TestConfig, call, config, mail_account, server_config, sign_in_all, token_reply, write_accounts};
use crate::{Engine, OAuthConfig};

struct Device {
    _folder: TestConfig,
    cache: Arc<MailCache>,
    mail: Arc<PeerMail>,
    sync: PeerSync,
}

impl Device {
    async fn open(lookup: &MemoryLookup, accounts: &[Value]) -> Self {
        let folder = config(OAuthConfig::default());
        write_accounts(&folder, accounts);
        let cache = Arc::new(MailCache::new(folder.support_directory.join("mail.db"), folder.cache_directory.clone()));
        let mail = PeerMail::new(cache.clone(), Arc::new(Accounts::new(&folder.support_directory)));
        let services: Vec<Arc<dyn Service>> = vec![mail.clone()];
        let sync = PeerSync::open_with(&folder.support_directory, Network::Loopback(lookup.clone()), services).await.unwrap();
        lookup.add_endpoint_info(sync.loopback_addr());
        Self { _folder: folder, cache, mail, sync }
    }

    async fn id(&self) -> EndpointId {
        self.sync.status().await.endpoint_id.parse().unwrap()
    }
}

fn message(account: &str, mailbox: &str, id: &str) -> MessageRef {
    MessageRef { account: account.into(), mailbox: mailbox.into(), id: id.into() }
}

fn mailbox(id: &str, role: &str) -> Mailbox {
    serde_json::from_value(json!({ "account": "me@gmail.com", "id": id, "name": id, "role": role })).unwrap()
}

/// Stores a body with an inline image and an attachment for `message`, as the provider's
/// answer would have been stored.
fn store(cache: &MailCache, message: &MessageRef) {
    let directory = cache.message_directory(message);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("0-logo.png"), b"png").unwrap();
    std::fs::write(directory.join("1-menu.pdf"), b"%PDF").unwrap();
    let content = MessageContent {
        html: Some(format!("<p>Hi <img src=\"{}0-logo.png\"></p>", MailCache::part_url_base(message))),
        text: Some("Hi".into()),
        cc: Vec::new(),
        reply_to: Vec::new(),
        references: Vec::new(),
        attachments: vec![Attachment {
            part: 1,
            filename: "menu.pdf".into(),
            mime: "application/pdf".into(),
            size: 4,
            content_id: None,
            inline: false,
            stored_name: "1-menu.pdf".into(),
        }],
        snippet: "Hi".into(),
        invitation: None,
    };
    cache.save_content(&content, message);
}

/// Sends `request` on [`ALPN`] from `endpoint` to `to` and reads the whole answer.
async fn ask(endpoint: &Endpoint, to: EndpointId, request: Value) -> Result<Vec<u8>, String> {
    let connection = endpoint.connect(EndpointAddr::new(to), ALPN).await.map_err(|error| error.to_string())?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| error.to_string())?;
    send.write_all(&serde_json::to_vec(&request).unwrap()).await.map_err(|error| error.to_string())?;
    send.finish().map_err(|error| error.to_string())?;
    recv.read_to_end(1 << 20).await.map_err(|error| error.to_string())
}

fn wanted(id: &str) -> Value {
    json!({ "account": "me@gmail.com", "mailbox": "INBOX", "id": id })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

async fn eventually<T>(what: &str, mut attempt: impl AsyncFnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = attempt().await {
            return value;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_devices_take_stored_bodies_from_each_other() {
    let lookup = MemoryLookup::new();
    let account = mail_account("me@gmail.com", "google");
    let a = Device::open(&lookup, std::slice::from_ref(&account)).await;
    let b = Device::open(&lookup, std::slice::from_ref(&account)).await;
    // A holds m1 under a label, not the inbox B asks about, and mail of an account it isn't
    // signed in to.
    store(&a.cache, &message("me@gmail.com", "Label_9", "m1"));
    store(&a.cache, &message("other@gmail.com", "INBOX", "x1"));
    a.cache.save_mailboxes(&[mailbox("INBOX", "inbox"), mailbox("Label_9", "other")], "me@gmail.com");
    a.cache.flush().await;

    a.sync.create_group().await.unwrap();
    b.sync.join(&a.sync.invite().await.unwrap().0).await.unwrap();
    let (id_a, id_b) = (a.id().await, b.id().await);
    for device in [&a, &b] {
        eventually("both admitted", async || {
            let admitted = device.sync.snapshot().await.ok()?.admitted;
            (admitted.contains_key(&id_a.to_string()) && admitted.contains_key(&id_b.to_string())).then_some(())
        })
        .await;
    }

    // B takes m1 from A without asking the provider, under its own key.
    let inbox = message("me@gmail.com", "INBOX", "m1");
    let content = eventually("B to take m1 from A", async || b.mail.fetch(&inbox, true, Priority::Background).await).await;
    assert_eq!(content.html.as_deref(), Some(format!("<p>Hi <img src=\"{}0-logo.png\"></p>", MailCache::part_url_base(&inbox)).as_str()));
    assert_eq!(content.text.as_deref(), Some("Hi"));
    assert_eq!(content.attachments[0].stored_name, "1-menu.pdf");
    let directory = b.cache.message_directory(&inbox);
    assert_eq!(std::fs::read(directory.join("0-logo.png")).unwrap(), b"png");
    assert_eq!(std::fs::read(directory.join("1-menu.pdf")).unwrap(), b"%PDF");
    b.cache.flush().await;
    assert_eq!(b.cache.load_content(&inbox).await, Some(content));
    let received = b.mail.received().await;
    assert_eq!((received.messages, received.gmail_units), (1, 80));

    // What A doesn't hold, or holds for an account not signed in there, it doesn't give; a
    // reader's load, with A connected now, gives up at once.
    let start = Instant::now();
    assert_eq!(b.mail.fetch(&message("me@gmail.com", "INBOX", "m2"), true, Priority::Interactive).await, None);
    assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    assert_eq!(b.mail.fetch(&message("other@gmail.com", "INBOX", "x1"), true, Priority::Background).await, None);

    // Another content version gets "no".
    let endpoint = b.sync.endpoint();
    let have = |version: u32| json!({ "ask": "have", "version": version, "messages": [wanted("m1"), wanted("m2")] });
    assert_eq!(ask(&endpoint, id_a, have(CONTENT_VERSION)).await.unwrap(), b"[true,false]");
    assert_eq!(ask(&endpoint, id_a, have(CONTENT_VERSION + 1)).await.unwrap(), b"[false,false]");
    let get = json!({ "ask": "get", "version": CONTENT_VERSION + 1, "message": wanted("m1") });
    let answer = ask(&endpoint, id_a, get).await.unwrap();
    assert_eq!(&answer[4..], br#"{"answer":"missing"}"#);

    // A device that isn't admitted is refused, also one that dials past its own gate.
    let stranger = Device::open(&lookup, std::slice::from_ref(&account)).await;
    stranger.sync.dial_anyone();
    assert!(ask(&stranger.sync.endpoint(), id_a, have(CONTENT_VERSION)).await.is_err());

    // Once the group removed the account, A no longer gives its mail.
    assert!(b.sync.set_account("me@gmail.com", None, now_ms()).await.unwrap());
    eventually("A to refuse the removed account", async || {
        (ask(&endpoint, id_a, have(CONTENT_VERSION)).await.ok()? == b"[false,false]").then_some(())
    })
    .await;

    // Once A removed B (MS-396), B gets nothing more, whatever it dials.
    a.sync.remove_device(&id_b.to_string()).await.unwrap();
    b.sync.dial_anyone();
    assert!(ask(&endpoint, id_a, have(CONTENT_VERSION)).await.is_err(), "A served a removed device");

    for device in [stranger, b, a] {
        device.sync.shutdown().await;
    }
}

// MARK: - Engines

/// A fake provider that records each request as `<path>?<format parameter>`.
fn recording_server(reply: impl Fn(&crate::testkit::FakeRequest) -> crate::testkit::Reply + Send + Sync + 'static) -> (FakeServer, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = FakeServer::start(move |request| {
        log.lock().unwrap().push(format!("{}?{}", request.path, request.param("format").unwrap_or("")));
        reply(request)
    });
    (server, seen)
}

fn asked(seen: &Mutex<Vec<String>>, what: &str) -> usize {
    seen.lock().unwrap().iter().filter(|request| request.as_str() == what).count()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An engine on `server` signed in to `account`, in a sync group, its mailboxes loaded; the
/// client that issued its token is known, so a device that joins gets the account (MS-376).
fn first_device(server: &FakeServer, lookup: &MemoryLookup, account: &str, provider: &str) -> (Engine, TestConfig) {
    let mut cfg = server_config(server);
    cfg.device_sync = Some(Network::Loopback(lookup.clone()));
    sign_in_all(&cfg, &[mail_account(account, provider)]);
    let keychain = Keychain::new(cfg.keychain_service.clone(), cfg.keychain_directory.clone());
    let mut sign_ins = keychain.load().unwrap();
    sign_ins.last_clients.insert(provider.into(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    keychain.store(&sign_ins).unwrap();
    let engine = Engine::start(cfg.clone());
    call(&engine, "sync.enable", json!({})).unwrap();
    until("mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()[account].as_array().is_some_and(|boxes| !boxes.is_empty()));
    (engine, cfg)
}

/// A new device on `server` that joins `first`'s group and gets `account` from it.
fn second_device(server: &FakeServer, lookup: &MemoryLookup, first: &Engine, account: &str) -> (Engine, TestConfig) {
    let mut cfg = server_config(server);
    cfg.device_sync = Some(Network::Loopback(lookup.clone()));
    let engine = Engine::start(cfg.clone());
    let ticket = call(first, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    call(&engine, "sync.join", json!({ "ticket": ticket })).unwrap();
    until("mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()[account].as_array().is_some_and(|boxes| !boxes.is_empty()));
    (engine, cfg)
}

fn listed(listing: &Value) -> Vec<String> {
    listing["conversations"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
}

/// Lists A's inbox and waits for every preview, so A holds each conversation's latest body.
fn load_previews(engine: &Engine) {
    let ids = listed(&call(engine, "mail.list", json!({ "limit": 25 })).unwrap());
    until("A's previews", || {
        let previews = call(engine, "mail.previews", json!({ "ids": ids })).unwrap();
        ids.iter().all(|id| previews[id.as_str()].is_string())
    });
}

fn received(engine: &Engine) -> Value {
    call(engine, "sync.status", json!({})).unwrap()["mail"].clone()
}

#[test]
fn paired_engines_take_gmail_bodies_from_each_other_first() {
    let counters = Arc::new(GmailCounters { tokens: AtomicUsize::new(0), labels: AtomicUsize::new(0), listings: AtomicUsize::new(0) });
    let metadata = gmail_metadata();
    let (server, seen) = recording_server(move |request| {
        if request.path == "/token" {
            return token_reply(&counters.tokens);
        }
        gmail_reply(request, &counters, &metadata)
    });
    let lookup = MemoryLookup::new();
    let (engine_a, _cfg_a) = first_device(&server, &lookup, "me@gmail.com", "google");

    // A reads the lunch thread (an inline image and an attachment) and the alert thread.
    load_previews(&engine_a);
    let lunch = "me@gmail.com|i00@x";
    let shown_a = call(&engine_a, "mail.show", json!({ "id": lunch })).unwrap();
    let alert = listed(&call(&engine_a, "mail.list", json!({ "limit": 25 })).unwrap())
        .into_iter()
        .find(|id| id.contains("i01") || id.contains("i02"))
        .unwrap();
    call(&engine_a, "mail.show", json!({ "id": alert })).unwrap();
    let (full, attachment) = ("/gmail/v1/users/me/messages/i00?full", "/gmail/v1/users/me/messages/i00/attachments/att1?");
    assert_eq!((asked(&seen, full), asked(&seen, attachment)), (1, 1));

    // B joins and gets the account: its previews come from A, which also connects them.
    let (engine_b, cfg_b) = second_device(&server, &lookup, &engine_a, "me@gmail.com");
    listed(&call(&engine_b, "mail.list", json!({ "limit": 25 })).unwrap());
    until("B to take previews from A", || received(&engine_b)["messages"].as_u64() > Some(0));

    // B opens the lunch thread: nothing goes to Gmail, and it shows what A shows.
    let shown_b = call(&engine_b, "mail.show", json!({ "id": lunch, "html": true })).unwrap();
    assert_eq!((asked(&seen, full), asked(&seen, attachment)), (1, 1));
    let (message_a, message_b) = (&shown_a["messages"][0], &shown_b["messages"][0]);
    assert_eq!(message_b["text"], message_a["text"]);
    assert_eq!(message_b["cc"], message_a["cc"]);
    let path = message_b["attachments"][0]["path"].as_str().unwrap();
    assert!(path.starts_with(cfg_b.cache_directory.to_str().unwrap()), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), b"%PDF");
    let html = message_b["html"].as_str().unwrap();
    assert!(html.starts_with("<p>Hi, lunch <img src=\"file://") && html.ends_with("/0-logo.png\"></p>"), "{html}");
    let inline = cfg_b.cache_directory.join("messages-v2").join(MailCache::message_key(&message("me@gmail.com", "INBOX", "i00")));
    assert_eq!(std::fs::read(inline.join("0-logo.png")).unwrap(), b"png");
    let counted = received(&engine_b);
    assert!(counted["gmailUnits"].as_u64().unwrap() >= 60 * counted["messages"].as_u64().unwrap() + 20, "{counted}");

    // What A doesn't hold comes from Gmail.
    let archived = call(&engine_b, "mail.search", json!({ "query": "invoice" })).unwrap();
    assert_eq!(listed(&archived), ["me@gmail.com|a0@x"]);
    call(&engine_b, "mail.show", json!({ "id": "me@gmail.com|a0@x" })).unwrap();
    assert_eq!(asked(&seen, "/gmail/v1/users/me/messages/a0?full"), 1);

    // With A gone, B asks Gmail for the alert's earlier message, which A read.
    drop(engine_a);
    let earlier = "/gmail/v1/users/me/messages/i02?full";
    let before = asked(&seen, earlier);
    let shown = call(&engine_b, "mail.show", json!({ "id": alert })).unwrap();
    assert!(shown["messages"].as_array().unwrap().iter().any(|message| message["text"] == "Body of i02"), "{shown}");
    assert_eq!(asked(&seen, earlier), before + 1);
    drop(engine_b);
}

#[test]
fn paired_engines_take_outlook_bodies_from_each_other_first() {
    let tokens = Arc::new(AtomicUsize::new(0));
    let (server, seen) = recording_server(move |request| {
        if request.path == "/token" {
            return token_reply(&tokens);
        }
        graph_reply(request)
    });
    let lookup = MemoryLookup::new();
    let (engine_a, _cfg_a) = first_device(&server, &lookup, "me@outlook.com", "microsoft");

    let photos = "me@outlook.com|g2@outlook";
    load_previews(&engine_a);
    let shown_a = call(&engine_a, "mail.show", json!({ "id": photos, "html": true })).unwrap();
    let requests = ["/v1.0/me/messages/g2?", "/v1.0/me/messages/g2/attachments?", "/v1.0/me/messages/g2/attachments/a2?"];
    let before: Vec<usize> = requests.iter().map(|request| asked(&seen, request)).collect();
    // A's preview load and its reader's may both have asked.
    assert!(before.iter().all(|count| *count > 0), "{before:?}");

    let (engine_b, cfg_b) = second_device(&server, &lookup, &engine_a, "me@outlook.com");
    listed(&call(&engine_b, "mail.list", json!({})).unwrap());
    until("B to take previews from A", || received(&engine_b)["messages"].as_u64() > Some(0));
    let shown_b = call(&engine_b, "mail.show", json!({ "id": photos, "html": true })).unwrap();
    let after: Vec<usize> = requests.iter().map(|request| asked(&seen, request)).collect();
    assert_eq!(after, before);
    let html_b = shown_b["messages"][0]["html"].as_str().unwrap();
    let html_a = shown_a["messages"][0]["html"].as_str().unwrap();
    assert!(html_b.starts_with("<p>See <img src=\"file://") && html_b.contains("0-photo.jpg"), "{html_b}");
    assert_eq!(html_b.split_once("0-photo.jpg").unwrap().1, html_a.split_once("0-photo.jpg").unwrap().1);
    let inline = cfg_b.cache_directory.join("messages-v2").join(MailCache::message_key(&message("me@outlook.com", "F-inbox", "g2")));
    assert_eq!(std::fs::read(inline.join("0-photo.jpg")).unwrap(), b"jpg");
    // Outlook has no units to count.
    assert_eq!(received(&engine_b)["gmailUnits"], 0);
    drop(engine_b);
    drop(engine_a);
}
