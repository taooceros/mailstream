//! Platform-neutral Android frame timing helpers:
//! - [`VsyncTimeline`] (`AChoreographer` API 33+ frame timeline / legacy fallback)
//! - [`MonotonicClockAnchor`] (`CLOCK_MONOTONIC` ns → `Instant` conversion without
//!   false re-anchoring on future presentation timestamps)
//! - ADPF work-duration and target-budget helpers
//! - [`LtpoVoteController`] (dynamic 120 Hz active / 0.0 Hz idle LTPO voting)

use std::time::{Duration, Instant};

/// Maximum acceptable distance into the future for a `CLOCK_MONOTONIC` timestamp
/// (e.g. API 33+ `expectedPresentationTimeNanos` 1–3 frames ahead, even on 30 Hz / VRR
/// panels) before triggering a clock re-anchor.
pub(crate) const MAX_FUTURE_OFFSET_NS: i128 = 250_000_000;

/// Maximum acceptable distance into the past (historical touch samples / queued events)
/// before triggering a clock re-anchor.
pub(crate) const MAX_PAST_OFFSET_NS: i128 = 500_000_000;

/// Minimum actual work duration reported to ADPF (`APerformanceHint_reportActualWorkDuration`
/// requires a strictly positive value; 100 µs floor guards against zero/negative durations
/// after deducting swapchain acquire wait time).
pub(crate) const ADPF_MIN_ACTUAL_NS: i64 = 100_000;

/// Target ADPF CPU/frame work duration as a percentage of the display refresh interval
/// (85% of 8.33 ms = 7.08 ms at 120 Hz, leaving 15% headroom for compositor hand-off).
const ADPF_TARGET_BUDGET_NUM: i128 = 85;
const ADPF_TARGET_BUDGET_DEN: i128 = 100;

/// Continuous idle window without frame demand before releasing the 120 Hz
/// `ANativeWindow_setFrameRate` vote back to `0.0` so LTPO displays can step down.
pub(crate) const LTPO_IDLE_TIMEOUT: Duration = Duration::from_millis(300);

/// Frame-rate vote while interacting, scrolling, or animating.
pub(crate) const LTPO_ACTIVE_HZ: f32 = 120.0;

/// Frame-rate vote once idle (`0.0` hands control back to SurfaceFlinger / LTPO).
pub(crate) const LTPO_IDLE_HZ: f32 = 0.0;

/// Hardware vsync timeline snapshot delivered by `AChoreographer`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VsyncTimeline {
    /// `CLOCK_MONOTONIC` ns when the frame began (`getFrameTimeNanos` on API 33+,
    /// or the callback's `frameTimeNanos` on API 26–32).
    pub frame_time_ns: i64,
    /// `CLOCK_MONOTONIC` ns when the frame is expected to light up on screen
    /// (`getFrameTimelineExpectedPresentationTimeNanos` on API 33+, or
    /// `frame_time_ns + refresh_interval_ns` on API 26–32).
    pub expected_present_ns: i64,
    /// `CLOCK_MONOTONIC` ns by which the frame must finish to hit
    /// `expected_present_ns` (`getFrameTimelineDeadlineNanos` on API 33+, or
    /// `frame_time_ns + refresh_interval_ns` on API 26–32).
    pub deadline_ns: i64,
}

impl VsyncTimeline {
    /// Constructs a sanitized timeline from API 33+ choreographer values, falling
    /// back to `frame_time_ns + refresh` if a driver reports non-monotonic
    /// presentation or deadline timestamps.
    pub(crate) fn new(
        frame_time_ns: i64,
        expected_present_ns: i64,
        deadline_ns: i64,
        refresh_interval_ns: i64,
    ) -> Self {
        let refresh = refresh_interval_ns.clamp(2_000_000, 100_000_000);
        let fallback_present = frame_time_ns.saturating_add(refresh);
        let expected_present_ns = if expected_present_ns > frame_time_ns {
            expected_present_ns
        } else {
            fallback_present
        };
        let deadline_ns = if deadline_ns > frame_time_ns {
            deadline_ns
        } else {
            fallback_present
        };
        Self {
            frame_time_ns,
            expected_present_ns,
            deadline_ns,
        }
    }

    /// Fallback timeline for API 26–32 callbacks that only report `frame_time_ns`.
    pub(crate) fn from_legacy_frame_time(frame_time_ns: i64, refresh_interval_ns: i64) -> Self {
        let refresh = refresh_interval_ns.clamp(2_000_000, 100_000_000);
        let next = frame_time_ns.saturating_add(refresh);
        Self {
            frame_time_ns,
            expected_present_ns: next,
            deadline_ns: next,
        }
    }

    /// Anti-overshoot touch resampling target for this vsync:
    /// `min(expected_present_ns - refresh_interval_ns, frame_time_ns + min(refresh / 2, 4 ms))`.
    ///
    /// Extrapolating directly to `expected_present_ns` (+16 ms) causes visible
    /// stop-finger overshoot and springback; clamping one refresh behind
    /// presentation and at most `min(refresh / 2, 4 ms)` past `frame_time_ns`
    /// tightens tracking while keeping overshoot strictly bounded.
    pub(crate) fn touch_resample_target_ns(self, refresh_interval_ns: i64) -> i64 {
        let refresh = refresh_interval_ns.clamp(2_000_000, 100_000_000);
        let lead = (refresh / 2).min(4_000_000);
        let frame_cap = self.frame_time_ns.saturating_add(lead);
        if self.expected_present_ns > 0 {
            self.expected_present_ns
                .saturating_sub(refresh)
                .min(frame_cap)
        } else {
            frame_cap
        }
    }
}

/// Infers an updated display refresh interval from consecutive API 33+
/// `AChoreographer` timeline presentation timestamps when the spacing sits in
/// the plausible 50–285 Hz range and differs from `current_ns` by at least
/// 0.5 ms (avoiding ADPF thrash from sub-millisecond jitter, MS-470).
pub(crate) fn infer_refresh_interval_ns(current_ns: i64, timeline_spacing_ns: i64) -> Option<i64> {
    const MIN_SPACING_NS: i64 = 3_500_000; // ~285 Hz
    const MAX_SPACING_NS: i64 = 20_000_000; // 50 Hz
    const HYSTERESIS_NS: i64 = 500_000; // 0.5 ms
    if !(MIN_SPACING_NS..=MAX_SPACING_NS).contains(&timeline_spacing_ns) {
        return None;
    }
    if current_ns.abs_diff(timeline_spacing_ns) < HYSTERESIS_NS as u64 {
        return None;
    }
    Some(timeline_spacing_ns)
}

/// Cached `(Instant, CLOCK_MONOTONIC ns)` anchor so consecutive hardware touch
/// and `AChoreographer` timestamps (including future `expectedPresentationTimeNanos`)
/// map onto the exact same epoch without per-sample syscall jitter or false
/// re-anchoring.
#[derive(Default)]
pub(crate) struct MonotonicClockAnchor {
    anchor: Option<(Instant, i64)>,
    #[cfg(test)]
    reanchor_count: usize,
}
impl MonotonicClockAnchor {
    pub(crate) const fn new() -> Self {
        Self {
            anchor: None,
            #[cfg(test)]
            reanchor_count: 0,
        }
    }

    /// Converts `nanos` (`CLOCK_MONOTONIC` ns) into a `std::time::Instant` using
    /// the cached anchor, only re-anchoring via `sample_now` on initial setup or
    /// genuine clock discontinuity outside `[-MAX_FUTURE_OFFSET_NS, MAX_PAST_OFFSET_NS]`.
    pub(crate) fn to_instant_with(
        &mut self,
        nanos: i64,
        now_instant: Instant,
        mut sample_now: impl FnMut() -> (Instant, i64),
    ) -> Instant {
        if nanos <= 0 {
            return now_instant;
        }
        let (anchor_instant, anchor_ns) = match self.anchor {
            Some(anchor) => anchor,
            None => {
                let (now, now_ns) = sample_now();
                if now_ns <= 0 {
                    return now;
                }
                self.anchor = Some((now, now_ns));
                (now, now_ns)
            }
        };

        let elapsed_ns = now_instant.saturating_duration_since(anchor_instant).as_nanos() as i128;
        let now_ns_est = (anchor_ns as i128) + elapsed_ns;
        let age_ns = now_ns_est - (nanos as i128);
        if !(-MAX_FUTURE_OFFSET_NS..=MAX_PAST_OFFSET_NS).contains(&age_ns) {
            // Re-anchor once in case the process resumed after a long suspend or
            // clock shift, then validate again.
            let (now, now_ns) = sample_now();
            if now_ns <= 0 {
                return now;
            }
            self.anchor = Some((now, now_ns));
            #[cfg(test)]
            {
                self.reanchor_count += 1;
            }
            let delta_ns = (now_ns as i128) - (nanos as i128);
            if (0..=MAX_PAST_OFFSET_NS).contains(&delta_ns) {
                return now
                    .checked_sub(Duration::from_nanos(delta_ns as u64))
                    .unwrap_or(now);
            } else if (-MAX_FUTURE_OFFSET_NS..0).contains(&delta_ns) {
                return now
                    .checked_add(Duration::from_nanos((-delta_ns) as u64))
                    .unwrap_or(now);
            }
            return now;
        }

        let offset_ns = (nanos as i128) - (anchor_ns as i128);
        if offset_ns >= 0 {
            anchor_instant
                .checked_add(Duration::from_nanos(offset_ns as u64))
                .unwrap_or(now_instant)
        } else {
            anchor_instant
                .checked_sub(Duration::from_nanos((-offset_ns) as u64))
                .unwrap_or(now_instant)
        }
    }

    #[cfg(test)]
    pub(crate) fn reanchor_count(&self) -> usize {
        self.reanchor_count
    }
}

/// Computes the ADPF target work duration (`85% * refresh_interval_ns`, e.g.
/// `7.08 ms` at 120 Hz).
pub(crate) fn adpf_target_work_duration_ns(refresh_interval_ns: i64) -> i64 {
    let clamped = refresh_interval_ns.clamp(2_000_000, 100_000_000) as i128;
    ((clamped * ADPF_TARGET_BUDGET_NUM) / ADPF_TARGET_BUDGET_DEN) as i64
}

/// Computes the actual CPU/frame work duration to report to ADPF:
/// `(frame_end_ns - frame_start_ns - acquire_wait_ns).max(100_000)`.
///
/// Deducting `surface.get_current_texture()` fence wait time ensures `Fifo`
/// back-pressure never tricks Android's scheduler into pinning prime cores at
/// max frequency while blocked on SurfaceFlinger.
pub(crate) fn adpf_actual_work_duration_ns(
    frame_start_ns: i64,
    frame_end_ns: i64,
    acquire_wait_ns: i64,
) -> i64 {
    let total = frame_end_ns.saturating_sub(frame_start_ns).max(0);
    let wait = acquire_wait_ns.max(0);
    total.saturating_sub(wait).max(ADPF_MIN_ACTUAL_NS)
}

/// Active vs. idle LTPO frame-rate vote state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LtpoVote {
    High,
    Idle,
}

/// Dynamic LTPO frame-rate vote controller.
///
/// Votes `120.0 Hz` while touch input, scrolling, or animations are active, and
/// drops the vote back to `0.0` after [`LTPO_IDLE_TIMEOUT`] (300 ms) of
/// continuous inactivity so LTPO panels can step down to 1–10 Hz.
#[derive(Default)]
pub(crate) struct LtpoVoteController {
    current: Option<LtpoVote>,
    last_activity: Option<Instant>,
}

impl LtpoVoteController {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Synchronizes state after a new surface has already been initialized at
    /// [`LTPO_ACTIVE_HZ`].
    pub(crate) fn on_surface_attached(&mut self, now: Instant) {
        self.current = Some(LtpoVote::High);
        self.last_activity = Some(now);
    }

    /// Clears state when the native surface is destroyed.
    pub(crate) fn on_surface_destroyed(&mut self) {
        self.current = None;
        self.last_activity = None;
    }

    /// Records interactive/rendering activity. Returns `Some(120.0)` only when
    /// transitioning from idle/unset into [`LtpoVote::High`].
    pub(crate) fn on_activity(&mut self, now: Instant) -> Option<f32> {
        self.last_activity = Some(now);
        if self.current != Some(LtpoVote::High) {
            self.current = Some(LtpoVote::High);
            Some(LTPO_ACTIVE_HZ)
        } else {
            None
        }
    }

    /// Releases the high-refresh vote immediately on activity pause/background.
    pub(crate) fn on_pause(&mut self) -> Option<f32> {
        self.last_activity = None;
        if self.current == Some(LtpoVote::High) {
            self.current = Some(LtpoVote::Idle);
            Some(LTPO_IDLE_HZ)
        } else {
            None
        }
    }

    /// Evaluates whether the idle timeout has elapsed. Returns `Some(120.0)` if
    /// `has_demand` reactivates an idle surface, or `Some(0.0)` once
    /// [`LTPO_IDLE_TIMEOUT`] passes with no demand.
    pub(crate) fn poll_idle(&mut self, now: Instant, has_demand: bool) -> Option<f32> {
        if has_demand {
            return self.on_activity(now);
        }
        if self.current == Some(LtpoVote::High)
            && self
                .last_activity
                .is_some_and(|last| now.saturating_duration_since(last) >= LTPO_IDLE_TIMEOUT)
        {
            self.current = Some(LtpoVote::Idle);
            return Some(LTPO_IDLE_HZ);
        }
        None
    }

    /// Next instant the event loop should wake to release the 120 Hz vote.
    /// Returns `None` once already idle so the loop sleeps with zero extra
    /// wakeups.
    pub(crate) fn next_idle_deadline(&self) -> Option<Instant> {
        if self.current == Some(LtpoVote::High) {
            self.last_activity.map(|last| last + LTPO_IDLE_TIMEOUT)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const REFRESH_120HZ: i64 = 8_333_333;
    const REFRESH_60HZ: i64 = 16_666_667;

    #[test]
    fn future_expected_present_ns_does_not_trigger_false_reanchor() {
        let base_instant = Instant::now();
        let base_ns: i64 = 10_000 * MS;
        let mut anchor = MonotonicClockAnchor::default();

        let mut sample_calls = 0usize;
        let mut sample = |offset_ms: i64| {
            sample_calls += 1;
            (
                base_instant + Duration::from_millis(offset_ms as u64),
                base_ns + offset_ms * MS,
            )
        };

        // Prime the anchor at t = 0.
        let t0 = anchor.to_instant_with(base_ns, base_instant, || sample(0));
        assert_eq!(t0, base_instant);
        assert_eq!(sample_calls, 1);
        assert_eq!(anchor.reanchor_count(), 0);

        // API 33+ expectedPresentationTimeNanos values 1..20 frames in the future
        // (+8.33 ms, +16.67 ms, +33.33 ms, +150 ms, +200 ms) must map cleanly to
        // future Instants without triggering any re-anchor.
        let now_instant = base_instant + Duration::from_millis(10);
        for future_ahead_ns in [
            REFRESH_120HZ,
            2 * REFRESH_120HZ,
            2 * REFRESH_60HZ,
            150 * MS,
            200 * MS,
        ] {
            let target_ns = base_ns + 10 * MS + future_ahead_ns;
            let got = anchor.to_instant_with(target_ns, now_instant, || {
                panic!("future expectedPresentationTimeNanos must not re-anchor");
            });
            let expected = now_instant + Duration::from_nanos(future_ahead_ns as u64);
            assert_eq!(got, expected);
            assert_eq!(anchor.reanchor_count(), 0);
        }

        // Genuine huge discontinuity (> 500 ms drift) still re-anchors once.
        let shifted_now = base_instant + Duration::from_millis(20);
        let shifted_ns = base_ns + 2_000 * MS;
        let got = anchor.to_instant_with(shifted_ns + 16 * MS, shifted_now, || {
            (shifted_now, shifted_ns)
        });
        assert_eq!(got, shifted_now + Duration::from_millis(16));
        assert_eq!(anchor.reanchor_count(), 1);
    }

    #[test]
    fn vsync_timeline_touch_target_clamps_against_overshoot() {
        // 120 Hz with 2-frame presentation latency (+16.67 ms):
        // min(expected - refresh, frame + min(refresh/2, 4ms)) = min(frame + 8.33ms, frame + 4ms)
        let tl_2f = VsyncTimeline::new(100 * MS, 100 * MS + 2 * REFRESH_120HZ, 100 * MS + REFRESH_120HZ, REFRESH_120HZ);
        assert_eq!(tl_2f.touch_resample_target_ns(REFRESH_120HZ), 104 * MS);

        // 120 Hz with 1-frame presentation latency (+8.33 ms):
        // min(frame + 0ms, frame + 4ms) = frame_time_ns
        let tl_1f = VsyncTimeline::new(100 * MS, 100 * MS + REFRESH_120HZ, 100 * MS + REFRESH_120HZ, REFRESH_120HZ);
        assert_eq!(tl_1f.touch_resample_target_ns(REFRESH_120HZ), 100 * MS);

        // 60 Hz with 2-frame presentation latency (+33.33 ms):
        // min(frame + 16.67ms, frame + 4ms) = frame + 4ms
        let tl_60 = VsyncTimeline::new(100 * MS, 100 * MS + 2 * REFRESH_60HZ, 100 * MS + REFRESH_60HZ, REFRESH_60HZ);
        assert_eq!(tl_60.touch_resample_target_ns(REFRESH_60HZ), 104 * MS);
    }

    #[test]
    fn adpf_budget_and_actual_work_deduct_acquire_wait() {
        // 85% of 8.333333 ms = 7.083333 ms.
        assert_eq!(adpf_target_work_duration_ns(REFRESH_120HZ), 7_083_333);
        assert_eq!(adpf_target_work_duration_ns(REFRESH_60HZ), 14_166_666);

        // Frame took 8.0 ms total, 5.2 ms waiting in surface.get_current_texture() -> 2.8 ms CPU work.
        assert_eq!(
            adpf_actual_work_duration_ns(100 * MS, 108 * MS, 5_200_000),
            2_800_000
        );
        // If acquire wait accounts for ~all wall time, clamp at 100 µs floor (never <= 0).
        assert_eq!(
            adpf_actual_work_duration_ns(100 * MS, 105 * MS, 5_100_000),
            ADPF_MIN_ACTUAL_NS
        );
    }

    #[test]
    fn ltpo_controller_releases_to_zero_after_300ms_idle_and_restores_on_activity() {
        let t0 = Instant::now();
        let mut ltpo = LtpoVoteController::new();
        ltpo.on_surface_attached(t0);
        assert_eq!(ltpo.next_idle_deadline(), Some(t0 + LTPO_IDLE_TIMEOUT));

        // Still active before 300 ms.
        assert_eq!(ltpo.poll_idle(t0 + Duration::from_millis(299), false), None);

        // Continuous demand keeps 120 Hz alive and pushes deadline forward.
        let t1 = t0 + Duration::from_millis(250);
        assert_eq!(ltpo.poll_idle(t1, true), None);
        assert_eq!(ltpo.next_idle_deadline(), Some(t1 + LTPO_IDLE_TIMEOUT));

        // 300 ms after last activity with no demand -> vote 0.0 Hz and clear deadline.
        let t_idle = t1 + LTPO_IDLE_TIMEOUT;
        assert_eq!(ltpo.poll_idle(t_idle, false), Some(LTPO_IDLE_HZ));
        assert_eq!(ltpo.next_idle_deadline(), None);
        // Repeated idle polls emit no redundant calls.
        assert_eq!(ltpo.poll_idle(t_idle + Duration::from_secs(1), false), None);

        // Next touch / frame demand restores 120.0 Hz immediately.
        let t_resume = t_idle + Duration::from_secs(2);
        assert_eq!(ltpo.on_activity(t_resume), Some(LTPO_ACTIVE_HZ));
        assert_eq!(ltpo.next_idle_deadline(), Some(t_resume + LTPO_IDLE_TIMEOUT));

        // Pause immediately releases vote to 0.0 Hz.
        assert_eq!(ltpo.on_pause(), Some(LTPO_IDLE_HZ));
        assert_eq!(ltpo.next_idle_deadline(), None);
    }

    #[test]
    fn infer_refresh_interval_tracks_ltpo_transitions_with_hysteresis() {
        // 60 Hz -> 120 Hz transition updates immediately.
        assert_eq!(
            infer_refresh_interval_ns(REFRESH_60HZ, REFRESH_120HZ),
            Some(REFRESH_120HZ)
        );
        // 120 Hz -> 90 Hz transition updates immediately.
        let refresh_90hz = 11_111_111;
        assert_eq!(
            infer_refresh_interval_ns(REFRESH_120HZ, refresh_90hz),
            Some(refresh_90hz)
        );
        // Sub-0.5 ms vsync jitter does not thrash ADPF target updates.
        assert_eq!(
            infer_refresh_interval_ns(REFRESH_120HZ, REFRESH_120HZ + 200_000),
            None
        );
        // Out-of-range spacings are rejected.
        assert_eq!(infer_refresh_interval_ns(REFRESH_60HZ, 1_000_000), None);
        assert_eq!(infer_refresh_interval_ns(REFRESH_60HZ, 50_000_000), None);
    }
}
