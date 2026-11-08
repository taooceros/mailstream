//! Outgoing messages as RFC 5322 bytes with CRLF line ends, as the Swift engine's `MIMEMessage`
//! (`Sources/MailstreamKit/Backend/MIMEMessage.swift`) writes them: the raw message Gmail's upload
//! endpoint sends and Graph's MIME `sendMail` takes. Both providers drop the Bcc header when they
//! send.
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::model::Address;

/// A message to send. `references` and `in_reply_to` are normalized Message-IDs (no angle
/// brackets); `thread` is the provider's thread of the message replied to.
#[derive(Clone, Debug, Default)]
pub struct OutgoingMessage {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    /// An HTML alternative of `text`, when the reply quotes HTML.
    pub html: Option<String>,
    pub attachments: Vec<PathBuf>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub thread: Option<String>,
}

/// The message under a fresh Message-ID, dated now in the local time zone. Fails only when an
/// attachment cannot be read.
pub fn render(message: &OutgoingMessage) -> Result<Vec<u8>, String> {
    let date = chrono::Local::now().format("%a, %d %b %Y %H:%M:%S %z").to_string();
    render_at(message, &date, &format!("{}@mailstream.local", uuid()), &mut || boundary("=_ms_", true))
}

/// `render` with the varying parts given, so tests can compare whole messages.
fn render_at(
    message: &OutgoingMessage,
    date: &str,
    message_id: &str,
    boundaries: &mut dyn FnMut() -> String,
) -> Result<Vec<u8>, String> {
    let mut headers = vec![address_field("From", std::slice::from_ref(&message.from))];
    if !message.to.is_empty() {
        headers.push(address_field("To", &message.to));
    }
    if !message.cc.is_empty() {
        headers.push(address_field("Cc", &message.cc));
    }
    if !message.bcc.is_empty() {
        headers.push(address_field("Bcc", &message.bcc));
    }
    headers.push(text_field("Subject", &message.subject));
    headers.push(format!("Date: {date}"));
    headers.push(format!("Message-ID: <{message_id}>"));
    if let Some(in_reply_to) = message.in_reply_to.as_deref().filter(|id| !id.is_empty()) {
        headers.push(format!("In-Reply-To: {}", angle(in_reply_to)));
    }
    if !message.references.is_empty() {
        let ids: Vec<String> = message.references.iter().map(|id| angle(id)).collect();
        headers.push(list_field("References", &ids, ""));
    }
    headers.push("MIME-Version: 1.0".into());

    let plain = Entity {
        headers: vec!["Content-Type: text/plain; charset=utf-8".into(), "Content-Transfer-Encoding: quoted-printable".into()],
        body: quoted_printable(&message.text).into_bytes(),
    };
    let mut body = plain;
    if let Some(html) = &message.html {
        let rich = Entity {
            headers: vec!["Content-Type: text/html; charset=utf-8".into(), "Content-Transfer-Encoding: base64".into()],
            body: base64_lines(html.as_bytes()),
        };
        body = multipart("alternative", vec![body, rich], boundaries());
    }
    if !message.attachments.is_empty() {
        let mut parts = vec![body];
        for path in &message.attachments {
            parts.push(attachment(path)?);
        }
        body = multipart("mixed", parts, boundaries());
    }
    headers.append(&mut body.headers);
    body.headers = headers;
    Ok(body.bytes())
}

struct Entity {
    headers: Vec<String>,
    body: Vec<u8>,
}

impl Entity {
    fn bytes(&self) -> Vec<u8> {
        let mut data = self.headers.join("\r\n").into_bytes();
        data.extend_from_slice(b"\r\n\r\n");
        data.extend_from_slice(&self.body);
        data
    }
}

/// A random UUID (version 4), lowercase, as `UUID().uuidString.lowercased()` writes one.
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// A fresh multipart boundary: `prefix` and 32 random hex digits, lowercase unless not `lowercase`.
pub fn boundary(prefix: &str, lowercase: bool) -> String {
    let digits = uuid().replace('-', "");
    format!("{prefix}{}", if lowercase { digits } else { digits.to_uppercase() })
}

/// Boundaries start with `=_`, which neither quoted-printable nor base64 output contains.
fn multipart(subtype: &str, parts: Vec<Entity>, boundary: String) -> Entity {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(&part.bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Entity { headers: vec![format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"")], body }
}

fn attachment(path: &std::path::Path) -> Result<Entity, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = file_name.nfc().filter(|c| !is_newline(*c)).collect();
    let extension = path.extension().map(|extension| extension.to_string_lossy().into_owned()).unwrap_or_default();
    let mime = mime_type(&extension).unwrap_or_else(|| "application/octet-stream".into());
    let (type_name, filename) = if is_plain_ascii(&name) {
        (quoted(&name), format!("filename={}", quoted(&name)))
    } else {
        (format!("\"{}\"", encoded_words(&name).join("\r\n ")), format!("filename*=UTF-8''{}", percent_encoded(&name)))
    };
    Ok(Entity {
        headers: vec![
            format!("Content-Type: {mime};\r\n name={type_name}"),
            format!("Content-Disposition: attachment;\r\n {filename}"),
            "Content-Transfer-Encoding: base64".into(),
        ],
        body: base64_lines(&bytes),
    })
}

/// The MIME type macOS names for files with `extension` (`UTType(filenameExtension:)`, conforming
/// to data); nil when it knows none.
#[cfg(target_os = "macos")]
pub fn mime_type(extension: &str) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        static kUTTagClassMIMEType: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(class: CFStringRef, tag: CFStringRef, conforming: CFStringRef) -> CFStringRef;
        fn UTTypeCopyPreferredTagWithClass(identifier: CFStringRef, class: CFStringRef) -> CFStringRef;
    }
    if extension.is_empty() {
        return None;
    }
    let tag = CFString::new(extension);
    let data = CFString::from_static_string("public.data");
    // SAFETY: the tag classes are constant strings of CoreServices; both calls follow the create
    // rule, so the results are owned here and released when dropped.
    unsafe {
        let identifier =
            UTTypeCreatePreferredIdentifierForTag(kUTTagClassFilenameExtension, tag.as_concrete_TypeRef(), data.as_concrete_TypeRef());
        if identifier.is_null() {
            return None;
        }
        let identifier = CFString::wrap_under_create_rule(identifier);
        let mime = UTTypeCopyPreferredTagWithClass(identifier.as_concrete_TypeRef(), kUTTagClassMIMEType);
        (!mime.is_null()).then(|| CFString::wrap_under_create_rule(mime).to_string())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn mime_type(extension: &str) -> Option<String> {
    mime_guess::from_ext(extension).first_raw().map(String::from)
}

// MARK: - Header fields

fn address_field(name: &str, addresses: &[Address]) -> String {
    let items: Vec<String> = addresses.iter().map(mailbox).collect();
    list_field(name, &items, ",")
}

/// Characters as Swift counts them: extended grapheme clusters.
fn count(text: &str) -> usize {
    text.graphemes(true).count()
}

/// `name: item, item …`, folded between items to keep lines near 78 columns.
fn list_field(name: &str, items: &[String], separator: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = format!("{name}:");
    let name_count = count(name);
    for (index, item) in items.iter().enumerate() {
        let piece = if index + 1 < items.len() { format!("{item}{separator}") } else { item.clone() };
        if count(&line) + 1 + count(&piece) > 78 && count(&line) > name_count + 1 {
            lines.push(std::mem::take(&mut line));
        }
        line.push(' ');
        line.push_str(&piece);
    }
    lines.push(line);
    lines.join("\r\n")
}

/// The line breaks Swift's `Character.isNewline` knows.
fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Unstructured text (Subject): encoded-words when not plain ASCII, else folded at spaces.
fn text_field(name: &str, value: &str) -> String {
    let value = value.split(is_newline).filter(|piece| !piece.is_empty()).collect::<Vec<_>>().join(" ");
    if !is_plain_ascii(&value) || value.contains("=?") {
        return format!("{name}: {}", encoded_words(&value).join("\r\n "));
    }
    let words: Vec<String> = value.split(' ').map(String::from).collect();
    list_field(name, &words, "")
}

fn mailbox(address: &Address) -> String {
    let name = address.name.as_deref().unwrap_or("");
    let name = name.split(|c| is_newline(c) || c == '\t').filter(|piece| !piece.is_empty()).collect::<Vec<_>>().join(" ");
    let name = name.trim_matches(|c| c == ' ' || c == '\t');
    if name.is_empty() || name == address.email {
        return address.email.clone();
    }
    format!("{} <{}>", phrase(name), address.email)
}

const ATEXT_SYMBOLS: &str = "!#$%&'*+-/=?^_`{|}~";

/// A display name: atoms as they are, other ASCII quoted, anything else encoded-words.
fn phrase(text: &str) -> String {
    if !is_plain_ascii(text) {
        return encoded_words(text).join(" ");
    }
    let atom = !text.contains("=?") && text.chars().all(|c| c == ' ' || c.is_ascii_alphanumeric() || ATEXT_SYMBOLS.contains(c));
    if atom { text.to_string() } else { quoted(text) }
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn is_plain_ascii(text: &str) -> bool {
    text.chars().all(|c| (' '..'\u{7f}').contains(&c))
}

/// RFC 2047 B-encoded UTF-8 words of at most 75 characters, split between characters.
fn encoded_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chunk: Vec<u8> = Vec::new();
    for character in text.graphemes(true) {
        if chunk.len() + character.len() > 45 && !chunk.is_empty() {
            words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
            chunk.clear();
        }
        chunk.extend_from_slice(character.as_bytes());
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
    }
    words
}

/// RFC 2231 value characters; everything else as `%XX` of its UTF-8 bytes.
fn percent_encoded(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn angle(id: &str) -> String {
    format!("<{}>", id.trim_matches(|c| matches!(c, '<' | '>' | ' ' | '\t' | '\r' | '\n')))
}

// MARK: - Bodies

/// Base64 in lines of 76 characters ending in CRLF, the last one without.
fn base64_lines(data: &[u8]) -> Vec<u8> {
    let encoded = STANDARD.encode(data);
    let lines: Vec<&str> =
        encoded.as_bytes().chunks(76).map(|line| std::str::from_utf8(line).expect("base64 is ASCII")).collect();
    lines.join("\r\n").into_bytes()
}

/// Quoted-printable UTF-8 with CRLF line ends and soft breaks before column 76.
fn quoted_printable(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::new();
    for (line_index, line) in normalized.split('\n').enumerate() {
        if line_index > 0 {
            out.push_str("\r\n");
        }
        let bytes = line.as_bytes();
        let mut length = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            let literal = match byte {
                b' ' | b'\t' => index + 1 < bytes.len(),
                33..=60 | 62..=126 => true,
                _ => false,
            };
            let token = if literal { (byte as char).to_string() } else { format!("={byte:02X}") };
            if length + token.len() > 75 {
                out.push_str("=\r\n");
                length = 0;
            }
            length += token.len();
            out.push_str(&token);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address::new(name.map(String::from), email)
    }

    /// Boundaries numbered from 1, so whole messages compare.
    fn numbered() -> impl FnMut() -> String {
        let mut next = 0;
        move || {
            next += 1;
            format!("=_ms_{next:032}")
        }
    }

    #[test]
    fn writes_headers_folded_and_encoded() {
        let message = OutgoingMessage {
            from: address(Some("Ann Example"), "ann@x.com"),
            to: (0..6).map(|n| address(Some(&format!("Person, {n}")), &format!("person{n}@example.com"))).collect(),
            cc: vec![address(Some("Zoë"), "zoe@x.com"), address(Some("zoe@x.com"), "zoe@x.com")],
            subject: "A subject long enough that it has to fold somewhere past the seventy-eighth column".into(),
            text: "Hi".into(),
            ..OutgoingMessage::default()
        };
        let mime = String::from_utf8(render_at(&message, "D", "id@mailstream.local", &mut numbered()).unwrap()).unwrap();
        let head = mime.split("\r\n\r\n").next().unwrap();
        assert_eq!(
            head,
            "From: Ann Example <ann@x.com>\r\n\
             To: \"Person, 0\" <person0@example.com>, \"Person, 1\" <person1@example.com>,\r\n \
             \"Person, 2\" <person2@example.com>, \"Person, 3\" <person3@example.com>,\r\n \
             \"Person, 4\" <person4@example.com>, \"Person, 5\" <person5@example.com>\r\n\
             Cc: =?UTF-8?B?Wm/Dqw==?= <zoe@x.com>, zoe@x.com\r\n\
             Subject: A subject long enough that it has to fold somewhere past the\r\n seventy-eighth column\r\n\
             Date: D\r\n\
             Message-ID: <id@mailstream.local>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: quoted-printable"
        );
        let subject = text_field("Subject", "Grüße aus Köln, und noch ein bisschen mehr Text dazu\nzweite Zeile");
        assert_eq!(
            subject,
            "Subject: =?UTF-8?B?R3LDvMOfZSBhdXMgS8O2bG4sIHVuZCBub2NoIGVpbiBiaXNzY2hlbiBtZWhy?=\r\n =?UTF-8?B?IFRleHQgZGF6dSB6d2VpdGUgWmVpbGU=?="
        );
    }

    #[test]
    fn encodes_bodies_quoted_printable() {
        let long = "x".repeat(80);
        assert_eq!(quoted_printable(&format!("caf\u{e9} = ok \r\n{long}\nend\t")), format!(
            "caf=C3=A9 =3D ok=20\r\n{}=\r\n{}\r\nend=09",
            "x".repeat(75),
            "x".repeat(5)
        ));
        assert_eq!(base64_lines(&[0u8; 60]).iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(percent_encoded("Grüße.pdf"), "Gr%C3%BC%C3%9Fe.pdf");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn knows_mime_types_as_macos_does() {
        assert_eq!(mime_type("pdf").as_deref(), Some("application/pdf"));
        assert_eq!(mime_type("PNG").as_deref(), Some("image/png"));
        assert_eq!(mime_type("xyz"), None);
        assert_eq!(mime_type(""), None);
    }
}
