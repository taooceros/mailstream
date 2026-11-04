import Foundation

nonisolated struct Address: Codable, Hashable, Sendable {
    var name: String?
    var email: String

    /// Display name when present, the address otherwise.
    var displayName: String {
        if let name = name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty,
           name.caseInsensitiveCompare(email) != .orderedSame {
            return name.trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
        }
        return email
    }

    /// First name for compact participant lists ("Alice, Bob").
    var shortName: String {
        let full = displayName
        if full == email { return String(email.split(separator: "@").first ?? Substring(email)) }
        if full.unicodeScalars.contains(where: { $0.properties.isIdeographic }) { return full }
        let first = full.split(whereSeparator: { $0 == " " || $0 == "," }).first.map(String.init) ?? full
        return first
    }

    /// RFC 5322 mailbox form, as recipient tokens hold it.
    var rfc5322: String {
        guard let name = name?.trimmingCharacters(in: .whitespaces), !name.isEmpty, name != email else { return email }
        let escaped = name.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        return "\"\(escaped)\" <\(email)>"
    }

    var normalizedEmail: String { email.lowercased() }

    /// Parses `Name <a@b>` or a bare address.
    static func parse(_ raw: String) -> Address? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let lt = trimmed.lastIndex(of: "<"), let gt = trimmed.lastIndex(of: ">"), lt < gt {
            let email = String(trimmed[trimmed.index(after: lt)..<gt]).trimmingCharacters(in: .whitespaces)
            var name = String(trimmed[..<lt]).trimmingCharacters(in: .whitespaces)
            if name.count >= 2, name.hasPrefix("\""), name.hasSuffix("\"") {
                // The quoted form `rfc5322` writes, escapes undone.
                name = String(name.dropFirst().dropLast())
                    .replacingOccurrences(of: "\\\"", with: "\"").replacingOccurrences(of: "\\\\", with: "\\")
            } else {
                name = name.trimmingCharacters(in: CharacterSet(charactersIn: "\""))
            }
            guard email.contains("@") else { return nil }
            return Address(name: name.isEmpty ? nil : name, email: email)
        }
        guard trimmed.contains("@") else { return nil }
        return Address(name: nil, email: trimmed)
    }
}

nonisolated enum MailboxRole: String, Codable, Sendable, CaseIterable {
    case inbox, starred, important, sent, drafts, archive, all, junk, trash, other

    var sortOrder: Int {
        switch self {
        case .inbox: 0
        case .starred: 1
        case .important: 2
        case .sent: 3
        case .drafts: 4
        case .archive: 5
        case .all: 6
        case .junk: 7
        case .trash: 8
        case .other: 9
        }
    }

    var title: String {
        switch self {
        case .inbox: "Inbox"
        case .starred: "Starred"
        case .important: "Important"
        case .sent: "Sent"
        case .drafts: "Drafts"
        case .archive: "Archive"
        case .all: "All Mail"
        case .junk: "Spam"
        case .trash: "Trash"
        case .other: "Folder"
        }
    }

    var symbol: String {
        switch self {
        case .inbox: "tray"
        case .starred: "star"
        case .important: "bookmark"
        case .sent: "paperplane"
        case .drafts: "doc"
        case .archive: "archivebox"
        case .all: "tray.full"
        case .junk: "xmark.bin"
        case .trash: "trash"
        case .other: "folder"
        }
    }

    /// Mailboxes whose listing should show recipients rather than senders.
    var showsRecipients: Bool { self == .sent || self == .drafts }
}

nonisolated struct Account: Hashable, Sendable, Identifiable, Codable {
    /// `AccountRecord.id`: the lowercased address.
    let name: String
    /// Shown in the sidebar and on threads.
    var label: String
    var provider: Provider
    /// The address and the name the account sends as.
    var identity: Address?

    var id: String { name }
    var email: String { identity?.email ?? name }
}

nonisolated struct Mailbox: Hashable, Sendable, Identifiable, Codable {
    let account: String
    let id: String
    let name: String
    var role: MailboxRole
    var unread: Int?
    var total: Int?

    var key: MailboxKey { MailboxKey(account: account, mailbox: id) }

    var displayName: String { role == .other ? name : role.title }
}

nonisolated struct MailboxKey: Hashable, Sendable, Codable {
    let account: String
    let mailbox: String
}

nonisolated struct Envelope: Codable, Hashable, Sendable {
    let id: String
    /// The provider's thread (Gmail thread id, Graph conversation id).
    let thread: String?
    let messageID: String?
    let inReplyTo: [String]
    var flags: Set<String>
    let subject: String
    let from: [Address]
    let to: [Address]
    let date: Date?
    let size: Int
    let hasAttachment: Bool?

    var isSeen: Bool { flags.contains("seen") }
    var isFlagged: Bool { flags.contains("flagged") }
    var isDraft: Bool { flags.contains("draft") }
}

nonisolated struct MessageRef: Hashable, Sendable, Codable {
    let account: String
    let mailbox: String
    let id: String

    var mailboxKey: MailboxKey { MailboxKey(account: account, mailbox: mailbox) }
}

nonisolated struct MailMessage: Hashable, Sendable, Identifiable {
    let ref: MessageRef
    var envelope: Envelope
    var role: MailboxRole

    var id: MessageRef { ref }

    /// Written by the user: a draft, sent mail, or from one of `own` (lowercased addresses).
    func isFromUser(own: Set<String>) -> Bool {
        envelope.isDraft || role == .sent || envelope.from.contains { own.contains($0.normalizedEmail) }
    }
}

/// One thread. Everything the list sorts and filters by is worked out once, when the threader
/// builds it, instead of on every read.
nonisolated struct Conversation: Identifiable, Hashable, Sendable {
    let id: String
    let account: String
    /// Ascending by date.
    let messages: [MailMessage]
    let latestDate: Date?
    let subject: String
    let isUnread: Bool
    let isFlagged: Bool
    let hasAttachment: Bool
    let hasDraft: Bool

    init(id: String, account: String, messages: [MailMessage]) {
        self.id = id
        self.account = account
        self.messages = messages
        var latestDate: Date?
        var unread = false, flagged = false, attachment = false, draft = false
        for message in messages {
            let envelope = message.envelope
            if let date = envelope.date, date > (latestDate ?? .distantPast) { latestDate = date }
            if !envelope.isSeen && message.role != .sent { unread = true }
            if envelope.isFlagged { flagged = true }
            if envelope.hasAttachment == true { attachment = true }
            if envelope.isDraft { draft = true }
        }
        self.latestDate = latestDate
        let subject = messages.first(where: { !$0.envelope.subject.isEmpty })?.envelope.subject ?? ""
        self.subject = subject.isEmpty ? "(No Subject)" : subject
        isUnread = unread
        isFlagged = flagged
        hasAttachment = attachment
        hasDraft = draft
    }

    var latest: MailMessage { messages[messages.count - 1] }

    /// The latest message the user did not write; nil when they wrote them all.
    func latestIncoming(own: Set<String>) -> MailMessage? {
        messages.last { !$0.isFromUser(own: own) }
    }

    /// The same thread with some envelopes changed in place (flags), for optimistic updates.
    func updating(_ refs: Set<MessageRef>, _ change: (inout Envelope) -> Void) -> Conversation {
        Conversation(id: id, account: account, messages: messages.map { message in
            guard refs.contains(message.ref) else { return message }
            var copy = message
            change(&copy.envelope)
            return copy
        })
    }
}

nonisolated struct Attachment: Codable, Hashable, Sendable {
    let part: Int
    let filename: String
    let mime: String
    let size: Int
    let contentID: String?
    let inline: Bool
    /// File name inside the message's cache directory.
    let storedName: String
}

nonisolated struct MessageContent: Codable, Sendable {
    /// HTML body with `cid:` references rewritten to the `mscid:` scheme, nil for text-only mail.
    var html: String?
    var text: String?
    var cc: [Address]
    var replyTo: [Address]
    var references: [String]
    var attachments: [Attachment]
    var snippet: String
}
