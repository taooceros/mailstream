import AppKit
import Observation

/// Shows one Google Chat space in the reader's web page (`WebShell`), as the mail reader's peer:
/// the space's messages as a chat log with a composer at the bottom. The log is built off the main
/// actor and diffed against what the page shows, so new messages append, changed ones (a pending
/// message that failed) are replaced in place, and the page never reloads.
@MainActor
final class ChatReader: WebShellClient {
    private let store: MailStore
    private let shell: WebShell

    /// The space on screen.
    private(set) var key: ChatSpaceKey?
    /// Bumped per space shown, so a log built for an earlier one is dropped.
    private var token = 0
    /// The log as the page has it, in order.
    private var shown: [ChatPage.Item] = []
    /// Whether this space's page was handed to the shell; later builds only send changes.
    private var staged = false
    /// Read state when the space was opened, so its unread messages stay marked while it is open.
    private var lastRead: Date?
    private var unreadAtOpen = 0
    private var firstUnread: String?
    /// Follows what the page shows of the space on screen.
    private var watch: Task<Void, Never>?
    /// What the page was last given, so passes that change nothing publish nothing.
    private var published: Snapshot?
    /// Requests made while a pass runs coalesce into one more pass.
    private lazy var building = CoalescedTask("Chat log") { [weak self] in await self?.buildPass() }

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

    init(store: MailStore, shell: WebShell) {
        self.store = store
        self.shell = shell
    }

    isolated deinit { watch?.cancel() }

    /// Puts `key` in the page with whatever is in memory; messages fill in as they arrive.
    func show(_ key: ChatSpaceKey) {
        hide()
        token += 1
        self.key = key
        draft = drafts.removeValue(forKey: key) ?? Draft()
        let space = store.chat.space(key)
        unreadAtOpen = space?.unread ?? 0
        lastRead = unreadAtOpen > 0 ? space?.lastRead : nil
        shell.begin(self)
        watch = Task.immediate(name: "Chat reader") { [weak self, store] in
            // Reads only: the pass builds the page's input, which may start picture lookups.
            let changes = Observations {
                _ = (store.avatars.revision, store.chat.thread(key), store.chat.space(key), store.chat.isLoadingEarlier(key))
            }
            for await _ in changes { self?.build() }
        }
    }

    /// Leaves the space, keeping an unsent draft.
    func hide() {
        guard let key else { return }
        drafts[key] = draft.isEmpty ? nil : draft
        watch?.cancel()
        watch = nil
        published = nil
        self.key = nil
        token += 1
        shown = []
        staged = false
        composerFocused = false
        firstUnread = nil
    }

    /// Renders the space into a fresh page (its web process restarted).
    func shellDidReload(_ shell: WebShell) {
        guard key != nil else { return }
        shell.begin(self)
        staged = false
        published = nil
        build()
    }

    // MARK: - Building

    /// Everything the page shows of a space. Messages of other spaces, and pictures of people not
    /// in this one, leave it equal, so their passes publish nothing.
    private struct Snapshot: Equatable {
        let input: ChatPage.Input
        let more: Bool
        let loaded: Bool
        let failure: String?
        let loadingEarlier: Bool
    }

    private func snapshot(of key: ChatSpaceKey) -> Snapshot {
        let thread = store.chat.thread(key)
        return Snapshot(input: input(for: key), more: thread?.more ?? false, loaded: thread?.loaded ?? false,
                        failure: thread?.failure, loadingEarlier: store.chat.isLoadingEarlier(key))
    }

    private func build() { building.request() }

    private func buildPass() async {
        guard let key else { return }
        let snapshot = snapshot(of: key)
        guard snapshot != published else { return }
        let token = token
        let output = await snapshot.input.build()
        // Shown elsewhere meanwhile, or changed again: the next pass publishes.
        guard token == self.token, !building.isPending else { return }
        published = snapshot
        publish(output, key: key)
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
            if let photo = user.avatarUrl.flatMap(URL.init(string:)) { store.avatars.publish(photo, for: user.avatarKey) }
            // Until AvatarStore has the picture, the page loads Google's link itself.
            if let url = store.avatars.dataURL(for: user.avatarKey, automated: user.bot) ?? user.avatarUrl {
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
        guard staged else {
            let space = store.chat.space(key)
            let title = space?.title ?? "Chat"
            state["html"] = ChatPage.page(title: title.isEmpty ? "Chat" : title, kind: space?.kind ?? .space,
                                          memberCount: space?.members.count ?? 0,
                                          accountLabel: store.label(account: key.account),
                                          accountColor: AccountStyle.chatHex(for: key.account, in: store))
            state["log"] = items.map(\.html).joined()
            state["draft"] = ["text": draft.text, "thread": draft.thread ?? "", "who": draft.who ?? ""]
            shell.render("MS.chat.render", state)
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
        shell.call("MS.chat.update(update)", ["update": state])
    }

    // MARK: - Messages from the page

    func shell(_ shell: WebShell, received action: String, _ body: [String: Any]) {
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
            guard let email = nonEmpty("email"), email.contains("@") else { return }
            let name = store.chat.thread(key)?.messages.last { $0.sender.email?.lowercased() == email }?.sender.displayName
            shell.showAvatar(of: Address(name: name, email: email), automated: false, from: body)
        default:
            break
        }
    }

    /// Escape in the composer leaves it; false when Escape should go back to the list.
    func handleEscape() -> Bool {
        guard composerFocused else { return false }
        composerFocused = false
        shell.call("MS.chat.blur()")
        return true
    }
}
