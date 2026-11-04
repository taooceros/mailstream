import AppKit
import Observation
import SwiftUI

/// The New Chat sheet's state: the chat account and the people to chat with. One person opens the
/// user's direct message with them (Google makes one if there is none), several a group chat.
@Observable
final class NewChatModel: Identifiable, RecipientSource {
    @ObservationIgnored let store: MailStore

    var account: String {
        didSet { if account != oldValue { loadContacts() } }
    }
    private(set) var people: [String] = []
    private(set) var starting = false
    /// Why the chat could not start.
    private(set) var failure: String?

    @ObservationIgnored var labels: [String: String] = [:]
    /// People to complete to: the account's chat partners first, then everyone in loaded mail.
    @ObservationIgnored private var contacts: [Contact] = []
    @ObservationIgnored private weak var field: NSTokenField?
    /// The field takes focus once it is in the sheet's window.
    @ObservationIgnored private var focusPending = true
    /// The space to open; the host closes the sheet.
    @ObservationIgnored var onStarted: (ChatSpaceKey) -> Void = { _ in }

    init(store: MailStore, account: String) {
        self.store = store
        self.account = account
        loadContacts()
    }

    var accounts: [ChatAccount] { store.chat.accounts }

    var canStart: Bool { !people.isEmpty && !starting }

    var hint: String {
        people.count > 1
            ? "Starts a group chat with these \(people.count) people."
            : "Opens your direct message with this person, or starts one."
    }

    private func loadContacts() {
        let account = account
        let partners = store.chat.people(account: account).map { person in
            Contact(address: person.user.address, sent: 0, latest: person.activity ?? .distantPast, isOwn: false)
        }
        contacts = partners
        Task { [weak self, store] in
            let mail = await store.contacts()
            guard let self, self.account == account else { return }
            var seen = Set(partners.map(\.address.normalizedEmail))
            contacts = partners + mail.filter { !$0.isOwn && seen.insert($0.address.normalizedEmail).inserted }
        }
    }

    // MARK: - RecipientSource

    func recipients(_ field: ComposeField) -> [String] { people }

    func setRecipients(_ tokens: [String], in field: ComposeField) {
        if people != tokens { people = tokens }
    }

    func recipientsTyped(_ tokens: [String], in field: ComposeField) {
        failure = nil
        setRecipients(tokens, in: field)
    }

    func suggestions(for typed: String) -> [Contact] {
        let needle = Contact.fold(typed.trimmingCharacters(in: .whitespaces))
        guard !needle.isEmpty else { return [] }
        let taken = Set(people.compactMap { Address.parse($0)?.normalizedEmail })
        var matches = Array(contacts.lazy.filter { !taken.contains($0.address.normalizedEmail) && $0.matches(needle) }.prefix(12))
        if let exact = matches.firstIndex(where: { Contact.fold($0.address.email) == needle }) {
            matches.insert(matches.remove(at: exact), at: 0)
        }
        return matches
    }

    func attach(_ view: NSView, as field: ComposeField) {
        self.field = view as? NSTokenField
    }

    func moved(toWindow field: ComposeField) {
        guard focusPending, let view = self.field, let window = view.window else { return }
        focusPending = false
        window.makeFirstResponder(view)
    }

    // MARK: - Starting

    func start() {
        guard !starting else { return }
        commitEditing()
        let invalid = people.filter { Address.parse($0) == nil }
        guard invalid.isEmpty else { return stop("Not an email address: \(invalid.joined(separator: ", "))") }
        let emails = people.compactMap { Address.parse($0)?.email }
        guard !emails.isEmpty else { return stop("Add someone to chat with.") }
        starting = true
        failure = nil
        Task {
            do {
                let key = try await store.chat.start(account: account, with: emails)
                starting = false
                onStarted(key)
            } catch {
                starting = false
                stop(error.localizedDescription)
            }
        }
    }

    private func stop(_ message: String) {
        failure = message
        if let field, let window = field.window { window.makeFirstResponder(field) }
    }

    /// A person still being typed becomes a token first.
    private func commitEditing() {
        guard let field, let window = field.window else { return }
        if let editor = window.firstResponder as? NSText, editor.isFieldEditor { window.makeFirstResponder(nil) }
        setRecipients(ComposeModel.tokens(in: field), in: .to)
    }
}

/// Picks the people to chat with, completing to the account's chat partners and mail contacts.
struct NewChatView: View {
    @Bindable var model: NewChatModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("New Chat")
                .font(.design(.headline, weight: .semibold))
                .padding(.horizontal, ComposeView.margin)
                .padding(.top, 18)
                .padding(.bottom, 10)
            if model.accounts.count > 1 {
                row("From") {
                    accountPicker
                    Spacer(minLength: 0)
                }
            }
            row("To") {
                RecipientField(model: model, field: .to).disabled(model.starting)
            }
            Text(model.failure ?? model.hint)
                .font(.design(.footnote))
                .foregroundStyle(Color.design(model.failure == nil ? .muted : .danger))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, ComposeView.margin)
                .padding(.top, 10)
            HStack(spacing: 8) {
                Spacer()
                if model.starting { ProgressView().controlSize(.small) }
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Start Chat") { model.start() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!model.canStart)
            }
            .padding(ComposeView.margin)
        }
        .frame(width: 460)
    }

    /// One field line: a label and the field over a hairline, as in the compose window.
    private func row(_ title: String, @ViewBuilder field: () -> some View) -> some View {
        HStack(spacing: 8) {
            Text(title + ":")
                .font(.design(.body))
                .foregroundStyle(Color.design(.muted))
                .frame(width: 52, alignment: .trailing)
            field()
        }
        .padding(.horizontal, ComposeView.margin)
        .frame(height: 34)
        .overlay(alignment: .bottom) { Divider().padding(.leading, ComposeView.margin) }
    }

    private var accountPicker: some View {
        Picker("From", selection: $model.account) {
            ForEach(model.accounts, id: \.name) { account in
                Text(model.store.label(account: account.name)).help(account.name).tag(account.name)
            }
        }
        .pickerStyle(.menu)
        .buttonStyle(.borderless)
        .labelsHidden()
        .font(.design(.body))
        .fixedSize()
        .disabled(model.starting)
    }
}
