//! The runner's side of the channels the Dart UI calls, as `macos/Runner` and `windows/runner`
//! implement them:
//! - `mailstream/files`: the compose column's open dialog (`pick`), pasted files and images
//!   (`pasted`), and files dragged onto the window (`hover`, `drop`).
//! - `mailstream/shell`: opens a file or link in its default app (`open`).
//! - `mailstream/window`: opens a link clicked in a mail body (`openMailLink`, which Dart checked
//!   against the engine's `is_mail_link`); `frameRate` (the
//!   Mac's menu checkmark) has nothing to update here.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;

use crate::flutter::{MethodCall, MethodChannel, Value};

/// `GDK_CURRENT_TIME`: the event time of an action not tied to an input event.
const NOW: u32 = gdk::ffi::GDK_CURRENT_TIME as u32;

struct Channels {
    window: gtk::ApplicationWindow,
    files: MethodChannel,
    shell: MethodChannel,
    window_channel: MethodChannel,
    /// Whether a compose column takes files (`accept`).
    accepts_files: Cell<bool>,
    /// Whether the compose column was told files hover over the window.
    hovering: Cell<bool>,
}

/// Serves the channels for `view`'s engine until `window` is destroyed.
pub fn attach(view: &gtk::Widget, window: &gtk::ApplicationWindow) {
    let channels = Rc::new(Channels {
        window: window.clone(),
        files: MethodChannel::new(view, "mailstream/files"),
        shell: MethodChannel::new(view, "mailstream/shell"),
        window_channel: MethodChannel::new(view, "mailstream/window"),
        accepts_files: Cell::new(false),
        hovering: Cell::new(false),
    });
    let weak = Rc::downgrade(&channels);
    channels.files.handle(with(&weak, Channels::files));
    channels.shell.handle(with(&weak, Channels::shell));
    channels.window_channel.handle(with(&weak, Channels::window));
    accept_drops(window, &weak);
    // The window holds the channels; they go (and stop answering) with it.
    let kept = RefCell::new(Some(channels));
    window.connect_destroy(move |_| drop(kept.take()));
}

/// `handler` on the channels while they live.
fn with(channels: &Weak<Channels>, handler: fn(&Channels, MethodCall)) -> impl Fn(MethodCall) + 'static {
    let channels = channels.clone();
    move |call| match channels.upgrade() {
        Some(channels) => handler(&channels, call),
        None => call.not_implemented(),
    }
}

impl Channels {
    fn files(&self, call: MethodCall) {
        match call.method() {
            "accept" => {
                self.accepts_files.set(call.bool_argument().unwrap_or(false));
                if !self.accepts_files.get() {
                    self.hover(false);
                }
                call.succeed(None);
            }
            // The Mac routes its Edit menu's Cmd+V to the compose column with it; Ctrl+V reaches
            // Flutter's keys directly here.
            "editing" => call.succeed(None),
            "pick" => pick(&self.window, call),
            "pasted" => pasted(&self.window, call),
            _ => call.not_implemented(),
        }
    }

    fn shell(&self, call: MethodCall) {
        if call.method() != "open" {
            return call.not_implemented();
        }
        let Some(uri) = call.string_argument().as_deref().and_then(target_uri) else {
            return call.fail("open", "Nothing to open.");
        };
        // The default app, as a double-click in the file manager: a file opens in its app, a link
        // in the browser.
        match gtk::show_uri_on_window(Some(&self.window), &uri, NOW) {
            Ok(()) => call.succeed(None),
            Err(error) => call.fail("open", error.message()),
        }
    }

    fn window(&self, call: MethodCall) {
        match call.method() {
            "openMailLink" => {
                if let Some(url) = call.string_argument() {
                    let _ = gtk::show_uri_on_window(Some(&self.window), &url, NOW);
                }
                call.succeed(None);
            }
            // The Mac's View menu checkmark; there is no menu here.
            "frameRate" => call.succeed(None),
            _ => call.not_implemented(),
        }
    }

    fn hover(&self, hovering: bool) {
        if self.hovering.replace(hovering) != hovering {
            self.files.invoke("hover", Value::bool(hovering));
        }
    }
}

/// The regular files among `uris`: folders, and anything not a local file, are not attached, as
/// on the Mac.
fn regular_files<S: AsRef<str>>(uris: &[S]) -> Vec<String> {
    uris.iter()
        .filter_map(|uri| glib::filename_from_uri(uri.as_ref()).ok())
        .map(|(path, _)| path)
        .filter(|path| path.is_file())
        .filter_map(|path| path.into_os_string().into_string().ok())
        .collect()
}

/// The files chosen in the desktop's open dialog (the portal's under a sandbox); none when it is
/// cancelled.
fn pick(window: &gtk::ApplicationWindow, call: MethodCall) {
    let dialog = gtk::FileChooserNative::new(
        Some("Attach Files"),
        Some(window),
        gtk::FileChooserAction::Open,
        Some("_Attach"),
        Some("_Cancel"),
    );
    dialog.set_select_multiple(true);
    dialog.set_local_only(true);
    // The dialog lives until it answers: this holds it, and lets it go then.
    let held = Rc::new(RefCell::new(Some(dialog.clone())));
    let call = RefCell::new(Some(call));
    dialog.connect_response(move |dialog, response| {
        let paths: Vec<PathBuf> = if response == gtk::ResponseType::Accept { dialog.filenames() } else { Vec::new() };
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|path| path.is_file())
            .filter_map(|path| path.into_os_string().into_string().ok())
            .collect();
        if let Some(call) = call.take() {
            call.succeed(Some(Value::strings(&paths)));
        }
        held.borrow_mut().take();
    });
    dialog.show();
}

/// The files on the clipboard: a file manager's copied files, else copied image pixels or a
/// screenshot written to a new PNG. Text, even with a picture of itself beside it (cells copied
/// from a spreadsheet), stays text: no files, so Flutter pastes it.
fn pasted(window: &gtk::ApplicationWindow, call: MethodCall) {
    let clipboard = gtk::Clipboard::for_display(&WidgetExt::display(window), &gdk::SELECTION_CLIPBOARD);
    // The call is answered once, by whichever step ends the paste.
    let call = Rc::new(RefCell::new(Some(call)));
    let answer = move |answer: &dyn Fn(MethodCall)| {
        if let Some(call) = call.borrow_mut().take() {
            answer(call);
        }
    };
    let none = |call: MethodCall| call.succeed(Some(Value::strings::<String>(&[])));
    clipboard.request_contents(&gdk::Atom::intern("TARGETS"), move |clipboard, selection| {
        let targets = selection.targets().unwrap_or_default();
        let answer = answer.clone();
        if gtk::targets_include_uri(&targets) {
            clipboard.request_uris(move |_, uris| {
                let paths = regular_files(uris);
                answer(&|call| call.succeed(Some(Value::strings(&paths))));
            });
        } else if !gtk::targets_include_text(&targets) && gtk::targets_include_image(&targets, false) {
            clipboard.request_image(move |_, image| match image.map(save_pasted_image) {
                None => answer(&none),
                Some(Ok(path)) => answer(&|call| call.succeed(Some(Value::strings(&[&path])))),
                Some(Err(message)) => answer(&|call| call.fail("clipboard", message)),
            });
        } else {
            answer(&none);
        }
    });
}

/// `image` in a new PNG in the temporary folder, named as the Mac names them, readable by the
/// user alone.
fn save_pasted_image(image: &gdk_pixbuf::Pixbuf) -> Result<String, &'static str> {
    let (file, path) =
        glib::file_open_tmp(Some("Mailstream-pasted-XXXXXX.png")).map_err(|_| "Could not make a file for the pasted image.")?;
    // SAFETY: the descriptor g_file_open_tmp opened is ours to close.
    drop(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(file) });
    if image.savev(&path, "png", &[]).is_err() {
        let _ = std::fs::remove_file(&path);
        return Err("Could not save the pasted image.");
    }
    path.into_os_string().into_string().map_err(|_| "Could not save the pasted image.")
}

/// Drops: the window takes files dragged from a file manager (`text/uri-list`) while a compose
/// column accepts them.
fn accept_drops(window: &gtk::ApplicationWindow, channels: &Weak<Channels>) {
    window.drag_dest_set(gtk::DestDefaults::empty(), &[], gdk::DragAction::COPY);
    window.drag_dest_add_uri_targets();
    let weak = channels.clone();
    window.connect_drag_motion(move |window, context, _, _, time| {
        let Some(channels) = weak.upgrade() else { return false };
        let files = channels.accepts_files.get() && window.drag_dest_find_target(context, None).is_some();
        context.drag_status(if files { gdk::DragAction::COPY } else { gdk::DragAction::empty() }, time);
        channels.hover(files);
        true
    });
    let weak = channels.clone();
    window.connect_drag_leave(move |_, _, _| {
        if let Some(channels) = weak.upgrade() {
            channels.hover(false);
        }
    });
    let weak = channels.clone();
    window.connect_drag_drop(move |window, context, _, _, time| {
        let channels = weak.upgrade();
        if let Some(channels) = &channels {
            channels.hover(false);
        }
        let target = window.drag_dest_find_target(context, None);
        match target {
            Some(target) if channels.is_some_and(|channels| channels.accepts_files.get()) => {
                window.drag_get_data(context, &target, time)
            }
            _ => context.drag_finish(false, false, time),
        }
        true
    });
    let weak = channels.clone();
    window.connect_drag_data_received(move |_, context, _, _, selection, _, time| {
        let paths = regular_files(&selection.uris());
        let dropped = !paths.is_empty() && weak.upgrade().is_some_and(|channels| channels.accepts_files.get());
        if let (true, Some(channels)) = (dropped, weak.upgrade()) {
            channels.files.invoke("drop", Value::strings(&paths));
        }
        context.drag_finish(dropped, false, time);
    });
}

/// `target` as a URI: a link as it is, an absolute path as its `file:` URI; `None` for anything
/// else.
fn target_uri(target: &str) -> Option<String> {
    if Path::new(target).is_absolute() {
        return glib::filename_to_uri(target, None).ok().map(String::from);
    }
    glib::Uri::peek_scheme(target).map(|_| target.to_owned())
}
