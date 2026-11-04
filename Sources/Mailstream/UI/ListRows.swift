import AppKit
import SwiftUI

extension NSColor {
    /// `token` with its alpha replaced by `alpha`, as AppKit's `withAlphaComponent` does; SwiftUI's
    /// `opacity` would multiply into the system color's own alpha and come out lighter. Resolved
    /// per appearance, since a system color with a new alpha otherwise keeps the light one. Made
    /// once per token and alpha: rows ask for these every time they draw.
    static func listTone(_ token: Design.Color, alpha: CGFloat) -> NSColor {
        let key = ListToneKey(token: token, alpha: alpha)
        if let color = listTones[key] { return color }
        let color = NSColor(name: nil) { appearance in
            var resolved = token.color
            appearance.performAsCurrentDrawingAppearance {
                resolved = token.color.usingColorSpace(.sRGB)?.withAlphaComponent(alpha) ?? token.color
            }
            return resolved
        }
        listTones[key] = color
        return color
    }

    private static var listTones: [ListToneKey: NSColor] = [:]
}

extension Color {
    /// `NSColor.listTone` for SwiftUI.
    static func listTone(_ token: Design.Color, alpha: CGFloat) -> Color { Color(nsColor: .listTone(token, alpha: alpha)) }
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

// The conversation list's lines. Every view here draws from plain values, so the same rows render
// from the store in the list and from fixtures anywhere else.

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
    /// One line each, cut with an ellipsis.
    var sender: NSAttributedString
    var line: NSAttributedString
    var date: String
    var hasAttachment: Bool
    var flagged: Bool
    var topic: String?
    var topicColor: NSColor?
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
        let name = NSMutableAttributedString(attributedString: senderText(participants, unread: unread))
        if count > 1 {
            name.append(Self.text("  \(count)", font: Self.digits(.caption, weight: .medium), color: Design.Color.faint.color))
        }
        let line = NSMutableAttributedString()
        if !subjectPrefix.isEmpty {
            line.append(Self.text(subjectPrefix + " ", font: Design.font(.footnote), color: Design.Color.faint.color))
        }
        line.append(Self.text(subjectTitle, font: Design.font(.body, weight: unread ? .medium : .regular),
                              color: unread ? Design.Color.text.color : .listTone(.text, alpha: 0.8)))
        if let snippet, !snippet.isEmpty {
            // One line shows a fraction of a long preview; laying out the rest costs every draw.
            line.append(Self.text("   " + snippet.prefix(Self.snippetLength), font: Design.font(.body),
                                  color: .listTone(.muted, alpha: 0.75)))
        }
        let state = ConversationAction.State(unread: unread, flagged: flagged)
        var actions: [(action: ConversationAction, state: ConversationAction.State)] = []
        if waiting { actions.append((.toggleWaiting, ConversationAction.State())) }
        if archivable { actions.append((.archive, state)) }
        actions += [(.trash, state), (.toggleRead, state), (.toggleStar, state)]
        return ListRowContent(unread: unread, initials: initials, avatarColor: Avatar.color(for: avatarKey), photo: photo,
                              person: person, automated: automated, sender: Self.oneLine(name), line: Self.oneLine(line),
                              date: date, hasAttachment: hasAttachment, flagged: flagged, topic: topic, account: account,
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
        let line = Self.text(preview, font: Design.font(.body, weight: unread ? .medium : .regular),
                             color: unread ? Design.Color.text.color : Design.Color.muted.color)
        return ListRowContent(unread: unread, initials: initials, avatarColor: Avatar.color(for: colorKey), photo: photo,
                              person: person, automated: false, sender: Self.oneLine(senderText(title, unread: unread)),
                              line: Self.oneLine(line), date: date, hasAttachment: hasAttachment, flagged: false,
                              topic: nil, account: account, actions: [])
    }

    init(_ row: ChatRow, account: (name: String, color: NSColor)?, photo: NSImage?) {
        self = Self.chat(title: row.title, preview: row.preview, unread: row.isUnread, hasAttachment: row.hasAttachment,
                         date: row.date, initials: row.avatarInitials,
                         person: Address(name: row.avatarName, email: row.avatarKey), colorKey: row.avatarColorKey,
                         photo: photo, account: account)
    }

    private init(unread: Bool, initials: String, avatarColor: NSColor, photo: NSImage?, person: Address, automated: Bool,
                 sender: NSAttributedString, line: NSAttributedString, date: String, hasAttachment: Bool, flagged: Bool,
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
        self.topicColor = topic.map { TagPalette.color(for: $0) }
        self.account = account
        self.actions = actions
    }

    private static let snippetLength = 240

    static func text(_ string: some StringProtocol, font: NSFont, color: NSColor) -> NSAttributedString {
        NSAttributedString(string: String(string), attributes: [.font: font, .foregroundColor: color])
    }

    /// Digits of equal width, for counts and dates that line up in columns.
    static func digits(_ size: Design.TextSize, weight: NSFont.Weight = .regular) -> NSFont {
        .monospacedDigitSystemFont(ofSize: size.rawValue, weight: weight)
    }

    /// `text` kept to one line, cut with an ellipsis where it runs out of room.
    private static func oneLine(_ text: NSAttributedString) -> NSAttributedString {
        let line = NSMutableAttributedString(attributedString: text)
        line.addAttribute(.paragraphStyle, value: truncating, range: NSRange(location: 0, length: line.length))
        return line
    }

    private static let truncating: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    /// The sender column: bold while unread, softened once read.
    private static func senderText(_ text: String, unread: Bool) -> NSAttributedString {
        Self.text(text, font: Design.font(.body, weight: unread ? .semibold : .regular),
                  color: unread ? Design.Color.text.color : .listTone(.text, alpha: 0.72))
    }
}

extension ListRowContent {
    /// The line read aloud: sender, subject and preview, date, then its marks.
    var accessibilityText: String {
        var parts = [sender.string, line.string, date]
        if unread { parts.insert("Unread", at: 0) }
        if let topic { parts.append(topic) }
        if flagged { parts.append("Starred") }
        if hasAttachment { parts.append("Attachment") }
        if let account { parts.append(account.name) }
        return parts.joined(separator: ", ")
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
                        .padding(.trailing, ListRowDrawing.trailingInset - 2.75)
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
    let content: ListRowContent
    var pill: Double
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale

    var animatableData: Double {
        get { pill }
        set { pill = newValue }
    }

    var body: some View {
        let appearance = ListRowDrawing.appearance(colorScheme)
        let scale = displayScale
        Canvas { context, size in
            context.withCGContext { cg in
                ListRowDrawing.drawing(in: cg, appearance: appearance) {
                    ListRowDrawing.row(content, pill: pill, size: size, scale: scale)
                }
            }
        }
        .accessibilityElement()
        .accessibilityLabel(content.accessibilityText)
    }
}

/// The drawing of the list's lines, in AppKit and Core Graphics: a line's bitmap in the list and
/// the live line under the pointer draw the same pixels. Drawing a line this way costs a fraction
/// of a millisecond, where rendering it through SwiftUI took several.
enum ListRowDrawing {
    static let trailingInset: CGFloat = 14

    static func appearance(_ scheme: ColorScheme) -> NSAppearance {
        NSAppearance(named: scheme == .dark ? .darkAqua : .aqua) ?? NSAppearance.currentDrawing()
    }

    /// Runs `draw` with `cg` (top-left origin) as AppKit's current context, in `appearance`.
    static func drawing(in cg: CGContext, appearance: NSAppearance, _ draw: () -> Void) {
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: cg, flipped: true)
        appearance.performAsCurrentDrawingAppearance(draw)
        NSGraphicsContext.restoreGraphicsState()
    }

    /// The tinted (never solid) selection behind a highlighted line, as `ListRowChrome` lays it.
    static func highlight(emphasized: Bool, size: CGSize) {
        let rect = CGRect(origin: .zero, size: size).insetBy(dx: 4, dy: 1)
        let radius = Design.Radius.control.rawValue
        NSColor.listTone(.accent, alpha: emphasized ? 0.16 : 0.09).setFill()
        NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
    }

    /// A mail or chat row's columns, for a context of `scale` pixels per point.
    static func row(_ content: ListRowContent, pill: Double, size: CGSize, scale: CGFloat) {
        guard let cg = NSGraphicsContext.current?.cgContext else { return }
        let mid = size.height / 2
        if content.unread {
            Design.Color.accent.color.setFill()
            NSBezierPath(ovalIn: CGRect(x: ListMetrics.dotX - 3.5, y: mid - 3.5, width: 7, height: 7)).fill()
        }
        avatar(content, mid: mid, scale: scale)
        line(content.sender, x: ListMetrics.nameX, width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX,
             mid: mid)

        // Paperclip, star, topic, date and account tick, packed right to left; hidden items take no space.
        let right = size.width - trailingInset
        cg.saveGState()
        cg.setAlpha(1 - pill)
        if let account = content.account {
            account.color.setFill()
            NSBezierPath(roundedRect: CGRect(x: right - 2.5, y: mid - 6, width: 2.5, height: 12), xRadius: 1, yRadius: 1).fill()
        }
        var x = right - 2.5 - 8
        trailing(ListRowContent.text(content.date, font: ListRowContent.digits(.footnote, weight: content.unread ? .medium : .regular),
                                     color: content.unread ? Design.Color.text.color : Design.Color.muted.color),
                 right: x, mid: mid)
        x -= ListMetrics.dateWidth
        if let topic = content.topic, let color = content.topicColor {
            x -= 10
            x -= trailing(ListRowContent.text(topic, font: Design.font(.caption), color: Design.Color.muted.color),
                          right: x, mid: mid) + 5
            color.setFill()
            NSBezierPath(ovalIn: CGRect(x: x - 6, y: mid - 3, width: 6, height: 6)).fill()
            x -= 6
        }
        if content.flagged {
            x -= 10
            x -= symbol("star.fill", size: .micro, weight: .semibold, color: .star, right: x, mid: mid, scale: scale)
        }
        if content.hasAttachment {
            x -= 10
            x -= symbol("paperclip", size: .caption, weight: .medium, color: .faint, right: x, mid: mid, scale: scale)
        }
        cg.restoreGState()

        var reserved = right - x
        if content.showsActions {
            let pillWidth = CGFloat(content.actions.count) * ListRowActionButton.size.width + 2 * ListRowActionPill.inset
            reserved += (max(reserved, pillWidth - 8) - reserved) * pill
        }
        line(content.line, x: ListMetrics.subjectX, width: right - reserved - 14 - ListMetrics.subjectX, mid: mid)
    }

    /// "TODAY", or "● WAITING ON YOU 3": caption caps, 6 pt above the bottom edge.
    static func section(title: String, count: Int?, accent: Bool, size: CGSize) {
        let text = NSMutableAttributedString(string: title.uppercased(), attributes: [
            .font: Design.font(.caption, weight: .semibold),
            .foregroundColor: accent ? NSColor.listTone(.text, alpha: 0.75) : Design.Color.muted.color,
            .kern: 0.6,
        ])
        if let count {
            text.append(ListRowContent.text("   \(count)", font: ListRowContent.digits(.caption, weight: .medium),
                                            color: Design.Color.faint.color))
        }
        let height = text.size().height
        let top = size.height - 6 - height
        var x = ListMetrics.senderX
        if accent {
            Design.Color.waiting.color.setFill()
            NSBezierPath(ovalIn: CGRect(x: x, y: top + height / 2 - 3, width: 6, height: 6)).fill()
            x += 6 + 7
        }
        text.draw(with: CGRect(x: x, y: top, width: max(size.width - x, 0), height: height),
                  options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    /// Initials on a tint of the person's color, or their picture once one is known.
    private static func avatar(_ content: ListRowContent, mid: CGFloat, scale: CGFloat) {
        let side = ListMetrics.avatarSize
        let avatar = CGRect(x: ListMetrics.senderX, y: mid - side / 2, width: side, height: side)
        content.avatarColor.withAlphaComponent(0.15).setFill()
        NSBezierPath(ovalIn: avatar).fill()
        if let photo = content.photo {
            if let image = circle(of: photo, scale: scale) { draw(image, in: avatar) }
        } else {
            let initials = ListRowContent.text(content.initials, font: Design.font(.body, weight: .semibold),
                                               color: content.avatarColor)
            let size = initials.size()
            initials.draw(at: CGPoint(x: avatar.midX - size.width / 2, y: avatar.midY - size.height / 2))
        }
    }

    /// `photo` filling the avatar's circle, at `scale`: made once per picture, since drawing the
    /// picture itself decodes and color-converts it every time.
    private static func circle(of photo: NSImage, scale: CGFloat) -> CGImage? {
        let space = currentSpace
        if let made = circles.object(forKey: photo), made.scale == scale, made.space == space { return made.image }
        let side = ListMetrics.avatarSize
        let rect = CGRect(x: 0, y: 0, width: side, height: side)
        guard let image = bitmap(size: rect.size, scale: scale, space: space, draw: {
            NSBezierPath(ovalIn: rect).addClip()
            photo.draw(in: filling(rect, with: photo.size), from: .zero, operation: .sourceOver, fraction: 1,
                       respectFlipped: true, hints: [.interpolation: NSNumber(value: NSImageInterpolation.high.rawValue)])
        }) else { return nil }
        circles.setObject(ScaledImage(image: image, scale: scale, space: space), forKey: photo)
        return image
    }

    private final class ScaledImage {
        let image: CGImage
        let scale: CGFloat
        let space: CGColorSpace

        init(image: CGImage, scale: CGFloat, space: CGColorSpace) {
            self.image = image
            self.scale = scale
            self.space = space
        }
    }

    /// Avatar circles by picture, as long as the picture lives.
    private static let circles = NSMapTable<NSImage, ScaledImage>.weakToStrongObjects()

    private static let sRGB = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()

    /// The current context's color space: images drawn into it in that space are copied as they are.
    private static var currentSpace: CGColorSpace { NSGraphicsContext.current?.cgContext.colorSpace ?? sRGB }

    /// `draw` (top-left origin, in points) rendered into an image at `scale`, in `space`: the
    /// screen's, for a layer's contents, so Core Animation shows it without converting every pixel.
    static func bitmap(size: CGSize, scale: CGFloat, space: CGColorSpace?, draw: () -> Void) -> CGImage? {
        let width = Int((size.width * scale).rounded(.up)), height = Int((size.height * scale).rounded(.up))
        guard width > 0, height > 0,
              let cg = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                 space: space ?? sRGB,
                                 bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        else { return nil }
        cg.translateBy(x: 0, y: CGFloat(height))
        cg.scaleBy(x: scale, y: -scale)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: cg, flipped: true)
        draw()
        NSGraphicsContext.restoreGraphicsState()
        return cg.makeImage()
    }

    /// Draws `image` upright in `rect` of the current (top-left origin) context.
    private static func draw(_ image: CGImage, in rect: CGRect) {
        guard let cg = NSGraphicsContext.current?.cgContext else { return }
        cg.saveGState()
        cg.translateBy(x: rect.minX, y: rect.maxY)
        cg.scaleBy(x: 1, y: -1)
        cg.interpolationQuality = .high
        cg.draw(image, in: CGRect(origin: .zero, size: rect.size))
        cg.restoreGState()
    }

    /// The height of a line of row text, centered on the row.
    private static let lineHeight = ListRowContent.text("Ag", font: Design.font(.body, weight: .semibold), color: .labelColor)
        .size().height

    /// One line of `text` from `x`, cut with an ellipsis at `width`.
    private static func line(_ text: NSAttributedString, x: CGFloat, width: CGFloat, mid: CGFloat) {
        guard width > 0 else { return }
        text.draw(with: CGRect(x: x, y: mid - lineHeight / 2, width: width, height: lineHeight),
                  options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    /// Draws `text` ending at `right`, centered on `mid`; returns its width.
    @discardableResult
    private static func trailing(_ text: NSAttributedString, right: CGFloat, mid: CGFloat) -> CGFloat {
        let size = text.size()
        text.draw(at: CGPoint(x: right - size.width, y: mid - size.height / 2))
        return size.width
    }

    /// Draws a symbol ending at `right`, centered on `mid`; returns its width.
    private static func symbol(_ name: String, size: Design.TextSize, weight: NSFont.Weight, color: Design.Color,
                               right: CGFloat, mid: CGFloat, scale: CGFloat) -> CGFloat {
        let dark = NSAppearance.currentDrawing().bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let space = currentSpace
        let key = SymbolKey(name: name, size: size, weight: weight, color: color, dark: dark, scale: scale,
                            space: space.name.map { $0 as String } ?? "")
        let symbol: (image: CGImage?, size: CGSize)
        if let cached = symbols[key] {
            symbol = cached
        } else {
            let configuration = NSImage.SymbolConfiguration(pointSize: size.rawValue, weight: weight)
                .applying(NSImage.SymbolConfiguration(paletteColors: [color.color]))
            let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                .withSymbolConfiguration(configuration) ?? NSImage()
            let rect = CGRect(origin: .zero, size: image.size)
            symbol = (bitmap(size: image.size, scale: scale, space: space) {
                image.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
            }, image.size)
            symbols[key] = symbol
        }
        if let image = symbol.image {
            draw(image, in: CGRect(x: right - symbol.size.width, y: mid - symbol.size.height / 2,
                                   width: symbol.size.width, height: symbol.size.height))
        }
        return symbol.size.width
    }

    private struct SymbolKey: Hashable {
        let name: String
        let size: Design.TextSize
        let weight: NSFont.Weight
        let color: Design.Color
        let dark: Bool
        let scale: CGFloat
        let space: String
    }

    /// Symbols drawn once per appearance, scale and color space.
    private static var symbols: [SymbolKey: (image: CGImage?, size: CGSize)] = [:]

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
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let appearance = ListRowDrawing.appearance(colorScheme)
        Canvas { context, size in
            context.withCGContext { cg in
                ListRowDrawing.drawing(in: cg, appearance: appearance) {
                    ListRowDrawing.section(title: title, count: count, accent: accent, size: size)
                }
            }
        }
        .frame(height: ListMetrics.sectionHeight)
        .accessibilityElement()
        .accessibilityLabel(count.map { "\(title), \($0)" } ?? title)
        .accessibilityAddTraits(.isHeader)
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
