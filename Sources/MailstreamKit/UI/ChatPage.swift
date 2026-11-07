import AppKit

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
        // The space opens on its first unread message, under a "New" line, starting a run.
        let firstUnread = entries.first { unread.contains($0.id) }?.id
        let runs = ReaderPage.chatRuns(entries, date: { $0.created }, sender: { $0.senderID },
                                       opensRun: { $0.id == firstUnread })
        var items: [Item] = []
        items.reserveCapacity(entries.count)
        for (entry, run) in zip(entries, runs) {
            var html = "<div class=\"chat-item\" id=\"\(entry.id)\">" + run.divider
            if entry.id == firstUnread { html += "<div class=\"day new\"><span>New</span></div>" }
            html += article(entry, continued: run.continued, unread: unread.contains(entry.id), input: input, starters: starters)
            html += "</div>"
            items.append(Item(id: entry.id, html: html))
        }
        return Input.Output(items: items, firstUnread: firstUnread)
    }

    private static func article(_ entry: Entry, continued: Bool, unread: Bool, input: Input,
                                starters: [String: ChatMessage]) -> String {
        let time = entry.created.formatted(.dateTime.hour().minute())
        let message = entry.message
        var head: ReaderPage.ChatHead?
        if !continued {
            let photo = input.avatars[entry.senderKey]
            // Only pictures AvatarStore has (by address) can be enlarged.
            let avatar = ReaderPage.avatar(key: entry.senderKey, photo: photo, initials: entry.initials,
                                           enlarges: entry.hasEmail && photo?.hasPrefix("data:") == true)
            head = ReaderPage.ChatHead(avatar: avatar, name: entry.label)
        }
        let status = entry.pending.map { $0.failure == nil ? "Sending…" : "Not sent" }
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
                + "data-thread=\"\(HTMLText.escape(thread))\" data-who=\"\(HTMLText.escape(who))\">\(ReaderPage.Icon.reply)</button></div>"
        }
        var classes: [String] = []
        if unread { classes.append("unread") }
        if entry.mine { classes.append("mine") }
        if message?.threadReply == true { classes.append("threaded") }
        if let pending = entry.pending { classes.append(pending.failure == nil ? "pending" : "failed") }
        // A pending message's head says how sending goes; continued, it shows no time until sent.
        return ReaderPage.chatMessage(classes: classes, style: "--avatar:\(Avatar.color(for: entry.senderKey).cssRGBA)",
                                      head: head, time: continued ? (entry.pending == nil ? time : "") : status ?? time,
                                      full: entry.created.formatted(date: .complete, time: .shortened),
                                      main: body, actions: actions)
    }

    /// A link-only attachment, opened in the browser; one without a web link shows disabled.
    private static func attachment(_ attachment: ChatAttachment) -> String {
        let subtype = attachment.contentType?.split(separator: "/").last.map(String.init)
        let label = ReaderPage.attachmentLabel(pathExtension: (attachment.name as NSString).pathExtension,
                                               fallback: subtype ?? "file")
        let detail = attachment.contentType ?? ""
        guard let url = attachment.url, let scheme = URL(string: url)?.scheme?.lowercased(), ["http", "https"].contains(scheme) else {
            return ReaderPage.attachmentTile(name: attachment.name, detail: detail, label: label, disabled: true)
        }
        return ReaderPage.attachmentTile(name: attachment.name, detail: detail, label: label,
                                         attributes: " data-action=\"chat-open\" data-url=\"\(HTMLText.escape(url))\"")
    }

    /// A DOM id for a resource name (`spaces/x/messages/y`) or pending id.
    static func domID(_ name: String) -> String {
        "cm-" + String(name.map { $0.isASCII && ($0.isLetter || $0.isNumber) ? $0 : "-" })
    }

    // MARK: - Page chrome

    /// Header, an empty log and the composer; the log fills in through `MS.chat.render`/`MS.chat.update`.
    static func page(title: String, kind: ChatSpaceKind, memberCount: Int, accountLabel: String, accountColor: String) -> String {
        var meta = [ReaderPage.meta(kind.title, tint: nil), ReaderPage.meta(accountLabel, tint: accountColor, mark: "bar")]
        if kind == .group, memberCount > 0 { meta.append(ReaderPage.meta("\(memberCount + 1) people", tint: nil)) }
        let composer = ReaderPage.composer(
            head: """
            <span class="c-mode">\(ReaderPage.Icon.reply)</span>
            <div class="c-to" id="c-thread-label"></div>
            <button class="c-btn" title="Cancel Thread Reply" data-action="chat-unthread">✕</button>
            """,
            headAttributes: " id=\"c-thread\" hidden", placeholder: "Message \(title)",
            status: "Return to send · Shift-Return for a new line", send: "chat-send", sendKey: "↩")
        return ReaderPage.threadHeader(title: title, meta: meta) + """
        <main id="thread" class="chat chat-space">
          <div id="chat-earlier" class="chat-earlier" hidden><button data-action="chat-earlier">Load earlier messages</button></div>
          <div id="chat-log" class="chat-log"></div>
          <div id="chat-status" class="chat-status"></div>
        </main>
        <footer class="reply-dock composing chat-dock" id="dock">
          \(composer)
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

    /// The chat space page, run after `WebShell`'s script, whose `Shell` it uses: `MS.chat.render`
    /// puts a space in the column, `MS.chat.update` applies the log's changes.
    static let script = """
    (() => {
      const { post, $, escapeHTML, show, autosize, bindComposer, focusAtEnd } = Shell;
      // The message field never gets shorter than this.
      const TEXT_MIN = 40;
      // The thread the next message replies in, set from a message's reply button.
      let thread = null, who = null, positioned = false;

      const space = {
        act(el, action) {
          if (action === 'chat-reply') {
            setThread(el.dataset.thread, el.dataset.who);
            $('c-text').focus();
            report();
          } else if (action === 'chat-unthread') {
            setThread(null, null);
            report();
          } else if (action === 'chat-send') {
            send();
          } else {
            post({ action, id: el.dataset.id || '', url: el.dataset.url || '' });
          }
        },
      };

      function setThread(next, name) {
        thread = next || null;
        who = name || null;
        const head = $('c-thread');
        if (!head) return;
        head.hidden = !thread;
        $('c-thread-label').innerHTML = thread
          ? '<span class="c-label">Reply in thread to</span><b>' + escapeHTML(who || '') + '</b>' : '';
      }

      // Every keystroke reaches the app, so leaving the space keeps the draft.
      function report() {
        const text = $('c-text');
        if (text) post({ action: 'chat-state', text: text.value, thread: thread || '', who: who || '',
                         focused: document.activeElement === text });
      }

      function send() {
        const text = $('c-text');
        if (!text.value.trim()) return;
        post({ action: 'chat-send', text: text.value, thread: thread || '' });
        text.value = '';
        autosize(text, TEXT_MIN);
        setThread(null, null);
        report();
      }

      function fragment(html) {
        const template = document.createElement('template');
        template.innerHTML = html;
        template.content.querySelectorAll('.msg').forEach((msg) => msg.classList.add('static'));
        return template.content;
      }

      function nearBottom() {
        return window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 80;
      }

      function toBottom(smooth) {
        window.scrollTo({ top: document.documentElement.scrollHeight, behavior: smooth ? 'smooth' : 'auto' });
      }

      // A space opens on its first unread message, else at its end.
      function position(focus) {
        const target = focus ? document.getElementById(focus) : null;
        if (target) target.scrollIntoView({ block: 'center' }); else toBottom(false);
        positioned = true;
      }

      function chrome(state) {
        const earlier = $('chat-earlier');
        earlier.hidden = !state.earlier;
        const button = earlier.querySelector('button');
        button.disabled = !!state.loadingEarlier;
        button.textContent = state.loadingEarlier ? 'Loading…' : 'Load earlier messages';
        $('chat-status').innerHTML = state.status || '';
      }

      MS.chat = {
        render(state) {
          show(space, state);
          $('chat-log').innerHTML = state.log;
          chrome(state);
          const text = $('c-text');
          // Return sends, Shift-Return breaks the line; an input method's Return only commits its text.
          bindComposer(text, TEXT_MIN, {
            report,
            isSend: (event) => !event.shiftKey && !event.altKey && !event.isComposing && event.keyCode !== 229,
            send,
          });
          text.value = state.draft.text || '';
          autosize(text, TEXT_MIN);
          setThread(state.draft.thread, state.draft.who);
          positioned = false;
          if (state.log) position(state.focus);
          focusAtEnd(text);
        },
        // New messages append (and slide in); changed ones are replaced in place; anything else
        // replaces the log, keeping what is on screen where it was.
        update(update) {
          const log = $('chat-log');
          if (!log) return;
          const bottom = nearBottom();
          const fromBottom = document.documentElement.scrollHeight - window.scrollY;
          for (const id of update.remove || []) {
            const el = document.getElementById(id);
            if (el) el.remove();
          }
          for (const [id, html] of update.replace || []) {
            const el = document.getElementById(id);
            if (el) el.replaceWith(fragment(html));
          }
          const replaced = update.log !== undefined && update.log !== null;
          if (replaced) {
            log.innerHTML = '';
            log.appendChild(fragment(update.log));
          }
          let mine = false;
          for (const html of update.append || []) {
            log.insertAdjacentHTML('beforeend', html);
            if (log.lastElementChild && log.lastElementChild.querySelector('.msg.mine')) mine = true;
          }
          chrome(update);
          if (!positioned) {
            if (log.childElementCount) position(update.focus);
            return;
          }
          if ((update.append || []).length && (bottom || mine)) toBottom(true);
          else if (replaced && !bottom) window.scrollTo(0, document.documentElement.scrollHeight - fromBottom);
          else if (bottom) toBottom(false);
        },
        blur() { const text = $('c-text'); if (text) text.blur(); },
      };
    })();
    """
}
