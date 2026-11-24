//! Vsync-paced, demand-driven frames for the Android loops.
//!
//! Both loops — [`jni::run_event_loop`](super::jni::run_event_loop) over
//! `android-activity` and the [`host`](super::host) render thread — used to
//! call [`AndroidWindow::request_frame`](super::window::AndroidWindow::request_frame)
//! on every iteration and pace themselves with a sleep, leaving the frame rate
//! to the swapchain: with `Mailbox` presentation a scrolling window was drawn
//! as fast as the CPU could lay it out, most of those frames were dropped by
//! the compositor, and an idle window still cost a wakeup every 500 µs.
//!
//! Now the loop draws only when GPUI asked for a frame *and* a vsync has
//! passed since:
//!
//! 1. GPUI raises demand through `PlatformWindow::frame_waker` /
//!    `schedule_frame` (both forwarded here by `AndroidPlatformWindow`), a
//!    surface swap or a resume raise it through [`resume`], and software
//!    keyboard input raises it from the UI thread through [`wake`].
//! 2. The first demand since the last frame asks the vsync thread to post
//!    one `AChoreographer` frame callback. When it fires, the vsync thread
//!    marks a frame due and wakes the loop's `ALooper`.
//! 3. The loop takes the due frame with [`take_frame`] and ticks GPUI once.
//!    A frame that left the window dirty (an animation, a fling) re-raises
//!    demand from inside the tick, which posts the next callback.
//!
//! Between frames the loop blocks on the looper for as long as
//! [`poll_timeout`] allows; input, main-thread tasks and lifecycle commands
//! all reach the looper through their own file descriptors, so nothing is
//! missed while it sleeps.
//!
//! The choreographer lives on its own thread rather than the loop's looper
//! because `android-activity` treats every looper source it did not register
//! as an error (`Spurious ALOOPER_POLL_CALLBACK`, logged on each delivery —
//! at 60 Hz that is a logcat flood); a plain `ALooper_wake` is the one signal
//! it accepts silently. The hop costs a few microseconds per frame.
//!
//! The pacing itself lives in the platform-neutral, unit-tested
//! [`FramePacer`]; this module is the thread-local instance plus the vsync
//! thread and `ALooper` glue around it. Without a choreographer (no looper
//! available to the vsync thread) the pacer spaces frames by a 60 Hz clock.

use std::{
    cell::RefCell,
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering},
        mpsc, LazyLock, OnceLock,
    },
    time::{Duration, Instant},
};

use crate::{
    frame_pacer::FramePacer,
    frame_timing::{MonotonicClockAnchor, VsyncTimeline},
};

/// Longest the loop blocks with nothing due; a missed wakeup costs at most
/// this much latency instead of a frozen app.
const IDLE_POLL_CAP: Duration = Duration::from_secs(1);

/// Default refresh interval before the display's real refresh rate is queried.
const DEFAULT_REFRESH_INTERVAL_NS: u64 = 16_666_667;

/// Active display refresh interval in nanoseconds (updated from `Display.getRefreshRate()`).
static REFRESH_INTERVAL_NS: AtomicU64 = AtomicU64::new(DEFAULT_REFRESH_INTERVAL_NS);

#[repr(C)]
struct AChoreographerFrameCallbackData {
    _private: [u8; 0],
}

type ChoreographerVsyncCallback =
    unsafe extern "C" fn(*const AChoreographerFrameCallbackData, *mut c_void);
type ChoreographerFrameCallback64 = unsafe extern "C" fn(i64, *mut c_void);

type PostVsyncCallbackFn =
    unsafe extern "C" fn(*mut ndk_sys::AChoreographer, ChoreographerVsyncCallback, *mut c_void);
type GetFrameTimeNanosFn = unsafe extern "C" fn(*const AChoreographerFrameCallbackData) -> i64;
type GetFrameTimelinesLengthFn =
    unsafe extern "C" fn(*const AChoreographerFrameCallbackData) -> usize;
type GetPreferredFrameTimelineIndexFn =
    unsafe extern "C" fn(*const AChoreographerFrameCallbackData) -> usize;
type GetExpectedPresentationTimeNanosFn =
    unsafe extern "C" fn(*const AChoreographerFrameCallbackData, usize) -> i64;
type GetDeadlineNanosFn =
    unsafe extern "C" fn(*const AChoreographerFrameCallbackData, usize) -> i64;
type PostFrameCallback64Fn =
    unsafe extern "C" fn(*mut ndk_sys::AChoreographer, ChoreographerFrameCallback64, *mut c_void);

struct ChoreographerVsyncApi {
    post_vsync_callback: PostVsyncCallbackFn,
    get_frame_time_nanos: GetFrameTimeNanosFn,
    get_frame_timelines_length: GetFrameTimelinesLengthFn,
    get_preferred_frame_timeline_index: GetPreferredFrameTimelineIndexFn,
    get_expected_presentation_time_nanos: GetExpectedPresentationTimeNanosFn,
    get_deadline_nanos: GetDeadlineNanosFn,
}

fn load_libandroid_symbol(name: &[u8]) -> *mut c_void {
    debug_assert_eq!(name.last().copied(), Some(0));
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }
}

static VSYNC_API: LazyLock<Option<ChoreographerVsyncApi>> = LazyLock::new(|| {
    let api_level = unsafe { ndk_sys::android_get_device_api_level() };
    if api_level < 33 {
        return None;
    }
    let post_vsync = load_libandroid_symbol(b"AChoreographer_postVsyncCallback\0");
    let get_frame_time =
        load_libandroid_symbol(b"AChoreographerFrameCallbackData_getFrameTimeNanos\0");
    let get_len =
        load_libandroid_symbol(b"AChoreographerFrameCallbackData_getFrameTimelinesLength\0");
    let get_preferred =
        load_libandroid_symbol(b"AChoreographerFrameCallbackData_getPreferredFrameTimelineIndex\0");
    let get_expected_present = load_libandroid_symbol(
        b"AChoreographerFrameCallbackData_getFrameTimelineExpectedPresentationTimeNanos\0",
    );
    let get_deadline =
        load_libandroid_symbol(b"AChoreographerFrameCallbackData_getFrameTimelineDeadlineNanos\0");

    if post_vsync.is_null()
        || get_frame_time.is_null()
        || get_len.is_null()
        || get_preferred.is_null()
        || get_expected_present.is_null()
        || get_deadline.is_null()
    {
        return None;
    }

    // SAFETY: symbol names and signatures match Android NDK `choreographer.h` (API 33+).
    Some(unsafe {
        ChoreographerVsyncApi {
            post_vsync_callback: std::mem::transmute::<*mut c_void, PostVsyncCallbackFn>(
                post_vsync,
            ),
            get_frame_time_nanos: std::mem::transmute::<*mut c_void, GetFrameTimeNanosFn>(
                get_frame_time,
            ),
            get_frame_timelines_length: std::mem::transmute::<
                *mut c_void,
                GetFrameTimelinesLengthFn,
            >(get_len),
            get_preferred_frame_timeline_index: std::mem::transmute::<
                *mut c_void,
                GetPreferredFrameTimelineIndexFn,
            >(get_preferred),
            get_expected_presentation_time_nanos: std::mem::transmute::<
                *mut c_void,
                GetExpectedPresentationTimeNanosFn,
            >(get_expected_present),
            get_deadline_nanos: std::mem::transmute::<*mut c_void, GetDeadlineNanosFn>(
                get_deadline,
            ),
        }
    })
});

static POST_FRAME_CALLBACK_64: LazyLock<Option<PostFrameCallback64Fn>> = LazyLock::new(|| {
    let api_level = unsafe { ndk_sys::android_get_device_api_level() };
    if api_level < 29 {
        return None;
    }
    let symbol = load_libandroid_symbol(b"AChoreographer_postFrameCallback64\0");
    if symbol.is_null() {
        None
    } else {
        // SAFETY: symbol signature matches Android NDK `choreographer.h` (API 29+).
        Some(unsafe { std::mem::transmute::<*mut c_void, PostFrameCallback64Fn>(symbol) })
    }
});

thread_local! {
    /// The loop thread's pacer, installed by [`install`].
    static PACER: RefCell<Option<FramePacer>> = const { RefCell::new(None) };
}

/// The loop thread's `ALooper`, for [`wake`] from other threads.
static MAIN_LOOPER: AtomicPtr<ndk_sys::ALooper> = AtomicPtr::new(ptr::null_mut());

/// The vsync thread's `ALooper`; null while it has no choreographer.
static VSYNC_LOOPER: AtomicPtr<ndk_sys::ALooper> = AtomicPtr::new(ptr::null_mut());

/// Linux/Android TID of the dedicated `gpui-vsync` thread (for ADPF binding).
static VSYNC_THREAD_TID: AtomicI32 = AtomicI32::new(0);

/// The vsync thread, spawned once per process; `true` if it found a
/// choreographer.
static VSYNC_THREAD: OnceLock<bool> = OnceLock::new();

/// The loop asked the vsync thread to post a frame callback.
static POST_REQUESTED: AtomicBool = AtomicBool::new(false);

/// A frame callback fired and its frame has not been handed to the pacer yet.
static FRAME_DUE: AtomicBool = AtomicBool::new(false);

/// Demand raised from a thread that is not the loop thread; the loop turns it
/// into [`schedule_frame`] on its next iteration.
static OFF_THREAD_DEMAND: AtomicBool = AtomicBool::new(false);

/// Latest hardware vsync timeline and pending presentation-aligned `signal_at`
/// passed through `RequestFrameOptions::signal_at` for `FrameClock` and
/// `tick_momentum_at`.
struct VsyncState {
    latest: Option<VsyncTimeline>,
    pending_signal_at: Option<Instant>,
}

static VSYNC_STATE: parking_lot::Mutex<VsyncState> = parking_lot::Mutex::new(VsyncState {
    latest: None,
    pending_signal_at: None,
});
/// Installs the pacer on the calling thread, which must own an `ALooper`
/// (`android-activity`'s native thread, or the host render thread after
/// `ALooper_prepare`). Idempotent; spawns the vsync thread on first use.
pub(crate) fn install() {
    // SAFETY: plain query of the calling thread's looper.
    let looper = unsafe { ndk_sys::ALooper_forThread() };
    MAIN_LOOPER.store(looper, Ordering::Release);

    let has_choreographer = *VSYNC_THREAD.get_or_init(spawn_vsync_thread);
    PACER.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let interval = Duration::from_nanos(REFRESH_INTERVAL_NS.load(Ordering::Relaxed));
        let pacer = if has_choreographer {
            FramePacer::with_vsync(
                Box::new(|| {
                    POST_REQUESTED.store(true, Ordering::Release);
                    wake_looper(&VSYNC_LOOPER);
                }),
                interval,
            )
        } else {
            log::warn!("frame_source: no AChoreographer available; pacing by clock");
            FramePacer::with_clock(interval)
        };
        *slot.borrow_mut() = Some(pacer);
    });
}

/// Starts the vsync thread and waits until it reports whether it has a
/// choreographer, so the caller can pick the pacer to match.
fn spawn_vsync_thread() -> bool {
    let (ready_tx, ready_rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("gpui-vsync".into())
        .spawn(move || vsync_thread(ready_tx));
    match spawned {
        Ok(_) => ready_rx.recv().unwrap_or(false),
        Err(err) => {
            log::warn!("frame_source: could not spawn the vsync thread: {err}");
            false
        }
    }
}

fn vsync_thread(ready: mpsc::Sender<bool>) {
    // SAFETY: called once on this fresh thread; `AChoreographer_getInstance`
    // needs the looper to exist first and returns null otherwise.
    let (looper, choreographer) = unsafe {
        let looper = ndk_sys::ALooper_prepare(0);
        let choreographer = if looper.is_null() {
            ptr::null_mut()
        } else {
            ndk_sys::AChoreographer_getInstance()
        };
        (looper, choreographer)
    };
    if choreographer.is_null() {
        let _ = ready.send(false);
        return;
    }
    VSYNC_THREAD_TID.store(unsafe { libc::gettid() as i32 }, Ordering::Release);
    VSYNC_LOOPER.store(looper, Ordering::Release);
    let api_tier = if VSYNC_API.is_some() {
        "API 33+ AChoreographer_postVsyncCallback"
    } else if POST_FRAME_CALLBACK_64.is_some() {
        "API 29-32 AChoreographer_postFrameCallback64"
    } else {
        "API 26-28 AChoreographer_postFrameCallback"
    };
    log::info!("frame_source: vsync thread ready ({api_tier})");
    let _ = ready.send(true);

    loop {
        if POST_REQUESTED.swap(false, Ordering::AcqRel) {
            // SAFETY: the choreographer belongs to this thread and lives as
            // long as it; the callback takes no data pointer.
            unsafe {
                if let Some(api) = VSYNC_API.as_ref() {
                    (api.post_vsync_callback)(choreographer, on_vsync_callback, ptr::null_mut());
                } else if let Some(post64) = *POST_FRAME_CALLBACK_64 {
                    post64(choreographer, on_vsync_64, ptr::null_mut());
                } else {
                    ndk_sys::AChoreographer_postFrameCallback(
                        choreographer,
                        Some(on_vsync_legacy),
                        ptr::null_mut(),
                    );
                }
            }
        }
        // a wake from `POST_REQUESTED`.
        // SAFETY: called only from the thread that owns this looper.
        unsafe { ndk_sys::ALooper_pollOnce(-1, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
    }
}

/// Current `CLOCK_MONOTONIC` timestamp in nanoseconds.
pub(crate) fn monotonic_nanos_now() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-pointer for `CLOCK_MONOTONIC`.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0 {
        (ts.tv_sec as i64)
            .saturating_mul(1_000_000_000)
            .saturating_add(ts.tv_nsec as i64)
    } else {
        0
    }
}

/// Cached `(Instant, CLOCK_MONOTONIC ns)` anchor so consecutive hardware touch
/// and `AChoreographer` timestamps (including future `expectedPresentationTimeNanos`)
/// map onto the exact same epoch with zero per-sample syscall jitter.
static CLOCK_ANCHOR: parking_lot::Mutex<MonotonicClockAnchor> =
    parking_lot::Mutex::new(MonotonicClockAnchor::new());

/// Converts a `CLOCK_MONOTONIC` nanosecond timestamp (`AChoreographer` vsync or
/// `AMotionEvent` event time) to `std::time::Instant`.
pub(crate) fn monotonic_nanos_to_instant(nanos: i64) -> Instant {
    let now = Instant::now();
    CLOCK_ANCHOR
        .lock()
        .to_instant_with(nanos, now, || (Instant::now(), monotonic_nanos_now()))
}

/// Updates the active display refresh interval (from `Display.getRefreshRate()`).
pub(crate) fn set_refresh_interval(interval: Duration) {
    let ns = interval.as_nanos().clamp(2_000_000, 100_000_000) as u64;
    REFRESH_INTERVAL_NS.store(ns, Ordering::Relaxed);
    PACER.with(|slot| {
        if let Some(pacer) = slot.borrow().as_ref() {
            pacer.set_interval(Duration::from_nanos(ns));
        }
    });
}

/// Active display refresh interval in nanoseconds.
pub(crate) fn refresh_interval_ns() -> i64 {
    REFRESH_INTERVAL_NS.load(Ordering::Relaxed) as i64
}

/// TID of the `gpui-vsync` thread once started, for ADPF session binding.
pub(crate) fn vsync_thread_tid() -> Option<i32> {
    let tid = VSYNC_THREAD_TID.load(Ordering::Acquire);
    (tid > 0).then_some(tid)
}

/// Most recent hardware `AChoreographer` [`VsyncTimeline`], if at least one
/// vsync callback has fired.
pub(crate) fn last_timeline() -> Option<VsyncTimeline> {
    VSYNC_STATE.lock().latest
}

fn record_vsync_timeline(timeline: VsyncTimeline) {
    // Feed `expected_present_ns` to `FrameClock` and `tick_momentum_at` so
    // animations and post-fling momentum anchor to the actual display lighting
    // moment without triggering a false clock re-anchor.
    let signal_at = monotonic_nanos_to_instant(timeline.expected_present_ns);
    {
        let mut state = VSYNC_STATE.lock();
        state.latest = Some(timeline);
        state.pending_signal_at = Some(signal_at);
    }
    FRAME_DUE.store(true, Ordering::Release);
    wake_looper(&MAIN_LOOPER);
}

unsafe extern "C" fn on_vsync_callback(
    data: *const AChoreographerFrameCallbackData,
    _user_data: *mut c_void,
) {
    let mut refresh_ns = refresh_interval_ns();
    let timeline = if !data.is_null() {
        if let Some(api) = VSYNC_API.as_ref() {
            let frame_time_ns = unsafe { (api.get_frame_time_nanos)(data) };
            let len = unsafe { (api.get_frame_timelines_length)(data) };
            if len > 0 {
                let preferred = unsafe { (api.get_preferred_frame_timeline_index)(data) };
                let idx = if preferred < len { preferred } else { 0 };
                let expected_present_ns =
                    unsafe { (api.get_expected_presentation_time_nanos)(data, idx) };
                let deadline_ns = unsafe { (api.get_deadline_nanos)(data, idx) };
                if len >= 2 {
                    let next_idx = if idx + 1 < len { idx + 1 } else { idx - 1 };
                    let other_present_ns =
                        unsafe { (api.get_expected_presentation_time_nanos)(data, next_idx) };
                    let spacing = expected_present_ns.abs_diff(other_present_ns) as i64;
                    if let Some(inferred) =
                        crate::frame_timing::infer_refresh_interval_ns(refresh_ns, spacing)
                    {
                        REFRESH_INTERVAL_NS.store(inferred as u64, Ordering::Relaxed);
                        refresh_ns = inferred;
                        log::info!(
                            "frame_source: inferred live vsync interval {:.2} ms ({:.1} Hz)",
                            inferred as f64 / 1_000_000.0,
                            1_000_000_000.0 / inferred as f64,
                        );
                    }
                }
                VsyncTimeline::new(frame_time_ns, expected_present_ns, deadline_ns, refresh_ns)
            } else {
                VsyncTimeline::from_legacy_frame_time(frame_time_ns, refresh_ns)
            }
        } else {
            VsyncTimeline::from_legacy_frame_time(monotonic_nanos_now(), refresh_ns)
        }
    } else {
        VsyncTimeline::from_legacy_frame_time(monotonic_nanos_now(), refresh_ns)
    };
    record_vsync_timeline(timeline);
}

unsafe extern "C" fn on_vsync_64(frame_time_nanos: i64, _data: *mut c_void) {
    let timeline = VsyncTimeline::from_legacy_frame_time(frame_time_nanos, refresh_interval_ns());
    record_vsync_timeline(timeline);
}

unsafe extern "C" fn on_vsync_legacy(
    frame_time_nanos: std::os::raw::c_long,
    _data: *mut c_void,
) {
    let timeline =
        VsyncTimeline::from_legacy_frame_time(frame_time_nanos as i64, refresh_interval_ns());
    record_vsync_timeline(timeline);
}

/// Takes the most recent `AChoreographer` presentation-aligned instant, if one
/// fired since the last frame request.
pub(crate) fn take_signal_at() -> Option<Instant> {
    VSYNC_STATE.lock().pending_signal_at.take()
}
fn wake_looper(looper: &AtomicPtr<ndk_sys::ALooper>) {
    let looper = looper.load(Ordering::Acquire);
    if !looper.is_null() {
        // SAFETY: `ALooper_wake` is documented as safe to call from any thread.
        unsafe { ndk_sys::ALooper_wake(looper) };
    }
}

/// Runs `f` on the loop thread's pacer after handing it any vsync the vsync
/// thread reported since the last call.
fn with_pacer<R>(f: impl FnOnce(&FramePacer) -> R) -> Option<R> {
    PACER.with(|slot| {
        let slot = slot.borrow();
        let pacer = slot.as_ref()?;
        if FRAME_DUE.swap(false, Ordering::AcqRel) {
            pacer.on_vsync();
        }
        Some(f(pacer))
    })
}

/// Asks for a frame at the next vsync. On the loop thread this posts the
/// callback (once); from any other thread it records the demand and wakes the
/// loop so it can post.
pub(crate) fn schedule_frame() {
    if with_pacer(|pacer| pacer.schedule(Instant::now())).is_none() {
        OFF_THREAD_DEMAND.store(true, Ordering::Release);
        wake();
    }
}

/// Re-posts the vsync callback after a surface swap or a resume: a callback
/// posted before the app went to the background may never fire.
pub(crate) fn resume() {
    if with_pacer(|pacer| pacer.resume(Instant::now())).is_none() {
        schedule_frame();
    }
}

/// Whether the loop should tick GPUI now. Call once per loop iteration,
/// only while the window can draw; a frame left untaken stays due.
pub(crate) fn take_frame() -> bool {
    if OFF_THREAD_DEMAND.swap(false, Ordering::AcqRel) {
        schedule_frame();
    }
    with_pacer(|pacer| pacer.take_frame(Instant::now())).unwrap_or(false)
}

/// Whether a frame is currently posted, due, or requested from another thread.
pub(crate) fn has_demand() -> bool {
    if OFF_THREAD_DEMAND.load(Ordering::Acquire) || FRAME_DUE.load(Ordering::Acquire) {
        return true;
    }
    with_pacer(|pacer| pacer.has_demand()).unwrap_or(false)
}

/// Whether a vsync frame is already due right now (without consuming it).
pub(crate) fn has_due_frame() -> bool {
    if FRAME_DUE.load(Ordering::Acquire) {
        return true;
    }
    with_pacer(|pacer| pacer.is_due(Instant::now())).unwrap_or(false)
}

/// How long the loop may block on its looper: `Some(Duration::ZERO)` with a frame due, up to
/// the next delayed dispatcher task or the fallback frame otherwise, and `None` (wait
/// indefinitely until an event or `ALooper_wake` arrives) while the window is inactive with no
/// delayed tasks pending. `can_draw` is whether the loop would take a frame right now; while
/// the window is inactive a due frame stays owed but must not keep the loop awake.
pub(crate) fn poll_timeout(
    next_delayed_task: Option<Instant>,
    can_draw: bool,
) -> Option<Duration> {
    let now = Instant::now();
    let mut timeout: Option<Duration> = if can_draw { Some(IDLE_POLL_CAP) } else { None };
    if let Some(due) = next_delayed_task {
        let until = due.saturating_duration_since(now);
        timeout = Some(timeout.map_or(until, |cur| cur.min(until)));
    }
    if can_draw {
        if let Some(Some(frame)) = with_pacer(|pacer| pacer.poll_timeout(now)) {
            timeout = Some(timeout.map_or(frame, |cur| cur.min(frame)));
        }
    }
    // The loops hand this to `ALooper_pollOnce` in whole milliseconds; round
    // up so a sub-millisecond remainder does not become a zero-timeout spin.
    timeout.map(|dur| Duration::from_millis(dur.as_micros().div_ceil(1000) as u64))
}

/// Wakes the loop from any thread so it notices work that arrived outside
/// the looper's file descriptors (`TEXT_INPUT_DIRTY`, [`schedule_frame`]
/// from another thread). A no-op before [`install`].
pub(crate) fn wake() {
    wake_looper(&MAIN_LOOPER);
}
