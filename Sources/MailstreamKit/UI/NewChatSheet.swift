import AppKit

/// The New Chat sheet: the chat account and the people to chat with, completing to the account's
/// chat partners and mail contacts. One person opens the user's direct message with them (Google
/// makes one if there is none), several a group chat.
@MainActor
final class NewChatSheet: NSWindowController {
    private let store: MailStore
    private var account: String
    /// The space to open; the sheet has closed.
    private let onStarted: (ChatSpaceKey) -> Void

    private let completion = RecipientCompletion()
    private let accountPopup = NSPopUpButton()
    private let peopleField = NSTokenField()
    private let note = NSTextField.label(font: Design.font(.footnote), color: .muted)
    private let progress = NSProgressIndicator()
    private let startButton = NSButton(title: "Start Chat", target: nil, action: nil)
    private let cancelButton = NSButton(title: "Cancel", target: nil, action: nil)
    private var starting = false
    /// Why the chat could not start.
    private var failure: String?

    private static let margin: CGFloat = 20

    init(store: MailStore, account: String, onStarted: @escaping (ChatSpaceKey) -> Void) {
        self.store = store
        self.account = account
        self.onStarted = onStarted
        let window = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 460, height: 200), styleMask: [.titled],
                             backing: .buffered, defer: false)
        super.init(window: window)
        buildUI()
        loadContacts()
        update()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// Shows the sheet over `parent` with the people field focused; `onEnd` runs once it closes.
    func begin(over parent: NSWindow, onEnd: @escaping () -> Void) {
        guard let window else { return }
        parent.beginSheet(window) { _ in onEnd() }
        window.makeFirstResponder(peopleField)
    }

    // MARK: - Layout

    private func buildUI() {
        guard let window else { return }
        let title = NSTextField.label("New Chat", font: Design.font(.headline, weight: .semibold))

        for chat in store.chat.accounts {
            accountPopup.addItem(withTitle: store.label(account: chat.name))
            accountPopup.lastItem?.representedObject = chat.name
            accountPopup.lastItem?.toolTip = chat.name
        }
        if let index = store.chat.accounts.firstIndex(where: { $0.name == account }) { accountPopup.selectItem(at: index) }
        accountPopup.target = self
        accountPopup.action = #selector(accountChanged)
        accountPopup.isBordered = false
        accountPopup.font = Design.font(.body)

        completion.taken = { [weak self] in
            guard let self else { return [] }
            return Set(RecipientCompletion.tokens(in: peopleField).compactMap { Address.parse($0)?.normalizedEmail })
        }
        completion.onEdit = { [weak self] _ in
            self?.failure = nil
            self?.update()
        }
        completion.attach(peopleField)

        note.lineBreakMode = .byWordWrapping
        note.maximumNumberOfLines = 0
        note.preferredMaxLayoutWidth = 460 - 2 * Self.margin

        progress.style = .spinning
        progress.controlSize = .small
        progress.isDisplayedWhenStopped = false
        cancelButton.target = self
        cancelButton.action = #selector(cancel)
        cancelButton.keyEquivalent = "\u{1b}"
        startButton.target = self
        startButton.action = #selector(start)
        startButton.keyEquivalent = "\r"
        let buttons = NSStackView(views: [progress, cancelButton, startButton])
        buttons.spacing = 8

        var rows: [NSView] = []
        if store.chat.accounts.count > 1 { rows.append(row("From", accountPopup)) }
        rows.append(row("To", peopleField))
        let fields = NSStackView(views: rows)
        fields.orientation = .vertical
        fields.spacing = 0
        for row in rows { row.widthAnchor.constraint(equalTo: fields.widthAnchor).isActive = true }

        let content = NSView()
        content.addConstrained([title, fields, note, buttons])
        NSLayoutConstraint.activate([
            title.topAnchor.constraint(equalTo: content.topAnchor, constant: 18),
            title.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: Self.margin),
            fields.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 10),
            fields.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            fields.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            note.topAnchor.constraint(equalTo: fields.bottomAnchor, constant: 10),
            note.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: Self.margin),
            note.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Self.margin),
            buttons.topAnchor.constraint(equalTo: note.bottomAnchor, constant: Self.margin),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Self.margin),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -Self.margin),
            content.widthAnchor.constraint(equalToConstant: 460),
        ])
        window.contentView = content
        window.autorecalculatesKeyViewLoop = true
    }

    /// One field line: a label and the field over a hairline, as in the compose window.
    private func row(_ title: String, _ field: NSView) -> NSView {
        let row = NSView()
        let label = NSTextField.label(title + ":", font: Design.font(.body), color: .muted)
        label.alignment = .right
        let separator = NSBox.separator()
        row.addConstrained([label, field, separator])
        var constraints = [
            row.heightAnchor.constraint(equalToConstant: 34),
            label.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: Self.margin),
            label.widthAnchor.constraint(equalToConstant: 52),
            label.firstBaselineAnchor.constraint(equalTo: field.firstBaselineAnchor),
            field.leadingAnchor.constraint(equalTo: label.trailingAnchor, constant: 8),
            field.centerYAnchor.constraint(equalTo: row.centerYAnchor),
            separator.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: Self.margin),
            separator.trailingAnchor.constraint(equalTo: row.trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: row.bottomAnchor),
        ]
        constraints.append(field === accountPopup
            ? field.trailingAnchor.constraint(lessThanOrEqualTo: row.trailingAnchor, constant: -Self.margin)
            : field.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -Self.margin))
        NSLayoutConstraint.activate(constraints)
        return row
    }

    // MARK: - State

    /// The people chosen as tokens.
    private var people: [String] { RecipientCompletion.tokens(in: peopleField) }

    /// Whether anything is in the field, a token or text not yet one.
    private var hasPeople: Bool {
        let text = peopleField.currentEditor()?.string ?? peopleField.stringValue
        return !people.isEmpty || !text.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private func update() {
        let count = people.count
        note.stringValue = failure ?? (count > 1
            ? "Starts a group chat with these \(count) people."
            : "Opens your direct message with this person, or starts one.")
        note.textColor = (failure == nil ? Design.Color.muted : .danger).color
        startButton.isEnabled = hasPeople && !starting
        peopleField.isEnabled = !starting
        accountPopup.isEnabled = !starting
        if starting { progress.startAnimation(nil) } else { progress.stopAnimation(nil) }
    }

    /// People to complete to: the account's chat partners first, then everyone in loaded mail.
    private func loadContacts() {
        let account = account
        let partners = store.chat.people(account: account).map { person in
            Contact(address: person.user.address, sent: 0, latest: person.activity ?? .distantPast, isOwn: false)
        }
        completion.contacts = partners
        Task { [weak self, store] in
            let mail = await store.contacts()
            guard let self, self.account == account else { return }
            var seen = Set(partners.map(\.address.normalizedEmail))
            completion.contacts = partners + mail.filter { !$0.isOwn && seen.insert($0.address.normalizedEmail).inserted }
        }
    }

    // MARK: - Actions

    @objc private func accountChanged() {
        guard let name = accountPopup.selectedItem?.representedObject as? String, name != account else { return }
        account = name
        loadContacts()
    }

    @objc private func cancel() {
        guard let window else { return }
        window.sheetParent?.endSheet(window)
    }

    @objc private func start() {
        guard !starting, let window else { return }
        // A person still being typed becomes a token first.
        if let editor = window.firstResponder as? NSText, editor.isFieldEditor { window.makeFirstResponder(nil) }
        let invalid = people.filter { Address.parse($0) == nil }
        guard invalid.isEmpty else { return stop("Not an email address: \(invalid.joined(separator: ", "))") }
        let emails = people.compactMap { Address.parse($0)?.email }
        guard !emails.isEmpty else { return stop("Add someone to chat with.") }
        starting = true
        failure = nil
        update()
        Task {
            do {
                let key = try await store.chat.start(account: account, with: emails)
                starting = false
                window.sheetParent?.endSheet(window)
                onStarted(key)
            } catch {
                starting = false
                stop(error.localizedDescription)
            }
        }
    }

    private func stop(_ message: String) {
        failure = message
        update()
        window?.makeFirstResponder(peopleField)
    }
}
