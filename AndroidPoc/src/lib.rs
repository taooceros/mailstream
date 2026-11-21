//! Android entry reusing `GPUI/` (`mailstream-gpui`) + `FFI/` (`mailstream`)
//! + `Engine/` (`mailstream-engine`) on top of `gpui-mobile` + `gpui-fast`.

#[cfg(target_os = "android")]
extern crate gpui_mobile;

#[cfg(target_os = "android")]
mod android_entry {
    use std::path::PathBuf;

    use gpui::{App, AppContext as _, Application};
    use gpui_mobile::android::{host, jni, text_input};
    use mailstream_engine::Config;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    static INSET_TOP: AtomicU32 = AtomicU32::new(0);
    static INSET_BOTTOM: AtomicU32 = AtomicU32::new(0);
    static INSET_LEFT: AtomicU32 = AtomicU32::new(0);
    static INSET_RIGHT: AtomicU32 = AtomicU32::new(0);
    static IME_BOTTOM: AtomicU32 = AtomicU32::new(0);
    static IME_TARGET_BOTTOM: AtomicU32 = AtomicU32::new(0);
    static IME_ANIMATING: AtomicBool = AtomicBool::new(false);
    static FONT_SCALE: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());
    static DISPLAY_DENSITY: AtomicU32 = AtomicU32::new(0);

    /// Receives 120 Hz IME inset updates from `WindowInsetsAnimation.Callback` via lock-free
    /// atomics and wakes only the next hardware vsync (`frame_source::schedule_frame()`),
    /// avoiding `request_config_refresh()` and `drop_layers_on_resize` storms (MS-449).
    #[unsafe(no_mangle)]
    pub extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeInsets(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        ime_bottom_px: i32,
        target_bottom_px: i32,
        animating: u8,
        density: f32,
    ) {
        let scale = if density.is_finite() && density > 0.0 { density } else { 1.0 };
        let bottom_bits = (ime_bottom_px.max(0) as f32 / scale).to_bits();
        let target_bits = (target_bottom_px.max(0) as f32 / scale).to_bits();
        let is_animating = animating != 0;
        let changed = IME_BOTTOM.swap(bottom_bits, Ordering::Release) != bottom_bits
            || IME_TARGET_BOTTOM.swap(target_bits, Ordering::Release) != target_bits
            || IME_ANIMATING.swap(is_animating, Ordering::AcqRel) != is_animating;
        if !changed {
            return;
        }
        jni::schedule_ime_frame();
    }

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
        let t = (top.max(0) as f32 / scale).to_bits();
        let b = (bottom.max(0) as f32 / scale).to_bits();
        let l = (left.max(0) as f32 / scale).to_bits();
        let r = (right.max(0) as f32 / scale).to_bits();
        let changed = INSET_TOP.swap(t, Ordering::Relaxed) != t
            || INSET_BOTTOM.swap(b, Ordering::Relaxed) != b
            || INSET_LEFT.swap(l, Ordering::Relaxed) != l
            || INSET_RIGHT.swap(r, Ordering::Relaxed) != r;
        if !changed {
            return;
        }
        log::info!(
            "mailstream-android: safe area insets logical top={:.1} bottom={:.1} left={:.1} right={:.1}",
            f32::from_bits(t),
            f32::from_bits(b),
            f32::from_bits(l),
            f32::from_bits(r),
        );
        // Wake the render thread safely from the Java UI thread without touching GPUI directly.
        jni::request_config_refresh();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeDisplayMetrics(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        density: f32,
        font_scale: f32,
    ) {
        let scale = if font_scale.is_finite() && font_scale > 0.0 {
            font_scale.clamp(0.75, 2.0)
        } else {
            1.0
        };
        let density_bits = if density.is_finite() && density > 0.0 {
            density.to_bits()
        } else {
            0
        };
        let font_bits = scale.to_bits();
        let changed = DISPLAY_DENSITY.swap(density_bits, Ordering::Relaxed) != density_bits
            || FONT_SCALE.swap(font_bits, Ordering::Relaxed) != font_bits;
        if !changed {
            return;
        }
        log::info!(
            "mailstream-android: display metrics density={:.3} font_scale={:.3}",
            density,
            scale,
        );
        jni::request_config_refresh();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeTrimMemory(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        level: i32,
    ) {
        jni::request_trim_memory(level);
    }

    fn android_font_scale() -> f32 {
        let scale = f32::from_bits(FONT_SCALE.load(Ordering::Relaxed));
        if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }
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

    fn android_ime_insets() -> mailstream_gpui::design::compact::ImeInsets {
        let bottom = f32::from_bits(IME_BOTTOM.load(Ordering::Acquire));
        let target_bottom = f32::from_bits(IME_TARGET_BOTTOM.load(Ordering::Acquire));
        let animating = IME_ANIMATING.load(Ordering::Acquire);
        mailstream_gpui::design::compact::ImeInsets {
            bottom: if bottom.is_finite() && bottom >= 0.0 { bottom } else { 0.0 },
            target_bottom: if target_bottom.is_finite() && target_bottom >= 0.0 {
                target_bottom
            } else {
                0.0
            },
            animating,
        }
    }

    /// Safely extracts a Java String from a raw JNI call frame (works in both Activity and
    /// cold-started headless `FirebaseMessagingService` / `BroadcastReceiver` processes).
    unsafe fn jstring_to_rust(env: *mut ::jni::sys::JNIEnv, s: ::jni::sys::jstring) -> String {
        if env.is_null() || s.is_null() {
            return String::new();
        }
        let Some(fns) = (unsafe { (*env).as_ref() }) else {
            return String::new();
        };
        let chars = unsafe { (fns.v1_1.GetStringUTFChars)(env, s, std::ptr::null_mut()) };
        if chars.is_null() {
            return String::new();
        }
        let out = unsafe { std::ffi::CStr::from_ptr(chars) }
            .to_string_lossy()
            .into_owned();
        unsafe { (fns.v1_1.ReleaseStringUTFChars)(env, s, chars) };
        out
    }

    /// Allocates a new Java String in the caller's JNI frame.
    unsafe fn rust_to_jstring(env: *mut ::jni::sys::JNIEnv, text: &str) -> ::jni::sys::jstring {
        if env.is_null() {
            return std::ptr::null_mut();
        }
        let Some(fns) = (unsafe { (*env).as_ref() }) else {
            return std::ptr::null_mut();
        };
        let sanitized = text.replace('\0', "");
        let Ok(cstr) = std::ffi::CString::new(sanitized) else {
            return std::ptr::null_mut();
        };
        unsafe { (fns.v1_1.NewStringUTF)(env, cstr.as_ptr()) }
    }

    fn configure_android_engine(support_dir: PathBuf, cache_dir: PathBuf) {
        let mut config = Config::for_this_app().in_directory(support_dir);
        config.cache_directory = cache_dir;
        config.control_socket = false;
        config.oauth.open_url = std::sync::Arc::new(|url| {
            if let Err(err) = gpui_mobile::packages::url_launcher::launch_url(url) {
                log::warn!("mailstream-android: open_url failed: {err}");
            }
        });
        let _ = mailstream::configure_engine(config);
    }

    fn init_android_logger_once() {
        let filter = android_logger::FilterBuilder::new()
            .filter_level(log::LevelFilter::Warn)
            .filter_module("mailstream_android_poc", log::LevelFilter::Info)
            .filter_module("mailstream_gpui", log::LevelFilter::Info)
            .filter_module("mailstream_android_fonts", log::LevelFilter::Info)
            .filter_module("mailstream", log::LevelFilter::Info)
            .filter_module("mailstream_engine", log::LevelFilter::Info)
            .filter_module("gpui_mobile", log::LevelFilter::Info)
            .filter_module("gpui_wgpu", log::LevelFilter::Info)
            .build();
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_filter(filter)
                .with_tag("mailstream"),
        );
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeNotificationResponse(
        env: *mut ::jni::sys::JNIEnv,
        _class: ::jni::sys::jclass,
        tag: ::jni::sys::jstring,
        action_id: ::jni::sys::jstring,
    ) {
        let tag = unsafe { jstring_to_rust(env, tag) };
        let action_id = unsafe { jstring_to_rust(env, action_id) };
        if tag.is_empty() {
            return;
        }
        if let Some(platform) = jni::shared_platform() {
            platform.0.deliver_system_notification_response(
                tag,
                (!action_id.is_empty()).then_some(action_id),
            );
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_MailstreamFcm_nativeOnFcmToken(
        env: *mut ::jni::sys::JNIEnv,
        _class: ::jni::sys::jclass,
        support_dir: ::jni::sys::jstring,
        cache_dir: ::jni::sys::jstring,
        device_id: ::jni::sys::jstring,
        token: ::jni::sys::jstring,
    ) {
        init_android_logger_once();
        let support = unsafe { jstring_to_rust(env, support_dir) };
        let cache = unsafe { jstring_to_rust(env, cache_dir) };
        let device_id = unsafe { jstring_to_rust(env, device_id) };
        let token = unsafe { jstring_to_rust(env, token) };
        if support.is_empty() || device_id.is_empty() || token.is_empty() {
            return;
        }
        let support_path = PathBuf::from(support);
        let cache_path = if cache.is_empty() { support_path.join("cache") } else { PathBuf::from(cache) };
        configure_android_engine(support_path, cache_path);
        let engine = mailstream::Mailstream::new();
        log::info!("mailstream-android: registering FCM token for device={device_id}");
        engine.set_fcm_device(device_id, token);
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_dev_gpui_mobile_MailstreamFcm_nativeNetworkChanged(
        _env: *mut ::jni::sys::JNIEnv,
        _class: ::jni::sys::jclass,
    ) {
        let engine = mailstream::Mailstream::new();
        engine.notify_network_change();
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_MailstreamMessagingService_nativeOnPushWake(
        env: *mut ::jni::sys::JNIEnv,
        _class: ::jni::sys::jclass,
        support_dir: ::jni::sys::jstring,
        cache_dir: ::jni::sys::jstring,
        source: ::jni::sys::jstring,
        account: ::jni::sys::jstring,
        subscription_id: ::jni::sys::jstring,
    ) -> ::jni::sys::jstring {
        init_android_logger_once();
        let support = unsafe { jstring_to_rust(env, support_dir) };
        let cache = unsafe { jstring_to_rust(env, cache_dir) };
        let source = unsafe { jstring_to_rust(env, source) };
        let account = unsafe { jstring_to_rust(env, account) };
        let sub_id = unsafe { jstring_to_rust(env, subscription_id) };
        if support.is_empty() {
            return unsafe { rust_to_jstring(env, "[]") };
        }
        let support_path = PathBuf::from(support);
        let cache_path = if cache.is_empty() { support_path.join("cache") } else { PathBuf::from(cache) };
        configure_android_engine(support_path, cache_path);
        let engine = mailstream::Mailstream::new();
        let items = engine.background_push_wake_blocking(
            if source.is_empty() { "mail".into() } else { source },
            (!account.is_empty()).then_some(account),
            (!sub_id.is_empty()).then_some(sub_id),
            std::time::Duration::from_secs(15),
        );
        let json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".into());
        unsafe { rust_to_jstring(env, &json) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiNotificationActionReceiver_nativeNotificationAction(
        env: *mut ::jni::sys::JNIEnv,
        _class: ::jni::sys::jclass,
        support_dir: ::jni::sys::jstring,
        cache_dir: ::jni::sys::jstring,
        tag: ::jni::sys::jstring,
        action_id: ::jni::sys::jstring,
    ) {
        init_android_logger_once();
        let support = unsafe { jstring_to_rust(env, support_dir) };
        let cache = unsafe { jstring_to_rust(env, cache_dir) };
        let tag = unsafe { jstring_to_rust(env, tag) };
        let action_id = unsafe { jstring_to_rust(env, action_id) };
        if support.is_empty() || tag.is_empty() || action_id.is_empty() {
            return;
        }
        let support_path = PathBuf::from(support);
        let cache_path = if cache.is_empty() { support_path.join("cache") } else { PathBuf::from(cache) };
        configure_android_engine(support_path, cache_path);
        let engine = mailstream::Mailstream::new();
        if let Err(err) = engine.run_notification_action_blocking(&tag, &action_id) {
            log::warn!("mailstream-android: notification action {action_id} failed for {tag}: {err}");
        }
    }
    fn android_long_press_haptic() {
        let _ = gpui_mobile::packages::vibration::haptic_feedback(
            gpui_mobile::packages::vibration::HapticFeedback::Heavy,
        );
    }

    fn launch_mailstream_ui(cx: &mut App) {
        mailstream_gpui::design::compact::set_safe_area_provider(android_safe_area_insets);
        mailstream_gpui::design::compact::set_ime_insets_provider(android_ime_insets);
        mailstream_gpui::design::compact::set_back_enabled_provider(jni::set_back_enabled_android);
        mailstream_gpui::design::compact::set_font_scale_provider(android_font_scale);
        mailstream_gpui::design::compact::set_long_press_haptic_provider(android_long_press_haptic);
        mailstream_gpui::init(cx);
        mailstream::Mailstream::new().on_fcm_config(|fcm| {
            if let Err(err) = jni::init_fcm_options_via_jni(
                &fcm.app_id,
                &fcm.api_key,
                &fcm.project_id,
                &fcm.sender_id,
            ) {
                log::warn!("mailstream-android: init_fcm_options_via_jni failed: {err}");
            }
        });
        let (window, app) = mailstream_gpui::open_window(mailstream_gpui::mobile_window_options(), cx);
        let async_cx_ime = cx.to_async();
        let weak_app_ime = app.downgrade();
        text_input::set_ime_insets_callback(Some(Box::new(move || {
            let mut async_cx = async_cx_ime.clone();
            let _ = weak_app_ime.update(&mut async_cx, |_, cx| cx.notify());
        })));
        let async_cx_back = cx.to_async();
        let weak_app_back = app.downgrade();
        text_input::set_predictive_back_callback(Some(Box::new(move |event| {
            let mapped = match event {
                text_input::PredictiveBackEvent::Started { progress, edge } => {
                    mailstream_gpui::swipe::PredictiveBackEvent::Started {
                        progress,
                        edge: mailstream_gpui::swipe::PredictiveEdge::from_android(edge),
                    }
                }
                text_input::PredictiveBackEvent::Progressed { progress, edge } => {
                    mailstream_gpui::swipe::PredictiveBackEvent::Progressed {
                        progress,
                        edge: mailstream_gpui::swipe::PredictiveEdge::from_android(edge),
                    }
                }
                text_input::PredictiveBackEvent::Invoked => {
                    mailstream_gpui::swipe::PredictiveBackEvent::Invoked
                }
                text_input::PredictiveBackEvent::Cancelled => {
                    mailstream_gpui::swipe::PredictiveBackEvent::Cancelled
                }
            };
            let mut async_cx = async_cx_back.clone();
            let _ = async_cx.update_window(window.into(), |_, window, cx| {
                let _ = weak_app_back.update(cx, |app, cx| {
                    app.handle_predictive_back(mapped, window, cx);
                });
            });
        })));
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
        init_android_logger_once();
        jni::install_panic_hook();
        log::info!("mailstream-android: android_main entered");

        let _platform = jni::init_platform(&app);
        let Some(shared) = jni::shared_platform() else {
            log::error!("mailstream-android: shared_platform() returned None");
            return;
        };

        let support_dir = app
            .internal_data_path()
            .unwrap_or_else(|| PathBuf::from("/data/local/tmp/mailstream-poc"));
        let cache_dir = jni::query_cache_dir_via_jni()
            .unwrap_or_else(|| support_dir.join("cache"));
        jni::set_pipeline_cache_dir(cache_dir.join("wgpu-pipeline-cache"));
        configure_android_engine(support_dir, cache_dir);

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
        let cache_dir = support_dir.join("cache");
        jni::set_pipeline_cache_dir(cache_dir.join("wgpu-pipeline-cache"));
        configure_android_engine(support_dir, cache_dir);
        host::start_with_assets(mailstream_gpui::Assets, |cx| {
            launch_mailstream_ui(cx);
        });
    }
}
