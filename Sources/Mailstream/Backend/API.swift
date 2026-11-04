import Foundation

nonisolated enum RequestPriority: Sendable {
    case interactive, background
}

/// A request a provider refused: the HTTP status and the provider's message.
nonisolated struct APIError: LocalizedError, Sendable {
    let status: Int
    let message: String
    var errorDescription: String? { message }

    init(status: Int, message: String) {
        self.status = status
        self.message = message
    }

    /// A refusal from the error answer `body`: its message, else the start of the body.
    init(status: Int, body: Data) {
        let text = String(decoding: body.prefix(300), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        self.status = status
        message = Body(body)?.message ?? (text.isEmpty ? "The server answered \(status)" : text)
    }

    /// The error answer Google and Graph share: `{"error": {"message": …}}`, plus Google's
    /// `status` and `errors[].reason`.
    struct Body: Decodable, Sendable {
        struct Detail: Decodable, Sendable {
            struct Reason: Decodable, Sendable { let reason: String? }
            let message: String?
            let status: String?
            let errors: [Reason]?
        }
        let error: Detail?

        init?(_ data: Data) {
            guard let body = try? JSONDecoder().decode(Self.self, from: data) else { return nil }
            self = body
        }

        var message: String? { error?.message.flatMap { $0.isEmpty ? nil : $0 } }

        /// Google refuses over-quota requests with 403 and one of these reasons; nothing ran.
        var isRateLimit: Bool {
            guard let error else { return false }
            let reasons: Set<String> = ["rateLimitExceeded", "userRateLimitExceeded", "quotaExceeded"]
            return error.status == "RESOURCE_EXHAUSTED" || (error.errors ?? []).contains { reasons.contains($0.reason ?? "") }
        }
    }
}

/// Access tokens by account id.
nonisolated protocol TokenSource: Sendable {
    func token(for account: String) async throws -> String
    /// Forgets `token` after a server rejected it, so the next `token(for:)` refreshes.
    func reject(_ token: String, account: String) async
}

/// Authorized HTTPS requests for one account. A 401 refreshes the token and repeats once; rate
/// limiting (429, or Google's 403 rate-limit reasons: never processed) backs off and repeats, and
/// also pauses the account's `QuotaBudget` so its other requests stop adding to the overload; 5xx
/// and dropped connections repeat only requests that may run twice. Concurrency is bounded per
/// priority so background prefetch never starves interactive reads; a request with a `cost`
/// spends it from the account's `QuotaBudget` on every attempt, once it holds a permit, so
/// queued requests neither stampede the budget nor spend it long before they go out. Decoding
/// runs off the caller's actor.
nonisolated final class API: Sendable {
    let account: String
    private let tokens: any TokenSource
    private let budget: QuotaBudget?
    private let interactive: AsyncSemaphore
    private let background: AsyncSemaphore

    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpMaximumConnectionsPerHost = 8
        configuration.timeoutIntervalForRequest = 30
        configuration.urlCache = nil
        return URLSession(configuration: configuration)
    }()

    /// Outlook refuses more than four requests at once per mailbox (MailboxConcurrency), so Graph
    /// accounts pass 3 + 1; Gmail takes 6 + 3.
    init(account: String, tokens: any TokenSource, budget: QuotaBudget? = nil,
         interactive: Int = 6, background: Int = 3) {
        self.account = account
        self.tokens = tokens
        self.budget = budget
        self.interactive = AsyncSemaphore(interactive)
        self.background = AsyncSemaphore(background)
    }

    /// The provider refused for load: no request of this account spends quota for `delay`.
    func throttled(for delay: Duration) async {
        await budget?.pause(for: delay)
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

    @concurrent
    func decode<T: Decodable & Sendable>(_ type: T.Type, _ request: URLRequest, priority: RequestPriority = .interactive,
                                         cost: Int = 0, decoder: JSONDecoder = JSONDecoder()) async throws -> T {
        let (data, _) = try await send(request, priority: priority, cost: cost)
        do {
            return try decoder.decode(T.self, from: data)
        } catch {
            throw APIError(status: 200, message: "Unexpected answer from \(request.url?.host() ?? "the server"): \(error)")
        }
    }

    func get<T: Decodable & Sendable>(_ type: T.Type, _ url: URL, priority: RequestPriority = .interactive, cost: Int = 0,
                                      decoder: JSONDecoder = JSONDecoder()) async throws -> T {
        try await decode(type, Self.request(url), priority: priority, cost: cost, decoder: decoder)
    }

    /// Sends `request`; any status outside 200–299 throws `APIError`. `idempotent` defaults to
    /// every method but POST. `cost` is the request's units of the provider's per-user quota.
    func send(_ request: URLRequest, priority: RequestPriority = .interactive, idempotent: Bool? = nil,
              cost: Int = 0) async throws -> (Data, HTTPURLResponse) {
        let repeatable = idempotent ?? (request.httpMethod != "POST")
        let semaphore = priority == .interactive ? interactive : background
        return try await semaphore.withPermit { [self] in
            var refreshed = false
            var attempt = 0
            while true {
                attempt += 1
                if let budget, cost > 0 { try await budget.spend(cost, priority: priority) }
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
                case 429 where attempt < Self.rateLimitAttempts,
                     403 where attempt < Self.rateLimitAttempts && APIError.Body(data)?.isRateLimit == true,
                     500..<600 where repeatable && attempt < 4:
                    let retryAfter = http.value(forHTTPHeaderField: "Retry-After").flatMap(Double.init)
                    let delay = Self.backoff(attempt: attempt, retryAfter: retryAfter)
                    if http.statusCode < 500 { await throttled(for: delay) }
                    try await Task.sleep(for: delay)
                    continue
                default:
                    throw APIError(status: http.statusCode, body: data)
                }
            }
        }
    }

    /// Sends `items` through `send`, which answers the ones to send again (throttled or left
    /// unanswered) and the longest Retry-After among them, and repeats those after `backoff`,
    /// pausing the account's budget meanwhile, until none are left. `service` names the provider
    /// when it keeps refusing.
    func repeatingThrottled<Item>(_ items: [Item], service: String,
                                  _ send: ([Item]) async throws -> (retry: [Item], retryAfter: Double?)) async throws {
        var pending = items
        var attempt = 0
        while !pending.isEmpty {
            attempt += 1
            let (retry, retryAfter) = try await send(pending)
            guard !retry.isEmpty else { return }
            guard attempt < Self.rateLimitAttempts else {
                throw APIError(status: 429, message: "\(service) is limiting requests; try again shortly")
            }
            let delay = Self.backoff(attempt: attempt, retryAfter: retryAfter)
            await throttled(for: delay)
            try await Task.sleep(for: delay)
            pending = retry
        }
    }

    /// Attempts at a request the provider keeps throttling before giving up.
    private static let rateLimitAttempts = 6

    private static let transient: Set<URLError.Code> = [.timedOut, .networkConnectionLost, .cannotConnectToHost,
                                                         .dnsLookupFailed, .notConnectedToInternet]

    /// The pause before repeating attempt `attempt`: Retry-After when given (seconds, at most 30),
    /// else exponential from half a second, capped at 8 s.
    private static func backoff(attempt: Int, retryAfter: Double?) -> Duration {
        if let retryAfter { return .milliseconds(Int(min(retryAfter, 30) * 1000)) }
        return .milliseconds(min(500 << (attempt - 1), 8000))
    }
}
