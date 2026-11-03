import Foundation

/// When remote images and fonts load on their own, following the "known senders" policy of most
/// mail clients: mail from the user's own addresses, from anyone they have written to, or from a
/// sender they chose to always trust loads remote content; junk never does; everything else waits
/// for a click on the banner.
extension MailStore {
    func loadsRemoteContent(for conversation: Conversation) -> Bool {
        !conversation.messages.contains { $0.role == .junk } && untrustedSenders(in: conversation).isEmpty
    }

    /// Distinct senders of the conversation whose remote content does not load automatically.
    func untrustedSenders(in conversation: Conversation) -> [Address] {
        let own = ownAddresses
        let trusted = trustedSenders
        // Replies inside this thread count too, so a search result's sender is known before the
        // sent mailbox that holds the reply is loaded.
        var known = correspondents()
        for message in conversation.messages where message.envelope.from.contains(where: { own.contains($0.normalizedEmail) }) {
            known.formUnion(message.envelope.to.map(\.normalizedEmail))
        }
        var seen = Set<String>()
        return conversation.messages.flatMap(\.envelope.from).filter { sender in
            let email = sender.normalizedEmail
            return !own.contains(email) && !known.contains(email) && !trusted.contains(email) && seen.insert(email).inserted
        }
    }

    /// Always loads remote content from `senders` from now on (`remoteContent.trustedSenders`).
    func trustRemoteContent(from senders: [Address]) {
        let updated = trustedSenders.union(senders.map(\.normalizedEmail))
        do {
            try Settings.shared.set(.trustedSenders, .strings(updated.sorted()))
        } catch {
            NotificationCenter.default.post(name: .storeError, object: self,
                                            userInfo: ["message": error.localizedDescription, "alert": true])
        }
    }

    private var trustedSenders: Set<String> { Set(Settings.shared.strings(.trustedSenders)) }
}
