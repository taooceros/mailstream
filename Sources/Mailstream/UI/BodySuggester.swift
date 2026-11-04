import Foundation

/// The grey text the compose body offers after the caret, taken from the message itself: no model
/// and no network. A greeting line gets the recipients' first names, a sign-off line the sender's
/// name, and a word being typed the longer word it starts among the subject, the names and the
/// text (the quote included).
nonisolated struct BodySuggester {
    /// First names of the named recipients on To.
    var recipients: [String] = []
    /// The sender's first name.
    var signature: String?
    /// Words of five letters or more on the message, with how often each appears.
    private var words: [String: Int] = [:]

    private static let greetings: Set<String> = [
        "hi", "hello", "hey", "dear", "morning", "good morning", "good afternoon", "good evening",
    ]
    private static let signOffs: Set<String> = [
        "best", "best regards", "kind regards", "regards", "warm regards", "warmly", "best wishes", "all the best",
        "thanks", "thank you", "many thanks", "thanks again", "thanks so much", "cheers", "sincerely", "yours",
    ]

    /// Replaces the words completion draws on with those in `texts`.
    mutating func learn(_ texts: [String]) {
        var counts: [String: Int] = [:]
        for text in texts {
            text.enumerateSubstrings(in: text.startIndex..., options: .byWords) { word, _, _, _ in
                guard let word, word.count >= 5, word.allSatisfy(Self.isWordCharacter) else { return }
                counts[word, default: 0] += 1
            }
        }
        words = counts
    }

    /// What to show after `caret` (a UTF-16 offset) in `text`, if anything. Suggestions only come
    /// at the end of a line, where there is room for them.
    func suggestion(in text: String, at caret: Int) -> String? {
        let ns = text as NSString
        guard caret > 0, caret <= ns.length else { return nil }
        let rest = ns.substring(from: caret)
        guard rest.isEmpty || rest.hasPrefix("\n") else { return nil }
        let newline = ns.range(of: "\n", options: .backwards, range: NSRange(location: 0, length: caret))
        let lineStart = newline.location == NSNotFound ? 0 : newline.location + 1
        let line = ns.substring(with: NSRange(location: lineStart, length: caret - lineStart))
        let key = line.trimmingCharacters(in: .whitespaces).lowercased()

        if !recipients.isEmpty, Self.greetings.contains(key) {
            let names = switch recipients.count {
            case 1: recipients[0]
            case 2: "\(recipients[0]) and \(recipients[1])"
            default: "all"
            }
            return (line.hasSuffix(" ") ? "" : " ") + names + ","
        }

        let comma = key.hasSuffix(",")
        if let signature, !line.hasSuffix(" "), Self.signOffs.contains(comma ? String(key.dropLast()) : key) {
            // The name goes on the next line, which must be free for the grey text to show there.
            let roomBelow = rest.isEmpty || rest == "\n" || rest.hasPrefix("\n\n")
            if roomBelow { return (comma ? "" : ",") + "\n" + signature }
            return comma ? nil : ","
        }

        let partial = String(line.reversed().prefix(while: Self.isWordCharacter).reversed())
        guard partial.count >= 3 || (partial.count == 2 && partial.first?.isUppercase == true) else { return nil }
        let folded = Contact.fold(partial)
        let best = words
            .filter { $0.key.count >= partial.count + 2 && Contact.fold($0.key).hasPrefix(folded) }
            .max { a, b in a.value != b.value ? a.value < b.value : a.key.count > b.key.count }
        return best.map { String($0.key.dropFirst(partial.count)) }
    }

    /// Letters of alphabetic scripts; ideographic text has no spaces to find a word's start by.
    private static func isWordCharacter(_ character: Character) -> Bool {
        character.isLetter && !character.unicodeScalars.contains { $0.properties.isIdeographic }
    }
}
