//! Text fields are gpui-kit's (MS-226): `InputState` for one line (the search box, forms;
//! `masked` for a secret), `EditorState` for live-highlighted markup (`editor`). `field` and
//! `editor` draw them without the kit's frame, in the text style around them, so a view frames
//! them itself (`controls::field_frame`) or not at all. Their key context is `Input` (`!Input`
//! keeps single-key bindings out of them). The kit emits `Change` and
//! `PressEnter`; Esc reaches the view as the kit's `Escape` action, and a view that needs other
//! keys inside its fields binds them in `"<its context> > Input"`, after the kit's own. Fields
//! built with the kit's `.autocorrect(true)` also show the Mac's text checking
//! (`text_checking`).

use gpui::base::input::{InputBaseState, MultiLineMode};
use gpui::component::Sizable;
use gpui::component::input::{Editor, EditorState, Input, InputState};
use gpui::{
    App, Context, DefiniteLength, Entity, EntityInputHandler, IntoElement, ParentElement, RenderOnce, Styled, Window,
    div, px, relative,
};

use crate::{design, text_checking};

/// A one-line field filling its row, as tall as a line of the text around it.
#[derive(IntoElement)]
pub struct Field {
    input: Input,
    state: Entity<InputState>,
}

pub fn field(state: &Entity<InputState>) -> Field {
    Field { input: Input::new(state).appearance(false), state: state.clone() }
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let line_height = window.line_height();
        // The kit sizes the field's text and height by its own scale; take the surrounding text's.
        let input = Styled::h(self.input.text_size(style.font_size).line_height(style.line_height).p_0().gap_0(), line_height);
        if !text_checking::enabled(&self.state, cx) {
            return input.into_any_element();
        }
        let input = input.context_menu(text_checking::context_menu(&self.state));
        text_checking::checked(input, &self.state, style.font(), window).w_full().h(line_height).into_any_element()
    }
}

/// The horizontal inset the kit keeps inside an `area` (its `XSmall` padding): a view pads it that
/// much less to keep the text where it was.
pub const AREA_INSET: f32 = 4.0;
/// A multi-line WYSIWYM markup editor without the kit's frame or monospace override, in the
/// surrounding UI font and text style.
#[derive(IntoElement)]
pub struct MarkupEditor {
    state: Entity<EditorState>,
    editor: Editor,
    height: Option<DefiniteLength>,
    auto_grow: Option<(usize, usize)>,
}

pub fn editor(state: &Entity<EditorState>) -> MarkupEditor {
    MarkupEditor {
        state: state.clone(),
        editor: Editor::new(state).appearance(false).bordered(false).xsmall(),
        height: None,
        auto_grow: None,
    }
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

    /// Sizes the editor by wrapped line count (`min_rows..=max_rows`) using `window.line_height()`.
    pub fn auto_grow(mut self, min_rows: usize, max_rows: usize) -> Self {
        self.auto_grow = Some((min_rows, max_rows));
        self
    }
}

impl RenderOnce for MarkupEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut style = window.text_style();
        style.font_family = design::ui_font().into();
        let line_height = window.line_height();
        let height = self.height.or_else(|| {
            self.auto_grow.map(|(min_rows, max_rows)| {
                let rows = self.state.read(cx).display_map().wrap_row_count().clamp(min_rows, max_rows);
                (line_height * rows as f32).into()
            })
        });
        let editor = self
            .editor
            .font_family(design::ui_font())
            .text_size(style.font_size)
            .line_height(style.line_height);
        if !text_checking::enabled(&self.state, cx) {
            return match height {
                Some(h) => div().w_full().h(h).overflow_hidden().child(editor.h(h)).into_any_element(),
                None => editor.into_any_element(),
            };
        }
        let editor = editor.context_menu(text_checking::context_menu(&self.state));
        match height {
            Some(h) => text_checking::checked(editor.h(h), &self.state, style.font(), window)
                .w_full()
                .h(h)
                .overflow_hidden()
                .into_any_element(),
            None => text_checking::checked(editor, &self.state, style.font(), window).w_full().into_any_element(),
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
