//! Text fields are gpui-kit's (MS-226): `InputState` for one line (the search box, forms, the chat
//! composer; `masked` for a secret), `TextareaState` for written text (the compose body, the
//! inline reply). `field` and `area` draw them without the kit's frame, in the text style around
//! them, so a view frames them itself (`controls::field_frame`) or not at all. Their key context
//! is `Input` (`!Input` keeps single-key bindings out of them). The kit emits `Change` and
//! `PressEnter`; Esc reaches the view as the kit's `Escape` action, and a view that needs other
//! keys inside its fields binds them in `"<its context> > Input"`, after the kit's own.

use gpui::component::Sizable;
use gpui::component::input::{Input, InputState, Textarea, TextareaState};
use gpui::{App, Entity, IntoElement, RenderOnce, Styled, Window};

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
