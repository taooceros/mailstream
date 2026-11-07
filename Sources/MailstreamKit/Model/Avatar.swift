import Foundation

/// A sender's letter, shared by the native views and the rows front ends get; the Mac's colors
/// for it are in `UI/ListCells.swift`.
nonisolated enum Avatar {
    /// The first letter of the name (or the address), uppercased; "?" when there is none.
    static func initials(for address: Address) -> String {
        let name = address.displayName.trimmingCharacters(in: .whitespaces)
        guard let first = name.first(where: { $0.isLetter || $0.isNumber }) else { return "?" }
        return String(first).uppercased()
    }
}
