import Foundation

private nonisolated let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

nonisolated extension String {
    /// Everything but the RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`) as `%XX`: safe as a
    /// path segment, a query value or a form field.
    var percentEncoded: String { addingPercentEncoding(withAllowedCharacters: unreserved) ?? self }
}

/// `name=value&…` for a URL query or an `application/x-www-form-urlencoded` body. Values are
/// percent-encoded; names are constants and kept as they are (Graph's `$select` keeps its `$`).
nonisolated func formEncoded(_ fields: [(String, String)]) -> String {
    fields.map { "\($0)=\($1.percentEncoded)" }.joined(separator: "&")
}

/// `fields` sorted by name, so the same fields always encode the same way.
nonisolated func formEncoded(_ fields: [String: String]) -> String {
    formEncoded(fields.sorted { $0.key < $1.key }.map { ($0.key, $0.value) })
}
