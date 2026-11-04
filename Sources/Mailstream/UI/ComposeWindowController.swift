import AppKit

/// New message, reply and forward. Reply and forward prefill the recipients, subject and quoted
/// original so they are visible and editable; what the window shows is what is sent. The body takes
/// typing at once: the quote is appended under it when the original has loaded.
@MainActor
final class ComposeWindowController: NSWindowController, NSWindowDelegate, NSTokenFieldDelegate, NSTextViewDelegate {
    private static var open: Set<ComposeWindowController> = []

    /// `text` starts the body, above any quote (an inline reply moved into a window keeps its text,
    /// and its `plan` when one was already worked out).
    static func open(store: MailStore, mode: ComposeMode, account: String?, source: MailMessage? = nil,
                     replyAll: Bool = false, to: [Address] = [], text: String = "", plan: ReplyPlan? = nil) {
        guard let account = account ?? store.accounts.first?.name else { return }
        let controller = ComposeWindowController(store: store, mode: mode, account: account, source: source,
                                                 replyAll: replyAll, to: to, text: text, plan: plan)
        open.insert(controller)
        controller.showWindow(nil)
        controller.window?.makeFirstResponder(controller.tokens(controller.toField).isEmpty ? controller.toField : controller.bodyView)
    }

    private let store: MailStore
    private let mode: ComposeMode
    private var account: String
    private let source: MailMessage?
    private let replyAll: Bool
    private let initialText: String
    private let plan: ReplyPlan?

    private let fromPopup = NSPopUpButton()
    private let toField = NSTokenField()
    private let ccField = NSTokenField()
    private let bccField = NSTokenField()
    private let subjectField = NSTextField()
    private let bodyView = ComposeTextView()
    private let attachmentsStack = NSStackView()
    private let progress = NSProgressIndicator()
    private let sendButton = NSButton()
    private var bccRow: NSGridRow?
    private var attachments: [URL] = []
    private var edited = false
    private var sending = false
    /// Whether the user changed To or Cc, which a reply's worked-out recipients then leave alone.
    private var recipientsEdited = false
    /// Loading the original for the quote (and a reply's recipients); Send waits for it.
    private var prefillTask: Task<Void, Never>?
    /// People to complete recipients to, best first; filled just after the window opens.
    private var contacts: [Contact] = []
    /// Completion and editing labels (`Name <address>`) to the token they stand for.
    private var labels: [String: String] = [:]

    private init(store: MailStore, mode: ComposeMode, account: String, source: MailMessage?,
                 replyAll: Bool, to: [Address], text: String, plan: ReplyPlan?) {
        self.store = store
        self.mode = mode
        self.account = account
        self.source = source
        self.replyAll = replyAll
        self.initialText = text
        self.plan = plan
        let window = ComposeWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 600),
                                   styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                                   backing: .buffered, defer: false)
        window.minSize = NSSize(width: 480, height: 360)
        window.toolbarStyle = .unified
        super.init(window: window)
        window.delegate = self
        window.onSend = { [weak self] in self?.send() }
        buildUI()
        toField.objectValue = to.map(\.rfc5322)

        switch mode {
        case .new: window.title = "New Message"
        case .reply: window.title = replyAll ? "Reply All" : "Reply"
        case .forward: window.title = "Forward"
        }
        window.subtitle = source?.envelope.subject ?? ""
        window.center()
        if let parent = NSApp.mainWindow, parent !== window {
            window.setFrameTopLeftPoint(NSPoint(x: parent.frame.midX - 340, y: parent.frame.maxY - 60))
        }
        prefill()
        Task { contacts = await store.contacts() }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Layout

    private func buildUI() {
        guard let window else { return }

        for account in store.accounts {
            fromPopup.addItem(withTitle: account.identity.map(Contact.label) ?? account.name)
            fromPopup.lastItem?.representedObject = account.name
            fromPopup.lastItem?.toolTip = account.name
        }
        if let index = store.accounts.firstIndex(where: { $0.name == account }) { fromPopup.selectItem(at: index) }
        fromPopup.target = self
        fromPopup.action = #selector(accountChanged)
        if case .new = mode {} else { fromPopup.isEnabled = false }

        for field in [toField, ccField, bccField] {
            field.delegate = self
            field.tokenizingCharacterSet = CharacterSet(charactersIn: ",;")
            field.tokenStyle = .rounded
            field.isBordered = false
            field.drawsBackground = false
            field.focusRingType = .none
            field.font = .systemFont(ofSize: 13)
            field.placeholderString = ""
            field.completionDelay = 0.05
        }
        subjectField.isBordered = false
        subjectField.drawsBackground = false
        subjectField.focusRingType = .none
        subjectField.font = .systemFont(ofSize: 13, weight: .medium)
        subjectField.delegate = self

        func label(_ text: String) -> NSTextField {
            let label = NSTextField(labelWithString: text)
            label.textColor = .secondaryLabelColor
            label.alignment = .right
            label.font = .systemFont(ofSize: 13)
            return label
        }
        let ccToggle = NSButton(title: "Bcc", target: self, action: #selector(toggleBcc))
        ccToggle.bezelStyle = .inline
        ccToggle.controlSize = .small

        let grid = NSGridView(views: [
            [label("From:"), fromPopup],
            [label("To:"), toField],
            [label("Cc:"), row(ccField, trailing: ccToggle)],
            [label("Bcc:"), bccField],
            [label("Subject:"), subjectField],
        ])
        grid.rowSpacing = 0
        grid.columnSpacing = 8
        grid.column(at: 0).width = 64
        grid.column(at: 0).xPlacement = .trailing
        for index in 0..<grid.numberOfRows {
            let row = grid.row(at: index)
            row.height = 34
            row.yPlacement = .center
            row.topPadding = 2
            row.bottomPadding = 2
        }
        fromPopup.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        grid.cell(for: fromPopup)?.xPlacement = .leading
        bccRow = grid.row(at: 3)
        bccRow?.isHidden = true

        bodyView.isRichText = false
        bodyView.allowsUndo = true
        bodyView.font = .systemFont(ofSize: 14)
        bodyView.textContainerInset = NSSize(width: 14, height: 14)
        bodyView.isAutomaticQuoteSubstitutionEnabled = false
        bodyView.isAutomaticDashSubstitutionEnabled = false
        bodyView.isContinuousSpellCheckingEnabled = true
        bodyView.isVerticallyResizable = true
        bodyView.isHorizontallyResizable = false
        bodyView.autoresizingMask = [.width]
        bodyView.textContainer?.widthTracksTextView = true
        bodyView.delegate = self
        bodyView.onFiles = { [weak self] urls in self?.add(urls) }
        let scroll = NSScrollView()
        scroll.documentView = bodyView
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        bodyView.drawsBackground = false

        attachmentsStack.orientation = .horizontal
        attachmentsStack.spacing = 6
        attachmentsStack.edgeInsets = NSEdgeInsets(top: 6, left: 14, bottom: 8, right: 14)
        attachmentsStack.isHidden = true

        progress.style = .spinning
        progress.controlSize = .small
        progress.isDisplayedWhenStopped = false

        let separator = NSBox()
        separator.boxType = .separator
        let content = NSView()
        for view in [grid, separator, scroll, attachmentsStack, progress] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            grid.topAnchor.constraint(equalTo: content.safeAreaLayoutGuide.topAnchor, constant: 8),
            grid.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 12),
            grid.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            separator.topAnchor.constraint(equalTo: grid.bottomAnchor, constant: 6),
            separator.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: separator.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: attachmentsStack.topAnchor),
            attachmentsStack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            attachmentsStack.trailingAnchor.constraint(lessThanOrEqualTo: content.trailingAnchor),
            attachmentsStack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            progress.centerXAnchor.constraint(equalTo: scroll.centerXAnchor),
            progress.topAnchor.constraint(equalTo: scroll.topAnchor, constant: 40),
        ])
        window.contentView = content
        // Tab walks the fields top to bottom: To, Cc, (Bcc,) Subject, body.
        window.autorecalculatesKeyViewLoop = true

        sendButton.title = "Send"
        sendButton.image = NSImage(systemSymbolName: "paperplane.fill", accessibilityDescription: "Send")
        sendButton.imagePosition = .imageLeading
        sendButton.bezelStyle = .push
        sendButton.bezelColor = .controlAccentColor
        sendButton.target = self
        sendButton.action = #selector(send)
        sendButton.toolTip = "Send (⌘↩)"

        let toolbar = NSToolbar(identifier: "ComposeToolbar")
        toolbar.delegate = toolbarDelegate
        toolbar.displayMode = .iconOnly
        toolbarDelegate.items = [
            .init("attach"): {
                let item = NSToolbarItem(itemIdentifier: .init("attach"))
                item.image = NSImage(systemSymbolName: "paperclip", accessibilityDescription: "Attach")
                item.label = "Attach"
                item.toolTip = "Attach Files (or drop them on the message)"
                item.target = self
                item.action = #selector(self.attach)
                return item
            }(),
            .init("send"): {
                let item = NSToolbarItem(itemIdentifier: .init("send"))
                item.view = sendButton
                item.label = "Send"
                return item
            }(),
        ]
        window.toolbar = toolbar
    }

    private let toolbarDelegate = StaticToolbarDelegate(order: [.flexibleSpace, .init("attach"), .init("send")])

    private func row(_ field: NSView, trailing: NSView) -> NSView {
        let stack = NSStackView(views: [field, trailing])
        stack.orientation = .horizontal
        stack.spacing = 6
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return stack
    }

    @objc private func toggleBcc() {
        bccRow?.isHidden.toggle()
        if bccRow?.isHidden == false { window?.makeFirstResponder(bccField) }
    }

    @objc private func accountChanged() {
        if let name = fromPopup.selectedItem?.representedObject as? String { account = name }
    }

    // MARK: - Prefill

    /// Fills what is known at once (subject, a reply's recipients from what is loaded) and appends
    /// the quote when the original has loaded, leaving the user's typing and caret alone.
    private func prefill() {
        bodyView.string = initialText
        bodyView.setSelectedRange(NSRange(location: (initialText as NSString).length, length: 0))
        edited = !initialText.isEmpty
        guard let source else { return }
        var fillsRecipients = false
        switch mode {
        case .new:
            return
        case .reply:
            subjectField.stringValue = plan?.subject ?? Subject.reply(to: source.envelope.subject)
            if tokens(toField).isEmpty {
                fillsRecipients = true
                let recipients = plan.map { ($0.to, $0.cc) }
                    ?? MailStore.replyRecipients(for: source, content: store.contents.loaded(source.ref)?.content,
                                                 own: store.ownAddresses, all: replyAll)
                toField.objectValue = recipients.0.map(\.rfc5322)
                ccField.objectValue = recipients.1.map(\.rfc5322)
            }
        case .forward:
            subjectField.stringValue = Subject.forward(source.envelope.subject)
        }
        prefillTask = Task {
            do {
                let quote: String
                if case .reply = mode {
                    let plan: ReplyPlan
                    if let ready = self.plan { plan = ready } else { plan = try await store.replyPlan(for: source, all: replyAll) }
                    if fillsRecipients, !recipientsEdited {
                        toField.objectValue = plan.to.map(\.rfc5322)
                        ccField.objectValue = plan.cc.map(\.rfc5322)
                    }
                    quote = plan.quotedText.hasPrefix(">") ? plan.attribution + "\n" + plan.quotedText : plan.quotedText
                } else {
                    quote = try await store.forwardDraft(for: source).body
                }
                guard !quote.isEmpty else { return }
                // Appended past everything typed, so the selection and undo history stay valid.
                let selection = bodyView.selectedRanges
                bodyView.textStorage?.append(NSAttributedString(string: "\n\n" + quote, attributes: bodyView.typingAttributes))
                bodyView.selectedRanges = selection
            } catch {
                presentError(message: "Could not load the original message", error: error)
            }
        }
    }

    // MARK: - Attachments

    @objc private func attach() {
        guard let window else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let self else { return }
            add(panel.urls)
        }
    }

    private func add(_ urls: [URL]) {
        guard !sending else { return }
        attachments += urls.filter { !attachments.contains($0) }
        edited = true
        rebuildAttachments()
    }

    private func rebuildAttachments() {
        attachmentsStack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for (index, url) in attachments.enumerated() {
            let button = NSButton(title: url.lastPathComponent, image: NSWorkspace.shared.icon(forFile: url.path),
                                  target: self, action: #selector(removeAttachment(_:)))
            button.image?.size = NSSize(width: 16, height: 16)
            button.imagePosition = .imageLeading
            button.bezelStyle = .inline
            button.tag = index
            button.toolTip = "Click to remove \(url.lastPathComponent)"
            attachmentsStack.addArrangedSubview(button)
        }
        attachmentsStack.isHidden = attachments.isEmpty
    }

    @objc private func removeAttachment(_ sender: NSButton) {
        guard sender.tag < attachments.count else { return }
        attachments.remove(at: sender.tag)
        rebuildAttachments()
    }

    // MARK: - Sending

    private func tokens(_ field: NSTokenField) -> [String] {
        (field.objectValue as? [Any] ?? []).compactMap { ($0 as? String)?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    @objc private func send() {
        guard !sending, let window else { return }
        // A recipient still being typed becomes a token first.
        if let editor = window.firstResponder as? NSText, editor.isFieldEditor { window.makeFirstResponder(nil) }
        sending = true
        setControlsEnabled(false)
        progress.startAnimation(nil)
        Task {
            await prefillTask?.value
            let fields = [toField, ccField] + (bccRow?.isHidden == false ? [bccField] : [])
            if let field = fields.first(where: { tokens($0).contains { Address.parse($0) == nil } }) {
                stopSending("Invalid address: \(tokens(field).filter { Address.parse($0) == nil }.joined(separator: ", "))",
                            error: nil, focus: field)
                return
            }
            // Someone listed twice gets one copy, in the first field naming them.
            var seen = Set<String>()
            let lists = fields.map { field in tokens(field).compactMap(Address.parse).filter { seen.insert($0.normalizedEmail).inserted } }
            guard !seen.isEmpty else {
                stopSending("Add at least one recipient", error: nil, focus: toField)
                return
            }
            do {
                let message = await store.outgoing(account: account, source: source, to: lists[0], cc: lists[1],
                                                   bcc: lists.count > 2 ? lists[2] : [], subject: subjectField.stringValue,
                                                   text: bodyView.string, attachments: attachments)
                try await store.send(message, account: account)
                edited = false
                close()
            } catch {
                stopSending("The message could not be sent", error: error, focus: nil)
            }
        }
    }

    private func stopSending(_ message: String, error: (any Error)?, focus field: NSTokenField?) {
        sending = false
        setControlsEnabled(true)
        progress.stopAnimation(nil)
        if let field { window?.makeFirstResponder(field) }
        presentError(message: message, error: error)
    }

    private func setControlsEnabled(_ enabled: Bool) {
        for control in [toField, ccField, bccField, subjectField, sendButton] as [NSControl] { control.isEnabled = enabled }
        fromPopup.isEnabled = enabled && { if case .new = mode { true } else { false } }()
        bodyView.isEditable = enabled
    }

    private func presentError(message: String, error: (any Error)?) {
        guard let window else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = message
        alert.informativeText = error?.localizedDescription ?? ""
        alert.beginSheetModal(for: window)
    }

    // MARK: - Recipient completion

    func textDidChange(_ notification: Notification) { edited = true }

    func controlTextDidChange(_ obj: Notification) {
        edited = true
        if obj.object is NSTokenField { recipientsEdited = true }
    }

    /// Addresses already in a recipient field, which completion leaves out.
    private func takenAddresses() -> Set<String> {
        Set([toField, ccField, bccField].flatMap(tokens).compactMap { Address.parse($0)?.normalizedEmail })
    }

    /// People whose name, a word of it, or address starts with `typed`, best first; an address
    /// typed out in full first of all.
    private func suggestions(for typed: String) -> [Contact] {
        let needle = Contact.fold(typed.trimmingCharacters(in: .whitespaces))
        guard !needle.isEmpty else { return [] }
        let taken = takenAddresses()
        var matches = Array(contacts.lazy.filter { !taken.contains($0.address.normalizedEmail) && $0.matches(needle) }.prefix(12))
        if let exact = matches.firstIndex(where: { Contact.fold($0.address.email) == needle }) {
            matches.insert(matches.remove(at: exact), at: 0)
        }
        return matches
    }

    func tokenField(_ tokenField: NSTokenField, completionsForSubstring substring: String, indexOfToken tokenIndex: Int,
                    indexOfSelectedItem selectedIndex: UnsafeMutablePointer<Int>?) -> [Any]? {
        let shown = suggestions(for: substring).map { contact in
            let label = Contact.label(contact.address)
            labels[label] = contact.address.rfc5322
            return label
        }
        // The token field writes the selected completion over the typed text, which garbles it
        // unless the completion starts with that text; otherwise the list shows with nothing
        // selected, and committing the typed text still picks the first (`shouldAdd`).
        let needle = Contact.fold(substring.trimmingCharacters(in: .whitespaces))
        if let first = shown.first, !Contact.fold(first).hasPrefix(needle) { selectedIndex?.pointee = -1 }
        return shown
    }

    func tokenField(_ tokenField: NSTokenField, representedObjectForEditing editingString: String) -> Any? {
        labels[editingString] ?? editingString
    }

    /// A comma, Return or Tab after a partial name commits the typed text, not the completion
    /// shown: it becomes the best match. An address typed out in full is kept as typed.
    func tokenField(_ tokenField: NSTokenField, shouldAdd tokens: [Any], at index: Int) -> [Any] {
        tokens.map { token in
            guard let typed = token as? String, Address.parse(typed) == nil,
                  let best = suggestions(for: typed).first else { return token }
            return best.address.rfc5322
        }
    }

    func tokenField(_ tokenField: NSTokenField, displayStringForRepresentedObject representedObject: Any) -> String? {
        guard let raw = representedObject as? String, let address = Address.parse(raw) else { return nil }
        return address.displayName
    }

    /// `Name <address>` without the quotes and commas of the stored form, so editing a token and
    /// committing it again gives the same recipient.
    func tokenField(_ tokenField: NSTokenField, editingStringForRepresentedObject representedObject: Any) -> String? {
        guard let raw = representedObject as? String, let address = Address.parse(raw) else { return representedObject as? String }
        let label = Contact.label(address)
        labels[label] = raw
        return label
    }

    // MARK: - Closing

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard edited, !sending else { return true }
        let alert = NSAlert()
        alert.messageText = "Discard this message?"
        alert.informativeText = "Your changes will be lost."
        alert.addButton(withTitle: "Discard")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        alert.beginSheetModal(for: sender) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            self?.edited = false
            sender.close()
        }
        return false
    }

    func windowWillClose(_ notification: Notification) {
        prefillTask?.cancel()
        Self.open.remove(self)
    }
}

/// Sends on ⌘↩, as the inline reply does, and on ⇧⌘D, as Mail does.
private final class ComposeWindow: NSWindow {
    var onSend: (() -> Void)?

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection([.command, .shift, .option, .control])
        let returnKey = event.keyCode == 36 || event.keyCode == 76  // Return, keypad Enter
        if (modifiers == .command && returnKey)
            || (modifiers == [.command, .shift] && event.charactersIgnoringModifiers?.lowercased() == "d") {
            onSend?()
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

/// The message body: files dropped or pasted on it attach instead of inserting their paths.
private final class ComposeTextView: NSTextView {
    var onFiles: (([URL]) -> Void)?

    private func files(on pasteboard: NSPasteboard) -> [URL] {
        pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    }

    override var acceptableDragTypes: [NSPasteboard.PasteboardType] { super.acceptableDragTypes + [.fileURL] }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        files(on: sender.draggingPasteboard).isEmpty ? super.draggingEntered(sender) : .copy
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        files(on: sender.draggingPasteboard).isEmpty ? super.draggingUpdated(sender) : .copy
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        let urls = files(on: sender.draggingPasteboard)
        guard !urls.isEmpty else { return super.performDragOperation(sender) }
        onFiles?(urls)
        return true
    }

    override func paste(_ sender: Any?) {
        let urls = files(on: .general)
        guard !urls.isEmpty else { return super.paste(sender) }
        onFiles?(urls)
    }
}

/// Toolbar delegate over a fixed set of prebuilt items.
@MainActor
final class StaticToolbarDelegate: NSObject, NSToolbarDelegate {
    let order: [NSToolbarItem.Identifier]
    var items: [NSToolbarItem.Identifier: NSToolbarItem] = [:]

    init(order: [NSToolbarItem.Identifier]) { self.order = order }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] { order }
    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] { order }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        items[itemIdentifier]
    }
}
