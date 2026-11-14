//! Mailstream patch (MS-225): smooth mouse-wheel steps on macOS.
//!
//! AppKit reports a notch of a plain mouse wheel (`hasPreciseScrollingDeltas` false) as a step
//! of lines, and upstream applies it at once, so the list jumps about a row in one frame while
//! Finder and Safari slide there over a few frames. The window feeds each step here and delivers
//! a share of what is left on every frame: an ease-out whose time constant matches the Linux
//! patch (MS-208), so 95% of a step lands within 120 ms and the first share goes out with the
//! notch itself. Touchpads and Magic Mice send pixel deltas with AppKit's own momentum and don't
//! come here.
//!
//! Free of GPUI types so the tests run standalone: `rustc --edition 2024 --test
//! src/wheel_motion.rs -o /tmp/wheel_motion && /tmp/wheel_motion` (cargo won't run tests of a
//! patched dependency).

use std::time::{Duration, Instant};

/// The step one frame takes when no frame came before it (120 Hz).
const FRAME: Duration = Duration::from_micros(8_333);
/// Ease-out time constant: 95% of a step lands within three of these (120 ms).
const TAU: f32 = 0.04;
/// Below this many lines left (about a pixel), a step finishes.
const DONE: f32 = 0.05;
/// The longest step one frame takes, so a stalled frame doesn't jump.
const MAX_DT: f32 = 0.1;

#[derive(Default)]
pub(crate) struct WheelMotion {
    /// Lines not yet delivered, (x, y).
    left: (f32, f32),
    last_tick: Option<Instant>,
}

impl WheelMotion {
    /// A wheel step of `lines`. A step against the motion on an axis drops what was left there,
    /// so turning the wheel back reverses at once.
    pub fn wheel(&mut self, lines: (f32, f32)) {
        fn add(left: f32, step: f32) -> f32 {
            if left * step < 0.0 { step } else { left + step }
        }
        self.left = (add(self.left.0, lines.0), add(self.left.1, lines.1));
    }

    /// Drops what is left (a click, touchpad scrolling).
    pub fn stop(&mut self) {
        self.left = (0.0, 0.0);
        self.last_tick = None;
    }

    pub fn is_active(&self) -> bool {
        self.left != (0.0, 0.0)
    }

    /// The lines due by `now`, or `None` when nothing is left.
    pub fn tick(&mut self, now: Instant) -> Option<(f32, f32)> {
        if !self.is_active() {
            self.last_tick = None;
            return None;
        }
        let dt = self
            .last_tick
            .map_or(FRAME.as_secs_f32(), |last| {
                now.saturating_duration_since(last).as_secs_f32()
            })
            .min(MAX_DT);
        self.last_tick = Some(now);
        let share = 1.0 - (-dt / TAU).exp();
        let (x, y) = self.left;
        let mut step = (x * share, y * share);
        if (x - step.0).abs() < DONE && (y - step.1).abs() < DONE {
            step = self.left;
        }
        self.left = (x - step.0, y - step.1);
        if !self.is_active() {
            self.last_tick = None;
        }
        Some(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticks at 120 Hz until the motion stops: (lines delivered, frames).
    fn run(motion: &mut WheelMotion, mut now: Instant) -> ((f32, f32), usize) {
        let (mut total, mut frames) = ((0.0, 0.0), 0);
        while let Some((x, y)) = motion.tick(now) {
            total = (total.0 + x, total.1 + y);
            frames += 1;
            now += FRAME;
            assert!(frames < 1000);
        }
        (total, frames)
    }

    #[test]
    fn step_lands_in_full_over_a_few_frames() {
        let mut motion = WheelMotion::default();
        motion.wheel((0.0, -3.0));
        let (total, frames) = run(&mut motion, Instant::now());
        assert_eq!(total.0, 0.0);
        assert!((total.1 + 3.0).abs() < 1e-4, "{total:?}");
        // More than a jump, and settled within 170 ms.
        assert!(frames > 3 && frames <= 21, "{frames} frames");
        assert!(!motion.is_active());
    }

    #[test]
    fn first_share_goes_out_at_once() {
        let mut motion = WheelMotion::default();
        motion.wheel((0.0, 1.0));
        let (_, y) = motion.tick(Instant::now()).unwrap();
        assert!(y > 0.1 && y < 0.3, "{y}");
    }

    #[test]
    fn steps_in_one_direction_add_up() {
        let mut motion = WheelMotion::default();
        let start = Instant::now();
        motion.wheel((0.0, 2.0));
        let (_, first) = motion.tick(start).unwrap();
        motion.wheel((0.0, 2.0));
        let (total, _) = run(&mut motion, start + FRAME);
        assert!((first + total.1 - 4.0).abs() < 1e-4);
    }

    #[test]
    fn turning_back_drops_what_was_left() {
        let mut motion = WheelMotion::default();
        let start = Instant::now();
        motion.wheel((0.0, 3.0));
        motion.tick(start);
        motion.wheel((0.0, -1.0));
        let (total, _) = run(&mut motion, start + FRAME);
        assert!((total.1 + 1.0).abs() < 1e-4, "{total:?}");
    }

    #[test]
    fn a_stalled_frame_does_not_jump() {
        let mut motion = WheelMotion::default();
        let start = Instant::now();
        motion.wheel((0.0, 10.0));
        motion.tick(start);
        let (_, y) = motion.tick(start + Duration::from_secs(2)).unwrap();
        // At most the share of MAX_DT (92%) of what was left, never all of it.
        assert!(y < 10.0 * 0.95, "{y}");
    }

    #[test]
    fn stop_drops_the_rest() {
        let mut motion = WheelMotion::default();
        motion.wheel((1.0, 1.0));
        motion.stop();
        assert!(motion.tick(Instant::now()).is_none());
    }
}
