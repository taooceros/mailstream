//! Device sync (`peer_sync.rs`) between two support folders on the loopback interface: pairing,
//! edits made while both are offline converging once they reconnect, and one process per folder.
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use serde_json::json;

use crate::OAuthConfig;
use crate::peer_sync::{AdmitRequest, Network, PeerSync, PeerSyncError, Snapshot, Stamped};
use crate::testkit::{TestConfig, config};

async fn open(config: &TestConfig, lookup: &MemoryLookup) -> PeerSync {
    let sync = PeerSync::open_on(&config.support_directory, Network::Loopback(lookup.clone())).await.unwrap();
    lookup.add_endpoint_info(sync.loopback_addr());
    sync
}

/// Waits up to 20 s for `sync`'s snapshot to satisfy `done`, and returns it.
async fn until(what: &str, sync: &PeerSync, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let start = Instant::now();
    loop {
        let snapshot = sync.snapshot().await.unwrap();
        if done(&snapshot) {
            return snapshot;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}: {snapshot:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn trusted(snapshot: &Snapshot, address: &str) -> Option<Stamped<bool>> {
    snapshot.trusted.get(address).cloned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paired_devices_converge_after_editing_offline() {
    let (folder_a, folder_b) = (config(OAuthConfig::default()), config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let a = open(&folder_a, &lookup).await;
    let b = open(&folder_b, &lookup).await;
    assert_eq!(b.snapshot().await, Err(PeerSyncError::NoGroup));

    // A starts the group and B joins it with A's ticket.
    a.create_group().await.unwrap();
    assert!(a.set_trusted("Alice@Example.com", true, 1_000).await.unwrap());
    assert!(a.set_setting("autoRead.days", json!(3), 1_000).await.unwrap());
    b.join(&a.ticket().await.unwrap()).await.unwrap();
    let mut changes = b.changes();
    let (id_a, id_b) = (a.status().await.endpoint_id, b.status().await.endpoint_id);
    let paired = until("B to get A's entries", &b, |snapshot| {
        trusted(snapshot, "alice@example.com").is_some()
            && snapshot.settings.contains_key("autoRead.days")
            && snapshot.devices.contains_key(&id_a)
            && snapshot.devices.contains_key(&id_b)
    })
    .await;
    assert_eq!(trusted(&paired, "alice@example.com"), Some(Stamped { value: true, at: 1_000 }));
    assert_eq!(paired.devices[&id_a].platform, std::env::consts::OS);
    until("A to see B", &a, |snapshot| snapshot.devices.contains_key(&id_b)).await;
    assert_eq!(a.status().await.namespace, b.status().await.namespace);
    assert!(*changes.borrow_and_update() > 0, "B counted A's entries as changes");

    // Both go offline and edit.
    a.pause().await.unwrap();
    b.pause().await.unwrap();
    assert!(a.status().await.paused);
    // A untrusts Alice (a tombstone), trusts Bob and changes a setting.
    assert!(a.set_trusted("alice@example.com", false, 2_000).await.unwrap());
    assert!(a.set_trusted("bob@example.com", true, 2_100).await.unwrap());
    assert!(a.set_setting("autoRead.days", json!(5), 2_000).await.unwrap());
    // B, still holding A's old `true`, trusts Alice again before A untrusted her, sets the
    // same setting earlier than A, trusts Carol and sets another setting.
    assert!(b.set_trusted("alice@example.com", true, 1_500).await.unwrap());
    assert!(b.set_setting("autoRead.days", json!(7), 1_800).await.unwrap());
    assert!(b.set_trusted("carol@example.com", true, 2_050).await.unwrap());
    assert!(b.set_setting("aiTagging.enabled", json!(true), 2_200).await.unwrap());
    // Offline, nothing crossed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(trusted(&a.snapshot().await.unwrap(), "carol@example.com"), None);
    assert_eq!(trusted(&b.snapshot().await.unwrap(), "bob@example.com"), None);

    // Back online, both reach the same state: the newest value of each key, so A's tombstone
    // stays and B's older `true` doesn't bring Alice back.
    a.resume().await.unwrap();
    b.resume().await.unwrap();
    let converged = |snapshot: &Snapshot| {
        trusted(snapshot, "alice@example.com") == Some(Stamped { value: false, at: 2_000 })
            && trusted(snapshot, "bob@example.com") == Some(Stamped { value: true, at: 2_100 })
            && trusted(snapshot, "carol@example.com") == Some(Stamped { value: true, at: 2_050 })
            && snapshot.settings.get("autoRead.days") == Some(&Stamped { value: json!(5), at: 2_000 })
            && snapshot.settings.get("aiTagging.enabled") == Some(&Stamped { value: json!(true), at: 2_200 })
    };
    let on_a = until("A to converge", &a, converged).await;
    let on_b = until("B to converge", &b, converged).await;
    assert_eq!((on_a.trusted, on_a.settings), (on_b.trusted, on_b.settings));

    // Connected, a new entry reaches the other device by itself.
    assert!(a.set_trusted("erin@example.com", true, 3_000).await.unwrap());
    until("B to get A's live change", &b, |snapshot| trusted(snapshot, "erin@example.com").is_some()).await;

    // A write older than what the group holds is dropped.
    assert!(!b.set_trusted("ALICE@example.com", true, 1_999).await.unwrap());
    assert_eq!(trusted(&b.snapshot().await.unwrap(), "alice@example.com"), Some(Stamped { value: false, at: 2_000 }));

    a.shutdown().await;
    b.shutdown().await;
}

/// Waits `quiet` and checks `stranger` read none of the group's entries, not even their keys.
async fn reads_nothing(stranger: &PeerSync, quiet: Duration) {
    tokio::time::sleep(quiet).await;
    let seen = stranger.snapshot().await.unwrap();
    assert_eq!(seen, Snapshot::default(), "a device that knows only the group's id read the group");
}

/// MS-380: a device that knows only the group's id (settings and `sync status` show it) and
/// reaches its devices reads nothing, before or after a live change; a ticket still pairs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_knows_only_the_group_id_reads_nothing() {
    let (folder_a, folder_b, folder_c) =
        (config(OAuthConfig::default()), config(OAuthConfig::default()), config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let (a, b, c) = (open(&folder_a, &lookup).await, open(&folder_b, &lookup).await, open(&folder_c, &lookup).await);
    let namespace = a.create_group().await.unwrap();
    assert!(a.set_trusted("alice@example.com", true, 1_000).await.unwrap());
    assert!(a.set_setting("autoRead.days", json!(3), 1_000).await.unwrap());
    b.join(&a.ticket().await.unwrap()).await.unwrap();
    until("B to get A's entries", &b, |snapshot| trusted(snapshot, "alice@example.com").is_some()).await;
    let (id_a, id_b) = (a.status().await.endpoint_id, b.status().await.endpoint_id);
    let admitted = until("B to read both admissions", &b, |snapshot| {
        snapshot.admitted.contains_key(&id_a) && snapshot.admitted.contains_key(&id_b)
    })
    .await;
    assert_eq!(admitted.admitted[&id_a].by, id_a, "A admitted itself making the group");
    let admitted = until("A to admit B", &a, |snapshot| snapshot.admitted.contains_key(&id_b)).await;
    assert_eq!(admitted.admitted[&id_b].by, id_a, "A admitted B at its door");

    c.watch(&namespace, vec![a.loopback_addr(), b.loopback_addr()]).await.unwrap();
    reads_nothing(&c, Duration::from_secs(2)).await;
    assert!(a.set_trusted("bob@example.com", true, 2_000).await.unwrap());
    until("B to get A's live change", &b, |snapshot| trusted(snapshot, "bob@example.com").is_some()).await;
    c.realign().await;
    reads_nothing(&c, Duration::from_secs(2)).await;

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// MS-380: a group paired before admission keeps syncing once its devices run this version: each
/// admits the devices the group lists the first time it opens it, and a stranger reads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_from_before_admission_keeps_syncing_after_the_upgrade() {
    let (folder_a, folder_b, folder_c) =
        (config(OAuthConfig::default()), config(OAuthConfig::default()), config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let before = |folder: &TestConfig| {
        let (directory, lookup) = (folder.support_directory.clone(), lookup.clone());
        async move { PeerSync::open_on(&directory, Network::BeforeAdmission(lookup)).await.unwrap() }
    };
    let (a, b) = (before(&folder_a).await, before(&folder_b).await);
    let namespace = a.create_group().await.unwrap();
    assert!(a.set_trusted("alice@example.com", true, 1_000).await.unwrap());
    b.join(&a.ticket().await.unwrap()).await.unwrap();
    let (id_a, id_b) = (a.status().await.endpoint_id, b.status().await.endpoint_id);
    until("B to get A's entries", &b, |snapshot| trusted(snapshot, "alice@example.com").is_some()).await;
    let paired = until("A to see B", &a, |snapshot| snapshot.devices.contains_key(&id_b)).await;
    assert!(paired.admitted.is_empty(), "devices from before MS-380 admit no one");
    a.shutdown().await;
    b.shutdown().await;

    let (a, b) = (open(&folder_a, &lookup).await, open(&folder_b, &lookup).await);
    let both = |snapshot: &Snapshot| snapshot.admitted.contains_key(&id_a) && snapshot.admitted.contains_key(&id_b);
    until("A to admit both", &a, both).await;
    until("B to admit both", &b, both).await;
    assert!(a.set_trusted("bob@example.com", true, 2_000).await.unwrap());
    until("B to get A's change after the upgrade", &b, |snapshot| trusted(snapshot, "bob@example.com").is_some()).await;
    assert!(b.set_trusted("carol@example.com", true, 2_100).await.unwrap());
    until("A to get B's change after the upgrade", &a, |snapshot| trusted(snapshot, "carol@example.com").is_some()).await;

    let c = open(&folder_c, &lookup).await;
    c.watch(&namespace, vec![a.loopback_addr(), b.loopback_addr()]).await.unwrap();
    reads_nothing(&c, Duration::from_secs(2)).await;
    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// MS-380: the door admits a device only for its own proof of the group's write key: not a
/// forged one, and not another device's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_door_admits_only_a_device_that_holds_the_write_key() {
    let (folder_a, folder_b, folder_c) =
        (config(OAuthConfig::default()), config(OAuthConfig::default()), config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let (a, b, c) = (open(&folder_a, &lookup).await, open(&folder_b, &lookup).await, open(&folder_c, &lookup).await);
    let namespace = a.create_group().await.unwrap();
    b.join(&a.ticket().await.unwrap()).await.unwrap();
    let (member, id_b, id_c) = (a.loopback_addr(), b.status().await.endpoint_id, c.status().await.endpoint_id);
    until("A to admit B", &a, |snapshot| snapshot.admitted.contains_key(&id_b)).await;

    let forged = AdmitRequest::Proof { namespace: namespace.clone(), signature: "00".repeat(64) };
    assert!(c.ask_admission(member.clone(), &forged).await.is_err());
    let borrowed = b.admission_proof(member.id).await.unwrap();
    assert!(c.ask_admission(member.clone(), &borrowed).await.is_err(), "B's proof admits B only");
    assert_eq!(b.ask_admission(member.clone(), &borrowed).await, Ok(()));
    assert!(!a.snapshot().await.unwrap().admitted.contains_key(&id_c));
    c.watch(&namespace, vec![member]).await.unwrap();
    reads_nothing(&c, Duration::from_secs(2)).await;

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_process_holds_a_folder_and_reopens_it_as_it_was() {
    let folder = config(OAuthConfig::default());
    let lookup = MemoryLookup::new();
    let sync = open(&folder, &lookup).await;
    let namespace = sync.create_group().await.unwrap();
    assert!(sync.set_trusted("dave@example.com", true, 3_000).await.unwrap());
    sync.pause().await.unwrap();
    let before = sync.status().await;

    // A second open of the same folder (another app, the CLI) is refused while the first holds it.
    let second = PeerSync::open_on(&folder.support_directory, Network::Loopback(lookup.clone())).await;
    assert_eq!(second.err(), Some(PeerSyncError::Busy));

    // Once closed, the folder reopens as the same device in the same group, still paused.
    sync.shutdown().await;
    let sync = open(&folder, &lookup).await;
    let after = sync.status().await;
    assert_eq!(after.endpoint_id, before.endpoint_id);
    assert_eq!(after.namespace, Some(namespace.clone()));
    assert!(after.paused);
    let snapshot = sync.snapshot().await.unwrap();
    assert_eq!(trusted(&snapshot, "dave@example.com"), Some(Stamped { value: true, at: 3_000 }));
    assert!(snapshot.devices.contains_key(&after.endpoint_id));

    // A new group carries over what the old one held.
    let renewed = sync.create_group().await.unwrap();
    assert_ne!(renewed, namespace);
    assert_eq!(trusted(&sync.snapshot().await.unwrap(), "dave@example.com"), Some(Stamped { value: true, at: 3_000 }));
    sync.shutdown().await;
}
