import Foundation

/// A subject line taken apart: leading reply/forward prefixes in the languages mail clients write
/// them (`Re:`, `AW:`, `SV:`, `回复：`, numbered `Re[2]:` / `Re(2):`) and `[list]` tags.
nonisolated struct Subject: Sendable {
    enum Prefix: Sendable { case reply, forward }

    /// The first reply/forward prefix, nil when there is none.
    let prefix: Prefix?
    /// `[list]` tags, e.g. "[swift-evolution]"; empty when none.
    let list: String
    /// Without prefixes and list tags; empty when the subject is nothing but prefixes.
    let title: String
    private let raw: String

    init(_ subject: String) {
        raw = subject.trimmingCharacters(in: .whitespacesAndNewlines)
        var text = Substring(raw)
        var prefix = Self.dropPrefixes(&text)
        var list = ""
        // Lists tag the subject in front of the prefixes or after them; tags with nothing after
        // them are the title.
        if let tags = Self.leadingListTags(text) {
            var rest = tags.rest
            let inner = Self.dropPrefixes(&rest)
            if !rest.allSatisfy(\.isWhitespace) {
                list = String(tags.tags)
                prefix = prefix ?? inner
                text = rest
            }
        }
        self.prefix = prefix
        self.list = list
        title = text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// What a reader sees as the subject: the title, or the whole subject when prefixes are all there is.
    var displayTitle: String { title.isEmpty ? raw : title }

    /// The subject of a reply to `subject`: unchanged when it already starts as a reply.
    static func reply(to subject: String) -> String { prefixed(subject, "Re:", unless: .reply) }

    /// The subject of a forward of `subject`: unchanged when it already starts as a forward.
    static func forward(_ subject: String) -> String { prefixed(subject, "Fwd:", unless: .forward) }

    private static func prefixed(_ subject: String, _ marker: String, unless kind: Prefix) -> String {
        let parsed = Subject(subject)
        return parsed.prefix == kind ? parsed.raw : marker + " " + parsed.raw
    }

    private static let words: [(word: String, kind: Prefix)] = [
        ("re", .reply), ("aw", .reply), ("sv", .reply), ("vs", .reply), ("rif", .reply),
        ("回复", .reply), ("答复", .reply), ("回覆", .reply),
        ("fwd", .forward), ("fw", .forward), ("wg", .forward), ("tr", .forward), ("转发", .forward), ("轉寄", .forward),
    ]

    /// Drops the prefixes `text` starts with, returning the kind of the first.
    private static func dropPrefixes(_ text: inout Substring) -> Prefix? {
        var first: Prefix?
        while let found = leadingPrefix(text) {
            first = first ?? found.kind
            text = found.rest
        }
        return first
    }

    /// The `[list]` tags `text` starts with (each 1–40 characters on one line) and what follows them.
    private static func leadingListTags(_ text: Substring) -> (tags: Substring, rest: Substring)? {
        var rest = text.drop(while: \.isWhitespace)
        let start = rest.startIndex
        var end = start
        while rest.first == "[" {
            let inner = rest.dropFirst().prefix { $0 != "]" && $0 != "\n" }
            guard (1...40).contains(inner.count), inner.endIndex < rest.endIndex, rest[inner.endIndex] == "]" else { break }
            end = rest.index(after: inner.endIndex)
            rest = rest[end...].drop(while: \.isWhitespace)
        }
        return end == start ? nil : (text[start..<end], rest)
    }

    /// The prefix `text` starts with (a word, an optional `[2]` or `(2)` count, a colon) and what
    /// follows it. Scanned by hand: the threader runs this for every message on every rebuild.
    private static func leadingPrefix(_ text: Substring) -> (kind: Prefix, rest: Substring)? {
        let start = text.drop(while: \.isWhitespace)
        for (word, kind) in words where start.prefix(word.count).lowercased() == word {
            var rest = start.dropFirst(word.count).drop(while: \.isWhitespace)
            if let open = rest.first, open == "[" || open == "(" {
                let digits = rest.dropFirst().prefix { $0.isASCII && $0.isWholeNumber }
                guard !digits.isEmpty, rest[digits.endIndex...].first == (open == "[" ? "]" : ")") else { continue }
                rest = rest[rest.index(after: digits.endIndex)...].drop(while: \.isWhitespace)
            }
            if let colon = rest.first, colon == ":" || colon == "：" { return (kind, rest.dropFirst()) }
        }
        return nil
    }
}
