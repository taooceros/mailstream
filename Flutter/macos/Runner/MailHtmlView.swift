import Cocoa
import FlutterMacOS
import WebKit

/// HTML mail bodies drawn by WebKit, the engine mail is written for, as Flutter `AppKitView`s of
/// type `mailstream/html`. Each view takes `html` and its `owner` (account, conversation, message,
/// part) as creation arguments and reports the height its content needs on the channel
/// `mailstream/html/<view id>`, so the reader sizes it to fit and scrolls it with the rest of the
/// conversation.
///
/// Creating a WebKit page and loading its HTML holds the main thread for 20–35 ms, a visible stall
/// when a reader opens (MS-109). So each view is a light `MailHtmlHost` around a page: when the
/// host goes, its page waits idle, and showing the same owner and HTML again takes that page as
/// it was. A page never loads other HTML and shows in one host at a time.
/// Flutter makes platform views on the main thread.
@MainActor final class MailHtmlFactory: NSObject, @preconcurrency FlutterPlatformViewFactory {
  private let messenger: any FlutterBinaryMessenger
  private let idle = MailHtmlPages()

  init(messenger: any FlutterBinaryMessenger) {
    self.messenger = messenger
  }

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let args = args as? [String: Any]
    let key = MailHtmlView.Key(owner: args?["owner"] as? [String] ?? [], html: args?["html"] as? String ?? "")
    let channel = FlutterMethodChannel(name: "mailstream/html/\(viewId)", binaryMessenger: messenger)
    return MailHtmlHost(page: idle.take(key) ?? MailHtmlView(key: key), channel: channel, idle: idle)
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}

/// Pages no host shows, the most recently released last. Past the limit, the eight entries of the
/// reader's HTML parts cache, the oldest closes.
@MainActor final class MailHtmlPages {
  private static let limit = 8
  private var pages: [MailHtmlView] = []

  func take(_ key: MailHtmlView.Key) -> MailHtmlView? {
    guard let index = pages.lastIndex(where: { $0.key == key }) else { return nil }
    let page = pages.remove(at: index)
    return page.terminated ? nil : page
  }

  func put(_ page: MailHtmlView) {
    pages.removeAll { $0.key == page.key || $0.terminated }
    guard !page.terminated else { return }
    pages.append(page)
    if pages.count > Self.limit { pages.removeFirst() }
  }
}

/// One showing of a page: Flutter makes a host per platform view and releases it when the view
/// goes, which returns the page to the idle ones.
final class MailHtmlHost: NSView {
  private let page: MailHtmlView
  private let channel: FlutterMethodChannel
  private let idle: MailHtmlPages

  init(page: MailHtmlView, channel: FlutterMethodChannel, idle: MailHtmlPages) {
    self.page = page
    self.channel = channel
    self.idle = idle
    super.init(frame: .zero)
    addSubview(page)
    page.show(in: self)
    // Flutter asks once it listens, for the height at its width: a page measured before this
    // showing (or while Flutter was not listening yet) has already sent it.
    channel.setMethodCallHandler { [weak self] call, result in
      guard call.method == "height", let self else { return result(FlutterMethodNotImplemented) }
      result(self.page.height(forWidth: (call.arguments as? NSNumber)?.doubleValue))
    }
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  /// Until Flutter sizes the host, a reused page keeps the width it was measured at.
  override func resizeSubviews(withOldSize oldSize: NSSize) {
    if bounds.width > 0 { page.frame = bounds }
  }

  fileprivate func send(height: Double, version: Int) {
    channel.invokeMethod("height", arguments: [height, version])
  }

  isolated deinit {
    channel.setMethodCallHandler(nil)
    page.removeFromSuperview()
    page.hide()
    idle.put(page)
  }
}

/// One body. The mail's own scripts never run; links open in the browser; remote images load,
/// without cookies. Inline parts are `file://` copies in Mailstream's cache, which the page may
/// read. A body wider than the column (a fixed 600 px newsletter) zooms out to fit.
final class MailHtmlView: WKWebView {
  /// What a page shows, for its whole life.
  struct Key: Equatable {
    let owner: [String]
    let html: String
  }

  /// Mailstream's cache: inline parts live under it.
  private static let cache = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("dev.mailstream.Mailstream", isDirectory: true)

  /// Bodies share one cookie-less store, so tracking pixels see no history.
  private static let store = WKWebsiteDataStore.nonPersistent()

  let key: Key
  private let bridge = Bridge()
  private weak var host: MailHtmlHost?

  /// The last height in points, the width it was measured at, and its number, which grows with
  /// each new height so Flutter can drop a late, older one.
  private var height: CGFloat = -1
  private var measuredWidth: CGFloat = -1
  private var version = 0

  /// The host has not heard the height yet: the next measurement goes to it even if unchanged.
  private var owed = false

  /// WebKit ended the page's process; an idle page that lost it is not shown again.
  fileprivate(set) var terminated = false

  init(key: Key) {
    self.key = key
    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = Self.store
    configuration.defaultWebpagePreferences.allowsContentJavaScript = false
    configuration.suppressesIncrementalRendering = true
    let content = configuration.userContentController
    // The measuring script runs in the app's own world, which the mail's disabled scripts never reach.
    content.addUserScript(WKUserScript(source: Self.script, injectionTime: .atDocumentEnd, forMainFrameOnly: true, in: .defaultClient))
    content.add(bridge, contentWorld: .defaultClient, name: "size")
    super.init(frame: .zero, configuration: configuration)
    bridge.view = self
    navigationDelegate = bridge
    setValue(false, forKey: "drawsBackground")
    loadHTMLString(key.html, baseURL: Self.cache)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  fileprivate func show(in host: MailHtmlHost) {
    self.host = host
    owed = true
  }

  fileprivate func hide() {
    host = nil
  }

  /// The last height and its number, if it was measured at this width; otherwise the next
  /// measurement goes to the host.
  fileprivate func height(forWidth width: Double?) -> [Any]? {
    guard let width, height >= 0, abs(measuredWidth - width) < 0.5 else {
      owed = true
      return nil
    }
    owed = false
    return [Double(height), version]
  }

  /// The conversation scrolls as a whole: wheel events go on to Flutter.
  override func scrollWheel(with event: NSEvent) {
    nextResponder?.scrollWheel(with: event)
  }

  /// A new width measures again from full size. A reused page whose new host is another width
  /// measures even when its body keeps its size, since that host has not heard a height yet.
  override func setFrameSize(_ newSize: NSSize) {
    let widthChanged = newSize.width != frame.width
    let reflowed = abs(newSize.width - frame.width) >= 0.5
    super.setFrameSize(newSize)
    if widthChanged, pageZoom != 1 { pageZoom = 1 }
    if reflowed, owed, height >= 0 { evaluateJavaScript("mailstreamMeasure()", in: nil, in: .defaultClient) }
  }

  /// Content wider than the view zooms out until it fits; the height goes to Flutter in points.
  fileprivate func measured(height: Double, width: Double, room: Double) {
    if room > 0, width > room + 2 {
      pageZoom = max(pageZoom * room / width, 0.3)
      return  // The zoom lays the page out again, which measures again.
    }
    let points = (height * pageZoom).rounded(.up)
    measuredWidth = frame.width
    if abs(points - self.height) >= 1 {
      self.height = points
      version += 1
    } else if !owed {
      return
    }
    guard let host else { return }
    owed = false
    host.send(height: Double(self.height), version: version)
  }

  /// Applied over the mail's own styles, as the native reader's frames are: the document sizes to
  /// its content, which the script reports on every change (layout, late images, zoom). The view's
  /// height is the viewport's, so nothing may size from the viewport: in quirks mode a
  /// `height: 100%` (mail layout tables) passes over auto-height blocks up to the viewport, and
  /// the reported height then grows the view, which grows the table, without end. A flex `html`
  /// stops that walk and makes those percentages auto; `html` itself still stretches to the
  /// viewport in quirks mode, so the height is where the body ends.
  private static let script = """
  (() => {
    const doc = document, root = doc.documentElement;
    const style = doc.createElement('style');
    style.textContent = `
      html, body { height: auto !important; min-height: 0 !important; overflow: hidden !important; }
      html { display: flex !important; flex-direction: column !important; }
      body { margin: 0; padding: 2px 0; color: #1f1f1f; font: 14px/1.45 -apple-system, system-ui, sans-serif;
        overflow-wrap: break-word; }
      img { max-width: 100%; }
      pre { white-space: pre-wrap; }
    `;
    (doc.head || root).appendChild(style);
    let pending = false;
    const measure = () => {
      pending = false;
      const body = doc.body;
      const width = Math.max(root.scrollWidth, body ? body.scrollWidth : 0);
      const box = (body || root).getBoundingClientRect();
      const margin = body ? parseFloat(getComputedStyle(body).marginBottom) || 0 : 0;
      window.webkit.messageHandlers.size.postMessage({
        height: box.bottom + window.scrollY + margin, width, room: root.clientWidth,
      });
    };
    const schedule = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(measure);
    };
    new ResizeObserver(schedule).observe(doc.body || root);
    window.mailstreamMeasure = schedule;
    doc.querySelectorAll('img').forEach((img) => img.addEventListener('load', schedule));
    measure();
  })();
  """
}

/// The view's navigation delegate and message handler, held weakly by WebKit's controller.
private final class Bridge: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
  weak var view: MailHtmlView?

  func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
    guard let size = message.body as? [String: Any],
      let height = (size["height"] as? NSNumber)?.doubleValue,
      let width = (size["width"] as? NSNumber)?.doubleValue,
      let room = (size["room"] as? NSNumber)?.doubleValue
    else { return }
    view?.measured(height: height, width: width, room: room)
  }

  func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
    view?.terminated = true
  }

  /// The body loads; a clicked link opens in the default browser (or mail app) instead.
  func webView(
    _ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
    decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void
  ) {
    if action.navigationType == .linkActivated, let url = action.request.url {
      NSWorkspace.shared.open(url)
      decisionHandler(.cancel)
    } else {
      decisionHandler(action.navigationType == .other ? .allow : .cancel)
    }
  }
}
