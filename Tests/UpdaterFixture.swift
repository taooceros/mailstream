import AppKit
import CryptoKit
import Sparkle

// Test-only host: no Flutter engine, accounts, Keychain access or mail database.
// Its HTTP loopback feed is intentionally not accepted by the production AppUpdater.
@main
struct UpdaterFixture {
  static func main() throws {
    if CommandLine.arguments.count == 3 && CommandLine.arguments[1] == "--generate-test-key" {
      let key = Curve25519.Signing.PrivateKey()
      let path = CommandLine.arguments[2]
      try Data(key.rawRepresentation.base64EncodedString().utf8).write(to: URL(fileURLWithPath: path))
      print(key.publicKey.rawRepresentation.base64EncodedString())
      return
    }
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    let delegate = FixtureDelegate()
    app.delegate = delegate
    app.run()
    withExtendedLifetime(delegate) {}
  }
}
final class FixtureDelegate: NSObject, NSApplicationDelegate, SPUUpdaterDelegate {
  private var window: NSWindow!
  private var updater: SPUStandardUpdaterController!

  func applicationDidFinishLaunching(_ notification: Notification) {
    let appMenu = NSMenu()
    let main = NSMenu()
    let root = NSMenuItem()
    root.submenu = appMenu
    main.addItem(root)
    NSApp.mainMenu = main
    updater = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: self, userDriverDelegate: nil)
    let check = NSMenuItem(title: "Check for Updates…", action: #selector(SPUStandardUpdaterController.checkForUpdates(_:)), keyEquivalent: "u")
    check.target = updater
    appMenu.addItem(check)
    appMenu.addItem(.separator())
    appMenu.addItem(withTitle: "Quit Update Fixture", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 260),
                      styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
    window.title = "Mailstream — Synthetic Update Test"
    let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
    let label = NSTextField(wrappingLabelWithString: "Synthetic update profile\nInstalled build: \(version)\n\nNo accounts, mail database, telemetry or production release.\nUse Check for Updates… in the application menu.")
    label.frame = NSRect(x: 30, y: 40, width: 500, height: 180)
    window.contentView?.addSubview(label)
    if let displayID = Bundle.main.object(forInfoDictionaryKey: "MSFixtureDisplayID") as? UInt32,
       let screen = NSScreen.screens.first(where: {
         ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == displayID
       }) {
      window.setFrameOrigin(NSPoint(x: screen.frame.minX + 100, y: screen.frame.minY + 100))
    } else {
      window.center()
    }
    window.makeKeyAndOrderFront(nil)
  }

  func allowedChannels(for updater: SPUUpdater) -> Set<String> {
    Bundle.main.object(forInfoDictionaryKey: "MSUpdateChannel") as? String == "prerelease" ? ["prerelease"] : []
  }
  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
