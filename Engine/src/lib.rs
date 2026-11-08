//! Mailstream's engine in Rust, replacing the Swift one (`Sources/MailstreamKit`) phase by
//! phase (MS-110). The `mailstream` crate in `FFI/` runs it instead of the Swift engine when
//! `MAILSTREAM_ENGINE=rust` is set; commands not ported yet answer `unported`.
//!
//! The engine runs on its own tokio runtime, so its host needs no main-thread run loop. Requests
//! are the control socket's commands (`config.set`, `accounts.list`, …) with their arguments as a
//! JSON object, answered with the same result values and error codes as the Swift engine. It
//! reads and writes the same files and Keychain item, so either engine picks up what the other
//! wrote.
mod accounts;
mod bundle;
mod commands;
mod files;
mod json;
mod keychain;
mod oauth;
mod settings;
mod tokens;

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
/// `unknown_account`, `confirm`, `unported`, `failed`, …) and a `message` for people.
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
    /// Holds settings.json and accounts.json: `~/Library/Application Support/Mailstream`.
    pub support_directory: PathBuf,
    /// The Keychain service of the sign-ins item; tests use their own.
    pub keychain_service: String,
    pub built_in_clients: BuiltInClients,
    pub oauth: OAuthConfig,
}

impl Config {
    /// The data, sign-ins and OAuth clients this process's app shares with the Swift engine.
    /// `MAILSTREAM_KEYCHAIN_SERVICE` names another sign-ins item, as it does for the Swift engine
    /// (benchmarks and tests that must not read the person's sign-ins).
    pub fn for_this_app() -> Self {
        let support = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Mailstream");
        let service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE").ok().filter(|service| !service.is_empty());
        Self {
            support_directory: support,
            keychain_service: service.unwrap_or_else(|| keychain::SERVICE.into()),
            built_in_clients: bundle::built_in_clients(),
            oauth: OAuthConfig::default(),
        }
    }
}

type Listener = Arc<dyn Fn(&'static str) + Send + Sync>;

/// The change events front ends re-read on: `accounts`, `settings`, ….
#[derive(Default)]
struct Events {
    listeners: Mutex<Vec<Listener>>,
}

impl Events {
    fn emit(&self, event: &'static str) {
        let listeners = self.listeners.lock().expect("events lock").clone();
        for listener in listeners {
            listener(event);
        }
    }
}

/// What the commands work on, shared by the runtime's tasks.
struct Core {
    settings: Settings,
    accounts: Accounts,
    tokens: Tokens,
    oauth: Arc<OAuth>,
    events: Events,
}

/// One engine; the FFI host keeps one per process.
pub struct Engine {
    core: Arc<Core>,
    /// Follows settings.json; dropping it stops.
    _watcher: Option<notify::RecommendedWatcher>,
    runtime: tokio::runtime::Runtime,
}

impl Engine {
    /// Reads the settings and accounts and starts following settings.json.
    pub fn start(config: Config) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("mailstream-engine")
            .enable_all()
            .build()
            .expect("the engine's runtime starts");
        let directory = &config.support_directory;
        let _ = std::fs::create_dir_all(directory);
        let accounts = Accounts::new(directory);
        let providers: HashMap<_, _> =
            accounts.list().into_iter().rev().map(|record| (record.id, record.provider)).collect();
        let oauth = Arc::new(OAuth::new(config.oauth.clone()));
        let tokens = Tokens::new(
            Keychain::new(config.keychain_service.clone()),
            oauth.clone(),
            config.built_in_clients,
            providers,
        );
        let core =
            Arc::new(Core { settings: Settings::new(directory), accounts, tokens, oauth, events: Events::default() });
        let watcher = {
            let _entered = runtime.enter();
            Self::follow_settings(&core)
        };
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

    /// Calls `listener` with each change from now on (`accounts`, `settings`), on an engine thread.
    pub fn subscribe(&self, listener: impl Fn(&'static str) + Send + Sync + 'static) {
        self.core.events.listeners.lock().expect("events lock").push(Arc::new(listener));
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
            "accounts.list" => Ok(json!(self.accounts.list().iter().map(|record| record.json()).collect::<Vec<_>>())),
            "accounts.rename" => {
                let record = self.record(&args)?;
                let renamed = self
                    .accounts
                    .rename(&record.id, &args.string("label").unwrap_or_default())
                    .map_err(EngineError::failed)?;
                if renamed.as_ref() != Some(&record) {
                    self.events.emit("accounts");
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
                // The Swift engine also discards the Gmail envelope cache and stops push first;
                // those arrive with mail (MS-113) and push (MS-115).
                self.accounts.remove(&record.id).map_err(EngineError::failed)?;
                self.tokens.forget(&record.id);
                self.events.emit("accounts");
                Ok(json!({ "removed": record.id }))
            }
            _ => unreachable!("{command} is a ported command"),
        }
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
        self.tokens
            .install(&grant.id, provider, &client, &grant.refresh_token, &grant.access_token, grant.expires_in)
            .map_err(|error| EngineError::failed(error.to_string()))?;
        let record = self.accounts.signed_in(provider, &grant).map_err(EngineError::failed)?;
        self.events.emit("accounts");
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

    fn config(oauth: OAuthConfig) -> Config {
        let name = format!("mailstream-engine-{}-{}", std::process::id(), unique());
        Config {
            support_directory: std::env::temp_dir().join(name.clone()),
            keychain_service: format!("dev.mailstream.test.{name}"),
            built_in_clients: BuiltInClients::new(),
            oauth,
        }
    }

    fn cleanup(config: &Config) {
        let _ = Keychain::new(config.keychain_service.clone()).store(&SignIns::default());
        let _ = std::fs::remove_dir_all(&config.support_directory);
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
        assert_eq!(list["settings"][9]["value"], json!(30));
        assert_eq!(list["settings"][7]["changed"], json!(false));
        assert_eq!(block_on(engine.call("mail.list", json!({}))).unwrap_err().code, "unported");
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
