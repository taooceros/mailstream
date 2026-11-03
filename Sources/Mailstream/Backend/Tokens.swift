import Foundation
import Security

/// OAuth access tokens by account id, refreshed from the refresh tokens kept in the Keychain.
/// Concurrent callers for one account share a single refresh; the Keychain item is read once.
final class Tokens: TokenSource {
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

    /// Starts over from a fresh sign-in: stores `refreshToken` and caches `accessToken`.
    func install(account: String, provider: Provider, refreshToken: String, accessToken: String,
                 expiresIn: TimeInterval) async throws {
        try await state.install(account: account, provider: provider, refreshToken: refreshToken,
                                access: Access(token: accessToken, expiry: Date(timeIntervalSinceNow: expiresIn)))
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
        private struct Flight {
            let id = UUID()
            let task: Task<TokenResponse, any Error>
        }

        private var providers: [String: Provider]
        private var access: [String: Access] = [:]
        private var flights: [String: Flight] = [:]
        /// The Keychain item's contents once read.
        private var refreshTokens: [String: String]?

        init(providers: [String: Provider]) {
            self.providers = providers
        }

        func token(for account: String) async throws -> String {
            if let cached = access[account], cached.expiry.timeIntervalSinceNow > 300 { return cached.token }
            guard let provider = providers[account] else { throw Tokens.signInAgain(account) }
            let flight: Flight
            if let current = flights[account] {
                flight = current
            } else {
                guard let refreshToken = try stored()[account] else { throw Tokens.signInAgain(account) }
                flight = Flight(task: Task { try await OAuth.refresh(refreshToken, provider: provider) })
                flights[account] = flight
            }
            let response: TokenResponse
            do {
                response = try await flight.task.value
            } catch let error as OAuthError {
                if flights[account]?.id == flight.id { flights[account] = nil }
                if error.code == "invalid_grant" { throw Tokens.signInAgain(account) }
                throw APIError(status: error.status, message: error.message)
            } catch {
                if flights[account]?.id == flight.id { flights[account] = nil }
                throw error
            }
            // The first waiter back records the result; a sign-in or removal meanwhile supersedes it.
            if flights[account]?.id == flight.id {
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

        func install(account: String, provider: Provider, refreshToken: String, access fresh: Access) throws {
            try setRefreshToken(refreshToken, for: account)
            providers[account] = provider
            flights[account] = nil
            access[account] = fresh
        }

        func forget(_ account: String) {
            try? setRefreshToken(nil, for: account)
            providers[account] = nil
            flights[account] = nil
            access[account] = nil
        }

        private func stored() throws -> [String: String] {
            if let refreshTokens { return refreshTokens }
            let loaded = try Keychain.load()
            refreshTokens = loaded
            return loaded
        }

        private func setRefreshToken(_ token: String?, for account: String) throws {
            var tokens = try stored()
            guard tokens[account] != token else { return }
            tokens[account] = token
            try Keychain.store(tokens)
            refreshTokens = tokens
        }
    }

    private static func signInAgain(_ account: String) -> APIError {
        APIError(status: 401, message: "\(account) needs to sign in again (Settings > Accounts).")
    }
}

/// Refresh tokens in the login keychain: one generic password (service
/// `dev.mailstream.Mailstream.oauth`, account `refresh-tokens`) holding a JSON object of refresh
/// token by account id, so macOS asks about Mailstream's access once rather than per account.
enum Keychain {
    static let service = "dev.mailstream.Mailstream.oauth"
    private static let account = "refresh-tokens"

    struct Failure: LocalizedError, Sendable {
        let status: OSStatus
        var errorDescription: String? {
            let reason = SecCopyErrorMessageString(status, nil) as String? ?? "OSStatus \(status)"
            return "Keychain: \(reason)"
        }
    }

    /// The stored refresh tokens by account id; empty when there are none.
    static func load() throws -> [String: String] {
        var query = base
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else { return [:] }
            return (try? JSONDecoder().decode([String: String].self, from: data)) ?? [:]
        case errSecItemNotFound:
            return [:]
        default:
            throw Failure(status: status)
        }
    }

    /// Replaces the stored tokens; none left deletes the item.
    static func store(_ tokens: [String: String]) throws {
        guard !tokens.isEmpty else {
            let deleted = SecItemDelete(base as CFDictionary)
            guard deleted == errSecSuccess || deleted == errSecItemNotFound else { throw Failure(status: deleted) }
            return
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let data = try encoder.encode(tokens)
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
