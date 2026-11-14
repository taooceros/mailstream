//! Headless device-sync node (`mailstream sync daemon`, MS-362): runs `PeerSync` and the
//! `settings.json` bridge (`DeviceSync`) on one data directory without Mailstream's mail, chat,
//! calendar, AI tagging, or keychain/OAuth layers.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::commands::{self, Arguments, Handler};
use crate::device_sync::DeviceSync;
use crate::peer_sync::{Network, PeerSync};
use crate::settings::{Setting, Settings};
use crate::{EngineError, Events, control};

/// How long `mailstream sync join` waits for the pairing peer to become reachable before giving up.
const JOIN_REACH_TIMEOUT: Duration = Duration::from_secs(60);
const JOIN_POLL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct SyncNodeConfig {
    pub data_directory: PathBuf,
    pub network: Network,
    pub control_socket: bool,
}

impl SyncNodeConfig {
    pub fn new(data_directory: PathBuf) -> Self {
        Self { data_directory, network: Network::Public, control_socket: false }
    }
}

struct Inner {
    settings: Arc<Settings>,
    events: Arc<Events>,
    device_sync: Arc<DeviceSync>,
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
                    "`{}` requires the Mailstream app; this headless sync node only serves `sync.*` and `config.*`.",
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&config.data_directory, std::fs::Permissions::from_mode(0o700));
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
            DeviceSync::start(config.data_directory.clone(), config.network, settings.clone(), events.clone());
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                config.data_directory.join("peer-sync"),
                std::fs::Permissions::from_mode(0o700),
            );
        }

        let inner = Arc::new(Inner { settings, events, device_sync });
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

    /// Joins `ticket` and waits up to `timeout` until at least one peer device appears in the
    /// group, so a one-shot `mailstream sync join` does not exit before discovering peers.
    pub fn join_and_wait(&self, ticket: &str, timeout: Duration) -> Result<Value, EngineError> {
        PeerSync::ticket_namespace(ticket).map_err(|error| EngineError::failed(error.to_string()))?;
        let mut status = self.call_sync("sync.join", json!({ "ticket": ticket }))?;
        let deadline = Instant::now() + timeout;
        while !has_peer(&status) {
            if Instant::now() >= deadline {
                return Err(EngineError::new(
                    "unreachable",
                    format!(
                        "Joined the sync group locally, but no other device answered within {} s. Make sure the pairing device is online and try again (or pass `--join` to `mailstream sync daemon`).",
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

fn has_peer(status: &Value) -> bool {
    let me = status.get("endpointId").and_then(Value::as_str);
    status
        .get("devices")
        .and_then(Value::as_array)
        .is_some_and(|devices| devices.iter().any(|device| device.get("id").and_then(Value::as_str) != me))
}

/// Runs `mailstream sync join` headlessly when no daemon/app serves the control socket.
pub fn run_join(data_directory: PathBuf, ticket: &str) -> Result<Value, EngineError> {
    PeerSync::ticket_namespace(ticket).map_err(|error| EngineError::failed(error.to_string()))?;
    let node = SyncNode::open(SyncNodeConfig::new(data_directory))?;
    node.join_and_wait(ticket, JOIN_REACH_TIMEOUT)
}

/// Runs `mailstream sync daemon`: serves `control.sock`, syncs `peer_sync` + `settings.json`,
/// emits JSON event lines via `emit`, and flushes cleanly on `SIGTERM`/`SIGINT`.
pub fn run_daemon(
    data_directory: PathBuf,
    join_ticket: Option<&str>,
    mut emit: impl FnMut(&Value),
) -> Result<(), EngineError> {
    let target_namespace = join_ticket
        .map(PeerSync::ticket_namespace)
        .transpose()
        .map_err(|error| EngineError::failed(error.to_string()))?;

    let mut config = SyncNodeConfig::new(data_directory);
    config.control_socket = true;
    let node = SyncNode::open(config)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    node.subscribe(move |event| {
        if matches!(event, "deviceSync" | "settings") {
            let _ = tx.send(event);
        }
    });

    if let (Some(ticket), Some(target_ns)) = (join_ticket, target_namespace.as_deref()) {
        let current = node.call_sync("sync.status", json!({}))?;
        let already_paired = current.get("namespace").and_then(Value::as_str) == Some(target_ns)
            && current.get("paused").and_then(Value::as_bool) != Some(true)
            && has_peer(&current);
        if !already_paired {
            node.call_sync("sync.join", json!({ "ticket": ticket }))?;
        }
    }

    let initial = node.call_sync("sync.status", json!({}))?;
    emit(&json!({ "event": "started", "status": initial }));

    node.runtime.block_on(async {
        loop {
            tokio::select! {
                _ = shutdown_signal() => {
                    let final_status = node.inner.handle("sync.status", json!({})).await.ok();
                    emit(&json!({ "event": "stopping", "status": final_status }));
                    break;
                }
                event = rx.recv() => {
                    let Some(first) = event else { break };
                    let mut had_sync = first == "deviceSync";
                    let mut had_settings = first == "settings";
                    while let Ok(next) = rx.try_recv() {
                        had_sync |= next == "deviceSync";
                        had_settings |= next == "settings";
                    }
                    if let Ok(status) = node.inner.handle("sync.status", json!({})).await {
                        let kind = if had_sync { "synced" } else { "settings" };
                        let mut payload = json!({ "event": kind, "status": status });
                        if had_settings {
                            payload["trustedSenders"] = node.inner.settings.value(Setting::TrustedSenders);
                        }
                        emit(&payload);
                    }
                }
            }
        }
    });
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            match sigterm.as_mut() {
                Some(stream) => {
                    let _ = stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        } => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
