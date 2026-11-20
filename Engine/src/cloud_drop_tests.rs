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

    // B pairs with A (a pairing code needs its maker online, MS-376) and goes offline at once.
    let engine_a = Engine::start(cfg_a.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    let code = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_b, "sync.join", json!({ "ticket": code })).unwrap();
    drop(engine_b);

    // A turns the drop on and trusts two senders, then goes offline.
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

    // B comes back while A is offline, turns the drop on and gets A's settings.
    let engine_b = Engine::start(cfg_b.clone());
    assert_eq!(trusted(&engine_b), json!([]));
    // Sync opens in the background on start; the status opens it now.
    assert_eq!(call(&engine_b, "sync.status", json!({})).unwrap()["enabled"], json!(true));
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

/// MS-396: removing a device moves the group's copies to a place derived from a key the removed
/// device never gets, and deletes them from the old one, where the removed device's writes stay.
#[test]
fn removing_a_device_moves_the_drop_where_it_cannot_follow() {
    let (server, groups) = worker();
    let lookup = MemoryLookup::new();
    let paired = || {
        let mut cfg = config(OAuthConfig::default());
        cfg.device_sync = Some(Network::Loopback(lookup.clone()));
        cfg
    };
    let configs = [paired(), paired(), paired()];
    let engines = configs.each_ref().map(|cfg| Engine::start((**cfg).clone()));
    let [engine_a, engine_b, engine_c] = &engines;
    call(engine_a, "sync.enable", json!({})).unwrap();
    for engine in [engine_b, engine_c] {
        let code = call(engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
        call(engine, "sync.join", json!({ "ticket": code })).unwrap();
    }
    for engine in &engines {
        let status = call(engine, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
        assert_eq!(status["cloud"]["problem"], Value::Null, "{status}");
    }
    let group_slots = || groups.lock().unwrap().slots.iter().map(|(group, slots)| (group.clone(), slots.len())).collect::<Vec<_>>();
    until("the three copies in one group", || group_slots().iter().any(|(_, slots)| *slots == 3));
    let old_group = group_slots().into_iter().find(|(_, slots)| *slots == 3).unwrap().0;

    let id_c = call(engine_c, "sync.status", json!({})).unwrap()["endpointId"].as_str().unwrap().to_string();
    let status = call(engine_a, "sync.remove", json!({ "device": id_c })).unwrap();
    assert_eq!(status["cloud"]["problem"], Value::Null, "{status}");
    call(engine_b, "sync.now", json!({})).unwrap();
    until("A's and B's copies to move, and leave the old place", || {
        let held = group_slots();
        held.iter().any(|(group, slots)| *group != old_group && *slots == 2)
            && held.iter().all(|(group, slots)| *group != old_group || *slots <= 1)
    });

    // What C writes now goes only to the old place, which no device of the group reads.
    call(engine_c, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["mallory@x.com"] })).unwrap();
    call(engine_c, "sync.now", json!({})).unwrap();
    call(engine_a, "sync.now", json!({})).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(trusted(engine_a), json!([]), "C's write reached A");
    let held = groups.lock().unwrap();
    assert_eq!(held.slots.get(&old_group).map(BTreeMap::len), Some(1), "only C's copy stays at the old place");
    drop(held);
    drop(engines);
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

/// MS-419: Cloudflare fallback is per sync group: joining devices inherit it, changing the
/// Worker URL or turning it off replicates to every device in the group, and old slots are
/// deleted from the Worker.
#[test]
fn cloud_fallback_syncs_across_group_and_inherits_on_join() {
    let (server1, groups1) = worker();
    let (server2, groups2) = worker();
    let lookup = MemoryLookup::new();
    let paired = || {
        let mut cfg = config(OAuthConfig::default());
        cfg.device_sync = Some(Network::Loopback(lookup.clone()));
        cfg
    };
    let (cfg_a, cfg_b) = (paired(), paired());
    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());

    call(&engine_a, "sync.enable", json!({})).unwrap();
    let status = call(&engine_a, "sync.cloud_enable", json!({ "url": server1.base })).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(true));
    assert_eq!(status["cloud"]["url"], json!(server1.base));
    until("A's copy on server1", || copies(&groups1).len() == 1);

    // B joins A without calling sync.cloud_enable itself and inherits the group's Worker.
    let code = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    call(&engine_b, "sync.join", json!({ "ticket": code })).unwrap();
    until("B to inherit server1 and upload its copy", || {
        let status = call(&engine_b, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(true)
            && status["cloud"]["url"] == json!(server1.base)
            && copies(&groups1).len() == 2
    });

    // B switches the group's Worker to server2; A follows and both delete their slots on server1.
    let status = call(&engine_b, "sync.cloud_enable", json!({ "url": server2.base })).unwrap();
    assert_eq!(status["cloud"]["url"], json!(server2.base));
    until("A to follow server2 and both to leave server1", || {
        let status = call(&engine_a, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(true)
            && status["cloud"]["url"] == json!(server2.base)
            && copies(&groups2).len() == 2
            && copies(&groups1).is_empty()
    });

    // A disables cloud fallback for the group; B follows and deletes its copy from server2.
    let status = call(&engine_a, "sync.cloud_disable", json!({})).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(false));
    assert_eq!(status["cloud"]["url"], json!(server2.base));
    until("B to disable cloud fallback and delete its copy", || {
        let status = call(&engine_b, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(false)
            && status["cloud"]["url"] == json!(server2.base)
            && copies(&groups2).is_empty()
    });

    // Re-enabling without passing a URL reuses the group's saved Worker URL.
    let status = call(&engine_a, "sync.cloud_enable", json!({})).unwrap();
    assert_eq!(status["cloud"]["enabled"], json!(true));
    assert_eq!(status["cloud"]["url"], json!(server2.base));
    until("B to re-enable with the group's saved URL", || {
        let status = call(&engine_b, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(true)
            && status["cloud"]["url"] == json!(server2.base)
            && copies(&groups2).len() == 2
    });

    drop(engine_b);
    drop(engine_a);
}

/// MS-419: `sync.reset` starts the new sync group with Cloudflare fallback off and deletes the
/// old group's copy; joining another group whose fallback is off also turns it off and cleans up.
#[test]
fn sync_reset_and_switching_groups_isolate_cloud_fallback() {
    let (server, groups) = worker();
    let lookup = MemoryLookup::new();
    let paired = || {
        let mut cfg = config(OAuthConfig::default());
        cfg.device_sync = Some(Network::Loopback(lookup.clone()));
        cfg
    };
    let (cfg_a, cfg_c) = (paired(), paired());
    let engine_a = Engine::start(cfg_a.clone());
    let engine_c = Engine::start(cfg_c.clone());

    call(&engine_a, "sync.enable", json!({})).unwrap();
    call(&engine_a, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
    until("A's copy before reset", || copies(&groups).len() == 1);

    // Reset creates a fresh sync group: cloud fallback resets to off + default URL, and old copy is deleted.
    let reset = call(&engine_a, "sync.reset", json!({ "yes": true })).unwrap();
    assert_eq!(reset["cloud"]["enabled"], json!(false));
    assert_eq!(reset["cloud"]["url"], json!(crate::cloud_drop::DEFAULT_WORKER_URL));
    assert_eq!(reset["cloud"]["lastUpload"], Value::Null);
    assert_eq!(reset["cloud"]["problem"], Value::Null);
    assert!(copies(&groups).is_empty(), "old group's copy should be deleted on reset");

    // Re-enable on A's new group, then join C's group (where cloud fallback is off).
    call(&engine_a, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
    until("A's copy on new group", || copies(&groups).len() == 1);
    call(&engine_c, "sync.enable", json!({})).unwrap();
    let code = call(&engine_c, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    let joined = call(&engine_a, "sync.join", json!({ "ticket": code })).unwrap();
    assert_eq!(joined["cloud"]["enabled"], json!(false));
    assert_eq!(joined["cloud"]["url"], json!(crate::cloud_drop::DEFAULT_WORKER_URL));
    until("A's old group copy deleted after joining C's group", || copies(&groups).is_empty());

    drop(engine_c);
    drop(engine_a);
}

/// MS-419: cloud fallback commands require a sync group, and a pre-MS-419 `cloud.json` (without
/// `namespace`) migrates into the device's existing sync group once on start.
#[test]
fn cloud_commands_require_a_sync_group_and_migrate_legacy_cloud_json() {
    let (server, groups) = worker();
    let lookup = MemoryLookup::new();
    let paired = || {
        let mut cfg = config(OAuthConfig::default());
        cfg.device_sync = Some(Network::Loopback(lookup.clone()));
        cfg
    };
    let (cfg_a, cfg_b) = (paired(), paired());

    // Without a sync group, cloud commands fail with `no_group`.
    let engine_a = Engine::start(cfg_a.clone());
    assert_eq!(call(&engine_a, "sync.cloud_enable", json!({ "url": server.base })).unwrap_err().code, "no_group");
    assert_eq!(call(&engine_a, "sync.cloud_disable", json!({})).unwrap_err().code, "no_group");

    // Create a group, stop, write a legacy pre-MS-419 cloud.json (no `namespace` field), restart.
    call(&engine_a, "sync.enable", json!({})).unwrap();
    drop(engine_a);
    std::fs::write(
        cfg_a.support_directory.join("peer-sync").join("cloud.json"),
        serde_json::to_vec_pretty(&json!({ "url": server.base, "enabled": true })).unwrap(),
    )
    .unwrap();

    let engine_a = Engine::start(cfg_a.clone());
    until("legacy cloud.json to migrate into A's sync group", || {
        let status = call(&engine_a, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(true)
            && status["cloud"]["url"] == json!(server.base)
            && copies(&groups).len() == 1
    });

    // A device joining A now inherits the migrated setting automatically.
    let engine_b = Engine::start(cfg_b.clone());
    let code = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    call(&engine_b, "sync.join", json!({ "ticket": code })).unwrap();
    until("B to inherit migrated cloud fallback from A", || {
        let status = call(&engine_b, "sync.status", json!({})).unwrap();
        status["cloud"]["enabled"] == json!(true) && status["cloud"]["url"] == json!(server.base)
    });

    drop(engine_b);
    drop(engine_a);
}

/// MS-429: when an offline device restarts with an older local edit in settings.json, its startup
/// reconcile must stamp that edit with its real modification time so a newer edit waiting in the
/// CloudDrop still wins instead of being overwritten by startup's wall clock.
#[test]
fn offline_cloud_drop_respects_newer_edit_over_stale_local_startup_push() {
    use std::time::SystemTime;

    let (server, groups) = worker();
    let (cfg_a, cfg_b) = (device(), device());

    // Pair A and B, enable CloudDrop, sync baseline autoRead.days = 7 on both, then stop both.
    let engine_a = Engine::start(cfg_a.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    call(&engine_a, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.days", "value": "7" })).unwrap();
    call(&engine_a, "sync.now", json!({})).unwrap();

    let code = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_b, "sync.join", json!({ "ticket": code })).unwrap();
    call(&engine_b, "sync.cloud_enable", json!({ "url": server.base })).unwrap();
    until("B to hold baseline autoRead.days = 7 and upload", || {
        call(&engine_b, "config.get", json!({ "key": "autoRead.days" })).unwrap()["value"] == json!(7)
            && copies(&groups).len() == 2
    });
    drop(engine_a);
    drop(engine_b);

    // A gets an older offline edit (autoRead.days = 14, mtime = now - 60s).
    crate::run_settings_command(
        &cfg_a.support_directory,
        "config.set",
        json!({ "key": "autoRead.days", "value": "14" }).as_object().unwrap(),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(cfg_a.support_directory.join("settings.json"))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();

    // B comes online alone, makes a newer edit (autoRead.days = 21 at now), uploads, and stops.
    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_b, "config.set", json!({ "key": "autoRead.days", "value": "21" })).unwrap();
    call(&engine_b, "sync.now", json!({})).unwrap();
    drop(engine_b);

    // A starts alone: startup reconcile runs before CloudDrop exchange, then CloudDrop brings B's
    // newer 21 which must win over A's older 14.
    let engine_a = Engine::start(cfg_a.clone());
    until("A to adopt B's newer autoRead.days = 21 from CloudDrop", || {
        call(&engine_a, "config.get", json!({ "key": "autoRead.days" })).unwrap()["value"] == json!(21)
    });
    drop(engine_a);
}
