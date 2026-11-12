//! Files a draft carries (MS-213, Flutter's compose attachments): picked in the system's file
//! chooser (the paperclip), dropped on the draft, or pasted: files a file manager copied
//! (`file://` URIs) or a copied image, written to a PNG in the temporary folder that only the
//! user can read. Each shows as a pill with its size until it is removed.

use std::fs::OpenOptions;
use std::io::{Cursor, ErrorKind, Write as _};
use std::path::PathBuf;
use std::rc::Rc;

use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClipboardEntry, Context, Div, ElementId, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::controls::pill;
use crate::design::{Palette, compose as metrics, icon as icon_size, space, text};
use crate::format;
use crate::icons::{Icon, icon};

struct Attached {
    path: PathBuf,
    name: SharedString,
    size: u64,
}

/// Removes the attachment at an index.
pub type Remove = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(Default)]
pub struct Attachments {
    files: Vec<Attached>,
}

impl Attachments {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The files' full paths, as the engine takes them.
    pub fn paths(&self) -> Vec<String> {
        self.files.iter().map(|file| file.path.to_string_lossy().into_owned()).collect()
    }

    /// Hands the files over (a reply moving to the compose window).
    pub fn take(&mut self) -> Vec<PathBuf> {
        self.files.drain(..).map(|file| file.path).collect()
    }

    /// Adds the files among `paths` not attached yet; a folder or a file that cannot be read is
    /// refused, naming it.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Result<(), String> {
        for path in paths {
            if self.files.iter().any(|file| file.path == path) {
                continue;
            }
            let name: SharedString =
                path.file_name().map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy()).into_owned().into();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => self.files.push(Attached { path, name, size: metadata.len() }),
                Ok(_) => return Err(format!("“{name}” is a folder; attach the files in it.")),
                Err(error) => return Err(format!("Could not attach “{name}”: {error}.")),
            }
        }
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.files.len() {
            self.files.remove(index);
        }
    }

    /// The attachments as pills, each with a button removing it (none while sending).
    pub fn render(&self, id: &str, removable: bool, remove: &Remove, p: &'static Palette) -> Option<Div> {
        if self.files.is_empty() {
            return None;
        }
        let pills = self.files.iter().enumerate().map(|(index, file)| {
            let remove = remove.clone();
            pill(
                format!("{}  ·  {}", file.name, format::file_size(file.size)),
                Some(icon(Icon::Attachment, icon_size::INLINE, p.muted).into_any_element()),
                p,
            )
            .max_w_full()
            .when(removable, |pill| {
                pill.child(
                    div()
                        .id(ElementId::NamedInteger(format!("{id}-remove").into(), index as u64))
                        .ml(px(space::S2))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .child(icon(Icon::Close, icon_size::INLINE, p.muted))
                        .on_click(move |_, window, cx| remove(index, window, cx)),
                )
            })
        });
        Some(div().flex().flex_wrap().gap(px(space::S6)).children(pills))
    }
}

/// Asks the system's file chooser for files to attach; `done` gets them, or why it could not open
/// (on Linux the desktop portal draws it). Cancelling does nothing.
pub fn pick<T: 'static>(
    cx: &mut Context<T>,
    done: impl FnOnce(&mut T, Result<Vec<PathBuf>, String>, &mut Context<T>) + 'static,
) {
    let answer = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some("Attach".into()),
    });
    cx.spawn(async move |this, cx| {
        let result = match answer.await {
            Ok(Ok(Some(paths))) => Ok(paths),
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(error)) => Err(format!("Could not open the file chooser: {error}")),
        };
        this.update(cx, |this, cx| done(this, result, cx)).ok();
    })
    .detach();
}

/// What the clipboard holds to attach: the files a file manager copied, else a copied image
/// saved as a PNG. `None` when it holds text (or nothing) for a field to paste.
pub fn pasted(cx: &App) -> Option<Result<Vec<PathBuf>, String>> {
    let item = cx.read_from_clipboard()?;
    match item.entries().first()? {
        ClipboardEntry::String(copied) => copied_files(copied.text()).map(Ok),
        ClipboardEntry::Image(image) => Some(save_image(image).map(|path| vec![path])),
    }
}

/// The files a file manager copied: `file://` URIs, one a line, after GNOME's
/// `x-special/nautilus-clipboard` and `copy` lines. Any other text is text.
fn copied_files(copied: &str) -> Option<Vec<PathBuf>> {
    let mut lines = copied.lines().map(str::trim).filter(|line| !line.is_empty()).peekable();
    if lines.next_if_eq(&"x-special/nautilus-clipboard").is_some() {
        lines.next_if(|line| matches!(*line, "copy" | "cut"));
    }
    let paths: Option<Vec<PathBuf>> = lines
        .map(|line| url::Url::parse(line).ok().filter(|url| url.scheme() == "file")?.to_file_path().ok())
        .collect();
    paths.filter(|paths| !paths.is_empty())
}

/// A copied image in a new PNG in the temporary folder, named as the Mac names them, readable by
/// the user alone.
fn save_image(image: &gpui::Image) -> Result<PathBuf, String> {
    const FAILED: &str = "Could not save the pasted image.";
    let png = if image.format() == gpui::ImageFormat::Png {
        image.bytes().to_vec()
    } else {
        let decoded = image::load_from_memory(image.bytes()).map_err(|_| FAILED.to_owned())?;
        let mut png = Cursor::new(Vec::new());
        decoded.write_to(&mut png, image::ImageFormat::Png).map_err(|_| FAILED.to_owned())?;
        png.into_inner()
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    for attempt in 0..100 {
        let suffix = if attempt == 0 { String::new() } else { format!("-{attempt}") };
        let path = std::env::temp_dir().join(format!("Mailstream-pasted-{stamp}{suffix}.png"));
        match options.open(&path) {
            Ok(mut file) => {
                if file.write_all(&png).is_err() {
                    let _ = std::fs::remove_file(&path);
                    return Err(FAILED.into());
                }
                return Ok(path);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return Err(FAILED.into()),
        }
    }
    Err(FAILED.into())
}

/// What covers a draft while files are dragged over it (`group` names the draft's element).
pub fn drop_target(group: &'static str, rounding: f32, p: &'static Palette) -> Div {
    let mut fill = p.accent;
    fill.a = metrics::DROP_FILL_ALPHA;
    div()
        .absolute()
        .top(px(metrics::DROP_INSET))
        .left(px(metrics::DROP_INSET))
        .right(px(metrics::DROP_INSET))
        .bottom(px(metrics::DROP_INSET))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::S12))
        .rounded(px(rounding))
        .border_2()
        .border_color(p.accent)
        .bg(fill)
        .text_color(p.on_accent)
        .text_size(px(text::PANE_TITLE))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .opacity(0.)
        .group_drag_over::<gpui::ExternalPaths>(group, |style| style.opacity(1.))
        .child(icon(Icon::Attachment, metrics::DROP_GLYPH, p.on_accent))
        .child("Drop files to attach")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_managers_copies_are_files_and_other_text_is_text() {
        let nautilus = "x-special/nautilus-clipboard\ncopy\nfile:///tmp/a%20b.pdf\nfile:///tmp/c.png\n";
        assert_eq!(copied_files(nautilus), Some(vec![PathBuf::from("/tmp/a b.pdf"), PathBuf::from("/tmp/c.png")]));
        assert_eq!(copied_files("file:///home/me/notes.txt"), Some(vec![PathBuf::from("/home/me/notes.txt")]));
        assert_eq!(copied_files("see file:///tmp/x"), None);
        assert_eq!(copied_files("https://example.com/a.pdf"), None);
        assert_eq!(copied_files("/tmp/a.pdf"), None);
        assert_eq!(copied_files(""), None);
    }

    #[test]
    fn attaching_skips_repeats_and_refuses_folders() {
        let folder = std::env::temp_dir().join(format!("mailstream-attach-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let mut attachments = Attachments::default();
        attachments.add([file.clone(), file.clone()]).unwrap();
        assert_eq!(attachments.paths(), vec![file.to_string_lossy().into_owned()]);
        assert!(attachments.add([folder.clone()]).is_err());
        assert!(attachments.add([folder.join("missing")]).is_err());
        attachments.remove(0);
        assert!(attachments.is_empty());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
