//! Auto-read end to end: an engine in a throwaway folder against a fake Gmail that records every
//! change asked of it, with tags set through `mail.tag` (no omp runs). Nothing reaches a real
//! provider.
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mail_tests::{gmail_batch, header};
use crate::testkit::{FakeRequest, FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};

type Changes = Arc<Mutex<Vec<FakeRequest>>>;

fn changes(changes: &Changes) -> Vec<Value> {
    std::mem::take(&mut *changes.lock().unwrap()).iter().map(|request| serde_json::from_slice(&request.body).unwrap()).collect()
}

/// The inbox: `(id, sender, subject, days old, unread)`.
const INBOX: [(&str, &str, &str, f64, bool); 6] = [
    ("n1", "Shop <deals@shop.com>", "Weekend sale", 0.1, true),
    ("n2", "Shop <deals@shop.com>", "Flash sale", 1.0, true),
    ("a1", "Bank <alerts@bank.com>", "Statement ready", 5.0, true),
    ("a2", "GitHub <noreply@github.com>", "Review requested", 4.5, true),
    ("p1", "Ann <ann@x.com>", "Dinner", 4.0, true),
    ("a3", "Old <old@x.com>", "Receipt", 6.0, false),
];

fn gmail_server(changed: Changes) -> FakeServer {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as f64;
    let metadata: HashMap<String, Value> = INBOX
        .iter()
        .map(|(id, from, subject, days, unread)| {
            let labels = if *unread { json!(["INBOX", "UNREAD"]) } else { json!(["INBOX"]) };
            let value = json!({
                "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
                "internalDate": ((now_ms - days * 86_400_000.0) as i64).to_string(), "sizeEstimate": 100,
                "payload": { "mimeType": "text/plain", "headers": [
                    header("From", from), header("To", "Me <me@gmail.com>"), header("Subject", subject),
                    header("Message-ID", &format!("<{id}@x>")),
                ]},
            });
            (id.to_string(), value)
        })
        .collect();
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if path == "/batch/gmail/v1" {
            return gmail_batch(request, &metadata);
        }
        if request.method == "POST" {
            changed.lock().unwrap().push(request.clone());
            return Reply::json(json!({}));
        }
        let Some(resource) = path.strip_prefix("/gmail/v1/users/me/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        match resource {
            "profile" => Reply::json(json!({ "emailAddress": "me@gmail.com", "historyId": "100" })),
            "settings/sendAs" => {
                Reply::json(json!({ "sendAs": [{ "sendAsEmail": "me@gmail.com", "displayName": "Me Gmail", "isPrimary": true }] }))
            }
            "labels" => Reply::json(json!({ "labels": [
                { "id": "INBOX", "name": "INBOX", "type": "system" }, { "id": "SENT", "name": "SENT", "type": "system" },
                { "id": "TRASH", "name": "TRASH", "type": "system" },
            ]})),
            "history" => Reply::json(json!({ "historyId": "101" })),
            "messages" => {
                let ids: Vec<&str> =
                    if request.param("labelIds") == Some("INBOX") { INBOX.iter().map(|entry| entry.0).collect() } else { vec![] };
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                Reply::json(json!({ "messages": items, "resultSizeEstimate": ids.len() }))
            }
            _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
        }
    })
}

fn id(message: &str) -> String {
    format!("me@gmail.com|{message}@x")
}

/// The unread conversations as kept; none while the account is still loading.
fn unread(engine: &crate::Engine) -> Vec<String> {
    let Ok(listing) = call(engine, "mail.list", json!({ "cached": true, "unread": true, "limit": 20 })) else { return vec![] };
    listing["conversations"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn marks_stale_notifications_and_newsletters_read_and_undoes() {
    let changed: Changes = Arc::default();
    let server = gmail_server(changed.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let heard = events.clone();
    engine.subscribe(move |event| heard.lock().unwrap().push(event));
    // No omp: tagging fails and leaves the tags below as they are.
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": "/nonexistent/omp" })).unwrap();
    wait_until("the inbox", || unread(&engine).len() == 5);
    for (message, category, waiting) in [
        ("n1", "newsletter", false),
        ("n2", "newsletter", false),
        ("a1", "notification", false),
        ("a2", "notification", true),
        ("p1", "people", false),
        ("a3", "notification", false),
    ] {
        let answer =
            call(&engine, "mail.tag", json!({ "ids": [id(message)], "category": category, "waiting": waiting.to_string() })).unwrap();
        assert_eq!(answer["failed"], json!([]), "{message}");
    }
    // Off by default: nothing is marked.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(changes(&changed), Vec::<Value>::new());
    assert_eq!(call(&engine, "mail.autoread", json!({})).unwrap(), json!({ "enabled": false, "days": 3, "entries": [] }));

    // On: the newsletter a newer one from the same shop replaced, and the 5-day-old bank notice,
    // in one request; the fresh newsletter, the one waiting on the user, people and read mail stay.
    call(&engine, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();
    wait_until("the auto-read request", || !changed.lock().unwrap().is_empty());
    assert_eq!(changes(&changed), [json!({ "ids": ["n2", "a1"], "addLabelIds": [], "removeLabelIds": ["UNREAD"] })]);
    wait_until("the autoRead event", || events.lock().unwrap().contains(&"autoRead"));
    assert_eq!(unread(&engine), [id("n1"), id("p1"), id("a2")]);
    let log = call(&engine, "mail.autoread", json!({})).unwrap();
    assert_eq!((&log["enabled"], &log["days"]), (&json!(true), &json!(3)));
    let entries = log["entries"].as_array().unwrap();
    let summary: Vec<(&str, &str, &str, &str, i64)> = entries
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap(),
                entry["sender"].as_str().unwrap(),
                entry["category"].as_str().unwrap(),
                entry["reason"].as_str().unwrap(),
                entry["batch"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [(id("n2").as_str(), "Shop", "newsletter", "superseded", 1), (id("a1").as_str(), "Bank", "notification", "age", 1)]
    );
    assert_eq!(entries[0]["subject"], "Flash sale");

    // A later pass finds nothing new.
    call(&engine, "config.set", json!({ "key": "autoRead.days", "value": "2" })).unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(changes(&changed), Vec::<Value>::new());

    // Undo puts the latest pass back to unread, and no pass reads them again.
    let undone = call(&engine, "mail.autoread.undo", json!({})).unwrap();
    assert_eq!(undone, json!({ "done": [id("n2"), id("a1")], "failed": [] }));
    assert_eq!(changes(&changed), [json!({ "ids": ["n2", "a1"], "addLabelIds": ["UNREAD"], "removeLabelIds": [] })]);
    assert_eq!(unread(&engine).len(), 5);
    assert_eq!(call(&engine, "mail.autoread", json!({})).unwrap()["entries"], json!([]));
    call(&engine, "config.set", json!({ "key": "autoRead.days", "value": "1" })).unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(changes(&changed), Vec::<Value>::new());
    let answer = call(&engine, "mail.autoread.undo", json!({ "ids": ["nope"] })).unwrap();
    assert_eq!(answer, json!({ "done": [], "failed": [{ "id": "nope", "code": "not_found", "message": "nope was not auto-read." }] }));
    cleanup(&config);
}
