//! The frame measurement `scripts/bench.sh gpui` runs (AGENTS.md, "Performance: 120 fps"). With
//! `MAILSTREAM_BENCH=scroll` the window scrolls its conversation list by itself, logs every frame
//! to `MAILSTREAM_BENCH_OUT` and quits. `MAILSTREAM_BENCH=pages` instead slides the inbox's first
//! bundle over the list and back off, again and again (MS-289): a bundle changes no mail, so it
//! runs over the person's own.

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
/// `pages`: a bundle opens every `TURN` and goes back `TURN / 2` later, for `LEGS` × `LEG`.
const TURN: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Scroll,
    Pages,
}

/// What the `pages` scenario asks of the window this frame (`Bench::take_turn`).
#[derive(Clone, Copy)]
pub enum Turn {
    Open,
    Back,
}

pub struct Bench {
    scenario: Scenario,
    /// Where the log goes; taken when it is written.
    out: Option<PathBuf>,
    launched: Instant,
    started: Option<Instant>,
    /// Each frame's start and work in ms since launch; the paint at the end of the window fills
    /// in the work.
    frames: Rc<RefCell<Vec<(f64, f64)>>>,
    /// `pages`: the half turns taken so far, and what this frame asks for.
    turns: u32,
    turn: Option<Turn>,
}

fn scenario() -> Option<Scenario> {
    match std::env::var("MAILSTREAM_BENCH").ok()?.as_str() {
        "scroll" => Some(Scenario::Scroll),
        "pages" => Some(Scenario::Pages),
        _ => None,
    }
}

/// Whether the environment asks for a measurement (`MAILSTREAM_BENCH=scroll` or `pages`).
pub fn requested() -> bool {
    scenario().is_some()
}

impl Bench {
    /// The measurement the environment asks for, waking the window once the app has settled.
    pub fn from_env(cx: &mut Context<MailApp>) -> Option<Self> {
        let scenario = scenario()?;
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
        Some(Self {
            scenario,
            out: Some(out),
            launched: Instant::now(),
            started: None,
            frames: Rc::default(),
            turns: 0,
            turn: None,
        })
    }

    /// What the `pages` scenario asks of the window this frame, once.
    pub fn take_turn(&mut self) -> Option<Turn> {
        self.turn.take()
    }

    /// Called as the window renders: scrolls the list (or asks for the next half turn) for this
    /// frame and returns the element to paint last, which ends the frame's work.
    pub fn frame(
        &mut self,
        list: &ListState,
        window: &mut Window,
        cx: &mut Context<MailApp>,
    ) -> Option<AnyElement> {
        let now = Instant::now();
        let max = f32::from(list.max_offset_for_scrollbar().y);
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
        if self.scenario == Scenario::Pages {
            let half = (elapsed.as_secs_f32() / (TURN.as_secs_f32() / 2.)) as u32;
            if half >= self.turns {
                self.turn = Some(if half % 2 == 0 { Turn::Open } else { Turn::Back });
                self.turns = half + 1;
            }
        } else {
            // A triangle wave: down on even legs, up on odd ones.
            let leg = elapsed.as_secs_f32() / LEG.as_secs_f32();
            let along = leg.fract();
            let depth = if (leg as u32) % 2 == 0 { along } else { 1. - along };
            // The list measures rows as they come into view, so it moves by the difference.
            let at = -f32::from(list.scroll_px_offset_for_scrollbar().y);
            list.scroll_by(px(depth * DISTANCE - at));
        }
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
        let mut log = match self.scenario {
            Scenario::Scroll => format!(
                "# frontend=gpui scenario=scroll settle={}s legs={}x{}s distance={}px\n",
                SETTLE.as_secs(),
                LEGS,
                LEG.as_secs(),
                DISTANCE
            ),
            Scenario::Pages => format!(
                "# frontend=gpui scenario=pages settle={}s for={}s turn={}ms\n",
                SETTLE.as_secs(),
                (LEG * LEGS).as_secs(),
                TURN.as_millis()
            ),
        };
        for (start, work) in self.frames.borrow().iter() {
            writeln!(log, "{start:.3}\t{work:.3}").ok();
        }
        if let Err(error) = std::fs::write(&out, log) {
            eprintln!("bench: cannot write {}: {error}", out.display());
        }
        cx.quit();
    }
}
