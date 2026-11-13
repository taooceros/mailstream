//! Text checking for the window's focused text input (Mailstream patch, MS-318): spelling
//! underlines, automatic spelling correction and inline predictions (macOS 14+), from
//! `NSSpellChecker` as `NSTextView` gets them. The input draws what is found
//! (`Window::text_assistance`); the view asks again after each key press, click and inserted
//! text, and corrects the word a typed space or punctuation mark ends.

use std::{
    ops::Range,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use block2::RcBlock;
use futures::channel::oneshot;
use gpui::{
    FocusId, ForegroundExecutor, InlinePrediction, Task, TextAssistance, TextCheckingSettings,
    TextInputConfiguration, TextInputStateChange,
};
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSCorrectionIndicatorType, NSCorrectionResponse, NSSpellChecker, NSView};
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSOperatingSystemVersion, NSOrthography, NSPoint,
    NSProcessInfo, NSRange, NSRect, NSSize, NSString, NSTextCheckingAllSystemTypes,
    NSTextCheckingResult, NSTextCheckingType, NSUserDefaults,
};
use parking_lot::Mutex;

use crate::window::{MacWindowState, get_window_state, with_input_handler};

/// How much text around the caret is checked, in UTF-16 code units.
const CHECKED_TEXT: usize = 16_384;
/// How far back from a typed separator the word to correct is looked for.
const WORD_CONTEXT: usize = 128;

/// The app's own default for spell checking while typing; the other two settings are AppKit's
/// (`NSSpellChecker`'s class properties read them, per app over the system's).
const CHECK_SPELLING_WHILE_TYPING: &str = "GPUIContinuousSpellCheckingEnabled";
const CORRECT_SPELLING_AUTOMATICALLY: &str = "NSAutomaticSpellingCorrectionEnabled";
const INLINE_PREDICTIONS: &str = "NSAutomaticInlinePredictionEnabled";

/// A window's text checking.
pub(crate) struct TextChecking {
    executor: ForegroundExecutor,
    configuration: TextInputConfiguration,
    document_tag: Option<isize>,
    on_change: Option<Box<dyn FnMut(FocusId, TextAssistance)>>,
    /// What `on_change` was last handed.
    published: Option<(FocusId, TextAssistance)>,
    /// The prediction the user declined, kept from coming back.
    dismissed: Option<InlinePrediction>,
    check: Option<Task<()>>,
}

impl TextChecking {
    pub(crate) fn new(executor: ForegroundExecutor) -> Self {
        Self {
            executor,
            configuration: TextInputConfiguration::default(),
            document_tag: None,
            on_change: None,
            published: None,
            dismissed: None,
            check: None,
        }
    }

    pub(crate) fn on_change(&mut self, callback: Box<dyn FnMut(FocusId, TextAssistance)>) {
        self.on_change = Some(callback);
    }

    pub(crate) fn set_configuration(&mut self, configuration: TextInputConfiguration) {
        self.configuration = configuration;
    }

    pub(crate) fn dismiss_inline_prediction(&mut self) {
        self.dismissed = self
            .published
            .as_ref()
            .and_then(|(_, assistance)| assistance.prediction.clone());
    }

    /// The spell checker's "document": words ignored in this window.
    fn document_tag(&mut self) -> isize {
        *self
            .document_tag
            .get_or_insert_with(NSSpellChecker::uniqueSpellDocumentTag)
    }
}

impl Drop for TextChecking {
    fn drop(&mut self) {
        if let Some(tag) = self.document_tag {
            NSSpellChecker::sharedSpellChecker().closeSpellDocumentWithTag(tag);
        }
    }
}

pub(crate) fn settings() -> TextCheckingSettings {
    let defaults = NSUserDefaults::standardUserDefaults();
    let check_spelling_key = NSString::from_str(CHECK_SPELLING_WHILE_TYPING);
    TextCheckingSettings {
        check_spelling_while_typing: defaults.objectForKey(&check_spelling_key).is_none()
            || defaults.boolForKey(&check_spelling_key),
        correct_spelling_automatically: NSSpellChecker::isAutomaticSpellingCorrectionEnabled(),
        inline_predictions: inline_predictions_available()
            && NSSpellChecker::isAutomaticInlinePredictionEnabled(),
    }
}

pub(crate) fn set_settings(settings: TextCheckingSettings) {
    let current = self::settings();
    let defaults = NSUserDefaults::standardUserDefaults();
    for (key, was, is) in [
        (
            CHECK_SPELLING_WHILE_TYPING,
            current.check_spelling_while_typing,
            settings.check_spelling_while_typing,
        ),
        (
            CORRECT_SPELLING_AUTOMATICALLY,
            current.correct_spelling_automatically,
            settings.correct_spelling_automatically,
        ),
        (
            INLINE_PREDICTIONS,
            current.inline_predictions,
            settings.inline_predictions,
        ),
    ] {
        if was != is {
            defaults.setBool_forKey(is, &NSString::from_str(key));
        }
    }
}

fn inline_predictions_available() -> bool {
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: 14,
        minorVersion: 0,
        patchVersion: 0,
    })
}

pub(crate) fn input_state_changed(
    window_state: &Rc<Mutex<MacWindowState>>,
    change: TextInputStateChange,
) {
    if change == TextInputStateChange::FocusLost {
        let mut lock = window_state.lock();
        let checking = &mut lock.text_checking;
        checking.check = None;
        checking.published = None;
        checking.dismissed = None;
    } else {
        check_again(window_state);
    }
}

/// Checks the focused input again once the event being handled is done: its text or caret may
/// have changed. A check still running is dropped.
pub(crate) fn check_again(window_state: &Rc<Mutex<MacWindowState>>) {
    let mut lock = window_state.lock();
    let checking = &mut lock.text_checking;
    checking.check = checking
        .configuration
        .suggestions
        .then(|| checking.executor.spawn(check(Rc::downgrade(window_state))));
}

/// After `insertText:`: corrects the word `text` ends, if it is a separator the user typed.
pub(crate) fn did_insert_text(view: &AnyObject, text: &str, was_composing: bool) {
    let window_state = unsafe { get_window_state(view) };
    let spell_checker = NSSpellChecker::sharedSpellChecker();
    // Typing on accepts the last correction.
    spell_checker.dismissCorrectionIndicatorForView(ns_view(view));
    let autocorrect = window_state.lock().text_checking.configuration.autocorrect;
    if autocorrect
        && !was_composing
        && ends_word(text)
        && NSSpellChecker::isAutomaticSpellingCorrectionEnabled()
    {
        correct_word_before(view, &window_state, text);
    }
    check_again(&window_state);
}

pub(crate) fn spelling_guesses(
    window_state: &Rc<Mutex<MacWindowState>>,
    word: &str,
) -> Vec<String> {
    let tag = window_state.lock().text_checking.document_tag();
    guesses(word, tag)
}

fn guesses(word: &str, tag: isize) -> Vec<String> {
    let word = NSString::from_str(word);
    NSSpellChecker::sharedSpellChecker()
        .guessesForWordRange_inString_language_inSpellDocumentWithTag(
            NSRange::new(0, word.length()),
            &word,
            None,
            tag,
        )
        .map_or_else(Vec::new, |guesses| {
            guesses.iter().map(|guess| guess.to_string()).collect()
        })
}

pub(crate) fn ignore_spelling(window_state: &Rc<Mutex<MacWindowState>>, word: &str) {
    let tag = window_state.lock().text_checking.document_tag();
    NSSpellChecker::sharedSpellChecker()
        .ignoreWord_inSpellDocumentWithTag(&NSString::from_str(word), tag);
    check_again(window_state);
}

pub(crate) fn learn_spelling(window_state: &Rc<Mutex<MacWindowState>>, word: &str) {
    NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(word));
    check_again(window_state);
}

/// The focused input's text around the caret.
struct Snapshot {
    focus: FocusId,
    text: String,
    /// Where `text` starts in the input.
    start: usize,
    /// The selection, in `text`.
    selection: Range<usize>,
    composing: bool,
}

impl Snapshot {
    fn read(view: &AnyObject) -> Option<Self> {
        with_input_handler(view, |input| {
            let focus = input.focus_id()?;
            let selection = input.selected_text_range(false)?.range;
            let start = selection.start.saturating_sub(CHECKED_TEXT / 2);
            let mut actual = None;
            let text = input.text_for_range(start..start + CHECKED_TEXT, &mut actual)?;
            let start = actual.map_or(start, |range| range.start);
            Some(Self {
                focus,
                text,
                start,
                selection: selection.start.saturating_sub(start)
                    ..selection.end.saturating_sub(start),
                composing: input.marked_text_range().is_some(),
            })
        })
        .flatten()
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

async fn check(window_state: Weak<Mutex<MacWindowState>>) {
    let Some((view, tag)) = window_state.upgrade().map(|state| {
        let mut lock = state.lock();
        (lock.native_view, lock.text_checking.document_tag())
    }) else {
        return;
    };
    let Some(snapshot) = Snapshot::read(unsafe { view.as_ref() }) else {
        return;
    };
    let settings = settings();

    let prediction = snapshot
        .before_caret()
        .filter(|_| settings.inline_predictions)
        .map(|before_caret| (before_caret, request_candidates(before_caret, tag)));
    let misspellings = settings
        .check_spelling_while_typing
        .then(|| request_misspellings(&snapshot.text, tag));

    let prediction = match prediction {
        Some((before_caret, candidates)) => candidates
            .await
            .ok()
            .and_then(|candidates| continuation(before_caret, candidates))
            .map(|text| InlinePrediction {
                offset: snapshot.start + snapshot.selection.start,
                text,
            }),
        None => None,
    };
    let misspellings = match misspellings {
        Some(misspellings) => misspellings.await.unwrap_or_default(),
        None => Vec::new(),
    };
    // The word being typed isn't misspelled yet.
    let caret = snapshot.selection.end;
    let misspellings = misspellings
        .into_iter()
        .filter(|word| !(word.start..=word.end).contains(&caret))
        .map(|word| snapshot.start + word.start..snapshot.start + word.end)
        .collect();

    if let Some(window_state) = window_state.upgrade() {
        publish(
            &window_state,
            snapshot.focus,
            TextAssistance {
                prediction,
                misspellings,
            },
        );
    }
}

fn publish(
    window_state: &Rc<Mutex<MacWindowState>>,
    focus: FocusId,
    mut assistance: TextAssistance,
) {
    let mut lock = window_state.lock();
    let checking = &mut lock.text_checking;
    if assistance.prediction.is_some() && assistance.prediction == checking.dismissed {
        assistance.prediction = None;
    }
    if checking
        .published
        .as_ref()
        .is_some_and(|(published_focus, published)| {
            *published_focus == focus && *published == assistance
        })
    {
        return;
    }
    checking.published = Some((focus, assistance.clone()));
    let Some(mut on_change) = checking.on_change.take() else {
        return;
    };
    drop(lock);
    on_change(focus, assistance);
    window_state
        .lock()
        .text_checking
        .on_change
        .get_or_insert(on_change);
}

/// The spell checker's candidates for the word at the end of `before_caret` (its completions)
/// or, after a space, the next word: `(range in before_caret, replacement)`.
fn request_candidates(
    before_caret: &str,
    tag: isize,
) -> oneshot::Receiver<Vec<(Range<usize>, String)>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let completion = RcBlock::new(
        move |_: isize, results: NonNull<NSArray<NSTextCheckingResult>>| {
            let candidates = unsafe { results.as_ref() }
                .iter()
                .filter_map(|result| {
                    let range = result.range();
                    let replacement = result.replacementString()?;
                    Some((
                        range.location..range.location + range.length,
                        replacement.to_string(),
                    ))
                })
                .collect();
            if let Some(sender) = sender.lock().take() {
                sender.send(candidates).ok();
            }
        },
    );
    let text = NSString::from_str(before_caret);
    // NSTextCheckingGenerateInlinePredictionsKey (macOS 14), by value: the symbol would keep the
    // binary from loading on older systems.
    let key = NSString::from_str("GenerateInlinePredictions");
    let yes = NSNumber::new_bool(true);
    let options = NSDictionary::from_slices(&[&*key], &[yes.as_ref() as &AnyObject]);
    unsafe {
        NSSpellChecker::sharedSpellChecker()
            .requestCandidatesForSelectedRange_inString_types_options_inSpellDocumentWithTag_completionHandler(
                NSRange::new(text.length(), 0),
                &text,
                NSTextCheckingAllSystemTypes,
                Some(&options),
                tag,
                Some(&completion),
            );
    }
    receiver
}

/// The misspelled words in `text`.
fn request_misspellings(text: &str, tag: isize) -> oneshot::Receiver<Vec<Range<usize>>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let completion = RcBlock::new(
        move |_: isize,
              results: NonNull<NSArray<NSTextCheckingResult>>,
              _: NonNull<NSOrthography>,
              _: isize| {
            let misspellings = unsafe { results.as_ref() }
                .iter()
                .filter(|result| result.resultType() == NSTextCheckingType::Spelling)
                .map(|result| {
                    let range = result.range();
                    range.location..range.location + range.length
                })
                .collect();
            if let Some(sender) = sender.lock().take() {
                sender.send(misspellings).ok();
            }
        },
    );
    let text = NSString::from_str(text);
    unsafe {
        NSSpellChecker::sharedSpellChecker()
            .requestCheckingOfString_range_types_options_inSpellDocumentWithTag_completionHandler(
                &text,
                NSRange::new(0, text.length()),
                NSTextCheckingType::Spelling.0,
                None,
                tag,
                Some(&completion),
            );
    }
    receiver
}

/// Replaces the word before the separator just typed with the spell checker's correction and
/// shows the correction's bubble, which takes it back when clicked.
fn correct_word_before(
    view: &AnyObject,
    window_state: &Rc<Mutex<MacWindowState>>,
    separator: &str,
) {
    let separator_len = utf16_len(separator);
    // The text before the separator, and where it starts.
    let Some((focus, start, before)) = with_input_handler(view, |input| {
        let caret = input.selected_text_range(false)?.range;
        let end = caret
            .start
            .checked_sub(separator_len)
            .filter(|_| caret.is_empty())?;
        let mut actual = None;
        let before = input.text_for_range(end.saturating_sub(WORD_CONTEXT)..end, &mut actual)?;
        let start = actual.map_or(end.saturating_sub(WORD_CONTEXT), |range| range.start);
        Some((input.focus_id()?, start, before))
    })
    .flatten() else {
        return;
    };
    let Some(original) = word_before(&before) else {
        return;
    };
    let word = utf16_len(&before) - utf16_len(original)..utf16_len(&before);
    let spell_checker = NSSpellChecker::sharedSpellChecker();
    let language = spell_checker.language();
    if spell_checker.preventsAutocorrectionBeforeString_language(
        &NSString::from_str(separator),
        Some(&language),
    ) {
        return;
    }
    let tag = window_state.lock().text_checking.document_tag();
    let Some(correction) = spell_checker
        .correctionForWordRange_inString_language_inSpellDocumentWithTag(
            NSRange::new(word.start, word.len()),
            &NSString::from_str(&before),
            &language,
            tag,
        )
        .map(|correction| correction.to_string())
        .filter(|correction| correction != original)
    else {
        return;
    };

    let typed = start + word.start..start + word.end;
    let corrected = typed.start..typed.start + utf16_len(&correction);
    let caret = corrected.end + separator_len;
    let Some(bounds) = with_input_handler(view, |input| {
        input.replace_text_in_range(Some(typed), &correction);
        input.set_selected_text_range(caret..caret);
        input.bounds_for_range(corrected.clone())
    })
    .flatten() else {
        return;
    };

    // The bubble offers the original word; choosing it reverts the correction.
    let reverted_to = original.to_owned();
    let weak_state = Rc::downgrade(window_state);
    let on_choice = RcBlock::new(move |chosen: *mut NSString| {
        let Some(chosen) = (unsafe { chosen.as_ref() }) else {
            return;
        };
        if chosen.to_string() != reverted_to {
            return;
        }
        let Some(window_state) = weak_state.upgrade() else {
            return;
        };
        let view = window_state.lock().native_view;
        let reverted = with_input_handler(unsafe { view.as_ref() }, |input| {
            let mut actual = None;
            if input.focus_id() != Some(focus)
                || input
                    .text_for_range(corrected.clone(), &mut actual)
                    .as_deref()
                    != Some(correction.as_str())
            {
                return false;
            }
            let caret = input
                .selected_text_range(false)
                .map(|selection| selection.range);
            input.replace_text_in_range(Some(corrected.clone()), &reverted_to);
            if let Some(caret) = caret.filter(|caret| caret.start >= corrected.end) {
                let caret = caret.start - corrected.len() + utf16_len(&reverted_to);
                input.set_selected_text_range(caret..caret);
            }
            true
        })
        .unwrap_or(false);
        if reverted {
            NSSpellChecker::sharedSpellChecker()
                .recordResponse_toCorrection_forWord_language_inSpellDocumentWithTag(
                    NSCorrectionResponse::Reverted,
                    &NSString::from_str(&correction),
                    &NSString::from_str(&reverted_to),
                    None,
                    tag,
                );
            check_again(&window_state);
        }
    });
    // GPUI's bounds are from the content's top left; the view's coordinates from its bottom left.
    let height = window_state.lock().content_size().height.as_f32() as f64;
    let rect = NSRect::new(
        NSPoint::new(
            bounds.origin.x.as_f32() as f64,
            height - bounds.bottom().as_f32() as f64,
        ),
        NSSize::new(
            bounds.size.width.as_f32() as f64,
            bounds.size.height.as_f32() as f64,
        ),
    );
    spell_checker
        .showCorrectionIndicatorOfType_primaryString_alternativeStrings_forStringInRect_view_completionHandler(
            NSCorrectionIndicatorType::Reversion,
            &NSString::from_str(original),
            &NSArray::new(),
            rect,
            ns_view(view),
            Some(&on_choice),
        );
}

fn ns_view(view: &AnyObject) -> &NSView {
    unsafe { &*(view as *const AnyObject as *const NSView) }
}

/// Whether typing `text` ends a word.
fn ends_word(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (
            Some(' ' | '\t' | '\n' | '.' | ',' | '!' | '?' | ';' | ':' | ')'),
            None
        )
    )
}

/// The word `text` ends with, unless it's too short to correct, an acronym, or part of an
/// address, path, number or identifier.
fn word_before(text: &str) -> Option<&str> {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic() || matches!(c, '\'' | '’'))
        .last()?
        .0;
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
fn continuation(
    before_caret: &str,
    candidates: impl IntoIterator<Item = (Range<usize>, String)>,
) -> Option<String> {
    let end = utf16_len(before_caret);
    candidates.into_iter().find_map(|(range, replacement)| {
        if range.end != end {
            return None;
        }
        let typed = &before_caret[byte_offset(before_caret, range.start)?..];
        let rest = replacement.strip_prefix(typed)?;
        rest.starts_with(char::is_alphanumeric)
            .then(|| rest.to_owned())
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

    fn candidates(list: &[(Range<usize>, &str)]) -> Vec<(Range<usize>, String)> {
        list.iter()
            .map(|(range, text)| (range.clone(), text.to_string()))
            .collect()
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

    /// Waits for the system's spell checker to answer, running this thread's run loop: AppKit
    /// answers some requests on the main queue.
    fn answer<T>(mut receiver: oneshot::Receiver<T>) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(answer)) = receiver.try_recv() {
                return answer;
            }
            let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
        }
        panic!("the spell checker didn't answer");
    }

    #[test]
    fn the_spell_checker_finds_misspellings() {
        let tag = NSSpellChecker::uniqueSpellDocumentTag();
        let misspellings = answer(request_misspellings("teh quick brown fox", tag));
        assert_eq!(misspellings, vec![0..3]);
        NSSpellChecker::sharedSpellChecker().closeSpellDocumentWithTag(tag);
    }

    #[test]
    fn the_spell_checker_guesses() {
        let tag = NSSpellChecker::uniqueSpellDocumentTag();
        assert!(guesses("jumsp", tag).iter().any(|guess| guess == "jump"));
        NSSpellChecker::sharedSpellChecker().closeSpellDocumentWithTag(tag);
    }

    #[test]
    #[ignore = "AppKit answers on the main queue: cargo test -- --ignored --test-threads=1"]
    fn the_spell_checker_predicts_a_continuation() {
        if !(inline_predictions_available() && NSSpellChecker::isAutomaticInlinePredictionEnabled())
        {
            return;
        }
        let tag = NSSpellChecker::uniqueSpellDocumentTag();
        let before = "Looking forward to hea";
        let candidates = answer(request_candidates(before, tag));
        let rest = continuation(before, candidates).expect("a prediction");
        assert!(rest.starts_with(char::is_alphabetic), "{rest:?}");
        NSSpellChecker::sharedSpellChecker().closeSpellDocumentWithTag(tag);
    }
}
