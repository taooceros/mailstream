import Cocoa
import Darwin
import FlutterMacOS
import WebKit

/// HTML mail bodies drawn by WebKit, the engine mail is written for, as Flutter `AppKitView`s of
/// type `mailstream/html`. Each view takes `html` (the mail as the engine gave it), its `owner`
/// (account, conversation, message, part), the inline cache's folder (`inlineCache`) and whether
/// its quoted history is open (`open`) as creation arguments, and reports the height its content
/// needs, so the reader sizes it to fit and scrolls it with the rest of the conversation. Every
/// view talks over the one channel `mailstream/html`, its calls carrying the view id: Flutter
/// keeps a channel it has listened on for good, so a channel per view added one with every body
/// ever shown (MS-154).
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
    // has none. `fold` opens or closes the page's quoted history.
    channel.setMethodCallHandler { [weak self] call, result in
      MainActor.assumeIsolated {
        guard call.method == "height" || call.method == "fold" else { return result(FlutterMethodNotImplemented) }
        guard let args = call.arguments as? [String: Any], let view = (args["view"] as? NSNumber)?.int64Value,
          let host = self?.hosts[view]?.host
        else { return result(nil) }
        if call.method == "fold" {
          host.fold(open: args["open"] as? Bool ?? false)
          result(nil)
        } else {
          result(host.height(forWidth: (args["width"] as? NSNumber)?.doubleValue))
        }
      }
    }
  }

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let args = args as? [String: Any]
    let key = MailHtmlView.Key(owner: args?["owner"] as? [String] ?? [], html: args?["html"] as? String ?? "")
    let page = idle.take(key) ?? MailHtmlView(key: key, inlineCache: args?["inlineCache"] as? String ?? "")
    page.fold(open: args?["open"] as? Bool ?? false)
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

  fileprivate func fold(open: Bool) {
    page.fold(open: open)
  }

  /// The page's height, its number, and whether it has quoted history to fold.
  fileprivate func send(height: Double, version: Int, quoted: Bool) {
    channel.invokeMethod("height", arguments: [id, height, version, quoted])
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

/// One body: the mail's HTML as the engine gave it, under the content policy the reader puts at
/// its start (`webKitDocument`), which allows HTTPS, data and `mscid:` resources only. The mail's
/// own scripts never run; links open in the browser only through `MailLink`; nothing navigates
/// the view. The page has no base URL, so it may read no local file: its inline images come from
/// the engine's inline cache through `InlinePartHandler`. Remote images load, without cookies. A
/// body wider than the column (a fixed 600 px newsletter) zooms out to fit. Quoted history folds
/// inside the page, behind the reader's toggle (MS-167).
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

  /// Whether the page found quoted history to fold, and whether the reader wants it open.
  private var quoted = false
  private var open = false

  /// WebKit ended the page's process; an idle page that lost it is not shown again.
  fileprivate(set) var terminated = false

  /// `inlineCache` is the folder of the engine's inline parts (`messages-v2`).
  init(key: Key, inlineCache: String) {
    self.key = key
    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = Self.store
    configuration.defaultWebpagePreferences.allowsContentJavaScript = false
    configuration.suppressesIncrementalRendering = true
    if key.html.contains("\(InlinePartHandler.scheme)://") {
      configuration.setURLSchemeHandler(InlinePartHandler(root: inlineCache), forURLScheme: InlinePartHandler.scheme)
    }
    let content = configuration.userContentController
    // The page's script runs in the app's own world, which the mail's disabled scripts never reach.
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

  /// The last height, its number and whether the page has quoted history, if it was measured at
  /// this width; otherwise the next measurement goes to the host.
  func height(forWidth width: Double?) -> [Any]? {
    guard let width, height >= 0, abs(measuredWidth - width) < 0.5 else {
      owed = true
      return nil
    }
    owed = false
    return [Double(height), version, quoted]
  }

  /// Opens or closes the page's quoted history, which lays the page out and measures it again.
  /// Before the page has found its quotes (still loading) the first measurement applies it.
  fileprivate func fold(open: Bool) {
    self.open = open
    if quoted { evaluateJavaScript("mailstreamFold(\(open))", in: nil, in: .defaultClient) }
  }

  /// The conversation scrolls as a whole: wheel events go on to Flutter.
  override func scrollWheel(with event: NSEvent) {
    nextResponder?.scrollWheel(with: event)
  }

  /// A reused page whose new host is another width measures even when its body keeps its size,
  /// since that host has not heard a height yet (a new width lays the page out again by itself).
  override func setFrameSize(_ newSize: NSSize) {
    let reflowed = abs(newSize.width - frame.width) >= 0.5
    super.setFrameSize(newSize)
    if reflowed, owed, height >= 0 { evaluateJavaScript("mailstreamMeasure()", in: nil, in: .defaultClient) }
  }

  /// The page's height in points goes to Flutter. A page whose quotes show otherwise than the
  /// reader wants folds first, which measures again.
  fileprivate func measured(height: Double, quoted: Bool, open: Bool) {
    self.quoted = quoted
    if quoted, open != self.open {
      fold(open: self.open)
      return
    }
    let points = height.rounded(.up)
    measuredWidth = frame.width
    if abs(points - self.height) >= 1 {
      self.height = points
      version += 1
    } else if !owed {
      return
    }
    guard let host else { return }
    owed = false
    host.send(height: Double(self.height), version: version, quoted: quoted)
  }

  /// Applied over the mail's own styles, as the native reader's frames are: the document sizes to
  /// its content, which the script reports on every change (layout, late images, the fold, a new
  /// width). The view's height is the viewport's, so nothing may size from the viewport. In quirks
  /// mode (mail without a doctype, as Outlook writes it) an `auto` height stretches `html` and
  /// `body` to the viewport, and a `height: 100%` (mail layout tables) passes over auto-height
  /// blocks up to it: the reported height then grows the view, which grows the page (MS-78,
  /// MS-160). A `fit-content` height is not `auto`: neither stretches, and percentages inside them
  /// resolve as auto, so the body ends where its content does in either mode.
  ///
  /// Mail wider than the view zooms out to fit, once per measurement, as the native reader's
  /// frames do: `html` is pinned to a width in pixels, the view's or, for content that needs more
  /// (a 600 or 900 px newsletter), its own minimum (`min-content`); the page is measured there and
  /// `html` zoomed by the view's width over the content's. The zoom never changes the width the
  /// page lays out at, so content as wide as the view plus a share of it (a `width: 100.775%`
  /// table) zooms out by that share once, instead of widening with every step out down to the 30%
  /// floor (MS-183). The height is measured zoomed.
  ///
  /// Quoted history (Gmail, Outlook, Thunderbird and Apple Mail markup: the outermost element
  /// matching, as `splitMailHtml` cuts it out for the other renderers) is hidden until the reader's
  /// toggle opens it (`mailstreamFold`), then shows in place with a rule at its left. Mail that is
  /// nothing but quotes (no text or image outside them) shows whole, with no toggle.
  private static let script = """
  (() => {
    const doc = document, root = doc.documentElement, body = doc.body;
    const style = doc.createElement('style');
    style.textContent = `
      html, body { height: fit-content !important; min-height: 0 !important; overflow: hidden !important; }
      body { margin: 0; padding: 2px 0; color: #1f1f1f; font: 14px/1.45 -apple-system, system-ui, sans-serif;
        overflow-wrap: break-word; }
      img { max-width: 100%; }
      pre { white-space: pre-wrap; }
      html[data-mailstream-folded] [data-mailstream-quote] { display: none !important; }
      [data-mailstream-quote] { border-left: 2px solid rgba(0, 0, 0, 0.1); padding-left: 12px; }
    `;
    (doc.head || root).appendChild(style);
    const quotes = [];
    const isQuote = (el) => el.classList.contains('gmail_quote') || el.classList.contains('gmail_quote_container')
      || el.classList.contains('moz-cite-prefix') || el.id === 'divRplyFwdMsg' || el.id === 'appendonsend'
      || (el.localName === 'blockquote' && el.getAttribute('type') === 'cite');
    const collect = (el) => {
      for (const child of el.children) isQuote(child) ? quotes.push(child) : collect(child);
    };
    if (body) collect(body);
    quotes.forEach((quote) => quote.setAttribute('data-mailstream-quote', ''));
    const outside = (node) => !(node.nodeType === 1 ? node : node.parentElement).closest('[data-mailstream-quote]');
    const kept = () => {
      const texts = doc.createTreeWalker(body, NodeFilter.SHOW_TEXT);
      for (let text = texts.nextNode(); text; text = texts.nextNode()) {
        if (text.data.trim() && outside(text)) return true;
      }
      return Array.from(body.querySelectorAll('img')).some(outside);
    };
    const quoted = quotes.length > 0 && kept();
    if (quoted) root.setAttribute('data-mailstream-folded', '');
    else quotes.forEach((quote) => quote.removeAttribute('data-mailstream-quote'));
    let pending = false;
    const pin = (width) => root.style.setProperty('width', width, 'important');
    const contentWidth = () => Math.max(root.scrollWidth, body ? body.scrollWidth : 0);
    const measure = () => {
      pending = false;
      const view = window.innerWidth;
      root.style.zoom = '';
      pin(view + 'px');
      let width = contentWidth();
      if (width > view + 1) {
        pin('min-content');
        pin(Math.max(view, root.offsetWidth) + 'px');
        width = contentWidth();
      }
      const zoom = width > view + 1 ? Math.max(view / width, 0.3) : 1;
      if (zoom !== 1) root.style.zoom = String(zoom);
      // Zoomed text keeps WebKit's minimum font size, so a zoomed page is taller than its unzoomed
      // height times the zoom: measure it zoomed (in the view's pixels).
      const box = (body || root).getBoundingClientRect();
      const margin = body ? parseFloat(getComputedStyle(body).marginBottom) || 0 : 0;
      window.webkit.messageHandlers.size.postMessage({
        height: box.bottom + window.scrollY + margin * zoom,
        quoted, open: !root.hasAttribute('data-mailstream-folded'),
      });
    };
    const schedule = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(measure);
    };
    new ResizeObserver(schedule).observe(body || root);
    window.addEventListener('resize', schedule);
    window.mailstreamMeasure = schedule;
    window.mailstreamFold = (open) => {
      if (open) root.removeAttribute('data-mailstream-folded');
      else root.setAttribute('data-mailstream-folded', '');
      schedule();
    };
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
    guard let size = message.body as? [String: Any], let height = (size["height"] as? NSNumber)?.doubleValue
    else { return }
    view?.measured(height: height, quoted: size["quoted"] as? Bool ?? false, open: size["open"] as? Bool ?? true)
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

/// Serves a body's inline images at `mscid://<message-key>/<stored-name>`, the addresses the
/// reader gives the engine's inline cache files (`webKitDocument`), as the native reader's
/// `PartSchemeHandler` does. Only a file whose canonical path is inside the inline cache, at most
/// 20 MB, whose bytes are PNG, JPEG, GIF or WebP, the rule HyperRender's preparation reads them by
/// (`cachedImageData`). Files are read off the main thread; a task WebKit stopped meanwhile is
/// never answered.
@MainActor private final class InlinePartHandler: NSObject, WKURLSchemeHandler {
  static let scheme = "mscid"
  private nonisolated static let limit = 20 << 20

  /// The inline cache's canonical folder; nil serves nothing.
  private let root: URL?
  /// Tasks started and not yet stopped or answered.
  private var active = Set<ObjectIdentifier>()

  init(root: String) {
    self.root = root.isEmpty ? nil : URL(fileURLWithPath: root, isDirectory: true).resolvingSymlinksInPath()
  }

  func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
    let name = task.request.url.map { String($0.path.drop(while: { $0 == "/" })) } ?? ""
    guard let root, let url = task.request.url, let key = url.host, !key.isEmpty, key.allSatisfy(\.isHexDigit),
      !name.isEmpty, !name.contains("/"), !name.hasPrefix(".")
    else { return task.didFailWithError(URLError(.fileDoesNotExist)) }
    let file = root.appendingPathComponent(key, isDirectory: true).appendingPathComponent(name)
    let id = ObjectIdentifier(task)
    active.insert(id)
    Task {
      let image = await Self.read(file, inside: root)
      guard active.remove(id) != nil else { return }
      guard let (data, mime) = image else { return task.didFailWithError(URLError(.fileDoesNotExist)) }
      task.didReceive(URLResponse(url: url, mimeType: mime, expectedContentLength: data.count, textEncodingName: nil))
      task.didReceive(data)
      task.didFinish()
    }
  }

  func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
    active.remove(ObjectIdentifier(task))
  }

  private nonisolated static func read(_ file: URL, inside root: URL) async -> (Data, String)? {
    let canonical = file.resolvingSymlinksInPath()
    guard canonical.path.hasPrefix(root.path + "/"),
      let size = (try? FileManager.default.attributesOfItem(atPath: canonical.path))?[.size] as? NSNumber,
      size.intValue <= limit, let data = try? Data(contentsOf: canonical), let mime = rasterMime(data)
    else { return nil }
    return (data, mime)
  }

  /// The MIME type the bytes are, not the one the file's name claims.
  private nonisolated static func rasterMime(_ data: Data) -> String? {
    let b = [UInt8](data.prefix(12))
    if b.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
    if b.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
    if b.starts(with: Array("GIF8".utf8)) { return "image/gif" }
    if b.count >= 12, b.starts(with: Array("RIFF".utf8)), Array(b[8..<12]) == Array("WEBP".utf8) { return "image/webp" }
    return nil
  }
}
