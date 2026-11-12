//! The window as islands, after Flutter's `MailShell` (MS-200): the sidebar (a rail while it is
//! hidden) sits on the window, and the list and the conversation are rounded islands floating on
//! it. Wide windows show the conversation's island beside the list's, the list narrowing as it
//! slides in; narrow ones slide it over the list's. A Smart Inbox bundle slides over the rows
//! within the list's island. The list stays mounted under its layers, so a back swipe uncovers
//! the live list.

mod list;
mod menu;
mod reader;
mod sidebar;
mod toolbar;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, DispatchPhase, ElementId, Focusable, FontWeight,
    InteractiveElement, IntoElement,
    MouseButton, NavigationDirection, ParentElement, Render, ScrollWheelEvent, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, px, relative,
};

use crate::avatars;
use crate::controls::{
    ButtonKind, ControlSize, Tone, capsule, centered_note, field_frame, hairline, icon_avatar, icon_button,
    icon_label_button, island, list_row, mark_dot, notice, pill, popover, raised_capsule, row_layout, section_header,
    tag, title_bar, toast,
};
use crate::design::{self, Palette, icon as icon_size, layout, list as metrics, radius, space, stroke, text};
use crate::format;
use crate::icons::{Icon, icon};
use crate::model::{Category, Conversation, Message, Role};
use crate::state::{
    Archive, Back, CONTEXT, Forward, Item, Layer, List, MailApp, NewMessage, PagedView, Refresh, Removal, Reply,
    ReplyAll, Restore, Source, Star, ThreadState, ToggleRead, ToggleSidebar, ToggleWaiting, Trash,
};
use crate::webview;

impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let bench_end = self.bench.as_mut().and_then(|bench| bench.frame(&self.list_scroll, window, cx));
        let navigation = if self.sidebar_open { layout::SIDEBAR_WIDTH } else { layout::RAIL_WIDTH };
        // The room beside the navigation, inside the gaps around the islands.
        let room = (f32::from(window.viewport_size().width) - navigation - layout::GAP).max(layout::MIN_LIST);
        let content = if self.chat_view.is_some() {
            // Chat draws its own header; it takes one island.
            island(p.background, p).occlude().size_full().child(self.chat.clone()).into_any_element()
        } else {
            self.render_content(room, p, window, cx)
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
            .relative()
            .size_full()
            .flex()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.window)
            // Painted first: native web views placed last frame are hidden until painted again
            // (the Mac); pages no longer painted are closed (Blitz, elsewhere).
            .child({
                #[cfg(target_os = "macos")]
                let begin = {
                    let covered = self.context_menu.is_some();
                    canvas(|_, _, _| {}, move |_, _, _, _| webview::begin_frame(covered))
                };
                #[cfg(not(target_os = "macos"))]
                let begin = canvas(|_, _, _| {}, |_, _, window, _| webview::begin_frame(window));
                begin.absolute().size_0()
            })
            // The window between the islands moves it, as a title bar does.
            .child(title_bar("window", window, cx).absolute().size_full())
            .child(if self.sidebar_open {
                self.render_sidebar(p, window, cx).into_any_element()
            } else {
                self.render_rail(p, window, cx)
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pt(px(layout::GAP))
                    .pr(px(layout::GAP))
                    .pb(px(layout::GAP))
                    .child(content),
            )
            .when_some(self.toast.as_ref(), |d, shown| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(space::S24))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(toast(shown.text.clone(), false, p).text_size(px(text::BODY))),
                )
            })
            .children(self.render_context_menu(p, cx))
            // Painted last: ends the frame a measurement times.
            .children(bench_end)
    }
}

impl MailApp {
    // MARK: - Content

    /// The list's island and the conversation's, `room` wide together. The top layer sits where
    /// the slide puts it (opening, under the fingers, settling); the ones beneath rest.
    fn render_content(&mut self, room: f32, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let two_pane = f32::from(window.viewport_size().width) >= layout::TWO_PANE_WIDTH;
        // The list beside a conversation: its share, but never narrower than `MIN_LIST` nor so wide
        // that the conversation gets less than `MIN_DETAIL`.
        let split_list = (layout::LIST_SHARE * room)
            .clamp(layout::MIN_LIST, layout::MIN_LIST.max(room - layout::GAP - layout::MIN_DETAIL));
        let reader_on_top = matches!(self.layers.last(), Some(Layer::Reader(_)));
        let travel = if two_pane && reader_on_top { room - split_list } else { room };
        let offset = self.top_offset(travel, window);
        let top = self.layers.len().saturating_sub(1);

        let mut bundles: Vec<AnyElement> = Vec::new();
        let mut reader: Option<(AnyElement, f32)> = None;
        for (index, layer) in self.layers.iter().enumerate() {
            let shift = if index == top { offset } else { 0. };
            match layer {
                Layer::Bundle(category) => bundles.push(
                    div()
                        .id(ElementId::Name(layer.id()))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w_full()
                        .left(px(shift))
                        .bg(p.tray)
                        .rounded_b(px(radius::PANE))
                        .shadow(p.page_shadow.clone())
                        // The page keeps the keyboard; clicks never reach the list beneath.
                        .occlude()
                        .child(self.render_bundle(*category, p, cx))
                        .into_any_element(),
                ),
                Layer::Reader(conversation) => {
                    reader = Some((self.render_detail(conversation, two_pane, p, window, cx), shift));
                }
            }
        }

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

        let list = self.render_list_island(bundles, p, window, cx);
        let content = div().relative().size_full().child(swipe);
        match reader {
            // Beside the list, which narrows as the conversation slides in from the window's edge;
            // the conversation keeps its width, so its text does not reflow as it moves.
            Some((detail, shift)) if two_pane => {
                let split = (1. - shift / travel.max(1.)).clamp(0., 1.);
                let list_width = room + (split_list - room) * split;
                content
                    .child(list.absolute().left_0().top_0().bottom_0().w(px(list_width)))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(list_width + layout::GAP))
                            .w(px(room - split_list - layout::GAP))
                            .child(detail),
                    )
                    .into_any_element()
            }
            // Over the list's island.
            Some((detail, shift)) => content
                .child(list.size_full())
                .child(div().absolute().top_0().bottom_0().w_full().left(px(shift)).child(detail))
                .into_any_element(),
            None => content.child(list.size_full()).into_any_element(),
        }
    }

    /// The list's island: its title row and search field over the rows, an open bundle sliding
    /// over the rows on a page of its own.
    fn render_list_island(
        &self,
        bundles: Vec<AnyElement>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        island(p.tray, p)
            .occlude()
            .flex()
            .flex_col()
            .child(self.render_list_header(p, window, cx))
            .child(
                div().flex_shrink_0().px(px(space::S12)).pb(px(space::S10)).child(
                    field_frame(search_focused, true, p)
                        .id("search")
                        .h(px(28.))
                        .pl(px(space::S10))
                        .pr(px(space::S8))
                        .gap(px(space::S6))
                        .line_height(px(18.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(icon(Icon::Search, 14., p.muted))
                        .child(div().flex_1().min_w_0().child(self.search.clone())),
                ),
            )
            .child(hairline(p))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(self.render_list(p, cx))
                    .children(bundles),
            )
    }
}
