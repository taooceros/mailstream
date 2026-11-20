//! Shared per-loop frame, touch-resampling, ADPF, and LTPO driver.
//!
//! Both Android entry points — `android-activity` (`super::jni`) and the
//! host-driven `SurfaceView` render thread (`super::host`) — drive frames
//! through [`LoopDriver`] so neither path bypasses `TouchResampler`,
//! `VsyncTimeline` presentation alignment, ADPF hints, or dynamic LTPO
//! refresh-rate voting.

#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use crate::{
    android::{adpf::AdpfSession, frame_source, window::AndroidWindow, TouchPoint},
    frame_timing::{LtpoVoteController, VsyncTimeline},
    touch_resampler::TouchResampler,
};

const AMOTION_EVENT_ACTION_DOWN: u32 = 0;
const AMOTION_EVENT_ACTION_UP: u32 = 1;
const AMOTION_EVENT_ACTION_MOVE: u32 = 2;
const AMOTION_EVENT_ACTION_CANCEL: u32 = 3;

/// Per-render-loop coordinator owning touch resampling, ADPF performance hints,
/// and dynamic LTPO refresh-rate voting.
pub(crate) struct LoopDriver {
    resampler: TouchResampler,
    adpf: Option<AdpfSession>,
    ltpo: LtpoVoteController,
}

impl LoopDriver {
    /// Creates a driver on the current render loop thread.
    ///
    /// Call after [`frame_source::install`] so the vsync thread's TID is
    /// already available for ADPF session binding.
    pub(crate) fn new() -> Self {
        let render_tid = unsafe { libc::gettid() as i32 };
        let vsync_tid = frame_source::vsync_thread_tid();
        let refresh_ns = frame_source::refresh_interval_ns();
        let adpf = AdpfSession::new(render_tid, vsync_tid, refresh_ns);
        Self {
            resampler: TouchResampler::new(),
            adpf,
            ltpo: LtpoVoteController::new(),
        }
    }

    /// Lightweight driver without an ADPF session (used by the legacy
    /// compatibility `jni::poll_events` helper).
    pub(crate) fn without_adpf() -> Self {
        Self {
            resampler: TouchResampler::new(),
            adpf: None,
            ltpo: LtpoVoteController::new(),
        }
    }

    /// Synchronizes LTPO state when a fresh `ANativeWindow` surface is attached
    /// (which already votes 120 Hz inside `AndroidWindow::new` / `init_window`).
    pub(crate) fn on_surface_attached(&mut self) {
        self.ltpo.on_surface_attached(Instant::now());
    }

    /// Clears transient touch and LTPO state when the surface is torn down.
    pub(crate) fn on_surface_destroyed(&mut self) {
        self.ltpo.on_surface_destroyed();
        self.resampler.clear();
    }

    /// Stops ADPF reporting and releases the 120 Hz LTPO vote on Activity pause.
    pub(crate) fn on_paused(&mut self, win: Option<&AndroidWindow>) {
        if let Some(adpf) = &mut self.adpf {
            adpf.set_paused(true);
        }
        if let Some(hz) = self.ltpo.on_pause() {
            if let Some(win) = win {
                win.set_frame_rate_vote(hz);
            }
        }
    }

    /// Re-enables ADPF reporting and restores 120 Hz voting on Activity resume.
    pub(crate) fn on_resumed(&mut self, win: Option<&AndroidWindow>) {
        if let Some(adpf) = &mut self.adpf {
            adpf.set_paused(false);
        }
        if let Some(win) = win {
            self.note_activity(win);
        }
    }

    /// Marks interactive/frame activity, restoring the 120 Hz vote if idle.
    pub(crate) fn note_activity(&mut self, win: &AndroidWindow) {
        if let Some(hz) = self.ltpo.on_activity(Instant::now()) {
            win.set_frame_rate_vote(hz);
        }
    }

    /// Clears tracked pointers at the start of a fresh `ACTION_DOWN` gesture.
    pub(crate) fn on_touch_stream_reset(&mut self) {
        self.resampler.clear();
    }

    /// Handles `ACTION_DOWN` / `ACTION_POINTER_DOWN` for one pointer.
    pub(crate) fn on_touch_down(
        &mut self,
        win: &AndroidWindow,
        pointer_id: i32,
        time_ns: i64,
        x: f32,
        y: f32,
    ) {
        self.note_activity(win);
        self.resampler.on_down(pointer_id, time_ns, x, y);
        win.handle_touch(TouchPoint {
            id: pointer_id,
            x,
            y,
            action: AMOTION_EVENT_ACTION_DOWN,
            predicted: None,
            timestamp: Some(frame_source::monotonic_nanos_to_instant(time_ns)),
        });
    }

    /// Records one historical or current `ACTION_MOVE` sample into the
    /// resampler. Returns `true` if accepted (non-duplicate).
    pub(crate) fn on_touch_move_sample(
        &mut self,
        pointer_id: i32,
        time_ns: i64,
        x: f32,
        y: f32,
    ) -> bool {
        self.resampler.on_move(pointer_id, time_ns, x, y)
    }

    /// Signals that at least one `ACTION_MOVE` sample was accepted: elevates
    /// LTPO to 120 Hz and schedules the next vsync frame.
    pub(crate) fn on_touch_move_accepted(&mut self, win: &AndroidWindow) {
        self.note_activity(win);
        frame_source::schedule_frame();
    }

    /// Flushes any pending raw moves for `pointer_id` and delivers `ACTION_UP`.
    pub(crate) fn on_touch_up(
        &mut self,
        win: &AndroidWindow,
        pointer_id: i32,
        time_ns: i64,
        x: f32,
        y: f32,
    ) {
        self.note_activity(win);
        let mut pending = Vec::new();
        self.resampler
            .on_up_or_cancel(pointer_id, |m| pending.push(m));
        for m in pending {
            win.handle_touch(TouchPoint {
                id: m.id,
                x: m.raw_x,
                y: m.raw_y,
                action: AMOTION_EVENT_ACTION_MOVE,
                predicted: None,
                timestamp: Some(frame_source::monotonic_nanos_to_instant(m.raw_time_ns)),
            });
        }
        win.handle_touch(TouchPoint {
            id: pointer_id,
            x,
            y,
            action: AMOTION_EVENT_ACTION_UP,
            predicted: None,
            timestamp: Some(frame_source::monotonic_nanos_to_instant(time_ns)),
        });
    }

    /// Drops buffered move samples for `pointer_id` and delivers `ACTION_CANCEL`.
    pub(crate) fn on_touch_cancel(
        &mut self,
        win: &AndroidWindow,
        pointer_id: i32,
        time_ns: i64,
        x: f32,
        y: f32,
    ) {
        self.resampler.on_cancel(pointer_id);
        win.handle_touch(TouchPoint {
            id: pointer_id,
            x,
            y,
            action: AMOTION_EVENT_ACTION_CANCEL,
            predicted: None,
            timestamp: Some(frame_source::monotonic_nanos_to_instant(time_ns)),
        });
    }

    /// Flushes resampled touch moves aligned to the current [`VsyncTimeline`].
    fn flush_resampled_touches(&mut self, win: &AndroidWindow) {
        let refresh_ns = frame_source::refresh_interval_ns();
        let timeline = frame_source::last_timeline().unwrap_or_else(|| {
            VsyncTimeline::from_legacy_frame_time(frame_source::monotonic_nanos_now(), refresh_ns)
        });
        let mut moves = Vec::new();
        self.resampler
            .flush_for_timeline(timeline, refresh_ns, |m| moves.push(m));
        for m in moves {
            win.handle_touch(TouchPoint {
                id: m.id,
                x: m.raw_x,
                y: m.raw_y,
                action: AMOTION_EVENT_ACTION_MOVE,
                predicted: m.predicted,
                timestamp: Some(frame_source::monotonic_nanos_to_instant(m.raw_time_ns)),
            });
        }
    }

    /// Updates ADPF target duration if the display refresh interval changed.
    fn sync_adpf_target(&mut self) {
        if let Some(adpf) = &mut self.adpf {
            adpf.update_target_for_refresh(frame_source::refresh_interval_ns());
        }
    }

    /// Renders an immediate frame (e.g. first frame on `InitWindow`) while
    /// keeping LTPO and ADPF metrics updated.
    pub(crate) fn render_immediate_frame(&mut self, win: &AndroidWindow) {
        self.note_activity(win);
        self.sync_adpf_target();
        win.reset_frame_metrics();
        let t_start = frame_source::monotonic_nanos_now();
        win.request_frame();
        let t_end = frame_source::monotonic_nanos_now();
        let (drew, acquire_wait_ns) = win.take_frame_metrics();
        if drew {
            if let Some(adpf) = &self.adpf {
                adpf.report_frame(t_start, t_end, acquire_wait_ns);
            }
        }
    }

    /// Consumes a due vsync frame if available, flushes resampled touches,
    /// ticks GPUI, reports CPU work (minus swapchain acquire wait) to ADPF,
    /// and re-schedules vsync if touch bridging needs the next tick.
    pub(crate) fn render_due_frame(
        &mut self,
        win: &AndroidWindow,
        before_flush: impl FnOnce(&mut Self),
        after_draw: impl FnOnce(&mut Self),
    ) -> bool {
        if !frame_source::take_frame() {
            return false;
        }

        before_flush(self);
        self.note_activity(win);
        self.sync_adpf_target();

        win.reset_frame_metrics();
        let t_start = frame_source::monotonic_nanos_now();
        self.flush_resampled_touches(win);
        win.request_frame();
        let t_end = frame_source::monotonic_nanos_now();

        let (drew, acquire_wait_ns) = win.take_frame_metrics();
        if drew {
            if let Some(adpf) = &self.adpf {
                adpf.report_frame(t_start, t_end, acquire_wait_ns);
            }
        }

        after_draw(self);

        let now_ns = frame_source::monotonic_nanos_now();
        let refresh_ns = frame_source::refresh_interval_ns();
        if self.resampler.needs_next_vsync(now_ns, refresh_ns) {
            self.note_activity(win);
            frame_source::schedule_frame();
        }
        true
    }

    /// Updates dynamic LTPO voting (releasing 120 Hz to `0.0` after 300 ms of
    /// continuous inactivity) and computes the looper poll timeout including
    /// the LTPO idle deadline.
    pub(crate) fn poll_timeout(
        &mut self,
        win: Option<&AndroidWindow>,
        next_delayed_task: Option<Instant>,
        can_draw: bool,
    ) -> Option<Duration> {
        let now = Instant::now();
        let has_demand = can_draw && frame_source::has_demand();
        if let Some(hz) = self.ltpo.poll_idle(now, has_demand) {
            if let Some(win) = win {
                win.set_frame_rate_vote(hz);
            }
        }

        let ltpo_deadline = can_draw
            .then(|| self.ltpo.next_idle_deadline())
            .flatten();
        let combined_deadline = match (next_delayed_task, ltpo_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        frame_source::poll_timeout(combined_deadline, can_draw)
    }
}
