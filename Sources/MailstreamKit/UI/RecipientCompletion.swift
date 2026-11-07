import AppKit

/// The delegate of recipient token fields (the compose window's To, Cc and Bcc, the New Chat
/// sheet's people): completes to `contacts`, shows each token by name and edits it as
/// `Name <address>`. Tokens are RFC 5322 address strings.
@MainActor
final class RecipientCompletion: NSObject, NSTokenFieldDelegate {
    /// People to complete to, best first.
    var contacts: [Contact] = []
    /// Addresses already chosen, which completion leaves out.
    var taken: () -> Set<String> = { [] }
    /// The user typed in one of the fields.
    var onEdit: (NSTokenField) -> Void = { _ in }
    /// Completion and editing labels (`Name <address>`) to the token they stand for.
    private var labels: [String: String] = [:]

    /// Styles `field` as a borderless recipient line and makes this its delegate.
    func attach(_ field: NSTokenField) {
        field.delegate = self
        field.tokenizingCharacterSet = CharacterSet(charactersIn: ",;")
        field.tokenStyle = .rounded
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = Design.font(.body)
        field.placeholderString = ""
        field.completionDelay = 0.05
    }

    /// The field's tokens, trimmed, without empty ones.
    static func tokens(in field: NSTokenField) -> [String] {
        (field.objectValue as? [Any] ?? []).compactMap { ($0 as? String)?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// People whose name, a word of it, or address starts with `typed`, best first, without the
    /// ones already chosen.
    func suggestions(for typed: String) -> [Contact] {
        Contact.suggestions(for: typed, in: contacts, taken: taken())
    }

    func controlTextDidChange(_ obj: Notification) {
        if let field = obj.object as? NSTokenField { onEdit(field) }
    }

    func tokenField(_ tokenField: NSTokenField, completionsForSubstring substring: String, indexOfToken tokenIndex: Int,
                    indexOfSelectedItem selectedIndex: UnsafeMutablePointer<Int>?) -> [Any]? {
        let shown = suggestions(for: substring).map { contact in
            let label = Contact.label(contact.address)
            labels[label] = contact.address.rfc5322
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
        labels[editingString] ?? editingString
    }

    /// A comma, Return or Tab after a partial name commits the typed text, not the completion
    /// shown: it becomes the best match. An address typed out in full is kept as typed.
    func tokenField(_ tokenField: NSTokenField, shouldAdd tokens: [Any], at index: Int) -> [Any] {
        tokens.map { token in
            guard let typed = token as? String, Address.parse(typed) == nil,
                  let best = suggestions(for: typed).first else { return token }
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
        labels[label] = raw
        return label
    }
}
