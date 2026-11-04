import AppKit

/// The app's visual language, defined once. AppKit views read these tokens directly; the web pages
/// get the same values as CSS custom properties (`Design.stylesheet`), resolved for the light and
/// the dark appearance, so the native list and the HTML reader cannot drift apart.
///
/// Colors follow the system's semantic colors, so both surfaces track the appearance and accent
/// the user picked. Sizes, radii and durations are short scales: pick the nearest step rather than
/// a new literal. Values that follow from geometry (a pill's radius is half its height, a circle is
/// 50%) and one-off layout offsets are not tokens.
enum Design {
    // MARK: Color

    enum Color: String, CaseIterable {
        /// The reading surface: the reader page, and what a sliding list uncovers.
        case page = "bg"
        case text, muted, faint
        /// Hairlines between messages, card borders, separators.
        case line
        /// Row and button hover fill.
        case hover
        /// Quiet fill for chips, skeletons, code and thumbnails.
        case chip
        case accent, danger, success, star, waiting

        var color: NSColor {
            switch self {
            case .page: .textBackgroundColor
            case .text: .labelColor
            case .muted: .secondaryLabelColor
            case .faint: .tertiaryLabelColor
            case .line: .separatorColor
            case .hover: NSColor.labelColor.withAlphaComponent(0.04)
            case .chip: NSColor.labelColor.withAlphaComponent(0.06)
            case .accent: .controlAccentColor
            case .danger: .systemRed
            case .success: .systemGreen
            case .star: .systemYellow
            case .waiting: .systemOrange
            }
        }

        var cgColor: CGColor { color.cgColor }

        /// `var(--name)`, for CSS written in Swift.
        var css: String { "var(--\(rawValue))" }
    }

    // MARK: Type

    /// The type scale, in points (CSS px).
    enum TextSize: CGFloat, CaseIterable {
        case micro = 10.5
        case caption = 11
        case footnote = 12
        case body = 13
        /// Message text in the reader and chat.
        case reading = 14
        case headline = 15
        case title = 20
        /// The thread title.
        case display = 27

        var cssName: String { "--size-\(self)" }
        var css: String { "var(\(cssName))" }
    }

    static func font(_ size: TextSize, weight: NSFont.Weight = .regular) -> NSFont {
        .systemFont(ofSize: size.rawValue, weight: weight)
    }

    static func monospacedFont(_ size: TextSize, weight: NSFont.Weight = .regular) -> NSFont {
        .monospacedSystemFont(ofSize: size.rawValue, weight: weight)
    }

    /// CSS font stacks; AppKit uses the system fonts they name first.
    enum FontFamily: String, CaseIterable {
        case ui, serif, mono

        var stack: String {
            switch self {
            case .ui: "-apple-system, BlinkMacSystemFont, \"SF Pro Text\", \"PingFang SC\", sans-serif"
            case .serif: "\"New York\", ui-serif, Georgia, serif"
            case .mono: "ui-monospace, SFMono-Regular, Menlo, monospace"
            }
        }

        var cssName: String { "--font-\(rawValue)" }
        var css: String { "var(\(cssName))" }
    }

    // MARK: Shape

    /// Corner radii.
    enum Radius: CGFloat, CaseIterable {
        /// Inline code, small marks.
        case small = 4
        /// Buttons, thumbnails, rows.
        case control = 8
        /// Cards, banners, attachment tiles, popovers.
        case card = 10
        /// Panels: the composer, large sheets.
        case panel = 14

        var cssName: String { "--radius-\(self)" }
        var css: String { "var(\(cssName))" }
    }

    /// A drop shadow. Native layers apply it; CSS gets it as `box-shadow`.
    struct Shadow {
        var opacity: Float
        var radius: CGFloat
        var offsetY: CGFloat = 0

        /// Under a page sliding over another (the reader, a bundle) during a back swipe.
        static let page = Shadow(opacity: 0.12, radius: 14)

        func apply(to layer: CALayer, path: CGPath? = nil) {
            layer.shadowColor = NSColor.black.cgColor
            layer.shadowOpacity = opacity
            layer.shadowRadius = radius
            layer.shadowOffset = CGSize(width: 0, height: -offsetY)
            layer.shadowPath = path
        }

        /// Elevations the web pages use, lowest first.
        static let css: [(name: String, value: String)] = [
            ("raised", "0 1px 2px rgba(0,0,0,.04), 0 6px 20px rgba(0,0,0,.06)"),
            ("lifted", "0 1px 2px rgba(0,0,0,.05), 0 8px 26px rgba(0,0,0,.09)"),
            ("floating", "0 1px 2px rgba(0,0,0,.04), 0 10px 32px rgba(0,0,0,.09)"),
            ("popover", "0 2px 10px rgba(0,0,0,.08)"),
        ]
    }

    // MARK: Motion

    /// Animation durations, in seconds.
    enum Duration: TimeInterval, CaseIterable {
        /// Hover, press, color changes.
        case quick = 0.15
        /// Fades and small moves.
        case brief = 0.24
        /// Folds and slides.
        case standard = 0.3
        /// Entrances.
        case long = 0.45

        var cssName: String { "--dur-\(self)" }
        var css: String { "var(\(cssName))" }
    }

    /// The house easing curve: quick start, long soft landing.
    static let easeControlPoints: (Float, Float, Float, Float) = (0.2, 0.9, 0.25, 1)

    // MARK: CSS

    /// The tokens as CSS custom properties on `:root`, for both appearances.
    static var stylesheet: String {
        """
        :root { color-scheme: light dark; \(variables(for: .aqua)) \(scales) }
        @media (prefers-color-scheme: dark) { :root { \(variables(for: .darkAqua)) } }
        """
    }

    /// The tokens for one appearance only, for documents that do not follow the app's (HTML mail
    /// frames always render on a light card).
    static func stylesheet(fixedTo appearance: NSAppearance.Name) -> String {
        ":root { color-scheme: only light; \(variables(for: appearance)) \(scales) }"
    }

    /// The color properties, resolved as `appearance` draws them.
    static func variables(for appearance: NSAppearance.Name) -> String {
        var declarations = ""
        NSAppearance(named: appearance)?.performAsCurrentDrawingAppearance {
            declarations = Color.allCases.map { "--\($0.rawValue): \($0.color.cssRGBA);" }.joined(separator: " ")
        }
        return declarations
    }

    /// Everything that does not depend on the appearance.
    private static var scales: String {
        let (x1, y1, x2, y2) = easeControlPoints
        var parts = ["--ease: cubic-bezier(\(x1), \(y1), \(x2), \(y2));"]
        parts += TextSize.allCases.map { "\($0.cssName): \($0.rawValue.css)px;" }
        parts += FontFamily.allCases.map { "\($0.cssName): \($0.stack);" }
        parts += Radius.allCases.map { "\($0.cssName): \($0.rawValue.css)px;" }
        parts += Shadow.css.map { "--shadow-\($0.name): \($0.value);" }
        parts += Duration.allCases.map { "\($0.cssName): \($0.rawValue.css)s;" }
        return parts.joined(separator: " ")
    }
}

private extension CGFloat {
    /// `13`, `10.5`: no trailing zeros.
    var css: String { Double(self).css }
}

private extension Double {
    var css: String {
        self == rounded() ? String(Int(self)) : String(self)
    }
}

extension NSColor {
    /// `rgba(r, g, b, a)` in sRGB, as the current drawing appearance resolves the color.
    var cssRGBA: String {
        guard let rgb = usingColorSpace(.sRGB) else { return "transparent" }
        let channel = { (value: CGFloat) in Int((min(max(value, 0), 1) * 255).rounded()) }
        let alpha = (Double(rgb.alphaComponent) * 1000).rounded() / 1000
        return "rgba(\(channel(rgb.redComponent)), \(channel(rgb.greenComponent)), \(channel(rgb.blueComponent)), \(alpha.css))"
    }
}
