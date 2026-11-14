//! Device sync between one user's machines (MS-348, MS-359): an iroh-docs document, the sync
//! group, of which every paired device holds a full replica. Devices reach each other over iroh
//! (QUIC, direct or through a relay, found through number 0's DNS address lookup), reconcile their
//! replicas whenever they meet and pass new entries on live through iroh-gossip; the values ride
//! in iroh-blobs. Nothing is stored on a server, so a change reaches another device once both
//! have been online at the same time (directly or through a third device of the group).
//!
//! Everything lives in `peer-sync/` in the engine's support folder:
//! - `lock`: the one process with sync open holds it (redb opens its stores for one process
//!   only); another gets [`PeerSyncError::Busy`].
//! - `endpoint.key`: this device's iroh secret key; its public half, the `EndpointId`, is the
//!   device's address and name in the group.
//! - `docs/`: iroh-docs' store (`docs.redb`) and this device's author key (`default-author`),
//!   which signs what it writes.
//! - `blobs/`: iroh-blobs' store with the entries' values.
//! - `sync.json`: `{"namespace": …, "relayUrl": …, "paused": …}`: the group this device is in,
//!   a relay to use instead of number 0's public ones, and whether sync is paused.
//!
//! The group's entries are one key per item, each value JSON with its own time (Unix ms):
//! - `v1/trusted/<lowercased address>`: `{"trusted": true|false, "at": …}`; untrusting writes
//!   `trusted: false` (a tombstone), never deletes.
//! - `v1/setting/<key>`: `{"value": <json>, "at": …}`.
//! - `v1/meta/device/<endpoint id>`: `{"name": …, "platform": …, "lastSeen": …}`.
//!
//! Each device writes with its own author, so a key holds one entry per device that wrote it.
//! Readers take the entry with the latest `at` (`lastSeen` for devices), on a tie the one of the
//! larger author key, so every replica that holds the same entries reads the same state, and an
//! old `trusted: true` from a device that was offline never outvotes a newer tombstone.
//!
//! A device reconciles with every device the group lists under `v1/meta/device/`, not only the
//! ones iroh-docs remembers: iroh-docs 0.101 remembers a peer only after a reconciliation it
//! counts as successful, and it counts one as failed when closing the connection afterwards
//! fails, which often happens although every entry crossed.
//!
//! The engine bridges this to settings.json (MS-360); this module knows nothing of settings.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::str::FromStr;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt};
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use iroh_blobs::api::Store as BlobStore;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::store::GcConfig;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::fs::options::Options as BlobOptions;
use iroh_docs::api::Doc;
use iroh_docs::api::protocol::{AddrInfoOptions, ShareMode};
use iroh_docs::engine::{LiveEvent, ProtectCallbackHandler};
use iroh_docs::protocol::Docs;
use iroh_docs::store::Query;
use iroh_docs::{AuthorId, ContentStatus, DocTicket, NamespaceId};
use iroh_gossip::net::Gossip;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::files;
use crate::signal::{ProcessSignal, next_wake};

/// The folder in the support folder.
const FOLDER: &str = "peer-sync";
/// Every key the group holds starts with this.
const PREFIX: &str = "v1/";
const TRUSTED: &str = "v1/trusted/";
const SETTING: &str = "v1/setting/";
const DEVICE: &str = "v1/meta/device/";
/// How often unreferenced values are deleted from `blobs/`.
const GC_INTERVAL: Duration = Duration::from_secs(3600);
/// How often to look whether the machine slept.
const WAKE_CHECK: Duration = Duration::from_secs(10);
/// How long a ticket waits for the relay, whose address it then carries.
const ONLINE_WAIT: Duration = Duration::from_secs(5);

/// Why sync can't do what was asked.
#[derive(Clone, Debug, PartialEq)]
pub enum PeerSyncError {
    /// Another process on this machine has sync open on the same folder.
    Busy,
    /// This device is in no sync group yet.
    NoGroup,
    /// The disk, iroh or a ticket refused.
    Failed(String),
}

impl fmt::Display for PeerSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("Another Mailstream process on this machine has device sync open."),
            Self::NoGroup => formatter.write_str("This device is in no sync group yet."),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

fn failed(error: impl fmt::Display) -> PeerSyncError {
    PeerSyncError::Failed(format!("{error:#}"))
}

type Result<T, E = PeerSyncError> = std::result::Result<T, E>;

/// A value with the time it was set, Unix ms.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamped<T> {
    pub value: T,
    pub at: u64,
}

/// A device of the group, as it last described itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub name: String,
    /// `macos`, `linux`, `windows`.
    pub platform: String,
    /// Unix ms.
    pub last_seen: u64,
}

/// The group's state as this replica reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// By lowercased address; `false` is a tombstone.
    pub trusted: BTreeMap<String, Stamped<bool>>,
    pub settings: BTreeMap<String, Stamped<Value>>,
    /// By endpoint id.
    pub devices: BTreeMap<String, Device>,
    /// Keys left out above because a value is still downloading: what they hold is unknown,
    /// not absent.
    pub pending: BTreeSet<String>,
}

/// What sync is doing.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// This device's endpoint id.
    pub endpoint_id: String,
    /// The group's id; `None` before creating or joining one.
    pub namespace: Option<String>,
    pub paused: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrustedValue {
    trusted: bool,
    at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingValue {
    value: Value,
    at: u64,
}

/// `sync.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_url: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    paused: bool,
}

impl SyncFile {
    fn read(directory: &Path) -> Result<Self> {
        match std::fs::read(directory.join("sync.json")) {
            Ok(data) => serde_json::from_slice(&data).map_err(|error| failed(format!("peer-sync/sync.json: {error}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(failed(error)),
        }
    }

    fn write(&self, directory: &Path) -> Result<()> {
        let data = serde_json::to_vec_pretty(self).map_err(failed)?;
        files::replace(directory, &directory.join("sync.json"), &data).map_err(failed)
    }
}

/// How the endpoint reaches other devices.
pub(crate) enum Network {
    /// Number 0's address lookup and relays, or the relay `sync.json` names.
    Public,
    /// 127.0.0.1 only, devices found through a lookup the test shares: no relay or DNS.
    #[cfg(test)]
    Loopback(iroh::address_lookup::MemoryLookup),
}

/// Sync open on one support folder. Call [`PeerSync::shutdown`] before opening it again.
pub struct PeerSync {
    inner: Arc<Inner>,
    /// Realigning after network changes and wakes.
    watchers: Vec<JoinHandle<()>>,
}

struct Inner {
    directory: PathBuf,
    /// Held until dropped.
    _lock: File,
    router: Router,
    blobs: FsStore,
    docs: Docs,
    author: AuthorId,
    group: tokio::sync::Mutex<Group>,
    /// Counts remote changes that became readable.
    changes: watch::Sender<u64>,
}

/// The group this device is in, if any.
struct Group {
    file: SyncFile,
    doc: Option<Doc>,
    /// Follows the doc's events.
    events: Option<JoinHandle<()>>,
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(events) = self.events.take() {
            events.abort();
        }
    }
}

impl PeerSync {
    /// Opens sync in `support_directory/peer-sync`, creating this device's keys the first time,
    /// and rejoins the group it is in unless paused. Call on a tokio runtime.
    pub async fn open(support_directory: &Path) -> Result<Self> {
        Self::open_on(support_directory, Network::Public).await
    }

    pub(crate) async fn open_on(support_directory: &Path, network: Network) -> Result<Self> {
        let directory = support_directory.join(FOLDER);
        std::fs::create_dir_all(directory.join("docs")).map_err(failed)?;
        std::fs::create_dir_all(directory.join("blobs")).map_err(failed)?;
        let lock = File::options().create(true).truncate(false).write(true).open(directory.join("lock")).map_err(failed)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(PeerSyncError::Busy),
            Err(TryLockError::Error(error)) => return Err(failed(error)),
        }
        let file = SyncFile::read(&directory)?;
        let endpoint = bind(secret_key(&directory)?, network, file.relay_url.as_deref()).await?;

        let (protect, protected) = ProtectCallbackHandler::new();
        let mut options = BlobOptions::new(&directory.join("blobs"));
        options.gc = Some(GcConfig { interval: GC_INTERVAL, add_protected: Some(protected) });
        let blobs = match FsStore::load_with_opts(directory.join("blobs").join("blobs.db"), options).await {
            Ok(blobs) => blobs,
            Err(error) => {
                endpoint.close().await;
                return Err(failed(error));
            }
        };
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs = match Docs::persistent(directory.join("docs"))
            .protect_handler(protect)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await
        {
            Ok(docs) => docs,
            Err(error) => {
                blobs.shutdown().await.ok();
                endpoint.close().await;
                return Err(failed(error));
            }
        };
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, iroh_blobs::BlobsProtocol::new(&blobs, None))
            .accept(iroh_gossip::ALPN, gossip)
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();
        let author = match docs.author_default().await {
            Ok(author) => author,
            Err(error) => {
                router.shutdown().await.ok();
                blobs.shutdown().await.ok();
                return Err(failed(error));
            }
        };

        let inner = Arc::new(Inner {
            directory,
            _lock: lock,
            router,
            blobs,
            docs,
            author,
            group: tokio::sync::Mutex::new(Group { file, doc: None, events: None }),
            changes: watch::channel(0).0,
        });
        let sync = Self { watchers: watch_network(&inner), inner };
        let reopened = {
            let mut group = sync.inner.group.lock().await;
            match group.file.namespace.as_deref().map(NamespaceId::from_str) {
                Some(Ok(namespace)) => sync.inner.reopen(&mut group, namespace).await,
                Some(Err(error)) => Err(failed(format!("peer-sync/sync.json: {error}"))),
                None => Ok(()),
            }
        };
        if let Err(error) = reopened {
            sync.shutdown().await;
            return Err(error);
        }
        Ok(sync)
    }

    /// Closes the stores and the endpoint and releases the folder.
    pub async fn shutdown(self) {
        for watcher in &self.watchers {
            watcher.abort();
        }
        let inner = &self.inner;
        if let Some(events) = inner.group.lock().await.events.take() {
            events.abort();
        }
        inner.router.shutdown().await.ok();
        inner.blobs.shutdown().await.ok();
    }

    pub async fn status(&self) -> Status {
        let group = self.inner.group.lock().await;
        Status {
            endpoint_id: self.inner.endpoint().id().to_string(),
            namespace: group.doc.as_ref().map(|doc| doc.id().to_string()),
            paused: group.file.paused,
        }
    }

    /// Counts the remote changes that became readable; read [`PeerSync::snapshot`] when it
    /// moves.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Starts a new group with this device in it, carrying over what the current group holds
    /// (trusted senders and settings, at their times), and leaves the current one: devices that
    /// were in it are no longer reached (pair them again). Returns the group's id.
    pub async fn create_group(&self) -> Result<String> {
        let inner = &self.inner;
        let mut group = inner.group.lock().await;
        let carried = match &group.doc {
            Some(doc) => Some(inner.read(doc).await?),
            None => None,
        };
        let doc = inner.docs.create().await.map_err(failed)?;
        let namespace = doc.id();
        if let Some(carried) = carried {
            for (address, trusted) in carried.trusted {
                let value = TrustedValue { trusted: trusted.value, at: trusted.at };
                inner.put(&doc, format!("{TRUSTED}{address}"), &value).await?;
            }
            for (key, setting) in carried.settings {
                let value = SettingValue { value: setting.value, at: setting.at };
                inner.put(&doc, format!("{SETTING}{key}"), &value).await?;
            }
        }
        let events = doc.subscribe().await.map_err(failed)?;
        inner.replace(&mut group, doc, events).await?;
        if !group.file.paused {
            let doc = group.doc.as_ref().expect("just set");
            doc.start_sync(Vec::new()).await.map_err(failed)?;
        }
        Ok(namespace.to_string())
    }

    /// A ticket another device joins the group with: the group's write key and this device's
    /// addresses. Whoever has it can read and write the group until it is replaced
    /// ([`PeerSync::create_group`]).
    pub async fn ticket(&self) -> Result<String> {
        let group = self.inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let endpoint = self.inner.endpoint();
        if !endpoint.addr().relay_urls().any(|_| true) {
            tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await.ok();
        }
        let ticket = doc.share(ShareMode::Write, AddrInfoOptions::RelayAndAddresses).await.map_err(failed)?;
        Ok(ticket.to_string())
    }

    /// Joins the group of `ticket` (see [`PeerSync::ticket`]) and syncs with the devices it
    /// names, resuming sync if paused. The group this device was in, if another, is deleted
    /// from it.
    pub async fn join(&self, ticket: &str) -> Result<()> {
        let ticket = DocTicket::from_str(ticket.trim()).map_err(|error| failed(format!("Not a device sync ticket: {error}")))?;
        let inner = &self.inner;
        let mut group = inner.group.lock().await;
        if group.file.paused {
            group.file.paused = false;
            group.file.write(&inner.directory)?;
        }
        if let Some(doc) = group.doc.as_ref().filter(|doc| doc.id() == ticket.capability.id()) {
            return doc.start_sync(ticket.nodes).await.map_err(failed);
        }
        let (doc, events) = inner.docs.import_and_subscribe(ticket).await.map_err(failed)?;
        inner.replace(&mut group, doc, events).await
    }

    /// Stops syncing until [`PeerSync::resume`], also after a restart. Writes still go to this
    /// replica and reach the group on resuming.
    pub async fn pause(&self) -> Result<()> {
        let mut group = self.inner.group.lock().await;
        if let Some(doc) = &group.doc {
            doc.leave().await.map_err(failed)?;
        }
        group.file.paused = true;
        group.file.write(&self.inner.directory)
    }

    pub async fn resume(&self) -> Result<()> {
        let mut group = self.inner.group.lock().await;
        group.file.paused = false;
        group.file.write(&self.inner.directory)?;
        drop(group);
        self.inner.realign().await;
        Ok(())
    }

    /// Tells iroh the network changed and reconciles with every known device now. Sync does this
    /// itself after a network change or a wake.
    pub async fn realign(&self) {
        self.inner.realign().await;
    }

    /// The group's state.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let group = self.inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        self.inner.read(doc).await
    }

    /// Records `address` (any case) as trusted or not at `at` (Unix ms); `false` when the group
    /// already holds a value from `at` or later, which stays.
    pub async fn set_trusted(&self, address: &str, trusted: bool, at: u64) -> Result<bool> {
        let key = format!("{TRUSTED}{}", address.trim().to_lowercase());
        self.set(key, &TrustedValue { trusted, at }, at).await
    }

    /// Records setting `key` as `value` at `at` (Unix ms); `false` when the group already holds
    /// a value from `at` or later, which stays.
    pub async fn set_setting(&self, key: &str, value: Value, at: u64) -> Result<bool> {
        self.set(format!("{SETTING}{key}"), &SettingValue { value, at }, at).await
    }

    async fn set(&self, key: String, value: &impl Serialize, at: u64) -> Result<bool> {
        let inner = &self.inner;
        let group = inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let current = inner.collect(doc, Query::key_exact(&key).build()).await?;
        if current.iter().any(|found| found.record.as_ref().is_some_and(|record| record.at() >= at)) {
            return Ok(false);
        }
        inner.put(doc, key, value).await?;
        Ok(true)
    }

    /// Where the loopback endpoint listens, for the tests' shared lookup.
    #[cfg(test)]
    pub(crate) fn loopback_addr(&self) -> EndpointAddr {
        let endpoint = self.inner.endpoint();
        endpoint.bound_sockets().into_iter().fold(EndpointAddr::new(endpoint.id()), EndpointAddr::with_ip_addr)
    }
}

impl Drop for PeerSync {
    fn drop(&mut self) {
        for watcher in &self.watchers {
            watcher.abort();
        }
    }
}

impl Inner {
    fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    fn blob_store(&self) -> &BlobStore {
        &self.blobs
    }

    /// Opens the group `sync.json` names, rejoining it unless paused.
    async fn reopen(&self, group: &mut Group, namespace: NamespaceId) -> Result<()> {
        let Some(doc) = self.docs.open(namespace).await.map_err(failed)? else {
            return Ok(());
        };
        let events = doc.subscribe().await.map_err(failed)?;
        group.events = Some(follow(events, self.changes.clone()));
        self.describe(&doc).await?;
        if !group.file.paused {
            let peers = self.peers(&doc).await;
            doc.start_sync(peers).await.map_err(failed)?;
        }
        group.doc = Some(doc);
        Ok(())
    }

    /// Makes `doc` the group, deleting the previous one, and records it in `sync.json`.
    async fn replace(
        &self,
        group: &mut Group,
        doc: Doc,
        events: impl Stream<Item = std::result::Result<LiveEvent, impl Send + 'static>> + Send + 'static,
    ) -> Result<()> {
        if let Some(events) = group.events.take() {
            events.abort();
        }
        if let Some(previous) = group.doc.take() {
            previous.leave().await.ok();
            self.docs.drop_doc(previous.id()).await.map_err(failed)?;
        }
        group.events = Some(follow(events, self.changes.clone()));
        self.describe(&doc).await?;
        group.file.namespace = Some(doc.id().to_string());
        group.doc = Some(doc);
        group.file.write(&self.directory)
    }

    /// Records this device's name, platform and the time in the group.
    async fn describe(&self, doc: &Doc) -> Result<()> {
        let device = Device { name: device_name(), platform: std::env::consts::OS.to_string(), last_seen: now_ms() };
        self.put(doc, format!("{DEVICE}{}", self.endpoint().id()), &device).await
    }

    async fn put(&self, doc: &Doc, key: String, value: &impl Serialize) -> Result<()> {
        let data = serde_json::to_vec(value).map_err(failed)?;
        doc.set_bytes(self.author, key, data).await.map_err(failed)?;
        Ok(())
    }

    async fn realign(&self) {
        self.endpoint().network_change().await;
        let group = self.group.lock().await;
        let Some(doc) = group.doc.as_ref().filter(|_| !group.file.paused) else { return };
        // Failures wait for the next change or wake.
        let peers = self.peers(doc).await;
        if doc.start_sync(peers).await.is_ok() {
            self.describe(doc).await.ok();
        }
    }

    /// The other devices of the group, by the descriptions they wrote; iroh-docs adds the ones
    /// it synced with itself.
    async fn peers(&self, doc: &Doc) -> Vec<EndpointAddr> {
        let me = self.endpoint().id();
        let mut peers = BTreeSet::new();
        if let Ok(entries) = doc.get_many(Query::key_prefix(DEVICE).build()).await {
            let mut entries = pin!(entries);
            while let Some(Ok(entry)) = entries.next().await {
                let id = std::str::from_utf8(entry.key()).ok().and_then(|key| key.strip_prefix(DEVICE));
                if let Some(id) = id.and_then(|id| EndpointId::from_str(id).ok()).filter(|id| *id != me) {
                    peers.insert(id);
                }
            }
        }
        peers.into_iter().map(EndpointAddr::new).collect()
    }

    /// The entries of `query`. Values that aren't the JSON their key calls for are left out.
    async fn collect(&self, doc: &Doc, query: impl Into<Query>) -> Result<Vec<Found>> {
        let blobs = self.blob_store().blobs();
        let mut entries = pin!(doc.get_many(query).await.map_err(failed)?);
        let mut found = Vec::new();
        while let Some(entry) = entries.next().await {
            let entry = entry.map_err(failed)?;
            let Ok(key) = std::str::from_utf8(entry.key()) else { continue };
            let hash = entry.content_hash();
            let data = match blobs.status(hash).await {
                Ok(BlobStatus::Complete { .. }) => blobs.get_bytes(hash).await.ok(),
                _ => None,
            };
            let record = match data {
                Some(data) => match Record::parse(key, &data) {
                    Some(record) => Some(record),
                    None => continue,
                },
                None => None,
            };
            found.push(Found { key: key.to_string(), author: *entry.author().as_bytes(), record });
        }
        Ok(found)
    }

    async fn read(&self, doc: &Doc) -> Result<Snapshot> {
        let mut snapshot = Snapshot::default();
        let mut newest: BTreeMap<String, ((u64, [u8; 32]), Record)> = BTreeMap::new();
        for Found { key, author, record } in self.collect(doc, Query::key_prefix(PREFIX).build()).await? {
            let Some(record) = record else {
                snapshot.pending.insert(key);
                continue;
            };
            let rank = (record.at(), author);
            if newest.get(&key).is_none_or(|(kept, _)| rank > *kept) {
                newest.insert(key, (rank, record));
            }
        }
        for (key, (_, record)) in newest {
            if snapshot.pending.contains(&key) {
                continue;
            }
            // `Record::parse` checked the prefix.
            match record {
                Record::Trusted(value) => {
                    let stamped = Stamped { value: value.trusted, at: value.at };
                    snapshot.trusted.insert(key[TRUSTED.len()..].to_string(), stamped);
                }
                Record::Setting(value) => {
                    let stamped = Stamped { value: value.value, at: value.at };
                    snapshot.settings.insert(key[SETTING.len()..].to_string(), stamped);
                }
                Record::Device(device) => {
                    snapshot.devices.insert(key[DEVICE.len()..].to_string(), device);
                }
            }
        }
        Ok(snapshot)
    }
}

/// One device's entry for a key: its author's bytes and, once downloaded, its value.
struct Found {
    key: String,
    author: [u8; 32],
    record: Option<Record>,
}

/// An entry's value, as its key calls for.
enum Record {
    Trusted(TrustedValue),
    Setting(SettingValue),
    Device(Device),
}

impl Record {
    /// `None` for keys outside the scheme and values that aren't the JSON their key calls for.
    fn parse(key: &str, data: &[u8]) -> Option<Self> {
        if key.starts_with(TRUSTED) {
            serde_json::from_slice(data).ok().map(Self::Trusted)
        } else if key.starts_with(SETTING) {
            serde_json::from_slice(data).ok().map(Self::Setting)
        } else if key.starts_with(DEVICE) {
            serde_json::from_slice(data).ok().map(Self::Device)
        } else {
            None
        }
    }

    /// When it was written, Unix ms.
    fn at(&self) -> u64 {
        match self {
            Self::Trusted(value) => value.at,
            Self::Setting(value) => value.at,
            Self::Device(device) => device.last_seen,
        }
    }
}

/// Counts the group's remote changes once readable.
fn follow<E: Send + 'static>(
    events: impl Stream<Item = std::result::Result<LiveEvent, E>> + Send + 'static,
    changes: watch::Sender<u64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = pin!(events);
        while let Some(event) = events.next().await {
            if let Ok(LiveEvent::InsertRemote { content_status: ContentStatus::Complete, .. } | LiveEvent::ContentReady { .. }) =
                event
            {
                changes.send_modify(|count| *count += 1);
            }
        }
    })
}

/// Realigns after each network change and wake while `inner` lives.
fn watch_network(inner: &Arc<Inner>) -> Vec<JoinHandle<()>> {
    let weak: Weak<Inner> = Arc::downgrade(inner);
    let changes = tokio::spawn(async move {
        let Some(mut posts) = ProcessSignal::NETWORK_CHANGE.posts() else { return };
        while posts.next().await {
            let Some(inner) = weak.upgrade() else { return };
            inner.realign().await;
        }
    });
    let weak = Arc::downgrade(inner);
    let wakes = tokio::spawn(async move {
        loop {
            next_wake(WAKE_CHECK).await;
            let Some(inner) = weak.upgrade() else { return };
            inner.realign().await;
        }
    });
    vec![changes, wakes]
}

/// This device's iroh key, made the first time.
fn secret_key(directory: &Path) -> Result<SecretKey> {
    let path = directory.join("endpoint.key");
    match std::fs::read(&path) {
        Ok(data) => {
            let bytes: [u8; 32] = data.try_into().map_err(|_| failed("peer-sync/endpoint.key is damaged."))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let mut options = File::options();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&path).map_err(failed)?;
            std::io::Write::write_all(&mut file, &key.to_bytes()).and_then(|()| file.sync_all()).map_err(failed)?;
            Ok(key)
        }
        Err(error) => Err(failed(error)),
    }
}

async fn bind(secret: SecretKey, network: Network, relay_url: Option<&str>) -> Result<Endpoint> {
    let builder = match network {
        Network::Public => {
            let builder = Endpoint::builder(presets::N0);
            match relay_url {
                Some(url) => {
                    let url = RelayUrl::from_str(url).map_err(|error| failed(format!("peer-sync/sync.json relayUrl: {error}")))?;
                    builder.relay_mode(RelayMode::custom([url]))
                }
                None => builder,
            }
        }
        #[cfg(test)]
        Network::Loopback(lookup) => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .map_err(failed)?
            .address_lookup(lookup),
    };
    builder.secret_key(secret).bind().await.map_err(failed)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// The machine's name, as other devices list it.
fn device_name() -> String {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: writes at most `buffer.len()` bytes into a live buffer.
        if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0 {
            let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
            let name = String::from_utf8_lossy(&buffer[..end]);
            let name = name.trim_end_matches(".local");
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    #[cfg(windows)]
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        return name;
    }
    std::env::consts::OS.to_string()
}
