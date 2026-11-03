import Foundation

extension MailStore {
    /// Conversations of `keys` fetched fresh from the providers, independent of what the window
    /// shows. Inbox and sent mail already loaded thread in as context, as in the window's list.
    struct AgentListing {
        var conversations: [Conversation]
        /// Mailboxes the listing came from; "Archive" moves only messages in these.
        var primary: Set<MailboxKey>
        /// One line per mailbox that could not be fetched.
        var failures: [String]
    }

    func agentListing(_ keys: [MailboxKey], query: String?, pageSize: Int) async -> AgentListing {
        let jobs = keys.compactMap { key in accountStore.backend(for: key.account).map { (key, $0) } }
        var fetched: [MailboxKey: [Envelope]] = [:]
        var failures = keys.filter { key in !jobs.contains { $0.0 == key } }.map { "\($0.account): signed out" }
        await withTaskGroup(of: (MailboxKey, Result<[Envelope], Error>).self) { group in
            for (key, backend) in jobs {
                group.addTask {
                    do {
                        let list = if let query {
                            try await backend.search(mailbox: key.mailbox, query: query, pageSize: pageSize)
                        } else {
                            try await backend.envelopes(mailbox: key.mailbox, cursor: nil, pageSize: pageSize,
                                                        priority: .interactive).envelopes
                        }
                        return (key, .success(list))
                    } catch {
                        return (key, .failure(error))
                    }
                }
            }
            for await (key, result) in group {
                switch result {
                case .success(let list): fetched[key] = list
                case .failure(let error): failures.append("\(key.account) \(mailbox(key)?.displayName ?? key.mailbox): \(error.localizedDescription)")
                }
            }
        }
        let primary = keys.filter { fetched[$0] != nil }
        let roles = Set(primary.map { mailbox($0)?.role ?? .other })
        var context: [MailboxKey] = []
        if !roles.isSubset(of: [.trash, .junk, .drafts, .all]) {
            for account in Set(primary.map(\.account)) {
                for role in [MailboxRole.inbox, .sent] {
                    if let key = mailbox(account: account, role: role)?.key, fetched[key] == nil { context.append(key) }
                }
            }
        }
        func source(_ key: MailboxKey, _ envelopes: [Envelope]) -> ListInput.Source {
            ListInput.Source(key: key, role: mailbox(key)?.role ?? .other, envelopes: envelopes)
        }
        let input = ListInput(primary: primary.map { source($0, fetched[$0] ?? []) },
                              context: context.map { source($0, loadedEnvelopes($0)) },
                              own: ownAddresses, recipients: false)
        let output = await input.build()
        return AgentListing(conversations: output.conversations, primary: Set(primary), failures: failures.sorted())
    }
}
