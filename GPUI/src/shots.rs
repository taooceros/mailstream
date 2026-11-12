//! Screenshots and scripted input without the screen (MS-243), for `scripts/shots.py`. A build with
//! the `shots` feature started with `MAILSTREAM_SHOTS=<folder>` reads commands from `<folder>/cmd`
//! and answers in `<folder>/done`: it renders the window to PNGs with GPUI's `render_to_image`
//! (the frame as the GPU draws it, without presenting it) and feeds the window input as the
//! platform would. Nothing goes through the screen, so it works with the Mac locked, the window
//! behind others or the user at work, and never moves the user's pointer or keyboard focus.
//!
//! Commands, one per line (coordinates in the window's points, origin top left):
//! - `size W H`: resizes the window's content to W×H.
//! - `move X Y`: the pointer moves there (hover).
//! - `click X Y [right|middle] [2|3] [cmd|shift|alt|ctrl]...`: a click there.
//! - `scroll X Y DY [DX]`: a scroll of DY (DX) points over X Y; positive DY moves the content down,
//!   toward its top, as a wheel turned up does.
//! - `key COMBO...`: keystrokes in GPUI's form (`escape`, `down`, `r`, `cmd-shift-r`; `+` works as
//!   well as `-`).
//! - `type TEXT`: the rest of the line typed into the focused field.
//! - `wait MS`: a pause, for an animation or a load to finish.
//! - `shot FILE`: a PNG of the window at its scale (2× on a Retina Mac), once two renders 250 ms
//!   apart agree (at most 8 s, so content still arriving lands in it).
//! - `appearance light|dark` (Mac): the app's appearance from then on.
//! - `info`: the window's size and scale.
//! - `quit`.
//!
//! `done` holds a line per command, `<command> => ok` or `<command> => error: …`; `ready` appears in
//! the folder once the window is open. The release app is built without the feature: GPUI's
//! `render_to_image` comes with its `test-support` feature, which shipping builds leave out.

/// The control folder the environment names, when it asks for this mode.
pub fn requested() -> bool {
    std::env::var_os("MAILSTREAM_SHOTS").is_some()
}

#[cfg(not(feature = "shots"))]
pub fn start(_: gpui::AnyWindowHandle, _: &mut gpui::App) {
    if requested() {
        eprintln!("MAILSTREAM_SHOTS needs a build with `--features shots` (scripts/shots.py builds one)");
    }
}

#[cfg(feature = "shots")]
pub use control::start;

#[cfg(feature = "shots")]
mod control {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use gpui::{
        AnyWindowHandle, App, AsyncApp, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, PlatformInput, ScrollDelta, ScrollEvent, Window, point, px, size,
    };

    /// How often the folder is checked for a command file.
    const POLL: Duration = Duration::from_millis(100);
    /// A shot waits for two renders this far apart to agree, for at most `SETTLE_LIMIT`.
    const SETTLE_STEP: Duration = Duration::from_millis(250);
    const SETTLE_LIMIT: Duration = Duration::from_secs(8);

    pub fn start(window: AnyWindowHandle, cx: &mut App) {
        let Some(folder) = std::env::var_os("MAILSTREAM_SHOTS").map(PathBuf::from) else { return };
        if let Err(error) = std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(folder.join("ready"), "")) {
            eprintln!("shots: cannot use {}: {error}", folder.display());
            return;
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let request = folder.join("cmd");
                let Ok(script) = std::fs::read_to_string(&request) else { continue };
                let _ = std::fs::remove_file(&request);
                let mut answer = String::new();
                let mut quit = false;
                for line in script.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
                    let result = match run(line, window, cx).await {
                        Ok(Step::Done(note)) => note.map_or_else(|| "ok".to_string(), |note| format!("ok {note}")),
                        Ok(Step::Quit) => {
                            quit = true;
                            "ok".to_string()
                        }
                        Err(error) => format!("error: {error}"),
                    };
                    answer.push_str(&format!("{line} => {result}\n"));
                    if quit {
                        break;
                    }
                }
                // Written whole, then renamed, so the reader never sees half an answer.
                let partial = folder.join("done.partial");
                if std::fs::write(&partial, answer).and_then(|()| std::fs::rename(&partial, folder.join("done"))).is_err() {
                    eprintln!("shots: cannot answer in {}", folder.display());
                }
                if quit {
                    cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
        .detach();
    }

    enum Step {
        Done(Option<String>),
        Quit,
    }

    async fn run(line: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<Step, String> {
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let words: Vec<&str> = rest.split_whitespace().collect();
        let number = |i: usize| -> Result<f32, String> {
            let word = words.get(i).ok_or_else(|| format!("{verb} needs more arguments"))?;
            word.parse().map_err(|_| format!("{word} is not a number"))
        };
        let note = match verb {
            "wait" => {
                cx.background_executor().timer(Duration::from_millis(number(0)? as u64)).await;
                None
            }
            "shot" => {
                let file = rest.trim();
                if file.is_empty() {
                    return Err("shot needs a file".into());
                }
                Some(shot(Path::new(file), window, cx).await?)
            }
            "quit" => return Ok(Step::Quit),
            "size" => {
                let (width, height) = (number(0)?, number(1)?);
                on_window(window, cx, |window, _| {
                    window.resize(size(px(width), px(height)));
                    Ok(None)
                })?
            }
            "info" => on_window(window, cx, |window, _| {
                let viewport = window.viewport_size();
                Ok(Some(format!("{}x{} scale {}", f32::from(viewport.width), f32::from(viewport.height), window.scale_factor())))
            })?,
            "move" => {
                let position = point(px(number(0)?), px(number(1)?));
                on_window(window, cx, |window, cx| {
                    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, ..Default::default() }), cx);
                    Ok(None)
                })?
            }
            "click" => {
                let position = point(px(number(0)?), px(number(1)?));
                let (mut button, mut count, mut modifiers) = (MouseButton::Left, 1, Modifiers::default());
                for word in &words[2..] {
                    match *word {
                        "left" => button = MouseButton::Left,
                        "right" => button = MouseButton::Right,
                        "middle" => button = MouseButton::Middle,
                        "cmd" => modifiers.platform = true,
                        "shift" => modifiers.shift = true,
                        "alt" => modifiers.alt = true,
                        "ctrl" => modifiers.control = true,
                        word => count = word.parse().map_err(|_| format!("click doesn't take {word}"))?,
                    }
                }
                on_window(window, cx, |window, cx| {
                    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, modifiers, ..Default::default() }), cx);
                    // A double click is the second of two, as the platform reports it.
                    for click_count in 1..=count {
                        let down = MouseDownEvent { button, position, modifiers, click_count, first_mouse: false };
                        window.dispatch_event(PlatformInput::MouseDown(down), cx);
                        let up = MouseUpEvent { button, position, modifiers, click_count };
                        window.dispatch_event(PlatformInput::MouseUp(up), cx);
                    }
                    Ok(None)
                })?
            }
            "scroll" => {
                let position = point(px(number(0)?), px(number(1)?));
                let delta = point(px(if words.len() > 3 { number(3)? } else { 0. }), px(number(2)?));
                on_window(window, cx, |window, cx| {
                    let event = ScrollEvent { position, delta: ScrollDelta::Pixels(delta), ..Default::default() };
                    window.dispatch_event(PlatformInput::Scroll(event), cx);
                    Ok(None)
                })?
            }
            "key" => {
                let keystrokes = words
                    .iter()
                    .map(|combo| Keystroke::parse(&combo.replace('+', "-")).map_err(|error| format!("{combo}: {error}")))
                    .collect::<Result<Vec<_>, _>>()?;
                on_window(window, cx, |window, cx| {
                    for keystroke in &keystrokes {
                        window.dispatch_keystroke(keystroke.clone(), cx);
                    }
                    Ok(None)
                })?
            }
            "type" => {
                let text = rest.to_string();
                on_window(window, cx, |window, cx| {
                    for character in text.chars() {
                        let typed = character.to_string();
                        let key = if character == ' ' { "space".to_string() } else { typed.to_lowercase() };
                        let modifiers = Modifiers { shift: character.is_uppercase(), ..Default::default() };
                        window.dispatch_keystroke(Keystroke { modifiers, key, key_char: Some(typed) }, cx);
                    }
                    Ok(None)
                })?
            }
            "appearance" => {
                let dark = match rest.trim() {
                    "dark" => true,
                    "light" => false,
                    other => return Err(format!("appearance is light or dark, not {other}")),
                };
                set_appearance(dark)?;
                None
            }
            other => return Err(format!("unknown command {other}")),
        };
        Ok(Step::Done(note))
    }

    /// Runs `f` on the window.
    fn on_window(
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
        f: impl FnOnce(&mut Window, &mut App) -> Result<Option<String>, String>,
    ) -> Result<Option<String>, String> {
        cx.update_window(window, |_, window, cx| f(window, cx)).map_err(|error| error.to_string())?
    }

    /// Renders the window until two renders `SETTLE_STEP` apart agree, then saves the last.
    async fn shot(file: &Path, window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<String, String> {
        let started = Instant::now();
        let mut last: Option<image::RgbaImage> = None;
        loop {
            let frame = cx
                .update_window(window, |_, window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                    window.render_to_image()
                })
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("render: {error}"))?;
            let settled = last.as_ref().is_some_and(|last| last.as_raw() == frame.as_raw());
            if settled || started.elapsed() >= SETTLE_LIMIT {
                let (width, height) = frame.dimensions();
                if let Some(parent) = file.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
                }
                frame.save_with_format(file, image::ImageFormat::Png).map_err(|error| format!("{}: {error}", file.display()))?;
                let unsettled = if settled { "" } else { " (still changing)" };
                return Ok(format!("{width}x{height}{unsettled}"));
            }
            last = Some(frame);
            cx.background_executor().timer(SETTLE_STEP).await;
        }
    }

    #[cfg(target_os = "macos")]
    fn set_appearance(dark: bool) -> Result<(), String> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication};

        let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
        let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
        NSApplication::sharedApplication(mtm).setAppearance(NSAppearance::appearanceNamed(name).as_deref());
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn set_appearance(_: bool) -> Result<(), String> {
        Err("appearance is the desktop's here (scripts/vhypr.py appearance)".into())
    }
}
