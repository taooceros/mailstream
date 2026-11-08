//! Header and body text as the Swift engine reads it: subject lines (`Model/Subject.swift`),
//! address lists, RFC 2047 words and charsets (`Backend/AddressList.swift`), Message-ID lists
//! (`Backend/MailBackend.swift`) and HTML (`Backend/HTMLText.swift`).
use std::collections::HashSet;
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;

use crate::model::Address;

// MARK: - Subjects

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefix {
    Reply,
    Forward,
}

/// A subject line taken apart: leading reply/forward prefixes in the languages mail clients write
/// them (`Re:`, `AW:`, `回复：`, `Re[2]:`) and `[list]` tags.
pub struct Subject {
    pub prefix: Option<Prefix>,
    /// `[list]` tags; empty when none.
    pub list: String,
    /// Without prefixes and list tags; empty when the subject is nothing but prefixes.
    pub title: String,
}

const PREFIX_WORDS: [(&str, Prefix); 14] = [
    ("re", Prefix::Reply),
    ("aw", Prefix::Reply),
    ("sv", Prefix::Reply),
    ("vs", Prefix::Reply),
    ("rif", Prefix::Reply),
    ("回复", Prefix::Reply),
    ("答复", Prefix::Reply),
    ("回覆", Prefix::Reply),
    ("fwd", Prefix::Forward),
    ("fw", Prefix::Forward),
    ("wg", Prefix::Forward),
    ("tr", Prefix::Forward),
    ("转发", Prefix::Forward),
    ("轉寄", Prefix::Forward),
];

impl Subject {
    pub fn parse(subject: &str) -> Self {
        let raw = subject.trim();
        let mut text = raw;
        let mut prefix = drop_prefixes(&mut text);
        let mut list = String::new();
        // Lists tag the subject in front of the prefixes or after them; tags with nothing after
        // them are the title.
        if let Some((tags, rest)) = leading_list_tags(text) {
            let mut rest = rest;
            let inner = drop_prefixes(&mut rest);
            if !rest.chars().all(char::is_whitespace) {
                list = tags.to_string();
                prefix = prefix.or(inner);
                text = rest;
            }
        }
        Self { prefix, list, title: text.trim().to_string() }
    }
}

fn drop_prefixes(text: &mut &str) -> Option<Prefix> {
    let mut first = None;
    while let Some((kind, rest)) = leading_prefix(text) {
        first = first.or(Some(kind));
        *text = rest;
    }
    first
}

/// The `[list]` tags `text` starts with (each 1–40 characters on one line) and what follows.
fn leading_list_tags(text: &str) -> Option<(&str, &str)> {
    let mut rest = text.trim_start();
    let start = text.len() - rest.len();
    let mut end = start;
    while let Some(after) = rest.strip_prefix('[') {
        let inner = after.find([']', '\n']).map_or(after, |index| &after[..index]);
        let count = inner.chars().count();
        if !(1..=40).contains(&count) || !after[inner.len()..].starts_with(']') {
            break;
        }
        let consumed = 1 + inner.len() + 1;
        end = text.len() - rest.len() + consumed;
        rest = rest[consumed..].trim_start();
    }
    (end != start).then(|| (&text[start..end], rest))
}

/// The prefix `text` starts with (a word, an optional `[2]` or `(2)` count, a colon) and what follows.
fn leading_prefix(text: &str) -> Option<(Prefix, &str)> {
    let start = text.trim_start();
    for (word, kind) in PREFIX_WORDS {
        let count = word.chars().count();
        let head: String = start.chars().take(count).collect();
        if head.to_lowercase() != word {
            continue;
        }
        let mut rest = start[head.len()..].trim_start();
        if let Some(open) = rest.chars().next().filter(|c| *c == '[' || *c == '(') {
            let after = &rest[1..];
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            let close = if open == '[' { ']' } else { ')' };
            if digits == 0 || !after[digits..].starts_with(close) {
                continue;
            }
            rest = after[digits + 1..].trim_start();
        }
        if let Some(after) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：')) {
            return Some((kind, after));
        }
    }
    None
}

// MARK: - Message-IDs

pub fn normalize_message_id(raw: &str) -> String {
    raw.trim_matches(|c| matches!(c, '<' | '>' | ' ' | '\t' | '\r' | '\n')).to_lowercase()
}

static BRACKETED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>]+)>").unwrap());

/// Normalized ids in a Message-ID list header (`In-Reply-To`, `References`): the bracketed
/// ones, else whitespace-separated bare ids.
pub fn parse_message_id_list(header: Option<&str>) -> Vec<String> {
    let Some(header) = header.filter(|header| !header.is_empty()) else { return Vec::new() };
    let bracketed: Vec<&str> = BRACKETED.captures_iter(header).map(|found| found.get(1).unwrap().as_str()).collect();
    let raw = if bracketed.is_empty() { header.split_whitespace().collect() } else { bracketed };
    raw.into_iter().map(normalize_message_id).filter(|id| !id.is_empty()).collect()
}

// MARK: - Address lists

/// RFC 5322 address-list headers (`To`, `Cc`, `From`, `Reply-To`) as provider APIs hand them
/// over: display names quoted or bare, commas inside quotes, comments, groups, bare addresses.
pub fn parse_address_list(header: &str) -> Vec<Address> {
    let mut addresses = Vec::new();
    let (mut phrase, mut comment) = (String::new(), String::new());
    let mut angle: Option<String> = None;
    let mut in_angle = false;
    let mut finish = |phrase: &mut String, comment: &mut String, angle: &mut Option<String>| {
        let name = collapse(&decode_header(phrase));
        let comment_name = collapse(&decode_header(comment));
        if let Some(inside) = angle.take() {
            let email = inside.strip_prefix("mailto:").unwrap_or(&inside).to_string();
            if email.contains('@') {
                let display = if name.is_empty() { comment_name } else { name };
                addresses.push(Address::new((!display.is_empty()).then_some(display), email));
            }
        } else {
            let email: String = phrase.chars().filter(|c| !c.is_whitespace()).collect();
            if email.contains('@') {
                addresses.push(Address::new((!comment_name.is_empty()).then_some(comment_name), email));
            }
        }
        phrase.clear();
        comment.clear();
    };
    let characters: Vec<char> = header.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if in_angle {
            if character == '>' {
                in_angle = false;
            } else if !character.is_whitespace()
                && let Some(angle) = angle.as_mut()
            {
                angle.push(character);
            }
            index += 1;
            continue;
        }
        match character {
            '"' => {
                index += 1;
                while index < characters.len() && characters[index] != '"' {
                    if characters[index] == '\\' && index + 1 < characters.len() {
                        index += 1;
                    }
                    phrase.push(characters[index]);
                    index += 1;
                }
            }
            '(' => {
                let mut depth = 1;
                index += 1;
                while index < characters.len() {
                    let inner = characters[index];
                    if inner == '\\' && index + 1 < characters.len() {
                        index += 1;
                        comment.push(characters[index]);
                    } else if inner == '(' {
                        depth += 1;
                        comment.push(inner);
                    } else if inner == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        comment.push(inner);
                    } else {
                        comment.push(inner);
                    }
                    index += 1;
                }
            }
            '<' => {
                in_angle = true;
                angle = Some(String::new());
            }
            ',' | ';' => {
                finish(&mut phrase, &mut comment, &mut angle);
                in_angle = false;
            }
            ':' => {
                // A group's name; its members follow.
                phrase.clear();
                comment.clear();
            }
            _ => phrase.push(character),
        }
        index += 1;
    }
    finish(&mut phrase, &mut comment, &mut angle);
    addresses
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// MARK: - Header words and charsets

static ENCODED_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"=\?([^?\s]+)\?([BbQq])\?([^?\s]*)\?=").unwrap());

/// Decodes RFC 2047 encoded-words (`=?UTF-8?B?…?=`); text without them is returned as is.
pub fn decode_header(text: &str) -> String {
    if !text.contains("=?") {
        return text.to_string();
    }
    let mut out = String::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut pending_charset: Option<String> = None;
    let mut cursor = 0;
    let flush = |out: &mut String, pending: &mut Vec<u8>, charset: &mut Option<String>| {
        if let Some(name) = charset.take() {
            out.push_str(&decode_charset(pending, Some(&name)));
            pending.clear();
        }
    };
    for found in ENCODED_WORD.captures_iter(text) {
        let whole = found.get(0).unwrap();
        let between = &text[cursor..whole.start()];
        // Whitespace between two encoded-words is not part of the text.
        if pending_charset.is_none() || !between.chars().all(char::is_whitespace) {
            flush(&mut out, &mut pending, &mut pending_charset);
            out.push_str(between);
        }
        let declared = &found[1];
        let charset = declared.split('*').next().unwrap_or(declared).to_lowercase();
        if pending_charset.as_deref() != Some(charset.as_str()) {
            flush(&mut out, &mut pending, &mut pending_charset);
        }
        pending_charset = Some(charset);
        let payload = &found[3];
        if found[2].eq_ignore_ascii_case("b") {
            pending.extend(base64_url_decode(payload).unwrap_or_default());
        } else {
            pending.extend(quoted_word(payload));
        }
        cursor = whole.end();
    }
    flush(&mut out, &mut pending, &mut pending_charset);
    out.push_str(&text[cursor..]);
    out
}

fn quoted_word(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'_' {
            out.push(b' ');
        } else if byte == b'='
            && index + 2 < bytes.len()
            && let Some(value) = std::str::from_utf8(&bytes[index + 1..=index + 2])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(value);
            index += 2;
        } else {
            out.push(byte);
        }
        index += 1;
    }
    out
}

/// `data` in `charset` (IANA name), else UTF-8, else ISO-8859-1. Charsets senders commonly
/// mislabel read as their supersets (GB2312 as GB18030, Latin-1 as Windows-1252).
pub fn decode_charset(data: &[u8], charset: Option<&str>) -> String {
    if let Some(name) = charset.map(str::to_lowercase) {
        let name = match name.as_str() {
            "gb2312" | "gbk" | "x-gbk" => "gb18030",
            "iso-8859-1" | "latin1" => "windows-1252",
            other => other,
        };
        if matches!(name, "us-ascii" | "ascii") {
            if data.is_ascii() {
                return String::from_utf8_lossy(data).into_owned();
            }
        } else if let Some(encoding) = encoding_rs::Encoding::for_label(name.as_bytes())
            && let Some(text) = encoding.decode_without_bom_handling_and_without_replacement(data)
        {
            return text.into_owned();
        }
    }
    match std::str::from_utf8(data) {
        Ok(text) => text.to_string(),
        Err(_) => data.iter().map(|&byte| byte as char).collect(),
    }
}

/// The value of parameter `name` (`charset`, `boundary`) in a `Content-Type`-style header.
pub fn header_parameter(name: &str, header: Option<&str>) -> Option<String> {
    for piece in header?.split(';').skip(1) {
        let Some((key, value)) = piece.split_once('=') else { continue };
        if key.trim().to_lowercase() != name {
            continue;
        }
        let value = value.trim_matches(|c| matches!(c, '"' | ' ' | '\t' | '\r' | '\n'));
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

static BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireCanonical),
);

/// Base64url or plain base64, padded or not; characters outside the alphabet are skipped.
pub fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut clean: String = text
        .chars()
        .filter_map(|c| match c {
            '-' => Some('+'),
            '_' => Some('/'),
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            _ => None,
        })
        .collect();
    let padding = (4 - clean.len() % 4) % 4;
    clean.extend(std::iter::repeat_n('=', padding));
    BASE64.decode(clean).ok()
}

// MARK: - HTML

pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Plain text as HTML with its links (web addresses and email addresses) linked; the text
/// between them escaped.
pub fn linkify(text: &str) -> String {
    if !text.contains(['.', ':']) {
        return escape_html(text);
    }
    let mut finder = linkify::LinkFinder::new();
    finder.url_must_have_scheme(false);
    let mut out = String::with_capacity(text.len());
    for span in finder.spans(text) {
        match span.kind() {
            Some(kind) => {
                let shown = span.as_str();
                let target = match kind {
                    linkify::LinkKind::Email => format!("mailto:{shown}"),
                    _ if shown.contains("://") || shown.to_lowercase().starts_with("mailto:") => shown.to_string(),
                    _ => format!("http://{shown}"),
                };
                out.push_str(&format!("<a href=\"{}\">{}</a>", escape_html(&target), escape_html(shown)));
            }
            None => out.push_str(&escape_html(span.as_str())),
        }
    }
    out
}

static REMOTE_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(src|background|poster)\s*=\s*["']?\s*(https?:)?//|url\(\s*["']?\s*(https?:)?//|<link[^>]+href\s*=\s*["']?\s*https?:"#,
    )
    .unwrap()
});

/// Whether `html` references remote images, styles or fonts.
pub fn has_remote_content(html: &str) -> bool {
    REMOTE_CONTENT.is_match(html)
}

static MOBILE_FOOTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(Sent from my |Get Outlook for |发自我的)").unwrap());

/// Plain text to HTML: linkified, whitespace kept, `>` quote runs in blockquotes and signatures
/// (after a `-- ` line, or a lone mobile footer) in `.ms-signature`, for the reader to fold.
pub fn plain_to_html(text: &str) -> String {
    let mut html = String::from("<div class=\"ms-plain\">");
    let mut quote_depth = 0;
    let mut in_signature = false;
    for raw_line in text.replace("\r\n", "\n").split('\n') {
        let mut line = raw_line;
        let mut depth = 0;
        while let Some(rest) = line.strip_prefix('>') {
            depth += 1;
            line = rest.strip_prefix(' ').unwrap_or(rest);
        }
        if in_signature && depth > 0 {
            html.push_str("</div>");
            in_signature = false;
        }
        while quote_depth < depth {
            html.push_str("<blockquote class=\"ms-quote\">");
            quote_depth += 1;
        }
        while quote_depth > depth {
            html.push_str("</blockquote>");
            quote_depth -= 1;
        }
        let trimmed = trim_spaces(line);
        if depth == 0 && !in_signature && trimmed == "--" {
            html.push_str("<div class=\"ms-signature\">");
            in_signature = true;
        }
        if depth == 0 && !in_signature && trimmed.chars().count() < 40 && MOBILE_FOOTER.is_match(trimmed) {
            html.push_str(&format!("<span class=\"ms-signature\">{}</span>\n", escape_html(line)));
        } else {
            html.push_str(&linkify(line));
            html.push('\n');
        }
    }
    if in_signature {
        html.push_str("</div>");
    }
    while quote_depth > 0 {
        html.push_str("</blockquote>");
        quote_depth -= 1;
    }
    html.push_str("</div>");
    html
}

static STRIP_BLOCKS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    ["style", "script", "head", "title"].map(|block| Regex::new(&format!(r"(?i)<{block}[^>]*>[\s\S]*?</{block}>")).unwrap())
});
static STRIP_COMMENTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<!--[\s\S]*?-->").unwrap());
static STRIP_BREAKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<(br|/p|/div|/tr|/li|/h[1-6])[^>]*>").unwrap());
static STRIP_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Visible text of an HTML document.
pub fn strip_html(html: &str) -> String {
    let mut text = html.to_string();
    for block in STRIP_BLOCKS.iter() {
        text = block.replace_all(&text, " ").into_owned();
    }
    text = STRIP_COMMENTS.replace_all(&text, " ").into_owned();
    text = STRIP_BREAKS.replace_all(&text, "\n").into_owned();
    text = STRIP_TAGS.replace_all(&text, " ").into_owned();
    decode_entities(&text)
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" | "ensp" | "emsp" | "thinsp" => " ",
        "zwnj" | "zwj" | "shy" => "",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "rsquo" => "’",
        "lsquo" => "‘",
        "rdquo" => "”",
        "ldquo" => "“",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "middot" => "·",
        "bull" => "•",
        _ => return None,
    })
}

/// HTML entities decoded, scalar by scalar.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        let window: String = rest.chars().take(12).collect();
        let Some(semi) = window.find(';') else {
            out.push('&');
            continue;
        };
        let entity = &rest[..semi];
        let replaced = if let Some(digits) = entity.strip_prefix('#') {
            let value = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => digits.parse::<u32>().ok(),
            };
            value.and_then(char::from_u32).map(String::from)
        } else {
            named_entity(&entity.to_lowercase()).map(String::from)
        };
        match replaced {
            Some(replacement) => {
                out.push_str(&replacement);
                rest = &rest[semi + 1..];
            }
            None => out.push('&'),
        }
    }
    out.push_str(rest);
    out
}

static SNIPPET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"<!--[\s\S]*?-->|<!\[endif\]-->|<!--\[if[^\]]*\]>", " "),
        (r"\[([^\]\n]*)\]\(\s*https?://[^)\s]*\s*\)", "$1"),
        (r"[<\[(]\s*(https?://|mailto:)[^>\])\s]*\s*[>\])]", " "),
        (r"(https?://|www\.)\S+", " "),
        (r"\[image:[^\]]*\]", " "),
    ]
    .into_iter()
    .map(|(pattern, template)| (Regex::new(pattern).unwrap(), template))
    .collect()
});

static HISTORY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?s)\s+On\s[^\n]{4,200}?\s(?:wrote|написал)\s*:.*$",
        r"(?s)\s+On\s(?:(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)[a-z]*,?\s+)?(?:\d{1,2}\s+)?(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.?\s+\d{1,2}?,?\s*\d{4}.*$",
        r"(?s)\s+在\s?\d{4}年.*$",
        r"(?s)\s+-{2,}\s*(?:Original Message|Forwarded message).*$",
        r"(?s)\s+From:\s.{1,120}?\s(?:Sent|Date):\s.*$",
        r"(?s)\s+(?:Sent with Spark|Sent from my (?:iPhone|iPad|Android)|Get Outlook for \w+)\b.*$",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).unwrap())
    .collect()
});

const INVISIBLE: [char; 8] =
    ['\u{034F}', '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{00AD}', '\u{180E}'];

/// One-paragraph preview of at most 240 characters: quotes, attributions, link targets and
/// preheader padding removed.
pub fn snippet(text: &str) -> String {
    let head: String = text.chars().take(8000).collect();
    let mut source = decode_entities(&head);
    for (pattern, template) in SNIPPET_PATTERNS.iter() {
        source = pattern.replace_all(&source, *template).into_owned();
    }
    let mut lines: Vec<&str> = Vec::new();
    for line in source.split(['\n', '\r', '\u{0B}', '\u{0C}', '\u{85}', '\u{2028}', '\u{2029}']).filter(|l| !l.is_empty())
    {
        let trimmed = trim_spaces(line);
        if trimmed.starts_with('>') {
            continue;
        }
        if (trimmed.starts_with("On ") && trimmed.ends_with("wrote:"))
            || trimmed.starts_with("-----Original Message")
            || trimmed.starts_with("---------- Forwarded")
            || (trimmed.starts_with('在') && (trimmed.ends_with("写道：") || trimmed.ends_with("写道:")))
        {
            break;
        }
        lines.push(line);
        if lines.len() > 40 {
            break;
        }
    }
    let mut out = String::new();
    let mut count = 0;
    let mut last_was_space = true;
    for c in lines.join(" ").chars() {
        if INVISIBLE.contains(&c) {
            continue;
        }
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                count += 1;
            }
            last_was_space = true;
        } else {
            out.push(c);
            count += 1;
            last_was_space = false;
        }
        if count >= 240 {
            break;
        }
    }
    trim_history(trim_spaces(&out))
}

/// `text` without the spaces and tabs (`CharacterSet.whitespaces`) around it; line breaks stay.
pub fn trim_spaces(text: &str) -> &str {
    text.trim_matches(|c: char| {
        c.is_whitespace() && !matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
    })
}

fn trim_history(snippet: &str) -> String {
    let mut text = snippet.to_string();
    for pattern in HISTORY_PATTERNS.iter() {
        if let Some(found) = pattern.find(&text)
            && found.start() > 0
        {
            text.replace_range(found.range(), "");
        }
    }
    text
}

pub fn file_extension(mime: &str) -> Option<&'static str> {
    Some(match mime.to_lowercase().as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/calendar" => "ics",
        "text/html" => "html",
        "text/plain" => "txt",
        "message/rfc822" => "eml",
        _ => return None,
    })
}

/// The first `count` characters (grapheme clusters, as Swift counts) of `text`, and whether
/// that cut anything.
pub fn prefix_characters(text: &str, count: usize) -> (&str, bool) {
    use unicode_segmentation::UnicodeSegmentation;
    match text.grapheme_indices(true).nth(count) {
        Some((end, _)) => (&text[..end], true),
        None => (text, false),
    }
}

/// Lowercased addresses, deduplicated.
pub fn lowercased(addresses: &[Address]) -> HashSet<String> {
    addresses.iter().map(Address::normalized_email).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_subjects_apart() {
        let subject = Subject::parse("Re: [swift-evolution] AW: Pitch");
        assert_eq!((subject.prefix, subject.list.as_str(), subject.title.as_str()), (Some(Prefix::Reply), "[swift-evolution]", "Pitch"));
        let subject = Subject::parse("回复：会议");
        assert_eq!((subject.prefix, subject.title.as_str()), (Some(Prefix::Reply), "会议"));
        let subject = Subject::parse("Fwd[2]: Re(3): x");
        assert_eq!((subject.prefix, subject.title.as_str()), (Some(Prefix::Forward), "x"));
        let subject = Subject::parse("[alone]");
        assert_eq!((subject.list.as_str(), subject.title.as_str()), ("", "[alone]"));
        assert_eq!(Subject::parse("Rebate: 10%").prefix, None);
    }

    #[test]
    fn parses_address_lists() {
        let parsed = parse_address_list(r#""Doe, Jane" <jane@x.com>, bob@y.org (Bob B), undisclosed:;, =?UTF-8?B?5byg5LiJ?= <z@c.cn>"#);
        assert_eq!(
            parsed,
            vec![
                Address::new(Some("Doe, Jane".into()), "jane@x.com"),
                Address::new(Some("Bob B".into()), "bob@y.org"),
                Address::new(Some("张三".into()), "z@c.cn"),
            ]
        );
        assert_eq!(parse_address_list("<mailto:a@b.c>"), vec![Address::new(None, "a@b.c")]);
    }

    #[test]
    fn decodes_words_and_charsets() {
        assert_eq!(decode_header("=?utf-8?Q?caf=C3=A9_au?= =?utf-8?Q?_lait?= ok"), "café au lait ok");
        assert_eq!(decode_header("=?GB2312?B?xOO6ww==?="), "你好");
        assert_eq!(decode_charset(&[0xE9], Some("iso-8859-1")), "é");
        assert_eq!(decode_charset("é".as_bytes(), Some("us-ascii")), "é");
        assert_eq!(decode_charset(&[0xE9], Some("utf-8")), "é");
        assert_eq!(header_parameter("charset", Some("text/plain; Charset=\"UTF-8\"")).as_deref(), Some("UTF-8"));
        assert_eq!(parse_message_id_list(Some("<A@B> <c@d>")), vec!["a@b", "c@d"]);
        assert_eq!(base64_url_decode("aGk_").unwrap(), b"hi?");
    }

    #[test]
    fn makes_previews_and_html() {
        assert_eq!(snippet("Hello&nbsp;there\n> quoted\nhttps://x.com/a more\nOn Mon, Bob wrote:\nold"), "Hello there more");
        assert_eq!(snippet("Thanks! On Tue, Jan 2, 2024 at 9:00 AM Bob <b@x.com> wrote: earlier"), "Thanks!");
        assert_eq!(strip_html("<style>p{}</style><p>A&amp;B</p><br>C"), "  A&B\n\nC");
        assert!(has_remote_content(r#"<img src="https://x/y.png">"#));
        assert!(!has_remote_content(r#"<img src="cid:a">"#));
        assert_eq!(
            plain_to_html("Hi see www.x.com\n> q\n-- \nSig"),
            "<div class=\"ms-plain\">Hi see <a href=\"http://www.x.com\">www.x.com</a>\n<blockquote class=\"ms-quote\">q\n</blockquote><div class=\"ms-signature\">-- \nSig\n</div></div>"
        );
        assert_eq!(prefix_characters("e\u{301}ab", 2), ("e\u{301}a", true));
    }
}
