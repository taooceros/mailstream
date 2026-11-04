import AppKit
import SwiftUI

extension Color {
    /// `token` with its alpha replaced by `alpha`, as AppKit's `withAlphaComponent` does; SwiftUI's
    /// `opacity` would multiply into the system color's own alpha and come out lighter. Resolved
    /// per appearance, since a system color with a new alpha otherwise keeps the light one. Made
    /// once per token and alpha: rows ask for these every time they draw.
    static func listTone(_ token: Design.Color, alpha: CGFloat) -> Color {
        let key = ListToneKey(token: token, alpha: alpha)
        if let color = listTones[key] { return color }
        let color = Color(nsColor: NSColor(name: nil) { appearance in
            var resolved = token.color
            appearance.performAsCurrentDrawingAppearance {
                resolved = token.color.usingColorSpace(.sRGB)?.withAlphaComponent(alpha) ?? token.color
            }
            return resolved
        })
        listTones[key] = color
        return color
    }

    private static var listTones: [ListToneKey: Color] = [:]
}

private struct ListToneKey: Hashable {
    let token: Design.Color
    let alpha: CGFloat
}

/// Fixed horizontal stops, so every row's columns read as straight vertical lines.
enum ListMetrics {
    static let rowHeight: CGFloat = 38
    static let sectionHeight: CGFloat = 34
    static let shelfHeight: CGFloat = 70
    /// Center of the unread dot.
    static let dotX: CGFloat = 14
    /// Left edge of the sender column: the avatar, then the names.
    static let senderX: CGFloat = 28
    static let avatarSize: CGFloat = 28
    static let senderWidth: CGFloat = 196
    static let nameX: CGFloat = senderX + avatarSize + 9
    static let subjectX: CGFloat = senderX + senderWidth + 16
    static let dateWidth: CGFloat = 62
}

/// A sender's letter and a stable per-address color, shared by the sidebar and the reader.
enum Avatar {
    nonisolated static func color(for email: String) -> NSColor {
        let palette: [NSColor] = [.systemBlue, .systemPurple, .systemPink, .systemRed, .systemOrange,
                                  .systemGreen, .systemTeal, .systemIndigo, .systemBrown, .systemMint, .systemCyan]
        return palette[stableIndex(of: email.lowercased(), in: palette.count)]
    }

    /// The first letter of the name (or the address), uppercased; "?" when there is none.
    nonisolated static func initials(for address: Address) -> String {
        let name = address.displayName.trimmingCharacters(in: .whitespaces)
        guard let first = name.first(where: { $0.isLetter || $0.isNumber }) else { return "?" }
        return String(first).uppercased()
    }
}

// The conversation list's lines in SwiftUI. Every view here draws from plain values, so the same
// rows render from the store in the list and from fixtures anywhere else.

// MARK: - Row content

/// What one mail or chat row shows, formatted and ready to draw.
struct ListRowContent {
    var unread: Bool
    var initials: String
    var avatarColor: Color
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
    var topicColor: Color?
    var account: (name: String, color: Color)?
    /// The hover pill's buttons, in order; empty for chat rows, which keep their date on hover.
    var actions: [(action: ConversationAction, state: ConversationAction.State)]

    var showsActions: Bool { !actions.isEmpty }

    /// A conversation: senders and count, subject prefix, subject and snippet.
    static func mail(participants: String, count: Int, subjectPrefix: String, subjectTitle: String, snippet: String?,
                     unread: Bool, flagged: Bool, hasAttachment: Bool, date: String, initials: String,
                     person: Address, avatarKey: String, photo: NSImage?, automated: Bool,
                     archivable: Bool, waiting: Bool, topic: String?,
                     account: (name: String, color: Color)?) -> ListRowContent {
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
            // One line shows a fraction of a long preview; laying out the rest costs every draw.
            var preview = AttributedString("   " + snippet.prefix(Self.snippetLength))
            preview.font = .design(.body)
            preview.foregroundColor = Color.listTone(.muted, alpha: 0.75)
            line += preview
        }
        let state = ConversationAction.State(unread: unread, flagged: flagged)
        var actions: [(action: ConversationAction, state: ConversationAction.State)] = []
        if waiting { actions.append((.toggleWaiting, ConversationAction.State())) }
        if archivable { actions.append((.archive, state)) }
        actions += [(.trash, state), (.toggleRead, state), (.toggleStar, state)]
        return ListRowContent(unread: unread, initials: initials, avatarColor: Self.avatarColor(for: avatarKey), photo: photo,
                              person: person, automated: automated, sender: name, line: line, date: date,
                              hasAttachment: hasAttachment, flagged: flagged, topic: topic, account: account,
                              actions: actions)
    }

    /// A store conversation row.
    init(_ row: ConversationRow, snippet: String?, archivable: Bool, account: (name: String, color: Color)?,
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
                     account: (name: String, color: Color)?) -> ListRowContent {
        var line = AttributedString(preview)
        line.font = .design(.body, weight: unread ? .medium : .regular)
        line.foregroundColor = unread ? .design(.text) : .design(.muted)
        return ListRowContent(unread: unread, initials: initials, avatarColor: Self.avatarColor(for: colorKey), photo: photo,
                              person: person, automated: false, sender: senderText(title, unread: unread), line: line,
                              date: date, hasAttachment: hasAttachment, flagged: false, topic: nil, account: account,
                              actions: [])
    }

    init(_ row: ChatRow, account: (name: String, color: Color)?, photo: NSImage?) {
        self = Self.chat(title: row.title, preview: row.preview, unread: row.isUnread, hasAttachment: row.hasAttachment,
                         date: row.date, initials: row.avatarInitials,
                         person: Address(name: row.avatarName, email: row.avatarKey), colorKey: row.avatarColorKey,
                         photo: photo, account: account)
    }

    private init(unread: Bool, initials: String, avatarColor: Color, photo: NSImage?, person: Address, automated: Bool,
                 sender: AttributedString, line: AttributedString, date: String, hasAttachment: Bool, flagged: Bool,
                 topic: String?, account: (name: String, color: Color)?,
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
        self.topicColor = topic.map { Color(nsColor: TagPalette.color(for: $0)) }
        self.account = account
        self.actions = actions
    }

    private static let snippetLength = 240

    private static func avatarColor(for key: String) -> Color { Color(nsColor: Avatar.color(for: key)) }

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
///
/// The columns are drawn in one `Canvas`: built from a dozen views, a line cost SwiftUI a view
/// graph and a layout pass every time it scrolled into view. Only what takes input is a view: the
/// pill, the picture's click target and the account tick's tooltip.
struct ListRowConversation: View {
    let content: ListRowContent
    let hovered: Bool
    var onAction: (ConversationAction) -> Void = { _ in }
    /// Where the sender's picture enlarges from; without it the picture does not take clicks.
    var avatars: AvatarStore?

    private var showsPill: Bool { hovered && content.showsActions }

    var body: some View {
        ListRowCanvas(content: content, pill: showsPill ? 1 : 0)
            .overlay(alignment: .leading) {
                if let photo = content.photo, let avatars {
                    ListRowPictureTarget(person: content.person, photo: photo, automated: content.automated, avatars: avatars)
                        .offset(x: ListMetrics.senderX)
                }
            }
            .overlay(alignment: .trailing) {
                if let account = content.account {
                    Color.clear
                        .frame(width: 8, height: 16)
                        .contentShape(Rectangle())
                        .help(account.name)
                        .padding(.trailing, ListRowCanvas.trailingInset - 2.75)
                }
            }
            .overlay(alignment: .trailing) {
                if showsPill {
                    ListRowActionPill(actions: content.actions, onAction: onAction)
                        .padding(.trailing, 10)
                        .transition(.asymmetric(insertion: .opacity.combined(with: .offset(x: 6)), removal: .opacity))
                }
            }
            .frame(height: ListMetrics.rowHeight)
            .frame(maxWidth: .infinity)
            .animation(.house(.quick), value: showsPill)
    }
}

/// The drawn columns of a row. `pill` runs from 0 to 1 as the hover pill fades in: the trailing
/// metadata fades out and the subject ends 10 pt before the pill.
private struct ListRowCanvas: View, Animatable {
    static let trailingInset: CGFloat = 14

    let content: ListRowContent
    var pill: Double

    var animatableData: Double {
        get { pill }
        set { pill = newValue }
    }

    var body: some View {
        Canvas { context, size in draw(in: &context, size: size) }
            .accessibilityElement()
            .accessibilityLabel(accessibilityText)
    }

    private var accessibilityText: String {
        var parts = [String(content.sender.characters), String(content.line.characters), content.date]
        if content.unread { parts.insert("Unread", at: 0) }
        if let topic = content.topic { parts.append(topic) }
        if content.flagged { parts.append("Starred") }
        if content.hasAttachment { parts.append("Attachment") }
        if let account = content.account { parts.append(account.name) }
        return parts.joined(separator: ", ")
    }

    private func draw(in context: inout GraphicsContext, size: CGSize) {
        let mid = size.height / 2
        let lineHeight = context.resolve(Text("Ag").font(.design(.body, weight: .semibold)))
            .measure(in: size).height
        if content.unread {
            context.fill(Path(ellipseIn: CGRect(x: ListMetrics.dotX - 3.5, y: mid - 3.5, width: 7, height: 7)),
                         with: .color(.design(.accent)))
        }
        drawAvatar(in: &context, mid: mid)
        Self.drawLine(context.resolve(Text(content.sender)), in: &context, x: ListMetrics.nameX,
                      width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX, mid: mid, height: lineHeight)

        // Paperclip, star, topic, date and account tick, packed right to left; hidden items take no space.
        let right = size.width - Self.trailingInset
        var trailing = context
        trailing.opacity = 1 - pill
        if let account = content.account {
            trailing.fill(Path(roundedRect: CGRect(x: right - 2.5, y: mid - 6, width: 2.5, height: 12), cornerRadius: 1),
                          with: .color(account.color))
        }
        var x = right - 2.5 - 8
        trailing.draw(Text(content.date)
                          .font(.designDigits(.footnote, weight: content.unread ? .medium : .regular))
                          .foregroundStyle(content.unread ? Color.design(.text) : Color.design(.muted)),
                      at: CGPoint(x: x, y: mid), anchor: .trailing)
        x -= ListMetrics.dateWidth
        if let topic = content.topic, let color = content.topicColor {
            x -= 10
            x -= Self.drawTrailing(Text(topic).font(.design(.caption)).foregroundStyle(Color.design(.muted)),
                                   in: &trailing, right: x, mid: mid, size: size) + 5
            trailing.fill(Path(ellipseIn: CGRect(x: x - 6, y: mid - 3, width: 6, height: 6)), with: .color(color))
            x -= 6
        }
        if content.flagged {
            x -= 10
            x -= Self.drawTrailing(Text(Image(systemName: "star.fill"))
                                       .font(.system(size: Design.TextSize.micro.rawValue, weight: .semibold))
                                       .foregroundStyle(Color.design(.star)),
                                   in: &trailing, right: x, mid: mid, size: size)
        }
        if content.hasAttachment {
            x -= 10
            x -= Self.drawTrailing(Text(Image(systemName: "paperclip"))
                                       .font(.system(size: Design.TextSize.caption.rawValue, weight: .medium))
                                       .foregroundStyle(Color.design(.faint)),
                                   in: &trailing, right: x, mid: mid, size: size)
        }

        var reserved = right - x
        if content.showsActions {
            let pillWidth = CGFloat(content.actions.count) * ListRowActionButton.size.width + 2 * ListRowActionPill.inset
            reserved += (max(reserved, pillWidth - 8) - reserved) * pill
        }
        Self.drawLine(context.resolve(Text(content.line)), in: &context, x: ListMetrics.subjectX,
                      width: right - reserved - 14 - ListMetrics.subjectX, mid: mid, height: lineHeight)
    }

    /// Initials on a tint of the person's color, or their picture once one is known.
    private func drawAvatar(in context: inout GraphicsContext, mid: CGFloat) {
        let side = ListMetrics.avatarSize
        let avatar = CGRect(x: ListMetrics.senderX, y: mid - side / 2, width: side, height: side)
        context.fill(Path(ellipseIn: avatar), with: .color(content.avatarColor.opacity(0.15)))
        if let photo = content.photo {
            var clipped = context
            clipped.clip(to: Path(ellipseIn: avatar))
            clipped.draw(Image(nsImage: photo).resizable(), in: Self.filling(avatar, with: photo.size))
        } else {
            context.draw(Text(content.initials).font(.design(.body, weight: .semibold)).foregroundStyle(content.avatarColor),
                         at: CGPoint(x: avatar.midX, y: avatar.midY))
        }
    }

    /// One line of `text` from `x`, cut with an ellipsis at `width`: the rect holds a single line.
    private static func drawLine(_ text: GraphicsContext.ResolvedText, in context: inout GraphicsContext, x: CGFloat,
                                 width: CGFloat, mid: CGFloat, height: CGFloat) {
        guard width > 0 else { return }
        context.draw(text, in: CGRect(x: x, y: mid - height / 2, width: width, height: height))
    }

    /// Draws `text` ending at `right`; returns its width.
    private static func drawTrailing(_ text: Text, in context: inout GraphicsContext, right: CGFloat, mid: CGFloat,
                                     size: CGSize) -> CGFloat {
        let resolved = context.resolve(text)
        context.draw(resolved, at: CGPoint(x: right, y: mid), anchor: .trailing)
        return resolved.measure(in: size).width
    }

    /// `rect` filled by an image of `size`, centered, as `aspectRatio(contentMode: .fill)` does.
    private static func filling(_ rect: CGRect, with size: CGSize) -> CGRect {
        guard size.width > 0, size.height > 0 else { return rect }
        let scale = max(rect.width / size.width, rect.height / size.height)
        let fitted = CGSize(width: size.width * scale, height: size.height * scale)
        return CGRect(x: rect.midX - fitted.width / 2, y: rect.midY - fitted.height / 2,
                      width: fitted.width, height: fitted.height)
    }
}

/// A row picture's click target, enlarging it in a popover.
private struct ListRowPictureTarget: View {
    let person: Address
    let photo: NSImage
    let automated: Bool
    let avatars: AvatarStore
    @State private var shown: AvatarPreviewItem?

    var body: some View {
        Color.clear
            .frame(width: ListMetrics.avatarSize, height: ListMetrics.avatarSize)
            .contentShape(Circle())
            .onTapGesture { shown = AvatarPreviewItem(address: person, small: photo, automated: automated, avatars: avatars) }
            .pointerStyle(.link)
            .popover(item: $shown, arrowEdge: .trailing) { AvatarPreviewView(item: $0) }
    }
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
