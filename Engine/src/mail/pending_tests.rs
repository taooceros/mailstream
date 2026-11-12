//! Mail sent from here lists at once and once (MS-291): a reply joins its conversation and a new
//! message lists in the inbox while the provider is still sending; the provider's copy takes its
//! place when Sent lists it (Gmail's by the id its send answered, Outlook's by Message-ID or, when
//! Outlook wrote its own, by subject and people); a failed send leaves the lists again.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::mail_tests::{BASE_MS, gmail_batch, gmail_full, graph_message, header};
use crate::model::{iso8601, now};
use crate::testkit::{FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};

const LUNCH: &str = "me@gmail.com|i00@x";

/// The conversations the default list holds now, without a request; none before the mailboxes
/// are known.
fn held(engine: &crate::Engine) -> Vec<Value> {
    match call(engine, "mail.list", json!({ "cached": true, "limit": 50 })) {
        Ok(listing) => listing["conversations"].as_array().unwrap().clone(),
        Err(error) if error.code == "unknown_mailbox" => Vec::new(),
        Err(error) => panic!("{error:?}"),
    }
}

fn find<'a>(conversations: &'a [Value], subject: &str) -> Vec<&'a Value> {
    conversations.iter().filter(|c| c["subject"] == subject).collect()
}

fn message_ids(shown: &Value) -> Vec<String> {
    shown["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect()
}

/// A fake Gmail with two inbox messages and an empty Sent. Sends answer after `delay` with the
/// copy's id and thread (`g-sent-<n>`), and `refuse` makes them fail; Sent lists the copies only
/// once `synced` is set, under a Message-ID Gmail wrote itself.
struct Gmail {
    server: FakeServer,
    synced: Arc<AtomicBool>,
    refuse: Arc<AtomicBool>,
}

fn gmail(delay: Duration) -> Gmail {
    let metadata: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
    for (n, from, subject) in [(0, "Ann <ann@x.com>", "Lunch?"), (1, "Bob <bob@x.com>", "Report")] {
        let id = format!("i{n:02}");
        metadata.lock().unwrap().insert(
            id.clone(),
            json!({
                "id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX"],
                "internalDate": (BASE_MS - n * 3_600_000).to_string(),
                "payload": { "mimeType": "text/plain", "headers": [
                    header("From", from), header("To", "Me <me@gmail.com>"), header("Subject", subject),
                    header("Message-ID", &format!("<{id}@x>")),
                ]},
            }),
        );
    }
    let copies: Arc<Mutex<Vec<String>>> = Arc::default();
    let (synced, refuse) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (listed, refused) = (synced.clone(), refuse.clone());
    let tokens = AtomicUsize::new(0);
    let server = FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if path == "/batch/gmail/v1" {
            return gmail_batch(request, &metadata.lock().unwrap());
        }
        if path == "/upload/gmail/v1/users/me/messages/send" {
            if refused.load(Ordering::SeqCst) {
                return Reply::status(400, json!({ "error": { "message": "Recipient address rejected" } })).after(delay);
            }
            let raw = request.text();
            let field = |name: &str| {
                raw.lines().find_map(|line| line.strip_prefix(&format!("{name}: "))).map(str::to_string).unwrap_or_default()
            };
            let thread = raw.split("\"threadId\":\"").nth(1).and_then(|rest| rest.split('"').next()).map(str::to_string);
            let mut copies = copies.lock().unwrap();
            let id = format!("g-sent-{}", copies.len() + 1);
            let thread = thread.unwrap_or_else(|| format!("t-{id}"));
            let mut headers = vec![
                header("From", "Me Gmail <me@gmail.com>"), header("To", &field("To")), header("Subject", &field("Subject")),
                header("Message-ID", &format!("<{id}@mail.gmail.com>")),
            ];
            let in_reply_to = field("In-Reply-To");
            if !in_reply_to.is_empty() {
                headers.push(header("In-Reply-To", &in_reply_to));
            }
            metadata.lock().unwrap().insert(
                id.clone(),
                json!({
                    "id": id, "threadId": thread, "labelIds": ["SENT"],
                    "internalDate": ((crate::model::unix_seconds(now()) * 1000.0) as i64).to_string(),
                    "payload": { "mimeType": "text/plain", "headers": headers },
                }),
            );
            copies.push(id.clone());
            return Reply::json(json!({ "id": id, "threadId": thread, "labelIds": ["SENT"] })).after(delay);
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
            "messages/i00/attachments/att1" => Reply::json(json!({ "data": crate::mail_tests::encode("%PDF") })),
            "messages" => {
                let ids: Vec<String> = match request.param("labelIds") {
                    Some("INBOX") => vec!["i00".into(), "i01".into()],
                    Some("SENT") if listed.load(Ordering::SeqCst) => copies.lock().unwrap().iter().rev().cloned().collect(),
                    _ => Vec::new(),
                };
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                Reply::json(json!({ "messages": items, "resultSizeEstimate": ids.len() }))
            }
            other => match other.strip_prefix("messages/") {
                Some(id) if request.param("format") == Some("full") => Reply::json(gmail_full(id)),
                _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            },
        }
    });
    Gmail { server, synced, refuse }
}

#[test]
fn gmail_reply_and_new_mail_list_at_once_and_once_after_sync() {
    let fake = gmail(Duration::from_millis(800));
    let (engine, config) = engine_for(&fake.server, &[("me@gmail.com", "google")]);
    wait_until("the inbox", || find(&held(&engine), "Lunch?").len() == 1);
    call(&engine, "mail.list", json!({})).unwrap();
    assert_eq!(find(&held(&engine), "Lunch?")[0]["messageCount"], 1);

    // The reply joins its conversation while Gmail is still sending, the user's own.
    let engine = Arc::new(engine);
    let replying = {
        let engine = engine.clone();
        std::thread::spawn(move || call(&engine, "mail.reply", json!({ "id": LUNCH, "body": "See you at noon", "yes": true })))
    };
    wait_until("the reply in its conversation", || find(&held(&engine), "Lunch?")[0]["messageCount"] == 2);
    assert!(!replying.is_finished(), "the reply listed before the send finished");
    let lunch = find(&held(&engine), "Lunch?")[0].clone();
    assert_eq!((lunch["fromMe"].clone(), lunch["from"].clone()), (json!(true), json!("Me Gmail <me@gmail.com>")));
    assert_eq!(lunch["snippet"], "See you at noon");
    let shown = call(&engine, "mail.show", json!({ "id": LUNCH })).unwrap();
    let reply = &shown["messages"][1];
    assert_eq!((reply["role"].clone(), reply["unread"].clone()), (json!("sent"), json!(false)));
    assert!(reply["text"].as_str().unwrap().starts_with("See you at noon"), "{reply}");
    replying.join().unwrap().unwrap();

    // A new message lists in the inbox as a conversation the user started, at once.
    call(&engine, "mail.send", json!({ "from": "me@gmail.com", "to": "cy@x.com", "subject": "Plans", "body": "Friday?", "yes": true }))
        .unwrap();
    let conversations = held(&engine);
    let plans = find(&conversations, "Plans");
    assert_eq!(plans.len(), 1);
    assert_eq!((plans[0]["fromMe"].clone(), plans[0]["started"].clone()), (json!(true), json!(true)));
    assert_eq!(plans[0]["mailboxes"], json!(["sent"]));

    // Sent doesn't list the copies yet: what was sent stays listed, once.
    std::thread::sleep(Duration::from_millis(2500));
    let conversations = held(&engine);
    assert_eq!(find(&conversations, "Lunch?")[0]["messageCount"], 2);
    assert_eq!(find(&conversations, "Plans").len(), 1);

    // Sent lists them (under Message-IDs of Gmail's own): the copies take their places.
    fake.synced.store(true, Ordering::SeqCst);
    call(&engine, "refresh", json!({})).unwrap();
    // A listing hands out the conversations as they are now; `mail.show` reads those.
    wait_until("the copies", || {
        held(&engine);
        let shown = call(&engine, "mail.show", json!({ "id": LUNCH })).unwrap();
        message_ids(&shown) == ["i00", "g-sent-1"]
    });
    let conversations = held(&engine);
    assert_eq!(find(&conversations, "Lunch?")[0]["messageCount"], 2);
    let plans = find(&conversations, "Plans");
    assert_eq!(plans.len(), 1, "{conversations:?}");
    assert_eq!(plans[0]["started"], true);
    let shown = call(&engine, "mail.show", json!({ "id": plans[0]["id"] })).unwrap();
    assert_eq!(message_ids(&shown), ["g-sent-2"]);
    drop(engine);
    cleanup(&config);
}

#[test]
fn a_failed_send_leaves_the_conversation_and_answers_the_error() {
    let fake = gmail(Duration::from_millis(800));
    fake.refuse.store(true, Ordering::SeqCst);
    let (engine, config) = engine_for(&fake.server, &[("me@gmail.com", "google")]);
    wait_until("the inbox", || find(&held(&engine), "Lunch?").len() == 1);
    let engine = Arc::new(engine);
    let replying = {
        let engine = engine.clone();
        std::thread::spawn(move || call(&engine, "mail.reply", json!({ "id": LUNCH, "body": "See you", "yes": true })))
    };
    wait_until("the reply in its conversation", || find(&held(&engine), "Lunch?")[0]["messageCount"] == 2);
    let error = replying.join().unwrap().unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("failed", "Recipient address rejected"));
    assert_eq!(find(&held(&engine), "Lunch?")[0]["messageCount"], 1);
    assert_eq!(message_ids(&call(&engine, "mail.show", json!({ "id": LUNCH })).unwrap()), ["i00"]);

    let error = call(&engine, "mail.send", json!({ "from": "me@gmail.com", "to": "cy@x.com", "subject": "Plans", "body": "Hi", "yes": true }))
        .unwrap_err();
    assert_eq!(error.message, "Recipient address rejected");
    assert!(find(&held(&engine), "Plans").is_empty());
    drop(engine);
    cleanup(&config);
}

#[test]
fn outlook_copies_replace_what_was_sent_by_message_id_or_subject_and_people() {
    let synced = Arc::new(AtomicBool::new(false));
    let copies: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (listed, filed) = (synced.clone(), copies.clone());
    let tokens = AtomicUsize::new(0);
    let server = FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if request.method == "POST" && path == "/v1.0/me/sendMail" {
            let mime = String::from_utf8(STANDARD.decode(&request.body).unwrap()).unwrap();
            let field = |name: &str| mime.lines().find_map(|line| line.strip_prefix(&format!("{name}: "))).unwrap_or("").to_string();
            let mut copies = filed.lock().unwrap();
            let n = copies.len() + 1;
            let reply = !field("In-Reply-To").is_empty();
            // Outlook keeps the reply's Message-ID and writes its own for the new message.
            let message_id = if reply { field("Message-ID") } else { format!("<outlook-{n}@outlook>") };
            copies.push(json!({
                "id": format!("g-sent-{n}"), "conversationId": if reply { "c1".to_string() } else { format!("c-new-{n}") },
                "internetMessageId": message_id, "subject": field("Subject"),
                "from": { "emailAddress": { "name": "Me Outlook", "address": "me@outlook.com" } },
                "toRecipients": [{ "emailAddress": { "address": field("To") } }],
                "receivedDateTime": iso8601(now()), "sentDateTime": iso8601(now()), "isRead": true,
                "flag": { "flagStatus": "notFlagged" }, "isDraft": false, "hasAttachments": false,
            }));
            return Reply::bytes(202, Vec::new());
        }
        if request.method == "POST" {
            // Nobody else wrote in any conversation asked about.
            let batch: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let responses: Vec<Value> = batch["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|sub| json!({ "id": sub["id"], "status": 200, "body": { "value": [] } }))
                .collect();
            return Reply::json(json!({ "responses": responses }));
        }
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 3 });
        match path.strip_prefix("/v1.0/").unwrap_or(path) {
            "me" => Reply::json(json!({ "displayName": "Me Outlook", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items")] })),
            "me/mailFolders/F-inbox/messages" => Reply::json(json!({ "value": [graph_message("g1", "c1", ("Bob", "bob@x.com"), "Plan", 0)] })),
            "me/mailFolders/F-sent/messages" => {
                let value: Vec<Value> =
                    if listed.load(Ordering::SeqCst) { copies.lock().unwrap().iter().rev().cloned().collect() } else { Vec::new() };
                Reply::json(json!({ "value": value }))
            }
            other => {
                let well_known = [("inbox", "F-inbox"), ("sentitems", "F-sent")];
                if let Some(name) = other.strip_prefix("me/mailFolders/")
                    && let Some((_, id)) = well_known.iter().find(|(known, _)| *known == name)
                {
                    return Reply::json(json!({ "id": id }));
                }
                if let Some(id) = other.strip_prefix("me/messages/") {
                    return Reply::json(json!({ "body": { "contentType": "text", "content": format!("Text of {id}") } }));
                }
                Reply::status(404, json!({ "error": { "message": "Not found" } }))
            }
        }
    });
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    let plan = "me@outlook.com|g1@outlook";
    wait_until("the inbox", || find(&held(&engine), "Plan").len() == 1);

    call(&engine, "mail.reply", json!({ "id": plan, "body": "Agreed", "yes": true })).unwrap();
    call(&engine, "mail.send", json!({ "from": "me@outlook.com", "to": "cy@x.com", "subject": "Dinner", "body": "Tonight?", "yes": true }))
        .unwrap();
    let conversations = held(&engine);
    assert_eq!(find(&conversations, "Plan")[0]["messageCount"], 2);
    assert_eq!(find(&conversations, "Dinner").len(), 1);
    assert_eq!(find(&conversations, "Dinner")[0]["started"], true);

    synced.store(true, Ordering::SeqCst);
    call(&engine, "refresh", json!({})).unwrap();
    // A listing hands out the conversations as they are now; `mail.show` reads those.
    wait_until("the copies", || {
        held(&engine);
        let shown = call(&engine, "mail.show", json!({ "id": plan })).unwrap();
        message_ids(&shown) == ["g1", "g-sent-1"]
    });
    let conversations = held(&engine);
    let dinner = find(&conversations, "Dinner");
    assert_eq!(dinner.len(), 1, "{conversations:?}");
    assert_eq!(dinner[0]["started"], true);
    assert_eq!(message_ids(&call(&engine, "mail.show", json!({ "id": dinner[0]["id"] })).unwrap()), ["g-sent-2"]);
    assert_eq!(find(&conversations, "Plan")[0]["messageCount"], 2);
    drop(engine);
    cleanup(&config);
}
