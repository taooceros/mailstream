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
        var byMessageID: [AccountScopedKey: Int] = [:]
        for (index, message) in messages.enumerated() {
            guard let id = message.envelope.messageID else { continue }
            let key = AccountScopedKey(account: message.ref.account, value: id)
            if let existing = byMessageID[key] { union(existing, index) } else { byMessageID[key] = index }
        }
        for (index, message) in messages.enumerated() {
            for parentID in message.envelope.inReplyTo {
                if let target = byMessageID[AccountScopedKey(account: message.ref.account, value: parentID)] { union(index, target) }
            }
        }

        // The provider's own threads, scoped per account.
        var byThread: [AccountScopedKey: Int] = [:]
        for (index, message) in messages.enumerated() {
            guard let thread = message.envelope.thread else { continue }
            let key = AccountScopedKey(account: message.ref.account, value: thread)
            if let existing = byThread[key] { union(existing, index) } else { byThread[key] = index }
        }

        // Subject fallback, oldest first so a reply joins the latest earlier message on that subject.
        let order = messages.indices.sorted { (messages[$0].envelope.date ?? .distantPast) < (messages[$1].envelope.date ?? .distantPast) }
        var lastBySubject: [AccountScopedKey: Int] = [:]
        for index in order {
            let envelope = messages[index].envelope
            let (normalized, hadPrefix) = threadKey(envelope.subject)
            guard normalized.count >= 4 else { continue }
            let key = AccountScopedKey(account: messages[index].ref.account, value: normalized)
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
            var chosenIDs = Set<String>(), chosenMessageIDs = Set<String>()
            var unique: [Int] = []
            for index in members.sorted(by: { isPrimary[$0] && !isPrimary[$1] }) {
                let envelope = messages[index].envelope
                guard chosenIDs.insert(envelope.id).inserted else { continue }
                if let id = envelope.messageID, !chosenMessageIDs.insert(id).inserted { continue }
                unique.append(index)
            }
            conversations.append(Conversation(thread: unique.map { messages[$0] }))
        }
        conversations.sortNewestFirst(by: \.latestDate)
        return conversations
    }

    /// Folds conversations that are one sender repeating itself (every message from the same
    /// address, on the same subject, such as security alerts or listing digests the provider
    /// threads apart) into one, so the inbox shows that stream once at its latest message.
    /// Conversations with anyone else in them stay as they are.
    static func mergeRepeats(_ conversations: [Conversation]) -> [Conversation] {
        var index: [RepeatKey: Int] = [:]
        var groups: [[Conversation]] = []
        for conversation in conversations {
            guard let key = repeatKey(conversation) else {
                groups.append([conversation])
                continue
            }
            if let existing = index[key] {
                groups[existing].append(conversation)
            } else {
                index[key] = groups.count
                groups.append([conversation])
            }
        }
        // `conversations` is newest first, so each group sits where its latest member did.
        return groups.map { $0.count > 1 ? Conversation(thread: $0.flatMap(\.messages)) : $0[0] }
    }

    private nonisolated struct RepeatKey: Hashable {
        let account: String
        let sender: String
        let subject: String
    }

    /// Account, sender and subject of a conversation whose every message has that one sender.
    private static func repeatKey(_ conversation: Conversation) -> RepeatKey? {
        guard let sender = conversation.messages.first?.envelope.from.first?.email.lowercased() else { return nil }
        for message in conversation.messages where message.envelope.from.first?.email.lowercased() != sender {
            return nil
        }
        let (subject, _) = threadKey(conversation.subject)
        guard !subject.isEmpty else { return nil }
        return RepeatKey(account: conversation.account, sender: sender, subject: subject)
    }

    /// The subject as threading compares it: list tags and title lowercased, whitespace collapsed;
    /// and whether it had a reply/forward prefix.
    private static func threadKey(_ raw: String) -> (String, Bool) {
        let subject = Subject(raw)
        let text = subject.list.isEmpty ? subject.title : subject.list + " " + subject.title
        return (text.lowercased().split(whereSeparator: \.isWhitespace).joined(separator: " "), subject.prefix != nil)
    }
}
