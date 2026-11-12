//! The Settings window (Flutter's `settings.dart`): the accounts (sign in with your own OAuth
//! client, rename, remove), every setting of settings.json in Flutter's groups, what auto-read
//! marked read (with undo), and the engine's status.
//!
//! Settings go through the engine (`settings`, `set_setting`, `reset_setting`): settings.json is the
//! file Mailstream.app, Flutter and `mailstream config` share, and the window reads it again on the
//! engine's `Settings` event, whoever changed it. The `flutter.*` keys configure only the Flutter
//! app and stay out of this window.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Local, Utc};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Bounds, Context, Div, ElementId, Entity, Focusable, FontWeight, Global,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, TitlebarOptions, Window, WeakEntity, WindowBounds, WindowOptions, actions, div, px, size,
};
use mailstream::{AutoReadLog, Change, Event, EventListener, OAuthClientInfo, SettingEntry, SettingValue};

use crate::controls::{ButtonKind, ControlSize, Tone, button, field_frame, icon_button, notice, pill, switch, tag};
use crate::design::{self, Palette, radius, settings as metrics, stroke, text};
use crate::engine::{MailError, engine};
use crate::format;
use crate::icons::{Icon, icon};
use crate::input;

actions!(settings, [ShowSettings]);

/// Binds cmd-, to `ShowSettings`, which opens the window with `on_changed` (see `open_settings`),
/// and hands the engine's change events to the window while it is open.
pub fn init(on_changed: impl Fn(&mut App) + 'static, cx: &mut App) {
    cx.bind_keys([KeyBinding::new(secondary!(","), ShowSettings, None)]);
    let on_changed = Rc::new(on_changed);
    cx.on_action(move |_: &ShowSettings, cx| {
        let on_changed = on_changed.clone();
        open_settings(move |cx| on_changed(cx), cx);
    });
    follow_engine(cx);
}

/// Opens the Settings window, or brings the open one forward. `on_changed` runs after an account
/// is renamed, removed or added, a setting changes or auto-read is undone, so the main window can
/// reload.
pub fn open_settings(on_changed: impl Fn(&mut App) + 'static, cx: &mut App) {
    if let Some(open) = cx.try_global::<OpenSettings>() {
        let handle = open.0;
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let on_changed: Rc<dyn Fn(&mut App)> = Rc::new(on_changed);
    let bounds = Bounds::centered(None, size(px(metrics::WIDTH), px(metrics::HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("Settings".into()), ..Default::default() }),
        window_min_size: Some(size(px(metrics::WIDTH), px(360.))),
        ..Default::default()
    };
    let Ok((window, settings)) =
        gpui::open_window(options, cx, |window, cx| cx.new(|cx| Settings::new(on_changed, window, cx)))
    else {
        return;
    };
    cx.set_global(OpenSettings(window, settings.downgrade()));
    window.update(cx, |_, window, _| window.activate_window()).ok();
}

/// The open Settings window (in gpui-kit's root) and its view.
struct OpenSettings(AnyWindowHandle, WeakEntity<Settings>);

impl Global for OpenSettings {}

/// One listener for the process (the engine keeps every listener for good): it passes the events
/// to the open window, if any.
fn follow_engine(cx: &mut App) {
    let (sender, mut events) = unbounded();
    // The listener only queues the event, so it runs on the engine's thread (see `MailApp`).
    engine().subscribe_any_thread(Arc::new(Changes(sender)));
    cx.spawn(async move |cx| {
        while let Some(first) = events.next().await {
            let mut batch = vec![first];
            while let Ok(event) = events.try_recv() {
                batch.push(event);
            }
            let open = cx.update(|cx| cx.try_global::<OpenSettings>().map(|open| open.1.clone()));
            if let Some(settings) = open {
                settings.update(cx, |settings, cx| settings.follow_engine(&batch, cx)).ok();
            }
        }
    })
    .detach();
}

struct Changes(UnboundedSender<Event>);

impl EventListener for Changes {
    fn on_event(&self, change: Change) {
        let _ = self.0.unbounded_send(change.event);
    }
}

// MARK: - Values

#[derive(Clone)]
struct AccountRow {
    id: SharedString,
    label: SharedString,
    provider: SharedString,
    name: Option<SharedString>,
    services: Vec<SharedString>,
}

impl From<mailstream::Account> for AccountRow {
    fn from(account: mailstream::Account) -> Self {
        Self {
            id: account.id.into(),
            label: account.label.into(),
            provider: account.provider.into(),
            name: account.name.map(SharedString::from),
            services: account.services.into_iter().map(SharedString::from).collect(),
        }
    }
}

impl AccountRow {
    fn title(&self) -> SharedString {
        if self.label.is_empty() { self.id.clone() } else { self.label.clone() }
    }

    fn provider_name(&self) -> &str {
        match self.provider.as_ref() {
            "google" => "Google",
            "microsoft" => "Microsoft",
            other => other,
        }
    }
}

/// What the Status section shows of the engine's `status`.
struct Status {
    version: String,
    loading: bool,
    last_refresh: Option<DateTime<Utc>>,
    last_error: Option<String>,
    push_connected: bool,
    tagging: mailstream::TaggingStatus,
}

impl From<mailstream::Status> for Status {
    fn from(status: mailstream::Status) -> Self {
        let sync = status.sync;
        Self {
            version: status.version,
            loading: sync.loading,
            last_refresh: sync
                .last_refresh
                .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
                .map(|at| at.with_timezone(&Utc)),
            last_error: sync.last_error,
            push_connected: sync.push_connected,
            tagging: status.ai_tagging,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    fn key(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }
}

/// A note under the accounts: what the last action did.
struct Note {
    text: SharedString,
    error: bool,
}

// MARK: - Keys

/// Sections of settings.json by key prefix, in the order they show (Flutter's `_sections`, less
/// its own `flutter.*` ones). Keys under none of them follow in a group without a title.
const SECTIONS: &[(&str, &[&str])] = &[
    ("Sync", &["sync.", "chat."]),
    ("AI tagging", &["aiTagging."]),
    ("Auto-read", &["autoRead."]),
    ("Remote content", &["remoteContent."]),
];

/// Keys only the Flutter app reads.
const FLUTTER_ONLY: &str = "flutter.";

const AI_TAGGING: &str = "aiTagging.enabled";
const SYNC_ACCOUNTS: &str = "aiTagging.syncAccounts";
const AUTO_READ: &str = "autoRead.enabled";
const AUTO_READ_DAYS: &str = "autoRead.days";
/// Where the auto-read log's error goes in `Settings::errors`.
const AUTO_READ_LOG: &str = "autoRead.log";
/// Suffix of the field that adds to an address setting, in `Settings::fields`.
const ADD: &str = "+add";
/// Rows of the auto-read log shown; the rest stay in the engine's log (`mailstream mail autoread`).
const AUTO_READ_ROWS: usize = 50;

/// Short names for the keys (Flutter's `_titles`); the engine's description sits under each.
fn title(key: &str) -> &str {
    match key {
        "aiTagging.enabled" => "Sort and flag mail with AI",
        "aiTagging.backend" => "Tagged by",
        "aiTagging.model" => "Model",
        "aiTagging.ompPath" => "omp executable",
        "aiTagging.batchSize" => "Batch size",
        "aiTagging.passLimit" => "Conversations per pass",
        "aiTagging.backoffSeconds" => "Wait after a failed run",
        "aiTagging.syncAccounts" => "Sync tags with the server",
        "autoRead.enabled" => "Mark stale notifications and newsletters read",
        "autoRead.days" => "Days before marking read",
        "remoteContent.trustedSenders" => "Trusted senders",
        "sync.pollSeconds" => "Mail polling",
        "sync.pushPollSeconds" => "Polling while push is connected",
        "chat.pollSeconds" => "Google Chat polling",
        other => other,
    }
}

/// Names for the words a choice setting takes.
fn choice_title(word: &str) -> &str {
    match word {
        "apple" => "Apple Intelligence",
        other => other,
    }
}

fn in_section(key: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| key.starts_with(prefix))
}

/// Shown in some section of this window.
fn shown(key: &str) -> bool {
    !key.starts_with(FLUTTER_ONLY)
}

// MARK: - The window

/// A text field of a number or text setting (keyed by the setting), or the field that adds to an
/// address setting (keyed by the setting and `ADD`).
struct Field {
    input: Entity<InputState>,
    /// The engine's value the field last took; a new one replaces the text unless it is being edited.
    shown: SharedString,
    _subscriptions: Vec<Subscription>,
}

/// The account being renamed and its field.
struct Rename {
    id: SharedString,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The OAuth client to sign in with (Flutter's `_ClientDialog`), filled in with the one used last
/// or the build's. Its secret never leaves the engine: an empty secret with that client's id keeps
/// the saved one.
struct ClientForm {
    provider: Provider,
    suggested: Option<OAuthClientInfo>,
    id: Entity<InputState>,
    secret: Entity<InputState>,
    error: Option<SharedString>,
    _subscriptions: [Subscription; 2],
}

struct Settings {
    on_changed: Rc<dyn Fn(&mut App)>,
    accounts: Option<Result<Vec<AccountRow>, MailError>>,
    status: Option<Status>,
    settings: Option<Result<mailstream::Settings, MailError>>,
    auto_read: Option<AutoReadLog>,
    fields: HashMap<String, Field>,
    /// Why the last write of a setting (or undoing auto-read) failed, by key.
    errors: HashMap<String, SharedString>,
    renaming: Option<Rename>,
    /// The account whose removal waits for confirmation.
    confirming: Option<SharedString>,
    client: Option<ClientForm>,
    /// The provider being signed in to, and the attempt it belongs to.
    signing_in: Option<Provider>,
    attempt: u64,
    retag_started: bool,
    note: Option<Note>,
    _appearance: Subscription,
}

impl Settings {
    fn new(on_changed: Rc<dyn Fn(&mut App)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let this = Self {
            on_changed,
            accounts: None,
            status: None,
            settings: None,
            auto_read: None,
            fields: HashMap::new(),
            errors: HashMap::new(),
            renaming: None,
            confirming: None,
            client: None,
            signing_in: None,
            attempt: 0,
            retag_started: false,
            note: None,
            _appearance: cx.observe_window_appearance(window, |_, window, cx| {
                design::sync_kit_theme(window.appearance(), cx);
                cx.notify()
            }),
        };
        this.load_accounts(cx);
        this.load_status(cx);
        this.load_settings(cx);
        this.load_auto_read(cx);
        this
    }

    /// Runs `work` on the background executor and hands its result to `done` on the main thread.
    fn background<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl Future<Output = R> + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(work).await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    // MARK: Loading

    fn follow_engine(&mut self, batch: &[Event], cx: &mut Context<Self>) {
        if batch.contains(&Event::Settings) {
            self.load_settings(cx);
        }
        if batch.contains(&Event::AutoRead) {
            self.load_auto_read(cx);
        }
        if batch.contains(&Event::Accounts) {
            self.load_accounts(cx);
        }
        if batch.iter().any(|e| matches!(e, Event::Settings | Event::Accounts | Event::Sync | Event::Problems)) {
            self.load_status(cx);
        }
    }

    fn load_accounts(&self, cx: &mut Context<Self>) {
        self.background(
            cx,
            async { engine().accounts().await.map(|list| list.into_iter().map(AccountRow::from).collect()) },
            |this, accounts, cx| {
                this.accounts = Some(accounts);
                cx.notify();
            },
        );
    }

    fn load_status(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().status().await.ok().map(Status::from) }, |this, status, cx| {
            this.status = status;
            cx.notify();
        });
    }

    fn load_settings(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().settings().await }, |this, settings, cx| {
            this.settings = Some(settings);
            cx.notify();
        });
    }

    fn load_auto_read(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().auto_read_log().await.ok() }, |this, log, cx| {
            this.auto_read = log;
            cx.notify();
        });
    }

    /// Lets the main window reload.
    fn changed(&self, cx: &mut Context<Self>) {
        let on_changed = self.on_changed.clone();
        cx.defer(move |cx| on_changed(cx));
    }

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note = Some(Note { text: message.into(), error: true });
        cx.notify();
    }

    fn rows(&self) -> &[AccountRow] {
        match &self.accounts {
            Some(Ok(rows)) => rows,
            _ => &[],
        }
    }

    fn entries(&self) -> &[SettingEntry] {
        match &self.settings {
            Some(Ok(settings)) => &settings.settings,
            _ => &[],
        }
    }

    fn entry(&self, key: &str) -> Option<&SettingEntry> {
        self.entries().iter().find(|entry| entry.key == key)
    }

    fn flag(&self, key: &str) -> Option<bool> {
        match self.entry(key)?.value {
            SettingValue::Flag { enabled } => Some(enabled),
            _ => None,
        }
    }

    // MARK: Settings

    /// Writes `value` to `key`, or puts it back to its default (`None`). The switch or choice
    /// shows the new value at once; the engine's answer replaces it, its refusal shows under the row.
    fn write(&mut self, key: String, value: Option<SettingValue>, cx: &mut Context<Self>) {
        if let (Some(value), Some(Ok(settings))) = (&value, &mut self.settings)
            && let Some(entry) = settings.settings.iter_mut().find(|entry| entry.key == key)
        {
            entry.changed = *value != entry.default_value;
            entry.value = value.clone();
        }
        cx.notify();
        let setting = key.clone();
        self.background(
            cx,
            async move {
                match value {
                    Some(value) => engine().set_setting(setting, value).await,
                    None => engine().reset_setting(setting).await,
                }
            },
            move |this, result, cx| {
                match result {
                    Ok(answer) => {
                        this.errors.remove(&key);
                        if let Some(Ok(settings)) = &mut this.settings
                            && let Some(entry) = settings.settings.iter_mut().find(|entry| entry.key == key)
                        {
                            *entry = answer;
                        }
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.errors.insert(key, error.to_string().into());
                        this.load_settings(cx);
                    }
                }
                cx.notify();
            },
        );
    }

    /// Keeps a field for every number and text setting (and every address setting's add field),
    /// and puts values written elsewhere (Mailstream.app, the CLI, an editor) in them unless they
    /// are being edited.
    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted: Vec<(String, SharedString)> = self
            .entries()
            .iter()
            .filter(|entry| shown(&entry.key))
            .filter_map(|entry| match &entry.value {
                SettingValue::Number { value } => Some((entry.key.clone(), value.to_string().into())),
                SettingValue::Text { text } if entry.choices.is_empty() => Some((entry.key.clone(), text.clone().into())),
                SettingValue::Addresses { .. } if entry.key != SYNC_ACCOUNTS => {
                    Some((format!("{}{ADD}", entry.key), SharedString::default()))
                }
                _ => None,
            })
            .collect();
        for (key, value) in wanted {
            if let Some(field) = self.fields.get_mut(&key) {
                if field.shown != value && !field.input.focus_handle(cx).is_focused(window) {
                    field.input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                    field.shown = value;
                }
                continue;
            }
            let adds = key.ends_with(ADD);
            let placeholder = match key.as_str() {
                "aiTagging.ompPath" => "From the login shell's PATH",
                _ if adds => "Add address",
                _ => "",
            };
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value.clone()));
            let submitted = key.clone();
            let mut subscriptions = vec![cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary: false, .. } = event {
                    this.commit_field(&submitted, window, cx);
                }
            })];
            // A number or text commits as the field loses the keys; an address waits for Return or Add.
            if !adds {
                let blurred = key.clone();
                let focus = input.focus_handle(cx);
                subscriptions.push(cx.on_blur(&focus, window, move |this, window, cx| this.commit_field(&blurred, window, cx)));
            }
            self.fields.insert(key, Field { input, shown: value, _subscriptions: subscriptions });
        }
    }

    fn revert_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get(key) else { return };
        let shown = field.shown.clone();
        field.input.update(cx, |input, cx| input.set_value(shown, window, cx));
        self.errors.remove(key);
        cx.notify();
    }

    fn commit_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.fields.get(key).map(|field| field.input.clone()) else { return };
        let typed = input.read(cx).value().trim().to_string();
        if let Some(setting) = key.strip_suffix(ADD) {
            let added: Vec<String> =
                typed.split(|c: char| c == ',' || c.is_whitespace()).filter(|a| !a.is_empty()).map(str::to_owned).collect();
            let Some(SettingValue::Addresses { mut addresses }) = self.entry(setting).map(|e| e.value.clone()) else {
                return;
            };
            if added.is_empty() {
                return;
            }
            input.update(cx, |input, cx| input.clean(window, cx));
            addresses.extend(added);
            self.write(setting.to_owned(), Some(SettingValue::Addresses { addresses }), cx);
            return;
        }
        let Some(entry) = self.entry(key) else { return };
        let value = match &entry.value {
            SettingValue::Number { value } => match typed.parse::<i64>() {
                Ok(number) if number == *value => return,
                Ok(number) => SettingValue::Number { value: number },
                Err(_) => {
                    self.errors.insert(key.to_owned(), "Enter a whole number.".into());
                    cx.notify();
                    return;
                }
            },
            SettingValue::Text { text } if *text == typed => return,
            SettingValue::Text { .. } => SettingValue::Text { text: typed },
            _ => return,
        };
        self.write(key.to_owned(), Some(value), cx);
    }

    fn retag(&mut self, cx: &mut Context<Self>) {
        self.retag_started = true;
        cx.notify();
        self.background(
            cx,
            async { engine().retag().await },
            |this, result, cx| match result {
                Ok(_) => this.load_status(cx),
                Err(error) => {
                    this.retag_started = false;
                    this.errors.insert(AI_TAGGING.into(), error.to_string().into());
                    cx.notify();
                }
            },
        );
    }

    fn undo_auto_read(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.background(
            cx,
            async move { engine().undo_auto_read(Some(ids)).await },
            |this, result, cx| {
                match result {
                    Ok(result) => {
                        match result.failed.first() {
                            Some(failure) => this.errors.insert(AUTO_READ_LOG.into(), failure.message.clone().into()),
                            None => this.errors.remove(AUTO_READ_LOG),
                        };
                        this.load_auto_read(cx);
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.errors.insert(AUTO_READ_LOG.into(), error.to_string().into());
                    }
                }
                cx.notify();
            },
        );
    }

    // MARK: Rename

    fn start_rename(&mut self, account: &AccountRow, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(account.id.clone()).default_value(account.label.clone()));
        let subscription = cx.subscribe_in(&input, window, Self::on_rename_event);
        window.focus(&input.read(cx).focus_handle(cx), cx);
        input.update(cx, |input, cx| input.select_all(window, cx));
        self.confirming = None;
        self.renaming = Some(Rename { id: account.id.clone(), input, _subscription: subscription });
        cx.notify();
    }

    fn on_rename_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { secondary: false, .. } = event {
            let label = input.read(cx).value().trim().to_string();
            self.commit_rename(label, cx);
        }
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        cx.notify();
    }

    fn commit_rename(&mut self, label: String, cx: &mut Context<Self>) {
        let Some(Rename { id, .. }) = self.renaming.take() else { return };
        let unchanged = self.rows().iter().any(|row| row.id == id && row.label.as_ref() == label);
        cx.notify();
        if unchanged {
            return;
        }
        let account = id.to_string();
        self.background(
            cx,
            async move { engine().rename_account(account, label).await },
            |this, result, cx| match result {
                Ok(_) => {
                    this.note = None;
                    this.load_accounts(cx);
                    this.changed(cx);
                }
                Err(error) => this.fail(error.to_string(), cx),
            },
        );
    }

    // MARK: Remove

    fn remove(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.confirming = None;
        cx.notify();
        let account = id.to_string();
        self.background(
            cx,
            async move { engine().remove_account(account, true).await },
            move |this, result, cx| match result {
                Ok(_) => {
                    this.note = Some(Note { text: format!("Removed {id}.").into(), error: false });
                    this.load_accounts(cx);
                    this.changed(cx);
                }
                Err(error) => this.fail(error.to_string(), cx),
            },
        );
    }

    // MARK: Sign in

    /// Asks the engine for the client used last, then shows the client form filled in with it.
    fn ask_client(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        self.note = None;
        cx.spawn_in(window, async move |this, cx| {
            let suggested = cx
                .background_executor()
                .spawn(async move { engine().suggested_client(provider.key().to_string()).await })
                .await;
            this.update_in(cx, |this, window, cx| this.show_client_form(provider, suggested.ok().flatten(), window, cx))
                .ok();
        })
        .detach();
    }

    fn show_client_form(
        &mut self,
        provider: Provider,
        suggested: Option<OAuthClientInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let google = provider == Provider::Google;
        let id = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(if google { "Client ID" } else { "Application (client) ID" });
            match &suggested {
                Some(client) => input.default_value(client.id.clone()),
                None => input,
            }
        });
        let secret_placeholder = if suggested.as_ref().is_some_and(|client| client.has_secret) {
            "Saved; leave empty to keep it"
        } else if google {
            "Client secret"
        } else {
            "Empty for a public client"
        };
        let secret = cx.new(|cx| InputState::new(window, cx).placeholder(secret_placeholder).masked(true));
        let focus = if suggested.is_some() { &secret } else { &id };
        window.focus(&focus.focus_handle(cx), cx);
        let on_event = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { secondary: false, .. } = event {
                this.submit_client(cx);
            }
        };
        let subscriptions = [cx.subscribe_in(&id, window, on_event), cx.subscribe_in(&secret, window, on_event)];
        self.client = Some(ClientForm { provider, suggested, id, secret, error: None, _subscriptions: subscriptions });
        cx.notify();
    }

    fn cancel_client(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        cx.notify();
    }

    fn submit_client(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.client else { return };
        let id = form.id.read(cx).value().trim().to_string();
        let secret = form.secret.read(cx).value().trim().to_string();
        let saved_secret = form.suggested.as_ref().is_some_and(|client| client.has_secret && client.id == id);
        if id.is_empty() {
            form.error = Some("Enter your OAuth client's ID to sign in.".into());
        } else if form.provider == Provider::Google && secret.is_empty() && !saved_secret {
            form.error =
                Some("Google needs the client secret too; the Cloud Console shows it next to the client ID.".into());
        } else {
            let provider = form.provider;
            self.client = None;
            self.sign_in(provider, id, (!secret.is_empty()).then_some(secret), cx);
        }
        cx.notify();
    }

    fn sign_in(&mut self, provider: Provider, client_id: String, client_secret: Option<String>, cx: &mut Context<Self>) {
        self.attempt += 1;
        let attempt = self.attempt;
        self.signing_in = Some(provider);
        self.note = None;
        cx.notify();
        self.background(
            cx,
            async move { engine().add_account(provider.key().to_string(), Some(client_id), client_secret).await },
            move |this, result, cx| {
                // The user stopped waiting, or started over.
                if this.attempt != attempt {
                    return;
                }
                this.signing_in = None;
                match result {
                    Ok(account) => {
                        this.note = Some(Note { text: format!("Signed in as {}.", account.id).into(), error: false });
                        this.load_accounts(cx);
                        this.changed(cx);
                    }
                    Err(error) => this.fail(error.to_string(), cx),
                }
            },
        );
    }

    fn stop_waiting(&mut self, cx: &mut Context<Self>) {
        self.attempt += 1;
        self.signing_in = None;
        cx.notify();
    }
}

// MARK: - Rendering

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_fields(window, cx);
        let p = design::palette(window.appearance());
        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.background)
            .p(px(metrics::PADDING))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.render_accounts(p, window, cx))
            .children(self.render_settings(p, window, cx))
            .child(self.render_status(p))
    }
}

impl Settings {
    fn render_accounts(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let body: AnyElement = match &self.accounts {
            None => note("Loading accounts…", p).into_any_element(),
            Some(Err(error)) => note(error.to_string(), p).into_any_element(),
            Some(Ok(rows)) if rows.is_empty() => card(p)
                .px(px(14.))
                .py(px(10.))
                .gap(px(2.))
                .child(div().font_weight(FontWeight::MEDIUM).child("No accounts"))
                .child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child("Sign in to bring in mail, contacts and Google Chat."),
                )
                .into_any_element(),
            Some(Ok(rows)) => {
                let last = rows.len() - 1;
                card(p)
                    .children(rows.iter().enumerate().map(|(index, row)| self.render_row(index, row, index == last, p, cx)))
                    .into_any_element()
            }
        };

        let add: AnyElement = match self.signing_in {
            Some(provider) => div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child(format!("Waiting for the {} sign-in in your browser…", provider.name())),
                )
                .child(
                    button("stop-waiting", "Stop Waiting", ButtonKind::Secondary, ControlSize::Regular, true, p)
                        .on_click(cx.listener(|this, _, _, cx| this.stop_waiting(cx))),
                )
                .into_any_element(),
            None => div()
                .flex()
                .gap(px(8.))
                .children([Provider::Google, Provider::Microsoft].map(|provider| {
                    button(
                        ElementId::Name(format!("add-{}", provider.key()).into()),
                        format!("{} Account…", provider.name()),
                        ButtonKind::Secondary,
                        ControlSize::Regular,
                        self.client.is_none(),
                        p,
                    )
                    .when(self.client.is_none(), |b| {
                        b.on_click(cx.listener(move |this, _, window, cx| this.ask_client(provider, window, cx)))
                    })
                }))
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(heading("Accounts", p))
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                "Mailstream signs in to Google and Microsoft in your browser, with an OAuth client you registered.",
            ))
            .child(body)
            .child(add)
            .children(self.client.as_ref().map(|form| self.render_client_form(form, p, window, cx)))
            .when_some(self.note.as_ref(), |this, n| {
                this.child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(if n.error { p.danger } else { p.muted })
                        .child(n.text.clone()),
                )
            })
    }

    fn render_client_form(&self, form: &ClientForm, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let google = form.provider == Provider::Google;
        let field = |label: &'static str, input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>| {
            let focused = input.focus_handle(cx).is_focused(window);
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(104.)).flex_shrink_0().flex().justify_end().child(format!("{label}:")))
                .child(
                    field_frame(focused, false, p)
                        .overflow_hidden()
                        .flex_1()
                        .min_w_0()
                        .h(px(26.))
                        .px(px(8.))
                        .on_action(cx.listener(|this, _: &Escape, _, cx| this.cancel_client(cx)))
                        .child(div().flex_1().min_w_0().child(input::field(input))),
                )
        };
        card(p)
            .p(px(16.))
            .gap(px(8.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Sign in with {}", form.provider.name())))
            .child(div().child(if google {
                "The client ID and secret of your Google Cloud OAuth client (type Desktop app). Mailstream then \
                 opens Google's sign-in in your browser."
            } else {
                "The application ID of your Microsoft Entra app, and its secret unless it is a public client. \
                 Mailstream then opens Microsoft's sign-in in your browser."
            }))
            .when(form.suggested.is_some(), |d| {
                d.child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child("Filled in with the client you signed in with last; change it to use another."),
                )
            })
            .child(field(if google { "Client ID" } else { "Application ID" }, &form.id, window, cx))
            .child(field("Client secret", &form.secret, window, cx))
            .when_some(form.error.clone(), |d, error| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.danger).child(error))
            })
            .child(
                div()
                    .pt(px(4.))
                    .flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        button("client-cancel", "Cancel", ButtonKind::Secondary, ControlSize::Regular, true, p)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_client(cx))),
                    )
                    .child(
                        button("client-continue", "Continue in Browser", ButtonKind::Primary, ControlSize::Regular, true, p)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_client(cx))),
                    ),
            )
    }

    fn render_row(
        &self,
        index: usize,
        row: &AccountRow,
        last: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let renaming = self.renaming.as_ref().filter(|rename| rename.id == id).map(|rename| rename.input.clone());
        let confirming = self.confirming.as_ref() == Some(&id);
        let mut detail = vec![row.provider_name().to_string(), row.id.to_string()];
        if let Some(name) = row.name.as_ref().filter(|n| !n.is_empty()) {
            detail.insert(1, name.to_string());
        }

        let title: AnyElement = match &renaming {
            Some(input) => div()
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(radius::SMALL))
                .border_1()
                .border_color(p.accent)
                .on_action(cx.listener(|this, _: &Escape, _, cx| this.cancel_rename(cx)))
                .child(input::field(input))
                .into_any_element(),
            None => div().font_weight(FontWeight::MEDIUM).truncate().child(row.title()).into_any_element(),
        };

        let actions: AnyElement = if renaming.is_some() {
            div()
                .flex()
                .gap(px(4.))
                .child(
                    button(("rename-cancel", index), "Cancel", ButtonKind::Secondary, ControlSize::Small, true, p)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_rename(cx))),
                )
                .child(button(("rename-save", index), "Save", ButtonKind::Primary, ControlSize::Small, true, p).on_click(
                    cx.listener(|this, _, _, cx| {
                        let label = this.renaming.as_ref().map(|rename| rename.input.read(cx).value().trim().to_string());
                        if let Some(label) = label {
                            this.commit_rename(label, cx);
                        }
                    }),
                ))
                .into_any_element()
        } else {
            let account = row.clone();
            let remove_id = id.clone();
            let idle = self.signing_in.is_none();
            div()
                .flex()
                .gap(px(4.))
                .child(
                    button(("rename", index), "Rename", ButtonKind::Plain(p.accent), ControlSize::Small, true, p)
                        .on_click(cx.listener(move |this, _, window, cx| this.start_rename(&account, window, cx))),
                )
                .child(
                    button(("remove", index), "Remove…", ButtonKind::Plain(p.danger), ControlSize::Small, idle, p)
                        .when(idle, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_rename(cx);
                                this.confirming = Some(remove_id.clone());
                            }))
                        }),
                )
                .into_any_element()
        };

        let main = div()
            .min_h(px(metrics::ROW_HEIGHT))
            .px(px(14.))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().flex_shrink_0().size(px(metrics::DOT)).rounded_full().bg(design::account_color(index)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(title)
                    .child(div().text_size(px(text::CAPTION)).text_color(p.muted).truncate().child(detail.join(" · ")))
                    .child(div().flex().gap(px(4.)).children(row.services.iter().map(|s| tag(service_title(s), None, p)))),
            )
            .child(actions);

        div()
            .flex()
            .flex_col()
            .when(!last, |this| this.border_b_1().border_color(p.line))
            .child(main)
            .when(confirming, |this| {
                let remove_id = id.clone();
                this.child(
                    div()
                        .mx(px(14.))
                        .mb(px(10.))
                        .p(px(12.))
                        .rounded(px(radius::CONTROL))
                        .bg(design::tint(p.danger, design::notice::TINT))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Remove {}?", row.title())))
                        .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(format!(
                            "Mailstream signs out of {} and deletes its saved sign-in. Mail on the server is not \
                             affected; you can add the account again later.",
                            row.id
                        )))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    button(("remove-cancel", index), "Cancel", ButtonKind::Secondary, ControlSize::Regular, true, p)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirming = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    button(("remove-confirm", index), "Remove", ButtonKind::Destructive, ControlSize::Regular, true, p)
                                        .on_click(cx.listener(move |this, _, _, cx| this.remove(remove_id.clone(), cx))),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    /// The settings' sections, as Flutter's Settings shows them: the file's problems first, then each
    /// group under its title, the keys of no section after, and where the file is.
    fn render_settings(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let settings = match &self.settings {
            None => return vec![note("Loading settings…", p).into_any_element()],
            Some(Err(error)) => return vec![note(error.to_string(), p).into_any_element()],
            Some(Ok(settings)) => settings,
        };
        let mut out: Vec<AnyElement> =
            settings.issues.iter().map(|issue| notice(Tone::Error, None, issue.clone(), false, p).into_any_element()).collect();
        for (title, prefixes) in SECTIONS {
            let mut rows: Vec<AnyElement> = settings
                .settings
                .iter()
                .filter(|entry| in_section(&entry.key, prefixes))
                .map(|entry| self.render_entry(entry, p, window, cx))
                .collect();
            let tagging = prefixes.contains(&"aiTagging.");
            if tagging {
                rows.push(self.render_retag(p, cx));
            }
            // Why tagging stopped, as the engine reports it: omp or Apple Intelligence
            // unavailable, a failed run.
            let problem = self
                .status
                .as_ref()
                .filter(|_| tagging)
                .and_then(|status| status.tagging.failed.then(|| status.tagging.status.clone()).flatten());
            out.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(heading(*title, p))
                    .children(problem.map(|problem| notice(Tone::Error, None, problem, false, p)))
                    .children(group(rows, p))
                    .when(prefixes.contains(&"autoRead."), |d| d.children(self.render_auto_read(p, cx)))
                    .into_any_element(),
            );
        }
        let rest: Vec<AnyElement> = settings
            .settings
            .iter()
            .filter(|entry| shown(&entry.key) && !SECTIONS.iter().any(|(_, prefixes)| in_section(&entry.key, prefixes)))
            .map(|entry| self.render_entry(entry, p, window, cx))
            .collect();
        out.extend(group(rest, p).map(IntoElement::into_any_element));
        out.push(
            div()
                .px(px(4.))
                .text_size(px(text::FOOTNOTE))
                .text_color(p.muted)
                .child(format!("Saved in {}, shared with Mailstream and `mailstream config`.", settings.file))
                .into_any_element(),
        );
        out
    }

    fn render_entry(&self, entry: &SettingEntry, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if entry.key == SYNC_ACCOUNTS {
            return self.render_sync_accounts(entry, p, cx);
        }
        let key = entry.key.clone();
        let range = entry.minimum.zip(entry.maximum).map(|(low, high)| format!("{low}–{high}"));
        let control: Option<AnyElement> = match &entry.value {
            SettingValue::Flag { enabled } => {
                let (on, key) = (*enabled, key.clone());
                Some(
                    switch(ElementId::Name(format!("switch-{key}").into()), on)
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            this.write(key.clone(), Some(SettingValue::Flag { enabled: !on }), cx)
                        }))
                        .into_any_element(),
                )
            }
            SettingValue::Text { text } if !entry.choices.is_empty() => Some(
                div()
                    .flex()
                    .gap(px(4.))
                    .children(entry.choices.iter().map(|word| {
                        let selected = word == text;
                        let (key, word) = (key.clone(), word.clone());
                        button(
                            ElementId::Name(format!("{key}={word}").into()),
                            choice_title(&word).to_owned(),
                            if selected { ButtonKind::Primary } else { ButtonKind::Secondary },
                            ControlSize::Small,
                            true,
                            p,
                        )
                        .when(!selected, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.write(key.clone(), Some(SettingValue::Text { text: word.clone() }), cx)
                            }))
                        })
                    }))
                    .into_any_element(),
            ),
            SettingValue::Number { .. } => {
                self.render_field(&key, 120., key.ends_with("Seconds").then_some("s"), p, window, cx)
            }
            SettingValue::Text { .. } => self.render_field(&key, 220., None, p, window, cx),
            SettingValue::Addresses { .. } => None,
        };
        let reset_key = key.clone();
        let reset = div().w(px(52.)).flex_shrink_0().flex().justify_end().when(entry.changed, |d| {
            d.child(
                button(ElementId::Name(format!("reset-{key}").into()), "Reset", ButtonKind::Plain(p.accent), ControlSize::Small, true, p)
                    .on_click(cx.listener(move |this, _, _, cx| this.write(reset_key.clone(), None, cx))),
            )
        });
        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(setting_label(entry, range, p))
                    .children(control)
                    .child(reset),
            )
            .when_some(
                match &entry.value {
                    SettingValue::Addresses { addresses } => Some(addresses),
                    _ => None,
                },
                |d, addresses| d.child(self.render_addresses(&key, addresses, p, window, cx)),
            )
            .children(self.render_error(&key, p))
            .into_any_element()
    }

    fn render_field(
        &self,
        key: &str,
        width: f32,
        suffix: Option<&'static str>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let field = self.fields.get(key)?;
        let focused = field.input.focus_handle(cx).is_focused(window);
        Some(
            field_frame(focused, false, p)
                .overflow_hidden()
                .w(px(width))
                .flex_shrink_0()
                .h(px(26.))
                .px(px(8.))
                .gap(px(4.))
                .on_action(cx.listener({
                    let key = key.to_owned();
                    move |this, _: &Escape, window, cx| {
                        this.revert_field(&key, window, cx);
                        window.blur(cx);
                    }
                }))
                .child(div().flex_1().min_w_0().child(input::field(&field.input)))
                .children(suffix.map(|suffix| div().text_color(p.muted).child(suffix)))
                .into_any_element(),
        )
    }

    /// An address setting's addresses as pills, each removable, and a field that adds more
    /// (comma-separated).
    fn render_addresses(
        &self,
        key: &str,
        addresses: &[String],
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let add_key = format!("{key}{ADD}");
        div()
            .pt(px(8.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .children(addresses.iter().map(|address| {
                let next: Vec<String> = addresses.iter().filter(|a| *a != address).cloned().collect();
                let key = key.to_owned();
                pill(address.clone(), None, p).child(
                    div()
                        .id(ElementId::Name(format!("remove-{key}-{address}").into()))
                        .cursor_pointer()
                        .child(icon(Icon::Close, 12., p.muted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.write(key.clone(), Some(SettingValue::Addresses { addresses: next.clone() }), cx)
                        })),
                )
            }))
            .children(self.render_field(&add_key, 220., None, p, window, cx))
            .child(
                button(ElementId::Name(format!("add-{key}").into()), "Add", ButtonKind::Secondary, ControlSize::Small, true, p)
                    .on_click(cx.listener(move |this, _, window, cx| this.commit_field(&add_key, window, cx))),
            )
    }

    /// `aiTagging.syncAccounts` as a switch per mail account: on, that account's AI tags sync both
    /// ways with its Gmail labels or Outlook categories. Accounts listed but no longer signed in
    /// show too, so they can be switched off.
    fn render_sync_accounts(&self, entry: &SettingEntry, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let listed: Vec<String> = match &entry.value {
            SettingValue::Addresses { addresses } => addresses.clone(),
            _ => Vec::new(),
        };
        let mut rows: Vec<(String, String)> = self
            .rows()
            .iter()
            .filter(|account| account.services.iter().any(|s| s == "mail"))
            .map(|account| {
                let what = if account.provider == "google" { "Gmail labels" } else { "Outlook categories" };
                (account.id.to_lowercase(), format!("{}  ·  {what}", account.title()))
            })
            .collect();
        for address in &listed {
            if !rows.iter().any(|(a, _)| a == address) {
                rows.push((address.clone(), format!("{address}  ·  not signed in")));
            }
        }
        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(div().flex().child(setting_label(entry, None, p)))
            .when(rows.is_empty(), |d| {
                d.child(
                    div()
                        .pt(px(6.))
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child("Sign in to a Gmail or Outlook account to sync its tags."),
                )
            })
            .children(rows.into_iter().map(|(address, label)| {
                let on = listed.contains(&address);
                let next: Vec<String> = if on {
                    listed.iter().filter(|a| **a != address).cloned().collect()
                } else {
                    listed.iter().cloned().chain([address.clone()]).collect()
                };
                div()
                    .pt(px(8.))
                    .pr(px(64.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(switch(ElementId::Name(format!("sync-{address}").into()), on).on_click(cx.listener(
                        move |this, _: &bool, _, cx| {
                            this.write(SYNC_ACCOUNTS.into(), Some(SettingValue::Addresses { addresses: next.clone() }), cx)
                        },
                    )))
            }))
            .children(self.render_error(SYNC_ACCOUNTS, p))
            .into_any_element()
    }

    /// GPUI's own row in AI tagging: tag every conversation again.
    fn render_retag(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.flag(AI_TAGGING) == Some(true) && !self.retag_started;
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(10.))
            .child(div().flex_1().min_w_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                if self.retag_started {
                    "Re-tagging started."
                } else {
                    "Tag every conversation again, replacing the tags the model gave before."
                },
            ))
            .child(
                button("retag", "Re-tag All Conversations", ButtonKind::Secondary, ControlSize::Regular, enabled, p)
                    .when(enabled, |b| b.on_click(cx.listener(|this, _, _, cx| this.retag(cx)))),
            )
            .into_any_element()
    }

    /// What auto-read marked read and nobody undid, newest first, each with Mark Unread, and Mark
    /// All Unread above them; a line saying nothing was marked while it is on and has done nothing.
    fn render_auto_read(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<Div> {
        let log = self.auto_read.as_ref()?;
        let enabled = self.flag(AUTO_READ).unwrap_or(false);
        if log.entries.is_empty() && !enabled {
            return None;
        }
        let days = match self.entry(AUTO_READ_DAYS).map(|entry| &entry.value) {
            Some(SettingValue::Number { value }) => *value,
            _ => log.days,
        };
        let count = log.entries.len();
        let summary = if count == 0 {
            format!("Nothing yet. Unread notifications and newsletters older than {days} days show here once marked read.")
        } else {
            format!(
                "{count} conversation{}. Marking one unread keeps auto-read away from it until new mail arrives.",
                if count == 1 { "" } else { "s" }
            )
        };
        let all: Vec<String> = log.entries.iter().map(|entry| entry.id.clone()).collect();
        let header = div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child("Recently marked read")
                            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(summary)),
                    )
                    .when(count > 0, |d| {
                        d.child(
                            button("auto-read-all", "Mark All Unread", ButtonKind::Secondary, ControlSize::Regular, true, p)
                                .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(all.clone(), cx))),
                        )
                    }),
            )
            .children(self.render_error(AUTO_READ_LOG, p))
            .into_any_element();
        let now = Local::now();
        let rows = log.entries.iter().take(AUTO_READ_ROWS).enumerate().map(|(index, entry)| {
            let marked = DateTime::parse_from_rfc3339(&entry.at)
                .ok()
                .map(|at| format!("marked {}", format::list_date(Some(at.with_timezone(&Utc)), now)));
            let detail = [
                Some(if entry.category == "newsletter" { "Newsletter" } else { "Notification" }.to_owned()),
                Some(
                    if entry.reason == "superseded" {
                        "a newer one from the same sender arrived"
                    } else {
                        "unread too long"
                    }
                    .to_owned(),
                ),
                marked,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("  ·  ");
            let id = entry.id.clone();
            div()
                .px(px(14.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .min_w_0()
                                .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(if entry.sender.is_empty() {
                                    "(Unknown sender)".to_owned()
                                } else {
                                    entry.sender.clone()
                                }))
                                .child(div().flex_1().min_w_0().truncate().child(entry.subject.clone())),
                        )
                        .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).truncate().child(detail)),
                )
                .child(
                    icon_button(("auto-read", index), Icon::MarkUnread, "Mark Unread", true, p.muted)
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(vec![id.clone()], cx))),
                )
                .into_any_element()
        });
        group(std::iter::once(header).chain(rows).collect(), p)
    }

    fn render_error(&self, key: &str, p: &Palette) -> Option<Div> {
        let error = self.errors.get(key)?;
        Some(div().pt(px(4.)).text_size(px(text::FOOTNOTE)).text_color(p.danger).child(error.clone()))
    }

    fn render_status(&self, p: &'static Palette) -> Div {
        let section = div().flex().flex_col().gap(px(8.)).child(heading("Status", p));
        let Some(status) = &self.status else {
            return section.child(note("Mailstream is not answering.", p));
        };
        let refreshed = if status.loading {
            "Refreshing now".to_string()
        } else {
            status.last_refresh.map_or("Not yet".into(), |at| format::message_date(Some(at), Local::now()))
        };
        let tagging = match &status.tagging {
            mailstream::TaggingStatus { enabled: false, .. } => "Off".to_string(),
            mailstream::TaggingStatus { status: None, .. } => "On".to_string(),
            mailstream::TaggingStatus { status: Some(line), .. } => line.clone(),
        };
        let mut rows = vec![
            ("Version", status.version.clone(), None),
            ("Last refresh", refreshed, None),
            ("Push", if status.push_connected { "Connected" } else { "Not connected" }.into(), None),
            ("AI tagging", tagging, None),
        ];
        if let Some(error) = &status.last_error {
            rows.push(("Sync error", error.clone(), Some(p.danger)));
        }
        let last = rows.len() - 1;
        section.child(card(p).children(rows.into_iter().enumerate().map(|(i, (label, value, color))| {
            div()
                .flex()
                .gap(px(12.))
                .px(px(14.))
                .py(px(8.))
                .when(i != last, |this| this.border_b_1().border_color(p.line))
                .child(div().w(px(110.)).flex_shrink_0().text_color(p.muted).child(label))
                .child(div().flex_1().min_w_0().text_color(color.unwrap_or(p.text)).child(value))
        })))
    }
}

// MARK: - Pieces

/// A section's title (Flutter's `_Heading`): bold secondary text over its group.
fn heading(title: impl Into<SharedString>, p: &Palette) -> Div {
    div().pt(px(8.)).px(px(4.)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(title.into())
}

fn card(p: &Palette) -> Div {
    div().flex().flex_col().rounded(px(radius::CARD)).border(px(stroke::HAIRLINE)).border_color(p.line).bg(p.fill)
}

/// Rows in a card divided by hairlines (Flutter's `_Group`); nothing when there are none.
fn group(rows: Vec<AnyElement>, p: &Palette) -> Option<Div> {
    (!rows.is_empty()).then(|| {
        card(p).children(
            rows.into_iter().enumerate().map(|(index, row)| div().when(index > 0, |d| d.border_t_1().border_color(p.line)).child(row)),
        )
    })
}

/// A setting's name over the engine's description and its key (with the range a number takes).
fn setting_label(entry: &SettingEntry, range: Option<String>, p: &Palette) -> Div {
    let key = match range {
        Some(range) => format!("{}  ·  {range}", entry.key),
        None => entry.key.clone(),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(title(&entry.key).to_owned())
        .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(entry.summary.clone()))
        .child(div().text_size(px(text::CAPTION)).text_color(p.faint).child(key))
}

fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().py(px(12.)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(text.into())
}

fn service_title(service: &str) -> String {
    match service {
        "mail" => "Mail",
        "contacts" => "Contacts",
        "chat" => "Chat",
        "calendar" => "Calendar",
        other => other,
    }
    .to_owned()
}
