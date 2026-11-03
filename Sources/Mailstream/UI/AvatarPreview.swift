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
        picture.layer?.cornerRadius = 14
        picture.layer?.cornerCurve = .continuous
        picture.layer?.masksToBounds = true

        let name = NSTextField(labelWithString: address.displayName)
        name.font = .systemFont(ofSize: 15, weight: .semibold)
        name.alignment = .center
        name.lineBreakMode = .byTruncatingTail
        let email = NSTextField(labelWithString: address.email)
        email.font = .systemFont(ofSize: 12)
        email.textColor = .secondaryLabelColor
        email.alignment = .center
        email.lineBreakMode = .byTruncatingMiddle
        email.isSelectable = true

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
        let fade = CATransition()
        fade.type = .fade
        fade.duration = 0.2
        fade.preferredFrameRateRange = Motion.frameRate
        picture.layer?.add(fade, forKey: "sharpen")
        picture.image = image
    }
}
