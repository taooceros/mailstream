import Foundation

nonisolated extension Data {
    /// Decodes base64url (RFC 4648 §5) or plain base64, padded or not; characters outside the
    /// alphabet (line breaks, padding) are skipped before the padding is recomputed.
    init?(base64URLEncoded text: some StringProtocol) {
        var base64 = String.UnicodeScalarView()
        for scalar in text.unicodeScalars {
            switch scalar {
            case "-": base64.append("+")
            case "_": base64.append("/")
            case "A"..."Z", "a"..."z", "0"..."9", "+", "/": base64.append(scalar)
            default: continue
            }
        }
        base64.append(contentsOf: repeatElement("=", count: (4 - base64.count % 4) % 4))
        self.init(base64Encoded: String(base64))
    }

    /// Unpadded base64url (RFC 4648 §5).
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
