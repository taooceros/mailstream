//! Mailstream's GPUI front end (experiment). The Swift app stays the backend; this window talks to
//! it over the control socket. See README.md.

mod control;
mod design;
mod format;
mod input;
mod model;
mod state;
mod ui;

use gpui::{
    App, AppContext, Application, Bounds, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};

use crate::state::{MailApp, Quit};

fn main() {
    Application::new().run(|cx: &mut App| {
        input::bind_keys(cx);
        state::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(vec![Menu { name: "Mailstream".into(), items: vec![MenuItem::action("Quit Mailstream", Quit)] }]);
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
        window
            .update(cx, |app, window, cx| {
                window.focus(&app.focus);
                cx.activate(true);
            })
            .ok();
    });
}
