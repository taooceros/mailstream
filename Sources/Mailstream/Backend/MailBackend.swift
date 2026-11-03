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

    /// Messages in `mailbox` matching `query` (free text over subject, people and body), newest first.
    @concurrent func search(mailbox: String, query: String, pageSize: Int) async throws -> [Envelope]

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

nonisolated func parseISODate(_ raw: String) -> Date? {
    if let date = try? Date(raw, strategy: .iso8601) { return date }
    return try? Date(raw, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
}
