/// Mailstream's engine (accounts, sync, cache, threading, tagging) running in this process, as
/// UniFFI generates it from the `FFI/` crate and the engine's records (`Engine/`). `lib/src` is
/// generated: see this package's README.
library;

export 'src/bindings.dart';
