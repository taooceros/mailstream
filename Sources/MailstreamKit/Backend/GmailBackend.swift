import Foundation

/// Gmail over its REST API. Mailbox ids are label ids plus the virtual `ALL` (All Mail: every
/// message but spam and trash). Envelopes' immutable fields are cached by message id on disk and
/// their labels kept current through the mailbox history, so a refresh or a relaunch fetches
/// metadata only for messages it has never seen.
nonisolated final class GmailBackend: MailBackend {
    static let allMail = "ALL"
    private let api: API
    private let cache: GmailEnvelopeCache

    init(api: API) {
        self.api = api
        cache = GmailEnvelopeCache(api: api, file: Self.cacheFile(api.account))
    }

    private static func cacheFile(_ account: String) -> URL {
        Platform.caches.appendingPathComponent("dev.mailstream.Mailstream/gmail-envelopes-v1/\(MailCache.hash(account)).json")
    }

    /// Drops the signed-out `account`'s envelope cache, off the caller's actor.
    @concurrent static func discardCache(account: String) async {
        try? FileManager.default.removeItem(at: cacheFile(account))
    }

    private static let roles: [String: MailboxRole] = [
        "INBOX": .inbox, "SENT": .sent, "DRAFT": .drafts, "TRASH": .trash, "SPAM": .junk,
        "STARRED": .starred, "IMPORTANT": .important,
    ]

    @concurrent func identity() async throws -> Address {
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
        return Address(name: name.nilIfEmpty, email: email)
    }

    @concurrent func mailboxes(counts: Bool) async throws -> [Mailbox] {
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
            let tallies = try await Self.batchGet(Tally.self, "labels", ids: boxes.map(\.id),
                                                  query: formEncoded([("fields", "messagesTotal,messagesUnread")]),
                                                  unitCost: 1, api: api, priority: .background)
            for index in boxes.indices {
                boxes[index].unread = tallies[boxes[index].id]?.messagesUnread
                boxes[index].total = tallies[boxes[index].id]?.messagesTotal
            }
        }
        boxes.append(Mailbox(account: account, id: Self.allMail, name: MailboxRole.all.title, role: .all, unread: nil, total: nil))
        return boxes
    }

    @concurrent func envelopes(mailbox: String, cursor: String?, pageSize: Int, priority: RequestPriority) async throws -> EnvelopePage {
        var query = Self.scope(mailbox) + [("maxResults", API.pageSize(pageSize, max: 500)),
                                           ("fields", Self.listFields)]
        if let cursor { query.append(("pageToken", cursor)) }
        let (ids, next) = try await listing(Self.url("messages", query), priority: priority)
        return EnvelopePage(envelopes: try await cache.envelopes(ids, priority: priority), next: next)
    }

    /// No label filter: Gmail then searches everything but spam and trash, as its web search does.
    @concurrent func search(query: String, pageSize: Int) async throws -> [SearchHit] {
        let parameters = [("q", query), ("maxResults", API.pageSize(pageSize, max: 500)), ("fields", Self.listFields)]
        let (ids, _) = try await listing(Self.url("messages", parameters), priority: .interactive)
        return try await cache.hits(ids, priority: .interactive)
    }

    /// `resultSizeEstimate` is always present, so a list with no matches still answers `{…}`;
    /// with only `messages` and `nextPageToken` Gmail sends an empty body, which is not JSON.
    private static let listFields = "messages(id),nextPageToken,resultSizeEstimate"

    /// Lists message ids, bringing cached labels up to date meanwhile.
    private func listing(_ url: URL, priority: RequestPriority) async throws -> (ids: [String], next: String?) {
        struct MessageList: Decodable, Sendable {
            struct Item: Decodable, Sendable { let id: String }
            let messages: [Item]?
            let nextPageToken: String?
        }
        async let synchronized: Void = cache.synchronize(priority: priority)
        let page = try await api.get(MessageList.self, url, priority: priority, cost: 5)
        try await synchronized
        return (page.messages?.map(\.id) ?? [], page.nextPageToken)
    }

    @concurrent func content(id: String, mailbox: String, directory: URL, urlBase: String,
                             priority: RequestPriority) async throws -> MessageContent {
        let message = try await api.get(GmailMessage.self, Self.url("messages/\(id.percentEncoded)", [("format", "full")]),
                                        priority: priority, cost: 5)
        guard let payload = message.payload else {
            throw APIError.malformed("Gmail returned message \(id) without content")
        }
        let fetched = try await attachmentData(in: payload, message: id, priority: priority)
        var renderer = try GmailContentRenderer(assembly: MessageAssembly(directory: directory, urlBase: urlBase),
                                                fetched: fetched)
        let body = renderer.render(payload, related: false)
        return renderer.assembly.finish(html: body?.primary == true ? body?.html : nil, text: body?.text,
                                        cc: AddressList.parse(payload.header("Cc") ?? ""),
                                        replyTo: AddressList.parse(payload.header("Reply-To") ?? ""),
                                        references: parseMessageIDList(payload.header("References")))
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
                    let url = Self.url("messages/\(message.percentEncoded)/attachments/\(attachment.percentEncoded)")
                    let body = try await api.get(GmailBody.self, url, priority: priority, cost: 5)
                    return (attachment, body.data.flatMap { Data(base64URLEncoded: $0) } ?? Data())
                }
            }
            var data: [String: Data] = [:]
            for try await (id, bytes) in group { data[id] = bytes }
            return data
        }
    }

    @concurrent func setFlag(_ flag: MessageFlag, _ on: Bool, ids: [String], mailbox: String) async throws {
        let label = flag == .seen ? "UNREAD" : "STARRED"
        // Seen is the absence of UNREAD.
        let add = (flag == .seen) != on
        try await modify(ids, add: add ? [label] : [], remove: add ? [] : [label])
    }

    /// Labels Gmail keeps to itself: a move never takes them off.
    private static let fixed: Set<String> = [allMail, "SENT", "DRAFT"]

    @concurrent func move(ids: [String], from: String, to: String) async throws {
        guard from != to, !ids.isEmpty else { return }
        if to == "TRASH" {
            try await each(ids, "trash")
            await cache.relabel(ids, add: ["TRASH"], remove: [])
            return
        }
        if from == "TRASH" {
            // Untrash restores the labels the message had; then file it where asked, unless that
            // is a label Gmail keeps to itself (Sent, Drafts, All Mail), which untrash restored.
            try await each(ids, "untrash")
            await cache.relabel(ids, add: [], remove: ["TRASH"])
            if !Self.fixed.contains(to) { try await modify(ids, add: [to], remove: to == "SPAM" ? ["INBOX"] : []) }
            return
        }
        // Leaving STARRED or IMPORTANT unstars or unmarks: the message leaves that mailbox.
        var remove: Set<String> = Self.fixed.contains(from) ? [] : [from]
        if to == "SPAM" { remove.insert("INBOX") }
        try await modify(ids, add: to == Self.allMail ? [] : [to], remove: remove.sorted())
    }

    @concurrent func delete(ids: [String], mailbox: String) async throws {
        struct Body: Encodable { let ids: [String] }
        for chunk in ids.chunked(into: 1000) {
            let request = API.request(Self.url("messages/batchDelete"), method: "POST", json: Body(ids: chunk))
            _ = try await api.send(request, idempotent: true, cost: 50)
        }
        await cache.forget(ids)
    }

    /// Uploads through the media endpoint, which takes messages up to 35 MB with their thread.
    @concurrent func send(_ message: OutgoingMessage) async throws {
        struct Metadata: Encodable { let threadId: String? }
        let raw = try MIMEMessage.render(message)
        let boundary = MIMEMessage.boundary("ms_upload_")
        var body = Data("--\(boundary)\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".utf8)
        body.append(try JSONEncoder().encode(Metadata(threadId: message.thread)))
        body.append(Data("\r\n--\(boundary)\r\nContent-Type: message/rfc822\r\n\r\n".utf8))
        body.append(raw)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        var request = API.request(API.url(Endpoints.gmailUpload, "messages/send", [("uploadType", "multipart")])!,
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
        for chunk in ids.chunked(into: 1000) {
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
                    let request = API.request(Self.url("messages/\(id.percentEncoded)/\(action)"), method: "POST")
                    _ = try await api.send(request, idempotent: true, cost: 5)
                }
            }
            try await group.waitForAll()
        }
    }

    // MARK: Helpers

    /// Query parameters naming `mailbox`; All Mail is the unfiltered listing.
    private static func scope(_ mailbox: String) -> [(String, String)] {
        switch mailbox {
        case allMail: []
        case "TRASH", "SPAM": [("labelIds", mailbox), ("includeSpamTrash", "true")]
        default: [("labelIds", mailbox)]
        }
    }

    /// Paths are percent-encoded where they carry ids, so the URL always parses.
    fileprivate static func url(_ path: String, _ query: [(String, String)] = []) -> URL {
        API.url(Endpoints.gmail, path, query)!
    }
}

// MARK: - Gmail resources

private nonisolated struct GmailMessage: Decodable, Sendable {
    let id: String
    let threadId: String?
    let labelIds: [String]?
    let historyId: String?
    let internalDate: String?
    let sizeEstimate: Int?
    let payload: GmailPart?
}

private nonisolated struct GmailPart: Decodable, Sendable {
    let mimeType: String?
    let filename: String?
    let headers: [HeaderField]?
    let body: GmailBody?
    let parts: [GmailPart]?

    func header(_ name: String) -> String? {
        headers?.value(of: name)
    }

    /// Whether some part below this one is a named file.
    var hasFile: Bool {
        (parts ?? []).contains { !($0.filename ?? "").isEmpty || $0.hasFile }
    }
}

private nonisolated struct GmailBody: Decodable, Sendable {
    let attachmentId: String?
    let data: String?
}

// MARK: - Envelope cache

/// Envelopes by message id. Immutable fields come from one metadata fetch; labels follow the
/// mailbox history from the last history id seen, and a history too old to answer drops the
/// cache so the next listing fetches metadata afresh. The entries and their history id are kept
/// on disk, so a relaunch costs one history pull instead of every listed message's metadata.
private actor GmailEnvelopeCache {
    private nonisolated struct Entry: Codable {
        let envelope: Envelope
        var labels: Set<String>

        var current: Envelope {
            var envelope = envelope
            envelope.flags = Envelope.flags(seen: !labels.contains("UNREAD"), flagged: labels.contains("STARRED"),
                                            draft: labels.contains("DRAFT"))
            return envelope
        }
    }

    private nonisolated struct Snapshot: Codable {
        let historyID: UInt64
        let entries: [String: Entry]
    }

    /// Newest messages kept on disk; older ones are fetched again if a listing reaches them.
    private static let storedLimit = 10_000

    private let api: API
    private let file: URL
    private var loaded = false
    private var saveScheduled = false
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

    init(api: API, file: URL) {
        self.api = api
        self.file = file
    }

    /// Reads the disk snapshot once, before anything else touches `entries`.
    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let snapshot = JSONFile.read(Snapshot.self, at: file) else { return }
        historyID = snapshot.historyID
        entries = snapshot.entries
    }

    /// Writes the snapshot a moment after a change, so a burst of changes is one write.
    private func scheduleSave() {
        guard !saveScheduled else { return }
        saveScheduled = true
        Task(name: "Gmail envelope cache save") { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            await self?.save()
        }
    }

    private func save() {
        saveScheduled = false
        guard let historyID else {
            try? FileManager.default.removeItem(at: file)
            return
        }
        var kept = entries
        if kept.count > Self.storedLimit {
            let newest = kept.sorted { ($0.value.envelope.date ?? .distantPast) > ($1.value.envelope.date ?? .distantPast) }
            kept = Dictionary(uniqueKeysWithValues: newest.prefix(Self.storedLimit).map { ($0.key, $0.value) })
        }
        try? JSONFile.write(Snapshot(historyID: historyID, entries: kept), to: file, creatingDirectory: true)
    }

    /// Applies every label change and deletion since the last pull. A pull already running
    /// answers callers that arrived before it started; later ones get a pull of their own.
    func synchronize(priority: RequestPriority) async throws {
        load()
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
                // A pull that changed nothing leaves fetches in flight current.
                if !ids.isEmpty { advance(ids) }
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
        try await lookup(ids, priority: priority).map(\.current)
    }

    /// `envelopes` placed in the mailbox their labels list them under: Inbox, Sent or Drafts
    /// when they carry that label, else All Mail.
    func hits(_ ids: [String], priority: RequestPriority) async throws -> [SearchHit] {
        try await lookup(ids, priority: priority).map { entry in
            let mailbox = ["INBOX", "SENT", "DRAFT"].first(where: entry.labels.contains) ?? GmailBackend.allMail
            return SearchHit(mailbox: mailbox, envelope: entry.current)
        }
    }

    private func lookup(_ ids: [String], priority: RequestPriority) async throws -> [Entry] {
        load()
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
        return ids.compactMap { entries[$0] ?? fetched[$0] }
    }

    private func fetch(_ ids: [String], priority: RequestPriority) async throws -> [String: Entry] {
        defer { for id in ids { loading[id] = nil } }
        // The baseline comes first: history from it replays any change racing the fetch.
        if historyID == nil {
            let baseline = try await GmailBackend.historyID(api: api, priority: priority)
            if historyID == nil { historyID = baseline }
        }
        // Fifty at a time, each stored as it lands: a refusal or quit midway keeps what came before.
        var fetched: [String: Entry] = [:]
        for chunk in ids.chunked(into: 50) {
            let since = generation
            let messages = try await GmailBackend.metadata(chunk, query: GmailBackend.metadataQuery, api: api,
                                                           priority: priority)
            // Metadata shows only the top MIME type; a multipart/mixed message has attachments
            // only if a part carries a file name.
            let mixed = messages.filter { $0.payload?.mimeType?.lowercased() == "multipart/mixed" }.map(\.id)
            let withFiles = mixed.isEmpty ? [] : Set(try await GmailBackend.metadata(
                mixed, query: GmailBackend.partsQuery, api: api, priority: priority)
                .filter { $0.payload?.hasFile == true }.map(\.id))
            let stale = changed(since: since)
            for message in messages {
                let entry = Self.entry(message, hasAttachment: withFiles.contains(message.id))
                fetched[message.id] = entry
                guard let stale, !stale.contains(message.id), entries[message.id] == nil else { continue }
                entries[message.id] = entry
            }
            scheduleSave()
        }
        return fetched
    }

    /// Our own label change, ahead of the history reporting it.
    func relabel(_ ids: [String], add: [String], remove: [String]) {
        load()
        for id in ids {
            entries[id]?.labels.formUnion(add)
            entries[id]?.labels.subtract(remove)
        }
        advance(Set(ids))
    }

    func forget(_ ids: [String]) {
        load()
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
        scheduleSave()
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
        let inReplyTo = parseMessageIDList(payload?.header("In-Reply-To"))
        let envelope = Envelope(
            id: message.id,
            thread: message.threadId,
            messageID: payload?.header("Message-ID").map(normalizeMessageID)?.nilIfEmpty,
            inReplyTo: inReplyTo.isEmpty ? Array(parseMessageIDList(payload?.header("References")).suffix(1)) : inReplyTo,
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
        return formEncoded(query)
    }()

    /// The part tree's file names, two levels deep, without bodies or headers.
    fileprivate static let partsQuery = formEncoded([("format", "full"),
                                                    ("fields", "id,payload(parts(filename,parts(filename)))")])

    /// The mailbox's current history id.
    fileprivate static func historyID(api: API, priority: RequestPriority) async throws -> UInt64? {
        struct Profile: Decodable, Sendable { let historyId: String? }
        return try await api.get(Profile.self, url("profile", [("fields", "historyId")]), priority: priority, cost: 1)
            .historyId.flatMap(UInt64.init)
    }

    /// Message metadata of `ids` through the batch endpoint; messages that no longer exist are left out.
    fileprivate static func metadata(_ ids: [String], query: String, api: API,
                                     priority: RequestPriority) async throws -> [GmailMessage] {
        Array(try await batchGet(GmailMessage.self, "messages", ids: ids, query: query, unitCost: 5, api: api,
                                 priority: priority).values)
    }

    /// `GET <collection>/<id>?<query>` for every id through Gmail's batch endpoint, 50 per
    /// request and one batch at a time: each sub-request counts against the per-user quota, so
    /// parallel batches were what Gmail refused. Sub-requests Gmail throttles or fails transiently
    /// are sent again. Answers are keyed by id; ids that no longer exist are left out. `unitCost`
    /// is one sub-request's quota units.
    fileprivate static func batchGet<T: Decodable & Sendable>(_ type: T.Type, _ collection: String, ids: [String],
                                                             query: String, unitCost: Int, api: API,
                                                             priority: RequestPriority) async throws -> [String: T] {
        // Sub-requests are numbered within their batch (`Content-ID: <m3>`), and answers matched by that number.
        let numbered = ids.chunked(into: 50).flatMap { chunk in chunk.enumerated().map { (number: $0.offset, id: $0.element) } }
        var found: [String: T] = [:]
        try await api.batched(numbered, size: 50, service: "Gmail") { pending in
            let boundary = MIMEMessage.boundary("batch_", lowercased: false)
            var body = ""
            for (number, id) in pending {
                body += "--\(boundary)\r\nContent-Type: application/http\r\nContent-ID: <m\(number)>\r\n\r\n"
                    + "GET \(Endpoints.gmail.path())\(collection)/\(id.percentEncoded)?\(query)\r\n\r\n"
            }
            body += "--\(boundary)--\r\n"
            var request = API.request(Endpoints.gmailBatch, method: "POST",
                                      headers: ["Content-Type": "multipart/mixed; boundary=\(boundary)"])
            request.httpBody = Data(body.utf8)
            let (data, response) = try await api.send(request, priority: priority, idempotent: true,
                                                      cost: unitCost * pending.count)
            guard let responseBoundary = HeaderText.parameter("boundary", in: response.value(forHTTPHeaderField: "Content-Type")) else {
                throw APIError(status: response.statusCode, message: "Gmail answered a batch request without parts")
            }
            // Whatever stays in `retry` (throttled, failed transiently, unanswered) is sent again.
            var retry = Dictionary(uniqueKeysWithValues: pending.map { ($0.number, $0.id) })
            for part in batchParts(data, boundary: responseBoundary) {
                guard let number = part.index, let id = retry[number] else { continue }
                switch part.status {
                case 200:
                    do {
                        found[id] = try JSONDecoder().decode(T.self, from: part.body)
                    } catch {
                        throw APIError.unexpectedAnswer(from: Endpoints.gmailBatch.host(), error)
                    }
                case 404:
                    break
                default:
                    guard API.retry(status: part.status, body: part.body) != nil else {
                        throw APIError(status: part.status, body: part.body)
                    }
                    continue
                }
                retry[number] = nil
            }
            return (pending.filter { retry[$0.number] != nil }, nil)
        }
        return found
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
}

// MARK: - Content

/// Walks a `format=full` payload: picks the HTML and plain bodies, stores every other part
/// through `MessageAssembly` in part order, and renders forwarded messages inline.
private nonisolated struct GmailContentRenderer {
    var assembly: MessageAssembly
    let fetched: [String: Data]
    private var nextIndex = 0

    init(assembly: MessageAssembly, fetched: [String: Data]) {
        self.assembly = assembly
        self.fetched = fetched
    }

    /// What a part shows. `primary` when `html` is a real HTML body (a forwarded message's
    /// included); other HTML (an inline image) only joins when a sibling has one.
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
        let attachment = assembly.store(bytes, index: index, filename: filename.nilIfEmpty, mime: mime,
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
        let inner = nested.count == 1 ? render(nested[0], related: false)
            : Self.combine(renderAll(nested, related: false))
        let innerHTML = inner?.primary == true ? inner?.html : nil
        let attached = MessageAssembly.AttachedMessage(
            from: AddressList.parse(source?.header("From") ?? "").first,
            subject: HeaderText.decode(source?.header("Subject") ?? ""),
            html: innerHTML, text: inner?.text ?? innerHTML.map(HTMLText.strip) ?? "")
        return Segment(html: attached.html, text: attached.text, primary: attached.hasHTML)
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
        if let data = part.body?.data { return Data(base64URLEncoded: data) }
        if let id = part.body?.attachmentId { return fetched[id] }
        return nil
    }
}
