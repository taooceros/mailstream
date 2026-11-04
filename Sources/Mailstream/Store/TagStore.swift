import Foundation
import Observation

/// AI tags of the list's inbox conversations, from the user's `omp` CLI: one pass at a time,
/// debounced after list changes, backing off after a failure.
///
/// Observable: the list follows `tags`, `isEnabled` and `status`.
@Observable
final class TagStore {
    private static let passLimit = 150
    private static let batchSize = 20
    private static let backoff: TimeInterval = 300
    private static let enabledKey = "AITaggingEnabled"

    /// Keyed by `key(_:)`, so a new message in a conversation re-tags it; read through `tag(for:)`.
    private(set) var tags: [String: ConversationTag] = [:]
    /// Short user-facing progress or failure text; nil when idle.
    private(set) var status: String?
    private var enabled: Bool

    @ObservationIgnored private let cache: MailCache
    @ObservationIgnored private let contents: ContentStore
    @ObservationIgnored private let tagger: OmpTagger
    /// What the last `schedule` offered.
    @ObservationIgnored private var candidates: [Conversation] = []
    @ObservationIgnored private var own: Set<String> = []
    /// Keys the model was asked about but skipped; automatic passes leave them alone.
    @ObservationIgnored private var attempted = Set<String>()
    @ObservationIgnored private lazy var passes = CoalescedTask("AI tagging") { [weak self] in await self?.pass() }
    @ObservationIgnored private var debounce: Task<Void, Never>?
    @ObservationIgnored private var backoffUntil: Date?

    init(cache: MailCache, tools: ToolEnvironment, contents: ContentStore) {
        self.cache = cache
        self.contents = contents
        tagger = OmpTagger(tools: tools, workingDirectory: cache.root)
        enabled = UserDefaults.standard.object(forKey: Self.enabledKey) as? Bool ?? true
    }

    var isEnabled: Bool {
        get { enabled }
        set {
            enabled = newValue
            UserDefaults.standard.set(newValue, forKey: Self.enabledKey)
            attempted = []
            backoffUntil = nil
            debounce?.cancel()
            if newValue {
                passes.request()
            } else {
                passes.cancel()
                status = nil
            }
        }
    }

    /// Adds the tags saved on disk, keeping any already made.
    func restore() async {
        guard let saved = await cache.loadTags() else { return }
        tags.merge(saved) { current, _ in current }
    }

    func tag(for conversation: Conversation) -> ConversationTag? {
        enabled ? tags[Self.key(conversation)] : nil
    }

    /// Whether `conversation` can be resolved or put back on "Waiting on you": only people
    /// conversations the tagger has judged carry the flag.
    func canMarkWaiting(_ conversation: Conversation) -> Bool {
        tag(for: conversation)?.category == .people
    }

    /// Resolves the conversation (`waiting: false`) or puts it back on "Waiting on you". The flag
    /// belongs to the latest message, so a new incoming one is judged afresh.
    func setWaiting(_ conversation: Conversation, waiting: Bool) {
        let key = Self.key(conversation)
        guard enabled, var tag = tags[key], tag.category == .people, tag.toRespond != waiting else { return }
        tag.toRespond = waiting
        tags[key] = tag
        cache.saveTags(tags)
    }

    /// Forgets every tag and tags the current list again, ignoring any failure backoff.
    func retagAll() {
        tags = [:]
        attempted = []
        backoffUntil = nil
        cache.saveTags(tags)
        debounce?.cancel()
        passes.restart()
    }

    /// Tags `conversations` (newest first, each with a message in a listed inbox) a second after
    /// the last call; `own` are the user's lowercased addresses.
    func schedule(_ conversations: [Conversation], own: Set<String>) {
        candidates = conversations
        self.own = own
        guard enabled else { return }
        debounce?.cancel()
        debounce = Task {
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled else { return }
            passes.request()
        }
    }

    /// The conversation's latest message, so a new reply re-tags it.
    private static func key(_ conversation: Conversation) -> String {
        let latest = conversation.latest
        return latest.envelope.messageID ?? "\(latest.ref.account)/\(latest.ref.mailbox)/\(latest.ref.id)"
    }

    /// Tags the newest untagged candidates in batches, one omp process at a time.
    private func pass() async {
        guard enabled else { return }
        if let until = backoffUntil, until > Date() { return }
        let items = untaggedItems()
        var remaining = items.count
        for batch in items.chunked(into: Self.batchSize) {
            status = "Tagging \(remaining) conversation\(remaining == 1 ? "" : "s")…"
            do {
                let result = try await tagger.tag(batch)
                try Task.checkCancellation()
                tags.merge(result) { _, new in new }
                attempted.formUnion(batch.map(\.id))
                cache.saveTags(tags)
                remaining -= batch.count
            } catch {
                if Task.isCancelled || error is CancellationError {
                    status = nil
                } else {
                    status = "AI tagging failed: \(error.localizedDescription)"
                    backoffUntil = Date().addingTimeInterval(Self.backoff)
                }
                return
            }
        }
        if status != nil { status = nil }
    }

    private func untaggedItems() -> [OmpTagger.Item] {
        var keys = Set<String>()
        var items: [OmpTagger.Item] = []
        for conversation in candidates where items.count < Self.passLimit {
            let key = Self.key(conversation)
            guard tags[key] == nil, !attempted.contains(key), keys.insert(key).inserted else { continue }
            items.append(item(conversation, key: key))
        }
        return items
    }

    private func item(_ conversation: Conversation, key: String) -> OmpTagger.Item {
        let messages = conversation.messages
        let fromUser = messages.map { $0.isFromUser(own: own) }
        let firstIncoming = fromUser.firstIndex(of: false)
        let incoming = conversation.latestIncoming(own: own) ?? conversation.latest
        let sender = incoming.envelope.from.first
        return OmpTagger.Item(
            id: key,
            senderName: sender?.displayName ?? "",
            senderEmail: sender?.email ?? "",
            userInTo: incoming.envelope.to.contains { own.contains($0.normalizedEmail) },
            subject: conversation.subject,
            snippet: contents.snippet(for: incoming.ref) ?? "",
            messageCount: messages.count,
            userReplied: firstIncoming.map { fromUser[($0 + 1)...].contains(true) } ?? false,
            latestFromUser: fromUser.last ?? false)
    }
}
