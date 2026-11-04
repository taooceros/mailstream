import AppKit
import Observation
import SwiftUI

/// A command on one conversation, from the toolbar, the menus, the keyboard or a row's hover pill.
nonisolated enum ConversationAction: CaseIterable {
    case archive, trash, restore, toggleRead, toggleStar, toggleWaiting, reply, replyAll, forward

    /// What the toggles act on; without a conversation they read as read, unstarred and waiting.
    struct State {
        var unread = false
        var flagged = false
        var waiting = true
    }

    /// Title and SF Symbol in `state`: a toggle names what it will do.
    func label(in state: State = State()) -> (title: String, symbol: String) {
        switch self {
        case .archive: ("Archive", "archivebox")
        case .trash: ("Move to Trash", "trash")
        case .restore: ("Move to Inbox", "tray.and.arrow.up")
        case .toggleRead: state.unread ? ("Mark as Read", "envelope.open") : ("Mark as Unread", "envelope.badge")
        case .toggleStar: state.flagged ? ("Unstar", "star.slash") : ("Star", "star")
        case .toggleWaiting: state.waiting ? ("Resolve", "checkmark.circle") : ("Mark as Waiting on You", "clock.badge")
        case .reply: ("Reply", "arrowshape.turn.up.left")
        case .replyAll: ("Reply All", "arrowshape.turn.up.left.2")
        case .forward: ("Forward", "arrowshape.turn.up.right")
        }
    }
}

nonisolated extension ConversationAction.State {
    init(_ conversation: Conversation?, tag: ConversationTag?) {
        self.init(unread: conversation?.isUnread == true, flagged: conversation?.isFlagged == true,
                  waiting: tag?.toRespond ?? true)
    }
}

/// One line of the conversation list.
enum ConversationListItem: Identifiable {
    case section(String, count: Int?, accent: Bool)
    case shelf([ConversationListBundle])
    case conversation(Conversation)
    case space(ChatRow)

    var conversation: Conversation? {
        if case .conversation(let conversation) = self { return conversation }
        return nil
    }

    var space: ChatRow? {
        if case .space(let space) = self { return space }
        return nil
    }

    /// Identity of a line that can be highlighted and opened; nil for headers and the shelf.
    var selectionID: String? {
        switch self {
        case .conversation(let conversation): conversation.id
        case .space(let space): "h:" + space.id
        case .section, .shelf: nil
        }
    }

    /// Stable identity across rebuilds: the view keeps a line in place while its content changes.
    var id: String {
        switch self {
        case .section(let title, _, _): "s:" + title
        case .shelf: "shelf"
        case .conversation(let conversation): "c:" + conversation.id
        case .space(let space): "h:" + space.id
        }
    }

    var height: CGFloat {
        switch self {
        case .section: ListMetrics.sectionHeight
        case .shelf: ListMetrics.shelfHeight
        case .conversation, .space: ListMetrics.rowHeight
        }
    }
}

/// One tile of the Smart Inbox shelf.
struct ConversationListBundle: Identifiable {
    let category: MailCategory
    let members: [Conversation]
    var id: MailCategory { category }
}

/// Where the list's scroll view stands, in content coordinates.
struct ConversationListGeometry: Equatable {
    var top: CGFloat = 0
    var contentHeight: CGFloat = 0
    var containerHeight: CGFloat = 0
    var visibleMaxY: CGFloat { top + containerHeight }
}

/// What one line draws and how it stands, observed by that line alone: a snippet, picture, hover
/// or highlight reaching one line redraws that line, not every line in view.
@Observable
final class ListRowState {
    /// Nil for headers and the shelf, which draw from their item.
    fileprivate(set) var content: ListRowContent?
    fileprivate(set) var hovered = false
    fileprivate(set) var highlighted = false
    /// What `content` was made from; a refresh that finds the same keeps the line as it is.
    @ObservationIgnored fileprivate var inputs: ListRowInputs?

    fileprivate func setHovered(_ hovered: Bool) { if self.hovered != hovered { self.hovered = hovered } }
    fileprivate func setHighlighted(_ highlighted: Bool) {
        if self.highlighted != highlighted { self.highlighted = highlighted }
    }
}

/// Everything a line's content is made from.
private struct ListRowInputs: Equatable {
    enum Source: Equatable {
        case mail(ConversationRow, snippet: String?, tag: ConversationTag?, archivable: Bool)
        case chat(ChatRow)
    }

    var source: Source
    /// The account's label and color hex, in lists that mix accounts.
    var account: (name: String, hex: String)?
    var photo: NSImage?

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.source == rhs.source && lhs.account?.name == rhs.account?.name && lhs.account?.hex == rhs.account?.hex
            && lhs.photo === rhs.photo
    }

    var content: ListRowContent {
        let account = account.map { (name: $0.name, color: Color(nsColor: NSColor(hex: $0.hex))) }
        switch source {
        case .mail(let row, let snippet, let tag, let archivable):
            return ListRowContent(row, snippet: snippet, archivable: archivable, account: account, tag: tag, photo: photo)
        case .chat(let space):
            return ListRowContent(space, account: account, photo: photo)
        }
    }
}

/// A request for the view to bring a line into sight, pinned at `anchor`.
struct ConversationListReveal: Equatable {
    let id: String
    let anchor: UnitPoint
    let serial: Int
}

/// The open bundle's list as laid over the inbox: it stays shown while it slides or folds away
/// after the inbox stopped treating it as open.
@Observable
final class ConversationListBundleLayer: Identifiable {
    let model: ConversationListModel
    /// Sideways offset under a back swipe.
    var offset: CGFloat = 0
    /// The page shadow, shown while the layer moves with the fingers.
    var lifted = false
    var id: ObjectIdentifier { ObjectIdentifier(model) }

    init(model: ConversationListModel) { self.model = model }
}

/// Full-width list. Inboxes become a Smart Inbox: AI-tagged notifications, newsletters and
/// invitations sit on a shelf of tiles, mail waiting on the user's reply or action comes first
/// (bundled mail too, which stays in its bundle), the rest by date.
/// The Chat selection lists Google Chat spaces in the same rows, newest first, without mail actions.
@Observable
final class ConversationListModel {
    // MARK: Events (set by the shell)

    @ObservationIgnored var onHighlight: (Conversation?) -> Void = { _ in }
    @ObservationIgnored var onOpen: (Conversation) -> Void = { _ in }
    @ObservationIgnored var onOpenChat: (ChatSpaceKey) -> Void = { _ in }
    @ObservationIgnored var onPerform: (ConversationAction, Conversation) -> Void = { _, _ in }
    @ObservationIgnored var onBundleChange: () -> Void = {}

    // MARK: State

    private static let bundled: [MailCategory] = [.notification, .newsletter, .invitation]
    private static let waitingTitle = "Waiting on you"
    private static let autoFillLimit = 6
    /// Distance from the bottom at which the next page loads.
    private static let pagingMargin: CGFloat = 600

    @ObservationIgnored let store: MailStore
    /// Set on a bundle's own list, which lies over the inbox's list: it shows only that bundle.
    @ObservationIgnored let bundle: MailCategory?

    private(set) var items: [ConversationListItem] = []
    /// Top of each item in the stack, for mapping the pointer to a line.
    @ObservationIgnored private var tops: [CGFloat] = []
    /// Each item's line state by item id, made and refreshed with the items.
    @ObservationIgnored private var rowStates: [String: ListRowState] = [:]
    private(set) var highlightedID: String? {
        didSet {
            guard highlightedID != oldValue else { return }
            for item in items where item.selectionID != nil && (item.selectionID == oldValue || item.selectionID == highlightedID) {
                rowStates[item.id]?.setHighlighted(item.selectionID == highlightedID)
            }
        }
    }
    @ObservationIgnored private var lastHighlightedIndex = 0
    @ObservationIgnored private var lastContext: ListContext?
    @ObservationIgnored private var preloadTask: Task<Void, Never>?
    @ObservationIgnored private var autoFillPages = 0
    @ObservationIgnored private var storeWatches: [Task<Void, Never>] = []
    @ObservationIgnored private var memberCounts: (total: Int, unread: Int) = (0, 0)

    /// The line under the pointer, as the pointer and scroll position place it.
    private(set) var hoveredID: String? {
        didSet {
            guard hoveredID != oldValue else { return }
            if let oldValue { rowStates[oldValue]?.setHovered(false) }
            if let hoveredID { rowStates[hoveredID]?.setHovered(true) }
        }
    }
    @ObservationIgnored private var pointer: CGPoint?
    @ObservationIgnored private(set) var geometry = ConversationListGeometry()
    /// The lines `requestVisibleContent` last asked content for; nil after the items change.
    @ObservationIgnored private var requestedRange: Range<Int>?
    /// The user is scrolling: rows passing under the pointer don't flash.
    private(set) var isScrolling = false
    /// The window is key: hover follows the pointer only then.
    @ObservationIgnored var windowActive = true { didSet { if windowActive != oldValue { refreshHover() } } }
    /// The shell sets it while the reader covers the list.
    var hoverSuspended = false { didSet { if hoverSuspended != oldValue { refreshHover() } } }

    /// The view's keyboard focus, as it reports it.
    @ObservationIgnored var isFocused = false
    /// Bumped to make the view take keyboard focus.
    private(set) var focusRequest = 0
    private(set) var reveal: ConversationListReveal?
    @ObservationIgnored private var revealSerial = 0
    /// Bumped when the list shows another mailbox, search or chat: the view drifts the new content in.
    private(set) var contextGeneration = 0
    /// Width of the list's page, for the back swipe's settle.
    @ObservationIgnored var pageWidth: CGFloat = 0

    /// Search text in the Chat selection: filters spaces by title and preview, locally.
    var chatFilter: String? {
        didSet { if chatFilter != oldValue, isChat { listChanged() } }
    }

    /// The open bundle's list (commands, focus and titles go to it), nil at the top level.
    @ObservationIgnored private var bundleList: ConversationListModel?
    /// What lies over the inbox: the open bundle, or one still sliding or folding away.
    private(set) var bundleLayer: ConversationListBundleLayer?
    /// Bumped per back-swipe step, so a slide that a new drag interrupts does not clean up under it.
    @ObservationIgnored private var swipeStep = 0
    @ObservationIgnored private weak var parent: ConversationListModel?

    /// The Smart Inbox bundle being browsed, nil at the top level.
    var openBundle: MailCategory? { bundle ?? bundleList?.bundle }
    /// Conversations in the open bundle and how many are unread.
    var bundleCounts: (total: Int, unread: Int) { bundleList?.memberCounts ?? memberCounts }

    init(store: MailStore, bundle: MailCategory? = nil) {
        self.store = store
        self.bundle = bundle
        observeStore()
    }

    isolated deinit {
        storeWatches.forEach { $0.cancel() }
        preloadTask?.cancel()
    }

    /// Each change reaches the list once per main-actor turn, however many arrived in it.
    private func observeStore() {
        let store = store
        storeWatches = [
            // What the rows are built from: the published list, its tags and loading state, and the
            // chat spaces while the list shows them.
            Task.immediate(name: "List rows") { [weak self] in
                let built = Observations {
                    _ = (store.listContext, store.conversations, store.isLoading, store.tagging.isEnabled, store.tagging.tags)
                    if store.listContext.selection.isChat { _ = (store.chat.rows, store.chat.isLoading) }
                }
                for await _ in built { self?.listChanged() }
            },
            // What lines show beyond their items: previews, pictures, account names. Each line's
            // state takes only its own change.
            Task.immediate(name: "List row content") { [weak self] in
                let content = Observations {
                    _ = (store.contents.snippets, store.avatars.revision, store.accounts, store.chat.accounts)
                }
                for await _ in content { self?.refreshRowStates() }
            },
        ]
    }

    // MARK: - Building rows

    /// Decided by what the published list was built for, never the live selection.
    private var isSmart: Bool {
        store.listContext.search == nil && store.listContext.role == .inbox && store.tagging.isEnabled
    }

    /// The published list is Google Chat's.
    var isChat: Bool { store.listContext.selection.isChat }

    private func buildItems() -> [ConversationListItem] {
        let conversations = store.conversations
        if let bundle {
            // Left in another list, the inbox closes this page; until then it shows nothing.
            guard !isChat, isSmart else { return [] }
            let members = conversations.filter { store.tagging.tag(for: $0)?.category == bundle }
            memberCounts = (members.count, members.filter(\.isUnread).count)
            return dated(members)
        }
        if isChat { return chatItems() }
        guard isSmart else { return dated(conversations) }
        var buckets: [MailCategory: [Conversation]] = [:]
        var waiting: [Conversation] = []
        var people: [Conversation] = []
        for conversation in conversations {
            let tag = store.tagging.tag(for: conversation)
            let category = tag?.category ?? .people
            let bundled = Self.bundled.contains(category)
            if bundled { buckets[category, default: []].append(conversation) }
            if tag?.toRespond == true {
                waiting.append(conversation)
            } else if !bundled {
                people.append(conversation)
            }
        }
        var items: [ConversationListItem] = []
        let shelf = Self.bundled.compactMap { category in
            buckets[category].map { ConversationListBundle(category: category, members: $0) }
        }
        if !shelf.isEmpty { items.append(.shelf(shelf)) }
        if !waiting.isEmpty {
            items.append(.section(Self.waitingTitle, count: waiting.count, accent: true))
            items += waiting.map(ConversationListItem.conversation)
        }
        return items + dated(people)
    }

    private func dated(_ conversations: [Conversation]) -> [ConversationListItem] {
        let store = store
        return Self.sectioned(conversations, section: { store.row(for: $0).section }, item: ConversationListItem.conversation)
    }

    private func chatItems() -> [ConversationListItem] {
        guard case .chat(let account) = store.listContext.selection else { return [] }
        var spaces = store.chat.rows(account: account)
        if let filter = chatFilter?.trimmingCharacters(in: .whitespaces), !filter.isEmpty {
            spaces = spaces.filter { $0.title.localizedCaseInsensitiveContains(filter) || $0.preview.localizedCaseInsensitiveContains(filter) }
        }
        return Self.sectioned(spaces, section: \.section, item: ConversationListItem.space)
    }

    /// `elements` in order, under a header wherever their section changes.
    private static func sectioned<Element>(_ elements: [Element], section: (Element) -> String,
                                           item: (Element) -> ConversationListItem) -> [ConversationListItem] {
        var items: [ConversationListItem] = []
        items.reserveCapacity(elements.count + 8)
        var current: String?
        for element in elements {
            let title = section(element)
            if title != current {
                items.append(.section(title, count: nil, accent: false))
                current = title
            }
            items.append(item(element))
        }
        return items
    }

    // MARK: - Store updates

    private func listChanged() {
        var contextChanged = false
        if lastContext != store.listContext {
            contextChanged = lastContext != nil
            lastContext = store.listContext
            autoFillPages = 0
            highlightedID = nil
        }
        // Another mailbox, a search, chat or tagging turned off: no bundle to be in.
        if bundleList != nil, contextChanged || isChat || !isSmart { closeBundle() }
        let previousIndex = highlightedIndex ?? lastHighlightedIndex
        let hadCounts = memberCounts
        let newItems = buildItems()
        // Only this rebuild knows the bundle's counts, which the window's subtitle shows.
        if bundle != nil, hadCounts != memberCounts { onBundleChange() }
        apply(newItems, contextChanged: contextChanged)

        var notify = false
        if let id = highlightedID, !items.contains(where: { $0.selectionID == id }) {
            if let index = nearestSelectableIndex(to: previousIndex), store.listContext.search == nil {
                // The highlighted conversation left the list (archived, trashed): advance like Mail does.
                highlightedID = items[index].selectionID
                lastHighlightedIndex = index
            } else {
                highlightedID = nil
            }
            notify = true
        }
        if notify { onHighlight(highlightedConversation) }
        Task { [weak self] in self?.afterReload() }
    }

    /// Replaces the items. Small changes animate as lines sliding in and out; a new context drifts in.
    private func apply(_ newItems: [ConversationListItem], contextChanged: Bool) {
        let oldKeys = items.map(\.id)
        let newKeys = newItems.map(\.id)
        let animated = !contextChanged && !oldKeys.isEmpty && newKeys != oldKeys
            && newKeys.difference(from: oldKeys).count <= 24
        if animated {
            withAnimation(.house(.brief)) { setItems(newItems) }
        } else {
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) { setItems(newItems) }
        }
        if contextChanged { contextGeneration += 1 }
        refreshHover()
    }

    private func setItems(_ newItems: [ConversationListItem]) {
        items = newItems
        var top: CGFloat = 0
        tops = newItems.map { item in
            defer { top += item.height }
            return top
        }
        requestedRange = nil
        var states: [String: ListRowState] = [:]
        states.reserveCapacity(newItems.count)
        for item in newItems {
            let state = rowStates[item.id] ?? ListRowState()
            refresh(state, for: item)
            state.setHovered(item.id == hoveredID)
            state.setHighlighted(item.selectionID != nil && item.selectionID == highlightedID)
            states[item.id] = state
        }
        rowStates = states
    }

    /// The state `item`'s line draws from.
    func rowState(for item: ConversationListItem) -> ListRowState {
        if let state = rowStates[item.id] { return state }
        // Not reached: `setItems` makes every item's state with the items.
        let state = ListRowState()
        rowStates[item.id] = state
        return state
    }

    /// Previews, pictures or account names changed: lines whose inputs moved make new content.
    private func refreshRowStates() {
        for item in items {
            if let state = rowStates[item.id] { refresh(state, for: item) }
        }
    }

    private func refresh(_ state: ListRowState, for item: ConversationListItem) {
        guard let inputs = rowInputs(for: item), inputs != state.inputs else { return }
        state.inputs = inputs
        state.content = inputs.content
    }

    /// What `item`'s line shows, from the store; nil for headers and the shelf. Pictures only as
    /// far as they are in memory: looking for missing ones is left to the lines in view.
    private func rowInputs(for item: ConversationListItem) -> ListRowInputs? {
        let context = store.listContext
        switch item {
        case .section, .shelf:
            return nil
        case .conversation(let conversation):
            let row = store.row(for: conversation)
            let account = context.selection.isUnified
                ? (store.label(account: conversation.account), AccountStyle.hex(for: conversation.account, in: store)) : nil
            return ListRowInputs(
                source: .mail(row, snippet: store.contents.snippet(for: conversation.latest.ref),
                              tag: store.tagging.tag(for: conversation),
                              archivable: ![.archive, .all, .trash].contains(context.role)),
                account: account, photo: store.avatars.cachedImage(for: row.avatarKey))
        case .space(let space):
            let account = context.selection == .allChats
                ? (store.label(account: space.key.account), AccountStyle.chatHex(for: space.key.account, in: store)) : nil
            return ListRowInputs(source: .chat(space), account: account,
                                 photo: space.avatarKey.isEmpty ? nil : store.avatars.cachedImage(for: space.avatarKey))
        }
    }

    /// What the list shows when it has no lines.
    var emptyText: String? {
        guard items.isEmpty else { return nil }
        if isChat {
            return store.chat.isLoading ? "Loading…" : (chatFilter?.isEmpty == false ? "No Results" : "No Chats")
        }
        return store.isLoading ? "Loading…" : (store.listContext.search != nil ? "No Results" : "No Conversations")
    }

    /// What the floating status capsule says; nil hides it. It appears only while something is
    /// happening or went wrong. Busy lines spin; the others are failures.
    var statusLine: (text: String, busy: Bool)? {
        let problems = store.problems.current
        if isChat {
            if store.chat.isLoading { return ("Checking for messages…", true) }
            return problems[.chat].map { ($0.message, false) }
        }
        if store.isLoading { return (store.searchQuery != nil ? "Searching…" : "Checking for mail…", true) }
        if let problem = problems[.mail] { return (problem.message, false) }
        return store.tagging.status.map { ($0, true) }
    }

    private func afterReload() {
        requestVisibleContent()
        autoFill()
    }

    /// Fills a viewport the rows don't reach the bottom of, a few pages at most without the user scrolling.
    private func autoFill() {
        guard canLoadMore, !isLoadingList, autoFillPages < Self.autoFillLimit, geometry.containerHeight > 0 else { return }
        if geometry.contentHeight < geometry.containerHeight + Self.pagingMargin {
            autoFillPages += 1
            loadMore()
        }
    }

    /// Paging of what the list shows: mail pages, or a larger chat spaces limit.
    private var canLoadMore: Bool {
        guard isChat else { return store.canLoadMore }
        guard case .chat(let account) = store.listContext.selection, chatFilter?.isEmpty ?? true else { return false }
        return store.chat.canLoadMore(account: account)
    }

    private var isLoadingList: Bool { isChat ? store.chat.isLoading : store.isLoading }

    private func loadMore() {
        if case .chat(let account) = store.listContext.selection {
            store.chat.loadMore(account: account)
        } else {
            store.loadMore()
        }
    }

    /// Starts loading previews and pictures for the lines in view and the next few, once per
    /// stretch of lines.
    private func requestVisibleContent() {
        let range = visibleRange
        guard !range.isEmpty, range != requestedRange else { return }
        requestedRange = range
        let lines = items[range.lowerBound..<min(items.count, range.upperBound + 10)]
        store.contents.prefetchSnippets(for: lines.compactMap(\.conversation))
        for item in lines {
            guard let state = rowStates[item.id], let inputs = state.inputs, inputs.photo == nil else { continue }
            let found: NSImage? = switch inputs.source {
            case .mail(let row, _, let tag, _):
                store.avatars.image(for: row.avatarKey, automated: tag.map { $0.category != .people } ?? false)
            case .chat(let space):
                space.avatarKey.isEmpty ? nil : store.avatars.image(for: space.avatarKey, automated: false)
            }
            if found != nil { refresh(state, for: item) }
        }
    }

    /// Items at least partly in view.
    private var visibleRange: Range<Int> {
        guard !items.isEmpty, geometry.containerHeight > 0 else { return 0..<0 }
        let top = geometry.top - ConversationListView.inset
        let first = index(at: max(top, 0)) ?? 0
        let last = index(at: top + geometry.containerHeight) ?? first
        return first..<max(first, last) + 1
    }

    /// The item whose span holds `y` (stack coordinates): the last one starting at or above it.
    private func index(at y: CGFloat) -> Int? {
        var low = 0, high = tops.count
        while low < high {
            let mid = (low + high) / 2
            if tops[mid] <= y { low = mid + 1 } else { high = mid }
        }
        return low == 0 ? nil : low - 1
    }

    private func nearestSelectableIndex(to index: Int) -> Int? {
        guard !items.isEmpty else { return nil }
        let start = min(max(index, 0), items.count - 1)
        for index in start..<items.count where items[index].selectionID != nil { return index }
        for index in stride(from: start, through: 0, by: -1) where items[index].selectionID != nil { return index }
        return nil
    }

    // MARK: - Scrolling (reported by the view)

    /// The scroll view moved or its content changed size.
    func geometryChanged(_ new: ConversationListGeometry) {
        let old = geometry
        geometry = new
        refreshHover()
        requestVisibleContent()
        if new.top != old.top {
            // Scrolled: page in before the bottom comes into view.
            if canLoadMore, !isLoadingList, new.visibleMaxY > new.contentHeight - Self.pagingMargin { loadMore() }
        } else if new.contentHeight != old.contentHeight || new.containerHeight != old.containerHeight {
            autoFill()
        }
    }

    /// The user started (true) or stopped (false) scrolling.
    func scrollingChanged(_ scrolling: Bool) {
        guard scrolling != isScrolling else { return }
        if scrolling { autoFillPages = 0 }
        isScrolling = scrolling
        refreshHover()
    }

    // MARK: - Hover

    /// The pointer in the scroll view's own coordinates, nil once it left.
    func pointerMoved(_ point: CGPoint?) {
        pointer = point
        refreshHover()
    }

    /// Re-derives the hovered line from where the pointer is and how far the list is scrolled, so
    /// content moving under a still pointer never strands a hover.
    private func refreshHover() {
        var under: String?
        if !hoverSuspended, !isScrolling, windowActive, bundleList == nil, let pointer {
            let y = geometry.top + pointer.y - ConversationListView.inset
            if y >= 0, let index = index(at: y), y < tops[index] + items[index].height {
                under = items[index].id
            }
        }
        guard under != hoveredID else { return }
        hoveredID = under
        preload(under.flatMap { id in items.firstIndex { $0.id == id } })
    }

    // MARK: - Bundles

    /// Opens `category` as its own list laid over this one, spreading out from the middle as the
    /// reader does. The inbox beneath keeps its rows, scroll position and updates.
    func enterBundle(_ category: MailCategory) {
        guard bundle == nil, bundleList == nil else { return }
        let list = ConversationListModel(store: store, bundle: category)
        list.parent = self
        list.wireToParent()
        list.pageWidth = pageWidth
        bundleList = list
        // The inbox's highlight is not what toolbar commands act on while the bundle is open.
        let hadHighlight = highlightedID != nil
        highlightedID = nil
        refreshHover()
        if isFocused { list.focus() }
        bundleLayer = ConversationListBundleLayer(model: list)
        if hadHighlight { onHighlight(nil) }
        onBundleChange()
    }

    /// A bundle's events go to the inbox's handlers, as long as it is the open one.
    private func wireToParent() {
        onHighlight = { [weak self] conversation in
            guard let self, let parent = self.parent, parent.bundleList === self else { return }
            parent.onHighlight(conversation)
        }
        onOpen = { [weak self] conversation in
            guard let self, let parent = self.parent, parent.bundleList === self else { return }
            parent.onOpen(conversation)
        }
        onOpenChat = { [weak self] key in
            guard let self, let parent = self.parent, parent.bundleList === self else { return }
            parent.onOpenChat(key)
        }
        onPerform = { [weak self] action, conversation in
            guard let self, let parent = self.parent, parent.bundleList === self else { return }
            parent.onPerform(action, conversation)
        }
        onBundleChange = { [weak self] in
            guard let self, let parent = self.parent, parent.bundleList === self else { return }
            parent.onBundleChange()
        }
    }

    /// Back to the inbox: the bundle shrinks back toward the middle and fades away.
    func leaveBundle() {
        guard detachBundle() != nil else { return }
        bundleLayer?.lifted = false
        withAnimation(.house(.brief)) { bundleLayer = nil }
    }

    /// The bundle at once, without animation (the list beneath is changing to something else).
    private func closeBundle() {
        guard detachBundle() != nil else { return }
        removeLayer()
    }

    private func removeLayer() {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) { bundleLayer = nil }
        refreshHover()
    }

    /// The bundle stops being the open one: commands, focus and the title go back to the inbox. Its
    /// layer stays on screen for the caller to take away.
    private func detachBundle() -> ConversationListModel? {
        guard let list = bundleList else { return nil }
        let wasFocused = list.isFocused
        bundleList = nil
        swipeStep += 1
        if wasFocused { focus() }
        refreshHover()
        onHighlight(highlightedConversation)
        onBundleChange()
        return list
    }

    /// A back swipe in a bundle: its list slides with the fingers like a page, over the inbox.
    func dragBack(to offset: CGFloat) {
        guard bundleList != nil, let layer = bundleLayer else { return }
        swipeStep += 1
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            layer.lifted = true
            layer.offset = offset
        }
    }

    /// Fingers lifted: the bundle slides off and the inbox, already there beneath it, stays, or it
    /// slides back (see `BackSwipe.settle`). The slide starts at the fingers' speed.
    func releaseBack(at offset: CGFloat, velocity: CGFloat) {
        guard bundleList != nil, let layer = bundleLayer, layer.lifted else { return }
        let (back, target) = BackSwipe.settle(offset: offset, velocity: velocity, width: pageWidth, canGoBack: true)
        if back { _ = detachBundle() }
        let step = swipeStep
        // In units of the whole change per second; speed away from the target is dropped.
        let initialVelocity = target != offset ? max(0, velocity / (target - offset)) : 0
        let spring = Animation.interpolatingSpring(mass: 1, stiffness: 440, damping: 42, initialVelocity: initialVelocity)
        withAnimation(spring, completionCriteria: .logicallyComplete) {
            layer.offset = target
        } completion: { [weak self] in
            guard let self else { return }
            if back {
                if self.bundleLayer === layer { self.removeLayer() }
            } else if self.swipeStep == step {
                layer.lifted = false
            }
        }
    }

    // MARK: - Highlight and opening

    private var highlightedIndex: Int? {
        guard let highlightedID else { return nil }
        return items.firstIndex { $0.selectionID == highlightedID }
    }

    /// The highlighted conversation; nil for a chat space or nothing.
    var highlightedConversation: Conversation? {
        highlightedIndex.flatMap { items[$0].conversation }
    }

    /// Moves the keyboard highlight to `conversation` without opening it (used after Back), in the
    /// open bundle's list when there is one.
    func highlight(_ conversation: Conversation) {
        if let bundleList { return bundleList.highlight(conversation) }
        guard let index = items.firstIndex(where: { $0.conversation?.id == conversation.id }) else { return }
        highlightIndex(index, notify: false)
    }

    /// Moves the keyboard highlight to a chat space's row (used after Back).
    func highlight(space key: ChatSpaceKey) {
        let id = "h:" + key.id
        guard let index = items.firstIndex(where: { $0.selectionID == id }) else { return }
        highlightIndex(index, notify: false)
    }

    /// Makes the list the keyboard target: the open bundle's list when there is one.
    func focus() {
        if let bundleList { return bundleList.focus() }
        focusRequest += 1
    }

    /// Highlights `index` and keeps it in sight; the user's highlight notifies and preloads.
    private func highlightIndex(_ index: Int, notify: Bool) {
        guard let id = items[index].selectionID else { return }
        let changed = id != highlightedID
        highlightedID = id
        lastHighlightedIndex = index
        revealItem(at: index)
        guard notify, changed else { return }
        onHighlight(highlightedConversation)
        preload(index)
    }

    /// Scrolls `index` into view the least distance, unless it is already in view.
    private func revealItem(at index: Int) {
        let id = items[index].id
        let top = geometry.top - ConversationListView.inset
        let line = tops[index]..<tops[index] + items[index].height
        if geometry.containerHeight > 0, line.lowerBound >= top, line.upperBound <= top + geometry.containerHeight { return }
        let anchor: UnitPoint = geometry.containerHeight == 0 || line.lowerBound < top ? .top : .bottom
        revealSerial += 1
        reveal = ConversationListReveal(id: id, anchor: anchor, serial: revealSerial)
    }

    /// A click on a line: highlights it as the user's, then opens it.
    func click(_ id: String) {
        guard let index = items.firstIndex(where: { $0.id == id }), items[index].selectionID != nil else { return }
        highlightIndex(index, notify: true)
        open(index)
    }

    /// Arrow keys: the next or previous line that can be highlighted.
    func moveHighlight(by step: Int) {
        let selectable = items.indices.filter { items[$0].selectionID != nil }
        guard !selectable.isEmpty else { return }
        let target: Int
        if let current = highlightedIndex {
            let next = step > 0 ? selectable.first { $0 > current } : selectable.last { $0 < current }
            guard let next else { return revealItem(at: current) }
            target = next
        } else {
            target = step > 0 ? selectable[0] : selectable[selectable.count - 1]
        }
        highlightIndex(target, notify: true)
    }

    /// Return, Enter or Space: opens the highlighted line.
    func openHighlighted() {
        guard let index = highlightedIndex else { return }
        open(index)
    }

    /// The single-key commands: act on the highlighted conversation.
    func performOnHighlight(_ action: ConversationAction) {
        guard let conversation = highlightedConversation else { return }
        onPerform(action, conversation)
    }

    private func open(_ index: Int) {
        switch items[index] {
        case .conversation(let conversation): onOpen(conversation)
        case .space(let space): onOpenChat(space.key)
        case .section, .shelf: break
        }
    }

    /// Loads the conversation at `index` once the pointer or highlight rests on it, so opening it
    /// finds its bodies in memory; passing over rows loads nothing.
    private func preload(_ index: Int?) {
        preloadTask?.cancel()
        guard let index, items.indices.contains(index) else { return }
        if let space = items[index].space {
            store.chat.preload(space.key)
            return
        }
        guard let conversation = items[index].conversation else { return }
        preloadTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(80))
            guard !Task.isCancelled, let self else { return }
            store.contents.preload(conversation)
        }
    }

    /// A row's hover pill or context menu: acts on the conversation the list holds under `id` now.
    func perform(_ action: ConversationAction, id: String) {
        guard let conversation = items.lazy.compactMap(\.conversation).first(where: { $0.id == id }) else { return }
        onPerform(action, conversation)
    }

    func openChat(_ key: ChatSpaceKey) {
        guard let index = items.firstIndex(where: { $0.space?.key == key }) else { return }
        open(index)
    }

    func markChatRead(_ key: ChatSpaceKey) {
        guard items.contains(where: { $0.space?.key == key }) else { return }
        store.chat.markRead(key)
    }
}
