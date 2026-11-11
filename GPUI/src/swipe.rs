//! The two-finger trackpad back swipe (`UI/BackSwipe.swift`): fingers moving sideways drag the top
//! page (the reader, a bundle) off the list; gestures that start vertical pass through untouched,
//! so the page scrolls. Fed from scroll events in the capture phase.

#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Instant;

use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

pub enum Step {
    /// Not a back swipe: deliver the event as usual.
    Pass,
    /// Part of a back swipe with nothing to show (the momentum after release).
    Consume,
    /// The page follows the fingers, `offset` points to the right (negative: left).
    Drag(f32),
    /// Fingers lifted; `velocity` in points per second, positive to the right.
    Release { offset: f32, velocity: f32 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Undecided,
    Ignoring,
    Tracking,
    /// After release: the momentum events that follow are swallowed, so the flick does not
    /// scroll whatever ends up under the pointer.
    Coasting,
}

/// The system trackpad settings the gesture follows, read once off the main thread.
#[derive(Clone, Copy)]
pub struct Preferences {
    /// "Swipe between pages" with two fingers is on.
    pub enabled: bool,
    /// Natural scrolling: content follows the fingers.
    pub natural: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { enabled: true, natural: true }
    }
}

impl Preferences {
    /// `defaults` values, with the system defaults (both on) when a key is unset. Blocking.
    #[cfg(target_os = "macos")]
    pub fn read() -> Self {
        let flag = |key: &str| {
            Command::new("/usr/bin/defaults")
                .args(["read", "-g", key])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim() != "0")
                .unwrap_or(true)
        };
        Self { enabled: flag("AppleEnableSwipeNavigateWithScrolls"), natural: flag("com.apple.swipescrolldirection") }
    }

    /// Linux keeps no such settings where every desktop reads them: the swipe stays on, and the
    /// compositor has already turned the scroll for the person's scrolling direction.
    #[cfg(not(target_os = "macos"))]
    pub fn read() -> Self {
        Self::default()
    }
}

pub struct BackSwipe {
    state: State,
    offset: f32,
    /// The side the swipe set off to (1 right, -1 left); the page never crosses back past 0.
    direction: f32,
    travel: (f32, f32),
    velocity: f32,
    last: Instant,
    pub preferences: Preferences,
}

/// Movement it takes to tell a sideways swipe from a scroll.
const DECISION_DISTANCE: f32 = 6.0;

impl Default for BackSwipe {
    fn default() -> Self {
        Self {
            state: State::Idle,
            offset: 0.0,
            direction: 1.0,
            travel: (0.0, 0.0),
            velocity: 0.0,
            last: Instant::now(),
            preferences: Preferences::default(),
        }
    }
}

impl BackSwipe {
    /// `can_start`: whether a gesture beginning with this event may become a back swipe.
    pub fn feed(&mut self, event: &ScrollWheelEvent, can_start: bool) -> Step {
        // Mouse wheels scroll by lines; only trackpads send pixel deltas.
        let ScrollDelta::Pixels(delta) = event.delta else { return Step::Pass };
        let (raw_dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        // Fingers to the right, whichever way the user's scrolling direction is set.
        let dx = if self.preferences.natural { raw_dx } else { -raw_dx };
        self.step(event.touch_phase, dx, dy, Instant::now(), can_start && self.preferences.enabled)
    }

    fn step(&mut self, phase: TouchPhase, dx: f32, dy: f32, now: Instant, can_start: bool) -> Step {
        match phase {
            TouchPhase::Started => {
                self.state = if can_start { State::Undecided } else { State::Ignoring };
                self.offset = 0.0;
                self.travel = (0.0, 0.0);
                self.velocity = 0.0;
                self.last = now;
                // `Started` also stands for "may begin", which carries no movement.
                return Step::Pass;
            }
            // gpui reports momentum as `Moved` with no touch: after `Ended` they are the flick.
            TouchPhase::Moved | TouchPhase::Ended => {}
        }
        let lifted = matches!(phase, TouchPhase::Ended);
        match self.state {
            State::Idle => Step::Pass,
            State::Coasting => {
                if lifted {
                    self.state = State::Idle;
                }
                Step::Consume
            }
            State::Ignoring => {
                if lifted {
                    self.state = State::Idle;
                }
                Step::Pass
            }
            State::Undecided => {
                if lifted {
                    self.state = State::Idle;
                    return Step::Pass;
                }
                self.travel.0 += dx;
                self.travel.1 += dy;
                if self.travel.0.hypot(self.travel.1) < DECISION_DISTANCE {
                    return Step::Pass;
                }
                if self.travel.0.abs() <= self.travel.1.abs() * 1.5 {
                    self.state = State::Ignoring;
                    return Step::Pass;
                }
                self.state = State::Tracking;
                self.direction = if self.travel.0 > 0.0 { 1.0 } else { -1.0 };
                self.offset = self.travel.0;
                self.last = now;
                Step::Drag(self.offset)
            }
            State::Tracking => {
                if lifted {
                    self.state = State::Coasting;
                    return Step::Release { offset: self.offset, velocity: self.velocity };
                }
                let dt = now.duration_since(self.last).as_secs_f32();
                self.last = now;
                if dt > 0.0 {
                    self.velocity = self.velocity * 0.5 + dx / dt * 0.5;
                }
                self.offset = self.direction * (self.direction * (self.offset + dx)).max(0.0);
                Step::Drag(self.offset)
            }
        }
    }
}

/// Where released fingers send a page `width` wide: past a third of the width, or flicked
/// onward, off the side it was swiped toward; otherwise back to 0. Returns (back, target).
pub fn settle(offset: f32, velocity: f32, width: f32) -> (bool, f32) {
    let width = width.max(1.0);
    let distance = offset.abs();
    let onward = if offset < 0.0 { -velocity } else { velocity };
    let back = distance > width / 3.0 || (onward > 400.0 && distance > 20.0);
    let target = if !back {
        0.0
    } else if offset < 0.0 {
        -width
    } else {
        width
    };
    (back, target)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn run(swipe: &mut BackSwipe, events: &[(TouchPhase, f32, f32)]) -> Vec<Step> {
        let start = Instant::now();
        events
            .iter()
            .enumerate()
            .map(|(i, &(phase, dx, dy))| swipe.step(phase, dx, dy, start + Duration::from_millis(16 * i as u64), true))
            .collect()
    }

    #[test]
    fn sideways_fingers_drag_and_release_and_swallow_the_flick() {
        let mut swipe = BackSwipe::default();
        let steps = run(
            &mut swipe,
            &[
                (TouchPhase::Started, 0.0, 0.0),
                (TouchPhase::Moved, 4.0, 0.5),
                (TouchPhase::Moved, 4.0, 0.5),
                (TouchPhase::Moved, 30.0, 1.0),
                (TouchPhase::Ended, 0.0, 0.0),
                (TouchPhase::Moved, 20.0, 0.0),
            ],
        );
        assert!(matches!(steps[1], Step::Pass), "not decided below the threshold");
        assert!(matches!(steps[2], Step::Drag(offset) if offset == 8.0));
        assert!(matches!(steps[3], Step::Drag(offset) if offset == 38.0));
        assert!(matches!(steps[4], Step::Release { offset, .. } if offset == 38.0));
        assert!(matches!(steps[5], Step::Consume), "momentum after release is swallowed");
    }

    #[test]
    fn vertical_scrolls_pass_through_for_the_whole_gesture() {
        let mut swipe = BackSwipe::default();
        let steps = run(
            &mut swipe,
            &[
                (TouchPhase::Started, 0.0, 0.0),
                (TouchPhase::Moved, 1.0, 10.0),
                (TouchPhase::Moved, 40.0, 0.0),
                (TouchPhase::Ended, 0.0, 0.0),
            ],
        );
        assert!(steps.iter().all(|step| matches!(step, Step::Pass)));
    }

    #[test]
    fn the_page_never_crosses_back_past_its_place() {
        let mut swipe = BackSwipe::default();
        let steps = run(
            &mut swipe,
            &[(TouchPhase::Started, 0.0, 0.0), (TouchPhase::Moved, 10.0, 0.0), (TouchPhase::Moved, -50.0, 0.0)],
        );
        assert!(matches!(steps[2], Step::Drag(offset) if offset == 0.0));
    }

    #[test]
    fn settling_goes_back_past_a_third_or_on_a_flick() {
        assert_eq!(settle(400.0, 0.0, 900.0), (true, 900.0));
        assert_eq!(settle(100.0, 0.0, 900.0), (false, 0.0));
        assert_eq!(settle(100.0, 800.0, 900.0), (true, 900.0));
        assert_eq!(settle(-100.0, -800.0, 900.0), (true, -900.0));
        assert_eq!(settle(100.0, -800.0, 900.0), (false, 0.0));
    }
}
