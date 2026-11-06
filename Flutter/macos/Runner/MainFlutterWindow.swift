import Cocoa
import FlutterMacOS
import macos_window_utils

class MainFlutterWindow: NSWindow {
  private var chrome: FlutterMethodChannel?

  /// Files for the compose column: dropped on the window or chosen in an open panel.
  private var files: FlutterMethodChannel?

  /// Whether a drop attaches: Flutter turns it on while the compose column is open.
  private var takesFiles = false

  /// The window's Liquid Glass: it fills the window under the Flutter view, which leaves it open
  /// around the islands it paints (the list, the conversation), so the desktop shows through
  /// the sidebar and the gaps, blurred by the system's glass.
  private let backdrop = BackdropGlass()

  override func awakeFromNib() {
    // macos_ui's controls follow the window (main or not, the accent color) through
    // macos_window_utils, which hosts the Flutter view in its own controller.
    let utils = MacOSWindowUtilsViewController()
    let flutterViewController = utils.flutterViewController
    self.contentViewController = utils
    MainFlutterWindowManipulator.start(mainFlutterWindow: self)
    // The controller's view is a window-background material; masked out, so the glass shows the
    // desktop rather than gray.
    (utils.view as? NSVisualEffectView)?.maskImage = NSImage(size: NSSize(width: 1, height: 1))
    backdrop.frame = utils.view.bounds
    backdrop.autoresizingMask = [.width, .height]
    backdrop.cornerRadius = 16
    utils.view.addSubview(backdrop, positioned: .below, relativeTo: flutterViewController.view)
    isOpaque = false
    backgroundColor = .clear
    // Content runs under the title bar: the sidebar reaches the top and the traffic lights sit
    // over it.
    styleMask.insert(.fullSizeContentView)
    titlebarAppearsTransparent = true
    titleVisibility = .hidden
    isMovableByWindowBackground = false
    minSize = NSSize(width: 820, height: 520)
    setContentSize(NSSize(width: 1240, height: 800))
    center()
    setFrameAutosaveName("MailstreamFlutterMain")

    // Flutter draws the whole title bar area, so it asks for window drags and zooms itself.
    let chrome = FlutterMethodChannel(name: "mailstream/window", binaryMessenger: flutterViewController.engine.binaryMessenger)
    chrome.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "drag":
        if let self, let event = NSApp.currentEvent { self.performDrag(with: event) }
        result(nil)
      case "zoom":
        self?.performZoom(nil)
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    self.chrome = chrome

    // `accept` turns drops on and off; `pick` answers the paths chosen in an open panel, `pasted`
    // the files on the clipboard. Flutter hears `hover` (a drag of files over the window starts or
    // ends) and `drop` (their paths).
    let files = FlutterMethodChannel(name: "mailstream/files", binaryMessenger: flutterViewController.engine.binaryMessenger)
    files.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "accept":
        self?.takesFiles = call.arguments as? Bool ?? false
        result(nil)
      case "pick":
        guard let self else { return result([String]()) }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.beginSheetModal(for: self) { response in
          result(response == .OK ? panel.urls.map(\.path) : [String]())
        }
      case "pasted":
        result(self?.files(on: .general).map(\.path) ?? [String]())
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    self.files = files
    registerForDraggedTypes([.fileURL])

    RegisterGeneratedPlugins(registry: flutterViewController)

    let html = flutterViewController.registrar(forPlugin: "MailHtmlView")
    html.register(MailHtmlFactory(messenger: html.messenger), withId: "mailstream/html")
    let glass = flutterViewController.registrar(forPlugin: "Glass")
    glass.register(GlassFactory(), withId: "mailstream/glass")

    super.awakeFromNib()
  }

  /// The app menu's Settings… (⌘,), which Flutter opens.
  @objc func showSettings(_ sender: Any?) {
    chrome?.invokeMethod("settings", arguments: nil)
  }

  // MARK: - Dropped and pasted files

  /// The files on a pasteboard, without folders.
  private func files(on pasteboard: NSPasteboard) -> [URL] {
    let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    return urls.filter { (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) != true }
  }

  private func droppedFiles(_ info: any NSDraggingInfo) -> [URL] { files(on: info.draggingPasteboard) }

  // NSWindow declares no implementations of these, so they need `@objc` to reach AppKit.
  @objc func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
    guard takesFiles, !droppedFiles(sender).isEmpty else { return [] }
    files?.invokeMethod("hover", arguments: true)
    return .copy
  }

  @objc func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
    takesFiles && !droppedFiles(sender).isEmpty ? .copy : []
  }

  @objc func draggingExited(_ sender: (any NSDraggingInfo)?) {
    files?.invokeMethod("hover", arguments: false)
  }

  @objc func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
    files?.invokeMethod("hover", arguments: false)
    let urls = droppedFiles(sender)
    guard takesFiles, !urls.isEmpty else { return false }
    files?.invokeMethod("drop", arguments: urls.map(\.path))
    return true
  }
}

/// Glass over Flutter's own content, as Flutter `AppKitView`s of type `mailstream/glass` (a
/// `radius` creation argument rounds them): the toolbars' capsules. AppKit composites the Flutter
/// layers under a platform view below it, so this glass refracts the list and the message that
/// scroll under it.
final class GlassFactory: NSObject, FlutterPlatformViewFactory {
  func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
    let radius = (args as? [String: Any])?["radius"] as? Double ?? 0
    let glass = BackdropGlass()
    glass.cornerRadius = radius
    glass.autoresizingMask = [.width, .height]
    // The glass paints a tinted rect past its rounded shape; the clip leaves only the capsule.
    let clip = BackdropView()
    clip.wantsLayer = true
    clip.layer?.cornerRadius = radius
    clip.layer?.cornerCurve = .continuous
    clip.layer?.masksToBounds = true
    clip.addSubview(glass)
    return clip
  }

  func createArgsCodec() -> (any FlutterMessageCodec & NSObjectProtocol)? {
    FlutterStandardMessageCodec.sharedInstance()
  }
}

/// Glass that is only a backdrop: clicks go to the Flutter view, which draws the controls on it.
private final class BackdropGlass: NSGlassEffectView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

private final class BackdropView: NSView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }

  override func layout() {
    super.layout()
    subviews.forEach { $0.frame = bounds }
  }
}
