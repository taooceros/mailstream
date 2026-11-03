import AppKit

enum Selection: Hashable, Sendable {
    /// The same role across every account: All Inboxes, all Sent, …
    case unified(MailboxRole)
    case mailbox(MailboxKey)

    static let allInboxes = Selection.unified(.inbox)

    var isUnified: Bool {
        if case .unified = self { return true }
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

/// Single source of truth for the UI, and the only main-actor part of the data layer.
///
/// The main actor only holds state and applies small changes: disk reads, process I/O, parsing,
/// threading and row formatting all run on the concurrent executor, and their results land here
/// as finished values. Lists paint from the disk cache before any process starts, then refresh
/// from himalaya. Mutations apply optimistically to the visible list at once (no rethreading) and
/// roll back on failure.
@MainActor
final class MailStore {
    nonisolated static let pageSize = 50
    static let prefetchCount = 40

    let himalaya: Himalaya
    private let tools: ToolEnvironment
    let cache = MailCache()
    private(set) lazy var avatars = AvatarStore(tools: tools, root: cache.root)

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
        var pages: Int
        var reachedEnd: Bool
    }

    private var listings: [MailboxKey: Listing] = [:]
    /// Mailboxes whose disk cache was already read (hit or miss), so it is read at most once.
    private var cacheRead = Set<MailboxKey>()
    private var searchResults: [MailboxKey: [Envelope]] = [:]
    private var rows: [String: ConversationRow] = [:]
    /// Display-ready previews: quoted history trimmed, whitespace collapsed.
    private var snippets: [MessageRef: String] = [:]
    private var contents: [MessageRef: MessageContent] = [:]
    private var contentOrder: [MessageRef] = []
    private var contentTasks: [MessageRef: Task<LoadedContent, Error>] = [:]
    private var prefetching = Set<MessageRef>()
    private var loadedRefs = Set<MessageRef>()
    private var rebuildTask: Task<Void, Never>?
    private var needsRebuild = false
    private var refreshTask: Task<Void, Never>?
    private var searchTask: Task<Void, Never>?
    private var loadMoreTask: Task<Void, Never>?
    private var timers: [Timer] = []

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

    init(himalaya: Himalaya, tools: ToolEnvironment) {
        self.himalaya = himalaya
        self.tools = tools
        taggingEnabled = UserDefaults.standard.object(forKey: Self.taggingDefaultsKey) as? Bool ?? true
    }

    // MARK: - Startup

    func start() {
        Task {
            await restoreFromCache()
            await connect()
        }
        timers.append(Timer.scheduledTimer(withTimeInterval: 90, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { if self?.isLoading == false { self?.refresh() } }
        })
        timers.append(Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshCounts() }
        })
    }

    /// Paints the last known accounts, mailboxes, tags and lists, and starts fetching mail for
    /// them, before himalaya has listed a single account.
    private func restoreFromCache() async {
        let cache = cache
        async let savedTags = cache.loadTags()
        async let savedAccounts = cache.loadAccounts()
        if let saved = await savedTags { tags.merge(saved) { current, _ in current } }
        guard let saved = await savedAccounts, !saved.isEmpty else { return }
        accounts = saved
        await loadCachedMailboxes(saved.map(\.name))
        post(.storeAccountsChanged)
        setNeedsRebuild()
        refresh()
    }

    private func connect() async {
        do {
            let dtos = try await himalaya.accounts()
            let previous = Dictionary(accounts.map { ($0.name, $0) }, uniquingKeysWith: { a, _ in a })
            let names = dtos.map(\.name)
            let changed = names != accounts.map(\.name)
            accounts = dtos.map { dto in
                Account(name: dto.name, isDefault: dto.default, backends: dto.backends,
                        identity: previous[dto.name]?.identity,
                        kind: dto.backends.contains("msgraph") ? .outlook : previous[dto.name]?.kind ?? .generic)
            }
            if changed {
                mailboxes = mailboxes.filter { names.contains($0.key) }
                await loadCachedMailboxes(names.filter { mailboxes[$0] == nil })
                post(.storeAccountsChanged)
                setNeedsRebuild()
                refresh()
            }
            await withTaskGroup(of: Void.self) { group in
                for account in accounts { group.addTask { await self.loadAccount(account.name) } }
            }
            cache.saveAccounts(accounts)
            taggingReady = true
            refresh()
            scheduleTagging()
            refreshCounts()
        } catch {
            report(error)
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
        let himalaya = himalaya
        guard let account = accounts.first(where: { $0.name == name }) else { return }
        async let identity = try? himalaya.identity(account: name)
        do {
            let dtos = try await himalaya.mailboxes(account: name, counts: false)
            var attributes: [String: [String]] = [:]
            if account.backends.contains("imap") {
                for mailbox in (try? await himalaya.imapMailboxes(account: name)) ?? [] {
                    attributes[mailbox.name] = mailbox.attributes
                }
            }
            let previous = Dictionary((mailboxes[name] ?? []).map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
            let built = dtos.compactMap { dto -> Mailbox? in
                let attrs = attributes[dto.name] ?? []
                if attrs.contains(where: { $0.caseInsensitiveCompare("\\Noselect") == .orderedSame }) { return nil }
                return Mailbox(account: name, id: dto.id, name: dto.name,
                               role: Self.role(himalaya: dto.role, attributes: attrs, name: dto.name),
                               unread: dto.unread ?? previous[dto.id]?.unread,
                               total: dto.total ?? previous[dto.id]?.total)
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
        if let index = accounts.firstIndex(where: { $0.name == account }), accounts[index].kind != .outlook {
            accounts[index].kind = list.contains { $0.name.hasPrefix("[Gmail]") } ? .gmail : .generic
        }
    }

    static func role(himalaya role: String?, attributes: [String], name: String) -> MailboxRole {
        let candidates = [role?.lowercased()].compactMap { $0 } + attributes.map { $0.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "\\")) }
        for candidate in candidates {
            switch candidate {
            case "inbox": return .inbox
            case "sent": return .sent
            case "drafts", "draft": return .drafts
            case "trash": return .trash
            case "junk", "spam": return .junk
            case "archive": return .archive
            case "all": return .all
            case "flagged", "starred": return .starred
            case "important": return .important
            default: continue
            }
        }
        return name.caseInsensitiveCompare("INBOX") == .orderedSame ? .inbox : .other
    }

    func refreshCounts() {
        let himalaya = himalaya
        for account in accounts {
            Task {
                guard let dtos = try? await himalaya.mailboxes(account: account.name, counts: true) else { return }
                let counts = Dictionary(dtos.map { ($0.id, ($0.unread, $0.total)) }, uniquingKeysWith: { a, _ in a })
                guard var list = mailboxes[account.name] else { return }
                for index in list.indices {
                    if let (unread, total) = counts[list[index].id] {
                        list[index].unread = unread ?? list[index].unread
                        list[index].total = total ?? list[index].total
                    }
                }
                guard list != mailboxes[account.name] else { return }
                mailboxes[account.name] = list
                cache.saveMailboxes(list, for: account.name)
                post(.storeAccountsChanged)
            }
        }
    }

    // MARK: - Lookup

    func account(named name: String) -> Account? { accounts.first { $0.name == name } }

    func mailbox(_ key: MailboxKey) -> Mailbox? {
        mailboxes[key.account]?.first { $0.id == key.mailbox }
    }

    func mailbox(account: String, role: MailboxRole) -> Mailbox? {
        mailboxes[account]?.first { $0.role == role }
    }

    /// Lowercased addresses of every configured account, to render "me".
    var ownAddresses: Set<String> { Set(accounts.compactMap { $0.email?.lowercased() }) }

    /// `address` with the owning account's display name when the header carried none.
    func withIdentityName(_ address: Address) -> Address {
        guard address.displayName == address.email,
              let identity = accounts.first(where: { $0.email?.lowercased() == address.normalizedEmail })?.identity
        else { return address }
        return Address(name: identity.name, email: address.email)
    }

    var selectionTitle: String {
        switch selection {
        case .unified(let role): role == .inbox ? "All Inboxes" : role.title
        case .mailbox(let key): mailbox(key)?.displayName ?? key.mailbox
        }
    }

    var selectionRole: MailboxRole {
        switch selection {
        case .unified(let role): role
        case .mailbox(let key): mailbox(key)?.role ?? .other
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
        }
        return boxes.reduce(0) { $0 + (($1.role == .drafts ? $1.total : $1.unread) ?? 0) }
    }

    var canLoadMore: Bool {
        searchQuery == nil && primaryKeys.contains { !(listings[$0]?.reachedEnd ?? false) }
    }

    private var primaryKeys: [MailboxKey] {
        switch selection {
        case .unified(let role): unifiedMailboxes(role).map(\.key)
        case .mailbox(let key): [key]
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
        refresh()
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
                listings[key] = Listing(envelopes: cached, pages: 1, reachedEnd: cached.count < Self.pageSize)
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
        let himalaya = himalaya
        refreshTask = Task {
            await loadCachedListings(keys)
            await withTaskGroup(of: (MailboxKey, Result<[Envelope], Error>).self) { group in
                for key in keys {
                    group.addTask {
                        do {
                            let list = try await himalaya.envelopes(account: key.account, mailbox: key.mailbox,
                                                                    page: 1, pageSize: Self.pageSize)
                            return (key, .success(list))
                        } catch {
                            return (key, .failure(error))
                        }
                    }
                }
                for await (key, result) in group {
                    guard !Task.isCancelled else { continue }
                    switch result {
                    case .success(let list):
                        if merge(firstPage: list, into: key) { setNeedsRebuild() }
                    case .failure(let error): report(error, context: key.account)
                    }
                }
            }
            guard !Task.isCancelled else { return }
            isLoading = false
            lastRefresh = Date()
            post(.storeListChanged)
        }
    }

    /// Returns whether anything changed, so an unchanged mailbox costs no rebuild.
    private func merge(firstPage: [Envelope], into key: MailboxKey) -> Bool {
        let before = listings[key]
        var listing = before ?? Listing(envelopes: [], pages: 0, reachedEnd: false)
        if firstPage.count < Self.pageSize {
            listing = Listing(envelopes: firstPage, pages: 1, reachedEnd: true)
        } else {
            let oldest = firstPage.compactMap(\.date).min() ?? .distantPast
            let ids = Set(firstPage.map(\.id))
            let older = listing.envelopes.filter { !ids.contains($0.id) && ($0.date ?? .distantPast) < oldest }
            listing.envelopes = firstPage + older
            listing.pages = max(listing.pages, 1)
        }
        guard listing.envelopes != before?.envelopes || listing.reachedEnd != before?.reachedEnd else { return false }
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
        let keys = primaryKeys.filter { !(listings[$0]?.reachedEnd ?? false) }
        let himalaya = himalaya
        isLoading = true
        post(.storeListChanged)
        loadMoreTask = Task {
            defer { loadMoreTask = nil }
            for key in keys {
                let page = (listings[key]?.pages ?? 0) + 1
                do {
                    let list = try await himalaya.envelopes(account: key.account, mailbox: key.mailbox,
                                                            page: page, pageSize: Self.pageSize)
                    guard !Task.isCancelled, var listing = listings[key] else { return }
                    let known = Set(listing.envelopes.map(\.id))
                    listing.envelopes += list.filter { !known.contains($0.id) }
                    listing.pages = page
                    listing.reachedEnd = list.count < Self.pageSize
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
        let query = text.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "\"", with: "")
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
        let keys = primaryKeys
        let himalaya = himalaya
        let expression = ["subject", "from", "to", "body"].map { "\($0) \"\(query)\"" }.joined(separator: " or ")
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            await withTaskGroup(of: (MailboxKey, Result<[Envelope], Error>).self) { group in
                for key in keys {
                    group.addTask {
                        do {
                            return (key, .success(try await himalaya.search(account: key.account, mailbox: key.mailbox,
                                                                            query: expression, pageSize: Self.pageSize)))
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

    // MARK: - Content

    /// A message as the reader needs it, with its display preview, worked out off the main actor.
    private struct LoadedContent: Sendable {
        let content: MessageContent
        let snippet: String
    }

    /// Display-ready preview for the list, the reader's collapsed cards and the tagger.
    func snippet(for ref: MessageRef) -> String? { snippets[ref] }

    func loadedContent(for ref: MessageRef) -> MessageContent? { contents[ref] }

    func content(for ref: MessageRef, priority: RequestPriority = .interactive) async throws -> MessageContent {
        if let content = contents[ref] { return content }
        let task = contentTasks[ref] ?? loadTask(ref, priority: priority)
        let loaded = try await task.value
        remember(loaded, for: ref)
        return loaded.content
    }

    /// One shared load per message: disk cache first, else himalaya, parsed and cached off the main actor.
    private func loadTask(_ ref: MessageRef, priority: RequestPriority) -> Task<LoadedContent, Error> {
        if let task = contentTasks[ref] { return task }
        let himalaya = himalaya, cache = cache
        let task = Task.detached(priority: priority == .interactive ? .userInitiated : .utility) {
            let content: MessageContent
            if let cached = await cache.loadContent(ref) {
                content = cached
            } else {
                let data = try await himalaya.readMessage(ref, priority: priority)
                content = try MessageParser.parse(data, directory: cache.messageDirectory(ref), urlBase: cache.partURLBase(ref))
                cache.saveContent(content, for: ref)
            }
            return LoadedContent(content: content, snippet: Self.displaySnippet(content.snippet))
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
        contents[ref] = loaded.content
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

    private static let taggingDefaultsKey = "AITaggingEnabled"
    static let taggingPassLimit = 150
    static let taggingBatchSize = 20
    static let taggingBackoff: TimeInterval = 300

    /// Short user-facing progress or failure text; nil when idle.
    private(set) var aiTaggingStatus: String? {
        didSet { if aiTaggingStatus != oldValue { post(.storeTagsChanged) } }
    }

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
                taggingTask?.cancel()
                taggingRerun = false
                aiTaggingStatus = nil
            }
            post(.storeTagsChanged)
        }
    }

    func tag(for conversation: Conversation) -> ConversationTag? {
        taggingEnabled ? tags[Self.tagKey(conversation)] : nil
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
                post(.storeTagsChanged)
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
        mutateFlags(targets, flag: "seen", add: read)
        adjustUnread(targets, delta: read ? -1 : 1)
    }

    func setFlagged(_ conversation: Conversation, flagged: Bool) {
        let targets = flagged ? [conversation.latest] : conversation.messages.filter(\.envelope.isFlagged)
        mutateFlags(targets, flag: "flagged", add: flagged)
    }

    private func mutateFlags(_ targets: [MailMessage], flag: String, add: Bool) {
        let refs = Set(targets.map(\.ref))
        updateEnvelopes(refs) { envelope in
            if add { envelope.flags.insert(flag) } else { envelope.flags.remove(flag) }
        }
        let himalaya = himalaya
        for (key, messages) in Dictionary(grouping: targets, by: \.ref.mailboxKey) {
            let ids = messages.map(\.ref.id)
            Task {
                do {
                    if add {
                        try await himalaya.addFlags([flag], ids: ids, account: key.account, mailbox: key.mailbox)
                    } else {
                        try await himalaya.removeFlags([flag], ids: ids, account: key.account, mailbox: key.mailbox)
                    }
                } catch {
                    updateEnvelopes(Set(messages.map(\.ref))) { envelope in
                        if add { envelope.flags.remove(flag) } else { envelope.flags.insert(flag) }
                    }
                    report(error, context: key.account, alert: true)
                }
            }
        }
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
            report(HimalayaError(message: "No trash mailbox found."), context: conversation.account, alert: true)
            return
        }
        let inTrash = conversation.messages.filter { $0.ref.mailbox == trash.id }
        let elsewhere = conversation.messages.filter { $0.ref.mailbox != trash.id && $0.role != .drafts }
        move(elsewhere, to: trash)
        guard !inTrash.isEmpty else { return }
        removeEnvelopes(Set(inTrash.map(\.ref)))
        let himalaya = himalaya
        Task {
            do {
                try await himalaya.delete(ids: inTrash.map(\.ref.id), account: trash.account, mailbox: trash.id)
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
        let himalaya = himalaya
        for (key, messages) in Dictionary(grouping: targets, by: \.ref.mailboxKey) {
            Task {
                do {
                    try await himalaya.move(ids: messages.map(\.ref.id), account: key.account,
                                            from: key.mailbox, to: destination.id)
                } catch {
                    report(error, context: key.account, alert: true)
                    refresh()
                }
            }
        }
    }

    func send(_ fields: Himalaya.ComposeFields, mode: Himalaya.ComposeMode, account: String) async throws {
        try await himalaya.send(fields, mode: mode, account: account)
        await didSend(mode)
    }

    /// Marks the source answered and picks up the sent copy.
    func didSend(_ mode: Himalaya.ComposeMode) async {
        if case .reply(let ref) = mode {
            updateEnvelopes([ref]) { $0.flags.insert("answered") }
        }
        try? await Task.sleep(for: .seconds(1))
        refresh()
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
