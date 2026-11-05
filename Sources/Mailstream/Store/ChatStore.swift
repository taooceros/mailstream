import Foundation
import Observation

/// One space of one chat account.
nonisolated struct ChatSpaceKey: Hashable, Codable, Sendable {
    let account: String
    let space: String

    var id: String { account + "\u{1}" + space }
}

/// A message sent from here that Google has not confirmed yet, or that failed to send.
nonisolated struct ChatPending: Hashable, Sendable {
    let id: String
    let text: String
    let thread: String?
    let created: Date
    /// Why sending failed; nil while sending.
    var failure: String?
}

/// The loaded part of one space's history.
nonisolated struct ChatThread: Equatable, Sendable {
    /// Ascending by creation.
    var messages: [ChatMessage] = []
    /// Older messages exist beyond the first one here.
    var more = false
    var lastRead: Date?
    /// Answered by Google this session (not only read from disk).
    var loaded = false
    /// Why the last fetch failed, while there is nothing to show.
    var failure: String?
    var pending: [ChatPending] = []
}

/// Google Chat state: chat accounts (Google accounts granted Chat access), each account's most
/// recently active spaces, and the messages of spaces the user opened. Main-actor state only;
/// requests, JSON decoding, row formatting and the disk cache run off the main actor, like
/// `MailStore`'s.
///
/// Spaces are polled every `chat.pollSeconds` (`Chat` refetches only spaces whose activity changed). Reading
/// and sending are optimistic: opening a space clears its unread count at once, and a sent message
/// shows as pending until Google returns it. A failed send is marked failed, never retried.
///
/// Observable: the list and sidebar follow `accounts`, `rows`, `isLoading` and the spaces' unread
/// counts; the reader follows `thread(_:)` and `isLoadingEarlier(_:)` of its space.
@Observable
final class ChatStore {
    nonisolated static let spacesPage = 60
    nonisolated static let messagesPage = 100
    /// Spaces whose messages stay on disk, most recently opened first.
    static let recentLimit = 20
    /// Messages kept on disk per recent space.
    static let cachedMessages = 150

    /// The scope that makes a Google account a chat account.
    static let chatScope = OAuthScope.chatMessages

    @ObservationIgnored private let chat: Chat
    @ObservationIgnored private let cache: MailCache
    @ObservationIgnored private let accountStore: AccountStore
    /// Takes the pictures Google Chat publishes for the people the rows show.
    @ObservationIgnored private let avatars: AvatarStore

    private(set) var accounts: [ChatAccount] = []
    /// Every account's spaces as list rows, newest first.
    private(set) var rows: [ChatRow] = []
    private(set) var isLoading = false
    /// The space in the reader: polling keeps its messages current and read.
    private(set) var openSpace: ChatSpaceKey?

    private struct Listing {
        var list: ChatSpaceList
        /// `--limit` of the next poll; scrolling to the end of the list raises it.
        var limit: Int
    }

    private var listings: [String: Listing] = [:]
    private var threads: [ChatSpaceKey: ChatThread] = [:]
    @ObservationIgnored private var recent: [ChatSpaceKey] = []
    /// When the user last read a space here; polls answered before Google saw it keep it read.
    @ObservationIgnored private var readMarks: [ChatSpaceKey: Date] = [:]
    /// Bumped by each mark-read, so only the latest one may roll its optimistic mark back.
    @ObservationIgnored private var readRevisions: [ChatSpaceKey: Int] = [:]
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    /// The `chat.pollSeconds` timer.
    @ObservationIgnored private var polling = PeriodicTask()
    private var loadMoreTask: Task<Void, Never>?
    /// Newest-page fetches per space; one asked for during a fetch runs after it while the space is open.
    @ObservationIgnored private var fetches: [ChatSpaceKey: CoalescedTask] = [:]
    private var earlier: [ChatSpaceKey: Task<Void, Never>] = [:]
    /// Rows are formatted off the main actor; requests made meanwhile coalesce into one more pass.
    @ObservationIgnored private lazy var rebuild = CoalescedTask("Chat list rebuild") { [weak self] in
        await self?.rebuildPass()
    }
    /// Follows the chat accounts one pass at a time.
    @ObservationIgnored private lazy var accountSync = CoalescedTask("Chat accounts") { [weak self] in
        await self?.syncAccounts()
    }
    /// Account changes and the poll.
    @ObservationIgnored private var watchers: [Task<Void, Never>] = []

    init(cache: MailCache, accounts: AccountStore, avatars: AvatarStore) {
        self.cache = cache
        accountStore = accounts
        self.avatars = avatars
        chat = Chat { id in
            guard accounts.record(id).map(ChatStore.isChat) == true else { return nil }
            return accounts.api(for: id)
        }
    }

    isolated deinit {
        watchers.forEach { $0.cancel() }
        pollTask?.cancel()
        polling.cancel()
        loadMoreTask?.cancel()
        earlier.values.forEach { $0.cancel() }
        fetches.values.forEach { $0.cancel() }
    }

    private var problems: StoreProblems { accountStore.problems }

    private static func isChat(_ record: AccountRecord) -> Bool {
        record.provider == .google && record.scopes.contains(chatScope)
    }

    // MARK: - Startup and polling

    func start() {
        Task(name: "Chat startup") {
            await restoreFromCache()
            await accountSync.settle()
            poll(quiet: false)
        }
        let accountStore = accountStore
        watchers = [
            Task.immediate(name: "Chat account changes") { [weak self] in
                for await _ in Observations({ accountStore.records }).dropFirst() {
                    await self?.accountSync.settle()
                    self?.poll(quiet: false)
                }
            },
            Task.immediate(name: "Chat poll interval") { [weak self] in
                for await seconds in Observations({ Settings.shared.int(.chatPollSeconds) }) {
                    self?.schedulePolling(every: .seconds(seconds))
                }
            },
        ]
    }

    /// Polls every `chat.pollSeconds`; a changed setting starts the wait afresh.
    private func schedulePolling(every interval: Duration) {
        polling.schedule(every: interval, name: "Chat polling", on: self) { $0.poll(quiet: true) }
    }

    /// Paints the last known accounts and spaces before Google answers.
    private func restoreFromCache() async {
        let cache = cache
        let savedRecent = await cache.loadChatRecent() ?? []
        if recent.isEmpty { recent = savedRecent }
        guard let saved = await cache.loadChatAccounts(), !saved.isEmpty, accounts.isEmpty else { return }
        accounts = saved
        await loadCachedSpaces(saved.map(\.name))
        rebuild.request()
    }

    private func loadCachedSpaces(_ names: [String]) async {
        let cache = cache
        await forEachConcurrently(names) { await cache.loadChatSpaces($0) } consume: { name, result in
            guard let list = try? result.get(), listings[name] == nil, account(named: name) != nil else { return }
            listings[name] = Listing(list: list, limit: max(Self.spacesPage, list.spaces.count))
        }
    }

    /// Takes the chat accounts from `AccountStore`; removed accounts' spaces, messages and fetches
    /// disappear. Runs as `accountSync` passes, never two at once.
    private func syncAccounts() async {
        let fresh = accountStore.records.filter(Self.isChat).map { ChatAccount(name: $0.id, email: $0.id) }
        guard fresh != accounts else { return }
        let names = Set(fresh.map(\.name))
        let chat = chat
        for gone in accounts.map(\.name) where !names.contains(gone) {
            Task { await chat.forget(account: gone) }
        }
        accounts = fresh
        prune()
        await loadCachedSpaces(fresh.map(\.name).filter { listings[$0] == nil })
        cache.saveChatAccounts(accounts)
        rebuild.request()
    }

    /// Drops whatever belongs to accounts no longer in `accounts`, including their cached messages.
    private func prune() {
        let names = Set(accounts.map(\.name))
        listings.removeAll { !names.contains($0) }
        threads.removeAll { !names.contains($0.account) }
        fetches.removeAll { !names.contains($0.account) } cancelling: { $0.cancel() }
        earlier.removeAll { !names.contains($0.account) } cancelling: { $0.cancel() }
        readMarks.removeAll { !names.contains($0.account) }
        readRevisions.removeAll { !names.contains($0.account) }
        let gone = recent.filter { !names.contains($0.account) }
        if !gone.isEmpty {
            gone.forEach(cache.removeChatMessages)
            recent.removeAll { !names.contains($0.account) }
            cache.saveChatRecent(recent)
        }
        if let openSpace, !names.contains(openSpace.account) { self.openSpace = nil }
    }

    /// Fetches every account's spaces now (toolbar refresh, selecting Chat).
    func refresh() { poll(quiet: false) }

    /// `quiet` polls (the poll loop's) show no progress.
    private func poll(quiet: Bool) {
        let jobs = accounts.map { (name: $0.name, limit: listings[$0.name]?.limit ?? Self.spacesPage) }
        guard !jobs.isEmpty else { return }
        if !quiet { setLoading(true) }
        guard pollTask == nil else { return }
        let chat = chat
        pollTask = Task(name: "Chat poll") {
            var failed = false
            await forEachConcurrently(jobs) { job in
                try await chat.spaces(account: job.name, limit: job.limit)
            } consume: { job, result in
                switch result {
                case .success(let list): apply(list, account: job.name, limit: job.limit)
                case .failure(let error):
                    failed = true
                    report(error, context: job.name)
                }
            }
            pollTask = nil
            if !failed { problems.clear(.chat) }
            setLoading(loadMoreTask != nil)
        }
    }

    /// Takes in one account's spaces; returns early when nothing changed, so an idle poll costs nothing.
    private func apply(_ fresh: ChatSpaceList, account: String, limit: Int) {
        // A poll that outlived its account's sign-out.
        guard accounts.contains(where: { $0.name == account }) else { return }
        // A poll started before the limit was raised would shrink the list back.
        if let current = listings[account]?.limit, current != limit { return }
        let previous = Dictionary((listings[account]?.list.spaces ?? []).map { ($0.name, $0) }, uniquingKeysWith: { a, _ in a })
        var list = fresh
        for index in list.spaces.indices {
            let space = list.spaces[index]
            let key = ChatSpaceKey(account: account, space: space.name)
            if let mark = readMarks[key], (space.latest?.created ?? space.activity ?? .distantPast) <= mark {
                list.spaces[index].unread = 0
            }
            if key == openSpace, previous[space.name]?.activity != space.activity { fetchLatest(key) }
        }
        let before = listings[account]?.list
        guard before?.spaces != list.spaces || before?.more != list.more || before?.me != list.me else { return }
        listings[account] = Listing(list: list, limit: limit)
        cache.saveChatSpaces(list, for: account)
        rebuild.request()
    }

    private func rebuildPass() async {
        let input = ChatListInput(sources: accounts.compactMap { account in
            listings[account.name].map { ChatListInput.Source(account: account.name, spaces: $0.list.spaces) }
        })
        let output = await input.build()
        // Spaces changed meanwhile: the next pass shows them.
        guard !rebuild.isPending else { return }
        for row in output { if let photo = row.avatarPhoto { avatars.publish(photo, for: row.avatarKey) } }
        rows = output
    }

    // MARK: - Lookup

    func account(named name: String) -> ChatAccount? { accounts.first { $0.name == name } }

    func space(_ key: ChatSpaceKey) -> ChatSpace? {
        listings[key.account]?.list.spaces.first { $0.name == key.space }
    }

    /// The user's `users/<id>` in `account`.
    func me(account: String) -> String? { listings[account]?.list.me }

    /// Rows of one account, or of all of them.
    func rows(account: String?) -> [ChatRow] {
        guard let account else { return rows }
        return rows.filter { $0.key.account == account }
    }

    /// Spaces with unread messages, the sidebar's badge.
    func unreadSpaces(account: String?) -> Int {
        listings.reduce(0) { sum, entry in
            guard account == nil || entry.key == account else { return sum }
            return sum + entry.value.list.spaces.filter { $0.unread > 0 }.count
        }
    }

    func canLoadMore(account: String?) -> Bool {
        loadMoreTask == nil && listings.contains { (account == nil || $0.key == account) && $0.value.list.more }
    }

    /// Raises the spaces limit of the accounts in view by a page.
    func loadMore(account: String?) {
        guard canLoadMore(account: account) else { return }
        let names = accounts.map(\.name).filter { (account == nil || $0 == account) && listings[$0]?.list.more == true }
        let chat = chat
        setLoading(true)
        loadMoreTask = Task(name: "Chat load more") {
            for name in names {
                let limit = (listings[name]?.limit ?? Self.spacesPage) + Self.spacesPage
                do {
                    let list = try await chat.spaces(account: name, limit: limit)
                    listings[name]?.limit = limit
                    apply(list, account: name, limit: limit)
                } catch {
                    report(error, context: name)
                }
            }
            loadMoreTask = nil
            setLoading(pollTask != nil)
        }
    }

    private func setLoading(_ loading: Bool) {
        if isLoading != loading { isLoading = loading }
    }

    func thread(_ key: ChatSpaceKey) -> ChatThread? { threads[key] }

    func isLoadingEarlier(_ key: ChatSpaceKey) -> Bool { earlier[key] != nil }

    // MARK: - Opening

    /// Reads a recent space's messages from disk into memory (pointer resting on its row), so
    /// opening it paints them in the first frame.
    func preload(_ key: ChatSpaceKey) {
        guard threads[key] == nil else { return }
        threads[key] = ChatThread()
        let cache = cache
        Task {
            guard let saved = await cache.loadChatMessages(key), var thread = threads[key],
                  !thread.loaded, thread.messages.isEmpty else { return }
            thread.messages = saved.messages
            thread.more = saved.more
            thread.lastRead = saved.lastRead
            threads[key] = thread
        }
    }

    /// The reader shows `key`: its cached messages at once, the newest from Google when they
    /// arrive, and it is marked read.
    func open(_ key: ChatSpaceKey) {
        openSpace = key
        remember(key)
        preload(key)
        fetchLatest(key)
        markRead(key)
    }

    func close() { openSpace = nil }

    private func fetchLatest(_ key: ChatSpaceKey) {
        if let fetch = fetches[key] { return fetch.request() }
        let fetch = CoalescedTask("Chat messages") { [weak self] in await self?.fetchPass(key) }
        fetches[key] = fetch
        fetch.request()
    }

    /// Asked for while open: a space closed before an extra fetch began needs none.
    private func fetchPass(_ key: ChatSpaceKey) async {
        guard key == openSpace else { return }
        do {
            let page = try await chat.messages(account: key.account, space: key.space, limit: Self.messagesPage)
            merge(latest: page, into: key)
        } catch {
            guard account(named: key.account) != nil else { return }
            if threads[key]?.messages.isEmpty ?? true { threads[key, default: ChatThread()].failure = error.localizedDescription }
            report(error, context: key.account)
        }
    }

    /// The newest page replaces what it covers; older loaded messages stay below it.
    private func merge(latest page: ChatMessagePage, into key: ChatSpaceKey) {
        guard accounts.contains(where: { $0.name == key.account }) else { return }
        var thread = threads[key] ?? ChatThread()
        // Messages newer than the page's newest were confirmed (sent from here) after this fetch
        // left; the page can't have them, so they stay.
        let newest = page.messages.last?.created ?? .distantPast
        let names = Set(page.messages.map(\.name))
        let newer = thread.messages.filter { !names.contains($0.name) && $0.created > newest }
        if page.more, let oldest = page.messages.first?.created {
            let older = thread.messages.filter { !names.contains($0.name) && $0.created < oldest }
            thread.more = older.isEmpty ? true : thread.more
            thread.messages = older + page.messages + newer
        } else {
            thread.messages = page.messages + newer
            thread.more = page.more
        }
        thread.lastRead = page.lastRead ?? thread.lastRead
        thread.loaded = true
        thread.failure = nil
        threads[key] = thread
        persist(key)
        // While open, a message from someone else newer than the read mark is read on arrival.
        if key == openSpace, let newest = thread.messages.last(where: { !$0.mine }),
           newest.created > (readMarks[key] ?? .distantPast) {
            markRead(key)
        }
    }

    /// Fetches the page before the oldest loaded message.
    func loadEarlier(_ key: ChatSpaceKey) {
        guard earlier[key] == nil, let thread = threads[key], thread.more,
              let oldest = thread.messages.first?.created else { return }
        let chat = chat
        earlier[key] = Task {
            do {
                let page = try await chat.messages(account: key.account, space: key.space, limit: Self.messagesPage,
                                                     before: oldest)
                earlier[key] = nil
                guard var thread = threads[key] else { return }
                let known = Set(thread.messages.map(\.name))
                thread.messages = (page.messages.filter { !known.contains($0.name) } + thread.messages)
                    .sorted { $0.created < $1.created }
                thread.more = page.more
                threads[key] = thread
            } catch {
                earlier[key] = nil
                report(error, context: key.account)
            }
        }
    }

    /// A page of `key`'s messages straight from Google, ascending: the newest `limit`, or the
    /// newest before `before`. For the control socket; the reader's thread is left alone.
    func page(_ key: ChatSpaceKey, limit: Int, before: Date?) async throws -> ChatMessagePage {
        guard account(named: key.account) != nil else { throw ChatError(message: "\(key.account) has no Google Chat") }
        return try await chat.messages(account: key.account, space: key.space, limit: limit, before: before)
    }

    // MARK: - Reading and sending

    /// Clears the unread count at once; restores it if Google refuses.
    func markRead(_ key: ChatSpaceKey) {
        readMarks[key] = max(readMarks[key] ?? .distantPast, Date())
        let revision = (readRevisions[key] ?? 0) + 1
        readRevisions[key] = revision
        let previous = space(key)
        if let previous, previous.unread != 0 { updateSpace(key) { $0.unread = 0 } }
        let chat = chat
        Task {
            do {
                let lastRead = try await chat.markRead(account: key.account, space: key.space)
                guard let lastRead, account(named: key.account) != nil else { return }
                readMarks[key] = max(readMarks[key] ?? lastRead, lastRead)
                threads[key]?.lastRead = lastRead
                updateSpace(key) { $0.lastRead = lastRead }
            } catch {
                // A later mark-read owns the state now; this failure must not undo it.
                guard readRevisions[key] == revision else { return }
                readMarks[key] = nil
                if let previous, previous.unread > 0 { updateSpace(key) { $0.unread = previous.unread } }
                report(error, context: key.account)
            }
        }
    }

    /// Shows `text` as a pending message at once and sends it, in `thread` when given.
    func send(_ text: String, thread: String?, in key: ChatSpaceKey) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let pending = ChatPending(id: "pending-" + UUID().uuidString, text: trimmed, thread: thread, created: Date())
        threads[key, default: ChatThread()].pending.append(pending)
        Task {
            do {
                try await deliver(pending.text, thread: pending.thread, in: key, replacing: pending.id)
            } catch {
                guard let index = threads[key]?.pending.firstIndex(where: { $0.id == pending.id }) else { return }
                threads[key]?.pending[index].failure = error.localizedDescription
            }
        }
    }

    /// Sends `text` and waits for Google's answer, which takes the place of the pending message
    /// `replacing` in the loaded thread and becomes the row's latest message. Not idempotent: never
    /// retried. The control socket sends through this too.
    @discardableResult
    func deliver(_ text: String, thread: String?, in key: ChatSpaceKey, replacing pending: String? = nil) async throws
        -> ChatMessage {
        let message = try await chat.send(account: key.account, space: key.space, text: text, thread: thread)
        // A send that outlived its account's sign-out.
        guard account(named: key.account) != nil else { return message }
        if var loaded = threads[key] {
            loaded.pending.removeAll { $0.id == pending }
            if !loaded.messages.contains(where: { $0.name == message.name }) {
                loaded.messages.append(message)
                loaded.messages.sort { $0.created < $1.created }
            }
            threads[key] = loaded
            persist(key)
        }
        updateSpace(key) { space in
            if (space.latest?.created ?? .distantPast) <= message.created { space.latest = message }
            space.lastActive = max(space.lastActive ?? message.created, message.created)
        }
        return message
    }

    /// Sends a failed message again, on the user's request only.
    func retry(_ id: String, in key: ChatSpaceKey) {
        guard let pending = threads[key]?.pending.first(where: { $0.id == id && $0.failure != nil }) else { return }
        threads[key]?.pending.removeAll { $0.id == id }
        send(pending.text, thread: pending.thread, in: key)
    }

    func discard(_ id: String, in key: ChatSpaceKey) {
        guard threads[key]?.pending.contains(where: { $0.id == id && $0.failure != nil }) == true else { return }
        threads[key]?.pending.removeAll { $0.id == id }
    }

    // MARK: - Starting

    /// Whether `account`'s sign-in lets it create direct messages and group chats; accounts signed
    /// in before Mailstream asked for that can only reopen the direct messages they have.
    func canStartChats(account: String) -> Bool {
        accountStore.record(account)?.scopes.contains(OAuthScope.chatSpacesCreate) == true
    }

    /// People `account` chats with, most recently active first: the other members of its direct
    /// messages and group chats that Google names by address.
    func people(account: String) -> [(user: ChatUser, activity: Date?)] {
        var seen = Set<String>()
        var out: [(user: ChatUser, activity: Date?)] = []
        for space in listings[account]?.list.spaces ?? [] where space.kind == .dm || space.kind == .group {
            for member in space.members where !member.bot {
                guard let email = member.email?.lowercased(), seen.insert(email).inserted else { continue }
                out.append((member, space.activity))
            }
        }
        return out
    }

    /// The conversation with exactly `emails` in `account`: the direct message with one person or
    /// the group chat with several, found in the list or at Google, else a new one Google creates.
    /// It joins the list; the returned key opens it.
    func start(account: String, with emails: [String]) async throws -> ChatSpaceKey {
        guard self.account(named: account) != nil else { throw ChatError(message: "\(account) has no Google Chat") }
        let own = account.lowercased()
        var seen = Set<String>()
        let people = emails.map { $0.lowercased() }.filter { $0 != own && seen.insert($0).inserted }
        guard !people.isEmpty else { throw ChatError(message: "Add someone to chat with") }
        guard people.count <= 49 else { throw ChatError(message: "A group chat holds at most 49 other people") }
        if let known = listings[account]?.list.spaces.first(where: { Self.space($0, isWith: people) }) {
            return ChatSpaceKey(account: account, space: known.name)
        }
        if people.count == 1, let found = try await chat.directMessage(account: account, with: people[0]) {
            return insert(found, account: account)
        }
        guard canStartChats(account: account) else {
            throw ChatError(message: "To start new chats, sign in to \(account) again in Settings > Accounts and allow "
                + "Mailstream to create Google Chat conversations.")
        }
        let space = try await chat.setUp(account: account, with: people, request: UUID().uuidString)
        return insert(space, account: account)
    }

    /// A direct message or unnamed group chat whose other members are exactly `people`.
    private static func space(_ space: ChatSpace, isWith people: [String]) -> Bool {
        guard space.kind == (people.count == 1 ? .dm : .group), space.members.count == people.count else { return false }
        return Set(space.members.compactMap { $0.email?.lowercased() }) == Set(people)
    }

    /// Puts a space the user just found or made into its account's list, newest first. One
    /// without activity yet (made just now) counts as active now, so it shows at the top.
    private func insert(_ found: ChatSpace, account: String) -> ChatSpaceKey {
        guard self.account(named: account) != nil else { return ChatSpaceKey(account: account, space: found.name) }
        var space = found
        if space.activity == nil { space.lastActive = Date() }
        var listing = listings[account] ?? Listing(list: ChatSpaceList(me: nil, more: false, spaces: []), limit: Self.spacesPage)
        listing.list.spaces.removeAll { $0.name == space.name }
        listing.list.spaces.append(space)
        listing.list.spaces.sortNewestFirst(by: \.activity)
        listings[account] = listing
        cache.saveChatSpaces(listing.list, for: account)
        rebuild.request()
        return ChatSpaceKey(account: account, space: space.name)
    }

    /// Changes one space in its listing, keeping the list newest first, and republishes the rows.
    private func updateSpace(_ key: ChatSpaceKey, _ change: (inout ChatSpace) -> Void) {
        guard var listing = listings[key.account],
              let index = listing.list.spaces.firstIndex(where: { $0.name == key.space }) else { return }
        let before = listing.list.spaces[index]
        change(&listing.list.spaces[index])
        guard listing.list.spaces[index] != before else { return }
        if listing.list.spaces[index].activity != before.activity {
            listing.list.spaces.sortNewestFirst(by: \.activity)
        }
        listings[key.account] = listing
        cache.saveChatSpaces(listing.list, for: key.account)
        rebuild.request()
    }

    // MARK: - Disk cache of opened spaces

    private func remember(_ key: ChatSpaceKey) {
        recent.removeAll { $0 == key }
        recent.insert(key, at: 0)
        while recent.count > Self.recentLimit { cache.removeChatMessages(recent.removeLast()) }
        cache.saveChatRecent(recent)
    }

    private func persist(_ key: ChatSpaceKey) {
        guard recent.contains(key), let thread = threads[key] else { return }
        let kept = Array(thread.messages.suffix(Self.cachedMessages))
        cache.saveChatMessages(ChatMessagePage(messages: kept, more: thread.more || kept.count < thread.messages.count,
                                               lastRead: thread.lastRead), for: key)
    }

    // MARK: - Reporting

    private func report(_ error: any Error, context: String? = nil) {
        problems.report(error, from: .chat, context: context)
    }
}

/// What one space's list row shows, formatted off the main thread.
nonisolated struct ChatRow: Hashable, Sendable {
    let key: ChatSpaceKey
    let title: String
    /// "Brent: see you" / "You: thanks".
    let preview: String
    let date: String
    /// Date group header, as in mail lists.
    let section: String
    let activity: Date?
    let isUnread: Bool
    let hasAttachment: Bool
    let avatarInitials: String
    let avatarName: String
    /// AvatarStore key: the person's lowercased address, else their Chat user id; empty when the
    /// row shows no one.
    let avatarKey: String
    /// The picture Google Chat publishes for that person.
    let avatarPhoto: URL?
    /// Seed of the initials' color.
    let avatarColorKey: String

    var id: String { key.id }

    init(_ space: ChatSpace, account: String, calendar: Calendar, now: Date) {
        key = ChatSpaceKey(account: account, space: space.name)
        title = space.title.isEmpty ? space.members.map(\.label).joined(separator: ", ") : space.title
        activity = space.activity
        (date, section) = ConversationRow.dateLabels(activity, calendar: calendar, now: now)
        isUnread = space.unread > 0
        let latest = space.latest
        hasAttachment = !(latest?.attachments.isEmpty ?? true)
        preview = latest.map(Self.preview) ?? ""
        let incoming = latest.flatMap { $0.mine ? nil : $0.sender }
        let person: ChatUser? = switch space.kind {
        case .dm, .bot: space.members.first ?? incoming
        case .group: incoming ?? space.members.first
        case .space: incoming
        }
        if let person {
            avatarKey = person.avatarKey
            avatarPhoto = person.avatarUrl.flatMap(URL.init(string:))
            avatarColorKey = person.avatarKey
            avatarName = person.label
            avatarInitials = Avatar.initials(for: Address(name: person.label, email: person.email ?? ""))
        } else {
            avatarKey = ""
            avatarPhoto = nil
            avatarColorKey = space.name
            avatarName = title
            avatarInitials = Avatar.initials(for: Address(name: title, email: ""))
        }
    }

    static func preview(_ message: ChatMessage) -> String {
        var body = ChatMarkup.plain(message.text)
        if body.isEmpty {
            body = message.attachments.first.map { "Attachment: \($0.name)" } ?? (message.card ? "Card" : "")
        }
        let who = message.mine ? "You" : message.sender.shortLabel
        return body.isEmpty ? who : "\(who): \(body)"
    }
}

/// Every account's spaces, copied out of the store so rows are formatted off the main actor.
nonisolated struct ChatListInput: Sendable {
    struct Source: Sendable {
        let account: String
        let spaces: [ChatSpace]
    }

    let sources: [Source]

    /// Runs on the concurrent executor, never on the caller's actor.
    @concurrent func build() async -> [ChatRow] {
        let calendar = Calendar.current, now = Date()
        var rows: [ChatRow] = []
        rows.reserveCapacity(sources.reduce(0) { $0 + $1.spaces.count })
        for source in sources {
            for space in source.spaces { rows.append(ChatRow(space, account: source.account, calendar: calendar, now: now)) }
        }
        rows.sortNewestFirst(by: \.activity)
        return rows
    }
}
