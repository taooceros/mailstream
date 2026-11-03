import Foundation

/// What one list row shows, worked out off the main thread together with the list, so scrolling
/// and reloads only copy finished strings into views.
nonisolated struct ConversationRow: Hashable, Sendable {
    let id: String
    let latest: MessageRef
    /// "me, Josh" or "To Josh, Heidi"; the thread count is separate.
    let participants: String
    let count: Int
    /// Letter and name of the person the conversation is with, and the address their avatar is keyed on.
    let avatarInitials: String
    let avatarName: String
    let avatarKey: String
    /// `[list]` tags, shown quieter than the subject.
    let subjectPrefix: String
    let subjectTitle: String
    let date: String
    /// Date group header ("Today", "Last Week", "March").
    let section: String
    let isUnread: Bool
    let isFlagged: Bool
    let hasAttachment: Bool

    init(_ conversation: Conversation, own: Set<String>, recipients: Bool, calendar: Calendar, now: Date) {
        id = conversation.id
        latest = conversation.latest.ref
        participants = Self.participants(conversation, own: own, recipients: recipients)
        count = conversation.messages.count
        let person = Self.counterpart(conversation, own: own)
        avatarInitials = person.map(Avatar.initials) ?? "?"
        avatarName = person?.displayName ?? ""
        avatarKey = person?.normalizedEmail ?? ""
        let subject = Subject(conversation.subject)
        subjectPrefix = subject.list
        subjectTitle = subject.displayTitle
        date = conversation.latestDate.map { Self.format($0, calendar: calendar, now: now) } ?? ""
        section = Self.section(conversation.latestDate, calendar: calendar, now: now)
        isUnread = conversation.isUnread
        isFlagged = conversation.isFlagged
        hasAttachment = conversation.hasAttachment
    }

    /// Rows for a whole list; pure, so it runs on any thread.
    static func rows(for conversations: [Conversation], own: Set<String>, recipients: Bool) -> [String: ConversationRow] {
        let calendar = Calendar.current, now = Date()
        var rows: [String: ConversationRow] = [:]
        rows.reserveCapacity(conversations.count)
        for conversation in conversations {
            rows[conversation.id] = ConversationRow(conversation, own: own, recipients: recipients, calendar: calendar, now: now)
        }
        return rows
    }

    static func participants(_ conversation: Conversation, own: Set<String>, recipients: Bool) -> String {
        if recipients {
            let names = conversation.latest.envelope.to.map(\.shortName)
            return names.isEmpty ? "(No Recipients)" : "To " + names.joined(separator: ", ")
        }
        var names: [String] = []
        var seen = Set<String>()
        for message in conversation.messages {
            for address in message.envelope.from where seen.insert(address.normalizedEmail).inserted {
                names.append(own.contains(address.normalizedEmail) ? "me" : address.shortName)
            }
        }
        if conversation.hasDraft { names.append("Draft") }
        if names.isEmpty { return "(Unknown Sender)" }
        if names.count == 1, let only = names.first, conversation.messages.count == 1 {
            return conversation.messages[0].envelope.from.first.map { own.contains($0.normalizedEmail) ? "me" : $0.displayName } ?? only
        }
        if names.count > 3 { return "\(names[0]) … \(names[names.count - 1])" }
        return names.joined(separator: ", ")
    }

    /// The latest sender who isn't the user; in a thread of only the user's own mail, its recipient.
    static func counterpart(_ conversation: Conversation, own: Set<String>) -> Address? {
        conversation.latestIncoming(own: own)?.envelope.from.first
            ?? conversation.latest.envelope.to.first { !own.contains($0.normalizedEmail) }
            ?? conversation.latest.envelope.from.first
    }

    static func format(_ date: Date, calendar: Calendar = .current, now: Date = Date()) -> String {
        if calendar.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        if calendar.isDate(date, equalTo: now, toGranularity: .year) {
            return date.formatted(.dateTime.month(.abbreviated).day())
        }
        return date.formatted(date: .numeric, time: .omitted)
    }

    static func section(_ date: Date?, calendar: Calendar = .current, now: Date = Date()) -> String {
        guard let date else { return "Older" }
        if calendar.isDateInToday(date) { return "Today" }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        if calendar.isDate(date, equalTo: now, toGranularity: .weekOfYear) { return "This Week" }
        if let lastWeek = calendar.date(byAdding: .weekOfYear, value: -1, to: now),
           calendar.isDate(date, equalTo: lastWeek, toGranularity: .weekOfYear) { return "Last Week" }
        if calendar.isDate(date, equalTo: now, toGranularity: .year) {
            return date.formatted(.dateTime.month(.wide))
        }
        return date.formatted(.dateTime.month(.wide).year())
    }
}
