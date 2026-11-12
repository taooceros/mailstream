//! The window's own settings (MS-209): the list's and the calendar's width beside what they open
//! (`split.rs`), and the frame rate meter (`frame_rate.rs`).

use std::time::Duration;

use gpui::{AppContext, Context, Window};
use mailstream::SettingValue;

use super::{MailApp, ToggleFrameRate};
use crate::engine::engine;
use crate::frame_rate::FrameRateMeter;
use crate::split::{self, CALENDAR_WIDTH, LIST_WIDTH, ResetListWidth, SHOW_FRAME_RATE, shares};

/// A dragged width is written once the drag has rested this long.
const SAVE_AFTER: Duration = Duration::from_millis(400);

impl MailApp {
    /// Reads the widths and the meter's switch from settings.json, as the engine has them.
    pub(super) fn load_layout_settings(&self, cx: &mut Context<Self>) {
        self.background(cx, async { engine().settings().await }, |this, settings, cx| {
            let Ok(settings) = settings else { return };
            let share = |key: &str| {
                settings.settings.iter().find(|entry| entry.key == key).and_then(|entry| match entry.value {
                    SettingValue::Number { value } => Some(value as f32 / 1000.),
                    _ => None,
                })
            };
            // A drag still being written keeps its width.
            if this.share_save.is_none() {
                this.list_share = share(LIST_WIDTH).unwrap_or(shares(LIST_WIDTH).default);
                let calendar = share(CALENDAR_WIDTH).unwrap_or(shares(CALENDAR_WIDTH).default);
                this.set_calendar_share(calendar, cx);
            }
            let shown = settings
                .settings
                .iter()
                .any(|entry| entry.key == SHOW_FRAME_RATE && entry.value == SettingValue::Flag { enabled: true });
            this.show_frame_rate(shown, cx);
            cx.notify();
        });
    }

    fn set_calendar_share(&mut self, share: f32, cx: &mut Context<Self>) {
        self.calendar_share = share;
        self.calendar.update(cx, |pane, cx| pane.set_list_share(share, cx));
    }

    fn show_frame_rate(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown == self.frame_meter.is_some() {
            return;
        }
        let window = self.window.window_id();
        self.frame_meter = shown.then(|| cx.new(|cx| FrameRateMeter::new(window, cx)));
    }

    /// Follows a drag of the gap after the list (or the calendar) to the pointer's `x`.
    pub fn drag_list_edge(&mut self, x: f32, cx: &mut Context<Self>) {
        let width = split::dragged_width(x, self.split_left, self.split_mail);
        let (key, room) = if self.calendar_view { (CALENDAR_WIDTH, self.split_mail) } else { (LIST_WIDTH, self.split_room) };
        let range = shares(key);
        let share = (width / room.max(1.)).clamp(range.least, range.most);
        if self.calendar_view {
            self.set_calendar_share(share, cx);
        } else {
            self.list_share = share;
        }
        cx.notify();
        self.share_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let value = SettingValue::Number { value: (share * 1000.).round() as i64 };
            let result = engine().set_setting(key.into(), value).await;
            this.update(cx, |this, cx| {
                this.share_save = None;
                if let Err(error) = result {
                    this.show_toast(format!("Could not keep the list's width: {error}"), cx);
                }
            })
            .ok();
        }));
    }

    /// Puts the default width back for the list in view (a double click on its gap).
    pub(crate) fn reset_list_width(&mut self, _: &ResetListWidth, _: &mut Window, cx: &mut Context<Self>) {
        self.share_save = None;
        let key = if self.calendar_view { CALENDAR_WIDTH } else { LIST_WIDTH };
        let share = shares(key).default;
        if self.calendar_view {
            self.set_calendar_share(share, cx);
        } else {
            self.list_share = share;
        }
        cx.notify();
        self.background(cx, async move { engine().reset_setting(key.into()).await }, |this, result, cx| {
            if let Err(error) = result {
                this.show_toast(format!("Could not reset the list's width: {error}"), cx);
            }
        });
    }

    /// Shows or hides the frame rate meter, keeping the choice in settings.json.
    pub(crate) fn toggle_frame_rate(&mut self, _: &ToggleFrameRate, _: &mut Window, cx: &mut Context<Self>) {
        let shown = self.frame_meter.is_none();
        self.show_frame_rate(shown, cx);
        cx.notify();
        let value = SettingValue::Flag { enabled: shown };
        self.background(cx, async move { engine().set_setting(SHOW_FRAME_RATE.into(), value).await }, |this, result, cx| {
            if let Err(error) = result {
                this.show_toast(format!("Could not keep the frame rate setting: {error}"), cx);
            }
        });
    }
}
