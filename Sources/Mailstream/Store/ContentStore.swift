import Foundation
import Observation

/// A message as the reader needs it, with its body markup worked out off the main actor.
nonisolated struct LoadedContent: Sendable {
    let content: MessageContent
    /// The HTML body, or the plain-text one as escaped HTML.
    let body: String
    let isHTML: Bool
    /// Whether the HTML body references remote images, styles or fonts.
    let hasRemote: Bool

    init(_ content: MessageContent) {
        self.content = content
        if let html = content.html {
            body = html
            isHTML = true
            hasRemote = HTMLText.hasRemoteContent(html)
        } else {
            body = HTMLText.plainToHTML(content.text ?? "")
            isHTML = false
            hasRemote = false
        }
    }
}

/// Message bodies and list previews: one shared load per message (disk cache first, else the
/// provider), the bodies the reader recently used in memory, and the preview of every message
/// loaded so far.
///
/// Observable through `snippets`: arrivals within one main-actor turn reach observers as one change.
@Observable
final class ContentStore {
    /// Display-ready previews (`MessageContent.snippet`) by message.
    private(set) var snippets: [MessageRef: String] = [:]

    @ObservationIgnored private let accounts: AccountStore
    @ObservationIgnored private let cache: MailCache
    /// Bodies in memory, oldest first in `order`; at most `capacity`.
    @ObservationIgnored private var bodies: [MessageRef: LoadedContent] = [:]
    @ObservationIgnored private var order: [MessageRef] = []
    @ObservationIgnored private var tasks: [MessageRef: Task<LoadedContent, any Error>] = [:]
    /// Bumped by `forget(accounts:)`, so loads in flight then don't bring a removed account's mail back.
    @ObservationIgnored private var generation = 0

    private static let capacity = 80
    /// Messages `preload` brings in per conversation: enough for the thread's visible end.
    private static let preloadCount = 10

    init(accounts: AccountStore, cache: MailCache) {
        self.accounts = accounts
        self.cache = cache
    }

    func snippet(for ref: MessageRef) -> String? { snippets[ref] }

    /// The message's content if it is in memory, so a thread can render it in its first frame.
    func loaded(_ ref: MessageRef) -> LoadedContent? { bodies[ref] }

    func load(_ ref: MessageRef, priority: RequestPriority = .interactive) async throws -> LoadedContent {
        if let loaded = bodies[ref] { return loaded }
        return try await keep(task(ref, priority: priority), for: ref)
    }

    /// Starts loading the conversation the user is about to open (pointer resting on it, keyboard
    /// highlight), newest messages first, so the reader finds their bodies in memory.
    func preload(_ conversation: Conversation) {
        for message in conversation.messages.suffix(Self.preloadCount).reversed() where bodies[message.ref] == nil {
            let task = task(message.ref, priority: .interactive)
            Task { _ = try? await keep(task, for: message.ref) }
        }
    }

    /// Loads the latest message of `conversations` (rows the list is showing) in the background,
    /// for their previews.
    func prefetchSnippets(for conversations: [Conversation]) {
        for conversation in conversations where snippets[conversation.latest.ref] == nil {
            _ = task(conversation.latest.ref, priority: .background)
        }
    }

    /// Drops everything of `removed` accounts: bodies, previews and loads in flight.
    func forget(accounts removed: Set<String>) {
        generation += 1
        for (ref, task) in tasks where removed.contains(ref.account) {
            task.cancel()
            tasks[ref] = nil
        }
        bodies = bodies.filter { !removed.contains($0.key.account) }
        order.removeAll { removed.contains($0.account) }
        snippets = snippets.filter { !removed.contains($0.key.account) }
    }

    /// One shared load per message, assembled and cached off the main actor; its preview is
    /// published when it lands.
    private func task(_ ref: MessageRef, priority: RequestPriority) -> Task<LoadedContent, any Error> {
        if let task = tasks[ref] { return task }
        let backend = accounts.backend(for: ref.account), cache = cache
        let task = Task(priority: priority == .interactive ? .userInitiated : .utility) {
            try await Self.load(ref, backend: backend, cache: cache, priority: priority)
        }
        tasks[ref] = task
        Task {
            let loaded = try? await task.value
            // Forgotten meanwhile.
            guard tasks[ref] == task else { return }
            tasks[ref] = nil
            if let snippet = loaded?.content.snippet, snippets[ref] != snippet { snippets[ref] = snippet }
        }
        return task
    }

    /// Awaits `task` and keeps its body in memory, unless an account was forgotten meanwhile.
    private func keep(_ task: Task<LoadedContent, any Error>, for ref: MessageRef) async throws -> LoadedContent {
        let generation = generation
        let loaded = try await task.value
        guard generation == self.generation else { return loaded }
        if bodies[ref] == nil { order.append(ref) }
        bodies[ref] = loaded
        while order.count > Self.capacity { bodies[order.removeFirst()] = nil }
        return loaded
    }

    @concurrent private nonisolated static func load(_ ref: MessageRef, backend: (any MailBackend)?, cache: MailCache,
                                                     priority: RequestPriority) async throws -> LoadedContent {
        if let cached = await cache.loadContent(ref) { return LoadedContent(cached) }
        guard let backend else { throw StoreError.signedOut }
        let content = try await backend.content(id: ref.id, mailbox: ref.mailbox, directory: cache.messageDirectory(ref),
                                                urlBase: cache.partURLBase(ref), priority: priority)
        cache.saveContent(content, for: ref)
        return LoadedContent(content)
    }
}
