import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';

/// Bundles the engine with the app: `libmailstream.dylib`, the UniFFI crate the bindings call with
/// the Rust engine in it (asset `package:mailstream/uniffi:mailstream`). `scripts/ffi.sh` at the
/// repository root builds it, for this Mac's architecture (arm64) only; the release build is used.
void main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets || input.config.code.targetArchitecture != Architecture.arm64) return;
    final root = input.packageRoot.resolve('../../../');
    final libraries = {
      'uniffi:mailstream': root.resolve('FFI/target/release/libmailstream.dylib'),
    };
    for (final MapEntry(key: name, value: file) in libraries.entries) {
      if (!File.fromUri(file).existsSync()) {
        throw BuildError(message: 'No ${file.toFilePath()}: run `CONFIG=release ./scripts/ffi.sh` at the repository root first.');
      }
      output.assets.code.add(CodeAsset(package: input.packageName, name: name, linkMode: DynamicLoadingBundled(), file: file));
      output.dependencies.add(file);
    }
  });
}
