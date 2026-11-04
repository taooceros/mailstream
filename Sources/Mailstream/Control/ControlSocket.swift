import Foundation
import Network

/// The app's control socket: `~/Library/Application Support/Mailstream/control.sock`, mode 0600,
/// so only the user's own processes reach it. One request per connection: the client writes one
/// JSON object and a newline, the app answers with one JSON object and closes.
///
/// Request: `{"command": "mail.list", "args": {…}}`.
/// Answer: `{"ok": true, "result": …}` or `{"ok": false, "error": {"code": …, "message": …}}`.
///
/// These are the CLI's side: blocking POSIX calls, which suit its short-lived process.
nonisolated enum ControlSocket {
    static let path = SettingsFile.directory.appending(path: "control.sock").path
    /// Requests larger than this are refused.
    static let maxRequest = 16 << 20

    fileprivate static func address() throws -> (sockaddr_un, socklen_t) {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard bytes.count < capacity else { throw ControlError("socket", "Socket path is too long: \(path)") }
        withUnsafeMutableBytes(of: &address.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        return (address, socklen_t(MemoryLayout<sockaddr_un>.size))
    }

    /// A connected socket, or nil when nothing listens.
    static func connect() -> Int32? {
        guard let (address, length) = try? address() else { return nil }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        var copy = address
        let status = withUnsafePointer(to: &copy) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, length) }
        }
        guard status == 0 else {
            close(fd)
            return nil
        }
        return fd
    }

    static func writeAll(_ fd: Int32, _ data: Data) -> Bool {
        data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let written = write(fd, raw.baseAddress! + offset, raw.count - offset)
                if written < 0 {
                    if errno == EINTR { continue }
                    return false
                }
                offset += written
            }
            return true
        }
    }

    /// Bytes up to the first newline (or the end), at most `limit`.
    static func readMessage(_ fd: Int32, limit: Int) -> Data? {
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 64 << 10)
        while data.count <= limit {
            let count = read(fd, &buffer, buffer.count)
            if count < 0 {
                if errno == EINTR { continue }
                return nil
            }
            if count == 0 { break }
            if let newline = buffer[0..<count].firstIndex(of: 0x0A) {
                data.append(contentsOf: buffer[0..<newline])
                return data
            }
            data.append(contentsOf: buffer[0..<count])
        }
        return data.count <= limit ? data : nil
    }
}

/// The app's side of the control socket: a `NetworkListener` on the Unix-domain path that hands
/// each connection's request to a handler, connections served concurrently.
nonisolated struct ControlServer: Sendable {
    private let listener: NetworkListener<TCP>

    /// Nil when another Mailstream already serves the socket, so a second copy never steals it.
    /// The probe is a Unix-domain connect, which answers at once; a stale socket file is replaced.
    static func claim() throws -> ControlServer? {
        if let existing = ControlSocket.connect() {
            close(existing)
            return nil
        }
        try FileManager.default.createDirectory(at: SettingsFile.directory, withIntermediateDirectories: true)
        unlink(ControlSocket.path)
        let listener = try NetworkListener(using: .parameters { TCP() }
            .localEndpoint(.unix(path: ControlSocket.path)))
        // Bound with the default mode first; ~/Library is private to the user, so nobody else
        // reaches the path before it is narrowed.
        listener.onStateUpdate { _, state in
            if case .ready = state { chmod(ControlSocket.path, 0o600) }
        }
        return ControlServer(listener: listener)
    }

    /// Serves requests until the calling task is cancelled.
    @concurrent func run(_ handler: @escaping @Sendable (Data) async -> Data) async throws {
        try await listener.run { connection in
            do {
                guard let request = try await Self.receive(connection, limit: ControlSocket.maxRequest) else { return }
                var answer = await handler(request)
                answer.append(0x0A)
                try await connection.send(answer, endOfStream: true)
            } catch {
                // The client went away; nothing to answer.
            }
        }
    }

    /// Deletes the socket file: the app is quitting.
    func removeSocket() {
        unlink(ControlSocket.path)
    }

    /// Bytes up to the first newline or the end of the stream; nil past `limit`.
    private static func receive(_ connection: NetworkConnection<TCP>, limit: Int) async throws -> Data? {
        var data = Data()
        while data.count <= limit {
            let (chunk, metadata) = try await connection.receive(atMost: 64 << 10)
            if let newline = chunk.firstIndex(of: 0x0A) {
                data.append(chunk[..<newline])
                break
            }
            data.append(chunk)
            if metadata.endOfStream { break }
        }
        return data.count <= limit ? data : nil
    }
}
