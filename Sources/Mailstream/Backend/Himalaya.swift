import Foundation

struct HimalayaError: LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }
}

enum RequestPriority: Sendable {
    case interactive, background
}

/// The environment external helpers need: the token commands in himalaya's config (ortie,
/// `security`) and omp. A GUI app inherits launchd's minimal PATH, so the user's login-shell PATH
/// is recovered once at startup.
struct ToolEnvironment: Sendable {
    let environment: [String: String]

    static func resolve() -> ToolEnvironment {
        var env = ProcessInfo.processInfo.environment
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        var dirs: [String] = []
        if let loginPath = loginShellPath() {
            dirs += loginPath.split(separator: ":").map(String.init)
        }
        dirs += ["\(home)/.cargo/bin", "\(home)/.local/bin", "/opt/homebrew/bin", "/usr/local/bin",
                 "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        dirs += (env["PATH"] ?? "").split(separator: ":").map(String.init)
        var seen = Set<String>()
        dirs = dirs.filter { !$0.isEmpty && seen.insert($0).inserted }
        env["PATH"] = dirs.joined(separator: ":")
        env["NO_COLOR"] = "1"
        env.removeValue(forKey: "RUST_LOG")
        return ToolEnvironment(environment: env)
    }

    /// Executable files called `name` on the recovered PATH, in PATH order, one per resolved target.
    func executables(named name: String) -> [String] {
        let fm = FileManager.default
        var seen = Set<String>()
        return (environment["PATH"] ?? "").split(separator: ":")
            .map { "\($0)/\(name)" }
            .filter { fm.isExecutableFile(atPath: $0)
                && seen.insert(URL(fileURLWithPath: $0).resolvingSymlinksInPath().path).inserted }
    }

    /// Runs a short-lived command, giving up after three seconds.
    static func runQuickly(_ executable: String, _ arguments: [String], environment: [String: String]? = nil) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        if let environment { process.environment = environment }
        let out = Pipe()
        process.standardOutput = out
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return nil }
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { process.waitUntilExit(); done.signal() }
        if done.wait(timeout: .now() + 3) == .timedOut {
            process.terminate()
            return nil
        }
        return String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    }

    private static func loginShellPath() -> String? {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let text = runQuickly(shell, ["-l", "-c", "printf '__MS_PATH__%s__MS_PATH__' \"$PATH\""]) ?? ""
        let parts = text.components(separatedBy: "__MS_PATH__")
        return parts.count >= 3 ? parts[1] : nil
    }
}

// MARK: - JSON shapes of `himalaya --json`

private struct ErrorDTO: Decodable { let error: String }

struct AccountDTO: Decodable, Sendable {
    let name: String
    let `default`: Bool
    let backends: [String]
}

private struct AccountsDTO: Decodable { let accounts: [AccountDTO] }

struct MailboxDTO: Decodable, Sendable {
    let id: String
    let name: String
    let role: String?
    let total: Int?
    let unread: Int?
}

private struct MailboxesDTO: Decodable { let mailboxes: [MailboxDTO] }

struct ImapMailboxDTO: Decodable, Sendable {
    let name: String
    let attributes: [String]
}

private struct ImapMailboxesDTO: Decodable { let mailboxes: [ImapMailboxDTO] }

private struct ComposeDTO: Decodable { let from: String? }

private struct FlagDTO: Decodable {
    let raw: String
    let iana: String?
}

private struct EnvelopeDTO: Decodable {
    let id: String
    let messageID: String?
    let inReplyTo: [String]?
    let flags: [FlagDTO]?
    let subject: String?
    let from: [Address]?
    let to: [Address]?
    let date: String?
    let size: Int?
    let hasAttachment: Bool?

    enum CodingKeys: String, CodingKey {
        case id, flags, subject, from, to, date, size
        case messageID = "message-id"
        case inReplyTo = "in-reply-to"
        case hasAttachment = "has-attachment"
    }

    var envelope: Envelope {
        Envelope(
            id: id,
            messageID: messageID.map(normalizeMessageID),
            inReplyTo: (inReplyTo ?? []).map(normalizeMessageID),
            flags: Set((flags ?? []).map { $0.iana ?? $0.raw.lowercased() }),
            subject: subject ?? "",
            from: from ?? [],
            to: to ?? [],
            date: date.flatMap(parseISODate),
            size: size ?? 0,
            hasAttachment: hasAttachment)
    }
}

private struct EnvelopesDTO: Decodable { let envelopes: [EnvelopeDTO] }

func normalizeMessageID(_ raw: String) -> String {
    raw.trimmingCharacters(in: CharacterSet(charactersIn: "<> \t\r\n")).lowercased()
}

func parseISODate(_ raw: String) -> Date? {
    if let date = try? Date(raw, strategy: .iso8601) { return date }
    return try? Date(raw, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
}

/// Typed wrapper over the himalaya engine linked into the app (`Engine/`). A call is one himalaya
/// command line, run in-process over the account's pooled connection; per-account semaphores
/// bound concurrency so background prefetch never starves interactive reads.
final class Himalaya: Sendable {
    private let limiters: Limiters

    init(tools: ToolEnvironment) {
        MailEngine.start(environment: tools.environment)
        self.limiters = Limiters()
    }

    struct ComposeFields: Sendable {
        var from: String?
        var to: [String]
        var cc: [String]
        var bcc: [String]
        var subject: String
        var body: String
        var attachments: [URL]
    }

    enum ComposeMode: Sendable {
        case new
        case reply(MessageRef)
        case forward(MessageRef)
    }

    // MARK: Queries

    func accounts() async throws -> [AccountDTO] {
        try await decode(AccountsDTO.self, ["account", "list"], account: nil).accounts
    }

    /// The account's sender identity, read from a dry-run compose which fills `from` from config.
    func identity(account: String) async throws -> Address? {
        let dto = try await decode(ComposeDTO.self,
                                   ["message", "compose", "--to", "probe@invalid", "--subject", "", "--body", ""],
                                   account: account)
        return dto.from.flatMap(Address.parse)
    }

    func mailboxes(account: String, counts: Bool) async throws -> [MailboxDTO] {
        var args = ["mailbox", "list"]
        if counts { args.append("--counts") }
        return try await decode(MailboxesDTO.self, args, account: account, priority: counts ? .background : .interactive)
            .mailboxes
    }

    func imapMailboxes(account: String) async throws -> [ImapMailboxDTO] {
        try await decode(ImapMailboxesDTO.self, ["imap", "list"], account: account).mailboxes
    }

    func envelopes(account: String, mailbox: String, page: Int, pageSize: Int,
                   priority: RequestPriority = .interactive) async throws -> [Envelope] {
        try await decode(EnvelopesDTO.self,
                         ["envelope", "list", "-m", mailbox, "-p", String(page), "-s", String(pageSize)],
                         account: account, priority: priority)
            .envelopes.map(\.envelope)
    }

    func search(account: String, mailbox: String, query: String, pageSize: Int) async throws -> [Envelope] {
        try await decode(EnvelopesDTO.self,
                         ["envelope", "search", "-m", mailbox, "-s", String(pageSize), query],
                         account: account)
            .envelopes.map(\.envelope)
    }

    /// The mail-parser JSON of one message. Does not touch `\Seen`.
    func readMessage(_ ref: MessageRef, priority: RequestPriority) async throws -> Data {
        try await run(["message", "read", "-m", ref.mailbox, ref.id], account: ref.account, priority: priority)
    }

    // MARK: Mutations

    struct Draft: Decodable, Sendable {
        let from: String?
        let to: [String]
        let cc: [String]
        let subject: String?
        let body: String
    }

    /// Dry-runs `message reply` / `message forward` for the derived subject, recipients and quoted
    /// body, so the composer can show and edit the quote before sending with `--posting-style none`.
    func draft(mode: ComposeMode, account: String) async throws -> Draft? {
        var args: [String]
        switch mode {
        case .new: return nil
        case .reply(let ref): args = ["message", "reply", "-m", ref.mailbox, ref.id]
        case .forward(let ref): args = ["message", "forward", "-m", ref.mailbox, ref.id]
        }
        args += ["--body", "", "--signature", ""]
        return try await decode(Draft.self, args, account: account)
    }

    func addFlags(_ flags: [String], ids: [String], account: String, mailbox: String) async throws {
        guard !ids.isEmpty else { return }
        _ = try await run(["flag", "add", "-m", mailbox] + flags.flatMap { ["--flag", $0] } + ids, account: account)
    }

    func removeFlags(_ flags: [String], ids: [String], account: String, mailbox: String) async throws {
        guard !ids.isEmpty else { return }
        _ = try await run(["flag", "remove", "-m", mailbox] + flags.flatMap { ["--flag", $0] } + ids, account: account)
    }

    func move(ids: [String], account: String, from: String, to: String) async throws {
        guard !ids.isEmpty else { return }
        _ = try await run(["message", "move", "--from", from, "--to", to] + ids, account: account)
    }

    func delete(ids: [String], account: String, mailbox: String) async throws {
        guard !ids.isEmpty else { return }
        _ = try await run(["message", "delete", "-m", mailbox] + ids, account: account)
    }

    func send(_ fields: ComposeFields, mode: ComposeMode, account: String) async throws {
        let bodyFile = FileManager.default.temporaryDirectory
            .appendingPathComponent("mailstream-body-\(UUID().uuidString).txt")
        try Data(fields.body.utf8).write(to: bodyFile)
        defer { try? FileManager.default.removeItem(at: bodyFile) }

        var args: [String]
        switch mode {
        case .new: args = ["message", "compose"]
        case .reply(let ref): args = ["message", "reply", "-m", ref.mailbox, ref.id, "--posting-style", "none"]
        case .forward(let ref): args = ["message", "forward", "-m", ref.mailbox, ref.id, "--posting-style", "none"]
        }
        if let from = fields.from { args += ["--from", from] }
        for to in fields.to { args += ["--to", to] }
        for cc in fields.cc { args += ["--cc", cc] }
        for bcc in fields.bcc { args += ["--bcc", bcc] }
        args += ["--subject", fields.subject, "--body-file", bodyFile.path]
        for url in fields.attachments { args += ["--attach", url.path] }
        args.append("--send")
        _ = try await run(args, account: account)
    }

    /// Sends a reply with an HTML alternative. himalaya composes the message in a dry run (threading
    /// headers, `Re:` subject, encoded addresses, the plain body and any configured signature, which
    /// stays plain-only); the HTML part is spliced in before `message send`. A dry run that is not a
    /// single text part falls back to the plain send.
    func sendReply(_ fields: ComposeFields, html: String, to ref: MessageRef, account: String) async throws {
        let directory = FileManager.default.temporaryDirectory
        let bodyFile = directory.appendingPathComponent("mailstream-body-\(UUID().uuidString).txt")
        let messageFile = directory.appendingPathComponent("mailstream-message-\(UUID().uuidString).eml")
        try Data(fields.body.utf8).write(to: bodyFile)
        defer {
            try? FileManager.default.removeItem(at: bodyFile)
            try? FileManager.default.removeItem(at: messageFile)
        }
        var args = ["message", "reply", "-m", ref.mailbox, ref.id, "--posting-style", "none"]
        if let from = fields.from { args += ["--from", from] }
        for to in fields.to { args += ["--to", to] }
        for cc in fields.cc { args += ["--cc", cc] }
        args += ["--subject", fields.subject, "--body-file", bodyFile.path]
        let raw = try await run(args, account: account, json: false)
        guard let message = Self.addingHTML(html, to: raw) else {
            return try await send(fields, mode: .reply(ref), account: account)
        }
        try message.write(to: messageFile)
        _ = try await run(["message", "send", "--", messageFile.path], account: account)
    }

    /// Turns a single-part `text/plain` message into `multipart/alternative` with `html` added,
    /// keeping every other header and the plain part byte for byte. Nil for any other shape.
    static func addingHTML(_ html: String, to raw: Data) -> Data? {
        let text = String(decoding: raw, as: UTF8.self)
        let newline = text.contains("\r\n") ? "\r\n" : "\n"
        guard let split = text.range(of: newline + newline) else { return nil }
        var fields: [String] = []
        for line in text[..<split.lowerBound].components(separatedBy: newline) {
            if let first = line.first, first == " " || first == "\t", !fields.isEmpty {
                fields[fields.count - 1] += newline + line
            } else {
                fields.append(line)
            }
        }
        func name(_ field: String) -> String { field.prefix { $0 != ":" }.trimmingCharacters(in: .whitespaces).lowercased() }
        guard let type = fields.first(where: { name($0) == "content-type" }), type.lowercased().contains("text/plain") else {
            return nil
        }
        let partNames: Set<String> = ["content-type", "content-transfer-encoding"]
        let boundary = "mailstream-\(UUID().uuidString)"
        let encoded = Data(html.utf8).base64EncodedString(options: newline == "\r\n"
            ? [.lineLength76Characters, .endLineWithCarriageReturn, .endLineWithLineFeed]
            : [.lineLength76Characters, .endLineWithLineFeed])
        let lines = fields.filter { !partNames.contains(name($0)) && name($0) != "mime-version" }
            + ["MIME-Version: 1.0", "Content-Type: multipart/alternative; boundary=\"\(boundary)\"", "",
               "--\(boundary)"]
            + fields.filter { partNames.contains(name($0)) }
            + ["", String(text[split.upperBound...]), "--\(boundary)",
               "Content-Type: text/html; charset=\"utf-8\"", "Content-Transfer-Encoding: base64", "", encoded,
               "--\(boundary)--", ""]
        return Data(lines.joined(separator: newline).utf8)
    }

    // MARK: Plumbing

    private func decode<T: Decodable>(_ type: T.Type, _ args: [String], account: String?,
                                      priority: RequestPriority = .interactive) async throws -> T {
        let data = try await run(args, account: account, priority: priority)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw HimalayaError(message: "Unexpected himalaya output for `\(args.prefix(2).joined(separator: " "))`: \(error)")
        }
    }

    /// Without `json`, a dry run prints the composed raw message instead of its decoded fields.
    private func run(_ args: [String], account: String?, priority: RequestPriority = .interactive,
                     json: Bool = true) async throws -> Data {
        let full = (json ? ["--json"] : []) + (account.map { ["-a", $0] } ?? []) + args
        let semaphore = await limiters.semaphore(account: account ?? "", priority: priority)
        let output = try await semaphore.withPermit { try await MailEngine.run(full) }
        guard output.status == 0 else {
            if let dto = try? JSONDecoder().decode(ErrorDTO.self, from: output.data) {
                throw HimalayaError(message: dto.error)
            }
            let text = String(decoding: output.data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            throw HimalayaError(message: text.isEmpty ? "himalaya failed with status \(output.status)" : text)
        }
        return output.data
    }
}

private actor Limiters {
    private var interactive: [String: AsyncSemaphore] = [:]
    private var background: [String: AsyncSemaphore] = [:]

    func semaphore(account: String, priority: RequestPriority) -> AsyncSemaphore {
        switch priority {
        case .interactive:
            if let existing = interactive[account] { return existing }
            let created = AsyncSemaphore(4)
            interactive[account] = created
            return created
        case .background:
            if let existing = background[account] { return existing }
            let created = AsyncSemaphore(2)
            background[account] = created
            return created
        }
    }
}
