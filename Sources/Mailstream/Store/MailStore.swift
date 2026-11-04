import AppKit
import Observation

nonisolated enum Selection: Hashable, Sendable {
    /// The same role across every account: All Inboxes, all Sent, …
    case unified(MailboxRole)
    case mailbox(MailboxKey)
    /// Google Chat spaces of one chat account, or of all of them (nil).
    case chat(String?)

    static let allInboxes = Selection.unified(.inbox)
    static let allChats = Selection.chat(nil)

    var isUnified: Bool {
        if case .unified = self { return true }
        return false
    }

    var isChat: Bool {
        if case .chat = self { return true }
        return false
    }
}

/// What a published conversation list was built for. The list compares this, not the live
/// selection, so it never dresses an old list in a new mailbox's layout while a rebuild is running.
nonisolated struct ListContext: Equatable, Sendable {
    var selection: Selection
    var search: String?
    var role: MailboxRole
}

/// A failure of the store itself rather than of a provider.
nonisolated struct StoreError: LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }

    static let signedOut = StoreError(message: "The account is no longer signed in.")
}

/// Single source of truth for the mail UI: accounts and mailboxes, the selected list with its
/// pagination and search, and mutations. Message bodies and previews live in `contents`, AI tags
/// in `tagging`.
///
/// The main actor only holds state and applies small changes: disk reads, network I/O, parsing,
/// threading and row formatting all run on the concurrent executor, and their results land here
/// as finished values. Lists paint from the disk cache before any request starts, then refresh
/// from the providers. Mutations apply optimistically to the visible list at once (no rethreading)
/// and roll back on failure.
///
/// Observable: the sidebar and window follow `accounts` and `mailboxes`, the list follows
/// `listContext`, `conversations` and `isLoading`; failures go to `problems`.
@Observable
final class MailStore {
    nonisolated static let pageSize = 50
    private static let prefetchCount = 40
    static let countsInterval: Duration = .seconds(300)
    /// Polling while push is down, and as a safety net while it is up (`sync.*` settings).
    private var pollInterval: Duration {
        .seconds(Settings.shared.int(push.isConnected ? .pushPollSeconds : .pollSeconds))
    }

    /// Signed-in accounts, their tokens and backends.
    let accountStore: AccountStore
    let cache: MailCache
    /// Message bodies and list previews.
    @ObservationIgnored let contents: ContentStore
    /// AI tags of the inbox conversations.
    @ObservationIgnored let tagging: TagStore
    @ObservationIgnored private(set) lazy var avatars = AvatarStore(accounts: accountStore, root: cache.root)
    /// Google Chat accounts, spaces and messages; selected through `Selection.chat`.
    @ObservationIgnored private(set) lazy var chat = ChatStore(cache: cache, accounts: accountStore)
    @ObservationIgnored private lazy var push = PushService(accounts: accountStore) { [weak self] id in
        self?.refresh(account: id)
    }
    var isPushConnected: Bool { push.isConnected }

    /// Failures of every store, for status lines and alerts.
    var problems: StoreProblems { accountStore.problems }

    private(set) var accounts: [Account] = []
    private(set) var mailboxes: [String: [Mailbox]] = [:]
    private(set) var selection: Selection = .allInboxes
    private(set) var conversations: [Conversation] = []
    private(set) var listContext = ListContext(selection: .allInboxes, search: nil, role: .inbox)
    private(set) var isLoading = false
    private(set) var searchQuery: String?
    /// When the list in view last finished refreshing, for `mailstream status`.
    @ObservationIgnored private(set) var lastRefresh: Date?

    private struct Listing {
        var envelopes: [Envelope]
        /// Cursor of the page after `envelopes`; nil at the end, or when the listing came from
        /// disk and no first page arrived yet.
        var next: String?
    }

    @ObservationIgnored private var listings: [MailboxKey: Listing] = [:]
    /// Disk reads of mailbox listings, in flight or done, so each is read at most once and a later
    /// caller awaits the read in flight instead of skipping it.
    @ObservationIgnored private var cacheReads: [MailboxKey: Task<Void, Never>] = [:]
    @ObservationIgnored private var searchResults: [MailboxKey: [Envelope]] = [:]
    /// Replaced together with `conversations`, which observers follow instead.
    @ObservationIgnored private var rows: [String: ConversationRow] = [:]
    /// Rethreads the list off the main actor. Requests made while a pass runs coalesce into one
    /// more pass, and a pass whose inputs changed under it is dropped rather than shown, so
    /// optimistic edits never flicker back.
    @ObservationIgnored private lazy var rebuild = CoalescedTask("Mail list rebuild") { [weak self] in
        await self?.rebuildPass()
    }
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    /// Pushed changes waiting out their coalescing delay, by account.
    @ObservationIgnored private var accountRefreshes: [String: Task<Void, Never>] = [:]
    /// First-page requests made per mailbox; only the latest one's answer is applied.
    @ObservationIgnored private var firstPageRequests: [MailboxKey: Int] = [:]
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var loadMoreTask: Task<Void, Never>?
    @ObservationIgnored private var polling: (interval: Duration, task: Task<Void, Never>)?
    /// Account changes, push connectivity and the counts refresh.
    @ObservationIgnored private var watchers: [Task<Void, Never>] = []

    /// Every account's first load finished, so identities are known: "to me" and "from me" mean
    /// something to the tagger, and a reconnecting push socket has a list to catch up.
    @ObservationIgnored private var connected = false

    init(accounts: AccountStore, tools: ToolEnvironment) {
        let cache = MailCache()
        let contents = ContentStore(accounts: accounts, cache: cache)
        accountStore = accounts
        self.cache = cache
        self.contents = contents
        tagging = TagStore(cache: cache, tools: tools, contents: contents, problems: accounts.problems)
    }

    isolated deinit {
        watchers.forEach { $0.cancel() }
        polling?.task.cancel()
        refreshTask?.cancel()
        searchTask?.cancel()
        loadMoreTask?.cancel()
        accountRefreshes.values.forEach { $0.cancel() }
        cacheReads.values.forEach { $0.cancel() }
    }

    // MARK: - Startup

    func start() {
        Task(name: "Mail startup") {
            await restoreFromCache()
            await connect()
        }
        chat.start()
        push.start()
        tagging.start()
        let accountStore = accountStore, push = push
        watchers = [
            Task.immediate(name: "Mail account changes") { [weak self] in
                for await _ in Observations({ accountStore.records }).dropFirst() { self?.accountsDidChange() }
            },
            Task.immediate(name: "Mail push connectivity") { [weak self] in
                for await _ in Observations({ push.isConnected }).dropFirst() { self?.pushConnectionChanged() }
            },
            Task.immediate(name: "Mail polling settings") { [weak self] in
                let intervals = Observations { (Settings.shared.int(.pollSeconds), Settings.shared.int(.pushPollSeconds)) }
                for await _ in intervals.dropFirst() { self?.schedulePolling() }
            },
            Task(name: "Mail counts") { [weak self] in
                while true {
                    do { try await Task.sleep(for: Self.countsInterval) } catch { return }
                    self?.refreshCounts()
                }
            },
        ]
        schedulePolling()
    }

    /// Polls every `sync.pollSeconds` while push is down, every `sync.pushPollSeconds` while it
    /// delivers changes.
    private func schedulePolling() {
        let interval = pollInterval
        if polling?.interval == interval { return }
        polling?.task.cancel()
        polling = (interval, Task(name: "Mail polling") { [weak self] in
            while true {
                do { try await Task.sleep(for: interval) } catch { return }
                guard let self else { return }
                if !isLoading { refresh() }
            }
        })
    }

    /// A socket that just connected catches up on whatever changed while it was down: the lists in
    /// view and every account's counts.
    private func pushConnectionChanged() {
        schedulePolling()
        guard push.isConnected, connected else { return }
        if !isLoading { refresh() }
        refreshCounts()
    }

    /// Paints the signed-in accounts with their last known identities, mailboxes, tags and lists,
    /// and starts fetching mail for them, before any provider has answered.
    private func restoreFromCache() async {
        async let restoredTags: Void = tagging.restore()
        let restored = signedInAccounts(keeping: await cache.loadAccounts() ?? [])
        await restoredTags
        guard !restored.isEmpty else { return }
        accounts = restored
        await loadCachedMailboxes(restored.map(\.name))
        rebuild.request()
        refresh()
    }

    private func connect() async {
        await withTaskGroup(of: Void.self) { group in
            for account in accounts { group.addTask { await self.loadAccount(account.name) } }
        }
        cache.saveAccounts(accounts)
        connected = true
        refresh()
        scheduleTagging()
        refreshCounts()
    }

    /// The signed-in accounts in sidebar order, with the identities `known` already has, else
    /// the name and address the sign-in reported.
    private func signedInAccounts(keeping known: [Account]) -> [Account] {
        let identities = Dictionary(known.compactMap { account in account.identity.map { (account.name, $0) } },
                                    uniquingKeysWith: { a, _ in a })
        return accountStore.records.map { record in
            Account(name: record.id, label: record.label, provider: record.provider,
                    identity: identities[record.id] ?? Address(name: record.displayName, email: record.id))
        }
    }

    /// Follows sign-ins, removals and renames: added accounts load, removed ones drop their
    /// mailboxes, listings, message content and selection. An account without mailboxes (its last
    /// load failed, e.g. before signing in again) loads again.
    private func accountsDidChange() {
        let next = signedInAccounts(keeping: accounts)
        let before = Set(accounts.map(\.name))
        let load = next.map(\.name).filter { !before.contains($0) || mailboxes[$0]?.isEmpty != false }
        if next != accounts {
            let removed = before.subtracting(next.map(\.name))
            accounts = next
            cache.saveAccounts(next)
            if !removed.isEmpty { forget(removed) }
            rebuild.request()
        }
        guard !load.isEmpty else { return }
        Task {
            await loadCachedMailboxes(load)
            await withTaskGroup(of: Void.self) { group in
                for name in load { group.addTask { await self.loadAccount(name) } }
            }
            cache.saveAccounts(accounts)
            refresh()
            refreshCounts()
        }
    }

    private func forget(_ removed: Set<String>) {
        mailboxes = mailboxes.filter { !removed.contains($0.key) }
        listings = listings.filter { !removed.contains($0.key.account) }
        searchResults = searchResults.filter { !removed.contains($0.key.account) }
        for (key, read) in cacheReads where removed.contains(key.account) {
            read.cancel()
            cacheReads[key] = nil
        }
        for account in removed { accountRefreshes.removeValue(forKey: account)?.cancel() }
        firstPageRequests = firstPageRequests.filter { !removed.contains($0.key.account) }
        contents.forget(accounts: removed)
        switch selection {
        case .mailbox(let key) where removed.contains(key.account): select(.allInboxes)
        case .chat(let account?) where removed.contains(account): select(.allChats)
        default: break
        }
    }

    private func loadCachedMailboxes(_ names: [String]) async {
        let cache = cache
        await withTaskGroup(of: (String, [Mailbox]?).self) { group in
            for name in names {
                group.addTask {
                    let list = await cache.loadMailboxes(name)
                    return (name, list)
                }
            }
            for await (name, list) in group {
                if let list, mailboxes[name] == nil { applyMailboxes(list, account: name) }
            }
        }
    }

    private func loadAccount(_ name: String) async {
        guard let backend = accountStore.backend(for: name) else { return }
        async let identity = try? backend.identity()
        do {
            let fresh = try await backend.mailboxes(counts: false)
            guard account(named: name) != nil else { return }
            let previous = Dictionary((mailboxes[name] ?? []).map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
            let built = fresh.map { mailbox in
                var mailbox = mailbox
                mailbox.unread = mailbox.unread ?? previous[mailbox.id]?.unread
                mailbox.total = mailbox.total ?? previous[mailbox.id]?.total
                return mailbox
            }
            applyMailboxes(built, account: name)
            cache.saveMailboxes(built, for: name)
        } catch {
            report(error, context: name)
        }
        if let identity = await identity, let index = accounts.firstIndex(where: { $0.name == name }),
           accounts[index].identity != identity {
            accounts[index].identity = identity
            // Rows say "me" for the user's own addresses.
            rebuild.request()
        }
    }

    private func applyMailboxes(_ list: [Mailbox], account: String) {
        mailboxes[account] = list.sorted {
            ($0.role.sortOrder, $0.displayName.localizedLowercase) < ($1.role.sortOrder, $1.displayName.localizedLowercase)
        }
    }

    func refreshCounts() {
        for account in accounts { refreshCounts(account: account.name) }
    }

    private func refreshCounts(account name: String) {
        guard let backend = accountStore.backend(for: name) else { return }
        Task {
            guard let fresh = try? await backend.mailboxes(counts: true) else { return }
            let counts = Dictionary(fresh.map { ($0.id, ($0.unread, $0.total)) }, uniquingKeysWith: { a, _ in a })
            guard var list = mailboxes[name] else { return }
            for index in list.indices {
                if let (unread, total) = counts[list[index].id] {
                    list[index].unread = unread ?? list[index].unread
                    list[index].total = total ?? list[index].total
                }
            }
            guard list != mailboxes[name] else { return }
            mailboxes[name] = list
            cache.saveMailboxes(list, for: name)
        }
    }

    // MARK: - Lookup

    func account(named name: String) -> Account? { accounts.first { $0.name == name } }

    /// The account's sidebar label; its id (the address) while unknown.
    func label(account name: String) -> String { account(named: name)?.label ?? name }

    func mailbox(_ key: MailboxKey) -> Mailbox? {
        mailboxes[key.account]?.first { $0.id == key.mailbox }
    }

    func mailbox(account: String, role: MailboxRole) -> Mailbox? {
        mailboxes[account]?.first { $0.role == role }
    }

    /// Lowercased addresses of every configured account, to render "me".
    var ownAddresses: Set<String> { Set(accounts.flatMap { [$0.name, $0.email.lowercased()] }) }

    /// `address` with the owning account's display name when the header carried none.
    func withIdentityName(_ address: Address) -> Address {
        guard address.displayName == address.email,
              let identity = accounts.first(where: { $0.email.lowercased() == address.normalizedEmail })?.identity
        else { return address }
        return Address(name: identity.name, email: address.email)
    }

    var selectionTitle: String {
        switch selection {
        case .unified(let role): role == .inbox ? "All Inboxes" : role.title
        case .mailbox(let key): mailbox(key)?.displayName ?? key.mailbox
        case .chat(let account): account.map { label(account: $0) } ?? "Chat"
        }
    }

    var selectionRole: MailboxRole {
        switch selection {
        case .unified(let role): role
        case .mailbox(let key): mailbox(key)?.role ?? .other
        case .chat: .other
        }
    }

    /// Each account's mailbox for `role`; Gmail has no Archive, so its All Mail stands in.
    func unifiedMailboxes(_ role: MailboxRole) -> [Mailbox] {
        accounts.compactMap { account in
            mailbox(account: account.name, role: role)
                ?? (role == .archive ? mailbox(account: account.name, role: .all) : nil)
        }
    }

    /// Unread count shown for a selection; drafts count every message.
    func badgeCount(_ selection: Selection) -> Int {
        let boxes: [Mailbox]
        switch selection {
        case .unified(let role): boxes = unifiedMailboxes(role)
        case .mailbox(let key): boxes = mailbox(key).map { [$0] } ?? []
        case .chat(let account): return chat.unreadSpaces(account: account)
        }
        return boxes.reduce(0) { $0 + (($1.role == .drafts ? $1.total : $1.unread) ?? 0) }
    }

    var canLoadMore: Bool {
        searchQuery == nil && primaryKeys.contains { listings[$0]?.next != nil }
    }

    /// The envelopes listed for `key` so far (memory or disk cache); empty when none are loaded.
    func loadedEnvelopes(_ key: MailboxKey) -> [Envelope] { listings[key]?.envelopes ?? [] }

    /// The mailboxes the list on screen shows.
    var primaryKeys: [MailboxKey] {
        switch selection {
        case .unified(let role): unifiedMailboxes(role).map(\.key)
        case .mailbox(let key): [key]
        case .chat: []
        }
    }

    /// The mailboxes the conversations on screen come from: the selection's, or while searching
    /// wherever the results are.
    var listedKeys: [MailboxKey] {
        guard searchQuery != nil else { return primaryKeys }
        return searchResults.keys.sorted { ($0.account, $0.mailbox) < ($1.account, $1.mailbox) }
    }

    /// Inbox and sent mail of the accounts in view, so conversations show both directions.
    private var contextKeys: [MailboxKey] {
        if [.trash, .junk, .drafts, .all].contains(selectionRole) { return [] }
        let primary = Set(primaryKeys)
        let accountNames = Set(primary.map(\.account))
        return accounts.filter { accountNames.contains($0.name) }.flatMap { account in
            [MailboxRole.inbox, .sent].compactMap { mailbox(account: account.name, role: $0)?.key }
        }.filter { !primary.contains($0) }
    }

    // MARK: - Selection and listing

    func select(_ selection: Selection) {
        guard selection != self.selection || conversations.isEmpty else { return }
        self.selection = selection
        searchTask?.cancel()
        loadMoreTask?.cancel()
        searchQuery = nil
        searchResults = [:]
        // The cancelled search or page load won't clear it; a refresh sets it again.
        if isLoading { isLoading = false }
        rebuild.request()
        if selection.isChat { chat.refresh() } else { refresh() }
    }

    /// The row a cell shows for `conversation`, formatted with the list it belongs to.
    func row(for conversation: Conversation) -> ConversationRow {
        rows[conversation.id] ?? ConversationRow(conversation, own: ownAddresses, recipients: listContext.role.showsRecipients,
                                                 calendar: .current, now: Date())
    }

    /// Reads the disk cache of mailboxes not loaded yet, off the main actor, all at once, and
    /// awaits reads already in flight. A listing landing in view outside a rebuild pass (which
    /// reads it anyway) requests one.
    private func loadCachedListings(_ keys: [MailboxKey], inRebuild: Bool = false) async {
        let cache = cache
        for key in keys where listings[key] == nil && cacheReads[key] == nil {
            cacheReads[key] = Task {
                let cached = await cache.loadEnvelopes(key)
                guard !Task.isCancelled, let cached, listings[key] == nil else { return }
                listings[key] = Listing(envelopes: cached, next: nil)
                if !inRebuild, (primaryKeys + contextKeys).contains(key) { rebuild.request() }
            }
        }
        for key in keys { await cacheReads[key]?.value }
    }

    private func sources(_ keys: [MailboxKey], primary: Bool) -> [ListInput.Source] {
        keys.map { key in
            let envelopes = primary && searchQuery != nil ? searchResults[key] ?? [] : listings[key]?.envelopes ?? []
            return ListInput.Source(key: key, role: mailbox(key)?.role ?? .other, envelopes: envelopes)
        }
    }

    private func rebuildPass() async {
        await loadCachedListings(primaryKeys + contextKeys, inRebuild: true)
        let context = ListContext(selection: selection, search: searchQuery, role: selectionRole)
        let input = ListInput(primary: sources(listedKeys, primary: true), context: sources(contextKeys, primary: false),
                              own: ownAddresses, recipients: context.role.showsRecipients,
                              mergeRepeats: context.role == .inbox && searchQuery == nil)
        let output = await input.build()
        // Inputs changed meanwhile: the next pass shows them.
        guard !rebuild.isPending else { return }
        conversations = output.conversations
        rows = output.rows
        if listContext != context { listContext = context }
        contents.prefetchSnippets(for: Array(conversations.prefix(Self.prefetchCount)))
        scheduleTagging()
    }

    func refresh() {
        let keys = primaryKeys + contextKeys
        guard !keys.isEmpty else { return }
        refreshTask?.cancel()
        isLoading = true
        problems.clear(.mail)
        refreshTask = Task(name: "Mail refresh") {
            await loadCachedListings(keys)
            await fetchFirstPages(keys)
            guard !Task.isCancelled else { return }
            isLoading = false
            lastRefresh = Date()
        }
    }

    /// Picks up a change push reported for `account`: its mailboxes in view and its counts.
    /// Bursts (a sync touching many messages) coalesce into one pass.
    func refresh(account: String) {
        guard accountRefreshes[account] == nil else { return }
        accountRefreshes[account] = Task(name: "Mail account refresh") {
            do { try await Task.sleep(for: .milliseconds(500)) } catch { return }
            accountRefreshes[account] = nil
            refreshCounts(account: account)
            await fetchFirstPages((primaryKeys + contextKeys).filter { $0.account == account })
        }
    }

    /// `keys` paired with their account's backend; keys of signed-out accounts drop out.
    private func backends(_ keys: [MailboxKey]) -> [(MailboxKey, any MailBackend)] {
        keys.compactMap { key in accountStore.backend(for: key.account).map { (key, $0) } }
    }

    /// First pages of `keys`. A key's answer counts only if no later request for it was made,
    /// so a slow older pass never overwrites a newer one.
    private func fetchFirstPages(_ keys: [MailboxKey]) async {
        var tickets: [MailboxKey: Int] = [:]
        for key in keys {
            firstPageRequests[key, default: 0] += 1
            tickets[key] = firstPageRequests[key]
        }
        await withTaskGroup(of: (MailboxKey, Result<EnvelopePage, any Error>).self) { group in
            for (key, backend) in backends(keys) {
                group.addTask {
                    do {
                        let page = try await backend.envelopes(mailbox: key.mailbox, cursor: nil,
                                                               pageSize: Self.pageSize, priority: .interactive)
                        return (key, .success(page))
                    } catch {
                        return (key, .failure(error))
                    }
                }
            }
            for await (key, result) in group {
                guard !Task.isCancelled, account(named: key.account) != nil,
                      firstPageRequests[key] == tickets[key] else { continue }
                switch result {
                case .success(let page):
                    if merge(firstPage: page, into: key) { rebuild.request() }
                case .failure(let error): report(error, context: key.account)
                }
            }
        }
    }

    /// Returns whether anything changed, so an unchanged mailbox costs no rebuild.
    private func merge(firstPage page: EnvelopePage, into key: MailboxKey) -> Bool {
        let before = listings[key]
        var listing = Listing(envelopes: page.envelopes, next: page.next)
        if page.next != nil, let before {
            let oldest = page.envelopes.compactMap(\.date).min() ?? .distantPast
            let ids = Set(page.envelopes.map(\.id))
            let older = before.envelopes.filter { !ids.contains($0.id) && ($0.date ?? .distantPast) < oldest }
            listing.envelopes += older
            // Pages loaded past the first keep their cursor; older mail only from disk is
            // reached again through the first page's.
            if !older.isEmpty, let next = before.next { listing.next = next }
        }
        guard listing.envelopes != before?.envelopes || listing.next != before?.next else { return false }
        listings[key] = listing
        persist(key)
        return true
    }

    private func persist(_ key: MailboxKey) {
        guard let envelopes = listings[key]?.envelopes.prefix(200) else { return }
        cache.saveEnvelopes(Array(envelopes), for: key)
    }

    func loadMore() {
        guard loadMoreTask == nil, canLoadMore else { return }
        let jobs = backends(primaryKeys.filter { listings[$0]?.next != nil })
        isLoading = true
        loadMoreTask = Task(name: "Mail load more") {
            defer { loadMoreTask = nil }
            for (key, backend) in jobs {
                guard let cursor = listings[key]?.next else { continue }
                do {
                    let page = try await backend.envelopes(mailbox: key.mailbox, cursor: cursor,
                                                           pageSize: Self.pageSize, priority: .interactive)
                    // `select` cancelled it and resets `isLoading`.
                    guard !Task.isCancelled else { return }
                    // The account was removed meanwhile.
                    guard var listing = listings[key] else { continue }
                    let known = Set(listing.envelopes.map(\.id))
                    listing.envelopes += page.envelopes.filter { !known.contains($0.id) }
                    listing.next = page.next
                    listings[key] = listing
                } catch {
                    if Task.isCancelled { return }
                    report(error, context: key.account)
                }
            }
            isLoading = false
            rebuild.request()
        }
    }

    // MARK: - Search

    /// Searches every mailbox of the accounts in view. Matches among mail already loaded show at
    /// once; the providers' own search, which also reads bodies and mail never loaded, follows once
    /// typing pauses and merges in.
    func search(_ text: String) {
        searchTask?.cancel()
        let query = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            if searchQuery != nil {
                searchQuery = nil
                searchResults = [:]
                isLoading = false
                rebuild.request()
            }
            return
        }
        searchQuery = query
        isLoading = true
        let accounts = Set(primaryKeys.map(\.account))
        let jobs = accounts.sorted().compactMap { account in accountStore.backend(for: account).map { (account, $0) } }
        let loaded = listings.filter { accounts.contains($0.key.account) && isSearchable($0.key) }.mapValues(\.envelopes)
        let snippets = contents.snippets
        searchTask = Task(name: "Mail search") {
            let local = await Self.matches(query, in: loaded, snippets: snippets)
            guard !Task.isCancelled else { return }
            searchResults = local
            rebuild.request()
            try? await Task.sleep(for: .milliseconds(450))
            guard !Task.isCancelled else { return }
            await withTaskGroup(of: (String, Result<[SearchHit], any Error>).self) { group in
                for (account, backend) in jobs {
                    group.addTask {
                        // Bound before the tuple is built: Swift 6.3 at -O drops the first element
                        // from `(account, .success(try await …))` on success.
                        do {
                            let hits = try await backend.search(query: query, pageSize: Self.pageSize)
                            return (account, .success(hits))
                        } catch {
                            return (account, .failure(error))
                        }
                    }
                }
                for await (account, result) in group {
                    guard !Task.isCancelled else { continue }
                    switch result {
                    case .success(let hits): merge(hits, account: account)
                    case .failure(let error): report(error, context: account)
                    }
                    rebuild.request()
                }
            }
            guard !Task.isCancelled else { return }
            isLoading = false
        }
    }

    /// Provider results replace the loaded-mail matches of the same messages.
    private func merge(_ hits: [SearchHit], account: String) {
        let ids = Set(hits.map(\.envelope.id))
        for key in searchResults.keys where key.account == account {
            searchResults[key]?.removeAll { ids.contains($0.id) }
        }
        for hit in hits {
            let key = MailboxKey(account: account, mailbox: hit.mailbox)
            if isSearchable(key) { searchResults[key, default: []].append(hit.envelope) }
        }
    }

    /// Search leaves out trash and junk unless they are what is selected, and folders the sidebar
    /// hides (Outlook's conversation history).
    func isSearchable(_ key: MailboxKey) -> Bool {
        guard let role = mailbox(key)?.role else { return false }
        return (role != .trash && role != .junk) || role == selectionRole
    }

    /// Loaded envelopes where every word of `query` appears in the subject, a sender or recipient,
    /// or the preview. Queries with provider operators (`from:`) wait for the provider.
    @concurrent private nonisolated static func matches(_ query: String, in loaded: [MailboxKey: [Envelope]],
                                                        snippets: [MessageRef: String]) async -> [MailboxKey: [Envelope]] {
        let terms = query.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !terms.contains(where: { $0.contains(":") }) else { return [:] }
        var seen = Set<String>()
        var out: [MailboxKey: [Envelope]] = [:]
        for (key, envelopes) in loaded {
            for envelope in envelopes where !seen.contains(key.account + "\u{1}" + envelope.id) {
                let people = (envelope.from + envelope.to).map { "\($0.name ?? "") \($0.email)" }.joined(separator: " ")
                let text = [envelope.subject, people, snippets[MessageRef(account: key.account, mailbox: key.mailbox, id: envelope.id)] ?? ""]
                    .joined(separator: "\n")
                guard terms.allSatisfy({ text.range(of: $0, options: [.caseInsensitive, .diacriticInsensitive]) != nil }) else { continue }
                seen.insert(key.account + "\u{1}" + envelope.id)
                out[key, default: []].append(envelope)
            }
        }
        return out
    }

    /// Every sender and recipient in loaded listings, for recipient autocompletion.
    func knownAddresses() -> [Address] {
        var seen = Set<String>()
        var out: [Address] = []
        for listing in listings.values {
            for envelope in listing.envelopes {
                for address in envelope.from + envelope.to where seen.insert(address.normalizedEmail).inserted {
                    out.append(address)
                }
            }
        }
        return out
    }

    /// Lowercased addresses the user has written to in loaded listings: recipients of sent mail.
    func correspondents() -> Set<String> {
        let own = ownAddresses
        var out = Set<String>()
        for (key, listing) in listings {
            let sent = mailbox(key)?.role == .sent
            for envelope in listing.envelopes where sent || envelope.from.contains(where: { own.contains($0.normalizedEmail) }) {
                out.formUnion(envelope.to.map(\.normalizedEmail))
            }
        }
        return out
    }

    // MARK: - AI tagging

    /// Offers the conversations with a message in a listed inbox to the tagger, once identities
    /// are known.
    private func scheduleTagging() {
        guard connected else { return }
        let primary = Set(primaryKeys)
        tagging.schedule(conversations.filter { conversation in
            conversation.messages.contains { $0.role == .inbox && primary.contains($0.ref.mailboxKey) }
        }, own: ownAddresses)
    }

    // MARK: - Mutations

    /// Applies at once; the returned task finishes when the provider confirmed (and throws after
    /// rolling back and reporting if it refused).
    @discardableResult
    func setRead(_ conversation: Conversation, read: Bool) -> Task<Void, any Error> {
        let targets = read
            ? conversation.messages.filter { !$0.envelope.isSeen }
            : [conversation.messages.last(where: { $0.role != .sent }) ?? conversation.latest]
        guard !targets.isEmpty else { return Task {} }
        let task = mutateFlags(targets, flag: .seen, add: read)
        adjustUnread(targets, delta: read ? -1 : 1)
        return task
    }

    @discardableResult
    func setFlagged(_ conversation: Conversation, flagged: Bool) -> Task<Void, any Error> {
        let targets = flagged ? [conversation.latest] : conversation.messages.filter(\.envelope.isFlagged)
        return mutateFlags(targets, flag: .flagged, add: flagged)
    }

    private func mutateFlags(_ targets: [MailMessage], flag: MessageFlag, add: Bool) -> Task<Void, any Error> {
        let refs = Set(targets.map(\.ref))
        updateEnvelopes(refs) { envelope in
            if add { envelope.flags.insert(flag.rawValue) } else { envelope.flags.remove(flag.rawValue) }
        }
        let requests = Dictionary(grouping: targets, by: \.ref.mailboxKey).map { key, messages in
            let ids = messages.map(\.ref.id)
            return Task {
                do {
                    try await backend(for: key.account).setFlag(flag, add, ids: ids, mailbox: key.mailbox)
                } catch {
                    updateEnvelopes(Set(messages.map(\.ref))) { envelope in
                        if add { envelope.flags.remove(flag.rawValue) } else { envelope.flags.insert(flag.rawValue) }
                    }
                    report(error, context: key.account, alert: true)
                    throw error
                }
            }
        }
        return Self.all(requests)
    }

    /// Finishes when every request has; throws the first failure.
    private static func all(_ requests: [Task<Void, any Error>]) -> Task<Void, any Error> {
        Task {
            var failure: (any Error)?
            for request in requests {
                do { try await request.value } catch { failure = failure ?? error }
            }
            if let failure { throw failure }
        }
    }

    private func backend(for account: String) throws -> any MailBackend {
        guard let backend = accountStore.backend(for: account) else { throw StoreError.signedOut }
        return backend
    }

    private func adjustUnread(_ targets: [MailMessage], delta: Int) {
        for (key, messages) in Dictionary(grouping: targets, by: \.ref.mailboxKey) {
            guard var list = mailboxes[key.account], let index = list.firstIndex(where: { $0.id == key.mailbox }),
                  let unread = list[index].unread else { continue }
            list[index].unread = max(0, unread + delta * messages.count)
            mailboxes[key.account] = list
        }
    }

    /// Destination of "Archive": the archive folder, else Gmail's All Mail.
    func archiveMailbox(account: String) -> Mailbox? {
        mailbox(account: account, role: .archive) ?? mailbox(account: account, role: .all)
    }

    func canArchive(_ conversation: Conversation) -> Bool {
        archiveMailbox(account: conversation.account) != nil && !archivable(conversation).isEmpty
    }

    /// Messages "Archive" moves: those in `primary` (default: the list on screen) that sit in an
    /// inbox-like mailbox.
    private func archivable(_ conversation: Conversation, primary: Set<MailboxKey>? = nil) -> [MailMessage] {
        let primary = primary ?? Set(listedKeys)
        return conversation.messages.filter {
            primary.contains($0.ref.mailboxKey) && ![.sent, .all, .archive, .trash, .drafts].contains($0.role)
        }
    }

    /// `primary` names the mailboxes the conversation was listed from, when not the screen's.
    /// With `undoManager`, Undo puts the messages back.
    @discardableResult
    func archive(_ conversation: Conversation, from primary: Set<MailboxKey>? = nil,
                 undoManager: UndoManager? = nil) -> Task<Void, any Error> {
        guard let target = archiveMailbox(account: conversation.account) else {
            return Task { throw StoreError(message: "\(conversation.account) has no archive mailbox.") }
        }
        let targets = archivable(conversation, primary: primary)
        guard !targets.isEmpty else {
            return Task { throw StoreError(message: "Nothing in \(conversation.id) is in a mailbox Archive moves from.") }
        }
        return move(targets, to: target, undoManager: undoManager, name: "Archive")
    }

    /// Moves the conversation to the trash; messages already there are deleted for good, which
    /// Undo cannot take back.
    @discardableResult
    func trash(_ conversation: Conversation, undoManager: UndoManager? = nil) -> Task<Void, any Error> {
        guard let trash = mailbox(account: conversation.account, role: .trash) else {
            let error = StoreError(message: "No trash mailbox found.")
            report(error, context: conversation.account, alert: true)
            return Task { throw error }
        }
        let inTrash = conversation.messages.filter { $0.ref.mailbox == trash.id }
        let elsewhere = conversation.messages.filter { $0.ref.mailbox != trash.id && $0.role != .drafts }
        var requests = [move(elsewhere, to: trash, undoManager: undoManager, name: "Move to Trash")]
        if !inTrash.isEmpty {
            removeEnvelopes(inTrash)
            requests.append(Task {
                do {
                    try await backend(for: trash.account).delete(ids: inTrash.map(\.ref.id), mailbox: trash.id)
                } catch {
                    report(error, context: trash.account, alert: true)
                    refresh()
                    refreshCounts(account: trash.account)
                    throw error
                }
            })
        }
        return Self.all(requests)
    }

    /// Messages of the conversation in the trash or junk, which "Move to Inbox" takes back.
    private func restorable(_ conversation: Conversation) -> [MailMessage] {
        conversation.messages.filter { $0.role == .trash || $0.role == .junk }
    }

    func canRestore(_ conversation: Conversation) -> Bool {
        mailbox(account: conversation.account, role: .inbox) != nil && !restorable(conversation).isEmpty
    }

    /// Moves the conversation's trashed or junk messages to the inbox.
    @discardableResult
    func restore(_ conversation: Conversation, undoManager: UndoManager? = nil) -> Task<Void, any Error> {
        guard let inbox = mailbox(account: conversation.account, role: .inbox) else {
            return Task { throw StoreError(message: "\(conversation.account) has no inbox.") }
        }
        let targets = restorable(conversation)
        guard !targets.isEmpty else {
            return Task { throw StoreError(message: "Nothing in \(conversation.id) is in the trash or junk.") }
        }
        return move(targets, to: inbox, undoManager: undoManager, name: "Move to Inbox")
    }

    private func move(_ targets: [MailMessage], to destination: Mailbox, undoManager: UndoManager? = nil,
                      name: String = "Move") -> Task<Void, any Error> {
        let targets = targets.filter { $0.ref.mailbox != destination.id }
        guard !targets.isEmpty else { return Task {} }
        removeEnvelopes(targets)
        if let undoManager {
            undoManager.registerUndo(withTarget: self) { store in
                store.putBack(targets, from: destination, undoManager: undoManager, name: name)
            }
            undoManager.setActionName(name)
        }
        let requests = Dictionary(grouping: targets, by: \.ref.mailboxKey).map { key, messages in
            Task {
                do {
                    try await backend(for: key.account).move(ids: messages.map(\.ref.id), from: key.mailbox, to: destination.id)
                } catch {
                    report(error, context: key.account, alert: true)
                    refresh()
                    refreshCounts(account: key.account)
                    throw error
                }
            }
        }
        return Self.all(requests)
    }

    /// Undo of `move`: `messages` (as they were listed before it) leave `destination` for the
    /// mailboxes they came from, and show there again at once. Redo moves them again.
    private func putBack(_ messages: [MailMessage], from destination: Mailbox, undoManager: UndoManager, name: String) {
        removeEnvelopes(messages.map { message in
            MailMessage(ref: MessageRef(account: message.ref.account, mailbox: destination.id, id: message.ref.id),
                        envelope: message.envelope, role: destination.role)
        })
        let groups = Dictionary(grouping: messages, by: \.ref.mailboxKey)
        for (key, group) in groups {
            let envelopes = group.map(\.envelope)
            if var listing = listings[key] {
                let known = Set(listing.envelopes.map(\.id))
                listing.envelopes += envelopes.filter { !known.contains($0.id) }
                listing.envelopes.sort { ($0.date ?? .distantPast) > ($1.date ?? .distantPast) }
                listings[key] = listing
                persist(key)
            }
            if searchQuery != nil { searchResults[key, default: []] += envelopes }
        }
        adjustUnread(messages.filter { !$0.envelope.isSeen }, delta: 1)
        rebuild.request()
        undoManager.registerUndo(withTarget: self) { store in
            _ = store.move(messages, to: destination, undoManager: undoManager, name: name)
        }
        undoManager.setActionName(name)
        for (key, group) in groups {
            Task {
                do {
                    try await backend(for: key.account).move(ids: group.map(\.ref.id), from: destination.id, to: key.mailbox)
                } catch {
                    report(error, context: key.account, alert: true)
                    refresh()
                    refreshCounts(account: key.account)
                }
            }
        }
    }

    /// Sends from `account`, then picks up the sent copy.
    func send(_ message: OutgoingMessage, account: String) async throws {
        try await backend(for: account).send(message)
        Task {
            try? await Task.sleep(for: .seconds(1))
            refresh(account: account)
        }
    }

    /// Flags never change threading, so the visible list is patched in place instead of rebuilt.
    private func updateEnvelopes(_ refs: Set<MessageRef>, _ change: (inout Envelope) -> Void) {
        for key in Set(refs.map(\.mailboxKey)) {
            let ids = Set(refs.filter { $0.mailboxKey == key }.map(\.id))
            if var listing = listings[key] {
                for index in listing.envelopes.indices where ids.contains(listing.envelopes[index].id) {
                    change(&listing.envelopes[index])
                }
                listings[key] = listing
                persist(key)
            }
            if var results = searchResults[key] {
                for index in results.indices where ids.contains(results[index].id) { change(&results[index]) }
                searchResults[key] = results
            }
        }
        patchList(touching: refs) { $0.updating(refs, change) }
    }

    /// Drops the messages from the visible list and their mailboxes' unread counts at once; a
    /// rebuild then reconciles threading.
    private func removeEnvelopes(_ targets: [MailMessage]) {
        let refs = Set(targets.map(\.ref))
        adjustUnread(targets.filter { !$0.envelope.isSeen }, delta: -1)
        for key in Set(refs.map(\.mailboxKey)) {
            let ids = Set(refs.filter { $0.mailboxKey == key }.map(\.id))
            listings[key]?.envelopes.removeAll { ids.contains($0.id) }
            searchResults[key]?.removeAll { ids.contains($0.id) }
            persist(key)
        }
        let primary = Set(listedKeys)
        patchList(touching: refs) { conversation in
            let remaining = conversation.messages.filter { !refs.contains($0.ref) }
            guard remaining.contains(where: { primary.contains($0.ref.mailboxKey) }) else { return nil }
            return Conversation(id: conversation.id, account: conversation.account, messages: remaining)
        }
        rebuild.request()
    }

    /// Replaces (or with nil, removes) the visible conversations holding any of `refs`, reformats
    /// their rows, and publishes the list; a rebuild already running read the old envelopes, so it
    /// runs again.
    private func patchList(touching refs: Set<MessageRef>, _ transform: (Conversation) -> Conversation?) {
        var changed = false
        var next: [Conversation] = []
        next.reserveCapacity(conversations.count)
        let own = ownAddresses, recipients = listContext.role.showsRecipients
        let calendar = Calendar.current, now = Date()
        for conversation in conversations {
            guard conversation.messages.contains(where: { refs.contains($0.ref) }) else {
                next.append(conversation)
                continue
            }
            changed = true
            if let updated = transform(conversation) {
                next.append(updated)
                rows[updated.id] = ConversationRow(updated, own: own, recipients: recipients, calendar: calendar, now: now)
            } else {
                rows[conversation.id] = nil
            }
        }
        if rebuild.isRunning { rebuild.request() }
        guard changed else { return }
        conversations = next
    }

    // MARK: - Reporting

    private func report(_ error: any Error, context: String? = nil, alert: Bool = false) {
        problems.report(error, from: .mail, context: context, alert: alert)
    }
}

/// Everything a list rebuild reads, copied out of the store so threading and row formatting run
/// on the concurrent executor. Arrays are copy-on-write, so taking this snapshot copies nothing.
nonisolated struct ListInput: Sendable {
    struct Source: Sendable {
        let key: MailboxKey
        let role: MailboxRole
        let envelopes: [Envelope]
    }

    let primary: [Source]
    let context: [Source]
    let own: Set<String>
    let recipients: Bool
    /// Folds one sender's repeats on a subject into one conversation (`Threader.mergeRepeats`).
    let mergeRepeats: Bool

    struct Output: Sendable {
        let conversations: [Conversation]
        let rows: [String: ConversationRow]
    }

    /// Runs on the concurrent executor, never on the caller's actor.
    @concurrent func build() async -> Output {
        func messages(_ sources: [Source]) -> [MailMessage] {
            var out: [MailMessage] = []
            out.reserveCapacity(sources.reduce(0) { $0 + $1.envelopes.count })
            for source in sources {
                for envelope in source.envelopes {
                    out.append(MailMessage(ref: MessageRef(account: source.key.account, mailbox: source.key.mailbox, id: envelope.id),
                                           envelope: envelope, role: source.role))
                }
            }
            return out
        }
        let threaded = Threader.conversations(primary: messages(primary), context: messages(context))
        let conversations = mergeRepeats ? Threader.mergeRepeats(threaded) : threaded
        return Output(conversations: conversations,
                      rows: ConversationRow.rows(for: conversations, own: own, recipients: recipients))
    }
}
