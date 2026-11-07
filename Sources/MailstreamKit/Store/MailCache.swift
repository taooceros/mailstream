#if canImport(CryptoKit)
import CryptoKit
#else
import Crypto
#endif
import Foundation

/// What Mailstream has fetched and worked out, kept so launches paint the last known state before
/// the network answers and mail once fetched stays: accounts, mailboxes, every listed envelope
/// with its paging cursor, message content, AI tags and chat, in a Turso database
/// (`MailDatabase`, Application Support/Mailstream/mail.db). Attachments and inline parts are files
/// under ~/Library/Caches, which the content refers to.
///
/// Reads are `@concurrent`, so decoding never runs on the caller's (main) thread. Writes are
/// queued in call order to one writer task off the main actor, which also encodes, so the last
/// write for a row always wins and callers never wait.
nonisolated final class MailCache: Sendable {
    static let contentVersion = 3
    static let scheme = "mscid"

    /// Files: attachments, inline parts, avatars, the tagger's working directory.
    let root: URL
    private let database: MailDatabase
    private let writes: AsyncStream<@Sendable () async -> Void>.Continuation

    init() {
        root = Platform.caches.appendingPathComponent("dev.mailstream.Mailstream", isDirectory: true)
        database = MailDatabase(url: Platform.support.appendingPathComponent("mail.db"), files: root)
        let (queue, writes) = AsyncStream.makeStream(of: (@Sendable () async -> Void).self)
        self.writes = writes
        Task(name: "Cache writer", priority: .utility) {
            for await write in queue { await write() }
        }
        let root = root
        writes.yield {
            let fm = FileManager.default
            for legacy in Self.legacy { try? fm.removeItem(at: root.appendingPathComponent(legacy)) }
            try? fm.createDirectory(at: root.appendingPathComponent("messages-v2"), withIntermediateDirectories: true)
        }
    }

    deinit { writes.finish() }

    /// Layouts no longer read: from before the provider APIs, keyed by account, mailbox and chat
    /// account names that no provider id matches; tags from before automated mail could wait on
    /// the user; and listings without their paging cursor.
    private static let legacy = ["envelopes", "messages", "mailboxes", "chat", "accounts-v1.json", "chat-accounts-v1.json",
                                 "tags-v1.json", "envelopes-v2"]

    static func hash(_ parts: String...) -> String {
        let digest = SHA256.hash(data: Data(parts.joined(separator: "\u{1F}").utf8))
        return digest.prefix(16).map { String(format: "%02x", $0) }.joined()
    }

    @concurrent private func read<T: Decodable & Sendable>(_ type: T.Type, _ table: MailDatabase.Table, _ key: String) async -> T? {
        guard let data = await database.value(table, key) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    private func write<T: Encodable & Sendable>(_ value: T, _ table: MailDatabase.Table, _ key: String) {
        let database = database
        writes.yield {
            guard let data = try? JSONEncoder().encode(value) else { return }
            await database.setValue(data, table, key)
        }
    }

    // MARK: Accounts

    func loadAccounts() async -> [Account]? { await read([Account].self, .state, "accounts") }

    func saveAccounts(_ accounts: [Account]) { write(accounts, .state, "accounts") }

    // MARK: Tags

    @concurrent func loadTags() async -> [String: ConversationTag]? {
        let rows = await database.tags()
        guard !rows.isEmpty else { return nil }
        let decoder = JSONDecoder()
        return rows.compactMapValues { try? decoder.decode(ConversationTag.self, from: $0) }
    }

    /// Writes the tags that changed, then tells every Mailstream process to read them again.
    func saveTags(_ changed: [String: ConversationTag]) {
        let database = database
        writes.yield {
            let encoder = JSONEncoder()
            await database.setTags(changed.compactMapValues { try? encoder.encode($0) })
            ProcessSignal.tags.post()
        }
    }

    /// Forgets every tag, then tells every Mailstream process.
    func clearTags() {
        let database = database
        writes.yield {
            await database.clearTags()
            ProcessSignal.tags.post()
        }
    }

    /// Returns once every write queued before the call has landed.
    func flush() async {
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            writes.yield { done.resume() }
        }
    }

    // MARK: Listings

    private func listingKey(_ key: MailboxKey) -> String { Self.hash(key.account, key.mailbox) }

    @concurrent func loadListing(_ key: MailboxKey) async -> CachedListing? {
        guard let stored = await database.listing(listingKey(key)) else { return nil }
        let decoder = JSONDecoder()
        return CachedListing(envelopes: stored.envelopes.compactMap { try? decoder.decode(Envelope.self, from: $0) },
                             next: stored.next)
    }

    func saveListing(_ listing: CachedListing, for key: MailboxKey) {
        let database = database, key = listingKey(key)
        writes.yield {
            let encoder = JSONEncoder()
            let rows = listing.envelopes.compactMap { envelope in (try? encoder.encode(envelope)).map { (envelope.id, $0) } }
            await database.replaceListing(key, next: listing.next, envelopes: rows)
        }
    }

    // MARK: Mailboxes

    func loadMailboxes(_ account: String) async -> [Mailbox]? { await read([Mailbox].self, .mailboxes, Self.hash(account)) }

    func saveMailboxes(_ mailboxes: [Mailbox], for account: String) { write(mailboxes, .mailboxes, Self.hash(account)) }

    // MARK: Message content

    func messageKey(_ ref: MessageRef) -> String { Self.hash(ref.account, ref.mailbox, ref.id) }

    func messageDirectory(_ ref: MessageRef) -> URL {
        messageDirectory(key: messageKey(ref))
    }

    func messageDirectory(key: String) -> URL {
        root.appendingPathComponent("messages-v2/\(key)", isDirectory: true)
    }

    /// Base URL inline parts of `ref` are served from by the reader's scheme handler.
    func partURLBase(_ ref: MessageRef) -> String { "\(Self.scheme)://\(messageKey(ref))/" }

    private func contentKey(_ ref: MessageRef) -> String { MailDatabase.contentKey(messageKey(ref), version: Self.contentVersion) }

    func loadContent(_ ref: MessageRef) async -> MessageContent? { await read(MessageContent.self, .contents, contentKey(ref)) }

    func saveContent(_ content: MessageContent, for ref: MessageRef) { write(content, .contents, contentKey(ref)) }

    func attachmentURL(_ attachment: Attachment, of ref: MessageRef) -> URL {
        messageDirectory(ref).appendingPathComponent(attachment.storedName)
    }

    // MARK: Chat

    func loadChatAccounts() async -> [ChatAccount]? { await read([ChatAccount].self, .state, "chatAccounts") }

    func saveChatAccounts(_ accounts: [ChatAccount]) { write(accounts, .state, "chatAccounts") }

    func loadChatSpaces(_ account: String) async -> ChatSpaceList? {
        await read(ChatSpaceList.self, .chatSpaces, Self.hash(account))
    }

    func saveChatSpaces(_ list: ChatSpaceList, for account: String) { write(list, .chatSpaces, Self.hash(account)) }

    /// Spaces whose messages are kept, most recently opened first.
    func loadChatRecent() async -> [ChatSpaceKey]? { await read([ChatSpaceKey].self, .state, "chatRecent") }

    func saveChatRecent(_ keys: [ChatSpaceKey]) { write(keys, .state, "chatRecent") }

    private func chatMessagesKey(_ key: ChatSpaceKey) -> String { Self.hash(key.account, key.space) }

    func loadChatMessages(_ key: ChatSpaceKey) async -> ChatMessagePage? {
        await read(ChatMessagePage.self, .chatMessages, chatMessagesKey(key))
    }

    func saveChatMessages(_ page: ChatMessagePage, for key: ChatSpaceKey) { write(page, .chatMessages, chatMessagesKey(key)) }

    func removeChatMessages(_ key: ChatSpaceKey) {
        let database = database, key = chatMessagesKey(key)
        writes.yield { await database.remove(.chatMessages, key) }
    }
}

/// A mailbox's listing as last loaded: its envelopes, newest first, and the cursor of the page
/// after them, so paging resumes past what is kept instead of fetching it again.
nonisolated struct CachedListing: Codable, Sendable {
    var envelopes: [Envelope]
    var next: String?
}
