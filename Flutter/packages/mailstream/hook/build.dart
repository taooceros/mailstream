import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';

/// Bundles the engine with the app: the UniFFI crate the bindings call, with the Rust engine in it
/// (asset `package:mailstream/uniffi:mailstream`). `scripts/ffi.py` at the repository root builds
/// it, and the release build is used: on macOS `libmailstream.dylib` for this Mac's architecture
/// (arm64) only, on Windows (x64) `mailstream.dll`, on Linux (x64, arm64) `libmailstream.so` for
/// the host's architecture (`cd FFI && cargo build --release -p mailstream`).
void main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets) return;
    final code = input.config.code;
    final root = input.packageRoot.resolve('../../../');
    final Uri library;
    if (code.targetOS == OS.macOS && code.targetArchitecture == Architecture.arm64) {
      library = root.resolve('FFI/target/release/libmailstream.dylib');
    } else if (code.targetOS == OS.windows && code.targetArchitecture == Architecture.x64) {
      library = root.resolve('FFI/target/release/mailstream.dll');
    } else if (code.targetOS == OS.linux && const [Architecture.x64, Architecture.arm64].contains(code.targetArchitecture)) {
      library = root.resolve('FFI/target/release/libmailstream.so');
    } else {
      return;
    }
    if (!File.fromUri(library).existsSync()) {
      throw BuildError(message: 'No ${library.toFilePath()}: run `CONFIG=release python3 scripts/ffi.py` at the repository root first.');
    }
    output.assets.code.add(
      CodeAsset(package: input.packageName, name: 'uniffi:mailstream', linkMode: DynamicLoadingBundled(), file: library),
    );
    output.dependencies.add(library);
  });
}
