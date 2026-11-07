import Cocoa
import FlutterMacOS
import XCTest
@testable import Mailstream_Flutter

class RunnerTests: XCTestCase {

  private let valid: [String: Any] = [
    "SUPublicEDKey": Data(repeating: 7, count: 32).base64EncodedString(),
    "SUFeedURL": "https://github.com/taooceros/mailstream/releases/download/updates/appcast.xml",
    "SUVerifyUpdateBeforeExtraction": true,
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
    for feed in ["http://localhost/appcast.xml", "file:///tmp/appcast.xml",
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

}
