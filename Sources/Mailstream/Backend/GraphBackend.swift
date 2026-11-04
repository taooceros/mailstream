import Foundation

/// A Microsoft account's mail over Microsoft Graph. Every request asks for immutable ids, so a
/// message keeps its id across moves; folder ids are mailbox ids.
nonisolated final class GraphBackend: MailBackend {
    private let api: API
    private let wellKnownFolders: WellKnownFolders

    init(api: API) {
        self.api = api
        wellKnownFolders = WellKnownFolders(api: api)
    }

    @concurrent func identity() async throws -> Address {
        let user = try await api.decode(User.self, Self.request(Self.url("me", [("$select", "displayName,mail,userPrincipalName")])))
        guard let email = [user.mail, user.userPrincipalName].compactMap({ $0 }).first(where: { !$0.isEmpty }) else {
            throw APIError(status: 200, message: "Microsoft Graph returned no address for this account")
        }
        return Address(name: user.displayName, email: email)
    }

    // MARK: Folders

    @concurrent func mailboxes(counts: Bool) async throws -> [Mailbox] {
        async let known = wellKnownFolders.ids()
        let top = try await pages(Folder.self, Self.url("me/mailFolders", Self.folderQuery), priority: .interactive)
        let wellKnown = try await known
        let hidden = Set(Self.systemFolders.compactMap { wellKnown[$0] })
        var roles: [String: MailboxRole] = [:]
        for (name, role) in Self.roleFolders {
            if let id = wellKnown[name] { roles[id] = role }
        }
        let folders = try await expand(top, path: nil, hidden: hidden)
        return folders.map { folder, name in
            Mailbox(account: api.account, id: folder.id, name: name, role: roles[folder.id] ?? .other,
                    unread: counts ? folder.unreadItemCount : nil, total: counts ? folder.totalItemCount : nil)
        }.sorted { a, b in
            if a.role.sortOrder != b.role.sortOrder { return a.role.sortOrder < b.role.sortOrder }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    private static let folderQuery = [("$top", "100"),
                                      ("$select", "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount")]
    private static let roleFolders: [(String, MailboxRole)] = [("inbox", .inbox), ("sentitems", .sent), ("drafts", .drafts),
                                                               ("deleteditems", .trash), ("junkemail", .junk), ("archive", .archive)]
    /// Exchange's own folders, left out of the sidebar with everything under them.
    private static let systemFolders = ["conversationhistory", "outbox", "syncissues"]

    /// Well-known folder name → folder id; names the mailbox lacks are absent. Found ids are kept
    /// for the backend's life (Graph's folder ids never change); missing names, which the mailbox
    /// may gain (Archive appears on first use), are asked again.
    private actor WellKnownFolders {
        private let api: API
        private var known: [String: String] = [:]

        init(api: API) {
            self.api = api
        }

        func ids() async throws -> [String: String] {
            let wanted = (GraphBackend.roleFolders.map(\.0) + GraphBackend.systemFolders).filter { known[$0] == nil }
            guard !wanted.isEmpty else { return known }
            let found = try await Self.lookUp(wanted, api: api)
            known.merge(found) { current, _ in current }
            return known
        }

        private static func lookUp(_ names: [String], api: API) async throws -> [String: String] {
            try await withThrowingTaskGroup(of: (String, String?).self) { group in
                for name in names {
                    group.addTask {
                        do {
                            let url = try GraphBackend.url("me/mailFolders/\(name)", [("$select", "id")])
                            let folder = try await api.decode(FolderID.self, GraphBackend.request(url))
                            return (name, folder.id)
                        } catch let error as APIError where error.status == 404 {
                            return (name, nil)
                        }
                    }
                }
                var ids: [String: String] = [:]
                for try await (name, id) in group {
                    if let id { ids[name] = id }
                }
                return ids
            }
        }
    }

    /// `folders` and all their descendants except `hidden` ones, each with its "Parent/Child" name.
    private func expand(_ folders: [Folder], path: String?, hidden: Set<String>) async throws -> [(Folder, String)] {
        try await withThrowingTaskGroup(of: [(Folder, String)].self) { group in
            var found: [(Folder, String)] = []
            for folder in folders where !hidden.contains(folder.id) {
                let name = path.map { "\($0)/\(folder.displayName ?? "")" } ?? folder.displayName ?? ""
                found.append((folder, name))
                guard (folder.childFolderCount ?? 0) > 0 else { continue }
                group.addTask {
                    let children = try await self.pages(Folder.self,
                                                        Self.url("me/mailFolders/\(folder.id.percentEncoded)/childFolders", Self.folderQuery),
                                                        priority: .interactive)
                    return try await self.expand(children, path: name, hidden: hidden)
                }
            }
            for try await nested in group { found += nested }
            return found
        }
    }

    // MARK: Listing

    private static let envelopeFields =
        "id,conversationId,internetMessageId,subject,from,toRecipients,receivedDateTime,isRead,flag,isDraft,hasAttachments"

    @concurrent func envelopes(mailbox: String, cursor: String?, pageSize: Int, priority: RequestPriority) async throws -> EnvelopePage {
        let url: URL
        if let cursor {
            guard let next = URL(string: cursor) else { throw APIError(status: 0, message: "Invalid page cursor") }
            url = next
        } else {
            url = try Self.url("me/mailFolders/\(mailbox.percentEncoded)/messages",
                               [("$top", String(pageSize)), ("$orderby", "receivedDateTime desc"), ("$select", Self.envelopeFields)])
        }
        let page = try await api.decode(Page<Message>.self, Self.request(url), priority: priority)
        return EnvelopePage(envelopes: page.value.map(Self.envelope), next: page.nextLink)
    }

    @concurrent func search(mailbox: String, query: String, pageSize: Int) async throws -> [Envelope] {
        let terms = query.replacingOccurrences(of: "\"", with: "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !terms.isEmpty else { return [] }
        let url = try Self.url("me/mailFolders/\(mailbox.percentEncoded)/messages",
                               [("$search", "\"\(terms)\""), ("$top", String(pageSize)), ("$select", Self.envelopeFields)])
        let page = try await api.decode(Page<Message>.self, Self.request(url))
        return page.value.map(Self.envelope).sorted { ($0.date ?? .distantPast) > ($1.date ?? .distantPast) }
    }

    private static func envelope(_ message: Message) -> Envelope {
        var flags = Set<String>()
        if message.isRead == true { flags.insert("seen") }
        if message.flag?.flagStatus == "flagged" { flags.insert("flagged") }
        if message.isDraft == true { flags.insert("draft") }
        let messageID = message.internetMessageId.map(normalizeMessageID)
        return Envelope(id: message.id, thread: message.conversationId,
                        messageID: messageID?.isEmpty == false ? messageID : nil, inReplyTo: [], flags: flags,
                        subject: message.subject ?? "", from: message.from?.address.map { [$0] } ?? [],
                        to: (message.toRecipients ?? []).compactMap(\.address),
                        date: message.receivedDateTime.flatMap(parseISODate), size: 0, hasAttachment: message.hasAttachments)
    }

    // MARK: Content

    @concurrent func content(id: String, mailbox: String, directory: URL, urlBase: String,
                             priority: RequestPriority) async throws -> MessageContent {
        let messagePath = "me/messages/\(id.percentEncoded)"
        let message = try await api.decode(Full.self,
                                           Self.request(Self.url(messagePath,
                                                                 [("$select", "body,ccRecipients,replyTo,internetMessageHeaders,hasAttachments")]),
                                                        prefer: Self.htmlBody),
                                           priority: priority)
        let body = message.body?.content ?? ""
        let isText = message.body?.contentType?.lowercased() == "text"
        var html: String? = isText ? nil : body
        var text: String? = isText ? body : nil

        var parts: [(Int, Part)] = []
        if message.hasAttachments == true || html?.range(of: "cid:", options: .caseInsensitive) != nil {
            let items = try await pages(AttachmentItem.self, Self.url("\(messagePath)/attachments"), priority: priority)
            parts = try await withThrowingTaskGroup(of: (Int, Part)?.self) { group in
                for (index, item) in items.enumerated() {
                    group.addTask {
                        let part = try await self.part(item, messagePath: messagePath, priority: priority)
                        return part.map { (index, $0) }
                    }
                }
                var found: [(Int, Part)] = []
                for try await part in group {
                    if let part { found.append(part) }
                }
                return found.sorted { $0.0 < $1.0 }
            }
        }

        var assembly = try MessageAssembly(directory: directory, urlBase: urlBase)
        for (index, part) in parts {
            switch part {
            case let .file(name, mime, contentID, inline, bytes):
                assembly.store(bytes, index: index, filename: name, mime: mime, contentID: contentID, inline: inline)
            case let .message(attached):
                // A plain message with an HTML one attached is shown as HTML, its own text included.
                if html == nil, attached.hasHTML { html = HTMLText.plainToHTML(text ?? "") }
                if let outer = html { html = outer + attached.html }
                if let outer = text { text = outer + "\n\n" + attached.text }
            }
        }

        let references = parseMessageIDList(message.internetMessageHeaders?
            .first { $0.name.caseInsensitiveCompare("References") == .orderedSame }?.value)
        return assembly.finish(html: html, text: text, cc: (message.ccRecipients ?? []).compactMap(\.address),
                               replyTo: (message.replyTo ?? []).compactMap(\.address), references: references)
    }

    /// A file's bytes or an attached message's rendering; nil for kinds not shown (links to cloud files).
    private func part(_ item: AttachmentItem, messagePath: String, priority: RequestPriority) async throws -> Part? {
        let path = "\(messagePath)/attachments/\(item.id.percentEncoded)"
        switch item.kind {
        case "#microsoft.graph.fileAttachment":
            let bytes: Data
            if let encoded = item.contentBytes, let decoded = Data(base64URLEncoded: encoded) {
                bytes = decoded
            } else {
                bytes = try await api.send(Self.request(Self.url("\(path)/$value")), priority: priority).0
            }
            let mime = item.contentType?.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
            return .file(name: item.name, mime: mime, contentID: item.contentId, inline: item.isInline ?? false, bytes: bytes)
        case "#microsoft.graph.itemAttachment":
            let expanded = try await api.decode(AttachedItem.self,
                                                Self.request(Self.url(path, [("$expand", "microsoft.graph.itemattachment/item")]),
                                                             prefer: Self.htmlBody),
                                                priority: priority)
            let attached = expanded.item
            let content = attached?.body?.content ?? ""
            let isText = attached?.body?.contentType?.lowercased() == "text"
            return .message(MessageAssembly.AttachedMessage(from: attached?.from?.address, subject: attached?.subject ?? "",
                                                            html: isText ? nil : content,
                                                            text: isText ? content : HTMLText.strip(content)))
        default:
            return nil
        }
    }

    // MARK: Changes

    @concurrent func setFlag(_ flag: MessageFlag, _ on: Bool, ids: [String], mailbox: String) async throws {
        let change: Change = switch flag {
        case .seen: .read(on)
        case .flagged: .flagged(on)
        }
        try await batch("PATCH", ids: ids, body: change)
    }

    @concurrent func move(ids: [String], from: String, to: String) async throws {
        try await batch("POST", ids: ids, path: "/move", body: .move(to))
    }

    @concurrent func delete(ids: [String], mailbox: String) async throws {
        try await batch("DELETE", ids: ids)
    }

    /// Sends `method` to `/me/messages/{id}<path>` for every id through `$batch`, twenty at a
    /// time. Sub-requests Graph throttled (429) or left unanswered are sent again, after the
    /// longest Retry-After; any other failure throws.
    private func batch(_ method: String, ids: [String], path: String = "", body: Change? = nil) async throws {
        var headers = ["Prefer": Self.immutableIDs]
        if body != nil { headers["Content-Type"] = "application/json" }
        let requests = ids.enumerated().map { index, id in
            BatchRequest(id: String(index + 1), method: method, url: "/me/messages/\(id.percentEncoded)\(path)",
                         headers: headers, body: body)
        }
        for chunk in requests.chunked(into: 20) {
            try await api.repeatingThrottled(chunk, service: "Microsoft Graph") { pending in
                let answer = try await api.decode(BatchAnswer.self,
                                                  Self.request(Self.url("$batch"), method: "POST", json: BatchPayload(requests: pending)))
                var retry = Set(pending.map(\.id))
                var retryAfter: Double?
                for item in answer.responses.sorted(by: { (Int($0.id) ?? 0) < (Int($1.id) ?? 0) }) where retry.contains(item.id) {
                    switch item.status {
                    case 200..<300:
                        retry.remove(item.id)
                    case 429:
                        if let seconds = item.retryAfter { retryAfter = max(retryAfter ?? 0, seconds) }
                    default:
                        throw APIError(status: item.status, message: item.body?.message ?? "Microsoft Graph answered \(item.status)")
                    }
                }
                return (pending.filter { retry.contains($0.id) }, retryAfter)
            }
        }
    }

    // MARK: Sending

    @concurrent func send(_ message: OutgoingMessage) async throws {
        let mime = try MIMEMessage.render(message, messageID: "\(UUID().uuidString.lowercased())@mailstream.local")
        var request = try Self.request(Self.url("me/sendMail"), method: "POST")
        request.setValue("text/plain", forHTTPHeaderField: "Content-Type")
        request.httpBody = mime.base64EncodedData()
        _ = try await api.send(request, idempotent: false)
    }

    // MARK: Requests

    private static let root = "https://graph.microsoft.com/v1.0/"
    private static let immutableIDs = "IdType=\"ImmutableId\""
    private static let htmlBody = "outlook.body-content-type=\"html\""

    /// `path` is relative to the v1.0 root with ids already percent-encoded; query values are
    /// percent-encoded here.
    private static func url(_ path: String, _ query: [(String, String)] = []) throws -> URL {
        var string = root + path
        if !query.isEmpty { string += "?" + formEncoded(query) }
        guard let url = URL(string: string) else { throw APIError(status: 0, message: "Invalid Microsoft Graph URL: \(path)") }
        return url
    }

    /// A Graph request that asks for immutable ids, plus any other `prefer` preference.
    private static func request(_ url: URL, method: String = "GET", json body: (any Encodable)? = nil,
                                prefer: String? = nil) -> URLRequest {
        API.request(url, method: method, json: body,
                    headers: ["Prefer": [immutableIDs, prefer].compactMap { $0 }.joined(separator: ", ")])
    }

    /// Pages through `first` and every `@odata.nextLink` after it.
    private func pages<Item: Decodable & Sendable>(_ type: Item.Type, _ first: URL, priority: RequestPriority) async throws -> [Item] {
        var items: [Item] = []
        var next: URL? = first
        while let url = next {
            let page = try await api.decode(Page<Item>.self, Self.request(url), priority: priority)
            items += page.value
            next = page.nextLink.flatMap(URL.init(string:))
        }
        return items
    }
}

// MARK: - Graph JSON

nonisolated extension GraphBackend {
    fileprivate struct Page<Item: Decodable & Sendable>: Decodable, Sendable {
        let value: [Item]
        let nextLink: String?

        enum CodingKeys: String, CodingKey {
            case value
            case nextLink = "@odata.nextLink"
        }
    }

    fileprivate struct User: Decodable, Sendable {
        let displayName: String?
        let mail: String?
        let userPrincipalName: String?
    }

    fileprivate struct FolderID: Decodable, Sendable {
        let id: String
    }

    fileprivate struct Folder: Decodable, Sendable {
        let id: String
        let displayName: String?
        let childFolderCount: Int?
        let unreadItemCount: Int?
        let totalItemCount: Int?
    }

    fileprivate struct Recipient: Decodable, Sendable {
        struct EmailAddress: Decodable, Sendable {
            let name: String?
            let address: String?
        }

        let emailAddress: EmailAddress?

        var address: Address? {
            guard let email = emailAddress?.address, !email.isEmpty else { return nil }
            return Address(name: emailAddress?.name, email: email)
        }
    }

    fileprivate struct Message: Decodable, Sendable {
        struct Flag: Decodable, Sendable { let flagStatus: String? }

        let id: String
        let conversationId: String?
        let internetMessageId: String?
        let subject: String?
        let from: Recipient?
        let toRecipients: [Recipient]?
        let receivedDateTime: String?
        let isRead: Bool?
        let flag: Flag?
        let isDraft: Bool?
        let hasAttachments: Bool?
    }

    fileprivate struct Body: Decodable, Sendable {
        let contentType: String?
        let content: String?
    }

    fileprivate struct Full: Decodable, Sendable {
        struct Header: Decodable, Sendable {
            let name: String
            let value: String
        }

        let body: Body?
        let ccRecipients: [Recipient]?
        let replyTo: [Recipient]?
        let internetMessageHeaders: [Header]?
        let hasAttachments: Bool?
    }

    fileprivate struct AttachmentItem: Decodable, Sendable {
        let kind: String?
        let id: String
        let name: String?
        let contentType: String?
        let contentId: String?
        let isInline: Bool?
        let contentBytes: String?

        enum CodingKeys: String, CodingKey {
            case kind = "@odata.type"
            case id, name, contentType, contentId, isInline, contentBytes
        }
    }

    fileprivate struct AttachedItem: Decodable, Sendable {
        struct Item: Decodable, Sendable {
            let subject: String?
            let from: Recipient?
            let body: Body?
        }

        let item: Item?
    }

    /// An attachment ready for `MessageAssembly`, or an attached message rendered inline.
    fileprivate enum Part: Sendable {
        case file(name: String?, mime: String, contentID: String?, inline: Bool, bytes: Data)
        case message(MessageAssembly.AttachedMessage)
    }

    /// The JSON body of one batched change.
    fileprivate enum Change: Encodable, Sendable {
        case read(Bool)
        case flagged(Bool)
        case move(String)

        func encode(to encoder: any Encoder) throws {
            switch self {
            case .read(let on): try ["isRead": on].encode(to: encoder)
            case .flagged(let on): try ["flag": ["flagStatus": on ? "flagged" : "notFlagged"]].encode(to: encoder)
            case .move(let destination): try ["destinationId": destination].encode(to: encoder)
            }
        }
    }

    fileprivate struct BatchPayload: Encodable {
        let requests: [BatchRequest]
    }

    fileprivate struct BatchRequest: Encodable {
        let id: String
        let method: String
        let url: String
        let headers: [String: String]
        let body: Change?
    }

    fileprivate struct BatchAnswer: Decodable, Sendable {
        struct Item: Decodable, Sendable {
            let id: String
            let status: Int
            let headers: [String: String]?
            /// A failed sub-request's error answer.
            let body: APIError.Body?

            enum CodingKeys: String, CodingKey { case id, status, headers, body }

            init(from decoder: any Decoder) throws {
                let container = try decoder.container(keyedBy: CodingKeys.self)
                id = try container.decode(String.self, forKey: .id)
                status = try container.decode(Int.self, forKey: .status)
                headers = try? container.decodeIfPresent([String: String].self, forKey: .headers)
                body = try? container.decodeIfPresent(APIError.Body.self, forKey: .body)
            }

            /// Seconds Graph asked to wait before repeating a throttled sub-request.
            var retryAfter: Double? {
                headers?.first { $0.key.caseInsensitiveCompare("Retry-After") == .orderedSame }.flatMap { Double($0.value) }
            }
        }

        let responses: [Item]
    }
}
