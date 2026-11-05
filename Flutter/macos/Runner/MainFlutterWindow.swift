import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  private var chrome: FlutterMethodChannel?

  /// Files for the compose window: dropped on the window or chosen in an open panel.
  private var files: FlutterMethodChannel?

  /// Whether a drop attaches: Flutter turns it on while the compose window is open.
  private var takesFiles = false

  /// The navigation's Liquid Glass panel. It sits under the Flutter view, which paints the
  /// window's surface everywhere but the panel's rect (`GlassPanel` in `lib/ui/glass.dart`), so
  /// the desktop shows through it, blurred by the system's glass.
  private let panel = BackdropGlass()

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    flutterViewController.backgroundColor = .clear
    let container = NSViewController()
    container.view = NSView()
    container.addChild(flutterViewController)
    flutterViewController.view.frame = container.view.bounds
    flutterViewController.view.autoresizingMask = [.width, .height]
    panel.isHidden = true
    // Fixed left edge, width and distance to the top and bottom: it follows the window's height
    // as the window resizes, before Flutter lays out again.
    panel.autoresizingMask = [.height, .maxXMargin]
    container.view.addSubview(panel)
    container.view.addSubview(flutterViewController.view)
    self.contentViewController = container
    isOpaque = false
    backgroundColor = .clear
    // Content runs under the title bar, as in the native app: the sidebar reaches the top and the
    // traffic lights sit over it.
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
      case "panel":
        // The panel's rect in Flutter's coordinates (top-left origin) and its corner radius;
        // no arguments hide it.
        self?.placePanel(call.arguments as? [String: Double])
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

    super.awakeFromNib()
  }

  /// The app menu's Settings… (⌘,), which Flutter opens.
  @objc func showSettings(_ sender: Any?) {
    chrome?.invokeMethod("settings", arguments: nil)
  }

  private func placePanel(_ rect: [String: Double]?) {
    guard let rect, let x = rect["x"], let y = rect["y"], let width = rect["width"], let height = rect["height"],
          let container = panel.superview
    else {
      panel.isHidden = true
      return
    }
    panel.frame = NSRect(x: x, y: container.bounds.height - y - height, width: width, height: height)
    panel.cornerRadius = rect["radius"] ?? 0
    panel.isHidden = false
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

/// The glass under the navigation: only a backdrop, so clicks go to the Flutter view over it.
private final class BackdropGlass: NSGlassEffectView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
