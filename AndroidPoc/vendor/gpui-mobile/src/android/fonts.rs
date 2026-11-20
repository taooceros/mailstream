//! General Android system font loader and `cosmic_text::Fallback` implementation for GPUI.
//!
//! Uses `mailstream_android_fonts::AndroidFontConfig::discover()` (Android NDK API 29+
//! `ASystemFontIterator` / `AFontMatcher` + XML + directory scan) so GPUI loads all system
//! fonts lazily via `fontdb::Database::load_font_file` (mmap on demand), resolves generic
//! families (`sans-serif`, `serif`, `monospace`), and provides script-, locale-, and emoji-aware
//! fallback chains instead of cosmic-text's empty Android fallback stub.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use cosmic_text::{Fallback, FontSystem, fontdb};
use gpui_wgpu::CosmicTextSystem;
use mailstream_android_fonts::{
    AndroidFontConfig, FontFileRef, GenericRole, SAFETY_NET_EMOJI, SAFETY_NET_HK, SAFETY_NET_JP,
    SAFETY_NET_KR, SAFETY_NET_MATH, SAFETY_NET_MONO, SAFETY_NET_SANS, SAFETY_NET_SC,
    SAFETY_NET_SERIF, SAFETY_NET_SYMBOLS, SAFETY_NET_TC, safety_net_script_families,
};
use unicode_script::Script;

/// Process-wide table of discovered Android font family names (initialized once from `fontdb`),
/// allowing `AndroidFallback` to hand out `&'static str` slices to `cosmic_text::Fallback`
/// without leaking memory on repeated calls.
static DISCOVERED_FAMILY_NAMES: OnceLock<Vec<String>> = OnceLock::new();

struct AndroidFallback {
    common: Box<[&'static str]>,
    scripts: HashMap<[u8; 4], Box<[&'static str]>>,
    han_by_locale: HashMap<&'static str, Box<[&'static str]>>,
}

impl Fallback for AndroidFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &self.common
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        &[]
    }

    fn script_fallback(&self, script: Script, locale: &str) -> &[&'static str] {
        if script == Script::Han {
            let key = AndroidFontConfig::canonical_han_locale(locale);
            if let Some(chain) = self.han_by_locale.get(key) {
                return chain;
            }
            if let Some(chain) = self.han_by_locale.get("zh-Hans") {
                return chain;
            }
        }
        if let Ok(tag) = <[u8; 4]>::try_from(script.short_name().as_bytes()) {
            if let Some(chain) = self.scripts.get(&tag) {
                return chain;
            }
            return safety_net_script_families(tag);
        }
        &[]
    }
}

/// Builds GPUI's Android `CosmicTextSystem` backed by the device's full system font table.
pub(crate) fn build_android_text_system() -> Arc<CosmicTextSystem> {
    let config = AndroidFontConfig::discover();

    let mut db = fontdb::Database::new();
    for path in &config.font_files {
        if let Err(err) = db.load_font_file(path) {
            log::debug!("skipping unreadable system font {}: {err}", path.display());
        }
    }

    // Index loaded faces by `(file_path, collection_index)` -> primary family name,
    // and collect every family name available in `db`.
    let mut by_file: HashMap<(PathBuf, u32), String> = HashMap::new();
    let mut available_set: HashSet<String> = HashSet::new();
    for face in db.faces() {
        let Some((family_name, _)) = face.families.first() else {
            continue;
        };
        available_set.insert(family_name.clone());
        match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                by_file
                    .entry((path.clone(), face.index))
                    .or_insert_with(|| family_name.clone());
            }
            fontdb::Source::Binary(_) => {}
        }
    }

    // Freeze discovered family names once for `'static` borrowing by `cosmic_text::Fallback`.
    let stored_names: &'static [String] = DISCOVERED_FAMILY_NAMES.get_or_init(|| {
        let mut names: Vec<String> = available_set.iter().cloned().collect();
        names.sort_unstable();
        names
    });
    let static_by_name: HashMap<&str, &'static str> = stored_names
        .iter()
        .map(|s| (s.as_str(), s.as_str()))
        .collect();

    let resolve_chain =
        |faces: Option<&Vec<FontFileRef>>, safety_net: &[&str]| -> Box<[&'static str]> {
            let mut out: Vec<&'static str> = Vec::new();
            let mut push_name = |name: &str| {
                if available_set.contains(name) {
                    if let Some(&static_name) = static_by_name.get(name) {
                        if !out.contains(&static_name) {
                            out.push(static_name);
                        }
                    }
                }
            };
            if let Some(faces) = faces {
                for f in faces {
                    if let Some(name) = by_file.get(&(f.path.clone(), f.index)) {
                        push_name(name);
                    }
                }
            }
            for &name in safety_net {
                push_name(name);
            }
            out.into_boxed_slice()
        };

    let sans_chain = resolve_chain(
        config.generic_faces.get(&GenericRole::SansSerif),
        SAFETY_NET_SANS,
    );
    let serif_chain = resolve_chain(
        config.generic_faces.get(&GenericRole::Serif),
        SAFETY_NET_SERIF,
    );
    let mono_chain = resolve_chain(
        config.generic_faces.get(&GenericRole::Monospace),
        SAFETY_NET_MONO,
    );
    let emoji_chain = resolve_chain(
        config.generic_faces.get(&GenericRole::Emoji),
        SAFETY_NET_EMOJI,
    );

    let sans_primary = sans_chain
        .first()
        .copied()
        .or_else(|| {
            db.faces()
                .next()
                .and_then(|f| f.families.first())
                .and_then(|(n, _)| static_by_name.get(n.as_str()).copied())
        })
        .unwrap_or("Roboto");
    db.set_sans_serif_family(sans_primary);
    if let Some(&serif) = serif_chain.first() {
        db.set_serif_family(serif);
    }
    if let Some(&mono) = mono_chain.first() {
        db.set_monospace_family(mono);
    }

    // Build CJK locale-specific Han chains (with Tier-3 safety nets appended).
    let mut han_by_locale: HashMap<&'static str, Box<[&'static str]>> = HashMap::new();
    for &(locale_key, safety) in &[
        ("zh-Hans", SAFETY_NET_SC),
        ("zh-Hant", SAFETY_NET_TC),
        ("zh-HK", SAFETY_NET_HK),
        ("ja", SAFETY_NET_JP),
        ("ko", SAFETY_NET_KR),
    ] {
        let chain = resolve_chain(
            config
                .locale_script_faces
                .get(&(*b"Hani", locale_key.to_string())),
            safety,
        );
        if !chain.is_empty() {
            han_by_locale.insert(locale_key, chain);
        }
    }

    // Build general per-script fallback chains from all discovered scripts.
    let mut scripts: HashMap<[u8; 4], Box<[&'static str]>> = HashMap::new();
    for (&tag, faces) in &config.script_faces {
        let chain = resolve_chain(Some(faces), safety_net_script_families(tag));
        if !chain.is_empty() {
            scripts.insert(tag, chain);
        }
    }
    for &tag in &[
        *b"Hira", *b"Kana", *b"Hang", *b"Bopo", *b"Arab", *b"Hebr", *b"Thai", *b"Deva",
        *b"Beng", *b"Taml", *b"Telu", *b"Knda", *b"Mlym", *b"Gujr", *b"Guru", *b"Orya",
        *b"Sinh", *b"Mymr", *b"Khmr", *b"Laoo", *b"Tibt", *b"Geor", *b"Armn", *b"Ethi",
    ] {
        scripts
            .entry(tag)
            .or_insert_with(|| resolve_chain(None, safety_net_script_families(tag)));
    }

    // Build common fallback chain: sans-serif -> symbols/math -> color emoji & flags -> CJK punctuation.
    let mut common: Vec<&'static str> = Vec::new();
    let mut push_common = |slice: &[&'static str]| {
        for &name in slice {
            if !common.contains(&name) {
                common.push(name);
            }
        }
    };
    push_common(&sans_chain);
    let discovered_common = resolve_chain(Some(&config.common_fallback_faces), SAFETY_NET_SYMBOLS);
    push_common(&discovered_common);
    let math_chain = resolve_chain(config.generic_faces.get(&GenericRole::Math), SAFETY_NET_MATH);
    push_common(&math_chain);
    push_common(&emoji_chain);
    if let Some(han_default) =
        han_by_locale.get(AndroidFontConfig::canonical_han_locale(&config.system_locale))
    {
        push_common(han_default);
    }

    log::info!(
        "gpui-mobile: Android font system ready: {} faces, sans={sans_primary:?}, mono={:?}, emoji={:?}, locale={:?}, ndk_api={}",
        db.len(),
        mono_chain.first().copied(),
        emoji_chain.as_ref(),
        config.system_locale,
        config.used_ndk_api,
    );

    let fallback = AndroidFallback {
        common: common.into_boxed_slice(),
        scripts,
        han_by_locale,
    };
    let font_system =
        FontSystem::new_with_locale_and_db_and_fallback(config.system_locale.clone(), db, fallback);
    Arc::new(CosmicTextSystem::new_with_font_system(
        font_system,
        sans_primary,
    ))
}
