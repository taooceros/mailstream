//! Mailstream Flutter's Linux runner, in place of Flutter's C++ template: a GTK application whose
//! window holds Flutter's view (the engine's `FlView`), as `windows/runner` does on Windows, and
//! the channels the Dart UI calls (`channels`).
//!
//! The crate is a static library that defines `main`; CMake links it with Flutter's embedder,
//! GTK and the plugin registrant into the executable `flutter build linux` bundles.
use std::ffi::{c_char, c_int};

use glib::translate::{FromGlibPtrNone, ToGlibPtr};
use gtk::prelude::*;

mod channels;
mod flutter;

/// The Mac app's bundle identifier (runner/CMakeLists.txt passes `APPLICATION_ID`).
const APPLICATION_ID: &str = env!("APPLICATION_ID");

/// The process's entry point. `argv[1..]` go to Dart's `main`, as in Flutter's template.
///
/// # Safety
/// Called by the C runtime with the process's `argc` and NULL-terminated `argv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    // Program name = application ID, so desktops match the window to its .desktop file.
    glib::set_prgname(Some(APPLICATION_ID));
    let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::NON_UNIQUE);
    // NULL-terminated, as `argv` is; Flutter copies it.
    let arguments = if argc > 1 { unsafe { argv.add(1) } } else { std::ptr::null_mut() };
    app.connect_activate(move |app| activate(app, arguments));
    // GApplication sees no arguments of its own (it would take them for files to open).
    let program = if argc > 0 {
        unsafe { std::ffi::CStr::from_ptr(*argv) }.to_string_lossy().into_owned()
    } else {
        "mailstream_ui".to_owned()
    };
    app.run_with_args(&[program]).value()
}

fn activate(app: &gtk::Application, arguments: *mut *mut c_char) {
    let window = gtk::ApplicationWindow::new(app);
    if uses_header_bar() {
        let bar = gtk::HeaderBar::new();
        bar.set_title(Some("Mailstream"));
        bar.set_show_close_button(true);
        bar.show();
        window.set_titlebar(Some(&bar));
    }
    window.set_title("Mailstream");
    // As on Windows: 1240×800, centered, no smaller than 820×520.
    window.set_default_size(1240, 800);
    window.set_position(gtk::WindowPosition::Center);
    let minimum = gdk::Geometry::new(820, 520, -1, -1, 0, 0, 0, 0, 0.0, 0.0, gdk::Gravity::NorthWest);
    window.set_geometry_hints(None::<&gtk::Widget>, Some(&minimum), gdk::WindowHints::MIN_SIZE);

    let view: gtk::Widget = unsafe {
        let project = flutter::sys::fl_dart_project_new();
        if !arguments.is_null() {
            flutter::sys::fl_dart_project_set_dart_entrypoint_arguments(project, arguments);
        }
        let view = flutter::sys::fl_view_new(project);
        glib::gobject_ffi::g_object_unref(project.cast());
        // Takes the floating reference.
        gtk::Widget::from_glib_none(view.cast::<gtk::ffi::GtkWidget>())
    };
    view.show();
    window.add(&view);
    // Shown once Flutter has drawn, so the window never opens empty.
    view.connect_local("first-frame", false, |values| {
        let view = values[0].get::<gtk::Widget>().expect("the view");
        view.toplevel().expect("the window").show();
        None
    });
    // Flutter renders only into a realized view.
    view.realize();
    unsafe {
        let registry: *mut gtk::ffi::GtkWidget = view.to_glib_none().0;
        flutter::sys::fl_register_plugins(registry.cast());
    }
    channels::attach(&view, &window);
    view.grab_focus();
}

/// GTK's header bar, as Flutter's template decides: on Wayland, and on X11 under GNOME Shell; a
/// traditional title bar for other X11 window managers, which may lay windows out their own way
/// (tiling).
fn uses_header_bar() -> bool {
    let x11 = gdk::Display::default().is_some_and(|display| display.type_().name() == "GdkX11Display");
    !x11 || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| desktop.split(':').any(|name| name == "GNOME"))
}
