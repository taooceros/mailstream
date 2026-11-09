//! Mailstream's engine in Rust, rewritten from the Swift one (`Sources/MailstreamKit`, which only
//! Mailstream.app still runs until MS-120). The `mailstream` crate in `FFI/` runs it in Mailstream
//! Flutter and Mailstream GPUI; `src/bin/mailstream.rs` is the command line.
//!
//! The engine runs on its own tokio runtime, so its host needs no main-thread run loop. Requests
//! are the control socket's commands (`config.set`, `accounts.list`, …) with their arguments as a
//! JSON object, answered with the same result values and error codes as the Swift engine. It
//! reads and writes the same files and Keychain item, so either engine picks up what the other
//! wrote.
mod accounts;
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
pub mod commands;
mod content;
pub mod control;
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
pub mod model;
mod oauth;
mod omp;
mod polling;
pub mod push;
mod settings;
pub mod signal;
pub mod tagging;
pub mod text;
mod threader;
mod tokens;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod mail_tests;
#[cfg(test)]
mod push_tests;
#[cfg(test)]
mod chat_tests;
// Runs a shell-script omp (`omp::tests::fake_omp`).
#[cfg(all(test, unix))]
mod tagging_tests;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{Map, Value, json};

use crate::accounts::Accounts;
use crate::commands::Arguments;
use crate::keychain::Keychain;
use crate::oauth::OAuth;
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
}

impl Config {
    /// The data, sign-ins and OAuth clients this process's app shares with the Swift engine.
    /// `MAILSTREAM_KEYCHAIN_SERVICE` names another sign-ins item, as it does for the Swift engine
    /// (benchmarks and tests that must not read the person's sign-ins).
    pub fn for_this_app() -> Self {
        let support = support_directory();
        let caches = cache_directory();
        let service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE").ok().filter(|service| !service.is_empty());
        Self {
            support_directory: support,
            cache_directory: caches,
            keychain_service: service.unwrap_or_else(|| keychain::SERVICE.into()),
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
        }
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
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("mailstream-engine")
            .enable_all()
            .build()
            .expect("the engine's runtime starts");
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
            Keychain::new(config.keychain_service.clone()),
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
        let push = Arc::new(push::Push::new(
            config.push.clone(),
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
        });
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
        });
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
        let (sender, mut changes) = tokio::sync::mpsc::unbounded_channel::<()>();
        let file = core.settings.file().path().to_path_buf();
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
        let core = core.clone();
        tokio::spawn(async move {
            while changes.recv().await.is_some() {
                while changes.try_recv().is_ok() {}
                if core.settings.reload() {
                    core.events.emit("settings");
                }
            }
        });
        Some(watcher)
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

    /// Whether remote images and fonts of conversation `id` (from a listing) load on their own,
    /// and the senders that keep them from it; see [`mail::Mail::remote_content`]. For the
    /// AppKit reader once it moves to this engine (MS-120); no command carries it.
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
}

impl Core {
    async fn handle(&self, command: &str, args: Value) -> Result<Value, EngineError> {
        let spec = commands::spec(command)?;
        let values = match args {
            Value::Object(values) => values,
            _ => Map::new(),
        };
        let args = Arguments::new(spec, &values)?;
        if command.starts_with("config.") {
            // The settings in effect follow at once rather than when the watcher next reads the file.
            let result = self.settings.run(command, &values)?;
            if self.settings.reload() {
                self.events.emit("settings");
            }
            return Ok(result);
        }
        match command {
            "mailboxes" | "mail.list" | "mail.search" | "mail.show" | "mail.previews" | "mail.read" | "mail.star"
            | "mail.archive" | "mail.trash" | "mail.restore" | "mail.autoread" | "mail.autoread.undo" | "mail.send"
            | "mail.reply" => {
                return self.mail.run(command, &args).await;
            }
            "refresh" => Ok(self.polling.refresh()),
            command if chat::COMMANDS.contains(&command) => {
                return self.chat.run(command, &args).await;
            }
            command if calendar::COMMANDS.contains(&command) => {
                return self.calendar.run(command, &args).await;
            }
            "mail.tag" | "mail.retag" => {
                let mail = &self.mail;
                mail.tagging().run(command, &args, |id| mail.lookup(id))
            }
            "status" => Ok(self.status()),
            "mail.avatars" => self.avatars.command(&args).await,
            "contacts.search" => {
                let avatars = self.avatars.clone();
                let people = async move {
                    avatars.people().await.into_iter().map(|person| (person.name, person.emails)).collect()
                };
                Ok(self.mail.search_contacts(&args, people).await)
            }
            "accounts.list" => Ok(json!(self.accounts.list().iter().map(|record| record.json()).collect::<Vec<_>>())),
            "accounts.rename" => {
                let record = self.record(&args)?;
                let renamed = self
                    .accounts
                    .rename(&record.id, &args.string("label").unwrap_or_default())
                    .map_err(EngineError::failed)?;
                if renamed.as_ref() != Some(&record) {
                    self.events.emit("accounts");
                    self.mail.accounts_changed();
                }
                Ok(renamed.map_or(Value::Null, |record| record.json()))
            }
            "accounts.client" => {
                let client = self
                    .tokens
                    .suggested_client(provider(&args)?)
                    .map_err(|error| EngineError::failed(error.to_string()))?;
                Ok(client.map_or(Value::Null, |client| {
                    json!({ "id": client.id, "hasSecret": client.secret.is_some_and(|secret| !secret.is_empty()) })
                }))
            }
            "accounts.add" => self.add_account(&args).await,
            "accounts.remove" => {
                let record = self.record(&args)?;
                if !args.bool("yes") {
                    return Err(EngineError::new(
                        "confirm",
                        format!(
                            "Would sign out of {} and delete its saved sign-in (mail on the server stays). Re-run with --yes.",
                            record.id
                        ),
                    ));
                }
                // Push stops the account's registration with its API, taken before it goes,
                // while the token still works; the token goes after.
                let api = self.mail.api(&record.id);
                self.accounts.remove(&record.id).map_err(EngineError::failed)?;
                self.mail.accounts_changed();
                self.chat.accounts_changed();
                self.calendar.accounts_changed();
                self.events.emit("accounts");
                self.push.sign_out(&record.id, api).await;
                let _ = std::fs::remove_file(gmail::cache_file(&self.mail.cache_root(), &record.id));
                self.tokens.forget(&record.id);
                Ok(json!({ "removed": record.id }))
            }
            _ => unreachable!("{command} is in the command table"),
        }
    }

    /// The engine's state: version, accounts, sync, push, AI tagging and settings problems
    /// (`ControlService.status`).
    fn status(&self) -> Value {
        let tagging = self.mail.tagging();
        json!({
            "version": bundle::short_version().unwrap_or_else(|| "dev".into()),
            "pid": std::process::id(),
            "settingsFile": self.settings.file().path().display().to_string(),
            "settingsIssues": self.settings.issues(),
            "accounts": self.mail.account_statuses(),
            "sync": {
                "loading": self.mail.is_loading(),
                "lastRefresh": self.mail.last_refresh().map(model::iso8601),
                "lastError": self.mail.problem(),
                "pushConnected": self.push.is_connected(),
            },
            "aiTagging": { "enabled": tagging.is_enabled(), "status": tagging.status(), "failed": tagging.failed() },
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
        let grant = self.oauth.sign_in(provider, &client).await.map_err(|error| EngineError::failed(error.message))?;
        let signed_in = tokens::SignedIn { provider, scopes: grant.scopes.clone() };
        self.tokens
            .install(&grant.id, signed_in, &client, &grant.refresh_token, &grant.access_token, grant.expires_in)
            .map_err(|error| EngineError::failed(error.to_string()))?;
        let record = self.accounts.signed_in(provider, &grant).map_err(EngineError::failed)?;
        self.events.emit("accounts");
        self.mail.accounts_changed();
        self.push.check();
        self.chat.accounts_changed();
        self.calendar.accounts_changed();
        Ok(record.json())
    }
}

fn provider(args: &Arguments) -> Result<Provider, EngineError> {
    Provider::named(&args.required("provider")?)
        .ok_or_else(|| EngineError::usage("provider must be google or microsoft."))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use futures::executor::block_on;

    use super::*;
    use crate::keychain::SignIns;
    use crate::oauth::tests::{TokenServer, free_port, redirecting_browser};

    /// A number no other test in this process gets, for temporary names.
    pub(crate) fn unique() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn config(oauth: OAuthConfig) -> Config {
        let name = format!("mailstream-engine-{}-{}", std::process::id(), unique());
        let root = std::env::temp_dir().join(name.clone());
        Config {
            support_directory: root.join("support"),
            cache_directory: root.join("caches"),
            keychain_service: format!("dev.mailstream.test.{name}"),
            built_in_clients: BuiltInClients::new(),
            oauth,
            // Nothing listens there: no test reaches a real provider.
            endpoints: backend::Endpoints {
                gmail: gmail::GmailEndpoints {
                    root: "http://127.0.0.1:9/gmail/v1/users/me/".into(),
                    batch: "http://127.0.0.1:9/batch/gmail/v1".into(),
                },
                graph: "http://127.0.0.1:9/v1.0/".into(),
                chat: "http://127.0.0.1:9/chat/v1/".into(),
                people: "http://127.0.0.1:9/people/v1/".into(),
                calendar: "http://127.0.0.1:9/calendar/v3/".into(),
            },
            // No relay: no test registers push with a provider unless it sets a fake one.
            push: push::PushConfig::default(),
            // A PATH without omp (the system's tools only): tagging runs only the fake omp a
            // test names.
            tool_environment: Some(HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])),
            // No Apple Intelligence: only a fake helper a test writes there runs.
            apple_tagger: root.join(omp::APPLE_TAGGER),
            avatar_web: Arc::new(avatars::tests::FakeWeb::default()),
            // The test of the socket turns it on.
            control_socket: false,
        }
    }

    pub(crate) fn cleanup(config: &Config) {
        let _ = Keychain::new(config.keychain_service.clone()).store(&SignIns::default());
        let _ = std::fs::remove_dir_all(config.support_directory.parent().unwrap());
    }

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
        assert_eq!(list["settings"][10]["value"], json!(30));
        assert_eq!(list["settings"][8]["changed"], json!(false));
        drop(engine);
        cleanup(&config);
    }

    #[test]
    fn signs_in_renames_and_signs_out() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let id_token = format!(
            "e30.{}.sig",
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                br#"{"email":"Me@Outlook.com","name":"Me"}"#
            )
        );
        let server = runtime.block_on(TokenServer::start(move |_| {
            (200, format!(r#"{{"access_token":"at","expires_in":3599,"refresh_token":"rt","scope":"openid Mail.ReadWrite","id_token":"{id_token}"}}"#))
        }));
        let port = free_port();
        let oauth = OAuthConfig {
            redirect_port: port,
            token_endpoint: Some(server.url.clone()),
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
            json!({ "id": "me@outlook.com", "label": "me@outlook.com", "provider": "microsoft", "name": "Me", "services": ["mail"] })
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
        let keychain = Keychain::new(config.keychain_service.clone());
        assert_eq!(keychain.load().unwrap().refresh_tokens["me@outlook.com"], "rt");
        block_on(engine.call("accounts.remove", json!({ "account": "me@outlook.com", "yes": true }))).unwrap();
        assert_eq!(block_on(engine.call("accounts.list", json!({}))).unwrap(), json!([]));
        assert!(keychain.load().unwrap().refresh_tokens.is_empty());
        drop(engine);
        cleanup(&config);
    }
}
