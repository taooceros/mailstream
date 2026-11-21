//! Android entry point and event loop using the `android-activity` crate.
//!
//! This module replaces the previous hand-rolled `ANativeActivity_onCreate`,
//! `JNI_OnLoad`, and lifecycle callback implementations with the higher-level
//! `android-activity` glue layer.
//!
//! ## Entry sequence
//!
//! ```text
//! android-activity loads the .so and calls android_main(app: AndroidApp)
//!   └── We store the AndroidApp globally
//!       └── Call the user-supplied gpui_android_main(app)
//! ```
//!
//! ## Threading model
//!
//! `android-activity` spawns a dedicated native thread and calls `android_main`
//! on it.  All GPUI draw / event callbacks run on this thread.  The
//! `AndroidApp` handle is `Send + Sync` and can be shared across threads.
//!
//! ## User entry point
//!
//! Applications must define:
//!
//! ```rust,no_run
//! #[no_mangle]
//! fn android_main(app: android_activity::AndroidApp) {
//!     // Initialise GPUI and run the application.
//! }
//! ```
//!
//! ## Event handling
//!
//! Lifecycle events (window creation/destruction, focus changes, etc.) are
//! delivered via `AndroidApp::poll_events()`.  Input events are obtained via
//! `AndroidApp::input_events_iter()`.
//!
//! ## Frames
//!
//! The loop blocks on the looper between events and draws only when GPUI
//! asked for a frame and a vsync has passed since — see
//! [`super::frame_source`].

#![allow(unsafe_code)]
#![allow(non_snake_case)]

use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

/// Whether the deferred init-window callback has already been invoked.
///
/// Reset to `false` on `TerminateWindow` so that when the surface is
/// recreated on resume the init callbacks run again.
static INIT_WINDOW_DONE: AtomicBool = AtomicBool::new(false);

/// Whether the GPUI native library has completed initialization.
///
/// Set to `true` after the first frame is rendered. This can be queried
/// via JNI by a custom Activity to dismiss the splash screen, although
/// with NativeActivity the system splash handles this automatically.
pub static NATIVE_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Deferred lifecycle flags.
///
/// We must NOT call `win.set_active()` or `platform.did_enter_background()`
/// inside `handle_main_event` (which runs within `poll_events`).
/// The android-activity crate's Java-side callbacks block on a condvar
/// waiting for the native thread to finish processing the command.
/// If our handler tries to acquire the window `state` lock (a
/// `parking_lot::Mutex`), and a background thread is holding it (e.g.
/// during a render pass), we deadlock: native waits on the lock,
/// the lock holder waits for native rendering to complete, but native
/// is stuck.
///
/// Instead, the handlers set these flags and the main loop body
/// processes them AFTER `poll_events` returns.
static PAUSE_PENDING: AtomicBool = AtomicBool::new(false);
static RESUME_PENDING: AtomicBool = AtomicBool::new(false);
static TERM_WINDOW_PENDING: AtomicBool = AtomicBool::new(false);
static INIT_WINDOW_PENDING: AtomicBool = AtomicBool::new(false);
static WINDOW_RESIZED_PENDING: AtomicBool = AtomicBool::new(false);
static CONFIG_CHANGED_PENDING: AtomicBool = AtomicBool::new(false);
static TRIM_MEMORY_PENDING: AtomicI32 = AtomicI32::new(0);

use android_activity::{AndroidApp, MainEvent, PollEvent};

use super::platform::{AndroidPlatform, SharedPlatform};

use jni::objects::{JObject, JString, JValue};
use jni::JavaVM;
use std::sync::{atomic::AtomicPtr, Mutex};

// ── JNI helpers (safe `jni` crate wrappers) ──────────────────────────────────

static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();

/// Get or create the static `JavaVM` wrapper.
fn java_vm_safe() -> Result<&'static JavaVM, String> {
    if let Some(vm) = JAVA_VM.get() {
        return Ok(vm);
    }
    let ptr = java_vm();
    if ptr.is_null() {
        return Err("JavaVM not available".into());
    }
    Ok(JAVA_VM.get_or_init(|| unsafe { JavaVM::from_raw(ptr as *mut jni::sys::JavaVM) }))
}

/// Run a closure with an attached `jni::Env` for the current thread.
///
/// In jni 0.22 the `attach_current_thread` API is closure-based.
/// The thread is auto-detached when the closure returns (if it was
/// not already attached).
pub fn with_env<T>(f: impl FnOnce(&mut jni::Env) -> Result<T, String>) -> Result<T, String> {
    let vm = java_vm_safe()?;
    let mut result: Option<Result<T, String>> = None;
    vm.attach_current_thread(|env: &mut jni::Env| -> Result<(), jni::errors::Error> {
        result = Some(f(env));
        Ok(())
    })
    .map_err(|e: jni::errors::Error| e.to_string())?;
    result.unwrap()
}

/// Convenience alias: kept so existing callers that import `obtain_env`
/// compile with minimal changes. Returns a result by running the given
/// closure inside `with_env`.
#[inline]
pub fn obtain_env<T>(f: impl FnOnce(&mut jni::Env) -> Result<T, String>) -> Result<T, String> {
    with_env(f)
}

/// Get the Activity as a [`JObject`].
///
/// `activity_as_ptr()` returns a JNI global reference from `android-activity`
/// that is valid for the lifetime of the app. We wrap it in a `JObject`.
///
/// Requires `&Env` because jni 0.22's `JObject::from_raw` binds the
/// local-reference-frame lifetime.
pub fn activity<'local>(env: &jni::Env<'local>) -> Result<JObject<'local>, String> {
    let ptr = activity_as_ptr();
    if ptr.is_null() {
        return Err("Activity not available".into());
    }
    Ok(unsafe { JObject::from_raw(env, ptr as jni::sys::jobject) })
}

/// Convert a Java String (`JObject` wrapping a `java.lang.String`) to a Rust `String`.
///
/// Returns an empty string on null or error.
pub fn get_string(env: &mut jni::Env<'_>, obj: &JObject<'_>) -> String {
    if obj.is_null() {
        return String::new();
    }
    let jstr = unsafe { JString::from_raw(env, obj.as_raw()) };
    jstr.to_string()
}

/// Extension trait for converting `jni::errors::Result<T>` to `Result<T, String>`.
pub(crate) trait JniExt<T> {
    fn e(self) -> Result<T, String>;
}

impl<T> JniExt<T> for jni::errors::Result<T> {
    fn e(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
    }
}

/// Find an application class by name using the Activity's classloader.
///
/// From native threads, `JNIEnv::FindClass` uses the system classloader
/// which doesn't know about application classes.  This helper uses the
/// Activity's classloader via `activity.getClass().getClassLoader().loadClass(name)`.
///
/// `class_name` uses Java dot notation (e.g. `"dev.gpui.mobile.GpuiHelper"`).
pub fn find_app_class<'local>(
    env: &mut jni::Env<'local>,
    class_name: &str,
) -> Result<jni::objects::JClass<'local>, String> {
    let act = activity(env)?;

    // activity.getClassLoader() — call on the Context instance directly.
    // Do NOT use activity.getClass().getClassLoader(): NativeActivity is a
    // framework class loaded by BootClassLoader, which cannot see app classes.
    let class_loader = env
        .call_method(
            &act,
            jni::jni_str!("getClassLoader"),
            jni::jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| {
            env.exception_clear();
            let msg = format!("getClassLoader failed: {e}");
            log::error!("find_app_class({class_name}): {msg}");
            msg
        })?;

    // classLoader.loadClass("dev.gpui.mobile.GpuiHelper")
    let jname = env.new_string(class_name).e()?;
    let loaded = env
        .call_method(
            &class_loader,
            jni::jni_str!("loadClass"),
            jni::jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(&jname)],
        )
        .and_then(|v| v.l())
        .map_err(|e| {
            // Print full Java stack trace to logcat, then clear.
            env.exception_describe();
            env.exception_clear();
            let msg = format!("loadClass({class_name}) failed: {e}");
            log::error!("{msg}");
            msg
        })?;

    log::debug!("find_app_class: loaded {class_name}");
    Ok(unsafe { jni::objects::JClass::from_raw(env, loaded.as_raw()) })
}

// ── global state ─────────────────────────────────────────────────────────────

/// The `AndroidApp` handle from `android-activity`.
///
/// Set once in `android_main`; read-only thereafter.
static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// Process-global `AndroidPlatform` instance.
///
/// Initialised once during `android_main`; read-only thereafter.
static PLATFORM: OnceLock<Arc<AndroidPlatform>> = OnceLock::new();

/// Get the unicode character produced by an Android key event via JNI.
///
/// This creates a `android.view.KeyEvent` Java object and calls
/// `getUnicodeChar(metaState)` on it.  Returns 0 on failure.
pub fn unicode_char_for_key_event(key_code: i32, action: i32, meta_state: i32) -> u32 {
    with_env(|env| {
        let key_event = match env.new_object(
            jni::jni_str!("android/view/KeyEvent"),
            jni::jni_sig!("(II)V"),
            &[JValue::Int(action), JValue::Int(key_code)],
        ) {
            Ok(o) => o,
            Err(_) => {
                env.exception_clear();
                return Ok(0);
            }
        };
        match env.call_method(
            &key_event,
            jni::jni_str!("getUnicodeChar"),
            jni::jni_sig!("(I)I"),
            &[JValue::Int(meta_state)],
        ) {
            Ok(v) => {
                let c = v.i().unwrap_or(0);
                Ok(if c > 0 { c as u32 } else { 0 })
            }
            Err(_) => {
                env.exception_clear();
                Ok(0)
            }
        }
    })
    .unwrap_or(0)
}

// ── public accessors ──────────────────────────────────────────────────────────

/// JVM for the **host-driven** entry point ([`super::host`]), which has no
/// `AndroidApp` to read it from.
static HOST_VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Current Activity for the host-driven entry point, as a JNI global reference we
/// own. Replaced on every Activity creation so [`activity_as_ptr`] always names the
/// *live* Activity.
///
/// The outgoing reference is kept one generation longer: `activity_as_ptr` hands out a
/// raw `jobject`, and a call on the render thread may still be using the previous
/// Activity while the UI thread installs the next one.
static HOST_ACTIVITY: Mutex<HostActivity> = Mutex::new(HostActivity {
    current: None,
    previous: None,
});

struct HostActivity {
    current: Option<jni::refs::Global<JObject<'static>>>,
    previous: Option<jni::refs::Global<JObject<'static>>>,
}

/// Register the current Activity when running without `android-activity`.
///
/// Call from a JNI entry point in `Activity.onCreate`, every time — a recreated
/// Activity is a new object. This module takes its own global reference and records
/// the JVM, so `java_vm()` / `activity_as_ptr()` and everything built on them (IME,
/// safe areas, file pickers, `rustls-platform-verifier`) work exactly as on the
/// `android-activity` path. A no-op when `android-activity` owns the process.
pub fn set_host_activity(env: &mut jni::Env<'_>, activity: &JObject<'_>) -> Result<(), String> {
    if ANDROID_APP.get().is_some() {
        return Ok(());
    }
    let global = env.new_global_ref(activity).e()?;
    let vm = env.get_java_vm().e()?;
    HOST_VM.store(
        vm.get_raw() as *mut c_void,
        std::sync::atomic::Ordering::SeqCst,
    );
    let mut slot = HOST_ACTIVITY.lock().expect("poisoned");
    slot.previous = slot.current.take();
    slot.current = Some(global);
    Ok(())
}

/// Public accessor for the JavaVM pointer.
///
/// Uses `AndroidApp::vm_as_ptr()` from the stored `AndroidApp`.
/// Used by `platform.rs` for JNI calls.
pub fn java_vm() -> *mut c_void {
    ANDROID_APP
        .get()
        .map(|app| app.vm_as_ptr())
        .unwrap_or_else(|| HOST_VM.load(std::sync::atomic::Ordering::SeqCst))
}

/// Public accessor for the current Activity's JNI object reference.
///
/// Uses `AndroidApp::activity_as_ptr()` from the stored `AndroidApp`.
/// Used by `platform.rs` and `window.rs` for JNI calls that require the
/// activity's jobject.
///
/// NOTE: This returns a jobject (JNI global ref), NOT an `ANativeActivity *`.
/// Code that previously used `(*activity).clazz` should use this directly
/// as the activity jobject.
pub fn activity_as_ptr() -> *mut c_void {
    ANDROID_APP
        .get()
        .map(|app| app.activity_as_ptr())
        .unwrap_or_else(|| {
            HOST_ACTIVITY
                .lock()
                .expect("poisoned")
                .current
                .as_ref()
                .map(|activity| activity.as_raw() as *mut c_void)
                .unwrap_or(std::ptr::null_mut())
        })
}

/// Returns a clone of the stored `AndroidApp`, if initialised.
pub fn android_app() -> Option<AndroidApp> {
    ANDROID_APP.get().cloned()
}

/// Install the platform built by the host-driven path (`super::host`).
///
/// The `android-activity` path calls `init_platform` instead; both end up in the same
/// slot so every accessor in this module keeps working regardless of entry point.
pub(crate) fn set_host_platform(platform: Arc<AndroidPlatform>) {
    if PLATFORM.set(platform).is_err() {
        log::warn!("set_host_platform: PLATFORM already set — both entry points in one process?");
    }
}

/// Returns a reference to the global `AndroidPlatform`, if initialised.
///
/// Returns `None` before `android_main` has set it up.
pub fn platform() -> Option<&'static Arc<AndroidPlatform>> {
    PLATFORM.get()
}

/// Returns a [`SharedPlatform`] wrapping the global `Arc<AndroidPlatform>`.
///
/// This is the value you hand to `Application::with_platform(...)`:
///
/// ```rust,no_run
/// let platform = jni::shared_platform().unwrap();
/// Application::with_platform(platform.into_rc()).run(|cx| { … });
/// ```
///
/// Returns `None` before `init_platform` has been called.
pub fn shared_platform() -> Option<SharedPlatform> {
    PLATFORM
        .get()
        .map(|arc| SharedPlatform::new(Arc::clone(arc)))
}

// ── input event types ─────────────────────────────────────────────────────────

/// Motion event action constants from the NDK.
const AMOTION_EVENT_ACTION_DOWN: u32 = 0;
const AMOTION_EVENT_ACTION_UP: u32 = 1;
const AMOTION_EVENT_ACTION_MOVE: u32 = 2;
const AMOTION_EVENT_ACTION_CANCEL: u32 = 3;

// ── night mode query via NDK Configuration ───────────────────────────────────

/// Query the current night mode using the NDK Configuration API.
///
/// Returns `true` if the system is in dark mode.
pub fn query_night_mode_via_jni() -> bool {
    let app = match android_app() {
        Some(app) => app,
        None => return false,
    };

    // Build an ndk::configuration::Configuration from the app's asset manager.
    let config = ndk::configuration::Configuration::from_asset_manager(&app.asset_manager());
    let is_dark = config.ui_mode_night() == ndk::configuration::UiModeNight::Yes;

    log::debug!("query_night_mode (ndk): is_dark={}", is_dark);
    is_dark
}

/// Query the active display refresh rate in Hertz via JNI (`Display.getRefreshRate()`).
pub(crate) fn query_refresh_rate_via_jni() -> Option<f32> {
    with_env(|env| {
        let act = activity(env)?;
        let api_level = unsafe { ndk_sys::android_get_device_api_level() };
        let mut display = if api_level >= 30 {
            match env
                .call_method(
                    &act,
                    jni::jni_str!("getDisplay"),
                    jni::jni_sig!("()Landroid/view/Display;"),
                    &[],
                )
                .and_then(|v| v.l())
            {
                Ok(d) => Some(d),
                Err(_) => {
                    env.exception_clear();
                    None
                }
            }
        } else {
            None
        };
        if display.as_ref().is_none_or(|d| d.is_null()) {
            if let Ok(wm) = env
                .call_method(
                    &act,
                    jni::jni_str!("getWindowManager"),
                    jni::jni_sig!("()Landroid/view/WindowManager;"),
                    &[],
                )
                .and_then(|v| v.l())
            {
                if !wm.is_null() {
                    match env
                        .call_method(
                            &wm,
                            jni::jni_str!("getDefaultDisplay"),
                            jni::jni_sig!("()Landroid/view/Display;"),
                            &[],
                        )
                        .and_then(|v| v.l())
                    {
                        Ok(d) if !d.is_null() => display = Some(d),
                        _ => env.exception_clear(),
                    }
                }
            } else {
                env.exception_clear();
            }
        }
        let Some(display) = display.filter(|d| !d.is_null()) else {
            return Ok(None);
        };
        let hz = match env
            .call_method(
                &display,
                jni::jni_str!("getRefreshRate"),
                jni::jni_sig!("()F"),
                &[],
            )
            .and_then(|v| v.f())
        {
            Ok(hz) if hz.is_finite() && (10.0..=480.0).contains(&hz) => {
                log::info!(
                    "AndroidDisplay refresh rate: {:.2} Hz ({:.2} ms)",
                    hz,
                    1000.0 / hz
                );
                Some(hz)
            }
            Ok(_) => None,
            Err(_) => {
                env.exception_clear();
                None
            }
        };
        Ok(hz)
    })
    .ok()
    .flatten()
}

/// Query exact `DisplayMetrics.density` and `Configuration.fontScale` via JNI.
///
/// Prefer this over `AConfiguration_getDensity()`, which can report bucketed or
/// sentinel values (`ANY`/`NONE`) and does not expose `fontScale`.
pub(crate) fn query_display_metrics_via_jni() -> Option<(f32, f32)> {
    with_env(|env| {
        let act = activity(env)?;
        let resources = match env
            .call_method(
                &act,
                jni::jni_str!("getResources"),
                jni::jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )
            .and_then(|v| v.l())
        {
            Ok(r) if !r.is_null() => r,
            _ => {
                env.exception_clear();
                return Ok(None);
            }
        };
        let metrics = match env
            .call_method(
                &resources,
                jni::jni_str!("getDisplayMetrics"),
                jni::jni_sig!("()Landroid/util/DisplayMetrics;"),
                &[],
            )
            .and_then(|v| v.l())
        {
            Ok(m) if !m.is_null() => m,
            _ => {
                env.exception_clear();
                return Ok(None);
            }
        };
        let density = match env
            .get_field(&metrics, jni::jni_str!("density"), jni::jni_sig!("F"))
            .and_then(|v| v.f())
        {
            Ok(d) if d.is_finite() && d > 0.0 => d,
            _ => {
                env.exception_clear();
                return Ok(None);
            }
        };
        let font_scale = match env
            .call_method(
                &resources,
                jni::jni_str!("getConfiguration"),
                jni::jni_sig!("()Landroid/content/res/Configuration;"),
                &[],
            )
            .and_then(|v| v.l())
        {
            Ok(cfg) if !cfg.is_null() => match env
                .get_field(&cfg, jni::jni_str!("fontScale"), jni::jni_sig!("F"))
                .and_then(|v| v.f())
            {
                Ok(s) if s.is_finite() && s > 0.0 => s,
                _ => {
                    env.exception_clear();
                    1.0
                }
            },
            _ => {
                env.exception_clear();
                1.0
            }
        };
        log::info!(
            "AndroidDisplay metrics: density={:.3} fontScale={:.3}",
            density,
            font_scale,
        );
        Ok(Some((density, font_scale)))
    })
    .ok()
    .flatten()
}

/// Request a deferred configuration refresh on the native render thread (safe to
/// call from the Android Java UI thread, e.g. `onConfigurationChanged`).
pub fn request_config_refresh() {
    CONFIG_CHANGED_PENDING.store(true, Ordering::Relaxed);
    super::frame_source::wake();
}

/// Request a deferred GPU/cache memory trim on the native render thread (safe to call from
/// `Activity.onTrimMemory(int)` on the Java UI thread or `MainEvent::LowMemory`).
pub fn request_trim_memory(level: i32) {
    if level <= 0 {
        return;
    }
    TRIM_MEMORY_PENDING.fetch_max(level, Ordering::AcqRel);
    super::frame_source::wake();
}

/// Configures the directory where `gpui-fast-wgpu` persists its Vulkan `PipelineCache`.
pub fn set_pipeline_cache_dir(dir: std::path::PathBuf) {
    gpui_wgpu::set_pipeline_cache_dir(dir);
}

/// Query Android's app cache directory (`Context.getCacheDir().getAbsolutePath()`) via JNI.
pub fn query_cache_dir_via_jni() -> Option<std::path::PathBuf> {
    with_env(|env| {
        let act = activity(env)?;
        let cache_file = match env
            .call_method(
                &act,
                jni::jni_str!("getCacheDir"),
                jni::jni_sig!("()Ljava/io/File;"),
                &[],
            )
            .and_then(|v| v.l())
        {
            Ok(f) if !f.is_null() => f,
            _ => {
                env.exception_clear();
                return Ok(None);
            }
        };
        let path_obj = match env
            .call_method(
                &cache_file,
                jni::jni_str!("getAbsolutePath"),
                jni::jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .and_then(|v| v.l())
        {
            Ok(s) if !s.is_null() => s,
            _ => {
                env.exception_clear();
                return Ok(None);
            }
        };
        let path = get_string(env, &path_obj);
        if path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(std::path::PathBuf::from(path)))
        }
    })
    .ok()
    .flatten()
}

// ── input event processing ────────────────────────────────────────────────────

/// Process input events from the `AndroidApp` and dispatch them to the window.
fn process_input_events(app: &AndroidApp, driver: &mut super::loop_driver::LoopDriver) {
    let platform = match PLATFORM.get() {
        Some(p) => p,
        None => {
            log::trace!("process_input_events: no platform yet");
            return;
        }
    };

    let win = match platform.primary_window() {
        Some(w) => w,
        None => {
            log::trace!("process_input_events: no primary window yet");
            return;
        }
    };

    match app.input_events_iter() {
        Ok(mut iter) => {
            loop {
                let read_input = iter.next(|event| {
                    use android_activity::input::{InputEvent, MotionAction};

                    match event {
                        InputEvent::MotionEvent(motion_event) => {
                            let action = motion_event.action();
                            let pointer_count = motion_event.pointer_count();

                            log::debug!(
                                "process_input_events: MotionEvent action={:?} pointers={}",
                                action,
                                pointer_count,
                            );

                            // Check if this touch lands on a platform view.
                            //
                            // On Android with NativeActivity, ALL touch events go
                            // to the native surface first. Platform views are real
                            // Java Views in a FrameLayout overlay, but they won't
                            // receive touches unless we skip GPUI dispatch and let
                            // Android's view hierarchy handle the event instead.
                            //
                            // We use the primary pointer for the hit-test on DOWN
                            // actions. Physical pixel coordinates are converted to
                            // logical pixels to match PlatformViewBounds.
                            let hits_platform_view = {
                                let registry = crate::platform_view::PlatformViewRegistry::global();
                                if registry.active_view_count() > 0 {
                                    let primary = motion_event.pointer_at_index(
                                        match action {
                                            MotionAction::PointerDown | MotionAction::PointerUp => {
                                                motion_event.pointer_index()
                                            }
                                            _ => 0,
                                        }
                                    );
                                    let scale = win.scale_factor();
                                    let logical_x = primary.x() / scale;
                                    let logical_y = primary.y() / scale;
                                    registry.hit_test(logical_x, logical_y)
                                } else {
                                    false
                                }
                            };

                            if hits_platform_view {
                                log::debug!(
                                    "process_input_events: touch hits platform view, skipping GPUI dispatch",
                                );
                                // Return Unhandled so android-activity can pass
                                // the event back to the Java view hierarchy where
                                // the platform view's FrameLayout lives.
                                return android_activity::InputStatus::Unhandled;
                            }

                            let event_time_ns = motion_event.event_time();

                            if matches!(action, MotionAction::Down) {
                                // `ACTION_DOWN` always starts a new gesture with no other active pointers.
                                driver.on_touch_stream_reset();
                            }
                            for i in 0..pointer_count {
                                let pointer = motion_event.pointer_at_index(i);

                                let touch_action = match action {
                                    MotionAction::Down => AMOTION_EVENT_ACTION_DOWN,
                                    MotionAction::PointerDown => {
                                        // For pointer down, only dispatch the specific pointer
                                        if i != motion_event.pointer_index() {
                                            continue;
                                        }
                                        AMOTION_EVENT_ACTION_DOWN
                                    }
                                    MotionAction::Up => AMOTION_EVENT_ACTION_UP,
                                    MotionAction::PointerUp => {
                                        // For pointer up, only dispatch the specific pointer
                                        if i != motion_event.pointer_index() {
                                            continue;
                                        }
                                        AMOTION_EVENT_ACTION_UP
                                    }
                                    MotionAction::Move => AMOTION_EVENT_ACTION_MOVE,
                                    MotionAction::Cancel => AMOTION_EVENT_ACTION_CANCEL,
                                    _ => continue,
                                };

                                let pointer_id = pointer.pointer_id();
                                match touch_action {
                                    AMOTION_EVENT_ACTION_DOWN => {
                                        driver.on_touch_down(
                                            &win,
                                            pointer_id,
                                            event_time_ns,
                                            pointer.x(),
                                            pointer.y(),
                                        );
                                    }
                                    AMOTION_EVENT_ACTION_MOVE => {
                                        let mut any_sample = false;
                                        for hist in pointer.history() {
                                            any_sample |= driver.on_touch_move_sample(
                                                pointer_id,
                                                hist.event_time(),
                                                hist.x(),
                                                hist.y(),
                                            );
                                        }
                                        any_sample |= driver.on_touch_move_sample(
                                            pointer_id,
                                            event_time_ns,
                                            pointer.x(),
                                            pointer.y(),
                                        );
                                        if any_sample {
                                            driver.on_touch_move_accepted(&win);
                                        }
                                    }
                                    AMOTION_EVENT_ACTION_UP => {
                                        driver.on_touch_up(
                                            &win,
                                            pointer_id,
                                            event_time_ns,
                                            pointer.x(),
                                            pointer.y(),
                                        );
                                    }
                                    AMOTION_EVENT_ACTION_CANCEL => {
                                        driver.on_touch_cancel(
                                            &win,
                                            pointer_id,
                                            event_time_ns,
                                            pointer.x(),
                                            pointer.y(),
                                        );
                                    }
                                    _ => {}
                                }
                            }
                            android_activity::InputStatus::Handled
                        }
                        InputEvent::KeyEvent(key_event) => {
                            use android_activity::input::KeyAction;

                            let action = match key_event.action() {
                                KeyAction::Down => 0,
                                KeyAction::Up => 1,
                                _ => return android_activity::InputStatus::Unhandled,
                            };

                            let key_code: u32 = key_event.key_code().into();
                            let meta_state: u32 = key_event.meta_state().0;

                            let unicode_char = unicode_char_for_key_event(
                                key_code as i32,
                                action,
                                meta_state as i32,
                            );

                            if unicode_char != 0 {
                                log::trace!(
                                    "dispatch_key_event: code={} action={} meta={:#x} → unicode=U+{:04X}",
                                    key_code,
                                    action,
                                    meta_state,
                                    unicode_char
                                );
                            }

                            let key_event = crate::android::AndroidKeyEvent {
                                key_code: key_code as i32,
                                action,
                                meta_state: meta_state as i32,
                                unicode_char,
                            };

                            win.handle_key_event(key_event);
                            android_activity::InputStatus::Handled
                        }
                        _ => android_activity::InputStatus::Unhandled,
                    }
                });

                if !read_input {
                    break;
                }
            }
        }
        Err(err) => {
            log::error!("Failed to get input events iterator: {err:?}");
        }
    }
}

// ── main event loop ───────────────────────────────────────────────────────────

/// The event loop that processes `android-activity` events and drives the
/// platform.
///
/// Called from `android_main` after the platform is initialised.
/// Runs until the platform requests quit or the activity is destroyed.
pub fn run_event_loop(app: &AndroidApp) {
    log::info!("run_event_loop: entering main loop");

    // Track whether the on_init_window callback has already been invoked.
    // We do NOT invoke it inside handle_main_event (which runs inside
    // poll_events) because the callback can be heavy (shader compilation,
    // GPUI Application setup).  Running it there blocks the event loop and
    // prevents the system's FocusEvent from being consumed, triggering an
    // ANR after 10 seconds.
    //
    // Instead we check each loop iteration: if a primary window exists and
    // the callback is still pending, invoke it *after* poll_events has
    // returned so focus/input events have already been drained.
    INIT_WINDOW_DONE.store(false, Ordering::Relaxed);
    // Vsync pacing lives on this thread's looper.
    super::frame_source::install();
    let mut driver = super::loop_driver::LoopDriver::new();
    let mut iteration: u64 = 0;
    let mut app_is_active = false;

    loop {
        iteration += 1;

        // Check if quit was requested.
        if let Some(platform) = PLATFORM.get() {
            if platform.should_quit() {
                log::info!("run_event_loop: platform requested quit");
                dispose_all_platform_views();
                break;
            }
            platform.tick();
        }

        // ── Poll for events ──
        //
        // Blocks until something reaches the looper: a lifecycle command,
        // input, a main-thread task, or the vsync callback that
        // `frame_source` posted for a wanted frame. The timeout only covers
        // the delayed dispatcher tasks `platform.tick()` has to release and
        // the clock fallback without a choreographer.
        let active_window = PLATFORM.get().and_then(|platform| platform.primary_window());
        let timeout = driver.poll_timeout(
            active_window.as_deref(),
            PLATFORM
                .get()
                .and_then(|platform| platform.next_delayed_due()),
            INIT_WINDOW_DONE.load(Ordering::Relaxed) && app_is_active,
        );
        app.poll_events(timeout, |event| match event {
            PollEvent::Main(main_event) => {
                handle_main_event(app, main_event);
            }
            PollEvent::Wake => {}
            PollEvent::Timeout => {}
            _ => {}
        });

        // ── Deferred lifecycle processing ──
        //
        // Between each handler, call poll_events again to drain any
        // events the Java UI thread may have queued while we were
        // processing.  This keeps the condvar wait short and prevents
        // the InputDispatcher ANR (10s timeout on MotionEvent delivery).
        //
        // Helper closure to drain events quickly:
        let drain_events = |app: &AndroidApp| {
            app.poll_events(Some(Duration::ZERO), |event| {
                if let PollEvent::Main(main_event) = event {
                    handle_main_event(app, main_event);
                }
            });
        };

        // 1. TerminateWindow — unconfigure surface, release native window
        if TERM_WINDOW_PENDING.swap(false, Ordering::Relaxed) {
            log::info!("deferred: TerminateWindow (iter={})", iteration);
            INIT_WINDOW_DONE.store(false, Ordering::Relaxed);
            *LAST_CHROME_STYLE.lock().unwrap() = None;
            driver.on_surface_destroyed();
            if let Some(platform) = PLATFORM.get() {
                if let Some(win) = platform.primary_window() {
                    win.term_window();
                }
            }
            // Drain immediately — Java thread may be blocked on InitWindow condvar.
            drain_events(app);
        }

        // 2. InitWindow — replace surface on existing renderer, or create new
        if INIT_WINDOW_PENDING.swap(false, Ordering::Relaxed) {
            log::info!("deferred: InitWindow (iter={})", iteration);
            if let Some(platform) = PLATFORM.get() {
                if let Some(native_window) = app.native_window() {
                    let width = native_window.width();
                    let height = native_window.height();
                    log::info!("InitWindow: {}×{}", width, height);

                    platform.update_primary_display(&native_window, &app.asset_manager());

                    let scale_factor = platform
                        .primary_display()
                        .map(|d| d.scale_factor())
                        .unwrap_or(1.0);

                    if let Some(existing) = platform.primary_window() {
                        let gpu_ctx = platform.gpu_context();
                        match existing.init_window(native_window, gpu_ctx) {
                            Ok(()) => {
                                driver.on_surface_attached();
                                log::info!("InitWindow: reinitialised existing window");
                            }
                            Err(e) => {
                                log::error!("failed to reinit window surface: {e:#}");
                            }
                        }

                        // Refresh scale factor if density changed while backgrounded,
                        // then trigger GPUI resize so layout adapts to new dimensions.
                        if !existing.set_scale_factor(scale_factor) {
                            existing.handle_resize();
                        }
                        let cr = app.content_rect();
                        existing.update_safe_area_from_content_rect(
                            cr.left, cr.top, cr.right, cr.bottom,
                        );

                        INIT_WINDOW_DONE.store(true, Ordering::Relaxed);
                    } else {
                        match platform.open_window(native_window, scale_factor, false) {
                            Ok(win) => {
                                driver.on_surface_attached();
                                log::info!(
                                    "window opened — id={:#x} scale={:.3}",
                                    win.id(),
                                    scale_factor
                                );

                                let cr = app.content_rect();
                                win.update_safe_area_from_content_rect(
                                    cr.left, cr.top, cr.right, cr.bottom,
                                );
                            }
                            Err(e) => {
                                log::error!("failed to open window: {e:#}");
                            }
                        }
                    }
                }
            }
            // Drain — Java thread may have queued ConfigChanged/WindowResized.
            drain_events(app);
        }

        // 3. WindowResized
        if WINDOW_RESIZED_PENDING.swap(false, Ordering::Relaxed) {
            log::debug!("deferred: WindowResized");
            if let Some(platform) = PLATFORM.get() {
                if let Some(win) = platform.primary_window() {
                    win.handle_resize();
                    let cr = app.content_rect();
                    win.update_safe_area_from_content_rect(cr.left, cr.top, cr.right, cr.bottom);
                }
            }
        }

        // 4. ConfigChanged
        if CONFIG_CHANGED_PENDING.swap(false, Ordering::Relaxed) {
            log::debug!("deferred: ConfigChanged");
            if let Some(platform) = PLATFORM.get() {
                platform.notify_keyboard_layout_change();
                if let Some(native_window) = app.native_window() {
                    platform.update_primary_display(&native_window, &app.asset_manager());
                }
                let scale_factor = platform
                    .primary_display()
                    .map(|d| d.scale_factor())
                    .unwrap_or(1.0);
                let is_dark = query_night_mode_via_jni();
                if let Some(win) = platform.primary_window() {
                    let appearance = if is_dark {
                        crate::android::window::WindowAppearance::Dark
                    } else {
                        crate::android::window::WindowAppearance::Light
                    };
                    win.set_appearance(appearance);
                    if !win.set_scale_factor(scale_factor) {
                        // Font scale or other config changed without density change:
                        // force a fresh render pass on the render thread.
                        super::frame_source::resume();
                    }
                }
            }
        }

        // 5. Pause / background
        if PAUSE_PENDING.swap(false, Ordering::Relaxed) {
            log::info!("deferred: Pause (iter={})", iteration);
            let win = PLATFORM.get().and_then(|platform| platform.primary_window());
            driver.on_paused(win.as_deref());
            if let Some(platform) = PLATFORM.get() {
                platform.did_enter_background();
                if let Some(win) = win {
                    win.set_active(false);
                }
            }
            pause_platform_views();
        }

        // 6. Resume / foreground
        if RESUME_PENDING.swap(false, Ordering::Relaxed) {
            log::info!("deferred: Resume (iter={})", iteration);
            let win = PLATFORM.get().and_then(|platform| platform.primary_window());
            if let Some(platform) = PLATFORM.get() {
                platform.did_become_active();
                if let Some(win) = &win {
                    win.set_active(true);
                }
            }
            driver.on_resumed(win.as_deref());
            resume_platform_views();
        }

        // 7. TrimMemory / LowMemory — evict cached GPU tile/atlas memory under OS pressure.
        let trim_level = TRIM_MEMORY_PENDING.swap(0, Ordering::AcqRel);
        if trim_level > 0 {
            // Android ComponentCallbacks2 levels:
            //   RUNNING_MODERATE = 5, RUNNING_LOW = 10, RUNNING_CRITICAL = 15,
            //   UI_HIDDEN = 20, BACKGROUND = 40, MODERATE = 60, COMPLETE = 80.
            let clear_atlas =
                trim_level >= 40 || trim_level == 10 || trim_level == 15;
            log::info!(
                "deferred: TrimMemory level={} (clear_atlas={})",
                trim_level,
                clear_atlas
            );
            if let Some(platform) = PLATFORM.get() {
                if let Some(win) = platform.primary_window() {
                    win.trim_memory(clear_atlas);
                }
            }
        }

        // Track active/focused state.
        if let Some(platform) = PLATFORM.get() {
            if let Some(win) = platform.primary_window() {
                let is_active = win.is_active();
                if is_active != app_is_active {
                    log::info!(
                        "run_event_loop: active {} -> {} (iter={})",
                        app_is_active,
                        is_active,
                        iteration,
                    );
                    app_is_active = is_active;
                }
            }
        }

        // Process input events.
        process_input_events(app, &mut driver);

        // Deferred initialisation callbacks (runs once).
        if !INIT_WINDOW_DONE.load(Ordering::Relaxed) {
            if let Some(platform) = PLATFORM.get() {
                if platform.primary_window().is_some() {
                    if let Some(finish_cb) = platform.take_finish_launching_callback() {
                        log::info!("invoking finish_launching callback (iter={})", iteration);
                        finish_cb();
                    }

                    if let Some(init_cb) = platform.take_on_init_window_callback() {
                        let win = platform.primary_window().unwrap();
                        log::info!("invoking on_init_window callback (iter={})", iteration);
                        init_cb(win);
                    }

                    INIT_WINDOW_DONE.store(true, Ordering::Relaxed);
                    NATIVE_INITIALIZED.store(true, Ordering::Release);
                    log::info!("NATIVE_INITIALIZED = true");

                    // Render first frame immediately.
                    platform.flush_main_thread_tasks();
                    if let Some(win) = platform.primary_window() {
                        driver.render_immediate_frame(&win);
                    }
                }
            }
        }

        // ── Render ──
        if let Some(platform) = PLATFORM.get() {
            if INIT_WINDOW_DONE.load(Ordering::Relaxed) && app_is_active {
                platform.flush_main_thread_tasks();
                // Software-keyboard text bypasses GPUI's invalidator; the
                // frame callback turns it into a forced render.
                if crate::TEXT_INPUT_DIRTY.load(Ordering::Acquire) {
                    super::frame_source::schedule_frame();
                }
                if let Some(win) = platform.primary_window() {
                    driver.render_due_frame(
                        &win,
                        |driver| process_input_events(app, driver),
                        |driver| {
                            // Drain lifecycle events that arrived during rendering
                            // (e.g. rotation triggers TerminateWindow while we were
                            // in get_current_texture / present).
                            drain_events(app);
                            process_input_events(app, driver);
                        },
                    );
                }
            }
        }
    }

    log::info!("run_event_loop: exiting main loop");
}

/// Pause all platform views when the app goes to background.
fn pause_platform_views() {
    if crate::platform_view::PlatformViewRegistry::global().active_view_count() == 0 {
        return;
    }
    let _ = with_env(|env| {
        if let Ok(helper_class) = find_app_class(env, "dev.gpui.mobile.GpuiPlatformView") {
            let _ = env.call_static_method(
                &helper_class,
                jni::jni_str!("pauseAll"),
                jni::jni_sig!("()V"),
                &[],
            );
            env.exception_clear();
        }
        Ok(())
    });
}

/// Resume all platform views when the app returns to foreground.
fn resume_platform_views() {
    if crate::platform_view::PlatformViewRegistry::global().active_view_count() == 0 {
        return;
    }
    let _ = with_env(|env| {
        if let Ok(helper_class) = find_app_class(env, "dev.gpui.mobile.GpuiPlatformView") {
            let _ = env.call_static_method(
                &helper_class,
                jni::jni_str!("resumeAll"),
                jni::jni_sig!("()V"),
                &[],
            );
            env.exception_clear();
        }
        Ok(())
    });
}

/// Dispose all platform views during app shutdown.
fn dispose_all_platform_views() {
    if crate::platform_view::PlatformViewRegistry::global().active_view_count() == 0 {
        return;
    }
    let _ = with_env(|env| {
        if let Ok(helper_class) = find_app_class(env, "dev.gpui.mobile.GpuiPlatformView") {
            let _ = env.call_static_method(
                &helper_class,
                jni::jni_str!("disposeAll"),
                jni::jni_sig!("()V"),
                &[],
            );
            env.exception_clear();
        }
        Ok(())
    });
}

/// Handle a single `MainEvent` from `android-activity`.
fn handle_main_event(_app: &AndroidApp, event: MainEvent<'_>) {
    match event {
        MainEvent::InitWindow { .. } => {
            log::info!("MainEvent::InitWindow");
            // Defer to after poll_events to avoid deadlock with state lock.
            INIT_WINDOW_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::TerminateWindow { .. } => {
            log::info!("MainEvent::TerminateWindow");
            // Defer to after poll_events to avoid deadlock with state lock.
            TERM_WINDOW_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::WindowResized { .. } => {
            log::debug!("MainEvent::WindowResized");
            // Defer to after poll_events to avoid deadlock with state lock.
            WINDOW_RESIZED_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::GainedFocus => {
            log::info!("MainEvent::GainedFocus");
            RESUME_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::LostFocus => {
            log::info!("MainEvent::LostFocus");
            PAUSE_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::Resume { .. } => {
            log::info!("MainEvent::Resume");
            RESUME_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::Pause => {
            log::info!("MainEvent::Pause");
            // set_active uses AtomicBool so it never blocks.
            PAUSE_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::ConfigChanged { .. } => {
            log::debug!("MainEvent::ConfigChanged");
            CONFIG_CHANGED_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::Start => {
            log::info!("MainEvent::Start");
        }

        MainEvent::Stop => {
            log::info!("MainEvent::Stop");
        }

        MainEvent::SaveState { .. } => {
            log::info!("MainEvent::SaveState");
        }

        MainEvent::LowMemory => {
            log::warn!("MainEvent::LowMemory — scheduling aggressive GPU cache trim");
            // Matches Android ComponentCallbacks2.TRIM_MEMORY_COMPLETE (80).
            TRIM_MEMORY_PENDING.fetch_max(80, Ordering::AcqRel);
        }

        MainEvent::Destroy => {
            log::info!("MainEvent::Destroy");

            if let Some(platform) = PLATFORM.get() {
                platform.quit();
            }
        }

        MainEvent::InsetsChanged { .. } => {
            log::debug!("MainEvent::InsetsChanged");
            WINDOW_RESIZED_PENDING.store(true, Ordering::Relaxed);
        }

        MainEvent::ContentRectChanged { .. } => {
            log::debug!("MainEvent::ContentRectChanged");
            WINDOW_RESIZED_PENDING.store(true, Ordering::Relaxed);
        }

        _ => {
            log::trace!("MainEvent: other variant");
        }
    }
}

// ── main loop helper (compat with existing code) ──────────────────────────────

/// Run one iteration of the event loop.
///
/// This is a compatibility wrapper for code that uses a manual poll loop.
/// Prefer `run_event_loop` for the standard event loop.
///
/// `timeout_ms` — how long to block waiting for events (milliseconds).
/// Pass `0` for non-blocking, `-1` to block indefinitely.
///
/// Returns `true` if the application should exit.
pub fn poll_events(timeout_ms: i32) -> bool {
    if let Some(platform) = PLATFORM.get() {
        if platform.should_quit() {
            return true;
        }
        platform.tick();
    }

    let app = match ANDROID_APP.get() {
        Some(app) => app,
        None => return false,
    };

    let timeout = if timeout_ms < 0 {
        None
    } else {
        Some(Duration::from_millis(timeout_ms as u64))
    };

    app.poll_events(timeout, |event| match event {
        PollEvent::Main(main_event) => {
            handle_main_event(app, main_event);
        }
        PollEvent::Wake => {}
        _ => {}
    });

    let mut compat_driver = super::loop_driver::LoopDriver::without_adpf();
    process_input_events(app, &mut compat_driver);

    // Drive the GPUI rendering pipeline (same as run_event_loop).
    if let Some(platform) = PLATFORM.get() {
        platform.flush_main_thread_tasks();
        if let Some(win) = platform.primary_window() {
            win.request_frame();
        }
    }

    false
}

// ── public init / run helpers ─────────────────────────────────────────────────

/// Install a panic hook that routes panic messages to logcat.
///
/// Call this early in `android_main` so that any subsequent panic is
/// visible via `adb logcat`.  Safe to call multiple times — each call
/// replaces the previous hook.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        if let Some(loc) = info.location() {
            log::error!(
                "PANIC at {}:{}:{}: {}",
                loc.file(),
                loc.line(),
                loc.column(),
                payload
            );
        } else {
            log::error!("PANIC: {}", payload);
        }
    }));
}

/// Store the `AndroidApp` globally and create the `AndroidPlatform`.
///
/// Must be called exactly once from `android_main` before
/// `run_event_loop`.  Returns a reference to the platform so the caller
/// can register callbacks (e.g. `set_on_init_window`) before entering
/// the event loop.
pub fn init_platform(app: &AndroidApp) -> &'static Arc<AndroidPlatform> {
    let _ = ANDROID_APP.set(app.clone());
    log::info!("init_platform: stored AndroidApp");

    let platform = Arc::new(AndroidPlatform::new(false));
    log::info!("init_platform: AndroidPlatform created");

    PLATFORM
        .set(Arc::clone(&platform))
        .unwrap_or_else(|_| log::warn!("PLATFORM already set — duplicate init_platform?"));

    // SAFETY: we just set it above.
    PLATFORM.get().unwrap()
}

// ── system chrome (status bar / navigation bar) ───────────────────────────────

/// Cached last-applied system chrome style.
///
/// `set_system_chrome` is called on every frame render.  The JNI calls it
/// makes (getWindow, setStatusBarColor, etc.) are View operations that can
/// contend with the Android UI thread and intermittently deadlock.
/// By caching the last applied style we skip the JNI calls entirely when
/// nothing changed — which is the common case.
#[allow(clippy::type_complexity)]
static LAST_CHROME_STYLE: std::sync::Mutex<
    Option<(Option<u32>, Option<u32>, crate::StatusBarContentStyle)>,
> = std::sync::Mutex::new(None);

/// Apply system chrome styling on Android.
///
/// Sets the status bar color, navigation bar color, and light/dark
/// status bar icons via JNI calls to `Window` and `WindowInsetsController`.
///
/// Must be called from the main (native) thread that has JNI access.
pub fn set_system_chrome(style: &crate::SystemChromeStyle) {
    let status_bar_color = style.status_bar_color;
    let navigation_bar_color = style.navigation_bar_color;
    let status_bar_style = style.status_bar_style;

    // Skip the (expensive, potentially deadlocking) JNI calls when nothing changed.
    {
        let key = (status_bar_color, navigation_bar_color, status_bar_style);
        let mut last = LAST_CHROME_STYLE.lock().unwrap();
        if *last == Some(key) {
            return;
        }
        *last = Some(key);
    }

    let result = with_env(|env| {
        let activity_obj = activity(env)?;

        // 1. Get the Window: activity.getWindow()
        let window = env
            .call_method(
                &activity_obj,
                jni::jni_str!("getWindow"),
                jni::jni_sig!("()Landroid/view/Window;"),
                &[],
            )
            .and_then(|v: jni::objects::JValueOwned| v.l())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })?;
        if window.is_null() {
            return Err("getWindow returned null".into());
        }

        // 2. Set status bar color if provided
        if let Some(color) = status_bar_color {
            let argb = (0xFF000000_u32 | color) as i32;
            let _ = env.call_method(
                &window,
                jni::jni_str!("setStatusBarColor"),
                jni::jni_sig!("(I)V"),
                &[JValue::Int(argb)],
            );
            env.exception_clear();
        }

        // 3. Set navigation bar color if provided
        if let Some(color) = navigation_bar_color {
            let argb = (0xFF000000_u32 | color) as i32;
            let _ = env.call_method(
                &window,
                jni::jni_str!("setNavigationBarColor"),
                jni::jni_sig!("(I)V"),
                &[JValue::Int(argb)],
            );
            env.exception_clear();
        }

        // 4. Set light/dark status bar icons via WindowInsetsController (API 30+)
        let insetsctl = env.call_method(
            &window,
            jni::jni_str!("getInsetsController"),
            jni::jni_sig!("()Landroid/view/WindowInsetsController;"),
            &[],
        );

        if let Ok(v) = insetsctl {
            if let Ok(ctl) = v.l() {
                if !ctl.is_null() {
                    let mask: i32 = 0x00000008;
                    let appearance: i32 = match status_bar_style {
                        crate::StatusBarContentStyle::Dark => 0x00000008,
                        crate::StatusBarContentStyle::Light => 0,
                    };
                    let _ = env.call_method(
                        &ctl,
                        jni::jni_str!("setSystemBarsAppearance"),
                        jni::jni_sig!("(II)V"),
                        &[JValue::Int(appearance), JValue::Int(mask)],
                    );
                    env.exception_clear();
                }
            }
        } else {
            env.exception_clear();

            if let Ok(decor) = env
                .call_method(
                    &window,
                    jni::jni_str!("getDecorView"),
                    jni::jni_sig!("()Landroid/view/View;"),
                    &[],
                )
                .and_then(|v: jni::objects::JValueOwned| v.l())
            {
                if !decor.is_null() {
                    if let Ok(current) = env
                        .call_method(
                            &decor,
                            jni::jni_str!("getSystemUiVisibility"),
                            jni::jni_sig!("()I"),
                            &[],
                        )
                        .and_then(|v: jni::objects::JValueOwned| v.i())
                    {
                        let new_flags = match status_bar_style {
                            crate::StatusBarContentStyle::Dark => current | 0x00002000,
                            crate::StatusBarContentStyle::Light => current & !0x00002000,
                        };
                        let _ = env.call_method(
                            &decor,
                            jni::jni_str!("setSystemUiVisibility"),
                            jni::jni_sig!("(I)V"),
                            &[JValue::Int(new_flags)],
                        );
                        env.exception_clear();
                    }
                }
            }
        }

        Ok(())
    });

    if let Err(e) = result {
        log::warn!("set_system_chrome: {e}");
    }

    log::info!(
        "set_system_chrome: status_bar_color={:?}, nav_bar_color={:?}, style={:?}",
        style.status_bar_color,
        style.navigation_bar_color,
        style.status_bar_style
    );
}

// ── software keyboard (IME) control ───────────────────────────────────────────

/// The keyboard type currently requested, or `None` when hidden.
///
/// The Java-side `InputProxy` belongs to the Activity, so an Activity recreation takes
/// the IME with it — while GPUI still considers the same field focused and therefore
/// never asks for the keyboard again. Remembering the request lets a new Activity put
/// the IME back; see [`restore_keyboard`].
static SHOWN_KEYBOARD: std::sync::Mutex<Option<crate::KeyboardType>> = std::sync::Mutex::new(None);

/// Re-request the keyboard on the current Activity if one was showing.
///
/// Called by [`super::host`] once a recreated Activity's surface is attached. A fresh
/// IME session is started, which is what we want: the proxy is a new object.
pub(crate) fn restore_keyboard() {
    let keyboard_type = *SHOWN_KEYBOARD.lock().expect("poisoned");
    if let Some(keyboard_type) = keyboard_type {
        log::info!("restore_keyboard: re-showing {keyboard_type:?} on the new Activity");
        show_keyboard_android(keyboard_type);
    }
}

/// Show the UI-thread EditText proxy supplied by GpuiInputActivity.
///
/// Unlike NativeActivity's key-event-only connection, its InputConnection
/// supports composing text, commits and Unicode surrounding-text deletion.
pub fn show_keyboard_android(keyboard_type: crate::KeyboardType) {
    *SHOWN_KEYBOARD.lock().expect("poisoned") = Some(keyboard_type);
    let kind = match keyboard_type {
        crate::KeyboardType::Default => 0,
        crate::KeyboardType::EmailAddress => 1,
        crate::KeyboardType::Phone => 2,
        crate::KeyboardType::NumberPad => 3,
        crate::KeyboardType::URL => 4,
        crate::KeyboardType::Decimal => 5,
    };
    let session = super::text_input::new_session();
    if let Err(error) = with_env(|env| {
        let activity = activity(env)?;
        env.call_method(
            &activity,
            jni::jni_str!("gpuiShowKeyboard"),
            jni::jni_sig!("(IJ)V"),
            &[JValue::Int(kind), JValue::Long(session as i64)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }) {
        log::warn!("IME requires GpuiInputActivity: {error}");
        let _ = with_env(|env| {
            env.exception_clear();
            Ok(())
        });
    }
}

/// Hide the software keyboard on Android.
///
/// Invalidates queued IME callbacks and clears the native composition buffer.
pub fn hide_keyboard_android() {
    *SHOWN_KEYBOARD.lock().expect("poisoned") = None;
    let session = super::text_input::new_session();
    let _ = with_env(|env| {
        let activity = activity(env)?;
        let result = env.call_method(
            &activity,
            jni::jni_str!("gpuiHideKeyboard"),
            jni::jni_sig!("(J)V"),
            &[JValue::Long(session as i64)],
        );
        env.exception_clear();
        result.map_err(|e| e.to_string())?;
        Ok(())
    });
    if let Some(app) = android_app() {
        app.hide_soft_input(false);
    }
}

pub(super) fn reset_keyboard_composition() {
    let session = super::text_input::new_session();
    let _ = with_env(|env| {
        let activity = activity(env)?;
        let result = env.call_method(
            &activity,
            jni::jni_str!("gpuiResetComposition"),
            jni::jni_sig!("(J)V"),
            &[JValue::Long(session as i64)],
        );
        env.exception_clear();
        result.map_err(|e| e.to_string())?;
        Ok(())
    });
}

/// Receive Java InputConnection updates without touching GPUI on the UI thread.
///
/// # Safety
/// Called by JNI with a valid Java string and JNI call frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeIme(
    _env: *mut c_void,
    _class: *mut c_void,
    session: i64,
    kind: i32,
    text: *mut c_void,
    start: i32,
    end: i32,
) {
    let _ = with_env(|env| {
        let text = unsafe { JObject::from_raw(env, text as jni::sys::jobject) };
        super::text_input::enqueue(super::text_input::ImeEvent {
            session: session as u64,
            kind,
            text: get_string(env, &text),
            start: start.max(0) as usize,
            end: end.max(0) as usize,
        });
        Ok(())
    });
}

/// Receive Android system back (`OnBackInvokedCallback` / `onBackPressed`) without touching GPUI
/// on the UI thread.
///
/// # Safety
/// Called by JNI from `GpuiInputActivity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeBack(
    _env: *mut c_void,
    _class: *mut c_void,
) {
    super::text_input::enqueue_back();
}

/// Receive Android 14+ `OnBackAnimationCallback.onBackStarted` without touching GPUI on UI thread.
///
/// # Safety
/// Called by JNI from `GpuiInputActivity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeBackStarted(
    _env: *mut c_void,
    _class: *mut c_void,
    progress: f32,
    edge: i32,
) {
    super::text_input::enqueue_predictive_back(super::text_input::PredictiveBackEvent::Started {
        progress,
        edge,
    });
}

/// Receive Android 14+ `OnBackAnimationCallback.onBackProgressed` without touching GPUI on UI thread.
///
/// # Safety
/// Called by JNI from `GpuiInputActivity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeBackProgressed(
    _env: *mut c_void,
    _class: *mut c_void,
    progress: f32,
    edge: i32,
) {
    super::text_input::enqueue_predictive_back(
        super::text_input::PredictiveBackEvent::Progressed { progress, edge },
    );
}

/// Receive Android 14+ `OnBackAnimationCallback.onBackCancelled` without touching GPUI on UI thread.
///
/// # Safety
/// Called by JNI from `GpuiInputActivity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeBackCancelled(
    _env: *mut c_void,
    _class: *mut c_void,
) {
    super::text_input::enqueue_predictive_back(super::text_input::PredictiveBackEvent::Cancelled);
}

/// Schedules a single vsync frame for lock-free IME animation updates (MS-449) without invoking
/// `request_config_refresh()` or dropping Scroll Layers.
pub fn schedule_ime_frame() {
    super::text_input::mark_ime_insets_dirty();
}

/// Registers or unregisters `GpuiInputActivity`'s system back callback so the root inbox yields
/// to Android's native Back-to-Home preview animation (MS-449).
pub fn set_back_enabled_android(enabled: bool) {
    static LAST_BACK_ENABLED: AtomicBool = AtomicBool::new(false);
    if LAST_BACK_ENABLED.swap(enabled, Ordering::AcqRel) == enabled {
        return;
    }
    let _ = with_env(|env| {
        let activity = activity(env)?;
        let result = env.call_method(
            &activity,
            jni::jni_str!("gpuiSetBackEnabled"),
            jni::jni_sig!("(Z)V"),
            &[JValue::Bool(enabled)],
        );
        env.exception_clear();
        result.map_err(|e| e.to_string())?;
        Ok(())
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

// ── JNI export for GpuiActivity splash screen ────────────────────────────────

/// Called from `GpuiActivity.nativeIsInitialized()` to check whether the
/// native library has finished initializing (first frame rendered).
///
/// The AndroidX SplashScreen API calls this via `setKeepOnScreenCondition`
/// to hold the splash visible until GPUI is ready.
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiActivity_nativeIsInitialized(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) -> u8 {
    if NATIVE_INITIALIZED.load(Ordering::Acquire) {
        1 // JNI_TRUE
    } else {
        0 // JNI_FALSE
    }
}

/// JNI bridge: receive a deeplink URL from `GpuiActivity.onNewIntent()`.
///
/// When the app is already running and a deeplink is opened (e.g. via
/// `adb shell am start -d gpui://video_player`), the Java side calls
/// this to notify the Rust deeplink handler.
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread with a valid `url` jobject.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiActivity_nativeOnDeepLink(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    url: *mut std::ffi::c_void,
) {
    // We already have a JVM attached on this thread (UI thread).
    // Use with_env to get a properly wrapped Env handle.
    let url_raw = url as jni::sys::jobject;
    let _ = with_env(|env| {
        let url_obj = unsafe { JObject::from_raw(env, url_raw) };
        let url_string = get_string(env, &url_obj);
        // Don't let the JObject be dropped (it's owned by the JNI call frame).
        if url_string.is_empty() {
            return Ok(());
        }
        log::info!("nativeOnDeepLink: {}", url_string);

        #[cfg(feature = "deeplink")]
        {
            crate::packages::deeplink::notify_deep_link(&url_string);
        }
        Ok(())
    });
}

/// JNI bridge: receive a media action from `GpuiMediaSession` system controls.
///
/// Actions: "play", "pause", "stop", "next", "previous"
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread with a valid `action` jobject.
#[cfg(feature = "media_session")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiMediaSession_nativeMediaAction(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    action: *mut std::ffi::c_void,
) {
    let action_raw = action as jni::sys::jobject;
    let _ = with_env(|env| {
        let action_obj = unsafe { JObject::from_raw(env, action_raw) };
        let action_str = get_string(env, &action_obj);

        let media_action = match action_str.as_str() {
            "play" => crate::packages::media_session::MediaAction::Play,
            "pause" => crate::packages::media_session::MediaAction::Pause,
            "stop" => crate::packages::media_session::MediaAction::Stop,
            "next" => crate::packages::media_session::MediaAction::Next,
            "previous" => crate::packages::media_session::MediaAction::Previous,
            other => {
                log::warn!("nativeMediaAction: unknown action '{}'", other);
                return Ok(());
            }
        };

        log::info!("nativeMediaAction: {:?}", media_action);
        crate::packages::media_session::notify_action(media_action);
        Ok(())
    });
}

/// JNI bridge: receive a seek request from `GpuiMediaSession` system controls.
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread.
#[cfg(feature = "media_session")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiMediaSession_nativeMediaSeek(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    position_ms: i64,
) {
    log::info!("nativeMediaSeek: {}ms", position_ms);
    crate::packages::media_session::notify_seek(position_ms as u64);
}

/// Invokes `dev.gpui.mobile.MailstreamFcm.initializeWithOptions(...)` over JNI so runtime
/// Firebase client options synced via Device Sync initialize FCM on Android (MS-443).
pub fn init_fcm_options_via_jni(
    app_id: &str,
    api_key: &str,
    project_id: &str,
    sender_id: &str,
) -> Result<(), String> {
    with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.MailstreamFcm")?;
        let j_app_id = env.new_string(app_id).e()?;
        let j_api_key = env.new_string(api_key).e()?;
        let j_project_id = env.new_string(project_id).e()?;
        let j_sender_id = env.new_string(sender_id).e()?;
        env.call_static_method(
            &cls,
            jni::jni_str!("initializeWithOptions"),
            jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
            &[
                JValue::Object(&act),
                JValue::Object(&j_app_id),
                JValue::Object(&j_api_key),
                JValue::Object(&j_project_id),
                JValue::Object(&j_sender_id),
            ],
        )
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_events_returns_false_when_no_platform() {
        // PLATFORM is not set in a unit-test context, so poll_events should
        // be a safe no-op and return false (don't quit).
        let result = poll_events(0);
        let _ = result;
    }

    #[test]
    fn java_vm_returns_null_before_init() {
        // Before android_main is called, java_vm() should return null.
        let vm = java_vm();
        assert!(vm.is_null());
    }

    #[test]
    fn activity_as_ptr_returns_null_before_init() {
        let ptr = activity_as_ptr();
        assert!(ptr.is_null());
    }

    #[test]
    fn android_app_returns_none_before_init() {
        assert!(android_app().is_none());
    }

    #[test]
    fn platform_returns_none_before_init() {
        assert!(platform().is_none());
    }
}
