import Foundation

/// The `chat.*` commands over `ChatStore`: accounts and spaces come from what the store already
/// polls; messages are fetched from Google per request, leaving the native reader's state alone.
extension ControlService {
    private var chat: ChatStore { store.chat }

    func chatAccounts() -> JSON {
        .array(chat.accounts.map { account in
            [
                "id": .string(account.name),
                "label": .string(accounts.record(account.name)?.label ?? account.name),
                "canStart": .bool(chat.canStartChats(account: account.name)),
                "unread": .int(chat.unreadSpaces(account: account.name)),
            ]
        })
    }

    func chatSpaces(_ args: Arguments) throws -> JSON {
        let account = try args.string("account").map(chatAccount)
        return [
            "loading": .bool(chat.isLoading),
            "spaces": .array(chat.rows(account: account).compactMap { row in
                chat.space(row.key).map { Self.json($0, row: row) }
            }),
        ]
    }

    func chatMessages(_ args: Arguments) async throws -> JSON {
        let key = try chatKey(args)
        let before = try args.string("before").map { raw in
            guard let date = parseISODate(raw) else { throw ControlError.usage("--before must be an ISO 8601 time, not \"\(raw)\".") }
            return date
        }
        let limit = min(max(args.int("limit") ?? 50, 1), ChatStore.messagesPage)
        let page = try await chat.page(key, limit: limit, before: before)
        return [
            "messages": .array(page.messages.map { Self.json($0) }),
            "more": .bool(page.more),
            "lastRead": Self.date(page.lastRead),
            "me": chat.me(account: key.account).json,
        ]
    }

    func chatSend(_ args: Arguments) async throws -> JSON {
        let key = try chatKey(args)
        let text = try args.required("body").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw ControlError.usage("--body is empty.") }
        let title = chat.space(key).map { "\"\($0.title)\"" } ?? key.space
        try requireYes(args, "Would send \(text.count) characters to \(title) as \(key.account). Re-run with --yes.")
        let message = try await chat.deliver(text, thread: args.string("thread"), in: key)
        return ["sent": true, "message": Self.json(message)]
    }

    func chatRead(_ args: Arguments) throws -> JSON {
        let key = try chatKey(args)
        chat.markRead(key)
        return ["read": .string(key.space)]
    }

    func chatStart(_ args: Arguments) async throws -> JSON {
        let account = try chatAccount(try args.required("account"))
        let emails = try (args.list("emails") ?? []).map { raw in
            guard let address = Address.parse(raw), address.email.contains("@") else {
                throw ControlError.usage("\"\(raw)\" is not an email address.")
            }
            return address.email
        }
        let key = try await chat.start(account: account, with: emails)
        guard let space = chat.space(key) else { return ["account": .string(account), "id": .string(key.space)] }
        return Self.json(space, row: ChatRow(space, account: account, calendar: .current, now: Date()))
    }

    // MARK: - Arguments

    /// `--account` as a chat account's id.
    private func chatAccount(_ raw: String) throws -> String {
        let id = raw.lowercased()
        guard chat.account(named: id) != nil else {
            throw ControlError("unknown_account", "No Google Chat account \(id); `mailstream chat accounts` lists them.")
        }
        return id
    }

    private func chatKey(_ args: Arguments) throws -> ChatSpaceKey {
        let account = try chatAccount(try args.required("account"))
        let space = try args.required("space")
        guard space.hasPrefix("spaces/") else {
            throw ControlError.usage("A space id looks like spaces/…; `mailstream chat spaces` lists them.")
        }
        return ChatSpaceKey(account: account, space: space)
    }

    // MARK: - JSON

    /// Microsecond RFC 3339, so a message's `created` passed back as `--before` loses nothing.
    private static func date(_ date: Date?) -> JSON {
        date.map { .string(formatChatDate($0)) } ?? .null
    }

    private static func json(_ space: ChatSpace, row: ChatRow) -> JSON {
        [
            "account": .string(row.key.account),
            "id": .string(space.name),
            "kind": .string(space.kind.rawValue),
            "title": .string(row.title),
            "snippet": .string(row.preview),
            "unread": .int(space.unread),
            "lastActive": date(space.activity),
            "lastRead": date(space.lastRead),
            "hasAttachment": .bool(row.hasAttachment),
            "avatar": [
                "key": (row.avatarKey.isEmpty ? nil : row.avatarKey).json,
                "name": .string(row.avatarName),
                "initials": .string(row.avatarInitials),
                "colorKey": .string(row.avatarColorKey),
            ],
            "members": .array(space.members.map { json($0) }),
        ]
    }

    private static func json(_ user: ChatUser) -> JSON {
        [
            "id": .string(user.name), "name": user.displayName.json, "email": user.email.json,
            "label": .string(user.label), "bot": .bool(user.bot), "avatarUrl": user.avatarUrl.json,
        ]
    }

    private static func json(_ message: ChatMessage) -> JSON {
        [
            "id": .string(message.name),
            "sender": json(message.sender),
            "created": date(message.created),
            "edited": date(message.edited),
            "text": .string(message.text),
            "thread": message.thread.json,
            "threadReply": .bool(message.threadReply),
            "mine": .bool(message.mine),
            "card": .bool(message.card),
            "attachments": .array(message.attachments.map { attachment in
                ["name": .string(attachment.name), "contentType": attachment.contentType.json, "url": attachment.url.json]
            }),
        ]
    }
}
