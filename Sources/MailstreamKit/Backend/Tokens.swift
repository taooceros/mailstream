import Foundation
import Security

/// OAuth access tokens by account id, refreshed from the refresh tokens kept in the Keychain.
/// Concurrent callers for one account share a single refresh; the Keychain item is read once and
/// again before each change.
nonisolated final class Tokens: TokenSource {
    private let state: State

    /// `providers` are the signed-in accounts and whose token endpoint each refreshes against.
    init(providers: [String: Provider]) {
        state = State(providers: providers)
    }

    func token(for account: String) async throws -> String {
        try await state.token(for: account)
    }

    func reject(_ token: String, account: String) async {
        await state.reject(token, account: account)
    }

    /// Starts over from a fresh sign-in: stores `refreshToken` with the `client` that issued it,
    /// remembers `client` for the provider's next sign-in, and caches `accessToken`.
    func install(account: String, provider: Provider, client: OAuthClient, refreshToken: String,
                 accessToken: String, expiresIn: TimeInterval) async throws {
        try await state.install(account: account, provider: provider, client: client, refreshToken: refreshToken,
                                access: Access(token: accessToken, expiry: Date(timeIntervalSinceNow: expiresIn)))
    }

    /// The client to offer for `provider`'s next sign-in: the one used last, else the build's.
    func suggestedClient(for provider: Provider) async throws -> OAuthClient? {
        try await state.suggestedClient(for: provider)
    }

    /// Signs `account` out: deletes its refresh token and forgets its access token.
    func forget(_ account: String) {
        Task { [state] in await state.forget(account) }
    }

    fileprivate struct Access: Sendable {
        let token: String
        let expiry: Date
    }

    private actor State {
        private var providers: [String: Provider]
        private var access: [String: Access] = [:]
        /// Refreshes in flight, shared by every caller for that account.
        private var flights: [String: Task<TokenResponse, any Error>] = [:]
        /// The Keychain item's contents once read. Changes re-read the item first: other
        /// processes hosting the engine (FFI front ends) write it too.
        private var signIns: SignIns?

        init(providers: [String: Provider]) {
            self.providers = providers
        }

        func token(for account: String) async throws -> String {
            if let cached = access[account], cached.expiry.timeIntervalSinceNow > 300 { return cached.token }
            guard let provider = providers[account] else { throw Tokens.signInAgain(account) }
            let flight: Task<TokenResponse, any Error>
            if let current = flights[account] {
                flight = current
            } else {
                let stored = try stored()
                guard let refreshToken = stored.refreshTokens[account] else { throw Tokens.signInAgain(account) }
                // Only the client that issued a refresh token can redeem it. Tokens from before
                // clients were stored came from the build's client, which only the app's
                // Info.plist carries: storing it lets every host of the engine redeem them.
                guard let client = stored.clients[account] ?? OAuthClient.builtIn(provider) else {
                    throw Tokens.signInAgain(account)
                }
                if stored.clients[account] == nil { try setClient(client, for: account) }
                flight = Task { try await OAuth.refresh(refreshToken, provider: provider, client: client) }
                flights[account] = flight
            }
            let response: TokenResponse
            do {
                response = try await flight.value
            } catch let error as OAuthError {
                if flights[account] == flight { flights[account] = nil }
                if error.code == "invalid_grant" { throw Tokens.signInAgain(account) }
                throw APIError(status: error.status, message: error.message)
            } catch {
                if flights[account] == flight { flights[account] = nil }
                throw error
            }
            // The first waiter back records the result; a sign-in or removal meanwhile supersedes it.
            if flights[account] == flight {
                flights[account] = nil
                if let rotated = response.refreshToken {
                    try setRefreshToken(rotated, for: account)
                }
                access[account] = Access(token: response.accessToken,
                                         expiry: Date(timeIntervalSinceNow: TimeInterval(response.expiresIn ?? 3600)))
            }
            return response.accessToken
        }

        func reject(_ token: String, account: String) {
            if access[account]?.token == token { access[account] = nil }
        }

        func install(account: String, provider: Provider, client: OAuthClient, refreshToken: String,
                     access fresh: Access) throws {
            var signIns = try self.fresh()
            signIns.refreshTokens[account] = refreshToken
            signIns.clients[account] = client
            signIns.lastClients[provider.rawValue] = client
            try save(signIns)
            providers[account] = provider
            flights[account] = nil
            access[account] = fresh
        }

        func suggestedClient(for provider: Provider) throws -> OAuthClient? {
            try stored().lastClients[provider.rawValue] ?? OAuthClient.builtIn(provider)
        }

        func forget(_ account: String) {
            if var signIns = try? fresh() {
                signIns.refreshTokens[account] = nil
                signIns.clients[account] = nil
                try? save(signIns)
            }
            providers[account] = nil
            flights[account] = nil
            access[account] = nil
        }

        private func stored() throws -> SignIns {
            if let signIns { return signIns }
            let loaded = try Keychain.load()
            signIns = loaded
            return loaded
        }

        /// The item as it is now, for a change to start from.
        private func fresh() throws -> SignIns {
            let loaded = try Keychain.load()
            signIns = loaded
            return loaded
        }

        private func setRefreshToken(_ token: String, for account: String) throws {
            var signIns = try fresh()
            guard signIns.refreshTokens[account] != token else { return }
            signIns.refreshTokens[account] = token
            try save(signIns)
        }

        private func setClient(_ client: OAuthClient, for account: String) throws {
            var signIns = try fresh()
            guard signIns.clients[account] == nil else { return }
            signIns.clients[account] = client
            try save(signIns)
        }

        private func save(_ contents: SignIns) throws {
            guard contents != signIns else { return }
            try Keychain.store(contents)
            signIns = contents
        }
    }

    private static func signInAgain(_ account: String) -> APIError {
        APIError(status: 401, message: "\(account) needs to sign in again (Settings > Accounts).")
    }
}

/// What the Keychain keeps for sign-ins, by account id and provider.
nonisolated struct SignIns: Codable, Equatable, Sendable {
    var refreshTokens: [String: String] = [:]
    /// The client each account's refresh token was issued to.
    var clients: [String: OAuthClient] = [:]
    /// The client last signed in with, by `Provider.rawValue`, offered for the next sign-in.
    var lastClients: [String: OAuthClient] = [:]
}

/// Sign-ins in the login keychain: one generic password (service `dev.mailstream.Mailstream.oauth`,
/// account `refresh-tokens`) holding `SignIns` as JSON, so macOS asks about Mailstream's access
/// once rather than per account. Items from before clients were stored hold a bare object of
/// refresh token by account id.
nonisolated enum Keychain {
    /// `MAILSTREAM_KEYCHAIN_SERVICE` names another item, for benchmarks and tests that must not
    /// read the person's sign-ins (`FFI/examples/bench.rs`).
    static let service = ProcessInfo.processInfo.environment["MAILSTREAM_KEYCHAIN_SERVICE"]
        .flatMap { $0.isEmpty ? nil : $0 } ?? "dev.mailstream.Mailstream.oauth"
    private static let account = "refresh-tokens"

    struct Failure: LocalizedError, Sendable {
        let status: OSStatus
        var errorDescription: String? {
            let reason = SecCopyErrorMessageString(status, nil) as String? ?? "OSStatus \(status)"
            return "Keychain: \(reason)"
        }
    }

    /// The stored sign-ins; empty when there are none.
    static func load() throws -> SignIns {
        var query = base
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else { return SignIns() }
            if let signIns = try? JSONDecoder().decode(SignIns.self, from: data) { return signIns }
            let legacy = (try? JSONDecoder().decode([String: String].self, from: data)) ?? [:]
            return SignIns(refreshTokens: legacy)
        case errSecItemNotFound:
            return SignIns()
        default:
            throw Failure(status: status)
        }
    }

    /// Replaces the stored sign-ins; with no refresh token left the item is deleted.
    static func store(_ signIns: SignIns) throws {
        guard !signIns.refreshTokens.isEmpty || !signIns.lastClients.isEmpty else {
            let deleted = SecItemDelete(base as CFDictionary)
            guard deleted == errSecSuccess || deleted == errSecItemNotFound else { throw Failure(status: deleted) }
            return
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let data = try encoder.encode(signIns)
        let update = SecItemUpdate(base as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        switch update {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            var item = base
            item[kSecValueData as String] = data
            item[kSecAttrLabel as String] = "Mailstream sign-ins"
            let added = SecItemAdd(item as CFDictionary, nil)
            guard added == errSecSuccess else { throw Failure(status: added) }
        default:
            throw Failure(status: update)
        }
    }

    private static var base: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }
}
