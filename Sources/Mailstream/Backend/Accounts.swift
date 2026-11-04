import Foundation
import Observation

nonisolated enum Provider: String, Codable, Sendable {
    case google, microsoft
}

nonisolated struct AccountRecord: Codable, Hashable, Sendable, Identifiable {
    /// Lowercased address. The key of everything: Account.name, MailboxKey.account, MessageRef.account, cache paths.
    let id: String
    let provider: Provider
    /// Shown in the sidebar; user-editable (defaults to the address).
    var label: String
    /// The person's name from the provider (sender display name).
    var displayName: String?
    /// Scopes the provider granted.
    var scopes: [String]
}

/// The signed-in accounts, in the order they were added, persisted in
/// `~/Library/Application Support/Mailstream/accounts.json`; refresh tokens live in the Keychain.
/// Stores follow sign-ins, removals and renames by observing `records`.
@Observable
final class AccountStore {
    private(set) var records: [AccountRecord]
    @ObservationIgnored let tokens: Tokens
    /// Where a failed save of the account list is reported.
    @ObservationIgnored let problems: StoreProblems
    @ObservationIgnored private var apis: [String: API] = [:]
    @ObservationIgnored private var backends: [String: any MailBackend] = [:]
    /// Runs after an account is removed, while its token still works: push stops its registration.
    @ObservationIgnored var beforeSignOut: (() async -> Void)?
    /// Removals waiting for `beforeSignOut` before deleting the token; signing in again calls one off.
    @ObservationIgnored private var signOuts: [String: Task<Void, Never>] = [:]

    private static let file = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appending(path: "Mailstream/accounts.json")

    init(problems: StoreProblems) {
        self.problems = problems
        let records = (try? Data(contentsOf: Self.file)).flatMap { try? JSONDecoder().decode([AccountRecord].self, from: $0) } ?? []
        self.records = records
        tokens = Tokens(providers: Dictionary(records.map { ($0.id, $0.provider) }) { first, _ in first })
    }

    func record(_ id: String) -> AccountRecord? {
        records.first { $0.id == id }
    }

    func api(for id: String) -> API? {
        if let api = apis[id] { return api }
        guard let record = record(id) else { return nil }
        let api = record.provider == .google
            ? API(account: id, tokens: tokens, budget: .gmail())
            : API(account: id, tokens: tokens, interactive: 3, background: 1)
        apis[id] = api
        return api
    }

    func backend(for id: String) -> (any MailBackend)? {
        if let backend = backends[id] { return backend }
        guard let record = record(id), let api = api(for: id) else { return nil }
        let backend: any MailBackend = switch record.provider {
        case .google: GmailBackend(api: api)
        case .microsoft: GraphBackend(api: api)
        }
        backends[id] = backend
        return backend
    }

    /// Browser OAuth with the person's `client`. Signing in to an account already here replaces
    /// its token, client, name and scopes and keeps its label.
    func signIn(_ provider: Provider, client: OAuthClient) async throws -> AccountRecord {
        let grant = try await OAuth.signIn(provider, client: client)
        signOuts.removeValue(forKey: grant.id)?.cancel()
        try await tokens.install(account: grant.id, provider: provider, client: client,
                                 refreshToken: grant.refreshToken,
                                 accessToken: grant.accessToken, expiresIn: TimeInterval(grant.expiresIn))
        var record = AccountRecord(id: grant.id, provider: provider, label: grant.id, displayName: grant.name,
                                   scopes: grant.scopes)
        if let index = records.firstIndex(where: { $0.id == record.id }) {
            record.label = records[index].label
            if records[index].provider != provider { backends[record.id] = nil }
            records[index] = record
        } else {
            records.append(record)
        }
        try persist()
        return record
    }

    /// Signs out: deletes the record and the Gmail envelope cache, then the Keychain item once
    /// `beforeSignOut` is done.
    func remove(_ id: String) {
        apis[id] = nil
        backends[id] = nil
        records.removeAll { $0.id == id }
        save()
        signOuts[id] = Task {
            await GmailBackend.discardCache(account: id)
            await beforeSignOut?()
            guard !Task.isCancelled else { return }
            signOuts[id] = nil
            tokens.forget(id)
        }
    }

    /// An empty label shows the address again.
    func rename(_ id: String, label: String) {
        guard let index = records.firstIndex(where: { $0.id == id }) else { return }
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        records[index].label = trimmed.isEmpty ? id : trimmed
        save()
    }

    /// Persists `records`; a failed write is reported as an alert.
    private func save() {
        do {
            try persist()
        } catch {
            problems.report("Could not save the account list: \(error.localizedDescription)", from: .accounts, alert: true)
        }
    }

    private func persist() throws {
        try FileManager.default.createDirectory(at: Self.file.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(records).write(to: Self.file, options: .atomic)
    }
}
