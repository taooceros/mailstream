import Foundation

/// Converts the mail-parser JSON printed by `himalaya --json message read` into a
/// [`MessageContent`], writing binary parts into the message's cache directory.
///
/// Shape (serde of `mail_parser::Message`): `parts[]` each with `headers[] {name, value}` and a
/// `body` tagged `Text` / `Html` / `Binary` / `InlineBinary` / `Message` / `Multipart`;
/// `html_body`, `text_body` and `attachments` are indices into `parts`.
enum MessageParser {
    static func parse(_ data: Data, directory: URL, urlBase: String) throws -> MessageContent {
        guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw HimalayaError(message: "Message JSON is not an object")
        }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var context = Context(directory: directory, urlBase: urlBase)
        let rendered = context.render(message: root, prefix: "")
        let headers = Part(root: root, index: 0)?.headers ?? []

        let text = rendered.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let snippetSource = text.isEmpty ? HTMLText.strip(rendered.html ?? "") : text
        return MessageContent(
            html: rendered.html,
            text: text.isEmpty ? nil : text,
            cc: Header.addresses(headers, "cc"),
            replyTo: Header.addresses(headers, "reply_to"),
            references: Header.texts(headers, "references").flatMap { $0.split(whereSeparator: \.isWhitespace) }
                .map { normalizeMessageID(String($0)) },
            attachments: context.attachments,
            snippet: HTMLText.snippet(from: snippetSource),
            listUnsubscribe: Header.addresses(headers, "list_unsubscribe").first?.email)
    }

    private struct Rendered {
        var html: String?
        var text: String
    }

    private struct Context {
        let directory: URL
        let urlBase: String
        var attachments: [Attachment] = []

        mutating func render(message: [String: Any], prefix: String) -> Rendered {
            let parts = (message["parts"] as? [[String: Any]] ?? []).enumerated().map { Part(raw: $0.element, index: $0.offset) }
            let htmlIndices = message["html_body"] as? [Int] ?? []
            let textIndices = message["text_body"] as? [Int] ?? []
            let attachmentIndices = message["attachments"] as? [Int] ?? []

            // Store every binary part first so cid references resolve.
            var stored: [Int: Attachment] = [:]
            for part in parts {
                guard let bytes = part.binary else { continue }
                let attachment = store(part: part, bytes: bytes, prefix: prefix)
                stored[part.index] = attachment
            }

            let hasHTML = htmlIndices.contains { parts[safe: $0]?.bodyKind == "Html" }
            var html = ""
            var inlineShown = Set<Int>()
            if hasHTML {
                for index in htmlIndices {
                    guard let part = parts[safe: index] else { continue }
                    switch part.bodyKind {
                    case "Html": html += part.string ?? ""
                    case "Text": html += HTMLText.plainToHTML(part.string ?? "")
                    default:
                        if let attachment = stored[index], attachment.mime.hasPrefix("image/") {
                            html += "<p><img src=\"\(url(for: attachment))\" style=\"max-width:100%\"></p>"
                            inlineShown.insert(index)
                        }
                    }
                }
                for attachment in stored.values {
                    guard let cid = attachment.contentID else { continue }
                    let target = url(for: attachment)
                    let before = html
                    html = html.replacingOccurrences(of: "cid:\(cid)", with: target, options: .caseInsensitive)
                    if html != before { inlineShown.insert(attachment.part) }
                }
            }

            var text = textIndices.compactMap { index -> String? in
                guard let part = parts[safe: index] else { return nil }
                switch part.bodyKind {
                case "Text": return part.string
                case "Html": return HTMLText.strip(part.string ?? "")
                default: return nil
                }
            }.joined(separator: "\n\n")

            for index in attachmentIndices {
                guard let part = parts[safe: index] else { continue }
                if part.bodyKind == "Message", let nested = part.nestedMessage {
                    let nestedHeaders = Part(root: nested, index: 0)?.headers ?? []
                    let inner = render(message: nested, prefix: "\(prefix)m\(index)-")
                    let headline = Header.forwardedHeadline(nestedHeaders)
                    if hasHTML {
                        html += "<div class=\"ms-attached-message\"><div class=\"ms-attached-headline\">\(HTMLText.escape(headline))</div>"
                            + (inner.html ?? HTMLText.plainToHTML(inner.text)) + "</div>"
                    }
                    text += "\n\n---------- \(headline) ----------\n" + inner.text
                    continue
                }
                if inlineShown.contains(index) { continue }
                if var attachment = stored[index] {
                    attachment = Attachment(part: attachment.part, filename: attachment.filename, mime: attachment.mime,
                                            size: attachment.size, contentID: attachment.contentID, inline: false,
                                            storedName: attachment.storedName)
                    attachments.append(attachment)
                } else if let string = part.string, part.bodyKind == "Html" || part.bodyKind == "Text" {
                    let bytes = Data(string.utf8)
                    attachments.append(store(part: part, bytes: bytes, prefix: prefix))
                }
            }
            return Rendered(html: hasHTML ? html : nil, text: text)
        }

        private func url(for attachment: Attachment) -> String {
            urlBase + (attachment.storedName.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? attachment.storedName)
        }

        private func store(part: Part, bytes: Data, prefix: String) -> Attachment {
            let contentType = part.contentType
            let mime = contentType.map { "\($0.type)/\($0.subtype ?? "octet-stream")" } ?? "application/octet-stream"
            var filename = part.disposition?.attributes["filename"] ?? contentType?.attributes["name"] ?? ""
            if filename.isEmpty {
                filename = "attachment-\(part.index)" + (HTMLText.fileExtension(forMIME: mime).map { ".\($0)" } ?? "")
            }
            let safe = filename.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_")
            let storedName = "\(prefix)\(part.index)-\(safe)"
            try? bytes.write(to: directory.appendingPathComponent(storedName))
            let cid = Header.texts(part.headers, "content_id").first.map {
                $0.trimmingCharacters(in: CharacterSet(charactersIn: "<> "))
            }
            return Attachment(part: part.index, filename: filename, mime: mime.lowercased(), size: bytes.count,
                              contentID: cid, inline: part.disposition?.type == "inline", storedName: storedName)
        }
    }

    struct ContentType {
        let type: String
        let subtype: String?
        let attributes: [String: String]
    }

    struct Part {
        let raw: [String: Any]
        let index: Int

        init(raw: [String: Any], index: Int) {
            self.raw = raw
            self.index = index
        }

        init?(root: [String: Any], index: Int) {
            guard let parts = root["parts"] as? [[String: Any]], index < parts.count else { return nil }
            self.init(raw: parts[index], index: index)
        }

        var headers: [(String, Any)] {
            (raw["headers"] as? [[String: Any]] ?? []).compactMap { header in
                let value = header["value"] ?? NSNull()
                if let name = header["name"] as? String { return (name.lowercased(), value) }
                if let other = (header["name"] as? [String: Any])?["other"] as? String {
                    return (other.lowercased().replacingOccurrences(of: "-", with: "_"), value)
                }
                return nil
            }
        }

        private var body: (String, Any)? {
            if let body = raw["body"] as? [String: Any], let (key, value) = body.first { return (key, value) }
            if let body = raw["body"] as? String { return (body, NSNull()) }
            return nil
        }

        var bodyKind: String? { body?.0 }
        var string: String? { body?.1 as? String }

        var binary: Data? {
            guard let (kind, value) = body, kind == "Binary" || kind == "InlineBinary" else { return nil }
            if let numbers = value as? [NSNumber] {
                return Data(numbers.map(\.uint8Value))
            }
            if let string = value as? String { return Data(base64Encoded: string) ?? Data(string.utf8) }
            return nil
        }

        var nestedMessage: [String: Any]? {
            guard let (kind, value) = body, kind == "Message" else { return nil }
            return value as? [String: Any]
        }

        var contentType: ContentType? { Header.contentType(headers, "content_type") }
        var disposition: ContentType? { Header.contentType(headers, "content_disposition") }
    }

    enum Header {
        static func values(_ headers: [(String, Any)], _ name: String) -> [Any] {
            headers.filter { $0.0 == name }.map(\.1)
        }

        static func texts(_ headers: [(String, Any)], _ name: String) -> [String] {
            values(headers, name).flatMap { value -> [String] in
                guard let dict = value as? [String: Any] else { return [] }
                if let text = dict["Text"] as? String { return [text] }
                if let list = dict["TextList"] as? [String] { return list }
                return []
            }
        }

        static func addresses(_ headers: [(String, Any)], _ name: String) -> [Address] {
            values(headers, name).flatMap { value -> [Address] in
                guard let address = (value as? [String: Any])?["Address"] as? [String: Any] else { return [] }
                var entries: [[String: Any]] = address["List"] as? [[String: Any]] ?? []
                for group in address["Group"] as? [[String: Any]] ?? [] {
                    entries += group["addresses"] as? [[String: Any]] ?? []
                }
                return entries.compactMap { entry in
                    guard let email = entry["address"] as? String else { return nil }
                    return Address(name: entry["name"] as? String, email: email)
                }
            }
        }

        static func contentType(_ headers: [(String, Any)], _ name: String) -> ContentType? {
            guard let value = values(headers, name).first as? [String: Any],
                  let ct = value["ContentType"] as? [String: Any],
                  let type = ct["c_type"] as? String else { return nil }
            var attributes: [String: String] = [:]
            for attribute in ct["attributes"] as? [[String: Any]] ?? [] {
                if let key = attribute["name"] as? String, let value = attribute["value"] as? String {
                    attributes[key.lowercased()] = value
                }
            }
            return ContentType(type: type.lowercased(), subtype: (ct["c_subtype"] as? String)?.lowercased(),
                               attributes: attributes)
        }

        static func forwardedHeadline(_ headers: [(String, Any)]) -> String {
            let from = addresses(headers, "from").first?.rfc5322 ?? "unknown sender"
            let subject = texts(headers, "subject").first ?? ""
            return subject.isEmpty ? "Attached message from \(from)" : "Attached message from \(from): \(subject)"
        }
    }
}

extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
