#if canImport(CryptoKit)
import CryptoKit
#else
import Crypto
#endif
import Foundation
#if canImport(Network)
import Network
#endif

nonisolated extension Provider {
    var authorizeURL: URL {
        switch self {
        case .google: URL(string: "https://accounts.google.com/o/oauth2/v2/auth")!
        case .microsoft: URL(string: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize")!
        }
    }

    var tokenURL: URL {
        switch self {
        case .google: URL(string: "https://oauth2.googleapis.com/token")!
        case .microsoft: URL(string: "https://login.microsoftonline.com/common/oauth2/v2.0/token")!
        }
    }

    /// Requested at sign-in.
    var scopes: [String] {
        switch self {
        case .google:
            ["openid", "email", "profile", OAuthScope.gmail, OAuthScope.googleContacts, OAuthScope.googleOtherContacts,
             OAuthScope.googleDirectory, OAuthScope.chatSpaces, OAuthScope.chatSpacesCreate, OAuthScope.chatMessages,
             OAuthScope.chatCreate, OAuthScope.chatMemberships, OAuthScope.chatReadState]
        case .microsoft:
            ["openid", "email", "profile", "offline_access",
             OAuthScope.graphUser, OAuthScope.graphMail, OAuthScope.graphSend, OAuthScope.graphContacts]
        }
    }

    /// Authorization parameters beyond the standard ones: Google issues a refresh token only with
    /// offline access and consent; Microsoft lets the person pick which account.
    fileprivate var authorizeExtras: [String: String] {
        switch self {
        case .google: ["access_type": "offline", "prompt": "consent", "include_granted_scopes": "true"]
        case .microsoft: ["prompt": "select_account"]
        }
    }

    /// Microsoft's token endpoint takes the scopes again; Google's ignores them.
    fileprivate var tokenExtras: [String: String] {
        switch self {
        case .google: [:]
        case .microsoft: ["scope": scopes.joined(separator: " ")]
        }
    }
}

/// An OAuth client the person registered with the provider and entered in Settings ▸ Accounts.
nonisolated struct OAuthClient: Codable, Equatable, Sendable {
    var id: String
    /// Google's desktop clients have one; a Microsoft public client has none.
    var secret: String?

    /// The client this build carries in its Info.plist, if any (scripts/bundle.sh): the default
    /// offered before anything was entered, and what accounts signed in before clients were
    /// stored refresh with.
    static func builtIn(_ provider: Provider) -> OAuthClient? {
        let config = AppConfig.current
        let (id, secret) = switch provider {
        case .google: (config.googleClientID, config.googleClientSecret)
        case .microsoft: (config.microsoftClientID, config.microsoftClientSecret)
        }
        return id.map { OAuthClient(id: $0, secret: secret) }
    }
}

/// A token endpoint answer.
nonisolated struct TokenResponse: Decodable, Sendable {
    let accessToken: String
    let expiresIn: Int?
    /// Always present after sign-in; on refresh only when the provider rotated it (Microsoft).
    let refreshToken: String?
    /// Granted scopes, space separated.
    let scope: String?
    let idToken: String?
}

/// A sign-in or token endpoint failure. `code` is the OAuth error code (`invalid_grant`, …), or
/// one of this app's: `config`, `listener`, `timeout`, `state`, `response`.
nonisolated struct OAuthError: LocalizedError, Sendable {
    var status = 0
    let code: String
    let message: String
    var errorDescription: String? { message }
}

/// Browser sign-in: authorization code with PKCE, redirected to a one-shot loopback listener.
nonisolated enum OAuth {
    /// The only redirect the Microsoft app registration allows; Google desktop clients accept any
    /// loopback port.
    static let redirect = "http://localhost:49152"

    /// What a completed sign-in yields.
    struct Grant: Sendable {
        /// Lowercased address.
        let id: String
        let name: String?
        let scopes: [String]
        let refreshToken: String
        let accessToken: String
        let expiresIn: Int
    }

    /// Opens the provider's sign-in page in the default browser once the redirect listener is up,
    /// waits (up to five minutes) for the redirect, then redeems the code with `client`.
    /// Cancelling the task stops waiting and closes the listener.
    @MainActor
    static func signIn(_ provider: Provider, client: OAuthClient) async throws -> Grant {
        let verifier = randomToken()
        let state = randomToken()
        let challenge = Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncodedString()
        var components = URLComponents(url: provider.authorizeURL, resolvingAgainstBaseURL: false)
        let parameters = [
            "client_id": client.id,
            "redirect_uri": redirect,
            "response_type": "code",
            "scope": provider.scopes.joined(separator: " "),
            "state": state,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
        ].merging(provider.authorizeExtras) { current, _ in current }
        components?.percentEncodedQuery = formEncoded(parameters)
        guard let url = components?.url else {
            throw OAuthError(code: "config", message: "Could not build the sign-in address.")
        }

        // The browser opens from here, on the main actor, once the redirect can be received.
        let listener = try RedirectListener(state: state)
        let code = try await withThrowingTaskGroup { group in
            group.addTask { try await listener.code() }
            if await listener.ready() { Platform.open(url) }
            guard let code = try await group.next() else { throw CancellationError() }
            return code
        }

        let response = try await exchange([
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect,
            "code_verifier": verifier,
        ], provider: provider, client: client)
        guard let refreshToken = response.refreshToken else {
            throw OAuthError(code: "response", message: "The sign-in did not grant offline access; try again.")
        }
        guard let claims = response.idToken.flatMap(Claims.init(jwt:)),
              let email = (claims.email ?? claims.preferredUsername)?.lowercased(), !email.isEmpty else {
            throw OAuthError(code: "response", message: "The sign-in did not say which account it was for.")
        }
        let granted = response.scope?.split(separator: " ").map(String.init) ?? provider.scopes
        return Grant(id: email, name: claims.name, scopes: granted, refreshToken: refreshToken,
                     accessToken: response.accessToken, expiresIn: response.expiresIn ?? 3600)
    }

    /// A new access token from `refreshToken`, redeemed with the `client` that issued it.
    static func refresh(_ refreshToken: String, provider: Provider, client: OAuthClient) async throws -> TokenResponse {
        try await exchange(["grant_type": "refresh_token", "refresh_token": refreshToken],
                           provider: provider, client: client)
    }

    /// POSTs `form` (plus the client credentials) to the token endpoint. Failures answer
    /// `{"error": …, "error_description": …}`.
    private static func exchange(_ form: [String: String], provider: Provider,
                                 client: OAuthClient) async throws -> TokenResponse {
        var request = URLRequest(url: provider.tokenURL)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        var credentials = ["client_id": client.id]
        if let secret = client.secret, !secret.isEmpty { credentials["client_secret"] = secret }
        let body = form.merging(credentials) { current, _ in current }
            .merging(provider.tokenExtras) { current, _ in current }
        request.httpBody = Data(formEncoded(body).utf8)
        let (data, response) = try await API.session.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        if (200..<300).contains(status), let tokens = try? decoder.decode(TokenResponse.self, from: data) {
            return tokens
        }
        struct Failure: Decodable {
            let error: String?
            let errorDescription: String?
        }
        let failure = try? decoder.decode(Failure.self, from: data)
        // Microsoft appends trace and correlation ids on further lines.
        let description = failure?.errorDescription?.split(whereSeparator: \.isNewline).first.map(String.init)
        throw OAuthError(status: status, code: failure?.error ?? "response",
                         message: description ?? failure?.error
                             ?? "\(provider.tokenURL.host() ?? "The sign-in server") answered \(status).")
    }

    /// The id token's claims. Its signature is not checked: it came straight from the token
    /// endpoint over TLS.
    private struct Claims: Decodable {
        let email: String?
        let preferredUsername: String?
        let name: String?

        init?(jwt: String) {
            let parts = jwt.split(separator: ".")
            guard parts.count >= 2, let data = Data(base64URLEncoded: parts[1]) else { return nil }
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            guard let claims = try? decoder.decode(Claims.self, from: data) else { return nil }
            self = claims
        }
    }

    /// 256 random bits, base64url: a PKCE verifier (43 characters) or a `state`.
    private static func randomToken() -> String {
        SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }.base64URLEncodedString()
    }
}

/// The OAuth redirect as the loopback listener answers it, whichever system listens.
nonisolated enum RedirectPage {
    static let port: UInt16 = 49152

    /// The end of the request head in `buffer`, once it has arrived.
    static func head(in buffer: Data) -> String? {
        buffer.range(of: Data("\r\n\r\n".utf8)).map { String(decoding: buffer[..<$0.lowerBound], as: UTF8.self) }
    }

    /// The response to send for request `head`, and the sign-in's result when this was the
    /// redirect. Accepts only `GET /?code=…&state=…` (or the provider's `error=…`) with this
    /// sign-in's state; anything else (a favicon, a stale tab) is answered and ignored.
    static func answer(head: String, state: String) -> (response: Data, outcome: Result<String, any Error>?) {
        let requestLine = head.components(separatedBy: "\r\n").first ?? ""
        let parts = requestLine.split(separator: " ")
        guard parts.count == 3, parts[0] == "GET",
              let components = URLComponents(string: "http://localhost" + String(parts[1])), components.path == "/" else {
            return (response(status: "404 Not Found", text: "Not found."), nil)
        }
        let query = Dictionary((components.queryItems ?? []).map { ($0.name, $0.value ?? "") }) { first, _ in first }
        guard query["state"] == state else {
            return (response(status: "400 Bad Request", text: "This sign-in page is out of date. Start again from Mailstream."), nil)
        }
        if let code = query["code"], !code.isEmpty {
            return (response(status: "200 OK", text: nil), .success(code))
        }
        let reason = query["error_description"]?.nilIfEmpty
            ?? query["error"] ?? "The provider sent no authorization code."
        return (response(status: "200 OK", text: "Sign-in failed: \(reason)"),
                .failure(OAuthError(code: query["error"] ?? "response", message: "Sign-in failed: \(reason)")))
    }

    static func failure(_ error: any Error) -> OAuthError {
        OAuthError(code: "listener", message: "Could not listen for the sign-in on port \(port): \(error.localizedDescription)")
    }

    /// `text` nil: the success page.
    private static func response(status: String, text: String?) -> Data {
        let message = text ?? "You can close this tab and return to Mailstream."
        let html = """
            <!doctype html><html><head><meta charset="utf-8"><title>Mailstream</title></head>\
            <body style="font: 15px -apple-system, 'Segoe UI', sans-serif; margin: 4em auto; max-width: 32em; text-align: center">\
            <p>\(HTMLText.escape(message))</p></body></html>
            """
        let body = Data(html.utf8)
        let head = "HTTP/1.1 \(status)\r\nContent-Type: text/html; charset=utf-8\r\n"
            + "Content-Length: \(body.count)\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
        return Data(head.utf8) + body
    }
}

#if canImport(Network)
/// One-shot HTTP listener on the loopback interface for the OAuth redirect. Answers the browser
/// with a small page and stops at the first sign-in result, after five minutes, or when the
/// task running `code()` is cancelled.
nonisolated struct RedirectListener: Sendable {
    private static let port = NWEndpoint.Port(rawValue: RedirectPage.port)!

    private let listener: NetworkListener<TCP>
    private let state: String
    /// One element once the port is open; finishes when listening stops.
    private let readiness: AsyncStream<Void>
    private let opened: AsyncStream<Void>.Continuation
    private let outcomes: AsyncStream<Result<String, any Error>>
    private let report: AsyncStream<Result<String, any Error>>.Continuation

    init(state: String) throws {
        do {
            listener = try NetworkListener(using: .parameters { TCP() }
                .localPort(Self.port).requiredInterfaceType(.loopback).localEndpointReuseAllowed(true))
        } catch {
            throw RedirectPage.failure(error)
        }
        self.state = state
        let (readiness, opened) = AsyncStream.makeStream(of: Void.self)
        let (outcomes, report) = AsyncStream.makeStream(of: Result<String, any Error>.self)
        (self.readiness, self.opened, self.outcomes, self.report) = (readiness, opened, outcomes, report)
        listener.onStateUpdate { _, update in
            switch update {
            case .ready: opened.yield()
            case .failed(let error): report.yield(.failure(RedirectPage.failure(error)))
            default: break
            }
        }
    }

    /// Waits, while `code()` runs, for the port to open; false when listening stopped first.
    func ready() async -> Bool {
        for await _ in readiness { return true }
        return false
    }

    /// Listens and returns the authorization code.
    func code() async throws -> String {
        defer { opened.finish() }
        return try await withThrowingTaskGroup(of: String.self) { group in
            group.addTask { [listener, state, report] in
                do {
                    try await listener.run { connection in
                        if let outcome = await Self.serve(connection, state: state) { report.yield(outcome) }
                    }
                } catch {
                    throw Task.isCancelled ? CancellationError() : RedirectPage.failure(error)
                }
                throw CancellationError()
            }
            group.addTask {
                try await Task.sleep(for: .seconds(300))
                throw OAuthError(code: "timeout", message: "The sign-in timed out after five minutes.")
            }
            group.addTask { [outcomes] in
                for await outcome in outcomes { return try outcome.get() }
                throw CancellationError()
            }
            // The first to finish decides; leaving the group closes the listener.
            defer { group.cancelAll() }
            guard let code = try await group.next() else { throw CancellationError() }
            return code
        }
    }

    /// Reads up to the end of the request head and answers it; returns the sign-in's result when
    /// this was the redirect. A connection that fails or ends early is dropped.
    private static func serve(_ connection: NetworkConnection<TCP>, state: String) async -> Result<String, any Error>? {
        var buffer = Data()
        do {
            while true {
                let (data, metadata) = try await connection.receive(atMost: 16_384)
                buffer.append(data)
                if let head = RedirectPage.head(in: buffer) {
                    let (response, outcome) = RedirectPage.answer(head: head, state: state)
                    try await connection.send(response, endOfStream: true)
                    return outcome
                }
                if metadata.endOfStream || buffer.count > 65_536 { return nil }
            }
        } catch {
            return nil
        }
    }
}
#endif
