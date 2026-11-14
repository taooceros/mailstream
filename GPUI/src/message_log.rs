//! The message log mail between people (the reader's chat log, MS-73) and Google Chat both draw,
//! so the two look alike (MS-326): a line across it per day, and each message under its sender's
//! avatar, name and time; a sender's next message joins theirs, its time in the avatar's column on
//! hover. The user's own messages sit on the right, their avatar too, mirrored. A message's text
//! takes at most `log::SHARE` of the log's width, so the user's own leave a gap on the left and
//! others' on the right (MS-350). The message under
//! the pointer takes a faint wash laid over it (an HTML body paints backgrounds of its own), its
//! buttons floating on its top edge on the side away from the avatar.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful,
    Styled, div, px, relative,
};

use crate::design::{Palette, log as metrics, radius, space, stroke, text};

/// One message's place in the log.
pub struct Row {
    pub id: ElementId,
    /// The user's own: on the right.
    pub mine: bool,
    /// Joins the message above it (same sender, close in time): no avatar or name.
    pub continued: bool,
    /// Under the pointer: the wash and the buttons.
    pub hovered: bool,
}

/// A line across the log with `label` in it: a day, or where unread messages start.
pub fn divider(label: SharedString, color: Hsla, line: Hsla) -> Div {
    // A half-point hairline vanishes at some offsets on a 1x screen.
    let rule = || div().flex_1().h(px(stroke::RIM)).bg(line);
    div()
        .py(px(space::S12))
        .flex()
        .items_center()
        .gap(px(space::S12))
        .text_size(px(text::FOOTNOTE))
        .text_color(color)
        .child(rule())
        .child(label)
        .child(rule())
}

/// A day's line.
pub fn day_divider(label: SharedString, p: &Palette) -> Div {
    divider(label, p.muted, p.line)
}

/// The avatar column of a message joining a run: its time, in `color` (transparent unless it is
/// hovered or calls for attention).
pub fn run_time(time: SharedString, color: Hsla, bold: bool) -> Div {
    div()
        .pt(px(space::S2))
        .text_size(px(text::TAG))
        .text_color(color)
        .when(bold, |d| d.font_weight(FontWeight::SEMIBOLD))
        .child(time)
}

/// A run's first line: the sender's name, its time, then `marks` (unread, starred); the user's
/// own mirrored, the name beside their avatar on the right.
pub fn head(
    name: SharedString,
    time: SharedString,
    marks: impl IntoIterator<Item = AnyElement>,
    mine: bool,
    p: &Palette,
) -> Div {
    div()
        .pb(px(space::S4))
        .flex()
        .when(mine, |d| d.flex_row_reverse())
        .items_baseline()
        .gap(px(space::S8))
        .min_w_0()
        .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
        .child(div().flex_shrink_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(time))
        .children(marks)
}

/// A message: `lead` (its sender's avatar, or for a message joining a run its `run_time`) in the
/// avatar's column, then `head` and `content` in a column beside it, at most `log::SHARE` of the
/// log's width (an HTML body too: in the log it is a person's reply, which reflows, MS-350), its
/// pieces at its right edge for the user's own, each as wide as its content: a piece with no width
/// of its own (an HTML body) sets one (`w_full`). The gap above belongs to the message, so
/// `actions` floating over it keep it hovered; the caller adds the hover handler.
pub fn message(
    row: Row,
    lead: impl IntoElement,
    head: Option<Div>,
    content: impl IntoIterator<Item = AnyElement>,
    actions: Option<AnyElement>,
    p: &Palette,
) -> Stateful<Div> {
    let Row { id, mine, continued, hovered } = row;
    let gutter = div().w(px(metrics::GUTTER)).flex_shrink_0().flex().when(mine, |d| d.justify_end()).child(lead);
    let column = div()
        .flex_1()
        .min_w_0()
        .max_w(relative(metrics::SHARE))
        .flex()
        .flex_col()
        .when(mine, |d| d.items_end())
        .children(head)
        .children(content);
    div()
        .id(id)
        .relative()
        .pt(px(if continued { space::S2 } else { space::S12 }))
        .child(
            div()
                .relative()
                .px(px(space::S12))
                .py(px(space::S6))
                .flex()
                .when(mine, |d| d.flex_row_reverse())
                .items_start()
                .child(gutter)
                .child(column)
                .when(hovered, |d| d.child(div().absolute().inset_0().rounded(px(radius::CONTROL)).bg(p.hover))),
        )
        .when_some(actions.filter(|_| hovered), |d, actions| {
            let place = div().absolute().top_0();
            let place = if mine { place.left(px(space::S12)) } else { place.right(px(space::S12)) };
            d.child(place.child(actions))
        })
}
