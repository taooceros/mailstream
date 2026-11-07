import Foundation

/// What a composer writes: a new message, or a reply to or forward of a message.
enum ComposeMode: Sendable {
    case new
    case reply(MessageRef)
    case forward(MessageRef)
}

/// What a reply goes out with, worked out before the user hits Send so the recipients can be shown.
struct ReplyPlan: Sendable {
    var to: [Address]
    var cc: [Address]
    var subject: String
    /// The source's text with every line `> `-quoted, without the attribution line.
    var quotedText: String
    /// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:"
    var attribution: String

    /// The plain-text body: the written text above the attributed quote.
    func plainBody(_ text: String) -> String {
        quotedText.isEmpty ? text : text + "\n\n" + attribution + "\n" + quotedText
    }
}

/// A forward's prefilled subject and body: the source under a forwarded-message header.
struct ForwardDraft: Sendable {
    var subject: String
    var body: String
}

/// Someone recipient fields complete to.
nonisolated struct Contact: Sendable {
    let address: Address
    /// Messages the user sent them.
    let sent: Int
    /// When they last appeared on a message.
    let latest: Date
    /// One of the user's own addresses.
    let isOwn: Bool
    /// Folded name and its words, address and the parts of its local part: typing matches their start.
    private let keys: [String]

    init(address: Address, sent: Int, latest: Date, isOwn: Bool) {
        self.address = address
        self.sent = sent
        self.latest = latest
        self.isOwn = isOwn
        let email = Contact.fold(address.email)
        var keys = [email]
        keys += (email.split(separator: "@").first ?? "").split(whereSeparator: { ".-_+".contains($0) }).map(String.init)
        if address.displayName != address.email {
            let name = Contact.fold(address.displayName)
            keys.append(name)
            keys += name.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init)
        }
        self.keys = keys
    }

    /// `no-reply@…`, `notifications@…`: senders nobody writes to.
    static func isNoReply(_ email: String) -> Bool {
        let local = email.lowercased().split(separator: "@").first.map(String.init) ?? ""
        let squashed = local.filter(\.isLetter)
        return squashed.hasPrefix("noreply") || squashed.hasPrefix("donotreply")
            || ["mailerdaemon", "notification", "notifications", "bounce", "bounces"].contains(squashed)
    }

    /// Whether typing `needle`, already `Contact.fold`ed, offers this contact.
    func matches(_ needle: String) -> Bool {
        keys.contains { $0.hasPrefix(needle) }
    }

    /// People in `contacts` (best first) whose name, a word of it, or address starts with `typed`,
    /// leaving out the normalized addresses in `taken`; an address typed out in full first of all.
    static func suggestions(for typed: String, in contacts: [Contact], taken: Set<String> = []) -> [Contact] {
        let needle = fold(typed.trimmingCharacters(in: .whitespaces))
        guard !needle.isEmpty else { return [] }
        var matches = Array(contacts.lazy.filter { !taken.contains($0.address.normalizedEmail) && $0.matches(needle) }.prefix(12))
        if let exact = matches.firstIndex(where: { fold($0.address.email) == needle }) {
            matches.insert(matches.remove(at: exact), at: 0)
        }
        return matches
    }

    /// `Name <address>` as a completion shows it, without the characters that end a token, so
    /// picking it gives one token.
    static func label(_ address: Address) -> String {
        let name = address.displayName.components(separatedBy: CharacterSet(charactersIn: ",;\"<>"))
            .joined(separator: " ").split(separator: " ").joined(separator: " ")
        return name.isEmpty || address.displayName == address.email ? address.email : "\(name) <\(address.email)>"
    }

    /// Case- and accent-insensitive form completions compare.
    static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
    }
}

extension MailStore {
    /// Recipients the way mail clients derive them: `Reply-To`, else `From`, or for one of the
    /// user's own messages its recipients; reply-all copies everyone else on the source except
    /// the user's own addresses.
    nonisolated static func replyRecipients(for source: MailMessage, content: MessageContent?, own: Set<String>,
                                            all: Bool) -> (to: [Address], cc: [Address]) {
        let envelope = source.envelope
        let fromMe = source.isFromUser(own: own)
        let replyTo = content?.replyTo ?? []
        var to = fromMe ? envelope.to : (replyTo.isEmpty ? envelope.from : replyTo)
        guard all else { return (to, []) }
        var seen = Set(to.map(\.normalizedEmail)).union(own)
        var cc = (envelope.to + (content?.cc ?? [])).filter { seen.insert($0.normalizedEmail).inserted }
        if to.isEmpty, !cc.isEmpty { to = [cc.removeFirst()] }
        return (to, cc)
    }

    func replyPlan(for source: MailMessage, all: Bool) async throws -> ReplyPlan {
        let content = try await contents.load(source.ref).content
        let recipients = Self.replyRecipients(for: source, content: content, own: ownAddresses, all: all)
        return ReplyPlan(to: recipients.to, cc: recipients.cc, subject: Subject.reply(to: source.envelope.subject),
                         quotedText: Self.quote(content.text ?? ""), attribution: Self.attribution(for: source))
    }

    func forwardDraft(for source: MailMessage) async throws -> ForwardDraft {
        let content = try await contents.load(source.ref).content
        let envelope = source.envelope
        var header = ["---------- Forwarded message ---------",
                      "From: " + envelope.from.map(Self.headerForm).joined(separator: ", ")]
        if let date = envelope.date { header.append("Date: " + Self.attributionDate(date)) }
        header.append("Subject: " + envelope.subject)
        header.append("To: " + envelope.to.map(Self.headerForm).joined(separator: ", "))
        if !content.cc.isEmpty { header.append("Cc: " + content.cc.map(Self.headerForm).joined(separator: ", ")) }
        let text = (content.text ?? "").replacingOccurrences(of: "\r\n", with: "\n")
        return ForwardDraft(subject: Subject.forward(envelope.subject),
                            body: header.joined(separator: "\n") + "\n\n" + text)
    }

    static func attribution(for source: MailMessage) -> String {
        let sender = source.envelope.from.first.map { $0.name ?? $0.email } ?? "Someone"
        let when = source.envelope.date.map(attributionDate)
        return "On \(when.map { "\($0), " } ?? "")\(sender) wrote:"
    }

    private nonisolated static func attributionDate(_ date: Date) -> String {
        date.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().year().hour().minute())
    }

    /// `Name <address>`, or the bare address.
    private nonisolated static func headerForm(_ address: Address) -> String {
        address.displayName == address.email ? address.email : "\(address.displayName) <\(address.email)>"
    }

    private nonisolated static func quote(_ text: String) -> String {
        let body = text.replacingOccurrences(of: "\r\n", with: "\n").trimmingCharacters(in: .newlines)
        guard !body.isEmpty else { return "" }
        return body.split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.hasPrefix(">") ? ">" + $0 : "> " + $0 }
            .joined(separator: "\n")
    }

    /// The message `account` sends, from its identity; a reply or forward of `source` is threaded
    /// under it.
    func outgoing(account: String, source: MailMessage?, to: [Address], cc: [Address], bcc: [Address],
                  subject: String, text: String, html: String? = nil, attachments: [URL] = []) async -> OutgoingMessage {
        var message = OutgoingMessage(from: self.account(named: account)?.identity ?? Address(name: nil, email: account),
                                      to: to, cc: cc, bcc: bcc, subject: subject, text: text, html: html,
                                      attachments: attachments, inReplyTo: nil, references: [], thread: nil)
        guard let source else { return message }
        let references = (try? await contents.load(source.ref).content.references) ?? []
        message.thread = source.envelope.thread
        if let id = source.envelope.messageID {
            message.inReplyTo = id
            message.references = references.filter { $0 != id } + [id]
        } else {
            message.references = references
        }
        return message
    }

    /// Sends an inline reply as plain text plus HTML, the HTML quoting the source's own HTML body.
    func sendReply(_ text: String, plan: ReplyPlan, source: MailMessage, bcc: [Address] = [], attachments: [URL] = []) async throws {
        let content = try? await contents.load(source.ref).content
        let html = ReplyHTML.document(reply: text, attribution: plan.attribution, source: content)
        let message = await outgoing(account: source.ref.account, source: source, to: plan.to, cc: plan.cc, bcc: bcc,
                                     subject: plan.subject, text: plan.plainBody(text), html: html, attachments: attachments)
        try await send(message, account: source.ref.account)
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
