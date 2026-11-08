//! Changes every Mailstream process on this Mac picks up, as the Swift engine's `ProcessSignal`
//! (`Sources/MailstreamKit/Store/ProcessSignal.swift`) sends them: the app, the front ends running
//! an engine in-process (GPUI, Flutter) and the app the CLI talks to share mail.db but hold what
//! they show in memory. A Darwin notification carries no data, so receivers read the change from
//! mail.db; the posting process hears its own posts too. Both engines post and hear the same
//! names, so a Swift and a Rust engine running at once follow each other.
//!
//! Also the system's own notifications of this kind, such as [`ProcessSignal::NETWORK_CHANGE`].
//!
//! Elsewhere (Windows) the front end is the only process with an engine (mail.db has no
//! multi-process WAL there, and the CLI goes through its control pipe), so posts reach this
//! process's own receivers only and system names never post.
use std::borrow::Cow;
use std::ffi::CString;

pub struct ProcessSignal {
    pub name: Cow<'static, str>,
}

impl ProcessSignal {
    /// AI tags written (the Swift engine's `MailCache.saveTags`, `clearTags`).
    pub const TAGS: Self = Self { name: Cow::Borrowed("dev.mailstream.Mailstream.tags") };
    /// configd's `kNotifySCNetworkChange`: the network configuration changed (an interface came
    /// up or went down, an address changed).
    pub const NETWORK_CHANGE: Self = Self { name: Cow::Borrowed("com.apple.system.config.network_change") };

    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self { name: name.into() }
    }

    pub fn post(&self) {
        let name = CString::new(self.name.as_bytes()).expect("signal names have no NUL");
        platform::post(&name);
    }

    /// Every post from now on, posts close together coalesced, until the value is dropped; `None`
    /// when the system refuses the registration. Call on a tokio runtime.
    pub fn posts(&self) -> Option<Posts> {
        let name = CString::new(self.name.as_bytes()).expect("signal names have no NUL");
        platform::register(&name).map(|inner| Posts { inner })
    }
}

/// Posts of one name, as they come.
pub struct Posts {
    inner: platform::Registration,
}

impl Posts {
    /// Returns at the next post, or at once for posts since the last call (however many, as
    /// one); `false` once no more can come.
    pub async fn next(&mut self) -> bool {
        self.inner.next().await
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, c_char, c_int};
    use std::os::fd::{AsRawFd, RawFd};

    use tokio::io::unix::AsyncFd;

    const NOTIFY_STATUS_OK: u32 = 0;

    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
        fn notify_register_file_descriptor(
            name: *const c_char,
            notify_fd: *mut c_int,
            flags: c_int,
            out_token: *mut c_int,
        ) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    pub fn post(name: &CStr) {
        // SAFETY: `name` is a valid C string for the call.
        unsafe { notify_post(name.as_ptr()) };
    }

    /// The descriptor notifyd writes each post's token to; `notify_cancel` closes it.
    struct Descriptor(RawFd);

    impl AsRawFd for Descriptor {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }

    pub struct Registration {
        /// Taken in `drop` so the descriptor leaves the runtime's poller before notifyd closes it.
        fd: Option<AsyncFd<Descriptor>>,
        token: c_int,
    }

    pub fn register(name: &CStr) -> Option<Registration> {
        let (mut fd, mut token): (c_int, c_int) = (-1, 0);
        // SAFETY: `name` is a valid C string and both out-pointers point at live locals.
        let status = unsafe { notify_register_file_descriptor(name.as_ptr(), &mut fd, 0, &mut token) };
        if status != NOTIFY_STATUS_OK {
            return None;
        }
        // SAFETY: `fd` is the descriptor notifyd just opened for this registration.
        let nonblocking = unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
        };
        let watched = nonblocking
            .then(|| {
                // SAFETY: `fd` stays open until `notify_cancel`, which `drop` calls only after
                // dropping the `AsyncFd`.
                unsafe { AsyncFd::register_with_interest(Descriptor(fd), tokio::io::Interest::READABLE) }.ok()
            })
            .flatten();
        let Some(watched) = watched else {
            // SAFETY: the token is this registration's; cancelling closes `fd`.
            unsafe { notify_cancel(token) };
            return None;
        };
        Some(Registration { fd: Some(watched), token })
    }

    impl Registration {
        pub async fn next(&mut self) -> bool {
            let Some(fd) = &self.fd else { return false };
            loop {
                let Ok(mut ready) = fd.readable().await else { return false };
                // Each post writes a four-byte token; read them all, as one post.
                let mut heard = false;
                let mut buffer = [0u8; 256];
                loop {
                    // SAFETY: reads into a live buffer of its length from an open descriptor.
                    let count = unsafe { libc::read(fd.get_ref().0, buffer.as_mut_ptr().cast(), buffer.len()) };
                    if count > 0 {
                        heard = true;
                        continue;
                    }
                    if count == 0 {
                        return false;
                    }
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                        ready.clear_ready();
                        break;
                    }
                    return false;
                }
                if heard {
                    return true;
                }
            }
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            drop(self.fd.take());
            // SAFETY: the token is this registration's; cancelling closes its descriptor.
            unsafe { notify_cancel(self.token) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::collections::HashMap;
    use std::ffi::{CStr, CString};
    use std::sync::{Arc, LazyLock, Mutex, Weak};

    use tokio::sync::Notify;

    /// The receivers registered for each name.
    static RECEIVERS: LazyLock<Mutex<HashMap<CString, Vec<Weak<Notify>>>>> = LazyLock::new(Default::default);

    pub fn post(name: &CStr) {
        let mut receivers = RECEIVERS.lock().expect("signal receivers");
        if let Some(list) = receivers.get_mut(name) {
            // A receiver not waiting keeps one wake: posts since its last call read as one.
            list.retain(|receiver| receiver.upgrade().inspect(|receiver| receiver.notify_one()).is_some());
        }
    }

    pub struct Registration(Arc<Notify>);

    pub fn register(name: &CStr) -> Option<Registration> {
        let receiver = Arc::new(Notify::new());
        let mut receivers = RECEIVERS.lock().expect("signal receivers");
        let list = receivers.entry(name.to_owned()).or_default();
        list.retain(|receiver| receiver.strong_count() > 0);
        list.push(Arc::downgrade(&receiver));
        Some(Registration(receiver))
    }

    impl Registration {
        pub async fn next(&mut self) -> bool {
            self.0.notified().await;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn hears_posts_from_this_process() {
        let signal =
            ProcessSignal::new(format!("dev.mailstream.test.signal.{}.{}", std::process::id(), crate::tests::unique()));
        let mut posts = signal.posts().expect("the name registers");
        signal.post();
        signal.post();
        signal.post();
        assert!(tokio::time::timeout(Duration::from_secs(5), posts.next()).await.unwrap());
        // Posts are delivered as they reach the receiver; those already waiting are read as one.
        let mut more = 0;
        while tokio::time::timeout(Duration::from_millis(300), posts.next()).await.is_ok() {
            more += 1;
        }
        assert!(more <= 2, "{more} more wakes for three posts");
        signal.post();
        assert!(tokio::time::timeout(Duration::from_secs(5), posts.next()).await.unwrap());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn hears_posts_from_another_process() {
        // What the Swift engine's `notify_post` does, from a process of its own.
        let name = format!("dev.mailstream.test.signal.{}.{}", std::process::id(), crate::tests::unique());
        let mut posts = ProcessSignal::new(name.clone()).posts().expect("notifyd registers the name");
        let status = std::process::Command::new("/usr/bin/notifyutil").args(["-p", &name]).status().unwrap();
        assert!(status.success());
        assert!(tokio::time::timeout(Duration::from_secs(5), posts.next()).await.unwrap());
    }
}
