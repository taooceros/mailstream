import AppKit
import Observation

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
    /// What the reader shows; nil while the list is showing.
    private var opened: ReaderTarget?
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
    /// The published list, the title's inputs and alerts.
    private var storeWatches: [Task<Void, Never>] = []

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
            Task { [split] in split.splitView.setPosition(230, ofDividerAt: 0) }
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
        // Event monitors call back on the main thread.
        escapeMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            self?.handleEscape(event) == true ? nil : event
        }
        // A two-finger swipe right drags the reader away (when "Swipe between pages" uses two
        // fingers), and a mouse's Back button goes back like Escape.
        backMonitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .otherMouseDown]) { [weak self] event in
            self?.handleBackInput(event) == true ? nil : event
        }

        let problems = store.problems
        storeWatches = [
            Task.immediate(name: "Window list") { [weak self, store] in
                for await _ in Observations({ _ = (store.listContext, store.conversations, store.searchQuery) }) {
                    self?.listChanged()
                }
            },
            Task.immediate(name: "Window title") { [weak self] in
                for await titles in Observations({ [weak self] in self?.titles() }) {
                    if let titles { self?.show(titles) }
                }
            },
            Task.immediate(name: "Window alerts") { [weak self] in
                for await problem in Observations({ problems.alert }).dropFirst() {
                    if let problem { self?.presentAlert(problem) }
                }
            },
        ]
    }

    isolated deinit { storeWatches.forEach { $0.cancel() } }

    /// Escape goes back from the reader or a bundle, wherever focus is (web content included).
    private func handleEscape(_ event: NSEvent) -> Bool {
        guard event.keyCode == 53, let window, event.window === window, window.attachedSheet == nil,
              canGoBack, !(window.firstResponder is NSTextView) else { return false }
        if opened != nil, reader.handleEscape() { return true }
        goBack(nil)
        return true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// What toolbar and menu commands act on: the open conversation, else the highlighted row.
    /// Chat has none, so every mail command validates to disabled there.
    private var selected: Conversation? { store.selection.isChat ? nil : opened?.conversation ?? highlighted }

    /// Account new messages default to: the selected conversation's, then the selected mailbox's.
    var preferredAccount: String? {
        if let selected { return selected.account }
        if case .mailbox(let key) = store.selection { return key.account }
        return store.accounts.first?.name
    }

    // MARK: - Navigation

    /// Opens a conversation (marking it read) or a chat space (the store fetches its newest
    /// messages and marks it read) in the reader.
    private func open(_ target: ReaderTarget) {
        let wasOpen = opened != nil
        opened = target
        if let conversation = target.conversation { highlighted = conversation }
        navigation += 1
        let current = navigation
        var presented = false
        let present = { [weak self] in
            guard let self, !presented, self.navigation == current else { return }
            presented = true
            if !wasOpen { self.swap(forward: true) }
            self.window?.makeFirstResponder(self.reader.view)
        }
        // Slide in as soon as the target is in the page, so the reader never arrives blank. A shell
        // still loading (cold start, web process restart) gets a short fallback; a warm shell gets a
        // longer one that only matters when the web process stalls.
        reader.show(target, onReady: present)
        let fallback: Duration = reader.awaitsShell ? .milliseconds(120) : .milliseconds(400)
        Task {
            try? await Task.sleep(for: fallback)
            present()
        }
        // The reader captured a space's unread state before opening marks it read.
        switch target {
        case .conversation(let conversation): if conversation.isUnread { store.setRead(conversation, read: true) }
        case .chat(let key): store.chat.open(key)
        }
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    private func closeReader() {
        leaveReader { swap(forward: false) { [weak self] in self?.reader.show(nil) } }
    }

    /// Back to the list; `animation` takes the reader away once the row is highlighted, before the
    /// list takes focus.
    private func leaveReader(_ animation: () -> Void) {
        guard let last = opened else { return }
        opened = nil
        navigation += 1
        switch last {
        case .conversation(let conversation):
            list.highlight(conversation)
        case .chat(let key):
            store.chat.close()
            list.highlight(space: key)
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
        if opened != nil {
            closeReader()
        } else {
            list.leaveBundle()
        }
    }

    private var canGoBack: Bool { opened != nil || list.openBundle != nil }

    // MARK: - Swipe back

    private func handleBackInput(_ event: NSEvent) -> Bool {
        guard let window, event.window === window, window.attachedSheet == nil else { return false }
        if event.type == .otherMouseDown {
            guard event.buttonNumber == 3, canGoBack else { return false }
            goBack(nil)
            return true
        }
        let page = reader.view
        let canStart = opened != nil && NSEvent.isSwipeTrackingFromScrollEventsEnabled
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

    /// The reader follows the fingers sideways, uncovering the list, which settles from slightly
    /// enlarged as it does on close.
    private func dragReader(to offset: CGFloat) {
        guard opened != nil, let page = reader.view.layer, let below = list.view.layer else { return }
        if !dragging {
            dragging = true
            // An open still settling must not keep animating under the fingers.
            transitions += 1
            for key in ["unfold", "fold", "opacity", "swipe"] { page.removeAnimation(forKey: key) }
            below.removeAnimation(forKey: "unfold")
            reader.view.alphaValue = 1
        }
        let progress = min(abs(offset) / max(reader.view.bounds.width, 1), 1)
        Motion.hold(page, "transform", at: CATransform3DMakeTranslation(offset, 0, 0), key: "swipe")
        Motion.hold(below, "transform", at: Motion.centered(1 + 0.03 * (1 - progress), in: below), key: "swipe")
    }

    /// Past a third of the width, or flicked onward, the reader slides off the side it was swiped
    /// toward and the list stays; otherwise it slides back into place.
    private func releaseReader(at offset: CGFloat, velocity: CGFloat) {
        guard dragging, let page = reader.view.layer, let below = list.view.layer else { return }
        dragging = false
        let width = max(reader.view.bounds.width, 1)
        let distance = abs(offset)
        let onward = offset < 0 ? -velocity : velocity
        let back = opened != nil && (distance > width / 3 || (onward > 400 && distance > 20))
        let remaining = back ? width - distance : distance
        let duration = min(0.3, max(0.16, Double(remaining / max(abs(velocity), 1500))))
        let progress = min(distance / width, 1)
        let away = offset < 0 ? -width : width
        transitions += 1
        let current = transitions
        Motion.group({
            for (layer, from, to) in [
                (page, CATransform3DMakeTranslation(offset, 0, 0), back ? CATransform3DMakeTranslation(away, 0, 0) : CATransform3DIdentity),
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

    /// The store published a list: the open conversation follows it, or the reader closes.
    private func listChanged() {
        switch opened {
        case .chat(let key):
            // Another selection replaced the chat list the open space came from: return to the list.
            if store.listContext.selection != .allChats, store.listContext.selection != .chat(key.account) { closeReader() }
        case .conversation(let conversation):
            if let fresh = store.conversations.first(where: { $0.id == conversation.id }) {
                opened = .conversation(fresh)
                reader.update(fresh)
            } else if store.searchQuery == nil {
                // Archived, trashed or moved away while open: return to the list.
                closeReader()
            }
        case nil:
            break
        }
        if !store.listContext.selection.isChat { list.chatFilter = nil }
        if let highlighted, let fresh = store.conversations.first(where: { $0.id == highlighted.id }) {
            self.highlighted = fresh
        }
        window?.toolbar?.validateVisibleItems()
    }

    private func presentAlert(_ problem: StoreProblem) {
        guard let window, window.attachedSheet == nil else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "The action could not be completed"
        alert.informativeText = problem.message
        alert.beginSheetModal(for: window)
    }

    private func updateTitle() { show(titles()) }

    private func show(_ titles: (title: String, subtitle: String)) {
        window?.title = titles.title
        window?.subtitle = titles.subtitle
    }

    /// The selection's name, and its unread count, search results or loading state. Observed: the
    /// window retitles whenever anything read here changes.
    private func titles() -> (title: String, subtitle: String) {
        let unread = store.selectionRole == .drafts ? 0 : store.badgeCount(store.selection)
        let title = list.openBundle?.title ?? store.selectionTitle
        if store.searchQuery != nil {
            return (title, store.isLoading ? "Searching…" : "\(store.conversations.count) results")
        } else if store.selection.isChat {
            return (title, store.chat.isLoading && store.chat.rows.isEmpty ? "Loading…" : (unread > 0 ? "\(unread.formatted()) unread" : ""))
        } else if store.isLoading && store.conversations.isEmpty {
            return (title, "Loading…")
        } else if list.openBundle != nil {
            let counts = list.bundleCounts
            return (title, "\(counts.total.formatted()) conversations" + (counts.unread > 0 ? " · \(counts.unread.formatted()) unread" : ""))
        } else {
            return (title, unread > 0 ? "\(unread.formatted()) unread" : "")
        }
    }

    // MARK: - List and reader delegates

    func conversationList(_ list: ConversationListViewController, didHighlight conversation: Conversation?) {
        highlighted = conversation
        window?.toolbar?.validateVisibleItems()
    }

    func conversationList(_ list: ConversationListViewController, open conversation: Conversation) {
        open(.conversation(conversation))
    }

    func conversationList(_ list: ConversationListViewController, openChat space: ChatSpaceKey) {
        open(.chat(space))
    }

    func conversationList(_ list: ConversationListViewController, perform action: ConversationAction, on conversation: Conversation) {
        perform(action, on: conversation)
    }

    func conversationListDidChangeBundle(_ list: ConversationListViewController) {
        updateTitle()
        window?.toolbar?.validateVisibleItems()
    }

    func reader(_ reader: ReaderViewController, compose mode: ComposeMode, replyAll: Bool, message: MailMessage,
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
        case .toggleWaiting:
            store.tagging.setWaiting(conversation, waiting: store.tagging.tag(for: conversation)?.toRespond != true)
        case .reply: replyInline(all: false, conversation)
        case .replyAll: replyInline(all: true, conversation)
        case .forward: composeForward(conversation)
        }
    }

    /// Replies open the conversation and its inline reply; the window composer is one click away there.
    private func replyInline(all: Bool, _ conversation: Conversation) {
        if opened?.conversation?.id != conversation.id { open(.conversation(conversation)) }
        reader.startReply(all: all)
    }

    private func composeForward(_ conversation: Conversation) {
        let source = conversation.latestIncoming(own: store.ownAddresses) ?? conversation.latest
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

    @objc func toggleAITagging(_ sender: Any?) { store.tagging.isEnabled.toggle() }
    @objc func retagAll(_ sender: Any?) { store.tagging.retagAll() }

    @objc func archive(_ sender: Any?) { if let selected { perform(.archive, on: selected) } }
    @objc func trash(_ sender: Any?) { if let selected { perform(.trash, on: selected) } }
    @objc func toggleRead(_ sender: Any?) { if let selected { perform(.toggleRead, on: selected) } }
    @objc func toggleStar(_ sender: Any?) { if let selected { perform(.toggleStar, on: selected) } }
    @objc func toggleWaiting(_ sender: Any?) { if let selected { perform(.toggleWaiting, on: selected) } }
    @objc func reply(_ sender: Any?) { if let selected { perform(.reply, on: selected) } }
    @objc func replyAll(_ sender: Any?) { if let selected { perform(.replyAll, on: selected) } }
    @objc func forward(_ sender: Any?) { if let selected { perform(.forward, on: selected) } }

    private func isEnabled(_ action: Selector) -> Bool {
        if let command = ConversationAction(command: action) {
            guard let selected else { return false }
            switch command {
            case .archive: return store.canArchive(selected)
            case .toggleWaiting: return store.tagging.canMarkWaiting(selected)
            default: return true
            }
        }
        switch action {
        case #selector(goBack(_:)): return canGoBack
        case #selector(retagAll(_:)): return store.tagging.isEnabled
        default: return true
        }
    }

    /// What the selection's toggles act on, for their titles and symbols.
    private var selectedState: ConversationAction.State {
        ConversationAction.State(selected, tag: selected.flatMap(store.tagging.tag(for:)))
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        guard let action = menuItem.action else { return true }
        if let command = ConversationAction(command: action) {
            menuItem.title = command.label(in: selectedState).title
        } else if action == #selector(toggleAITagging(_:)) {
            menuItem.state = store.tagging.isEnabled ? .on : .off
        }
        return isEnabled(action)
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        guard let action = item.action else { return true }
        if let command = ConversationAction(command: action) {
            let label = command.label(in: selectedState)
            if item.label != label.title {
                item.label = label.title
                item.image = NSImage(systemSymbolName: label.symbol, accessibilityDescription: label.title)
            }
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
        default:
            guard let action = Self.toolbarActions[identifier] else { return nil }
            let label = action.label()
            return button(identifier, label.title, label.symbol, action.command)
        }
    }

    private static let toolbarActions: [NSToolbarItem.Identifier: ConversationAction] = [
        .archive: .archive, .trash: .trash, .read: .toggleRead, .star: .toggleStar, .reply: .reply,
        .replyAll: .replyAll, .forward: .forward,
    ]

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

extension ConversationAction {
    /// The window command performing this action, which the main menu and the toolbar send.
    var command: Selector {
        switch self {
        case .archive: #selector(MainWindowController.archive(_:))
        case .trash: #selector(MainWindowController.trash(_:))
        case .toggleRead: #selector(MainWindowController.toggleRead(_:))
        case .toggleStar: #selector(MainWindowController.toggleStar(_:))
        case .toggleWaiting: #selector(MainWindowController.toggleWaiting(_:))
        case .reply: #selector(MainWindowController.reply(_:))
        case .replyAll: #selector(MainWindowController.replyAll(_:))
        case .forward: #selector(MainWindowController.forward(_:))
        }
    }

    init?(command: Selector) {
        guard let action = Self.allCases.first(where: { $0.command == command }) else { return nil }
        self = action
    }
}
