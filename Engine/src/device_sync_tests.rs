//! End-to-end tests of `device_sync` bridging `PeerSync` and `settings.json` across full
//! `Engine` instances on the loopback interface (MS-360).
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use serde_json::{Value, json};

use crate::keychain::Keychain;
use crate::mail_tests::{GmailCounters, gmail_server};
use crate::oauth::OAuthClient;
use crate::peer_sync::{Network, PeerSync};
use crate::testkit::{TestConfig, call, config, mail_account, sign_in_all};
use crate::{Engine, OAuthConfig, run_settings_command};

fn sync_config(lookup: &MemoryLookup) -> TestConfig {
    let mut cfg = config(OAuthConfig::default());
    cfg.device_sync = Some(Network::Loopback(lookup.clone()));
    cfg
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn setting_value(engine: &Engine, key: &str) -> Value {
    call(engine, "config.get", json!({ "key": key })).unwrap()["value"].clone()
}

#[test]
fn paired_engines_sync_whitelisted_settings_and_remote_content() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let counters = Arc::new(GmailCounters {
        tokens: AtomicUsize::new(0),
        labels: AtomicUsize::new(0),
        listings: AtomicUsize::new(0),
    });
    let server = gmail_server(counters);
    let mut cfg_b = crate::testkit::server_config(&server);
    cfg_b.device_sync = Some(Network::Loopback(lookup.clone()));
    sign_in_all(&cfg_b, &[mail_account("me@gmail.com", "google")]);

    // Pre-existing machine-local settings on both engines must never be overwritten.
    run_settings_command(&cfg_a.support_directory, "config.set", json!({ "key": "gpui.listWidth", "value": "350" }).as_object().unwrap()).unwrap();
    run_settings_command(&cfg_a.support_directory, "config.set", json!({ "key": "sync.pollSeconds", "value": "120" }).as_object().unwrap()).unwrap();
    run_settings_command(&cfg_b.support_directory, "config.set", json!({ "key": "gpui.listWidth", "value": "520" }).as_object().unwrap()).unwrap();
    run_settings_command(&cfg_b.support_directory, "config.set", json!({ "key": "aiTagging.model", "value": "big" }).as_object().unwrap()).unwrap();

    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());

    let (b_tx, b_events) = mpsc::channel();
    engine_b.subscribe(move |event| {
        let _ = b_tx.send(event);
    });

    // Wait for B to load its inbox so we can test `mail.remote_content` on alerts@x.com.
    until("B to load mailboxes", || {
        call(&engine_b, "mailboxes", json!({})).unwrap()["me@gmail.com"]
            .as_array()
            .is_some_and(|boxes| !boxes.is_empty())
    });
    let listing = call(&engine_b, "mail.list", json!({ "limit": 10 })).unwrap();
    let alert_id = listing["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["subject"] == "Security alert")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (allowed, untrusted) = engine_b.remote_content(&alert_id).unwrap();
    assert!(!allowed);
    assert_eq!(untrusted.len(), 1);
    assert_eq!(untrusted[0].normalized_email(), "alerts@x.com");

    // Before pairing, both report available = true, enabled = false.
    let status_a = call(&engine_a, "sync.status", json!({})).unwrap();
    assert_eq!((status_a["available"].clone(), status_a["enabled"].clone()), (json!(true), json!(false)));

    // A enables sync and generates a pairing ticket; B joins with it.
    let enabled_a = call(&engine_a, "sync.enable", json!({})).unwrap();
    assert_eq!(enabled_a["enabled"], json!(true));
    let ticket = call(&engine_a, "sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    let joined_b = call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();
    assert_eq!(joined_b["enabled"], json!(true));
    assert_eq!(joined_b["namespace"], enabled_a["namespace"]);

    until("both engines to see 2 devices", || {
        let a_devs = call(&engine_a, "sync.status", json!({})).unwrap()["devices"].as_array().unwrap().len();
        let b_devs = call(&engine_b, "sync.status", json!({})).unwrap()["devices"].as_array().unwrap().len();
        a_devs == 2 && b_devs == 2
    });
    while b_events.try_recv().is_ok() {}

    // A trusts alerts@x.com and changes whitelisted settings; B applies them and allows remote content.
    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["alerts@x.com"] })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.days", "value": "7" })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine_a, "config.add", json!({ "key": "aiTagging.localOnlyAccounts", "values": ["me@gmail.com"] })).unwrap();

    until("B to receive all whitelisted settings from A", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!(["alerts@x.com"])
            && setting_value(&engine_b, "autoRead.enabled") == json!(true)
            && setting_value(&engine_b, "autoRead.days") == json!(7)
            && setting_value(&engine_b, "aiTagging.enabled") == json!(false)
            && setting_value(&engine_b, "aiTagging.localOnlyAccounts") == json!(["me@gmail.com"])
    });
    let mut saw_settings_event = false;
    while let Ok(event) = b_events.try_recv() {
        if event == "settings" {
            saw_settings_event = true;
        }
    }
    assert!(saw_settings_event, "B should emit \"settings\" when remote updates land");

    let (allowed, untrusted) = engine_b.remote_content(&alert_id).unwrap();
    assert!(allowed, "trusted sender from A must allow remote content on B");
    assert!(untrusted.is_empty());

    // Machine-local settings stayed independent on both sides.
    assert_eq!(setting_value(&engine_a, "gpui.listWidth"), json!(350));
    assert_eq!(setting_value(&engine_a, "sync.pollSeconds"), json!(120));
    assert_eq!(setting_value(&engine_a, "aiTagging.model"), json!("smol"));
    assert_eq!(setting_value(&engine_b, "gpui.listWidth"), json!(520));
    assert_eq!(setting_value(&engine_b, "sync.pollSeconds"), json!(90));
    assert_eq!(setting_value(&engine_b, "aiTagging.model"), json!("big"));

    // A untrusts alerts@x.com; B removes it and blocks remote content again.
    call(&engine_a, "config.remove", json!({ "key": "remoteContent.trustedSenders", "values": ["alerts@x.com"] })).unwrap();
    until("B to remove alerts@x.com", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!([])
    });
    let (allowed, _) = engine_b.remote_content(&alert_id).unwrap();
    assert!(!allowed);

    // Direct edit to B's settings.json (like `mailstream config` / editor) propagates to A.
    let mut raw_b: Value = serde_json::from_str(&std::fs::read_to_string(cfg_b.support_directory.join("settings.json")).unwrap()).unwrap();
    raw_b["autoRead.days"] = json!(14);
    std::fs::write(cfg_b.support_directory.join("settings.json"), serde_json::to_string_pretty(&raw_b).unwrap()).unwrap();
    until("A to pick up B's settings.json edit", || {
        setting_value(&engine_a, "autoRead.days") == json!(14)
    });

    // Disable (pause) on B stops sync until re-enabled.
    let disabled = call(&engine_b, "sync.disable", json!({})).unwrap();
    assert_eq!((disabled["paused"].clone(), disabled["enabled"].clone()), (json!(true), json!(false)));
    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["carol@x.com"] })).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(setting_value(&engine_b, "remoteContent.trustedSenders"), json!([]));

    call(&engine_b, "sync.enable", json!({})).unwrap();
    call(&engine_b, "sync.now", json!({})).unwrap();
    until("B to sync carol@x.com after resume", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!(["carol@x.com"])
    });

    // Reset requires confirmation and isolates into a new namespace while keeping carried settings.
    let confirm_err = call(&engine_b, "sync.reset", json!({})).unwrap_err();
    assert_eq!(confirm_err.code, "confirm");
    let reset_b = call(&engine_b, "sync.reset", json!({ "yes": true })).unwrap();
    assert_ne!(reset_b["namespace"], enabled_a["namespace"]);
    assert_eq!(setting_value(&engine_b, "remoteContent.trustedSenders"), json!(["carol@x.com"]));
    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["dan@x.com"] })).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(setting_value(&engine_b, "remoteContent.trustedSenders"), json!(["carol@x.com"]));

    drop(engine_b);
    drop(engine_a);
}

#[test]
fn second_engine_on_same_folder_reports_busy_and_recovers_after_first_drops() {
    let lookup = MemoryLookup::new();
    let cfg = sync_config(&lookup);
    let first = Engine::start(cfg.clone());
    let enabled = call(&first, "sync.enable", json!({})).unwrap();
    assert_eq!(enabled["enabled"], json!(true));

    let second = Engine::start(cfg.clone());
    let status = call(&second, "sync.status", json!({})).unwrap();
    assert_eq!(status["available"], json!(false));
    let err = call(&second, "sync.enable", json!({})).unwrap_err();
    assert_eq!(err.code, "busy");

    // Once first drops and releases `peer-sync/lock`, second can take over.
    drop(first);
    let recovered = call(&second, "sync.enable", json!({})).unwrap();
    assert_eq!((recovered["available"].clone(), recovered["enabled"].clone()), (json!(true), json!(true)));
    drop(second);
}

#[test]
fn sync_stays_closed_without_a_group_and_while_paused() {
    let lookup = MemoryLookup::new();
    let cfg = sync_config(&lookup);
    let folder = cfg.support_directory.join("peer-sync");

    // Never enabled or paired: no endpoint is bound, so no relay is reached (MS-370).
    let engine = Engine::start(cfg.clone());
    std::thread::sleep(Duration::from_millis(300));
    let idle = call(&engine, "sync.status", json!({})).unwrap();
    assert_eq!(
        (idle["available"].clone(), idle["enabled"].clone(), idle["endpointId"].clone()),
        (json!(true), json!(false), Value::Null)
    );
    assert!(!folder.exists(), "sync.status must not open device sync");
    assert_eq!(call(&engine, "sync.now", json!({})).unwrap_err().code, "no_group");
    assert_eq!(call(&engine, "sync.pair_ticket", json!({})).unwrap_err().code, "no_group");
    let disabled = call(&engine, "sync.disable", json!({})).unwrap();
    assert_eq!((disabled["available"].clone(), disabled["enabled"].clone()), (json!(true), json!(false)));
    assert!(!folder.exists(), "commands without a group must not open device sync");

    // Enabling opens it; pausing closes it again and releases `peer-sync/lock`.
    let enabled = call(&engine, "sync.enable", json!({})).unwrap();
    assert_eq!(enabled["enabled"], json!(true));
    assert!(PeerSync::busy(&cfg.support_directory));
    let paused = call(&engine, "sync.disable", json!({})).unwrap();
    assert_eq!(
        (paused["available"].clone(), paused["paused"].clone(), paused["endpointId"].clone()),
        (json!(true), json!(true), enabled["endpointId"].clone())
    );
    assert!(!PeerSync::busy(&cfg.support_directory), "pausing must close device sync");
    drop(engine);

    // A paused device stays closed across a restart until enabled.
    let engine = Engine::start(cfg.clone());
    std::thread::sleep(Duration::from_millis(300));
    let status = call(&engine, "sync.status", json!({})).unwrap();
    assert_eq!((status["available"].clone(), status["paused"].clone()), (json!(true), json!(true)));
    assert_eq!(status["namespace"], enabled["namespace"]);
    assert!(!PeerSync::busy(&cfg.support_directory), "a paused device must not open device sync");
    assert_eq!(call(&engine, "sync.now", json!({})).unwrap_err().code, "paused");
    let resumed = call(&engine, "sync.enable", json!({})).unwrap();
    assert_eq!((resumed["enabled"].clone(), resumed["namespace"].clone()), (json!(true), enabled["namespace"].clone()));
    drop(engine);
}

#[test]
fn offline_cli_edit_syncs_after_restart() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let cfg_b = sync_config(&lookup);

    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    let ticket = call(&engine_a, "sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();

    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com"] })).unwrap();
    until("B to receive alice@x.com", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!(["alice@x.com"])
    });

    // Stop A and edit its settings.json offline (as `mailstream config` does).
    drop(engine_a);
    run_settings_command(
        &cfg_a.support_directory,
        "config.remove",
        json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com"] }).as_object().unwrap(),
    )
    .unwrap();
    run_settings_command(
        &cfg_a.support_directory,
        "config.add",
        json!({ "key": "remoteContent.trustedSenders", "values": ["bob@x.com"] }).as_object().unwrap(),
    )
    .unwrap();

    // Restart A: persisted bridged.json lets it detect the offline removal + addition and push both.
    let engine_a2 = Engine::start(cfg_a.clone());
    until("B to converge to bob@x.com without alice@x.com", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!(["bob@x.com"])
    });

    drop(engine_a2);
    drop(engine_b);
}

#[test]
fn joining_device_merges_its_settings_and_the_group_wins_conflicts() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let cfg_b = sync_config(&lookup);

    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());

    // A's group: autoRead.days 7, aiTagging off, alice trusted, carol trusted and then untrusted.
    call(&engine_a, "sync.enable", json!({})).unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.days", "value": "7" })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com", "carol@x.com"] }))
        .unwrap();
    let bridged_carol = || {
        let data = std::fs::read(cfg_a.support_directory.join("peer-sync/bridged.json")).unwrap_or_default();
        serde_json::from_slice::<Value>(&data).ok().map(|bridged| bridged["trusted"]["carol@x.com"].clone())
    };
    until("A to trust carol@x.com in its group", || bridged_carol() == Some(json!(true)));
    call(&engine_a, "config.remove", json!({ "key": "remoteContent.trustedSenders", "values": ["carol@x.com"] })).unwrap();
    until("A to untrust carol@x.com in its group", || bridged_carol() == Some(json!(false)));

    // B, in no group yet, set its own: autoRead.days 14 (conflicts with the group), autoRead on
    // (only B), bob trusted (only B) and carol trusted (the group untrusted her).
    call(&engine_b, "config.set", json!({ "key": "autoRead.days", "value": "14" })).unwrap();
    call(&engine_b, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();
    call(&engine_b, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["bob@x.com", "carol@x.com"] }))
        .unwrap();

    let ticket = call(&engine_a, "sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();

    let merged_trusted = json!(["alice@x.com", "bob@x.com"]);
    until("both engines to hold the merged settings", || {
        // The group's values never give way to the joining device's.
        assert_eq!(setting_value(&engine_a, "autoRead.days"), json!(7));
        assert_eq!(setting_value(&engine_a, "aiTagging.enabled"), json!(false));
        assert!(
            !setting_value(&engine_a, "remoteContent.trustedSenders")
                .as_array()
                .unwrap()
                .contains(&json!("carol@x.com"))
        );
        [&engine_a, &engine_b].iter().all(|engine| {
            setting_value(engine, "autoRead.days") == json!(7)
                && setting_value(engine, "autoRead.enabled") == json!(true)
                && setting_value(engine, "aiTagging.enabled") == json!(false)
                && setting_value(engine, "remoteContent.trustedSenders") == merged_trusted
        })
    });

    // After the merge, B's own edits are newer than the group's again.
    call(&engine_b, "config.set", json!({ "key": "autoRead.days", "value": "21" })).unwrap();
    call(&engine_b, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["carol@x.com"] }))
        .unwrap();
    until("A to take B's edits after the merge", || {
        setting_value(&engine_a, "autoRead.days") == json!(21)
            && setting_value(&engine_a, "remoteContent.trustedSenders")
                == json!(["alice@x.com", "bob@x.com", "carol@x.com"])
    });

    drop(engine_b);
    drop(engine_a);
}

#[test]
fn invalid_synced_keys_in_settings_file_do_not_wipe_group_or_local_edit() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let cfg_b = sync_config(&lookup);

    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());
    let (a_tx, a_events) = mpsc::channel();
    engine_a.subscribe(move |event| {
        let _ = a_tx.send(event);
    });

    call(&engine_a, "sync.enable", json!({})).unwrap();
    let ticket = call(&engine_a, "sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();

    call(
        &engine_a,
        "config.add",
        json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com", "bob@x.com"] }),
    )
    .unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.days", "value": "7" })).unwrap();
    until("B to receive initial trustedSenders and autoRead.days", || {
        setting_value(&engine_b, "remoteContent.trustedSenders") == json!(["alice@x.com", "bob@x.com"])
            && setting_value(&engine_b, "autoRead.days") == json!(7)
    });
    while a_events.try_recv().is_ok() {}

    // Hand-edit A's settings.json with one malformed address, an out-of-range autoRead.days,
    // and a valid change to aiTagging.enabled (MS-368).
    let path_a = cfg_a.support_directory.join("settings.json");
    std::fs::write(
        &path_a,
        serde_json::to_string_pretty(&json!({
            "remoteContent.trustedSenders": ["alice@x.com", "bob@x.com", "missing-at-sign"],
            "autoRead.days": 999,
            "aiTagging.enabled": false,
        }))
        .unwrap(),
    )
    .unwrap();

    // Valid keys still bridge, proving A ran reconcile; invalid keys report a problem and sit out.
    until("B to receive aiTagging.enabled and A to report the invalid settings", || {
        setting_value(&engine_b, "aiTagging.enabled") == json!(false)
            && call(&engine_a, "sync.status", json!({})).unwrap()["problem"].is_string()
    });
    let problem = call(&engine_a, "sync.status", json!({})).unwrap()["problem"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(problem.contains("remoteContent.trustedSenders"), "unexpected problem: {problem}");
    assert!(problem.contains("autoRead.days"), "unexpected problem: {problem}");
    let mut saw_device_sync_event = false;
    while let Ok(event) = a_events.try_recv() {
        if event == "deviceSync" {
            saw_device_sync_event = true;
        }
    }
    assert!(saw_device_sync_event, "A should emit \"deviceSync\" when problem appears");

    // B must NOT lose its trusted senders or reset autoRead.days to default 3.
    assert_eq!(setting_value(&engine_b, "remoteContent.trustedSenders"), json!(["alice@x.com", "bob@x.com"]));
    assert_eq!(setting_value(&engine_b, "autoRead.days"), json!(7));

    // Remote edits from B arrive while A's file still has invalid keys: valid keys apply on A,
    // while A's raw invalid entries stay untouched rather than being overwritten.
    call(&engine_b, "config.remove", json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com"] }))
        .unwrap();
    call(&engine_b, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["carol@x.com"] }))
        .unwrap();
    call(&engine_b, "config.set", json!({ "key": "autoRead.days", "value": "14" })).unwrap();
    call(&engine_b, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();
    until("A to apply valid remote autoRead.enabled", || {
        setting_value(&engine_a, "autoRead.enabled") == json!(true)
    });
    let raw_mid: Value = serde_json::from_str(&std::fs::read_to_string(&path_a).unwrap()).unwrap();
    assert_eq!(
        raw_mid["remoteContent.trustedSenders"],
        json!(["alice@x.com", "bob@x.com", "missing-at-sign"])
    );
    assert_eq!(raw_mid["autoRead.days"], json!(999));
    assert_eq!(setting_value(&engine_b, "remoteContent.trustedSenders"), json!(["bob@x.com", "carol@x.com"]));
    assert_eq!(setting_value(&engine_b, "autoRead.days"), json!(14));

    // Fix A's file: repair the mistyped address to dave@x.com and set autoRead.days to 30.
    // Kept baseline entries (alice, bob) let B's tombstone for alice and addition of carol merge
    // cleanly with A's new dave@x.com, and A's new 30 overrides B's 14.
    while a_events.try_recv().is_ok() {}
    let mut raw_fixed = raw_mid;
    raw_fixed["remoteContent.trustedSenders"] = json!(["alice@x.com", "bob@x.com", "dave@x.com"]);
    raw_fixed["autoRead.days"] = json!(30);
    std::fs::write(&path_a, serde_json::to_string_pretty(&raw_fixed).unwrap()).unwrap();

    until("both engines to converge after settings.json is fixed", || {
        let expected = json!(["bob@x.com", "carol@x.com", "dave@x.com"]);
        setting_value(&engine_a, "remoteContent.trustedSenders") == expected
            && setting_value(&engine_b, "remoteContent.trustedSenders") == expected
            && setting_value(&engine_a, "autoRead.days") == json!(30)
            && setting_value(&engine_b, "autoRead.days") == json!(30)
            && call(&engine_a, "sync.status", json!({})).unwrap()["problem"].is_null()
    });
    let mut saw_clear_event = false;
    while let Ok(event) = a_events.try_recv() {
        if event == "deviceSync" {
            saw_clear_event = true;
        }
    }
    assert!(saw_clear_event, "A should emit \"deviceSync\" when problem clears");

    drop(engine_b);
    drop(engine_a);
}

/// Every file under `directory` whose bytes contain `needle`.
fn files_containing(directory: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_containing(&path, needle));
        } else if std::fs::read(&path).is_ok_and(|data| data.windows(needle.len()).any(|window| window == needle)) {
            found.push(path);
        }
    }
    found
}

fn account_ids(engine: &Engine) -> Vec<String> {
    call(engine, "accounts.list", json!({})).unwrap().as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
}

/// MS-376: a device that joins gets the group's accounts and their refresh tokens without
/// signing in; the tokens cross only sealed to its own key; renames and removals follow.
#[test]
fn joining_device_gets_accounts_with_tokens_sealed_to_it() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let cfg_b = sync_config(&lookup);
    sign_in_all(&cfg_a, &[mail_account("me@gmail.com", "google")]);
    let keychain = |cfg: &TestConfig| Keychain::new(cfg.keychain_service.clone(), cfg.keychain_directory.clone());
    let mut sign_ins = keychain(&cfg_a).load().unwrap();
    let client = OAuthClient { id: "client".into(), secret: Some("secret".into()) };
    sign_ins.last_clients.insert("google".into(), client.clone());
    keychain(&cfg_a).store(&sign_ins).unwrap();

    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());
    assert!(account_ids(&engine_b).is_empty());

    call(&engine_a, "sync.enable", json!({})).unwrap();
    let ticket = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
    call(&engine_b, "sync.join", json!({ "ticket": ticket })).unwrap();

    until("B to get A's account", || account_ids(&engine_b) == ["me@gmail.com"]);
    let received = keychain(&cfg_b).load().unwrap();
    assert_eq!(received.refresh_tokens["me@gmail.com"], "rt-me@gmail.com");
    assert_eq!(received.clients["me@gmail.com"], client);
    assert_eq!(received.last_clients["google"], client);
    let record = &call(&engine_b, "accounts.list", json!({})).unwrap()[0];
    assert_eq!((record["provider"].as_str(), record["services"][0].as_str()), (Some("google"), Some("mail")));

    // Neither device's group store, blobs or bridge files hold the token or the client secret.
    for cfg in [&cfg_a, &cfg_b] {
        let folder = cfg.support_directory.join("peer-sync");
        assert!(files_containing(&folder, b"rt-me@gmail.com").is_empty(), "{:?}", files_containing(&folder, b"rt-me@gmail.com"));
        assert!(files_containing(&folder, b"\"secret\"").is_empty());
        // The account record itself is in the group.
        assert!(!files_containing(&folder, b"me@gmail.com").is_empty());
    }

    // A rename follows; then a removal signs B out and deletes its saved token.
    call(&engine_a, "accounts.rename", json!({ "account": "me@gmail.com", "label": "Work" })).unwrap();
    until("B to take the new label", || {
        call(&engine_b, "accounts.list", json!({})).unwrap()[0]["label"] == json!("Work")
    });
    call(&engine_a, "accounts.remove", json!({ "account": "me@gmail.com", "yes": true })).unwrap();
    until("B to sign out of the removed account", || account_ids(&engine_b).is_empty());
    until("B to delete the token", || !keychain(&cfg_b).load().unwrap().refresh_tokens.contains_key("me@gmail.com"));

    drop(engine_b);
    drop(engine_a);
}

/// MS-396: `sync.remove` takes a device out of the status, withdraws the sign-ins sealed to it
/// and seals it no new ones, and the group's changes stop reaching it while the other devices
/// keep syncing.
#[test]
fn removing_a_device_withdraws_its_sign_ins_and_stops_its_sync() {
    let lookup = MemoryLookup::new();
    let (cfg_a, cfg_b, cfg_c) = (sync_config(&lookup), sync_config(&lookup), sync_config(&lookup));
    sign_in_all(&cfg_a, &[mail_account("me@gmail.com", "google")]);
    let engine_a = Engine::start(cfg_a.clone());
    let engine_b = Engine::start(cfg_b.clone());
    let engine_c = Engine::start(cfg_c.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    for engine in [&engine_b, &engine_c] {
        let ticket = call(&engine_a, "sync.pair_ticket", json!({})).unwrap()["ticket"].as_str().unwrap().to_string();
        call(engine, "sync.join", json!({ "ticket": ticket })).unwrap();
        until("the device to get A's account", || account_ids(engine) == ["me@gmail.com"]);
    }
    let id = |engine: &Engine| call(engine, "sync.status", json!({})).unwrap()["endpointId"].as_str().unwrap().to_string();
    let (id_b, id_c) = (id(&engine_b), id(&engine_c));
    let listed = |engine: &Engine| -> Vec<String> {
        let status = call(engine, "sync.status", json!({})).unwrap();
        status["devices"].as_array().unwrap().iter().map(|device| device["id"].as_str().unwrap().to_string()).collect()
    };
    until("A to list all three devices", || listed(&engine_a).len() == 3);

    let unknown = call(&engine_a, "sync.remove", json!({ "device": "nobody" })).unwrap_err();
    assert!(unknown.message.contains("No device"), "{}", unknown.message);
    let status = call(&engine_a, "sync.remove", json!({ "device": id_c })).unwrap();
    let devices: Vec<&str> = status["devices"].as_array().unwrap().iter().map(|device| device["id"].as_str().unwrap()).collect();
    assert!(devices.contains(&id_b.as_str()) && !devices.contains(&id_c.as_str()), "{devices:?}");
    until("B to stop listing C", || !listed(&engine_b).contains(&id_c));

    // A's next change reaches B, not C.
    call(&engine_a, "config.set", json!({ "key": "autoRead.days", "value": "9" })).unwrap();
    until("B to take A's change", || setting_value(&engine_b, "autoRead.days") == json!(9));
    call(&engine_c, "sync.now", json!({})).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_ne!(setting_value(&engine_c, "autoRead.days"), json!(9), "C got a change made after its removal");

    // In A's replica, the sign-in sealed to C is withdrawn; B's stays.
    drop(engine_c);
    drop(engine_b);
    drop(engine_a);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let sync = PeerSync::open_on(&cfg_a.support_directory, Network::Loopback(lookup.clone())).await.unwrap();
        let snapshot = sync.snapshot().await.unwrap();
        assert_eq!(snapshot.credentials[&format!("{id_c}/me@gmail.com")].sealed, None, "C's sign-in wasn't withdrawn");
        assert!(snapshot.credentials[&format!("{id_b}/me@gmail.com")].sealed.is_some());
        sync.shutdown().await;
    });
}
