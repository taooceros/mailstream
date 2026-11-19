#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn main() {
    mailstream_gpui::desktop_main();
}

#[cfg(any(target_os = "ios", target_os = "android"))]
fn main() {}
