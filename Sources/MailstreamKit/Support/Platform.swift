import Foundation
#if os(Windows)
import CWindows
#else
import AppKit
import UniformTypeIdentifiers
#endif

/// The few system services the engine needs that the Mac reaches through Apple frameworks and
/// Windows through `CWindows`.
nonisolated enum Platform {
    /// The user's caches folder: ~/Library/Caches on the Mac. Windows has none, so it is a folder
    /// in Mailstream's under %LOCALAPPDATA%.
    static let caches: URL = {
        #if os(Windows)
        URL.applicationSupportDirectory.appending(path: "Mailstream/Caches", directoryHint: .isDirectory)
        #else
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        #endif
    }()

    /// Opens `url` in its default application (the browser for a web page).
    @MainActor
    static func open(_ url: URL) {
        #if os(Windows)
        _ = ms_open_url(url.absoluteString)
        #else
        NSWorkspace.shared.open(url)
        #endif
    }

    /// The MIME type the system registers for files ending in `pathExtension`.
    static func mimeType(forExtension pathExtension: String) -> String? {
        guard !pathExtension.isEmpty else { return nil }
        #if os(Windows)
        var buffer = [CChar](repeating: 0, count: 256)
        guard ms_mime_type(pathExtension, &buffer, buffer.count) == 0 else { return nil }
        return String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        #else
        return UTType(filenameExtension: pathExtension)?.preferredMIMEType
        #endif
    }

    /// `data` decoded from `charset`, an IANA name; nil when the charset is unknown or the bytes
    /// are not text in it.
    static func decode(_ data: Data, charset: String) -> String? {
        #if os(Windows)
        data.withUnsafeBytes { bytes -> String? in
            var text: UnsafeMutablePointer<CChar>?
            var length = 0
            guard ms_decode_charset(charset, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, &text, &length) == 0,
                  let text else { return nil }
            defer { ms_free(text) }
            return String(decoding: UnsafeRawBufferPointer(start: text, count: length), as: UTF8.self)
        }
        #else
        let cf = CFStringConvertIANACharSetNameToEncoding(charset as CFString)
        guard cf != kCFStringEncodingInvalidId else { return nil }
        return String(data: data, encoding: String.Encoding(rawValue: CFStringConvertEncodingToNSStringEncoding(cf)))
        #endif
    }
}
