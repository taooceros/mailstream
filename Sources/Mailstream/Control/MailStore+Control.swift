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

    func agentListing(_ keys: [MailboxKey], pageSize: Int) async -> AgentListing {
        let jobs = backends(keys, account: \.account)
        var fetched: [MailboxKey: [Envelope]] = [:]
        var failures = keys.filter { key in !jobs.contains { $0.key == key } }.map { "\($0.account): signed out" }
        await forEachConcurrently(jobs) { job in
            try await job.backend.envelopes(mailbox: job.key.mailbox, cursor: nil, pageSize: pageSize, priority: .interactive).envelopes
        } consume: { job, result in
            let key = job.key
            switch result {
            case .success(let list): fetched[key] = list
            case .failure(let error): failures.append("\(key.account) \(mailbox(key)?.displayName ?? key.mailbox): \(error.localizedDescription)")
            }
        }
        let inboxes = !keys.isEmpty && keys.allSatisfy { mailbox($0)?.role == .inbox }
        return await threads(fetched, failures: failures, mergeRepeats: inboxes)
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
        return await threads(fetched, failures: failures, mergeRepeats: false)
    }

    private func threads(_ fetched: [MailboxKey: [Envelope]], failures: [String], mergeRepeats: Bool) async -> AgentListing {
        let primary = fetched.keys.sorted { ($0.account, $0.mailbox) < ($1.account, $1.mailbox) }
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
                              own: ownAddresses, recipients: false, mergeRepeats: mergeRepeats)
        let output = await input.build()
        return AgentListing(conversations: output.conversations, primary: Set(primary), failures: failures.sorted())
    }
}
