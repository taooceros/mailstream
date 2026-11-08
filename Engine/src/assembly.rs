//! Builds a message's content from a provider's parts, as the Swift engine's `MessageAssembly`
//! (`Sources/MailstreamKit/Backend/MessageAssembly.swift`): binary parts go into the message's
//! cache directory, `cid:` references in the HTML point at them, and parts the HTML shows stay
//! out of the attachment list.
use std::path::PathBuf;

use crate::model::{Address, Attachment, MessageContent};
use crate::text;

pub struct MessageAssembly {
    directory: PathBuf,
    url_base: String,
    stored: Vec<Attachment>,
}

/// `URL`'s path-allowed characters: everything else in a stored name is percent-encoded.
const PATH_ESCAPED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    .remove(b':')
    .remove(b'@')
    .remove(b'/');

pub fn path_encoded(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, PATH_ESCAPED).to_string()
}

impl MessageAssembly {
    pub fn new(directory: PathBuf, url_base: String) -> std::io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        Ok(Self { directory, url_base, stored: Vec::new() })
    }

    /// Writes one part. `index` is unique within the message and becomes `Attachment::part`.
    pub fn store(
        &mut self,
        bytes: &[u8],
        index: i64,
        filename: Option<&str>,
        mime: &str,
        content_id: Option<&str>,
        inline: bool,
    ) -> Attachment {
        let mime = if mime.is_empty() { "application/octet-stream".to_string() } else { mime.to_lowercase() };
        let mut name = filename.unwrap_or("").to_string();
        if name.is_empty() {
            name = format!("attachment-{index}");
            if let Some(extension) = text::file_extension(&mime) {
                name.push('.');
                name.push_str(extension);
            }
        }
        let safe = name.replace(['/', ':'], "_");
        let stored_name = format!("{index}-{safe}");
        let _ = std::fs::write(self.directory.join(&stored_name), bytes);
        let content_id = content_id
            .map(|id| id.trim_matches(|c| matches!(c, '<' | '>' | ' ')).to_string())
            .filter(|id| !id.is_empty());
        let attachment = Attachment {
            part: index,
            filename: name,
            mime,
            size: bytes.len() as i64,
            content_id,
            inline,
            stored_name,
        };
        self.stored.push(attachment.clone());
        attachment
    }

    fn url(&self, attachment: &Attachment) -> String {
        format!("{}{}", self.url_base, path_encoded(&attachment.stored_name))
    }

    /// `html` is nil for text-only mail; `text` the plain body, nil when the message has none
    /// (then the snippet and quoting come from the HTML).
    pub fn finish(
        self,
        html: Option<String>,
        text: Option<String>,
        cc: Vec<Address>,
        reply_to: Vec<Address>,
        references: Vec<String>,
    ) -> MessageContent {
        let mut shown = std::collections::HashSet::new();
        let html = html.map(|mut rewritten| {
            for attachment in &self.stored {
                let Some(cid) = &attachment.content_id else { continue };
                let pattern = regex::RegexBuilder::new(&regex::escape(&format!("cid:{cid}")))
                    .case_insensitive(true)
                    .build()
                    .expect("an escaped literal compiles");
                if pattern.is_match(&rewritten) {
                    let url = self.url(attachment);
                    rewritten = pattern.replace_all(&rewritten, regex::NoExpand(&url)).into_owned();
                    shown.insert(attachment.part);
                }
            }
            rewritten
        });
        let attachments = self
            .stored
            .iter()
            .filter(|attachment| !shown.contains(&attachment.part))
            .map(|attachment| Attachment { inline: false, ..attachment.clone() })
            .collect();
        let plain = text.as_deref().map(str::trim).unwrap_or("").to_string();
        let snippet_source = if plain.is_empty() { text::strip_html(html.as_deref().unwrap_or("")) } else { plain.clone() };
        let text = if plain.is_empty() { html.as_deref().map(text::strip_html) } else { Some(plain) };
        MessageContent {
            html,
            text,
            cc,
            reply_to,
            references,
            attachments,
            snippet: text::snippet(&snippet_source),
        }
    }
}

/// A message attached to the one assembled (`message/rfc822`, an Outlook item), shown inline
/// below a headline naming its sender and subject.
pub struct AttachedMessage {
    /// The block for an HTML body: the attached HTML, else its text as HTML.
    pub html: String,
    /// The block for a plain body: the headline as a divider, then the text.
    pub text: String,
    /// The attached message has an HTML body, worth showing even below a plain one.
    pub has_html: bool,
}

impl AttachedMessage {
    pub fn new(sender: Option<&Address>, subject: &str, html: Option<&str>, text: &str) -> Self {
        let from = sender.map_or_else(|| "unknown sender".to_string(), Address::rfc5322);
        let headline = if subject.is_empty() {
            format!("Attached message from {from}")
        } else {
            format!("Attached message from {from}: {subject}")
        };
        let body = html.map_or_else(|| text::plain_to_html(text), String::from);
        Self {
            html: format!(
                "<div class=\"ms-attached-message\"><div class=\"ms-attached-headline\">{}</div>{body}</div>",
                text::escape_html(&headline)
            ),
            text: format!("---------- {headline} ----------\n{text}"),
            has_html: html.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_parts_and_points_the_html_at_them() {
        let directory = std::env::temp_dir().join(format!("mailstream-assembly-{}", std::process::id()));
        let mut assembly = MessageAssembly::new(directory.clone(), "mscid://k/".into()).unwrap();
        assembly.store(b"png", 0, Some("logo a.png"), "image/PNG", Some("<Logo@x>"), true);
        assembly.store(b"pdf", 1, None, "application/pdf", None, false);
        let content = assembly.finish(
            Some("<img src=\"CID:logo@x\"><p>Hi&amp;bye</p>".into()),
            None,
            vec![],
            vec![],
            vec![],
        );
        assert_eq!(content.html.as_deref(), Some("<img src=\"mscid://k/0-logo%20a.png\"><p>Hi&amp;bye</p>"));
        assert_eq!(content.text.as_deref(), Some("  Hi&bye\n"));
        assert_eq!(content.attachments.len(), 1);
        assert_eq!(content.attachments[0].filename, "attachment-1.pdf");
        assert_eq!(std::fs::read(directory.join("1-attachment-1.pdf")).unwrap(), b"pdf");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
