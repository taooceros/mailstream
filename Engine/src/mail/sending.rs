//! `mail.send` and `mail.reply`, as the Swift engine's `ControlService` and `MailStore`
//! (`Store/Replies.swift`) do them: new mail and forwards with attachments, and replies that work
//! out their recipients, subject and quote the way mail clients do, threaded under the message
//! replied to, all with an HTML alternative (quoting the original HTML as Gmail lays it out).
//! Both need `yes`; without it the error says what would be sent.
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

use super::Mail;
use crate::answers::{self, Sent};
use crate::cache::SCHEME;
use crate::commands::Arguments;
use crate::mime::OutgoingMessage;
use crate::model::{Address, Date, MailMessage, MailboxRole, MessageContent};
use crate::text;
use crate::EngineError;

/// What a reply goes out with, worked out before it is sent.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplyPlan {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// The source's text with every line `> `-quoted, without the attribution line.
    pub quoted_text: String,
    /// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:"
    pub attribution: String,
}

impl ReplyPlan {
    /// The plain-text body: the written text above the attributed quote.
    pub fn plain_body(&self, text: &str) -> String {
        if self.quoted_text.is_empty() { text.to_string() } else { format!("{text}\n\n{}\n{}", self.attribution, self.quoted_text) }
    }
}

pub(crate) use crate::text::reply_subject;

/// Recipients the way mail clients derive them: `Reply-To`, else `From`, or for one of the user's
/// own messages its recipients; reply-all copies everyone else on the source except the user's
/// own addresses.
pub(crate) fn reply_recipients(
    source: &MailMessage,
    content: Option<&MessageContent>,
    own: &std::collections::HashSet<String>,
    all: bool,
) -> (Vec<Address>, Vec<Address>) {
    let envelope = &source.envelope;
    let reply_to = content.map(|content| content.reply_to.clone()).unwrap_or_default();
    let mut to = if source.is_from_user(own) {
        envelope.to.clone()
    } else if reply_to.is_empty() {
        envelope.from.clone()
    } else {
        reply_to
    };
    if !all {
        return (to, Vec::new());
    }
    let mut seen: std::collections::HashSet<String> = to.iter().map(Address::normalized_email).collect();
    seen.extend(own.iter().cloned());
    let cc_source = content.map(|content| content.cc.as_slice()).unwrap_or_default();
    let mut cc: Vec<Address> =
        envelope.to.iter().chain(cc_source).filter(|address| seen.insert(address.normalized_email())).cloned().collect();
    if to.is_empty() && !cc.is_empty() {
        to = vec![cc.remove(0)];
    }
    (to, cc)
}

/// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:", the date in the local time zone as
/// macOS writes it for English (a narrow no-break space before AM/PM).
pub(crate) fn attribution(source: &MailMessage) -> String {
    let envelope = &source.envelope;
    let sender = envelope.from.first().map_or_else(|| "Someone".to_string(), |from| from.name.clone().unwrap_or_else(|| from.email.clone()));
    let when = envelope.date.map(|date| format!("{}, ", attribution_date(date))).unwrap_or_default();
    format!("On {when}{sender} wrote:")
}

fn attribution_date(date: Date) -> String {
    crate::display::local(date).map(|moment| crate::display::full(&moment)).unwrap_or_default()
}

/// The line breaks of `CharacterSet.newlines`.
fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

pub(crate) fn quote(text: &str) -> String {
    let body = text.replace("\r\n", "\n");
    let body = body.trim_matches(is_newline);
    if body.is_empty() {
        return String::new();
    }
    body.split('\n').map(|line| if line.starts_with('>') { format!(">{line}") } else { format!("> {line}") }).collect::<Vec<_>>().join("\n")
}

const FORWARD_MARKER: &str = "---------- Forwarded message ---------";

/// The fields `display::forward_header` writes under [`FORWARD_MARKER`].
const FORWARD_FIELDS: [&str; 5] = ["From:", "Date:", "Subject:", "To:", "Cc:"];

/// What follows [`FORWARD_MARKER`] in a forward's body, split into the rest of its header (the
/// marker line's end and the field lines under it) and the forwarded text. The header ends at the
/// first line that is no field, so the text is found even when the blank line after it is gone.
fn split_forward_block(after_marker: &str) -> (&str, &str) {
    let mut lines = after_marker.split_inclusive('\n');
    let mut end = lines.next().map_or(0, str::len);
    for line in lines {
        if !FORWARD_FIELDS.iter().any(|field| line.starts_with(field)) {
            break;
        }
        end += line.len();
    }
    let (header, text) = after_marker.split_at(end);
    (header.trim_end_matches('\n'), text.trim_matches('\n'))
}

/// The same text but for blank lines and the whitespace around lines, which compose's prefill
/// drops (GPUI's `format::body`).
fn same_text(a: &str, b: &str) -> bool {
    fn lines(text: &str) -> impl Iterator<Item = &str> {
        text.lines().map(str::trim).filter(|line| !line.is_empty())
    }
    lines(a).eq(lines(b))
}

/// Written plain text as HTML lines with links turned into anchors.
fn lines_html(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| if line.is_empty() { "<div><br></div>".to_string() } else { format!("<div>{}</div>", text::linkify(line)) })
        .collect::<Vec<_>>()
        .join("\n")
}

fn quote_html(source: Option<&MessageContent>) -> Option<String> {
    match source {
        Some(MessageContent { html: Some(body), .. }) => Some(body_contents(body)),
        Some(MessageContent { text: Some(body), .. }) => Some(plain_quote_html(body)),
        _ => None,
    }
}

/// Quoted plain text as HTML, its whitespace kept.
fn plain_quote_html(text: &str) -> String {
    format!("<div style=\"white-space:pre-wrap\">{}</div>", text::plain_to_html(text))
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn is_safe_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('/') {
        return true;
    }
    match trimmed.split_once(':') {
        Some((scheme, _)) if !scheme.contains(['/', '?', '#']) => {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "mailto")
        }
        _ => true,
    }
}

fn is_safe_image_url(url: &str) -> bool {
    let trimmed = url.trim();
    match trimmed.split_once(':') {
        Some((scheme, _)) => matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https"),
        None => false,
    }
}

struct PendingImage {
    url: String,
    title: String,
    alt: String,
}

/// Written Markdown text rendered as clean HTML, with bare URLs and email addresses linkified.
pub(crate) fn markdown_to_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);
    let mut out = String::with_capacity(text.len() * 2);
    let mut in_code_block = false;
    let mut link_stack: Vec<bool> = Vec::new();
    let mut in_table_head = false;
    let mut in_table_body = false;
    let mut pending_image: Option<PendingImage> = None;

    for event in parser {
        match event {
            Event::Start(tag) => {
                if pending_image.is_some() {
                    continue;
                }
                match tag {
                    Tag::Paragraph => out.push_str("<p>"),
                    Tag::Heading { level, .. } => {
                        out.push_str(&format!("<h{}>", heading_level(level)));
                    }
                    Tag::BlockQuote(_) => out.push_str("<blockquote>\n"),
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        match kind {
                            CodeBlockKind::Fenced(lang) => {
                                let lang = lang.split_whitespace().next().unwrap_or("");
                                if lang.is_empty() {
                                    out.push_str("<pre><code>");
                                } else {
                                    out.push_str(&format!("<pre><code class=\"language-{}\">", text::escape_html(lang)));
                                }
                            }
                            CodeBlockKind::Indented => out.push_str("<pre><code>"),
                        }
                    }
                    Tag::List(None) => out.push_str("<ul>\n"),
                    Tag::List(Some(1)) => out.push_str("<ol>\n"),
                    Tag::List(Some(start)) => out.push_str(&format!("<ol start=\"{start}\">\n")),
                    Tag::Item => out.push_str("<li>"),
                    Tag::Emphasis => out.push_str("<em>"),
                    Tag::Strong => out.push_str("<strong>"),
                    Tag::Strikethrough => out.push_str("<del>"),
                    Tag::Link { dest_url, title, .. } => {
                        let safe = is_safe_url(&dest_url);
                        link_stack.push(safe);
                        if safe {
                            out.push_str(&format!("<a href=\"{}\"", text::escape_html(&dest_url)));
                            if !title.is_empty() {
                                out.push_str(&format!(" title=\"{}\"", text::escape_html(&title)));
                            }
                            out.push('>');
                        }
                    }
                    Tag::Image { dest_url, title, .. } => {
                        pending_image = Some(PendingImage {
                            url: dest_url.into_string(),
                            title: title.into_string(),
                            alt: String::new(),
                        });
                    }
                    Tag::Table(_) => {
                        in_table_body = false;
                        out.push_str("<table>\n");
                    }
                    Tag::TableHead => {
                        in_table_head = true;
                        out.push_str("<thead>\n<tr>\n");
                    }
                    Tag::TableRow => {
                        if !in_table_body {
                            in_table_body = true;
                            out.push_str("<tbody>\n");
                        }
                        out.push_str("<tr>\n");
                    }
                    Tag::TableCell => {
                        out.push_str(if in_table_head { "<th>" } else { "<td>" });
                    }
                    _ => {}
                }
            }
            Event::End(tag_end) => {
                if matches!(tag_end, TagEnd::Image) {
                    if let Some(img) = pending_image.take() {
                        if is_safe_image_url(&img.url) {
                            out.push_str(&format!(
                                "<img src=\"{}\" alt=\"{}\"",
                                text::escape_html(&img.url),
                                text::escape_html(&img.alt)
                            ));
                            if !img.title.is_empty() {
                                out.push_str(&format!(" title=\"{}\"", text::escape_html(&img.title)));
                            }
                            out.push('>');
                        } else {
                            out.push_str(&text::escape_html(&img.alt));
                        }
                    }
                    continue;
                }
                if pending_image.is_some() {
                    continue;
                }
                match tag_end {
                    TagEnd::Paragraph => out.push_str("</p>\n"),
                    TagEnd::Heading(level) => {
                        out.push_str(&format!("</h{}>\n", heading_level(level)));
                    }
                    TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        out.push_str("</code></pre>\n");
                    }
                    TagEnd::List(ordered) => out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" }),
                    TagEnd::Item => out.push_str("</li>\n"),
                    TagEnd::Emphasis => out.push_str("</em>"),
                    TagEnd::Strong => out.push_str("</strong>"),
                    TagEnd::Strikethrough => out.push_str("</del>"),
                    TagEnd::Link => {
                        if link_stack.pop() == Some(true) {
                            out.push_str("</a>");
                        }
                    }
                    TagEnd::TableCell => out.push_str(if in_table_head { "</th>\n" } else { "</td>\n" }),
                    TagEnd::TableHead => {
                        in_table_head = false;
                        out.push_str("</tr>\n</thead>\n");
                    }
                    TagEnd::TableRow => out.push_str("</tr>\n"),
                    TagEnd::Table => {
                        if in_table_body {
                            in_table_body = false;
                            out.push_str("</tbody>\n");
                        }
                        out.push_str("</table>\n");
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some(img) = &mut pending_image {
                    img.alt.push_str(&t);
                } else if in_code_block || link_stack.iter().any(|&open| open) {
                    out.push_str(&text::escape_html(&t));
                } else {
                    out.push_str(&text::linkify(&t));
                }
            }
            Event::Code(t) => {
                if let Some(img) = &mut pending_image {
                    img.alt.push_str(&t);
                } else {
                    out.push_str(&format!("<code>{}</code>", text::escape_html(&t)));
                }
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                if let Some(img) = &mut pending_image {
                    img.alt.push_str(&h);
                } else {
                    out.push_str(&text::escape_html(&h));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(img) = &mut pending_image {
                    img.alt.push(' ');
                } else {
                    out.push_str("<br>\n");
                }
            }
            Event::Rule => out.push_str("<hr>\n"),
            Event::TaskListMarker(checked) => {
                out.push_str(if checked {
                    "<input type=\"checkbox\" disabled checked> "
                } else {
                    "<input type=\"checkbox\" disabled> "
                });
            }
            Event::FootnoteReference(name) | Event::InlineMath(name) | Event::DisplayMath(name) => {
                out.push_str(&text::escape_html(&name));
            }
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn html_document_with(written_html: String, quoted: Option<(&str, String)>) -> String {
    let written_html = if written_html.is_empty() { "<div><br></div>".to_string() } else { written_html };
    let mut html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\n<div dir=\"auto\">{written_html}</div>\n");
    if let Some((attribution, quote)) = quoted {
        let attr = attribution
            .replace("\r\n", "\n")
            .split('\n')
            .map(text::escape_html)
            .collect::<Vec<_>>()
            .join("<br>");
        html.push_str(&format!(
            "<br><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">{attr}<br></div>\n\
             <blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">\n\
             {quote}\n</blockquote></div>\n"
        ));
    }
    html + "</body></html>\n"
}

#[cfg(test)]
fn html_document(written: &str, quoted: Option<(&str, String)>) -> String {
    html_document_with(lines_html(written), quoted)
}

fn markdown_document(written: &str, quoted: Option<(&str, String)>) -> String {
    html_document_with(markdown_to_html(written), quoted)
}

/// The HTML alternative of a reply, laid out the way Gmail does so recipients' clients fold the
/// quote.
#[cfg(test)]
pub(crate) fn reply_html(reply: &str, attribution: &str, source: Option<&MessageContent>) -> String {
    html_document(reply, quote_html(source).map(|quote| (attribution, quote)))
}

/// The HTML alternative of a Markdown reply, laid out the way Gmail does so recipients' clients
/// fold the quote.
pub(crate) fn reply_markdown_html(reply: &str, attribution: &str, source: Option<&MessageContent>) -> String {
    markdown_document(reply, quote_html(source).map(|quote| (attribution, quote)))
}

/// Plain-text and HTML bodies for a forward of `source`: keeps `body` as `text/plain` when it
/// already carries the forwarded quote block (as compose prefills it), otherwise appends the
/// plain-text quote; when `source` had HTML, quotes its HTML in `.gmail_quote` (dropping inline
/// `mscid:`/`file:` images) unless the user changed the forwarded text, in which case the HTML
/// quote is built from the edited plain text so neither part leaks removed content.
fn forward_bodies(body: &str, source: &MailMessage, content: &MessageContent) -> (String, String) {
    let cc = content.cc.as_slice();
    let plain_source = content
        .text
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| content.html.as_deref().map(text::strip_html))
        .unwrap_or_default();
    let forwarded = crate::display::Forwarded {
        from: &source.envelope.from,
        to: &source.envelope.to,
        cc,
        date: source.envelope.date.and_then(crate::display::local),
        subject: &source.envelope.subject,
        text: &plain_source,
    };
    let normalized = body.replace("\r\n", "\n");
    let has_marker = normalized.contains(FORWARD_MARKER);
    let text = if has_marker {
        body.to_string()
    } else {
        let quoted = crate::display::forward_body(&forwarded);
        if body.is_empty() { quoted } else { format!("{body}\n\n{quoted}") }
    };
    let html = match content.html.as_deref() {
        Some(orig_html) => {
            if let Some((before, after)) = normalized.split_once(FORWARD_MARKER) {
                let written = before.trim_end_matches('\n');
                let (header, quoted_text) = split_forward_block(after);
                let attr = format!("{FORWARD_MARKER}{header}");
                let quote = if same_text(quoted_text, &plain_source) {
                    body_contents(orig_html)
                } else {
                    plain_quote_html(quoted_text)
                };
                markdown_document(written, Some((&attr, quote)))
            } else {
                let attr = crate::display::forward_header(&forwarded);
                markdown_document(body, Some((&attr, body_contents(orig_html))))
            }
        }
        None => {
            let written_html = if let Some((before, after)) = normalized.split_once(FORWARD_MARKER) {
                let written = markdown_to_html(before.trim_end_matches('\n'));
                let block = lines_html(&format!("{FORWARD_MARKER}{after}"));
                if written.is_empty() { block } else { format!("{written}\n<div><br></div>\n{block}") }
            } else {
                let written = markdown_to_html(body);
                let quoted = lines_html(&crate::display::forward_body(&forwarded));
                if written.is_empty() { quoted } else { format!("{written}\n<div><br></div>\n{quoted}") }
            };
            html_document_with(written_html, None)
        }
    };
    (text, html)
}

/// The inside of `<body>`, without images of inline parts: those are not re-attached, so they
/// would arrive broken.
fn body_contents(html: &str) -> String {
    static OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)<body[^>]*>").unwrap());
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)</body>").unwrap());
    static FRAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)<head[^>]*>[\s\S]*?</head>|<!doctype[^>]*>|</?html[^>]*>").unwrap());
    static INLINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#"(?i)<img\b[^>]*\bsrc\s*=\s*["']?(?:{SCHEME}|file):[^>]*>"#)).unwrap());
    let inner = match OPEN.find(html) {
        Some(open) => {
            let rest = &html[open.end()..];
            match CLOSE.find_iter(rest).last() {
                Some(close) => rest[..close.start()].to_string(),
                None => rest.to_string(),
            }
        }
        None => FRAME.replace_all(html, "").into_owned(),
    };
    INLINE.replace_all(&inner, "").into_owned()
}

/// The addresses as a confirmation names them.
fn emails(list: &[Address]) -> String {
    list.iter().map(|address| address.email.as_str()).collect::<Vec<_>>().join(", ")
}

/// What `mail.send` and `mail.reply` answer: what was sent, with `"sent": true`.
fn sent(sent: Sent) -> Value {
    let mut answer = answers::json(&sent);
    answer["sent"] = json!(true);
    answer
}

fn texts(list: &[Address]) -> Vec<String> {
    list.iter().map(Address::text).collect()
}

fn confirm(message: String) -> EngineError {
    EngineError::new("confirm", message)
}

impl Mail {
    pub(super) async fn send_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let from = args.required("from")?.to_lowercase();
        if !self.state().accounts.iter().any(|account| account.name == from) {
            return Err(Self::unknown_account(&from));
        }
        let (to, cc, bcc) = (addresses(args, "to")?, addresses(args, "cc")?, addresses(args, "bcc")?);
        if to.is_empty() {
            return Err(EngineError::usage("--to needs at least one address."));
        }
        let forward = match args.string("forward") {
            Some(id) => {
                let conversation = self.lookup(&id)?;
                let source = match args.string("forward-message") {
                    Some(message) => conversation
                        .messages
                        .iter()
                        .find(|m| m.envelope.id == message)
                        .ok_or_else(|| EngineError::usage(format!("Message {message} is not in the conversation.")))?,
                    None => conversation
                        .messages
                        .iter()
                        .rev()
                        .find(|m| m.role != MailboxRole::Drafts)
                        .unwrap_or_else(|| conversation.latest()),
                }
                .clone();
                Some(source)
            }
            None if args.string("forward-message").is_some() => {
                return Err(EngineError::usage("--forward-message requires --forward."));
            }
            None => None,
        };
        let (subject, body) = (args.required("subject")?, args.required("body")?);
        let attachments = files(&args.list("attachments").unwrap_or_default(), "attachments")?;
        let attached = match attachments.len() {
            0 => String::new(),
            1 => ", 1 attachment".into(),
            count => format!(", {count} attachments"),
        };
        if !args.bool("yes") {
            let everyone: Vec<Address> = to.iter().chain(&cc).chain(&bcc).cloned().collect();
            return Err(confirm(format!(
                "Would send \"{subject}\" ({} characters{attached}) from {from} to {}. Re-run with --yes.",
                body.graphemes(true).count(),
                emails(&everyone)
            )));
        }
        let (text, html) = match &forward {
            Some(source) => {
                let content =
                    self.contents.load(&source.message_ref(), self.backend(source.account())).await.map_err(EngineError::failed)?;
                forward_bodies(&body, source, &content)
            }
            None => (body.clone(), markdown_document(&body, None)),
        };
        let message = self.outgoing(&from, None, to.clone(), cc, bcc, subject.clone(), text, Some(html), attachments).await;
        self.deliver(&message, &from).await?;
        Ok(sent(Sent { from, to: texts(&to), subject }))
    }

    pub(super) async fn reply_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let conversation = self.lookup(&args.required("id")?)?;
        let source = match args.string("message") {
            Some(id) => conversation
                .messages
                .iter()
                .find(|message| message.envelope.id == id)
                .ok_or_else(|| EngineError::usage(format!("Message {id} is not in the conversation.")))?,
            None => conversation
                .messages
                .iter()
                .rev()
                .find(|message| message.role != MailboxRole::Drafts)
                .unwrap_or_else(|| conversation.latest()),
        }
        .clone();
        let mut plan = self.reply_plan(&source, args.bool("all")).await?;
        if args.list("to").is_some() {
            plan.to = addresses(args, "to")?;
        }
        if args.list("cc").is_some() {
            plan.cc = addresses(args, "cc")?;
        }
        let bcc = addresses(args, "bcc")?;
        if let Some(subject) = args.string("subject") {
            plan.subject = subject;
        }
        let recipients: Vec<Address> = plan.to.iter().chain(&plan.cc).chain(&bcc).cloned().collect();
        if recipients.is_empty() {
            return Err(EngineError::usage("A reply needs at least one recipient."));
        }
        let attachments = files(&args.list("attachments").unwrap_or_default(), "attachments")?;
        let body = args.required("body")?;
        let account = source.account().to_string();
        if !args.bool("yes") {
            return Err(confirm(format!(
                "Would send \"{}\" from {account} to {}, {} attachments. Re-run with --yes.",
                plan.subject,
                emails(&recipients),
                attachments.len()
            )));
        }
        let content = self.contents.load(&source.message_ref(), self.backend(&account)).await.ok();
        let html = reply_markdown_html(&body, &plan.attribution, content.as_deref());
        let message = self
            .outgoing(
                &account,
                Some(&source),
                plan.to.clone(),
                plan.cc.clone(),
                bcc,
                plan.subject.clone(),
                plan.plain_body(&body),
                Some(html),
                attachments,
            )
            .await;
        self.deliver(&message, &account).await?;
        Ok(sent(Sent { from: account, to: texts(&recipients), subject: plan.subject }))
    }

    async fn reply_plan(self: &Arc<Self>, source: &MailMessage, all: bool) -> Result<ReplyPlan, EngineError> {
        let content =
            self.contents.load(&source.message_ref(), self.backend(source.account())).await.map_err(EngineError::failed)?;
        let own = Self::own_addresses(&self.state());
        let (to, cc) = reply_recipients(source, Some(&content), &own, all);
        Ok(ReplyPlan {
            to,
            cc,
            subject: reply_subject(&source.envelope.subject),
            quoted_text: quote(content.text.as_deref().unwrap_or("")),
            attribution: attribution(source),
        })
    }

    /// The message `account` sends, from its identity; a reply to `source` is threaded under it.
    #[allow(clippy::too_many_arguments)]
    async fn outgoing(
        self: &Arc<Self>,
        account: &str,
        source: Option<&MailMessage>,
        to: Vec<Address>,
        cc: Vec<Address>,
        bcc: Vec<Address>,
        subject: String,
        text: String,
        html: Option<String>,
        attachments: Vec<PathBuf>,
    ) -> OutgoingMessage {
        let from = self
            .state()
            .accounts
            .iter()
            .find(|known| known.name == account)
            .and_then(|known| known.identity.clone())
            .unwrap_or_else(|| Address::new(None, account));
        let mut message = OutgoingMessage {
            message_id: crate::mime::message_id(),
            from,
            to,
            cc,
            bcc,
            subject,
            text,
            html,
            attachments,
            ..OutgoingMessage::default()
        };
        let Some(source) = source else { return message };
        let references = match self.contents.load(&source.message_ref(), self.backend(account)).await {
            Ok(content) => content.references.clone(),
            Err(_) => Vec::new(),
        };
        message.thread = source.envelope.thread.clone();
        match &source.envelope.message_id {
            Some(id) => {
                message.in_reply_to = Some(id.clone());
                message.references = references.into_iter().filter(|reference| reference != id).chain([id.clone()]).collect();
            }
            None => message.references = references,
        }
        message
    }

    /// Sends from `account`, the message listing in its conversation from the start (`pending`),
    /// then picks up the sent copy a moment later. A failed send leaves the lists again.
    async fn deliver(self: &Arc<Self>, message: &OutgoingMessage, account: &str) -> Result<(), EngineError> {
        let backend = self.backend(account).ok_or_else(|| EngineError::failed("The account is no longer signed in."))?;
        let local = self.sending(message, account);
        match backend.send(message).await {
            Ok(copy) => {
                if let Some(local) = &local {
                    self.delivered(local, copy);
                }
            }
            Err(error) => {
                if let Some(local) = &local {
                    self.undelivered(local);
                }
                return Err(EngineError::failed(error.message));
            }
        }
        let (mail, account) = (self.clone(), account.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            mail.refresh_counts();
            let keys: Vec<_> = Self::default_keys(&mail.state()).into_iter().filter(|key| key.account == account).collect();
            mail.fetch_first_pages(&keys).await;
        });
        Ok(())
    }
}

/// The addresses of list argument `name`; `usage` names the first that is not one.
fn addresses(args: &Arguments, name: &str) -> Result<Vec<Address>, EngineError> {
    args.list(name)
        .unwrap_or_default()
        .into_iter()
        .map(|raw| {
            Address::parse(&raw)
                .filter(|address| address.email.contains('@'))
                .ok_or_else(|| EngineError::usage(format!("--{name}: \"{raw}\" is not an email address.")))
        })
        .collect()
}

/// The files named by full path, each one a readable file rather than a folder.
fn files(paths: &[String], argument: &str) -> Result<Vec<PathBuf>, EngineError> {
    paths
        .iter()
        .map(|path| {
            let readable = Path::new(path).is_absolute()
                && std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir())
                && std::fs::File::open(path).is_ok();
            if readable {
                Ok(PathBuf::from(path))
            } else {
                Err(EngineError::usage(format!("--{argument}: \"{path}\" is not a readable file (give its full path).")))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Envelope, MailboxKey};

    fn sample_source() -> MailMessage {
        let key = Arc::new(MailboxKey::new("me@x.com", "INBOX"));
        let envelope = Arc::new(Envelope {
            id: "m1".into(),
            thread: Some("t1".into()),
            message_id: Some("m1@x.com".into()),
            in_reply_to: Vec::new(),
            flags: Default::default(),
            subject: "Q4 plan".into(),
            from: vec![Address::new(Some("Jordan".into()), "jordan@x.com")],
            to: vec![Address::new(Some("Sam".into()), "me@x.com")],
            date: Some(783_800_000.0),
            size: 100,
            has_attachment: None,
            tags: Default::default(),
            provider_category: None,
            important: false,
        });
        MailMessage { key, envelope, role: MailboxRole::Inbox }
    }

    #[test]
    fn html_document_linkifies_lines_and_preserves_blank_lines() {
        let html = html_document("See https://example.com\n\nWrite bob@x.com <ok>", None);
        assert_eq!(
            html,
            "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\n\
             <div dir=\"auto\"><div>See <a href=\"https://example.com\">https://example.com</a></div>\n\
             <div><br></div>\n\
             <div>Write <a href=\"mailto:bob@x.com\">bob@x.com</a> &lt;ok&gt;</div></div>\n\
             </body></html>\n"
        );
    }

    #[test]
    fn body_contents_drops_mscid_and_file_inline_images() {
        let raw = "<html><head><style>p{}</style></head><body>\
            <p>Keep <img src=\"https://example.com/a.png\"></p>\
            <img src=\"mscid://m1/0.png\">\
            <img alt=\"local\" src='file:///tmp/x.png'>\
            <IMG SRC=FILE:///var/folders/y.jpg>\
            </body></html>";
        assert_eq!(body_contents(raw), "<p>Keep <img src=\"https://example.com/a.png\"></p>");
    }

    #[test]
    fn forward_bodies_handles_html_and_plain_sources() {
        let source = sample_source();
        let html_content = MessageContent {
            html: Some("<body><p>Hello <b>Sam</b></p><img src=\"mscid://m1/logo.png\"><img src=\"file:///tmp/a.png\"></body>".into()),
            text: Some("Hello Sam".into()),
            cc: Vec::new(),
            reply_to: Vec::new(),
            references: Vec::new(),
            attachments: Vec::new(),
            snippet: String::new(),
            invitation: None,
        };
        let prefilled = "FYI https://example.com\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\nSubject: Q4 plan\nTo: Sam <me@x.com>\n\nHello Sam";
        let (plain, html) = forward_bodies(prefilled, &source, &html_content);
        assert_eq!(plain, prefilled);
        assert!(html.contains("<div dir=\"auto\"><p>FYI <a href=\"https://example.com\">https://example.com</a></p></div>"), "{html}");
        assert!(html.contains("<div dir=\"ltr\" class=\"gmail_attr\">---------- Forwarded message ---------<br>From: Jordan &lt;jordan@x.com&gt;<br>Subject: Q4 plan<br>To: Sam &lt;me@x.com&gt;<br></div>"), "{html}");
        assert!(html.contains("<p>Hello <b>Sam</b></p>"), "{html}");
        assert!(!html.contains("mscid:") && !html.contains("file:///"), "{html}");

        // Editing or deleting part of the forwarded quote replaces the original HTML quote with the
        // edited plain text so deleted content is not leaked in `text/html`.
        let multi_html_content = MessageContent {
            html: Some("<body><p>Public line</p><p>Secret: <b>1234</b></p></body>".into()),
            text: Some("Public line\nSecret: 1234".into()),
            ..html_content.clone()
        };
        let redacted = "FYI\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\nSubject: Q4 plan\nTo: Sam <me@x.com>\n\nPublic line";
        let (redacted_plain, redacted_html) = forward_bodies(redacted, &source, &multi_html_content);
        assert_eq!(redacted_plain, redacted);
        assert!(redacted_html.contains("Public line") && !redacted_html.contains("Secret") && !redacted_html.contains("1234"), "{redacted_html}");
        assert!(redacted_html.contains("<div class=\"gmail_quote\">") && redacted_html.contains("<div class=\"ms-plain\">"), "{redacted_html}");

        // Deleting the blank line after the forward header still separates header from body and
        // does not duplicate the body into `.gmail_attr`.
        let no_blank = "FYI\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\nTo: Sam <me@x.com>\nPublic line\nSecret: 1234";
        let (_, no_blank_html) = forward_bodies(no_blank, &source, &multi_html_content);
        assert_eq!(no_blank_html.matches("Public line").count(), 1, "{no_blank_html}");
        assert!(no_blank_html.contains("<p>Secret: <b>1234</b></p>"), "{no_blank_html}");

        // Edited without the blank line: still puts only headers in `.gmail_attr` and edited text in the quote.
        let no_blank_edited = "FYI\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\nTo: Sam <me@x.com>\nPublic line";
        let (_, no_blank_edited_html) = forward_bodies(no_blank_edited, &source, &multi_html_content);
        assert_eq!(no_blank_edited_html.matches("Public line").count(), 1, "{no_blank_edited_html}");
        assert!(!no_blank_edited_html.contains("Secret"), "{no_blank_edited_html}");

        // Bare forward without intro text.
        let bare = "\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\n\nHello Sam";
        let (plain_bare, html_bare) = forward_bodies(bare, &source, &html_content);
        assert_eq!(plain_bare, bare);
        assert!(html_bare.contains("<div dir=\"auto\"><div><br></div></div>\n<br><div class=\"gmail_quote\">"), "{html_bare}");

        // CLI forward without prefilled marker adds plain-text quote and HTML quote.
        let (cli_plain, cli_html) = forward_bodies("Take a look", &source, &html_content);
        assert!(cli_plain.starts_with("Take a look\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>"), "{cli_plain}");
        assert!(cli_plain.ends_with("\n\nHello Sam"), "{cli_plain}");
        assert!(cli_html.contains("<p>Take a look</p>") && cli_html.contains("<p>Hello <b>Sam</b></p>"), "{cli_html}");

        // Plain-text-only source produces linkified line HTML without a redundant .gmail_quote block.
        let plain_content = MessageContent {
            html: None,
            text: Some("See www.example.org".into()),
            cc: Vec::new(),
            reply_to: Vec::new(),
            references: Vec::new(),
            attachments: Vec::new(),
            snippet: String::new(),
            invitation: None,
        };
        let plain_prefilled = "FYI\n\n---------- Forwarded message ---------\nFrom: Jordan <jordan@x.com>\n\nSee www.example.org";
        let (plain_only_text, plain_only_html) = forward_bodies(plain_prefilled, &source, &plain_content);
        assert_eq!(plain_only_text, plain_prefilled);
        assert!(!plain_only_html.contains("gmail_quote"), "{plain_only_html}");
        assert!(plain_only_html.contains("<a href=\"http://www.example.org\">www.example.org</a>"), "{plain_only_html}");
    }

    #[test]
    fn markdown_to_html_renders_formatting_lists_links_and_code() {
        let md = "# Heading\n\n**Bold**, *italic*, ~~strike~~, and `inline <code/>`.\n\n- First item\n- Second item\n\n1. Step one\n2. Step two\n\n> Quoted line\n\n[Mailstream](https://example.com) and bare https://bare.example\n\n```rust\nlet x = 1 < 2;\n```";
        let html = markdown_to_html(md);
        assert!(html.contains("<h1>Heading</h1>"), "{html}");
        assert!(html.contains("<strong>Bold</strong>, <em>italic</em>, <del>strike</del>, and <code>inline &lt;code/&gt;</code>."), "{html}");
        assert!(html.contains("<ul>\n<li>First item</li>\n<li>Second item</li>\n</ul>"), "{html}");
        assert!(html.contains("<ol>\n<li>Step one</li>\n<li>Step two</li>\n</ol>"), "{html}");
        assert!(html.contains("<blockquote>\n<p>Quoted line</p>\n</blockquote>"), "{html}");
        assert!(html.contains("<a href=\"https://example.com\">Mailstream</a>"), "{html}");
        assert!(html.contains("<a href=\"https://bare.example\">https://bare.example</a>"), "{html}");
        assert!(html.contains("<pre><code class=\"language-rust\">let x = 1 &lt; 2;\n</code></pre>"), "{html}");

        // Unsafe link schemes are stripped while keeping text; inline HTML is escaped.
        let unsafe_md = "[click](javascript:alert(1)) <script>bad</script>";
        let unsafe_html = markdown_to_html(unsafe_md);
        assert!(!unsafe_html.contains("javascript:"), "{unsafe_html}");
        assert!(unsafe_html.contains("click &lt;script&gt;bad&lt;/script&gt;"), "{unsafe_html}");
    }
}
