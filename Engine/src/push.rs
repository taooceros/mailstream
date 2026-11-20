//! Live mail changes through the push relay (`Push/worker.js`), as the Swift engine's
//! `PushService` (`Sources/MailstreamKit/Backend/PushService.swift`) gets them: one WebSocket
//! carrying Gmail Pub/Sub and Graph change notifications, plus the Gmail watches and Graph
//! subscriptions that make the providers send them. A notification asks the mail store to
//! refresh the account it names. Without push configuration (the host's items `MSPushURL`,
//! `MSPushToken`, `MSGraphClientState`, `MSGmailTopic`, as `bundle` reads them) it stays idle and
//! the store keeps polling.
//!
//! The registrations live in Application Support/Mailstream/push.json in the Swift engine's
//! format. Every process with an engine shares the file, so a pass reads it afresh before it
//! changes anything: a subscription another process made is renewed rather than made again.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::Provider;
use crate::accounts::Accounts;
use crate::api::{Api, ApiError, Priority, Request, percent_encoded};
use crate::mail::Mail;
use crate::model::{Date, date_from_unix, iso8601, now, parse_iso8601};
use crate::signal::ProcessSignal;

/// Requested lifetime of a Graph subscription (70 h); renewed with a day left.
const GRAPH_LIFETIME: f64 = 4200.0 * 60.0;
/// A registration closer than this to expiring is renewed.
const RENEW_WITHIN: f64 = 24.0 * 3600.0;
/// How long the socket's handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The relay and what the registrations name, from the host app's build.
#[derive(Clone, Debug, Default)]
pub struct PushConfig {
    /// The relay, `https://…workers.dev`.
    pub url: Option<String>,
    /// Authorizes the app's socket.
    pub token: Option<String>,
    /// The `clientState` of the Graph subscriptions, which the relay checks.
    pub graph_client_state: Option<String>,
    /// The Pub/Sub topic Gmail watches publish to.
    pub gmail_topic: Option<String>,
    pub timing: PushTiming,
}


/// Portable push relay settings synced across paired devices (MS-444), omitting local timing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_client_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gmail_topic: Option<String>,
}

impl PushSettings {
    pub fn is_configured(&self) -> bool {
        PushConfig::configured(&self.url).is_some() && PushConfig::configured(&self.token).is_some()
    }
}

const SYNCED_CONFIG_FILE: &str = "push-config.json";

/// Reads push relay settings previously synced from a paired device (`peer-sync/push-config.json`).
pub(crate) fn read_synced(support_directory: &Path) -> Option<PushSettings> {
    let path = support_directory.join("peer-sync").join(SYNCED_CONFIG_FILE);
    let data = std::fs::read(path).ok()?;
    let settings: PushSettings = serde_json::from_slice(&data).ok()?;
    settings.is_configured().then_some(settings)
}

/// Writes push relay settings received from a paired device (`peer-sync/push-config.json`, 0600).
pub(crate) fn write_synced(support_directory: &Path, settings: &PushSettings) -> Result<(), String> {
    let dir = support_directory.join("peer-sync");
    let path = dir.join(SYNCED_CONFIG_FILE);
    let data = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    crate::files::replace(&dir, &path, &data).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
/// The socket's and the registrations' clocks; tests shorten them.
#[derive(Clone, Debug)]
pub struct PushTiming {
    /// Between pings.
    pub ping: Duration,
    /// A socket that says nothing this long after a ping is dead.
    pub silence: Duration,
    /// The first reconnection's delay; each failure doubles it, up to `backoff_max`.
    pub backoff: Duration,
    pub backoff_max: Duration,
    /// Between registration checks.
    pub renewal: Duration,
    /// Between looks at the clocks for a sleep.
    pub wake_check: Duration,
}

impl Default for PushTiming {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(30),
            silence: Duration::from_secs(15),
            backoff: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            renewal: Duration::from_secs(3600),
            wake_check: Duration::from_secs(10),
        }
    }
}

impl PushConfig {
    fn configured(text: &Option<String>) -> Option<&str> {
        text.as_deref().map(str::trim).filter(|text| !text.is_empty())
    }

    pub(crate) fn socket_configured(&self) -> bool {
        Self::configured(&self.url).is_some() && Self::configured(&self.token).is_some()
    }

    /// Portable relay settings (trimmed; empty strings normalized to `None`).
    pub(crate) fn settings(&self) -> PushSettings {
        PushSettings {
            url: Self::configured(&self.url).map(String::from),
            token: Self::configured(&self.token).map(String::from),
            graph_client_state: Self::configured(&self.graph_client_state).map(String::from),
            gmail_topic: Self::configured(&self.gmail_topic).map(String::from),
        }
    }

    /// A copy with `settings` applied, preserving local [`PushTiming`].
    pub(crate) fn with_settings(&self, settings: &PushSettings) -> Self {
        Self {
            url: Self::configured(&settings.url).map(String::from),
            token: Self::configured(&settings.token).map(String::from),
            graph_client_state: Self::configured(&settings.graph_client_state).map(String::from),
            gmail_topic: Self::configured(&settings.gmail_topic).map(String::from),
            timing: self.timing.clone(),
        }
    }
    /// `path` on the relay.
    fn endpoint(&self, path: &str) -> Option<String> {
        Some(format!("{}/{path}", Self::configured(&self.url)?.trim_end_matches('/')))
    }

    /// The relay's `/socket` as a WebSocket URL: `https` becomes `wss` (the Swift engine always
    /// says `wss`; the relay is only served over https), plain `http` (a local relay) `ws`.
    fn socket_url(&self) -> Option<String> {
        let url = self.endpoint("socket")?;
        Some(if let Some(rest) = url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            url
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Gmail,
    Graph,
}

/// A Gmail watch (`id` is its Pub/Sub topic) or a Graph subscription (`id` is the subscription id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Registration {
    kind: Kind,
    id: String,
    /// `JSONEncoder`'s `.iso8601`: whole seconds in UTC.
    #[serde(with = "iso_date")]
    expires: Date,
}

impl Registration {
    /// Closer than a day to expiring: renew.
    fn due(&self) -> bool {
        self.expires - now() < RENEW_WITHIN
    }
}

mod iso_date {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::model::{Date, iso8601, parse_iso8601};

    pub fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&iso8601(*date))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_iso8601(&text).ok_or_else(|| serde::de::Error::custom(format!("not an ISO 8601 date: {text}")))
    }
}

type Registrations = HashMap<String, Registration>;

#[derive(Default)]
struct State {
    registrations: Registrations,
    /// The API of every account seen, kept so a removed account's registration can still be stopped.
    apis: HashMap<String, Arc<Api>>,
    /// Graph accounts whose subscription asked to be reauthorized.
    reauthorize: HashSet<String>,
    /// Accounts whose last registration failure was already logged.
    reported: HashSet<String>,
}

/// Why a socket session ended.
enum Ended {
    /// It failed or went quiet: reconnect after a backoff.
    Failed,
    /// A wake or a network change asked for a fresh socket now.
    Reconnect,
}

pub struct Push {
    config: Mutex<PushConfig>,
    timing: PushTiming,
    directory: PathBuf,
    file: PathBuf,
    gmail_root: String,
    graph_root: String,
    accounts: Arc<Accounts>,
    mail: Arc<Mail>,
    connected: watch::Sender<bool>,
    started: AtomicBool,
    reconnect: Notify,
    state: Mutex<State>,
    /// Called after local registration changes so device sync can share them across devices.
    on_registrations: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Registration passes asked for; one runs at a time, and one asked for during a pass runs
    /// after it.
    requested: AtomicU64,
    /// The last request the latest finished pass covered.
    passes: tokio::sync::Mutex<u64>,
}

impl Push {
    pub fn new(
        config: PushConfig,
        directory: PathBuf,
        endpoints: &crate::backend::Endpoints,
        accounts: Arc<Accounts>,
        mail: Arc<Mail>,
    ) -> Self {
        let file = directory.join("push.json");
        let registrations = read(&file);
        let timing = config.timing.clone();
        Self {
            config: Mutex::new(config),
            timing,
            directory,
            file,
            gmail_root: endpoints.gmail.root.clone(),
            graph_root: endpoints.graph.clone(),
            accounts,
            mail,
            connected: watch::channel(false).0,
            started: AtomicBool::new(false),
            reconnect: Notify::new(),
            state: Mutex::new(State { registrations, ..State::default() }),
            on_registrations: Mutex::new(None),
            requested: AtomicU64::new(0),
            passes: tokio::sync::Mutex::new(0),
        }
    }

    fn config(&self) -> PushConfig {
        self.config.lock().expect("push config").clone()
    }

    /// Current portable push settings, if configured.
    pub fn settings(&self) -> Option<PushSettings> {
        let settings = self.config().settings();
        settings.is_configured().then_some(settings)
    }

    /// Registers a listener invoked whenever local push registrations change (MS-444).
    pub fn on_registrations(&self, handler: impl Fn() + Send + Sync + 'static) {
        *self.on_registrations.lock().expect("push registrations listener") = Some(Arc::new(handler));
    }

    fn notify_registrations(&self) {
        if let Some(handler) = self.on_registrations.lock().expect("push registrations listener").clone() {
            handler();
        }
    }

    /// Applies updated push relay settings at runtime (MS-444): hot-starts `Push` when it was
    /// previously unconfigured, or reconnects and re-registers when settings changed.
    pub fn reconfigure(self: &Arc<Self>, settings: PushSettings) {
        let (changed_socket, configured) = {
            let mut guard = self.config.lock().expect("push config");
            let previous = guard.settings();
            let next = guard.with_settings(&settings);
            let normalized = next.settings();
            if previous == normalized {
                return;
            }
            let changed_socket = previous.url != normalized.url || previous.token != normalized.token;
            let configured = next.socket_configured();
            *guard = next;
            (changed_socket, configured)
        };
        if !configured {
            return;
        }
        if !self.started.load(Ordering::SeqCst) {
            self.start();
        } else {
            if changed_socket {
                self.reconnect_now();
            }
            self.check();
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("push lock")
    }

    /// The socket is open and the relay answered on it.
    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }

    /// Follows `is_connected`.
    pub fn connections(&self) -> watch::Receiver<bool> {
        self.connected.subscribe()
    }

    /// Connects the socket and registers every account; reconnects after a wake and when the
    /// network comes back, and renews registrations hourly. Nothing without push configuration.
    /// Call on the engine's runtime.
    pub fn start(self: &Arc<Self>) {
        if !self.config().socket_configured() || self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        tokio::spawn(self.clone().run_socket());
        let push = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(push.timing.renewal).await;
                push.check();
            }
        });
        let push = self.clone();
        tokio::spawn(async move {
            loop {
                crate::signal::next_wake(push.timing.wake_check).await;
                push.reconnect_now();
                push.check();
            }
        });
        let push = self.clone();
        tokio::spawn(async move {
            let Some(mut changes) = ProcessSignal::NETWORK_CHANGE.posts() else { return };
            // A socket that is up stays; one that is down (the network was gone) retries now
            // rather than at the end of its backoff.
            while changes.next().await {
                if !push.is_connected() {
                    push.reconnect_now();
                }
            }
        });
        self.check();
    }

    /// Runs a registration pass soon: after account changes, a wake, the hourly renewal.
    pub fn check(self: &Arc<Self>) {
        if !self.started.load(Ordering::SeqCst) {
            return;
        }
        let push = self.clone();
        tokio::spawn(async move { push.settle().await });
    }

    /// Drops a removed account's registration; when `unregister` is true, also tells Gmail/Graph
    /// to stop/delete it while its token still works (call after the account left accounts.json
    /// and before its token goes). When `unregister` is false (a removal synced from another
    /// device that already stopped it, or a local sign-out while sync is paused), only the local
    /// `push.json` entry is removed so other devices' shared watch/subscription stays intact
    /// (MS-444).
    pub async fn sign_out(&self, account: &str, api: Option<Arc<Api>>, unregister: bool) {
        if !unregister {
            self.state().apis.remove(account);
            self.update_silent(|registrations| {
                registrations.remove(account);
            });
            return;
        }
        if !self.started.load(Ordering::SeqCst) {
            return;
        }
        if let Some(api) = api {
            self.state().apis.entry(account.to_string()).or_insert(api);
        }
        self.settle().await;
    }

    /// Returns once a registration pass that started after this call has finished.
    async fn settle(&self) {
        let ticket = self.requested.fetch_add(1, Ordering::SeqCst) + 1;
        let mut covered = self.passes.lock().await;
        if *covered >= ticket {
            return;
        }
        let upto = self.requested.load(Ordering::SeqCst);
        self.reconcile().await;
        *covered = upto;
    }

    fn reconnect_now(&self) {
        self.reconnect.notify_one();
    }

    fn set_connected(&self, connected: bool) {
        self.connected.send_if_modified(|current| {
            let changed = *current != connected;
            *current = connected;
            changed
        });
    }

    // MARK: - Socket

    /// Keeps a socket open: after a failure it reconnects after 1 s, doubling to at most 60 s,
    /// with jitter so many clients do not reconnect in step.
    async fn run_socket(self: Arc<Self>) {
        let mut failures = 0u32;
        loop {
            let ended = self.session(&mut failures).await;
            self.set_connected(false);
            match ended {
                Ended::Reconnect => failures = 0,
                Ended::Failed => {
                    let delay = backoff(&self.timing, failures, jitter());
                    failures += 1;
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = self.reconnect.notified() => failures = 0,
                    }
                }
            }
        }
    }

    /// One socket from connecting to its end. Pings every `ping`; a socket that says nothing
    /// within `silence` of a ping is dead. The first ping goes out at once, so its pong is what
    /// marks the socket connected.
    async fn session(self: &Arc<Self>, failures: &mut u32) -> Ended {
        let config = self.config();
        let (Some(url), Some(token)) = (config.socket_url(), PushConfig::configured(&config.token).map(String::from)) else {
            return std::future::pending().await;
        };
        let Ok(mut request) = url.into_client_request() else { return Ended::Failed };
        let Ok(authorization) = format!("Bearer {token}").parse() else { return Ended::Failed };
        request.headers_mut().insert("Authorization", authorization);
        let stream = tokio::select! {
            connected = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)) => match connected {
                Ok(Ok((stream, _))) => stream,
                _ => return Ended::Failed,
            },
            () = self.reconnect.notified() => return Ended::Reconnect,
        };
        let (mut write, mut read) = stream.split();
        let timing = &self.timing;
        let mut heard: Option<Instant> = None;
        let mut next_ping = Instant::now();
        // When the ping awaiting an answer went out.
        let mut sent: Option<Instant> = None;
        loop {
            let wake = sent.map_or(next_ping, |sent| next_ping.min(sent + timing.silence));
            tokio::select! {
                message = read.next() => {
                    let Some(Ok(message)) = message else { return Ended::Failed };
                    heard = Some(Instant::now());
                    if !self.is_connected() {
                        *failures = 0;
                        self.set_connected(true);
                    }
                    match message {
                        Message::Text(text) if text.as_str() == "pong" => {}
                        Message::Text(text) => self.handle(text.as_bytes()),
                        Message::Binary(data) => self.handle(&data),
                        Message::Close(_) => return Ended::Failed,
                        _ => {}
                    }
                }
                () = tokio::time::sleep_until(wake) => {
                    let now = Instant::now();
                    if let Some(at) = sent && now >= at + timing.silence {
                        if heard.is_none_or(|heard| heard < at) {
                            return Ended::Failed;
                        }
                        sent = None;
                    }
                    if now >= next_ping {
                        if write.send(Message::text("ping")).await.is_err() {
                            return Ended::Failed;
                        }
                        // The earliest ping not yet checked is the one that counts.
                        sent.get_or_insert(now);
                        next_ping = now + timing.ping;
                    }
                }
                () = self.reconnect.notified() => {
                    let _ = write.send(Message::Close(None)).await;
                    return Ended::Reconnect;
                }
            }
        }
    }

    // MARK: - Events

    /// One notification from the relay: `{source: "gmail", email}` or `{source: "graph",
    /// subscriptionId, lifecycleEvent}`.
    fn handle(self: &Arc<Self>, data: &[u8]) {
        #[derive(Deserialize)]
        struct Event {
            source: String,
            email: Option<String>,
            #[serde(rename = "subscriptionId")]
            subscription_id: Option<String>,
            #[serde(rename = "lifecycleEvent")]
            lifecycle_event: Option<String>,
        }
        let Ok(event) = serde_json::from_slice::<Event>(data) else { return };
        match event.source.as_str() {
            "gmail" => {
                let Some(email) = event.email.map(|email| email.to_lowercase()) else { return };
                if self.accounts.record(&email).is_some() {
                    self.mail.refresh_account(&email);
                }
            }
            "graph" => {
                let Some(id) = event.subscription_id else { return };
                let Some(account) = self.graph_account(&id) else { return };
                if self.accounts.record(&account).is_none() {
                    return;
                }
                match event.lifecycle_event.as_deref() {
                    Some("reauthorizationRequired") => {
                        self.state().reauthorize.insert(account);
                        self.check();
                    }
                    Some("subscriptionRemoved") => {
                        self.update(|registrations| {
                            registrations.remove(&account);
                        });
                        self.check();
                        self.mail.refresh_account(&account);
                    }
                    _ => self.mail.refresh_account(&account),
                }
            }
            _ => {}
        }
    }

    /// The account whose Graph subscription `id` is; another process may have made it.
    fn graph_account(&self, id: &str) -> Option<String> {
        let find = |registrations: &Registrations| {
            registrations
                .iter()
                .find(|(_, registration)| registration.kind == Kind::Graph && registration.id == id)
                .map(|(account, _)| account.clone())
        };
        if let Some(account) = find(&self.state().registrations) {
            return Some(account);
        }
        let fresh = read(&self.file);
        let account = find(&fresh);
        self.state().registrations = fresh;
        account
    }

    // MARK: - Registrations

    /// The registrations as push.json has them now, also kept in memory.
    fn reload(&self) {
        let fresh = read(&self.file);
        self.state().registrations = fresh;
    }

    /// Changes push.json as it is now, writes it back, and wakes listeners when it changed.
    fn update(&self, change: impl FnOnce(&mut Registrations)) {
        if self.update_silent(change) {
            self.notify_registrations();
        }
    }

    /// Changes push.json as it is now and writes it back without notifying sync listeners;
    /// returns whether `push.json` actually changed.
    fn update_silent(&self, change: impl FnOnce(&mut Registrations)) -> bool {
        let mut state = self.state();
        let before = read(&self.file);
        let mut next = before.clone();
        change(&mut next);
        state.registrations = next.clone();
        if next == before {
            return false;
        }
        let value = serde_json::to_value(&next).expect("registrations encode");
        if let Err(error) = crate::files::replace(&self.directory, &self.file, &crate::json::encode(&value, true, true)) {
            eprintln!("Could not save push registrations: {error}");
        }
        true
    }

    /// Current registrations for signed-in accounts as `(kind, id, expires_iso)` for device sync.
    pub(crate) fn registrations_snapshot(&self) -> BTreeMap<String, (String, String, String)> {
        self.reload();
        let state = self.state();
        state
            .registrations
            .iter()
            .filter(|(account, _)| self.accounts.record(account).is_some())
            .map(|(account, reg)| {
                let kind = match reg.kind {
                    Kind::Gmail => "gmail",
                    Kind::Graph => "graph",
                };
                (account.clone(), (kind.to_string(), reg.id.clone(), iso8601(reg.expires)))
            })
            .collect()
    }

    /// Adopts fresher shared registrations from paired devices into `push.json` so devices reuse
    /// one Graph subscription id and avoid redundant Gmail watch calls (MS-444).
    pub(crate) fn adopt_registrations(&self, incoming: &BTreeMap<String, crate::peer_sync::PushRegistration>) -> bool {
        let config = self.config();
        let gmail_topic = PushConfig::configured(&config.gmail_topic).map(String::from);
        let current_time = now();
        self.update_silent(|registrations| {
            for (account, remote) in incoming {
                if self.accounts.record(account).is_none() {
                    continue;
                }
                let kind = match remote.kind.as_str() {
                    "gmail" => Kind::Gmail,
                    "graph" => Kind::Graph,
                    _ => continue,
                };
                if kind == Kind::Gmail
                    && let Some(topic) = &gmail_topic
                    && topic != &remote.id
                {
                    continue;
                }
                let Some(expires) = parse_iso8601(&remote.expires) else { continue };
                if expires <= current_time {
                    continue;
                }
                let adopt = match registrations.get(account) {
                    None => true,
                    Some(local) => {
                        local.kind != kind
                            || expires > local.expires
                            || (local.due() && expires >= local.expires)
                    }
                };
                if adopt {
                    registrations.insert(account.clone(), Registration { kind, id: remote.id.clone(), expires });
                }
            }
        })
    }
    fn registration(&self, account: &str) -> Option<Registration> {
        self.state().registrations.get(account).cloned()
    }

    async fn reconcile(&self) {
        self.reload();
        let removed: Vec<(String, Registration)> = self
            .state()
            .registrations
            .iter()
            .filter(|(account, _)| self.accounts.record(account).is_none())
            .map(|(account, registration)| (account.clone(), registration.clone()))
            .collect();
        for (account, registration) in removed {
            self.update(|registrations| {
                registrations.remove(&account);
            });
            let api = self.state().apis.get(&account).cloned().or_else(|| self.mail.api(&account));
            if let Some(api) = api {
                self.unregister(&registration, &api).await;
            }
        }
        self.state().apis.retain(|account, _| self.accounts.record(account).is_some());
        for record in self.accounts.list() {
            let Some(api) = self.mail.api(&record.id) else { continue };
            self.state().apis.insert(record.id.clone(), api.clone());
            let result = match record.provider {
                Provider::Google => self.watch_gmail(&record.id, &api).await,
                Provider::Microsoft => self.subscribe_graph(&record.id, &api).await,
            };
            match result {
                Ok(()) => {
                    self.state().reported.remove(&record.id);
                }
                Err(error) => {
                    if self.state().reported.insert(record.id.clone()) {
                        eprintln!("Push registration for {} failed: {}", record.id, error.message);
                    }
                }
            }
        }
    }

    /// Watches every change of the mailbox; renewing is the same call.
    async fn watch_gmail(&self, account: &str, api: &Api) -> Result<(), ApiError> {
        let Some(topic) = PushConfig::configured(&self.config().gmail_topic).map(String::from) else { return Ok(()) };
        if let Some(current) = self.registration(account)
            && current.kind == Kind::Gmail
            && current.id == topic
            && !current.due()
        {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Answer {
            expiration: String,
        }
        let body = serde_json::to_vec(&json!({ "topicName": topic })).expect("watch encodes");
        let request = Request::post(format!("{}watch", self.gmail_root), "application/json", body);
        let answer: Answer = api.decode(request, Priority::Background, 100).await?;
        let milliseconds: f64 =
            answer.expiration.parse().map_err(|_| ApiError::malformed("Gmail watch answered no expiration"))?;
        let registration = Registration { kind: Kind::Gmail, id: topic, expires: date_from_unix(milliseconds / 1000.0) };
        self.update(|registrations| {
            registrations.insert(account.to_string(), registration);
        });
        Ok(())
    }

    /// Renews a subscription that is due or asked for reauthorization; creates one when there is
    /// none or Graph no longer knows it.
    async fn subscribe_graph(&self, account: &str, api: &Api) -> Result<(), ApiError> {
        let config = self.config();
        let (Some(endpoint), Some(client_state)) =
            (config.endpoint("graph"), PushConfig::configured(&config.graph_client_state).map(String::from))
        else {
            return Ok(());
        };
        let forced = self.state().reauthorize.remove(account);
        if let Some(current) = self.registration(account)
            && current.kind == Kind::Graph
        {
            if !forced && !current.due() {
                return Ok(());
            }
            match self.renew_graph(account, &current.id, api).await {
                Ok(()) => return Ok(()),
                Err(error) if error.status == 404 => self.update(|registrations| {
                    registrations.remove(account);
                }),
                Err(error) => {
                    if forced {
                        self.state().reauthorize.insert(account.to_string());
                    }
                    return Err(error);
                }
            }
        }
        let expires = now() + GRAPH_LIFETIME;
        let body = json!({
            "changeType": "created,updated,deleted", "notificationUrl": endpoint, "lifecycleNotificationUrl": endpoint,
            "resource": "me/messages", "expirationDateTime": iso8601(expires), "clientState": client_state,
        });
        let request = Request::post(
            format!("{}subscriptions", self.graph_root),
            "application/json",
            serde_json::to_vec(&body).expect("subscription encodes"),
        );
        let answer: GraphSubscription = api.decode(request, Priority::Background, 0).await?;
        let expires = answer.expiration_date_time.as_deref().and_then(parse_iso8601).unwrap_or(expires);
        let registration = Registration { kind: Kind::Graph, id: answer.id, expires };
        self.update(|registrations| {
            registrations.insert(account.to_string(), registration);
        });
        Ok(())
    }

    async fn renew_graph(&self, account: &str, id: &str, api: &Api) -> Result<(), ApiError> {
        let expires = now() + GRAPH_LIFETIME;
        let body = serde_json::to_vec(&json!({ "expirationDateTime": iso8601(expires) })).expect("renewal encodes");
        let request = Request {
            method: reqwest::Method::PATCH,
            url: format!("{}subscriptions/{}", self.graph_root, percent_encoded(id)),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(body),
        };
        let answer: GraphSubscription = api.decode(request, Priority::Background, 0).await?;
        let expires = answer.expiration_date_time.as_deref().and_then(parse_iso8601).unwrap_or(expires);
        let registration = Registration { kind: Kind::Graph, id: id.to_string(), expires };
        self.update(|registrations| {
            registrations.insert(account.to_string(), registration);
        });
        Ok(())
    }

    /// Best effort: the account's token may already be gone, and registrations expire on their own.
    async fn unregister(&self, registration: &Registration, api: &Api) {
        let request = match registration.kind {
            Kind::Gmail => Request {
                method: reqwest::Method::POST,
                url: format!("{}stop", self.gmail_root),
                headers: Vec::new(),
                body: None,
            },
            Kind::Graph => Request {
                method: reqwest::Method::DELETE,
                url: format!("{}subscriptions/{}", self.graph_root, percent_encoded(&registration.id)),
                headers: Vec::new(),
                body: None,
            },
        };
        // `stop` costs 50 units on Gmail, left unbudgeted so sign-out never blocks on a paused bucket.
        let _ = api.send(request, Priority::Background, Some(true), 0).await;
    }
}

#[derive(Deserialize)]
struct GraphSubscription {
    id: String,
    #[serde(rename = "expirationDateTime")]
    expiration_date_time: Option<String>,
}

/// push.json's registrations; none when it is missing or does not decode.
fn read(file: &std::path::Path) -> Registrations {
    std::fs::read(file).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
}

/// The wait before reconnection attempt `failures` + 1: `backoff` × 2^failures (at most 2^6),
/// times `jitter` (0.8…1.2), at most `backoff_max`.
fn backoff(timing: &PushTiming, failures: u32, jitter: f64) -> Duration {
    let factor = f64::from(1u32 << failures.min(6)) * jitter;
    timing.backoff.mul_f64(factor).min(timing.backoff_max)
}

/// A factor in 0.8…1.2.
fn jitter() -> f64 {
    let random = getrandom::u32().unwrap_or(u32::MAX / 2);
    0.8 + 0.4 * f64::from(random) / f64::from(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_from_a_second_to_a_minute_with_jitter() {
        let timing = PushTiming::default();
        assert_eq!(backoff(&timing, 0, 1.0), Duration::from_secs(1));
        assert_eq!(backoff(&timing, 3, 1.0), Duration::from_secs(8));
        assert_eq!(backoff(&timing, 5, 1.2), Duration::from_secs_f64(38.4));
        assert_eq!(backoff(&timing, 6, 0.8), Duration::from_secs_f64(51.2));
        assert_eq!(backoff(&timing, 40, 1.2), Duration::from_secs(60));
        for _ in 0..100 {
            assert!((0.8..=1.2).contains(&jitter()));
        }
    }

    #[test]
    fn names_the_relay_s_endpoints() {
        let config = PushConfig {
            url: Some("https://mailstream-push.example.workers.dev/".into()),
            token: Some("t".into()),
            ..PushConfig::default()
        };
        assert!(config.socket_configured());
        assert_eq!(config.socket_url().unwrap(), "wss://mailstream-push.example.workers.dev/socket");
        assert_eq!(config.endpoint("graph").unwrap(), "https://mailstream-push.example.workers.dev/graph");
        let local = PushConfig { url: Some("http://127.0.0.1:8787".into()), ..PushConfig::default() };
        assert!(!local.socket_configured(), "no token, no socket");
        assert_eq!(local.socket_url().unwrap(), "ws://127.0.0.1:8787/socket");
    }

    #[test]
    fn reads_what_the_swift_engine_wrote() {
        let file = std::env::temp_dir().join(format!("mailstream-push-{}-{}.json", std::process::id(), crate::testkit::unique()));
        std::fs::write(
            &file,
            "{\n  \"a@outlook.com\" : {\n    \"expires\" : \"2025-11-10T12:00:00Z\",\n    \"id\" : \"sub\",\n    \"kind\" : \"graph\"\n  }\n}",
        )
        .unwrap();
        let read = read(&file);
        let registration = &read["a@outlook.com"];
        assert_eq!((registration.kind, registration.id.as_str()), (Kind::Graph, "sub"));
        assert_eq!(iso8601(registration.expires), "2025-11-10T12:00:00Z");
        assert!(registration.due(), "long past");
        assert!(!Registration { kind: Kind::Gmail, id: "t".into(), expires: now() + 2.0 * 86_400.0 }.due());
        std::fs::remove_file(file).unwrap();
    }
}
