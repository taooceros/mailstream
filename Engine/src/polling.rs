//! Keeping mail current, as the Swift engine's `MailStore` schedules it
//! (`Sources/MailstreamKit/Store/MailStore.swift`): the default list refreshes every
//! `sync.pollSeconds` while push is down and every `sync.pushPollSeconds` while it delivers
//! changes, every account's counts every five minutes, and a push socket that just connected
//! catches up on what changed while it was down. The `refresh` command and the `sync` event
//! (loading or push connectivity changed) live here too.
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::Events;
use crate::mail::Mail;
use crate::push::Push;
use crate::settings::{Setting, Settings};

/// Between counts refreshes.
const COUNTS_INTERVAL: Duration = Duration::from_secs(300);

pub struct Polling {
    mail: Arc<Mail>,
    push: Arc<Push>,
    settings: Arc<Settings>,
    events: Arc<Events>,
    /// The interval may have changed: a setting, or push connectivity.
    reschedule: Notify,
}

impl Polling {
    pub fn new(mail: Arc<Mail>, push: Arc<Push>, settings: Arc<Settings>, events: Arc<Events>) -> Self {
        Self { mail, push, settings, events, reschedule: Notify::new() }
    }

    /// Starts polling and following push connectivity. Call on the engine's runtime.
    pub fn start(self: &Arc<Self>) {
        let polling = self.clone();
        tokio::spawn(async move {
            periodic(
                || polling.interval(),
                &polling.reschedule,
                || {
                    if !polling.mail.is_loading() {
                        polling.mail.clone().refresh();
                    }
                },
            )
            .await;
        });
        let mail = self.mail.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(COUNTS_INTERVAL).await;
                mail.refresh_counts();
            }
        });
        let polling = self.clone();
        let mut connections = self.push.connections();
        tokio::spawn(async move {
            while connections.changed().await.is_ok() {
                let connected = *connections.borrow_and_update();
                polling.events.emit_soon("sync");
                polling.reschedule.notify_one();
                if connected && polling.mail.is_connected() {
                    if !polling.mail.is_loading() {
                        polling.mail.clone().refresh();
                    }
                    polling.mail.refresh_counts();
                }
            }
        });
    }

    /// The settings changed; a changed interval starts its wait afresh.
    pub fn settings_changed(&self) {
        self.reschedule.notify_one();
    }

    /// The `refresh` command: new mail and counts now.
    pub fn refresh(&self) -> Value {
        self.mail.clone().refresh();
        self.mail.refresh_counts();
        json!({ "started": true })
    }

    fn interval(&self) -> Duration {
        let setting = if self.push.is_connected() { Setting::PushPollSeconds } else { Setting::PollSeconds };
        let seconds = self.settings.value(setting).as_u64().or_else(|| setting.default_value().as_u64()).unwrap_or(90);
        Duration::from_secs(seconds)
    }
}

/// Calls `tick` every `interval()`, as the Swift engine's `PeriodicTask`: when `changed` is
/// notified and the interval is another, the wait starts afresh; the same interval keeps the
/// wait already running.
async fn periodic(interval: impl Fn() -> Duration, changed: &Notify, mut tick: impl FnMut()) {
    let mut current = interval();
    let mut deadline = Instant::now() + current;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                tick();
                deadline = Instant::now() + current;
            }
            () = changed.notified() => {
                let next = interval();
                if next != current {
                    current = next;
                    deadline = Instant::now() + next;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn ticks_every_interval_and_restarts_only_on_a_change() {
        let interval = Arc::new(Mutex::new(Duration::from_secs(90)));
        let ticks = Arc::new(AtomicUsize::new(0));
        let changed = Arc::new(Notify::new());
        let (now, count) = (interval.clone(), ticks.clone());
        let notify = changed.clone();
        let started = Instant::now();
        let at = Arc::new(Mutex::new(Vec::new()));
        let seen = at.clone();
        tokio::spawn(async move {
            periodic(
                move || *now.lock().unwrap(),
                &notify,
                move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    seen.lock().unwrap().push(started.elapsed().as_secs());
                },
            )
            .await;
        });
        tokio::time::sleep(Duration::from_secs(185)).await;
        assert_eq!(*at.lock().unwrap(), [90, 180]);
        // The same interval: the wait already running goes on.
        changed.notify_one();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
        // Another one: the wait starts afresh from then (200 s), so no tick at 270.
        *interval.lock().unwrap() = Duration::from_secs(900);
        tokio::time::sleep(Duration::from_secs(5)).await;
        changed.notify_one();
        tokio::time::sleep(Duration::from_secs(1000)).await;
        assert_eq!(*at.lock().unwrap(), [90, 180, 1100]);
    }
}
