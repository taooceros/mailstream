import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var store: MailStore!
    private var mainWindow: MainWindowController!

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = Self.buildMenu()
        let tools = ToolEnvironment.resolve()
        store = MailStore(himalaya: Himalaya(tools: tools), tools: tools)
        mainWindow = MainWindowController(store: store)
        mainWindow.showWindow(nil)
        NSApp.activate()
        store.start()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { mainWindow.showWindow(nil) }
        return true
    }

    @objc func newMessage(_ sender: Any?) {
        ComposeWindowController.open(store: store, mode: .new, account: mainWindow.preferredAccount)
    }

    // MARK: - Menu

    private static func buildMenu() -> NSMenu {
        let main = NSMenu()

        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About Mailstream", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        let services = NSMenu()
        let servicesItem = appMenu.addItem(withTitle: "Services", action: nil, keyEquivalent: "")
        servicesItem.submenu = services
        NSApp.servicesMenu = services
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Hide Mailstream", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(item("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option]))
        appMenu.addItem(withTitle: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit Mailstream", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        add(appMenu, title: "Mailstream", to: main)

        let file = NSMenu(title: "File")
        file.addItem(item("New Message", #selector(AppDelegate.newMessage(_:)), "n"))
        file.addItem(.separator())
        file.addItem(item("Close", #selector(NSWindow.performClose(_:)), "w"))
        add(file, title: "File", to: main)

        let edit = NSMenu(title: "Edit")
        edit.addItem(item("Undo", Selector(("undo:")), "z"))
        edit.addItem(item("Redo", Selector(("redo:")), "z", [.command, .shift]))
        edit.addItem(.separator())
        edit.addItem(item("Cut", #selector(NSText.cut(_:)), "x"))
        edit.addItem(item("Copy", #selector(NSText.copy(_:)), "c"))
        edit.addItem(item("Paste", #selector(NSText.paste(_:)), "v"))
        edit.addItem(item("Paste and Match Style", #selector(NSTextView.pasteAsPlainText(_:)), "v", [.command, .option, .shift]))
        edit.addItem(item("Select All", #selector(NSText.selectAll(_:)), "a"))
        edit.addItem(.separator())
        edit.addItem(item("Find…", #selector(MainWindowController.focusSearch(_:)), "f"))
        add(edit, title: "Edit", to: main)

        let view = NSMenu(title: "View")
        view.addItem(item("Toggle Sidebar", #selector(NSSplitViewController.toggleSidebar(_:)), "s", [.command, .control]))
        view.addItem(item("Back", #selector(MainWindowController.goBack(_:)), "["))
        view.addItem(item("Get New Mail", #selector(MainWindowController.refresh(_:)), "n", [.command, .shift]))
        view.addItem(.separator())
        view.addItem(item("Auto-Tag with AI", #selector(MainWindowController.toggleAITagging(_:)), ""))
        view.addItem(item("Re-tag All Conversations", #selector(MainWindowController.retagAll(_:)), ""))
        view.addItem(.separator())
        view.addItem(item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]))
        add(view, title: "View", to: main)

        let message = NSMenu(title: "Message")
        message.addItem(item("Reply", #selector(MainWindowController.reply(_:)), "r"))
        message.addItem(item("Reply All", #selector(MainWindowController.replyAll(_:)), "r", [.command, .shift]))
        message.addItem(item("Forward", #selector(MainWindowController.forward(_:)), "f", [.command, .shift]))
        message.addItem(.separator())
        message.addItem(item("Archive", #selector(MainWindowController.archive(_:)), "a", [.command, .control]))
        message.addItem(item("Move to Trash", #selector(MainWindowController.trash(_:)), "\u{8}"))
        message.addItem(.separator())
        message.addItem(item("Mark as Unread", #selector(MainWindowController.toggleRead(_:)), "u", [.command, .shift]))
        message.addItem(item("Star", #selector(MainWindowController.toggleStar(_:)), "l", [.command, .shift]))
        add(message, title: "Message", to: main)

        let window = NSMenu(title: "Window")
        window.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        window.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        window.addItem(.separator())
        window.addItem(withTitle: "Bring All to Front", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: "")
        add(window, title: "Window", to: main)
        NSApp.windowsMenu = window

        return main
    }

    private static func item(_ title: String, _ action: Selector, _ key: String,
                             _ modifiers: NSEvent.ModifierFlags = .command) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        return item
    }

    private static func add(_ menu: NSMenu, title: String, to main: NSMenu) {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.submenu = menu
        main.addItem(item)
    }
}
