import Cocoa
import FlutterMacOS
import Sparkle

@main
class AppDelegate: FlutterAppDelegate {
  @IBOutlet var appUpdater: AppUpdater!
  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}

/// Sparkle owns scheduling, consent, downloading, authentication, replacement and relaunch.
/// Mail sync and the engine are deliberately not involved.
final class AppUpdater: NSObject, NSMenuItemValidation, SPUUpdaterDelegate {
  private var controller: SPUStandardUpdaterController?
  private var configurationError: String?

  override init() {
    super.init()
    configurationError = Self.configurationError(Bundle.main.infoDictionary ?? [:])
    if configurationError == nil {
      let controller = SPUStandardUpdaterController(
        startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)
      self.controller = controller
      do {
        try controller.updater.start()
      } catch {
        configurationError = error.localizedDescription
        self.controller = nil
      }
    }
  }

  static func configurationError(_ info: [String: Any]) -> String? {
    guard let key = info["SUPublicEDKey"] as? String,
          Data(base64Encoded: key)?.count == 32 else {
      return "This build has no update signing public key. The publisher must configure a signed update channel before automatic updates can be enabled."
    }
    guard let feed = info["SUFeedURL"] as? String, let url = URL(string: feed),
          url.scheme == "https", url.host != nil, url.user == nil, url.password == nil else {
      return "The publisher has not enabled a secure update feed for this build. Automatic updates will be available after the signed feed is published."
    }
    guard info["SUVerifyUpdateBeforeExtraction"] as? Bool == true else {
      return "This build does not require authenticated update archives. Contact the publisher for a configured build."
    }
    guard info["SURequireSignedFeed"] as? Bool == true,
          info["SUSignedFeedFailureExpirationInterval"] as? Int == 0 else {
      return "This build does not require authenticated update information. Contact the publisher for a configured build."
    }
    guard let channel = info["MSUpdateChannel"] as? String,
          channel == "stable" || channel == "prerelease" else {
      return "This build has no supported update channel. Contact the publisher for a configured build."
    }
    return nil
  }

  static func updateChannels(in info: [String: Any]) -> Set<String> {
    info["MSUpdateChannel"] as? String == "prerelease" ? ["prerelease"] : []
  }

  func allowedChannels(for updater: SPUUpdater) -> Set<String> {
    Self.updateChannels(in: Bundle.main.infoDictionary ?? [:])
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
