//! Parses raw RFC 5322 / MIME messages (from Gmail IMAP `BODY.PEEK[]`, MS-400) into
//! [`MessageContent`] through [`MessageAssembly`], matching [`crate::gmail`]'s REST renderer.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use base64::Engine as _;
use percent_encoding::percent_decode_str;

use crate::assembly::{AttachedMessage, MessageAssembly, Segment, combine};
use crate::model::MessageContent;
use crate::text;

static BASE64_LENIENT: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// Parses a raw RFC 5322 message into [`MessageContent`], storing inline parts and attachments
/// under `directory` and rewriting `cid:` links to `url_base`.
pub fn parse_message(raw: &[u8], directory: PathBuf, url_base: String) -> std::io::Result<MessageContent> {
    let entity = Entity::parse(raw, "text/plain");
    let assembly = MessageAssembly::new(directory, url_base)?;
    let mut renderer = Renderer { assembly, next_index: 0 };
    let body = renderer.render(&entity, false);
    let html = body.as_ref().filter(|segment| segment.primary).and_then(|segment| segment.html.clone());
    let text = body.and_then(|segment| segment.text);
    Ok(renderer.assembly.finish(
        html,
        text,
        text::parse_address_list(&entity.headers_joined("cc", ", ")),
        text::parse_address_list(&entity.headers_joined("reply-to", ", ")),
        text::parse_message_id_list(entity.header("references")),
    ))
}

struct Entity<'a> {
    headers: Vec<(String, String)>,
    body: &'a [u8],
    mime: String,
    type_params: HashMap<String, String>,
}

impl<'a> Entity<'a> {
    fn parse(raw: &'a [u8], default_mime: &str) -> Self {
        let (header_bytes, body) = split_headers_and_body(raw);
        let headers = parse_headers(header_bytes);
        let content_type = headers
            .iter()
            .find(|(name, _)| name == "content-type")
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        let (mime, type_params) = parse_header_value(content_type, default_mime);
        Self { headers, body, mime, type_params }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    fn headers_joined(&self, name: &str, separator: &str) -> String {
        let values: Vec<&str> = self.headers.iter().filter(|(key, _)| key == name).map(|(_, value)| value.as_str()).collect();
        values.join(separator)
    }
}

struct Renderer {
    assembly: MessageAssembly,
    next_index: i64,
}

impl Renderer {
    fn render(&mut self, entity: &Entity<'_>, related: bool) -> Option<Segment> {
        if entity.mime.starts_with("multipart/") {
            let boundary = entity.type_params.get("boundary").map(String::as_str).unwrap_or("");
            if boundary.is_empty() {
                return None;
            }
            let child_default = if entity.mime == "multipart/digest" { "message/rfc822" } else { "text/plain" };
            let parts = split_multipart(entity.body, boundary);
            let children: Vec<Entity<'_>> = parts.into_iter().map(|part| Entity::parse(part, child_default)).collect();
            if entity.mime == "multipart/alternative" {
                let segments = self.render_all(&children, false);
                let html = segments.iter().rev().find(|segment| segment.primary).and_then(|segment| segment.html.clone());
                let text = segments.iter().rev().find(|segment| segment.text.is_some()).and_then(|segment| segment.text.clone());
                return (html.is_some() || text.is_some()).then(|| Segment { primary: html.is_some(), html, text });
            }
            let is_related = entity.mime == "multipart/related";
            let segments = self.render_all(&children, is_related);
            return combine(segments);
        }

        let (disposition, disp_params) = entity
            .header("content-disposition")
            .map(|value| parse_header_value(value, ""))
            .unwrap_or_default();
        let filename = disp_params
            .get("filename")
            .or_else(|| entity.type_params.get("name"))
            .map(String::as_str)
            .unwrap_or("");
        let cte = entity.header("content-transfer-encoding").unwrap_or("");

        if entity.mime == "message/rfc822" {
            return self.attached_message(entity.body, cte, &disposition, filename);
        }

        if (entity.mime == "text/plain" || entity.mime == "text/html")
            && !disposition.starts_with("attachment")
            && filename.is_empty()
        {
            let bytes = decode_transfer_encoding(entity.body, cte);
            let charset = entity.type_params.get("charset").map(String::as_str);
            let decoded = text::decode_charset(&bytes, charset);
            return Some(if entity.mime == "text/html" {
                Segment { html: Some(decoded), text: None, primary: true }
            } else {
                Segment { html: None, text: Some(decoded), primary: false }
            });
        }

        let bytes = decode_transfer_encoding(entity.body, cte);
        let inline = disposition.starts_with("inline");
        let placed = inline && !related && entity.mime.starts_with("image/");
        let index = self.next_index;
        self.next_index += 1;
        let content_id = entity
            .header("content-id")
            .map(String::from)
            .or_else(|| placed.then(|| format!("ms-part-{index}")));
        let attachment = self.assembly.store(
            &bytes,
            index,
            (!filename.is_empty()).then_some(filename),
            &entity.mime,
            content_id.as_deref(),
            inline,
        );
        let cid = attachment.content_id.filter(|cid| placed && text::escape_html(cid) == *cid)?;
        Some(Segment { html: Some(format!("<p><img src=\"cid:{cid}\" style=\"max-width:100%\"></p>")), text: None, primary: false })
    }

    fn render_all(&mut self, entities: &[Entity<'_>], related: bool) -> Vec<Segment> {
        entities.iter().filter_map(|entity| self.render(entity, related)).collect()
    }

    fn attached_message(&mut self, raw: &[u8], cte: &str, disposition: &str, filename: &str) -> Option<Segment> {
        let bytes = decode_transfer_encoding(raw, cte);
        if disposition.starts_with("attachment") {
            let name = if filename.is_empty() { "message.eml" } else { filename };
            self.assembly.store(&bytes, self.next_index, Some(name), "message/rfc822", None, false);
            self.next_index += 1;
            return None;
        }
        let inner = Entity::parse(&bytes, "text/plain");
        let from = text::parse_address_list(inner.header("from").unwrap_or(""));
        let subject = text::decode_header(inner.header("subject").unwrap_or(""));
        let rendered = self.render(&inner, false);
        if rendered.is_none() && from.is_empty() && subject.is_empty() {
            let name = if filename.is_empty() { "message.eml" } else { filename };
            self.assembly.store(&bytes, self.next_index, Some(name), "message/rfc822", None, false);
            self.next_index += 1;
            return None;
        }
        let inner_html = rendered.as_ref().filter(|segment| segment.primary).and_then(|segment| segment.html.clone());
        let inner_text = rendered
            .as_ref()
            .and_then(|segment| segment.text.clone())
            .or_else(|| inner_html.as_deref().map(text::strip_html))
            .unwrap_or_default();
        let attached = AttachedMessage::new(from.first(), &subject, inner_html.as_deref(), &inner_text);
        Some(Segment { primary: attached.has_html, html: Some(attached.html), text: Some(attached.text) })
    }
}

fn split_headers_and_body(raw: &[u8]) -> (&[u8], &[u8]) {
    let mut cursor = 0;
    while cursor < raw.len() {
        let line_start = cursor;
        let line_end = raw[cursor..].iter().position(|&b| b == b'\n').map_or(raw.len(), |pos| cursor + pos);
        let next = if line_end < raw.len() { line_end + 1 } else { raw.len() };
        let stripped = if line_end > line_start && raw[line_end - 1] == b'\r' { line_end - 1 } else { line_end };
        if stripped == line_start {
            return (&raw[..line_start], &raw[next..]);
        }
        cursor = next;
    }
    (raw, &[])
}

fn parse_headers(raw: &[u8]) -> Vec<(String, String)> {
    let text = text::decode_charset(raw, None);
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim_start_matches([' ', '\t']));
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let value = value.trim_start_matches([' ', '\t']).trim_end().to_string();
            headers.push((key, value));
        }
    }
    headers
}

/// Parses `main; param=val; param*="utf-8''..."` (including RFC 2231 continuations `*0`, `*1`).
fn parse_header_value(raw: &str, default_main: &str) -> (String, HashMap<String, String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (default_main.to_string(), HashMap::new());
    }
    let (main_raw, rest) = split_outside_quotes(trimmed, ';');
    let main = main_raw.trim().to_ascii_lowercase();
    let main = if main.is_empty() { default_main.to_string() } else { main };

    let mut plain: HashMap<String, String> = HashMap::new();
    let mut extended: HashMap<String, String> = HashMap::new();
    let mut continuations: HashMap<String, BTreeMap<usize, (bool, String)>> = HashMap::new();

    let mut tail = rest;
    while !tail.is_empty() {
        let (piece, next) = split_outside_quotes(tail, ';');
        tail = next;
        let Some((name_raw, value_raw)) = piece.split_once('=') else { continue };
        let key = name_raw.trim().to_ascii_lowercase();
        let value = unquote_param(value_raw.trim());
        if let Some((base, index_part)) = key.split_once('*') {
            if index_part.is_empty() {
                if let Some(decoded) = decode_rfc2231_value(&value) {
                    extended.insert(base.to_string(), decoded);
                }
            } else {
                let (digits, encoded) = match index_part.strip_suffix('*') {
                    Some(digits) => (digits, true),
                    None => (index_part, false),
                };
                if let Ok(index) = digits.parse::<usize>() {
                    continuations.entry(base.to_string()).or_default().insert(index, (encoded, value));
                }
            }
        } else {
            plain.insert(key, text::decode_header(&value));
        }
    }

    for (base, parts) in continuations {
        if let Some(decoded) = assemble_rfc2231(&parts) {
            extended.entry(base).or_insert(decoded);
        }
    }
    for (base, value) in extended {
        plain.insert(base, value);
    }

    (main, plain)
}

fn split_outside_quotes(input: &str, delimiter: char) -> (&str, &str) {
    let mut in_quotes = false;
    let mut escape = false;
    for (index, ch) in input.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if in_quotes {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_quotes = false;
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delimiter {
            return (&input[..index], &input[index + delimiter.len_utf8()..]);
        }
    }
    (input, "")
}

fn unquote_param(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return raw.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.next()
        {
            out.push(next);
        } else {
            out.push(ch);
        }
    }
    out
}

fn decode_rfc2231_value(raw: &str) -> Option<String> {
    let (charset, rest) = raw.split_once('\'')?;
    let (_, encoded) = rest.split_once('\'')?;
    let bytes: Vec<u8> = percent_decode_str(encoded).collect();
    let charset = (!charset.is_empty()).then_some(charset);
    Some(text::decode_charset(&bytes, charset))
}

fn assemble_rfc2231(parts: &BTreeMap<usize, (bool, String)>) -> Option<String> {
    let mut charset: Option<String> = None;
    let mut bytes = Vec::new();
    for (&index, (encoded, raw)) in parts {
        if index == 0 && *encoded {
            if let Some((cs, rest)) = raw.split_once('\'')
                && let Some((_, data)) = rest.split_once('\'')
            {
                if !cs.is_empty() {
                    charset = Some(cs.to_string());
                }
                bytes.extend(percent_decode_str(data));
                continue;
            }
        }
        if *encoded {
            bytes.extend(percent_decode_str(raw));
        } else {
            bytes.extend_from_slice(raw.as_bytes());
        }
    }
    Some(text::decode_charset(&bytes, charset.as_deref()))
}

fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let marker = format!("--{boundary}");
    let marker_bytes = marker.as_bytes();
    let mut parts = Vec::new();
    let mut part_start: Option<usize> = None;

    let mut cursor = 0;
    while cursor < body.len() {
        let line_start = cursor;
        let line_end = body[cursor..].iter().position(|&b| b == b'\n').map_or(body.len(), |pos| cursor + pos);
        let next = if line_end < body.len() { line_end + 1 } else { body.len() };
        let stripped = if line_end > line_start && body[line_end - 1] == b'\r' { line_end - 1 } else { line_end };
        let line = &body[line_start..stripped];

        if let Some(after) = line.strip_prefix(marker_bytes) {
            let is_close = after.starts_with(b"--") && after[2..].iter().all(|b| matches!(b, b' ' | b'\t'));
            let is_boundary = is_close || after.iter().all(|b| matches!(b, b' ' | b'\t'));
            if is_boundary {
                if let Some(start) = part_start {
                    let mut end = line_start;
                    if end > start && body[end - 1] == b'\n' {
                        end -= 1;
                    }
                    if end > start && body[end - 1] == b'\r' {
                        end -= 1;
                    }
                    parts.push(&body[start..end]);
                }
                if is_close {
                    break;
                }
                part_start = Some(next);
            }
        }

        cursor = next;
    }

    parts
}

fn decode_transfer_encoding(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => decode_base64(body),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn decode_base64(raw: &[u8]) -> Vec<u8> {
    let clean: Vec<u8> = raw.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    if let Ok(decoded) = BASE64_LENIENT.decode(&clean) {
        return decoded;
    }
    let filtered: Vec<u8> = clean
        .into_iter()
        .filter(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/'))
        .collect();
    BASE64_LENIENT.decode(&filtered).unwrap_or_else(|_| raw.to_vec())
}

fn decode_quoted_printable(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'=' {
            // Soft line break: `=` followed by optional WSP then CRLF or LF.
            let mut j = i + 1;
            while j < raw.len() && matches!(raw[j], b' ' | b'\t') {
                j += 1;
            }
            if j < raw.len() && raw[j] == b'\r' {
                if j + 1 < raw.len() && raw[j + 1] == b'\n' {
                    i = j + 2;
                    continue;
                }
                i = j + 1;
                continue;
            }
            if j < raw.len() && raw[j] == b'\n' {
                i = j + 1;
                continue;
            }
            if i + 2 < raw.len()
                && let (Some(hi), Some(lo)) = (hex_digit(raw[i + 1]), hex_digit(raw[i + 2]))
            {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipart_with_inline_image_attachment_forward_and_charsets() {
        let dir = std::env::temp_dir().join(format!("mailstream-mime-parse-{}", std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);
        let raw = concat!(
            "From: Ann <ann@x.com>\r\n",
            "To: Me <me@gmail.com>\r\n",
            "Cc: Cy <cy@x.com>\r\n",
            "Reply-To: Reply <r@x.com>\r\n",
            "References: <m0@x> <m1@x>\r\n",
            "Subject: Lunch\r\n",
            "Content-Type: multipart/mixed; boundary=\"mix;1\"\r\n",
            "\r\n",
            "--mix;1\r\n",
            "Content-Type: multipart/related; boundary=\"rel1\"\r\n",
            "\r\n",
            "--rel1\r\n",
            "Content-Type: multipart/alternative; boundary=\"alt1\"\r\n",
            "\r\n",
            "--alt1\r\n",
            "Content-Type: text/plain; charset=gb2312\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "=C4=E3=BA=C3=\r\n",
            "=20=41\r\n",
            "--alt1\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "PHA+SGkgPGltZyBzcmM9ImNpZDpsb2dvQHgiPjwvcD4=\r\n",
            "--alt1--\r\n",
            "--rel1\r\n",
            "Content-Type: image/png; name=\"logo.png\"\r\n",
            "Content-ID: <logo@x>\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "cG5n\r\n",
            "--rel1--\r\n",
            "--mix;1\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment;\r\n",
            " filename*=UTF-8''%E8%B4%A6%E5%8D%95.pdf\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "JVBERg==\r\n",
            "--mix;1\r\n",
            "Content-Type: message/rfc822\r\n",
            "\r\n",
            "From: Bob <bob@x.com>\r\n",
            "Subject: Old\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Earlier\r\n",
            "--mix;1--\r\n"
        );
        let content = parse_message(raw.as_bytes(), dir.clone(), "mscid://k/".into()).unwrap();
        assert_eq!(content.cc, vec![crate::model::Address::new(Some("Cy".into()), "cy@x.com")]);
        assert_eq!(content.reply_to, vec![crate::model::Address::new(Some("Reply".into()), "r@x.com")]);
        assert_eq!(content.references, vec!["m0@x", "m1@x"]);
        assert_eq!(content.text.as_deref(), Some("你好 A\n\n---------- Attached message from \"Bob\" <bob@x.com>: Old ----------\nEarlier"));
        let html = content.html.as_deref().unwrap();
        assert!(html.contains("<p>Hi <img src=\"mscid://k/0-logo.png\"></p>"), "{html}");
        assert!(html.contains("Attached message from &quot;Bob&quot; &lt;bob@x.com&gt;: Old"), "{html}");
        assert_eq!(content.attachments.len(), 1);
        assert_eq!(content.attachments[0].filename, "账单.pdf");
        assert_eq!(std::fs::read(dir.join("0-logo.png")).unwrap(), b"png");
        assert_eq!(std::fs::read(dir.join(&content.attachments[0].stored_name)).unwrap(), b"%PDF");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn parses_inline_images_without_related_continuations_eml_and_invitations() {
        let dir = std::env::temp_dir().join(format!("mailstream-mime-edge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ics = concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:evt-400\r\n",
            "SUMMARY:Design Sync\r\n",
            "DTSTART:20261118T100000Z\r\n",
            "DTEND:20261118T110000Z\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n"
        );
        let ics_b64 = base64::engine::general_purpose::STANDARD.encode(ics);
        let raw = format!(
            "From: Ann <ann@x.com>\r\n\
             Subject: Parts\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n\
             --mix\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n\
             <p>Hello</p>\r\n\
             --mix\r\n\
             Content-Type: image/png\r\n\
             Content-Disposition: inline; filename=\"snap.png\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             cG5n\r\n\
             --mix\r\n\
             Content-Type: application/pdf\r\n\
             Content-Disposition: attachment;\r\n \
             filename*0*=UTF-8''%E6%B5%8B;\r\n \
             filename*1*=%E8%AF%95.pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             JVBERg==\r\n\
             --mix\r\n\
             Content-Type: message/rfc822\r\n\
             Content-Disposition: attachment; filename=\"orig.eml\"\r\n\r\n\
             From: Bob <bob@x.com>\r\nSubject: Inner\r\n\r\nHi\r\n\
             --mix\r\n\
             Content-Type: text/calendar; method=REQUEST\r\n\
             Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             {ics_b64}\r\n\
             --mix--\r\n"
        );
        let content = parse_message(raw.as_bytes(), dir.clone(), "mscid://k/".into()).unwrap();
        let html = content.html.as_deref().unwrap();
        assert!(html.contains("<p>Hello</p><p><img src=\"mscid://k/0-snap.png\" style=\"max-width:100%\"></p>"), "{html}");
        let names: Vec<&str> = content.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["测试.pdf", "orig.eml", "invite.ics"]);
        assert_eq!(content.invitation.as_ref().map(|inv| inv.title.as_str()), Some("Design Sync"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
