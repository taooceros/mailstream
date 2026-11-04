import Foundation

/// A Google Chat request that could not be made, or an answer that could not be read.
nonisolated struct ChatError: LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }
}

// MARK: - Chat values (also the disk cache's JSON)

/// A Google account with Chat access; `name` and `email` are both its address (the account id).
nonisolated struct ChatAccount: Codable, Hashable, Sendable {
    let name: String
    let email: String?
}

nonisolated struct ChatUser: Codable, Hashable, Sendable {
    /// `users/<id>`, stable across spaces.
    let name: String
    let displayName: String?
    let email: String?
    let avatarUrl: String?
    let bot: Bool

    init(from decoder: any Decoder) throws {
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

nonisolated struct ChatAttachment: Codable, Hashable, Sendable {
    let name: String
    let contentType: String?
    let url: String?
}

nonisolated struct ChatMessage: Codable, Hashable, Sendable {
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

    init(from decoder: any Decoder) throws {
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

nonisolated enum ChatSpaceKind: String, Codable, Sendable {
    case dm, group, space, bot

    init(from decoder: any Decoder) throws {
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

nonisolated struct ChatSpace: Codable, Hashable, Sendable {
    /// `spaces/x`.
    let name: String
    let kind: ChatSpaceKind
    /// Never empty: the space's name, the DM peer, or the members' names.
    let title: String
    /// Other people (not the user) in a DM or group; empty for named spaces.
    let members: [ChatUser]
    var lastActive: Date?
    var lastRead: Date?
    /// Messages after `lastRead` not sent by the user, capped at `Chat.summaryMessages`.
    var unread: Int
    var latest: ChatMessage?

    init(from decoder: any Decoder) throws {
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

/// One `Chat.spaces` answer; also the disk cache of an account's space list.
nonisolated struct ChatSpaceList: Codable, Sendable {
    /// The user's `users/<id>`.
    var me: String?
    var more: Bool
    var spaces: [ChatSpace]
}

/// One `Chat.messages` answer: ascending, the newest `limit` before `before`.
nonisolated struct ChatMessagePage: Codable, Sendable {
    var messages: [ChatMessage]
    var more: Bool
    var lastRead: Date?
}

/// RFC 3339 in UTC with microseconds, the precision Google reports, for the `createTime` filter.
nonisolated func formatChatDate(_ date: Date) -> String {
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

// MARK: - From the API

nonisolated extension ChatUser {
    init(_ user: ChatAPI.User?) {
        name = user?.name ?? ""
        displayName = user?.displayName.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
        email = user?.email.flatMap { $0.isEmpty ? nil : $0 }
        avatarUrl = user?.avatarUrl.flatMap { $0.isEmpty ? nil : $0 }
        bot = user?.type == "BOT"
    }
}

nonisolated extension ChatMessage {
    /// `mine` overrides the comparison of the sender with `me`.
    init(_ message: ChatAPI.Message, me: String?, mine: Bool? = nil) {
        let sender = ChatUser(message.sender)
        // `text` has the markup stripped; `formattedText` keeps it (`*bold*`, `<url|label>`,
        // `<users/…>` mentions), with a trailing newline.
        let formatted = message.formattedText.map { String($0.reversed().drop { $0 == "\n" }.reversed()) }
        text = [formatted, message.text].compactMap { $0 }.first { !$0.isEmpty } ?? message.fallbackText ?? ""
        let createdRaw = message.createTime.flatMap(parseISODate)
        created = createdRaw ?? .distantPast
        // The API sets `lastUpdateTime` on every message; only a later one is an edit.
        edited = message.lastUpdateTime.flatMap(parseISODate).flatMap { updated in
            createdRaw.map { updated > $0 } ?? true ? updated : nil
        }
        name = message.name
        self.sender = sender
        thread = message.thread?.name
        threadReply = message.threadReply
        self.mine = mine ?? me.map { sender.name == $0 } ?? false
        card = message.cardsV2 != nil || message.cards != nil
        attachments = message.attachment.map { attachment in
            let download = attachment.downloadUri.flatMap { $0.isEmpty ? nil : $0 }
            let drive = attachment.driveDataRef?.driveFileId.map { "https://drive.google.com/open?id=\($0)" }
            return ChatAttachment(name: attachment.contentName ?? "Attachment", contentType: attachment.contentType ?? "",
                                  url: download ?? drive)
        }
    }
}

nonisolated extension ChatSpace {
    init(name: String, kind: ChatSpaceKind, title: String, members: [ChatUser], lastActive: Date?, lastRead: Date?,
         unread: Int, latest: ChatMessage?) {
        self.name = name
        self.kind = kind
        self.title = title
        self.members = members
        self.lastActive = lastActive
        self.lastRead = lastRead
        self.unread = unread
        self.latest = latest
    }
}

// MARK: - Chat

/// Google Chat over its REST API, for the accounts `api` resolves.
///
/// The space list is polled, so it is kept cheap: each account's space summaries stay in memory
/// keyed by the space's last activity, and a poll only refetches the spaces whose activity
/// changed (plus the read state of the ones still unread, which another client may have read).
actor Chat {
    /// Messages read per space for its summary: the latest one, and enough to count what is unread.
    static let summaryMessages = 25
    /// Space summaries fetched at once.
    static let fanOut = 6

    private struct Summary {
        var space: ChatSpace
        /// `lastActiveTime` as Google sent it; a different one means the space changed.
        let lastActiveRaw: String
        /// Its unread count went out of date; the next poll recounts it.
        var stale = false
    }

    private struct Memory {
        /// The user's own `users/{id}`, learned from a read state.
        var me: String?
        var summaries: [String: Summary] = [:]
        /// Members of DMs and unnamed group chats, which name them.
        var members: [String: [ChatAPI.User]] = [:]
    }

    private let api: @MainActor @Sendable (String) -> API?
    private var memory: [String: Memory] = [:]

    /// `api` resolves an account id to its requests, or nil when it has no Chat access.
    init(api: @escaping @MainActor @Sendable (String) -> API?) {
        self.api = api
    }

    /// The `limit` most recently active spaces, newest first.
    func spaces(account: String, limit: Int) async throws -> ChatSpaceList {
        let api = try await resolve(account)
        let limit = max(limit, 1)
        var all = try await ChatAPI.spaces(api)
        all.sort { (parseISODate($0.lastActiveTime ?? "") ?? .distantPast) > (parseISODate($1.lastActiveTime ?? "") ?? .distantPast) }
        let more = all.count > limit
        all = Array(all.prefix(limit))

        // What memory can't answer: spaces new or active since, and unread ones (read elsewhere?).
        var stale: [ChatAPI.Space] = [], recheck: [ChatAPI.Space] = []
        for space in all {
            if let known = memory[account]?.summaries[space.name], !known.stale, known.lastActiveRaw == space.lastActiveTime {
                if known.space.unread > 0 { recheck.append(space) }
            } else {
                stale.append(space)
            }
        }

        async let freshResults = Self.bounded(stale) { space in try await self.summarize(api, account: account, space: space) }
        async let recheckResults = Self.bounded(recheck) { space in try await ChatAPI.readState(api, space: space.name) }
        let fresh = await freshResults, rechecked = await recheckResults

        var failures: [any Error] = []
        for result in fresh {
            switch result {
            case .success(let summary): memory[account, default: Memory()].summaries[summary.space.name] = summary
            case .failure(let error): failures.append(error)
            }
        }
        for (space, result) in zip(recheck, rechecked) {
            guard case .success(let state) = result, let raw = state.lastReadTime,
                  var summary = memory[account]?.summaries[space.name] else { continue }
            let lastRead = parseISODate(raw)
            guard lastRead != summary.space.lastRead else { continue }
            if let lastRead, lastRead >= (parseISODate(summary.lastActiveRaw) ?? .distantPast) {
                summary.space.unread = 0
                summary.space.lastRead = lastRead
            } else {
                // Read part of the way elsewhere: shown as it was until the next poll recounts it.
                summary.stale = true
            }
            memory[account]?.summaries[space.name] = summary
        }
        let summaries = all.compactMap { memory[account]?.summaries[$0.name]?.space }
        // A partial list is better than none, unless nothing at all could be read.
        if summaries.isEmpty, let failure = failures.first { throw failure }
        return ChatSpaceList(me: memory[account]?.me, more: more, spaces: summaries)
    }

    /// The newest `limit` messages created before `before` (default: now), ascending.
    func messages(account: String, space: String, limit: Int, before: Date? = nil) async throws -> ChatMessagePage {
        try ChatAPI.check(space, shape: ["spaces"])
        let api = try await resolve(account)
        async let readState = ChatAPI.readState(api, space: space)
        let page = try await ChatAPI.messages(api, space: space, limit: limit, before: before.map(formatChatDate))
        let state = try? await readState
        let me = learnMe(state?.user, account: account)
        return ChatMessagePage(messages: (page.messages ?? []).reversed().map { ChatMessage($0, me: me) },
                               more: !(page.nextPageToken ?? "").isEmpty,
                               lastRead: state?.lastReadTime.flatMap(parseISODate))
    }

    /// Marks the space read up to now; returns the new read time.
    func markRead(account: String, space: String) async throws -> Date? {
        try ChatAPI.check(space, shape: ["spaces"])
        let api = try await resolve(account)
        let now = Date()
        let state = try await ChatAPI.markRead(api, space: space, time: formatChatDate(now))
        let lastRead = state.lastReadTime.flatMap(parseISODate) ?? now
        if var summary = memory[account]?.summaries[space] {
            summary.space.unread = 0
            summary.space.lastRead = lastRead
            memory[account]?.summaries[space] = summary
        }
        return lastRead
    }

    /// Sends as the user, in `thread` when given. Not idempotent: never retried here.
    func send(account: String, space: String, text: String, thread: String?) async throws -> ChatMessage {
        try ChatAPI.check(space, shape: ["spaces"])
        if let thread {
            try ChatAPI.check(thread, shape: ["spaces", "threads"])
            guard thread.hasPrefix(space + "/") else { throw ChatError(message: "Thread \(thread) is not in \(space)") }
        }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw ChatError(message: "Message is empty") }
        let api = try await resolve(account)
        let message = try await ChatAPI.createMessage(api, space: space, text: text, thread: thread)
        let me = message.sender?.name
        // The space's activity moved: its summary is refetched on the next poll.
        memory[account]?.summaries[space]?.stale = true
        if memory[account, default: Memory()].me == nil { memory[account, default: Memory()].me = me }
        return ChatMessage(message, me: me, mine: true)
    }

    /// Forgets an account's summaries (it was removed).
    func forget(account: String) {
        memory[account] = nil
    }

    // MARK: Summaries

    private func summarize(_ api: API, account: String, space: ChatAPI.Space) async throws -> Summary {
        let state = try await ChatAPI.readState(api, space: space.name)
        let me = learnMe(state.user, account: account)
        let page = try await ChatAPI.messages(api, space: space.name, limit: Self.summaryMessages, before: nil)
        let messages = page.messages ?? []
        let lastRead = state.lastReadTime.flatMap(parseISODate)
        let unread = messages.filter { message in
            guard message.sender?.name != me else { return false }
            guard let lastRead else { return true }
            return (message.createTime.flatMap(parseISODate) ?? .distantPast) > lastRead
        }.count

        let kind: ChatSpaceKind = switch space.spaceType {
        case "DIRECT_MESSAGE" where space.singleUserBotDm: .bot
        case "DIRECT_MESSAGE": .dm
        case "GROUP_CHAT": .group
        default: .space
        }
        let displayName = space.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        let named = !displayName.isEmpty
        var members: [ChatAPI.User] = []
        if !named {
            if let known = memory[account]?.members[space.name] {
                members = known
            } else {
                members = try await ChatAPI.members(api, space: space.name).filter { $0.name != me }
                memory[account, default: Memory()].members[space.name] = members
            }
        }
        // Google withholds names of some members (apps, people without "affinity"), but not of a
        // message's sender: fill them in from the messages at hand.
        members = members.map { member in
            guard member.displayName == nil else { return member }
            return messages.lazy.compactMap(\.sender).first { $0.name == member.name && $0.displayName != nil } ?? member
        }
        let title = named ? displayName : Self.title(members, kind: kind)
        let summary = ChatSpace(name: space.name, kind: kind, title: title, members: members.map { ChatUser($0) },
                                lastActive: space.lastActiveTime.flatMap(parseISODate), lastRead: lastRead,
                                unread: unread, latest: messages.first.map { ChatMessage($0, me: me) })
        return Summary(space: summary, lastActiveRaw: space.lastActiveTime ?? "")
    }

    /// Records the user's id when a read state names it; returns the best known one.
    private func learnMe(_ user: String?, account: String) -> String? {
        if let user { memory[account, default: Memory()].me = user }
        return memory[account]?.me
    }

    /// An unnamed space is named after the people in it.
    private static func title(_ members: [ChatAPI.User], kind: ChatSpaceKind) -> String {
        let names = members.compactMap { $0.displayName ?? $0.email }.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
        switch (names.count, kind) {
        case (0, .bot): return "App"
        case (0, .dm): return "Direct message"
        case (0, _): return "Group chat"
        case (1...4, _): return names.joined(separator: ", ")
        default: return names.prefix(3).joined(separator: ", ") + ", +\(names.count - 3)"
        }
    }

    // MARK: Plumbing

    private func resolve(_ account: String) async throws -> API {
        guard let api = await api(account) else { throw ChatError(message: "No Google Chat account named \(account)") }
        return api
    }

    /// Runs `work` over `items`, at most `fanOut` at once; results keep `items`' order.
    private static func bounded<T: Sendable, R: Sendable>(_ items: [T], _ work: @escaping @Sendable (T) async throws -> R)
        async -> [Result<R, any Error>] {
        var results = [Result<R, any Error>?](repeating: nil, count: items.count)
        await withTaskGroup(of: (Int, Result<R, any Error>).self) { group in
            let run: @Sendable (Int, T) async -> (Int, Result<R, any Error>) = { index, item in
                do {
                    let value = try await work(item)
                    return (index, .success(value))
                } catch {
                    return (index, .failure(error))
                }
            }
            var next = 0
            while next < min(items.count, fanOut) {
                let index = next, item = items[index]
                group.addTask { await run(index, item) }
                next += 1
            }
            for await (index, result) in group {
                results[index] = result
                guard next < items.count else { continue }
                let index = next, item = items[index]
                group.addTask { await run(index, item) }
                next += 1
            }
        }
        return results.map { $0 ?? .failure(ChatError(message: "Space summary did not run")) }
    }
}
