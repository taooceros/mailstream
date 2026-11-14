import Foundation
import Observation

/// AI tags of the list's inbox conversations, from the user's `omp` CLI: one pass at a time,
/// debounced after list changes, backing off after a failure. Runs as the `aiTagging.*` settings
/// say, following edits of settings.json as they land.
///
/// Every Mailstream process (the app, front ends running the engine in-process) keeps its own
/// copy of the tags, saves only what it changed, and reads them all again when any process saves
/// (`ProcessSignal.tags`): so a tag the CLI sets through the app shows in GPUI and Flutter too.
///
/// Observable: the list follows `tags`, `isEnabled` and `status`.
@Observable
final class TagStore {
    /// Settings whose change deserves another try at once: a fixed omp or model, another backend.
    private static let retrySettings: [Setting] = [.aiTaggingBackend, .aiTaggingModel, .aiTaggingOmpPath,
                                                   .aiTaggingPassLimit, .aiTaggingBackoffSeconds]
    /// Why tag edits and retagging are refused while tagging is off.
    static let offMessage = "AI tagging is off (aiTagging.enabled)."

    /// Keyed by `key(_:)`, so a new message in a conversation re-tags it; read through `tag(for:)`.
    private(set) var tags: [String: ConversationTag] = [:]
    /// Short user-facing progress or failure text; nil when idle.
    private(set) var status: String?
    private var enabled: Bool

    @ObservationIgnored private let cache: MailCache
    @ObservationIgnored private let contents: ContentStore
    @ObservationIgnored private let problems: StoreProblems
    @ObservationIgnored private let tagger: OmpTagger
    /// What the last `schedule` offered.
    @ObservationIgnored private var candidates: [Conversation] = []
    @ObservationIgnored private var own: Set<String> = []
    /// Keys the model was asked about but skipped; automatic passes leave them alone.
    @ObservationIgnored private var attempted = Set<String>()
    @ObservationIgnored private lazy var passes = CoalescedTask("AI tagging") { [weak self] in await self?.pass() }
    @ObservationIgnored private lazy var reloads = CoalescedTask("AI tags reload") { [weak self] in await self?.reload() }
    /// Counts this process's changes, so a reload that raced one drops its older copy.
    @ObservationIgnored private var edits = 0
    @ObservationIgnored private var debounce: Task<Void, Never>?
    @ObservationIgnored private var backoffUntil: Date?
    /// The `aiTagging.*` settings and other processes' tag changes.
    @ObservationIgnored private var watchers: [Task<Void, Never>] = []

    init(cache: MailCache, tools: ToolEnvironment, contents: ContentStore, problems: StoreProblems) {
        self.cache = cache
        self.contents = contents
        self.problems = problems
        tagger = OmpTagger(tools: tools, workingDirectory: cache.root)
        enabled = Settings.shared.bool(.aiTaggingEnabled)
    }

    isolated deinit {
        watchers.forEach { $0.cancel() }
        debounce?.cancel()
    }

    /// The `aiTagging.enabled` setting; setting it writes settings.json.
    var isEnabled: Bool {
        get { enabled }
        set {
            do {
                try Settings.shared.set(.aiTaggingEnabled, .bool(newValue))
                apply(enabled: newValue)
            } catch {
                problems.report(error, from: .mail, alert: true)
            }
        }
    }

    /// Follows the `aiTagging.*` settings, however settings.json is edited, and the tags any
    /// Mailstream process saves.
    func start() {
        watchers = [
            Task.immediate(name: "AI tags saved") { [weak self] in
                for await _ in ProcessSignal.tags.posts() { self?.reloads.request() }
            },
            Task.immediate(name: "AI tagging switch") { [weak self] in
                for await enabled in Observations({ Settings.shared.bool(.aiTaggingEnabled) }) {
                    self?.apply(enabled: enabled)
                }
            },
            Task.immediate(name: "AI tagging settings") { [weak self] in
                var last = Self.retryValues
                for await values in Observations({ Self.retryValues }) where values != last {
                    last = values
                    self?.retry()
                }
            },
        ]
    }

    private static var retryValues: [JSON] { retrySettings.map { Settings.shared[$0] } }

    private func apply(enabled: Bool) {
        guard enabled != self.enabled else { return }
        self.enabled = enabled
        attempted = []
        backoffUntil = nil
        debounce?.cancel()
        if enabled {
            passes.request()
        } else {
            passes.cancel()
            status = nil
        }
    }

    private func retry() {
        attempted = []
        backoffUntil = nil
        if enabled { passes.request() }
    }

    /// Adds the tags saved on disk, keeping any already made.
    func restore() async {
        guard let saved = await cache.loadTags() else { return }
        tags.merge(saved) { current, _ in current }
    }

    /// Reads the tags again from mail.db, where every process saves its changes as it makes them.
    private func reload() async {
        await cache.flush()
        let edits = edits
        let saved = await cache.loadTags() ?? [:]
        // A change made meanwhile is still on its way to disk; its save signals another reload.
        guard edits == self.edits, saved != tags else { return }
        tags = saved
    }

    private func save(_ changed: [String: ConversationTag]) {
        edits += 1
        cache.saveTags(changed)
    }

    /// The conversation's tag, with `toRespond` false once the ask has lapsed.
    func tag(for conversation: Conversation) -> ConversationTag? {
        guard enabled, var tag = tags[Self.key(conversation)] else { return nil }
        tag.toRespond = tag.isWaiting()
        return tag
    }

    /// Whether `conversation` can be resolved or put back on "Waiting on you": any conversation
    /// the tagger has judged but a newsletter.
    func canMarkWaiting(_ conversation: Conversation) -> Bool {
        tag(for: conversation).map { $0.category != .newsletter } ?? false
    }

    /// Resolves the conversation (`waiting: false`) or puts it back on "Waiting on you". The flag
    /// belongs to the latest message, so a new incoming one is judged afresh.
    func setWaiting(_ conversation: Conversation, waiting: Bool) {
        let key = Self.key(conversation)
        guard enabled, var tag = tags[key], tag.category != .newsletter, tag.isWaiting() != waiting else { return }
        tag.toRespond = waiting
        tag.waitingUntil = nil
        tags[key] = tag
        save([key: tag])
    }

    /// Corrects the conversation's tag; nil leaves a part as it is. A conversation the tagger has
    /// not judged needs a category, and newsletters never wait on the user.
    func edit(_ conversation: Conversation, category: MailCategory?, waiting: Bool?,
              labels: [String]?) throws -> ConversationTag {
        guard enabled else { throw StoreError(message: Self.offMessage) }
        let key = Self.key(conversation)
        guard var tag = tags[key] ?? category.map({ ConversationTag(category: $0, toRespond: false, labels: []) }) else {
            throw StoreError(message: "\(conversation.id) has no AI tag yet; give a category.")
        }
        if let category { tag.category = category }
        if waiting == true, tag.category == .newsletter {
            throw StoreError(message: "Newsletters cannot wait on you.")
        }
        if let waiting {
            tag.toRespond = waiting
            tag.waitingUntil = nil
        }
        if tag.category == .newsletter { tag.toRespond = false }
        if let labels {
            tag.labels = Array(labels.map {
                String($0.trimmingCharacters(in: .whitespaces).prefix(OmpTagger.maxLabelLength)).trimmingCharacters(in: .whitespaces)
            }.filter { !$0.isEmpty }.prefix(2))
        }
        tags[key] = tag
        attempted.insert(key)
        save([key: tag])
        tag.toRespond = tag.isWaiting()
        return tag
    }

    /// Forgets every tag and tags the current list again, ignoring any failure backoff.
    func retagAll() {
        tags = [:]
        attempted = []
        backoffUntil = nil
        edits += 1
        cache.clearTags()
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

    /// Tags the newest untagged candidates in batches, one omp process at a time. Apple
    /// Intelligence (`aiTagging.backend` apple) runs only in the Rust engine's front ends; this
    /// engine then leaves tagging to them rather than tagging with omp instead.
    private func pass() async {
        guard enabled else { return }
        if let until = backoffUntil, until > Date() { return }
        let settings = Settings.shared
        guard settings.string(.aiTaggingBackend) == "omp" else {
            status = "Apple Intelligence tags in Mailstream Flutter and Mailstream GPUI; Mailstream.app tags only with omp."
            return
        }
        let options = OmpTagger.Options(model: settings.string(.aiTaggingModel),
                                        executable: settings.string(.aiTaggingOmpPath))
        let items = untaggedItems(limit: settings.int(.aiTaggingPassLimit))
        var remaining = items.count
        for batch in items.chunked(into: settings.int(.aiTaggingBatchSize)) {
            status = "Tagging \(remaining) conversation\(remaining == 1 ? "" : "s")…"
            do {
                let result = try await tagger.tag(batch, options: options)
                try Task.checkCancellation()
                tags.merge(result) { _, new in new }
                attempted.formUnion(batch.map(\.id))
                save(result)
                remaining -= batch.count
            } catch {
                if Task.isCancelled || error is CancellationError {
                    status = nil
                } else {
                    status = "AI tagging failed: \(error.localizedDescription)"
                    backoffUntil = Date().addingTimeInterval(TimeInterval(settings.int(.aiTaggingBackoffSeconds)))
                }
                return
            }
        }
        if status != nil { status = nil }
    }

    private func untaggedItems(limit: Int) -> [OmpTagger.Item] {
        var keys = Set<String>()
        var items: [OmpTagger.Item] = []
        for conversation in candidates where items.count < limit {
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
            date: incoming.envelope.date,
            messageCount: messages.count,
            userReplied: firstIncoming.map { fromUser[($0 + 1)...].contains(true) } ?? false,
            latestFromUser: fromUser.last ?? false)
    }
}
