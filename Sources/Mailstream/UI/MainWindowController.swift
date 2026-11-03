import AppKit

private extension NSToolbarItem.Identifier {
    static let back = Self("back")
    static let compose = Self("compose")
    static let refresh = Self("refresh")
    static let archive = Self("archive")
    static let trash = Self("trash")
    static let read = Self("read")
    static let star = Self("star")
    static let reply = Self("reply")
    static let replyAll = Self("replyAll")
    static let forward = Self("forward")
    static let search = Self("search")
}

/// Two panes in the Spark style: mailboxes | a full-width content area that shows either the
/// conversation list or one conversation. Routes menu, toolbar and keyboard commands to the store.
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate, NSSearchFieldDelegate,
    NSMenuItemValidation, NSToolbarItemValidation, ConversationListDelegate, ReaderDelegate {
    private let store: MailStore
    private let split = NSSplitViewController()
    private let sidebar: SidebarViewController
    private let list: ConversationListViewController
    private let reader: ReaderViewController
    private let content = NSViewController()
    private var searchItem: NSSearchToolbarItem?
    /// Conversation open in the reader; nil while the list is showing.
    private var opened: Conversation?
    /// Conversation under the list's keyboard highlight.
    private var highlighted: Conversation?
    private var escapeMonitor: Any?
    /// Bumped per open/close so a late "page ready" callback cannot re-present a stale reader.
    private var navigation = 0
    private var transitions = 0

    init(store: MailStore) {
        self.store = store
        sidebar = SidebarViewController(store: store)
        list = ConversationListViewController(store: store)
        // Its view loads below, with the window, so the reader's web process and shell page are warm
        // before the first open.
        reader = ReaderViewController(store: store)

        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 820),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.title = "Mailstream"
        window.titleVisibility = .visible
        window.toolbarStyle = .unified
        window.minSize = NSSize(width: 760, height: 480)
        window.setFrameAutosaveName("MainWindow")
        super.init(window: window)

        let container = NSView()
        container.wantsLayer = true
        for child in [list, reader] as [NSViewController] {
            content.addChild(child)
            child.view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(child.view)
            NSLayoutConstraint.activate([
                child.view.topAnchor.constraint(equalTo: container.topAnchor),
                child.view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                child.view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                child.view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
        }
        reader.view.wantsLayer = true
        list.view.wantsLayer = true
        reader.view.isHidden = true
        content.view = container

        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = 190
        sidebarItem.maximumThickness = 320
        let contentItem = NSSplitViewItem(viewController: content)
        contentItem.minimumThickness = 520
        split.splitViewItems = [sidebarItem, contentItem]
        let firstRun = UserDefaults.standard.object(forKey: "NSSplitView Subview Frames ContentSplit") == nil
        split.splitView.autosaveName = "ContentSplit"
        window.contentViewController = split
        if firstRun {
            DispatchQueue.main.async { [split] in split.splitView.setPosition(230, ofDividerAt: 0) }
        }
        if !window.setFrameUsingName("MainWindow") {
            window.setContentSize(NSSize(width: 1280, height: 820))
            window.center()
        }
        window.delegate = self

        list.delegate = self
        reader.delegate = self

        let toolbar = NSToolbar(identifier: "ContentToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = true
        toolbar.autosavesConfiguration = true
        window.toolbar = toolbar

        // Escape goes back from the reader or a bundle, wherever focus is (web content included).
        escapeMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard event.keyCode == 53 else { return event }
            let handled = MainActor.assumeIsolated { () -> Bool in
                guard let self, let window = self.window, event.window === window, window.attachedSheet == nil,
                      self.canGoBack, !(window.firstResponder is NSTextView) else { return false }
                if self.opened != nil, self.reader.handleEscape() { return true }
                self.goBack(nil)
                return true
            }
            return handled ? nil : event
        }

        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(listChanged), name: .storeListChanged, object: store)
        center.addObserver(self, selector: #selector(accountsChanged), name: .storeAccountsChanged, object: store)
        center.addObserver(self, selector: #selector(storeError(_:)), name: .storeError, object: store)
        updateTitle()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// What toolbar and menu commands act on: the open conversation, else the highlighted row.
    private var selected: Conversation? { opened ?? highlighted }

    /// Account new messages default to: the selected conversation's, then the selected mailbox's.
    var preferredAccount: String? {
        if let selected { return selected.account }
        if case .mailbox(let key) = store.selection { return key.account }
        return store.accounts.first(where: \.isDefault)?.name ?? store.accounts.first?.name
    }

    // MARK: - Navigation

    private func open(_ conversation: Conversation) {
        let wasOpen = opened != nil
        opened = conversation
        highlighted = conversation
        navigation += 1
        let current = navigation
        var presented = false
        let present = { [weak self] in
            guard let self, !presented, self.navigation == current else { return }
            presented = true
            if !wasOpen { self.swap(forward: true) }
            self.window?.makeFirstResponder(self.reader.view)
        }
        // Slide in as soon as the thread is in the page, so the reader never arrives blank. A shell
        // still loading (cold start, CSP switch) gets a short fallback; a warm shell gets a longer
        // one that only matters when the web process stalls.
        reader.show(conversation, onReady: present)
        DispatchQueue.main.asyncAfter(deadline: .now() + (reader.awaitsShell ? 0.12 : 0.4)) { present() }
        if conversation.isUnread { store.setRead(conversation, read: true) }
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    private func closeReader() {
        guard opened != nil else { return }
        let last = opened
        opened = nil
        navigation += 1
        list.highlight(last)
        swap(forward: false) { [weak self] in self?.reader.show(nil) }
        window?.makeFirstResponder(list.tableView)
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    /// The reader is an opaque layer above the list: it fades in spreading out from the middle on
    /// open, and fades out shrinking back toward it on close, so the list is never blanked mid-transition.
    private func swap(forward: Bool, completion: (() -> Void)? = nil) {
        transitions += 1
        let current = transitions
        let page = reader.view
        page.layer?.removeAnimation(forKey: "fold")
        list.view.isHidden = false
        list.view.alphaValue = 1
        page.isHidden = false
        if forward {
            page.alphaValue = 0
            Motion.unfold(page, from: 0.94)
        } else {
            Motion.unfold(list.view, from: 1.03)
            if let layer = page.layer {
                let fold = Motion.basic("transform", from: CATransform3DIdentity, to: Motion.centered(0.96, in: layer),
                                        duration: 0.24)
                fold.fillMode = .forwards
                fold.isRemovedOnCompletion = false
                layer.add(fold, forKey: "fold")
            }
        }
        Motion.group({
            Motion.fade(page, to: forward ? 1 : 0, duration: forward ? Motion.standard : 0.24)
        }, completion: { [weak self] in
            guard let self, self.transitions == current else { return }
            page.layer?.removeAnimation(forKey: "fold")
            if forward {
                self.list.view.isHidden = true
            } else {
                page.isHidden = true
                page.alphaValue = 1
                // The reader covered the list, so the row under the pointer only now gets its hover.
                (self.list.tableView as? ConversationTableView)?.refreshHover()
            }
            completion?()
        })
    }

    @objc func goBack(_ sender: Any?) {
        if opened != nil {
            closeReader()
        } else {
            list.leaveBundle()
        }
    }

    private var canGoBack: Bool { opened != nil || list.openBundle != nil }

    // MARK: - Store observation

    @objc private func listChanged() {
        if let opened {
            if let fresh = store.conversations.first(where: { $0.id == opened.id }) {
                self.opened = fresh
                reader.update(fresh)
            } else if store.searchQuery == nil {
                // Archived, trashed or moved away while open: return to the list.
                closeReader()
            }
        }
        if let highlighted, let fresh = store.conversations.first(where: { $0.id == highlighted.id }) {
            self.highlighted = fresh
        }
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    @objc private func accountsChanged() { updateTitle() }

    @objc private func storeError(_ note: Notification) {
        guard note.userInfo?["alert"] as? Bool == true, let message = note.userInfo?["message"] as? String,
              let window, window.attachedSheet == nil else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "The action could not be completed"
        alert.informativeText = message
        alert.beginSheetModal(for: window)
    }

    private func updateTitle() {
        guard let window else { return }
        let unread = store.selectionRole == .drafts ? 0 : store.badgeCount(store.selection)
        window.title = list.openBundle?.title ?? store.selectionTitle
        if store.searchQuery != nil {
            window.subtitle = store.isLoading ? "Searching…" : "\(store.conversations.count) results"
        } else if store.isLoading && store.conversations.isEmpty {
            window.subtitle = "Loading…"
        } else if list.openBundle != nil {
            let counts = list.bundleCounts
            window.subtitle = "\(counts.total.formatted()) conversations" + (counts.unread > 0 ? " · \(counts.unread.formatted()) unread" : "")
        } else {
            window.subtitle = unread > 0 ? "\(unread.formatted()) unread" : ""
        }
    }

    // MARK: - List and reader delegates

    func conversationList(_ list: ConversationListViewController, didHighlight conversation: Conversation?) {
        highlighted = conversation
        window?.toolbar?.validateVisibleItems()
    }

    func conversationList(_ list: ConversationListViewController, open conversation: Conversation) {
        open(conversation)
    }

    func conversationList(_ list: ConversationListViewController, perform action: ConversationAction, on conversation: Conversation) {
        perform(action, on: conversation)
    }

    func conversationListDidChangeBundle(_ list: ConversationListViewController) {
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    func reader(_ reader: ReaderViewController, compose mode: Himalaya.ComposeMode, replyAll: Bool, message: MailMessage,
                text: String, plan: ReplyPlan?) {
        ComposeWindowController.open(store: store, mode: mode, account: message.ref.account, source: message,
                                     replyAll: replyAll, text: text, plan: plan)
    }

    func reader(_ reader: ReaderViewController, composeTo address: Address) {
        ComposeWindowController.open(store: store, mode: .new, account: preferredAccount, to: [address])
    }

    private func perform(_ action: ConversationAction, on conversation: Conversation) {
        switch action {
        case .archive: store.archive(conversation)
        case .trash:
            if store.selectionRole == .trash {
                confirmPermanentDelete(conversation)
            } else {
                store.trash(conversation)
            }
        case .toggleRead: store.setRead(conversation, read: conversation.isUnread)
        case .toggleStar: store.setFlagged(conversation, flagged: !conversation.isFlagged)
        case .toggleWaiting: store.setWaiting(conversation, waiting: store.tag(for: conversation)?.toRespond != true)
        case .reply: replyInline(all: false, conversation)
        case .replyAll: replyInline(all: true, conversation)
        case .forward: composeForward(conversation)
        }
    }

    /// Replies open the conversation and its inline reply; the window composer is one click away there.
    private func replyInline(all: Bool, _ conversation: Conversation) {
        if opened?.id != conversation.id { open(conversation) }
        reader.startReply(all: all)
    }

    private func composeForward(_ conversation: Conversation) {
        let own = store.ownAddresses
        let source = conversation.messages.last { message in
            !message.envelope.isDraft && !(message.envelope.from.contains { own.contains($0.normalizedEmail) })
        } ?? conversation.latest
        ComposeWindowController.open(store: store, mode: .forward(source.ref), account: source.ref.account, source: source)
    }

    private func confirmPermanentDelete(_ conversation: Conversation) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "Delete this conversation permanently?"
        alert.informativeText = "Messages already in the trash are removed for good."
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        alert.beginSheetModal(for: window) { [store] response in
            if response == .alertFirstButtonReturn { store.trash(conversation) }
        }
    }

    // MARK: - Actions

    @objc func refresh(_ sender: Any?) {
        store.refresh()
        store.refreshCounts()
    }

    @objc func composeNew(_ sender: Any?) {
        ComposeWindowController.open(store: store, mode: .new, account: preferredAccount)
    }

    @objc func focusSearch(_ sender: Any?) {
        guard let searchItem else { return }
        searchItem.beginSearchInteraction()
    }

    @objc func toggleAITagging(_ sender: Any?) { store.aiTaggingEnabled.toggle() }
    @objc func retagAll(_ sender: Any?) { store.retagAll() }

    @objc func archive(_ sender: Any?) { if let selected { perform(.archive, on: selected) } }
    @objc func trash(_ sender: Any?) { if let selected { perform(.trash, on: selected) } }
    @objc func toggleRead(_ sender: Any?) { if let selected { perform(.toggleRead, on: selected) } }
    @objc func toggleStar(_ sender: Any?) { if let selected { perform(.toggleStar, on: selected) } }
    @objc func toggleWaiting(_ sender: Any?) { if let selected { perform(.toggleWaiting, on: selected) } }
    @objc func reply(_ sender: Any?) { if let selected { perform(.reply, on: selected) } }
    @objc func replyAll(_ sender: Any?) { if let selected { perform(.replyAll, on: selected) } }
    @objc func forward(_ sender: Any?) { if let selected { perform(.forward, on: selected) } }

    private func isEnabled(_ action: Selector) -> Bool {
        switch action {
        case #selector(goBack(_:)): return canGoBack
        case #selector(retagAll(_:)): return store.aiTaggingEnabled
        case #selector(archive(_:)): return selected.map(store.canArchive) ?? false
        case #selector(toggleWaiting(_:)): return selected.map(store.canMarkWaiting) ?? false
        case #selector(trash(_:)), #selector(toggleRead(_:)), #selector(toggleStar(_:)),
             #selector(reply(_:)), #selector(replyAll(_:)), #selector(forward(_:)):
            return selected != nil
        default: return true
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        guard let action = menuItem.action else { return true }
        if action == #selector(toggleRead(_:)) {
            menuItem.title = selected?.isUnread == true ? "Mark as Read" : "Mark as Unread"
        } else if action == #selector(toggleStar(_:)) {
            menuItem.title = selected?.isFlagged == true ? "Unstar" : "Star"
        } else if action == #selector(toggleWaiting(_:)) {
            let waiting = selected.flatMap(store.tag(for:))?.toRespond ?? true
            menuItem.title = waiting ? "Resolve" : "Mark as Waiting on You"
        } else if action == #selector(toggleAITagging(_:)) {
            menuItem.state = store.aiTaggingEnabled ? .on : .off
        }
        return isEnabled(action)
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        guard let action = item.action else { return true }
        if action == #selector(toggleRead(_:)) {
            let unread = selected?.isUnread == true
            item.image = NSImage(systemSymbolName: unread ? "envelope.open" : "envelope.badge", accessibilityDescription: nil)
            item.label = unread ? "Mark as Read" : "Mark as Unread"
        } else if action == #selector(toggleStar(_:)) {
            let starred = selected?.isFlagged == true
            item.image = NSImage(systemSymbolName: starred ? "star.fill" : "star", accessibilityDescription: nil)
            item.label = starred ? "Unstar" : "Star"
        }
        return isEnabled(action)
    }

    // MARK: - Search

    func controlTextDidChange(_ obj: Notification) {
        guard let field = obj.object as? NSSearchField else { return }
        search(field.stringValue)
    }

    @objc private func searchSubmitted(_ sender: NSSearchField) {
        search(sender.stringValue)
    }

    func searchFieldDidEndSearching(_ sender: NSSearchField) {
        search("")
    }

    private func search(_ query: String) {
        if !query.isEmpty { closeReader() }
        store.search(query)
    }

    // MARK: - Toolbar

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .back, .flexibleSpace, .archive, .trash, .read, .star,
         .space, .reply, .replyAll, .forward, .space, .refresh, .compose, .search]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar) + [.flexibleSpace]
    }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        switch identifier {
        case .search:
            let item = NSSearchToolbarItem(itemIdentifier: identifier)
            item.searchField.placeholderString = "Search"
            item.searchField.delegate = self
            item.searchField.target = self
            item.searchField.action = #selector(searchSubmitted(_:))
            item.searchField.sendsSearchStringImmediately = false
            item.preferredWidthForSearchField = 220
            searchItem = item
            return item
        case .back:
            let item = button(identifier, "Back", "chevron.left", #selector(goBack(_:)))
            item.isNavigational = true
            return item
        case .compose: return button(identifier, "New Message", "square.and.pencil", #selector(composeNew(_:)))
        case .refresh: return button(identifier, "Get New Mail", "arrow.clockwise", #selector(refresh(_:)))
        case .archive: return button(identifier, "Archive", "archivebox", #selector(archive(_:)))
        case .trash: return button(identifier, "Trash", "trash", #selector(trash(_:)))
        case .read: return button(identifier, "Mark as Unread", "envelope.badge", #selector(toggleRead(_:)))
        case .star: return button(identifier, "Star", "star", #selector(toggleStar(_:)))
        case .reply: return button(identifier, "Reply", "arrowshape.turn.up.left", #selector(reply(_:)))
        case .replyAll: return button(identifier, "Reply All", "arrowshape.turn.up.left.2", #selector(replyAll(_:)))
        case .forward: return button(identifier, "Forward", "arrowshape.turn.up.right", #selector(forward(_:)))
        default: return nil
        }
    }

    private func button(_ identifier: NSToolbarItem.Identifier, _ label: String, _ symbol: String,
                        _ action: Selector) -> NSToolbarItem {
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = label
        item.paletteLabel = label
        item.toolTip = label
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        item.isBordered = true
        item.target = self
        item.action = action
        return item
    }
}
