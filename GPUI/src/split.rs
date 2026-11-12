//! The list's width beside an open conversation or event (Flutter's `flutter.listWidth` and
//! `flutter.calendarWidth`, MS-209): dragging the gap between them moves the list's edge, and the
//! width is kept in settings.json as a share of the room beside the sidebar (`gpui.listWidth`,
//! `gpui.calendarWidth`, in thousandths); a double click on the gap puts the default back.
//!
//! The gap holds gpui-kit's resize handle, which owns the band, the cursor and the drag; it wears
//! a grip shaped like the compose column's (`ui::resize_gap`). The window (`MailApp`) follows the
//! drag and keeps the share.

use std::rc::Rc;
use std::sync::LazyLock;

use mailstream::{SettingEntry, SettingValue};

use gpui::base::{InteractiveElementExt as _, ResizeHandle, ResizeHandleRenderer, ResizeHandleState, resize_handle};
use gpui::prelude::FluentBuilder as _;
use gpui::{AppContext as _, Axis, Div, EmptyView, IntoElement, ParentElement, Styled, actions, div, px};

use crate::design::{self, compose as metrics, layout};

actions!(split, [ResetListWidth]);

pub const LIST_WIDTH: &str = "gpui.listWidth";
pub const CALENDAR_WIDTH: &str = "gpui.calendarWidth";
pub const SHOW_FRAME_RATE: &str = "gpui.showFrameRate";

/// A width setting's share as the engine describes it (`mailstream::setting_defaults`, the one
/// copy of the defaults): the default and the range settings.json may keep, from thousandths.
#[derive(Clone, Copy)]
pub struct Shares {
    pub default: f32,
    pub least: f32,
    pub most: f32,
}

/// The engine's `Shares` of the width setting `key`.
pub fn shares(key: &str) -> Shares {
    static DEFAULTS: LazyLock<Vec<SettingEntry>> = LazyLock::new(mailstream::setting_defaults);
    let entry = DEFAULTS.iter().find(|entry| entry.key == key).expect("the engine describes every width setting");
    let SettingValue::Number { value } = entry.default_value else { panic!("{key} is a number setting") };
    let thousandths = |value: Option<i64>| value.expect("a number setting has a range") as f32 / 1000.;
    Shares { default: value as f32 / 1000., least: thousandths(entry.minimum), most: thousandths(entry.maximum) }
}

/// The drag of the gap beside the mail list or the calendar (whichever is in view).
#[derive(Clone, Copy)]
pub struct ResizeList;

/// The list's width for a list edge dragged to `x` (window coordinates): the list starts at
/// `left`, and its edge is the middle of the gap after it. It stays at least `MIN_LIST` wide and
/// leaves the page beside it at least `MIN_DETAIL` of `mail`.
pub fn dragged_width(x: f32, left: f32, mail: f32) -> f32 {
    let widest = layout::MIN_LIST.max(mail - layout::GAP - layout::MIN_DETAIL);
    (x - left - layout::GAP / 2.).clamp(layout::MIN_LIST, widest)
}

/// The handle in the gap after the list: zero wide, its band straddling where the caller places it
/// (the middle of the gap). Double-clicked, it asks the window to put the default width back.
pub fn list_handle(id: &'static str) -> Div {
    let handle: ResizeHandle<ResizeList, EmptyView> = resize_handle(id, Axis::Horizontal)
        .with_appearance(grip())
        .on_drag(ResizeList, |_, _, _, cx| cx.new(|_| EmptyView))
        .on_double_click(|_, window, cx| window.dispatch_action(Box::new(ResetListWidth), cx));
    div().absolute().top_0().bottom_0().w_0().child(handle)
}

/// The gap shows nothing at rest and the grip under the pointer, held or dragged. The band's
/// content is its one-pixel middle; the grip, wider, is pulled back by half its overhang to sit
/// centred on it (flex centring returns an overflowing child to the start).
fn grip() -> ResizeHandleRenderer {
    Rc::new(|handle, window, _| {
        let p = design::palette(window.appearance());
        // As gpui-kit's own divider: fainter on hover, solid while held or dragged.
        let opacity = match handle.state() {
            ResizeHandleState::Idle => 0.,
            ResizeHandleState::Hovered => 0.35,
            ResizeHandleState::Pressed => 0.6,
            ResizeHandleState::Dragging => 0.9,
        };
        Some(
            div()
                .flex_none()
                .w(px(1.))
                .h_full()
                .flex()
                .items_center()
                .when(opacity > 0., |line| {
                    line.child(
                        div()
                            .flex_none()
                            .w(px(metrics::GRIP_WIDTH))
                            .h(px(metrics::GRIP_HEIGHT))
                            .ml(px((1. - metrics::GRIP_WIDTH) / 2.))
                            .rounded_full()
                            .bg(p.muted.opacity(opacity)),
                    )
                })
                .into_any_element(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_come_from_the_engine() {
        for key in [LIST_WIDTH, CALENDAR_WIDTH] {
            let shares = shares(key);
            assert_eq!((shares.default, shares.least, shares.most), (0.4, 0.1, 0.9), "{key}");
        }
    }
}
