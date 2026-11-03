import Foundation

/// `Mailstream <command> …`: the binary run from a shell instead of launched as an app. Settings
/// commands work on settings.json directly; the rest go to the running app's control socket,
/// starting the app in the background when it is not running.
enum CLI {
    enum Exit: Int32 {
        case ok = 0
        case failure = 1
        case usage = 2
        case unavailable = 3
    }

    /// Whether `arguments` (without the program name) ask for the CLI rather than the app.
    /// LaunchServices and Xcode pass `-psn_…` and `-NS…`/`-Apple…` flags to apps.
    static func wants(_ arguments: [String]) -> Bool {
        guard let first = arguments.first else { return false }
        return !(first.hasPrefix("-psn") || first.hasPrefix("-NS") || first.hasPrefix("-Apple"))
    }

    static func run(_ arguments: [String]) -> Int32 {
        do {
            let result = try execute(arguments)
            print(result)
            return Exit.ok.rawValue
        } catch let failure as Failure {
            FileHandle.standardError.write(failure.error.json.encoded() + Data("\n".utf8))
            return failure.exit.rawValue
        } catch {
            FileHandle.standardError.write(ControlError("failed", error.localizedDescription).json.encoded() + Data("\n".utf8))
            return Exit.failure.rawValue
        }
    }

    private struct Failure: Error {
        let error: ControlError
        let exit: Exit
    }

    private static func usage(_ message: String) -> Failure { Failure(error: ControlError("usage", message), exit: .usage) }

    /// The text to print on success.
    private static func execute(_ arguments: [String]) throws -> String {
        if arguments.isEmpty || ["help", "--help", "-h"].contains(arguments[0]) {
            let rest = Array(arguments.dropFirst())
            if rest.isEmpty { return generalHelp }
            guard let (spec, _) = match(rest) else { throw usage("Unknown command \"\(rest.joined(separator: " "))\". `mailstream help` lists commands.") }
            return spec.help
        }
        if arguments == ["commands"] {
            return text(.array(CommandSpec.all.map(\.json)))
        }
        guard let (spec, rest) = match(arguments) else {
            throw usage("Unknown command \"\(arguments.joined(separator: " "))\". `mailstream help` lists commands.")
        }
        if rest.contains("--help") || rest.contains("-h") { return spec.help }
        let args = try parse(rest, for: spec)
        let result = spec.local ? try runLocal(spec, args) : try send(spec, args)
        if let failed = result["failed"]?.array, !failed.isEmpty {
            print(text(result))
            throw Failure(error: ControlError("partial", "\(failed.count) item\(failed.count == 1 ? "" : "s") failed; see \"failed\"."),
                          exit: .failure)
        }
        return text(result)
    }

    private static func text(_ json: JSON) -> String { String(decoding: json.encoded(), as: UTF8.self) }

    private static var generalHelp: String {
        var groups: [(String, [CommandSpec])] = []
        for spec in CommandSpec.all {
            let group = spec.name.split(separator: " ").first.map(String.init) ?? spec.name
            if let index = groups.firstIndex(where: { $0.0 == group }) { groups[index].1.append(spec) } else { groups.append((group, [spec])) }
        }
        var lines = [CommandSpec.overview, "", "Commands:"]
        for (_, specs) in groups {
            for spec in specs { lines.append("  \(spec.name.padding(toLength: 18, withPad: " ", startingAt: 0)) \(spec.summary)") }
        }
        return lines.joined(separator: "\n")
    }

    /// The command whose name is the longest prefix of `words`, and the words after it.
    private static func match(_ words: [String]) -> (CommandSpec, [String])? {
        var best: (spec: CommandSpec, length: Int)?
        for spec in CommandSpec.all {
            let name = spec.name.split(separator: " ").map(String.init)
            guard words.count >= name.count, Array(words.prefix(name.count)) == name else { continue }
            if name.count > (best?.length ?? 0) { best = (spec, name.count) }
        }
        guard let best else { return nil }
        return (best.spec, Array(words.dropFirst(best.length)))
    }

    // MARK: - Parsing

    private static func parse(_ words: [String], for spec: CommandSpec) throws -> [String: JSON] {
        var values: [String: JSON] = [:]
        var positionals = spec.arguments.filter(\.positional)
        var index = 0
        func append(_ argument: CommandSpec.Argument, _ raw: String) throws {
            let value = try convert(raw, argument, spec)
            if case .list = argument.kind, case .array(let items) = value {
                values[argument.name] = .array((values[argument.name]?.array ?? []) + items)
            } else {
                values[argument.name] = value
            }
        }
        while index < words.count {
            let word = words[index]
            index += 1
            if word.hasPrefix("--"), word.count > 2 {
                var name = String(word.dropFirst(2))
                var inline: String?
                if let equals = name.firstIndex(of: "=") {
                    inline = String(name[name.index(after: equals)...])
                    name = String(name[..<equals])
                }
                var negated = false
                if spec.arguments.first(where: { $0.name == name && !$0.positional }) == nil, name.hasPrefix("no-") {
                    name = String(name.dropFirst(3))
                    negated = true
                }
                guard let argument = spec.arguments.first(where: { $0.name == name && !$0.positional }) else {
                    throw usage("\(spec.name) has no option --\(name). `mailstream help \(spec.name)` lists its options.")
                }
                if case .bool = argument.kind {
                    if let inline {
                        values[name] = try convert(inline, argument, spec)
                    } else {
                        values[name] = .bool(!negated)
                    }
                    continue
                }
                guard !negated else { throw usage("--no-\(name) only works for on/off options.") }
                if let inline {
                    try append(argument, inline)
                } else {
                    guard index < words.count else { throw usage("--\(name) needs a value.") }
                    try append(argument, words[index])
                    index += 1
                }
            } else {
                guard let argument = positionals.first else {
                    throw usage("Unexpected \"\(word)\". `mailstream help \(spec.name)` shows the usage.")
                }
                try append(argument, word)
                if !argument.variadic { positionals.removeFirst() }
            }
        }
        for argument in spec.arguments where argument.required && values[argument.name] == nil {
            let label = argument.positional ? "<\(argument.name)>" : "--\(argument.name)"
            throw usage("\(spec.name): \(label) is required. `mailstream help \(spec.name)` shows the usage.")
        }
        return values
    }

    private static func convert(_ raw: String, _ argument: CommandSpec.Argument, _ spec: CommandSpec) throws -> JSON {
        switch argument.kind {
        case .string:
            if raw == "-" && argument.name == "body" {
                let data = FileHandle.standardInput.readDataToEndOfFile()
                return .string(String(decoding: data, as: UTF8.self))
            }
            return .string(raw)
        case .int:
            guard let value = Int(raw) else { throw usage("--\(argument.name) must be a whole number, not \"\(raw)\".") }
            return .int(value)
        case .bool:
            switch raw.lowercased() {
            case "true", "yes", "on", "1": return true
            case "false", "no", "off", "0": return false
            default: throw usage("--\(argument.name) must be true or false, not \"\(raw)\".")
            }
        case .list:
            // Settings values and conversation ids are taken whole; recipient and label lists split on commas.
            if argument.positional { return .strings([raw]) }
            return .strings(raw.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty })
        case .choice(let choices):
            guard choices.contains(raw) else {
                throw usage("\(argument.positional ? argument.name : "--" + argument.name) must be one of \(choices.joined(separator: ", ")), not \"\(raw)\".")
            }
            return .string(raw)
        }
    }

    // MARK: - Settings, without the app

    private static func runLocal(_ spec: CommandSpec, _ args: [String: JSON]) throws -> JSON {
        func setting() throws -> Setting {
            do { return try Setting.named(args["key"]?.string ?? "") } catch { throw usage(error.localizedDescription) }
        }
        do {
            switch spec.id {
            case "config.list":
                let (values, issues) = SettingsFile.read()
                return ["file": .string(SettingsFile.url.path), "settings": .array(Setting.allCases.map { entry($0, values) }),
                        "issues": .strings(issues)]
            case "config.get":
                return entry(try setting(), SettingsFile.read().values)
            case "config.set":
                let setting = try setting()
                try SettingsFile.update(setting, to: try setting.parse(args["value"]?.string ?? ""))
                return entry(setting, SettingsFile.read().values)
            case "config.reset":
                let setting = try setting()
                try SettingsFile.update(setting, to: nil)
                return entry(setting, SettingsFile.read().values)
            case "config.add", "config.remove":
                let setting = try setting()
                guard case .addresses = setting.kind else { throw usage("\(setting.rawValue) is not a list setting.") }
                let words = (args["values"]?.strings ?? []).flatMap { $0.split(separator: ",").map(String.init) }
                let given = try setting.validate(.strings(words)).strings ?? []
                let current = SettingsFile.read().values[setting]?.strings ?? setting.defaultValue.strings ?? []
                let next = spec.id == "config.add" ? current + given : current.filter { !given.contains($0) }
                try SettingsFile.update(setting, to: .strings(next))
                return entry(setting, SettingsFile.read().values)
            case "config.schema":
                return Setting.fileSchema
            case "config.path":
                return ["path": .string(SettingsFile.url.path), "exists": .bool(FileManager.default.fileExists(atPath: SettingsFile.url.path))]
            default:
                throw usage("\(spec.name) is not a local command.")
            }
        } catch let error as SettingsError {
            throw Failure(error: ControlError("invalid_setting", error.message), exit: .failure)
        }
    }

    private static func entry(_ setting: Setting, _ values: [Setting: JSON]) -> JSON {
        let type = switch setting.kind {
        case .bool: "boolean"
        case .int: "integer"
        case .string: "string"
        case .addresses: "array of email addresses"
        }
        return [
            "key": .string(setting.rawValue),
            "value": values[setting] ?? setting.defaultValue,
            "default": setting.defaultValue,
            "changed": .bool(values[setting] != nil),
            "type": .string(type),
            "summary": .string(setting.summary),
        ]
    }

    // MARK: - The app

    private static func send(_ spec: CommandSpec, _ args: [String: JSON]) throws -> JSON {
        let fd = try connectOrLaunch()
        defer { close(fd) }
        var timeout = timeval(tv_sec: 420, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var noSigPipe: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
        let request: JSON = ["command": .string(spec.id), "args": .object(args)]
        guard ControlSocket.writeAll(fd, request.encoded(pretty: false) + Data("\n".utf8)) else {
            throw Failure(error: ControlError("unavailable", "Could not write to Mailstream's control socket."), exit: .unavailable)
        }
        shutdown(fd, SHUT_WR)
        guard let data = ControlSocket.readMessage(fd, limit: 256 << 20), !data.isEmpty,
              let answer = try? JSON.decode(data) else {
            throw Failure(error: ControlError("unavailable", "Mailstream closed the connection without answering."), exit: .unavailable)
        }
        if answer["ok"]?.bool == true { return answer["result"] ?? .null }
        let code = answer["error"]?["code"]?.string ?? "failed"
        let message = answer["error"]?["message"]?.string ?? "Mailstream reported a failure."
        throw Failure(error: ControlError(code, message), exit: code == "usage" ? .usage : .failure)
    }

    /// A connection to the running app; starts the app (in the background) when it is not running.
    private static func connectOrLaunch() throws -> Int32 {
        if let fd = ControlSocket.connect() { return fd }
        guard let app = appBundle() else {
            throw Failure(error: ControlError("not_running", "Mailstream is not running, and this binary is not inside Mailstream.app to start it."),
                          exit: .unavailable)
        }
        let open = Process()
        open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        open.arguments = ["-g", app.path]
        open.standardOutput = FileHandle.nullDevice
        open.standardError = FileHandle.nullDevice
        do {
            try open.run()
            open.waitUntilExit()
        } catch {
            throw Failure(error: ControlError("not_running", "Could not start \(app.path): \(error.localizedDescription)"), exit: .unavailable)
        }
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            if let fd = ControlSocket.connect() { return fd }
            usleep(200_000)
        }
        throw Failure(error: ControlError("not_running", "Started \(app.path), but it did not open its control socket within 20 s."),
                      exit: .unavailable)
    }

    /// The .app this executable lives in, following a symlink such as ~/.local/bin/mailstream.
    private static func appBundle() -> URL? {
        var url = Bundle.main.executableURL?.resolvingSymlinksInPath()
            ?? URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
        while url.path != "/" {
            url.deleteLastPathComponent()
            if url.pathExtension == "app" { return url }
        }
        return nil
    }
}
