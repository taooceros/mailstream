import AppKit
import Observation

/// What a compose window opens with. `text` starts the body, above any quote (an inline reply moved
/// into a window keeps its text, and its `plan` when one was already worked out).
struct ComposeRequest: Identifiable {
    let id = UUID()
    var mode: ComposeMode
    var account: String
    var source: MailMessage?
    var replyAll = false
    var to: [Address] = []
    var text = ""
    var plan: ReplyPlan?
}

/// A field focus can be moved to.
enum ComposeField {
    case to, cc, bcc, body
}

/// A file going out with the message, with what its chip shows.
struct ComposeAttachment: Identifiable {
    let url: URL
    /// Finder's icon for the file; only `NSWorkspace` provides it.
    let icon: NSImage
    let size: String?

    var id: URL { url }

    init(_ url: URL) {
        self.url = url
        icon = NSWorkspace.shared.icon(forFile: url.path)
        size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize)
            .map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file) }
    }
}

/// A warning shown over the window.
struct ComposeAlert {
    let title: String
    let detail: String
}

/// New message, reply and forward. Reply and forward prefill the recipients, subject and quoted
/// original so they are visible and editable; what the window shows is what is sent. The body takes
/// typing at once: the quote is appended under it when the original has loaded.
@Observable
final class ComposeModel {
    let request: ComposeRequest
    @ObservationIgnored let store: MailStore

    var account: String
    /// Recipient tokens as the fields hold them (`Name <address>` or a typed string), trimmed.
    var to: [String]
    var cc: [String] = []
    var bcc: [String] = []
    /// Whether the Bcc row shows; Bcc counts only then.
    var showsBcc = false
    var subject = ""
    var attachments: [ComposeAttachment] = []
    var sending = false
    var alert: ComposeAlert?
    var confirmingDiscard = false
    var choosingFiles = false

    /// Whether anything was written, which closing then asks about.
    @ObservationIgnored var edited = false
    /// Whether the user changed To or Cc, which a reply's worked-out recipients then leave alone.
    @ObservationIgnored private var recipientsEdited = false
    /// Loading the original for the quote (and a reply's recipients); Send waits for it.
    @ObservationIgnored private var prefillTask: Task<Void, Never>?
    /// People to complete recipients to, best first; filled just after the window opens.
    @ObservationIgnored private var contacts: [Contact] = []
    /// Completion and editing labels (`Name <address>`) to the token they stand for.
    @ObservationIgnored var labels: [String: String] = [:]
    /// The body until its text view exists (and again if the view is torn down); the view holds it
    /// while it is shown.
    @ObservationIgnored var pendingBody: String
    /// Where the caret starts: after the text the window opened with, above the quote.
    @ObservationIgnored let initialCaret: Int
    /// Closes the window without asking; the host sets it.
    @ObservationIgnored var close: () -> Void = {}

    // The AppKit fields, for focus, committing a token being typed and the body's text.
    @ObservationIgnored private weak var toView: NSTokenField?
    @ObservationIgnored private weak var ccView: NSTokenField?
    @ObservationIgnored private weak var bccView: NSTokenField?
    @ObservationIgnored private(set) weak var bodyView: NSTextView?
    /// Focus asked for before its field was in the window.
    @ObservationIgnored private var pendingFocus: ComposeField?

    init(store: MailStore, request: ComposeRequest) {
        self.store = store
        self.request = request
        account = request.account
        to = request.to.map(\.rfc5322)
        pendingBody = request.text
        initialCaret = (request.text as NSString).length
        prefill()
        Task { contacts = await store.contacts() }
    }

    var title: String {
        switch request.mode {
        case .new: "New Message"
        case .reply: request.replyAll ? "Reply All" : "Reply"
        case .forward: "Forward"
        }
    }

    var subtitle: String { request.source?.envelope.subject ?? "" }

    /// Only a new message picks its account; a reply or forward goes out from the source's.
    var choosesAccount: Bool {
        if case .new = request.mode { true } else { false }
    }

    func recipients(_ field: ComposeField) -> [String] {
        switch field {
        case .to: to
        case .cc: cc
        case .bcc: bcc
        case .body: []
        }
    }

    func setRecipients(_ tokens: [String], in field: ComposeField) {
        guard recipients(field) != tokens else { return }
        switch field {
        case .to: to = tokens
        case .cc: cc = tokens
        case .bcc: bcc = tokens
        case .body: break
        }
    }

    /// The user typed in a recipient field.
    func recipientsTyped(_ tokens: [String], in field: ComposeField) {
        edited = true
        recipientsEdited = true
        setRecipients(tokens, in: field)
    }

    /// A token field's recipients, trimmed, without empty tokens.
    static func tokens(in field: NSTokenField) -> [String] {
        (field.objectValue as? [Any] ?? []).compactMap { ($0 as? String)?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    var bodyText: String { bodyView?.string ?? pendingBody }

    // MARK: - Fields and focus

    func attach(_ view: NSView, as field: ComposeField) {
        switch field {
        case .to: toView = view as? NSTokenField
        case .cc: ccView = view as? NSTokenField
        case .bcc: bccView = view as? NSTokenField
        case .body: bodyView = view as? NSTextView
        }
    }

    private func view(_ field: ComposeField) -> NSView? {
        switch field {
        case .to: toView
        case .cc: ccView
        case .bcc: bccView
        case .body: bodyView
        }
    }

    /// Makes `field` the first responder, or does so as soon as it is in the window.
    func focus(_ field: ComposeField) {
        if let view = view(field), let window = view.window {
            pendingFocus = nil
            window.makeFirstResponder(view)
        } else {
            pendingFocus = field
        }
    }

    /// `field`'s view joined the window: it takes focus if it was asked for.
    func moved(toWindow field: ComposeField) {
        guard pendingFocus == field, let view = view(field), let window = view.window else { return }
        pendingFocus = nil
        window.makeFirstResponder(view)
    }

    func toggleBcc() {
        showsBcc.toggle()
        if showsBcc { focus(.bcc) }
    }

    // MARK: - Prefill

    /// Fills what is known at once (subject, a reply's recipients from what is loaded) and appends
    /// the quote when the original has loaded, leaving the user's typing and caret alone.
    private func prefill() {
        edited = !request.text.isEmpty
        guard let source = request.source else { return }
        var fillsRecipients = false
        switch request.mode {
        case .new:
            return
        case .reply:
            subject = request.plan?.subject ?? Subject.reply(to: source.envelope.subject)
            if to.isEmpty {
                fillsRecipients = true
                let recipients = request.plan.map { ($0.to, $0.cc) }
                    ?? MailStore.replyRecipients(for: source, content: store.contents.loaded(source.ref)?.content,
                                                 own: store.ownAddresses, all: request.replyAll)
                to = recipients.0.map(\.rfc5322)
                cc = recipients.1.map(\.rfc5322)
            }
        case .forward:
            subject = Subject.forward(source.envelope.subject)
        }
        prefillTask = Task {
            do {
                let quote: String
                if case .reply = request.mode {
                    let plan: ReplyPlan
                    if let ready = request.plan { plan = ready } else { plan = try await store.replyPlan(for: source, all: request.replyAll) }
                    if fillsRecipients, !recipientsEdited {
                        to = plan.to.map(\.rfc5322)
                        cc = plan.cc.map(\.rfc5322)
                    }
                    quote = plan.quotedText.hasPrefix(">") ? plan.attribution + "\n" + plan.quotedText : plan.quotedText
                } else {
                    quote = try await store.forwardDraft(for: source).body
                }
                guard !quote.isEmpty else { return }
                appendQuote(quote)
            } catch {
                presentError("Could not load the original message", error: error)
            }
        }
    }

    /// Appended past everything typed, so the selection and undo history stay valid.
    private func appendQuote(_ quote: String) {
        let text = "\n\n" + quote
        guard let bodyView else {
            pendingBody += text
            return
        }
        let selection = bodyView.selectedRanges
        bodyView.textStorage?.append(NSAttributedString(string: text, attributes: bodyView.typingAttributes))
        bodyView.selectedRanges = selection
    }

    // MARK: - Attachments

    func add(_ urls: [URL]) {
        guard !sending else { return }
        attachments += urls.filter { url in !attachments.contains { $0.url == url } }.map(ComposeAttachment.init)
        edited = true
    }

    func remove(_ attachment: ComposeAttachment) {
        attachments.removeAll { $0.id == attachment.id }
    }

    // MARK: - Sending

    func send() {
        guard !sending else { return }
        commitEditing()
        sending = true
        Task {
            await prefillTask?.value
            let fields: [ComposeField] = showsBcc ? [.to, .cc, .bcc] : [.to, .cc]
            if let field = fields.first(where: { recipients($0).contains { Address.parse($0) == nil } }) {
                stopSending("Invalid address: \(recipients(field).filter { Address.parse($0) == nil }.joined(separator: ", "))",
                            error: nil, focus: field)
                return
            }
            // Someone listed twice gets one copy, in the first field naming them.
            var seen = Set<String>()
            let lists = fields.map { field in
                recipients(field).compactMap(Address.parse).filter { seen.insert($0.normalizedEmail).inserted }
            }
            guard !seen.isEmpty else {
                stopSending("Add at least one recipient", error: nil, focus: .to)
                return
            }
            do {
                let message = await store.outgoing(account: account, source: request.source, to: lists[0], cc: lists[1],
                                                   bcc: lists.count > 2 ? lists[2] : [], subject: subject,
                                                   text: bodyText, attachments: attachments.map(\.url))
                try await store.send(message, account: account)
                edited = false
                close()
            } catch {
                stopSending("The message could not be sent", error: error, focus: nil)
            }
        }
    }

    /// A recipient still being typed becomes a token first.
    private func commitEditing() {
        guard let window = [toView, ccView, bccView, bodyView].lazy.compactMap({ $0?.window }).first else { return }
        if let editor = window.firstResponder as? NSText, editor.isFieldEditor { window.makeFirstResponder(nil) }
        for (field, view) in [(ComposeField.to, toView), (.cc, ccView), (.bcc, bccView)] {
            if let view { setRecipients(Self.tokens(in: view), in: field) }
        }
    }

    private func stopSending(_ message: String, error: (any Error)?, focus field: ComposeField?) {
        sending = false
        if let field { focus(field) }
        presentError(message, error: error)
    }

    private func presentError(_ message: String, error: (any Error)?) {
        alert = ComposeAlert(title: message, detail: error?.localizedDescription ?? "")
    }

    // MARK: - Recipient completion

    /// Addresses already in a recipient field, which completion leaves out.
    private func takenAddresses() -> Set<String> {
        Set((to + cc + bcc).compactMap { Address.parse($0)?.normalizedEmail })
    }

    /// People whose name, a word of it, or address starts with `typed`, best first; an address
    /// typed out in full first of all.
    func suggestions(for typed: String) -> [Contact] {
        let needle = Contact.fold(typed.trimmingCharacters(in: .whitespaces))
        guard !needle.isEmpty else { return [] }
        let taken = takenAddresses()
        var matches = Array(contacts.lazy.filter { !taken.contains($0.address.normalizedEmail) && $0.matches(needle) }.prefix(12))
        if let exact = matches.firstIndex(where: { Contact.fold($0.address.email) == needle }) {
            matches.insert(matches.remove(at: exact), at: 0)
        }
        return matches
    }

    // MARK: - Closing

    /// Whether the window may close now; otherwise it asks before throwing away what was written.
    func shouldClose() -> Bool {
        guard edited, !sending else { return true }
        confirmingDiscard = true
        return false
    }

    func discard() {
        edited = false
        // On the next turn, once the dialog's sheet has ended.
        Task { close() }
    }

    func closed() {
        prefillTask?.cancel()
    }

    var discardTitle: String {
        let kind = switch request.mode {
        case .new: "message"
        case .reply: "reply"
        case .forward: "forward"
        }
        return "Discard this \(kind)?"
    }

    var discardDetail: String {
        var summary: [String] = []
        let people = (to + cc + bcc).compactMap(Address.parse)
        if let first = people.first {
            let others = people.count - 1
            summary.append("To \(first.displayName)" + (others == 0 ? "" : " and \(others) other\(others == 1 ? "" : "s")"))
        }
        if !attachments.isEmpty { summary.append("\(attachments.count) attachment\(attachments.count == 1 ? "" : "s")") }
        return (summary.isEmpty ? "" : summary.joined(separator: " · ") + "\n\n")
            + "It isn't kept as a draft, so it can't be brought back."
    }

    /// The discard dialog's red trash. SwiftUI's `Image` has no palette colors of its own, so the
    /// symbol is configured as an `NSImage`.
    var discardIcon: NSImage? {
        NSImage(systemSymbolName: "trash.circle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 56, weight: .regular)
                .applying(NSImage.SymbolConfiguration(paletteColors: [.white, Design.Color.danger.color])))
    }
}
