import Foundation

/// The Google Chat REST resources `Chat` reads and writes.
/// Reference: <https://developers.google.com/workspace/chat/api/reference/rest>.
nonisolated enum ChatAPI {
    static let base = URL(string: "https://chat.googleapis.com/v1/")!

    /// `path` under the API root, with `query` encoded.
    static func url(_ path: String, _ query: [(String, String)] = []) -> URL {
        var url = base.appending(path: path)
        if !query.isEmpty { url.append(queryItems: query.map { URLQueryItem(name: $0.0, value: $0.1) }) }
        return url
    }

    /// Fails unless `name` is a resource name of the given shape (`spaces/…`, `spaces/…/threads/…`),
    /// so a caller's value can't reach another path of the API.
    static func check(_ name: String, shape: [String]) throws {
        let parts = name.split(separator: "/", omittingEmptySubsequences: false)
        let allowed = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.")
        let ok = parts.count == shape.count * 2 && shape.indices.allSatisfy { index in
            let kind = parts[index * 2], id = parts[index * 2 + 1]
            return kind == shape[index] && !id.isEmpty && id.allSatisfy { allowed.contains($0) }
        }
        guard ok else { throw ChatError(message: "Invalid Google Chat resource name: \(name)") }
    }

    struct Space: Decodable, Sendable {
        let name: String
        let spaceType: String
        let displayName: String
        let lastActiveTime: String?
        let singleUserBotDm: Bool

        init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decode(String.self, forKey: .name)
            spaceType = try c.decodeIfPresent(String.self, forKey: .spaceType) ?? ""
            displayName = try c.decodeIfPresent(String.self, forKey: .displayName) ?? ""
            lastActiveTime = try c.decodeIfPresent(String.self, forKey: .lastActiveTime)
            singleUserBotDm = try c.decodeIfPresent(Bool.self, forKey: .singleUserBotDm) ?? false
        }

        private enum CodingKeys: String, CodingKey { case name, spaceType, displayName, lastActiveTime, singleUserBotDm }
    }

    struct SpacesPage: Decodable, Sendable {
        let spaces: [Space]?
        let nextPageToken: String?
    }

    struct User: Decodable, Sendable {
        let name: String
        let displayName: String?
        let email: String?
        let avatarUrl: String?
        let type: String?

        init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            displayName = try c.decodeIfPresent(String.self, forKey: .displayName)
            email = try c.decodeIfPresent(String.self, forKey: .email)
            avatarUrl = try c.decodeIfPresent(String.self, forKey: .avatarUrl)
            type = try c.decodeIfPresent(String.self, forKey: .type)
        }

        private enum CodingKeys: String, CodingKey { case name, displayName, email, avatarUrl, type }
    }

    /// Any JSON value other than null; only its presence matters.
    struct Present: Decodable, Sendable {
        init(from decoder: any Decoder) throws {}
    }

    struct Message: Decodable, Sendable {
        struct Thread: Codable, Sendable { let name: String }
        struct DriveRef: Decodable, Sendable { let driveFileId: String? }
        struct Attachment: Decodable, Sendable {
            let contentName: String?
            let contentType: String?
            let downloadUri: String?
            let driveDataRef: DriveRef?
        }

        let name: String
        let sender: User?
        let createTime: String?
        let lastUpdateTime: String?
        let text: String?
        let formattedText: String?
        let fallbackText: String?
        let thread: Thread?
        let threadReply: Bool
        let attachment: [Attachment]
        let cardsV2: Present?
        let cards: Present?

        init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            sender = try c.decodeIfPresent(User.self, forKey: .sender)
            createTime = try c.decodeIfPresent(String.self, forKey: .createTime)
            lastUpdateTime = try c.decodeIfPresent(String.self, forKey: .lastUpdateTime)
            text = try c.decodeIfPresent(String.self, forKey: .text)
            formattedText = try c.decodeIfPresent(String.self, forKey: .formattedText)
            fallbackText = try c.decodeIfPresent(String.self, forKey: .fallbackText)
            thread = try c.decodeIfPresent(Thread.self, forKey: .thread)
            threadReply = try c.decodeIfPresent(Bool.self, forKey: .threadReply) ?? false
            attachment = try c.decodeIfPresent([Attachment].self, forKey: .attachment) ?? []
            cardsV2 = try c.decodeIfPresent(Present.self, forKey: .cardsV2)
            cards = try c.decodeIfPresent(Present.self, forKey: .cards)
        }

        private enum CodingKeys: String, CodingKey {
            case name, sender, createTime, lastUpdateTime, text, formattedText, fallbackText, thread, threadReply,
                 attachment, cardsV2, cards
        }
    }

    struct MessagesPage: Decodable, Sendable {
        let messages: [Message]?
        let nextPageToken: String?
    }

    struct MembersPage: Decodable, Sendable {
        struct Membership: Decodable, Sendable { let member: User? }
        let memberships: [Membership]?
    }

    struct ReadState: Decodable, Sendable {
        /// `users/{user}/spaces/{space}/spaceReadState`: the only place the API names the caller.
        let name: String?
        let lastReadTime: String?

        /// The caller's own `users/{id}`.
        var user: String? {
            let parts = (name ?? "").split(separator: "/", omittingEmptySubsequences: false)
            guard parts.count >= 2, parts[0] == "users", !parts[1].isEmpty, parts[1] != "me" else { return nil }
            return "users/\(parts[1])"
        }
    }

    // MARK: - Calls

    /// Every space the user is in. Pages are as large as the API allows, so this is one or two calls.
    static func spaces(_ api: API) async throws -> [Space] {
        var spaces: [Space] = []
        var token: String?
        while true {
            var query = [("pageSize", "1000")]
            if let token { query.append(("pageToken", token)) }
            let page = try await api.get(SpacesPage.self, url("spaces", query))
            spaces += page.spaces ?? []
            guard let next = page.nextPageToken, !next.isEmpty else { return spaces }
            token = next
        }
    }

    /// The newest `limit` messages created before `before` (RFC 3339), newest first.
    static func messages(_ api: API, space: String, limit: Int, before: String?) async throws -> MessagesPage {
        var query = [("pageSize", String(min(max(limit, 1), 1000))), ("orderBy", "createTime desc")]
        if let before { query.append(("filter", "createTime < \"\(before)\"")) }
        return try await api.get(MessagesPage.self, url("\(space)/messages", query))
    }

    /// Members, invited ones included: a DM with someone outside the domain who hasn't accepted yet
    /// has them only as an invitation.
    static func members(_ api: API, space: String) async throws -> [User] {
        let page = try await api.get(MembersPage.self, url("\(space)/members", [("pageSize", "100"), ("showInvited", "true")]))
        return (page.memberships ?? []).compactMap(\.member)
    }

    static func readState(_ api: API, space: String) async throws -> ReadState {
        try await api.get(ReadState.self, url("users/me/\(space)/spaceReadState"))
    }

    static func markRead(_ api: API, space: String, time: String) async throws -> ReadState {
        struct Body: Encodable { let lastReadTime: String }
        let request = API.request(url("users/me/\(space)/spaceReadState", [("updateMask", "lastReadTime")]),
                                  method: "PATCH", json: Body(lastReadTime: time))
        return try await api.decode(ReadState.self, request)
    }

    /// Creates a message; never repeated (a POST).
    static func createMessage(_ api: API, space: String, text: String, thread: String?) async throws -> Message {
        struct Body: Encodable {
            let text: String
            let thread: Message.Thread?
        }
        let query = thread == nil ? [] : [("messageReplyOption", "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD")]
        let request = API.request(url("\(space)/messages", query), method: "POST",
                                  json: Body(text: text, thread: thread.map { Message.Thread(name: $0) }))
        return try await api.decode(Message.self, request)
    }
}
