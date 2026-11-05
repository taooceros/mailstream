//! Lets the binary load `libMailstreamKit.dylib`: from where the `mailstream` crate linked it
//! (`swift build`'s `.build/<config>`) during development, and from the bundle's `Frameworks`
//! once the binary sits in an app bundle's `Contents/MacOS`.
fn main() {
    let dir = std::env::var("DEP_MAILSTREAMKIT_DIR").expect("the mailstream crate names the engine's directory");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}
