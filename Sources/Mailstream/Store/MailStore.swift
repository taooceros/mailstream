import AppKit

enum Selection: Hashable, Sendable {
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
struct ListContext: Equatable, Sendable {
    var selection: Selection
    var search: String?
    var role: MailboxRole
}

extension Notification.Name {
    /// Accounts, mailboxes or unread counts changed.
    static let storeAccountsChanged = Notification.Name("MailStore.accountsChanged")
    /// The conversation list for the current selection, or its loading state, changed.
    static let storeListChanged = Notification.Name("MailStore.listChanged")
    /// Snippets or message bodies arrived; `userInfo["refs"]` is a `Set<MessageRef>`, batched per
    /// main-actor turn.
    static let storeContentLoaded = Notification.Name("MailStore.contentLoaded")
    /// A user-facing failure; `userInfo["message"]` is the text.
    static let storeError = Notification.Name("MailStore.error")
}

/// A failure of the store itself rather than of a provider.
struct StoreError: LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }

    static let signedOut = StoreError(message: "The account is no longer signed in.")
}

/// Single source of truth for the UI, and the only main-actor part of the data layer.
///
/// The main actor only holds state and applies small changes: disk reads, network I/O, parsing,
/// threading and row formatting all run on the concurrent executor, and their results land here
/// as finished values. Lists paint from the disk cache before any request starts, then refresh
/// from the providers. Mutations apply optimistically to the visible list at once (no rethreading)
/// and roll back on failure.
@MainActor
final class MailStore {
    nonisolated static let pageSize = 50
    static let prefetchCount = 40
    /// Polling while push is down, and as a safety net while it is up (`sync.*` settings).
    private var pollInterval: TimeInterval {
        TimeInterval(Settings.shared.int(push.isConnected ? .pushPollSeconds : .pollSeconds))
    }

    /// Signed-in accounts, their tokens and backends.
    let accountStore: AccountStore
    private let tools: ToolEnvironment
    let cache = MailCache()
    private(set) lazy var avatars = AvatarStore(accounts: accountStore, root: cache.root)
    /// Google Chat accounts, spaces and messages; selected through `Selection.chat`.
    private(set) lazy var chat = ChatStore(cache: cache, accounts: accountStore)
    private lazy var push = PushService(accounts: accountStore) { [weak self] id in self?.refresh(account: id) }
    var isPushConnected: Bool { push.isConnected }

    private(set) var accounts: [Account] = []
    private(set) var mailboxes: [String: [Mailbox]] = [:]
    private(set) var selection: Selection = .allInboxes
    private(set) var conversations: [Conversation] = []
    private(set) var listContext = ListContext(selection: .allInboxes, search: nil, role: .inbox)
    private(set) var isLoading = false
    private(set) var searchQuery: String?
    private(set) var lastRefresh: Date?
    private(set) var lastError: String?

    private struct Listing {
        var envelopes: [Envelope]
        /// Cursor of the page after `envelopes`; nil at the end, or when the listing came from
        /// disk and no first page arrived yet.
        var next: String?
    }

    private var listings: [MailboxKey: Listing] = [:]
    /// Mailboxes whose disk cache was already read (hit or miss), so it is read at most once.
    private var cacheRead = Set<MailboxKey>()
    private var searchResults: [MailboxKey: [Envelope]] = [:]
    private var rows: [String: ConversationRow] = [:]
    /// Display-ready previews: quoted history trimmed, whitespace collapsed.
    private var snippets: [MessageRef: String] = [:]
    private var contents: [MessageRef: LoadedContent] = [:]
    private var contentOrder: [MessageRef] = []
    private var contentTasks: [MessageRef: Task<LoadedContent, Error>] = [:]
    private var prefetching = Set<MessageRef>()
    private var loadedRefs = Set<MessageRef>()
    private var rebuildTask: Task<Void, Never>?
    private var needsRebuild = false
    private var refreshTask: Task<Void, Never>?
    /// Pushed changes waiting out their coalescing delay, by account.
    private var accountRefreshes: [String: Task<Void, Never>] = [:]
    /// First-page requests made per mailbox; only the latest one's answer is applied.
    private var firstPageRequests: [MailboxKey: Int] = [:]
    private var searchTask: Task<Void, Never>?
    private var loadMoreTask: Task<Void, Never>?
    private var pollTimer: Timer?
    private var countsTimer: Timer?
    private var observers: [NSObjectProtocol] = []

    private lazy var tagger = OmpTagger(tools: tools, workingDirectory: cache.root)
    /// Keyed by `tagKey(_:)`, so a new message in a conversation re-tags it.
    private var tags: [String: ConversationTag] = [:]
    /// Keys the model was asked about but skipped; automatic passes leave them alone.
    private var tagAttempted = Set<String>()
    private var taggingEnabled = true
    /// Identities are known, so "to me" and "from me" are meaningful.
    private var taggingReady = false
    private var taggingTask: Task<Void, Never>?
    private var taggingDebounce: Task<Void, Never>?
    private var taggingRerun = false
    private var taggingBackoffUntil: Date?

    init(accounts: AccountStore, tools: ToolEnvironment) {
        self.accountStore = accounts
        self.tools = tools
        taggingEnabled = Settings.shared.bool(.aiTaggingEnabled)
    }

    // MARK: - Startup

    func start() {
        Task {
            await restoreFromCache()
            await connect()
        }
        chat.start()
        push.start()
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: AccountStore.didChange, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.accountsDidChange() }
        })
        observers.append(center.addObserver(forName: PushService.connectionChanged, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.pushConnectionChanged() }
        })
        observers.append(center.addObserver(forName: Settings.didChange, object: nil, queue: .main) { [weak self] note in
            let changed = note.changedSettings
            MainActor.assumeIsolated { self?.settingsDidChange(changed) }
        })
        schedulePolling()
        countsTimer = Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshCounts() }
        }
    }

    /// Polls every `sync.pollSeconds` while push is down, every `sync.pushPollSeconds` while it
    /// delivers changes.
    private func schedulePolling() {
        let interval = pollInterval
        if let pollTimer, pollTimer.timeInterval == interval { return }
        pollTimer?.invalidate()
        pollTimer = Timer.scheduledTimer(withTimeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { if self?.isLoading == false { self?.refresh() } }
        }
    }

    /// A socket that just connected catches up on whatever changed while it was down.
    private func pushConnectionChanged() {
        schedulePolling()
        if push.isConnected, taggingReady, !isLoading { refresh() }
    }

    /// Paints the signed-in accounts with their last known identities, mailboxes, tags and lists,
    /// and starts fetching mail for them, before any provider has answered.
    private func restoreFromCache() async {
        let cache = cache
        async let savedTags = cache.loadTags()
        async let savedAccounts = cache.loadAccounts()
        if let saved = await savedTags { tags.merge(saved) { current, _ in current } }
        let restored = signedInAccounts(keeping: await savedAccounts ?? [])
        guard !restored.isEmpty else { return }
        accounts = restored
        await loadCachedMailboxes(restored.map(\.name))
        post(.storeAccountsChanged)
        setNeedsRebuild()
        refresh()
    }

    private func connect() async {
        await withTaskGroup(of: Void.self) { group in
            for account in accounts { group.addTask { await self.loadAccount(account.name) } }
        }
        cache.saveAccounts(accounts)
        taggingReady = true
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
    /// mailboxes, listings and selection. An account without mailboxes (its last load failed,
    /// e.g. before signing in again) loads again.
    private func accountsDidChange() {
        let next = signedInAccounts(keeping: accounts)
        let before = Set(accounts.map(\.name))
        let load = next.map(\.name).filter { !before.contains($0) || mailboxes[$0]?.isEmpty != false }
        if next != accounts {
            let removed = before.subtracting(next.map(\.name))
            accounts = next
            cache.saveAccounts(next)
            if !removed.isEmpty { forget(removed) }
            post(.storeAccountsChanged)
            setNeedsRebuild()
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
        cacheRead = cacheRead.filter { !removed.contains($0.account) }
        switch selection {
        case .mailbox(let key) where removed.contains(key.account): select(.allInboxes)
        case .chat(let account?) where removed.contains(account): select(.allChats)
        default: break
        }
    }

    private func loadCachedMailboxes(_ names: [String]) async {
        let cache = cache
        await withTaskGroup(of: (String, [Mailbox]?).self) { group in
            for name in names { group.addTask { (name, await cache.loadMailboxes(name)) } }
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
            setNeedsRebuild()
        }
        post(.storeAccountsChanged)
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
            post(.storeAccountsChanged)
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
        setNeedsRebuild()
        if selection.isChat { chat.refresh() } else { refresh() }
    }

    /// The row a cell shows for `conversation`, formatted with the list it belongs to.
    func row(for conversation: Conversation) -> ConversationRow {
        rows[conversation.id] ?? ConversationRow(conversation, own: ownAddresses, recipients: listContext.role.showsRecipients,
                                                 calendar: .current, now: Date())
    }

    /// Reads the disk cache of mailboxes not loaded yet, off the main actor, all at once.
    private func loadCachedListings(_ keys: [MailboxKey]) async {
        let missing = keys.filter { listings[$0] == nil && !cacheRead.contains($0) }
        guard !missing.isEmpty else { return }
        cacheRead.formUnion(missing)
        let cache = cache
        await withTaskGroup(of: (MailboxKey, [Envelope]?).self) { group in
            for key in missing { group.addTask { (key, await cache.loadEnvelopes(key)) } }
            for await (key, cached) in group {
                guard let cached, listings[key] == nil else { continue }
                listings[key] = Listing(envelopes: cached, next: nil)
            }
        }
    }

    private func sources(_ keys: [MailboxKey], primary: Bool) -> [ListInput.Source] {
        keys.map { key in
            let envelopes = primary && searchQuery != nil ? searchResults[key] ?? [] : listings[key]?.envelopes ?? []
            return ListInput.Source(key: key, role: mailbox(key)?.role ?? .other, envelopes: envelopes)
        }
    }

    /// Rethreads the list off the main actor. Requests made while a pass runs coalesce into one more
    /// pass, and a pass whose inputs changed under it is dropped rather than shown, so optimistic
    /// edits never flicker back.
    private func setNeedsRebuild() {
        needsRebuild = true
        guard rebuildTask == nil else { return }
        rebuildTask = Task { [weak self] in
            await self?.rebuildPasses()
        }
    }

    private func rebuildPasses() async {
        while needsRebuild {
            needsRebuild = false
            await loadCachedListings(primaryKeys + contextKeys)
            let context = ListContext(selection: selection, search: searchQuery, role: selectionRole)
            let input = ListInput(primary: sources(primaryKeys, primary: true), context: sources(contextKeys, primary: false),
                                  own: ownAddresses, recipients: context.role.showsRecipients)
            let output = await input.build()
            guard !needsRebuild else { continue }
            conversations = output.conversations
            rows = output.rows
            listContext = context
            post(.storeListChanged)
            prefetchSnippets()
            scheduleTagging()
        }
        rebuildTask = nil
    }

    func refresh() {
        let keys = primaryKeys + contextKeys
        guard !keys.isEmpty else { return }
        refreshTask?.cancel()
        isLoading = true
        lastError = nil
        post(.storeListChanged)
        refreshTask = Task {
            await loadCachedListings(keys)
            await fetchFirstPages(keys)
            guard !Task.isCancelled else { return }
            isLoading = false
            lastRefresh = Date()
            post(.storeListChanged)
        }
    }

    /// Picks up a change push reported for `account`: its mailboxes in view and its counts.
    /// Bursts (a sync touching many messages) coalesce into one pass.
    func refresh(account: String) {
        guard accountRefreshes[account] == nil else { return }
        accountRefreshes[account] = Task {
            try? await Task.sleep(for: .milliseconds(500))
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
        await withTaskGroup(of: (MailboxKey, Result<EnvelopePage, Error>).self) { group in
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
                    if merge(firstPage: page, into: key) { setNeedsRebuild() }
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
        post(.storeListChanged)
        loadMoreTask = Task {
            defer { loadMoreTask = nil }
            for (key, backend) in jobs {
                guard let cursor = listings[key]?.next else { continue }
                do {
                    let page = try await backend.envelopes(mailbox: key.mailbox, cursor: cursor,
                                                           pageSize: Self.pageSize, priority: .interactive)
                    guard !Task.isCancelled, var listing = listings[key] else { return }
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
            post(.storeListChanged)
            setNeedsRebuild()
        }
    }

    // MARK: - Search

    func search(_ text: String) {
        searchTask?.cancel()
        let query = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            if searchQuery != nil {
                searchQuery = nil
                searchResults = [:]
                isLoading = false
                setNeedsRebuild()
            }
            return
        }
        searchQuery = query
        searchResults = [:]
        isLoading = true
        setNeedsRebuild()
        let jobs = backends(primaryKeys)
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            await withTaskGroup(of: (MailboxKey, Result<[Envelope], Error>).self) { group in
                for (key, backend) in jobs {
                    group.addTask {
                        // Bound before the tuple is built: Swift 6.3 at -O drops `key` from
                        // `(key, .success(try await …))` on success, so the list got garbage keys.
                        do {
                            let list = try await backend.search(mailbox: key.mailbox, query: query, pageSize: Self.pageSize)
                            return (key, .success(list))
                        } catch {
                            return (key, .failure(error))
                        }
                    }
                }
                for await (key, result) in group {
                    guard !Task.isCancelled else { continue }
                    switch result {
                    case .success(let list): searchResults[key] = list
                    case .failure(let error): report(error, context: key.account)
                    }
                    setNeedsRebuild()
                }
            }
            guard !Task.isCancelled else { return }
            isLoading = false
            post(.storeListChanged)
        }
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

    // MARK: - Content

    /// A message as the reader needs it, with its display preview and body markup, worked out off
    /// the main actor.
    struct LoadedContent: Sendable {
        let content: MessageContent
        let snippet: String
        /// The HTML body, or the plain-text one as escaped HTML.
        let body: String
        let isHTML: Bool
        /// Whether the HTML body references remote images, styles or fonts.
        let hasRemote: Bool

        init(_ content: MessageContent) {
            self.content = content
            snippet = MailStore.displaySnippet(content.snippet)
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

    /// Display-ready preview for the list, the reader's collapsed cards and the tagger.
    func snippet(for ref: MessageRef) -> String? { snippets[ref] }

    func loadedContent(for ref: MessageRef) -> MessageContent? { contents[ref]?.content }

    /// The message's reader-ready content if it is in memory, so a thread can render it in its first frame.
    func preparedContent(for ref: MessageRef) -> LoadedContent? { contents[ref] }

    func content(for ref: MessageRef, priority: RequestPriority = .interactive) async throws -> LoadedContent {
        if let loaded = contents[ref] { return loaded }
        let task = contentTasks[ref] ?? loadTask(ref, priority: priority)
        let loaded = try await task.value
        remember(loaded, for: ref)
        return loaded
    }

    /// Starts loading the conversation the user is about to open (pointer resting on it, keyboard
    /// highlight), newest messages first, so the reader finds their bodies in memory.
    func preload(_ conversation: Conversation) {
        for message in conversation.messages.suffix(Self.preloadCount).reversed() {
            let ref = message.ref
            guard contents[ref] == nil else { continue }
            let task = loadTask(ref, priority: .interactive)
            Task {
                guard let loaded = try? await task.value, contents[ref] == nil else { return }
                remember(loaded, for: ref)
            }
        }
    }

    /// Messages `preload` brings in per conversation: enough for the thread's visible end.
    private static let preloadCount = 10

    /// One shared load per message: disk cache first, else the provider, assembled and cached off
    /// the main actor.
    private func loadTask(_ ref: MessageRef, priority: RequestPriority) -> Task<LoadedContent, Error> {
        if let task = contentTasks[ref] { return task }
        let backend = accountStore.backend(for: ref.account), cache = cache
        let task = Task.detached(priority: priority == .interactive ? .userInitiated : .utility) {
            if let cached = await cache.loadContent(ref) { return LoadedContent(cached) }
            guard let backend else { throw StoreError.signedOut }
            let content = try await backend.content(id: ref.id, mailbox: ref.mailbox, directory: cache.messageDirectory(ref),
                                                    urlBase: cache.partURLBase(ref), priority: priority)
            cache.saveContent(content, for: ref)
            return LoadedContent(content)
        }
        contentTasks[ref] = task
        Task {
            _ = try? await task.value
            if contentTasks[ref] == task { contentTasks[ref] = nil }
        }
        return task
    }

    nonisolated static func displaySnippet(_ raw: String) -> String {
        HTMLText.trimHistory(raw)
            .split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    private func remember(_ loaded: LoadedContent, for ref: MessageRef) {
        if contents[ref] == nil { contentOrder.append(ref) }
        contents[ref] = loaded
        while contentOrder.count > 80 { contents[contentOrder.removeFirst()] = nil }
        setSnippet(loaded.snippet, for: ref)
    }

    private func setSnippet(_ snippet: String, for ref: MessageRef) {
        guard snippets[ref] != snippet else { return }
        snippets[ref] = snippet
        loadedRefs.insert(ref)
        guard loadedRefs.count == 1 else { return }
        // Arrivals within one main-actor turn reach the list as one batch of row reloads.
        Task {
            let refs = loadedRefs
            loadedRefs = []
            post(.storeContentLoaded, userInfo: ["refs": refs])
        }
    }

    /// Fetches the latest message of the top conversations in the background, for list previews.
    private func prefetchSnippets() {
        prefetchSnippets(for: Array(conversations.prefix(Self.prefetchCount)))
    }

    /// Fetches previews for `conversations` (rows the list is showing) in the background.
    func prefetchSnippets(for conversations: [Conversation]) {
        for conversation in conversations {
            let ref = conversation.latest.ref
            guard snippets[ref] == nil, prefetching.insert(ref).inserted else { continue }
            let task = loadTask(ref, priority: .background)
            Task {
                defer { prefetching.remove(ref) }
                guard let loaded = try? await task.value else { return }
                setSnippet(loaded.snippet, for: ref)
            }
        }
    }

    func attachmentURL(_ attachment: Attachment, of ref: MessageRef) -> URL {
        cache.attachmentURL(attachment, of: ref)
    }

    // MARK: - AI tagging

    /// Short user-facing progress or failure text; nil when idle.
    private(set) var aiTaggingStatus: String? {
        didSet { if aiTaggingStatus != oldValue { post(.storeTagsChanged) } }
    }

    /// The `aiTagging.enabled` setting; setting it writes settings.json, and the change applies
    /// through `settingsDidChange` like any other edit of the file.
    var aiTaggingEnabled: Bool {
        get { taggingEnabled }
        set {
            do {
                try Settings.shared.set(.aiTaggingEnabled, .bool(newValue))
            } catch {
                report(error, alert: true)
            }
        }
    }

    private func settingsDidChange(_ changed: Set<Setting>) {
        if !changed.isDisjoint(with: [.pollSeconds, .pushPollSeconds]) { schedulePolling() }
        if changed.contains(.aiTaggingEnabled) {
            applyTaggingEnabled(Settings.shared.bool(.aiTaggingEnabled))
        } else if !changed.isDisjoint(with: [.aiTaggingModel, .aiTaggingOmpPath, .aiTaggingPassLimit, .aiTaggingBackoffSeconds]) {
            // A fixed omp or model deserves another try at once.
            tagAttempted = []
            taggingBackoffUntil = nil
            startTaggingPass()
        }
    }

    private func applyTaggingEnabled(_ enabled: Bool) {
        guard enabled != taggingEnabled else { return }
        taggingEnabled = enabled
        tagAttempted = []
        taggingBackoffUntil = nil
        if enabled {
            startTaggingPass()
        } else {
            taggingDebounce?.cancel()
            taggingTask?.cancel()
            taggingRerun = false
            aiTaggingStatus = nil
        }
        post(.storeTagsChanged)
    }

    func tag(for conversation: Conversation) -> ConversationTag? {
        taggingEnabled ? tags[Self.tagKey(conversation)] : nil
    }

    /// Whether `conversation` can be resolved or put back on "Waiting on you": only people
    /// conversations the tagger has judged carry the flag.
    func canMarkWaiting(_ conversation: Conversation) -> Bool {
        tag(for: conversation)?.category == .people
    }

    /// Resolves the conversation (`waiting: false`) or puts it back on "Waiting on you". The flag
    /// belongs to the latest message, so a new incoming one is judged afresh.
    func setWaiting(_ conversation: Conversation, waiting: Bool) {
        let key = Self.tagKey(conversation)
        guard taggingEnabled, var tag = tags[key], tag.category == .people, tag.toRespond != waiting else { return }
        tag.toRespond = waiting
        tags[key] = tag
        cache.saveTags(tags)
        post(.storeTagsChanged)
    }

    /// Corrects the conversation's AI tag; nil leaves a part as it is. A conversation the tagger has
    /// not judged needs a category. Only people conversations can wait on a reply.
    func editTag(_ conversation: Conversation, category: MailCategory?, waiting: Bool?,
                 labels: [String]?) throws -> ConversationTag {
        guard taggingEnabled else { throw StoreError(message: "AI tagging is off (aiTagging.enabled).") }
        let key = Self.tagKey(conversation)
        guard var tag = tags[key] ?? category.map({ ConversationTag(category: $0, toRespond: false, labels: []) }) else {
            throw StoreError(message: "\(conversation.id) has no AI tag yet; give a category.")
        }
        if let category { tag.category = category }
        if let waiting { tag.toRespond = waiting }
        if tag.category != .people { tag.toRespond = false }
        if waiting == true, tag.category != .people {
            throw StoreError(message: "Only people conversations can wait on a reply.")
        }
        if let labels {
            tag.labels = Array(labels.map { String($0.trimmingCharacters(in: .whitespaces).prefix(OmpTagger.maxLabelLength)) }
                .filter { !$0.isEmpty }.prefix(2))
        }
        tags[key] = tag
        tagAttempted.insert(key)
        cache.saveTags(tags)
        post(.storeTagsChanged)
        return tag
    }

    /// Forgets every tag and tags the current list again, ignoring any failure backoff.
    func retagAll() {
        tags = [:]
        tagAttempted = []
        taggingBackoffUntil = nil
        cache.saveTags(tags)
        post(.storeTagsChanged)
        if let taggingTask {
            taggingTask.cancel()
            taggingRerun = true
        } else {
            startTaggingPass()
        }
    }

    /// The conversation's latest message, so a new reply re-tags it.
    nonisolated static func tagKey(_ conversation: Conversation) -> String {
        let latest = conversation.latest
        return latest.envelope.messageID ?? "\(latest.ref.account)/\(latest.ref.mailbox)/\(latest.ref.id)"
    }

    private func scheduleTagging() {
        guard taggingEnabled, taggingReady else { return }
        taggingDebounce?.cancel()
        taggingDebounce = Task {
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled else { return }
            startTaggingPass()
        }
    }

    /// Runs one pass now, or once after the pass in flight.
    private func startTaggingPass() {
        taggingDebounce?.cancel()
        guard taggingEnabled, taggingReady else { return }
        if taggingTask != nil {
            taggingRerun = true
            return
        }
        if let until = taggingBackoffUntil, until > Date() { return }
        let items = untaggedItems()
        guard !items.isEmpty else { return }
        taggingTask = Task {
            await runTagging(items)
            taggingTask = nil
            if taggingRerun {
                taggingRerun = false
                startTaggingPass()
            }
        }
    }

    /// Tags `items` in batches of `aiTagging.batchSize`, one omp process at a time.
    private func runTagging(_ items: [OmpTagger.Item]) async {
        let tagger = tagger
        let settings = Settings.shared
        let batchSize = settings.int(.aiTaggingBatchSize)
        let options = OmpTagger.Options(model: settings.string(.aiTaggingModel), executable: settings.string(.aiTaggingOmpPath))
        var remaining = items.count
        for start in stride(from: 0, to: items.count, by: batchSize) {
            let batch = Array(items[start..<min(start + batchSize, items.count)])
            aiTaggingStatus = "Tagging \(remaining) conversation\(remaining == 1 ? "" : "s")…"
            do {
                let result = try await tagger.tag(batch, options: options)
                try Task.checkCancellation()
                tags.merge(result) { _, new in new }
                tagAttempted.formUnion(batch.map(\.id))
                cache.saveTags(tags)
                post(.storeTagsChanged)
                remaining -= batch.count
            } catch {
                if Task.isCancelled || error is CancellationError {
                    aiTaggingStatus = nil
                } else {
                    aiTaggingStatus = "AI tagging failed: \(error.localizedDescription)"
                    taggingBackoffUntil = Date().addingTimeInterval(TimeInterval(settings.int(.aiTaggingBackoffSeconds)))
                }
                return
            }
        }
        aiTaggingStatus = nil
    }

    /// Newest untagged conversations whose primary messages are in an inbox.
    private func untaggedItems() -> [OmpTagger.Item] {
        let primary = Set(primaryKeys)
        let own = ownAddresses
        var keys = Set<String>()
        var items: [OmpTagger.Item] = []
        let limit = Settings.shared.int(.aiTaggingPassLimit)
        for conversation in conversations where items.count < limit {
            guard conversation.messages.contains(where: { $0.role == .inbox && primary.contains($0.ref.mailboxKey) })
            else { continue }
            let key = Self.tagKey(conversation)
            guard tags[key] == nil, !tagAttempted.contains(key), keys.insert(key).inserted else { continue }
            items.append(taggingItem(conversation, key: key, own: own))
        }
        return items
    }

    private func taggingItem(_ conversation: Conversation, key: String, own: Set<String>) -> OmpTagger.Item {
        let messages = conversation.messages
        let fromUser = messages.map { message in
            message.role == .sent || message.envelope.from.contains { own.contains($0.normalizedEmail) }
        }
        let firstIncoming = fromUser.firstIndex(of: false)
        let incoming = fromUser.lastIndex(of: false).map { messages[$0] } ?? conversation.latest
        let sender = incoming.envelope.from.first
        return OmpTagger.Item(
            id: key,
            senderName: sender?.displayName ?? "",
            senderEmail: sender?.email ?? "",
            userInTo: incoming.envelope.to.contains { own.contains($0.normalizedEmail) },
            subject: conversation.subject,
            snippet: String((snippet(for: incoming.ref) ?? "").prefix(OmpTagger.maxSnippet)),
            messageCount: messages.count,
            userReplied: firstIncoming.map { fromUser[($0 + 1)...].contains(true) } ?? false,
            latestFromUser: fromUser.last ?? false)
    }

    // MARK: - Mutations

    /// Applies at once; the returned task finishes when the provider confirmed (and throws after
    /// rolling back and reporting if it refused).
    @discardableResult
    func setRead(_ conversation: Conversation, read: Bool) -> Task<Void, Error> {
        let targets = read
            ? conversation.messages.filter { !$0.envelope.isSeen }
            : [conversation.messages.last(where: { $0.role != .sent }) ?? conversation.latest]
        guard !targets.isEmpty else { return Task {} }
        let task = mutateFlags(targets, flag: .seen, add: read)
        adjustUnread(targets, delta: read ? -1 : 1)
        return task
    }

    @discardableResult
    func setFlagged(_ conversation: Conversation, flagged: Bool) -> Task<Void, Error> {
        let targets = flagged ? [conversation.latest] : conversation.messages.filter(\.envelope.isFlagged)
        return mutateFlags(targets, flag: .flagged, add: flagged)
    }

    private func mutateFlags(_ targets: [MailMessage], flag: MessageFlag, add: Bool) -> Task<Void, Error> {
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
    private static func all(_ requests: [Task<Void, Error>]) -> Task<Void, Error> {
        Task {
            var failure: Error?
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
        post(.storeAccountsChanged)
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
        let primary = primary ?? Set(primaryKeys)
        return conversation.messages.filter {
            primary.contains($0.ref.mailboxKey) && ![.sent, .all, .archive, .trash, .drafts].contains($0.role)
        }
    }

    /// `primary` names the mailboxes the conversation was listed from, when not the screen's.
    @discardableResult
    func archive(_ conversation: Conversation, from primary: Set<MailboxKey>? = nil) -> Task<Void, Error> {
        guard let target = archiveMailbox(account: conversation.account) else {
            return Task { throw StoreError(message: "\(conversation.account) has no archive mailbox.") }
        }
        let targets = archivable(conversation, primary: primary)
        guard !targets.isEmpty else {
            return Task { throw StoreError(message: "Nothing in \(conversation.id) is in a mailbox Archive moves from.") }
        }
        return move(targets, to: target)
    }

    /// Moves the conversation to the trash; messages already there are deleted for good.
    @discardableResult
    func trash(_ conversation: Conversation) -> Task<Void, Error> {
        guard let trash = mailbox(account: conversation.account, role: .trash) else {
            let error = StoreError(message: "No trash mailbox found.")
            report(error, context: conversation.account, alert: true)
            return Task { throw error }
        }
        let inTrash = conversation.messages.filter { $0.ref.mailbox == trash.id }
        let elsewhere = conversation.messages.filter { $0.ref.mailbox != trash.id && $0.role != .drafts }
        var requests = [move(elsewhere, to: trash)]
        if !inTrash.isEmpty {
            removeEnvelopes(Set(inTrash.map(\.ref)))
            requests.append(Task {
                do {
                    try await backend(for: trash.account).delete(ids: inTrash.map(\.ref.id), mailbox: trash.id)
                } catch {
                    report(error, context: trash.account, alert: true)
                    refresh()
                    throw error
                }
            })
        }
        return Self.all(requests)
    }

    private func move(_ targets: [MailMessage], to destination: Mailbox) -> Task<Void, Error> {
        let targets = targets.filter { $0.ref.mailbox != destination.id }
        guard !targets.isEmpty else { return Task {} }
        removeEnvelopes(Set(targets.map(\.ref)))
        let requests = Dictionary(grouping: targets, by: \.ref.mailboxKey).map { key, messages in
            Task {
                do {
                    try await backend(for: key.account).move(ids: messages.map(\.ref.id), from: key.mailbox, to: destination.id)
                } catch {
                    report(error, context: key.account, alert: true)
                    refresh()
                    throw error
                }
            }
        }
        return Self.all(requests)
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

    /// Drops the messages from the visible list at once; a rebuild then reconciles threading.
    private func removeEnvelopes(_ refs: Set<MessageRef>) {
        for key in Set(refs.map(\.mailboxKey)) {
            let ids = Set(refs.filter { $0.mailboxKey == key }.map(\.id))
            listings[key]?.envelopes.removeAll { ids.contains($0.id) }
            searchResults[key]?.removeAll { ids.contains($0.id) }
            persist(key)
        }
        let primary = Set(primaryKeys)
        patchList(touching: refs) { conversation in
            let remaining = conversation.messages.filter { !refs.contains($0.ref) }
            guard remaining.contains(where: { primary.contains($0.ref.mailboxKey) }) else { return nil }
            return Conversation(id: conversation.id, account: conversation.account, messages: remaining)
        }
        setNeedsRebuild()
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
        if rebuildTask != nil { needsRebuild = true }
        guard changed else { return }
        conversations = next
        post(.storeListChanged)
    }

    // MARK: - Reporting

    private func post(_ name: Notification.Name, userInfo: [String: Any]? = nil) {
        NotificationCenter.default.post(name: name, object: self, userInfo: userInfo)
    }

    private func report(_ error: Error, context: String? = nil, alert: Bool = false) {
        if error is CancellationError { return }
        let text = context.map { "\($0): \(error.localizedDescription)" } ?? error.localizedDescription
        lastError = text
        post(.storeError, userInfo: ["message": text, "alert": alert])
    }
}

/// Everything a list rebuild reads, copied out of the store so threading and row formatting run
/// on the concurrent executor. Arrays are copy-on-write, so taking this snapshot copies nothing.
struct ListInput: Sendable {
    struct Source: Sendable {
        let key: MailboxKey
        let role: MailboxRole
        let envelopes: [Envelope]
    }

    let primary: [Source]
    let context: [Source]
    let own: Set<String>
    let recipients: Bool

    struct Output: Sendable {
        let conversations: [Conversation]
        let rows: [String: ConversationRow]
    }

    /// Nonisolated and async, so it never runs on the caller's actor.
    func build() async -> Output {
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
        let conversations = Threader.conversations(primary: messages(primary), context: messages(context))
        return Output(conversations: conversations,
                      rows: ConversationRow.rows(for: conversations, own: own, recipients: recipients))
    }
}
