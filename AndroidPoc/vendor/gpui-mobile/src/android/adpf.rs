//! Android Dynamic Performance Framework (ADPF) performance hint session.
//!
//! Dynamically resolves `APerformanceHint_*` from `libandroid.so` via `dlsym`
//! so the shared library keeps loading cleanly on `minSdk = 26` devices while
//! providing exact frame-budget hints (`85% * refresh_interval_ns`) on
//! Android 12+ / 13+ devices that expose the NDK symbols.

#![allow(unsafe_code)]
use std::{ffi::c_void, sync::LazyLock};

use crate::frame_timing::{adpf_actual_work_duration_ns, adpf_target_work_duration_ns};

#[repr(C)]
struct APerformanceHintManager {
    _private: [u8; 0],
}

#[repr(C)]
struct APerformanceHintSession {
    _private: [u8; 0],
}

type GetManagerFn = unsafe extern "C" fn() -> *mut APerformanceHintManager;
type CreateSessionFn = unsafe extern "C" fn(
    *mut APerformanceHintManager,
    *const i32,
    usize,
    i64,
) -> *mut APerformanceHintSession;
type UpdateTargetWorkDurationFn =
    unsafe extern "C" fn(*mut APerformanceHintSession, i64) -> libc::c_int;
type ReportActualWorkDurationFn =
    unsafe extern "C" fn(*mut APerformanceHintSession, i64) -> libc::c_int;
type CloseSessionFn = unsafe extern "C" fn(*mut APerformanceHintSession);

struct AdpfApi {
    get_manager: GetManagerFn,
    create_session: CreateSessionFn,
    update_target_work_duration: UpdateTargetWorkDurationFn,
    report_actual_work_duration: ReportActualWorkDurationFn,
    close_session: CloseSessionFn,
}

fn load_symbol(name: &[u8]) -> *mut c_void {
    debug_assert_eq!(name.last().copied(), Some(0));
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }
}

static ADPF_API: LazyLock<Option<AdpfApi>> = LazyLock::new(|| {
    let api_level = unsafe { ndk_sys::android_get_device_api_level() };
    if api_level < 31 {
        log::debug!("ADPF: skipping on Android API {api_level}");
        return None;
    }
    let get_manager = load_symbol(b"APerformanceHint_getManager\0");
    let create_session = load_symbol(b"APerformanceHint_createSession\0");
    let update_target = load_symbol(b"APerformanceHint_updateTargetWorkDuration\0");
    let report_actual = load_symbol(b"APerformanceHint_reportActualWorkDuration\0");
    let close_session = load_symbol(b"APerformanceHint_closeSession\0");

    if get_manager.is_null()
        || create_session.is_null()
        || update_target.is_null()
        || report_actual.is_null()
        || close_session.is_null()
    {
        log::debug!("ADPF: NDK APerformanceHint symbols unavailable on API {api_level}");
        return None;
    }

    // SAFETY: symbol names and signatures match Android NDK `performance_hint.h`.
    Some(unsafe {
        AdpfApi {
            get_manager: std::mem::transmute::<*mut c_void, GetManagerFn>(get_manager),
            create_session: std::mem::transmute::<*mut c_void, CreateSessionFn>(create_session),
            update_target_work_duration: std::mem::transmute::<
                *mut c_void,
                UpdateTargetWorkDurationFn,
            >(update_target),
            report_actual_work_duration: std::mem::transmute::<
                *mut c_void,
                ReportActualWorkDurationFn,
            >(report_actual),
            close_session: std::mem::transmute::<*mut c_void, CloseSessionFn>(close_session),
        }
    })
});

fn load_adpf_api() -> Option<&'static AdpfApi> {
    ADPF_API.as_ref()
}

/// Active ADPF hint session owned by the Android render loop thread.
///
/// `APerformanceHintSession` methods are not thread-safe, so this struct stays
/// confined to the loop thread (`!Send`, `!Sync`).
pub(crate) struct AdpfSession {
    api: &'static AdpfApi,
    session: *mut APerformanceHintSession,
    target_ns: i64,
    paused: bool,
}

impl AdpfSession {
    /// Creates an ADPF session bound to `[render_loop_tid, vsync_thread_tid]`
    /// with an initial work budget of `85% * refresh_interval_ns`.
    pub(crate) fn new(
        render_loop_tid: i32,
        vsync_thread_tid: Option<i32>,
        refresh_interval_ns: i64,
    ) -> Option<Self> {
        let api = load_adpf_api()?;
        // SAFETY: `get_manager` takes no arguments and returns null if unsupported.
        let manager = unsafe { (api.get_manager)() };
        if manager.is_null() {
            log::debug!("ADPF: APerformanceHint_getManager returned null");
            return None;
        }

        let mut tids = Vec::with_capacity(2);
        if render_loop_tid > 0 {
            tids.push(render_loop_tid);
        }
        if let Some(vsync_tid) = vsync_thread_tid {
            if vsync_tid > 0 && vsync_tid != render_loop_tid {
                tids.push(vsync_tid);
            }
        }
        if tids.is_empty() {
            return None;
        }

        let target_ns = adpf_target_work_duration_ns(refresh_interval_ns);
        // SAFETY: `manager` is non-null, `tids` is valid for `tids.len()` items,
        // and `target_ns > 0`.
        let session =
            unsafe { (api.create_session)(manager, tids.as_ptr(), tids.len(), target_ns) };
        if session.is_null() {
            log::debug!(
                "ADPF: APerformanceHint_createSession returned null for tids={tids:?}"
            );
            return None;
        }

        log::info!(
            "ADPF: created performance hint session tids={tids:?} target={:.2} ms",
            target_ns as f64 / 1_000_000.0,
        );

        Some(Self {
            api,
            session,
            target_ns,
            paused: false,
        })
    }

    /// Updates the session target duration (`85% * refresh_interval_ns`) whenever
    /// the display refresh interval changes.
    pub(crate) fn update_target_for_refresh(&mut self, refresh_interval_ns: i64) {
        let target_ns = adpf_target_work_duration_ns(refresh_interval_ns);
        if target_ns == self.target_ns {
            return;
        }
        // SAFETY: `self.session` is valid and `target_ns > 0`.
        let status = unsafe { (self.api.update_target_work_duration)(self.session, target_ns) };
        if status == 0 {
            self.target_ns = target_ns;
            log::debug!(
                "ADPF: updated target work duration to {:.2} ms",
                target_ns as f64 / 1_000_000.0,
            );
        } else {
            log::debug!("ADPF: updateTargetWorkDuration({target_ns}) failed status={status}");
        }
    }

    /// Reports the actual CPU/frame work duration, deducting swapchain
    /// `get_current_texture()` wait time so `Fifo` back-pressure does not
    /// mislead the scheduler into locking prime cores at max frequency.
    pub(crate) fn report_frame(
        &self,
        frame_start_ns: i64,
        frame_end_ns: i64,
        acquire_wait_ns: i64,
    ) {
        if self.paused {
            return;
        }
        let actual_ns = adpf_actual_work_duration_ns(frame_start_ns, frame_end_ns, acquire_wait_ns);
        // SAFETY: `self.session` is valid and `actual_ns >= 100_000 > 0`.
        let status = unsafe { (self.api.report_actual_work_duration)(self.session, actual_ns) };
        if status != 0 {
            log::trace!("ADPF: reportActualWorkDuration({actual_ns}) status={status}");
        }
    }

    /// Pauses or resumes ADPF duration reporting (must stop reporting while
    /// the Activity is paused/backgrounded).
    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }
}

impl Drop for AdpfSession {
    fn drop(&mut self) {
        // SAFETY: `self.session` was returned by `APerformanceHint_createSession`
        // and is closed exactly once here.
        unsafe { (self.api.close_session)(self.session) };
    }
}
