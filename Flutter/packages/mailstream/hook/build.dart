import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';

/// Bundles the engine with the app: the UniFFI crate the bindings call (asset
/// `package:mailstream/uniffi:mailstream`) and the Swift engine it links. `scripts/ffi.sh` at the
/// repository root builds both, release builds are used: on macOS `libmailstream.dylib` and
/// `libMailstreamKit.dylib` for this Mac's architecture (arm64) only; on Windows (x64)
/// `mailstream.dll` and `MailstreamKit.dll`. The Swift runtime's DLLs the engine needs on Windows
/// are copied next to the app by `windows/CMakeLists.txt`.
void main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets) return;
    final code = input.config.code;
    final root = input.packageRoot.resolve('../../../');
    final Map<String, Uri> libraries;
    if (code.targetOS == OS.macOS && code.targetArchitecture == Architecture.arm64) {
      libraries = {
        'uniffi:mailstream': root.resolve('FFI/target/release/libmailstream.dylib'),
        'engine': root.resolve('.build/release/libMailstreamKit.dylib'),
      };
    } else if (code.targetOS == OS.windows && code.targetArchitecture == Architecture.x64) {
      libraries = {
        'uniffi:mailstream': root.resolve('FFI/target/release/mailstream.dll'),
        'engine': root.resolve('.build/release/MailstreamKit.dll'),
      };
    } else {
      return;
    }
    for (final MapEntry(key: name, value: file) in libraries.entries) {
      if (!File.fromUri(file).existsSync()) {
        throw BuildError(message: 'No ${file.toFilePath()}: run `CONFIG=release ./scripts/ffi.sh` at the repository root first.');
      }
      output.assets.code.add(CodeAsset(package: input.packageName, name: name, linkMode: DynamicLoadingBundled(), file: file));
      output.dependencies.add(file);
    }
  });
}
