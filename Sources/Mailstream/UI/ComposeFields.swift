import AppKit
import SwiftUI

// The compose window's two AppKit fields. SwiftUI has no token field, and `TextEditor` cannot set
// inline predictions, spell checking, quote and dash substitution, the text inset or the font.

/// What a `RecipientField` edits: the compose window's To, Cc and Bcc, or the New Chat sheet's
/// people.
protocol RecipientSource: AnyObject {
    /// Completion and editing labels (`Name <address>`) to the token they stand for.
    var labels: [String: String] { get set }
    func recipients(_ field: ComposeField) -> [String]
    func setRecipients(_ tokens: [String], in field: ComposeField)
    /// The user typed in the field.
    func recipientsTyped(_ tokens: [String], in field: ComposeField)
    /// People whose name, a word of it, or address starts with `typed`, best first.
    func suggestions(for typed: String) -> [Contact]
    func attach(_ view: NSView, as field: ComposeField)
    /// `field`'s view joined its window, which is when focus asked for earlier can move to it.
    func moved(toWindow field: ComposeField)
}

/// A recipient field: an `NSTokenField` completing to the user's contacts. SwiftUI has no token
/// field with a completion menu.
struct RecipientField: NSViewRepresentable {
    let model: any RecipientSource
    let field: ComposeField

    func makeCoordinator() -> Coordinator { Coordinator(model: model, field: field) }

    func makeNSView(context: Context) -> RecipientTokenField {
        let view = RecipientTokenField()
        view.delegate = context.coordinator
        view.tokenizingCharacterSet = CharacterSet(charactersIn: ",;")
        view.tokenStyle = .rounded
        view.isBordered = false
        view.drawsBackground = false
        view.focusRingType = .none
        view.font = Design.font(.body)
        view.placeholderString = ""
        view.completionDelay = 0.05
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.objectValue = model.recipients(field)
        view.movedToWindow = { [model, field] in model.moved(toWindow: field) }
        model.attach(view, as: field)
        return view
    }

    func updateNSView(_ view: RecipientTokenField, context: Context) {
        let tokens = model.recipients(field)
        if ComposeModel.tokens(in: view) != tokens { view.objectValue = tokens }
        view.isEnabled = context.environment.isEnabled
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView view: RecipientTokenField, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? view.intrinsicContentSize.width, height: view.intrinsicContentSize.height)
    }

    final class Coordinator: NSObject, NSTokenFieldDelegate {
        let model: any RecipientSource
        let field: ComposeField

        init(model: any RecipientSource, field: ComposeField) {
            self.model = model
            self.field = field
        }

        func controlTextDidChange(_ obj: Notification) {
            guard let view = obj.object as? NSTokenField else { return }
            model.recipientsTyped(ComposeModel.tokens(in: view), in: field)
        }

        func controlTextDidEndEditing(_ obj: Notification) {
            guard let view = obj.object as? NSTokenField else { return }
            model.setRecipients(ComposeModel.tokens(in: view), in: field)
        }

        func tokenField(_ tokenField: NSTokenField, completionsForSubstring substring: String, indexOfToken tokenIndex: Int,
                        indexOfSelectedItem selectedIndex: UnsafeMutablePointer<Int>?) -> [Any]? {
            let shown = model.suggestions(for: substring).map { contact in
                let label = Contact.label(contact.address)
                model.labels[label] = contact.address.rfc5322
                return label
            }
            // The token field writes the selected completion over the typed text, which garbles it
            // unless the completion starts with that text; otherwise the list shows with nothing
            // selected, and committing the typed text still picks the first (`shouldAdd`).
            let needle = Contact.fold(substring.trimmingCharacters(in: .whitespaces))
            if let first = shown.first, !Contact.fold(first).hasPrefix(needle) { selectedIndex?.pointee = -1 }
            return shown
        }

        func tokenField(_ tokenField: NSTokenField, representedObjectForEditing editingString: String) -> Any? {
            model.labels[editingString] ?? editingString
        }

        /// A comma, Return or Tab after a partial name commits the typed text, not the completion
        /// shown: it becomes the best match. An address typed out in full is kept as typed.
        func tokenField(_ tokenField: NSTokenField, shouldAdd tokens: [Any], at index: Int) -> [Any] {
            tokens.map { token in
                guard let typed = token as? String, Address.parse(typed) == nil,
                      let best = model.suggestions(for: typed).first else { return token }
                return best.address.rfc5322
            }
        }

        func tokenField(_ tokenField: NSTokenField, displayStringForRepresentedObject representedObject: Any) -> String? {
            guard let raw = representedObject as? String, let address = Address.parse(raw) else { return nil }
            return address.displayName
        }

        /// `Name <address>` without the quotes and commas of the stored form, so editing a token and
        /// committing it again gives the same recipient.
        func tokenField(_ tokenField: NSTokenField, editingStringForRepresentedObject representedObject: Any) -> String? {
            guard let raw = representedObject as? String, let address = Address.parse(raw) else { return representedObject as? String }
            let label = Contact.label(address)
            model.labels[label] = raw
            return label
        }
    }
}

/// Takes focus asked for before it joined the window.
final class RecipientTokenField: NSTokenField {
    var movedToWindow: (() -> Void)?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil { movedToWindow?() }
    }
}

/// The message body: an `NSTextView`, for the input settings `TextEditor` lacks.
struct ComposeBody: NSViewRepresentable {
    let model: ComposeModel

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    func makeNSView(context: Context) -> NSScrollView {
        let bodyView = ComposeTextView()
        bodyView.isRichText = false
        bodyView.allowsUndo = true
        bodyView.font = Design.font(.reading)
        // Text lines up with the header's labels (the container pads 5 pt).
        bodyView.textContainerInset = NSSize(width: ComposeView.margin - 5, height: 14)
        bodyView.isAutomaticQuoteSubstitutionEnabled = false
        bodyView.isAutomaticDashSubstitutionEnabled = false
        bodyView.isContinuousSpellCheckingEnabled = true
        // The system's grey inline predictions, whatever the user's keyboard setting; Tab takes one.
        bodyView.inlinePredictionType = .yes
        bodyView.isVerticallyResizable = true
        bodyView.isHorizontallyResizable = false
        bodyView.autoresizingMask = [.width]
        bodyView.textContainer?.widthTracksTextView = true
        bodyView.delegate = context.coordinator
        bodyView.onFiles = { [model] urls in model.add(urls) }
        bodyView.movedToWindow = { [model] in model.moved(toWindow: .body) }
        let scroll = NSScrollView()
        scroll.documentView = bodyView
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        bodyView.drawsBackground = false

        bodyView.string = model.pendingBody
        bodyView.setSelectedRange(NSRange(location: min(model.initialCaret, (model.pendingBody as NSString).length), length: 0))
        model.attach(bodyView, as: .body)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        (scroll.documentView as? NSTextView)?.isEditable = context.environment.isEnabled
    }

    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        if let bodyView = scroll.documentView as? NSTextView { coordinator.model.pendingBody = bodyView.string }
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        let model: ComposeModel

        init(model: ComposeModel) { self.model = model }

        func textDidChange(_ notification: Notification) { model.edited = true }
    }
}

/// The message body: files dropped or pasted on it attach instead of inserting their paths.
final class ComposeTextView: NSTextView {
    var onFiles: (([URL]) -> Void)?
    var movedToWindow: (() -> Void)?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil { movedToWindow?() }
    }

    private func files(on pasteboard: NSPasteboard) -> [URL] {
        pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    }

    override var acceptableDragTypes: [NSPasteboard.PasteboardType] { super.acceptableDragTypes + [.fileURL] }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        files(on: sender.draggingPasteboard).isEmpty ? super.draggingEntered(sender) : .copy
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        files(on: sender.draggingPasteboard).isEmpty ? super.draggingUpdated(sender) : .copy
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        let urls = files(on: sender.draggingPasteboard)
        guard !urls.isEmpty else { return super.performDragOperation(sender) }
        onFiles?(urls)
        return true
    }

    override func paste(_ sender: Any?) {
        let urls = files(on: .general)
        guard !urls.isEmpty else { return super.paste(sender) }
        onFiles?(urls)
    }
}
