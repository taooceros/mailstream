#!/usr/bin/env swift
import CryptoKit
import Foundation

// No private key or Keychain access: verify the final package against the app's pinned key.
do {
  guard CommandLine.arguments.count == 4,
        let key = Data(base64Encoded: CommandLine.arguments[1]),
        let signature = Data(base64Encoded: CommandLine.arguments[3]) else {
    throw NSError(domain: "MailstreamUpdate", code: 1,
                  userInfo: [NSLocalizedDescriptionKey: "Expected public key, archive path, signature"])
  }
  let publicKey = try Curve25519.Signing.PublicKey(rawRepresentation: key)
  let archive = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]), options: .mappedIfSafe)
  guard publicKey.isValidSignature(signature, for: archive) else {
    throw NSError(domain: "MailstreamUpdate", code: 2,
                  userInfo: [NSLocalizedDescriptionKey: "Archive does not match the app's pinned update key"])
  }
} catch {
  FileHandle.standardError.write(Data("verify-update: \(error.localizedDescription)\n".utf8))
  exit(1)
}
