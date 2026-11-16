//! `mailstream daemon` (MS-375): the whole engine (accounts, mail, calendar, chat, AI tagging,
//! settings and device sync) without a GUI, serving every command on `control.sock` in one data
//! directory, for servers and the agents on them. `mailstream sync daemon` (`sync_node`) stays the
//! light node that only relays settings between devices.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::peer_sync::PeerSync;
use crate::{Config, Engine, EngineError, control};

/// How long the started engine may take to serve its control socket.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
/// The events that change what the daemon reports, and the lines they print.
const STATUS_EVENTS: [&str; 2] = ["accounts", "problems"];
const SYNC_EVENTS: [&str; 2] = ["deviceSync", "settings"];

/// Runs `mailstream daemon` on `data_directory` until `SIGTERM`/`SIGINT`, printing JSON event
/// lines through `emit`; `join_ticket` and `cloud_url` as `mailstream sync daemon` takes them.
pub fn run(
    data_directory: PathBuf,
    join_ticket: Option<&str>,
    cloud_url: Option<&str>,
    emit: impl FnMut(&Value),
) -> Result<(), EngineError> {
    let config = Config::for_this_app().in_directory(data_directory);
    run_with(config, join_ticket, cloud_url, shutdown_signal(), emit)
}

/// [`run`] on `config`, stopping once `shutdown` completes (tests stop it themselves).
pub(crate) fn run_with(
    mut config: Config,
    join_ticket: Option<&str>,
    cloud_url: Option<&str>,
    shutdown: impl Future<Output = ()>,
    mut emit: impl FnMut(&Value),
) -> Result<(), EngineError> {
    let directory = config.support_directory.clone();
    let socket = control::socket_path(&directory);
    refuse_if_served(&directory)?;
    if let Some(ticket) = join_ticket {
        PeerSync::ticket_namespace(ticket).map_err(|error| EngineError::failed(error.to_string()))?;
    }
    std::fs::create_dir_all(&directory).map_err(|error| EngineError::failed(error.to_string()))?;
    // `accounts add` and `accounts connect` open the provider's sign-in page: on a server there is
    // no browser, so the page's address goes to the log, and to a browser only on a desktop
    // (`xdg-open` without one falls back to terminal browsers, which a service can't show).
    let (sign_ins, mut sign_in_pages) = tokio::sync::mpsc::unbounded_channel::<String>();
    let browser = config.oauth.open_url.clone();
    let desktop = desktop();
    config.oauth.open_url = Arc::new(move |url: &str| {
        let _ = sign_ins.send(url.to_owned());
        if desktop {
            browser(url);
        }
    });
    let port = config.oauth.redirect_port;
    let redirect = format!("http://localhost:{port}");
    config.control_socket = true;

    let engine = Engine::start(config);
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    engine.subscribe(move |event| {
        if STATUS_EVENTS.contains(&event) || SYNC_EVENTS.contains(&event) {
            let _ = sender.send(event);
        }
    });
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    while !served(&socket) {
        if Instant::now() >= deadline {
            return Err(EngineError::failed(format!("Could not serve {}.", socket.display())));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let call = |command: &str, args: Value| engine.runtime.block_on(engine.core.handle(command, args));
    join_and_cloud(call, join_ticket, cloud_url)?;

    let mut last_status = engine.core.status();
    emit(&json!({
        "event": "started",
        "dataDir": directory.display().to_string(),
        "socket": socket.display().to_string(),
        "status": last_status,
        "sync": call("sync.status", json!({})).ok(),
    }));

    engine.runtime.block_on(async {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    // mail.db queues its writes: land them before the runtime goes away.
                    engine.core.mail.flush().await;
                    emit(&json!({ "event": "stopping", "status": engine.core.status() }));
                    break;
                }
                Some(url) = sign_in_pages.recv() => {
                    emit(&json!({
                        "event": "signIn",
                        "url": url,
                        "redirect": redirect,
                        "message": format!("Open this page in a browser to sign in. It returns to {redirect} on this machine: from another one, forward that port first (ssh -L {port}:localhost:{port} <this server>)."),
                    }));
                }
                event = events.recv() => {
                    let Some(first) = event else { break };
                    let mut seen = vec![first];
                    while let Ok(next) = events.try_recv() {
                        seen.push(next);
                    }
                    if seen.iter().any(|event| STATUS_EVENTS.contains(event)) {
                        let status = engine.core.status();
                        if status != last_status {
                            emit(&json!({ "event": "status", "status": status }));
                            last_status = status;
                        }
                    }
                    if seen.iter().any(|event| SYNC_EVENTS.contains(event))
                        && let Ok(sync) = engine.core.handle("sync.status", json!({})).await
                    {
                        let kind = if seen.contains(&"deviceSync") { "synced" } else { "settings" };
                        emit(&json!({ "event": kind, "sync": sync }));
                    }
                }
            }
        }
    });
    Ok(())
}

/// `--join` and `--cloud`, as both daemons take them: joins `ticket`'s sync group unless this
/// device already syncs it with another device, then turns on the Cloudflare drop at `cloud_url`.
pub(crate) fn join_and_cloud(
    call: impl Fn(&str, Value) -> Result<Value, EngineError>,
    join_ticket: Option<&str>,
    cloud_url: Option<&str>,
) -> Result<(), EngineError> {
    if let Some(ticket) = join_ticket {
        let namespace = PeerSync::ticket_namespace(ticket).map_err(|error| EngineError::failed(error.to_string()))?;
        let current = call("sync.status", json!({}))?;
        let already_paired = current.get("namespace").and_then(Value::as_str) == Some(namespace.as_str())
            && current.get("paused").and_then(Value::as_bool) != Some(true)
            && crate::sync_node::has_peer(&current);
        if !already_paired {
            call("sync.join", json!({ "ticket": ticket }))?;
        }
    }
    if let Some(url) = cloud_url {
        call("sync.cloud_enable", json!({ "url": url }))?;
    }
    Ok(())
}

/// `busy` when a process (the app, either daemon) already serves `directory`'s control socket:
/// one Mailstream per data directory, or two would share mail.db, settings.json and `peer-sync/`.
pub(crate) fn refuse_if_served(directory: &Path) -> Result<(), EngineError> {
    if served(&control::socket_path(directory)) {
        return Err(EngineError::new(
            "busy",
            format!(
                "Another Mailstream process already serves {}; stop it, or give this daemon another --data-dir.",
                directory.display()
            ),
        ));
    }
    Ok(())
}

/// Whether a process answers on the control socket at `path`.
#[cfg(unix)]
fn served(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(windows)]
fn served(path: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open(path).is_ok()
}

/// Whether this process runs in a desktop session that can show a browser.
fn desktop() -> bool {
    if cfg!(all(unix, not(target_os = "macos"))) {
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
    } else {
        true
    }
}

/// Completes on `SIGTERM` or `SIGINT` (Ctrl-C on Windows). One subscription for the whole run:
/// tokio's handler stays installed once registered, so a signal arriving between per-iteration
/// subscriptions would be swallowed (MS-369).
#[cfg(unix)]
pub(crate) async fn shutdown_signal() {
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
pub(crate) async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
