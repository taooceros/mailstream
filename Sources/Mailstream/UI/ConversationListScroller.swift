import AppKit
import SwiftUI

/// The list's lines in an AppKit scroll view, each drawn once with Core Graphics into its layer
/// (`ListRowDrawing`). Scrolling moves the clip view and Core Animation composites those layers:
/// nothing draws per frame, and no SwiftUI view moves with the scroll. A line coming into view is
/// drawn once; a line whose content changes is drawn again.
///
/// The line under the pointer is a live SwiftUI view, laid over its bitmap: it takes the hover
/// pill, clicks, the context menu, the picture's popover and tooltips. The Smart Inbox shelf, one
/// line at the top whose tiles lift and open, is always live.
///
/// The model decides which lines exist (`placed`) and where; this view follows it, and reports
/// back the geometry, pointer, scroll phase and keyboard focus.
struct ConversationListScroller: NSViewRepresentable {
    let model: ConversationListModel
    /// Read here so SwiftUI calls `updateNSView` when the items change, inside the change's
    /// transaction (a list change animates). Scrolling changes no input here: the view lays out
    /// what the model places as it goes.
    let itemsSerial: Int
    let reveal: ConversationListReveal?
    let focusRequest: Int
    let appearsActive: Bool

    func makeNSView(context: Context) -> ConversationListScrollView {
        ConversationListScrollView(model: model)
    }

    func updateNSView(_ view: ConversationListScrollView, context: Context) {
        view.appearsActive = appearsActive
        if context.transaction.animation != nil, !context.transaction.disablesAnimations {
            context.animate {
                view.syncLines(animated: true)
            } completion: {
                view.finishFades()
            }
        } else {
            view.syncLines(animated: false)
        }
        view.follow(reveal: reveal, focusRequest: focusRequest)
    }
}

/// One line as SwiftUI shows it: the hovered line over its bitmap, or the shelf.
private struct ConversationListLine: View {
    let model: ConversationListModel
    let item: ConversationListItem
    let state: ListRowState
    let emphasized: Bool
    let onClick: () -> Void

    var body: some View {
        let hovered = state.hovered
        switch item {
        case .section(let title, let count, let accent):
            ListRowSectionHeader(title: title, count: count, accent: accent)
        case .shelf(let bundles):
            ConversationListShelfRow(bundles: bundles, own: model.store.ownAddresses, hovered: hovered) {
                model.enterBundle($0)
            }
        case .conversation(let conversation):
            ConversationListMailRow(model: model, conversation: conversation, state: state,
                                    emphasized: emphasized, hovered: hovered, onClick: onClick)
                .equatable()
        case .space(let space):
            ConversationListChatRow(model: model, space: space, state: state,
                                    emphasized: emphasized, hovered: hovered, onClick: onClick)
                .equatable()
        }
    }
}

/// A live line, or nothing.
private struct ConversationListHoverLine: View {
    var line: ConversationListLine?

    var body: some View { line }
}

/// A line drawn once into its layer's bitmap. Drawing tracks what the line read, so a change to
/// any of it (content arriving, a highlight) draws it again. The line draws only when it says so
/// (`invalidate`): AppKit asks layers to redisplay as the clip view scrolls, and those asks cost
/// nothing here.
private final class ConversationListLineView: NSView {
    var item: ConversationListItem {
        didSet { if item != oldValue { invalidate() } }
    }
    /// Gathers what the line draws, then returns the drawing; only reads in the drawing are tracked.
    var paint: (ConversationListLineView) -> () -> Void = { _ in {} }
    var label: (ConversationListItem) -> String = { _ in "" }
    var onPress: (ConversationListItem) -> Bool = { _ in false }
    private var dirty = true

    init(item: ConversationListItem) {
        self.item = item
        super.init(frame: .zero)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    /// Draws the line again before the next frame.
    func invalidate() {
        dirty = true
        needsDisplay = true
    }

    override func setFrameSize(_ newSize: NSSize) {
        let resized = newSize != frame.size
        super.setFrameSize(newSize)
        if resized { invalidate() }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        invalidate()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        invalidate()
    }

    override func updateLayer() {
        guard dirty, let layer else { return }
        dirty = false
        let scale = window?.backingScaleFactor ?? 2
        let drawing = paint(self)
        let redraw: @MainActor () -> Void = { [weak self] in self?.invalidate() }
        var image: CGImage?
        effectiveAppearance.performAsCurrentDrawingAppearance {
            image = ListRowDrawing.bitmap(size: bounds.size, scale: scale, space: window?.colorSpace?.cgColorSpace) {
                withObservationTracking(drawing) {
                    Task { @MainActor in redraw() }
                }
            }
        }
        layer.contentsScale = scale
        layer.contents = image
    }

    override func accessibilityLabel() -> String? { label(item) }
    override func accessibilityPerformPress() -> Bool { onPress(item) }
}

final class ConversationListScrollView: NSScrollView {
    private let model: ConversationListModel
    private let lines = ConversationListLinesView()
    private var geometry = ConversationListGeometry()

    /// Lines laid out by the model, by item id.
    private var live: [String: ConversationListLineView] = [:]
    /// Lines that left in an animated change, fading out; recycled once their fade ends.
    private var fading: [String: ConversationListLineView] = [:]
    /// Views of lines gone, ready for the next line coming into view.
    private var pool: [ConversationListLineView] = []

    /// The live line over the hovered line's bitmap.
    private let hoverHost = NSHostingView(rootView: ConversationListHoverLine())
    /// The line the hover host shows, and what it showed it with.
    private var hovered: (id: String, item: ConversationListItem, emphasized: Bool)?
    /// The Smart Inbox shelf while it is placed: live, since its tiles lift and open.
    private let shelfHost = NSHostingView(rootView: ConversationListHoverLine())
    private var shelfItem: ConversationListItem?

    private var lastReveal: Int?
    private var lastFocusRequest: Int
    var appearsActive = true { didSet { if appearsActive != oldValue { emphasisChanged() } } }
    private var emphasized: Bool { lines.isFocused && appearsActive }

    init(model: ConversationListModel) {
        self.model = model
        lastReveal = model.reveal?.serial
        lastFocusRequest = model.focusRequest
        super.init(frame: .zero)
        drawsBackground = false
        contentView.drawsBackground = false
        hasVerticalScroller = true
        autohidesScrollers = true
        horizontalScrollElasticity = .none
        lines.wantsLayer = true
        documentView = lines
        for host in [shelfHost, hoverHost] {
            host.sizingOptions = []
            host.safeAreaRegions = []
            host.isHidden = true
            lines.addSubview(host)
        }
        lines.onFocusChange = { [weak self] focused in
            self?.model.isFocused = focused
            self?.emphasisChanged()
        }
        lines.onKey = { [weak self] key in self?.handle(key) ?? false }
        lines.onClick = { [weak self] point in self?.click(at: point) }
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                       owner: self))
        NotificationCenter.default.addObserver(self, selector: #selector(liveScrollStarted),
                                               name: NSScrollView.willStartLiveScrollNotification, object: self)
        NotificationCenter.default.addObserver(self, selector: #selector(liveScrollEnded),
                                               name: NSScrollView.didEndLiveScrollNotification, object: self)
        followHover()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    // MARK: Lines

    /// Makes the line views match `model.placed`: lines staying move to their place, new ones take
    /// a recycled view and render, and those gone are recycled (after fading out, when `animated`).
    func syncLines(animated: Bool) {
        let width = max(contentView.bounds.width, 0)
        let height = model.stackHeight + 2 * ConversationListView.inset
        if lines.frame.size != CGSize(width: width, height: height) {
            lines.setFrameSize(CGSize(width: width, height: height))
        }
        var next: [String: ConversationListLineView] = [:]
        next.reserveCapacity(model.placed.count)
        var shelf: (item: ConversationListItem, frame: CGRect)?
        for placed in model.placed {
            let frame = frame(for: placed, width: width)
            if case .shelf = placed.item {
                shelf = (placed.item, frame)
                continue
            }
            if let view = live.removeValue(forKey: placed.id) ?? fading.removeValue(forKey: placed.id) {
                let resized = view.frame.size != frame.size
                if view.frame != frame {
                    if animated && !resized { view.animator().frame = frame } else { view.frame = frame }
                }
                if view.alphaValue != 1 {
                    if animated { view.animator().alphaValue = 1 } else { view.alphaValue = 1 }
                }
                view.item = placed.item
                next[placed.id] = view
            } else {
                let view: ConversationListLineView
                if let recycled = pool.popLast() {
                    view = recycled
                    view.item = placed.item
                } else {
                    view = makeLine(placed.item)
                }
                // A line coming in starts where it belongs, never sliding over from where its
                // recycled view last stood; in an animated change it fades in there.
                Self.immediately {
                    view.layer?.removeAllAnimations()
                    view.frame = frame
                    view.alphaValue = animated ? 0 : 1
                    view.isHidden = false
                }
                // A line drawn while hidden shows what it drew; one gone stale draws now.
                view.needsDisplay = true
                if animated { view.animator().alphaValue = 1 }
                next[placed.id] = view
            }
        }
        for (id, view) in live {
            if animated {
                fading[id] = view
                view.animator().alphaValue = 0
            } else {
                recycle(view)
            }
        }
        live = next
        placeShelf(shelf, animated: animated)
        updateHover()
    }

    /// Shows the shelf's live view where the shelf is placed, or takes it away.
    private func placeShelf(_ shelf: (item: ConversationListItem, frame: CGRect)?, animated: Bool) {
        guard let shelf else {
            if shelfItem != nil {
                shelfItem = nil
                shelfHost.isHidden = true
                shelfHost.rootView = ConversationListHoverLine()
            }
            return
        }
        if shelfItem != shelf.item {
            shelfItem = shelf.item
            shelfHost.rootView = ConversationListHoverLine(line: line(for: shelf.item))
        }
        if shelfHost.isHidden {
            Self.immediately {
                shelfHost.frame = shelf.frame
                shelfHost.isHidden = false
            }
        } else if shelfHost.frame != shelf.frame {
            if animated { shelfHost.animator().frame = shelf.frame } else { shelfHost.frame = shelf.frame }
        }
    }

    /// Recycles the lines whose fade-out finished (a line that came back meanwhile is live again).
    func finishFades() {
        for (id, view) in fading where view.alphaValue == 0 {
            fading[id] = nil
            recycle(view)
        }
    }

    private func recycle(_ view: ConversationListLineView) {
        view.layer?.removeAllAnimations()
        view.isHidden = true
        pool.append(view)
    }

    /// Applies `changes` at once, even inside an animated change.
    private static func immediately(_ changes: () -> Void) {
        NSAnimationContext.beginGrouping()
        NSAnimationContext.current.duration = 0
        NSAnimationContext.current.allowsImplicitAnimation = false
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        changes()
        CATransaction.commit()
        NSAnimationContext.endGrouping()
    }

    private func frame(for placed: ConversationListPlacedItem, width: CGFloat) -> CGRect {
        let inset = ConversationListView.inset
        return CGRect(x: inset, y: inset + placed.top, width: max(width - 2 * inset, 0), height: placed.item.height)
    }

    private func line(for item: ConversationListItem) -> ConversationListLine {
        let id = item.id
        return ConversationListLine(model: model, item: item, state: model.rowState(for: item), emphasized: emphasized) {
            [weak self] in self?.click(id)
        }
    }

    // MARK: Drawing

    private func makeLine(_ item: ConversationListItem) -> ConversationListLineView {
        let view = ConversationListLineView(item: item)
        view.paint = { [unowned self] view in paint(view) }
        view.label = { [unowned self] item in label(for: item) }
        view.onPress = { [weak self] item in
            guard let self, item.selectionID != nil else { return false }
            click(item.id)
            return true
        }
        lines.addSubview(view, positioned: .below, relativeTo: hoverHost)
        return view
    }

    /// What a line draws into its layer. Its state is gathered here, untracked (bringing it up to
    /// date reads the store); the drawing reads the state, and is drawn again when that changes.
    private func paint(_ view: ConversationListLineView) -> () -> Void {
        let size = view.bounds.size
        switch view.item {
        case .section(let title, let count, let accent):
            return { ListRowDrawing.section(title: title, count: count, accent: accent, size: size) }
        case .shelf:
            return {}
        case .conversation, .space:
            let state = model.rowState(for: view.item)
            let emphasized = emphasized
            let scale = view.window?.backingScaleFactor ?? 2
            return {
                if state.highlighted { ListRowDrawing.highlight(emphasized: emphasized, size: size) }
                if let content = state.content { ListRowDrawing.row(content, pill: 0, size: size, scale: scale) }
            }
        }
    }

    private func label(for item: ConversationListItem) -> String {
        switch item {
        case .section(let title, _, _): title
        case .shelf(let bundles): bundles.map(\.category.title).joined(separator: ", ")
        case .conversation, .space: model.rowState(for: item).content?.accessibilityText ?? ""
        }
    }

    /// Focus or window activation changed: highlighted lines show it.
    private func emphasisChanged() {
        for view in live.values where model.rowState(for: view.item).highlighted { view.invalidate() }
        updateHover()
    }

    // MARK: Hover

    /// Lays the live line over the hovered line's bitmap, or takes it away.
    private func updateHover() {
        let id = model.hoveredID.flatMap { live[$0] == nil ? nil : $0 }
        if let old = hovered?.id, old != id { live[old]?.isHidden = false }
        guard let id, let view = live[id] else {
            hovered = nil
            if !hoverHost.isHidden {
                hoverHost.isHidden = true
                hoverHost.rootView = ConversationListHoverLine()
            }
            return
        }
        let emphasized = emphasized
        if hovered?.id != id || hovered?.item != view.item || hovered?.emphasized != emphasized {
            hovered = (id, view.item, emphasized)
            hoverHost.rootView = ConversationListHoverLine(line: line(for: view.item))
        }
        if hoverHost.frame != view.frame { hoverHost.frame = view.frame }
        hoverHost.isHidden = false
        view.isHidden = true
    }

    /// Follows hover changes the model makes on its own (window activation, the reader covering
    /// the list, the list changing under the pointer).
    private func followHover() {
        withObservationTracking { _ = model.hoveredID } onChange: { [weak self] in
            Task { @MainActor in
                self?.updateHover()
                self?.followHover()
            }
        }
    }

    // MARK: Clicks

    private func click(_ id: String) {
        window?.makeFirstResponder(lines)
        model.click(id)
    }

    /// A click on a bitmap (the live line takes clicks on the hovered one).
    private func click(at point: CGPoint) {
        window?.makeFirstResponder(lines)
        guard let view = live.values.first(where: { $0.frame.contains(point) }), view.item.selectionID != nil else { return }
        model.click(view.item.id)
    }

    // MARK: Geometry

    override func tile() {
        super.tile()
        // A new width resizes the stack and every line in it.
        if lines.frame.width != contentView.bounds.width { Self.immediately { syncLines(animated: false) } }
        reportGeometry()
    }

    override func reflectScrolledClipView(_ clipView: NSClipView) {
        super.reflectScrolledClipView(clipView)
        reportGeometry()
    }

    /// Tells the model where the scroll view stands, and lays out the lines it places for that at
    /// once, so a line coming into view is there in the same frame.
    private func reportGeometry() {
        let insetTop = contentInsets.top
        let clip = contentView.bounds
        let new = ConversationListGeometry(top: clip.minY + insetTop, topInset: insetTop,
                                           contentHeight: lines.frame.height,
                                           containerHeight: max(clip.height - insetTop, 0))
        guard new != geometry else { return }
        geometry = new
        model.geometryChanged(new)
        Self.immediately { syncLines(animated: false) }
    }

    // MARK: Model requests

    func follow(reveal: ConversationListReveal?, focusRequest: Int) {
        if let reveal, reveal.serial != lastReveal {
            lastReveal = reveal.serial
            contentView.scroll(to: CGPoint(x: 0, y: reveal.top - contentInsets.top))
            reflectScrolledClipView(contentView)
        }
        if focusRequest != lastFocusRequest {
            lastFocusRequest = focusRequest
            window?.makeFirstResponder(lines)
        }
    }

    // MARK: Pointer and scroll phase

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        pointerMoved(event)
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        pointerMoved(event)
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        model.pointerMoved(nil)
        updateHover()
    }

    /// The pointer measured from the scroll view's top edge below the toolbar, as the model reads it.
    private func pointerMoved(_ event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let fromTop = isFlipped ? point.y : bounds.height - point.y
        model.pointerMoved(CGPoint(x: point.x, y: fromTop - contentInsets.top))
        updateHover()
    }

    @objc private func liveScrollStarted() {
        model.scrollingChanged(true)
        updateHover()
    }

    @objc private func liveScrollEnded() {
        model.scrollingChanged(false)
        updateHover()
    }

    // MARK: Keys

    /// Mail-style single-key commands.
    private static let singleKey: [String: ConversationAction] = [
        "e": .archive, "s": .toggleStar, "u": .toggleRead, "d": .toggleWaiting,
        "r": .reply, "a": .replyAll, "f": .forward,
    ]

    private func handle(_ event: NSEvent) -> Bool {
        // Plain keys only; the keypad and function flags come with arrows, Delete and Enter.
        guard event.modifierFlags.intersection([.shift, .control, .option, .command, .capsLock]).isEmpty,
              let characters = event.characters else { return false }
        switch event.specialKey {
        case .upArrow?: model.moveHighlight(by: -1)
        case .downArrow?: model.moveHighlight(by: 1)
        case .carriageReturn?, .enter?: model.openHighlighted()
        case .delete?, .deleteForward?: model.performOnHighlight(.trash)
        default:
            if characters == " " {
                model.openHighlighted()
            } else if let action = Self.singleKey[characters] {
                model.performOnHighlight(action)
            } else {
                return false
            }
        }
        return true
    }
}

/// The scroll view's document: as tall as all lines, holding the views of those placed, and the
/// list's keyboard target.
private final class ConversationListLinesView: NSView {
    var onFocusChange: (Bool) -> Void = { _ in }
    var onKey: (NSEvent) -> Bool = { _ in false }
    var onClick: (CGPoint) -> Void = { _ in }
    private(set) var isFocused = false

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    override func becomeFirstResponder() -> Bool {
        isFocused = true
        onFocusChange(true)
        return true
    }

    override func resignFirstResponder() -> Bool {
        isFocused = false
        onFocusChange(false)
        return true
    }

    override func keyDown(with event: NSEvent) {
        if !onKey(event) { super.keyDown(with: event) }
    }

    override func mouseDown(with event: NSEvent) {
        onClick(convert(event.locationInWindow, from: nil))
    }
}
