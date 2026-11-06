import Foundation
import Testing
@testable import MailstreamKit

struct ReplyAttachmentTests {
    @Test func replyArgumentsDistinguishOmittedAndClearedFields() throws {
        let spec = try #require(CommandSpec.named("mail.reply"))
        let required: [String: JSON] = ["id": .string("conversation"), "body": .string("Reply")]
        let defaults = try Arguments(spec: spec, values: required)
        #expect(defaults.list("to") == nil)
        #expect(defaults.list("cc") == nil)
        #expect(defaults.string("subject") == nil)
        #expect(defaults.list("attachments") == nil)
        let cleared = try Arguments(spec: spec, values: required.merging([
            "to": .array([]), "cc": .array([]), "bcc": .array([]),
            "subject": .string(""), "attachments": .array([]),
        ]) { _, new in new })
        #expect(cleared.list("to") == [])
        #expect(cleared.list("cc") == [])
        #expect(cleared.list("bcc") == [])
        #expect(cleared.string("subject") == "")
        #expect(cleared.list("attachments") == [])
        let nulls = try Arguments(spec: spec, values: required.merging([
            "to": .null, "cc": .null, "subject": .null,
        ]) { _, new in new })
        #expect(nulls.list("to") == nil)
        #expect(nulls.list("cc") == nil)
        #expect(nulls.string("subject") == nil)
    }

    @Test func replyMIMEKeepsThreadQuoteAndAttachmentBytes() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("ms68-\(UUID()).txt")
        let bytes = Data("MS-68 attachment payload".utf8)
        try bytes.write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let plan = ReplyPlan(to: [Address(name: nil, email: "edited@example.com")], cc: [], subject: "Edited subject",
                             quotedText: "> Original body", attribution: "Original sender wrote:")
        let message = OutgoingMessage(from: Address(name: nil, email: "self@example.com"),
                                      to: plan.to, cc: plan.cc, bcc: [Address(name: nil, email: "copy@example.com")],
                                      subject: plan.subject, text: plan.plainBody("Reply body"),
                                      html: ReplyHTML.document(reply: "Reply body", attribution: plan.attribution, source: nil),
                                      attachments: [file], inReplyTo: "source@example.com",
                                      references: ["ancestor@example.com", "source@example.com"], thread: "thread")
        let mime = String(decoding: try MIMEMessage.render(message), as: UTF8.self)
        #expect(mime.contains("To: edited@example.com\r\n"))
        #expect(mime.contains("Bcc: copy@example.com\r\n"))
        #expect(mime.contains("Subject: Edited subject\r\n"))
        #expect(mime.contains("In-Reply-To: <source@example.com>\r\n"))
        #expect(mime.contains("References: <ancestor@example.com> <source@example.com>\r\n"))
        #expect(mime.contains("multipart/mixed"))
        #expect(mime.contains("multipart/alternative"))
        #expect(mime.contains("> Original body"))
        #expect(mime.contains("filename=\"\(file.lastPathComponent)\""))
        #expect(mime.contains(bytes.base64EncodedString()))
    }
}
