//! The chat commands end to end: an engine in a throwaway folder, signed in through a test
//! Keychain item, against a fake Google Chat answering what Google answers. Nothing reaches
//! Google and no message is sent anywhere but the fake.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use serde_json::{Value, json};

use crate::fake::{FakeRequest, FakeServer, Reply};
use crate::keychain::{Keychain, SignIns};
use crate::tests::{cleanup, config};
use crate::{Config, Engine, OAuthClient, OAuthConfig, backend, gmail};

const ME: &str = "me@gmail.com";

fn call(engine: &Engine, command: &str, args: Value) -> Result<Value, crate::EngineError> {
    block_on(engine.call(command, args))
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn user(id: &str, name: Option<&str>, email: Option<&str>) -> Value {
    json!({ "name": format!("users/{id}"), "displayName": name, "email": email, "type": "HUMAN" })
}

fn message(space: &str, id: &str, sender: Value, time: &str, text: &str) -> Value {
    json!({
        "name": format!("spaces/{space}/messages/{id}"), "sender": sender, "createTime": time, "lastUpdateTime": time,
        "text": text, "formattedText": format!("{text}\n"), "thread": { "name": format!("spaces/{space}/threads/{id}") },
    })
}

fn ann() -> Value {
    user("222", Some("Ann Lee"), Some("ann@x.com"))
}

fn me() -> Value {
    user("111", Some("Me"), None)
}

/// The fake Chat: a direct message with Ann (`A`, one unread), a named space (`B`, which refuses
/// sends), an unnamed group chat (`C`), and `D`, the direct message `spaces:setup` creates.
fn chat_reply(request: &FakeRequest, offline: &AtomicBool) -> Reply {
    if request.path == "/token" {
        return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
    }
    let Some(path) = request.path.strip_prefix("/chat/v1/") else {
        return Reply::status(404, json!({ "error": { "message": "Not Found" } }));
    };
    if offline.load(Ordering::SeqCst) {
        return Reply::status(404, json!({ "error": { "message": "Offline" } }));
    }
    let method = request.method.as_str();
    match (method, path) {
        ("GET", "spaces") => Reply::json(json!({ "spaces": [
            { "name": "spaces/C", "spaceType": "GROUP_CHAT", "lastActiveTime": "2026-11-01T08:00:00Z" },
            { "name": "spaces/A", "spaceType": "DIRECT_MESSAGE", "lastActiveTime": "2026-11-07T12:00:00.000000Z" },
            { "name": "spaces/B", "spaceType": "SPACE", "displayName": "Team", "lastActiveTime": "2026-11-07T11:30:00Z" },
        ]})),
        ("GET", "spaces:findDirectMessage") => match request.param("name") {
            Some("users/bad@x.com") => Reply::status(400, json!({ "error": { "message": "malformed user resource name" } })),
            _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
        },
        ("POST", "spaces:setup") => Reply::json(json!({ "name": "spaces/D", "spaceType": "DIRECT_MESSAGE" })),
        ("POST", "spaces/A/messages") => {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            Reply::json(json!({
                "name": "spaces/A/messages/m6", "sender": { "name": "users/111" }, "createTime": "2026-11-07T12:05:00Z",
                "text": body["text"], "thread": { "name": "spaces/A/threads/m6" },
            }))
        }
        ("POST", "spaces/B/messages") => Reply::status(403, json!({ "error": { "message": "No permission" } })),
        ("GET", "media/r/1") => Reply { status: 200, headers: vec![], body: b"PNG".to_vec() },
        _ => {
            let Some((space, rest)) = path.strip_prefix("spaces/").and_then(|rest| rest.split_once('/')) else {
                return read_state(method, path, request);
            };
            match rest {
                "messages" => messages(space, request.param("filter").is_some()),
                "members" => Reply::json(json!({ "memberships": match space {
                    "A" => vec![json!({ "member": ann() }), json!({ "member": { "name": "users/111" } })],
                    "C" => vec![json!({ "member": { "name": "users/222", "email": "ann@x.com" } }),
                                json!({ "member": user("333", Some("Cy"), Some("cy@x.com")) })],
                    _ => vec![json!({ "member": user("444", None, Some("new@x.com")) })],
                }})),
                _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            }
        }
    }
}

fn read_state(method: &str, path: &str, request: &FakeRequest) -> Reply {
    let Some(space) = path.strip_prefix("users/me/spaces/").and_then(|rest| rest.strip_suffix("/spaceReadState")) else {
        return Reply::status(404, json!({ "error": { "message": "Not Found" } }));
    };
    let name = format!("users/111/spaces/{space}/spaceReadState");
    if method == "PATCH" {
        assert_eq!(request.param("updateMask"), Some("lastReadTime"));
        return Reply::json(json!({ "name": name, "lastReadTime": "2026-11-07T12:00:01Z" }));
    }
    let last_read = match space {
        "A" => "2026-11-07T10:30:00Z",
        _ => "2026-11-07T11:30:00Z",
    };
    Reply::json(json!({ "name": name, "lastReadTime": last_read }))
}

/// Newest first, as `orderBy=createTime desc` answers; A has an older page behind its first.
fn messages(space: &str, older: bool) -> Reply {
    match (space, older) {
        ("A", false) => Reply::json(json!({ "nextPageToken": "next", "messages": [
            message("A", "m5", ann(), "2026-11-07T12:00:00Z", "see <https://x.com/doc|the doc>"),
            message("A", "m4", me(), "2026-11-07T11:00:00Z", "*sure*"),
            message("A", "m3", ann(), "2026-11-07T10:00:00Z", "lunch?"),
        ]})),
        ("A", true) => Reply::json(json!({ "messages": [
            message("A", "m2", ann(), "2026-11-07T09:00:00Z", "hi"),
            message("A", "m1", ann(), "2026-11-07T08:00:00Z", "hello"),
        ]})),
        ("B", _) => Reply::json(json!({ "messages": [
            message("B", "b1", user("333", None, Some("bob@x.com")), "2026-11-07T11:30:00Z", "*hi* all"),
        ]})),
        ("C", _) => Reply::json(json!({ "messages": [message("C", "c1", me(), "2026-11-01T08:00:00Z", "ok")] })),
        _ => Reply::json(json!({})),
    }
}

fn engine_for(server: &FakeServer) -> Config {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = backend::Endpoints {
        gmail: gmail::GmailEndpoints {
            root: format!("{}/gmail/v1/users/me/", server.base),
            batch: format!("{}/batch/gmail/v1", server.base),
        },
        graph: format!("{}/v1.0/", server.base),
        chat: format!("{}/chat/v1/", server.base),
    };
    std::fs::create_dir_all(&config.support_directory).unwrap();
    let scopes = [
        "openid",
        "https://www.googleapis.com/auth/chat.messages.readonly",
        "https://www.googleapis.com/auth/chat.spaces.create",
    ];
    let records = json!([
        { "id": ME, "provider": "google", "label": "Personal", "scopes": scopes },
        { "id": "me@outlook.com", "provider": "microsoft", "label": "Work", "scopes": ["openid"] },
    ]);
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(&records).unwrap()).unwrap();
    let mut sign_ins = SignIns::default();
    sign_ins.refresh_tokens.insert(ME.into(), "rt".into());
    sign_ins.clients.insert(ME.into(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    Keychain::new(config.keychain_service.clone()).store(&sign_ins).unwrap();
    config
}

fn requests(server: &FakeServer, method: &str, path: &str) -> Vec<FakeRequest> {
    let requests = server.requests.lock().unwrap();
    requests.iter().filter(|request| request.method == method && request.path == path).cloned().collect()
}

fn spaces(engine: &Engine) -> Value {
    call(engine, "chat.spaces", json!({})).unwrap()
}

fn names(list: &Value) -> Vec<String> {
    list["spaces"].as_array().unwrap().iter().map(|space| space["space"].as_str().unwrap().to_string()).collect()
}

#[test]
fn lists_opens_reads_sends_and_starts_chats() {
    let offline = Arc::new(AtomicBool::new(false));
    let switch = offline.clone();
    let server = FakeServer::start(move |request| chat_reply(request, &switch));
    let config = engine_for(&server);
    let engine = Engine::start(config.clone());
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let heard = events.clone();
    engine.subscribe(move |event| heard.lock().unwrap().push(event));

    // Only the Google account with Chat access is a chat account.
    wait_until("the spaces", || names(&spaces(&engine)).len() == 3);
    let list = spaces(&engine);
    assert_eq!(names(&list), ["spaces/A", "spaces/B", "spaces/C"]);
    assert_eq!(list["accounts"], json!([{ "id": ME, "label": "Personal", "canStart": true, "unread": 1 }]));
    let (a, b, c) = (&list["spaces"][0], &list["spaces"][1], &list["spaces"][2]);
    assert_eq!((a["kind"].as_str(), a["title"].as_str(), a["preview"].as_str()), (Some("dm"), Some("Ann Lee"), Some("Ann: see the doc")));
    assert_eq!((a["unread"].as_bool(), a["avatarKey"].as_str(), a["avatarName"].as_str()), (Some(true), Some("ann@x.com"), Some("Ann Lee")));
    assert_eq!(a["activity"], json!("2026-11-07T12:00:00Z"));
    assert_eq!((b["title"].as_str(), b["preview"].as_str(), b["unread"].as_bool()), (Some("Team"), Some("bob@x.com: hi all"), Some(false)));
    // Google withholds Ann's name in the group, and no message of hers there gives it.
    assert_eq!((c["kind"].as_str(), c["title"].as_str(), c["preview"].as_str()), (Some("group"), Some("ann@x.com, Cy"), Some("You: ok")));
    assert!(events.lock().unwrap().contains(&"chat"));
    assert_eq!(call(&engine, "chat.spaces", json!({ "account": "nobody@x.com" })).unwrap_err().code, "unknown_account");
    assert_eq!(call(&engine, "chat.messages", json!({ "account": "me@outlook.com", "space": "spaces/A" })).unwrap_err().code, "unknown_account");

    // Opening: the newest page, marked read at once and at Google.
    let open = json!({ "account": ME, "space": "spaces/A" });
    call(&engine, "chat.open", open.clone()).unwrap();
    wait_until("the messages", || call(&engine, "chat.messages", open.clone()).unwrap()["loaded"] == json!(true));
    let thread = call(&engine, "chat.messages", open.clone()).unwrap();
    let ids: Vec<&str> = thread["messages"].as_array().unwrap().iter().map(|message| message["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["spaces/A/messages/m3", "spaces/A/messages/m4", "spaces/A/messages/m5"]);
    assert_eq!((thread["title"].as_str(), thread["me"].as_str(), thread["more"].as_bool()), (Some("Ann Lee"), Some("users/111"), Some(true)));
    assert_eq!(thread["messages"][1]["mine"], json!(true));
    assert_eq!((thread["messages"][1]["text"].as_str(), thread["messages"][1]["markup"].as_str()), (Some("sure"), Some("*sure*")));
    assert_eq!(thread["members"][0]["email"], json!("ann@x.com"));
    assert_eq!(spaces(&engine)["spaces"][0]["unread"], json!(false));
    wait_until("the read mark", || !requests(&server, "PATCH", "/chat/v1/users/me/spaces/A/spaceReadState").is_empty());
    assert!(events.lock().unwrap().contains(&"chatMessages"));

    // Earlier messages join below.
    call(&engine, "chat.earlier", open.clone()).unwrap();
    wait_until("the earlier page", || call(&engine, "chat.messages", open.clone()).unwrap()["messages"].as_array().unwrap().len() == 5);
    let thread = call(&engine, "chat.messages", open.clone()).unwrap();
    assert_eq!((thread["more"].as_bool(), thread["loadingEarlier"].as_bool()), (Some(false), Some(false)));
    let filter = requests(&server, "GET", "/chat/v1/spaces/A/messages").iter().find_map(|request| request.param("filter").map(String::from));
    assert_eq!(filter.as_deref(), Some("createTime < \"2026-11-07T10:00:00.000000Z\""));

    // Sending asks first, then waits for Google; a refusal comes back and leaves nothing pending.
    let send = json!({ "account": ME, "space": "spaces/A", "text": "  Hello there " });
    assert_eq!(call(&engine, "chat.send", send.clone()).unwrap_err().code, "confirm");
    assert!(requests(&server, "POST", "/chat/v1/spaces/A/messages").is_empty());
    let mut confirmed = send.clone();
    confirmed["yes"] = json!(true);
    assert_eq!(call(&engine, "chat.send", confirmed).unwrap(), json!({ "sent": true, "account": ME, "space": "spaces/A" }));
    let posted = requests(&server, "POST", "/chat/v1/spaces/A/messages");
    assert_eq!(serde_json::from_slice::<Value>(&posted[0].body).unwrap(), json!({ "text": "Hello there" }));
    let thread = call(&engine, "chat.messages", open.clone()).unwrap();
    assert_eq!(thread["messages"][5]["id"], json!("spaces/A/messages/m6"));
    assert_eq!(thread["pending"], json!([]));
    assert_eq!(spaces(&engine)["spaces"][0]["preview"], json!("You: Hello there"));
    let refused = json!({ "account": ME, "space": "spaces/B", "text": "hi", "yes": true });
    let error = call(&engine, "chat.send", refused).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("provider", "No permission"));
    let thread_b = call(&engine, "chat.messages", json!({ "account": ME, "space": "spaces/B" })).unwrap();
    assert_eq!(thread_b["pending"], json!([]));

    // People, and starting conversations: a known one locally, a new one at Google.
    let people = call(&engine, "chat.people", json!({ "account": ME })).unwrap();
    let emails: Vec<&str> = people["people"].as_array().unwrap().iter().map(|person| person["email"].as_str().unwrap()).collect();
    assert_eq!((people["canStart"].as_bool(), emails), (Some(true), vec!["ann@x.com", "cy@x.com"]));
    let known = call(&engine, "chat.start", json!({ "account": ME, "people": ["ANN@x.com", ME] })).unwrap();
    assert_eq!(known, json!({ "account": ME, "space": "spaces/A" }));
    assert!(requests(&server, "GET", "/chat/v1/spaces:findDirectMessage").is_empty());
    let started = call(&engine, "chat.start", json!({ "account": ME, "people": "new@x.com" })).unwrap();
    assert_eq!(started["space"], json!("spaces/D"));
    let setup: Value = serde_json::from_slice(&requests(&server, "POST", "/chat/v1/spaces:setup")[0].body).unwrap();
    assert_eq!(setup["space"], json!({ "spaceType": "DIRECT_MESSAGE" }));
    assert_eq!(setup["memberships"], json!([{ "member": { "name": "users/new@x.com", "type": "HUMAN" } }]));
    assert_eq!(setup["requestId"].as_str().map(str::len), Some(36));
    assert_eq!(names(&spaces(&engine))[0], "spaces/D");
    let error = call(&engine, "chat.start", json!({ "account": ME, "people": ["bad@x.com"] })).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("failed", "No Google Chat user has the address bad@x.com."));
    let error = call(&engine, "chat.start", json!({ "account": ME, "people": [ME] })).unwrap_err();
    assert_eq!(error.message, "Add someone to chat with");

    // Attachments download once into the cache.
    let fetch = json!({ "account": ME, "resource": "r/1", "name": "a/b.png" });
    let path = call(&engine, "chat.attachment", fetch.clone()).unwrap()["path"].as_str().unwrap().to_string();
    assert!(path.starts_with(config.cache_directory.join("chat-media").to_str().unwrap()) && path.ends_with("/a-b.png"));
    assert_eq!(std::fs::read(&path).unwrap(), b"PNG");
    call(&engine, "chat.attachment", fetch).unwrap();
    assert_eq!(requests(&server, "GET", "/chat/v1/media/r/1").len(), 1);
    assert_eq!(call(&engine, "chat.close", json!({})).unwrap(), json!({ "closed": true }));

    // Next start, offline: the list and the opened space paint from mail.db.
    block_on(engine.core.chat.flush());
    drop(engine);
    offline.store(true, Ordering::SeqCst);
    let engine = Engine::start(config.clone());
    wait_until("the cached spaces", || names(&spaces(&engine)).len() == 4);
    assert_eq!(names(&spaces(&engine)), ["spaces/D", "spaces/A", "spaces/B", "spaces/C"]);
    call(&engine, "chat.open", open.clone()).unwrap();
    wait_until("the cached messages", || call(&engine, "chat.messages", open.clone()).unwrap()["messages"].as_array().unwrap().len() == 6);
    let thread = call(&engine, "chat.messages", open.clone()).unwrap();
    assert_eq!((thread["loaded"].as_bool(), thread["failure"].clone()), (Some(false), Value::Null));
    drop(engine);
    cleanup(&config);
}

#[test]
fn signing_out_drops_the_account_and_its_messages() {
    let offline = Arc::new(AtomicBool::new(false));
    let switch = offline.clone();
    let server = FakeServer::start(move |request| chat_reply(request, &switch));
    let config = engine_for(&server);
    let engine = Engine::start(config.clone());
    wait_until("the spaces", || names(&spaces(&engine)).len() == 3);
    call(&engine, "chat.open", json!({ "account": ME, "space": "spaces/C" })).unwrap();
    call(&engine, "accounts.remove", json!({ "account": ME, "yes": true })).unwrap();
    wait_until("the account to go", || spaces(&engine)["accounts"] == json!([]));
    assert_eq!(names(&spaces(&engine)), Vec::<String>::new());
    assert_eq!(call(&engine, "chat.open", json!({ "account": ME, "space": "spaces/C" })).unwrap_err().code, "unknown_account");
    block_on(engine.core.chat.flush());
    let cache = engine.core.mail.cache();
    let key = crate::chat_model::ChatSpaceKey::new(ME, "spaces/C");
    let recent = engine.runtime.block_on(async { (cache.load_chat_recent().await, cache.load_chat_messages(&key).await) });
    assert_eq!(recent.0, Some(vec![]));
    assert!(recent.1.is_none());
    drop(engine);
    cleanup(&config);
}

#[test]
fn polls_every_chat_poll_seconds_and_refetches_only_what_changed() {
    let offline = Arc::new(AtomicBool::new(false));
    let switch = offline.clone();
    let server = FakeServer::start(move |request| chat_reply(request, &switch));
    let config = engine_for(&server);
    std::fs::write(config.support_directory.join("settings.json"), "{\n  \"chat.pollSeconds\" : 5\n}\n").unwrap();
    let engine = Engine::start(config.clone());
    wait_until("the spaces", || names(&spaces(&engine)).len() == 3);
    let start = Instant::now();
    while requests(&server, "GET", "/chat/v1/spaces").len() < 2 {
        assert!(start.elapsed() < Duration::from_secs(9), "no poll after chat.pollSeconds");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    // Unchanged spaces are not summarized again; the unread one has its read state rechecked.
    assert_eq!(requests(&server, "GET", "/chat/v1/spaces/B/messages").len(), 1);
    assert_eq!(requests(&server, "GET", "/chat/v1/spaces/C/members").len(), 1);
    assert_eq!(requests(&server, "GET", "/chat/v1/users/me/spaces/A/spaceReadState").len(), 2);
    drop(engine);
    cleanup(&config);
}
