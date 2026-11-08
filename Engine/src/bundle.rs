//! Build-time configuration of the host app, never committed: on macOS its Info.plist
//! (scripts/bundle.sh writes it into Mailstream.app), as the Swift engine's `AppConfig` reads it;
//! elsewhere `config.json` beside the executable, one object with the same keys and string values.
use crate::oauth::{BuiltInClients, OAuthClient, Provider};
use crate::push::PushConfig;

/// The host's item `key`; missing keys and empty strings read as absent.
fn value(key: &str) -> Option<String> {
    let text = platform::value(key)?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The OAuth clients the host carries (`MSGoogleClientID`/`Secret`,
/// `MSMicrosoftClientID`/`Secret`).
pub fn built_in_clients() -> BuiltInClients {
    let mut clients = BuiltInClients::new();
    for (provider, id, secret) in [
        (Provider::Google, "MSGoogleClientID", "MSGoogleClientSecret"),
        (Provider::Microsoft, "MSMicrosoftClientID", "MSMicrosoftClientSecret"),
    ] {
        if let Some(id) = value(id) {
            clients.insert(provider, OAuthClient { id, secret: value(secret) });
        }
    }
    clients
}

/// The push relay the host names (`MSPushURL`, `MSPushToken`, `MSGraphClientState`,
/// `MSGmailTopic`); without the first two there is no push.
pub fn push_config() -> PushConfig {
    PushConfig {
        url: value("MSPushURL"),
        token: value("MSPushToken"),
        graph_client_state: value("MSGraphClientState"),
        gmail_topic: value("MSGmailTopic"),
        timing: Default::default(),
    }
}

/// The host app's version: the main bundle's `CFBundleShortVersionString` on macOS, the
/// executable's version resource on Windows; None outside an app (tools, tests).
pub fn short_version() -> Option<String> {
    platform::short_version()
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::bundle::CFBundle;
    use core_foundation::string::CFString;

    /// The main bundle's Info.plist item `key`.
    pub fn value(key: &str) -> Option<String> {
        let info = CFBundle::main_bundle().info_dictionary();
        Some(info.find(CFString::new(key))?.downcast::<CFString>()?.to_string())
    }

    pub fn short_version() -> Option<String> {
        super::value("CFBundleShortVersionString")
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::LazyLock;

    use serde_json::{Map, Value};

    /// `config.json` beside the executable; empty when there is none or it is not an object.
    static CONFIG: LazyLock<Map<String, Value>> = LazyLock::new(|| {
        let file = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("config.json")));
        file.and_then(|file| std::fs::read(file).ok())
            .and_then(|data| serde_json::from_slice::<Map<String, Value>>(&data).ok())
            .unwrap_or_default()
    });

    pub fn value(key: &str) -> Option<String> {
        CONFIG.get(key)?.as_str().map(String::from)
    }

    /// `major.minor.patch` of the executable's version resource (Flutter's runner takes it from
    /// pubspec.yaml).
    #[cfg(windows)]
    pub fn short_version() -> Option<String> {
        use std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
        };

        let exe = std::env::current_exe().ok()?;
        let name: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: `name` is NUL-terminated; the buffer is as long as the size asked for, and the
        // fixed info `VerQueryValueW` points at lives inside it.
        unsafe {
            let size = GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut());
            if size == 0 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            if GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
                return None;
            }
            let root: [u16; 2] = [u16::from(b'\\'), 0];
            let mut info: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut length = 0u32;
            if VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut length) == 0
                || info.is_null()
                || (length as usize) < size_of::<VS_FIXEDFILEINFO>()
            {
                return None;
            }
            let info = &*info.cast::<VS_FIXEDFILEINFO>();
            let (major, minor) = (info.dwProductVersionMS >> 16, info.dwProductVersionMS & 0xffff);
            let patch = info.dwProductVersionLS >> 16;
            (info.dwProductVersionMS != 0 || info.dwProductVersionLS != 0).then(|| format!("{major}.{minor}.{patch}"))
        }
    }

    #[cfg(not(windows))]
    pub fn short_version() -> Option<String> {
        None
    }
}
