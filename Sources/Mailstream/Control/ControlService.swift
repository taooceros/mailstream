import Foundation

/// Runs control-socket requests against the app's stores, on the main actor like the UI.
@MainActor
final class ControlService {
    private let accounts: AccountStore
    private let store: MailStore

    /// Conversations handed out by `mail list`/`search`/`show`, so later commands can address them
    /// by id while the app runs, with the mailboxes they were listed from.
    private struct Known {
        var conversation: Conversation
        var primary: Set<MailboxKey>
    }

    private var known: [String: Known] = [:]
    private var knownOrder: [String] = []
    private static let knownLimit = 5000

    init(accounts: AccountStore, store: MailStore) {
        self.accounts = accounts
        self.store = store
    }

    /// One request's answer, encoded.
    nonisolated func handle(_ data: Data) async -> Data {
        let answer: JSON
        do {
            let request = try? JSON.decode(data)
            guard let name = request?["command"]?.string else {
                throw ControlError("bad_request", "Expected {\"command\": …, \"args\": {…}}.")
            }
            guard let spec = CommandSpec.named(name), !spec.local else {
                throw ControlError("unknown_command", "Unknown command \"\(name)\"; `mailstream commands` lists them.")
            }
            let args = try Arguments(spec: spec, values: request?["args"]?.object ?? [:])
            answer = ["ok": true, "result": try await run(spec, args)]
        } catch let error as ControlError {
            answer = ["ok": false, "error": ["code": .string(error.code), "message": .string(error.message)]]
        } catch {
            answer = ["ok": false, "error": ["code": "failed", "message": .string(error.localizedDescription)]]
        }
        return answer.encoded(pretty: false)
    }

    private func run(_ spec: CommandSpec, _ args: Arguments) async throws -> JSON {
        switch spec.id {
        case "status": return status()
        case "accounts.list": return .array(accounts.records.map(accountJSON))
        case "accounts.rename": return try rename(args)
        case "accounts.add": return try await addAccount(args)
        case "accounts.remove": return try removeAccount(args)
        case "mailboxes": return try mailboxes(args)
        case "mail.list": return try await list(args, query: nil)
        case "mail.search": return try await list(args, query: try args.required("query"))
        case "mail.show": return try await show(args)
        case "mail.read":
            let unread = args.bool("unread")
            return try await each(args) { known in
                try await self.store.setRead(known.conversation, read: !unread).value
                return .remember(known.conversation.updating(Set(known.conversation.messages.map(\.ref))) { envelope in
                    if unread { envelope.flags.remove(MessageFlag.seen.rawValue) } else { envelope.flags.insert(MessageFlag.seen.rawValue) }
                })
            }
        case "mail.star":
            let off = args.bool("off")
            return try await each(args) { known in
                try await self.store.setFlagged(known.conversation, flagged: !off).value
                let refs: Set<MessageRef> = off ? Set(known.conversation.messages.map(\.ref)) : [known.conversation.latest.ref]
                return .remember(known.conversation.updating(refs) { envelope in
                    if off { envelope.flags.remove(MessageFlag.flagged.rawValue) } else { envelope.flags.insert(MessageFlag.flagged.rawValue) }
                })
            }
        case "mail.archive":
            return try await each(args) { known in
                try await self.store.archive(known.conversation, from: known.primary).value
                return .forget
            }
        case "mail.trash":
            let yes = args.bool("yes")
            return try await each(args) { known in
                if !yes, let trash = self.store.mailbox(account: known.conversation.account, role: .trash),
                   known.conversation.messages.contains(where: { $0.ref.mailbox == trash.id }) {
                    throw ControlError("confirm", "\(known.conversation.id) is already in the trash, so it would be deleted for good. Re-run with --yes.")
                }
                try await self.store.trash(known.conversation).value
                return .forget
            }
        case "mail.tag": return try tag(args)
        case "mail.retag":
            guard store.tagging.isEnabled else { throw ControlError("disabled", "AI tagging is off (aiTagging.enabled).") }
            store.tagging.retagAll()
            return ["started": true]
        case "mail.send": return try await send(args)
        case "mail.reply": return try await reply(args)
        case "refresh":
            store.refresh()
            store.refreshCounts()
            return ["started": true]
        default:
            throw ControlError("unknown_command", "\(spec.name) is not handled by the app.")
        }
    }

    // MARK: - Status and accounts

    private func status() -> JSON {
        let info = Bundle.main.infoDictionary ?? [:]
        return [
            "version": .string(info["CFBundleShortVersionString"] as? String ?? "dev"),
            "pid": .int(Int(ProcessInfo.processInfo.processIdentifier)),
            "settingsFile": .string(SettingsFile.url.path),
            "settingsIssues": .strings(Settings.shared.issues),
            "accounts": .array(store.accounts.map { account in
                [
                    "id": .string(account.name), "label": .string(account.label), "provider": .string(account.provider.rawValue),
                    "mailboxesLoaded": .bool(!(store.mailboxes[account.name] ?? []).isEmpty),
                ]
            }),
            "sync": [
                "loading": .bool(store.isLoading),
                "lastRefresh": .date(store.lastRefresh),
                "lastError": (store.problems.current[.mail]?.message).json,
                "pushConnected": .bool(store.isPushConnected),
            ],
            "aiTagging": ["enabled": .bool(store.tagging.isEnabled), "status": store.tagging.status.json],
        ]
    }

    private func accountJSON(_ record: AccountRecord) -> JSON {
        let scopes = record.scopes.joined(separator: " ").lowercased()
        var services: [String] = []
        if scopes.contains("mail") { services.append("mail") }
        if scopes.contains("contacts") { services.append("contacts") }
        if scopes.contains("chat") { services.append("chat") }
        return [
            "id": .string(record.id), "label": .string(record.label), "provider": .string(record.provider.rawValue),
            "name": record.displayName.json, "services": .strings(services),
        ]
    }

    private func record(_ args: Arguments) throws -> AccountRecord {
        let id = try args.required("account").lowercased()
        guard let record = accounts.record(id) else {
            throw ControlError("unknown_account", "No account \(id); `mailstream accounts list` shows them.")
        }
        return record
    }

    private func rename(_ args: Arguments) throws -> JSON {
        let record = try record(args)
        accounts.rename(record.id, label: args.string("label") ?? "")
        return accounts.record(record.id).map(accountJSON) ?? .null
    }

    private func removeAccount(_ args: Arguments) throws -> JSON {
        let record = try record(args)
        guard args.bool("yes") else {
            throw ControlError("confirm", "Would sign out of \(record.id) and delete its saved sign-in (mail on the server stays). Re-run with --yes.")
        }
        accounts.remove(record.id)
        return ["removed": .string(record.id)]
    }

    private func addAccount(_ args: Arguments) async throws -> JSON {
        guard let provider = Provider(rawValue: try args.required("provider")) else {
            throw ControlError("usage", "provider must be google or microsoft.")
        }
        let suggested = try await accounts.tokens.suggestedClient(for: provider)
        let id = args.string("client-id") ?? suggested?.id
        let secret = args.string("client-secret") ?? (args.string("client-id") == nil ? suggested?.secret : nil)
        guard let id, !id.isEmpty else {
            throw ControlError("usage", "No OAuth client known for \(provider.rawValue); pass --client-id (and --client-secret).")
        }
        if provider == .google, secret?.isEmpty != false {
            throw ControlError("usage", "Google needs the client secret too: --client-secret.")
        }
        let record = try await accounts.signIn(provider, client: OAuthClient(id: id, secret: secret?.isEmpty == true ? nil : secret))
        return accountJSON(record)
    }

    private func mailboxes(_ args: Arguments) throws -> JSON {
        let names = try accountFilter(args).map { [$0] } ?? store.accounts.map(\.name)
        return .object(Dictionary(uniqueKeysWithValues: names.map { name in
            (name, .array((store.mailboxes[name] ?? []).map { box in
                [
                    "id": .string(box.id), "name": .string(box.displayName), "role": .string(box.role.rawValue),
                    "unread": box.unread.map(JSON.int) ?? .null, "total": box.total.map(JSON.int) ?? .null,
                ]
            }))
        }))
    }

    private func accountFilter(_ args: Arguments) throws -> String? {
        guard let id = args.string("account")?.lowercased() else { return nil }
        guard store.account(named: id) != nil else {
            throw ControlError("unknown_account", "No account \(id); `mailstream accounts list` shows them.")
        }
        return id
    }

    // MARK: - Listing

    private func mailboxKeys(_ args: Arguments) throws -> [MailboxKey] {
        let account = try accountFilter(args)
        let names = args.list("mailbox") ?? ["inbox"]
        var keys: [MailboxKey] = []
        for name in names {
            if let role = MailboxRole(rawValue: name.lowercased()), role != .other {
                let boxes = store.unifiedMailboxes(role).filter { account == nil || $0.account == account }
                guard !boxes.isEmpty else {
                    throw ControlError("unknown_mailbox", "No \(role.rawValue) mailbox\(account.map { " in \($0)" } ?? "").")
                }
                keys += boxes.map(\.key)
            } else {
                let matches = store.accounts.filter { account == nil || $0.name == account }.flatMap { store.mailboxes[$0.name] ?? [] }
                    .filter { $0.id == name || $0.name.caseInsensitiveCompare(name) == .orderedSame }
                guard !matches.isEmpty else {
                    throw ControlError("unknown_mailbox", "No mailbox \"\(name)\"; use a role or an id from `mailstream mailboxes`.")
                }
                keys += matches.map(\.key)
            }
        }
        var seen = Set<MailboxKey>()
        return keys.filter { seen.insert($0).inserted }
    }

    private func list(_ args: Arguments, query: String?) async throws -> JSON {
        let limit = min(max(args.int("limit") ?? 25, 1), 100)
        let keys = try mailboxKeys(args)
        let listing = await store.agentListing(keys, query: query, pageSize: min(max(limit, 25), 100))
        if listing.primary.isEmpty, !listing.failures.isEmpty {
            throw ControlError("provider", listing.failures.joined(separator: "; "))
        }
        let category = args.string("category").flatMap(MailCategory.init(rawValue:))
        let conversations = listing.conversations.filter { conversation in
            if args.bool("unread"), !conversation.isUnread { return false }
            if args.bool("starred"), !conversation.isFlagged { return false }
            let tag = store.tagging.tag(for: conversation)
            if let category, tag?.category != category { return false }
            if args.bool("waiting"), !(tag?.category == .people && tag?.toRespond == true) { return false }
            return true
        }.prefix(limit)
        for conversation in conversations { remember(conversation, primary: listing.primary) }
        return ["conversations": .array(conversations.map(summary)), "failures": .strings(listing.failures)]
    }

    private func show(_ args: Arguments) async throws -> JSON {
        let known = try lookup(try args.required("id"))
        let conversation = known.conversation
        let maxChars = max(args.int("max-chars") ?? 20_000, 0)
        var bodies: [MessageRef: Result<MessageContent, any Error>] = [:]
        for message in conversation.messages {
            do {
                bodies[message.ref] = .success(try await store.contents.load(message.ref).content)
            } catch {
                bodies[message.ref] = .failure(error)
            }
        }
        var out = summary(conversation).object ?? [:]
        out["messages"] = .array(conversation.messages.map { message in
            let envelope = message.envelope
            var item: [String: JSON] = [
                "id": .string(message.ref.id), "mailbox": .string(message.ref.mailbox), "role": .string(message.role.rawValue),
                "from": .strings(envelope.from.map(Self.text)), "to": .strings(envelope.to.map(Self.text)),
                "date": .date(envelope.date), "subject": .string(envelope.subject),
                "unread": .bool(!envelope.isSeen), "starred": .bool(envelope.isFlagged),
            ]
            switch bodies[message.ref] {
            case .success(let content):
                let text = content.text.flatMap { $0.isEmpty ? nil : $0 } ?? content.html.map(HTMLText.strip) ?? ""
                item["cc"] = .strings(content.cc.map(Self.text))
                item["text"] = .string(String(text.prefix(maxChars)))
                item["truncated"] = .bool(text.count > maxChars)
                item["attachments"] = .array(content.attachments.filter { !$0.inline }.map { attachment in
                    ["filename": .string(attachment.filename), "mime": .string(attachment.mime), "size": .int(attachment.size),
                     "path": .string(store.cache.attachmentURL(attachment, of: message.ref).path)]
                })
            case .failure(let error):
                item["error"] = .string(error.localizedDescription)
            case nil:
                break
            }
            return .object(item)
        })
        return .object(out)
    }

    private func summary(_ conversation: Conversation) -> JSON {
        let own = store.ownAddresses
        let incoming = conversation.latestIncoming(own: own) ?? conversation.latest
        var seen = Set<String>()
        let people = conversation.messages.flatMap(\.envelope.from).filter { seen.insert($0.normalizedEmail).inserted }
        let tag = store.tagging.tag(for: conversation)
        return [
            "id": .string(conversation.id),
            "account": .string(conversation.account),
            "subject": .string(conversation.subject),
            "date": .date(conversation.latestDate),
            "from": incoming.envelope.from.first.map { .string(Self.text($0)) } ?? .null,
            "participants": .strings(people.map { own.contains($0.normalizedEmail) ? "me" : $0.displayName }),
            "messageCount": .int(conversation.messages.count),
            "unread": .bool(conversation.isUnread),
            "starred": .bool(conversation.isFlagged),
            "hasAttachment": .bool(conversation.hasAttachment),
            "mailboxes": .strings(Array(Set(conversation.messages.map(\.role.rawValue))).sorted()),
            "snippet": store.contents.snippet(for: conversation.latest.ref).json,
            "category": tag.map { .string($0.category.rawValue) } ?? .null,
            "waiting": .bool(tag?.category == .people && tag?.toRespond == true),
            "labels": .strings(tag?.labels ?? []),
        ]
    }

    private static func text(_ address: Address) -> String {
        address.name.map { "\($0) <\(address.email)>" } ?? address.email
    }

    // MARK: - Known conversations

    private func remember(_ conversation: Conversation, primary: Set<MailboxKey>) {
        if known.updateValue(Known(conversation: conversation, primary: primary), forKey: conversation.id) == nil {
            knownOrder.append(conversation.id)
        }
        if knownOrder.count > Self.knownLimit {
            for id in knownOrder.prefix(knownOrder.count - Self.knownLimit) { known[id] = nil }
            knownOrder.removeFirst(knownOrder.count - Self.knownLimit)
        }
    }

    private func lookup(_ id: String) throws -> Known {
        if let found = known[id] { return found }
        if let onScreen = store.conversations.first(where: { $0.id == id }) {
            return Known(conversation: onScreen, primary: Set(store.primaryKeys))
        }
        throw ControlError("unknown_conversation",
                           "Unknown conversation \(id). Ids come from `mail list`, `mail search` or `mail show`, and are remembered while the app runs.")
    }

    private enum Outcome {
        case remember(Conversation)
        case forget
    }

    /// Runs `action` on each conversation id; failures are listed rather than stopping the rest.
    private func each(_ args: Arguments, _ action: (Known) async throws -> Outcome) async throws -> JSON {
        let ids = args.list("ids") ?? []
        guard !ids.isEmpty else { throw ControlError("usage", "Give at least one conversation id.") }
        var done: [String] = []
        var failed: [JSON] = []
        for id in ids {
            do {
                let found = try lookup(id)
                switch try await action(found) {
                case .remember(let updated): known[id]?.conversation = updated
                case .forget: known[id] = nil
                }
                done.append(id)
            } catch let error as ControlError {
                failed.append(["id": .string(id), "code": .string(error.code), "message": .string(error.message)])
            } catch {
                failed.append(["id": .string(id), "code": "failed", "message": .string(error.localizedDescription)])
            }
        }
        return ["done": .strings(done), "failed": .array(failed)]
    }

    // MARK: - Tags and sending

    private func tag(_ args: Arguments) throws -> JSON {
        let category = try args.string("category").map { name in
            guard let category = MailCategory(rawValue: name) else { throw ControlError("usage", "Unknown category \(name).") }
            return category
        }
        let waiting = args.string("waiting").map { $0 == "true" }
        let labels = args.list("labels")
        guard category != nil || waiting != nil || labels != nil else {
            throw ControlError("usage", "Give --category, --waiting or --labels.")
        }
        var tagged: [JSON] = []
        var failed: [JSON] = []
        for id in args.list("ids") ?? [] {
            do {
                let tag = try store.tagging.edit(try lookup(id).conversation, category: category, waiting: waiting, labels: labels)
                tagged.append(["id": .string(id), "category": .string(tag.category.rawValue), "waiting": .bool(tag.toRespond),
                               "labels": .strings(tag.labels)])
            } catch {
                failed.append(["id": .string(id), "message": .string(error.localizedDescription)])
            }
        }
        return ["done": .array(tagged), "failed": .array(failed)]
    }

    private func addresses(_ args: Arguments, _ name: String) throws -> [Address] {
        try (args.list(name) ?? []).map { raw in
            guard let address = Address.parse(raw), address.email.contains("@") else {
                throw ControlError("usage", "--\(name): \"\(raw)\" is not an email address.")
            }
            return address
        }
    }

    private func send(_ args: Arguments) async throws -> JSON {
        let from = try args.required("from").lowercased()
        guard store.account(named: from) != nil else {
            throw ControlError("unknown_account", "No account \(from); `mailstream accounts list` shows them.")
        }
        let to = try addresses(args, "to"), cc = try addresses(args, "cc"), bcc = try addresses(args, "bcc")
        guard !to.isEmpty else { throw ControlError("usage", "--to needs at least one address.") }
        let subject = try args.required("subject"), body = try args.required("body")
        let everyone = (to + cc + bcc).map(\.email).joined(separator: ", ")
        guard args.bool("yes") else {
            throw ControlError("confirm", "Would send \"\(subject)\" (\(body.count) characters) from \(from) to \(everyone). Re-run with --yes.")
        }
        let message = await store.outgoing(account: from, source: nil, to: to, cc: cc, bcc: bcc, subject: subject, text: body)
        try await store.send(message, account: from)
        return ["sent": true, "from": .string(from), "to": .strings(to.map(Self.text)), "subject": .string(subject)]
    }

    private func reply(_ args: Arguments) async throws -> JSON {
        let conversation = try lookup(try args.required("id")).conversation
        let source = conversation.messages.last { $0.role != .drafts } ?? conversation.latest
        let plan = try await store.replyPlan(for: source, all: args.bool("all"))
        let body = try args.required("body")
        let everyone = (plan.to + plan.cc).map(\.email).joined(separator: ", ")
        guard args.bool("yes") else {
            throw ControlError("confirm", "Would send \"\(plan.subject)\" from \(source.ref.account) to \(everyone). Re-run with --yes.")
        }
        try await store.sendReply(body, plan: plan, source: source)
        return ["sent": true, "from": .string(source.ref.account), "to": .strings((plan.to + plan.cc).map(Self.text)),
                "subject": .string(plan.subject)]
    }
}

/// A request's arguments, checked against the command's spec, defaults filled in.
nonisolated struct Arguments: Sendable {
    private let values: [String: JSON]

    init(spec: CommandSpec, values: [String: JSON]) throws {
        var out: [String: JSON] = [:]
        for argument in spec.arguments {
            if let value = values[argument.name], value != .null {
                out[argument.name] = value
            } else if let value = argument.defaultValue {
                out[argument.name] = value
            } else if argument.required {
                throw ControlError("usage", "\(spec.name): \(argument.positional ? "<\(argument.name)>" : "--\(argument.name)") is required.")
            }
        }
        if let unknown = values.keys.first(where: { key in !spec.arguments.contains { $0.name == key } }) {
            throw ControlError("usage", "\(spec.name) has no argument \"\(unknown)\".")
        }
        self.values = out
    }

    func string(_ name: String) -> String? {
        switch values[name] {
        case .string(let value): value
        case .int(let value): String(value)
        case .bool(let value): String(value)
        default: nil
        }
    }

    func required(_ name: String) throws -> String {
        guard let value = string(name) else { throw ControlError("usage", "\(name) is required.") }
        return value
    }

    func int(_ name: String) -> Int? { values[name]?.int ?? string(name).flatMap { Int($0) } }
    func bool(_ name: String) -> Bool { values[name]?.bool ?? (string(name) == "true") }

    func list(_ name: String) -> [String]? {
        switch values[name] {
        case .array(let items): items.compactMap(\.string)
        case .string(let value): value.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        default: nil
        }
    }
}
