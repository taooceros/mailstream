import Foundation
import UniformTypeIdentifiers

/// An outgoing message as RFC 5322 bytes with CRLF line ends: the raw message Gmail sends and
/// Graph's MIME `sendMail` takes. Both providers drop the Bcc header when they send.
enum MIMEMessage {
    /// `messageID` without angle brackets.
    static func render(_ message: OutgoingMessage, messageID: String) throws -> Data {
        var headers = [addressField("From", [message.from])]
        if !message.to.isEmpty { headers.append(addressField("To", message.to)) }
        if !message.cc.isEmpty { headers.append(addressField("Cc", message.cc)) }
        if !message.bcc.isEmpty { headers.append(addressField("Bcc", message.bcc)) }
        headers.append(textField("Subject", message.subject))
        headers.append("Date: " + date(Date()))
        headers.append("Message-ID: " + angle(messageID))
        if let inReplyTo = message.inReplyTo, !inReplyTo.isEmpty {
            headers.append("In-Reply-To: " + angle(inReplyTo))
        }
        if !message.references.isEmpty {
            headers.append(listField("References", message.references.map(angle), separator: ""))
        }
        headers.append("MIME-Version: 1.0")

        let plain = Entity(headers: ["Content-Type: text/plain; charset=utf-8", "Content-Transfer-Encoding: quoted-printable"],
                           body: Data(quotedPrintable(message.text).utf8))
        var body = plain
        if let html = message.html {
            let rich = Entity(headers: ["Content-Type: text/html; charset=utf-8", "Content-Transfer-Encoding: base64"],
                              body: base64(Data(html.utf8)))
            body = multipart("alternative", [plain, rich])
        }
        if !message.attachments.isEmpty {
            body = multipart("mixed", [body] + (try message.attachments.map { try attachment($0) }))
        }
        body.headers = headers + body.headers
        return body.bytes
    }

    private struct Entity {
        var headers: [String]
        var body: Data

        var bytes: Data {
            var data = Data(headers.joined(separator: "\r\n").utf8)
            data.append(Data("\r\n\r\n".utf8))
            data.append(body)
            return data
        }
    }

    /// Boundaries start with `=_`, which neither quoted-printable nor base64 output contains.
    private static func multipart(_ subtype: String, _ parts: [Entity]) -> Entity {
        let boundary = "=_ms_" + UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        var body = Data()
        for part in parts {
            body.append(Data("--\(boundary)\r\n".utf8))
            body.append(part.bytes)
            body.append(Data("\r\n".utf8))
        }
        body.append(Data("--\(boundary)--\r\n".utf8))
        return Entity(headers: ["Content-Type: multipart/\(subtype); boundary=\"\(boundary)\""], body: body)
    }

    private static func attachment(_ url: URL) throws -> Entity {
        let bytes = try Data(contentsOf: url)
        let name = url.lastPathComponent.precomposedStringWithCanonicalMapping.filter { !$0.isNewline }
        let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
        let typeName = isPlainASCII(name) ? quoted(name) : "\"" + encodedWords(name).joined(separator: "\r\n ") + "\""
        let filename = isPlainASCII(name) ? "filename=" + quoted(name) : "filename*=UTF-8''" + percentEncoded(name)
        return Entity(headers: ["Content-Type: \(mime);\r\n name=\(typeName)",
                                "Content-Disposition: attachment;\r\n \(filename)",
                                "Content-Transfer-Encoding: base64"],
                      body: base64(bytes))
    }

    // MARK: Header fields

    private static func addressField(_ name: String, _ addresses: [Address]) -> String {
        listField(name, addresses.map(mailbox), separator: ",")
    }

    /// `name: item, item …`, folded between items to keep lines near 78 columns.
    private static func listField(_ name: String, _ items: [String], separator: String) -> String {
        var lines: [String] = []
        var line = name + ":"
        for (index, item) in items.enumerated() {
            let piece = item + (index < items.count - 1 ? separator : "")
            if line.count + 1 + piece.count > 78, line.count > name.count + 1 {
                lines.append(line)
                line = ""
            }
            line += " " + piece
        }
        lines.append(line)
        return lines.joined(separator: "\r\n")
    }

    /// Unstructured text (Subject): encoded-words when not plain ASCII, else folded at spaces.
    private static func textField(_ name: String, _ value: String) -> String {
        let value = value.split(whereSeparator: \.isNewline).joined(separator: " ")
        if !isPlainASCII(value) || value.contains("=?") {
            return name + ": " + encodedWords(value).joined(separator: "\r\n ")
        }
        var lines: [String] = []
        var line = name + ":"
        for word in value.split(separator: " ", omittingEmptySubsequences: false) {
            if line.count + 1 + word.count > 78, line.count > name.count + 1 {
                lines.append(line)
                line = ""
            }
            line += " " + word
        }
        lines.append(line)
        return lines.joined(separator: "\r\n")
    }

    private static func mailbox(_ address: Address) -> String {
        let name = (address.name ?? "").split(whereSeparator: { $0.isNewline || $0 == "\t" }).joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, name != address.email else { return address.email }
        return phrase(name) + " <\(address.email)>"
    }

    /// A display name: atoms as they are, other ASCII quoted, anything else encoded-words.
    private static func phrase(_ text: String) -> String {
        if !isPlainASCII(text) { return encodedWords(text).joined(separator: " ") }
        let atom = !text.contains("=?") && text.allSatisfy { $0 == " " || $0.isLetter || $0.isNumber || atextSymbols.contains($0) }
        return atom ? text : quoted(text)
    }

    private static let atextSymbols: Set<Character> = Set("!#$%&'*+-/=?^_`{|}~")

    private static func quoted(_ text: String) -> String {
        "\"" + text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
    }

    private static func isPlainASCII(_ text: String) -> Bool {
        text.unicodeScalars.allSatisfy { $0.value >= 0x20 && $0.value < 0x7F }
    }

    /// RFC 2047 B-encoded UTF-8 words of at most 75 characters, split between characters.
    private static func encodedWords(_ text: String) -> [String] {
        var words: [String] = []
        var chunk = Data()
        for character in text {
            let bytes = Data(String(character).utf8)
            if chunk.count + bytes.count > 45, !chunk.isEmpty {
                words.append("=?UTF-8?B?" + chunk.base64EncodedString() + "?=")
                chunk = Data()
            }
            chunk.append(bytes)
        }
        if !chunk.isEmpty { words.append("=?UTF-8?B?" + chunk.base64EncodedString() + "?=") }
        return words
    }

    /// RFC 2231 value characters; everything else as `%XX` of its UTF-8 bytes.
    private static func percentEncoded(_ text: String) -> String {
        var out = ""
        for byte in text.utf8 {
            if attributeCharacters.contains(byte) {
                out.append(Character(Unicode.Scalar(byte)))
            } else {
                out += String(format: "%%%02X", byte)
            }
        }
        return out
    }

    private static let attributeCharacters: Set<UInt8> = Set(
        Array(UInt8(ascii: "0")...UInt8(ascii: "9")) + Array(UInt8(ascii: "A")...UInt8(ascii: "Z"))
            + Array(UInt8(ascii: "a")...UInt8(ascii: "z")) + Array("!#$&+-.^_`|~".utf8))

    private static func angle(_ id: String) -> String {
        "<" + id.trimmingCharacters(in: CharacterSet(charactersIn: "<> \t\r\n")) + ">"
    }

    private static func date(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss Z"
        return formatter.string(from: date)
    }

    // MARK: Bodies

    private static func base64(_ data: Data) -> Data {
        Data(data.base64EncodedString(options: [.lineLength76Characters, .endLineWithCarriageReturn, .endLineWithLineFeed]).utf8)
    }

    /// Quoted-printable UTF-8 with CRLF line ends and soft breaks before column 76.
    private static func quotedPrintable(_ text: String) -> String {
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        var out = ""
        for (lineIndex, line) in normalized.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
            if lineIndex > 0 { out += "\r\n" }
            let bytes = Array(line.utf8)
            var length = 0
            for (index, byte) in bytes.enumerated() {
                let literal: Bool
                switch byte {
                case 0x20, 0x09: literal = index < bytes.count - 1
                case 33...60, 62...126: literal = true
                default: literal = false
                }
                let token = literal ? String(Character(Unicode.Scalar(byte))) : String(format: "=%02X", byte)
                if length + token.count > 75 {
                    out += "=\r\n"
                    length = 0
                }
                out += token
                length += token.count
            }
        }
        return out
    }
}
