//! Lists the inboxes through the in-process engine once the accounts' mailboxes are in, then
//! prints change events for a while: `cargo run --example inbox [seconds]` (after
//! `swift build --product MailstreamKit`; sign it with `scripts/sign.sh` to read the sign-ins).
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use futures_executor::block_on;
use mailstream::{Change, Event, EventListener, ListQuery, Mailstream, run_main_loop};

struct Forward(Sender<Event>);

impl EventListener for Forward {
    fn on_event(&self, change: Change) {
        let _ = self.0.send(change.event);
    }
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let engine = Mailstream::new();
    let (events, received) = channel();
    engine.subscribe(Arc::new(Forward(events)));
    std::thread::spawn(move || {
        let status = block_on(engine.status());
        match status {
            Ok(status) if status.accounts.iter().all(|account| account.mailboxes_loaded) && !status.accounts.is_empty() => {}
            _ => while received.recv() != Ok(Event::Mailboxes) {},
        }
        match block_on(engine.list(ListQuery { limit: 10, ..ListQuery::default() })) {
            Ok(listing) => {
                for conversation in &listing.conversations {
                    let mark = if conversation.unread { "●" } else { " " };
                    println!("{mark} {:<28.28}  {:.60}", conversation.from.as_deref().unwrap_or(""), conversation.subject);
                }
                if !listing.failures.is_empty() {
                    println!("failures: {:?}", listing.failures);
                }
                if let Some(first) = listing.conversations.first() {
                    show(&engine, first, listing.next.as_deref());
                }
            }
            Err(error) => println!("list failed: {error}"),
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
        while let Ok(event) = received.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            if event != Event::Previews {
                println!("event: {event:?}");
            }
        }
        std::process::exit(0);
    });
    run_main_loop();
}

/// The first conversation's thread, preview and sender picture, and the next page's size.
fn show(engine: &Mailstream, first: &mailstream::Conversation, next: Option<&str>) {
    match block_on(engine.show(first.id.clone(), 200, true)) {
        Ok(thread) => {
            let html = thread.messages.iter().filter(|message| message.html.is_some()).count();
            println!("thread {:?}: {} messages, {html} with HTML", thread.conversation.subject, thread.messages.len());
        }
        Err(error) => println!("show failed: {error}"),
    }
    let previews = block_on(engine.previews(vec![first.id.clone()]));
    println!("preview: {:?}", previews.map(|map| map.into_values().next().flatten()));
    if let Some(address) = first.from.as_deref().and_then(|from| from.rsplit('<').next()).map(|a| a.trim_end_matches('>')) {
        println!("avatar: {:?}", block_on(engine.avatars(vec![address.into()], vec![])));
    }
    if let Some(cursor) = next {
        let page = block_on(engine.list(ListQuery { limit: 10, cursor: Some(cursor.into()), ..ListQuery::default() }));
        println!("next page: {:?}", page.map(|page| page.conversations.len()));
    }
}