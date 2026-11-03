import AppKit

extension NSParagraphStyle {
    /// Single line, ellipsis at the end: attributed label text ignores the field's own line break mode.
    @MainActor static let truncatingTail: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()
}

extension NSColor {
    /// `#RRGGBB` in sRGB.
    convenience init(hex: String) {
        let value = UInt32(hex.dropFirst(), radix: 16) ?? 0
        self.init(srgbRed: CGFloat((value >> 16) & 0xFF) / 255, green: CGFloat((value >> 8) & 0xFF) / 255,
                  blue: CGFloat(value & 0xFF) / 255, alpha: 1)
    }

    /// One color for light appearances and another for dark ones.
    static func dynamic(light: String, dark: String) -> NSColor {
        NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? NSColor(hex: dark) : NSColor(hex: light)
        }
    }
}

/// Muted topic colors shared by the list (AppKit) and the reader (CSS). Known topics keep fixed hues.
enum TagPalette {
    private static let known: [String: (String, String)] = [
        "work": ("#3B82F6", "#60A5FA"), "education": ("#A855F7", "#C084FC"), "finance": ("#10B981", "#34D399"),
        "shopping": ("#F59E0B", "#FBBF24"), "tech": ("#06B6D4", "#22D3EE"), "travel": ("#EC4899", "#F472B6"),
        "food": ("#F97316", "#FB923C"), "health": ("#14B8A6", "#2DD4BF"), "social": ("#8B5CF6", "#A78BFA"),
    ]
    private static let fallback: [(String, String)] = [("#64748B", "#94A3B8"), ("#6366F1", "#818CF8"),
                                                       ("#84CC16", "#A3E635"), ("#E11D48", "#FB7185")]

    private static func pair(for label: String) -> (String, String) {
        let key = label.lowercased()
        if let pair = known[key] { return pair }
        var hash: UInt64 = 1469598103934665603
        for byte in key.utf8 { hash = (hash ^ UInt64(byte)) &* 1099511628211 }
        return fallback[Int(hash % UInt64(fallback.count))]
    }

    /// Light-appearance hex, for CSS (the reader derives its dark variant).
    static func hex(for label: String) -> String { pair(for: label).0 }

    static func color(for label: String) -> NSColor {
        let (light, dark) = pair(for: label)
        return .dynamic(light: light, dark: dark)
    }
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
    static let avatarSize: CGFloat = 22
    static let senderWidth: CGFloat = 196
    static let nameX: CGFloat = senderX + avatarSize + 9
    static let subjectX: CGFloat = senderX + senderWidth + 16
    static let dateWidth: CGFloat = 62
}

/// The house motion: one easing curve, a few durations, and every animation asks for ProMotion's
/// full rate. Animations are explicit layer animations that start from what is on screen, so a
/// reversal mid-flight (hover in, then out) turns around instead of jumping.
@MainActor
enum Motion {
    /// The house easing curve: quick start, long soft landing.
    static let ease = CAMediaTimingFunction(controlPoints: 0.2, 0.9, 0.25, 1)
    static let quick: TimeInterval = 0.14
    static let standard: TimeInterval = 0.28
    /// 120 Hz on ProMotion displays; Core Animation otherwise caps animations at 60.
    static let frameRate = CAFrameRateRange(minimum: 80, maximum: 120, preferred: 120)

    /// An eased animation at the house frame rate.
    static func basic(_ keyPath: String, from: Any?, to: Any?, duration: TimeInterval) -> CABasicAnimation {
        let animation = CABasicAnimation(keyPath: keyPath)
        animation.fromValue = from
        animation.toValue = to
        animation.duration = duration
        animation.timingFunction = ease
        animation.preferredFrameRateRange = frameRate
        return animation
    }

    /// A spring at the house frame rate that runs until it settles.
    static func spring(_ keyPath: String, from: Any?, to: Any?, stiffness: CGFloat, damping: CGFloat) -> CASpringAnimation {
        let animation = CASpringAnimation(keyPath: keyPath)
        animation.fromValue = from
        animation.toValue = to
        animation.mass = 1
        animation.stiffness = stiffness
        animation.damping = damping
        animation.duration = animation.settlingDuration
        animation.preferredFrameRateRange = frameRate
        return animation
    }

    /// Runs `animations`, then `completion` once everything they added has finished or been replaced.
    static func group(_ animations: () -> Void, completion: @escaping @MainActor () -> Void) {
        CATransaction.begin()
        let handler: @Sendable () -> Void = { MainActor.assumeIsolated { completion() } }
        CATransaction.setCompletionBlock(handler)
        animations()
        CATransaction.commit()
    }

    /// Moves `layer`'s `keyPaths` to the model values `change` sets, animating each from its on-screen
    /// value; a key already at its target just drops any animation still heading elsewhere.
    static func animate(_ layer: CALayer, _ keyPaths: [String], duration: TimeInterval = quick, _ change: () -> Void) {
        let shown = keyPaths.map { key in
            ((layer.animation(forKey: key) != nil ? layer.presentation() : nil) ?? layer).value(forKeyPath: key)
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        change()
        CATransaction.commit()
        for (key, from) in zip(keyPaths, shown) {
            let to = layer.value(forKeyPath: key)
            if (from as AnyObject?)?.isEqual(to) ?? (to == nil) {
                layer.removeAnimation(forKey: key)
            } else {
                layer.add(basic(key, from: from, to: to, duration: duration), forKey: key)
            }
        }
    }

    /// Fades a view to `alpha`; `alphaValue` holds the target at once. Views without a layer just jump.
    static func fade(_ view: NSView, to alpha: CGFloat, duration: TimeInterval = quick) {
        guard let layer = view.layer else { view.alphaValue = alpha; return }
        animate(layer, ["opacity"], duration: duration) {
            view.alphaValue = alpha
            layer.opacity = Float(alpha)
        }
    }

    /// Slides a layer-backed view's content by `dx`/`dy` back to rest, without touching its frame.
    static func slideIn(_ view: NSView, dx: CGFloat = 0, dy: CGFloat = 0, duration: TimeInterval = standard) {
        guard let layer = view.layer else { return }
        let animation = spring("transform", from: CATransform3DMakeTranslation(dx, dy, 0), to: CATransform3DIdentity,
                               stiffness: 320, damping: 34)
        animation.duration = max(duration, animation.settlingDuration)
        layer.add(animation, forKey: "slideIn")
    }

    /// Scales a layer-backed view's content from `scale` back to rest about its center, without
    /// touching its frame: below 1 it spreads out from the middle, above 1 it settles inward.
    static func unfold(_ view: NSView, from scale: CGFloat) {
        guard let layer = view.layer else { return }
        layer.add(spring("transform", from: centered(scale, in: layer), to: CATransform3DIdentity, stiffness: 300, damping: 32),
                  forKey: "unfold")
    }

    /// A scale about the layer's center. Transforms pivot on the anchor point, so shift the pivot to
    /// the center, which is the same point whichever way the layer's geometry is flipped.
    static func centered(_ scale: CGFloat, in layer: CALayer) -> CATransform3D {
        let dx = layer.bounds.width * (0.5 - layer.anchorPoint.x)
        let dy = layer.bounds.height * (0.5 - layer.anchorPoint.y)
        var transform = CATransform3DMakeTranslation(dx, dy, 0)
        transform = CATransform3DScale(transform, scale, scale, 1)
        return CATransform3DTranslate(transform, -dx, -dy, 0)
    }
}

/// Height Auto Layout gives a one-line label in `font`.
@MainActor private func labelLineHeight(_ font: NSFont) -> CGFloat {
    let label = NSTextField(labelWithString: "Ag")
    label.font = font
    return label.intrinsicContentSize.height
}

/// Puts `view`'s alignment rect of `size` at `x`, centered in `height`, pixel-aligned in its
/// superview: what leading, centerY, width and height constraints would produce.
@MainActor private func place(_ view: NSView, x: CGFloat, size: NSSize, height: CGFloat) {
    let rect = view.frame(forAlignmentRect: NSRect(x: x, y: (height - size.height) / 2, width: size.width, height: size.height))
    view.frame = view.superview?.backingAlignedRect(rect, options: .alignAllEdgesNearest) ?? rect
}

// MARK: - Row view

/// Cells that react to the pointer hovering their row.
@MainActor
protocol HoverableCell: AnyObject {
    func setHovering(_ hovering: Bool, animated: Bool)
}

/// Row with a soft, fading hover wash and a tinted (never solid) selection. Hover is pushed in by
/// `ConversationTableView`; rows don't track the pointer themselves, since a row's own tracking
/// area misses its exit when it scrolls or reloads under a still pointer.
final class ListRowView: NSTableRowView {
    private let wash = FillView()
    private var hovering = false

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wash.fill = NSColor.labelColor.withAlphaComponent(0.045)
        wash.radius = 8
        wash.alphaValue = 0
        addSubview(wash, positioned: .below, relativeTo: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// Forwarded even when unchanged: a cell reloaded in place resets itself and must be re-told.
    func setHovering(_ value: Bool) {
        (view(atColumn: 0) as? HoverableCell)?.setHovering(value, animated: true)
        guard value != hovering else { return }
        hovering = value
        Motion.fade(wash, to: washAlpha)
    }

    private var washAlpha: CGFloat { hovering && !isSelected ? 1 : 0 }

    override func layout() {
        super.layout()
        wash.frame = bounds.insetBy(dx: 4, dy: 1)
    }

    /// A cell swapped in by a reload takes on the row's current hover state at once.
    override func didAddSubview(_ subview: NSView) {
        super.didAddSubview(subview)
        (subview as? HoverableCell)?.setHovering(hovering, animated: false)
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        hovering = false
        wash.layer?.removeAllAnimations()
        wash.alphaValue = 0
    }

    override var isSelected: Bool {
        didSet {
            wash.layer?.removeAnimation(forKey: "opacity")
            wash.alphaValue = washAlpha
        }
    }

    override var interiorBackgroundStyle: NSView.BackgroundStyle { .normal }

    override func drawSelection(in dirtyRect: NSRect) {
        let alpha: CGFloat = isEmphasized ? 0.16 : 0.09
        NSColor.controlAccentColor.withAlphaComponent(alpha).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 4, dy: 1), xRadius: 8, yRadius: 8).fill()
    }
}

/// A rounded rectangle of one color, drawn by its layer; animatable through `alphaValue`.
final class FillView: NSView {
    var fill: NSColor = .clear { didSet { needsDisplay = true } }
    var radius: CGFloat = 0 { didSet { needsDisplay = true } }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        guard let layer else { return }
        layer.cornerRadius = radius
        effectiveAppearance.performAsCurrentDrawingAppearance { layer.backgroundColor = fill.cgColor }
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

// MARK: - Conversation row

/// One line, five quiet columns: unread dot · avatar and sender · subject and preview · topic · date and account.
/// Hovering cross-fades the trailing metadata into a floating action pill.
final class ConversationRowCell: NSTableCellView, HoverableCell {
    static let identifier = NSUserInterfaceItemIdentifier("ConversationRowCell")

    var onAction: ((ConversationAction) -> Void)?

    private static let textHeight = labelLineHeight(.systemFont(ofSize: 13))
    private static let dateHeight = labelLineHeight(.monospacedDigitSystemFont(ofSize: 12, weight: .regular))
    private static let dateFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular)
    private static let unreadDateFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .medium)
    private static let paperclipImage = NSImage(systemSymbolName: "paperclip", accessibilityDescription: "Attachment")
    private static let starredImage = NSImage(systemSymbolName: "star.fill", accessibilityDescription: "Starred")
    private static let markReadImage = NSImage(systemSymbolName: "envelope.open", accessibilityDescription: nil)
    private static let markUnreadImage = NSImage(systemSymbolName: "envelope.badge", accessibilityDescription: nil)
    private static let starImage = NSImage(systemSymbolName: "star", accessibilityDescription: nil)
    private static let unstarImage = NSImage(systemSymbolName: "star.slash", accessibilityDescription: nil)

    private let unreadDot = DotView()
    private let avatar = AvatarView()
    /// Whose picture `avatar` stands for, so a late one only lands on the row it belongs to.
    private var avatarKey = ""
    private let sender = NSTextField(labelWithString: "")
    private let line = NSTextField(labelWithString: "")
    /// Paperclip, star, topic, date and account tick: one layer, so hover fades them together.
    private let trailing = NSView()
    private let paperclip = NSImageView()
    private let star = NSImageView()
    private let topic = TopicLabel()
    private let date = NSTextField(labelWithString: "")
    private let accountTick = FillView()
    private let pill = ActionPill()
    private var readButton: HoverActionButton!
    private var starButton: HoverActionButton!
    private var archiveButton: HoverActionButton!
    private var hovering = false

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier

        sender.lineBreakMode = .byTruncatingTail
        line.lineBreakMode = .byTruncatingTail
        date.alignment = .right
        date.font = Self.dateFont

        paperclip.image = Self.paperclipImage
        paperclip.symbolConfiguration = .init(pointSize: 11, weight: .medium)
        paperclip.contentTintColor = .tertiaryLabelColor
        star.image = Self.starredImage
        star.symbolConfiguration = .init(pointSize: 10, weight: .semibold)
        star.contentTintColor = .systemYellow
        accountTick.radius = 1
        trailing.wantsLayer = true
        for view in [paperclip, star, topic, date, accountTick] as [NSView] { trailing.addSubview(view) }

        readButton = HoverActionButton(symbol: "envelope.badge", label: "Mark as Unread")
        starButton = HoverActionButton(symbol: "star", label: "Star")
        archiveButton = HoverActionButton(symbol: "archivebox", label: "Archive")
        let trashButton = HoverActionButton(symbol: "trash", label: "Move to Trash")
        for (button, action) in [(archiveButton!, ConversationAction.archive), (trashButton, .trash),
                                 (readButton!, .toggleRead), (starButton!, .toggleStar)] {
            button.handler = { [weak self] in self?.onAction?(action) }
            pill.add(button)
        }
        pill.alphaValue = 0
        pill.isHidden = true

        for view in [unreadDot, avatar, sender, line, trailing, pill] as [NSView] { addSubview(view) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// `snippet` is display-ready (history trimmed, whitespace collapsed) and `row` precomputed off
    /// the main thread, so this only copies values into views.
    func configure(_ row: ConversationRow, snippet: String?, archivable: Bool,
                   account: (name: String, color: NSColor)?, tag: ConversationTag?, photo: NSImage?) {
        let unread = row.isUnread
        unreadDot.isHidden = !unread
        avatarKey = row.avatarKey
        avatar.configure(initials: row.avatarInitials, color: Avatar.color(for: row.avatarKey), photo: photo)
        star.isHidden = !row.isFlagged
        paperclip.isHidden = !row.hasAttachment
        accountTick.fill = account?.color ?? .clear
        accountTick.toolTip = account?.name
        topic.isHidden = tag?.labels.first == nil
        if let label = tag?.labels.first { topic.configure(label, color: TagPalette.color(for: label)) }
        date.stringValue = row.date
        date.textColor = unread ? .labelColor : .secondaryLabelColor
        date.font = unread ? Self.unreadDateFont : Self.dateFont

        let name = NSMutableAttributedString(string: row.participants, attributes: [
            .font: NSFont.systemFont(ofSize: 13, weight: unread ? .semibold : .regular),
            .foregroundColor: unread ? NSColor.labelColor : NSColor.labelColor.withAlphaComponent(0.72),
        ])
        if row.count > 1 {
            name.append(NSAttributedString(string: "  \(row.count)", attributes: [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .medium),
                .foregroundColor: NSColor.tertiaryLabelColor,
            ]))
        }
        name.addAttribute(.paragraphStyle, value: NSParagraphStyle.truncatingTail, range: NSRange(location: 0, length: name.length))
        sender.attributedStringValue = name

        let text = NSMutableAttributedString()
        if !row.subjectPrefix.isEmpty {
            text.append(NSAttributedString(string: row.subjectPrefix + " ", attributes: [
                .font: NSFont.systemFont(ofSize: 12), .foregroundColor: NSColor.tertiaryLabelColor,
            ]))
        }
        text.append(NSAttributedString(string: row.subjectTitle, attributes: [
            .font: NSFont.systemFont(ofSize: 13, weight: unread ? .medium : .regular),
            .foregroundColor: unread ? NSColor.labelColor : NSColor.labelColor.withAlphaComponent(0.8),
        ]))
        if let snippet, !snippet.isEmpty {
            text.append(NSAttributedString(string: "   " + snippet, attributes: [
                .font: NSFont.systemFont(ofSize: 13), .foregroundColor: NSColor.secondaryLabelColor.withAlphaComponent(0.75),
            ]))
        }
        text.addAttribute(.paragraphStyle, value: NSParagraphStyle.truncatingTail, range: NSRange(location: 0, length: text.length))
        line.attributedStringValue = text

        readButton.image = unread ? Self.markReadImage : Self.markUnreadImage
        readButton.toolTip = unread ? "Mark as Read" : "Mark as Unread"
        starButton.image = row.isFlagged ? Self.unstarImage : Self.starImage
        starButton.toolTip = row.isFlagged ? "Unstar" : "Star"
        archiveButton.isHidden = !archivable
        needsLayout = true
    }

    /// A picture that arrived after the row was configured.
    func showPhoto(_ photo: NSImage, for key: String) {
        guard key == avatarKey else { return }
        avatar.show(photo, animated: true)
    }

    override func setFrameSize(_ newSize: NSSize) {
        if newSize != frame.size { needsLayout = true }
        super.setFrameSize(newSize)
    }

    /// Fixed columns on the left; the trailing group packs right to left, hidden items taking no
    /// space; the subject line ends before it, and before the action pill while hovering.
    override func layout() {
        super.layout()
        let width = bounds.width, height = bounds.height
        place(unreadDot, x: ListMetrics.dotX - 3.5, size: NSSize(width: 7, height: 7), height: height)
        place(avatar, x: ListMetrics.senderX, size: NSSize(width: ListMetrics.avatarSize, height: ListMetrics.avatarSize),
              height: height)
        place(sender, x: ListMetrics.nameX, size: NSSize(width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX,
                                                         height: Self.textHeight), height: height)

        let end = width - 14
        let tickX = end - 2.5
        let dateX = tickX - 8 - ListMetrics.dateWidth
        var start = dateX
        var packed: [(view: NSView, x: CGFloat, size: NSSize)] = []
        for view in [topic, star, paperclip] as [NSView] where !view.isHidden {
            let size = view === topic ? NSSize(width: topic.fittingWidth, height: height) : view.intrinsicContentSize
            start -= 10 + size.width
            packed.append((view, start, size))
        }
        trailing.frame = NSRect(x: start, y: 0, width: end - start, height: height)
        for item in packed { place(item.view, x: item.x - start, size: item.size, height: height) }
        place(date, x: dateX - start, size: NSSize(width: ListMetrics.dateWidth, height: Self.dateHeight), height: height)
        place(accountTick, x: tickX - start, size: NSSize(width: 2.5, height: 12), height: height)

        let pillWidth = pill.fittingWidth
        let pillX = width - 10 - pillWidth
        place(pill, x: pillX, size: NSSize(width: pillWidth, height: ActionPill.height), height: height)

        let lineEnd = hovering ? min(start - 14, pillX - 10) : start - 14
        place(line, x: ListMetrics.subjectX, size: NSSize(width: max(0, lineEnd - ListMetrics.subjectX), height: Self.textHeight),
              height: height)
    }

    func setHovering(_ hovering: Bool, animated: Bool) {
        if hovering != self.hovering {
            self.hovering = hovering
            needsLayout = true
        } else if animated {
            return
        }
        guard animated else {
            for view in [pill, trailing] as [NSView] { view.layer?.removeAllAnimations() }
            pill.isHidden = !hovering
            pill.alphaValue = hovering ? 1 : 0
            trailing.alphaValue = hovering ? 0 : 1
            return
        }
        if hovering { pill.isHidden = false }
        Motion.group({
            Motion.fade(pill, to: hovering ? 1 : 0)
            Motion.fade(trailing, to: hovering ? 0 : 1)
        }, completion: { [weak self] in
            guard let self, !self.hovering else { return }
            self.pill.isHidden = true
        })
        if hovering { Motion.slideIn(pill, dx: 6, duration: 0.2) }
    }

    /// A recycled cell must not carry the previous row's action pill.
    override func prepareForReuse() {
        super.prepareForReuse()
        setHovering(false, animated: false)
    }
}

// MARK: - Bundle shelf

/// The Smart Inbox's automated mail as a shelf of equal tiles above the people.
final class BundleShelfCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("BundleShelfCell")
    var onOpen: ((MailCategory) -> Void)?
    private let stack = NSStackView()
    private var tiles: [BundleTile] = []

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        stack.orientation = .horizontal
        stack.distribution = .fillEqually
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(_ bundles: [(MailCategory, [Conversation])], own: Set<String>) {
        while tiles.count < bundles.count {
            let tile = BundleTile()
            tile.onOpen = { [weak self] category in self?.onOpen?(category) }
            tiles.append(tile)
            stack.addArrangedSubview(tile)
        }
        for (index, tile) in tiles.enumerated() {
            tile.isHidden = index >= bundles.count
            if index < bundles.count { tile.configure(bundles[index].0, conversations: bundles[index].1, own: own) }
        }
    }

    /// `point` is the pointer in this cell's coordinates, or nil once it has left the shelf.
    func track(_ point: NSPoint?) {
        for tile in tiles {
            let inside = point.map { !tile.isHidden && tile.bounds.contains(tile.convert($0, from: self)) } ?? false
            tile.setHovering(inside)
        }
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        track(nil)
    }
}

/// One shelf tile: symbol, title and senders on the left, the unread count on the right.
/// Lifts on hover and dips when pressed.
final class BundleTile: NSView {
    var onOpen: ((MailCategory) -> Void)?
    private var category: MailCategory = .notification
    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let senders = NSTextField(labelWithString: "")
    private let badge = NSTextField(labelWithString: "")
    private let badgeBack = FillView()
    private var hovering = false
    /// The tile's frame in its window when the press began.
    private var pressFrame: NSRect?

    static func tint(_ category: MailCategory) -> NSColor {
        switch category {
        case .people: .systemBlue
        case .notification: .systemTeal
        case .newsletter: .systemOrange
        case .invitation: .systemPink
        }
    }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 11
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 1
        icon.symbolConfiguration = .init(pointSize: 15, weight: .medium)
        title.font = .systemFont(ofSize: 13, weight: .semibold)
        senders.font = .systemFont(ofSize: 12)
        senders.textColor = .secondaryLabelColor
        senders.lineBreakMode = .byTruncatingTail
        senders.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        badge.font = .monospacedDigitSystemFont(ofSize: 11, weight: .semibold)
        badge.textColor = .controlAccentColor
        badge.alignment = .center
        badgeBack.fill = NSColor.controlAccentColor.withAlphaComponent(0.12)
        badgeBack.radius = 9
        for view in [icon, title, senders, badgeBack, badge] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 22),
            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 9),
            title.bottomAnchor.constraint(equalTo: centerYAnchor, constant: 1),
            senders.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            senders.topAnchor.constraint(equalTo: centerYAnchor, constant: 1),
            senders.trailingAnchor.constraint(lessThanOrEqualTo: badgeBack.leadingAnchor, constant: -8),
            title.trailingAnchor.constraint(lessThanOrEqualTo: badgeBack.leadingAnchor, constant: -8),
            badgeBack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            badgeBack.centerYAnchor.constraint(equalTo: centerYAnchor),
            badgeBack.heightAnchor.constraint(equalToConstant: 18),
            badgeBack.widthAnchor.constraint(equalTo: badge.widthAnchor, constant: 14),
            badge.centerXAnchor.constraint(equalTo: badgeBack.centerXAnchor),
            badge.centerYAnchor.constraint(equalTo: badgeBack.centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(_ category: MailCategory, conversations: [Conversation], own: Set<String>) {
        self.category = category
        icon.image = NSImage(systemSymbolName: category.symbol, accessibilityDescription: nil)
        icon.contentTintColor = Self.tint(category)
        title.stringValue = category.title
        let unread = conversations.filter(\.isUnread).count
        badge.stringValue = unread > 0 ? unread.formatted() : conversations.count.formatted()
        badge.textColor = unread > 0 ? .controlAccentColor : .secondaryLabelColor
        badgeBack.fill = unread > 0 ? NSColor.controlAccentColor.withAlphaComponent(0.12) : NSColor.labelColor.withAlphaComponent(0.06)
        var seen = Set<String>()
        var names: [String] = []
        for conversation in conversations {
            guard let name = ConversationRow.counterpart(conversation, own: own)?.shortName,
                  seen.insert(name.lowercased()).inserted else { continue }
            names.append(name)
            if names.count == 4 { break }
        }
        senders.stringValue = names.joined(separator: ", ")
        toolTip = "\(category.title): \(conversations.count) conversations, \(unread) unread"
        needsDisplay = true
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(hovering ? 0.065 : 0.035).cgColor
            layer?.borderColor = NSColor.labelColor.withAlphaComponent(hovering ? 0.1 : 0.06).cgColor
        }
    }

    /// Driven by `BundleShelfCell.track`, so a scroll under a still pointer can't strand the lift.
    func setHovering(_ value: Bool) {
        guard value != hovering else { return }
        hovering = value
        guard let layer else { return }
        Motion.animate(layer, ["backgroundColor", "borderColor"], duration: 0.18) { updateLayer() }
    }

    /// The tile is one button: its icon and labels never take the click themselves.
    override func hitTest(_ point: NSPoint) -> NSView? {
        super.hitTest(point) == nil ? nil : self
    }

    /// Like the list rows around it, the click that brings the window forward also opens the tile.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        pressFrame = convert(bounds, to: nil)
        press(scale: 0.97)
    }

    /// Judged against where the press began: a list update can swap this tile out mid-click.
    override func mouseUp(with event: NSEvent) {
        press(scale: 1)
        defer { pressFrame = nil }
        if let pressFrame, pressFrame.contains(event.locationInWindow) { onOpen?(category) }
    }

    /// Scales about the center; layer-backed views anchor at their corner, so translate around it.
    private func press(scale: CGFloat) {
        guard let layer else { return }
        let w = bounds.width / 2, h = bounds.height / 2
        var transform = CATransform3DMakeTranslation(w, h, 0)
        transform = CATransform3DScale(transform, scale, scale, 1)
        transform = CATransform3DTranslate(transform, -w, -h, 0)
        let spring = Motion.spring("transform", from: layer.presentation()?.transform ?? layer.transform, to: transform,
                                   stiffness: 500, damping: 30)
        layer.transform = transform
        layer.add(spring, forKey: "press")
    }
}

// MARK: - Section header

final class SectionHeaderCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("SectionHeaderCell")
    private static let textHeight = labelLineHeight(.systemFont(ofSize: 11, weight: .semibold))
    private let label = NSTextField(labelWithString: "")
    private let dot = DotView()

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        dot.color = .systemOrange
        addSubview(label)
        addSubview(dot)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(_ title: String, count: Int?, accent: Bool) {
        let text = NSMutableAttributedString(string: title.uppercased(), attributes: [
            .font: NSFont.systemFont(ofSize: 11, weight: .semibold),
            .foregroundColor: accent ? NSColor.labelColor.withAlphaComponent(0.75) : NSColor.secondaryLabelColor,
            .kern: 0.6,
        ])
        if let count {
            text.append(NSAttributedString(string: "   \(count)", attributes: [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .medium),
                .foregroundColor: NSColor.tertiaryLabelColor,
            ]))
        }
        label.attributedStringValue = text
        dot.isHidden = !accent
        needsLayout = true
    }

    override func setFrameSize(_ newSize: NSSize) {
        if newSize != frame.size { needsLayout = true }
        super.setFrameSize(newSize)
    }

    /// The label sits 6 pt above the bottom edge, after the accent dot when there is one.
    override func layout() {
        super.layout()
        let height = Self.textHeight
        let x = dot.isHidden ? ListMetrics.senderX : ListMetrics.senderX + 6 + 7
        let y = isFlipped ? bounds.height - 6 - height : 6
        let rect = label.frame(forAlignmentRect: NSRect(x: x, y: y, width: max(0, bounds.width - x), height: height))
        label.frame = backingAlignedRect(rect, options: .alignAllEdgesNearest)
        dot.frame = backingAlignedRect(NSRect(x: ListMetrics.senderX, y: y + (height - 6) / 2, width: 6, height: 6),
                                       options: .alignAllEdgesNearest)
    }
}

// MARK: - Small views

/// "● Work": a topic as a tiny colored dot and quiet text, no pill.
final class TopicLabel: NSView {
    private static let textHeight = labelLineHeight(.systemFont(ofSize: 11.5))
    private let dot = DotView()
    private let label = NSTextField(labelWithString: "")
    private var textWidth: CGFloat = 0
    /// Dot, 5 pt gap, text.
    var fittingWidth: CGFloat { 11 + textWidth }

    init() {
        super.init(frame: .zero)
        label.font = .systemFont(ofSize: 11.5)
        label.textColor = .secondaryLabelColor
        addSubview(dot)
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(_ text: String, color: NSColor) {
        dot.color = color
        guard text != label.stringValue else { return }
        label.stringValue = text
        textWidth = label.intrinsicContentSize.width
        needsLayout = true
    }

    override func setFrameSize(_ newSize: NSSize) {
        if newSize != frame.size { needsLayout = true }
        super.setFrameSize(newSize)
    }

    override func layout() {
        super.layout()
        place(dot, x: 0, size: NSSize(width: 6, height: 6), height: bounds.height)
        place(label, x: 11, size: NSSize(width: textWidth, height: Self.textHeight), height: bounds.height)
    }
}

/// Floating capsule holding the row's quick actions, laid out in a row; hidden buttons take no space.
final class ActionPill: NSView {
    static let height: CGFloat = 28
    private static let inset: CGFloat = 3
    private var buttons: [HoverActionButton] = []

    /// Width that fits the visible buttons.
    var fittingWidth: CGFloat {
        CGFloat(buttons.count(where: { !$0.isHidden })) * HoverActionButton.size.width + 2 * Self.inset
    }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 1
        layer?.shadowOpacity = 1
        layer?.shadowRadius = 4
        layer?.shadowOffset = CGSize(width: 0, height: -1)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func add(_ button: HoverActionButton) {
        buttons.append(button)
        addSubview(button)
    }

    override func setFrameSize(_ newSize: NSSize) {
        if newSize != frame.size { needsLayout = true }
        super.setFrameSize(newSize)
    }

    override func layout() {
        super.layout()
        var x = Self.inset
        for button in buttons where !button.isHidden {
            place(button, x: x, size: HoverActionButton.size, height: bounds.height)
            x += HoverActionButton.size.width
        }
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
            layer?.borderColor = NSColor.separatorColor.cgColor
            layer?.shadowColor = NSColor.black.withAlphaComponent(0.08).cgColor
        }
    }
}

final class HoverActionButton: NSButton {
    static let size = NSSize(width: 28, height: 26)
    var handler: (() -> Void)?

    init(symbol: String, label: String) {
        super.init(frame: .zero)
        image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        symbolConfiguration = .init(pointSize: 12, weight: .regular)
        toolTip = label
        isBordered = false
        contentTintColor = .secondaryLabelColor
        setButtonType(.momentaryChange)
        target = self
        action = #selector(fire)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private var hoverArea: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                  owner: self)
        addTrackingArea(area)
        hoverArea = area
    }

    /// The pill hides on row exit, which may skip this button's own exit; start plain next time.
    override func viewDidHide() {
        super.viewDidHide()
        contentTintColor = .secondaryLabelColor
    }

    override func mouseEntered(with event: NSEvent) { contentTintColor = .labelColor }
    override func mouseExited(with event: NSEvent) { contentTintColor = .secondaryLabelColor }

    @objc private func fire() { handler?() }
}

/// A filled circle drawn by its layer.
final class DotView: NSView {
    var color: NSColor = .controlAccentColor { didSet { needsDisplay = true } }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var wantsUpdateLayer: Bool { true }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        layer?.cornerRadius = min(newSize.width, newSize.height) / 2
    }

    override func updateLayer() {
        guard let layer else { return }
        effectiveAppearance.performAsCurrentDrawingAppearance { layer.backgroundColor = color.cgColor }
    }
}

/// Initials on a tint of the person's color, or their picture once one is known: the reader's
/// avatar in miniature, drawn by its layer.
final class AvatarView: NSView {
    private static let font = NSFont.systemFont(ofSize: 9, weight: .semibold)
    private static let lineHeight = labelLineHeight(font)

    private let label = NSTextField(labelWithString: "")
    private let picture = CALayer()
    private var color: NSColor = .systemBlue

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = true
        label.font = Self.font
        label.alignment = .center
        addSubview(label)
        picture.contentsGravity = .resizeAspectFill
        picture.isHidden = true
        layer?.addSublayer(picture)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(initials: String, color: NSColor, photo: NSImage?) {
        label.stringValue = initials
        label.textColor = color
        self.color = color
        needsDisplay = true
        if let photo { show(photo, animated: false) } else { hidePhoto() }
    }

    func show(_ photo: NSImage, animated: Bool) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        picture.contents = photo
        picture.contentsScale = window?.backingScaleFactor ?? 2
        picture.isHidden = false
        CATransaction.commit()
        label.isHidden = true
        if animated {
            let fade = Motion.basic("opacity", from: 0, to: 1, duration: 0.2)
            picture.add(fade, forKey: "fade")
        }
    }

    private func hidePhoto() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        picture.contents = nil
        picture.isHidden = true
        CATransaction.commit()
        label.isHidden = false
    }

    override var wantsUpdateLayer: Bool { true }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        layer?.cornerRadius = min(newSize.width, newSize.height) / 2
        needsLayout = true
    }

    override func layout() {
        super.layout()
        place(label, x: 0, size: NSSize(width: bounds.width, height: Self.lineHeight), height: bounds.height)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        picture.frame = bounds
        CATransaction.commit()
    }

    override func updateLayer() {
        guard let layer else { return }
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer.backgroundColor = color.withAlphaComponent(0.15).cgColor
        }
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// Sender initials and a stable per-address color, shared by the sidebar and the reader.
enum Avatar {
    static func color(for email: String) -> NSColor {
        let palette: [NSColor] = [.systemBlue, .systemPurple, .systemPink, .systemRed, .systemOrange,
                                  .systemGreen, .systemTeal, .systemIndigo, .systemBrown, .systemMint, .systemCyan]
        var hash: UInt64 = 1469598103934665603
        for byte in email.lowercased().utf8 { hash = (hash ^ UInt64(byte)) &* 1099511628211 }
        return palette[Int(hash % UInt64(palette.count))]
    }

    static func initials(for address: Address) -> String {
        let name = address.displayName
        if name.unicodeScalars.contains(where: { $0.properties.isIdeographic }) {
            return String(name.trimmingCharacters(in: .whitespaces).prefix(1))
        }
        let words = name.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).filter { !$0.isEmpty }
        let letters = words.prefix(2).compactMap(\.first).map { String($0).uppercased() }
        return letters.isEmpty ? "?" : letters.joined()
    }
}

/// Floating capsule at the bottom of the list for transient work ("Checking for mail…") and errors.
/// Rises and fades in, sinks and fades out.
final class StatusPill: NSVisualEffectView {
    private let spinner = NSProgressIndicator()
    private let label = NSTextField(labelWithString: "")
    private var shown = false
    private var generation = 0

    init() {
        super.init(frame: .zero)
        material = .popover
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        alphaValue = 0
        isHidden = true
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let stack = NSStackView(views: [spinner, label])
        stack.spacing = 7
        stack.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 14)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
            heightAnchor.constraint(equalToConstant: 28),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func updateLayer() {
        super.updateLayer()
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    func show(_ text: String, busy: Bool, warning: Bool = false) {
        label.stringValue = warning ? "⚠︎  " + text : text
        label.textColor = warning ? .systemOrange : .secondaryLabelColor
        toolTip = text
        busy ? spinner.startAnimation(nil) : spinner.stopAnimation(nil)
        spinner.isHidden = !busy
        guard !shown else { return }
        shown = true
        generation += 1
        isHidden = false
        Motion.fade(self, to: 1, duration: 0.22)
        Motion.slideIn(self, dy: -8)
    }

    func hide() {
        guard shown else { return }
        shown = false
        generation += 1
        let current = generation
        Motion.group({ Motion.fade(self, to: 0, duration: 0.3) }, completion: { [weak self] in
            guard let self, self.generation == current else { return }
            self.isHidden = true
            self.spinner.stopAnimation(nil)
        })
    }
}
