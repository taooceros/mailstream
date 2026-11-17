//! Headless device-sync node (`mailstream sync daemon`, MS-362): runs `PeerSync` and the
//! `settings.json` bridge (`DeviceSync`) on one data directory without Mailstream's mail, chat,
//! calendar, AI tagging or OAuth layers. It bridges the accounts too (MS-376): records and the
//! refresh tokens sealed to this device land in accounts.json and the sign-ins, so an engine
//! started on the same data later uses them without signing in.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde_json::{Map, Value, json};

use crate::account_sync::{self, AccountHost, Credential};
use crate::accounts::{AccountRecord, Accounts};
use crate::commands::{self, Arguments, Handler};
use crate::device_sync::DeviceSync;
use crate::keychain::{self, Keychain};
use crate::peer_sync::{Network, PeerSync};
use crate::settings::Settings;
use crate::{EngineError, Events, control, daemon};

/// How long `mailstream sync join` waits for the pairing peer to become reachable before giving up.
const JOIN_REACH_TIMEOUT: Duration = Duration::from_secs(60);
const JOIN_POLL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct SyncNodeConfig {
    pub data_directory: PathBuf,
    pub network: Network,
    pub control_socket: bool,
    /// The sign-ins item the accounts' refresh tokens go to, as [`crate::Config`] names it.
    pub keychain_service: String,
    pub keychain_directory: PathBuf,
}

impl SyncNodeConfig {
    /// The app's sign-ins (`MAILSTREAM_KEYCHAIN_SERVICE` names another item, as for the engine):
    /// kept where the app keeps them for the app's data folder, else beside the data.
    pub fn new(data_directory: PathBuf) -> Self {
        let keychain_service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE")
            .ok()
            .filter(|service| !service.is_empty())
            .unwrap_or_else(|| keychain::SERVICE.into());
        let keychain_directory = if data_directory == crate::support_directory() {
            keychain::default_directory()
        } else {
            data_directory.clone()
        };
        Self { data_directory, network: Network::Public, control_socket: false, keychain_service, keychain_directory }
    }
}

/// The node's accounts.json and sign-ins, as device sync bridges them.
struct NodeAccounts {
    accounts: Accounts,
    keychain: Keychain,
}

impl AccountHost for NodeAccounts {
    fn keychain(&self) -> &Keychain {
        &self.keychain
    }

    fn accounts(&self) -> Vec<AccountRecord> {
        self.accounts.list()
    }

    fn adopt(&self, record: AccountRecord, credential: Option<Credential>) -> Result<(), String> {
        if let Some(credential) = &credential {
            let mut sign_ins = self.keychain.load().map_err(|error| error.to_string())?;
            account_sync::adopt_sign_in(&mut sign_ins, &record.id, record.provider, credential);
            self.keychain.store(&sign_ins).map_err(|error| error.to_string())?;
        }
        self.accounts.adopt(record).map(|_| ())
    }

    fn remove(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.accounts.remove(&id)?;
            let mut sign_ins = self.keychain.load().map_err(|error| error.to_string())?;
            sign_ins.refresh_tokens.remove(&id);
            sign_ins.clients.remove(&id);
            self.keychain.store(&sign_ins).map_err(|error| error.to_string())
        })
    }
}

struct Inner {
    settings: Arc<Settings>,
    events: Arc<Events>,
    device_sync: Arc<DeviceSync>,
    /// Held for `device_sync`, which keeps it weakly.
    _accounts: Arc<dyn AccountHost>,
}

impl Inner {
    async fn handle(&self, command: &str, args: Value) -> Result<Value, EngineError> {
        let spec = commands::spec(command)?;
        let values = match args {
            Value::Object(values) => values,
            _ => Map::new(),
        };
        let args = Arguments::new(spec, &values)?;
        match spec.handler {
            Handler::Settings => crate::run_local_settings(&self.settings, &self.events, command, &values),
            Handler::Sync => self.device_sync.run(command, &args).await,
            _ => Err(EngineError::new(
                "unavailable",
                format!(
                    "`{}` needs the whole engine: this headless sync node (`mailstream sync daemon`) only serves `sync.*` and `config.*`; run `mailstream daemon` instead.",
                    spec.name()
                ),
            )),
        }
    }
}

impl control::Handle for Inner {
    fn handle(&self, command: &str, args: Value) -> impl Future<Output = Result<Value, EngineError>> + Send {
        Inner::handle(self, command, args)
    }
}

/// One headless sync node owning `peer-sync/` in `data_directory`.
pub struct SyncNode {
    inner: Arc<Inner>,
    _watcher: Option<notify::RecommendedWatcher>,
    runtime: tokio::runtime::Runtime,
}

impl SyncNode {
    /// Opens `peer-sync/` in `config.data_directory`, failing immediately if another Mailstream
    /// process already holds its lock (`busy`).
    pub fn open(config: SyncNodeConfig) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&config.data_directory).map_err(|error| EngineError::failed(error.to_string()))?;
        // `peer-sync/` is private before the store writes anything into it (MS-369).
        let peer_sync = config.data_directory.join("peer-sync");
        std::fs::create_dir_all(&peer_sync).map_err(|error| EngineError::failed(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [&config.data_directory, &peer_sync] {
                let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
            }
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("mailstream-sync")
            .enable_all()
            .build()
            .map_err(|error| EngineError::failed(error.to_string()))?;
        let _entered = runtime.enter();
        let settings = Arc::new(Settings::new(&config.data_directory));
        let events = Arc::new(Events::default());
        let device_sync =
            DeviceSync::start(config.data_directory.clone(), config.network, settings.clone(), events.clone(), None);
        let accounts: Arc<dyn AccountHost> = Arc::new(NodeAccounts {
            accounts: Accounts::new(&config.data_directory),
            keychain: Keychain::new(config.keychain_service, config.keychain_directory),
        });
        device_sync.attach_accounts(Arc::downgrade(&accounts));
        let bridge_sync = Arc::downgrade(&device_sync);
        events.listen(move |event| {
            if event == "settings"
                && let Some(sync) = bridge_sync.upgrade()
            {
                sync.settings_changed();
            }
        });

        if let Err(error) = runtime.block_on(device_sync.open_or_err()) {
            runtime.block_on(device_sync.shutdown());
            return Err(error);
        }

        let inner = Arc::new(Inner { settings, events, device_sync, _accounts: accounts });
        let watcher = crate::watch_settings(inner.settings.clone(), inner.events.clone());
        if config.control_socket {
            control::serve(inner.clone(), config.data_directory);
        }
        drop(_entered);
        Ok(Self { inner, _watcher: watcher, runtime })
    }

    pub fn call(&self, command: &str, args: Value) -> impl Future<Output = Result<Value, EngineError>> + Send + 'static {
        let inner = self.inner.clone();
        let command = command.to_owned();
        let task = self.runtime.spawn(async move { inner.handle(&command, args).await });
        async move {
            task.await
                .unwrap_or_else(|error| Err(EngineError::failed(format!("The sync node stopped the request: {error}"))))
        }
    }

    pub fn call_sync(&self, command: &str, args: Value) -> Result<Value, EngineError> {
        self.runtime.block_on(self.call(command, args))
    }

    pub fn subscribe(&self, listener: impl Fn(&'static str) + Send + Sync + 'static) {
        self.inner.events.listen(listener);
    }

    /// Joins with pairing code `code` and waits up to `timeout` until another device of the group
    /// answered and this one has the group's accounts with their sign-ins, so a one-shot
    /// `mailstream sync join` does not exit before it can use them.
    pub fn join_and_wait(&self, code: &str, timeout: Duration) -> Result<Value, EngineError> {
        PeerSync::code_namespace(code).map_err(|error| EngineError::failed(error.to_string()))?;
        let mut status = self.call_sync("sync.join", json!({ "ticket": code }))?;
        let deadline = Instant::now() + timeout;
        let sync = self.inner.device_sync.clone();
        while !has_peer(&status) || !self.runtime.block_on(sync.accounts_received()) {
            if Instant::now() >= deadline {
                let what = if has_peer(&status) { "sent this device the group's accounts" } else { "answered" };
                return Err(EngineError::new(
                    "unreachable",
                    format!(
                        "Joined the sync group, but no other device {what} within {} s. Keep a device of the group online and try again (or pass `--join` to `mailstream sync daemon`).",
                        timeout.as_secs()
                    ),
                ));
            }
            std::thread::sleep(JOIN_POLL);
            status = self.call_sync("sync.status", json!({}))?;
        }
        Ok(status)
    }
}

impl Drop for SyncNode {
    fn drop(&mut self) {
        self.runtime.block_on(self.inner.device_sync.shutdown());
    }
}

pub(crate) fn has_peer(status: &Value) -> bool {
    let me = status.get("endpointId").and_then(Value::as_str);
    status
        .get("devices")
        .and_then(Value::as_array)
        .is_some_and(|devices| devices.iter().any(|device| device.get("id").and_then(Value::as_str) != me))
}

/// Runs `mailstream sync join` headlessly when no daemon/app serves the control socket.
pub fn run_join(data_directory: PathBuf, code: &str) -> Result<Value, EngineError> {
    PeerSync::code_namespace(code).map_err(|error| EngineError::failed(error.to_string()))?;
    let node = SyncNode::open(SyncNodeConfig::new(data_directory))?;
    node.join_and_wait(code, JOIN_REACH_TIMEOUT)
}

/// How long `mailstream sync pair-ticket` waits, once its code was used, for the joining device to
/// get this device's accounts.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(60);

/// Runs `mailstream sync pair-ticket` headlessly when no daemon or app serves the control socket
/// (MS-376). A code works only while the device that made it runs, so this prints it at once
/// through `emit`, then waits until a device used it and got this device's accounts, or until it
/// expired. Returns the status after.
pub fn run_pair(data_directory: PathBuf, mut emit: impl FnMut(&Value)) -> Result<Value, EngineError> {
    let node = SyncNode::open(SyncNodeConfig::new(data_directory))?;
    let receiving = |status: &Value| {
        status["devices"].as_array().map_or(0, |devices| devices.iter().filter(|device| device["accounts"] == true).count())
    };
    let before = receiving(&node.call_sync("sync.status", json!({}))?);
    let issued = Instant::now();
    emit(&node.call_sync("sync.pair_ticket", json!({}))?);
    let sync = node.inner.device_sync.clone();
    node.runtime.block_on(async {
        while sync.open_invites().await > 0 {
            tokio::time::sleep(JOIN_POLL).await;
        }
    });
    if issued.elapsed() >= crate::sync_invite::LIFETIME {
        return Err(EngineError::new("expired", "No device used the pairing code within 10 minutes."));
    }
    let deadline = Instant::now() + HANDOVER_TIMEOUT;
    loop {
        let status = node.call_sync("sync.status", json!({}))?;
        if receiving(&status) > before && node.runtime.block_on(sync.accounts_handed_over()) {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(EngineError::new(
                "unreachable",
                "The other device joined, but didn't get this device's accounts within a minute; keep a device \
                 with them online (or `mailstream sync daemon`) until it does.",
            ));
        }
        std::thread::sleep(JOIN_POLL);
    }
}

/// Runs `mailstream sync daemon`: serves `control.sock`, syncs `peer_sync` + `settings.json`,
/// emits JSON event lines via `emit`, and flushes cleanly on `SIGTERM`/`SIGINT`. `cloud_url`
/// turns on the Cloudflare drop (`sync.cloud_enable`) first.
pub fn run_daemon(
    data_directory: PathBuf,
    join_ticket: Option<&str>,
    cloud_url: Option<&str>,
    mut emit: impl FnMut(&Value),
) -> Result<(), EngineError> {
    if let Some(code) = join_ticket {
        PeerSync::code_namespace(code).map_err(|error| EngineError::failed(error.to_string()))?;
    }
    // The full daemon or the app may already serve this folder without holding `peer-sync/`.
    daemon::refuse_if_served(&data_directory)?;
    let mut config = SyncNodeConfig::new(data_directory);
    config.control_socket = true;
    let node = SyncNode::open(config)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    node.subscribe(move |event| {
        if matches!(event, "deviceSync" | "settings") {
            let _ = tx.send(event);
        }
    });
    daemon::join_and_cloud(|command, args| node.call_sync(command, args), join_ticket, cloud_url)?;

    let initial = node.call_sync("sync.status", json!({}))?;
    emit(&json!({ "event": "started", "status": initial }));

    node.runtime.block_on(async {
        let shutdown = daemon::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    let final_status = node.inner.handle("sync.status", json!({})).await.ok();
                    emit(&json!({ "event": "stopping", "status": final_status }));
                    break;
                }
                event = rx.recv() => {
                    let Some(first) = event else { break };
                    let mut had_sync = first == "deviceSync";
                    while let Ok(next) = rx.try_recv() {
                        had_sync |= next == "deviceSync";
                    }
                    if let Ok(status) = node.inner.handle("sync.status", json!({})).await {
                        let kind = if had_sync { "synced" } else { "settings" };
                        emit(&json!({ "event": kind, "status": status }));
                    }
                }
            }
        }
    });
    Ok(())
}
