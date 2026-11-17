//! A video sent in Chat, played in the log (MS-221), as Flutter's `_ChatVideo` does: a dark poster
//! with the play button and the file's name; clicking it downloads the file (once) and plays it.
//! Then a click on the picture pauses or resumes, the bar along its bottom shows the time and
//! seeks, and its corner button opens the file in the system's player.
//!
//! The picture is GPUI's own: each frame the player hands over becomes an image GPUI draws, so the
//! log clips and scrolls it like anything else (Zed shows video the same way on Linux). The
//! platform's player does the rest: GStreamer's GstPlay on Linux (`video/linux.rs`). The Mac's
//! player (AVPlayer) is MS-398; until then the Mac shows videos as tiles.

#[cfg(target_os = "linux")]
#[path = "video/linux.rs"]
mod player;

use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{self, UnboundedSender};
use gpui::component::slider::{Slider, SliderEvent, SliderState};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton, ObjectFit,
    ParentElement, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription,
    Window, div, img, linear_color_stop, linear_gradient, px,
};

use crate::controls::icon_button;
use crate::design::{radius, space, text, video as tokens};
use crate::icons::{Icon, icon};

use player::Player;

/// A picture the player decoded: `width` × `height` BGRA pixels, row after row.
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// What the player reports, from its own threads.
enum Event {
    /// A new picture waits in the shared slot.
    Frame,
    Position(Duration),
    Duration(Duration),
    Playing(bool),
    Ended,
    Failed(String),
}

/// Asks the pane for the video's file; the pane answers with [`ChatVideo::load`].
pub type Fetch = Rc<dyn Fn(Entity<ChatVideo>, &mut App)>;

pub struct ChatVideo {
    name: SharedString,
    fetch: Fetch,
    /// Downloading or opening, until the first picture shows.
    loading: bool,
    /// Why it couldn't download or play, on the poster.
    failure: Option<SharedString>,
    path: Option<Arc<Path>>,
    player: Option<Player>,
    /// The latest picture from the player, and the one before it, freed from the sprite atlas once
    /// the next arrives (by then no frame draws it).
    frame: Option<Arc<RenderImage>>,
    retired: Option<Arc<RenderImage>>,
    /// Where the player puts its newest picture; the view takes it on [`Event::Frame`].
    slot: Arc<Mutex<Option<Frame>>>,
    events: UnboundedSender<Event>,
    playing: bool,
    ended: bool,
    position: Duration,
    duration: Option<Duration>,
    /// Where the bar is held while it is dragged (0–1); the time shows it.
    scrubbing: Option<f32>,
    bar: Entity<SliderState>,
    _bar_events: Subscription,
}

impl ChatVideo {
    pub fn new(name: SharedString, fetch: Fetch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (events, mut received) = mpsc::unbounded();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = received.next().await {
                if this.update_in(cx, |this, window, cx| this.event(event, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Continuous (no step): any point of the bar seeks there.
        let bar = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.));
        let bar_events = cx.subscribe_in(&bar, window, |this, _, event: &SliderEvent, _, cx| match event {
            SliderEvent::Change(value) => {
                this.scrubbing = Some(value.end());
                cx.notify();
            }
            SliderEvent::Release(value) => {
                this.scrubbing = None;
                if let (Some(player), Some(length)) = (&this.player, this.duration) {
                    let to = length.mul_f32(value.end().clamp(0., 1.));
                    player.seek(to);
                    this.position = to;
                    this.ended = false;
                }
                cx.notify();
            }
        });
        // Frees the pictures it leaves in the sprite atlas.
        cx.on_release(|this, cx| {
            for image in this.frame.take().into_iter().chain(this.retired.take()) {
                cx.drop_image(image, None);
            }
        })
        .detach();
        ChatVideo {
            name,
            fetch,
            loading: false,
            failure: None,
            path: None,
            player: None,
            frame: None,
            retired: None,
            slot: Arc::default(),
            events,
            playing: false,
            ended: false,
            position: Duration::ZERO,
            duration: None,
            scrubbing: None,
            bar,
            _bar_events: bar_events,
        }
    }

    /// The file the pane fetched for it, or why it couldn't: plays it.
    pub fn load(&mut self, file: Result<Arc<Path>, SharedString>, cx: &mut Context<Self>) {
        cx.notify();
        let path = match file {
            Ok(path) => path,
            Err(reason) => return self.fail(format!("Could not download: {reason}").into()),
        };
        match Player::new(&path, self.slot.clone(), self.events.clone()) {
            Ok(player) => {
                player.play();
                self.playing = true;
                self.player = Some(player);
                self.path = Some(path);
            }
            Err(reason) => self.fail(reason.into()),
        }
    }

    /// Back to the poster, saying why.
    fn fail(&mut self, reason: SharedString) {
        self.loading = false;
        self.failure = Some(reason);
        self.player = None;
        self.playing = false;
    }

    /// A click on the poster: downloading starts, unless it is under way. Answers the pane's fetch
    /// to call once this view is let go (the pane may answer at once, with the file here).
    fn start(&mut self, cx: &mut Context<Self>) -> Option<Fetch> {
        if self.loading {
            return None;
        }
        self.loading = true;
        self.failure = None;
        cx.notify();
        Some(self.fetch.clone())
    }

    /// A click on the picture: pauses, resumes, or plays again from the start once it ended.
    fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(player) = &self.player else { return };
        if self.ended {
            self.ended = false;
            self.position = Duration::ZERO;
            player.seek(Duration::ZERO);
            player.play();
            self.playing = true;
        } else if self.playing {
            player.pause();
            self.playing = false;
        } else {
            player.play();
            self.playing = true;
        }
        cx.notify();
    }

    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Frame => {
                let Some(frame) = self.slot.lock().expect("frame lock").take() else { return };
                let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels) else { return };
                // GPUI's images hold BGRA, as the player hands them over.
                let image = Arc::new(RenderImage::new([image::Frame::new(buffer)]));
                if let Some(old) = self.retired.take() {
                    cx.drop_image(old, Some(window));
                }
                self.retired = self.frame.replace(image);
                self.loading = false;
            }
            Event::Position(at) => {
                self.position = at;
                if self.scrubbing.is_none() {
                    let length = self.duration.unwrap_or_default().as_secs_f32();
                    let fraction = if length > 0. { at.as_secs_f32() / length } else { 0. };
                    self.bar.update(cx, |bar, cx| bar.set_value(fraction.clamp(0., 1.), window, cx));
                }
            }
            Event::Duration(length) => self.duration = Some(length),
            Event::Playing(playing) => self.playing = playing,
            Event::Ended => {
                self.playing = false;
                self.ended = true;
                if let Some(length) = self.duration {
                    self.position = length;
                }
            }
            Event::Failed(reason) => {
                self.fail(reason.into());
                for image in self.frame.take().into_iter().chain(self.retired.take()) {
                    cx.drop_image(image, Some(window));
                }
            }
        }
        cx.notify();
    }

    /// Before the first picture: the dark frame with the play button (or "Loading…") and the
    /// file's name, or why it couldn't play.
    fn render_poster(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let ink = tokens::ink();
        let this = cx.entity();
        video_box("video-poster")
            .flex()
            .items_center()
            .justify_center()
            .when(!self.loading, |d| {
                d.cursor_pointer().on_click(move |_: &ClickEvent, _, cx| {
                    if let Some(fetch) = this.update(cx, |video, cx| video.start(cx)) {
                        fetch(this.clone(), cx);
                    }
                })
            })
            .child(if self.loading {
                div().text_size(px(text::FOOTNOTE)).text_color(ink.text).child("Loading…").into_any_element()
            } else {
                badge().into_any_element()
            })
            .child(
                div()
                    .absolute()
                    .left(px(space::S12))
                    .right(px(space::S12))
                    .bottom(px(space::S10))
                    .truncate()
                    .text_size(px(text::FOOTNOTE))
                    .text_color(if self.failure.is_some() { ink.failure } else { ink.text })
                    .child(self.failure.clone().unwrap_or_else(|| self.name.clone())),
            )
    }
}

impl Render for ChatVideo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (Some(frame), Some(path)) = (self.frame.clone(), self.path.clone()) else {
            return self.render_poster(cx);
        };
        let ink = tokens::ink();
        let length = self.duration.unwrap_or_default();
        let shown = self.scrubbing.map_or(self.position, |fraction| length.mul_f32(fraction));
        video_box("video")
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle(cx)))
            // Fills the box, the picture letterboxed: a laid-out image's own aspect ratio sets its
            // minimum height, so it is placed rather than laid out.
            .child(
                img(frame)
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(radius::PANE))
                    .object_fit(ObjectFit::Contain),
            )
            .when(!self.playing, |d| {
                d.child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().child(badge()))
            })
            .child(
                // Clicks and drags here stay off the picture under it.
                div()
                    .id("video-bar")
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(tokens::BAR))
                    .pl(px(space::S10))
                    .pr(px(space::S4))
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .rounded_b(px(radius::PANE))
                    .bg(linear_gradient(180., linear_color_stop(ink.scrim[0], 0.), linear_color_stop(ink.scrim[1], 1.)))
                    .cursor_default()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(text::CAPTION))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink.text)
                            .child(format!("{} / {}", clock(shown), clock(length))),
                    )
                    .child(Slider::new(&self.bar).flex_1().bg(ink.text).text_color(ink.text))
                    .child(
                        icon_button("video-open", Icon::OpenInWindow, "Open in the system's player", true, ink.text)
                            .on_click(move |_, _, cx| cx.open_with_system(&path)),
                    ),
            )
    }
}

/// The video's box in the log, the same before and while it plays, on its dark frame.
fn video_box(id: &'static str) -> gpui::Stateful<gpui::Div> {
    let (width, height) = tokens::SIZE;
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .rounded(px(radius::PANE))
        .bg(tokens::ink().frame)
}

/// The round play button.
fn badge() -> gpui::Div {
    let ink = tokens::ink();
    div()
        .flex_none()
        .size(px(tokens::BADGE))
        .rounded_full()
        .bg(ink.badge)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(Icon::Play, tokens::BADGE_GLYPH, ink.text))
}

/// A time as players show it: `m:ss`, `h:mm:ss` from an hour.
fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_reads_like_a_players() {
        assert_eq!(clock(Duration::ZERO), "0:00");
        assert_eq!(clock(Duration::from_millis(9_999)), "0:09");
        assert_eq!(clock(Duration::from_secs(605)), "10:05");
        assert_eq!(clock(Duration::from_secs(3_725)), "1:02:05");
    }
}
