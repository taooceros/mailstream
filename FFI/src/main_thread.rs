//! The main thread's queue, where listeners are called: uniffi-dart's callbacks only run on the
//! Dart isolate's thread, which is Flutter's main (platform) thread.
//!
//! On Apple platforms that is libdispatch's main queue. On Windows the thread that subscribes
//! first (Flutter's platform thread, or a tool's main thread before [`run_loop`]) gets a
//! message-only window; work is posted to it and runs when that thread's message loop dispatches
//! the message.
pub use imp::{adopt_current_thread, run, run_loop};

#[cfg(not(windows))]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    struct DispatchQueue {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        /// libdispatch's main queue, which `dispatch_get_main_queue()` returns.
        static _dispatch_main_q: DispatchQueue;
        fn dispatch_async_f(queue: *const DispatchQueue, context: *mut c_void, work: extern "C" fn(*mut c_void));
        fn dispatch_main() -> !;
    }

    /// The main queue is the main thread's already.
    pub fn adopt_current_thread() {}

    /// Runs `work` on the main thread.
    pub fn run(work: impl FnOnce() + Send + 'static) {
        extern "C" fn call(context: *mut c_void) {
            let work = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
            work();
        }
        let work: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(work));
        unsafe { dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(work).cast(), call) };
    }

    /// Runs the main queue on this (the main) thread; never returns.
    pub fn run_loop() -> ! {
        unsafe { dispatch_main() }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, PostMessageW,
        RegisterClassExW, TranslateMessage, WM_APP, WNDCLASSEXW,
    };

    type Work = Box<dyn FnOnce() + Send>;

    /// The message carrying one `Box<Work>` in its `lparam`.
    const WORK: u32 = WM_APP + 0x4d53;

    /// The message-only window of the thread that subscribed first, as an address.
    static WINDOW: OnceLock<usize> = OnceLock::new();

    /// Makes this thread the one listeners are called on, unless one already is.
    pub fn adopt_current_thread() {
        WINDOW.get_or_init(create);
    }

    /// Runs `work` on the adopted thread; dropped when no thread was adopted.
    pub fn run(work: impl FnOnce() + Send + 'static) {
        let Some(&window) = WINDOW.get().filter(|window| **window != 0) else { return };
        let work: *mut Work = Box::into_raw(Box::new(Box::new(work)));
        // SAFETY: the window lives as long as the process; a message that is not posted gives
        // the work back.
        if unsafe { PostMessageW(window as HWND, WORK, 0, work as LPARAM) } == 0 {
            drop(unsafe { Box::from_raw(work) });
        }
    }

    /// Adopts this thread and runs its message loop; never returns.
    pub fn run_loop() -> ! {
        adopt_current_thread();
        // SAFETY: a zeroed MSG is a valid out-parameter.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::process::exit(0)
    }

    unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if message == WORK {
            // SAFETY: `run` posted this pointer from `Box::into_raw`, and each message is
            // dispatched once.
            let work = unsafe { Box::from_raw(lparam as *mut Work) };
            work();
            return 0;
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    /// A message-only window of this thread, as an address; 0 when Windows refuses one.
    fn create() -> usize {
        let class: Vec<u16> = "MailstreamMainThread".encode_utf16().chain([0]).collect();
        // SAFETY: the class name outlives both calls; the window procedure is a plain function
        // of this library, which stays loaded.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut description: WNDCLASSEXW = std::mem::zeroed();
            description.cbSize = size_of::<WNDCLASSEXW>() as u32;
            description.lpfnWndProc = Some(procedure);
            description.hInstance = instance;
            description.lpszClassName = class.as_ptr();
            RegisterClassExW(&description);
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            window as usize
        }
    }
}
