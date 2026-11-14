import Foundation

/// Every user setting. The file `~/Library/Application Support/Mailstream/settings.json` holds the
/// ones changed from their default as a flat object of dotted keys (`"sync.pollSeconds": 120`);
/// the app follows edits to it as they happen, whoever makes them: Settings UI, `mailstream config`,
/// an agent or an editor.
nonisolated enum Setting: String, CaseIterable, Sendable {
    case aiTaggingEnabled = "aiTagging.enabled"
    case aiTaggingBackend = "aiTagging.backend"
    case aiTaggingModel = "aiTagging.model"
    case aiTaggingOmpPath = "aiTagging.ompPath"
    case aiTaggingBatchSize = "aiTagging.batchSize"
    case aiTaggingPassLimit = "aiTagging.passLimit"
    case aiTaggingBackoffSeconds = "aiTagging.backoffSeconds"
    case aiTaggingLocalOnlyAccounts = "aiTagging.localOnlyAccounts"
    case trustedSenders = "remoteContent.trustedSenders"
    case pollSeconds = "sync.pollSeconds"
    case pushPollSeconds = "sync.pushPollSeconds"
    case chatPollSeconds = "chat.pollSeconds"
    case flutterShowFrameRate = "flutter.showFrameRate"
    case flutterListWidth = "flutter.listWidth"
    case flutterCalendarWidth = "flutter.calendarWidth"
    case gpuiShowFrameRate = "gpui.showFrameRate"
    case gpuiListWidth = "gpui.listWidth"
    case gpuiCalendarWidth = "gpui.calendarWidth"
    case autoReadEnabled = "autoRead.enabled"
    case autoReadDays = "autoRead.days"

    enum Kind: Sendable {
        case bool
        case int(ClosedRange<Int>)
        case string
        /// One of the given words.
        case choice([String])
        /// Lowercased email addresses, kept sorted and unique.
        case addresses
    }

    /// What the key holds, its default and its description: the one place a new setting is
    /// described.
    private var descriptor: (kind: Kind, defaultValue: JSON, summary: String) {
        switch self {
        case .aiTaggingEnabled:
            (.bool, true,
             "Sort the inbox into People, Notifications, Newsletters and Invitations with AI, and flag mail waiting on your reply or action.")
        case .aiTaggingBackend:
            (.choice(["omp", "apple"]), "omp",
             "Who tags: omp runs the model aiTagging.model names; apple runs Apple Intelligence's on-device model (macOS 26 or later, Apple Intelligence on).")
        case .aiTaggingModel: (.string, "smol", "omp model role or model id the tagger runs (`omp --model`).")
        case .aiTaggingOmpPath: (.string, "", "Path of the omp executable; empty picks the newest omp on the login shell's PATH.")
        case .aiTaggingBatchSize: (.int(1...100), 20, "Conversations sent to the model per run.")
        case .aiTaggingPassLimit: (.int(1...2000), 150, "Most conversations one tagging pass looks at, newest first.")
        case .aiTaggingBackoffSeconds: (.int(0...86_400), 300, "Wait after a failed tagging run before tagging automatically again.")
        case .aiTaggingLocalOnlyAccounts:
            (.addresses, [],
             "Accounts whose AI tags stay on this device. Every other Gmail and Outlook account's tags sync with the server both ways, so your other devices show them and don't tag that mail again: as labels or categories named People, Notifications, Newsletters, Invitations and Waiting on you (an existing one of that name is used, in any case), and topic labels as Mailstream/<topic>. Labels are never deleted.")
        case .trustedSenders:
            (.addresses, [],
             "Senders whose remote images and fonts always load, besides your own addresses and people you wrote to.")
        case .pollSeconds: (.int(15...3600), 90, "Mail polling interval while push is not connected.")
        case .pushPollSeconds: (.int(60...86_400), 900, "Safety-net polling interval while push is connected.")
        case .chatPollSeconds: (.int(5...3600), 20, "Google Chat polling interval.")
        case .flutterShowFrameRate:
            (.bool, false,
             "Show the frame rate and the slowest recent frame in a corner of Mailstream Flutter's window (also View ▸ Show Frame Rate).")
        case .flutterListWidth:
            (.int(100...900), 400,
             "Width of the mail or chat list beside an open conversation in Mailstream Flutter, in thousandths of the window's width beside the sidebar. Drag the gap between them to change it; double-click the gap to reset.")
        case .flutterCalendarWidth:
            (.int(100...900), 500,
             "Width of the calendar beside an open event in Mailstream Flutter, in thousandths of the window's width beside the sidebar. Drag the gap between them to change it; double-click the gap to reset.")
        case .gpuiShowFrameRate:
            (.bool, false,
             "Show the frame rate and the slowest recent frame in a corner of Mailstream GPUI's window (also Ctrl+Alt+P, ⌥⌘P on the Mac).")
        case .gpuiListWidth:
            (.int(100...900), 400,
             "Width of the mail list beside an open conversation in Mailstream GPUI, in thousandths of the window's width beside the sidebar. Drag the gap between them to change it; double-click the gap to reset.")
        case .gpuiCalendarWidth:
            (.int(100...900), 400,
             "Width of the calendar beside an open event in Mailstream GPUI, in thousandths of the window's width beside the sidebar. Drag the gap between them to change it; double-click the gap to reset.")
        case .autoReadEnabled:
            (.bool, false,
             "Mark stale notifications and newsletters (ads, promotions) read on their own, as AI tagging sorts them: unread ones older than autoRead.days, and a newsletter once its sender sends a newer one. Never mail waiting on you, starred mail, people, invitations or threads you wrote in. Mailstream Flutter offers Undo.")
        case .autoReadDays: (.int(1...90), 3, "Days an unread notification or newsletter waits before auto-read marks it read.")
        }
    }

    var kind: Kind { descriptor.kind }
    var defaultValue: JSON { descriptor.defaultValue }
    var summary: String { descriptor.summary }

    /// The value in effect given the file's valid `values`: the default when the key is absent.
    func value(in values: [Setting: JSON]) -> JSON { values[self] ?? defaultValue }

    /// `value` checked against the kind and normalized; throws a message naming the setting.
    func validate(_ value: JSON) throws -> JSON {
        switch kind {
        case .bool:
            guard let bool = value.bool else { throw ControlError.invalidSetting("\(rawValue) must be true or false.") }
            return .bool(bool)
        case .int(let range):
            guard let int = value.int else { throw ControlError.invalidSetting("\(rawValue) must be an integer.") }
            guard range.contains(int) else {
                throw ControlError.invalidSetting("\(rawValue) must be between \(range.lowerBound) and \(range.upperBound).")
            }
            return .int(int)
        case .string:
            guard let string = value.string else { throw ControlError.invalidSetting("\(rawValue) must be a string.") }
            return .string(string.trimmingCharacters(in: .whitespacesAndNewlines))
        case .choice(let words):
            guard let word = value.string?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(), words.contains(word) else {
                throw ControlError.invalidSetting("\(rawValue) must be one of \(words.joined(separator: ", ")).")
            }
            return .string(word)
        case .addresses:
            guard let items = value.array, let strings = value.strings, strings.count == items.count else {
                throw ControlError.invalidSetting("\(rawValue) must be an array of email addresses.")
            }
            let addresses = strings.map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
            if let bad = addresses.first(where: { !$0.contains("@") || $0.contains(where: \.isWhitespace) }) {
                throw ControlError.invalidSetting("\(rawValue): \"\(bad)\" is not an email address.")
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
            guard let bool = TypedValue.bool(trimmed) else {
                throw ControlError.invalidSetting("\(rawValue) must be true or false, not \"\(text)\".")
            }
            return .bool(bool)
        case .int:
            guard let int = Int(trimmed) else { throw ControlError.invalidSetting("\(rawValue) must be an integer, not \"\(text)\".") }
            return try validate(.int(int))
        case .string, .choice:
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
        case .choice(let words):
            out["type"] = "string"
            out["enum"] = .strings(words)
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
            throw ControlError.invalidSetting("Unknown setting \"\(key)\"." + (close.isEmpty ? "" : " Did you mean \(close.joined(separator: ", "))?")
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

/// Reading and writing settings.json, without the app: the CLI uses this directly.
nonisolated enum SettingsFile {
    static let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appending(path: "Mailstream", directoryHint: .isDirectory)
    static let url = directory.appending(path: "settings.json")
    /// Keys earlier versions wrote that mean nothing now: read without an issue and dropped when
    /// the file is next written. `aiTagging.syncAccounts` listed the accounts that synced tags,
    /// until every account did (`aiTagging.localOnlyAccounts`, MS-341).
    static let retired: Set<String> = ["aiTagging.syncAccounts"]

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
            throw ControlError.invalidSetting("\(url.path) is not valid JSON: \(error.localizedDescription)")
        }
        guard let object = json.object else { throw ControlError.invalidSetting("\(url.path) must hold a JSON object.") }
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
                if !retired.contains(key) { issues.append("Unknown setting \"\(key)\" is ignored.") }
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

    /// Sets (or with nil, removes) one key, keeping everything else in the file as it was but
    /// `retired` keys. Writing a default value removes the key, so the file lists only real changes.
    static func update(_ setting: Setting, to value: JSON?) throws {
        var raw = try readRaw().filter { !retired.contains($0.key) }
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
        try JSON.object(raw).encodedLine().write(to: url, options: .atomic)
    }
}

/// The `config …` commands, on settings.json: the CLI runs them without the app, the control
/// service (socket and FFI) runs them for front ends.
nonisolated extension CommandID.Local {
    func run(_ args: [String: JSON]) throws -> JSON {
        func setting() throws -> Setting {
            do { return try Setting.named(args["key"]?.string ?? "") } catch { throw ControlError.usage(error.localizedDescription) }
        }
        switch self {
        case .configList:
            let (values, issues) = SettingsFile.read()
            return ["file": .string(SettingsFile.url.path), "settings": .array(Setting.allCases.map { $0.entry(values) }),
                    "issues": .strings(issues)]
        case .configGet:
            return try setting().entry()
        case .configSet:
            let setting = try setting()
            try SettingsFile.update(setting, to: try setting.parse(args["value"]?.string ?? ""))
            return setting.entry()
        case .configReset:
            let setting = try setting()
            try SettingsFile.update(setting, to: nil)
            return setting.entry()
        case .configAdd, .configRemove:
            let setting = try setting()
            guard case .addresses = setting.kind else { throw ControlError.usage("\(setting.rawValue) is not a list setting.") }
            let words = (args["values"]?.strings ?? []).flatMap { $0.split(separator: ",").map(String.init) }
            let given = try setting.validate(.strings(words)).strings ?? []
            let current = setting.value(in: SettingsFile.read().values).strings ?? []
            let next = self == .configAdd ? current + given : current.filter { !given.contains($0) }
            try SettingsFile.update(setting, to: .strings(next))
            return setting.entry()
        case .configSchema:
            return Setting.fileSchema
        case .configPath:
            return ["path": .string(SettingsFile.url.path), "exists": .bool(FileManager.default.fileExists(atPath: SettingsFile.url.path))]
        }
    }
}

nonisolated extension Setting {
    /// The setting as `config` commands answer it; `values` defaults to the file as it is now.
    func entry(_ values: [Setting: JSON] = SettingsFile.read().values) -> JSON {
        var out: [String: JSON] = [
            "key": .string(rawValue),
            "value": value(in: values),
            "default": defaultValue,
            "changed": .bool(values[self] != nil),
            "summary": .string(summary),
        ]
        switch kind {
        case .bool: out["type"] = "boolean"
        case .int(let range):
            out["type"] = "integer"
            out["minimum"] = .int(range.lowerBound)
            out["maximum"] = .int(range.upperBound)
        case .string: out["type"] = "string"
        case .choice(let words):
            out["type"] = "string"
            out["choices"] = .strings(words)
        case .addresses: out["type"] = "array of email addresses"
        }
        return .object(out)
    }
}

/// The settings in effect, following settings.json as it changes. Every value lives in `values`,
/// so an observer reading a setting through `Observations` hears of any edit of the file.
@Observable
final class Settings {
    static let shared = Settings()

    private(set) var values: [Setting: JSON]
    /// Problems with the file as last read: unknown keys, invalid values, broken JSON.
    private(set) var issues: [String]
    @ObservationIgnored private var watcher: Task<Void, Never>?

    private init() {
        Self.migrateUserDefaults()
        (values, issues) = SettingsFile.read()
    }

    subscript(_ setting: Setting) -> JSON { setting.value(in: values) }
    func bool(_ setting: Setting) -> Bool { self[setting].bool ?? setting.defaultValue.bool ?? false }
    func int(_ setting: Setting) -> Int { self[setting].int ?? setting.defaultValue.int ?? 0 }
    func string(_ setting: Setting) -> String { self[setting].string ?? "" }
    func strings(_ setting: Setting) -> [String] { self[setting].strings ?? [] }

    /// Writes the file and applies the change at once; nil resets to the default.
    func set(_ setting: Setting, _ value: JSON?) throws {
        try SettingsFile.update(setting, to: value)
        reload()
    }

    /// Follows the file however it is edited: saves that replace it and creation or deletion (the
    /// directory), writes in place (the file). Each round watches afresh before reading, so a
    /// replaced file is followed and no edit falls between the read and the watch.
    func startWatching() {
        guard watcher == nil else { return }
        try? FileManager.default.createDirectory(at: SettingsFile.directory, withIntermediateDirectories: true)
        watcher = Task.immediate(name: "Settings file") { [weak self] in
            while !Task.isCancelled {
                let changes = FileChanges.stream([(SettingsFile.directory.path, [.write, .rename]),
                                                  (SettingsFile.url.path, [.write, .extend, .delete, .rename])])
                self?.reload()
                for await _ in changes { break }
            }
        }
    }

    func reload() {
        let (next, nextIssues) = SettingsFile.read()
        if nextIssues != issues { issues = nextIssues }
        if next != values { values = next }
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

/// File-system events as an async sequence: one element per burst of changes to any of the
/// watched paths (those that exist), until iteration stops.
private nonisolated enum FileChanges {
    /// Callback boundary: a vnode watch has no async API, so a DispatchSource's handler only yields.
    static func stream(_ watches: [(path: String, events: DispatchSource.FileSystemEvent)]) -> AsyncStream<Void> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let sources = watches.compactMap { path, events -> (any DispatchSourceFileSystemObject)? in
                let descriptor = open(path, O_EVTONLY)
                guard descriptor >= 0 else { return nil }
                let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: descriptor, eventMask: events)
                source.setEventHandler { continuation.yield() }
                source.setCancelHandler { close(descriptor) }
                return source
            }
            continuation.onTermination = { _ in sources.forEach { $0.cancel() } }
            sources.forEach { $0.resume() }
        }
    }
}
