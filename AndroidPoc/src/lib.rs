//! Android entry reusing `GPUI/` (`mailstream-gpui`) + `FFI/` (`mailstream`)
//! + `Engine/` (`mailstream-engine`) on top of `gpui-mobile` + `gpui-fast`.

#[cfg(target_os = "android")]
extern crate gpui_mobile;

#[cfg(target_os = "android")]
mod android_entry {
    use std::path::PathBuf;

    use gpui::{App, Application};
    use gpui_mobile::android::{host, jni};
    use mailstream_engine::Config;
    use std::sync::atomic::{AtomicU32, Ordering};

    static INSET_TOP: AtomicU32 = AtomicU32::new(0);
    static INSET_BOTTOM: AtomicU32 = AtomicU32::new(0);
    static INSET_LEFT: AtomicU32 = AtomicU32::new(0);
    static INSET_RIGHT: AtomicU32 = AtomicU32::new(0);

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeInsets(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        top: i32,
        bottom: i32,
        left: i32,
        right: i32,
        density: f32,
    ) {
        let scale = if density > 0.0 { density } else { 1.0 };
        INSET_TOP.store((top.max(0) as f32 / scale).to_bits(), Ordering::Relaxed);
        INSET_BOTTOM.store((bottom.max(0) as f32 / scale).to_bits(), Ordering::Relaxed);
        INSET_LEFT.store((left.max(0) as f32 / scale).to_bits(), Ordering::Relaxed);
        INSET_RIGHT.store((right.max(0) as f32 / scale).to_bits(), Ordering::Relaxed);
        log::info!(
            "mailstream-android-poc: safe area insets logical top={:.1} bottom={:.1} left={:.1} right={:.1}",
            top.max(0) as f32 / scale,
            bottom.max(0) as f32 / scale,
            left.max(0) as f32 / scale,
            right.max(0) as f32 / scale,
        );
        if let Some(win) = jni::shared_platform().and_then(|p| p.0.primary_window()) {
            win.request_frame();
        }
    }

    fn android_safe_area_insets() -> (f32, f32, f32, f32) {
        let top = f32::from_bits(INSET_TOP.load(Ordering::Relaxed));
        let bottom = f32::from_bits(INSET_BOTTOM.load(Ordering::Relaxed));
        let left = f32::from_bits(INSET_LEFT.load(Ordering::Relaxed));
        let right = f32::from_bits(INSET_RIGHT.load(Ordering::Relaxed));
        if top > 0.0 || bottom > 0.0 || left > 0.0 || right > 0.0 {
            (top, bottom, left, right)
        } else {
            gpui_mobile::safe_area_insets()
        }
    }

    fn configure_android_engine(support_dir: PathBuf) {
        let mut config = Config::for_this_app().in_directory(support_dir);
        config.control_socket = false;
        config.oauth.open_url = std::sync::Arc::new(|url| {
            if let Err(err) = gpui_mobile::packages::url_launcher::launch_url(url) {
                log::warn!("mailstream-android-poc: open_url failed: {err}");
            }
        });
        let _ = mailstream::configure_engine(config);
    }

    fn launch_mailstream_ui(cx: &mut App) {
        mailstream_gpui::design::compact::set_safe_area_provider(android_safe_area_insets);
        mailstream_gpui::init(cx);
        let (window, app) = mailstream_gpui::open_window(mailstream_gpui::mobile_window_options(), cx);
        window
            .update(cx, |_, window, cx| {
                let focus = app.read_with(cx, |app, _| app.focus.clone());
                window.focus(&focus, cx);
                cx.activate(true);
            })
            .ok();
    }

    /// Entry point when hosted via `android-activity` (`NativeActivity` / `GpuiActivity`).
    #[unsafe(no_mangle)]
    fn android_main(app: android_activity::AndroidApp) {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("mailstream-gpui-poc"),
        );
        jni::install_panic_hook();
        log::info!("mailstream-android-poc: android_main entered");

        let support_dir = app
            .internal_data_path()
            .unwrap_or_else(|| PathBuf::from("/data/local/tmp/mailstream-poc"));
        configure_android_engine(support_dir);

        let _platform = jni::init_platform(&app);
        let Some(shared) = jni::shared_platform() else {
            log::error!("mailstream-android-poc: shared_platform() returned None");
            return;
        };

        Application::with_platform(shared.into_rc())
            .with_assets(mailstream_gpui::Assets)
            .run(|cx: &mut App| {
                launch_mailstream_ui(cx);
            });
    }

    /// Alternative entry point when hosted by a Java `Activity` + `SurfaceView` using
    /// `gpui-mobile`'s persistent render thread (`host.rs`), avoiding `ALooper` main-thread panics
    /// across Activity recreations.
    #[allow(dead_code)]
    pub fn start_hosted(support_dir: PathBuf) {
        configure_android_engine(support_dir);
        host::start_with_assets(mailstream_gpui::Assets, |cx| {
            launch_mailstream_ui(cx);
        });
    }
}
