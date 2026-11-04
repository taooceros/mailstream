import AppKit
import UniformTypeIdentifiers
import WebKit

/// A page rendered into the shell: a mail thread (`ReaderModel`) or a chat space (`ChatModel`).
@MainActor
protocol WebShellClient: AnyObject {
    /// A message the page posted while this client's render was current.
    func shell(_ shell: WebShell, received action: String, _ body: [String: Any])
    /// The web process died and a fresh shell is loading: render again.
    func shellDidReload(_ shell: WebShell)
}

/// The reader's one long-lived web page. It loads the shell (at launch, and again after its web
/// process dies) and hosts one client's render at a time: each `begin` starts a generation, the
/// page's messages from earlier ones are dropped, and scripts wait until the generation's render is
/// in the page, then run in order. Links open through `openLink`; nothing ever navigates the page.
@MainActor
final class WebShell: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    let webView: WKWebView
    /// Bumped per render, so work for an earlier one can tell it is stale.
    private(set) var generation = 0
    /// Runs once, after the next render is in the page.
    var onReady: (() -> Void)?

    private let openLink: (URL) -> Void
    private let world = WKContentWorld.world(name: "mailstream")
    private weak var client: (any WebShellClient)?
    /// The shell's navigation while it loads.
    private var shellLoad: WKNavigation?
    /// The render of the current generation, waiting for the shell to finish loading.
    private var staged: (script: String, state: [String: Any])?
    /// Whether the current generation's render has run; scripts sent before then wait in `queue`.
    private var rendered = false
    private var queue: [(script: String, arguments: [String: Any])] = []

    init(cache: MailCache, openLink: @escaping (URL) -> Void) {
        self.openLink = openLink
        let configuration = WKWebViewConfiguration()
        Self.allowFullFrameRate(configuration.preferences)
        configuration.setURLSchemeHandler(PartSchemeHandler(cache: cache), forURLScheme: MailCache.scheme)
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.websiteDataStore = .nonPersistent()
        configuration.suppressesIncrementalRendering = false
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        let controller = configuration.userContentController
        for source in [Self.script, ReaderPage.script, ChatPage.script] {
            controller.addUserScript(WKUserScript(source: source, injectionTime: .atDocumentEnd, forMainFrameOnly: true,
                                                  in: world))
        }
        controller.add(WeakScriptHandler(self), contentWorld: world, name: "ms")
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsMagnification = true
        webView.underPageBackgroundColor = .windowBackgroundColor
        loadShell()
    }

    /// Whether a render now waits for the shell page to load.
    var awaitsShell: Bool { shellLoad != nil }

    /// Hands the page to `client` for a new render.
    func begin(_ client: any WebShellClient) {
        reset()
        self.client = client
    }

    /// Renders the current generation by calling the page's `function` with `state`, at once or
    /// once the shell has loaded. The page takes on the accent color as the web view draws it now.
    func render(_ function: String, _ state: [String: Any]) {
        var state = state
        state["generation"] = generation
        webView.effectiveAppearance.performAsCurrentDrawingAppearance {
            state["accent"] = NSColor.controlAccentColor.cssRGBA
        }
        staged = ("\(function)(state)", state)
        rendered = false
        if shellLoad == nil { commit() }
    }

    /// Runs `script` in the current render's page, at once or as soon as the render is in.
    func call(_ script: String, _ arguments: [String: Any] = [:]) {
        guard client != nil else { return }
        guard rendered else { return queue.append((script, arguments)) }
        run(script, arguments)
    }

    /// Empties the page until the next `begin`.
    func clear() {
        reset()
        if shellLoad == nil { run("MS.clear(generation)", ["generation": generation]) }
    }

    private func reset() {
        client = nil
        generation += 1
        staged = nil
        rendered = false
        queue = []
    }

    private func loadShell() {
        shellLoad = webView.loadHTMLString(Self.page, baseURL: URL(string: "\(MailCache.scheme)://reader/"))
    }

    /// Sends the staged render; scripts queued meanwhile follow once it has run.
    private func commit() {
        guard let staged else { return }
        self.staged = nil
        let generation = generation
        webView.callAsyncJavaScript(staged.script, arguments: ["state": staged.state], in: nil, in: world) { [weak self] _ in
            guard let self, generation == self.generation else { return }
            rendered = true
            onReady?()
            onReady = nil
            let queued = queue
            queue = []
            for (script, arguments) in queued { run(script, arguments) }
        }
    }

    private func run(_ script: String, _ arguments: [String: Any]) {
        webView.callAsyncJavaScript(script, arguments: arguments, in: nil, in: world, completionHandler: nil)
    }

    // MARK: - WebKit

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any], let action = body["action"] as? String,
              body["generation"] as? Int == generation else { return }
        if action == "link" {
            if let href = body["href"] as? String, let url = URL(string: href) { openLink(url) }
            return
        }
        client?.shell(self, received: action, body)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard navigation === shellLoad else { return }
        shellLoad = nil
        commit()
    }

    /// The shell dies with its web process: load a new one and have the client render again.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        let client = client
        reset()
        loadShell()
        client?.shellDidReload(self)
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        let url = navigationAction.request.url
        let scheme = url?.scheme?.lowercased()
        if navigationAction.navigationType == .linkActivated, let url {
            openLink(url)
            decisionHandler(.cancel)
            return
        }
        // Allow only our own document and srcdoc frames; refreshes or redirects inside mail are refused.
        if scheme == "about" || (scheme == MailCache.scheme && navigationAction.targetFrame?.isMainFrame == true
                                 && navigationAction.navigationType == .other) {
            decisionHandler(.allow)
        } else {
            decisionHandler(.cancel)
        }
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url { openLink(url) }
        return nil
    }

    /// WebKit holds page rendering near 60 fps through its internal feature
    /// `PreferPageRenderingUpdatesNear60FPSEnabled`; turning it off lets the reader animate at the
    /// display's full rate (120 Hz on ProMotion). Private API, so anything missing makes this a no-op.
    private static func allowFullFrameRate(_ preferences: WKPreferences) {
        let list = NSSelectorFromString("_features"), set = NSSelectorFromString("_setEnabled:forFeature:")
        let key = NSSelectorFromString("key")
        guard let listMethod = class_getClassMethod(WKPreferences.self, list), preferences.responds(to: set) else { return }
        typealias List = @convention(c) (AnyClass, Selector) -> NSArray?
        typealias SetEnabled = @convention(c) (WKPreferences, Selector, Bool, NSObject) -> Void
        let features = unsafeBitCast(method_getImplementation(listMethod), to: List.self)(WKPreferences.self, list)
        guard let feature = (features as? [NSObject])?.first(where: { feature in
            feature.responds(to: key)
                && feature.perform(key)?.takeUnretainedValue() as? String == "PreferPageRenderingUpdatesNear60FPSEnabled"
        }) else { return }
        unsafeBitCast(preferences.method(for: set), to: SetEnabled.self)(preferences, set, false, feature)
    }

    // MARK: - Page

    /// The tokens, styles and an empty column; the scripts are user scripts. Its CSP allows remote
    /// images, styles and fonts so that no page needs a different shell; only mail bodies, which
    /// block them per frame, and escaped markup ever reach it. HTML mail renders in same-origin
    /// `srcdoc` frames that inherit this policy (no scripts).
    private static let page: String = {
        let csp = "default-src 'none'; img-src data: mscid: https: http:; style-src 'unsafe-inline' https: http:; "
            + "font-src data: https: http:; media-src data: mscid:; frame-src about:; child-src about:"
        return """
        <!doctype html><html><head><meta charset="utf-8">
        <meta http-equiv="Content-Security-Policy" content="\(csp)">
        <meta name="color-scheme" content="light dark">
        <style>\(Design.stylesheet)\n\(ReaderPage.css)</style></head><body><div class="column" id="column"></div></body></html>
        """
    }()

    /// What every page shares, as `Shell`: posting to the app, the column handoff, click routing and
    /// the composer's behavior. A page is an object with `act(el, action)` for clicks on its
    /// `data-action` elements and an optional `leave()`, run when another page takes the column.
    /// `MS` holds the app's entry points.
    private static let script = """
    (() => {
      // Messages carry the render they came from, so the app ignores clicks on a page it replaced.
      let generation = 0;
      let page = null;
      const post = (msg) => window.webkit.messageHandlers.ms.postMessage(Object.assign(msg, { generation }));
      const $ = (id) => document.getElementById(id);
      // The same five characters as `HTMLText.escape`.
      const escapeHTML = (text) => String(text).replace(/[&<>"']/g,
        (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

      // Puts `next`'s markup in the column, scrolled to the top, once the page there has left.
      function show(next, state) {
        if (page && page.leave) page.leave();
        page = next;
        generation = state.generation;
        if (state.accent) document.documentElement.style.setProperty('--accent', state.accent);
        $('column').innerHTML = state.html || '';
        window.scrollTo(0, 0);
      }

      // Links in `doc` open through the app; one inside a `skip` element is left to it.
      function interceptLinks(doc, skip) {
        doc.addEventListener('click', (event) => {
          const link = event.target.closest('a[href]');
          if (!link || (skip && event.target.closest(skip))) return;
          event.preventDefault();
          post({ action: 'link', href: link.href });
        }, true);
      }

      // A composer's text field grows with its text, from `minHeight`.
      function autosize(text, minHeight) {
        text.style.height = 'auto';
        text.style.height = Math.max(minHeight, text.scrollHeight) + 'px';
      }

      // Every edit and focus change runs `report`, so the app always has the draft; Enter sends
      // when `isSend(event)` says so.
      function bindComposer(text, minHeight, { report, isSend, send }) {
        text.addEventListener('input', () => { autosize(text, minHeight); report(); });
        text.addEventListener('focus', report);
        text.addEventListener('blur', report);
        text.addEventListener('keydown', (event) => {
          if (event.key === 'Enter' && isSend(event)) {
            event.preventDefault();
            send(text);
          }
        });
      }

      // Focuses `text` with the caret after its text, once the composer has settled in.
      function focusAtEnd(text) {
        setTimeout(() => {
          text.focus();
          text.setSelectionRange(text.value.length, text.value.length);
        }, 80);
      }

      // The page's own controls handle clicks on links inside them.
      interceptLinks(document, '[data-action]');
      document.addEventListener('click', (event) => {
        const el = event.target.closest('[data-action]');
        if (!el || !page) return;
        event.preventDefault();
        event.stopPropagation();
        const action = el.dataset.action;
        if (action === 'avatar') {
          const r = el.getBoundingClientRect();
          const msg = el.closest('.msg');
          post({ action, index: msg && msg.id ? Number(msg.id.slice(4)) : -1, email: el.dataset.avatar || '',
                 rect: [r.left, r.top, r.width, r.height] });
          return;
        }
        page.act(el, action);
      }, true);

      window.Shell = { post, $, escapeHTML, show, interceptLinks, autosize, bindComposer, focusAtEnd };
      window.MS = {
        clear(next) { show(null, { generation: next }); },
      };
    })();
    """
}

/// Breaks the retain cycle between `WKUserContentController` and its handler.
private final class WeakScriptHandler: NSObject, WKScriptMessageHandler {
    weak var target: (any WKScriptMessageHandler)?

    init(_ target: any WKScriptMessageHandler) { self.target = target }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        target?.userContentController(controller, didReceive: message)
    }
}

/// Serves cached MIME parts at `mscid://<message-key>/<stored-name>`, reading files off the main
/// thread; a task WebKit stopped meanwhile is never answered.
@MainActor
private final class PartSchemeHandler: NSObject, WKURLSchemeHandler {
    private let cache: MailCache
    /// Tasks started and not yet stopped or answered.
    private var active = Set<ObjectIdentifier>()

    init(cache: MailCache) { self.cache = cache }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let url = task.request.url, let key = url.host, key.allSatisfy(\.isHexDigit) else {
            task.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let name = url.path.removingPercentEncoding.map { String($0.drop(while: { $0 == "/" })) } ?? ""
        guard !name.isEmpty, !name.contains("/"), !name.hasPrefix(".") else {
            task.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let file = cache.messageDirectory(key: key).appendingPathComponent(name)
        let id = ObjectIdentifier(task)
        active.insert(id)
        Task(priority: .userInitiated) {
            let data = await Self.read(file)
            guard active.remove(id) != nil else { return }
            guard let data else {
                task.didFailWithError(URLError(.fileDoesNotExist))
                return
            }
            let mime = UTType(filenameExtension: file.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
            task.didReceive(URLResponse(url: url, mimeType: mime, expectedContentLength: data.count, textEncodingName: nil))
            task.didReceive(data)
            task.didFinish()
        }
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
        active.remove(ObjectIdentifier(task))
    }

    @concurrent private nonisolated static func read(_ file: URL) async -> Data? {
        try? Data(contentsOf: file)
    }
}
