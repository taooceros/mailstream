import Cocoa
import FlutterMacOS
import WebKit

/// HTML mail bodies drawn by WebKit, the engine mail is written for, as Flutter `AppKitView`s of
/// type `mailstream/html`. Each view takes `html` as its creation argument and reports the height
/// its content needs on the channel `mailstream/html/<view id>`, so the reader sizes it to fit and
/// scrolls it with the rest of the conversation.
final class MailHtmlFactory: NSObject, FlutterPlatformViewFactory {
  private let messenger: any FlutterBinaryMessenger

  init(messenger: any FlutterBinaryMessenger) {
    self.messenger = messenger
  }

  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let html = (args as? [String: Any])?["html"] as? String ?? ""
    let channel = FlutterMethodChannel(name: "mailstream/html/\(viewId)", binaryMessenger: messenger)
    return MailHtmlView(html: html, channel: channel)
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}

/// One body. The mail's own scripts never run; links open in the browser; remote images load,
/// without cookies. Inline parts are `file://` copies in Mailstream's cache, which the page may
/// read. A body wider than the column (a fixed 600 px newsletter) zooms out to fit.
final class MailHtmlView: WKWebView {
  /// Mailstream's cache: inline parts live under it.
  private static let cache = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("dev.mailstream.Mailstream", isDirectory: true)

  /// Bodies share one cookie-less store, so tracking pixels see no history.
  private static let store = WKWebsiteDataStore.nonPersistent()

  fileprivate let channel: FlutterMethodChannel
  private let bridge = Bridge()
  private var lastHeight: CGFloat = -1

  init(html: String, channel: FlutterMethodChannel) {
    self.channel = channel
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
    loadHTMLString(html, baseURL: Self.cache)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  /// The conversation scrolls as a whole: wheel events go on to Flutter.
  override func scrollWheel(with event: NSEvent) {
    nextResponder?.scrollWheel(with: event)
  }

  /// A new width measures again from full size.
  override func setFrameSize(_ newSize: NSSize) {
    let widthChanged = newSize.width != frame.width
    super.setFrameSize(newSize)
    if widthChanged, pageZoom != 1 { pageZoom = 1 }
  }

  /// Content wider than the view zooms out until it fits; the height goes to Flutter in points.
  fileprivate func measured(height: Double, width: Double, room: Double) {
    if room > 0, width > room + 2 {
      pageZoom = max(pageZoom * room / width, 0.3)
      return  // The zoom lays the page out again, which measures again.
    }
    let points = (height * pageZoom).rounded(.up)
    guard abs(points - lastHeight) >= 1 else { return }
    lastHeight = points
    channel.invokeMethod("height", arguments: points)
  }

  /// Applied over the mail's own styles, as the native reader's frames are: the document sizes to
  /// its content, which the script reports on every change (layout, late images, zoom).
  private static let script = """
  (() => {
    const doc = document, root = doc.documentElement;
    const style = doc.createElement('style');
    style.textContent = `
      html, body { height: auto !important; min-height: 0 !important; overflow: hidden !important; }
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
      window.webkit.messageHandlers.size.postMessage({
        height: root.getBoundingClientRect().height, width, room: root.clientWidth,
      });
    };
    const schedule = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(measure);
    };
    new ResizeObserver(schedule).observe(root);
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
