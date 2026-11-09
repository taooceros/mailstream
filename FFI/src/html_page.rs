//! Mail HTML laid out and drawn by Blitz (MS-147), the reader's third renderer beside WebKit and
//! HyperRender. Every page lives on one thread of its own: parsing, styling, layout and drawing
//! never touch the front end's UI thread, which only asks for a page's height and for tiles, strips
//! of RGBA pixels it shows as they scroll into view.
//!
//! Mail safety: Blitz runs no scripts, follows no `<meta refresh>` and opens nothing on a click
//! (the front end asks [`HtmlPage::link_at`] and opens the link itself). Frames and plug-ins are
//! removed before anything is fetched, and linked stylesheets get nothing. A page reads raster
//! images only: `data:` ones and `https:` ones, fetched anonymously; nothing else is read, `file:`
//! included (the front end inlines the cached parts it allows as `data:`), and no web font, which
//! would join the font collection every page shares. SVG is off, as its renderer would read files an
//! SVG's `<image>` names.
use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex};

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig, FontContext, NodeId, QualName, local_name, ns};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};
use futures_channel::oneshot;
use mailstream_engine::avatars::web::{Internet, Web};
use peniko::kurbo::Rect;
use peniko::{Color, Fill};

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

/// Linked stylesheets: kept (Blitz attaches what they load to them), answered empty.
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
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        send(Job::Open { id, html });
        Arc::new(Self { id })
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

enum Job {
    Open { id: u64, html: String },
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

/// Remote images: fetched on a runtime of their own, so a slow server never holds the pages' thread.
static NET: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("mail-html-net")
        .enable_all()
        .build()
        .expect("the mail HTML network runtime starts")
});

/// Every page on the pages' thread.
struct Pages {
    pages: HashMap<u64, Page>,
    /// System fonts, scanned once: a font collection per page costs 30–40 ms.
    fonts: FontContext,
    /// Pages a resource arrived for, laid out again once no job waits.
    arrived: HashSet<u64>,
}

struct Page {
    doc: HtmlDocument,
    shell: Arc<PageShell>,
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
        use parley::fontique::{Collection, CollectionOptions, SourceCache};
        let mut fonts = FontContext {
            source_cache: SourceCache::new_shared(),
            collection: Collection::new(CollectionOptions { shared: true, system_fonts: true }),
        };
        // List bullets draw with Blitz's own font.
        fonts.collection.register_fonts(peniko::Blob::new(Arc::new(blitz_dom::BULLET_FONT) as _), None);
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
            Job::Open { id, html } => {
                let page = Page::open(id, &html, &self.fonts);
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

/// `html` parsed, its requests going to `net`.
fn document(html: &str, net: Arc<dyn NetProvider>, shell: Arc<PageShell>, fonts: &FontContext) -> HtmlDocument {
    HtmlDocument::from_html(
        html,
        DocumentConfig {
            net_provider: Some(net),
            shell_provider: Some(shell),
            font_ctx: Some(fonts.clone()),
            base_url: Some(BASE_URL.into()),
            viewport: Some(Viewport::new(1, 1, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    )
}

impl Page {
    fn open(id: u64, html: &str, fonts: &FontContext) -> Self {
        let shell = Arc::new(PageShell { id, quiet: AtomicBool::new(false) });
        let gate = Arc::new(Gate::default());
        let mut doc = document(html, gate.clone(), shell.clone(), fonts);
        let blocked = sanitize(doc.as_mut());
        gate.open(blocked);
        Self { doc, shell, width: 0.0, scale: 0.0, zoom: 1.0, height: 0.0, version: 0, waiting: Vec::new() }
    }

    fn lay_out(&mut self, width: f64, scale: f64) {
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

/// The URLs a page's requests may not have: those of the elements [`sanitize`] removes get no
/// answer, linked stylesheets an empty one.
#[derive(Default)]
struct Blocked {
    dropped: HashSet<String>,
    empty: HashSet<String>,
}

/// Removes [`REMOVED`] elements, maps [`BACKGROUNDS`] attributes and adds [`MAIL_CSS`].
fn sanitize(doc: &mut BaseDocument) -> Blocked {
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
    let (_, empty) = urls(doc, LINKS, &["href"]);
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
    Blocked { dropped, empty }
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

/// A page's resources: held until parsing ends and the removed elements' requests are known, then
/// answered by [`fetch`] unless [`Blocked`].
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    open: bool,
    held: Vec<(Request, Box<dyn NetHandler>)>,
    blocked: Blocked,
}

impl Gate {
    fn open(&self, blocked: Blocked) {
        let held = {
            let mut state = self.state.lock().unwrap();
            state.open = true;
            state.blocked = blocked;
            std::mem::take(&mut state.held)
        };
        for (request, handler) in held {
            self.fetch(0, request, handler);
        }
    }
}

impl NetProvider for Gate {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let mut state = self.state.lock().unwrap();
        if !state.open {
            state.held.push((request, handler));
            return;
        }
        let url = request.url.as_str();
        let (dropped, empty) = (state.blocked.dropped.contains(url), state.blocked.empty.contains(url));
        drop(state);
        if empty {
            refuse(request.url, handler);
        } else if !dropped {
            fetch(request.url, handler);
        }
    }
}

/// A raster image from `data:` (at once) or `https:`; anything else gets an empty answer.
fn fetch(url: Url, handler: Box<dyn NetHandler>) {
    match url.scheme() {
        "data" => match data_url::DataUrl::process(url.as_str()).ok().and_then(|data| data.decode_to_vec().ok()) {
            Some((bytes, _)) if is_raster_image(&bytes) => handler.bytes(url.to_string(), Bytes::from(bytes)),
            _ => refuse(url, handler),
        },
        "https" => {
            NET.spawn(async move {
                let page = Internet.get(url.to_string(), IMAGE_CAP).await;
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

/// PNG, JPEG, GIF, WebP, BMP or ICO, by their first bytes.
fn is_raster_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"BM")
        || bytes.starts_with(b"\0\0\x01\0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(html: &str) -> Page {
        Page::open(0, html, &Pages::new().fonts)
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
    fn frames_and_plugins_are_removed_and_linked_stylesheets_get_nothing() {
        let mut page = page(concat!(
            r#"<html><head><link rel="stylesheet" href="https://example.com/a.css"></head><body>"#,
            r#"<iframe src="https://example.com/frame"></iframe><iframe srcdoc="<p>inner</p>"></iframe>"#,
            r#"<object data="https://example.com/o"></object><p>Kept</p></body></html>"#,
        ));
        page.lay_out(400.0, 1.0);
        let doc = page.doc.as_ref();
        assert_eq!(doc.query_selector_all("iframe, object").unwrap().len(), 0);
        assert_eq!(doc.query_selector_all("p").unwrap().len(), 1);
        // The stylesheet's empty answer arrived, so nothing holds the page back from painting.
        page.doc.as_mut().resolve(0.0);
        assert!(!page.doc.as_ref().has_pending_critical_resources());
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
            fetch(Url::parse(&url).unwrap(), Box::new(Probe(got.clone())));
        }
        assert_eq!(*got.lock().unwrap(), [6, 0, 0, 0, 0, 0, 0]);
        assert!(is_raster_image(b"\x89PNG\r\n") && !is_raster_image(b"body { color: red }"));
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
        // before (images load, stylesheets and fonts get nothing either way), while `//host/…`
        // and `logo.png` resolve to a scheme `fetch` never reads, as WebKit's view, whose base
        // is `about:blank`, resolves them to nothing (MS-161).
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
}

