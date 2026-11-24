//! Android task dispatcher.
//!
//! Mirrors the two-queue model used in `gpui_linux` and `IosDispatcher`:
//!
//! * **Foreground / main-thread tasks** — posted via a pipe-backed `ALooper`
//!   callback so they run on the Android native-activity main thread (the same
//!   thread that owns the `ANativeWindow` and processes input events).
//!
//! * **Background tasks** — dispatched onto a fixed-size Rust thread-pool
//!   backed by `std::thread`.  The pool size defaults to
//!   `std::thread::available_parallelism()`.
//!
//! ## Design notes
//!
//! `ALooper` (NDK) is the native equivalent of GCD's main queue or a Linux
//! `epoll` loop.  We register a pipe file-descriptor with `ALooper_addFd` and
//! write a single byte to wake it up whenever a foreground task is enqueued.
//! The looper callback drains all pending tasks before returning.
//!
//! ## GPUI integration
//!
//! `AndroidDispatcher` implements `gpui::PlatformDispatcher` so it can be
//! used to construct `BackgroundExecutor` and `ForegroundExecutor` instances.

#![allow(unsafe_code)]

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use gpui::{PlatformDispatcher, Priority, RunnableVariant};
use parking_lot::Mutex;

// ── NDK symbols we need ───────────────────────────────────────────────────────

unsafe extern "C" {
    /// Returns the looper associated with the calling thread, or null.
    fn ALooper_forThread() -> *mut libc_looper_opaque;
    /// Returns from a blocking poll on the looper; safe from any thread.
    fn ALooper_wake(looper: *mut libc_looper_opaque);
}
// Opaque C type placeholder.
#[repr(C)]
struct libc_looper_opaque {
    _priv: [u8; 0],
}

// ── task queue ────────────────────────────────────────────────────────────────

type BoxedTask = Box<dyn FnOnce() + Send + 'static>;

/// Shared main-thread task queue drained by the render loop.
///
/// Wakes the main looper with `ALooper_wake` instead of registering a pipe fd
/// callback on `MAIN_LOOPER`: `android-activity` logs `Spurious
/// ALOOPER_POLL_CALLBACK` on every foreign callback fd and runs callbacks
/// re-entrantly inside `poll_events` ahead of input/vsync delivery (MS-470).
struct MainQueue {
    tasks: VecDeque<BoxedTask>,
}
// ── thread-pool ───────────────────────────────────────────────────────────────

/// A minimal fixed-size thread-pool for background tasks.
struct ThreadPool {
    sender: std::sync::mpsc::Sender<BoxedTask>,
}

impl ThreadPool {
    fn new(threads: usize) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<BoxedTask>();
        let receiver = Arc::new(Mutex::new(receiver));

        for i in 0..threads {
            let rx = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("gpui-bg-{}", i))
                .spawn(move || {
                    loop {
                        let task = {
                            let lock = rx.lock();
                            lock.recv()
                        };
                        match task {
                            Ok(f) => f(),
                            Err(_) => break, // channel closed
                        }
                    }
                })
                .expect("failed to spawn background thread");
        }

        ThreadPool { sender }
    }

    fn dispatch(&self, task: BoxedTask) {
        // If the pool is shutting down the send will fail silently.
        let _ = self.sender.send(task);
    }
}

// ── delayed task queue ────────────────────────────────────────────────────────

struct DelayedTask {
    due: Instant,
    task: BoxedTask,
}

// ── AndroidDispatcher ─────────────────────────────────────────────────────────

/// GPUI dispatcher for Android.
///
/// * Foreground tasks run on the Android main/native thread via `ALooper`.
/// * Background tasks run on a Rust thread-pool.
/// * Delayed tasks are checked on each `tick()` call (driven by the main loop).
pub struct AndroidDispatcher {
    /// Shared task queue for the main thread.
    main_queue: Arc<Mutex<MainQueue>>,
    /// Raw pointer to the main-thread `ALooper` (used only for `is_main_thread`
    /// and thread-safe `ALooper_wake`).
    looper: *mut libc_looper_opaque,
    /// Background thread-pool.
    pool: ThreadPool,
    /// Delayed background tasks sorted by due time.
    delayed: Mutex<Vec<DelayedTask>>,
    /// Set to `true` once `shutdown()` is called.
    shutdown: AtomicBool,
}

// SAFETY: The `looper` pointer is only ever used on the main thread
// (in `register_with_looper` and `unregister`), except for `ALooper_wake`,
// which is documented as safe from any thread.  The rest of the fields are
// `Send`-safe via `Mutex` / `Arc`.
unsafe impl Send for AndroidDispatcher {}
unsafe impl Sync for AndroidDispatcher {}

impl AndroidDispatcher {
    /// Create a new dispatcher.
    ///
    /// Must be called on the **main thread** so that `ALooper_forThread()`
    /// returns a valid looper.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// * `pipe(2)` fails.
    /// * `ALooper_forThread()` returns null (i.e. not called on main thread).
    pub fn new() -> Arc<Self> {
        let looper = unsafe { ALooper_forThread() };
        assert!(
            !looper.is_null(),
            "AndroidDispatcher::new() must be called on the Android main thread"
        );

        // Cap GPUI's background pool on mobile so an 8-core phone does not spawn 8 mostly-idle
        // worker threads alongside the engine's tokio runtime (MS-441).
        let pool_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 4);

        let main_queue = Arc::new(Mutex::new(MainQueue {
            tasks: VecDeque::new(),
        }));

        let dispatcher = Arc::new(Self {
            main_queue,
            looper,
            pool: ThreadPool::new(pool_threads),
            delayed: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        });

        log::debug!("AndroidDispatcher created (pool_threads={})", pool_threads);

        dispatcher
    }

    /// Create a dispatcher without a real `ALooper`.
    ///
    /// Safe to call from any thread (including non-Android host environments
    /// and unit tests).  Foreground tasks accumulate in the queue and must
    /// be drained manually via `flush_main_thread_tasks()`.
    pub fn new_headless() -> Arc<Self> {
        let pool_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);

        let main_queue = Arc::new(Mutex::new(MainQueue {
            tasks: VecDeque::new(),
        }));

        Arc::new(Self {
            main_queue,
            looper: std::ptr::null_mut(), // no real looper in headless mode
            pool: ThreadPool::new(pool_threads),
            delayed: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        })
    }

    // ── public API ────────────────────────────────────────────────────────────

    /// Returns `true` if the calling thread is the main/UI thread.
    pub fn is_main_thread(&self) -> bool {
        // On Android the main thread is the one that owns the looper we
        // registered with.  `ALooper_forThread()` returns the *same* pointer
        // if we're on that thread, or a *different* (or null) pointer otherwise.
        let current = unsafe { ALooper_forThread() };
        !current.is_null() && current == self.looper
    }

    /// Enqueue a task to run on the **main** (foreground) thread.
    pub fn dispatch_on_main_thread<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut q = self.main_queue.lock();
            q.tasks.push_back(Box::new(f));
        }
        self.wake_looper();
    }

    /// Enqueue a task on the **background** thread-pool.
    pub fn dispatch<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        self.pool.dispatch(Box::new(f));
    }

    /// Enqueue a task on the **background** thread-pool after `delay`.
    pub fn dispatch_after<F>(&self, delay: Duration, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let due = Instant::now() + delay;
        let mut delayed = self.delayed.lock();
        delayed.push(DelayedTask {
            due,
            task: Box::new(f),
        });
        // Keep sorted by ascending due time.
        delayed.sort_by_key(|d| d.due);
        drop(delayed);
        self.wake_looper();
    }

    /// When the earliest delayed task is due, so the main loop can sleep
    /// until then instead of polling `tick()` blindly.
    pub fn next_delayed_due(&self) -> Option<Instant> {
        self.delayed.lock().first().map(|d| d.due)
    }

    /// Interrupts a main loop blocked on the looper so it re-reads
    /// `next_delayed_due()`; a delayed task posted from another thread would
    /// otherwise wait for an unrelated wakeup.
    fn wake_looper(&self) {
        if !self.looper.is_null() {
            // SAFETY: `ALooper_wake` is documented as safe from any thread,
            // and the looper outlives the dispatcher's thread.
            unsafe { ALooper_wake(self.looper) };
        }
    }

    /// Process any delayed background tasks whose due time has passed.
    ///
    /// Should be called from the main loop on every iteration (e.g. just
    /// before calling `ALooper_pollOnce`).
    pub fn tick(&self) {
        let now = Instant::now();
        let mut ready: Vec<BoxedTask> = Vec::new();
        {
            let mut delayed = self.delayed.lock();
            while delayed.first().map(|d| d.due <= now).unwrap_or(false) {
                ready.push(delayed.remove(0).task);
            }
        }
        for task in ready {
            self.pool.dispatch(task);
        }
    }

    /// Whether at least one main-thread task is waiting to run.
    pub fn has_pending_main_tasks(&self) -> bool {
        !self.main_queue.lock().tasks.is_empty()
    }

    /// Drain all pending **main-thread** tasks synchronously.
    pub fn flush_main_thread_tasks(&self) {
        loop {
            let task = {
                let mut q = self.main_queue.lock();
                q.tasks.pop_front()
            };
            match task {
                Some(f) => f(),
                None => break,
            }
        }
    }

    /// Drain pending main-thread tasks up to `budget` so a burst of engine/tile
    /// callbacks arriving right at vsync cannot blow an 8.33 ms frame deadline
    /// (MS-470). Returns `true` if more tasks remain queued (and wakes the
    /// looper so they run immediately after the frame presents).
    pub fn flush_main_thread_tasks_budgeted(&self, budget: Duration) -> bool {
        let start = Instant::now();
        let mut ran = 0usize;
        loop {
            let (task, more) = {
                let mut q = self.main_queue.lock();
                let task = q.tasks.pop_front();
                let more = !q.tasks.is_empty();
                (task, more)
            };
            let Some(f) = task else {
                return false;
            };
            f();
            ran += 1;
            if !more {
                return false;
            }
            if ran > 0 && start.elapsed() >= budget {
                self.wake_looper();
                return true;
            }
        }
    }

    /// Stop accepting new tasks and signal the thread-pool to wind down.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    // (No private ALooper fd registration — `wake_looper()` signals `ALooper_wake` directly.)
}

impl Default for AndroidDispatcher {
    fn default() -> Self {
        panic!(
            "AndroidDispatcher must be constructed via `AndroidDispatcher::new()` \
             on the main thread"
        );
    }
}

impl Drop for AndroidDispatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}
// ── impl PlatformDispatcher ───────────────────────────────────────────────────

impl PlatformDispatcher for AndroidDispatcher {
    fn is_main_thread(&self) -> bool {
        // Delegate to the existing `is_main_thread` method.
        AndroidDispatcher::is_main_thread(self)
    }

    fn dispatch(&self, runnable: RunnableVariant, _priority: Priority) {
        // All non-realtime background tasks go to the thread-pool.
        // Priority-based scheduling is not yet implemented; all tasks are
        // treated equally by the pool.
        self.pool.dispatch(Box::new(move || {
            runnable.run();
        }));
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut q = self.main_queue.lock();
            q.tasks.push_back(Box::new(move || {
                runnable.run();
            }));
        }
        self.wake_looper();
    }
    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let due = Instant::now() + duration;
        let mut delayed = self.delayed.lock();
        delayed.push(DelayedTask {
            due,
            task: Box::new(move || {
                runnable.run();
            }),
        });
        delayed.sort_by_key(|d| d.due);
        drop(delayed);
        self.wake_looper();
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        // Spawn a dedicated thread for realtime (audio) tasks, matching the
        // behaviour of the Linux dispatcher.
        std::thread::Builder::new()
            .name("gpui-realtime".to_string())
            .spawn(move || {
                f();
            })
            .expect("failed to spawn realtime thread");
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    /// Verify that the thread-pool executes background tasks.
    #[test]
    fn background_tasks_run() {
        let pool = ThreadPool::new(2);
        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..10 {
            let c = Arc::clone(&counter);
            pool.dispatch(Box::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }));
        }

        // Give threads time to process.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(counter.load(Ordering::Relaxed), 10);
    }

    /// Verify that delayed tasks are not dispatched before their due time.
    #[test]
    fn delayed_tasks_not_early() {
        let dispatcher = AndroidDispatcher::new_headless();

        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);

        dispatcher.dispatch_after(Duration::from_secs(60), move || {
            ran2.store(true, Ordering::Relaxed);
        });

        dispatcher.tick();
        assert!(!ran.load(Ordering::Relaxed), "task should not run yet");
    }

    /// Verify budgeted flush yields back to rendering when a task burst exceeds budget.
    #[test]
    fn budgeted_flush_defers_excess_tasks() {
        let dispatcher = AndroidDispatcher::new_headless();
        let count = Arc::new(AtomicUsize::new(0));

        for _ in 0..5 {
            let c = Arc::clone(&count);
            dispatcher.dispatch_on_main_thread(move || {
                std::thread::sleep(Duration::from_millis(2));
                c.fetch_add(1, Ordering::Relaxed);
            });
        }

        assert!(dispatcher.has_pending_main_tasks());
        let remaining = dispatcher.flush_main_thread_tasks_budgeted(Duration::from_millis(1));
        assert!(remaining, "should pause after hitting the budget");
        let ran_first = count.load(Ordering::Relaxed);
        assert!((1..5).contains(&ran_first));

        dispatcher.flush_main_thread_tasks();
        assert_eq!(count.load(Ordering::Relaxed), 5);
        assert!(!dispatcher.has_pending_main_tasks());
    }
}
