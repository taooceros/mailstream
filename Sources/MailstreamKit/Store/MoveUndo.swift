/// Where `MailStore`'s moves (archive, trash, back to the inbox) register their Undo: the native
/// window's `UndoManager` (`UI/MailStore+Undo.swift`). Front ends over FFI pass none.
protocol MoveUndo: AnyObject {
    /// Registers `undo`, run on `store` when the person chooses Undo `name`.
    func register(_ name: String, on store: MailStore, undo: @escaping (MailStore) -> Void)
}
