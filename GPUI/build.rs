//! Lets the binary load `libMailstreamKit.dylib` from where the `mailstream` crate linked it
//! (`swift build`'s output during development).
fn main() {
    let dir = std::env::var("DEP_MAILSTREAMKIT_DIR").expect("the mailstream crate names the engine's directory");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
}
