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

use std::collections::HashMap;

/// Minimum interval between two samples before velocity is considered valid
/// (0.5 ms), guarding against duplicate or near-simultaneous samples.
const MIN_DT_NS: i64 = 500_000;

/// Maximum gap between consecutive samples (40 ms, matching GPUI's
/// `VELOCITY_ASSUME_STOPPED_GAP`); beyond this the finger is treated as paused.
const MAX_DT_NS: i64 = 40_000_000;

/// Minimum physical-pixel travel between the last two samples required before
/// extrapolating across a vsync gap, preventing resting-finger jitter.
const MIN_EXTRAP_DISTANCE_PX: f32 = 1.5;

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
    prev: Option<Sample>,
    last: Sample,
    pending: Vec<Sample>,
    last_flushed_vsync_ns: Option<i64>,
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
        self.pointers.insert(
            id,
            PointerTrack {
                prev: None,
                last: Sample { time_ns, x, y },
                pending: Vec::new(),
                last_flushed_vsync_ns: None,
            },
        );
    }

    /// Record one hardware move sample (historical or current) for `id`.
    /// Returns `true` if the sample was accepted (not a duplicate).
    pub(crate) fn on_move(&mut self, id: i32, time_ns: i64, x: f32, y: f32) -> bool {
        let sample = Sample { time_ns, x, y };
        match self.pointers.get_mut(&id) {
            Some(track) => {
                if time_ns <= track.last.time_ns && x == track.last.x && y == track.last.y {
                    return false;
                }
                if time_ns > track.last.time_ns {
                    track.prev = Some(track.last);
                    track.last = sample;
                } else {
                    track.last = sample;
                }
                track.pending.push(sample);
                true
            }
            None => {
                self.pointers.insert(
                    id,
                    PointerTrack {
                        prev: None,
                        last: sample,
                        pending: vec![sample],
                        last_flushed_vsync_ns: None,
                    },
                );
                true
            }
        }
    }

    /// Flush buffered move samples on a vsync tick (`vsync_ns`), attaching a
    /// bounded linear resample/prediction to the latest move of each active
    /// pointer and bridging at most one missed-beat vsync when touch arrives
    /// slower than the display refresh rate.
    pub(crate) fn flush_for_vsync(
        &mut self,
        vsync_ns: i64,
        refresh_interval_ns: i64,
        mut emit: impl FnMut(ResampledMove),
    ) {
        let max_extrap_ns = refresh_interval_ns.clamp(4_000_000, 16_666_667);

        let mut ids: Vec<i32> = self.pointers.keys().copied().collect();
        ids.sort_unstable();

        for id in ids {
            let Some(track) = self.pointers.get_mut(&id) else {
                continue;
            };
            let predicted = resample_position(track.prev, track.last, vsync_ns, max_extrap_ns);

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
                let already_flushed = track.last_flushed_vsync_ns == Some(vsync_ns);
                let age_ns = vsync_ns.saturating_sub(track.last.time_ns);
                if !already_flushed && age_ns > 0 && age_ns <= max_extrap_ns {
                    emit(ResampledMove {
                        id,
                        raw_x: track.last.x,
                        raw_y: track.last.y,
                        raw_time_ns: track.last.time_ns,
                        predicted: Some(pred),
                    });
                    track.last_flushed_vsync_ns = Some(vsync_ns);
                }
            }
        }
    }

    /// Whether another vsync should be requested so an actively moving pointer
    /// can bridge a single frame if the next touch packet arrives slightly
    /// after the next display vsync.
    pub(crate) fn needs_next_vsync(&self, now_ns: i64, refresh_interval_ns: i64) -> bool {
        let max_extrap_ns = refresh_interval_ns.clamp(4_000_000, 16_666_667);
        self.pointers.values().any(|track| {
            if !track.pending.is_empty() {
                return true;
            }
            let Some(prev) = track.prev else {
                return false;
            };
            let dt = track.last.time_ns.saturating_sub(prev.time_ns);
            if !(MIN_DT_NS..=MAX_DT_NS).contains(&dt) {
                return false;
            }
            let moved = (track.last.x - prev.x).hypot(track.last.y - prev.y);
            if moved < MIN_EXTRAP_DISTANCE_PX {
                return false;
            }
            let age = now_ns.saturating_sub(track.last.time_ns);
            age <= max_extrap_ns
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
}

fn resample_position(
    prev: Option<Sample>,
    last: Sample,
    target_ns: i64,
    max_extrap_ns: i64,
) -> Option<(f32, f32)> {
    let prev = prev?;
    let dt = last.time_ns.saturating_sub(prev.time_ns);
    if !(MIN_DT_NS..=MAX_DT_NS).contains(&dt) {
        return None;
    }
    let dx = last.x - prev.x;
    let dy = last.y - prev.y;
    if dx.hypot(dy) < MIN_EXTRAP_DISTANCE_PX {
        return None;
    }
    if target_ns >= prev.time_ns && target_ns <= last.time_ns {
        let alpha = (target_ns - prev.time_ns) as f64 / dt as f64;
        return Some((
            prev.x + (dx as f64 * alpha) as f32,
            prev.y + (dy as f64 * alpha) as f32,
        ));
    }
    if target_ns > last.time_ns {
        let ahead = target_ns - last.time_ns;
        if ahead > max_extrap_ns * 2 {
            return None;
        }
        let extrap = ahead.min(max_extrap_ns).min(dt);
        let alpha = extrap as f64 / dt as f64;
        return Some((
            last.x + (dx as f64 * alpha) as f32,
            last.y + (dy as f64 * alpha) as f32,
        ));
    }
    None
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
}
