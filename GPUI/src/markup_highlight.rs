//! Shared WYSIWYM live syntax highlighter for `EditorState` (MS-316 Google Chat, MS-317 Mail Markdown).
//! Keeps all markup characters visible while rendering bold, italic, strikethrough, code, links,
//! headings, quotes and list markers in place.

use std::ops::Range;
use std::rc::Rc;

use gpui::component::input::{
    EditorState, HighlightStyleResolver, InputEdit, InputHighlighter, InputHighlighterFactory, Rope,
};
use gpui::{Context, FontStyle, FontWeight, HighlightStyle, SharedString, StrikethroughStyle, UnderlineStyle, Window, WindowAppearance, px};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::design::{self, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkupFlavor {
    Markdown,
    GoogleChat,
}

pub fn markup_highlighter_factory(flavor: MarkupFlavor) -> InputHighlighterFactory {
    Rc::new(move |_| Some(Box::new(MarkupHighlighter::new(flavor))))
}

/// Colors come from the window's palette when the text is parsed, so an editor that uses one
/// sets its highlighter again when the window's appearance changes (Chat's composers, the
/// compose and reply bodies).
pub struct MarkupHighlighter {
    flavor: MarkupFlavor,
    /// The text as of the last parse, kept to reuse its allocation.
    source: String,
    runs: Vec<(Range<usize>, HighlightStyle)>,
}

impl MarkupHighlighter {
    pub fn new(flavor: MarkupFlavor) -> Self {
        Self { flavor, source: String::new(), runs: Vec::new() }
    }

    fn parse(&mut self, text: &Rope, window: &Window) {
        let dark = design::palette(window.appearance()).dark;
        self.source.clear();
        self.source.extend(text.chunks());
        self.runs = match self.flavor {
            MarkupFlavor::Markdown => markdown_runs(&self.source, dark),
            MarkupFlavor::GoogleChat => google_chat_runs(&self.source, dark),
        };
    }
}

impl InputHighlighter for MarkupHighlighter {
    fn language(&self) -> SharedString {
        match self.flavor {
            MarkupFlavor::Markdown => "markdown".into(),
            MarkupFlavor::GoogleChat => "google-chat".into(),
        }
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.parse(text, window);
    }

    /// One parse for the whole change (typing with several carets), of the text after its last
    /// edit.
    fn update_batch(&mut self, edits: &[(InputEdit, Rope)], _folding: bool, window: &mut Window, _cx: &mut Context<EditorState>) {
        if let Some((_, text)) = edits.last() {
            self.parse(text, window);
        }
    }

    fn styles(&self, range: &Range<usize>, _resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        cover_range(range, &self.runs)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<gpui::component::input::FoldRange> {
        Vec::new()
    }
}

fn palette(dark: bool) -> &'static Palette {
    design::palette(if dark { WindowAppearance::Dark } else { WindowAppearance::Light })
}

fn marker_style(p: &Palette) -> HighlightStyle {
    HighlightStyle { color: Some(p.muted.into()), ..Default::default() }
}

fn bold_style() -> HighlightStyle {
    HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() }
}

fn italic_style() -> HighlightStyle {
    HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() }
}

fn strike_style(p: &Palette) -> HighlightStyle {
    HighlightStyle {
        color: Some(p.muted.into()),
        strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: None }),
        ..Default::default()
    }
}

fn code_style(p: &Palette) -> HighlightStyle {
    HighlightStyle { background_color: Some(design::tint(p.text, 0.08).into()), ..Default::default() }
}

fn link_style(p: &Palette) -> HighlightStyle {
    HighlightStyle {
        color: Some(p.accent.into()),
        underline: Some(UnderlineStyle { thickness: px(1.), color: Some(p.accent.into()), wavy: false }),
        ..Default::default()
    }
}

fn mention_style(p: &Palette) -> HighlightStyle {
    HighlightStyle { color: Some(p.accent.into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }
}

fn merge_style(base: &mut HighlightStyle, overlay: &HighlightStyle) {
    if overlay.color.is_some() {
        base.color = overlay.color;
    }
    if overlay.font_weight.is_some() {
        base.font_weight = overlay.font_weight;
    }
    if overlay.font_style.is_some() {
        base.font_style = overlay.font_style;
    }
    if overlay.background_color.is_some() {
        base.background_color = overlay.background_color;
    }
    if overlay.underline.is_some() {
        base.underline = overlay.underline;
    }
    if overlay.strikethrough.is_some() {
        base.strikethrough = overlay.strikethrough;
    }
}

/// Flattens potentially overlapping `(range, style)` intervals into ordered, non-overlapping runs
/// within `0..len`, merging styles where intervals overlap: a later interval's style goes over an
/// earlier one's. One sweep over the intervals sorted by start, holding only those that cover the
/// current segment (as many as markup nests), so a long body costs O(n log n), not O(n²).
fn flatten_spans(len: usize, spans: &[(Range<usize>, HighlightStyle)]) -> Vec<(Range<usize>, HighlightStyle)> {
    let clip = |i: usize| spans[i].0.start.min(len)..spans[i].0.end.min(len);
    let mut by_start: Vec<usize> = (0..spans.len()).filter(|&i| !clip(i).is_empty()).collect();
    if by_start.is_empty() {
        return Vec::new();
    }
    by_start.sort_by_key(|&i| spans[i].0.start);
    let mut boundaries: Vec<usize> = by_start.iter().flat_map(|&i| [clip(i).start, clip(i).end]).collect();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    // Indices of the intervals covering the segment, in input order.
    let mut active: Vec<usize> = Vec::new();
    let mut next = 0;
    for pair in boundaries.windows(2) {
        let seg = pair[0]..pair[1];
        // Every start and end is a boundary, so an interval covers a segment whole or not at all.
        active.retain(|&i| clip(i).end > seg.start);
        while let Some(&i) = by_start.get(next)
            && spans[i].0.start <= seg.start
        {
            let at = active.partition_point(|&j| j < i);
            active.insert(at, i);
            next += 1;
        }
        if active.is_empty() {
            continue;
        }
        let mut combined = HighlightStyle::default();
        for &i in &active {
            merge_style(&mut combined, &spans[i].1);
        }
        if let Some((prev_range, prev_style)) = out.last_mut()
            && prev_range.end == seg.start
            && *prev_style == combined
        {
            prev_range.end = seg.end;
        } else {
            out.push((seg, combined));
        }
    }
    out
}

/// Clips `runs` to `range`, filling any uncovered gaps with `HighlightStyle::default()`.
pub fn cover_range(range: &Range<usize>, runs: &[(Range<usize>, HighlightStyle)]) -> Vec<(Range<usize>, HighlightStyle)> {
    if range.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = range.start;
    let start_idx = runs.partition_point(|(r, _)| r.end <= range.start);
    for (r, style) in &runs[start_idx..] {
        if r.start >= range.end {
            break;
        }
        let s = r.start.max(cursor);
        let e = r.end.min(range.end);
        if s > cursor {
            out.push((cursor..s, HighlightStyle::default()));
        }
        if s < e {
            out.push((s..e, *style));
            cursor = e;
        }
    }
    if cursor < range.end {
        out.push((cursor..range.end, HighlightStyle::default()));
    }
    out
}

/// The web and email addresses in `text`, which starts at `at` in the body, as the sent mail
/// links them (`text::linkify` in the engine, which also takes `example.com` without a scheme).
fn push_bare_links(finder: &LinkFinder, text: &str, at: usize, p: &Palette, spans: &mut Vec<(Range<usize>, HighlightStyle)>) {
    if !text.contains(['.', ':']) {
        return;
    }
    for link in finder.links(text) {
        spans.push((at + link.start()..at + link.end(), link_style(p)));
    }
}

pub fn markdown_runs(text: &str, dark: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() {
        return Vec::new();
    }
    let p = palette(dark);
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut finder = LinkFinder::new();
    finder.url_must_have_scheme(false);

    // The options `markdown_to_html` sends with, so what shows as markup here is what the mail shows.
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options).into_offset_iter();
    // Bare addresses are links only in plain text, as `markdown_to_html` links them: not in code,
    // a link (its text or target) or an image's description.
    let mut in_code_block = false;
    let mut links = 0usize;
    let mut images = 0usize;

    for (event, range) in parser {
        match &event {
            Event::Start(Tag::Link { .. }) => links += 1,
            Event::End(TagEnd::Link) => links = links.saturating_sub(1),
            Event::Start(Tag::Image { .. }) => images += 1,
            Event::End(TagEnd::Image) => images = images.saturating_sub(1),
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            _ => {}
        }
        let s = range.start.min(text.len());
        let e = range.end.min(text.len());
        if s >= e {
            continue;
        }
        let slice = &text[s..e];
        match event {
            Event::Start(Tag::Strong) => {
                if slice.len() >= 4 && (slice.starts_with("**") && slice.ends_with("**") || slice.starts_with("__") && slice.ends_with("__")) {
                    spans.push((s..s + 2, marker_style(p)));
                    spans.push((s + 2..e - 2, bold_style()));
                    spans.push((e - 2..e, marker_style(p)));
                } else {
                    spans.push((s..e, bold_style()));
                }
            }
            Event::Start(Tag::Emphasis) => {
                if slice.len() >= 2 && (slice.starts_with('*') && slice.ends_with('*') || slice.starts_with('_') && slice.ends_with('_')) {
                    spans.push((s..s + 1, marker_style(p)));
                    spans.push((s + 1..e - 1, italic_style()));
                    spans.push((e - 1..e, marker_style(p)));
                } else {
                    spans.push((s..e, italic_style()));
                }
            }
            Event::Start(Tag::Strikethrough) => {
                let m = if slice.starts_with("~~") && slice.ends_with("~~") && slice.len() >= 4 {
                    2
                } else if slice.starts_with('~') && slice.ends_with('~') && slice.len() >= 2 {
                    1
                } else {
                    0
                };
                if m > 0 {
                    spans.push((s..s + m, marker_style(p)));
                    spans.push((s + m..e - m, strike_style(p)));
                    spans.push((e - m..e, marker_style(p)));
                } else {
                    spans.push((s..e, strike_style(p)));
                }
            }
            Event::Code(_) => {
                let ticks = slice.bytes().take_while(|&b| b == b'`').count();
                if ticks > 0 && slice.len() >= ticks * 2 && slice[slice.len() - ticks..].bytes().all(|b| b == b'`') {
                    spans.push((s..s + ticks, marker_style(p)));
                    spans.push((s + ticks..e - ticks, code_style(p)));
                    spans.push((e - ticks..e, marker_style(p)));
                } else {
                    spans.push((s..e, code_style(p)));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                spans.push((s..e, code_style(p)));
                if slice.starts_with("```") || slice.starts_with("~~~") {
                    if let Some(nl) = slice.find('\n') {
                        spans.push((s..s + nl, marker_style(p)));
                    }
                    let trimmed = slice.trim_end_matches('\n');
                    if let Some(last_nl) = trimmed.rfind('\n') {
                        let tail = trimmed[last_nl + 1..].trim();
                        if tail.starts_with("```") || tail.starts_with("~~~") {
                            spans.push((s + last_nl + 1..s + trimmed.len(), marker_style(p)));
                        }
                    }
                }
            }
            Event::Start(Tag::Heading { .. }) => {
                let hashes = slice.bytes().take_while(|&b| b == b'#').count();
                if hashes > 0 {
                    let marker_end = if slice.as_bytes().get(hashes) == Some(&b' ') { s + hashes + 1 } else { s + hashes };
                    spans.push((s..marker_end, marker_style(p)));
                    if marker_end < e {
                        spans.push((marker_end..e, bold_style()));
                    }
                } else {
                    spans.push((s..e, bold_style()));
                }
            }
            Event::Start(Tag::Link { link_type, .. }) => match link_type {
                LinkType::Inline => {
                    if let Some(mid) = slice.find("](")
                        && slice.starts_with('[')
                        && slice.ends_with(')')
                    {
                        spans.push((s..s + 1, marker_style(p)));
                        spans.push((s + 1..s + mid, link_style(p)));
                        spans.push((s + mid..e, marker_style(p)));
                    } else {
                        spans.push((s..e, link_style(p)));
                    }
                }
                LinkType::Autolink | LinkType::Email if slice.starts_with('<') && slice.ends_with('>') && slice.len() >= 2 => {
                    spans.push((s..s + 1, marker_style(p)));
                    spans.push((s + 1..e - 1, link_style(p)));
                    spans.push((e - 1..e, marker_style(p)));
                }
                _ => spans.push((s..e, link_style(p))),
            },
            Event::Start(Tag::BlockQuote(_)) => {
                let mut offset = s;
                for line in slice.split_inclusive('\n') {
                    let trimmed = line.trim_start();
                    let indent = line.len() - trimmed.len();
                    if trimmed.starts_with('>') {
                        let m_start = offset + indent;
                        let m_len = if trimmed.starts_with("> ") { 2 } else { 1 };
                        spans.push((m_start..m_start + m_len, marker_style(p)));
                        if m_start + m_len < offset + line.len() {
                            spans.push((m_start + m_len..offset + line.len(), marker_style(p)));
                        }
                    }
                    offset += line.len();
                }
            }
            Event::Start(Tag::Item) => {
                let trimmed = slice.trim_start();
                let indent = slice.len() - trimmed.len();
                let marker_len = if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
                    2
                } else {
                    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
                    if digits > 0 && (trimmed[digits..].starts_with(". ") || trimmed[digits..].starts_with(") ")) {
                        digits + 2
                    } else {
                        0
                    }
                };
                if marker_len > 0 {
                    spans.push((s + indent..s + indent + marker_len, HighlightStyle { color: Some(p.accent.into()), ..Default::default() }));
                }
            }
            Event::Rule => {
                spans.push((s..e, marker_style(p)));
            }
            // An escape or entity reads otherwise than its source: offsets wouldn't line up.
            Event::Text(t) if !in_code_block && links == 0 && images == 0 && t.as_ref() == slice => {
                push_bare_links(&finder, slice, s, p, &mut spans);
            }
            _ => {}
        }
    }

    flatten_spans(text.len(), &spans)
}

enum GchatToken {
    Link { url_end: usize, label: Option<(usize, usize)> },
    User,
}

fn parse_gchat_token(rest: &str) -> Option<(usize, GchatToken)> {
    if !rest.starts_with('<') {
        return None;
    }
    let close = rest[1..].find('>')? + 1;
    let inner = &rest[1..close];
    if inner.contains('\n') {
        return None;
    }
    if inner.starts_with("http://") || inner.starts_with("https://") {
        if let Some(pipe) = inner.find('|') {
            let url = &inner[..pipe];
            if url.chars().any(char::is_whitespace) {
                return None;
            }
            let label_start = 1 + pipe + 1;
            let label = (label_start < close).then_some((label_start, close));
            return Some((close + 1, GchatToken::Link { url_end: 1 + pipe, label }));
        }
        if inner.chars().any(char::is_whitespace) {
            return None;
        }
        return Some((close + 1, GchatToken::Link { url_end: close, label: None }));
    }
    if inner.starts_with("users/") && inner.len() > "users/".len() && !inner.chars().any(char::is_whitespace) {
        return Some((close + 1, GchatToken::User));
    }
    None
}

fn gchat_can_open(text: &str, at: usize, delim: char) -> bool {
    let prev = text[..at].chars().next_back();
    let next = text[at + delim.len_utf8()..].chars().next();
    prev.is_none_or(|c| !c.is_ascii_alphanumeric() && c != delim) && next.is_some_and(|c| !c.is_whitespace() && c != delim)
}

fn gchat_can_close(text: &str, at: usize, delim: char) -> bool {
    let prev = text[..at].chars().next_back();
    let next = text[at + delim.len_utf8()..].chars().next();
    prev.is_some_and(|c| !c.is_whitespace() && c != delim) && next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != delim)
}

fn gchat_closing_delim(text: &str, from: usize, delim: char) -> Option<usize> {
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
            && let Some((len, _)) = parse_gchat_token(rest)
        {
            i += len;
            continue;
        }
        if (rest.starts_with("http://") || rest.starts_with("https://"))
            && let Some(link) = LinkFinder::new().kinds(&[LinkKind::Url]).links(rest).next()
            && link.start() == 0
        {
            let end = i + link.end();
            if link.as_str().ends_with(delim) && gchat_can_close(text, end - 1, delim) {
                return Some(end - 1);
            }
            i = end;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if ch == delim && gchat_can_close(text, i, delim) {
            return Some(i);
        }
        i += ch.len_utf8();
    }
    None
}

fn scan_gchat_inline(text: &str, base: usize, in_link: bool, p: &Palette, spans: &mut Vec<(Range<usize>, HighlightStyle)>) {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('`') {
            let line_end = rest[1..].find('\n').map_or(rest.len(), |n| 1 + n);
            if let Some(rel_close) = rest[1..line_end].find('`')
                && rel_close > 0
            {
                let end = i + 1 + rel_close + 1;
                spans.push((base + i..base + i + 1, marker_style(p)));
                spans.push((base + i + 1..base + end - 1, code_style(p)));
                spans.push((base + end - 1..base + end, marker_style(p)));
                i = end;
                continue;
            }
        }
        if rest.starts_with('<')
            && let Some((len, token)) = parse_gchat_token(rest)
        {
            let end = i + len;
            match token {
                GchatToken::Link { url_end, label: Some((ls, le)) } => {
                    spans.push((base + i..base + i + url_end + 1, marker_style(p)));
                    spans.push((base + i + ls..base + i + le, link_style(p)));
                    spans.push((base + end - 1..base + end, marker_style(p)));
                }
                GchatToken::Link { .. } => {
                    spans.push((base + i..base + i + 1, marker_style(p)));
                    spans.push((base + i + 1..base + end - 1, link_style(p)));
                    spans.push((base + end - 1..base + end, marker_style(p)));
                }
                GchatToken::User => {
                    spans.push((base + i..base + i + 1, marker_style(p)));
                    spans.push((base + i + 1..base + end - 1, mention_style(p)));
                    spans.push((base + end - 1..base + end, marker_style(p)));
                }
            }
            i = end;
            continue;
        }
        if !in_link
            && (rest.starts_with("http://") || rest.starts_with("https://"))
            && let Some(link) = LinkFinder::new().kinds(&[LinkKind::Url]).links(rest).next()
            && link.start() == 0
        {
            let end = i + link.end();
            spans.push((base + i..base + end, link_style(p)));
            i = end;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if matches!(ch, '*' | '_' | '~')
            && gchat_can_open(text, i, ch)
            && let Some(close) = gchat_closing_delim(text, i + 1, ch)
        {
            let inner = match ch {
                '*' => bold_style(),
                '_' => italic_style(),
                '~' => strike_style(p),
                _ => unreachable!(),
            };
            spans.push((base + i..base + i + 1, marker_style(p)));
            spans.push((base + i + 1..base + close, inner));
            spans.push((base + close..base + close + 1, marker_style(p)));
            scan_gchat_inline(&text[i + 1..close], base + i + 1, in_link, p, spans);
            i = close + 1;
            continue;
        }
        i += ch.len_utf8();
    }
}

pub fn google_chat_runs(text: &str, dark: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() {
        return Vec::new();
    }
    let p = palette(dark);
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let Some(rel_open) = text[pos..].find("```") else {
            scan_gchat_inline(&text[pos..], pos, false, p, &mut spans);
            break;
        };
        let open = pos + rel_open;
        let Some(rel_close) = text[open + 3..].find("```") else {
            scan_gchat_inline(&text[pos..], pos, false, p, &mut spans);
            break;
        };
        let close = open + 3 + rel_close;
        if open > pos {
            scan_gchat_inline(&text[pos..open], pos, false, p, &mut spans);
        }
        spans.push((open..open + 3, marker_style(p)));
        if close > open + 3 {
            spans.push((open + 3..close, code_style(p)));
        }
        spans.push((close..close + 3, marker_style(p)));
        pos = close + 3;
    }

    flatten_spans(text.len(), &spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_range_fills_gaps_and_clips() {
        let p = palette(false);
        let runs = vec![(2..5, bold_style()), (7..10, italic_style())];
        let covered = cover_range(&(0..12), &runs);
        assert_eq!(covered.len(), 5);
        assert_eq!(covered[0], (0..2, HighlightStyle::default()));
        assert_eq!(covered[1], (2..5, bold_style()));
        assert_eq!(covered[2], (5..7, HighlightStyle::default()));
        assert_eq!(covered[3], (7..10, italic_style()));
        assert_eq!(covered[4], (10..12, HighlightStyle::default()));

        let partial = cover_range(&(3..8), &runs);
        assert_eq!(partial, vec![(3..5, bold_style()), (5..7, HighlightStyle::default()), (7..8, italic_style())]);
        let _ = p;
    }

    #[test]
    fn markdown_runs_handles_nested_and_cjk() {
        let text = "**加粗** 和 *斜体* 以及 [链接](https://example.com) 和 `code`";
        let runs = markdown_runs(text, false);
        let covered = cover_range(&(0..text.len()), &runs);
        // Must fully cover 0..text.len() with contiguous non-overlapping ranges.
        let mut cursor = 0;
        for (r, _) in &covered {
            assert_eq!(r.start, cursor);
            assert!(r.start < r.end);
            cursor = r.end;
        }
        assert_eq!(cursor, text.len());
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "加粗" && s.font_weight == Some(FontWeight::BOLD)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "斜体" && s.font_style == Some(FontStyle::Italic)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "链接" && s.underline.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "code" && s.background_color.is_some()));
    }

    /// The byte ranges `runs` underline, merged where they touch.
    fn underlined<'a>(text: &'a str, runs: &[(Range<usize>, HighlightStyle)]) -> Vec<&'a str> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (r, _) in runs.iter().filter(|(_, s)| s.underline.is_some()) {
            match out.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                _ => out.push(r.clone()),
            }
        }
        out.into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn markdown_runs_link_addresses_only_in_plain_text() {
        let text = "see https://a.example.com/x_y_z, example.org or ops@example.org\n\n\
                    `https://code.example.com` [click](https://target.example.com) ![pic](https://img.example.com/p.png \"https://title.example.com\") <https://auto.example.com>\n\n\
                    ```\nhttps://block.example.com\n```\n\n    https://indented.example.com\n";
        let runs = markdown_runs(text, false);
        assert_eq!(
            underlined(text, &runs),
            ["https://a.example.com/x_y_z", "example.org", "ops@example.org", "click", "https://auto.example.com"]
        );
        // The link's target is a muted marker, not underlined.
        let target = text.find("(https://target").unwrap();
        assert!(runs.iter().any(|(r, s)| r.start <= target && r.end > target + 10 && s.color.is_some() && s.underline.is_none()));
    }

    #[test]
    fn flatten_spans_layers_later_spans_over_earlier_ones() {
        let p = palette(false);
        let spans = vec![(0..10, bold_style()), (2..6, marker_style(p)), (4..8, italic_style()), (12..14, code_style(p)), (13..20, link_style(p))];
        let runs = flatten_spans(16, &spans);
        let mut bold_marker = bold_style();
        merge_style(&mut bold_marker, &marker_style(p));
        let mut bold_marker_italic = bold_marker;
        merge_style(&mut bold_marker_italic, &italic_style());
        let mut bold_italic = bold_style();
        merge_style(&mut bold_italic, &italic_style());
        let mut code_link = code_style(p);
        merge_style(&mut code_link, &link_style(p));
        assert_eq!(
            runs,
            vec![
                (0..2, bold_style()),
                (2..4, bold_marker),
                (4..6, bold_marker_italic),
                (6..8, bold_italic),
                (8..10, bold_style()),
                (12..13, code_style(p)),
                (13..14, code_link),
                (14..16, link_style(p)),
            ]
        );
    }

    #[test]
    fn google_chat_runs_highlights_markers_nested_and_ignores_literal_delims() {
        let text = "*粗体* _斜体_ ~删除线~ `code` 2 * 3 * 4 snake_case_name *bold _both_*";
        let runs = google_chat_runs(text, false);
        let covered = cover_range(&(0..text.len()), &runs);
        let mut cursor = 0;
        for (r, _) in &covered {
            assert_eq!(r.start, cursor);
            assert!(r.start < r.end);
            cursor = r.end;
        }
        assert_eq!(cursor, text.len());
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "粗体" && s.font_weight == Some(FontWeight::BOLD)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "斜体" && s.font_style == Some(FontStyle::Italic)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "删除线" && s.strikethrough.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "code" && s.background_color.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "both" && s.font_weight == Some(FontWeight::BOLD) && s.font_style == Some(FontStyle::Italic)));
        assert!(!runs.iter().any(|(r, s)| text[r.clone()].contains('3') && s.font_weight.is_some()));
        assert!(!runs.iter().any(|(r, s)| text[r.clone()].contains("case") && s.font_style.is_some()));
    }

    #[test]
    fn google_chat_runs_handles_code_blocks_and_tokens() {
        let text = "```\n*not bold* https://a.com\n```\n<https://example.com|docs> <users/all>";
        let runs = google_chat_runs(text, true);
        assert!(!runs.iter().any(|(r, s)| &text[r.clone()] == "not bold" && s.font_weight.is_some()));
        assert!(!runs.iter().any(|(r, s)| &text[r.clone()] == "https://a.com" && s.underline.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "docs" && s.underline.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "users/all" && s.font_weight == Some(FontWeight::SEMIBOLD)));
    }
}
