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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// A mention of a person; the text is `@Name`.
    pub mention: bool,
    /// Where the text links to.
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 1 to 6.
    Heading(u8),
    /// A fenced or indented block: one span, its text as written.
    Code,
    Rule,
    /// Its [`Block::rows`], the header row first.
    Table,
}

/// One block of a message, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    /// A table's rows of cells.
    pub rows: Vec<Vec<Vec<Span>>>,
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

impl Block {
    fn new(kind: BlockKind) -> Self {
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
pub fn parse(markup: &str, name: &dyn Fn(&str) -> Option<String>) -> Vec<Block> {
    let mut builder = Builder::new(name);
    for event in Parser::new_ext(markup, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        builder.event(event);
    }
    builder.close();
    builder.blocks
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
    blocks: Vec<Block>,
    /// The open paragraph, heading or code block.
    current: Option<Block>,
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
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
    cell: Option<Vec<Span>>,
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
                Some(block) if block.kind == BlockKind::Code => block.spans[0].text.push_str(&text),
                _ => match self.label.take() {
                    Some(label) => self.pending.push_str(&label),
                    None => self.pending.push_str(&text),
                },
            },
            Event::Code(code) => {
                self.flush();
                let style = Style { code: true, ..self.style() };
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
                self.open(BlockKind::Rule, false);
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
                self.open(BlockKind::Paragraph, false);
            }
            Tag::Heading { level, .. } => {
                self.close();
                self.open(BlockKind::Heading(heading(level)), false);
            }
            Tag::CodeBlock(_) => {
                self.close();
                self.open(BlockKind::Code, false);
                if let Some(block) = &mut self.current {
                    block.spans.push(Span { text: String::new(), style: Style { code: true, ..Style::default() } });
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
                    let mut block = self.block(BlockKind::Table, false);
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

    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: false,
            mention: false,
            link: self.link.clone(),
        }
    }

    fn block(&mut self, kind: BlockKind, tight: bool) -> Block {
        let mut block = Block::new(kind);
        block.quote = self.quote;
        block.indent = self.lists.len() as u32;
        block.marker = self.marker.take();
        block.tight = tight;
        block
    }

    fn open(&mut self, kind: BlockKind, tight: bool) {
        self.current = Some(self.block(kind, tight));
    }

    /// Ends the open block; one left without text takes no room and hands its marker on.
    fn close(&mut self) {
        self.flush();
        let Some(mut block) = self.current.take() else { return };
        if matches!(block.kind, BlockKind::Paragraph | BlockKind::Heading(_)) {
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
    fn spans(&mut self) -> Option<&mut Vec<Span>> {
        if let Some(table) = &mut self.table {
            return table.cell.as_mut();
        }
        self.current.as_mut().map(|block| &mut block.spans)
    }

    fn push(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        if self.table.is_none() && self.current.is_none() {
            let tight = !self.lists.is_empty();
            self.open(BlockKind::Paragraph, tight);
        }
        let Some(spans) = self.spans() else { return };
        match spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(&text),
            _ => spans.push(Span { text, style }),
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
                    let linked = Style { link: Some(url.as_str().into()), ..style.clone() };
                    self.push(label.unwrap_or(url.as_str()).into(), linked);
                }
                (None, Some(user)) => self.mention(user.as_str()),
                _ => {}
            }
            at = whole.end();
        }
        self.text(&text[at..], &style);
    }

    fn text(&mut self, text: &str, style: &Style) {
        if style.link.is_some() {
            self.push(text.to_string(), style.clone());
            return;
        }
        let mut at = 0;
        for link in LinkFinder::new().kinds(&[LinkKind::Url]).links(text) {
            self.push(text[at..link.start()].to_string(), style.clone());
            self.push(link.as_str().to_string(), Style { link: Some(link.as_str().to_string()), ..style.clone() });
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
                    self.push(format!("@{name}"), Style { mention: true, ..Style::default() });
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
        self.push(format!("@{}", name.unwrap_or_else(|| "someone".into())), Style { mention: true, ..Style::default() });
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
fn trim_end(spans: &mut Vec<Span>) {
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

    fn texts(block: &Block) -> Vec<(&str, Style)> {
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
        let bold = Style { bold: true, ..Style::default() };
        let link = |url: &str| Style { link: Some(url.into()), ..Style::default() };
        let mention = Style { mention: true, ..Style::default() };
        assert_eq!(
            texts(&blocks[0]),
            [
                ("bold", bold),
                (" and ", Style::default()),
                ("it", Style { italic: true, ..Style::default() }),
                (" ", Style::default()),
                ("gone", Style { strike: true, ..Style::default() }),
                (" ", Style::default()),
                ("x", Style { code: true, ..Style::default() }),
                (" ", Style::default()),
                ("site", link("https://a.example/b")),
                (" ", Style::default()),
                ("@Brent", mention.clone()),
                (" ", Style::default()),
                ("@someone", mention),
                (" ", Style::default()),
                ("https://c.example/d", link("https://c.example/d")),
                (".", Style::default()),
            ]
        );
    }

    #[test]
    fn escapes_and_wrapped_lines_read_as_text() {
        let blocks = parse("thanks\\! give me \\~5min \\*not bold\\*\nsnake_case_name and 2 * 3 * 4", &names);
        assert_eq!(texts(&blocks[0]), [("thanks! give me ~5min *not bold* snake_case_name and 2 * 3 * 4", Style::default())]);
        let blocks = parse("line one\\\nline two", &names);
        assert_eq!(blocks[0].spans[0].text, "line one\nline two");
    }

    #[test]
    fn lays_out_blocks() {
        let blocks = parse("# Title\n\n> quoted\n\n- a\n- b\n  1. c\n\n```\nlet a = 1;\n```\n\n---\n\n| x | y |\n|---|---|\n| 1 | 2 |", &names);
        let shape: Vec<(BlockKind, u32, u32, Option<&str>, bool)> = blocks
            .iter()
            .map(|block| (block.kind, block.quote, block.indent, block.marker.as_deref(), block.tight))
            .collect();
        assert_eq!(
            shape,
            [
                (BlockKind::Heading(1), 0, 0, None, false),
                (BlockKind::Paragraph, 1, 0, None, false),
                (BlockKind::Paragraph, 0, 1, Some("•"), true),
                (BlockKind::Paragraph, 0, 1, Some("•"), true),
                (BlockKind::Paragraph, 0, 2, Some("1."), true),
                (BlockKind::Code, 0, 0, None, false),
                (BlockKind::Rule, 0, 0, None, false),
                (BlockKind::Table, 0, 0, None, false),
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
}
