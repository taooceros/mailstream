//! Mail HTML laid out and drawn by Blitz (MS-147), the reader's third renderer beside WebKit and
//! HyperRender. Every page lives on one thread of its own: parsing, styling, layout and drawing
//! never touch the front end's UI thread, which only asks for a page's height and for tiles, strips
//! of RGBA pixels it shows as they scroll into view.
//!
//! Mail safety: Blitz runs no scripts, follows no `<meta refresh>` and opens nothing on a click
//! (the front end asks [`HtmlPage::link_at`] and opens the link itself). Frames and plug-ins are
//! removed before anything is fetched. A page reads what WebKit's view does, anonymously: `data:`
//! and `https:` raster images, `https:` stylesheets (linked, and what those and the mail's own
//! styles `@import`, one level deep) and the `https:` and `data:` web fonts they name; nothing
//! else, `file:` included (the front end inlines the cached parts it allows as `data:`). SVG is
//! off, as its renderer would read files an SVG's `<image>` names.
//!
//! Web fonts (MS-164) never hold a page back: it lays out and draws in system fonts at once, and a
//! font arriving later lays it out again, unless it comes [`LATE`] after the page was first shown,
//! when it only goes to the cache for the next open. A page reads only the font files whose
//! `unicode-range` covers some of its text (Blitz would read every one a stylesheet names: 54 for
//! two weights of Google Sans), at most [`MAX_FONTS`]. Stylesheets and fonts are kept in a cache on
//! disk for as long as their answers allow ([`ResourceCache`]), and each page registers its fonts
//! in a copy of its own of the system font collection, so one mail's font never changes another's.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig, FontContext, NodeId, QualName, local_name, ns};
use blitz_html::DocumentHtmlParser;
use blitz_paint::paint_scene;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};
use futures_channel::oneshot;
use mailstream_engine::avatars::web::{Browser, Web};
use peniko::kurbo::Rect;
use peniko::{Color, Fill};
use sha2::{Digest, Sha256};

/// The reader's look over the mail's own styles, the one WebKit's view applies
/// (`MailHtmlView.swift`): the document is as tall as its content, so nothing sizes from the
/// viewport (a flex `html` makes mail layout tables' `height: 100%` auto).
const MAIL_CSS: &str = "
html, body { height: auto !important; min-height: 0 !important; overflow: hidden !important; }
html { display: flex !important; flex-direction: column !important; width: 100% !important; }
body { margin: 0; padding: 2px 0; color: #1f1f1f; font: 14px/1.45 -apple-system, system-ui, sans-serif;
  overflow-wrap: break-word; }
img { max-width: 100%; }
pre { white-space: pre-wrap; }
";

/// Elements whose content would come from elsewhere: removed right after parsing, their requests
/// dropped unanswered.
const REMOVED: &str = "iframe, frame, frameset, object, embed";

/// Linked stylesheets: kept (Blitz attaches what they load to them); `https:` ones are read.
const LINKS: &str = "link[href]";

/// Elements whose `background` attribute is an image, which HTML maps to `background-image`; Blitz
/// maps no `background` attribute (only `bgcolor`), and mail built for Outlook draws its hero and
/// listing photos this way: Zillow's photos stayed their gray placeholder (MS-162).
const BACKGROUNDS: &str = "body[background], table[background], thead[background], tbody[background], \
    tfoot[background], tr[background], td[background], th[background]";

/// Marks each [`BACKGROUNDS`] element for the rule that gives it its image.
const BACKGROUND_MARK: &str = "data-mailstream-background";

/// What a mail's relative URLs resolve against. Blitz panics on a URL it can't resolve, and its
/// default base (`data:`) resolves none: a Google security alert's protocol-relative font link
/// (`//fonts.googleapis.com/…`) brought the page down (MS-161). Nothing is fetched from this
/// scheme, so `//host/…` and `logo.png` load nothing, as in WebKit's view, which has no base.
const BASE_URL: &str = "mailstream-body:///";

/// The viewport's height in points; only `vh` units read it, since pages are as tall as their content.
const VIEWPORT_HEIGHT: f64 = 800.0;

/// Content wider than the page zooms out to fit, down to this.
const MIN_ZOOM: f32 = 0.3;

/// The most of a remote image read; a larger one is not shown.
const IMAGE_CAP: usize = 16 << 20;

/// The most of a stylesheet read, and of a font file; a larger one is not applied.
const SHEET_CAP: usize = 512 << 10;
const FONT_CAP: usize = 2 << 20;

/// The most stylesheets (linked and imported) and remote font files one page reads.
const MAX_SHEETS: usize = 8;
const MAX_FONTS: usize = 8;

/// A font arriving this long after its page was first laid out only goes to the cache: swapping
/// it in would move text the reader has started on.
const LATE: Duration = Duration::from_secs(3);

/// How long a stylesheet or font is kept when its answer doesn't say.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// The most the stylesheet and font cache keeps on disk, and in memory.
const CACHE_CAP: u64 = 50 << 20;
const MEMORY_CAP: usize = 16 << 20;

/// A page's size at the width it was laid out for, in points, and its version, which grows each
/// time late content (an image) changes it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct HtmlSize {
    pub height: f64,
    pub version: i64,
}

/// `width` × `height` pixels, RGBA, 8 bits each, opaque, row by row.
#[derive(Clone, Debug, uniffi::Record)]
pub struct HtmlTile {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// One mail body. Calls queue on the pages' thread and answer in order; [`HtmlPage::close`] (or
/// dropping the page) frees it.
#[derive(uniffi::Object)]
pub struct HtmlPage {
    id: u64,
}

#[uniffi::export]
impl HtmlPage {
    /// Starts parsing `html` on the pages' thread; returns at once.
    #[uniffi::constructor]
    pub fn new(html: String) -> Arc<Self> {
        Self::open(html, RESOURCES.clone())
    }

    /// Lays the page out `width` points wide for `scale` pixels per point (unless it already
    /// is) and gives its height. `None` once closed.
    pub async fn layout(&self, width: f64, scale: f64) -> Option<HtmlSize> {
        let (reply, answer) = oneshot::channel();
        send(Job::Layout { id: self.id, width, scale, reply });
        answer.await.ok().flatten()
    }

    /// The part of the page from `top` down `height` points, laid out `width` points wide, in
    /// pixels at `scale` per point. `None` once closed.
    pub async fn tile(&self, width: f64, scale: f64, top: f64, height: f64) -> Option<HtmlTile> {
        let (reply, answer) = oneshot::channel();
        send(Job::Tile { id: self.id, width, scale, top, height, reply });
        answer.await.ok().flatten()
    }

    /// The `href` of the link at (`x`, `y`) points from the page's top left, as the mail wrote it.
    /// The caller decides whether to open it.
    pub async fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        send(Job::LinkAt { id: self.id, x, y, reply });
        answer.await.ok().flatten()
    }

    /// Selects the text from the point a drag started at to where it is now (points from the
    /// page's top left). Over no text, the drag's end keeps the last place it selected to; a start
    /// over no text selects nothing. Whether any text is selected.
    pub async fn select(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64) -> bool {
        let (reply, answer) = oneshot::channel();
        send(Job::Select { id: self.id, from: (from_x, from_y), to: (to_x, to_y), reply });
        answer.await.unwrap_or(false)
    }

    /// Clears the selection; whether there was one.
    pub async fn clear_selection(&self) -> bool {
        let (reply, answer) = oneshot::channel();
        send(Job::ClearSelection { id: self.id, reply });
        answer.await.unwrap_or(false)
    }

    /// The selected text, if any.
    pub async fn selected_text(&self) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        send(Job::SelectedText { id: self.id, reply });
        answer.await.ok().flatten()
    }

    /// The page's text, for screen readers.
    pub async fn text(&self) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        send(Job::Text { id: self.id, reply });
        answer.await.ok().flatten()
    }

    /// Waits until the page's version passes `version` (late content changed it) and gives the
    /// new one; `None` once closed.
    pub async fn changed(&self, version: i64) -> Option<i64> {
        let (reply, answer) = oneshot::channel();
        send(Job::Changed { id: self.id, version, reply });
        answer.await.ok()
    }

    /// Frees the page; later calls answer `None`. A pending [`HtmlPage::changed`] answers `None`.
    pub fn close(&self) {
        send(Job::Close { id: self.id });
    }
}

impl Drop for HtmlPage {
    fn drop(&mut self) {
        send(Job::Close { id: self.id });
    }
}

impl HtmlPage {
    /// A page reading its remote resources from `resources`.
    fn open(html: String, resources: Arc<Resources>) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        send(Job::Open { id, html, resources });
        Arc::new(Self { id })
    }
}

enum Job {
    Open { id: u64, html: String, resources: Arc<Resources> },
    Close { id: u64 },
    Layout { id: u64, width: f64, scale: f64, reply: oneshot::Sender<Option<HtmlSize>> },
    Tile { id: u64, width: f64, scale: f64, top: f64, height: f64, reply: oneshot::Sender<Option<HtmlTile>> },
    LinkAt { id: u64, x: f64, y: f64, reply: oneshot::Sender<Option<String>> },
    Select { id: u64, from: (f64, f64), to: (f64, f64), reply: oneshot::Sender<bool> },
    ClearSelection { id: u64, reply: oneshot::Sender<bool> },
    SelectedText { id: u64, reply: oneshot::Sender<Option<String>> },
    Text { id: u64, reply: oneshot::Sender<Option<String>> },
    Changed { id: u64, version: i64, reply: oneshot::Sender<i64> },
    /// A resource arrived for the page: it lays out again once the queue is empty.
    Arrived { id: u64 },
}

impl Job {
    /// The page the job is for.
    fn page(&self) -> u64 {
        match *self {
            Job::Open { id, .. }
            | Job::Close { id }
            | Job::Layout { id, .. }
            | Job::Tile { id, .. }
            | Job::LinkAt { id, .. }
            | Job::Select { id, .. }
            | Job::ClearSelection { id, .. }
            | Job::SelectedText { id, .. }
            | Job::Text { id, .. }
            | Job::Changed { id, .. }
            | Job::Arrived { id } => id,
        }
    }
}

static PAGES: LazyLock<Sender<Job>> = LazyLock::new(|| {
    let (jobs, queue) = mpsc::channel();
    std::thread::Builder::new()
        .name("mail-html".into())
        .spawn(move || Pages::new().run(queue))
        .expect("the mail HTML thread starts");
    jobs
});

fn send(job: Job) {
    // The thread lives as long as the process; a send fails only while it exits.
    let _ = PAGES.send(job);
}

/// Remote images, stylesheets and fonts: fetched on a runtime of their own, so a slow server never
/// holds the pages' thread.
static NET: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("mail-html-net")
        .enable_all()
        .build()
        .expect("the mail HTML network runtime starts")
});

/// Where pages read their remote resources: the web as a browser asks it, and the stylesheets and
/// fonts already read, under the engine's cache folder.
static RESOURCES: LazyLock<Arc<Resources>> = LazyLock::new(|| {
    Arc::new(Resources {
        web: Arc::new(Browser),
        cache: ResourceCache::new(mailstream_engine::cache_directory().join("reader-resources"), CACHE_CAP),
        late: LATE,
    })
});

/// Every page on the pages' thread.
struct Pages {
    pages: HashMap<u64, Page>,
    /// System fonts, scanned once (a scan per page costs 30–40 ms); each page lays out with a copy
    /// of its own, which the web fonts it reads join.
    fonts: FontContext,
    /// Pages a resource arrived for, laid out again once no job waits.
    arrived: HashSet<u64>,
}

struct Page {
    doc: BaseDocument,
    shell: Arc<PageShell>,
    gate: Arc<Gate>,
    /// What the page was last laid out for: width and scale (0 before the first layout), the zoom
    /// that fit its content and the height that gave.
    width: f64,
    scale: f64,
    zoom: f32,
    height: f64,
    version: i64,
    waiting: Vec<oneshot::Sender<i64>>,
}

impl Pages {
    fn new() -> Self {
        use parley::fontique::{Collection, CollectionOptions, FallbackKey, Script, SourceCache};
        let mut fonts = FontContext {
            source_cache: SourceCache::new_shared(),
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
        };
        // List bullets draw with Blitz's own font.
        fonts.collection.register_fonts(peniko::Blob::new(Arc::new(blitz_dom::BULLET_FONT) as _), None);
        // The fonts the system falls back to for the scripts mail is mostly written in, looked up
        // once: every page's copy would ask the system again (Han for every page, as parley adds
        // it to each fallback list).
        for script in [b"Latn", b"Hani", b"Cyrl", b"Grek", b"Arab", b"Hebr", b"Thai", b"Hira", b"Kana", b"Hang", b"Deva"] {
            fonts.collection.fallback_families(FallbackKey::new(Script::from_bytes(*script), None)).for_each(drop);
        }
        Self { pages: HashMap::new(), fonts, arrived: HashSet::new() }
    }

    fn run(mut self, queue: Receiver<Job>) {
        loop {
            let job = match queue.try_recv() {
                Ok(job) => job,
                Err(TryRecvError::Empty) => {
                    self.relayout_arrived();
                    match queue.recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    }
                }
                Err(TryRecvError::Disconnected) => return,
            };
            self.guard(job.page(), |pages| pages.handle(job));
        }
    }

    /// Runs `work` for page `id`. Blitz panics on some mail; the page it panicked on is dropped,
    /// so its calls answer `None`, and the thread carries on with every other page instead of
    /// dying and leaving every later one blank (MS-161).
    fn guard(&mut self, id: u64, work: impl FnOnce(&mut Self)) {
        if panic::catch_unwind(AssertUnwindSafe(|| work(self))).is_err() {
            self.pages.remove(&id);
            self.arrived.remove(&id);
        }
    }

    fn handle(&mut self, job: Job) {
        match job {
            Job::Open { id, html, resources } => {
                let page = Page::open(id, &html, &self.fonts, resources);
                self.pages.insert(id, page);
            }
            Job::Close { id } => {
                // Dropping the senders answers pending `changed` calls.
                self.pages.remove(&id);
                self.arrived.remove(&id);
            }
            Job::Layout { id, width, scale, reply } => {
                let size = self.pages.get_mut(&id).map(|page| {
                    page.lay_out(width, scale);
                    HtmlSize { height: page.height, version: page.version }
                });
                let _ = reply.send(size);
            }
            Job::Tile { id, width, scale, top, height, reply } => {
                let tile = self.pages.get_mut(&id).map(|page| page.tile(width, scale, top, height));
                let _ = reply.send(tile);
            }
            Job::LinkAt { id, x, y, reply } => {
                let _ = reply.send(self.pages.get(&id).and_then(|page| page.link_at(x, y)));
            }
            Job::Select { id, from, to, reply } => {
                let _ = reply.send(self.pages.get_mut(&id).is_some_and(|page| page.select(from, to)));
            }
            Job::ClearSelection { id, reply } => {
                let had = self.pages.get_mut(&id).is_some_and(|page| {
                    let had = page.doc.has_text_selection();
                    page.doc.clear_text_selection();
                    had
                });
                let _ = reply.send(had);
            }
            Job::SelectedText { id, reply } => {
                let text = self.pages.get(&id).and_then(|page| page.doc.get_selected_text());
                let _ = reply.send(text.filter(|text| !text.is_empty()));
            }
            Job::Text { id, reply } => {
                let _ = reply.send(self.pages.get(&id).map(|page| page.text()));
            }
            Job::Changed { id, version, reply } => {
                if let Some(page) = self.pages.get_mut(&id) {
                    if page.version > version {
                        let _ = reply.send(page.version);
                    } else {
                        page.waiting.push(reply);
                    }
                }
            }
            Job::Arrived { id } => {
                self.arrived.insert(id);
            }
        }
    }

    /// Takes in the resources that arrived and lays those pages out again at their last width;
    /// one whose height changed tells its waiters.
    fn relayout_arrived(&mut self) {
        for id in std::mem::take(&mut self.arrived) {
            self.guard(id, |pages| pages.relayout(id));
        }
    }

    fn relayout(&mut self, id: u64) {
        let Some(page) = self.pages.get_mut(&id) else { return };
        page.doc.as_mut().resolve(0.0);
        if page.scale > 0.0 {
            page.fit();
        }
        page.version += 1;
        let version = page.version;
        for reply in page.waiting.drain(..) {
            let _ = reply.send(version);
        }
    }
}

/// `html` parsed, its requests going to `net`, its fonts a copy of `fonts`.
///
/// Always as HTML, as WebKit's view reads it: Blitz's own entry point (`HtmlDocument::from_html`)
/// parses mail that starts with an XHTML doctype, an `<?xml` line or an `<html
/// xmlns="http://www.w3.org/1999/xhtml">` root as XML, where HTML that isn't well-formed XML (a
/// `<link>` without its `/>`) swallows the rest of the page: Linear's notifications, re-serialized
/// by the reader's preparation with their XHTML doctype first, came out blank (MS-173).
fn document(html: &str, net: Arc<dyn NetProvider>, shell: Arc<PageShell>, fonts: &FontContext) -> BaseDocument {
    use html5ever::tendril::TendrilSink as _;
    use html5ever::tree_builder::TreeBuilderOpts;
    let mut doc = BaseDocument::new(DocumentConfig {
        net_provider: Some(net),
        shell_provider: Some(shell),
        font_ctx: Some(fonts.clone()),
        base_url: Some(BASE_URL.into()),
        viewport: Some(Viewport::new(1, 1, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    let mut mutator = doc.mutate();
    // The options Blitz parses HTML with.
    let options = html5ever::ParseOpts {
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            drop_doctype: true,
            ..Default::default()
        },
        ..Default::default()
    };
    html5ever::parse_document(DocumentHtmlParser::new(&mut mutator), options).one(html);
    drop(mutator);
    doc
}

impl Page {
    fn open(id: u64, html: &str, fonts: &FontContext, resources: Arc<Resources>) -> Self {
        let shell = Arc::new(PageShell { id, quiet: AtomicBool::new(false) });
        let gate = Arc::new(Gate::new(resources));
        let mut doc = document(html, gate.clone(), shell.clone(), fonts);
        let allowed = sanitize(doc.as_mut());
        gate.open(allowed);
        Self { doc, shell, gate, width: 0.0, scale: 0.0, zoom: 1.0, height: 0.0, version: 0, waiting: Vec::new() }
    }

    fn lay_out(&mut self, width: f64, scale: f64) {
        // From now on, a font arriving late only goes to the cache.
        self.gate.shared.shown.get_or_init(Instant::now);
        if width == self.width && scale == self.scale {
            return;
        }
        self.width = width;
        self.scale = scale;
        self.zoom = 1.0;
        self.fit();
    }

    /// Lays out at the page's width; content wider than that zooms out until it fits, as WebKit's
    /// view does.
    fn fit(&mut self) {
        for _ in 0..4 {
            self.apply_viewport();
            self.doc.as_mut().resolve(0.0);
            let doc = self.doc.as_ref();
            let Some(root) = doc.try_root_element() else { break };
            let content = content_width(doc).max(root.scroll_width()) as f64;
            let room = self.width / self.zoom as f64;
            if content <= room + 2.0 || self.zoom <= MIN_ZOOM {
                break;
            }
            self.zoom = (self.zoom * (room / content) as f32).max(MIN_ZOOM);
        }
        let doc = self.doc.as_ref();
        let height = doc.try_root_element().map_or(0.0, |root| root.final_layout().size.height);
        self.height = (height as f64 * self.zoom as f64).ceil();
    }

    fn apply_viewport(&mut self) {
        let pixels = |points: f64| (points * self.scale).round().max(1.0) as u32;
        let mut viewport = Viewport::new(pixels(self.width), pixels(VIEWPORT_HEIGHT), self.scale as f32, ColorScheme::Light);
        viewport.set_zoom(self.zoom);
        self.doc.as_mut().set_viewport(viewport);
    }

    fn tile(&mut self, width: f64, scale: f64, top: f64, height: f64) -> HtmlTile {
        self.lay_out(width, scale);
        let (pixel_width, pixel_height) = ((width * scale).round().max(1.0) as u32, (height * scale).round().max(1.0) as u32);
        let zoom = self.zoom as f64;
        let doc = self.doc.as_mut();
        doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: top / zoom });
        let pixel_scale = scale * zoom;
        let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| {
                let all = Rect::new(0.0, 0.0, pixel_width as f64, pixel_height as f64);
                scene.fill(Fill::NonZero, Default::default(), Color::WHITE, None, &all);
                paint_scene(scene, doc, pixel_scale, pixel_width, pixel_height, 0, 0);
            },
            pixel_width,
            pixel_height,
        );
        doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
        HtmlTile { width: pixel_width, height: pixel_height, pixels }
    }

    /// A point in points from the page's top left, in the document's CSS pixels.
    fn css(&self, x: f64, y: f64) -> (f32, f32) {
        let zoom = self.zoom as f64;
        ((x / zoom) as f32, (y / zoom) as f32)
    }

    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (x, y) = self.css(x, y);
        let doc = self.doc.as_ref();
        let mut node = doc.get_node(doc.hit(x, y)?.node_id);
        while let Some(current) = node {
            if current.data.is_element_with_tag_name(&local_name!("a"))
                && let Some(href) = current.attr(local_name!("href"))
            {
                return Some(href.trim().to_string());
            }
            node = current.parent.and_then(|parent| doc.get_node(parent));
        }
        None
    }

    /// The body's text, its white space collapsed.
    fn text(&self) -> String {
        let doc = self.doc.as_ref();
        let body = doc.query_selector("body").ok().flatten().and_then(|id| doc.get_node(id));
        let text = body.or_else(|| doc.try_root_element()).map_or_else(String::new, |node| node.text_content());
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn select(&mut self, from: (f64, f64), to: (f64, f64)) -> bool {
        let (from, to) = (self.css(from.0, from.1), self.css(to.0, to.1));
        // Blitz asks for a redraw as the selection changes; the caller draws again itself.
        self.shell.quiet.store(true, Ordering::Relaxed);
        let doc = self.doc.as_mut();
        let selected = match doc.find_text_position(from.0, from.1) {
            None => {
                doc.clear_text_selection();
                false
            }
            Some((anchor, offset)) => {
                match doc.find_text_position(to.0, to.1) {
                    Some((focus, focus_offset)) => doc.set_text_selection(anchor, offset, focus, focus_offset),
                    None if !doc.has_text_selection() => doc.set_text_selection(anchor, offset, anchor, offset),
                    None => {}
                }
                doc.has_text_selection()
            }
        };
        self.shell.quiet.store(false, Ordering::Relaxed);
        selected
    }
}

/// How far right the document's tables and images reach, in CSS pixels: a fixed-width table
/// nested in a narrower one overflows its cell, which the root's scroll width does not count.
fn content_width(doc: &BaseDocument) -> f32 {
    let wide = doc.query_selector_all("table, img").unwrap_or_default();
    wide.iter()
        .filter_map(|&id| doc.get_node(id))
        .map(|node| node.absolute_position(0.0, 0.0).x + node.final_layout().size.width)
        .fold(0.0, f32::max)
}

/// What a page's requests may get, as [`sanitize`] finds it: the removed elements' URLs no
/// answer, the rest as [`Gate`] decides.
#[derive(Default)]
struct Allowed {
    dropped: HashSet<String>,
    /// The linked stylesheets.
    links: HashSet<String>,
    /// What the mail's own `<style>`s ask for.
    asks: Asks,
    /// The characters of the body's text, sorted, which a font file's `unicode-range` must cover.
    text: Vec<u32>,
}

/// Removes [`REMOVED`] elements, maps [`BACKGROUNDS`] attributes and adds [`MAIL_CSS`].
fn sanitize(doc: &mut BaseDocument) -> Allowed {
    let urls = |doc: &BaseDocument, selector: &str, names: &[&str]| -> (Vec<NodeId>, HashSet<String>) {
        let ids = doc.query_selector_all(selector).map(|ids| ids.to_vec()).unwrap_or_default();
        let urls = ids
            .iter()
            .filter_map(|&id| doc.get_node(id))
            .flat_map(|node| names.iter().filter_map(|&name| node.attr(blitz_dom::LocalName::from(name))))
            // As Blitz resolves them for its requests.
            .filter_map(|raw| doc.base_url().join(raw).ok())
            .map(|url| url.to_string())
            .collect();
        (ids, urls)
    };
    let (removed, dropped) = urls(doc, REMOVED, &["src", "data"]);
    let (_, links) = urls(doc, LINKS, &["href"]);
    let body = doc.query_selector("body").ok().flatten().and_then(|id| doc.get_node(id));
    let mut text: Vec<u32> = body.map(|body| body.text_content()).unwrap_or_default().chars().map(u32::from).collect();
    text.sort_unstable();
    text.dedup();
    let mut asks = Asks::default();
    for id in doc.query_selector_all("style").unwrap_or_default().iter() {
        let Some(css) = doc.get_node(*id).map(|node| node.text_content()) else { continue };
        let lower = css.to_ascii_lowercase();
        if lower.contains("@font-face") || lower.contains("@import") {
            asks.extend(scan(&css, doc.base_url(), &text));
        }
    }
    let backgrounds: Vec<(NodeId, String)> = doc
        .query_selector_all(BACKGROUNDS)
        .unwrap_or_default()
        .iter()
        .filter_map(|&id| Some((id, doc.get_node(id)?.attr(local_name!("background"))?.trim().to_string())))
        .filter(|(_, url)| !url.is_empty())
        .collect();
    let head = doc.query_selector("head").ok().flatten();
    let mut mutator = doc.mutate();
    for id in removed {
        if mutator.node_has_parent(id) {
            mutator.remove_and_drop_node(id);
        }
    }
    if let Some(head) = head {
        let style = |mutator: &mut blitz_dom::DocumentMutator, css: &str| {
            let style = mutator.create_element(QualName::new(None, ns!(html), local_name!("style")), Vec::new());
            let text = mutator.create_text_node(css);
            mutator.append_children(style, &[text]);
            style
        };
        if !backgrounds.is_empty() {
            // A presentational hint yields to every author rule: zero specificity, ahead of the
            // mail's own styles.
            let mut css = String::new();
            for (mark, (id, url)) in backgrounds.iter().enumerate() {
                mutator.set_attribute(*id, QualName::new(None, ns!(), BACKGROUND_MARK.into()), &mark.to_string());
                css.push_str(&format!(":where([{BACKGROUND_MARK}=\"{mark}\"]) {{ background-image: url(\"{}\") }}\n", css_string(url)));
            }
            let hints = style(&mut mutator, &css);
            mutator.prepend_nodes(head, &[hints]);
        }
        let reader = style(&mut mutator, MAIL_CSS);
        mutator.append_children(head, &[reader]);
    }
    drop(mutator);
    Allowed { dropped, links, asks, text }
}

/// `value` as the inside of a CSS double-quoted string.
fn css_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\{:x} ", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A page's link to its thread: Blitz calls it when a resource arrives.
struct PageShell {
    id: u64,
    /// Set while the pages' thread changes the selection, which needs no layout.
    quiet: AtomicBool,
}

impl ShellProvider for PageShell {
    fn request_redraw(&self) {
        if !self.quiet.load(Ordering::Relaxed) {
            send(Job::Arrived { id: self.id });
        }
    }
}

/// A page's resources: held until parsing ends and what the page may read is known, then
/// answered.
struct Gate {
    shared: Arc<GateShared>,
}

/// What a page's requests share with the fetches answering them.
struct GateShared {
    state: Mutex<GateState>,
    resources: Arc<Resources>,
    /// When the page was first laid out.
    shown: OnceLock<Instant>,
}

#[derive(Default)]
struct GateState {
    open: bool,
    held: Vec<(Request, Box<dyn NetHandler>)>,
    /// The removed elements' URLs: never answered.
    dropped: HashSet<String>,
    /// The stylesheets the page may read, each with whether its own `@import`s may be read: a
    /// linked one's may, an imported one's may not.
    sheets: HashMap<String, bool>,
    /// The font files of `@font-face` rules covering some of the page's text.
    fonts: HashSet<String>,
    /// What the page's stylesheets name that it never reads: fonts covering none of its text,
    /// imports more than one level deep.
    skipped: HashSet<String>,
    text: Arc<Vec<u32>>,
    sheets_read: usize,
    fonts_read: usize,
}

impl GateState {
    /// Takes in what a stylesheet asks for; its imports may be read when `imports` says so.
    fn learn(&mut self, asks: Asks, imports: bool) {
        for url in asks.fonts {
            self.skipped.remove(&url);
            self.fonts.insert(url);
        }
        for url in asks.skipped {
            if !self.fonts.contains(&url) {
                self.skipped.insert(url);
            }
        }
        for url in asks.imports {
            if imports {
                self.skipped.remove(&url);
                self.sheets.entry(url).or_insert(false);
            } else if !self.sheets.contains_key(&url) {
                self.skipped.insert(url);
            }
        }
    }
}

impl GateShared {
    /// Whether what arrives now comes [`Resources::late`] after the page was first shown.
    fn late(&self) -> bool {
        self.shown.get().is_some_and(|shown| shown.elapsed() >= self.resources.late)
    }

    /// A stylesheet: what it asks for is learned before Blitz reads it and asks.
    async fn read_sheet(self: Arc<Self>, url: Url, imports: bool, handler: Box<dyn NetHandler>) {
        let Some(css) = self.resources.read(&url, SHEET_CAP, |body| std::str::from_utf8(body).is_ok()).await else {
            return refuse(url, handler);
        };
        let text = self.state.lock().unwrap().text.clone();
        let asks = scan(std::str::from_utf8(&css).unwrap_or_default(), &url, &text);
        self.state.lock().unwrap().learn(asks, imports);
        handler.bytes(url.to_string(), css);
    }

    /// A font file; one arriving late is only cached.
    async fn read_font(self: Arc<Self>, url: Url, handler: Box<dyn NetHandler>) {
        if let Some(font) = self.resources.read(&url, FONT_CAP, is_font).await
            && !self.late()
        {
            handler.bytes(url.to_string(), font);
        }
    }
}

impl Gate {
    fn new(resources: Arc<Resources>) -> Self {
        let state = Mutex::new(GateState::default());
        Self { shared: Arc::new(GateShared { state, resources, shown: OnceLock::new() }) }
    }

    fn open(&self, allowed: Allowed) {
        let held = {
            let mut state = self.shared.state.lock().unwrap();
            state.open = true;
            state.dropped = allowed.dropped;
            state.text = Arc::new(allowed.text);
            state.sheets = allowed.links.into_iter().map(|url| (url, true)).collect();
            state.learn(allowed.asks, true);
            std::mem::take(&mut state.held)
        };
        for (request, handler) in held {
            self.fetch(0, request, handler);
        }
    }
}

impl NetProvider for Gate {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let mut state = self.shared.state.lock().unwrap();
        if !state.open {
            state.held.push((request, handler));
            return;
        }
        enum Read {
            Nothing,
            Empty,
            Sheet { imports: bool },
            Font,
            InlineFont,
            Image,
        }
        let url = request.url;
        let (key, https) = (url.as_str(), url.scheme() == "https");
        let read = if state.dropped.contains(key) || state.skipped.contains(key) {
            Read::Nothing
        } else if let Some(&imports) = state.sheets.get(key) {
            if https && state.sheets_read < MAX_SHEETS {
                state.sheets_read += 1;
                Read::Sheet { imports }
            } else {
                Read::Empty
            }
        } else if state.fonts.contains(key) {
            if https && state.fonts_read < MAX_FONTS {
                state.fonts_read += 1;
                Read::Font
            } else if url.scheme() == "data" {
                Read::InlineFont
            } else {
                Read::Nothing
            }
        } else {
            Read::Image
        };
        drop(state);
        match read {
            Read::Nothing => {}
            Read::Empty => refuse(url, handler),
            Read::Sheet { imports } => {
                NET.spawn(self.shared.clone().read_sheet(url, imports, handler));
            }
            Read::Font => {
                NET.spawn(self.shared.clone().read_font(url, handler));
            }
            Read::InlineFont => {
                let font = data_url::DataUrl::process(url.as_str()).ok().and_then(|data| data.decode_to_vec().ok());
                if let Some((font, _)) = font.filter(|(font, _)| font.len() <= FONT_CAP && is_font(font)) {
                    handler.bytes(url.to_string(), Bytes::from(font));
                }
            }
            Read::Image => fetch_image(url, handler, self.shared.resources.web.clone()),
        }
    }

    /// A linked stylesheet never holds the page back from laying out: Blitz waits for those in
    /// the `<head>` unless its provider says it never answers (MS-164).
    fn is_noop(&self) -> bool {
        true
    }
}

/// A raster image from `data:` (at once) or `https:`; anything else gets an empty answer.
fn fetch_image(url: Url, handler: Box<dyn NetHandler>, web: Arc<dyn Web>) {
    match url.scheme() {
        "data" => match data_url::DataUrl::process(url.as_str()).ok().and_then(|data| data.decode_to_vec().ok()) {
            Some((bytes, _)) if is_raster_image(&bytes) => handler.bytes(url.to_string(), Bytes::from(bytes)),
            _ => refuse(url, handler),
        },
        "https" => {
            NET.spawn(async move {
                let page = web.get(url.to_string(), IMAGE_CAP).await;
                match page.filter(|page| page.body.len() <= IMAGE_CAP && is_raster_image(&page.body)) {
                    Some(page) => handler.bytes(page.url, Bytes::from(page.body)),
                    None => refuse(url, handler),
                }
            });
        }
        _ => refuse(url, handler),
    }
}

/// An empty answer: an image stays empty, a stylesheet adds nothing.
fn refuse(url: Url, handler: Box<dyn NetHandler>) {
    handler.bytes(url.to_string(), Bytes::new());
}

/// What a stylesheet asks for: the files of its `@font-face` rules, split by whether their
/// `unicode-range` covers any of the page's text, and its `@import`s.
#[derive(Default)]
struct Asks {
    fonts: Vec<String>,
    skipped: Vec<String>,
    imports: Vec<String>,
}

impl Asks {
    fn extend(&mut self, other: Asks) {
        self.fonts.extend(other.fonts);
        self.skipped.extend(other.skipped);
        self.imports.extend(other.imports);
    }
}

/// What `css`, read from `base`, asks for, with `text` the page's characters (sorted). Read with
/// Blitz's own parser, so the URLs are the ones it asks for.
fn scan(css: &str, base: &Url, text: &[u32]) -> Asks {
    use style::context::QuirksMode;
    use style::font_face::Source;
    use style::media_queries::MediaList;
    use style::servo_arc::Arc as ServoArc;
    use style::shared_lock::SharedRwLock;
    use style::stylesheets::{AllowImportRules, CssRule, Origin, Stylesheet, StylesheetInDocument, UrlExtraData};

    let lock = SharedRwLock::new();
    let imports = Imports::default();
    let sheet = Stylesheet::from_str(
        css,
        UrlExtraData::from(base.clone()),
        Origin::Author,
        ServoArc::new(lock.wrap(MediaList::empty())),
        lock.clone(),
        Some(&imports),
        None,
        QuirksMode::NoQuirks,
        AllowImportRules::Yes,
    );
    let guard = lock.read();
    let mut asks = Asks { imports: imports.0.into_inner(), ..Asks::default() };
    // Blitz reads only top-level `@font-face` rules.
    for rule in sheet.contents(&guard).rules(&guard).iter() {
        let CssRule::FontFace(face) = rule else { continue };
        let face = &face.read_with(&guard).descriptors;
        let Some(sources) = &face.src else { continue };
        let urls = sources.0.iter().filter_map(|source| match source {
            Source::Url(source) => source.url.url().map(|url| url.as_str().to_string()),
            Source::Local(_) => None,
        });
        let covered = face.unicode_range.as_ref().is_none_or(|ranges| {
            ranges.iter().any(|range| {
                let first = text.partition_point(|&c| c < range.start);
                text.get(first).is_some_and(|&c| c <= range.end)
            })
        });
        if covered { asks.fonts.extend(urls) } else { asks.skipped.extend(urls) }
    }
    asks
}

/// Records a stylesheet's `@import`s and loads none of them.
#[derive(Default)]
struct Imports(RefCell<Vec<String>>);

impl style::stylesheets::StylesheetLoader for Imports {
    fn request_stylesheet(
        &self,
        url: style::values::CssUrl,
        location: style::values::SourceLocation,
        lock: &style::shared_lock::SharedRwLock,
        _media: style::servo_arc::Arc<style::shared_lock::Locked<style::media_queries::MediaList>>,
        supports: Option<style::stylesheets::import_rule::ImportSupportsCondition>,
        layer: style::stylesheets::import_rule::ImportLayer,
    ) -> style::servo_arc::Arc<style::shared_lock::Locked<style::stylesheets::ImportRule>> {
        if let Some(resolved) = url.url() {
            self.0.borrow_mut().push(resolved.as_str().to_string());
        }
        let stylesheet = style::stylesheets::import_rule::ImportSheet::new_refused();
        style::servo_arc::Arc::new(lock.wrap(style::stylesheets::ImportRule { url, stylesheet, supports, layer, source_location: location }))
    }
}

/// Where a page's remote resources come from.
struct Resources {
    web: Arc<dyn Web>,
    cache: ResourceCache,
    /// See [`LATE`].
    late: Duration,
}

impl Resources {
    /// `url`'s body when `valid` takes it: from the cache while it is fresh, else from the web,
    /// then kept for as long as the answer allows.
    async fn read(&self, url: &Url, cap: usize, valid: fn(&[u8]) -> bool) -> Option<Bytes> {
        if let Some(body) = self.cache.get(url.as_str()) {
            return Some(body);
        }
        let page = self.web.get(url.to_string(), cap).await?;
        if page.body.len() > cap || !valid(&page.body) {
            return None;
        }
        let body = Bytes::from(page.body);
        let lifetime = page.max_age.unwrap_or(DEFAULT_LIFETIME);
        if !lifetime.is_zero() {
            self.cache.put(url.as_str(), &body, lifetime);
        }
        Some(body)
    }
}

/// Stylesheets and fonts kept between opens: a file each in `folder`, named by its URL's hash and
/// holding when it expires, the URL and the body, at most `cap` bytes in all (the least recently
/// used go first); those read this run are also kept in memory.
struct ResourceCache {
    folder: PathBuf,
    cap: u64,
    memory: Mutex<Memory>,
}

#[derive(Default)]
struct Memory {
    entries: HashMap<String, (Bytes, SystemTime)>,
    /// The entries' URLs, oldest first.
    order: VecDeque<String>,
    size: usize,
}

impl ResourceCache {
    fn new(folder: PathBuf, cap: u64) -> Self {
        Self { folder, cap, memory: Mutex::default() }
    }

    /// `url`'s body while it has not expired.
    fn get(&self, url: &str) -> Option<Bytes> {
        let now = SystemTime::now();
        if let Some((body, expires)) = self.memory.lock().unwrap().entries.get(url)
            && *expires > now
        {
            return Some(body.clone());
        }
        let path = self.file(url);
        let data = Bytes::from(fs::read(&path).ok()?);
        let expires = UNIX_EPOCH + Duration::from_secs(u64::from_le_bytes(data.get(..8)?.try_into().ok()?));
        let length = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?) as usize;
        if data.get(12..12 + length)? != url.as_bytes() {
            return None;
        }
        if expires <= now {
            let _ = fs::remove_file(&path);
            return None;
        }
        // Used now: the size cap removes it last.
        let _ = File::options().write(true).open(&path).and_then(|file| file.set_modified(now));
        let body = data.slice(12 + length..);
        self.remember(url, &body, expires);
        Some(body)
    }

    /// Keeps `body` as `url`'s for `lifetime`.
    fn put(&self, url: &str, body: &Bytes, lifetime: Duration) {
        let expires = SystemTime::now().checked_add(lifetime).unwrap_or(SystemTime::now());
        self.remember(url, body, expires);
        if self.write(url, body, expires).is_ok() {
            self.trim();
        }
    }

    fn file(&self, url: &str) -> PathBuf {
        let hash = Sha256::digest(url.as_bytes());
        self.folder.join(hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
    }

    fn remember(&self, url: &str, body: &Bytes, expires: SystemTime) {
        if body.len() > MEMORY_CAP {
            return;
        }
        let mut memory = self.memory.lock().unwrap();
        match memory.entries.insert(url.to_string(), (body.clone(), expires)) {
            Some((old, _)) => memory.size -= old.len(),
            None => memory.order.push_back(url.to_string()),
        }
        memory.size += body.len();
        while memory.size > MEMORY_CAP {
            let Some(oldest) = memory.order.pop_front() else { break };
            if let Some((old, _)) = memory.entries.remove(&oldest) {
                memory.size -= old.len();
            }
        }
    }

    /// Written whole under another name, then moved in place, so a reader never sees half of it.
    fn write(&self, url: &str, body: &[u8], expires: SystemTime) -> std::io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(&self.folder)?;
        let seconds = expires.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let mut data = Vec::with_capacity(12 + url.len() + body.len());
        data.extend_from_slice(&seconds.to_le_bytes());
        data.extend_from_slice(&(url.len() as u32).to_le_bytes());
        data.extend_from_slice(url.as_bytes());
        data.extend_from_slice(body);
        let path = self.file(url);
        let partial = path.with_extension(format!("{}-{}.part", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::write(&partial, &data)?;
        fs::rename(&partial, &path)
    }

    /// Removes the least recently used files until the folder holds at most `cap` bytes.
    fn trim(&self) {
        let Ok(entries) = fs::read_dir(&self.folder) else { return };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let metadata = entry.metadata().ok()?;
                metadata.is_file().then(|| (metadata.modified().unwrap_or(UNIX_EPOCH), metadata.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, length, _)| length).sum();
        if total <= self.cap {
            return;
        }
        files.sort_by_key(|(used, _, _)| *used);
        for (_, length, path) in files {
            if total <= self.cap {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= length;
            }
        }
    }
}

/// PNG, JPEG, GIF, WebP, BMP or ICO, by their first bytes.
fn is_raster_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"BM")
        || bytes.starts_with(b"\0\0\x01\0")
}

/// WOFF, WOFF2, OpenType or TrueType, by their first bytes, as Blitz tells them apart.
fn is_font(bytes: &[u8]) -> bool {
    [b"wOFF", b"wOF2", b"OTTO", b"true", b"\0\x01\0\0"].iter().any(|magic| bytes.starts_with(*magic))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(html: &str) -> Page {
        Page::open(0, html, &Pages::new().fonts, offline())
    }

    /// A web of the files a test gives, recording what is asked; a file can be held back until
    /// the test lets it go.
    #[derive(Default)]
    struct FakeWeb {
        files: Mutex<HashMap<String, Vec<u8>>>,
        held: Mutex<HashMap<String, oneshot::Receiver<()>>>,
        asked: Mutex<Vec<String>>,
    }

    impl FakeWeb {
        fn file(&self, url: &str, body: impl Into<Vec<u8>>) {
            self.files.lock().unwrap().insert(url.into(), body.into());
        }

        /// Holds `url` back until the sender returned sends or drops.
        fn hold(&self, url: &str) -> oneshot::Sender<()> {
            let (release, held) = oneshot::channel();
            self.held.lock().unwrap().insert(url.into(), held);
            release
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Web for FakeWeb {
        fn get(&self, url: String, cap: usize) -> futures::future::BoxFuture<'static, Option<mailstream_engine::avatars::web::Page>> {
            self.asked.lock().unwrap().push(url.clone());
            let body = self.files.lock().unwrap().get(&url).cloned();
            let held = self.held.lock().unwrap().remove(&url);
            Box::pin(async move {
                if let Some(held) = held {
                    let _ = held.await;
                }
                let body = body?;
                Some(mailstream_engine::avatars::web::Page { body: body[..body.len().min(cap + 1)].to_vec(), url, max_age: None })
            })
        }

        fn txt(&self, _name: String) -> futures::future::BoxFuture<'static, Vec<String>> {
            Box::pin(async { Vec::new() })
        }
    }

    /// A cache folder of its own, empty.
    fn cache_folder() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = std::env::temp_dir().join(format!("ms164-cache-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&folder);
        folder
    }

    /// Resources from `web`, with a cache of their own.
    fn resources(web: Arc<FakeWeb>, late: Duration) -> Arc<Resources> {
        Arc::new(Resources { web, cache: ResourceCache::new(cache_folder(), CACHE_CAP), late })
    }

    /// Resources from a web that answers nothing.
    fn offline() -> Arc<Resources> {
        resources(Arc::default(), LATE)
    }

    /// Runs `work` on a thread of its own; the test fails when it takes over 10 s.
    fn in_time<T: Send + 'static>(work: impl Future<Output = T> + Send + 'static) -> T {
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(futures_executor::block_on(work));
        });
        result.recv_timeout(Duration::from_secs(10)).expect("in time")
    }

    /// The page's height, laid out 400 points wide, once `settled` takes it.
    async fn height_when(page: &HtmlPage, settled: impl Fn(f64) -> bool) -> f64 {
        loop {
            let size = page.layout(400.0, 1.0).await.expect("open");
            if settled(size.height) {
                return size.height;
            }
            page.changed(size.version).await.expect("open");
        }
    }

    fn pixel(tile: &HtmlTile, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * tile.width + x) * 4) as usize;
        tile.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_page_is_as_tall_as_its_content() {
        let mut short = page("<p>One line</p>");
        short.lay_out(400.0, 2.0);
        assert!(short.height > 10.0 && short.height < 60.0, "{}", short.height);
        // A layout table's `height: 100%` takes nothing from the viewport.
        let mut table = page(r#"<table height="100%" style="height:100%"><tr><td>Cell</td></tr></table>"#);
        table.lay_out(400.0, 2.0);
        assert!(table.height < 60.0, "{}", table.height);
    }

    #[test]
    fn mail_that_looks_like_xhtml_is_read_as_html() {
        // A Linear notification, as the reader's preparation writes it back: its XHTML doctype
        // first, then HTML that isn't well-formed XML (a `<link>` without its `/>`). Blitz's own
        // entry point read that as XML, where the `<link>` swallowed the page: a few points tall,
        // blank (MS-173). The same goes for an `<html xmlns=…>` root or an `<?xml` line.
        let body = concat!(
            r##"<body bgcolor="#F9F8F9" style="margin:0;padding:0">"##,
            r##"<table width="100%" bgcolor="#F9F8F9"><tbody><tr><td align="center">"##,
            r#"<link rel="preload" as="image" href="https://example.com/logo.png">"#,
            r#"<table id="main" width="502" style="width:502px;margin:24px;max-width:90vw" border="0" cellspacing="0" cellpadding="0">"#,
            r#"<tbody><tr><td><h1 style="font-size:24px;margin:0 0 24px">You have an unread notification</h1>"#,
            r#"<p>Someone&nbsp;commented on an issue:<br>First line</p><hr style="margin:12px 0">"#,
            r#"<div style="height:200px">Comment</div></td></tr></tbody></table>"#,
            "</td></tr></tbody></table></body></html>",
        );
        let head = r#"<head><meta http-equiv="Content-Type" content="text/html; charset=utf-8"><title>Notification</title></head>"#;
        let root = r#"<html dir="ltr" xmlns="http://www.w3.org/1999/xhtml" xmlns:v="urn:schemas-microsoft-com:vml">"#;
        let doctype = r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd">"#;
        for html in [
            format!("{doctype}{root}{head}{body}"),
            format!("{root}{head}{body}"),
            format!(r#"<?xml version="1.0" encoding="utf-8"?>{doctype}{root}{head}{body}"#),
        ] {
            let mut page = page(&html);
            page.lay_out(600.0, 1.0);
            assert!(page.height > 300.0, "{} for {}", page.height, &html[..40]);
            // Every element after the `<link>` is there, `&nbsp;` and `<br>` read as HTML.
            let text = page.text();
            assert_eq!(text, "You have an unread notificationSomeone commented on an issue:First lineComment");
        }
    }

    #[test]
    fn content_wider_than_the_page_zooms_out_to_fit() {
        let mut page = page(concat!(
            r#"<table width="100%"><tr><td align="center">"#,
            r#"<table width="600" style="max-width:600px"><tr><td>Newsletter</td></tr></table>"#,
            "</td></tr></table>",
        ));
        page.lay_out(300.0, 1.0);
        assert!((page.zoom - 0.5).abs() < 0.05, "zoom {}", page.zoom);
        page.lay_out(800.0, 1.0);
        assert_eq!(page.zoom, 1.0);
    }

    #[test]
    fn links_are_found_not_followed() {
        let mut page = page(r#"<a href=" https://example.com/a " style="display:block;height:40px">Link</a><p style="height:40px">Text</p>"#);
        page.lay_out(400.0, 1.0);
        assert_eq!(page.link_at(10.0, 10.0).as_deref(), Some("https://example.com/a"));
        assert_eq!(page.link_at(10.0, 60.0), None);
    }

    #[test]
    fn frames_and_plugins_are_removed_and_a_stylesheet_never_holds_the_page() {
        let web = Arc::new(FakeWeb::default());
        web.file("https://example.com/a.css", "p { margin: 40px }");
        let _held = web.hold("https://example.com/a.css");
        let mut page = Page::open(
            0,
            concat!(
                r#"<html><head><link rel="stylesheet" href="https://example.com/a.css"></head><body>"#,
                r#"<iframe src="https://example.com/frame"></iframe><iframe srcdoc="<p>inner</p>"></iframe>"#,
                r#"<object data="https://example.com/o"></object><p>Kept</p></body></html>"#,
            ),
            &Pages::new().fonts,
            resources(web.clone(), LATE),
        );
        // The stylesheet is still on its way, and the page lays out without it (MS-164).
        page.lay_out(400.0, 1.0);
        assert!(!page.doc.as_ref().has_pending_critical_resources());
        assert!(page.height > 10.0, "{}", page.height);
        let doc = page.doc.as_ref();
        assert_eq!(doc.query_selector_all("iframe, object").unwrap().len(), 0);
        assert_eq!(doc.query_selector_all("p").unwrap().len(), 1);
    }

    #[test]
    fn only_raster_images_are_read() {
        let file = std::env::temp_dir().join("ms147-file-probe.png");
        std::fs::write(&file, b"\x89PNG").unwrap();
        struct Probe(Arc<Mutex<Vec<usize>>>);
        impl NetHandler for Probe {
            fn bytes(self: Box<Self>, _url: String, bytes: Bytes) {
                self.0.lock().unwrap().push(bytes.len());
            }
        }
        let got = Arc::new(Mutex::new(Vec::new()));
        for url in [
            "data:image/png;base64,iVBORw0K".to_string(),
            "data:font/woff2;base64,d09GMgABAAA".into(),
            "data:text/css,p{color:red}".into(),
            format!("file://{}", file.display()),
            "http://example.com/a.png".into(),
            "javascript:alert(1)".into(),
            // A protocol-relative image, as the page resolves it.
            Url::parse(BASE_URL).unwrap().join("//example.com/a.png").unwrap().to_string(),
        ] {
            fetch_image(Url::parse(&url).unwrap(), Box::new(Probe(got.clone())), Arc::new(FakeWeb::default()));
        }
        assert_eq!(*got.lock().unwrap(), [6, 0, 0, 0, 0, 0, 0]);
        assert!(is_raster_image(b"\x89PNG\r\n") && !is_raster_image(b"body { color: red }"));
        assert!(is_font(b"wOF2\0\x01\0\0") && is_font(b"\0\x01\0\0\0\x10") && !is_font(b"body { color: red }"));
    }

    #[test]
    fn tiles_draw_the_part_asked_for() {
        let mut page = page(r#"<div style="height:100px;background:#ff0000"></div><div style="height:100px;background:#0000ff"></div>"#);
        let top = page.tile(100.0, 2.0, 0.0, 50.0);
        assert_eq!((top.width, top.height), (200, 100));
        let below = page.tile(100.0, 2.0, 120.0, 50.0);
        assert_eq!(pixel(&top, 100, 50)[..3], [255, 0, 0]);
        assert_eq!(pixel(&below, 100, 50)[..3], [0, 0, 255]);
    }

    #[test]
    fn a_drag_selects_text() {
        let mut page = page("<p style='margin:0'>Alpha beta gamma delta</p>");
        page.lay_out(400.0, 1.0);
        assert!(page.select((2.0, 8.0), (60.0, 8.0)));
        // Past the line's end the selection keeps where it reached.
        assert!(page.select((2.0, 8.0), (390.0, 8.0)));
        assert!(page.doc.get_selected_text().unwrap().starts_with("Alpha"));
        assert!(!page.select((10.0, 300.0), (20.0, 300.0)));
    }

    #[test]
    fn relative_urls_resolve_and_load_nothing() {
        // A Google security alert links its font protocol-relative; the default base made Blitz
        // panic on that (MS-161).
        let mut page = page(concat!(
            r#"<html><head><link href="//fonts.example.com/css?family=Sans" rel="stylesheet" type="text/css"></head>"#,
            r#"<body><img src="//images.example.com/logo.png" width="74" height="24"><img src="logo.png"><p>Kept</p></body></html>"#,
        ));
        page.lay_out(400.0, 1.0);
        page.doc.as_mut().resolve(0.0);
        assert!(!page.doc.as_ref().has_pending_critical_resources());
        assert_eq!(page.text(), "Kept");
    }

    #[test]
    fn absolute_https_urls_are_still_asked_for_and_relative_ones_go_nowhere() {
        // The base only gives relative URLs somewhere to resolve: `https:` ones are asked for as
        // before, while `//host/…` and `logo.png` resolve to a scheme nothing reads, as WebKit's
        // view, whose base is `about:blank`, resolves them to nothing (MS-161).
        struct Record(Mutex<HashSet<String>>);
        impl NetProvider for Record {
            fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
                self.0.lock().unwrap().insert(request.url.to_string());
                refuse(request.url, handler);
            }
        }
        let net = Arc::new(Record(Mutex::new(HashSet::new())));
        let shell = Arc::new(PageShell { id: 0, quiet: AtomicBool::new(false) });
        let mut doc = document(
            concat!(
                r#"<html><head><link href="//fonts.example.com/css?family=Sans" rel="stylesheet">"#,
                r#"<link href="https://fonts.example.com/css?family=Serif" rel="stylesheet"></head>"#,
                r#"<body><img src="//images.example.com/a.png"><img src="https://images.example.com/b.png"><img src="c.png">"#,
                r#"<div style="height:10px;background:url(//images.example.com/d.png)"></div>"#,
                r#"<div style="height:10px;background:url(https://images.example.com/e.png)"></div></body></html>"#,
            ),
            net.clone(),
            shell,
            &Pages::new().fonts,
        );
        doc.as_mut().set_viewport(Viewport::new(400, 800, 1.0, ColorScheme::Light));
        doc.as_mut().resolve(0.0);
        let asked = net.0.lock().unwrap();
        let https: HashSet<_> = asked.iter().filter(|url| url.starts_with("https://")).map(String::as_str).collect();
        assert_eq!(
            https,
            HashSet::from([
                "https://fonts.example.com/css?family=Serif",
                "https://images.example.com/b.png",
                "https://images.example.com/e.png"
            ])
        );
        let based: Vec<_> = asked.iter().filter(|url| !url.starts_with("https://")).collect();
        assert_eq!(based.len(), 4, "{based:?}");
        assert!(based.iter().all(|url| url.starts_with(BASE_URL.trim_end_matches('/'))), "{based:?}");
    }

    #[test]
    fn a_cells_background_attribute_draws_its_image() {
        // Zillow's listing photo is a card cell's `background` attribute over a gray
        // `background-color`, as mail built for Outlook draws hero images; Blitz maps no
        // `background` attribute, so the photo stayed gray (MS-162). A 2×2 red PNG stands in for it.
        const RED: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGP4z8AARAwQCgAf7gP9i18U1AAAAABJRU5ErkJggg==";
        let mut page = page(&format!(
            concat!(
                r#"<table width="200" cellpadding="0" cellspacing="0" border="0" style="width:200px"><tr>"#,
                r#"<td class="dmBgGray800" align="center" valign="top" background="{red}" "#,
                r#"style="background-repeat:no-repeat; background-position:center; background-size:cover; background-color:#dddde1">"#,
                r#"<a href="https://example.com/home"><div style="height:100px"></div></a></td></tr>"#,
                r#"<tr><td><div style="height:20px"></div></td></tr></table>"#,
                // An author rule over the attribute wins, as over any presentational hint.
                r#"<style>.plain {{ background-image: none }}</style>"#,
                r#"<table width="200" cellpadding="0" cellspacing="0" border="0"><tr>"#,
                r##"<td class="plain" background="{red}" bgcolor="#0000ff" height="50"></td></tr></table>"##,
            ),
            red = RED
        ));
        page.lay_out(200.0, 1.0);
        page.doc.as_mut().resolve(0.0);
        let tile = page.tile(200.0, 1.0, 0.0, 180.0);
        assert_eq!(pixel(&tile, 100, 50)[..3], [255, 0, 0]);
        assert_eq!(pixel(&tile, 100, 150)[..3], [0, 0, 255]);
    }

    #[test]
    fn a_page_blitz_panics_on_leaves_the_others_drawing() {
        use futures_executor::block_on;
        // Blitz panics on an image URL it can't resolve; the pages' thread used to die with it,
        // leaving every later page blank (MS-161).
        let broken = HtmlPage::new(r#"<img src="http://"><p>Broken</p>"#.into());
        assert!(block_on(broken.layout(400.0, 1.0)).is_none());
        let page = HtmlPage::new("<p>Next</p>".into());
        assert!(block_on(page.layout(400.0, 1.0)).is_some_and(|size| size.height > 0.0));
        assert!(block_on(page.tile(400.0, 1.0, 0.0, 20.0)).is_some());
    }

    const SHEET: &str = "https://fonts.test/css";
    const MONO: &str = "https://fonts.test/mono.ttf";

    /// A stylesheet giving 'Mail Mono' its Latin file, [`MONO`], and a Cyrillic one.
    fn mono_sheet() -> String {
        format!(concat!(
            "@font-face {{ font-family: 'Mail Mono'; src: url({mono}) format('truetype'); unicode-range: U+0000-00FF; }}\n",
            "@font-face {{ font-family: 'Mail Mono'; src: url(https://fonts.test/cyrillic.woff2) format('woff2'); unicode-range: U+0400-045F; }}\n",
        ), mono = MONO)
    }

    /// A paragraph in 'Mail Mono', which wraps onto many more lines in a monospaced font than in
    /// the system one, with `head`.
    fn mono_mail(head: &str) -> String {
        format!(
            r#"<html><head>{head}</head><body><p style="font-family:'Mail Mono',sans-serif;font-size:20px;width:200px;margin:0">{}</p></body></html>"#,
            "iiiii ".repeat(40)
        )
    }

    #[cfg(target_os = "macos")]
    fn monaco() -> Vec<u8> {
        fs::read("/System/Library/Fonts/Monaco.ttf").expect("macOS has Monaco")
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn web_fonts_arrive_after_the_first_layout_and_only_the_text_needs_are_read() {
        let web = Arc::new(FakeWeb::default());
        web.file(SHEET, format!("@import url(https://fonts.test/more.css);\n{}", mono_sheet()));
        web.file("https://fonts.test/more.css", "@import url(https://fonts.test/nested.css); p { color: #333 }");
        web.file(MONO, monaco());
        let release = web.hold(MONO);
        let resources = resources(web.clone(), LATE);
        let head = format!(
            r#"<link rel="stylesheet" href="{SHEET}"><link rel="stylesheet" href="http://fonts.test/plain.css"><link rel="stylesheet" href="//fonts.test/relative.css">"#
        );
        let page = HtmlPage::open(mono_mail(&head), resources.clone());
        let (system, mono) = in_time(async move {
            // The font is held back: the page lays out in the system font meanwhile.
            let system = page.layout(400.0, 1.0).await.unwrap().height;
            release.send(()).unwrap();
            let mono = height_when(&page, |height| height > system * 1.5).await;
            (system, mono)
        });
        assert!(system > 20.0, "{system}");
        let asked = web.asked();
        assert_eq!(
            HashSet::<&str>::from_iter(asked.iter().map(String::as_str)),
            HashSet::from([SHEET, "https://fonts.test/more.css", MONO]),
            "no Cyrillic file for Latin text, no second-level import, no `http:` or `//` sheet"
        );
        // Open again: everything from the cache, nothing asked.
        let page = HtmlPage::open(mono_mail(&head), resources);
        let again = in_time(async move { height_when(&page, |height| height > system * 1.5).await });
        assert_eq!(again, mono);
        assert_eq!(web.asked().len(), asked.len());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_font_arriving_late_is_only_cached_for_the_next_open() {
        let web = Arc::new(FakeWeb::default());
        web.file(SHEET, mono_sheet());
        web.file(MONO, monaco());
        let release = web.hold(MONO);
        // Every font is late once the page has been laid out.
        let first = resources(web.clone(), Duration::ZERO);
        let head = format!(r#"<link rel="stylesheet" href="{SHEET}">"#);
        let page = HtmlPage::open(mono_mail(&head), first.clone());
        let (page, system) = in_time(async move {
            let system = page.layout(400.0, 1.0).await.unwrap().height;
            release.send(()).unwrap();
            (page, system)
        });
        assert!((0..200).any(|_| {
            std::thread::sleep(Duration::from_millis(25));
            first.cache.get(MONO).is_some()
        }));
        std::thread::sleep(Duration::from_millis(300));
        let shown = in_time(async move { page.layout(400.0, 1.0).await.unwrap().height });
        assert_eq!(shown, system, "the late font is not swapped in");
        // The next run of the app reads the sheet and the font from the disk cache in time.
        let offline = Arc::new(FakeWeb::default());
        let next = Arc::new(Resources { web: offline.clone(), cache: ResourceCache::new(first.cache.folder.clone(), CACHE_CAP), late: LATE });
        let page = HtmlPage::open(mono_mail(&head), next);
        in_time(async move { height_when(&page, |height| height > system * 1.5).await });
        assert!(offline.asked().is_empty(), "{:?}", offline.asked());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_web_font_changes_only_the_page_that_read_it() {
        let web = Arc::new(FakeWeb::default());
        web.file(SHEET, mono_sheet());
        web.file(MONO, monaco());
        let release = web.hold(MONO);
        let reader = HtmlPage::open(mono_mail(&format!(r#"<link rel="stylesheet" href="{SHEET}">"#)), resources(web, LATE));
        let (system, mono, other) = in_time(async move {
            let system = reader.layout(400.0, 1.0).await.unwrap().height;
            release.send(()).unwrap();
            let mono = height_when(&reader, |height| height > system * 1.5).await;
            // A mail naming the same family, opened after the font arrived for the first one:
            // all pages used to share one collection, so it got the font too.
            let other = HtmlPage::open(mono_mail(""), offline());
            let other = other.layout(400.0, 1.0).await.unwrap().height;
            (system, mono, other)
        });
        assert!(system * 1.5 < mono, "{system} {mono}");
        assert_eq!(other, system);
    }

    #[test]
    fn font_files_covering_none_of_the_text_are_skipped() {
        let css = concat!(
            "@import 'more.css';\n",
            "@font-face { font-family: A; src: local(A), url(a-latin.woff2) format('woff2'), url(a.ttf); unicode-range: U+0000-00FF, U+2000-206F; }\n",
            "@font-face { font-family: A; src: url(https://cdn.test/a-cyrillic.woff2); unicode-range: U+0400-045F; }\n",
            "@font-face { font-family: B; src: url(data:font/woff2;base64,d09GMg); }\n",
            "@media screen { @font-face { font-family: C; src: url(c.woff2); } }\n",
        );
        let base = Url::parse("https://fonts.test/css/sheet.css").unwrap();
        let text = |text: &str| {
            let mut chars: Vec<u32> = text.chars().map(u32::from).collect();
            chars.sort_unstable();
            chars
        };
        let latin = scan(css, &base, &text("Hello"));
        assert_eq!(
            latin.fonts,
            ["https://fonts.test/css/a-latin.woff2", "https://fonts.test/css/a.ttf", "data:font/woff2;base64,d09GMg"]
        );
        assert_eq!(latin.skipped, ["https://cdn.test/a-cyrillic.woff2"]);
        assert_eq!(latin.imports, ["https://fonts.test/css/more.css"]);
        let cyrillic = scan(css, &base, &text("Привет"));
        assert_eq!(cyrillic.fonts, ["https://cdn.test/a-cyrillic.woff2", "data:font/woff2;base64,d09GMg"]);
    }

    #[test]
    fn the_cache_keeps_files_for_their_lifetime_within_its_size() {
        let folder = cache_folder();
        let url = |name: &str| format!("https://fonts.test/{name}.woff2");
        let body = |byte: u8| Bytes::from(vec![byte; 100]);
        // Each file is 100 bytes, its URL (26) and 12 bytes of header: 138.
        let cache = ResourceCache::new(folder.clone(), 300);
        cache.put(&url("a"), &body(1), Duration::from_secs(60));
        cache.put(&url("b"), &body(2), Duration::ZERO);
        // The next run reads them from disk: `a` while it lives, `b` no more.
        let next = ResourceCache::new(folder.clone(), 300);
        assert_eq!(next.get(&url("a")), Some(body(1)));
        assert_eq!(next.get(&url("b")), None);
        assert!(!next.file(&url("b")).exists());
        let pause = || std::thread::sleep(Duration::from_millis(20));
        pause();
        next.put(&url("c"), &body(3), Duration::from_secs(60));
        pause();
        assert!(ResourceCache::new(folder.clone(), 300).get(&url("a")).is_some());
        pause();
        // Three files are over the cap: the least recently used, `c`, goes.
        next.put(&url("d"), &body(4), Duration::from_secs(60));
        let last = ResourceCache::new(folder.clone(), 300);
        assert_eq!((last.get(&url("a")), last.get(&url("c")), last.get(&url("d"))), (Some(body(1)), None, Some(body(4))));
        let _ = fs::remove_dir_all(folder);
    }

    /// What a Google Fonts stylesheet costs a mail on the real network (MS-164): `cargo test
    /// --release -p mailstream --lib google_fonts_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads fonts.googleapis.com"]
    fn google_fonts_timing() {
        /// The browser's web, logging each URL read and its size.
        struct Counted(Arc<Mutex<Vec<(String, usize)>>>);
        impl Web for Counted {
            fn get(&self, url: String, cap: usize) -> futures::future::BoxFuture<'static, Option<mailstream_engine::avatars::web::Page>> {
                let (read, log) = (Browser.get(url.clone(), cap), self.0.clone());
                Box::pin(async move {
                    let page = read.await;
                    log.lock().unwrap().push((url, page.as_ref().map_or(0, |page| page.body.len())));
                    page
                })
            }
            fn txt(&self, _name: String) -> futures::future::BoxFuture<'static, Vec<String>> {
                Box::pin(async { Vec::new() })
            }
        }
        let mail = format!(
            concat!(
                r#"<html><head><link href="https://fonts.googleapis.com/css2?family=Roboto:wght@400;700&family=Google+Sans:wght@400;500&display=swap" rel="stylesheet"></head>"#,
                r#"<body><div style="font-family:'Google Sans',Roboto,Arial,sans-serif;font-size:24px">MS164 synthetic newsletter</div>{}</body></html>"#
            ),
            "<p style=\"font-family:Roboto,Arial,sans-serif\">We noticed a new sign-in to your account on a Mac device. <b>Check activity</b> if this wasn't you.</p>".repeat(20)
        );
        let log = Arc::new(Mutex::new(Vec::new()));
        let web: Arc<dyn Web> = Arc::new(Counted(log.clone()));
        let open = |resources: Arc<Resources>| {
            let start = Instant::now();
            let page = HtmlPage::open(mail.clone(), resources);
            let (page, painted) = in_time(async move {
                let size = page.layout(600.0, 2.0).await.unwrap();
                page.tile(600.0, 2.0, 0.0, 512.0).await.unwrap();
                (page, (start.elapsed(), size.version))
            });
            // Then until nothing more arrives for 1.5 s.
            let page = Arc::new(page);
            let (mut version, mut settled) = (painted.1, painted.0);
            loop {
                let (done, changed) = mpsc::channel();
                let watched = page.clone();
                std::thread::spawn(move || {
                    let _ = done.send(futures_executor::block_on(watched.changed(version)));
                });
                match changed.recv_timeout(Duration::from_millis(1500)) {
                    Ok(Some(next)) => (version, settled) = (next, start.elapsed()),
                    _ => break,
                }
            }
            (painted.0, settled)
        };
        let measure = |name: &str, resources: Arc<Resources>| {
            let from = log.lock().unwrap().len();
            let (painted, settled) = open(resources);
            let asked = log.lock().unwrap();
            let bytes: usize = asked[from..].iter().map(|(_, length)| length).sum();
            println!("{name}: first paint {painted:.1?}, last arrival laid out {settled:.1?}, {} requests, {bytes} bytes", asked.len() - from);
        };
        let resources = |folder: &PathBuf, web: Arc<dyn Web>| Arc::new(Resources { web, cache: ResourceCache::new(folder.clone(), CACHE_CAP), late: LATE });
        let offline = || resources(&cache_folder(), Arc::new(FakeWeb::default()));
        // The pages' thread starts and scans the system fonts once per process.
        open(offline());
        for run in 1..=3 {
            measure(&format!("{run}. without web fonts (as before MS-164)"), offline());
            let folder = cache_folder();
            let warm = resources(&folder, web.clone());
            measure(&format!("{run}. first open, empty cache"), warm.clone());
            measure(&format!("{run}. open again, same run"), warm);
            measure(&format!("{run}. after a restart, disk cache"), resources(&folder, web.clone()));
            let _ = fs::remove_dir_all(folder);
        }
        let mut paints: Vec<Duration> = (0..20)
            .map(|_| {
                let start = Instant::now();
                let page = HtmlPage::open(mail.clone(), offline());
                in_time(async move {
                    page.layout(600.0, 2.0).await.unwrap();
                    page.tile(600.0, 2.0, 0.0, 512.0).await.unwrap();
                });
                start.elapsed()
            })
            .collect();
        paints.sort();
        println!("first paint without web fonts, 20 opens: median {:.1?}, slowest {:.1?}", paints[10], paints[19]);
        let start = Instant::now();
        let fonts = Pages::new().fonts;
        println!("the pages' system fonts, scanned and their fallbacks looked up: {:.1?}", start.elapsed());
        let start = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(fonts.clone());
        }
        println!("a page's copy of the font collection: {:.1?}", start.elapsed() / 100);
    }
}

