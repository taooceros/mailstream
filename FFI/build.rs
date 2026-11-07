//! Links the Swift engine, `libMailstreamKit.dylib` (`MailstreamKit.dll` through its import library
//! `MailstreamKit.lib` on Windows), which `swift build` puts in the package's `.build/<config>`
//! (`MAILSTREAM_KIT_DIR` overrides; `MAILSTREAM_KIT_CONFIG` picks release).
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
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=MailstreamKit");
    if target == "windows" {
        // Windows has no rpath: the DLL is found beside the executable (the Flutter runner's
        // build copies it there) or on PATH.
        println!("cargo:rerun-if-changed={}", dir.join("MailstreamKit.lib").display());
    } else {
        // Development hosts find the dylib where swift build left it; app bundles embed it and add
        // their own rpath (@executable_path/../Frameworks). The rpath reaches this package's own
        // binaries; other crates' binaries add it from `DEP_MAILSTREAMKIT_DIR`.
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
        println!("cargo:rerun-if-changed={}", dir.join("libMailstreamKit.dylib").display());
    }
    // On macOS cdylib, reserve Mach-O header padding so `install_name_tool` (run when Flutter
    // bundles or tests native assets) can rewrite IDs and rpaths without relinking.
    if target == "macos" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-headerpad_max_install_names");
    }
    println!("cargo:dir={}", dir.display());
}
