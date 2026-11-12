//! Mailstream's GPUI front end, on macOS and Linux. The Rust engine runs in this process through
//! the `mailstream` crate in `FFI/`. See README.md.

/// A `secondary-` keystroke: ⌘ on the Mac, Ctrl elsewhere. gpui-fast's `KeyBinding::new` takes
/// explicit modifiers only, so the bindings name the platform's at compile time.
#[cfg(target_os = "macos")]
macro_rules! secondary {
    ($keys:literal) => {
        concat!("cmd-", $keys)
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! secondary {
    ($keys:literal) => {
        concat!("ctrl-", $keys)
    };
}

mod avatars;
mod bench;
mod calendar;
mod chat;
mod compose;
mod controls;
mod design;
mod engine;
mod format;
mod frame_rate;
#[cfg(target_os = "macos")]
mod glass;
mod icons;
mod input;
mod model;
mod settings;
mod split;
mod state;
mod swipe;
mod ui;
#[cfg(target_os = "macos")]
mod updater;
// HTML bodies: the engine's Blitz pages drawn by GPUI everywhere; on the Mac the reader can switch
// to native `WKWebView`s instead (`state::Renderer`).
mod blitz_body;
#[cfg(target_os = "macos")]
mod webview;

use gpui::component::input::{Copy, Cut, Paste, SelectAll};
use gpui::{
    App, AppContext, Bounds, Menu, MenuItem, OsAction, Styled, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, point, px, size,
};

use crate::state::{
    Archive, Back, FocusSearch, Forward, MailApp, NewChat, NewMessage, Quit, Refresh, Reply, ReplyAll, Restore,
    Retag, Star, ToggleFrameRate, ToggleRead, ToggleSidebar, ToggleWaiting, Trash,
};

fn main() {
    gpui::application().with_assets(icons::Assets).run(|cx: &mut App| {
        // gpui-kit's components, in the app's palette (`design::sync_kit_theme`).
        gpui::init(cx);
        #[cfg(target_os = "macos")]
        appearance_from_arguments();
        design::sync_kit_theme(cx.window_appearance(), cx);
        #[cfg(target_os = "macos")]
        set_dock_icon();
        // The engine paints from the cache and syncs while the window opens.
        engine::engine();
        avatars::init(cx);
        chat::bind_keys(cx);
        calendar::bind_keys(cx);
        compose::bind_keys(cx);
        state::bind_keys(cx);
        blitz_body::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(menus());
        #[cfg(target_os = "macos")]
        updater::start();
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
        let mut options = WindowOptions {
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
            window_background: design::window_background(),
            ..Default::default()
        };
        // GPUI Fast draws a window that isn't the key window at about 30 fps unless input is
        // arriving. The measurement runs in the background on purpose and scrolls by itself, so
        // it lifts that limit to see what the window does at the display's rate.
        if bench::requested() {
            options.inactive_frame_interval = None;
        }
        // In gpui-kit's root, which hosts the kit's tooltips, menus and popovers over the app; its
        // surface left clear (the kit paints its theme's background there), so the window's glass
        // (or blur) shows where the app paints nothing.
        let mut app = None;
        let window = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| MailApp::new(window, cx));
                app = Some(view.clone());
                cx.new(|cx| gpui::base::Root::new(view, window, cx).bg(gpui::transparent_black()))
            })
            .expect("the window opens");
        let app = app.expect("the window built its view");
        // Renames, sign-outs, added accounts and setting changes reload accounts and the list.
        let reload = app.downgrade();
        settings::init(
            move |cx| {
                reload.update(cx, |app, cx| app.retry(cx)).ok();
            },
            cx,
        );
        window
            .update(cx, |_, window, cx| {
                #[cfg(target_os = "macos")]
                if design::window_material() == design::WindowMaterial::Glass {
                    glass::install(window);
                }
                let (focus, bench) = app.read_with(cx, |app, _| (app.focus.clone(), app.bench.is_some()));
                window.focus(&focus, cx);
                // A measurement runs beside the user's work without taking the foreground.
                if !bench {
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

/// `-AppleInterfaceStyle Dark` (or `Light`) on the command line shows the app in that appearance
/// without changing the system's setting, for screenshots (AGENTS.md, Release delivery). AppKit
/// leaves an app's appearance to the system's even with that argument, so it is applied here;
/// only the command line's value counts, so a normal launch keeps following the system.
#[cfg(target_os = "macos")]
fn appearance_from_arguments() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication};
    use objc2_foundation::{NSArgumentDomain, NSString, NSUserDefaults, ns_string};

    let Some(mtm) = MainThreadMarker::new() else { return };
    let arguments = NSUserDefaults::standardUserDefaults().volatileDomainForName(unsafe { NSArgumentDomain });
    let Some(style) = arguments.objectForKey(ns_string!("AppleInterfaceStyle")) else { return };
    let dark = style.downcast_ref::<NSString>().is_some_and(|style| style.to_string() == "Dark");
    let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
    NSApplication::sharedApplication(mtm).setAppearance(NSAppearance::appearanceNamed(name).as_deref());
}

/// The menu bar (`AppDelegate.buildMenu`). Items dispatch to the focused window's handlers, so
/// they show the shortcuts bound in `state::bind_keys`.
fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Mailstream").items([
            MenuItem::action("Settings…", settings::ShowSettings),
            MenuItem::separator(),
            MenuItem::action("Quit Mailstream", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("New Message", NewMessage), MenuItem::action("New Chat", NewChat)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find", FocusSearch),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Back", Back),
            MenuItem::separator(),
            MenuItem::action("Get New Mail", Refresh),
            MenuItem::action("Re-tag All Mail", Retag),
            MenuItem::separator(),
            MenuItem::action("Show Frame Rate", ToggleFrameRate),
        ]),
        Menu::new("Message").items([
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
        ]),
        Menu::new("Window"),
    ]
}
