//! Device sync (`peer_sync.rs`) between two support folders on the loopback interface: pairing,
//! edits made while both are offline converging once they reconnect, and one process per folder.
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use serde_json::json;

use crate::OAuthConfig;
use crate::peer_sync::{Network, PeerSync, PeerSyncError, Snapshot, Stamped};
use crate::sync_invite::Code;
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

    // A starts the group and B joins it with a pairing code of A's.
    a.create_group().await.unwrap();
    assert!(a.set_trusted("Alice@Example.com", true, 1_000).await.unwrap());
    assert!(a.set_setting("autoRead.days", json!(3), 1_000).await.unwrap());
    b.join(&a.invite().await.unwrap().0).await.unwrap();
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
/// reaches its devices reads nothing, before or after a live change; a pairing code still pairs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_knows_only_the_group_id_reads_nothing() {
    let (folder_a, folder_b, folder_c) =
        (config(OAuthConfig::default()), config(OAuthConfig::default()), config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let (a, b, c) = (open(&folder_a, &lookup).await, open(&folder_b, &lookup).await, open(&folder_c, &lookup).await);
    let namespace = a.create_group().await.unwrap();
    assert!(a.set_trusted("alice@example.com", true, 1_000).await.unwrap());
    assert!(a.set_setting("autoRead.days", json!(3), 1_000).await.unwrap());
    b.join(&a.invite().await.unwrap().0).await.unwrap();
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
    b.join_legacy(&a.legacy_ticket().await.unwrap()).await.unwrap();
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

/// MS-376 on MS-380's door: a pairing code admits one device, once; a used, forged or old-style
/// one admits nobody, and a device that holds the group's write key without a code reads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pairing_code_admits_one_device_once() {
    let folders = [(); 4].map(|()| config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let (a, b) = (open(&folders[0], &lookup).await, open(&folders[1], &lookup).await);
    let (c, d) = (open(&folders[2], &lookup).await, open(&folders[3], &lookup).await);
    let namespace = a.create_group().await.unwrap();
    assert!(a.set_trusted("alice@example.com", true, 1_000).await.unwrap());
    let (code, _) = a.invite().await.unwrap();
    assert_eq!(a.open_invites(), 1);
    b.join(&code).await.unwrap();
    assert_eq!(a.open_invites(), 0);
    let (id_b, id_c, id_d) = (b.status().await.endpoint_id, c.status().await.endpoint_id, d.status().await.endpoint_id);
    until("B to get A's entries", &b, |snapshot| trusted(snapshot, "alice@example.com").is_some()).await;
    until("A to admit B", &a, |snapshot| snapshot.admitted.contains_key(&id_b)).await;

    let used = c.join(&code).await.unwrap_err().to_string();
    assert!(used.contains("used already"), "{used}");
    let forged = Code::new(a.loopback_addr(), &namespace, &[7; 32]).to_string();
    assert!(c.join(&forged).await.is_err());
    let old = c.join(&a.legacy_ticket().await.unwrap()).await.unwrap_err().to_string();
    assert!(old.contains("old device sync ticket"), "{old}");
    // D holds the write key, as an old ticket gave it, but nobody admitted it.
    d.join_legacy(&a.legacy_ticket().await.unwrap()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(trusted(&d.snapshot().await.unwrap(), "alice@example.com"), None, "D read the group without a code");
    let admitted = a.snapshot().await.unwrap().admitted;
    assert!(!admitted.contains_key(&id_c) && !admitted.contains_key(&id_d));

    for sync in [a, b, c, d] {
        sync.shutdown().await;
    }
}

/// MS-396: removing a device shuts it out of every device that heard of the removal, its open
/// connections both ways included: it gets none of the group's new entries, its own reach
/// nobody, and the others keep syncing. Pairing it again brings it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_device_is_shut_out_until_paired_again() {
    let folders = [(); 3].map(|()| config(OAuthConfig::default()));
    let lookup = MemoryLookup::new();
    let (a, b, c) = (open(&folders[0], &lookup).await, open(&folders[1], &lookup).await, open(&folders[2], &lookup).await);
    a.create_group().await.unwrap();
    assert!(a.set_trusted("alice@example.com", true, 1_000).await.unwrap());
    b.join(&a.invite().await.unwrap().0).await.unwrap();
    c.join(&a.invite().await.unwrap().0).await.unwrap();
    let (id_a, id_b, id_c) = (a.status().await.endpoint_id, b.status().await.endpoint_id, c.status().await.endpoint_id);
    let all = |snapshot: &Snapshot| [&id_a, &id_b, &id_c].iter().all(|id| snapshot.admitted.contains_key(*id));
    for sync in [&a, &b, &c] {
        until("every device to admit all three", sync, all).await;
    }
    // Connected, live entries reach C.
    assert!(b.set_trusted("bob@example.com", true, 2_000).await.unwrap());
    until("C to get B's live change", &c, |snapshot| trusted(snapshot, "bob@example.com").is_some()).await;

    // Neither an unknown device nor this one can be removed.
    assert!(a.remove_device("not-a-device").await.is_err());
    assert!(a.remove_device(&id_a).await.is_err());

    a.remove_device(&id_c).await.unwrap();
    let removed = a.snapshot().await.unwrap();
    assert!(!removed.admitted.contains_key(&id_c));
    assert_eq!(removed.removed[&id_c].by, id_a);
    until("B to hear of the removal", &b, |snapshot| {
        snapshot.removed.contains_key(&id_c) && !snapshot.admitted.contains_key(&id_c)
    })
    .await;

    // A and B keep syncing; C, still connected to both a moment ago and now dialing past its own
    // gate, gets nothing new, and what it writes with the group's key it still holds reaches
    // neither.
    c.dial_anyone();
    assert!(a.set_trusted("carol@example.com", true, 3_000).await.unwrap());
    until("B to get A's change after the removal", &b, |snapshot| trusted(snapshot, "carol@example.com").is_some()).await;
    assert!(c.set_trusted("mallory@example.com", true, 3_100).await.unwrap());
    c.realign().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let on_c = c.snapshot().await.unwrap();
    assert_eq!(trusted(&on_c, "carol@example.com"), None, "C got an entry written after its removal");
    for sync in [&a, &b] {
        assert_eq!(trusted(&sync.snapshot().await.unwrap(), "mallory@example.com"), None, "C's entry reached the group");
    }

    // Paired again with a code of B's, C is admitted anew and catches up.
    c.join(&b.invite().await.unwrap().0).await.unwrap();
    until("C to catch up once paired again", &c, |snapshot| trusted(snapshot, "carol@example.com").is_some()).await;
    until("A to admit C again", &a, |snapshot| snapshot.admitted.contains_key(&id_c) && !snapshot.removed.contains_key(&id_c))
        .await;
    for sync in [a, b, c] {
        sync.shutdown().await;
    }
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

/// Closing sync releases the blob store's runtime and the endpoint's sockets (MS-378): pausing
/// and resuming device sync closes and reopens it, which before left another set of
/// `iroh-blob-store-*` threads and UDP sockets behind each time. Counts this process's threads
/// and sockets, so it reruns itself alone in a child process: the other tests open endpoints and
/// stores meanwhile.
#[cfg(unix)]
#[test]
fn closing_releases_threads_and_sockets() {
    const CHILD: &str = "MAILSTREAM_TEST_CLOSING_RELEASES";
    if std::env::var_os(CHILD).is_none() {
        let name = format!("{}::closing_releases_threads_and_sockets", module_path!().split_once("::").unwrap().1);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success() && stdout.contains("1 passed"), "child run failed:\n{stdout}\n{stderr}");
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(2).enable_all().build().unwrap();
    runtime.block_on(async {
        let (folder_a, folder_b) = (config(OAuthConfig::default()), config(OAuthConfig::default()));
        let lookup = MemoryLookup::new();
        // B stays open and paired, so each time A opens it rejoins the group and reaches B.
        let b = open(&folder_b, &lookup).await;
        let a = open(&folder_a, &lookup).await;
        a.create_group().await.unwrap();
        b.join(&a.invite().await.unwrap().0).await.unwrap();
        let id_b = b.status().await.endpoint_id;
        until("A to see B", &a, |snapshot| snapshot.devices.contains_key(&id_b)).await;
        a.shutdown().await;
        // What the process holds with A closed once: everything lazily set up is there now.
        let before = settled(None).await;
        for round in 0..4 {
            let a = open(&folder_a, &lookup).await;
            assert!(a.set_setting("round", json!(round), 10_000 + round).await.unwrap());
            until("B to get A's round", &b, |snapshot| snapshot.settings.get("round").map(|setting| &setting.value) == Some(&json!(round))).await;
            a.shutdown().await;
        }
        let after = settled(Some(before)).await;
        assert!(after.0 <= before.0 && after.1 <= before.1, "(threads, sockets) grew from {before:?} to {after:?} closing A four more times");
        b.shutdown().await;
    });
}

/// This process's (threads, sockets) once they stop falling, or once they are back at `target`:
/// a closed store's threads and an endpoint's sockets go shortly after it closes.
#[cfg(unix)]
async fn settled(target: Option<(usize, usize)>) -> (usize, usize) {
    let start = Instant::now();
    let mut last = (threads(), sockets());
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = (threads(), sockets());
        let done = match target {
            Some(target) => now.0 <= target.0 && now.1 <= target.1,
            None => now == last,
        };
        if done || start.elapsed() > Duration::from_secs(15) {
            return now;
        }
        last = now;
    }
}

#[cfg(target_os = "linux")]
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[cfg(target_os = "macos")]
fn threads() -> usize {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is a writable `proc_taskinfo` of `size` bytes.
    let written = unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size) };
    assert_eq!(written, size, "proc_pidinfo");
    info.pti_threadnum as usize
}

/// The sockets among this process's open files, of any kind.
#[cfg(unix)]
fn sockets() -> usize {
    let descriptors: Vec<i32> =
        std::fs::read_dir("/dev/fd").unwrap().filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok()).collect();
    descriptors
        .into_iter()
        .filter(|&fd| {
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `stat` is a writable `struct stat`; a descriptor closed meanwhile fails.
            unsafe { libc::fstat(fd, &mut stat) == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFSOCK }
        })
        .count()
}
