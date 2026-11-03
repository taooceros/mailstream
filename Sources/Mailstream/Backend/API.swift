import Foundation

enum RequestPriority: Sendable {
    case interactive, background
}

/// A request a provider refused: the HTTP status and the provider's message.
struct APIError: LocalizedError, Sendable {
    let status: Int
    let message: String
    var errorDescription: String? { message }
}

/// Access tokens by account id.
protocol TokenSource: Sendable {
    func token(for account: String) async throws -> String
    /// Forgets `token` after a server rejected it, so the next `token(for:)` refreshes.
    func reject(_ token: String, account: String) async
}

/// Authorized HTTPS requests for one account. A 401 refreshes the token and repeats once; rate
/// limiting (429, or Google's 403 rate-limit reasons: never processed) backs off and repeats; 5xx
/// and dropped connections repeat only requests that may run twice. Concurrency is bounded per
/// priority so background prefetch never starves interactive reads, and requests with a `cost`
/// first spend it from the account's `QuotaBudget`.
final class API: Sendable {
    let account: String
    private let tokens: any TokenSource
    private let budget: QuotaBudget?
    private let interactive = AsyncSemaphore(6)
    private let background = AsyncSemaphore(3)

    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpMaximumConnectionsPerHost = 8
        configuration.timeoutIntervalForRequest = 30
        configuration.urlCache = nil
        return URLSession(configuration: configuration)
    }()

    init(account: String, tokens: any TokenSource, budget: QuotaBudget? = nil) {
        self.account = account
        self.tokens = tokens
        self.budget = budget
    }

    /// A request with `body` encoded as JSON when given.
    static func request(_ url: URL, method: String = "GET", json body: (any Encodable)? = nil,
                        headers: [String: String] = [:]) -> URLRequest {
        var request = URLRequest(url: url)
        request.httpMethod = method
        for (name, value) in headers { request.setValue(value, forHTTPHeaderField: name) }
        if let body {
            request.httpBody = try? JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return request
    }

    func decode<T: Decodable>(_ type: T.Type, _ request: URLRequest, priority: RequestPriority = .interactive,
                              cost: Int = 0, decoder: JSONDecoder = JSONDecoder()) async throws -> T {
        let (data, _) = try await send(request, priority: priority, cost: cost)
        do {
            return try decoder.decode(T.self, from: data)
        } catch {
            throw APIError(status: 200, message: "Unexpected answer from \(request.url?.host() ?? "the server"): \(error)")
        }
    }

    func get<T: Decodable>(_ type: T.Type, _ url: URL, priority: RequestPriority = .interactive, cost: Int = 0,
                           decoder: JSONDecoder = JSONDecoder()) async throws -> T {
        try await decode(type, Self.request(url), priority: priority, cost: cost, decoder: decoder)
    }

    /// Sends `request`; any status outside 200–299 throws `APIError`. `idempotent` defaults to
    /// every method but POST. `cost` is the request's units of the provider's per-user quota.
    func send(_ request: URLRequest, priority: RequestPriority = .interactive, idempotent: Bool? = nil,
              cost: Int = 0) async throws -> (Data, HTTPURLResponse) {
        let repeatable = idempotent ?? (request.httpMethod != "POST")
        if let budget, cost > 0 { try await budget.spend(cost, priority: priority) }
        let semaphore = priority == .interactive ? interactive : background
        return try await semaphore.withPermit { [self] in
            var refreshed = false
            var attempt = 0
            while true {
                attempt += 1
                let token = try await tokens.token(for: account)
                var authorized = request
                authorized.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                let data: Data, response: URLResponse
                do {
                    (data, response) = try await Self.session.data(for: authorized)
                } catch let error as URLError where repeatable && attempt < 3 && Self.transient.contains(error.code) {
                    try await Task.sleep(for: .milliseconds(300 * attempt))
                    continue
                }
                guard let http = response as? HTTPURLResponse else {
                    throw APIError(status: 0, message: "No HTTP response from \(request.url?.host() ?? "the server")")
                }
                switch http.statusCode {
                case 200..<300:
                    return (data, http)
                case 401 where !refreshed:
                    refreshed = true
                    await tokens.reject(token, account: account)
                    continue
                case 429 where attempt < 6, 403 where attempt < 6 && Self.isRateLimit(data),
                     500..<600 where repeatable && attempt < 4:
                    try await Task.sleep(for: Self.delay(http, attempt: attempt))
                    continue
                default:
                    throw APIError(status: http.statusCode, message: Self.message(data, status: http.statusCode))
                }
            }
        }
    }

    /// Google refuses over-quota requests with 403 and one of these reasons; nothing ran.
    static func isRateLimit(_ data: Data) -> Bool {
        struct Body: Decodable {
            struct Inner: Decodable {
                struct Reason: Decodable { let reason: String? }
                let errors: [Reason]?
                let status: String?
            }
            let error: Inner?
        }
        guard let error = (try? JSONDecoder().decode(Body.self, from: data))?.error else { return false }
        let reasons: Set<String> = ["rateLimitExceeded", "userRateLimitExceeded", "quotaExceeded"]
        return error.status == "RESOURCE_EXHAUSTED" || (error.errors ?? []).contains { reasons.contains($0.reason ?? "") }
    }

    private static let transient: Set<URLError.Code> = [.timedOut, .networkConnectionLost, .cannotConnectToHost,
                                                         .dnsLookupFailed, .notConnectedToInternet]

    /// Retry-After when given (seconds), else exponential from half a second, capped at 8 s.
    private static func delay(_ response: HTTPURLResponse, attempt: Int) -> Duration {
        if let header = response.value(forHTTPHeaderField: "Retry-After"), let seconds = Double(header) {
            return .milliseconds(Int(min(seconds, 30) * 1000))
        }
        return .milliseconds(min(500 << (attempt - 1), 8000))
    }

    /// Google and Graph both answer `{"error": {"message": …}}`.
    private static func message(_ data: Data, status: Int) -> String {
        struct Body: Decodable {
            struct Inner: Decodable { let message: String? }
            let error: Inner?
        }
        if let message = (try? JSONDecoder().decode(Body.self, from: data))?.error?.message, !message.isEmpty {
            return message
        }
        let text = String(decoding: data.prefix(300), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? "The server answered \(status)" : text
    }
}
