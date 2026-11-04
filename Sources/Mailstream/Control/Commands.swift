import Foundation

/// The command-line and socket interface: one table both the `mailstream` CLI (parsing, help,
/// `mailstream commands`) and the app's control server (dispatch) read, so they cannot drift.
nonisolated struct CommandSpec: Sendable {
    struct Argument: Sendable {
        enum Kind: Sendable {
            case string
            case int
            case bool
            /// Repeatable, or comma-separated.
            case list
            case choice([String])
        }

        let name: String
        let kind: Kind
        let summary: String
        /// Given by position rather than `--name`.
        var positional = false
        /// A positional that takes every remaining word.
        var variadic = false
        var required = false
        var defaultValue: JSON? = nil
    }

    /// Space-separated words, e.g. `mail list`.
    let name: String
    let summary: String
    var arguments: [Argument] = []
    /// Has effects Mailstream cannot undo (sending, signing out): the request must carry `yes: true`.
    var confirm = false
    /// Runs in the CLI itself on settings.json; the app need not be running.
    var local = false

    /// The request's `command`: `mail.list`.
    var id: String { name.replacingOccurrences(of: " ", with: ".") }

    static func named(_ id: String) -> CommandSpec? { all.first { $0.id == id } }

    static let yes = Argument(name: "yes", kind: .bool, summary: "Confirm; without it nothing is done and the error says what would happen.")

    private static let conversationIDs = Argument(
        name: "ids", kind: .list, summary: "Conversation ids from `mail list`, `mail search` or `mail show`.",
        positional: true, variadic: true, required: true)

    private static let mailbox = Argument(
        name: "mailbox", kind: .list,
        summary: "Mailbox roles (inbox, starred, important, sent, drafts, archive, all, junk, trash), or with --account a mailbox id or name. Comma-separated for several.")

    private static let account = Argument(name: "account", kind: .string, summary: "Only this account (its address).")

    static let all: [CommandSpec] = [
        CommandSpec(name: "status", summary: "The app's state: version, accounts, sync, push, AI tagging, settings problems."),

        CommandSpec(name: "config list", summary: "Every setting with its value, default, type and description.", local: true),
        CommandSpec(name: "config get", summary: "One setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "Setting key, e.g. sync.pollSeconds.", positional: true, required: true),
        ], local: true),
        CommandSpec(name: "config set", summary: "Changes a setting; the running app applies it at once.", arguments: [
            Argument(name: "key", kind: .string, summary: "Setting key.", positional: true, required: true),
            Argument(name: "value", kind: .string, summary: "true/false, a number, text, or for lists a JSON array or comma-separated values.",
                     positional: true, required: true),
        ], local: true),
        CommandSpec(name: "config reset", summary: "Puts a setting back to its default.", arguments: [
            Argument(name: "key", kind: .string, summary: "Setting key.", positional: true, required: true),
        ], local: true),
        CommandSpec(name: "config add", summary: "Adds values to a list setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "List setting key, e.g. remoteContent.trustedSenders.", positional: true, required: true),
            Argument(name: "values", kind: .list, summary: "Values to add.", positional: true, variadic: true, required: true),
        ], local: true),
        CommandSpec(name: "config remove", summary: "Removes values from a list setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "List setting key.", positional: true, required: true),
            Argument(name: "values", kind: .list, summary: "Values to remove.", positional: true, variadic: true, required: true),
        ], local: true),
        CommandSpec(name: "config schema", summary: "JSON Schema of settings.json.", local: true),
        CommandSpec(name: "config path", summary: "Where settings.json lives; editing it directly works too.", local: true),

        CommandSpec(name: "accounts list", summary: "Signed-in accounts: address, label, provider, services."),
        CommandSpec(name: "accounts rename", summary: "Changes the label an account shows in the sidebar; empty shows the address.", arguments: [
            Argument(name: "account", kind: .string, summary: "Account address.", positional: true, required: true),
            Argument(name: "label", kind: .string, summary: "New label.", positional: true, required: true),
        ]),
        CommandSpec(name: "accounts add", summary: "Signs in a new account through the browser and waits (up to 5 minutes) for the person to finish.", arguments: [
            Argument(name: "provider", kind: .choice(["google", "microsoft"]), summary: "Provider.", positional: true, required: true),
            Argument(name: "client-id", kind: .string, summary: "OAuth client id; defaults to the one used last (or the build's)."),
            Argument(name: "client-secret", kind: .string, summary: "OAuth client secret; Google requires one."),
        ]),
        CommandSpec(name: "accounts remove", summary: "Signs an account out and deletes its saved sign-in. Mail on the server is untouched.", arguments: [
            Argument(name: "account", kind: .string, summary: "Account address.", positional: true, required: true),
            yes,
        ], confirm: true),

        CommandSpec(name: "mailboxes", summary: "Each account's mailboxes with role, id and counts.", arguments: [account]),

        CommandSpec(name: "mail list", summary: "Conversations in mailboxes, newest first, with AI category and waiting-on-you flag.", arguments: [
            mailbox, account,
            Argument(name: "unread", kind: .bool, summary: "Only unread conversations."),
            Argument(name: "starred", kind: .bool, summary: "Only starred conversations."),
            Argument(name: "category", kind: .choice(MailCategory.allCases.map(\.rawValue)), summary: "Only this AI category."),
            Argument(name: "waiting", kind: .bool, summary: "Only conversations waiting on your reply."),
            Argument(name: "limit", kind: .int, summary: "Most conversations returned (1-100).", defaultValue: 25),
        ]),
        CommandSpec(name: "mail search", summary: "Searches every mailbox but trash and junk with the providers' own search (subject, people, body; Gmail operators such as from: work), newest first.", arguments: [
            Argument(name: "query", kind: .string, summary: "Free text, or the provider's search syntax.", positional: true, required: true),
            account,
            Argument(name: "limit", kind: .int, summary: "Most conversations returned (1-100).", defaultValue: 25),
        ]),
        CommandSpec(name: "mail show", summary: "A conversation's messages with headers, text bodies and attachments.", arguments: [
            Argument(name: "id", kind: .string, summary: "Conversation id.", positional: true, required: true),
            Argument(name: "max-chars", kind: .int, summary: "Longest text body returned per message.", defaultValue: 20_000),
        ]),
        CommandSpec(name: "mail read", summary: "Marks conversations read (or with --unread, unread).", arguments: [
            conversationIDs,
            Argument(name: "unread", kind: .bool, summary: "Mark unread instead."),
        ]),
        CommandSpec(name: "mail star", summary: "Stars conversations (or with --off, unstars them).", arguments: [
            conversationIDs,
            Argument(name: "off", kind: .bool, summary: "Remove the star instead."),
        ]),
        CommandSpec(name: "mail archive", summary: "Moves conversations out of the inbox (or the mailbox they were listed from) into Archive / All Mail.",
                    arguments: [conversationIDs]),
        CommandSpec(name: "mail trash", summary: "Moves conversations to the trash. Messages already in the trash are deleted for good, which needs --yes.",
                    arguments: [conversationIDs, yes]),
        CommandSpec(name: "mail restore", summary: "Moves conversations' messages in the trash or junk back to the inbox.",
                    arguments: [conversationIDs]),
        CommandSpec(name: "mail tag", summary: "Corrects the AI tag: category, waiting-on-you, topic labels.", arguments: [
            conversationIDs,
            Argument(name: "category", kind: .choice(MailCategory.allCases.map(\.rawValue)), summary: "New category."),
            Argument(name: "waiting", kind: .choice(["true", "false"]), summary: "Whether it waits on your reply (people conversations only)."),
            Argument(name: "labels", kind: .list, summary: "Topic labels (at most two)."),
        ]),
        CommandSpec(name: "mail retag", summary: "Forgets every AI tag and tags the current list again."),
        CommandSpec(name: "mail send", summary: "Sends a new plain-text message.", arguments: [
            Argument(name: "from", kind: .string, summary: "Sending account (its address).", required: true),
            Argument(name: "to", kind: .list, summary: "Recipients.", required: true),
            Argument(name: "cc", kind: .list, summary: "Cc recipients."),
            Argument(name: "bcc", kind: .list, summary: "Bcc recipients."),
            Argument(name: "subject", kind: .string, summary: "Subject.", required: true),
            Argument(name: "body", kind: .string, summary: "Plain-text body; `-` reads standard input.", required: true),
            yes,
        ], confirm: true),
        CommandSpec(name: "mail reply", summary: "Replies to a conversation's latest message, quoting it like the app does.", arguments: [
            Argument(name: "id", kind: .string, summary: "Conversation id.", positional: true, required: true),
            Argument(name: "body", kind: .string, summary: "Plain-text reply; `-` reads standard input.", required: true),
            Argument(name: "all", kind: .bool, summary: "Reply to everyone."),
            yes,
        ], confirm: true),

        CommandSpec(name: "refresh", summary: "Fetches new mail and counts for the list on screen now."),
    ]

    static let overview = """
        mailstream — control the Mailstream mail app from a shell or an AI agent.

        Every command prints JSON on stdout. Failures print {"error": {"code", "message"}} on stderr
        and exit 1 (2 for usage errors, 3 when the app cannot be reached).

        `config …` reads and writes ~/Library/Application Support/Mailstream/settings.json directly;
        the running app applies changes at once. Every other command talks to the running app over
        a private socket, launching the app in the background if needed.

        Conversations are addressed by the `id` that `mail list`, `mail search` and `mail show`
        return (e.g. "you@gmail.com|<message-id>"). Accounts are addressed by their email address.
        Sending mail and signing accounts out require --yes; without it the error says what would
        happen.

        `mailstream commands` prints every command and argument as JSON.
        `mailstream help <command>` describes one command.
        """

    /// Usage text for one command.
    var help: String {
        let positionals = arguments.filter(\.positional).map { $0.variadic ? "<\($0.name)>..." : "<\($0.name)>" }
        var lines = ["mailstream \(([name] + positionals).joined(separator: " "))\(arguments.contains { !$0.positional } ? " [options]" : "")",
                     "", summary]
        if !arguments.isEmpty { lines.append("") }
        for argument in arguments {
            let label = argument.positional ? "<\(argument.name)>" : "--\(argument.name)"
            var detail = argument.summary
            if case .choice(let values) = argument.kind { detail += " One of: \(values.joined(separator: ", "))." }
            if argument.required && !argument.positional { detail += " Required." }
            if let value = argument.defaultValue { detail += " Default: \(String(decoding: value.encoded(pretty: false), as: UTF8.self))." }
            lines.append("  \(label.padding(toLength: 18, withPad: " ", startingAt: 0)) \(detail)")
        }
        if local { lines += ["", "Works without the app running."] }
        return lines.joined(separator: "\n")
    }

    /// The command as JSON, for `mailstream commands`.
    var json: JSON {
        .object([
            "name": .string(name),
            "summary": .string(summary),
            "confirm": .bool(confirm),
            "needsApp": .bool(!local),
            "arguments": .array(arguments.map { argument in
                var out: [String: JSON] = [
                    "name": .string(argument.name), "summary": .string(argument.summary),
                    "positional": .bool(argument.positional), "required": .bool(argument.required),
                ]
                switch argument.kind {
                case .string: out["type"] = "string"
                case .int: out["type"] = "integer"
                case .bool: out["type"] = "boolean"
                case .list: out["type"] = "array"
                case .choice(let values):
                    out["type"] = "string"
                    out["enum"] = .strings(values)
                }
                if argument.variadic { out["variadic"] = true }
                if let value = argument.defaultValue { out["default"] = value }
                return .object(out)
            }),
        ])
    }
}

/// A failed command: a stable `code` for programs, a `message` for people.
nonisolated struct ControlError: LocalizedError, Sendable {
    let code: String
    let message: String

    init(_ code: String, _ message: String) {
        self.code = code
        self.message = message
    }

    var errorDescription: String? { message }
    var json: JSON { ["error": ["code": .string(code), "message": .string(message)]] }
}
