//! The Settings window (Flutter's `settings.dart`): the accounts (sign in with your own OAuth
//! client, rename, remove), every setting of settings.json in Flutter's groups, what auto-read
//! marked read (with undo), and the engine's status.
//!
//! Settings go through the engine (`settings`, `set_setting`, `reset_setting`): settings.json is the
//! file Mailstream.app, Flutter and `mailstream config` share, and the window reads it again on the
//! engine's `Settings` event, whoever changed it. The `flutter.*` keys configure only the Flutter
//! app and stay out of this window; the `gpui.*` ones are this app's (list widths, frame rate).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::component::Disableable;
use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, ClipboardItem, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, FontWeight, Global, InteractiveElement, IntoElement, KeyBinding, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowOptions, actions, div, px, size,
};
use mailstream::{
    Account, AutoReadLog, Change, DeviceSyncStatus, Event, EventListener, OAuthClientInfo, Provider, SettingEntry,
    SettingValue, SyncDevice, SyncTicket,
};

use crate::controls::{
    ButtonKind, ControlSize, Tone, button, compact_capsule, compact_icon_button, field_frame, hairline, icon_button,
    icon_label_button, notice, pill, switch, tag_for,
};
use crate::design::{self, Palette, layout, radius, settings as metrics, space, stroke, text};
use crate::engine::{MailError, engine};
use crate::icons::{Icon, icon};
use crate::input;
use crate::model::{AccountExt, ProviderExt};
use crate::settings_list::{self, SettingsListKind, SettingsListView};
actions!(settings, [ShowSettings, CloseSettings]);

pub const CONTEXT: &str = "Settings";

/// Binds cmd-, to `ShowSettings`, which opens the window with `on_changed` (see `open_settings`),
/// and hands the engine's change events to the window while it is open.
pub fn init(on_changed: impl Fn(&mut App) + 'static, cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!(","), ShowSettings, None),
        KeyBinding::new("escape", CloseSettings, Some("Settings && !Input")),
        KeyBinding::new(secondary!("w"), CloseSettings, Some(CONTEXT)),
    ]);
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
    if let Some(OpenSettings::Window(handle, _)) = cx.try_global::<OpenSettings>() {
        let handle = *handle;
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
        gpui::open_window(options, cx, |window, cx| cx.new(|cx| Settings::new(on_changed, None, window, cx)))
    else {
        return;
    };
    cx.set_global(OpenSettings::Window(window, settings.downgrade()));
    window.update(cx, |_, window, _| window.activate_window()).ok();
}

/// Registers an inline `Settings` view (compact / Android single-window) so engine events reach it.
pub(crate) fn register_inline(settings: &Entity<Settings>, cx: &mut App) {
    cx.set_global(OpenSettings::Inline(settings.downgrade()));
}

/// Clears the inline `Settings` registration once the overlay closes.
pub(crate) fn unregister_inline(cx: &mut App) {
    if matches!(cx.try_global::<OpenSettings>(), Some(OpenSettings::Inline(_))) {
        cx.remove_global::<OpenSettings>();
    }
}

/// The open Settings view — either a standalone OS window or an inline overlay inside `MailApp`.
enum OpenSettings {
    Window(AnyWindowHandle, WeakEntity<Settings>),
    Inline(WeakEntity<Settings>),
}

impl OpenSettings {
    fn view(&self) -> WeakEntity<Settings> {
        match self {
            Self::Window(_, view) | Self::Inline(view) => view.clone(),
        }
    }
}

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
            let open = cx.update(|cx| cx.try_global::<OpenSettings>().map(OpenSettings::view));
            if let Some(settings) = open {
                settings.update(cx, |settings, cx| settings.follow_engine(&batch, cx)).ok();
            }
            settings_list::notify_open_lists(&batch, cx);
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

/// A note under the accounts: what the last action did.
struct Note {
    text: SharedString,
    error: bool,
}

// MARK: - Keys

/// Sections of settings.json by key prefix, in the order they show (Flutter's `_sections`, with
/// this app's `gpui.*` keys for its `flutter.*` ones). Keys under none of them follow in a group
/// without a title.
const SECTIONS: &[(&str, &[&str])] = &[
    ("Sync", &["sync.", "chat."]),
    ("Notifications", &["notifications."]),
    ("AI tagging", &["aiTagging."]),
    ("Auto-read", &["autoRead."]),
    ("Remote content", &["remoteContent."]),
    ("Layout", &["gpui.listWidth", "gpui.calendarWidth"]),
    ("Performance", &["gpui.showFrameRate"]),
];

/// Keys only the Flutter app reads.
const FLUTTER_ONLY: &str = "flutter.";

const AI_TAGGING: &str = "aiTagging.enabled";
const TAGGING_DEVICES_MODE: &str = "aiTagging.devicesMode";
const TAGGING_DEVICES: &str = "aiTagging.devices";
const LOCAL_ONLY_ACCOUNTS: &str = "aiTagging.localOnlyAccounts";
const AUTO_READ: &str = "autoRead.enabled";
const AUTO_READ_DAYS: &str = "autoRead.days";
/// Where the auto-read log's error goes in `Settings::errors`.
const AUTO_READ_LOG: &str = "autoRead.log";
/// Suffix of the field that adds to an address setting, in `Settings::fields`.
const ADD: &str = "+add";

/// Short names for the keys (Flutter's `_titles`); the engine's description sits under each.
pub(crate) fn title(key: &str) -> &str {
    match key {
        "aiTagging.enabled" => "Sort and flag mail with AI",
        "aiTagging.devicesMode" => "Tagging devices",
        "aiTagging.devices" => "Selected tagging devices",
        "aiTagging.backend" => "Tagged by",
        "aiTagging.model" => "Model",
        "aiTagging.ompPath" => "omp executable",
        "aiTagging.batchSize" => "Batch size",
        "aiTagging.passLimit" => "Conversations per pass",
        "aiTagging.backoffSeconds" => "Wait after a failed run",
        "aiTagging.localOnlyAccounts" => "Sync tags with the server",
        "autoRead.enabled" => "Mark stale notifications and newsletters read",
        "autoRead.days" => "Days before marking read",
        "remoteContent.trustedSenders" => "Trusted senders",
        "sync.pollSeconds" => "Mail polling",
        "sync.pushPollSeconds" => "Polling while push is connected",
        "chat.pollSeconds" => "Google Chat polling",
        "notifications.enabled" => "System notifications",
        "notifications.mail" => "Mail notifications",
        "notifications.chat" => "Google Chat notifications",
        "gpui.showFrameRate" => "Show frame rate",
        "gpui.listWidth" => "List width (‰ of the window)",
        "gpui.calendarWidth" => "Calendar width (‰ of the window)",
        other => other,
    }
}

/// Names for the words a choice setting takes.
fn choice_title<'a>(key: &str, word: &'a str) -> &'a str {
    match (key, word) {
        (_, "apple") => "Apple Intelligence",
        ("notifications.mail", "important") => "Important",
        ("notifications.mail", "all") => "All mail",
        ("notifications.mail", "off") => "Off",
        (_, "all") => "All devices",
        (_, "selected") => "Selected devices",
        (_, other) => other,
    }
}
fn in_section(key: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| key.starts_with(prefix))
}

/// Shown in some section of this window.
fn shown(key: &str) -> bool {
    !key.starts_with(FLUTTER_ONLY) && key != TAGGING_DEVICES
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
    id: String,
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

/// A sign-in waiting in the browser: a new account with `provider`, or one more service for an
/// account (`connecting`: its address and the service).
struct Waiting {
    provider: Provider,
    connecting: Option<(String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncBusy {
    Toggle,
    Pair,
    Join,
    Reset,
    Remove,
    Now,
    Cloud,
}

/// The one-time pairing code on show (MS-376), redrawn every second for its countdown until it
/// expires.
struct PairCode {
    code: String,
    expires: Option<DateTime<Utc>>,
    _tick: Task<()>,
}

impl PairCode {
    fn new(ticket: SyncTicket, cx: &mut Context<Settings>) -> Self {
        let expires = DateTime::parse_from_rfc3339(&ticket.expires_at).ok().map(|at| at.with_timezone(&Utc));
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let ticking = this.update(cx, |_, cx| {
                    cx.notify();
                    expires.is_some_and(|at| at > Utc::now())
                });
                if !ticking.unwrap_or(false) {
                    break;
                }
            }
        });
        Self { code: ticket.ticket, expires, _tick: tick }
    }

    /// How long it still works, `None` once expired.
    fn left(&self) -> Option<chrono::Duration> {
        self.expires.map(|at| at - Utc::now()).filter(|left| *left > chrono::Duration::zero())
    }
}

pub(crate) struct Settings {
    pub(crate) focus: FocusHandle,
    on_changed: Rc<dyn Fn(&mut App)>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    accounts: Option<Result<Vec<Account>, MailError>>,
    status: Option<Status>,
    device_sync: Option<DeviceSyncStatus>,
    settings: Option<Result<mailstream::Settings, MailError>>,
    auto_read: Option<AutoReadLog>,
    fields: HashMap<String, Field>,
    /// Why the last write of a setting (or undoing auto-read) failed, by key.
    errors: HashMap<String, SharedString>,
    join_input: Option<Entity<InputState>>,
    join_subscription: Option<Subscription>,
    /// The Cloudflare drop's Worker URL, kept in sync with the active sync group's setting.
    cloud_input: Option<Entity<InputState>>,
    cloud_url_shown: String,
    cloud_subscription: Option<Subscription>,
    /// Why the last change to the Cloudflare drop failed before reaching it (no URL, a bad one).
    cloud_error: Option<SharedString>,
    sync_ticket: Option<PairCode>,
    ticket_copied: bool,
    sync_confirm_reset: bool,
    /// The paired device whose removal waits for confirmation (MS-396), by endpoint id.
    sync_confirm_remove: Option<String>,
    sync_busy: Option<SyncBusy>,
    sync_error: Option<SharedString>,
    renaming: Option<Rename>,
    /// The account whose removal waits for confirmation.
    confirming: Option<String>,
    client: Option<ClientForm>,
    /// The sign-in in the browser being waited for, and the attempt it belongs to.
    signing_in: Option<Waiting>,
    attempt: u64,
    retag_started: bool,
    note: Option<Note>,
    /// In compact / Android inline mode, the full-list sub-page currently open over Settings (MS-422).
    subpage: Option<Entity<SettingsListView>>,
    _appearance: Subscription,
}

impl Settings {
    pub(crate) fn new_inline(
        on_changed: impl Fn(&mut App) + 'static,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(Rc::new(on_changed), Some(Rc::new(on_close)), window, cx)
    }

    fn new(
        on_changed: Rc<dyn Fn(&mut App)>,
        on_close: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = Self {
            focus: cx.focus_handle(),
            on_changed,
            on_close,
            accounts: None,
            status: None,
            device_sync: None,
            settings: None,
            auto_read: None,
            fields: HashMap::new(),
            errors: HashMap::new(),
            join_input: None,
            join_subscription: None,
            cloud_input: None,
            cloud_url_shown: String::new(),
            cloud_subscription: None,
            cloud_error: None,
            sync_ticket: None,
            ticket_copied: false,
            sync_confirm_reset: false,
            sync_confirm_remove: None,
            sync_busy: None,
            sync_error: None,
            renaming: None,
            confirming: None,
            client: None,
            signing_in: None,
            attempt: 0,
            retag_started: false,
            note: None,
            subpage: None,
            _appearance: cx.observe_window_appearance(window, |_, window, cx| {
                design::sync_kit_theme(window.appearance(), cx);
                cx.notify()
            }),
        };
        this.load_accounts(cx);
        this.load_status(cx);
        this.load_device_sync(cx);
        this.load_settings(cx);
        this.load_auto_read(cx);
        this
    }

    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.subpage.take().is_some() {
            window.cancel_touch_momentum();
            window.focus(&self.focus, cx);
            cx.notify();
            true
        } else {
            false
        }
    }

    fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.cancel_touch_momentum();
        self.subpage = None;
        if let Some(on_close) = self.on_close.clone() {
            on_close(window, cx);
        } else {
            window.remove_window();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.go_back(window, cx) {
            return;
        }
        self.close_all(window, cx);
    }

    fn open_list(&mut self, kind: SettingsListKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.on_close.is_some() || cfg!(target_os = "android") {
            let settings = cx.entity().downgrade();
            let back_settings = settings.clone();
            let close_settings = settings;
            let on_changed = self.on_changed.clone();
            let view = cx.new(|cx| {
                SettingsListView::new_inline(
                    kind,
                    on_changed,
                    Rc::new(move |window, cx| {
                        back_settings.update(cx, |this, cx| {
                            this.go_back(window, cx);
                        }).ok();
                    }),
                    Rc::new(move |window, cx| {
                        close_settings.update(cx, |this, cx| this.close_all(window, cx)).ok();
                    }),
                    window,
                    cx,
                )
            });
            let focus = view.read(cx).focus.clone();
            self.subpage = Some(view);
            window.focus(&focus, cx);
            cx.notify();
        } else {
            settings_list::open_list_window(kind, self.on_changed.clone(), cx);
        }
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
        if batch.iter().any(|e| matches!(e, Event::Settings | Event::Accounts | Event::Sync | Event::Problems | Event::DeviceSync)) {
            self.load_status(cx);
        }
        if batch.contains(&Event::DeviceSync) {
            self.load_device_sync(cx);
        }
        if let Some(subpage) = &self.subpage {
            subpage.update(cx, |view, cx| view.follow_engine(batch, cx));
        }
    }

    fn load_accounts(&self, cx: &mut Context<Self>) {
        self.background(
            cx,
            async { engine().accounts().await },
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

    fn load_device_sync(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().sync_status().await }, |this, result, cx| {
            match result {
                Ok(status) => this.device_sync = Some(status),
                Err(error) => this.sync_error = Some(error.to_string().into()),
            }
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

    fn rows(&self) -> &[Account] {
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
                SettingValue::Addresses { .. } if entry.key != LOCAL_ONLY_ACCOUNTS => {
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
            let added = settings_list::parse_added_addresses(&typed);
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


    // MARK: Device sync

    fn ensure_join_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.join_input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Paste a pairing code from another device"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary: false, .. } = event {
                this.join_sync(window, cx);
            }
        });
        self.join_input = Some(input);
        self.join_subscription = Some(subscription);
    }

    fn ensure_cloud_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &self.device_sync else { return };
        let saved = status.cloud.url.clone().unwrap_or_default();
        if let Some(input) = &self.cloud_input {
            if self.cloud_url_shown != saved && !input.focus_handle(cx).is_focused(window) {
                input.update(cx, |input, cx| input.set_value(saved.clone(), window, cx));
                self.cloud_url_shown = saved;
                self.cloud_error = None;
            }
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Worker URL, like https://mailstream-sync-drop.<you>.workers.dev")
                .default_value(saved.clone())
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { secondary: false, .. } = event {
                this.toggle_cloud(true, cx);
            }
        });
        self.cloud_input = Some(input);
        self.cloud_url_shown = saved;
        self.cloud_subscription = Some(subscription);
    }

    fn run_sync<F>(&mut self, busy: SyncBusy, work: F, after: impl FnOnce(&mut Self) + 'static, cx: &mut Context<Self>)
    where
        F: Future<Output = Result<DeviceSyncStatus, MailError>> + Send + 'static,
    {
        self.sync_busy = Some(busy);
        self.sync_error = None;
        cx.notify();
        self.background(cx, work, move |this, result, cx| {
            this.sync_busy = None;
            match result {
                Ok(status) => {
                    this.device_sync = Some(status);
                    after(this);
                    this.changed(cx);
                }
                Err(error) => {
                    this.sync_error = Some(error.to_string().into());
                }
            }
            cx.notify();
        });
    }

    fn toggle_device_sync(&mut self, enable: bool, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        self.sync_confirm_reset = false;
        self.sync_confirm_remove = None;
        if !enable {
            self.sync_ticket = None;
            self.ticket_copied = false;
        }
        self.run_sync(
            SyncBusy::Toggle,
            async move {
                if enable { engine().sync_enable().await } else { engine().sync_disable().await }
            },
            |_| {},
            cx,
        );
    }

    fn pair_device(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        let needs_enable = self.device_sync.as_ref().is_none_or(|status| !status.enabled);
        self.sync_busy = Some(SyncBusy::Pair);
        self.sync_confirm_reset = false;
        self.sync_confirm_remove = None;
        self.sync_error = None;
        self.ticket_copied = false;
        cx.notify();
        self.background(
            cx,
            async move {
                if needs_enable {
                    engine().sync_enable().await?;
                }
                let ticket = engine().sync_pair_ticket().await?;
                let status = engine().sync_status().await?;
                Ok::<_, MailError>((status, ticket))
            },
            |this, result, cx| {
                this.sync_busy = None;
                match result {
                    Ok((status, ticket)) => {
                        this.device_sync = Some(status);
                        this.sync_ticket = Some(PairCode::new(ticket, cx));
                    }
                    Err(error) => {
                        this.sync_error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            },
        );
    }

    fn copy_sync_ticket(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.sync_ticket.as_ref().map(|ticket| ticket.code.clone()) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(code));
        self.ticket_copied = true;
        cx.notify();
    }

    fn join_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        let Some(input) = self.join_input.clone() else { return };
        let ticket = input.read(cx).value().trim().to_string();
        if ticket.is_empty() {
            self.sync_error = Some("Paste a pairing code from another device to join its sync group.".into());
            cx.notify();
            return;
        }
        self.sync_busy = Some(SyncBusy::Join);
        self.sync_confirm_reset = false;
        self.sync_confirm_remove = None;
        self.sync_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { engine().sync_join(ticket).await }).await;
            this.update_in(cx, |this, window, cx| {
                this.sync_busy = None;
                match result {
                    Ok(status) => {
                        this.device_sync = Some(status);
                        this.sync_ticket = None;
                        this.ticket_copied = false;
                        this.cloud_error = None;
                        input.update(cx, |input, cx| input.clean(window, cx));
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.sync_error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        self.run_sync(SyncBusy::Now, async { engine().sync_now().await }, |_| {}, cx);
    }

    fn reset_sync_group(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        self.sync_confirm_reset = false;
        self.run_sync(
            SyncBusy::Reset,
            async { engine().sync_reset(true).await },
            |this| {
                this.sync_ticket = None;
                this.ticket_copied = false;
                this.cloud_error = None;
            },
            cx,
        );
    }

    /// Removes the paired device `id` from the sync group (MS-396).
    fn remove_sync_device(&mut self, id: String, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        self.run_sync(
            SyncBusy::Remove,
            async move { engine().sync_remove(id).await },
            |this| this.sync_confirm_remove = None,
            cx,
        );
    }

    /// Turns the Cloudflare drop on with the URL typed, or off (which deletes this device's copy).
    fn toggle_cloud(&mut self, enable: bool, cx: &mut Context<Self>) {
        if self.sync_busy.is_some() {
            return;
        }
        let url = self.cloud_input.as_ref().map(|input| input.read(cx).value().trim().to_string()).unwrap_or_default();
        if enable && url.is_empty() {
            self.cloud_error = Some("Enter your Cloudflare Worker's URL first.".into());
            cx.notify();
            return;
        }
        self.sync_busy = Some(SyncBusy::Cloud);
        self.cloud_error = None;
        cx.notify();
        self.background(
            cx,
            async move {
                if enable { engine().sync_cloud_enable(url).await } else { engine().sync_cloud_disable().await }
            },
            |this, result, cx| {
                this.sync_busy = None;
                match result {
                    Ok(status) => this.device_sync = Some(status),
                    Err(error) => this.cloud_error = Some(error.to_string().into()),
                }
                cx.notify();
            },
        );
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

    fn start_rename(&mut self, account: &Account, window: &mut Window, cx: &mut Context<Self>) {
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
        let unchanged = self.rows().iter().any(|row| row.id == id && row.label == label);
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

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
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
                .spawn(async move { engine().suggested_client(provider.as_str().to_string()).await })
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
        self.signing_in = Some(Waiting { provider, connecting: None });
        self.note = None;
        cx.notify();
        self.background(
            cx,
            async move { engine().add_account(provider.as_str().to_string(), Some(client_id), client_secret).await },
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

    /// Asks `account`'s provider, in the browser, to let it use `service` too (MS-306).
    fn connect(&mut self, account: &Account, service: String, cx: &mut Context<Self>) {
        self.attempt += 1;
        let attempt = self.attempt;
        let id = account.id.clone();
        self.signing_in = Some(Waiting { provider: account.provider, connecting: Some((id.clone(), service.clone())) });
        self.note = None;
        cx.notify();
        let title = service_title(&service);
        self.background(cx, async move { engine().connect_account(id, service).await }, move |this, result, cx| {
            if this.attempt != attempt {
                return;
            }
            this.signing_in = None;
            match result {
                Ok(account) => {
                    this.note = Some(Note { text: format!("{title} connected for {}.", account.id).into(), error: false });
                    this.load_accounts(cx);
                    this.changed(cx);
                }
                Err(error) => this.fail(error.to_string(), cx),
            }
        });
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
        if let Some(subpage) = self.subpage.clone() {
            return div().size_full().child(subpage);
        }
        self.sync_fields(window, cx);
        self.ensure_join_input(window, cx);
        self.ensure_cloud_input(window, cx);
        let p = design::palette(window.appearance());
        let inline = self.on_close.is_some();
        let mobile = inline && f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let ts = design::type_scale(mobile);
        let body = div()
            .id("settings")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .p(px(if mobile { space::S16 } else { metrics::PADDING }))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.render_accounts(mobile, p, window, cx))
            .child(self.render_device_sync(mobile, p, window, cx))
            .children(self.render_settings(mobile, p, window, cx))
            .child(self.render_status(p));

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.on_close.is_some() {
                    window.focus(&this.focus, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(ts.body))
            .text_color(p.text)
            .bg(p.background)
            .when(inline, |d| {
                d.child(
                    div()
                        .id("settings-header")
                        .flex_shrink_0()
                        .h(px(design::compact::TOP_BAR_HEIGHT))
                        .px(px(space::S12))
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .child(
                            compact_capsule(p).child(
                                compact_icon_button("settings-back", Icon::Back, "Back (Esc)", true, p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))),
                            ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(ts.header_title))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(p.text)
                                        .child("Settings"),
                                ),
                        )
                        .child(
                            compact_capsule(p).child(
                                compact_icon_button("settings-close", Icon::Close, "Close Settings", true, p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_all(window, cx))),
                            ),
                        ),
                )
                .child(hairline(p))
            })
            .child(body)
    }
}

impl Settings {
    fn render_accounts(&self, mobile: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let ts = design::type_scale(mobile);
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
                        .text_size(px(ts.footnote))
                        .text_color(p.muted)
                        .child("Sign in to bring in mail, contacts and Google Chat."),
                )
                .into_any_element(),
            Some(Ok(rows)) => {
                let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
                let last = rows.len() - 1;
                card(p)
                    .children(rows.iter().enumerate().map(|(index, row)| self.render_row(index, row, index == last, compact, mobile, p, cx)))
                    .into_any_element()
            }
        };

        let add: AnyElement = match &self.signing_in {
            Some(waiting) => div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().text_size(px(ts.footnote)).text_color(p.muted).child(match &waiting.connecting {
                    Some((account, service)) => format!(
                        "Waiting for {} to allow {} for {account} in your browser…",
                        waiting.provider.name(),
                        service_title(service)
                    ),
                    None => format!("Waiting for the {} sign-in in your browser…", waiting.provider.name()),
                }))
                .child(
                    button("stop-waiting", "Stop Waiting", ButtonKind::Secondary, ControlSize::regular(mobile), true, p)
                        .on_click(cx.listener(|this, _, _, cx| this.stop_waiting(cx))),
                )
                .into_any_element(),
            None => div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .children([Provider::Google, Provider::Microsoft].map(|provider| {
                    button(
                        ElementId::Name(format!("add-{}", provider.as_str()).into()),
                        format!("{} Account…", provider.name()),
                        ButtonKind::Secondary,
                        ControlSize::regular(mobile),
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
            .child(heading_for("Accounts", mobile, p))
            .child(div().text_size(px(ts.footnote)).text_color(p.muted).child(
                "Mailstream signs in to Google and Microsoft in your browser, with an OAuth client you registered. \
                 Signing in asks only for mail; connect contacts, chat or calendar for an account when you want them.",
            ))
            .child(body)
            .child(add)
            .children(self.client.as_ref().map(|form| self.render_client_form(form, p, window, cx)))
            .when_some(self.note.as_ref(), |this, n| {
                this.child(
                    div()
                        .text_size(px(ts.footnote))
                        .text_color(if n.error { p.danger } else { p.muted })
                        .child(n.text.clone()),
                )
            })
    }

    fn render_client_form(&self, form: &ClientForm, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let google = form.provider == Provider::Google;
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let field = |label: &'static str, input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>| {
            let focused = input.focus_handle(cx).is_focused(window);
            let box_el = field_frame(focused, false, p)
                .overflow_hidden()
                .when(!compact, |d| d.flex_1().min_w_0())
                .when(compact, |d| d.w_full())
                .h(px(28.))
                .px(px(8.))
                .on_action(cx.listener(|this, _: &Escape, _, cx| this.cancel_client(cx)))
                .child(div().flex_1().min_w_0().child(input::field(input)));
            if compact {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(label))
                    .child(box_el)
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().w(px(104.)).flex_shrink_0().flex().justify_end().child(format!("{label}:")))
                    .child(box_el)
            }
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
        row: &Account,
        last: bool,
        compact: bool,
        mobile: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let renaming = self.renaming.as_ref().filter(|rename| rename.id == id).map(|rename| rename.input.clone());
        let confirming = self.confirming.as_ref() == Some(&id);
        let mut detail = vec![row.provider.name().to_string(), row.id.to_string()];
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

        let services = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .children(row.services.iter().map(|s| tag_for(service_title(s), None, mobile, p).into_any_element()))
            .children(row.connectable.iter().map(|service| {
                let idle = self.signing_in.is_none();
                let partly = row.services.contains(service);
                let label = if partly {
                    format!("Allow Full {}", service_title(service))
                } else {
                    format!("Connect {}…", service_title(service))
                };
                let (account, service) = (row.clone(), service.clone());
                button(
                    ElementId::Name(format!("connect:{}:{service}", row.id).into()),
                    label,
                    ButtonKind::Plain(p.accent),
                    ControlSize::small(mobile),
                    idle,
                    p,
                )
                .when(idle, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.connect(&account, service.clone(), cx)))
                })
                .into_any_element()
            }));
        let main = if compact {
            div()
                .min_h(px(metrics::ROW_HEIGHT))
                .px(px(14.))
                .py(px(10.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_shrink_0().size(px(metrics::DOT)).rounded_full().bg(design::account_color(index)))
                        .child(div().flex_1().min_w_0().child(title))
                        .child(actions),
                )
                .child(div().pl(px(18.)).text_size(px(design::type_scale(mobile).caption)).text_color(p.muted).truncate().child(detail.join(" · ")))
                .child(div().pl(px(18.)).child(services))
        } else {
            div()
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
                        .child(services),
                )
                .child(actions)
        };
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
    fn render_settings(&self, mobile: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ts = design::type_scale(mobile);
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
                .filter(|entry| shown(&entry.key) && in_section(&entry.key, prefixes))
                .map(|entry| self.render_entry(entry, mobile, p, window, cx))
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
                    .child(heading_for(*title, mobile, p))
                    .children(problem.map(|problem| notice(Tone::Error, None, problem, false, p)))
                    .children(group(rows, p))
                    .when(prefixes.contains(&"autoRead."), |d| d.children(self.render_auto_read(p, window, cx)))
                    .into_any_element(),
            );
        }
        let rest: Vec<AnyElement> = settings
            .settings
            .iter()
            .filter(|entry| shown(&entry.key) && !SECTIONS.iter().any(|(_, prefixes)| in_section(&entry.key, prefixes)))
            .map(|entry| self.render_entry(entry, mobile, p, window, cx))
            .collect();
        out.extend(group(rest, p).map(IntoElement::into_any_element));
        out.push(
            div()
                .px(px(4.))
                .text_size(px(ts.footnote))
                .text_color(p.muted)
                .child(format!("Saved in {}, shared with Mailstream and `mailstream config`.", settings.file))
                .into_any_element(),
        );
        out
    }

    fn render_entry(&self, entry: &SettingEntry, mobile: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if entry.key == TAGGING_DEVICES_MODE {
            return self.render_tagging_devices(entry, mobile, p, window, cx);
        }
        if entry.key == LOCAL_ONLY_ACCOUNTS {
            return self.render_sync_accounts(entry, p, cx);
        }
        let key = entry.key.clone();
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let range = entry.minimum.zip(entry.maximum).map(|(low, high)| format!("{low}–{high}"));
        let inline_control = matches!(entry.value, SettingValue::Flag { .. });
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
                    .flex_wrap()
                    .gap(px(4.))
                    .children(entry.choices.iter().map(|word| {
                        let selected = word == text;
                        let (key, word) = (key.clone(), word.clone());
                        button(
                            ElementId::Name(format!("{key}={word}").into()),
                            choice_title(&key, &word).to_owned(),
                            if selected { ButtonKind::Primary } else { ButtonKind::Secondary },
                            ControlSize::small(mobile),
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
            SettingValue::Text { .. } => {
                self.render_field(&key, if compact { 200. } else { 220. }, None, p, window, cx)
            }
            SettingValue::Addresses { .. } | SettingValue::Strings { .. } => None,
        };
        let reset_key = key.clone();
        let reset_btn = entry.changed.then(|| {
            button(ElementId::Name(format!("reset-{key}").into()), "Reset", ButtonKind::Plain(p.accent), ControlSize::small(mobile), true, p)
                .on_click(cx.listener(move |this, _, _, cx| this.write(reset_key.clone(), None, cx)))
        });
        let header_row = if compact && !inline_control {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(setting_label(entry, range, mobile, p))
                .when(control.is_some() || reset_btn.is_some(), |d| {
                    d.child(div().flex().items_center().gap(px(8.)).children(control).children(reset_btn))
                })
        } else {
            let reset = if compact {
                div().flex_shrink_0().children(reset_btn)
            } else {
                div().w(px(52.)).flex_shrink_0().flex().justify_end().children(reset_btn)
            };
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(setting_label(entry, range, mobile, p))
                .children(control)
                .child(reset)
        };
        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(header_row)
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
                        if this.on_close.is_some() {
                            window.focus(&this.focus, cx);
                        } else {
                            window.blur(cx);
                        }
                    }
                }))
                .child(div().flex_1().min_w_0().child(input::field(&field.input)))
                .children(suffix.map(|suffix| div().text_color(p.muted).child(suffix)))
                .into_any_element(),
        )
    }

    /// An address setting's addresses as pills, previewing at most [`settings_list::PREVIEW_LIMIT`]
    /// (5) inline with a button to open the full searchable list window / sub-page when there are
    /// more, plus a field that adds more (comma-separated).
    fn render_addresses(
        &self,
        key: &str,
        addresses: &[String],
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let add_key = format!("{key}{ADD}");
        let (shown, hidden) = settings_list::preview_slice(addresses);
        let total = addresses.len();
        let inline = self.on_close.is_some() || cfg!(target_os = "android");
        let open_key = key.to_owned();
        let open_icon = if inline { Icon::Disclosure } else { Icon::OpenInWindow };

        div()
            .pt(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(!shown.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .children(shown.iter().map(|address| {
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
                        })),
                )
            })
            .when(hidden > 0, |d| {
                let key_for_click = open_key.clone();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(text::FOOTNOTE))
                                .text_color(p.muted)
                                .child(format!("Showing {} of {total} (+{hidden} more)", shown.len())),
                        )
                        .child(
                            icon_label_button(
                                ElementId::Name(format!("view-all-{open_key}").into()),
                                Some(open_icon),
                                format!("View All ({total})"),
                                ButtonKind::Secondary,
                                ControlSize::Small,
                                true,
                                p,
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_list(SettingsListKind::Addresses { key: key_for_click.clone() }, window, cx)
                            })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(self.render_field(&add_key, 220., None, p, window, cx))
                    .child(
                        button(ElementId::Name(format!("add-{key}").into()), "Add", ButtonKind::Secondary, ControlSize::Small, true, p)
                            .on_click(cx.listener(move |this, _, window, cx| this.commit_field(&add_key, window, cx))),
                    ),
            )
    }

    /// `aiTagging.localOnlyAccounts` as a switch per mail account, on unless the account is
    /// listed: on, its AI tags and topic labels sync both ways with its Gmail labels or Outlook
    /// categories. Accounts listed but no longer signed in show too, switched off, so switching
    /// them on takes them off the list.
    fn render_sync_accounts(&self, entry: &SettingEntry, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let local_only: Vec<String> = match &entry.value {
            SettingValue::Addresses { addresses } => addresses.clone(),
            _ => Vec::new(),
        };
        let mut rows: Vec<(String, String)> = self
            .rows()
            .iter()
            .filter(|account| account.services.iter().any(|s| s == "mail"))
            .map(|account| {
                let what = if account.provider == Provider::Google { "Gmail labels" } else { "Outlook categories" };
                (account.id.to_lowercase(), format!("{}  ·  {what}", account.title()))
            })
            .collect();
        for address in &local_only {
            if !rows.iter().any(|(a, _)| a == address) {
                rows.push((address.clone(), format!("{address}  ·  not signed in")));
            }
        }
        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(div().flex().child(setting_label(entry, None, self.on_close.is_some(), p)))
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
                let on = !local_only.contains(&address);
                let next: Vec<String> = if on {
                    local_only.iter().cloned().chain([address.clone()]).collect()
                } else {
                    local_only.iter().filter(|a| **a != address).cloned().collect()
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
                            this.write(LOCAL_ONLY_ACCOUNTS.into(), Some(SettingValue::Addresses { addresses: next.clone() }), cx)
                        },
                    )))
            }))
            .children(self.render_error(LOCAL_ONLY_ACCOUNTS, p))
            .into_any_element()
    }

    /// Whether this device currently runs AI tagging automatically (`aiTagging.enabled` and
    /// included by `aiTagging.devicesMode` / `aiTagging.devices`).
    fn tagging_active_here(&self) -> bool {
        if self.flag(AI_TAGGING) != Some(true) {
            return false;
        }
        let mode = match self.entry(TAGGING_DEVICES_MODE).map(|entry| &entry.value) {
            Some(SettingValue::Text { text }) => text.as_str(),
            _ => "selected",
        };
        if mode != "selected" {
            return true;
        }
        let selected: &[String] = match self.entry(TAGGING_DEVICES).map(|entry| &entry.value) {
            Some(SettingValue::Strings { values }) => values,
            _ => &[],
        };
        if let Some(me) = self.device_sync.as_ref().and_then(|s| s.endpoint_id.as_deref()) {
            return selected.iter().any(|id| id.eq_ignore_ascii_case(me));
        }
        self.status.as_ref().is_some_and(|status| status.tagging.active_here)
    }

    /// `aiTagging.devicesMode` and `aiTagging.devices` together: choose whether all paired
    /// devices run AI tagging or only selected devices (multi-select; unselected devices still
    /// display and sync tags from the server).
    fn render_tagging_devices(
        &self,
        entry: &SettingEntry,
        mobile: bool,
        p: &'static Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let mode = match &entry.value {
            SettingValue::Text { text } => text.as_str(),
            _ => "selected",
        };
        let selected: Vec<String> = match self.entry(TAGGING_DEVICES).map(|entry| &entry.value) {
            Some(SettingValue::Strings { values }) => values.clone(),
            _ => Vec::new(),
        };
        let mode_buttons = div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .children(entry.choices.iter().map(|word| {
                let chosen = word == mode;
                let word = word.clone();
                button(
                    ElementId::Name(format!("{TAGGING_DEVICES_MODE}={word}").into()),
                    choice_title(TAGGING_DEVICES_MODE, &word).to_owned(),
                    if chosen { ButtonKind::Primary } else { ButtonKind::Secondary },
                    ControlSize::small(mobile),
                    true,
                    p,
                )
                .when(!chosen, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.write(TAGGING_DEVICES_MODE.into(), Some(SettingValue::Text { text: word.clone() }), cx)
                    }))
                })
            }));
        let reset_btn = entry.changed.then(|| {
            button(
                ElementId::Name(format!("reset-{TAGGING_DEVICES_MODE}").into()),
                "Reset",
                ButtonKind::Plain(p.accent),
                ControlSize::small(mobile),
                true,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.write(TAGGING_DEVICES_MODE.into(), None, cx)))
        });
        let header_row = if compact {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(setting_label(entry, None, mobile, p))
                .child(div().flex().items_center().gap(px(8.)).child(mode_buttons).children(reset_btn))
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(setting_label(entry, None, mobile, p))
                .child(mode_buttons)
                .child(div().w(px(52.)).flex_shrink_0().flex().justify_end().children(reset_btn))
        };

        let device_list = (mode == "selected").then(|| {
            let status = self.device_sync.as_ref();
            let me = status.and_then(|s| s.endpoint_id.as_deref());
            let mut rows: Vec<(String, String)> = Vec::new();
            if let Some(status) = status {
                let mut known: Vec<&SyncDevice> = status.devices.iter().collect();
                known.sort_by(|a, b| {
                    let a_me = me.is_some_and(|id| id.eq_ignore_ascii_case(&a.id));
                    let b_me = me.is_some_and(|id| id.eq_ignore_ascii_case(&b.id));
                    b_me.cmp(&a_me)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                        .then_with(|| a.id.cmp(&b.id))
                });
                for device in known {
                    let id = device.id.to_lowercase();
                    let is_me = me.is_some_and(|m| m.eq_ignore_ascii_case(&id));
                    let label = if is_me {
                        format!("{} (This device)  ·  {}", device.name, platform_label(&device.platform))
                    } else {
                        format!("{}  ·  {}", device.name, platform_label(&device.platform))
                    };
                    rows.push((id, label));
                }
            }
            if let Some(my_id) = me {
                let my_lower = my_id.to_lowercase();
                if !rows.iter().any(|(id, _)| *id == my_lower) {
                    rows.push((
                        my_lower,
                        format!("This device  ·  {}  ·  ID {}", platform_label(std::env::consts::OS), short_id(my_id)),
                    ));
                }
            }
            for saved in &selected {
                let lower = saved.to_lowercase();
                if !rows.iter().any(|(id, _)| *id == lower) {
                    rows.push((lower, format!("Device {}  ·  not in current sync list", short_id(saved))));
                }
            }

            let sync_hint: Option<&'static str> = match status {
                None => Some("Loading paired devices…"),
                Some(status) if status.namespace.is_none() => {
                    Some("Enable Device sync below to see and pick your paired devices.")
                }
                Some(status) if status.paused => {
                    Some("Device sync is paused — turn it on below to see all paired devices.")
                }
                Some(_) => None,
            };
            let empty_hint = selected.is_empty().then_some(
                "No devices selected. Automatic AI tagging is paused on all devices; existing and server tags still display and sync.",
            );

            div()
                .pt(px(6.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(rows.into_iter().map(|(id, label)| {
                    let on = selected.iter().any(|item| item.eq_ignore_ascii_case(&id));
                    let next: Vec<String> = if on {
                        selected.iter().filter(|item| !item.eq_ignore_ascii_case(&id)).cloned().collect()
                    } else {
                        selected.iter().cloned().chain([id.clone()]).collect()
                    };
                    div()
                        .pt(px(6.))
                        .when(!compact, |d| d.pr(px(64.)))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .child(
                            switch(ElementId::Name(format!("tagger-{id}").into()), on).on_click(cx.listener(
                                move |this, _: &bool, _, cx| {
                                    this.write(
                                        TAGGING_DEVICES.into(),
                                        Some(SettingValue::Strings { values: next.clone() }),
                                        cx,
                                    )
                                },
                            )),
                        )
                }))
                .children(empty_hint.map(|text| {
                    div().pt(px(4.)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(text)
                }))
                .children(sync_hint.map(|text| {
                    div().pt(px(2.)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(text)
                }))
        });

        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(header_row)
            .children(device_list)
            .children(self.render_error(TAGGING_DEVICES_MODE, p))
            .children(self.render_error(TAGGING_DEVICES, p))
            .into_any_element()
    }

    /// GPUI's own row in AI tagging: tag every conversation again.
    fn render_retag(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let ai_on = self.flag(AI_TAGGING) == Some(true);
        let active_here = self.tagging_active_here();
        let enabled = ai_on && active_here && !self.retag_started;
        let note = if self.retag_started {
            "Re-tagging started."
        } else if ai_on && !active_here {
            "This device is not selected for AI tagging. Run Re-tag on a selected tagging device."
        } else {
            "Tag every conversation again, replacing the tags the model gave before."
        };
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(10.))
            .child(div().flex_1().min_w_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(note))
            .child(
                button("retag", "Re-tag All Conversations", ButtonKind::Secondary, ControlSize::Regular, enabled, p)
                    .when(enabled, |b| b.on_click(cx.listener(|this, _, _, cx| this.retag(cx)))),
            )
            .into_any_element()
    }

    /// What auto-read marked read and nobody undid, newest first: previews at most the 5 latest
    /// rows inline with per-row Mark Unread, and offers "View All (N)" to open the full searchable
    /// list window (or in-window sub-page in compact / Android mode).
    fn render_auto_read(&self, p: &'static Palette, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let log = self.auto_read.as_ref()?;
        let enabled = self.flag(AUTO_READ).unwrap_or(false);
        if log.entries.is_empty() && !enabled {
            return None;
        }
        let inline = self.on_close.is_some() || cfg!(target_os = "android");
        let narrow = inline && f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let days = match self.entry(AUTO_READ_DAYS).map(|entry| &entry.value) {
            Some(SettingValue::Number { value }) => *value,
            _ => log.days,
        };
        let count = log.entries.len();
        let (shown, hidden) = settings_list::preview_slice(&log.entries);
        let summary = if count == 0 {
            format!("Nothing yet. Unread notifications and newsletters older than {days} days show here once marked read.")
        } else {
            format!(
                "{count} conversation{}. Marking one unread keeps auto-read away from it until new mail arrives.",
                if count == 1 { "" } else { "s" }
            )
        };
        let all: Vec<String> = log.entries.iter().map(|entry| entry.id.clone()).collect();
        let title_block = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child("Recently marked read")
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(summary));
        let mark_all_btn = (count > 0).then(|| {
            button("auto-read-all", "Mark All Unread", ButtonKind::Secondary, ControlSize::Regular, true, p)
                .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(all.clone(), cx)))
        });
        let header_top = if narrow {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(title_block)
                .when_some(mark_all_btn, |d, btn| d.child(div().flex().justify_end().child(btn)))
        } else {
            div().flex().items_center().gap(px(12.)).child(title_block).children(mark_all_btn)
        };
        let header = div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .child(header_top)
            .children(self.render_error(AUTO_READ_LOG, p))
            .into_any_element();
        let rows = shown.iter().enumerate().map(|(index, entry)| {
            let detail = settings_list::format_auto_read_detail(entry);
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
                    icon_button(("auto-read", index), Icon::MarkUnread, "Mark Unread", true, p.glyph)
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(vec![id.clone()], cx))),
                )
                .into_any_element()
        });
        let footer = (hidden > 0).then(|| {
            let open_icon = if inline { Icon::Disclosure } else { Icon::OpenInWindow };
            div()
                .px(px(14.))
                .py(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child(format!("Showing {} latest of {count}", shown.len())),
                )
                .child(
                    icon_label_button(
                        "auto-read-view-all",
                        Some(open_icon),
                        format!("View All ({count})"),
                        ButtonKind::Secondary,
                        ControlSize::Small,
                        true,
                        p,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_list(SettingsListKind::AutoRead, window, cx))),
                )
                .into_any_element()
        });
        group(std::iter::once(header).chain(rows).chain(footer).collect(), p)
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
            status.last_refresh.map_or("Not yet".into(), |at| mailstream::message_date(Some(at.to_rfc3339())))
        };
        let tagging = match &status.tagging {
            mailstream::TaggingStatus { enabled: false, .. } => "Off".to_string(),
            mailstream::TaggingStatus { active_here: false, .. } => {
                "Synced only (tagged by selected devices)".to_string()
            }
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

    fn render_device_sync(&self, mobile: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let ts = design::type_scale(mobile);
        let section = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(heading_for("Device sync", mobile, p))
            .child(div().text_size(px(ts.footnote)).text_color(p.muted).child(
                "Syncs your accounts, trusted senders, auto-read, and AI tagging preferences directly between \
                 your devices with iroh; sign-ins travel encrypted for each device. Off until enabled or paired, \
                 so Mailstream does not connect to a relay on its own.",
            ));
        let Some(status) = &self.device_sync else {
            return section
                .child(note("Loading device sync…", p))
                .when_some(self.sync_error.clone(), |d, error| {
                    d.child(div().text_size(px(ts.footnote)).text_color(p.danger).child(error))
                });
        };

        let available = status.available;
        let interactive = available && self.sync_busy.is_none();
        let grouped = status.namespace.is_some();
        let enabled = status.enabled;

        let status_line = if !available {
            // The engine's reason: another process holds sync, opening failed, or this mode has none.
            status.problem.as_ref().map_or_else(|| "Unavailable.".to_owned(), |problem| format!("Unavailable · {problem}"))
        } else if enabled {
            match &status.last_sync {
                Some(at) => format!("On · Last synced {}", mailstream::message_date(Some(at.clone()))),
                None if status.devices.len() > 1 => "On · Paired".to_owned(),
                None => "On · Waiting for another device to join".to_owned(),
            }
        } else if status.paused {
            "Paused · Local changes wait until you turn sync back on.".to_owned()
        } else {
            "Off · Not connected to any relay or sync group.".to_owned()
        };

        let toggle_row = div()
            .px(px(14.))
            .py(px(10.))
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
                    .child(div().font_weight(FontWeight::MEDIUM).child("Cross-device settings sync"))
                    .child(div().text_size(px(ts.footnote)).text_color(p.muted).child(status_line)),
            )
            .child(
                switch("device-sync-toggle", enabled)
                    .disabled(!interactive)
                    .when(interactive, |s| {
                        s.on_click(cx.listener(move |this, _: &bool, _, cx| this.toggle_device_sync(!enabled, cx)))
                    }),
            )
            .into_any_element();

        let this_device = status
            .endpoint_id
            .as_ref()
            .and_then(|id| status.devices.iter().find(|device| &device.id == id));
        let this_title = this_device.map_or_else(|| "This device".to_owned(), |device| device.name.clone());
        let this_detail = match (this_device, &status.endpoint_id) {
            (Some(device), Some(id)) => format!("{} · ID {}", platform_label(&device.platform), short_id(id)),
            (None, Some(id)) => format!("{} · ID {}", platform_label(std::env::consts::OS), short_id(id)),
            (_, None) => "Endpoint ID appears once device sync opens.".to_owned(),
        };
        let sync_now_label = if self.sync_busy == Some(SyncBusy::Now) { "Syncing…" } else { "Sync Now" };
        let sync_now_enabled = interactive && enabled;
        let this_row = div()
            .px(px(14.))
            .py(px(10.))
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
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(this_title))
                    .child(div().text_size(px(ts.caption)).text_color(p.muted).truncate().child(this_detail)),
            )
            .child(
                button("sync-now", sync_now_label, ButtonKind::Secondary, ControlSize::regular(mobile), sync_now_enabled, p)
                    .when(sync_now_enabled, |b| b.on_click(cx.listener(|this, _, _, cx| this.sync_now(cx)))),
            )
            .into_any_element();

        let mut rows = vec![toggle_row, this_row];

        if grouped {
            let peers: Vec<&SyncDevice> =
                status.devices.iter().filter(|device| Some(&device.id) != status.endpoint_id.as_ref()).collect();
            let devices_row = div()
                .px(px(14.))
                .py(px(10.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_size(px(text::FOOTNOTE)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child("Paired devices"))
                .when(peers.is_empty(), |d| {
                    // Paused sync is closed (MS-370), so the engine can't list the group's devices.
                    d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(if status.paused {
                        "Paired devices show again once you turn sync back on."
                    } else {
                        "No other devices paired yet. Create a pairing code below or paste one from another device."
                    }))
                })
                .children(peers.into_iter().map(|peer| {
                    let seen = mailstream::message_date(Some(peer.last_seen.clone()));
                    let mut meta = format!("{} · ID {} · Last synced {seen}", platform_label(&peer.platform), short_id(&peer.id));
                    if peer.accounts {
                        meta.push_str(" · Has your accounts");
                    }
                    let id = peer.id.clone();
                    let device = div()
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
                                .child(div().font_weight(FontWeight::MEDIUM).truncate().child(peer.name.clone()))
                                .child(div().text_size(px(text::CAPTION)).text_color(p.muted).truncate().child(meta)),
                        )
                        .child(
                            button(
                                ElementId::Name(format!("remove-sync-{id}").into()),
                                "Remove…",
                                ButtonKind::Plain(p.danger),
                                ControlSize::Small,
                                interactive,
                                p,
                            )
                            .when(interactive, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.sync_confirm_remove = Some(id.clone());
                                    this.sync_confirm_reset = false;
                                    cx.notify();
                                }))
                            }),
                        );
                    let confirming = self.sync_confirm_remove.as_ref() == Some(&peer.id);
                    div().flex().flex_col().gap(px(8.)).child(device).when(confirming, |d| {
                        let id = peer.id.clone();
                        d.child(
                            div()
                                .p(px(12.))
                                .rounded(px(radius::CONTROL))
                                .bg(design::tint(p.danger, design::notice::TINT))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Remove {}?", peer.name)))
                                .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(
                                    "It stops syncing with your other devices, can't get mail from them and gets no \
                                     new sign-ins; your other devices keep syncing. It keeps the sign-ins it already \
                                     has: to cut it off, remove Mailstream's access in your Google or Microsoft \
                                     account settings. You can pair it again later.",
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap(px(6.))
                                        .child(
                                            button("remove-sync-cancel", "Cancel", ButtonKind::Secondary, ControlSize::Regular, true, p)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sync_confirm_remove = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            button(
                                                "remove-sync-confirm",
                                                if self.sync_busy == Some(SyncBusy::Remove) { "Removing…" } else { "Remove Device" },
                                                ButtonKind::Destructive,
                                                ControlSize::Regular,
                                                interactive,
                                                p,
                                            )
                                            .when(interactive, |b| {
                                                b.on_click(cx.listener(move |this, _, _, cx| this.remove_sync_device(id.clone(), cx)))
                                            }),
                                        ),
                                ),
                        )
                    })
                }))
                .into_any_element();
            rows.push(devices_row);
            rows.push(self.render_cloud(status, interactive, p, window, cx));
        }

        let pair_block: AnyElement = match &self.sync_ticket {
            Some(ticket) => {
                let left = ticket.left();
                let (title, detail): (SharedString, SharedString) = match left {
                    Some(left) => (
                        format!("Pairing code · expires in {}:{:02}", left.num_minutes(), left.num_seconds() % 60).into(),
                        "Paste it on your other device and click Join, or run `mailstream sync join '<code>'` there. \
                         It works once, and this device must stay open until the other one joins. The device that \
                         joins also gets your signed-in accounts, without signing in again."
                            .into(),
                    ),
                    None => (
                        "Pairing code expired".into(),
                        "Codes work once, for 10 minutes. Create a new one when the other device is ready.".into(),
                    ),
                };
                let live = left.is_some();
                div()
                    .p(px(10.))
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.background)
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(detail))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(26.))
                                    .px(px(8.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(radius::SMALL))
                                    .border_1()
                                    .border_color(p.line)
                                    .bg(p.fill)
                                    .font_family(design::mono_font())
                                    .text_size(px(text::CAPTION))
                                    .when(!live, |d| d.text_color(p.muted).line_through())
                                    .child(div().min_w_0().flex_1().truncate().child(ticket.code.clone())),
                            )
                            .child(if live {
                                button(
                                    "copy-sync-ticket",
                                    if self.ticket_copied { "Copied" } else { "Copy" },
                                    ButtonKind::Primary,
                                    ControlSize::Small,
                                    true,
                                    p,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.copy_sync_ticket(cx)))
                            } else {
                                button("new-sync-ticket", "New Code", ButtonKind::Primary, ControlSize::Small, interactive, p)
                                    .when(interactive, |b| b.on_click(cx.listener(|this, _, _, cx| this.pair_device(cx))))
                            })
                        .child(
                            button("done-sync-ticket", "Done", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.sync_ticket = None;
                                    this.ticket_copied = false;
                                    cx.notify();
                                }),
                            ),
                        ),
                    )
                    .into_any_element()
            }
            None => {
                let pair_label = if self.sync_busy == Some(SyncBusy::Pair) { "Creating…" } else { "Pair Another Device" };
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
                            .child(div().font_weight(FontWeight::MEDIUM).child("Pair another device"))
                            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                                "Creates a one-time code for your other device, valid for 10 minutes. The device \
                                 that joins with it also gets your signed-in accounts.",
                            )),
                    )
                    .child(
                        button("pair-device", pair_label, ButtonKind::Secondary, ControlSize::Regular, interactive, p)
                            .when(interactive, |b| b.on_click(cx.listener(|this, _, _, cx| this.pair_device(cx)))),
                    )
                    .into_any_element()
            }
        };

        let join_label = if self.sync_busy == Some(SyncBusy::Join) { "Joining…" } else { "Join" };
        let join_row = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                "Or paste a pairing code from a device already in a sync group:",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when_some(self.join_input.as_ref(), |d, input| {
                        let focused = input.focus_handle(cx).is_focused(window);
                        d.child(
                            field_frame(focused, false, p)
                                .overflow_hidden()
                                .flex_1()
                                .min_w_0()
                                .h(px(26.))
                                .px(px(8.))
                                .child(div().flex_1().min_w_0().child(input::field(input))),
                        )
                    })
                    .child(
                        button("join-sync", join_label, ButtonKind::Secondary, ControlSize::Regular, interactive, p)
                            .when(interactive, |b| b.on_click(cx.listener(|this, _, window, cx| this.join_sync(window, cx)))),
                    ),
            );

        rows.push(
            div()
                .px(px(14.))
                .py(px(10.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(pair_block)
                .child(join_row)
                .into_any_element(),
        );

        if grouped {
            let reset_row = if self.sync_confirm_reset {
                div()
                    .m(px(10.))
                    .p(px(12.))
                    .rounded(px(radius::CONTROL))
                    .bg(design::tint(p.danger, design::notice::TINT))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Reset sync group?"))
                    .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(
                        "Starts a new sync group carrying over your synced settings and accounts; pair every other \
                         device again. To drop just one device, remove it under Paired devices instead.",
                    ))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(6.))
                            .child(
                                button("reset-sync-cancel", "Cancel", ButtonKind::Secondary, ControlSize::Regular, true, p)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_confirm_reset = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                button(
                                    "reset-sync-confirm",
                                    if self.sync_busy == Some(SyncBusy::Reset) { "Resetting…" } else { "Reset Sync Group" },
                                    ButtonKind::Destructive,
                                    ControlSize::Regular,
                                    interactive,
                                    p,
                                )
                                .when(interactive, |b| b.on_click(cx.listener(|this, _, _, cx| this.reset_sync_group(cx)))),
                            ),
                    )
                    .into_any_element()
            } else {
                div()
                    .px(px(14.))
                    .py(px(10.))
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
                            .child(div().font_weight(FontWeight::MEDIUM).child("Reset sync group"))
                            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                                "Start over with a new group that every device must pair with again.",
                            )),
                    )
                    .child(
                        button("reset-sync", "Reset Sync Group…", ButtonKind::Plain(p.danger), ControlSize::Small, interactive, p)
                            .when(interactive, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| {
                                    this.sync_confirm_reset = true;
                                    this.sync_confirm_remove = None;
                                    cx.notify();
                                }))
                            }),
                    )
                    .into_any_element()
            };
            rows.push(reset_row);
        }

        // Unavailable: the status line already gives the reason.
        let problem_notice = status.problem.as_ref().filter(|_| available).map(|problem| notice(Tone::Error, None, problem.clone(), false, p));

        section
            .children(problem_notice)
            .children(group(rows, p))
            .when_some(self.sync_error.clone(), |d, error| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.danger).child(error))
            })
    }

    /// The Cloudflare drop (MS-374): its switch, state, the Worker's URL and what it stores.
    fn render_cloud(
        &self,
        status: &DeviceSyncStatus,
        interactive: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cloud = &status.cloud;
        let on = cloud.enabled;
        let date = |at: &String| mailstream::message_date(Some(at.clone()));
        let state = if self.sync_busy == Some(SyncBusy::Cloud) {
            "Contacting the Worker…".to_owned()
        } else if !on {
            "Off · Changes reach another device only while both are online.".to_owned()
        } else if !status.enabled {
            "On · Waits until device sync is on again.".to_owned()
        } else {
            match (&cloud.last_upload, &cloud.last_received) {
                (Some(up), Some(down)) => format!("On · Uploaded {} · Received {}", date(up), date(down)),
                (Some(up), None) => format!("On · Uploaded {}", date(up)),
                (None, Some(down)) => format!("On · Received {}", date(down)),
                (None, None) => "On".to_owned(),
            }
        };
        div()
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
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
                            .child(div().font_weight(FontWeight::MEDIUM).child("Cloudflare fallback"))
                            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(state)),
                    )
                    .child(
                        switch("cloud-drop-toggle", on)
                            .disabled(!interactive)
                            .when(interactive, |s| s.on_click(cx.listener(move |this, _: &bool, _, cx| this.toggle_cloud(!on, cx)))),
                    ),
            )
            .when_some(self.cloud_input.as_ref(), |d, input| {
                let focused = input.focus_handle(cx).is_focused(window);
                d.child(
                    field_frame(focused, false, p)
                        .overflow_hidden()
                        .h(px(26.))
                        .px(px(8.))
                        .child(div().flex_1().min_w_0().child(input::field(input))),
                )
            })
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(
                "Shared across this sync group: turning this on or off applies to every device in the group, and \
                 devices that join inherit it. Keeps an encrypted copy of the synced settings and accounts on a \
                 Cloudflare Worker (SyncDrop/ in the Mailstream repository), so devices that are never online at \
                 the same time still sync. The Worker sees only ciphertext under ids derived from this sync group; \
                 turning this off or resetting the group deletes stored copies. Off for new groups until turned on.",
            ))
            .when_some(self.cloud_error.clone().or_else(|| cloud.problem.clone().map(SharedString::from)), |d, problem| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.danger).child(problem))
            })
            .into_any_element()
    }
}

// MARK: - Pieces

/// A section's title (Flutter's `_Heading`): bold secondary text over its group.
fn heading(title: impl Into<SharedString>, p: &Palette) -> Div {
    heading_for(title, false, p)
}

fn heading_for(title: impl Into<SharedString>, mobile: bool, p: &Palette) -> Div {
    let ts = design::type_scale(mobile);
    div().pt(px(8.)).px(px(4.)).text_size(px(ts.footnote)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(title.into())
}

pub(crate) fn card(p: &Palette) -> Div {
    div().flex().flex_col().rounded(px(radius::CARD)).border(px(stroke::HAIRLINE)).border_color(p.line).bg(p.fill)
}

/// Rows in a card divided by hairlines (Flutter's `_Group`); nothing when there are none.
pub(crate) fn group(rows: Vec<AnyElement>, p: &Palette) -> Option<Div> {
    (!rows.is_empty()).then(|| {
        card(p).children(
            rows.into_iter().enumerate().map(|(index, row)| div().when(index > 0, |d| d.border_t_1().border_color(p.line)).child(row)),
        )
    })
}

/// A setting's name over the engine's description and its key (with the range a number takes).
fn setting_label(entry: &SettingEntry, range: Option<String>, mobile: bool, p: &Palette) -> Div {
    let ts = design::type_scale(mobile);
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
        .child(div().text_size(px(ts.footnote)).text_color(p.muted).child(entry.summary.clone()))
        .child(div().text_size(px(ts.caption)).text_color(p.faint).child(key))
}

pub(crate) fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
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

fn short_id(id: &str) -> String {
    let count = id.chars().count();
    if count <= 14 {
        return id.to_owned();
    }
    let head: String = id.chars().take(8).collect();
    let tail: String = id.chars().skip(count - 4).collect();
    format!("{head}…{tail}")
}

fn platform_label(platform: &str) -> &str {
    match platform {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        "android" => "Android",
        other => other,
    }
}
