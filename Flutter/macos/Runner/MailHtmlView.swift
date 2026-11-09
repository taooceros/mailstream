import Cocoa
import Darwin
import FlutterMacOS
import WebKit

/// HTML mail bodies drawn by WebKit, the engine mail is written for, as Flutter `AppKitView`s of
/// type `mailstream/html`. Each view takes `html` and its `owner` (account, conversation, message,
/// part) as creation arguments and reports the height its content needs, so the reader sizes it
/// to fit and scrolls it with the rest of the conversation. Every view talks over the one channel
/// `mailstream/html`, its calls carrying the view id: Flutter keeps a channel it has listened on
/// for good, so a channel per view added one with every body ever shown (MS-154).
///
/// Creating a WebKit page and loading its HTML holds the main thread for 20–35 ms, a visible stall
/// when a reader opens (MS-109). So each view is a light `MailHtmlHost` around a page: when the
/// host goes, its page waits idle (`MailHtmlPages`), and showing the same owner and HTML again
/// takes that page as it was. A page never loads other HTML and shows in one host at a time.
/// Flutter makes platform views on the main thread.
@MainActor final class MailHtmlFactory: NSObject, @preconcurrency FlutterPlatformViewFactory {
  private let channel: FlutterMethodChannel
  private let idle = MailHtmlPages()
  /// The hosts showing, by view id.
  private var hosts: [Int64: HostRef] = [:]

  private struct HostRef {
    weak var host: MailHtmlHost?
  }

  init(messenger: any FlutterBinaryMessenger) {
    channel = FlutterMethodChannel(name: "mailstream/html", binaryMessenger: messenger)
    super.init()
    // Flutter asks once it listens, for the height at its width: a page measured before this
    // showing (or while Flutter was not listening yet) has already sent it. A view already gone
    // has none.
    channel.setMethodCallHandler { [weak self] call, result in
      MainActor.assumeIsolated {
        guard call.method == "height", let args = call.arguments as? [String: Any],
          let view = (args["view"] as? NSNumber)?.int64Value
        else { return result(FlutterMethodNotImplemented) }
        result(self?.hosts[view]?.host?.height(forWidth: (args["width"] as? NSNumber)?.doubleValue))
      }
    }
  }

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let args = args as? [String: Any]
    let key = MailHtmlView.Key(owner: args?["owner"] as? [String] ?? [], html: args?["html"] as? String ?? "")
    let page = idle.take(key) ?? MailHtmlView(key: key)
    let host = MailHtmlHost(id: viewId, page: page, channel: channel, idle: idle) { [weak self] in
      self?.hosts[viewId] = nil
    }
    hosts[viewId] = HostRef(host: host)
    return host
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}

/// Pages no host shows, the most recently released last. An idle page keeps its WebContent
/// process and the images it decoded: 15–20 MB for most mail, up to 300 MB for a newsletter full
/// of pictures (MS-155). So a page closes once it has waited a minute, and past eight pages (the
/// entries of the reader's HTML parts cache) or once the idle pages' processes together use more
/// than 200 MB, the oldest close first; the most recently released stays, for a reader opened
/// again right away.
@MainActor final class MailHtmlPages {
  private struct Idle {
    let page: MailHtmlView
    let until: ContinuousClock.Instant
  }

  private static let limit = 8
  private static let lifetime: Duration = .seconds(60)
  private static let budget: UInt64 = 200 << 20
  private var pages: [Idle] = []

  func take(_ key: MailHtmlView.Key) -> MailHtmlView? {
    guard let index = pages.lastIndex(where: { $0.page.key == key }) else { return nil }
    let page = pages.remove(at: index).page
    return page.terminated ? nil : page
  }

  func put(_ page: MailHtmlView) {
    close { $0.page.key == page.key || $0.page.terminated }
    guard !page.terminated else { return }
    pages.append(Idle(page: page, until: .now + Self.lifetime))
    var footprints = pages.map(\.page.footprint)
    var total = footprints.reduce(0, +)
    while pages.count > 1, pages.count > Self.limit || total > Self.budget {
      total -= footprints.removeFirst()
      pages.removeFirst().page.close()
    }
    Task { [weak self] in
      try? await Task.sleep(for: Self.lifetime)
      let now = ContinuousClock.now
      self?.close { $0.until <= now }
    }
  }

  private func close(where closes: (Idle) -> Bool) {
    pages.removeAll { idle in
      guard closes(idle) else { return false }
      idle.page.close()
      return true
    }
  }
}

/// One showing of a page: Flutter makes a host per platform view and releases it when the view
/// goes, which returns the page to the idle ones.
final class MailHtmlHost: NSView {
  private let id: Int64
  private let page: MailHtmlView
  private let channel: FlutterMethodChannel
  private let idle: MailHtmlPages
  private let gone: @MainActor () -> Void

  init(
    id: Int64, page: MailHtmlView, channel: FlutterMethodChannel, idle: MailHtmlPages,
    gone: @escaping @MainActor () -> Void
  ) {
    self.id = id
    self.page = page
    self.channel = channel
    self.idle = idle
    self.gone = gone
    super.init(frame: .zero)
    addSubview(page)
    page.show(in: self)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  /// Until Flutter sizes the host, a reused page keeps the width it was measured at.
  override func resizeSubviews(withOldSize oldSize: NSSize) {
    if bounds.width > 0 { page.frame = bounds }
  }

  fileprivate func height(forWidth width: Double?) -> [Any]? {
    page.height(forWidth: width)
  }

  fileprivate func send(height: Double, version: Int) {
    channel.invokeMethod("height", arguments: [id, height, version])
  }

  isolated deinit {
    gone()
    page.removeFromSuperview()
    page.hide()
    idle.put(page)
  }
}

/// The links mail may open, in either renderer: HTTP(S) with a host, or mailto, without
/// credentials. Anything else (file, javascript, custom schemes) is refused.
enum MailLink {
  static func open(_ url: URL) -> Bool {
    guard let scheme = url.scheme?.lowercased(), url.user == nil, url.password == nil,
          scheme == "mailto" || (["https", "http"].contains(scheme) && !(url.host ?? "").isEmpty)
    else { return false }
    NSWorkspace.shared.open(url)
    return true
  }
}

/// One body. The mail's own scripts never run; links open in the browser only through
/// `MailLink`; nothing navigates the view. The page has no base URL, so it may read no local
/// file: the reader hands over prepared HTML whose inline cache images are already data URLs,
/// with a content policy allowing HTTPS and data resources only. Remote images load, without
/// cookies. A body wider than the column (a fixed 600 px newsletter) zooms out to fit.
final class MailHtmlView: WKWebView {
  /// What a page shows, for its whole life.
  struct Key: Equatable {
    let owner: [String]
    let html: String
  }

  /// Bodies share one cookie-less store, so tracking pixels see no history.
  private static let store = WKWebsiteDataStore.nonPersistent()

  /// Every page alive, shown or idle, so closing one spares a process another still uses.
  private static let live = NSHashTable<MailHtmlView>.weakObjects()

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
    loadHTMLString(key.html, baseURL: nil)
    Self.live.add(self)
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

  /// The page's WebContent process, which only WebKit's private `_webProcessIdentifier` names;
  /// nil before it starts or once it ended.
  var process: pid_t? {
    guard responds(to: Selector(("_webProcessIdentifier"))),
      let pid = (value(forKey: "_webProcessIdentifier") as? NSNumber)?.int32Value, pid > 0
    else { return nil }
    return pid
  }

  /// The memory the page's process uses (its footprint, Activity Monitor's Memory), 0 without one.
  var footprint: UInt64 {
    guard let process else { return 0 }
    var usage = rusage_info_v4()
    let status = withUnsafeMutablePointer(to: &usage) {
      $0.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) { proc_pid_rusage(process, RUSAGE_INFO_V4, $0) }
    }
    return status == 0 ? usage.ri_phys_footprint : 0
  }

  /// Ends the page's process once the page is gone: call it when letting go of the page for good.
  /// WebKit closes a released page by dropping its process's connection and leaves the process to
  /// exit by itself, and the process of a page off screen runs at background priority: on a busy
  /// machine it got no time to exit, so every closed page kept its process and its images until
  /// the app quit (MS-153: 163 processes, 18 GB). WebKit's own `_killWebContentProcess` leaves the
  /// process running, detached. So on the next turn of the main loop, once WebKit has closed the
  /// page, the process gets SIGKILL, which needs no time from it, unless another live page uses
  /// it or it is no longer a WebContent process.
  func close() {
    guard let process else { return }
    Task { @MainActor in
      guard !Self.live.allObjects.contains(where: { $0.process == process }), Self.isWebContent(process) else { return }
      kill(process, SIGKILL)
    }
  }

  private static func isWebContent(_ process: pid_t) -> Bool {
    var path = [UInt8](repeating: 0, count: 4 * Int(MAXPATHLEN))
    let length = proc_pidpath(process, &path, UInt32(path.count))
    return length > 0 && String(decoding: path.prefix(Int(length)), as: UTF8.self).hasSuffix("/com.apple.WebKit.WebContent")
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
  /// height is the viewport's, so nothing may size from the viewport. In quirks mode (mail without
  /// a doctype, as Outlook writes it) an `auto` height stretches `html` and `body` to the viewport,
  /// and a `height: 100%` (mail layout tables) passes over auto-height blocks up to it: the
  /// reported height then grows the view, which grows the page. Zoomed out, each pass came back a
  /// fraction of a point taller and was rounded up, so the body grew a point a frame without end
  /// (MS-78, MS-160). A `fit-content` height is not `auto`: neither stretches, and percentages
  /// inside them resolve as auto, so the body ends where its content does in either mode.
  private static let script = """
  (() => {
    const doc = document, root = doc.documentElement;
    const style = doc.createElement('style');
    style.textContent = `
      html, body { height: fit-content !important; min-height: 0 !important; overflow: hidden !important; }
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
  /// The body's own load has been allowed; every later navigation is refused.
  private var loaded = false

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

  /// Only the body's own load (the first, main-frame about: navigation) proceeds: no frames,
  /// redirects or meta refreshes, not even back to about:blank. A clicked link opens in the
  /// browser (or mail app) when `MailLink` allows.
  func webView(
    _ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
    decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void
  ) {
    if action.navigationType == .linkActivated, let url = action.request.url {
      _ = MailLink.open(url)
      decisionHandler(.cancel)
    } else {
      let initial = !loaded && action.navigationType == .other && action.targetFrame?.isMainFrame == true
        && (action.request.url?.scheme ?? "about") == "about"
      if initial { loaded = true }
      decisionHandler(initial ? .allow : .cancel)
    }
  }
}
