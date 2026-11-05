import AppKit

/// Click-to-enlarge for sender pictures: a popover with the picture, the name and the address.
/// It opens at once on the small cached picture and cross-fades to a 512 px one when it arrives.
@MainActor
enum AvatarPreview {
    private static weak var current: NSPopover?

    static func show(_ address: Address, small: NSImage, automated: Bool, avatars: AvatarStore,
                     relativeTo rect: NSRect, of view: NSView) {
        current?.close()
        let content = AvatarPreviewController(address: address, image: small)
        let popover = NSPopover()
        popover.behavior = .transient
        popover.animates = true
        popover.contentViewController = content
        popover.show(relativeTo: rect, of: view, preferredEdge: .maxX)
        current = popover
        Task { [weak content] in
            guard let image = await avatars.largeImage(for: address.email, automated: automated) else { return }
            content?.show(image)
        }
    }
}

private final class AvatarPreviewController: NSViewController {
    private static let side: CGFloat = 220

    private let address: Address
    private let picture = NSImageView()

    init(address: Address, image: NSImage) {
        self.address = address
        super.init(nibName: nil, bundle: nil)
        picture.image = image
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        picture.imageScaling = .scaleProportionallyUpOrDown
        picture.wantsLayer = true
        picture.layer?.cornerRadius = Design.Radius.panel.rawValue
        picture.layer?.cornerCurve = .continuous
        picture.layer?.masksToBounds = true

        let name = NSTextField.label(address.displayName, font: Design.font(.headline, weight: .semibold))
        name.alignment = .center
        name.lineBreakMode = .byTruncatingTail
        let email = NSTextField.label(address.email, font: Design.font(.footnote), color: .muted)
        email.alignment = .center
        email.lineBreakMode = .byTruncatingMiddle
        email.isSelectable = true
        // Chat apps and people Google withholds the address of are keyed by their user id.
        email.isHidden = !address.email.contains("@")

        let stack = NSStackView(views: [picture, name, email])
        stack.orientation = .vertical
        stack.spacing = 4
        stack.setCustomSpacing(12, after: picture)
        stack.edgeInsets = NSEdgeInsets(top: 14, left: 14, bottom: 14, right: 14)
        NSLayoutConstraint.activate([
            picture.widthAnchor.constraint(equalToConstant: Self.side),
            picture.heightAnchor.constraint(equalToConstant: Self.side),
            name.widthAnchor.constraint(lessThanOrEqualToConstant: Self.side),
            email.widthAnchor.constraint(lessThanOrEqualToConstant: Self.side),
        ])
        view = stack
    }

    func show(_ image: NSImage) {
        if let layer = picture.layer { Motion.crossfade(layer, duration: .brief, key: "sharpen") }
        picture.image = image
    }
}
