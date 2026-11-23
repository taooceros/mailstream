//! Bridges [`PeerSync`] (`peer-sync/`) to the whitelisted cross-device keys in `settings.json`
//! without feedback loops (MS-360) and to the signed-in accounts (`account_sync.rs`, MS-376),
//! runs the optional cloud drop (`cloud_drop.rs`, MS-374), and answers the `sync.*` commands.
//! Sync is open only while this device syncs a group, or a command needs it: opening binds an
//! iroh endpoint, which reaches number 0's relays (MS-370).
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex as AsyncMutex, Notify, RwLock, mpsc, watch};
use tokio::task::JoinHandle;

use crate::account_sync::{self, AccountHost, DropKey};
use crate::answers::{self, DeviceSyncStatus, SyncDevice, SyncTicket};
use crate::cloud_drop::{self, CloudDrop};
use crate::commands::Arguments;
use crate::peer_mail::PeerMail;
use crate::peer_sync::{CloudConfig, Network, PeerSync, PeerSyncError, Service, Snapshot, Stamped, Stored};
use crate::settings::{Setting, Settings, Values};
use crate::signal::{ProcessSignal, next_wake};
use crate::{EngineError, Events, files, model};

const BRIDGE_FILE: &str = "bridged.json";
const RETRY_BUSY: Duration = Duration::from_secs(5);
const RETRY_FAILED: Duration = Duration::from_secs(30);
/// How often to look whether the machine slept, to exchange with the cloud drop on waking.
const WAKE_CHECK: Duration = Duration::from_secs(10);

/// When the settings a device brings into a group it joins were set, as far as the group can
/// tell (MS-367): older than anything a device writes, so a value the group already holds for the
/// same key outweighs it whenever that value arrives, and a key only this device holds still
/// reaches every device.
const JOIN_AT: u64 = 1;
/// How often closed sync looks whether another process put this device in a group.
const IDLE_CHECK: Duration = Duration::from_secs(30);
/// How long an idle backgrounded app waits before closing its iroh endpoint so quick app
/// switches and OAuth browser handoffs stay warm while long background sessions use zero QUIC
/// traffic (MS-454).
pub(crate) const BACKGROUND_SUSPEND_DELAY: Duration = Duration::from_secs(15);

/// Scalar whitelisted settings synced under `v1/setting/<key>`.
/// `remoteContent.trustedSenders` is synced per address under `v1/trusted/<email>`.
/// Machine-local keys (`aiTagging.backend`/`model`/`ompPath`/`batchSize`/`passLimit`/
/// `backoffSeconds`, `sync.*`, `chat.pollSeconds`, `gpui.*`, `flutter.*`) stay local.
const SCALAR_SETTINGS: [Setting; 6] = [
    Setting::AutoReadEnabled,
    Setting::AutoReadDays,
    Setting::AiTaggingEnabled,
    Setting::AiTaggingDevicesMode,
    Setting::AiTaggingDevices,
    Setting::AiTaggingLocalOnlyAccounts,
];

/// Last reconciled group state, persisted in `peer-sync/bridged.json` so offline edits to
/// `settings.json` (e.g. via `mailstream config`) are detected on the next start.
///
/// Joining a group merges this device's settings into it (MS-367): what only the group holds
/// comes here, what only this device holds goes to the group, trusted senders end up the union of
/// both, and where both set a key differently (an address the group untrusted included) the
/// group's value wins, so joining never changes the other devices' settings.
#[derive(Clone, Debug, Default, PartialEq)]
struct Bridged {
    namespace: Option<String>,
    trusted: BTreeMap<String, Stamped<bool>>,
    settings: BTreeMap<String, Stamped<Value>>,
    /// Account records by id as last reconciled (accounts.json's JSON); `None`: removed.
    accounts: BTreeMap<String, Stamped<Option<Value>>>,
    /// Set by `sync.join` until the first reconcile with the joined group completes: pushes of
    /// keys without a bridged value are then stamped [`JOIN_AT`], not now. `join` returns
    /// before the group's entries have arrived, so "the group doesn't hold it" is unknown there.
    joining: bool,
}

/// On-disk layout of `peer-sync/bridged.json`: keeps plain value maps for backward compatibility
/// with pre-MS-429 files and stores per-key timestamps alongside them.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgedDisk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default)]
    trusted: BTreeMap<String, bool>,
    #[serde(default)]
    settings: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    accounts: BTreeMap<String, Option<Value>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    trusted_at: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings_at: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    accounts_at: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    joining: bool,
}

fn unpack_stamped<T>(values: BTreeMap<String, T>, times: &BTreeMap<String, u64>) -> BTreeMap<String, Stamped<T>> {
    values
        .into_iter()
        .map(|(k, value)| {
            let at = times.get(&k).copied().unwrap_or(0);
            (k, Stamped { value, at })
        })
        .collect()
}

fn pack_stamped<T: Clone>(entries: &BTreeMap<String, Stamped<T>>) -> (BTreeMap<String, T>, BTreeMap<String, u64>) {
    let mut values = BTreeMap::new();
    let mut times = BTreeMap::new();
    for (k, entry) in entries {
        values.insert(k.clone(), entry.value.clone());
        if entry.at > 0 {
            times.insert(k.clone(), entry.at);
        }
    }
    (values, times)
}

impl Bridged {
    fn path(support_directory: &Path) -> PathBuf {
        support_directory.join("peer-sync").join(BRIDGE_FILE)
    }

    fn read(support_directory: &Path) -> Self {
        let Ok(data) = std::fs::read(Self::path(support_directory)) else {
            return Self::default();
        };
        let Ok(disk) = serde_json::from_slice::<BridgedDisk>(&data) else {
            return Self::default();
        };
        Self {
            namespace: disk.namespace,
            trusted: unpack_stamped(disk.trusted, &disk.trusted_at),
            settings: unpack_stamped(disk.settings, &disk.settings_at),
            accounts: unpack_stamped(disk.accounts, &disk.accounts_at),
            joining: disk.joining,
        }
    }

    fn write(&self, support_directory: &Path) -> Result<(), EngineError> {
        let (trusted, trusted_at) = pack_stamped(&self.trusted);
        let (settings, settings_at) = pack_stamped(&self.settings);
        let (accounts, accounts_at) = pack_stamped(&self.accounts);
        let disk = BridgedDisk {
            namespace: self.namespace.clone(),
            trusted,
            settings,
            accounts,
            trusted_at,
            settings_at,
            accounts_at,
            joining: self.joining,
        };
        let dir = support_directory.join("peer-sync");
        let data = serde_json::to_vec_pretty(&disk).map_err(|error| EngineError::failed(error.to_string()))?;
        files::replace(&dir, &Self::path(support_directory), &data)
            .map_err(|error| EngineError::failed(error.to_string()))
    }
}

#[derive(Default)]
struct BridgeState {
    loaded: bool,
    bridged: Bridged,
    last_at: u64,
}

impl BridgeState {
    fn load(&mut self, support_directory: &Path, namespace: &str) -> &Bridged {
        let bridged = self.loaded(support_directory);
        if bridged.namespace.as_deref() != Some(namespace) {
            *bridged = Bridged { namespace: Some(namespace.to_string()), ..Default::default() };
        }
        bridged
    }

    /// After `sync.join` entered the group `namespace`: unless this device bridges that group
    /// already, its bridge starts over as joining, on disk so a restart before the first
    /// reconcile still merges.
    fn joined(&mut self, support_directory: &Path, namespace: &str) -> Result<(), EngineError> {
        if self.loaded(support_directory).namespace.as_deref() == Some(namespace) {
            return Ok(());
        }
        let bridged = Bridged { namespace: Some(namespace.to_string()), joining: true, ..Default::default() };
        bridged.write(support_directory)?;
        self.bridged = bridged;
        Ok(())
    }

    fn loaded(&mut self, support_directory: &Path) -> &mut Bridged {
        if !self.loaded {
            self.bridged = Bridged::read(support_directory);
            self.loaded = true;
        }
        &mut self.bridged
    }

    fn next_at(&mut self, min_at: u64) -> u64 {
        let next = now_ms().max(self.last_at + 1).max(min_at + 1);
        self.last_at = next;
        next
    }
}

#[derive(Default)]
struct StatusMeta {
    last_sync_ms: Option<u64>,
    problem: Option<String>,
}

/// Bridges `settings.json` and the accounts to `PeerSync` for one running `Engine` or sync node.
pub struct DeviceSync {
    support_directory: PathBuf,
    network: Network,
    settings: Arc<Settings>,
    events: Arc<Events>,
    open_gate: AsyncMutex<()>,
    sync: RwLock<Option<PeerSync>>,
    bridge: AsyncMutex<BridgeState>,
    meta: Mutex<StatusMeta>,
    wake_tx: mpsc::UnboundedSender<()>,
    cloud: CloudDrop,
    /// Asks for an exchange with the cloud drop.
    cloud_wake: Notify,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// The accounts and sign-ins to bridge, once attached.
    accounts: OnceLock<Weak<dyn AccountHost>>,
    /// Mail bodies served to and asked of the group's devices (MS-394); `None` on a sync node,
    /// which holds no mail.
    mail: Option<Arc<PeerMail>>,
    /// The cloud drop's key as the last reconcile left it, with the group it belongs to (MS-396).
    drop_key: Mutex<Option<(String, DropKey)>>,
    foreground: watch::Receiver<bool>,
    /// Keeps `foreground` open when no external lifecycle owner exists (e.g. `sync_node`).
    _foreground_keepalive: Option<watch::Sender<bool>>,
    background_suspend_delay_ms: std::sync::atomic::AtomicU64,
}

impl DeviceSync {
    /// Starts opening `PeerSync` and bridging `settings.json` in the background on the current
    /// tokio runtime. Never blocks `Engine::start`.
    pub(crate) fn start(
        support_directory: PathBuf,
        network: Network,
        settings: Arc<Settings>,
        events: Arc<Events>,
        mail: Option<Arc<PeerMail>>,
    ) -> Arc<Self> {
        let (tx, rx) = watch::channel(true);
        Self::start_inner(
            support_directory,
            network,
            settings,
            events,
            mail,
            rx,
            Some(tx),
            BACKGROUND_SUSPEND_DELAY,
        )
    }

    /// Starts `DeviceSync` following host foreground/background transitions (MS-454).
    pub(crate) fn start_with_foreground(
        support_directory: PathBuf,
        network: Network,
        settings: Arc<Settings>,
        events: Arc<Events>,
        mail: Option<Arc<PeerMail>>,
        foreground: watch::Receiver<bool>,
        background_suspend_delay: Duration,
    ) -> Arc<Self> {
        Self::start_inner(
            support_directory,
            network,
            settings,
            events,
            mail,
            foreground,
            None,
            background_suspend_delay,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_inner(
        support_directory: PathBuf,
        network: Network,
        settings: Arc<Settings>,
        events: Arc<Events>,
        mail: Option<Arc<PeerMail>>,
        foreground: watch::Receiver<bool>,
        keepalive: Option<watch::Sender<bool>>,
        background_suspend_delay: Duration,
    ) -> Arc<Self> {
        let (wake_tx, wake_rx) = mpsc::unbounded_channel();
        let this = Arc::new(Self {
            cloud: CloudDrop::new(&support_directory),
            support_directory,
            network,
            settings,
            events,
            open_gate: AsyncMutex::new(()),
            sync: RwLock::new(None),
            bridge: AsyncMutex::new(BridgeState::default()),
            meta: Mutex::new(StatusMeta::default()),
            wake_tx,
            cloud_wake: Notify::new(),
            tasks: Mutex::new(Vec::new()),
            accounts: OnceLock::new(),
            mail,
            drop_key: Mutex::new(None),
            foreground,
            _foreground_keepalive: keepalive,
            background_suspend_delay_ms: std::sync::atomic::AtomicU64::new(background_suspend_delay.as_millis() as u64),
        });
        let bridge = tokio::spawn(this.clone().run_loop(wake_rx));
        let cloud = tokio::spawn(this.clone().cloud_loop());
        *this.tasks.lock().expect("device sync task lock") = vec![bridge, cloud];
        this
    }

    /// Realigns iroh sockets and wakes cloud drop after a platform network transition while in
    /// the foreground (MS-454).
    pub fn network_changed(self: &Arc<Self>) {
        if !*self.foreground.borrow() {
            return;
        }
        let sync = self.clone();
        tokio::spawn(async move {
            if let Some(peer) = sync.sync.read().await.as_ref() {
                peer.realign().await;
            }
            sync.cloud_wake.notify_one();
        });
    }

    #[cfg(test)]
    pub(crate) async fn is_endpoint_open(&self) -> bool {
        self.sync.read().await.is_some()
    }

    fn background_suspend_delay(&self) -> Duration {
        Duration::from_millis(self.background_suspend_delay_ms.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn set_background_suspend_delay(&self, delay: Duration) {
        self.background_suspend_delay_ms
            .store(delay.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Wakes the bridge after `settings.json` changed.
    pub fn settings_changed(&self) {
        let _ = self.wake_tx.send(());
    }

    /// Bridges `host`'s accounts and sign-ins from now on.
    pub(crate) fn attach_accounts(&self, host: Weak<dyn AccountHost>) {
        if self.accounts.set(host).is_ok() {
            let _ = self.wake_tx.send(());
        }
    }

    /// Wakes the bridge after an account was added, renamed or removed here.
    pub fn accounts_changed(&self) {
        let _ = self.wake_tx.send(());
    }

    /// Wakes the bridge after local push registrations changed so peers reuse them (MS-444).
    pub fn push_changed(&self) {
        let _ = self.wake_tx.send(());
    }

    /// Whether a local `accounts.remove` should also stop/delete the provider's push watch or
    /// subscription (MS-444): false while paused in a sync group, because a paused local sign-out
    /// leaves the account active on other devices sharing that watch/subscription.
    pub(crate) async fn should_unregister_push(&self) -> bool {
        if let Some(sync) = self.sync.read().await.as_ref() {
            let status = sync.status().await;
            return status.namespace.is_none() || !status.paused;
        }
        match PeerSync::stored(&self.support_directory) {
            Ok(stored) => stored.namespace.is_none() || !stored.paused,
            Err(_) => true,
        }
    }

    /// Records that `account` signed in here, so its new credential reaches the group's devices.
    pub(crate) fn account_signed_in(&self, account: &str) {
        if let Err(error) = account_sync::signed_in(&self.support_directory, account, now_ms()) {
            self.meta.lock().expect("meta lock").problem = Some(error.message);
        }
        let _ = self.wake_tx.send(());
    }

    /// How many pairing codes this device issued are still redeemable; none while sync is closed.
    pub(crate) async fn open_invites(&self) -> usize {
        self.sync.read().await.as_ref().map_or(0, PeerSync::open_invites)
    }

    /// Whether every other device that gets the accounts has each of this device's signed-in
    /// accounts sealed to it, so this device may go offline after pairing.
    pub(crate) async fn accounts_handed_over(&self) -> bool {
        let Some(host) = self.accounts.get().and_then(Weak::upgrade) else { return true };
        let guard = self.sync.read().await;
        let Some(sync) = guard.as_ref() else { return false };
        let me = sync.status().await.endpoint_id;
        let Ok(snapshot) = sync.snapshot().await else { return false };
        account_sync::handed_over(&snapshot, &me, &*host)
    }

    /// Whether this device has every account the group holds, each with a saved sign-in.
    pub(crate) async fn accounts_received(&self) -> bool {
        let Some(host) = self.accounts.get().and_then(Weak::upgrade) else { return true };
        let guard = self.sync.read().await;
        let Some(sync) = guard.as_ref() else { return false };
        let Ok(snapshot) = sync.snapshot().await else { return false };
        account_sync::received(&snapshot, &*host)
    }

    /// Stops the background bridge and cleanly closes `PeerSync` (releasing `peer-sync/lock`).
    pub async fn shutdown(&self) {
        for task in self.tasks.lock().expect("device sync task lock").drain(..) {
            task.abort();
        }
        let sync = self.sync.write().await.take();
        if let Some(sync) = sync {
            sync.shutdown().await;
        }
    }

    async fn run_loop(self: Arc<Self>, mut wake_rx: mpsc::UnboundedReceiver<()>) {
        let mut fg = self.foreground.clone();
        let mut changes: Option<watch::Receiver<u64>> = None;
        let mut suspend_at: Option<tokio::time::Instant> = None;
        loop {
            if changes.is_some() && self.sync.read().await.is_none() {
                changes = None;
                suspend_at = None;
            }
            if changes.is_none() {
                if !*fg.borrow_and_update() && self.sync.read().await.is_none() {
                    tokio::select! {
                        res = fg.changed() => {
                            if res.is_err() {
                                return;
                            }
                        }
                        msg = wake_rx.recv() => {
                            if msg.is_none() {
                                return;
                            }
                            while wake_rx.try_recv().is_ok() {}
                        }
                    }
                    continue;
                }
                // Stays closed while this device syncs no group; the command that opens sync
                // wakes this, and so does `IDLE_CHECK` once another process joined a group.
                let delay = if self.sync.read().await.is_some() || self.syncing_on_disk() {
                    match self.ensure_open().await {
                        Ok(rx) => {
                            changes = rx;
                            suspend_at = (!*fg.borrow()).then(|| tokio::time::Instant::now() + self.background_suspend_delay());
                            let _ = self.reconcile_and_report().await;
                            self.events.emit("deviceSync");
                            self.cloud_wake.notify_one();
                            continue;
                        }
                        Err(PeerSyncError::Busy) => RETRY_BUSY,
                        Err(_) => RETRY_FAILED,
                    }
                } else {
                    IDLE_CHECK
                };
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    msg = wake_rx.recv() => {
                        if msg.is_none() {
                            return;
                        }
                        while wake_rx.try_recv().is_ok() {}
                    }
                    res = fg.changed() => {
                        if res.is_err() {
                            return;
                        }
                    }
                }
                continue;
            }
            if !*fg.borrow() && suspend_at.is_none() {
                suspend_at = Some(tokio::time::Instant::now() + self.background_suspend_delay());
            }
            let rx = changes.as_mut().expect("opened above");
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        changes = None;
                        suspend_at = None;
                        continue;
                    }
                    self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                    let _ = self.reconcile_and_report().await;
                    self.events.emit("deviceSync");
                    self.cloud_wake.notify_one();
                }
                msg = wake_rx.recv() => {
                    if msg.is_none() {
                        return;
                    }
                    while wake_rx.try_recv().is_ok() {}
                    if self.reconcile_and_report().await {
                        self.events.emit("deviceSync");
                    }
                    self.cloud_wake.notify_one();
                }
                () = async {
                    match suspend_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if !*fg.borrow() {
                        if self.open_invites().await == 0 {
                            self.suspend_for_background().await;
                            changes = None;
                            suspend_at = None;
                        } else {
                            suspend_at = Some(tokio::time::Instant::now() + self.background_suspend_delay());
                        }
                    } else {
                        suspend_at = None;
                    }
                }
                res = fg.changed() => {
                    if res.is_err() {
                        return;
                    }
                    if *fg.borrow_and_update() {
                        suspend_at = None;
                        if let Some(peer) = self.sync.read().await.as_ref() {
                            peer.realign().await;
                        }
                        self.cloud_wake.notify_one();
                    } else {
                        suspend_at = Some(tokio::time::Instant::now() + self.background_suspend_delay());
                    }
                }
            }
        }
    }

    /// Exchanges with the cloud drop when asked (after opening and after every local or synced
    /// change), every [`cloud_drop::POLL`] ([`cloud_drop::RETRY`] after a failure), and after a
    /// network change or a wake. Pauses while in the background (MS-454).
    async fn cloud_loop(self: Arc<Self>) {
        let mut fg = self.foreground.clone();
        let mut was_background = !*fg.borrow_and_update();
        let mut network = ProcessSignal::NETWORK_CHANGE.posts();
        let mut delay = cloud_drop::POLL;
        loop {
            if was_background {
                if !crate::polling::wait_foreground(&mut fg).await {
                    return;
                }
                was_background = false;
                delay = if self.cloud_exchange().await { cloud_drop::POLL } else { cloud_drop::RETRY };
                continue;
            }
            let (network_closed, backgrounded) = tokio::select! {
                _ = self.cloud_wake.notified() => (false, false),
                _ = tokio::time::sleep(delay) => (false, false),
                _ = next_wake(WAKE_CHECK) => (false, false),
                posted = async {
                    match network.as_mut() {
                        Some(posts) => posts.next().await,
                        None => std::future::pending().await,
                    }
                } => (!posted, false),
                res = fg.changed() => {
                    if res.is_err() {
                        return;
                    }
                    (false, !*fg.borrow_and_update())
                }
            };
            if network_closed {
                network = None;
            }
            if backgrounded {
                was_background = true;
                continue;
            }
            delay = if self.cloud_exchange().await { cloud_drop::POLL } else { cloud_drop::RETRY };
        }
    }

    /// One exchange with the cloud drop, bridging what it brought into `settings.json`; false
    /// when it failed.
    async fn cloud_exchange(&self) -> bool {
        let namespace = {
            let guard = self.sync.read().await;
            let Some(sync) = guard.as_ref() else { return true };
            sync.status().await.namespace
        };
        let drop_key = || {
            let held = self.drop_key.lock().expect("drop key lock").clone();
            held.filter(|(group, _)| Some(group) == namespace.as_ref()).map(|(_, drop_key)| drop_key)
        };
        // A reconcile of this group tells which key the drop takes (MS-396).
        if drop_key().is_none() {
            self.reconcile_and_report().await;
        }
        let exchange = {
            let guard = self.sync.read().await;
            let Some(sync) = guard.as_ref() else { return true };
            // While the key of the latest removal is on its way, the drop waits: never where a
            // removed device reads.
            let drop_key = match drop_key() {
                Some(DropKey::Original) => None,
                Some(DropKey::Rotated(key)) => Some(key),
                Some(DropKey::Waiting) | None => return true,
            };
            self.cloud.exchange(sync, drop_key.as_ref()).await
        };
        let mut notify = exchange.changed;
        if exchange.merged {
            self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
            notify |= self.reconcile_and_report().await;
        }
        if notify {
            self.events.emit("deviceSync");
        }
        self.cloud.status(namespace.as_deref()).problem.is_none()
    }

    /// Whether `peer-sync/` names a group this device syncs; a damaged `sync.json` counts, so
    /// opening reports it.
    fn syncing_on_disk(&self) -> bool {
        PeerSync::stored(&self.support_directory).map_or(true, |stored| stored.syncing())
    }

    /// What `peer-sync/` holds while sync is closed; `None` while open.
    async fn closed_stored(&self) -> Result<Option<Stored>, EngineError> {
        if self.sync.read().await.is_some() {
            return Ok(None);
        }
        PeerSync::stored(&self.support_directory).map(Some).map_err(map_error)
    }

    async fn ensure_open(&self) -> Result<Option<watch::Receiver<u64>>, PeerSyncError> {
        let _gate = self.open_gate.lock().await;
        if let Some(existing) = self.sync.read().await.as_ref() {
            return Ok(Some(existing.changes()));
        }
        let services: Vec<Arc<dyn Service>> = self.mail.iter().map(|mail| mail.clone() as Arc<dyn Service>).collect();
        match PeerSync::open_with_foreground(&self.support_directory, self.network.clone(), services, Some(self.foreground.clone())).await {
            Ok(opened) => {
                let rx = opened.changes();
                *self.sync.write().await = Some(opened);
                self.meta.lock().expect("meta lock").problem = None;
                // Opened for a command: the bridge follows it from now on.
                let _ = self.wake_tx.send(());
                Ok(Some(rx))
            }
            Err(error) => {
                self.meta.lock().expect("meta lock").problem = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub(crate) async fn open_or_err(&self) -> Result<(), EngineError> {
        if self.sync.read().await.is_some() {
            return Ok(());
        }
        self.ensure_open().await.map(|_| ()).map_err(map_error)
    }

    /// Closes sync unless this device syncs a group, so a paused device holds no endpoint and
    /// reaches no relay (MS-370).
    async fn close_unless_syncing(&self) {
        let _gate = self.open_gate.lock().await;
        let mut slot = self.sync.write().await;
        let Some(sync) = slot.as_ref() else { return };
        if sync.status().await.syncing() {
            return;
        }
        let sync = slot.take().expect("checked above");
        drop(slot);
        sync.shutdown().await;
        let _ = self.wake_tx.send(());
    }

    /// Closes the iroh endpoint while the app stays in the background without marking the sync
    /// group paused on disk, so foreground return reopens it cleanly (MS-454).
    async fn suspend_for_background(&self) {
        let _gate = self.open_gate.lock().await;
        if *self.foreground.borrow() {
            return;
        }
        let mut slot = self.sync.write().await;
        let Some(sync) = slot.as_ref() else { return };
        if sync.open_invites() > 0 {
            return;
        }
        let sync = slot.take().expect("checked above");
        drop(slot);
        sync.shutdown().await;
    }

    async fn reconcile_and_report(&self) -> bool {
        let before = self.meta.lock().expect("meta lock").problem.clone();
        let cloud_changed = match self.reconcile().await {
            Ok(changed) => changed,
            Err(error) => {
                self.meta.lock().expect("meta lock").problem = Some(error.message);
                false
            }
        };
        cloud_changed || self.meta.lock().expect("meta lock").problem != before
    }

    async fn reconcile(&self) -> Result<bool, EngineError> {
        let mut bridge = self.bridge.lock().await;
        let sync_guard = self.sync.read().await;
        let Some(sync) = sync_guard.as_ref() else {
            return Ok(false);
        };
        let status = sync.status().await;
        let Some(namespace) = status.namespace else {
            return Ok(false);
        };

        let persisted = bridge.load(&self.support_directory, &namespace).clone();
        let joining = persisted.joining;
        let mut snapshot = sync.snapshot().await.map_err(map_error)?;

        let Some(invalid) = self.reconcile_settings(sync, &mut bridge, &mut snapshot, joining).await? else {
            return Ok(false);
        };
        let accounts_problem = self.reconcile_accounts(sync, &mut bridge, &status.endpoint_id, &namespace, &snapshot, joining).await;
        let cloud_changed = self.reconcile_cloud(sync, &mut bridge, &namespace, &mut snapshot, joining).await?;

        // Invalid keys or still-downloading remote entries sat out: keep joining until they settle.
        bridge.bridged.joining = joining && (!invalid.is_empty() || !snapshot.pending.is_empty());
        if bridge.bridged != persisted {
            bridge.bridged.write(&self.support_directory)?;
        }
        self.meta.lock().expect("meta lock").problem = invalid_problem(&invalid).or(accounts_problem);
        Ok(cloud_changed)
    }

    /// Reconciles `settings.json` (scalar settings + trusted senders) against `snapshot`, retrying
    /// if the file changes mid-pass. Returns `Ok(None)` when `settings.json` is invalid JSON or
    /// continuously raced (in which case another reconcile pass is queued).
    async fn reconcile_settings(
        &self,
        sync: &PeerSync,
        bridge: &mut BridgeState,
        snapshot: &mut Snapshot,
        joining: bool,
    ) -> Result<Option<BTreeMap<Setting, String>>, EngineError> {
        for _attempt in 0..3 {
            // Skip if settings.json is broken right now so we never push defaults over the group.
            let Ok(raw_before) = self.settings.file().read_raw() else {
                return Ok(None);
            };
            let file_mtime = self.settings.file().modified_ms();
            // Validate synced keys from `raw_before` itself: `SettingsFile::read` replaces an
            // invalid key with its default, which bridged as a local edit would reset every
            // paired device (MS-368), and reading the file a second time could race an edit.
            let (local_values, invalid) = synced_values(&raw_before);
            let trusted_valid = !invalid.contains_key(&Setting::TrustedSenders);
            let mut pushed = false;

            for setting in SCALAR_SETTINGS {
                let key = setting.key();
                if invalid.contains_key(&setting) || snapshot.pending.contains(&format!("v1/setting/{key}")) {
                    continue;
                }
                let local = setting.value(&local_values);
                let base = bridge.bridged.settings.get(key);
                let edit_at = pick_edit_at(self.settings.setting_edited_at(setting), base, file_mtime);
                if let Some(at) = causal_write_at(
                    &local,
                    base,
                    snapshot.settings.get(key),
                    edit_at,
                    joining,
                    local != setting.default_value(),
                ) && sync.set_setting(key, local.clone(), at).await.map_err(map_error)?
                {
                    bridge.bridged.settings.insert(key.to_string(), Stamped { value: local, at });
                    bridge.last_at = bridge.last_at.max(at);
                    pushed = true;
                }
            }

            let local_trusted = addresses(&local_values);
            if trusted_valid {
                let mut all_addresses: BTreeSet<String> = local_trusted.clone();
                all_addresses.extend(bridge.bridged.trusted.keys().cloned());
                for address in all_addresses {
                    if snapshot.pending.contains(&format!("v1/trusted/{address}")) {
                        continue;
                    }
                    let in_local = local_trusted.contains(&address);
                    let base = bridge.bridged.trusted.get(&address);
                    let edit_at = pick_edit_at(self.settings.trusted_edited_at(&address), base, file_mtime);
                    if let Some(at) = causal_write_at(
                        &in_local,
                        base,
                        snapshot.trusted.get(&address),
                        edit_at,
                        joining,
                        in_local,
                    ) && sync.set_trusted(&address, in_local, at).await.map_err(map_error)?
                    {
                        bridge.bridged.trusted.insert(address, Stamped { value: in_local, at });
                        bridge.last_at = bridge.last_at.max(at);
                        pushed = true;
                    }
                }
            }

            if pushed {
                bridge.bridged.write(&self.support_directory)?;
                *snapshot = sync.snapshot().await.map_err(map_error)?;
            }

            let mut next_settings = BTreeMap::new();
            let mut pending_updates: Vec<(Setting, Value)> = Vec::new();
            let still_joining = joining && (!invalid.is_empty() || !snapshot.pending.is_empty());

            for setting in SCALAR_SETTINGS {
                let key = setting.key();
                if snapshot.pending.contains(&format!("v1/setting/{key}")) || invalid.contains_key(&setting) {
                    if let Some(prev) = bridge.bridged.settings.get(key) {
                        next_settings.insert(key.to_string(), prev.clone());
                    }
                    continue;
                }
                if let Some(stamped) = snapshot.settings.get(key) {
                    let local = setting.value(&local_values);
                    let value = if stamped.value != local {
                        match setting.validate(&stamped.value) {
                            Ok(valid) => {
                                pending_updates.push((setting, valid.clone()));
                                valid
                            }
                            Err(_) => stamped.value.clone(),
                        }
                    } else {
                        local
                    };
                    next_settings.insert(key.to_string(), Stamped { value, at: stamped.at });
                } else if !still_joining {
                    next_settings.insert(key.to_string(), Stamped { value: setting.default_value(), at: 0 });
                }
            }

            let next_trusted = if trusted_valid {
                let mut next = BTreeMap::new();
                let mut desired = local_trusted.clone();
                for (address, stamped) in &snapshot.trusted {
                    if snapshot.pending.contains(&format!("v1/trusted/{address}")) || !is_valid_email(address) {
                        continue;
                    }
                    if stamped.value {
                        desired.insert(address.clone());
                    } else {
                        desired.remove(address);
                    }
                    next.insert(address.clone(), stamped.clone());
                }
                for address in &local_trusted {
                    if snapshot.pending.contains(&format!("v1/trusted/{address}"))
                        && let Some(prev) = bridge.bridged.trusted.get(address)
                    {
                        next.insert(address.clone(), prev.clone());
                    }
                }
                if desired != local_trusted {
                    let list: Vec<String> = desired.into_iter().collect();
                    pending_updates.push((Setting::TrustedSenders, json!(list)));
                }
                next
            } else {
                bridge.bridged.trusted.clone()
            };

            // Atomically apply remote settings only if settings.json wasn't edited during this pass.
            if !self.settings.file().update_batch_if_unchanged(&raw_before, &pending_updates)? {
                continue;
            }
            bridge.bridged.settings = next_settings;
            bridge.bridged.trusted = next_trusted;
            if !pending_updates.is_empty() && self.settings.reload() {
                self.events.emit("settings");
            }
            return Ok(Some(invalid));
        }
        let _ = self.wake_tx.send(());
        Ok(None)
    }

    async fn reconcile_accounts(
        &self,
        sync: &PeerSync,
        bridge: &mut BridgeState,
        me: &str,
        namespace: &str,
        snapshot: &Snapshot,
        joining: bool,
    ) -> Option<String> {
        let mut problem = None;
        let mut drop_key = DropKey::without_key(snapshot);
        if let Some(host) = self.accounts.get().and_then(Weak::upgrade) {
            let (bridged_accounts, last_at) = (&bridge.bridged.accounts, &mut bridge.last_at);
            let pass = account_sync::Pass {
                sync,
                host: &*host,
                support_directory: &self.support_directory,
                me,
                bridged: bridged_accounts,
                local_modified_ms: files::modified_ms(&self.support_directory.join("accounts.json")),
                joining,
                join_at: JOIN_AT,
            };
            let result = account_sync::reconcile(pass, &mut |min_at| {
                let next = now_ms().max(*last_at + 1).max(min_at + 1);
                *last_at = next;
                next
            })
            .await;
            match result {
                Ok(done) => {
                    bridge.bridged.accounts = done.bridged;
                    drop_key = done.drop_key;
                }
                Err(error) => problem = Some(format!("Accounts: {}", error.message)),
            }
        }
        *self.drop_key.lock().expect("drop key lock") = Some((namespace.to_string(), drop_key));
        problem
    }

    /// Mirrors `v1/cloud` locally, migrating a pre-MS-419 `cloud.json` once (never while joining).
    async fn reconcile_cloud(
        &self,
        sync: &PeerSync,
        bridge: &mut BridgeState,
        namespace: &str,
        snapshot: &mut Snapshot,
        joining: bool,
    ) -> Result<bool, EngineError> {
        let mut cloud_changed = false;
        if !snapshot.pending.contains("v1/cloud") {
            if !joining
                && snapshot.cloud.is_none()
                && let Some(url) = self.cloud.legacy_enabled_url()
            {
                let config = CloudConfig { enabled: true, url: Some(url), at: bridge.next_at(0) };
                if sync.set_cloud(&config).await.map_err(map_error)? {
                    snapshot.cloud = Some(config);
                }
            }
            cloud_changed = self.cloud.apply_group(namespace, snapshot.cloud.as_ref())?;
        }
        if cloud_changed || self.cloud.has_pending_cleanup(namespace) {
            self.cloud_wake.notify_one();
        }
        Ok(cloud_changed)
    }

    pub(crate) async fn run(&self, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "sync.status" => {
                if self.sync.read().await.is_none() && self.syncing_on_disk() {
                    let _ = self.ensure_open().await;
                }
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.enable" => {
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    let status = sync.status().await;
                    if status.namespace.is_none() {
                        sync.create_group().await.map_err(map_error)?;
                    } else if status.paused {
                        sync.resume().await.map_err(map_error)?;
                    }
                }
                self.reconcile().await?;
                self.cloud_exchange().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.disable" => {
                // Nothing to pause without a group, and nothing to open for one already paused.
                if self.closed_stored().await?.is_none_or(|stored| stored.syncing()) {
                    self.open_or_err().await?;
                    {
                        let guard = self.sync.read().await;
                        let sync = guard.as_ref().expect("opened");
                        sync.pause().await.map_err(map_error)?;
                    }
                    self.close_unless_syncing().await;
                }
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.pair_ticket" => {
                if self.closed_stored().await?.is_some_and(|stored| stored.namespace.is_none()) {
                    return Err(map_error(PeerSyncError::NoGroup));
                }
                self.open_or_err().await?;
                let (ticket, expires) = {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.invite().await.map_err(map_error)?
                };
                Ok(answers::json(&SyncTicket { ticket, expires_at: iso_ms(expires) }))
            }
            "sync.join" => {
                let ticket = args.required("ticket")?;
                self.open_or_err().await?;
                {
                    // Held until the bridge knows it joined, so no reconcile meanwhile pushes this
                    // device's settings as newer than the group's (MS-367).
                    let mut bridge = self.bridge.lock().await;
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.join(&ticket).await.map_err(map_error)?;
                    if let Some(namespace) = sync.status().await.namespace {
                        bridge.joined(&self.support_directory, &namespace)?;
                    }
                }
                self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                self.reconcile().await?;
                self.cloud_exchange().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.reset" => {
                if !args.bool("yes") {
                    return Err(EngineError::new(
                        "confirm",
                        "Would create a new sync group and drop older devices until paired again (local settings stay). Re-run with --yes.",
                    ));
                }
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.create_group().await.map_err(map_error)?;
                }
                self.reconcile().await?;
                self.cloud_exchange().await;
                self.close_unless_syncing().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.remove" => {
                let device = args.required("device")?;
                if self.closed_stored().await?.is_some_and(|stored| stored.namespace.is_none()) {
                    return Err(map_error(PeerSyncError::NoGroup));
                }
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.remove_device(&device).await.map_err(map_error)?;
                }
                // Withdraws the credentials sealed to it, and moves the cloud drop.
                self.reconcile().await?;
                self.cloud_exchange().await;
                self.close_unless_syncing().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.now" => {
                if let Some(stored) = self.closed_stored().await? {
                    if stored.namespace.is_none() {
                        return Err(map_error(PeerSyncError::NoGroup));
                    }
                    if stored.paused {
                        return Err(paused_error());
                    }
                }
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    let status = sync.status().await;
                    if status.namespace.is_none() {
                        return Err(map_error(PeerSyncError::NoGroup));
                    }
                    if status.paused {
                        return Err(paused_error());
                    }
                    sync.realign().await;
                }
                self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                self.reconcile().await?;
                self.cloud_exchange().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.cloud_enable" => {
                let explicit_url = args.string("url").filter(|url| !url.trim().is_empty());
                let validated_explicit = explicit_url.as_deref().map(cloud_drop::validate_url).transpose()?;
                if self.closed_stored().await?.is_some_and(|stored| stored.namespace.is_none()) {
                    return Err(map_error(PeerSyncError::NoGroup));
                }
                self.open_or_err().await?;
                {
                    let mut bridge = self.bridge.lock().await;
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    let Some(namespace) = sync.status().await.namespace else {
                        return Err(map_error(PeerSyncError::NoGroup));
                    };
                    let snapshot = sync.snapshot().await.map_err(map_error)?;
                    let existing_url = snapshot
                        .cloud
                        .as_ref()
                        .and_then(|c| c.url.as_deref())
                        .and_then(|u| cloud_drop::validate_url(u).ok());
                    let url = validated_explicit
                        .or(existing_url)
                        .unwrap_or_else(|| cloud_drop::DEFAULT_WORKER_URL.to_owned());
                    let min_at = snapshot.cloud.as_ref().map_or(0, |c| c.at);
                    let at = bridge.next_at(min_at);
                    let config = CloudConfig { enabled: true, url: Some(url.clone()), at };
                    sync.set_cloud(&config).await.map_err(map_error)?;
                    self.cloud.enable_for_group(&namespace, url)?;
                }
                self.cloud_exchange().await;
                self.close_unless_syncing().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.cloud_disable" => {
                if self.closed_stored().await?.is_some_and(|stored| stored.namespace.is_none()) {
                    return Err(map_error(PeerSyncError::NoGroup));
                }
                self.open_or_err().await?;
                let namespace = {
                    let mut bridge = self.bridge.lock().await;
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    let Some(namespace) = sync.status().await.namespace else {
                        return Err(map_error(PeerSyncError::NoGroup));
                    };
                    let snapshot = sync.snapshot().await.map_err(map_error)?;
                    let url = snapshot.cloud.as_ref().and_then(|c| c.url.clone());
                    let min_at = snapshot.cloud.as_ref().map_or(0, |c| c.at);
                    let at = bridge.next_at(min_at);
                    let config = CloudConfig { enabled: false, url: url.clone(), at };
                    sync.set_cloud(&config).await.map_err(map_error)?;
                    self.cloud.mark_disabled_for_group(&namespace, url)?;
                    namespace
                };
                self.cloud.cleanup_disabled_upload(&namespace).await;
                self.close_unless_syncing().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            _ => unreachable!("{command} is a sync command"),
        }
    }

    async fn status_answer(&self) -> DeviceSyncStatus {
        let mut answer = self.group_status().await;
        if let Some(mail) = &self.mail {
            answer.mail = mail.received().await;
        }
        answer
    }

    async fn group_status(&self) -> DeviceSyncStatus {
        // The gate keeps an open in progress, which holds `peer-sync/lock`, from reading as
        // another process's.
        let gate = self.open_gate.lock().await;
        let guard = self.sync.read().await;
        let Some(sync) = guard.as_ref() else {
            return self.closed_status();
        };
        drop(gate);
        let status = sync.status().await;
        let snapshot = if status.namespace.is_some() { sync.snapshot().await.ok() } else { None };
        let mut devices = Vec::new();
        let mut latest_other_seen = None;
        let mut removed_here = None;
        if let Some(snapshot) = snapshot {
            removed_here = snapshot.removed.get(&status.endpoint_id).map(|removal| {
                let by = snapshot.devices.get(&removal.by).map_or(removal.by.as_str(), |device| device.name.as_str());
                format!(
                    "{by} removed this device from the sync group; pair it again with a code from a device in the group to rejoin."
                )
            });
            for (id, device) in snapshot.devices.iter().filter(|(id, _)| !snapshot.removed.contains_key(*id)) {
                if *id != status.endpoint_id {
                    latest_other_seen = Some(latest_other_seen.unwrap_or(0).max(device.last_seen));
                }
                devices.push(SyncDevice {
                    accounts: account_sync::receives(&snapshot, id),
                    id: id.clone(),
                    name: device.name.clone(),
                    platform: device.platform.clone(),
                    last_seen: iso_ms(device.last_seen),
                });
            }
        }
        let meta = self.meta.lock().expect("meta lock");
        let last_sync = meta.last_sync_ms.or(latest_other_seen).map(iso_ms);
        let cloud = self.cloud.status(status.namespace.as_deref());
        DeviceSyncStatus {
            available: true,
            enabled: status.namespace.is_some() && !status.paused,
            paused: status.paused,
            endpoint_id: Some(status.endpoint_id),
            namespace: status.namespace,
            devices,
            last_sync,
            problem: removed_here.or_else(|| meta.problem.clone()),
            cloud,
            mail: Default::default(),
        }
    }

    /// `sync.status` while sync is closed, from `peer-sync/` without opening it: available
    /// unless another process has it open, or this device syncs a group that failed to open.
    fn closed_status(&self) -> DeviceSyncStatus {
        let mut answer = DeviceSyncStatus {
            available: false,
            enabled: false,
            paused: false,
            endpoint_id: None,
            namespace: None,
            devices: Vec::new(),
            last_sync: None,
            problem: None,
            cloud: Default::default(),
            mail: Default::default(),
        };
        match PeerSync::stored(&self.support_directory) {
            Err(error) => answer.problem = Some(error.to_string()),
            Ok(stored) => {
                if PeerSync::busy(&self.support_directory) {
                    answer.problem = Some(PeerSyncError::Busy.to_string());
                } else if stored.syncing() {
                    answer.problem = self.meta.lock().expect("meta lock").problem.clone();
                } else {
                    answer.available = true;
                }
                answer.paused = stored.paused;
                answer.endpoint_id = stored.endpoint_id;
                answer.namespace = stored.namespace;
            }
        }
        answer.cloud = self.cloud.status(answer.namespace.as_deref());
        answer
    }
}

fn addresses(values: &Values) -> BTreeSet<String> {
    Setting::TrustedSenders
        .value(values)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|addr| addr.trim().to_lowercase())
        .collect()
}

/// Splits `raw`'s synced keys into valid values and those that fail validation (with why).
/// `SettingsFile::read` gives invalid keys their default, which bridged as a local change would
/// reset them on every device (one mistyped address untrusting every sender, MS-368), and a
/// remote value written over them would drop what the user is still typing; so device sync
/// leaves them alone, both ways, until the file is fixed.
fn synced_values(raw: &Map<String, Value>) -> (Values, BTreeMap<Setting, String>) {
    let mut values = Values::new();
    let mut invalid = BTreeMap::new();
    for setting in SCALAR_SETTINGS.into_iter().chain([Setting::TrustedSenders]) {
        let Some(value) = raw.get(setting.key()) else {
            continue;
        };
        match setting.validate(value) {
            Ok(valid) => {
                values.insert(setting, valid);
            }
            Err(error) => {
                invalid.insert(setting, error.message);
            }
        }
    }
    (values, invalid)
}

/// What `sync.status` reports while `invalid_synced` keeps settings out of device sync.
fn invalid_problem(invalid: &BTreeMap<Setting, String>) -> Option<String> {
    if invalid.is_empty() {
        return None;
    }
    let messages: Vec<&str> = invalid.values().map(String::as_str).collect();
    Some(format!("Device sync leaves these settings alone until settings.json is fixed: {}", messages.join(" ")))
}

fn is_valid_email(address: &str) -> bool {
    let trimmed = address.trim();
    !trimmed.is_empty() && trimmed.contains('@') && !trimmed.contains(char::is_whitespace)
}

fn paused_error() -> EngineError {
    EngineError::new("paused", "Device sync is paused; enable it first (`mailstream sync enable`).")
}

fn map_error(error: PeerSyncError) -> EngineError {
    match error {
        PeerSyncError::Busy => EngineError::new("busy", error.to_string()),
        PeerSyncError::NoGroup => EngineError::new("no_group", error.to_string()),
        PeerSyncError::Failed(message) => EngineError::failed(message),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Best known edit time (Unix ms) for a local `settings.json` change: the in-process `config.*`
/// timestamp if newer than the last synced `base.at`, else `settings.json`'s mtime.
fn pick_edit_at<T>(in_process: Option<u64>, base: Option<&Stamped<T>>, file_mtime: Option<u64>) -> u64 {
    let floor = base.map_or(0, |b| b.at);
    in_process.filter(|&at| at > floor).or(file_mtime).unwrap_or_else(now_ms)
}

fn iso_ms(ms: u64) -> String {
    model::iso8601(model::date_from_unix(ms as f64 / 1000.0))
}

/// Timestamp to publish a local change under, or `None` when nothing should be pushed.
///
/// Single invariant across settings, trusted senders, and accounts:
/// - A local edit carries its actual time `edit_at`, floored only at `base.at + 1` (the causal
///   timestamp this device had already observed before editing), NEVER bumped above an unseen
///   concurrent `remote.at`. Thus `PeerSync`'s LWW rule always respects whichever edit genuinely
///   happened latest (online, offline, or via CloudDrop), while causal edits after an already
///   observed clock-skewed remote value still supersede it.
/// - Joining a group stamps pre-existing local values with [`JOIN_AT`], so the group always wins
///   join-time conflicts (MS-367).
pub(crate) fn causal_write_at<T: PartialEq>(
    local: &T,
    base: Option<&Stamped<T>>,
    remote: Option<&Stamped<T>>,
    edit_at: u64,
    joining: bool,
    push_when_unbridged: bool,
) -> Option<u64> {
    match base {
        Some(prev) if prev.value == *local => None,
        Some(prev) => {
            let floor = if prev.at > 0 {
                prev.at
            } else {
                remote.filter(|r| r.value == prev.value).map_or(0, |r| r.at)
            };
            Some(edit_at.max(floor + 1))
        }
        None if !push_when_unbridged => None,
        None if joining => remote.is_none().then_some(JOIN_AT),
        None => Some(edit_at.max(1)),
    }
}
