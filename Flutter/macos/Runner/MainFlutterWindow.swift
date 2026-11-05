import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    self.contentViewController = flutterViewController
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
      default:
        result(FlutterMethodNotImplemented)
      }
    }

    RegisterGeneratedPlugins(registry: flutterViewController)

    let html = flutterViewController.registrar(forPlugin: "MailHtmlView")
    html.register(MailHtmlFactory(messenger: html.messenger), withId: "mailstream/html")

    super.awakeFromNib()
  }
}
