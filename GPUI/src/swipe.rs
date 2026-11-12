//! Dragging a page (the reader, a bundle, the calendar's open event and its page) off what it
//! covers to close it, two ways that share one state machine (`Gesture`) and one slide (`Slide`):
//! the two-finger trackpad back swipe (`UI/BackSwipe.swift`), fed from scroll events in the
//! capture phase, and the mouse (MS-266), a press dragged sideways. The page follows the pointer;
//! released past a third of its width or flicked onward it goes off that side and closes,
//! otherwise it springs back (`settle`), on the house curve either way. Gestures that set off
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
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Canvas, Context, DispatchPhase, Entity, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollDelta, ScrollEvent, TouchPhase, Window, canvas,
};

use crate::controls::WINDOW_MOVE_SLOP;
use crate::design::motion;

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

/// The state machine the trackpad and the mouse share: movement starts undecided; past `decision`
/// points it drags the page when mostly sideways, else the gesture is left alone to its end.
struct Gesture {
    state: State,
    decision: f32,
    offset: f32,
    /// The side the gesture set off to (1 right, -1 left); the page never crosses back past 0.
    direction: f32,
    travel: (f32, f32),
    velocity: f32,
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
            velocity: 0.0,
            last: Instant::now(),
        }
    }

    /// The fingers touch or the button goes down; `can_start`: it may drag a page.
    fn start(&mut self, can_start: bool, now: Instant) {
        self.state = if can_start { State::Undecided } else { State::Ignoring };
        self.offset = 0.0;
        self.travel = (0.0, 0.0);
        self.velocity = 0.0;
        self.last = now;
    }

    fn cancel(&mut self) -> Step {
        let tracking = self.state == State::Tracking;
        self.state = State::Idle;
        if tracking { Step::Cancel(self.offset) } else { Step::Pass }
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
                Step::Drag(self.offset)
            }
            State::Tracking => {
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

    /// The fingers lift or the button goes up; `coast`: momentum events follow (the trackpad's).
    fn lifted(&mut self, now: Instant, coast: bool) -> Step {
        match self.state {
            State::Tracking => {
                self.state = if coast { State::Coasting } else { State::Idle };
                let velocity = if now.duration_since(self.last) > STILL { 0.0 } else { self.velocity };
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
pub struct BackSwipe {
    gesture: Gesture,
    pub preferences: Preferences,
}

impl Default for BackSwipe {
    fn default() -> Self {
        Self { gesture: Gesture::new(SWIPE_DECISION), preferences: Preferences::default() }
    }
}

impl BackSwipe {
    /// `can_start`: whether a gesture beginning with this event may become a back swipe.
    pub fn feed(&mut self, event: &ScrollEvent, can_start: bool) -> Step {
        // Mouse wheels scroll by lines; only trackpads send pixel deltas.
        let ScrollDelta::Pixels(delta) = event.delta else { return Step::Pass };
        let (raw_dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        // Fingers to the right, whichever way the user's scrolling direction is set.
        let dx = if self.preferences.natural { raw_dx } else { -raw_dx };
        self.step(event.touch_phase, dx, dy, Instant::now(), can_start && self.preferences.enabled)
    }

    fn step(&mut self, phase: TouchPhase, dx: f32, dy: f32, now: Instant, can_start: bool) -> Step {
        match phase {
            // `Started` also stands for "may begin", which carries no movement.
            TouchPhase::Started => {
                self.gesture.start(can_start, now);
                Step::Pass
            }
            TouchPhase::Cancelled => self.gesture.cancel(),
            // gpui reports momentum as `Moved` with no touch: after `Ended` they are the flick.
            TouchPhase::Moved => self.gesture.moved(dx, dy, now),
            TouchPhase::Ended => self.gesture.lifted(now, true),
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

    /// Where a page `width` wide sits now, and whether its motion has arrived.
    fn place(&self, width: f32) -> (f32, bool) {
        let motion = motion::page();
        match *self {
            Slide::Rest => (0., true),
            Slide::Drag(offset) => (offset, true),
            Slide::Opening(start) => motion.at(width, 0., 0., since(start)),
            Slide::Settle { from, to, velocity, start, .. } => motion.at(from, to, velocity, since(start)),
        }
    }

    /// How far right of its place a page `width` wide sits now, without advancing anything.
    pub fn offset(&self, width: f32) -> f32 {
        self.place(width).0
    }

    /// Where a page `width` wide sits, called while drawing it: asks for the next frame while it
    /// moves and comes to rest on arrival. None once it has slid off: the page closes.
    pub fn advance(&mut self, width: f32, window: &mut Window) -> Option<f32> {
        let (offset, arrived) = self.place(width);
        if !arrived {
            window.request_animation_frame();
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

/// Now, for the slides: in the screenshot mode the clock a script films them by while it does
/// (`shots.rs`, `freeze`), so each frame shows the moment it stands for.
fn now() -> Instant {
    #[cfg(feature = "shots")]
    if let Some(now) = crate::shots::film_clock() {
        return now;
    }
    Instant::now()
}

/// Seconds since `start` by the slides' clock.
fn since(start: Instant) -> f32 {
    now().saturating_duration_since(start).as_secs_f32()
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

/// Hands `host` the scroll events over the element it is drawn as (laid over a page or the
/// pages), in the capture phase, before any scroll view under them scrolls with them;
/// `feed` answers true when the swipe took one.
pub fn scroll_capture<T: 'static>(host: Entity<T>, feed: fn(&mut T, &ScrollEvent, &mut Context<T>) -> bool) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx: &mut App| {
                if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                    return;
                }
                if host.update(cx, |this, cx| feed(this, event, cx)) {
                    cx.stop_propagation();
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
}
