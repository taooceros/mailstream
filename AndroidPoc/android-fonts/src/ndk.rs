//! Runtime `dlsym` bindings to Android NDK API 29+ system font APIs
//! (`ASystemFontIterator`, `AFont`, `AFontMatcher`) and system locale properties.
//!
//! Loading symbols dynamically via `dlsym` keeps binary compatibility with `minSdkVersion = 26`
//! while automatically unlocking the authoritative `minikin` system font configuration on
//! Android 10–16+ devices.

use crate::AndroidFontConfig;

#[cfg(not(target_os = "android"))]
pub(crate) fn system_locale() -> Option<String> {
    None
}

#[cfg(not(target_os = "android"))]
pub(crate) fn populate_from_ndk(_config: &mut AndroidFontConfig) {}

#[cfg(target_os = "android")]
pub(crate) fn system_locale() -> Option<String> {
    use std::ffi::{CStr, CString, c_char, c_int};

    type PropGetFn = unsafe extern "C" fn(*const c_char, *mut c_char) -> c_int;
    let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__system_property_get".as_ptr()) };
    if sym.is_null() {
        return None;
    }
    let prop_get: PropGetFn = unsafe { std::mem::transmute(sym) };

    for key in ["persist.sys.locale", "ro.product.locale", "persist.sys.language"] {
        let Ok(c_key) = CString::new(key) else {
            continue;
        };
        let mut buf = [0u8; 128];
        let len = unsafe { prop_get(c_key.as_ptr(), buf.as_mut_ptr() as *mut c_char) };
        if len > 0
            && let Some(val) = CStr::from_bytes_until_nul(&buf).ok().and_then(|s| s.to_str().ok())
        {
            let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.replace('_', "-"));
                }
        }
    }
    None
}

#[cfg(target_os = "android")]
pub(crate) fn populate_from_ndk(config: &mut AndroidFontConfig) {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::PathBuf;

    use crate::{
        EMOJI_PROBES, FontFileRef, GenericRole, HAN_LOCALE_PROBES, MATH_PROBES, SCRIPT_PROBES,
        SYMBOL_PROBES,
    };

    #[repr(C)]
    struct ASystemFontIterator {
        _private: [u8; 0],
    }
    #[repr(C)]
    struct AFont {
        _private: [u8; 0],
    }
    #[repr(C)]
    struct AFontMatcher {
        _private: [u8; 0],
    }

    struct NdkFontApi {
        iter_open: unsafe extern "C" fn() -> *mut ASystemFontIterator,
        iter_next: unsafe extern "C" fn(*mut ASystemFontIterator) -> *mut AFont,
        iter_close: unsafe extern "C" fn(*mut ASystemFontIterator),
        font_close: unsafe extern "C" fn(*mut AFont),
        font_path: unsafe extern "C" fn(*const AFont) -> *const c_char,
        font_index: unsafe extern "C" fn(*const AFont) -> usize,
        matcher_create: Option<unsafe extern "C" fn() -> *mut AFontMatcher>,
        matcher_destroy: Option<unsafe extern "C" fn(*mut AFontMatcher)>,
        matcher_set_style: Option<unsafe extern "C" fn(*mut AFontMatcher, u16, bool)>,
        matcher_set_locales: Option<unsafe extern "C" fn(*mut AFontMatcher, *const c_char)>,
        matcher_match: Option<
            unsafe extern "C" fn(
                *const AFontMatcher,
                *const c_char,
                *const u16,
                u32,
                *mut u32,
            ) -> *mut AFont,
        >,
    }

    unsafe fn load_sym(handle: *mut c_void, name: &CStr) -> *mut c_void {
        let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if !p.is_null() {
            return p;
        }
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
    }

    let handle = unsafe {
        let h = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() { libc::RTLD_DEFAULT } else { h }
    };

    let api = unsafe {
        let iter_open = load_sym(handle, c"ASystemFontIterator_open");
        let iter_next = load_sym(handle, c"ASystemFontIterator_next");
        let iter_close = load_sym(handle, c"ASystemFontIterator_close");
        let font_close = load_sym(handle, c"AFont_close");
        let font_path = load_sym(handle, c"AFont_getFontFilePath");
        let font_index = load_sym(handle, c"AFont_getCollectionIndex");

        if iter_open.is_null()
            || iter_next.is_null()
            || iter_close.is_null()
            || font_close.is_null()
            || font_path.is_null()
            || font_index.is_null()
        {
            log::info!("android-fonts: NDK API 29+ system font symbols unavailable; falling back to XML/dir scan");
            return;
        }

        let matcher_create = load_sym(handle, c"AFontMatcher_create");
        let matcher_destroy = load_sym(handle, c"AFontMatcher_destroy");
        let matcher_set_style = load_sym(handle, c"AFontMatcher_setStyle");
        let matcher_set_locales = load_sym(handle, c"AFontMatcher_setLocales");
        let matcher_match = load_sym(handle, c"AFontMatcher_match");

        NdkFontApi {
            iter_open: std::mem::transmute(iter_open),
            iter_next: std::mem::transmute(iter_next),
            iter_close: std::mem::transmute(iter_close),
            font_close: std::mem::transmute(font_close),
            font_path: std::mem::transmute(font_path),
            font_index: std::mem::transmute(font_index),
            matcher_create: (!matcher_create.is_null()).then(|| std::mem::transmute(matcher_create)),
            matcher_destroy: (!matcher_destroy.is_null()).then(|| std::mem::transmute(matcher_destroy)),
            matcher_set_style: (!matcher_set_style.is_null()).then(|| std::mem::transmute(matcher_set_style)),
            matcher_set_locales: (!matcher_set_locales.is_null()).then(|| std::mem::transmute(matcher_set_locales)),
            matcher_match: (!matcher_match.is_null()).then(|| std::mem::transmute(matcher_match)),
        }
    };

    // 1. Enumerate every active system font file via `ASystemFontIterator`.
    unsafe {
        let iter = (api.iter_open)();
        if !iter.is_null() {
            config.used_ndk_api = true;
            loop {
                let font = (api.iter_next)(iter);
                if font.is_null() {
                    break;
                }
                let path_ptr = (api.font_path)(font);
                if !path_ptr.is_null()
                    && let Ok(path_str) = CStr::from_ptr(path_ptr).to_str()
                {
                    let path = PathBuf::from(path_str);
                    if path.is_file() {
                        config.push_font_file(path);
                    }
                }
                (api.font_close)(font);
            }
            (api.iter_close)(iter);
        }
    }
    // 2. Query Android's native `AFontMatcher` for generic roles, emoji, symbols, math, and scripts.
    let (Some(create), Some(destroy), Some(set_style), Some(set_locales), Some(match_fn)) = (
        api.matcher_create,
        api.matcher_destroy,
        api.matcher_set_style,
        api.matcher_set_locales,
        api.matcher_match,
    ) else {
        return;
    };

    let matcher = unsafe { create() };
    if matcher.is_null() {
        return;
    }

    let query = |family: &CStr, locale: Option<&str>, text: &str| -> Option<FontFileRef> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        if utf16.is_empty() {
            return None;
        }
        let c_locale = locale.and_then(|l| CString::new(l).ok());
        unsafe {
            set_style(matcher, 400, false);
            if let Some(loc) = &c_locale {
                set_locales(matcher, loc.as_ptr());
            } else {
                set_locales(matcher, c"".as_ptr());
            }
            let font = match_fn(
                matcher,
                family.as_ptr(),
                utf16.as_ptr(),
                utf16.len() as u32,
                std::ptr::null_mut(),
            );
            if font.is_null() {
                return None;
            }
            let path_ptr = (api.font_path)(font);
            let index = (api.font_index)(font) as u32;
            let result = if !path_ptr.is_null() {
                CStr::from_ptr(path_ptr)
                    .to_str()
                    .ok()
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
                    .map(|p| FontFileRef::new(p, index))
            } else {
                None
            };
            (api.font_close)(font);
            result
        }
    };

    let sans_default = query(c"sans-serif", None, "A");
    if let Some(face) = &sans_default {
        config.push_generic(GenericRole::SansSerif, face.clone());
        config.push_generic(GenericRole::SystemUi, face.clone());
    }
    if let Some(face) = query(c"serif", None, "A") {
        config.push_generic(GenericRole::Serif, face);
    }
    if let Some(face) = query(c"monospace", None, "A") {
        config.push_generic(GenericRole::Monospace, face);
    }
    if let Some(face) = query(c"cursive", None, "A") {
        config.push_generic(GenericRole::Cursive, face);
    }
    if let Some(face) = query(c"fantasy", None, "A") {
        config.push_generic(GenericRole::Fantasy, face);
    }

    // Symbols & Math
    for probe in SYMBOL_PROBES {
        if let Some(face) = query(c"sans-serif", None, probe)
            && sans_default.as_ref() != Some(&face)
        {
            config.push_common_fallback(face);
        }
    }
    for probe in MATH_PROBES {
        if let Some(face) = query(c"sans-serif", None, probe)
            && sans_default.as_ref() != Some(&face)
        {
            config.push_generic(GenericRole::Math, face.clone());
            config.push_common_fallback(face);
        }
    }

    // Color Emoji & Flag Emoji
    for probe in EMOJI_PROBES {
        if let Some(face) = query(c"sans-serif", None, probe)
            && sans_default.as_ref() != Some(&face)
        {
            config.push_generic(GenericRole::Emoji, face.clone());
            config.push_common_fallback(face);
        }
    }

    // CJK locale disambiguation (`zh-Hans`, `zh-Hant`, `zh-HK`, `ja`, `ko`)
    for &(canonical, locale_list, probes) in HAN_LOCALE_PROBES {
        for probe in probes {
            if let Some(face) = query(c"sans-serif", Some(locale_list), probe) {
                config.push_locale_script(*b"Hani", canonical, face.clone());
                if canonical == "zh-Hans" {
                    config.push_script(*b"Hani", face);
                }
            }
        }
    }

    // All major Unicode scripts
    for &(script, probe) in SCRIPT_PROBES {
        if let Some(face) = query(c"sans-serif", None, probe) {
            if script == *b"Latn" || sans_default.as_ref() != Some(&face) {
                config.push_script(script, face);
            }
        }
    }

    unsafe {
        destroy(matcher);
    }
}
