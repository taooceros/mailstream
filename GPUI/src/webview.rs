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
//! and the placeholder re-renders at that height. Page scripts are off; links open in the
//! browser.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use block2::RcBlock;
use gpui::{Bounds, Pixels, SharedString, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSResponder, NSView, NSWorkspace};
use objc2_foundation::{NSError, NSNumber, NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType, WKUIDelegate, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Prepended to every body: no page margins, so the measured height is the content's; images and
/// long words fit the reading width.
const STYLE: &str = "<meta charset=\"utf-8\"><style>html,body{margin:0!important;padding:0;height:auto!important;\
overflow:hidden}html{color-scheme:light;background:#fff}body{font:15px/1.5 -apple-system,sans-serif;color:#1d1d1f;\
overflow-wrap:break-word}img{max-width:100%;height:auto}</style>";

const MEASURE: &str = "Math.ceil(document.documentElement.getBoundingClientRect().height)";

struct Entry {
    clip: Retained<NSView>,
    web: Retained<MailWebView>,
    _delegate: Retained<Delegate>,
    /// The page's height and the width it was measured at.
    measured: Rc<Cell<Option<(f32, f32)>>>,
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
    VIEWS.with_borrow(|views| views.entries.get(key).and_then(|e| e.measured.get()).map(|(height, _)| height))
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

/// Shows `html` under `key` at `bounds`, clipped to the current content mask.
pub fn paint(key: &SharedString, html: &SharedString, bounds: Bounds<Pixels>, window: &mut Window) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(host) = host_view(window) else { return };
    let pending = VIEWS.with_borrow_mut(|views| {
        let covered = views.covered;
        let entry = views.entries.entry(key.clone()).or_insert_with(|| Entry::new(html, &host, mtm));
        entry.painted = true;
        let clip = bounds.intersect(&window.content_mask().bounds);
        if covered || clip.size.width <= Pixels::ZERO || clip.size.height <= Pixels::ZERO {
            return entry.update(f32::from(bounds.size.width));
        }
        let host_height = host.bounds().size.height;
        let (x, y, w, h) = (f64::from(clip.origin.x), f64::from(clip.origin.y), f64::from(clip.size.width), f64::from(clip.size.height));
        entry.clip.setFrame(rect(x, host_height - y - h, w, h));
        let web_y = f64::from(bounds.origin.y) - y;
        let web_h = f64::from(bounds.size.height);
        entry.web.setFrame(rect(f64::from(bounds.origin.x) - x, h - web_y - web_h, f64::from(bounds.size.width), web_h));
        entry.clip.setHidden(false);
        entry.update(f32::from(bounds.size.width))
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
        let base = base_url(html).map(|url| NSURL::URLWithString(&NSString::from_str(&url)));
        let page = NSString::from_str(&format!("{STYLE}{html}"));
        unsafe { web.loadHTMLString_baseURL(&page, base.flatten().as_deref()) };
        Entry { clip, web, _delegate: delegate, measured: Rc::default(), measuring: Rc::default(), painted: true }
    }

    /// Measures the page once it has loaded, again when the width changes. True while the height
    /// is still to come.
    fn update(&self, width: f32) -> bool {
        if unsafe { self.web.isLoading() } {
            return true;
        }
        if self.measuring.get() {
            return true;
        }
        if self.measured.get().is_some_and(|(_, at)| at == width) {
            return false;
        }
        self.measuring.set(true);
        let (measured, measuring) = (self.measured.clone(), self.measuring.clone());
        let done = RcBlock::new(move |result: *mut AnyObject, _: *mut NSError| {
            measuring.set(false);
            let height = unsafe { result.as_ref() }.and_then(|r| r.downcast_ref::<NSNumber>()).map(|n| n.doubleValue());
            // A page that cannot be measured keeps a modest height rather than asking forever.
            measured.set(Some((height.map_or(200., |h| h.max(1.) as f32), width)));
        });
        unsafe { self.web.evaluateJavaScript_completionHandler(&NSString::from_str(MEASURE), Some(&done)) };
        true
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// The window's GPUI view.
fn host_view(window: &Window) -> Option<Retained<NSView>> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
}

/// The engine points inline images at files under the cache's `messages-v2` directory; the page's
/// base is that directory, so WebKit may read them.
fn base_url(html: &str) -> Option<String> {
    let start = html.find("file://")?;
    let rest = &html[start..];
    let end = rest.find("/messages-v2/")?;
    Some(format!("{}/messages-v2/", &rest[..end]))
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
                if let Some(url) = url {
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
                handler.call((WKNavigationActionPolicy::Cancel,));
                return;
            }
            let scheme = url.and_then(|url| url.scheme()).map(|s| s.to_string().to_lowercase());
            let allowed = match scheme.as_deref() {
                Some("about") => true,
                Some("file") => kind == WKNavigationType::Other,
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
            if let Some(url) = unsafe { action.request() }.URL() {
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
