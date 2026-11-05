//! Mailstream's GPUI front end (experiment). The Swift app stays the backend; this window talks to
//! it over the control socket. See README.md.

mod avatars;
mod chat;
mod control;
mod design;
mod format;
mod icons;
mod input;
mod model;
mod settings;
mod state;
mod swipe;
mod ui;

use gpui::{
    App, AppContext, Application, Bounds, Menu, MenuItem, OsAction, TitlebarOptions, WindowBounds, WindowOptions,
    point, px, size,
};

use crate::input::{Copy, Cut, Paste, SelectAll};
use crate::state::{
    Archive, Back, FocusSearch, MailApp, Quit, Refresh, Restore, Retag, Star, ToggleRead, ToggleSidebar,
    ToggleWaiting, Trash,
};

fn main() {
    Application::new().with_assets(icons::Assets).run(|cx: &mut App| {
        avatars::init(cx);
        input::bind_keys(cx);
        chat::bind_keys(cx);
        state::bind_keys(cx);
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
                cx.activate(true);
            })
            .ok();
    });
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
