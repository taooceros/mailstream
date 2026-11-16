//! End-to-end tests of the cloud drop (MS-374) against a stand-in for `SyncDrop/worker.js`: two
//! devices of one group that are never online together sync through it, and it sees only
//! ciphertext.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use iroh::address_lookup::MemoryLookup;
use serde_json::{Value, json};

use crate::fake_server::{FakeRequest, FakeServer, Reply};
use crate::peer_sync::Network;
use crate::testkit::{TestConfig, call, config};
use crate::{Engine, OAuthConfig};

/// What the Worker holds: per group the token its first write registered and each slot's copy.
#[derive(Default)]
struct Groups {
    tokens: BTreeMap<String, String>,
    slots: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

/// The Worker's protocol (`SyncDrop/worker.js`), in memory.
fn worker() -> (FakeServer, Arc<Mutex<Groups>>) {
    let groups = Arc::new(Mutex::new(Groups::default()));
    let held = groups.clone();
    let server = FakeServer::start(move |request: &FakeRequest| {
        let mut groups = held.lock().unwrap();
        let parts: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
        let (group, slot) = match parts.as_slice() {
            ["v1", "groups", group] => (group.to_string(), None),
            ["v1", "groups", group, "slots", slot] => (group.to_string(), Some(slot.to_string())),
            _ => return Reply::bytes(404, Vec::new()),
        };
        let Some(token) = request.header("Authorization").and_then(|value| value.strip_prefix("Bearer ")) else {
            return Reply::bytes(401, Vec::new());
        };
        match groups.tokens.get(&group) {
            Some(owner) if owner != token => return Reply::bytes(403, Vec::new()),
            Some(_) => {}
            None if request.method == "PUT" => {
                groups.tokens.insert(group.clone(), token.to_string());
            }
            None => {}
        }
        let held = groups.slots.entry(group).or_default();
        match (request.method.as_str(), slot) {
            ("GET", None) => {
                let slots: Vec<Value> = held
                    .iter()
                    .map(|(slot, data)| {
                        json!({ "slot": slot, "data": base64::engine::general_purpose::STANDARD.encode(data), "updated": 0 })
                    })
                    .collect();
                Reply::json(json!({ "slots": slots }))
            }
            ("PUT", Some(slot)) => {
                held.insert(slot, request.body.clone());
                Reply::bytes(204, Vec::new())
            }
            ("DELETE", Some(slot)) => {
                held.remove(&slot);
                Reply::bytes(204, Vec::new())
            }
            _ => Reply::bytes(405, Vec::new()),
        }
    });
    (server, groups)
}

/// A device that reaches no other device over iroh: its own lookup, loopback only.
fn device() -> TestConfig {
    let mut cfg = config(OAuthConfig::default());
    cfg.device_sync = Some(Network::Loopback(MemoryLookup::new()));
    cfg
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn trusted(engine: &Engine) -> Value {
    call(engine, "config.get", json!({ "key": "remoteContent.trustedSenders" })).unwrap()["value"].clone()
}

fn copies(groups: &Mutex<Groups>) -> Vec<Vec<u8>> {
    groups.lock().unwrap().slots.values().flat_map(|slots| slots.values().cloned()).collect()
}

#[test]
fn devices_never_online_together_sync_through_the_drop() {
    let (server, groups) = worker();
    let (cfg_a, cfg_b) = (device(), device());

    // A starts the group, turns the drop on and trusts two senders, then goes offline.
    let engine_a = Engine::start(cfg_a.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    let ticket = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    let status = call(&engine_a, "sync.cloud_enable", json!({ "url": format!("{}/", server.base) })).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(true));
    assert_eq!(status["cloud"]["url"], json!(server.base));
    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com", "bob@y.com"] }))
        .unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();
    let status = call(&engine_a, "sync.now", json!({})).unwrap();
    assert_eq!(status["cloud"]["problem"], Value::Null, "{status}");
    assert!(status["cloud"]["lastUpload"].is_string(), "{status}");
    let held = copies(&groups);
    assert_eq!(held.len(), 1);
    assert!(held[0].starts_with(b"msd1"));
    for plain in [&b"alice@x.com"[..], b"bob@y.com", b"autoRead", b"trusted"] {
        assert!(!held[0].windows(plain.len()).any(|window| window == plain), "the Worker sees plaintext");
    }
    let endpoint_a = status["endpointId"].as_str().unwrap().to_string();
    drop(engine_a);

    // B joins with A's ticket while A is offline, turns the drop on and gets A's settings.
    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();
    assert_eq!(trusted(&engine_b), json!([]));
    let status = call(&engine_b, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
    assert_eq!(status["cloud"]["problem"], Value::Null, "{status}");
    assert!(status["cloud"]["lastReceived"].is_string(), "{status}");
    assert!(status["devices"].as_array().unwrap().iter().any(|device| device["id"] == json!(endpoint_a)), "{status}");
    assert_eq!(trusted(&engine_b), json!(["alice@x.com", "bob@y.com"]));
    assert_eq!(call(&engine_b, "config.get", json!({ "key": "autoRead.enabled" })).unwrap()["value"], json!(true));
    until("B's copy", || copies(&groups).len() == 2);

    // B untrusts bob and goes offline; A comes back and takes the tombstone on its own.
    call(&engine_b, "config.remove", json!({ "key": "remoteContent.trustedSenders", "values": ["bob@y.com"] })).unwrap();
    call(&engine_b, "sync.now", json!({})).unwrap();
    drop(engine_b);
    let engine_a = Engine::start(cfg_a.clone());
    until("A to take B's untrust from the drop", || trusted(&engine_a) == json!(["alice@x.com"]));

    // Turning the drop off deletes A's copy; B's stays.
    let status = call(&engine_a, "sync.cloud_disable", json!({})).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(false));
    assert_eq!(status["cloud"]["problem"], Value::Null, "{status}");
    assert_eq!(copies(&groups).len(), 1);
    let gets = server.requests.lock().unwrap().iter().filter(|request| request.method == "GET").count();
    call(&engine_a, "sync.now", json!({})).unwrap();
    let after = server.requests.lock().unwrap().iter().filter(|request| request.method == "GET").count();
    assert_eq!(after, gets, "the drop stays off");
    drop(engine_a);
}

#[test]
fn the_drop_refuses_plain_http_and_reports_an_unreachable_worker() {
    let cfg = device();
    let engine = Engine::start(cfg.clone());
    call(&engine, "sync.enable", json!({})).unwrap();
    let error = call(&engine, "sync.cloud_enable", json!({ "url": "http://drop.example.workers.dev" })).unwrap_err();
    assert_eq!(error.code, "invalid");
    // Nothing listens on port 9 of this machine.
    let status = call(&engine, "sync.cloud_enable", json!({ "url": "http://127.0.0.1:9" })).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(true));
    assert!(status["cloud"]["problem"].as_str().unwrap().contains("Couldn't reach"), "{status}");
    drop(engine);
}
