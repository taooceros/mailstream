import Foundation

/// Runs control-socket requests against the app's stores, on the main actor like the UI.
@MainActor
final class ControlService {
    let accounts: AccountStore
    let store: MailStore

    /// Conversations handed out by `mail list`/`search`/`show`, so later commands can address them
    /// by id while the app runs, with the mailboxes they were listed from.
    private struct Known {
        var conversation: Conversation
        var primary: Set<MailboxKey>
    }

    private var known: [String: Known] = [:]
    private var knownOrder: [String] = []
    private static let knownLimit = 5000
    /// `contacts.search`'s people (`MailStore.contacts()`), loaded again after `contactsAge`.
    var contactsLoad: (task: Task<[Contact], Never>, started: ContinuousClock.Instant)?

    init(accounts: AccountStore, store: MailStore) {
        self.accounts = accounts
        self.store = store
    }

    /// One request's answer, encoded. Decoding and encoding run off the main actor, which only
    /// runs the command: answers carrying message bodies take milliseconds to encode.
    @concurrent nonisolated func handle(_ data: Data) async -> Data {
        let answer: JSON
        do {
            let request = try? JSON.decode(data)
            guard let name = request?["command"]?.string else {
                throw ControlError("bad_request", "Expected {\"command\": …, \"args\": {…}}.")
            }
            guard let spec = CommandSpec.named(name) else {
                throw ControlError("unknown_command", "Unknown command \"\(name)\"; `mailstream commands` lists them.")
            }
            let values = request?["args"]?.object ?? [:]
            let args = try Arguments(spec: spec, values: values)
            let result = switch spec.command {
            case .app(let command): try await run(command, args)
            case .local(let command): try await configure(command, values)
            }
            answer = ["ok": true, "result": result]
        } catch {
            answer = ["ok": false, "error": ControlError(error).body]
        }
        return answer.encoded(pretty: false)
    }

    /// A `config …` command; the settings in effect follow at once rather than when the file
    /// watcher next reads the file.
    private func configure(_ command: CommandID.Local, _ values: [String: JSON]) throws -> JSON {
        let result = try command.run(values)
        Settings.shared.reload()
        return result
    }

    private func run(_ command: CommandID.App, _ args: Arguments) async throws -> JSON {
        switch command {
        case .status: return status()
        case .accountsList: return .array(accounts.records.map(accountJSON))
        case .accountsRename: return try rename(args)
        case .accountsAdd: return try await addAccount(args)
        case .accountsRemove: return try removeAccount(args)
        case .mailboxes: return try mailboxes(args)
        case .mailList: return try await list(args, query: nil)
        case .mailSearch: return try await list(args, query: try args.required("query"))
        case .mailShow: return try await show(args)
        case .mailPreviews: return try await previews(args)
        case .mailAvatars: return try await avatars(args)
        case .mailRead:
            let unread = args.bool("unread")
            return try await each(args) { known in
                try await self.store.setRead(known.conversation, read: !unread).value
                return .remember(known.conversation.updating(Set(known.conversation.messages.map(\.ref))) { $0.setFlag(.seen, !unread) })
            }
        case .mailStar:
            let off = args.bool("off")
            return try await each(args) { known in
                try await self.store.setFlagged(known.conversation, flagged: !off).value
                let refs: Set<MessageRef> = off ? Set(known.conversation.messages.map(\.ref)) : [known.conversation.latest.ref]
                return .remember(known.conversation.updating(refs) { $0.setFlag(.flagged, !off) })
            }
        case .mailArchive:
            return try await each(args) { known in
                try await self.store.archive(known.conversation, from: known.primary).value
                return .forget
            }
        case .mailTrash:
            return try await each(args) { known in
                if let trash = self.store.mailbox(account: known.conversation.account, role: .trash),
                   known.conversation.messages.contains(where: { $0.ref.mailbox == trash.id }) {
                    try self.requireYes(args, "\(known.conversation.id) is already in the trash, so it would be deleted for good. Re-run with --yes.")
                }
                try await self.store.trash(known.conversation).value
                return .forget
            }
        case .mailRestore:
            return try await each(args) { known in
                try await self.store.restore(known.conversation).value
                return .forget
            }
        case .mailTag: return try await tag(args)
        case .mailRetag:
            guard store.tagging.isEnabled else { throw ControlError("disabled", TagStore.offMessage) }
            store.tagging.retagAll()
            return ["started": true]
        case .mailSend: return try await send(args)
        case .mailReply: return try await reply(args)
        case .refresh:
            store.refresh()
            store.refreshCounts()
            return ["started": true]
        case .contactsSearch: return await searchContacts(args)
        case .chatSpaces: return try chatSpaces(args)
        case .chatMore:
            let account = try args.string("account").map { try chatAccount($0) }
            store.chat.loadMore(account: account)
            return ["started": true]
        case .chatRefresh:
            store.chat.refresh()
            return ["started": true]
        case .chatOpen:
            let key = try chatSpace(args)
            store.chat.open(key)
            return chatThread(key)
        case .chatClose:
            store.chat.close()
            return ["closed": true]
        case .chatMessages: return chatThread(try chatSpace(args))
        case .chatEarlier:
            let key = try chatSpace(args)
            store.chat.loadEarlier(key)
            return chatThread(key)
        case .chatSend: return try await chatSend(args)
        case .chatPeople: return try chatPeople(args)
        case .chatStart:
            let account = try chatAccount(try args.required("account"))
            let key = try await store.chat.start(account: account, with: args.list("people") ?? [])
            return ["account": .string(key.account), "space": .string(key.space)]
        case .chatAttachment:
            let account = try chatAccount(try args.required("account"))
            let file = try await store.chat.attachmentFile(account: account, resource: try args.required("resource"),
                                                           name: args.string("name") ?? "attachment")
            return ["path": .string(file.path)]
        }
    }

    // MARK: - Google Chat

    private func chatAccount(_ id: String) throws -> String {
        let id = id.lowercased()
        guard store.chat.account(named: id) != nil else {
            throw ControlError("unknown_account", "No chat account \(id); `mailstream chat spaces` lists them.")
        }
        return id
    }

    private func chatSpace(_ args: Arguments) throws -> ChatSpaceKey {
        ChatSpaceKey(account: try chatAccount(try args.required("account")), space: try args.required("space"))
    }

    private func chatSpaces(_ args: Arguments) throws -> JSON {
        let account = try args.string("account").map { try chatAccount($0) }
        let chat = store.chat
        let accounts = chat.accounts.map(\.name).filter { account == nil || $0 == account }
        return [
            "accounts": .array(accounts.map { name in
                ["id": .string(name), "label": .string(self.accounts.record(name)?.label ?? name),
                 "canStart": .bool(chat.canStartChats(account: name)), "unread": .int(chat.unreadSpaces(account: name))]
            }),
            "loading": .bool(chat.isLoading),
            "more": .bool(chat.canLoadMore(account: account)),
            "spaces": .array(chat.rows(account: account).map { row in
                let space = chat.space(row.key)
                return [
                    "account": .string(row.key.account), "space": .string(row.key.space),
                    "kind": (space?.kind.rawValue).json, "title": .string(row.title), "preview": .string(row.preview),
                    "date": .string(row.date), "section": .string(row.section), "activity": .date(row.activity),
                    "unread": .bool(row.isUnread), "attachment": .bool(row.hasAttachment),
                    "avatarKey": .string(row.avatarKey), "avatarName": .string(row.avatarName),
                    "avatarColorKey": .string(row.avatarColorKey), "avatarUrl": (row.avatarPhoto?.absoluteString).json,
                ]
            }),
        ]
    }

    private func chatUser(_ user: ChatUser) -> JSON {
        [
            "id": .string(user.name), "name": .string(user.label), "email": user.email.json,
            "avatarKey": .string(user.avatarKey), "avatarUrl": user.avatarUrl.json, "bot": .bool(user.bot),
        ]
    }

    /// What the store holds of `key`'s history. Senders' pictures go to the avatar store, so
    /// `mail avatars` finds them by address.
    private func chatThread(_ key: ChatSpaceKey) -> JSON {
        let chat = store.chat
        let thread = chat.thread(key) ?? ChatThread()
        for message in thread.messages {
            if let photo = message.sender.avatarUrl.flatMap(URL.init(string:)) {
                store.avatars.publish(photo, for: message.sender.avatarKey)
            }
        }
        return [
            "account": .string(key.account), "space": .string(key.space),
            "title": (chat.space(key)?.title).json, "me": chat.me(account: key.account).json,
            "members": .array((chat.space(key)?.members ?? []).map(chatUser)), "lastRead": .date(thread.lastRead),
            "messages": .array(thread.messages.map { message in
                [
                    "id": .string(message.name), "sender": chatUser(message.sender), "created": .date(message.created),
                    "edited": .date(message.edited), "text": .string(ChatMarkup.plain(message.text)),
                    "markup": .string(message.text),
                    "thread": message.thread.json, "threadReply": .bool(message.threadReply), "mine": .bool(message.mine),
                    "card": .bool(message.card),
                    "attachments": .array(message.attachments.map { attachment in
                        ["name": .string(attachment.name), "url": attachment.url.json,
                         "contentType": (attachment.contentType?.nilIfEmpty).json, "resource": attachment.resource.json]
                    }),
                ]
            }),
            "pending": .array(thread.pending.map { pending in
                ["id": .string(pending.id), "text": .string(pending.text), "created": .date(pending.created),
                 "failure": pending.failure.json]
            }),
            "more": .bool(thread.more), "loaded": .bool(thread.loaded), "failure": thread.failure.json,
            "loadingEarlier": .bool(chat.isLoadingEarlier(key)),
        ]
    }

    private func chatSend(_ args: Arguments) async throws -> JSON {
        let key = try chatSpace(args)
        let text = try args.required("text")
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw ControlError.usage("--text is empty.") }
        try requireYes(args, "Would send \(text.count) characters to \(key.space) from \(key.account). Re-run with --yes.")
        guard let sending = store.chat.send(text, thread: args.string("thread"), in: key) else {
            throw ControlError.usage("--text is empty.")
        }
        await sending.task.value
        if let failure = store.chat.thread(key)?.pending.first(where: { $0.id == sending.pending })?.failure {
            store.chat.discard(sending.pending, in: key)
            throw ControlError("provider", failure)
        }
        return ["sent": true, "account": .string(key.account), "space": .string(key.space)]
    }

    private func chatPeople(_ args: Arguments) throws -> JSON {
        let account = try chatAccount(try args.required("account"))
        return [
            "canStart": .bool(store.chat.canStartChats(account: account)),
            "people": .array(store.chat.people(account: account).map { chatUser($0.user) }),
        ]
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

    /// The signed-in account named by `--account`, as the account list knows it.
    private func record(_ args: Arguments) throws -> AccountRecord {
        let id = try args.required("account").lowercased()
        guard let record = accounts.record(id) else { throw Self.unknownAccount(id) }
        return record
    }

    /// `id` when the mail store has the account. Accounts restored from the cache are there before
    /// the account list loads, so this is not `record(_:)`.
    private func mailAccount(_ id: String) throws -> String {
        guard store.account(named: id) != nil else { throw Self.unknownAccount(id) }
        return id
    }

    private static func unknownAccount(_ id: String) -> ControlError {
        ControlError("unknown_account", "No account \(id); `mailstream accounts list` shows them.")
    }

    private func rename(_ args: Arguments) throws -> JSON {
        let record = try record(args)
        accounts.rename(record.id, label: args.string("label") ?? "")
        return accounts.record(record.id).map(accountJSON) ?? .null
    }

    private func removeAccount(_ args: Arguments) throws -> JSON {
        let record = try record(args)
        try requireYes(args, "Would sign out of \(record.id) and delete its saved sign-in (mail on the server stays). Re-run with --yes.")
        accounts.remove(record.id)
        return ["removed": .string(record.id)]
    }

    private func addAccount(_ args: Arguments) async throws -> JSON {
        guard let provider = Provider(rawValue: try args.required("provider")) else {
            throw ControlError.usage("provider must be google or microsoft.")
        }
        let suggested = try await accounts.tokens.suggestedClient(for: provider)
        let id = args.string("client-id") ?? suggested?.id
        let secret = args.string("client-secret") ?? (args.string("client-id") == nil ? suggested?.secret : nil)
        guard let id, !id.isEmpty else {
            throw ControlError.usage("No OAuth client known for \(provider.rawValue); pass --client-id (and --client-secret).")
        }
        if provider == .google, secret?.isEmpty != false {
            throw ControlError.usage("Google needs the client secret too: --client-secret.")
        }
        let record = try await accounts.signIn(provider, client: OAuthClient(id: id, secret: secret?.isEmpty == true ? nil : secret))
        return accountJSON(record)
    }

    private func mailboxes(_ args: Arguments) throws -> JSON {
        let names = try accountNames(args)
        return .object(Dictionary(uniqueKeysWithValues: names.map { name in
            (name, .array((store.mailboxes[name] ?? []).map { box in
                [
                    "id": .string(box.id), "name": .string(box.displayName), "role": .string(box.role.rawValue),
                    "unread": box.unread.json, "total": box.total.json,
                ]
            }))
        }))
    }

    private func accountFilter(_ args: Arguments) throws -> String? {
        try args.string("account").map { try mailAccount($0.lowercased()) }
    }

    /// The `--account` filter's account, or every account.
    private func accountNames(_ args: Arguments) throws -> [String] {
        try accountFilter(args).map { [$0] } ?? store.accounts.map(\.name)
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
                let accounts: [Account] = store.accounts.filter { account == nil || $0.name == account }
                let boxes: [Mailbox] = accounts.flatMap { store.mailboxes[$0.name] ?? [] }
                let matches = boxes.filter { $0.id == name || $0.name.caseInsensitiveCompare(name) == .orderedSame }
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
        let limit = min(max(args.int("limit") ?? 25, 1), 200)
        let category = args.string("category").flatMap(MailCategory.init(rawValue:))
        let include = { (conversation: Conversation) -> Bool in
            if args.bool("unread"), !conversation.isUnread { return false }
            if args.bool("starred"), !conversation.isFlagged { return false }
            let tag = self.store.tagging.tag(for: conversation)
            if let category, tag?.category != category { return false }
            if args.bool("waiting"), tag?.toRespond != true { return false }
            return true
        }
        var listing: MailStore.AgentListing
        if let query {
            listing = await store.agentSearch(try accountNames(args), query: query, pageSize: min(max(limit, 25), 200))
            listing.conversations = Array(listing.conversations.filter(include).prefix(limit))
        } else {
            let cursor = try args.string("cursor").map { text in
                guard let cursor = MailStore.AgentCursor(text) else { throw ControlError.usage("--cursor: not a `next` from mail list.") }
                return cursor
            }
            listing = await store.agentListing(try mailboxKeys(args), after: cursor, limit: limit,
                                               cached: args.bool("cached"), include: include)
        }
        if listing.primary.isEmpty, !listing.failures.isEmpty {
            throw ControlError("provider", listing.failures.joined(separator: "; "))
        }
        for conversation in listing.conversations { remember(conversation, primary: listing.primary) }
        // Previews missing now load in the background; `mail previews` waits for them.
        store.contents.prefetchSnippets(for: listing.conversations)
        var answer: [String: JSON] = ["conversations": .array(listing.conversations.map(summary)), "failures": .strings(listing.failures)]
        if query == nil { answer["next"] = (listing.next?.text).json }
        return .object(answer)
    }

    private func previews(_ args: Arguments) async throws -> JSON {
        let ids = args.list("ids") ?? []
        guard !ids.isEmpty else { throw ControlError.usage("Give at least one conversation id.") }
        let refs = try ids.map { id in (id, try lookup(id).conversation.latest.ref) }
        let contents = store.contents
        var out: [String: JSON] = [:]
        await forEachConcurrently(refs) { job in
            await contents.preview(job.1)
        } consume: { job, result in
            out[job.0] = (try? result.get() ?? nil).json
        }
        return .object(out)
    }

    private func avatars(_ args: Arguments) async throws -> JSON {
        let addresses = Array(Set((args.list("addresses") ?? []).map { $0.lowercased() }.filter { $0.contains("@") }))
        guard !addresses.isEmpty else { throw ControlError.usage("Give at least one address.") }
        let automated = Set((args.list("automated") ?? []).map { $0.lowercased() })
        let avatars = store.avatars
        var out: [String: JSON] = [:]
        await forEachConcurrently(addresses) { address in
            await avatars.pictureFile(for: address, automated: automated.contains(address))
        } consume: { address, result in
            out[address] = ((try? result.get() ?? nil)?.absoluteString).json
        }
        return .object(out)
    }

    private func show(_ args: Arguments) async throws -> JSON {
        let known = try lookup(try args.required("id"))
        let conversation = known.conversation
        var bodies: [MessageRef: Result<MessageContent, ControlError>] = [:]
        for message in conversation.messages {
            do {
                bodies[message.ref] = .success(try await store.contents.load(message.ref).content)
            } catch {
                bodies[message.ref] = .failure(ControlError(error))
            }
        }
        var out = summary(conversation).object ?? [:]
        out["messages"] = await Self.messages(conversation.messages, bodies: bodies, cache: store.cache,
                                              maxChars: max(args.int("max-chars") ?? 20_000, 0), html: args.bool("html"))
        return .object(out)
    }

    /// The messages of `mail show` with their bodies, off the main actor: stripping, cutting and
    /// rewriting bodies of several megabytes would hold up the front end's frames.
    @concurrent private nonisolated static func messages(_ messages: [MailMessage], bodies: [MessageRef: Result<MessageContent, ControlError>],
                                                         cache: MailCache, maxChars: Int, html: Bool) async -> JSON {
        .array(messages.map { message in
            let envelope = message.envelope
            var item: [String: JSON] = [
                "id": .string(message.ref.id), "mailbox": .string(message.ref.mailbox), "role": .string(message.role.rawValue),
                "from": addresses(envelope.from), "to": addresses(envelope.to),
                "date": .date(envelope.date), "subject": .string(envelope.subject),
                "unread": .bool(!envelope.isSeen), "starred": .bool(envelope.isFlagged),
            ]
            switch bodies[message.ref] {
            case .success(let content):
                let text = content.text.flatMap { $0.isEmpty ? nil : $0 } ?? content.html.map(HTMLText.strip) ?? ""
                let shown = text.prefix(maxChars)
                item["cc"] = addresses(content.cc)
                item["text"] = .string(String(shown))
                item["truncated"] = .bool(shown.endIndex < text.endIndex)
                if html, let body = content.html {
                    // Inline parts point at the reader's `mscid:` scheme; other clients read the cached files.
                    item["html"] = .string(body.replacingOccurrences(
                        of: cache.partURLBase(message.ref), with: cache.messageDirectory(message.ref).absoluteString))
                }
                item["attachments"] = .array(content.attachments.filter { !$0.inline }.map { attachment in
                    ["filename": .string(attachment.filename), "mime": .string(attachment.mime), "size": .int(attachment.size),
                     "path": .string(cache.attachmentURL(attachment, of: message.ref).path)]
                })
            case .failure(let error):
                item["error"] = .string(error.message)
            case nil:
                break
            }
            return .object(item)
        })
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
            "from": incoming.envelope.from.first.map(Self.text).json,
            "participants": .strings(people.map { own.contains($0.normalizedEmail) ? "me" : $0.displayName }),
            "messageCount": .int(conversation.messages.count),
            "unread": .bool(conversation.isUnread),
            "starred": .bool(conversation.isFlagged),
            "hasAttachment": .bool(conversation.hasAttachment),
            "mailboxes": .strings(Array(Set(conversation.messages.map(\.role.rawValue))).sorted()),
            "snippet": store.contents.snippet(for: conversation.latest.ref).json,
            "category": (tag?.category.rawValue).json,
            "waiting": .bool(tag?.toRespond == true),
            "labels": .strings(tag?.labels ?? []),
        ]
    }

    private nonisolated static func text(_ address: Address) -> String {
        address.name.map { "\($0) <\(address.email)>" } ?? address.email
    }

    private nonisolated static func addresses(_ list: [Address]) -> JSON { .strings(list.map(text)) }

    /// The addresses as a confirmation names them.
    private static func emails(_ list: [Address]) -> String { list.map(\.email).joined(separator: ", ") }

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
            return Known(conversation: onScreen, primary: Set(store.listedKeys))
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
        try await batch(args, rejectsEmpty: true, failureCodes: true) { id in
            let found = try lookup(id)
            switch try await action(found) {
            case .remember(let updated): known[id]?.conversation = updated
            case .forget: known[id] = nil
            }
            return .string(id)
        }
    }

    /// `{"done": [what work returned], "failed": [{id, (code,) message}]}` over the request's ids.
    /// `each` refuses an empty list and reports failure codes; `tag` does neither.
    private func batch(_ args: Arguments, rejectsEmpty: Bool, failureCodes: Bool,
                       _ work: (String) async throws -> JSON) async throws -> JSON {
        let ids = args.list("ids") ?? []
        if rejectsEmpty, ids.isEmpty { throw ControlError.usage("Give at least one conversation id.") }
        var done: [JSON] = []
        var failed: [JSON] = []
        for id in ids {
            do {
                done.append(try await work(id))
            } catch {
                let error = ControlError(error)
                var failure: [String: JSON] = ["id": .string(id), "message": .string(error.message)]
                if failureCodes { failure["code"] = .string(error.code) }
                failed.append(.object(failure))
            }
        }
        return ["done": .array(done), "failed": .array(failed)]
    }

    /// Refuses unless the request carries `yes`; `message` says what would have happened.
    private func requireYes(_ args: Arguments, _ message: @autoclosure () -> String) throws {
        guard args.bool("yes") else { throw ControlError("confirm", message()) }
    }

    // MARK: - Tags and sending

    private func tag(_ args: Arguments) async throws -> JSON {
        let category = try args.string("category").map { name in
            guard let category = MailCategory(rawValue: name) else { throw ControlError.usage("Unknown category \(name).") }
            return category
        }
        let waiting = args.string("waiting").map { $0 == "true" }
        let labels = args.list("labels")
        guard category != nil || waiting != nil || labels != nil else {
            throw ControlError.usage("Give --category, --waiting or --labels.")
        }
        return try await batch(args, rejectsEmpty: false, failureCodes: false) { id in
            let tag = try store.tagging.edit(try lookup(id).conversation, category: category, waiting: waiting, labels: labels)
            return ["id": .string(id), "category": .string(tag.category.rawValue), "waiting": .bool(tag.toRespond),
                    "labels": .strings(tag.labels)]
        }
    }

    private func addresses(_ args: Arguments, _ name: String) throws -> [Address] {
        try (args.list(name) ?? []).map { raw in
            guard let address = Address.parse(raw), address.email.contains("@") else {
                throw ControlError.usage("--\(name): \"\(raw)\" is not an email address.")
            }
            return address
        }
    }

    private func send(_ args: Arguments) async throws -> JSON {
        let from = try mailAccount(try args.required("from").lowercased())
        let to = try addresses(args, "to"), cc = try addresses(args, "cc"), bcc = try addresses(args, "bcc")
        guard !to.isEmpty else { throw ControlError.usage("--to needs at least one address.") }
        let subject = try args.required("subject"), body = try args.required("body")
        let attachments = try await Self.files(args.list("attachments") ?? [], argument: "attachments")
        let attached = attachments.isEmpty ? "" : ", \(attachments.count) attachment\(attachments.count == 1 ? "" : "s")"
        try requireYes(args, "Would send \"\(subject)\" (\(body.count) characters\(attached)) from \(from) to \(Self.emails(to + cc + bcc)). Re-run with --yes.")
        let message = await store.outgoing(account: from, source: nil, to: to, cc: cc, bcc: bcc, subject: subject, text: body,
                                           attachments: attachments)
        try await store.send(message, account: from)
        return ["sent": true, "from": .string(from), "to": Self.addresses(to), "subject": .string(subject)]
    }

    /// The files named by full path, each one a readable file rather than a folder.
    @concurrent private nonisolated static func files(_ paths: [String], argument: String) async throws -> [URL] {
        try paths.map { path in
            var isFolder: ObjCBool = false
            guard path.hasPrefix("/"), FileManager.default.fileExists(atPath: path, isDirectory: &isFolder), !isFolder.boolValue,
                  FileManager.default.isReadableFile(atPath: path) else {
                throw ControlError.usage("--\(argument): \"\(path)\" is not a readable file (give its full path).")
            }
            return URL(fileURLWithPath: path)
        }
    }

    private func reply(_ args: Arguments) async throws -> JSON {
        let conversation = try lookup(try args.required("id")).conversation
        let source = conversation.messages.last { $0.role != .drafts } ?? conversation.latest
        let plan = try await store.replyPlan(for: source, all: args.bool("all"))
        let body = try args.required("body")
        try requireYes(args, "Would send \"\(plan.subject)\" from \(source.ref.account) to \(Self.emails(plan.to + plan.cc)). Re-run with --yes.")
        try await store.sendReply(body, plan: plan, source: source)
        return ["sent": true, "from": .string(source.ref.account), "to": Self.addresses(plan.to + plan.cc),
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
                throw ControlError.usage("\(spec.name): \(argument.label) is required.")
            }
        }
        if let unknown = values.keys.first(where: { key in !spec.arguments.contains { $0.name == key } }) {
            throw ControlError.usage("\(spec.name) has no argument \"\(unknown)\".")
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
        guard let value = string(name) else { throw ControlError.usage("\(name) is required.") }
        return value
    }

    func int(_ name: String) -> Int? { values[name]?.int ?? string(name).flatMap { Int($0) } }
    func bool(_ name: String) -> Bool { values[name]?.bool ?? (string(name) == "true") }

    func list(_ name: String) -> [String]? {
        switch values[name] {
        case .array(let items): items.compactMap(\.string)
        case .string(let value): TypedValue.list(value)
        default: nil
        }
    }
}
