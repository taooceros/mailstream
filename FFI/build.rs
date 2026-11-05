//! Links the Swift engine, `libMailstreamKit.dylib`, which `swift build` puts in the package's
//! `.build/<config>` (`MAILSTREAM_KIT_DIR` overrides; `MAILSTREAM_KIT_CONFIG` picks release).
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MAILSTREAM_KIT_DIR");
    println!("cargo:rerun-if-env-changed=MAILSTREAM_KIT_CONFIG");
    let dir = std::env::var_os("MAILSTREAM_KIT_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let config = std::env::var("MAILSTREAM_KIT_CONFIG").unwrap_or_else(|_| "debug".into());
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.build").join(config)
    });
    let dir = dir.canonicalize().unwrap_or_else(|_| {
        panic!("no Swift engine at {}: run `swift build --product MailstreamKit` first", dir.display())
    });
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=MailstreamKit");
    // Development hosts find the dylib where swift build left it; app bundles embed it and add
    // their own rpath (@executable_path/../Frameworks).
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    println!("cargo:rerun-if-changed={}", dir.join("libMailstreamKit.dylib").display());
}
