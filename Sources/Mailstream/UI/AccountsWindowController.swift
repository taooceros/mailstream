import AppKit

/// The Accounts window: the signed-in accounts in a rounded list (badge in the account's
/// sidebar color, label, address, what it is used for), adding one by browser sign-in,
/// removing, and renaming a label inline (double-click).
@MainActor
final class AccountsWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate,
    NSTextFieldDelegate {
    private static var shared: AccountsWindowController?

    /// Opens the single Accounts window.
    static func show(accounts: AccountStore) {
        if shared?.accounts !== accounts { shared = AccountsWindowController(accounts: accounts) }
        shared?.showWindow(nil)
    }

    private let accounts: AccountStore
    private let table = NSTableView()
    private let listBox = NSView()
    private let emptyState = NSStackView()
    private let addButton = NSButton()
    private let removeButton = NSButton()
    private let spinner = NSProgressIndicator()
    private let statusField = NSTextField(labelWithString: "")
    private let cancelButton = NSButton(title: "Cancel", target: nil, action: nil)
    private var signIn: Task<Void, Never>?
    /// The status line: progress, the last result or the last error.
    private var status: (text: String, isError: Bool)?
    /// What the table shows: `accounts.records` as of the last reload.
    private var rows: [AccountRecord]

    private init(accounts: AccountStore) {
        self.accounts = accounts
        rows = accounts.records
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 420),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.title = "Accounts"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.minSize = NSSize(width: 460, height: 320)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        buildUI()
        window.center()
        NotificationCenter.default.addObserver(self, selector: #selector(accountsChanged),
                                               name: AccountStore.didChange, object: accounts)
        updateControls()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Layout

    private func buildUI() {
        guard let content = window?.contentView else { return }

        let title = NSTextField(labelWithString: "Accounts")
        title.font = .systemFont(ofSize: 20, weight: .semibold)
        let subtitle = NSTextField(wrappingLabelWithString:
            "Mailstream signs in to Google and Microsoft in your browser. Sign-ins stay in your Keychain.")
        subtitle.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        subtitle.textColor = .secondaryLabelColor

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("account"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.dataSource = self
        table.delegate = self
        table.style = .plain
        table.rowHeight = 56
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.gridStyleMask = []
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.target = self
        table.doubleAction = #selector(editLabel)

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder

        listBox.wantsLayer = true
        listBox.layer?.cornerRadius = 10
        listBox.layer?.borderWidth = 1
        listBox.layer?.masksToBounds = true

        configureEmptyState()

        for (button, symbol, tip, action) in [
            (addButton, "plus", "Add an account", #selector(showAddMenu)),
            (removeButton, "minus", "Remove the selected account", #selector(confirmRemove)),
        ] {
            button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tip)
            button.bezelStyle = .smallSquare
            button.isBordered = false
            button.imagePosition = .imageOnly
            button.toolTip = tip
            button.target = self
            button.action = action
            button.widthAnchor.constraint(equalToConstant: 28).isActive = true
        }
        let barSeparator = NSBox()
        barSeparator.boxType = .separator
        let buttonDivider = NSBox()
        buttonDivider.boxType = .separator
        let bar = NSStackView(views: [addButton, buttonDivider, removeButton])
        bar.orientation = .horizontal
        bar.spacing = 0
        bar.edgeInsets = NSEdgeInsets(top: 0, left: 4, bottom: 0, right: 4)
        buttonDivider.heightAnchor.constraint(equalToConstant: 14).isActive = true

        for view in [scroll, emptyState, barSeparator, bar] {
            view.translatesAutoresizingMaskIntoConstraints = false
            listBox.addSubview(view)
        }

        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        statusField.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        statusField.lineBreakMode = .byTruncatingTail
        statusField.isSelectable = true
        statusField.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        cancelButton.bezelStyle = .push
        cancelButton.controlSize = .small
        cancelButton.target = self
        cancelButton.action = #selector(cancelSignIn)
        let statusRow = NSStackView(views: [spinner, statusField, cancelButton])
        statusRow.orientation = .horizontal
        statusRow.spacing = 6

        for view in [title, subtitle, listBox, statusRow] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            title.topAnchor.constraint(equalTo: content.topAnchor, constant: 38),
            title.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 24),
            subtitle.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 4),
            subtitle.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            subtitle.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -24),

            listBox.topAnchor.constraint(equalTo: subtitle.bottomAnchor, constant: 14),
            listBox.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            listBox.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),

            scroll.topAnchor.constraint(equalTo: listBox.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: listBox.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: listBox.trailingAnchor),
            emptyState.centerXAnchor.constraint(equalTo: scroll.centerXAnchor),
            emptyState.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            emptyState.leadingAnchor.constraint(greaterThanOrEqualTo: listBox.leadingAnchor, constant: 20),
            barSeparator.topAnchor.constraint(equalTo: scroll.bottomAnchor),
            barSeparator.leadingAnchor.constraint(equalTo: listBox.leadingAnchor),
            barSeparator.trailingAnchor.constraint(equalTo: listBox.trailingAnchor),
            bar.topAnchor.constraint(equalTo: barSeparator.bottomAnchor),
            bar.leadingAnchor.constraint(equalTo: listBox.leadingAnchor),
            bar.bottomAnchor.constraint(equalTo: listBox.bottomAnchor),
            bar.heightAnchor.constraint(equalToConstant: 26),

            statusRow.topAnchor.constraint(equalTo: listBox.bottomAnchor, constant: 10),
            statusRow.leadingAnchor.constraint(equalTo: listBox.leadingAnchor, constant: 4),
            statusRow.trailingAnchor.constraint(lessThanOrEqualTo: listBox.trailingAnchor),
            statusRow.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16),
            statusRow.heightAnchor.constraint(greaterThanOrEqualToConstant: 20),
        ])
        updateBoxColors()
    }

    private func configureEmptyState() {
        let icon = NSImageView(image: NSImage(systemSymbolName: "person.crop.circle.badge.plus",
                                              accessibilityDescription: nil) ?? NSImage())
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 34, weight: .light)
        icon.contentTintColor = .tertiaryLabelColor
        let heading = NSTextField(labelWithString: "No Accounts")
        heading.font = .systemFont(ofSize: 15, weight: .semibold)
        let detail = NSTextField(labelWithString: "Sign in to bring in mail, contacts and Google Chat.")
        detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        detail.textColor = .secondaryLabelColor
        let google = NSButton(title: "Sign in with Google…", target: self, action: #selector(addGoogle))
        let microsoft = NSButton(title: "Sign in with Microsoft…", target: self, action: #selector(addMicrosoft))
        google.image = ProviderBadge.image(.google, color: NSColor(hex: AccountStyle.hex(at: 0)), side: 16)
        microsoft.image = ProviderBadge.image(.microsoft, color: NSColor(hex: AccountStyle.hex(at: 0)), side: 16)
        for button in [google, microsoft] {
            button.bezelStyle = .push
            button.imagePosition = .imageLeading
            button.imageHugsTitle = true
        }
        let buttons = NSStackView(views: [google, microsoft])
        buttons.spacing = 10
        for view in [icon, heading, detail, buttons] { emptyState.addArrangedSubview(view) }
        emptyState.orientation = .vertical
        emptyState.alignment = .centerX
        emptyState.spacing = 6
        emptyState.setCustomSpacing(16, after: detail)
    }

    private func updateBoxColors() {
        window?.contentView?.effectiveAppearance.performAsCurrentDrawingAppearance {
            listBox.layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
            listBox.layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    private func updateControls() {
        let busy = signIn != nil
        addButton.isEnabled = !busy
        removeButton.isEnabled = !busy && table.selectedRow >= 0
        emptyState.isHidden = !rows.isEmpty
        for case let button as NSButton in emptyState.arrangedSubviews.last?.subviews ?? [] { button.isEnabled = !busy }
        cancelButton.isHidden = !busy
        if busy { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
        let text = status?.text ?? ""
        statusField.stringValue = text
        statusField.toolTip = text.isEmpty ? nil : text
        statusField.textColor = status?.isError == true ? .systemRed : .secondaryLabelColor
    }

    func windowDidChangeEffectiveAppearance(_ notification: Notification) { updateBoxColors() }

    // MARK: - Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        let cell = tableView.makeView(withIdentifier: AccountCell.identifier, owner: self) as? AccountCell ?? {
            let cell = AccountCell()
            cell.textField?.delegate = self
            return cell
        }()
        cell.show(rows[row], color: NSColor(hex: AccountStyle.hex(at: row)), last: row == rows.count - 1)
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) { updateControls() }

    /// Double-click edits the label.
    @objc private func editLabel() {
        let row = table.clickedRow
        guard rows.indices.contains(row), let cell = table.view(atColumn: 0, row: row, makeIfNecessary: false) as? AccountCell,
              let field = cell.textField else { return }
        field.isEditable = true
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    /// A label edited inline.
    func controlTextDidEndEditing(_ obj: Notification) {
        guard let field = obj.object as? NSTextField else { return }
        field.isEditable = false
        let row = table.row(for: field)
        guard rows.indices.contains(row) else { return }
        let record = rows[row]
        if field.stringValue != record.label {
            accounts.rename(record.id, label: field.stringValue)
        }
    }

    /// Reloads the table, keeping the selected account selected.
    @objc private func accountsChanged() {
        let selected = rows.indices.contains(table.selectedRow) ? rows[table.selectedRow].id : nil
        rows = accounts.records
        table.reloadData()
        if let selected, let row = rows.firstIndex(where: { $0.id == selected }) {
            table.selectRowIndexes([row], byExtendingSelection: false)
        }
        updateControls()
    }

    // MARK: - Actions

    @objc private func showAddMenu() {
        let menu = NSMenu()
        let color = NSColor(hex: AccountStyle.hex(at: rows.count))
        for (title, provider, action) in [("Google Account…", Provider.google, #selector(addGoogle)),
                                          ("Microsoft Account…", Provider.microsoft, #selector(addMicrosoft))] {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: "")
            item.target = self
            item.image = ProviderBadge.image(provider, color: color, side: 16)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: addButton.bounds.height + 2), in: addButton)
    }

    @objc private func addGoogle() { startSignIn(.google) }
    @objc private func addMicrosoft() { startSignIn(.microsoft) }

    private func startSignIn(_ provider: Provider) {
        guard signIn == nil else { return }
        status = ("Waiting for the browser…", false)
        signIn = Task { [weak self, accounts] in
            do {
                let record = try await accounts.signIn(provider)
                self?.signInEnded(.success(record))
            } catch {
                self?.signInEnded(.failure(error))
            }
        }
        updateControls()
    }

    private func signInEnded(_ result: Result<AccountRecord, any Error>) {
        signIn = nil
        switch result {
        case .success(let record):
            status = ("Signed in as \(record.id).", false)
            if let row = rows.firstIndex(where: { $0.id == record.id }) {
                table.selectRowIndexes([row], byExtendingSelection: false)
            }
        case .failure(let error as URLError) where error.code == .cancelled:
            status = nil
        case .failure(is CancellationError):
            status = nil
        case .failure(let error):
            status = (error.localizedDescription, true)
        }
        updateControls()
    }

    @objc private func cancelSignIn() {
        signIn?.cancel()
    }

    @objc private func confirmRemove() {
        guard let window, rows.indices.contains(table.selectedRow) else { return }
        let record = rows[table.selectedRow]
        let alert = NSAlert()
        alert.messageText = "Remove \(record.label)?"
        alert.informativeText = "Mailstream signs out of \(record.id) and deletes its saved sign-in. "
            + "Mail on the server is not affected; you can add the account again later."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            status = nil
            accounts.remove(record.id)
        }
    }

    func windowWillClose(_ notification: Notification) {
        signIn?.cancel()
    }
}

/// One account row: badge, label (editable on double-click) over the address, and the
/// services its sign-in covers.
@MainActor
private final class AccountCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("AccountCell")

    private let badge = NSImageView()
    private let address = NSTextField(labelWithString: "")
    private let services = NSTextField(labelWithString: "")
    private let separator = NSBox()

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        let label = NSTextField(labelWithString: "")
        label.font = .systemFont(ofSize: 13, weight: .semibold)
        label.lineBreakMode = .byTruncatingTail
        label.focusRingType = .none
        textField = label
        address.font = .systemFont(ofSize: 11.5)
        address.textColor = .secondaryLabelColor
        address.lineBreakMode = .byTruncatingMiddle
        services.font = .systemFont(ofSize: 11)
        services.textColor = .tertiaryLabelColor
        services.alignment = .right
        services.setContentCompressionResistancePriority(.required, for: .horizontal)
        services.setContentHuggingPriority(.required, for: .horizontal)
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        address.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let text = NSStackView(views: [label, address])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        for view in [badge, text, services] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)
        NSLayoutConstraint.activate([
            separator.leadingAnchor.constraint(equalTo: text.leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            badge.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            badge.centerYAnchor.constraint(equalTo: centerYAnchor),
            badge.widthAnchor.constraint(equalToConstant: 32),
            badge.heightAnchor.constraint(equalToConstant: 32),
            text.leadingAnchor.constraint(equalTo: badge.trailingAnchor, constant: 12),
            text.centerYAnchor.constraint(equalTo: centerYAnchor),
            services.leadingAnchor.constraint(greaterThanOrEqualTo: text.trailingAnchor, constant: 12),
            services.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            services.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func show(_ record: AccountRecord, color: NSColor, last: Bool) {
        separator.isHidden = last
        badge.image = ProviderBadge.image(record.provider, color: color, side: 32)
        textField?.stringValue = record.label
        textField?.isEditable = false
        let provider = record.provider == .google ? "Google" : "Microsoft"
        address.stringValue = record.label == record.id ? provider : "\(record.id) · \(provider)"
        services.stringValue = Self.services(record).joined(separator: " · ")
    }

    private static func services(_ record: AccountRecord) -> [String] {
        let scopes = record.scopes
        func has(_ suffix: String) -> Bool { scopes.contains { $0.hasSuffix(suffix) } }
        var names: [String] = []
        if has("mail.google.com/") || has("Mail.ReadWrite") { names.append("Mail") }
        if has("contacts.readonly") || has("Contacts.Read") { names.append("Contacts") }
        if has("chat.messages.readonly") { names.append("Chat") }
        return names
    }
}

/// A round badge in an account's color carrying its provider's mark: Google's "G" or
/// Microsoft's four squares.
@MainActor
enum ProviderBadge {
    static func image(_ provider: Provider, color: NSColor, side: CGFloat) -> NSImage {
        NSImage(size: NSSize(width: side, height: side), flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect).fill()
            NSColor.white.setFill()
            switch provider {
            case .google:
                let font = NSFont.systemFont(ofSize: side * 0.56, weight: .bold)
                let text = NSAttributedString(string: "G", attributes: [.font: font, .foregroundColor: NSColor.white])
                let size = text.size()
                text.draw(at: NSPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2))
            case .microsoft:
                let square = side * 0.19, gap = side * 0.04
                let origin = NSPoint(x: rect.midX - square - gap / 2, y: rect.midY - square - gap / 2)
                for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    NSRect(x: origin.x + CGFloat(column) * (square + gap), y: origin.y + CGFloat(row) * (square + gap),
                           width: square, height: square).fill()
                }
            }
            return true
        }
    }
}
