//! Bridges [`PeerSync`] (`peer-sync/`) to the whitelisted cross-device keys in `settings.json`
//! without feedback loops (MS-360), and answers the `sync.*` commands.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, RwLock, mpsc, watch};
use tokio::task::JoinHandle;

use crate::answers::{self, DeviceSyncStatus, SyncDevice, SyncTicket};
use crate::commands::Arguments;
use crate::peer_sync::{Network, PeerSync, PeerSyncError};
use crate::settings::{Setting, Settings, Values};
use crate::{EngineError, Events, files, model};

const BRIDGE_FILE: &str = "bridged.json";
const RETRY_BUSY: Duration = Duration::from_secs(5);
const RETRY_FAILED: Duration = Duration::from_secs(30);

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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bridged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default)]
    trusted: BTreeMap<String, bool>,
    #[serde(default)]
    settings: BTreeMap<String, Value>,
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
        if !self.loaded {
            self.bridged = Bridged::read(support_directory);
            self.loaded = true;
        }
        if self.bridged.namespace.as_deref() != Some(namespace) {
            self.bridged = Bridged { namespace: Some(namespace.to_string()), ..Default::default() };
        }
        &self.bridged
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

/// Bridges `settings.json` and `PeerSync` for one running `Engine`.
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
    task: Mutex<Option<JoinHandle<()>>>,
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
            support_directory,
            network,
            settings,
            events,
            open_gate: AsyncMutex::new(()),
            sync: RwLock::new(None),
            bridge: AsyncMutex::new(BridgeState::default()),
            meta: Mutex::new(StatusMeta::default()),
            wake_tx,
            task: Mutex::new(None),
        });
        let runner = this.clone();
        let handle = tokio::spawn(async move {
            runner.run_loop(wake_rx).await;
        });
        *this.task.lock().expect("device sync task lock") = Some(handle);
        this
    }

    /// Wakes the bridge after `settings.json` changed.
    pub fn settings_changed(&self) {
        let _ = self.wake_tx.send(());
    }

    /// Stops the background bridge and cleanly closes `PeerSync` (releasing `peer-sync/lock`).
    pub async fn shutdown(&self) {
        if let Some(handle) = self.task.lock().expect("device sync task lock").take() {
            handle.abort();
        }
        let sync = self.sync.write().await.take();
        if let Some(sync) = sync {
            sync.shutdown().await;
        }
    }

    async fn run_loop(self: Arc<Self>, mut wake_rx: mpsc::UnboundedReceiver<()>) {
        let mut changes: Option<watch::Receiver<u64>> = None;
        loop {
            if changes.is_none() {
                match self.ensure_open().await {
                    Ok(rx) => {
                        changes = rx;
                        self.reconcile_and_report().await;
                        self.events.emit("deviceSync");
                    }
                    Err(error) => {
                        let delay = match error {
                            PeerSyncError::Busy => RETRY_BUSY,
                            _ => RETRY_FAILED,
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
                }
            }
            let rx = changes.as_mut().expect("opened above");
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        changes = None;
                        continue;
                    }
                    self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                    self.reconcile_and_report().await;
                    self.events.emit("deviceSync");
                }
                msg = wake_rx.recv() => {
                    if msg.is_none() {
                        return;
                    }
                    while wake_rx.try_recv().is_ok() {}
                    self.reconcile_and_report().await;
                }
            }
        }
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
                Ok(Some(rx))
            }
            Err(error) => {
                self.meta.lock().expect("meta lock").problem = Some(error.to_string());
                Err(error)
            }
        }
    }

    async fn open_or_err(&self) -> Result<(), EngineError> {
        if self.sync.read().await.is_some() {
            return Ok(());
        }
        self.ensure_open().await.map(|_| ()).map_err(map_error)
    }

    async fn reconcile_and_report(&self) {
        if let Err(error) = self.reconcile().await {
            self.meta.lock().expect("meta lock").problem = Some(error.message);
        }
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
            let (local_values, _) = self.settings.file().read();
            let prev_bridged = bridge.load(&self.support_directory, &namespace).clone();

            let mut snapshot = sync.snapshot().await.map_err(map_error)?;
            let mut pushed = false;

            for setting in SCALAR_SETTINGS {
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
                    let min_at = if base.is_some() { remote.map_or(0, |r| r.at) } else { 0 };
                    let at = bridge.next_at(min_at);
                    sync.set_setting(key, local, at).await.map_err(map_error)?;
                    pushed = true;
                }
            }

            let local_trusted = addresses(&local_values);
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
                    let min_at = if base.is_some() { remote.map_or(0, |r| r.at) } else { 0 };
                    let at = bridge.next_at(min_at);
                    sync.set_trusted(&address, in_local, at).await.map_err(map_error)?;
                    pushed = true;
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

            let mut next_bridged = Bridged { namespace: Some(namespace.clone()), ..Default::default() };
            let mut pending_updates: Vec<(Setting, Value)> = Vec::new();

            for setting in SCALAR_SETTINGS {
                let key = setting.key();
                let doc_key = format!("v1/setting/{key}");
                if snapshot.pending.contains(&doc_key) {
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
            self.meta.lock().expect("meta lock").problem = None;
            return Ok(());
        }
        Ok(())
    }

    pub(crate) async fn run(&self, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "sync.status" => {
                if self.sync.read().await.is_none() {
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
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.disable" => {
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.pause().await.map_err(map_error)?;
                }
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.pair_ticket" => {
                self.open_or_err().await?;
                let ticket = {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.ticket().await.map_err(map_error)?
                };
                Ok(answers::json(&SyncTicket { ticket }))
            }
            "sync.join" => {
                let ticket = args.required("ticket")?;
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    sync.join(&ticket).await.map_err(map_error)?;
                }
                self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                self.reconcile().await?;
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
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            "sync.now" => {
                self.open_or_err().await?;
                {
                    let guard = self.sync.read().await;
                    let sync = guard.as_ref().expect("opened");
                    let status = sync.status().await;
                    if status.namespace.is_none() {
                        return Err(map_error(PeerSyncError::NoGroup));
                    }
                    if status.paused {
                        return Err(EngineError::new(
                            "paused",
                            "Device sync is paused; enable it first (`mailstream sync enable`).",
                        ));
                    }
                    sync.realign().await;
                }
                self.meta.lock().expect("meta lock").last_sync_ms = Some(now_ms());
                self.reconcile().await?;
                self.events.emit("deviceSync");
                Ok(answers::json(&self.status_answer().await))
            }
            _ => unreachable!("{command} is a sync command"),
        }
    }

    async fn status_answer(&self) -> DeviceSyncStatus {
        let guard = self.sync.read().await;
        let Some(sync) = guard.as_ref() else {
            let meta = self.meta.lock().expect("meta lock");
            return DeviceSyncStatus {
                available: false,
                enabled: false,
                paused: false,
                endpoint_id: None,
                namespace: None,
                devices: Vec::new(),
                last_sync: None,
                problem: meta.problem.clone(),
            };
        };
        let status = sync.status().await;
        let snapshot = if status.namespace.is_some() { sync.snapshot().await.ok() } else { None };
        let mut devices = Vec::new();
        let mut latest_other_seen = None;
        if let Some(snapshot) = snapshot {
            for (id, device) in snapshot.devices {
                if id != status.endpoint_id {
                    latest_other_seen = Some(latest_other_seen.unwrap_or(0).max(device.last_seen));
                }
                devices.push(SyncDevice {
                    id,
                    name: device.name,
                    platform: device.platform,
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
        }
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

fn is_valid_email(address: &str) -> bool {
    let trimmed = address.trim();
    !trimmed.is_empty() && trimmed.contains('@') && !trimmed.contains(char::is_whitespace)
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
