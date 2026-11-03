import AppKit

/// New message, reply and forward. Reply and forward prefill the recipients, subject and quoted
/// original so they are visible and editable; what the window shows is what is sent.
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
        controller.window?.makeFirstResponder(to.isEmpty && source == nil ? controller.toField : controller.bodyView)
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
    private let bodyView = NSTextView()
    private let attachmentsStack = NSStackView()
    private let progress = NSProgressIndicator()
    private let sendButton = NSButton()
    private var bccRow: NSGridRow?
    private var attachments: [URL] = []
    private var edited = false
    private var sending = false

    private init(store: MailStore, mode: ComposeMode, account: String, source: MailMessage?,
                 replyAll: Bool, to: [Address], text: String, plan: ReplyPlan?) {
        self.store = store
        self.mode = mode
        self.account = account
        self.source = source
        self.replyAll = replyAll
        self.initialText = text
        self.plan = plan
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 600),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.minSize = NSSize(width: 480, height: 360)
        window.toolbarStyle = .unified
        super.init(window: window)
        window.delegate = self
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
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Layout

    private func buildUI() {
        guard let window else { return }

        for account in store.accounts {
            let title = account.identity.map { "\($0.displayName) <\($0.email)>" } ?? account.name
            fromPopup.addItem(withTitle: title)
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
            field.completionDelay = 0.1
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

        sendButton.title = "Send"
        sendButton.image = NSImage(systemSymbolName: "paperplane.fill", accessibilityDescription: "Send")
        sendButton.imagePosition = .imageLeading
        sendButton.bezelStyle = .push
        sendButton.bezelColor = .controlAccentColor
        sendButton.keyEquivalent = "d"
        sendButton.keyEquivalentModifierMask = [.command, .shift]
        sendButton.target = self
        sendButton.action = #selector(send)
        sendButton.toolTip = "Send (⇧⌘D)"

        let toolbar = NSToolbar(identifier: "ComposeToolbar")
        toolbar.delegate = toolbarDelegate
        toolbar.displayMode = .iconOnly
        toolbarDelegate.items = [
            .init("attach"): {
                let item = NSToolbarItem(itemIdentifier: .init("attach"))
                item.image = NSImage(systemSymbolName: "paperclip", accessibilityDescription: "Attach")
                item.label = "Attach"
                item.toolTip = "Attach Files"
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

    private func prefill() {
        guard let source else {
            bodyView.string = initialText
            return
        }
        progress.startAnimation(nil)
        bodyView.isEditable = false
        Task {
            defer {
                progress.stopAnimation(nil)
                bodyView.isEditable = true
            }
            do {
                let quote: String
                if case .reply = mode {
                    let plan: ReplyPlan
                    if let ready = self.plan { plan = ready } else { plan = try await store.replyPlan(for: source, all: replyAll) }
                    subjectField.stringValue = plan.subject
                    if (toField.objectValue as? [String])?.isEmpty ?? true { toField.objectValue = plan.to.map(\.rfc5322) }
                    ccField.objectValue = plan.cc.map(\.rfc5322)
                    quote = plan.quotedText.hasPrefix(">") ? plan.attribution + "\n" + plan.quotedText : plan.quotedText
                } else {
                    let draft = try await store.forwardDraft(for: source)
                    subjectField.stringValue = draft.subject
                    quote = draft.body
                }
                bodyView.string = initialText + "\n\n" + quote
                let caret = NSRange(location: (initialText as NSString).length, length: 0)
                bodyView.setSelectedRange(caret)
                bodyView.scrollRangeToVisible(NSRange(location: 0, length: 0))
                edited = !initialText.isEmpty
            } catch {
                subjectField.stringValue = source.envelope.subject
                bodyView.string = initialText
                presentError(message: "Could not prepare the reply", error: error)
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
            attachments += panel.urls
            edited = true
            rebuildAttachments()
        }
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

    private func recipients(_ field: NSTokenField) -> [String] {
        let tokens = (field.objectValue as? [Any] ?? []).compactMap { $0 as? String }
        return tokens.flatMap { $0.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) } }
            .filter { !$0.isEmpty }
    }

    @objc private func send() {
        guard !sending else { return }
        let to = recipients(toField), cc = recipients(ccField), bcc = bccRow?.isHidden == false ? recipients(bccField) : []
        let invalid = (to + cc + bcc).filter { Address.parse($0) == nil }
        guard !to.isEmpty || !cc.isEmpty || !bcc.isEmpty else {
            presentError(message: "Add at least one recipient", error: nil)
            return
        }
        guard invalid.isEmpty else {
            presentError(message: "Invalid address: \(invalid.joined(separator: ", "))", error: nil)
            return
        }
        let subject = subjectField.stringValue, text = bodyView.string, files = attachments
        sending = true
        setControlsEnabled(false)
        progress.startAnimation(nil)
        Task {
            do {
                let message = await store.outgoing(account: account, source: source,
                                                   to: to.compactMap(Address.parse), cc: cc.compactMap(Address.parse),
                                                   bcc: bcc.compactMap(Address.parse), subject: subject, text: text,
                                                   attachments: files)
                try await store.send(message, account: account)
                edited = false
                close()
            } catch {
                sending = false
                setControlsEnabled(true)
                progress.stopAnimation(nil)
                presentError(message: "The message could not be sent", error: error)
            }
        }
    }

    private func setControlsEnabled(_ enabled: Bool) {
        for control in [toField, ccField, bccField, subjectField, sendButton] as [NSControl] { control.isEnabled = enabled }
        fromPopup.isEnabled = enabled && { if case .new = mode { true } else { false } }()
        bodyView.isEditable = enabled
    }

    private func presentError(message: String, error: Error?) {
        guard let window else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = message
        alert.informativeText = error?.localizedDescription ?? ""
        alert.beginSheetModal(for: window)
    }

    // MARK: - Edits and closing

    func textDidChange(_ notification: Notification) { edited = true }
    func controlTextDidChange(_ obj: Notification) { edited = true }

    func tokenField(_ tokenField: NSTokenField, completionsForSubstring substring: String, indexOfToken tokenIndex: Int,
                    indexOfSelectedItem selectedIndex: UnsafeMutablePointer<Int>?) -> [Any]? {
        let needle = substring.lowercased()
        guard !needle.isEmpty else { return [] }
        return store.knownAddresses()
            .filter { $0.email.lowercased().hasPrefix(needle) || ($0.name?.lowercased().contains(needle) ?? false) }
            .prefix(12).map(\.rfc5322)
    }

    func tokenField(_ tokenField: NSTokenField, displayStringForRepresentedObject representedObject: Any) -> String? {
        guard let raw = representedObject as? String, let address = Address.parse(raw) else { return nil }
        return address.displayName
    }

    func tokenField(_ tokenField: NSTokenField, editingStringForRepresentedObject representedObject: Any) -> String? {
        representedObject as? String
    }

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
        Self.open.remove(self)
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
