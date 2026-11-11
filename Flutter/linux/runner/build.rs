//! Rust bindings of Flutter's Linux embedder (`flutter_linux/flutter_linux.h`) and of the plugin
//! registrant (`generated_plugin_registrant.h`), made with bindgen from the headers the Flutter
//! build put in `FLUTTER_MANAGED_DIR` (`linux/flutter`, which runner/CMakeLists.txt passes; the
//! embedder's headers are under its `ephemeral/`). Only Flutter's own items are bound: GLib, GIO
//! and GTK types are gtk-rs's `-sys` ones, so values pass between the two as they are.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-env-changed=FLUTTER_MANAGED_DIR");
    let managed = PathBuf::from(
        std::env::var_os("FLUTTER_MANAGED_DIR")
            .expect("FLUTTER_MANAGED_DIR: build the runner through Flutter (flutter build linux), which sets it"),
    );
    println!("cargo:rerun-if-changed={}", managed.join("generated_plugin_registrant.h").display());
    println!("cargo:rerun-if-changed={}", managed.join("ephemeral/flutter_linux").display());
    // GTK's include folders; the libraries are linked by CMake, not cargo.
    let gtk = pkg_config::Config::new().cargo_metadata(false).probe("gtk+-3.0").expect("GTK 3 (pkg-config gtk+-3.0)");
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", managed.display()))
        .clang_arg(format!("-I{}", managed.join("ephemeral").display()))
        .clang_args(gtk.include_paths.iter().map(|path| format!("-I{}", path.display())))
        .rust_edition(bindgen::RustEdition::Edition2024)
        .allowlist_function("fl_.*")
        .allowlist_type("_?Fl.*")
        .allowlist_var("FL_.*")
        // GLib, GIO, GTK and the libraries under them come from gtk-rs (`src/flutter.rs` imports
        // them for the generated code); GLib's primitive aliases (`gchar`, `gboolean`) are bound
        // here, as the same C types.
        .blocklist_type("_?(G|Gtk|Gdk|Pango|Atk|cairo)[A-Za-z0-9_]*")
        .prepend_enum_name(false)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("flutter_linux bindings");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    bindings.write_to_file(out.join("flutter_linux.rs")).expect("bindings written");
}
