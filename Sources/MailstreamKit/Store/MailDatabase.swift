import Foundation

/// The Turso database behind `MailCache`. Values are JSON; envelopes and tags have a row each.
/// Opens on first use; a database made fresh first takes in the JSON files of the layout before
/// it, so nothing kept there is lost, and removes them.
actor MailDatabase {
    /// Tables holding one JSON value per key.
    enum Table: String {
        case state, mailboxes, contents
        case chatSpaces = "chat_spaces"
        case chatMessages = "chat_messages"
    }

    private let url: URL
    /// Where the JSON layout kept its files.
    private let files: URL
    private var connection: SQLiteConnection?
    private var unavailable = false

    init(url: URL, files: URL) {
        self.url = url
        self.files = files
    }

    static func contentKey(_ messageKey: String, version: Int) -> String { "v\(version):\(messageKey)" }

    private static let schema = """
        CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS mailboxes (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS contents (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS chat_spaces (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS chat_messages (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS tags (key TEXT PRIMARY KEY, value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS listings (key TEXT PRIMARY KEY, next TEXT);
        CREATE TABLE IF NOT EXISTS envelopes (
            listing TEXT NOT NULL, position INTEGER NOT NULL, id TEXT NOT NULL, value BLOB NOT NULL,
            PRIMARY KEY (listing, id));
        CREATE INDEX IF NOT EXISTS envelopes_order ON envelopes (listing, position);
        """

    // MARK: - Values

    func value(_ table: Table, _ key: String) -> Data? {
        run { db in try db.rows("SELECT value FROM \(table.rawValue) WHERE key = ?", [.text(key)]) { $0.data(0) }.first }
            ?? nil
    }

    func setValue(_ data: Data, _ table: Table, _ key: String) {
        run { db in
            try db.execute("INSERT INTO \(table.rawValue) (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                           [.text(key), .blob(data)])
        }
    }

    func remove(_ table: Table, _ key: String) {
        run { db in try db.execute("DELETE FROM \(table.rawValue) WHERE key = ?", [.text(key)]) }
    }

    // MARK: - Tags

    func tags() -> [String: Data] {
        let rows = run { db in
            try db.rows("SELECT key, value FROM tags") { row in row.text(0).flatMap { key in row.data(1).map { (key, $0) } } }
        }
        return Dictionary(rows ?? [], uniquingKeysWith: { a, _ in a })
    }

    /// Writes these tags, leaving the others: each process writes only what it changed, so one
    /// never overwrites another's changes with its older copy.
    func setTags(_ tags: [String: Data]) {
        run { db in
            try db.transaction {
                for (key, value) in tags {
                    try db.execute("INSERT OR REPLACE INTO tags (key, value) VALUES (?, ?)", [.text(key), .blob(value)])
                }
            }
        }
    }

    func clearTags() {
        run { db in try db.execute("DELETE FROM tags") }
    }

    // MARK: - Listings

    /// A mailbox's envelopes in listed order and the cursor of the page after them.
    func listing(_ key: String) -> (envelopes: [Data], next: String?)? {
        run { db -> (envelopes: [Data], next: String?)? in
            let listing = try db.rows("SELECT next FROM listings WHERE key = ?", [.text(key)]) { row in Optional(row.text(0)) }
            guard let next = listing.first else { return nil }
            let envelopes = try db.rows("SELECT value FROM envelopes WHERE listing = ? ORDER BY position", [.text(key)]) { $0.data(0) }
            return (envelopes, next)
        } ?? nil
    }

    func replaceListing(_ key: String, next: String?, envelopes: [(id: String, value: Data)]) {
        run { db in try db.transaction { try Self.insertListing(key, next: next, envelopes: envelopes, into: db) } }
    }

    private static func insertListing(_ key: String, next: String?, envelopes: [(id: String, value: Data)],
                                      into db: SQLiteConnection) throws {
        try db.execute("INSERT INTO listings (key, next) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET next = excluded.next",
                       [.text(key), next.map { .text($0) } ?? .null])
        try db.execute("DELETE FROM envelopes WHERE listing = ?", [.text(key)])
        var seen = Set<String>()
        for (position, envelope) in envelopes.enumerated() where seen.insert(envelope.id).inserted {
            try db.execute("INSERT INTO envelopes (listing, position, id, value) VALUES (?, ?, ?, ?)",
                           [.text(key), .integer(Int64(position)), .text(envelope.id), .blob(envelope.value)])
        }
    }

    // MARK: - Opening

    /// `body` on the open database; nil when it cannot open or `body` fails, which is logged.
    private func run<T>(_ body: (SQLiteConnection) throws -> T) -> T? {
        guard let db = open() else { return nil }
        do {
            return try body(db)
        } catch {
            NSLog("Mailstream database: %@", error.localizedDescription)
            return nil
        }
    }

    private func open() -> SQLiteConnection? {
        if let connection { return connection }
        guard !unavailable else { return nil }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            let db = try SQLiteConnection(path: url.path)
            try db.script(Self.schema)
            if try db.rows("SELECT value FROM state WHERE key = 'imported'", [], { $0.data(0) }).isEmpty {
                try importFiles(into: db)
            }
            connection = db
            return db
        } catch {
            unavailable = true
            NSLog("Mailstream database %@: %@", url.path, error.localizedDescription)
            return nil
        }
    }

    // MARK: - Importing the JSON layout

    /// Takes in the JSON files the cache kept before the database, in one transaction, then
    /// removes them; attachments and inline parts stay where they are.
    private func importFiles(into db: SQLiteConnection) throws {
        let fm = FileManager.default
        let decoder = JSONDecoder(), encoder = JSONEncoder()
        var values: [(table: Table, key: String, file: URL)] = []
        for (name, key) in [("accounts-v2.json", "accounts"), ("chat-accounts-v2.json", "chatAccounts"),
                            ("chat-v2/recent-v1.json", "chatRecent")] {
            values.append((.state, key, files.appendingPathComponent(name)))
        }
        for file in Self.listed(files.appendingPathComponent("mailboxes-v2")) {
            values.append((.mailboxes, file.deletingPathExtension().lastPathComponent, file))
        }
        for file in Self.listed(files.appendingPathComponent("chat-v2")) {
            let name = file.deletingPathExtension().lastPathComponent
            for (prefix, table) in [("spaces-", Table.chatSpaces), ("messages-", .chatMessages)] where name.hasPrefix(prefix) {
                values.append((table, String(name.dropFirst(prefix.count)), file))
            }
        }
        for directory in Self.listed(files.appendingPathComponent("messages-v2"), json: false) {
            values.append((.contents, Self.contentKey(directory.lastPathComponent, version: MailCache.contentVersion),
                           directory.appendingPathComponent("content-v\(MailCache.contentVersion).json")))
        }
        let tagsFile = files.appendingPathComponent("tags-v2.json")
        let listingFiles = Self.listed(files.appendingPathComponent("envelopes-v3"))

        try db.transaction {
            for value in values {
                guard let data = try? Data(contentsOf: value.file) else { continue }
                try db.execute("INSERT OR REPLACE INTO \(value.table.rawValue) (key, value) VALUES (?, ?)",
                               [.text(value.key), .blob(data)])
            }
            if let data = try? Data(contentsOf: tagsFile), let tags = try? decoder.decode([String: ConversationTag].self, from: data) {
                for (key, tag) in tags {
                    try db.execute("INSERT OR REPLACE INTO tags (key, value) VALUES (?, ?)", [.text(key), .blob(try encoder.encode(tag))])
                }
            }
            for file in listingFiles {
                guard let data = try? Data(contentsOf: file),
                      let listing = try? decoder.decode(CachedListing.self, from: data) else { continue }
                let rows = try listing.envelopes.map { (id: $0.id, value: try encoder.encode($0)) }
                try Self.insertListing(file.deletingPathExtension().lastPathComponent, next: listing.next, envelopes: rows, into: db)
            }
            try db.execute("INSERT OR REPLACE INTO state (key, value) VALUES ('imported', ?)", [.blob(Data("1".utf8))])
        }
        for value in values { try? fm.removeItem(at: value.file) }
        try? fm.removeItem(at: tagsFile)
        for directory in ["envelopes-v3", "mailboxes-v2", "chat-v2"] {
            try? fm.removeItem(at: files.appendingPathComponent(directory))
        }
    }

    /// The JSON files in `directory` (or with `json` false, everything in it).
    private static func listed(_ directory: URL, json: Bool = true) -> [URL] {
        let items = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        return json ? items.filter { $0.pathExtension == "json" } : items
    }
}
