import AppKit

extension UndoManager: MoveUndo {
    func register(_ name: String, on store: MailStore, undo: @escaping (MailStore) -> Void) {
        registerUndo(withTarget: store) { store in undo(store) }
        setActionName(name)
    }
}
