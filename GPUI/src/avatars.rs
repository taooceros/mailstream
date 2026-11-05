//! Contact photos: the picture the native app shows for an address (directory photo, BIMI,
//! Gravatar), asked of the engine (`Mailstream::avatars`), which answers with PNG files in its
//! cache. Addresses drawn during a frame are asked for together, one request at a time; answers,
//! pictures and misses alike, are kept for the session. A failed request is retried after a
//! growing pause.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Div, FontWeight, Global, Hsla, IntoElement, ObjectFit, ParentElement, SharedString, Styled,
    StyledImage, div, img, px,
};
use url::Url;

use crate::design::{self, Palette};
use crate::engine::{MailError, engine};
use crate::model::Address;

/// Addresses per request.
const BATCH: usize = 50;
/// The pause before an address whose request failed is asked for again; it doubles with every
/// further failure up to `LAST_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(10);
const LAST_RETRY: Duration = Duration::from_secs(600);

enum Photo {
    /// Queued or being asked for; `failures` earlier requests for it failed.
    Pending { failures: u32 },
    /// The engine's answer: its picture, or none.
    Known(Option<Arc<Path>>),
    /// Not asked again before `retry`.
    Failed { retry: Instant, failures: u32 },
}

/// The session's photos by address as drawn (the engine matches addresses case-insensitively).
#[derive(Default)]
struct Avatars {
    photos: HashMap<SharedString, Photo>,
    /// Addresses waiting for the next request; the newest are asked for first.
    queue: Vec<SharedString>,
    /// A request is in flight (or about to be sent); it takes the queue when it finishes.
    asking: bool,
}

impl Global for Avatars {}

pub fn init(cx: &mut App) {
    cx.set_global(Avatars::default());
}

/// A `size` circle for `address`: its photo once the app has one, else its initial on its color.
/// Drawing it asks for the photo; rows redraw when it arrives.
pub fn avatar(address: &Address, size: f32, p: &Palette, cx: &mut App) -> AnyElement {
    let color = design::avatar_color(&address.email, p.dark);
    let Some(path) = photo(&address.email, cx) else {
        return initials(address.initial(), size, color, p.on_accent).into_any_element();
    };
    let initial = address.initial();
    let on_accent = p.on_accent;
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        // The app's pictures are square; anything else is cut to the circle's square, not spilled.
        .overflow_hidden()
        .bg(color)
        .child(
            img(path)
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover)
                // A file that is gone or unreadable shows the letter, as no photo would.
                .with_fallback(move || initials(initial.clone(), size, color, on_accent).into_any_element()),
        )
        .into_any_element()
}

fn initials(initial: SharedString, size: f32, color: Hsla, text: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.43))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(text)
        .child(initial)
}

/// The known photo for `email`; an address not asked for yet (or due for a retry) joins the queue.
fn photo(email: &SharedString, cx: &mut App) -> Option<Arc<Path>> {
    let avatars = cx.global_mut::<Avatars>();
    if email.is_empty() {
        return None;
    }
    let failures = match avatars.photos.get(email.as_ref()) {
        Some(Photo::Known(path)) => return path.clone(),
        Some(Photo::Pending { .. }) => return None,
        Some(Photo::Failed { retry, failures }) => {
            if Instant::now() < *retry {
                return None;
            }
            *failures
        }
        None => 0,
    };
    avatars.photos.insert(email.clone(), Photo::Pending { failures });
    avatars.queue.push(email.clone());
    if !avatars.asking {
        avatars.asking = true;
        // Runs after this frame, so everything it draws goes out in one request.
        cx.spawn(async move |cx| {
            while let Ok(Some(emails)) = cx.update(next_batch) {
                let (emails, answer) = cx.background_executor().spawn(ask(emails)).await;
                if cx.update(|cx| settle(emails, answer, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    None
}

/// The newest queued addresses, or none once the queue is empty (the request loop then ends).
fn next_batch(cx: &mut App) -> Option<Vec<SharedString>> {
    let avatars = cx.global_mut::<Avatars>();
    if avatars.queue.is_empty() {
        avatars.asking = false;
        return None;
    }
    let start = avatars.queue.len().saturating_sub(BATCH);
    Some(avatars.queue.split_off(start))
}

/// Asks the engine for `emails`' photos; the answer is in `emails`' order.
async fn ask(emails: Vec<SharedString>) -> (Vec<SharedString>, Result<Vec<Option<Arc<Path>>>, MailError>) {
    let addresses = emails.iter().map(|email| email.to_string()).collect();
    let answer = engine().avatars(addresses, Vec::new()).await.map(|mut urls| {
        emails
            .iter()
            .map(|email| {
                let url = urls.remove(&email.to_lowercase()).flatten()?;
                Some(Arc::from(Url::parse(&url).ok()?.to_file_path().ok()?))
            })
            .collect()
    });
    (emails, answer)
}

fn settle(emails: Vec<SharedString>, answer: Result<Vec<Option<Arc<Path>>>, MailError>, cx: &mut App) {
    let avatars = cx.global_mut::<Avatars>();
    match answer {
        Ok(paths) => {
            let found = paths.iter().any(Option::is_some);
            for (email, path) in emails.into_iter().zip(paths) {
                avatars.photos.insert(email, Photo::Known(path));
            }
            if found {
                cx.refresh_windows();
            }
        }
        Err(_) => {
            let now = Instant::now();
            let mut soonest = LAST_RETRY;
            for email in emails {
                let failures = match avatars.photos.get(&email) {
                    Some(Photo::Pending { failures }) => failures + 1,
                    _ => 1,
                };
                let pause = FIRST_RETRY.saturating_mul(1 << (failures - 1).min(6)).min(LAST_RETRY);
                soonest = soonest.min(pause);
                avatars.photos.insert(email, Photo::Failed { retry: now + pause, failures });
            }
            // Redraw once the first of them is due, so the rows still showing them ask again.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(soonest).await;
                cx.update(|cx| cx.refresh_windows()).ok();
            })
            .detach();
        }
    }
}
