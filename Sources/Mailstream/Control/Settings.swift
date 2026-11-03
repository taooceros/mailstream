import Foundation

/// Every user setting. The file `~/Library/Application Support/Mailstream/settings.json` holds the
/// ones changed from their default as a flat object of dotted keys (`"sync.pollSeconds": 120`);
/// the app follows edits to it as they happen, whoever makes them: Settings UI, `mailstream config`,
/// an agent or an editor.
enum Setting: String, CaseIterable, Sendable {
    case aiTaggingEnabled = "aiTagging.enabled"
    case aiTaggingModel = "aiTagging.model"
    case aiTaggingOmpPath = "aiTagging.ompPath"
    case aiTaggingBatchSize = "aiTagging.batchSize"
    case aiTaggingPassLimit = "aiTagging.passLimit"
    case aiTaggingBackoffSeconds = "aiTagging.backoffSeconds"
    case trustedSenders = "remoteContent.trustedSenders"
    case pollSeconds = "sync.pollSeconds"
    case pushPollSeconds = "sync.pushPollSeconds"
    case chatPollSeconds = "chat.pollSeconds"

    enum Kind: Sendable {
        case bool
        case int(ClosedRange<Int>)
        case string
        /// Lowercased email addresses, kept sorted and unique.
        case addresses
    }

    var kind: Kind {
        switch self {
        case .aiTaggingEnabled: .bool
        case .aiTaggingModel, .aiTaggingOmpPath: .string
        case .aiTaggingBatchSize: .int(1...100)
        case .aiTaggingPassLimit: .int(1...2000)
        case .aiTaggingBackoffSeconds: .int(0...86_400)
        case .trustedSenders: .addresses
        case .pollSeconds: .int(15...3600)
        case .pushPollSeconds: .int(60...86_400)
        case .chatPollSeconds: .int(5...3600)
        }
    }

    var defaultValue: JSON {
        switch self {
        case .aiTaggingEnabled: true
        case .aiTaggingModel: "smol"
        case .aiTaggingOmpPath: ""
        case .aiTaggingBatchSize: 20
        case .aiTaggingPassLimit: 150
        case .aiTaggingBackoffSeconds: 300
        case .trustedSenders: []
        case .pollSeconds: 90
        case .pushPollSeconds: 900
        case .chatPollSeconds: 20
        }
    }

    var summary: String {
        switch self {
        case .aiTaggingEnabled:
            "Sort the inbox into People, Notifications, Newsletters and Invitations with omp, and flag mail waiting on a reply."
        case .aiTaggingModel: "omp model role or model id the tagger runs (`omp --model`)."
        case .aiTaggingOmpPath: "Path of the omp executable; empty picks the newest omp on the login shell's PATH."
        case .aiTaggingBatchSize: "Conversations sent to omp per run."
        case .aiTaggingPassLimit: "Most conversations one tagging pass looks at, newest first."
        case .aiTaggingBackoffSeconds: "Wait after a failed tagging run before tagging automatically again."
        case .trustedSenders:
            "Senders whose remote images and fonts always load, besides your own addresses and people you wrote to."
        case .pollSeconds: "Mail polling interval while push is not connected."
        case .pushPollSeconds: "Safety-net polling interval while push is connected."
        case .chatPollSeconds: "Google Chat polling interval."
        }
    }

    /// `value` checked against the kind and normalized; throws a message naming the setting.
    func validate(_ value: JSON) throws -> JSON {
        switch kind {
        case .bool:
            guard let bool = value.bool else { throw SettingsError("\(rawValue) must be true or false.") }
            return .bool(bool)
        case .int(let range):
            guard let int = value.int else { throw SettingsError("\(rawValue) must be an integer.") }
            guard range.contains(int) else {
                throw SettingsError("\(rawValue) must be between \(range.lowerBound) and \(range.upperBound).")
            }
            return .int(int)
        case .string:
            guard let string = value.string else { throw SettingsError("\(rawValue) must be a string.") }
            return .string(string.trimmingCharacters(in: .whitespacesAndNewlines))
        case .addresses:
            guard let items = value.array, let strings = value.strings, strings.count == items.count else {
                throw SettingsError("\(rawValue) must be an array of email addresses.")
            }
            let addresses = strings.map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
            if let bad = addresses.first(where: { !$0.contains("@") || $0.contains(where: \.isWhitespace) }) {
                throw SettingsError("\(rawValue): \"\(bad)\" is not an email address.")
            }
            return .strings(Array(Set(addresses)).sorted())
        }
    }

    /// A value typed on a command line: `true`/`off`, `120`, text, or for addresses a JSON array
    /// or comma-separated list.
    func parse(_ text: String) throws -> JSON {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        switch kind {
        case .bool:
            switch trimmed.lowercased() {
            case "true", "on", "yes", "1": return true
            case "false", "off", "no", "0": return false
            default: throw SettingsError("\(rawValue) must be true or false, not \"\(text)\".")
            }
        case .int:
            guard let int = Int(trimmed) else { throw SettingsError("\(rawValue) must be an integer, not \"\(text)\".") }
            return try validate(.int(int))
        case .string:
            return try validate(.string(text))
        case .addresses:
            if trimmed.hasPrefix("["), let parsed = try? JSON.decode(Data(trimmed.utf8)) { return try validate(parsed) }
            let parts = trimmed.split(separator: ",").map { String($0) }.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
            return try validate(.strings(parts))
        }
    }

    /// JSON Schema of this setting.
    var schema: JSON {
        var out: [String: JSON] = ["description": .string(summary), "default": defaultValue]
        switch kind {
        case .bool: out["type"] = "boolean"
        case .int(let range):
            out["type"] = "integer"
            out["minimum"] = .int(range.lowerBound)
            out["maximum"] = .int(range.upperBound)
        case .string: out["type"] = "string"
        case .addresses:
            out["type"] = "array"
            out["items"] = ["type": "string", "format": "email"]
            out["uniqueItems"] = true
        }
        return .object(out)
    }

    static func named(_ key: String) throws -> Setting {
        guard let setting = Setting(rawValue: key) else {
            let close = allCases.map(\.rawValue).filter { $0.lowercased().contains(key.lowercased()) }
            throw SettingsError("Unknown setting \"\(key)\"." + (close.isEmpty ? "" : " Did you mean \(close.joined(separator: ", "))?")
                                + " `mailstream config list` shows them all.")
        }
        return setting
    }

    /// JSON Schema of the whole file.
    static var fileSchema: JSON {
        .object([
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Mailstream settings",
            "description": "~/Library/Application Support/Mailstream/settings.json. Only settings changed from their default need to be present; the app applies edits immediately.",
            "type": "object",
            "additionalProperties": false,
            "properties": .object(Dictionary(uniqueKeysWithValues: allCases.map { ($0.rawValue, $0.schema) })),
        ])
    }
}

struct SettingsError: LocalizedError, Sendable {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

/// Reading and writing settings.json, without the app: the CLI uses this directly.
enum SettingsFile {
    static let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appending(path: "Mailstream", directoryHint: .isDirectory)
    static let url = directory.appending(path: "settings.json")

    /// The file's object as written; empty when there is no file.
    static func readRaw() throws -> [String: JSON] {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch CocoaError.fileReadNoSuchFile {
            return [:]
        }
        if data.allSatisfy({ $0 == 0x20 || $0 == 0x0A || $0 == 0x0D || $0 == 0x09 }) { return [:] }
        let json: JSON
        do {
            json = try JSON.decode(data)
        } catch {
            throw SettingsError("\(url.path) is not valid JSON: \(error.localizedDescription)")
        }
        guard let object = json.object else { throw SettingsError("\(url.path) must hold a JSON object.") }
        return object
    }

    /// Valid values by setting, plus a message for each key that is unknown or invalid (those
    /// fall back to their default).
    static func read() -> (values: [Setting: JSON], issues: [String]) {
        let raw: [String: JSON]
        do {
            raw = try readRaw()
        } catch {
            return ([:], [error.localizedDescription])
        }
        var values: [Setting: JSON] = [:]
        var issues: [String] = []
        for (key, value) in raw.sorted(by: { $0.key < $1.key }) {
            guard let setting = Setting(rawValue: key) else {
                issues.append("Unknown setting \"\(key)\" is ignored.")
                continue
            }
            do {
                values[setting] = try setting.validate(value)
            } catch {
                issues.append(error.localizedDescription + " The default applies.")
            }
        }
        return (values, issues)
    }

    /// Sets (or with nil, removes) one key, keeping everything else in the file as it was.
    /// Writing a default value removes the key, so the file lists only real changes.
    static func update(_ setting: Setting, to value: JSON?) throws {
        var raw = try readRaw()
        let normalized = try value.map(setting.validate)
        if let normalized, normalized != setting.defaultValue {
            raw[setting.rawValue] = normalized
        } else {
            raw[setting.rawValue] = nil
        }
        try write(raw)
    }

    static func write(_ raw: [String: JSON]) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var data = JSON.object(raw).encoded()
        data.append(0x0A)
        try data.write(to: url, options: .atomic)
    }
}

/// The settings in effect, following settings.json as it changes.
@MainActor
final class Settings {
    static let shared = Settings()
    /// Posted when effective values change; `userInfo["settings"]` is the `Set<Setting>` changed.
    static let didChange = Notification.Name("Settings.didChange")

    private(set) var values: [Setting: JSON]
    /// Problems with the file as last read: unknown keys, invalid values, broken JSON.
    private(set) var issues: [String]
    /// The directory (saves that replace the file, creation, deletion) and the file itself
    /// (writes in place), re-armed whenever the file is replaced.
    private var directoryWatcher: DispatchSourceFileSystemObject?
    private var fileWatcher: DispatchSourceFileSystemObject?

    private init() {
        Self.migrateUserDefaults()
        (values, issues) = SettingsFile.read()
    }

    subscript(_ setting: Setting) -> JSON { values[setting] ?? setting.defaultValue }
    func bool(_ setting: Setting) -> Bool { self[setting].bool ?? setting.defaultValue.bool ?? false }
    func int(_ setting: Setting) -> Int { self[setting].int ?? setting.defaultValue.int ?? 0 }
    func string(_ setting: Setting) -> String { self[setting].string ?? "" }
    func strings(_ setting: Setting) -> [String] { self[setting].strings ?? [] }

    /// Writes the file and applies the change at once; nil resets to the default.
    func set(_ setting: Setting, _ value: JSON?) throws {
        try SettingsFile.update(setting, to: value)
        reload()
    }

    /// Follows the file however it is edited.
    func startWatching() {
        guard directoryWatcher == nil else { return }
        try? FileManager.default.createDirectory(at: SettingsFile.directory, withIntermediateDirectories: true)
        directoryWatcher = watch(SettingsFile.directory.path, [.write, .rename]) { [weak self] in
            self?.watchFile()
            self?.reload()
        }
        watchFile()
    }

    private func watchFile() {
        fileWatcher?.cancel()
        fileWatcher = watch(SettingsFile.url.path, [.write, .extend, .delete, .rename]) { [weak self] in
            self?.watchFile()
            self?.reload()
        }
    }

    private func watch(_ path: String, _ events: DispatchSource.FileSystemEvent,
                       _ handler: @escaping @MainActor () -> Void) -> DispatchSourceFileSystemObject? {
        let descriptor = open(path, O_EVTONLY)
        guard descriptor >= 0 else { return nil }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: descriptor, eventMask: events, queue: .main)
        source.setEventHandler { MainActor.assumeIsolated { handler() } }
        source.setCancelHandler { close(descriptor) }
        source.resume()
        return source
    }

    func reload() {
        let (next, nextIssues) = SettingsFile.read()
        issues = nextIssues
        let changed = Set(Setting.allCases.filter { (next[$0] ?? $0.defaultValue) != self[$0] })
        values = next
        guard !changed.isEmpty else { return }
        NotificationCenter.default.post(name: Self.didChange, object: self, userInfo: ["settings": changed])
    }

    /// Settings kept in UserDefaults before settings.json existed move into it once.
    private static func migrateUserDefaults() {
        let defaults = UserDefaults.standard
        let moves: [(String, Setting)] = [("AITaggingEnabled", .aiTaggingEnabled), ("remoteContentSenders", .trustedSenders)]
        for (key, setting) in moves {
            guard let old = defaults.object(forKey: key) else { continue }
            let value: JSON? = switch setting.kind {
            case .bool: (old as? Bool).map(JSON.bool)
            case .addresses: (old as? [String]).map(JSON.strings)
            default: nil
            }
            do {
                if let value { try SettingsFile.update(setting, to: value) }
                defaults.removeObject(forKey: key)
            } catch {
                // Kept for the next launch.
            }
        }
    }
}

extension Notification {
    /// The settings a `Settings.didChange` reports.
    var changedSettings: Set<Setting> { userInfo?["settings"] as? Set<Setting> ?? [] }
}
