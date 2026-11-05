import AppKit

extension NSTextField {
    /// A one-line label in the app's type and colors.
    static func label(_ text: String = "", font: NSFont, color: Design.Color = .text) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = font
        label.textColor = color.color
        return label
    }
}

extension NSBox {
    /// A hairline between sections.
    static func separator() -> NSBox {
        let box = NSBox()
        box.boxType = .separator
        return box
    }
}

extension NSView {
    /// Adds `views`, in order (later ones on top), to be placed by constraints.
    func addConstrained(_ views: [NSView]) {
        for view in views {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
    }

    /// Adds `child` filling this view edge to edge.
    func addFilling(_ child: NSView) {
        addConstrained([child])
        NSLayoutConstraint.activate([
            child.topAnchor.constraint(equalTo: topAnchor),
            child.bottomAnchor.constraint(equalTo: bottomAnchor),
            child.leadingAnchor.constraint(equalTo: leadingAnchor),
            child.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }
}

extension CALayer {
    /// A card: a hairline edge around the control background. Call where colors resolve for the
    /// view's appearance (`updateLayer`, or inside `performAsCurrentDrawingAppearance`).
    func styleAsCard(radius: Design.Radius) {
        cornerRadius = radius.rawValue
        borderWidth = 1
        backgroundColor = NSColor.controlBackgroundColor.cgColor
        borderColor = Design.Color.line.cgColor
    }
}

extension NSToolbarItem {
    /// Makes this item a button: an SF Symbol named for `label`, sending `action` to `target`.
    func makeButton(label: String, symbol: String, toolTip: String? = nil, target: AnyObject?, action: Selector) {
        self.label = label
        paletteLabel = label
        self.toolTip = toolTip ?? label
        image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        self.target = target
        self.action = action
    }
}

extension NSEvent {
    /// Return or the keypad's Enter.
    var isReturnKey: Bool { keyCode == 36 || keyCode == 76 }
}

extension NSWindow {
    /// A warning sheet with just an OK button.
    func showWarning(_ title: String, detail: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = detail
        alert.beginSheetModal(for: self)
    }

    /// A sheet asking to `confirm` or `cancel`; `confirmed` runs only for the first. A destructive
    /// confirm button is drawn red and gives up Return. `prepare` adds anything else (an icon, an
    /// accessory view, key equivalents) before the sheet shows.
    func confirm(_ title: String, detail: String, confirm: String, cancel: String = "Cancel",
                 destructive: Bool = false, prepare: (NSAlert) -> Void = { _ in },
                 confirmed: @escaping @MainActor () -> Void) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: confirm).hasDestructiveAction = destructive
        alert.addButton(withTitle: cancel)
        prepare(alert)
        alert.beginSheetModal(for: self) { response in
            if response == .alertFirstButtonReturn { confirmed() }
        }
    }
}

/// A table cell made in code, recycled under its own identifier.
protocol ReusableCell: NSView {
    static var identifier: NSUserInterfaceItemIdentifier { get }
    init()
}

extension NSTableView {
    /// A recycled `Cell`, or a new one.
    func dequeue<Cell: ReusableCell>(_ type: Cell.Type) -> Cell {
        makeView(withIdentifier: Cell.identifier, owner: nil) as? Cell ?? Cell()
    }
}
