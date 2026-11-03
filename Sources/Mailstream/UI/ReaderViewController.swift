import AppKit
import UniformTypeIdentifiers
@preconcurrency import WebKit

@MainActor
protocol ReaderDelegate: AnyObject {
    func reader(_ reader: ReaderViewController, compose mode: Himalaya.ComposeMode, replyAll: Bool, message: MailMessage,
                text: String, plan: ReplyPlan?)
    func reader(_ reader: ReaderViewController, composeTo address: Address)
}

/// Renders one conversation into a single long-lived web page: native-feeling cards for each
/// message, HTML bodies in auto-sized iframes, quoted history folded, remote content blocked unless
/// the sender is known (`MailStore.loadsRemoteContent`). Switching conversations swaps the page's
/// content through script, not a navigation.
@MainActor
final class ReaderViewController: NSViewController, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    weak var delegate: ReaderDelegate?

    private let store: MailStore
    private var webView: WKWebView!
    private let placeholder = NSStackView()
    private let world = WKContentWorld.world(name: "mailstream")
    private var conversation: Conversation?
    private var messages: [MailMessage] = []
    /// Whether the current thread is in the page's DOM, so bodies and reply state can be sent.
    private var pageReady = false
    private var pending: [(Int, [String: Any])] = []
    private var loaded = Set<Int>()
    /// Whether this thread's bodies may load remote content; blocked ones carry their own strict CSP.
    private var allowRemote = false
    private var generation = 0
    private var onReady: (() -> Void)?
    /// The shell's navigation while it loads (at launch, and after its web process died).
    private var shellLoad: WKNavigation?
    /// `MS.render` state of the current thread, waiting for the shell to finish loading.
    private var staged: [String: Any]?

    /// An inline reply in progress; `plan` arrives from a himalaya dry run shortly after opening.
    private struct Reply {
        let conversation: String
        var source: MailMessage
        var all: Bool
        var text = ""
        var plan: ReplyPlan?
        var sending = false

        var isEmpty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }

    private var reply: Reply?
    /// Unsent inline replies of conversations not on screen, so leaving one keeps its text.
    private var drafts: [String: Reply] = [:]
    /// Whether the reply's text field has focus, as last reported by the page, so Escape can decide at once.
    private var replyFocused = false
    private var replyFocusPending = false
    private var planning: (ref: MessageRef, all: Bool)?

    init(store: MailStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        let configuration = WKWebViewConfiguration()
        Self.allowFullFrameRate(configuration.preferences)
        configuration.setURLSchemeHandler(PartSchemeHandler(cache: store.cache), forURLScheme: MailCache.scheme)
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.websiteDataStore = .nonPersistent()
        configuration.suppressesIncrementalRendering = false
        let controller = configuration.userContentController
        controller.addUserScript(WKUserScript(source: ReaderPage.script, injectionTime: .atDocumentEnd,
                                              forMainFrameOnly: true, in: world))
        controller.add(WeakScriptHandler(self), contentWorld: world, name: "ms")

        webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsMagnification = true
        webView.underPageBackgroundColor = .windowBackgroundColor
        webView.isHidden = true

        let icon = NSImageView(image: NSImage(systemSymbolName: "envelope.open", accessibilityDescription: nil) ?? NSImage())
        icon.symbolConfiguration = .init(pointSize: 44, weight: .thin)
        icon.contentTintColor = .tertiaryLabelColor
        let label = NSTextField(labelWithString: "No Conversation Selected")
        label.font = .systemFont(ofSize: 15, weight: .medium)
        label.textColor = .tertiaryLabelColor
        placeholder.orientation = .vertical
        placeholder.spacing = 10
        placeholder.addArrangedSubview(icon)
        placeholder.addArrangedSubview(label)

        let container = NSView()
        for view in [webView!, placeholder] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(view)
        }
        NSLayoutConstraint.activate([
            webView.topAnchor.constraint(equalTo: container.topAnchor),
            webView.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            webView.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: container.centerYAnchor),
        ])
        view = container
        loadShell()
        NotificationCenter.default.addObserver(self, selector: #selector(avatarResolved(_:)), name: .avatarResolved,
                                               object: store.avatars)
        NotificationCenter.default.addObserver(self, selector: #selector(tagsChanged), name: .storeTagsChanged, object: store)
    }

    @objc private func avatarResolved(_ note: Notification) {
        guard let email = note.userInfo?["email"] as? String,
              messages.contains(where: { $0.envelope.from.first?.normalizedEmail == email })
        else { return }
        pushAvatars(only: email)
    }

    /// Resolving from the list or the menu flips the header's "Waiting on you" chip in place.
    @objc private func tagsChanged() {
        guard let conversation else { return }
        call("const chip = document.getElementById('waiting'); if (chip) chip.hidden = !waiting;",
             ["waiting": store.tag(for: conversation)?.toRespond == true])
    }

    /// Sends known pictures into the page; `only` limits it to one sender.
    private func pushAvatars(only email: String? = nil) {
        let automated = conversation.flatMap { store.tag(for: $0) }.map { $0.category != .people } ?? false
        var seen = Set<String>()
        for message in messages {
            guard let sender = message.envelope.from.first?.normalizedEmail, email == nil || sender == email,
                  seen.insert(sender).inserted,
                  let url = store.avatars.dataURL(for: sender, automated: automated)
            else { continue }
            call("MS.setAvatar(email, url)", ["email": sender, "url": url])
        }
    }

    /// Enlarges a sender's picture; `rect` is the avatar's box in page (CSS) pixels.
    private func showAvatar(of message: MailMessage, at rect: [Double]) {
        guard let from = message.envelope.from.first else { return }
        let sender = store.withIdentityName(from)
        let automated = conversation.flatMap { store.tag(for: $0) }.map { $0.category != .people } ?? false
        guard let photo = store.avatars.image(for: sender.email, automated: automated) else { return }
        let scale = webView.pageZoom * webView.magnification
        let height = rect[3] * scale
        let top = rect[1] * scale
        let anchor = NSRect(x: rect[0] * scale, y: webView.isFlipped ? top : webView.bounds.height - top - height,
                            width: rect[2] * scale, height: height)
        AvatarPreview.show(sender, small: photo, automated: automated, avatars: store.avatars, relativeTo: anchor, of: webView)
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

    // MARK: - Rendering

    /// Whether showing a conversation now waits for the shell page to load.
    var awaitsShell: Bool { shellLoad != nil }

    /// Renders `conversation`; `onReady` runs once its thread is in the page (a later `show` replaces it).
    func show(_ conversation: Conversation?, onReady: (() -> Void)? = nil) {
        allowRemote = conversation.map(store.loadsRemoteContent) ?? false
        render(conversation)
        self.onReady = onReady
    }

    /// Re-renders only when the conversation gained or lost messages; flag changes keep the page.
    func update(_ conversation: Conversation) {
        guard conversation.id == self.conversation?.id else { return }
        let before = messages.map(\.ref)
        self.conversation = conversation
        if conversation.messages.map(\.ref) != before {
            render(conversation)
        } else {
            messages = conversation.messages
        }
    }

    private func render(_ conversation: Conversation?) {
        generation += 1
        if conversation?.id != self.conversation?.id {
            if let reply, reply.sending || !reply.isEmpty { drafts[reply.conversation] = reply }
            reply = conversation.flatMap { drafts.removeValue(forKey: $0.id) }
            replyFocused = false
        }
        self.conversation = conversation
        messages = conversation?.messages ?? []
        pageReady = false
        pending = []
        loaded = []
        staged = nil
        guard let conversation else {
            webView.isHidden = true
            placeholder.isHidden = false
            if !awaitsShell {
                webView.callAsyncJavaScript("MS.clear(generation)", arguments: ["generation": generation],
                                            in: nil, in: world, completionHandler: nil)
            }
            return
        }
        webView.isHidden = false
        placeholder.isHidden = true

        let lastIndex = messages.count - 1
        let automated = store.tag(for: conversation).map { $0.category != .people } ?? false
        // Back-and-forth between people reads as a chat; mail from services keeps the card stack.
        let chat = !automated && messages.count > 1
        let cards = messages.enumerated().map { index, message in
            let sender = store.withIdentityName(message.envelope.from.first ?? Address(name: nil, email: "unknown"))
            return ReaderPage.Card(index: index, message: message,
                                   expanded: chat || index == lastIndex || !message.envelope.isSeen || messages.count == 1,
                                   snippet: store.snippet(for: message.ref),
                                   sender: sender,
                                   avatar: store.avatars.dataURL(for: sender.email, automated: automated))
        }
        let labels = Array(Set(messages.compactMap { message -> String? in
            message.role == .other ? store.mailbox(message.ref.mailboxKey)?.displayName : nil
        })).sorted()
        let junk = conversation.messages.contains { $0.role == .junk }
        let untrusted = store.untrustedSenders(in: conversation)
        let trustLabel = junk || untrusted.isEmpty ? nil : untrusted.count == 1 ? untrusted[0].shortName : "These Senders"
        let html = ReaderPage.thread(conversation: conversation, cards: cards, own: store.ownAddresses,
                                     accountLabel: conversation.account,
                                     accountColor: AccountStyle.hex(for: conversation.account, in: store),
                                     mailboxLabels: labels, tag: store.tag(for: conversation), chat: chat,
                                     trustLabel: trustLabel)
        // A chat opens on the first unread message, else the latest.
        let focus = messages.firstIndex { !$0.envelope.isSeen } ?? lastIndex
        // Open bodies already in memory render with the thread itself.
        var bodies: [[Any]] = []
        for card in cards where card.expanded {
            guard let prepared = store.preparedContent(for: card.message.ref) else { continue }
            loaded.insert(card.index)
            bodies.append([card.index, payload(for: prepared, message: card.message)])
        }
        staged = ["generation": generation, "accent": NSColor.controlAccentColor.cssHex, "html": html,
                  "chat": chat, "focus": focus, "bodies": bodies]
        if shellLoad == nil { commit() }
        // Collapsed messages load in the background so their previews fill in and expanding is instant.
        for card in cards { load(card.index, priority: card.expanded ? .interactive : .background) }
    }

    /// Navigates to a fresh shell; a staged thread renders once it loads.
    private func loadShell() {
        shellLoad = webView.loadHTMLString(ReaderPage.shell, baseURL: URL(string: "\(MailCache.scheme)://reader/"))
    }

    /// Swaps the staged thread into the loaded shell; the page is ready once that script has run.
    private func commit() {
        guard let state = staged else { return }
        staged = nil
        let generation = generation
        webView.callAsyncJavaScript("MS.render(state)", arguments: ["state": state], in: nil, in: world) { [weak self] _ in
            guard let self, generation == self.generation else { return }
            self.pageReady = true
            self.onReady?()
            self.onReady = nil
            let queued = self.pending
            self.pending = []
            for (index, payload) in queued { self.deliver(index, payload) }
            // Pictures that resolved while the thread was on its way in.
            self.pushAvatars()
            if self.reply != nil { self.presentReply(focus: false) }
        }
    }

    private func load(_ index: Int, priority: RequestPriority = .interactive) {
        guard index < messages.count, loaded.insert(index).inserted else { return }
        let message = messages[index]
        let generation = generation
        Task {
            let payload: [String: Any]
            do {
                let prepared = try await store.content(for: message.ref, priority: priority)
                payload = self.payload(for: prepared, message: message)
            } catch {
                payload = ["kind": "error", "content": "Could not load this message: \(error.localizedDescription)"]
            }
            guard generation == self.generation else { return }
            deliver(index, payload)
        }
    }

    private func payload(for prepared: MailStore.LoadedContent, message: MailMessage) -> [String: Any] {
        let content = prepared.content
        var payload: [String: Any] = [
            "recipients": ReaderPage.recipients(message.envelope.to, cc: content.cc, own: store.ownAddresses),
            "snippet": content.snippet,
            "attachments": content.attachments.map { attachment -> [String: Any] in
                let url = store.attachmentURL(attachment, of: message.ref)
                var item: [String: Any] = [
                    "part": attachment.part,
                    "name": attachment.filename,
                    "size": ByteCountFormatter.string(fromByteCount: Int64(attachment.size), countStyle: .file),
                    "ext": url.pathExtension.isEmpty ? "file" : String(url.pathExtension.prefix(4)),
                ]
                if attachment.mime.hasPrefix("image/") {
                    item["thumb"] = store.cache.partURLBase(message.ref)
                        + (attachment.storedName.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? "")
                }
                return item
            },
        ]
        if prepared.isHTML {
            payload["kind"] = "html"
            payload["content"] = allowRemote ? prepared.body : ReaderPage.blockingRemoteContent(prepared.body)
            payload["remote"] = !allowRemote && prepared.hasRemote
        } else {
            payload["kind"] = "text"
            payload["content"] = prepared.body
        }
        return payload
    }

    private func deliver(_ index: Int, _ payload: [String: Any]) {
        guard pageReady else {
            pending.append((index, payload))
            return
        }
        webView.callAsyncJavaScript("MS.setBody(index, payload)", arguments: ["index": index, "payload": payload],
                                    in: nil, in: world, completionHandler: nil)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard navigation === shellLoad else { return }
        shellLoad = nil
        commit()
    }

    /// The shell dies with its web process: load a new one, with the thread on screen if any.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        loadShell()
        if conversation != nil { render(conversation) }
    }

    // MARK: - Messages from the page

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any], let action = body["action"] as? String,
              body["generation"] as? Int == generation else { return }
        let index = body["index"] as? Int ?? -1
        let target = index >= 0 && index < messages.count ? messages[index] : nil
        switch action {
        case "expand":
            load(index)
        case "reply", "replyAll":
            guard let target else { return }
            startReply(all: action == "replyAll", to: target)
        case "forward":
            guard let target else { return }
            compose(action, message: target)
        case "reply-latest", "replyAll-latest":
            startReply(all: action == "replyAll-latest")
        case "forward-latest":
            guard let latest = latestIncoming else { return }
            compose("forward", message: latest)
        case "reply-state":
            reply?.text = body["text"] as? String ?? ""
            replyFocused = body["focused"] as? Bool ?? false
        case "reply-send":
            sendReply(body["text"] as? String ?? "")
        case "reply-discard":
            discardReply()
        case "reply-popout":
            reply?.text = body["text"] as? String ?? ""
            popOutReply()
        case "reply-toggle-all":
            if let reply { startReply(all: !reply.all, to: reply.source) }
        case "attachment", "save-attachment":
            guard let target, let part = body["part"] as? Int,
                  let attachment = store.loadedContent(for: target.ref)?.attachments.first(where: { $0.part == part }) else { return }
            let url = store.attachmentURL(attachment, of: target.ref)
            if action == "attachment" { NSWorkspace.shared.open(url) } else { save(url, name: attachment.filename) }
        case "load-remote":
            allowRemote = true
            render(conversation)
        case "trust-remote":
            guard let conversation else { return }
            store.trustRemoteContent(from: store.untrustedSenders(in: conversation))
            allowRemote = true
            render(conversation)
        case "resolve":
            if let conversation { store.setWaiting(conversation, waiting: false) }
        case "avatar":
            guard let target, let rect = body["rect"] as? [Double], rect.count == 4 else { return }
            showAvatar(of: target, at: rect)
        case "compose-to":
            if let email = body["email"] as? String, !email.isEmpty {
                delegate?.reader(self, composeTo: Address(name: nil, email: email))
            }
        case "link":
            if let href = body["href"] as? String, let url = URL(string: href) { open(url) }
        default:
            break
        }
    }

    private var latestIncoming: MailMessage? {
        let own = store.ownAddresses
        return messages.last { message in
            !message.envelope.isDraft && !message.envelope.from.contains { own.contains($0.normalizedEmail) }
        } ?? messages.last
    }

    private func compose(_ action: String, message: MailMessage) {
        let mode: Himalaya.ComposeMode = action == "forward" ? .forward(message.ref) : .reply(message.ref)
        delegate?.reader(self, compose: mode, replyAll: action == "replyAll", message: message, text: "", plan: nil)
    }

    // MARK: - Inline reply

    /// Opens the inline reply to `message`, by default the latest one from someone else. Switching
    /// between reply and reply-all keeps the text.
    func startReply(all: Bool, to message: MailMessage? = nil) {
        guard let conversation, let source = message ?? latestIncoming, reply?.sending != true else { return }
        reply = Reply(conversation: conversation.id, source: source, all: all, text: reply?.text ?? "")
        presentReply(focus: true)
    }

    private func presentReply(focus: Bool) {
        guard let reply else { return }
        planReply()
        guard pageReady else {
            replyFocusPending = replyFocusPending || focus
            return
        }
        let options: [String: Any] = [
            "all": reply.all, "canAll": canReplyAll(reply.source), "text": reply.text,
            "focus": focus || replyFocusPending, "sending": reply.sending,
        ]
        replyFocusPending = false
        call("MS.openReply(options)", ["options": options])
        if let plan = reply.plan {
            pushReplyInfo(to: plan.to, cc: plan.cc)
        } else {
            let guess = guessRecipients(for: reply.source, all: reply.all)
            pushReplyInfo(to: guess.to, cc: guess.cc)
        }
        pushReplyQuote()
    }

    /// The reply in `conversation`, on screen or waiting in `drafts`.
    private func withReply(in conversation: String, _ change: (inout Reply) -> Void) {
        if reply?.conversation == conversation {
            change(&reply!)
        } else if drafts[conversation] != nil {
            change(&drafts[conversation]!)
        }
    }

    private func planReply() {
        guard let reply, reply.plan == nil else { return }
        let key = reply.conversation, source = reply.source, all = reply.all
        if let planning, planning.ref == source.ref, planning.all == all { return }
        planning = (source.ref, all)
        Task {
            defer { if planning?.ref == source.ref, planning?.all == all { planning = nil } }
            do {
                let plan = try await store.replyPlan(for: source, all: all)
                var current: Reply?
                withReply(in: key) { reply in
                    guard reply.source.ref == source.ref, reply.all == all else { return }
                    reply.plan = plan
                    current = reply
                }
                guard let current else { return }
                if current.sending {
                    transmit(current, plan: plan)
                } else if self.reply?.conversation == key {
                    pushReplyInfo(to: plan.to, cc: plan.cc)
                }
            } catch {
                var wasSending = false
                withReply(in: key) { reply in
                    wasSending = reply.sending
                    reply.sending = false
                }
                guard self.reply?.conversation == key else { return }
                call("MS.replyStatus(state, message)",
                     ["state": "error", "message": "\(wasSending ? "Not sent" : "Could not prepare the reply"): \(error.localizedDescription)"])
            }
        }
    }

    /// Recipients from local data, shown at once until himalaya's answer replaces them: `Reply-To`
    /// or `From`, or, for one of your own messages, its recipients.
    private func guessRecipients(for source: MailMessage, all: Bool) -> (to: [Address], cc: [Address]) {
        let own = store.ownAddresses
        let content = store.loadedContent(for: source.ref)
        let fromMe = source.envelope.from.contains { own.contains($0.normalizedEmail) }
        let replyTo = content?.replyTo ?? []
        let to = fromMe ? source.envelope.to : (replyTo.isEmpty ? source.envelope.from : replyTo)
        guard all else { return (to, []) }
        var seen = Set(to.map(\.normalizedEmail)).union(own)
        let cc = (source.envelope.to + source.envelope.from + (content?.cc ?? [])).filter { seen.insert($0.normalizedEmail).inserted }
        return (to, cc)
    }

    private func pushReplyInfo(to: [Address], cc: [Address]) {
        let own = store.ownAddresses
        func names(_ list: [Address]) -> String {
            list.map { own.contains($0.normalizedEmail) ? "me" : $0.displayName }.joined(separator: ", ")
        }
        var title = "To: " + to.map(\.rfc5322).joined(separator: ", ")
        if !cc.isEmpty { title += "\nCc: " + cc.map(\.rfc5322).joined(separator: ", ") }
        call("MS.setReplyInfo(info)", ["info": ["to": names(to), "cc": names(cc), "title": title]])
    }

    /// The source rendered like a message body, for the folded quote under the reply.
    private func pushReplyQuote() {
        guard let source = reply?.source else { return }
        let generation = generation
        Task {
            guard let content = try? await store.content(for: source.ref), generation == self.generation,
                  self.reply?.source.ref == source.ref else { return }
            var quote = payload(for: content, message: source)
            quote["attribution"] = MailStore.attribution(for: source)
            call("MS.setReplyQuote(quote)", ["quote": quote])
        }
    }

    private func canReplyAll(_ source: MailMessage) -> Bool {
        let cc = store.loadedContent(for: source.ref)?.cc ?? []
        let others = Set((source.envelope.to + source.envelope.from + cc).map(\.normalizedEmail)).subtracting(store.ownAddresses)
        return others.count > 1
    }

    /// Sends at once when the recipients are known, else as soon as they are.
    private func sendReply(_ text: String) {
        guard var reply, !reply.sending else { return }
        reply.text = text
        guard !reply.isEmpty else {
            self.reply = reply
            return
        }
        reply.sending = true
        self.reply = reply
        call("MS.replyStatus(state, message)", ["state": "sending", "message": "Sending…"])
        if let plan = reply.plan { transmit(reply, plan: plan) } else { planReply() }
    }

    private func transmit(_ reply: Reply, plan: ReplyPlan) {
        let key = reply.conversation
        Task {
            do {
                try await store.sendReply(reply.text, plan: plan, source: reply.source)
                drafts[key] = nil
                guard self.reply?.conversation == key else { return }
                self.reply = nil
                replyFocused = false
                call("MS.replySent()", [:])
            } catch {
                withReply(in: key) { $0.sending = false }
                guard self.reply?.conversation == key else { return }
                call("MS.replyStatus(state, message)",
                     ["state": "error", "message": "Not sent: \(error.localizedDescription)"])
            }
        }
    }

    private func discardReply() {
        guard let reply, !reply.sending else { return }
        drafts[reply.conversation] = nil
        self.reply = nil
        replyFocused = false
        call("MS.closeReply()", [:])
    }

    /// Moves the reply into a full composer window (attachments, editable recipients and quote).
    private func popOutReply() {
        guard let reply, !reply.sending else { return }
        discardReply()
        delegate?.reader(self, compose: .reply(reply.source.ref), replyAll: reply.all, message: reply.source,
                         text: reply.text, plan: reply.plan)
    }

    /// Escape while replying: leaves the text field, or closes an empty reply. False when Escape
    /// should go back as usual, the reply then waiting as a draft.
    func handleEscape() -> Bool {
        guard let reply, !reply.sending, !webView.isHidden else { return false }
        if reply.isEmpty {
            discardReply()
            return true
        }
        guard replyFocused else { return false }
        replyFocused = false
        call("MS.blurReply()", [:])
        return true
    }

    private func call(_ script: String, _ arguments: [String: Any]) {
        guard pageReady else { return }
        webView.callAsyncJavaScript(script, arguments: arguments, in: nil, in: world, completionHandler: nil)
    }

    private func open(_ url: URL) {
        if url.scheme?.lowercased() == "mailto" {
            let address = url.absoluteString.dropFirst("mailto:".count).split(separator: "?").first.map(String.init) ?? ""
            let decoded = address.removingPercentEncoding ?? address
            if !decoded.isEmpty { delegate?.reader(self, composeTo: Address(name: nil, email: decoded)) }
            return
        }
        guard let scheme = url.scheme?.lowercased(), ["http", "https", "tel", "facetime"].contains(scheme) else { return }
        NSWorkspace.shared.open(url)
    }

    private func save(_ url: URL, name: String) {
        guard let window = view.window else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.beginSheetModal(for: window) { response in
            guard response == .OK, let destination = panel.url else { return }
            try? FileManager.default.removeItem(at: destination)
            do {
                try FileManager.default.copyItem(at: url, to: destination)
            } catch {
                NSAlert(error: error).beginSheetModal(for: window)
            }
        }
    }

    // MARK: - Navigation policy

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        let url = navigationAction.request.url
        let scheme = url?.scheme?.lowercased()
        if navigationAction.navigationType == .linkActivated, let url {
            open(url)
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
        if let url = navigationAction.request.url { open(url) }
        return nil
    }
}

/// Breaks the retain cycle between `WKUserContentController` and its handler.
private final class WeakScriptHandler: NSObject, WKScriptMessageHandler {
    weak var target: WKScriptMessageHandler?

    init(_ target: WKScriptMessageHandler) { self.target = target }

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
        Task {
            let data = await Task.detached(priority: .userInitiated) { try? Data(contentsOf: file) }.value
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
}
