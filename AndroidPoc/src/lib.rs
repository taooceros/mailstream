//! Minimal isolated Android PoC reusing `GPUI/` (`mailstream-gpui`) + `FFI/` (`mailstream`)
//! + `Engine/` (`mailstream-engine`) directly on top of `gpui-mobile` + upstream `gpui` (`gpui-pre`).

#[cfg(target_os = "android")]
extern crate gpui_mobile;

#[cfg(target_os = "android")]
mod android_entry {
    use std::path::PathBuf;

    use gpui::{App, Application};
    use gpui_mobile::android::{host, jni};
    use mailstream_engine::Config;

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
