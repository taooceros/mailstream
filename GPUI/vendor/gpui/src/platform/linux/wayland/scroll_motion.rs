//! Mailstream patch (MS-208): smooth wheel steps and kinetic touchpad scrolling on Wayland.
//!
//! Wayland hands clients raw scroll input and leaves the motion to them: a wheel notch arrives as
//! one step of lines, and a touchpad stream simply ends with `axis_stop` when the fingers lift.
//! Upstream gpui applies both as they come, so a wheel jumps three lines at once and a flick stops
//! dead. This spreads each wheel step over a short ease-out and, after a flick, keeps the content
//! moving with the release velocity under the same deceleration as the Mac's (0.998 per ms), so
//! scroll views see what AppKit gives them on macOS: `Started`, `Moved` with the fingers, `Ended`
//! on lift, then momentum as `Moved`.
//!
//! Cargo won't run tests of a patched dependency; the tests below run standalone with `rustc
//! --edition 2024 --test` on a file that defines `Point<T> { x, y }` and `point`, then includes
//! this one as `mod scroll_motion` (`#[path]`).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::{Point, point};

/// How often the motion advances while it runs (120 Hz).
pub(super) const TICK: Duration = Duration::from_micros(8_333);
/// Wheel ease-out time constant: 95% of a step lands within three of these (120 ms).
const WHEEL_TAU: f32 = 0.04;
/// Below this many lines left (about a pixel), a wheel step finishes.
const WHEEL_DONE: f32 = 0.05;
/// Fling deceleration time constant: the Mac's normal rate, 0.998 per millisecond.
const FLING_TAU: f32 = 0.4995;
/// Release speeds below this (pixels per second) don't fling.
const FLING_START: f32 = 150.0;
/// A fling stops once it is slower than this.
const FLING_STOP: f32 = 15.0;
/// Release speeds are capped here.
const FLING_MAX: f32 = 6_000.0;
/// The window of touchpad movement before lift that sets the release velocity (ms).
const VELOCITY_WINDOW: u32 = 100;
/// Fingers resting this long before lifting release without a fling (ms).
const REST: u32 = 50;
/// The longest step one tick takes, so a stalled loop doesn't jump.
const MAX_DT: f32 = 0.1;

/// What one tick delivers.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Tick {
    /// Wheel motion, in lines.
    pub lines: Option<Point<f32>>,
    /// Fling motion, in pixels.
    pub pixels: Option<Point<f32>>,
    /// Whether the motion keeps running.
    pub more: bool,
}

#[derive(Default)]
pub(super) struct ScrollMotion {
    /// Wheel lines not yet delivered.
    wheel: Point<f32>,
    /// Fling velocity, pixels per second.
    velocity: Point<f32>,
    /// Recent touchpad movement: (time in ms, pixels).
    samples: VecDeque<(u32, Point<f32>)>,
    /// Fingers are on the touchpad, scrolling.
    touching: bool,
    /// A timer drives the motion: the one started with `generation`.
    ticking: bool,
    generation: u64,
    last_tick: Option<Instant>,
}

impl ScrollMotion {
    /// A wheel step of `lines`; it stops a fling.
    pub fn wheel(&mut self, lines: Point<f32>) {
        self.velocity = point(0.0, 0.0);
        self.wheel.x += lines.x;
        self.wheel.y += lines.y;
    }

    /// Touchpad movement at `time` (ms). Returns true when it starts a gesture.
    pub fn finger(&mut self, time: u32, delta: Point<f32>) -> bool {
        self.velocity = point(0.0, 0.0);
        let began = !self.touching;
        self.touching = true;
        if began {
            self.samples.clear();
        }
        self.samples.push_back((time, delta));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| time.wrapping_sub(*at) > VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
        began
    }

    /// The fingers lifted at `time` (ms): sets the fling. Returns true when a gesture was going.
    pub fn finger_stop(&mut self, time: u32) -> bool {
        if !self.touching {
            return false;
        }
        self.touching = false;
        self.velocity = self.release_velocity(time);
        true
    }

    fn release_velocity(&mut self, time: u32) -> Point<f32> {
        let none = point(0.0, 0.0);
        let samples = std::mem::take(&mut self.samples);
        let (Some(&(first, _)), Some(&(last, _))) = (samples.front(), samples.back()) else {
            return none;
        };
        let span = last.wrapping_sub(first);
        if samples.len() < 2 || span == 0 || time.wrapping_sub(last) > REST {
            return none;
        }
        // The first sample's movement happened before the window opened.
        let (dx, dy) = samples
            .iter()
            .skip(1)
            .fold((0.0, 0.0), |(x, y), (_, d)| (x + d.x, y + d.y));
        let seconds = span as f32 / 1000.0;
        let mut velocity = point(dx / seconds, dy / seconds);
        let speed = velocity.x.hypot(velocity.y);
        if speed < FLING_START {
            return none;
        }
        if speed > FLING_MAX {
            let scale = FLING_MAX / speed;
            velocity = point(velocity.x * scale, velocity.y * scale);
        }
        velocity
    }

    /// Stops any wheel step or fling in progress (a click, the pointer leaving, fingers resting).
    pub fn stop(&mut self) {
        self.wheel = point(0.0, 0.0);
        self.velocity = point(0.0, 0.0);
        self.ticking = false;
    }

    fn active(&self) -> bool {
        self.wheel != point(0.0, 0.0) || self.velocity != point(0.0, 0.0)
    }

    /// When the motion has work and no timer drives it yet, the generation of the timer the
    /// caller starts: it calls `tick` every `TICK` while `is_current` and `tick` says `more`.
    pub fn needs_timer(&mut self) -> Option<u64> {
        if self.ticking || !self.active() {
            return None;
        }
        self.ticking = true;
        self.generation += 1;
        self.last_tick = None;
        Some(self.generation)
    }

    /// Whether the timer started with `generation` still drives the motion.
    pub fn is_current(&self, generation: u64) -> bool {
        self.ticking && self.generation == generation
    }

    pub fn tick(&mut self, now: Instant) -> Tick {
        let dt = self
            .last_tick
            .map_or(TICK.as_secs_f32(), |last| now.duration_since(last).as_secs_f32())
            .min(MAX_DT);
        self.last_tick = Some(now);
        let mut tick = Tick::default();

        if self.wheel != point(0.0, 0.0) {
            let share = 1.0 - (-dt / WHEEL_TAU).exp();
            let mut step = point(self.wheel.x * share, self.wheel.y * share);
            if (self.wheel.x - step.x).abs() < WHEEL_DONE && (self.wheel.y - step.y).abs() < WHEEL_DONE
            {
                step = self.wheel;
            }
            self.wheel = point(self.wheel.x - step.x, self.wheel.y - step.y);
            tick.lines = Some(step);
        }

        if self.velocity != point(0.0, 0.0) {
            let decay = (-dt / FLING_TAU).exp();
            // Distance covered while the speed decays over `dt`.
            let travel = FLING_TAU * (1.0 - decay);
            tick.pixels = Some(point(self.velocity.x * travel, self.velocity.y * travel));
            self.velocity = point(self.velocity.x * decay, self.velocity.y * decay);
            if self.velocity.x.hypot(self.velocity.y) < FLING_STOP {
                self.velocity = point(0.0, 0.0);
            }
        }

        tick.more = self.active();
        self.ticking = tick.more;
        tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(motion: &mut ScrollMotion) -> (Point<f32>, Point<f32>, usize) {
        let mut now = Instant::now();
        let (mut lines, mut pixels, mut ticks) = (point(0.0, 0.0), point(0.0, 0.0), 0);
        let Some(generation) = motion.needs_timer() else {
            return (lines, pixels, ticks);
        };
        while motion.is_current(generation) {
            let tick = motion.tick(now);
            if let Some(l) = tick.lines {
                lines = point(lines.x + l.x, lines.y + l.y);
            }
            if let Some(p) = tick.pixels {
                pixels = point(pixels.x + p.x, pixels.y + p.y);
            }
            ticks += 1;
            now += TICK;
            if !tick.more {
                break;
            }
        }
        (lines, pixels, ticks)
    }

    #[test]
    fn wheel_step_lands_in_full_and_quickly() {
        let mut motion = ScrollMotion::default();
        motion.wheel(point(0.0, -3.0));
        let (lines, pixels, ticks) = run(&mut motion);
        assert_eq!(lines.x, 0.0);
        assert!((lines.y + 3.0).abs() < 1e-4, "{lines:?}");
        assert_eq!(pixels, point(0.0, 0.0));
        // A three-line step settles within 170 ms at 120 Hz.
        assert!(ticks > 3 && ticks <= 21, "{ticks} ticks");
        assert!(motion.needs_timer().is_none());
    }

    #[test]
    fn flick_coasts_on_in_the_same_direction() {
        let mut motion = ScrollMotion::default();
        assert!(motion.finger(1000, point(0.0, 20.0)));
        for i in 1..=6 {
            assert!(!motion.finger(1000 + i * 8, point(0.0, 20.0)));
        }
        assert!(motion.finger_stop(1050));
        // 6 × 20 px over 48 ms: 2500 px/s, coasting about 2500 × 0.5 = 1250 px.
        let (_, pixels, _) = run(&mut motion);
        assert!(pixels.y > 1100.0 && pixels.y < 1300.0, "{pixels:?}");
        assert_eq!(pixels.x, 0.0);
    }

    #[test]
    fn resting_fingers_release_without_fling() {
        let mut motion = ScrollMotion::default();
        motion.finger(1000, point(0.0, 20.0));
        motion.finger(1008, point(0.0, 20.0));
        assert!(motion.finger_stop(1200));
        assert!(motion.needs_timer().is_none());
    }

    #[test]
    fn slow_drag_releases_without_fling() {
        let mut motion = ScrollMotion::default();
        for i in 0..10 {
            motion.finger(1000 + i * 10, point(0.0, 1.0));
        }
        motion.finger_stop(1095);
        assert!(motion.needs_timer().is_none());
    }

    #[test]
    fn new_input_stops_a_fling() {
        let mut motion = ScrollMotion::default();
        for i in 0..6 {
            motion.finger(1000 + i * 8, point(0.0, 30.0));
        }
        motion.finger_stop(1045);
        assert!(motion.needs_timer().is_some());
        motion.tick(Instant::now());
        assert!(motion.finger(2000, point(0.0, 1.0)));
        assert_eq!(motion.tick(Instant::now()), Tick::default());
        motion.finger_stop(2001);
        motion.wheel(point(0.0, 1.0));
        motion.stop();
        assert!(!motion.tick(Instant::now()).more);
    }

    #[test]
    fn lift_without_gesture_is_ignored() {
        let mut motion = ScrollMotion::default();
        assert!(!motion.finger_stop(10));
    }
}
