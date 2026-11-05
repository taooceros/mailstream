//! A multi-line plain-text editor that soft-wraps to its width and grows with its text: IME,
//! selection by keyboard and mouse, word and line moves, the clipboard. Built like `input.rs`
//! (gpui's `examples/input.rs`, Apache-2.0), over `shape_text`'s wrapped lines.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollHandle, SharedString,
    Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point,
    prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::design::{self, editor};

actions!(
    text_area,
    [Backspace, Delete, DeleteWordLeft, DeleteToLineStart, Newline, Left, Right, Up, Down, SelectLeft, SelectRight,
     SelectUp, SelectDown, WordLeft, WordRight, SelectWordLeft, SelectWordRight, LineStart, LineEnd, SelectLineStart,
     SelectLineEnd, DocumentStart, DocumentEnd, SelectDocumentStart, SelectDocumentEnd, SelectAll, Copy, Cut, Paste,
     Submit, ShowCharacterPalette]
);

const CONTEXT: &str = "TextArea";

pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("alt-backspace", DeleteWordLeft, context),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
        KeyBinding::new("enter", Newline, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("alt-left", WordLeft, context),
        KeyBinding::new("alt-right", WordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-left", LineStart, context),
        KeyBinding::new("cmd-right", LineEnd, context),
        KeyBinding::new("ctrl-a", LineStart, context),
        KeyBinding::new("ctrl-e", LineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("cmd-up", DocumentStart, context),
        KeyBinding::new("cmd-down", DocumentEnd, context),
        KeyBinding::new("home", DocumentStart, context),
        KeyBinding::new("end", DocumentEnd, context),
        KeyBinding::new("cmd-shift-up", SelectDocumentStart, context),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, context),
        KeyBinding::new("shift-home", SelectDocumentStart, context),
        KeyBinding::new("shift-end", SelectDocumentEnd, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-enter", Submit, context),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
    ]);
}

pub enum TextAreaEvent {
    /// The text changed.
    Changed,
    /// ⌘↩ was pressed.
    Submit,
}

/// One visual row: a logical line, or the part of it between two soft wraps.
struct Row {
    /// The logical line it belongs to.
    line: usize,
    /// Its text, as byte offsets into the whole content.
    start: usize,
    end: usize,
    /// Where the row starts in its unwrapped line.
    x: Pixels,
    /// Whether the line ends with this row (a line break or the end of the text follows it).
    last: bool,
}

/// The content as last shaped: logical lines, where each starts, and their wrapped rows.
struct Layout {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    rows: Vec<Row>,
    line_height: Pixels,
}

impl Layout {
    fn new(lines: Vec<WrappedLine>, line_height: Pixels) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut rows = Vec::with_capacity(lines.len());
        let mut base = 0;
        for (ix, line) in lines.iter().enumerate() {
            starts.push(base);
            let (mut start, mut x) = (0, Pixels::ZERO);
            for boundary in line.wrap_boundaries() {
                let glyph = &line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix];
                rows.push(Row { line: ix, start: base + start, end: base + glyph.index, x, last: false });
                start = glyph.index;
                x = glyph.position.x;
            }
            rows.push(Row { line: ix, start: base + start, end: base + line.len(), x, last: true });
            base += line.len() + 1;
        }
        Layout { lines, starts, rows, line_height }
    }

    /// The row the caret at `offset` shows on; at a soft wrap, the row after it.
    fn row_for(&self, offset: usize) -> usize {
        self.rows
            .partition_point(|row| row.end < offset || (row.end == offset && !row.last))
            .min(self.rows.len() - 1)
    }

    /// The x of `offset` within its row, which must contain it.
    fn x_in_row(&self, row: &Row, offset: usize) -> Pixels {
        let local = offset.saturating_sub(self.starts[row.line]);
        self.lines[row.line].unwrapped_layout.x_for_index(local) - row.x
    }

    fn point_for(&self, offset: usize) -> Point<Pixels> {
        let ix = self.row_for(offset);
        point(self.x_in_row(&self.rows[ix], offset), self.line_height * ix as f32)
    }

    /// The offset nearest `x` on row `ix`; past a soft-wrapped row's end it stays on that row.
    fn offset_in_row(&self, ix: usize, x: Pixels, content: &str) -> usize {
        let row = &self.rows[ix];
        let base = self.starts[row.line];
        let offset = (base + self.lines[row.line].unwrapped_layout.closest_index_for_x(x.max(Pixels::ZERO) + row.x))
            .clamp(row.start, row.end);
        if offset == row.end && !row.last && row.end > row.start {
            previous_boundary(content, row.end)
        } else {
            offset
        }
    }

    /// The offset for a point relative to the text's origin.
    fn offset_for_point(&self, position: Point<Pixels>, content: &str) -> usize {
        if position.y < Pixels::ZERO {
            return 0;
        }
        let ix = (position.y / self.line_height) as usize;
        if ix >= self.rows.len() {
            return content.len();
        }
        self.offset_in_row(ix, position.x, content)
    }
}

fn previous_boundary(content: &str, offset: usize) -> usize {
    content.grapheme_indices(true).rev().find_map(|(ix, _)| (ix < offset).then_some(ix)).unwrap_or(0)
}

fn next_boundary(content: &str, offset: usize) -> usize {
    content.grapheme_indices(true).find_map(|(ix, _)| (ix > offset).then_some(ix)).unwrap_or(content.len())
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

pub struct TextArea {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    min_rows: usize,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The x that up/down keep to across rows of different lengths.
    goal_x: Option<Pixels>,
    layout: Option<Layout>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// The scroll container to keep the caret visible in, and whether the caret moved since.
    scroll: Option<ScrollHandle>,
    autoscroll: bool,
}

impl EventEmitter<TextAreaEvent> for TextArea {}

impl TextArea {
    pub fn new(placeholder: impl Into<SharedString>, min_rows: usize, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            min_rows: min_rows.max(1),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            goal_x: None,
            layout: None,
            last_bounds: None,
            is_selecting: false,
            scroll: None,
            autoscroll: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the text and puts the caret at its start (above a quote).
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    /// Keeps the caret visible inside the scroll container `handle` belongs to.
    pub fn scroll_with(&mut self, handle: ScrollHandle) {
        self.scroll = Some(handle);
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    /// `offset` within the text and on a character boundary (a layout may lag an edit by a frame).
    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.goal_x = None;
        self.autoscroll = true;
        cx.notify()
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.goal_x = None;
        self.autoscroll = true;
        cx.notify()
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content[..offset]
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, segment)| is_word(segment).then_some(ix))
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content[offset..]
            .split_word_bound_indices()
            .find_map(|(ix, segment)| is_word(segment).then_some(offset + ix + segment.len()))
            .unwrap_or(self.content.len())
    }

    /// The start of the visual row the caret is on.
    fn row_start(&self, offset: usize) -> usize {
        match &self.layout {
            Some(layout) => layout.rows[layout.row_for(offset)].start,
            None => self.content[..offset].rfind('\n').map_or(0, |ix| ix + 1),
        }
    }

    /// The end of the visual row the caret is on, before a soft wrap's trailing space.
    fn row_end(&self, offset: usize) -> usize {
        match &self.layout {
            Some(layout) => {
                let row = &layout.rows[layout.row_for(offset)];
                if row.last || row.end == row.start { row.end } else { previous_boundary(&self.content, row.end) }
            }
            None => self.content[offset..].find('\n').map_or(self.content.len(), |ix| offset + ix),
        }
    }

    /// Where the caret lands one row up (`-1`) or down (`1`), and the x it aims for.
    fn vertical(&self, offset: usize, delta: isize) -> (usize, Option<Pixels>) {
        let Some(layout) = &self.layout else {
            return (if delta < 0 { 0 } else { self.content.len() }, None);
        };
        let ix = layout.row_for(offset);
        let x = self.goal_x.unwrap_or_else(|| layout.x_in_row(&layout.rows[ix], offset));
        let target = ix as isize + delta;
        if target < 0 {
            return (0, Some(x));
        }
        if target as usize >= layout.rows.len() {
            return (self.content.len(), Some(x));
        }
        (layout.offset_in_row(target as usize, x, &self.content), Some(x))
    }

    fn move_vertically(&mut self, delta: isize, select: bool, cx: &mut Context<Self>) {
        let (offset, goal) = if !select && !self.selected_range.is_empty() {
            let edge = if delta < 0 { self.selected_range.start } else { self.selected_range.end };
            self.vertical(edge, delta)
        } else {
            self.vertical(self.cursor_offset(), delta)
        };
        if select { self.select_to(offset, cx) } else { self.move_to(offset, cx) }
        self.goal_x = goal;
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(previous_boundary(&self.content, self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(next_boundary(&self.content, self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, true, cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), cx);
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_start(self.cursor_offset()), cx);
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_end(self.cursor_offset()), cx);
    }

    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_start(self.cursor_offset()), cx);
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_end(self.cursor_offset()), cx);
    }

    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_document_start(&mut self, _: &SelectDocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_document_end(&mut self, _: &SelectDocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection_reversed = false;
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(next_boundary(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let offset = self.cursor_offset();
            let start = self.row_start(offset);
            // At a row's start, ⌘⌫ joins it to the line above, as in AppKit.
            self.select_to(if start == offset { previous_boundary(&self.content, offset) } else { start }, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx)
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::Submit);
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\r\n", "\n").replace('\r', "\n"), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn offset_for_mouse(&self, position: Point<Pixels>) -> usize {
        match (&self.layout, self.last_bounds) {
            (Some(layout), Some(bounds)) => layout.offset_for_point(position - bounds.origin, &self.content),
            _ => self.content.len(),
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let offset = self.clamp(self.offset_for_mouse(event.position));
        match event.click_count {
            2 => {
                let word = self
                    .content
                    .split_word_bound_indices()
                    .map(|(ix, segment)| ix..ix + segment.len())
                    .find(|range| range.contains(&offset) || range.end == self.content.len() && range.end == offset);
                if let Some(word) = word {
                    self.selected_range = word;
                    self.selection_reversed = false;
                    cx.notify();
                }
            }
            count if count >= 3 => {
                let start = self.content[..offset].rfind('\n').map_or(0, |ix| ix + 1);
                let end = self.content[offset..].find('\n').map_or(self.content.len(), |ix| offset + ix + 1);
                self.selected_range = start..end;
                self.selection_reversed = false;
                cx.notify();
            }
            _ => {
                self.is_selecting = true;
                if event.modifiers.shift { self.select_to(offset, cx) } else { self.move_to(offset, cx) }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.offset_for_mouse(event.position), cx);
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// The range an edit replaces: the IME's, else the marked text, else the selection.
    fn edit_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone())
    }

    fn splice(&mut self, range: &Range<usize>, new_text: &str) {
        let mut content = String::with_capacity(self.content.len() - range.len() + new_text.len());
        content.push_str(&self.content[..range.start]);
        content.push_str(new_text);
        content.push_str(&self.content[range.end..]);
        self.content = content.into();
        self.goal_x = None;
        self.autoscroll = true;
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.splice(&range, new_text);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.splice(&range, new_text);
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.point_for(range.start);
        let end = layout.point_for(range.end);
        let end_x = if end.y == start.y { end.x } else { start.x };
        Some(Bounds::from_corners(
            bounds.origin + start,
            bounds.origin + point(end_x, start.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.layout.as_ref()?;
        Some(self.offset_to_utf16(layout.offset_for_point(point - bounds.origin, &self.content)))
    }
}

/// The text's runs: one color, the IME's marked text underlined.
fn text_runs(text: &SharedString, color: Hsla, marked: Option<&Range<usize>>, window: &Window) -> Vec<TextRun> {
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Some(marked) = marked.filter(|marked| marked.end <= text.len()) else {
        return vec![run];
    };
    [
        TextRun { len: marked.start, ..run.clone() },
        TextRun {
            len: marked.end - marked.start,
            underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false }),
            ..run.clone()
        },
        TextRun { len: text.len() - marked.end, ..run },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

struct TextAreaElement {
    area: Entity<TextArea>,
}

struct PrepaintState {
    layout: Option<Layout>,
    /// The placeholder's lines, when the text is empty.
    placeholder: Vec<WrappedLine>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let palette = design::palette(window.appearance());
        let area = self.area.read(cx);
        let (text, color) = if area.content.is_empty() {
            (area.placeholder.clone(), palette.faint)
        } else {
            (area.content.clone(), window.text_style().color)
        };
        let runs = text_runs(&text, color, area.marked_range.as_ref(), window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let min_rows = area.min_rows;

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows: usize = window
                .text_system()
                .shape_text(text.clone(), font_size, &runs, width, None)
                .map(|lines| lines.iter().map(|line| line.wrap_boundaries().len() + 1).sum())
                .unwrap_or(1);
            size(width.unwrap_or_default(), line_height * rows.max(min_rows) as f32)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let palette = design::palette(window.appearance());
        let area = self.area.read(cx);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap = Some(bounds.size.width);
        let shape = |text: &SharedString, runs: &[TextRun], window: &mut Window| {
            window
                .text_system()
                .shape_text(text.clone(), font_size, runs, wrap, None)
                .map(|lines| lines.into_vec())
                .unwrap_or_default()
        };

        let runs = text_runs(&area.content, window.text_style().color, area.marked_range.as_ref(), window);
        let lines = shape(&area.content, &runs, window);
        let placeholder = if area.content.is_empty() {
            let runs = text_runs(&area.placeholder, palette.faint, None, window);
            shape(&area.placeholder, &runs, window)
        } else {
            Vec::new()
        };
        if lines.is_empty() {
            return PrepaintState { layout: None, placeholder, selections: Vec::new(), cursor: None };
        }
        let layout = Layout::new(lines, line_height);

        let range = area.selected_range.clone();
        let mut selections = Vec::new();
        if !range.is_empty() {
            let mut color = palette.accent;
            color.a = editor::SELECTION_ALPHA;
            for (ix, row) in layout.rows.iter().enumerate() {
                let covers_break = row.last && range.start <= row.end && range.end > row.end;
                let (start, end) = (range.start.max(row.start), range.end.min(row.end));
                if start >= end && !covers_break {
                    continue;
                }
                let left = layout.x_in_row(row, start);
                let mut right = layout.x_in_row(row, end);
                if covers_break {
                    right += px(editor::NEWLINE_SELECTION);
                }
                let top = bounds.top() + line_height * ix as f32;
                selections.push(fill(
                    Bounds::from_corners(point(bounds.left() + left, top), point(bounds.left() + right, top + line_height)),
                    color,
                ));
            }
        }

        let caret = layout.point_for(area.cursor_offset());
        let caret = Bounds::new(bounds.origin + caret, size(px(editor::CARET_WIDTH), line_height));
        if area.autoscroll
            && let Some(handle) = &area.scroll
        {
            let viewport = handle.bounds();
            let mut offset = handle.offset();
            if caret.bottom() > viewport.bottom() {
                offset.y -= caret.bottom() - viewport.bottom();
            } else if caret.top() < viewport.top() {
                offset.y += viewport.top() - caret.top();
            }
            if offset != handle.offset() && viewport.size.height > Pixels::ZERO {
                handle.set_offset(offset);
                window.on_next_frame(|window, _| window.refresh());
            }
        }
        let cursor = range.is_empty().then(|| fill(caret, palette.accent));
        PrepaintState { layout: Some(layout), placeholder, selections, cursor }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.area.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.area.clone()), cx);
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        let line_height = window.line_height();
        let mut y = bounds.top();
        for line in &prepaint.placeholder {
            line.paint(point(bounds.left(), y), line_height, TextAlign::Left, None, window, cx).ok();
            y += line_height * (line.wrap_boundaries().len() + 1) as f32;
        }
        let Some(layout) = prepaint.layout.take() else { return };
        if prepaint.placeholder.is_empty() {
            let mut y = bounds.top();
            for line in &layout.lines {
                line.paint(point(bounds.left(), y), line_height, TextAlign::Left, None, window, cx).ok();
                y += line_height * (line.wrap_boundaries().len() + 1) as f32;
            }
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.area.update(cx, |area, _| {
            area.layout = Some(layout);
            area.last_bounds = Some(bounds);
            area.autoscroll = false;
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_document_start))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextAreaElement { area: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
