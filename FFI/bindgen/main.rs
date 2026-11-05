//! `cargo run -p mailstream-bindgen -- target/debug/libmailstream.dylib --out-dir <dir>`: the
//! crate's Dart bindings, read from the compiled library's UniFFI metadata.
fn main() -> anyhow::Result<()> {
    uniffi_dart::main()
}
