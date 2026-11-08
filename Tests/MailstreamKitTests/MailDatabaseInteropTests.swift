import Foundation
import Testing
@testable import MailstreamKit

/// mail.db as the Swift and the Rust engine (`Engine/`, MS-113) both keep it, checked across
/// engines by `scripts/mail-db-interop.sh`: each engine writes `Tests/Fixtures/mail-db-interop.json`
/// into a throwaway home (`MAILSTREAM_INTEROP=write`), then reads the home the other one wrote
/// (`MAILSTREAM_INTEROP=read`). Without the variable the test does nothing, so it never touches the
/// person's own mail.db.
struct MailDatabaseInteropTests {
    struct Fixture: Decodable {
        struct Listing: Decodable {
            let key: MailboxKey
            let envelopes: [Envelope]
            let next: String?
        }

        struct Content: Decodable {
            let ref: MessageRef
            let content: MessageContent
        }

        struct Chat: Decodable {
            struct Page: Decodable {
                let key: ChatSpaceKey
                let page: ChatMessagePage
            }

            let accounts: [ChatAccount]
            let recent: [ChatSpaceKey]
            let spaces: [String: ChatSpaceList]
            let messages: [Page]
        }

        let accounts: [Account]
        let mailboxes: [String: [Mailbox]]
        let listings: [Listing]
        let contents: [Content]
        let chat: Chat
    }

    nonisolated static let mode = ProcessInfo.processInfo.environment["MAILSTREAM_INTEROP"]

    @Test(.enabled(if: mode != nil)) func readsAndWritesWhatTheRustEngineDoes() async throws {
        let home = try #require(ProcessInfo.processInfo.environment["CFFIXED_USER_HOME"])
        #expect(URL.applicationSupportDirectory.path.hasPrefix(home), "mail.db must be in the throwaway home")
        let url = URL(filePath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "Fixtures/mail-db-interop.json")
        let fixture = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: url))
        let cache = MailCache()
        if Self.mode == "write" {
            cache.saveAccounts(fixture.accounts)
            for (account, mailboxes) in fixture.mailboxes { cache.saveMailboxes(mailboxes, for: account) }
            for listing in fixture.listings {
                cache.saveListing(CachedListing(envelopes: listing.envelopes, next: listing.next), for: listing.key)
            }
            for item in fixture.contents { cache.saveContent(item.content, for: item.ref) }
            cache.saveChatAccounts(fixture.chat.accounts)
            cache.saveChatRecent(fixture.chat.recent)
            for (account, list) in fixture.chat.spaces { cache.saveChatSpaces(list, for: account) }
            for item in fixture.chat.messages { cache.saveChatMessages(item.page, for: item.key) }
            // Writes are queued: wait until every one reads back before the process ends.
            let start = ContinuousClock.now
            while await mismatches(cache, fixture).count > 0, ContinuousClock.now - start < .seconds(10) {
                try await Task.sleep(for: .milliseconds(20))
            }
        }
        let differences = await mismatches(cache, fixture)
        #expect(differences.isEmpty, "\(differences)")
    }

    /// What `cache` holds that differs from `fixture`, one line each.
    private func mismatches(_ cache: MailCache, _ fixture: Fixture) async -> [String] {
        var out: [String] = []
        if await cache.loadAccounts() != fixture.accounts { out.append("accounts") }
        for (account, mailboxes) in fixture.mailboxes where await cache.loadMailboxes(account) != mailboxes {
            out.append("mailboxes of \(account)")
        }
        for listing in fixture.listings {
            let loaded = await cache.loadListing(listing.key)
            if loaded?.envelopes != listing.envelopes || loaded?.next != listing.next {
                out.append("listing \(listing.key.account) \(listing.key.mailbox)")
            }
        }
        for item in fixture.contents {
            let loaded = await cache.loadContent(item.ref)
            let expected = item.content
            if loaded?.html != expected.html || loaded?.text != expected.text || loaded?.cc != expected.cc
                || loaded?.replyTo != expected.replyTo || loaded?.references != expected.references
                || loaded?.attachments != expected.attachments || loaded?.snippet != expected.snippet {
                out.append("content of \(item.ref.id)")
            }
        }
        if await cache.loadChatAccounts() != fixture.chat.accounts { out.append("chat accounts") }
        if await cache.loadChatRecent() != fixture.chat.recent { out.append("chat recent") }
        for (account, list) in fixture.chat.spaces {
            let loaded = await cache.loadChatSpaces(account)
            if loaded?.me != list.me || loaded?.more != list.more || loaded?.spaces != list.spaces {
                out.append("chat spaces of \(account)")
            }
        }
        for item in fixture.chat.messages {
            let loaded = await cache.loadChatMessages(item.key)
            if loaded?.messages != item.page.messages || loaded?.more != item.page.more || loaded?.lastRead != item.page.lastRead {
                out.append("chat messages of \(item.key.space)")
            }
        }
        return out
    }
}
