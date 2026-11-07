import Cocoa
import FlutterMacOS
import Sparkle

@main
class AppDelegate: FlutterAppDelegate {
  private var appUpdater: AppUpdater?

  override func applicationDidFinishLaunching(_ notification: Notification) {
    super.applicationDidFinishLaunching(notification)
    appUpdater = AppUpdater()
  }
  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}

/// Sparkle owns scheduling, consent, downloading, authentication, replacement and relaunch.
/// Mail sync and the engine are deliberately not involved.
final class AppUpdater: NSObject, NSMenuItemValidation {
  private var controller: SPUStandardUpdaterController?
  private var configurationError: String?

  override init() {
    super.init()
    configurationError = Self.configurationError(Bundle.main.infoDictionary ?? [:])
    if configurationError == nil {
      let controller = SPUStandardUpdaterController(
        startingUpdater: false, updaterDelegate: nil, userDriverDelegate: nil)
      self.controller = controller
      do {
        try controller.updater.start()
      } catch {
        configurationError = error.localizedDescription
        self.controller = nil
      }
    }
    guard let menu = NSApp.mainMenu?.items.first?.submenu else { return }
    let check = NSMenuItem(title: "Check for Updates…",
                          action: #selector(checkForUpdates(_:)), keyEquivalent: "")
    check.target = self
    menu.insertItem(check, at: min(2, menu.numberOfItems))
    let automatic = NSMenuItem(title: "Automatically Check for Updates",
                              action: #selector(toggleAutomaticChecks(_:)), keyEquivalent: "")
    automatic.target = self
    menu.insertItem(automatic, at: min(3, menu.numberOfItems))
  }

  static func configurationError(_ info: [String: Any]) -> String? {
    guard let key = info["SUPublicEDKey"] as? String,
          Data(base64Encoded: key)?.count == 32 else {
      return "This build has no update signing public key. The publisher must configure a signed update channel before automatic updates can be enabled."
    }
    guard let feed = info["SUFeedURL"] as? String, let url = URL(string: feed),
          url.scheme == "https", url.host != nil, url.user == nil, url.password == nil else {
      return "This build has no secure HTTPS update feed. Contact the publisher for a configured build."
    }
    guard info["SUVerifyUpdateBeforeExtraction"] as? Bool == true else {
      return "This build does not require authenticated update archives. Contact the publisher for a configured build."
    }
    return nil
  }

  @objc private func checkForUpdates(_ sender: Any?) {
    if let controller {
      controller.checkForUpdates(sender)
    } else {
      let alert = NSAlert()
      alert.messageText = "Updates Aren’t Configured"
      alert.informativeText = configurationError ?? "The updater could not start."
      alert.addButton(withTitle: "OK")
      alert.runModal()
    }
  }

  @objc private func toggleAutomaticChecks(_ sender: Any?) {
    guard let updater = controller?.updater else { return }
    updater.automaticallyChecksForUpdates.toggle()
  }

  func validateMenuItem(_ item: NSMenuItem) -> Bool {
    if item.action == #selector(toggleAutomaticChecks(_:)) {
      item.state = controller?.updater.automaticallyChecksForUpdates == true ? .on : .off
      return controller != nil
    }
    return controller?.updater.canCheckForUpdates ?? true
  }
}
