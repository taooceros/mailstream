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
    #[allow(dead_code)]
    GoogleChat,
}

pub fn markup_highlighter_factory(flavor: MarkupFlavor) -> InputHighlighterFactory {
    Rc::new(move |_| Some(Box::new(MarkupHighlighter::new(flavor))))
}

pub struct MarkupHighlighter {
    flavor: MarkupFlavor,
    dark: bool,
    runs: Vec<(Range<usize>, HighlightStyle)>,
}

impl MarkupHighlighter {
    pub fn new(flavor: MarkupFlavor) -> Self {
        Self { flavor, dark: false, runs: Vec::new() }
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
        self.dark = design::palette(window.appearance()).dark;
        let source = text.to_string();
        self.runs = match self.flavor {
            MarkupFlavor::Markdown => markdown_runs(&source, self.dark),
            MarkupFlavor::GoogleChat => google_chat_runs(&source, self.dark),
        };
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
/// within `0..len`, merging styles where intervals overlap.
fn flatten_spans(len: usize, spans: &[(Range<usize>, HighlightStyle)]) -> Vec<(Range<usize>, HighlightStyle)> {
    if len == 0 || spans.is_empty() {
        return Vec::new();
    }
    let mut boundaries: Vec<usize> = Vec::with_capacity(spans.len() * 2);
    for (r, _) in spans {
        let s = r.start.min(len);
        let e = r.end.min(len);
        if s < e {
            boundaries.push(s);
            boundaries.push(e);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    if boundaries.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in boundaries.windows(2) {
        let seg = pair[0]..pair[1];
        let mut combined = HighlightStyle::default();
        let mut any = false;
        for (r, style) in spans {
            if r.start <= seg.start && r.end >= seg.end {
                merge_style(&mut combined, style);
                any = true;
            }
        }
        if any {
            if let Some((prev_range, prev_style)) = out.last_mut()
                && prev_range.end == seg.start
                && *prev_style == combined
            {
                prev_range.end = seg.end;
            } else {
                out.push((seg, combined));
            }
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

fn push_bare_links(text: &str, p: &Palette, spans: &mut Vec<(Range<usize>, HighlightStyle)>) {
    if !text.contains(['.', ':']) {
        return;
    }
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url, LinkKind::Email]);
    for link in finder.links(text) {
        spans.push((link.start()..link.end(), link_style(p)));
    }
}

pub fn markdown_runs(text: &str, dark: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() {
        return Vec::new();
    }
    let p = palette(dark);
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    push_bare_links(text, p, &mut spans);

    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS).into_offset_iter();
    let mut code_stack: Vec<usize> = Vec::new();

    for (event, range) in parser {
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
                code_stack.push(s);
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
            Event::End(TagEnd::CodeBlock) => {
                code_stack.pop();
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
            _ => {}
        }
    }

    flatten_spans(text.len(), &spans)
}

pub fn google_chat_runs(text: &str, dark: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() {
        return Vec::new();
    }
    let p = palette(dark);
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    push_bare_links(text, p, &mut spans);

    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Fenced code block ```...```
        if text[i..].starts_with("```") {
            if let Some(rel_end) = text[i + 3..].find("```") {
                let end = i + 3 + rel_end + 3;
                spans.push((i..i + 3, marker_style(p)));
                spans.push((i + 3..end - 3, code_style(p)));
                spans.push((end - 3..end, marker_style(p)));
                i = end;
                continue;
            }
        }
        // Inline code `...`
        if bytes[i] == b'`' {
            if let Some(rel_end) = text[i + 1..].find('`')
                && !text[i + 1..i + 1 + rel_end].contains('\n')
            {
                let end = i + 1 + rel_end + 1;
                spans.push((i..i + 1, marker_style(p)));
                spans.push((i + 1..end - 1, code_style(p)));
                spans.push((end - 1..end, marker_style(p)));
                i = end;
                continue;
            }
        }
        // *bold*, _italic_, ~strike~
        let marker = bytes[i];
        if matches!(marker, b'*' | b'_' | b'~') {
            let prev_ok = i == 0 || bytes[i - 1].is_ascii_whitespace() || matches!(bytes[i - 1], b'(' | b'[' | b'*' | b'_' | b'~');
            if prev_ok
                && let Some(rel_end) = text[i + 1..].find(marker as char)
                && rel_end > 0
                && !text[i + 1..i + 1 + rel_end].contains('\n')
            {
                let end = i + 1 + rel_end + 1;
                spans.push((i..i + 1, marker_style(p)));
                let inner = match marker {
                    b'*' => bold_style(),
                    b'_' => italic_style(),
                    _ => strike_style(p),
                };
                spans.push((i + 1..end - 1, inner));
                spans.push((end - 1..end, marker_style(p)));
            }
        }
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
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

    #[test]
    fn google_chat_runs_highlights_markers_and_styles() {
        let text = "*bold* _italic_ ~strike~ `code`";
        let runs = google_chat_runs(text, false);
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "bold" && s.font_weight == Some(FontWeight::BOLD)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "italic" && s.font_style == Some(FontStyle::Italic)));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "strike" && s.strikethrough.is_some()));
        assert!(runs.iter().any(|(r, s)| &text[r.clone()] == "code" && s.background_color.is_some()));
    }
}
