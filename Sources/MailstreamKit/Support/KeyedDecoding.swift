nonisolated extension KeyedDecodingContainer {
    /// The value at `key`, or `fallback` when it is absent or null.
    func decodeIfPresent<T: Decodable>(_ key: Key, default fallback: T) throws -> T {
        try decodeIfPresent(T.self, forKey: key) ?? fallback
    }
}
