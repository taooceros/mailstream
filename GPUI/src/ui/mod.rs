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
    AnyElement, App, AppContext, ClickEvent, Context, CursorStyle, Div, DragMoveEvent, ElementId, EmptyView,
    Focusable,
    FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, NavigationDirection, ParentElement, Render,
    ScrollEvent, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, px, relative,
};
use mailstream::{Category, Conversation, MailboxRole};

use crate::avatars;
use crate::bench::{Bench, Turn};
use crate::compose::{ComposeColumn, ResizeColumn};
use crate::controls::{
    Tone, capsule, centered_note, hairline, icon_avatar, icon_button, island, list_row, mark_dot, notice, notice_close,
    own_presses, pill, raised_capsule, row_layout, scrim, search_field, section_header, tag, title_bar,
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
use crate::swipe;
use crate::model::{AccountExt, CategoryExt, ConversationExt, MailboxExt, RoleExt, ShownMessage, UNIFIED};
use crate::state::{
    Archive, Back, Beside, BundleKind, CONTEXT, Forward, Item, Layer, List, MailApp, NewMessage, PagedView, Refresh, Removal, Reply,
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
        // `MAILSTREAM_BENCH=pages`: the inbox's first bundle slides in and back off (MS-289).
        match self.bench.as_mut().and_then(Bench::take_turn) {
            Some(Turn::Open) => {
                let bundle = self.items.iter().find_map(|item| match item {
                    Item::Bundle(bundle) => Some(format!("bundle:{}", bundle.category.as_str())),
                    _ => None,
                });
                if let Some(id) = bundle {
                    self.activate(&id, window, cx);
                }
            }
            Some(Turn::Back) => self.back(&Back, window, cx),
            None => {}
        }
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
            // Chat lays out its own islands, the list as wide as mail's (MS-328).
            let list = split::list_width(self.list_share * room, mail);
            self.chat.update(cx, |chat, cx| chat.set_split(list, two_pane, cx));
            div().size_full().child(self.chat.clone()).into_any_element()
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
        // The calendar's open event over the whole window, sidebar and all (MS-184).
        let page = if self.calendar_view { self.calendar.update(cx, |pane, cx| pane.render_page(window, cx)) } else { None };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::extend_next))
            .on_action(cx.listener(Self::extend_previous))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::trash))
            .on_action(cx.listener(Self::restore))
            .on_action(cx.listener(Self::star))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::toggle_waiting))
            .on_action(cx.listener(Self::undo))
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
            // The window behind the islands: over a blur (an older Mac, a Linux desktop that blurs)
            // a wash, nothing over the Mac's glass, elsewhere opaque.
            .bg(design::window_fill(p))
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
            .children(page)
            // The frosted window's edge (MS-249), over the islands and pages; takes no pointer.
            .when_some(design::window_rim(p), |d, (radius, rim)| {
                d.child(div().absolute().inset_0().rounded(px(radius)).shadow(rim))
            })
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
            None => (split::list_width(share, mail), mail, None),
        };
        // Where the layers rest beside each other, for the gestures and the slide (MS-299).
        let inset = between.map_or(0., |(column, _, _)| column + layout::GAP);
        self.content_width = mail;
        self.beside = two_pane.then(|| Beside {
            list: split_list,
            edge: split_list + layout::GAP + inset,
            travel: mail - split_list,
        });
        // The sliding layer: the top one, or one a gesture took beneath a conversation beside the
        // list (MS-299); the layers over it rest.
        let (sliding, offset) = self.slide_offset(window, cx).unwrap_or((0, 0.));
        let reader_beside = two_pane && matches!(self.layers.get(sliding), Some(Layer::Reader(_)));
        let travel = if self.layers.is_empty() { mail } else { self.travel(sliding) };
        // What the sliding layer covers sits back and darkens under it, as the pages' motion says,
        // and comes forward as it slides off (MS-289); a conversation beside the list covers nothing.
        let (beneath, dim) = if self.layers.is_empty() || reader_beside {
            (0., 0.)
        } else {
            design::motion::beneath(1. - offset.abs() / travel.max(1.), travel, p.dark)
        };

        let mut bundles: Vec<AnyElement> = Vec::new();
        let mut reader: Option<(AnyElement, f32)> = None;
        for (index, layer) in self.layers.iter().enumerate() {
            let shift = if index == sliding {
                offset
            } else if index > sliding || reader_beside {
                0.
            } else {
                beneath
            };
            match layer {
                Layer::Bundle(kind) => {
                    if index == sliding && dim > 0. {
                        bundles.push(scrim(dim, 0.).into_any_element());
                    }
                    bundles.push(
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
                            .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer.id(), cx))
                            .child(match *kind {
                                BundleKind::Mail(category) => self.render_bundle(category, p, cx),
                                BundleKind::Chat => self.render_chat_bundle(p, cx),
                            })
                            .into_any_element(),
                    )
                }
                Layer::Search => {
                    if index == sliding && dim > 0. {
                        bundles.push(scrim(dim, 0.).into_any_element());
                    }
                    bundles.push(
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
                            .occlude()
                            .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer.id(), cx))
                            .child(self.render_list(p, cx))
                            .into_any_element(),
                    )
                }
                Layer::Reader(conversation) => {
                    reader = Some((self.render_detail(conversation, layer.id(), two_pane, p, window, cx), shift));
                }
            }
        }
        let rows_shift = if reader_beside { 0. } else { beneath };

        // Two-finger back swipe: scroll events are seen in the capture phase, before the lists
        // and the reader scroll with them, so a horizontal swipe is never eaten by a scroll view.
        // A mouse drag from a press on the top layer moves it the same way (MS-266); its
        // listeners go under the layers and over them (`swipe::mouse_capture`).
        let swipe = swipe::scroll_capture(cx.entity(), Self::feed_swipe).absolute().size_full();
        let (drag_under, drag_over) = swipe::mouse_capture(cx.entity(), Self::feed_drag);

        // Beside a conversation, the list's content keeps the width the list rests at once the
        // slide ends (the list's share while the conversation slides in or is dragged, the whole
        // width while it slides off) and the island's edge moves over it, clipping it: its rows
        // lay out and shape their text once as the slide starts, not every frame (MS-292).
        let laid_out = (two_pane && reader.is_some()).then(|| if self.closing_reader() { mail } else { split_list });
        let list = self.render_list_island(bundles, rows_shift, laid_out, p, window, cx);
        let content = div().relative().size_full().child(swipe).child(drag_under.absolute().size_0());
        let content = match reader {
            // Beside the list, which narrows as the conversation slides in from the window's edge;
            // the conversation keeps its width, so its text does not reflow as it moves, and so
            // does the list's content (`laid_out`).
            Some((detail, shift)) if two_pane => {
                let split = (1. - shift / (mail - split_list).max(1.)).clamp(0., 1.);
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
            }
            // Over the list's island, which sits back under it.
            Some((detail, shift)) => content
                .when(beneath != 0., |content| content.overflow_hidden())
                .child(div().absolute().top_0().bottom_0().w_full().left(px(beneath)).child(list.size_full()))
                .when(dim > 0., |content| content.child(scrim(dim, radius::PANE)))
                .child(div().absolute().top_0().bottom_0().w_full().left(px(shift)).child(detail)),
            None => content.child(list.size_full()),
        };
        content.child(drag_over.absolute().size_0()).into_any_element()
    }

    /// A single left press on layer `layer`, from which a sideways drag may close it (MS-266).
    fn press_layer_listener(
        layer: SharedString,
        cx: &mut Context<Self>,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, _| {
            if event.click_count == 1 {
                this.press_layer(&layer, event.position);
            }
        })
    }

    /// The docked compose column, `width` wide.
    fn render_docked_column(&self, width: f32) -> Div {
        div().flex_shrink_0().w(px(width)).h_full().child(self.compose.clone())
    }

    /// The list's island: its title row and search field over the rows, an open bundle sliding
    /// over the rows on a page of its own. The rows sit `rows_shift` right of their place (left,
    /// under a bundle). With `laid_out`, the content keeps that width (the island's, rim
    /// included) whatever the island's, clipped by it.
    fn render_list_island(
        &self,
        bundles: Vec<AnyElement>,
        rows_shift: f32,
        laid_out: Option<f32>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let column = div().flex().flex_col().h_full();
        let column = match laid_out {
            Some(width) => column.flex_shrink_0().w(px(width - 2. * stroke::HAIRLINE)),
            None => column.w_full(),
        };
        let column = column
            .child(self.render_list_header(p, window, cx))
            .child(search_field("search", search_focused, input::field(&self.search), cx.listener(Self::cancel_search), p))
            .children(self.render_account_notice(p, cx))
            .child(hairline(p))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(div().absolute().top_0().bottom_0().w_full().left(px(rows_shift)).child(self.render_list(p, cx)))
                    .children(bundles),
            );
        island(p.tray, p).occlude().child(column)
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
                .id("compose-grip")
                .w(px(compose_metrics::GRIP_WIDTH))
                .h(px(compose_metrics::GRIP_HEIGHT))
                .rounded_full()
                // As the list's gap (`split::grip`, MS-232): `line` vanishes into the light window.
                .group_hover("compose-gap", |s| s.bg(p.muted.opacity(0.35)))
                .group_active("compose-gap", |s| s.bg(p.muted.opacity(0.6))),
        )
}
