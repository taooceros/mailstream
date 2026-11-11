//! Mailstream's GPUI front end, on macOS and Linux. The Rust engine runs in this process through
//! the `mailstream` crate in `FFI/`. See README.md.

mod avatars;
mod bench;
mod chat;
mod compose;
mod design;
mod engine;
mod format;
mod icons;
mod input;
mod model;
mod settings;
mod state;
mod swipe;
mod textarea;
mod ui;
// HTML bodies: native `WKWebView`s on the Mac, the engine's Blitz pages drawn by GPUI elsewhere.
#[cfg_attr(not(target_os = "macos"), path = "blitz_body.rs")]
mod webview;

use gpui::{
    App, AppContext, Application, Bounds, Menu, MenuItem, OsAction, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, point, px, size,
};

use crate::input::{Copy, Cut, Paste, SelectAll};
use crate::state::{
    Archive, Back, FocusSearch, Forward, MailApp, NewChat, NewMessage, Quit, Refresh, Reply, ReplyAll, Restore,
    Retag, Star, ToggleRead, ToggleSidebar, ToggleWaiting, Trash,
};

fn main() {
    Application::new().with_assets(icons::Assets).run(|cx: &mut App| {
        #[cfg(target_os = "macos")]
        set_dock_icon();
        // The engine paints from the cache and syncs while the window opens.
        engine::engine();
        avatars::init(cx);
        input::bind_keys(cx);
        chat::bind_keys(cx);
        textarea::bind_keys(cx);
        compose::bind_keys(cx);
        state::bind_keys(cx);
        #[cfg(not(target_os = "macos"))]
        webview::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(menus());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Mailstream".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(19.))),
            }),
            window_min_size: Some(size(px(760.), px(420.))),
            // Linux: the compositor draws the frame and title bar where it offers to (where it
            // doesn't, the toolbar moves the window); desktop files and window rules match the id.
            window_decorations: Some(WindowDecorations::Server),
            app_id: Some(APP_ID.into()),
            ..Default::default()
        };
        let window = cx
            .open_window(options, |window, cx| cx.new(|cx| MailApp::new(window, cx)))
            .expect("the window opens");
        // Renames, sign-outs, added accounts and setting changes reload accounts and the list.
        settings::init(
            move |cx| {
                window.update(cx, |app, _, cx| app.retry(cx)).ok();
            },
            cx,
        );
        window
            .update(cx, |app, window, cx| {
                window.focus(&app.focus);
                // A measurement runs beside the user's work without taking the foreground.
                if app.bench.is_none() {
                    cx.activate(true);
                }
            })
            .ok();
    });
}

/// The window's application id on Linux (Wayland `app_id`, X11 `WM_CLASS`).
const APP_ID: &str = "dev.mailstream.MailstreamGPUI";

/// The Dock icon: the binary runs outside an app bundle, so without this the Dock shows the
/// generic executable icon. `assets/AppIcon.png` is Mailstream's icon with a "GPUI" badge
/// (`swift scripts/make-icon.swift GPUI/assets/AppIcon.png GPUI`, then `sips -Z 512`).
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::MainThreadMarker;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    static ICON: &[u8] = include_bytes!("../assets/AppIcon.png");
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

/// The menu bar (`AppDelegate.buildMenu`). Items dispatch to the focused window's handlers, so
/// they show the shortcuts bound in `state::bind_keys`.
fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Mailstream".into(),
            items: vec![
                MenuItem::action("Settings…", settings::ShowSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Mailstream", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![MenuItem::action("New Message", NewMessage), MenuItem::action("New Chat", NewChat)],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Find", FocusSearch),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Back", Back),
                MenuItem::separator(),
                MenuItem::action("Get New Mail", Refresh),
                MenuItem::action("Re-tag All Mail", Retag),
            ],
        },
        Menu {
            name: "Message".into(),
            items: vec![
                MenuItem::action("Reply", Reply),
                MenuItem::action("Reply All", ReplyAll),
                MenuItem::action("Forward", Forward),
                MenuItem::separator(),
                MenuItem::action("Mark as Read/Unread", ToggleRead),
                MenuItem::action("Star/Unstar", Star),
                MenuItem::action("Waiting on You", ToggleWaiting),
                MenuItem::separator(),
                MenuItem::action("Archive", Archive),
                MenuItem::action("Move to Inbox", Restore),
                MenuItem::action("Move to Trash", Trash),
            ],
        },
        Menu { name: "Window".into(), items: vec![] },
    ]
}
