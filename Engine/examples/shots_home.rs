//! Writes a synthetic home for screenshots (`scripts/shots.py`, MS-243): no real account, no
//! personal mail, nothing that reaches a provider. `cargo run --example shots_home -- DIR` fills
//! `DIR` as the engine's home (settings.json, accounts.json, mail.db) for an app started with
//! `HOME=DIR CFFIXED_USER_HOME=DIR`:
//!
//! - Two accounts: "Work Mail" (Gmail, `reader@example.com`, with mail, chat and calendar scopes)
//!   and "Personal" (Outlook, `reader@example.org`).
//! - Work's inbox: a three-message RFC thread with HTML, attachments, an inline-quoted reply, a
//!   star and unread messages (tagged, waiting on you); a four-message people thread (HTML with a
//!   plain-text part, a `>` quote with the answer below it, two messages in a row from one
//!   sender, one sent by the user); a meeting invitation; a long single HTML mail with a table;
//!   and filler mail across the Smart Inbox's categories. Personal's inbox holds a few more.
//! - Work's Google Chat: a direct message, a group and a named space, one with a conversation.
//! - Work's calendar: three weeks of meetings around today (answered, unanswered, declined, one
//!   starting ten minutes after the seed), all-day and multi-day events.
//!
//! The accounts have no sign-in, so the engine serves what mail.db holds and syncing fails
//! quietly; the calendar's copy counts as fresh for five minutes after the seed, then shows a
//! "sign in again" notice above the same events. Dates follow the time the seed runs.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{Datelike, Duration, Local, NaiveTime, TimeZone, Timelike};
use mailstream_engine::cache::{CachedListing, MailCache};
use mailstream_engine::model::{Envelope, MailboxKey, MessageRef};
use serde_json::{Value, json};

const WORK: &str = "reader@example.com";
const PERSONAL: &str = "reader@example.org";
/// Seconds between 1970 and 2001, where the engine's dates start.
const REFERENCE: f64 = 978_307_200.0;

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the fixture has the engine's shape")
}

fn person(name: &str, email: &str) -> Value {
    json!({ "name": name, "email": email })
}

/// The engine's date for a local time.
fn date(time: chrono::DateTime<Local>) -> f64 {
    time.timestamp() as f64 - REFERENCE
}

/// One message as the fixture describes it.
struct Message {
    account: &'static str,
    id: String,
    thread: String,
    subject: String,
    from: Value,
    to: Vec<Value>,
    cc: Vec<Value>,
    minutes_ago: f64,
    seen: bool,
    flagged: bool,
    html: Option<String>,
    text: Option<String>,
    /// File name, type, size.
    attachments: Vec<(&'static str, &'static str, i64)>,
    invitation: Option<Value>,
    sent: bool,
}

impl Message {
    fn new(account: &'static str, id: &str, thread: &str, subject: &str, from: &Value, minutes_ago: f64) -> Self {
        Message {
            account,
            id: id.into(),
            thread: thread.into(),
            subject: subject.into(),
            from: from.clone(),
            to: vec![],
            cc: vec![],
            minutes_ago,
            seen: true,
            flagged: false,
            html: None,
            text: None,
            attachments: vec![],
            invitation: None,
            sent: false,
        }
    }

    /// The tags' key.
    fn message_id(&self) -> String {
        format!("{}@shots.example", self.id)
    }
}

fn main() {
    let home = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example shots_home -- DIR");
        std::process::exit(2)
    });
    let home = Path::new(&home);
    let support = home.join("Library/Application Support/Mailstream");
    let caches = home.join("Library/Caches/dev.mailstream.Mailstream");
    std::fs::create_dir_all(&support).expect("the home is writable");
    std::fs::create_dir_all(caches.join("avatars")).expect("the home is writable");
    write_settings(home, &support);

    let now = Local::now();
    let (messages, tags) = mail();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let cache = MailCache::new(support.join("mail.db"), caches.clone());
        save_mail(&cache, &messages, date(now));
        cache.save_tags(tags.into_iter().map(|(key, tag)| (key, parse(tag))).collect());
        save_chat(&cache, date(now));
        let copy = json!({ "fetched": date(now), "calendars": calendars(), "events": events(now), "format": 1 });
        cache.save_calendar(&parse(copy), WORK);
        cache.flush().await;
    });
    println!("{}", home.display());
}

fn write_settings(home: &Path, support: &Path) {
    // Tagging on with an omp that doesn't exist: the fixture's tags stay as written.
    let settings = json!({
        "aiTagging.enabled": true,
        "aiTagging.ompPath": home.join("no-omp").display().to_string(),
        "sync.pollSeconds": 3600,
        "chat.pollSeconds": 3600,
    });
    std::fs::write(support.join("settings.json"), serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    let google = [
        "openid",
        "email",
        "profile",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/chat.messages.readonly",
        "https://www.googleapis.com/auth/calendar.readonly",
        "https://www.googleapis.com/auth/calendar.events",
    ];
    let microsoft = ["openid", "email", "profile", "offline_access", "https://graph.microsoft.com/Mail.ReadWrite"];
    let accounts = json!([
        { "id": WORK, "provider": "google", "label": "Work Mail", "displayName": "Reader Test", "scopes": google },
        { "id": PERSONAL, "provider": "microsoft", "label": "Personal", "displayName": "Reader Test", "scopes": microsoft },
    ]);
    std::fs::write(support.join("accounts.json"), serde_json::to_string_pretty(&accounts).unwrap()).unwrap();
}

/// Every message, and the tags (by message id) of the conversations that have one.
fn mail() -> (Vec<Message>, HashMap<String, Value>) {
    let me = person("Reader Test", WORK);
    let maya = person("Maya Lin", "maya.lin@example.com");
    let devon = person("Devon Brooks", "devon.brooks@example.com");
    let elena = person("Elena Rostova", "elena.rostova@example.org");
    let jordan = person("Jordan Lee", "jordan.lee@example.net");
    let alex = person("Alex Rivera", "alex.rivera@example.net");
    let platform = person("Platform Architecture", "platform-arch@example.com");
    let qa = person("QA", "qa@example.com");
    let long_html = long_html();
    let mut messages = Vec::new();
    let mut tags: HashMap<String, Value> = HashMap::new();
    let tag = |id: &str| format!("{id}@shots.example");

    // A work thread of three, shown as cards: tagged, waiting on you.
    let mut m = Message::new(WORK, "rfc1", "t-rfc", "[RFC] Schema v4 migration plan", &maya, 26. * 60.);
    m.to = vec![me.clone(), devon.clone(), elena.clone()];
    m.cc = vec![platform.clone(), qa.clone()];
    m.html = Some("<p>Kicking off the RFC for <b>schema v4</b>. Draft attached; comments by Friday please.</p><p>Maya</p>".into());
    m.text = Some("Kicking off the RFC for schema v4. Draft attached; comments by Friday please.\n\nMaya".into());
    m.attachments = vec![("schema-v4-draft.pdf", "application/pdf", 482_113)];
    messages.push(m);
    let mut m = Message::new(WORK, "rfc2", "t-rfc", "Re: [RFC] Schema v4 migration plan", &devon, 5. * 60.);
    m.to = vec![maya.clone(), me.clone()];
    m.cc = vec![platform.clone()];
    m.seen = false;
    m.flagged = true;
    m.text = Some("Two questions inline.\n\nOn Tue, Maya Lin <maya.lin@example.com> wrote:\n> Kicking off the RFC for schema v4.\n\nDo we dual-write during backfill, or freeze writes?\n\n> Draft attached; comments by Friday please.\n\nFriday works. I'll send the benchmark numbers Thursday.\n\nDevon".into());
    messages.push(m);
    let mut m = Message::new(WORK, "rfc3", "t-rfc", "Re: [RFC] Schema v4 migration plan", &elena, 40.);
    m.to = vec![me.clone(), maya.clone(), devon.clone()];
    m.cc = vec![platform.clone(), qa.clone()];
    m.seen = false;
    m.html = Some(long_html.clone());
    m.text = Some("Hi team, this is the schema v4 migration plan.".into());
    m.attachments = vec![
        ("schema-v4-migration-architecture.pdf", "application/pdf", 2_310_442),
        ("bench-results.zip", "application/zip", 9_812_004),
        ("cutover.png", "image/png", 312_880),
    ];
    messages.push(m);
    tags.insert(tag("rfc3"), json!({ "category": "notification", "toRespond": true, "labels": ["Work", "tech"] }));

    // People: a conversation of four, shown as a chat log.
    let mut m = Message::new(WORK, "chat1", "t-chat", "Inline replies in the reader", &jordan, 3. * 60.);
    m.to = vec![me.clone(), alex.clone()];
    m.html = Some("<p>I pushed the <b>Reader</b> changes. The chat log now reads <code>m.text</code> first; details in the <a href=\"https://example.com/spec\">design spec</a>.</p>".into());
    m.text = Some("I pushed the Reader changes. The chat log now reads m.text first; details in the design spec (https://example.com/spec).".into());
    messages.push(m);
    let mut m = Message::new(WORK, "chat2", "t-chat", "Re: Inline replies in the reader", &alex, 2. * 60. + 4.);
    m.to = vec![jordan.clone(), me.clone()];
    m.text = Some("Nice, trying it now.".into());
    messages.push(m);
    let mut m = Message::new(WORK, "chat3", "t-chat", "Re: Inline replies in the reader", &alex, 2. * 60. + 2.);
    m.to = vec![jordan.clone(), me.clone()];
    m.text = Some("One question from the review:\n> Should we keep inline replies visible below a quote line?\nYes, the answer sits right below it, and\n    indented code like this\n    keeps its indentation.".into());
    messages.push(m);
    let mut m = Message::new(WORK, "chat4", "t-chat", "Re: Inline replies in the reader", &me, 50.);
    m.to = vec![jordan.clone(), alex.clone()];
    m.text = Some("Agreed, I'll file it.\n\nOn Wed, Alex Rivera wrote:\n> One question from the review".into());
    m.sent = true;
    messages.push(m);
    tags.insert(tag("chat4"), json!({ "category": "people", "toRespond": false, "labels": ["Design"] }));

    // A meeting invitation, two days from now at 17:00.
    let start = date((Local::now() + Duration::days(2)).with_time(NaiveTime::from_hms_opt(17, 0, 0).unwrap()).unwrap());
    let mut m = Message::new(WORK, "inv1", "t-inv", "Invitation: Reader design review", &elena, 90.);
    m.to = vec![me.clone()];
    m.seen = false;
    m.html = Some("<p>Let's walk through the reader review together.</p>".into());
    m.text = Some("Let's walk through the reader review together.".into());
    m.attachments = vec![("invite.ics", "text/calendar", 2_048)];
    m.invitation = Some(json!({
        "method": "request", "uid": "review@shots.example", "title": "Reader design review", "start": start,
        "end": start + 3600., "location": "https://meet.example.com/abc-defg-hij", "organizer": elena,
    }));
    messages.push(m);
    tags.insert(tag("inv1"), json!({ "category": "invitation", "toRespond": true, "labels": [] }));

    // A long single mail.
    let mut m = Message::new(WORK, "long1", "t-long", "Weekly platform notes: storage, sync and the reader", &platform, 7. * 60.);
    m.to = vec![me.clone()];
    m.cc = vec![qa.clone()];
    m.html = Some(long_html.replace("Hi team,", "Hello,"));
    messages.push(m);
    tags.insert(tag("long1"), json!({ "category": "people", "toRespond": false, "labels": ["Work"] }));

    let fillers: [(&'static str, &str, &str, &str, &str, &str); 12] = [
        (WORK, "Lunch Thursday?", "Sam Patel", "sam.patel@example.com", "people", "Are you free for lunch Thursday at noon?"),
        (WORK, "Your receipt from Example Store", "Example Store", "receipts@store.example", "notification", "Thanks for your order #4411. Total $42.10."),
        (WORK, "Security alert: new sign-in", "Example Account", "no-reply@accounts.example", "notification", "A new sign-in on Mac. If this was you, there's nothing to do."),
        (WORK, "The Weekly Byte #212", "The Weekly Byte", "news@weeklybyte.example", "newsletter", "Ten links on GPU UI frameworks this week."),
        (WORK, "Design crit notes", "Priya Shah", "priya.shah@example.com", "people", "Notes from today's crit are in the doc; the density pass looks good."),
        (WORK, "Re: Offsite agenda", "Chris Wong", "chris.wong@example.com", "people", "Added the hike on day two. Thoughts?"),
        (WORK, "Build failed: main #1842", "CI", "ci@example.com", "notification", "The build of main failed in the GPUI step."),
        (WORK, "Quarterly planning", "Taylor Kim", "taylor.kim@example.com", "people", "Can you send your Q1 priorities by Monday?"),
        (PERSONAL, "Your flight to New York", "Example Air", "trips@air.example", "notification", "Check-in opens 24 hours before departure."),
        (PERSONAL, "Book club: next pick", "Morgan Diaz", "morgan.diaz@example.org", "people", "How about a short one this month?"),
        (PERSONAL, "Weekend sale: 30% off", "Example Outfitters", "deals@outfitters.example", "promotion", "Our biggest sale of the season ends Sunday."),
        (PERSONAL, "Photos from the hike", "Jamie Fox", "jamie.fox@example.org", "people", "Uploaded the photos; the one at the summit is great."),
    ];
    for (i, (account, subject, name, email, category, body)) in fillers.into_iter().enumerate() {
        let id = format!("f{i}");
        let mut m = Message::new(account, &id, &format!("t-{id}"), subject, &person(name, email), 8. * 60. + i as f64 * 300.);
        m.to = vec![person("Reader Test", account)];
        m.seen = i % 3 != 0;
        m.text = Some(body.into());
        tags.insert(m.message_id(), json!({ "category": category, "toRespond": false, "labels": [] }));
        messages.push(m);
    }
    (messages, tags)
}

fn long_html() -> String {
    let cell = "border:1px solid #ccc;padding:4px";
    let mut html = String::from("<div style=\"font-family:-apple-system,Helvetica,Arial;font-size:15px;line-height:1.5\"><p>Hi team,</p><p>This is the <b>schema v4 migration plan</b>. The short version: we move the listings table to per-mailbox partitions, keep the old reader path for one release and drop it after.</p>");
    for n in 1..=8 {
        html.push_str(&format!("<h3>Step {n}</h3><p>Paragraph {n} explains what changes and why. Rows written before the cutover keep their old layout until a background pass rewrites them; readers accept both until then. See <a href=\"https://example.com/spec#{n}\">the spec, section {n}</a>.</p>"));
        if n == 3 {
            html.push_str(&format!("<table style=\"border-collapse:collapse;width:100%\"><tr><th style=\"{cell}\">Phase</th><th style=\"{cell}\">Owner</th><th style=\"{cell}\">Due</th></tr><tr><td style=\"{cell}\">Dual write</td><td style=\"{cell}\">Maya</td><td style=\"{cell}\">Nov 18</td></tr><tr><td style=\"{cell}\">Backfill</td><td style=\"{cell}\">Devon</td><td style=\"{cell}\">Nov 25</td></tr></table>"));
        }
    }
    html.push_str("<p>Thanks,<br>Elena</p></div>");
    html
}

fn save_mail(cache: &MailCache, messages: &[Message], now: f64) {
    for (account, inbox, sent) in [(WORK, "INBOX", "SENT"), (PERSONAL, "inbox", "sentitems")] {
        let boxes = [(inbox, "Inbox", "inbox"), (sent, "Sent", "sent"), ("drafts", "Drafts", "drafts"), ("junk", "Spam", "junk"), ("trash", "Trash", "trash")];
        let boxes: Vec<_> = boxes
            .iter()
            .map(|(id, name, role)| parse(json!({ "account": account, "id": id, "name": name, "role": role })))
            .collect();
        cache.save_mailboxes(&boxes, account);
        let (mut received, mut outgoing): (Vec<Arc<Envelope>>, Vec<Arc<Envelope>>) = (vec![], vec![]);
        for m in messages.iter().filter(|m| m.account == account) {
            let mut flags = vec![];
            if m.seen {
                flags.push("seen");
            }
            if m.flagged {
                flags.push("flagged");
            }
            let envelope = Arc::new(parse(json!({
                "id": m.id, "thread": m.thread, "messageID": m.message_id(), "inReplyTo": [], "flags": flags,
                "subject": m.subject, "from": [m.from], "to": m.to, "date": now - m.minutes_ago * 60.,
                "size": 4000, "hasAttachment": !m.attachments.is_empty(),
            })));
            let attachments: Vec<Value> = m
                .attachments
                .iter()
                .enumerate()
                .map(|(i, (name, mime, size))| json!({ "part": i + 2, "filename": name, "mime": mime, "size": size, "inline": false, "storedName": format!("{i}-{name}") }))
                .collect();
            let snippet: String = m.text.as_deref().unwrap_or_default().chars().take(80).collect();
            let mut content = json!({ "cc": m.cc, "replyTo": [], "references": [], "attachments": attachments, "snippet": snippet });
            if let Some(html) = &m.html {
                content["html"] = json!(html);
            }
            if let Some(text) = &m.text {
                content["text"] = json!(text);
            }
            if let Some(invitation) = &m.invitation {
                content["invitation"] = invitation.clone();
            }
            let mailbox = if m.sent { sent } else { inbox };
            cache.save_content(&parse(content), &MessageRef { account: account.into(), mailbox: mailbox.into(), id: m.id.clone() });
            if m.sent { outgoing.push(envelope) } else { received.push(envelope) }
        }
        received.sort_by(|a, b| b.date.partial_cmp(&a.date).expect("dated"));
        cache.save_listing(CachedListing { envelopes: received, next: None }, &MailboxKey::new(account, inbox));
        cache.save_listing(CachedListing { envelopes: outgoing, next: None }, &MailboxKey::new(account, sent));
    }
    let accounts: Vec<_> = [(WORK, "Work Mail", "google"), (PERSONAL, "Personal", "microsoft")]
        .iter()
        .map(|(id, label, provider)| parse(json!({ "name": id, "label": label, "provider": provider, "identity": { "name": "Reader Test", "email": id } })))
        .collect();
    cache.save_accounts(&accounts);
}

/// Work's Google Chat: three spaces, the first with a conversation.
fn save_chat(cache: &MailCache, now: f64) {
    let user = |n: usize, name: &str| json!({ "name": format!("users/{n}"), "displayName": name, "email": format!("{}@example.com", name.to_lowercase().replace(' ', ".")) });
    let me = user(0, "Reader Test");
    let (maya, devon, priya) = (user(1, "Maya Lin"), user(2, "Devon Brooks"), user(3, "Priya Shah"));
    let lines = [
        (&maya, "Morning! Did the *schema v4* RFC go out?"),
        (&me, "Yes, it's in your inbox. Comments by Friday."),
        (&maya, "Great. I'll look at the backfill part first."),
        (&maya, "Also: the benchmark numbers are in <https://example.com/bench|the sheet>."),
        (&me, "Thanks, adding them to the doc now."),
    ];
    let space = "spaces/dm-maya";
    let messages: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(n, (sender, text))| {
            json!({
                "name": format!("{space}/messages/m{n}"), "sender": sender, "created": now - (40. - n as f64 * 6.) * 60.,
                "text": text, "thread": format!("{space}/threads/t{n}"), "threadReply": false, "mine": sender["name"] == me["name"],
                "card": false, "attachments": [],
            })
        })
        .collect();
    let latest = messages.last().unwrap().clone();
    let other = |space: &str, sender: &Value, text: &str, minutes: f64| {
        json!({ "name": format!("{space}/messages/last"), "sender": sender, "created": now - minutes * 60., "text": text, "thread": format!("{space}/threads/last"), "threadReply": false, "mine": false, "card": false, "attachments": [] })
    };
    let spaces = json!([
        { "name": space, "kind": "dm", "title": "Maya Lin", "members": [maya], "lastActive": latest["created"], "lastRead": latest["created"], "unread": 0, "latest": latest },
        { "name": "spaces/group", "kind": "group", "title": "Devon Brooks, Priya Shah", "members": [devon, priya], "lastActive": now - 3. * 3600., "lastRead": now - 4. * 3600., "unread": 2, "latest": other("spaces/group", &devon, "Lunch at noon?", 180.) },
        { "name": "spaces/platform", "kind": "space", "title": "Platform", "members": [], "lastActive": now - 26. * 3600., "lastRead": now - 26. * 3600., "unread": 0, "latest": other("spaces/platform", &priya, "Release notes are up.", 26. * 60.) },
    ]);
    cache.save_chat_accounts(&[parse(json!({ "name": WORK, "email": WORK }))]);
    cache.save_chat_spaces(&parse(json!({ "me": "users/0", "more": false, "spaces": spaces })), WORK);
    let key = parse(json!({ "account": WORK, "space": space }));
    cache.save_chat_recent(std::slice::from_ref(&key));
    cache.save_chat_messages(&parse(json!({ "messages": messages, "more": false, "lastRead": latest["created"] })), &key);
}

fn calendars() -> Value {
    json!([
        { "id": "work", "name": "Work", "color": "#4285f4", "primary": true, "writable": true },
        { "id": "personal", "name": "Personal", "color": "#33b679", "writable": true },
    ])
}

/// Three weeks of events around `now`.
fn events(now: chrono::DateTime<Local>) -> Vec<Value> {
    let at = |day: i64, hour: u32, minute: u32| {
        let day = now.date_naive() + Duration::days(day);
        let time = day.and_hms_opt(hour, 0, 0).unwrap() + Duration::minutes(minute as i64);
        date(Local.from_local_datetime(&time).earliest().expect("a local time"))
    };
    let guest = |n: usize, response: &str| json!({ "name": format!("Person {n}"), "email": format!("person{n}@example.com"), "response": response });
    let mut next = 0;
    let mut timed = |calendar: &str, title: &str, (day, hour, minute): (i64, u32, u32), minutes: u32, response: Option<&str>| {
        next += 1;
        let start = at(day, hour, minute);
        let mut event = json!({
            "id": format!("e{next}"), "calendar": calendar, "title": title, "start": start, "end": start + minutes as f64 * 60.,
            "mine": response.is_none() || response == Some("organizer"),
        });
        if let Some(response) = response {
            event["response"] = json!(response);
            event["organizer"] = if response == "organizer" { person("Reader Test", WORK) } else { guest(1, "accepted") };
            event["attendees"] = json!([guest(1, "accepted"), guest(2, "tentative"), guest(3, "needsAction")]);
            event["meetingUrl"] = json!(format!("https://meet.example.com/{next}"));
        }
        if title.contains("Lunch") || title.contains("Dinner") {
            event["location"] = json!("Cafe Example");
        }
        event
    };
    let all_day = |id: &str, calendar: &str, title: &str, first: i64, days: i64| {
        let day = |offset: i64| (now.date_naive() + Duration::days(offset)).format("%Y-%m-%d").to_string();
        json!({
            "id": id, "calendar": calendar, "title": title, "start": at(first, 0, 0), "end": at(first + days, 0, 0),
            "allDay": true, "startDay": day(first), "endDay": day(first + days), "mine": true,
        })
    };
    let mut events: Vec<Value> = (-10..=14)
        .filter(|day| (now + Duration::days(*day)).weekday().number_from_monday() <= 5)
        .map(|day| timed("work", "Standup", (day, 9, 30), 15, Some("accepted")))
        .collect();
    events.extend([
        timed("work", "Design review", (-2, 10, 0), 60, Some("organizer")),
        timed("work", "1:1 with Person 4", (-2, 14, 0), 30, Some("accepted")),
        timed("work", "Sprint planning", (-1, 9, 45), 90, Some("organizer")),
        timed("personal", "Lunch with Person 5", (-1, 12, 0), 60, None),
        timed("work", "Architecture review", (0, 10, 30), 90, Some("organizer")),
        timed("personal", "Lunch", (0, 12, 30), 45, None),
        timed("work", "Interview: frontend", (0, 14, 0), 60, Some("needsAction")),
        timed("work", "Focus time", (0, 15, 0), 120, None),
        timed("personal", "Gym", (0, 18, 0), 60, None),
        timed("work", "Customer call", (1, 11, 0), 30, Some("accepted")),
        timed("work", "Offsite prep", (1, 13, 0), 60, Some("organizer")),
        timed("work", "Hiring sync", (1, 13, 0), 30, Some("tentative")),
        timed("personal", "Dinner with Person 6", (1, 19, 0), 120, None),
        timed("work", "Release retro", (2, 16, 0), 60, Some("accepted")),
        timed("work", "Workshop", (4, 13, 0), 180, Some("accepted")),
        timed("work", "Quarterly planning", (7, 9, 0), 180, Some("organizer")),
        timed("personal", "Dentist", (8, 8, 0), 60, Some("declined")),
        // Ten minutes after the seed, so the calendar's Up Next bar shows for the next half hour.
        timed("work", "Design crit", (0, now.hour(), now.minute() + 10), 30, Some("accepted")),
    ]);
    events.push(all_day("trip", "personal", "Conference trip", 3, 3));
    events.push(all_day("hike", "personal", "Weekend hike", -3, 1));
    events.push(all_day("ooo", "work", "Person 7 out of office", 1, 2));
    events
}
