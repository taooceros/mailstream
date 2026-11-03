import Foundation

/// Groups messages into conversations from the provider's thread (Gmail thread, Graph
/// conversation) and `Message-ID` / `In-Reply-To` links, falling back to the normalized subject
/// for replies whose parent was not loaded.
///
/// `primary` messages decide which conversations exist (the selected mailbox); `context` messages
/// (the account's inbox and sent mail) only join conversations a primary message already started,
/// so the reader shows both sides of an exchange.
nonisolated enum Threader {
    static let subjectWindow: TimeInterval = 45 * 24 * 3600

    static func conversations(primary: [MailMessage], context: [MailMessage]) -> [Conversation] {
        var seenRefs = Set<MessageRef>()
        var messages: [MailMessage] = []
        var isPrimary: [Bool] = []
        for (list, flag) in [(primary, true), (context, false)] {
            for message in list where seenRefs.insert(message.ref).inserted {
                messages.append(message)
                isPrimary.append(flag)
            }
        }
        guard !messages.isEmpty else { return [] }

        var parent = Array(messages.indices)
        func find(_ x: Int) -> Int {
            var x = x
            while parent[x] != x {
                parent[x] = parent[parent[x]]
                x = parent[x]
            }
            return x
        }
        func union(_ a: Int, _ b: Int) {
            let ra = find(a), rb = find(b)
            if ra != rb { parent[max(ra, rb)] = min(ra, rb) }
        }

        // Message-ID links, scoped per account.
        var byMessageID: [String: Int] = [:]
        for (index, message) in messages.enumerated() {
            guard let id = message.envelope.messageID else { continue }
            let key = message.ref.account + "\u{1}" + id
            if let existing = byMessageID[key] { union(existing, index) } else { byMessageID[key] = index }
        }
        for (index, message) in messages.enumerated() {
            for parentID in message.envelope.inReplyTo {
                if let target = byMessageID[message.ref.account + "\u{1}" + parentID] { union(index, target) }
            }
        }

        // The provider's own threads, scoped per account.
        var byThread: [String: Int] = [:]
        for (index, message) in messages.enumerated() {
            guard let thread = message.envelope.thread else { continue }
            let key = message.ref.account + "\u{1}" + thread
            if let existing = byThread[key] { union(existing, index) } else { byThread[key] = index }
        }

        // Subject fallback, oldest first so a reply joins the latest earlier message on that subject.
        let order = messages.indices.sorted { (messages[$0].envelope.date ?? .distantPast) < (messages[$1].envelope.date ?? .distantPast) }
        var lastBySubject: [String: Int] = [:]
        for index in order {
            let envelope = messages[index].envelope
            let (normalized, hadPrefix) = threadKey(envelope.subject)
            guard normalized.count >= 4 else { continue }
            let key = messages[index].ref.account + "\u{1}" + normalized
            if let previous = lastBySubject[key], hadPrefix || !envelope.inReplyTo.isEmpty {
                let gap = (envelope.date ?? .distantPast).timeIntervalSince(messages[previous].envelope.date ?? .distantPast)
                if gap < subjectWindow { union(index, previous) }
            }
            lastBySubject[key] = index
        }

        var groups: [Int: [Int]] = [:]
        for index in messages.indices { groups[find(index), default: []].append(index) }

        var conversations: [Conversation] = []
        for members in groups.values where members.contains(where: { isPrimary[$0] }) {
            // Collapse copies of one message living in several mailboxes (Gmail labels share the
            // provider id), preferring the primary copy.
            var chosen = Set<String>()
            var unique: [Int] = []
            for index in members.sorted(by: { isPrimary[$0] && !isPrimary[$1] }) {
                let envelope = messages[index].envelope
                guard chosen.insert("id\u{1}" + envelope.id).inserted else { continue }
                if let id = envelope.messageID, !chosen.insert(id).inserted { continue }
                unique.append(index)
            }
            let sorted = unique.map { messages[$0] }.sorted {
                ($0.envelope.date ?? .distantPast, $0.ref.id) < ($1.envelope.date ?? .distantPast, $1.ref.id)
            }
            let first = sorted[0]
            let id = first.ref.account + "|" + (first.envelope.messageID ?? first.ref.id)
            conversations.append(Conversation(id: id, account: first.ref.account, messages: sorted))
        }
        conversations.sort { ($0.latestDate ?? .distantPast) > ($1.latestDate ?? .distantPast) }
        return conversations
    }

    /// The subject as threading compares it: list tags and title lowercased, whitespace collapsed;
    /// and whether it had a reply/forward prefix.
    private static func threadKey(_ raw: String) -> (String, Bool) {
        let subject = Subject(raw)
        let text = subject.list.isEmpty ? subject.title : subject.list + " " + subject.title
        return (text.lowercased().split(whereSeparator: \.isWhitespace).joined(separator: " "), subject.prefix != nil)
    }
}
