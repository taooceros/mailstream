import AppKit

/// The app's visual language, defined once. SwiftUI views read these tokens through
/// `DesignSwiftUI.swift`; the web pages get the same values as CSS custom properties
/// (`Design.stylesheet`), resolved for the light and the dark appearance, so the native list and
/// the HTML reader cannot drift apart.
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
        /// Text and symbols on an accent fill (the Send button).
        case onAccent = "on-accent"

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
            case .onAccent: .alternateSelectedControlTextColor
            }
        }

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
    }

    /// For the AppKit text views of the compose fields.
    static func font(_ size: TextSize, weight: NSFont.Weight = .regular) -> NSFont {
        .systemFont(ofSize: size.rawValue, weight: weight)
    }

    /// CSS font stacks; native text uses the system fonts they name first.
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
    }

    /// A drop shadow. SwiftUI applies it (`designShadow`); CSS gets it as `box-shadow`.
    struct Shadow {
        var opacity: Float
        var radius: CGFloat
        var offsetY: CGFloat = 0

        /// Under a page sliding over another (the reader, a bundle) during a back swipe.
        static let page = Shadow(opacity: 0.12, radius: 14)

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

/// Muted topic colors shared by the list (SwiftUI) and the reader (CSS). Known topics keep fixed hues.
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
        return known[key] ?? fallback[stableIndex(of: key, in: fallback.count)]
    }

    /// Light-appearance hex, for CSS (the reader derives its dark variant).
    static func hex(for label: String) -> String { pair(for: label).0 }

    static func color(for label: String) -> NSColor {
        let (light, dark) = pair(for: label)
        return .dynamic(light: light, dark: dark)
    }
}

/// A slot in `0..<count` for `key` that is the same on every launch (FNV-1a; `hashValue` is seeded).
nonisolated func stableIndex(of key: String, in count: Int) -> Int {
    var hash: UInt64 = 1469598103934665603
    for byte in key.utf8 { hash = (hash ^ UInt64(byte)) &* 1099511628211 }
    return Int(hash % UInt64(count))
}

nonisolated private extension CGFloat {
    /// `13`, `10.5`: no trailing zeros.
    var css: String { Double(self).css }
}

nonisolated private extension Double {
    var css: String {
        self == rounded() ? String(Int(self)) : String(self)
    }
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

    /// `rgba(r, g, b, a)` in sRGB, as the current drawing appearance resolves the color.
    nonisolated var cssRGBA: String {
        guard let rgb = usingColorSpace(.sRGB) else { return "transparent" }
        let channel = { (value: CGFloat) in Int((min(max(value, 0), 1) * 255).rounded()) }
        let alpha = (Double(rgb.alphaComponent) * 1000).rounded() / 1000
        return "rgba(\(channel(rgb.redComponent)), \(channel(rgb.greenComponent)), \(channel(rgb.blueComponent)), \(alpha.css))"
    }
}
