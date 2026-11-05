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

        /// The status a failure exits with, by its code.
        init(code: String) {
            self = switch code {
            case "usage": .usage
            case "unavailable", "not_running": .unavailable
            default: .failure
            }
        }
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
        } catch {
            let failure = ControlError(error)
            FileHandle.standardError.write(failure.json.encodedLine())
            return Exit(code: failure.code).rawValue
        }
    }

    /// The text to print on success.
    private static func execute(_ arguments: [String]) throws -> String {
        if arguments.isEmpty || ["help", "--help", "-h"].contains(arguments[0]) {
            let rest = Array(arguments.dropFirst())
            if rest.isEmpty { return generalHelp }
            return try match(rest).spec.help
        }
        if arguments == ["commands"] {
            return JSON.array(CommandSpec.all.map(\.json)).text()
        }
        let (spec, rest) = try match(arguments)
        if rest.contains("--help") || rest.contains("-h") { return spec.help }
        let args = try parse(rest, for: spec)
        let result = switch spec.command {
        case .local(let command): try command.run(args)
        case .app: try send(spec, args)
        }
        if let failed = result["failed"]?.array, !failed.isEmpty {
            print(result.text())
            throw ControlError("partial", "\(failed.count) item\(failed.count == 1 ? "" : "s") failed; see \"failed\".")
        }
        return result.text()
    }

    private static var generalHelp: String {
        ([CommandSpec.overview, "", "Commands:"] + CommandSpec.all.map { CommandSpec.row($0.name, $0.summary) }).joined(separator: "\n")
    }

    /// The command whose name is the longest prefix of `words`, and the words after it.
    private static func match(_ words: [String]) throws -> (spec: CommandSpec, rest: [String]) {
        var best: (spec: CommandSpec, length: Int)?
        for spec in CommandSpec.all {
            let name = spec.name.split(separator: " ").map(String.init)
            guard words.count >= name.count, Array(words.prefix(name.count)) == name else { continue }
            if name.count > (best?.length ?? 0) { best = (spec, name.count) }
        }
        guard let best else {
            throw ControlError.usage("Unknown command \"\(words.joined(separator: " "))\". `mailstream help` lists commands.")
        }
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
                    throw ControlError.usage("\(spec.name) has no option --\(name). `mailstream help \(spec.name)` lists its options.")
                }
                if case .bool = argument.kind {
                    if let inline {
                        values[name] = try convert(inline, argument, spec)
                    } else {
                        values[name] = .bool(!negated)
                    }
                    continue
                }
                guard !negated else { throw ControlError.usage("--no-\(name) only works for on/off options.") }
                if let inline {
                    try append(argument, inline)
                } else {
                    guard index < words.count else { throw ControlError.usage("--\(name) needs a value.") }
                    try append(argument, words[index])
                    index += 1
                }
            } else {
                guard let argument = positionals.first else {
                    throw ControlError.usage("Unexpected \"\(word)\". `mailstream help \(spec.name)` shows the usage.")
                }
                try append(argument, word)
                if !argument.variadic { positionals.removeFirst() }
            }
        }
        for argument in spec.arguments where argument.required && values[argument.name] == nil {
            throw ControlError.usage("\(spec.name): \(argument.label) is required. `mailstream help \(spec.name)` shows the usage.")
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
            guard let value = Int(raw) else { throw ControlError.usage("--\(argument.name) must be a whole number, not \"\(raw)\".") }
            return .int(value)
        case .bool:
            guard let value = TypedValue.bool(raw) else { throw ControlError.usage("--\(argument.name) must be true or false, not \"\(raw)\".") }
            return .bool(value)
        case .list:
            // Settings values and conversation ids are taken whole; recipient and label lists split on commas.
            if argument.positional { return .strings([raw]) }
            return .strings(TypedValue.list(raw))
        case .choice(let choices):
            guard choices.contains(raw) else {
                throw ControlError.usage("\(argument.positional ? argument.name : "--" + argument.name) must be one of \(choices.joined(separator: ", ")), not \"\(raw)\".")
            }
            return .string(raw)
        }
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
        guard ControlSocket.writeAll(fd, request.encodedLine(pretty: false)) else {
            throw ControlError("unavailable", "Could not write to Mailstream's control socket.")
        }
        shutdown(fd, SHUT_WR)
        guard let data = ControlSocket.readMessage(fd, limit: ControlSocket.maxAnswer), !data.isEmpty,
              let answer = try? JSON.decode(data) else {
            throw ControlError("unavailable", "Mailstream closed the connection without answering.")
        }
        if answer["ok"]?.bool == true { return answer["result"] ?? .null }
        throw ControlError(answer["error"]?["code"]?.string ?? "failed", answer["error"]?["message"]?.string ?? "Mailstream reported a failure.")
    }

    /// A connection to the running app; starts the app (in the background) when it is not running.
    private static func connectOrLaunch() throws -> Int32 {
        if let fd = ControlSocket.connect() { return fd }
        guard let app = appBundle() else {
            throw ControlError("not_running", "Mailstream is not running, and this binary is not inside Mailstream.app to start it.")
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
            throw ControlError("not_running", "Could not start \(app.path): \(error.localizedDescription)")
        }
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            if let fd = ControlSocket.connect() { return fd }
            usleep(200_000)
        }
        throw ControlError("not_running", "Started \(app.path), but it did not open its control socket within 20 s.")
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
