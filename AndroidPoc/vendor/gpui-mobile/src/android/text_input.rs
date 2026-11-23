//! UI-thread IME events are queued, then applied on GPUI's native thread.

use gpui::{
    DispatchEventResult, KeyDownEvent, Keystroke, Modifiers, PlatformInput, PlatformInputHandler,
};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    collections::VecDeque,
    ops::Range,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

pub(super) const IME_COMPOSING: i32 = 0;
pub(super) const IME_COMMIT: i32 = 1;
pub(super) const IME_DELETE_SURROUNDING: i32 = 2;
pub(super) const IME_DELETE_CODE_POINTS: i32 = 3;
pub(super) const IME_ESCAPE: i32 = 4;
pub(super) const IME_FINISH_COMPOSING: i32 = 5;
pub(super) const IME_KEY_ENTER: i32 = 6;
pub(super) const IME_KEY_BACKSPACE: i32 = 7;
pub(super) const IME_KEY_FORWARD_DELETE: i32 = 8;
pub(super) const IME_KEY_TAB: i32 = 9;
pub(super) const IME_KEY_NAV: i32 = 10;
pub(super) const IME_SET_SELECTION: i32 = 11;
pub(super) const IME_SET_COMPOSING_REGION: i32 = 12;

/// Maximum UTF-16 code units mirrored to Java's `BaseInputConnection` `Editable`.
const MAX_MIRROR_UTF16: usize = 4096;

static SESSION: AtomicU64 = AtomicU64::new(0);
static COMPOSING: AtomicBool = AtomicBool::new(false);
static INPUT_SYNC_NEEDED: AtomicBool = AtomicBool::new(false);
static WINDOW_BASE_UTF16: AtomicUsize = AtomicUsize::new(0);
static EVENTS: Mutex<VecDeque<ImeEvent>> = Mutex::new(VecDeque::new());
static BACK_PENDING: AtomicBool = AtomicBool::new(false);
static PREDICTIVE_BACK_ACTIVE: AtomicBool = AtomicBool::new(false);
static PREDICTIVE_BACK_EVENTS: Mutex<VecDeque<PredictiveBackEvent>> = Mutex::new(VecDeque::new());
static IME_INSETS_DIRTY: AtomicBool = AtomicBool::new(false);

thread_local! {
    static IME_INSETS_CALLBACK: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static PREDICTIVE_BACK_CALLBACK: RefCell<Option<Box<dyn Fn(PredictiveBackEvent)>>> =
        const { RefCell::new(None) };
}
/// Android 14+ `OnBackAnimationCallback` lifecycle event queued from the Java UI thread
/// and drained on GPUI's native thread on the next vsync (MS-449).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PredictiveBackEvent {
    Started { progress: f32, edge: i32 },
    Progressed { progress: f32, edge: i32 },
    Invoked,
    Cancelled,
}

pub(super) struct ImeEvent {
    pub session: u64,
    pub kind: i32,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Windowed snapshot of the focused input field pushed to `GpuiInputActivity`'s mirror `Editable`
/// so Android IMEs receive accurate `getTextBeforeCursor` / `getSurroundingText` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InputSnapshot {
    pub base_utf16: usize,
    pub text: String,
    pub sel_start: i32,
    pub sel_end: i32,
    pub composing_start: i32,
    pub composing_end: i32,
}

pub(super) fn new_session() -> u64 {
    let session = SESSION.fetch_add(1, Ordering::AcqRel) + 1;
    EVENTS.lock().clear();
    COMPOSING.store(false, Ordering::Release);
    WINDOW_BASE_UTF16.store(0, Ordering::Release);
    session
}

pub(super) fn current_session() -> u64 {
    SESSION.load(Ordering::Acquire)
}

/// Requests pushing the focused input's text/selection snapshot to Java once the
/// current GPUI draw/dispatch pass releases its `App`/`Window` borrows.
pub(super) fn mark_input_sync_needed() {
    INPUT_SYNC_NEEDED.store(true, Ordering::Release);
}

pub(super) fn clear_input_sync_needed() {
    INPUT_SYNC_NEEDED.store(false, Ordering::Release);
}

pub(super) fn enqueue(event: ImeEvent) {
    if event.session == SESSION.load(Ordering::Acquire) {
        EVENTS.lock().push_back(event);
        crate::mark_text_input_dirty();
    }
}

/// Registers a main-thread callback invoked on the next vsync when Android's
/// `WindowInsetsAnimation.Callback` updates IME height (without triggering a
/// full `window.refresh()` or `request_config_refresh()`).
pub fn set_ime_insets_callback(callback: Option<Box<dyn Fn()>>) {
    IME_INSETS_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

/// Marks IME animation insets dirty and schedules the next hardware vsync frame.
pub(super) fn mark_ime_insets_dirty() {
    IME_INSETS_DIRTY.store(true, Ordering::Release);
    super::frame_source::schedule_frame();
}

/// Registers a main-thread callback invoked for Android 14+ predictive back
/// gesture events (`STARTED` / `PROGRESSED` / `INVOKED` / `CANCELLED`).
pub fn set_predictive_back_callback(callback: Option<Box<dyn Fn(PredictiveBackEvent)>>) {
    PREDICTIVE_BACK_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

/// Enqueues a predictive back gesture event from the Java UI thread.
/// Coalesces consecutive `Progressed` events so each vsync applies the latest progress.
pub(super) fn enqueue_predictive_back(event: PredictiveBackEvent) {
    match event {
        PredictiveBackEvent::Started { .. } | PredictiveBackEvent::Progressed { .. } => {
            PREDICTIVE_BACK_ACTIVE.store(true, Ordering::Release);
        }
        PredictiveBackEvent::Invoked | PredictiveBackEvent::Cancelled => {
            PREDICTIVE_BACK_ACTIVE.store(false, Ordering::Release);
        }
    }
    let mut queue = PREDICTIVE_BACK_EVENTS.lock();
    if matches!(event, PredictiveBackEvent::Progressed { .. })
        && matches!(queue.back(), Some(PredictiveBackEvent::Progressed { .. }))
    {
        if let Some(last) = queue.back_mut() {
            *last = event;
        }
    } else {
        queue.push_back(event);
    }
    drop(queue);
    super::frame_source::schedule_frame();
}

/// Enqueues a system back event from the Java UI thread (`OnBackInvokedCallback` / `onBackPressed`).
/// When an Android 14+ predictive back gesture is active, routes `Invoked` into the predictive
/// back queue (guaranteeing commit even if progress < 1/3); otherwise dispatches an `escape`
/// keystroke on the native loop thread.
pub(super) fn enqueue_back() {
    if PREDICTIVE_BACK_ACTIVE.swap(false, Ordering::AcqRel) {
        PREDICTIVE_BACK_EVENTS
            .lock()
            .push_back(PredictiveBackEvent::Invoked);
        super::frame_source::schedule_frame();
    } else {
        BACK_PENDING.store(true, Ordering::Release);
        crate::mark_text_input_dirty();
    }
}

pub(super) fn drain(
    slot: &Rc<RefCell<Option<PlatformInputHandler>>>,
    input: &mut dyn FnMut(PlatformInput) -> DispatchEventResult,
) {
    if IME_INSETS_DIRTY.swap(false, Ordering::AcqRel) {
        let cb = IME_INSETS_CALLBACK.with(|slot| slot.borrow_mut().take());
        if let Some(cb) = cb {
            cb();
            IME_INSETS_CALLBACK.with(|slot| {
                let mut guard = slot.borrow_mut();
                if guard.is_none() {
                    *guard = Some(cb);
                }
            });
        }
    }
    let back_events = std::mem::take(&mut *PREDICTIVE_BACK_EVENTS.lock());
    if !back_events.is_empty() {
        let cb = PREDICTIVE_BACK_CALLBACK.with(|slot| slot.borrow_mut().take());
        if let Some(cb) = cb {
            for event in back_events {
                if matches!(
                    event,
                    PredictiveBackEvent::Started { .. } | PredictiveBackEvent::Invoked
                ) {
                    finish_composition(slot);
                }
                cb(event);
            }
            PREDICTIVE_BACK_CALLBACK.with(|slot| {
                let mut guard = slot.borrow_mut();
                if guard.is_none() {
                    *guard = Some(cb);
                }
            });
        } else {
            for event in back_events {
                if matches!(event, PredictiveBackEvent::Invoked) {
                    finish_composition(slot);
                    input(PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: Keystroke {
                            key: "escape".into(),
                            key_char: None,
                            modifiers: Modifiers::default(),
                        },
                        is_held: false,
                        prefer_character_input: false,
                    }));
                }
            }
        }
    }
    if BACK_PENDING.swap(false, Ordering::AcqRel) {
        finish_composition(slot);
        input(PlatformInput::KeyDown(KeyDownEvent {
            keystroke: Keystroke {
                key: "escape".into(),
                key_char: None,
                modifiers: Modifiers::default(),
            },
            is_held: false,
            prefer_character_input: false,
        }));
    }
    let events = std::mem::take(&mut *EVENTS.lock());
    let mut synced_change = false;
    for event in events {
        if event.session != SESSION.load(Ordering::Acquire) {
            continue;
        }
        match event.kind {
            IME_ESCAPE => {
                finish_composition(slot);
                dispatch_key(input, "escape", Modifiers::default());
                synced_change = true;
                continue;
            }
            IME_KEY_ENTER => {
                finish_composition(slot);
                dispatch_key(input, "enter", Modifiers::default());
                synced_change = true;
                continue;
            }
            IME_KEY_BACKSPACE => {
                if COMPOSING.swap(false, Ordering::AcqRel) {
                    if let Some(mut handler) = slot.borrow_mut().take() {
                        handler.unmark_text();
                        let mut current = slot.borrow_mut();
                        if current.is_none() {
                            *current = Some(handler);
                        }
                    }
                }
                dispatch_key(input, "backspace", Modifiers::default());
                synced_change = true;
                continue;
            }
            IME_KEY_FORWARD_DELETE => {
                dispatch_key(input, "delete", Modifiers::default());
                synced_change = true;
                continue;
            }
            IME_KEY_TAB => {
                finish_composition(slot);
                let modifiers = Modifiers {
                    shift: event.start != 0,
                    ..Modifiers::default()
                };
                dispatch_key(input, "tab", modifiers);
                synced_change = true;
                continue;
            }
            IME_KEY_NAV => {
                finish_composition(slot);
                if let Some(key) = nav_key_name(event.start) {
                    dispatch_key(input, key, Modifiers::default());
                    synced_change = true;
                }
                continue;
            }
            _ => {}
        }
        // No RefCell borrow spans a GPUI update (which may replace the handler).
        let Some(mut handler) = slot.borrow_mut().take() else {
            forward_legacy_text(event.kind, &event.text, event.start, |text| {
                crate::dispatch_text_input(text);
            });
            continue;
        };
        match event.kind {
            IME_COMPOSING => {
                COMPOSING.store(!event.text.is_empty(), Ordering::Release);
                handler.replace_and_mark_text_in_range(
                    None,
                    &event.text,
                    Some(event.start..event.end),
                );
            }
            IME_COMMIT => {
                COMPOSING.store(false, Ordering::Release);
                handler.replace_text_in_range(None, &event.text);
                handler.unmark_text();
            }
            IME_FINISH_COMPOSING => {
                // Android `finishComposingText()` accepts the active composing text
                // as-is and removes only the composing span; never replace with "".
                COMPOSING.store(false, Ordering::Release);
                handler.unmark_text();
            }
            IME_DELETE_SURROUNDING | IME_DELETE_CODE_POINTS => {
                if let Some(selection) = handler.selected_text_range(false) {
                    let mut adjusted = None;
                    if let Some(text) = handler.text_for_range(0..usize::MAX, &mut adjusted) {
                        let ranges = deletion_ranges(
                            &text,
                            selection.range,
                            event.start,
                            event.end,
                            event.kind == IME_DELETE_CODE_POINTS,
                        );
                        // Ranges are ordered back-to-front so earlier offsets stay valid.
                        for range in ranges {
                            if !range.is_empty() {
                                handler.replace_text_in_range(Some(range), "");
                            }
                        }
                    }
                }
            }
            IME_SET_SELECTION => {
                let base = WINDOW_BASE_UTF16.load(Ordering::Acquire);
                let start = base.saturating_add(event.start);
                let end = base.saturating_add(event.end);
                let range = start.min(end)..start.max(end);
                handler.set_selected_text_range(range);
            }
            IME_SET_COMPOSING_REGION => {
                let base = WINDOW_BASE_UTF16.load(Ordering::Acquire);
                let start = base.saturating_add(event.start);
                let end = base.saturating_add(event.end);
                let range = start.min(end)..start.max(end);
                if range.is_empty() {
                    COMPOSING.store(false, Ordering::Release);
                    handler.unmark_text();
                } else {
                    let mut adjusted = None;
                    if let Some(text) = handler.text_for_range(range.clone(), &mut adjusted) {
                        let actual = adjusted.unwrap_or(range);
                        let len_utf16 = text.encode_utf16().count();
                        COMPOSING.store(len_utf16 > 0, Ordering::Release);
                        handler.replace_and_mark_text_in_range(
                            Some(actual),
                            &text,
                            Some(len_utf16..len_utf16),
                        );
                    }
                }
            }
            _ => {}
        }
        let mut current = slot.borrow_mut();
        if current.is_none() {
            *current = Some(handler);
        }
    }
    if synced_change {
        mark_input_sync_needed();
    }
}

fn dispatch_key(
    input: &mut dyn FnMut(PlatformInput) -> DispatchEventResult,
    key: &str,
    modifiers: Modifiers,
) {
    input(PlatformInput::KeyDown(KeyDownEvent {
        keystroke: Keystroke {
            key: key.into(),
            key_char: None,
            modifiers,
        },
        is_held: false,
        prefer_character_input: false,
    }));
}

fn nav_key_name(code: usize) -> Option<&'static str> {
    match code {
        0 => Some("left"),
        1 => Some("right"),
        2 => Some("up"),
        3 => Some("down"),
        4 => Some("home"),
        5 => Some("end"),
        _ => None,
    }
}

/// Queries the focused `PlatformInputHandler` outside any active `App`/`Window` borrow
/// and pushes its text/selection/composing window to `GpuiInputActivity` when requested.
pub(super) fn sync_input_state_if_needed(slot: &Rc<RefCell<Option<PlatformInputHandler>>>) {
    if !INPUT_SYNC_NEEDED.swap(false, Ordering::AcqRel) {
        return;
    }
    let session = current_session();
    if session == 0 {
        return;
    }
    let Some(mut handler) = slot.borrow_mut().take() else {
        return;
    };
    let selection = handler
        .selected_text_range(false)
        .map(|s| s.range)
        .unwrap_or(0..0);
    let marked = handler.marked_text_range();
    let mut adjusted = None;
    let text = handler
        .text_for_range(0..usize::MAX, &mut adjusted)
        .unwrap_or_default();
    let mut current = slot.borrow_mut();
    if current.is_none() {
        *current = Some(handler);
    }
    drop(current);

    let snapshot = build_input_snapshot(&text, selection, marked, MAX_MIRROR_UTF16);
    WINDOW_BASE_UTF16.store(snapshot.base_utf16, Ordering::Release);
    super::jni::update_input_state_android(session, &snapshot);
}

/// Builds a UTF-16-safe windowed snapshot centered around `selection` so very large
/// documents never ship megabytes over JNI while short fields stay at `base_utf16 = 0`.
pub(super) fn build_input_snapshot(
    full_text: &str,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    max_utf16: usize,
) -> InputSnapshot {
    let mut boundaries = Vec::with_capacity(full_text.len() + 1);
    let mut byte_offsets = Vec::with_capacity(full_text.len() + 1);
    boundaries.push(0usize);
    byte_offsets.push(0usize);
    for (byte_idx, ch) in full_text.char_indices() {
        boundaries.push(boundaries.last().unwrap() + ch.len_utf16());
        byte_offsets.push(byte_idx + ch.len_utf8());
    }
    let total_utf16 = *boundaries.last().unwrap();
    let sel_start = selection.start.min(total_utf16);
    let sel_end = selection.end.min(total_utf16);
    let (sel_start, sel_end) = (sel_start.min(sel_end), sel_start.max(sel_end));

    let (win_start_idx, win_end_idx) = if total_utf16 <= max_utf16 {
        (0, boundaries.len() - 1)
    } else {
        let half = max_utf16 / 2;
        let desired_start = sel_start.saturating_sub(half);
        let desired_end = desired_start.saturating_add(max_utf16).min(total_utf16);
        let desired_start = desired_end.saturating_sub(max_utf16);
        let start_idx = boundaries.partition_point(|&o| o < desired_start);
        let end_idx = boundaries
            .partition_point(|&o| o <= desired_end)
            .saturating_sub(1)
            .max(start_idx);
        (start_idx, end_idx)
    };

    let base_utf16 = boundaries[win_start_idx];
    let win_len_utf16 = boundaries[win_end_idx] - base_utf16;
    let slice = &full_text[byte_offsets[win_start_idx]..byte_offsets[win_end_idx]];

    let rel_sel_start = sel_start.saturating_sub(base_utf16).min(win_len_utf16) as i32;
    let rel_sel_end = sel_end.saturating_sub(base_utf16).min(win_len_utf16) as i32;

    let (composing_start, composing_end) = match marked {
        Some(range) if range.start < range.end => {
            let cs = range.start.min(total_utf16);
            let ce = range.end.min(total_utf16);
            if ce <= base_utf16 || cs >= base_utf16 + win_len_utf16 {
                (-1, -1)
            } else {
                (
                    cs.saturating_sub(base_utf16).min(win_len_utf16) as i32,
                    ce.saturating_sub(base_utf16).min(win_len_utf16) as i32,
                )
            }
        }
        _ => (-1, -1),
    };

    InputSnapshot {
        base_utf16,
        text: slice.to_owned(),
        sel_start: rel_sel_start,
        sel_end: rel_sel_end,
        composing_start,
        composing_end,
    }
}

pub(super) fn finish_composition(slot: &Rc<RefCell<Option<PlatformInputHandler>>>) {
    if !COMPOSING.swap(false, Ordering::AcqRel) {
        return;
    }
    let handler = slot.borrow_mut().take();
    if let Some(mut handler) = handler {
        handler.unmark_text();
        let mut current = slot.borrow_mut();
        if current.is_none() {
            *current = Some(handler);
        }
    }
    super::jni::reset_keyboard_composition();
}

/// Keep the original TextInput callback contract for clients without a platform
/// input handler. Its backspace sentinel deletes one character per callback;
/// exact UTF-16 ranges and forward deletion require PlatformInputHandler.
fn forward_legacy_text(kind: i32, text: &str, before: usize, mut dispatch: impl FnMut(&str)) {
    match kind {
        IME_COMMIT => dispatch(text),
        IME_DELETE_SURROUNDING | IME_DELETE_CODE_POINTS => {
            for _ in 0..before {
                dispatch("\x08");
            }
        }
        _ => {}
    }
}

/// Android offers deletion in either UTF-16 units or Unicode code points.
/// Per Android's `InputConnection.deleteSurroundingText` contract, deletions
/// remove `before` units before `selection.start` and `after` units after
/// `selection.end` without removing the selection itself. UTF-16 endpoints are
/// rounded outwards so a surrogate pair is never split. Returned ranges are
/// ordered back-to-front so applying them sequentially preserves offsets.
fn deletion_ranges(
    text: &str,
    selection: Range<usize>,
    before: usize,
    after: usize,
    code_points: bool,
) -> Vec<Range<usize>> {
    let mut boundaries = vec![0];
    for ch in text.chars() {
        boundaries.push(boundaries.last().unwrap() + ch.len_utf16());
    }
    let length = *boundaries.last().unwrap();
    let sel_start = selection.start.min(length);
    let sel_end = selection.end.min(length);
    let (sel_start, sel_end) = (sel_start.min(sel_end), sel_start.max(sel_end));

    let (del_start, del_end) = if code_points {
        let start_index = boundaries.partition_point(|offset| *offset < sel_start);
        let end_index = boundaries.partition_point(|offset| *offset < sel_end);
        (
            boundaries[start_index.saturating_sub(before)],
            boundaries[end_index.saturating_add(after).min(boundaries.len() - 1)],
        )
    } else {
        let start = sel_start.saturating_sub(before);
        let end = sel_end.saturating_add(after).min(length);
        let start_index = boundaries
            .partition_point(|offset| *offset <= start)
            .saturating_sub(1);
        let end_index = boundaries.partition_point(|offset| *offset < end);
        (boundaries[start_index], boundaries[end_index])
    };

    if sel_start == sel_end {
        if del_start < del_end {
            vec![del_start..del_end]
        } else {
            Vec::new()
        }
    } else {
        let mut ranges = Vec::with_capacity(2);
        if del_end > sel_end {
            ranges.push(sel_end..del_end);
        }
        if del_start < sel_start {
            ranges.push(del_start..sel_start);
        }
        ranges
    }
}

#[cfg(test)]
mod tests {
    use super::{build_input_snapshot, deletion_ranges, forward_legacy_text, nav_key_name};

    #[test]
    fn legacy_inputs_receive_commits_and_backspace_sentinels() {
        let mut received = Vec::new();
        for (kind, text, before) in [(0, "ni", 0), (1, "你", 0), (2, "", 1), (3, "", 2), (5, "", 0)]
        {
            forward_legacy_text(kind, text, before, |text| received.push(text.to_owned()));
        }
        assert_eq!(received, vec!["你", "\x08", "\x08", "\x08"]);
    }

    #[test]
    fn deletion_preserves_surrogate_pairs_and_selection() {
        assert_eq!(deletion_ranges("你😀好", 3..3, 1, 0, false), vec![1..3]);
        assert_eq!(deletion_ranges("你😀好", 1..1, 0, 1, false), vec![1..3]);
        assert_eq!(deletion_ranges("你😀好", 3..3, 2, 0, true), vec![0..3]);
        assert_eq!(deletion_ranges("你😀好", 1..1, 0, 2, true), vec![1..4]);
        assert_eq!(deletion_ranges("你😀好", 0..0, 99, 99, true), vec![0..4]);
        // Non-empty selection: delete before & after selection without wiping selection itself.
        assert_eq!(
            deletion_ranges("abcdef", 2..4, 1, 1, false),
            vec![4..5, 1..2]
        );
    }

    #[test]
    fn snapshot_maps_utf16_selection_and_composing_and_clamps_long_text() {
        let snap = build_input_snapshot("你😀好", 3..3, Some(1..3), 64);
        assert_eq!(snap.base_utf16, 0);
        assert_eq!(snap.text, "你😀好");
        assert_eq!((snap.sel_start, snap.sel_end), (3, 3));
        assert_eq!((snap.composing_start, snap.composing_end), (1, 3));

        let long = "a".repeat(20) + "😀" + &"b".repeat(20);
        let snap = build_input_snapshot(&long, 21..21, None, 10);
        assert!(snap.base_utf16 > 0);
        assert!(snap.text.encode_utf16().count() <= 10);
        assert_eq!(snap.sel_start, snap.sel_end);
        assert_eq!(snap.composing_start, -1);
        assert_eq!(nav_key_name(0), Some("left"));
        assert_eq!(nav_key_name(5), Some("end"));
        assert_eq!(nav_key_name(99), None);
    }
}
