import Foundation

/// Classifies conversations into Smart Inbox categories with the user's `omp` CLI, using its
/// "smol" model role in a one-shot, tool-less run.
actor OmpTagger {
    /// One conversation as the model sees it.
    nonisolated struct Item: Sendable {
        /// Opaque key echoed back in the result.
        var id: String
        var senderName: String
        var senderEmail: String
        /// The user is a direct To recipient, not cc'd or reached through a list.
        var userInTo: Bool
        var subject: String
        /// The message's one-line snippet; empty when not loaded yet. Cut to `maxSnippet` here.
        var snippet: String
        var messageCount: Int
        var userReplied: Bool
        var latestFromUser: Bool
    }

    nonisolated struct Failure: LocalizedError, Sendable {
        let message: String
        var errorDescription: String? { message }
    }

    private static let maxSnippet = 200
    private static let maxLabelLength = 16

    private let tools: ToolEnvironment
    private let workingDirectory: URL
    private var executable: URL?

    init(tools: ToolEnvironment, workingDirectory: URL) {
        self.tools = tools
        self.workingDirectory = workingDirectory
    }

    /// Tags keyed by `Item.id`. Items the model skipped or answered invalidly are absent.
    func tag(_ items: [Item]) async throws -> [String: ConversationTag] {
        guard !items.isEmpty else { return [:] }
        let executable = try await resolvedExecutable()
        let output = try await ProcessRunner.run(executable: executable,
                                                 arguments: Self.arguments(prompt: Self.prompt(items)),
                                                 environment: await tools.environment,
                                                 currentDirectory: workingDirectory)
        guard output.status == 0 else {
            let line = String(decoding: output.stderr, as: UTF8.self)
                .split(whereSeparator: \.isNewline)
                .last { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
                .map { String($0.trimmingCharacters(in: .whitespaces).prefix(160)) }
            throw Failure(message: line ?? "omp exited with status \(output.status)")
        }
        return try Self.parse(output.stdout, items: items)
    }

    // MARK: Executable

    private func resolvedExecutable() async throws -> URL {
        if let executable { return executable }
        guard let found = await Self.locate(tools: tools) else { throw Failure(message: "omp not found") }
        executable = found
        return found
    }

    /// `MAILSTREAM_OMP`, else the PATH candidate reporting the highest `--version`.
    @concurrent
    private static func locate(tools: ToolEnvironment) async -> URL? {
        let environment = await tools.environment
        if let override = environment["MAILSTREAM_OMP"], FileManager.default.isExecutableFile(atPath: override) {
            return URL(fileURLWithPath: override)
        }
        let candidates = await tools.executables(named: "omp")
        guard !candidates.isEmpty else { return nil }
        let versions = await withTaskGroup { group in
            for (index, candidate) in candidates.enumerated() {
                group.addTask {
                    let output = await ToolEnvironment.runQuickly(candidate, ["--version"], environment: environment)
                    return (index, output.flatMap(version(in:)))
                }
            }
            var versions = [[Int]?](repeating: nil, count: candidates.count)
            for await (index, version) in group { versions[index] = version }
            return versions
        }
        // Highest version wins; ties and unversioned candidates keep PATH order.
        let best = candidates.indices.max { a, b in
            let va = versions[a] ?? [], vb = versions[b] ?? []
            return va == vb ? a > b : va.lexicographicallyPrecedes(vb)
        }
        return best.map { URL(fileURLWithPath: candidates[$0]) }
    }

    private static func version(in output: String) -> [Int]? {
        guard let match = output.firstMatch(of: /omp\/(\d+)\.(\d+)\.(\d+)/) else { return nil }
        return [match.1, match.2, match.3].compactMap { Int($0) }
    }

    // MARK: Prompt

    private static func arguments(prompt: String) -> [String] {
        ["-p", "--no-session", "--no-tools", "--no-skills", "--no-rules", "--no-context-files",
         "--no-extensions", "--no-lsp", "--no-title", "--thinking", "off", "--model", "smol",
         "--max-time", "120", "--system-prompt", systemPrompt, prompt]
    }

    private static let systemPrompt = """
        You sort email conversations for a mail client. The user message is a JSON array with one object \
        per conversation: id; from (sender of the latest incoming message); toMe (true if the user is a \
        direct To recipient, false if cc'd or reached via a list); subject; snippet (start of that message, \
        may be absent); count (messages in thread); replied (the user already replied in the thread); \
        lastFromMe (the latest message was sent by the user).

        Reply with ONLY a JSON array, no prose or code fences, one object per input id:
        {"id":"<id>","category":"<category>","toRespond":<bool>,"labels":[<string>...]}

        category, exactly one of:
        - people: written by a human to the user or a small group: colleagues, friends, family, a \
        recruiter or support agent writing personally. Human-written mail is people even from a company \
        address or when cc'd.
        - notification: automated transactional or service mail: receipts, orders, shipping, bills, bank \
        and security alerts, codes, password resets, account notices, CI/GitHub/issue-tracker activity, \
        social-network alerts.
        - newsletter: bulk content or marketing: newsletters, digests, promotions, product announcements, \
        surveys, mass mailing-list bulletins.
        - invitation: calendar invitations, event RSVPs and their responses or updates.

        toRespond: true when the latest incoming message is from a human who directly asks the user a \
        question or makes a request (send, review, confirm, decide, attend) and lastFromMe is false; an \
        earlier reply (replied) does not answer a newer ask. False when lastFromMe is true, for FYI mail, \
        and for every non-people category.

        labels: 0-2 English Title Case topic words, each at most 16 characters, e.g. "Finance", "Travel", \
        "Work", "Shopping", "Security", "Health", "Education", "Tech". Prefer broad, reusable topics; use [] \
        when unclear.
        """

    private nonisolated struct PromptItem: Encodable {
        let id: String
        let from: String
        let toMe: Bool
        let subject: String
        let snippet: String?
        let count: Int
        let replied: Bool
        let lastFromMe: Bool
    }

    /// Items are numbered from 1 so the model echoes short ids; `parse` maps them back.
    private static func prompt(_ items: [Item]) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let lines = items.enumerated().compactMap { index, item -> String? in
            let name = item.senderName.trimmingCharacters(in: .whitespacesAndNewlines)
            let from = name.isEmpty || name == item.senderEmail ? item.senderEmail : "\(name) <\(item.senderEmail)>"
            let snippet = item.snippet.prefix(maxSnippet)
            let entry = PromptItem(id: String(index + 1), from: from, toMe: item.userInTo, subject: item.subject,
                                   snippet: snippet.isEmpty ? nil : String(snippet), count: item.messageCount,
                                   replied: item.userReplied, lastFromMe: item.latestFromUser)
            return (try? encoder.encode(entry)).map { String(decoding: $0, as: UTF8.self) }
        }
        return "[\n" + lines.joined(separator: ",\n") + "\n]"
    }

    // MARK: Parsing

    private static func parse(_ data: Data, items: [Item]) throws -> [String: ConversationTag] {
        let text = String(decoding: data, as: UTF8.self)
        guard let start = text.firstIndex(of: "["), let end = text.lastIndex(of: "]"), start < end,
              let array = try? JSONSerialization.jsonObject(with: Data(text[start...end].utf8)) as? [Any]
        else {
            let preview = text.trimmingCharacters(in: .whitespacesAndNewlines).prefix(80)
            throw Failure(message: preview.isEmpty ? "omp returned nothing" : "unexpected omp output: \(preview)")
        }
        var result: [String: ConversationTag] = [:]
        for case let object as [String: Any] in array {
            let rawID = (object["id"] as? String) ?? (object["id"] as? NSNumber)?.stringValue
            guard let number = rawID.flatMap({ Int($0.trimmingCharacters(in: .whitespaces)) }),
                  items.indices.contains(number - 1),
                  let category = (object["category"] as? String).flatMap(category(named:))
            else { continue }
            let item = items[number - 1]
            let toRespond = (object["toRespond"] as? Bool) ?? false
            var labels: [String] = []
            for case let raw as String in (object["labels"] as? [Any]) ?? [] {
                let label = titleCase(raw)
                guard !label.isEmpty, label.count <= maxLabelLength, !labels.contains(label) else { continue }
                labels.append(label)
                if labels.count == 2 { break }
            }
            result[item.id] = ConversationTag(category: category,
                                              toRespond: toRespond && category == .people && !item.latestFromUser,
                                              labels: labels)
        }
        return result
    }

    private static func category(named raw: String) -> MailCategory? {
        let name = raw.trimmingCharacters(in: .whitespaces).lowercased()
        return MailCategory(rawValue: name) ?? (name.hasSuffix("s") ? MailCategory(rawValue: String(name.dropLast())) : nil)
    }

    private static func titleCase(_ raw: String) -> String {
        raw.split(whereSeparator: \.isWhitespace)
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}
