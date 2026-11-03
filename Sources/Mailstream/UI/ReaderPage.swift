import AppKit

/// HTML scaffolding for the conversation reader: one long-lived shell page whose column
/// `MS.render` fills per conversation. Message bodies arrive later through `MS.setBody`; HTML mail
/// renders inside a same-origin `srcdoc` iframe that inherits the page's Content-Security-Policy,
/// so remote content stays blocked until the user allows it and the relaxed shell is loaded.
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

    /// The persistent page: styles and an empty column (the script is a user script). A CSP cannot
    /// be loosened after load, so allowing remote content means navigating to the relaxed shell.
    static func shell(allowRemote: Bool) -> String {
        let remote = allowRemote ? " https: http:" : ""
        let csp = "default-src 'none'; img-src data: mscid:\(remote); style-src 'unsafe-inline'\(remote); "
            + "font-src data:\(remote); media-src data: mscid:; frame-src about:; child-src about:"
        return """
        <!doctype html><html><head><meta charset="utf-8">
        <meta http-equiv="Content-Security-Policy" content="\(csp)">
        <meta name="color-scheme" content="light dark">
        <style>\(css)</style></head><body><div class="column" id="column"></div></body></html>
        """
    }

    /// The column's content for one conversation: header, remote banner, thread and reply dock.
    /// `chat` lays the thread out as a conversation: every message open, grouped by sender and day.
    static func thread(conversation: Conversation, cards: [Card], own: Set<String>, accountLabel: String,
                       accountColor: String, mailboxLabels: [String], tag: ConversationTag?, chat: Bool) -> String {
        var meta: [String] = []
        if tag?.toRespond == true { meta.append(Self.meta("Waiting on you", tint: "#F59E0B", mark: "dot")) }
        if let tag, tag.category != .people { meta.append(Self.meta(tag.category.title, tint: nil)) }
        for label in tag?.labels ?? [] { meta.append(Self.meta(label, tint: TagPalette.hex(for: label), mark: "dot")) }
        meta.append(Self.meta(accountLabel, tint: accountColor, mark: "bar"))
        for label in mailboxLabels { meta.append(Self.meta(label, tint: nil)) }
        if conversation.messages.count > 1 { meta.append(Self.meta("\(conversation.messages.count) messages", tint: nil)) }

        let replyTarget = conversation.messages.last { message in
            !(message.envelope.from.contains { own.contains($0.normalizedEmail) })
        }?.envelope.from.first?.shortName
        let replyLabel = replyTarget.map { "Reply to \(HTMLText.escape($0))…" } ?? "Reply…"
        var html = """
        <header class="thread-header">
          <h1>\(HTMLText.escape(SubjectStyle.split(conversation.subject).title))</h1>
          <div class="thread-meta">\(meta.joined(separator: "<i class=\"sep\">·</i>"))</div>
        </header>
        <div id="remote-banner" class="banner" hidden>
          <span>Remote images are blocked to protect your privacy.</span>
          <button data-action="load-remote">Load Images</button>
        </div>
        <main id="thread"\(chat ? " class=\"chat\"" : "")>
        """
        if chat {
            html += chatMessages(cards, own: own)
        } else {
            for (position, card) in cards.enumerated() { html += article(card, own: own, position: position, total: cards.count) }
        }
        html += """
        </main>
        <footer class="reply-dock" id="dock">
          <div class="fold-y bar-fold"><div class="fold-clip"><div class="dock-bar">
            <button class="reply-pill" data-action="reply-latest">\(Icon.reply)<span id="pill-label">\(replyLabel)</span></button>
            <button class="dock-icon" title="Reply All" data-action="replyAll-latest">\(Icon.replyAll)</button>
            <button class="dock-icon" title="Forward" data-action="forward-latest">\(Icon.forward)</button>
          </div></div></div>
          <div class="fold-y composer-fold"><div class="fold-clip">
            <section class="composer" id="composer">
              <div class="c-head">
                <span class="c-mode">\(Icon.reply)\(Icon.replyAll)</span>
                <div class="c-to" id="c-to"></div>
                <button class="c-btn" id="c-all" title="Reply All" data-action="reply-toggle-all" hidden>\(Icon.replyAll)</button>
                <button class="c-btn" title="Open in Window" data-action="reply-popout">\(Icon.popout)</button>
                <button class="c-btn" title="Discard" data-action="reply-discard">\(Icon.trash)</button>
              </div>
              <div class="c-scroll">
                <textarea id="c-text" rows="1" placeholder="Write a reply…" spellcheck="true"></textarea>
                <div class="c-quote" id="c-quote-wrap" hidden>
                  <button class="ms-fold-toggle" data-action="reply-quote" title="Show quoted text">•••</button>
                  <div class="c-quote-fold"><div><div class="c-quote-body" id="c-quote"></div></div></div>
                </div>
              </div>
              <div class="c-foot">
                <span class="c-status" id="c-status"></span>
                <button class="c-send" data-action="reply-send">\(Icon.send)<span>Send</span><span class="kbd">⌘↩</span></button>
              </div>
            </section>
          </div></div>
        </footer>
        """
        return html
    }

    /// Inline SVG glyphs, so icons match SF Symbols' weight instead of falling back to text arrows.
    enum Icon {
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
    private static func meta(_ text: String, tint: String?, mark: String? = nil) -> String {
        let style = tint.map { " style=\"--tint:\(HTMLText.escape($0))\"" } ?? ""
        let markup = mark.map { "<i class=\"mark \($0)\"></i>" } ?? ""
        return "<span class=\"meta-item\"\(style)>\(markup)\(HTMLText.escape(text))</span>"
    }

    /// A sender's next message within this window, on the same day, joins their previous one.
    private static let groupWindow: TimeInterval = 3600

    /// Messages as a chat log: a divider per day, and runs from one sender under one name.
    private static func chatMessages(_ cards: [Card], own: Set<String>) -> String {
        let calendar = Calendar.current
        var html = ""
        var previous: Card?
        for (position, card) in cards.enumerated() {
            let date = card.message.envelope.date
            let last = previous?.message.envelope.date
            let sameDay = date.flatMap { date in last.map { calendar.isDate($0, inSameDayAs: date) } } ?? false
            if let date, !sameDay { html += dayDivider(date) }
            let continued = sameDay && previous?.sender.normalizedEmail == card.sender.normalizedEmail
                && date.flatMap { date in last.map { date.timeIntervalSince($0) < groupWindow } } == true
            html += chatArticle(card, own: own, continued: continued, position: position, total: cards.count)
            previous = card
        }
        return html
    }

    private static func dayDivider(_ date: Date) -> String {
        let calendar = Calendar.current
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

    /// One message in a chat log; a `continued` one drops the avatar and name, showing its time on hover.
    private static func chatArticle(_ card: Card, own: Set<String>, continued: Bool, position: Int, total: Int) -> String {
        let envelope = card.message.envelope
        let sender = card.sender
        let isOwn = own.contains(sender.normalizedEmail)
        let name = HTMLText.escape(isOwn ? "\(sender.displayName) (me)" : sender.displayName)
        let email = HTMLText.escape(sender.email)
        let color = Avatar.color(for: sender.email).cssHex
        let time = HTMLText.escape(envelope.date?.formatted(.dateTime.hour().minute()) ?? "")
        let full = HTMLText.escape(envelope.date?.formatted(date: .complete, time: .shortened) ?? "")
        let i = card.index
        let gutter: String
        let head: String
        if continued {
            gutter = "<span class=\"chat-time\" title=\"\(full)\">\(time)</span>"
            head = ""
        } else {
            let key = HTMLText.escape(sender.normalizedEmail)
            gutter = card.avatar.map {
                "<div class=\"avatar photo\" data-action=\"avatar\" data-avatar=\"\(key)\" style=\"background-image:url('\($0)')\"></div>"
            } ?? "<div class=\"avatar\" data-avatar=\"\(key)\">\(HTMLText.escape(Avatar.initials(for: sender)))</div>"
            head = """
            <div class="chat-head">
              <span class="name" title="\(email)">\(name)</span>
              <span class="date" title="\(full)">\(time)</span>
              \(envelope.isFlagged ? Icon.star : "")
              <span class="recipients" id="rcpt-\(i)">\(recipients(envelope.to, cc: [], own: own))</span>
            </div>
            """
        }
        let unread = envelope.isSeen ? "" : " unread"
        let delay = max(0, position - max(0, total - 6)) * 30
        return """
        <article id="msg-\(i)" class="msg chat expanded\(continued ? " continued" : "")\(unread)" style="--avatar:\(color); animation-delay:\(delay)ms">
          <div class="chat-gutter">\(gutter)</div>
          <div class="chat-main">
            \(head)
            <div class="msg-body" id="body-\(i)"><div class="skeleton"><i></i><i></i><i class="short"></i></div></div>
            <div class="attachments" id="att-\(i)"></div>
          </div>
          <div class="actions">
            <button title="Reply" data-action="reply" data-index="\(i)">\(Icon.reply)</button>
            <button title="Reply All" data-action="replyAll" data-index="\(i)">\(Icon.replyAll)</button>
            <button title="Forward" data-action="forward" data-index="\(i)">\(Icon.forward)</button>
          </div>
        </article>
        """
    }

    private static func article(_ card: Card, own: Set<String>, position: Int, total: Int) -> String {
        let envelope = card.message.envelope
        let sender = card.sender
        let isOwn = own.contains(sender.normalizedEmail)
        let name = HTMLText.escape(isOwn ? "\(sender.displayName) (me)" : sender.displayName)
        let email = HTMLText.escape(sender.email)
        let color = Avatar.color(for: sender.email).cssHex
        let initials = HTMLText.escape(Avatar.initials(for: sender))
        let key = HTMLText.escape(sender.normalizedEmail)
        let avatar = card.avatar.map {
            "<div class=\"avatar photo\" data-action=\"avatar\" data-avatar=\"\(key)\" style=\"background-image:url('\($0)')\"></div>"
        } ?? "<div class=\"avatar\" data-avatar=\"\(key)\">\(initials)</div>"
        let date = envelope.date.map { date -> String in
            let calendar = Calendar.current
            let style: Date.FormatStyle = calendar.isDate(date, equalTo: Date(), toGranularity: .year)
                ? .dateTime.month(.abbreviated).day().hour().minute()
                : .dateTime.month(.abbreviated).day().year().hour().minute()
            return HTMLText.escape(date.formatted(style))
        } ?? ""
        let state = card.expanded ? "expanded" : "collapsed"
        let unread = envelope.isSeen ? "" : " unread"
        let i = card.index
        // Later messages arrive a beat after earlier ones; only the last few animate at all.
        let delay = max(0, position - max(0, total - 6)) * 30
        return """
        <article id="msg-\(i)" class="msg \(state)\(unread)" style="--avatar:\(color); animation-delay:\(delay)ms">
          <div class="msg-head" data-action="toggle" data-index="\(i)">
            \(avatar)
            <div class="who">
              <div class="line1">
                <span class="name">\(name)</span>
                <span class="email" data-action="compose-to" data-email="\(email)">\(email)</span>
                <span class="snippet">\(HTMLText.escape(card.snippet ?? ""))</span>
              </div>
              <div class="recipients" id="rcpt-\(i)">\(recipients(envelope.to, cc: [], own: own))</div>
            </div>
            <div class="meta">
              <div class="actions">
                <button title="Reply" data-action="reply" data-index="\(i)">\(Icon.reply)</button>
                <button title="Reply All" data-action="replyAll" data-index="\(i)">\(Icon.replyAll)</button>
                <button title="Forward" data-action="forward" data-index="\(i)">\(Icon.forward)</button>
              </div>
              \(envelope.isFlagged ? Icon.star : "")
              <span class="date">\(date)</span>
            </div>
          </div>
          <div class="fold"><div class="fold-inner"><div class="fold-content">
            <div class="msg-body" id="body-\(i)"><div class="skeleton"><i></i><i></i><i class="short"></i></div></div>
            <div class="attachments" id="att-\(i)"></div>
          </div></div></div>
        </article>
        """
    }

    /// "to Josh, me +2", with the full list in the tooltip.
    static func recipients(_ to: [Address], cc: [Address], own: Set<String>) -> String {
        let all = (to + cc).map { own.contains($0.normalizedEmail) ? "me" : $0.displayName }
        guard !all.isEmpty else { return "" }
        let shown = all.prefix(3).joined(separator: ", ")
        let more = all.count > 3 ? " +\(all.count - 3)" : ""
        let full = (to.isEmpty ? "" : "to " + to.map(\.rfc5322).joined(separator: ", "))
            + (cc.isEmpty ? "" : "\ncc " + cc.map(\.rfc5322).joined(separator: ", "))
        return "<span title=\"\(HTMLText.escape(full))\">to \(HTMLText.escape(shown))\(more)</span>"
    }

    static func hasRemoteContent(_ html: String) -> Bool {
        html.range(of: #"(src|background|poster)\s*=\s*["']?\s*(https?:)?//|url\(\s*["']?\s*(https?:)?//|<link[^>]+href\s*=\s*["']?\s*https?:"#,
                   options: [.regularExpression, .caseInsensitive]) != nil
    }

    // MARK: - Styles

    static let css = """
    :root {
      color-scheme: light dark;
      --bg: #ffffff; --text: #1d1d1f; --muted: #6e6e73; --faint: #a1a1a6; --line: rgba(0,0,0,.07);
      --hover: rgba(0,0,0,.035); --chip: rgba(0,0,0,.05); --accent: #007aff;
      --ease: cubic-bezier(.2,.9,.25,1);
    }
    @media (prefers-color-scheme: dark) {
      :root { --bg: #1e1e1e; --text: #f2f2f7; --muted: #a1a1a6; --faint: #6e6e73; --line: rgba(255,255,255,.08);
        --hover: rgba(255,255,255,.04); --chip: rgba(255,255,255,.08); }
    }
    * { box-sizing: border-box; }
    html, body { margin: 0; background: var(--bg); color: var(--text);
      font: 14px/1.5 -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif;
      -webkit-font-smoothing: antialiased; }
    .column { max-width: 720px; margin: 0 auto; padding: 30px 28px 0; min-height: 100vh;
      display: flex; flex-direction: column; }
    /* The native view handles the fade; the page only settles, so text is legible from the first frame. */
    @keyframes rise { from { opacity: .35; transform: translateY(10px); } to { opacity: 1; transform: none; } }
    @keyframes settle { from { transform: translateY(6px); } to { transform: none; } }
    .thread-header { margin: 0 0 18px; animation: settle .45s var(--ease) both; }
    .thread-header h1 { font-family: "New York", ui-serif, Georgia, serif; font-size: 27px; line-height: 1.22;
      font-weight: 600; letter-spacing: -.012em; margin: 0 0 9px; overflow-wrap: anywhere; }
    .thread-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; font-size: 12px; color: var(--muted); }
    .thread-meta .sep { font-style: normal; color: var(--faint); }
    .meta-item { display: inline-flex; align-items: center; gap: 6px; white-space: nowrap; }
    .mark { display: inline-block; background: var(--tint); }
    .mark.dot { width: 6px; height: 6px; border-radius: 50%; }
    .mark.bar { width: 2.5px; height: 11px; border-radius: 2px; }
    .banner { display: flex; align-items: center; gap: 12px; margin: 0 0 14px; padding: 7px 8px 7px 14px;
      border-radius: 10px; font-size: 12px; color: var(--muted); background: var(--chip);
      animation: rise .3s var(--ease) both; }
    .banner[hidden] { display: none; }
    .banner span { flex: 1; }
    button { font: inherit; font-size: 12px; font-weight: 550; color: var(--text); background: transparent;
      border: 0; border-radius: 7px; padding: 4px 10px; cursor: default;
      transition: background-color .15s var(--ease), color .15s var(--ease), transform .15s var(--ease); }
    button:hover { background: var(--hover); }
    button:active { transform: scale(.96); }
    .banner button { color: var(--accent); background: var(--bg); }
    .icon { width: 15px; height: 15px; display: block; flex: none; }
    .icon.star { color: #f5b800; width: 12px; height: 12px; }
    main { display: flex; flex-direction: column; border-top: 1px solid var(--line); }
    .msg { border-bottom: 1px solid var(--line); animation: rise .34s var(--ease) both; }
    .msg-head { display: flex; gap: 12px; align-items: center; cursor: default; padding: 10px 6px;
      margin: 0 -6px; border-radius: 8px; user-select: none; -webkit-user-select: none;
      transition: background-color .15s var(--ease); }
    .msg.collapsed .msg-head:hover { background: var(--hover); }
    .msg.expanded .msg-head { padding-top: 18px; align-items: flex-start; }
    .avatar { flex: none; width: 34px; height: 34px; border-radius: 50%; font-weight: 650; font-size: 15px;
      display: flex; align-items: center; justify-content: center; color: var(--avatar);
      background: color-mix(in srgb, var(--avatar) 15%, transparent); transition: transform .25s var(--ease); }
    .msg.collapsed .avatar { transform: scale(.86); }
    .avatar.photo { background-size: cover; background-position: center; cursor: zoom-in; }
    .who { flex: 1; min-width: 0; }
    .line1 { display: flex; gap: 8px; align-items: baseline; white-space: nowrap; min-width: 0; }
    .line1 > span { overflow: hidden; text-overflow: ellipsis; }
    .name { font-weight: 600; flex: none; max-width: 50%; }
    .msg.unread .name::after { content: ""; display: inline-block; width: 6px; height: 6px; border-radius: 50%;
      background: var(--accent); margin: 0 0 2px 7px; vertical-align: middle; }
    .email { color: var(--faint); font-size: 12px; flex: 0 1 auto; }
    .msg.expanded .email:hover { color: var(--accent); }
    .recipients { color: var(--muted); font-size: 12px; margin-top: 1px; overflow: hidden; text-overflow: ellipsis;
      white-space: nowrap; }
    .snippet { color: var(--muted); font-size: 13px; flex: 1 1 auto; min-width: 0; }
    .msg.expanded .snippet, .msg.collapsed .recipients, .msg.collapsed .email { display: none; }
    .meta { flex: none; display: flex; align-items: center; gap: 6px; }
    .date { color: var(--faint); font-size: 12px; white-space: nowrap; font-variant-numeric: tabular-nums; }
    .actions { display: flex; gap: 0; opacity: 0; transform: translateX(4px);
      transition: opacity .16s var(--ease), transform .16s var(--ease); }
    .msg.expanded .msg-head:hover .actions, .msg.expanded .actions:focus-within { opacity: 1; transform: none; }
    .msg.collapsed .actions { display: none; }
    .actions button { padding: 4px 6px; color: var(--muted); }
    .actions button:hover { color: var(--text); }
    /* Folds animate only their row track; the fade runs on the fixed-size content inside, so the
       compositor blends it instead of repainting a layer that resizes every frame. */
    .fold { display: grid; grid-template-rows: 1fr; transition: grid-template-rows .32s var(--ease); }
    .msg.collapsed .fold { grid-template-rows: 0fr; }
    .fold-inner { min-height: 0; overflow: hidden; }
    .fold-content { transition: opacity .24s var(--ease); }
    .msg.collapsed .fold-content { opacity: 0; }
    .msg-body { padding: 4px 0 22px 40px; overflow-wrap: anywhere; }
    .mail-frame { display: block; width: 100%; border: 0; height: 60px; background: transparent; }
    @media (prefers-color-scheme: dark) {
      .mail-frame { background: #fff; border-radius: 10px; padding: 14px 16px; box-sizing: content-box; width: calc(100% - 32px); }
    }
    .plain { white-space: pre-wrap; font-size: 14.5px; line-height: 1.6; }
    .ms-plain { white-space: pre-wrap; }
    .plain a, .ms-plain a { color: var(--accent); }
    blockquote.ms-quote { margin: 6px 0; padding: 0 0 0 12px; border-left: 2px solid var(--line); color: var(--muted); }
    .ms-fold-toggle { display: inline-flex; align-items: center; margin: 8px 0; padding: 0 9px; height: 18px;
      border-radius: 9px; font-size: 12px; letter-spacing: 1px; color: var(--muted); background: var(--chip);
      transition: background-color .15s var(--ease); }
    .ms-fold-toggle:hover { background: var(--line); }
    .ms-fold-toggle.block { display: flex; width: fit-content; }
    .ms-folded { display: none !important; }
    .error { color: #d70015; font-size: 13px; padding: 8px 0; }
    .skeleton i { display: block; height: 9px; border-radius: 5px; margin: 10px 0; background: var(--chip);
      animation: pulse 1.4s ease-in-out infinite; }
    .skeleton i:nth-child(2) { width: 88%; animation-delay: .15s; }
    .skeleton i.short { width: 55%; animation-delay: .3s; }
    @keyframes pulse { 50% { opacity: .4; } }
    .attachments { display: flex; flex-wrap: wrap; gap: 8px; padding: 0 0 22px 40px; margin-top: -8px; }
    .attachments:empty { display: none; }
    .att { display: flex; align-items: center; gap: 10px; padding: 6px 8px 6px 6px; border-radius: 10px;
      border: 1px solid var(--line); max-width: 280px; cursor: default; transition: background-color .15s var(--ease); }
    .att:hover { background: var(--hover); }
    .att .thumb { width: 34px; height: 34px; border-radius: 7px; flex: none; background: var(--chip);
      display: flex; align-items: center; justify-content: center; font-size: 9.5px; font-weight: 700;
      color: var(--muted); overflow: hidden; text-transform: uppercase; }
    .att .thumb img { width: 100%; height: 100%; object-fit: cover; }
    .att .meta-a { min-width: 0; }
    .att .fname { font-size: 12.5px; font-weight: 550; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .att .fsize { font-size: 11px; color: var(--muted); }
    .att .save { margin-left: 2px; padding: 2px 7px; color: var(--muted); }
    /* Chat layout: a flat log, one row per message, no cards or folds. */
    main.chat { border-top: 0; }
    .day { display: flex; align-items: center; gap: 12px; margin: 16px 0 6px; font-size: 11.5px; font-weight: 600;
      color: var(--muted); user-select: none; -webkit-user-select: none; }
    .day::before, .day::after { content: ""; flex: 1; height: 1px; background: var(--line); }
    .msg.chat { position: relative; display: flex; gap: 12px; margin: 0 -10px; padding: 8px 10px 6px;
      border: 0; border-radius: 10px; scroll-margin-top: 14px; transition: background-color .15s var(--ease); }
    .msg.chat.continued { padding-top: 2px; }
    .msg.chat:hover { background: var(--hover); }
    .chat-gutter { flex: none; width: 34px; display: flex; justify-content: center; }
    .chat-time { font-size: 10.5px; line-height: 22px; color: var(--faint); white-space: nowrap;
      font-variant-numeric: tabular-nums; opacity: 0; transition: opacity .15s var(--ease); }
    .msg.chat.continued:hover .chat-time, .msg.chat.continued.unread .chat-time { opacity: 1; }
    .msg.chat.continued.unread .chat-time { color: var(--accent); font-weight: 600; }
    .chat-main { flex: 1; min-width: 0; }
    .chat-head { display: flex; align-items: baseline; gap: 8px; min-width: 0; white-space: nowrap; margin-bottom: 1px; }
    .chat-head .name { max-width: 60%; overflow: hidden; text-overflow: ellipsis; }
    .chat-head .icon.star { align-self: center; }
    .chat-head .recipients { flex: 1 1 auto; min-width: 0; margin: 0; color: var(--faint); }
    .msg.chat .msg-body { padding: 0 0 2px; }
    .msg.chat .plain { font-size: 14px; line-height: 1.55; }
    .msg.chat .attachments { padding: 8px 0 4px; margin: 0; }
    .msg.chat .actions { position: absolute; top: -10px; right: 10px; padding: 1px; border-radius: 8px;
      border: 1px solid var(--line); background: var(--bg); box-shadow: 0 2px 10px rgba(0,0,0,.08); pointer-events: none; }
    .msg.chat:hover .actions, .msg.chat .actions:focus-within { opacity: 1; transform: none; pointer-events: auto; }
    .reply-dock { position: sticky; bottom: 0; margin-top: auto; padding: 28px 0 20px;
      background: linear-gradient(to bottom, transparent, var(--bg) 38%); animation: rise .5s var(--ease) .08s both; }
    /* The pill bar folds away as the composer unfolds in its place, both on the same curve. */
    .fold-y { display: grid; transition: grid-template-rows .38s var(--ease); }
    .fold-clip { min-height: 0; overflow: hidden; padding: 6px 4px 16px; margin: -6px -4px -16px; }
    .bar-fold { grid-template-rows: 1fr; }
    .composer-fold { grid-template-rows: 0fr; pointer-events: none; }
    .reply-dock.composing .bar-fold { grid-template-rows: 0fr; pointer-events: none; }
    .reply-dock.composing .composer-fold { grid-template-rows: 1fr; pointer-events: auto; }
    .dock-bar { display: flex; gap: 6px; align-items: center; transition: opacity .24s var(--ease); }
    .reply-dock.composing .dock-bar { opacity: 0; }
    .reply-pill { flex: 1; display: flex; align-items: center; gap: 9px; height: 38px; padding: 0 16px;
      border-radius: 19px; border: 1px solid var(--line); background: var(--bg); color: var(--muted);
      font-size: 13px; font-weight: 450; text-align: left;
      box-shadow: 0 1px 2px rgba(0,0,0,.04), 0 6px 20px rgba(0,0,0,.06); }
    .reply-pill:hover { background: var(--bg); color: var(--text); border-color: color-mix(in srgb, var(--accent) 40%, var(--line));
      box-shadow: 0 1px 2px rgba(0,0,0,.05), 0 8px 26px rgba(0,0,0,.09); }
    .reply-pill .icon { color: var(--accent); }
    .reply-dock.sent .reply-pill { color: #248a3d; border-color: color-mix(in srgb, #34c759 45%, var(--line)); }
    .reply-dock.sent .reply-pill .icon { color: #34c759; }
    .dock-icon { width: 38px; height: 38px; padding: 0; border-radius: 50%; display: flex; align-items: center;
      justify-content: center; color: var(--muted); border: 1px solid var(--line); background: var(--bg); }
    .dock-icon:hover { color: var(--text); background: var(--bg); border-color: var(--faint); }
    .composer { display: flex; flex-direction: column; max-height: min(62vh, 560px); border-radius: 14px;
      border: 1px solid var(--line); background: var(--bg);
      box-shadow: 0 1px 2px rgba(0,0,0,.04), 0 10px 32px rgba(0,0,0,.09);
      transform: translateY(12px) scale(.985); transform-origin: 50% 100%; opacity: 0;
      transition: transform .4s var(--ease), opacity .24s var(--ease), border-color .15s var(--ease); }
    .reply-dock.composing .composer { transform: none; opacity: 1; }
    .composer:focus-within { border-color: color-mix(in srgb, var(--accent) 45%, var(--line)); }
    .c-head { display: flex; align-items: center; gap: 2px; padding: 7px 6px 0 14px; font-size: 12.5px; color: var(--muted); }
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
      background: transparent; color: var(--text); font: 14px/1.55 -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif; }
    #c-text::placeholder { color: var(--faint); }
    .c-quote { padding: 0 14px 4px; }
    .c-quote .ms-fold-toggle { padding: 0 9px; margin: 2px 0 6px; }
    .c-quote-fold { display: grid; grid-template-rows: 0fr; transition: grid-template-rows .32s var(--ease); }
    .c-quote.open .c-quote-fold { grid-template-rows: 1fr; }
    .c-quote-fold > div { min-height: 0; overflow: hidden; }
    .c-quote-body { padding: 2px 0 8px 12px; border-left: 2px solid var(--line); opacity: 0;
      transition: opacity .22s var(--ease); }
    .c-quote.open .c-quote-body { opacity: 1; }
    .c-quote-attr { font-size: 12px; color: var(--muted); margin-bottom: 6px; }
    .c-foot { display: flex; align-items: center; gap: 10px; padding: 6px 8px 8px 14px; }
    .c-status { flex: 1; min-width: 0; font-size: 12px; color: var(--muted); white-space: nowrap; overflow: hidden;
      text-overflow: ellipsis; }
    .c-status.error { color: #d70015; }
    .c-send { display: flex; align-items: center; gap: 6px; height: 28px; padding: 0 12px; border-radius: 8px;
      background: var(--accent); color: #fff; font-weight: 600; }
    .c-send:hover { background: color-mix(in srgb, var(--accent) 86%, #000); }
    .c-send .icon { width: 13px; height: 13px; }
    .c-send .kbd { font-weight: 500; font-size: 11px; opacity: .72; }
    .composer.sending .c-send { opacity: .55; pointer-events: none; }
    .composer.sending #c-text { opacity: .6; }
    .ms-attached-message { margin-top: 14px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 10px; }
    .ms-attached-headline { font-size: 12px; color: var(--muted); margin-bottom: 8px; }
    @media (prefers-reduced-motion: reduce) { *, *::before { animation: none !important; transition: none !important; } }
    """

    /// Injected into every HTML mail frame.
    static let frameCSS = """
    html, body { height: auto !important; min-height: 0 !important; }
    html { background: #fff; }
    body { margin: 0; padding: 2px 0; color: #1d1d1f; background-color: #fff;
      font: 14px/1.45 -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif; overflow-wrap: break-word; }
    img { max-width: 100%; }
    pre { white-space: pre-wrap; }
    .ms-plain { white-space: pre-wrap; }
    blockquote.ms-quote { margin: 6px 0; padding-left: 12px; border-left: 3px solid #d2d2d7; color: #6e6e73; }
    .ms-fold-toggle { display: inline-block; margin: 8px 0; padding: 0 10px; height: 18px; line-height: 16px;
      border-radius: 9px; font: 13px -apple-system; letter-spacing: 1px; color: #6e6e73; background: #f0f0f2;
      border: 1px solid #e0e0e4; cursor: default; }
    .ms-folded { display: none !important; }
    .ms-fold-toggle.block { display: block; width: fit-content; }
    .ms-attached-message { margin-top: 14px; padding: 10px 12px; border: 1px solid #e0e0e4; border-radius: 8px; }
    .ms-attached-headline { font-size: 12px; color: #6e6e73; margin-bottom: 8px; }
    """

    // MARK: - Script

    static let script = """
    (() => {
      // Messages carry the render they came from, so the app ignores clicks on a thread it replaced.
      let generation = 0;
      const post = (msg) => window.webkit.messageHandlers.ms.postMessage(Object.assign(msg, { generation }));
      const FRAME_CSS = \(jsString(frameCSS));
      // Chat threads fold signatures along with quotes.
      let chatMode = false;
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

      document.addEventListener('click', (event) => {
        const link = event.target.closest('a[href]');
        if (link && !event.target.closest('[data-action]')) {
          event.preventDefault();
          post({ action: 'link', href: link.href });
          return;
        }
        const el = event.target.closest('[data-action]');
        if (!el) return;
        const action = el.dataset.action;
        if (action === 'compose-to' && el.closest('.msg.collapsed')) {
          toggle(Number(el.closest('.msg').id.slice(4)));
          return;
        }
        event.preventDefault();
        event.stopPropagation();
        if (action === 'avatar') {
          const r = el.getBoundingClientRect();
          post({ action, index: Number(el.closest('.msg').id.slice(4)), rect: [r.left, r.top, r.width, r.height] });
          return;
        }
        if (action === 'toggle') {
          if (window.getSelection && String(window.getSelection()).length > 0) return;
          toggle(Number(el.dataset.index));
          return;
        }
        if (action === 'reply-quote') {
          toggleQuote();
          return;
        }
        post({ action, index: el.dataset.index !== undefined ? Number(el.dataset.index) : -1,
               part: el.dataset.part !== undefined ? Number(el.dataset.part) : -1,
               email: el.dataset.email || '', text: composerText() });
      }, true);

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
        const style = doc.createElement('style');
        style.textContent = FRAME_CSS;
        (doc.head || doc.documentElement).appendChild(style);
        if (doc.body) foldQuotes(doc, doc.body, () => fit(frame));
        doc.addEventListener('click', (event) => {
          const link = event.target.closest('a[href]');
          if (link) {
            event.preventDefault();
            post({ action: 'link', href: link.href });
          }
        }, true);
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

      function escapeHTML(text) {
        return String(text).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
      }

      // MARK: Inline reply

      const $ = (id) => document.getElementById(id);
      let sentTimer = 0;

      function composerText() {
        const text = $('c-text');
        return text ? text.value : '';
      }

      // Every keystroke reaches the app, so Escape and leaving the conversation never lose text.
      function reportReply() {
        const text = $('c-text');
        if (text) post({ action: 'reply-state', text: text.value, focused: document.activeElement === text });
      }

      function growText() {
        const text = $('c-text');
        text.style.height = 'auto';
        text.style.height = Math.max(68, text.scrollHeight) + 'px';
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
        growText();
        $('c-quote-wrap').classList.remove('open');
        setStatus('idle', '');
      }

      // The composer is rebuilt with every thread, so its listeners are bound per render.
      function bindComposer() {
        const text = $('c-text');
        text.addEventListener('input', () => { growText(); reportReply(); });
        text.addEventListener('focus', reportReply);
        text.addEventListener('blur', reportReply);
        text.addEventListener('keydown', (event) => {
          if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            post({ action: 'reply-send', text: text.value });
          }
        });
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

      window.MS = {
        // Replaces the whole column, so bodies, frames, folds, the composer and the banner all
        // start fresh, scrolled to the top (a chat thread: to its focus message).
        render(state) {
          generation = state.generation;
          chatMode = !!state.chat;
          clearTimeout(sentTimer);
          document.documentElement.style.setProperty('--accent', state.accent);
          $('column').innerHTML = state.html;
          bindComposer();
          unfollow();
          window.scrollTo(0, 0);
          if (chatMode && state.focus > 0) follow(state.focus);
        },
        clear(next) {
          generation = next;
          clearTimeout(sentTimer);
          unfollow();
          $('column').innerHTML = '';
          window.scrollTo(0, 0);
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
          const list = document.getElementById('att-' + index);
          list.innerHTML = '';
          for (const att of payload.attachments || []) {
            const item = document.createElement('div');
            item.className = 'att';
            item.dataset.action = 'attachment';
            item.dataset.index = String(index);
            item.dataset.part = String(att.part);
            item.title = att.name;
            const thumb = att.thumb ? '<img src="' + escapeHTML(att.thumb) + '">' : escapeHTML(att.ext || 'file');
            item.innerHTML = '<div class="thumb">' + thumb + '</div><div class="meta-a"><div class="fname">'
              + escapeHTML(att.name) + '</div><div class="fsize">' + escapeHTML(att.size) + '</div></div>'
              + '<button class="save" title="Save…" data-action="save-attachment" data-index="' + index
              + '" data-part="' + att.part + '">↓</button>';
            list.appendChild(item);
          }
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
          growText();
          setStatus(options.sending ? 'sending' : 'idle', options.sending ? 'Sending…' : '');
          if (options.focus) {
            setTimeout(() => {
              text.focus();
              text.setSelectionRange(text.value.length, text.value.length);
            }, 80);
          }
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
      };
    })();
    """

    static func jsString(_ text: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [text], options: [])) ?? Data("[\"\"]".utf8)
        let array = String(decoding: data, as: UTF8.self)
        return String(array.dropFirst().dropLast())
    }
}

extension NSColor {
    var cssHex: String {
        guard let rgb = usingColorSpace(.sRGB) else { return "#888" }
        return String(format: "#%02x%02x%02x", Int(rgb.redComponent * 255), Int(rgb.greenComponent * 255),
                      Int(rgb.blueComponent * 255))
    }
}
