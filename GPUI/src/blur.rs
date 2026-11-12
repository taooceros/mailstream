//! Whether the Linux desktop blurs what lies behind a translucent window (MS-249), which decides
//! the window's material (`design::window_material`): the frosted window where it does, the opaque
//! one where it doesn't, so the desktop never shows through sharp.
//!
//! - Hyprland blurs every translucent window while `decoration:blur:enabled` is on; its IPC socket
//!   answers that option, and its event socket says when the configuration reloads, after which
//!   the option is asked again. (Its `ext_background_effect_manager_v1` claims blur whatever the
//!   option says, so it isn't asked.)
//! - Elsewhere on Wayland the window asks for blur through `ext_background_effect_manager_v1`
//!   (KWin, others), whose capabilities say whether the compositor blurs and tell when that
//!   changes, or KWin's older `org_kde_kwin_blur_manager`, whose presence is the blur effect's.
//!   GNOME offers neither and keeps the opaque window, as does X11.
//!
//! [`init`] asks once before the window opens; [`follow`] then switches the open window when the
//! answer changes.

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{AnyWindowHandle, App};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::{
    self, Capability, ExtBackgroundEffectManagerV1,
};

use crate::design;

/// The answer the window is drawn with; changed only on the main thread, with the window's
/// background, so a frame never pairs a translucent fill with an opaque window or the reverse.
static BLURS: AtomicBool = AtomicBool::new(false);

/// Whether the desktop blurs behind the window now.
pub fn blurs() -> bool {
    BLURS.load(Ordering::Relaxed)
}

/// Where later answers come from.
pub enum Watch {
    /// Hyprland's instance folder, with its `.socket.sock` and `.socket2.sock`.
    Hyprland(PathBuf),
    /// A Wayland connection of its own with the bound effect manager, whose capability events
    /// carry the answer.
    Wayland(EventQueue<Probe>, Probe),
    /// Nothing changes the answer.
    Fixed,
}

/// Asks the desktop once, before the window opens (`design::window_background` reads the answer).
pub fn init() -> Watch {
    let (blurs, watch) = match env::var_os("HYPRLAND_INSTANCE_SIGNATURE").zip(env::var_os("XDG_RUNTIME_DIR")) {
        Some((instance, runtime)) => {
            let folder = Path::new(&runtime).join("hypr").join(instance);
            (hyprland_blurs(&folder), Watch::Hyprland(folder))
        }
        None if env::var_os("WAYLAND_DISPLAY").is_some() => wayland().unwrap_or((false, Watch::Fixed)),
        None => (false, Watch::Fixed),
    };
    BLURS.store(blurs, Ordering::Relaxed);
    watch
}

/// Switches `window` between the frosted and the opaque material whenever the desktop's answer
/// changes, for as long as the app runs.
pub fn follow(watch: Watch, window: AnyWindowHandle, cx: &mut App) {
    let (sender, mut answers) = unbounded();
    let spawned = match watch {
        Watch::Hyprland(folder) => std::thread::Builder::new()
            .name("hyprland-blur".into())
            .spawn(move || watch_hyprland(&folder, &sender)),
        Watch::Wayland(mut queue, mut probe) => {
            probe.answers = Some(sender);
            std::thread::Builder::new()
                .name("wayland-blur".into())
                .spawn(move || while queue.blocking_dispatch(&mut probe).is_ok() {})
        }
        Watch::Fixed => return,
    };
    if spawned.is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        while let Some(mut blurs) = answers.next().await {
            while let Ok(later) = answers.try_recv() {
                blurs = later;
            }
            if blurs == self::blurs() {
                continue;
            }
            BLURS.store(blurs, Ordering::Relaxed);
            let switched = window.update(cx, |_, window, _| {
                window.set_background_appearance(design::window_background());
                window.refresh();
            });
            if switched.is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Hyprland's `decoration:blur:enabled`; false when Hyprland doesn't answer.
fn hyprland_blurs(folder: &Path) -> bool {
    let ask = || -> std::io::Result<String> {
        let mut socket = UnixStream::connect(folder.join(".socket.sock"))?;
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        socket.write_all(b"j/getoption decoration:blur:enabled")?;
        let mut answer = String::new();
        socket.read_to_string(&mut answer)?;
        Ok(answer)
    };
    let Ok(option) = ask().and_then(|answer| Ok(serde_json::from_str::<serde_json::Value>(&answer)?)) else {
        return false;
    };
    // Hyprland 0.57 answers a boolean `bool`; older ones an integer `int`.
    option["bool"].as_bool().or_else(|| option["int"].as_i64().map(|value| value != 0)).unwrap_or(false)
}

/// Asks Hyprland again each time its configuration reloads (`configreloaded` on its event socket);
/// ends with Hyprland or the app.
fn watch_hyprland(folder: &Path, answers: &UnboundedSender<bool>) {
    let Ok(events) = UnixStream::connect(folder.join(".socket2.sock")) else { return };
    for line in BufReader::new(events).lines() {
        let Ok(line) = line else { return };
        if line.starts_with("configreloaded>>") && answers.unbounded_send(hyprland_blurs(folder)).is_err() {
            return;
        }
    }
}

/// The compositor's blur capability from `ext_background_effect_manager_v1`, kept on a connection
/// of the app's own beside GPUI's.
pub struct Probe {
    blurs: bool,
    answers: Option<UnboundedSender<bool>>,
}

/// The first answer on Wayland, and a watch when the compositor can tell of changes.
fn wayland() -> Option<(bool, Watch)> {
    let connection = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Probe>(&connection).ok()?;
    let handle = queue.handle();
    let mut probe = Probe { blurs: false, answers: None };
    match globals.bind::<ExtBackgroundEffectManagerV1, _, _>(&handle, 1..=1, ()) {
        // The capabilities arrive as the global is bound.
        Ok(_manager) => {
            queue.roundtrip(&mut probe).ok()?;
            Some((probe.blurs, Watch::Wayland(queue, probe)))
        }
        Err(_) => {
            let kwin = globals.contents().with_list(|list| list.iter().any(|global| global.interface == "org_kde_kwin_blur_manager"));
            Some((kwin, Watch::Fixed))
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags: WEnum::Value(flags) } = event {
            probe.blurs = flags.contains(Capability::Blur);
            if let Some(answers) = &probe.answers {
                let _ = answers.unbounded_send(probe.blurs);
            }
        }
    }
}
