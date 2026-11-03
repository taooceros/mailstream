import Foundation

/// What a reply goes out with, worked out before the user hits Send so the recipients can be shown.
struct ReplyPlan: Sendable {
    var to: [Address]
    var cc: [Address]
    var subject: String
    /// himalaya's `> `-quoted text of the source, without the attribution line.
    var quotedText: String
    /// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:"
    var attribution: String

    /// The plain-text body: the written text above the attributed quote.
    func plainBody(_ text: String) -> String {
        quotedText.isEmpty ? text : text + "\n\n" + attribution + "\n" + quotedText
    }
}

extension MailStore {
    /// Recipients the way mail clients derive them: himalaya answers `Reply-To` or `From`; reply-all
    /// adds everyone else on the source except the user's own addresses.
    func replyPlan(for source: MailMessage, all: Bool) async throws -> ReplyPlan {
        let draft = try await himalaya.draft(mode: .reply(source.ref), account: source.ref.account)
        var to = draft?.to.compactMap(Address.parse) ?? []
        var cc: [Address] = []
        if all {
            let content = try? await content(for: source.ref)
            var seen = Set(to.map(\.normalizedEmail)).union(ownAddresses)
            for address in source.envelope.to + source.envelope.from + (content?.cc ?? [])
                where seen.insert(address.normalizedEmail).inserted {
                cc.append(address)
            }
            if to.isEmpty, let first = cc.first { to = [first]; cc.removeFirst() }
        }
        let quoted = (draft?.body ?? "").replacingOccurrences(of: "\r\n", with: "\n").trimmingCharacters(in: .newlines)
        return ReplyPlan(to: to, cc: cc, subject: draft?.subject ?? source.envelope.subject,
                         quotedText: quoted, attribution: Self.attribution(for: source))
    }

    static func attribution(for source: MailMessage) -> String {
        let sender = source.envelope.from.first.map { $0.name ?? $0.email } ?? "Someone"
        let when = source.envelope.date.map {
            $0.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().year().hour().minute())
        }
        return "On \(when.map { "\($0), " } ?? "")\(sender) wrote:"
    }

    /// Sends an inline reply as plain text plus HTML, the HTML quoting the source's own HTML body.
    func sendReply(_ text: String, plan: ReplyPlan, source: MailMessage) async throws {
        let content = try? await content(for: source.ref)
        let fields = Himalaya.ComposeFields(from: account(named: source.ref.account)?.identity?.rfc5322,
                                            to: plan.to.map(\.rfc5322), cc: plan.cc.map(\.rfc5322), bcc: [],
                                            subject: plan.subject, body: plan.plainBody(text), attachments: [])
        let html = ReplyHTML.document(reply: text, attribution: plan.attribution, source: content)
        try await himalaya.sendReply(fields, html: html, to: source.ref, account: source.ref.account)
        Task { await didSend(.reply(source.ref)) }
    }
}

/// The HTML alternative of a reply, laid out the way Gmail does so recipients' clients fold the quote.
enum ReplyHTML {
    static func document(reply: String, attribution: String, source: MessageContent?) -> String {
        let written = reply.replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.isEmpty ? "<div><br></div>" : "<div>\(HTMLText.linkify(String($0)))</div>" }
            .joined(separator: "\n")
        var html = "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\n<div dir=\"auto\">\(written)</div>\n"
        let quote: String? = if let source = source?.html {
            bodyContents(source)
        } else if let text = source?.text {
            "<div style=\"white-space:pre-wrap\">\(HTMLText.plainToHTML(text))</div>"
        } else {
            nil
        }
        if let quote {
            html += "<br><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">\(HTMLText.escape(attribution))<br></div>\n"
                + "<blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">\n"
                + quote + "\n</blockquote></div>\n"
        }
        return html + "</body></html>\n"
    }

    /// The inside of `<body>`, without images of inline parts: those are not re-attached, so they
    /// would arrive broken.
    static func bodyContents(_ html: String) -> String {
        var inner = html
        if let open = html.range(of: "<body[^>]*>", options: [.regularExpression, .caseInsensitive]) {
            inner = String(html[open.upperBound...])
            if let close = inner.range(of: "</body>", options: [.caseInsensitive, .backwards]) {
                inner = String(inner[..<close.lowerBound])
            }
        } else {
            inner = inner.replacingOccurrences(of: "<head[^>]*>[\\s\\S]*?</head>|<!doctype[^>]*>|</?html[^>]*>", with: "",
                                               options: [.regularExpression, .caseInsensitive])
        }
        return inner.replacingOccurrences(of: "<img[^>]*src\\s*=\\s*[\"']?\(MailCache.scheme):[^>]*>", with: "",
                                          options: [.regularExpression, .caseInsensitive])
    }
}
