import Foundation

/// One signed-in account's mail server: Gmail (`GmailBackend`) or Microsoft Graph
/// (`GraphBackend`). Mailbox ids and message ids are the provider's own (Gmail label and message
/// ids; Graph folder ids and immutable message ids) and are what `MailboxKey`, `MessageRef` and
/// the disk cache store. Every call runs off the caller's actor: answers are decoded and parts
/// written to disk there.
nonisolated protocol MailBackend: Sendable {
    /// The account's address and the name it sends as.
    @concurrent func identity() async throws -> Address

    /// Every mailbox, roles resolved. `unread` and `total` are nil unless `counts`.
    @concurrent func mailboxes(counts: Bool) async throws -> [Mailbox]

    /// Newest first. `cursor` is nil for the first page, else the previous page's `next`.
    @concurrent func envelopes(mailbox: String, cursor: String?, pageSize: Int, priority: RequestPriority) async throws -> EnvelopePage

    /// Messages anywhere in the account but spam and trash matching `query`, in the provider's own
    /// search syntax (Gmail's `from:`/`has:attachment`, Outlook's KQL), each with the mailbox it is in.
    @concurrent func search(query: String, pageSize: Int) async throws -> [SearchHit]

    /// The full message. Parts it shows inline or offers as attachments are written into
    /// `directory`; HTML references them under `urlBase` (see `MessageAssembly`). Never marks
    /// the message read.
    @concurrent func content(id: String, mailbox: String, directory: URL, urlBase: String,
                             priority: RequestPriority) async throws -> MessageContent

    @concurrent func setFlag(_ flag: MessageFlag, _ on: Bool, ids: [String], mailbox: String) async throws

    /// Moves to another mailbox of this account. Moving to the `.all` mailbox archives.
    @concurrent func move(ids: [String], from: String, to: String) async throws

    /// Deletes permanently; only called for messages already in trash.
    @concurrent func delete(ids: [String], mailbox: String) async throws

    /// Sends and files the copy in Sent. Not idempotent: never repeated.
    @concurrent func send(_ message: OutgoingMessage) async throws
}

nonisolated struct EnvelopePage: Sendable {
    var envelopes: [Envelope]
    /// Cursor of the next older page; nil at the end.
    var next: String?
}

/// A search result: the message and the mailbox it lists under.
nonisolated struct SearchHit: Sendable {
    let mailbox: String
    let envelope: Envelope
}

nonisolated enum MessageFlag: String, Sendable {
    case seen, flagged
}

/// A message to send. `references` and `inReplyTo` are normalized Message-IDs (no angle
/// brackets); `thread` is the provider's thread of the message replied to or forwarded.
nonisolated struct OutgoingMessage: Sendable {
    var from: Address
    var to: [Address]
    var cc: [Address]
    var bcc: [Address]
    var subject: String
    var text: String
    /// An HTML alternative of `text`, when the reply quotes HTML.
    var html: String?
    var attachments: [URL]
    var inReplyTo: String?
    var references: [String]
    var thread: String?
}

nonisolated func normalizeMessageID(_ raw: String) -> String {
    raw.trimmingCharacters(in: CharacterSet(charactersIn: "<> \t\r\n")).lowercased()
}

/// Normalized ids in a Message-ID list header (`In-Reply-To`, `References`): the bracketed ones,
/// adjacent or spaced, else whitespace-separated bare ids.
nonisolated func parseMessageIDList(_ header: String?) -> [String] {
    guard let header, !header.isEmpty else { return [] }
    let bracketed = header.matches(of: /<([^<>]+)>/).map { String($0.1) }
    let raw = bracketed.isEmpty ? header.split(whereSeparator: \.isWhitespace).map(String.init) : bracketed
    return raw.map(normalizeMessageID).filter { !$0.isEmpty }
}

nonisolated extension Envelope {
    /// The flags of a message the provider reports as read, flagged and a draft.
    static func flags(seen: Bool, flagged: Bool, draft: Bool) -> Set<String> {
        var flags = Set<String>()
        if seen { flags.insert("seen") }
        if flagged { flags.insert("flagged") }
        if draft { flags.insert("draft") }
        return flags
    }
}

/// A message header as Gmail and Graph spell one out in JSON.
nonisolated struct HeaderField: Decodable, Sendable {
    let name: String
    let value: String
}

nonisolated extension [HeaderField] {
    /// The first `name` header's value; header names compare case-insensitively.
    func value(of name: String) -> String? {
        first { $0.name.caseInsensitiveCompare(name) == .orderedSame }?.value
    }
}
