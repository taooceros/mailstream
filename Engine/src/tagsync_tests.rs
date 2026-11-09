//! AI tags synced with Gmail labels and Outlook categories end to end (MS-172): an engine in a
//! throwaway folder against a fake Gmail that keeps its labels, its messages' labels and their
//! history, and a fake Outlook that keeps its messages' categories, tagging with the fake omp
//! of `tagging_tests`.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::fake::{FakeServer, Reply};
use crate::mail_tests::{BASE_MS, call, engine_for, gmail_batch, graph_message, header, token_reply, wait_until};
use crate::tagging_tests::{counts, fake_omp};
use crate::tests::cleanup;

/// The fake mailbox: user labels by id, each message's label ids, and the history since 100.
struct Gmail {
    labels: BTreeMap<String, String>,
    messages: BTreeMap<String, BTreeSet<String>>,
    history: Vec<(u64, Value)>,
    made: usize,
}

impl Gmail {
    fn history_id(&self) -> u64 {
        100 + self.history.len() as u64
    }

    /// A label change, as Gmail records it in the history.
    fn relabel(&mut self, id: &str, add: &[&str], remove: &[&str]) {
        let labels = self.messages.get_mut(id).unwrap();
        labels.extend(add.iter().map(|label| label.to_string()));
        for label in remove {
            labels.remove(*label);
        }
        let message = json!({ "id": id });
        let next = self.history_id() + 1;
        self.history.push((next, json!({
            "labelsAdded": [{ "message": message, "labelIds": add }],
            "labelsRemoved": [{ "message": message, "labelIds": remove }],
        })));
    }

    fn label(&self, name: &str) -> Option<String> {
        self.labels.iter().find(|(_, known)| known.as_str() == name).map(|(id, _)| id.clone())
    }

    /// The user labels' names on `id`.
    fn names(&self, id: &str) -> BTreeSet<String> {
        self.messages[id].iter().filter_map(|label| self.labels.get(label).cloned()).collect()
    }

    fn metadata(&self) -> HashMap<String, Value> {
        self.messages
            .iter()
            .enumerate()
            .map(|(n, (id, labels))| {
                let value = json!({
                    "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
                    "internalDate": (BASE_MS - n as i64 * 3_600_000).to_string(), "sizeEstimate": 100,
                    "payload": { "mimeType": "text/plain", "headers": [
                        header("From", &format!("Person {n} <p{n}@x.com>")), header("To", "Me <me@gmail.com>"),
                        header("Subject", &format!("Topic {n}")), header("Message-ID", &format!("<{id}@x>")),
                    ]},
                });
                (id.clone(), value)
            })
            .collect()
    }
}

/// Gmail as the engine calls it: labels listed and made, messages listed, read in batches and
/// relabelled, and the history of those changes.
fn server(gmail: Arc<Mutex<Gmail>>) -> FakeServer {
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        let mut gmail = gmail.lock().unwrap();
        if path == "/batch/gmail/v1" {
            return gmail_batch(request, &gmail.metadata());
        }
        let Some(resource) = path.strip_prefix("/gmail/v1/users/me/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        match (request.method.as_str(), resource) {
            ("GET", "profile") => Reply::json(json!({ "emailAddress": "me@gmail.com", "historyId": gmail.history_id().to_string() })),
            ("GET", "settings/sendAs") => Reply::json(json!({ "sendAs": [{ "sendAsEmail": "me@gmail.com", "isPrimary": true }] })),
            ("GET", "labels") => {
                let mut labels = vec![json!({ "id": "INBOX", "name": "INBOX", "type": "system" })];
                labels.extend(gmail.labels.iter().map(|(id, name)| json!({ "id": id, "name": name, "type": "user" })));
                Reply::json(json!({ "labels": labels }))
            }
            ("POST", "labels") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let name = body["name"].as_str().unwrap().to_string();
                if gmail.labels.values().any(|known| known.to_lowercase() == name.to_lowercase()) {
                    return Reply::status(409, json!({ "error": { "message": "Label name exists or conflicts" } }));
                }
                assert_eq!((body["labelListVisibility"].as_str(), body["messageListVisibility"].as_str()), (Some("labelShow"), Some("show")));
                gmail.made += 1;
                let id = format!("Label_{}", 10 + gmail.made);
                gmail.labels.insert(id.clone(), name.clone());
                Reply::json(json!({ "id": id, "name": name, "type": "user" }))
            }
            ("POST", "messages/batchModify") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let strings = |key: &str| -> Vec<String> {
                    body[key].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_string()).collect()
                };
                let (add, remove) = (strings("addLabelIds"), strings("removeLabelIds"));
                for id in strings("ids") {
                    let add: Vec<&str> = add.iter().map(String::as_str).collect();
                    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
                    gmail.relabel(&id, &add, &remove);
                }
                Reply { status: 204, headers: vec![], body: vec![] }
            }
            ("GET", "history") => {
                let start: u64 = request.param("startHistoryId").unwrap().parse().unwrap();
                let records: Vec<&Value> = gmail.history.iter().filter(|(id, _)| *id > start).map(|(_, record)| record).collect();
                Reply::json(json!({ "history": records, "historyId": gmail.history_id().to_string() }))
            }
            ("GET", "messages") => {
                let ids: Vec<Value> = match request.param("labelIds") {
                    Some("INBOX") => gmail.messages.keys().map(|id| json!({ "id": id })).collect(),
                    _ => vec![],
                };
                Reply::json(json!({ "messages": ids, "resultSizeEstimate": ids.len() }))
            }
            _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
        }
    })
}

fn list(engine: &crate::Engine) -> Vec<Value> {
    let listed = call(engine, "mail.list", json!({ "limit": 100, "cached": true })).unwrap();
    listed["conversations"].as_array().unwrap().clone()
}

/// The conversation of message `n` as listed: (category, waiting).
fn tag(engine: &crate::Engine, n: usize) -> (Value, Value) {
    let id = format!("me@gmail.com|i{n}@x");
    let conversation = list(engine).into_iter().find(|conversation| conversation["id"] == id.as_str()).unwrap();
    (conversation["category"].clone(), conversation["waiting"].clone())
}

#[test]
fn syncs_tags_with_gmail_labels_both_ways() {
    let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<BTreeSet<_>>();
    // i0 already carries the user's own `newsletters` label (another device tagged it); i4
    // carries `Receipts`, which sync never touches; the user also has an `INVITATIONS` label.
    let gmail = Arc::new(Mutex::new(Gmail {
        labels: BTreeMap::from([
            ("Label_1".into(), "newsletters".into()),
            ("Label_2".into(), "Receipts".into()),
            ("Label_3".into(), "INVITATIONS".into()),
        ]),
        messages: (0..5)
            .map(|n| {
                let mut labels = BTreeSet::from(["INBOX".to_string()]);
                match n {
                    0 => labels.insert("Label_1".into()),
                    4 => labels.insert("Label_2".into()),
                    _ => false,
                };
                (format!("i{n}"), labels)
            })
            .collect(),
        history: Vec::new(),
        made: 0,
    }));
    let server = server(gmail.clone());
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    let dir = config.support_directory.parent().unwrap().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    let omp = fake_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp.display().to_string() })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.syncAccounts", "value": "[\"Me@Gmail.com\"]" })).unwrap();
    wait_until("the inbox", || call(&engine, "mail.list", json!({ "limit": 100, "cached": true })).is_ok_and(|listed| {
        listed["conversations"].as_array().unwrap().len() == 5
    }));

    // The conversation another device tagged takes the server's tag and is not sent to the
    // model; the model's tags go to Gmail, in labels made for the names Gmail lacked.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("tags on the server", || {
        let gmail = gmail.lock().unwrap();
        gmail.names("i1") == names(&["People", "Waiting on you"]) && gmail.names("i4") == names(&["Notifications", "Receipts"])
    });
    assert_eq!(counts(&dir), [4]);
    assert_eq!(tag(&engine, 0), (json!("newsletter"), json!(false)));
    assert_eq!(tag(&engine, 1), (json!("people"), json!(true)));
    {
        let gmail = gmail.lock().unwrap();
        // The lapsed ask doesn't wait; the conversation the model skipped stays bare.
        assert_eq!(gmail.names("i2"), names(&["People"]));
        assert_eq!(gmail.names("i3"), names(&[]));
        assert_eq!(gmail.names("i0"), names(&["newsletters"]));
        let made: BTreeSet<String> = gmail.labels.values().cloned().collect();
        assert_eq!(made, names(&["INVITATIONS", "Notifications", "People", "Receipts", "Waiting on you", "newsletters"]));
    }

    // Changed in Gmail (another device, or the user there): the server's tag wins here.
    std::thread::sleep(Duration::from_millis(1500));
    {
        let mut gmail = gmail.lock().unwrap();
        let people = gmail.label("People").unwrap();
        gmail.relabel("i2", &["Label_3"], &[&people]);
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the server's change", || tag(&engine, 2) == (json!("invitation"), json!(false)));

    // A correction here goes there, into the user's own label of that name.
    let i4 = "me@gmail.com|i4@x";
    call(&engine, "mail.tag", json!({ "ids": [i4], "category": "newsletter" })).unwrap();
    wait_until("the correction on the server", || gmail.lock().unwrap().names("i4") == names(&["Receipts", "newsletters"]));

    // Taken off in Gmail altogether: the tag stays here and is not put back.
    std::thread::sleep(Duration::from_millis(1500));
    let histories = || server.requests.lock().unwrap().iter().filter(|request| request.path.ends_with("/history")).count();
    let before = histories();
    {
        let mut gmail = gmail.lock().unwrap();
        let (people, waiting) = (gmail.label("People").unwrap(), gmail.label("Waiting on you").unwrap());
        gmail.relabel("i1", &[], &[&people, &waiting]);
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the removal read", || histories() > before);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(tag(&engine, 1), (json!("people"), json!(true)));
    assert_eq!(gmail.lock().unwrap().names("i1"), names(&[]));

    // Off for the account: corrections stay here.
    call(&engine, "config.set", json!({ "key": "aiTagging.syncAccounts", "value": "[]" })).unwrap();
    call(&engine, "mail.tag", json!({ "ids": ["me@gmail.com|i3@x"], "category": "people" })).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(gmail.lock().unwrap().names("i3"), names(&[]));

    // No label was ever deleted.
    assert!(server.requests.lock().unwrap().iter().all(|request| request.method != "DELETE"));
    assert_eq!(gmail.lock().unwrap().labels.len(), 6);
    drop(engine);
    cleanup(&config);
}

/// Outlook as the engine calls it: the inbox, its messages with their categories, and `$batch`
/// reading and replacing categories. Every request that changes something is a PATCH of
/// `categories` inside `$batch`.
fn outlook(categories: Arc<Mutex<BTreeMap<String, Vec<String>>>>) -> FakeServer {
    let tokens = AtomicUsize::new(0);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return token_reply(&tokens);
        }
        let Some(resource) = path.strip_prefix("/v1.0/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        let mut categories = categories.lock().unwrap();
        let message = |id: &str, conversation: &str, minutes: i64, categories: &[String]| {
            let mut message = graph_message(id, conversation, ("Bob", "bob@x.com"), &format!("Topic {id}"), minutes);
            message["categories"] = json!(categories);
            message
        };
        match (request.method.as_str(), resource) {
            ("GET", "me") => Reply::json(json!({ "displayName": "Me", "mail": "me@outlook.com" })),
            ("GET", "me/mailFolders") => Reply::json(json!({ "value": [
                { "id": "F-inbox", "displayName": "Inbox", "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 2 },
            ]})),
            ("GET", "me/mailFolders/inbox") => Reply::json(json!({ "id": "F-inbox" })),
            ("GET", "me/mailFolders/F-inbox/messages") => Reply::json(json!({ "value": [
                message("g2", "c2", 20, &categories["g2"]),
                message("g1", "c1", 0, &categories["g1"]),
            ]})),
            ("POST", "$batch") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let responses: Vec<Value> = body["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sub| {
                        let url = sub["url"].as_str().unwrap();
                        let id = url.trim_start_matches("/me/messages/").split('?').next().unwrap();
                        match sub["method"].as_str().unwrap() {
                            "GET" => {
                                assert_eq!(url, format!("/me/messages/{id}?$select=categories"));
                                json!({ "id": sub["id"], "status": 200, "body": { "categories": categories[id] } })
                            }
                            method => {
                                assert_eq!((method, url), ("PATCH", format!("/me/messages/{id}").as_str()));
                                let names = sub["body"]["categories"].as_array().unwrap();
                                assert_eq!(sub["body"].as_object().unwrap().len(), 1, "only categories change");
                                categories.insert(id.to_string(), names.iter().map(|name| name.as_str().unwrap().to_string()).collect());
                                json!({ "id": sub["id"], "status": 200, "body": {} })
                            }
                        }
                    })
                    .collect();
                Reply::json(json!({ "responses": responses }))
            }
            ("GET", other) if other.starts_with("me/messages/") => {
                Reply::json(json!({ "body": { "contentType": "text", "content": "Text" } }))
            }
            ("GET", other) if other.starts_with("me/mailFolders/") && other.ends_with("/messages") => {
                Reply::json(json!({ "value": [] }))
            }
            _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
        }
    })
}

#[test]
fn syncs_tags_with_outlook_categories() {
    let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    // g1 carries the user's own category and `people` from another device.
    let categories = Arc::new(Mutex::new(BTreeMap::from([
        ("g1".to_string(), names(&["Red category", "people"])),
        ("g2".to_string(), names(&["Blue category"])),
    ])));
    let server = outlook(categories.clone());
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    let dir = config.support_directory.parent().unwrap().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    let omp = fake_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp.display().to_string() })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.syncAccounts", "value": "me@outlook.com" })).unwrap();
    let listed = || call(&engine, "mail.list", json!({ "limit": 10, "cached": true })).map(|listed| listed["conversations"].clone());
    wait_until("the inbox", || listed().is_ok_and(|conversations| conversations.as_array().unwrap().len() == 2));

    // g1 takes the server's tag; only g2 goes to the model, and its tag joins its categories.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("categories", || categories.lock().unwrap()["g2"] == names(&["Blue category", "People", "Waiting on you"]));
    assert_eq!(counts(&dir), [1]);
    let conversations = listed().unwrap();
    assert_eq!((&conversations[1]["category"], &conversations[1]["waiting"]), (&json!("people"), &json!(false)));

    // A correction replaces the old name however it was spelled, and keeps the user's own.
    call(&engine, "mail.tag", json!({ "ids": ["me@outlook.com|g1@outlook"], "category": "notification" })).unwrap();
    wait_until("the correction", || categories.lock().unwrap()["g1"] == names(&["Red category", "Notifications"]));
    drop(engine);
    cleanup(&config);
}
