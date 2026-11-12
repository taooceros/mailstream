//! The frame measurement `scripts/bench.sh gpui` runs (AGENTS.md, "Performance: 120 fps"). With
//! `MAILSTREAM_BENCH=scroll` the window scrolls its conversation list by itself, logs every frame
//! to `MAILSTREAM_BENCH_OUT` and quits.

use std::cell::RefCell;
use std::fmt::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, IntoElement, ListState, Styled, Window, canvas, px};

use crate::state::MailApp;

/// The scenario every front end runs: after `SETTLE`, `LEGS` legs of `LEG` each, down `DISTANCE`
/// and back, the offset set from the time each frame.
const SETTLE: Duration = Duration::from_secs(12);
const LEGS: u32 = 8;
const LEG: Duration = Duration::from_secs(3);
const DISTANCE: f32 = 4000.;

pub struct Bench {
    /// Where the log goes; taken when it is written.
    out: Option<PathBuf>,
    launched: Instant,
    started: Option<Instant>,
    /// Each frame's start and work in ms since launch; the paint at the end of the window fills
    /// in the work.
    frames: Rc<RefCell<Vec<(f64, f64)>>>,
}

impl Bench {
    /// The measurement the environment asks for, waking the window once the app has settled.
    pub fn from_env(cx: &mut Context<MailApp>) -> Option<Self> {
        if std::env::var("MAILSTREAM_BENCH").ok()? != "scroll" {
            return None;
        }
        let out = PathBuf::from(std::env::var_os("MAILSTREAM_BENCH_OUT")?);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            // Until the list has rows to scroll.
            while this
                .update(cx, |app, cx| {
                    cx.notify();
                    app.bench.as_ref().is_some_and(|bench| bench.started.is_none())
                })
                .unwrap_or(false)
            {
                cx.background_executor().timer(Duration::from_millis(500)).await;
            }
        })
        .detach();
        Some(Self { out: Some(out), launched: Instant::now(), started: None, frames: Rc::default() })
    }

    /// Called as the window renders: scrolls the list for this frame and returns the element to
    /// paint last, which ends the frame's work.
    pub fn frame(
        &mut self,
        list: &ListState,
        window: &mut Window,
        cx: &mut Context<MailApp>,
    ) -> Option<AnyElement> {
        let now = Instant::now();
        let max = f32::from(list.max_offset_for_scrollbar().height);
        let started = match self.started {
            Some(started) => started,
            None if now - self.launched >= SETTLE && max > 0. => *self.started.insert(now),
            None => return None,
        };
        let elapsed = now - started;
        if elapsed >= LEG * LEGS {
            self.finish(cx);
            return None;
        }
        // A triangle wave: down on even legs, up on odd ones.
        let leg = elapsed.as_secs_f32() / LEG.as_secs_f32();
        let along = leg.fract();
        let depth = if (leg as u32) % 2 == 0 { along } else { 1. - along };
        // The list measures rows as they come into view, so it moves by the difference.
        let at = -f32::from(list.scroll_px_offset_for_scrollbar().y);
        list.scroll_by(px(depth * DISTANCE - at));
        window.request_animation_frame();

        let start = (now - self.launched).as_secs_f64() * 1000.;
        self.frames.borrow_mut().push((start, 0.));
        let frames = self.frames.clone();
        let launched = self.launched;
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, _, _| {
                    if let Some(frame) = frames.borrow_mut().last_mut() {
                        frame.1 = (launched.elapsed().as_secs_f64() * 1000.) - frame.0;
                    }
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    fn finish(&mut self, cx: &mut Context<MailApp>) {
        let Some(out) = self.out.take() else { return };
        let mut log = format!(
            "# frontend=gpui scenario=scroll settle={}s legs={}x{}s distance={}px\n",
            SETTLE.as_secs(),
            LEGS,
            LEG.as_secs(),
            DISTANCE
        );
        for (start, work) in self.frames.borrow().iter() {
            writeln!(log, "{start:.3}\t{work:.3}").ok();
        }
        if let Err(error) = std::fs::write(&out, log) {
            eprintln!("bench: cannot write {}: {error}", out.display());
        }
        cx.quit();
    }
}
