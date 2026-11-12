//! HTML mail bodies in native `WKWebView`s laid over the window.
//!
//! GPUI draws everything into one Metal layer, so a web view is a subview of the window's GPUI
//! `NSView`, placed each frame at the bounds its placeholder was laid out at and clipped to the
//! placeholder's content mask (the reader's scroll viewport). A view is as tall as its page, so it
//! never scrolls itself: it hands wheel events to the GPUI view, and the reader scrolls over a
//! body as over text.
//!
//! Each frame starts with [`begin_frame`] (painted first, at the window root): it removes the
//! views no placeholder painted in the frame before and hides the rest; [`paint`] shows the ones
//! still on screen. A view is sized to its page: once loaded, its height is read from the page
//! and the placeholder re-renders at that height. The page is the mail as the engine gave it
//! under the engine's content policy, with the engine's style sheet and quote fold
//! (`FFI/src/mail_html.rs`, as Flutter's `MailHtmlView`): its quoted history folds behind the
//! reader's toggle. Page scripts are off; links open in the browser when the engine's
//! `is_mail_link` allows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

use block2::RcBlock;
use gpui::{Bounds, Pixels, SharedString, Window};
use mailstream::{InlineParts, MailWebView as WebPage, is_mail_link, mail_web_document, mail_web_script, served_inline_part};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSResponder, NSView, NSWorkspace};
use objc2_foundation::{NSData, NSError, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLResponse};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType, WKUIDelegate,
    WKURLSchemeHandler, WKURLSchemeTask, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The engine's page script (style sheet, quotes marked and folded; it runs once per page).
static SETUP: LazyLock<String> = LazyLock::new(|| mail_web_script(WebPage::WebKit));

/// [`SETUP`], then the fold the reader asks for, answering the page's height and whether it has
/// quoted history.
fn measure(open: bool) -> String {
    format!(
        "{}\nmailstreamFold({open});\n\
         JSON.stringify([Math.ceil(document.documentElement.getBoundingClientRect().height), !!window.mailstreamQuoted])",
        *SETUP
    )
}

/// A measured page: its height, the width and fold it was measured at, and whether it has quoted
/// history.
#[derive(Clone, Copy)]
struct Measured {
    height: f32,
    width: f32,
    open: bool,
    quoted: bool,
}

struct Entry {
    clip: Retained<NSView>,
    web: Retained<MailWebView>,
    _delegate: Retained<Delegate>,
    measured: Rc<Cell<Option<Measured>>>,
    measuring: Rc<Cell<bool>>,
    painted: bool,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.clip.removeFromSuperview();
    }
}

#[derive(Default)]
struct Views {
    entries: HashMap<SharedString, Entry>,
    /// Set by [`begin_frame`] while something GPUI draws (a menu) covers the content.
    covered: bool,
}

thread_local! {
    static VIEWS: RefCell<Views> = RefCell::default();
}

/// The page height of the body shown under `key`, once measured.
pub fn height(key: &SharedString) -> Option<f32> {
    VIEWS.with_borrow(|views| views.entries.get(key).and_then(|e| e.measured.get()).map(|measured| measured.height))
}

/// Whether the body shown under `key` has quoted history to fold, once measured.
pub fn quoted(key: &SharedString) -> Option<bool> {
    VIEWS.with_borrow(|views| views.entries.get(key).and_then(|e| e.measured.get()).map(|measured| measured.quoted))
}

/// Starts a frame of the window: drops the views not painted last frame and hides the others
/// until [`paint`] places them. `covered` keeps them all hidden (a GPUI overlay is open).
pub fn begin_frame(covered: bool) {
    VIEWS.with_borrow_mut(|views| {
        views.covered = covered;
        views.entries.retain(|_, entry| std::mem::take(&mut entry.painted));
        for entry in views.entries.values() {
            entry.clip.setHidden(true);
        }
    });
}

/// Shows `html` under `key` at `bounds`, clipped to the current content mask, its quoted history
/// shown while `open`.
pub fn paint(key: &SharedString, html: &SharedString, open: bool, bounds: Bounds<Pixels>, window: &mut Window) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(host) = host_view(window) else { return };
    let pending = VIEWS.with_borrow_mut(|views| {
        let covered = views.covered;
        let entry = views.entries.entry(key.clone()).or_insert_with(|| Entry::new(html, &host, mtm));
        entry.painted = true;
        let clip = bounds.intersect(&window.content_mask().bounds);
        if covered || clip.size.width <= Pixels::ZERO || clip.size.height <= Pixels::ZERO {
            return entry.update(f32::from(bounds.size.width), open);
        }
        let host_height = host.bounds().size.height;
        let (x, y, w, h) = (f64::from(clip.origin.x), f64::from(clip.origin.y), f64::from(clip.size.width), f64::from(clip.size.height));
        entry.clip.setFrame(rect(x, host_height - y - h, w, h));
        let web_y = f64::from(bounds.origin.y) - y;
        let web_h = f64::from(bounds.size.height);
        entry.web.setFrame(rect(f64::from(bounds.origin.x) - x, h - web_y - web_h, f64::from(bounds.size.width), web_h));
        entry.clip.setHidden(false);
        entry.update(f32::from(bounds.size.width), open)
    });
    if pending {
        window.request_animation_frame();
    }
}

impl Entry {
    fn new(html: &str, host: &NSView, mtm: MainThreadMarker) -> Entry {
        let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe {
            configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            configuration.defaultWebpagePreferences().setAllowsContentJavaScript(false);
            let parts = InlinePartHandler::new(mtm);
            configuration.setURLSchemeHandler_forURLScheme(Some(ProtocolObject::from_ref(&*parts)), &NSString::from_str("mscid"));
        }
        let frame = rect(0., 0., 1., 1.);
        let this = MailWebView::alloc(mtm).set_ivars(());
        let web: Retained<MailWebView> =
            unsafe { msg_send![super(this), initWithFrame: frame, configuration: &*configuration] };
        let delegate = Delegate::new(mtm);
        unsafe {
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            web.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        let clip = NSView::initWithFrame(NSView::alloc(mtm), frame);
        clip.setClipsToBounds(true);
        clip.setHidden(true);
        clip.addSubview(&web);
        host.addSubview(&clip);
        // No base URL: the page reads no file; its inline images come through `InlinePartHandler`.
        let page = NSString::from_str(&mail_web_document(html.to_owned(), InlineParts::Served));
        unsafe { web.loadHTMLString_baseURL(&page, None) };
        Entry { clip, web, _delegate: delegate, measured: Rc::default(), measuring: Rc::default(), painted: true }
    }

    /// Measures the page once it has loaded, again when the width or the fold changes. True while
    /// the height is still to come.
    fn update(&self, width: f32, open: bool) -> bool {
        if unsafe { self.web.isLoading() } {
            return true;
        }
        if self.measuring.get() {
            return true;
        }
        if self.measured.get().is_some_and(|measured| measured.width == width && measured.open == open) {
            return false;
        }
        self.measuring.set(true);
        let (measured, measuring) = (self.measured.clone(), self.measuring.clone());
        let done = RcBlock::new(move |result: *mut AnyObject, _: *mut NSError| {
            measuring.set(false);
            let answer = unsafe { result.as_ref() }.and_then(|r| r.downcast_ref::<NSString>()).map(|s| s.to_string());
            let measurement = answer.and_then(|answer| serde_json::from_str::<(f64, bool)>(&answer).ok());
            // A page that cannot be measured keeps a modest height rather than asking forever.
            let (height, quoted) = measurement.map_or((200., false), |(height, quoted)| (height.max(1.) as f32, quoted));
            measured.set(Some(Measured { height, width, open, quoted }));
        });
        unsafe { self.web.evaluateJavaScript_completionHandler(&NSString::from_str(&measure(open)), Some(&done)) };
        true
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// The window's GPUI view.
pub fn host_view(window: &Window) -> Option<Retained<NSView>> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
}

/// Whether a click opens `url` (the engine's rule).
fn opens(url: &NSURL) -> bool {
    url.absoluteString().is_some_and(|link| is_mail_link(&link.to_string()))
}

define_class!(
    /// Serves a page's inline images at `mscid://<message>/<part>`, the addresses the engine gives
    /// its inline cache's files (`mail_web_document`), under the engine's rule
    /// (`served_inline_part`: canonical files inside the cache, at most 20 MB, raster bytes), as
    /// Flutter's `InlinePartHandler` does. Read on the main thread: an inline part is a small file.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MailstreamGPUIInlineParts"]
    struct InlinePartHandler;

    unsafe impl NSObjectProtocol for InlinePartHandler {}

    unsafe impl WKURLSchemeHandler for InlinePartHandler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, _: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let Some(url) = unsafe { task.request() }.URL() else { return };
            let part = url.absoluteString().and_then(|source| served_inline_part(&source.to_string()));
            let Some((bytes, mime)) = part else {
                let error = NSError::new(1, &NSString::from_str("MailstreamInlinePart"));
                return unsafe { task.didFailWithError(&error) };
            };
            let response = NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(
                NSURLResponse::alloc(),
                &url,
                Some(&NSString::from_str(mime)),
                bytes.len() as isize,
                None,
            );
            unsafe {
                task.didReceiveResponse(&response);
                task.didReceiveData(&NSData::from_vec(bytes));
                task.didFinish();
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self, _: &WKWebView, _: &ProtocolObject<dyn WKURLSchemeTask>) {}
    }
);

impl InlinePartHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    /// A web view that leaves scrolling to the reader around it.
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MailstreamGPUIMailWebView"]
    struct MailWebView;

    impl MailWebView {
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // The clip view's superview is the window's GPUI view.
            if let Some(host) = unsafe { self.superview().and_then(|clip| clip.superview()) } {
                host.scrollWheel(event);
            }
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MailstreamGPUIMailWebDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl WKNavigationDelegate for Delegate {
        /// Links open in the browser; only the body itself and `about:` frames load here.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let url = unsafe { action.request() }.URL();
            let kind = unsafe { action.navigationType() };
            if kind == WKNavigationType::LinkActivated {
                if let Some(url) = url.filter(|url| opens(url)) {
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
                handler.call((WKNavigationActionPolicy::Cancel,));
                return;
            }
            let scheme = url.and_then(|url| url.scheme()).map(|s| s.to_string().to_lowercase());
            let allowed = match scheme.as_deref() {
                Some("about") => kind == WKNavigationType::Other,
                _ => false,
            };
            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        /// `target=_blank` links: the browser, never a new web view.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create(
            &self,
            _: &WKWebView,
            _: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            if let Some(url) = unsafe { action.request() }.URL().filter(|url| opens(url)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            None
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}
