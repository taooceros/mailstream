//! `mail.undo` end to end (MS-280): each action taken back against fake Gmail and Graph servers
//! that record every change asked of them, the lists and counts following at once.
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use serde_json::{Value, json};

use super::action_tests::{Changes, changes, conversation, gmail_server, json_body};
use crate::mail_tests::{graph_message, ids};
use crate::testkit::{FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};

#[test]
fn undoes_gmail_actions_on_the_server_too() {
    let changed: Changes = Arc::default();
    let server = gmail_server(changed.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    let unread = || call(&engine, "mailboxes", json!({})).unwrap()["me@gmail.com"][0]["unread"].clone();
    wait_until("counts", || unread() == json!(3));
    call(&engine, "mail.list", json!({ "limit": 10 })).unwrap();
    let cached = || call(&engine, "mail.list", json!({ "cached": true, "limit": 10 })).unwrap();
    let undo = |args: Value| call(&engine, "mail.undo", args);
    let lunch = "me@gmail.com|i00@x";
    changes(&changed);

    // Read, then unread: undo takes back the latest step first, each on the server, the list
    // and the inbox count.
    let read = call(&engine, "mail.read", json!({ "ids": [lunch] })).unwrap();
    let marked = call(&engine, "mail.read", json!({ "ids": [lunch], "unread": true })).unwrap();
    assert_eq!((read["undo"].clone(), marked["undo"].clone()), (json!(1), json!(2)));
    changes(&changed);
    assert_eq!(undo(json!({})).unwrap(), json!({ "step": 2, "action": "unread", "done": [lunch], "failed": [] }));
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["i00"], "addLabelIds": [], "removeLabelIds": ["UNREAD"] }));
    assert_eq!((conversation(&cached(), lunch)["unread"].clone(), unread()), (json!(false), json!(2)));
    assert_eq!(undo(json!({})).unwrap()["action"], "read");
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["i00"], "addLabelIds": ["UNREAD"], "removeLabelIds": [] }));
    assert_eq!((conversation(&cached(), lunch)["unread"].clone(), unread()), (json!(true), json!(3)));
    assert_eq!(undo(json!({})).unwrap_err().code, "nothing");

    // A star comes off again; reading what is read already changes nothing, so it has no step.
    let starred = call(&engine, "mail.star", json!({ "ids": [lunch] })).unwrap();
    changes(&changed);
    assert_eq!(undo(json!({ "step": starred["undo"] })).unwrap()["action"], "star");
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["s0"], "addLabelIds": [], "removeLabelIds": ["STARRED"] }));
    assert_eq!(conversation(&cached(), lunch)["starred"], false);
    let unchanged = call(&engine, "mail.read", json!({ "ids": ["me@gmail.com|i03@x"] })).unwrap();
    assert_eq!(unchanged, json!({ "done": ["me@gmail.com|i03@x"], "failed": [] }));

    // Archive taken back: the message gets INBOX back and lists again, unread as it was.
    let archived = call(&engine, "mail.archive", json!({ "ids": [lunch] })).unwrap();
    assert!(!ids(&cached()).contains(&lunch.to_string()));
    assert_eq!(unread(), 2);
    changes(&changed);
    let undone = undo(json!({ "step": archived["undo"] })).unwrap();
    assert_eq!(undone, json!({ "step": archived["undo"], "action": "archive", "done": [lunch], "failed": [] }));
    assert_eq!(json_body(&changes(&changed)[0]), json!({ "ids": ["i00"], "addLabelIds": ["INBOX"], "removeLabelIds": [] }));
    assert_eq!(ids(&cached())[0], lunch);
    assert_eq!(conversation(&cached(), lunch)["unread"], true);
    assert_eq!(unread(), 3);
    // The conversation can be acted on again.
    assert_eq!(call(&engine, "mail.read", json!({ "ids": [lunch] })).unwrap()["done"], json!([lunch]));

    // Trash taken back: Gmail untrashes it and it is filed in the inbox again.
    let trashed = call(&engine, "mail.trash", json!({ "ids": ["me@gmail.com|i05@x"] })).unwrap();
    changes(&changed);
    assert_eq!(undo(json!({ "step": trashed["undo"] })).unwrap()["done"], json!(["me@gmail.com|i05@x"]));
    let sent = changes(&changed);
    assert_eq!(sent[0].path, "/gmail/v1/users/me/messages/i05/untrash");
    assert_eq!(json_body(&sent[1]), json!({ "ids": ["i05"], "addLabelIds": ["INBOX"], "removeLabelIds": [] }));
    assert!(ids(&cached()).contains(&"me@gmail.com|i05@x".to_string()));

    // Restore taken back: it goes back to the trash.
    let trash = || call(&engine, "mail.list", json!({ "mailbox": "trash", "cached": true })).unwrap();
    assert_eq!(ids(&call(&engine, "mail.list", json!({ "mailbox": "trash" })).unwrap()).len(), 2);
    let restored = call(&engine, "mail.restore", json!({ "ids": ["me@gmail.com|t2@x"] })).unwrap();
    assert_eq!(ids(&trash()), ["me@gmail.com|t1@x"]);
    changes(&changed);
    assert_eq!(undo(json!({ "step": restored["undo"] })).unwrap()["action"], "restore");
    assert_eq!(changes(&changed)[0].path, "/gmail/v1/users/me/messages/t2/trash");
    assert_eq!(ids(&trash()), ["me@gmail.com|t1@x", "me@gmail.com|t2@x"]);

    // A step taken back is gone.
    let error = undo(json!({ "step": restored["undo"] })).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("not_found", "Step 7 can no longer be undone."));
    drop(engine);
    cleanup(&config);
}

/// Outlook with Bob's plan (g1, and g3 unread) in the inbox, answering moves and flag changes
/// in `$batch`; marking unread is refused.
fn graph_server(changed: Changes) -> FakeServer {
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        if request.method == "POST" && path == "/v1.0/$batch" {
            changed.lock().unwrap().push(request.clone());
            let batch: Value = serde_json::from_slice(&request.body).unwrap();
            let responses: Vec<Value> = batch["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|sub| {
                    if sub["body"] == json!({ "isRead": false }) {
                        return json!({ "id": sub["id"], "status": 403, "body": { "error": { "message": "Access denied" } } });
                    }
                    json!({ "id": sub["id"], "status": 200, "body": {} })
                })
                .collect();
            return Reply::json(json!({ "responses": responses }));
        }
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 1, "totalItemCount": 2 });
        match path.strip_prefix("/v1.0/").unwrap_or(path) {
            "me" => Reply::json(json!({ "displayName": "Me Outlook", "mail": "me@outlook.com" })),
            "me/mailFolders" => Reply::json(json!({ "value": [
                folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items"), folder("F-trash", "Deleted Items"), folder("F-archive", "Archive"),
            ]})),
            "me/mailFolders/F-inbox/messages" => Reply::json(json!({ "value": [
                graph_message("g3", "c1", ("Bob", "bob@x.com"), "RE: Plan", 30),
                graph_message("g1", "c1", ("Bob", "bob@x.com"), "Plan", 0),
            ]})),
            other => {
                let well_known = [("inbox", "F-inbox"), ("sentitems", "F-sent"), ("deleteditems", "F-trash"), ("archive", "F-archive")];
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
fn moves_outlook_mail_back_and_reports_a_refused_undo() {
    let changed: Changes = Arc::default();
    let server = graph_server(changed.clone());
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    wait_until("mailboxes", || call(&engine, "mailboxes", json!({})).unwrap()["me@outlook.com"].as_array().is_some_and(|b| !b.is_empty()));
    let plan = "me@outlook.com|g1@outlook";
    assert_eq!(ids(&call(&engine, "mail.list", json!({})).unwrap()), [plan]);
    let cached = || call(&engine, "mail.list", json!({ "cached": true })).unwrap();
    changes(&changed);

    // Archive taken back moves both messages from Archive back to the inbox.
    let archived = call(&engine, "mail.archive", json!({ "ids": [plan] })).unwrap();
    assert_eq!(cached()["conversations"], json!([]));
    changes(&changed);
    let undone = call(&engine, "mail.undo", json!({ "step": archived["undo"] })).unwrap();
    assert_eq!(undone["done"], json!([plan]));
    let moved = json_body(&changes(&changed)[0]);
    let mut urls: Vec<&str> = moved["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    urls.sort();
    assert_eq!(urls, ["/me/messages/g1/move", "/me/messages/g3/move"]);
    assert_eq!(moved["requests"][0]["body"], json!({ "destinationId": "F-inbox" }));
    assert_eq!(ids(&cached()), [plan]);
    assert_eq!(conversation(&cached(), plan)["unread"], true);

    // Undoing a read Outlook refuses: it is listed as failed, and the conversation stays read.
    call(&engine, "mail.read", json!({ "ids": [plan] })).unwrap();
    let refused = call(&engine, "mail.undo", json!({})).unwrap();
    assert_eq!(refused["action"], "read");
    assert_eq!(refused["failed"], json!([{ "id": plan, "code": "failed", "message": "Access denied" }]));
    assert_eq!(conversation(&cached(), plan)["unread"], false);
    drop(engine);
    cleanup(&config);
}
