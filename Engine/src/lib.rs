//! Mailstream's engine in Rust, rewritten from the Swift one (`Sources/MailstreamKit`, which only
//! Mailstream.app still runs until MS-120). The `mailstream` crate in `FFI/` runs it in Mailstream
//! Flutter and Mailstream GPUI; `src/bin/mailstream.rs` is the command line.
//!
//! The engine runs on its own tokio runtime, so its host needs no main-thread run loop. Requests
//! are the control socket's commands (`config.set`, `accounts.list`, …) with their arguments as a
//! JSON object, answered with the same result values and error codes as the Swift engine. It
//! reads and writes the same files and Keychain item, so either engine picks up what the other
//! wrote.
uniffi::setup_scaffolding!();

mod account_sync;
mod accounts;
pub mod answers;
pub mod api;
mod assembly;
pub mod avatars;
pub mod backend;
mod bundle;
pub mod cache;
mod calendar;
mod calendar_api;
#[cfg(test)]
mod calendar_tests;
mod chat;
mod chat_api;
pub mod chat_model;
pub mod chat_markup;
mod cloud_drop;
pub mod commands;
mod content;
pub mod control;
pub mod daemon;
pub mod device_sync;
pub mod display;
// The providers' stand-in: the tests' and the screenshot mode's (`shots`).
#[cfg(any(test, feature = "shots"))]
#[cfg_attr(not(test), allow(dead_code))]
mod fake_server;
#[cfg(test)]
mod fake_imap;
mod files;
pub mod gmail;
pub mod graph;
mod ics;
#[cfg(test)]
mod invitation_tests;
pub mod json;
mod keychain;
pub mod mail;
mod mime;
mod mime_parse;
pub mod model;
pub mod notifications;
mod oauth;
mod omp;
mod polling;
mod peer_mail;
pub mod peer_sync;
pub mod push;
pub mod settings;
#[cfg(feature = "shots")]
pub mod shots;
pub mod signal;
mod sync_invite;
pub mod sync_node;
pub mod tagging;
pub mod tagsync;
pub mod text;
mod threader;
mod tokens;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod mail_tests;
#[cfg(test)]
mod push_tests;
#[cfg(test)]
mod chat_tests;
#[cfg(test)]
mod peer_mail_tests;
#[cfg(test)]
mod peer_sync_tests;
#[cfg(test)]
mod device_sync_tests;
#[cfg(test)]
mod daemon_tests;
#[cfg(test)]
mod sync_node_tests;
#[cfg(test)]
mod cloud_drop_tests;
// Runs a shell-script omp (`omp::tests::fake_omp`).
#[cfg(all(test, unix))]
mod tagging_tests;
#[cfg(all(test, unix))]
mod tagsync_tests;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{Map, Value, json};

use crate::accounts::{AccountRecord, Accounts};
use crate::commands::{Arguments, Handler};
use crate::keychain::Keychain;
use crate::oauth::{Connect, Grant, OAuth, Service};
pub use crate::oauth::{BuiltInClients, OAuthClient, OAuthConfig, Provider};
use crate::settings::Settings;
use crate::tokens::Tokens;

/// A failed request: a stable `code` for programs (`usage`, `invalid_setting`,
/// `unknown_account`, `confirm`, `failed`, …) and a `message` for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub code: String,
    pub message: String,
}

impl EngineError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::new("usage", message)
    }

    fn invalid_setting(message: impl Into<String>) -> Self {
        Self::new("invalid_setting", message)
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

/// Where the engine keeps its data and how it signs in.
#[derive(Clone)]
pub struct Config {
    /// Holds settings.json, accounts.json and mail.db: `~/Library/Application Support/Mailstream`.
    pub support_directory: PathBuf,
    /// Attachments, inline parts and Gmail's envelope caches: `~/Library/Caches/dev.mailstream.Mailstream`.
    pub cache_directory: PathBuf,
    /// The Keychain service of the sign-ins item; tests use their own.
    pub keychain_service: String,
    /// Where Linux and Windows keep `<service>.signins` (`~/.local/share/Mailstream` /
    /// `%LOCALAPPDATA%\Mailstream`); tests use their temporary folder. Unused on macOS.
    pub keychain_directory: PathBuf,
    pub built_in_clients: BuiltInClients,
    pub oauth: OAuthConfig,
    /// The providers' API roots; tests use a local server.
    pub endpoints: backend::Endpoints,
    /// The push relay; none (tests, builds without it) polls only.
    pub push: push::PushConfig,
    /// The environment the AI tagger runs `omp` in; None recovers the login shell's PATH as the
    /// Swift engine does. Tests give their own, so no test ever finds the user's omp.
    pub tool_environment: Option<HashMap<String, String>>,
    /// The Apple Intelligence tagger (`mailstream-apple-tagger`, for `aiTagging.backend`
    /// `apple`): beside this process's executable, where the front ends bundle it. Tests name a
    /// fake one.
    pub apple_tagger: PathBuf,
    /// The public web sender pictures come from (Gravatar, BIMI, site icons); tests answer
    /// from memory.
    pub avatar_web: Arc<dyn avatars::web::Web>,
    /// Serves the control socket in `support_directory` whenever no other process does.
    pub control_socket: bool,
    /// Cross-device settings sync network; `None` in `shots` mode and unit tests that don't sync.
    pub device_sync: Option<peer_sync::Network>,
}

impl Config {
    /// The data, sign-ins and OAuth clients this process's app shares with the Swift engine.
    /// `MAILSTREAM_KEYCHAIN_SERVICE` names another sign-ins item, as it does for the Swift engine
    /// (benchmarks and tests that must not read the person's sign-ins). Builds with the `shots`
    /// feature use the screenshot mode's provider instead once it runs (`shots::start`).
    #[allow(clippy::let_and_return, reason = "`shots` builds change it before it returns")]
    pub fn for_this_app() -> Self {
        let support = support_directory();
        let caches = cache_directory();
        let service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE").ok().filter(|service| !service.is_empty());
        let config = Self {
            support_directory: support,
            cache_directory: caches,
            keychain_service: service.unwrap_or_else(|| keychain::SERVICE.into()),
            keychain_directory: keychain::default_directory(),
            built_in_clients: bundle::built_in_clients(),
            oauth: OAuthConfig::default(),
            endpoints: backend::Endpoints::default(),
            push: bundle::push_config(),
            tool_environment: None,
            apple_tagger: std::env::current_exe()
                .ok()
                .and_then(|executable| executable.parent().map(|directory| directory.join(omp::APPLE_TAGGER)))
                .unwrap_or_else(|| PathBuf::from(omp::APPLE_TAGGER)),
            avatar_web: avatars::web::Internet::shared(),
            control_socket: true,
            device_sync: Some(peer_sync::Network::Public),
        };
        #[cfg(feature = "shots")]
        let config = shots::config(config);
        config
    }

    /// The same with every file in `directory` (`mailstream daemon --data-dir`, MS-375):
    /// settings.json, accounts.json, mail.db, `peer-sync/`, control.sock and, off macOS, the
    /// sign-ins file in it, the caches in its `cache/`. The app's own support directory keeps
    /// the app's places, so a daemon there shares the GUI's sign-ins and caches.
    pub fn in_directory(mut self, directory: PathBuf) -> Self {
        if directory != self.support_directory {
            self.cache_directory = directory.join("cache");
            self.keychain_directory = directory.clone();
            self.support_directory = directory;
        }
        self
    }
}

/// Where the app's data lives: `~/Library/Application Support/Mailstream` (settings.json,
/// accounts.json, mail.db, the control socket).
pub fn support_directory() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Mailstream")
}

/// The folder of files the engine can fetch again (attachments, avatars, the reader's web fonts):
/// `~/Library/Caches/dev.mailstream.Mailstream`.
pub fn cache_directory() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("dev.mailstream.Mailstream")
}

/// A `config.*` command on the settings.json in `directory`, without an engine: what the
/// command line runs itself.
pub fn run_settings_command(
    directory: &std::path::Path,
    command: &str,
    args: &Map<String, Value>,
) -> Result<Value, EngineError> {
    Settings::new(directory).run(command, args)
}

type Listener = Arc<dyn Fn(&'static str) + Send + Sync>;

/// The change events front ends re-read on: `accounts`, `settings`, `mailboxes`, ….
#[derive(Default)]
pub struct Events {
    listeners: Mutex<Vec<Listener>>,
    /// Events `emit_soon` has scheduled and not sent yet.
    pending: Mutex<std::collections::HashSet<&'static str>>,
}

impl Events {
    pub fn emit(&self, event: &'static str) {
        let listeners = self.listeners.lock().expect("events lock").clone();
        for listener in listeners {
            listener(event);
        }
    }

    /// Calls `listener` with each event from now on, on whichever thread sends it.
    pub fn listen(&self, listener: impl Fn(&'static str) + Send + Sync + 'static) {
        self.listeners.lock().expect("events lock").push(Arc::new(listener));
    }

    /// `event` a moment from now, once however often it is asked for meanwhile: a burst of
    /// changes (previews landing one by one) reaches front ends as one event, as the Swift
    /// engine's observations coalesce within a run loop turn. Call on the engine's runtime.
    pub fn emit_soon(self: &Arc<Self>, event: &'static str) {
        if !self.pending.lock().expect("events lock").insert(event) {
            return;
        }
        let events = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            events.pending.lock().expect("events lock").remove(event);
            events.emit(event);
        });
    }
}

/// What the commands work on, shared by the runtime's tasks.
struct Core {
    settings: Arc<Settings>,
    accounts: Arc<Accounts>,
    tokens: Arc<Tokens>,
    oauth: Arc<OAuth>,
    events: Arc<Events>,
    mail: Arc<mail::Mail>,
    push: Arc<push::Push>,
    polling: Arc<polling::Polling>,
    chat: Arc<chat::Chat>,
    calendar: Arc<calendar::CalendarStore>,
    avatars: Arc<avatars::Avatars>,
    device_sync: Option<Arc<device_sync::DeviceSync>>,
    notified: Arc<notifications::NotifiedLog>,
    support_directory: PathBuf,
    /// Push relay settings provided by the host bundle / config.json, if any (MS-444).
    host_push: Option<push::PushSettings>,
}

/// One engine; the FFI host keeps one per process.
pub struct Engine {
    core: Arc<Core>,
    /// Follows settings.json; dropping it stops.
    _watcher: Option<notify::RecommendedWatcher>,
    runtime: tokio::runtime::Runtime,
}

impl Engine {
    /// Reads the settings and accounts, starts following settings.json, and starts the mail
    /// store (it paints from mail.db and loads every account's mailboxes and inboxes), polling,
    /// push and the chat store (it paints from mail.db and polls every account's chat spaces).
    pub fn start(config: Config) -> Self {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.thread_name("mailstream-engine").enable_all();
        #[cfg(target_os = "android")]
        {
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
                .clamp(2, 4);
            builder.worker_threads(workers).max_blocking_threads(16);
        }
        let runtime = builder.build().expect("the engine's runtime starts");
        let directory = &config.support_directory;
        let _ = std::fs::create_dir_all(directory);
        let accounts = Arc::new(Accounts::new(directory));
        let signed_in: HashMap<_, _> = accounts
            .list()
            .into_iter()
            .rev()
            .map(|record| (record.id, tokens::SignedIn { provider: record.provider, scopes: record.scopes }))
            .collect();
        let oauth = Arc::new(OAuth::new(config.oauth.clone()));
        let tokens = Arc::new(Tokens::new(
            Keychain::new(config.keychain_service.clone(), config.keychain_directory.clone()),
            oauth.clone(),
            config.built_in_clients,
            signed_in,
        ));
        let settings = Arc::new(Settings::new(directory));
        let events = Arc::new(Events::default());
        let _entered = runtime.enter();
        let mail = Arc::new(mail::Mail::new(
            directory.join("mail.db"),
            config.cache_directory.clone(),
            config.endpoints.clone(),
            tokens.clone(),
            accounts.clone(),
            settings.clone(),
            events.clone(),
            config.tool_environment.clone(),
            config.apple_tagger.clone(),
        ));
        let host_push = config.push.socket_configured().then(|| config.push.settings());
        let mut push_config = config.push.clone();
        if !push_config.socket_configured()
            && config.device_sync.is_some()
            && let Some(synced) = push::read_synced(directory)
        {
            push_config = push_config.with_settings(&synced);
        }
        let push = Arc::new(push::Push::new(
            push_config,
            directory.clone(),
            &config.endpoints,
            accounts.clone(),
            mail.clone(),
        ));
        let polling = Arc::new(polling::Polling::new(mail.clone(), push.clone(), settings.clone(), events.clone()));
        let avatars = {
            let (accounts, mail) = (accounts.clone(), mail.clone());
            Arc::new(avatars::Avatars::new(
                config.cache_directory.join("avatars"),
                config.avatar_web.clone(),
                avatars::Roots { people: config.endpoints.people.clone(), graph: config.endpoints.graph.clone() },
                move || avatars::photo_accounts(&accounts.list(), |account| mail.api(account)),
            ))
        };
        let chat = Arc::new(chat::Chat::new(
            mail.clone(),
            config.endpoints.chat.clone(),
            accounts.clone(),
            settings.clone(),
            events.clone(),
            avatars.clone(),
        ));
        let calendar = Arc::new(calendar::CalendarStore::new(
            mail.clone(),
            config.endpoints.calendar.clone(),
            config.endpoints.graph.clone(),
            accounts.clone(),
            events.clone(),
        ));
        let device_sync = config.device_sync.clone().filter(|_| {
            #[cfg(feature = "shots")]
            {
                !shots::running()
            }
            #[cfg(not(feature = "shots"))]
            {
                true
            }
        }).map(|network| {
            device_sync::DeviceSync::start(
                directory.clone(),
                network,
                settings.clone(),
                events.clone(),
                Some(mail.peer_mail()),
            )
        });
        let bridge_sync = device_sync.as_ref().map(Arc::downgrade);
        let (reschedule, chat_interval) = (Arc::downgrade(&polling), Arc::downgrade(&chat));
        events.listen(move |event| {
            if event != "settings" {
                return;
            }
            if let Some(polling) = reschedule.upgrade() {
                polling.settings_changed();
            }
            if let Some(chat) = chat_interval.upgrade() {
                chat.settings_changed();
            }
            if let Some(sync) = bridge_sync.as_ref().and_then(std::sync::Weak::upgrade) {
                sync.settings_changed();
            }
        });
        let notified = Arc::new(notifications::NotifiedLog::new(directory));
        let core = Arc::new(Core {
            settings,
            accounts,
            tokens,
            oauth,
            events,
            mail: mail.clone(),
            push: push.clone(),
            polling: polling.clone(),
            chat: chat.clone(),
            calendar: calendar.clone(),
            avatars,
            device_sync,
            notified,
            support_directory: directory.clone(),
            host_push,
        });
        if let Some(sync) = &core.device_sync {
            let host: std::sync::Weak<dyn account_sync::AccountHost> = Arc::downgrade(&core) as _;
            sync.attach_accounts(host);
            let sync_weak = Arc::downgrade(sync);
            push.on_registrations(move || {
                if let Some(sync) = sync_weak.upgrade() {
                    sync.push_changed();
                }
            });
        }
        let watcher = Self::follow_settings(&core);
        runtime.spawn(mail.start());
        polling.start();
        push.start();
        runtime.spawn(chat.start());
        runtime.spawn(calendar.start());
        if config.control_socket {
            control::serve(core.clone(), config.support_directory.clone());
        }
        drop(_entered);
        Self { core, _watcher: watcher, runtime }
    }

    /// Re-reads settings.json whenever it is written, replaced, created or deleted, and tells the
    /// listeners when what it says changed. The directory is watched, so saves that replace the
    /// file are followed too.
    fn follow_settings(core: &Arc<Core>) -> Option<notify::RecommendedWatcher> {
        // Android has no external CLI/daemon editing settings.json inside the app sandbox, and
        // `run_local_settings` already reloads and emits `"settings"` directly.
        #[cfg(target_os = "android")]
        {
            let _ = core;
            None
        }
        #[cfg(not(target_os = "android"))]
        {
            watch_settings(core.settings.clone(), core.events.clone())
        }
    }

    /// Runs one command; the future may be awaited on any executor.
    pub fn call(
        &self,
        command: &str,
        args: Value,
    ) -> impl Future<Output = Result<Value, EngineError>> + Send + 'static {
        let core = self.core.clone();
        let command = command.to_owned();
        let task = self.runtime.spawn(async move { core.handle(&command, args).await });
        async move {
            task.await
                .unwrap_or_else(|error| Err(EngineError::failed(format!("The engine stopped the request: {error}"))))
        }
    }

    /// Calls `listener` with each change from now on (`accounts`, `settings`, `mailboxes`,
    /// `conversations`, `previews`, `tags`, `problems`, `sync`, `chat`, `chatMessages`,
    /// `calendar`), on an engine thread.
    pub fn subscribe(&self, listener: impl Fn(&'static str) + Send + Sync + 'static) {
        self.core.events.listen(listener);
    }

    /// Subscribes to newly arrived unread inbox mail events (`mailstream mail watch`, MS-452).
    pub fn subscribe_new_mail(&self) -> tokio::sync::broadcast::Receiver<answers::NewMail> {
        self.core.mail.subscribe_new_mail()
    }

    /// Subscribes to newly arrived unread incoming Google Chat messages (MS-443).
    pub fn subscribe_new_chat(&self) -> tokio::sync::broadcast::Receiver<answers::NewChat> {
        self.core.chat.subscribe_new_chat()
    }

    /// Calls `listener` on the engine's runtime for each newly arrived unread inbox message.
    pub fn on_new_mail(&self, listener: impl Fn(answers::NewMail) + Send + Sync + 'static) {
        let mut rx = self.core.mail.subscribe_new_mail();
        self.runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(item) => listener(item),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Calls `listener` on the engine's runtime for each newly arrived unread incoming Chat message.
    pub fn on_new_chat(&self, listener: impl Fn(answers::NewChat) + Send + Sync + 'static) {
        let mut rx = self.core.chat.subscribe_new_chat();
        self.runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(item) => listener(item),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Formats `mail` as an OS notification if allowed by `notifications.*` settings.
    pub fn format_mail_notification(&self, mail: &answers::NewMail) -> Option<notifications::NotificationItem> {
        notifications::format_mail(&self.core.settings, mail)
    }

    /// Formats `chat` as an OS notification if allowed by `notifications.*` settings.
    pub fn format_chat_notification(&self, chat: &answers::NewChat) -> Option<notifications::NotificationItem> {
        notifications::format_chat(&self.core.settings, chat)
    }

    /// Persistently claims `dedup_id` so the same message is never notified twice across restarts.
    pub fn claim_notification(&self, dedup_id: &str) -> bool {
        self.core.notified.claim(dedup_id)
    }

    /// Records this mobile device's FCM registration token and uploads it to the push relay.
    pub fn set_fcm_device(&self, device_id: String, fcm_token: String) {
        let push = self.core.push.clone();
        let _entered = self.runtime.enter();
        push.set_fcm_device(device_id, fcm_token);
    }

    /// Removes this mobile device's FCM registration token locally and on the push relay.
    pub fn clear_fcm_device(&self, device_id: String) {
        let push = self.core.push.clone();
        let _entered = self.runtime.enter();
        push.clear_fcm_device(device_id);
    }

    /// Current FCM client options (`MSFcm*` / Device Sync), if configured.
    pub fn fcm_client_config(&self) -> Option<push::FcmClientConfig> {
        self.core.push.fcm_client_config()
    }

    /// Registers a listener invoked whenever FCM client options become available or change.
    pub fn on_fcm_config(&self, handler: impl Fn(push::FcmClientConfig) + Send + Sync + 'static) {
        self.core.push.on_fcm_config(handler);
    }

    /// Announces a platform network transition (e.g. Android `ConnectivityManager`) so push
    /// reconnects immediately instead of waiting out backoff.
    pub fn notify_network_change(&self) {
        signal::ProcessSignal::NETWORK_CHANGE.post();
    }

    /// Flushes queued `mail.db` writes before a background service exits.
    pub fn flush_mail(&self) -> impl Future<Output = ()> + Send + 'static {
        let mail = self.core.mail.clone();
        let task = self.runtime.spawn(async move { mail.flush().await });
        async move {
            let _ = task.await;
        }
    }

    /// Runs one bounded background push-wake sync pass (MS-443 Android FCM silent wake):
    /// seeds from `mail.db` cache if cold-started, refreshes target accounts, collects & filters
    /// newly arrived notifications, deduplicates via `notified.json`, and flushes `mail.db`.
    pub fn background_push_wake(
        &self,
        source: String,
        account: Option<String>,
        subscription_id: Option<String>,
        timeout: std::time::Duration,
    ) -> impl Future<Output = Vec<notifications::NotificationItem>> + Send + 'static {
        let core = self.core.clone();
        let task = self.runtime.spawn(async move {
            let run = async move {
                let mut items = Vec::new();
                if source == "chat" {
                    core.chat.wait_cache_loaded().await;
                    core.chat.seed_from_cache();
                    let mut chat_rx = core.chat.subscribe_new_chat();
                    core.chat.refresh_now(account.as_deref()).await;
                    while let Ok(msg) = chat_rx.try_recv() {
                        if let Some(item) = notifications::format_chat(&core.settings, &msg)
                            && core.notified.claim(&item.dedup_id)
                        {
                            items.push(item);
                        }
                    }
                    core.mail.flush().await;
                    return items;
                }

                core.mail.seed_inbox_from_cache().await;
                let mut mail_rx = core.mail.subscribe_new_mail();
                let accounts = core.push.resolve_wake_accounts(
                    &source,
                    account.as_deref(),
                    subscription_id.as_deref(),
                );
                core.mail.refresh_accounts_now(&accounts).await;
                // Give `publish_new_mail` snippet enrichment a brief settle window (up to 400ms).
                let drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
                loop {
                    let remaining = drain_deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(remaining, mail_rx.recv()).await {
                        Ok(Ok(mail)) => {
                            if let Some(item) = notifications::format_mail(&core.settings, &mail)
                                && core.notified.claim(&item.dedup_id)
                            {
                                items.push(item);
                            }
                        }
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
                    }
                }
                core.mail.flush().await;
                items
            };
            tokio::time::timeout(timeout, run).await.unwrap_or_default()
        });
        async move { task.await.unwrap_or_default() }
    }

    /// Synchronous wrapper around [`Engine::background_push_wake`] for native background callbacks
    /// (such as Android `FirebaseMessagingService.onMessageReceived`).
    pub fn background_push_wake_blocking(
        &self,
        source: String,
        account: Option<String>,
        subscription_id: Option<String>,
        timeout: std::time::Duration,
    ) -> Vec<notifications::NotificationItem> {
        let fut = self.background_push_wake(source, account, subscription_id, timeout);
        self.runtime.block_on(fut)
    }

    /// Synchronous command execution + DB flush for background notification quick actions.
    pub fn run_command_blocking(&self, command: &str, args: Value) -> Result<Value, EngineError> {
        let core = self.core.clone();
        let command = command.to_owned();
        self.runtime.block_on(async move {
            let res = core.handle(&command, args).await;
            core.mail.flush().await;
            res
        })
    }
    /// Whether remote images and fonts of conversation `id` (from a listing) load on their own,
    /// and the senders that keep them from it; see [`mail::Mail::remote_content`]. The
    /// `mail.remote` command answers the same.
    pub fn remote_content(&self, id: &str) -> Result<(bool, Vec<model::Address>), EngineError> {
        self.core.mail.remote_content(id)
    }

    /// A current OAuth access token for `account`, refreshed from its Keychain sign-in when it
    /// is missing or expires within five minutes: what the provider APIs of the next phases
    /// authorize with. `failed` with the reason when there is none (401 means sign in again).
    pub fn access_token(&self, account: &str) -> impl Future<Output = Result<String, EngineError>> + Send + 'static {
        let core = self.core.clone();
        let account = account.to_owned();
        let task = self.runtime.spawn(async move { core.tokens.token(&account).await });
        async move {
            match task.await {
                Ok(result) => result.map_err(|error| EngineError::failed(error.to_string())),
                Err(error) => Err(EngineError::failed(format!("The engine stopped the request: {error}"))),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn direct_peers(&self) -> usize {
        self.core.mail.peer_mail().direct_connected_count()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(device_sync) = self.core.device_sync.clone() {
            self.runtime.block_on(device_sync.shutdown());
        }
    }
}

/// Re-reads settings.json whenever it is written, replaced, created or deleted, and tells the
/// listeners when what it says changed. Call on a tokio runtime.
pub(crate) fn watch_settings(settings: Arc<Settings>, events: Arc<Events>) -> Option<notify::RecommendedWatcher> {
    let (sender, mut changes) = tokio::sync::mpsc::unbounded_channel::<()>();
    let file = settings.file().path().to_path_buf();
    let name = file.file_name().map(ToOwned::to_owned);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, EventKind::Access(_))
            && event.paths.iter().any(|path| path.file_name() == name.as_deref())
        {
            let _ = sender.send(());
        }
    })
    .ok()?;
    watcher.watch(file.parent()?, RecursiveMode::NonRecursive).ok()?;
    tokio::spawn(async move {
        while changes.recv().await.is_some() {
            while changes.try_recv().is_ok() {}
            if settings.reload() {
                events.emit("settings");
            }
        }
    });
    Some(watcher)
}

/// Runs a `config.*` command on `settings`, reloads it, and notifies listeners if it changed.
pub(crate) fn run_local_settings(
    settings: &Settings,
    events: &Events,
    command: &str,
    values: &Map<String, Value>,
) -> Result<Value, EngineError> {
    let result = settings.run(command, values)?;
    if settings.reload() {
        events.emit("settings");
    }
    Ok(result)
}

impl Core {
    async fn handle(&self, command: &str, args: Value) -> Result<Value, EngineError> {
        let spec = commands::spec(command)?;
        let values = match args {
            Value::Object(values) => values,
            _ => Map::new(),
        };
        let args = Arguments::new(spec, &values)?;
        match spec.handler {
            Handler::Settings => run_local_settings(&self.settings, &self.events, command, &values),
            Handler::Core => self.run(command, &args).await,
            Handler::Mail => self.mail.run(command, &args).await,
            Handler::Tagging => self.mail.tagging_command(command, &args),
            Handler::Avatars => self.avatars.command(&args).await,
            Handler::Chat => self.chat.run(command, &args).await,
            Handler::Calendar => self.calendar.run(command, &args).await,
            Handler::Sync => match &self.device_sync {
                Some(device_sync) => device_sync.run(command, &args).await,
                None => {
                    #[cfg(feature = "shots")]
                    if shots::running() {
                        return shots::device_sync(command, &args, &self.events);
                    }
                    match command {
                        "sync.status" => Ok(answers::json(&answers::DeviceSyncStatus {
                            available: false,
                            enabled: false,
                            paused: false,
                            endpoint_id: None,
                            namespace: None,
                            devices: Vec::new(),
                            last_sync: None,
                            problem: Some("Device sync is disabled in this mode.".into()),
                            cloud: Default::default(),
                            mail: Default::default(),
                        })),
                        _ => Err(EngineError::new("unavailable", "Device sync is disabled in this mode.")),
                    }
                }
            },
        }
    }

    async fn stream(&self, command: &str, args: Value) -> control::StreamReply {
        if command != "mail.watch" {
            return control::StreamReply::NotStreaming;
        }
        let spec = match commands::spec(command) {
            Ok(spec) => spec,
            Err(error) => return control::StreamReply::Error(error),
        };
        let values = match args {
            Value::Object(values) => values,
            _ => Map::new(),
        };
        let args = match Arguments::new(spec, &values) {
            Ok(args) => args,
            Err(error) => return control::StreamReply::Error(error),
        };
        match self.mail.watch(&args) {
            Ok(rx) => control::StreamReply::Started(rx),
            Err(error) => control::StreamReply::Error(error),
        }
    }

    /// The commands the table gives [`Handler::Core`].
    async fn run(&self, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "status" => Ok(self.status()),
            "refresh" => Ok(self.polling.refresh()),
            "contacts.search" => {
                let avatars = self.avatars.clone();
                let people = async move {
                    avatars.people().await.into_iter().map(|person| (person.name, person.emails)).collect()
                };
                Ok(self.mail.search_contacts(args, people).await)
            }
            "accounts.list" => {
                let accounts: Vec<answers::Account> = self
                    .accounts
                    .list()
                    .iter()
                    .map(|record| {
                        let mut answer = record.answer();
                        answer.identities = self.mail.account_identities(&record.id);
                        answer
                    })
                    .collect();
                Ok(answers::json(&accounts))
            }
            "accounts.rename" => {
                let record = self.record(args)?;
                let renamed = self
                    .accounts
                    .rename(&record.id, &args.string("label").unwrap_or_default())
                    .map_err(EngineError::failed)?;
                if renamed.as_ref() != Some(&record) {
                    self.events.emit("accounts");
                    self.mail.accounts_changed();
                    if let Some(sync) = &self.device_sync {
                        sync.accounts_changed();
                    }
                }
                Ok(answers::json(&renamed.as_ref().map(AccountRecord::answer)))
            }
            "accounts.client" => {
                let client = self
                    .tokens
                    .suggested_client(provider(args)?)
                    .map_err(|error| EngineError::failed(error.to_string()))?;
                Ok(answers::json(&client.map(|client| answers::OAuthClientInfo {
                    has_secret: client.secret.is_some_and(|secret| !secret.is_empty()),
                    id: client.id,
                })))
            }
            "accounts.add" => self.add_account(args).await,
            "accounts.connect" => self.connect_account(args).await,
            "accounts.remove" => {
                let record = self.record(args)?;
                if !args.bool("yes") {
                    return Err(EngineError::new(
                        "confirm",
                        format!(
                            "Would sign out of {} and delete its saved sign-in (mail on the server stays). Re-run with --yes.",
                            record.id
                        ),
                    ));
                }
                let unregister = match &self.device_sync {
                    Some(sync) => sync.should_unregister_push().await,
                    None => true,
                };
                self.sign_out(&record.id, unregister).await?;
                if let Some(sync) = &self.device_sync {
                    sync.accounts_changed();
                }
                Ok(json!({ "removed": record.id }))
            }
            _ => unreachable!("{command} is a core command"),
        }
    }

    /// The engine's state: version, accounts, sync, push, AI tagging and settings problems
    /// (`ControlService.status`).
    fn status(&self) -> Value {
        let tagging = self.mail.tagging();
        answers::json(&answers::Status {
            version: bundle::short_version().unwrap_or_else(|| "dev".into()),
            pid: std::process::id(),
            settings_file: self.settings.file().path().display().to_string(),
            settings_issues: self.settings.issues(),
            accounts: self.mail.account_statuses(),
            sync: answers::SyncStatus {
                loading: self.mail.is_loading(),
                last_refresh: self.mail.last_refresh().map(model::iso8601),
                last_error: self.mail.problem(),
                push_connected: self.push.is_connected(),
            },
            ai_tagging: answers::TaggingStatus {
                enabled: tagging.is_enabled(),
                active_here: tagging.is_active_here(),
                status: tagging.status(),
                failed: tagging.failed(),
            },
        })
    }

    /// The signed-in account named by `account`.
    fn record(&self, args: &Arguments) -> Result<accounts::AccountRecord, EngineError> {
        let id = args.required("account")?.to_lowercase();
        self.accounts.record(&id).ok_or_else(|| {
            EngineError::new("unknown_account", format!("No account {id}; `mailstream accounts list` shows them."))
        })
    }

    /// Signs in with the client given, else the suggested one. A given id that is the suggested
    /// client's takes its saved secret when none is given, so front ends can show the id without
    /// ever holding the secret.
    async fn add_account(&self, args: &Arguments) -> Result<Value, EngineError> {
        let provider = provider(args)?;
        let suggested =
            self.tokens.suggested_client(provider).map_err(|error| EngineError::failed(error.to_string()))?;
        let id = args.string("client-id").or_else(|| suggested.as_ref().map(|client| client.id.clone()));
        let secret = args.string("client-secret").or_else(|| {
            suggested.as_ref().filter(|client| Some(&client.id) == id.as_ref()).and_then(|client| client.secret.clone())
        });
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return Err(EngineError::usage(format!(
                "No OAuth client known for {}; pass --client-id (and --client-secret).",
                provider.as_str()
            )));
        };
        if provider == Provider::Google && secret.as_deref().is_none_or(str::is_empty) {
            return Err(EngineError::usage("Google needs the client secret too: --client-secret."));
        }
        let client = OAuthClient { id, secret: secret.filter(|secret| !secret.is_empty()) };
        let grant = self.oauth.sign_in(provider, &client, None).await.map_err(|error| EngineError::failed(error.message))?;
        let record = self.signed_in(provider, &client, &grant)?;
        Ok(answers::json(&record.answer()))
    }

    /// Asks the provider, in the browser, to let `account` use one more service, with the client
    /// its sign-in came from, and keeps what the person allowed. `not_granted` when that was not
    /// the service after all (unticked in Google's consent).
    async fn connect_account(&self, args: &Arguments) -> Result<Value, EngineError> {
        let record = self.record(args)?;
        let name = args.required("service")?;
        let service = Service::named(&name)
            .filter(|service| !service.scopes(record.provider).is_empty())
            .ok_or_else(|| EngineError::usage(format!("{} accounts have no {name}.", record.provider.as_str())))?;
        let client = self
            .tokens
            .client(&record.id, record.provider)
            .map_err(|error| EngineError::failed(error.to_string()))?
            .ok_or_else(|| EngineError::failed(format!("{} needs to sign in again (Settings > Accounts).", record.id)))?;
        let connect = Connect { account: &record.id, granted: &record.scopes, service };
        let grant = self
            .oauth
            .sign_in(record.provider, &client, Some(connect))
            .await
            .map_err(|error| EngineError::failed(error.message))?;
        let connected = self.signed_in(record.provider, &client, &grant)?;
        if !connected.has(service) {
            return Err(EngineError::new(
                "not_granted",
                format!("{} was not allowed {name} access; connect it again and leave its boxes ticked.", record.id),
            ));
        }
        Ok(answers::json(&connected.answer()))
    }

    /// Keeps a completed sign-in (its tokens, then the account) and tells everything that
    /// depends on what the accounts may use.
    fn signed_in(&self, provider: Provider, client: &OAuthClient, grant: &Grant) -> Result<AccountRecord, EngineError> {
        let signed_in = tokens::SignedIn { provider, scopes: grant.scopes.clone() };
        self.tokens
            .install(&grant.id, signed_in, client, &grant.refresh_token, &grant.access_token, grant.expires_in)
            .map_err(|error| EngineError::failed(error.to_string()))?;
        let record = self.accounts.signed_in(provider, grant).map_err(EngineError::failed)?;
        self.accounts_updated();
        if let Some(sync) = &self.device_sync {
            sync.account_signed_in(&grant.id);
        }
        Ok(record)
    }

    /// Tells everything that depends on what the accounts may use that they changed.
    fn accounts_updated(&self) {
        self.events.emit("accounts");
        self.mail.accounts_changed();
        self.push.check();
        self.chat.accounts_changed();
        self.calendar.accounts_changed();
    }

    /// Signs out of `id`: deletes its record, its saved sign-in and its envelope cache.
    async fn sign_out(&self, id: &str, unregister: bool) -> Result<(), EngineError> {
        // Push stops the account's registration with its API, taken before it goes, while the
        // token still works; the token goes after.
        let api = self.mail.api(id);
        if !self.accounts.remove(id).map_err(EngineError::failed)? {
            return Ok(());
        }
        self.mail.accounts_changed();
        self.chat.accounts_changed();
        self.calendar.accounts_changed();
        self.events.emit("accounts");
        self.push.sign_out(id, api, unregister).await;
        let _ = std::fs::remove_file(gmail::cache_file(&self.mail.cache_root(), id));
        self.tokens.forget(id);
        Ok(())
    }
}

/// The engine's accounts as device sync bridges them (MS-376).
impl account_sync::AccountHost for Core {
    fn keychain(&self) -> &Keychain {
        self.tokens.keychain()
    }

    fn accounts(&self) -> Vec<AccountRecord> {
        self.accounts.list()
    }

    fn adopt(
        &self,
        record: AccountRecord,
        credential: Option<account_sync::Credential>,
        push_registration: Option<&peer_sync::PushRegistration>,
    ) -> Result<(), String> {
        let account_id = record.id.clone();
        let signed_in = tokens::SignedIn { provider: record.provider, scopes: record.scopes.clone() };
        self.tokens.adopt(&account_id, signed_in, credential.as_ref()).map_err(|error| error.to_string())?;
        let changed = self.accounts.adopt(record)? || credential.is_some();
        if let Some(reg) = push_registration {
            let mut one = std::collections::BTreeMap::new();
            one.insert(account_id, reg.clone());
            self.push.adopt_registrations(&one);
        }
        if changed {
            self.accounts_updated();
        }
        Ok(())
    }

    fn remove(&self, id: String) -> futures::future::BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.sign_out(&id, false).await.map_err(|error| error.message) })
    }

    fn host_push_settings(&self) -> Option<push::PushSettings> {
        self.host_push.clone()
    }

    fn push_settings(&self) -> Option<push::PushSettings> {
        self.push.settings()
    }

    fn adopt_push_settings(&self, settings: push::PushSettings) -> Result<(), String> {
        push::write_synced(&self.support_directory, &settings)?;
        self.push.reconfigure(settings);
        Ok(())
    }

    fn push_registrations(&self) -> std::collections::BTreeMap<String, (String, String, String)> {
        self.push.registrations_snapshot()
    }

    fn adopt_push_registrations(&self, incoming: &std::collections::BTreeMap<String, peer_sync::PushRegistration>) {
        self.push.adopt_registrations(incoming);
    }
}

impl control::Handle for Core {
    fn handle(&self, command: &str, args: Value) -> impl Future<Output = Result<Value, EngineError>> + Send {
        Core::handle(self, command, args)
    }

    fn stream(&self, command: &str, args: Value) -> impl Future<Output = control::StreamReply> + Send {
        Core::stream(self, command, args)
    }
}

fn provider(args: &Arguments) -> Result<Provider, EngineError> {
    Provider::named(&args.required("provider")?)
        .ok_or_else(|| EngineError::usage("provider must be google or microsoft."))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use futures::executor::block_on;

    use super::*;
    use crate::testkit::{FakeServer, Reply, cleanup, config, free_port, redirecting_browser};

    #[test]
    fn answers_settings_commands_and_follows_the_file() {
        let config = config(OAuthConfig::default());
        let engine = Engine::start(config.clone());
        let (sender, events) = mpsc::channel();
        engine.subscribe(move |event| {
            let _ = sender.send(event);
        });
        let entry = block_on(engine.call("config.set", json!({ "key": "sync.pollSeconds", "value": "120" }))).unwrap();
        assert_eq!(entry["value"], json!(120));
        assert_eq!(events.recv_timeout(Duration::from_secs(5)), Ok("settings"));
        let error =
            block_on(engine.call("config.set", json!({ "key": "sync.pollSeconds", "value": "1" }))).unwrap_err();
        assert_eq!(error.code, "invalid_setting");
        // An edit from elsewhere (an editor, the Swift engine) is heard.
        std::thread::sleep(Duration::from_millis(300));
        while events.try_recv().is_ok() {}
        std::fs::write(config.support_directory.join("settings.json"), "{\n  \"chat.pollSeconds\" : 30\n}\n").unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(5)), Ok("settings"));
        let list = block_on(engine.call("config.list", json!({}))).unwrap();
        assert_eq!(list["settings"][11]["value"], json!(30));
        assert_eq!(list["settings"][9]["changed"], json!(false));
        drop(engine);
        cleanup(&config);
    }

    #[test]
    fn signs_in_renames_and_signs_out() {
        let id_token = format!(
            "e30.{}.sig",
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                br#"{"email":"Me@Outlook.com","name":"Me"}"#
            )
        );
        // Sign-in grants mail; connecting Calendar later grants that too.
        let answered = std::sync::atomic::AtomicUsize::new(0);
        let server = FakeServer::start(move |_| {
            let (scope, refresh_token) = match answered.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => ("openid Mail.ReadWrite", "rt"),
                _ => ("openid Mail.ReadWrite Calendars.ReadWrite", "rt2"),
            };
            Reply::json(json!({ "access_token": "at", "expires_in": 3599, "refresh_token": refresh_token, "scope": scope, "id_token": id_token }))
        });
        let port = free_port();
        let oauth = OAuthConfig {
            redirect_port: port,
            token_endpoint: Some(format!("{}/token", server.base)),
            sign_in_timeout: Duration::from_secs(10),
            open_url: redirecting_browser(port, "code=c"),
        };
        let config = config(oauth);
        let engine = Engine::start(config.clone());
        let (sender, events) = mpsc::channel();
        engine.subscribe(move |event| {
            let _ = sender.send(event);
        });
        let error = block_on(engine.call("accounts.add", json!({ "provider": "microsoft" }))).unwrap_err();
        assert_eq!(error.code, "usage");
        let added = block_on(
            engine.call("accounts.add", json!({ "provider": "microsoft", "client-id": "ms", "client-secret": null })),
        )
        .unwrap();
        assert_eq!(
            added,
            json!({ "id": "me@outlook.com", "label": "me@outlook.com", "provider": "microsoft", "name": "Me", "services": ["mail"], "connectable": ["contacts", "calendar"] })
        );
        assert_eq!(events.recv_timeout(Duration::from_secs(5)), Ok("accounts"));
        assert_eq!(
            block_on(engine.call("accounts.client", json!({ "provider": "microsoft" }))).unwrap(),
            json!({ "id": "ms", "hasSecret": false })
        );
        let renamed =
            block_on(engine.call("accounts.rename", json!({ "account": "Me@Outlook.com", "label": "Home" }))).unwrap();
        assert_eq!(renamed["label"], json!("Home"));
        let error = block_on(engine.call("accounts.remove", json!({ "account": "me@outlook.com" }))).unwrap_err();
        assert_eq!(error.code, "confirm");
        let error = block_on(engine.call("accounts.rename", json!({ "account": "x@y.z", "label": "a" }))).unwrap_err();
        assert_eq!(error.code, "unknown_account");
        let keychain = Keychain::new(config.keychain_service.clone(), config.keychain_directory.clone());
        assert_eq!(keychain.load().unwrap().refresh_tokens["me@outlook.com"], "rt");
        let error = block_on(engine.call("accounts.connect", json!({ "account": "me@outlook.com", "service": "chat" })))
            .unwrap_err();
        assert_eq!(error.code, "usage");
        let connected =
            block_on(engine.call("accounts.connect", json!({ "account": "me@outlook.com", "service": "calendar" })))
                .unwrap();
        assert_eq!((&connected["services"], &connected["connectable"]), (&json!(["mail", "calendar"]), &json!(["contacts"])));
        // Same client, the label kept, the new refresh token stored, Calendars asked of the token endpoint.
        assert_eq!(connected["label"], json!("Home"));
        assert_eq!(keychain.load().unwrap().refresh_tokens["me@outlook.com"], "rt2");
        let form = server.forms("/token")[1].clone();
        assert_eq!(form["client_id"], "ms");
        assert!(form["scope"].contains("Calendars.ReadWrite") && !form["scope"].contains("Contacts"), "{}", form["scope"]);
        block_on(engine.call("accounts.remove", json!({ "account": "me@outlook.com", "yes": true }))).unwrap();
        assert_eq!(block_on(engine.call("accounts.list", json!({}))).unwrap(), json!([]));
        assert!(keychain.load().unwrap().refresh_tokens.is_empty());
        drop(engine);
        cleanup(&config);
    }
}
