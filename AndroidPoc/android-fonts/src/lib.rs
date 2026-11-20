//! General Android system font discovery and fallback resolution shared by GPUI (`CosmicTextSystem`)
//! and Blitz (`fontique`).
//!
//! Discovery runs once per process (`AndroidFontConfig::discover`) across three layers:
//! 1. **Android NDK API 29+ (`ASystemFontIterator` + `AFontMatcher`)** via runtime `dlsym` on
//!    `libandroid.so` (compatible with `minSdk = 26`). This reads the exact active system font
//!    table used by Android's own `minikin` text engine — including `/data/fonts/files/` updatable
//!    fonts, `/product/fonts`, `/system_ext/fonts`, and OEM fonts (HyperOS/HarmonyOS/OneUI) — even
//!    when SELinux restricts `/system/etc/fonts.xml` or Android 15+ uses `font_fallback.xml`.
//! 2. **Android XML config + directory scan** (`config.xml`, `font_fallback.xml`, `fonts.xml`,
//!    `fonts_customization.xml` + `/data/fonts/files`, `/product/fonts`, `/system_ext/fonts`,
//!    `/system/fonts`) preserving TTC `index`, `<alias>`, `und-Zsye` emoji, and script/locale tags.
//! 3. **Built-in safety-net family chains** appended after system-discovered matches so broken or
//!    subsetted ROM font configs can never regress to tofu.

mod ndk;
pub mod xml;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// A concrete font face identified by file path and TTC/OTC collection index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FontFileRef {
    pub path: PathBuf,
    pub index: u32,
}

impl FontFileRef {
    pub fn new(path: impl Into<PathBuf>, index: u32) -> Self {
        Self { path: path.into(), index }
    }
}

/// Generic CSS / platform font roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenericRole {
    SansSerif,
    Serif,
    Monospace,
    SystemUi,
    Emoji,
    Math,
    Cursive,
    Fantasy,
}

/// Device-tailored Android system font inventory and fallback map.
#[derive(Clone, Debug, Default)]
pub struct AndroidFontConfig {
    /// Android system locale (BCP-47, e.g. `"zh-CN"`, `"en-US"`).
    pub system_locale: String,
    /// Readable font files in system priority order (updatable fonts first, then `/product`,
    /// `/system_ext`, `/system`).
    pub font_files: Vec<PathBuf>,
    /// Extra readable font directories beyond `/system/fonts` (e.g. `/data/fonts/files`,
    /// `/product/fonts`, `/system_ext/fonts`).
    pub extra_font_dirs: Vec<PathBuf>,
    /// Ordered font faces per generic role (`SansSerif`, `Monospace`, `Emoji`, ...).
    pub generic_faces: HashMap<GenericRole, Vec<FontFileRef>>,
    /// Ordered font faces per ISO 15924 script tag (e.g. `*b"Hani"`, `*b"Arab"`, `*b"Deva"`).
    pub script_faces: HashMap<[u8; 4], Vec<FontFileRef>>,
    /// Ordered font faces per `(script_tag, canonical_locale)` (e.g. `(*b"Hani", "zh-Hans")`,
    /// `(*b"Hani", "zh-Hant")`, `(*b"Hani", "zh-HK")`, `(*b"Hani", "ja")`, `(*b"Hani", "ko")`).
    pub locale_script_faces: HashMap<([u8; 4], String), Vec<FontFileRef>>,
    /// Ordered common fallback faces (symbols, math, color emoji, flag emoji, wide coverage).
    pub common_fallback_faces: Vec<FontFileRef>,
    /// Named family aliases parsed from Android XML (`name` -> `to`).
    pub aliases: HashMap<String, String>,
    /// Whether NDK API 29+ (`ASystemFontIterator` / `AFontMatcher`) succeeded.
    pub used_ndk_api: bool,
}

/// Standard Android font directories in priority order (Play Services updatable fonts first).
pub const DEFAULT_FONT_DIRS: &[&str] = &[
    "/data/fonts/files",
    "/product/fonts",
    "/system_ext/fonts",
    "/system/fonts",
];

/// Standard Android font configuration XML files in priority order.
pub const DEFAULT_XML_PATHS: &[&str] = &[
    "/data/fonts/config/config.xml",
    "/system/etc/font_fallback.xml",
    "/system/etc/fonts.xml",
    "/product/etc/fonts_customization.xml",
    "/system_ext/etc/fonts_customization.xml",
];

/// Tier-3 safety-net family name chains (appended after device-discovered faces).
pub const SAFETY_NET_SANS: &[&str] = &[
    "Roboto Flex",
    "Roboto",
    "MiSans Latin VF",
    "Noto Sans",
    "MiSans VF",
    "Droid Sans",
];
pub const SAFETY_NET_SERIF: &[&str] = &["Noto Serif", "Droid Serif"];
pub const SAFETY_NET_MONO: &[&str] = &[
    "Roboto Mono",
    "Droid Sans Mono",
    "Cutive Mono",
    "Noto Sans Mono",
];
pub const SAFETY_NET_EMOJI: &[&str] = &[
    "Noto Color Emoji",
    "Noto Color Emoji Flags",
    "SamsungColorEmoji",
    "Android Emoji",
];
pub const SAFETY_NET_MATH: &[&str] = &["Noto Sans Math", "Noto Sans Symbols", "Noto Sans"];
pub const SAFETY_NET_SYMBOLS: &[&str] = &[
    "Noto Sans Symbols",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols2",
    "Noto Sans Math",
];
pub const SAFETY_NET_SC: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans CN",
    "Source Han Sans SC",
    "HarmonyOS Sans SC",
    "MiSans VF",
    "Noto Serif CJK SC",
    "Droid Sans Fallback",
    "Noto Sans CJK TC",
    "Noto Sans CJK HK",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    "Misans TC VF",
    "MiSans L3",
];
pub const SAFETY_NET_TC: &[&str] = &[
    "Noto Sans CJK TC",
    "Noto Sans TC",
    "Source Han Sans TW",
    "Source Han Sans TC",
    "HarmonyOS Sans TC",
    "Misans TC VF",
    "Noto Sans CJK HK",
    "Noto Sans CJK SC",
    "MiSans VF",
    "Noto Serif CJK TC",
    "Droid Sans Fallback",
    "MiSans L3",
];
pub const SAFETY_NET_HK: &[&str] = &[
    "Noto Sans CJK HK",
    "Noto Sans HK",
    "Noto Sans CJK TC",
    "Misans TC VF",
    "Noto Sans CJK SC",
    "MiSans VF",
    "Droid Sans Fallback",
    "MiSans L3",
];
pub const SAFETY_NET_JP: &[&str] = &[
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "MiSans Japanese VF",
    "Noto Sans CJK SC",
    "MiSans VF",
    "Droid Sans Fallback",
];
pub const SAFETY_NET_KR: &[&str] = &[
    "Noto Sans CJK KR",
    "Noto Sans KR",
    "MiSans Korean VF",
    "Noto Sans CJK SC",
    "MiSans VF",
    "Droid Sans Fallback",
];

/// Well-known Android Noto family names per ISO 15924 script tag (used as Tier-3 safety net
/// when neither `AFontMatcher` nor XML yields a script entry).
pub fn safety_net_script_families(script: [u8; 4]) -> &'static [&'static str] {
    match &script {
        b"Hani" => SAFETY_NET_SC,
        b"Hira" | b"Kana" => SAFETY_NET_JP,
        b"Hang" => SAFETY_NET_KR,
        b"Bopo" => SAFETY_NET_TC,
        b"Arab" => &["Noto Naskh Arabic", "Noto Sans Arabic"],
        b"Hebr" => &["Noto Sans Hebrew"],
        b"Thai" => &["Noto Sans Thai"],
        b"Deva" => &["Noto Sans Devanagari"],
        b"Beng" => &["Noto Sans Bengali"],
        b"Taml" => &["Noto Sans Tamil"],
        b"Telu" => &["Noto Sans Telugu"],
        b"Knda" => &["Noto Sans Kannada"],
        b"Mlym" => &["Noto Sans Malayalam"],
        b"Gujr" => &["Noto Sans Gujarati"],
        b"Guru" => &["Noto Sans Gurmukhi"],
        b"Orya" => &["Noto Sans Oriya"],
        b"Sinh" => &["Noto Sans Sinhala"],
        b"Mymr" => &["Noto Sans Myanmar"],
        b"Khmr" => &["Noto Sans Khmer"],
        b"Laoo" => &["Noto Sans Lao"],
        b"Tibt" => &["Noto Serif Tibetan", "Noto Sans Tibetan"],
        b"Geor" => &["Noto Sans Georgian"],
        b"Armn" => &["Noto Sans Armenian"],
        b"Ethi" => &["Noto Sans Ethiopic"],
        b"Cher" => &["Noto Sans Cherokee"],
        b"Mong" => &["Noto Sans Mongolian"],
        b"Syrc" => &["Noto Sans Syriac"],
        b"Thaa" => &["Noto Sans Thaana"],
        b"Adlm" => &["Noto Sans Adlam"],
        b"Tfng" => &["Noto Sans Tifinagh"],
        b"Vaii" => &["Noto Sans Vai"],
        b"Yiii" => &["Noto Sans Yi", "Noto Sans CJK SC"],
        b"Cans" => &["Noto Sans Canadian Aboriginal"],
        b"Cyrl" | b"Grek" | b"Latn" => SAFETY_NET_SANS,
        _ => &[],
    }
}

/// Representative Unicode probes for querying `AFontMatcher` per script.
#[cfg(target_os = "android")]
pub(crate) const SCRIPT_PROBES: &[([u8; 4], &str)] = &[
    (*b"Latn", "A"),
    (*b"Cyrl", "Ж"),
    (*b"Grek", "Ω"),
    (*b"Hira", "あ"),
    (*b"Kana", "ア"),
    (*b"Hang", "한"),
    (*b"Bopo", "ㄅ"),
    (*b"Arab", "ع"),
    (*b"Hebr", "א"),
    (*b"Thai", "ก"),
    (*b"Deva", "अ"),
    (*b"Beng", "অ"),
    (*b"Taml", "அ"),
    (*b"Telu", "అ"),
    (*b"Knda", "ಅ"),
    (*b"Mlym", "അ"),
    (*b"Gujr", "અ"),
    (*b"Guru", "ਅ"),
    (*b"Orya", "ଅ"),
    (*b"Sinh", "අ"),
    (*b"Mymr", "က"),
    (*b"Khmr", "ក"),
    (*b"Laoo", "ກ"),
    (*b"Tibt", "ཀ"),
    (*b"Geor", "ა"),
    (*b"Armn", "Ա"),
    (*b"Ethi", "ሀ"),
    (*b"Cher", "Ꭰ"),
    (*b"Mong", "ᠠ"),
    (*b"Syrc", "ܐ"),
    (*b"Thaa", "ހ"),
    (*b"Adlm", "𞤀"),
    (*b"Tfng", "ⴰ"),
    (*b"Vaii", "ꔀ"),
    (*b"Yiii", "ꀀ"),
    (*b"Cans", "ᐁ"),
];

/// Representative Han probes for CJK locale disambiguation via `AFontMatcher`.
/// Including Simplified-only characters (`简`, `邮`) catches OEM subset fonts that omit them.
#[cfg(target_os = "android")]
pub(crate) const HAN_LOCALE_PROBES: &[(&str, &str, &[&str])] = &[
    ("zh-Hans", "zh-Hans,zh-CN", &["骨", "简", "邮"]),
    ("zh-Hant", "zh-Hant,zh-TW", &["骨", "繁", "郵"]),
    ("zh-HK", "zh-Hant-HK,zh-HK", &["骨", "邨", "郵"]),
    ("ja", "ja-JP,ja", &["骨", "直", "あ"]),
    ("ko", "ko-KR,ko", &["骨", "한"]),
];

/// Representative emoji probes covering standard emoji, VS16 text-default emoji, ZWJ sequences,
/// regional-indicator flags (often in a separate `NotoColorEmojiFlags.ttf`), and newer emoji.
#[cfg(target_os = "android")]
pub(crate) const EMOJI_PROBES: &[&str] = &[
    "😀",
    "❤️\u{fe0f}",
    "👨‍👩‍👧",
    "🇺🇸",
    "🦀",
    "✅",
];

#[cfg(target_os = "android")]
pub(crate) const MATH_PROBES: &[&str] = &["∑", "∫", "ℝ"];
#[cfg(target_os = "android")]
pub(crate) const SYMBOL_PROBES: &[&str] = &["★", "✓", "→", "⌘", "🄀"];

impl AndroidFontConfig {
    /// Discovers Android system fonts once per process and returns the cached configuration.
    pub fn discover() -> &'static Self {
        static CACHE: LazyLock<AndroidFontConfig> =
            LazyLock::new(AndroidFontConfig::discover_uncached);
        &CACHE
    }

    fn discover_uncached() -> Self {
        let mut config = Self {
            system_locale: ndk::system_locale().unwrap_or_else(|| "en-US".to_string()),
            ..Self::default()
        };

        // Tier 1: Android NDK API 29+ (`ASystemFontIterator` + `AFontMatcher`).
        ndk::populate_from_ndk(&mut config);

        // Tier 2: Android XML (`config.xml`, `font_fallback.xml`, `fonts.xml`, customization)
        // + standard font directories. Appends anything NDK missed (or runs as primary on API 26-28).
        let xml_paths: Vec<PathBuf> = DEFAULT_XML_PATHS.iter().map(PathBuf::from).collect();
        let font_dirs: Vec<PathBuf> = DEFAULT_FONT_DIRS.iter().map(PathBuf::from).collect();
        config.merge_xml_and_dirs(&xml_paths, &font_dirs);

        log::info!(
            "android-fonts: discovered {} files ({} extra dirs, ndk_api={}, locale={}, emoji_faces={}, scripts={})",
            config.font_files.len(),
            config.extra_font_dirs.len(),
            config.used_ndk_api,
            config.system_locale,
            config.generic_faces.get(&GenericRole::Emoji).map_or(0, Vec::len),
            config.script_faces.len(),
        );

        config
    }

    /// Builds a configuration solely from explicit XML paths and font directories.
    /// Useful for deterministic cross-platform unit tests.
    pub fn discover_from_xml_and_dirs(
        xml_paths: &[PathBuf],
        font_dirs: &[PathBuf],
        system_locale: &str,
    ) -> Self {
        let mut config = Self {
            system_locale: system_locale.to_string(),
            ..Self::default()
        };
        config.merge_xml_and_dirs(xml_paths, font_dirs);
        config
    }

    fn merge_xml_and_dirs(&mut self, xml_paths: &[PathBuf], font_dirs: &[PathBuf]) {
        let existing_dirs: Vec<PathBuf> = font_dirs
            .iter()
            .filter(|dir| dir.is_dir())
            .cloned()
            .collect();

        for dir in &existing_dirs {
            if dir != Path::new("/system/fonts") {
                push_unique(&mut self.extra_font_dirs, dir.clone());
            }
        }

        // Parse XML files in priority order; ignore missing or SELinux-denied files cleanly.
        for xml_path in xml_paths {
            if let Ok(contents) = std::fs::read_to_string(xml_path) {
                xml::parse_into(&contents, &existing_dirs, self);
            }
        }

        // Scan font directories so every readable font file is registered even if XML was unreadable.
        let mut seen_files: HashSet<PathBuf> = self.font_files.iter().cloned().collect();
        for dir in &existing_dirs {
            scan_font_dir(dir, 3, &mut self.font_files, &mut seen_files);
        }
    }

    pub(crate) fn push_font_file(&mut self, path: PathBuf) {
        if !self.font_files.contains(&path) {
            if let Some(parent) = path.parent()
                && parent != Path::new("/system/fonts")
                && parent.is_dir()
            {
                push_unique(&mut self.extra_font_dirs, parent.to_path_buf());
            }
            self.font_files.push(path);
        }
    }

    pub(crate) fn push_generic(&mut self, role: GenericRole, face: FontFileRef) {
        self.push_font_file(face.path.clone());
        let list = self.generic_faces.entry(role).or_default();
        push_unique(list, face);
    }

    pub(crate) fn push_script(&mut self, script: [u8; 4], face: FontFileRef) {
        self.push_font_file(face.path.clone());
        let list = self.script_faces.entry(script).or_default();
        push_unique(list, face);
    }

    pub(crate) fn push_locale_script(&mut self, script: [u8; 4], locale: &str, face: FontFileRef) {
        self.push_font_file(face.path.clone());
        let list = self
            .locale_script_faces
            .entry((script, locale.to_string()))
            .or_default();
        push_unique(list, face);
    }

    pub(crate) fn push_common_fallback(&mut self, face: FontFileRef) {
        self.push_font_file(face.path.clone());
        push_unique(&mut self.common_fallback_faces, face);
    }

    /// Returns the preferred Han locale key (`"zh-Hans"`, `"zh-Hant"`, `"zh-HK"`, `"ja"`, `"ko"`)
    /// for a given BCP-47 locale tag.
    pub fn canonical_han_locale(locale: &str) -> &'static str {
        let lower = locale.trim().to_ascii_lowercase().replace('_', "-");
        if lower.starts_with("ja") {
            "ja"
        } else if lower.starts_with("ko") {
            "ko"
        } else if lower.contains("-hk") || lower.contains("-mo") {
            "zh-HK"
        } else if lower.contains("hant") || lower.contains("-tw") {
            "zh-Hant"
        } else {
            "zh-Hans"
        }
    }
}

pub(crate) fn push_unique<T: PartialEq>(vec: &mut Vec<T>, item: T) {
    if !vec.contains(&item) {
        vec.push(item);
    }
}

fn is_font_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        })
}

fn scan_font_dir(dir: &Path, max_depth: u32, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    children.sort();

    for path in children {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if max_depth > 0 {
                scan_font_dir(&path, max_depth - 1, out, seen);
            }
        } else if meta.is_file() && is_font_extension(&path) && seen.insert(path.clone()) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ms434-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_aosp_fonts_xml_with_ttc_indices_emoji_and_scripts() {
        let root = temp_dir("aosp-xml");
        let sys_fonts = root.join("system_fonts");
        let upd_fonts = root.join("data_fonts_files");
        fs::create_dir_all(&sys_fonts).unwrap();
        fs::create_dir_all(&upd_fonts).unwrap();

        for name in [
            "Roboto-Regular.ttf",
            "NotoSerif-Regular.ttf",
            "DroidSansMono.ttf",
            "NotoColorEmoji.ttf",
            "NotoColorEmojiFlags.ttf",
            "NotoSansCJK-Regular.ttc",
            "NotoSansArabic-Regular.ttf",
            "NotoSansSymbols-Regular-Subsetted.ttf",
            "NotoSansMath-Regular.ttf",
        ] {
            fs::write(sys_fonts.join(name), b"stub").unwrap();
        }
        // Updatable Play Services emoji in /data/fonts/files takes priority over /system/fonts.
        fs::write(upd_fonts.join("NotoColorEmoji.ttf"), b"updated-colrv1").unwrap();

        let xml_text = r#"<?xml version="1.0" encoding="utf-8"?>
<familyset version="23">
  <family name="sans-serif">
    <font weight="400" style="normal">Roboto-Regular.ttf</font>
  </family>
  <family name="serif">
    <font weight="400" style="normal">NotoSerif-Regular.ttf</font>
  </family>
  <family name="monospace">
    <font weight="400" style="normal">DroidSansMono.ttf</font>
  </family>
  <alias name="arial" to="sans-serif" />
  <alias name="sans-serif-medium" to="sans-serif" weight="500" />
  <family lang="und-Arab">
    <font weight="400" style="normal">NotoSansArabic-Regular.ttf</font>
  </family>
  <family lang="zh-Hans">
    <font weight="400" style="normal" index="2" postScriptName="NotoSansCJKJP-Regular">NotoSansCJK-Regular.ttc</font>
  </family>
  <family lang="zh-Hant,zh-Bopo">
    <font weight="400" style="normal" index="3" postScriptName="NotoSansCJKJP-Regular">NotoSansCJK-Regular.ttc</font>
  </family>
  <family lang="zh-Hant-HK">
    <font weight="400" style="normal" index="4">NotoSansCJK-Regular.ttc</font>
  </family>
  <family lang="ja">
    <font weight="400" style="normal" index="0">NotoSansCJK-Regular.ttc</font>
  </family>
  <family lang="ko">
    <font weight="400" style="normal" index="1">NotoSansCJK-Regular.ttc</font>
  </family>
  <family lang="und-Zsye">
    <font weight="400" style="normal">NotoColorEmoji.ttf</font>
    <font weight="400" style="normal">NotoColorEmojiFlags.ttf</font>
  </family>
  <family lang="und-Zmth">
    <font weight="400" style="normal">NotoSansMath-Regular.ttf</font>
  </family>
  <family lang="und-Zsym">
    <font weight="400" style="normal">NotoSansSymbols-Regular-Subsetted.ttf</font>
  </family>
</familyset>"#;

        let xml_path = root.join("font_fallback.xml");
        fs::write(&xml_path, xml_text).unwrap();

        let cfg = AndroidFontConfig::discover_from_xml_and_dirs(
            &[xml_path],
            &[upd_fonts.clone(), sys_fonts.clone()],
            "zh-CN",
        );

        assert_eq!(
            cfg.generic_faces.get(&GenericRole::SansSerif).unwrap()[0],
            FontFileRef::new(sys_fonts.join("Roboto-Regular.ttf"), 0)
        );
        assert_eq!(
            cfg.generic_faces.get(&GenericRole::Monospace).unwrap()[0],
            FontFileRef::new(sys_fonts.join("DroidSansMono.ttf"), 0)
        );
        // Updatable font dir took precedence for NotoColorEmoji.ttf, and Flags is also included.
        let emoji = cfg.generic_faces.get(&GenericRole::Emoji).unwrap();
        assert_eq!(emoji[0], FontFileRef::new(upd_fonts.join("NotoColorEmoji.ttf"), 0));
        assert_eq!(emoji[1], FontFileRef::new(sys_fonts.join("NotoColorEmojiFlags.ttf"), 0));

        // TTC collection indices are preserved per CJK locale, and default Hani prefers zh-Hans (index 2).
        let cjk = sys_fonts.join("NotoSansCJK-Regular.ttc");
        assert_eq!(cfg.script_faces.get(b"Hani").unwrap()[0], FontFileRef::new(&cjk, 2));
        assert_eq!(
            cfg.locale_script_faces.get(&(*b"Hani", "zh-Hans".into())).unwrap()[0],
            FontFileRef::new(&cjk, 2)
        );
        assert_eq!(
            cfg.locale_script_faces.get(&(*b"Hani", "zh-Hant".into())).unwrap()[0],
            FontFileRef::new(&cjk, 3)
        );
        assert_eq!(
            cfg.locale_script_faces.get(&(*b"Hani", "zh-HK".into())).unwrap()[0],
            FontFileRef::new(&cjk, 4)
        );
        assert_eq!(
            cfg.locale_script_faces.get(&(*b"Hani", "ja".into())).unwrap()[0],
            FontFileRef::new(&cjk, 0)
        );
        assert_eq!(
            cfg.locale_script_faces.get(&(*b"Hani", "ko".into())).unwrap()[0],
            FontFileRef::new(&cjk, 1)
        );
        assert_eq!(cfg.script_faces.get(b"Arab").unwrap()[0], FontFileRef::new(sys_fonts.join("NotoSansArabic-Regular.ttf"), 0));
        assert_eq!(cfg.aliases.get("arial").map(String::as_str), Some("sans-serif"));
        assert!(!cfg.aliases.contains_key("sans-serif-medium"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_directories_when_xml_is_missing_or_unreadable() {
        let root = temp_dir("no-xml");
        let sys_fonts = root.join("system_fonts");
        let nested = root.join("data_fonts").join("uuid-1");
        fs::create_dir_all(&sys_fonts).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(sys_fonts.join("Roboto-Regular.ttf"), b"stub").unwrap();
        fs::write(nested.join("NotoColorEmoji.ttf"), b"stub").unwrap();

        let cfg = AndroidFontConfig::discover_from_xml_and_dirs(
            &[root.join("nonexistent-fonts.xml")],
            &[root.join("data_fonts"), sys_fonts.clone()],
            "en-US",
        );

        assert!(cfg.font_files.contains(&nested.join("NotoColorEmoji.ttf")));
        assert!(cfg.font_files.contains(&sys_fonts.join("Roboto-Regular.ttf")));
        let _ = fs::remove_dir_all(&root);
    }
}
