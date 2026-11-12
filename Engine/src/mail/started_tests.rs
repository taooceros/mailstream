//! The user's sent mail in the inbox's conversations (MS-270), end to end against fake Gmail and
//! Graph servers: a reply of theirs is the conversation's latest message, from `me`; a
//! conversation they started lists until someone answers it or they archive it; one answered and
//! archived elsewhere stays out.
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mail_tests::{BASE_MS, gmail_batch, graph_message, header, ids};
use crate::model::{date_from_unix, iso8601};
use crate::testkit::{FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};
use crate::Engine;

/// What the server was asked: Gmail's thread lookups, Graph's conversation filters.
type Asked = Arc<Mutex<Vec<String>>>;

fn date(minutes: i64) -> String {
    iso8601(date_from_unix((BASE_MS / 1000 + minutes * 60) as f64))
}

fn gmail_message(id: &str, thread: &str, labels: &[&str], from: &str, subject: &str, minutes: i64, reply_to: Option<&str>) -> Value {
    let mut headers = vec![
        header("From", from), header("To", "Ann <ann@x.com>"), header("Subject", subject),
        header("Message-ID", &format!("<{id}@x>")),
    ];
    if let Some(parent) = reply_to {
        headers.push(header("In-Reply-To", &format!("<{parent}@x>")));
    }
    json!({
        "id": id, "threadId": thread, "labelIds": labels, "internalDate": (BASE_MS + minutes * 60_000).to_string(),
        "sizeEstimate": 100, "payload": { "mimeType": "text/plain", "headers": headers },
    })
}

/// Inbox: Ann's lunch question (answered by the user since) and Bob's report. Sent: the answer,
/// a kickoff nobody answered, a plan answered and archived since, and a reply in a thread
/// archived since.
fn gmail_server(asked: Asked) -> FakeServer {
    let me = "Me <me@gmail.com>";
    let mut metadata: HashMap<String, Value> = [
        gmail_message("i1", "t1", &["INBOX", "UNREAD"], "Ann <ann@x.com>", "Lunch?", 0, None),
        gmail_message("i2", "t2", &["INBOX"], "Bob <bob@x.com>", "Report", -120, None),
        gmail_message("s1", "t1", &["SENT"], me, "Re: Lunch?", 1, Some("i1")),
        gmail_message("s2", "t5", &["SENT"], me, "Kickoff", 2, None),
        gmail_message("s3", "t6", &["SENT"], me, "Old plan", -60, None),
        gmail_message("s4", "t7", &["SENT"], me, "Re: Gone", 3, Some("gone")),
    ]
    .into_iter()
    .map(|message| (message["id"].as_str().unwrap().to_string(), message))
    .collect();
    // Thread lookups answer by the thread's id, as metadata lookups do by the message's.
    metadata.insert("t5".into(), json!({ "messages": [{ "labelIds": ["SENT"] }] }));
    metadata.insert("t6".into(), json!({ "messages": [{ "labelIds": ["SENT"] }, { "labelIds": ["CATEGORY_PERSONAL"] }] }));
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if path == "/batch/gmail/v1" {
            let text = request.text();
            for line in text.lines().filter_map(|line| line.strip_prefix("GET ")).filter(|line| line.contains("/threads/")) {
                assert!(line.contains("format=minimal"), "{line}");
                let (path, _) = line.split_once('?').unwrap();
                asked.lock().unwrap().push(path.rsplit('/').next().unwrap().to_string());
            }
            return gmail_batch(request, &metadata);
        }
        assert_eq!(request.method, "GET", "nothing moves: {path}");
        let Some(resource) = path.strip_prefix("/gmail/v1/users/me/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        match resource {
            "profile" => Reply::json(json!({ "emailAddress": "me@gmail.com", "historyId": "100" })),
            "settings/sendAs" => {
                Reply::json(json!({ "sendAs": [{ "sendAsEmail": "me@gmail.com", "displayName": "Me", "isPrimary": true }] }))
            }
            "labels" => Reply::json(json!({ "labels": [
                { "id": "INBOX", "name": "INBOX", "type": "system" }, { "id": "SENT", "name": "SENT", "type": "system" },
                { "id": "DRAFT", "name": "DRAFT", "type": "system" }, { "id": "TRASH", "name": "TRASH", "type": "system" },
            ]})),
            "history" => Reply::json(json!({ "historyId": "101" })),
            "messages" => {
                // Sent has a second page, which the inbox's listing reaches.
                let (ids, next): (Vec<&str>, Option<&str>) = match (request.param("labelIds"), request.param("pageToken")) {
                    (Some("INBOX"), _) => (vec!["i1", "i2"], None),
                    (Some("SENT"), None) => (vec!["s4", "s2", "s1"], Some("sent-2")),
                    (Some("SENT"), Some("sent-2")) => (vec!["s3"], None),
                    _ => (vec![], None),
                };
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                Reply::json(json!({ "messages": items, "nextPageToken": next, "resultSizeEstimate": ids.len() }))
            }
            other => match other.strip_prefix("messages/") {
                Some(id) => Reply::json(json!({ "id": id, "payload": {
                    "mimeType": "text/plain", "body": { "data": crate::mail_tests::encode(&format!("Body of {id}")) },
                }})),
                None => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            },
        }
    })
}

fn conversation<'a>(listing: &'a Value, id: &str) -> &'a Value {
    listing["conversations"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("{id} in {listing}"))
}

fn listed_once_asked(engine: &Engine, started: &str) -> Value {
    wait_until("the started conversation lists", || {
        ids(&call(engine, "mail.list", json!({ "cached": true })).unwrap()).contains(&started.to_string())
    });
    call(engine, "mail.list", json!({ "cached": true })).unwrap()
}

#[test]
fn gmail_inbox_lists_the_users_replies_and_the_conversations_they_started() {
    let asked: Asked = Arc::default();
    let server = gmail_server(asked.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    wait_until("mailboxes", || {
        call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"].as_array().is_some_and(|boxes| !boxes.is_empty())
    });
    call(&engine, "mail.list", json!({})).unwrap();
    let listing = listed_once_asked(&engine, "me@gmail.com|s2@x");
    // The kickoff, newest; lunch at the user's answer; Bob's report. The plan answered since and
    // the reply in a thread gone from the inbox stay out.
    assert_eq!(ids(&listing), ["me@gmail.com|s2@x", "me@gmail.com|i1@x", "me@gmail.com|i2@x"]);
    let lunch = conversation(&listing, "me@gmail.com|i1@x");
    assert_eq!(lunch["from"], "Me <me@gmail.com>");
    assert_eq!((lunch["fromMe"].clone(), lunch["started"].clone()), (json!(true), json!(false)));
    assert_eq!(lunch["date"], date(1));
    assert_eq!(lunch["participants"], json!(["Ann", "me"]));
    assert_eq!(lunch["unread"], true);
    let kickoff = conversation(&listing, "me@gmail.com|s2@x");
    assert_eq!((kickoff["fromMe"].clone(), kickoff["started"].clone()), (json!(true), json!(true)));
    assert_eq!(kickoff["participants"], json!(["me"]));
    assert_eq!(kickoff["mailboxes"], json!(["sent"]));
    let report = conversation(&listing, "me@gmail.com|i2@x");
    assert_eq!((report["from"].clone(), report["fromMe"].clone()), (json!("Bob <bob@x.com>"), json!(false)));
    // One lookup of each thread the user started, the one on Sent's second page too; the reply
    // is never asked about.
    let mut threads = asked.lock().unwrap().clone();
    threads.sort();
    assert_eq!(threads, ["t5", "t6"]);

    let shown = call(&engine, "mail.show", json!({ "id": "me@gmail.com|i1@x" })).unwrap();
    let roles: Vec<&str> = shown["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["inbox", "sent"]);
    assert_eq!(shown["fromMe"], true);
    let shown = call(&engine, "mail.show", json!({ "id": "me@gmail.com|s2@x" })).unwrap();
    assert_eq!(shown["messages"][0]["text"], "Body of s2");
    assert_eq!(shown["started"], true);
    // A Sent listing names its mail as sent, not started.
    let sent = call(&engine, "mail.list", json!({ "cached": true, "mailbox": "sent", "account": "me@gmail.com" })).unwrap();
    assert_eq!(conversation(&sent, "me@gmail.com|s2@x")["started"], false);

    // Archive takes the kickoff out of the inbox with nothing to move at Gmail.
    let archived = call(&engine, "mail.archive", json!({ "ids": ["me@gmail.com|s2@x"] })).unwrap();
    assert_eq!(archived, json!({ "done": ["me@gmail.com|s2@x"], "failed": [] }));
    let listing = call(&engine, "mail.list", json!({ "cached": true })).unwrap();
    assert_eq!(ids(&listing), ["me@gmail.com|i1@x", "me@gmail.com|i2@x"]);
    std::thread::sleep(Duration::from_millis(1500));
    drop(engine);

    // Next launch: mail.db knows, so nothing is asked again and the kickoff stays archived.
    let engine = Engine::start(config.clone());
    wait_until("restored mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"].is_array());
    let again = call(&engine, "mail.list", json!({ "cached": true })).unwrap();
    assert_eq!(ids(&again), ["me@gmail.com|i1@x", "me@gmail.com|i2@x"]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(asked.lock().unwrap().len(), 2);
    drop(engine);
    cleanup(&config);
}

/// Inbox: Bob's plan. Sent: the user's answer to it, a kickoff nobody answered, and one answered
/// and archived since.
fn graph_server(asked: Asked) -> FakeServer {
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        let me = ("Me", "me@outlook.com");
        if path == "/v1.0/$batch" {
            let batch: Value = serde_json::from_slice(&request.body).unwrap();
            let responses: Vec<Value> = batch["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|sub| {
                    let url = sub["url"].as_str().unwrap();
                    if sub["method"] == "POST" {
                        assert_eq!(url, "/me/messages/g1/move");
                        return json!({ "id": sub["id"], "status": 201, "body": {} });
                    }
                    let query = url.strip_prefix("/me/messages?").unwrap_or_else(|| panic!("only lookups: {url}"));
                    let filter = query.split('&').find_map(|pair| pair.strip_prefix("$filter=")).unwrap();
                    let filter = percent_encoding::percent_decode_str(filter).decode_utf8().unwrap().to_string();
                    asked.lock().unwrap().push(filter.clone());
                    let folders: &[&str] = match filter.as_str() {
                        "conversationId eq 'c5'" => &["F-sent"],
                        "conversationId eq 'c6'" => &["F-sent", "F-archive"],
                        other => panic!("{other}"),
                    };
                    let value: Vec<Value> = folders.iter().map(|folder| json!({ "parentFolderId": folder })).collect();
                    json!({ "id": sub["id"], "status": 200, "body": { "value": value } })
                })
                .collect();
            return Reply::json(json!({ "responses": responses }));
        }
        let Some(resource) = path.strip_prefix("/v1.0/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 3 });
        let well_known = [
            ("inbox", "F-inbox"), ("sentitems", "F-sent"), ("drafts", "F-drafts"), ("deleteditems", "F-trash"),
            ("junkemail", "F-junk"), ("archive", "F-archive"),
        ];
        match resource {
            "me" => Reply::json(json!({ "displayName": "Me", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [
                folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items"), folder("F-drafts", "Drafts"),
                folder("F-trash", "Deleted Items"), folder("F-junk", "Junk Email"), folder("F-archive", "Archive"),
            ]})),
            "me/mailFolders/F-inbox/messages" => {
                Reply::json(json!({ "value": [graph_message("g1", "c1", ("Bob", "bob@x.com"), "Plan", 0)] }))
            }
            "me/mailFolders/F-sent/messages" => Reply::json(json!({ "value": [
                graph_message("g5", "c5", me, "Kickoff", 20),
                graph_message("g4", "c1", me, "RE: Plan", 10),
                graph_message("g6", "c6", me, "Old plan", -30),
            ]})),
            other => {
                if let Some(name) = other.strip_prefix("me/mailFolders/")
                    && let Some((_, id)) = well_known.iter().find(|(known, _)| *known == name)
                {
                    return Reply::json(json!({ "id": id }));
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
fn outlook_inbox_lists_the_users_replies_and_the_conversations_they_started() {
    let asked: Asked = Arc::default();
    let server = graph_server(asked.clone());
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    wait_until("mailboxes", || {
        call(&engine, "mailboxes", json!({})).unwrap()["me@outlook.com"].as_array().is_some_and(|boxes| !boxes.is_empty())
    });
    call(&engine, "mail.list", json!({})).unwrap();
    let listing = listed_once_asked(&engine, "me@outlook.com|g5@outlook");
    assert_eq!(ids(&listing), ["me@outlook.com|g5@outlook", "me@outlook.com|g1@outlook"]);
    let plan = conversation(&listing, "me@outlook.com|g1@outlook");
    assert_eq!((plan["from"].clone(), plan["fromMe"].clone()), (json!("Me <me@outlook.com>"), json!(true)));
    assert_eq!(plan["date"], date(10));
    assert_eq!(conversation(&listing, "me@outlook.com|g5@outlook")["started"], true);
    let mut filters = asked.lock().unwrap().clone();
    filters.sort();
    assert_eq!(filters, ["conversationId eq 'c5'", "conversationId eq 'c6'"]);

    // Archived, the plan leaves the user's answer behind in Sent (Outlook names no message it
    // answers), which doesn't list as a conversation they started, nor is asked about.
    let archived = call(&engine, "mail.archive", json!({ "ids": ["me@outlook.com|g1@outlook"] })).unwrap();
    assert_eq!(archived["failed"], json!([]));
    let listing = call(&engine, "mail.list", json!({ "cached": true })).unwrap();
    assert_eq!(ids(&listing), ["me@outlook.com|g5@outlook"]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(asked.lock().unwrap().len(), 2);
    drop(engine);
    cleanup(&config);
}
