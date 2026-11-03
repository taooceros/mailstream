import Foundation

/// Builds a `MessageContent` from a provider's message parts. Binary parts are written into
/// the message's cache directory (served to the reader under `urlBase` by `PartSchemeHandler`),
/// `cid:` references in the HTML are pointed at them, and parts the HTML shows stay out of the
/// attachment list.
nonisolated struct MessageAssembly {
    let directory: URL
    let urlBase: String
    private var stored: [Attachment] = []

    init(directory: URL, urlBase: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        self.directory = directory
        self.urlBase = urlBase
    }

    /// Writes one part. `index` is unique within the message and becomes `Attachment.part`.
    @discardableResult
    mutating func store(_ bytes: Data, index: Int, filename: String?, mime: String, contentID: String?,
                        inline: Bool) -> Attachment {
        let mime = mime.isEmpty ? "application/octet-stream" : mime.lowercased()
        var name = filename ?? ""
        if name.isEmpty {
            name = "attachment-\(index)" + (HTMLText.fileExtension(forMIME: mime).map { ".\($0)" } ?? "")
        }
        let safe = name.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_")
        let storedName = "\(index)-\(safe)"
        try? bytes.write(to: directory.appendingPathComponent(storedName))
        let cid = contentID.map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "<> ")) }.flatMap { $0.isEmpty ? nil : $0 }
        let attachment = Attachment(part: index, filename: name, mime: mime, size: bytes.count, contentID: cid,
                                    inline: inline, storedName: storedName)
        stored.append(attachment)
        return attachment
    }

    func url(for attachment: Attachment) -> String {
        urlBase + (attachment.storedName.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? attachment.storedName)
    }

    /// `html` is nil for text-only mail; `text` is the plain body, nil when the message has none
    /// (then the snippet and quoting come from the HTML).
    func finish(html: String?, text: String?, cc: [Address], replyTo: [Address], references: [String]) -> MessageContent {
        var shown = Set<Int>()
        var html = html
        if var rewritten = html {
            for attachment in stored {
                guard let cid = attachment.contentID else { continue }
                let before = rewritten
                rewritten = rewritten.replacingOccurrences(of: "cid:\(cid)", with: url(for: attachment), options: .caseInsensitive)
                if rewritten != before { shown.insert(attachment.part) }
            }
            html = rewritten
        }
        let attachments = stored.filter { !shown.contains($0.part) }.map {
            Attachment(part: $0.part, filename: $0.filename, mime: $0.mime, size: $0.size, contentID: $0.contentID,
                       inline: false, storedName: $0.storedName)
        }
        let plain = text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let snippetSource = plain.isEmpty ? HTMLText.strip(html ?? "") : plain
        return MessageContent(html: html, text: plain.isEmpty ? (html.map(HTMLText.strip) ?? nil) : plain,
                              cc: cc, replyTo: replyTo, references: references, attachments: attachments,
                              snippet: HTMLText.snippet(from: snippetSource))
    }
}
