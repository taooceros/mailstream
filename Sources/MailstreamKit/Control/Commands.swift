import Foundation

/// Every command, by its request id. Settings commands work on settings.json, so the CLI runs
/// them itself without the app; the control service (socket and FFI) runs them too.
nonisolated enum CommandID: Sendable, Equatable {
    case local(Local)
    case app(App)

    enum Local: String, Sendable {
        case configList = "config.list"
        case configGet = "config.get"
        case configSet = "config.set"
        case configReset = "config.reset"
        case configAdd = "config.add"
        case configRemove = "config.remove"
        case configSchema = "config.schema"
        case configPath = "config.path"
    }

    enum App: String, Sendable {
        case status
        case accountsList = "accounts.list"
        case accountsRename = "accounts.rename"
        case accountsAdd = "accounts.add"
        case accountsRemove = "accounts.remove"
        case mailboxes
        case mailList = "mail.list"
        case mailSearch = "mail.search"
        case mailShow = "mail.show"
        case mailPreviews = "mail.previews"
        case mailAvatars = "mail.avatars"
        case mailRead = "mail.read"
        case mailStar = "mail.star"
        case mailArchive = "mail.archive"
        case mailTrash = "mail.trash"
        case mailRestore = "mail.restore"
        case mailTag = "mail.tag"
        case mailRetag = "mail.retag"
        case mailSend = "mail.send"
        case mailReply = "mail.reply"
        case refresh
        case contactsSearch = "contacts.search"
        case chatSpaces = "chat.spaces"
        case chatMore = "chat.more"
        case chatRefresh = "chat.refresh"
        case chatOpen = "chat.open"
        case chatClose = "chat.close"
        case chatMessages = "chat.messages"
        case chatEarlier = "chat.earlier"
        case chatSend = "chat.send"
        case chatPeople = "chat.people"
        case chatStart = "chat.start"
        case chatAttachment = "chat.attachment"
    }

    /// The request's `command`: `mail.list`.
    var rawValue: String {
        switch self {
        case .local(let command): command.rawValue
        case .app(let command): command.rawValue
        }
    }
}

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

        /// How usage text and errors name it: `<id>` or `--limit`.
        var label: String { positional ? "<\(name)>" : "--\(name)" }
    }

    let command: CommandID
    let summary: String
    var arguments: [Argument] = []
    /// Has effects Mailstream cannot undo (sending, signing out): the request must carry `yes: true`.
    var confirm = false

    /// The request's `command`: `mail.list`.
    var id: String { command.rawValue }
    /// Space-separated words, e.g. `mail list`.
    var name: String { id.replacingOccurrences(of: ".", with: " ") }
    /// Runs in the CLI itself on settings.json; the app need not be running.
    var local: Bool { if case .local = command { true } else { false } }

    static func named(_ id: String) -> CommandSpec? { all.first { $0.id == id } }

    static let yes = Argument(name: "yes", kind: .bool, summary: "Confirm; without it nothing is done and the error says what would happen.")

    private static let conversationIDs = Argument(
        name: "ids", kind: .list, summary: "Conversation ids from `mail list`, `mail search` or `mail show`.",
        positional: true, variadic: true, required: true)

    private static let conversationID = Argument(name: "id", kind: .string, summary: "Conversation id.", positional: true, required: true)

    private static let mailbox = Argument(
        name: "mailbox", kind: .list,
        summary: "Mailbox roles (inbox, starred, important, sent, drafts, archive, all, junk, trash), or with --account a mailbox id or name. Comma-separated for several.")

    private static let account = Argument(name: "account", kind: .string, summary: "Only this account (its address).")

    private static let accountAddress = Argument(name: "account", kind: .string, summary: "Account address.", positional: true, required: true)

    private static let limit = Argument(name: "limit", kind: .int, summary: "Most conversations returned (1-200).", defaultValue: 25)

    private static let settingKey = Argument(name: "key", kind: .string, summary: "Setting key.", positional: true, required: true)

    private static let chatAccount = Argument(name: "account", kind: .string, summary: "Chat account (its address).", positional: true, required: true)

    private static let chatSpace = Argument(name: "space", kind: .string, summary: "Space name from `chat spaces` (spaces/…).", positional: true, required: true)

    static let all: [CommandSpec] = [
        CommandSpec(command: .app(.status), summary: "The app's state: version, accounts, sync, push, AI tagging, settings problems."),

        CommandSpec(command: .local(.configList), summary: "Every setting with its value, default, type and description."),
        CommandSpec(command: .local(.configGet), summary: "One setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "Setting key, e.g. sync.pollSeconds.", positional: true, required: true),
        ]),
        CommandSpec(command: .local(.configSet), summary: "Changes a setting; the running app applies it at once.", arguments: [
            settingKey,
            Argument(name: "value", kind: .string, summary: "true/false, a number, text, or for lists a JSON array or comma-separated values.",
                     positional: true, required: true),
        ]),
        CommandSpec(command: .local(.configReset), summary: "Puts a setting back to its default.", arguments: [settingKey]),
        CommandSpec(command: .local(.configAdd), summary: "Adds values to a list setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "List setting key, e.g. remoteContent.trustedSenders.", positional: true, required: true),
            Argument(name: "values", kind: .list, summary: "Values to add.", positional: true, variadic: true, required: true),
        ]),
        CommandSpec(command: .local(.configRemove), summary: "Removes values from a list setting.", arguments: [
            Argument(name: "key", kind: .string, summary: "List setting key.", positional: true, required: true),
            Argument(name: "values", kind: .list, summary: "Values to remove.", positional: true, variadic: true, required: true),
        ]),
        CommandSpec(command: .local(.configSchema), summary: "JSON Schema of settings.json."),
        CommandSpec(command: .local(.configPath), summary: "Where settings.json lives; editing it directly works too."),

        CommandSpec(command: .app(.accountsList), summary: "Signed-in accounts: address, label, provider, services."),
        CommandSpec(command: .app(.accountsRename), summary: "Changes the label an account shows in the sidebar; empty shows the address.", arguments: [
            accountAddress,
            Argument(name: "label", kind: .string, summary: "New label.", positional: true, required: true),
        ]),
        CommandSpec(command: .app(.accountsAdd), summary: "Signs in a new account through the browser and waits (up to 5 minutes) for the person to finish.", arguments: [
            Argument(name: "provider", kind: .choice(["google", "microsoft"]), summary: "Provider.", positional: true, required: true),
            Argument(name: "client-id", kind: .string, summary: "OAuth client id; defaults to the one used last (or the build's)."),
            Argument(name: "client-secret", kind: .string, summary: "OAuth client secret; Google requires one."),
        ]),
        CommandSpec(command: .app(.accountsRemove), summary: "Signs an account out and deletes its saved sign-in. Mail on the server is untouched.", arguments: [
            accountAddress,
            yes,
        ], confirm: true),

        CommandSpec(command: .app(.mailboxes), summary: "Each account's mailboxes with role, id and counts.", arguments: [account]),

        CommandSpec(command: .app(.mailList), summary: "Conversations in mailboxes, newest first, with AI category and waiting-on-you flag. The answer's `next`, when there is more, passed as --cursor gives the following page.", arguments: [
            mailbox, account,
            Argument(name: "unread", kind: .bool, summary: "Only unread conversations."),
            Argument(name: "starred", kind: .bool, summary: "Only starred conversations."),
            Argument(name: "category", kind: .choice(MailCategory.allCases.map(\.rawValue)), summary: "Only this AI category."),
            Argument(name: "waiting", kind: .bool, summary: "Only conversations waiting on your reply or action."),
            limit,
            Argument(name: "cursor", kind: .string, summary: "The previous page's `next`: lists the conversations after it, with the same filters."),
            Argument(name: "cached", kind: .bool, summary: "Lists the mail already loaded without asking the providers for their newest first."),
        ]),
        CommandSpec(command: .app(.mailSearch), summary: "Searches every mailbox but trash and junk with the providers' own search (subject, people, body; Gmail operators such as from: work), newest first.", arguments: [
            Argument(name: "query", kind: .string, summary: "Free text, or the provider's search syntax.", positional: true, required: true),
            account,
            limit,
        ]),
        CommandSpec(command: .app(.mailShow), summary: "A conversation's messages with headers, text bodies and attachments.", arguments: [
            conversationID,
            Argument(name: "max-chars", kind: .int, summary: "Longest text body returned per message.", defaultValue: 20_000),
            Argument(name: "html", kind: .bool, summary: "Also return each message's HTML body, when it has one, under `html`; inline images point at files in the cache."),
        ]),
        CommandSpec(command: .app(.mailPreviews), summary: "The preview line of each conversation's latest message, loading the messages that have none yet.",
                    arguments: [conversationIDs]),
        CommandSpec(command: .app(.mailAvatars), summary: "Each sender's picture as a PNG file in the cache (contact photo, BIMI logo, Gravatar or, for automated senders, the site icon), looking up those not cached yet; null when there is none.", arguments: [
            Argument(name: "addresses", kind: .list, summary: "Sender addresses.", positional: true, variadic: true, required: true),
            Argument(name: "automated", kind: .list, summary: "Those of the addresses that send automated mail, for which a site icon may stand in."),
        ]),
        CommandSpec(command: .app(.mailRead), summary: "Marks conversations read (or with --unread, unread).", arguments: [
            conversationIDs,
            Argument(name: "unread", kind: .bool, summary: "Mark unread instead."),
        ]),
        CommandSpec(command: .app(.mailStar), summary: "Stars conversations (or with --off, unstars them).", arguments: [
            conversationIDs,
            Argument(name: "off", kind: .bool, summary: "Remove the star instead."),
        ]),
        CommandSpec(command: .app(.mailArchive), summary: "Moves conversations out of the inbox (or the mailbox they were listed from) into Archive / All Mail.",
                    arguments: [conversationIDs]),
        CommandSpec(command: .app(.mailTrash), summary: "Moves conversations to the trash. Messages already in the trash are deleted for good, which needs --yes.",
                    arguments: [conversationIDs, yes]),
        CommandSpec(command: .app(.mailRestore), summary: "Moves conversations' messages in the trash or junk back to the inbox.",
                    arguments: [conversationIDs]),
        CommandSpec(command: .app(.mailTag), summary: "Corrects the AI tag: category, waiting-on-you, topic labels.", arguments: [
            conversationIDs,
            Argument(name: "category", kind: .choice(MailCategory.allCases.map(\.rawValue)), summary: "New category."),
            Argument(name: "waiting", kind: .choice(["true", "false"]), summary: "Whether it waits on your reply or action (not newsletters)."),
            Argument(name: "labels", kind: .list, summary: "Topic labels (at most two)."),
        ]),
        CommandSpec(command: .app(.mailRetag), summary: "Forgets every AI tag and tags the current list again."),
        CommandSpec(command: .app(.mailSend), summary: "Sends a new plain-text message, with files attached if given.", arguments: [
            Argument(name: "from", kind: .string, summary: "Sending account (its address).", required: true),
            Argument(name: "to", kind: .list, summary: "Recipients.", required: true),
            Argument(name: "cc", kind: .list, summary: "Cc recipients."),
            Argument(name: "bcc", kind: .list, summary: "Bcc recipients."),
            Argument(name: "subject", kind: .string, summary: "Subject.", required: true),
            Argument(name: "body", kind: .string, summary: "Plain-text body; `-` reads standard input.", required: true),
            Argument(name: "attachments", kind: .list, summary: "A file to attach; repeat for several."),
            yes,
        ], confirm: true),
        CommandSpec(command: .app(.mailReply), summary: "Replies to a conversation's latest message, quoting it like the app does.", arguments: [
            conversationID,
            Argument(name: "body", kind: .string, summary: "Plain-text reply; `-` reads standard input.", required: true),
            Argument(name: "all", kind: .bool, summary: "Reply to everyone."),
            yes,
        ], confirm: true),

        CommandSpec(command: .app(.refresh), summary: "Fetches new mail and counts for the list on screen now."),

        CommandSpec(command: .app(.contactsSearch), summary: "People whose name or address starts with the query, best first, as compose completes them: the accounts' Google and Outlook directories and recent correspondents.", arguments: [
            Argument(name: "query", kind: .string, summary: "The start of a name, a word of it, or an address.", positional: true, required: true),
            Argument(name: "limit", kind: .int, summary: "Most people returned (1-12).", defaultValue: 10),
        ]),

        CommandSpec(command: .app(.chatSpaces), summary: "Google Chat: the chat accounts and their spaces (direct messages, group chats, spaces) as the list shows them, most recently active first, from what the app has loaded.", arguments: [account]),
        CommandSpec(command: .app(.chatMore), summary: "Loads another page of older spaces for one chat account, or all of them.", arguments: [account]),
        CommandSpec(command: .app(.chatRefresh), summary: "Fetches every chat account's spaces now."),
        CommandSpec(command: .app(.chatOpen), summary: "Opens a space as the reader does: its cached messages at once, the newest from Google after, kept current and read while it stays open. Answers what is loaded now.", arguments: [chatAccount, chatSpace]),
        CommandSpec(command: .app(.chatClose), summary: "Closes the open space: it is no longer kept current."),
        CommandSpec(command: .app(.chatMessages), summary: "A space's loaded messages, oldest first, with those being sent from here.", arguments: [chatAccount, chatSpace]),
        CommandSpec(command: .app(.chatEarlier), summary: "Loads the page of messages before the oldest loaded one.", arguments: [chatAccount, chatSpace]),
        CommandSpec(command: .app(.chatSend), summary: "Sends a plain-text message to a space and waits for Google to take it.", arguments: [
            chatAccount, chatSpace,
            Argument(name: "text", kind: .string, summary: "Message text; `-` reads standard input.", required: true),
            Argument(name: "thread", kind: .string, summary: "Reply in this thread (`spaces/…/threads/…`)."),
            yes,
        ], confirm: true),
        CommandSpec(command: .app(.chatPeople), summary: "People a chat account talks with, most recently active first, and whether it may start new conversations.",
                    arguments: [accountAddress]),
        CommandSpec(command: .app(.chatStart), summary: "Finds or starts the direct message with one person, or the group chat with several, and answers its space.", arguments: [
            accountAddress,
            Argument(name: "people", kind: .list, summary: "Email addresses of the people to chat with.", positional: true, variadic: true, required: true),
        ]),
        CommandSpec(command: .app(.chatAttachment), summary: "Downloads a file uploaded to a chat message (its `resource` from `chat messages`) into the cache once and answers its path.", arguments: [
            chatAccount,
            Argument(name: "resource", kind: .string, summary: "The attachment's `resource` from `chat messages`.", positional: true, required: true),
            Argument(name: "name", kind: .string, summary: "The file name to keep it under (its kind follows the extension)."),
        ]),
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
        Google Chat spaces are addressed by their account and the `space` that `chat spaces` returns
        (e.g. "spaces/AAAA…"). Sending mail or chat messages and signing accounts out require
        --yes; without it the error says what would happen.

        `mailstream commands` prints every command and argument as JSON.
        `mailstream help <command>` describes one command.
        """

    /// Usage text for one command.
    var help: String {
        let positionals = arguments.filter(\.positional).map { $0.variadic ? "\($0.label)..." : $0.label }
        var lines = ["mailstream \(([name] + positionals).joined(separator: " "))\(arguments.contains { !$0.positional } ? " [options]" : "")",
                     "", summary]
        if !arguments.isEmpty { lines.append("") }
        for argument in arguments {
            var detail = argument.summary
            if case .choice(let values) = argument.kind { detail += " One of: \(values.joined(separator: ", "))." }
            if argument.required && !argument.positional { detail += " Required." }
            if let value = argument.defaultValue { detail += " Default: \(value.text(pretty: false))." }
            lines.append(Self.row(argument.label, detail))
        }
        if local { lines += ["", "Works without the app running."] }
        return lines.joined(separator: "\n")
    }

    /// One line of a two-column help listing.
    static func row(_ term: String, _ description: String) -> String {
        "  \(term.padding(toLength: 18, withPad: " ", startingAt: 0)) \(description)"
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

    /// Any failure as a command failure: a `ControlError` as it is, anything else under `failed`.
    init(_ error: any Error) {
        self = error as? ControlError ?? ControlError("failed", error.localizedDescription)
    }

    static func usage(_ message: String) -> ControlError { ControlError("usage", message) }
    static func invalidSetting(_ message: String) -> ControlError { ControlError("invalid_setting", message) }

    var errorDescription: String? { message }
    /// `{"code", "message"}`: the `error` of a failed answer, and of the CLI's stderr.
    var body: JSON { ["code": .string(code), "message": .string(message)] }
    var json: JSON { ["error": body] }
}

/// Values typed as words, on a command line or as a request's text.
nonisolated enum TypedValue {
    /// `true`/`yes`/`on`/`1` or their opposites, any case; nil for anything else.
    static func bool(_ word: String) -> Bool? {
        switch word.lowercased() {
        case "true", "yes", "on", "1": true
        case "false", "no", "off", "0": false
        default: nil
        }
    }

    /// Comma-separated values, each trimmed of spaces, empty ones dropped.
    static func list(_ text: String) -> [String] {
        text.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }
}
