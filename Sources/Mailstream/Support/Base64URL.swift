import Foundation

nonisolated extension Data {
    /// Decodes base64url (RFC 4648 §5) or plain base64, padded or not; characters outside the
    /// alphabet (line breaks) are skipped.
    init?(base64URLEncoded text: some StringProtocol) {
        var base64 = String(text).replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        self.init(base64Encoded: base64, options: .ignoreUnknownCharacters)
    }

    /// Unpadded base64url (RFC 4648 §5).
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
