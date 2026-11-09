import Cocoa
import FlutterMacOS
import XCTest
import Sparkle
@testable import Mailstream_Flutter

class RunnerTests: XCTestCase {

  private let valid: [String: Any] = [
    "SUPublicEDKey": Data(repeating: 7, count: 32).base64EncodedString(),
    "SUFeedURL": "https://github.com/taooceros/mailstream/releases/download/updates/appcast.xml",
    "SUVerifyUpdateBeforeExtraction": true,
    "SURequireSignedFeed": true,
    "SUSignedFeedFailureExpirationInterval": 0,
    "MSUpdateChannel": "prerelease",
  ]

  func testConfiguredChannelIsAccepted() {
    XCTAssertNil(AppUpdater.configurationError(valid))
  }

  func testMissingOrMalformedSigningKeyFailsClosed() {
    for key in ["", "$(MAILSTREAM_UPDATE_PUBLIC_KEY)", "not a public key",
                Data(repeating: 0, count: 31).base64EncodedString()] {
      var info = valid
      info["SUPublicEDKey"] = key
      XCTAssertNotNil(AppUpdater.configurationError(info))
    }
    XCTAssertNotNil(AppUpdater.configurationError([:]))
  }

  func testInsecureOrCredentialBearingFeedFailsClosed() {
    for feed in ["", "http://localhost/appcast.xml", "file:///tmp/appcast.xml",
                 "https://name:password@example.com/appcast.xml", "not a URL"] {
      var info = valid
      info["SUFeedURL"] = feed
      XCTAssertNotNil(AppUpdater.configurationError(info))
    }
  }

  func testArchiveAuthenticationCannotBeDisabled() {
    var info = valid
    info["SUVerifyUpdateBeforeExtraction"] = false
    XCTAssertNotNil(AppUpdater.configurationError(info))
  }

  func testSignedFeedCannotBeDisabledOrExpire() {
    for changes: [String: Any] in [
      ["SURequireSignedFeed": false],
      ["SUSignedFeedFailureExpirationInterval": 1728000],
    ] {
      XCTAssertNotNil(AppUpdater.configurationError(valid.merging(changes) { _, new in new }))
    }
  }

  func testPrereleaseIsExplicitAndStableDoesNotOptIn() {
    XCTAssertEqual(AppUpdater.updateChannels(in: valid), ["prerelease"])
    var stable = valid
    stable["MSUpdateChannel"] = "stable"
    XCTAssertNil(AppUpdater.configurationError(stable))
    XCTAssertEqual(AppUpdater.updateChannels(in: stable), [])
    stable["MSUpdateChannel"] = "unknown"
    XCTAssertNotNil(AppUpdater.configurationError(stable))
  }

  func testBuildVersionsCompareNumericallyWithoutDowngrades() {
    let comparator = SUStandardVersionComparator()
    XCTAssertEqual(comparator.compareVersion("10", toVersion: "2"), .orderedDescending)
    XCTAssertEqual(comparator.compareVersion("2", toVersion: "10"), .orderedAscending)
    XCTAssertEqual(comparator.compareVersion("2", toVersion: "2"), .orderedSame)
  }

  func testRunningApplicationMenuOwnsUpdater() throws {
    let appMenu = try XCTUnwrap(NSApp.mainMenu?.items.compactMap(\.submenu).first {
      $0.item(withTitle: "Check for Updates…") != nil
    })
    let check = try XCTUnwrap(appMenu.item(withTitle: "Check for Updates…"))
    let automatic = try XCTUnwrap(appMenu.item(withTitle: "Automatically Check for Updates"))
    let updater = try XCTUnwrap(check.target as? AppUpdater)
    XCTAssertTrue(automatic.target === updater)
    XCTAssertEqual(check.action, Selector(("checkForUpdates:")))
    XCTAssertEqual(automatic.action, Selector(("toggleAutomaticChecks:")))
    appMenu.update()
    XCTAssertTrue(check.isEnabled, "Unconfigured builds still explain why checking is unavailable")
    if AppUpdater.configurationError(Bundle.main.infoDictionary ?? [:]) != nil {
      XCTAssertFalse(automatic.isEnabled)
      XCTAssertEqual(automatic.state, .off)
    }
  }

}

/// The WebKit body's zoom to fit (MS-183), on synthetic mail in a real page 600 points wide: the
/// page lays out at a pinned width and zooms once, so mail of a width of its own zooms out to fit
/// and mail that widens with the view zooms out only by the share it overflows.
@MainActor
class MailHtmlZoomTests: XCTestCase {
  private func rows(_ count: Int) -> String {
    String(repeating: "<tr><td>A row of synthetic text that wraps where the column ends.</td><td>Second</td><td>Third</td></tr>", count: count)
  }

  /// The zoom the page's script gave `html`, 1 without one.
  private func zoom(of page: MailHtmlView) async throws -> Double {
    let value = try await page.evaluateJavaScript("document.documentElement.style.zoom") as? String ?? ""
    return Double(value) ?? 1
  }

  /// The page's zoom once it has stayed the same for 1.5 s after loading (at most 15 s).
  private func settledZoom(of page: MailHtmlView) async throws -> Double {
    var zoom = -1.0
    var since = Date()
    let deadline = Date().addingTimeInterval(15)
    while Date() < deadline {
      try await Task.sleep(nanoseconds: 100_000_000)
      guard !page.isLoading else { continue }
      let now = try await self.zoom(of: page)
      if now != zoom {
        zoom = now
        since = Date()
      } else if Date().timeIntervalSince(since) > 1.5 {
        break
      }
    }
    return zoom
  }

  /// `html` in a page `width` points wide; `check` runs while it shows.
  private func showing(_ html: String, width: CGFloat = 600, _ check: (MailHtmlView) async throws -> Void) async throws {
    let page = MailHtmlView(key: .init(owner: ["synthetic", "\(html.hashValue)"], html: html), inlineCache: "")
    let window = NSWindow(contentRect: NSRect(x: -20000, y: -20000, width: width, height: 400), styleMask: [.titled],
                          backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.contentView = NSView(frame: NSRect(x: 0, y: 0, width: width, height: 400))
    page.frame = NSRect(x: 0, y: 0, width: width, height: 400)
    window.contentView?.addSubview(page)
    window.orderFrontRegardless()
    defer {
      page.removeFromSuperview()
      window.orderOut(nil)
      page.close()
    }
    try await check(page)
  }

  func testMailWiderThanTheViewByAShareOfItZoomsOutByThatShare() async throws {
    // The shape of a newsletter written in a web editor: no doctype, plain paragraphs and a
    // `width: 100.775%` table, which overflowed again after every step out, down to 30%.
    let mail = "<div>Hello! Plain paragraphs of synthetic text.</div><div>More text.</div>"
      + "<table style=\"border-collapse: collapse; width: 100.775%;\" border=\"1\"><tbody>\(rows(6))</tbody></table>"
    try await showing(mail) { page in
      let zoom = try await settledZoom(of: page)
      XCTAssertEqual(zoom, 1 / 1.00775, accuracy: 0.005)
    }
  }

  func testMailOfAFixedWidthZoomsOutToFitAndAgainAtANewWidth() async throws {
    let mail = "<!DOCTYPE html><html><body><table width=\"900\" border=\"0\"><tbody>\(rows(6))</tbody></table></body></html>"
    try await showing(mail) { page in
      let wide = try await settledZoom(of: page)
      XCTAssertEqual(wide, 600.0 / 900.0, accuracy: 0.02)
      page.frame.size.width = 400
      let narrow = try await settledZoom(of: page)
      XCTAssertEqual(narrow, 400.0 / 900.0, accuracy: 0.02)
    }
  }

  func testFixedWidthMailWithATableThatWidensFitsBoth() async throws {
    // Laid out at the 900 px table's width, the 101% table is 909 px: the zoom fits both.
    let mail = "<table width=\"900\" border=\"0\"><tbody>\(rows(3))</tbody></table>"
      + "<table style=\"width: 101%\" border=\"1\"><tbody>\(rows(3))</tbody></table>"
    try await showing(mail) { page in
      let zoom = try await settledZoom(of: page)
      XCTAssertEqual(zoom, 600.0 / 909.0, accuracy: 0.02)
    }
  }

  func testMailThatFitsIsNotZoomed() async throws {
    try await showing("<p>Synthetic text that wraps at the column.</p><table width=\"100%\"><tbody>\(rows(3))</tbody></table>") { page in
      let zoom = try await settledZoom(of: page)
      XCTAssertEqual(zoom, 1)
    }
  }

  func testZoomedMailReportsTheHeightItShowsAt() async throws {
    // A 900 px newsletter in a 480 point view: zoomed to about half, its 15 px text keeps
    // WebKit's minimum font size, so it is taller than its unzoomed height times the zoom. The
    // height Flutter gets must reach the last row's bottom as drawn, or the mail is cut off.
    let row = "<tr><td style=\"padding:14px 24px;font:15px Helvetica,sans-serif\">A synthetic row, 900 px wide.</td></tr>"
    let mail = "<html><body><table width=\"100%\" style=\"height:100%\"><tr><td align=\"center\">"
      + "<table width=\"900\">\(String(repeating: row, count: 24))<tr><td id=\"last\">End</td></tr></table></td></tr></table></body></html>"
    try await showing(mail, width: 480) { page in
      let zoom = try await settledZoom(of: page)
      XCTAssertLessThan(zoom, 0.6)
      let bottom = try await page.evaluateJavaScript("document.getElementById('last').getBoundingClientRect().bottom") as? Double ?? 0
      let height = page.height(forWidth: 480)?.first as? Double ?? 0
      XCTAssertGreaterThanOrEqual(height, bottom)
      XCTAssertLessThan(height, bottom + 12)
    }
  }
}
