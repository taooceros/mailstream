import CryptoKit
import Foundation

/// On-disk cache under ~/Library/Caches so launches paint from the last known state before the
/// network answers. Everything here is disposable.
///
/// Reads are `async` and nonisolated, so they always run on the concurrent executor, never on the
/// caller's (main) thread. Writes go through one serial queue, which also encodes, so the last
/// write for a file always wins and callers never wait.
final class MailCache: Sendable {
    static let contentVersion = 2
    static let scheme = "mscid"

    let root: URL
    private let writer = DispatchQueue(label: "dev.mailstream.cache-writer", qos: .utility)

    init() {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        root = caches.appendingPathComponent("dev.mailstream.Mailstream", isDirectory: true)
        let root = root
        writer.async {
            for sub in ["envelopes", "messages", "mailboxes", "chat"] {
                try? FileManager.default.createDirectory(at: root.appendingPathComponent(sub), withIntermediateDirectories: true)
            }
        }
    }

    static func hash(_ parts: String...) -> String {
        let digest = SHA256.hash(data: Data(parts.joined(separator: "\u{1F}").utf8))
        return digest.prefix(16).map { String(format: "%02x", $0) }.joined()
    }

    private func read<T: Decodable>(_ type: T.Type, at url: URL) async -> T? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    private func write<T: Encodable & Sendable>(_ value: T, to url: URL, creatingDirectory: Bool = false) {
        writer.async {
            guard let data = try? JSONEncoder().encode(value) else { return }
            if creatingDirectory {
                try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            }
            try? data.write(to: url, options: .atomic)
        }
    }

    // MARK: Accounts

    private var accountsURL: URL { root.appendingPathComponent("accounts-v1.json") }

    func loadAccounts() async -> [Account]? { await read([Account].self, at: accountsURL) }

    func saveAccounts(_ accounts: [Account]) { write(accounts, to: accountsURL) }

    // MARK: Tags

    private var tagsURL: URL { root.appendingPathComponent("tags-v1.json") }

    func loadTags() async -> [String: ConversationTag]? { await read([String: ConversationTag].self, at: tagsURL) }

    func saveTags(_ tags: [String: ConversationTag]) { write(tags, to: tagsURL) }

    // MARK: Envelopes

    private func envelopesURL(_ key: MailboxKey) -> URL {
        root.appendingPathComponent("envelopes/\(Self.hash(key.account, key.mailbox)).json")
    }

    func loadEnvelopes(_ key: MailboxKey) async -> [Envelope]? { await read([Envelope].self, at: envelopesURL(key)) }

    func saveEnvelopes(_ envelopes: [Envelope], for key: MailboxKey) { write(envelopes, to: envelopesURL(key)) }

    // MARK: Mailboxes

    private func mailboxesURL(_ account: String) -> URL {
        root.appendingPathComponent("mailboxes/\(Self.hash(account)).json")
    }

    func loadMailboxes(_ account: String) async -> [Mailbox]? { await read([Mailbox].self, at: mailboxesURL(account)) }

    func saveMailboxes(_ mailboxes: [Mailbox], for account: String) { write(mailboxes, to: mailboxesURL(account)) }

    // MARK: Message content

    func messageKey(_ ref: MessageRef) -> String { Self.hash(ref.account, ref.mailbox, ref.id) }

    func messageDirectory(_ ref: MessageRef) -> URL {
        messageDirectory(key: messageKey(ref))
    }

    func messageDirectory(key: String) -> URL {
        root.appendingPathComponent("messages/\(key)", isDirectory: true)
    }

    /// Base URL inline parts of `ref` are served from by the reader's scheme handler.
    func partURLBase(_ ref: MessageRef) -> String { "\(Self.scheme)://\(messageKey(ref))/" }

    private func contentURL(_ ref: MessageRef) -> URL {
        messageDirectory(ref).appendingPathComponent("content-v\(Self.contentVersion).json")
    }

    func loadContent(_ ref: MessageRef) async -> MessageContent? { await read(MessageContent.self, at: contentURL(ref)) }

    func saveContent(_ content: MessageContent, for ref: MessageRef) {
        write(content, to: contentURL(ref), creatingDirectory: true)
    }

    func attachmentURL(_ attachment: Attachment, of ref: MessageRef) -> URL {
        messageDirectory(ref).appendingPathComponent(attachment.storedName)
    }

    // MARK: Chat

    private var chatAccountsURL: URL { root.appendingPathComponent("chat-accounts-v1.json") }

    func loadChatAccounts() async -> [ChatAccount]? { await read([ChatAccount].self, at: chatAccountsURL) }

    func saveChatAccounts(_ accounts: [ChatAccount]) { write(accounts, to: chatAccountsURL) }

    private func chatSpacesURL(_ account: String) -> URL {
        root.appendingPathComponent("chat/spaces-\(Self.hash(account)).json")
    }

    func loadChatSpaces(_ account: String) async -> ChatSpaceList? { await read(ChatSpaceList.self, at: chatSpacesURL(account)) }

    func saveChatSpaces(_ list: ChatSpaceList, for account: String) { write(list, to: chatSpacesURL(account)) }

    private var chatRecentURL: URL { root.appendingPathComponent("chat/recent-v1.json") }

    /// Spaces whose messages are kept on disk, most recently opened first.
    func loadChatRecent() async -> [ChatSpaceKey]? { await read([ChatSpaceKey].self, at: chatRecentURL) }

    func saveChatRecent(_ keys: [ChatSpaceKey]) { write(keys, to: chatRecentURL) }

    private func chatMessagesURL(_ key: ChatSpaceKey) -> URL {
        root.appendingPathComponent("chat/messages-\(Self.hash(key.account, key.space)).json")
    }

    func loadChatMessages(_ key: ChatSpaceKey) async -> ChatMessagePage? {
        await read(ChatMessagePage.self, at: chatMessagesURL(key))
    }

    func saveChatMessages(_ page: ChatMessagePage, for key: ChatSpaceKey) { write(page, to: chatMessagesURL(key)) }

    func removeChatMessages(_ key: ChatSpaceKey) {
        let url = chatMessagesURL(key)
        writer.async { try? FileManager.default.removeItem(at: url) }
    }
}
