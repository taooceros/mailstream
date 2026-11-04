nonisolated extension String {
    /// The string, or nil when it is empty.
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
