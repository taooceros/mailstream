//! The animations' clock (MS-293): the slides (`swipe::Slide`) and the calendar's paging read the
//! time of the frame they are drawn for here, so they move by the same amount every frame.
//!
//! Read with `Instant::now()` while drawing, a frame's time carried whatever the main thread did
//! before it got there: the display link's callback comes a little late or early, and the views
//! drawn first take a varying while, so at 60 Hz and 120 Hz the steps between frames came out
//! a few milliseconds uneven, a visible judder in a moving page. Each window's clock instead
//! puts its frames on the display's refresh grid (`Window::refresh_interval`): a frame's time is
//! the grid point a whole number of refreshes after the one before, counted from when GPUI Fast's
//! platform asked for it (`Window::frame_signal_at`, our patch in `GPUI/vendor/gpui-fast`; the
//! time of drawing where the platform gives none). Steps are then one refresh apart, or a whole
//! number of them where frames were dropped; the clock starts again from the moment's time after
//! a pause.
//!
//! A motion starts on the first frame drawn after it was asked for, as if it had begun one
//! refresh before, so that frame already moves one step (`elapsed`): an open that took a while
//! to lay out does not jump ahead by the time it took.
//!
//! In the screenshot mode the clock is the one a script films by (`shots.rs`, `freeze`), so each
//! frame shows the moment it stands for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Window, WindowId};

/// Taken for the refresh where the display reports none.
const FALLBACK_REFRESH: Duration = Duration::from_nanos(16_666_667);
/// A gap this long between frames a clock drew for is a pause: it starts again from that time.
const PAUSE: Duration = Duration::from_millis(250);

/// One window's frames on its display's refresh grid.
#[derive(Default)]
struct FrameClock {
    /// The grid: the time of the frame it started from, and the refresh.
    origin: Option<(Instant, Duration)>,
    /// The latest frame: its index on the grid, its time, and what told it apart (the platform's
    /// request time).
    index: u64,
    frame: Option<Instant>,
    signal: Option<Instant>,
    /// The frame before the latest.
    previous: Option<Instant>,
}

impl FrameClock {
    /// The time of the frame drawn now, `raw` the platform's request time (or the time of
    /// drawing) and `signal` the request time that tells frames apart, None where there is none.
    fn tick(&mut self, raw: Instant, signal: Option<Instant>, refresh: Option<Duration>) -> Instant {
        let refresh = refresh.filter(|refresh| !refresh.is_zero()).unwrap_or(FALLBACK_REFRESH);
        if let Some(frame) = self.frame {
            // The same frame read again: the same request, or with none, under half a refresh on.
            let same = match signal {
                Some(_) => signal == self.signal,
                None => raw.saturating_duration_since(frame) < refresh / 2,
            };
            if same {
                return frame;
            }
        }
        let resume = match (self.origin, self.frame) {
            (Some((_, grid)), Some(frame)) => grid != refresh || raw < frame || raw - frame > PAUSE,
            _ => true,
        };
        self.previous = self.frame;
        self.signal = signal;
        let frame = if resume {
            self.origin = Some((raw, refresh));
            self.index = 0;
            raw
        } else {
            let (origin, _) = self.origin.expect("a grid");
            // The refreshes since the grid's start, never fewer than one more than the last frame:
            // a request that came late stays on the grid, and the next frame steps on from it.
            let on_grid = ((raw - origin).as_secs_f64() / refresh.as_secs_f64()).round() as u64;
            self.index = on_grid.max(self.index + 1);
            origin + refresh * self.index as u32
        };
        self.frame = Some(frame);
        frame
    }

    /// Settles `start` (when a motion was asked for) against the frame drawn now, `frame`: a
    /// start after the frame before it is the motion's first frame, so it is taken to be one
    /// refresh before this one.
    fn begin(&self, start: &mut Instant, frame: Instant) {
        if self.previous.is_none_or(|previous| *start > previous) {
            let refresh = self.origin.map_or(FALLBACK_REFRESH, |(_, refresh)| refresh);
            *start = frame.checked_sub(refresh).unwrap_or(frame);
        }
    }
}

thread_local! {
    static CLOCKS: RefCell<HashMap<WindowId, FrameClock>> = RefCell::new(HashMap::new());
}

/// Now, for a motion's start and for reading where it is outside drawing: in the screenshot mode
/// the clock a script films by.
pub fn now() -> Instant {
    #[cfg(feature = "shots")]
    if let Some(now) = crate::shots::film_clock() {
        return now;
    }
    Instant::now()
}

/// Seconds since `start` by `now`, for reading where a motion is outside drawing.
pub fn since(start: Instant) -> f32 {
    now().saturating_duration_since(start).as_secs_f32()
}

/// Seconds a motion that started at `start` has run by the frame `window` is drawing, called
/// while drawing it; settles `start` on the motion's first frame (`FrameClock::begin`).
pub fn elapsed(start: &mut Instant, window: &Window, cx: &App) -> f32 {
    #[cfg(feature = "shots")]
    if let Some(now) = crate::shots::film_clock() {
        return now.saturating_duration_since(*start).as_secs_f32();
    }
    let signal = window.frame_signal_at();
    let raw = signal.unwrap_or_else(Instant::now);
    let refresh = window.refresh_interval(cx);
    CLOCKS.with_borrow_mut(|clocks| {
        let clock = clocks.entry(window.window_handle().window_id()).or_default();
        let frame = clock.tick(raw, signal, refresh);
        clock.begin(start, frame);
        frame.saturating_duration_since(*start).as_secs_f32()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator for the jitter (no `rand` in the crate).
    struct Jitter(u64);

    impl Jitter {
        /// Somewhere in 0..`max`.
        fn next(&mut self, max: Duration) -> Duration {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            max.mul_f64((self.0 >> 11) as f64 / (1u64 << 53) as f64)
        }
    }

    fn steps(times: &[Instant]) -> Vec<f64> {
        times.windows(2).map(|pair| (pair[1] - pair[0]).as_secs_f64() * 1000.).collect()
    }

    fn spread(steps: &[f64]) -> f64 {
        let max = steps.iter().copied().fold(f64::MIN, f64::max);
        let min = steps.iter().copied().fold(f64::MAX, f64::min);
        max - min
    }

    /// Frames at 60 Hz and 120 Hz whose display link calls come up to 2 ms late and whose views
    /// take up to 3 ms more before the slide is drawn: by the time of drawing the steps were up
    /// to 5 ms uneven; on the clock they are one refresh each.
    #[test]
    fn frames_step_one_refresh_apart_whatever_the_main_thread_did() {
        for hz in [60u32, 120] {
            let refresh = Duration::from_secs(1) / hz;
            let start = Instant::now();
            let mut jitter = Jitter(u64::from(hz));
            let (mut drawing, mut ticked, mut signalled) = (Vec::new(), Vec::new(), Vec::new());
            let (mut by_signal, mut by_drawing) = (FrameClock::default(), FrameClock::default());
            for frame in 0..120u32 {
                let vsync = start + refresh * frame;
                let signal = vsync + jitter.next(Duration::from_millis(2));
                let drawn = signal + jitter.next(Duration::from_millis(3));
                drawing.push(drawn);
                signalled.push(by_signal.tick(signal, Some(signal), Some(refresh)));
                // Read twice in a frame (two views): the same time.
                assert_eq!(by_signal.tick(signal, Some(signal), Some(refresh)), *signalled.last().unwrap());
                ticked.push(by_drawing.tick(drawn, None, Some(refresh)));
            }
            let ms = refresh.as_secs_f64() * 1000.;
            let raw = steps(&drawing);
            eprintln!(
                "{hz} Hz: time of drawing steps {:.2}..{:.2} ms (spread {:.2}); clock steps spread {:.4} (by request), {:.4} (by drawing)",
                raw.iter().copied().fold(f64::MAX, f64::min),
                raw.iter().copied().fold(f64::MIN, f64::max),
                spread(&raw),
                spread(&steps(&signalled)),
                spread(&steps(&ticked)),
            );
            assert!(spread(&raw) > 2., "the jitter shows in the time of drawing");
            for step in steps(&signalled).into_iter().chain(steps(&ticked)) {
                assert!((step - ms).abs() < 0.001, "{hz} Hz: a step of {step} ms, not {ms}");
            }
        }
    }

    /// A frame dropped is two refreshes; a request that came half a refresh late stays on the
    /// grid, and the frame after it still steps one refresh on.
    #[test]
    fn dropped_and_late_frames_stay_on_the_grid() {
        let refresh = Duration::from_secs(1) / 120;
        let start = Instant::now();
        let mut clock = FrameClock::default();
        let at = |frame: u32, late: Duration| start + refresh * frame + late;
        let times: Vec<Instant> = [(0, 0.), (1, 0.), (3, 0.), (4, 0.6), (5, 0.), (6, 0.)]
            .into_iter()
            .map(|(frame, late)| {
                let signal = at(frame, refresh.mul_f64(late));
                clock.tick(signal, Some(signal), Some(refresh))
            })
            .collect();
        let refreshes: Vec<u32> = times.windows(2).map(|pair| ((pair[1] - pair[0]).as_secs_f64() / refresh.as_secs_f64()).round() as u32).collect();
        assert_eq!(refreshes, [1, 2, 2, 1, 1]);
        for pair in times.windows(2) {
            let step = (pair[1] - pair[0]).as_secs_f64() / refresh.as_secs_f64();
            assert!((step - step.round()).abs() < 1e-6, "a step of {step} refreshes");
        }
    }

    /// After a pause the clock starts from the moment's time; a motion asked for since the last
    /// frame moves one refresh on its first frame, however long that frame took to come.
    #[test]
    fn a_motion_moves_one_refresh_on_its_first_frame() {
        let refresh = Duration::from_secs(1) / 120;
        let mut clock = FrameClock::default();
        let idle = Instant::now();
        clock.tick(idle, Some(idle), Some(refresh));
        // A click a second later; the open takes 20 ms to lay out before its first frame.
        let click = idle + Duration::from_secs(1);
        let mut start = click;
        let first = click + Duration::from_millis(20);
        let frame = clock.tick(first, Some(first), Some(refresh));
        assert_eq!(frame, first, "after a pause the clock starts again");
        clock.begin(&mut start, frame);
        assert_eq!(frame - start, refresh);
        let second = first + refresh + Duration::from_millis(1);
        let frame = clock.tick(second, Some(second), Some(refresh));
        clock.begin(&mut start, frame);
        assert_eq!(frame - start, refresh * 2, "the start stays once settled");
    }
}
