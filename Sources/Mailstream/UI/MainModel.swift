import AppKit
import Observation
import SwiftUI

/// The main window's state and flows: the list, the reader layer above it, back navigation,
/// search, alerts and the commands the menu and toolbar send. Created once with the store, before
/// the window, so the reader's web view is warm by the first open.
@Observable
final class MainModel {
    @ObservationIgnored let store: MailStore
    @ObservationIgnored let list: ConversationListModel
    @ObservationIgnored let reader: ReaderModel

    /// What the reader shows; nil while the list is showing.
    private(set) var opened: ReaderTarget?
    /// Conversation under the list's keyboard highlight.
    private(set) var highlighted: Conversation?
    /// The open bundle, mirrored for observation (titles, toolbar).
    private(set) var openBundle: MailCategory?
    /// Bumped per open/close so a late "page ready" callback cannot re-present a stale reader.
    @ObservationIgnored private var navigation = 0
    /// Bumped per reader animation, so a superseded one does not clean up under its successor.
    @ObservationIgnored private var transitions = 0
    /// Shows the reader being opened, until it has slid in; a swipe calls it to skip the wait.
    @ObservationIgnored private var presentReader: (() -> Void)?

    // MARK: The reader layer, as MainView draws it

    /// The reader is on screen (above the list); false is the old `isHidden`.
    private(set) var readerShown = false
    private(set) var readerOpacity: Double = 1
    private(set) var readerScale: CGFloat = 1
    private(set) var readerOffset: CGFloat = 0
    /// The page shadow while the reader is under the fingers or sliding after them.
    private(set) var readerLifted = false
    private(set) var listScale: CGFloat = 1
    /// Whether a back swipe has the reader under the user's fingers.
    @ObservationIgnored private var dragging = false
    @ObservationIgnored private var swipe = BackSwipe()

    // MARK: Window

    /// The main window, once SwiftUI has made it; the monitors and sheets check against it.
    @ObservationIgnored private(set) weak var window: NSWindow?
    /// The content area's frame (list and reader), in SwiftUI's window coordinates (top-left).
    @ObservationIgnored var pageFrame: CGRect = .zero
    /// A text view (search, a field editor, compose-like fields) holds keyboard focus.
    private(set) var firstResponderIsText = false
    @ObservationIgnored private var responderWatch: NSKeyValueObservation?
    /// The main window is key: its commands are live (they used to reach it through the responder
    /// chain, so another key window disabled them).
    private(set) var isKeyWindow = false
    @ObservationIgnored private var keyWatch: Task<Void, Never>?
    /// Reopens the main window after it was closed.
    @ObservationIgnored var openMainWindow: (() -> Void)?

    // MARK: Search

    var searchText = ""
    var searchPresented = false

    // MARK: Alerts

    /// The store problem on screen, if any.
    var alert: StoreProblem?
    /// Store alerts not shown yet, oldest first; shown one at a time.
    @ObservationIgnored private var pendingAlerts: [StoreProblem] = []
    /// The conversation awaiting the permanent-delete confirmation.
    var pendingDelete: Conversation?

    @ObservationIgnored private var storeWatches: [Task<Void, Never>] = []
    @ObservationIgnored private var monitors: [Any] = []

    init(store: MailStore) {
        self.store = store
        list = ConversationListModel(store: store)
        reader = ReaderModel(store: store)

        list.onHighlight = { [weak self] conversation in self?.highlighted = conversation }
        list.onOpen = { [weak self] conversation in self?.open(.conversation(conversation)) }
        list.onOpenChat = { [weak self] key in self?.open(.chat(key)) }
        list.onPerform = { [weak self] action, conversation in self?.perform(action, on: conversation) }
        list.onBundleChange = { [weak self] in self?.openBundle = self?.list.openBundle }
        reader.onCompose = { [store] mode, replyAll, message, text, plan in
            ComposeWindowController.open(store: store, mode: mode, account: message.ref.account, source: message,
                                         replyAll: replyAll, text: text, plan: plan)
        }
        reader.onComposeTo = { [weak self] address in self?.compose(to: [address]) }

        let problems = store.problems
        storeWatches = [
            Task.immediate(name: "Window list") { [weak self, store] in
                for await _ in Observations({ _ = (store.listContext, store.conversations, store.searchQuery) }) {
                    self?.listChanged()
                }
            },
            Task.immediate(name: "Window alerts") { [weak self] in
                for await _ in Observations({ _ = problems.alerts }) {
                    guard let self else { return }
                    pendingAlerts += problems.takeAlerts()
                    presentAlert()
                }
            },
        ]
        installMonitors()
    }

    isolated deinit {
        storeWatches.forEach { $0.cancel() }
        monitors.forEach(NSEvent.removeMonitor)
        keyWatch?.cancel()
    }

    /// SwiftUI made (or remade) the window: keep its frame under the old autosave name and follow
    /// its first responder for the Trash command's text-editing rule.
    func attach(_ window: NSWindow) {
        guard window !== self.window else { return }
        self.window = window
        window.setFrameAutosaveName("MainWindow")
        responderWatch = window.observe(\.firstResponder, options: [.initial, .new]) { [weak self] window, _ in
            MainActor.assumeIsolated {
                self?.firstResponderIsText = window.firstResponder is NSTextView
            }
        }
        isKeyWindow = window.isKeyWindow
        keyWatch?.cancel()
        keyWatch = Task { [weak self, weak window] in
            let center = NotificationCenter.default
            let changes = center.notifications(named: NSWindow.didBecomeKeyNotification)
            for await note in changes {
                guard let self, let window else { return }
                self.isKeyWindow = (note.object as? NSWindow) === window
            }
        }
        presentAlert()
    }

    // MARK: - Selection

    /// What toolbar and menu commands act on: the open conversation, else the highlighted row.
    /// Chat has none, so every mail command validates to disabled there.
    var selected: Conversation? { store.selection.isChat ? nil : opened?.conversation ?? highlighted }

    /// Account new messages default to: the selected conversation's, then the selected mailbox's.
    var preferredAccount: String? {
        if let selected { return selected.account }
        if case .mailbox(let key) = store.selection { return key.account }
        return store.accounts.first?.name
    }

    /// What the selection's toggles act on, for their titles and symbols.
    var selectedState: ConversationAction.State {
        ConversationAction.State(selected, tag: selected.flatMap(store.tagging.tag(for:)))
    }

    var canGoBack: Bool { opened != nil || openBundle != nil }

    /// A text field or the inline reply has keyboard focus: ⌘⌫ belongs to the text.
    var isEditingText: Bool { firstResponderIsText || (opened != nil && reader.isReplyFocused) }

    func isEnabled(_ action: ConversationAction) -> Bool {
        guard let selected else { return false }
        switch action {
        case .archive: return store.canArchive(selected)
        case .restore: return store.canRestore(selected)
        case .toggleWaiting: return store.tagging.canMarkWaiting(selected)
        case .trash: return !isEditingText
        default: return true
        }
    }

    // MARK: - Titles

    /// The selection's name, and its unread count, search results or loading state.
    func titles() -> (title: String, subtitle: String) {
        let unread = store.selectionRole == .drafts ? 0 : store.badgeCount(store.selection)
        let title = openBundle?.title ?? store.selectionTitle
        if store.searchQuery != nil {
            return (title, store.isLoading ? "Searching…" : "\(store.conversations.count) results")
        } else if store.selection.isChat {
            return (title, store.chat.isLoading && store.chat.rows.isEmpty ? "Loading…" : (unread > 0 ? "\(unread.formatted()) unread" : ""))
        } else if store.isLoading && store.conversations.isEmpty {
            return (title, "Loading…")
        } else if openBundle != nil {
            let counts = list.bundleCounts
            return (title, "\(counts.total.formatted()) conversations" + (counts.unread > 0 ? " · \(counts.unread.formatted()) unread" : ""))
        } else {
            return (title, unread > 0 ? "\(unread.formatted()) unread" : "")
        }
    }

    // MARK: - Navigation

    /// Opens a conversation (marking it read) or a chat space (the store fetches its newest
    /// messages and marks it read) in the reader.
    func open(_ target: ReaderTarget) {
        let wasOpen = opened != nil
        opened = target
        if let conversation = target.conversation { highlighted = conversation }
        navigation += 1
        let current = navigation
        var presented = false
        let present = { [weak self] in
            guard let self, !presented, self.navigation == current else { return }
            presented = true
            self.presentReader = nil
            if !wasOpen { self.swap(forward: true) }
            self.reader.focus()
        }
        presentReader = present
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
    }

    private func closeReader() {
        leaveReader { swap(forward: false) { [weak self] in self?.reader.show(nil) } }
    }

    /// Back to the list; `animation` takes the reader away once the row is highlighted, before the
    /// list takes focus.
    private func leaveReader(_ animation: () -> Void) {
        guard let last = opened else { return }
        opened = nil
        presentReader = nil
        navigation += 1
        switch last {
        case .conversation(let conversation):
            list.highlight(conversation)
        case .chat(let key):
            store.chat.close()
            list.highlight(space: key)
        }
        animation()
        list.focus()
    }

    /// The reader is an opaque layer above the list, which stays shown beneath it so leaving never
    /// waits on the list redrawing: it fades in spreading out from the middle on open, and fades out
    /// shrinking back toward it on close.
    private func swap(forward: Bool, completion: (() -> Void)? = nil) {
        transitions += 1
        let current = transitions
        dragging = false
        var reset = Transaction()
        reset.disablesAnimations = true
        withTransaction(reset) {
            readerShown = true
            readerLifted = false
            readerOffset = 0
            if forward {
                readerOpacity = 0
                readerScale = 0.94
            } else {
                listScale = 1.03
            }
        }
        list.hoverSuspended = true
        if forward {
            withAnimation(Self.unfold) { readerScale = 1 }
        } else {
            withAnimation(Self.unfold) { listScale = 1 }
            withAnimation(.house(.brief)) { readerScale = 0.96 }
        }
        withAnimation(.house(forward ? .standard : .brief)) {
            readerOpacity = forward ? 1 : 0
        } completion: { [weak self] in
            guard let self, self.transitions == current else { return }
            if !forward { self.hideReader() }
            completion?()
        }
    }

    /// The reader leaves the screen; the row under the pointer only now gets its hover.
    private func hideReader() {
        var reset = Transaction()
        reset.disablesAnimations = true
        withTransaction(reset) {
            readerShown = false
            readerOpacity = 1
            readerScale = 1
            readerOffset = 0
            readerLifted = false
        }
        // The list last saw the pointer before the reader covered it; that spot is stale now, so
        // hover waits for the pointer's next move instead of lighting a row it may have left.
        list.pointerMoved(nil)
        list.hoverSuspended = false
    }

    /// `Motion.unfold`'s spring.
    private static let unfold = Animation.interpolatingSpring(mass: 1, stiffness: 300, damping: 32)

    func goBack() {
        if opened != nil {
            closeReader()
        } else {
            list.leaveBundle()
        }
    }

    // MARK: - Escape, mouse Back and swipe back

    // The one sanctioned AppKit island besides the web and text views: SwiftUI has no interactive
    // trackpad swipe-back on macOS and no window-wide key hook that sees Escape inside web content,
    // so local `NSEvent` monitors read Escape, the mouse's Back button and two-finger scrolls.
    private func installMonitors() {
        // Event monitors call back on the main thread.
        if let escape = NSEvent.addLocalMonitorForEvents(matching: .keyDown, handler: { [weak self] event in
            self?.handleEscape(event) == true ? nil : event
        }) { monitors.append(escape) }
        if let back = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .otherMouseDown], handler: { [weak self] event in
            self?.handleBackInput(event) == true ? nil : event
        }) { monitors.append(back) }
    }

    /// Escape goes back from the reader or a bundle, wherever focus is (web content included).
    private func handleEscape(_ event: NSEvent) -> Bool {
        guard event.keyCode == 53, let window, event.window === window, window.attachedSheet == nil,
              canGoBack, !(window.firstResponder is NSTextView) else { return false }
        if opened != nil, reader.handleEscape() { return true }
        goBack()
        return true
    }

    private func handleBackInput(_ event: NSEvent) -> Bool {
        guard let window, event.window === window, window.attachedSheet == nil else { return false }
        if event.type == .otherMouseDown {
            guard event.buttonNumber == 3, canGoBack else { return false }
            goBack()
            return true
        }
        // The swipe takes back whichever page is on top: the reader, else the open bundle. Both fill
        // the content area.
        let height = window.contentView?.bounds.height ?? 0
        let point = CGPoint(x: event.locationInWindow.x, y: height - event.locationInWindow.y)
        let canStart = canGoBack && NSEvent.isSwipeTrackingFromScrollEventsEnabled && pageFrame.contains(point)
        switch swipe.feed(event, canStart: canStart) {
        case .pass:
            return false
        case .consume:
            return true
        case .drag(let offset):
            if opened != nil { dragReader(to: offset) } else { list.dragBack(to: offset) }
            return true
        case .release(let offset, let velocity):
            if dragging { releaseReader(at: offset, velocity: velocity) } else { list.releaseBack(at: offset, velocity: velocity) }
            // The view under the fingers saw this gesture begin: let it see the end too, or a
            // scroll view stays in a live scroll (hover suspended) until the next one.
            return false
        }
    }

    /// The reader follows the fingers sideways like a page, uncovering the list where it lies.
    private func dragReader(to offset: CGFloat) {
        guard opened != nil else { return }
        var hold = Transaction()
        hold.disablesAnimations = true
        if !dragging {
            // Swiped before the reader arrived: bring it up now, under the fingers, so a quick
            // swipe right after opening still goes back.
            presentReader?()
            dragging = true
            // An open still settling must not keep animating under the fingers.
            transitions += 1
            withTransaction(hold) {
                readerShown = true
                readerOpacity = 1
                readerScale = 1
                listScale = 1
                readerLifted = true
            }
        }
        withTransaction(hold) { readerOffset = offset }
    }

    /// The reader slides off the side it was swiped toward and the list stays, or it slides back
    /// into place (see `BackSwipe.settle`). The slide starts at the fingers' speed.
    private func releaseReader(at offset: CGFloat, velocity: CGFloat) {
        guard dragging else { return }
        dragging = false
        let (back, target) = BackSwipe.settle(offset: offset, velocity: velocity, width: pageFrame.width,
                                              canGoBack: opened != nil)
        transitions += 1
        let current = transitions
        // `Motion.slide`: critically damped, starting at the fingers' speed in units of the change.
        let initial = target != offset ? max(0, velocity / (target - offset)) : 0
        withAnimation(.interpolatingSpring(mass: 1, stiffness: 440, damping: 42, initialVelocity: initial)) {
            readerOffset = target
        } completion: { [weak self] in
            guard let self, self.transitions == current else { return }
            self.readerLifted = false
            if back {
                self.hideReader()
                self.reader.show(nil)
            }
        }
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
    }

    // MARK: - Alerts

    /// Shows the oldest pending alert once the window is on screen and free of sheets.
    func presentAlert() {
        guard let window, window.isVisible || window.isMiniaturized, window.attachedSheet == nil,
              alert == nil, pendingDelete == nil, !pendingAlerts.isEmpty else { return }
        alert = pendingAlerts.removeFirst()
    }

    /// A sheet ended: the next pending alert follows on the next turn, once it has detached.
    func sheetEnded() {
        Task { [weak self] in self?.presentAlert() }
    }

    // MARK: - Actions

    func perform(_ action: ConversationAction, on conversation: Conversation) {
        let undo = window?.undoManager
        switch action {
        case .archive: store.archive(conversation, undoManager: undo)
        case .trash:
            if store.selectionRole == .trash {
                pendingDelete = conversation
            } else {
                store.trash(conversation, undoManager: undo)
            }
        case .restore: store.restore(conversation, undoManager: undo)
        case .toggleRead: store.setRead(conversation, read: conversation.isUnread)
        case .toggleStar: store.setFlagged(conversation, flagged: !conversation.isFlagged)
        case .toggleWaiting:
            store.tagging.setWaiting(conversation, waiting: store.tagging.tag(for: conversation)?.toRespond != true)
        case .reply: replyInline(all: false, conversation)
        case .replyAll: replyInline(all: true, conversation)
        case .forward: composeForward(conversation)
        }
    }

    /// The menu and toolbar's commands act on the selection.
    func perform(_ action: ConversationAction) {
        if let selected { perform(action, on: selected) }
    }

    func confirmPermanentDelete() {
        guard let conversation = pendingDelete else { return }
        store.trash(conversation)
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

    func compose(to addresses: [Address] = []) {
        ComposeWindowController.open(store: store, mode: .new, account: preferredAccount, to: addresses)
    }

    func refresh() {
        if store.selection.isChat {
            store.chat.refresh()
            return
        }
        store.refresh()
        store.refreshCounts()
    }

    func focusSearch() { searchPresented = true }

    func toggleAITagging() { store.tagging.isEnabled.toggle() }
    func retagAll() { store.tagging.retagAll() }

    // MARK: - Search

    /// Every keystroke, Return and the cancel button search; a query closes the reader.
    func search(_ query: String) {
        if !query.isEmpty { closeReader() }
        if store.selection.isChat {
            list.chatFilter = query.isEmpty ? nil : query
        } else {
            store.search(query)
        }
    }
}
