//! Text fields are gpui-kit's (MS-226): `InputState` for one line (the search box, forms, the chat
//! composer; `masked` for a secret), `TextareaState` for written text (the compose body, the
//! inline reply). `field` and `area` draw them without the kit's frame, in the text style around
//! them, so a view frames them itself (`controls::field_frame`) or not at all. Their key context
//! is `Input` (`!Input` keeps single-key bindings out of them). The kit emits `Change` and
//! `PressEnter`; Esc reaches the view as the kit's `Escape` action, and a view that needs other
//! keys inside its fields binds them in `"<its context> > Input"`, after the kit's own.

use gpui::base::input::{InputBaseState, MultiLineMode};
use gpui::component::Sizable;
use gpui::component::input::{Editor, EditorState, Input, InputState, Textarea, TextareaState};
use gpui::{App, Context, DefiniteLength, Entity, EntityInputHandler, IntoElement, RenderOnce, Styled, Window, px, relative};

use crate::design;

/// A one-line field filling its row, as tall as a line of the text around it.
#[derive(IntoElement)]
pub struct Field(Input);

pub fn field(state: &Entity<InputState>) -> Field {
    Field(Input::new(state).appearance(false))
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let line_height = window.line_height();
        // The kit sizes the field's text and height by its own scale; take the surrounding text's.
        Styled::h(self.0.text_size(style.font_size).line_height(style.line_height).p_0().gap_0(), line_height)
    }
}

/// A multi-line editor in the text style around it, inset only by the kit's smallest padding
/// (`AREA_INSET` on each side); its state's rows or `auto_grow` set its height, `full` fills its
/// parent and scrolls inside.
#[derive(IntoElement)]
pub struct Area {
    textarea: Textarea,
    full: bool,
}

/// The horizontal inset the kit keeps inside an `area` (its `XSmall` padding): a view pads it that
/// much less to keep the text where it was.
pub const AREA_INSET: f32 = 4.0;

pub fn area(state: &Entity<TextareaState>) -> Area {
    Area { textarea: Textarea::new(state).appearance(false).xsmall(), full: false }
}

impl Area {
    /// Fills the parent's height and scrolls inside it.
    #[allow(dead_code)]
    pub fn full(mut self) -> Self {
        self.full = true;
        self
    }
}

impl RenderOnce for Area {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let textarea = self.textarea.text_size(style.font_size).line_height(style.line_height);
        if self.full { textarea.h(gpui::relative(1.)) } else { textarea }
    }
}

/// A multi-line WYSIWYM markup editor without the kit's frame or monospace override, in the
/// surrounding UI font and text style.
#[derive(IntoElement)]
pub struct MarkupEditor {
    editor: Editor,
    height: Option<DefiniteLength>,
}

pub fn editor(state: &Entity<EditorState>) -> MarkupEditor {
    MarkupEditor { editor: Editor::new(state).appearance(false).bordered(false).xsmall(), height: None }
}

impl MarkupEditor {
    /// Fills the parent's height and scrolls inside it.
    pub fn full(mut self) -> Self {
        self.height = Some(relative(1.).into());
        self
    }

    /// Fixes the editor's height (for auto-growing inline cards).
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(px(height).into());
        self
    }
}

impl RenderOnce for MarkupEditor {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let editor = self
            .editor
            .font_family(design::ui_font())
            .text_size(style.font_size)
            .line_height(style.line_height);
        match self.height {
            Some(h) => editor.h(h),
            None => editor,
        }
    }
}

/// Wraps (or unwraps) the active selection in `marker` (`**`, `*`, `_`, `~~`, `~`, `` ` ``).
pub fn wrap_selection<M: MultiLineMode>(
    input: &mut InputBaseState<M>,
    marker: &str,
    window: &mut Window,
    cx: &mut Context<InputBaseState<M>>,
) {
    if input.marked_text_range(window, cx).is_some() {
        return;
    }
    let range = input.selected_range();
    let full = input.text().to_string();
    let m = marker.len();
    if range.is_empty() {
        let pos = range.start;
        if pos >= m && full.get(pos - m..pos) == Some(marker) && full.get(pos..pos + m) == Some(marker) {
            input.set_selected_range(pos - m..pos + m, cx);
            input.replace("", window, cx);
            input.set_selected_range(pos - m..pos - m, cx);
            return;
        }
        input.replace(format!("{marker}{marker}"), window, cx);
        input.set_selected_range(pos + m..pos + m, cx);
        return;
    }
    let selected = &full[range.clone()];
    let surrounding_match = if marker == "*" {
        let left = full[..range.start].bytes().rev().take_while(|&b| b == b'*').count();
        let right = full[range.end..].bytes().take_while(|&b| b == b'*').count();
        matches!(left.min(right), 1 | 3)
    } else {
        range.start >= m
            && full.get(range.start - m..range.start) == Some(marker)
            && full.get(range.end..range.end + m) == Some(marker)
    };
    if surrounding_match {
        let unwrapped = selected.to_owned();
        let start = range.start - m;
        let end = range.end - m;
        input.set_selected_range(range.start - m..range.end + m, cx);
        input.replace(unwrapped, window, cx);
        input.set_selected_range(start..end, cx);
    } else if selected.len() > 2 * m
        && selected.starts_with(marker)
        && selected.ends_with(marker)
        && !selected[m..selected.len() - m].contains(marker)
    {
        let inner = selected[m..selected.len() - m].to_owned();
        let start = range.start;
        let end = start + inner.len();
        input.replace(inner, window, cx);
        input.set_selected_range(start..end, cx);
    } else {
        let wrapped = format!("{marker}{selected}{marker}");
        let start = range.start + m;
        let end = range.end + m;
        input.replace(wrapped, window, cx);
        input.set_selected_range(start..end, cx);
    }
}

/// Inserts a Markdown link `[label](url)`: wraps the selection as `[selected](url)` with `url`
/// selected, or inserts `[](url)` with the caret inside `[]` when nothing is selected.
pub fn insert_link<M: MultiLineMode>(
    input: &mut InputBaseState<M>,
    window: &mut Window,
    cx: &mut Context<InputBaseState<M>>,
) {
    if input.marked_text_range(window, cx).is_some() {
        return;
    }
    let range = input.selected_range();
    let full = input.text().to_string();
    if range.is_empty() {
        let pos = range.start;
        input.replace("[](url)", window, cx);
        input.set_selected_range(pos + 1..pos + 1, cx);
    } else {
        let selected = &full[range.clone()];
        let link = format!("[{selected}](url)");
        let url_start = range.start + 1 + selected.len() + 2;
        input.replace(link, window, cx);
        input.set_selected_range(url_start..url_start + 3, cx);
    }
}
