import AppKit

/// Google Chat's message markup (`*bold*`, `_italic_`, `~strike~`, `` `code` ``, fenced blocks,
/// `<url|label>`, `<users/…>` mentions), rendered minimally. Everything else is escaped, so a
/// message can never inject markup into the reader.
nonisolated enum ChatMarkup {
    private static let fence = try! NSRegularExpression(pattern: "```\\n?([\\s\\S]*?)\\n?```")
    private static let code = try! NSRegularExpression(pattern: "`([^`\\n]+)`")
    private static let token = try! NSRegularExpression(pattern: #"<(https?://[^|>\s]+)(?:\|([^>\n]*))?>|<(users/[^>\s]+)>"#)
    private static let bold = try! NSRegularExpression(pattern: #"(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])"#)
    private static let italic = try! NSRegularExpression(pattern: #"(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])"#)
    private static let strike = try! NSRegularExpression(pattern: #"(?<![\w~])~(?=\S)([^~\n]*?\S)~(?![\w~])"#)
    private static let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)

    /// `names` maps `users/<id>` to a display name, for mentions.
    static func html(_ text: String, names: [String: String]) -> String {
        split(text, fence, match: { match, ns in
            "<pre>" + HTMLText.escape(ns.substring(with: match.range(at: 1))) + "</pre>"
        }, rest: { part in
            split(part, code, match: { match, ns in
                "<code>" + HTMLText.escape(ns.substring(with: match.range(at: 1))) + "</code>"
            }, rest: { part in
                split(part, token, match: { match, ns in tokenHTML(match, ns, names: names) }, rest: autolink)
            })
        })
    }

    /// One line of text for list previews: links as their labels, emphasis markers dropped.
    static func plain(_ text: String) -> String {
        var out = split(text, token, match: { match, ns in
            if match.range(at: 1).location != NSNotFound {
                let label = match.range(at: 2).location != NSNotFound ? ns.substring(with: match.range(at: 2)) : ""
                return label.isEmpty ? ns.substring(with: match.range(at: 1)) : label
            }
            return "@"
        }, rest: { $0 })
        out = out.replacingOccurrences(of: "```", with: " ")
        for regex in [bold, italic, strike] {
            out = regex.stringByReplacingMatches(in: out, range: NSRange(location: 0, length: (out as NSString).length),
                                                 withTemplate: "$1")
        }
        return out.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// Hands the matches of `regex` to `match` and the text between them to `rest`.
    private static func split(_ text: String, _ regex: NSRegularExpression, match: (NSTextCheckingResult, NSString) -> String,
                              rest: (String) -> String) -> String {
        let ns = text as NSString
        var out = ""
        var cursor = 0
        for found in regex.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            out += rest(ns.substring(with: NSRange(location: cursor, length: found.range.location - cursor)))
            out += match(found, ns)
            cursor = NSMaxRange(found.range)
        }
        out += rest(ns.substring(from: cursor))
        return out
    }

    private static func tokenHTML(_ match: NSTextCheckingResult, _ ns: NSString, names: [String: String]) -> String {
        if match.range(at: 1).location != NSNotFound {
            let url = ns.substring(with: match.range(at: 1))
            let label = match.range(at: 2).location != NSNotFound ? ns.substring(with: match.range(at: 2)) : ""
            return "<a href=\"\(HTMLText.escape(url))\">\(HTMLText.escape(label.isEmpty ? url : label))</a>"
        }
        let user = ns.substring(with: match.range(at: 3))
        return "<span class=\"mention\">@\(HTMLText.escape(names[user] ?? "user"))</span>"
    }

    /// Escapes, links bare URLs and applies emphasis outside them.
    private static func autolink(_ text: String) -> String {
        guard let detector, text.contains(where: { $0 == "." || $0 == ":" }) else { return emphasis(HTMLText.escape(text)) }
        let ns = text as NSString
        var out = ""
        var cursor = 0
        for found in detector.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            guard let url = found.url, ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") else { continue }
            out += emphasis(HTMLText.escape(ns.substring(with: NSRange(location: cursor, length: found.range.location - cursor))))
            out += "<a href=\"\(HTMLText.escape(url.absoluteString))\">\(HTMLText.escape(ns.substring(with: found.range)))</a>"
            cursor = NSMaxRange(found.range)
        }
        out += emphasis(HTMLText.escape(ns.substring(from: cursor)))
        return out
    }

    /// On already escaped text, whose entities hold no `*`, `_` or `~`.
    private static func emphasis(_ escaped: String) -> String {
        guard escaped.contains(where: { $0 == "*" || $0 == "_" || $0 == "~" }) else { return escaped }
        var out = escaped
        for (regex, tag) in [(bold, "b"), (italic, "i"), (strike, "s")] {
            out = regex.stringByReplacingMatches(in: out, range: NSRange(location: 0, length: (out as NSString).length),
                                                 withTemplate: "<\(tag)>$1</\(tag)>")
        }
        return out
    }
}

/// Markup of a chat space in the reader page: a header, a log of messages and a composer. The
/// log is a list of items, one per message, each with a stable DOM id, so new messages are
/// appended and changed ones replaced without re-rendering the page.
nonisolated enum ChatPage {
    /// One message (with the day divider or "New" line above it, if any) as it sits in the log.
    struct Item: Equatable, Sendable {
        let id: String
        let html: String
    }

    /// Who sent a pending message: the user.
    struct Sender: Equatable, Sendable {
        let key: String
        let label: String
    }

    /// Everything the log is built from, copied out of the stores so it builds off the main actor.
    struct Input: Equatable, Sendable {
        let messages: [ChatMessage]
        let pending: [ChatPending]
        /// Incoming messages after this are marked unread; without it the last `unread` incoming ones are.
        let lastRead: Date?
        let unread: Int
        let me: Sender
        /// Avatar key (lowercased address, else `users/<id>`) → picture URL.
        let avatars: [String: String]
        /// `users/<id>` → display name, for mentions and thread references.
        let names: [String: String]

        struct Output: Sendable {
            let items: [Item]
            /// The first unread message, where the space opens.
            let firstUnread: String?
        }

        /// Runs on the concurrent executor, never on the caller's actor.
        @concurrent func build() async -> Output {
            ChatPage.log(self)
        }
    }

    /// A sender's next message within this window, on the same day, joins their previous one.
    private static let groupWindow: TimeInterval = 3600

    private struct Entry {
        let id: String
        let senderID: String
        let senderKey: String
        let label: String
        let initials: String
        let hasEmail: Bool
        let created: Date
        let message: ChatMessage?
        let pending: ChatPending?
        var mine: Bool { message?.mine ?? true }
    }

    private static func log(_ input: Input) -> Input.Output {
        var entries: [Entry] = input.messages.map { message in
            let sender = message.sender
            return Entry(id: domID(message.name), senderID: sender.name, senderKey: sender.avatarKey,
                         label: message.mine ? "You" : sender.label,
                         initials: Avatar.initials(for: Address(name: sender.label, email: sender.email ?? "")),
                         hasEmail: sender.email != nil, created: message.created, message: message, pending: nil)
        }
        let mySenderID = input.messages.last(where: \.mine)?.sender.name ?? "me"
        entries += input.pending.map { pending in
            Entry(id: domID(pending.id), senderID: mySenderID, senderKey: input.me.key, label: "You",
                  initials: Avatar.initials(for: Address(name: input.me.label, email: input.me.key)),
                  hasEmail: input.me.key.contains("@"), created: pending.created, message: nil, pending: pending)
        }
        var unread = Set<String>()
        if let lastRead = input.lastRead {
            for entry in entries where !entry.mine && entry.created > lastRead { unread.insert(entry.id) }
        } else if input.unread > 0 {
            for entry in entries.filter({ !$0.mine }).suffix(input.unread) { unread.insert(entry.id) }
        }
        var starters: [String: ChatMessage] = [:]
        for message in input.messages where !message.threadReply {
            if let thread = message.thread, starters[thread] == nil { starters[thread] = message }
        }
        let calendar = Calendar.current
        var items: [Item] = []
        items.reserveCapacity(entries.count)
        var firstUnread: String?
        var previous: Entry?
        for entry in entries {
            var html = "<div class=\"chat-item\" id=\"\(entry.id)\">"
            let sameDay = previous.map { calendar.isDate($0.created, inSameDayAs: entry.created) } ?? false
            if !sameDay { html += dayDivider(entry.created, calendar: calendar) }
            var opensUnread = false
            if unread.contains(entry.id), firstUnread == nil {
                firstUnread = entry.id
                opensUnread = true
                html += "<div class=\"day new\"><span>New</span></div>"
            }
            let continued = sameDay && !opensUnread && previous?.senderID == entry.senderID
                && entry.created.timeIntervalSince(previous?.created ?? .distantPast) < groupWindow
            html += article(entry, continued: continued, unread: unread.contains(entry.id), input: input, starters: starters)
            html += "</div>"
            items.append(Item(id: entry.id, html: html))
            previous = entry
        }
        return Input.Output(items: items, firstUnread: firstUnread)
    }

    private static func article(_ entry: Entry, continued: Bool, unread: Bool, input: Input,
                                starters: [String: ChatMessage]) -> String {
        let color = Avatar.color(for: entry.senderKey).cssHex
        let time = HTMLText.escape(entry.created.formatted(.dateTime.hour().minute()))
        let full = HTMLText.escape(entry.created.formatted(date: .complete, time: .shortened))
        let key = HTMLText.escape(entry.senderKey)
        let message = entry.message
        let gutter: String
        var head = ""
        if continued {
            gutter = "<span class=\"chat-time\" title=\"\(full)\">\(entry.pending == nil ? time : "")</span>"
        } else {
            if let url = input.avatars[entry.senderKey], let safe = cssURL(url) {
                // Only pictures AvatarStore has (by address) can be enlarged.
                let action = entry.hasEmail && url.hasPrefix("data:") ? " data-action=\"avatar\"" : ""
                gutter = "<div class=\"avatar photo\"\(action) data-avatar=\"\(key)\" style=\"background-image:url('\(safe)')\"></div>"
            } else {
                gutter = "<div class=\"avatar\" data-avatar=\"\(key)\">\(HTMLText.escape(entry.initials))</div>"
            }
            let status = entry.pending.map { $0.failure == nil ? "Sending…" : "Not sent" }
            head = "<div class=\"chat-head\"><span class=\"name\">\(HTMLText.escape(entry.label))</span>"
                + "<span class=\"date\" title=\"\(full)\">\(status.map(HTMLText.escape) ?? time)</span></div>"
        }
        var body = ""
        if let message, message.threadReply, let thread = message.thread {
            var reference = "<span>↳ Reply in thread</span>"
            if let starter = starters[thread] {
                let who = starter.mine ? "you" : starter.sender.label
                reference += "<span>to <b>\(HTMLText.escape(who))</b></span>"
                let quote = ChatMarkup.plain(starter.text)
                if !quote.isEmpty { reference += "<span class=\"q\">\(HTMLText.escape(quote))</span>" }
            }
            body += "<div class=\"chat-thread-ref\">\(reference)</div>"
        }
        let text = message?.text ?? entry.pending?.text ?? ""
        if message?.card == true && text.isEmpty {
            body += "<div class=\"chat-text chat-card\">Card message</div>"
        } else if !text.isEmpty {
            let edited = message?.edited.map {
                " <span class=\"chat-edited\" title=\"\(HTMLText.escape($0.formatted(date: .abbreviated, time: .shortened)))\">(edited)</span>"
            } ?? ""
            body += "<div class=\"chat-text\">\(ChatMarkup.html(text, names: input.names))\(edited)</div>"
        }
        if let attachments = message?.attachments, !attachments.isEmpty {
            body += "<div class=\"attachments\">" + attachments.map(attachment).joined() + "</div>"
        }
        if let pending = entry.pending, let failure = pending.failure {
            let id = HTMLText.escape(pending.id)
            body += "<div class=\"chat-failed\"><span title=\"\(HTMLText.escape(failure))\">Not sent</span>"
                + "<button data-action=\"chat-retry\" data-id=\"\(id)\">Retry</button>"
                + "<button data-action=\"chat-discard\" data-id=\"\(id)\">Delete</button></div>"
        }
        var actions = ""
        if let message, let thread = message.thread {
            let starter = starters[thread] ?? message
            let who = starter.mine ? "your message" : starter.sender.label
            actions = "<div class=\"actions\"><button title=\"Reply in Thread\" data-action=\"chat-reply\" "
                + "data-thread=\"\(HTMLText.escape(thread))\" data-who=\"\(HTMLText.escape(who))\">\(replyIcon)</button></div>"
        }
        var classes = "msg chat"
        if continued { classes += " continued" }
        if unread { classes += " unread" }
        if entry.mine { classes += " mine" }
        if message?.threadReply == true { classes += " threaded" }
        if let pending = entry.pending { classes += pending.failure == nil ? " pending" : " failed" }
        return """
        <article class="\(classes)" style="--avatar:\(color)">\
        <div class="chat-gutter">\(gutter)</div>\
        <div class="chat-main">\(head)\(body)</div>\(actions)\
        </article>
        """
    }

    private static func attachment(_ attachment: ChatAttachment) -> String {
        let ext = (attachment.name as NSString).pathExtension
        let label = ext.isEmpty ? (attachment.contentType?.split(separator: "/").last.map(String.init) ?? "file") : ext
        let inner = "<div class=\"thumb\">\(HTMLText.escape(String(label.prefix(4))))</div><div class=\"meta-a\">"
            + "<div class=\"fname\">\(HTMLText.escape(attachment.name))</div>"
            + "<div class=\"fsize\">\(HTMLText.escape(attachment.contentType ?? ""))</div></div>"
        guard let url = attachment.url, let scheme = URL(string: url)?.scheme?.lowercased(), ["http", "https"].contains(scheme) else {
            return "<div class=\"att disabled\" title=\"\(HTMLText.escape(attachment.name))\">\(inner)</div>"
        }
        return "<div class=\"att\" data-action=\"chat-open\" data-url=\"\(HTMLText.escape(url))\" "
            + "title=\"\(HTMLText.escape(attachment.name))\">\(inner)</div>"
    }

    private static func dayDivider(_ date: Date, calendar: Calendar) -> String {
        let label: String
        if calendar.isDateInToday(date) {
            label = "Today"
        } else if calendar.isDateInYesterday(date) {
            label = "Yesterday"
        } else if calendar.isDate(date, equalTo: Date(), toGranularity: .year) {
            label = date.formatted(.dateTime.weekday(.wide).month(.wide).day())
        } else {
            label = date.formatted(.dateTime.month(.wide).day().year())
        }
        return "<div class=\"day\"><span>\(HTMLText.escape(label))</span></div>"
    }

    /// A DOM id for a resource name (`spaces/x/messages/y`) or pending id.
    static func domID(_ name: String) -> String {
        "cm-" + String(name.map { $0.isASCII && ($0.isLetter || $0.isNumber) ? $0 : "-" })
    }

    /// `url` if it is safe inside `url('…')` in a style attribute: a data URL or a plain https URL.
    private static func cssURL(_ url: String) -> String? {
        if url.hasPrefix("data:image/") { return url.contains(where: { "'\"()\\ ".contains($0) }) ? nil : url }
        guard url.hasPrefix("https://"), !url.contains(where: { "'\"()\\<> \n\r\t".contains($0) }) else { return nil }
        return url
    }

    private static let replyIcon = "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path "
        + "d=\"M6.5 3.5 2.5 7.5l4 4M3 7.5h6.5a4 4 0 0 1 4 4v1\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" "
        + "stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>"

    // MARK: - Page chrome

    /// Header, an empty log and the composer; the log fills in through `MS.render`/`MS.chatUpdate`.
    static func page(title: String, kind: ChatSpaceKind, memberCount: Int, accountLabel: String, accountColor: String) -> String {
        var meta = ["<span class=\"meta-item\">\(HTMLText.escape(kind.title))</span>",
                    "<span class=\"meta-item\" style=\"--tint:\(HTMLText.escape(accountColor))\"><i class=\"mark bar\"></i>"
                        + "\(HTMLText.escape(accountLabel))</span>"]
        if kind == .group, memberCount > 0 { meta.append("<span class=\"meta-item\">\(memberCount + 1) people</span>") }
        return """
        <header class="thread-header">
          <h1>\(HTMLText.escape(title))</h1>
          <div class="thread-meta">\(meta.joined(separator: "<i class=\"sep\">·</i>"))</div>
        </header>
        <main id="thread" class="chat chat-space">
          <div id="chat-earlier" class="chat-earlier" hidden><button data-action="chat-earlier">Load earlier messages</button></div>
          <div id="chat-log" class="chat-log"></div>
          <div id="chat-status" class="chat-status"></div>
        </main>
        <footer class="reply-dock composing chat-dock" id="dock">
          <section class="composer" id="composer">
            <div class="c-head" id="c-thread" hidden>
              <span class="c-mode">\(replyIcon)</span>
              <div class="c-to" id="c-thread-label"></div>
              <button class="c-btn" title="Cancel Thread Reply" data-action="chat-unthread">✕</button>
            </div>
            <div class="c-scroll">
              <textarea id="c-text" rows="1" placeholder="Message \(HTMLText.escape(title))" spellcheck="true"></textarea>
            </div>
            <div class="c-foot">
              <span class="c-status" id="c-status">Return to send · Shift-Return for a new line</span>
              <button class="c-send" data-action="chat-send"><span>Send</span><span class="kbd">↩</span></button>
            </div>
          </section>
        </footer>
        """
    }

    /// The log's placeholder: a skeleton while the first page loads, a failure, or an empty space.
    static func status(loading: Bool, failure: String?, empty: Bool) -> String {
        guard empty else { return "" }
        if let failure { return "<div class=\"error\">Could not load messages: \(HTMLText.escape(failure))</div>" }
        if loading { return "<div class=\"skeleton\"><i></i><i></i><i class=\"short\"></i></div>" }
        return "No messages yet."
    }

    static let css = """
    main.chat-space { border-top: 0; }
    .chat-log { display: flex; flex-direction: column; }
    .chat-item { display: flex; flex-direction: column; }
    .msg.static { animation: none; }
    .chat-text { white-space: pre-wrap; overflow-wrap: anywhere; font-size: 14px; line-height: 1.55; }
    .chat-text a { color: var(--accent); }
    .chat-text code { font: 12.5px ui-monospace, SFMono-Regular, Menlo, monospace; padding: 1px 4px; border-radius: 4px;
      background: var(--chip); }
    .chat-text pre { font: 12.5px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; margin: 4px 0; padding: 8px 10px;
      border-radius: 8px; background: var(--chip); white-space: pre-wrap; }
    .chat-text .mention { color: var(--accent); font-weight: 550; }
    .chat-card { color: var(--muted); font-style: italic; }
    .chat-edited { color: var(--faint); font-size: 11px; white-space: nowrap; }
    .chat-thread-ref { display: flex; gap: 5px; align-items: baseline; min-width: 0; font-size: 12px; color: var(--muted);
      margin: 1px 0 3px; padding-left: 8px; border-left: 2px solid color-mix(in srgb, var(--accent) 45%, var(--line));
      white-space: nowrap; }
    .chat-thread-ref b { font-weight: 600; color: var(--text); }
    .chat-thread-ref .q { overflow: hidden; text-overflow: ellipsis; min-width: 0; color: var(--faint); }
    .msg.chat.threaded .chat-text { padding-left: 10px; }
    .msg.chat.pending .chat-main { opacity: .55; }
    .msg.chat.failed .chat-text { color: var(--muted); }
    .chat-failed { display: flex; align-items: center; gap: 4px; margin-top: 2px; font-size: 12px; color: #d70015; }
    .chat-failed button { color: var(--accent); padding: 1px 6px; }
    .day.new { color: var(--accent); }
    .day.new::before, .day.new::after { background: color-mix(in srgb, var(--accent) 45%, transparent); }
    .chat-earlier { display: flex; justify-content: center; margin: 0 0 6px; }
    .chat-earlier[hidden] { display: none; }
    .chat-earlier button { color: var(--accent); }
    .chat-status { color: var(--muted); font-size: 13px; padding: 10px 0; }
    .chat-status:empty { display: none; }
    .att.disabled { opacity: .55; }
    .chat-dock { padding-top: 18px; }
    .chat-dock #c-text { min-height: 40px; padding-top: 10px; }
    .chat-dock .c-head[hidden] { display: none; }
    .chat-dock .c-status { color: var(--faint); }
    """
}
