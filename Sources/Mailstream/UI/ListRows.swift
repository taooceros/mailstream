import AppKit
import SwiftUI

extension Color {
    /// `token` with its alpha replaced by `alpha`, as AppKit's `withAlphaComponent` does; SwiftUI's
    /// `opacity` would multiply into the system color's own alpha and come out lighter. Resolved
    /// per appearance, since a system color with a new alpha otherwise keeps the light one.
    static func listTone(_ token: Design.Color, alpha: CGFloat) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            var resolved = token.color
            appearance.performAsCurrentDrawingAppearance {
                resolved = token.color.usingColorSpace(.sRGB)?.withAlphaComponent(alpha) ?? token.color
            }
            return resolved
        })
    }
}

// The conversation list's lines in SwiftUI. Every view here draws from plain values, so the same
// rows render from the store in the list and from fixtures anywhere else.

// MARK: - Row content

/// What one mail or chat row shows, formatted and ready to draw.
struct ListRowContent {
    var unread: Bool
    var initials: String
    var avatarColor: NSColor
    var photo: NSImage?
    /// Whose picture the row shows, for the enlarged preview.
    var person: Address
    var automated: Bool
    var sender: AttributedString
    var line: AttributedString
    var date: String
    var hasAttachment: Bool
    var flagged: Bool
    var topic: String?
    var account: (name: String, color: NSColor)?
    /// The hover pill's buttons, in order; empty for chat rows, which keep their date on hover.
    var actions: [(action: ConversationAction, state: ConversationAction.State)]

    var showsActions: Bool { !actions.isEmpty }

    /// A conversation: senders and count, subject prefix, subject and snippet.
    static func mail(participants: String, count: Int, subjectPrefix: String, subjectTitle: String, snippet: String?,
                     unread: Bool, flagged: Bool, hasAttachment: Bool, date: String, initials: String,
                     person: Address, avatarKey: String, photo: NSImage?, automated: Bool,
                     archivable: Bool, waiting: Bool, topic: String?,
                     account: (name: String, color: NSColor)?) -> ListRowContent {
        var name = senderText(participants, unread: unread)
        if count > 1 {
            var suffix = AttributedString("  \(count)")
            suffix.font = .designDigits(.caption, weight: .medium)
            suffix.foregroundColor = .design(.faint)
            name += suffix
        }
        var line = AttributedString()
        if !subjectPrefix.isEmpty {
            var prefix = AttributedString(subjectPrefix + " ")
            prefix.font = .design(.footnote)
            prefix.foregroundColor = .design(.faint)
            line += prefix
        }
        var title = AttributedString(subjectTitle)
        title.font = .design(.body, weight: unread ? .medium : .regular)
        title.foregroundColor = unread ? .design(.text) : Color.listTone(.text, alpha: 0.8)
        line += title
        if let snippet, !snippet.isEmpty {
            var preview = AttributedString("   " + snippet)
            preview.font = .design(.body)
            preview.foregroundColor = Color.listTone(.muted, alpha: 0.75)
            line += preview
        }
        let state = ConversationAction.State(unread: unread, flagged: flagged)
        var actions: [(action: ConversationAction, state: ConversationAction.State)] = []
        if waiting { actions.append((.toggleWaiting, ConversationAction.State())) }
        if archivable { actions.append((.archive, state)) }
        actions += [(.trash, state), (.toggleRead, state), (.toggleStar, state)]
        return ListRowContent(unread: unread, initials: initials, avatarColor: Avatar.color(for: avatarKey), photo: photo,
                              person: person, automated: automated, sender: name, line: line, date: date,
                              hasAttachment: hasAttachment, flagged: flagged, topic: topic, account: account,
                              actions: actions)
    }

    /// A store conversation row.
    init(_ row: ConversationRow, snippet: String?, archivable: Bool, account: (name: String, color: NSColor)?,
         tag: ConversationTag?, photo: NSImage?) {
        self = Self.mail(participants: row.participants, count: row.count, subjectPrefix: row.subjectPrefix,
                         subjectTitle: row.subjectTitle, snippet: snippet, unread: row.isUnread, flagged: row.isFlagged,
                         hasAttachment: row.hasAttachment, date: row.date, initials: row.avatarInitials,
                         person: Address(name: row.avatarName, email: row.avatarKey), avatarKey: row.avatarKey,
                         photo: photo, automated: tag.map { $0.category != .people } ?? false,
                         archivable: archivable, waiting: tag?.toRespond == true, topic: tag?.labels.first,
                         account: account)
    }

    /// A chat space in the same columns: title where the senders go, "Sender: text" as the line.
    static func chat(title: String, preview: String, unread: Bool, hasAttachment: Bool, date: String, initials: String,
                     person: Address, colorKey: String, photo: NSImage?,
                     account: (name: String, color: NSColor)?) -> ListRowContent {
        var line = AttributedString(preview)
        line.font = .design(.body, weight: unread ? .medium : .regular)
        line.foregroundColor = unread ? .design(.text) : .design(.muted)
        return ListRowContent(unread: unread, initials: initials, avatarColor: Avatar.color(for: colorKey), photo: photo,
                              person: person, automated: false, sender: senderText(title, unread: unread), line: line,
                              date: date, hasAttachment: hasAttachment, flagged: false, topic: nil, account: account,
                              actions: [])
    }

    init(_ row: ChatRow, account: (name: String, color: NSColor)?, photo: NSImage?) {
        self = Self.chat(title: row.title, preview: row.preview, unread: row.isUnread, hasAttachment: row.hasAttachment,
                         date: row.date, initials: row.avatarInitials,
                         person: Address(name: row.avatarName, email: row.avatarKey), colorKey: row.avatarColorKey,
                         photo: photo, account: account)
    }

    private init(unread: Bool, initials: String, avatarColor: NSColor, photo: NSImage?, person: Address, automated: Bool,
                 sender: AttributedString, line: AttributedString, date: String, hasAttachment: Bool, flagged: Bool,
                 topic: String?, account: (name: String, color: NSColor)?,
                 actions: [(action: ConversationAction, state: ConversationAction.State)]) {
        self.unread = unread
        self.initials = initials
        self.avatarColor = avatarColor
        self.photo = photo
        self.person = person
        self.automated = automated
        self.sender = sender
        self.line = line
        self.date = date
        self.hasAttachment = hasAttachment
        self.flagged = flagged
        self.topic = topic
        self.account = account
        self.actions = actions
    }

    /// The sender column: bold while unread, softened once read.
    private static func senderText(_ text: String, unread: Bool) -> AttributedString {
        var name = AttributedString(text)
        name.font = .design(.body, weight: unread ? .semibold : .regular)
        name.foregroundColor = unread ? .design(.text) : Color.listTone(.text, alpha: 0.72)
        return name
    }
}

// MARK: - Row chrome

/// The hover wash and the tinted (never solid) selection behind a selectable line.
struct ListRowChrome: ViewModifier {
    let highlighted: Bool
    /// The list has keyboard focus in the key window.
    let emphasized: Bool
    let hovered: Bool

    func body(content: Content) -> some View {
        content.background {
            let shape = RoundedRectangle(cornerRadius: Design.Radius.control.rawValue)
            ZStack {
                shape.fill(Color.design(.hover)).opacity(hovered && !highlighted ? 1 : 0)
                    .animation(highlighted ? nil : .house(.quick), value: hovered)
                if highlighted {
                    shape.fill(Color.listTone(.accent, alpha: emphasized ? 0.16 : 0.09))
                }
            }
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
        }
    }
}

// MARK: - Conversation row

/// One line, five quiet columns: unread dot · avatar and sender · subject and preview · topic · date
/// and account. Hovering cross-fades the trailing metadata into a floating action pill.
struct ListRowConversation: View {
    let content: ListRowContent
    let hovered: Bool
    var onAction: (ConversationAction) -> Void = { _ in }
    /// A click on the sender's picture, with a view to anchor an enlarged one to.
    var onAvatar: ((NSView, NSImage) -> Void)?

    private var showsPill: Bool { hovered && content.showsActions }
    private var pillWidth: CGFloat {
        CGFloat(content.actions.count) * ListRowActionButton.size.width + 2 * ListRowActionPill.inset
    }

    var body: some View {
        ZStack(alignment: .leading) {
            if content.unread {
                Circle().fill(Color.design(.accent))
                    .frame(width: 7, height: 7)
                    .offset(x: ListMetrics.dotX - 3.5)
            }
            ListRowAvatar(initials: content.initials, color: content.avatarColor, photo: content.photo, onClick: onAvatar)
                .offset(x: ListMetrics.senderX)
            Text(content.sender)
                .lineLimit(1).truncationMode(.tail)
                .frame(width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX, alignment: .leading)
                .offset(x: ListMetrics.nameX)
            HStack(spacing: 0) {
                Text(content.line)
                    .lineLimit(1).truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.trailing, 14)
                trailing
                    // While hovering, the subject also ends 10 pt before the pill.
                    .frame(minWidth: showsPill ? pillWidth - 8 : 0, alignment: .trailing)
            }
            .padding(.leading, ListMetrics.subjectX)
            .padding(.trailing, 14)
            if showsPill {
                ListRowActionPill(actions: content.actions, onAction: onAction)
                    .frame(maxWidth: .infinity, alignment: .trailing)
                    .padding(.trailing, 10)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(x: 6)), removal: .opacity))
            }
        }
        .frame(height: ListMetrics.rowHeight)
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(.house(.quick), value: showsPill)
    }

    /// Paperclip, star, topic, date and account tick, packed right to left; hidden items take no space.
    private var trailing: some View {
        HStack(spacing: 0) {
            if content.hasAttachment {
                Image(systemName: "paperclip")
                    .font(.system(size: Design.TextSize.caption.rawValue, weight: .medium))
                    .foregroundStyle(Color.design(.faint))
                    .accessibilityLabel("Attachment")
                    .padding(.trailing, 10)
            }
            if content.flagged {
                Image(systemName: "star.fill")
                    .font(.system(size: Design.TextSize.micro.rawValue, weight: .semibold))
                    .foregroundStyle(Color.design(.star))
                    .accessibilityLabel("Starred")
                    .padding(.trailing, 10)
            }
            if let topic = content.topic {
                ListRowTopic(text: topic, color: Color(nsColor: TagPalette.color(for: topic)))
                    .padding(.trailing, 10)
            }
            Text(content.date)
                .font(.designDigits(.footnote, weight: content.unread ? .medium : .regular))
                .foregroundStyle(content.unread ? Color.design(.text) : Color.design(.muted))
                .lineLimit(1)
                .frame(width: ListMetrics.dateWidth, alignment: .trailing)
                .padding(.trailing, 8)
            RoundedRectangle(cornerRadius: 1)
                .fill(content.account.map { Color(nsColor: $0.color) } ?? .clear)
                .frame(width: 2.5, height: 12)
                .help(content.account?.name ?? "")
        }
        .opacity(showsPill ? 0 : 1)
    }
}

/// "● Work": a topic as a tiny colored dot and quiet text, no pill.
struct ListRowTopic: View {
    let text: String
    let color: Color

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text)
                .font(.design(.caption))
                .foregroundStyle(Color.design(.muted))
                .lineLimit(1)
                .fixedSize()
        }
    }
}

/// Initials on a tint of the person's color, or their picture once one is known.
struct ListRowAvatar: View {
    let initials: String
    let color: NSColor
    let photo: NSImage?
    /// Only a picture takes the click (to enlarge it); otherwise the row underneath gets it.
    var onClick: ((NSView, NSImage) -> Void)?
    @State private var anchor = ListRowAnchor.Box()

    var body: some View {
        let size = ListMetrics.avatarSize
        ZStack {
            Circle().fill(Color(nsColor: color).opacity(0.15))
            if let photo {
                Image(nsImage: photo)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .transition(.opacity.animation(.house(.brief)))
            } else {
                Text(initials)
                    .font(.design(.body, weight: .semibold))
                    .foregroundStyle(Color(nsColor: color))
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .background(ListRowAnchor(box: anchor))
        .overlay {
            if let photo, let onClick {
                Color.clear
                    .contentShape(Circle())
                    .onTapGesture { if let view = anchor.view { onClick(view, photo) } }
                    .pointerStyle(.link)
            }
        }
    }
}

/// An AppKit view behind a SwiftUI one, for anchoring an AppKit popover to it.
struct ListRowAnchor: NSViewRepresentable {
    final class Box {
        weak var view: NSView?
    }

    let box: Box

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        box.view = view
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) { box.view = nsView }
}

// MARK: - Hover pill

/// Floating capsule holding the row's quick actions.
struct ListRowActionPill: View {
    static let height: CGFloat = 28
    static let inset: CGFloat = 3
    let actions: [(action: ConversationAction, state: ConversationAction.State)]
    let onAction: (ConversationAction) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(actions, id: \.action) { item in
                ListRowActionButton(action: item.action, state: item.state, onAction: onAction)
            }
        }
        .padding(.horizontal, Self.inset)
        .frame(height: Self.height)
        .background {
            let shape = RoundedRectangle(cornerRadius: Design.Radius.control.rawValue, style: .continuous)
            shape.fill(Color(nsColor: .controlBackgroundColor))
                .overlay(shape.strokeBorder(Color.design(.line), lineWidth: 1))
                .shadow(color: .black.opacity(0.08), radius: 4, y: 1)
        }
    }
}

/// One quick action in a row's hover pill, titled for the row it sits on.
struct ListRowActionButton: View {
    static let size = CGSize(width: 28, height: 26)
    let action: ConversationAction
    let state: ConversationAction.State
    let onAction: (ConversationAction) -> Void
    @State private var hovered = false

    var body: some View {
        let label = action.label(in: state)
        Button { onAction(action) } label: {
            Image(systemName: label.symbol)
                .font(.system(size: Design.TextSize.footnote.rawValue, weight: .regular))
                .foregroundStyle(hovered ? Color.design(.text) : Color.design(.muted))
                .frame(width: Self.size.width, height: Self.size.height)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(label.title)
        .accessibilityLabel(label.title)
        .onHover { hovered = $0 }
    }
}

// MARK: - Section header

/// "TODAY", or "● WAITING ON YOU 3": caption caps, 6 pt above the bottom edge.
struct ListRowSectionHeader: View {
    let title: String
    let count: Int?
    let accent: Bool

    var body: some View {
        HStack(spacing: 7) {
            if accent {
                Circle().fill(Color.design(.waiting)).frame(width: 6, height: 6)
            }
            Text(text).lineLimit(1)
        }
        .padding(.leading, ListMetrics.senderX)
        .padding(.bottom, 6)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
        .frame(height: ListMetrics.sectionHeight)
    }

    private var text: AttributedString {
        var text = AttributedString(title.uppercased())
        text.font = .design(.caption, weight: .semibold)
        text.foregroundColor = accent ? Color.listTone(.text, alpha: 0.75) : .design(.muted)
        text.kern = 0.6
        if let count {
            var suffix = AttributedString("   \(count)")
            suffix.font = .designDigits(.caption, weight: .medium)
            suffix.foregroundColor = .design(.faint)
            text += suffix
        }
        return text
    }
}

// MARK: - Bundle shelf

/// The Smart Inbox's automated mail as a shelf of equal tiles above the people.
struct ListRowShelf: View {
    let bundles: [ConversationListBundle]
    let own: Set<String>
    /// The pointer is over the shelf: tiles lift only then, so a scroll can't strand one.
    let hovered: Bool
    let onOpen: (MailCategory) -> Void

    var body: some View {
        HStack(spacing: 10) {
            ForEach(bundles) { bundle in
                ListRowBundleTile(category: bundle.category, conversations: bundle.members, own: own,
                                  shelfHovered: hovered, onOpen: onOpen)
            }
        }
        .padding(EdgeInsets(top: 10, leading: 8, bottom: 8, trailing: 8))
        .frame(height: ListMetrics.shelfHeight)
    }
}

/// One shelf tile: symbol, title and senders on the left, the unread count on the right.
/// Lifts on hover and dips when pressed.
struct ListRowBundleTile: View {
    let category: MailCategory
    let conversations: [Conversation]
    let own: Set<String>
    let shelfHovered: Bool
    let onOpen: (MailCategory) -> Void
    @State private var pointerInside = false

    static func tint(_ category: MailCategory) -> Color {
        switch category {
        case .people: Color(nsColor: .systemBlue)
        case .notification: Color(nsColor: .systemTeal)
        case .newsletter: Color(nsColor: .systemOrange)
        case .invitation: Color(nsColor: .systemPink)
        }
    }

    var body: some View {
        let unread = conversations.filter(\.isUnread).count
        Button { onOpen(category) } label: {
            ListRowTileFace(category: category, senders: senders, total: conversations.count, unread: unread,
                            hovered: shelfHovered && pointerInside)
        }
        .buttonStyle(ListRowTilePress())
        .onHover { pointerInside = $0 }
        .help("\(category.title): \(conversations.count) conversations, \(unread) unread")
    }

    /// Up to four distinct first names, in list order.
    private var senders: String {
        var seen = Set<String>()
        var names: [String] = []
        for conversation in conversations {
            guard let name = ConversationRow.counterpart(conversation, own: own)?.shortName,
                  seen.insert(name.lowercased()).inserted else { continue }
            names.append(name)
            if names.count == 4 { break }
        }
        return names.joined(separator: ", ")
    }
}

/// A tile's face, drawn from values.
struct ListRowTileFace: View {
    let category: MailCategory
    let senders: String
    let total: Int
    let unread: Int
    let hovered: Bool

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Design.Radius.card.rawValue, style: .continuous)
        HStack(spacing: 9) {
            Image(systemName: category.symbol)
                .font(.design(.headline, weight: .medium))
                .foregroundStyle(ListRowBundleTile.tint(category))
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 1) {
                Text(category.title)
                    .font(.design(.body, weight: .semibold))
                    .foregroundStyle(Color.design(.text))
                    .lineLimit(1)
                Text(senders)
                    .font(.design(.footnote))
                    .foregroundStyle(Color.design(.muted))
                    .lineLimit(1).truncationMode(.tail)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(unread > 0 ? unread.formatted() : total.formatted())
                .font(.designDigits(.caption, weight: .semibold))
                .foregroundStyle(unread > 0 ? Color.design(.accent) : Color.design(.muted))
                .padding(.horizontal, 7)
                .frame(height: 18)
                .background(Capsule().fill(unread > 0 ? Color.listTone(.accent, alpha: 0.12) : Color.design(.chip)))
                .padding(.leading, -1)
        }
        .padding(.leading, 12)
        .padding(.trailing, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(shape.fill(Color.listTone(.text, alpha: hovered ? 0.065 : 0.035)))
        .overlay(shape.strokeBorder(Color.listTone(.text, alpha: hovered ? 0.1 : 0.06), lineWidth: 1))
        .contentShape(shape)
        .animation(.house(.brief), value: hovered)
    }
}

/// Dips to 0.97 while pressed, springing back.
struct ListRowTilePress: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.interpolatingSpring(mass: 1, stiffness: 500, damping: 30), value: configuration.isPressed)
    }
}

// MARK: - Status pill

/// Floating capsule at the bottom of the list for transient work ("Checking for mail…") and errors.
struct ListRowStatusPill: View {
    let text: String
    let busy: Bool

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        HStack(spacing: 7) {
            if busy {
                ProgressView().controlSize(.small)
            }
            Text(busy ? text : "⚠︎  " + text)
                .font(.design(.footnote, weight: .medium))
                .foregroundStyle(busy ? Color.design(.muted) : Color(nsColor: .systemOrange))
                .lineLimit(1).truncationMode(.tail)
        }
        .padding(.leading, 12)
        .padding(.trailing, 14)
        .frame(height: 28)
        .background(.regularMaterial, in: shape)
        .overlay(shape.strokeBorder(Color.design(.line), lineWidth: 1))
        .help(text)
    }
}
