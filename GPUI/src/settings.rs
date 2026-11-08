//! The Settings window (`AccountsWindowController`): signed-in accounts (rename, sign out, add),
//! the app's status, and the settings kept in settings.json.
//!
//! Accounts and status come from the in-process engine. `config.*` settings are local to the CLI:
//! it edits `~/Library/Application Support/Mailstream/settings.json` and the running app follows
//! the file, so this window does the same (`SettingsFile.update`): a flat object of dotted keys
//! holding only values changed from their default, unknown keys kept as they are.

use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, actions, AppContext, Bounds, Focusable, Context, Div, ElementId, Entity, FontWeight, Global, Hsla, KeyBinding,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, px, size,
};
use serde_json::{Map, Value};

use crate::engine::{MailError, engine};
use crate::design::{self, Palette, UI_FONT, radius, settings as metrics, text};
use crate::format;
use crate::input::{InputEvent, TextInput};

actions!(settings, [ShowSettings]);

/// Binds cmd-, to `ShowSettings`, which opens the window with `on_changed` (see `open_settings`).
pub fn init(on_changed: impl Fn(&mut App) + 'static, cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-,", ShowSettings, None)]);
    let on_changed = Rc::new(on_changed);
    cx.on_action(move |_: &ShowSettings, cx| {
        let on_changed = on_changed.clone();
        open_settings(move |cx| on_changed(cx), cx);
    });
}

/// Opens the Settings window, or brings the open one forward. `on_changed` runs after an account
/// is renamed, removed or added, or a setting changes, so the main window can reload.
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
    let Ok(handle) = cx.open_window(options, |window, cx| cx.new(|cx| Settings::new(on_changed, window, cx))) else {
        return;
    };
    cx.set_global(OpenSettings(handle));
    handle.update(cx, |_, window, _| window.activate_window()).ok();
}

struct OpenSettings(WindowHandle<Settings>);

impl Global for OpenSettings {}

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
    settings_issues: Vec<String>,
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
            settings_issues: status.settings_issues,
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

// MARK: - settings.json

const AI_TAGGING: &str = "aiTagging.enabled";
/// `Setting.aiTaggingEnabled`'s default.
const AI_TAGGING_DEFAULT: bool = true;

fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/Mailstream/settings.json")
}

/// The file's object; empty when there is no file or only whitespace.
fn read_settings() -> Result<Map<String, Value>, String> {
    let path = settings_path();
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    if data.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(&data) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(format!("{} must hold a JSON object.", path.display())),
        Err(error) => Err(format!("{} is not valid JSON: {error}", path.display())),
    }
}

/// The value in effect: an invalid or absent value means the default.
fn ai_tagging(settings: &Map<String, Value>) -> bool {
    settings.get(AI_TAGGING).and_then(Value::as_bool).unwrap_or(AI_TAGGING_DEFAULT)
}

/// Sets the key, keeping everything else; the default value removes it. Written atomically.
fn write_ai_tagging(enabled: bool) -> Result<bool, String> {
    let mut settings = read_settings()?;
    if enabled == AI_TAGGING_DEFAULT {
        settings.remove(AI_TAGGING);
    } else {
        settings.insert(AI_TAGGING.into(), Value::Bool(enabled));
    }
    let path = settings_path();
    let failed = |error: std::io::Error| format!("Could not write {}: {error}", path.display());
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(failed)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(settings)).expect("a JSON value always encodes");
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(failed)?;
    std::fs::rename(&temporary, &path).map_err(failed)?;
    Ok(enabled)
}

// MARK: - The window

struct Settings {
    on_changed: Rc<dyn Fn(&mut App)>,
    accounts: Option<Result<Vec<AccountRow>, MailError>>,
    status: Option<Status>,
    /// `aiTagging.enabled` as the file says; None until read.
    ai_tagging: Option<bool>,
    /// The account being renamed and its field.
    renaming: Option<(SharedString, Entity<TextInput>)>,
    /// The account whose sign-out waits for confirmation.
    confirming: Option<SharedString>,
    /// The provider being signed in to, and the attempt it belongs to.
    signing_in: Option<Provider>,
    attempt: u64,
    retag_started: bool,
    note: Option<Note>,
    _subscriptions: Vec<Subscription>,
}

impl Settings {
    fn new(on_changed: Rc<dyn Fn(&mut App)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let this = Self {
            on_changed,
            accounts: None,
            status: None,
            ai_tagging: None,
            renaming: None,
            confirming: None,
            signing_in: None,
            attempt: 0,
            retag_started: false,
            note: None,
            _subscriptions: vec![cx.observe_window_appearance(window, |_, _, cx| cx.notify())],
        };
        this.reload(cx);
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

    fn reload(&self, cx: &mut Context<Self>) {
        self.background(
            cx,
            async {
                let accounts = engine().accounts().await.map(|list| list.into_iter().map(AccountRow::from).collect());
                let status = engine().status().await.ok().map(Status::from);
                let ai = read_settings().map(|s| ai_tagging(&s));
                (accounts, status, ai)
            },
            |this, (accounts, status, ai), cx| {
                this.accounts = Some(accounts);
                this.status = status;
                match ai {
                    Ok(enabled) => this.ai_tagging = Some(enabled),
                    Err(message) => this.note = Some(Note { text: message.into(), error: true }),
                }
                cx.notify();
            },
        );
    }

    fn changed(&self, cx: &mut Context<Self>) {
        self.reload(cx);
        let on_changed = self.on_changed.clone();
        cx.defer(move |cx| on_changed(cx));
    }

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note = Some(Note { text: message.into(), error: true });
        cx.notify();
    }

    // MARK: Rename

    fn start_rename(&mut self, account: &AccountRow, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(account.id.clone(), cx));
        input.update(cx, |input, cx| input.set_text(account.label.clone(), cx));
        self._subscriptions.push(cx.subscribe_in(&input, window, Self::on_rename_event));
        window.focus(&input.read(cx).focus_handle(cx));
        self.confirming = None;
        self.renaming = Some((account.id.clone(), input));
        cx.notify();
    }

    fn on_rename_event(
        &mut self,
        input: &Entity<TextInput>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Submit => {
                let label = input.read(cx).text().trim().to_string();
                self.commit_rename(label, cx);
            }
            InputEvent::Cancel => self.cancel_rename(cx),
            InputEvent::Changed => {}
        }
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        self._subscriptions.truncate(1);
        cx.notify();
    }

    fn commit_rename(&mut self, label: String, cx: &mut Context<Self>) {
        let Some((id, _)) = self.renaming.take() else { return };
        self._subscriptions.truncate(1);
        let unchanged = self.rows().iter().any(|row| row.id == id && row.label.as_ref() == label);
        cx.notify();
        if unchanged || label.is_empty() {
            return;
        }
        let account = id.to_string();
        self.background(
            cx,
            async move { engine().rename_account(account, label).await },
            |this, result, cx| match result {
                Ok(_) => {
                    this.note = None;
                    this.changed(cx);
                }
                Err(error) => this.fail(error.to_string(), cx),
            },
        );
    }

    // MARK: Sign out

    fn remove(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.confirming = None;
        cx.notify();
        let account = id.to_string();
        self.background(
            cx,
            async move { engine().remove_account(account, true).await },
            move |this, result, cx| match result {
                Ok(_) => {
                    this.note = Some(Note { text: format!("Signed out of {id}.").into(), error: false });
                    this.changed(cx);
                }
                Err(error) => this.fail(error.to_string(), cx),
            },
        );
    }

    // MARK: Sign in

    fn sign_in(&mut self, provider: Provider, cx: &mut Context<Self>) {
        self.attempt += 1;
        let attempt = self.attempt;
        self.signing_in = Some(provider);
        self.note = None;
        cx.notify();
        self.background(
            cx,
            async move { engine().add_account(provider.key().to_string(), None, None).await },
            move |this, result, cx| {
                // The user stopped waiting, or started over.
                if this.attempt != attempt {
                    return;
                }
                this.signing_in = None;
                match result {
                    Ok(account) => {
                        this.note = Some(Note { text: format!("Signed in as {}.", account.id).into(), error: false });
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

    // MARK: Settings and status

    fn set_ai_tagging(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.ai_tagging = Some(enabled);
        cx.notify();
        self.background(
            cx,
            async move { write_ai_tagging(enabled) },
            |this, result, cx| match result {
                Ok(_) => this.changed(cx),
                Err(message) => {
                    this.fail(message, cx);
                    this.reload(cx);
                }
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
                Ok(_) => this.reload(cx),
                Err(error) => {
                    this.retag_started = false;
                    this.fail(error.to_string(), cx);
                }
            },
        );
    }

    fn rows(&self) -> &[AccountRow] {
        match &self.accounts {
            Some(Ok(rows)) => rows,
            _ => &[],
        }
    }
}

// MARK: - Rendering

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.page)
            .p(px(metrics::PADDING))
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(self.render_accounts(p, cx))
            .child(self.render_tagging(p, cx))
            .child(self.render_status(p))
    }
}

impl Settings {
    fn render_accounts(&self, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let heading = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("Accounts"))
            .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(
                "Mailstream signs in to Google and Microsoft in your browser, with an OAuth client you registered. \
                 Sign-ins stay in your Keychain.",
            ));

        let body: AnyElement = match &self.accounts {
            None => note("Loading accounts…", p).into_any_element(),
            Some(Err(error)) => note(error.to_string(), p).into_any_element(),
            Some(Ok(rows)) if rows.is_empty() => div()
                .py(px(24.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("No Accounts"))
                .child(
                    div()
                        .text_size(px(text::CAPTION))
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
                .child(button("stop-waiting", "Stop Waiting", p).on_click(cx.listener(|this, _, _, cx| this.stop_waiting(cx))))
                .into_any_element(),
            None => div()
                .flex()
                .gap(px(8.))
                .child(
                    button("add-google", "Sign in with Google…", p)
                        .on_click(cx.listener(|this, _, _, cx| this.sign_in(Provider::Google, cx))),
                )
                .child(
                    button("add-microsoft", "Sign in with Microsoft…", p)
                        .on_click(cx.listener(|this, _, _, cx| this.sign_in(Provider::Microsoft, cx))),
                )
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(heading)
            .child(body)
            .child(add)
            .when_some(self.note.as_ref(), |this, n| {
                this.child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(if n.error { p.danger } else { p.muted })
                        .child(n.text.clone()),
                )
            })
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
        let renaming = self.renaming.as_ref().filter(|(r, _)| *r == id).map(|(_, input)| input.clone());
        let confirming = self.confirming.as_ref() == Some(&id);
        let mut detail = vec![row.provider_name().to_string(), row.id.to_string()];
        if let Some(name) = row.name.as_ref().filter(|n| !n.is_empty()) {
            detail.insert(1, name.to_string());
        }

        let title: AnyElement = match renaming {
            Some(input) => div()
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(radius::SMALL))
                .border_1()
                .border_color(p.accent)
                .child(input)
                .into_any_element(),
            None => div().font_weight(FontWeight::MEDIUM).truncate().child(row.title()).into_any_element(),
        };

        let actions: AnyElement = if self.renaming.as_ref().is_some_and(|(r, _)| *r == id) {
            div()
                .flex()
                .gap(px(4.))
                .child(button(("rename-cancel", index), "Cancel", p).on_click(cx.listener(|this, _, _, cx| this.cancel_rename(cx))))
                .child(button(("rename-save", index), "Save", p).text_color(p.accent).on_click(cx.listener(
                    |this, _, _, cx| {
                        let label = this.renaming.as_ref().map(|(_, i)| i.read(cx).text().trim().to_string());
                        if let Some(label) = label {
                            this.commit_rename(label, cx);
                        }
                    },
                )))
                .into_any_element()
        } else {
            let account = row.clone();
            let remove_id = id.clone();
            div()
                .flex()
                .gap(px(4.))
                .child(button(("rename", index), "Rename", p).on_click(cx.listener(move |this, _, window, cx| {
                    this.start_rename(&account, window, cx)
                })))
                .child(button(("remove", index), "Sign Out…", p).text_color(p.danger).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.cancel_rename(cx);
                        this.confirming = Some(remove_id.clone());
                    },
                )))
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
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .children(row.services.iter().map(|s| service_chip(s, p))),
                    ),
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
                        .bg(tint(p.danger))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Sign out of {}?", row.title())))
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
                                .child(button(("remove-cancel", index), "Cancel", p).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.confirming = None;
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    button(("remove-confirm", index), "Sign Out", p)
                                        .bg(p.danger)
                                        .text_color(p.on_accent)
                                        .on_click(cx.listener(move |this, _, _, cx| this.remove(remove_id.clone(), cx))),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_tagging(&self, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let enabled = self.ai_tagging;
        let toggle = div()
            .id("ai-tagging")
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(10.))
            .cursor_pointer()
            .when_some(enabled, |this, on| {
                this.on_click(cx.listener(move |this, _, _, cx| this.set_ai_tagging(!on, cx)))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("AI tagging"))
                    .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(
                        "Sort the inbox into People, Notifications, Newsletters and Invitations with AI (omp, or \
                         Apple Intelligence with aiTagging.backend apple), and flag mail waiting on your reply or action.",
                    )),
            )
            .child(switch(enabled.unwrap_or(false), p));

        let retag = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(10.))
            .border_t_1()
            .border_color(p.line)
            .child(div().flex_1().min_w_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(if self.retag_started {
                "Re-tagging started."
            } else {
                "Tag every conversation again, replacing the tags the model gave before."
            }))
            .child(
                button("retag", "Re-tag All Conversations", p)
                    .when(enabled != Some(true) || self.retag_started, |this| this.opacity(0.5))
                    .when(enabled == Some(true) && !self.retag_started, |this| {
                        this.on_click(cx.listener(|this, _, _, cx| this.retag(cx)))
                    }),
            );

        section("Inbox Sorting", p).child(card(p).child(toggle).child(retag))
    }

    fn render_status(&self, p: &'static Palette) -> Div {
        let Some(status) = &self.status else {
            return section("Status", p).child(note("Mailstream is not answering.", p));
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
        for issue in &status.settings_issues {
            rows.push(("Settings", issue.clone(), Some(p.danger)));
        }
        let last = rows.len() - 1;
        section("Status", p).child(card(p).children(rows.into_iter().enumerate().map(|(i, (label, value, color))| {
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

fn section(title: &'static str, p: &Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
}

fn card(p: &Palette) -> Div {
    div().flex().flex_col().rounded(px(radius::CARD)).border_1().border_color(p.line).bg(p.sidebar)
}

fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().py(px(12.)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(text.into())
}

fn tint(color: Hsla) -> Hsla {
    let mut fill = color;
    fill.a *= 0.12;
    fill
}

fn service_chip(service: &str, p: &Palette) -> Div {
    let label = match service {
        "mail" => "Mail",
        "contacts" => "Contacts",
        "chat" => "Chat",
        other => other,
    };
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(radius::SMALL))
        .bg(p.chip)
        .text_color(p.muted)
        .text_size(px(text::MICRO))
        .font_weight(FontWeight::MEDIUM)
        .child(label.to_string())
}

/// A Material switch: a track with a thumb at its end.
fn switch(on: bool, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .w(px(34.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { p.accent } else { p.line })
        .flex()
        .when(on, |this| this.justify_end())
        .child(div().size(px(16.)).rounded_full().bg(p.on_accent))
}

/// A text button (as in the main window).
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .text_size(px(text::FOOTNOTE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .child(label.into())
}
