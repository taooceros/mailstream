import AppKit

/// Shows one Google Chat space in the reader's persistent page, beside conversations: the space's
/// messages as a chat log with a composer at the bottom. The log is built off the main actor and
/// diffed against what the page shows, so new messages append, changed ones (a pending message
/// that failed) are replaced in place, and the page never reloads.
@MainActor
final class ChatReader {
    private let store: MailStore
    private unowned let reader: ReaderViewController

    /// The space on screen.
    private(set) var key: ChatSpaceKey?
    /// Bumped per space shown, so a log built for an earlier one is dropped.
    private var token = 0
    /// The log as the page has it, in order.
    private var shown: [ChatPage.Item] = []
    /// Whether this space's page was handed to the reader; later builds only send changes.
    private var staged = false
    /// Read state when the space was opened, so its unread messages stay marked while it is open.
    private var lastRead: Date?
    private var unreadAtOpen = 0
    private var firstUnread: String?
    private var building = false
    private var needsBuild = false

    private struct Draft {
        var text = ""
        var thread: String?
        var who: String?

        var isEmpty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && thread == nil }
    }

    private var draft = Draft()
    /// Unsent text of spaces not on screen; in memory only, like inline mail replies.
    private var drafts: [ChatSpaceKey: Draft] = [:]
    /// Whether the composer has focus, as last reported by the page, so Escape can decide at once.
    private var composerFocused = false

    init(store: MailStore, reader: ReaderViewController) {
        self.store = store
        self.reader = reader
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(messagesChanged(_:)), name: .chatMessagesChanged, object: store.chat)
        center.addObserver(self, selector: #selector(avatarResolved(_:)), name: .avatarResolved, object: store.avatars)
    }

    /// Puts `key` in the page with whatever is in memory; messages fill in as they arrive.
    func show(_ key: ChatSpaceKey) {
        hide()
        token += 1
        self.key = key
        draft = drafts.removeValue(forKey: key) ?? Draft()
        let space = store.chat.space(key)
        unreadAtOpen = space?.unread ?? 0
        lastRead = unreadAtOpen > 0 ? space?.lastRead : nil
        reader.beginChat()
        build()
    }

    /// Leaves the space, keeping an unsent draft.
    func hide() {
        guard let key else { return }
        drafts[key] = draft.isEmpty ? nil : draft
        self.key = nil
        token += 1
        shown = []
        staged = false
        composerFocused = false
        firstUnread = nil
    }

    /// Renders the space into a fresh page (its web process restarted).
    func reload() {
        guard key != nil else { return }
        staged = false
        build()
    }

    // MARK: - Building

    @objc private func messagesChanged(_ note: Notification) {
        guard let key, note.userInfo?["space"] as? ChatSpaceKey == key else { return }
        build()
    }

    /// A sender's picture arrived: the log is rebuilt with it, replacing only their messages.
    @objc private func avatarResolved(_ note: Notification) {
        guard let key, let email = note.userInfo?["email"] as? String else { return }
        let senders = store.chat.thread(key)?.messages.compactMap { $0.sender.email?.lowercased() } ?? []
        let me = store.chat.account(named: key.account)?.email?.lowercased()
        guard senders.contains(email) || me == email else { return }
        build()
    }

    /// Requests made while a pass runs coalesce into one more pass.
    private func build() {
        needsBuild = true
        guard !building else { return }
        building = true
        Task { await buildPasses() }
    }

    private func buildPasses() async {
        while needsBuild {
            needsBuild = false
            guard let key else { break }
            let token = token
            let output = await input(for: key).build()
            guard token == self.token, !needsBuild else { continue }
            publish(output, key: key)
        }
        building = false
    }

    private func input(for key: ChatSpaceKey) -> ChatPage.Input {
        let thread = store.chat.thread(key) ?? ChatThread()
        let meEmail = store.chat.account(named: key.account)?.email?.lowercased() ?? ""
        var avatars: [String: String] = [:]
        var names: [String: String] = [:]
        var seen = Set<String>()
        for user in (store.chat.space(key)?.members ?? []) + thread.messages.map(\.sender) {
            names[user.name] = user.label
            guard seen.insert(user.avatarKey).inserted else { continue }
            if let email = user.email {
                if let url = store.avatars.dataURL(for: email, automated: user.bot) { avatars[user.avatarKey] = url }
            } else if let url = user.avatarUrl {
                // No address to look the person up by: the page loads Google's picture itself.
                avatars[user.avatarKey] = url
            }
        }
        if !meEmail.isEmpty, avatars[meEmail] == nil, let url = store.avatars.dataURL(for: meEmail, automated: false) {
            avatars[meEmail] = url
        }
        return ChatPage.Input(messages: thread.messages, pending: thread.pending, lastRead: lastRead, unread: unreadAtOpen,
                              me: ChatPage.Sender(key: meEmail.isEmpty ? "me" : meEmail, label: meEmail.isEmpty ? "You" : meEmail),
                              avatars: avatars, names: names)
    }

    /// The first build stages the whole space; later ones send only what changed.
    private func publish(_ output: ChatPage.Input.Output, key: ChatSpaceKey) {
        let items = output.items
        if firstUnread == nil { firstUnread = output.firstUnread }
        let thread = store.chat.thread(key)
        var state: [String: Any] = [
            "earlier": (thread?.more ?? false) && !items.isEmpty,
            "loadingEarlier": store.chat.isLoadingEarlier(key),
            "status": ChatPage.status(loading: !(thread?.loaded ?? false), failure: thread?.failure, empty: items.isEmpty),
            "focus": firstUnread ?? "",
        ]
        guard staged, reader.acceptsChatScript else {
            let space = store.chat.space(key)
            let title = space?.title ?? "Chat"
            state["space"] = true
            state["accent"] = NSColor.controlAccentColor.cssHex
            state["html"] = ChatPage.page(title: title.isEmpty ? "Chat" : title, kind: space?.kind ?? .space,
                                          memberCount: space?.members.count ?? 0,
                                          accountLabel: store.label(account: key.account),
                                          accountColor: AccountStyle.chatHex(for: key.account, in: store))
            state["log"] = items.map(\.html).joined()
            state["draft"] = ["text": draft.text, "thread": draft.thread ?? "", "who": draft.who ?? ""]
            reader.stageChat(state)
            staged = true
            shown = items
            return
        }
        let old = shown
        shown = items
        let fresh = Set(items.map(\.id))
        let kept = old.map(\.id).filter(fresh.contains)
        if items.prefix(kept.count).map(\.id) == kept {
            // Same order: drop what left, replace what changed, append what is new.
            let before = Dictionary(old.map { ($0.id, $0.html) }, uniquingKeysWith: { a, _ in a })
            state["remove"] = old.map(\.id).filter { !fresh.contains($0) }
            state["replace"] = items.prefix(kept.count).filter { before[$0.id] != $0.html }.map { [$0.id, $0.html] }
            state["append"] = items.dropFirst(kept.count).map(\.html)
        } else {
            // Earlier messages loaded above, or the order changed: the log is replaced in place.
            state["log"] = items.map(\.html).joined()
        }
        reader.runChat("MS.chatUpdate(update)", ["update": state])
    }

    // MARK: - Messages from the page

    func handle(_ action: String, _ body: [String: Any]) {
        guard let key else { return }
        func nonEmpty(_ field: String) -> String? {
            (body[field] as? String).flatMap { $0.isEmpty ? nil : $0 }
        }
        switch action {
        case "chat-send":
            store.chat.send(body["text"] as? String ?? "", thread: nonEmpty("thread"), in: key)
            draft = Draft()
        case "chat-state":
            draft = Draft(text: body["text"] as? String ?? "", thread: nonEmpty("thread"), who: nonEmpty("who"))
            composerFocused = body["focused"] as? Bool ?? false
        case "chat-earlier":
            store.chat.loadEarlier(key)
        case "chat-retry":
            if let id = nonEmpty("id") { store.chat.retry(id, in: key) }
        case "chat-discard":
            if let id = nonEmpty("id") { store.chat.discard(id, in: key) }
        case "chat-open":
            guard let url = nonEmpty("url").flatMap(URL.init(string:)),
                  ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return }
            NSWorkspace.shared.open(url)
        case "avatar":
            guard let email = nonEmpty("email"), email.contains("@"), let rect = body["rect"] as? [Double], rect.count == 4
            else { return }
            let name = store.chat.thread(key)?.messages.last { $0.sender.email?.lowercased() == email }?.sender.displayName
            reader.showAvatar(of: Address(name: name, email: email), automated: false, at: rect)
        default:
            break
        }
    }

    /// Escape in the composer leaves it; false when Escape should go back to the list.
    func handleEscape() -> Bool {
        guard composerFocused else { return false }
        composerFocused = false
        reader.runChat("MS.chatBlur()", [:])
        return true
    }
}
