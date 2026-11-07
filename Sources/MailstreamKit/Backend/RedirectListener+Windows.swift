#if os(Windows)
import CWindows
import Foundation

/// The OAuth redirect listener on Windows: a non-blocking WinSock socket on 127.0.0.1, checked
/// every 50 ms by a task (Windows has no Network.framework). Answers the browser with a small page
/// and stops at the first sign-in result, after five minutes, or when the task running `code()` is
/// cancelled, which closes the socket.
nonisolated struct RedirectListener: Sendable {
    private let listener: ms_socket
    private let state: String

    init(state: String) throws {
        var listener: ms_socket = 0
        let error = ms_listen_loopback(RedirectPage.port, &listener)
        guard error == 0 else { throw RedirectPage.failure(SocketError(code: error)) }
        self.listener = listener
        self.state = state
    }

    /// The socket listens from `init` on.
    func ready() async -> Bool { true }

    /// Listens and returns the authorization code.
    func code() async throws -> String {
        defer { ms_close_socket(listener) }
        let deadline = ContinuousClock.now + .seconds(300)
        var connections: [Connection] = []
        defer { connections.forEach { ms_close_socket($0.socket) } }
        while true {
            try Task.checkCancellation()
            guard ContinuousClock.now < deadline else {
                throw OAuthError(code: "timeout", message: "The sign-in timed out after five minutes.")
            }
            var socket: ms_socket = 0
            switch ms_accept(listener, &socket) {
            case 0: connections.append(Connection(socket: socket))
            case 1: break
            case let error: throw RedirectPage.failure(SocketError(code: error))
            }
            var open: [Connection] = []
            for var connection in connections {
                if let outcome = connection.step(state: state) { return try outcome.get() }
                if connection.done { ms_close_socket(connection.socket) } else { open.append(connection) }
            }
            connections = open
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    /// One browser connection: reads the request head, then writes the answer.
    private struct Connection {
        let socket: ms_socket
        var received = Data()
        var unsent: Data?
        var outcome: Result<String, any Error>?
        var done = false

        /// Moves the exchange on without blocking; the sign-in's result once the redirect is answered.
        mutating func step(state: String) -> Result<String, any Error>? {
            if unsent == nil { read(state: state) }
            guard var pending = unsent, !done else { return nil }
            while !pending.isEmpty {
                var sent = 0
                let status = pending.withUnsafeBytes { ms_send(socket, $0.bindMemory(to: UInt8.self).baseAddress, $0.count, &sent) }
                if status == 1 { break }
                guard status == 0 else { done = true; return outcome }
                pending.removeFirst(sent)
            }
            unsent = pending
            if pending.isEmpty { done = true; return outcome }
            return nil
        }

        private mutating func read(state: String) {
            var buffer = [UInt8](repeating: 0, count: 16_384)
            while true {
                var count = 0
                let status = ms_receive(socket, &buffer, buffer.count, &count)
                if status == 1 { return }
                // An error, or the browser closed it before sending a whole request.
                guard status == 0, count > 0 else { done = true; return }
                received.append(contentsOf: buffer[..<count])
                if let head = RedirectPage.head(in: received) {
                    (unsent, outcome) = RedirectPage.answer(head: head, state: state)
                    return
                }
                if received.count > 65_536 { done = true; return }
            }
        }
    }

    private struct SocketError: LocalizedError {
        let code: Int32
        var errorDescription: String? { "WinSock error \(code)" }
    }
}
#endif
