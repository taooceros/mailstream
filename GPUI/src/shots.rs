//! Screenshots of the real window without a display (MS-243), for the screenshots every UI change
//! hands over (AGENTS.md, Release delivery). Built only with the `shots` feature; run it through
//! `scripts/shots.py`, which builds, runs a script light and dark and uploads the pictures.
//!
//! `mailstream-gpui --shots SCRIPT --out DIR [--dark] [--size WxH] [--keep]` opens the app's own
//! window, views and engine in a headless GPUI app (gpui-kit's `HeadlessAppContext`: GPUI's test
//! platform with the system's text system and its offscreen renderer, Metal on the Mac and wgpu on
//! Linux), over the synthetic mail of `seed.rs` in a fresh data folder of its own (the person's
//! mail, sign-ins and settings are never read). Its accounts are signed in to the engine's
//! stand-in provider (`mailstream_engine::shots`, MS-265): marking read, starring, archiving and
//! sending succeed as they would, and nothing fetched replaces the synthetic mail. It needs no
//! compositor, screen or focus: it works with the screen locked, over SSH and beside the person's
//! work. SCRIPT (`-` for stdin) drives it, one command per line (`#` starts a comment; X Y are
//! window points from its top left):
//!
//! ```text
//! wait MS                    let MS of real time pass, drawing frames (data, animations)
//! shot NAME                  save DIR/NAME-light.png (or -dark) of the whole window, at 2x
//! click X Y [right|double] [secondary|shift|alt]...
//!                            move the pointer there and click, holding those keys (secondary:
//!                            ⌘ on the Mac, Ctrl elsewhere)
//! move X Y                   move the pointer there (hover)
//! drag X1 Y1 X2 Y2 [hold]    press at X1 Y1, move to X2 Y2, release (`hold`: keep the button
//!                            down, for a shot mid-drag, until `release`)
//! release                    let go of a held drag where the pointer is
//! scroll X Y DY [DX]         scroll the view under X Y by DY points down (DX right); negative: up
//! key COMBO...               press keys: `escape`, `down`, `enter`, `j`, `shift-tab`,
//!                            `secondary-n` (⌘N on the Mac, Ctrl+N elsewhere)
//! type TEXT                  type the rest of the line into the focused field
//! size W H                   resize the window to W×H points
//! toast TEXT                 show the rest of the line in the app's toast (`controls::toast`)
//! window other|main          drive and shoot another window the app opened (Settings, after
//!                            `key secondary-,`), or the main one again
//! provider google|microsoft answer|hang|fail
//!                            from now on that provider's requests are answered as usual, never
//!                            (each times out after the engine's 30 s) or with a 503: an account
//!                            that can't connect (MS-278); `key secondary-shift-n` asks again
//! freeze                     stop the pages' clock (`frame_clock.rs`, MS-289): a page opened, closed
//!                            or let go from now on stays where it starts, until `film`
//! film NAME COUNT [FPS]      move the stopped clock on 1/FPS s (default 60) a frame and save
//!                            COUNT frames, DIR/NAME-000-light.png on, the first where it stands;
//!                            logs what each frame took to draw and their mean, median and max
//!                            past the first (MS-292)
//! thaw                       run the pages' clock in real time again
//! misspelled FROM TO         the Mac's spell checker finds the focused field's text FROM..TO
//!                            (UTF-16 offsets) misspelled (MS-318; the field opts in to text
//!                            checking, `text_checking.rs`): the field underlines it
//! predict AT TEXT            the Mac predicts the rest of the line, TEXT, at offset AT of the
//!                            focused field, which draws it after the caret if the caret is there;
//!                            a click, keys or typing forget both, as the field checks again
//! accounts mail|all          what the synthetic accounts' sign-ins granted: mail alone, as a
//!                            sign-in now asks (MS-306), or everything (the default); applies
//!                            before the window opens, wherever the line is. Connecting a
//!                            service (Settings, the calendar's notice) is allowed at once
//! ```
//!
//! Every input command then lets `SETTLE` pass. The window opens at `--size` (default 1240×800,
//! the app's) and runs `START` first, so the list has loaded. `--dark` draws the dark palette:
//! the window here has no system appearance, so `design::palette` takes it from here. The window
//! is opaque, as on a desktop that doesn't blur (no Liquid Glass or compositor blur: there is no
//! desktop behind it; `scripts/vhypr.py --blur` shows that on Linux). `--keep` leaves the data
//! folder (printed) for a look at mail.db.
//!
//! Limits: the platform is GPUI's test platform, so what only the real platform does is absent:
//! menus, the Mac's WebView renderer (Blitz draws every body, as on Linux), native file and
//! print dialogs, opening links, drag and drop from outside, the touchpad's momentum.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::test::TestWindowExt;
use gpui::{
    AnyWindowHandle, Bounds, HeadlessAppContext, InputEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, OperatingSystem, Pixels, Point, ScrollDelta, ScrollEvent, TouchPhase, Window,
    WindowAppearance, point, px, size,
};

use crate::text_checking::{self, InlinePrediction, TextAssistance};

mod seed;

/// What the window shows first: the list loaded from the cache, its pictures and the first
/// conversation's preview.
const START: Duration = Duration::from_millis(2_500);
/// After each input: long enough for the app's transitions and an engine answer.
const SETTLE: Duration = Duration::from_millis(600);
const FRAME: Duration = Duration::from_millis(16);

static APPEARANCE: OnceLock<WindowAppearance> = OnceLock::new();

/// The appearance the screenshot mode draws, while it runs (`design::palette`).
pub fn appearance() -> Option<WindowAppearance> {
    APPEARANCE.get().copied()
}

/// The pages' clock while a script films them (`freeze` to `thaw`); None: real time.
static FILM: Mutex<Option<Instant>> = Mutex::new(None);

/// The pages' clock while a script films them (`frame_clock.rs`).
pub fn film_clock() -> Option<Instant> {
    *FILM.lock().expect("the film clock")
}

pub struct Shots {
    script: String,
    out: PathBuf,
    dark: bool,
    size: (f32, f32),
    keep: bool,
}

impl Shots {
    /// The run `--shots` asks for, or None for the app as usual.
    pub fn from_args() -> Option<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if !args.iter().any(|arg| arg == "--shots") {
            return None;
        }
        let usage = "usage: mailstream-gpui --shots SCRIPT --out DIR [--dark] [--size WxH] [--keep]";
        let mut shots = Shots { script: String::new(), out: PathBuf::new(), dark: false, size: (1240., 800.), keep: false };
        let mut script = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--shots" => script = args.next(),
                "--out" => shots.out = args.next().map(PathBuf::from).unwrap_or_else(|| fail(usage)),
                "--dark" => shots.dark = true,
                "--keep" => shots.keep = true,
                "--size" => {
                    let value = args.next().unwrap_or_default();
                    let parsed = value.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                    shots.size = parsed.unwrap_or_else(|| fail(&format!("--size {value}: expected WxH, e.g. 1360x880")));
                }
                other => fail(&format!("unknown argument {other}\n{usage}")),
            }
        }
        let script = script.unwrap_or_else(|| fail(usage));
        shots.script = if script == "-" {
            std::io::read_to_string(std::io::stdin()).unwrap_or_else(|error| fail(&format!("stdin: {error}")))
        } else {
            std::fs::read_to_string(&script).unwrap_or_else(|error| fail(&format!("{script}: {error}")))
        };
        if shots.out.as_os_str().is_empty() {
            fail(usage);
        }
        Some(shots)
    }

    pub fn run(self) {
        // Every command is checked before anything starts.
        let commands: Vec<(usize, Command)> = self
            .script
            .lines()
            .enumerate()
            .filter_map(|(n, line)| {
                let line = line.split_once('#').map_or(line, |(code, _)| code).trim();
                (!line.is_empty()).then(|| (n + 1, Command::parse(line).unwrap_or_else(|error| fail(&format!("line {}: {error}", n + 1)))))
            })
            .collect();
        std::fs::create_dir_all(&self.out).unwrap_or_else(|error| fail(&format!("{}: {error}", self.out.display())));
        APPEARANCE.set(if self.dark { WindowAppearance::Dark } else { WindowAppearance::Light }).ok();
        let home = data_folder();
        let support = mailstream_engine::support_directory();
        let mail_only = commands.iter().rev().find_map(|(_, command)| match command {
            Command::Accounts(mail_only) => Some(*mail_only),
            _ => None,
        });
        seed::write(&support, &mailstream_engine::cache_directory(), mail_only.unwrap_or(false));
        // Before the engine starts: its accounts sign in to a stand-in provider, so changes
        // (marking read, archive, star, send) succeed.
        mailstream_engine::shots::start(&support);

        let platform = gpui::platform::current_platform(true);
        let mut cx = HeadlessAppContext::with_platform(
            platform.text_system(),
            Arc::new(crate::icons::Assets),
            gpui::platform::current_headless_renderer,
        );
        // The engine answers on its own threads, in real time.
        cx.allow_parking();
        let (width, height) = self.size;
        let bounds = Bounds { origin: point(px(0.), px(0.)), size: size(px(width), px(height)) };
        let (window, app) = cx.update(|cx| {
            crate::init(cx);
            // What the Mac's spell checker finds differs between Macs: `misspelled` and `predict`
            // stand for it.
            text_checking::simulate_spell_checker(cx);
            crate::open_window(crate::window_options(bounds), cx)
        });
        let window: AnyWindowHandle = window.into();
        cx.update_window(window, |_, window, cx| {
            let focus = app.read(cx).focus.clone();
            window.activate_window();
            window.focus(&focus, cx);
            cx.activate(true);
        })
        .ok();

        let mut run = Run { cx, window, main: window, appearance: if self.dark { "dark" } else { "light" }, out: self.out, pointer: point(px(0.), px(0.)), assistance: TextAssistance::default() };
        run.pass(START);
        let mut failed = false;
        for (line, command) in commands {
            if let Err(error) = run.command(command) {
                eprintln!("shots: line {line}: {error}");
                failed = true;
                break;
            }
        }
        // What reached the provider other than refused reads: the changes the run made.
        for line in mailstream_engine::shots::log() {
            if !line.trim_start().starts_with("400 ") && !line.ends_with(" /token") {
                eprintln!("shots: provider: {line}");
            }
        }
        if self.keep {
            eprintln!("shots: data folder kept at {}", home.display());
        } else {
            std::fs::remove_dir_all(&home).ok();
        }
        // The engine's threads don't stop by themselves; nothing is left to save.
        std::process::exit(if failed { 1 } else { 0 });
    }
}

fn fail(message: &str) -> ! {
    eprintln!("shots: {message}");
    std::process::exit(2);
}

/// A fresh data folder the engine and the app use instead of the person's (`Engine/src/lib.rs`,
/// `support_directory`, `cache_directory`): HOME on the Mac (Application Support and Caches are
/// under it), the XDG data and cache folders elsewhere. Sign-ins stay in memory
/// (`mailstream_engine::shots`). On the Mac it is under `/tmp`: under the per-user temporary
/// folder (`/var/folders/…/T/`), the engine's control socket in `Library/Application
/// Support/Mailstream` passes the 104-byte limit of a Unix socket's path and never opens (MS-264).
fn data_folder() -> PathBuf {
    #[cfg(target_os = "macos")]
    let parent = PathBuf::from("/tmp");
    #[cfg(not(target_os = "macos"))]
    let parent = std::env::temp_dir();
    let home = parent.join(format!("mailstream-shots-{}", std::process::id()));
    std::fs::remove_dir_all(&home).ok();
    std::fs::create_dir_all(&home).unwrap_or_else(|error| fail(&format!("{}: {error}", home.display())));
    #[cfg(not(target_os = "macos"))]
    {
        // fontconfig finds the person's own fonts and its cache in the same folders; keep them.
        let real = |variable: &str, fallback: &str| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback))
        };
        let link = |from: &std::path::Path, to: &std::path::Path| {
            if from.exists() {
                std::fs::create_dir_all(to.parent().expect("a parent")).ok();
                std::os::unix::fs::symlink(from, to).ok();
            }
        };
        link(&real("XDG_DATA_HOME", ".local/share").join("fonts"), &home.join("data/fonts"));
        link(&real("XDG_CACHE_HOME", ".cache").join("fontconfig"), &home.join("cache/fontconfig"));
    }
    // SAFETY: no other thread runs yet; the engine and GPUI start after this.
    unsafe {
        #[cfg(target_os = "macos")]
        std::env::set_var("HOME", &home);
        #[cfg(not(target_os = "macos"))]
        {
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
        }
    }
    home
}

enum Command {
    Wait(Duration),
    Shot(String),
    Click(Point<Pixels>, MouseButton, usize, Modifiers),
    Move(Point<Pixels>),
    Drag(Point<Pixels>, Point<Pixels>, bool),
    Release,
    Scroll(Point<Pixels>, Point<Pixels>),
    Keys(Vec<String>),
    Type(String),
    Size(f32, f32),
    Toast(String),
    /// The other window (`true`) or the main one.
    Window(bool),
    Provider(mailstream_engine::Provider, mailstream_engine::shots::Outage),
    Freeze,
    Film(String, usize, f32),
    Thaw,
    /// The accounts were granted mail alone (`true`) or everything.
    Accounts(bool),
    Misspelled(Range<usize>),
    Predict(usize, String),
}

impl Command {
    fn parse(line: &str) -> Result<Self, String> {
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let words: Vec<&str> = rest.split_whitespace().collect();
        let number = |at: usize| -> Result<f32, String> {
            words.get(at).ok_or_else(|| format!("{verb}: missing argument {}", at + 1))?.parse().map_err(|_| format!("{verb}: {} is not a number", words[at]))
        };
        let spot = |at: usize| -> Result<Point<Pixels>, String> { Ok(point(px(number(at)?), px(number(at + 1)?))) };
        Ok(match verb {
            "wait" => Command::Wait(Duration::from_millis(number(0)? as u64)),
            "shot" if !rest.is_empty() && !rest.contains(['/', '\\']) => Command::Shot(rest.into()),
            "shot" => return Err("shot: give a file name without folders".into()),
            "click" => {
                let (mut button, mut count, mut modifiers) = (MouseButton::Left, 1, Modifiers::default());
                for word in words.iter().skip(2) {
                    match *word {
                        "right" => button = MouseButton::Right,
                        "double" => count = 2,
                        // ⌘ on the Mac, Ctrl elsewhere, as `key secondary-…`.
                        "secondary" if cfg!(target_os = "macos") => modifiers.platform = true,
                        "secondary" => modifiers.control = true,
                        "shift" => modifiers.shift = true,
                        "alt" => modifiers.alt = true,
                        other => return Err(format!("click: {other} is none of right, double, secondary, shift, alt")),
                    }
                }
                Command::Click(spot(0)?, button, count, modifiers)
            }
            "move" => Command::Move(spot(0)?),
            "drag" => match words.get(4).copied() {
                None => Command::Drag(spot(0)?, spot(2)?, false),
                Some("hold") => Command::Drag(spot(0)?, spot(2)?, true),
                Some(other) => return Err(format!("drag: {other} is not hold")),
            },
            "release" => Command::Release,
            "scroll" => {
                let dx = if words.len() > 3 { number(3)? } else { 0. };
                Command::Scroll(spot(0)?, point(px(dx), px(number(2)?)))
            }
            "key" if !words.is_empty() => {
                let keys: Vec<String> = words.iter().map(|key| platform_keys(key)).collect();
                for key in &keys {
                    let os = if cfg!(target_os = "macos") { OperatingSystem::Mac } else { OperatingSystem::Linux };
                    Keystroke::parse_with_os(key, os).map_err(|error| format!("key {key}: {error}"))?;
                }
                Command::Keys(keys)
            }
            "type" if !rest.is_empty() => Command::Type(rest.into()),
            "misspelled" => Command::Misspelled(number(0)? as usize..number(1)? as usize),
            "predict" => match rest.split_once(char::is_whitespace) {
                Some((_, text)) => Command::Predict(number(0)? as usize, text.trim_start().into()),
                None => return Err("predict: an offset, then the text".into()),
            },
            "size" => Command::Size(number(0)?, number(1)?),
            "toast" if !rest.is_empty() => Command::Toast(rest.into()),
            "window" if rest == "other" || rest == "main" => Command::Window(rest == "other"),
            "window" => return Err("window: other or main".into()),
            "provider" => {
                use mailstream_engine::{Provider, shots::Outage};
                let provider = match words.first().copied() {
                    Some("google") => Provider::Google,
                    Some("microsoft") => Provider::Microsoft,
                    _ => return Err("provider: google or microsoft, then answer, hang or fail".into()),
                };
                let outage = match words.get(1).copied() {
                    Some("answer") => Outage::Answer,
                    Some("hang") => Outage::Hang,
                    Some("fail") => Outage::Fail,
                    _ => return Err("provider: google or microsoft, then answer, hang or fail".into()),
                };
                Command::Provider(provider, outage)
            }
            "accounts" if rest == "mail" || rest == "all" => Command::Accounts(rest == "mail"),
            "accounts" => return Err("accounts: mail or all".into()),
            "freeze" => Command::Freeze,
            "thaw" => Command::Thaw,
            "film" if !words.is_empty() && !words[0].contains(['/', '\\']) => {
                let fps = if words.len() > 2 { number(2)? } else { 60. };
                Command::Film(words[0].into(), number(1)? as usize, fps)
            }
            _ => return Err(format!("unknown command: {line}")),
        })
    }
}

/// `secondary-` as this platform spells it (`main.rs`, `secondary!`).
fn platform_keys(key: &str) -> String {
    let secondary = if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" };
    key.replace("secondary-", secondary)
}

struct Run {
    cx: HeadlessAppContext,
    /// The window the commands drive and shoot; `main` is the app's.
    window: AnyWindowHandle,
    main: AnyWindowHandle,
    appearance: &'static str,
    out: PathBuf,
    pointer: Point<Pixels>,
    /// What `misspelled` and `predict` had the platform find so far.
    assistance: TextAssistance,
}

impl Run {
    /// Lets `time` pass in real time, running the app's tasks and drawing a frame every `FRAME`,
    /// as the platform's display link would: the engine's answers arrive, animations finish. The
    /// app's clock (its timers and animations) moves by the time that really passed, drawing
    /// included, so it keeps pace with the engine's: moved by `FRAME` per frame, it fell behind
    /// wherever drawing takes a while (on the Mac a 5 s toast took 8 s, MS-264).
    fn pass(&mut self, time: Duration) {
        let start = Instant::now();
        let mut moved = start;
        loop {
            self.cx.run_until_parked();
            self.on_window(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            if start.elapsed() >= time {
                break;
            }
            std::thread::sleep(FRAME);
            let now = Instant::now();
            self.cx.advance_clock(now - moved);
            moved = now;
        }
    }

    fn on_window(&mut self, update: impl FnOnce(&mut Window, &mut gpui::App)) {
        self.cx.update_window(self.window, |_, window, cx| update(window, cx)).ok();
    }

    fn event(&mut self, event: impl InputEvent) {
        let input = event.to_platform_input();
        self.on_window(|window, cx| {
            window.dispatch_event(input, cx);
        });
        self.cx.run_until_parked();
        self.on_window(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    fn pointer_to(&mut self, position: Point<Pixels>, pressed_button: Option<MouseButton>) {
        self.pointer = position;
        self.event(MouseMoveEvent { position, pressed_button, modifiers: Modifiers::default() });
    }

    /// Saves what the window shows now as DIR/NAME-<appearance>.png.
    fn save(&mut self, name: &str) -> Result<(), String> {
        let image = self.cx.capture_screenshot(self.window).map_err(|error| format!("capture: {error}"))?;
        let file = self.out.join(format!("{name}-{}.png", self.appearance));
        image.save(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        println!("{}", file.display());
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Wait(time) => return {
                self.pass(time);
                Ok(())
            },
            Command::Provider(provider, outage) => {
                mailstream_engine::shots::set_outage(provider, outage);
                return Ok(());
            }
            // Read before the window opened.
            Command::Accounts(_) => return Ok(()),
            Command::Shot(name) => {
                self.pass(FRAME);
                return self.save(&name);
            }
            Command::Freeze => {
                *FILM.lock().expect("the film clock") = Some(Instant::now());
                return Ok(());
            }
            Command::Thaw => {
                *FILM.lock().expect("the film clock") = None;
                return Ok(());
            }
            Command::Film(name, count, fps) => {
                let mut clock = FILM.lock().expect("the film clock").ok_or("film: freeze the clock first")?;
                let mut drawn = Vec::with_capacity(count);
                for frame in 0..count {
                    if frame > 0 {
                        clock += Duration::from_secs_f32(1. / fps);
                        *FILM.lock().expect("the film clock") = Some(clock);
                    }
                    self.cx.run_until_parked();
                    let started = Instant::now();
                    self.on_window(|window, cx| {
                        window.simulate_next_frame(cx);
                        window.draw(cx).clear(cx);
                    });
                    let ms = started.elapsed().as_secs_f64() * 1000.;
                    eprintln!("shots: film {name}-{frame:03} drawn in {ms:.2} ms");
                    drawn.push(ms);
                    self.save(&format!("{name}-{frame:03}"))?;
                }
                // What the frames cost to draw (build, lay out and paint the window; the GPU's
                // part is not in it), first frame (where the motion starts) left out (MS-292).
                let mut moving = drawn.get(1..).unwrap_or_default().to_vec();
                if !moving.is_empty() {
                    moving.sort_by(f64::total_cmp);
                    let mean = moving.iter().sum::<f64>() / moving.len() as f64;
                    let median = moving[moving.len() / 2];
                    let max = moving[moving.len() - 1];
                    eprintln!("shots: film {name}: {} frames drawn in {mean:.2} ms mean, {median:.2} median, {max:.2} max", moving.len());
                }
                return Ok(());
            }
            Command::Click(position, button, count, modifiers) => {
                // A click may move a field's caret, which then checks again.
                self.assistance = TextAssistance::default();
                self.pointer_to(position, None);
                for click_count in 1..=count {
                    self.event(MouseDownEvent { button, position, modifiers, click_count, first_mouse: false });
                    self.event(MouseUpEvent { button, position, modifiers, click_count });
                }
            }
            Command::Move(position) => self.pointer_to(position, None),
            Command::Drag(from, to, hold) => {
                self.pointer_to(from, None);
                let modifiers = Modifiers::default();
                self.event(MouseDownEvent { button: MouseButton::Left, position: from, modifiers, click_count: 1, first_mouse: false });
                for step in 1..=12 {
                    let along = step as f32 / 12.;
                    self.pointer_to(from + (to - from) * along, Some(MouseButton::Left));
                    self.pass(FRAME);
                }
                if !hold {
                    self.event(MouseUpEvent { button: MouseButton::Left, position: to, modifiers, click_count: 1 });
                }
            }
            Command::Release => {
                let position = self.pointer;
                self.event(MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 });
            }
            Command::Scroll(position, by) => {
                self.pointer_to(position, None);
                // GPUI's deltas move the content: down the page is a negative y.
                let delta = ScrollDelta::Pixels(point(-by.x, -by.y));
                self.event(ScrollEvent { position, delta, modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved, ..Default::default() });
            }
            Command::Keys(keys) => {
                self.assistance = TextAssistance::default();
                for key in keys {
                    self.on_window(|window, cx| window.press(&key, cx));
                    self.cx.run_until_parked();
                }
            }
            Command::Type(text) => {
                self.assistance = TextAssistance::default();
                self.on_window(|window, cx| window.input(&text, cx));
            }
            Command::Misspelled(word) => {
                self.assistance.misspellings.push(word);
                let assistance = self.assistance.clone();
                self.on_window(|window, cx| text_checking::simulate(assistance, window, cx));
            }
            Command::Predict(offset, text) => {
                self.assistance.prediction = Some(InlinePrediction { offset, text });
                let assistance = self.assistance.clone();
                self.on_window(|window, cx| text_checking::simulate(assistance, window, cx));
            }
            Command::Size(width, height) => self.on_window(|window, cx| {
                window.resize(size(px(width), px(height)));
                window.bounds_changed(cx);
            }),
            Command::Toast(text) => self.on_window(|window, cx| {
                use gpui::component::WindowExt;
                let p = crate::design::palette(window.appearance());
                window.push_notification(crate::controls::toast(text.into(), p), cx);
            }),
            Command::Window(other) => {
                let main = self.main;
                self.window = if other {
                    let windows = self.cx.update(|cx| cx.windows());
                    windows.into_iter().filter(|window| *window != main).last().ok_or("window: no other window is open")?
                } else {
                    main
                };
            }
        }
        self.pass(SETTLE);
        Ok(())
    }
}
