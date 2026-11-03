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

/// Single source of truth for the UI, and the only main-actor part of the data layer.
///
/// The main actor only holds state and applies small changes: disk reads, network I/O, parsing,
/// threading and row formatting all run on the concurrent executor, and their results land here
/// as finished values. Lists paint from the disk cache before any request starts, then refresh
/// from the providers. Mutations apply optimistically to the visible list at once (no rethreading)
/// and roll back on failure.
///
/// Observable: the sidebar and window follow `accounts` and `mailboxes`, the list follows
/// `listContext`, `conversations`, `isLoading`, tags and `snippets`; failures go to `problems`.
@Observable
final class MailStore {
    nonisolated static let pageSize = 50
    static let prefetchCount = 40
    /// Polling while push is down, and as a safety net while it is up.
    static let pollInterval: Duration = .seconds(90)
    static let pushPollInterval: Duration = .seconds(15 * 60)
    static let countsInterval: Duration = .seconds(300)

    /// Signed-in accounts, their tokens and backends.
    let accountStore: AccountStore
    @ObservationIgnored private let tools: ToolEnvironment
    let cache = MailCache()
    @ObservationIgnored private(set) lazy var avatars = AvatarStore(accounts: accountStore, root: cache.root)
    /// Google Chat accounts, spaces and messages; selected through `Selection.chat`.
    @ObservationIgnored private(set) lazy var chat = ChatStore(cache: cache, accounts: accountStore)
    @ObservationIgnored private lazy var push = PushService(accounts: accountStore) { [weak self] id in
        self?.refresh(account: id)
    }

    /// Failures of every store, for status lines and alerts.
    var problems: StoreProblems { accountStore.problems }

    private(set) var accounts: [Account] = []
    private(set) var mailboxes: [String: [Mailbox]] = [:]
    private(set) var selection: Selection = .allInboxes
    private(set) var conversations: [Conversation] = []
    private(set) var listContext = ListContext(selection: .allInboxes, search: nil, role: .inbox)
    private(set) var isLoading = false
    private(set) var searchQuery: String?
    private(set) var lastRefresh: Date?

    private struct Listing {
        var envelopes: [Envelope]
        /// Cursor of the page after `envelopes`; nil at the end, or when the listing came from
        /// disk and no first page arrived yet.
        var next: String?
    }

    private var listings: [MailboxKey: Listing] = [:]
    /// Mailboxes whose disk cache was already read (hit or miss), so it is read at most once.
    @ObservationIgnored private var cacheRead = Set<MailboxKey>()
    @ObservationIgnored private var searchResults: [MailboxKey: [Envelope]] = [:]
    private var rows: [String: ConversationRow] = [:]
    /// Display-ready previews: quoted history trimmed, whitespace collapsed. Arrivals within one
    /// main-actor turn reach observers as one change.
    private(set) var snippets: [MessageRef: String] = [:]
    @ObservationIgnored private var contents: [MessageRef: LoadedContent] = [:]
    @ObservationIgnored private var contentOrder: [MessageRef] = []
    @ObservationIgnored private var contentTasks: [MessageRef: Task<LoadedContent, any Error>] = [:]
    @ObservationIgnored private var prefetching = Set<MessageRef>()
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

    @ObservationIgnored private lazy var tagger = OmpTagger(tools: tools, workingDirectory: cache.root)
    /// Keyed by `tagKey(_:)`, so a new message in a conversation re-tags it; read through `tag(for:)`.
    private(set) var tags: [String: ConversationTag] = [:]
    /// Keys the model was asked about but skipped; automatic passes leave them alone.
    @ObservationIgnored private var tagAttempted = Set<String>()
    private var taggingEnabled = true
    /// Identities are known, so "to me" and "from me" are meaningful.
    @ObservationIgnored private var taggingReady = false
    /// One tagging pass at a time; a request during a pass runs one more after it.
    @ObservationIgnored private lazy var tagging = CoalescedTask("AI tagging") { [weak self] in
        await self?.taggingPass()
    }
    @ObservationIgnored private var taggingDebounce: Task<Void, Never>?
    @ObservationIgnored private var taggingBackoffUntil: Date?

    init(accounts: AccountStore, tools: ToolEnvironment) {
        self.accountStore = accounts
        self.tools = tools
        taggingEnabled = UserDefaults.standard.object(forKey: Self.taggingDefaultsKey) as? Bool ?? true
    }

    isolated deinit {
        watchers.forEach { $0.cancel() }
        polling?.task.cancel()
    }

    // MARK: - Startup

    func start() {
        Task {
            await restoreFromCache()
            await connect()
        }
        chat.start()
        push.start()
        let accountStore = accountStore, push = push
        watchers = [
            Task.immediate(name: "Mail account changes") { [weak self] in
                for await _ in Observations({ accountStore.records }).dropFirst() { self?.accountsDidChange() }
            },
            Task.immediate(name: "Mail push connectivity") { [weak self] in
                for await _ in Observations({ push.isConnected }).dropFirst() { self?.pushConnectionChanged() }
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

    /// Polls every 90 s while push is down, every 15 min while it delivers changes.
    private func schedulePolling() {
        let interval = push.isConnected ? Self.pushPollInterval : Self.pollInterval
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
        rebuild.request()
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

    private var primaryKeys: [MailboxKey] {
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
        rebuild.request()
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

    private func rebuildPass() async {
        await loadCachedListings(primaryKeys + contextKeys)
        let context = ListContext(selection: selection, search: searchQuery, role: selectionRole)
        let input = ListInput(primary: sources(primaryKeys, primary: true), context: sources(contextKeys, primary: false),
                              own: ownAddresses, recipients: context.role.showsRecipients)
        let output = await input.build()
        // Inputs changed meanwhile: the next pass shows them.
        guard !rebuild.isPending else { return }
        conversations = output.conversations
        rows = output.rows
        listContext = context
        prefetchSnippets()
        scheduleTagging()
    }

    func refresh() {
        let keys = primaryKeys + contextKeys
        guard !keys.isEmpty else { return }
        refreshTask?.cancel()
        isLoading = true
        problems.clear(.mail)
        refreshTask = Task {
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
            rebuild.request()
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
                rebuild.request()
            }
            return
        }
        searchQuery = query
        searchResults = [:]
        isLoading = true
        rebuild.request()
        let jobs = backends(primaryKeys)
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            await withTaskGroup(of: (MailboxKey, Result<[Envelope], any Error>).self) { group in
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
                    rebuild.request()
                }
            }
            guard !Task.isCancelled else { return }
            isLoading = false
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
    nonisolated struct LoadedContent: Sendable {
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
    private func loadTask(_ ref: MessageRef, priority: RequestPriority) -> Task<LoadedContent, any Error> {
        if let task = contentTasks[ref] { return task }
        let backend = accountStore.backend(for: ref.account), cache = cache
        let task = Task(priority: priority == .interactive ? .userInitiated : .utility) {
            try await Self.load(ref, backend: backend, cache: cache, priority: priority)
        }
        contentTasks[ref] = task
        Task {
            _ = try? await task.value
            if contentTasks[ref] == task { contentTasks[ref] = nil }
        }
        return task
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
        if snippets[ref] != snippet { snippets[ref] = snippet }
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

    private static let taggingDefaultsKey = "AITaggingEnabled"
    static let taggingPassLimit = 150
    static let taggingBatchSize = 20
    static let taggingBackoff: TimeInterval = 300

    /// Short user-facing progress or failure text; nil when idle.
    private(set) var aiTaggingStatus: String?

    var aiTaggingEnabled: Bool {
        get { taggingEnabled }
        set {
            taggingEnabled = newValue
            UserDefaults.standard.set(newValue, forKey: Self.taggingDefaultsKey)
            tagAttempted = []
            taggingBackoffUntil = nil
            if newValue {
                startTaggingPass()
            } else {
                taggingDebounce?.cancel()
                tagging.cancel()
                aiTaggingStatus = nil
            }
        }
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
    }

    /// Forgets every tag and tags the current list again, ignoring any failure backoff.
    func retagAll() {
        tags = [:]
        tagAttempted = []
        taggingBackoffUntil = nil
        cache.saveTags(tags)
        taggingDebounce?.cancel()
        tagging.restart()
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
        tagging.request()
    }

    private func taggingPass() async {
        guard taggingEnabled, taggingReady else { return }
        if let until = taggingBackoffUntil, until > Date() { return }
        let items = untaggedItems()
        guard !items.isEmpty else { return }
        await runTagging(items)
    }

    /// Tags `items` in batches, one omp process at a time.
    private func runTagging(_ items: [OmpTagger.Item]) async {
        let tagger = tagger
        var remaining = items.count
        for start in stride(from: 0, to: items.count, by: Self.taggingBatchSize) {
            let batch = Array(items[start..<min(start + Self.taggingBatchSize, items.count)])
            aiTaggingStatus = "Tagging \(remaining) conversation\(remaining == 1 ? "" : "s")…"
            do {
                let result = try await tagger.tag(batch)
                try Task.checkCancellation()
                tags.merge(result) { _, new in new }
                tagAttempted.formUnion(batch.map(\.id))
                cache.saveTags(tags)
                remaining -= batch.count
            } catch {
                if Task.isCancelled || error is CancellationError {
                    aiTaggingStatus = nil
                } else {
                    aiTaggingStatus = "AI tagging failed: \(error.localizedDescription)"
                    taggingBackoffUntil = Date().addingTimeInterval(Self.taggingBackoff)
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
        for conversation in conversations where items.count < Self.taggingPassLimit {
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

    func setRead(_ conversation: Conversation, read: Bool) {
        let targets = read
            ? conversation.messages.filter { !$0.envelope.isSeen }
            : [conversation.messages.last(where: { $0.role != .sent }) ?? conversation.latest]
        guard !targets.isEmpty else { return }
        mutateFlags(targets, flag: .seen, add: read)
        adjustUnread(targets, delta: read ? -1 : 1)
    }

    func setFlagged(_ conversation: Conversation, flagged: Bool) {
        let targets = flagged ? [conversation.latest] : conversation.messages.filter(\.envelope.isFlagged)
        mutateFlags(targets, flag: .flagged, add: flagged)
    }

    private func mutateFlags(_ targets: [MailMessage], flag: MessageFlag, add: Bool) {
        let refs = Set(targets.map(\.ref))
        updateEnvelopes(refs) { envelope in
            if add { envelope.flags.insert(flag.rawValue) } else { envelope.flags.remove(flag.rawValue) }
        }
        for (key, messages) in Dictionary(grouping: targets, by: \.ref.mailboxKey) {
            let ids = messages.map(\.ref.id)
            Task {
                do {
                    try await backend(for: key.account).setFlag(flag, add, ids: ids, mailbox: key.mailbox)
                } catch {
                    updateEnvelopes(Set(messages.map(\.ref))) { envelope in
                        if add { envelope.flags.remove(flag.rawValue) } else { envelope.flags.insert(flag.rawValue) }
                    }
                    report(error, context: key.account, alert: true)
                }
            }
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

    private func archivable(_ conversation: Conversation) -> [MailMessage] {
        let primary = Set(primaryKeys)
        return conversation.messages.filter {
            primary.contains($0.ref.mailboxKey) && ![.sent, .all, .archive, .trash, .drafts].contains($0.role)
        }
    }

    func archive(_ conversation: Conversation) {
        guard let target = archiveMailbox(account: conversation.account) else { return }
        move(archivable(conversation), to: target)
    }

    /// Moves the conversation to the trash; messages already there are deleted for good.
    func trash(_ conversation: Conversation) {
        guard let trash = mailbox(account: conversation.account, role: .trash) else {
            report(StoreError(message: "No trash mailbox found."), context: conversation.account, alert: true)
            return
        }
        let inTrash = conversation.messages.filter { $0.ref.mailbox == trash.id }
        let elsewhere = conversation.messages.filter { $0.ref.mailbox != trash.id && $0.role != .drafts }
        move(elsewhere, to: trash)
        guard !inTrash.isEmpty else { return }
        removeEnvelopes(Set(inTrash.map(\.ref)))
        Task {
            do {
                try await backend(for: trash.account).delete(ids: inTrash.map(\.ref.id), mailbox: trash.id)
            } catch {
                report(error, context: trash.account, alert: true)
                refresh()
            }
        }
    }

    private func move(_ targets: [MailMessage], to destination: Mailbox) {
        let targets = targets.filter { $0.ref.mailbox != destination.id }
        guard !targets.isEmpty else { return }
        removeEnvelopes(Set(targets.map(\.ref)))
        for (key, messages) in Dictionary(grouping: targets, by: \.ref.mailboxKey) {
            Task {
                do {
                    try await backend(for: key.account).move(ids: messages.map(\.ref.id), from: key.mailbox, to: destination.id)
                } catch {
                    report(error, context: key.account, alert: true)
                    refresh()
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
        let conversations = Threader.conversations(primary: messages(primary), context: messages(context))
        return Output(conversations: conversations,
                      rows: ConversationRow.rows(for: conversations, own: own, recipients: recipients))
    }
}
