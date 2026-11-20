//! Dragging a page (the reader, a bundle, the calendar's open event and its page) off what it
//! covers to close it, two ways that share one state machine (`Gesture`) and one slide (`Slide`):
//! the two-finger trackpad back swipe (`UI/BackSwipe.swift`), fed from scroll events in the
//! capture phase, and the mouse (MS-266), a press dragged sideways. The page follows the pointer;
//! released past a third of its width or flicked onward it goes off that side and closes,
//! otherwise it springs back (`settle`), on the pages' spring (`motion::PAGE`) from the speed it
//! was let go at, so it carries on as the pointer moved (MS-289). Gestures that set off
//! vertical pass through untouched, so the page scrolls (or the window moves).
//!
//! Where a mouse drag may start (MS-266): only on the page itself outside what takes presses of
//! its own. Mail bodies (their text selects and their links open on a drag or click), the reply
//! box, event notes and links, and every button stop their presses, so a drag there selects text,
//! or does what it did, never moves the page. Anywhere else on the page (its title row, the
//! margins, a message's header, pills, a bundle's rows, an event's head and guests) it starts
//! undecided, and the first `WINDOW_MOVE_SLOP` points decide it as the window's title rows do
//! (MS-206): mostly sideways (`SIDEWAYS` times the vertical), the page follows; otherwise nothing
//! moves the page, and on a page's title row the window moves instead. So the gaps between the
//! islands and the list's and sidebar's title rows always move the window; a page's title row
//! moves the page sideways and the window in any other direction. A drag the page followed never
//! clicks what it began on.

#[cfg(target_os = "macos")]
use std::process::Command;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use std::collections::VecDeque;

use gpui::{
    App, Bounds, Canvas, Context, DispatchPhase, Entity, HitboxBehavior, HitboxId, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollDelta, ScrollEvent, TouchPhase, Window, canvas,
};

use crate::controls::WINDOW_MOVE_SLOP;
use crate::design::motion;
use crate::frame_clock::{self, now, since};

pub enum Step {
    /// Not a back swipe: deliver the event as usual.
    Pass,
    /// Part of a back swipe with nothing to show (the momentum after release).
    Consume,
    /// The page follows the pointer, `offset` points to the right (negative: left).
    Drag(f32),
    /// Fingers lifted or the button released; `velocity` in points per second, positive to the right.
    Release { offset: f32, velocity: f32 },
    /// The system took the gesture: the page returns from `offset` to where it was.
    Cancel(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Undecided,
    Ignoring,
    Tracking,
    /// After the fingers lift: the momentum events that follow are swallowed, so the flick does
    /// not scroll whatever ends up under the pointer.
    Coasting,
}

/// Movement it takes to tell a sideways trackpad swipe from a scroll.
const SWIPE_DECISION: f32 = 6.0;
/// How much more sideways than vertical the first movement must be to drag the page.
const SIDEWAYS: f32 = 1.5;
/// A pointer resting this long before it lets go flicks nothing.
const STILL: Duration = Duration::from_millis(100);
/// Trailing window over which release velocity is measured: smooths frame jitter and prevents
/// batched events arriving microseconds apart (e.g. Android resampled touch bursts) from
/// dividing a tiny delta by a microsecond `dt`.
const VELOCITY_WINDOW: Duration = Duration::from_millis(60);
/// Events arriving closer than this are part of the same frame/batch and coalesce into one sample.
const MIN_SAMPLE_DT: Duration = Duration::from_millis(1);
/// Minimum time span across samples needed to compute a release velocity.
const MIN_VELOCITY_SPAN: Duration = Duration::from_millis(4);

/// The state machine the trackpad and the mouse share: movement starts undecided; past `decision`
/// points it drags the page when mostly sideways, else the gesture is left alone to its end.
struct Gesture {
    state: State,
    decision: f32,
    offset: f32,
    /// The side the gesture set off to (1 right, -1 left); the page never crosses back past 0.
    direction: f32,
    travel: (f32, f32),
    samples: VecDeque<(Instant, f32)>,
    last: Instant,
}

impl Gesture {
    fn new(decision: f32) -> Self {
        Self {
            state: State::Idle,
            decision,
            offset: 0.0,
            direction: 1.0,
            travel: (0.0, 0.0),
            samples: VecDeque::new(),
            last: Instant::now(),
        }
    }

    /// The fingers touch or the button goes down; `can_start`: it may drag a page.
    fn start(&mut self, can_start: bool, now: Instant) {
        self.state = if can_start { State::Undecided } else { State::Ignoring };
        self.offset = 0.0;
        self.travel = (0.0, 0.0);
        self.samples.clear();
        self.last = now;
    }

    fn cancel(&mut self) -> Step {
        let tracking = self.state == State::Tracking;
        self.state = State::Idle;
        self.samples.clear();
        if tracking { Step::Cancel(self.offset) } else { Step::Pass }
    }

    fn record_sample(&mut self, now: Instant, offset: f32) {
        if let Some(back) = self.samples.back_mut()
            && now.saturating_duration_since(back.0) < MIN_SAMPLE_DT
        {
            *back = (now, offset);
        } else {
            self.samples.push_back((now, offset));
        }
        while self.samples.len() > 2
            && self.samples.get(1).is_some_and(|&(next, _)| now.saturating_duration_since(next) > VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    fn velocity(&self, now: Instant) -> f32 {
        if now.saturating_duration_since(self.last) > STILL {
            return 0.0;
        }
        let Some(&(newest_t, newest_x)) = self.samples.back() else {
            return 0.0;
        };
        let mut oldest = (newest_t, newest_x);
        let mut prev_t = newest_t;
        for &(t, x) in self.samples.iter().rev().skip(1) {
            if newest_t.saturating_duration_since(t) > VELOCITY_WINDOW || prev_t.saturating_duration_since(t) > STILL {
                break;
            }
            oldest = (t, x);
            prev_t = t;
        }
        let span = newest_t.saturating_duration_since(oldest.0);
        if span < MIN_VELOCITY_SPAN {
            return 0.0;
        }
        (newest_x - oldest.1) / span.as_secs_f32()
    }

    /// The pointer moved `dx` to the right and `dy` down.
    fn moved(&mut self, dx: f32, dy: f32, now: Instant) -> Step {
        match self.state {
            State::Idle | State::Ignoring => Step::Pass,
            State::Coasting => Step::Consume,
            State::Undecided => {
                self.travel.0 += dx;
                self.travel.1 += dy;
                if self.travel.0.hypot(self.travel.1) <= self.decision {
                    return Step::Pass;
                }
                if self.travel.0.abs() <= self.travel.1.abs() * SIDEWAYS {
                    self.state = State::Ignoring;
                    return Step::Pass;
                }
                self.state = State::Tracking;
                self.direction = if self.travel.0 > 0.0 { 1.0 } else { -1.0 };
                self.offset = self.travel.0;
                self.last = now;
                self.samples.clear();
                self.samples.push_back((now, self.offset));
                Step::Drag(self.offset)
            }
            State::Tracking => {
                self.last = now;
                self.offset = self.direction * (self.direction * (self.offset + dx)).max(0.0);
                self.record_sample(now, self.offset);
                Step::Drag(self.offset)
            }
        }
    }

    /// The fingers lift or the button goes up; `coast`: momentum events follow (the trackpad's).
    fn lifted(&mut self, now: Instant, coast: bool) -> Step {
        match self.state {
            State::Tracking => {
                self.state = if coast { State::Coasting } else { State::Idle };
                let velocity = self.velocity(now);
                self.samples.clear();
                Step::Release { offset: self.offset, velocity }
            }
            State::Coasting => {
                self.state = State::Idle;
                Step::Consume
            }
            State::Idle | State::Undecided | State::Ignoring => {
                self.state = State::Idle;
                Step::Pass
            }
        }
    }
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

/// The two-finger swipe.
///
/// Uses interior mutability (`RefCell`) for the gesture state machine so `scroll_capture` can
/// inspect and ignore vertical scroll events via `Entity::read` without calling `Entity::update`,
/// which would otherwise bump GPUI's entity write generation and invalidate Scroll Layers on every
/// scroll frame (MS-421).
pub struct BackSwipe {
    gesture: RefCell<Gesture>,
    pub preferences: Preferences,
}

impl Default for BackSwipe {
    fn default() -> Self {
        Self { gesture: RefCell::new(Gesture::new(SWIPE_DECISION)), preferences: Preferences::default() }
    }
}

impl BackSwipe {
    /// `can_start`: whether a gesture beginning with this event may become a back swipe.
    pub fn feed(&self, event: &ScrollEvent, can_start: bool) -> Step {
        // Mouse wheels scroll by lines; only trackpads send pixel deltas.
        let ScrollDelta::Pixels(delta) = event.delta else { return Step::Pass };
        let (raw_dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        // Fingers to the right, whichever way the user's scrolling direction is set.
        let dx = if self.preferences.natural { raw_dx } else { -raw_dx };
        self.step(event.touch_phase, dx, dy, Instant::now(), can_start && self.preferences.enabled)
    }

    fn step(&self, phase: TouchPhase, dx: f32, dy: f32, now: Instant, can_start: bool) -> Step {
        let mut gesture = self.gesture.borrow_mut();
        match phase {
            // `Started` also stands for "may begin", which carries no movement.
            TouchPhase::Started => {
                gesture.start(can_start, now);
                Step::Pass
            }
            TouchPhase::Cancelled => gesture.cancel(),
            // gpui reports momentum as `Moved` with no touch: after `Ended` they are the flick.
            TouchPhase::Moved => gesture.moved(dx, dy, now),
            TouchPhase::Ended => gesture.lifted(now, true),
        }
    }
}

/// What a page's host hears of the mouse after a press on the page (`MouseDrag::press`).
#[derive(Clone, Copy)]
pub enum Pointer {
    /// `held`: the left button is still down.
    Move { at: Point<Pixels>, held: bool },
    Release,
}

/// The mouse's drag (MS-266): a left press on a page, then the pointer's movement until the
/// button goes up.
pub struct MouseDrag {
    gesture: Gesture,
    /// Where the pointer was last, from the press until the button goes up.
    at: Option<Point<Pixels>>,
}

impl Default for MouseDrag {
    fn default() -> Self {
        Self { gesture: Gesture::new(WINDOW_MOVE_SLOP as f32), at: None }
    }
}

impl MouseDrag {
    /// A single left press on a page at `at`; `can_start`: the page is one a drag may close.
    pub fn press(&mut self, at: Point<Pixels>, can_start: bool) {
        self.press_at(at, can_start, Instant::now());
    }

    fn press_at(&mut self, at: Point<Pixels>, can_start: bool, now: Instant) {
        self.gesture.start(can_start, now);
        self.at = Some(at);
    }

    pub fn feed(&mut self, pointer: Pointer) -> Step {
        self.step(pointer, Instant::now())
    }

    fn step(&mut self, pointer: Pointer, now: Instant) -> Step {
        let Some(last) = self.at else { return Step::Pass };
        match pointer {
            Pointer::Move { at, held: true } => {
                self.at = Some(at);
                self.gesture.moved(f32::from(at.x - last.x), f32::from(at.y - last.y), now)
            }
            // The button went up where the window did not hear it (outside it).
            Pointer::Move { held: false, .. } | Pointer::Release => {
                self.at = None;
                self.gesture.lifted(now, false)
            }
        }
    }
}

/// How a page sits over what it covers.
#[derive(Clone, Copy)]
pub enum Slide {
    Rest,
    /// Sliding in from the right since then.
    Opening(Instant),
    /// Under the pointer, `offset` points to the right.
    Drag(f32),
    /// Released (or sent back by a key): from `from` to `to`, leaving at `velocity` points per
    /// second to the right; `back` closes the page on arrival.
    Settle { from: f32, to: f32, velocity: f32, start: Instant, back: bool },
}

impl Slide {
    /// A page sliding in from the right now.
    pub fn opening() -> Self {
        Slide::Opening(now())
    }

    /// A page at `from` sliding to `to` now, from rest; `back` closes it on arrival.
    pub fn settle(from: f32, to: f32, back: bool) -> Self {
        Slide::Settle { from, to, velocity: 0., start: now(), back }
    }

    /// Where a page `width` wide sits `elapsed` seconds into its motion, and whether the motion
    /// has arrived.
    fn place(&self, width: f32, elapsed: f32) -> (f32, bool) {
        match *self {
            Slide::Rest => (0., true),
            Slide::Drag(offset) => (offset, true),
            Slide::Opening(_) => motion::PAGE.at(width, 0., 0., elapsed),
            Slide::Settle { from, to, velocity, .. } => motion::PAGE.at(from, to, velocity, elapsed),
        }
    }

    /// How far right of its place a page `width` wide sits now, without advancing anything.
    pub fn offset(&self, width: f32) -> f32 {
        let elapsed = match *self {
            Slide::Opening(start) | Slide::Settle { start, .. } => since(start),
            Slide::Rest | Slide::Drag(_) => 0.,
        };
        self.place(width, elapsed).0
    }

    /// Where a page `width` wide sits, called while drawing it, by the frame's time
    /// (`frame_clock`): asks for the next frame at the display's rate while it moves and comes to
    /// rest on arrival. None once it has slid off: the page closes.
    pub fn advance(&mut self, width: f32, window: &mut Window, cx: &App) -> Option<f32> {
        let elapsed = match self {
            Slide::Opening(start) | Slide::Settle { start, .. } => frame_clock::elapsed(start, window, cx),
            Slide::Rest | Slide::Drag(_) => 0.,
        };
        let (offset, arrived) = self.place(width, elapsed);
        if !arrived {
            window.request_animation_frame_at_display_rate();
            return Some(offset);
        }
        match *self {
            Slide::Rest | Slide::Drag(_) => Some(offset),
            Slide::Settle { back, .. } => {
                *self = Slide::Rest;
                (!back).then_some(0.)
            }
            Slide::Opening(_) => {
                *self = Slide::Rest;
                Some(0.)
            }
        }
    }

    /// Follows a gesture's step for a page `width` wide: true when the gesture took the event.
    pub fn follow(&mut self, step: Step, width: f32) -> bool {
        *self = match step {
            Step::Pass => return false,
            Step::Consume => return true,
            Step::Drag(offset) => Slide::Drag(offset),
            Step::Release { offset, velocity } => {
                let (back, to) = settle(offset, velocity, width);
                Slide::Settle { from: offset, to, velocity, start: now(), back }
            }
            Step::Cancel(offset) => Slide::settle(offset, 0., false),
        };
        true
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

/// Hands `host` the scroll events over the element it is drawn as, in the capture phase, before
/// any scroll view under it scrolls with them, with where it points from the element's top left:
/// none where something drawn after it covers the pointer (a toast, a floating draft, a menu, a
/// page laid over it), which `feed` answers with no swipe that starts there (MS-319). Draw it
/// over the pages it serves, after them, pinned to their edges (`absolute().inset_0()`: placed
/// after content in the flow it would sit below it, MS-384): it takes no pointer itself, its plain
/// hitbox only telling whether anything over it does. Only a gesture's start asks: one under way
/// when a toast shows up under the fingers goes on. `feed` answers true when the swipe took the
/// event.
#[allow(clippy::type_complexity)]
pub fn scroll_capture<T: 'static>(
    host: Entity<T>,
    peek: fn(&T, &ScrollEvent, Option<Point<Pixels>>) -> Step,
    follow: fn(&mut T, Step, &mut Context<T>) -> bool,
) -> Canvas<HitboxId> {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal).id,
        move |bounds: Bounds<Pixels>, hitbox: HitboxId, window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx: &mut App| {
                if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                    return;
                }
                let at = hitbox.should_handle_scroll(window).then(|| event.position - bounds.origin);
                match peek(host.read(cx), event, at) {
                    Step::Pass => {}
                    Step::Consume => {
                        window.cancel_touch_momentum();
                        cx.stop_propagation();
                    }
                    step => {
                        let ends = matches!(step, Step::Release { .. } | Step::Cancel(_));
                        if host.update(cx, |this, cx| follow(this, step, cx)) {
                            if ends {
                                window.cancel_touch_momentum();
                            }
                            cx.stop_propagation();
                        }
                    }
                }
            });
        },
    )
}

/// Hands `host` the mouse's movement and release after a press on one of its pages, as two
/// empty elements, both in the capture phase; `feed` answers true when the drag took the event,
/// which then goes no further. `under` is drawn before the pages: it hears the pointer move
/// before their title rows do, so a sideways drag never moves the window. `over` is drawn after
/// them: it hears the release once what the press went down on has, so the release of a drag
/// the page followed clicks nothing. Draw them only for the page on top.
pub fn mouse_capture<T: 'static>(
    host: Entity<T>,
    feed: fn(&mut T, Pointer, &mut Context<T>) -> bool,
) -> (Canvas<()>, Canvas<()>) {
    let released = host.clone();
    let under = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx: &mut App| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let pointer = Pointer::Move { at: event.position, held: event.pressed_button == Some(MouseButton::Left) };
                if host.update(cx, |this, cx| feed(this, pointer, cx)) {
                    cx.stop_propagation();
                }
            });
        },
    );
    let over = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx: &mut App| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                    return;
                }
                if released.update(cx, |this, cx| feed(this, Pointer::Release, cx)) {
                    cx.stop_propagation();
                }
            });
        },
    );
    (under, over)
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

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

    /// Mouse events `ms` apart from a press at (100, 100): moves to points, then a release.
    fn drag(mouse: &mut MouseDrag, can_start: bool, moves: &[(f32, f32)], ms: u64) -> Vec<Step> {
        let start = Instant::now();
        mouse.press_at(point(px(100.), px(100.)), can_start, start);
        let at = |i: usize| start + Duration::from_millis(ms * (i as u64 + 1));
        let mut steps: Vec<Step> = moves
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| mouse.step(Pointer::Move { at: point(px(x), px(y)), held: true }, at(i)))
            .collect();
        steps.push(mouse.step(Pointer::Release, at(moves.len())));
        steps
    }

    #[test]
    fn a_sideways_mouse_drag_decides_where_the_window_would_move_and_follows_the_pointer() {
        let mut mouse = MouseDrag::default();
        // 4 points is the title rows' slop: still a click there, so still undecided here.
        let steps = drag(&mut mouse, true, &[(104., 100.), (110., 101.), (190., 102.)], 8);
        assert!(matches!(steps[0], Step::Pass), "undecided within the slop");
        assert!(matches!(steps[1], Step::Drag(offset) if offset == 10.0), "decided past it, the page under the pointer");
        assert!(matches!(steps[2], Step::Drag(offset) if offset == 90.0));
        assert!(matches!(steps[3], Step::Release { offset, velocity } if offset == 90.0 && velocity > 400.0));
        // No momentum follows a mouse: the next press starts afresh.
        assert!(matches!(mouse.step(Pointer::Release, Instant::now()), Step::Pass));
    }

    #[test]
    fn a_mouse_drag_that_sets_off_vertical_leaves_the_page_and_the_window_moves() {
        let mut mouse = MouseDrag::default();
        let steps = drag(&mut mouse, true, &[(102., 105.), (160., 105.)], 8);
        assert!(steps.iter().all(|step| matches!(step, Step::Pass)), "left to the title row or the scroll");
    }

    #[test]
    fn a_press_where_no_page_may_close_never_drags() {
        let mut mouse = MouseDrag::default();
        let steps = drag(&mut mouse, false, &[(120., 100.), (200., 100.)], 8);
        assert!(steps.iter().all(|step| matches!(step, Step::Pass)));
        // Movement with no press before it is nobody's.
        let mut idle = MouseDrag::default();
        assert!(matches!(idle.step(Pointer::Move { at: point(px(300.), px(0.)), held: true }, Instant::now()), Step::Pass));
    }

    #[test]
    fn a_click_that_wobbles_never_closes_the_page() {
        // Within the slop the press stays a click.
        let mut mouse = MouseDrag::default();
        let steps = drag(&mut mouse, true, &[(103., 101.), (101., 102.)], 4);
        assert!(steps.iter().all(|step| matches!(step, Step::Pass)), "a click, whatever the hand did");
        // Just past it and let go fast, it springs back: a flick closes only past 20 points.
        let steps = drag(&mut mouse, true, &[(106., 100.), (112., 100.)], 4);
        let Step::Release { offset, velocity } = steps[2] else { panic!("the page followed the pointer") };
        assert!(velocity > 400.0);
        assert_eq!(settle(offset, velocity, 900.0), (false, 0.0));
    }

    #[test]
    fn a_pointer_resting_before_it_lets_go_flicks_nothing() {
        let mut mouse = MouseDrag::default();
        let start = Instant::now();
        mouse.press_at(point(px(0.), px(0.)), true, start);
        let move_to = |mouse: &mut MouseDrag, x: f32, ms: u64| {
            mouse.step(Pointer::Move { at: point(px(x), px(0.)), held: true }, start + Duration::from_millis(ms))
        };
        move_to(&mut mouse, 10., 8);
        move_to(&mut mouse, 60., 16);
        let release = mouse.step(Pointer::Release, start + Duration::from_millis(400));
        assert!(matches!(release, Step::Release { offset, velocity } if offset == 60.0 && velocity == 0.0));
        assert_eq!(settle(60.0, 0.0, 900.0), (false, 0.0), "a short drag let go at rest springs back");
    }

    #[test]
    fn a_button_released_outside_the_window_ends_the_drag() {
        let mut mouse = MouseDrag::default();
        let start = Instant::now();
        mouse.press_at(point(px(0.), px(0.)), true, start);
        mouse.step(Pointer::Move { at: point(px(400.), px(0.)), held: true }, start + Duration::from_millis(8));
        let back = mouse.step(Pointer::Move { at: point(px(420.), px(0.)), held: false }, start + Duration::from_millis(16));
        assert!(matches!(back, Step::Release { offset, .. } if offset == 400.0));
    }

    #[test]
    fn a_slide_follows_the_drag_and_settles_by_distance() {
        let mut slide = Slide::Rest;
        assert!(!slide.follow(Step::Pass, 900.0));
        assert!(slide.follow(Step::Drag(120.0), 900.0));
        assert_eq!(slide.offset(900.0), 120.0);
        assert!(slide.follow(Step::Release { offset: 400.0, velocity: 0.0 }, 900.0));
        assert!(matches!(slide, Slide::Settle { to, back: true, .. } if to == 900.0));
        assert!(slide.follow(Step::Release { offset: 100.0, velocity: 0.0 }, 900.0));
        assert!(matches!(slide, Slide::Settle { to, back: false, .. } if to == 0.0));
    }

    #[test]
    fn batched_microsecond_moves_do_not_explode_release_velocity() {
        let swipe = BackSwipe::default();
        let start = Instant::now();
        swipe.step(TouchPhase::Started, 0.0, 0.0, start, true);

        // Simulate Android TouchResampler flushing 3 historical samples ~20 µs apart per 120 Hz
        // vsync (~8.33 ms), moving at ~960 dp/s (8 dp per frame).
        let mut t = start + Duration::from_millis(8);
        for frame in 0..4u64 {
            let base = start + Duration::from_micros(8_333 * (frame + 1));
            for sub in 0..3u64 {
                t = base + Duration::from_micros(20 * sub);
                swipe.step(TouchPhase::Moved, 8.0 / 3.0, 0.0, t, true);
            }
        }
        // Simulate on_up_or_cancel flushing 2 pending samples 20 µs apart right before lift.
        t += Duration::from_millis(4);
        swipe.step(TouchPhase::Moved, 2.0, 0.0, t, true);
        t += Duration::from_micros(20);
        swipe.step(TouchPhase::Moved, 2.0, 0.0, t, true);
        let Step::Release { offset, velocity } = swipe.step(TouchPhase::Ended, 0.0, 0.0, t + Duration::from_micros(20), true)
        else {
            panic!("expected release");
        };
        assert!((800.0..=1_300.0).contains(&velocity), "expected realistic finger velocity (~960 dp/s), got {velocity}");

        // On a 375 dp phone viewport, the page spring must NOT slam to the end in < 0.1 s.
        let (_, arrived_at_100ms) = motion::PAGE.at(offset, 375.0, velocity, 0.10);
        assert!(!arrived_at_100ms, "settle animation finished way too fast (offset={offset}, velocity={velocity})");
    }

    #[test]
    fn slow_drag_with_batched_moves_does_not_falsely_flick_close() {
        let swipe = BackSwipe::default();
        let start = Instant::now();
        swipe.step(TouchPhase::Started, 0.0, 0.0, start, true);

        // Drag slowly (~240 dp/s = 2 dp per 8.33 ms frame) in 2-sample microsecond bursts.
        let mut t = start;
        for frame in 0..10u64 {
            let base = start + Duration::from_micros(8_333 * (frame + 1));
            swipe.step(TouchPhase::Moved, 1.0, 0.0, base, true);
            t = base + Duration::from_micros(20);
            swipe.step(TouchPhase::Moved, 1.0, 0.0, t, true);
        }
        let Step::Release { offset, velocity } = swipe.step(TouchPhase::Ended, 0.0, 0.0, t + Duration::from_micros(20), true)
        else {
            panic!("expected release");
        };
        assert!(velocity < 400.0, "slow batched drag should not exceed flick threshold: {velocity}");
        assert_eq!(settle(offset, velocity, 375.0), (false, 0.0));
    }

    struct OverlayHost {
        swipe: BackSwipe,
        covered: bool,
        dragged: Vec<f32>,
        released: Option<f32>,
        follow_calls: usize,
    }

    impl OverlayHost {
        fn peek(&self, event: &ScrollEvent, at: Option<Point<Pixels>>) -> Step {
            self.swipe.feed(event, at.is_some())
        }

        fn follow(&mut self, step: Step, cx: &mut Context<Self>) -> bool {
            self.follow_calls += 1;
            match step {
                Step::Pass => false,
                Step::Consume => true,
                Step::Drag(offset) => {
                    self.dragged.push(offset);
                    true
                }
                Step::Release { offset, .. } => {
                    self.released = Some(offset);
                    cx.notify();
                    true
                }
                Step::Cancel(_) => true,
            }
        }
    }

    impl gpui::Render for OverlayHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::{InteractiveElement as _, ParentElement as _, Styled as _, div};
            let swipe = scroll_capture(cx.entity(), Self::peek, Self::follow).absolute().size_full();
            let root = div().relative().size_full().child(div().size_full().child(swipe));
            if self.covered {
                root.child(div().id("overlay").occlude().absolute().right_0().top_0().w(px(200.)).h(px(200.)))
            } else {
                root
            }
        }
    }

    use gpui::TestAppContext;

    #[gpui::test]
    fn a_swipe_starting_over_an_overlay_is_ignored_while_one_already_started_continues(cx: &mut TestAppContext) {
        use gpui::{Modifiers, ScrollSource, size};

        let (host, cx) = cx.add_window_view(|_, _| OverlayHost {
            swipe: BackSwipe::default(),
            covered: true,
            dragged: Vec::new(),
            released: None,
            follow_calls: 0,
        });
        cx.simulate_resize(size(px(600.), px(400.)));
        cx.run_until_parked();

        let touchpad = |at, phase, dx| ScrollEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(dx), px(0.))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
            source: ScrollSource::Pointer,
        };

        // Over the overlay (x = 500, within the right 200x200 occluding box): never starts.
        let over_overlay = point(px(500.), px(100.));
        cx.simulate_event(touchpad(over_overlay, TouchPhase::Started, 0.));
        cx.simulate_event(touchpad(over_overlay, TouchPhase::Moved, 40.));
        cx.simulate_event(touchpad(over_overlay, TouchPhase::Ended, 0.));
        host.read_with(cx, |host, _| {
            assert!(host.dragged.is_empty(), "swipe over overlay must not drag: {:?}", host.dragged);
            assert_eq!(host.released, None);
        });

        // Over the uncovered page (x = 100): starts and drags; even if the pointer moves onto
        // the overlay mid-gesture, it keeps tracking to release.
        let uncovered = point(px(100.), px(100.));
        cx.simulate_event(touchpad(uncovered, TouchPhase::Started, 0.));
        cx.simulate_event(touchpad(uncovered, TouchPhase::Moved, 20.));
        cx.simulate_event(touchpad(over_overlay, TouchPhase::Moved, 30.));
        cx.simulate_event(touchpad(over_overlay, TouchPhase::Ended, 0.));
        host.read_with(cx, |host, _| {
            assert_eq!(host.dragged, vec![20., 50.]);
            assert_eq!(host.released, Some(50.));
        });
    }

    #[gpui::test]
    fn vertical_scroll_through_scroll_capture_never_updates_host(cx: &mut TestAppContext) {
        use gpui::{Modifiers, ScrollSource, size};

        let (host, cx) = cx.add_window_view(|_, _| OverlayHost {
            swipe: BackSwipe::default(),
            covered: false,
            dragged: Vec::new(),
            released: None,
            follow_calls: 0,
        });
        cx.simulate_resize(size(px(600.), px(400.)));
        cx.run_until_parked();

        let vertical = |phase, dy| ScrollEvent {
            position: point(px(100.), px(100.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
            source: ScrollSource::Touch,
        };

        cx.simulate_event(vertical(TouchPhase::Started, 0.));
        for _ in 0..10 {
            cx.simulate_event(vertical(TouchPhase::Moved, -24.));
        }
        cx.simulate_event(vertical(TouchPhase::Ended, 0.));

        host.read_with(cx, |host, _| {
            assert_eq!(host.follow_calls, 0, "vertical scroll must not call Entity::update via scroll_capture");
            assert!(host.dragged.is_empty());
            assert_eq!(host.released, None);
        });
    }

    struct TouchBackHost {
        swipe: BackSwipe,
        open: bool,
        taps: usize,
    }

    impl TouchBackHost {
        fn peek(&self, event: &ScrollEvent, at: Option<Point<Pixels>>) -> Step {
            self.swipe.feed(event, at.is_some() && self.open)
        }

        fn follow(&mut self, step: Step, cx: &mut Context<Self>) -> bool {
            match step {
                Step::Pass => false,
                Step::Consume => true,
                Step::Drag(_) => true,
                Step::Release { offset, velocity } => {
                    if settle(offset, velocity, 400.0).0 {
                        self.open = false;
                    }
                    cx.notify();
                    true
                }
                Step::Cancel(_) => true,
            }
        }
    }

    impl gpui::Render for TouchBackHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ClickEvent, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _, Styled as _, div};
            let swipe = scroll_capture(cx.entity(), Self::peek, Self::follow).absolute().inset_0();
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("underneath-row")
                        .size_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.taps += 1;
                            cx.notify();
                        })),
                )
                .child(swipe)
        }
    }

    #[gpui::test]
    fn first_tap_after_touch_swipe_back_is_not_swallowed_by_fling_momentum(cx: &mut TestAppContext) {
        use gpui::{TouchEvent, TouchId, size};

        let (host, cx) = cx.add_window_view(|_, _| TouchBackHost {
            swipe: BackSwipe::default(),
            open: true,
            taps: 0,
        });
        cx.simulate_resize(size(px(400.), px(800.)));
        cx.run_until_parked();

        let t0 = Instant::now() - Duration::from_millis(80);
        let touch = |id, phase, x, y, ms| TouchEvent {
            id: TouchId(id),
            phase,
            position: point(px(x), px(y)),
            predicted_position: None,
            force: None,
            timestamp: Some(t0 + Duration::from_millis(ms)),
        };

        // Fast horizontal touch swipe right past 1/3 width (~2500 px/s, well above min_fling_velocity).
        cx.simulate_event(touch(1, TouchPhase::Started, 40., 300., 0));
        cx.simulate_event(touch(1, TouchPhase::Moved, 70., 300., 12));
        cx.simulate_event(touch(1, TouchPhase::Moved, 140., 300., 24));
        cx.simulate_event(touch(1, TouchPhase::Moved, 230., 300., 36));
        cx.simulate_event(touch(1, TouchPhase::Ended, 230., 300., 48));
        cx.run_until_parked();

        host.read_with(cx, |host, _| {
            assert!(!host.open, "swipe-back must close the open page");
        });

        // Immediately tap a row underneath — must fire on_click on the very first tap.
        cx.simulate_event(touch(2, TouchPhase::Started, 200., 300., 60));
        cx.simulate_event(touch(2, TouchPhase::Ended, 200., 300., 75));
        cx.run_until_parked();

        host.read_with(cx, |host, _| {
            assert_eq!(host.taps, 1, "first tap after swipe-back must not be swallowed");
        });
    }

    #[gpui::test]
    fn first_tap_after_back_during_vertical_fling_is_not_swallowed(cx: &mut TestAppContext) {
        use gpui::{KeyDownEvent, Keystroke, Modifiers, TouchEvent, TouchId, size};

        let (host, cx) = cx.add_window_view(|_, _| TouchBackHost {
            swipe: BackSwipe::default(),
            open: false,
            taps: 0,
        });
        cx.simulate_resize(size(px(400.), px(800.)));
        cx.run_until_parked();

        let t0 = Instant::now() - Duration::from_millis(80);
        let touch = |id, phase, x, y, ms| TouchEvent {
            id: TouchId(id),
            phase,
            position: point(px(x), px(y)),
            predicted_position: None,
            force: None,
            timestamp: Some(t0 + Duration::from_millis(ms)),
        };

        // Vertical fling (leaves TouchGestureRecognizer momentum active).
        cx.simulate_event(touch(1, TouchPhase::Started, 200., 500., 0));
        cx.simulate_event(touch(1, TouchPhase::Moved, 200., 440., 16));
        cx.simulate_event(touch(1, TouchPhase::Moved, 200., 360., 32));
        cx.simulate_event(touch(1, TouchPhase::Ended, 200., 300., 48));

        // Simulate Android system Back (`escape` keystroke -> Keyboard modality) + cancelling momentum.
        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke { key: "escape".into(), key_char: None, modifiers: Modifiers::default() },
            is_held: false,
            prefer_character_input: false,
        });
        cx.update(|window, _| window.cancel_touch_momentum());

        // First tap on the uncovered view must register immediately.
        cx.simulate_event(touch(2, TouchPhase::Started, 200., 300., 60));
        cx.simulate_event(touch(2, TouchPhase::Ended, 200., 300., 75));
        cx.run_until_parked();

        host.read_with(cx, |host, _| {
            assert_eq!(host.taps, 1, "first tap after back navigation during fling must not be swallowed");
        });
    }

    #[gpui::test]
    fn first_tap_after_unclaimed_horizontal_swipe_is_not_swallowed(cx: &mut TestAppContext) {
        use gpui::{TouchEvent, TouchId, size};

        let (host, cx) = cx.add_window_view(|_, _| TouchBackHost {
            swipe: BackSwipe::default(),
            open: false,
            taps: 0,
        });
        cx.simulate_resize(size(px(400.), px(800.)));
        cx.run_until_parked();

        let t0 = Instant::now() - Duration::from_millis(80);
        let touch = |id, phase, x, y, ms| TouchEvent {
            id: TouchId(id),
            phase,
            position: point(px(x), px(y)),
            predicted_position: None,
            force: None,
            timestamp: Some(t0 + Duration::from_millis(ms)),
        };

        // Horizontal swipe when no page is open and nothing scrolls horizontally.
        cx.simulate_event(touch(1, TouchPhase::Started, 40., 300., 0));
        cx.simulate_event(touch(1, TouchPhase::Moved, 90., 300., 16));
        cx.simulate_event(touch(1, TouchPhase::Moved, 160., 300., 32));
        cx.simulate_event(touch(1, TouchPhase::Ended, 200., 300., 48));

        // Advance one frame so the momentum tick observes no scroll consumer and stops.
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });

        cx.simulate_event(touch(2, TouchPhase::Started, 200., 300., 90));
        cx.simulate_event(touch(2, TouchPhase::Ended, 200., 300., 120));
        cx.run_until_parked();

        host.read_with(cx, |host, _| {
            assert_eq!(host.taps, 1, "first tap after unclaimed horizontal swipe must not be swallowed");
        });
    }
}
