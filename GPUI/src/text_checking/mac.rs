//! The Mac's half of `text_checking`: `NSSpellChecker`, as `NSTextView` uses it. Spelling,
//! guesses, automatic correction and its bubble, and inline predictions (macOS 14+) need only a
//! string and an offset; the bubble also needs the window's view.

use std::cell::Cell;
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Mutex;

use block2::RcBlock;
use futures::channel::oneshot;
use gpui::{Bounds, Pixels, Window};
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSCorrectionIndicatorType, NSCorrectionResponse, NSSpellChecker};
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSOperatingSystemVersion, NSOrthography, NSPoint, NSProcessInfo, NSRange, NSRect,
    NSSize, NSString, NSTextCheckingAllSystemTypes, NSTextCheckingResult, NSTextCheckingType, NSUserDefaults,
};

use super::Settings;

/// The app's own default for checking spelling while typing (on unless turned off); the other two
/// settings are AppKit's, which `NSSpellChecker`'s class properties read, per app over the system's.
const CHECK_SPELLING_WHILE_TYPING: &str = "GPUIContinuousSpellCheckingEnabled";
const CORRECT_SPELLING_AUTOMATICALLY: &str = "NSAutomaticSpellingCorrectionEnabled";
const INLINE_PREDICTIONS: &str = "NSAutomaticInlinePredictionEnabled";

pub fn settings() -> Settings {
    let defaults = NSUserDefaults::standardUserDefaults();
    let check_spelling = NSString::from_str(CHECK_SPELLING_WHILE_TYPING);
    Settings {
        check_spelling_while_typing: defaults.objectForKey(&check_spelling).is_none()
            || defaults.boolForKey(&check_spelling),
        correct_spelling_automatically: NSSpellChecker::isAutomaticSpellingCorrectionEnabled(),
        inline_predictions: inline_predictions_available() && NSSpellChecker::isAutomaticInlinePredictionEnabled(),
    }
}

pub fn set_settings(settings: Settings) {
    let current = self::settings();
    let defaults = NSUserDefaults::standardUserDefaults();
    for (key, was, is) in [
        (CHECK_SPELLING_WHILE_TYPING, current.check_spelling_while_typing, settings.check_spelling_while_typing),
        (CORRECT_SPELLING_AUTOMATICALLY, current.correct_spelling_automatically, settings.correct_spelling_automatically),
        (INLINE_PREDICTIONS, current.inline_predictions, settings.inline_predictions),
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

/// The spell checker's "document": the words ignored in it.
pub struct SpellDocument(isize);

impl SpellDocument {
    pub fn new() -> Self {
        Self(NSSpellChecker::uniqueSpellDocumentTag())
    }
}

impl Drop for SpellDocument {
    fn drop(&mut self) {
        NSSpellChecker::sharedSpellChecker().closeSpellDocumentWithTag(self.0);
    }
}

/// The spell checker's suggestions for the misspelled `word`.
pub fn guesses(word: &str, document: &SpellDocument) -> Vec<String> {
    let word = NSString::from_str(word);
    NSSpellChecker::sharedSpellChecker()
        .guessesForWordRange_inString_language_inSpellDocumentWithTag(
            NSRange::new(0, word.length()),
            &word,
            None,
            document.0,
        )
        .map_or_else(Vec::new, |guesses| guesses.iter().map(|guess| guess.to_string()).collect())
}

/// Stops reporting `word` as misspelled in `document`.
pub fn ignore(word: &str, document: &SpellDocument) {
    NSSpellChecker::sharedSpellChecker().ignoreWord_inSpellDocumentWithTag(&NSString::from_str(word), document.0);
}

/// Adds `word` to the user's spelling dictionary.
pub fn learn(word: &str) {
    NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(word));
}

/// The spell checker's candidates for the word at the end of `before_caret` (its completions)
/// or, after a space, the next word: `(UTF-16 range in before_caret, replacement)`.
pub fn request_candidates(
    before_caret: &str,
    document: &SpellDocument,
) -> oneshot::Receiver<Vec<(Range<usize>, String)>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let completion = RcBlock::new(move |_: isize, results: NonNull<NSArray<NSTextCheckingResult>>| {
        let candidates = unsafe { results.as_ref() }
            .iter()
            .filter_map(|result| {
                let range = result.range();
                let replacement = result.replacementString()?;
                Some((range.location..range.location + range.length, replacement.to_string()))
            })
            .collect();
        if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
            sender.send(candidates).ok();
        }
    });
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
                document.0,
                Some(&completion),
            );
    }
    receiver
}

/// The misspelled words in `text` (UTF-16 ranges).
pub fn request_misspellings(text: &str, document: &SpellDocument) -> oneshot::Receiver<Vec<Range<usize>>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let completion = RcBlock::new(
        move |_: isize, results: NonNull<NSArray<NSTextCheckingResult>>, _: NonNull<NSOrthography>, _: isize| {
            let misspellings = unsafe { results.as_ref() }
                .iter()
                .filter(|result| result.resultType() == NSTextCheckingType::Spelling)
                .map(|result| {
                    let range = result.range();
                    range.location..range.location + range.length
                })
                .collect();
            if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
                sender.send(misspellings).ok();
            }
        },
    );
    let text = NSString::from_str(text);
    unsafe {
        NSSpellChecker::sharedSpellChecker().requestCheckingOfString_range_types_options_inSpellDocumentWithTag_completionHandler(
            &text,
            NSRange::new(0, text.length()),
            NSTextCheckingType::Spelling.0,
            None,
            document.0,
            Some(&completion),
        );
    }
    receiver
}

/// Whether a word the user finishes typing is corrected.
pub fn corrects_automatically() -> bool {
    NSSpellChecker::isAutomaticSpellingCorrectionEnabled()
}

/// The spell checker's correction for the UTF-16 range `word` of `text`, which the user ended by
/// typing `separator`, unless the separator keeps the word as typed.
pub fn correction(text: &str, word: Range<usize>, separator: &str, document: &SpellDocument) -> Option<String> {
    let spell_checker = NSSpellChecker::sharedSpellChecker();
    let language = spell_checker.language();
    if spell_checker.preventsAutocorrectionBeforeString_language(&NSString::from_str(separator), Some(&language)) {
        return None;
    }
    spell_checker
        .correctionForWordRange_inString_language_inSpellDocumentWithTag(
            NSRange::new(word.start, word.len()),
            &NSString::from_str(text),
            &language,
            document.0,
        )
        .map(|correction| correction.to_string())
}

/// Shows the bubble under a correction (`at`, in the window) that offers the word as typed,
/// `original`: the receiver gets what the user chose in it. `None` without the window's view.
pub fn show_correction(window: &Window, at: Bounds<Pixels>, original: &str) -> Option<oneshot::Receiver<String>> {
    let view = crate::webview::host_view(window)?;
    let (sender, receiver) = oneshot::channel();
    let sender = Cell::new(Some(sender));
    let on_choice = RcBlock::new(move |chosen: *mut NSString| {
        if let (Some(chosen), Some(sender)) = (unsafe { chosen.as_ref() }, sender.take()) {
            sender.send(chosen.to_string()).ok();
        }
    });
    // GPUI's bounds are from the content's top left; the view's coordinates from its bottom left.
    let height = view.bounds().size.height;
    let rect = NSRect::new(
        NSPoint::new(f32::from(at.origin.x) as f64, height - f32::from(at.bottom()) as f64),
        NSSize::new(f32::from(at.size.width) as f64, f32::from(at.size.height) as f64),
    );
    NSSpellChecker::sharedSpellChecker()
        .showCorrectionIndicatorOfType_primaryString_alternativeStrings_forStringInRect_view_completionHandler(
            NSCorrectionIndicatorType::Reversion,
            &NSString::from_str(original),
            &NSArray::new(),
            rect,
            &view,
            Some(&on_choice),
        );
    Some(receiver)
}

/// Hides the correction bubble, accepting its correction (typing on does).
pub fn dismiss_correction(window: &Window) {
    if let Some(view) = crate::webview::host_view(window) {
        NSSpellChecker::sharedSpellChecker().dismissCorrectionIndicatorForView(&view);
    }
}

/// Tells the spell checker the user took `correction` back to `original`.
pub fn record_reverted(correction: &str, original: &str, document: &SpellDocument) {
    NSSpellChecker::sharedSpellChecker().recordResponse_toCorrection_forWord_language_inSpellDocumentWithTag(
        NSCorrectionResponse::Reverted,
        &NSString::from_str(correction),
        &NSString::from_str(original),
        None,
        document.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_checking::continuation;

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
        let misspellings = answer(request_misspellings("teh quick brown fox", &SpellDocument::new()));
        assert_eq!(misspellings, vec![0..3]);
    }

    #[test]
    fn the_spell_checker_guesses() {
        assert!(guesses("jumsp", &SpellDocument::new()).iter().any(|guess| guess == "jump"));
    }

    #[test]
    fn the_spell_checker_corrects() {
        let text = "I recieve";
        assert_eq!(correction(text, 2..9, " ", &SpellDocument::new()).as_deref(), Some("receive"));
    }

    #[test]
    #[ignore = "AppKit answers on the main queue: cargo test -- --ignored --test-threads=1"]
    fn the_spell_checker_predicts_a_continuation() {
        if !settings().inline_predictions {
            return;
        }
        let before = "Looking forward to hea";
        let candidates = answer(request_candidates(before, &SpellDocument::new()));
        let rest = continuation(before, candidates).expect("a prediction");
        assert!(rest.starts_with(char::is_alphabetic), "{rest:?}");
    }
}
