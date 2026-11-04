import SwiftUI

// `Design` for SwiftUI views: the same tokens, as SwiftUI values.

extension Design.Color {
    var swiftUI: SwiftUI.Color { SwiftUI.Color(nsColor: color) }
}

extension SwiftUI.Color {
    static func design(_ token: Design.Color) -> SwiftUI.Color { token.swiftUI }
}

extension Font {
    static func design(_ size: Mailstream.Design.TextSize, weight: Font.Weight = .regular) -> Font {
        .system(size: size.rawValue, weight: weight)
    }

    /// Digits of equal width, for counts and dates that line up in columns.
    static func designDigits(_ size: Mailstream.Design.TextSize, weight: Font.Weight = .regular) -> Font {
        .system(size: size.rawValue, weight: weight).monospacedDigit()
    }
}

extension Animation {
    /// The house easing curve at one of the duration steps.
    static func house(_ duration: Design.Duration) -> Animation {
        let (x1, y1, x2, y2) = Design.easeControlPoints
        return .timingCurve(Double(x1), Double(y1), Double(x2), Double(y2), duration: duration.rawValue)
    }
}

extension View {
    /// `Design.Shadow` as a SwiftUI shadow.
    func designShadow(_ shadow: Design.Shadow) -> some View {
        self.shadow(color: .black.opacity(Double(shadow.opacity)), radius: shadow.radius, y: shadow.offsetY)
    }

    /// `CALayer.styleAsCard`: the control background inside a hairline edge.
    func designCard(_ radius: Design.Radius) -> some View {
        let shape = RoundedRectangle(cornerRadius: radius.rawValue)
        return background(SwiftUI.Color(nsColor: .controlBackgroundColor), in: shape)
            .overlay { shape.strokeBorder(SwiftUI.Color.design(.line), lineWidth: 1) }
    }
}
