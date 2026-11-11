//! Sign-ins in the login keychain, shared with the Swift engine: one generic password (service
//! `dev.mailstream.Mailstream.oauth`, account `refresh-tokens`, label `Mailstream sign-ins`)
//! holding [`SignIns`] as key-sorted JSON, so macOS asks about Mailstream's access once rather
//! than per account. Items from before clients were stored hold a bare object of refresh token
//! by account id.
//!
//! Windows has no such item: the same JSON is one file, `<service>.signins` in
//! `%LOCALAPPDATA%\Mailstream`, sealed with DPAPI for the signed-in Windows user (Credential
//! Manager caps an item at 2.5 KB, less than a few Microsoft refresh tokens).
//!
//! Linux keeps the same file, `<service>.signins` in `~/.local/share/Mailstream`, readable by the
//! user alone (mode 0600): many desktops run no Secret Service to hold it.
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::oauth::OAuthClient;

/// The service the apps' sign-ins live under. Tests use another one: an unsigned test binary
/// reading this item makes macOS ask the person for access.
pub const SERVICE: &str = "dev.mailstream.Mailstream.oauth";

/// What the item keeps, by account id and provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignIns {
    pub refresh_tokens: BTreeMap<String, String>,
    /// The client each account's refresh token was issued to.
    pub clients: BTreeMap<String, OAuthClient>,
    /// The client last signed in with, by provider (`google`, `microsoft`), offered next time.
    pub last_clients: BTreeMap<String, OAuthClient>,
}

impl SignIns {
    /// The item's contents: the current format, else the old bare object, else nothing.
    pub fn decode(data: &[u8]) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Current {
            refresh_tokens: BTreeMap<String, String>,
            clients: BTreeMap<String, OAuthClient>,
            last_clients: BTreeMap<String, OAuthClient>,
        }
        if let Ok(current) = serde_json::from_slice::<Current>(data) {
            return Self {
                refresh_tokens: current.refresh_tokens,
                clients: current.clients,
                last_clients: current.last_clients,
            };
        }
        let legacy = serde_json::from_slice::<BTreeMap<String, String>>(data).unwrap_or_default();
        Self { refresh_tokens: legacy, ..Self::default() }
    }

    /// Compact, key-sorted, slashes escaped: what the Swift engine's encoder writes.
    pub fn encode(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("sign-ins encode");
        crate::json::encode(&value, false, true)
    }
}

/// The sign-ins item under one service.
#[derive(Clone, Debug)]
pub struct Keychain {
    service: String,
}

#[cfg(target_os = "macos")]
pub use apple::KeychainError;
#[cfg(windows)]
pub use sealed::KeychainError;
#[cfg(all(unix, not(target_os = "macos")))]
pub use private_file::KeychainError;

#[cfg(target_os = "macos")]
mod apple {
    // Security's status constants (`errSecSuccess`) are matched by their C names.
    #![allow(non_upper_case_globals)]
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFTypeRef, OSStatus};
    use security_framework_sys::base::{SecCopyErrorMessageString, errSecItemNotFound, errSecSuccess};
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecReturnData,
        kSecValueData,
    };
    use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate};

    use super::*;

    const ACCOUNT: &str = "refresh-tokens";
    const LABEL: &str = "Mailstream sign-ins";

    /// A failed Keychain call.
    #[derive(Clone, Debug)]
    pub struct KeychainError(OSStatus);

    impl fmt::Display for KeychainError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let reason = unsafe {
                let message = SecCopyErrorMessageString(self.0, std::ptr::null_mut());
                if message.is_null() { None } else { Some(CFString::wrap_under_create_rule(message).to_string()) }
            };
            match reason {
                Some(reason) => write!(formatter, "Keychain: {reason}"),
                None => write!(formatter, "Keychain: OSStatus {}", self.0),
            }
        }
    }

    impl Keychain {
        pub fn new(service: impl Into<String>) -> Self {
            Self { service: service.into() }
        }

        /// The stored sign-ins; empty when there are none.
        pub fn load(&self) -> Result<SignIns, KeychainError> {
            let mut query = self.base();
            unsafe {
                query.push((CFString::wrap_under_get_rule(kSecReturnData), CFBoolean::true_value().into_CFType()));
            }
            let query = CFDictionary::from_CFType_pairs(&query);
            let mut result: CFTypeRef = std::ptr::null();
            let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
            match status {
                errSecSuccess => {
                    if result.is_null() {
                        return Ok(SignIns::default());
                    }
                    let value = unsafe { CFType::wrap_under_create_rule(result) };
                    Ok(value.downcast::<CFData>().map(|data| SignIns::decode(data.bytes())).unwrap_or_default())
                }
                errSecItemNotFound => Ok(SignIns::default()),
                status => Err(KeychainError(status)),
            }
        }

        /// Replaces the stored sign-ins; with no refresh token and no remembered client left the item
        /// is deleted.
        pub fn store(&self, sign_ins: &SignIns) -> Result<(), KeychainError> {
            let base = CFDictionary::from_CFType_pairs(&self.base());
            if sign_ins.refresh_tokens.is_empty() && sign_ins.last_clients.is_empty() {
                let deleted = unsafe { SecItemDelete(base.as_concrete_TypeRef()) };
                return match deleted {
                    errSecSuccess | errSecItemNotFound => Ok(()),
                    status => Err(KeychainError(status)),
                };
            }
            let data = CFData::from_buffer(&sign_ins.encode());
            let value_key = unsafe { CFString::wrap_under_get_rule(kSecValueData) };
            let update = CFDictionary::from_CFType_pairs(&[(value_key.clone(), data.clone().into_CFType())]);
            match unsafe { SecItemUpdate(base.as_concrete_TypeRef(), update.as_concrete_TypeRef()) } {
                errSecSuccess => Ok(()),
                errSecItemNotFound => {
                    let mut item = self.base();
                    item.push((value_key, data.into_CFType()));
                    item.push((
                        unsafe { CFString::wrap_under_get_rule(kSecAttrLabel) },
                        CFString::new(LABEL).into_CFType(),
                    ));
                    let item = CFDictionary::from_CFType_pairs(&item);
                    match unsafe { SecItemAdd(item.as_concrete_TypeRef(), std::ptr::null_mut()) } {
                        errSecSuccess => Ok(()),
                        status => Err(KeychainError(status)),
                    }
                }
                status => Err(KeychainError(status)),
            }
        }

        fn base(&self) -> Vec<(CFString, CFType)> {
            unsafe {
                vec![
                    (
                        CFString::wrap_under_get_rule(kSecClass),
                        CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
                    ),
                    (CFString::wrap_under_get_rule(kSecAttrService), CFString::new(&self.service).into_CFType()),
                    (CFString::wrap_under_get_rule(kSecAttrAccount), CFString::new(ACCOUNT).into_CFType()),
                ]
            }
        }
    }
}

#[cfg(windows)]
mod sealed {
    use std::io::ErrorKind;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    use super::*;

    /// A failed read or write of the sealed sign-ins file.
    #[derive(Clone, Debug)]
    pub struct KeychainError(String);

    impl fmt::Display for KeychainError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "Saved sign-ins: {}", self.0)
        }
    }

    impl Keychain {
        pub fn new(service: impl Into<String>) -> Self {
            Self { service: service.into() }
        }

        /// The stored sign-ins; empty when there are none.
        pub fn load(&self) -> Result<SignIns, KeychainError> {
            let sealed = match std::fs::read(self.path()) {
                Ok(sealed) => sealed,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(SignIns::default()),
                Err(error) => return Err(KeychainError(error.to_string())),
            };
            Ok(SignIns::decode(&unseal(&sealed)?))
        }

        /// Replaces the stored sign-ins; with no refresh token and no remembered client left the
        /// file is deleted.
        pub fn store(&self, sign_ins: &SignIns) -> Result<(), KeychainError> {
            let path = self.path();
            if sign_ins.refresh_tokens.is_empty() && sign_ins.last_clients.is_empty() {
                return match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != ErrorKind::NotFound => Err(KeychainError(error.to_string())),
                    _ => Ok(()),
                };
            }
            let sealed = seal(&sign_ins.encode())?;
            let directory = path.parent().expect("the file is in a directory");
            crate::files::replace(directory, &path, &sealed).map_err(|error| KeychainError(error.to_string()))
        }

        fn path(&self) -> PathBuf {
            let local = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
            local.join("Mailstream").join(format!("{}.signins", self.service))
        }
    }

    /// `data` sealed so only the signed-in Windows user reads it back, on this machine.
    fn seal(data: &[u8]) -> Result<Vec<u8>, KeychainError> {
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: the input blob points at `data` for the call; the system fills `out`.
        let sealed = unsafe {
            CryptProtectData(
                &blob(data),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if sealed == 0 {
            return Err(KeychainError(format!("sealing failed: {}", std::io::Error::last_os_error())));
        }
        Ok(unsafe { take(out) })
    }

    fn unseal(data: &[u8]) -> Result<Vec<u8>, KeychainError> {
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: as in `seal`.
        let opened = unsafe {
            CryptUnprotectData(
                &blob(data),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if opened == 0 {
            return Err(KeychainError(format!("unsealing failed: {}", std::io::Error::last_os_error())));
        }
        Ok(unsafe { take(out) })
    }

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() }
    }

    /// The bytes of a blob the system allocated, which is then freed.
    unsafe fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if blob.pbData.is_null() {
            return Vec::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
        unsafe { LocalFree(blob.pbData.cast()) };
        bytes
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod private_file {
    use std::io::{ErrorKind, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A failed read or write of the sign-ins file.
    #[derive(Clone, Debug)]
    pub struct KeychainError(String);

    impl fmt::Display for KeychainError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "Saved sign-ins: {}", self.0)
        }
    }

    impl Keychain {
        pub fn new(service: impl Into<String>) -> Self {
            Self { service: service.into() }
        }

        /// The stored sign-ins; empty when there are none.
        pub fn load(&self) -> Result<SignIns, KeychainError> {
            match std::fs::read(self.path()) {
                Ok(data) => Ok(SignIns::decode(&data)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(SignIns::default()),
                Err(error) => Err(KeychainError(error.to_string())),
            }
        }

        /// Replaces the stored sign-ins; with no refresh token and no remembered client left the
        /// file is deleted.
        pub fn store(&self, sign_ins: &SignIns) -> Result<(), KeychainError> {
            let path = self.path();
            if sign_ins.refresh_tokens.is_empty() && sign_ins.last_clients.is_empty() {
                return match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != ErrorKind::NotFound => Err(KeychainError(error.to_string())),
                    _ => Ok(()),
                };
            }
            write_private(&path, &sign_ins.encode()).map_err(|error| KeychainError(error.to_string()))
        }

        fn path(&self) -> PathBuf {
            let local = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
            local.join("Mailstream").join(format!("{}.signins", self.service))
        }
    }

    /// Replaces `path` whole with `data` in a file only its owner reads, created 0600 from the
    /// start so the tokens are never readable by others, even for a moment.
    fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = path.parent().expect("the file is in a directory");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("signins");
        let temporary =
            directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| file.write_all(data).and_then(|()| file.sync_all()))
            .and_then(|()| std::fs::rename(&temporary, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: &str, secret: Option<&str>) -> OAuthClient {
        OAuthClient { id: id.into(), secret: secret.map(String::from) }
    }

    #[test]
    fn reads_and_writes_the_swift_format() {
        let swift = br#"{"clients":{"a@b.c":{"id":"i"}},"lastClients":{"google":{"id":"g","secret":"s"}},"refreshTokens":{"a@b.c":"1\/\/x"}}"#;
        let sign_ins = SignIns::decode(swift);
        assert_eq!(sign_ins.refresh_tokens["a@b.c"], "1//x");
        assert_eq!(sign_ins.clients["a@b.c"], client("i", None));
        assert_eq!(sign_ins.last_clients["google"], client("g", Some("s")));
        assert_eq!(sign_ins.encode(), swift.to_vec());
        // The bare object of refresh tokens from before clients were stored.
        let legacy = SignIns::decode(br#"{"a@b.c":"tok"}"#);
        assert_eq!((legacy.refresh_tokens["a@b.c"].as_str(), legacy.clients.len()), ("tok", 0));
        assert_eq!(SignIns::decode(b"garbage"), SignIns::default());
    }

    #[test]
    fn keeps_one_item_under_its_service() {
        let keychain = Keychain::new(format!("dev.mailstream.test.{}.{}", std::process::id(), crate::tests::unique()));
        assert_eq!(keychain.load().unwrap(), SignIns::default());
        let mut sign_ins = SignIns::default();
        sign_ins.refresh_tokens.insert("a@b.c".into(), "one".into());
        sign_ins.last_clients.insert("google".into(), client("g", Some("s")));
        keychain.store(&sign_ins).unwrap();
        assert_eq!(keychain.load().unwrap(), sign_ins);
        sign_ins.refresh_tokens.insert("a@b.c".into(), "two".into());
        keychain.store(&sign_ins).unwrap();
        assert_eq!(keychain.load().unwrap(), sign_ins);
        // Nothing left to keep deletes the item.
        keychain.store(&SignIns::default()).unwrap();
        assert_eq!(keychain.load().unwrap(), SignIns::default());
    }
}
