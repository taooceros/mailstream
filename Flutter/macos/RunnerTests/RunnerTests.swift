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

/// The WebKit body's zoom to fit (MS-183), on synthetic mail in a real page 600 points wide: mail
/// of a width of its own zooms out to fit, mail that widens with the view keeps its size.
@MainActor
class MailHtmlZoomTests: XCTestCase {
  private func rows(_ count: Int) -> String {
    String(repeating: "<tr><td>A row of synthetic text that wraps where the column ends.</td><td>Second</td><td>Third</td></tr>", count: count)
  }

  /// The page's zoom once it has stayed the same for 1.5 s after loading (at most 15 s).
  private func settledZoom(_ html: String) async throws -> CGFloat {
    let page = MailHtmlView(key: .init(owner: ["synthetic", "\(html.hashValue)"], html: html), inlineCache: "")
    let window = NSWindow(contentRect: NSRect(x: -20000, y: -20000, width: 600, height: 400), styleMask: [.titled],
                          backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.contentView = NSView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
    page.frame = NSRect(x: 0, y: 0, width: 600, height: 400)
    window.contentView?.addSubview(page)
    window.orderFrontRegardless()
    defer {
      page.removeFromSuperview()
      window.orderOut(nil)
      page.close()
    }
    var zoom = page.pageZoom
    var since = Date()
    let deadline = Date().addingTimeInterval(15)
    while Date() < deadline {
      try await Task.sleep(nanoseconds: 100_000_000)
      if page.pageZoom != zoom {
        zoom = page.pageZoom
        since = Date()
      } else if !page.isLoading, Date().timeIntervalSince(since) > 1.5 {
        break
      }
    }
    return page.pageZoom
  }

  func testMailWiderThanTheViewByAShareOfItKeepsItsSize() async throws {
    // The shape of a newsletter written in a web editor: no doctype, plain paragraphs and a
    // `width: 100.775%` table, which overflowed again after every step out, down to 30%.
    let mail = "<div>Hello! Plain paragraphs of synthetic text.</div><div>More text.</div>"
      + "<table style=\"border-collapse: collapse; width: 100.775%;\" border=\"1\"><tbody>\(rows(6))</tbody></table>"
    let zoom = try await settledZoom(mail)
    XCTAssertGreaterThan(zoom, 0.98)
  }

  func testMailOfAFixedWidthZoomsOutToFit() async throws {
    let mail = "<!DOCTYPE html><html><body><table width=\"900\" border=\"0\"><tbody>\(rows(6))</tbody></table></body></html>"
    let zoom = try await settledZoom(mail)
    XCTAssertEqual(zoom, 600.0 / 900.0, accuracy: 0.02)
  }

  func testFixedWidthMailWithATableThatWidensStillFitsItsFixedPart() async throws {
    let mail = "<table width=\"900\" border=\"0\"><tbody>\(rows(3))</tbody></table>"
      + "<table style=\"width: 101%\" border=\"1\"><tbody>\(rows(3))</tbody></table>"
    let zoom = try await settledZoom(mail)
    XCTAssertEqual(zoom, 600.0 / 900.0, accuracy: 0.03)
  }
}
