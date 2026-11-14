//! End-to-end tests of `device_sync` bridging `PeerSync` and `settings.json` across full
//! `Engine` instances on the loopback interface (MS-360).
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use serde_json::{Value, json};

use crate::mail_tests::{GmailCounters, gmail_server};
use crate::peer_sync::Network;
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
    until("first engine opens device sync", || {
        call(&first, "sync.status", json!({})).unwrap()["available"] == json!(true)
    });

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
