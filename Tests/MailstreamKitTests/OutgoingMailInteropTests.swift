import Foundation
import Testing
@testable import MailstreamKit

/// Outgoing mail as the Swift engine writes it, for `scripts/outgoing-mail-interop.sh` to hold the
/// Rust engine (`Engine/`, MS-114) against: for the inputs of `Tests/Fixtures/outgoing-mail-interop.json`,
/// the rendered MIME messages, the reply pieces (recipients, subject, quote, attribution, HTML)
/// and the ranked contacts with their completions, written as JSON to the file
/// `MAILSTREAM_OUTGOING_INTEROP` names. Without the variable the test does nothing.
struct OutgoingMailInteropTests {
    struct Fixture: Decodable {
        struct File: Decodable {
            let name: String
            let base64: String
        }

        struct Message: Decodable {
            let from: Address
            let to: [Address]
            let cc: [Address]
            let bcc: [Address]
            let subject: String
            let text: String
            let html: String?
            let attachments: [File]?
            let inReplyTo: String?
            let references: [String]?
            let thread: String?
        }

        struct Source: Decodable {
            let ref: MessageRef
            let role: MailboxRole
            let envelope: Envelope
        }

        struct Reply: Decodable {
            let source: Source
            let content: MessageContent?
            let own: [String]
            let all: Bool
            let reply: String
        }

        struct Listing: Decodable {
            let account: String
            let envelopes: [Envelope]
        }

        struct Person: Decodable {
            let name: String?
            let emails: [String]
        }

        struct Contacts: Decodable {
            let own: [String]
            let listings: [Listing]
            let people: [Person]
            let queries: [String]
        }

        let messages: [Message]
        let replies: [Reply]
        let contacts: Contacts
    }

    nonisolated static let output = ProcessInfo.processInfo.environment["MAILSTREAM_OUTGOING_INTEROP"]

    @Test(.enabled(if: output != nil)) @MainActor func writesWhatTheRustEngineIsHeldTo() async throws {
        let url = URL(filePath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "Fixtures/outgoing-mail-interop.json")
        let fixture = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: url))
        let folder = FileManager.default.temporaryDirectory.appending(path: "mailstream-outgoing-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }

        var mime: [String] = []
        for (index, message) in fixture.messages.enumerated() {
            let files = try (message.attachments ?? []).map { file in
                let directory = folder.appending(path: "\(index)")
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let path = directory.appending(path: file.name)
                try #require(Data(base64Encoded: file.base64)).write(to: path)
                return path
            }
            let outgoing = OutgoingMessage(from: message.from, to: message.to, cc: message.cc, bcc: message.bcc,
                                           subject: message.subject, text: message.text, html: message.html,
                                           attachments: files, inReplyTo: message.inReplyTo,
                                           references: message.references ?? [], thread: message.thread)
            mime.append(String(decoding: try MIMEMessage.render(outgoing), as: UTF8.self))
        }

        let replies: [[String: Any]] = fixture.replies.map { reply in
            let source = MailMessage(ref: reply.source.ref, envelope: reply.source.envelope, role: reply.source.role)
            let recipients = MailStore.replyRecipients(for: source, content: reply.content, own: Set(reply.own), all: reply.all)
            let plan = ReplyPlan(to: recipients.to, cc: recipients.cc, subject: Subject.reply(to: source.envelope.subject),
                                 quotedText: MailStore.quote(reply.content?.text ?? ""),
                                 attribution: MailStore.attribution(for: source))
            return [
                "to": plan.to.map(Self.text), "cc": plan.cc.map(Self.text), "subject": plan.subject,
                "quotedText": plan.quotedText, "attribution": plan.attribution, "plainBody": plan.plainBody(reply.reply),
                "html": ReplyHTML.document(reply: reply.reply, attribution: plan.attribution, source: reply.content),
            ]
        }

        let contacts = fixture.contacts
        let ranked = await MailStore.rankContacts(contacts.listings.map { ($0.account, $0.envelopes) },
                                                  people: contacts.people.map { DirectoryPerson(name: $0.name, emails: $0.emails) },
                                                  own: Set(contacts.own))
        let completion = RecipientCompletion()
        completion.contacts = ranked
        var suggestions: [String: [String]] = [:]
        for query in contacts.queries {
            suggestions[query] = completion.suggestions(for: query).map(\.address.email)
        }

        let answer: [String: Any] = [
            "mime": mime, "replies": replies,
            "contacts": ["ranked": ranked.map { Self.text($0.address) }, "suggestions": suggestions],
        ]
        let data = try JSONSerialization.data(withJSONObject: answer, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: URL(filePath: try #require(Self.output)))
    }

    /// `name|address`, so names compare exactly.
    private static func text(_ address: Address) -> String { "\(address.name ?? "")|\(address.email)" }
}
