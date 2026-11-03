// Renders Resources/AppIcon.png (1024×1024). Run: swift scripts/make-icon.swift
import AppKit

let size: CGFloat = 1024
let image = NSImage(size: NSSize(width: size, height: size))
image.lockFocus()
let context = NSGraphicsContext.current!.cgContext

// macOS icon grid: 824pt body, ~185pt corner radius, soft drop shadow.
let body = CGRect(x: 100, y: 100, width: 824, height: 824)
let shape = CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: NSColor.black.withAlphaComponent(0.28).cgColor)
context.addPath(shape)
context.setFillColor(NSColor.black.cgColor)
context.fillPath()
context.restoreGState()

context.saveGState()
context.addPath(shape)
context.clip()
let colors = [NSColor(srgbRed: 0.36, green: 0.55, blue: 1.0, alpha: 1).cgColor,
              NSColor(srgbRed: 0.27, green: 0.24, blue: 0.86, alpha: 1).cgColor,
              NSColor(srgbRed: 0.55, green: 0.22, blue: 0.85, alpha: 1).cgColor] as CFArray
let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: colors, locations: [0, 0.6, 1])!
context.drawLinearGradient(gradient, start: CGPoint(x: 200, y: 924), end: CGPoint(x: 824, y: 100), options: [])

// Flowing "stream" ribbons behind the envelope.
for (index, alpha) in [0.16, 0.11, 0.07].enumerated() {
    let path = CGMutablePath()
    let base = 330 - CGFloat(index) * 70
    path.move(to: CGPoint(x: 60, y: base))
    path.addCurve(to: CGPoint(x: 964, y: base + 40),
                  control1: CGPoint(x: 360, y: base + 180), control2: CGPoint(x: 640, y: base - 140))
    path.addLine(to: CGPoint(x: 964, y: 60))
    path.addLine(to: CGPoint(x: 60, y: 60))
    path.closeSubpath()
    context.addPath(path)
    context.setFillColor(NSColor.white.withAlphaComponent(alpha).cgColor)
    context.fillPath()
}

// Top sheen.
let sheen = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB),
                       colors: [NSColor.white.withAlphaComponent(0.22).cgColor, NSColor.white.withAlphaComponent(0).cgColor] as CFArray,
                       locations: [0, 1])!
context.drawLinearGradient(sheen, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 560), options: [])
context.restoreGState()

// Envelope glyph.
let config = NSImage.SymbolConfiguration(pointSize: 400, weight: .medium)
    .applying(.init(paletteColors: [.white]))
if let symbol = NSImage(systemSymbolName: "envelope.fill", accessibilityDescription: nil)?.withSymbolConfiguration(config) {
    let glyph = symbol.size
    let rect = CGRect(x: (size - glyph.width) / 2, y: (size - glyph.height) / 2 + 30, width: glyph.width, height: glyph.height)
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: NSColor(srgbRed: 0.1, green: 0.05, blue: 0.4, alpha: 0.35).cgColor)
    symbol.draw(in: rect)
    context.restoreGState()
}
image.unlockFocus()

let rep = NSBitmapImageRep(data: image.tiffRepresentation!)!
let png = rep.representation(using: .png, properties: [:])!
let url = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "Resources/AppIcon.png")
try! png.write(to: url)
print("Wrote \(url.path)")
