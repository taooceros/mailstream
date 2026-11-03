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
/// conversation list or one conversation (or the chat spaces list or one space). Routes menu,
/// toolbar and keyboard commands to the store.
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
    /// Chat space open in the reader; nil while the list or a conversation is showing.
    private var openedSpace: ChatSpaceKey?
    /// Conversation under the list's keyboard highlight.
    private var highlighted: Conversation?
    private var escapeMonitor: Any?
    private var backMonitor: Any?
    private var swipe = BackSwipe()
    /// Whether a back swipe has the reader under the user's fingers.
    private var dragging = false
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
                if self.opened != nil || self.openedSpace != nil, self.reader.handleEscape() { return true }
                self.goBack(nil)
                return true
            }
            return handled ? nil : event
        }
        // A two-finger swipe right drags the reader away (when "Swipe between pages" uses two
        // fingers), and a mouse's Back button goes back like Escape.
        backMonitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .otherMouseDown]) { [weak self] event in
            let handled = MainActor.assumeIsolated { self?.handleBackInput(event) ?? false }
            return handled ? nil : event
        }

        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(listChanged), name: .storeListChanged, object: store)
        center.addObserver(self, selector: #selector(accountsChanged), name: .storeAccountsChanged, object: store)
        center.addObserver(self, selector: #selector(chatChanged), name: .chatSpacesChanged, object: store.chat)
        center.addObserver(self, selector: #selector(storeError(_:)), name: .storeError, object: store)
        updateTitle()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// What toolbar and menu commands act on: the open conversation, else the highlighted row.
    /// Chat has none, so every mail command validates to disabled there.
    private var selected: Conversation? { store.selection.isChat ? nil : opened ?? highlighted }

    /// Account new messages default to: the selected conversation's, then the selected mailbox's.
    var preferredAccount: String? {
        if let selected { return selected.account }
        if case .mailbox(let key) = store.selection { return key.account }
        return store.accounts.first(where: \.isDefault)?.name ?? store.accounts.first?.name
    }

    // MARK: - Navigation

    private func open(_ conversation: Conversation) {
        let wasOpen = opened != nil || openedSpace != nil
        openedSpace = nil
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
        // still loading (cold start, web process restart) gets a short fallback; a warm shell gets a
        // longer one that only matters when the web process stalls.
        reader.show(conversation, onReady: present)
        DispatchQueue.main.asyncAfter(deadline: .now() + (reader.awaitsShell ? 0.12 : 0.4)) { present() }
        if conversation.isUnread { store.setRead(conversation, read: true) }
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    /// Opens a chat space: the reader slides in with what is cached, and the store fetches the
    /// newest messages and marks the space read.
    private func openChat(_ key: ChatSpaceKey) {
        let wasOpen = opened != nil || openedSpace != nil
        opened = nil
        openedSpace = key
        navigation += 1
        let current = navigation
        var presented = false
        let present = { [weak self] in
            guard let self, !presented, self.navigation == current else { return }
            presented = true
            if !wasOpen { self.swap(forward: true) }
            self.window?.makeFirstResponder(self.reader.view)
        }
        // The reader captures the unread state before opening marks the space read.
        reader.showChat(key, onReady: present)
        DispatchQueue.main.asyncAfter(deadline: .now() + (reader.awaitsShell ? 0.12 : 0.4)) { present() }
        store.chat.open(key)
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    private func closeReader() {
        leaveReader { swap(forward: false) { [weak self] in self?.reader.show(nil) } }
    }

    /// Back to the list; `animation` takes the reader away once the row is highlighted, before the
    /// list takes focus.
    private func leaveReader(_ animation: () -> Void) {
        guard opened != nil || openedSpace != nil else { return }
        let last = opened, lastSpace = openedSpace
        opened = nil
        openedSpace = nil
        navigation += 1
        if let lastSpace {
            store.chat.close()
            list.highlight(space: lastSpace)
        } else {
            list.highlight(last)
        }
        animation()
        window?.makeFirstResponder(list.tableView)
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    /// The reader is an opaque layer above the list, which stays shown beneath it so leaving never
    /// waits on the list redrawing: it fades in spreading out from the middle on open, and fades out
    /// shrinking back toward it on close.
    private func swap(forward: Bool, completion: (() -> Void)? = nil) {
        transitions += 1
        let current = transitions
        let page = reader.view
        page.layer?.removeAnimation(forKey: "fold")
        page.layer?.removeAnimation(forKey: "swipe")
        list.view.layer?.removeAnimation(forKey: "swipe")
        dragging = false
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
            if !forward {
                page.isHidden = true
                page.alphaValue = 1
                // The reader covered the list, so the row under the pointer only now gets its hover.
                (self.list.tableView as? ConversationTableView)?.refreshHover()
            }
            completion?()
        })
    }

    @objc func goBack(_ sender: Any?) {
        if opened != nil || openedSpace != nil {
            closeReader()
        } else {
            list.leaveBundle()
        }
    }

    /// A conversation or a chat space is in the reader.
    private var readerOpen: Bool { opened != nil || openedSpace != nil }

    private var canGoBack: Bool { readerOpen || list.openBundle != nil }

    // MARK: - Swipe back

    private func handleBackInput(_ event: NSEvent) -> Bool {
        guard let window, event.window === window, window.attachedSheet == nil else { return false }
        if event.type == .otherMouseDown {
            guard event.buttonNumber == 3, canGoBack else { return false }
            goBack(nil)
            return true
        }
        let page = reader.view
        let canStart = readerOpen && NSEvent.isSwipeTrackingFromScrollEventsEnabled
            && page.bounds.contains(page.convert(event.locationInWindow, from: nil))
        switch swipe.feed(event, canStart: canStart) {
        case .pass:
            return false
        case .consume:
            return true
        case .drag(let offset):
            dragReader(to: offset)
            return true
        case .release(let offset, let velocity):
            releaseReader(at: offset, velocity: velocity)
            return true
        }
    }

    /// The reader follows the fingers to the right, uncovering the list, which settles from slightly
    /// enlarged as it does on close.
    private func dragReader(to offset: CGFloat) {
        guard readerOpen, let page = reader.view.layer, let below = list.view.layer else { return }
        if !dragging {
            dragging = true
            // An open still settling must not keep animating under the fingers.
            transitions += 1
            for key in ["unfold", "fold", "opacity", "swipe"] { page.removeAnimation(forKey: key) }
            below.removeAnimation(forKey: "unfold")
            reader.view.alphaValue = 1
        }
        let progress = min(offset / max(reader.view.bounds.width, 1), 1)
        Motion.hold(page, "transform", at: CATransform3DMakeTranslation(offset, 0, 0), key: "swipe")
        Motion.hold(below, "transform", at: Motion.centered(1 + 0.03 * (1 - progress), in: below), key: "swipe")
    }

    /// Past a third of the width, or flicked right, the reader slides off and the list stays; otherwise
    /// it slides back into place.
    private func releaseReader(at offset: CGFloat, velocity: CGFloat) {
        guard dragging, let page = reader.view.layer, let below = list.view.layer else { return }
        dragging = false
        let width = max(reader.view.bounds.width, 1)
        let back = readerOpen && (offset > width / 3 || (velocity > 400 && offset > 20))
        let remaining = back ? width - offset : offset
        let duration = min(0.3, max(0.16, Double(remaining / max(abs(velocity), 1500))))
        let progress = min(offset / width, 1)
        transitions += 1
        let current = transitions
        Motion.group({
            for (layer, from, to) in [
                (page, CATransform3DMakeTranslation(offset, 0, 0), back ? CATransform3DMakeTranslation(width, 0, 0) : CATransform3DIdentity),
                (below, Motion.centered(1 + 0.03 * (1 - progress), in: below), back ? CATransform3DIdentity : Motion.centered(1.03, in: below)),
            ] {
                let slide = Motion.basic("transform", from: from, to: to, duration: duration)
                slide.fillMode = .forwards
                slide.isRemovedOnCompletion = false
                layer.add(slide, forKey: "swipe")
            }
        }, completion: { [weak self] in
            guard let self, self.transitions == current else { return }
            page.removeAnimation(forKey: "swipe")
            below.removeAnimation(forKey: "swipe")
            if back {
                self.reader.view.isHidden = true
                (self.list.tableView as? ConversationTableView)?.refreshHover()
                self.reader.show(nil)
            }
        })
        if back { leaveReader {} }
    }

    // MARK: - Store observation

    @objc private func listChanged() {
        // Another selection replaced the chat list the open space came from: return to the list.
        if let openedSpace, store.listContext.selection != .allChats, store.listContext.selection != .chat(openedSpace.account) {
            closeReader()
        }
        if !store.listContext.selection.isChat { list.chatFilter = nil }
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

    @objc private func chatChanged() {
        guard store.selection.isChat else { return }
        updateTitle()
    }

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
        } else if store.selection.isChat {
            window.subtitle = store.chat.isLoading && store.chat.rows.isEmpty ? "Loading…" : (unread > 0 ? "\(unread.formatted()) unread" : "")
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

    func conversationList(_ list: ConversationListViewController, openChat space: ChatSpaceKey) {
        openChat(space)
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
        if store.selection.isChat {
            store.chat.refresh()
            return
        }
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
        if store.selection.isChat {
            list.chatFilter = query.isEmpty ? nil : query
        } else {
            store.search(query)
        }
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
