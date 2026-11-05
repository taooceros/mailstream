import Foundation

extension MailStore {
    /// Conversations for the control socket, independent of what the window shows. Inbox and sent
    /// mail already loaded thread in as context, as in the window's list.
    struct AgentListing {
        var conversations: [Conversation]
        /// Mailboxes the listing came from; "Archive" moves only messages in these.
        var primary: Set<MailboxKey>
        /// One line per mailbox that could not be fetched.
        var failures: [String]
        /// Where the next page starts; nil after the last.
        var next: AgentCursor? = nil
    }

    /// The position after a conversation in a newest-first listing: its date and id.
    nonisolated struct AgentCursor: Sendable {
        let date: Date
        let id: String

        init(after conversation: Conversation) {
            date = conversation.latestDate ?? .distantPast
            id = conversation.id
        }

        /// `<milliseconds since 1970>|<conversation id>`, opaque to clients.
        init?(_ text: String) {
            guard let bar = text.firstIndex(of: "|"), let millis = Int64(text[..<bar]) else { return nil }
            date = Date(timeIntervalSince1970: Double(millis) / 1000)
            id = String(text[text.index(after: bar)...])
        }

        var text: String { "\(Int64((date.timeIntervalSince1970 * 1000).rounded()))|\(id)" }

        /// Where the page after this cursor starts in `conversations`.
        func start(in conversations: [Conversation]) -> Int {
            if let index = conversations.firstIndex(where: { $0.id == id }) { return index + 1 }
            return conversations.firstIndex { ($0.latestDate ?? .distantPast) < date } ?? conversations.count
        }
    }

    /// Page fetches one listing request may make past the mail already kept, so a rare filter
    /// cannot page through a whole mailbox.
    private static let agentPageRounds = 6

    /// One page of `keys`' conversations that pass `include`, newest first, from the listings the
    /// window uses (memory, then disk). The first page fetches the providers' first pages unless
    /// `cached`; later pages fetch further pages until the page is complete: every mailbox with
    /// more mail has been listed back past the page's oldest conversation, so none can still be
    /// missing from it.
    func agentListing(_ keys: [MailboxKey], after cursor: AgentCursor?, limit: Int, cached: Bool = false,
                      include: (Conversation) -> Bool) async -> AgentListing {
        var failures = keys.filter { key in accountStore.backend(for: key.account) == nil }.map { "\($0.account): signed out" }
        let context = contextKeys(for: keys)
        await loadCachedListings(keys + context)
        if cursor == nil, !cached {
            let failed = await fetchFirstPages(keys + context)
            failures += failed.filter { keys.contains($0.key) }.map { key, error in
                "\(key.account) \(mailbox(key)?.displayName ?? key.mailbox): \(error.localizedDescription)"
            }
        }
        let inboxes = !keys.isEmpty && keys.allSatisfy { mailbox($0)?.role == .inbox }
        var round = 0
        while true {
            let listed = await threads(Dictionary(uniqueKeysWithValues: keys.map { ($0, loadedEnvelopes($0)) }),
                                       context: context, failures: failures.sorted(), mergeRepeats: inboxes)
            let matching = listed.conversations.filter(include)
            let start = cursor?.start(in: matching) ?? 0
            let end = min(start + limit, matching.count)
            // The page's oldest date when the page is full; a short page needs every mailbox exhausted.
            let floor = start + limit <= matching.count ? matching[end - 1].latestDate ?? .distantPast : nil
            let short = keys.filter { key in
                guard hasNextPage(key) else { return false }
                guard let floor else { return true }
                return (loadedEnvelopes(key).compactMap(\.date).min() ?? .distantFuture) > floor
            }
            if short.isEmpty || round == Self.agentPageRounds {
                var page = listed
                page.conversations = Array(matching[start..<end])
                let more = matching.count > end || keys.contains(where: hasNextPage)
                page.next = more ? page.conversations.last.map(AgentCursor.init(after:)) : nil
                return page
            }
            await fetchNextPages(short)
            round += 1
        }
    }

    /// The providers' search over every mailbox of `accounts` but trash and junk, as in the window.
    func agentSearch(_ accounts: [String], query: String, pageSize: Int) async -> AgentListing {
        let jobs = backends(accounts, account: \.self)
        var fetched: [MailboxKey: [Envelope]] = [:]
        var failures = accounts.filter { account in !jobs.contains { $0.key == account } }.map { "\($0): signed out" }
        await forEachConcurrently(jobs) { job in
            try await job.backend.search(query: query, pageSize: pageSize)
        } consume: { job, result in
            let account = job.key
            switch result {
            case .success(let hits):
                for hit in hits {
                    let key = MailboxKey(account: account, mailbox: hit.mailbox)
                    guard let role = mailbox(key)?.role, role != .trash, role != .junk else { continue }
                    fetched[key, default: []].append(hit.envelope)
                }
            case .failure(let error): failures.append("\(account): \(error.localizedDescription)")
            }
        }
        let primary = Array(fetched.keys)
        let context = contextKeys(for: primary).filter { fetched[$0] == nil }
        await loadCachedListings(context)
        return await threads(fetched, context: context, failures: failures.sorted(), mergeRepeats: false)
    }

    /// Inbox and sent of the accounts of `keys`, so conversations show both directions; none for
    /// trash, junk, drafts and All Mail alone.
    private func contextKeys(for keys: [MailboxKey]) -> [MailboxKey] {
        let roles = Set(keys.map { mailbox($0)?.role ?? .other })
        guard !roles.isSubset(of: [.trash, .junk, .drafts, .all]) else { return [] }
        return Set(keys.map(\.account)).sorted().flatMap { account in
            [MailboxRole.inbox, .sent].compactMap { mailbox(account: account, role: $0)?.key }
        }.filter { !keys.contains($0) }
    }

    private func threads(_ fetched: [MailboxKey: [Envelope]], context: [MailboxKey], failures: [String],
                         mergeRepeats: Bool) async -> AgentListing {
        let primary = fetched.keys.sorted { ($0.account, $0.mailbox) < ($1.account, $1.mailbox) }
        func source(_ key: MailboxKey, _ envelopes: [Envelope]) -> ListInput.Source {
            ListInput.Source(key: key, role: mailbox(key)?.role ?? .other, envelopes: envelopes)
        }
        let input = ListInput(primary: primary.map { source($0, fetched[$0] ?? []) },
                              context: context.map { source($0, loadedEnvelopes($0)) },
                              own: ownAddresses, recipients: false, mergeRepeats: mergeRepeats)
        let output = await input.build()
        return AgentListing(conversations: output.conversations, primary: Set(primary), failures: failures)
    }
}
