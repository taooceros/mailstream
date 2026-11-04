import AppKit
import SwiftUI

/// Hosts `ComposeView` in a window until the compose scene replaces it.
final class ComposeWindowController: NSWindowController, NSWindowDelegate {
    private static var open: Set<ComposeWindowController> = []

    /// `text` starts the body, above any quote (an inline reply moved into a window keeps its text,
    /// and its `plan` when one was already worked out).
    static func open(store: MailStore, mode: ComposeMode, account: String?, source: MailMessage? = nil,
                     replyAll: Bool = false, to: [Address] = [], text: String = "", plan: ReplyPlan? = nil) {
        guard let account = account ?? store.accounts.first?.name else { return }
        let request = ComposeRequest(mode: mode, account: account, source: source, replyAll: replyAll,
                                     to: to, text: text, plan: plan)
        let controller = ComposeWindowController(model: ComposeModel(store: store, request: request))
        open.insert(controller)
        controller.showWindow(nil)
        controller.model.focus(controller.model.to.isEmpty ? .to : .body)
    }

    private let model: ComposeModel

    private init(model: ComposeModel) {
        self.model = model
        // SwiftUI cannot veto a window's closing (the unsaved-changes question) until phase 2's scene.
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 600),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        let hosting = NSHostingController(rootView: ComposeView(model: model))
        hosting.sizingOptions = [.minSize]
        hosting.sceneBridgingOptions = [.title, .toolbars]
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 680, height: 600))
        window.toolbarStyle = .unified
        // Tab walks the fields top to bottom: To, Cc, (Bcc,) Subject, body.
        window.autorecalculatesKeyViewLoop = true
        super.init(window: window)
        window.delegate = self
        model.close = { [weak self] in self?.close() }
        window.center()
        if let parent = NSApp.mainWindow, parent !== window {
            window.setFrameTopLeftPoint(NSPoint(x: parent.frame.midX - 340, y: parent.frame.maxY - 60))
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// Asks before throwing away what was written; Return discards, Escape keeps editing.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        model.shouldClose()
    }

    func windowWillClose(_ notification: Notification) {
        model.closed()
        Self.open.remove(self)
    }
}
