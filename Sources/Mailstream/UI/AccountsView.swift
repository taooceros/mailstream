import SwiftUI

/// What the Accounts window does: sign-in, rename, remove, and the status line.
@MainActor
@Observable
final class AccountsModel {
    struct Status: Equatable {
        var text: String
        var isError: Bool
    }

    /// The OAuth client sheet for one provider, prefilled with the suggested client.
    struct ClientPrompt: Identifiable {
        let provider: Provider
        var id: String
        var secret: String
    }

    let accounts: AccountStore
    var selection: AccountRecord.ID?
    /// The account whose label is being edited inline.
    var editing: AccountRecord.ID?
    var clientPrompt: ClientPrompt?
    /// The account waiting on the remove confirmation.
    var removal: AccountRecord?
    /// The status line: progress, the last result or the last error.
    private(set) var status: Status?
    private var signIn: Task<Void, Never>?
    /// Looks up the suggested client before the client sheet shows, so a second Add can't queue another.
    private var clientLookup: Task<Void, Never>?

    init(accounts: AccountStore) { self.accounts = accounts }

    var isBusy: Bool { signIn != nil }
    var selected: AccountRecord? { accounts.records.first { $0.id == selection } }

    func rename(_ record: AccountRecord, to label: String) {
        editing = nil
        if label != record.label { accounts.rename(record.id, label: label) }
    }

    /// Asks for the OAuth client (prefilled with the last one used, else the build's), then signs in.
    func startSignIn(_ provider: Provider) {
        guard signIn == nil, clientLookup == nil, clientPrompt == nil else { return }
        clientLookup = Task { [weak self, accounts] in
            let suggested = (try? await accounts.tokens.suggestedClient(for: provider)) ?? nil
            guard let self else { return }
            clientLookup = nil
            guard signIn == nil else { return }
            clientPrompt = ClientPrompt(provider: provider, id: suggested?.id ?? "", secret: suggested?.secret ?? "")
        }
    }

    func continueInBrowser(_ prompt: ClientPrompt) {
        clientPrompt = nil
        let id = prompt.id.trimmingCharacters(in: .whitespacesAndNewlines)
        let secret = prompt.secret.trimmingCharacters(in: .whitespacesAndNewlines)
        if id.isEmpty {
            status = Status(text: "Enter your OAuth client's ID to sign in.", isError: true)
        } else if prompt.provider == .google && secret.isEmpty {
            // Google redeems a desktop client's code only with its secret.
            status = Status(text: "Google needs the client secret too; the Cloud Console shows it next to the client ID.",
                            isError: true)
        } else {
            beginSignIn(prompt.provider, client: OAuthClient(id: id, secret: secret.isEmpty ? nil : secret))
        }
    }

    private func beginSignIn(_ provider: Provider, client: OAuthClient) {
        guard signIn == nil else { return }
        status = Status(text: "Waiting for the browser…", isError: false)
        signIn = Task(name: "Sign-in") { [weak self, accounts] in
            do {
                let record = try await accounts.signIn(provider, client: client)
                self?.signInEnded(.success(record))
            } catch {
                self?.signInEnded(.failure(error))
            }
        }
    }

    private func signInEnded(_ result: Result<AccountRecord, any Error>) {
        signIn = nil
        switch result {
        case .success(let record):
            status = Status(text: "Signed in as \(record.id).", isError: false)
            selection = record.id
        case .failure(let error as URLError) where error.code == .cancelled:
            status = nil
        case .failure(is CancellationError):
            status = nil
        case .failure(let error):
            status = Status(text: error.localizedDescription, isError: true)
        }
    }

    func cancelSignIn() {
        signIn?.cancel()
    }

    func confirmRemove() {
        guard !isBusy, let selected else { return }
        removal = selected
    }

    func remove(_ record: AccountRecord) {
        removal = nil
        status = nil
        accounts.remove(record.id)
    }
}

/// The Accounts window: the signed-in accounts in a rounded list (badge in the account's
/// sidebar color, label, address, what it is used for), adding one by browser sign-in,
/// removing, and renaming a label inline (double-click).
struct AccountsView: View {
    @Bindable var model: AccountsModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Accounts")
                .font(.design(.title, weight: .semibold))
                .padding(.leading, 4)
            Text("Mailstream signs in to Google and Microsoft in your browser, with an OAuth client you registered. Sign-ins stay in your Keychain.")
                .font(.design(.caption))
                .foregroundStyle(Color.design(.muted))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 4)
                .padding(.horizontal, 4)
            listBox
                .padding(.top, 14)
            statusRow
                .padding(.top, 10)
                .padding(.leading, 4)
        }
        .padding(.top, 38)
        .padding(.horizontal, 20)
        .padding(.bottom, 16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .ignoresSafeArea(.container, edges: .top)
        .sheet(item: $model.clientPrompt) { prompt in
            AccountsClientSheet(prompt: prompt, model: model)
        }
        .alert(model.removal.map { "Remove \($0.label)?" } ?? "",
               isPresented: Binding(get: { model.removal != nil }, set: { if !$0 { model.removal = nil } }),
               presenting: model.removal) { record in
            Button("Remove", role: .destructive) { model.remove(record) }
            Button("Cancel", role: .cancel) { model.removal = nil }
        } message: { record in
            Text("Mailstream signs out of \(record.id) and deletes its saved sign-in. "
                + "Mail on the server is not affected; you can add the account again later.")
        }
    }

    private var records: [AccountRecord] { model.accounts.records }

    private var listBox: some View {
        let shape = RoundedRectangle(cornerRadius: Design.Radius.card.rawValue)
        return VStack(spacing: 0) {
            List(selection: $model.selection) {
                ForEach(Array(records.enumerated()), id: \.element.id) { index, record in
                    AccountsRow(record: record, color: AccountsView.color(at: index),
                                isLast: index == records.count - 1, model: model)
                        .tag(record.id)
                        .listRowInsets(EdgeInsets())
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 56)
            .contextMenu(forSelectionType: AccountRecord.ID.self, menu: { _ in }, primaryAction: { ids in
                // Double-click edits the label.
                if let id = ids.first, !model.isBusy { model.editing = id }
            })
            .overlay {
                if records.isEmpty { emptyState.padding(.horizontal, 20) }
            }
            Divider()
            bar
        }
        .background(Color(nsColor: .controlBackgroundColor), in: shape)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Color.design(.line), lineWidth: 1))
    }

    private var bar: some View {
        HStack(spacing: 0) {
            Menu {
                let color = AccountsView.color(at: records.count)
                Button { model.startSignIn(.google) } label: {
                    Label { Text("Google Account…") } icon: { ProviderBadge.image(.google, color: color, side: 16) }
                }
                Button { model.startSignIn(.microsoft) } label: {
                    Label { Text("Microsoft Account…") } icon: { ProviderBadge.image(.microsoft, color: color, side: 16) }
                }
            } label: {
                Image(systemName: "plus").accessibilityLabel("Add an account")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .frame(width: 28)
            .help("Add an account")
            .disabled(model.isBusy)
            Divider().frame(height: 14)
            Button(action: model.confirmRemove) {
                Image(systemName: "minus").accessibilityLabel("Remove the selected account")
                    .frame(width: 28, height: 26)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .help("Remove the selected account")
            .disabled(model.isBusy || model.selected == nil)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 4)
        .frame(height: 26)
    }

    private var emptyState: some View {
        VStack(spacing: 6) {
            Image(systemName: "person.crop.circle.badge.plus")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(Color.design(.faint))
            Text("No Accounts").font(.design(.headline, weight: .semibold))
            Text("Sign in to bring in mail, contacts and Google Chat.")
                .font(.design(.caption))
                .foregroundStyle(Color.design(.muted))
                .padding(.bottom, 10)
            HStack(spacing: 10) {
                let color = AccountsView.color(at: 0)
                Button { model.startSignIn(.google) } label: {
                    Label { Text("Sign in with Google…") } icon: { ProviderBadge(provider: .google, color: color, side: 16) }
                }
                Button { model.startSignIn(.microsoft) } label: {
                    Label { Text("Sign in with Microsoft…") } icon: {
                        ProviderBadge(provider: .microsoft, color: color, side: 16)
                    }
                }
            }
            .buttonStyle(.bordered)
            .labelStyle(.titleAndIcon)
            .disabled(model.isBusy)
        }
    }

    private var statusRow: some View {
        HStack(spacing: 6) {
            if model.isBusy { ProgressView().controlSize(.small) }
            let text = model.status?.text ?? ""
            Text(text)
                .font(.design(.caption))
                .foregroundStyle(Color.design(model.status?.isError == true ? .danger : .muted))
                .lineLimit(1)
                .truncationMode(.tail)
                .textSelection(.enabled)
                .help(text)
            if model.isBusy {
                Button("Cancel", action: model.cancelSignIn).controlSize(.small)
            }
        }
        .frame(minHeight: 20)
    }

    /// The row color: `AccountStyle`'s palette at the row index.
    static func color(at index: Int) -> Color { Color(nsColor: NSColor(hex: AccountStyle.hex(at: index))) }
}

/// One account row: badge, label (editable on double-click) over the address, and the
/// services its sign-in covers.
private struct AccountsRow: View {
    let record: AccountRecord
    let color: Color
    let isLast: Bool
    let model: AccountsModel
    @State private var draft = ""
    @FocusState private var fieldFocused: Bool

    private var isEditing: Bool { model.editing == record.id }

    var body: some View {
        HStack(spacing: 12) {
            ProviderBadge(provider: record.provider, color: color, side: 32)
            VStack(alignment: .leading, spacing: 1) {
                if isEditing {
                    TextField("", text: $draft)
                        .textFieldStyle(.plain)
                        .font(.design(.body, weight: .semibold))
                        .focused($fieldFocused)
                        .onSubmit { model.rename(record, to: draft) }
                        .onAppear {
                            draft = record.label
                            fieldFocused = true
                        }
                        .onChange(of: fieldFocused) { _, focused in
                            if !focused, isEditing { model.rename(record, to: draft) }
                        }
                } else {
                    Text(record.label)
                        .font(.design(.body, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Text(address)
                    .font(.design(.caption))
                    .foregroundStyle(Color.design(.muted))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .layoutPriority(-1)
            Spacer(minLength: 12)
            Text(services.joined(separator: " · "))
                .font(.design(.caption))
                .foregroundStyle(Color.design(.faint))
                .multilineTextAlignment(.trailing)
                .fixedSize()
        }
        .padding(.leading, 14)
        .padding(.trailing, 14)
        .frame(height: 56)
        .overlay(alignment: .bottom) {
            if !isLast {
                Divider().padding(.leading, 14 + 32 + 12)
            }
        }
    }

    private var address: String {
        let provider = record.provider == .google ? "Google" : "Microsoft"
        return record.label == record.id ? provider : "\(record.id) · \(provider)"
    }

    private var services: [String] {
        let scopes = record.scopes
        func has(_ suffix: String) -> Bool { scopes.contains { $0.hasSuffix(suffix) } }
        var names: [String] = []
        if has("mail.google.com/") || has("Mail.ReadWrite") { names.append("Mail") }
        if has("contacts.readonly") || has("Contacts.Read") { names.append("Contacts") }
        if has("chat.messages.readonly") { names.append("Chat") }
        return names
    }
}

/// The client sheet: the OAuth client's ID and secret before the browser sign-in.
private struct AccountsClientSheet: View {
    @State var prompt: AccountsModel.ClientPrompt
    let model: AccountsModel
    @Environment(\.dismiss) private var dismiss
    @FocusState private var focus: Field?

    private enum Field { case id, secret }

    private var google: Bool { prompt.provider == .google }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(google ? "Sign in with Google" : "Sign in with Microsoft")
                .font(.design(.headline, weight: .semibold))
            Text(detail)
                .font(.design(.caption))
                .fixedSize(horizontal: false, vertical: true)
            Grid(horizontalSpacing: 8, verticalSpacing: 8) {
                GridRow {
                    Text("Client ID:").gridColumnAlignment(.trailing)
                    TextField(google ? "Client ID" : "Application (client) ID", text: $prompt.id)
                        .focused($focus, equals: .id)
                        .frame(width: 260)
                }
                GridRow {
                    Text("Client secret:")
                    SecureField(google ? "Client secret" : "Client secret (empty for a public client)",
                                text: $prompt.secret)
                        .focused($focus, equals: .secret)
                        .frame(width: 260)
                }
            }
            .frame(maxWidth: .infinity)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Continue in Browser") { model.continueInBrowser(prompt) }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 420)
        .defaultFocus($focus, prompt.id.isEmpty ? .id : .secret)
    }

    private var detail: String {
        switch prompt.provider {
        case .google:
            "Enter the client ID and secret of your Google Cloud OAuth client (type Desktop app). "
                + "Mailstream then opens Google's sign-in in your browser."
        case .microsoft:
            "Enter the application ID of your Microsoft Entra app, and its secret unless it is a public client. "
                + "Its redirect URI must be \(OAuth.redirect)."
        }
    }
}

/// A round badge in an account's color carrying its provider's mark: Google's "G" or
/// Microsoft's four squares.
struct ProviderBadge: View {
    let provider: Provider
    let color: Color
    let side: CGFloat

    var body: some View {
        ZStack {
            Circle().fill(color)
            switch provider {
            case .google:
                Text("G")
                    .font(.system(size: side * 0.56, weight: .bold))
                    .foregroundStyle(.white)
            case .microsoft:
                let square = side * 0.19, gap = side * 0.04
                Grid(horizontalSpacing: gap, verticalSpacing: gap) {
                    ForEach(0..<2, id: \.self) { _ in
                        GridRow {
                            ForEach(0..<2, id: \.self) { _ in Rectangle().fill(.white).frame(width: square, height: square) }
                        }
                    }
                }
            }
        }
        .frame(width: side, height: side)
        .accessibilityHidden(true)
    }

    /// The badge as an image, for menu items, which render only images as icons.
    static func image(_ provider: Provider, color: Color, side: CGFloat) -> Image {
        let renderer = ImageRenderer(content: ProviderBadge(provider: provider, color: color, side: side))
        renderer.scale = NSScreen.main?.backingScaleFactor ?? 2
        guard let image = renderer.nsImage else { return Image(systemName: "person.crop.circle") }
        return Image(nsImage: image)
    }
}
