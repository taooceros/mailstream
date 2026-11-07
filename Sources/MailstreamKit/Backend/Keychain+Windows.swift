#if os(Windows)
import CWindows
import Foundation

/// Sign-ins on Windows, in the role the login keychain has on the Mac: `SignIns` as JSON,
/// encrypted with the system's data protection (DPAPI) for the signed-in Windows user, in one
/// file, `%LOCALAPPDATA%\Mailstream\sign-ins.dat`. Other users, and copies of the file taken to
/// another account or machine, cannot decrypt it.
nonisolated enum Keychain {
    private static let file = Platform.support.appendingPathComponent("sign-ins.dat")

    struct Failure: LocalizedError, Sendable {
        let action: String
        let code: Int32
        var errorDescription: String? { "Could not \(action) the saved sign-ins (Windows error \(code))." }
    }

    /// The stored sign-ins; empty when there are none.
    static func load() throws -> SignIns {
        guard let sealed = try? Data(contentsOf: file) else { return SignIns() }
        let data = try transform(sealed, ms_unprotect, action: "read")
        return try JSONDecoder().decode(SignIns.self, from: data)
    }

    /// Replaces the stored sign-ins; with nothing left to keep the file is deleted.
    static func store(_ signIns: SignIns) throws {
        guard !signIns.refreshTokens.isEmpty || !signIns.lastClients.isEmpty else {
            if FileManager.default.fileExists(atPath: file.path) { try FileManager.default.removeItem(at: file) }
            return
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let sealed = try transform(try encoder.encode(signIns), ms_protect, action: "save")
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try sealed.write(to: file, options: .atomic)
    }

    private typealias Transform = (UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
                                   UnsafeMutablePointer<Int>?) -> Int32

    private static func transform(_ data: Data, _ function: Transform, action: String) throws -> Data {
        var output: UnsafeMutablePointer<UInt8>?
        var length = 0
        let code = data.withUnsafeBytes { bytes in
            function(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, &output, &length)
        }
        guard code == 0, let output else { throw Failure(action: action, code: code) }
        defer { ms_free(output) }
        return Data(bytes: output, count: length)
    }
}
#endif
