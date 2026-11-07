import base64
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("prepare_update", ROOT / "scripts/prepare-update.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class UpdatePackageTests(unittest.TestCase):
    def setUp(self):
        self.info = {
            "CFBundleIdentifier": "dev.mailstream.MailstreamFlutter",
            "CFBundleVersion": "2",
            "SUPublicEDKey": base64.b64encode(bytes(range(32))).decode(),
            "SUFeedURL": package.FEED_ROOT + "appcast.xml",
            "SUVerifyUpdateBeforeExtraction": True,
        }

    def test_production_configuration(self):
        self.assertEqual(package.validate_info(self.info), "2")

    def test_unconfigured_or_insecure_channel_is_rejected(self):
        for key, value in [("SUPublicEDKey", ""), ("SUPublicEDKey", "not a key"),
                           ("SUVerifyUpdateBeforeExtraction", False),
                           ("SUFeedURL", "http://example.com/feed.xml"),
                           ("SUFeedURL", package.FEED_ROOT.replace("updates", "nightly") + "appcast.xml"),
                           ("CFBundleIdentifier", "dev.mailstream.MailstreamGPUI"),
                           ("CFBundleVersion", "revision-sha")]:
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                package.validate_info(self.info | {key: value})

    def test_appcast_names_exact_package_bytes(self):
        with tempfile.TemporaryDirectory() as folder:
            archive = Path(folder) / "Mailstream-Flutter-2.zip"
            archive.write_bytes(b"package bytes")
            feed = Path(folder) / "appcast.xml"
            signature = base64.b64encode(bytes(64)).decode()
            xml = f'''<rss xmlns:sparkle="{package.SPARKLE_NS}"><channel><item>
              <sparkle:version>2</sparkle:version>
              <enclosure url="{package.FEED_ROOT}{archive.name}" length="13"
                sparkle:edSignature="{signature}"/></item></channel></rss>'''
            feed.write_text(xml)
            self.assertEqual(package.validate_feed(feed, archive, "2"), signature)
            for changed in [xml.replace('length="13"', 'length="12"'),
                            xml.replace(archive.name, "another.zip"),
                            xml.replace("<sparkle:version>2", "<sparkle:version>1"),
                            xml.replace(signature, "unsigned")]:
                feed.write_text(changed)
                with self.assertRaises(ValueError):
                    package.validate_feed(feed, archive, "2")

    def test_real_signature_matches_pinned_key_not_just_signer(self):
        # An ephemeral test key never enters Keychain or the repository.
        with tempfile.TemporaryDirectory() as folder:
            archive = Path(folder) / "archive.zip"
            archive.write_bytes(b"original signed package")
            output = subprocess.check_output([
                "swift", "-e", '''import CryptoKit; import Foundation
let key = Curve25519.Signing.PrivateKey()
let other = Curve25519.Signing.PrivateKey()
let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
print(key.publicKey.rawRepresentation.base64EncodedString())
print(try key.signature(for: data).base64EncodedString())
print(other.publicKey.rawRepresentation.base64EncodedString())''', str(archive)], text=True).splitlines()
            public_key, signature, other_key = output
            def verify(key):
                return subprocess.run(["swift", str(ROOT / "scripts/verify-update.swift"), key,
                                       str(archive), signature], capture_output=True).returncode
            self.assertEqual(verify(public_key), 0)
            self.assertNotEqual(verify(other_key), 0)
            archive.write_bytes(b"tampered signed package")
            self.assertNotEqual(verify(public_key), 0)


if __name__ == "__main__":
    unittest.main()
