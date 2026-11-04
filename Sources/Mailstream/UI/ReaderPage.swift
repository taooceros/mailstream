import AppKit

/// HTML scaffolding for a conversation in the reader's shell page (`WebShell`): `MS.render` fills
/// the column per conversation, and message bodies arrive later through `MS.setBody`. HTML mail
/// renders inside a same-origin `srcdoc` iframe that inherits the shell's Content-Security-Policy;
/// a body whose remote content stays blocked carries a stricter policy of its own
/// (`blockingRemoteContent`). It also holds the markup and styles chat spaces (`ChatPage`) share.
@MainActor
enum ReaderPage {
    struct Card {
        let index: Int
        let message: MailMessage
        let expanded: Bool
        let snippet: String?
        /// First `From` address, with an own address given its account display name.
        let sender: Address
        /// The sender's picture as a `data:` URL, when already known.
        let avatar: String?
    }

    /// `html` with a CSP of its own that keeps its frame from loading anything remote; a `srcdoc`
    /// document enforces both its own and its parent's policy. The meta goes after any leading
    /// whitespace, comments and doctype, which must come first for the page to keep standards mode.
    static func blockingRemoteContent(_ html: String) -> String {
        var rest = Substring(html)
        while true {
            rest = rest.drop(while: { $0.isWhitespace || $0 == "\u{FEFF}" })
            if rest.hasPrefix("<!--"), let end = rest.range(of: "-->") {
                rest = rest[end.upperBound...]
            } else if rest.prefix(9).lowercased() == "<!doctype", let end = rest.firstIndex(of: ">") {
                rest = rest[rest.index(after: end)...]
            } else {
                break
            }
        }
        return String(html[..<rest.startIndex]) + strictPolicy + rest
    }

    private static let strictPolicy = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; "
        + "img-src data: mscid:; style-src 'unsafe-inline'; font-src data:; media-src data: mscid:\">"

    /// The column's content for one conversation: header, remote banner, thread and reply dock.
    /// `chat` lays the thread out as a conversation: every message open, grouped by sender and day.
    /// `trustLabel` names the senders the banner offers to always load remote content from; nil
    /// offers only a one-time load (junk).
    static func thread(conversation: Conversation, cards: [Card], own: Set<String>, accountLabel: String,
                       accountColor: String, mailboxLabels: [String], tag: ConversationTag?, chat: Bool,
                       trustLabel: String?) -> String {
        var meta: [String] = []
        // Every thread that can wait carries the chip, hidden once resolved, so the reader can flip it in place.
        var waiting = ""
        if let tag, tag.category != .newsletter {
            let chip = "<span class=\"meta-item\" style=\"--tint:\(Design.Color.waiting.css)\"><i class=\"mark dot\"></i>Waiting on you"
                + "<button class=\"meta-resolve\" data-action=\"resolve\">Resolve</button></span>"
            waiting = "<span id=\"waiting\" class=\"meta-wait\"\(tag.toRespond ? "" : " hidden")>\(chip)<i class=\"sep\">·</i></span>"
        }
        if let tag, tag.category != .people { meta.append(Self.meta(tag.category.title, tint: nil)) }
        for label in tag?.labels ?? [] { meta.append(Self.meta(label, tint: TagPalette.hex(for: label), mark: "dot")) }
        meta.append(Self.meta(accountLabel, tint: accountColor, mark: "bar"))
        for label in mailboxLabels { meta.append(Self.meta(label, tint: nil)) }
        if conversation.messages.count > 1 { meta.append(Self.meta("\(conversation.messages.count) messages", tint: nil)) }

        let replyTarget = conversation.latestIncoming(own: own)?.envelope.from.first?.shortName
        let replyLabel = replyTarget.map { "Reply to \(HTMLText.escape($0))…" } ?? "Reply…"
        var html = threadHeader(title: Subject(conversation.subject).displayTitle, meta: meta, leading: waiting) + """
        <div id="remote-banner" class="banner" hidden>
          <span>\(trustLabel == nil ? "Remote images are blocked in junk mail." : "Remote images are blocked to protect your privacy.")</span>
          <button data-action="load-remote">Load Images</button>
          \(trustLabel.map { "<button data-action=\"trust-remote\">Always Load from \(HTMLText.escape($0))</button>" } ?? "")
        </div>
        <main id="thread"\(chat ? " class=\"chat\"" : "")>
        """
        if chat {
            html += chatMessages(cards, own: own)
        } else {
            for (position, card) in cards.enumerated() { html += article(card, own: own, position: position, total: cards.count) }
        }
        let replyComposer = composer(
            head: """
            <span class="c-mode">\(Icon.reply)\(Icon.replyAll)</span>
            <div class="c-to" id="c-to"></div>
            <button class="c-btn" id="c-all" title="Reply All" data-action="reply-toggle-all" hidden>\(Icon.replyAll)</button>
            <button class="c-btn" title="Open in Window" data-action="reply-popout">\(Icon.popout)</button>
            <button class="c-btn" title="Discard" data-action="reply-discard">\(Icon.trash)</button>
            """,
            placeholder: "Write a reply…",
            extra: """
            <div class="c-quote" id="c-quote-wrap" hidden>
              <button class="ms-fold-toggle" data-action="reply-quote" title="Show quoted text">•••</button>
              <div class="c-quote-fold"><div><div class="c-quote-body" id="c-quote"></div></div></div>
            </div>
            """,
            send: "reply-send", sendIcon: Icon.send, sendKey: "⌘↩")
        html += """
        </main>
        <footer class="reply-dock" id="dock">
          <div class="fold-y bar-fold"><div class="fold-clip"><div class="dock-bar">
            <button class="reply-pill" data-action="reply-latest">\(Icon.reply)<span id="pill-label">\(replyLabel)</span></button>
            <button class="dock-icon" title="Reply All" data-action="replyAll-latest">\(Icon.replyAll)</button>
            <button class="dock-icon" title="Forward" data-action="forward-latest">\(Icon.forward)</button>
          </div></div></div>
          <div class="fold-y composer-fold"><div class="fold-clip">
            \(replyComposer)
          </div></div>
        </footer>
        """
        return html
    }

    /// The page's title over its metadata items, separated by dots; `leading` goes first, unseparated.
    nonisolated static func threadHeader(title: String, meta: [String], leading: String = "") -> String {
        """
        <header class="thread-header">
          <h1>\(HTMLText.escape(title))</h1>
          <div class="thread-meta">\(leading)\(meta.joined(separator: "<i class=\"sep\">·</i>"))</div>
        </header>

        """
    }

    /// The composer at the foot of a page, a mail reply's or a chat space's: `head` fills its
    /// header row (`headAttributes` names or hides it), `extra` follows the text field, and the
    /// send button posts `send`, with `sendKey` as its shortcut hint.
    nonisolated static func composer(head: String, headAttributes: String = "", placeholder: String, extra: String = "",
                                     status: String = "", send: String, sendIcon: String = "", sendKey: String) -> String {
        """
        <section class="composer" id="composer">
          <div class="c-head"\(headAttributes)>
        \(head)
          </div>
          <div class="c-scroll">
            <textarea id="c-text" rows="1" placeholder="\(HTMLText.escape(placeholder))" spellcheck="true"></textarea>
        \(extra)
          </div>
          <div class="c-foot">
            <span class="c-status" id="c-status">\(status)</span>
            <button class="c-send" data-action="\(send)">\(sendIcon)<span>Send</span><span class="kbd">\(sendKey)</span></button>
          </div>
        </section>
        """
    }

    /// Inline SVG glyphs, so icons match SF Symbols' weight instead of falling back to text arrows.
    nonisolated enum Icon {
        private static func svg(_ path: String) -> String {
            "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"\(path)\" fill=\"none\" "
                + "stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>"
        }
        static let reply = svg("M6.5 3.5 2.5 7.5l4 4M3 7.5h6.5a4 4 0 0 1 4 4v1")
        static let replyAll = svg("M8 3.5 4 7.5l4 4M4.5 3.5l-4 4 4 4M4.5 7.5h5a4 4 0 0 1 4 4v1")
        static let forward = svg("M9.5 3.5l4 4-4 4M13 7.5H6.5a4 4 0 0 0-4 4v1")
        static let popout = svg("M9.5 2.5h4v4M13.5 2.5 8 8M6.5 3.5h-3a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-3")
        static let trash = svg("M2.5 4.5h11M6.5 4.5V3a.5.5 0 0 1 .5-.5h2a.5.5 0 0 1 .5.5v1.5M4 4.5l.6 8.1a1 1 0 0 0 1 .9h4.8a1 1 0 0 0 1-.9l.6-8.1")
        static let send = svg("M14 2 7.2 8.8M14 2l-4.4 12-2.4-5.2L2 6.4z")
        static let star = "<svg class=\"icon star\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M8 1.8l1.9 3.9 4.3.6-3.1 3 .7 4.3L8 11.6l-3.8 2 .7-4.3-3.1-3 4.3-.6z\" fill=\"currentColor\"/></svg>"
    }

    /// Header metadata item: quiet text with an optional colored dot or account bar.
    nonisolated static func meta(_ text: String, tint: String?, mark: String? = nil) -> String {
        let style = tint.map { " style=\"--tint:\(HTMLText.escape($0))\"" } ?? ""
        let markup = mark.map { "<i class=\"mark \($0)\"></i>" } ?? ""
        return "<span class=\"meta-item\"\(style)>\(markup)\(HTMLText.escape(text))</span>"
    }

    /// Messages as a chat log: a divider per day, and runs from one sender under one name.
    private static func chatMessages(_ cards: [Card], own: Set<String>) -> String {
        let runs = chatRuns(cards, date: { $0.message.envelope.date }, sender: { $0.sender.normalizedEmail })
        var html = ""
        for (position, (card, run)) in zip(cards, runs).enumerated() {
            html += run.divider + chatArticle(card, own: own, continued: run.continued, position: position, total: cards.count)
        }
        return html
    }

    private static func chatArticle(_ card: Card, own: Set<String>, continued: Bool, position: Int, total: Int) -> String {
        let envelope = card.message.envelope
        let sender = card.sender
        let i = card.index
        let head = continued ? nil : ChatHead(
            avatar: avatar(card), name: displayName(sender, own: own), title: sender.email,
            extra: (envelope.isFlagged ? Icon.star : "")
                + "<span class=\"recipients\" id=\"rcpt-\(i)\">\(recipients(envelope.to, cc: [], own: own))</span>")
        return chatMessage(id: "msg-\(i)", classes: ["expanded"] + (envelope.isSeen ? [] : ["unread"]),
                           style: style(card, position: position, total: total), head: head,
                           time: envelope.date?.formatted(.dateTime.hour().minute()) ?? "",
                           full: envelope.date?.formatted(date: .complete, time: .shortened) ?? "",
                           main: bodySlot(i), actions: actions(i))
    }

    private static func article(_ card: Card, own: Set<String>, position: Int, total: Int) -> String {
        let envelope = card.message.envelope
        let email = HTMLText.escape(card.sender.email)
        let date = envelope.date.map { date -> String in
            let style: Date.FormatStyle = Calendar.current.isDate(date, equalTo: Date(), toGranularity: .year)
                ? .dateTime.month(.abbreviated).day().hour().minute()
                : .dateTime.month(.abbreviated).day().year().hour().minute()
            return HTMLText.escape(date.formatted(style))
        } ?? ""
        let state = card.expanded ? "expanded" : "collapsed"
        let unread = envelope.isSeen ? "" : " unread"
        let i = card.index
        return """
        <article id="msg-\(i)" class="msg \(state)\(unread)" style="\(style(card, position: position, total: total))">
          <div class="msg-head" data-action="toggle" data-index="\(i)">
            \(avatar(card))
            <div class="who">
              <div class="line1">
                <span class="name">\(HTMLText.escape(displayName(card.sender, own: own)))</span>
                <span class="email" data-action="compose-to" data-email="\(email)">\(email)</span>
                <span class="snippet">\(HTMLText.escape(card.snippet ?? ""))</span>
              </div>
              <div class="recipients" id="rcpt-\(i)">\(recipients(envelope.to, cc: [], own: own))</div>
            </div>
            <div class="meta">
              \(actions(i))
              \(envelope.isFlagged ? Icon.star : "")
              <span class="date">\(date)</span>
            </div>
          </div>
          <div class="fold"><div class="fold-inner"><div class="fold-content">\(bodySlot(i))</div></div></div>
        </article>
        """
    }

    private static func displayName(_ sender: Address, own: Set<String>) -> String {
        own.contains(sender.normalizedEmail) ? "\(sender.displayName) (me)" : sender.displayName
    }

    private static func avatar(_ card: Card) -> String {
        avatar(key: card.sender.normalizedEmail, photo: card.avatar, initials: Avatar.initials(for: card.sender),
               enlarges: true)
    }

    /// The sender's color; later messages arrive a beat after earlier ones, and only the last few animate at all.
    private static func style(_ card: Card, position: Int, total: Int) -> String {
        "--avatar:\(Avatar.color(for: card.sender.email).cssRGBA); animation-delay:\(max(0, position - max(0, total - 6)) * 30)ms"
    }

    /// Where `MS.setBody` puts message `index`'s body and attachments.
    private static func bodySlot(_ index: Int) -> String {
        "<div class=\"msg-body\" id=\"body-\(index)\"><div class=\"skeleton\"><i></i><i></i><i class=\"short\"></i></div></div>"
            + "<div class=\"attachments\" id=\"att-\(index)\"></div>"
    }

    private static func actions(_ index: Int) -> String {
        "<div class=\"actions\">"
            + "<button title=\"Reply\" data-action=\"reply\" data-index=\"\(index)\">\(Icon.reply)</button>"
            + "<button title=\"Reply All\" data-action=\"replyAll\" data-index=\"\(index)\">\(Icon.replyAll)</button>"
            + "<button title=\"Forward\" data-action=\"forward\" data-index=\"\(index)\">\(Icon.forward)</button></div>"
    }

    // MARK: - Attachments

    /// An attachment tile: a thumbnail, or else `label` (a short type name, uppercased by CSS), over
    /// the name and `detail`. `attributes` (its action) and `trailing` (its controls) are the page's own.
    nonisolated static func attachmentTile(name: String, detail: String, label: String, thumb: String? = nil,
                                           disabled: Bool = false, attributes: String = "", trailing: String = "") -> String {
        let name = HTMLText.escape(name)
        let face = thumb.map { "<img src=\"\(HTMLText.escape($0))\">" } ?? HTMLText.escape(label)
        return "<div class=\"att\(disabled ? " disabled" : "")\"\(attributes) title=\"\(name)\"><div class=\"thumb\">\(face)</div>"
            + "<div class=\"meta-a\"><div class=\"fname\">\(name)</div><div class=\"fsize\">\(HTMLText.escape(detail))</div></div>"
            + "\(trailing)</div>"
    }

    /// A tile's type label: up to four characters of `pathExtension`, else of `fallback`.
    nonisolated static func attachmentLabel(pathExtension: String, fallback: String) -> String {
        String((pathExtension.isEmpty ? fallback : pathExtension).prefix(4))
    }

    // MARK: - Chat logs

    /// A sender's next message within this window, on the same day, joins their previous one.
    nonisolated private static let groupWindow: TimeInterval = 3600

    /// A chat log's breaks: per item, the day divider above it (empty on the previous item's day)
    /// and whether it continues the previous item's run, from the same sender on the same day within
    /// `groupWindow`. An item without a date opens no day and breaks the run; `opensRun` items
    /// never continue one.
    nonisolated static func chatRuns<Item>(_ items: [Item], date: (Item) -> Date?, sender: (Item) -> String,
                                          opensRun: (Item) -> Bool = { _ in false }) -> [(divider: String, continued: Bool)] {
        let calendar = Calendar.current
        var runs: [(divider: String, continued: Bool)] = []
        runs.reserveCapacity(items.count)
        var previous: Item?
        for item in items {
            let day = date(item), last = previous.flatMap(date)
            let sameDay = day.flatMap { day in last.map { calendar.isDate($0, inSameDayAs: day) } } ?? false
            let continued = sameDay && !opensRun(item) && previous.map(sender) == sender(item)
                && day.flatMap { day in last.map { day.timeIntervalSince($0) < groupWindow } } == true
            let divider = sameDay ? "" : day.map { dayDivider($0, calendar: calendar) } ?? ""
            runs.append((divider, continued))
            previous = item
        }
        return runs
    }

    /// A divider naming `date`'s day.
    nonisolated private static func dayDivider(_ date: Date, calendar: Calendar) -> String {
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

    /// `key`'s picture, or their initials while none is known; `enlarges` lets a click enlarge it.
    nonisolated static func avatar(key: String, photo: String?, initials: String, enlarges: Bool) -> String {
        let key = HTMLText.escape(key)
        guard let photo = photo.flatMap(cssURL) else {
            return "<div class=\"avatar\" data-avatar=\"\(key)\">\(HTMLText.escape(initials))</div>"
        }
        let action = enlarges ? " data-action=\"avatar\"" : ""
        return "<div class=\"avatar photo\"\(action) data-avatar=\"\(key)\" style=\"background-image:url('\(photo)')\"></div>"
    }

    /// `url` if it is safe inside `url('…')` in a style attribute: a data URL or a plain https URL.
    nonisolated private static func cssURL(_ url: String) -> String? {
        if url.hasPrefix("data:image/") { return url.contains(where: { "'\"()\\ ".contains($0) }) ? nil : url }
        guard url.hasPrefix("https://"), !url.contains(where: { "'\"()\\<> \n\r\t".contains($0) }) else { return nil }
        return url
    }

    /// Who starts a run of messages in a chat log: avatar markup, name (`title` its tooltip) and
    /// markup following the time.
    nonisolated struct ChatHead {
        let avatar: String
        let name: String
        var title: String?
        var extra = ""
    }

    /// One message in a chat log, a mail thread's or a chat space's. Without a `head` it continues
    /// its sender's run: no avatar or name, and its `time` shows in the gutter on hover.
    nonisolated static func chatMessage(id: String? = nil, classes: [String], style: String, head: ChatHead?,
                                        time: String, full: String, main: String, actions: String) -> String {
        let time = HTMLText.escape(time), full = HTMLText.escape(full)
        let gutter: String, top: String
        if let head {
            let title = head.title.map { " title=\"\(HTMLText.escape($0))\"" } ?? ""
            gutter = head.avatar
            top = "<div class=\"chat-head\"><span class=\"name\"\(title)>\(HTMLText.escape(head.name))</span>"
                + "<span class=\"date\" title=\"\(full)\">\(time)</span>\(head.extra)</div>"
        } else {
            gutter = "<span class=\"chat-time\" title=\"\(full)\">\(time)</span>"
            top = ""
        }
        let classes = (["msg", "chat"] + classes + (head == nil ? ["continued"] : [])).joined(separator: " ")
        return "<article\(id.map { " id=\"\($0)\"" } ?? "") class=\"\(classes)\" style=\"\(style)\">"
            + "<div class=\"chat-gutter\">\(gutter)</div><div class=\"chat-main\">\(top)\(main)</div>\(actions)</article>"
    }

    /// How a recipient reads in a thread: "me" for the user's own addresses.
    static func name(_ address: Address, own: Set<String>) -> String {
        own.contains(address.normalizedEmail) ? "me" : address.displayName
    }

    /// "to Josh, me +2", with the full list in the tooltip.
    static func recipients(_ to: [Address], cc: [Address], own: Set<String>) -> String {
        let all = (to + cc).map { name($0, own: own) }
        guard !all.isEmpty else { return "" }
        let shown = all.prefix(3).joined(separator: ", ")
        let more = all.count > 3 ? " +\(all.count - 3)" : ""
        let full = (to.isEmpty ? "" : "to " + to.map(\.rfc5322).joined(separator: ", "))
            + (cc.isEmpty ? "" : "\ncc " + cc.map(\.rfc5322).joined(separator: ", "))
        return "<span title=\"\(HTMLText.escape(full))\">to \(HTMLText.escape(shown))\(more)</span>"
    }

    // MARK: - Styles

    /// The shell page's rules, after `Design.stylesheet`'s tokens (`WebShell.page`), by component.
    /// Order matters where selectors tie: `.banner button` beats `button:hover`, `.c-status` keeps
    /// its size over `.error`, and the chat dock overrides the reply dock.
    static let css = [Style.base, Style.header, Style.messages, Style.mailContent, Style.attachments, Style.chatLog,
                      Style.replyDock, Style.composer, Style.reducedMotion].joined(separator: "\n")

    /// Every value comes from a `Design` token, except geometry (pill and circle radii, offsets),
    /// keyframe loops and stagger delays, the white card under HTML mail in dark mode, and the
    /// black the Send button mixes in to darken on hover.
    private enum Style {
        static let base = """
        * { box-sizing: border-box; }
        html, body { margin: 0; background: var(--bg); color: var(--text); font: var(--size-reading)/1.5 var(--font-ui);
          -webkit-font-smoothing: antialiased; }
        .column { max-width: 720px; margin: 0 auto; padding: 30px 28px 0; min-height: 100vh;
          display: flex; flex-direction: column; }
        /* The native view handles the fade; the page only settles, so text is legible from the first frame. */
        @keyframes rise { from { opacity: .35; transform: translateY(10px); } to { opacity: 1; transform: none; } }
        @keyframes settle { from { transform: translateY(6px); } to { transform: none; } }
        @keyframes pulse { 50% { opacity: .4; } }
        button { font: inherit; font-size: var(--size-footnote); font-weight: 550; color: var(--text); background: transparent;
          border: 0; border-radius: var(--radius-control); padding: 4px 10px; cursor: default;
          transition: background-color var(--dur-quick) var(--ease), color var(--dur-quick) var(--ease),
            transform var(--dur-quick) var(--ease); }
        button:hover { background: var(--hover); }
        button:active { transform: scale(.96); }
        .icon { width: 15px; height: 15px; display: block; flex: none; }
        .icon.star { color: var(--star); width: 12px; height: 12px; }
        .avatar { flex: none; width: 34px; height: 34px; border-radius: 50%; font-weight: 650; font-size: var(--size-headline);
          display: flex; align-items: center; justify-content: center; color: var(--avatar);
          background: color-mix(in srgb, var(--avatar) 15%, transparent); transition: transform var(--dur-brief) var(--ease); }
        .avatar.photo { background-size: cover; background-position: center; cursor: zoom-in; }
        .error { color: var(--danger); font-size: var(--size-body); padding: 8px 0; }
        .skeleton i { display: block; height: 9px; border-radius: 5px; margin: 10px 0; background: var(--chip);
          animation: pulse 1.4s ease-in-out infinite; }
        .skeleton i:nth-child(2) { width: 88%; animation-delay: .15s; }
        .skeleton i.short { width: 55%; animation-delay: .3s; }
        """

        /// The title, its metadata line and the remote content banner.
        static let header = """
        .thread-header { margin: 0 0 18px; animation: settle var(--dur-long) var(--ease) both; }
        .thread-header h1 { font-family: var(--font-serif); font-size: var(--size-display); line-height: 1.22;
          font-weight: 600; letter-spacing: -.012em; margin: 0 0 9px; overflow-wrap: anywhere; }
        .thread-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; font-size: var(--size-footnote);
          color: var(--muted); }
        .thread-meta .sep { font-style: normal; color: var(--faint); }
        .meta-item { display: inline-flex; align-items: center; gap: 6px; white-space: nowrap; }
        .mark { display: inline-block; background: var(--tint); }
        .mark.dot { width: 6px; height: 6px; border-radius: 50%; }
        .mark.bar { width: 2.5px; height: 11px; border-radius: var(--radius-small); }
        .meta-wait:not([hidden]) { display: contents; }
        .meta-resolve { padding: 1px 8px; margin-left: 2px; font-size: var(--size-caption); font-weight: 550; color: var(--accent);
          background: color-mix(in srgb, var(--accent) 11%, transparent); }
        .meta-resolve:hover { background: color-mix(in srgb, var(--accent) 19%, transparent); }
        .banner { display: flex; align-items: center; gap: 12px; margin: 0 0 14px; padding: 7px 8px 7px 14px;
          border-radius: var(--radius-card); font-size: var(--size-footnote); color: var(--muted); background: var(--chip);
          animation: rise var(--dur-standard) var(--ease) both; }
        .banner[hidden] { display: none; }
        .banner span { flex: 1; }
        .banner button { color: var(--accent); background: var(--bg); }
        """

        /// A mail thread's message cards, their folds and bodies.
        static let messages = """
        main { display: flex; flex-direction: column; border-top: 1px solid var(--line); }
        .msg { border-bottom: 1px solid var(--line); animation: rise var(--dur-standard) var(--ease) both; }
        .msg-head { display: flex; gap: 12px; align-items: center; cursor: default; padding: 10px 6px;
          margin: 0 -6px; border-radius: var(--radius-control); user-select: none; -webkit-user-select: none;
          transition: background-color var(--dur-quick) var(--ease); }
        .msg.collapsed .msg-head:hover { background: var(--hover); }
        .msg.expanded .msg-head { padding-top: 18px; align-items: flex-start; }
        .msg.collapsed .avatar { transform: scale(.86); }
        .who { flex: 1; min-width: 0; }
        .line1 { display: flex; gap: 8px; align-items: baseline; white-space: nowrap; min-width: 0; }
        .line1 > span { overflow: hidden; text-overflow: ellipsis; }
        .name { font-weight: 600; flex: none; max-width: 50%; }
        .msg.unread .name::after { content: ""; display: inline-block; width: 6px; height: 6px; border-radius: 50%;
          background: var(--accent); margin: 0 0 2px 7px; vertical-align: middle; }
        .email { color: var(--faint); font-size: var(--size-footnote); flex: 0 1 auto; }
        .msg.expanded .email:hover { color: var(--accent); }
        .recipients { color: var(--muted); font-size: var(--size-footnote); margin-top: 1px; overflow: hidden;
          text-overflow: ellipsis; white-space: nowrap; }
        .snippet { color: var(--muted); font-size: var(--size-body); flex: 1 1 auto; min-width: 0; }
        .msg.expanded .snippet, .msg.collapsed .recipients, .msg.collapsed .email { display: none; }
        .meta { flex: none; display: flex; align-items: center; gap: 6px; }
        .date { color: var(--faint); font-size: var(--size-footnote); white-space: nowrap; font-variant-numeric: tabular-nums; }
        .actions { display: flex; gap: 0; opacity: 0; transform: translateX(4px);
          transition: opacity var(--dur-quick) var(--ease), transform var(--dur-quick) var(--ease); }
        .msg.expanded .msg-head:hover .actions, .msg.expanded .actions:focus-within { opacity: 1; transform: none; }
        .msg.collapsed .actions { display: none; }
        .actions button { padding: 4px 6px; color: var(--muted); }
        .actions button:hover { color: var(--text); }
        /* Folds animate only their row track; the fade runs on the fixed-size content inside, so the
           compositor blends it instead of repainting a layer that resizes every frame. */
        .fold { display: grid; grid-template-rows: 1fr; transition: grid-template-rows var(--dur-standard) var(--ease); }
        .msg.collapsed .fold { grid-template-rows: 0fr; }
        .fold-inner { min-height: 0; overflow: hidden; }
        .fold-content { transition: opacity var(--dur-brief) var(--ease); }
        .msg.collapsed .fold-content { opacity: 0; }
        .msg-body { padding: 4px 0 22px 40px; overflow-wrap: anywhere; }
        .mail-frame { display: block; width: 100%; border: 0; height: 60px; background: transparent; }
        /* HTML mail is written for a light page, so in dark mode it sits on a white card. */
        @media (prefers-color-scheme: dark) {
          .mail-frame { background: #fff; border-radius: var(--radius-card); padding: 14px 16px; box-sizing: content-box;
            width: calc(100% - 32px); }
        }
        .plain { white-space: pre-wrap; font-size: var(--size-reading); line-height: 1.6; }
        .plain a, .ms-plain a { color: var(--accent); }
        """

        /// Markup the app puts in message bodies, styled alike in the page and in HTML mail frames.
        static let mailContent = """
        .ms-plain { white-space: pre-wrap; }
        blockquote.ms-quote { margin: 6px 0; padding: 0 0 0 12px; border-left: 2px solid var(--line); color: var(--muted); }
        .ms-fold-toggle { display: inline-flex; align-items: center; margin: 8px 0; padding: 0 9px; height: 18px;
          border-radius: 9px; font-family: var(--font-ui); font-size: var(--size-footnote); letter-spacing: 1px;
          color: var(--muted); background: var(--chip); cursor: default; transition: background-color var(--dur-quick) var(--ease); }
        .ms-fold-toggle:hover { background: var(--line); }
        .ms-fold-toggle.block { display: flex; width: fit-content; }
        .ms-folded { display: none !important; }
        .ms-attached-message { margin-top: 14px; padding: 10px 12px; border: 1px solid var(--line); border-radius: var(--radius-card); }
        .ms-attached-headline { font-size: var(--size-footnote); color: var(--muted); margin-bottom: 8px; }
        """

        static let attachments = """
        .attachments { display: flex; flex-wrap: wrap; gap: 8px; padding: 0 0 22px 40px; margin-top: -8px; }
        .attachments:empty { display: none; }
        .att { display: flex; align-items: center; gap: 10px; padding: 6px 8px 6px 6px; border-radius: var(--radius-card);
          border: 1px solid var(--line); max-width: 280px; cursor: default; transition: background-color var(--dur-quick) var(--ease); }
        .att:hover { background: var(--hover); }
        .att.disabled { opacity: .55; }
        .att .thumb { width: 34px; height: 34px; border-radius: var(--radius-control); flex: none; background: var(--chip);
          display: flex; align-items: center; justify-content: center; font-size: var(--size-micro); font-weight: 700;
          color: var(--muted); overflow: hidden; text-transform: uppercase; }
        .att .thumb img { width: 100%; height: 100%; object-fit: cover; }
        .att .meta-a { min-width: 0; }
        .att .fname { font-size: var(--size-footnote); font-weight: 550; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
        .att .fsize { font-size: var(--size-caption); color: var(--muted); }
        .att .save { margin-left: 2px; padding: 2px 7px; color: var(--muted); }
        """

        /// A chat log, a mail thread's or a chat space's: a flat list, one row per message, no cards or folds.
        static let chatLog = """
        main.chat, main.chat-space { border-top: 0; }
        .day { display: flex; align-items: center; gap: 12px; margin: 16px 0 6px; font-size: var(--size-caption); font-weight: 600;
          color: var(--muted); user-select: none; -webkit-user-select: none; }
        .day::before, .day::after { content: ""; flex: 1; height: 1px; background: var(--line); }
        .day.new { color: var(--accent); }
        .day.new::before, .day.new::after { background: color-mix(in srgb, var(--accent) 45%, transparent); }
        .msg.chat { position: relative; display: flex; gap: 12px; margin: 0 -10px; padding: 8px 10px 6px;
          border: 0; border-radius: var(--radius-card); scroll-margin-top: 14px;
          transition: background-color var(--dur-quick) var(--ease); }
        .msg.chat.continued { padding-top: 2px; }
        .msg.chat:hover { background: var(--hover); }
        .msg.static { animation: none; }
        .chat-gutter { flex: none; width: 34px; display: flex; justify-content: center; }
        .chat-time { font-size: var(--size-micro); line-height: 22px; color: var(--faint); white-space: nowrap;
          font-variant-numeric: tabular-nums; opacity: 0; transition: opacity var(--dur-quick) var(--ease); }
        .msg.chat.continued:hover .chat-time, .msg.chat.continued.unread .chat-time { opacity: 1; }
        .msg.chat.continued.unread .chat-time { color: var(--accent); font-weight: 600; }
        .chat-main { flex: 1; min-width: 0; }
        .chat-head { display: flex; align-items: baseline; gap: 8px; min-width: 0; white-space: nowrap; margin-bottom: 1px; }
        .chat-head .name { max-width: 60%; overflow: hidden; text-overflow: ellipsis; }
        .chat-head .icon.star { align-self: center; }
        .chat-head .recipients { flex: 1 1 auto; min-width: 0; margin: 0; color: var(--faint); }
        .msg.chat .msg-body { padding: 0 0 2px; }
        .msg.chat .plain { line-height: 1.55; }
        .msg.chat .attachments { padding: 8px 0 4px; margin: 0; }
        .msg.chat .actions { position: absolute; top: -10px; right: 10px; padding: 1px; border-radius: var(--radius-control);
          border: 1px solid var(--line); background: var(--bg); box-shadow: var(--shadow-popover); pointer-events: none; }
        .msg.chat:hover .actions, .msg.chat .actions:focus-within { opacity: 1; transform: none; pointer-events: auto; }
        .msg.chat.threaded .chat-text { padding-left: 10px; }
        .msg.chat.pending .chat-main { opacity: .55; }
        .msg.chat.failed .chat-text { color: var(--muted); }
        .chat-log { display: flex; flex-direction: column; }
        .chat-item { display: flex; flex-direction: column; }
        .chat-text { white-space: pre-wrap; overflow-wrap: anywhere; font-size: var(--size-reading); line-height: 1.55; }
        .chat-text a { color: var(--accent); }
        .chat-text code { font: var(--size-footnote) var(--font-mono); padding: 1px 4px; border-radius: var(--radius-small);
          background: var(--chip); }
        .chat-text pre { font: var(--size-footnote)/1.5 var(--font-mono); margin: 4px 0; padding: 8px 10px;
          border-radius: var(--radius-control); background: var(--chip); white-space: pre-wrap; }
        .chat-text .mention { color: var(--accent); font-weight: 550; }
        .chat-card { color: var(--muted); font-style: italic; }
        .chat-edited { color: var(--faint); font-size: var(--size-caption); white-space: nowrap; }
        .chat-thread-ref { display: flex; gap: 5px; align-items: baseline; min-width: 0; font-size: var(--size-footnote);
          color: var(--muted); margin: 1px 0 3px; padding-left: 8px;
          border-left: 2px solid color-mix(in srgb, var(--accent) 45%, var(--line)); white-space: nowrap; }
        .chat-thread-ref b { font-weight: 600; color: var(--text); }
        .chat-thread-ref .q { overflow: hidden; text-overflow: ellipsis; min-width: 0; color: var(--faint); }
        .chat-failed { display: flex; align-items: center; gap: 4px; margin-top: 2px; font-size: var(--size-footnote);
          color: var(--danger); }
        .chat-failed button { color: var(--accent); padding: 1px 6px; }
        .chat-earlier { display: flex; justify-content: center; margin: 0 0 6px; }
        .chat-earlier[hidden] { display: none; }
        .chat-earlier button { color: var(--accent); }
        .chat-status { color: var(--muted); font-size: var(--size-body); padding: 10px 0; }
        .chat-status:empty { display: none; }
        """

        /// The dock at the column's foot: a mail thread's reply pill bar, which folds into the composer.
        static let replyDock = """
        .reply-dock { position: sticky; bottom: 0; margin-top: auto; padding: 28px 0 20px;
          background: linear-gradient(to bottom, transparent, var(--bg) 38%); animation: rise var(--dur-long) var(--ease) .08s both; }
        /* The pill bar folds away as the composer unfolds in its place, both on the same curve. */
        .fold-y { display: grid; transition: grid-template-rows var(--dur-long) var(--ease); }
        .fold-clip { min-height: 0; overflow: hidden; padding: 6px 4px 16px; margin: -6px -4px -16px; }
        .bar-fold { grid-template-rows: 1fr; }
        .composer-fold { grid-template-rows: 0fr; pointer-events: none; }
        .reply-dock.composing .bar-fold { grid-template-rows: 0fr; pointer-events: none; }
        .reply-dock.composing .composer-fold { grid-template-rows: 1fr; pointer-events: auto; }
        .dock-bar { display: flex; gap: 6px; align-items: center; transition: opacity var(--dur-brief) var(--ease); }
        .reply-dock.composing .dock-bar { opacity: 0; }
        .reply-pill { flex: 1; display: flex; align-items: center; gap: 9px; height: 38px; padding: 0 16px;
          border-radius: 19px; border: 1px solid var(--line); background: var(--bg); color: var(--muted);
          font-size: var(--size-body); font-weight: 450; text-align: left; box-shadow: var(--shadow-raised); }
        .reply-pill:hover { background: var(--bg); color: var(--text); border-color: color-mix(in srgb, var(--accent) 40%, var(--line));
          box-shadow: var(--shadow-lifted); }
        .reply-pill .icon { color: var(--accent); }
        .reply-dock.sent .reply-pill { color: var(--success); border-color: color-mix(in srgb, var(--success) 45%, var(--line)); }
        .reply-dock.sent .reply-pill .icon { color: var(--success); }
        .dock-icon { width: 38px; height: 38px; padding: 0; border-radius: 50%; display: flex; align-items: center;
          justify-content: center; color: var(--muted); border: 1px solid var(--line); background: var(--bg); }
        .dock-icon:hover { color: var(--text); background: var(--bg); border-color: var(--faint); }
        """

        /// The composer (`ReaderPage.composer`): a mail reply's, then where a chat space's differs.
        static let composer = """
        .composer { display: flex; flex-direction: column; max-height: min(62vh, 560px); border-radius: var(--radius-panel);
          border: 1px solid var(--line); background: var(--bg); box-shadow: var(--shadow-floating);
          transform: translateY(12px) scale(.985); transform-origin: 50% 100%; opacity: 0;
          transition: transform var(--dur-long) var(--ease), opacity var(--dur-brief) var(--ease),
            border-color var(--dur-quick) var(--ease); }
        .reply-dock.composing .composer { transform: none; opacity: 1; }
        .composer:focus-within { border-color: color-mix(in srgb, var(--accent) 45%, var(--line)); }
        .c-head { display: flex; align-items: center; gap: 2px; padding: 7px 6px 0 14px; font-size: var(--size-footnote);
          color: var(--muted); }
        .c-mode { display: flex; color: var(--accent); margin-right: 8px; }
        .c-mode .icon:last-child, .reply-dock.all .c-mode .icon:first-child { display: none; }
        .reply-dock.all .c-mode .icon:last-child { display: block; }
        .c-to { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
        .c-to b { font-weight: 550; color: var(--text); }
        .c-label { color: var(--faint); margin-right: 5px; }
        .c-label:not(:first-child) { margin-left: 12px; }
        .c-pending { display: inline-block; width: 120px; height: 9px; border-radius: 5px; background: var(--chip);
          vertical-align: middle; animation: pulse 1.4s ease-in-out infinite; }
        .c-btn { padding: 5px 7px; color: var(--muted); }
        .c-btn:hover { color: var(--text); }
        .c-btn.on { color: var(--accent); }
        .c-btn[hidden], .c-quote[hidden] { display: none; }
        .c-scroll { flex: 1 1 auto; min-height: 0; overflow-y: auto; }
        #c-text { display: block; width: 100%; min-height: 68px; padding: 8px 14px 6px; border: 0; outline: 0; resize: none;
          background: transparent; color: var(--text); font: var(--size-reading)/1.55 var(--font-ui); }
        #c-text::placeholder { color: var(--faint); }
        .c-quote { padding: 0 14px 4px; }
        .c-quote .ms-fold-toggle { padding: 0 9px; margin: 2px 0 6px; }
        .c-quote-fold { display: grid; grid-template-rows: 0fr; transition: grid-template-rows var(--dur-standard) var(--ease); }
        .c-quote.open .c-quote-fold { grid-template-rows: 1fr; }
        .c-quote-fold > div { min-height: 0; overflow: hidden; }
        .c-quote-body { padding: 2px 0 8px 12px; border-left: 2px solid var(--line); opacity: 0;
          transition: opacity var(--dur-brief) var(--ease); }
        .c-quote.open .c-quote-body { opacity: 1; }
        .c-quote-attr { font-size: var(--size-footnote); color: var(--muted); margin-bottom: 6px; }
        .c-foot { display: flex; align-items: center; gap: 10px; padding: 6px 8px 8px 14px; }
        .c-status { flex: 1; min-width: 0; font-size: var(--size-footnote); color: var(--muted); white-space: nowrap;
          overflow: hidden; text-overflow: ellipsis; }
        .c-status.error { color: var(--danger); }
        .c-send { display: flex; align-items: center; gap: 6px; height: 28px; padding: 0 12px; border-radius: var(--radius-control);
          background: var(--accent); color: var(--on-accent); font-weight: 600; }
        .c-send:hover { background: color-mix(in srgb, var(--accent) 86%, black); }
        .c-send .icon { width: 13px; height: 13px; }
        .c-send .kbd { font-weight: 500; font-size: var(--size-caption); opacity: .72; }
        .composer.sending .c-send { opacity: .55; pointer-events: none; }
        .composer.sending #c-text { opacity: .6; }
        .chat-dock { padding-top: 18px; }
        .chat-dock #c-text { min-height: 40px; padding-top: 10px; }
        .chat-dock .c-head[hidden] { display: none; }
        .chat-dock .c-status { color: var(--faint); }
        """

        static let reducedMotion = """
        @media (prefers-reduced-motion: reduce) { *, *::before { animation: none !important; transition: none !important; } }
        """

        /// Appended to every HTML mail frame, over the mail's own styles. The frame stays transparent
        /// so a body takes on its card's surface (the chat hover tint; in dark mode the frame's own
        /// white card).
        static let frame = """
        html, body { height: auto !important; min-height: 0 !important; }
        body { margin: 0; padding: 2px 0; color: var(--text); font: var(--size-reading)/1.45 var(--font-ui);
          overflow-wrap: break-word; }
        img { max-width: 100%; }
        pre { white-space: pre-wrap; }

        """ + mailContent
    }

    // MARK: - Script

    /// The mail thread page, run after `WebShell`'s script, whose `Shell` it uses.
    static let script = """
    (() => {
      const { post, $, escapeHTML, show, autosize, bindComposer, focusAtEnd, interceptLinks } = Shell;
      const FRAME_TOKENS = \(jsString(Design.stylesheet(fixedTo: .aqua)));
      const FRAME_CSS = \(jsString(Style.frame));
      // The reply's text field never gets shorter than this.
      const TEXT_MIN = 68;
      // Chat threads fold signatures along with quotes.
      let chatMode = false;
      // Clears the "Reply sent" note on the composer pill.
      let sentTimer = 0;
      const SIGNATURES = ['.gmail_signature', '.gmail_signature_prefix', '[data-smartmail="gmail_signature"]',
                          '.moz-signature', '#Signature', '#AppleMailSignature', '#ms-outlook-mobile-signature',
                          '.ms-signature'].join(', ');

      // A chat thread opens on one message and keeps it in place while bodies above it load and
      // grow, until the user scrolls, clicks or types. WebKit may still move the page on its own
      // (restoring a position after the shell loads); any such scroll is put back.
      let followed = null, followObserver = null, pinned = 0;
      function unfollow() {
        followed = null;
        if (followObserver) followObserver.disconnect();
        followObserver = null;
      }
      function place() {
        if (!followed) return;
        followed.scrollIntoView({ block: 'start' });
        pinned = window.scrollY;
      }
      function follow(index) {
        unfollow();
        followed = document.getElementById('msg-' + index);
        if (!followed) return;
        followObserver = new ResizeObserver(place);
        followObserver.observe($('thread'));
        place();
      }
      for (const type of ['wheel', 'mousedown', 'keydown']) {
        window.addEventListener(type, unfollow, { capture: true, passive: true });
      }
      window.addEventListener('scroll', () => { if (followed && Math.abs(window.scrollY - pinned) > 1) place(); });

      const mail = {
        act(el, action) {
          if (action === 'compose-to' && el.closest('.msg.collapsed')) {
            toggle(Number(el.closest('.msg').id.slice(4)));
          } else if (action === 'toggle') {
            if (window.getSelection && String(window.getSelection()).length > 0) return;
            toggle(Number(el.dataset.index));
          } else if (action === 'reply-quote') {
            toggleQuote();
          } else {
            post({ action, index: el.dataset.index !== undefined ? Number(el.dataset.index) : -1,
                   part: el.dataset.part !== undefined ? Number(el.dataset.part) : -1,
                   email: el.dataset.email || '', text: composerText() });
          }
        },
        leave() {
          clearTimeout(sentTimer);
          unfollow();
        },
      };

      function toggle(index) {
        const article = document.getElementById('msg-' + index);
        if (!article) return;
        const expanding = article.classList.contains('collapsed');
        article.classList.toggle('collapsed', !expanding);
        article.classList.toggle('expanded', expanding);
        if (expanding && !article.dataset.loaded) post({ action: 'expand', index });
        if (expanding) article.querySelectorAll('iframe').forEach(fit);
      }

      function fit(frame) {
        const doc = frame.contentDocument;
        if (!doc || !doc.documentElement || frame.clientWidth === 0) return;
        const root = doc.documentElement;
        root.style.zoom = '';
        const width = Math.max(root.scrollWidth, doc.body ? doc.body.scrollWidth : 0);
        const style = getComputedStyle(frame);
        const available = frame.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
        let zoom = 1;
        if (width > available + 2) {
          zoom = available / width;
          root.style.zoom = String(zoom);
        }
        const height = Math.ceil(Math.max(root.scrollHeight, doc.body ? doc.body.scrollHeight : 0) * zoom) + 1;
        if (Math.abs(frame.offsetHeight - height) > 1) frame.style.height = height + 'px';
      }

      function visibleTextLength(nodes) {
        return nodes.reduce((sum, node) => sum + (node.textContent || '').replace(/\\s+/g, '').length, 0);
      }

      // Folds trailing quoted history behind a "•••" toggle, unless folding would hide everything.
      function foldQuotes(doc, container, onToggle) {
        // Nodes just before `el` ("On …, X wrote:" as an element, or a bare text line plus <br>),
        // with text nodes wrapped in spans so they can be hidden.
        function attributionBefore(doc, el, pattern) {
          const nodes = [];
          let text = '';
          for (let node = el.previousSibling; node && nodes.length < 6; node = node.previousSibling) {
            if (node.nodeType === 8 || (node.nodeType === 1 && node.tagName === 'BR')
                || (node.nodeType === 3 && !node.textContent.trim())) { nodes.push(node); continue; }
            if (node.nodeType === 1 && node.querySelector('img')) break;
            if (node.nodeType === 3) {
              // Plain text runs the reply into its "On … wrote:" line (sometimes wrapped before
              // "wrote:"): only that tail is the attribution, split off so the reply stays.
              const body = node.textContent.replace(/\\s+$/, '');
              let cut = body.lastIndexOf('\\n');
              if (cut >= 0 && body.length - cut < 12) cut = body.lastIndexOf('\\n', cut - 1);
              if (cut >= 0 && pattern.test(body.slice(cut + 1).trim())) node = node.splitText(cut + 1);
            }
            if (node.nodeType === 1 || node.nodeType === 3) { nodes.push(node); text = node.textContent || ''; }
            break;
          }
          if (!text || text.length > 400 || !pattern.test(text.trim())) return [];
          return nodes.map((node) => {
            if (node.nodeType === 1) return node;
            const span = doc.createElement('span');
            node.parentNode.insertBefore(span, node);
            span.appendChild(node);
            return span;
          });
        }
        const selectors = ['.gmail_quote', 'blockquote[type="cite"]', '#divRplyFwdMsg', '.yahoo_quoted',
                           '.moz-cite-prefix', 'blockquote.ms-quote', '#appendonsend'];
        let found = [];
        for (const selector of selectors) {
          container.querySelectorAll(selector).forEach((el) => {
            if (!found.some((other) => other.contains(el))) {
              found = found.filter((other) => !el.contains(other));
              found.push(el);
            }
          });
        }
        const folded = new Set();
        // "On …, X wrote:" just above a quote belongs to it, so it folds away too.
        const attribution = /(wrote|写道|écrit|schrieb)\\s*[:：]\\s*$/i;
        for (const el of found) {
          folded.add(el);
          attributionBefore(doc, el, attribution).forEach((node) => folded.add(node));
          if (el.id === 'divRplyFwdMsg' || el.id === 'appendonsend') {
            let prev = el.previousElementSibling;
            if (prev && prev.tagName === 'HR') folded.add(prev);
            let next = el.nextElementSibling;
            while (next) { folded.add(next); next = next.nextElementSibling; }
          }
        }
        const earliest = () => [...folded].filter((el) => el.isConnected)
          .sort((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) ? -1 : 1)[0];
        const text = (container.textContent || '').replace(/\\s+/g, '').length;
        if (folded.size) {
          if (text - visibleTextLength([...folded]) < 8) return;
          if (/Forwarded message|转发的邮件/.test((earliest().textContent || '').slice(0, 300))) return;
        }
        // In a chat log signatures fold too, each only while real text stays visible.
        if (chatMode) {
          for (const el of container.querySelectorAll(SIGNATURES)) {
            if ([...folded].some((other) => other.contains(el))) continue;
            if (text - visibleTextLength([...folded, el]) < 8) continue;
            folded.add(el);
          }
        }
        const first = earliest();
        if (!first) return;
        if (chatMode) {
          blankTail(doc, first).forEach((node) => folded.add(node));
          // Breaks between and after folded blocks (Gmail puts the signature below the quote).
          for (const el of [...folded]) blankAfter(doc, el, folded).forEach((node) => folded.add(node));
        }
        folded.forEach((el) => el.classList.add('ms-folded'));
        const toggleButton = doc.createElement('span');
        // In a chat log the blank lines above it fold away, so it starts its own line.
        toggleButton.className = chatMode ? 'ms-fold-toggle block' : 'ms-fold-toggle';
        toggleButton.title = 'Show quoted text';
        toggleButton.textContent = '•••';
        toggleButton.addEventListener('click', (event) => {
          event.preventDefault();
          event.stopPropagation();
          folded.forEach((el) => el.classList.toggle('ms-folded'));
          onToggle();
        });
        first.parentNode.insertBefore(toggleButton, first);
      }

      // Blank lines, breaks and empty blocks just before `el` (or the block it starts), reaching into
      // the last element with text, whose trailing whitespace is trimmed; text nodes come back
      // wrapped so they can hide.
      function blankTail(doc, el) {
        const nodes = [];
        let anchor = el;
        while (!anchor.previousSibling && anchor.parentNode && anchor.parentNode !== doc.body
               && anchor.parentNode.nodeType === 1) anchor = anchor.parentNode;
        let node = anchor.previousSibling;
        while (node) {
          const previous = node.previousSibling;
          if (node.nodeType === 3) {
            if (node.textContent.trim()) {
              node.textContent = node.textContent.replace(/\\s+$/, '');
              break;
            }
            nodes.push(wrapText(doc, node));
          } else if (node.nodeType === 1) {
            if (!isEmptyElement(node)) { node = node.lastChild; continue; }
            nodes.push(node);
          }
          node = previous;
        }
        return nodes;
      }

      // Blank siblings after `el`, skipping ones already in `folded`, up to the next real content.
      function blankAfter(doc, el, folded) {
        const nodes = [];
        for (let node = el.nextSibling; node; node = node.nextSibling) {
          if (node.nodeType === 8 || folded.has(node)) continue;
          if (node.nodeType === 3) {
            if (node.textContent.trim()) break;
            node = wrapText(doc, node);
          } else if (node.nodeType !== 1 || !isEmptyElement(node)) {
            break;
          }
          nodes.push(node);
        }
        return nodes;
      }

      function isEmptyElement(node) {
        return node.tagName === 'BR'
          || (!node.textContent.trim() && !node.querySelector('img, video, iframe, table, hr'));
      }

      function wrapText(doc, node) {
        const span = doc.createElement('span');
        node.parentNode.insertBefore(span, node);
        span.appendChild(node);
        return span;
      }

      function prepareFrame(frame) {
        const doc = frame.contentDocument;
        if (!doc || !doc.documentElement) return;
        // The tokens go first, so a mail's own custom properties of the same names keep winning in
        // its markup; the rules go last, over the mail's.
        const head = doc.head || doc.documentElement;
        const tokens = doc.createElement('style');
        tokens.textContent = FRAME_TOKENS;
        head.insertBefore(tokens, head.firstChild);
        const style = doc.createElement('style');
        style.textContent = FRAME_CSS;
        head.appendChild(style);
        if (doc.body) foldQuotes(doc, doc.body, () => fit(frame));
        interceptLinks(doc);
        let pending = false;
        const schedule = () => {
          if (pending) return;
          pending = true;
          requestAnimationFrame(() => { pending = false; fit(frame); });
        };
        fit(frame);
        if (doc.body) new ResizeObserver(schedule).observe(doc.body);
        doc.querySelectorAll('img').forEach((img) => img.addEventListener('load', schedule));
      }

      window.addEventListener('resize', () => document.querySelectorAll('iframe.mail-frame').forEach(fit));

      // MARK: Inline reply

      function composerText() {
        const text = $('c-text');
        return text ? text.value : '';
      }

      // Every keystroke reaches the app, so Escape and leaving the conversation never lose text.
      function reportReply() {
        const text = $('c-text');
        if (text) post({ action: 'reply-state', text: text.value, focused: document.activeElement === text });
      }

      function setStatus(state, message) {
        $('composer').classList.toggle('sending', state === 'sending');
        $('c-status').classList.toggle('error', state === 'error');
        $('c-status').textContent = message || '';
      }

      function toggleQuote() {
        const wrap = $('c-quote-wrap');
        const open = !wrap.classList.contains('open');
        wrap.classList.toggle('open', open);
        wrap.querySelector('.ms-fold-toggle').title = open ? 'Hide quoted text' : 'Show quoted text';
        if (open) wrap.querySelectorAll('iframe').forEach(fit);
      }

      function closeComposer() {
        const text = $('c-text');
        $('dock').classList.remove('composing');
        text.blur();
        text.value = '';
        autosize(text, TEXT_MIN);
        $('c-quote-wrap').classList.remove('open');
        setStatus('idle', '');
      }

      // An HTML body in an auto-sized frame, or plain text, with its quoted history folded.
      function renderBody(body, payload) {
        body.innerHTML = '';
        if (payload.kind === 'html') {
          const frame = document.createElement('iframe');
          frame.className = 'mail-frame';
          frame.setAttribute('scrolling', 'no');
          frame.addEventListener('load', () => prepareFrame(frame));
          frame.srcdoc = payload.content;
          body.appendChild(frame);
        } else if (payload.kind === 'text') {
          const div = document.createElement('div');
          div.className = 'plain';
          div.innerHTML = payload.content;
          body.appendChild(div);
          foldQuotes(document, div, () => {});
        } else {
          body.innerHTML = '<div class="error">' + escapeHTML(payload.content) + '</div>';
        }
      }

      Object.assign(MS, {
        // Replaces the whole column, so bodies, frames, folds, the composer and the banner all
        // start fresh, scrolled to the top (a chat thread: to its focus message). Bodies already in
        // memory come along, so the thread's first frame shows them.
        render(state) {
          show(mail, state);
          chatMode = !!state.chat;
          // The composer is rebuilt with every thread, so its listeners are bound per render.
          bindComposer($('c-text'), TEXT_MIN, {
            report: reportReply,
            isSend: (event) => event.metaKey || event.ctrlKey,
            send: (text) => post({ action: 'reply-send', text: text.value }),
          });
          for (const [index, payload] of state.bodies) MS.setBody(index, payload);
          if (chatMode && state.focus > 0) follow(state.focus);
        },
        setBody(index, payload) {
          const article = document.getElementById('msg-' + index);
          const body = document.getElementById('body-' + index);
          if (!article || !body) return;
          article.dataset.loaded = '1';
          const preview = article.querySelector('.snippet');
          if (preview && payload.snippet && !preview.textContent) preview.textContent = payload.snippet;
          renderBody(body, payload);
          if (payload.recipients !== undefined) {
            const rcpt = document.getElementById('rcpt-' + index);
            if (rcpt) rcpt.innerHTML = payload.recipients;
          }
          document.getElementById('att-' + index).innerHTML = payload.attachments || '';
          if (payload.remote) document.getElementById('remote-banner').hidden = false;
        },
        // A sender's picture arrived after the thread rendered.
        setAvatar(email, url) {
          for (const el of document.querySelectorAll('.avatar[data-avatar]')) {
            if (el.dataset.avatar !== email) continue;
            el.textContent = '';
            el.classList.add('photo');
            el.dataset.action = 'avatar';
            el.style.backgroundImage = "url('" + url + "')";
          }
        },
        scrollToMessage(index) {
          const article = document.getElementById('msg-' + index);
          if (article) article.scrollIntoView({ block: 'start' });
        },
        openReply(options) {
          const dock = $('dock'), text = $('c-text'), all = $('c-all');
          clearTimeout(sentTimer);
          dock.classList.remove('sent');
          dock.classList.add('composing');
          dock.classList.toggle('all', !!options.all);
          all.hidden = !options.canAll;
          all.classList.toggle('on', !!options.all);
          all.title = options.all ? 'Reply to Sender Only' : 'Reply All';
          $('c-to').innerHTML = '<span class="c-label">To</span><span class="c-pending"></span>';
          if (!text.value && options.text) text.value = options.text;
          autosize(text, TEXT_MIN);
          setStatus(options.sending ? 'sending' : 'idle', options.sending ? 'Sending…' : '');
          if (options.focus) focusAtEnd(text);
        },
        setReplyInfo(info) {
          let html = '<span class="c-label">To</span><b>' + escapeHTML(info.to || '—') + '</b>';
          if (info.cc) html += '<span class="c-label">Cc</span><span>' + escapeHTML(info.cc) + '</span>';
          $('c-to').innerHTML = html;
          $('c-to').title = info.title || '';
        },
        setReplyQuote(quote) {
          const body = $('c-quote');
          body.innerHTML = '<div class="c-quote-attr">' + escapeHTML(quote.attribution || '') + '</div>';
          const holder = document.createElement('div');
          body.appendChild(holder);
          renderBody(holder, quote);
          $('c-quote-wrap').hidden = false;
        },
        replyStatus(state, message) { setStatus(state, message); },
        replySent() {
          closeComposer();
          const dock = $('dock'), label = $('pill-label');
          if (!label.dataset.original) label.dataset.original = label.textContent;
          label.textContent = 'Reply sent';
          dock.classList.add('sent');
          clearTimeout(sentTimer);
          sentTimer = setTimeout(() => {
            dock.classList.remove('sent');
            label.textContent = label.dataset.original;
          }, 2600);
        },
        closeReply() { closeComposer(); },
        blurReply() { $('c-text').blur(); },
      });
    })();
    """

    private static func jsString(_ text: String) -> String {
        String(decoding: (try? JSONEncoder().encode(text)) ?? Data("\"\"".utf8), as: UTF8.self)
    }
}
