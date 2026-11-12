//! The frame rate meter (Flutter's `frame_rate.dart`, MS-121; here MS-209): the rate of frames
//! drawn one after another and the slowest frame's work since the last reading, in a capsule in
//! the window's bottom leading corner while `gpui.showFrameRate` is on (Settings, or ctrl-alt-p;
//! cmd-alt-p on the Mac).
//!
//! It reads GPUI Fast's frame events (its `profiler` feature), which GPUI records only while
//! tracing is on: the meter turns tracing on while it exists and off when it goes, so it costs
//! nothing while hidden. Every `REFRESH` it takes the events since the last reading and repaints
//! only itself. The rate counts only presents that followed the previous one while the window
//! kept drawing (GPUI's `PresentTiming::animation_interval`), so it is the rate frames reach the
//! screen while scrolling, dragging or animating, and lower as frames drop, rather than falling
//! whenever the app rests (as Flutter's `FrameRateCounter`). Frames GPUI draws on input are
//! counted too, so on Wayland it can read above the display's rate while the pointer drags.

use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::profiler::{FrameEvent, FrameTiming, FrameTimingCollector, set_trace_enabled};
use gpui::{Context, FontWeight, IntoElement, ParentElement, Render, Styled, Task, Window, WindowId, div, px};

use crate::design::{self, space, text};

/// How often the reading changes.
const REFRESH: Duration = Duration::from_millis(500);

/// What the meter shows.
#[derive(Clone, Copy, PartialEq)]
struct Reading {
    fps: f64,
    /// The slowest frame's work: from its frame signal (or first invalidation) until it was
    /// submitted.
    worst: Duration,
    /// Nothing animated since the last reading: the numbers are the last burst's.
    idle: bool,
}

pub struct FrameRateMeter {
    window: WindowId,
    collector: FrameTimingCollector,
    /// A frame drawn and not yet presented when the last reading was taken.
    drawn: Option<FrameTiming>,
    reading: Option<Reading>,
    _ticks: Task<()>,
}

impl FrameRateMeter {
    pub fn new(window: WindowId, cx: &mut Context<Self>) -> Self {
        set_trace_enabled(true);
        let ticks = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |meter, cx| meter.take(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            window,
            collector: FrameTimingCollector::new(),
            drawn: None,
            reading: None,
            _ticks: ticks,
        }
    }

    /// Reads the frames since the last reading and repaints when the numbers changed.
    fn take(&mut self, cx: &mut Context<Self>) {
        let mut span = Duration::ZERO;
        let mut intervals = 0u32;
        let mut worst = Duration::ZERO;
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(frame) if frame.window_id == self.window => self.drawn = Some(frame),
                FrameEvent::Present(present) if present.window_id == self.window => {
                    if let Some(frame) = self.drawn.take() {
                        worst = worst.max(frame.work(present.present_start).unwrap_or_else(|| frame.draw_duration()));
                    }
                    if let Some(interval) = present.animation_interval.filter(|interval| !interval.is_zero()) {
                        span += interval;
                        intervals += 1;
                    }
                }
                _ => {}
            }
        }
        let reading = if intervals > 0 {
            Some(Reading { fps: f64::from(intervals) / span.as_secs_f64(), worst, idle: false })
        } else {
            self.reading.map(|last| Reading { idle: true, ..last })
        };
        let shown = |reading: Option<Reading>| reading.map(|r| (r.fps.round() as i64, r.worst.as_micros() / 100, r.idle));
        if shown(reading) != shown(self.reading) {
            cx.notify();
        }
        self.reading = reading;
    }
}

impl Drop for FrameRateMeter {
    fn drop(&mut self) {
        set_trace_enabled(false);
    }
}

impl Render for FrameRateMeter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let capsule = div()
            .flex()
            .items_center()
            .gap(px(space::S6))
            .px(px(space::S10))
            .py(px(space::S4))
            .rounded_full()
            .bg(p.toast.opacity(0.85))
            .text_color(p.on_toast)
            .text_size(px(text::CAPTION))
            .whitespace_nowrap();
        let Some(reading) = self.reading else { return capsule.child("– fps") };
        // Below 90% of the display's rate, or a frame over one refresh, shows red.
        let refresh = window.refresh_interval(cx).filter(|interval| !interval.is_zero());
        let slow_rate = refresh.is_some_and(|interval| reading.fps < 0.9 / interval.as_secs_f64());
        let slow_frame = refresh.is_some_and(|interval| reading.worst > interval);
        let ms = reading.worst.as_secs_f64() * 1000.;
        capsule
            .opacity(if reading.idle { 0.55 } else { 1. })
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(slow_rate, |d| d.text_color(p.danger))
                    .child(format!("{} fps", reading.fps.round())),
            )
            .child("·")
            .child(div().when(slow_frame, |d| d.text_color(p.danger)).child(format!("max {ms:.1} ms")))
    }
}

