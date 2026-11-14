//! AI tags synced with Gmail labels and Outlook categories end to end (MS-172), topic labels
//! as `Mailstream/<topic>` and on by default (MS-341): an engine in a throwaway folder against a
//! fake Gmail that keeps its labels, its messages' labels and their history, and a fake Outlook
//! that keeps its messages' categories, tagging with the fake omp of `tagging_tests`. Also the
//! providers' own sorting as the tagger's hint and the lists' provisional tag (MS-336), which
//! never reaches the server.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mail_tests::{BASE_MS, gmail_batch, graph_message, header};
use crate::tagging_tests::{counts, fake_omp};
use crate::testkit::{FakeServer, Reply, call, cleanup, engine_for, token_reply, wait_until};

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
                let value = match id.chars().next().zip(id.get(1..)) {
                    Some((prefix @ ('r' | 's' | 't'), parent)) => {
                        let parent_id = format!("i{parent}");
                        let step = (prefix as u8 - b'q') as i64;
                        let index = parent.parse::<i64>().unwrap_or(0);
                        json!({
                            "id": id, "threadId": format!("t-{parent_id}"), "labelIds": labels,
                            "internalDate": (BASE_MS + step * 3_600_000 - index * 60_000).to_string(),
                            "sizeEstimate": 100,
                            "payload": { "mimeType": "text/plain", "headers": [
                                header("From", &format!("Person {parent} <p{parent}@x.com>")),
                                header("To", "Me <me@gmail.com>"),
                                header("Subject", &format!("Re: Topic {parent}")),
                                header("Message-ID", &format!("<{id}@x>")),
                                header("In-Reply-To", &format!("<{parent_id}@x>")),
                            ]},
                        })
                    }
                    _ => json!({
                        "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
                        "internalDate": (BASE_MS - n as i64 * 3_600_000).to_string(), "sizeEstimate": 100,
                        "payload": { "mimeType": "text/plain", "headers": [
                            header("From", &format!("Person {n} <p{n}@x.com>")), header("To", "Me <me@gmail.com>"),
                            header("Subject", &format!("Topic {n}")), header("Message-ID", &format!("<{id}@x>")),
                        ]},
                    }),
                };
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
                Reply::bytes(204, Vec::new())
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
    let conversation = listed(engine, n);
    (conversation["category"].clone(), conversation["waiting"].clone())
}

fn listed(engine: &crate::Engine, n: usize) -> Value {
    let id = format!("me@gmail.com|i{n}@x");
    list(engine).into_iter().find(|conversation| conversation["id"] == id.as_str()).unwrap()
}

/// The topic labels of message `n`'s conversation as listed.
fn topics(engine: &crate::Engine, n: usize) -> Value {
    listed(engine, n)["labels"].clone()
}

#[test]
fn syncs_tags_with_gmail_labels_both_ways() {
    let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<BTreeSet<_>>();
    // i0 already carries the user's own `newsletters` label and a topic (another device tagged
    // it); i4 carries `Receipts`, which sync never touches; the user also has an `INVITATIONS`
    // label. No account is set to sync: every account does unless listed as local only.
    let gmail = Arc::new(Mutex::new(Gmail {
        labels: BTreeMap::from([
            ("Label_1".into(), "newsletters".into()),
            ("Label_2".into(), "Receipts".into()),
            ("Label_3".into(), "INVITATIONS".into()),
            ("Label_4".into(), "Mailstream/Travel".into()),
        ]),
        messages: (0..5)
            .map(|n| {
                let mut labels = BTreeSet::from(["INBOX".to_string()]);
                match n {
                    0 => labels.extend(["Label_1".to_string(), "Label_4".to_string()]),
                    4 => labels.extend(["Label_2".to_string()]),
                    _ => {}
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
    wait_until("the inbox", || call(&engine, "mail.list", json!({ "limit": 100, "cached": true })).is_ok_and(|listed| {
        listed["conversations"].as_array().unwrap().len() == 5
    }));

    // The conversation another device tagged takes the server's tag and topic and is not sent
    // to the model; the model's tags and topics go to Gmail, in labels made for the names Gmail
    // lacked, a topic's under a Mailstream label made for it.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("tags on the server", || {
        let gmail = gmail.lock().unwrap();
        gmail.names("i1") == names(&["People", "Waiting on you", "Mailstream/Work"])
            && gmail.names("i4") == names(&["Notifications", "Receipts"])
    });
    assert_eq!(counts(&dir), [4]);
    assert_eq!(tag(&engine, 0), (json!("newsletter"), json!(false)));
    assert_eq!(topics(&engine, 0), json!(["Travel"]));
    assert_eq!(tag(&engine, 1), (json!("people"), json!(true)));
    {
        let gmail = gmail.lock().unwrap();
        // The lapsed ask doesn't wait; the conversation the model skipped stays bare.
        assert_eq!(gmail.names("i2"), names(&["People"]));
        assert_eq!(gmail.names("i3"), names(&[]));
        assert_eq!(gmail.names("i0"), names(&["Mailstream/Travel", "newsletters"]));
        let made: BTreeSet<String> = gmail.labels.values().cloned().collect();
        assert_eq!(
            made,
            names(&[
                "INVITATIONS", "Mailstream", "Mailstream/Travel", "Mailstream/Work", "Notifications", "People", "Receipts",
                "Waiting on you", "newsletters",
            ])
        );
        // The parent before its child.
        assert!(gmail.label("Mailstream").unwrap() < gmail.label("Mailstream/Work").unwrap());
    }

    // Changed in Gmail (another device, or the user there): the server's tag and topics win
    // here, a topic taken off there included.
    std::thread::sleep(Duration::from_millis(1500));
    {
        let mut gmail = gmail.lock().unwrap();
        let people = gmail.label("People").unwrap();
        gmail.relabel("i2", &["Label_3", "Label_4"], &[&people]);
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the server's change", || {
        tag(&engine, 2) == (json!("invitation"), json!(false)) && topics(&engine, 2) == json!(["Travel"])
    });
    std::thread::sleep(Duration::from_millis(1500));
    gmail.lock().unwrap().relabel("i2", &[], &["Label_4"]);
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the topic taken off", || topics(&engine, 2) == json!([]));
    assert_eq!(tag(&engine, 2), (json!("invitation"), json!(false)));

    // A correction here goes there, into the user's own label of that name; so do topics
    // added and taken off here.
    let i4 = "me@gmail.com|i4@x";
    call(&engine, "mail.tag", json!({ "ids": [i4], "category": "newsletter" })).unwrap();
    wait_until("the correction on the server", || gmail.lock().unwrap().names("i4") == names(&["Receipts", "newsletters"]));
    call(&engine, "mail.tag", json!({ "ids": [i4], "add-labels": ["Finance"] })).unwrap();
    wait_until("the topic on the server", || {
        gmail.lock().unwrap().names("i4") == names(&["Mailstream/Finance", "Receipts", "newsletters"])
    });
    call(&engine, "mail.tag", json!({ "ids": [i4], "remove-labels": ["finance"] })).unwrap();
    wait_until("the topic off the server", || gmail.lock().unwrap().names("i4") == names(&["Receipts", "newsletters"]));

    // Taken off in Gmail altogether: the tag stays here and is not put back.
    std::thread::sleep(Duration::from_millis(1500));
    let histories = || server.requests.lock().unwrap().iter().filter(|request| request.path.ends_with("/history")).count();
    let before = histories();
    {
        let mut gmail = gmail.lock().unwrap();
        let gone = ["People", "Waiting on you", "Mailstream/Work"].map(|name| gmail.label(name).unwrap());
        gmail.relabel("i1", &[], &gone.each_ref().map(String::as_str));
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the removal read", || histories() > before);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(tag(&engine, 1), (json!("people"), json!(true)));
    assert_eq!(topics(&engine, 1), json!(["Work"]));
    assert_eq!(gmail.lock().unwrap().names("i1"), names(&[]));

    // Local only for the account: corrections stay here.
    call(&engine, "config.set", json!({ "key": "aiTagging.localOnlyAccounts", "value": "[\"Me@Gmail.com\"]" })).unwrap();
    call(&engine, "mail.tag", json!({ "ids": ["me@gmail.com|i3@x"], "category": "people", "labels": ["Home"] })).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(gmail.lock().unwrap().names("i3"), names(&[]));

    // No label was ever deleted.
    assert!(server.requests.lock().unwrap().iter().all(|request| request.method != "DELETE"));
    assert_eq!(gmail.lock().unwrap().labels.len(), 10);
    drop(engine);
    cleanup(&config);
}

/// Two devices, each an engine with a data folder of its own, on one Gmail account: what one
/// tags, or the user corrects there, category and topics, shows on the other (MS-341).
#[test]
fn two_devices_share_tags_through_gmail() {
    let gmail = Arc::new(Mutex::new(Gmail {
        labels: BTreeMap::new(),
        messages: (0..5).map(|n| (format!("i{n}"), BTreeSet::from(["INBOX".to_string()]))).collect(),
        history: Vec::new(),
        made: 0,
    }));
    let server = server(gmail.clone());
    let start = |omp: fn(&std::path::Path) -> std::path::PathBuf| {
        let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
        let dir = config.support_directory.parent().unwrap().join("omp");
        std::fs::create_dir_all(&dir).unwrap();
        call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
        call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp(&dir).display().to_string() })).unwrap();
        wait_until("the inbox", || call(&engine, "mail.list", json!({ "limit": 100, "cached": true })).is_ok_and(|listed| {
            listed["conversations"].as_array().unwrap().len() == 5
        }));
        (engine, config, dir)
    };
    // The laptop tags with its model; the desktop's model is unreachable, so all it shows it
    // took from the server.
    let (laptop, laptop_config, laptop_omp) = start(fake_omp);
    let (desktop, desktop_config, _) = start(offline_omp);
    call(&laptop, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("the laptop's tags on the server", || gmail.lock().unwrap().names("i4").contains("Notifications"));
    call(&desktop, "refresh", json!({})).unwrap();
    call(&desktop, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    let same = |n: usize| (tag(&desktop, n), topics(&desktop, n)) == (tag(&laptop, n), topics(&laptop, n));
    wait_until("the laptop's tags on the desktop", || [0, 1, 3, 4].into_iter().all(same));
    assert_eq!(tag(&desktop, 4).0, "notification");
    assert_eq!(topics(&desktop, 0), json!(["Work"]));
    assert_eq!(counts(&laptop_omp), [5]);

    // Corrected on the desktop: the laptop follows once it hears from Gmail.
    std::thread::sleep(Duration::from_millis(1500));
    let i0 = "me@gmail.com|i0@x";
    call(&desktop, "mail.tag", json!({ "ids": [i0], "category": "invitation", "add-labels": ["Trip"] })).unwrap();
    wait_until("the correction on the server", || gmail.lock().unwrap().names("i0").contains("Mailstream/Trip"));
    call(&laptop, "refresh", json!({})).unwrap();
    wait_until("the correction on the laptop", || {
        tag(&laptop, 0) == (json!("invitation"), json!(true)) && topics(&laptop, 0) == json!(["Trip", "Work"])
    });
    drop((laptop, desktop));
    cleanup(&laptop_config);
    cleanup(&desktop_config);
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
            message["inferenceClassification"] = json!(if id == "g1" { "focused" } else { "other" });
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
    // g1 carries the user's own category, and `people` with a topic from another device. Every
    // account syncs unless listed as local only.
    let categories = Arc::new(Mutex::new(BTreeMap::from([
        ("g1".to_string(), names(&["Red category", "people", "Mailstream/Bills"])),
        ("g2".to_string(), names(&["Blue category"])),
    ])));
    let server = outlook(categories.clone());
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    let dir = config.support_directory.parent().unwrap().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    let omp = fake_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp.display().to_string() })).unwrap();
    let listed = || call(&engine, "mail.list", json!({ "limit": 10, "cached": true })).map(|listed| listed["conversations"].clone());
    wait_until("the inbox", || listed().is_ok_and(|conversations| conversations.as_array().unwrap().len() == 2));

    // g1 takes the server's tag and topic; only g2 goes to the model, and its tag and topic join
    // its categories.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("categories", || {
        categories.lock().unwrap()["g2"] == names(&["Blue category", "People", "Waiting on you", "Mailstream/Work"])
    });
    assert_eq!(counts(&dir), [1]);
    let conversations = listed().unwrap();
    assert_eq!(
        (&conversations[1]["category"], &conversations[1]["waiting"], &conversations[1]["labels"]),
        (&json!("people"), &json!(false), &json!(["Bills"]))
    );

    // A correction replaces the old name however it was spelled, and keeps the user's own; a
    // topic taken off here comes off there.
    let g1 = "me@outlook.com|g1@outlook";
    call(&engine, "mail.tag", json!({ "ids": [g1], "category": "notification" })).unwrap();
    wait_until("the correction", || categories.lock().unwrap()["g1"] == names(&["Red category", "Mailstream/Bills", "Notifications"]));
    call(&engine, "mail.tag", json!({ "ids": [g1], "remove-labels": ["Bills"] })).unwrap();
    wait_until("the topic taken off", || categories.lock().unwrap()["g1"] == names(&["Red category", "Notifications"]));
    drop(engine);
    cleanup(&config);
}

/// An omp that writes each prompt to `<dir>/prompts` and fails, so nothing gets an AI tag.
fn offline_omp(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("offline-omp");
    crate::omp::tests::script(
        &path,
        &format!("for last; do :; done\nprintf '%s\\n' \"$last\" >> {}/prompts\necho 'Error: offline' >&2; exit 1", dir.display()),
    );
    path
}

fn tagging_failed(engine: &crate::Engine) -> bool {
    call(engine, "status", json!({})).unwrap()["aiTagging"]["failed"] == json!(true)
}

#[test]
fn stands_in_with_gmail_categories_until_the_model_answers() {
    let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<BTreeSet<_>>();
    // i0-i4 carry Gmail's five inbox categories, i1 also Important; i5 none, as in a Workspace
    // account without categories.
    let categories = ["CATEGORY_PERSONAL", "CATEGORY_UPDATES", "CATEGORY_PROMOTIONS", "CATEGORY_SOCIAL", "CATEGORY_FORUMS"];
    let gmail = Arc::new(Mutex::new(Gmail {
        labels: BTreeMap::new(),
        messages: (0..6)
            .map(|n| {
                let mut labels = BTreeSet::from(["INBOX".to_string()]);
                labels.extend(categories.get(n).map(|label| label.to_string()));
                if n == 1 {
                    labels.insert("IMPORTANT".into());
                }
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
    let offline = offline_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": offline.display().to_string() })).unwrap();
    wait_until("the inbox", || call(&engine, "mail.list", json!({ "limit": 100, "cached": true })).is_ok_and(|listed| {
        listed["conversations"].as_array().unwrap().len() == 6
    }));
    // Off: no tags at all, provisional ones included.
    assert_eq!(tag(&engine, 0), (Value::Null, json!(false)));

    // On, with the model unreachable: the lists group by Gmail's categories at once.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("the failed run", || tagging_failed(&engine));
    let stand_ins: Vec<Value> = (0..6).map(|n| tag(&engine, n).0).collect();
    assert_eq!(stand_ins, [json!("people"), json!("notification"), json!("newsletter"), json!("newsletter"), json!("newsletter"), Value::Null]);
    assert!((0..6).all(|n| tag(&engine, n).1 == json!(false)));
    let newsletters = call(&engine, "mail.list", json!({ "limit": 100, "cached": true, "category": "newsletter" })).unwrap();
    assert_eq!(newsletters["conversations"].as_array().unwrap().len(), 3);
    // The model was told, as hints.
    let prompts = std::fs::read_to_string(dir.join("prompts")).unwrap();
    let prompt = prompts.split("today: ").last().unwrap();
    for hint in ["personal", "updates", "promotions", "social", "forums"] {
        assert!(prompt.contains(&format!("\"providerCategory\":\"{hint}\"")), "{prompt}");
    }
    assert_eq!((prompt.matches("providerCategory").count(), prompt.matches("\"important\":true").count()), (5, 1), "{prompt}");
    // Provisional tags are never saved, synced or acted on (auto-read goes by `tag_for`).
    let mail = engine.core.mail.clone();
    let conversation = |n: usize| mail.lookup(&format!("me@gmail.com|i{n}@x")).unwrap();
    assert!(mail.tagging().tag_for(&conversation(2)).is_none());
    std::thread::sleep(Duration::from_millis(1500));
    assert!(gmail.lock().unwrap().labels.is_empty(), "no label was made for a provisional tag");
    assert!(server.requests.lock().unwrap().iter().all(|request| !request.path.ends_with("batchModify")));

    // Gmail recategorizes: the history brings the change.
    {
        let mut gmail = gmail.lock().unwrap();
        gmail.relabel("i5", &["CATEGORY_PROMOTIONS"], &[]);
        gmail.relabel("i2", &["CATEGORY_PERSONAL"], &["CATEGORY_PROMOTIONS"]);
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("the new categories", || tag(&engine, 5).0 == "newsletter" && tag(&engine, 2).0 == "people");

    // Marking a provisional conversation waiting keeps its category as the user's tag, which
    // then syncs like any correction.
    let answer = call(&engine, "mail.tag", json!({ "ids": ["me@gmail.com|i0@x"], "waiting": true })).unwrap();
    assert_eq!(answer["done"][0]["category"], "people");
    wait_until("the correction on the server", || gmail.lock().unwrap().names("i0") == names(&["People", "Waiting on you"]));

    // The model answers: its tags replace the provisional ones; the one it skips keeps its
    // stand-in, still off the server.
    let omp = fake_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp.display().to_string() })).unwrap();
    wait_until("the model's tags", || tag(&engine, 5).0 == "notification");
    assert_eq!(counts(&dir), [5]);
    // i1 people waiting, i2 people (its ask lapsed), i3 skipped, i4 and i5 notifications.
    assert_eq!(tag(&engine, 1), (json!("people"), json!(true)));
    assert_eq!(tag(&engine, 3), (json!("newsletter"), json!(false)));
    assert_eq!(tag(&engine, 4), (json!("notification"), json!(false)));
    wait_until("the model's tags on the server", || gmail.lock().unwrap().names("i4") == names(&["Notifications"]));
    assert_eq!(gmail.lock().unwrap().names("i3"), names(&[]));
    drop(engine);
    cleanup(&config);
}

#[test]
fn stands_in_with_outlook_focused_inbox() {
    let categories = Arc::new(Mutex::new(BTreeMap::from([("g1".to_string(), vec![]), ("g2".to_string(), vec![])])));
    let server = outlook(categories);
    let (engine, config) = engine_for(&server, &[("me@outlook.com", "microsoft")]);
    let dir = config.support_directory.parent().unwrap().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    let offline = offline_omp(&dir);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": offline.display().to_string() })).unwrap();
    let listed = || call(&engine, "mail.list", json!({ "limit": 10, "cached": true }));
    wait_until("the inbox", || listed().is_ok_and(|listed| listed["conversations"].as_array().unwrap().len() == 2));
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("the failed run", || tagging_failed(&engine));
    let listed = listed().unwrap();
    let category = |id: &str| {
        let conversations = listed["conversations"].as_array().unwrap();
        conversations.iter().find(|conversation| conversation["id"] == format!("me@outlook.com|{id}@outlook")).unwrap()["category"].clone()
    };
    // Focused is people, Other notifications.
    assert_eq!((category("g1"), category("g2")), (json!("people"), json!("notification")));
    let prompts = std::fs::read_to_string(dir.join("prompts")).unwrap();
    assert!(prompts.contains("\"providerCategory\":\"focused\"") && prompts.contains("\"providerCategory\":\"other\""), "{prompts}");
    drop(engine);
    cleanup(&config);
}

#[test]
fn keeps_manual_category_and_topics_when_a_reply_arrives() {
    let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<BTreeSet<_>>();
    let gmail = Arc::new(Mutex::new(Gmail {
        labels: BTreeMap::new(),
        messages: (0..5).map(|n| (format!("i{n}"), BTreeSet::from(["INBOX".to_string()]))).collect(),
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
    wait_until("the inbox", || call(&engine, "mail.list", json!({ "limit": 100, "cached": true })).is_ok_and(|listed| {
        listed["conversations"].as_array().unwrap().len() == 5
    }));

    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("initial tags", || {
        let gmail = gmail.lock().unwrap();
        gmail.names("i3").contains("Notifications") && gmail.names("i4").contains("Notifications")
    });
    assert_eq!(counts(&dir), [5]);

    // i3 gets manual category + topic; i4 gets only `waiting` changed (still AI's category).
    let c3 = "me@gmail.com|i3@x";
    let c4 = "me@gmail.com|i4@x";
    call(&engine, "mail.tag", json!({ "ids": [c3], "category": "invitation", "add-labels": ["Finance"], "waiting": false })).unwrap();
    call(&engine, "mail.tag", json!({ "ids": [c4], "waiting": true })).unwrap();
    wait_until("manual edits on the server", || {
        let gmail = gmail.lock().unwrap();
        gmail.names("i3") == names(&["Invitations", "Mailstream/Finance"])
            && gmail.names("i4") == names(&["Notifications", "Waiting on you"])
    });

    // Replies arrive in both threads: r3 (newest -> item 1 in fake_omp: people, toRespond=true,
    // label "Work") and r4 (item 2: people, lapsed ask -> waiting=false).
    {
        let mut gmail = gmail.lock().unwrap();
        gmail.messages.insert("r3".into(), BTreeSet::from(["INBOX".into()]));
        gmail.messages.insert("r4".into(), BTreeSet::from(["INBOX".into()]));
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("replies tagged", || counts(&dir).len() == 2);
    assert_eq!(counts(&dir), [5, 2]);
    wait_until("carried and retagged labels on server", || {
        let gmail = gmail.lock().unwrap();
        gmail.names("r3") == names(&["Invitations", "Waiting on you", "Mailstream/Finance"])
            && gmail.names("r4") == names(&["People"])
    });
    // c3 kept user's Invitation + Finance while AI re-judged waiting=true; c4 was re-judged.
    assert_eq!(tag(&engine, 3), (json!("invitation"), json!(true)));
    assert_eq!(topics(&engine, 3), json!(["Finance"]));
    assert_eq!(tag(&engine, 4), (json!("people"), json!(false)));
    assert_eq!(topics(&engine, 4), json!([]));

    // Switch c3 to newsletter and get a second reply s3: manual flag persists across replies,
    // keeping newsletter + Finance and forcing waiting=false despite fake_omp saying toRespond=true.
    call(&engine, "mail.tag", json!({ "ids": [c3], "category": "newsletter" })).unwrap();
    wait_until("newsletter on server", || {
        gmail.lock().unwrap().names("r3") == names(&["Newsletters", "Mailstream/Finance"])
    });
    {
        let mut gmail = gmail.lock().unwrap();
        gmail.messages.insert("s3".into(), BTreeSet::from(["INBOX".into()]));
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("second reply tagged", || counts(&dir).len() == 3);
    assert_eq!(counts(&dir), [5, 2, 1]);
    wait_until("second reply carried on server", || {
        gmail.lock().unwrap().names("s3") == names(&["Newsletters", "Mailstream/Finance"])
    });
    assert_eq!(tag(&engine, 3), (json!("newsletter"), json!(false)));
    assert_eq!(topics(&engine, 3), json!(["Finance"]));

    // Third reply t3 arrives already carrying another device's AI labels (People + Waiting + Work):
    // sync adopts it through `carry`, keeps user's Newsletter + Finance, and pushes them back
    // without calling omp.
    {
        let mut gmail = gmail.lock().unwrap();
        let people = gmail.label("People").unwrap();
        let waiting = gmail.label("Waiting on you").unwrap();
        let work = gmail.label("Mailstream/Work").unwrap();
        gmail.messages.insert("t3".into(), BTreeSet::from(["INBOX".into(), people, waiting, work]));
    }
    call(&engine, "refresh", json!({})).unwrap();
    wait_until("server-adopted reply corrected", || {
        gmail.lock().unwrap().names("t3") == names(&["Newsletters", "Mailstream/Finance"])
    });
    assert_eq!(counts(&dir), [5, 2, 1]);
    assert_eq!(tag(&engine, 3), (json!("newsletter"), json!(false)));
    assert_eq!(topics(&engine, 3), json!(["Finance"]));

    drop(engine);
    cleanup(&config);
}
