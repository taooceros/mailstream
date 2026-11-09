import Cocoa
import FlutterMacOS
import macos_window_utils

class MainFlutterWindow: NSWindow {
  private var chrome: FlutterMethodChannel?

  /// A view-level destination: Flutter's view handles drags before NSWindow gets them.
  private var composeFiles: ComposeFileDropView?
  private var composing = false
  /// Whether Flutter shows its frame rate meter (`flutter.showFrameRate`), for the check mark of
  /// View ▸ Show Frame Rate.
  private var frameRateShown = false

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
      case "openMailLink":
        // A link tapped in a HyperRender body, under the WebView's rules.
        guard let value = call.arguments as? String, let url = URL(string: value), MailLink.open(url) else {
          return result(FlutterError(code: "link", message: "Unsupported mail link", details: nil))
        }
        result(nil)
      case "frameRate":
        self?.frameRateShown = call.arguments as? Bool ?? false
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
    let destination = ComposeFileDropView(frame: flutterViewController.view.bounds, channel: files)
    destination.autoresizingMask = [.width, .height]
    flutterViewController.view.addSubview(destination)
    composeFiles = destination
    files.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "accept":
        self?.composeFiles?.acceptsFiles = call.arguments as? Bool ?? false
        result(nil)
      case "editing":
        self?.composing = call.arguments as? Bool ?? false
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
        do {
          result(try ComposeFileDropView.pastedFiles().map(\.path))
        } catch {
          result(FlutterError(code: "clipboard", message: error.localizedDescription, details: nil))
        }
      default:
        result(FlutterMethodNotImplemented)
      }
    }

    RegisterGeneratedPlugins(registry: flutterViewController)

    let html = flutterViewController.registrar(forPlugin: "MailHtmlView")
    html.register(MailHtmlFactory(messenger: html.messenger), withId: "mailstream/html")

    super.awakeFromNib()
    // AppKit consumes the system Edit > Paste shortcut before Flutter's key shortcuts.
    // Route it to the same focused Flutter action as a framework keyboard paste.
    if let paste = NSApp.mainMenu?.item(withTitle: "Edit")?.submenu?.items.first(where: { $0.action == #selector(NSText.paste(_:)) }) {
      paste.target = self
      paste.action = #selector(pasteIntoComposer(_:))
    }
  }

  /// The app menu's Settings… (⌘,), which Flutter opens.
  @objc func showSettings(_ sender: Any?) {
    chrome?.invokeMethod("settings", arguments: nil)
  }

  /// View ▸ Show Frame Rate (⌥⌘P), which Flutter toggles in settings.json.
  @objc func toggleFrameRate(_ sender: Any?) {
    chrome?.invokeMethod("toggleFrameRate", arguments: nil)
  }

  override func validateMenuItem(_ item: NSMenuItem) -> Bool {
    if item.action == #selector(toggleFrameRate(_:)) {
      item.state = frameRateShown ? .on : .off
      return true
    }
    return super.validateMenuItem(item)
  }

  @objc private func pasteIntoComposer(_ sender: Any?) {
    if composing, let composeFiles, composeFiles.acceptsFiles {
      composeFiles.paste()
      return
    }
    NSApp.sendAction(#selector(NSText.paste(_:)), to: nil, from: sender)
  }

}

/// Transparent to normal clicks, but registered as a drag destination above Flutter's view.
private final class ComposeFileDropView: NSView {
  var acceptsFiles = false
  private let channel: FlutterMethodChannel

  init(frame: NSRect, channel: FlutterMethodChannel) {
    self.channel = channel
    super.init(frame: frame)
    registerForDraggedTypes([.fileURL])
  }

  required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

  override func hitTest(_ point: NSPoint) -> NSView? { nil }

  func paste() {
    channel.invokeMethod("paste", arguments: nil)
  }

  /// Existing files win over alternate image representations on the same clipboard.
  static func fileURLs(on pasteboard: NSPasteboard) -> [URL] {
    let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    return urls.filter { (try? $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true }
  }

  static func pastedFiles() throws -> [URL] {
    let urls = fileURLs(on: .general)
    if !urls.isEmpty { return urls }
    // Screenshots and copied image pixels have no file URL. Materialize them once for the
    // existing send attachment pipeline; text-only paste remains Flutter's normal text paste.
    return try (NSPasteboard.general.pasteboardItems ?? []).compactMap { item in
      let data: Data?
      if let png = item.data(forType: .png) {
        data = png
      } else if let tiff = item.data(forType: .tiff) {
        data = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
      } else {
        return nil
      }
      guard let data else { return nil }
      let url = FileManager.default.temporaryDirectory.appendingPathComponent("Mailstream-pasted-\(UUID().uuidString).png")
      try data.write(to: url, options: .atomic)
      return url
    }
  }

  override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
    guard acceptsFiles, !Self.fileURLs(on: sender.draggingPasteboard).isEmpty else { return [] }
    channel.invokeMethod("hover", arguments: true)
    return .copy
  }

  override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
    acceptsFiles && !Self.fileURLs(on: sender.draggingPasteboard).isEmpty ? .copy : []
  }

  override func draggingExited(_ sender: (any NSDraggingInfo)?) {
    channel.invokeMethod("hover", arguments: false)
  }

  override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
    channel.invokeMethod("hover", arguments: false)
    let urls = Self.fileURLs(on: sender.draggingPasteboard)
    guard acceptsFiles, !urls.isEmpty else { return false }
    channel.invokeMethod("drop", arguments: urls.map(\.path))
    return true
  }
}

/// The window's glass, only a backdrop: clicks go to the Flutter view above it.
private final class BackdropGlass: NSGlassEffectView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
