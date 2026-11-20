//! Parser for Android `font_fallback.xml`, `fonts.xml`, `fonts_customization.xml`,
//! and `/data/fonts/config/config.xml`.

use std::path::{Path, PathBuf};

use roxmltree::{Document, Node};

use crate::{AndroidFontConfig, FontFileRef, GenericRole};

/// Parses an Android font configuration XML document and merges its mappings into `config`.
pub fn parse_into(xml: &str, font_dirs: &[PathBuf], config: &mut AndroidFontConfig) {
    let Ok(doc) = Document::parse(xml) else {
        return;
    };
    let root = doc.root_element();
    if root.tag_name().name() != "familyset" {
        return;
    }

    for child in root.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "alias" => parse_alias(child, config),
            "family" => parse_family(child, font_dirs, config),
            _ => {}
        }
    }
}

fn parse_alias(node: Node<'_, '_>, config: &mut AndroidFontConfig) {
    // Skip weight-only aliases (e.g. `sans-serif-medium -> sans-serif weight=500`),
    // which are legacy Android shims rather than family name aliases.
    if node.attribute("weight").is_some() {
        return;
    }
    if let (Some(name), Some(to)) = (node.attribute("name"), node.attribute("to")) {
        config
            .aliases
            .entry(name.to_string())
            .or_insert_with(|| to.to_string());
    }
}

fn parse_family(node: Node<'_, '_>, font_dirs: &[PathBuf], config: &mut AndroidFontConfig) {
    if let Some(name) = node.attribute("name") {
        let Some(role) = generic_role_for_name(name) else {
            // Still record readable font files from named OEM families so they are loaded.
            for font in collect_fonts(node, font_dirs, true) {
                config.push_font_file(font.path);
            }
            return;
        };
        for font in collect_fonts(node, font_dirs, true) {
            config.push_generic(role, font.clone());
            if role == GenericRole::SansSerif {
                config.push_generic(GenericRole::SystemUi, font);
            }
        }
        return;
    }

    let Some(lang_attr) = node.attribute("lang") else {
        // Unnamed, untagged fallback family (e.g. Symbols / Emoji in some OEM XMLs).
        for font in collect_fonts(node, font_dirs, false) {
            if is_likely_emoji_file(&font.path) {
                config.push_generic(GenericRole::Emoji, font.clone());
            }
            config.push_common_fallback(font);
        }
        return;
    };

    let fonts = collect_fonts(node, font_dirs, false);
    if fonts.is_empty() {
        return;
    }

    for raw_tag in lang_attr.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let lower = raw_tag.to_ascii_lowercase().replace('_', "-");
        // Emoji (`und-Zsye`, `und-Zsye-emoji`, etc.)
        if lower.contains("zsye") {
            for font in &fonts {
                config.push_generic(GenericRole::Emoji, font.clone());
                config.push_common_fallback(font.clone());
            }
            continue;
        }
        // Math (`und-Zmth`)
        if lower.contains("zmth") {
            for font in &fonts {
                config.push_generic(GenericRole::Math, font.clone());
                config.push_common_fallback(font.clone());
            }
            continue;
        }
        // Symbols (`und-Zsym`)
        if lower.contains("zsym") {
            for font in &fonts {
                config.push_common_fallback(font.clone());
            }
            continue;
        }

        // CJK locales: map explicitly and ensure `zh-Hans` populates default `Hani` ahead of `zh-Hant`.
        if lower == "zh-hans" || lower.starts_with("zh-hans-") || lower == "zh-cn" || lower == "zh-sg" {
            for font in &fonts {
                config.push_locale_script(*b"Hani", "zh-Hans", font.clone());
                // Ensure Simplified Chinese leads the default `Hani` chain even if `zh-Hant`
                // appeared earlier in the XML (fixes fontique's zh-Hant-only Hani bug).
                let list = config.script_faces.entry(*b"Hani").or_default();
                if let Some(pos) = list.iter().position(|f| f == font) {
                    list.remove(pos);
                }
                list.insert(0, font.clone());
            }
            continue;
        }
        if lower == "zh-hant-hk" || lower == "zh-hk" || lower == "zh-hant-mo" || lower == "zh-mo" {
            for font in &fonts {
                config.push_locale_script(*b"Hani", "zh-HK", font.clone());
            }
            continue;
        }
        if lower == "zh-hant" || lower.starts_with("zh-hant-") || lower == "zh-tw" {
            for font in &fonts {
                config.push_locale_script(*b"Hani", "zh-Hant", font.clone());
                config.push_script(*b"Hani", font.clone());
                config.push_script(*b"Bopo", font.clone());
            }
            continue;
        }
        if lower == "zh-bopo" {
            for font in &fonts {
                config.push_script(*b"Bopo", font.clone());
            }
            continue;
        }
        if lower == "ja" || lower.starts_with("ja-") {
            for font in &fonts {
                config.push_locale_script(*b"Hani", "ja", font.clone());
                config.push_script(*b"Hira", font.clone());
                config.push_script(*b"Kana", font.clone());
            }
            continue;
        }
        if lower == "ko" || lower.starts_with("ko-") {
            for font in &fonts {
                config.push_locale_script(*b"Hani", "ko", font.clone());
                config.push_script(*b"Hang", font.clone());
            }
            continue;
        }

        // General ISO 15924 script tag extraction (`und-Arab`, `pa-Guru`, `sr-Cyrl`, `und-Deva`, ...).
        if let Some(script) = extract_script_tag(raw_tag) {
            for font in &fonts {
                config.push_script(script, font.clone());
            }
        }
    }
}

fn generic_role_for_name(name: &str) -> Option<GenericRole> {
    match name.to_ascii_lowercase().as_str() {
        "sans-serif" => Some(GenericRole::SansSerif),
        "system-ui" => Some(GenericRole::SystemUi),
        "serif" => Some(GenericRole::Serif),
        "monospace" => Some(GenericRole::Monospace),
        "cursive" => Some(GenericRole::Cursive),
        "fantasy" => Some(GenericRole::Fantasy),
        "emoji" => Some(GenericRole::Emoji),
        "math" => Some(GenericRole::Math),
        _ => None,
    }
}

/// Extracts a 4-letter ISO 15924 script tag from a BCP-47 language tag (e.g. `und-Arab` -> `Arab`,
/// `pa-Guru-IN` -> `Guru`).
fn extract_script_tag(tag: &str) -> Option<[u8; 4]> {
    for subtag in tag.split(['-', '_']).skip(1) {
        if subtag.len() == 4 && subtag.bytes().all(|b| b.is_ascii_alphabetic()) {
            let bytes = subtag.as_bytes();
            return Some([
                bytes[0].to_ascii_uppercase(),
                bytes[1].to_ascii_lowercase(),
                bytes[2].to_ascii_lowercase(),
                bytes[3].to_ascii_lowercase(),
            ]);
        }
    }
    None
}

fn collect_fonts(
    family: Node<'_, '_>,
    font_dirs: &[PathBuf],
    include_all_styles: bool,
) -> Vec<FontFileRef> {
    let mut regular = Vec::new();
    let mut others = Vec::new();

    for child in family.children().filter(|n| n.is_element() && n.tag_name().name() == "font") {
        // For fallback families, ignore fonts scoped to a specific family like `fallbackFor="serif"`.
        if !include_all_styles
            && let Some(for_family) = child.attribute("fallbackFor")
            && !for_family.eq_ignore_ascii_case("sans-serif")
        {
            continue;
        }
        let Some(raw_name) = child.text().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(path) = resolve_font_path(raw_name, font_dirs) else {
            continue;
        };
        let index = child
            .attribute("index")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let entry = FontFileRef::new(path, index);
        let weight = child
            .attribute("weight")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(400);
        let is_italic = child
            .attribute("style")
            .is_some_and(|s| s.eq_ignore_ascii_case("italic"));

        if weight == 400 && !is_italic {
            crate::push_unique(&mut regular, entry);
        } else if include_all_styles {
            crate::push_unique(&mut others, entry);
        } else if regular.is_empty() {
            crate::push_unique(&mut others, entry);
        }
    }

    if regular.is_empty() {
        others
    } else if include_all_styles {
        regular.extend(others);
        regular
    } else {
        regular
    }
}

fn resolve_font_path(raw: &str, font_dirs: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    for dir in font_dirs {
        let candidate = dir.join(raw);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn is_likely_emoji_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("emoji"))
}
