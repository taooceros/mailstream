//! A video file played by Apple's `AVPlayer` (MS-398): it reads, decodes, plays the sound and
//! keeps sound and picture together; its pictures come out of an `AVPlayerItemVideoOutput` as
//! BGRA, which GPUI draws using the same [`Frame`] / `RenderImage` path as Linux (`video/linux.rs`).

use std::cell::RefCell;
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification, AVPlayerItemStatus,
    AVPlayerItemVideoOutput, AVPlayerStatus, AVPlayerTimeControlStatus,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{CMTime, CMTimeFlags, kCMTimeZero};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSNotificationCenter, NSNumber, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer,
    NSURL,
};

use super::{Event, Frame};

/// How often the video output and player state are polled while active (~120 Hz).
const POLL_INTERVAL: Duration = Duration::from_micros(8_333);
/// How often the position is reported while playing (matching `video/linux.rs`).
const POSITION_INTERVAL: Duration = Duration::from_millis(100);
/// How long after a paused seek we keep polling for its decoded frame.
const SEEK_WINDOW: Duration = Duration::from_millis(1_500);

struct State {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    frame: Arc<Mutex<Option<Frame>>>,
    events: UnboundedSender<Event>,
    timer: Option<Retained<NSTimer>>,
    want_playing: bool,
    last_playing: Option<bool>,
    duration_sent: bool,
    failed_sent: bool,
    last_position_report: Instant,
    seek_until: Option<Instant>,
    seek_settled: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

pub struct Player {
    state: Rc<RefCell<State>>,
    end_observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Player {
    /// Opens `path`, paused; its pictures go to `frame`, an [`Event::Frame`] announcing each one
    /// the view hasn't taken yet.
    pub fn new(path: &Path, frame: Arc<Mutex<Option<Frame>>>, events: UnboundedSender<Event>) -> Result<Player, String> {
        let mtm = MainThreadMarker::new().ok_or_else(|| "AVPlayer requires the main thread".to_string())?;
        if !path.is_file() {
            return Err(format!("Not a file: {}", path.display()));
        }
        let path_str = path.to_str().ok_or_else(|| format!("Invalid path: {}", path.display()))?;
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);

        let format_key: &NSString = unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const CFString).cast::<NSString>() };
        let format_value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
        let format_obj: &AnyObject = unsafe { &*Retained::as_ptr(&format_value).cast::<AnyObject>() };
        let attrs = NSDictionary::from_slices(&[format_key], &[format_obj]);

        let output = unsafe {
            AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(&attrs))
        };
        unsafe { output.setSuppressesPlayerRendering(true) };

        let item = unsafe { AVPlayerItem::playerItemWithURL(&url, mtm) };
        unsafe { item.addOutput(&output) };

        let player = unsafe { AVPlayer::playerWithPlayerItem(Some(&item), mtm) };
        unsafe { player.setActionAtItemEnd(AVPlayerActionAtItemEnd::Pause) };

        let ended = Arc::new(AtomicBool::new(false));
        let end_flag = ended.clone();
        let end_events = events.clone();
        let end_block = RcBlock::new(move |_: ptr::NonNull<NSNotification>| {
            if !end_flag.swap(true, Ordering::AcqRel) {
                end_events.unbounded_send(Event::Ended).ok();
            }
        });
        let item_obj: &AnyObject = unsafe { &*Retained::as_ptr(&item).cast::<AnyObject>() };
        let end_observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(AVPlayerItemDidPlayToEndTimeNotification),
                Some(item_obj),
                None,
                &end_block,
            )
        };

        let state = Rc::new(RefCell::new(State {
            player,
            item,
            output,
            frame,
            events,
            timer: None,
            want_playing: false,
            last_playing: None,
            duration_sent: false,
            failed_sent: false,
            last_position_report: Instant::now() - POSITION_INTERVAL,
            seek_until: None,
            seek_settled: Arc::new(AtomicBool::new(false)),
            ended,
        }));
        ensure_timer(&state);
        Ok(Player { state, end_observer })
    }

    pub fn play(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.want_playing = true;
            unsafe { state.player.play() };
        }
        ensure_timer(&self.state);
    }

    pub fn pause(&self) {
        let mut state = self.state.borrow_mut();
        state.want_playing = false;
        unsafe { state.player.pause() };
    }

    pub fn seek(&self, to: Duration) {
        {
            let mut state = self.state.borrow_mut();
            let target = if to.is_zero() {
                unsafe { kCMTimeZero }
            } else {
                unsafe { CMTime::with_seconds(to.as_secs_f64(), 600) }
            };
            let zero = unsafe { kCMTimeZero };
            state.ended.store(false, Ordering::Release);
            state.seek_settled.store(false, Ordering::Release);
            state.seek_until = Some(Instant::now() + SEEK_WINDOW);
            state.last_position_report = Instant::now() - POSITION_INTERVAL;
            let settled = state.seek_settled.clone();
            let done = RcBlock::new(move |_finished: Bool| {
                settled.store(true, Ordering::Release);
            });
            unsafe {
                state.player.seekToTime_toleranceBefore_toleranceAfter_completionHandler(target, zero, zero, &done);
            }
        }
        ensure_timer(&self.state);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let observer_obj: &AnyObject = unsafe { &*Retained::as_ptr(&self.end_observer).cast::<AnyObject>() };
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer_obj) };
        let mut state = self.state.borrow_mut();
        if let Some(timer) = state.timer.take() {
            timer.invalidate();
        }
        unsafe {
            state.player.pause();
            state.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}

fn ensure_timer(state_rc: &Rc<RefCell<State>>) {
    if state_rc.borrow().timer.is_some() {
        return;
    }
    let weak = Rc::downgrade(state_rc);
    let block = RcBlock::new(move |timer: ptr::NonNull<NSTimer>| {
        let Some(state_rc) = weak.upgrade() else {
            unsafe { timer.as_ref().invalidate() };
            return;
        };
        tick(&state_rc);
    });
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(POLL_INTERVAL.as_secs_f64(), true, &block) };
    unsafe {
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    state_rc.borrow_mut().timer = Some(timer);
}

fn tick(state_rc: &Rc<RefCell<State>>) {
    let mut state = state_rc.borrow_mut();
    let now = Instant::now();

    let item_status = unsafe { state.item.status() };
    let player_status = unsafe { state.player.status() };
    if item_status == AVPlayerItemStatus::Failed || player_status == AVPlayerStatus::Failed {
        if !state.failed_sent {
            state.failed_sent = true;
            let error = unsafe { state.item.error().or_else(|| state.player.error()) };
            let reason = error
                .map(|err| err.localizedDescription().to_string())
                .unwrap_or_else(|| "Could not play video".to_string());
            state.events.unbounded_send(Event::Failed(reason)).ok();
        }
        if let Some(timer) = state.timer.take() {
            timer.invalidate();
        }
        return;
    }

    if !state.duration_sent && item_status == AVPlayerItemStatus::ReadyToPlay {
        let duration = unsafe { state.item.duration() };
        if let Some(length) = cm_duration(duration) {
            state.events.unbounded_send(Event::Duration(length)).ok();
            state.duration_sent = true;
        }
    }

    let item_time = unsafe { state.item.currentTime() };
    let mut got_frame = false;
    if unsafe { state.output.hasNewPixelBufferForItemTime(item_time) }
        && let Some(pixel_buffer) =
            unsafe { state.output.copyPixelBufferForItemTime_itemTimeForDisplay(item_time, ptr::null_mut()) }
        && let Some(picture) = picture(&pixel_buffer)
    {
        let waiting = state.frame.lock().expect("frame lock").replace(picture).is_some();
        if !waiting {
            state.events.unbounded_send(Event::Frame).ok();
        }
        got_frame = true;
        state.seek_until = None;
    }

    let ended = state.ended.load(Ordering::Acquire);
    match unsafe { state.player.timeControlStatus() } {
        AVPlayerTimeControlStatus::Playing => {
            if state.last_playing != Some(true) {
                state.last_playing = Some(true);
                state.events.unbounded_send(Event::Playing(true)).ok();
            }
        }
        AVPlayerTimeControlStatus::Paused => {
            if !ended && state.last_playing != Some(false) {
                state.last_playing = Some(false);
                state.events.unbounded_send(Event::Playing(false)).ok();
            }
        }
        _ => {}
    }

    if !ended
        && (got_frame && state.seek_until.is_none() && !state.want_playing
            || (state.want_playing && now.duration_since(state.last_position_report) >= POSITION_INTERVAL))
        && let Some(position) = cm_duration(item_time)
    {
        state.events.unbounded_send(Event::Position(position)).ok();
        state.last_position_report = now;
    }

    if let Some(until) = state.seek_until
        && (now >= until || (state.seek_settled.load(Ordering::Acquire) && now + Duration::from_millis(100) >= until))
    {
        state.seek_until = None;
    }

    let keep_polling =
        (state.want_playing && !ended) || !state.duration_sent || state.seek_until.is_some();
    if !keep_polling && let Some(timer) = state.timer.take() {
        timer.invalidate();
    }
}

fn cm_duration(time: CMTime) -> Option<Duration> {
    if !time.flags.contains(CMTimeFlags::Valid) || time.flags.contains(CMTimeFlags::Indefinite) {
        return None;
    }
    let seconds = unsafe { time.seconds() };
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// A pixel buffer's picture, its rows copied out without their padding.
fn picture(buffer: &CVPixelBuffer) -> Option<Frame> {
    if CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA {
        return None;
    }
    let status = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    if status != 0 {
        return None;
    }
    struct Unlock<'a>(&'a CVPixelBuffer);
    impl Drop for Unlock<'_> {
        fn drop(&mut self) {
            unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
        }
    }
    let _unlock = Unlock(buffer);

    let width = u32::try_from(CVPixelBufferGetWidth(buffer)).ok()?;
    let height = u32::try_from(CVPixelBufferGetHeight(buffer)).ok()?;
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let row = width as usize * 4;
    if width == 0 || height == 0 || stride < row {
        return None;
    }
    let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
    if base.is_null() {
        return None;
    }
    // SAFETY: `buffer` is locked for reading; `stride * height` bytes are valid from `base`.
    let data = unsafe { std::slice::from_raw_parts(base, stride * height as usize) };
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&data[y * stride..y * stride + row]);
    }
    Some(Frame { width, height, pixels })
}
