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
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU64, Ordering},
        mpsc, OnceLock,
    },
    time::{Duration, Instant},
};

use crate::frame_pacer::FramePacer;

/// Longest the loop blocks with nothing due; a missed wakeup costs at most
/// this much latency instead of a frozen app.
const IDLE_POLL_CAP: Duration = Duration::from_secs(1);

/// Default refresh interval before the display's real refresh rate is queried.
const DEFAULT_REFRESH_INTERVAL_NS: u64 = 16_666_667;

/// Active display refresh interval in nanoseconds (updated from `Display.getRefreshRate()`).
static REFRESH_INTERVAL_NS: AtomicU64 = AtomicU64::new(DEFAULT_REFRESH_INTERVAL_NS);

/// Most recent hardware `AChoreographer` vsync timestamp in `CLOCK_MONOTONIC` ns.
static LAST_VSYNC_NS: AtomicI64 = AtomicI64::new(0);
thread_local! {
    /// The loop thread's pacer, installed by [`install`].
    static PACER: RefCell<Option<FramePacer>> = const { RefCell::new(None) };
}

/// The loop thread's `ALooper`, for [`wake`] from other threads.
static MAIN_LOOPER: AtomicPtr<ndk_sys::ALooper> = AtomicPtr::new(ptr::null_mut());

/// The vsync thread's `ALooper`; null while it has no choreographer.
static VSYNC_LOOPER: AtomicPtr<ndk_sys::ALooper> = AtomicPtr::new(ptr::null_mut());

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

/// Hardware vsync instant captured when `AChoreographer` last fired, passed
/// through `RequestFrameOptions::signal_at` for jitter-free animation clocks.
static LAST_VSYNC_AT: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);

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
    VSYNC_LOOPER.store(looper, Ordering::Release);
    let _ = ready.send(true);

    loop {
        if POST_REQUESTED.swap(false, Ordering::AcqRel) {
            // SAFETY: the choreographer belongs to this thread and lives as
            // long as it; the callback takes no data pointer.
            unsafe {
                ndk_sys::AChoreographer_postFrameCallback(
                    choreographer,
                    Some(on_vsync),
                    ptr::null_mut(),
                )
            };
        }
        // Returns on the frame callback (delivered through this looper) and on
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
/// and `AChoreographer` timestamps map onto the exact same epoch with zero
/// per-sample syscall jitter.
static CLOCK_ANCHOR: parking_lot::Mutex<Option<(Instant, i64)>> = parking_lot::Mutex::new(None);

/// Converts a `CLOCK_MONOTONIC` nanosecond timestamp (`AChoreographer` vsync or
/// `AMotionEvent` event time) to `std::time::Instant`.
pub(crate) fn monotonic_nanos_to_instant(nanos: i64) -> Instant {
    if nanos <= 0 {
        return Instant::now();
    }
    let (anchor_instant, anchor_ns) = {
        let mut guard = CLOCK_ANCHOR.lock();
        match *guard {
            Some(anchor) => anchor,
            None => {
                let now = Instant::now();
                let now_ns = monotonic_nanos_now();
                if now_ns <= 0 {
                    return now;
                }
                *guard = Some((now, now_ns));
                (now, now_ns)
            }
        }
    };
    let elapsed_ns = anchor_instant.elapsed().as_nanos() as i128;
    let now_ns_est = (anchor_ns as i128) + elapsed_ns;
    let age_ns = now_ns_est - (nanos as i128);
    if !(-100_000_000..=500_000_000).contains(&age_ns) {
        // Re-anchor once in case the process resumed after a long suspend or
        // clock shift, then validate again.
        let now = Instant::now();
        let now_ns = monotonic_nanos_now();
        if now_ns <= 0 {
            return now;
        }
        *CLOCK_ANCHOR.lock() = Some((now, now_ns));
        let delta_ns = (now_ns as i128) - (nanos as i128);
        if (0..=500_000_000).contains(&delta_ns) {
            return now
                .checked_sub(Duration::from_nanos(delta_ns as u64))
                .unwrap_or(now);
        } else if (-100_000_000..0).contains(&delta_ns) {
            return now
                .checked_add(Duration::from_nanos((-delta_ns) as u64))
                .unwrap_or(now);
        }
        return now;
    }
    let offset_ns = (nanos as i128) - (anchor_ns as i128);
    if offset_ns >= 0 {
        anchor_instant
            .checked_add(Duration::from_nanos(offset_ns as u64))
            .unwrap_or_else(Instant::now)
    } else {
        anchor_instant
            .checked_sub(Duration::from_nanos((-offset_ns) as u64))
            .unwrap_or_else(Instant::now)
    }
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

/// Most recent hardware `AChoreographer` vsync timestamp in `CLOCK_MONOTONIC` ns
/// (or `0` before the first vsync callback).
pub(crate) fn last_vsync_ns() -> i64 {
    LAST_VSYNC_NS.load(Ordering::Acquire)
}

unsafe extern "C" fn on_vsync(
    frame_time_nanos: std::os::raw::c_long,
    _data: *mut std::ffi::c_void,
) {
    let ns = frame_time_nanos as i64;
    LAST_VSYNC_NS.store(ns, Ordering::Release);
    *LAST_VSYNC_AT.lock() = Some(monotonic_nanos_to_instant(ns));
    FRAME_DUE.store(true, Ordering::Release);
    wake_looper(&MAIN_LOOPER);
}

/// Takes the most recent `AChoreographer` vsync instant, if one fired since
/// the last frame request.
pub(crate) fn take_signal_at() -> Option<Instant> {
    LAST_VSYNC_AT.lock().take()
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
