//! HTML mail bodies drawn by Blitz, the engine's renderer (`FFI/src/html_page.rs`), where there
//! is no `WKWebView` to lay over the window (Linux; the Mac's is `webview.rs`, same calls).
//!
//! A page lives on the engine's pages thread: parsing, layout and drawing never touch this one,
//! which only asks for its height and for tiles, strips [`TILE`] points tall of RGBA pixels, for
//! the part of the body in or near the reader's viewport, and paints them as GPUI images. The body
//! is as tall as its page, so the reader scrolls over it as over text. As in Flutter's Blitz reader
//! (`Flutter/lib/ui/blitz_body.dart`): a click opens the link under it in the browser (http,
//! https and mailto only), a drag selects text and ⌘C (Ctrl+C) copies it, and late content (a
//! remote image, a web font) lays the page out again. Inline parts the engine cached go in as
//! `data:` URLs ([`inline_images`]): the page reads no file.
//!
//! Each frame starts with [`begin_frame`] (painted first, at the window root), which closes the
//! pages no placeholder painted in the frame before and frees their tiles; [`paint`] draws one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use gpui::{
    App, Bounds, ClipboardItem, Corners, CursorStyle, DispatchPhase, Hitbox, HitboxBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, SharedString, Window, actions, point,
    px, size,
};
pub use mailstream::HtmlPart;
use mailstream::{HtmlPage, HtmlTile};

/// Tiles are this many points tall.
const TILE: f32 = 512.;

/// Tiles this many beyond the viewport are drawn ahead; past twice this they are freed.
const AHEAD: usize = 1;

/// The largest inline part made a `data:` URL.
const MAX_INLINE_IMAGE: u64 = 20 << 20;

actions!(mail_body, [CopyBody]);

/// ⌘C (Ctrl+C) copies the selection of a body. Text fields bind it in their own context, which
/// wins while one has focus.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new("secondary-c", CopyBody, None)]);
    cx.on_action(|_: &CopyBody, cx| copy_selection(cx));
}

struct Tile {
    image: Arc<RenderImage>,
    /// Points, in the layout it was drawn for.
    top: f32,
    width: f32,
    height: f32,
    generation: u64,
}

/// A finished layout: the width and scale it was made for and the page's height there.
#[derive(Clone, Copy, PartialEq)]
struct Layout {
    width: f32,
    scale: f32,
    height: f32,
}

#[derive(Default)]
struct Entry {
    /// None while the body's inline images are read.
    page: Option<Arc<HtmlPage>>,
    layout: Option<Layout>,
    /// The width and scale a layout was last asked for.
    asked: Option<(f32, f32)>,
    /// The page's content version, which late content raises.
    version: i64,
    /// Grows whenever drawn tiles go stale (a layout, a selection); a stale tile stays on screen
    /// until its replacement arrives, so nothing flashes.
    generation: u64,
    tiles: HashMap<usize, Tile>,
    /// The generation each tile being drawn was asked for in.
    drawing: HashMap<usize, u64>,
    painted: bool,
    /// The pointer, in page points, while over the body; and whether a link lies under it.
    hovered: Option<Point<f32>>,
    asking_link: bool,
    over_link: bool,
    /// Where the left button went down, in page points, and whether it has moved since.
    press: Option<Point<f32>>,
    dragged: bool,
    pending_selection: Option<(Point<f32>, Point<f32>)>,
    selecting: bool,
    selected: bool,
    /// Whether the mail has quoted history its part leaves out or shows, once the page says.
    quoted: Option<bool>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(page) = &self.page {
            page.close();
        }
    }
}

#[derive(Default)]
struct Pages {
    entries: HashMap<SharedString, Entry>,
    /// Tile images no longer shown, freed from the sprite atlas at the next frame's start.
    retired: Vec<Arc<RenderImage>>,
}

thread_local! {
    static PAGES: RefCell<Pages> = RefCell::default();
}

/// The page height of the body shown under `key`, once laid out.
pub fn height(key: &SharedString) -> Option<f32> {
    PAGES.with_borrow(|pages| pages.entries.get(key).and_then(|entry| entry.layout).map(|layout| layout.height))
}

/// Whether the body shown under `key` has quoted history (see [`HtmlPart`]), once its page says.
pub fn quoted(key: &SharedString) -> Option<bool> {
    PAGES.with_borrow(|pages| pages.entries.get(key).and_then(|entry| entry.quoted))
}

/// Starts a frame of the window: closes the pages not painted last frame and frees the tiles no
/// longer shown.
pub fn begin_frame(window: &mut Window) {
    let retired = PAGES.with_borrow_mut(|pages| {
        let mut retired = std::mem::take(&mut pages.retired);
        pages.entries.retain(|_, entry| {
            let keep = std::mem::take(&mut entry.painted);
            if !keep {
                retired.extend(entry.tiles.drain().map(|(_, tile)| tile.image));
            }
            keep
        });
        retired
    });
    for image in retired {
        window.drop_image(image).ok();
    }
}

/// The body's hitbox, for its pointer and its cursor (in the placeholder's prepaint).
pub fn prepaint(bounds: Bounds<Pixels>, window: &mut Window) -> Hitbox {
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}

/// Draws `part` of `html` under `key` at `bounds`, clipped to the current content mask.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    key: &SharedString,
    html: &SharedString,
    part: HtmlPart,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let width = f32::from(bounds.size.width);
    let scale = window.scale_factor();
    let visible = bounds.intersect(&window.content_mask().bounds);
    let viewport = (f32::from(visible.top() - bounds.top()), f32::from(visible.bottom() - bounds.top()));
    let mut images = Vec::new();
    let over_link = PAGES.with_borrow_mut(|pages| {
        let Pages { entries, retired } = pages;
        let entry = entries.entry(key.clone()).or_insert_with(|| {
            open(key, html, part, window, cx);
            Entry::default()
        });
        entry.painted = true;
        let Some(page) = entry.page.clone() else { return false };
        if entry.asked != Some((width, scale)) {
            entry.asked = Some((width, scale));
            lay_out(key, &page, width, scale, window, cx);
        }
        if let Some(layout) = entry.layout {
            update_tiles(key, &page, entry, layout, viewport, retired, window, cx);
        }
        let mut tiles: Vec<_> = entry.tiles.values().collect();
        tiles.sort_by_key(|tile| tile.generation);
        images.extend(tiles.into_iter().map(|tile| {
            let origin = bounds.origin + point(px(0.), px(tile.top));
            (Bounds::new(origin, size(px(tile.width), px(tile.height))), tile.image.clone())
        }));
        entry.over_link
    });
    for (bounds, image) in images {
        window.paint_image(bounds, Corners::default(), image, 0, false).ok();
    }
    if over_link {
        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
    }
    listen(key, bounds, hitbox, window);
}

/// Reads the body's inline images off this thread, then opens its page.
fn open(key: &SharedString, html: &SharedString, part: HtmlPart, window: &Window, cx: &App) {
    let (key, html) = (key.clone(), html.clone());
    let prepared = cx.background_executor().spawn(async move { inline_images(&html) });
    window
        .spawn(cx, async move |cx| {
            let page = HtmlPage::part(prepared.await, part);
            let opened = PAGES.with_borrow_mut(|pages| match pages.entries.get_mut(&key) {
                Some(entry) => {
                    entry.page = Some(page.clone());
                    true
                }
                None => false,
            });
            if !opened {
                page.close();
                return;
            }
            cx.update(|window, _| window.refresh()).ok();
            if part != HtmlPart::Whole {
                let quoted = page.quoted().await;
                if with_page(&key, &page, |entry| entry.quoted = quoted).is_none() {
                    return;
                }
                cx.update(|window, _| window.refresh()).ok();
            }
            // Late content (a remote image, a web font) changes the page: lay it out again.
            let mut version = 0;
            while let Some(changed) = page.changed(version).await {
                version = changed;
                let current = with_page(&key, &page, |entry| {
                    entry.version = changed;
                    entry.asked = None;
                });
                if current.is_none() {
                    return;
                }
                cx.update(|window, _| window.refresh()).ok();
            }
        })
        .detach();
}

/// Runs `f` on the entry under `key` if it still shows `page`.
fn with_page<R>(key: &SharedString, page: &Arc<HtmlPage>, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
    PAGES.with_borrow_mut(|pages| {
        let entry = pages.entries.get_mut(key)?;
        entry.page.as_ref().is_some_and(|shown| Arc::ptr_eq(shown, page)).then(|| f(entry))
    })
}

fn lay_out(key: &SharedString, page: &Arc<HtmlPage>, width: f32, scale: f32, window: &Window, cx: &App) {
    let (key, page) = (key.clone(), page.clone());
    window
        .spawn(cx, async move |cx| {
            let Some(size) = page.layout(f64::from(width), f64::from(scale)).await else { return };
            let laid_out = with_page(&key, &page, |entry| {
                if entry.asked != Some((width, scale)) {
                    return false;
                }
                entry.layout = Some(Layout { width, scale, height: size.height as f32 });
                entry.generation += 1;
                true
            });
            if laid_out == Some(true) {
                cx.update(|window, _| window.refresh()).ok();
            }
        })
        .detach();
}

/// Draws the tiles near the viewport that are missing or stale and frees those far from it.
#[allow(clippy::too_many_arguments)]
fn update_tiles(
    key: &SharedString,
    page: &Arc<HtmlPage>,
    entry: &mut Entry,
    layout: Layout,
    (top, bottom): (f32, f32),
    retired: &mut Vec<Arc<RenderImage>>,
    window: &Window,
    cx: &App,
) {
    let count = (layout.height / TILE).ceil() as usize;
    let ahead = TILE * AHEAD as f32;
    if count == 0 || bottom < top || bottom < -ahead || top > layout.height + ahead {
        // Out of view (another message's turn): keep nothing.
        retired.extend(entry.tiles.drain().map(|(_, tile)| tile.image));
        return;
    }
    let index = |y: f32| ((y / TILE).floor().max(0.) as usize).min(count - 1);
    let (keep_first, keep_last) = (index(top - 2. * ahead), index(bottom + 2. * ahead));
    entry.tiles.retain(|&i, tile| {
        let keep = (keep_first..=keep_last).contains(&i);
        if !keep {
            retired.push(tile.image.clone());
        }
        keep
    });
    for i in index(top - ahead)..=index(bottom + ahead) {
        let generation = entry.generation;
        if entry.tiles.get(&i).is_some_and(|tile| tile.generation == generation)
            || entry.drawing.get(&i) == Some(&generation)
        {
            continue;
        }
        let tile_top = i as f32 * TILE;
        let height = TILE.min(layout.height - tile_top);
        if height <= 0. {
            continue;
        }
        entry.drawing.insert(i, generation);
        draw_tile(key, page, i, generation, layout, tile_top, height, window, cx);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tile(
    key: &SharedString,
    page: &Arc<HtmlPage>,
    i: usize,
    generation: u64,
    layout: Layout,
    top: f32,
    height: f32,
    window: &Window,
    cx: &App,
) {
    let (key, page) = (key.clone(), page.clone());
    let executor = cx.background_executor().clone();
    window
        .spawn(cx, async move |cx| {
            let drawn = page
                .tile(f64::from(layout.width), f64::from(layout.scale), f64::from(top), f64::from(height))
                .await;
            let Some(drawn) = drawn else { return };
            let image = executor.spawn(async move { render_image(drawn) }).await;
            let shown = with_page(&key, &page, |entry| {
                if entry.drawing.get(&i) == Some(&generation) {
                    entry.drawing.remove(&i);
                }
                let tile = Tile { image: image.clone(), top, width: layout.width, height, generation };
                match entry.tiles.get(&i) {
                    Some(current) if current.generation >= generation => Some(image.clone()),
                    _ => entry.tiles.insert(i, tile).map(|old| old.image),
                }
            });
            let retire = match shown {
                Some(retire) => retire,
                // The page is gone: free what was drawn for it.
                None => Some(image),
            };
            PAGES.with_borrow_mut(|pages| pages.retired.extend(retire));
            cx.update(|window, _| window.refresh()).ok();
        })
        .detach();
}

/// A tile as a GPUI image, whose pixels are BGRA.
fn render_image(tile: HtmlTile) -> Arc<RenderImage> {
    let mut pixels = tile.pixels;
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(tile.width, tile.height, pixels).expect("a tile is width × height RGBA pixels");
    Arc::new(RenderImage::new([image::Frame::new(buffer)]))
}

/// The pointer: links under it, clicks on them, drags that select.
fn listen(key: &SharedString, bounds: Bounds<Pixels>, hitbox: Hitbox, window: &mut Window) {
    let local = move |position: Point<Pixels>| {
        point(f32::from(position.x - bounds.origin.x), f32::from(position.y - bounds.origin.y))
    };
    let (moved_key, moved_hitbox) = (key.clone(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let at = local(event.position);
        let pressed = event.pressed_button == Some(MouseButton::Left);
        let hovered = moved_hitbox.is_hovered(window);
        let Some((page, ask, select)) = PAGES.with_borrow_mut(|pages| {
            let entry = pages.entries.get_mut(&moved_key)?;
            let page = entry.page.clone()?;
            entry.hovered = hovered.then_some(at);
            if !hovered && entry.over_link {
                entry.over_link = false;
                window.refresh();
            }
            let ask = hovered && !entry.asking_link;
            entry.asking_link |= ask;
            let select = match entry.press {
                Some(from) if pressed => {
                    entry.dragged = true;
                    entry.pending_selection = Some((from, at));
                    !std::mem::replace(&mut entry.selecting, true)
                }
                _ => false,
            };
            Some((page, ask, select))
        }) else {
            return;
        };
        if ask {
            ask_link(&moved_key, &page, window, cx);
        }
        if select {
            run_selection(&moved_key, &page, window, cx);
        }
    });

    let (down_key, down_hitbox) = (key.clone(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !down_hitbox.is_hovered(window) {
            return;
        }
        let at = local(event.position);
        let cleared = PAGES.with_borrow_mut(|pages| {
            // One selection in the window: a press in any body ends the others'.
            let mut cleared = Vec::new();
            for (key, entry) in pages.entries.iter_mut() {
                if *key == down_key {
                    entry.press = Some(at);
                    entry.dragged = false;
                }
                if std::mem::take(&mut entry.selected)
                    && let Some(page) = entry.page.clone()
                {
                    cleared.push((key.clone(), page));
                }
            }
            cleared
        });
        for (key, page) in cleared {
            window
                .spawn(cx, async move |cx| {
                    if page.clear_selection().await {
                        with_page(&key, &page, |entry| entry.generation += 1);
                        cx.update(|window, _| window.refresh()).ok();
                    }
                })
                .detach();
        }
    });

    let up_key = key.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        let at = local(event.position);
        let Some(page) = PAGES.with_borrow_mut(|pages| {
            let entry = pages.entries.get_mut(&up_key)?;
            entry.press.take()?;
            let clicked = !std::mem::take(&mut entry.dragged) && hitbox.is_hovered(window);
            entry.page.clone().filter(|_| clicked)
        }) else {
            return;
        };
        window
            .spawn(cx, async move |cx| {
                if let Some(link) = page.link_at(f64::from(at.x), f64::from(at.y)).await
                    && is_mail_link(&link)
                {
                    cx.update(|_, cx| cx.open_url(&link)).ok();
                }
            })
            .detach();
    });
}

/// Whether a link lies under the pointer; one question in flight, the latest position wins.
fn ask_link(key: &SharedString, page: &Arc<HtmlPage>, window: &Window, cx: &App) {
    let (key, page) = (key.clone(), page.clone());
    window
        .spawn(cx, async move |cx| {
            let mut asked = None;
            loop {
                let Some(Some(at)) = with_page(&key, &page, |entry| entry.hovered) else { break };
                if asked == Some(at) {
                    break;
                }
                asked = Some(at);
                let over = page.link_at(f64::from(at.x), f64::from(at.y)).await.is_some_and(|link| is_mail_link(&link));
                let changed = with_page(&key, &page, |entry| std::mem::replace(&mut entry.over_link, over) != over);
                if changed == Some(true) {
                    cx.update(|window, _| window.refresh()).ok();
                }
            }
            with_page(&key, &page, |entry| entry.asking_link = false);
        })
        .detach();
}

/// Selects from the press to the pointer; one selection in flight, the drag's latest point wins.
fn run_selection(key: &SharedString, page: &Arc<HtmlPage>, window: &Window, cx: &App) {
    let (key, page) = (key.clone(), page.clone());
    window
        .spawn(cx, async move |cx| {
            while let Some(Some((from, to))) = with_page(&key, &page, |entry| entry.pending_selection.take()) {
                let selected =
                    page.select(f64::from(from.x), f64::from(from.y), f64::from(to.x), f64::from(to.y)).await;
                // Draw the tiles again: the shown ones stay until replaced.
                with_page(&key, &page, |entry| {
                    entry.selected = selected;
                    entry.generation += 1;
                });
                cx.update(|window, _| window.refresh()).ok();
            }
            with_page(&key, &page, |entry| entry.selecting = false);
        })
        .detach();
}

/// Copies the selected text of the body that has a selection, if any.
fn copy_selection(cx: &mut App) {
    let page = PAGES.with_borrow(|pages| {
        pages.entries.values().find(|entry| entry.selected).and_then(|entry| entry.page.clone())
    });
    let Some(page) = page else { return };
    cx.spawn(async move |cx| {
        if let Some(text) = page.selected_text().await {
            cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text))).ok();
        }
    })
    .detach();
}

/// Links a click opens: web pages and mail addresses, nothing local.
fn is_mail_link(link: &str) -> bool {
    let Ok(url) = url::Url::parse(link) else { return false };
    match url.scheme() {
        "mailto" => true,
        "http" | "https" => url.username().is_empty() && url.host().is_some(),
        _ => false,
    }
}

/// `html` with each `file:` URL of a part in the engine's inline cache replaced by a `data:` URL
/// of its bytes, when they are a raster image of at most [`MAX_INLINE_IMAGE`]. Other `file:` URLs
/// stay; Blitz reads none. Blocking.
fn inline_images(html: &str) -> String {
    if !html.contains("file:") {
        return html.to_owned();
    }
    let Some(root) = inline_cache().and_then(|root| root.canonicalize().ok()) else { return html.to_owned() };
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("file://") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find(['"', '\'', '>', ')', ' ']).unwrap_or(tail.len());
        let source = &tail[..end];
        out.push_str(cached_image_data(source, &root).as_deref().unwrap_or(source));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The engine's inline-part cache (`cache_directory()` in `Engine/src/lib.rs`, after the `dirs`
/// crate): `$XDG_CACHE_HOME`, else `~/.cache`.
fn inline_cache() -> Option<PathBuf> {
    let caches = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
    Some(caches.join("dev.mailstream.Mailstream").join("messages-v2"))
}

/// `source` as a `data:` URL, if it names a raster image inside `root`, its real path included.
fn cached_image_data(source: &str, root: &Path) -> Option<String> {
    let url = url::Url::parse(source).ok()?;
    if url.scheme() != "file" || url.host().is_some() || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let path = url.to_file_path().ok()?.canonicalize().ok()?;
    if !path.starts_with(root) || std::fs::metadata(&path).ok()?.len() > MAX_INLINE_IMAGE {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    // The type comes from the bytes, not from a name the sender chose.
    let mime = raster_mime(&bytes)?;
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_a_click_opens() {
        assert!(is_mail_link("https://example.com/a"));
        assert!(is_mail_link("mailto:someone@example.com"));
        assert!(!is_mail_link("file:///etc/passwd"));
        assert!(!is_mail_link("https://user@example.com/"));
        assert!(!is_mail_link("javascript:alert(1)"));
    }

    #[test]
    fn inline_parts_become_data_urls_only_inside_the_cache() {
        let base = std::env::temp_dir().join(format!("mailstream-gpui-inline-{}", std::process::id()));
        let root = base.join("dev.mailstream.Mailstream").join("messages-v2");
        std::fs::create_dir_all(root.join("m1")).unwrap();
        std::fs::write(root.join("m1/logo.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        std::fs::write(root.join("m1/notes.txt"), b"plain").unwrap();
        std::fs::write(base.join("outside.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        let root = root.canonicalize().unwrap();
        let url = |path: &Path| url::Url::from_file_path(path).unwrap().to_string();

        let inside = cached_image_data(&url(&root.join("m1/logo.png")), &root).unwrap();
        assert!(inside.starts_with("data:image/png;base64,"));
        assert_eq!(cached_image_data(&url(&root.join("m1/notes.txt")), &root), None);
        assert_eq!(cached_image_data(&url(&base.join("outside.png")), &root), None);
        assert_eq!(cached_image_data(&url(&root.join("m1/../../../outside.png")), &root), None);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
