import Foundation

nonisolated enum HTMLText {
    static func escape(_ text: String) -> String {
        var out = ""
        out.reserveCapacity(text.utf8.count)
        for char in text {
            switch char {
            case "&": out += "&amp;"
            case "<": out += "&lt;"
            case ">": out += "&gt;"
            case "\"": out += "&quot;"
            case "'": out += "&#39;"
            default: out.append(char)
            }
        }
        return out
    }

    private static let urlDetector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)

    /// Plain text as HTML with its URLs linked: the text between links goes through `plain`
    /// (escaping by default), and only URLs with one of `schemes` count when given.
    static func linkify(_ text: String, schemes: Set<String>? = nil, plain: (String) -> String = escape) -> String {
        guard let detector = urlDetector, text.contains(where: { $0 == "." || $0 == ":" }) else { return plain(text) }
        let ns = text as NSString
        var out = ""
        var cursor = 0
        for match in detector.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            guard let url = match.url, schemes?.contains(url.scheme?.lowercased() ?? "") ?? true else { continue }
            out += plain(ns.substring(with: NSRange(location: cursor, length: match.range.location - cursor)))
            out += "<a href=\"\(escape(url.absoluteString))\">\(escape(ns.substring(with: match.range)))</a>"
            cursor = NSMaxRange(match.range)
        }
        return out + plain(ns.substring(from: cursor))
    }

    /// Whether `html` references remote images, styles or fonts.
    static func hasRemoteContent(_ html: String) -> Bool {
        html.range(of: #"(src|background|poster)\s*=\s*["']?\s*(https?:)?//|url\(\s*["']?\s*(https?:)?//|<link[^>]+href\s*=\s*["']?\s*https?:"#,
                   options: [.regularExpression, .caseInsensitive]) != nil
    }

    /// Plain text to HTML: linkified, whitespace preserved, `>` quote runs wrapped in blockquotes
    /// and signatures (after a `-- ` line, or a lone mobile footer) in `.ms-signature`, for the
    /// reader to fold.
    static func plainToHTML(_ text: String) -> String {
        var html = "<div class=\"ms-plain\">"
        var quoteDepth = 0
        var inSignature = false
        for rawLine in text.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false) {
            var line = Substring(rawLine)
            var depth = 0
            while line.first == ">" {
                depth += 1
                line = line.dropFirst()
                if line.first == " " { line = line.dropFirst() }
            }
            if inSignature, depth > 0 { html += "</div>"; inSignature = false }
            while quoteDepth < depth { html += "<blockquote class=\"ms-quote\">"; quoteDepth += 1 }
            while quoteDepth > depth { html += "</blockquote>"; quoteDepth -= 1 }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if depth == 0, !inSignature, trimmed == "--" {
                html += "<div class=\"ms-signature\">"
                inSignature = true
            }
            if depth == 0, !inSignature, isMobileFooter(trimmed) {
                html += "<span class=\"ms-signature\">" + escape(String(line)) + "</span>\n"
            } else {
                html += linkify(String(line)) + "\n"
            }
        }
        if inSignature { html += "</div>" }
        while quoteDepth > 0 { html += "</blockquote>"; quoteDepth -= 1 }
        return html + "</div>"
    }

    /// "Sent from my iPhone" and the like, which mail apps append on their own.
    private static func isMobileFooter(_ line: String) -> Bool {
        line.count < 40 && line.range(of: #"^(Sent from my |Get Outlook for |发自我的)"#, options: .regularExpression) != nil
    }

    /// Visible text of an HTML document, for snippets.
    static func strip(_ html: String) -> String {
        var text = html
        for block in ["style", "script", "head", "title"] {
            text = text.replacingOccurrences(of: "<\(block)[^>]*>[\\s\\S]*?</\(block)>", with: " ",
                                             options: [.regularExpression, .caseInsensitive])
        }
        text = text.replacingOccurrences(of: "<!--[\\s\\S]*?-->", with: " ", options: .regularExpression)
        text = text.replacingOccurrences(of: "<(br|/p|/div|/tr|/li|/h[1-6])[^>]*>", with: "\n",
                                         options: [.regularExpression, .caseInsensitive])
        text = text.replacingOccurrences(of: "<[^>]+>", with: " ", options: .regularExpression)
        return decodeEntities(text)
    }

    private static let namedEntities: [String: String] = [
        "amp": "&", "lt": "<", "gt": ">", "quot": "\"", "apos": "'", "nbsp": " ", "zwnj": "", "zwj": "",
        "shy": "", "mdash": "—", "ndash": "–", "hellip": "…", "rsquo": "’", "lsquo": "‘", "rdquo": "”",
        "ldquo": "“", "copy": "©", "reg": "®", "trade": "™", "middot": "·", "bull": "•", "ensp": " ",
        "emsp": " ", "thinsp": " ",
    ]

    static func decodeEntities(_ text: String) -> String {
        guard text.contains("&") else { return text }
        var out = ""
        var index = text.startIndex
        while let amp = text[index...].firstIndex(of: "&") {
            out += text[index..<amp]
            guard let semi = text[amp...].prefix(12).firstIndex(of: ";") else {
                out.append("&")
                index = text.index(after: amp)
                continue
            }
            let entity = text[text.index(after: amp)..<semi]
            if entity.hasPrefix("#") {
                let digits = entity.dropFirst()
                let value = digits.first == "x" || digits.first == "X"
                    ? UInt32(digits.dropFirst(), radix: 16) : UInt32(digits)
                if let value, let scalar = Unicode.Scalar(value) {
                    out.unicodeScalars.append(scalar)
                    index = text.index(after: semi)
                    continue
                }
            } else if let replacement = namedEntities[entity.lowercased()] {
                out += replacement
                index = text.index(after: semi)
                continue
            }
            out.append("&")
            index = text.index(after: amp)
        }
        out += text[index...]
        return out
    }

    /// One-paragraph preview: quotes, attributions, link targets and preheader padding removed.
    static func snippet(from text: String, limit: Int = 240) -> String {
        var source = decodeEntities(String(text.prefix(8000)))
        for (pattern, template) in [
            (#"<!--[\s\S]*?-->|<!\[endif\]-->|<!--\[if[^\]]*\]>"#, " "),
            (#"\[([^\]\n]*)\]\(\s*https?://[^)\s]*\s*\)"#, "$1"),
            (#"[<\[(]\s*(https?://|mailto:)[^>\])\s]*\s*[>\])]"#, " "),
            (#"(https?://|www\.)\S+"#, " "),
            (#"\[image:[^\]]*\]"#, " "),
        ] {
            source = source.replacingOccurrences(of: pattern, with: template, options: .regularExpression)
        }
        var lines: [Substring] = []
        for line in source.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix(">") { continue }
            if trimmed.hasPrefix("On ") && trimmed.hasSuffix("wrote:") { break }
            if trimmed.hasPrefix("-----Original Message") || trimmed.hasPrefix("---------- Forwarded") { break }
            if trimmed.hasPrefix("在") && (trimmed.hasSuffix("写道：") || trimmed.hasSuffix("写道:")) { break }
            lines.append(line)
            if lines.count > 40 { break }
        }
        let invisible: Set<Unicode.Scalar> = ["\u{034F}", "\u{200B}", "\u{200C}", "\u{200D}", "\u{2060}",
                                              "\u{FEFF}", "\u{00AD}", "\u{180E}"]
        var scalars = String.UnicodeScalarView()
        var lastWasSpace = true
        for scalar in lines.joined(separator: " ").unicodeScalars {
            if invisible.contains(scalar) { continue }
            if scalar.properties.isWhitespace {
                if !lastWasSpace { scalars.append(" ") }
                lastWasSpace = true
            } else {
                scalars.append(scalar)
                lastWasSpace = false
            }
            if scalars.count >= limit { break }
        }
        return trimHistory(String(scalars).trimmingCharacters(in: .whitespaces))
    }

    private static let historyPatterns: [NSRegularExpression] = [
        #"\s+On\s[^\n]{4,200}?\s(?:wrote|написал)\s*:.*$"#,
        #"\s+On\s(?:(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)[a-z]*,?\s+)?(?:\d{1,2}\s+)?(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.?\s+\d{1,2}?,?\s*\d{4}.*$"#,
        #"\s+在\s?\d{4}年.*$"#,
        #"\s+-{2,}\s*(?:Original Message|Forwarded message).*$"#,
        #"\s+From:\s.{1,120}?\s(?:Sent|Date):\s.*$"#,
        #"\s+(?:Sent with Spark|Sent from my (?:iPhone|iPad|Android)|Get Outlook for \w+)\b.*$"#,
    ].map { try! NSRegularExpression(pattern: $0, options: [.dotMatchesLineSeparators]) }

    /// Drops a reply's quoted history and client signatures from a one-line preview, even when the
    /// attribution line was wrapped or cut off.
    private static func trimHistory(_ snippet: String) -> String {
        var text = snippet
        for pattern in historyPatterns {
            let range = NSRange(text.startIndex..., in: text)
            guard let match = pattern.firstMatch(in: text, range: range), match.range.location > 0,
                  let found = Range(match.range, in: text) else { continue }
            text.removeSubrange(found)
        }
        return text
    }

    static func fileExtension(forMIME mime: String) -> String? {
        switch mime.lowercased() {
        case "image/jpeg": "jpg"
        case "image/png": "png"
        case "image/gif": "gif"
        case "image/webp": "webp"
        case "application/pdf": "pdf"
        case "text/calendar": "ics"
        case "text/html": "html"
        case "text/plain": "txt"
        case "message/rfc822": "eml"
        default: nil
        }
    }
}
