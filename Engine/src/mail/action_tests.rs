//! Read, star, archive, trash, restore, send, reply and contacts.search end to end: an engine in a
//! throwaway folder against fake Gmail and Graph servers that record every change asked of them.
//! Nothing reaches a real provider.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::mail_tests::{BASE_MS, encode, gmail_batch, gmail_full, gmail_metadata, graph_message, header, ids};
use crate::testkit::{FakeRequest, FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};

/// The requests that change mail, as the server got them.
pub(super) type Changes = Arc<Mutex<Vec<FakeRequest>>>;

pub(super) fn changes(changes: &Changes) -> Vec<FakeRequest> {
    std::mem::take(&mut *changes.lock().unwrap())
}

pub(super) fn json_body(request: &FakeRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// The fake Gmail of `mail_tests` with two messages in the trash, answering label changes,
/// trashing, deleting and sending. A label change touching `i04` is refused.
pub(super) fn gmail_server(changed: Changes) -> FakeServer {
    let mut metadata = gmail_metadata();
    for (id, minutes) in [("t1", 10), ("t2", 20)] {
        metadata.insert(
            id.into(),
            json!({
                "id": id, "threadId": format!("t-{id}"), "labelIds": ["TRASH"],
                "internalDate": (BASE_MS - minutes * 60_000).to_string(),
                "payload": { "mimeType": "text/plain", "headers": [
                    header("From", "Old <old@x.com>"), header("To", "me@gmail.com"), header("Subject", &format!("Old {id}")),
                    header("Message-ID", &format!("<{id}@x>")),
                ]},
            }),
        );
    }
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
            if path.ends_with("/messages/batchModify") && request.text().contains("\"i04\"") {
                return Reply::status(400, json!({ "error": { "message": "Invalid label change" } }));
            }
            return if path.starts_with("/upload/") { Reply::json(json!({ "id": "sent1" })) } else { Reply::json(json!({})) };
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
                { "id": "TRASH", "name": "TRASH", "type": "system" }, { "id": "SPAM", "name": "SPAM", "type": "system" },
            ]})),
            "history" => Reply::json(json!({ "historyId": "101" })),
            "messages" => {
                let ids: Vec<String> = match request.param("labelIds") {
                    Some("INBOX") => (0..6).map(|n| format!("i{n:02}")).collect(),
                    Some("SENT") => vec!["s0".into()],
                    Some("TRASH") => vec!["t1".into(), "t2".into()],
                    _ => vec![],
                };
                let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
                Reply::json(json!({ "messages": items, "resultSizeEstimate": ids.len() }))
            }
            "labels/INBOX" => Reply::json(json!({ "messagesTotal": 6, "messagesUnread": 3 })),
            "messages/i00/attachments/att1" => Reply::json(json!({ "data": encode("%PDF") })),
            other => match other.strip_prefix("messages/") {
                Some(id) if request.param("format") == Some("full") => Reply::json(gmail_full(id)),
                _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            },
        }
    })
}

pub(super) fn conversation<'a>(listing: &'a Value, id: &str) -> &'a Value {
    listing["conversations"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("{id} in {listing}"))
}

#[test]
fn changes_trashes_and_sends_gmail_mail() {
    let changed: Changes = Arc::default();
    let server = gmail_server(changed.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    wait_until("counts", || call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"][0]["unread"] == json!(3));
    let listing = call(&engine, "mail.list", json!({ "limit": 10 })).unwrap();
    let lunch = "me@gmail.com|i00@x";
    assert_eq!(conversation(&listing, lunch)["unread"], true);
    changes(&changed);

    // Read: the unread message loses UNREAD; the list and the inbox count follow at once.
    let read = call(&engine, "mail.read", json!({ "ids": [lunch] })).unwrap();
    assert_eq!(read, json!({ "done": [lunch], "failed": [], "undo": 1 }));
    let sent = changes(&changed);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].path.ends_with("/messages/batchModify"));
    assert_eq!(json_body(&sent[0]), json!({ "ids": ["i00"], "addLabelIds": [], "removeLabelIds": ["UNREAD"] }));
    let cached = call(&engine, "mail.list", json!({ "cached": true, "limit": 10 })).unwrap();
    assert_eq!(conversation(&cached, lunch)["unread"], false);
    assert_eq!(call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"][0]["unread"], 2);

    // Unread marks the latest message the user did not send; starring stars the latest one.
    call(&engine, "mail.read", json!({ "ids": [lunch], "unread": true })).unwrap();
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["i00"], "addLabelIds": ["UNREAD"], "removeLabelIds": [] }));
    call(&engine, "mail.star", json!({ "ids": [lunch] })).unwrap();
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["s0"], "addLabelIds": ["STARRED"], "removeLabelIds": [] }));
    let cached = call(&engine, "mail.list", json!({ "cached": true, "limit": 10 })).unwrap();
    assert_eq!(conversation(&cached, lunch)["starred"], true);
    call(&engine, "mail.star", json!({ "ids": [lunch], "off": true })).unwrap();
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["s0"], "addLabelIds": [], "removeLabelIds": ["STARRED"] }));

    // A refused change is rolled back and listed for every conversation its request carried (one
    // batchModify for all of them, MS-305); unknown ids are listed too.
    let refused = "me@gmail.com|i04@x";
    let answer = call(&engine, "mail.star", json!({ "ids": [refused, "nope", "me@gmail.com|i03@x"] })).unwrap();
    assert_eq!(answer["done"], json!([]));
    assert_eq!(answer["failed"][0], json!({ "id": refused, "code": "failed", "message": "Invalid label change" }));
    assert_eq!(answer["failed"][1]["code"], "unknown_conversation");
    assert_eq!(answer["failed"][2], json!({ "id": "me@gmail.com|i03@x", "code": "failed", "message": "Invalid label change" }));
    assert_eq!(answer.get("undo"), None);
    let cached = call(&engine, "mail.list", json!({ "cached": true, "limit": 10 })).unwrap();
    assert_eq!(conversation(&cached, refused)["starred"], false);
    assert_eq!(conversation(&cached, "me@gmail.com|i03@x")["starred"], false);
    assert_eq!(call(&engine, "mail.read", json!({ "ids": [] })).unwrap_err().code, "usage");
    changes(&changed);

    // Archive takes the inbox message out of the inbox (All Mail stands in for Archive) and leaves
    // the sent reply alone; the conversation leaves the list.
    let archived = call(&engine, "mail.archive", json!({ "ids": [lunch] })).unwrap();
    assert_eq!(archived["done"], json!([lunch]));
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["i00"], "addLabelIds": [], "removeLabelIds": ["INBOX"] }));
    let cached = call(&engine, "mail.list", json!({ "cached": true, "limit": 10 })).unwrap();
    assert!(!ids(&cached).contains(&lunch.to_string()));

    // Trash moves through Gmail's trash call.
    call(&engine, "mail.trash", json!({ "ids": ["me@gmail.com|i05@x"] })).unwrap();
    let sent = changes(&changed);
    assert_eq!((sent[0].path.as_str(), sent.len()), ("/gmail/v1/users/me/messages/i05/trash", 1));

    // Mail already in the trash is deleted for good, only with yes; restoring untrashes and files
    // it in the inbox.
    let trash = call(&engine, "mail.list", json!({ "mailbox": "trash" })).unwrap();
    assert_eq!(ids(&trash), ["me@gmail.com|t1@x", "me@gmail.com|t2@x"]);
    let unconfirmed = call(&engine, "mail.trash", json!({ "ids": ["me@gmail.com|t1@x"] })).unwrap();
    assert_eq!(
        unconfirmed["failed"][0]["message"],
        "me@gmail.com|t1@x is already in the trash, so it would be deleted for good. Re-run with --yes."
    );
    assert_eq!(unconfirmed["failed"][0]["code"], "confirm");
    assert!(changes(&changed).is_empty());
    call(&engine, "mail.trash", json!({ "ids": ["me@gmail.com|t1@x"], "yes": true })).unwrap();
    let sent = changes(&changed);
    assert!(sent[0].path.ends_with("/messages/batchDelete"));
    assert_eq!(json_body(&sent[0]), json!({ "ids": ["t1"] }));
    call(&engine, "mail.restore", json!({ "ids": ["me@gmail.com|t2@x"] })).unwrap();
    let sent = changes(&changed);
    assert_eq!(sent[0].path, "/gmail/v1/users/me/messages/t2/untrash");
    assert_eq!(json_body(&sent[1]), json!({ "ids": ["t2"], "addLabelIds": ["INBOX"], "removeLabelIds": [] }));
    assert_eq!(call(&engine, "mail.list", json!({ "mailbox": "trash", "cached": true })).unwrap()["conversations"], json!([]));

    // Sending needs yes, and says what it would send without it.
    let file = config.support_directory.join("notes.txt");
    std::fs::write(&file, "attached words").unwrap();
    let draft = json!({
        "from": "ME@gmail.com", "to": ["Ann <ann@x.com>"], "cc": "cy@x.com", "subject": "Hello",
        "body": "Hi Ann 👋", "attachments": [file.to_string_lossy()],
    });
    let error = call(&engine, "mail.send", draft.clone()).unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        ("confirm", "Would send \"Hello\" (8 characters, 1 attachment) from me@gmail.com to ann@x.com, cy@x.com. Re-run with --yes.")
    );
    let mut bad = draft.clone();
    bad["to"] = json!(["not an address"]);
    assert_eq!(call(&engine, "mail.send", bad).unwrap_err().message, "--to: \"not an address\" is not an email address.");
    let mut missing = draft.clone();
    missing["attachments"] = json!(["relative.txt"]);
    assert_eq!(call(&engine, "mail.send", missing).unwrap_err().code, "usage");
    let mut confirmed = draft;
    confirmed["yes"] = json!(true);
    let answer = call(&engine, "mail.send", confirmed).unwrap();
    assert_eq!(answer, json!({ "sent": true, "from": "me@gmail.com", "to": ["Ann <ann@x.com>"], "subject": "Hello" }));
    let sent = changes(&changed);
    assert_eq!(sent.len(), 1);
    let upload = &sent[0];
    assert_eq!((upload.path.as_str(), upload.param("uploadType")), ("/upload/gmail/v1/users/me/messages/send", Some("multipart")));
    let content_type = upload.header("Content-Type").unwrap();
    assert!(content_type.starts_with("multipart/related; boundary=ms_upload_"), "{content_type}");
    let body = upload.text();
    assert!(body.contains("Content-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n"), "{body}");
    assert!(body.contains("Content-Type: message/rfc822\r\n\r\nFrom: Me Gmail <me@gmail.com>\r\nTo: Ann <ann@x.com>\r\nCc: cy@x.com\r\nSubject: Hello\r\n"));
    assert!(body.contains("Content-Type: multipart/alternative;"));
    assert!(body.contains("Hi Ann =F0=9F=91=8B") && body.contains(&STANDARD.encode("attached words")));
    assert!(body.contains("Content-Type: text/plain;\r\n name=\"notes.txt\""));
    // A reply goes to the other side of the latest message (here the user's own reply, so its
    // recipient), threaded under it.
    // The fake keeps listing the archived message in the inbox, so it threads with the reply again.
    // Sent lists the message just sent too, until Gmail's copy arrives.
    let sent_listing = call(&engine, "mail.list", json!({ "mailbox": "sent" })).unwrap();
    let subjects: Vec<&str> =
        sent_listing["conversations"].as_array().unwrap().iter().map(|c| c["subject"].as_str().unwrap()).collect();
    assert_eq!(subjects, ["Hello", "Lunch?"]);
    let id = ids(&sent_listing)[1].clone();
    let error = call(&engine, "mail.reply", json!({ "id": id, "body": "See you" })).unwrap_err();
    assert_eq!(error.message, "Would send \"Re: Lunch?\" from me@gmail.com to ann@x.com, 0 attachments. Re-run with --yes.");
    let error = call(&engine, "mail.reply", json!({ "id": id, "body": "x", "to": [], "yes": true })).unwrap_err();
    assert_eq!(error.message, "A reply needs at least one recipient.");
    // A reply can answer a message the conversation holds other than its latest.
    let error = call(&engine, "mail.reply", json!({ "id": id, "message": "nope", "body": "x", "yes": true })).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("usage", "Message nope is not in the conversation."));
    let error = call(&engine, "mail.reply", json!({ "id": id, "message": "s0", "body": "See you" })).unwrap_err();
    assert_eq!(error.message, "Would send \"Re: Lunch?\" from me@gmail.com to ann@x.com, 0 attachments. Re-run with --yes.");
    let answer = call(&engine, "mail.reply", json!({ "id": id, "body": "See you", "yes": true })).unwrap();
    assert_eq!(answer, json!({ "sent": true, "from": "me@gmail.com", "to": ["Ann <ann@x.com>"], "subject": "Re: Lunch?" }));
    let body = changes(&changed)[0].text();
    assert!(body.contains("{\"threadId\":\"t-i00\"}"), "{body}");
    assert!(body.contains("In-Reply-To: <s0@x>\r\nReferences: <s0@x>\r\n"), "{body}");
    assert!(body.contains("See you\r\n\r\nOn ") && body.contains("me@gmail.com wrote:\r\n> Body of s0"), "{body}");
    assert!(body.contains("Content-Type: multipart/alternative;"));

    // Recipients complete from the listings: people the user wrote to first.
    let people = call(&engine, "contacts.search", json!({ "query": "an" })).unwrap();
    assert_eq!(people[0], json!({ "name": "Ann", "email": "ann@x.com" }));
    assert_eq!(call(&engine, "contacts.search", json!({ "query": " " })).unwrap(), json!([]));
    let limited = call(&engine, "contacts.search", json!({ "query": "p", "limit": 2 })).unwrap();
    assert_eq!(limited.as_array().unwrap().len(), 2);
    drop(engine);
    cleanup(&config);
}

#[test]
fn changes_and_sends_outlook_mail_through_batches() {
    let changed: Changes = Arc::default();
    let throttled = Arc::new(AtomicUsize::new(0));
    let tokens = AtomicUsize::new(0);
    let (record, refused) = (changed.clone(), throttled.clone());
    let server = FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        assert!(request.header("Prefer").is_some_and(|prefer| prefer.contains("IdType=\"ImmutableId\"")), "{path}");
        if request.method == "POST" {
            record.lock().unwrap().push(request.clone());
            if path == "/v1.0/$batch" {
                let batch: Value = serde_json::from_slice(&request.body).unwrap();
                let responses: Vec<Value> = batch["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sub| {
                        // The first sub-request is throttled once: it is sent again on its own.
                        if sub["id"] == "1" && refused.fetch_add(1, Ordering::SeqCst) == 0 {
                            return json!({ "id": sub["id"], "status": 429, "headers": { "Retry-After": "0" } });
                        }
                        // Marking unread is refused for good: the change fails with Graph's message.
                        if sub["body"] == json!({ "isRead": false }) {
                            return json!({ "id": sub["id"], "status": 403, "body": { "error": { "message": "Access denied" } } });
                        }
                        json!({ "id": sub["id"], "status": 200, "body": {} })
                    })
                    .collect();
                return Reply::json(json!({ "responses": responses }));
            }
            return Reply::bytes(202, Vec::new());
        }
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 1, "totalItemCount": 3 });
        match path.strip_prefix("/v1.0/").unwrap_or(path) {
            "me" => Reply::json(json!({ "displayName": "Me Outlook", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [
                folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items"), folder("F-trash", "Deleted Items"), folder("F-archive", "Archive"),
            ]})),
            "me/mailFolders/F-inbox/messages" => Reply::json(json!({ "value": [
                graph_message("g3", "c1", ("Bob", "bob@x.com"), "RE: Plan", 30),
                graph_message("g1", "c1", ("Bob", "bob@x.com"), "Plan", 0),
            ]})),
            "me/mailFolders/F-sent/messages" => {
                Reply::json(json!({ "value": [graph_message("g4", "c1", ("Me", "me@outlook.com"), "RE: Plan", 10)] }))
            }
            other => {
                let well_known =
                    [("inbox", "F-inbox"), ("sentitems", "F-sent"), ("deleteditems", "F-trash"), ("archive", "F-archive")];
                if let Some(name) = other.strip_prefix("me/mailFolders/")
                    && let Some((_, id)) = well_known.iter().find(|(known, _)| *known == name)
                {
                    return Reply::json(json!({ "id": id }));
                }
                if other.starts_with("me/mailFolders/") {
                    return Reply::status(404, json!({ "error": { "message": "Not found" } }));
                }
                if let Some(id) = other.strip_prefix("me/messages/") {
                    return Reply::json(json!({ "body": { "contentType": "text", "content": format!("Text of {id}") } }));
                }
                Reply::status(404, json!({ "error": { "message": "Not found" } }))
            }
        }
    });
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    wait_until("mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()["me@outlook.com"].as_array().is_some_and(|b| !b.is_empty()));
    let listing = call(&engine, "mail.list", json!({})).unwrap();
    let plan = "me@outlook.com|g1@outlook";
    assert_eq!(ids(&listing), [plan]);
    changes(&changed);

    call(&engine, "mail.read", json!({ "ids": [plan] })).unwrap();
    let sent = changes(&changed);
    // Throttled once, then the sub-request alone again.
    assert_eq!(sent.len(), 2);
    let first = json_body(&sent[0]);
    assert_eq!(
        first["requests"][0],
        json!({ "id": "1", "method": "PATCH", "url": "/me/messages/g3", "body": { "isRead": true },
                "headers": { "Prefer": "IdType=\"ImmutableId\"", "Content-Type": "application/json" } })
    );
    assert_eq!(json_body(&sent[1])["requests"].as_array().unwrap().len(), 1);

    let refused = call(&engine, "mail.read", json!({ "ids": [plan], "unread": true })).unwrap();
    assert_eq!(refused["failed"], json!([{ "code": "failed", "id": plan, "message": "Access denied" }]));
    assert_eq!(json_body(&changes(&changed)[0])["requests"][0]["body"], json!({ "isRead": false }));

    call(&engine, "mail.star", json!({ "ids": [plan] })).unwrap();
    let starred = json_body(&changes(&changed)[0]);
    assert_eq!(starred["requests"][0]["body"], json!({ "flag": { "flagStatus": "flagged" } }));
    assert_eq!(starred["requests"][0]["url"], "/me/messages/g3");

    call(&engine, "mail.archive", json!({ "ids": [plan] })).unwrap();
    let moved = json_body(&changes(&changed)[0]);
    let urls: Vec<&str> = moved["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["/me/messages/g1/move", "/me/messages/g3/move"]);
    assert_eq!(moved["requests"][0]["body"], json!({ "destinationId": "F-archive" }));
    assert_eq!(call(&engine, "mail.list", json!({ "cached": true })).unwrap()["conversations"], json!([]));

    let answer = call(
        &engine,
        "mail.send",
        json!({ "from": "me@outlook.com", "to": "bob@x.com", "subject": "Ünïcode", "body": "Hi", "yes": true }),
    )
    .unwrap();
    assert_eq!(answer["sent"], true);
    let sent = changes(&changed);
    assert_eq!((sent[0].path.as_str(), sent[0].header("Content-Type")), ("/v1.0/me/sendMail", Some("text/plain")));
    let mime = String::from_utf8(STANDARD.decode(&sent[0].body).unwrap()).unwrap();
    assert!(mime.starts_with("From: Me Outlook <me@outlook.com>\r\nTo: bob@x.com\r\nSubject: =?UTF-8?B?"), "{mime}");
    assert!(mime.contains("Content-Type: multipart/alternative;"), "{mime}");
    assert!(!mime.contains("Bcc"));
    let _ = encode;
    drop(engine);
    cleanup(&config);
}

/// Decodes the base64 `text/html` part from a rendered MIME message.
fn html_part(mime: &str) -> String {
    let (_, rest) = mime.split_once("Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n").unwrap();
    let encoded = rest.split("\r\n--").next().unwrap().replace("\r\n", "");
    String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap()
}

#[test]
fn sends_multipart_new_mail_and_forwards_for_gmail_and_graph() {
    // Gmail: new mail with link + forward of HTML message (with mscid inline image) and plain message.
    let changed: Changes = Arc::default();
    let server = gmail_server(changed.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    wait_until("inbox", || call(&engine, "mail.list", json!({})).is_ok_and(|v| v["conversations"].as_array().is_some_and(|c| !c.is_empty())));
    changes(&changed);

    // New mail includes multipart/alternative with auto-linked URL and email, keeping text/plain exact.
    call(
        &engine,
        "mail.send",
        json!({
            "from": "me@gmail.com",
            "to": ["bob@x.com"],
            "subject": "Links",
            "body": "Visit https://example.com\n\nPing ann@x.com",
            "yes": true,
        }),
    )
    .unwrap();
    let upload = changes(&changed).pop().unwrap().text();
    assert!(upload.contains("Content-Type: multipart/alternative;"), "{upload}");
    assert!(upload.contains("Visit https://example.com\r\n\r\nPing ann@x.com\r\n"), "{upload}");
    let html = html_part(&upload);
    assert!(html.contains("<div>Visit <a href=\"https://example.com\">https://example.com</a></div>"), "{html}");
    assert!(html.contains("<div><br></div>"), "{html}");
    assert!(html.contains("<div>Ping <a href=\"mailto:ann@x.com\">ann@x.com</a></div>"), "{html}");

    // Forward Gmail HTML message `i00` inside `lunch` conversation.
    let lunch = "me@gmail.com|i00@x";
    let err = call(&engine, "mail.send", json!({ "from": "me@gmail.com", "to": ["bob@x.com"], "subject": "Fwd: Lunch?", "body": "Hi", "forward-message": "i00", "yes": true })).unwrap_err();
    assert_eq!(err.code, "usage");
    let err = call(&engine, "mail.send", json!({ "from": "me@gmail.com", "to": ["bob@x.com"], "subject": "Fwd: Lunch?", "body": "Hi", "forward": "me@gmail.com|nope@x", "yes": true })).unwrap_err();
    assert_eq!(err.code, "unknown_conversation");

    let prefilled = "FYI https://menu.example\n\n---------- Forwarded message ---------\nFrom: Ann <ann@x.com>\nSubject: Lunch?\nTo: Me <me@gmail.com>\n\nHi, lunch at noon?";
    call(
        &engine,
        "mail.send",
        json!({
            "from": "me@gmail.com",
            "to": ["bob@x.com"],
            "subject": "Fwd: Lunch?",
            "body": prefilled,
            "forward": lunch,
            "forward-message": "i00",
            "yes": true,
        }),
    )
    .unwrap();
    let fwd_upload = changes(&changed).pop().unwrap().text();
    assert!(fwd_upload.contains("Content-Type: multipart/alternative;"), "{fwd_upload}");
    assert!(fwd_upload.contains("FYI https://menu.example\r\n\r\n---------- Forwarded message ---------"), "{fwd_upload}");
    assert!(!fwd_upload.contains("In-Reply-To:"), "{fwd_upload}");
    let fwd_html = html_part(&fwd_upload);
    assert!(fwd_html.contains("<div>FYI <a href=\"https://menu.example\">https://menu.example</a></div>"), "{fwd_html}");
    assert!(fwd_html.contains("<div class=\"gmail_quote\">"), "{fwd_html}");
    assert!(fwd_html.contains("<p>Hi, lunch </p>"), "{fwd_html}");
    assert!(!fwd_html.contains("mscid:"), "{fwd_html}");

    // Forward plain-text message `i03`: no `.gmail_quote`, linkified HTML lines.
    let topic = "me@gmail.com|i03@x";
    let plain_fwd = "Look at www.security.example\n\n---------- Forwarded message ---------\nFrom: Person 3 <p3@x.com>\n\nBody of i03";
    call(
        &engine,
        "mail.send",
        json!({
            "from": "me@gmail.com",
            "to": ["bob@x.com"],
            "subject": "Fwd: Security alert",
            "body": plain_fwd,
            "forward": topic,
            "yes": true,
        }),
    )
    .unwrap();
    let plain_upload = changes(&changed).pop().unwrap().text();
    assert!(plain_upload.contains("Content-Type: multipart/alternative;"), "{plain_upload}");
    let plain_html = html_part(&plain_upload);
    assert!(!plain_html.contains("gmail_quote"), "{plain_html}");
    assert!(plain_html.contains("<a href=\"http://www.security.example\">www.security.example</a>"), "{plain_html}");
    drop(engine);
    cleanup(&config);

    // Graph: new mail and forward of HTML message (with file: inline image dropped).
    let changed: Changes = Arc::default();
    let record = changed.clone();
    let tokens = AtomicUsize::new(0);
    let server = FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if request.method == "POST" {
            record.lock().unwrap().push(request.clone());
            if path == "/v1.0/$batch" {
                let batch: Value = serde_json::from_slice(&request.body).unwrap();
                let responses: Vec<Value> = batch["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sub| json!({ "id": sub["id"], "status": 200, "body": {} }))
                    .collect();
                return Reply::json(json!({ "responses": responses }));
            }
            return Reply::bytes(202, Vec::new());
        }
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 1, "totalItemCount": 1 });
        match path.strip_prefix("/v1.0/").unwrap_or(path) {
            "me" => Reply::json(json!({ "displayName": "Me Outlook", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items")] })),
            "me/mailFolders/F-inbox/messages" => Reply::json(json!({ "value": [graph_message("g1", "c1", ("Bob", "bob@x.com"), "Report", 0)] })),
            "me/mailFolders/F-sent/messages" => Reply::json(json!({ "value": [] })),
            other => {
                if other == "me/mailFolders/inbox" {
                    return Reply::json(json!({ "id": "F-inbox" }));
                }
                if other == "me/mailFolders/sentitems" {
                    return Reply::json(json!({ "id": "F-sent" }));
                }
                if other == "me/messages/g1" {
                    return Reply::json(json!({
                        "body": {
                            "contentType": "html",
                            "content": "<html><body><p>Quarterly <b>numbers</b></p><img src=\"file:///tmp/inline.png\"></body></html>"
                        }
                    }));
                }
                Reply::status(404, json!({ "error": { "message": "Not found" } }))
            }
        }
    });
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    wait_until("outlook inbox", || call(&engine, "mail.list", json!({})).is_ok_and(|v| v["conversations"].as_array().is_some_and(|c| !c.is_empty())));
    changes(&changed);

    let report = "me@outlook.com|g1@outlook";
    call(
        &engine,
        "mail.send",
        json!({
            "from": "me@outlook.com",
            "to": ["carol@x.com"],
            "subject": "Fwd: Report",
            "body": "See https://contoso.example",
            "forward": report,
            "yes": true,
        }),
    )
    .unwrap();
    let sent = changes(&changed);
    assert_eq!(sent[0].path, "/v1.0/me/sendMail");
    let mime = String::from_utf8(STANDARD.decode(&sent[0].body).unwrap()).unwrap();
    assert!(mime.contains("Content-Type: multipart/alternative;"), "{mime}");
    assert!(mime.contains("See https://contoso.example\r\n\r\n---------- Forwarded message ---------"), "{mime}");
    let graph_html = html_part(&mime);
    assert!(graph_html.contains("<div>See <a href=\"https://contoso.example\">https://contoso.example</a></div>"), "{graph_html}");
    assert!(graph_html.contains("<div class=\"gmail_quote\">"), "{graph_html}");
    assert!(graph_html.contains("<p>Quarterly <b>numbers</b></p>"), "{graph_html}");
    assert!(!graph_html.contains("file:///"), "{graph_html}");
    drop(engine);
    cleanup(&config);
}
