import Foundation

/// Gmail over its REST API. Mailbox ids are label ids plus the virtual `ALL` (All Mail: every
/// message but spam and trash). Envelopes' immutable fields are cached by message id and their
/// labels kept current through the mailbox history, so a refresh fetches metadata only for
/// messages it has not seen.
final class GmailBackend: MailBackend {
    static let allMail = "ALL"
    private let api: API
    private let cache: GmailEnvelopeCache

    init(api: API) {
        self.api = api
        cache = GmailEnvelopeCache(api: api)
    }

    private static let roles: [String: MailboxRole] = [
        "INBOX": .inbox, "SENT": .sent, "DRAFT": .drafts, "TRASH": .trash, "SPAM": .junk,
        "STARRED": .starred, "IMPORTANT": .important,
    ]

    func identity() async throws -> Address {
        struct Profile: Decodable, Sendable { let emailAddress: String }
        struct SendAsList: Decodable, Sendable {
            struct Entry: Decodable, Sendable {
                let sendAsEmail: String
                let displayName: String?
                let isPrimary: Bool?
            }
            let sendAs: [Entry]?
        }
        async let profile = api.get(Profile.self, Self.url("profile"), cost: 1)
        async let sendAs = api.get(SendAsList.self, Self.url("settings/sendAs"), cost: 1)
        let email = try await profile.emailAddress
        let entries = try await sendAs.sendAs ?? []
        let primary = entries.first { $0.isPrimary == true }
            ?? entries.first { $0.sendAsEmail.caseInsensitiveCompare(email) == .orderedSame }
        let name = primary?.displayName?.trimmingCharacters(in: .whitespaces) ?? ""
        return Address(name: name.isEmpty ? nil : name, email: email)
    }

    func mailboxes(counts: Bool) async throws -> [Mailbox] {
        struct LabelList: Decodable, Sendable {
            struct Label: Decodable, Sendable {
                let id: String
                let name: String
                let type: String?
            }
            let labels: [Label]?
        }
        struct Tally: Decodable, Sendable {
            let messagesTotal: Int?
            let messagesUnread: Int?
        }
        let account = api.account
        let labels = try await api.get(LabelList.self, Self.url("labels", [("fields", "labels(id,name,type)")]), cost: 1).labels ?? []
        var boxes: [Mailbox] = labels.compactMap { label in
            if let role = Self.roles[label.id] {
                return Mailbox(account: account, id: label.id, name: role.title, role: role, unread: nil, total: nil)
            }
            guard label.type == "user" else { return nil }
            return Mailbox(account: account, id: label.id, name: label.name, role: .other, unread: nil, total: nil)
        }
        if counts {
            let api = self.api
            let tallies = try await withThrowingTaskGroup(of: (String, Tally).self) { group in
                for box in boxes {
                    group.addTask {
                        let url = Self.url("labels/\(Self.escape(box.id))", [("fields", "messagesTotal,messagesUnread")])
                        return (box.id, try await api.get(Tally.self, url, priority: .background, cost: 1))
                    }
                }
                var tallies: [String: Tally] = [:]
                for try await (id, tally) in group { tallies[id] = tally }
                return tallies
            }
            for index in boxes.indices {
                boxes[index].unread = tallies[boxes[index].id]?.messagesUnread
                boxes[index].total = tallies[boxes[index].id]?.messagesTotal
            }
        }
        boxes.append(Mailbox(account: account, id: Self.allMail, name: MailboxRole.all.title, role: .all, unread: nil, total: nil))
        return boxes.sorted { a, b in
            if a.role.sortOrder != b.role.sortOrder { return a.role.sortOrder < b.role.sortOrder }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    func envelopes(mailbox: String, cursor: String?, pageSize: Int, priority: RequestPriority) async throws -> EnvelopePage {
        var query = Self.scope(mailbox) + [("maxResults", "\(min(max(pageSize, 1), 500))"),
                                           ("fields", "messages(id),nextPageToken")]
        if let cursor { query.append(("pageToken", cursor)) }
        let (envelopes, next) = try await listing(Self.url("messages", query), priority: priority)
        return EnvelopePage(envelopes: envelopes, next: next)
    }

    func search(mailbox: String, query: String, pageSize: Int) async throws -> [Envelope] {
        let parameters = Self.scope(mailbox) + [("q", query), ("maxResults", "\(min(max(pageSize, 1), 500))"),
                                                ("fields", "messages(id),nextPageToken")]
        return try await listing(Self.url("messages", parameters), priority: .interactive).envelopes
    }

    /// Lists, brings cached labels up to date meanwhile, then fills in the envelopes.
    private func listing(_ url: URL, priority: RequestPriority) async throws -> (envelopes: [Envelope], next: String?) {
        struct MessageList: Decodable, Sendable {
            struct Item: Decodable, Sendable { let id: String }
            let messages: [Item]?
            let nextPageToken: String?
        }
        async let synchronized: Void = cache.synchronize(priority: priority)
        let page = try await api.get(MessageList.self, url, priority: priority, cost: 5)
        try await synchronized
        let envelopes = try await cache.envelopes(page.messages?.map(\.id) ?? [], priority: priority)
        return (envelopes, page.nextPageToken)
    }

    func content(id: String, mailbox: String, directory: URL, urlBase: String,
                 priority: RequestPriority) async throws -> MessageContent {
        let message = try await api.get(GmailMessage.self, Self.url("messages/\(Self.escape(id))", [("format", "full")]),
                                        priority: priority, cost: 5)
        guard let payload = message.payload else {
            throw APIError(status: 200, message: "Gmail returned message \(id) without content")
        }
        let fetched = try await attachmentData(in: payload, message: id, priority: priority)
        var renderer = try GmailContentRenderer(assembly: MessageAssembly(directory: directory, urlBase: urlBase),
                                                fetched: fetched)
        let body = renderer.render(payload, related: false)
        return renderer.assembly.finish(html: body?.primary == true ? body?.html : nil, text: body?.text,
                                        cc: AddressList.parse(payload.header("Cc") ?? ""),
                                        replyTo: AddressList.parse(payload.header("Reply-To") ?? ""),
                                        references: Self.messageIDs(payload.header("References")))
    }

    /// Bodies Gmail left out of the message (`body.attachmentId`), by attachment id. A
    /// forwarded message whose parts Gmail spelled out is rendered from those instead.
    private func attachmentData(in payload: GmailPart, message: String,
                                priority: RequestPriority) async throws -> [String: Data] {
        var wanted: [String] = []
        func collect(_ part: GmailPart) {
            if let parts = part.parts, !parts.isEmpty {
                for child in parts { collect(child) }
            } else if let id = part.body?.attachmentId {
                wanted.append(id)
            }
        }
        collect(payload)
        guard !wanted.isEmpty else { return [:] }
        let api = self.api
        return try await withThrowingTaskGroup(of: (String, Data).self) { group in
            for attachment in wanted {
                group.addTask {
                    let url = Self.url("messages/\(Self.escape(message))/attachments/\(Self.escape(attachment))")
                    let body = try await api.get(GmailBody.self, url, priority: priority, cost: 5)
                    return (attachment, body.data.flatMap(Self.base64URL) ?? Data())
                }
            }
            var data: [String: Data] = [:]
            for try await (id, bytes) in group { data[id] = bytes }
            return data
        }
    }

    func setFlag(_ flag: MessageFlag, _ on: Bool, ids: [String], mailbox: String) async throws {
        let label = flag == .seen ? "UNREAD" : "STARRED"
        // Seen is the absence of UNREAD.
        let add = (flag == .seen) != on
        try await modify(ids, add: add ? [label] : [], remove: add ? [] : [label])
    }

    /// Labels Gmail keeps to itself: a move never takes them off.
    private static let fixed: Set<String> = [allMail, "SENT", "DRAFT"]

    func move(ids: [String], from: String, to: String) async throws {
        guard from != to, !ids.isEmpty else { return }
        if to == "TRASH" {
            try await each(ids, "trash")
            await cache.relabel(ids, add: ["TRASH"], remove: [])
            return
        }
        if from == "TRASH" {
            // Untrash restores the labels the message had; then file it where it was sent.
            try await each(ids, "untrash")
            await cache.relabel(ids, add: [], remove: ["TRASH"])
            if to != Self.allMail { try await modify(ids, add: [to], remove: to == "SPAM" ? ["INBOX"] : []) }
            return
        }
        // Leaving STARRED or IMPORTANT unstars or unmarks: the message leaves that mailbox.
        var remove: Set<String> = Self.fixed.contains(from) ? [] : [from]
        if to == "SPAM" { remove.insert("INBOX") }
        try await modify(ids, add: to == Self.allMail ? [] : [to], remove: remove.sorted())
    }

    func delete(ids: [String], mailbox: String) async throws {
        struct Body: Encodable { let ids: [String] }
        for chunk in Self.chunks(ids, 1000) {
            let request = API.request(Self.url("messages/batchDelete"), method: "POST", json: Body(ids: chunk))
            _ = try await api.send(request, idempotent: true, cost: 50)
        }
        await cache.forget(ids)
    }

    /// Uploads through the media endpoint, which takes messages up to 35 MB with their thread.
    func send(_ message: OutgoingMessage) async throws {
        struct Metadata: Encodable { let threadId: String? }
        let raw = try MIMEMessage.render(message, messageID: "\(UUID().uuidString.lowercased())@mailstream.local")
        let boundary = "ms_upload_" + UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        var body = Data("--\(boundary)\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".utf8)
        body.append(try JSONEncoder().encode(Metadata(threadId: message.thread)))
        body.append(Data("\r\n--\(boundary)\r\nContent-Type: message/rfc822\r\n\r\n".utf8))
        body.append(raw)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        var request = API.request(URL(string: "https://gmail.googleapis.com/upload/gmail/v1/users/me/messages/send?uploadType=multipart")!,
                                  method: "POST", headers: ["Content-Type": "multipart/related; boundary=\(boundary)"])
        request.httpBody = body
        _ = try await api.send(request, idempotent: false, cost: 100)
    }

    private func modify(_ ids: [String], add: [String], remove: [String]) async throws {
        struct Body: Encodable {
            let ids: [String]
            let addLabelIds: [String]
            let removeLabelIds: [String]
        }
        guard !ids.isEmpty, !(add.isEmpty && remove.isEmpty) else { return }
        for chunk in Self.chunks(ids, 1000) {
            let request = API.request(Self.url("messages/batchModify"), method: "POST",
                                      json: Body(ids: chunk, addLabelIds: add, removeLabelIds: remove))
            _ = try await api.send(request, idempotent: true, cost: 50)
        }
        await cache.relabel(ids, add: add, remove: remove)
    }

    /// `messages/{id}/<action>` for every id, in parallel.
    private func each(_ ids: [String], _ action: String) async throws {
        let api = self.api
        try await withThrowingTaskGroup(of: Void.self) { group in
            for id in ids {
                group.addTask {
                    let request = API.request(Self.url("messages/\(Self.escape(id))/\(action)"), method: "POST")
                    _ = try await api.send(request, idempotent: true, cost: 5)
                }
            }
            try await group.waitForAll()
        }
    }

    // MARK: Helpers

    private static let base = "https://gmail.googleapis.com/gmail/v1/users/me/"

    /// Query parameters naming `mailbox`; All Mail is the unfiltered listing.
    private static func scope(_ mailbox: String) -> [(String, String)] {
        switch mailbox {
        case allMail: []
        case "TRASH", "SPAM": [("labelIds", mailbox), ("includeSpamTrash", "true")]
        default: [("labelIds", mailbox)]
        }
    }

    fileprivate static func url(_ path: String, _ query: [(String, String)] = []) -> URL {
        var string = base + path
        if !query.isEmpty { string += "?" + encode(query) }
        return URL(string: string)!
    }

    fileprivate static func encode(_ query: [(String, String)]) -> String {
        query.map { "\(escape($0.0))=\(escape($0.1))" }.joined(separator: "&")
    }

    private static let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    fileprivate static func escape(_ text: String) -> String {
        text.addingPercentEncoding(withAllowedCharacters: unreserved) ?? text
    }

    fileprivate static func chunks<T>(_ items: [T], _ size: Int) -> [[T]] {
        stride(from: 0, to: items.count, by: size).map { Array(items[$0..<min($0 + size, items.count)]) }
    }

    fileprivate static func base64URL(_ text: String) -> Data? {
        var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        return Data(base64Encoded: base64, options: .ignoreUnknownCharacters)
    }

    /// Normalized ids in a Message-ID list header (`In-Reply-To`, `References`).
    fileprivate static func messageIDs(_ header: String?) -> [String] {
        guard let header, !header.isEmpty else { return [] }
        let bracketed = header.matches(of: /<([^<>]+)>/).map { String($0.1) }
        let raw = bracketed.isEmpty ? header.split(whereSeparator: \.isWhitespace).map(String.init) : bracketed
        return raw.map(normalizeMessageID).filter { !$0.isEmpty }
    }
}

// MARK: - Gmail resources

private struct GmailMessage: Decodable, Sendable {
    let id: String
    let threadId: String?
    let labelIds: [String]?
    let historyId: String?
    let internalDate: String?
    let sizeEstimate: Int?
    let payload: GmailPart?
}

private struct GmailPart: Decodable, Sendable {
    struct Header: Decodable, Sendable {
        let name: String
        let value: String
    }
    let mimeType: String?
    let filename: String?
    let headers: [Header]?
    let body: GmailBody?
    let parts: [GmailPart]?

    func header(_ name: String) -> String? {
        headers?.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }?.value
    }

    /// Whether some part below this one is a named file.
    var hasFile: Bool {
        (parts ?? []).contains { !($0.filename ?? "").isEmpty || $0.hasFile }
    }
}

private struct GmailBody: Decodable, Sendable {
    let attachmentId: String?
    let data: String?
}

// MARK: - Envelope cache

/// Envelopes by message id. Immutable fields come from one metadata fetch; labels follow the
/// mailbox history from the last history id seen, and a history too old to answer drops the
/// cache so the next listing fetches metadata afresh.
private actor GmailEnvelopeCache {
    private struct Entry {
        let envelope: Envelope
        var labels: Set<String>

        var current: Envelope {
            var envelope = envelope
            envelope.flags = []
            if !labels.contains("UNREAD") { envelope.flags.insert("seen") }
            if labels.contains("STARRED") { envelope.flags.insert("flagged") }
            if labels.contains("DRAFT") { envelope.flags.insert("draft") }
            return envelope
        }
    }

    private let api: API
    private var entries: [String: Entry] = [:]
    /// Metadata fetches in flight, by the ids they fetch.
    private var loading: [String: Task<[String: Entry], any Error>] = [:]
    private var historyID: UInt64?
    /// Ids each change to `entries` touched (nil: all), by generation, so metadata fetched
    /// while a change landed is not cached with labels that change missed.
    private var generation = 0
    private var touched: [(generation: Int, ids: Set<String>?)] = []
    /// Callers that asked for a history pull; the running pull answers those numbered up to `covers`.
    private var tickets = 0
    private var running: (covers: Int, task: Task<Void, any Error>)?

    init(api: API) {
        self.api = api
    }

    /// Applies every label change and deletion since the last pull. A pull already running
    /// answers callers that arrived before it started; later ones get a pull of their own.
    func synchronize(priority: RequestPriority) async throws {
        tickets += 1
        let ticket = tickets
        while let current = running {
            if current.covers >= ticket { return try await current.task.value }
            _ = try? await current.task.value
        }
        guard historyID != nil else { return }
        let task = Task { try await self.pull(priority: priority) }
        running = (tickets, task)
        try await task.value
    }

    private func pull(priority: RequestPriority) async throws {
        struct Page: Decodable, Sendable {
            struct Change: Decodable, Sendable {
                struct Message: Decodable, Sendable { let id: String }
                let message: Message
                let labelIds: [String]?
            }
            struct Record: Decodable, Sendable {
                let labelsAdded: [Change]?
                let labelsRemoved: [Change]?
                let messagesDeleted: [Change]?
            }
            let history: [Record]?
            let nextPageToken: String?
            let historyId: String?
        }
        defer { running = nil }
        guard let start = historyID else { return }
        var records: [Page.Record] = []
        var latest = start
        var pageToken: String?
        for _ in 0..<10 {
            var query = [("startHistoryId", String(start)), ("historyTypes", "labelAdded"), ("historyTypes", "labelRemoved"),
                         ("historyTypes", "messageDeleted"), ("maxResults", "500"),
                         ("fields", "history(labelsAdded(message(id),labelIds),labelsRemoved(message(id),labelIds),"
                             + "messagesDeleted(message(id))),nextPageToken,historyId")]
            if let pageToken { query.append(("pageToken", pageToken)) }
            let page: Page
            do {
                page = try await api.get(Page.self, GmailBackend.url("history", query), priority: priority, cost: 2)
            } catch let error as APIError where error.status == 404 {
                reset()
                return
            }
            records += page.history ?? []
            if let id = page.historyId.flatMap(UInt64.init) { latest = max(latest, id) }
            guard let next = page.nextPageToken else {
                var ids = Set<String>()
                for record in records {
                    for change in record.labelsAdded ?? [] {
                        ids.insert(change.message.id)
                        entries[change.message.id]?.labels.formUnion(change.labelIds ?? [])
                    }
                    for change in record.labelsRemoved ?? [] {
                        ids.insert(change.message.id)
                        entries[change.message.id]?.labels.subtract(change.labelIds ?? [])
                    }
                    for change in record.messagesDeleted ?? [] {
                        ids.insert(change.message.id)
                        entries[change.message.id] = nil
                    }
                }
                historyID = latest
                advance(ids)
                return
            }
            pageToken = next
        }
        // More change than a refetch costs.
        reset()
    }

    /// Envelopes of `ids` in that order, fetching metadata for the ones not cached; messages
    /// that no longer exist are left out. Ids another caller is already fetching share that
    /// fetch rather than costing quota twice.
    func envelopes(_ ids: [String], priority: RequestPriority) async throws -> [Envelope] {
        let unknown = ids.filter { entries[$0] == nil }
        let missing = unknown.filter { loading[$0] == nil }
        if !missing.isEmpty {
            let task = Task { try await self.fetch(missing, priority: priority) }
            for id in missing { loading[id] = task }
        }
        var fetched: [String: Entry] = [:]
        for task in Set(unknown.compactMap { loading[$0] }) {
            fetched.merge(try await task.value) { a, _ in a }
        }
        return ids.compactMap { (entries[$0] ?? fetched[$0])?.current }
    }

    private func fetch(_ ids: [String], priority: RequestPriority) async throws -> [String: Entry] {
        defer { for id in ids { loading[id] = nil } }
        // The baseline comes first: history from it replays any change racing the fetch.
        if historyID == nil {
            let baseline = try await GmailBackend.historyID(api: api, priority: priority)
            if historyID == nil { historyID = baseline }
        }
        let since = generation
        let messages = try await GmailBackend.metadata(ids, query: GmailBackend.metadataQuery, api: api,
                                                       priority: priority)
        // Metadata shows only the top MIME type; a multipart/mixed message has attachments
        // only if a part carries a file name.
        let mixed = messages.filter { $0.payload?.mimeType?.lowercased() == "multipart/mixed" }.map(\.id)
        let withFiles = mixed.isEmpty ? [] : Set(try await GmailBackend.metadata(
            mixed, query: GmailBackend.partsQuery, api: api, priority: priority)
            .filter { $0.payload?.hasFile == true }.map(\.id))
        let stale = changed(since: since)
        var fetched: [String: Entry] = [:]
        for message in messages {
            let entry = Self.entry(message, hasAttachment: withFiles.contains(message.id))
            fetched[message.id] = entry
            guard let stale, !stale.contains(message.id), entries[message.id] == nil else { continue }
            entries[message.id] = entry
        }
        return fetched
    }

    /// Our own label change, ahead of the history reporting it.
    func relabel(_ ids: [String], add: [String], remove: [String]) {
        for id in ids {
            entries[id]?.labels.formUnion(add)
            entries[id]?.labels.subtract(remove)
        }
        advance(Set(ids))
    }

    func forget(_ ids: [String]) {
        for id in ids { entries[id] = nil }
        advance(Set(ids))
    }

    private func reset() {
        entries.removeAll()
        historyID = nil
        advance(nil)
    }

    private func advance(_ ids: Set<String>?) {
        generation += 1
        touched.append((generation, ids))
        if touched.count > 32 { touched.removeFirst(touched.count - 32) }
    }

    /// Ids changed after generation `since`; nil when that is everything or no longer known.
    private func changed(since: Int) -> Set<String>? {
        guard since < generation else { return [] }
        guard let first = touched.first, first.generation <= since + 1 else { return nil }
        var ids = Set<String>()
        for change in touched where change.generation > since {
            guard let changed = change.ids else { return nil }
            ids.formUnion(changed)
        }
        return ids
    }

    private static func entry(_ message: GmailMessage, hasAttachment: Bool) -> Entry {
        let payload = message.payload
        let inReplyTo = GmailBackend.messageIDs(payload?.header("In-Reply-To"))
        let messageID = payload?.header("Message-ID").map(normalizeMessageID)
        let envelope = Envelope(
            id: message.id,
            thread: message.threadId,
            messageID: messageID?.isEmpty == false ? messageID : nil,
            inReplyTo: inReplyTo.isEmpty ? Array(GmailBackend.messageIDs(payload?.header("References")).suffix(1)) : inReplyTo,
            flags: [],
            subject: HeaderText.decode(payload?.header("Subject") ?? ""),
            from: AddressList.parse(payload?.header("From") ?? ""),
            to: AddressList.parse(payload?.header("To") ?? ""),
            date: message.internalDate.flatMap(Double.init).map { Date(timeIntervalSince1970: $0 / 1000) },
            size: message.sizeEstimate ?? 0,
            hasAttachment: hasAttachment)
        return Entry(envelope: envelope, labels: Set(message.labelIds ?? []))
    }
}

// MARK: - Batch metadata

extension GmailBackend {
    fileprivate static let metadataQuery: String = {
        let headers = ["From", "To", "Subject", "Message-ID", "In-Reply-To", "References"]
        var query: [(String, String)] = [("format", "metadata")]
        query += headers.map { ("metadataHeaders", $0) }
        query.append(("fields", "id,threadId,labelIds,historyId,internalDate,sizeEstimate,payload(mimeType,headers)"))
        return encode(query)
    }()

    /// The part tree's file names, two levels deep, without bodies or headers.
    fileprivate static let partsQuery = encode([("format", "full"),
                                               ("fields", "id,payload(parts(filename,parts(filename)))")])

    /// The mailbox's current history id.
    fileprivate static func historyID(api: API, priority: RequestPriority) async throws -> UInt64? {
        struct Profile: Decodable { let historyId: String? }
        return try await api.get(Profile.self, url("profile", [("fields", "historyId")]), priority: priority, cost: 1)
            .historyId.flatMap(UInt64.init)
    }

    /// Metadata of `ids` through Gmail's batch endpoint, 50 per request; messages that no
    /// longer exist are left out. Sub-requests Gmail throttles are repeated after a pause.
    fileprivate static func metadata(_ ids: [String], query: String, api: API,
                                     priority: RequestPriority) async throws -> [GmailMessage] {
        try await withThrowingTaskGroup(of: [GmailMessage].self) { group in
            for chunk in chunks(ids, 50) {
                group.addTask { try await Self.batch(chunk, query: query, api: api, priority: priority) }
            }
            var messages: [GmailMessage] = []
            for try await part in group { messages += part }
            return messages
        }
    }

    private static func batch(_ ids: [String], query: String, api: API, priority: RequestPriority) async throws -> [GmailMessage] {
        var pending = ids
        var messages: [GmailMessage] = []
        var attempt = 0
        while !pending.isEmpty {
            attempt += 1
            let boundary = "batch_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
            var body = ""
            for (index, id) in pending.enumerated() {
                body += "--\(boundary)\r\nContent-Type: application/http\r\nContent-ID: <m\(index)>\r\n\r\n"
                    + "GET /gmail/v1/users/me/messages/\(escape(id))?\(query)\r\n\r\n"
            }
            body += "--\(boundary)--\r\n"
            var request = API.request(URL(string: "https://gmail.googleapis.com/batch/gmail/v1")!, method: "POST",
                                      headers: ["Content-Type": "multipart/mixed; boundary=\(boundary)"])
            request.httpBody = Data(body.utf8)
            let (data, response) = try await api.send(request, priority: priority, idempotent: true, cost: 5 * pending.count)
            guard let responseBoundary = HeaderText.parameter("boundary", in: response.value(forHTTPHeaderField: "Content-Type")) else {
                throw APIError(status: response.statusCode, message: "Gmail answered a batch request without parts")
            }
            var retry: [String] = []
            var answered = Set<Int>()
            for part in batchParts(data, boundary: responseBoundary) {
                guard let index = part.index, pending.indices.contains(index) else { continue }
                answered.insert(index)
                switch part.status {
                case 200:
                    do {
                        messages.append(try JSONDecoder().decode(GmailMessage.self, from: part.body))
                    } catch {
                        throw APIError(status: 200, message: "Unexpected answer from gmail.googleapis.com: \(error)")
                    }
                case 404:
                    continue
                case 429, 500..<600:
                    retry.append(pending[index])
                case 403 where API.isRateLimit(part.body):
                    retry.append(pending[index])
                default:
                    throw APIError(status: part.status, message: errorMessage(part.body, status: part.status))
                }
            }
            retry += pending.indices.filter { !answered.contains($0) }.map { pending[$0] }
            guard !retry.isEmpty else { break }
            guard attempt < 6 else { throw APIError(status: 429, message: "Gmail is limiting requests; try again shortly") }
            try await Task.sleep(for: .milliseconds(min(500 << (attempt - 1), 8000)))
            pending = retry
        }
        return messages
    }

    private struct BatchPart {
        let index: Int?
        let status: Int
        let body: Data
    }

    /// Splits a multipart/mixed batch answer into its HTTP responses; `index` comes from the
    /// `Content-ID` (`<response-m3>`) of the sub-request it answers.
    private static func batchParts(_ data: Data, boundary: String) -> [BatchPart] {
        let text = String(decoding: data, as: UTF8.self).replacingOccurrences(of: "\r\n", with: "\n")
        var parts: [BatchPart] = []
        for chunk in text.components(separatedBy: "--" + boundary).dropFirst() {
            if chunk.hasPrefix("--") { break }
            guard let split = chunk.range(of: "\n\n") else { continue }
            let headers = chunk[..<split.lowerBound]
            let response = chunk[split.upperBound...]
            let index = headers.firstMatch(of: /(?i)content-id:\s*<[^>]*?(\d+)>/).flatMap { Int($0.1) }
            let statusLine = response.prefix { $0 != "\n" }
            guard let status = statusLine.split(separator: " ").dropFirst().first.flatMap({ Int($0) }) else { continue }
            let body = response.range(of: "\n\n").map { response[$0.upperBound...] } ?? ""
            parts.append(BatchPart(index: index, status: status, body: Data(body.utf8)))
        }
        return parts
    }

    private static func errorMessage(_ data: Data, status: Int) -> String {
        struct Body: Decodable {
            struct Inner: Decodable { let message: String? }
            let error: Inner?
        }
        let message = (try? JSONDecoder().decode(Body.self, from: data))?.error?.message ?? ""
        return message.isEmpty ? "Gmail answered \(status)" : message
    }
}

// MARK: - Content

/// Walks a `format=full` payload: picks the HTML and plain bodies, stores every other part
/// through `MessageAssembly` in part order, and renders forwarded messages inline.
private struct GmailContentRenderer {
    var assembly: MessageAssembly
    let fetched: [String: Data]
    private var nextIndex = 0

    init(assembly: MessageAssembly, fetched: [String: Data]) {
        self.assembly = assembly
        self.fetched = fetched
    }

    /// What a part shows. `primary` when `html` is a real HTML body; other HTML (an inline
    /// image, a forwarded message) only joins when a sibling has one.
    struct Segment {
        var html: String?
        var text: String?
        var primary: Bool
    }

    mutating func render(_ part: GmailPart, related: Bool) -> Segment? {
        let mime = (part.mimeType ?? "").lowercased()
        if mime.hasPrefix("multipart/") {
            let children = part.parts ?? []
            if mime == "multipart/alternative" {
                let segments = renderAll(children, related: false)
                let html = segments.last(where: { $0.primary })?.html
                let text = segments.last(where: { $0.text != nil })?.text
                return html == nil && text == nil ? nil : Segment(html: html, text: text, primary: html != nil)
            }
            let isRelated = mime == "multipart/related"
            return Self.combine(renderAll(children, related: isRelated))
        }
        if mime == "message/rfc822" { return attachedMessage(part) }

        let disposition = (part.header("Content-Disposition") ?? "").lowercased()
        let filename = part.filename ?? ""
        if (mime == "text/plain" || mime == "text/html"), !disposition.hasPrefix("attachment"), filename.isEmpty {
            let text = HeaderText.string(data(part) ?? Data(), charset: HeaderText.parameter("charset", in: part.header("Content-Type")))
            return mime == "text/html" ? Segment(html: text, text: nil, primary: true) : Segment(html: nil, text: text, primary: false)
        }
        guard let bytes = data(part) else { return nil }
        let inline = disposition.hasPrefix("inline")
        // Images placed inline between bodies (Apple Mail) rather than referenced from the HTML.
        let placed = inline && !related && mime.hasPrefix("image/")
        let index = nextIndex
        nextIndex += 1
        let contentID = part.header("Content-ID") ?? (placed ? "ms-part-\(index)" : nil)
        let attachment = assembly.store(bytes, index: index, filename: filename.isEmpty ? nil : filename, mime: mime,
                                        contentID: contentID, inline: inline)
        guard placed, let cid = attachment.contentID, HTMLText.escape(cid) == cid else { return nil }
        return Segment(html: "<p><img src=\"cid:\(cid)\" style=\"max-width:100%\"></p>", text: nil, primary: false)
    }

    private mutating func renderAll(_ parts: [GmailPart], related: Bool) -> [Segment] {
        var segments: [Segment] = []
        for part in parts {
            if let segment = render(part, related: related) { segments.append(segment) }
        }
        return segments
    }

    /// A forwarded message: rendered below a headline when Gmail spelled out its parts, else
    /// offered as an `.eml` attachment.
    private mutating func attachedMessage(_ part: GmailPart) -> Segment? {
        let nested = part.parts ?? []
        guard !nested.isEmpty else {
            guard let bytes = data(part) else { return nil }
            let filename = part.filename ?? ""
            assembly.store(bytes, index: nextIndex, filename: filename.isEmpty ? "message.eml" : filename,
                           mime: "message/rfc822", contentID: nil, inline: false)
            nextIndex += 1
            return nil
        }
        let source = ([part] + nested).first { $0.header("From") != nil }
        let from = AddressList.parse(source?.header("From") ?? "").first?.rfc5322 ?? "unknown sender"
        let subject = HeaderText.decode(source?.header("Subject") ?? "")
        let headline = subject.isEmpty ? "Attached message from \(from)" : "Attached message from \(from): \(subject)"
        let inner = nested.count == 1 ? render(nested[0], related: false)
            : Self.combine(renderAll(nested, related: false))
        let innerText = inner?.text ?? (inner?.primary == true ? inner?.html.map(HTMLText.strip) : nil) ?? ""
        let innerHTML = inner?.primary == true ? (inner?.html ?? "") : HTMLText.plainToHTML(innerText)
        return Segment(html: "<div class=\"ms-attached-message\"><div class=\"ms-attached-headline\">\(HTMLText.escape(headline))</div>"
                           + innerHTML + "</div>",
                       text: "---------- \(headline) ----------\n" + innerText,
                       primary: false)
    }

    /// Siblings shown one after another (multipart/mixed, related and the rest).
    private static func combine(_ segments: [Segment]) -> Segment? {
        guard segments.count > 1 else { return segments.first }
        let primary = segments.contains { $0.primary }
        let html = primary ? segments.map { $0.html ?? HTMLText.plainToHTML($0.text ?? "") }.joined() : nil
        let hasText = segments.contains { $0.text != nil }
        let text = hasText
            ? segments.compactMap { $0.text ?? ($0.primary ? $0.html.map(HTMLText.strip) : nil) }.joined(separator: "\n\n")
            : nil
        return Segment(html: html, text: text, primary: primary)
    }

    private func data(_ part: GmailPart) -> Data? {
        if let data = part.body?.data { return GmailBackend.base64URL(data) }
        if let id = part.body?.attachmentId { return fetched[id] }
        return nil
    }
}
