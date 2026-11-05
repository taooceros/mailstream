import AppKit
import Observation

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
    /// Looks up the suggested client before the client sheet shows, so a second Add can't queue another.
    private var clientLookup: Task<Void, Never>?
    /// The status line: progress, the last result or the last error.
    private var status: (text: String, isError: Bool)?
    /// What the table shows: `accounts.records` as of the last reload.
    private var rows: [AccountRecord]
    private var accountWatch: Task<Void, Never>?

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
        accountWatch = Task.immediate(name: "Accounts window") { [weak self, accounts] in
            for await _ in Observations({ accounts.records }).dropFirst() { self?.reloadRows() }
        }
        updateControls()
    }

    isolated deinit { accountWatch?.cancel() }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Layout

    private func buildUI() {
        guard let content = window?.contentView else { return }

        let title = NSTextField.label("Accounts", font: Design.font(.title, weight: .semibold))
        let subtitle = NSTextField(wrappingLabelWithString:
            "Mailstream signs in to Google and Microsoft in your browser, with an OAuth client you registered. Sign-ins stay in your Keychain.")
        subtitle.font = Design.font(.caption)
        subtitle.textColor = Design.Color.muted.color

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
        let barSeparator = NSBox.separator()
        let buttonDivider = NSBox.separator()
        let bar = NSStackView(views: [addButton, buttonDivider, removeButton])
        bar.orientation = .horizontal
        bar.spacing = 0
        bar.edgeInsets = NSEdgeInsets(top: 0, left: 4, bottom: 0, right: 4)
        buttonDivider.heightAnchor.constraint(equalToConstant: 14).isActive = true

        listBox.addConstrained([scroll, emptyState, barSeparator, bar])

        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        statusField.font = Design.font(.caption)
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

        content.addConstrained([title, subtitle, listBox, statusRow])
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
        icon.contentTintColor = Design.Color.faint.color
        let heading = NSTextField.label("No Accounts", font: Design.font(.headline, weight: .semibold))
        let detail = NSTextField.label("Sign in to bring in mail, contacts and Google Chat.", font: Design.font(.caption),
                                       color: .muted)
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
            listBox.layer?.styleAsCard(radius: .card)
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
        statusField.textColor = status?.isError == true ? Design.Color.danger.color : Design.Color.muted.color
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
    private func reloadRows() {
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

    /// Asks for the OAuth client (prefilled with the last one used, else the build's), then signs in.
    private func startSignIn(_ provider: Provider) {
        guard signIn == nil, clientLookup == nil else { return }
        clientLookup = Task { [weak self, accounts] in
            let suggested = (try? await accounts.tokens.suggestedClient(for: provider)) ?? nil
            guard let self else { return }
            clientLookup = nil
            askForClient(provider, suggested: suggested)
        }
    }

    private func askForClient(_ provider: Provider, suggested: OAuthClient?) {
        guard signIn == nil, let window else { return }
        let idField = NSTextField(string: suggested?.id ?? "")
        idField.placeholderString = provider == .google ? "Client ID" : "Application (client) ID"
        let secretField = NSSecureTextField(string: suggested?.secret ?? "")
        secretField.placeholderString = provider == .google ? "Client secret" : "Client secret (empty for a public client)"
        let grid = NSGridView(views: [
            [NSTextField(labelWithString: "Client ID:"), idField],
            [NSTextField(labelWithString: "Client secret:"), secretField],
        ])
        grid.rowSpacing = 8
        grid.columnSpacing = 8
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .none
        for index in 0..<grid.numberOfRows { grid.row(at: index).yPlacement = .center }
        idField.widthAnchor.constraint(equalToConstant: 260).isActive = true
        secretField.widthAnchor.constraint(equalToConstant: 260).isActive = true
        grid.frame.size = grid.fittingSize

        let title = provider == .google ? "Sign in with Google" : "Sign in with Microsoft"
        let detail = switch provider {
        case .google:
            "Enter the client ID and secret of your Google Cloud OAuth client (type Desktop app). "
                + "Mailstream then opens Google's sign-in in your browser."
        case .microsoft:
            "Enter the application ID of your Microsoft Entra app, and its secret unless it is a public client. "
                + "Its redirect URI must be \(OAuth.redirect)."
        }
        window.confirm(title, detail: detail, confirm: "Continue in Browser", prepare: { alert in
            alert.accessoryView = grid
            alert.window.initialFirstResponder = idField.stringValue.isEmpty ? idField : secretField
        }) { [weak self] in
            guard let self else { return }
            let id = idField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            let secret = secretField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if id.isEmpty {
                status = ("Enter your OAuth client's ID to sign in.", true)
            } else if provider == .google && secret.isEmpty {
                // Google redeems a desktop client's code only with its secret.
                status = ("Google needs the client secret too; the Cloud Console shows it next to the client ID.", true)
            } else {
                beginSignIn(provider, client: OAuthClient(id: id, secret: secret.isEmpty ? nil : secret))
                return
            }
            updateControls()
        }
    }

    private func beginSignIn(_ provider: Provider, client: OAuthClient) {
        guard signIn == nil else { return }
        status = ("Waiting for the browser…", false)
        signIn = Task(name: "Sign-in") { [weak self, accounts] in
            do {
                let record = try await accounts.signIn(provider, client: client)
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
            // The account observation reloads on a later turn; the new row is needed now.
            reloadRows()
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
        window.confirm("Remove \(record.label)?",
                       detail: "Mailstream signs out of \(record.id) and deletes its saved sign-in. "
                           + "Mail on the server is not affected; you can add the account again later.",
                       confirm: "Remove", destructive: true) { [weak self] in
            guard let self else { return }
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
    private let address = NSTextField.label(font: Design.font(.caption), color: .muted)
    private let services = NSTextField.label(font: Design.font(.caption), color: .faint)
    private let separator = NSBox.separator()

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        let label = NSTextField.label(font: Design.font(.body, weight: .semibold))
        label.lineBreakMode = .byTruncatingTail
        label.focusRingType = .none
        textField = label
        address.lineBreakMode = .byTruncatingMiddle
        services.alignment = .right
        services.setContentCompressionResistancePriority(.required, for: .horizontal)
        services.setContentHuggingPriority(.required, for: .horizontal)
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        address.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let text = NSStackView(views: [label, address])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        addConstrained([badge, text, services, separator])
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
