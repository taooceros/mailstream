//! The compose column (Flutter's `ComposePane` and `StashedComposeBar`, MS-61, MS-66): the drafts
//! being written, in the main window beside the mail, so other mail can be read meanwhile. It docks
//! at the window's trailing edge (its width dragged by the gap beside it, laid out in `ui/mod.rs`),
//! swaps to the other side of the conversation, floats over the window (moved by its title, sized
//! from its top-left corner) or is minimized to a capsule in the bottom-right corner. Each draft is
//! a tab; drafts stay in memory until sent or discarded (not across launches).

use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Bounds, ClickEvent, Context, CursorStyle, DragMoveEvent, ElementId, EmptyView, Entity, EntityId,
    EventEmitter, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, Render,
    SharedString, Size, StatefulInteractiveElement, Styled, Subscription, Window, div, point, px, size,
};

use super::{ATTACH_TIP, Attach, Compose, ComposeMode, DraftEvent, send_button};
use crate::controls::{capsule, hairline, icon_button, island, pill, raised_capsule, title_bar};
use crate::design::{self, Palette, compose as metrics, layout, radius, space, text};
use crate::icons::{Icon, icon};
use crate::model::Account;

/// What the column tells the window.
pub enum ColumnEvent {
    /// A draft went out; a reply names the conversation it answered.
    Sent { reply: Option<SharedString> },
    /// The keyboard goes back to the list and the conversation.
    Unfocus,
    /// A "+" asked for a new message, whose sender the window picks (the mailbox in view).
    NewDraft,
}

/// A drag of the gap beside the docked column, which moves the column's free edge. `anchor` is
/// its other edge (in the window); `leading` when that is its left one.
#[derive(Clone, Copy)]
pub struct ResizeColumn {
    pub anchor: f32,
    pub leading: bool,
    pub widest: f32,
}

impl ResizeColumn {
    /// The column's width with the gap's middle at `x`.
    pub fn width(&self, x: f32) -> f32 {
        let width = if self.leading { x - self.anchor } else { self.anchor - x } - layout::GAP / 2.;
        width.clamp(metrics::MIN_COLUMN, self.widest.max(metrics::MIN_COLUMN))
    }
}

/// The floating draft moved by its title, and sized from its top-left corner.
struct MoveFloat;
struct SizeFloat;

struct Tab {
    draft: Entity<Compose>,
    _subscriptions: [Subscription; 2],
}

pub struct ComposeColumn {
    tabs: Vec<Tab>,
    active: usize,
    /// Drafts asked for with no window at hand (a forward whose messages just arrived), opened
    /// as the window draws next.
    pending: Vec<(ComposeMode, Vec<Account>)>,
    pub stashed: bool,
    pub floating: bool,
    /// Docked on the other side of the conversation: between it and the list, or at the leading
    /// edge with none open.
    pub swapped: bool,
    width: f32,
    /// The floating draft's top-left corner (the bottom-right corner until it is moved) and size.
    float_origin: Option<Point<Pixels>>,
    float_size: Size<Pixels>,
    /// Where the pointer holds the floating draft while moving it, from its top-left corner.
    grab: Option<Point<Pixels>>,
}

impl EventEmitter<ColumnEvent> for ComposeColumn {}

impl ComposeColumn {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            pending: Vec::new(),
            stashed: false,
            floating: false,
            swapped: false,
            width: metrics::COLUMN_WIDTH,
            float_origin: None,
            float_size: size(px(metrics::FLOAT_WIDTH), px(metrics::FLOAT_HEIGHT)),
            grab: None,
        }
    }

    fn is_open(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// A column beside the mail.
    pub fn docked(&self) -> bool {
        self.is_open() && !self.stashed && !self.floating
    }

    /// A draft floating over the window.
    pub fn floats(&self) -> bool {
        self.is_open() && !self.stashed && self.floating
    }

    /// The capsule in the bottom-right corner.
    pub fn minimized(&self) -> bool {
        self.is_open() && self.stashed
    }

    /// The widest the docked column gets in `room` (beside the navigation), leaving the mail
    /// `MIN_MAIL`.
    pub fn widest(room: f32) -> f32 {
        (room - metrics::MIN_MAIL - layout::GAP).clamp(metrics::MIN_COLUMN, metrics::MAX_COLUMN)
    }

    /// The docked column's width in `room`.
    pub fn width(&self, room: f32) -> f32 {
        self.width.clamp(metrics::MIN_COLUMN, Self::widest(room))
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if (self.width - width).abs() >= 0.5 {
            self.width = width;
            cx.notify();
        }
    }

    /// Where the floating draft sits in a window of `viewport`: within its edges, never smaller
    /// than `MIN_FLOAT` nor larger than `MAX_FLOAT`.
    pub fn float_bounds(&self, viewport: Size<Pixels>) -> Bounds<Pixels> {
        let (window_width, window_height) = (f32::from(viewport.width), f32::from(viewport.height));
        let fit = |length: f32, room: f32| {
            length.clamp(metrics::MIN_FLOAT, (room - 2. * metrics::FLOAT_MARGIN).clamp(metrics::MIN_FLOAT, metrics::MAX_FLOAT))
        };
        let width = fit(f32::from(self.float_size.width), window_width);
        let height = fit(f32::from(self.float_size.height), window_height);
        let origin = self.float_origin.map_or(
            (window_width - metrics::FLOAT_MARGIN - width, window_height - metrics::FLOAT_MARGIN - height),
            |origin| (f32::from(origin.x), f32::from(origin.y)),
        );
        let x = origin.0.clamp(layout::GAP, (window_width - width - layout::GAP).max(layout::GAP));
        let y = origin.1.clamp(layout::GAP, (window_height - height - layout::GAP).max(layout::GAP));
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    fn active_draft(&self) -> Option<&Entity<Compose>> {
        self.tabs.get(self.active).map(|tab| &tab.draft)
    }

    // MARK: Drafts

    /// Adds a draft as a tab and shows it, the keyboard in its first empty field.
    pub fn open(&mut self, mode: ComposeMode, accounts: Vec<Account>, window: &mut Window, cx: &mut Context<Self>) {
        let draft = cx.new(|cx| Compose::new(mode, accounts, window, cx));
        let subscriptions = [
            cx.subscribe_in(&draft, window, |this, draft, event: &DraftEvent, window, cx| match event {
                DraftEvent::Sent => {
                    let reply = draft.read(cx).reply.as_ref().map(|(conversation, ..)| conversation.clone());
                    this.remove(draft.entity_id(), window, cx);
                    cx.emit(ColumnEvent::Sent { reply });
                }
                DraftEvent::Closed => this.remove(draft.entity_id(), window, cx),
                DraftEvent::Unfocus => cx.emit(ColumnEvent::Unfocus),
            }),
            // The tabs and the capsule name drafts by their subject and recipients.
            cx.observe(&draft, |_, _, cx| cx.notify()),
        ];
        draft.update(cx, |draft, cx| draft.focus_first(window, cx));
        self.tabs.push(Tab { draft, _subscriptions: subscriptions });
        self.active = self.tabs.len() - 1;
        self.stashed = false;
        cx.notify();
    }

    /// Opens a draft as the window draws next.
    pub fn queue(&mut self, mode: ComposeMode, accounts: Vec<Account>, cx: &mut Context<Self>) {
        self.pending.push((mode, accounts));
        cx.notify();
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn open_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (mode, accounts) in std::mem::take(&mut self.pending) {
            self.open(mode, accounts, window, cx);
        }
    }

    /// A new message: the draft shown, when it is a new message with nothing written yet,
    /// otherwise a new tab.
    pub fn new_message(
        &mut self,
        from: Option<SharedString>,
        accounts: Vec<Account>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.active_draft().cloned() {
            let blank = {
                let draft = draft.read(cx);
                draft.reply.is_none() && !draft.from_locked && !draft.has_draft(cx)
            };
            if blank {
                self.stashed = false;
                draft.update(cx, |draft, cx| draft.focus_first(window, cx));
                cx.notify();
                return;
            }
        }
        self.open(ComposeMode::New { from }, accounts, window, cx);
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.stashed = false;
        self.tabs[index].draft.update(cx, |draft, cx| draft.focus_first(window, cx));
        cx.notify();
    }

    /// Closes a tab, showing it first when it asks whether to discard what is written.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.tabs.get(index).map(|tab| tab.draft.clone()) else { return };
        if index != self.active {
            self.select(index, window, cx);
        }
        draft.update(cx, |draft, cx| draft.request_close(cx));
    }

    /// A draft sent or discarded leaves; the keyboard goes to the next one shown, or back to the
    /// mail.
    fn remove(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.draft.entity_id() == id) else { return };
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        match self.active_draft().cloned() {
            Some(draft) if !self.stashed => draft.update(cx, |draft, cx| draft.focus_first(window, cx)),
            Some(_) => {}
            None => {
                self.stashed = false;
                cx.emit(ColumnEvent::Unfocus);
            }
        }
        cx.notify();
    }

    // MARK: Placement

    fn minimize(&mut self, cx: &mut Context<Self>) {
        self.stashed = true;
        cx.emit(ColumnEvent::Unfocus);
        cx.notify();
    }

    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select(self.active, window, cx);
    }

    fn toggle_floating(&mut self, cx: &mut Context<Self>) {
        self.floating = !self.floating;
        cx.notify();
    }

    fn toggle_swapped(&mut self, cx: &mut Context<Self>) {
        self.swapped = !self.swapped;
        cx.notify();
    }

    fn move_float(&mut self, event: &DragMoveEvent<MoveFloat>, window: &mut Window, cx: &mut Context<Self>) {
        let position = event.event.position;
        let grab = *self.grab.get_or_insert(position - event.bounds.origin);
        // Kept within the window as `float_bounds` puts it.
        self.float_origin = Some(position - grab);
        self.float_origin = Some(self.float_bounds(window.viewport_size()).origin);
        cx.notify();
    }

    /// The top-left corner follows the pointer; the bottom-right one stays.
    fn size_float(&mut self, event: &DragMoveEvent<SizeFloat>, window: &mut Window, cx: &mut Context<Self>) {
        let corner = event.bounds.bottom_right();
        let position = event.event.position;
        let viewport = window.viewport_size();
        let fit = |length: Pixels, room: Pixels| {
            let longest = (f32::from(room) - 2. * metrics::FLOAT_MARGIN).clamp(metrics::MIN_FLOAT, metrics::MAX_FLOAT);
            px(f32::from(length).clamp(metrics::MIN_FLOAT, longest))
        };
        let width = fit(corner.x - position.x, viewport.width);
        let height = fit(corner.y - position.y, viewport.height);
        self.float_size = size(width, height);
        self.float_origin = Some(point(corner.x - width, corner.y - height));
        cx.notify();
    }

    // MARK: Rendering

    /// Over several drafts: a tab each (its glyph, its name, ✕), then "+".
    fn render_tabs(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let draft = tab.draft.read(cx);
            let selected = index == self.active;
            let ink = if selected { p.text } else { p.muted };
            div()
                .id(ElementId::NamedInteger("compose-tab".into(), index as u64))
                .flex_shrink_0()
                .h_full()
                .pl(px(space::S8))
                .pr(px(space::S4))
                .flex()
                .items_center()
                .gap(px(space::S4))
                .rounded(px(radius::SMALL))
                .cursor_pointer()
                .when(selected, |d| d.bg(p.selection))
                .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(index, window, cx)))
                .child(icon(draft.glyph, metrics::TAB_GLYPH, ink))
                .child(
                    div()
                        .max_w(px(metrics::TAB_TITLE))
                        .truncate()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(ink)
                        .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(draft.tab_title(cx)),
                )
                .child(
                    div()
                        .id(ElementId::NamedInteger("compose-tab-close".into(), index as u64))
                        .size(px(metrics::TAB_CLOSE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .hover(|s| s.bg(p.hover))
                        .child(icon(Icon::Close, metrics::TAB_GLYPH - 2., p.muted))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.close_tab(index, window, cx);
                        })),
                )
        });
        div()
            .flex_shrink_0()
            .h(px(metrics::TAB_ROW))
            .px(px(space::S8))
            .py(px(space::S4))
            .flex()
            .items_center()
            .gap(px(space::S4))
            .child(
                div()
                    .id("compose-tabs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .gap(px(space::S4))
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(new_draft_button("compose-tabs-new", p, cx))
    }

    /// The draft's toolbar, as a conversation's: close and minimize, its name, then the column's
    /// tools and Send. Docked, it moves the window as a title bar does; floating, it moves the
    /// draft.
    fn render_title_bar(
        &self,
        draft: &Entity<Compose>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (title, sending) = {
            let draft = draft.read(cx);
            (draft.title.clone(), draft.sending())
        };
        let bar = if self.floating {
            div()
                .id("compose-title")
                .cursor(CursorStyle::OpenHand)
                .on_drag(MoveFloat, {
                    // Each move holds the draft where the pointer took it.
                    let this = cx.entity().downgrade();
                    move |_, _, _, cx| {
                        this.update(cx, |this, _| this.grab = None).ok();
                        cx.new(|_| EmptyView)
                    }
                })
        } else {
            title_bar("compose-title", window, cx)
        };
        let (swap_tip, float_glyph, float_tip) = (
            if self.swapped { "Move to the Right Edge" } else { "Move to the Other Side of the Conversation" },
            if self.floating { Icon::Dock } else { Icon::Float },
            if self.floating { "Dock as a Column" } else { "Float over the Window" },
        );
        let send_draft = draft.clone();
        let attach_draft = draft.clone();
        bar.flex_shrink_0()
            .h(px(layout::HEADER_HEIGHT))
            .px(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(
                capsule(p)
                    .child(icon_button("compose-close", Icon::Close, "Close Draft (⌘W)", !sending, p.glyph).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.close_tab(this.active, window, cx)),
                    ))
                    .child(
                        icon_button("compose-minimize", Icon::Minimize, "Minimize", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.minimize(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(text::PANE_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                capsule(p)
                    .when(self.tabs.len() == 1, |d| d.child(new_draft_button("compose-new", p, cx)))
                    .when(!self.floating, |d| {
                        d.child(
                            icon_button("compose-swap", Icon::Swap, swap_tip, true, p.glyph)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_swapped(cx))),
                        )
                    })
                    .child(
                        icon_button("compose-float", float_glyph, float_tip, true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_floating(cx))),
                    )
                    .child(icon_button("compose-attach", Icon::Attachment, ATTACH_TIP, !sending, p.glyph).on_click(
                        move |_, window, cx| attach_draft.update(cx, |draft, cx| draft.pick_files(&Attach, window, cx)),
                    )),
            )
            .child(
                send_button("compose-send", sending, p)
                    .on_click(move |_, window, cx| send_draft.update(cx, |draft, cx| draft.send(window, cx))),
            )
    }

    /// Minimized: the shown draft's name (and how many there are), which brings the column back,
    /// and "+".
    fn render_capsule(&self, draft: &Entity<Compose>, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.tabs.len();
        raised_capsule(p)
            .occlude()
            .gap(px(space::S4))
            .child(
                div()
                    .id("compose-restore")
                    .h(px(design::icon::BUTTON))
                    .px(px(space::S10))
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.restore(window, cx)))
                    .child(icon(Icon::Compose, 15., p.accent))
                    .child(
                        div()
                            .max_w(px(metrics::CAPSULE_TITLE))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(draft.read(cx).tab_title(cx)),
                    )
                    .when(count > 1, |d| d.child(pill(format!("{count} drafts"), None, p)))
                    .child(icon(Icon::Dock, 14., p.muted)),
            )
            .child(new_draft_button("compose-capsule-new", p, cx))
    }
}

/// "+": a new message.
fn new_draft_button(id: &'static str, p: &'static Palette, cx: &mut Context<ComposeColumn>) -> impl IntoElement {
    icon_button(id, Icon::AddDraft, "New Message (⌘N)", true, p.glyph)
        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ColumnEvent::NewDraft)))
}

impl Render for ComposeColumn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let Some(draft) = self.active_draft().cloned() else { return div().into_any_element() };
        if self.stashed {
            return self.render_capsule(&draft, p, cx).into_any_element();
        }
        island(p.background, p)
            .occlude()
            .size_full()
            .flex()
            .flex_col()
            // Floating, it is lifted over the islands beneath as a popover is.
            .when(self.floating, |d| d.shadow(p.popover_shadow.clone()))
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .on_drag_move(cx.listener(Self::move_float))
            .on_drag_move(cx.listener(Self::size_float))
            .when(self.tabs.len() > 1, |d| d.child(self.render_tabs(p, cx)).child(hairline(p)))
            .child(self.render_title_bar(&draft, p, window, cx))
            .child(div().flex_1().min_h_0().child(draft))
            .when(self.floating, |d| {
                d.child(
                    div()
                        .id("compose-size")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size(px(metrics::FLOAT_HANDLE))
                        .cursor(CursorStyle::ResizeUpLeftDownRight)
                        .on_drag(SizeFloat, |_, _, _, cx| cx.new(|_| EmptyView)),
                )
            })
            .into_any_element()
    }
}
