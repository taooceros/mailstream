// Mailstream patch (MS-449): Android 12+ Stretch Overscroll physics and state.
//
// Overscroll never modifies `scroll_offset` (which remains strictly clamped to
// `[0, max_offset]`) and never mutates layout or `LayerContext`, so Scroll
// Layers keep hitting `Decision::Composite` with `tiles_dirtied == 0`.
// Instead, the normalized stretch `(s_x, s_y)` in `[-0.06, 0.06]` is passed to
// `Scene::overscroll_stretch` and applied as an affine vertex scale anchored at
// `viewport.top`/`viewport.bottom` inside the GPU vertex shader.

use std::time::Instant;

use crate::{Pixels, Point, Size, point, px};

/// Maximum visual stretch factor (6%, matching Android 12+ Stretch EdgeEffect).
pub(crate) const MAX_STRETCH: f32 = 0.06;

/// Decay length as a fraction of viewport extent in the rubber-band curve:
/// `s = sign(d) * MAX_STRETCH * (1 - exp(-|d| / (0.65 * H)))`.
const RUBBER_BAND_FACTOR: f32 = 0.65;

/// Critically damped spring natural frequency (`omega_n = 26.0 rad/s, zeta = 1.0`).
const SPRING_OMEGA: f32 = 26.0;

/// Fraction of remaining inertial fling velocity injected into the overscroll
/// spring when momentum hits a scroll boundary.
const FLING_ABSORB_RATIO: f32 = 0.35;

/// Snap-to-rest thresholds for the critically damped return spring.
const SETTLE_STRETCH_EPS: f32 = 1e-4;
const SETTLE_VELOCITY_EPS: f32 = 1e-3;

/// Maximum per-frame `dt` fed to the spring step so a paused or stalled frame
/// never causes a huge jump.
const MAX_STEP_DT: f32 = 0.1;

/// Maps an accumulated over-pull distance `pull` (logical px) over a container
/// of extent `extent` (logical px) to a normalized stretch in
/// `[-MAX_STRETCH, MAX_STRETCH]`.
#[inline]
pub(crate) fn rubber_band_stretch(pull: f32, extent: f32) -> f32 {
    if pull == 0.0 || !pull.is_finite() {
        return 0.0;
    }
    let denom = (RUBBER_BAND_FACTOR * extent.max(1.0)).max(1.0);
    pull.signum() * MAX_STRETCH * (1.0 - (-pull.abs() / denom).exp())
}

/// Steps a critically damped harmonic oscillator (`zeta = 1.0`, natural
/// frequency `omega = SPRING_OMEGA`, equilibrium `0`) forward by `dt` seconds
/// using the exact analytical solution:
///
///   x(dt) = exp(-w*dt) * (x0 + (v0 + w*x0) * dt)
///   v(dt) = exp(-w*dt) * (v0 - w * (v0 + w*x0) * dt)
#[inline]
fn step_critically_damped(x: &mut f32, v: &mut f32, dt: f32) -> bool {
    if *x == 0.0 && *v == 0.0 {
        return false;
    }
    if dt > 0.0 {
        let w = SPRING_OMEGA;
        let decay = (-w * dt).exp();
        let b = *v + w * *x;
        let next_x = (decay * (*x + b * dt)).clamp(-MAX_STRETCH, MAX_STRETCH);
        let next_v = decay * (*v - w * b * dt);
        *x = next_x;
        *v = next_v;
    }
    if x.abs() < SETTLE_STRETCH_EPS && v.abs() < SETTLE_VELOCITY_EPS {
        *x = 0.0;
        *v = 0.0;
        false
    } else {
        true
    }
}

/// Per-container overscroll state embedded in `ScrollHandleState` and `ListState`.
#[derive(Default, Debug, Clone)]
pub(crate) struct OverscrollState {
    /// Current normalized stretch `(s_x, s_y)` in `[-0.06, 0.06]`.
    /// Positive `s_y` = pulled down past top (anchor at `viewport.top`);
    /// negative `s_y` = pulled up past bottom (anchor at `viewport.bottom`).
    pub(crate) stretch: Point<f32>,
    /// Accumulated over-pull distance in logical pixels while finger is down.
    pull: Point<f32>,
    /// Spring velocity in stretch units per second.
    velocity: Point<f32>,
    /// Whether the user is actively dragging past the edge.
    dragging: bool,
    /// Last frame signal instant used to compute `dt` for the spring.
    last_step: Option<Instant>,
    /// Whether a next-frame spring tick callback is already scheduled.
    pub(crate) tick_scheduled: bool,
}

impl OverscrollState {
    /// Current normalized stretch `(s_x, s_y)`.
    #[inline]
    pub(crate) fn stretch(&self) -> Point<f32> {
        self.stretch
    }

    /// Whether the container currently has a non-zero visual stretch.
    #[inline]
    pub(crate) fn is_stretched(&self) -> bool {
        self.stretch.x != 0.0 || self.stretch.y != 0.0
    }

    /// Whether the spring still needs to animate (either stretched or moving).
    #[inline]
    pub(crate) fn is_animating(&self) -> bool {
        !self.dragging
            && (self.stretch.x != 0.0
                || self.stretch.y != 0.0
                || self.velocity.x != 0.0
                || self.velocity.y != 0.0)
    }

    /// Resets overscroll immediately (e.g., on programmatic scroll or list reset).
    pub(crate) fn reset(&mut self) {
        self.stretch = Point::default();
        self.pull = Point::default();
        self.velocity = Point::default();
        self.dragging = false;
        self.last_step = None;
    }

    /// Processes a single axis of an active finger drag delta (`delta_px > 0`
    /// scrolls content down / pulls down from top; `delta_px < 0` scrolls
    /// content up / pulls up from bottom).
    ///
    /// Returns `(remaining_delta_px, overscroll_changed)`.
    pub(crate) fn drag_axis(
        stretch: &mut f32,
        pull: &mut f32,
        velocity: &mut f32,
        delta_px: f32,
        at_start: bool,
        at_end: bool,
        extent_px: f32,
    ) -> (f32, bool) {
        if delta_px == 0.0 || extent_px <= 0.0 {
            return (delta_px, false);
        }
        let mut remaining = delta_px;
        let old_stretch = *stretch;

        // First unwind any existing over-pull when dragging back inward.
        if *pull > 0.0 && remaining < 0.0 {
            let unwind = (-remaining).min(*pull);
            *pull -= unwind;
            remaining += unwind;
            *velocity = 0.0;
            *stretch = rubber_band_stretch(*pull, extent_px);
        } else if *pull < 0.0 && remaining > 0.0 {
            let unwind = remaining.min(-*pull);
            *pull += unwind;
            remaining -= unwind;
            *velocity = 0.0;
            *stretch = rubber_band_stretch(*pull, extent_px);
        }

        // Absorb further boundary over-pull into the rubber-band curve.
        if (at_start && remaining > 0.0) || (at_end && remaining < 0.0) {
            *pull += remaining;
            *velocity = 0.0;
            *stretch = rubber_band_stretch(*pull, extent_px);
            remaining = 0.0;
        }

        (remaining, (*stretch - old_stretch).abs() > f32::EPSILON)
    }

    /// Consumes boundary over-pull from an active touch drag along X and/or Y.
    ///
    /// `can_scroll` specifies which axes are scrollable; `at_start`/`at_end`
    /// report whether `scroll_offset` is currently at the start (`0`) or end
    /// (`-max`) of each axis.
    ///
    /// Returns `(remaining_delta, overscroll_changed)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn drag(
        &mut self,
        delta: Point<Pixels>,
        can_scroll_x: bool,
        can_scroll_y: bool,
        at_start_x: bool,
        at_end_x: bool,
        at_start_y: bool,
        at_end_y: bool,
        viewport: Size<Pixels>,
    ) -> (Point<Pixels>, bool) {
        let mut rem_x = f32::from(delta.x);
        let mut rem_y = f32::from(delta.y);
        let mut changed = false;

        if can_scroll_x {
            let (rx, cx) = Self::drag_axis(
                &mut self.stretch.x,
                &mut self.pull.x,
                &mut self.velocity.x,
                rem_x,
                at_start_x,
                at_end_x,
                f32::from(viewport.width),
            );
            rem_x = rx;
            changed |= cx;
        }
        if can_scroll_y {
            let (ry, cy) = Self::drag_axis(
                &mut self.stretch.y,
                &mut self.pull.y,
                &mut self.velocity.y,
                rem_y,
                at_start_y,
                at_end_y,
                f32::from(viewport.height),
            );
            rem_y = ry;
            changed |= cy;
        }
        if self.is_stretched() {
            self.dragging = true;
            self.last_step = None;
        } else if changed {
            self.dragging = false;
            self.last_step = None;
        }
        (point(px(rem_x), px(rem_y)), changed)
    }

    /// Releases an active drag so the critically damped spring rebounds to 0.
    /// Returns `true` if a spring animation should now run.
    pub(crate) fn release(&mut self, now: Instant) -> bool {
        let was_dragging = self.dragging;
        self.dragging = false;
        self.pull = Point::default();
        if self.is_stretched() || self.velocity.x != 0.0 || self.velocity.y != 0.0 {
            if was_dragging || self.last_step.is_none() {
                self.last_step = Some(now);
            }
            true
        } else {
            self.last_step = None;
            false
        }
    }

    /// Absorbs remaining inertial fling velocity `v_rem` (logical px/s) on a
    /// boundary impact by injecting `0.35 * v_rem / extent` into the critically
    /// damped stretch spring.
    ///
    /// Returns `true` if velocity was absorbed and the spring should tick.
    pub(crate) fn absorb_fling(
        &mut self,
        unused_x: bool,
        unused_y: bool,
        velocity_px_per_sec: Point<f32>,
        viewport: Size<Pixels>,
        now: Instant,
    ) -> bool {
        let mut absorbed = false;
        if unused_x && velocity_px_per_sec.x.abs() > 1.0 {
            let w = f32::from(viewport.width).max(1.0);
            let impulse = (FLING_ABSORB_RATIO * velocity_px_per_sec.x) / w;
            self.velocity.x += impulse;
            absorbed = true;
        }
        if unused_y && velocity_px_per_sec.y.abs() > 1.0 {
            let h = f32::from(viewport.height).max(1.0);
            let impulse = (FLING_ABSORB_RATIO * velocity_px_per_sec.y) / h;
            self.velocity.y += impulse;
            absorbed = true;
        }
        if absorbed {
            self.dragging = false;
            self.pull = Point::default();
            self.last_step = Some(now);
        }
        absorbed
    }

    /// Advances the critically damped return spring to `now` (the window's
    /// `frame_signal_at` timestamp). Returns `true` while still animating.
    pub(crate) fn step(&mut self, now: Instant) -> bool {
        if self.dragging {
            self.last_step = Some(now);
            return false;
        }
        let dt = match self.last_step {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(MAX_STEP_DT),
            None => 0.0,
        };
        self.last_step = Some(now);
        let active_x = step_critically_damped(&mut self.stretch.x, &mut self.velocity.x, dt);
        let active_y = step_critically_damped(&mut self.stretch.y, &mut self.velocity.y, dt);
        let active = active_x || active_y;
        if !active {
            self.last_step = None;
            self.pull = Point::default();
        }
        active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rubber_band_matches_android_curve_and_bounds() {
        let h = 800.0;
        assert_eq!(rubber_band_stretch(0.0, h), 0.0);
        let s1 = rubber_band_stretch(0.65 * h, h);
        let expected = MAX_STRETCH * (1.0 - (-1.0_f32).exp());
        assert!((s1 - expected).abs() < 1e-6);
        let s_neg = rubber_band_stretch(-0.65 * h, h);
        assert!((s_neg + expected).abs() < 1e-6);
        // Never exceeds 6% even on huge pull.
        assert!(rubber_band_stretch(100_000.0, h) <= MAX_STRETCH);
        assert!(rubber_band_stretch(-100_000.0, h) >= -MAX_STRETCH);
    }

    #[test]
    fn critically_damped_spring_rebounds_monotonically_without_overshoot() {
        let mut state = OverscrollState::default();
        let viewport = Size {
            width: px(400.0),
            height: px(800.0),
        };
        let (rem, changed) = state.drag(
            point(px(0.0), px(300.0)),
            false,
            true,
            false,
            false,
            true,
            false,
            viewport,
        );
        assert_eq!(rem, point(px(0.0), px(0.0)));
        assert!(changed);
        let initial = state.stretch().y;
        assert!(initial > 0.02 && initial <= MAX_STRETCH);

        let t0 = Instant::now();
        assert!(state.release(t0));
        let mut prev = initial;
        let mut t = t0;
        let mut frames = 0;
        while state.step(t) {
            t += Duration::from_millis(8); // ~120 Hz
            let cur = state.stretch().y;
            assert!(cur >= 0.0, "critically damped release must not overshoot below 0");
            assert!(cur <= prev + 1e-6, "release must decay monotonically");
            prev = cur;
            frames += 1;
            assert!(frames < 120, "spring should settle well within 1 second");
        }
        assert_eq!(state.stretch().y, 0.0);
    }

    #[test]
    fn fling_impact_absorbs_velocity_peaks_and_settles() {
        let mut state = OverscrollState::default();
        let viewport = Size {
            width: px(400.0),
            height: px(800.0),
        };
        let t0 = Instant::now();
        assert!(state.absorb_fling(false, true, point(0.0, 4000.0), viewport, t0));
        let mut peak = 0.0_f32;
        let mut t = t0;
        for _ in 0..80 {
            t += Duration::from_millis(8);
            let running = state.step(t);
            peak = peak.max(state.stretch().y);
            if !running {
                break;
            }
        }
        assert!(peak > 0.015 && peak <= MAX_STRETCH, "peak={peak}");
        assert_eq!(state.stretch().y, 0.0);
    }

    #[test]
    fn inward_drag_unwinds_overpull_before_resuming_scroll() {
        let mut state = OverscrollState::default();
        let viewport = Size {
            width: px(400.0),
            height: px(800.0),
        };
        let (rem, _) = state.drag(
            point(px(0.0), px(100.0)),
            false,
            true,
            false,
            false,
            true,
            false,
            viewport,
        );
        assert_eq!(rem.y, px(0.0));
        assert!(state.stretch().y > 0.0);

        // Dragging back 40px unwinds pull from 100 -> 60; no scroll emitted yet.
        let (rem, changed) = state.drag(
            point(px(0.0), px(-40.0)),
            false,
            true,
            false,
            false,
            true,
            false,
            viewport,
        );
        assert!(changed);
        assert_eq!(rem.y, px(0.0));
        assert!(state.stretch().y > 0.0);

        // Dragging back 100px unwinds remaining 60px and passes -40px to scroll.
        let (rem, changed) = state.drag(
            point(px(0.0), px(-100.0)),
            false,
            true,
            false,
            false,
            true,
            false,
            viewport,
        );
        assert!(changed);
        assert_eq!(rem.y, px(-40.0));
        assert_eq!(state.stretch().y, 0.0);
    }
}
