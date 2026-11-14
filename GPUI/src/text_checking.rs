//! The Mac's text checking for gpui-kit's text fields, as `NSTextView` does it (MS-318, MS-351):
//! misspelled words underlined, a misspelled word corrected as it is finished (with the system's
//! bubble, which takes the correction back), the system's inline prediction dimmed after the caret
//! (Tab takes it, Esc declines it), the spell checker's guesses atop the field's context menu, and
//! the toggles in Edit ▸ Spelling and Grammar.
//!
//! It is a component on the kit's public input API alone, no platform hooks: a field that opts in
//! with the kit's `.autocorrect(true)` gets a `TextChecker` (`checker`), which watches the field's
//! state (its edits, caret and focus) and asks the spell checker (`mac.rs`) again whenever they
//! change; `checked` draws what it found over the field and `context_menu` adds the guesses.
//! Elsewhere, and on other platforms, the fields look as before.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui::base::input::{InputBaseState, InputEditsEvent, InputEvent, InputModeKind};
use gpui::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui::component::native_menu::NativeMenu;
use gpui::{
    Action, App, ContentMask, Context, Div, Entity, EntityInputHandler, FocusHandle, Focusable, Font, Global,
    InteractiveElement, IntoElement, KeyBinding, Menu, MenuItem, ParentElement, Pixels, SharedString, Styled,
    Subscription, Task, TextAlign, TextRun, UnderlineStyle, WeakEntity, Window, WindowId, actions, canvas, div, point,
    px,
};

use crate::design::palette;

#[cfg(target_os = "macos")]
#[path = "text_checking/mac.rs"]
mod system;

/// Other platforms check nothing.
#[cfg(not(target_os = "macos"))]
mod system {
    use std::ops::Range;

    use futures::channel::oneshot;
    use gpui::{Bounds, Pixels, Window};

    use super::Settings;

    pub struct SpellDocument;

    impl SpellDocument {
        pub fn new() -> Self {
            Self
        }
    }

    pub fn settings() -> Settings {
        Settings::default()
    }
    pub fn set_settings(_: Settings) {}
    pub fn guesses(_: &str, _: &SpellDocument) -> Vec<String> {
        Vec::new()
    }
    pub fn ignore(_: &str, _: &SpellDocument) {}
    pub fn learn(_: &str) {}
    pub fn request_candidates(_: &str, _: &SpellDocument) -> oneshot::Receiver<Vec<(Range<usize>, String)>> {
        oneshot::channel().1
    }
    pub fn request_misspellings(_: &str, _: &SpellDocument) -> oneshot::Receiver<Vec<Range<usize>>> {
        oneshot::channel().1
    }
    pub fn corrects_automatically() -> bool {
        false
    }
    pub fn correction(_: &str, _: Range<usize>, _: &str, _: &SpellDocument) -> Option<String> {
        None
    }
    pub fn show_correction(_: &Window, _: Bounds<Pixels>, _: &str) -> Option<oneshot::Receiver<String>> {
        None
    }
    pub fn dismiss_correction(_: &Window) {}
    pub fn record_reverted(_: &str, _: &str, _: &SpellDocument) {}
}

use system::SpellDocument;

/// How much text around the caret is checked, in UTF-16 code units.
const CHECKED_TEXT: usize = 16_384;
/// How far back from a typed separator the word to correct is looked for.
const WORD_CONTEXT: usize = 128;

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

/// The user's text checking preferences (Edit ▸ Spelling and Grammar).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Underline misspelled words while typing.
    pub check_spelling_while_typing: bool,
    /// Correct a misspelled word when the user finishes typing it.
    pub correct_spelling_automatically: bool,
    /// Predict the rest of the word or the next word at the caret.
    pub inline_predictions: bool,
}

/// What the spell checker found in a field. Offsets are UTF-16 code units, as in
/// `EntityInputHandler`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextAssistance {
    /// The text predicted at the caret, if any.
    pub prediction: Option<InlinePrediction>,
    /// The misspelled words, in document order.
    pub misspellings: Vec<Range<usize>>,
}

/// Text predicted at the caret (macOS inline predictive text): drawn dimmed after the caret while
/// it is still at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePrediction {
    /// The caret offset the prediction continues from.
    pub offset: usize,
    /// The text to insert at `offset`.
    pub text: String,
}

/// Set whenever every field checks again: the settings changed, or (in tests and screenshots) the
/// simulated results did.
struct Recheck;

impl Global for Recheck {}

/// The spell checker's document of each window: words ignored in one field are ignored in the
/// window's others.
#[derive(Default)]
struct Documents(HashMap<WindowId, Weak<SpellDocument>>);

impl Global for Documents {}

/// Present in tests and screenshots: fields don't ask the system's spell checker, which differs
/// between Macs, but find what `simulate` hands the focused field.
#[cfg(any(test, feature = "shots"))]
#[derive(Default)]
struct Simulated(Vec<(FocusHandle, TextAssistance)>);

#[cfg(any(test, feature = "shots"))]
impl Global for Simulated {}

/// Binds Tab and Esc to the prediction (after every view's own bindings of them in fields, so
/// these come first; without a prediction they pass the key on) and the menu's toggles, after
/// which `menus_changed` rebuilds the menu bar to show them.
pub fn init(cx: &mut App, menus_changed: fn(&mut App)) {
    let field = Some("TextChecked > Input");
    cx.bind_keys([
        KeyBinding::new("tab", AcceptPrediction, field),
        KeyBinding::new("escape", DeclinePrediction, field),
    ]);
    cx.set_global(Recheck);
    let toggle = move |cx: &mut App, change: fn(&mut Settings)| {
        let mut settings = system::settings();
        change(&mut settings);
        system::set_settings(settings);
        cx.set_global(Recheck);
        menus_changed(cx);
    };
    cx.on_action(move |_: &ToggleSpellingWhileTyping, cx| {
        toggle(cx, |s| s.check_spelling_while_typing = !s.check_spelling_while_typing)
    });
    cx.on_action(move |_: &ToggleCorrectSpelling, cx| {
        toggle(cx, |s| s.correct_spelling_automatically = !s.correct_spelling_automatically)
    });
    cx.on_action(move |_: &ToggleInlinePredictions, cx| toggle(cx, |s| s.inline_predictions = !s.inline_predictions));
}

/// Edit ▸ Spelling and Grammar.
pub fn menu() -> MenuItem {
    let settings = system::settings();
    MenuItem::submenu(Menu::new("Spelling and Grammar").items([
        MenuItem::action("Check Spelling While Typing", ToggleSpellingWhileTyping)
            .checked(settings.check_spelling_while_typing),
        MenuItem::action("Correct Spelling Automatically", ToggleCorrectSpelling)
            .checked(settings.correct_spelling_automatically),
        MenuItem::action("Show Inline Predictive Text", ToggleInlinePredictions).checked(settings.inline_predictions),
    ]))
}

/// From now on fields find only what `simulate` hands them, and correct nothing.
#[cfg(any(test, feature = "shots"))]
pub fn simulate_spell_checker(cx: &mut App) {
    cx.set_global(Simulated::default());
}

/// Hands `assistance` to the focused field as the spell checker would, until its text or caret
/// changes (`simulate_spell_checker` first).
#[cfg(any(test, feature = "shots"))]
pub fn simulate(assistance: TextAssistance, window: &Window, cx: &mut App) {
    if let Some(focus) = window.focused(cx) {
        let simulated = &mut cx.global_mut::<Simulated>().0;
        simulated.retain(|(field, _)| *field != focus);
        simulated.push((focus, assistance));
        cx.set_global(Recheck);
    }
}

/// The text checking of a field drawn in this element, kept while the element is drawn; `None`
/// unless the field opted in (the kit's `.autocorrect(true)`).
pub fn checker<M: InputModeKind>(
    input: &Entity<InputBaseState<M>>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<TextChecker<M>>> {
    input.read(cx).text_input_configuration().suggestions.then(|| {
        window.use_keyed_state(("text-checker", input.entity_id().as_u64()), cx, |window, cx| {
            TextChecker::new(input.clone(), window, cx)
        })
    })
}

/// A field's text checking: what the spell checker found in it while it has focus.
pub struct TextChecker<M: InputModeKind> {
    input: Entity<InputBaseState<M>>,
    document: Rc<SpellDocument>,
    found: TextAssistance,
    /// The prediction the user declined, kept from coming back.
    declined: Option<InlinePrediction>,
    /// The field's selection and whether it was composing when last seen, and whether its text
    /// changed since.
    seen: (Range<usize>, bool),
    edited: bool,
    /// An undo, redo or paste is being handled: its edits weren't typed.
    replaying: bool,
    check: Option<Task<()>>,
    _subscriptions: [Subscription; 4],
}

impl<M: InputModeKind> TextChecker<M> {
    fn new(input: Entity<InputBaseState<M>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = [
            cx.observe_in(&input, window, |this, _, window, cx| this.input_notified(window, cx)),
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Focus => this.check(window, cx),
                InputEvent::Blur => this.forget(cx),
                _ => {}
            }),
            cx.subscribe_in(&input, window, |this, _, event: &InputEditsEvent, window, cx| {
                this.edited(event, window, cx)
            }),
            cx.observe_global_in::<Recheck>(window, |this, window, cx| this.check(window, cx)),
        ];
        let mut checker = Self {
            document: document(window, cx),
            seen: (input.read(cx).selected_range(), false),
            input,
            found: TextAssistance::default(),
            declined: None,
            edited: false,
            replaying: false,
            check: None,
            _subscriptions: subscriptions,
        };
        checker.check(window, cx);
        checker
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }

    /// The field changed in some way: checks again if its text, caret or composition did.
    fn input_notified(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // An undo or paste that changed nothing.
        self.replaying = false;
        let seen = self.input.update(cx, |input, cx| {
            (input.selected_range(), input.marked_text_range(window, cx).is_some())
        });
        if self.edited || seen != self.seen {
            let typing = std::mem::take(&mut self.edited);
            self.seen = seen;
            self.check_text(typing, window, cx);
        }
    }

    /// After an edit: a word the user ends by typing a space or punctuation mark is corrected,
    /// and typing on accepts the last correction. Typing is an edit that inserts one character at
    /// the caret outside an undo, redo or paste (an input method's commit replaces what it
    /// composed instead).
    fn edited(&mut self, event: &InputEditsEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.edited = true;
        if std::mem::take(&mut self.replaying) {
            return;
        }
        let [(edit, text)] = event.edits() else {
            return;
        };
        if edit.start_byte != edit.old_end_byte {
            return;
        }
        let typed = text.slice(edit.start_byte..edit.new_end_byte).to_string();
        if typed.chars().count() != 1 {
            return;
        }
        system::dismiss_correction(window);
        if ends_word(&typed)
            && self.input.read(cx).text_input_configuration().autocorrect
            && !simulated(cx)
            && system::corrects_automatically()
        {
            self.correct_word_before(&typed, window, cx);
        }
    }

    /// Checks the focused field again once the event being handled is done; a check still
    /// running is dropped.
    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.check_text(false, window, cx);
    }

    /// `check`; after an edit (`typing`) the word at the caret, being typed, isn't misspelled
    /// yet. A caret moved into a misspelled word leaves it underlined, as for the context menu.
    fn check_text(&mut self, typing: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle(cx).is_focused(window) {
            self.check = None;
            return;
        }
        self.check = Some(cx.spawn_in(window, async move |this, cx| {
            #[cfg(any(test, feature = "shots"))]
            if let Ok(Some(found)) = this.update(cx, |this, cx| this.simulated(cx)) {
                this.update(cx, |this, cx| this.found(found, cx)).ok();
                return;
            }
            let Ok(Some((snapshot, document))) = this.update_in(cx, |this, window, cx| {
                Some((Snapshot::read(&this.input, window, cx)?, this.document.clone()))
            }) else {
                return;
            };
            let settings = system::settings();
            let prediction = snapshot
                .before_caret()
                .filter(|_| settings.inline_predictions)
                .map(|before_caret| (before_caret, system::request_candidates(before_caret, &document)));
            let misspellings = settings
                .check_spelling_while_typing
                .then(|| system::request_misspellings(&snapshot.text, &document));

            let prediction = match prediction {
                Some((before_caret, candidates)) => candidates
                    .await
                    .ok()
                    .and_then(|candidates| continuation(before_caret, candidates))
                    .map(|text| InlinePrediction { offset: snapshot.start + snapshot.selection.start, text }),
                None => None,
            };
            let misspellings = match misspellings {
                Some(misspellings) => misspellings.await.unwrap_or_default(),
                None => Vec::new(),
            };
            let caret = snapshot.selection.end;
            let misspellings = misspellings
                .into_iter()
                .filter(|word| !(typing && (word.start..=word.end).contains(&caret)))
                .map(|word| snapshot.start + word.start..snapshot.start + word.end)
                .collect();
            this.update(cx, |this, cx| this.found(TextAssistance { prediction, misspellings }, cx)).ok();
        }));
    }

    /// What `simulate` handed this field since it last checked: `Some` in screenshots and
    /// always in tests, whose scheduler refuses the system spell checker's answers from its own
    /// threads.
    #[cfg(any(test, feature = "shots"))]
    fn simulated(&self, cx: &mut App) -> Option<TextAssistance> {
        let focus = self.focus_handle(cx);
        if !cx.has_global::<Simulated>() {
            return cfg!(test).then(TextAssistance::default);
        }
        let simulated = &mut cx.global_mut::<Simulated>().0;
        let found = simulated.iter().position(|(field, _)| *field == focus).map(|at| simulated.remove(at).1);
        Some(found.unwrap_or_default())
    }

    fn found(&mut self, mut found: TextAssistance, cx: &mut Context<Self>) {
        if found.prediction.is_some() && found.prediction == self.declined {
            found.prediction = None;
        }
        if self.found != found {
            self.found = found;
            cx.notify();
        }
    }

    /// The field lost focus.
    fn forget(&mut self, cx: &mut Context<Self>) {
        self.check = None;
        self.declined = None;
        self.found(TextAssistance::default(), cx);
    }

    fn decline_prediction(&mut self, cx: &mut Context<Self>) {
        self.declined = self.found.prediction.take();
        cx.notify();
    }

    /// Replaces the word before the separator just typed with the spell checker's correction and
    /// shows the correction's bubble, which takes it back when clicked.
    fn correct_word_before(&mut self, separator: &str, window: &mut Window, cx: &mut Context<Self>) {
        let separator_len = utf16_len(separator);
        // The text before the separator, and where it starts.
        let Some((start, before)) = self.input.update(cx, |input, cx| {
            let caret = input.selected_text_range(false, window, cx)?.range;
            let end = caret.start.checked_sub(separator_len).filter(|_| caret.is_empty())?;
            let from = end.saturating_sub(WORD_CONTEXT);
            let mut actual = None;
            let before = input.text_for_range(from..end, &mut actual, window, cx)?;
            Some((actual.map_or(from, |range| range.start), before))
        }) else {
            return;
        };
        let Some(original) = word_before(&before) else {
            return;
        };
        let word = utf16_len(&before) - utf16_len(original)..utf16_len(&before);
        let Some(correction) = system::correction(&before, word.clone(), separator, &self.document)
            .filter(|correction| correction != original)
        else {
            return;
        };

        let typed = start + word.start..start + word.end;
        let corrected = typed.start..typed.start + utf16_len(&correction);
        let caret = corrected.end + separator_len;
        let Some(bounds) = self.input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(typed), &correction, window, cx);
            input.set_selected_text_range(caret..caret, window, cx);
            let text = input.text_bounds()?;
            input.bounds_for_range(corrected.clone(), text, window, cx)
        }) else {
            return;
        };
        // The bubble offers the word as typed; choosing it reverts the correction.
        let original = original.to_owned();
        let Some(chosen) = system::show_correction(window, bounds, &original) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            if chosen.await.is_ok_and(|chosen| chosen == original) {
                this.update_in(cx, |this, window, cx| this.revert(corrected, &correction, &original, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Takes the correction at `corrected` back to `original`, if the field still shows it.
    fn revert(
        &mut self,
        corrected: Range<usize>,
        correction: &str,
        original: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_handle(cx).is_focused(window) {
            return;
        }
        let reverted = self.input.update(cx, |input, cx| {
            if input.text_for_range(corrected.clone(), &mut None, window, cx).as_deref() != Some(correction) {
                return false;
            }
            let caret = input.selected_text_range(false, window, cx).map(|selection| selection.range);
            input.replace_text_in_range(Some(corrected.clone()), original, window, cx);
            if let Some(caret) = caret.filter(|caret| caret.start >= corrected.end) {
                let caret = caret.start - corrected.len() + utf16_len(original);
                input.set_selected_text_range(caret..caret, window, cx);
            }
            true
        });
        if reverted {
            system::record_reverted(correction, original, &self.document);
            self.check(window, cx);
        }
    }
}

fn simulated(cx: &App) -> bool {
    #[cfg(any(test, feature = "shots"))]
    return cfg!(test) || cx.has_global::<Simulated>();
    #[cfg(not(any(test, feature = "shots")))]
    {
        let _ = cx;
        false
    }
}

/// The spell checker's document for `window`'s fields.
fn document(window: &Window, cx: &mut App) -> Rc<SpellDocument> {
    let id = window.window_handle().window_id();
    let documents = &mut cx.default_global::<Documents>().0;
    documents.retain(|_, document| document.strong_count() > 0);
    if let Some(document) = documents.get(&id).and_then(Weak::upgrade) {
        return document;
    }
    let document = Rc::new(SpellDocument::new());
    documents.insert(id, Rc::downgrade(&document));
    document
}

/// The focused field's text around the caret.
struct Snapshot {
    text: String,
    /// Where `text` starts in the field.
    start: usize,
    /// The selection, in `text`.
    selection: Range<usize>,
    composing: bool,
}

impl Snapshot {
    fn read<M: InputModeKind>(input: &Entity<InputBaseState<M>>, window: &mut Window, cx: &mut App) -> Option<Self> {
        input.update(cx, |input, cx| {
            let selection = input.selected_text_range(false, window, cx)?.range;
            let start = selection.start.saturating_sub(CHECKED_TEXT / 2);
            let mut actual = None;
            let text = input.text_for_range(start..start + CHECKED_TEXT, &mut actual, window, cx)?;
            let start = actual.map_or(start, |range| range.start);
            Some(Self {
                text,
                start,
                selection: selection.start.saturating_sub(start)..selection.end.saturating_sub(start),
                composing: input.marked_text_range(window, cx).is_some(),
            })
        })
    }

    /// The text before the caret, when a prediction can be drawn after it: nothing is selected
    /// or being composed and the caret ends its line (a drawn prediction would cover text).
    fn before_caret(&self) -> Option<&str> {
        if self.composing || !self.selection.is_empty() {
            return None;
        }
        let caret = byte_offset(&self.text, self.selection.start)?;
        let rest = &self.text[caret..];
        (rest.is_empty() || rest.starts_with(['\n', '\r'])).then(|| &self.text[..caret])
    }
}

/// `field` with its misspelled words underlined and the prediction drawn after its caret, in
/// `font`; Tab and Esc reach the prediction, the context menu's spelling items the field. The
/// returned box holds only `field`: the caller sizes it as it would have sized `field`.
pub fn checked<M: InputModeKind>(
    field: impl IntoElement,
    checker: &Entity<TextChecker<M>>,
    font: Font,
    window: &Window,
) -> Div {
    let p = palette(window.appearance());
    let (misspelled, predicted) = (p.danger, p.faint);
    div()
        .relative()
        .key_context(CONTEXT)
        .capture_action({
            let checker = checker.downgrade();
            move |_: &Undo, _, cx| replaying(&checker, cx)
        })
        .capture_action({
            let checker = checker.downgrade();
            move |_: &Redo, _, cx| replaying(&checker, cx)
        })
        .capture_action({
            let checker = checker.downgrade();
            move |_: &Paste, _, cx| replaying(&checker, cx)
        })
        .on_action({
            let checker = checker.clone();
            move |_: &AcceptPrediction, window, cx| match prediction(&checker, window, cx) {
                Some(prediction) => checker.read(cx).input.clone().update(cx, |input, cx| {
                    let at = prediction.offset..prediction.offset;
                    input.replace_text_in_range(Some(at), &prediction.text, window, cx);
                }),
                None => cx.propagate(),
            }
        })
        .on_action({
            let checker = checker.clone();
            move |_: &DeclinePrediction, window, cx| match prediction(&checker, window, cx) {
                Some(_) => checker.update(cx, |checker, cx| checker.decline_prediction(cx)),
                None => cx.propagate(),
            }
        })
        .on_action({
            let checker = checker.clone();
            move |action: &ReplaceMisspelling, window, cx| {
                checker.read(cx).input.clone().update(cx, |input, cx| {
                    input.replace_text_in_range(Some(action.range.clone()), &action.replacement, window, cx);
                });
            }
        })
        .on_action({
            let checker = checker.clone();
            move |action: &IgnoreSpelling, window, cx| {
                checker.update(cx, |checker, cx| {
                    system::ignore(&action.word, &checker.document);
                    checker.check(window, cx);
                })
            }
        })
        .on_action({
            let checker = checker.clone();
            move |action: &LearnSpelling, window, cx| {
                system::learn(&action.word);
                checker.update(cx, |checker, cx| checker.check(window, cx));
            }
        })
        .child(field)
        .child(
            canvas(|_, _, _| {}, {
                let checker = checker.clone();
                move |bounds, (), window, cx| {
                    let (input, focus, misspellings) = {
                        let checker = checker.read(cx);
                        (checker.input.clone(), checker.focus_handle(cx), checker.found.misspellings.clone())
                    };
                    if !focus.is_focused(window) {
                        return;
                    }
                    let prediction = prediction(&checker, window, cx);
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        let underline = UnderlineStyle { thickness: px(1.), color: Some(misspelled), wavy: true };
                        for word in misspellings {
                            if let Some(word) = range_bounds(&input, word, window, cx) {
                                let at = point(word.origin.x, word.bottom() - px(3.));
                                window.paint_underline(at, word.size.width, &underline);
                            }
                        }
                        let Some(prediction) = prediction else {
                            return;
                        };
                        let Some(caret) = range_bounds(&input, prediction.offset..prediction.offset, window, cx) else {
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

/// The field is about to undo, redo or paste.
fn replaying<M: InputModeKind>(checker: &WeakEntity<TextChecker<M>>, cx: &mut App) {
    checker.update(cx, |checker, _| checker.replaying = true).ok();
}

/// The checker's prediction while the field's caret is where it was predicted.
fn prediction<M: InputModeKind>(
    checker: &Entity<TextChecker<M>>,
    window: &mut Window,
    cx: &mut App,
) -> Option<InlinePrediction> {
    let (input, prediction) = {
        let checker = checker.read(cx);
        (checker.input.clone(), checker.found.prediction.clone()?)
    };
    let caret = input.update(cx, |input, cx| input.selected_text_range(false, window, cx))?.range;
    (caret == (prediction.offset..prediction.offset)).then_some(prediction)
}

/// Where the UTF-16 `range` of the field's text is drawn, one line high, in the window.
fn range_bounds<M: InputModeKind>(
    input: &Entity<InputBaseState<M>>,
    range: Range<usize>,
    window: &mut Window,
    cx: &mut App,
) -> Option<gpui::Bounds<Pixels>> {
    input.update(cx, |input, cx| {
        let text = input.text_bounds()?;
        input.bounds_for_range(range, text, window, cx)
    })
}

/// The context menu of a checked field: the spell checker's guesses for the misspelled word
/// under the pointer, Ignore and Learn Spelling, then the kit's own items.
pub fn context_menu<M: InputModeKind>(
    checker: &Entity<TextChecker<M>>,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let checker = checker.clone();
    move |mut menu, window, cx| {
        let (input, misspellings, document) = {
            let checker = checker.read(cx);
            (checker.input.clone(), checker.found.misspellings.clone(), checker.document.clone())
        };
        let pointer = window.mouse_position();
        let at = input.update(cx, |input, cx| input.character_index_for_point(pointer, window, cx));
        let misspelled = at.and_then(|at| misspellings.into_iter().find(|word| (word.start..=word.end).contains(&at)));
        if let Some(range) = misspelled {
            let word = input
                .update(cx, |input, cx| input.text_for_range(range.clone(), &mut None, window, cx))
                .unwrap_or_default();
            let guesses = system::guesses(&word, &document);
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
        let capabilities = input.read(cx).context_menu_capabilities();
        let editable = !capabilities.is_disabled() && !capabilities.is_readonly();
        menu.menu_with_disabled("Cut", !(editable && capabilities.is_copyable()), Box::new(Cut))
            .menu_with_disabled("Copy", !capabilities.is_copyable(), Box::new(Copy))
            .menu_with_disabled("Paste", !editable, Box::new(Paste))
            .separator()
            .menu("Select All", Box::new(SelectAll))
    }
}

/// Whether typing `text` ends a word.
fn ends_word(text: &str) -> bool {
    let mut chars = text.chars();
    matches!((chars.next(), chars.next()), (Some(' ' | '\t' | '\n' | '.' | ',' | '!' | '?' | ';' | ':' | ')'), None))
}

/// The word `text` ends with, unless it's too short to correct, an acronym, or part of an
/// address, path, number or identifier.
fn word_before(text: &str) -> Option<&str> {
    let start = text.char_indices().rev().take_while(|(_, c)| c.is_alphabetic() || matches!(c, '\'' | '’')).last()?.0;
    let word = &text[start..];
    let joined = text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '@' | '/' | '.' | '_' | '-' | ':'));
    let acronym = word.chars().all(|c| c.is_uppercase());
    (!joined && !acronym && word.chars().nth(1).is_some()).then_some(word)
}

/// What a candidate adds to the word `before_caret` ends with (`"om "` for "fr" and
/// "from "), or the next word after a space: the first that continues it with a letter or digit.
fn continuation(before_caret: &str, candidates: impl IntoIterator<Item = (Range<usize>, String)>) -> Option<String> {
    let end = utf16_len(before_caret);
    candidates.into_iter().find_map(|(range, replacement)| {
        if range.end != end {
            return None;
        }
        let typed = &before_caret[byte_offset(before_caret, range.start)?..];
        let rest = replacement.strip_prefix(typed)?;
        rest.starts_with(char::is_alphanumeric).then(|| rest.to_owned())
    })
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset of the UTF-16 offset `offset` in `text`, if it is on a character boundary.
fn byte_offset(text: &str, offset: usize) -> Option<usize> {
    let mut utf16 = 0;
    for (byte, c) in text.char_indices() {
        if utf16 == offset {
            return Some(byte);
        }
        utf16 += c.len_utf16();
    }
    (utf16 == offset).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::component::input::{Textarea, TextareaState};
    use gpui::{AppContext as _, Render, TestAppContext, VisualTestContext};

    struct Host(Entity<TextareaState>);

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let font = window.text_style().font();
            let checker = checker(&self.0, window, cx).expect("the area opts in");
            let field = Textarea::new(&self.0).context_menu(context_menu(&checker));
            div().w(px(300.)).h(px(120.)).child(checked(field, &checker, font, window).size_full())
        }
    }

    /// A focused, checked area holding `text` with the caret at its end.
    fn area<'a>(text: &str, cx: &'a mut TestAppContext) -> (Entity<TextareaState>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui::init(cx);
            init(cx, |_| {});
            simulate_spell_checker(cx);
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

    fn found(assistance: TextAssistance, cx: &mut VisualTestContext) {
        cx.update(|window, cx| simulate(assistance, window, cx));
        cx.run_until_parked();
    }

    fn predict(offset: usize, text: &str, cx: &mut VisualTestContext) {
        found(TextAssistance { prediction: Some(InlinePrediction { offset, text: text.into() }), misspellings: Vec::new() }, cx);
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
        // The spell checker offers it again: it stays declined, and Tab is the field's own.
        predict(8, "tomorrow ", cx);
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
    fn typing_forgets_what_was_found(cx: &mut TestAppContext) {
        let (state, cx) = area("See you ", cx);
        predict(8, "tomorrow ", cx);
        cx.simulate_input("x");
        cx.run_until_parked();
        // The caret moved past the prediction and the field was checked again: Tab is its own.
        cx.simulate_keystrokes("tab");
        assert!(!value(&state, cx).contains("tomorrow"));
    }

    #[gpui::test]
    fn a_guess_replaces_the_misspelling(cx: &mut TestAppContext) {
        let (state, cx) = area("Helo world", cx);
        found(TextAssistance { prediction: None, misspellings: vec![0..4] }, cx);
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(ReplaceMisspelling { range: 0..4, replacement: "Hello".into() }), cx)
        });
        cx.run_until_parked();
        assert_eq!(value(&state, cx), "Hello world");
    }

    #[test]
    fn continuation_completes_the_word_being_typed() {
        // What macOS 26 answers for "Looking forward to hea": the word as typed first.
        let before = "Looking forward to hea";
        let list = candidates(&[(19..22, "hea "), (19..22, "hearing "), (19..22, "hear ")]);
        assert_eq!(continuation(before, list).as_deref(), Some("ring "));
    }

    #[test]
    fn continuation_predicts_the_next_word_after_a_space() {
        let list = candidates(&[(8..8, "tomorrow "), (8..8, "soon ")]);
        assert_eq!(continuation("See you ", list).as_deref(), Some("tomorrow "));
    }

    #[test]
    fn continuation_skips_candidates_that_replace_the_word() {
        // An emoji for the word, a candidate elsewhere and one in another case add nothing.
        let list = candidates(&[(0..5, "👋 "), (0..3, "Help "), (0..5, "hello ")]);
        assert_eq!(continuation("Hello", list), None);
    }

    fn candidates(list: &[(Range<usize>, &str)]) -> Vec<(Range<usize>, String)> {
        list.iter().map(|(range, text)| (range.clone(), text.to_string())).collect()
    }

    #[test]
    fn word_before_skips_what_is_not_prose() {
        assert_eq!(word_before("hello wrld"), Some("wrld"));
        assert_eq!(word_before("it’s"), Some("it’s"));
        assert_eq!(word_before("user@examp"), None);
        assert_eq!(word_before("https://foo"), None);
        assert_eq!(word_before("v2beta"), None);
        assert_eq!(word_before("NASA"), None);
        assert_eq!(word_before("a"), None);
        assert_eq!(word_before("ends with space "), None);
    }

    #[test]
    fn ends_word_takes_one_separator() {
        assert!(ends_word(" "));
        assert!(ends_word("."));
        assert!(!ends_word("a"));
        assert!(!ends_word(". "));
    }

    #[test]
    fn byte_offset_counts_utf16() {
        let text = "a🙂中";
        assert_eq!(byte_offset(text, 1), Some(1));
        assert_eq!(byte_offset(text, 2), None);
        assert_eq!(byte_offset(text, 3), Some(5));
        assert_eq!(byte_offset(text, 4), Some(8));
        assert_eq!(byte_offset(text, 5), None);
    }
}
