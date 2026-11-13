//! The Mac's text checking in the fields that opt in with the kit's `.autocorrect(true)` (MS-318):
//! misspelled words underlined, the system's inline prediction dimmed after the caret (Tab takes
//! it, Esc declines it), the spell checker's guesses atop the field's context menu, and the
//! toggles in Edit ▸ Spelling and Grammar. The platform finds them (GPUI Fast's
//! `Window::text_assistance`, patched in `vendor/gpui-fast-macos/src/text_checking.rs`, which also
//! corrects a misspelled word as it is finished); `input::field`, `area` and `editor` draw them
//! through `checked` and `context_menu` here. Elsewhere the platform finds nothing and the fields
//! look as before.

use std::ops::Range;

use gpui::base::input::{InputBaseState, InputModeKind};
use gpui::component::input::{Copy, Cut, Paste, SelectAll};
use gpui::component::native_menu::NativeMenu;
use gpui::{
    Action, App, ContentMask, Div, Entity, EntityInputHandler, FocusHandle, Focusable, Font, InlinePrediction, InteractiveElement,
    IntoElement, KeyBinding, Menu, MenuItem, ParentElement, Pixels, SharedString, Styled, TextAlign, TextRun,
    UnderlineStyle, Window, actions, canvas, div, point, px,
};

use crate::design::palette;

actions!(
    text_checking,
    [
        /// Inserts the prediction drawn after the caret.
        AcceptPrediction,
        /// Hides the prediction drawn after the caret.
        DeclinePrediction,
        ToggleSpellingWhileTyping,
        ToggleCorrectSpelling,
        ToggleInlinePredictions,
    ]
);

/// Replaces a misspelled word (UTF-16 `range`) with one of the spell checker's guesses.
#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
struct ReplaceMisspelling {
    range: Range<usize>,
    replacement: String,
}

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
struct IgnoreSpelling {
    word: String,
}

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
struct LearnSpelling {
    word: String,
}

/// The key context `checked` puts around a field.
const CONTEXT: &str = "TextChecked";

/// Binds Tab and Esc to the prediction (after every view's own bindings of them in fields, so
/// these come first; without a prediction they pass the key on) and the menu's toggles, after
/// which `menus_changed` rebuilds the menu bar to show them.
pub fn init(cx: &mut App, menus_changed: fn(&mut App)) {
    let field = Some("TextChecked > Input");
    cx.bind_keys([
        KeyBinding::new("tab", AcceptPrediction, field),
        KeyBinding::new("escape", DeclinePrediction, field),
    ]);
    cx.on_action(move |_: &ToggleSpellingWhileTyping, cx| {
        let mut settings = cx.text_checking_settings();
        settings.check_spelling_while_typing = !settings.check_spelling_while_typing;
        cx.set_text_checking_settings(settings);
        menus_changed(cx);
    });
    cx.on_action(move |_: &ToggleCorrectSpelling, cx| {
        let mut settings = cx.text_checking_settings();
        settings.correct_spelling_automatically = !settings.correct_spelling_automatically;
        cx.set_text_checking_settings(settings);
        menus_changed(cx);
    });
    cx.on_action(move |_: &ToggleInlinePredictions, cx| {
        let mut settings = cx.text_checking_settings();
        settings.inline_predictions = !settings.inline_predictions;
        cx.set_text_checking_settings(settings);
        menus_changed(cx);
    });
}

/// Edit ▸ Spelling and Grammar.
pub fn menu(cx: &App) -> MenuItem {
    let settings = cx.text_checking_settings();
    MenuItem::submenu(Menu::new("Spelling and Grammar").items([
        MenuItem::action("Check Spelling While Typing", ToggleSpellingWhileTyping)
            .checked(settings.check_spelling_while_typing),
        MenuItem::action("Correct Spelling Automatically", ToggleCorrectSpelling)
            .checked(settings.correct_spelling_automatically),
        MenuItem::action("Show Inline Predictive Text", ToggleInlinePredictions).checked(settings.inline_predictions),
    ]))
}

/// Whether the platform checks the text of `state`'s field.
pub fn enabled<M: InputModeKind>(state: &Entity<InputBaseState<M>>, cx: &App) -> bool {
    state.read(cx).text_input_configuration().suggestions
}

/// `field` with its misspelled words underlined and the prediction drawn after its caret, in
/// `font`; Tab and Esc reach the prediction, the context menu's spelling items the field. The
/// returned box holds only `field`: the caller sizes it as it would have sized `field`.
pub fn checked<M: InputModeKind>(
    field: impl IntoElement,
    state: &Entity<InputBaseState<M>>,
    font: Font,
    window: &Window,
) -> Div {
    let p = palette(window.appearance());
    let (misspelled, predicted) = (p.danger, p.faint);
    div()
        .relative()
        .key_context(CONTEXT)
        .on_action({
            let state = state.clone();
            move |_: &AcceptPrediction, window, cx| match prediction(&state, window, cx) {
                Some(prediction) => state.update(cx, |state, cx| {
                    let at = prediction.offset..prediction.offset;
                    state.replace_text_in_range(Some(at), &prediction.text, window, cx);
                }),
                None => cx.propagate(),
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &DeclinePrediction, window, cx| match prediction(&state, window, cx) {
                Some(_) => window.dismiss_inline_prediction(),
                None => cx.propagate(),
            }
        })
        .on_action({
            let state = state.clone();
            move |action: &ReplaceMisspelling, window, cx| {
                state.update(cx, |state, cx| {
                    state.replace_text_in_range(Some(action.range.clone()), &action.replacement, window, cx);
                });
                // The platform didn't make this edit; it checks again when told.
                window.notify_text_input_changed(&focus_handle(&state, cx));
            }
        })
        .on_action(|action: &IgnoreSpelling, window, _| window.ignore_spelling(&action.word))
        .on_action(|action: &LearnSpelling, window, _| window.learn_spelling(&action.word))
        .child(field)
        .child(
            canvas(|_, _, _| {}, {
                let state = state.clone();
                move |bounds, (), window, cx| {
                    let focus = focus_handle(&state, cx);
                    let Some(assistance) = window.text_assistance(&focus).filter(|_| focus.is_focused(window)).cloned()
                    else {
                        return;
                    };
                    let prediction = prediction(&state, window, cx);
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        let underline = UnderlineStyle { thickness: px(1.), color: Some(misspelled), wavy: true };
                        for word in assistance.misspellings {
                            if let Some(word) = range_bounds(&state, word, window, cx) {
                                let at = point(word.origin.x, word.bottom() - px(3.));
                                window.paint_underline(at, word.size.width, &underline);
                            }
                        }
                        let Some(prediction) = prediction else {
                            return;
                        };
                        let Some(caret) = range_bounds(&state, prediction.offset..prediction.offset, window, cx) else {
                            return;
                        };
                        let text = SharedString::from(prediction.text.trim_end().to_owned());
                        let run = TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color: predicted,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let size = window.text_style().font_size.to_pixels(window.rem_size());
                        let line = window.text_system().shape_line(text, size, &[run], None);
                        line.paint(caret.origin, caret.size.height, TextAlign::Left, None, window, cx).ok();
                    });
                }
            })
            .absolute()
            .inset_0(),
        )
}

/// The context menu of a checked field: the spell checker's guesses for the misspelled word
/// under the pointer, Ignore and Learn Spelling, then the kit's own items.
pub fn context_menu<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    move |mut menu, window, cx| {
        let focus = focus_handle(&state, cx);
        let misspellings = window.text_assistance(&focus).map(|assistance| assistance.misspellings.clone());
        let pointer = window.mouse_position();
        let at = state.update(cx, |state, cx| state.character_index_for_point(pointer, window, cx));
        let misspelled = misspellings
            .zip(at)
            .and_then(|(words, at)| words.into_iter().find(|word| (word.start..=word.end).contains(&at)));
        if let Some(range) = misspelled {
            let word = state
                .update(cx, |state, cx| state.text_for_range(range.clone(), &mut None, window, cx))
                .unwrap_or_default();
            let guesses = window.spelling_guesses(&word);
            if guesses.is_empty() {
                menu = menu.menu_with_disabled("No Guesses Found", true, Box::new(gpui::NoAction));
            }
            for guess in guesses.into_iter().take(5) {
                let replace = ReplaceMisspelling { range: range.clone(), replacement: guess.clone() };
                menu = menu.menu(guess, Box::new(replace));
            }
            menu = menu
                .separator()
                .menu("Ignore Spelling", Box::new(IgnoreSpelling { word: word.clone() }))
                .menu("Learn Spelling", Box::new(LearnSpelling { word }))
                .separator();
        }
        // The kit's menu, which a custom one replaces.
        let capabilities = state.read(cx).context_menu_capabilities();
        let editable = !capabilities.is_disabled() && !capabilities.is_readonly();
        menu.menu_with_disabled("Cut", !(editable && capabilities.is_copyable()), Box::new(Cut))
            .menu_with_disabled("Copy", !capabilities.is_copyable(), Box::new(Copy))
            .menu_with_disabled("Paste", !editable, Box::new(Paste))
            .separator()
            .menu("Select All", Box::new(SelectAll))
    }
}

fn focus_handle<M: InputModeKind>(state: &Entity<InputBaseState<M>>, cx: &App) -> FocusHandle {
    state.focus_handle(cx)
}

/// The platform's prediction for `state`'s field while its caret is where it was predicted.
fn prediction<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    window: &mut Window,
    cx: &mut App,
) -> Option<InlinePrediction> {
    let focus = focus_handle(state, cx);
    let prediction = window.text_assistance(&focus)?.prediction.clone()?;
    let caret = state.update(cx, |state, cx| state.selected_text_range(false, window, cx))?.range;
    (caret == (prediction.offset..prediction.offset)).then_some(prediction)
}

/// Where the UTF-16 `range` of `state`'s text is drawn, one line high, in the window.
fn range_bounds<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    range: Range<usize>,
    window: &mut Window,
    cx: &mut App,
) -> Option<gpui::Bounds<Pixels>> {
    state.update(cx, |state, cx| {
        let text = state.text_bounds()?;
        state.bounds_for_range(range, text, window, cx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::component::input::{Textarea, TextareaState};
    use gpui::{AppContext as _, Context, Render, TestAppContext, TextAssistance, VisualTestContext};

    struct Host(Entity<TextareaState>);

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let font = window.text_style().font();
            let field = Textarea::new(&self.0).context_menu(context_menu(&self.0));
            div().w(px(300.)).h(px(120.)).child(checked(field, &self.0, font, window).size_full())
        }
    }

    /// A focused, checked area holding `text` with the caret at its end.
    fn area<'a>(text: &str, cx: &'a mut TestAppContext) -> (Entity<TextareaState>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui::init(cx);
            init(cx, |_| {});
        });
        let text = text.to_owned();
        let (host, cx) = cx.add_window_view(|window, cx| {
            Host(cx.new(|cx| TextareaState::new(window, cx).autocorrect(true).default_value(text)))
        });
        let state = host.read_with(cx, |host, _| host.0.clone());
        state.update_in(cx, |state, window, cx| {
            state.focus(window, cx);
            let end = state.text().len();
            state.set_selected_range(end..end, cx);
        });
        cx.run_until_parked();
        (state, cx)
    }

    fn predict(offset: usize, text: &str, cx: &mut VisualTestContext) {
        let prediction = Some(InlinePrediction { offset, text: text.into() });
        cx.update(|window, _| window.simulate_text_assistance(TextAssistance { prediction, misspellings: Vec::new() }));
        cx.run_until_parked();
    }

    fn value(state: &Entity<TextareaState>, cx: &mut VisualTestContext) -> String {
        state.read_with(cx, |state, _| state.value().to_string())
    }

    #[gpui::test]
    fn tab_takes_the_prediction(cx: &mut TestAppContext) {
        let (state, cx) = area("Looking forward to hea", cx);
        predict(22, "ring ", cx);
        cx.simulate_keystrokes("tab");
        assert_eq!(value(&state, cx), "Looking forward to hearing ");
    }

    #[gpui::test]
    fn escape_declines_the_prediction(cx: &mut TestAppContext) {
        let (state, cx) = area("See you ", cx);
        predict(8, "tomorrow ", cx);
        cx.simulate_keystrokes("escape");
        let focus = cx.update(|_, cx| state.focus_handle(cx));
        let prediction = cx.update(|window, _| window.text_assistance(&focus).and_then(|a| a.prediction.clone()));
        assert_eq!(prediction, None);
        // Tab is the field's own again.
        cx.simulate_keystrokes("tab");
        assert!(!value(&state, cx).contains("tomorrow"));
    }

    #[gpui::test]
    fn a_moved_caret_leaves_the_prediction_alone(cx: &mut TestAppContext) {
        let (state, cx) = area("See you ", cx);
        predict(3, "tomorrow ", cx);
        cx.simulate_keystrokes("tab");
        assert!(!value(&state, cx).contains("tomorrow"));
    }

    #[gpui::test]
    fn a_guess_replaces_the_misspelling(cx: &mut TestAppContext) {
        let (state, cx) = area("Helo world", cx);
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(ReplaceMisspelling { range: 0..4, replacement: "Hello".into() }), cx)
        });
        cx.run_until_parked();
        assert_eq!(value(&state, cx), "Hello world");
    }
}
