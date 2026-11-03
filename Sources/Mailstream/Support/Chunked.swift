nonisolated extension Collection {
    /// The elements in order, `size` at a time; the last chunk may be shorter.
    func chunked(into size: Int) -> [[Element]] {
        precondition(size > 0, "chunk size must be positive")
        var chunks: [[Element]] = []
        var start = startIndex
        while start != endIndex {
            let end = index(start, offsetBy: size, limitedBy: endIndex) ?? endIndex
            chunks.append(Array(self[start..<end]))
            start = end
        }
        return chunks
    }
}
