//! The app's shared controls, after Flutter's `controls.dart`, `glass.dart` and `common.dart`
//! (MS-189–193, MS-200), drawn from the tokens in `design.rs` and used by every view: buttons,
//! icon buttons and their tooltips, the capsules toolbar buttons sit on, islands, the title bars
//! that move the window, popovers, tags, pills, marks, the lifted card of a selected row, notices,
//! toasts, switches, notes, the list's group titles and the search field's frame. A view composes
//! these rather than restyling one.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, DispatchPhase, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, linear_color_stop, linear_gradient, px,
};

use crate::design::{
    self, Palette, avatar as avatar_metrics, icon as icon_size, layout, list as metrics, marks, notice as notice_metrics,
    radius, space, stroke, tag as tag_metrics, text,
};
use crate::icons::{Icon, icon};

/// What a button does: `Primary` is the one action a place leads to (the accent), `Secondary`
/// the others, `Destructive` one that deletes or discards, `Plain` a quiet text action.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Destructive,
    Plain,
}

/// A control's height: `Small` inside rows and cards, `Regular` in forms, bars and composers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ControlSize {
    Small,
    Regular,
}

impl ControlSize {
    fn height(self) -> f32 {
        match self {
            ControlSize::Small => 22.,
            ControlSize::Regular => 28.,
        }
    }

    fn padding(self) -> f32 {
        match self {
            ControlSize::Small => space::S8,
            ControlSize::Regular => space::S12,
        }
    }

    fn font(self) -> f32 {
        match self {
            ControlSize::Small => text::FOOTNOTE,
            ControlSize::Regular => text::BODY,
        }
    }
}

/// The black (white in dark mode) a filled face darkens toward under the pointer.
fn shade(p: &Palette, amount: f32) -> Hsla {
    let base = if p.dark { gpui::white() } else { gpui::black() };
    design::tint(base, amount)
}

/// A button (`MailButton`): `label` on its kind's face, `Radii.control` corners; filled faces
/// darken under the pointer, quiet ones take the wash. Its click never moves the window. Chain
/// `.on_click`; a disabled one (`enabled` false) is dimmed and inert.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    size: ControlSize,
    enabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    icon_label_button(id, None, label, kind, size, enabled, p)
}

/// A [`button`] with a glyph before its label.
pub fn icon_label_button(
    id: impl Into<ElementId>,
    glyph: Option<Icon>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    size: ControlSize,
    enabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    let filled = matches!(kind, ButtonKind::Primary | ButtonKind::Destructive);
    let face = match kind {
        ButtonKind::Primary => p.accent,
        ButtonKind::Destructive => p.danger,
        ButtonKind::Secondary => p.fill,
        ButtonKind::Plain => gpui::transparent_black(),
    };
    let ink = match kind {
        ButtonKind::Primary | ButtonKind::Destructive => p.on_accent,
        ButtonKind::Secondary => p.text,
        ButtonKind::Plain => p.accent,
    };
    let (hovered, pressed) = if filled {
        (face.blend(shade(p, 0.08)), face.blend(shade(p, 0.16)))
    } else {
        (face.blend(p.hover), face.blend(p.pressed))
    };
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(size.height()))
        .px(px(size.padding()))
        .flex()
        .items_center()
        .gap(px(space::S6))
        .rounded(px(radius::CONTROL))
        .bg(face)
        .when(kind == ButtonKind::Secondary, |d| d.border(px(stroke::HAIRLINE)).border_color(p.card_border))
        .text_size(px(size.font()))
        .font_weight(if filled { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
        .text_color(ink)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hovered)).active(move |s| s.bg(pressed)))
        .when(!enabled, |d| d.opacity(0.4))
        .children(glyph.map(|glyph| icon(glyph, if size == ControlSize::Small { icon_size::INLINE } else { 14. }, ink)))
        .child(label.into())
}

/// A round icon button (`MailIconButton`): the glyph in `color` (usually the secondary label), a
/// wash under the pointer, a tooltip naming it; a disabled one is faint and takes no clicks.
pub fn icon_button(
    id: impl Into<ElementId>,
    glyph: Icon,
    tip: impl Into<SharedString>,
    enabled: bool,
    color: Hsla,
    p: &'static Palette,
) -> Stateful<Div> {
    let tip = tip.into();
    div()
        .id(id)
        .flex_shrink_0()
        .size(px(icon_size::BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed)))
        .tooltip(move |_, cx| cx.new(|_| Tooltip { text: tip.clone(), p }).into())
        .child(icon(glyph, icon_size::TOOLBAR, if enabled { color } else { design::tint(color, 0.35) }))
}

/// A tooltip: small print on a popover.
pub struct Tooltip {
    pub text: SharedString,
    pub p: &'static Palette,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        popover(self.p)
            .font_family(design::ui_font())
            .px(px(space::S8))
            .py(px(space::S4))
            .rounded(px(radius::SMALL))
            .text_size(px(text::CAPTION))
            .text_color(self.p.text)
            .child(self.text.clone())
    }
}

/// A popover's panel (menus, tooltips, completions): the background in a hairline, rounded as a
/// card, on the popover shadow.
pub fn popover(p: &Palette) -> Div {
    div()
        .rounded(px(radius::CARD))
        .bg(p.background)
        .border(px(stroke::HAIRLINE))
        .border_color(p.line)
        .shadow(p.popover_shadow.clone())
}

/// Toolbar buttons grouped on a capsule floating over content (`GlassCapsule`): an opaque face
/// inside a hairline rim.
pub fn capsule(p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .h(px(layout::CAPSULE_HEIGHT))
        .p(px(layout::CAPSULE_PADDING))
        .flex()
        .items_center()
        .rounded_full()
        .bg(p.capsule)
        .border(px(stroke::HAIRLINE))
        .border_color(p.capsule_rim)
}

/// The capsule floating over a row (its hover actions, `RaisedCapsule`): lit from above, a hairline
/// rim, the raised shadow.
pub fn raised_capsule(p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .p(px(layout::CAPSULE_PADDING))
        .flex()
        .items_center()
        .rounded_full()
        .bg(linear_gradient(180., linear_color_stop(p.raised_capsule[0], 0.), linear_color_stop(p.raised_capsule[1], 1.)))
        .border(px(stroke::HAIRLINE))
        .border_color(p.raised_capsule_rim)
        .shadow(p.raised_shadow.clone())
}

/// A rounded island on the window (the list on the `tray`, the conversation on the `background`):
/// a hairline rim and a soft shadow lift it. Its content is clipped to it.
pub fn island(color: Hsla, p: &Palette) -> Div {
    div()
        .relative()
        .overflow_hidden()
        .rounded(px(radius::PANE))
        .bg(color)
        .border(px(stroke::HAIRLINE))
        .border_color(p.island_rim)
        .shadow(p.island_shadow.clone())
}

/// How far the pointer travels with the button held before a press on a title bar moves the
/// window; short of it the press stays a click (a tap on a touchpad wobbles a point or two).
const WINDOW_MOVE_SLOP: f64 = 4.;

/// A region that moves the window as a title bar does (the window between the islands, their
/// title rows), a double click zooming it (the Mac). The window moves only once the pointer has
/// travelled `WINDOW_MOVE_SLOP` with the button held, never on the press: on Linux the compositor
/// takes the pointer for a move (`start_window_move`), so a move begun on the press turned every
/// click and tap there into a drag of the window (MS-206). Controls and rows laid over one stop
/// their presses, so dragging them never moves the window.
pub fn title_bar(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let id = id.into();
    // Where the press that may move the window went down, until it is released or moves it.
    let press = window.use_keyed_state(id.clone(), cx, |_, _| None::<Point<Pixels>>);
    let arm = press.clone();
    // The pointer is followed over the whole window, not only the region: it may leave a narrow
    // one (the gap between islands) before it has travelled far enough.
    let follow = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let release = press.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    release.update(cx, |press, _| *press = None);
                }
            });
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let Some(start) = *press.read(cx) else { return };
                let held = event.pressed_button == Some(MouseButton::Left);
                if held && (event.position - start).magnitude() <= WINDOW_MOVE_SLOP {
                    return;
                }
                press.update(cx, |press, _| *press = None);
                if held {
                    window.start_window_move();
                }
            });
        },
    );
    div()
        .id(id)
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            let double = event.click_count == 2;
            arm.update(cx, |press, _| *press = (!double).then_some(event.position));
            if double {
                window.titlebar_double_click();
            }
        })
        .child(follow.absolute().size_0())
}

/// A hairline rule across.
pub fn hairline(p: &Palette) -> Div {
    div().flex_shrink_0().w_full().h(px(stroke::HAIRLINE)).bg(p.line)
}

/// A tag (MS-190): small print that describes, not acts (a topic, a bundle's count), in `color`
/// on a wash of it, fully round; neutral (the secondary label) without a color.
pub fn tag(label: impl Into<SharedString>, color: Option<Hsla>, p: &Palette) -> Div {
    let color = color.unwrap_or(p.muted);
    div()
        .flex_shrink_0()
        .h(px(tag_metrics::HEIGHT))
        .px(px(tag_metrics::PADDING))
        .flex()
        .items_center()
        .rounded_full()
        .bg(design::tint(color, tag_metrics::TINT))
        .text_color(color)
        .text_size(px(text::TAG))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into())
}

/// A pill naming a thing (`Pill`): an attachment, a recipient, an account; `leading` (a glyph or a
/// mark) before its label, fully round on the quiet fill.
pub fn pill(label: impl Into<SharedString>, leading: Option<AnyElement>, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .h(px(22.))
        .pl(px(if leading.is_some() { 7. } else { 9. }))
        .pr(px(9.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded_full()
        .bg(p.chip)
        .text_size(px(text::FOOTNOTE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.muted)
        .children(leading)
        .child(label.into())
}

/// An account or a topic: a dot of its color, the one mark for them (MS-190).
pub fn mark_dot(color: Hsla) -> Div {
    div().flex_shrink_0().size(px(marks::DOT)).rounded_full().bg(color)
}

/// The column before a list row's avatar: an accent dot while the row is unread, a little right
/// of center, clear of the lifted card's bar.
pub fn unread_dot(unread: bool, p: &Palette) -> Div {
    let left = (marks::UNREAD_SLOT - marks::UNREAD) / 2. * (1. + 1. / 3.);
    div().flex_shrink_0().w(px(marks::UNREAD_SLOT)).flex().items_center().when(unread, |d| {
        d.child(div().ml(px(left)).size(px(marks::UNREAD)).rounded_full().bg(p.accent))
    })
}

/// The card a row on the tray lifts off it as (MS-189), filling the row less `LIFT_GAP` / 2 top
/// and bottom, behind its content. Selected: the lifted face lit from above, an accent rim and the
/// lifted shadow, an accent bar at its start; under the pointer (`selected` false), half as far:
/// an opaque face, a hairline and the raised shadow.
pub fn lifted_card(selected: bool, p: &Palette) -> Div {
    let card = div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(metrics::LIFT_GAP / 2.))
        .bottom(px(metrics::LIFT_GAP / 2.))
        .rounded(px(radius::CARD));
    if !selected {
        return card
            .bg(p.half_lifted)
            .border(px(stroke::HAIRLINE))
            .border_color(p.card_border)
            .shadow(p.raised_shadow.clone());
    }
    card.bg(linear_gradient(180., linear_color_stop(p.lifted_card[0], 0.), linear_color_stop(p.lifted_card[1], 1.)))
        .border(px(stroke::RIM))
        .border_color(p.lifted_rim)
        .shadow(p.lifted_shadow.clone())
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(metrics::LIFT_BAR_INSET))
                .bottom(px(metrics::LIFT_BAR_INSET))
                .w(px(metrics::LIFT_BAR))
                .rounded_full()
                .bg(p.accent),
        )
}

/// A row on a list island's tray (the mail, bundle and chat lists), after Dia: `ROW_HEIGHT` tall,
/// selected it lifts off the tray as a [`lifted_card`], under the pointer half as far. With `rule`
/// a hairline runs under it from where its text starts (none beside a lifted row). Its content
/// goes on top, usually a [`row_layout`].
pub fn list_row(id: impl Into<ElementId>, selected: bool, rule: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .group("row")
        .relative()
        .flex_shrink_0()
        .h(px(metrics::ROW_HEIGHT))
        .w_full()
        .cursor_pointer()
        .child(if selected {
            lifted_card(true, p)
        } else {
            lifted_card(false, p).invisible().group_hover("row", |s| s.visible())
        })
        .when(rule && !selected, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(metrics::SEPARATOR_X))
                    .right(px(space::S8))
                    .bottom_0()
                    .h(px(stroke::HAIRLINE))
                    .bg(p.line)
                    .group_hover("row", |s| s.invisible()),
            )
        })
}

/// The layout every row shares (`_RowLayout`): the unread column, the avatar, and two lines; the
/// first is `title` then `meta` (the date) at its end, the second `subtitle` then `marks`.
pub fn row_layout(
    unread: bool,
    avatar: impl IntoElement,
    title: impl IntoElement,
    meta: impl IntoElement,
    subtitle: impl IntoElement,
    marks: Vec<AnyElement>,
    p: &Palette,
) -> Div {
    div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .pr(px(metrics::TRAILING_INSET))
        .child(unread_dot(unread, p))
        .child(avatar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .ml(px(metrics::AVATAR_GAP))
                .flex()
                .flex_col()
                .gap(px(space::S2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .child(div().flex_1().min_w_0().flex().items_center().child(title))
                        .child(meta),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::S6))
                        .child(div().flex_1().min_w_0().flex().items_center().overflow_hidden().child(subtitle))
                        .children(marks),
                ),
        )
}

/// A glyph in the avatar slot (a Smart Inbox bundle): white on `color`.
pub fn icon_avatar(glyph: Icon, color: Hsla, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(glyph, size * avatar_metrics::GLYPH, gpui::white()))
}

/// A person's initial in their hue on a wash of it (MS-192), where there is no picture. `key`
/// picks the hue (a lowercased address, else a Chat id or a name).
pub fn monogram(initial: SharedString, key: &str, size: f32, p: &Palette) -> Div {
    let hue = design::avatar_color(key, p.dark);
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(design::tint(hue, avatar_metrics::TINT))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * avatar_metrics::LETTER))
        .line_height(px(size * avatar_metrics::LETTER))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hue)
        .child(initial)
}

/// What a notice tells: `Action`, something to do (sign in again), in the indigo of "waiting on
/// you"; `Error`, something that failed, in red.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Action,
    Error,
}

/// A notice in a page (MS-191, `MailNotice`), the one look of every banner: the tone's glyph
/// before `message` (under a `title` when there is one), on a wash of the tone's color inside a
/// hairline of it. `compact`, it is one line over a view.
pub fn notice(tone: Tone, title: Option<SharedString>, message: impl Into<SharedString>, compact: bool, p: &Palette) -> Div {
    let (hue, ink, glyph) = match tone {
        Tone::Error => (p.danger, p.on_red_fill, Icon::Spam),
        Tone::Action => (p.waiting, p.on_waiting_fill, Icon::Person),
    };
    let face = design::tint(hue, if p.dark { notice_metrics::DARK_TINT } else { notice_metrics::TINT });
    let frame = div()
        .flex()
        .items_center()
        .rounded(px(radius::CARD))
        .bg(face)
        .border(px(stroke::HAIRLINE))
        .border_color(design::tint(hue, notice_metrics::RIM));
    if compact {
        return frame
            .h(px(notice_metrics::COMPACT_HEIGHT))
            .px(px(space::S10))
            .gap(px(space::S8))
            .child(icon(glyph, icon_size::ROW, ink))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .child(message.into()),
            );
    }
    frame.px(px(space::S12)).py(px(space::S10)).gap(px(space::S10)).child(icon(glyph, icon_size::TOOLBAR, ink)).child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .when_some(title, |d, title| {
                d.child(div().truncate().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
            })
            .child(div().text_color(p.text).child(message.into())),
    )
}

/// A notice floating over the bottom of the window: a dark rounded panel (red for an error).
pub fn toast(message: SharedString, error: bool, p: &Palette) -> Div {
    let (face, ink) = if error { (p.red_fill, p.on_red_fill) } else { (p.toast, p.on_toast) };
    div()
        .max_w(px(520.))
        .px(px(space::S16))
        .py(px(space::S10))
        .flex()
        .items_center()
        .gap(px(space::S8))
        .rounded(px(radius::PANE))
        .bg(face)
        .shadow(p.popover_shadow.clone())
        .text_color(ink)
        .when(error, |d| d.child(icon(Icon::Spam, icon_size::ROW, ink)))
        .child(message)
}

/// An on/off switch (`MailSwitch`): a capsule track, the accent when on, a white knob at its end.
pub fn switch(on: bool, p: &Palette) -> Div {
    const WIDTH: f32 = 32.;
    const HEIGHT: f32 = 18.;
    div()
        .flex_shrink_0()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .p(px(space::S2))
        .rounded_full()
        .bg(if on { p.accent } else { p.border })
        .flex()
        .when(on, |d| d.justify_end())
        .child(div().size(px(HEIGHT - 4.)).rounded_full().bg(gpui::white()).shadow(p.card_shadow.clone()))
}

/// A note centered in an empty view.
pub fn centered_note(note: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(space::S12))
        .items_center()
        .justify_center()
        .px(px(space::S32))
        .text_color(p.muted)
        .child(div().text_center().child(note.into()))
}

/// A group's title in a list (a date, "Waiting on you"), inside the rows' inset: small, bold,
/// secondary, at the foot of its `SECTION_HEIGHT`; with a `count`, the title takes the waiting color and the count follows.
pub fn section_header(title: impl Into<SharedString>, count: Option<usize>, p: &Palette) -> Div {
    div()
        .h(px(metrics::SECTION_HEIGHT))
        .flex()
        .items_end()
        .gap(px(space::S8))
        .pb(px(5.))
        .pl(px(metrics::SECTION_X - metrics::INSET))
        .text_size(px(text::FOOTNOTE))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(div().when(count.is_some(), |d| d.text_color(p.waiting)).child(title.into()))
        .children(count.map(|count| count.to_string()))
}

/// A text field's frame (`MailTextField`): the background in a border, the accent's ring while it
/// has the keys; `capsule` rounds it fully (the search field).
pub fn field_frame(focused: bool, capsule: bool, p: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .bg(p.background)
        .when(capsule, |d| d.rounded_full())
        .when(!capsule, |d| d.rounded(px(radius::CONTROL)))
        .when(focused, |d| {
            d.border(px(stroke::RIM)).border_color(design::tint(p.accent, 0.8)).shadow(vec![gpui::BoxShadow {
                color: design::tint(p.accent, 0.18),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
            }])
        })
        .when(!focused, |d| d.border(px(stroke::HAIRLINE)).border_color(p.border))
}
