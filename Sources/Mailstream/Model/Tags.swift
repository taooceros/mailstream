import Foundation

/// Smart Inbox bucket assigned by the AI tagger.
nonisolated enum MailCategory: String, Codable, Sendable, CaseIterable {
    /// Written by a person to the user; stays in the main list.
    case people
    /// Automated: receipts, alerts, security notices, shipping, service mail.
    case notification
    /// Marketing, promotions, digests, mailing-list newsletters.
    case newsletter
    /// Calendar invitations, event RSVPs and their responses.
    case invitation

    var title: String {
        switch self {
        case .people: "People"
        case .notification: "Notifications"
        case .newsletter: "Newsletters"
        case .invitation: "Invitations"
        }
    }

    var symbol: String {
        switch self {
        case .people: "person.2"
        case .notification: "bell.badge"
        case .newsletter: "newspaper"
        case .invitation: "calendar"
        }
    }
}

nonisolated struct ConversationTag: Codable, Sendable, Hashable {
    var category: MailCategory
    /// A person asked the user something and the latest message is not from the user.
    var toRespond: Bool
    /// At most two short topic labels, e.g. "Finance", "Travel".
    var labels: [String]

    /// Waiting on the user's reply: only people conversations can be.
    var isWaiting: Bool { category == .people && toRespond }
}
