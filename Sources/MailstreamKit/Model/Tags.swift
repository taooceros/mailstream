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
    /// The latest incoming message needs the user's reply or action and is not from the user: a
    /// person asked something, or automated mail asks for an approval, a payment, a security check.
    /// Newsletters never do.
    var toRespond: Bool
    /// At most two short topic labels, e.g. "Finance", "Travel".
    var labels: [String]
    /// When the ask lapses: the day after its deadline or event, or after a routine ask goes stale.
    /// Nil keeps it waiting until resolved or answered.
    var waitingUntil: Date?

    /// `toRespond`, unless the ask has lapsed by `now`.
    func isWaiting(at now: Date = .now) -> Bool {
        toRespond && (waitingUntil.map { now < $0 } ?? true)
    }
}
