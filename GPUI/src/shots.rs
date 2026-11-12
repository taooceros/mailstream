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
//! click X Y [right|double]   move the pointer there and click
//! move X Y                   move the pointer there (hover)
//! drag X1 Y1 X2 Y2           press at X1 Y1, move to X2 Y2, release
//! scroll X Y DY [DX]         scroll the view under X Y by DY points down (DX right); negative: up
//! key COMBO...               press keys: `escape`, `down`, `enter`, `j`, `shift-tab`,
//!                            `secondary-n` (⌘N on the Mac, Ctrl+N elsewhere)
//! type TEXT                  type the rest of the line into the focused field
//! size W H                   resize the window to W×H points
//! toast TEXT                 show the rest of the line in the app's toast (`controls::toast`)
//! window other|main          drive and shoot another window the app opened (Settings, after
//!                            `key secondary-,`), or the main one again
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

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gpui::test::TestWindowExt;
use gpui::{
    AnyWindowHandle, Bounds, HeadlessAppContext, InputEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, OperatingSystem, Pixels, Point, ScrollDelta, ScrollEvent, TouchPhase, Window,
    WindowAppearance, point, px, size,
};

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
        seed::write(&support, &mailstream_engine::cache_directory());
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

        let mut run = Run { cx, window, main: window, appearance: if self.dark { "dark" } else { "light" }, out: self.out, pointer: point(px(0.), px(0.)) };
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
    Click(Point<Pixels>, MouseButton, usize),
    Move(Point<Pixels>),
    Drag(Point<Pixels>, Point<Pixels>),
    Scroll(Point<Pixels>, Point<Pixels>),
    Keys(Vec<String>),
    Type(String),
    Size(f32, f32),
    Toast(String),
    /// The other window (`true`) or the main one.
    Window(bool),
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
                let (button, count) = match words.get(2).copied() {
                    None => (MouseButton::Left, 1),
                    Some("right") => (MouseButton::Right, 1),
                    Some("double") => (MouseButton::Left, 2),
                    Some(other) => return Err(format!("click: {other} is neither right nor double")),
                };
                Command::Click(spot(0)?, button, count)
            }
            "move" => Command::Move(spot(0)?),
            "drag" => Command::Drag(spot(0)?, spot(2)?),
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
            "size" => Command::Size(number(0)?, number(1)?),
            "toast" if !rest.is_empty() => Command::Toast(rest.into()),
            "window" if rest == "other" || rest == "main" => Command::Window(rest == "other"),
            "window" => return Err("window: other or main".into()),
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

    fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Wait(time) => return {
                self.pass(time);
                Ok(())
            },
            Command::Shot(name) => {
                self.pass(FRAME);
                let image = self.cx.capture_screenshot(self.window).map_err(|error| format!("capture: {error}"))?;
                let file = self.out.join(format!("{name}-{}.png", self.appearance));
                image.save(&file).map_err(|error| format!("{}: {error}", file.display()))?;
                println!("{}", file.display());
                return Ok(());
            }
            Command::Click(position, button, count) => {
                self.pointer_to(position, None);
                for click_count in 1..=count {
                    let modifiers = Modifiers::default();
                    self.event(MouseDownEvent { button, position, modifiers, click_count, first_mouse: false });
                    self.event(MouseUpEvent { button, position, modifiers, click_count });
                }
            }
            Command::Move(position) => self.pointer_to(position, None),
            Command::Drag(from, to) => {
                self.pointer_to(from, None);
                let modifiers = Modifiers::default();
                self.event(MouseDownEvent { button: MouseButton::Left, position: from, modifiers, click_count: 1, first_mouse: false });
                for step in 1..=12 {
                    let along = step as f32 / 12.;
                    self.pointer_to(from + (to - from) * along, Some(MouseButton::Left));
                    self.pass(FRAME);
                }
                self.event(MouseUpEvent { button: MouseButton::Left, position: to, modifiers, click_count: 1 });
            }
            Command::Scroll(position, by) => {
                self.pointer_to(position, None);
                // GPUI's deltas move the content: down the page is a negative y.
                let delta = ScrollDelta::Pixels(point(-by.x, -by.y));
                self.event(ScrollEvent { position, delta, modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved, ..Default::default() });
            }
            Command::Keys(keys) => {
                for key in keys {
                    self.on_window(|window, cx| window.press(&key, cx));
                    self.cx.run_until_parked();
                }
            }
            Command::Type(text) => self.on_window(|window, cx| window.input(&text, cx)),
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
