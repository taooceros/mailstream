import AppKit
import Network
import Observation

/// Live mail changes through the push relay: one WebSocket carrying Gmail Pub/Sub and Graph change
/// notifications, plus the Gmail watches and Graph subscriptions that make the providers send them.
/// Without push configuration it stays idle and the store keeps polling.
/// `isConnected` is observable; the mail store polls less while it is true.
@Observable
final class PushService {
    private(set) var isConnected = false

    @ObservationIgnored private let accounts: AccountStore
    @ObservationIgnored private let onChange: (String) -> Void
    @ObservationIgnored private let config = AppConfig.current
    @ObservationIgnored private var started = false
    /// Account changes, network paths and the hourly renewal.
    @ObservationIgnored private var watchers: [Task<Void, Never>] = []
    @ObservationIgnored private var wake: NotificationCenter.ObservationToken?
    @ObservationIgnored private var pathSatisfied = true

    @ObservationIgnored private var socket: URLSessionWebSocketTask?
    /// Bumped whenever a socket opens or closes, so loops of a replaced socket stop.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var lastHeard: ContinuousClock.Instant?
    @ObservationIgnored private var listener: Task<Void, Never>?
    @ObservationIgnored private var heartbeat: Task<Void, Never>?
    @ObservationIgnored private var reconnect: Task<Void, Never>?
    @ObservationIgnored private var failures = 0

    @ObservationIgnored private var registrations: [String: Registration]
    /// The API of every account seen, kept so a removed account's registration can still be stopped.
    @ObservationIgnored private var apis: [String: API] = [:]
    /// Graph accounts whose subscription asked to be reauthorized.
    @ObservationIgnored private var reauthorize: Set<String> = []
    /// Accounts whose last registration failure was already logged.
    @ObservationIgnored private var reported: Set<String> = []
    /// One registration pass at a time; a request during a pass runs another after it.
    @ObservationIgnored private lazy var registration = CoalescedTask("Push registrations") { [weak self] in
        await self?.reconcile()
    }

    private static let session = URLSession(configuration: .ephemeral)
    private static let gmail = URL(string: "https://gmail.googleapis.com/gmail/v1/users/me")!
    private static let subscriptions = URL(string: "https://graph.microsoft.com/v1.0/subscriptions")!
    private static let file = URL.applicationSupportDirectory.appending(path: "Mailstream/push.json")
    /// Requested lifetime of a Graph subscription (70 h); renewed with a day left.
    private static let graphLifetime: TimeInterval = 4200 * 60

    init(accounts: AccountStore, onChange: @escaping (String) -> Void) {
        self.accounts = accounts
        self.onChange = onChange
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        registrations = (try? Data(contentsOf: Self.file)).flatMap { try? decoder.decode([String: Registration].self, from: $0) } ?? [:]
    }

    isolated deinit {
        watchers.forEach { $0.cancel() }
        listener?.cancel()
        heartbeat?.cancel()
        reconnect?.cancel()
        if let wake { NSWorkspace.shared.notificationCenter.removeObserver(wake) }
    }

    /// Connects the socket and registers every account; follows account changes, wake and
    /// network changes, and renews registrations hourly.
    func start() {
        guard !started, socketConfigured else { return }
        started = true
        let accounts = accounts
        watchers = [
            Task.immediate(name: "Push account changes") { [weak self] in
                for await _ in Observations({ accounts.records }).dropFirst() { self?.checkRegistrations() }
            },
            Task(name: "Push network path") { [weak self] in
                for await path in NWPathMonitor() { self?.pathChanged(satisfied: path.status == .satisfied) }
            },
            Task(name: "Push renewal") { [weak self] in
                while true {
                    do { try await Task.sleep(for: .seconds(3600)) } catch { return }
                    self?.checkRegistrations()
                }
            },
        ]
        wake = NSWorkspace.shared.notificationCenter.addObserver(of: NSWorkspace.shared, for: .didWake) { [weak self] _ in
            self?.reconnectNow()
            self?.checkRegistrations()
        }
        // A removed account's registration is stopped before its token goes.
        accounts.beforeSignOut = { [weak self] in await self?.registration.settle() }
        connect()
        checkRegistrations()
    }

    private var socketConfigured: Bool {
        config.pushURL != nil && !(config.pushToken ?? "").isEmpty
    }

    // MARK: - Socket

    private func connect() {
        guard socket == nil, let pushURL = config.pushURL, let token = config.pushToken,
              var components = URLComponents(url: pushURL.appending(path: "socket"), resolvingAgainstBaseURL: false)
        else { return }
        components.scheme = "wss"
        guard let url = components.url else { return }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let socket = Self.session.webSocketTask(with: request)
        generation += 1
        let generation = self.generation
        self.socket = socket
        lastHeard = nil
        socket.resume()
        listener = Task(name: "Push socket listener") { await listen(socket, generation: generation) }
        heartbeat = Task(name: "Push socket heartbeat") { await beat(socket, generation: generation) }
    }

    private func listen(_ socket: URLSessionWebSocketTask, generation: Int) async {
        while true {
            let message: URLSessionWebSocketTask.Message
            do {
                message = try await socket.receive()
            } catch {
                fail(generation)
                return
            }
            guard generation == self.generation else { return }
            heard()
            switch message {
            case .string(let text) where text == "pong":
                break
            case .string(let text):
                handle(Data(text.utf8))
            case .data(let data):
                handle(data)
            @unknown default:
                break
            }
        }
    }

    /// Pings every 30 s; a socket that says nothing within 15 s of a ping is dead. The first ping
    /// goes out at once, so its pong is what marks the socket connected.
    private func beat(_ socket: URLSessionWebSocketTask, generation: Int) async {
        while generation == self.generation {
            let sent = ContinuousClock.now
            Task {
                do { try await socket.send(.string("ping")) } catch { fail(generation) }
            }
            do { try await Task.sleep(for: .seconds(15)) } catch { return }
            guard generation == self.generation else { return }
            guard let lastHeard, lastHeard >= sent else {
                fail(generation)
                return
            }
            do { try await Task.sleep(for: .seconds(15)) } catch { return }
        }
    }

    /// The store refreshes everything once the socket connects, so no per-account change is sent.
    private func heard() {
        lastHeard = .now
        guard !isConnected else { return }
        failures = 0
        setConnected(true)
    }

    private func fail(_ generation: Int) {
        guard generation == self.generation, socket != nil else { return }
        disconnect()
        scheduleReconnect()
    }

    private func disconnect() {
        generation += 1
        listener?.cancel()
        heartbeat?.cancel()
        listener = nil
        heartbeat = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        setConnected(false)
    }

    /// Exponential from 1 s to 60 s, with jitter so many clients do not reconnect in step.
    private func scheduleReconnect() {
        reconnect?.cancel()
        let delay = min(60, pow(2, Double(min(failures, 6))) * Double.random(in: 0.8...1.2))
        failures += 1
        reconnect = Task(name: "Push reconnect") {
            do { try await Task.sleep(for: .seconds(delay)) } catch { return }
            reconnect = nil
            connect()
        }
    }

    private func reconnectNow() {
        reconnect?.cancel()
        reconnect = nil
        failures = 0
        disconnect()
        connect()
    }

    private func pathChanged(satisfied: Bool) {
        guard satisfied != pathSatisfied else { return }
        pathSatisfied = satisfied
        if satisfied { reconnectNow() }
    }

    private func setConnected(_ connected: Bool) {
        if connected != isConnected { isConnected = connected }
    }

    // MARK: - Events

    private nonisolated struct Event: Decodable {
        let source: String
        let email: String?
        let subscriptionId: String?
        let lifecycleEvent: String?
    }

    private func handle(_ data: Data) {
        guard let event = try? JSONDecoder().decode(Event.self, from: data) else { return }
        switch event.source {
        case "gmail":
            guard let email = event.email?.lowercased(), accounts.record(email) != nil else { return }
            onChange(email)
        case "graph":
            guard let id = event.subscriptionId,
                  let account = registrations.first(where: { $0.value.kind == .graph && $0.value.id == id })?.key,
                  accounts.record(account) != nil else { return }
            switch event.lifecycleEvent {
            case "reauthorizationRequired":
                reauthorize.insert(account)
                checkRegistrations()
            case "subscriptionRemoved":
                registrations[account] = nil
                save()
                checkRegistrations()
                onChange(account)
            default:
                onChange(account)
            }
        default:
            break
        }
    }

    // MARK: - Registrations

    /// A Gmail watch (`id` is its Pub/Sub topic) or a Graph subscription (`id` is the subscription id).
    private nonisolated struct Registration: Codable {
        enum Kind: String, Codable { case gmail, graph }
        let kind: Kind
        let id: String
        let expires: Date

        /// Closer than a day to expiring: renew.
        var due: Bool { expires.timeIntervalSinceNow < 24 * 3600 }
    }

    private nonisolated struct GraphSubscription: Decodable, Sendable {
        let id: String
        let expirationDateTime: String?
    }

    private func checkRegistrations() { registration.request() }

    private func reconcile() async {
        for (account, registration) in registrations where accounts.record(account) == nil {
            registrations[account] = nil
            save()
            if let api = apis[account] ?? accounts.api(for: account) { await unregister(registration, api: api) }
        }
        apis = apis.filter { accounts.record($0.key) != nil }
        for record in accounts.records {
            guard let api = accounts.api(for: record.id) else { continue }
            apis[record.id] = api
            do {
                switch record.provider {
                case .google: try await watchGmail(record.id, api: api)
                case .microsoft: try await subscribeGraph(record.id, api: api)
                }
                reported.remove(record.id)
            } catch {
                if reported.insert(record.id).inserted {
                    NSLog("Push registration for %@ failed: %@", record.id, error.localizedDescription)
                }
            }
        }
    }

    /// Watches every change of the mailbox; renewing is the same call.
    private func watchGmail(_ account: String, api: API) async throws {
        guard let topic = config.gmailTopic, !topic.isEmpty else { return }
        if let current = registrations[account], current.kind == .gmail, current.id == topic, !current.due { return }
        struct Body: Encodable { let topicName: String }
        nonisolated struct Answer: Decodable, Sendable { let expiration: String }
        let request = API.request(Self.gmail.appending(path: "watch"), method: "POST", json: Body(topicName: topic))
        let answer = try await api.decode(Answer.self, request, priority: .background)
        guard let milliseconds = Double(answer.expiration) else {
            throw APIError(status: 200, message: "Gmail watch answered no expiration")
        }
        registrations[account] = Registration(kind: .gmail, id: topic,
                                              expires: Date(timeIntervalSince1970: milliseconds / 1000))
        save()
    }

    /// Renews a subscription that is due or asked for reauthorization; creates one when there is
    /// none or Graph no longer knows it.
    private func subscribeGraph(_ account: String, api: API) async throws {
        guard let pushURL = config.pushURL, let clientState = config.graphClientState, !clientState.isEmpty
        else { return }
        let forced = reauthorize.remove(account) != nil
        if let current = registrations[account], current.kind == .graph {
            guard forced || current.due else { return }
            do {
                try await renewGraph(account, id: current.id, api: api)
                return
            } catch let error as APIError where error.status == 404 {
                registrations[account] = nil
                save()
            } catch {
                if forced { reauthorize.insert(account) }
                throw error
            }
        }
        struct Body: Encodable {
            let changeType, notificationUrl, lifecycleNotificationUrl, resource, expirationDateTime, clientState: String
        }
        let expires = Date.now.addingTimeInterval(Self.graphLifetime)
        let endpoint = pushURL.appending(path: "graph").absoluteString
        let body = Body(changeType: "created,updated,deleted", notificationUrl: endpoint,
                        lifecycleNotificationUrl: endpoint, resource: "me/messages",
                        expirationDateTime: expires.formatted(.iso8601), clientState: clientState)
        let request = API.request(Self.subscriptions, method: "POST", json: body)
        let answer = try await api.decode(GraphSubscription.self, request, priority: .background)
        registrations[account] = Registration(kind: .graph, id: answer.id,
                                              expires: answer.expirationDateTime.flatMap(parseISODate) ?? expires)
        save()
    }

    private func renewGraph(_ account: String, id: String, api: API) async throws {
        struct Body: Encodable { let expirationDateTime: String }
        let expires = Date.now.addingTimeInterval(Self.graphLifetime)
        let request = API.request(Self.subscriptions.appending(path: id), method: "PATCH",
                                  json: Body(expirationDateTime: expires.formatted(.iso8601)))
        let answer = try await api.decode(GraphSubscription.self, request, priority: .background)
        registrations[account] = Registration(kind: .graph, id: id,
                                              expires: answer.expirationDateTime.flatMap(parseISODate) ?? expires)
        save()
    }

    /// Best effort: the account's token may already be gone, and registrations expire on their own.
    private func unregister(_ registration: Registration, api: API) async {
        let request = switch registration.kind {
        case .gmail: API.request(Self.gmail.appending(path: "stop"), method: "POST")
        case .graph: API.request(Self.subscriptions.appending(path: registration.id), method: "DELETE")
        }
        _ = try? await api.send(request, priority: .background, idempotent: true)
    }

    private func save() {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        do {
            try FileManager.default.createDirectory(at: Self.file.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            try encoder.encode(registrations).write(to: Self.file, options: .atomic)
        } catch {
            NSLog("Could not save push registrations: %@", error.localizedDescription)
        }
    }
}
