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

use crate::account_sync::{self, AccountHost};
use crate::answers::{self, DeviceSyncStatus, SyncDevice, SyncTicket};
use crate::cloud_drop::{self, CloudDrop};
use crate::commands::Arguments;
use crate::peer_sync::{Network, PeerSync, PeerSyncError, Stored};
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

/// Scalar whitelisted settings synced under `v1/setting/<key>`.
/// `remoteContent.trustedSenders` is synced per address under `v1/trusted/<email>`.
/// Machine-local keys (`aiTagging.backend`/`model`/`ompPath`/`batchSize`/`passLimit`/
/// `backoffSeconds`, `sync.*`, `chat.pollSeconds`, `gpui.*`, `flutter.*`) stay local.
const SCALAR_SETTINGS: [Setting; 4] = [
    Setting::AutoReadEnabled,
    Setting::AutoReadDays,
    Setting::AiTaggingEnabled,
    Setting::AiTaggingLocalOnlyAccounts,
];

/// Last reconciled group state, persisted in `peer-sync/bridged.json` so offline edits to
/// `settings.json` (e.g. via `mailstream config`) are detected on the next start.
///
/// Joining a group merges this device's settings into it (MS-367): what only the group holds
/// comes here, what only this device holds goes to the group, trusted senders end up the union of
/// both, and where both set a key differently (an address the group untrusted included) the
/// group's value wins, so joining never changes the other devices' settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bridged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default)]
    trusted: BTreeMap<String, bool>,
    #[serde(default)]
    settings: BTreeMap<String, Value>,
    /// Account records by id as last reconciled (accounts.json's JSON); `None`: removed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    accounts: BTreeMap<String, Option<Value>>,
    /// Set by `sync.join` until the first reconcile with the joined group completes: pushes of
    /// keys without a bridged value are then stamped [`JOIN_AT`], not now. `join` returns
    /// before the group's entries have arrived, so "the group doesn't hold it" is unknown there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    joining: bool,
}

impl Bridged {
    fn path(support_directory: &Path) -> PathBuf {
        support_directory.join("peer-sync").join(BRIDGE_FILE)
    }

    fn read(support_directory: &Path) -> Self {
        match std::fs::read(Self::path(support_directory)) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn write(&self, support_directory: &Path) -> Result<(), EngineError> {
        let dir = support_directory.join("peer-sync");
        let data = serde_json::to_vec_pretty(self).map_err(|error| EngineError::failed(error.to_string()))?;
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
}

impl DeviceSync {
    /// Starts opening `PeerSync` and bridging `settings.json` in the background on the current
    /// tokio runtime. Never blocks `Engine::start`.
    pub fn start(
        support_directory: PathBuf,
        network: Network,
        settings: Arc<Settings>,
        events: Arc<Events>,
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
        });
        let bridge = tokio::spawn(this.clone().run_loop(wake_rx));
        let cloud = tokio::spawn(this.clone().cloud_loop());
        *this.tasks.lock().expect("device sync task lock") = vec![bridge, cloud];
        this
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
        let mut changes: Option<watch::Receiver<u64>> = None;
        loop {
            if changes.is_some() && self.sync.read().await.is_none() {
                changes = None;
            }
            if changes.is_none() {
                // Stays closed while this device syncs no group; the command that opens sync
                // wakes this, and so does `IDLE_CHECK` once another process joined a group.
                let delay = if self.sync.read().await.is_some() || self.syncing_on_disk() {
                    match self.ensure_open().await {
                        Ok(rx) => {
                            changes = rx;
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
                }
                continue;
            }
            let rx = changes.as_mut().expect("opened above");
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        changes = None;
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
            }
        }
    }

    /// Exchanges with the cloud drop when asked (after opening and after every local or synced
    /// change), every [`cloud_drop::POLL`] ([`cloud_drop::RETRY`] after a failure), and after a
    /// network change or a wake.
    async fn cloud_loop(self: Arc<Self>) {
        let mut network = ProcessSignal::NETWORK_CHANGE.posts();
        let mut delay = cloud_drop::POLL;
        loop {
            let network_closed = tokio::select! {
                _ = self.cloud_wake.notified() => false,
                _ = tokio::time::sleep(delay) => false,
                _ = next_wake(WAKE_CHECK) => false,
                posted = async {
                    match network.as_mut() {
                        Some(posts) => posts.next().await,
                        None => std::future::pending().await,
                    }
                } => !posted,
            };
            if network_closed {
                network = None;
            }
            delay = if self.cloud_exchange().await { cloud_drop::POLL } else { cloud_drop::RETRY };
        }
    }

    /// One exchange with the cloud drop, bridging what it brought into `settings.json`; false
    /// when it failed.
    async fn cloud_exchange(&self) -> bool {
        let exchange = {
            let guard = self.sync.read().await;
            let Some(sync) = guard.as_ref() else { return true };
            self.cloud.exchange(sync).await
        };
        if exchange.merged {
            self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
            self.reconcile_and_report().await;
        }
        if exchange.changed {
            self.events.emit("deviceSync");
        }
        self.cloud.status().problem.is_none()
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
        match PeerSync::open_on(&self.support_directory, self.network.clone()).await {
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

    async fn reconcile_and_report(&self) -> bool {
        let before = self.meta.lock().expect("meta lock").problem.clone();
        if let Err(error) = self.reconcile().await {
            self.meta.lock().expect("meta lock").problem = Some(error.message);
        }
        self.meta.lock().expect("meta lock").problem != before
    }

    async fn reconcile(&self) -> Result<(), EngineError> {
        let mut bridge = self.bridge.lock().await;
        let sync_guard = self.sync.read().await;
        let Some(sync) = sync_guard.as_ref() else {
            return Ok(());
        };
        let status = sync.status().await;
        let Some(namespace) = status.namespace else {
            return Ok(());
        };

        for _attempt in 0..2 {
            // Skip if settings.json is broken right now so we never push defaults over the group.
            let Ok(raw_before) = self.settings.file().read_raw() else {
                return Ok(());
            };
            // Validate synced keys from `raw_before` itself: `SettingsFile::read` replaces an
            // invalid key with its default, which bridged as a local edit would reset every
            // paired device (MS-368), and reading the file a second time could race an edit.
            let (local_values, invalid) = synced_values(&raw_before);
            let trusted_valid = !invalid.contains_key(&Setting::TrustedSenders);
            let prev_bridged = bridge.load(&self.support_directory, &namespace).clone();

            let mut snapshot = sync.snapshot().await.map_err(map_error)?;
            let mut pushed = false;

            for setting in SCALAR_SETTINGS {
                if invalid.contains_key(&setting) {
                    continue;
                }
                let key = setting.key();
                let local = setting.value(&local_values);
                let doc_key = format!("v1/setting/{key}");
                let remote = snapshot.settings.get(key);
                let pending = snapshot.pending.contains(&doc_key);
                let base = prev_bridged.settings.get(key);
                let should_push = match base {
                    Some(prev) => *prev != local,
                    None => local != setting.default_value() && remote.is_none() && !pending,
                };
                if should_push {
                    let at = match base {
                        Some(_) => bridge.next_at(remote.map_or(0, |r| r.at)),
                        None if prev_bridged.joining => JOIN_AT,
                        None => bridge.next_at(0),
                    };
                    sync.set_setting(key, local, at).await.map_err(map_error)?;
                    pushed = true;
                }
            }

            let local_trusted = addresses(&local_values);
            if trusted_valid {
                let mut all_addresses: BTreeSet<String> = local_trusted.clone();
                all_addresses.extend(prev_bridged.trusted.keys().cloned());
                for address in all_addresses {
                    let in_local = local_trusted.contains(&address);
                    let doc_key = format!("v1/trusted/{address}");
                    let remote = snapshot.trusted.get(&address);
                    let pending = snapshot.pending.contains(&doc_key);
                    let base = prev_bridged.trusted.get(&address).copied();
                    let should_push = match base {
                        Some(prev) => in_local != prev,
                        None => in_local && remote.is_none() && !pending,
                    };
                    if should_push {
                        let at = match base {
                            Some(_) => bridge.next_at(remote.map_or(0, |r| r.at)),
                            None if prev_bridged.joining => JOIN_AT,
                            None => bridge.next_at(0),
                        };
                        sync.set_trusted(&address, in_local, at).await.map_err(map_error)?;
                        pushed = true;
                    }
                }
            }

            if pushed {
                snapshot = sync.snapshot().await.map_err(map_error)?;
            }

            // Re-check whether settings.json was edited underneath us while pushing to iroh-docs.
            let Ok(raw_now) = self.settings.file().read_raw() else {
                return Ok(());
            };
            if raw_now != raw_before {
                continue;
            }

            let mut next_bridged = Bridged {
                namespace: Some(namespace.clone()),
                // Invalid keys sat out: they merge as joining once the file is fixed.
                joining: prev_bridged.joining && !invalid.is_empty(),
                ..Default::default()
            };
            let mut pending_updates: Vec<(Setting, Value)> = Vec::new();

            for setting in SCALAR_SETTINGS {
                let key = setting.key();
                let doc_key = format!("v1/setting/{key}");
                // Pending or invalid here: keep the old base, so the change is bridged once it
                // lands or the file is fixed.
                if snapshot.pending.contains(&doc_key) || invalid.contains_key(&setting) {
                    if let Some(prev) = prev_bridged.settings.get(key) {
                        next_bridged.settings.insert(key.to_string(), prev.clone());
                    }
                    continue;
                }
                if let Some(stamped) = snapshot.settings.get(key) {
                    let local = setting.value(&local_values);
                    if stamped.value != local {
                        match setting.validate(&stamped.value) {
                            Ok(valid) => {
                                pending_updates.push((setting, valid.clone()));
                                next_bridged.settings.insert(key.to_string(), valid);
                            }
                            Err(_) => {
                                next_bridged.settings.insert(key.to_string(), stamped.value.clone());
                            }
                        }
                    } else {
                        next_bridged.settings.insert(key.to_string(), local);
                    }
                }
            }

            if trusted_valid {
                let mut desired_trusted = local_trusted.clone();
                for (address, stamped) in &snapshot.trusted {
                    let doc_key = format!("v1/trusted/{address}");
                    if snapshot.pending.contains(&doc_key) {
                        continue;
                    }
                    if !is_valid_email(address) {
                        continue;
                    }
                    if stamped.value {
                        desired_trusted.insert(address.clone());
                    } else {
                        desired_trusted.remove(address);
                    }
                    next_bridged.trusted.insert(address.clone(), stamped.value);
                }
                for address in &local_trusted {
                    let doc_key = format!("v1/trusted/{address}");
                    if snapshot.pending.contains(&doc_key)
                        && let Some(&prev) = prev_bridged.trusted.get(address)
                    {
                        next_bridged.trusted.insert(address.clone(), prev);
                    }
                }
                if desired_trusted != local_trusted {
                    let list: Vec<String> = desired_trusted.into_iter().collect();
                    pending_updates.push((Setting::TrustedSenders, json!(list)));
                }
            } else {
                next_bridged.trusted = prev_bridged.trusted.clone();
            }

            let mut accounts_problem = None;
            next_bridged.accounts = prev_bridged.accounts.clone();
            if let Some(host) = self.accounts.get().and_then(Weak::upgrade) {
                let pass = account_sync::Pass {
                    sync,
                    host: &*host,
                    support_directory: &self.support_directory,
                    me: &status.endpoint_id,
                    bridged: &prev_bridged.accounts,
                    joining: prev_bridged.joining,
                    join_at: JOIN_AT,
                };
                match account_sync::reconcile(pass, &mut |min_at| bridge.next_at(min_at)).await {
                    Ok(done) => next_bridged.accounts = done.bridged,
                    Err(error) => accounts_problem = Some(format!("Accounts: {}", error.message)),
                }
            }

            if !pending_updates.is_empty() {
                for (setting, value) in &pending_updates {
                    self.settings.file().update(*setting, Some(value))?;
                }
            }
            if next_bridged != prev_bridged {
                next_bridged.write(&self.support_directory)?;
                bridge.bridged = next_bridged;
            }
            if !pending_updates.is_empty() && self.settings.reload() {
                self.events.emit("settings");
            }
            self.meta.lock().expect("meta lock").problem = invalid_problem(&invalid).or(accounts_problem);
            return Ok(());
        }
        Ok(())
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
                let url = args
                    .string("url")
                    .filter(|url| !url.trim().is_empty())
                    .unwrap_or_else(|| cloud_drop::DEFAULT_WORKER_URL.to_owned());
                self.cloud.enable(&url)?;
                self.cloud_exchange().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.cloud_disable" => {
                self.cloud.disable().await;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            _ => unreachable!("{command} is a sync command"),
        }
    }

    async fn status_answer(&self) -> DeviceSyncStatus {
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
        if let Some(snapshot) = snapshot {
            for (id, device) in &snapshot.devices {
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
        DeviceSyncStatus {
            available: true,
            enabled: status.namespace.is_some() && !status.paused,
            paused: status.paused,
            endpoint_id: Some(status.endpoint_id),
            namespace: status.namespace,
            devices,
            last_sync,
            problem: meta.problem.clone(),
            cloud: self.cloud.status(),
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
            cloud: self.cloud.status(),
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

fn iso_ms(ms: u64) -> String {
    model::iso8601(model::date_from_unix(ms as f64 / 1000.0))
}
