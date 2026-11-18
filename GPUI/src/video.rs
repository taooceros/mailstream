//! A video sent in Chat, played in the log (MS-221), as Flutter's `_ChatVideo` does: a dark poster
//! with the play button and the file's name; clicking it downloads the file (once) and plays it.
//! Then a click on the picture pauses or resumes, the bar along its bottom shows the time and
//! seeks, and its corner button opens the file in the system's player.
//!
//! The picture is GPUI's own: each frame the player hands over becomes an image GPUI draws, so the
//! log clips and scrolls it like anything else (Zed shows video the same way on Linux). The
//! platform's player does the rest: GStreamer's GstPlay on Linux (`video/linux.rs`), and AVPlayer
//! on the Mac (`video/mac.rs`, MS-398).

#[cfg(target_os = "linux")]
#[path = "video/linux.rs"]
mod player;
#[cfg(target_os = "macos")]
#[path = "video/mac.rs"]
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
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, ObjectFit, ParentElement, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled,
    StyledImage, Subscription, Window, actions, div, img, linear_color_stop, linear_gradient, px,
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

/// Asks the pane to show this video in the window-wide lightbox.
pub type Zoom = Rc<dyn Fn(Entity<ChatVideo>, &mut Window, &mut App)>;

actions!(video_lightbox, [Exit, TogglePlay, ToggleFullscreen]);

const LIGHTBOX_CONTEXT: &str = "VideoLightbox";

pub fn bind_keys(cx: &mut App) {
    let lightbox = Some(LIGHTBOX_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", Exit, lightbox),
        KeyBinding::new("space", TogglePlay, lightbox),
        KeyBinding::new("f", ToggleFullscreen, lightbox),
    ]);
}

pub struct ChatVideo {
    name: SharedString,
    fetch: Fetch,
    on_zoom: Zoom,
    zoomed: bool,
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
    pub fn new(name: SharedString, fetch: Fetch, on_zoom: Zoom, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            on_zoom,
            zoomed: false,
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

    pub fn set_zoomed(&mut self, zoomed: bool, cx: &mut Context<Self>) {
        if self.zoomed != zoomed {
            self.zoomed = zoomed;
            cx.notify();
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

    /// While the video plays in the lightbox, the log keeps its box at the same size so the
    /// message column doesn't reflow (MS-399).
    fn render_zoomed(&self) -> gpui::Stateful<gpui::Div> {
        let ink = tokens::ink();
        video_box("video-zoomed")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::S8))
            .child(icon(Icon::Expand, tokens::BADGE_GLYPH, ink.text))
            .child(div().text_size(px(text::FOOTNOTE)).text_color(ink.text).child("Playing in expanded view"))
    }
}

impl Render for ChatVideo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.zoomed {
            return self.render_zoomed();
        }
        let (Some(frame), Some(path)) = (self.frame.clone(), self.path.clone()) else {
            return self.render_poster(cx);
        };
        let ink = tokens::ink();
        let length = self.duration.unwrap_or_default();
        let shown = self.scrubbing.map_or(self.position, |fraction| length.mul_f32(fraction));
        let this = cx.entity();
        let double_zoom = self.on_zoom.clone();
        let button_zoom = self.on_zoom.clone();
        let zoom_target = this.clone();
        video_box("video")
            .cursor_pointer()
            .on_click(move |event: &ClickEvent, window, cx| {
                this.update(cx, |this, cx| this.toggle(cx));
                if event.click_count() == 2 {
                    double_zoom(this.clone(), window, cx);
                }
            })
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
                    )
                    .child(
                        icon_button("video-zoom", Icon::Expand, "Expand video", true, ink.text)
                            .on_click(move |_, window, cx| button_zoom(zoom_target.clone(), window, cx)),
                    ),
            )
    }
}

/// The window-wide lightbox hosting a zoomed [`ChatVideo`] without restarting or interrupting its
/// player (MS-403). Supports toggling window fullscreen; Esc exits fullscreen first, then closes.
pub struct VideoLightbox {
    video: Entity<ChatVideo>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    on_close: Rc<dyn Fn(&mut Window, &mut App)>,
    _video: Subscription,
}

impl VideoLightbox {
    pub fn new(
        video: Entity<ChatVideo>,
        previous_focus: Option<FocusHandle>,
        on_close: Rc<dyn Fn(&mut Window, &mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        video.update(cx, |video, cx| video.set_zoomed(true, cx));
        let _video = cx.observe(&video, |_, _, cx| cx.notify());
        cx.on_release(|this, cx| {
            this.video.update(cx, |video, cx| video.set_zoomed(false, cx));
        })
        .detach();
        Self { video, focus, previous_focus, on_close, _video }
    }

    pub fn video(&self) -> &Entity<ChatVideo> {
        &self.video
    }

    /// Leaves fullscreen if active, returns the video to its inline box, and restores focus.
    pub fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.video.update(cx, |video, cx| video.set_zoomed(false, cx));
        if let Some(previous) = self.previous_focus.take() {
            window.focus(&previous, cx);
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(window, cx);
        (self.on_close)(window, cx);
    }

    fn exit(&mut self, _: &Exit, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_fullscreen() {
            window.toggle_fullscreen();
            cx.notify();
        } else {
            self.close(window, cx);
        }
    }

    fn toggle_play(&mut self, _: &TogglePlay, _: &mut Window, cx: &mut Context<Self>) {
        self.video.update(cx, |video, cx| video.toggle(cx));
    }

    fn toggle_fullscreen(&mut self, _: &ToggleFullscreen, window: &mut Window, cx: &mut Context<Self>) {
        window.toggle_fullscreen();
        cx.notify();
    }
}

impl Render for VideoLightbox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fullscreen = window.is_fullscreen();
        let ink = tokens::ink();
        let (frame, path, playing, length, shown, bar, failure, name) = {
            let video = self.video.read(cx);
            let length = video.duration.unwrap_or_default();
            let shown = video.scrubbing.map_or(video.position, |fraction| length.mul_f32(fraction));
            (
                video.frame.clone(),
                video.path.clone(),
                video.playing,
                length,
                shown,
                video.bar.clone(),
                video.failure.clone(),
                video.name.clone(),
            )
        };
        let body = if let Some(frame) = frame {
            div()
                .id("video-lightbox-player")
                .relative()
                .size_full()
                .bg(if fullscreen { ink.fullscreen } else { ink.frame })
                .when(!fullscreen, |d| d.rounded(px(radius::PANE)))
                .overflow_hidden()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.video.update(cx, |video, cx| video.toggle(cx));
                }))
                .child(img(frame).absolute().inset_0().size_full().object_fit(ObjectFit::Contain))
                .when(!playing, |d| {
                    d.child(div().absolute().inset_0().flex().items_center().justify_center().child(badge()))
                })
                .child(
                    div()
                        .id("video-lightbox-close-wrap")
                        .absolute()
                        .top(px(space::S12))
                        .right(px(space::S12))
                        .rounded_full()
                        .bg(ink.badge)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            icon_button("video-lightbox-close", Icon::Close, "Close (Esc)", true, ink.text)
                                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        ),
                )
                .child(
                    div()
                        .id("video-lightbox-bar")
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(tokens::LIGHTBOX_BAR))
                        .px(px(space::S12))
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .when(!fullscreen, |d| d.rounded_b(px(radius::PANE)))
                        .bg(linear_gradient(180., linear_color_stop(ink.scrim[0], 0.), linear_color_stop(ink.scrim[1], 1.)))
                        .cursor_default()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            icon_button(
                                "video-lightbox-play",
                                if playing { Icon::Pause } else { Icon::Play },
                                if playing { "Pause (Space)" } else { "Play (Space)" },
                                true,
                                ink.text,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.video.update(cx, |video, cx| video.toggle(cx));
                            })),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(text::CAPTION))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink.text)
                                .child(format!("{} / {}", clock(shown), clock(length))),
                        )
                        .child(Slider::new(&bar).flex_1().bg(ink.text).text_color(ink.text))
                        .when_some(path, |d, path| {
                            d.child(
                                icon_button("video-lightbox-open", Icon::OpenInWindow, "Open in the system's player", true, ink.text)
                                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
                            )
                        })
                        .child(
                            icon_button(
                                "video-lightbox-fullscreen",
                                if fullscreen { Icon::FullscreenExit } else { Icon::Fullscreen },
                                if fullscreen { "Exit Fullscreen (Esc)" } else { "Fullscreen (F)" },
                                true,
                                ink.text,
                            )
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.toggle_fullscreen();
                                cx.notify();
                            })),
                        ),
                )
                .into_any_element()
        } else {
            div()
                .id("video-lightbox-empty")
                .relative()
                .size_full()
                .bg(if fullscreen { ink.fullscreen } else { ink.frame })
                .when(!fullscreen, |d| d.rounded(px(radius::PANE)))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(if failure.is_some() { ink.failure } else { ink.text })
                        .child(failure.unwrap_or(name)),
                )
                .child(
                    div()
                        .id("video-lightbox-close-wrap")
                        .absolute()
                        .top(px(space::S12))
                        .right(px(space::S12))
                        .rounded_full()
                        .bg(ink.badge)
                        .child(
                            icon_button("video-lightbox-close", Icon::Close, "Close (Esc)", true, ink.text)
                                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        ),
                )
                .into_any_element()
        };
        div()
            .id("video-lightbox")
            .key_context(LIGHTBOX_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::exit))
            .on_action(cx.listener(Self::toggle_play))
            .on_action(cx.listener(Self::toggle_fullscreen))
            .absolute()
            .inset_0()
            .occlude()
            .bg(if fullscreen { ink.fullscreen } else { ink.backdrop })
            .when(!fullscreen, |d| d.p(px(tokens::LIGHTBOX_INSET)))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(body)
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

    struct LightboxHarness {
        focus: FocusHandle,
        video: Entity<ChatVideo>,
        lightbox: Option<Entity<VideoLightbox>>,
    }

    impl LightboxHarness {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            let host = cx.entity().downgrade();
            let video = cx.new(|cx| {
                let mut video = ChatVideo::new(
                    "clip.mp4".into(),
                    Rc::new(|_, _| {}),
                    Rc::new(move |video, window, cx| {
                        host.update(cx, |host, cx| host.open(video, window, cx)).ok();
                    }),
                    window,
                    cx,
                );
                let buffer = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
                video.frame = Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])));
                video.path = Some(Arc::from(Path::new("/tmp/clip.mp4")));
                video
            });
            Self { focus, video, lightbox: None }
        }

        fn open(&mut self, video: Entity<ChatVideo>, window: &mut Window, cx: &mut Context<Self>) {
            let previous = window.focused(cx).or_else(|| Some(self.focus.clone()));
            let host = cx.entity().downgrade();
            let lightbox = cx.new(|cx| {
                VideoLightbox::new(
                    video,
                    previous,
                    Rc::new(move |_, cx| {
                        host.update(cx, |host, cx| {
                            host.lightbox = None;
                            cx.notify();
                        })
                        .ok();
                    }),
                    window,
                    cx,
                )
            });
            self.lightbox = Some(lightbox);
            cx.notify();
        }

        fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            if let Some(lightbox) = self.lightbox.take() {
                lightbox.update(cx, |lightbox, cx| lightbox.finish(window, cx));
                cx.notify();
            }
        }
    }

    impl Render for LightboxHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .child(self.video.clone())
                .children(self.lightbox.clone())
        }
    }

    #[gpui::test]
    fn lightbox_toggles_fullscreen_and_esc_exits_fullscreen_before_closing(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui::init(cx);
            crate::design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let (host, cx) = cx.add_window_view(LightboxHarness::new);
        let video = host.read_with(cx, |h, _| h.video.clone());

        host.update_in(cx, |h, window, cx| {
            assert!(!window.is_fullscreen());
            h.open(video.clone(), window, cx);
        });
        cx.run_until_parked();
        assert!(video.read_with(cx, |v, _| v.zoomed));
        assert!(host.read_with(cx, |h, _| h.lightbox.is_some()));

        // Pressing `f` enters fullscreen.
        cx.simulate_keystrokes("f");
        cx.run_until_parked();
        host.read_with(cx, |_, cx| {
            // TestWindowExt / Window::is_fullscreen check via update_in below.
            let _ = cx;
        });
        host.update_in(cx, |_, window, _| assert!(window.is_fullscreen()));

        // First Esc exits fullscreen while keeping the lightbox open.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        host.update_in(cx, |h, window, cx| {
            assert!(!window.is_fullscreen());
            assert!(h.lightbox.is_some());
            assert!(video.read(cx).zoomed);
        });

        // Second Esc closes the lightbox and returns the video to its inline box.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        host.update_in(cx, |h, window, cx| {
            assert!(!window.is_fullscreen());
            assert!(h.lightbox.is_none());
            assert!(!video.read(cx).zoomed);
        });

        // Closing directly while fullscreen also exits fullscreen.
        host.update_in(cx, |h, window, cx| h.open(video.clone(), window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("f");
        cx.run_until_parked();
        host.update_in(cx, |h, window, cx| {
            assert!(window.is_fullscreen());
            h.close(window, cx);
        });
        cx.run_until_parked();
        host.update_in(cx, |h, window, cx| {
            assert!(!window.is_fullscreen());
            assert!(h.lightbox.is_none());
            assert!(!video.read(cx).zoomed);
        });
    }
}
