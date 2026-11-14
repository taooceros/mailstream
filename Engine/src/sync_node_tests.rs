//! End-to-end tests of [`SyncNode`] (`mailstream sync daemon`, MS-362): offline A->S->B relay
//! through an always-on headless node, control socket scoping, lock exclusivity, and persistence.
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use serde_json::{Value, json};

use crate::peer_sync::Network;
use crate::sync_node::{SyncNode, SyncNodeConfig};
use crate::testkit::{TestConfig, call, config};
use crate::{Engine, OAuthConfig, control};

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

#[cfg(unix)]
fn ask_socket(path: &Path, request: &str) -> Value {
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn offline_device_syncs_through_headless_sync_node() {
    let lookup = MemoryLookup::new();
    let cfg_a = sync_config(&lookup);
    let cfg_b = sync_config(&lookup);
    let cfg_s = sync_config(&lookup);
    let dir_s = cfg_s.support_directory.clone();

    let engine_a = Engine::start(cfg_a.clone());
    call(&engine_a, "sync.enable", json!({})).unwrap();
    let ticket_a = call(&engine_a, "sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();

    let node_s = SyncNode::open(SyncNodeConfig {
        data_directory: dir_s.clone(),
        network: Network::Loopback(lookup.clone()),
        control_socket: true,
    })
    .unwrap();
    let joined_s = node_s.join_and_wait(&ticket_a, Duration::from_secs(15)).unwrap();
    assert_eq!(joined_s["enabled"], json!(true));

    // Wait for S's control socket, generate a pairing ticket from S, and join B with it.
    let sock_s = control::socket_path(&dir_s);
    #[cfg(unix)]
    until("S control socket", || std::os::unix::net::UnixStream::connect(&sock_s).is_ok());

    #[cfg(unix)]
    let ticket_s = {
        let reply = ask_socket(&sock_s, "{\"command\":\"sync.pair_ticket\",\"args\":{}}\n");
        assert_eq!(reply["ok"], json!(true), "{reply}");
        reply["result"]["ticket"].as_str().unwrap().to_string()
    };
    #[cfg(not(unix))]
    let ticket_s = node_s.call_sync("sync.pair_ticket", json!({}))
        .unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();

    let engine_b = Engine::start(cfg_b.clone());
    call(&engine_b, "sync.join", json!({ "ticket": ticket_s })).unwrap();

    until("all 3 nodes to see 3 devices", || {
        let a_n = call(&engine_a, "sync.status", json!({})).unwrap()["devices"].as_array().unwrap().len();
        let s_n = node_s.call_sync("sync.status", json!({})).unwrap()["devices"].as_array().unwrap().len();
        let b_n = call(&engine_b, "sync.status", json!({})).unwrap()["devices"].as_array().unwrap().len();
        a_n == 3 && s_n == 3 && b_n == 3
    });

    // B goes offline before A writes.
    drop(engine_b);

    call(&engine_a, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["alice@x.com"] })).unwrap();
    call(&engine_a, "config.set", json!({ "key": "autoRead.enabled", "value": "true" })).unwrap();

    // Headless S receives A's changes into its local settings.json.
    until("headless S to receive A's settings", || {
        node_s.call_sync("config.get", json!({ "key": "remoteContent.trustedSenders" })).unwrap()["value"]
            == json!(["alice@x.com"])
            && node_s.call_sync("config.get", json!({ "key": "autoRead.enabled" })).unwrap()["value"] == json!(true)
    });

    // A goes offline BEFORE B comes back online: A and B never overlap after A's edit.
    drop(engine_a);

    let engine_b2 = Engine::start(cfg_b.clone());
    until("B to receive A's settings via always-on S", || {
        call(&engine_b2, "config.get", json!({ "key": "remoteContent.trustedSenders" })).unwrap()["value"]
            == json!(["alice@x.com"])
            && call(&engine_b2, "config.get", json!({ "key": "autoRead.enabled" })).unwrap()["value"] == json!(true)
    });

    // S serves sync.* / config.* over control.sock and refuses mail.* without launching GUI,
    // and never creates accounts.json.
    #[cfg(unix)]
    {
        let mail_reply = ask_socket(&sock_s, "{\"command\":\"mail.list\",\"args\":{}}\n");
        assert_eq!(mail_reply["ok"], json!(false));
        assert_eq!(mail_reply["error"]["code"], json!("unavailable"));
    }
    assert!(!dir_s.join("accounts.json").exists());

    drop(engine_b2);
    drop(node_s);
}

#[test]
fn sync_node_rejects_second_holder_and_persists_across_restart() {
    let lookup = MemoryLookup::new();
    let cfg = sync_config(&lookup);
    let dir = cfg.support_directory.clone();

    let node1 = SyncNode::open(SyncNodeConfig {
        data_directory: dir.clone(),
        network: Network::Loopback(lookup.clone()),
        control_socket: false,
    })
    .unwrap();
    let enabled = node1.call_sync("sync.enable", json!({})).unwrap();
    let endpoint_id = enabled["endpointId"].as_str().unwrap().to_string();
    let namespace = enabled["namespace"].as_str().unwrap().to_string();

    let busy = match SyncNode::open(SyncNodeConfig {
        data_directory: dir.clone(),
        network: Network::Loopback(lookup.clone()),
        control_socket: false,
    }) {
        Ok(_) => panic!("second SyncNode on same directory must fail with busy"),
        Err(error) => error,
    };
    assert_eq!(busy.code, "busy");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let peer_dir = dir.join("peer-sync");
        assert_eq!(std::fs::metadata(&peer_dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(peer_dir.join("endpoint.key")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    drop(node1);

    let node2 = SyncNode::open(SyncNodeConfig {
        data_directory: dir,
        network: Network::Loopback(lookup),
        control_socket: false,
    })
    .unwrap();
    let status2 = node2.call_sync("sync.status", json!({})).unwrap();
    assert_eq!(status2["endpointId"], json!(endpoint_id));
    assert_eq!(status2["namespace"], json!(namespace));
    assert_eq!(status2["enabled"], json!(true));
    drop(node2);
}
