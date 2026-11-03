import AppKit

enum ConversationAction {
    case archive, trash, toggleRead, toggleStar, toggleWaiting, reply, replyAll, forward
}

@MainActor
protocol ConversationListDelegate: AnyObject {
    /// The keyboard highlight moved; toolbar commands apply to this conversation.
    func conversationList(_ list: ConversationListViewController, didHighlight conversation: Conversation?)
    func conversationList(_ list: ConversationListViewController, open conversation: Conversation)
    func conversationList(_ list: ConversationListViewController, openChat space: ChatSpaceKey)
    func conversationList(_ list: ConversationListViewController, perform action: ConversationAction, on conversation: Conversation)
    /// Entered or left a Smart Inbox bundle.
    func conversationListDidChangeBundle(_ list: ConversationListViewController)
}

/// Full-width list. Inboxes become a Smart Inbox: AI-tagged notifications, newsletters and
/// invitations sit on a shelf of tiles, people waiting on a reply come first, the rest by date.
/// The Chat selection lists Google Chat spaces in the same rows, newest first, without mail actions.
/// Store updates are diffed so rows slide in and out instead of the list jumping.
@MainActor
final class ConversationListViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    weak var delegate: ConversationListDelegate?

    private enum Row {
        case section(String, count: Int?, accent: Bool)
        case shelf([(MailCategory, [Conversation])])
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

        /// Identity of a row that can be highlighted and opened; nil for headers and the shelf.
        var id: String? {
            switch self {
            case .conversation(let conversation): conversation.id
            case .space(let space): "h:" + space.id
            case .section, .shelf: nil
            }
        }

        /// Stable identity across rebuilds, for diffing.
        var key: String {
            switch self {
            case .section(let title, _, _): "s:" + title
            case .shelf: "shelf"
            case .conversation(let conversation): "c:" + conversation.id
            case .space(let space): "h:" + space.id
            }
        }
    }

    private static let bundled: [MailCategory] = [.notification, .newsletter, .invitation]
    private static let waitingTitle = "Waiting on you"

    private let store: MailStore
    private let table = ConversationTableView()
    private let scroll = NSScrollView()
    private let emptyLabel = NSTextField(labelWithString: "")
    private let status = StatusPill()
    private var rows: [Row] = []
    private var signatures: [Int] = []
    private var rowByRef: [MessageRef: Int] = [:]
    private var highlightedID: String?
    private var lastHighlightedRow = 0
    private var suppressSelection = false
    private var lastContext: ListContext?
    /// Waits out the pointer's or keyboard highlight's dwell before preloading a conversation.
    private var preloadTask: Task<Void, Never>?
    /// Account order the visible cells' account bars were colored with.
    private var accountNames: [String] = []
    private var savedScroll: [String: CGFloat] = [:]
    /// Pages fetched without the user scrolling, to fill a Smart Inbox whose rows were mostly bundled.
    private var autoFillPages = 0
    private static let autoFillLimit = 6

    /// The Smart Inbox bundle being browsed, nil at the top level.
    private(set) var openBundle: MailCategory?
    /// Conversations in the open bundle and how many are unread.
    private(set) var bundleCounts: (total: Int, unread: Int) = (0, 0)

    init(store: MailStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    var tableView: NSTableView { table }

    override func loadView() {
        let container = NSView()
        container.wantsLayer = true

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("conversation"))
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .inset
        table.usesAutomaticRowHeights = false
        table.intercellSpacing = NSSize(width: 0, height: 0)
        table.backgroundColor = .clear
        table.floatsGroupRows = false
        table.dataSource = self
        table.delegate = self
        table.allowsEmptySelection = true
        table.allowsMultipleSelection = false
        table.target = self
        table.action = #selector(rowClicked)
        table.onKey = { [weak self] action in self?.performOnHighlight(action) }
        table.onOpen = { [weak self] in self?.openHighlighted() }
        table.onHoverRow = { [weak self] row in self?.preload(row) }
        let menu = NSMenu()
        menu.delegate = self
        table.menu = menu

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.wantsLayer = true
        scroll.contentView.postsBoundsChangedNotifications = true

        emptyLabel.font = .systemFont(ofSize: 15, weight: .medium)
        emptyLabel.textColor = .tertiaryLabelColor
        emptyLabel.alignment = .center

        for view in [scroll, emptyLabel, status] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(view)
        }
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: container.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            status.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            status.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -14),
            status.widthAnchor.constraint(lessThanOrEqualTo: container.widthAnchor, constant: -40),
            emptyLabel.centerXAnchor.constraint(equalTo: scroll.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
        ])
        view = container

        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(listChanged), name: .storeListChanged, object: store)
        center.addObserver(self, selector: #selector(listChanged), name: .storeTagsChanged, object: store)
        center.addObserver(self, selector: #selector(contentLoaded(_:)), name: .storeContentLoaded, object: store)
        center.addObserver(self, selector: #selector(accountsChanged), name: .storeAccountsChanged, object: store)
        center.addObserver(self, selector: #selector(storeError), name: .storeError, object: store)
        center.addObserver(self, selector: #selector(avatarResolved(_:)), name: .avatarResolved, object: store.avatars)
        center.addObserver(self, selector: #selector(chatChanged), name: .chatSpacesChanged, object: store.chat)
        center.addObserver(self, selector: #selector(storeError), name: .storeError, object: store.chat)
        center.addObserver(self, selector: #selector(scrolled), name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        center.addObserver(self, selector: #selector(userScrolled), name: NSScrollView.willStartLiveScrollNotification, object: scroll)
        center.addObserver(self, selector: #selector(scrollEnded), name: NSScrollView.didEndLiveScrollNotification, object: scroll)
        listChanged()
    }

    // MARK: - Building rows

    /// Decided by what the published list was built for, never the live selection.
    private var isSmart: Bool {
        store.listContext.search == nil && store.listContext.role == .inbox && store.aiTaggingEnabled
    }

    /// The published list is Google Chat's.
    private var isChat: Bool { store.listContext.selection.isChat }

    /// Search text in the Chat selection: filters spaces by title and preview, locally.
    var chatFilter: String? {
        didSet { if chatFilter != oldValue, isChat { listChanged() } }
    }

    private func buildRows() -> [Row] {
        if isChat {
            openBundle = nil
            return chatRows()
        }
        let conversations = store.conversations
        guard isSmart else {
            openBundle = nil
            return dated(conversations)
        }
        var buckets: [MailCategory: [Conversation]] = [:]
        var waiting: [Conversation] = []
        var people: [Conversation] = []
        for conversation in conversations {
            let tag = store.tag(for: conversation)
            if let category = tag?.category, Self.bundled.contains(category) {
                buckets[category, default: []].append(conversation)
            } else if tag?.toRespond == true {
                waiting.append(conversation)
            } else {
                people.append(conversation)
            }
        }
        if let openBundle {
            let members = buckets[openBundle] ?? []
            bundleCounts = (members.count, members.filter(\.isUnread).count)
            return dated(members)
        }
        var rows: [Row] = []
        let shelf = Self.bundled.compactMap { category in buckets[category].map { (category, $0) } }
        if !shelf.isEmpty { rows.append(.shelf(shelf)) }
        if !waiting.isEmpty {
            rows.append(.section(Self.waitingTitle, count: waiting.count, accent: true))
            rows += waiting.map(Row.conversation)
        }
        return rows + dated(people)
    }

    private func dated(_ conversations: [Conversation]) -> [Row] {
        var rows: [Row] = []
        rows.reserveCapacity(conversations.count + 8)
        var current: String?
        for conversation in conversations {
            let title = store.row(for: conversation).section
            if title != current {
                rows.append(.section(title, count: nil, accent: false))
                current = title
            }
            rows.append(.conversation(conversation))
        }
        return rows
    }

    private func chatRows() -> [Row] {
        guard case .chat(let account) = store.listContext.selection else { return [] }
        var spaces = store.chat.rows(account: account)
        if let filter = chatFilter?.trimmingCharacters(in: .whitespaces), !filter.isEmpty {
            spaces = spaces.filter { $0.title.localizedCaseInsensitiveContains(filter) || $0.preview.localizedCaseInsensitiveContains(filter) }
        }
        var rows: [Row] = []
        rows.reserveCapacity(spaces.count + 8)
        var current: String?
        for space in spaces {
            if space.section != current {
                rows.append(.section(space.section, count: nil, accent: false))
                current = space.section
            }
            rows.append(.space(space))
        }
        return rows
    }

    /// What a row's cell shows; equal signatures need no reload.
    private func signature(_ row: Row) -> Int {
        var hasher = Hasher()
        switch row {
        case .section(let title, let count, let accent):
            hasher.combine(title); hasher.combine(count); hasher.combine(accent)
        case .shelf(let bundles):
            for (category, members) in bundles {
                hasher.combine(category)
                hasher.combine(members.count)
                hasher.combine(members.filter(\.isUnread).count)
                for member in members.prefix(8) { hasher.combine(member.id) }
            }
        case .conversation(let conversation):
            hasher.combine(store.row(for: conversation))
            hasher.combine(store.tag(for: conversation))
            hasher.combine(store.snippet(for: conversation.latest.ref))
        case .space(let space):
            hasher.combine(space)
        }
        return hasher.finalize()
    }

    // MARK: - Store updates

    /// Chat spaces matter only while the list shows them.
    @objc private func chatChanged() {
        if isChat { listChanged() }
    }

    @objc private func listChanged() {
        var context = false
        if lastContext != store.listContext {
            context = lastContext != nil
            lastContext = store.listContext
            autoFillPages = 0
            if openBundle != nil {
                openBundle = nil
                delegate?.conversationListDidChangeBundle(self)
            }
            highlightedID = nil
        }
        let previousRow = table.selectedRow >= 0 ? table.selectedRow : lastHighlightedRow
        let hadBundle = openBundle
        let newRows = buildRows()
        if hadBundle != openBundle { delegate?.conversationListDidChangeBundle(self) }
        if context {
            transition(.rise) { self.apply(newRows, animated: false) }
        } else {
            apply(newRows, animated: true)
        }

        suppressSelection = true
        var notify = false
        if let highlightedID {
            if let row = rows.firstIndex(where: { $0.id == highlightedID }) {
                if table.selectedRow != row { table.selectRowIndexes([row], byExtendingSelection: false) }
            } else if let row = nearestConversationRow(to: previousRow), store.listContext.search == nil {
                // The highlighted conversation left the list (archived, trashed): advance like Mail does.
                table.selectRowIndexes([row], byExtendingSelection: false)
                self.highlightedID = rows[row].id
                notify = true
            } else {
                table.deselectAll(nil)
                self.highlightedID = nil
                notify = true
            }
        } else if table.selectedRow >= 0 {
            table.deselectAll(nil)
        }
        suppressSelection = false
        if notify { delegate?.conversationList(self, didHighlight: highlightedConversation) }

        emptyLabel.isHidden = !rows.isEmpty
        if isChat {
            emptyLabel.stringValue = store.chat.isLoading ? "Loading…" : (chatFilter?.isEmpty == false ? "No Results" : "No Chats")
        } else {
            emptyLabel.stringValue = store.isLoading ? "Loading…" : (store.listContext.search != nil ? "No Results" : "No Conversations")
        }
        updateStatus()
        DispatchQueue.main.async { [weak self] in self?.afterReload() }
    }

    /// Replaces the rows; small changes animate as row insertions and removals.
    private func apply(_ newRows: [Row], animated: Bool) {
        let oldKeys = rows.map(\.key)
        let oldSignatures = signatures
        rows = newRows
        signatures = newRows.map(signature)
        rowByRef = [:]
        for (index, row) in rows.enumerated() {
            if let conversation = row.conversation { rowByRef[conversation.latest.ref] = index }
        }

        guard animated, !oldKeys.isEmpty, view.window != nil else {
            table.reloadData()
            return
        }
        let newKeys = newRows.map(\.key)
        guard newKeys != oldKeys else {
            reloadChanged(Set(0..<newKeys.count).filter { oldSignatures[$0] != signatures[$0] })
            return
        }
        let diff = newKeys.difference(from: oldKeys)
        guard diff.count <= 24 else {
            table.reloadData()
            return
        }
        var removals = IndexSet()
        var insertions = IndexSet()
        for change in diff {
            switch change {
            case .remove(let offset, _, _): removals.insert(offset)
            case .insert(let offset, _, _): insertions.insert(offset)
            }
        }
        // Rows kept by the diff pair up in order; reload the ones whose content changed.
        var changed = IndexSet()
        var old = 0
        for new in 0..<newKeys.count where !insertions.contains(new) {
            while removals.contains(old) { old += 1 }
            if old < oldSignatures.count, oldSignatures[old] != signatures[new] { changed.insert(new) }
            old += 1
        }
        if !removals.isEmpty || !insertions.isEmpty {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.24
                context.timingFunction = Motion.ease
                table.beginUpdates()
                table.removeRows(at: removals, withAnimation: [.effectFade, .slideUp])
                table.insertRows(at: insertions, withAnimation: [.effectFade, .slideDown])
                table.endUpdates()
            }
        }
        reloadChanged(changed)
    }

    /// How new list content arrives while the old cross-fades away.
    private enum Entrance {
        /// A different mailbox or search: drifts up into place.
        case rise
        /// Into a bundle: spreads out from the middle.
        case open
        /// Back out of a bundle: settles in from slightly larger.
        case close
    }

    private func transition(_ entrance: Entrance, _ change: () -> Void) {
        guard view.window != nil, let layer = scroll.layer else { change(); return }
        let fade = CATransition()
        fade.type = .fade
        fade.duration = 0.22
        fade.timingFunction = Motion.ease
        fade.preferredFrameRateRange = Motion.frameRate
        layer.add(fade, forKey: "contentFade")
        change()
        switch entrance {
        case .rise: Motion.slideIn(table, dy: 10)
        // The clip view, not the tall table: its center is the middle of what is on screen.
        case .open: Motion.unfold(scroll.contentView, from: 0.94)
        case .close: Motion.unfold(scroll.contentView, from: 1.04)
        }
    }

    private func afterReload() {
        requestVisibleSnippets()
        guard canLoadMore, !isLoadingList, autoFillPages < Self.autoFillLimit,
              let document = scroll.documentView else { return }
        if document.bounds.height < scroll.contentView.bounds.height + 600 {
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

    private func requestVisibleSnippets() {
        let range = table.rows(in: table.visibleRect)
        guard range.length > 0 else { return }
        let upper = min(rows.count, range.location + range.length + 10)
        store.prefetchSnippets(for: rows[range.location..<upper].compactMap(\.conversation))
    }

    private func nearestConversationRow(to row: Int) -> Int? {
        guard !rows.isEmpty else { return nil }
        let start = min(max(row, 0), rows.count - 1)
        for index in start..<rows.count where rows[index].id != nil { return index }
        for index in stride(from: start, through: 0, by: -1) where rows[index].id != nil { return index }
        return nil
    }

    /// Visible rows with that sender take the picture in place; offscreen ones get it when configured.
    @objc private func avatarResolved(_ note: Notification) {
        guard let email = note.userInfo?["email"] as? String,
              let photo = store.avatars.image(for: email, automated: false) else { return }
        let visible = table.rows(in: table.visibleRect)
        for index in visible.location..<(visible.location + visible.length) {
            (table.view(atColumn: 0, row: index, makeIfNecessary: false) as? ConversationRowCell)?.showPhoto(photo, for: email)
        }
    }

    /// One batch per main-actor turn, however many previews arrived in it.
    @objc private func contentLoaded(_ note: Notification) {
        guard let refs = note.userInfo?["refs"] as? Set<MessageRef> else { return }
        var changed = IndexSet()
        for ref in refs {
            guard let row = rowByRef[ref] else { continue }
            let signature = signature(rows[row])
            if signatures[row] != signature {
                signatures[row] = signature
                changed.insert(row)
            }
        }
        reloadChanged(changed)
    }

    /// Cells read the account bar's color from the account order, so only a different set of
    /// accounts redraws rows; unread-count changes don't.
    @objc private func accountsChanged() {
        let names = store.accounts.map(\.name)
        guard names != accountNames else { return }
        accountNames = names
        guard !rows.isEmpty else { return }
        table.reloadData(forRowIndexes: IndexSet(integersIn: 0..<rows.count), columnIndexes: [0])
    }

    /// A visible shelf is updated in place rather than replaced: a refresh bringing new mail must not
    /// swap the tiles out from under a click in progress.
    private func reloadChanged<S: Sequence<Int>>(_ indexes: S) {
        var set = IndexSet(indexes)
        for index in set {
            guard case .shelf(let bundles) = rows[index],
                  let cell = table.view(atColumn: 0, row: index, makeIfNecessary: false) as? BundleShelfCell
            else { continue }
            cell.configure(bundles, own: store.ownAddresses)
            set.remove(index)
        }
        if !set.isEmpty { table.reloadData(forRowIndexes: set, columnIndexes: [0]) }
    }

    @objc private func storeError() { updateStatus() }

    /// The floating status capsule appears only while something is happening or went wrong.
    private func updateStatus() {
        if isChat {
            if store.chat.isLoading {
                status.show("Checking for messages…", busy: true)
            } else if let error = store.chat.lastError {
                status.show(error, busy: false, warning: true)
            } else {
                status.hide()
            }
        } else if store.isLoading {
            status.show(store.searchQuery != nil ? "Searching…" : "Checking for mail…", busy: true)
        } else if let error = store.lastError {
            status.show(error, busy: false, warning: true)
        } else if let tagging = store.aiTaggingStatus {
            status.show(tagging, busy: true)
        } else {
            status.hide()
        }
    }

    @objc private func scrolled() {
        table.refreshHover()
        requestVisibleSnippets()
        guard canLoadMore, !isLoadingList, let document = scroll.documentView else { return }
        let visible = scroll.contentView.bounds
        if visible.maxY > document.bounds.height - 600 { loadMore() }
    }

    @objc private func userScrolled() {
        autoFillPages = 0
        table.hoverSuspended = true
    }

    @objc private func scrollEnded() { table.hoverSuspended = false }

    // MARK: - Bundles

    func enterBundle(_ category: MailCategory) {
        savedScroll["top"] = scroll.contentView.bounds.origin.y
        openBundle = category
        autoFillPages = 0
        highlightedID = nil
        transition(.open) {
            self.apply(self.buildRows(), animated: false)
            self.table.deselectAll(nil)
            self.table.scrollRowToVisible(0)
        }
        delegate?.conversationListDidChangeBundle(self)
        DispatchQueue.main.async { [weak self] in self?.afterReload() }
    }

    func leaveBundle() {
        guard openBundle != nil else { return }
        openBundle = nil
        highlightedID = nil
        transition(.close) {
            self.apply(self.buildRows(), animated: false)
            self.table.deselectAll(nil)
            if let y = self.savedScroll["top"] { self.scroll.contentView.scroll(to: NSPoint(x: 0, y: y)) }
        }
        delegate?.conversationListDidChangeBundle(self)
    }

    // MARK: - Highlight and opening

    var highlightedConversation: Conversation? {
        guard table.selectedRow >= 0, table.selectedRow < rows.count else { return nil }
        return rows[table.selectedRow].conversation
    }

    /// Moves the keyboard highlight to `conversation` without opening it (used after Back).
    func highlight(_ conversation: Conversation?) {
        guard let conversation, let row = rows.firstIndex(where: { $0.conversation?.id == conversation.id }) else { return }
        suppressSelection = true
        table.selectRowIndexes([row], byExtendingSelection: false)
        suppressSelection = false
        highlightedID = conversation.id
        lastHighlightedRow = row
        table.scrollRowToVisible(row)
    }

    /// Moves the keyboard highlight to a chat space's row (used after Back).
    func highlight(space key: ChatSpaceKey) {
        let id = "h:" + key.id
        guard let row = rows.firstIndex(where: { $0.id == id }) else { return }
        suppressSelection = true
        table.selectRowIndexes([row], byExtendingSelection: false)
        suppressSelection = false
        highlightedID = id
        lastHighlightedRow = row
        table.scrollRowToVisible(row)
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !suppressSelection else { return }
        if table.selectedRow >= 0 { lastHighlightedRow = table.selectedRow }
        highlightedID = table.selectedRow >= 0 && table.selectedRow < rows.count ? rows[table.selectedRow].id : nil
        delegate?.conversationList(self, didHighlight: highlightedConversation)
        preload(table.selectedRow)
    }

    /// Loads the conversation at `row` once the pointer or highlight rests on it, so opening it
    /// finds its bodies in memory; passing over rows loads nothing.
    private func preload(_ row: Int) {
        preloadTask?.cancel()
        guard row >= 0, row < rows.count else { return }
        if let space = rows[row].space {
            store.chat.preload(space.key)
            return
        }
        guard let conversation = rows[row].conversation else { return }
        preloadTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(80))
            guard !Task.isCancelled, let self else { return }
            store.preload(conversation)
        }
    }

    @objc private func rowClicked() {
        let row = table.clickedRow
        guard row >= 0, row < rows.count else { return }
        open(row)
    }

    private func openHighlighted() {
        guard table.selectedRow >= 0 else { return }
        open(table.selectedRow)
    }

    private func open(_ row: Int) {
        switch rows[row] {
        case .conversation(let conversation): delegate?.conversationList(self, open: conversation)
        case .space(let space): delegate?.conversationList(self, openChat: space.key)
        case .section, .shelf: break
        }
    }

    private func performOnHighlight(_ action: ConversationAction) {
        guard let conversation = highlightedConversation else { return }
        delegate?.conversationList(self, perform: action, on: conversation)
    }

    // MARK: - Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        switch rows[row] {
        case .section: ListMetrics.sectionHeight
        case .shelf: ListMetrics.shelfHeight
        case .conversation, .space: ListMetrics.rowHeight
        }
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        guard rows[row].id != nil else { return nil }
        return tableView.makeView(withIdentifier: Self.rowViewID, owner: nil) as? ListRowView
            ?? { let view = ListRowView(); view.identifier = Self.rowViewID; return view }()
    }

    private static let rowViewID = NSUserInterfaceItemIdentifier("ListRowView")

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        rows[row].id != nil
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch rows[row] {
        case .section(let title, let count, let accent):
            let cell = tableView.makeView(withIdentifier: SectionHeaderCell.identifier, owner: nil) as? SectionHeaderCell
                ?? SectionHeaderCell()
            cell.configure(title, count: count, accent: accent)
            return cell
        case .shelf(let bundles):
            let cell = tableView.makeView(withIdentifier: BundleShelfCell.identifier, owner: nil) as? BundleShelfCell
                ?? BundleShelfCell()
            cell.configure(bundles, own: store.ownAddresses)
            cell.onOpen = { [weak self] category in self?.enterBundle(category) }
            return cell
        case .conversation(let conversation):
            let cell = tableView.makeView(withIdentifier: ConversationRowCell.identifier, owner: nil) as? ConversationRowCell
                ?? ConversationRowCell()
            let context = store.listContext
            let row = store.row(for: conversation)
            let tag = store.tag(for: conversation)
            let automated = tag.map { $0.category != .people } ?? false
            cell.configure(row, snippet: store.snippet(for: conversation.latest.ref),
                           archivable: ![.archive, .all, .trash].contains(context.role),
                           account: context.selection.isUnified
                               ? (conversation.account, AccountStyle.color(for: conversation.account, in: store)) : nil,
                           tag: tag,
                           photo: store.avatars.image(for: row.avatarKey, automated: automated))
            let id = conversation.id
            cell.onAction = { [weak self] action in self?.perform(action, id: id) }
            let person = Address(name: row.avatarName, email: row.avatarKey)
            cell.onAvatar = { [weak self] anchor, photo in
                guard let self else { return }
                AvatarPreview.show(person, small: photo, automated: automated, avatars: self.store.avatars,
                                   relativeTo: anchor.bounds, of: anchor)
            }
            return cell
        case .space(let space):
            let cell = tableView.makeView(withIdentifier: ConversationRowCell.identifier, owner: nil) as? ConversationRowCell
                ?? ConversationRowCell()
            let photo = space.avatarKey.isEmpty ? nil : store.avatars.image(for: space.avatarKey, automated: false)
            cell.configure(space, account: store.listContext.selection == .allChats
                               ? (store.chat.account(named: space.key.account)?.email ?? space.key.account,
                                  AccountStyle.chatColor(for: space.key.account, in: store)) : nil,
                           photo: photo)
            cell.onAction = nil
            let person = Address(name: space.avatarName, email: space.avatarKey)
            cell.onAvatar = { [weak self] anchor, photo in
                guard let self else { return }
                AvatarPreview.show(person, small: photo, automated: false, avatars: self.store.avatars,
                                   relativeTo: anchor.bounds, of: anchor)
            }
            return cell
        }
    }

    func tableView(_ tableView: NSTableView, rowActionsForRow row: Int, edge: NSTableView.RowActionEdge) -> [NSTableViewRowAction] {
        guard row < rows.count, let conversation = rows[row].conversation else { return [] }
        let id = conversation.id
        switch edge {
        case .leading:
            let read = NSTableViewRowAction(style: .regular, title: conversation.isUnread ? "Read" : "Unread") { [weak self] _, _ in
                self?.perform(.toggleRead, id: id)
            }
            read.backgroundColor = .controlAccentColor
            read.image = NSImage(systemSymbolName: conversation.isUnread ? "envelope.open" : "envelope.badge", accessibilityDescription: nil)
            return [read]
        case .trailing:
            let trash = NSTableViewRowAction(style: .destructive, title: "Trash") { [weak self] _, _ in
                self?.perform(.trash, id: id)
            }
            trash.image = NSImage(systemSymbolName: "trash", accessibilityDescription: nil)
            guard store.canArchive(conversation) else { return [trash] }
            let archive = NSTableViewRowAction(style: .regular, title: "Archive") { [weak self] _, _ in
                self?.perform(.archive, id: id)
            }
            archive.backgroundColor = .systemPurple
            archive.image = NSImage(systemSymbolName: "archivebox", accessibilityDescription: nil)
            return [trash, archive]
        @unknown default:
            return []
        }
    }

    private func perform(_ action: ConversationAction, id: String) {
        guard let conversation = rows.lazy.compactMap(\.conversation).first(where: { $0.id == id }) else { return }
        table.rowActionsVisible = false
        delegate?.conversationList(self, perform: action, on: conversation)
    }

    // MARK: - Context menu

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let row = table.clickedRow
        guard row >= 0, row < rows.count else { return }
        if let space = rows[row].space {
            let open = NSMenuItem(title: "Open", action: #selector(openChatFromMenu(_:)), keyEquivalent: "")
            open.target = self
            open.representedObject = row
            menu.addItem(open)
            if space.isUnread {
                let read = NSMenuItem(title: "Mark as Read", action: #selector(markChatRead(_:)), keyEquivalent: "")
                read.image = NSImage(systemSymbolName: "envelope.open", accessibilityDescription: nil)
                read.target = self
                read.representedObject = space.key.id
                menu.addItem(read)
            }
            return
        }
        guard let conversation = rows[row].conversation else { return }
        func add(_ title: String, _ symbol: String, _ action: ConversationAction) {
            let item = NSMenuItem(title: title, action: #selector(contextAction(_:)), keyEquivalent: "")
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
            item.target = self
            item.representedObject = ContextPayload(action: action, id: conversation.id)
            menu.addItem(item)
        }
        add("Reply", "arrowshape.turn.up.left", .reply)
        add("Reply All", "arrowshape.turn.up.left.2", .replyAll)
        add("Forward", "arrowshape.turn.up.right", .forward)
        menu.addItem(.separator())
        add(conversation.isUnread ? "Mark as Read" : "Mark as Unread", conversation.isUnread ? "envelope.open" : "envelope.badge", .toggleRead)
        add(conversation.isFlagged ? "Unstar" : "Star", conversation.isFlagged ? "star.slash" : "star", .toggleStar)
        if let tag = store.tag(for: conversation), tag.category == .people {
            add(tag.toRespond ? "Resolve" : "Mark as Waiting on You", tag.toRespond ? "checkmark.circle" : "clock.badge", .toggleWaiting)
        }
        menu.addItem(.separator())
        if store.canArchive(conversation) { add("Archive", "archivebox", .archive) }
        add("Move to Trash", "trash", .trash)
    }

    private final class ContextPayload: NSObject {
        let action: ConversationAction
        let id: String
        init(action: ConversationAction, id: String) {
            self.action = action
            self.id = id
        }
    }

    @objc private func contextAction(_ sender: NSMenuItem) {
        guard let payload = sender.representedObject as? ContextPayload else { return }
        perform(payload.action, id: payload.id)
    }

    @objc private func openChatFromMenu(_ sender: NSMenuItem) {
        guard let row = sender.representedObject as? Int, row < rows.count else { return }
        open(row)
    }

    @objc private func markChatRead(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String,
              let space = rows.lazy.compactMap(\.space).first(where: { $0.id == id }) else { return }
        store.chat.markRead(space.key)
    }
}

/// Table with Mail-style single-key commands; Return or Space opens the highlighted row.
///
/// It is also the list's only hover tracker: one tracking area, and after every move, scroll,
/// reload, focus change or unhide it re-reads where the pointer really is and pushes that to every
/// visible row. Rows tracking themselves miss exits when content moves under a still pointer.
final class ConversationTableView: NSTableView {
    var onKey: ((ConversationAction) -> Void)?
    var onOpen: (() -> Void)?
    /// The row under the pointer changed (-1: none).
    var onHoverRow: ((Int) -> Void)?

    /// Set while the user scrolls, so rows don't flash as they pass under the pointer.
    var hoverSuspended = false {
        didSet { refreshHover() }
    }

    private var hoverArea: NSTrackingArea?
    private var pointer: NSPoint?
    private var hoveredRow = -1
    private var windowObservers: [NSObjectProtocol] = []

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeInKeyWindow, .inVisibleRect],
                                  owner: self)
        addTrackingArea(area)
        hoverArea = area
    }

    override func mouseEntered(with event: NSEvent) {
        guard event.trackingArea === hoverArea else { return super.mouseEntered(with: event) }
        refreshHover()
    }

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        refreshHover()
    }

    override func mouseExited(with event: NSEvent) {
        guard event.trackingArea === hoverArea else { return super.mouseExited(with: event) }
        setPointer(nil)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        windowObservers.forEach(NotificationCenter.default.removeObserver)
        windowObservers = []
        guard let window else { return }
        windowObservers = [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification].map { name in
            NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshHover() }
            }
        }
    }

    override func viewDidHide() {
        super.viewDidHide()
        setPointer(nil)
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        refreshHover()
    }

    override func reloadData() {
        super.reloadData()
        refreshHover()
    }

    override func reloadData(forRowIndexes rowIndexes: IndexSet, columnIndexes: IndexSet) {
        super.reloadData(forRowIndexes: rowIndexes, columnIndexes: columnIndexes)
        refreshHover()
    }

    override func endUpdates() {
        super.endUpdates()
        refreshHover()
    }

    override func didAdd(_ rowView: NSTableRowView, forRow row: Int) {
        super.didAdd(rowView, forRow: row)
        sync(rowView, row: row)
    }

    /// Re-derives hover from the pointer's current position. Only a point that actually lands on the
    /// table counts, so floating chrome or the reader sliding over it clears the hover.
    func refreshHover() {
        guard !hoverSuspended, let window, window.isKeyWindow, !isHiddenOrHasHiddenAncestor,
              let content = window.contentView else { return setPointer(nil) }
        let location = window.mouseLocationOutsideOfEventStream
        let point = convert(location, from: nil)
        guard visibleRect.contains(point), content.hitTest(location)?.isDescendant(of: self) == true else {
            return setPointer(nil)
        }
        setPointer(point)
    }

    private func setPointer(_ point: NSPoint?) {
        pointer = point
        let under = point.map { row(at: $0) } ?? -1
        if under != hoveredRow {
            hoveredRow = under
            onHoverRow?(under)
        }
        enumerateAvailableRowViews { rowView, row in sync(rowView, row: row) }
    }

    private func sync(_ rowView: NSTableRowView, row: Int) {
        let hovered = row == hoveredRow
        (rowView as? ListRowView)?.setHovering(hovered)
        if let shelf = rowView.view(atColumn: 0) as? BundleShelfCell {
            shelf.track(hovered ? pointer.map { shelf.convert($0, from: self) } : nil)
        }
    }

    override func keyDown(with event: NSEvent) {
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.numericPad, .function])
        if modifiers.isEmpty {
            switch event.keyCode {
            case 36, 76, 49: onOpen?(); return  // return, enter, space
            case 51, 117: onKey?(.trash); return  // delete, forward delete
            default: break
            }
            switch event.charactersIgnoringModifiers {
            case "e": onKey?(.archive); return
            case "s": onKey?(.toggleStar); return
            case "u": onKey?(.toggleRead); return
            case "d": onKey?(.toggleWaiting); return
            case "r": onKey?(.reply); return
            case "a": onKey?(.replyAll); return
            case "f": onKey?(.forward); return
            default: break
            }
        }
        super.keyDown(with: event)
    }
}
