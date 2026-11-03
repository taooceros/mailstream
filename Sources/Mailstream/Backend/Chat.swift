import Foundation

/// A Google Chat request the engine refused, or an answer it could not be read from.
struct ChatError: LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }
}

// MARK: - JSON shapes of the engine's `chat` commands

/// An ortie account named `*-chat`; `email` is its login hint, matched to a mail account's address.
struct ChatAccount: Codable, Hashable, Sendable {
    let name: String
    let email: String?
}

struct ChatUser: Codable, Hashable, Sendable {
    /// `users/<id>`, stable across spaces.
    let name: String
    let displayName: String?
    let email: String?
    let avatarUrl: String?
    let bot: Bool

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        displayName = try c.decodeIfPresent(String.self, forKey: .displayName)
        email = try c.decodeIfPresent(String.self, forKey: .email)
        avatarUrl = try c.decodeIfPresent(String.self, forKey: .avatarUrl)
        bot = try c.decodeIfPresent(Bool.self, forKey: .bot) ?? false
    }

    /// Display name, else address, else a generic label (Google withholds both for some people).
    var label: String {
        if let displayName, !displayName.trimmingCharacters(in: .whitespaces).isEmpty { return displayName }
        if let email, !email.isEmpty { return email }
        return bot ? "App" : "Unknown"
    }

    /// First name, for previews ("Brent: …").
    var shortLabel: String {
        guard let displayName, !displayName.isEmpty else { return label }
        return displayName.split(separator: " ").first.map(String.init) ?? displayName
    }

    /// The key AvatarStore and avatar colors use: the lowercased address, else the user's resource name.
    var avatarKey: String { email?.lowercased() ?? name }

    var address: Address { Address(name: displayName, email: email ?? "") }
}

struct ChatAttachment: Codable, Hashable, Sendable {
    let name: String
    let contentType: String?
    let url: String?
}

struct ChatMessage: Codable, Hashable, Sendable {
    /// `spaces/x/messages/y`.
    let name: String
    let sender: ChatUser
    let created: Date
    let edited: Date?
    /// Plain text with Google Chat markup (`*bold*`, `_italic_`, `` `code` ``, fences, `<url|label>`).
    let text: String
    /// `spaces/x/threads/z`.
    let thread: String?
    /// A reply inside a thread, not the thread's first message.
    let threadReply: Bool
    let mine: Bool
    /// A bot card; `text` holds its fallback text or "".
    let card: Bool
    let attachments: [ChatAttachment]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        sender = try c.decode(ChatUser.self, forKey: .sender)
        created = try c.decode(Date.self, forKey: .created)
        edited = try c.decodeIfPresent(Date.self, forKey: .edited)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        thread = try c.decodeIfPresent(String.self, forKey: .thread)
        threadReply = try c.decodeIfPresent(Bool.self, forKey: .threadReply) ?? false
        mine = try c.decodeIfPresent(Bool.self, forKey: .mine) ?? false
        card = try c.decodeIfPresent(Bool.self, forKey: .card) ?? false
        attachments = try c.decodeIfPresent([ChatAttachment].self, forKey: .attachments) ?? []
    }
}

enum ChatSpaceKind: String, Codable, Sendable {
    case dm, group, space, bot

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .space
    }

    var title: String {
        switch self {
        case .dm: "Direct message"
        case .group: "Group chat"
        case .space: "Space"
        case .bot: "App"
        }
    }
}

struct ChatSpace: Codable, Hashable, Sendable {
    /// `spaces/x`.
    let name: String
    let kind: ChatSpaceKind
    /// Never empty: the space's name, the DM peer, or the members' names.
    let title: String
    /// Other people (not the user) in a DM or group; empty for named spaces.
    let members: [ChatUser]
    var lastActive: Date?
    var lastRead: Date?
    /// Messages after `lastRead` not sent by the user, capped at 25 by the engine.
    var unread: Int
    var latest: ChatMessage?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decodeIfPresent(ChatSpaceKind.self, forKey: .kind) ?? .space
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        members = try c.decodeIfPresent([ChatUser].self, forKey: .members) ?? []
        lastActive = try c.decodeIfPresent(Date.self, forKey: .lastActive)
        lastRead = try c.decodeIfPresent(Date.self, forKey: .lastRead)
        unread = try c.decodeIfPresent(Int.self, forKey: .unread) ?? 0
        latest = try c.decodeIfPresent(ChatMessage.self, forKey: .latest)
    }

    /// Newest activity: `lastActive`, else the latest message's time.
    var activity: Date? { lastActive ?? latest?.created }
}

/// One `chat spaces` answer; also the disk cache of an account's space list.
struct ChatSpaceList: Codable, Sendable {
    /// The user's `users/<id>`.
    var me: String?
    var more: Bool
    var spaces: [ChatSpace]
}

/// One `chat messages` answer: ascending, the newest `limit` before `--before`.
struct ChatMessagePage: Codable, Sendable {
    var messages: [ChatMessage]
    var more: Bool
    var lastRead: Date?
}

private struct ChatAccountsDTO: Decodable { let accounts: [ChatAccount] }
private struct ChatReadDTO: Decodable { let lastRead: Date? }
private struct ChatSentDTO: Decodable { let message: ChatMessage }
private struct ChatErrorDTO: Decodable { let error: String }

/// RFC 3339 with or without fractional seconds; fractional first, so microseconds survive.
func parseChatDate(_ raw: String) -> Date? {
    if let date = try? Date(raw, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true)) { return date }
    return try? Date(raw, strategy: .iso8601)
}

/// RFC 3339 in UTC with microseconds, the precision the engine reports, for `--before`.
func formatChatDate(_ date: Date) -> String {
    let seconds = date.timeIntervalSince1970.rounded(.down)
    var micros = Int(((date.timeIntervalSince1970 - seconds) * 1_000_000).rounded())
    var whole = Date(timeIntervalSince1970: seconds)
    if micros >= 1_000_000 {
        micros -= 1_000_000
        whole = whole.addingTimeInterval(1)
    }
    var base = whole.formatted(Date.ISO8601FormatStyle(timeZone: .gmt))
    if base.hasSuffix("Z") { base.removeLast() }
    return base + String(format: ".%06dZ", micros)
}

/// Typed wrapper over the engine's `chat` commands. The engine does all network IO, token
/// handling and caching; a call is one command line run in-process, like a himalaya one.
final class Chat: Sendable {
    func accounts() async throws -> [ChatAccount] {
        try await decode(ChatAccountsDTO.self, ["accounts"]).accounts
    }

    /// The `limit` most recently active spaces, newest first.
    func spaces(account: String, limit: Int) async throws -> ChatSpaceList {
        try await decode(ChatSpaceList.self, ["spaces", account, "--limit", String(limit)])
    }

    /// The newest `limit` messages created before `before` (default: now), ascending.
    func messages(account: String, space: String, limit: Int, before: Date? = nil) async throws -> ChatMessagePage {
        var args = ["messages", account, space, "--limit", String(limit)]
        if let before { args += ["--before", formatChatDate(before)] }
        return try await decode(ChatMessagePage.self, args)
    }

    /// Marks the space read up to now; returns the new read time.
    func markRead(account: String, space: String) async throws -> Date? {
        try await decode(ChatReadDTO.self, ["read", account, space]).lastRead
    }

    /// Sends as the user, in `thread` when given. Not idempotent: never retried here.
    func send(account: String, space: String, text: String, thread: String?) async throws -> ChatMessage {
        var args = ["send", account, space, "--text", text]
        if let thread { args += ["--thread", thread] }
        return try await decode(ChatSentDTO.self, args).message
    }

    // MARK: Plumbing

    private func decode<T: Decodable>(_ type: T.Type, _ args: [String]) async throws -> T {
        let output = try await MailEngine.run(["chat"] + args)
        guard output.status == 0 else {
            if let dto = try? JSONDecoder().decode(ChatErrorDTO.self, from: output.data) {
                throw ChatError(message: dto.error)
            }
            let text = String(decoding: output.data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            throw ChatError(message: text.isEmpty ? "chat failed with status \(output.status)" : text)
        }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let raw = try container.decode(String.self)
            guard let date = parseChatDate(raw) else {
                throw DecodingError.dataCorruptedError(in: container, debugDescription: "Not an RFC 3339 date: \(raw)")
            }
            return date
        }
        do {
            return try decoder.decode(T.self, from: output.data)
        } catch {
            throw ChatError(message: "Unexpected engine output for `chat \(args.first ?? "")`: \(error)")
        }
    }
}
