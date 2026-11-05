//! The window: sidebar, toolbar, the conversation list, and the layers that slide over it (a
//! Smart Inbox bundle, the reader). The list stays mounted under its layers, so a back swipe
//! uncovers the live list.

mod list;
mod menu;
mod reader;
mod sidebar;
mod toolbar;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, BoxShadow, ClickEvent, Context, DispatchPhase, Div, ElementId, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, NavigationDirection, ParentElement, Render, ScrollWheelEvent,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, point, px, relative, uniform_list,
};

use crate::avatars;
use crate::design::{self, Palette, UI_FONT, icon as icon_size, layout, list as metrics, radius, text};
use crate::format;
use crate::icons::{Icon, icon};
use crate::model::{Address, Category, Conversation, Message, Role};
use crate::state::{
    Archive, Back, CONTEXT, Forward, Item, Layer, List, MailApp, NewMessage, PagedView, Refresh, Removal, Reply,
    ReplyAll, Restore, Source, Star, ThreadState, ToggleRead, ToggleSidebar, ToggleWaiting, Trash,
};

impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        // Chat replaces the list; the pane draws its own header under the title bar.
        let content = if self.chat_view.is_some() {
            div().flex_1().min_h_0().child(self.chat.clone()).into_any_element()
        } else {
            self.render_content(p, window, cx)
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::trash))
            .on_action(cx.listener(Self::restore))
            .on_action(cx.listener(Self::star))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::toggle_waiting))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::retag))
            .on_action(cx.listener(Self::reply))
            .on_action(cx.listener(Self::reply_all))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::new_message))
            .on_action(cx.listener(Self::new_chat))
            // The mouse's back button, as in the native window.
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| {
                    if this.chat_view.is_none() {
                        this.back(&Back, window, cx);
                    }
                }),
            )
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.page)
            .when(self.sidebar_open, |d| d.child(self.render_sidebar(p, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_toolbar(p, cx))
                    .child(content),
            )
            .children(self.render_context_menu(p, cx))
    }
}

/// Mouse-down in the title bar region moves the window; a double click zooms it.
fn title_bar(id: &'static str) -> Stateful<Div> {
    div().id(id).on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

/// A round icon button with a tooltip; a disabled one is faint and takes no clicks.
fn icon_button(
    id: impl Into<ElementId>,
    glyph: Icon,
    tip: &'static str,
    enabled: bool,
    color: Hsla,
    p: &'static Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .size(px(icon_size::BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)).active(|s| s.bg(p.selection)))
        .tooltip(move |_, cx| cx.new(|_| Tooltip { text: tip.into(), p }).into())
        .child(icon(glyph, icon_size::TOOLBAR, if enabled { color } else { p.faint }))
}

struct Tooltip {
    text: SharedString,
    p: &'static Palette,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(UI_FONT)
            .px(px(8.))
            .py(px(4.))
            .rounded(px(radius::SMALL))
            .bg(self.p.popover)
            .border_1()
            .border_color(self.p.line)
            .shadow(popover_shadow(self.p))
            .text_size(px(text::CAPTION))
            .text_color(self.p.text)
            .child(self.text.clone())
    }
}

fn popover_shadow(p: &Palette) -> Vec<BoxShadow> {
    vec![BoxShadow { color: p.shadow, offset: point(px(0.), px(4.)), blur_radius: px(14.), spread_radius: px(0.) }]
}

fn chip(label: SharedString, color: Hsla) -> Div {
    let mut fill = color;
    fill.a *= 0.14;
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(radius::SMALL))
        .bg(fill)
        .text_color(color)
        .text_size(px(text::MICRO))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn centered_note(note: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .items_center()
        .justify_center()
        .px(px(32.))
        .text_color(p.muted)
        .child(div().text_center().child(note.into()))
}

impl MailApp {
    // MARK: - Content

    /// The list with its layers stacked over it. The top layer sits where the slide puts it
    /// (opening, under the fingers, settling); the ones beneath rest.
    fn render_content(&mut self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sidebar = if self.sidebar_open { layout::SIDEBAR_WIDTH } else { 0.0 };
        let width = f32::from(window.viewport_size().width) - sidebar;
        let offset = self.top_offset(width, window);
        let top = self.layers.len().saturating_sub(1);
        let layers: Vec<AnyElement> = self
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                let body = match layer {
                    Layer::Bundle(category) => self.render_bundle(*category, p, cx),
                    Layer::Reader(conversation) => self.render_reader(conversation, p, cx),
                };
                div()
                    .id(ElementId::Name(layer.id()))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w_full()
                    .left(px(if index == top { offset } else { 0. }))
                    .bg(p.page)
                    .shadow(vec![BoxShadow {
                        color: p.shadow,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(14.),
                        spread_radius: px(0.),
                    }])
                    // The layer keeps the keyboard; clicks never reach the list beneath.
                    .occlude()
                    .child(body)
                    .into_any_element()
            })
            .collect();

        // Two-finger back swipe: scroll events are seen in the capture phase, before the lists
        // and the reader scroll with them, so a horizontal swipe is never eaten by a scroll view.
        let entity = cx.entity();
        let swipe = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx: &mut App| {
                    if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                        return;
                    }
                    if entity.update(cx, |this, cx| this.feed_swipe(event, cx)) {
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .size_full();

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(swipe)
            .child(self.render_list(p, cx))
            .children(layers)
            .when_some(self.toast.as_ref(), |d, toast| {
                d.child(
                    div().absolute().bottom(px(20.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .max_w(px(560.))
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(radius::CARD))
                            .bg(p.text)
                            .text_color(p.page)
                            .text_size(px(text::FOOTNOTE))
                            .child(toast.text.clone()),
                    ),
                )
            })
            .into_any_element()
    }
}
