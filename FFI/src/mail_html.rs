//! The rules every reader applies to a mail's HTML, one copy for all of them (MS-234): where
//! quoted history starts, the reader's look over the mail's own styles, what a web view's page
//! may load, which links a click opens, which cached inline parts a page may show and how a mail's
//! colours move to the reader's surface (`colours`). Blitz (`html_page.rs`, in Flutter and GPUI)
//! applies them itself; the web views (Flutter's WebKit `MailHtmlView.swift` and WebView2
//! `mail_html_windows.dart`, GPUI's `webview.rs`) load [`mail_web_document`] and run
//! [`mail_web_script`] (GPUI's also [`MAIL_WEB_COLOURS`] and [`mail_web_recolour`]).

use std::path::PathBuf;

use mailstream_engine::cache::MailCache;
use serde::Deserialize;

pub(crate) mod colours;

use colours::Rgba;

/// Elements that start quoted history, the outermost one matching: Gmail's, Thunderbird's,
/// Yahoo's, Spark's, Spike/Hop's and ProtonMail's quote containers, Outlook's reply headers,
/// Apple Mail's cited blockquote and the quotes the engine makes of plain text's `>` lines
/// (`text::plain_to_html`).
pub const QUOTES: &str = ".gmail_quote, .gmail_quote_container, .moz-cite-prefix, .yahoo_quoted, #divRplyFwdMsg, \
    #appendonsend, #OLK_SRC_BODY_SECTION, #mail-editor-reference-message-container, div[name=\"messageReplySection\"], \
    .hop_extra, blockquote.hop_quote, .protonmail_quote, blockquote[type=\"cite\" i], blockquote.ms-quote";

/// Element ids whose element and all following siblings in the same parent are quoted history
/// (Outlook Web / OWA reply and forward boundaries), checked after stripping Gmail's `m_<digits>`
/// id prefix.
pub(crate) const TAIL_QUOTE_IDS: &[&str] = &["divRplyFwdMsg", "appendonsend"];

/// Element ids whose element itself is a quoted-history container (Outlook Mac / Mobile / new
/// Outlook), checked after stripping Gmail's `m_<digits>` id prefix.
pub(crate) const QUOTE_CONTAINER_IDS: &[&str] = &["mail-editor-reference-message-container", "OLK_SRC_BODY_SECTION"];

/// Strips Gmail's `m_<signed-digits>` id prefix when present (e.g.
/// `m_-7080766174707785600divRplyFwdMsg` -> `divRplyFwdMsg`,
/// `m_6900309819208902883mail-editor-reference-message-container` ->
/// `mail-editor-reference-message-container`).
pub(crate) fn strip_gmail_id_prefix(id: &str) -> &str {
    let Some(rest) = id.strip_prefix("m_") else { return id };
    let digits = rest.strip_prefix('-').unwrap_or(rest);
    let count = digits.bytes().take_while(|b| b.is_ascii_digit()).count();
    if count == 0 { id } else { &digits[count..] }
}

fn is_zero_width(token: &str) -> bool {
    matches!(token, "0" | "0px" | "0pt" | "0em" | "0rem" | "0in" | "0cm" | "0mm")
}

fn is_hidden_border_style(token: &str) -> bool {
    token.is_empty() || matches!(token, "none" | "hidden")
}

/// Whether an inline `style` attribute sets a visible top border (as desktop Outlook's
/// `border:none;border-top:solid #E1E1E1 1.0pt` or Outlook Mac/Mobile's
/// `border-width:1pt medium medium;border-style:solid none none` reply header divider does).
pub(crate) fn has_visible_border_top(style: &str) -> bool {
    let mut visible_top = false;
    let mut top_width_zero = false;
    for decl in style.split(';') {
        let Some((name, value)) = decl.split_once(':') else { continue };
        let name = name.trim();
        let value = value.trim().to_ascii_lowercase();
        if name.eq_ignore_ascii_case("border") {
            if is_hidden_border_style(&value) || is_zero_width(&value) {
                visible_top = false;
                top_width_zero = false;
            }
        } else if name.eq_ignore_ascii_case("border-top") {
            if value.is_empty()
                || value.split_whitespace().any(|tok| is_hidden_border_style(tok) || is_zero_width(tok))
            {
                visible_top = false;
            } else {
                visible_top = true;
                top_width_zero = false;
            }
        } else if name.eq_ignore_ascii_case("border-top-style") || name.eq_ignore_ascii_case("border-style") {
            let top = value.split_whitespace().next().unwrap_or("");
            visible_top = !is_hidden_border_style(top);
        } else if name.eq_ignore_ascii_case("border-top-width") || name.eq_ignore_ascii_case("border-width") {
            let top = value.split_whitespace().next().unwrap_or("");
            top_width_zero = is_zero_width(top);
        }
    }
    visible_top && !top_width_zero
}

/// Whether `text` (with `<br>` and block tags turned into line breaks) is a desktop-Outlook-style
/// reply header block: first non-empty line starts with `From:` (or localized equivalent) and a
/// subsequent non-empty line starts with `Sent:`/`Date:`/`To:`/`Subject:` (or localized equivalent).
pub(crate) fn is_outlook_header_text(text: &str) -> bool {
    const FROM: &[&str] = &["from:", "发件人:", "发件人：", "寄件者:", "寄件者：", "von:", "de:", "差出人:", "差出人："];
    const OTHER: &[&str] = &[
        "sent:", "date:", "to:", "subject:",
        "发送时间:", "发送时间：", "日期:", "日期：", "收件人:", "收件人：", "主题:", "主题：",
        "gesendet:", "datum:", "envoyé:", "送信日時:", "送信日時：", "件名:", "件名：",
    ];
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else { return false };
    let first_lower = first.to_ascii_lowercase();
    if !FROM.iter().any(|prefix| first_lower.starts_with(prefix)) {
        return false;
    }
    lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        OTHER.iter().any(|prefix| lower.starts_with(prefix))
    })
}

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

/// What a web page may read: `data:` images and fonts, inline styles and the inline parts `inline`
/// names, and with `remote` (the conversation's senders are known, `mail.remote`) also HTTPS
/// images, styles and fonts; no scripts, frames, plug-ins, forms or base. The mail's own scripts
/// are also off in every view's settings.
fn content_policy(inline: InlineParts, remote: bool) -> &'static str {
    match (inline, remote) {
        (InlineParts::Served, true) => {
            "default-src 'none'; img-src https: data: mscid:; style-src 'unsafe-inline' https:; \
             font-src https: data:; base-uri 'none'; form-action 'none'"
        }
        (InlineParts::Files, true) => {
            "default-src 'none'; img-src https: data: file:; style-src 'unsafe-inline' https:; \
             font-src https: data:; base-uri 'none'; form-action 'none'"
        }
        (InlineParts::Served, false) => {
            "default-src 'none'; img-src data: mscid:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; \
             form-action 'none'"
        }
        (InlineParts::Files, false) => {
            "default-src 'none'; img-src data: file:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; \
             form-action 'none'"
        }
    }
}

/// The page a web view loads for `html`: the mail exactly as the engine gave it, with two string
/// edits and no parsing (MS-167). Its content policy goes ahead of everything but a leading
/// doctype (and the white space, comments and `<?xml …?>` line that may come before one), so the
/// parser puts it into the (implied) head and a standards-mode body is not laid out in quirks
/// mode (MS-160). With [`InlineParts::Served`] the inline cache's `file:` URLs become `mscid://`.
/// Without `remote` the page reads nothing from the web (MS-364).
#[uniffi::export]
pub fn mail_web_document(html: String, inline: InlineParts, remote: bool) -> String {
    let html = match inline {
        InlineParts::Served if html.contains("file:") => html.replace(&inline_parts_url(), "mscid://"),
        _ => html,
    };
    let at = prologue_end(&html);
    let policy = content_policy(inline, remote);
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
  const normalizeId = (id) => (id || '').replace(/^m_-?\\d+/, '');
  const isZeroWidth = (t) => /^(0|0px|0pt|0em|0rem|0in|0cm|0mm)$/.test(t);
  const isHiddenStyle = (t) => !t || t === 'none' || t === 'hidden';
  const hasBorderTop = (el) => {{
    const st = el.getAttribute('style') || '';
    let visibleTop = false, topWidthZero = false;
    for (const d of st.split(';')) {{
      const i = d.indexOf(':');
      if (i < 0) continue;
      const name = d.slice(0, i).trim().toLowerCase();
      const v = d.slice(i + 1).trim().toLowerCase();
      if (name === 'border') {{
        if (isHiddenStyle(v) || isZeroWidth(v)) {{ visibleTop = false; topWidthZero = false; }}
      }} else if (name === 'border-top') {{
        const toks = v.split(/\\s+/).filter(Boolean);
        if (!toks.length || toks.some((t) => isHiddenStyle(t) || isZeroWidth(t))) visibleTop = false;
        else {{ visibleTop = true; topWidthZero = false; }}
      }} else if (name === 'border-top-style' || name === 'border-style') {{
        visibleTop = !isHiddenStyle(v.split(/\\s+/)[0] || '');
      }} else if (name === 'border-top-width' || name === 'border-width') {{
        topWidthZero = isZeroWidth(v.split(/\\s+/)[0] || '');
      }}
    }}
    return visibleTop && !topWidthZero;
  }};
  const blockText = (el) => {{
    let out = '';
    const walk = (n) => {{
      if (out.length > 3000) return;
      if (n.nodeType === 3) {{ out += n.data; return; }}
      if (n.nodeType !== 1) return;
      const t = n.localName;
      if (t === 'br') {{ out += '\\n'; return; }}
      const blk = t === 'p' || t === 'div';
      if (blk) out += '\\n';
      for (const c of n.childNodes) walk(c);
      if (blk) out += '\\n';
    }};
    walk(el);
    return out;
  }};
  const fromRe = /^(from:|发件人[:：]|寄件者[:：]|von:|de:|差出人[:：])/i;
  const otherRe = /^(sent:|date:|to:|subject:|发送时间[:：]|日期[:：]|收件人[:：]|主题[:：]|gesendet:|datum:|envoyé:|送信日時[:：]|件名[:：])/i;
  const isOutlookHeader = (el) => {{
    if (el.localName !== 'div' || !hasBorderTop(el) || el.querySelector('table, blockquote')) return false;
    const lines = blockText(el).split('\\n').map((s) => s.trim()).filter(Boolean);
    return lines.length >= 2 && fromRe.test(lines[0]) && lines.slice(1).some((l) => otherRe.test(l));
  }};
  const isTailMarker = (el) => {{
    const nid = normalizeId(el.id);
    return nid === 'divRplyFwdMsg' || nid === 'appendonsend' || isOutlookHeader(el);
  }};
  const isQuoteContainerId = (el) => {{
    const nid = normalizeId(el.id);
    return nid === 'mail-editor-reference-message-container' || nid === 'OLK_SRC_BODY_SECTION';
  }};
  const isTailCutoff = (el) => {{
    let cur = el;
    while (cur && cur.nodeType === 1) {{
      if (isTailMarker(cur)) return true;
      if (cur.localName !== 'div') return false;
      let sole = null;
      for (const c of cur.childNodes) {{
        if (c.nodeType === 3 && c.data.trim()) return false;
        if (c.nodeType === 1) {{ if (sole) return false; sole = c; }}
      }}
      cur = sole;
    }}
    return false;
  }};
  const quotes = [];
  const collect = (el) => {{
    for (const child of Array.from(el.children)) {{
      if (isTailCutoff(child)) {{
        const prev = child.previousElementSibling;
        if (prev && prev.localName === 'hr' && !quotes.includes(prev)) quotes.push(prev);
        for (let s = child; s; s = s.nextSibling) {{
          if (s.nodeType === 1) quotes.push(s);
          else if (s.nodeType === 3 && s.data.trim()) {{
            const span = doc.createElement('span');
            s.parentNode.insertBefore(span, s);
            span.appendChild(s);
            quotes.push(span);
          }}
        }}
        return;
      }}
      if (child.matches({quotes}) || isQuoteContainerId(child)) quotes.push(child);
      else collect(child);
    }}
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

/// The surface a page sits on and the reader's colours there, `0xRRGGBB`: the page is transparent
/// where the mail paints nothing, and in dark mode the mail's colours move to the surface
/// (`colours`), unless the page is on paper (`HtmlPage::paper`, [`MailWebColours::paper`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HtmlTheme {
    pub dark: bool,
    pub surface: u32,
    pub text: u32,
    pub link: u32,
}

impl From<HtmlTheme> for colours::Theme {
    fn from(theme: HtmlTheme) -> Self {
        Self { dark: theme.dark, surface: Rgba::hex(theme.surface), text: Rgba::hex(theme.text), link: Rgba::hex(theme.link) }
    }
}

/// The script a web view runs after [`mail_web_script`] to read its page's computed colours for
/// [`mail_web_recolour`]: it answers JSON (the `color-scheme` meta tags' contents, the style
/// sheets' text and every node of the body's tree in order, with its parent: text as its length,
/// elements as their colours, which `<canvas>` writes as `#rrggbb` or `rgba(…)`) and keeps the
/// nodes in `window.mailstreamColoured`, by the same index. The head is left out.
pub const MAIL_WEB_COLOURS: &str = r#"(() => {
  const context = document.createElement('canvas').getContext('2d');
  const written = new Map();
  const colour = (value) => {
    let found = written.get(value);
    if (found === undefined) {
      context.fillStyle = 'transparent';
      context.fillStyle = value;
      found = context.fillStyle;
      written.set(value, found);
    }
    return found;
  };
  const nodes = [], entries = [];
  const visit = (node, parent) => {
    if (node.nodeType === 3) {
      nodes.push(node);
      entries.push([parent, node.data.trim().length]);
      return;
    }
    if (node.nodeType !== 1 || node.localName === 'head') return;
    const style = getComputedStyle(node), tag = node.localName, text = style.color;
    // A border in the text's colour is taken for `currentColor`, which follows the text.
    const border = (side) => {
      const kind = style.getPropertyValue(`border-${side}-style`), value = style.getPropertyValue(`border-${side}-color`);
      return kind === 'none' || kind === 'hidden' || value === text ? null : colour(value);
    };
    const flags = (style.backgroundImage !== 'none' ? 1 : 0) | (tag === 'html' || tag === 'body' ? 2 : 0)
      | (tag === 'img' ? 4 : 0) | (tag === 'a' && node.hasAttribute('href') ? 8 : 0);
    const index = nodes.push(node) - 1;
    entries.push([parent, colour(text), colour(style.backgroundColor), flags,
      border('top'), border('right'), border('bottom'), border('left')]);
    for (const child of node.childNodes) visit(child, index);
  };
  visit(document.documentElement, -1);
  window.mailstreamColoured = nodes;
  const metas = Array.from(document.querySelectorAll('meta[name][content]'))
    .filter((meta) => /^(supported-)?color-scheme$/i.test(meta.name)).map((meta) => meta.content);
  const styles = Array.from(document.querySelectorAll('style'), (style) => style.textContent);
  return JSON.stringify({ metas, styles, nodes: entries });
})()"#;

/// What [`mail_web_recolour`] makes of a web page's colours.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MailWebColours {
    /// The script that moves them, setting each changed element's `style` attribute.
    pub script: String,
    /// Whether the page stays as the mail wrote it, on white paper, which the reader shows as a
    /// sheet (dark mode, mail that can't move).
    pub paper: bool,
}

/// The colours [`MAIL_WEB_COLOURS`] read, moved to `theme`'s surface by the rules Blitz pages
/// follow (`colours::recolour`); the web view is transparent and laid out in the theme's
/// appearance. `None` for an answer it can't read.
#[uniffi::export]
pub fn mail_web_recolour(read: String, theme: HtmlTheme) -> Option<MailWebColours> {
    let page: WebColours = serde_json::from_str(&read).ok()?;
    let schemes = colours::schemes(page.metas.iter().map(String::as_str), page.styles.iter().map(String::as_str));
    let mut children = vec![Vec::new(); page.nodes.len()];
    let mut root = None;
    for (index, node) in page.nodes.iter().enumerate() {
        let parent = match node {
            WebNode::Text(parent, _) | WebNode::Element(parent, ..) => *parent,
        };
        match usize::try_from(parent).ok().and_then(|parent| children.get_mut(parent)) {
            Some(siblings) => siblings.push(index),
            None => root = root.or(Some(index)),
        }
    }
    let tree = WebTree { nodes: &page.nodes, children };
    let colours::Recoloured { changes, paper } = colours::recolour(&tree, root?, &theme.into(), schemes);
    let changes = serde_json::to_string(&changes).expect("changes encode");
    let script = format!(
        "(() => {{
  const nodes = window.mailstreamColoured || [];
  for (const [index, change] of {changes}) {{
    const node = nodes[index], style = node && node.getAttribute('style');
    if (node) node.setAttribute('style', style ? style + ';' + change : change);
  }}
}})();"
    );
    Some(MailWebColours { script, paper })
}

/// [`MAIL_WEB_COLOURS`]'s answer.
#[derive(Deserialize)]
struct WebColours {
    metas: Vec<String>,
    styles: Vec<String>,
    nodes: Vec<WebNode>,
}

/// A node of a web page, with its parent's index (-1 for the root): text as its length; an
/// element as its colour, background colour, flags (1 background image, 2 `html` or `body`, 4
/// `img`, 8 `a[href]`) and its borders' colours.
#[derive(Deserialize)]
#[serde(untagged)]
enum WebNode {
    Text(i64, usize),
    Element(i64, String, String, u8, Option<String>, Option<String>, Option<String>, Option<String>),
}

struct WebTree<'a> {
    nodes: &'a [WebNode],
    children: Vec<Vec<usize>>,
}

impl colours::Page for WebTree<'_> {
    type Id = usize;

    fn styled(&self, id: usize) -> colours::Styled {
        match &self.nodes[id] {
            WebNode::Text(_, length) => colours::Styled::Text(*length),
            WebNode::Element(_, color, background, flags, top, right, bottom, left) => {
                let border = |side: &Option<String>| side.as_deref().and_then(css_colour);
                colours::Styled::Element(colours::Element {
                    // `<canvas>` writes every colour it can draw; others count as black text on
                    // nothing.
                    color: css_colour(color).unwrap_or(Rgba::BLACK),
                    background: css_colour(background).unwrap_or(Rgba { a: 0., ..Rgba::BLACK }),
                    background_image: flags & 1 != 0,
                    page: flags & 2 != 0,
                    img: flags & 4 != 0,
                    link: flags & 8 != 0,
                    borders: [border(top), border(right), border(bottom), border(left)],
                })
            }
        }
    }

    fn children(&self, id: usize) -> impl Iterator<Item = usize> {
        self.children[id].iter().copied()
    }
}

/// A colour as `<canvas>` writes it: `#rrggbb` or `rgba(r, g, b, a)`.
fn css_colour(value: &str) -> Option<Rgba> {
    if let Some(hex) = value.strip_prefix('#') {
        return (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok().map(Rgba::hex)).flatten();
    }
    let inner = value.strip_prefix("rgba(").or_else(|| value.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<f32> = inner.split(',').map(|part| part.trim().parse().ok()).collect::<Option<_>>()?;
    let (r, g, b, a) = match parts[..] {
        [r, g, b] => (r, g, b, 1.),
        [r, g, b, a] => (r, g, b, a),
        _ => return None,
    };
    Some(Rgba { r: r / 255., g: g / 255., b: b / 255., a })
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
        let served = content_policy(InlineParts::Served, true);
        let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{served}\">");
        let page = |html: &str| mail_web_document(html.into(), InlineParts::Served, true);
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
        let page = mail_web_document(mail.clone(), InlineParts::Served, true);
        assert!(page.ends_with("<img src=\"mscid://0a1b/0-logo%20a.png\"><img src=\"file:///etc/x.png\">"), "{page}");
        assert!(page.contains("img-src https: data: mscid:;"));
        let page = mail_web_document(mail.clone(), InlineParts::Files, true);
        assert!(page.ends_with(&mail) && page.contains("img-src https: data: file:;"), "{page}");
        for (inline, remote) in [(InlineParts::Served, true), (InlineParts::Files, true), (InlineParts::Served, false), (InlineParts::Files, false)] {
            assert!(!content_policy(inline, remote).contains("script"));
        }
        assert!(!content_policy(InlineParts::Served, true).contains("file:"));
        assert!(!content_policy(InlineParts::Served, false).contains("file:"));
    }

    #[test]
    fn without_remote_a_page_reads_nothing_from_the_web() {
        for inline in [InlineParts::Served, InlineParts::Files] {
            let page = mail_web_document("<img src=\"https://example.com/a.png\">".into(), inline, false);
            assert!(!page.contains("https:;") && !page.contains("https: "), "{page}");
            assert!(page.contains("img-src data: ") && page.contains("font-src data:;"), "{page}");
        }
    }

    #[test]
    fn the_web_script_carries_the_rules() {
        for view in [MailWebView::WebKit, MailWebView::WebView2] {
            let script = mail_web_script(view);
            assert!(script.contains("blockquote.ms-quote") && script.contains("#divRplyFwdMsg"));
            assert!(script.contains("messageReplySection") && script.contains(".hop_extra"));
            assert!(script.contains("mail-editor-reference-message-container") && script.contains("normalizeId"));
            assert!(script.contains("isTailCutoff") && script.contains("isOutlookHeader"));
            assert!(script.contains("-apple-system, \\\"Segoe UI\\\""));
        }
        assert!(mail_web_script(MailWebView::WebKit).contains("fit-content"));
        assert!(mail_web_script(MailWebView::WebView2).contains("display: flex"));
    }

    #[test]
    fn outlook_header_div_helpers_match_headers_without_false_positives() {
        assert!(has_visible_border_top("border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in"));
        assert!(has_visible_border_top("BORDER-TOP: 1px solid #B5C4DF"));
        assert!(has_visible_border_top("padding: 3pt 0in 0in; border-width: 1pt medium medium; border-style: solid none none; border-color: rgb(181, 196, 223) currentcolor currentcolor;"));
        assert!(has_visible_border_top("border-top-style: solid; border-top-width: 1px"));
        assert!(!has_visible_border_top("border:none;border-top:none"));
        assert!(!has_visible_border_top("border-top: 0pt; padding: 3pt"));
        assert!(!has_visible_border_top("border-top: 0px solid #ccc"));
        assert!(!has_visible_border_top("border-style: none none solid none; border-width: 1pt"));
        assert!(!has_visible_border_top("border-style: solid none none; border-width: 0px 0px 0px"));
        assert!(!has_visible_border_top("border-top: 1px solid #ccc; border: none"));
        assert!(!has_visible_border_top("border-left: 1px solid #ccc"));

        assert_eq!(strip_gmail_id_prefix("mail-editor-reference-message-container"), "mail-editor-reference-message-container");
        assert_eq!(strip_gmail_id_prefix("m_6900309819208902883mail-editor-reference-message-container"), "mail-editor-reference-message-container");
        assert_eq!(strip_gmail_id_prefix("m_-7080766174707785600divRplyFwdMsg"), "divRplyFwdMsg");
        assert_eq!(strip_gmail_id_prefix("m_abc"), "m_abc");
        assert_eq!(strip_gmail_id_prefix("m_-"), "m_-");

        assert!(is_outlook_header_text("From: Alex <alex@example.org>\nSent: Monday, Nov 16, 2026 9:00 AM\nTo: Sam\nSubject: Sync"));
        assert!(is_outlook_header_text("发件人：林明 <ming@example.org>\n发送时间：2026年11月16日\n主题：周会"));
        // Normal prose starting with "From:" or missing a second header line must not match.
        assert!(!is_outlook_header_text("From: the engineering team\nWe are excited to share this update."));
        assert!(!is_outlook_header_text("Sent: 5 items\nTo: warehouse"));
        assert!(!is_outlook_header_text("From: Alex <alex@example.org>"));
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

    #[test]
    fn web_pages_move_to_the_surface_by_the_shared_rules() {
        const DARK: HtmlTheme = HtmlTheme { dark: true, surface: 0x231E1B, text: 0xF6EFE7, link: 0x0A84FF };
        // html > body (page white, near-black text) > [text, a (default link blue) > text, img].
        let page = |metas: &str| {
            format!(
                r##"{{"metas": [{metas}], "styles": [], "nodes": [
                [-1, "#1f1f1f", "rgba(0, 0, 0, 0)", 2, null, null, null, null],
                [0, "#1f1f1f", "#ffffff", 2, null, null, "#dddddd", null],
                [1, 12],
                [1, "#0000ee", "rgba(0, 0, 0, 0)", 8, null, null, null, null],
                [3, 4],
                [1, "#1f1f1f", "rgba(0, 0, 0, 0)", 4, null, null, null, null]]}}"##
            )
        };
        let moved = mail_web_recolour(page(""), DARK).unwrap();
        assert!(!moved.paper);
        let script = &moved.script;
        assert!(script.contains("[1,\"background-color: transparent !important;color: rgba(246, 239, 231, 1) !important;border-bottom-color"), "{script}");
        assert!(script.contains("[3,\"color: rgba(10, 132, 255, 1) !important;\"]"), "{script}");
        // The transparent image keeps the white it was drawn on.
        assert!(script.contains("background-color: rgba(255, 255, 255, 1) !important;"), "{script}");
        let light = mail_web_recolour(page(""), HtmlTheme { dark: false, ..DARK }).unwrap();
        assert!(!light.paper && light.script.contains("[1,\"background-color: transparent !important;\"]"), "{}", light.script);
        let paper = mail_web_recolour(page(r#""only light""#), DARK).unwrap();
        assert!(paper.paper && paper.script.contains("of []"), "{}", paper.script);
        assert_eq!(mail_web_recolour("not json".into(), DARK), None);
    }
}
