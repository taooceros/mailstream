import AppKit
import CryptoKit
import Network

extension Provider {
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
            ["openid", "email", "profile", "https://mail.google.com/",
             "https://www.googleapis.com/auth/contacts.readonly",
             "https://www.googleapis.com/auth/contacts.other.readonly",
             "https://www.googleapis.com/auth/directory.readonly",
             "https://www.googleapis.com/auth/chat.spaces.readonly",
             "https://www.googleapis.com/auth/chat.messages.readonly",
             "https://www.googleapis.com/auth/chat.messages.create",
             "https://www.googleapis.com/auth/chat.memberships.readonly",
             "https://www.googleapis.com/auth/chat.users.readstate"]
        case .microsoft:
            ["openid", "email", "profile", "offline_access",
             "https://graph.microsoft.com/User.Read",
             "https://graph.microsoft.com/Mail.ReadWrite",
             "https://graph.microsoft.com/Mail.Send",
             "https://graph.microsoft.com/Contacts.Read"]
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

    /// Client id and secret from Info.plist.
    fileprivate func client() throws -> (id: String, secret: String) {
        let config = AppConfig.current
        let (id, secret, idKey, secretKey) = switch self {
        case .google:
            (config.googleClientID, config.googleClientSecret, "MSGoogleClientID", "MSGoogleClientSecret")
        case .microsoft:
            (config.microsoftClientID, config.microsoftClientSecret, "MSMicrosoftClientID", "MSMicrosoftClientSecret")
        }
        guard let id else { throw OAuthError(code: "config", message: "This build has no \(idKey) in its Info.plist.") }
        guard let secret else {
            throw OAuthError(code: "config", message: "This build has no \(secretKey) in its Info.plist.")
        }
        return (id, secret)
    }
}

/// A token endpoint answer.
struct TokenResponse: Decodable, Sendable {
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
struct OAuthError: LocalizedError, Sendable {
    var status = 0
    let code: String
    let message: String
    var errorDescription: String? { message }
}

/// Browser sign-in: authorization code with PKCE, redirected to a one-shot loopback listener.
enum OAuth {
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

    /// Opens the provider's sign-in page in the default browser and waits (up to five minutes)
    /// for the redirect. Cancelling the task stops waiting and closes the listener.
    @MainActor
    static func signIn(_ provider: Provider) async throws -> Grant {
        let client = try provider.client()
        let verifier = randomToken()
        let state = randomToken()
        let challenge = base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
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

        let listener = try RedirectListener(state: state)
        let code = try await listener.code {
            Task { @MainActor in NSWorkspace.shared.open(url) }
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

    /// A new access token from `refreshToken`.
    static func refresh(_ refreshToken: String, provider: Provider) async throws -> TokenResponse {
        try await exchange(["grant_type": "refresh_token", "refresh_token": refreshToken],
                           provider: provider, client: provider.client())
    }

    /// POSTs `form` (plus the client credentials) to the token endpoint. Failures answer
    /// `{"error": …, "error_description": …}`.
    private static func exchange(_ form: [String: String], provider: Provider,
                                 client: (id: String, secret: String)) async throws -> TokenResponse {
        var request = URLRequest(url: provider.tokenURL)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let body = form.merging(["client_id": client.id, "client_secret": client.secret]) { current, _ in current }
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
            guard parts.count >= 2 else { return nil }
            var payload = parts[1].replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            payload += String(repeating: "=", count: (4 - payload.count % 4) % 4)
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            guard let data = Data(base64Encoded: payload),
                  let claims = try? decoder.decode(Claims.self, from: data) else { return nil }
            self = claims
        }
    }

    /// 256 random bits, base64url: a PKCE verifier (43 characters) or a `state`.
    private static func randomToken() -> String {
        base64URL(SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) })
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// `application/x-www-form-urlencoded`, also used for the authorization query.
    private static func formEncoded(_ fields: [String: String]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        func encode(_ text: String) -> String { text.addingPercentEncoding(withAllowedCharacters: allowed) ?? text }
        return fields.sorted { $0.key < $1.key }.map { "\(encode($0.key))=\(encode($0.value))" }.joined(separator: "&")
    }
}

/// One-shot HTTP listener on the loopback interface for the OAuth redirect. Answers the browser
/// with a small page, then stops. All mutable state is touched only on `queue`.
private final class RedirectListener: @unchecked Sendable {
    private static let port: NWEndpoint.Port = 49152
    private static let timeout: DispatchTimeInterval = .seconds(300)

    private let queue = DispatchQueue(label: "dev.mailstream.oauth-redirect")
    private let listener: NWListener
    private let state: String
    private var continuation: CheckedContinuation<String, any Error>?
    private var outcome: Result<String, any Error>?

    init(state: String) throws {
        self.state = state
        let parameters = NWParameters.tcp
        parameters.requiredInterfaceType = .loopback
        parameters.allowLocalEndpointReuse = true
        do {
            listener = try NWListener(using: parameters, on: Self.port)
        } catch {
            throw OAuthError(code: "listener", message: "Could not listen for the sign-in on port 49152: \(error.localizedDescription)")
        }
    }

    /// Starts listening, calls `ready` once the port is open, and returns the authorization code.
    func code(ready: @escaping @Sendable () -> Void) async throws -> String {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                self.queue.async { self.start(continuation, ready: ready) }
            }
        } onCancel: {
            self.queue.async { self.finish(.failure(CancellationError())) }
        }
    }

    private func start(_ continuation: CheckedContinuation<String, any Error>, ready: @escaping @Sendable () -> Void) {
        if let outcome {
            continuation.resume(with: outcome)
            return
        }
        self.continuation = continuation
        listener.stateUpdateHandler = { [self] state in
            switch state {
            case .ready:
                ready()
            case .failed(let error):
                finish(.failure(OAuthError(code: "listener", message: "Could not listen for the sign-in on port 49152: \(error.localizedDescription)")))
            default:
                break
            }
        }
        listener.newConnectionHandler = { [self] connection in
            connection.start(queue: queue)
            receive(connection, Data())
        }
        listener.start(queue: queue)
        queue.asyncAfter(deadline: .now() + Self.timeout) { [weak self] in
            self?.finish(.failure(OAuthError(code: "timeout", message: "The sign-in timed out after five minutes.")))
        }
    }

    /// Reads up to the end of the request head.
    private func receive(_ connection: NWConnection, _ buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16_384) { [self] data, _, isComplete, error in
            var buffer = buffer
            if let data { buffer.append(data) }
            if let end = buffer.range(of: Data("\r\n\r\n".utf8)) {
                handle(connection, head: String(decoding: buffer[..<end.lowerBound], as: UTF8.self))
            } else if error != nil || isComplete || buffer.count > 65_536 {
                connection.cancel()
            } else {
                receive(connection, buffer)
            }
        }
    }

    /// Accepts only `GET /?code=…&state=…` (or the provider's `error=…`) with this sign-in's state;
    /// anything else (a favicon, a stale tab) is answered and ignored.
    private func handle(_ connection: NWConnection, head: String) {
        let requestLine = head.components(separatedBy: "\r\n").first ?? ""
        let parts = requestLine.split(separator: " ")
        guard outcome == nil, parts.count == 3, parts[0] == "GET",
              let components = URLComponents(string: "http://localhost" + String(parts[1])), components.path == "/" else {
            respond(connection, status: "404 Not Found", text: "Not found.")
            return
        }
        let query = Dictionary((components.queryItems ?? []).map { ($0.name, $0.value ?? "") }) { first, _ in first }
        guard query["state"] == state else {
            respond(connection, status: "400 Bad Request",
                    text: "This sign-in page is out of date. Start again from Mailstream.")
            return
        }
        if let code = query["code"], !code.isEmpty {
            respond(connection, status: "200 OK", text: nil)
            finish(.success(code))
        } else {
            let reason = query["error_description"].flatMap { $0.isEmpty ? nil : $0 }
                ?? query["error"] ?? "The provider sent no authorization code."
            respond(connection, status: "200 OK", text: "Sign-in failed: \(reason)")
            finish(.failure(OAuthError(code: query["error"] ?? "response", message: "Sign-in failed: \(reason)")))
        }
    }

    /// `text` nil: the success page.
    private func respond(_ connection: NWConnection, status: String, text: String?) {
        let message = text ?? "You can close this tab and return to Mailstream."
        let escaped = message.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;").replacingOccurrences(of: "\"", with: "&quot;")
        let html = """
            <!doctype html><html><head><meta charset="utf-8"><title>Mailstream</title></head>\
            <body style="font: 15px -apple-system, sans-serif; margin: 4em auto; max-width: 32em; text-align: center">\
            <p>\(escaped)</p></body></html>
            """
        let body = Data(html.utf8)
        let head = "HTTP/1.1 \(status)\r\nContent-Type: text/html; charset=utf-8\r\n"
            + "Content-Length: \(body.count)\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
        connection.send(content: Data(head.utf8) + body, completion: .contentProcessed { _ in connection.cancel() })
    }

    private func finish(_ result: Result<String, any Error>) {
        guard outcome == nil else { return }
        outcome = result
        listener.stateUpdateHandler = nil
        listener.newConnectionHandler = nil
        listener.cancel()
        continuation?.resume(with: result)
        continuation = nil
    }
}
