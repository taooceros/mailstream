#![cfg_attr(all(not(debug_assertions), not(feature = "shots"), target_os = "windows"), windows_subsystem = "windows")]

#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn main() {
    mailstream_gpui::desktop_main();
}

#[cfg(any(target_os = "ios", target_os = "android"))]
fn main() {}
