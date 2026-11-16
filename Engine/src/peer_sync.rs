//! Device sync between one user's machines (MS-348, MS-359): an iroh-docs document, the sync
//! group, of which every paired device holds a full replica. Devices reach each other over iroh
//! (QUIC, direct or through a relay, found through number 0's DNS address lookup), reconcile their
//! replicas whenever they meet and pass new entries on live through iroh-gossip; the values ride
//! in iroh-blobs. Nothing is stored on a server, so a change reaches another device once both
//! have been online at the same time (directly or through a third device of the group), or
//! through the optional cloud drop (`cloud_drop.rs`, MS-374), which carries encrypted copies.
//!
//! Everything lives in `peer-sync/` in the engine's support folder:
//! - `lock`: the one process with sync open holds it (redb opens its stores for one process
//!   only); another gets [`PeerSyncError::Busy`].
//! - `endpoint.key`: this device's iroh secret key; its public half, the `EndpointId`, is the
//!   device's address and name in the group.
//! - `docs/`: iroh-docs' store (`docs.redb`) and this device's author key (`default-author`),
//!   which signs what it writes.
//! - `blobs/`: iroh-blobs' store with the entries' values.
//! - `sync.json`: `{"namespace": …, "relayUrl": …, "paused": …, "admission": …}`: the group this
//!   device is in, a relay to use instead of number 0's public ones, whether sync is paused, and
//!   whether the group's devices were admitted (below).
//!
//! The group's entries are one key per item, each value JSON with its own time (Unix ms):
//! - `v1/trusted/<lowercased address>`: `{"trusted": true|false, "at": …}`; untrusting writes
//!   `trusted: false` (a tombstone), never deletes.
//! - `v1/setting/<key>`: `{"value": <json>, "at": …}`.
//! - `v1/meta/device/<endpoint id>`: `{"name": …, "platform": …, "lastSeen": …}`.
//! - `v1/admit/<endpoint id>`: `{"by": <endpoint id>, "at": …}`: device `by` admitted it.
//! - `v1/account/<lowercased address>` (MS-376): `{"record": <accounts.json record>|null, "at": …}`;
//!   removing writes `record: null`.
//! - `v1/devkey/<endpoint id>`: `{"key": …, "signature": …, "at": …}`, the device's X25519
//!   credential key, signed with its endpoint key ([`DeviceKey`]); entries whose signature
//!   doesn't verify are left out.
//! - `v1/cred/<recipient endpoint id>/<account>`: a refresh token and its OAuth client sealed to
//!   that device's credential key ([`SealedCredential`], `account_sync.rs`).
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
//! Admission (MS-380): iroh-docs serves a group to any device that names its id, iroh-blobs a
//! value to any device that names its hash, and the group's id is no secret (settings and `sync
//! status` show it). So the protocols the group rides on (docs, blobs, gossip) serve only the
//! devices listed under `v1/admit/` ([`Gate`]), and a device reconciles only with those. Only
//! holders of the group's write key can write entries, and admissions travel only through the
//! gate: the cloud drop neither carries nor merges them. A device that joins asks the devices its
//! ticket names, and later the ones the group lists, to admit it at the door, ALPN
//! `mailstream/admit/1` ([`Door`]), until one does: today it proves it holds the write key, which
//! the ticket carries. A device whose group predates MS-380 admits every device the group lists
//! the first time it opens it (`admission` in `sync.json`), so existing groups keep syncing.
//!
//! The engine bridges this to settings.json (MS-360) and the signed-in accounts (MS-376); this
//! module knows nothing of either.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::str::FromStr;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt};
use iroh::endpoint::{Connection, VarInt, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, Signature};
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
use iroh_docs::{AuthorId, Capability, ContentStatus, DocTicket, NamespaceId, NamespacePublicKey, NamespaceSecret};
use iroh_gossip::net::Gossip;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, watch};
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
const ADMITTED: &str = "v1/admit/";
/// The door's ALPN ([`Door`]).
const ADMIT_ALPN: &[u8] = b"mailstream/admit/1";
/// Opens what a joiner signs with the group's write key to be admitted ([`proof_message`]).
const ADMIT_CONTEXT: &[u8] = b"mailstream device sync admission v1";
/// The most a door request or answer holds.
const ADMIT_LIMIT: usize = 4096;
/// How long a device waits for another to answer at the door.
const ADMIT_WAIT: Duration = Duration::from_secs(10);
/// How often a device that isn't admitted asks again.
const ADMIT_RETRY: Duration = Duration::from_secs(15);
/// How long a connection from a device not admitted yet waits for its admission ([`Gate::admits`]).
const ADMIT_GRACE: Duration = Duration::from_secs(10);
const ACCOUNT: &str = "v1/account/";
const DEVICE_KEY: &str = "v1/devkey/";
const CREDENTIAL: &str = "v1/cred/";
/// What a device signs to announce its credential key: this, its endpoint id, the key and when.
const DEVICE_KEY_CONTEXT: &[u8] = b"mailstream device sync credential key v1";
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
    /// Who admitted each device, by endpoint id (MS-380).
    pub admitted: BTreeMap<String, Admission>,
    /// Account records (accounts.json's) by lowercased address; `None` is a tombstone.
    pub accounts: BTreeMap<String, Stamped<Option<Value>>>,
    /// Credential keys by endpoint id, each verified against its device's endpoint key.
    pub device_keys: BTreeMap<String, DeviceKey>,
    /// Sealed credentials by `<recipient endpoint id>/<account>`.
    pub credentials: BTreeMap<String, SealedCredential>,
    /// Keys left out above because a value is still downloading: what they hold is unknown,
    /// not absent.
    pub pending: BTreeSet<String>,
}

/// How a device came into the group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Admission {
    /// The endpoint id of the device that admitted it.
    pub by: String,
    /// Unix ms.
    pub at: u64,
}

/// A device's X25519 credential key (MS-376), signed with the endpoint key whose public half is
/// the device's endpoint id, so no other device can announce a key in its name, also when the
/// cloud drop carries the entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceKey {
    /// The public key, hex.
    pub key: String,
    /// The endpoint key's signature over [`DEVICE_KEY_CONTEXT`], the endpoint id, the key and
    /// `at`, hex.
    pub signature: String,
    pub at: u64,
}

impl DeviceKey {
    fn message(endpoint_id: &EndpointId, key: &[u8; 32], at: u64) -> Vec<u8> {
        [DEVICE_KEY_CONTEXT, endpoint_id.as_bytes(), key, &at.to_be_bytes()].concat()
    }

    fn signed(secret: &SecretKey, key: &[u8; 32], at: u64) -> Self {
        let signature = secret.sign(&Self::message(&secret.public(), key, at));
        Self { key: hex(key), signature: hex(&signature.to_bytes()), at }
    }

    /// The public key, when the signature is `endpoint_id`'s.
    pub fn verified(&self, endpoint_id: &str) -> Option<[u8; 32]> {
        let id = EndpointId::from_str(endpoint_id).ok()?;
        let key: [u8; 32] = unhex(&self.key)?.try_into().ok()?;
        let signature: [u8; 64] = unhex(&self.signature)?.try_into().ok()?;
        id.verify(&Self::message(&id, &key, self.at), &iroh::Signature::from_bytes(&signature)).ok()?;
        Some(key)
    }
}

/// A refresh token and its client sealed to one device's credential key (`account_sync.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SealedCredential {
    /// The recipient's credential key it is sealed to, hex: a device whose key changed needs
    /// another.
    pub to: String,
    /// The sealed credential; `None` once withdrawn (its account was removed).
    pub sealed: Option<String>,
    /// When the credential inside was issued (its sign-in), Unix ms.
    pub issued: u64,
    /// When it was sealed, Unix ms: the latest wins.
    pub at: u64,
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

impl Status {
    /// Whether this device syncs a group: in one and not paused.
    pub fn syncing(&self) -> bool {
        self.namespace.is_some() && !self.paused
    }
}

/// What `peer-sync/` holds while sync isn't open, read without binding an endpoint
/// ([`PeerSync::stored`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    /// This device's endpoint id; `None` before sync first opened.
    pub endpoint_id: Option<String>,
    pub namespace: Option<String>,
    pub paused: bool,
}

impl Stored {
    /// Whether opening sync would rejoin a group: in one and not paused.
    pub fn syncing(&self) -> bool {
        self.namespace.is_some() && !self.paused
    }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountValue {
    record: Option<Value>,
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
    /// The group's devices were admitted: this device made or joined the group with admission
    /// (MS-380), or admitted the devices its older group listed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    admission: bool,
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
#[derive(Clone)]
pub enum Network {
    /// Number 0's address lookup and relays, or the relay `sync.json` names.
    Public,
    /// 127.0.0.1 only, devices found through a lookup the test shares: no relay or DNS.
    #[cfg(test)]
    Loopback(iroh::address_lookup::MemoryLookup),
    /// Loopback as a device from before MS-380: it neither admits nor asks, and lets every
    /// device sync.
    #[cfg(test)]
    BeforeAdmission(iroh::address_lookup::MemoryLookup),
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
    /// Who may sync the group.
    gate: Arc<Gate>,
    /// Wakes the admission loop ([`Inner::seek`]).
    asked: Arc<Notify>,
    /// Counts remote changes that became readable.
    changes: watch::Sender<u64>,
    /// A device from before MS-380 ([`Network::BeforeAdmission`]).
    #[cfg(test)]
    legacy: bool,
    /// One-time pairing codes this device issued (MS-376), redeemed at the admission door.
    invites: crate::sync_invite::Invites,
}

/// The group this device is in, if any.
struct Group {
    file: SyncFile,
    doc: Option<Doc>,
    /// Follows the doc's events.
    events: Option<JoinHandle<()>>,
    /// The devices the ticket this device joined with named, asked to admit it.
    candidates: Vec<EndpointAddr>,
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
        let endpoint = bind(secret_key(&directory)?, network.clone(), file.relay_url.as_deref()).await?;

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
        let author = match docs.author_default().await {
            Ok(author) => author,
            Err(error) => {
                ProtocolHandler::shutdown(&docs).await;
                blobs.shutdown().await.ok();
                endpoint.close().await;
                return Err(failed(error));
            }
        };
        let gate = Arc::new(Gate::default());
        #[cfg(test)]
        let legacy = matches!(network, Network::BeforeAdmission(_));
        #[cfg(test)]
        {
            gate.state.write().expect("gate lock").open = legacy;
        }
        let door = Door { gate: gate.clone(), docs: docs.clone(), author, me: endpoint.id() };
        // Every protocol of the group passes the gate; the door stands outside it.
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, Gated { gate: gate.clone(), protocol: iroh_blobs::BlobsProtocol::new(&blobs, None) })
            .accept(iroh_gossip::ALPN, Gated { gate: gate.clone(), protocol: gossip })
            .accept(iroh_docs::ALPN, Gated { gate: gate.clone(), protocol: docs.clone() })
            .accept(ADMIT_ALPN, door)
            .spawn();

        let inner = Arc::new(Inner {
            directory,
            _lock: lock,
            router,
            blobs,
            docs,
            author,
            group: tokio::sync::Mutex::new(Group { file, doc: None, events: None, candidates: Vec::new() }),
            gate,
            asked: Arc::new(Notify::new()),
            changes: watch::channel(0).0,
            #[cfg(test)]
            legacy,
            invites: Default::default(),
        });
        let sync = Self { watchers: watch_network(&inner), inner };
        #[cfg(test)]
        if let Network::Loopback(lookup) | Network::BeforeAdmission(lookup) = network {
            lookup.add_endpoint_info(sync.loopback_addr());
        }
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
    /// (trusted senders, settings and account records, at their times; no credential keys or
    /// sealed credentials, which belong to the old group's devices), and leaves the current one:
    /// devices that were in it are no longer reached or admitted (pair them again). Returns the group's id.
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
            for (id, account) in carried.accounts {
                let value = AccountValue { record: account.value, at: account.at };
                inner.put(&doc, format!("{ACCOUNT}{id}"), &value).await?;
            }
        }
        if !inner.legacy() {
            let me = inner.endpoint().id();
            inner.put(&doc, admit_key(me), &Admission { by: me.to_string(), at: now_ms() }).await?;
        }
        group.file.admission = !inner.legacy();
        let events = doc.subscribe().await.map_err(failed)?;
        inner.replace(&mut group, doc, events).await?;
        if !group.file.paused {
            let doc = group.doc.as_ref().expect("just set");
            doc.start_sync(Vec::new()).await.map_err(failed)?;
        }
        Ok(namespace.to_string())
    }

    /// A ticket another device joins the group with: the group's write key and this device's
    /// addresses. Whoever has it can read and write the group, and be admitted to it, until it is
    /// replaced ([`PeerSync::create_group`]).
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

    /// A one-time pairing code for another device (MS-376): this device's address, the group's
    /// id and a secret this device redeems once within [`crate::sync_invite::LIFETIME`]; with
    /// when it expires, Unix ms. It holds no group key.
    pub async fn invite(&self) -> Result<(String, u64)> {
        let group = self.inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let endpoint = self.inner.endpoint();
        if !endpoint.addr().relay_urls().any(|_| true) {
            tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await.ok();
        }
        let (secret, expires) = self.inner.invites.issue().map_err(failed)?;
        let code = crate::sync_invite::Code::new(endpoint.addr(), &doc.id().to_string(), &secret);
        Ok((code.to_string(), expires))
    }

    /// How many pairing codes this device issued are still redeemable.
    pub fn open_invites(&self) -> usize {
        self.inner.invites.open()
    }

    /// Joins the group of `ticket` (see [`PeerSync::ticket`]) and syncs with the devices it
    /// names, resuming sync if paused. The group this device was in, if another, is deleted
    /// from it. The group's devices sync with this one once one of them admitted it, which it
    /// asks for from here on ([`Inner::seek`]); the ones from before MS-380 sync at once.
    pub async fn join(&self, ticket: &str) -> Result<()> {
        let DocTicket { capability, nodes } =
            DocTicket::from_str(ticket.trim()).map_err(|error| failed(format!("Not a device sync ticket: {error}")))?;
        let inner = &self.inner;
        let mut group = inner.group.lock().await;
        if group.file.paused {
            group.file.paused = false;
            group.file.write(&inner.directory)?;
        }
        if let Some(doc) = group.doc.as_ref().filter(|doc| doc.id() == capability.id()) {
            doc.start_sync(nodes.clone()).await.map_err(failed)?;
        } else {
            let doc = inner.docs.import_namespace(capability).await.map_err(failed)?;
            let events = doc.subscribe().await.map_err(failed)?;
            doc.start_sync(nodes.clone()).await.map_err(failed)?;
            group.file.admission = !inner.legacy();
            inner.replace(&mut group, doc, events).await?;
        }
        group.candidates = nodes;
        inner.asked.notify_one();
        Ok(())
    }

    /// The group id named by `ticket`, validated without joining it.
    pub fn ticket_namespace(ticket: &str) -> Result<String> {
        let ticket = DocTicket::from_str(ticket.trim())
            .map_err(|error| failed(format!("Not a device sync ticket: {error}")))?;
        Ok(ticket.capability.id().to_string())
    }

    /// Reads the group and endpoint id in `support_directory/peer-sync` without opening sync:
    /// opening binds an iroh endpoint, which reaches number 0's relays and publishes this
    /// device's address, so the engine opens sync only while it syncs a group (MS-370).
    pub fn stored(support_directory: &Path) -> Result<Stored> {
        let directory = support_directory.join(FOLDER);
        let file = SyncFile::read(&directory)?;
        let endpoint_id = read_secret_key(&directory)?.map(|key| key.public().to_string());
        Ok(Stored { endpoint_id, namespace: file.namespace, paused: file.paused })
    }

    /// Whether another process has sync open on `support_directory`; asks the lock only, so
    /// call it while this process hasn't opened sync there.
    pub fn busy(support_directory: &Path) -> bool {
        File::options()
            .write(true)
            .open(support_directory.join(FOLDER).join("lock"))
            .is_ok_and(|lock| matches!(lock.try_lock(), Err(TryLockError::WouldBlock)))
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

    /// Records account `id`'s record (`None`: removed) at `at` (Unix ms); `false` when the group
    /// already holds a value from `at` or later, which stays.
    pub async fn set_account(&self, id: &str, record: Option<Value>, at: u64) -> Result<bool> {
        self.set(format!("{ACCOUNT}{id}"), &AccountValue { record, at }, at).await
    }

    /// Announces `key` as this device's credential key, signed with its endpoint key; `false`
    /// when the group already holds it.
    pub async fn announce_device_key(&self, key: &[u8; 32]) -> Result<bool> {
        let inner = &self.inner;
        let secret = inner.endpoint().secret_key().clone();
        let entry = format!("{DEVICE_KEY}{}", secret.public());
        let group = inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let current = inner.collect(doc, Query::key_exact(&entry).build()).await?;
        let mut latest = 0;
        for found in &current {
            if let Some(Record::DeviceKey(announced)) = &found.record {
                if announced.key == hex(key) {
                    return Ok(false);
                }
                latest = latest.max(announced.at);
            }
        }
        let announced = DeviceKey::signed(&secret, key, now_ms().max(latest + 1));
        inner.put(doc, entry, &announced).await?;
        Ok(true)
    }

    /// Records `credential` for `recipient` (an endpoint id) and `account`; `false` when the
    /// group already holds one sealed at `credential.at` or later, which stays.
    pub async fn set_credential(&self, recipient: &str, account: &str, credential: &SealedCredential) -> Result<bool> {
        self.set(format!("{CREDENTIAL}{recipient}/{account}"), credential, credential.at).await
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

    /// The group's write secret, which every device of the group holds (a ticket hands it over)
    /// and the cloud drop derives its keys from (MS-374); `None` outside a group or while paused
    /// (iroh-docs starts syncing a document it shares).
    pub async fn group_secret(&self) -> Result<Option<[u8; 32]>> {
        let group = self.inner.group.lock().await;
        let Some(doc) = group.doc.as_ref().filter(|_| !group.file.paused) else { return Ok(None) };
        Ok(write_secret(doc).await?.map(|secret| secret.to_bytes()))
    }

    /// Writes what `incoming` (another device's state, as the cloud drop carried it) holds that
    /// is newer than this replica's, each value at its own time, as syncing with that device
    /// would have; this device's own description stays. Admissions and `pending` are ignored:
    /// admissions cross only between admitted devices. Returns whether anything was written.
    pub async fn merge(&self, incoming: &Snapshot) -> Result<bool> {
        let inner = &self.inner;
        let group = inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let current = inner.read(doc).await?;
        let newer = |at: u64, have: Option<u64>| have.is_none_or(|have| at > have);
        let mut wrote = false;
        for (address, trusted) in &incoming.trusted {
            if newer(trusted.at, current.trusted.get(address).map(|have| have.at)) {
                let value = TrustedValue { trusted: trusted.value, at: trusted.at };
                inner.put(doc, format!("{TRUSTED}{address}"), &value).await?;
                wrote = true;
            }
        }
        for (key, setting) in &incoming.settings {
            if newer(setting.at, current.settings.get(key).map(|have| have.at)) {
                let value = SettingValue { value: setting.value.clone(), at: setting.at };
                inner.put(doc, format!("{SETTING}{key}"), &value).await?;
                wrote = true;
            }
        }
        let me = inner.endpoint().id().to_string();
        for (id, device) in incoming.devices.iter().filter(|(id, _)| **id != me) {
            if newer(device.last_seen, current.devices.get(id).map(|have| have.last_seen)) {
                inner.put(doc, format!("{DEVICE}{id}"), device).await?;
                wrote = true;
            }
        }
        for (id, account) in &incoming.accounts {
            if newer(account.at, current.accounts.get(id).map(|have| have.at)) {
                let value = AccountValue { record: account.value.clone(), at: account.at };
                inner.put(doc, format!("{ACCOUNT}{id}"), &value).await?;
                wrote = true;
            }
        }
        for (id, key) in incoming.device_keys.iter().filter(|(id, _)| **id != me) {
            if key.verified(id).is_some() && newer(key.at, current.device_keys.get(id).map(|have| have.at)) {
                inner.put(doc, format!("{DEVICE_KEY}{id}"), key).await?;
                wrote = true;
            }
        }
        for (slot, credential) in &incoming.credentials {
            if newer(credential.at, current.credentials.get(slot).map(|have| have.at)) {
                inner.put(doc, format!("{CREDENTIAL}{slot}"), credential).await?;
                wrote = true;
            }
        }
        Ok(wrote)
    }

    /// Where the loopback endpoint listens, for the tests' shared lookup.
    #[cfg(test)]
    pub(crate) fn loopback_addr(&self) -> EndpointAddr {
        let endpoint = self.inner.endpoint();
        endpoint.bound_sockets().into_iter().fold(EndpointAddr::new(endpoint.id()), EndpointAddr::with_ip_addr)
    }

    /// Syncs group `namespace` read-only with `peers`, as a device that knows only the group's
    /// id would (shown in settings and `sync status`): the tests' stranger.
    #[cfg(test)]
    pub(crate) async fn watch(&self, namespace: &str, peers: Vec<EndpointAddr>) -> Result<()> {
        let namespace = NamespaceId::from_str(namespace).map_err(failed)?;
        let inner = &self.inner;
        let mut group = inner.group.lock().await;
        let doc = inner.docs.import_namespace(Capability::Read(namespace)).await.map_err(failed)?;
        let events = doc.subscribe().await.map_err(failed)?;
        doc.start_sync(peers).await.map_err(failed)?;
        group.events = Some(follow(events, inner.changes.clone(), inner.gate.clone(), doc.clone()));
        group.doc = Some(doc);
        Ok(())
    }

    /// This device's proof of the group's write key, for asking `member` at its door.
    #[cfg(test)]
    pub(crate) async fn admission_proof(&self, member: EndpointId) -> Result<AdmitRequest> {
        let group = self.inner.group.lock().await;
        let doc = group.doc.as_ref().ok_or(PeerSyncError::NoGroup)?;
        let secret = write_secret(doc).await?.ok_or(PeerSyncError::NoGroup)?;
        Ok(proof(&secret, member, self.inner.endpoint().id()))
    }

    /// Asks `member` at its door to admit this device with `request`.
    #[cfg(test)]
    pub(crate) async fn ask_admission(&self, member: EndpointAddr, request: &AdmitRequest) -> Result<()> {
        ask(self.inner.endpoint(), member, request).await
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

    /// Opens the group `sync.json` names, rejoining it unless paused. A group from before MS-380
    /// gets its devices admitted: every device it lists, once.
    async fn reopen(&self, group: &mut Group, namespace: NamespaceId) -> Result<()> {
        let Some(doc) = self.docs.open(namespace).await.map_err(failed)? else {
            return Ok(());
        };
        let events = doc.subscribe().await.map_err(failed)?;
        group.events = Some(follow(events, self.changes.clone(), self.gate.clone(), doc.clone()));
        self.describe(&doc).await?;
        if !group.file.admission && !self.legacy() {
            let me = self.endpoint().id();
            let admitted = listed(&doc, ADMITTED).await;
            for id in listed(&doc, DEVICE).await.difference(&admitted) {
                self.put(&doc, admit_key(*id), &Admission { by: me.to_string(), at: now_ms() }).await?;
            }
            group.file.admission = true;
            group.file.write(&self.directory)?;
        }
        self.gate.load(&doc).await;
        if !group.file.paused {
            let peers = self.peers(&doc).await;
            doc.start_sync(peers).await.map_err(failed)?;
        }
        group.doc = Some(doc);
        self.asked.notify_one();
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
        group.events = Some(follow(events, self.changes.clone(), self.gate.clone(), doc.clone()));
        self.describe(&doc).await?;
        self.gate.load(&doc).await;
        group.file.namespace = Some(doc.id().to_string());
        group.doc = Some(doc);
        group.candidates.clear();
        group.file.write(&self.directory)
    }

    /// Records this device's name, platform and the time in the group.
    async fn describe(&self, doc: &Doc) -> Result<()> {
        let device = Device { name: device_name(), platform: std::env::consts::OS.to_string(), last_seen: now_ms() };
        self.put(doc, format!("{DEVICE}{}", self.endpoint().id()), &device).await
    }

    async fn put(&self, doc: &Doc, key: String, value: &impl Serialize) -> Result<()> {
        put(doc, self.author, key, value).await
    }

    /// Whether this is a device from before MS-380 ([`Network::BeforeAdmission`]).
    fn legacy(&self) -> bool {
        #[cfg(test)]
        return self.legacy;
        #[cfg(not(test))]
        false
    }

    async fn realign(&self) {
        self.endpoint().network_change().await;
        self.asked.notify_one();
        let group = self.group.lock().await;
        let Some(doc) = group.doc.as_ref().filter(|_| !group.file.paused) else { return };
        // Failures wait for the next change or wake.
        let peers = self.peers(doc).await;
        if doc.start_sync(peers).await.is_ok() {
            self.describe(doc).await.ok();
        }
    }

    /// Until a device of the group admits this one (MS-380), asks each device it knows of at its
    /// door: the ones the ticket it joined with named and the ones the group lists. Syncs with
    /// the ones that admitted it.
    async fn seek(&self) {
        let me = self.endpoint().id();
        let (doc, members) = {
            let group = self.group.lock().await;
            let Some(doc) = group.doc.as_ref().filter(|_| !group.file.paused) else { return };
            if self.gate.allows(&me) {
                return;
            }
            let mut members: BTreeMap<EndpointId, EndpointAddr> =
                group.candidates.iter().map(|member| (member.id, member.clone())).collect();
            for id in listed(doc, DEVICE).await.into_iter().filter(|id| *id != me) {
                members.entry(id).or_insert_with(|| EndpointAddr::new(id));
            }
            (doc.clone(), members)
        };
        let Ok(Some(secret)) = write_secret(&doc).await else { return };
        let endpoint = self.endpoint();
        let asks = members.into_values().map(|member| {
            let request = proof(&secret, member.id, me);
            async move {
                let asked = tokio::time::timeout(ADMIT_WAIT, ask(endpoint, member.clone(), &request)).await;
                matches!(asked, Ok(Ok(()))).then_some(member)
            }
        });
        let admitted: Vec<EndpointAddr> = futures::future::join_all(asks).await.into_iter().flatten().collect();
        let Some(by) = admitted.first() else { return };
        for member in &admitted {
            self.gate.vouch(doc.id(), member.id);
        }
        // Its own record, so it stops asking before the admitting device's arrives.
        if self.put(&doc, admit_key(me), &Admission { by: by.id.to_string(), at: now_ms() }).await.is_ok() {
            self.gate.add(doc.id(), me);
        }
        doc.start_sync(admitted).await.ok();
    }

    /// The other admitted devices of the group, by the descriptions they wrote; iroh-docs adds
    /// the ones it synced with itself.
    async fn peers(&self, doc: &Doc) -> Vec<EndpointAddr> {
        let me = self.endpoint().id();
        let devices = listed(doc, DEVICE).await;
        devices.into_iter().filter(|id| *id != me && self.gate.allows(id)).map(EndpointAddr::new).collect()
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
                Record::Admitted(admission) => {
                    snapshot.admitted.insert(key[ADMITTED.len()..].to_string(), admission);
                }
                Record::Account(value) => {
                    let stamped = Stamped { value: value.record, at: value.at };
                    snapshot.accounts.insert(key[ACCOUNT.len()..].to_string(), stamped);
                }
                Record::DeviceKey(announced) => {
                    snapshot.device_keys.insert(key[DEVICE_KEY.len()..].to_string(), announced);
                }
                Record::Credential(credential) => {
                    snapshot.credentials.insert(key[CREDENTIAL.len()..].to_string(), credential);
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
    Admitted(Admission),
    Account(AccountValue),
    DeviceKey(DeviceKey),
    Credential(SealedCredential),
}

impl Record {
    /// `None` for keys outside the scheme, values that aren't the JSON their key calls for and
    /// credential keys whose signature isn't their device's.
    fn parse(key: &str, data: &[u8]) -> Option<Self> {
        if key.starts_with(TRUSTED) {
            serde_json::from_slice(data).ok().map(Self::Trusted)
        } else if key.starts_with(SETTING) {
            serde_json::from_slice(data).ok().map(Self::Setting)
        } else if key.starts_with(DEVICE) {
            serde_json::from_slice(data).ok().map(Self::Device)
        } else if key.starts_with(ADMITTED) {
            serde_json::from_slice(data).ok().map(Self::Admitted)
        } else if key.starts_with(ACCOUNT) {
            serde_json::from_slice(data).ok().map(Self::Account)
        } else if let Some(endpoint_id) = key.strip_prefix(DEVICE_KEY) {
            let announced: DeviceKey = serde_json::from_slice(data).ok()?;
            announced.verified(endpoint_id)?;
            Some(Self::DeviceKey(announced))
        } else if key.starts_with(CREDENTIAL) {
            serde_json::from_slice(data).ok().map(Self::Credential)
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
            Self::Admitted(admission) => admission.at,
            Self::Account(value) => value.at,
            Self::DeviceKey(announced) => announced.at,
            Self::Credential(credential) => credential.at,
        }
    }
}

/// Counts the group's remote changes once readable, and lets in the devices `doc` admits as
/// their admissions arrive.
fn follow<E: Send + 'static>(
    events: impl Stream<Item = std::result::Result<LiveEvent, E>> + Send + 'static,
    changes: watch::Sender<u64>,
    gate: Arc<Gate>,
    doc: Doc,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = pin!(events);
        while let Some(event) = events.next().await {
            if let Ok(LiveEvent::InsertLocal { entry } | LiveEvent::InsertRemote { entry, .. }) = &event
                && entry.key().starts_with(ADMITTED.as_bytes())
            {
                gate.load(&doc).await;
            }
            if let Ok(LiveEvent::InsertRemote { content_status: ContentStatus::Complete, .. } | LiveEvent::ContentReady { .. }) =
                event
            {
                changes.send_modify(|count| *count += 1);
            }
        }
    })
}

/// Realigns after each network change and wake, and asks to be admitted until it is, while
/// `inner` lives.
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
    let weak = Arc::downgrade(inner);
    let asked = inner.asked.clone();
    let admission = tokio::spawn(async move {
        loop {
            let Some(inner) = weak.upgrade() else { return };
            inner.seek().await;
            drop(inner);
            tokio::select! {
                () = asked.notified() => {}
                () = tokio::time::sleep(ADMIT_RETRY) => {}
            }
        }
    });
    vec![changes, wakes, admission]
}

/// Who may sync the group (MS-380): the devices it admitted. Every protocol the group rides on
/// asks it before serving a connection ([`Gated`]).
#[derive(Debug, Default)]
struct Gate {
    state: RwLock<GateState>,
    /// Wakes connections waiting to be let in.
    changed: Notify,
}

#[derive(Debug, Default)]
struct GateState {
    /// The group open now.
    namespace: Option<NamespaceId>,
    admitted: BTreeSet<EndpointId>,
    /// The devices that admitted this one at their door ([`Inner::seek`]), let in before their
    /// own admission arrives: each is one the ticket or the group names, and it dials this one
    /// as soon as it admitted it.
    vouched: BTreeSet<EndpointId>,
    /// Lets every device in, as before MS-380 ([`Network::BeforeAdmission`]).
    #[cfg(test)]
    open: bool,
}

impl Gate {
    fn allows(&self, id: &EndpointId) -> bool {
        let state = self.state.read().expect("gate lock");
        #[cfg(test)]
        if state.open {
            return true;
        }
        state.namespace.is_some() && (state.admitted.contains(id) || state.vouched.contains(id))
    }

    /// Whether `id` is let in now or within [`ADMIT_GRACE`]: a device that joins connects while
    /// it asks to be admitted, and a device hears who else was admitted in the same
    /// reconciliation after which they fetch values from it.
    async fn admits(&self, id: &EndpointId) -> bool {
        let admitted = async {
            loop {
                let mut changed = pin!(self.changed.notified());
                changed.as_mut().enable();
                if self.allows(id) {
                    return;
                }
                changed.await;
            }
        };
        tokio::time::timeout(ADMIT_GRACE, admitted).await.is_ok()
    }

    fn namespace(&self) -> Option<NamespaceId> {
        self.state.read().expect("gate lock").namespace
    }

    /// Lets in the devices `doc` admits, and no others.
    async fn load(&self, doc: &Doc) {
        let admitted = listed(doc, ADMITTED).await;
        let mut state = self.state.write().expect("gate lock");
        if state.namespace != Some(doc.id()) {
            state.vouched.clear();
        }
        state.namespace = Some(doc.id());
        state.admitted = admitted;
        drop(state);
        self.changed.notify_waiters();
    }

    /// Lets in `id` before its admission is read back from the group.
    fn add(&self, namespace: NamespaceId, id: EndpointId) {
        let mut state = self.state.write().expect("gate lock");
        if state.namespace == Some(namespace) {
            state.admitted.insert(id);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// Lets in `id`, which admitted this device ([`GateState::vouched`]).
    fn vouch(&self, namespace: NamespaceId, id: EndpointId) {
        let mut state = self.state.write().expect("gate lock");
        if state.namespace == Some(namespace) {
            state.vouched.insert(id);
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

/// A protocol of the group, served only to the devices the gate lets in. iroh-docs serves a
/// group to whoever names its id, iroh-blobs a value to whoever names its hash, and gossip
/// passes new entries on to its neighbours.
#[derive(Debug)]
struct Gated<P> {
    gate: Arc<Gate>,
    protocol: P,
}

impl<P: ProtocolHandler> ProtocolHandler for Gated<P> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if !self.gate.admits(&connection.remote_id()).await {
            connection.close(VarInt::from_u32(403), b"not admitted to this sync group");
            return Ok(());
        }
        self.protocol.accept(connection).await
    }

    async fn shutdown(&self) {
        self.protocol.shutdown().await;
    }
}

/// What a device asks the door ([`Door`]) for: to be admitted, with one variant per way in, so a
/// way can be added or dropped on its own.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AdmitRequest {
    /// The asking device holds the group's write key (today's tickets carry it): its signature
    /// with that key over [`proof_message`], hex.
    Proof { namespace: String, signature: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AdmitAnswer {
    Admitted,
    Refused(String),
}

/// The door (MS-380), ALPN `mailstream/admit/1`, outside the gate: a device that asks to be
/// admitted and shows it may be is recorded under `v1/admit/` and let in.
#[derive(Debug, Clone)]
struct Door {
    gate: Arc<Gate>,
    docs: Docs,
    author: AuthorId,
    /// This device.
    me: EndpointId,
}

impl ProtocolHandler for Door {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await.map_err(AcceptError::from_err)?;
        let request = recv.read_to_end(ADMIT_LIMIT).await.map_err(AcceptError::from_err)?;
        let answer = self.answer(connection.remote_id(), &request).await;
        send.write_all(&serde_json::to_vec(&answer).map_err(AcceptError::from_err)?).await.map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;
        // The asking device closes once it read the answer.
        connection.closed().await;
        Ok(())
    }
}

impl Door {
    async fn answer(&self, asking: EndpointId, request: &[u8]) -> AdmitAnswer {
        let Some(namespace) = self.gate.namespace() else {
            return AdmitAnswer::Refused(PeerSyncError::NoGroup.to_string());
        };
        let shown = match serde_json::from_slice(request) {
            Ok(AdmitRequest::Proof { namespace: asked, signature }) => {
                asked == namespace.to_string() && proves(namespace, self.me, asking, &signature)
            }
            Err(_) => false,
        };
        if !shown {
            return AdmitAnswer::Refused("Not admitted: no proof of this sync group.".to_string());
        }
        if !self.gate.allows(&asking)
            && let Err(error) = self.admit(namespace, asking).await
        {
            return AdmitAnswer::Refused(error.to_string());
        }
        AdmitAnswer::Admitted
    }

    async fn admit(&self, namespace: NamespaceId, id: EndpointId) -> Result<()> {
        let doc = self.docs.open(namespace).await.map_err(failed)?.ok_or(PeerSyncError::NoGroup)?;
        put(&doc, self.author, admit_key(id), &Admission { by: self.me.to_string(), at: now_ms() }).await?;
        self.gate.add(namespace, id);
        Ok(())
    }
}

/// What a device signs with the group's write key to be admitted: the group, the device it asks
/// and itself, so the signature serves that pair only.
fn proof_message(namespace: NamespaceId, member: EndpointId, joiner: EndpointId) -> Vec<u8> {
    [ADMIT_CONTEXT, namespace.as_bytes(), member.as_bytes(), joiner.as_bytes()].concat()
}

/// Asks `member` to admit device `joiner` with the group's write key.
fn proof(secret: &NamespaceSecret, member: EndpointId, joiner: EndpointId) -> AdmitRequest {
    let signature = secret.sign(&proof_message(secret.id(), member, joiner));
    AdmitRequest::Proof { namespace: secret.id().to_string(), signature: hex(&signature.to_bytes()) }
}

/// Whether `signature` (hex) proves `joiner` holds the write key of `namespace`, asking `member`.
fn proves(namespace: NamespaceId, member: EndpointId, joiner: EndpointId, signature: &str) -> bool {
    let Some(bytes) = unhex(signature).and_then(|bytes| <[u8; Signature::LENGTH]>::try_from(bytes).ok()) else {
        return false;
    };
    let Ok(key) = NamespacePublicKey::from_bytes(namespace.as_bytes()) else { return false };
    key.verify(&proof_message(namespace, member, joiner), &Signature::from_bytes(&bytes)).is_ok()
}

/// Asks `member` at its door to admit this device; `Err` with the reason when it doesn't.
async fn ask(endpoint: &Endpoint, member: EndpointAddr, request: &AdmitRequest) -> Result<()> {
    let connection = endpoint.connect(member, ADMIT_ALPN).await.map_err(failed)?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(failed)?;
    send.write_all(&serde_json::to_vec(request).map_err(failed)?).await.map_err(failed)?;
    send.finish().map_err(failed)?;
    let answer = recv.read_to_end(ADMIT_LIMIT).await.map_err(failed);
    connection.close(VarInt::from_u32(0), b"");
    match serde_json::from_slice(&answer?).map_err(failed)? {
        AdmitAnswer::Admitted => Ok(()),
        AdmitAnswer::Refused(reason) => Err(PeerSyncError::Failed(reason)),
    }
}

/// `doc`'s write key; `None` for a group this device can only read.
async fn write_secret(doc: &Doc) -> Result<Option<NamespaceSecret>> {
    let ticket = doc.share(ShareMode::Write, AddrInfoOptions::Id).await.map_err(failed)?;
    match ticket.capability {
        Capability::Write(secret) => Ok(Some(secret)),
        Capability::Read(_) => Ok(None),
    }
}

/// The devices named by `doc`'s keys under `prefix`, whatever their values; every entry carries
/// the group's signature.
async fn listed(doc: &Doc, prefix: &str) -> BTreeSet<EndpointId> {
    let mut ids = BTreeSet::new();
    if let Ok(entries) = doc.get_many(Query::key_prefix(prefix).build()).await {
        let mut entries = pin!(entries);
        while let Some(Ok(entry)) = entries.next().await {
            let id = std::str::from_utf8(entry.key()).ok().and_then(|key| key.strip_prefix(prefix));
            if let Some(id) = id.and_then(|id| EndpointId::from_str(id).ok()) {
                ids.insert(id);
            }
        }
    }
    ids
}

fn admit_key(id: EndpointId) -> String {
    format!("{ADMITTED}{id}")
}

async fn put(doc: &Doc, author: AuthorId, key: String, value: &impl Serialize) -> Result<()> {
    let data = serde_json::to_vec(value).map_err(failed)?;
    doc.set_bytes(author, key, data).await.map_err(failed)?;
    Ok(())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect()
}

/// This device's iroh key, made the first time.
fn secret_key(directory: &Path) -> Result<SecretKey> {
    if let Some(key) = read_secret_key(directory)? {
        return Ok(key);
    }
    let key = SecretKey::generate();
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(directory.join("endpoint.key")).map_err(failed)?;
    std::io::Write::write_all(&mut file, &key.to_bytes()).and_then(|()| file.sync_all()).map_err(failed)?;
    Ok(key)
}

/// This device's iroh key, if made.
fn read_secret_key(directory: &Path) -> Result<Option<SecretKey>> {
    match std::fs::read(directory.join("endpoint.key")) {
        Ok(data) => {
            let bytes: [u8; 32] = data.try_into().map_err(|_| failed("peer-sync/endpoint.key is damaged."))?;
            Ok(Some(SecretKey::from_bytes(&bytes)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
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
        Network::Loopback(lookup) | Network::BeforeAdmission(lookup) => Endpoint::builder(presets::Minimal)
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
