//! Full-list viewer/manager for unbounded lists in Settings (MS-422):
//! - "Recently marked read" (`AutoReadLog` entries)
//! - Address tag/pill settings (`SettingValue::Addresses`, such as `remoteContent.trustedSenders`)
//!
//! Settings itself previews at most [`PREVIEW_LIMIT`] (5) items inline. Opening the full list
//! spawns a dedicated secondary window on desktop (reusing an existing one for the same list), or
//! an in-window sub-page with a back button in compact / Android single-window mode.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, BorrowAppContext, Bounds, ClickEvent, Context, Div, Entity,
    FocusHandle, Focusable, FontWeight, Global, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use mailstream::{AutoReadEntry, AutoReadLog, AutoReadReason, Category, Event, SettingEntry, SettingValue};

use crate::controls::{ButtonKind, ControlSize, button, capsule, field_frame, hairline, icon_button};
use crate::design::{self, Palette, layout, settings as metrics, space, text};
use crate::engine::{MailError, engine};
use crate::icons::{Icon, icon};
use crate::input;
use crate::settings::{self, CONTEXT, CloseSettings};

/// Maximum number of rows/tags previewed inline in Settings before showing "View All".
pub(crate) const PREVIEW_LIMIT: usize = 5;

/// Which unbounded Settings list a full-list window or compact sub-page displays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SettingsListKind {
    AutoRead,
    Addresses { key: String },
}

impl SettingsListKind {
    pub(crate) fn window_title(&self) -> SharedString {
        match self {
            Self::AutoRead => "Recently marked read".into(),
            Self::Addresses { key } => settings::title(key).to_owned().into(),
        }
    }
}

/// Returns the inline preview slice (at most [`PREVIEW_LIMIT`] items) and the count of hidden items.
pub(crate) fn preview_slice<T>(items: &[T]) -> (&[T], usize) {
    let shown = items.len().min(PREVIEW_LIMIT);
    (&items[..shown], items.len() - shown)
}

/// Formats the metadata line of an auto-read entry (shared between Settings preview and full list).
pub(crate) fn format_auto_read_detail(entry: &AutoReadEntry) -> String {
    let marked = Some(mailstream::list_date(Some(entry.at.clone())))
        .filter(|date| !date.is_empty())
        .map(|date| format!("marked {date}"));
    [
        Some(if entry.category == Category::Newsletter { "Newsletter" } else { "Notification" }.to_owned()),
        Some(
            if entry.reason == AutoReadReason::Superseded {
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
    .join("  ·  ")
}

/// Filters auto-read entries case-insensitively across sender, subject, category, and reason.
pub(crate) fn filter_auto_read<'a>(entries: &'a [AutoReadEntry], query: &str) -> Vec<&'a AutoReadEntry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|entry| {
            let category = if entry.category == Category::Newsletter { "newsletter" } else { "notification" };
            let reason = if entry.reason == AutoReadReason::Superseded {
                "superseded a newer one from the same sender arrived"
            } else {
                "unread too long"
            };
            entry.sender.to_lowercase().contains(&needle)
                || entry.subject.to_lowercase().contains(&needle)
                || category.contains(&needle)
                || reason.contains(&needle)
        })
        .collect()
}

/// Filters addresses case-insensitively by substring.
pub(crate) fn filter_addresses<'a>(addresses: &'a [String], query: &str) -> Vec<&'a String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return addresses.iter().collect();
    }
    addresses.iter().filter(|address| address.to_lowercase().contains(&needle)).collect()
}

/// Splits comma- or whitespace-separated address input into trimmed tokens.
pub(crate) fn parse_added_addresses(typed: &str) -> Vec<String> {
    typed
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Tracks open desktop secondary list windows by [`SettingsListKind`] so clicking "View All"
/// activates an existing window instead of opening duplicates, and engine events keep them live.
#[derive(Default)]
struct OpenSettingsLists {
    windows: HashMap<SettingsListKind, (AnyWindowHandle, WeakEntity<SettingsListView>)>,
}

impl Global for OpenSettingsLists {}

/// Opens (or activates) a dedicated desktop window for `kind`.
pub(crate) fn open_list_window(kind: SettingsListKind, on_changed: Rc<dyn Fn(&mut App)>, cx: &mut App) {
    if let Some((handle, _)) = cx.try_global::<OpenSettingsLists>().and_then(|open| open.windows.get(&kind)) {
        let handle = *handle;
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(metrics::LIST_WIDTH), px(metrics::LIST_HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some(kind.window_title()), ..Default::default() }),
        window_min_size: Some(size(px(420.), px(360.))),
        ..Default::default()
    };
    let kind_for_view = kind.clone();
    let Ok((window, view)) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsListView::new_window(kind_for_view, on_changed, window, cx))
    }) else {
        return;
    };
    cx.update_default_global::<OpenSettingsLists, _>(|open, _| {
        open.windows.retain(|_, (_, weak)| weak.upgrade().is_some());
        open.windows.insert(kind, (window, view.downgrade()));
    });
    let focus = view.read(cx).focus.clone();
    window
        .update(cx, |_, window, cx| {
            window.activate_window();
            window.focus(&focus, cx);
        })
        .ok();
}

/// Delivers batched engine events to every open desktop list window.
pub(crate) fn notify_open_lists(batch: &[Event], cx: &mut gpui::AsyncApp) {
    let views: Vec<WeakEntity<SettingsListView>> = cx
        .update(|cx| {
            cx.try_global::<OpenSettingsLists>()
                .map(|open| open.windows.values().map(|(_, weak)| weak.clone()).collect())
                .unwrap_or_default()
        });
    for view in views {
        view.update(cx, |view, cx| view.follow_engine(batch, cx)).ok();
    }
}

struct InlineNav {
    on_back: Rc<dyn Fn(&mut Window, &mut App)>,
    on_close: Rc<dyn Fn(&mut Window, &mut App)>,
}

pub(crate) struct SettingsListView {
    pub(crate) focus: FocusHandle,
    kind: SettingsListKind,
    on_changed: Rc<dyn Fn(&mut App)>,
    inline: Option<InlineNav>,
    auto_read: Option<AutoReadLog>,
    settings: Option<Result<mailstream::Settings, MailError>>,
    filter: Entity<InputState>,
    _filter_sub: Subscription,
    add_input: Option<Entity<InputState>>,
    _add_sub: Option<Subscription>,
    error: Option<SharedString>,
    _appearance: Subscription,
}

impl SettingsListView {
    fn new_window(
        kind: SettingsListKind,
        on_changed: Rc<dyn Fn(&mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(kind, on_changed, None, window, cx)
    }

    pub(crate) fn new_inline(
        kind: SettingsListKind,
        on_changed: Rc<dyn Fn(&mut App)>,
        on_back: Rc<dyn Fn(&mut Window, &mut App)>,
        on_close: Rc<dyn Fn(&mut Window, &mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(kind, on_changed, Some(InlineNav { on_back, on_close }), window, cx)
    }

    fn new(
        kind: SettingsListKind,
        on_changed: Rc<dyn Fn(&mut App)>,
        inline: Option<InlineNav>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter_placeholder = match &kind {
            SettingsListKind::AutoRead => "Filter by sender, subject, or reason…",
            SettingsListKind::Addresses { .. } => "Filter addresses…",
        };
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(filter_placeholder));
        let filter_sub = cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });

        let (add_input, add_sub) = match &kind {
            SettingsListKind::Addresses { .. } => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Add address (comma-separated)"));
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { secondary: false, .. } = event {
                        this.commit_add_address(window, cx);
                    }
                });
                (Some(input), Some(sub))
            }
            SettingsListKind::AutoRead => (None, None),
        };

        let this = Self {
            focus: cx.focus_handle(),
            kind,
            on_changed,
            inline,
            auto_read: None,
            settings: None,
            filter,
            _filter_sub: filter_sub,
            add_input,
            _add_sub: add_sub,
            error: None,
            _appearance: cx.observe_window_appearance(window, |_, window, cx| {
                design::sync_kit_theme(window.appearance(), cx);
                cx.notify();
            }),
        };
        this.reload(cx);
        this
    }

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
        match &self.kind {
            SettingsListKind::AutoRead => self.load_auto_read(cx),
            SettingsListKind::Addresses { .. } => self.load_settings(cx),
        }
    }

    pub(crate) fn follow_engine(&mut self, batch: &[Event], cx: &mut Context<Self>) {
        match &self.kind {
            SettingsListKind::AutoRead => {
                if batch.contains(&Event::AutoRead) || batch.contains(&Event::Settings) {
                    self.load_auto_read(cx);
                }
            }
            SettingsListKind::Addresses { .. } => {
                if batch.contains(&Event::Settings) {
                    self.load_settings(cx);
                }
            }
        }
    }

    fn load_auto_read(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().auto_read_log().await.ok() }, |this, log, cx| {
            this.auto_read = log;
            cx.notify();
        });
    }

    fn load_settings(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().settings().await }, |this, settings, cx| {
            this.settings = Some(settings);
            cx.notify();
        });
    }

    fn changed(&self, cx: &mut Context<Self>) {
        let on_changed = self.on_changed.clone();
        cx.defer(move |cx| on_changed(cx));
    }

    fn close_or_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(inline) = &self.inline {
            let on_back = inline.on_back.clone();
            on_back(window, cx);
        } else {
            let kind = self.kind.clone();
            if cx.try_global::<OpenSettingsLists>().is_some() {
                cx.update_default_global::<OpenSettingsLists, _>(|open, _| {
                    open.windows.remove(&kind);
                });
            }
            window.remove_window();
        }
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(inline) = &self.inline {
            let on_close = inline.on_close.clone();
            on_close(window, cx);
        } else {
            self.close_or_back(window, cx);
        }
    }

    fn address_entry(&self) -> Option<&SettingEntry> {
        let SettingsListKind::Addresses { key } = &self.kind else { return None };
        match &self.settings {
            Some(Ok(settings)) => settings.settings.iter().find(|entry| entry.key == *key),
            _ => None,
        }
    }

    fn write_addresses(&mut self, key: String, value: Option<SettingValue>, cx: &mut Context<Self>) {
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
                        this.error = None;
                        if let Some(Ok(settings)) = &mut this.settings
                            && let Some(entry) = settings.settings.iter_mut().find(|entry| entry.key == key)
                        {
                            *entry = answer;
                        }
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        this.load_settings(cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn commit_add_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let SettingsListKind::Addresses { key } = self.kind.clone() else { return };
        let Some(input) = self.add_input.clone() else { return };
        let typed = input.read(cx).value().trim().to_string();
        let added = parse_added_addresses(&typed);
        if added.is_empty() {
            return;
        }
        let Some(SettingValue::Addresses { mut addresses }) = self.address_entry().map(|e| e.value.clone()) else {
            return;
        };
        input.update(cx, |input, cx| input.clean(window, cx));
        addresses.extend(added);
        self.write_addresses(key, Some(SettingValue::Addresses { addresses }), cx);
    }

    fn undo_auto_read(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.background(
            cx,
            async move { engine().undo_auto_read(Some(ids)).await },
            |this, result, cx| {
                match result {
                    Ok(result) => {
                        this.error = result.failed.first().map(|failure| failure.message.clone().into());
                        this.load_auto_read(cx);
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            },
        );
    }

    fn render_filter_bar(&self, p: &'static Palette, window: &Window, cx: &mut Context<Self>) -> Div {
        let focused = self.filter.focus_handle(cx).is_focused(window);
        let has_query = !self.filter.read(cx).value().is_empty();
        field_frame(focused, false, p)
            .overflow_hidden()
            .w_full()
            .h(px(28.))
            .px(px(8.))
            .gap(px(6.))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if !this.filter.read(cx).value().is_empty() {
                    this.filter.update(cx, |input, cx| input.clean(window, cx));
                    cx.notify();
                } else {
                    window.focus(&this.focus, cx);
                }
            }))
            .child(icon(Icon::Search, 14., p.muted))
            .child(div().flex_1().min_w_0().child(input::field(&self.filter)))
            .when(has_query, |d| {
                d.child(
                    div()
                        .id("settings-list-filter-clear")
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .size(px(18.))
                        .hover(|s| s.bg(p.hover))
                        .child(icon(Icon::Close, 12., p.muted))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.filter.update(cx, |input, cx| input.clean(window, cx));
                            cx.notify();
                        })),
                )
            })
    }

    fn render_auto_read_content(&self, p: &'static Palette, compact: bool, window: &Window, cx: &mut Context<Self>) -> (Div, Stateful<Div>) {
        let query = self.filter.read(cx).value().to_string();
        let filtering = !query.trim().is_empty();
        let (summary, action, body): (SharedString, Option<AnyElement>, AnyElement) = match &self.auto_read {
            None => ("Loading auto-read log…".into(), None, settings::note("Loading auto-read log…", p).into_any_element()),
            Some(log) => {
                let total = log.entries.len();
                let filtered = filter_auto_read(&log.entries, &query);
                let shown = filtered.len();
                let summary: SharedString = if total == 0 {
                    format!(
                        "Nothing yet. Unread notifications and newsletters older than {} days show here once marked read.",
                        log.days
                    )
                    .into()
                } else if filtering {
                    format!(
                        "Showing {shown} of {total} conversation{}. Marking one unread keeps auto-read away from it until new mail arrives.",
                        if total == 1 { "" } else { "s" }
                    )
                    .into()
                } else {
                    format!(
                        "{total} conversation{}. Marking one unread keeps auto-read away from it until new mail arrives.",
                        if total == 1 { "" } else { "s" }
                    )
                    .into()
                };
                let ids: Vec<String> = filtered.iter().map(|entry| entry.id.clone()).collect();
                let action = (!ids.is_empty()).then(|| {
                    let label = if filtering {
                        format!("Mark {shown} Shown Unread")
                    } else {
                        "Mark All Unread".to_owned()
                    };
                    button("list-auto-read-all", label, ButtonKind::Secondary, ControlSize::Regular, true, p)
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(ids.clone(), cx)))
                        .into_any_element()
                });
                let body = if total == 0 {
                    settings::card(p)
                        .px(px(14.))
                        .py(px(12.))
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::MEDIUM).child("Nothing marked read yet"))
                        .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(format!(
                            "Unread notifications and newsletters older than {} days show here once auto-read marks them read.",
                            log.days
                        )))
                        .into_any_element()
                } else if filtered.is_empty() {
                    settings::note(format!("No conversations matching \"{}\".", query.trim()), p).into_any_element()
                } else {
                    let rows: Vec<AnyElement> = filtered
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let detail = format_auto_read_detail(entry);
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
                                                .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(
                                                    if entry.sender.is_empty() {
                                                        "(Unknown sender)".to_owned()
                                                    } else {
                                                        entry.sender.clone()
                                                    },
                                                ))
                                                .child(div().flex_1().min_w_0().truncate().child(entry.subject.clone())),
                                        )
                                        .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).truncate().child(detail)),
                                )
                                .child(
                                    icon_button(("list-auto-read", index), Icon::MarkUnread, "Mark Unread", true, p.glyph)
                                        .on_click(cx.listener(move |this, _, _, cx| this.undo_auto_read(vec![id.clone()], cx))),
                                )
                                .into_any_element()
                        })
                        .collect();
                    settings::group(rows, p).expect("filtered is non-empty").into_any_element()
                };
                (summary, action, body)
            }
        };

        let controls = self.render_controls_header(summary, action, None, p, compact, window, cx);
        let list = div()
            .id("settings-list-body")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .p(px(if compact { space::S12 } else { space::S16 }))
            .child(body);
        (controls, list)
    }

    fn render_addresses_content(&self, key: &str, p: &'static Palette, compact: bool, window: &Window, cx: &mut Context<Self>) -> (Div, Stateful<Div>) {
        let query = self.filter.read(cx).value().to_string();
        let filtering = !query.trim().is_empty();
        let add_bar = self.add_input.as_ref().map(|input| {
            let focused = input.focus_handle(cx).is_focused(window);
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    field_frame(focused, false, p)
                        .overflow_hidden()
                        .flex_1()
                        .min_w_0()
                        .h(px(28.))
                        .px(px(8.))
                        .on_action(cx.listener(|this, _: &Escape, window, cx| {
                            if let Some(input) = &this.add_input {
                                input.update(cx, |input, cx| input.clean(window, cx));
                            }
                            window.focus(&this.focus, cx);
                        }))
                        .child(div().flex_1().min_w_0().child(input::field(input))),
                )
                .child(
                    button("list-add-address", "Add", ButtonKind::Secondary, ControlSize::Regular, true, p)
                        .on_click(cx.listener(|this, _, window, cx| this.commit_add_address(window, cx))),
                )
                .into_any_element()
        });

        let (summary, action, body): (SharedString, Option<AnyElement>, AnyElement) = match &self.settings {
            None => ("Loading settings…".into(), None, settings::note("Loading settings…", p).into_any_element()),
            Some(Err(error)) => (error.to_string().into(), None, settings::note(error.to_string(), p).into_any_element()),
            Some(Ok(_)) => match self.address_entry() {
                None => ("Setting not found.".into(), None, settings::note("Setting not found.", p).into_any_element()),
                Some(entry) => {
                    let addresses: &[String] = match &entry.value {
                        SettingValue::Addresses { addresses } => addresses,
                        _ => &[],
                    };
                    let total = addresses.len();
                    let filtered = filter_addresses(addresses, &query);
                    let shown = filtered.len();
                    let count_text = if filtering {
                        format!("Showing {shown} of {total} address{}.", if total == 1 { "" } else { "es" })
                    } else {
                        format!("{total} address{}.", if total == 1 { "" } else { "es" })
                    };
                    let summary: SharedString = format!("{count_text} {}", entry.summary).into();
                    let reset_key = key.to_owned();
                    let action = entry.changed.then(|| {
                        button("list-reset-addresses", "Reset", ButtonKind::Plain(p.accent), ControlSize::Small, true, p)
                            .on_click(cx.listener(move |this, _, _, cx| this.write_addresses(reset_key.clone(), None, cx)))
                            .into_any_element()
                    });
                    let body = if addresses.is_empty() {
                        settings::card(p)
                            .px(px(14.))
                            .py(px(12.))
                            .gap(px(2.))
                            .child(div().font_weight(FontWeight::MEDIUM).child("No addresses yet"))
                            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(entry.summary.clone()))
                            .into_any_element()
                    } else if filtered.is_empty() {
                        settings::note(format!("No addresses matching \"{}\".", query.trim()), p).into_any_element()
                    } else {
                        let rows: Vec<AnyElement> = filtered
                            .into_iter()
                            .enumerate()
                            .map(|(index, address)| {
                                let next: Vec<String> = addresses.iter().filter(|a| *a != address).cloned().collect();
                                let remove_key = key.to_owned();
                                div()
                                    .px(px(14.))
                                    .py(px(8.))
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(div().flex_1().min_w_0().truncate().child(address.clone()))
                                    .child(
                                        button(
                                            ("list-remove-address", index),
                                            "Remove",
                                            ButtonKind::Plain(p.danger),
                                            ControlSize::Small,
                                            true,
                                            p,
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.write_addresses(
                                                remove_key.clone(),
                                                Some(SettingValue::Addresses { addresses: next.clone() }),
                                                cx,
                                            )
                                        })),
                                    )
                                    .into_any_element()
                            })
                            .collect();
                        settings::group(rows, p).expect("filtered is non-empty").into_any_element()
                    };
                    (summary, action, body)
                }
            },
        };

        let controls = self.render_controls_header(summary, action, add_bar, p, compact, window, cx);
        let list = div()
            .id("settings-list-body")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .p(px(if compact { space::S12 } else { space::S16 }))
            .child(body);
        (controls, list)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_controls_header(
        &self,
        summary: SharedString,
        action: Option<AnyElement>,
        extra_row: Option<AnyElement>,
        p: &'static Palette,
        compact: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let inline = self.inline.is_some();
        let narrow = inline && compact;
        let title_and_summary = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .when(!inline, |d| {
                d.child(
                    div()
                        .text_size(px(text::PANE_TITLE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(self.kind.window_title()),
                )
            })
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(summary));

        let top_row = if narrow {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(title_and_summary)
                .when_some(action, |d, action| d.child(div().flex().justify_end().child(action)))
        } else {
            div().flex().items_center().gap(px(12.)).child(title_and_summary).children(action)
        };

        div()
            .flex_shrink_0()
            .px(px(if narrow { space::S12 } else { space::S16 }))
            .py(px(if narrow { space::S10 } else { space::S12 }))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(top_row)
            .child(self.render_filter_bar(p, window, cx))
            .children(extra_row)
            .when_some(self.error.clone(), |d, error| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.danger).child(error))
            })
    }
}

impl Render for SettingsListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let inline = self.inline.is_some();
        let (controls, body) = match self.kind.clone() {
            SettingsListKind::AutoRead => self.render_auto_read_content(p, compact, window, cx),
            SettingsListKind::Addresses { key } => self.render_addresses_content(&key, p, compact, window, cx),
        };

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.close_or_back(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                window.focus(&this.focus, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.background)
            .when(inline, |d| {
                d.child(
                    div()
                        .id("settings-list-header")
                        .flex_shrink_0()
                        .h(px(design::compact::TOP_BAR_HEIGHT))
                        .px(px(space::S10))
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .child(
                            capsule(p).child(
                                icon_button("settings-list-back", Icon::Back, "Back to Settings", true, p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_or_back(window, cx))),
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
                                        .text_size(px(design::compact::HEADER_TITLE))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(p.text)
                                        .child(self.kind.window_title()),
                                ),
                        )
                        .child(
                            capsule(p).child(
                                icon_button("settings-list-close", Icon::Close, "Close Settings", true, p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx))),
                            ),
                        ),
                )
                .child(hairline(p))
            })
            .child(controls)
            .child(hairline(p))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, sender: &str, subject: &str, category: Category, reason: AutoReadReason) -> AutoReadEntry {
        AutoReadEntry {
            id: id.into(),
            account: "sam@example.com".into(),
            sender: sender.into(),
            subject: subject.into(),
            category,
            reason,
            at: "2026-11-18T12:00:00Z".into(),
            batch: 1,
        }
    }

    #[test]
    fn preview_slice_caps_at_five_and_reports_hidden_count() {
        let empty: Vec<i32> = vec![];
        assert_eq!(preview_slice(&empty), (&[][..], 0));

        let three = vec![1, 2, 3];
        assert_eq!(preview_slice(&three), (&[1, 2, 3][..], 0));

        let five = vec![1, 2, 3, 4, 5];
        assert_eq!(preview_slice(&five), (&[1, 2, 3, 4, 5][..], 0));

        let eight = vec![1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(preview_slice(&eight), (&[1, 2, 3, 4, 5][..], 3));
        assert_eq!(PREVIEW_LIMIT, 5);
    }

    #[test]
    fn filter_auto_read_matches_sender_subject_category_and_reason() {
        let entries = vec![
            entry("1", "GitHub", "CI passed on main", Category::Notification, AutoReadReason::Age),
            entry("2", "Design Digest", "Issue 42: Color systems", Category::Newsletter, AutoReadReason::Superseded),
            entry("3", "Billing", "Invoice #901", Category::Notification, AutoReadReason::Age),
        ];

        assert_eq!(filter_auto_read(&entries, "").len(), 3);
        assert_eq!(filter_auto_read(&entries, "   ").len(), 3);

        let by_sender = filter_auto_read(&entries, "github");
        assert_eq!(by_sender.len(), 1);
        assert_eq!(by_sender[0].id, "1");

        let by_subject = filter_auto_read(&entries, "COLOR");
        assert_eq!(by_subject.len(), 1);
        assert_eq!(by_subject[0].id, "2");

        let by_category = filter_auto_read(&entries, "newsletter");
        assert_eq!(by_category.len(), 1);
        assert_eq!(by_category[0].id, "2");

        let by_reason = filter_auto_read(&entries, "unread too long");
        assert_eq!(by_reason.len(), 2);

        assert!(filter_auto_read(&entries, "nonexistent").is_empty());
    }

    #[test]
    fn filter_addresses_matches_case_insensitively() {
        let addresses = vec![
            "alerts@example.com".to_owned(),
            "billing@cloudhost.example".to_owned(),
            "digest@engineering.example".to_owned(),
        ];
        assert_eq!(filter_addresses(&addresses, "").len(), 3);
        let hits = filter_addresses(&addresses, "CLOUD");
        assert_eq!(hits, vec![&"billing@cloudhost.example".to_owned()]);
        assert!(filter_addresses(&addresses, "missing").is_empty());
    }

    #[test]
    fn parse_added_addresses_splits_commas_and_whitespace() {
        assert_eq!(
            parse_added_addresses("  a@x.com, b@y.com\n c@z.com , "),
            vec!["a@x.com", "b@y.com", "c@z.com"]
        );
        assert!(parse_added_addresses(" ,  \t ").is_empty());
    }

    #[test]
    fn settings_list_kind_titles_match_settings_headings() {
        assert_eq!(SettingsListKind::AutoRead.window_title().as_ref(), "Recently marked read");
        assert_eq!(
            SettingsListKind::Addresses { key: "remoteContent.trustedSenders".into() }.window_title().as_ref(),
            "Trusted senders"
        );
    }

    #[test]
    fn format_auto_read_detail_describes_category_and_reason() {
        let newsletter = entry("1", "Digest", "Issue 1", Category::Newsletter, AutoReadReason::Superseded);
        let detail = format_auto_read_detail(&newsletter);
        assert!(detail.contains("Newsletter"));
        assert!(detail.contains("a newer one from the same sender arrived"));

        let notification = entry("2", "GitHub", "CI", Category::Notification, AutoReadReason::Age);
        let detail = format_auto_read_detail(&notification);
        assert!(detail.contains("Notification"));
        assert!(detail.contains("unread too long"));
    }

    #[test]
    fn settings_list_kind_keys_deduplicate_by_kind_and_setting_key() {
        let mut map = HashMap::new();
        map.insert(SettingsListKind::AutoRead, 1);
        map.insert(SettingsListKind::AutoRead, 2);
        map.insert(SettingsListKind::Addresses { key: "remoteContent.trustedSenders".into() }, 3);
        map.insert(SettingsListKind::Addresses { key: "remoteContent.trustedSenders".into() }, 4);
        map.insert(SettingsListKind::Addresses { key: "other.addresses".into() }, 5);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&SettingsListKind::AutoRead), Some(&2));
        assert_eq!(map.get(&SettingsListKind::Addresses { key: "remoteContent.trustedSenders".into() }), Some(&4));
    }
}
