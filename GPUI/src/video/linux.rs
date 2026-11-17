//! A video file played by GStreamer's player, GstPlay (MS-221): it reads, decodes, plays the sound
//! and keeps sound and picture together; its pictures come out of an app sink as BGRA, which GPUI
//! draws. GNOME's video player Showtime has the same shape (GstPlay with its pictures handed to
//! GTK). GPUI on Linux runs no GLib main loop, so GstPlay's signals are emitted on its own thread
//! and forwarded as [`Event`]s.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_play as gst_play;
use gstreamer_video::{self as gst_video, VideoFrameExt as _};

use super::{Event, Frame};

/// How often the position is reported while playing, in milliseconds: often enough for the bar
/// to move smoothly.
const POSITION_INTERVAL: u32 = 100;

pub struct Player {
    play: gst_play::Play,
    /// Keeps the signal handlers connected.
    _signals: gst_play::PlaySignalAdapter,
}

impl Player {
    /// Opens `path`, paused; its pictures go to `frame`, a [`Event::Frame`] announcing each one the
    /// view hasn't taken yet.
    pub fn new(path: &Path, frame: Arc<Mutex<Option<Frame>>>, events: UnboundedSender<Event>) -> Result<Player, String> {
        gst::init().map_err(|error| format!("GStreamer: {error}"))?;
        let uri = url::Url::from_file_path(path).map_err(|()| format!("Not a file: {}", path.display()))?;
        let caps = gst_video::VideoCapsBuilder::new().format(gst_video::VideoFormat::Bgra).build();
        // One picture waits at most; a late one gives way to the next.
        let sink = gst_app::AppSink::builder().caps(&caps).max_buffers(1).drop(true).build();
        let deliver = {
            let events = events.clone();
            move |sample: gst::Sample| {
                let Some(picture) = picture(&sample) else { return };
                let waiting = frame.lock().expect("frame lock").replace(picture).is_some();
                if !waiting {
                    events.unbounded_send(Event::Frame).ok();
                }
            }
        };
        let preroll = deliver.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    deliver(sink.pull_sample().map_err(|_| gst::FlowError::Eos)?);
                    Ok(gst::FlowSuccess::Ok)
                })
                // The picture a seek lands on while paused.
                .new_preroll(move |sink| {
                    preroll(sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&sink);
        let play = gst_play::Play::new(Some(renderer));
        let mut config = play.config();
        config.set_position_update_interval(POSITION_INTERVAL);
        play.set_config(config).map_err(|error| format!("GStreamer: {error}"))?;
        let signals = gst_play::PlaySignalAdapter::new_sync_emit(&play);
        let send = move |event: Event| {
            events.unbounded_send(event).ok();
        };
        let sent = send.clone();
        signals.connect_position_updated(move |_, at| sent(Event::Position(at.map_or(Duration::ZERO, duration))));
        let sent = send.clone();
        signals.connect_duration_changed(move |_, length| {
            if let Some(length) = length {
                sent(Event::Duration(duration(length)));
            }
        });
        let sent = send.clone();
        signals.connect_state_changed(move |_, state| sent(Event::Playing(state == gst_play::PlayState::Playing)));
        let sent = send.clone();
        signals.connect_end_of_stream(move |_| sent(Event::Ended));
        signals.connect_error(move |_, error, _| send(Event::Failed(error.message().to_string())));
        play.set_uri(Some(uri.as_str()));
        play.pause();
        Ok(Player { play, _signals: signals })
    }

    pub fn play(&self) {
        self.play.play();
    }

    pub fn pause(&self) {
        self.play.pause();
    }

    pub fn seek(&self, to: Duration) {
        self.play.seek(gst::ClockTime::from_nseconds(to.as_nanos().min(u64::MAX as u128) as u64));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.play.stop();
    }
}

fn duration(time: gst::ClockTime) -> Duration {
    Duration::from_nanos(time.nseconds())
}

/// A sample's picture, its rows copied out of the decoder's buffer without their padding.
fn picture(sample: &gst::Sample) -> Option<Frame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
    let (width, height) = (info.width(), info.height());
    let stride = usize::try_from(frame.plane_stride()[0]).ok()?;
    let data = frame.plane_data(0).ok()?;
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(data.get(y * stride..y * stride + row)?);
    }
    Some(Frame { width, height, pixels })
}
