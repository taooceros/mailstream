import AppKit
import Observation

@MainActor
protocol ReaderDelegate: AnyObject {
    func reader(_ reader: ReaderViewController, compose mode: ComposeMode, replyAll: Bool, message: MailMessage,
                text: String, plan: ReplyPlan?)
    func reader(_ reader: ReaderViewController, composeTo address: Address)
}

/// What the reader shows.
enum ReaderTarget {
    case conversation(Conversation)
    case chat(ChatSpaceKey)

    var conversation: Conversation? {
        if case .conversation(let conversation) = self { return conversation }
        return nil
    }
}

/// Renders one conversation into the reader's long-lived web page (`WebShell`): native-feeling
/// cards for each message, HTML bodies in auto-sized iframes, quoted history folded, remote content
/// blocked unless the sender is known (`MailStore.loadsRemoteContent`). Switching conversations
/// swaps the page's content through script, not a navigation. Chat spaces render into the same
/// page through `ChatReader`, its peer.
@MainActor
final class ReaderViewController: NSViewController, WebShellClient {
    weak var delegate: (any ReaderDelegate)?

    private let store: MailStore
    private var shell: WebShell!
    /// Renders chat spaces into the shell.
    private var chatReader: ChatReader!
    private let placeholder = NSStackView()
    private var conversation: Conversation?
    private var messages: [MailMessage] = []
    /// Messages whose body was requested for the page on screen.
    private var loaded = Set<Int>()
    /// Whether this thread's bodies may load remote content; blocked ones carry their own strict CSP.
    private var allowRemote = false

    /// An inline reply in progress; `plan` arrives once the source's content is loaded.
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
    private(set) var isReplyFocused = false
    private var planning: (ref: MessageRef, all: Bool)?
    /// Senders whose picture the page has.
    private var avatarsSent = Set<String>()
    /// Pictures arriving and the conversation's "Waiting on you" flag.
    private var storeWatches: [Task<Void, Never>] = []

    init(store: MailStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    isolated deinit { storeWatches.forEach { $0.cancel() } }

    override func loadView() {
        shell = WebShell(cache: store.cache, avatars: store.avatars) { [weak self] url in self?.open(url) }
        chatReader = ChatReader(store: store, shell: shell)
        let webView = shell.webView
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
        for view in [webView, placeholder] as [NSView] {
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
        let store = store
        storeWatches = [
            Task.immediate(name: "Reader pictures") { [weak self] in
                for await _ in Observations({ store.avatars.revision }).dropFirst() { self?.pushAvatars() }
            },
            // Resolving from the list or the menu flips the header's "Waiting on you" chip in place.
            Task.immediate(name: "Reader waiting flag") { [weak self] in
                let waiting = Observations { [weak self] in self?.conversation.map { store.tagging.tag(for: $0)?.toRespond == true } }
                for await waiting in waiting.dropFirst() {
                    guard let waiting = waiting ?? nil else { continue }
                    self?.shell.call("const chip = document.getElementById('waiting'); if (chip) chip.hidden = !waiting;",
                                     ["waiting": waiting])
                }
            },
        ]
    }

    /// Mail from services, whose pictures are logos.
    private var isAutomated: Bool {
        conversation.flatMap { store.tagging.tag(for: $0) }.map { $0.category != .people } ?? false
    }

    /// Sends the page the senders' pictures it does not have yet.
    private func pushAvatars() {
        let automated = isAutomated
        for message in messages {
            guard let sender = message.envelope.from.first?.normalizedEmail, !avatarsSent.contains(sender),
                  let url = store.avatars.dataURL(for: sender, automated: automated)
            else { continue }
            avatarsSent.insert(sender)
            shell.call("MS.setAvatar(email, url)", ["email": sender, "url": url])
        }
    }

    // MARK: - Showing

    /// Whether showing something now waits for the shell page to load.
    var awaitsShell: Bool { shell.awaitsShell }

    /// Renders `target` (nothing: the placeholder); `onReady` runs once it is in the page (a later
    /// `show` replaces it).
    func show(_ target: ReaderTarget?, onReady: (() -> Void)? = nil) {
        switch target {
        case .conversation(let conversation):
            chatReader.hide()
            allowRemote = store.loadsRemoteContent(for: conversation)
            render(conversation)
        case .chat(let key):
            leaveConversation()
            chatReader.show(key)
        case nil:
            chatReader.hide()
            leaveConversation()
            shell.clear()
        }
        shell.webView.isHidden = target == nil
        placeholder.isHidden = target != nil
        shell.onReady = onReady
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

    /// The conversation leaves the page, its unsent reply kept as a draft.
    private func leaveConversation() {
        if let reply, reply.sending || !reply.isEmpty { drafts[reply.conversation] = reply }
        reply = nil
        isReplyFocused = false
        conversation = nil
        messages = []
        loaded = []
        avatarsSent = []
    }

    private func render(_ conversation: Conversation) {
        if conversation.id != self.conversation?.id {
            leaveConversation()
            reply = drafts.removeValue(forKey: conversation.id)
        }
        shell.begin(self)
        self.conversation = conversation
        messages = conversation.messages
        loaded = []

        let lastIndex = messages.count - 1
        let tag = store.tagging.tag(for: conversation)
        let automated = isAutomated
        // Back-and-forth between people reads as a chat; mail from services keeps the card stack.
        let chat = !automated && messages.count > 1
        let cards = messages.enumerated().map { index, message in
            let sender = store.withIdentityName(message.envelope.from.first ?? Address(name: nil, email: "unknown"))
            return ReaderPage.Card(index: index, message: message,
                                   expanded: chat || index == lastIndex || !message.envelope.isSeen || messages.count == 1,
                                   snippet: store.contents.snippet(for: message.ref),
                                   sender: sender,
                                   avatar: store.avatars.dataURL(for: sender.email, automated: automated))
        }
        avatarsSent = Set(cards.compactMap { $0.avatar == nil ? nil : $0.sender.normalizedEmail })
        let labels = Array(Set(messages.compactMap { message -> String? in
            message.role == .other ? store.mailbox(message.ref.mailboxKey)?.displayName : nil
        })).sorted()
        let junk = conversation.messages.contains { $0.role == .junk }
        let untrusted = store.untrustedSenders(in: conversation)
        let trustLabel = junk || untrusted.isEmpty ? nil : untrusted.count == 1 ? untrusted[0].shortName : "These Senders"
        let html = ReaderPage.thread(conversation: conversation, cards: cards, own: store.ownAddresses,
                                     accountLabel: store.label(account: conversation.account),
                                     accountColor: AccountStyle.hex(for: conversation.account, in: store),
                                     mailboxLabels: labels, tag: tag, chat: chat, trustLabel: trustLabel)
        // A chat opens on the first unread message, else the latest.
        let focus = messages.firstIndex { !$0.envelope.isSeen } ?? lastIndex
        // Open bodies already in memory render with the thread itself.
        var bodies: [[Any]] = []
        for card in cards where card.expanded {
            guard let content = store.contents.loaded(card.message.ref) else { continue }
            loaded.insert(card.index)
            bodies.append([card.index, payload(for: content, message: card.message)])
        }
        shell.render("MS.render", ["accent": NSColor.controlAccentColor.cssHex, "html": html, "chat": chat,
                                   "focus": focus, "bodies": bodies])
        if reply != nil { presentReply(focus: false) }
        // Collapsed messages load in the background so their previews fill in and expanding is instant.
        for card in cards { load(card.index, priority: card.expanded ? .interactive : .background) }
    }

    private func load(_ index: Int, priority: RequestPriority = .interactive) {
        guard index < messages.count, loaded.insert(index).inserted else { return }
        let message = messages[index]
        let generation = shell.generation
        Task {
            let payload: [String: Any]
            do {
                payload = self.payload(for: try await store.contents.load(message.ref, priority: priority), message: message)
            } catch {
                payload = ["kind": "error", "content": "Could not load this message: \(error.localizedDescription)"]
            }
            guard generation == shell.generation else { return }
            shell.call("MS.setBody(index, payload)", ["index": index, "payload": payload])
        }
    }

    private func payload(for loaded: LoadedContent, message: MailMessage) -> [String: Any] {
        let content = loaded.content
        var payload: [String: Any] = [
            "recipients": ReaderPage.recipients(message.envelope.to, cc: content.cc, own: store.ownAddresses),
            "snippet": content.snippet,
            "attachments": content.attachments.map { attachment -> [String: Any] in
                let url = store.cache.attachmentURL(attachment, of: message.ref)
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
        if loaded.isHTML {
            payload["kind"] = "html"
            payload["content"] = allowRemote ? loaded.body : ReaderPage.blockingRemoteContent(loaded.body)
            payload["remote"] = !allowRemote && loaded.hasRemote
        } else {
            payload["kind"] = "text"
            payload["content"] = loaded.body
        }
        return payload
    }

    /// The shell came back from a crashed web process: the thread renders again.
    func shellDidReload(_ shell: WebShell) {
        if let conversation { render(conversation) }
    }

    // MARK: - Messages from the page

    func shell(_ shell: WebShell, received action: String, _ body: [String: Any]) {
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
            isReplyFocused = body["focused"] as? Bool ?? false
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
                  let attachment = store.contents.loaded(target.ref)?.content.attachments.first(where: { $0.part == part })
            else { return }
            let url = store.cache.attachmentURL(attachment, of: target.ref)
            if action == "attachment" { NSWorkspace.shared.open(url) } else { save(url, name: attachment.filename) }
        case "load-remote":
            allowRemote = true
            if let conversation { render(conversation) }
        case "trust-remote":
            guard let conversation else { return }
            store.trustRemoteContent(from: store.untrustedSenders(in: conversation))
            allowRemote = true
            render(conversation)
        case "resolve":
            if let conversation { store.tagging.setWaiting(conversation, waiting: false) }
        case "avatar":
            guard let target, let from = target.envelope.from.first, let rect = body["rect"] as? [Double], rect.count == 4
            else { return }
            shell.showAvatar(of: store.withIdentityName(from), automated: isAutomated, at: rect)
        case "compose-to":
            if let email = body["email"] as? String, !email.isEmpty {
                delegate?.reader(self, composeTo: Address(name: nil, email: email))
            }
        default:
            break
        }
    }

    private var latestIncoming: MailMessage? {
        conversation.map { $0.latestIncoming(own: store.ownAddresses) ?? $0.latest }
    }

    private func compose(_ action: String, message: MailMessage) {
        let mode: ComposeMode = action == "forward" ? .forward(message.ref) : .reply(message.ref)
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
        let options: [String: Any] = [
            "all": reply.all, "canAll": canReplyAll(reply.source), "text": reply.text, "focus": focus,
            "sending": reply.sending,
        ]
        shell.call("MS.openReply(options)", ["options": options])
        if let plan = reply.plan {
            pushReplyInfo(to: plan.to, cc: plan.cc)
        } else {
            // From what is loaded already, shown at once until the plan replaces it.
            let guess = MailStore.replyRecipients(for: reply.source, content: store.contents.loaded(reply.source.ref)?.content,
                                                  own: store.ownAddresses, all: reply.all)
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
                shell.call("MS.replyStatus(state, message)",
                           ["state": "error", "message": "\(wasSending ? "Not sent" : "Could not prepare the reply"): \(error.localizedDescription)"])
            }
        }
    }

    private func pushReplyInfo(to: [Address], cc: [Address]) {
        let own = store.ownAddresses
        func names(_ list: [Address]) -> String { list.map { ReaderPage.name($0, own: own) }.joined(separator: ", ") }
        var title = "To: " + to.map(\.rfc5322).joined(separator: ", ")
        if !cc.isEmpty { title += "\nCc: " + cc.map(\.rfc5322).joined(separator: ", ") }
        shell.call("MS.setReplyInfo(info)", ["info": ["to": names(to), "cc": names(cc), "title": title]])
    }

    /// The source rendered like a message body, for the folded quote under the reply.
    private func pushReplyQuote() {
        guard let source = reply?.source else { return }
        let generation = shell.generation
        Task {
            guard let content = try? await store.contents.load(source.ref), generation == shell.generation,
                  self.reply?.source.ref == source.ref else { return }
            var quote = payload(for: content, message: source)
            quote["attribution"] = MailStore.attribution(for: source)
            shell.call("MS.setReplyQuote(quote)", ["quote": quote])
        }
    }

    private func canReplyAll(_ source: MailMessage) -> Bool {
        let cc = store.contents.loaded(source.ref)?.content.cc ?? []
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
        shell.call("MS.replyStatus(state, message)", ["state": "sending", "message": "Sending…"])
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
                isReplyFocused = false
                shell.call("MS.replySent()")
            } catch {
                withReply(in: key) { $0.sending = false }
                guard self.reply?.conversation == key else { return }
                shell.call("MS.replyStatus(state, message)", ["state": "error", "message": "Not sent: \(error.localizedDescription)"])
            }
        }
    }

    private func discardReply() {
        guard let reply, !reply.sending else { return }
        drafts[reply.conversation] = nil
        self.reply = nil
        isReplyFocused = false
        shell.call("MS.closeReply()")
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
        if chatReader.key != nil { return chatReader.handleEscape() }
        guard let reply, !reply.sending else { return false }
        if reply.isEmpty {
            discardReply()
            return true
        }
        guard isReplyFocused else { return false }
        isReplyFocused = false
        shell.call("MS.blurReply()")
        return true
    }

    // MARK: - Links and files

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
}
