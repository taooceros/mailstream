import Foundation

/// RFC 3339 / ISO 8601 with or without fractional seconds; fractional first, so microseconds survive.
nonisolated func parseISODate(_ raw: String) -> Date? {
    if let date = try? Date(raw, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true)) { return date }
    return try? Date(raw, strategy: .iso8601)
}
