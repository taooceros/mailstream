import AppKit

/// The app's lifetime outside the scenes: settings watching, the store, the control socket, and
/// reopen and last-window behavior. Created by `MailstreamApp` before any window, so the store and
/// the main window's model exist by the time the main scene builds.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let accounts = AccountStore(problems: StoreProblems())
    let store: MailStore
    let main: MainModel
    /// The control socket this copy serves, and the task serving it.
    private var controlServer: ControlServer?
    private var controlTask: Task<Void, Never>?

    override init() {
        // Windows never restore (the main window opens at every launch). Saved state left by a copy
        // that was killed, or by the AppKit app before it, would otherwise count as a restore and
        // keep SwiftUI from opening the main window.
        UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true])
        Settings.shared.startWatching()
        store = MailStore(accounts: accounts, tools: ToolEnvironment.resolve())
        main = MainModel(store: store)
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.activate()
        store.start()
        startControl()
        if accounts.records.isEmpty { AccountsWindowController.show(accounts: accounts) }
    }

    /// The socket `mailstream` commands reach the app through.
    private func startControl() {
        do {
            guard let server = try ControlServer.claim() else { return }
            controlServer = server
            let service = ControlService(accounts: accounts, store: store)
            controlTask = Task(name: "Control socket") {
                do {
                    try await server.run { request in await service.handle(request) }
                } catch {
                    NSLog("Mailstream control socket: %@", error.localizedDescription)
                }
            }
        } catch {
            NSLog("Mailstream control socket: %@", error.localizedDescription)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        controlTask?.cancel()
        controlServer?.removeSocket()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { main.openMainWindow?() }
        return true
    }

    func showSettings() {
        AccountsWindowController.show(accounts: accounts)
    }
}
