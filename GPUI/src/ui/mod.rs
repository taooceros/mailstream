//! The window as islands, after Flutter's `MailShell` (MS-200): the sidebar (a rail while it is
//! hidden) sits on the window, and the list and the conversation are rounded islands floating on
//! it. Wide windows show the conversation's island beside the list's, the list narrowing as it
//! slides in; narrow ones slide it over the list's. A Smart Inbox bundle slides over the rows
//! within the list's island. The list stays mounted under its layers, so a back swipe uncovers
//! the live list. The compose column (`compose/column.rs`) docks beside them, at the trailing
//! edge, the leading one, or between the list and the conversation; floats over the window; or
//! waits minimized in its bottom-right corner.

mod invitation;
mod list;
mod menu;
mod reader;
mod sidebar;
mod toolbar;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, CursorStyle, Div, DispatchPhase, DragMoveEvent, ElementId, EmptyView,
    Focusable,
    FontWeight, InteractiveElement, IntoElement, MouseButton, NavigationDirection, ParentElement, Render,
    ScrollEvent, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, px, relative,
};

use crate::avatars;
use crate::compose::{ComposeColumn, ResizeColumn};
use crate::controls::{
    ButtonKind, ControlSize, Tone, capsule, centered_note, field_frame, hairline, icon_avatar, icon_button,
    icon_label_button, island, list_row, mark_dot, notice, pill, raised_capsule, row_layout, section_header, tag,
    title_bar,
};
#[cfg(target_os = "macos")]
use crate::controls::{segment, segmented};
use crate::design::{
    self, Palette, compose as compose_metrics, icon as icon_size, layout, list as metrics, radius, space, stroke, text,
};
use crate::format;
use crate::icons::{Icon, icon};
use crate::input;
use crate::split::{self, ResizeList};
use crate::model::{Category, Conversation, Message, Role};
use crate::state::{
    Archive, Back, CONTEXT, Forward, Item, Layer, List, MailApp, NewMessage, PagedView, Refresh, Removal, Reply,
    ReplyAll, Restore, Source, Star, ThreadState, ToggleRead, ToggleSidebar, ToggleWaiting, Trash,
};
#[cfg(target_os = "macos")]
use crate::state::Renderer;
use crate::blitz_body;
#[cfg(target_os = "macos")]
use crate::webview;

impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let bench_end = self.bench.as_mut().and_then(|bench| bench.frame(&self.list_scroll, window, cx));
        // Drafts asked for with no window at hand open now, before the column is laid out.
        if self.compose.read(cx).has_pending() {
            self.compose.update(cx, |column, cx| column.open_pending(window, cx));
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width);
        let (docked, swapped, floating, minimized) = {
            let column = self.compose.read(cx);
            (column.docked(), column.swapped, column.floats().then(|| column.float_bounds(viewport)), column.minimized())
        };
        self.sidebar_folded = docked && width < compose_metrics::SIDEBAR_WIDTH;
        let sidebar = self.sidebar_shown();
        let navigation = if sidebar { layout::SIDEBAR_WIDTH } else { layout::RAIL_WIDTH };
        // The room beside the navigation, inside the gaps around the islands.
        let room = (width - navigation - layout::GAP).max(layout::MIN_LIST);
        // The docked column's width, and the room it leaves the list and the conversation, which
        // stay side by side while they have `TWO_PANE_MAIL`.
        let column_width = docked.then(|| self.compose.read(cx).width(room));
        let mail = column_width.map_or(room, |column| (room - column - layout::GAP).max(0.));
        let two_pane = if docked { mail >= compose_metrics::TWO_PANE_MAIL } else { width >= layout::TWO_PANE_WIDTH };
        // Swapped beside an open conversation, the column sits between it and the list.
        let between = column_width.filter(|_| {
            swapped && two_pane && self.mail_in_view() && self.layers.iter().any(|layer| matches!(layer, Layer::Reader(_)))
        });
        // Where the list (or the calendar) starts and the room it shares, for a drag of its edge.
        self.split_left = match column_width {
            Some(column) if swapped && between.is_none() => navigation + column + layout::GAP,
            _ => navigation,
        };
        self.split_room = room;
        self.split_mail = match between {
            Some(_) => room - layout::GAP - compose_metrics::MIN_COLUMN,
            None => mail,
        };
        let content = if self.chat_view.is_some() {
            // Chat draws its own header; it takes one island.
            island(p.background, p).occlude().size_full().child(self.chat.clone()).into_any_element()
        } else if self.calendar_view {
            // The calendar lays out its own islands.
            div().size_full().child(self.calendar.clone()).into_any_element()
        } else {
            self.render_content(room, mail, two_pane, between.map(|column| (column, navigation)), p, window, cx)
        };
        let content = match column_width {
            Some(column) if between.is_none() => {
                let (anchor, leading) = if swapped { (navigation, true) } else { (width - layout::GAP, false) };
                let gap = resize_gap(ResizeColumn { anchor, leading, widest: ComposeColumn::widest(room) }, p);
                let content = div().flex_1().min_w_0().h_full().child(content);
                let column = self.render_docked_column(column);
                let row = div().size_full().flex();
                if swapped { row.child(column).child(gap).child(content) } else { row.child(content).child(gap).child(column) }
                    .into_any_element()
            }
            _ => content,
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::close))
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
            .on_action(cx.listener(Self::reset_list_width))
            .on_action(cx.listener(Self::toggle_frame_rate))
            // The gap beside the docked compose column moves its free edge.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ResizeColumn>, _, cx| {
                let width = event.drag(cx).width(f32::from(event.event.position.x));
                this.compose.update(cx, |column, cx| column.set_width(width, cx));
            }))
            // The gap after the list (or the calendar) moves the list's edge.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ResizeList>, _, cx| {
                this.drag_list_edge(f32::from(event.event.position.x), cx);
            }))
            // The mouse's back button, as in the native window.
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| {
                    if this.mail_in_view() {
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
            // Painted first: pages no longer painted are closed (Blitz); native web views placed
            // last frame are hidden until painted again (the Mac's WebView).
            .child({
                #[cfg(target_os = "macos")]
                let covered = self.context_menu.is_some();
                canvas(|_, _, _| {}, move |_, _, window, _| {
                    blitz_body::begin_frame(window);
                    #[cfg(target_os = "macos")]
                    webview::begin_frame(covered);
                })
                .absolute()
                .size_0()
            })
            // The window between the islands moves it, as a title bar does.
            .child(title_bar("window", window, cx).absolute().size_full())
            .child(if sidebar { self.render_sidebar(p, window, cx).into_any_element() } else { self.render_rail(p, window, cx) })
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
            .when_some(floating, |d, bounds| {
                d.child(
                    div()
                        .absolute()
                        .left(bounds.origin.x)
                        .top(bounds.origin.y)
                        .w(bounds.size.width)
                        .h(bounds.size.height)
                        .child(self.compose.clone()),
                )
            })
            .when(minimized, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(px(compose_metrics::FLOAT_MARGIN))
                        .bottom(px(compose_metrics::FLOAT_MARGIN))
                        .child(self.compose.clone()),
                )
            })
            .children(self.render_context_menu())
            // Over everything in the bottom leading corner, as in Flutter.
            .when_some(self.frame_meter.clone(), |d, meter| {
                d.child(div().absolute().left(px(space::S16)).bottom(px(space::S16)).child(meter))
            })
            // Painted last: ends the frame a measurement times.
            .children(bench_end)
    }
}

impl MailApp {
    // MARK: - Content

    /// The list's island and the conversation's, `mail` wide together: `room`, the width beside
    /// the navigation, less a docked compose column's. With `between` (the column's width, and the
    /// navigation's), the column sits between them. The top layer sits where the slide puts it
    /// (opening, under the fingers, settling); the ones beneath rest.
    #[allow(clippy::too_many_arguments)]
    fn render_content(
        &mut self,
        room: f32,
        mail: f32,
        two_pane: bool,
        between: Option<(f32, f32)>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The list beside a conversation: its share of `room` (so dragging a docked column's edge
        // moves that edge alone), but never narrower than `MIN_LIST` nor so wide that the
        // conversation gets less than `MIN_DETAIL`. With the column between them the list leaves
        // room for the narrowest column instead, and the column gives way to the conversation.
        let share = self.list_share * room;
        let (split_list, mail, between) = match between {
            Some((column, navigation)) => {
                let split_list = share.clamp(
                    layout::MIN_LIST,
                    layout::MIN_LIST
                        .max(room - 2. * layout::GAP - compose_metrics::MIN_COLUMN - layout::MIN_DETAIL),
                );
                let widest =
                    (room - split_list - 2. * layout::GAP - layout::MIN_DETAIL).max(compose_metrics::MIN_COLUMN);
                let column = column.min(widest);
                (split_list, room - column - layout::GAP, Some((column, navigation, widest)))
            }
            None => {
                (share.clamp(layout::MIN_LIST, layout::MIN_LIST.max(mail - layout::GAP - layout::MIN_DETAIL)), mail, None)
            }
        };
        let reader_on_top = matches!(self.layers.last(), Some(Layer::Reader(_)));
        let travel = if two_pane && reader_on_top { mail - split_list } else { mail };
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
                window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx: &mut App| {
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
                let list_width = mail + (split_list - mail) * split;
                let content = content
                    .child(list.absolute().left_0().top_0().bottom_0().w(px(list_width)))
                    // The gap after the list moves its edge, once the conversation has slid in.
                    .when(shift == 0., |content| {
                        content.child(split::list_handle("list-split").left(px(list_width + layout::GAP / 2.)))
                    });
                // The column follows the list's edge, its gap dragged against that edge; it never
                // widens so far that the conversation would get less than `MIN_DETAIL`.
                let (content, inset) = match between {
                    Some((column, navigation, widest)) => {
                        let widest = widest.min(ComposeColumn::widest(room));
                        let anchor = navigation + list_width + layout::GAP;
                        let gap = resize_gap(ResizeColumn { anchor, leading: true, widest }, p);
                        let placed = div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(list_width + layout::GAP))
                            .flex()
                            .child(self.render_docked_column(column))
                            .child(gap);
                        (content.child(placed), column + layout::GAP)
                    }
                    None => (content, 0.),
                };
                content
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(list_width + layout::GAP + inset))
                            .w(px(mail - split_list - layout::GAP))
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

    /// The docked compose column, `width` wide.
    fn render_docked_column(&self, width: f32) -> Div {
        div().flex_shrink_0().w(px(width)).h_full().child(self.compose.clone())
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
                        .on_action(cx.listener(Self::cancel_search))
                        .child(icon(Icon::Search, 14., p.muted))
                        .child(div().flex_1().min_w_0().child(input::field(&self.search))),
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

/// The gap beside the docked compose column (Flutter's `ResizeGap`): dragged, it moves the
/// column's free edge; under the pointer a grip shows in it.
fn resize_gap(drag: ResizeColumn, p: &'static Palette) -> Stateful<Div> {
    div()
        .id("compose-gap")
        .group("compose-gap")
        .flex_shrink_0()
        .w(px(layout::GAP))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        // The window beneath would take the press as a move.
        .occlude()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(drag, |_, _, _, cx| cx.new(|_| EmptyView))
        .child(
            div()
                .w(px(compose_metrics::GRIP_WIDTH))
                .h(px(compose_metrics::GRIP_HEIGHT))
                .rounded_full()
                .group_hover("compose-gap", |s| s.bg(p.line)),
        )
}
