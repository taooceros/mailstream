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
    private var bccRow: NSView?
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
        fromPopup.isBordered = false
        fromPopup.font = .systemFont(ofSize: 13)
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
        subjectField.font = .systemFont(ofSize: 15, weight: .semibold)
        subjectField.placeholderString = "Subject"
        subjectField.delegate = self

        let bccToggle = NSButton(title: "Bcc", target: self, action: #selector(toggleBcc))
        bccToggle.bezelStyle = .inline
        bccToggle.controlSize = .small
        bccToggle.toolTip = "Show Bcc"

        bccRow = headerRow("Bcc", bccField)
        bccRow?.isHidden = true
        let header = NSStackView(views: [
            headerRow("From", fromPopup),
            headerRow("To", toField),
            headerRow("Cc", ccField, trailing: bccToggle),
            bccRow!,
            headerRow(nil, subjectField, height: 44),
        ])
        header.orientation = .vertical
        header.spacing = 0
        for row in header.arrangedSubviews { row.widthAnchor.constraint(equalTo: header.widthAnchor).isActive = true }

        bodyView.isRichText = false
        bodyView.allowsUndo = true
        bodyView.font = .systemFont(ofSize: 14)
        // Text lines up with the header's labels (the container pads 5 pt).
        bodyView.textContainerInset = NSSize(width: Self.margin - 5, height: 14)
        bodyView.isAutomaticQuoteSubstitutionEnabled = false
        bodyView.isAutomaticDashSubstitutionEnabled = false
        bodyView.isContinuousSpellCheckingEnabled = true
        // The system's grey inline predictions, whatever the user's keyboard setting; Tab takes one.
        bodyView.inlinePredictionType = .yes
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
        attachmentsStack.edgeInsets = NSEdgeInsets(top: 8, left: Self.margin, bottom: 10, right: Self.margin)
        attachmentsStack.isHidden = true

        progress.style = .spinning
        progress.controlSize = .small
        progress.isDisplayedWhenStopped = false

        let content = NSView()
        for view in [header, scroll, attachmentsStack, progress] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: content.safeAreaLayoutGuide.topAnchor, constant: 4),
            header.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor),
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

        let toolbar = NSToolbar(identifier: "ComposeToolbar")
        toolbar.delegate = toolbarDelegate
        toolbar.displayMode = .iconOnly
        let attach = NSToolbarItem(itemIdentifier: .init("attach"))
        attach.image = NSImage(systemSymbolName: "paperclip", accessibilityDescription: "Attach")
        attach.label = "Attach"
        attach.toolTip = "Attach Files (or drop them on the message)"
        attach.target = self
        attach.action = #selector(self.attach)
        sendItem.image = NSImage(systemSymbolName: "paperplane.fill", accessibilityDescription: "Send")
        sendItem.label = "Send"
        sendItem.toolTip = "Send (⌘↩)"
        sendItem.style = .prominent
        sendItem.autovalidates = false
        sendItem.target = self
        sendItem.action = #selector(send)
        toolbarDelegate.items = [attach.itemIdentifier: attach, sendItem.itemIdentifier: sendItem]
        window.toolbar = toolbar
    }

    /// Leading inset of the header's labels and the body's text.
    private static let margin: CGFloat = 20
    private let sendItem = NSToolbarItem(itemIdentifier: .init("send"))
    private let toolbarDelegate = StaticToolbarDelegate(order: [.flexibleSpace, .init("attach"), .init("send")])

    /// One header line: a label, the field and an optional trailing control over a hairline.
    private func headerRow(_ title: String?, _ field: NSView, trailing: NSView? = nil, height: CGFloat = 34) -> NSView {
        let row = NSView()
        let separator = NSBox()
        separator.boxType = .separator
        var views: [NSView] = [field, separator]
        var constraints = [
            row.heightAnchor.constraint(equalToConstant: height),
            field.centerYAnchor.constraint(equalTo: row.centerYAnchor),
            separator.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: Self.margin),
            separator.trailingAnchor.constraint(equalTo: row.trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: row.bottomAnchor),
        ]
        if let title {
            let label = NSTextField(labelWithString: title + ":")
            label.textColor = .secondaryLabelColor
            label.font = .systemFont(ofSize: 13)
            label.alignment = .right
            views.append(label)
            constraints += [
                label.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: Self.margin),
                label.widthAnchor.constraint(equalToConstant: 52),
                label.firstBaselineAnchor.constraint(equalTo: field.firstBaselineAnchor),
                field.leadingAnchor.constraint(equalTo: label.trailingAnchor, constant: 8),
            ]
        } else {
            // Same start as the body text (the field's cell pads 2 pt).
            constraints.append(field.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: Self.margin - 2))
        }
        if let trailing {
            views.append(trailing)
            constraints += [
                trailing.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -Self.margin),
                trailing.centerYAnchor.constraint(equalTo: row.centerYAnchor),
                field.trailingAnchor.constraint(equalTo: trailing.leadingAnchor, constant: -8),
            ]
        } else if field === fromPopup {
            constraints.append(field.trailingAnchor.constraint(lessThanOrEqualTo: row.trailingAnchor, constant: -Self.margin))
        } else {
            constraints.append(field.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -Self.margin))
        }
        for view in views {
            view.translatesAutoresizingMaskIntoConstraints = false
            row.addSubview(view)
        }
        NSLayoutConstraint.activate(constraints)
        return row
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
        for (index, url) in attachments.enumerated() { attachmentsStack.addArrangedSubview(chip(for: url, index: index)) }
        attachmentsStack.isHidden = attachments.isEmpty
    }

    /// A rounded chip: the file's icon, name and size, and a button removing it.
    private func chip(for url: URL, index: Int) -> NSView {
        let icon = NSImageView(image: NSWorkspace.shared.icon(forFile: url.path))
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let name = NSTextField(labelWithString: url.lastPathComponent)
        name.font = .systemFont(ofSize: 12)
        name.lineBreakMode = .byTruncatingMiddle
        name.widthAnchor.constraint(lessThanOrEqualToConstant: 180).isActive = true
        var views: [NSView] = [icon, name]
        if let bytes = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize {
            let size = NSTextField(labelWithString: ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file))
            size.font = .systemFont(ofSize: 11)
            size.textColor = .secondaryLabelColor
            views.append(size)
        }
        let remove = NSButton(image: NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Remove")!,
                              target: self, action: #selector(removeAttachment(_:)))
        remove.isBordered = false
        remove.contentTintColor = .tertiaryLabelColor
        remove.tag = index
        remove.toolTip = "Remove \(url.lastPathComponent)"
        views.append(remove)
        let stack = NSStackView(views: views)
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 5, left: 8, bottom: 5, right: 6)
        let chip = ChipBackground()
        stack.translatesAutoresizingMaskIntoConstraints = false
        chip.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: chip.topAnchor),
            stack.bottomAnchor.constraint(equalTo: chip.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: chip.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: chip.trailingAnchor),
        ])
        return chip
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
        for control in [toField, ccField, bccField, subjectField] as [NSControl] { control.isEnabled = enabled }
        sendItem.isEnabled = enabled
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

    func textDidChange(_ notification: Notification) { edited = true }

    // MARK: - Recipient completion

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

    /// Asks before throwing away what was written; Return discards, Escape keeps editing.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard edited, !sending else { return true }
        let alert = NSAlert()
        let kind = switch mode {
        case .new: "message"
        case .reply: "reply"
        case .forward: "forward"
        }
        alert.messageText = "Discard this \(kind)?"
        var summary: [String] = []
        let people = [toField, ccField, bccField].flatMap(tokens).compactMap(Address.parse)
        if let first = people.first {
            let others = people.count - 1
            summary.append("To \(first.displayName)" + (others == 0 ? "" : " and \(others) other\(others == 1 ? "" : "s")"))
        }
        if !attachments.isEmpty { summary.append("\(attachments.count) attachment\(attachments.count == 1 ? "" : "s")") }
        alert.informativeText = (summary.isEmpty ? "" : summary.joined(separator: " · ") + "\n\n")
            + "It isn't kept as a draft, so it can't be brought back."
        alert.icon = NSImage(systemSymbolName: "trash.circle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 56, weight: .regular)
                .applying(NSImage.SymbolConfiguration(paletteColors: [.white, .systemRed])))
        // Discard is the default button, so Return takes it: closing already asked for it, and a
        // button marked destructive never answers Return. The red icon carries the warning.
        alert.addButton(withTitle: "Discard").keyEquivalent = "\r"
        alert.addButton(withTitle: "Keep Editing").keyEquivalent = "\u{1b}"
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

/// A rounded, hairline-bordered fill behind an attachment chip, following light and dark mode.
private final class ChipBackground: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        guard let layer else { return }
        layer.cornerRadius = 8
        layer.borderWidth = 1
        layer.borderColor = NSColor.separatorColor.cgColor
        layer.backgroundColor = NSColor.controlBackgroundColor.cgColor
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
