import Foundation

/// The app's control socket: `~/Library/Application Support/Mailstream/control.sock`, mode 0600,
/// accepting only the user's own processes. One request per connection: the client writes one
/// JSON object and a newline, the app answers with one JSON object and closes.
///
/// Request: `{"command": "mail.list", "args": {…}}`.
/// Answer: `{"ok": true, "result": …}` or `{"ok": false, "error": {"code": …, "message": …}}`.
enum ControlSocket {
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

/// Listens on the control socket on a background thread and hands each request to `handler`.
final class ControlServer: Sendable {
    private let fd: Int32

    private init(fd: Int32) {
        self.fd = fd
    }

    /// Nil when another Mailstream already serves the socket, so a second copy never steals it.
    static func start(handler: @escaping @Sendable (Data) async -> Data) throws -> ControlServer? {
        if let existing = ControlSocket.connect() {
            close(existing)
            return nil
        }
        try FileManager.default.createDirectory(at: SettingsFile.directory, withIntermediateDirectories: true)
        unlink(ControlSocket.path)
        let (address, length) = try ControlSocket.address()
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ControlError("socket", "socket(): \(String(cString: strerror(errno)))") }
        var copy = address
        let bound = withUnsafePointer(to: &copy) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, length) }
        }
        guard bound == 0, chmod(ControlSocket.path, 0o600) == 0, listen(fd, 16) == 0 else {
            let message = String(cString: strerror(errno))
            close(fd)
            throw ControlError("socket", "Cannot listen on \(ControlSocket.path): \(message)")
        }
        let server = ControlServer(fd: fd)
        let thread = Thread { server.acceptLoop(handler) }
        thread.name = "Mailstream control socket"
        thread.start()
        return server
    }

    /// Removes the socket file; the process is exiting.
    func stop() {
        unlink(ControlSocket.path)
    }

    private func acceptLoop(_ handler: @escaping @Sendable (Data) async -> Data) {
        while true {
            let client = accept(fd, nil, nil)
            if client < 0 {
                if errno == EINTR || errno == ECONNABORTED { continue }
                return
            }
            var uid: uid_t = 0, gid: gid_t = 0
            guard getpeereid(client, &uid, &gid) == 0, uid == getuid() else {
                close(client)
                continue
            }
            var noSigPipe: Int32 = 1
            setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
            DispatchQueue.global(qos: .userInitiated).async {
                guard let request = ControlSocket.readMessage(client, limit: ControlSocket.maxRequest) else {
                    close(client)
                    return
                }
                Task {
                    var response = await handler(request)
                    response.append(0x0A)
                    _ = ControlSocket.writeAll(client, response)
                    close(client)
                }
            }
        }
    }
}
