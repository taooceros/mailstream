//! A Chat message's text as front ends draw it (MS-235). Google answers `formattedText` in
//! Markdown for projects without a Chat app configuration (Mailstream's, MS-56): CommonMark with
//! GitHub's tables and strikethrough, escapes for what is meant literally (`\*\*`, `1\.`), bare
//! links, and mentions as `<chat-user data-user="users/…">` or `data-email="…"`; older messages
//! carry Chat's own tokens, `<users/…>` mentions and `<url|label>` links. Lines are wrapped at 80
//! columns, so a single line break is a space; a line break the sender typed comes as a hard
//! break.
//!
//! [`parse`] lays it out as a flat list of blocks (paragraphs, headings, code, rules, tables),
//! each with its quote depth and list indentation, holding runs of styled text; [`plain`] is the
//! one line previews show.
use std::sync::LazyLock;

use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};
use regex::Regex;

/// How a run of text is drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct ChatStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// A mention of a person; the text is `@Name`.
    pub mention: bool,
    /// Where the text links to.
    pub link: Option<String>,
}

/// A run of a message's text and how it is drawn.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatSpan {
    pub text: String,
    pub style: ChatStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChatBlockKind {
    Paragraph,
    /// 1 to 6.
    Heading { level: u8 },
    /// A fenced or indented block: one span, its text as written.
    Code,
    Rule,
    /// Its [`ChatBlock::rows`], the header row first.
    Table,
}

/// One block of a message, in order: blocks are drawn one below the other.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatBlock {
    pub kind: ChatBlockKind,
    pub spans: Vec<ChatSpan>,
    /// A table's rows of cells.
    pub rows: Vec<Vec<Vec<ChatSpan>>>,
    /// How many quotes it sits in.
    pub quote: u32,
    /// How many lists it sits in.
    pub indent: u32,
    /// The first block of a list item carries the item's marker: `•`, or `3.` in a numbered list.
    pub marker: Option<String>,
    /// It sits in a tight list item (no blank lines between items): the gap above it is a line's
    /// rather than a paragraph's.
    pub tight: bool,
}

impl ChatBlock {
    fn new(kind: ChatBlockKind) -> Self {
        Self { kind, spans: Vec::new(), rows: Vec::new(), quote: 0, indent: 0, marker: None, tight: false }
    }
}

static MENTION_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<chat-user\s+data-(user|email)="([^"]*)"[^>]*>(?:\s*</chat-user>)?|</chat-user>"#).unwrap());
/// Chat's older tokens: `<url|label>` links and `<users/…>` mentions.
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(https?://[^|>\s]+)(?:\|([^>\n]*))?>|<(users/[^>\s]+)>").unwrap());

/// `markup` laid out; `name` gives the name a mention shows for a `users/…` id or an address
/// (`users/all` reads as "all").
pub fn parse(markup: &str, name: &dyn Fn(&str) -> Option<String>) -> Vec<ChatBlock> {
    let mut builder = Builder::new(name);
    for event in Parser::new_ext(markup, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        builder.event(event);
    }
    builder.close();
    builder.blocks
}

/// `text` in Google Chat's input syntax (`*bold*`, `_italic_`, `~strike~`, `` `code` ``,
/// ```` ```code``` ````, `<url|label>`, `<users/…>`, bare URLs, single line breaks preserved) laid
/// out for a local pending message before the server returns `formattedText`.
pub fn parse_input(text: &str, name: &dyn Fn(&str) -> Option<String>) -> Vec<ChatBlock> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = Vec::new();
    let mut pos = 0;
    while pos < normalized.len() {
        let Some(rel_open) = normalized[pos..].find("```") else {
            push_input_prose(&normalized[pos..], name, &mut blocks);
            break;
        };
        let open = pos + rel_open;
        let Some(rel_close) = normalized[open + 3..].find("```") else {
            push_input_prose(&normalized[pos..], name, &mut blocks);
            break;
        };
        let close = open + 3 + rel_close;
        push_input_prose(&normalized[pos..open], name, &mut blocks);
        let code = &normalized[open + 3..close];
        let code = code.strip_prefix('\n').unwrap_or(code);
        let code = code.strip_suffix('\n').unwrap_or(code);
        let mut block = ChatBlock::new(ChatBlockKind::Code);
        block.spans.push(ChatSpan { text: code.to_string(), style: ChatStyle { code: true, ..ChatStyle::default() } });
        blocks.push(block);
        pos = close + 3;
    }
    blocks
}

fn push_input_prose(prose: &str, name: &dyn Fn(&str) -> Option<String>, blocks: &mut Vec<ChatBlock>) {
    let mut start = 0;
    let bytes = prose.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'\n' {
                j += 1;
            }
            if j - i >= 2 {
                push_input_paragraph(&prose[start..i], name, blocks);
                start = j;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    push_input_paragraph(&prose[start..], name, blocks);
}

fn push_input_paragraph(raw: &str, name: &dyn Fn(&str) -> Option<String>, blocks: &mut Vec<ChatBlock>) {
    let para = raw.trim_matches('\n');
    if para.trim().is_empty() {
        return;
    }
    let mut spans = Vec::new();
    parse_input_inline(para, &ChatStyle::default(), name, &mut spans);
    trim_end(&mut spans);
    if let Some(first) = spans.first_mut() {
        let trimmed = first.text.trim_start().len();
        first.text.drain(..first.text.len() - trimmed);
    }
    spans.retain(|span| !span.text.is_empty());
    if !spans.is_empty() {
        let mut block = ChatBlock::new(ChatBlockKind::Paragraph);
        block.spans = spans;
        blocks.push(block);
    }
}

fn push_input_span(spans: &mut Vec<ChatSpan>, text: &str, style: &ChatStyle) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if &last.style == style => last.text.push_str(text),
        _ => spans.push(ChatSpan { text: text.to_string(), style: style.clone() }),
    }
}

fn can_open_delim(text: &str, at: usize, delim: char) -> bool {
    let prev = text[..at].chars().next_back();
    let next = text[at + delim.len_utf8()..].chars().next();
    prev.is_none_or(|c| !c.is_ascii_alphanumeric() && c != delim)
        && next.is_some_and(|c| !c.is_whitespace() && c != delim)
}

fn can_close_delim(text: &str, at: usize, delim: char) -> bool {
    let prev = text[..at].chars().next_back();
    let next = text[at + delim.len_utf8()..].chars().next();
    prev.is_some_and(|c| !c.is_whitespace() && c != delim)
        && next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != delim)
}

fn find_closing_delim(text: &str, from: usize, delim: char) -> Option<usize> {
    let line_end = text[from..].find('\n').map_or(text.len(), |n| from + n);
    let mut i = from;
    while i < line_end {
        let rest = &text[i..line_end];
        if rest.starts_with('`')
            && let Some(rel_close) = rest[1..].find('`')
            && rel_close > 0
        {
            i += 1 + rel_close + 1;
            continue;
        }
        if rest.starts_with('<')
            && let Some(cap) = TOKEN.captures(rest)
            && cap.get(0).is_some_and(|m| m.start() == 0)
        {
            i += cap.get(0).unwrap().end();
            continue;
        }
        if (rest.starts_with("http://") || rest.starts_with("https://"))
            && let Some(link) = LinkFinder::new().kinds(&[LinkKind::Url]).links(rest).next()
            && link.start() == 0
        {
            let end = i + link.end();
            if link.as_str().ends_with(delim) && can_close_delim(text, end - 1, delim) {
                return Some(end - 1);
            }
            i = end;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if ch == delim && can_close_delim(text, i, delim) {
            return Some(i);
        }
        i += ch.len_utf8();
    }
    None
}

fn parse_input_inline(
    text: &str,
    style: &ChatStyle,
    name: &dyn Fn(&str) -> Option<String>,
    out: &mut Vec<ChatSpan>,
) {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('`') {
            let line_end = rest[1..].find('\n').map_or(rest.len(), |n| 1 + n);
            if let Some(rel_close) = rest[1..line_end].find('`')
                && rel_close > 0
            {
                push_input_span(out, &rest[1..1 + rel_close], &ChatStyle { code: true, ..style.clone() });
                i += 1 + rel_close + 1;
                continue;
            }
        }
        if rest.starts_with('<')
            && let Some(cap) = TOKEN.captures(rest)
            && cap.get(0).is_some_and(|m| m.start() == 0)
        {
            match (cap.get(1), cap.get(3)) {
                (Some(url), _) => {
                    let label = cap.get(2).map(|m| m.as_str()).filter(|s| !s.is_empty());
                    let linked = ChatStyle { link: Some(url.as_str().into()), ..style.clone() };
                    push_input_span(out, label.unwrap_or(url.as_str()), &linked);
                }
                (None, Some(user)) => {
                    let resolved = if user.as_str() == "users/all" { Some("all".to_string()) } else { name(user.as_str()) };
                    push_input_span(
                        out,
                        &format!("@{}", resolved.unwrap_or_else(|| "someone".into())),
                        &ChatStyle { mention: true, ..ChatStyle::default() },
                    );
                }
                _ => {}
            }
            i += cap.get(0).unwrap().end();
            continue;
        }
        if style.link.is_none()
            && (rest.starts_with("http://") || rest.starts_with("https://"))
            && let Some(link) = LinkFinder::new().kinds(&[LinkKind::Url]).links(rest).next()
            && link.start() == 0
        {
            let linked = ChatStyle { link: Some(link.as_str().to_string()), ..style.clone() };
            push_input_span(out, link.as_str(), &linked);
            i += link.end();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if matches!(ch, '*' | '_' | '~')
            && can_open_delim(text, i, ch)
            && let Some(close) = find_closing_delim(text, i + 1, ch)
        {
            let inner_style = match ch {
                '*' => ChatStyle { bold: true, ..style.clone() },
                '_' => ChatStyle { italic: true, ..style.clone() },
                '~' => ChatStyle { strike: true, ..style.clone() },
                _ => unreachable!(),
            };
            parse_input_inline(&text[i + 1..close], &inner_style, name, out);
            i = close + 1;
            continue;
        }
        let mut next = i + ch.len_utf8();
        while next < text.len() {
            let tail = &text[next..];
            let b = tail.as_bytes()[0];
            if matches!(b, b'`' | b'<' | b'*' | b'_' | b'~')
                || (style.link.is_none() && (tail.starts_with("http://") || tail.starts_with("https://")))
            {
                break;
            }
            next += tail.chars().next().unwrap().len_utf8();
        }
        push_input_span(out, &text[i..next], style);
        i = next;
    }
}

/// One line for list previews: the text without its markup, mentions as "@", whitespace runs as
/// one space.
pub fn plain(markup: &str) -> String {
    let blocks = parse(markup, &|_| Some(String::new()));
    let mut words = Vec::new();
    for block in &blocks {
        let cells = block.rows.iter().flatten().map(|cell| cell.iter().map(|span| span.text.as_str()).collect::<String>());
        let text: String = block.spans.iter().map(|span| span.text.as_str()).collect();
        for piece in std::iter::once(text).chain(cells) {
            words.extend(piece.split_whitespace().map(String::from));
        }
    }
    words.join(" ")
}

struct Builder<'a> {
    name: &'a dyn Fn(&str) -> Option<String>,
    blocks: Vec<ChatBlock>,
    /// The open paragraph, heading or code block.
    current: Option<ChatBlock>,
    quote: u32,
    /// Each open list's next number; `None` for bullets.
    lists: Vec<Option<u64>>,
    /// The marker of the item just opened, for its first block.
    marker: Option<String>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: Option<String>,
    /// Spans before an image's alt text, to tell whether it had any.
    image_start: Option<usize>,
    /// What an autolink in Chat's older form (`<url|label>`) shows in place of its text.
    label: Option<String>,
    /// Text not laid out yet, in the current style.
    pending: String,
    table: Option<Table>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<ChatSpan>>>,
    row: Vec<Vec<ChatSpan>>,
    cell: Option<Vec<ChatSpan>>,
}

impl<'a> Builder<'a> {
    fn new(name: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            name,
            blocks: Vec::new(),
            current: None,
            quote: 0,
            lists: Vec::new(),
            marker: None,
            bold: 0,
            italic: 0,
            strike: 0,
            link: None,
            image_start: None,
            label: None,
            pending: String::new(),
            table: None,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.current {
                Some(block) if block.kind == ChatBlockKind::Code => block.spans[0].text.push_str(&text),
                _ => match self.label.take() {
                    Some(label) => self.pending.push_str(&label),
                    None => self.pending.push_str(&text),
                },
            },
            Event::Code(code) => {
                self.flush();
                let style = ChatStyle { code: true, ..self.style() };
                self.push(code.into_string(), style);
            }
            Event::InlineHtml(html) | Event::Html(html) => {
                self.flush();
                self.html(&html);
            }
            Event::SoftBreak => self.pending.push(' '),
            Event::HardBreak => self.pending.push('\n'),
            Event::Rule => {
                self.close();
                self.open(ChatBlockKind::Rule, false);
                self.close();
            }
            Event::TaskListMarker(done) => self.pending.push_str(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close();
                self.open(ChatBlockKind::Paragraph, false);
            }
            Tag::Heading { level, .. } => {
                self.close();
                self.open(ChatBlockKind::Heading { level: heading(level) }, false);
            }
            Tag::CodeBlock(_) => {
                self.close();
                self.open(ChatBlockKind::Code, false);
                if let Some(block) = &mut self.current {
                    block.spans.push(ChatSpan { text: String::new(), style: ChatStyle { code: true, ..ChatStyle::default() } });
                }
            }
            Tag::BlockQuote(_) => {
                self.close();
                self.quote += 1;
            }
            Tag::List(start) => {
                self.close();
                self.lists.push(start);
            }
            Tag::Item => {
                self.close();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}.", *number - 1)
                    }
                    _ => "•".into(),
                });
            }
            Tag::Table(_) => {
                self.close();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                self.flush();
                if let Some(table) = &mut self.table {
                    table.cell = Some(Vec::new());
                }
            }
            Tag::Emphasis => self.restyle(|builder| builder.italic += 1),
            Tag::Strong => self.restyle(|builder| builder.bold += 1),
            Tag::Strikethrough => self.restyle(|builder| builder.strike += 1),
            Tag::Link { link_type, dest_url, .. } => {
                let mut url = dest_url.into_string();
                if link_type == LinkType::Autolink
                    && let Some((target, label)) = url.split_once('|')
                {
                    self.label = Some(if label.is_empty() { target } else { label }.to_string());
                    url = target.to_string();
                }
                self.restyle(|builder| builder.link = Some(url));
            }
            Tag::Image { dest_url, .. } => {
                self.restyle(|builder| builder.link = Some(dest_url.into_string()));
                self.image_start = Some(self.spans().map_or(0, |spans| spans.len()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item => self.close(),
            TagEnd::CodeBlock => {
                if let Some(block) = &mut self.current
                    && let Some(span) = block.spans.first_mut()
                    && span.text.ends_with('\n')
                {
                    span.text.pop();
                }
                self.close();
            }
            TagEnd::BlockQuote(_) => {
                self.close();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.close();
                self.lists.pop();
            }
            TagEnd::TableCell => {
                self.flush();
                if let Some(table) = &mut self.table
                    && let Some(mut cell) = table.cell.take()
                {
                    trim_end(&mut cell);
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let mut block = self.block(ChatBlockKind::Table, false);
                    block.rows = table.rows;
                    self.blocks.push(block);
                }
            }
            TagEnd::Emphasis => self.restyle(|builder| builder.italic = builder.italic.saturating_sub(1)),
            TagEnd::Strong => self.restyle(|builder| builder.bold = builder.bold.saturating_sub(1)),
            TagEnd::Strikethrough => self.restyle(|builder| builder.strike = builder.strike.saturating_sub(1)),
            TagEnd::Link => self.restyle(|builder| builder.link = None),
            TagEnd::Image => {
                self.flush();
                let start = self.image_start.take().unwrap_or(0);
                if self.spans().is_none_or(|spans| spans.len() <= start)
                    && let Some(url) = self.link.clone()
                {
                    let style = self.style();
                    self.push(url, style);
                }
                self.link = None;
            }
            _ => {}
        }
    }

    /// Lays out the text so far before the style changes.
    fn restyle(&mut self, change: impl FnOnce(&mut Self)) {
        self.flush();
        change(self);
    }

    fn style(&self) -> ChatStyle {
        ChatStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: false,
            mention: false,
            link: self.link.clone(),
        }
    }

    fn block(&mut self, kind: ChatBlockKind, tight: bool) -> ChatBlock {
        let mut block = ChatBlock::new(kind);
        block.quote = self.quote;
        block.indent = self.lists.len() as u32;
        block.marker = self.marker.take();
        block.tight = tight;
        block
    }

    fn open(&mut self, kind: ChatBlockKind, tight: bool) {
        self.current = Some(self.block(kind, tight));
    }

    /// Ends the open block; one left without text takes no room and hands its marker on.
    fn close(&mut self) {
        self.flush();
        let Some(mut block) = self.current.take() else { return };
        if matches!(block.kind, ChatBlockKind::Paragraph | ChatBlockKind::Heading { .. }) {
            trim_end(&mut block.spans);
            if let Some(first) = block.spans.first_mut() {
                let trimmed = first.text.trim_start().len();
                first.text.drain(..first.text.len() - trimmed);
            }
            block.spans.retain(|span| !span.text.is_empty());
            if block.spans.is_empty() {
                self.marker = self.marker.take().or(block.marker);
                return;
            }
        }
        self.blocks.push(block);
    }

    /// Where text goes now: the open table cell, else the open block, else a paragraph opened for
    /// it (a tight list item's text comes without one).
    fn spans(&mut self) -> Option<&mut Vec<ChatSpan>> {
        if let Some(table) = &mut self.table {
            return table.cell.as_mut();
        }
        self.current.as_mut().map(|block| &mut block.spans)
    }

    fn push(&mut self, text: String, style: ChatStyle) {
        if text.is_empty() {
            return;
        }
        if self.table.is_none() && self.current.is_none() {
            let tight = !self.lists.is_empty();
            self.open(ChatBlockKind::Paragraph, tight);
        }
        let Some(spans) = self.spans() else { return };
        match spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(&text),
            _ => spans.push(ChatSpan { text, style }),
        }
    }

    /// The pending text as spans: Chat's older tokens, and bare links outside links and code.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending);
        let style = self.style();
        let mut at = 0;
        for token in TOKEN.captures_iter(&text) {
            let whole = token.get(0).unwrap();
            self.text(&text[at..whole.start()], &style);
            match (token.get(1), token.get(3)) {
                (Some(url), _) => {
                    let label = token.get(2).map(|label| label.as_str()).filter(|label| !label.is_empty());
                    let linked = ChatStyle { link: Some(url.as_str().into()), ..style.clone() };
                    self.push(label.unwrap_or(url.as_str()).into(), linked);
                }
                (None, Some(user)) => self.mention(user.as_str()),
                _ => {}
            }
            at = whole.end();
        }
        self.text(&text[at..], &style);
    }

    fn text(&mut self, text: &str, style: &ChatStyle) {
        if style.link.is_some() {
            self.push(text.to_string(), style.clone());
            return;
        }
        let mut at = 0;
        for link in LinkFinder::new().kinds(&[LinkKind::Url]).links(text) {
            self.push(text[at..link.start()].to_string(), style.clone());
            self.push(link.as_str().to_string(), ChatStyle { link: Some(link.as_str().to_string()), ..style.clone() });
            at = link.end();
        }
        self.push(text[at..].to_string(), style.clone());
    }

    /// Raw HTML: mentions become names; other tags stay as written.
    fn html(&mut self, html: &str) {
        let style = self.style();
        let mut at = 0;
        for tag in MENTION_TAG.captures_iter(html) {
            let whole = tag.get(0).unwrap();
            self.text(&html[at..whole.start()], &style);
            match (tag.get(1).map(|kind| kind.as_str()), tag.get(2)) {
                (Some("email"), Some(email)) => {
                    let name = (self.name)(email.as_str()).unwrap_or_else(|| email.as_str().to_string());
                    self.push(format!("@{name}"), ChatStyle { mention: true, ..ChatStyle::default() });
                }
                (Some(_), Some(user)) => self.mention(user.as_str()),
                _ => {}
            }
            at = whole.end();
        }
        self.text(&html[at..], &style);
    }

    fn mention(&mut self, user: &str) {
        let name = if user == "users/all" { Some("all".to_string()) } else { (self.name)(user) };
        self.push(format!("@{}", name.unwrap_or_else(|| "someone".into())), ChatStyle { mention: true, ..ChatStyle::default() });
    }
}

fn heading(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Without the spaces and line breaks that end a paragraph.
fn trim_end(spans: &mut Vec<ChatSpan>) {
    while let Some(last) = spans.last_mut() {
        let kept = last.text.trim_end().len();
        last.text.truncate(kept);
        if !last.text.is_empty() {
            break;
        }
        spans.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(id: &str) -> Option<String> {
        (id == "users/1").then(|| "Brent".into())
    }

    fn texts(block: &ChatBlock) -> Vec<(&str, ChatStyle)> {
        block.spans.iter().map(|span| (span.text.as_str(), span.style.clone())).collect()
    }

    #[test]
    fn styles_runs_and_resolves_mentions_and_links() {
        let blocks = parse(
            "**bold** and _it_ ~~gone~~ `x` [site](https://a.example/b) <chat-user data-user=\"users/1\"></chat-user> \
             <users/2> https://c.example/d.",
            &names,
        );
        assert_eq!(blocks.len(), 1);
        let bold = ChatStyle { bold: true, ..ChatStyle::default() };
        let link = |url: &str| ChatStyle { link: Some(url.into()), ..ChatStyle::default() };
        let mention = ChatStyle { mention: true, ..ChatStyle::default() };
        assert_eq!(
            texts(&blocks[0]),
            [
                ("bold", bold),
                (" and ", ChatStyle::default()),
                ("it", ChatStyle { italic: true, ..ChatStyle::default() }),
                (" ", ChatStyle::default()),
                ("gone", ChatStyle { strike: true, ..ChatStyle::default() }),
                (" ", ChatStyle::default()),
                ("x", ChatStyle { code: true, ..ChatStyle::default() }),
                (" ", ChatStyle::default()),
                ("site", link("https://a.example/b")),
                (" ", ChatStyle::default()),
                ("@Brent", mention.clone()),
                (" ", ChatStyle::default()),
                ("@someone", mention),
                (" ", ChatStyle::default()),
                ("https://c.example/d", link("https://c.example/d")),
                (".", ChatStyle::default()),
            ]
        );
    }

    #[test]
    fn escapes_and_wrapped_lines_read_as_text() {
        let blocks = parse("thanks\\! give me \\~5min \\*not bold\\*\nsnake_case_name and 2 * 3 * 4", &names);
        assert_eq!(texts(&blocks[0]), [("thanks! give me ~5min *not bold* snake_case_name and 2 * 3 * 4", ChatStyle::default())]);
        let blocks = parse("line one\\\nline two", &names);
        assert_eq!(blocks[0].spans[0].text, "line one\nline two");
    }

    #[test]
    fn lays_out_blocks() {
        let blocks = parse("# Title\n\n> quoted\n\n- a\n- b\n  1. c\n\n```\nlet a = 1;\n```\n\n---\n\n| x | y |\n|---|---|\n| 1 | 2 |", &names);
        let shape: Vec<(ChatBlockKind, u32, u32, Option<&str>, bool)> = blocks
            .iter()
            .map(|block| (block.kind, block.quote, block.indent, block.marker.as_deref(), block.tight))
            .collect();
        assert_eq!(
            shape,
            [
                (ChatBlockKind::Heading { level: 1 }, 0, 0, None, false),
                (ChatBlockKind::Paragraph, 1, 0, None, false),
                (ChatBlockKind::Paragraph, 0, 1, Some("•"), true),
                (ChatBlockKind::Paragraph, 0, 1, Some("•"), true),
                (ChatBlockKind::Paragraph, 0, 2, Some("1."), true),
                (ChatBlockKind::Code, 0, 0, None, false),
                (ChatBlockKind::Rule, 0, 0, None, false),
                (ChatBlockKind::Table, 0, 0, None, false),
            ]
        );
        assert_eq!(blocks[5].spans[0].text, "let a = 1;");
        let cells: Vec<Vec<&str>> =
            blocks[7].rows.iter().map(|row| row.iter().map(|cell| cell[0].text.as_str()).collect()).collect();
        assert_eq!(cells, [["x", "y"], ["1", "2"]]);
    }

    #[test]
    fn previews_are_one_line() {
        assert_eq!(plain("hi <chat-user data-user=\"users/1\"></chat-user>,\n\n**bold** _it_ ~~gone~~ snake_case_name"), "hi @, bold it gone snake_case_name");
        assert_eq!(plain("```\ncode\n```\n2 * 3"), "code 2 * 3");
        assert_eq!(plain("*sure*"), "sure");
        assert_eq!(plain("see [the doc](https://x.com/a) and https://y.com"), "see the doc and https://y.com");
        assert_eq!(plain("see <https://x.com/doc|the doc>, <https://y.com|y> and <users/2>"), "see the doc, y and @");
    }

    #[test]
    fn google_chat_input_matches_server_formatting() {
        let blocks = parse_input(
            "*bold* and _it_ ~gone~ `x` *_both_* https://c.example/a_b~c.\nsame para\n\n```\nlet a = 1;\n```\n\nnext para",
            &names,
        );
        assert_eq!(blocks.len(), 3);
        let bold = ChatStyle { bold: true, ..ChatStyle::default() };
        let italic = ChatStyle { italic: true, ..ChatStyle::default() };
        let strike = ChatStyle { strike: true, ..ChatStyle::default() };
        let code = ChatStyle { code: true, ..ChatStyle::default() };
        let both = ChatStyle { bold: true, italic: true, ..ChatStyle::default() };
        let link = |url: &str| ChatStyle { link: Some(url.into()), ..ChatStyle::default() };
        assert_eq!(
            texts(&blocks[0]),
            [
                ("bold", bold.clone()),
                (" and ", ChatStyle::default()),
                ("it", italic),
                (" ", ChatStyle::default()),
                ("gone", strike),
                (" ", ChatStyle::default()),
                ("x", code),
                (" ", ChatStyle::default()),
                ("both", both),
                (" ", ChatStyle::default()),
                ("https://c.example/a_b~c", link("https://c.example/a_b~c")),
                (".\nsame para", ChatStyle::default()),
            ]
        );
        assert_eq!(blocks[1].kind, ChatBlockKind::Code);
        assert_eq!(blocks[1].spans[0].text, "let a = 1;");
        assert_eq!(blocks[2].kind, ChatBlockKind::Paragraph);
        assert_eq!(texts(&blocks[2]), [("next para", ChatStyle::default())]);

        let literal = parse_input("snake_case_name and 2 * 3 * 4 and 1. item and **plain**", &names);
        assert_eq!(
            texts(&literal[0]),
            [("snake_case_name and 2 * 3 * 4 and 1. item and **plain**", ChatStyle::default())]
        );

        let cjk = parse_input("你好*粗体*世界 *https://a.example*", &names);
        assert_eq!(
            texts(&cjk[0]),
            [
                ("你好", ChatStyle::default()),
                ("粗体", bold),
                ("世界 ", ChatStyle::default()),
                ("https://a.example", ChatStyle { bold: true, link: Some("https://a.example".into()), ..ChatStyle::default() }),
            ]
        );
    }
}
