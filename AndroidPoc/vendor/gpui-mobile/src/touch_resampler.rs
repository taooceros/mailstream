//! Vsync-aligned touch coalescing and linear resampling for Android.
//!
//! High-refresh Android panels (e.g. 120 Hz / 8.33 ms) frequently receive
//! `ACTION_MOVE` deliveries at a lower or non-integer beat rate (75–108 Hz)
//! while sub-frame samples are batched inside `AMotionEvent` history.
//! Dispatching raw moves asynchronously as they arrive causes beat-frequency
//! stutter (a frame with 0 px displacement followed by a 1.5–2× jump) and
//! collapses sample timestamps when multiple events drain in one loop pass.
//!
//! `TouchResampler` records every historical and current hardware sample with
//! its monotonic nanosecond timestamp, flushes them right before rendering on
//! each vsync, and computes a bounded linear interpolation/extrapolation
//! target for `TouchEvent::predicted_position`. When a 120 Hz vsync fires
//! slightly ahead of the next ~10 ms touch packet, it bridges that single
//! vsync using the latest raw `(x, y, timestamp)` plus the resampled
//! `predicted_position`, keeping velocity estimation clean while advancing
//! scroll smoothly every display refresh.

use std::collections::{HashMap, VecDeque};

/// Minimum interval between two samples before velocity is considered valid
/// (0.5 ms), guarding against duplicate or near-simultaneous samples.
const MIN_DT_NS: i64 = 500_000;

/// Maximum gap between consecutive samples (40 ms, matching GPUI's
/// `VELOCITY_ASSUME_STOPPED_GAP`); beyond this the finger is treated as paused.
const MAX_DT_NS: i64 = 40_000_000;

/// Trailing window over which extrapolation velocity is smoothed (~16 ms, about
/// two 120 Hz frames). Smooths 240–480 Hz digitizer quantization jitter and
/// preserves resampling during slow reading drags without lagging direction
/// reversals.
const VELOCITY_WINDOW_NS: i64 = 16_000_000;

/// Maximum number of recent hardware samples retained per pointer for
/// bracketing interpolation and smoothed extrapolation.
const MAX_HISTORY_SAMPLES: usize = 16;

/// Minimum physical-pixel travel across the velocity window required before
/// extrapolating across a vsync gap, preventing resting-finger jitter.
const MIN_EXTRAP_DISTANCE_PX: f32 = 1.5;

/// If the most recent hardware step spans at least this long with negligible
/// displacement, treat the finger as having come to rest instead of coasting
/// forward on older samples in the window.
const STOP_CHECK_MIN_DT_NS: i64 = 2_000_000;
const STOP_CHECK_MAX_MOVE_PX: f32 = 0.25;

/// One hardware touch sample in physical pixels with its `CLOCK_MONOTONIC`
/// timestamp in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    time_ns: i64,
    x: f32,
    y: f32,
}

/// A move emitted on vsync (or flushed just before release).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResampledMove {
    pub id: i32,
    pub raw_x: f32,
    pub raw_y: f32,
    pub raw_time_ns: i64,
    pub predicted: Option<(f32, f32)>,
}

struct PointerTrack {
    history: VecDeque<Sample>,
    pending: Vec<Sample>,
    last_flushed_vsync_ns: Option<i64>,
}

impl PointerTrack {
    fn new(initial: Sample) -> Self {
        let mut history = VecDeque::with_capacity(MAX_HISTORY_SAMPLES);
        history.push_back(initial);
        Self {
            history,
            pending: Vec::new(),
            last_flushed_vsync_ns: None,
        }
    }

    fn last(&self) -> Sample {
        *self.history.back().expect("PointerTrack history is non-empty")
    }

    fn record_history(&mut self, sample: Sample) {
        let last = self.last();
        if sample.time_ns > last.time_ns {
            self.history.push_back(sample);
            while self.history.len() > MAX_HISTORY_SAMPLES
                || (self.history.len() > 2
                    && sample
                        .time_ns
                        .saturating_sub(self.history.front().unwrap().time_ns)
                        > MAX_DT_NS)
            {
                self.history.pop_front();
            }
        } else if let Some(back) = self.history.back_mut() {
            *back = sample;
        }
    }
}

/// Maximum extrapolation horizon for a given display refresh interval (1.5×
/// refresh: ~12.5 ms at 120 Hz, ~25 ms at 60 Hz). Bridging up to 1.5 frames
/// eliminates `[1x, 0, 2x]` beat-frequency stutter when 90–110 Hz touch packets
/// land slightly after a 120 Hz vsync.
fn max_bridge_horizon_ns(refresh_interval_ns: i64) -> i64 {
    let base = refresh_interval_ns.clamp(4_000_000, 16_666_667);
    base + base / 2
}

/// Per-pointer touch sample history and vsync resampler.
#[derive(Default)]
pub(crate) struct TouchResampler {
    pointers: HashMap<i32, PointerTrack>,
}

impl TouchResampler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record `ACTION_DOWN` / `ACTION_POINTER_DOWN` as the initial hardware sample.
    pub(crate) fn on_down(&mut self, id: i32, time_ns: i64, x: f32, y: f32) {
        self.pointers
            .insert(id, PointerTrack::new(Sample { time_ns, x, y }));
    }

    /// Record one hardware move sample (historical or current) for `id`.
    /// Returns `true` if the sample was accepted (not a duplicate).
    pub(crate) fn on_move(&mut self, id: i32, time_ns: i64, x: f32, y: f32) -> bool {
        let sample = Sample { time_ns, x, y };
        match self.pointers.get_mut(&id) {
            Some(track) => {
                let last = track.last();
                if time_ns <= last.time_ns && x == last.x && y == last.y {
                    return false;
                }
                track.record_history(sample);
                track.pending.push(sample);
                true
            }
            None => {
                let mut track = PointerTrack::new(sample);
                track.pending.push(sample);
                self.pointers.insert(id, track);
                true
            }
        }
    }

    /// Flush buffered move samples on a vsync tick (`vsync_ns`), attaching a
    /// bounded linear resample/prediction to the latest move of each active
    /// pointer and bridging a missed-beat vsync when touch arrives slower than
    /// the display refresh rate.
    pub(crate) fn flush_for_vsync(
        &mut self,
        vsync_ns: i64,
        refresh_interval_ns: i64,
        mut emit: impl FnMut(ResampledMove),
    ) {
        let max_bridge_ns = max_bridge_horizon_ns(refresh_interval_ns);

        let mut ids: Vec<i32> = self.pointers.keys().copied().collect();
        ids.sort_unstable();

        for id in ids {
            let Some(track) = self.pointers.get_mut(&id) else {
                continue;
            };
            let predicted = resample_position(&track.history, vsync_ns, max_bridge_ns);

            if !track.pending.is_empty() {
                let last_ix = track.pending.len() - 1;
                for (ix, sample) in track.pending.drain(..).enumerate() {
                    emit(ResampledMove {
                        id,
                        raw_x: sample.x,
                        raw_y: sample.y,
                        raw_time_ns: sample.time_ns,
                        predicted: if ix == last_ix { predicted } else { None },
                    });
                }
                track.last_flushed_vsync_ns = Some(vsync_ns);
            } else if let Some(pred) = predicted {
                let last = track.last();
                let already_flushed = track.last_flushed_vsync_ns == Some(vsync_ns);
                let age_ns = vsync_ns.saturating_sub(last.time_ns);
                if !already_flushed && age_ns > 0 && age_ns <= max_bridge_ns {
                    emit(ResampledMove {
                        id,
                        raw_x: last.x,
                        raw_y: last.y,
                        raw_time_ns: last.time_ns,
                        predicted: Some(pred),
                    });
                    track.last_flushed_vsync_ns = Some(vsync_ns);
                }
            }
        }
    }

    /// Whether another vsync should be requested so an actively moving pointer
    /// can bridge a frame if the next touch packet arrives slightly after the
    /// next display vsync.
    pub(crate) fn needs_next_vsync(&self, now_ns: i64, refresh_interval_ns: i64) -> bool {
        let max_bridge_ns = max_bridge_horizon_ns(refresh_interval_ns);
        self.pointers.values().any(|track| {
            if !track.pending.is_empty() {
                return true;
            }
            let Some((base, last)) = velocity_window_endpoints(&track.history) else {
                return false;
            };
            if latest_step_stopped(&track.history) {
                return false;
            }
            let dt = last.time_ns.saturating_sub(base.time_ns);
            if !(MIN_DT_NS..=MAX_DT_NS).contains(&dt) {
                return false;
            }
            let moved = (last.x - base.x).hypot(last.y - base.y);
            if moved < MIN_EXTRAP_DISTANCE_PX {
                return false;
            }
            let age = now_ns.saturating_sub(last.time_ns);
            age <= max_bridge_ns
        })
    }

    /// Flush any remaining raw move samples for `id` and remove its tracking
    /// state (called on `ACTION_UP`, `ACTION_POINTER_UP`, or `ACTION_CANCEL`).
    pub(crate) fn on_up_or_cancel(&mut self, id: i32, mut emit_pending: impl FnMut(ResampledMove)) {
        if let Some(mut track) = self.pointers.remove(&id) {
            for sample in track.pending.drain(..) {
                emit_pending(ResampledMove {
                    id,
                    raw_x: sample.x,
                    raw_y: sample.y,
                    raw_time_ns: sample.time_ns,
                    predicted: None,
                });
            }
        }
    }

    /// Drop all buffered samples for `id` without emitting them (used on
    /// `ACTION_CANCEL`, e.g. when Android's system back gesture pilfers a touch).
    pub(crate) fn on_cancel(&mut self, id: i32) {
        self.pointers.remove(&id);
    }

    /// Clear all tracked pointers (called on `ACTION_DOWN`, which begins a fresh
    /// Android touch stream with no prior active pointers).
    pub(crate) fn clear(&mut self) {
        self.pointers.clear();
    }
}

fn velocity_window_endpoints(history: &VecDeque<Sample>) -> Option<(Sample, Sample)> {
    if history.len() < 2 {
        return None;
    }
    let last = *history.back()?;
    let mut base = None;
    let mut prev_time_ns = last.time_ns;
    for &sample in history.iter().rev().skip(1) {
        if prev_time_ns.saturating_sub(sample.time_ns) > MAX_DT_NS {
            break;
        }
        if last.time_ns.saturating_sub(sample.time_ns) > VELOCITY_WINDOW_NS && base.is_some() {
            break;
        }
        base = Some(sample);
        prev_time_ns = sample.time_ns;
        if last.time_ns.saturating_sub(sample.time_ns) >= VELOCITY_WINDOW_NS {
            break;
        }
    }
    Some((base?, last))
}

fn latest_step_stopped(history: &VecDeque<Sample>) -> bool {
    if history.len() < 2 {
        return false;
    }
    let last = history[history.len() - 1];
    let prev = history[history.len() - 2];
    let dt = last.time_ns.saturating_sub(prev.time_ns);
    dt >= STOP_CHECK_MIN_DT_NS && (last.x - prev.x).hypot(last.y - prev.y) < STOP_CHECK_MAX_MOVE_PX
}

fn resample_position(
    history: &VecDeque<Sample>,
    target_ns: i64,
    max_bridge_ns: i64,
) -> Option<(f32, f32)> {
    if history.len() < 2 {
        return None;
    }
    let last = *history.back()?;
    let first = *history.front()?;

    // Case 1: `target_ns` lies within or just before our hardware history —
    // interpolate directly between the bracketing samples without extrapolating.
    if target_ns <= last.time_ns {
        if target_ns < first.time_ns {
            return Some((first.x, first.y));
        }
        for i in (1..history.len()).rev() {
            let s0 = history[i - 1];
            let s1 = history[i];
            if s0.time_ns <= target_ns && target_ns <= s1.time_ns {
                let dt = s1.time_ns.saturating_sub(s0.time_ns);
                if (MIN_DT_NS..=MAX_DT_NS).contains(&dt) {
                    let alpha = (target_ns - s0.time_ns) as f64 / dt as f64;
                    return Some((
                        s0.x + ((s1.x - s0.x) as f64 * alpha) as f32,
                        s0.y + ((s1.y - s0.y) as f64 * alpha) as f32,
                    ));
                }
                return Some((s1.x, s1.y));
            }
        }
        return None;
    }

    // Case 2: `target_ns` is ahead of the latest sample — extrapolate along the
    // trailing ~16 ms window, guarded against a stopped or reversing finger.
    let ahead = target_ns - last.time_ns;
    if ahead > max_bridge_ns * 2 || latest_step_stopped(history) {
        return None;
    }
    let (mut base, last) = velocity_window_endpoints(history)?;
    let prev = history[history.len() - 2];
    let step_dx = last.x - prev.x;
    let step_dy = last.y - prev.y;
    let win_dx = last.x - base.x;
    let win_dy = last.y - base.y;
    // If the finger just reversed direction, drop older pre-reversal samples so
    // prediction never fights the new direction.
    if step_dx * win_dx + step_dy * win_dy < 0.0 {
        base = prev;
    }
    let dt = last.time_ns.saturating_sub(base.time_ns);
    if !(MIN_DT_NS..=MAX_DT_NS).contains(&dt) {
        return None;
    }
    let dx = last.x - base.x;
    let dy = last.y - base.y;
    if dx.hypot(dy) < MIN_EXTRAP_DISTANCE_PX {
        return None;
    }
    let extrap = ahead.min(max_bridge_ns).min(dt);
    let alpha = extrap as f64 / dt as f64;
    Some((
        last.x + (dx as f64 * alpha) as f32,
        last.y + (dy as f64 * alpha) as f32,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const REFRESH_120HZ: i64 = 8_333_333;

    #[test]
    fn bridges_90hz_touch_across_120hz_vsyncs_with_uniform_steps() {
        let mut r = TouchResampler::new();
        // Constant 10 px/ms vertical drag, delivered every 11 ms (~91 Hz).
        r.on_down(0, 0, 100.0, 1000.0);
        r.on_move(0, 11 * MS, 100.0, 890.0);

        let mut out = Vec::new();
        // Vsync 1 at 12 ms: raw at 11 ms, extrapolated +1 ms to 880.0.
        r.flush_for_vsync(12 * MS, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw_y, 890.0);
        assert!((out[0].predicted.unwrap().1 - 880.0).abs() < 0.01);
        assert!(r.needs_next_vsync(12 * MS, REFRESH_120HZ));

        // Vsync 2 at 19.3 ms: next touch (at 22 ms) hasn't arrived yet!
        // Resampler bridges this single vsync cleanly without changing raw sample.
        out.clear();
        r.flush_for_vsync(19_300_000, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw_time_ns, 11 * MS);
        assert_eq!(out[0].raw_y, 890.0);
        assert!((out[0].predicted.unwrap().1 - 807.0).abs() < 0.2);

        // If no touch arrives by 28 ms (> max_extrap_ns), stops scheduling.
        assert!(!r.needs_next_vsync(28 * MS, REFRESH_120HZ));
    }

    #[test]
    fn flushes_batched_history_with_prediction_only_on_latest_sample() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 4 * MS, 50.0, 480.0);
        r.on_move(0, 8 * MS, 50.0, 460.0);

        let mut out = Vec::new();
        r.flush_for_vsync(10 * MS, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].raw_time_ns, 4 * MS);
        assert_eq!(out[0].predicted, None);
        assert_eq!(out[1].raw_time_ns, 8 * MS);
        assert!((out[1].predicted.unwrap().1 - 450.0).abs() < 0.01);
    }

    #[test]
    fn stationary_finger_does_not_extrapolate_or_keep_vsync_alive() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 8 * MS, 50.5, 500.5);

        let mut out = Vec::new();
        r.flush_for_vsync(10 * MS, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].predicted, None);
        assert!(!r.needs_next_vsync(10 * MS, REFRESH_120HZ));
    }

    #[test]
    fn release_flushes_pending_samples_before_removing_pointer() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 5 * MS, 50.0, 450.0);
        let mut out = Vec::new();
        r.on_up_or_cancel(0, |m| out.push(m));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw_y, 450.0);
        assert_eq!(out[0].predicted, None);
        assert!(!r.needs_next_vsync(6 * MS, REFRESH_120HZ));
    }

    #[test]
    fn cancel_drops_pending_samples_without_emitting_moves() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 5 * MS, 80.0, 500.0);
        r.on_cancel(0);
        let mut out = Vec::new();
        r.flush_for_vsync(10 * MS, REFRESH_120HZ, |m| out.push(m));
        assert!(out.is_empty());
        assert!(!r.needs_next_vsync(10 * MS, REFRESH_120HZ));
    }

    #[test]
    fn bridges_90hz_touch_with_gap_over_one_refresh_without_zero_frame() {
        let mut r = TouchResampler::new();
        // 90 Hz (~11.11 ms) touch moving at 3000 px/s (3 px/ms).
        r.on_down(0, 0, 100.0, 1000.0);
        r.on_move(0, 11_111_111, 100.0, 966.6667);
        r.on_move(0, 22_222_222, 100.0, 933.3333);

        let mut out = Vec::new();
        // Vsync at 25.0 ms flushes pending moves.
        r.flush_for_vsync(25_000_000, REFRESH_120HZ, |m| out.push(m));
        assert!(!out.is_empty());
        let y1 = out.last().unwrap().predicted.unwrap().1;
        assert!((y1 - 925.0).abs() < 0.2, "expected ~925.0 at 25 ms, got {y1}");

        // Next 120 Hz vsync at 33.33 ms: next 90 Hz packet is still 0 ms away
        // (age = 11.11 ms > 8.33 ms refresh, within 1.5x horizon = 12.5 ms).
        // Must still emit a bridged move instead of a 0 px stall frame.
        out.clear();
        r.flush_for_vsync(33_333_333, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.len(), 1, "must bridge 11.1 ms gap without dropping frame");
        let y2 = out[0].predicted.unwrap().1;
        assert!(
            (y2 - 900.0).abs() < 0.5,
            "expected uniform step to ~900.0 at 33.33 ms, got {y2}"
        );
    }

    #[test]
    fn slow_high_rate_drag_resamples_across_trailing_window() {
        let mut r = TouchResampler::new();
        // 240 Hz (4.17 ms) slow reading drag at 300 px/s (1.25 px per sample,
        // below the 1.5 px threshold for a single step, but 5 px over 16.7 ms).
        r.on_down(0, 0, 100.0, 500.0);
        for step in 1..=4 {
            let t = step * 4_166_667;
            let y = 500.0 - step as f32 * 1.25;
            r.on_move(0, t, 100.0, y);
        }
        let mut out = Vec::new();
        // Vsync 3.33 ms after the latest sample (at 20.0 ms): should extrapolate
        // smoothly by 1.0 px to 494.0 instead of returning None.
        r.flush_for_vsync(20_000_000, REFRESH_120HZ, |m| out.push(m));
        let pred = out.last().unwrap().predicted.expect("slow high-rate drag must resample");
        assert!(
            (pred.1 - 494.0).abs() < 0.15,
            "expected ~494.0 at 20 ms, got {}",
            pred.1
        );
    }

    #[test]
    fn interpolates_when_samples_bracket_vsync() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 6 * MS, 50.0, 440.0);
        r.on_move(0, 10 * MS, 50.0, 400.0);

        let mut out = Vec::new();
        // Vsync timestamp at 8 ms sits between 6 ms and 10 ms samples.
        r.flush_for_vsync(8 * MS, REFRESH_120HZ, |m| out.push(m));
        let pred = out.last().unwrap().predicted.expect("must interpolate");
        assert!((pred.1 - 420.0).abs() < 0.05, "expected 420.0, got {}", pred.1);
    }

    #[test]
    fn stopped_finger_does_not_extrapolate_from_older_window_motion() {
        let mut r = TouchResampler::new();
        r.on_down(0, 0, 50.0, 500.0);
        r.on_move(0, 4 * MS, 50.0, 460.0);
        r.on_move(0, 8 * MS, 50.0, 420.0);
        // Finger comes to a stop: next sample 4 ms later has negligible travel.
        r.on_move(0, 12 * MS, 50.0, 419.9);

        let mut out = Vec::new();
        r.flush_for_vsync(15 * MS, REFRESH_120HZ, |m| out.push(m));
        assert_eq!(out.last().unwrap().predicted, None);
        assert!(!r.needs_next_vsync(15 * MS, REFRESH_120HZ));
    }
}
