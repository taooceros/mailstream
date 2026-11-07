import Foundation

/// RFC 5322 address-list headers (`To`, `Cc`, `From`, `Reply-To`) as provider APIs hand them
/// over: display names quoted or bare, commas inside quotes, comments, groups and bare addresses.
nonisolated enum AddressList {
    static func parse(_ header: String) -> [Address] {
        var addresses: [Address] = []
        var phrase = ""
        var comment = ""
        var angle: String?
        var inAngle = false

        func finish() {
            defer { phrase = ""; comment = ""; angle = nil; inAngle = false }
            let name = collapse(HeaderText.decode(phrase))
            let commentName = collapse(HeaderText.decode(comment))
            if let angle {
                let email = angle.hasPrefix("mailto:") ? String(angle.dropFirst("mailto:".count)) : angle
                guard email.contains("@") else { return }
                let display = name.isEmpty ? commentName : name
                addresses.append(Address(name: display.nilIfEmpty, email: email))
            } else {
                let email = phrase.filter { !$0.isWhitespace }
                guard email.contains("@") else { return }
                addresses.append(Address(name: commentName.nilIfEmpty, email: email))
            }
        }

        let characters = Array(header)
        var index = 0
        while index < characters.count {
            let character = characters[index]
            if inAngle {
                if character == ">" { inAngle = false } else if !character.isWhitespace { angle?.append(character) }
                index += 1
                continue
            }
            switch character {
            case "\"":
                index += 1
                while index < characters.count, characters[index] != "\"" {
                    if characters[index] == "\\", index + 1 < characters.count { index += 1 }
                    phrase.append(characters[index])
                    index += 1
                }
            case "(":
                var depth = 1
                index += 1
                while index < characters.count {
                    let inner = characters[index]
                    if inner == "\\", index + 1 < characters.count {
                        index += 1
                        comment.append(characters[index])
                    } else if inner == "(" {
                        depth += 1
                        comment.append(inner)
                    } else if inner == ")" {
                        depth -= 1
                        if depth == 0 { break }
                        comment.append(inner)
                    } else {
                        comment.append(inner)
                    }
                    index += 1
                }
            case "<":
                inAngle = true
                angle = ""
            case ",", ";":
                finish()
            case ":":
                // A group's name; its members follow.
                phrase = ""
                comment = ""
            default:
                phrase.append(character)
            }
            index += 1
        }
        finish()
        return addresses
    }

    private static func collapse(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
}

/// Header and body text in the charsets mail declares.
nonisolated enum HeaderText {
    /// Decodes RFC 2047 encoded-words (`=?UTF-8?B?…?=`); text without them is returned as is.
    static func decode(_ text: String) -> String {
        guard text.contains("=?") else { return text }
        var out = ""
        var pending = Data()
        var pendingCharset: String?
        var cursor = text.startIndex

        func flush() {
            guard let charset = pendingCharset else { return }
            out += string(pending, charset: charset)
            pending = Data()
            pendingCharset = nil
        }

        for match in text.matches(of: /=\?([^?\s]+)\?([BbQq])\?([^?\s]*)\?=/) {
            let between = text[cursor..<match.range.lowerBound]
            // Whitespace between two encoded-words is not part of the text.
            if pendingCharset == nil || !between.allSatisfy(\.isWhitespace) {
                flush()
                out += between
            }
            let charset = String(match.1.split(separator: "*").first ?? match.1).lowercased()
            if charset != pendingCharset { flush() }
            pendingCharset = charset
            pending.append(match.2.lowercased() == "b" ? Data(base64URLEncoded: match.3) ?? Data() : quoted(match.3))
            cursor = match.range.upperBound
        }
        flush()
        out += text[cursor...]
        return out
    }

    /// `data` in `charset` (IANA name), else UTF-8, else ISO-8859-1.
    static func string(_ data: Data, charset: String?) -> String {
        if let charset {
            let name = charset.lowercased()
            if let text = Platform.decode(data, charset: aliases[name] ?? name) { return text }
        }
        if let text = String(data: data, encoding: .utf8) { return text }
        return String(data: data, encoding: .isoLatin1) ?? ""
    }

    /// The value of parameter `name` (`charset`, `boundary`) in a `Content-Type`-style header.
    static func parameter(_ name: String, in header: String?) -> String? {
        guard let header else { return nil }
        for piece in header.split(separator: ";").dropFirst() {
            let pair = piece.split(separator: "=", maxSplits: 1)
            guard pair.count == 2, pair[0].trimmingCharacters(in: .whitespaces).lowercased() == name else { continue }
            let value = pair[1].trimmingCharacters(in: CharacterSet(charactersIn: "\" \t\r\n"))
            return value.nilIfEmpty
        }
        return nil
    }

    /// Supersets of charsets senders commonly mislabel.
    private static let aliases = ["gb2312": "gb18030", "gbk": "gb18030", "x-gbk": "gb18030",
                                  "iso-8859-1": "windows-1252", "latin1": "windows-1252"]

    private static func quoted(_ text: Substring) -> Data {
        var bytes = Data()
        let utf8 = Array(text.utf8)
        var index = 0
        while index < utf8.count {
            let byte = utf8[index]
            if byte == UInt8(ascii: "_") {
                bytes.append(0x20)
            } else if byte == UInt8(ascii: "="), index + 2 < utf8.count,
                      let value = UInt8(String(decoding: utf8[index + 1...index + 2], as: UTF8.self), radix: 16) {
                bytes.append(value)
                index += 2
            } else {
                bytes.append(byte)
            }
            index += 1
        }
        return bytes
    }
}
