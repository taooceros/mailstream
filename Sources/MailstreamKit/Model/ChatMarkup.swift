import Foundation

/// Google Chat's message markup (`*bold*`, `_italic_`, `~strike~`, `` `code` ``, fenced blocks,
/// `<url|label>`, `<users/…>` mentions), rendered minimally. Everything else is escaped, so a
/// message can never inject markup into the reader.
nonisolated enum ChatMarkup {
    private static let fence = try! NSRegularExpression(pattern: "```\\n?([\\s\\S]*?)\\n?```")
    private static let code = try! NSRegularExpression(pattern: "`([^`\\n]+)`")
    private static let token = try! NSRegularExpression(pattern: #"<(https?://[^|>\s]+)(?:\|([^>\n]*))?>|<(users/[^>\s]+)>"#)
    private static let bold = try! NSRegularExpression(pattern: #"(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])"#)
    private static let italic = try! NSRegularExpression(pattern: #"(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])"#)
    private static let strike = try! NSRegularExpression(pattern: #"(?<![\w~])~(?=\S)([^~\n]*?\S)~(?![\w~])"#)

    /// `names` maps `users/<id>` to a display name, for mentions.
    static func html(_ text: String, names: [String: String]) -> String {
        split(text, fence, match: { match, ns in
            "<pre>" + HTMLText.escape(ns.substring(with: match.range(at: 1))) + "</pre>"
        }, rest: { part in
            split(part, code, match: { match, ns in
                "<code>" + HTMLText.escape(ns.substring(with: match.range(at: 1))) + "</code>"
            }, rest: { part in
                split(part, token, match: { match, ns in tokenHTML(match, ns, names: names) }, rest: autolink)
            })
        })
    }

    /// One line of text for list previews: links as their labels, emphasis markers dropped.
    static func plain(_ text: String) -> String {
        var out = split(text, token, match: { match, ns in
            if match.range(at: 1).location != NSNotFound {
                let label = match.range(at: 2).location != NSNotFound ? ns.substring(with: match.range(at: 2)) : ""
                return label.isEmpty ? ns.substring(with: match.range(at: 1)) : label
            }
            return "@"
        }, rest: { $0 })
        out = out.replacingOccurrences(of: "```", with: " ")
        for regex in [bold, italic, strike] {
            out = regex.stringByReplacingMatches(in: out, range: NSRange(location: 0, length: (out as NSString).length),
                                                 withTemplate: "$1")
        }
        return out.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// Hands the matches of `regex` to `match` and the text between them to `rest`.
    private static func split(_ text: String, _ regex: NSRegularExpression, match: (NSTextCheckingResult, NSString) -> String,
                              rest: (String) -> String) -> String {
        let ns = text as NSString
        var out = ""
        var cursor = 0
        for found in regex.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            out += rest(ns.substring(with: NSRange(location: cursor, length: found.range.location - cursor)))
            out += match(found, ns)
            cursor = NSMaxRange(found.range)
        }
        out += rest(ns.substring(from: cursor))
        return out
    }

    private static func tokenHTML(_ match: NSTextCheckingResult, _ ns: NSString, names: [String: String]) -> String {
        if match.range(at: 1).location != NSNotFound {
            let url = ns.substring(with: match.range(at: 1))
            let label = match.range(at: 2).location != NSNotFound ? ns.substring(with: match.range(at: 2)) : ""
            return "<a href=\"\(HTMLText.escape(url))\">\(HTMLText.escape(label.isEmpty ? url : label))</a>"
        }
        let user = ns.substring(with: match.range(at: 3))
        return "<span class=\"mention\">@\(HTMLText.escape(names[user] ?? "user"))</span>"
    }

    /// Escapes, links bare URLs and applies emphasis outside them.
    private static func autolink(_ text: String) -> String {
        HTMLText.linkify(text, schemes: ["http", "https", "mailto"]) { emphasis(HTMLText.escape($0)) }
    }

    /// On already escaped text, whose entities hold no `*`, `_` or `~`.
    private static func emphasis(_ escaped: String) -> String {
        guard escaped.contains(where: { $0 == "*" || $0 == "_" || $0 == "~" }) else { return escaped }
        var out = escaped
        for (regex, tag) in [(bold, "b"), (italic, "i"), (strike, "s")] {
            out = regex.stringByReplacingMatches(in: out, range: NSRange(location: 0, length: (out as NSString).length),
                                                 withTemplate: "<\(tag)>$1</\(tag)>")
        }
        return out
    }
}
