//! The mail commands end to end: an engine in a throwaway folder, signed in through a test
//! Keychain item, against fake Gmail and Graph servers answering what the providers answer.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures::executor::block_on;
use serde_json::{Value, json};

use crate::fake::{FakeRequest, FakeServer, Reply};
use crate::keychain::{Keychain, SignIns};
use crate::tests::{cleanup, config};
use crate::{Config, Engine, OAuthClient, OAuthConfig, backend, gmail};

pub(crate) const BASE_MS: i64 = 1_790_000_000_000;

pub(crate) fn engine_for(server: &FakeServer, accounts: &[(&str, &str)]) -> (Engine, Config) {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = backend::Endpoints {
        gmail: gmail::GmailEndpoints {
            root: format!("{}/gmail/v1/users/me/", server.base),
            batch: format!("{}/batch/gmail/v1", server.base),
        },
        graph: format!("{}/v1.0/", server.base),
        chat: format!("{}/chat/v1/", server.base),
        people: format!("{}/people/v1/", server.base),
        calendar: format!("{}/calendar/v3/", server.base),
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
    (Engine::start(config.clone()), config)
}

pub(crate) fn call(engine: &Engine, command: &str, args: Value) -> Result<Value, crate::EngineError> {
    block_on(engine.call(command, args))
}

pub(crate) fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn token_reply(calls: &AtomicUsize) -> Reply {
    let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
    Reply::json(json!({ "access_token": format!("at{call}"), "expires_in": 3600 }))
}

// MARK: - Gmail

pub(crate) fn encode(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(text)
}

pub(crate) fn header(name: &str, value: &str) -> Value {
    json!({ "name": name, "value": value })
}

/// The fake mailbox: 60 inbox messages (two of them one sender's repeated alert) and one sent
/// reply to the newest.
pub(crate) fn gmail_metadata() -> HashMap<String, Value> {
    let mut messages = HashMap::new();
    for n in 0..60 {
        let id = format!("i{n:02}");
        let (from, subject) = match n {
            0 => ("Ann <ann@x.com>".to_string(), "Lunch?".to_string()),
            1 | 2 => ("Alerts <alerts@x.com>".to_string(), "Security alert".to_string()),
            _ => (format!("Person {n} <p{n}@x.com>"), format!("Topic {n}")),
        };
        let mut labels = vec!["INBOX"];
        if n < 3 {
            labels.push("UNREAD");
        }
        if n == 5 {
            labels.push("STARRED");
        }
        messages.insert(
            id.clone(),
            json!({
                "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
                "internalDate": (BASE_MS - n * 3_600_000).to_string(), "sizeEstimate": 100,
                "payload": { "mimeType": if n == 0 { "multipart/mixed" } else { "text/plain" }, "headers": [
                    header("From", &from), header("To", "Me <me@gmail.com>"), header("Subject", &subject),
                    header("Message-ID", &format!("<{id}@x>")),
                ]},
            }),
        );
    }
    messages.insert(
        "s0".into(),
        json!({
            "id": "s0", "threadId": "t-i00", "labelIds": ["SENT"], "internalDate": (BASE_MS + 60_000).to_string(),
            "payload": { "mimeType": "text/plain", "headers": [
                header("From", "me@gmail.com"), header("To", "Ann <ann@x.com>"), header("Subject", "Re: Lunch?"),
                header("Message-ID", "<s0@x>"), header("In-Reply-To", "<i00@x>"),
            ]},
        }),
    );
    messages
}

pub(crate) fn gmail_full(id: &str) -> Value {
    if id == "i00" {
        return json!({ "id": id, "payload": {
            "mimeType": "multipart/mixed", "headers": [header("Cc", "Cy <cy@x.com>")],
            "parts": [
                { "mimeType": "multipart/related", "parts": [
                    { "mimeType": "multipart/alternative", "parts": [
                        { "mimeType": "text/plain", "body": { "data": encode("Hi, lunch at noon?\n\nOn Mon, Bob wrote:\n> old") } },
                        { "mimeType": "text/html", "body": { "data": encode("<p>Hi, lunch <img src=\"cid:logo@x\"></p>") } },
                    ]},
                    { "mimeType": "image/png", "filename": "logo.png", "headers": [header("Content-ID", "<logo@x>")], "body": { "data": encode("png") } },
                ]},
                { "mimeType": "application/pdf", "filename": "menu.pdf", "body": { "attachmentId": "att1" } },
            ],
        }});
    }
    json!({ "id": id, "payload": { "mimeType": "text/plain", "body": { "data": encode(&format!("Body of {id}")) } } })
}

/// Gmail's batch answer for the sub-requests in `request`'s body.
pub(crate) fn gmail_batch(request: &FakeRequest, metadata: &HashMap<String, Value>) -> Reply {
    let text = request.text();
    let mut parts = String::new();
    let mut number = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Content-ID: <m") {
            number = rest.trim_end_matches('>').parse::<usize>().ok();
        }
        let Some(target) = line.strip_prefix("GET ") else { continue };
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let id = path.rsplit('/').next().unwrap();
        let body = if path.contains("/labels/") {
            Some(json!({ "messagesTotal": 10, "messagesUnread": if id == "INBOX" { 3 } else { 0 } }))
        } else if query.contains("format=full") {
            metadata.get(id).map(|_| {
                let files = if id == "i00" { json!([{ "filename": "menu.pdf" }]) } else { json!([]) };
                json!({ "id": id, "payload": { "parts": files } })
            })
        } else {
            metadata.get(id).cloned()
        };
        let status = if body.is_some() { "200 OK" } else { "404 Not Found" };
        parts.push_str(&format!(
            "--resp\r\nContent-Type: application/http\r\nContent-ID: <response-m{}>\r\n\r\nHTTP/1.1 {status}\r\nContent-Type: application/json\r\n\r\n{}\r\n",
            number.unwrap(),
            body.unwrap_or(json!({ "error": { "message": "Not Found" } }))
        ));
    }
    parts.push_str("--resp--\r\n");
    Reply { status: 200, headers: vec![("Content-Type".into(), "multipart/mixed; boundary=resp".into())], body: parts.into_bytes() }
}

pub(crate) struct GmailCounters {
    pub tokens: AtomicUsize,
    pub labels: AtomicUsize,
    pub listings: AtomicUsize,
}

pub(crate) fn gmail_server(counters: Arc<GmailCounters>) -> FakeServer {
    let metadata = gmail_metadata();
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&counters.tokens);
        }
        if path == "/batch/gmail/v1" {
            return gmail_batch(request, &metadata);
        }
        let Some(resource) = path.strip_prefix("/gmail/v1/users/me/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        match resource {
            "profile" => Reply::json(json!({ "emailAddress": "me@gmail.com", "historyId": "100" })),
            "settings/sendAs" => {
                Reply::json(json!({ "sendAs": [{ "sendAsEmail": "me@gmail.com", "displayName": "Me Gmail", "isPrimary": true }] }))
            }
            "labels" => {
                // The first answer refuses the token: the engine drops it and refreshes once.
                if counters.labels.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Reply::status(401, json!({ "error": { "message": "Invalid Credentials" } }));
                }
                Reply::json(json!({ "labels": [
                    { "id": "INBOX", "name": "INBOX", "type": "system" }, { "id": "SENT", "name": "SENT", "type": "system" },
                    { "id": "TRASH", "name": "TRASH", "type": "system" }, { "id": "SPAM", "name": "SPAM", "type": "system" },
                    { "id": "CATEGORY_SOCIAL", "name": "CATEGORY_SOCIAL", "type": "system" },
                    { "id": "Label_1", "name": "Receipts", "type": "user" },
                ]}))
            }
            "history" => Reply::json(json!({ "historyId": "101" })),
            "messages" => {
                // The first listing is throttled once: the engine waits as asked and repeats.
                if counters.listings.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Reply::status(429, json!({ "error": { "message": "Slow down" } })).header("Retry-After", "0");
                }
                let ids: Vec<String> = match (request.param("labelIds"), request.param("q"), request.param("pageToken")) {
                    (Some("INBOX"), _, None) => (0..50).map(|n| format!("i{n:02}")).collect(),
                    (Some("INBOX"), _, Some("p2")) => (50..60).map(|n| format!("i{n:02}")).collect(),
                    (Some("SENT"), _, _) => vec!["s0".into()],
                    (None, Some(_), _) => vec!["i10".into(), "s0".into()],
                    _ => vec![],
                };
                let next = (request.param("labelIds") == Some("INBOX") && request.param("pageToken").is_none()).then_some("p2");
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                Reply::json(json!({ "messages": items, "nextPageToken": next, "resultSizeEstimate": ids.len() }))
            }
            "messages/i00/attachments/att1" => Reply::json(json!({ "data": encode("%PDF") })),
            other => match other.strip_prefix("messages/") {
                Some(id) if request.param("format") == Some("full") => Reply::json(gmail_full(id)),
                _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            },
        }
    })
}

pub(crate) fn ids(listing: &Value) -> Vec<String> {
    listing["conversations"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn lists_shows_and_searches_gmail_and_serves_the_cache_without_asking_again() {
    let counters = Arc::new(GmailCounters {
        tokens: AtomicUsize::new(0),
        labels: AtomicUsize::new(0),
        listings: AtomicUsize::new(0),
    });
    let server = gmail_server(counters.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    wait_until("mailboxes", || {
        call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"].as_array().is_some_and(|boxes| !boxes.is_empty())
    });
    // A 401 dropped the access token and the engine refreshed once more.
    assert_eq!(counters.tokens.load(Ordering::SeqCst), 2);
    let boxes = call(&engine, "mailboxes", json!({ "account": "ME@gmail.com" })).unwrap();
    let roles: Vec<&str> = boxes["me@gmail.com"].as_array().unwrap().iter().map(|b| b["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["inbox", "sent", "all", "junk", "trash", "other"]);
    assert_eq!(boxes["me@gmail.com"][5]["name"], "Receipts");
    wait_until("counts", || call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"][0]["unread"] == json!(3));

    let first = call(&engine, "mail.list", json!({ "limit": 25 })).unwrap();
    let conversations = first["conversations"].as_array().unwrap();
    assert_eq!(conversations.len(), 25);
    assert_eq!(first["failures"], json!([]));
    let lunch = &conversations[0];
    assert_eq!(lunch["id"], "me@gmail.com|i00@x");
    assert_eq!(lunch["subject"], "Lunch?");
    assert_eq!(lunch["messageCount"], 2);
    assert_eq!(lunch["participants"], json!(["Ann", "me"]));
    assert_eq!(lunch["from"], "Ann <ann@x.com>");
    assert_eq!(lunch["mailboxes"], json!(["inbox", "sent"]));
    assert_eq!((lunch["unread"].clone(), lunch["hasAttachment"].clone()), (json!(true), json!(true)));
    assert_eq!(lunch["date"], crate::model::iso8601(crate::model::date_from_unix((BASE_MS + 60_000) as f64 / 1000.0)));
    // One sender's repeated alert shows as one row.
    let alert = conversations.iter().find(|c| c["subject"] == "Security alert").unwrap();
    assert_eq!(alert["messageCount"], 2);
    assert!(conversations.iter().any(|c| c["starred"] == json!(true)));

    // Paging reaches the second page of the provider's listing.
    let mut all = ids(&first);
    let mut next = first["next"].as_str().map(String::from);
    while let Some(cursor) = next {
        let page = call(&engine, "mail.list", json!({ "limit": 25, "cursor": cursor })).unwrap();
        all.extend(ids(&page));
        next = page["next"].as_str().map(String::from);
    }
    assert_eq!(all.len(), 59);
    assert_eq!(all.iter().collect::<std::collections::HashSet<_>>().len(), 59);
    let unread = call(&engine, "mail.list", json!({ "unread": true, "limit": 50 })).unwrap();
    // The merged alert takes its oldest message's id, as Swift's `Conversation(thread:)` does.
    assert_eq!(ids(&unread), ["me@gmail.com|i00@x", "me@gmail.com|i02@x"]);

    // What is held serves the list without a request.
    let before = server.count();
    let cached = call(&engine, "mail.list", json!({ "cached": true, "limit": 25 })).unwrap();
    assert_eq!(ids(&cached), ids(&first));
    assert_eq!(server.count(), before);

    let shown = call(&engine, "mail.show", json!({ "id": "me@gmail.com|i00@x", "html": true, "max-chars": 10 })).unwrap();
    let messages = shown["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let first_message = &messages[0];
    assert_eq!(first_message["id"], "i00");
    assert_eq!(first_message["text"], "Hi, lunch ");
    assert_eq!(first_message["truncated"], true);
    assert_eq!(first_message["cc"], json!(["Cy <cy@x.com>"]));
    let html = first_message["html"].as_str().unwrap();
    assert!(html.starts_with("<p>Hi, lunch <img src=\"file://") && html.contains("/messages-v2/") && html.ends_with("/0-logo.png\"></p>"), "{html}");
    let attachments = first_message["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!((attachments[0]["filename"].as_str(), attachments[0]["mime"].as_str()), (Some("menu.pdf"), Some("application/pdf")));
    assert_eq!(std::fs::read(attachments[0]["path"].as_str().unwrap()).unwrap(), b"%PDF");
    assert_eq!(messages[1]["text"], "Body of s0");
    assert_eq!(messages[1]["role"], "sent");

    let previews =
        call(&engine, "mail.previews", json!({ "ids": ["me@gmail.com|i00@x", "me@gmail.com|i03@x"] })).unwrap();
    assert_eq!(previews, json!({ "me@gmail.com|i00@x": "Body of s0", "me@gmail.com|i03@x": "Body of i03" }));

    let found = call(&engine, "mail.search", json!({ "query": "lunch OR topic" })).unwrap();
    // Inbox and sent are both hit, so no context loads (as in Swift): the reply stands alone.
    assert_eq!(ids(&found), ["me@gmail.com|s0@x", "me@gmail.com|i10@x"]);
    assert!(found.get("next").is_none());

    assert_eq!(call(&engine, "mail.show", json!({ "id": "nope" })).unwrap_err().code, "unknown_conversation");
    assert_eq!(call(&engine, "mail.list", json!({ "cursor": "x" })).unwrap_err().code, "usage");
    assert_eq!(call(&engine, "mail.list", json!({ "mailbox": "Nope" })).unwrap_err().code, "unknown_mailbox");
    assert_eq!(call(&engine, "mail.list", json!({ "account": "x@y.z" })).unwrap_err().code, "unknown_account");
    assert_eq!(call(&engine, "mail.previews", json!({ "ids": [] })).unwrap_err().code, "usage");
    let receipts = call(&engine, "mail.list", json!({ "mailbox": "receipts", "account": "me@gmail.com" })).unwrap();
    assert_eq!(receipts["conversations"], json!([]));
    std::thread::sleep(Duration::from_millis(2500));
    let envelope_cache = gmail::cache_file(&config.cache_directory, "me@gmail.com");
    assert!(envelope_cache.exists(), "the envelope cache was saved");
    drop(engine);

    // Next launch, offline: mail.db and the content cache answer.
    let mut offline = config.clone();
    offline.endpoints = crate::tests::config(OAuthConfig::default()).endpoints;
    offline.oauth.token_endpoint = Some("http://127.0.0.1:9/token".into());
    let engine = Engine::start(offline);
    wait_until("restored mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"].is_array());
    let again = call(&engine, "mail.list", json!({ "cached": true, "limit": 25 })).unwrap();
    assert_eq!(ids(&again), ids(&first));
    let shown_again = call(&engine, "mail.show", json!({ "id": "me@gmail.com|i00@x" })).unwrap();
    assert_eq!(shown_again["messages"][0]["text"], "Hi, lunch at noon?\n\nOn Mon, Bob wrote:\n> old");
    assert_eq!(shown_again["messages"][0]["attachments"][0]["filename"], "menu.pdf");
    assert_eq!(call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"][0]["unread"], 3);
    call(&engine, "accounts.remove", json!({ "account": "me@gmail.com", "yes": true })).unwrap();
    assert!(!envelope_cache.exists(), "signing out drops the envelope cache");
    assert_eq!(call(&engine, "mailboxes", json!({})).unwrap(), json!({}));
    drop(engine);
    cleanup(&config);
}

// MARK: - Graph

pub(crate) fn graph_message(id: &str, conversation: &str, from: (&str, &str), subject: &str, minutes: i64) -> Value {
    let date = crate::model::iso8601(crate::model::date_from_unix((BASE_MS / 1000 + minutes * 60) as f64));
    json!({
        "id": id, "conversationId": conversation, "internetMessageId": format!("<{id}@outlook>"), "subject": subject,
        "from": { "emailAddress": { "name": from.0, "address": from.1 } },
        "toRecipients": [{ "emailAddress": { "name": "Me", "address": "me@outlook.com" } }],
        "receivedDateTime": date, "isRead": id != "g3", "flag": { "flagStatus": "notFlagged" }, "isDraft": false,
        "hasAttachments": id == "g2",
    })
}

fn graph_server(tokens: Arc<AtomicUsize>) -> FakeServer {
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        assert!(request.header("Prefer").is_some_and(|prefer| prefer.contains("IdType=\"ImmutableId\"")), "{path}");
        assert_eq!(request.method, "GET", "receiving mail only reads: {path}");
        let Some(resource) = path.strip_prefix("/v1.0/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        let folder = |id: &str, name: &str, unread: i64, children: i64| {
            json!({ "id": id, "displayName": name, "childFolderCount": children, "unreadItemCount": unread, "totalItemCount": 9 })
        };
        let well_known = [
            ("inbox", "F-inbox"), ("sentitems", "F-sent"), ("drafts", "F-drafts"), ("deleteditems", "F-trash"),
            ("junkemail", "F-junk"), ("archive", "F-archive"), ("conversationhistory", "F-history"),
        ];
        match resource {
            "me" => Reply::json(json!({ "displayName": "Me Outlook", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [
                folder("F-inbox", "Inbox", 2, 1), folder("F-sent", "Sent Items", 0, 0), folder("F-drafts", "Drafts", 0, 0),
                folder("F-trash", "Deleted Items", 0, 0), folder("F-junk", "Junk Email", 0, 0),
                folder("F-archive", "Archive", 0, 0), folder("F-history", "Conversation History", 0, 0),
            ]})),
            "me/mailFolders/F-inbox/childFolders" => Reply::json(json!({ "value": [folder("F-projects", "Projects", 0, 0)] })),
            "me/mailFolders/F-inbox/messages" => Reply::json(json!({ "value": [
                graph_message("g3", "c1", ("Bob", "bob@x.com"), "RE: Plan", 30),
                graph_message("g2", "c2", ("Carol", "carol@x.com"), "Photos", 20),
                graph_message("g1", "c1", ("Bob", "bob@x.com"), "Plan", 0),
            ]})),
            "me/mailFolders/F-sent/messages" => Reply::json(json!({ "value": [
                graph_message("g4", "c1", ("Me", "me@outlook.com"), "RE: Plan", 10),
            ]})),
            "me/messages" => {
                assert_eq!(request.param("$search"), Some("\"photos\""));
                let mut hit = graph_message("g2", "c2", ("Carol", "carol@x.com"), "Photos", 20);
                hit["parentFolderId"] = json!("F-inbox");
                let mut junk = graph_message("g9", "c9", ("Spam", "spam@x.com"), "Photos!!", 25);
                junk["parentFolderId"] = json!("F-junk");
                Reply::json(json!({ "value": [hit, junk] }))
            }
            "me/messages/g2" => Reply::json(json!({
                "body": { "contentType": "html", "content": "<p>See <img src=\"cid:img1\"></p>" },
                "ccRecipients": [], "replyTo": [], "hasAttachments": true,
                "internetMessageHeaders": [{ "name": "References", "value": "<g1@outlook>" }],
            })),
            "me/messages/g2/attachments" => Reply::json(json!({ "value": [
                { "@odata.type": "#microsoft.graph.fileAttachment", "id": "a1", "name": "photo.jpg",
                  "contentType": "image/jpeg", "contentId": "img1", "isInline": true, "contentBytes": STANDARD.encode("jpg") },
                { "@odata.type": "#microsoft.graph.itemAttachment", "id": "a2", "name": "Fwd" },
            ]})),
            "me/messages/g2/attachments/a2" => Reply::json(json!({ "item": {
                "subject": "Old photos", "from": { "emailAddress": { "name": "Dan", "address": "dan@x.com" } },
                "body": { "contentType": "text", "content": "Inner text" },
            }})),
            other => {
                if let Some(name) = other.strip_prefix("me/mailFolders/")
                    && let Some((_, id)) = well_known.iter().find(|(known, _)| *known == name)
                {
                    return Reply::json(json!({ "id": id }));
                }
                if let Some(id) = other.strip_prefix("me/messages/") {
                    return Reply::json(json!({ "body": { "contentType": "text", "content": format!("Text of {id}") } }));
                }
                if other.starts_with("me/mailFolders/") && other.ends_with("/messages") {
                    return Reply::json(json!({ "value": [] }));
                }
                Reply::status(404, json!({ "error": { "message": "Not found" } }))
            }
        }
    })
}

#[test]
fn lists_shows_and_searches_outlook() {
    let tokens = Arc::new(AtomicUsize::new(0));
    let server = graph_server(tokens.clone());
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    wait_until("mailboxes", || {
        call(&engine, "mailboxes", json!({})).unwrap()["me@outlook.com"].as_array().is_some_and(|boxes| !boxes.is_empty())
    });
    let boxes = call(&engine, "mailboxes", json!({})).unwrap();
    let names: Vec<(&str, &str)> = boxes["me@outlook.com"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["role"].as_str().unwrap(), b["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        names,
        [("inbox", "Inbox"), ("sent", "Sent"), ("drafts", "Drafts"), ("archive", "Archive"), ("junk", "Spam"), ("trash", "Trash"), ("other", "Inbox/Projects")]
    );
    let listing = call(&engine, "mail.list", json!({})).unwrap();
    assert_eq!(ids(&listing), ["me@outlook.com|g1@outlook", "me@outlook.com|g2@outlook"]);
    let plan = &listing["conversations"][0];
    assert_eq!((plan["messageCount"].clone(), plan["unread"].clone()), (json!(3), json!(true)));
    assert_eq!(plan["participants"], json!(["Bob", "me"]));
    assert_eq!(listing["next"], Value::Null);

    let shown = call(&engine, "mail.show", json!({ "id": "me@outlook.com|g2@outlook", "html": true })).unwrap();
    let message = &shown["messages"][0];
    let html = message["html"].as_str().unwrap();
    assert!(html.starts_with("<p>See <img src=\"file://") && html.contains("0-photo.jpg"), "{html}");
    assert!(html.ends_with(
        "<div class=\"ms-attached-message\"><div class=\"ms-attached-headline\">Attached message from &quot;Dan&quot; &lt;dan@x.com&gt;: Old photos</div><div class=\"ms-plain\">Inner text\n</div></div>"
    ));
    assert_eq!(message["attachments"], json!([]));
    assert_eq!(call(&engine, "mail.previews", json!({ "ids": ["me@outlook.com|g1@outlook"] })).unwrap()["me@outlook.com|g1@outlook"], "Text of g3");

    let found = call(&engine, "mail.search", json!({ "query": "photos" })).unwrap();
    assert_eq!(ids(&found), ["me@outlook.com|g2@outlook"]);
    drop(engine);
    cleanup(&config);
}
