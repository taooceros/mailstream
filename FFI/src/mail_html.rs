//! The rules every reader applies to a mail's HTML, one copy for all of them (MS-234): where
//! quoted history starts, the reader's look over the mail's own styles, what a web view's page
//! may load, which links a click opens, which cached inline parts a page may show and how a mail's
//! colours move to the reader's surface (`colours`, Blitz's for now). Blitz
//! (`html_page.rs`, in Flutter and GPUI) applies them itself; the web views (Flutter's WebKit
//! `MailHtmlView.swift` and WebView2 `mail_html_windows.dart`, GPUI's `webview.rs`) load
//! [`mail_web_document`] and run [`mail_web_script`].

use std::path::PathBuf;

use mailstream_engine::cache::MailCache;

#[cfg(feature = "blitz")]
pub(crate) mod colours;

/// Elements that start quoted history, the outermost one matching: Gmail's, Thunderbird's and
/// Yahoo's quote containers, Outlook's reply header, Apple Mail's cited blockquote and the quotes
/// the engine makes of plain text's `>` lines (`text::plain_to_html`).
pub const QUOTES: &str = ".gmail_quote, .gmail_quote_container, .moz-cite-prefix, .yahoo_quoted, #divRplyFwdMsg, \
    #appendonsend, blockquote[type=\"cite\" i], blockquote.ms-quote";

/// The reader's look over the mail's own styles, in every renderer (its text colour only where the
/// mail sets none on `body`), and the look of the markup the engine makes of plain text
/// (`text::plain_to_html`: its line breaks kept, its `>` quotes muted, the ones inside a quote
/// behind a rule; the reader's fold draws the outer one's).
const BODY_CSS: &str = "
body { margin: 0; padding: 2px 0; font: 14px/1.45 -apple-system, \"Segoe UI\", system-ui, sans-serif;
  overflow-wrap: break-word; }
:where(body) { color: #1f1f1f; }
img { max-width: 100%; }
pre { white-space: pre-wrap; }
.ms-plain { white-space: pre-wrap; }
blockquote.ms-quote { margin: 6px 0; color: #5f6368; }
blockquote.ms-quote blockquote.ms-quote { padding-left: 12px; border-left: 2px solid rgba(0, 0, 0, 0.1); }
";

/// Blitz and WebView2: the document is as tall as its content, so nothing sizes from the viewport
/// (a flex `html` makes mail layout tables' `height: 100%` auto).
const FLEX_SIZING: &str = "
html, body { height: auto !important; min-height: 0 !important; overflow: hidden !important; }
html { display: flex !important; flex-direction: column !important; width: 100% !important; }
";

/// WebKit: in quirks mode (mail without a doctype, as Outlook writes it) an `auto` height
/// stretches `html` and `body` to the viewport, and a `height: 100%` (mail layout tables) passes
/// over auto-height blocks up to it, so the measured height grew the view, which grew the page
/// (MS-78, MS-160). A `fit-content` height is not `auto`: neither stretches, and percentages
/// inside them resolve as auto, so the body ends where its content does in either mode.
const FIT_SIZING: &str = "
html, body { height: fit-content !important; min-height: 0 !important; overflow: hidden !important; }
";

/// A web page's quoted history, marked by [`mail_web_script`]: hidden while folded, behind a rule
/// at its left when open.
const FOLD_CSS: &str = "
html[data-mailstream-folded] [data-mailstream-quote] { display: none !important; }
[data-mailstream-quote] { border-left: 2px solid rgba(0, 0, 0, 0.1); padding-left: 12px; }
";

/// The web views that show mail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MailWebView {
    /// The Mac's `WKWebView` (Flutter's `MailHtmlView`, GPUI's `webview.rs`).
    WebKit,
    /// Windows' WebView2 (Flutter's `WindowsMailHtml`).
    WebView2,
}

/// Where a web page's inline parts come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum InlineParts {
    /// `mscid://<message>/<part>`, which the view's scheme handler serves from the inline cache
    /// (Flutter's `InlinePartHandler`): the page reads no file.
    Served,
    /// The `file:` URLs the engine wrote, read by the view itself (WebView2's page is a file;
    /// GPUI's page has the inline cache as its base).
    Files,
}

#[cfg(feature = "blitz")]
/// Blitz's style sheet over the mail's own.
pub(crate) fn blitz_css() -> String {
    [BODY_CSS, FLEX_SIZING].concat()
}

/// What a web page may read: HTTPS and `data:` images, styles and fonts and the inline parts
/// `inline` names; no scripts, frames, plug-ins, forms or base. The mail's own scripts are also
/// off in every view's settings.
fn content_policy(inline: InlineParts) -> &'static str {
    match inline {
        InlineParts::Served => {
            "default-src 'none'; img-src https: data: mscid:; style-src 'unsafe-inline' https:; \
             font-src https: data:; base-uri 'none'; form-action 'none'"
        }
        InlineParts::Files => {
            "default-src 'none'; img-src https: data: file:; style-src 'unsafe-inline' https:; \
             font-src https: data:; base-uri 'none'; form-action 'none'"
        }
    }
}

/// The page a web view loads for `html`: the mail exactly as the engine gave it, with two string
/// edits and no parsing (MS-167). Its content policy goes ahead of everything but a leading
/// doctype (and the white space, comments and `<?xml …?>` line that may come before one), so the
/// parser puts it into the (implied) head and a standards-mode body is not laid out in quirks
/// mode (MS-160). With [`InlineParts::Served`] the inline cache's `file:` URLs become `mscid://`.
#[uniffi::export]
pub fn mail_web_document(html: String, inline: InlineParts) -> String {
    let html = match inline {
        InlineParts::Served if html.contains("file:") => html.replace(&inline_parts_url(), "mscid://"),
        _ => html,
    };
    let at = prologue_end(&html);
    let policy = content_policy(inline);
    let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">");
    let mut page = String::with_capacity(html.len() + meta.len());
    page.push_str(&html[..at]);
    page.push_str(&meta);
    page.push_str(&html[at..]);
    page
}

/// Where a leading doctype ends, after the byte-order mark, white space, comments and `<?…>`
/// lines (bogus comments to HTML) that may come before it; 0 without one.
fn prologue_end(html: &str) -> usize {
    let mut rest = html;
    loop {
        let trimmed = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
        if let Some(comment) = trimmed.strip_prefix("<!--") {
            let Some(end) = comment.find("-->") else { return 0 };
            rest = &comment[end + 3..];
        } else if let Some(line) = trimmed.strip_prefix("<?") {
            let Some(end) = line.find('>') else { return 0 };
            rest = &line[end + 1..];
        } else if trimmed.get(..9).is_some_and(|start| start.eq_ignore_ascii_case("<!doctype")) {
            return match trimmed.find('>') {
                Some(end) => html.len() - trimmed.len() + end + 1,
                None => 0,
            };
        } else {
            return 0;
        }
    }
}

/// The script a web view runs over its page at the end of loading, in the app's own world (the
/// mail's scripts never run): it adds the reader's style sheet for `view` and marks the outermost
/// [`QUOTES`] in the body, then folds them (`data-mailstream-folded` on the root). Mail that is
/// nothing but quotes (no text or image outside them) shows whole, with nothing marked. It
/// leaves `window.mailstreamQuoted` (whether there is quoted history to fold) and
/// `window.mailstreamFold(open)`, which calls `window.mailstreamChanged` if the view set one.
/// Running it again does nothing.
#[uniffi::export]
pub fn mail_web_script(view: MailWebView) -> String {
    let sizing = match view {
        MailWebView::WebKit => FIT_SIZING,
        MailWebView::WebView2 => FLEX_SIZING,
    };
    let css = serde_json::to_string(&[BODY_CSS, sizing, FOLD_CSS].concat()).expect("a string encodes");
    let quotes = serde_json::to_string(QUOTES).expect("a string encodes");
    format!(
        "(() => {{
  if (window.mailstreamFold) return;
  const doc = document, root = doc.documentElement, body = doc.body;
  const style = doc.createElement('style');
  style.textContent = {css};
  (doc.head || root).appendChild(style);
  const quotes = [];
  const collect = (el) => {{
    for (const child of el.children) child.matches({quotes}) ? quotes.push(child) : collect(child);
  }};
  if (body) collect(body);
  quotes.forEach((quote) => quote.setAttribute('data-mailstream-quote', ''));
  const outside = (node) => !(node.nodeType === 1 ? node : node.parentElement).closest('[data-mailstream-quote]');
  const kept = () => {{
    const texts = doc.createTreeWalker(body, NodeFilter.SHOW_TEXT);
    for (let text = texts.nextNode(); text; text = texts.nextNode()) {{
      if (text.data.trim() && outside(text)) return true;
    }}
    return Array.from(body.querySelectorAll('img')).some(outside);
  }};
  window.mailstreamQuoted = quotes.length > 0 && kept();
  if (window.mailstreamQuoted) root.setAttribute('data-mailstream-folded', '');
  else quotes.forEach((quote) => quote.removeAttribute('data-mailstream-quote'));
  window.mailstreamFold = (open) => {{
    if (open) root.removeAttribute('data-mailstream-folded');
    else if (window.mailstreamQuoted) root.setAttribute('data-mailstream-folded', '');
    if (window.mailstreamChanged) window.mailstreamChanged();
  }};
}})();"
    )
}

/// Whether a click on `link` opens it: web pages with a host and mail addresses, without
/// credentials; nothing local (`file:`), no scripts, no other app's scheme.
#[uniffi::export]
pub fn is_mail_link(link: &str) -> bool {
    let Ok(url) = url::Url::parse(link.trim()) else { return false };
    match url.scheme() {
        "mailto" => true,
        "http" | "https" => url.username().is_empty() && url.password().is_none() && url.host().is_some(),
        _ => false,
    }
}

/// The folder of every message's inline parts in the engine's cache (`messages-v2`), which the
/// Mac's WebKit bodies serve their `mscid:` images from.
#[uniffi::export]
pub fn inline_parts_folder() -> String {
    parts_folder().to_string_lossy().into_owned()
}

fn parts_folder() -> PathBuf {
    MailCache::parts_folder(&mailstream_engine::cache_directory())
}

/// The inline cache's `file:` URL, as the engine writes it into bodies.
fn inline_parts_url() -> String {
    MailCache::folder_url(&parts_folder())
}

/// The most of an inline part a page shows.
const MAX_INLINE_PART: u64 = 20 << 20;

/// The bytes of the inline part at the `file:` URL `source` (as the engine writes it into
/// bodies, Blitz's way), and their type, when [`inline_file`] allows. Blocking.
#[cfg(feature = "blitz")]
pub(crate) fn inline_part(source: &str) -> Option<(Vec<u8>, &'static str)> {
    let url = url::Url::parse(source).ok()?;
    if url.scheme() != "file" || url.host().is_some() || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    inline_file(&url.to_file_path().ok()?, &parts_folder().canonicalize().ok()?)
}

/// The bytes of the inline part a WebKit page asks for at `mscid://<message>/<part>` (the address
/// [`mail_web_document`] gives the inline cache's files with [`InlineParts::Served`]), and their
/// type, when [`inline_file`] allows: the message is a hex key, the part one file name. Blocking.
pub fn served_inline_part(source: &str) -> Option<(Vec<u8>, &'static str)> {
    let url = url::Url::parse(source).ok()?;
    let key = url.host_str()?;
    let name = percent_decoded(url.path().strip_prefix('/')?)?;
    let valid = url.scheme() == "mscid"
        && !key.is_empty()
        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !name.is_empty()
        && !name.contains('/')
        && !name.starts_with('.');
    if !valid {
        return None;
    }
    let folder = parts_folder();
    inline_file(&folder.join(key).join(name), &folder.canonicalize().ok()?)
}

/// `text` with its `%XX` escapes decoded, if it is UTF-8 then.
fn percent_decoded(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' && tail.len() >= 2 {
            let hex = std::str::from_utf8(&tail[..2]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// The bytes of the file at `path`, and their type, when its real path (links followed) lies
/// inside the canonical folder `root`, it holds at most 20 MB and it is a PNG, JPEG, GIF or WebP
/// image by its bytes (never by the name the sender chose).
fn inline_file(path: &std::path::Path, root: &std::path::Path) -> Option<(Vec<u8>, &'static str)> {
    let path = path.canonicalize().ok()?;
    if !path.starts_with(root) || std::fs::metadata(&path).ok()?.len() > MAX_INLINE_PART {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let mime = raster_mime(&bytes)?;
    Some((bytes, mime))
}

fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_a_click_opens() {
        assert!(is_mail_link("https://example.com/a"));
        assert!(is_mail_link("http://example.com"));
        assert!(is_mail_link("HTTPS://Example.com"));
        assert!(is_mail_link("mailto:someone@example.com"));
        for refused in [
            "file:///etc/passwd",
            "https://user@example.com/",
            "https://user:secret@example.com/",
            "javascript:alert(1)",
            "data:text/html,x",
            "zoommtg://join",
            "https:",
            "not a link",
        ] {
            assert!(!is_mail_link(refused), "{refused}");
        }
    }

    #[test]
    fn the_policy_goes_after_a_leading_doctype_only() {
        let served = content_policy(InlineParts::Served);
        let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{served}\">");
        let page = |html: &str| mail_web_document(html.into(), InlineParts::Served);
        let xhtml = "<?xml version=\"1.0\"?>\n<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"x.dtd\">";
        assert_eq!(page(&format!("\u{FEFF}{xhtml}<html></html>")), format!("\u{FEFF}{xhtml}{meta}<html></html>"));
        assert_eq!(
            page("<!-- <head> --><!DOCTYPE html><html><head><title>t</title></head></html>"),
            format!("<!-- <head> --><!DOCTYPE html>{meta}<html><head><title>t</title></head></html>")
        );
        assert_eq!(page(" <!doctype HTML>\n<p>x</p>"), format!(" <!doctype HTML>{meta}\n<p>x</p>"));
        // A doctype after content is no prologue.
        let late = "<html><head></head><body><!DOCTYPE html></body></html>";
        assert_eq!(page(late), format!("{meta}{late}"));
        assert_eq!(page("<p>x</p>"), format!("{meta}<p>x</p>"));
        assert_eq!(page("<!-- open"), format!("{meta}<!-- open"));
    }

    #[test]
    fn served_inline_parts_leave_the_file_system() {
        let cache = inline_parts_url();
        let mail = format!("<img src=\"{cache}0a1b/0-logo%20a.png\"><img src=\"file:///etc/x.png\">");
        let page = mail_web_document(mail.clone(), InlineParts::Served);
        assert!(page.ends_with("<img src=\"mscid://0a1b/0-logo%20a.png\"><img src=\"file:///etc/x.png\">"), "{page}");
        assert!(page.contains("img-src https: data: mscid:;"));
        let page = mail_web_document(mail.clone(), InlineParts::Files);
        assert!(page.ends_with(&mail) && page.contains("img-src https: data: file:;"), "{page}");
        for inline in [InlineParts::Served, InlineParts::Files] {
            assert!(!content_policy(inline).contains("script"));
        }
        assert!(!content_policy(InlineParts::Served).contains("file:"));
    }

    #[test]
    fn the_web_script_carries_the_rules() {
        for view in [MailWebView::WebKit, MailWebView::WebView2] {
            let script = mail_web_script(view);
            assert!(script.contains("blockquote.ms-quote") && script.contains("#divRplyFwdMsg"));
            assert!(script.contains("-apple-system, \\\"Segoe UI\\\""));
        }
        assert!(mail_web_script(MailWebView::WebKit).contains("fit-content"));
        assert!(mail_web_script(MailWebView::WebView2).contains("display: flex"));
    }

    #[test]
    fn inline_parts_only_inside_the_cache() {
        let base = std::env::temp_dir().join(format!("mailstream-inline-{}", std::process::id()));
        let root = base.join("messages-v2");
        std::fs::create_dir_all(root.join("m1")).unwrap();
        std::fs::write(root.join("m1/logo.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        std::fs::write(root.join("m1/notes.txt"), b"plain").unwrap();
        std::fs::write(base.join("outside.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        let root = root.canonicalize().unwrap();

        let (bytes, mime) = inline_file(&root.join("m1/logo.png"), &root).unwrap();
        assert_eq!((bytes.len(), mime), (12, "image/png"));
        assert_eq!(inline_file(&root.join("m1/notes.txt"), &root), None);
        assert_eq!(inline_file(&base.join("outside.png"), &root), None);
        assert_eq!(inline_file(&root.join("m1/../../outside.png"), &root), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("outside.png"), root.join("m1/link.png")).unwrap();
            assert_eq!(inline_file(&root.join("m1/link.png"), &root), None);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn served_parts_name_one_file_of_one_message() {
        assert_eq!(percent_decoded("0-logo%20a.png").as_deref(), Some("0-logo a.png"));
        assert_eq!(percent_decoded("%ZZ"), None);
        for refused in [
            "mscid://0a1b/..%2F..%2Fsecret.png",
            "mscid://0a1b/.hidden.png",
            "mscid://not-hex/0-logo.png",
            "mscid://0a1b/",
            "file:///etc/hosts",
            "https://0a1b/0-logo.png",
        ] {
            assert_eq!(served_inline_part(refused), None, "{refused}");
        }
    }
}
