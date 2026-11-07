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
