import AppKit
import SwiftUI

/// Hosts `AccountsView` in the single Accounts window.
@MainActor
final class AccountsWindowController: NSWindowController, NSWindowDelegate {
    private static var shared: AccountsWindowController?

    /// Opens the single Accounts window.
    static func show(accounts: AccountStore) {
        if shared?.model.accounts !== accounts { shared = AccountsWindowController(accounts: accounts) }
        shared?.showWindow(nil)
    }

    private let model: AccountsModel

    private init(accounts: AccountStore) {
        model = AccountsModel(accounts: accounts)
        // Window glue until phase 2 moves Accounts into a SwiftUI scene.
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 420),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        let hosting = NSHostingController(rootView: AccountsView(model: model))
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 560, height: 420))
        window.title = "Accounts"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.minSize = NSSize(width: 460, height: 320)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func windowWillClose(_ notification: Notification) {
        model.cancelSignIn()
    }
}
