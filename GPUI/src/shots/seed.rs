//! The synthetic mail the screenshot mode shows (`shots.rs`), written into a fresh data folder
//! through the engine's own cache layer, as `FFI/examples/bench.rs` seeds its runs, so it keeps
//! the engine's shapes as they change. The accounts sign in to the engine's stand-in provider
//! (`mailstream_engine::shots`), which takes changes and holds no mail: the engine paints what is
//! here, and nothing fetched replaces it.
//!
//! What there is, for the states a change usually touches:
//! - A Gmail account (Sam Lee, sam@example.com) with mail, Chat and a calendar, and an Outlook
//!   one (sam.lee@contoso.example) with a few mails.
//! - The Gmail inbox: a three-message thread with HTML, an attachment and inline `>` answers
//!   (unread, starred, waiting on you); a five-message group chat by mail with two messages from
//!   one person two minutes apart; a meeting invitation (with its event in the calendar); a long
//!   HTML report with a table; mail with Office attachments; a Chinese mail (the CJK fallback
//!   font); a text mail with a long quoted history; newsletters and notifications, which the
//!   list folds into bundles. Every conversation carries its AI tag, so nothing is tagged
//!   during a run.
//! - Chat: a direct message, a group and a space, the group's recent messages.
//! - The calendars: this week's meetings around today, the design review among them.
//! - Sender pictures: two senders have one; everyone else has a cached miss, so nothing is
//!   looked up on the web.
//!
//! Dates are relative to now, so the list reads "9:41", "Yesterday" and so on as on any day.
//! Add what a change needs here, in the same way; keep it free of anyone's real mail.

use std::path::Path;
use std::sync::Arc;

use chrono::{Datelike, Duration, Local, NaiveTime, TimeZone};
use mailstream_engine::cache::{CachedListing, MailCache};
use mailstream_engine::model::{Account, Envelope, Mailbox, MailboxKey, MessageContent, MessageRef};
use serde_json::{Value, json};

/// Seconds between 1970 and 2001, the engine's reference date.
const REFERENCE: f64 = 978_307_200.0;

const GMAIL: &str = "sam@example.com";
const OUTLOOK: &str = "sam.lee@contoso.example";
const ME: &str = "Sam Lee";

/// People who write; the first two have pictures.
const JORDAN: (&str, &str) = ("Jordan Park", "jordan.park@northwind.example");
const DEVON: (&str, &str) = ("Devon Kim", "devon@northwind.example");
const ALEX: (&str, &str) = ("Alex Rivera", "alex.rivera@example.org");
const PRIYA: (&str, &str) = ("Priya Shah", "priya@example.org");
const MORGAN: (&str, &str) = ("Morgan Lee", "morgan.lee@northwind.example");
const RILEY: (&str, &str) = ("Riley Chen", "riley@fabrikam.example");
const TAYLOR: (&str, &str) = ("Taylor Brooks", "taylor@example.net");
const XIAOMING: (&str, &str) = ("王小明", "xiaoming.wang@example.cn");
const CHRIS: (&str, &str) = ("Chris Doe", "chris@partner.example");
const DANA: (&str, &str) = ("Dana White", "dana@contoso.example");
const EVAN: (&str, &str) = ("Evan Ross", "evan@contoso.example");

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the synthetic mail has the engine's shape")
}

fn person((name, email): (&str, &str)) -> Value {
    json!({ "name": name, "email": email })
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
        - REFERENCE
}

/// Seconds since 2001 at `hour:minute` local time, `days` days from today.
fn at(days: i64, hour: u32, minute: u32) -> f64 {
    let day = Local::now().date_naive() + Duration::days(days);
    let time = day.and_time(NaiveTime::from_hms_opt(hour, minute, 0).expect("a time of day"));
    let local = Local.from_local_datetime(&time).earliest().expect("a local time");
    local.timestamp() as f64 - REFERENCE
}

/// One message: its listing, envelope and content.
struct Mail {
    account: &'static str,
    mailbox: &'static str,
    id: &'static str,
    thread: &'static str,
    from: (&'static str, &'static str),
    to: Vec<(&'static str, &'static str)>,
    cc: Vec<(&'static str, &'static str)>,
    subject: &'static str,
    minutes_ago: f64,
    unread: bool,
    starred: bool,
    reply_to: Option<&'static str>,
    text: Option<String>,
    html: Option<String>,
    /// File name, MIME type, size.
    attachments: Vec<(&'static str, &'static str, i64)>,
    invitation: Option<Value>,
}

impl Mail {
    fn new(account: &'static str, id: &'static str, from: (&'static str, &'static str), subject: &'static str) -> Self {
        let inbox = if account == GMAIL { "INBOX" } else { "AAMk-inbox" };
        let me = if account == GMAIL { (ME, GMAIL) } else { (ME, OUTLOOK) };
        Self {
            account,
            mailbox: inbox,
            id,
            thread: id,
            from,
            to: vec![me],
            cc: Vec::new(),
            subject,
            minutes_ago: 0.,
            unread: false,
            starred: false,
            reply_to: None,
            text: None,
            html: None,
            attachments: Vec::new(),
            invitation: None,
        }
    }

    /// Sent by the account's owner, from its Sent mailbox.
    fn sent(account: &'static str, id: &'static str, subject: &'static str) -> Self {
        let mut mail = Self::new(account, id, (ME, if account == GMAIL { GMAIL } else { OUTLOOK }), subject);
        mail.mailbox = if account == GMAIL { "SENT" } else { "AAMk-sent" };
        mail
    }

    fn message_id(&self) -> String {
        format!("{}@shots.example", self.id)
    }

    fn envelope(&self, now: f64) -> Arc<Envelope> {
        let mut flags = Vec::new();
        if !self.unread {
            flags.push("seen");
        }
        if self.starred {
            flags.push("flagged");
        }
        let in_reply_to: Vec<String> = self.reply_to.iter().map(|id| format!("{id}@shots.example")).collect();
        Arc::new(parse(json!({
            "id": self.id, "thread": self.thread, "messageID": self.message_id(), "inReplyTo": in_reply_to,
            "flags": flags, "subject": self.subject, "from": [person(self.from)],
            "to": self.to.iter().copied().map(person).collect::<Vec<_>>(),
            "date": now - self.minutes_ago * 60., "size": 4_000 + self.text.as_ref().map_or(0, String::len),
            "hasAttachment": !self.attachments.is_empty(),
        })))
    }

    fn content(&self) -> MessageContent {
        let snippet: String = self.text.as_deref().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        let attachments: Vec<Value> = self
            .attachments
            .iter()
            .enumerate()
            .map(|(part, (filename, mime, size))| {
                json!({ "part": part + 2, "filename": filename, "mime": mime, "size": size, "inline": false, "storedName": filename })
            })
            .collect();
        let references: Vec<String> = self.reply_to.iter().map(|id| format!("{id}@shots.example")).collect();
        let mut content = json!({
            "html": self.html, "text": self.text, "cc": self.cc.iter().copied().map(person).collect::<Vec<_>>(),
            "replyTo": [], "references": references, "attachments": attachments,
            "snippet": snippet.chars().take(140).collect::<String>(),
        });
        if let Some(invitation) = &self.invitation {
            content["invitation"] = invitation.clone();
        }
        parse(content)
    }
}

/// Writes the synthetic data into `support` (the engine's data folder) and `caches`.
pub fn write(support: &Path, caches: &Path) {
    std::fs::create_dir_all(support).expect("the data folder");
    write_accounts(support);
    // Tagging stays on, or the list shows no tags; every conversation has one already, and this
    // `omp` doesn't exist, so a run never tags anything with the person's own.
    let settings = json!({ "aiTagging.ompPath": "/nonexistent/shots-omp" });
    std::fs::write(support.join("settings.json"), serde_json::to_string_pretty(&settings).unwrap()).expect("settings.json");
    let mail = mail();
    write_avatars(&caches.join("avatars"), &mail);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let cache = MailCache::new(support.join("mail.db"), caches.to_path_buf());
        write_mail(&cache, &mail);
        write_chat(&cache);
        write_calendar(&cache);
        cache.flush().await;
    });
}

fn write_accounts(support: &Path) {
    let google = [
        "openid",
        "email",
        "profile",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/chat.messages.readonly",
        "https://www.googleapis.com/auth/calendar.readonly",
        "https://www.googleapis.com/auth/calendar.events",
    ];
    let microsoft = [
        "openid",
        "email",
        "profile",
        "offline_access",
        "https://graph.microsoft.com/Mail.ReadWrite",
        "https://graph.microsoft.com/Calendars.ReadWrite",
    ];
    let accounts = json!([
        { "id": GMAIL, "provider": "google", "label": GMAIL, "displayName": ME, "scopes": google },
        { "id": OUTLOOK, "provider": "microsoft", "label": OUTLOOK, "displayName": ME, "scopes": microsoft },
    ]);
    std::fs::write(support.join("accounts.json"), serde_json::to_string_pretty(&accounts).unwrap()).expect("accounts.json");
}

/// Every message, oldest conversations last.
fn mail() -> Vec<(Mail, Option<Value>)> {
    let mut all: Vec<(Mail, Option<Value>)> = Vec::new();
    let tag = |category: &str, labels: &[&str], respond: bool| {
        Some(json!({ "category": category, "toRespond": respond, "labels": labels, "synced": false }))
    };

    // A thread: HTML, an attachment, a reply and inline answers; unread, starred, waiting on you.
    let mut plan = Mail::new(GMAIL, "launch-1", JORDAN, "Q4 launch plan");
    plan.thread = "launch";
    plan.cc = vec![DEVON];
    plan.minutes_ago = 3. * 24. * 60.;
    plan.html = Some(
        "<p>Hi Sam, Devon,</p><p>Here is the <b>Q4 launch plan</b>. The short version:</p>\
         <ul><li>Ship date: <b>Nov 20</b></li><li>Feature flag: <code>launch.q4</code></li>\
         <li>Owners: see the attached plan</li></ul>\
         <p>The full spec is in <a href=\"https://example.com/spec\">the design doc</a>. \
         Let me know what you think by Friday.</p><p>— Jordan</p>"
            .into(),
    );
    plan.text = Some(
        "Hi Sam, Devon,\n\nHere is the Q4 launch plan. The short version:\n\n- Ship date: Nov 20\n\
         - Feature flag: launch.q4\n- Owners: see the attached plan\n\nThe full spec is in the design \
         doc (https://example.com/spec). Let me know what you think by Friday.\n\n— Jordan"
            .into(),
    );
    plan.attachments = vec![("launch-plan.pdf", "application/pdf", 248_000)];
    all.push((plan, None));
    let mut answer = Mail::sent(GMAIL, "launch-2", "Re: Q4 launch plan");
    answer.thread = "launch";
    answer.to = vec![JORDAN];
    answer.cc = vec![DEVON];
    answer.reply_to = Some("launch-1");
    answer.minutes_ago = 2. * 24. * 60.;
    answer.text = Some(
        "Thanks Jordan, two questions inline.\n\n> Ship date: Nov 20\n\nCan we move this to the 24th? \
         QA needs the weekend.\n\n> Owners: see the attached plan\n\nI'll take the onboarding flow.\n\nSam"
            .into(),
    );
    all.push((answer, None));
    let mut inline = Mail::new(GMAIL, "launch-3", DEVON, "Re: Q4 launch plan");
    inline.thread = "launch";
    inline.cc = vec![JORDAN];
    inline.reply_to = Some("launch-2");
    inline.minutes_ago = 95.;
    inline.unread = true;
    inline.starred = true;
    inline.text = Some(
        "Replying inline:\n\n> Can we move this to the 24th? QA needs the weekend.\n\nYes, QA confirmed \
         the 24th works. I moved the date in the tracker.\n\n> I'll take the onboarding flow.\n\nGreat, \
         you're assigned. Can you send the first draft by Wednesday?\n\nDevon"
            .into(),
    );
    all.push((inline, tag("people", &["Launch"], true)));

    // Group mail: five short messages, two from Alex two minutes apart.
    let lunch: [(&'static str, (&'static str, &'static str), f64, &str); 5] = [
        ("lunch-1", ALEX, 300., "Anyone up for lunch on Friday? Thinking the new ramen place on 5th."),
        ("lunch-2", PRIYA, 280., "I'm in! 12:30?"),
        ("lunch-3", (ME, GMAIL), 250., "Works for me 🍜"),
        ("lunch-4", ALEX, 248., "Booked a table for three at 12:30."),
        ("lunch-5", ALEX, 246., "They have vegetarian ramen too, Priya."),
    ];
    for (n, (id, from, minutes, text)) in lunch.into_iter().enumerate() {
        let mut mail = if from.1 == GMAIL {
            Mail::sent(GMAIL, id, "Re: Lunch on Friday?")
        } else {
            Mail::new(GMAIL, id, from, if n == 0 { "Lunch on Friday?" } else { "Re: Lunch on Friday?" })
        };
        mail.thread = "lunch";
        mail.to = [ALEX, PRIYA, (ME, GMAIL)].into_iter().filter(|to| *to != from).collect();
        mail.reply_to = (n > 0).then(|| lunch[n - 1].0);
        mail.minutes_ago = minutes;
        mail.text = Some(text.into());
        let last = n == lunch.len() - 1;
        all.push((mail, if last { tag("people", &["Social"], false) } else { None }));
    }

    // A meeting invitation, its event in the calendar (`write_calendar`).
    let (start, end) = design_review();
    let mut invite = Mail::new(GMAIL, "invite", MORGAN, "Invitation: Design review @ Thu 10:00 – 11:00");
    invite.minutes_ago = 40.;
    invite.unread = true;
    invite.text = Some("Design review for the Q4 launch: onboarding, settings and the new reader.\n\nJoin: https://meet.example.com/design-review".into());
    invite.attachments = vec![("invite.ics", "text/calendar", 2_100)];
    invite.invitation = Some(json!({
        "method": "request", "uid": "design-review@northwind.example", "title": "Design review", "start": start,
        "end": end, "location": "Room 4B", "organizer": { "name": MORGAN.0, "email": MORGAN.1 },
    }));
    all.push((invite, tag("invitation", &[], true)));

    // A long HTML report with a table.
    let mut report = Mail::new(GMAIL, "report", RILEY, "October metrics report");
    report.minutes_ago = 600.;
    report.html = Some(report_html());
    report.text = Some("October metrics: active users up 12%, churn down to 2.1%. Full table inside.".into());
    all.push((report, tag("people", &["Reports"], false)));

    let mut contract = Mail::new(GMAIL, "contract", TAYLOR, "Contract draft for review");
    contract.minutes_ago = 1_500.;
    contract.unread = true;
    contract.html = Some(
        "<p>Hi Sam,</p><p>Attached is <b>version 3</b> of the contract and the rate sheet. The changes from \
         v2 are highlighted in yellow. Could you review sections 4 and 7 before Thursday?</p><p>Thanks,<br>Taylor</p>"
            .into(),
    );
    contract.text = Some("Hi Sam,\n\nAttached is version 3 of the contract and the rate sheet. Could you review sections 4 and 7 before Thursday?\n\nThanks,\nTaylor".into());
    contract.attachments = vec![
        ("Contract v3.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 84_000),
        ("Rates 2027.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", 31_000),
    ];
    all.push((contract, tag("people", &["Legal"], true)));

    let mut chinese = Mail::new(GMAIL, "dinner", XIAOMING, "周末聚餐");
    chinese.minutes_ago = 180.;
    chinese.unread = true;
    chinese.text = Some("Sam 你好，\n\n这周六晚上大家一起吃个饭吧？地点在老地方，六点半。能来的话回我一下。\n\n小明".into());
    all.push((chinese, tag("people", &["Social"], true)));

    let mut quoted = Mail::new(GMAIL, "rate-limits", CHRIS, "Re: Question about the API rate limits");
    quoted.minutes_ago = 2. * 24. * 60. + 200.;
    quoted.text = Some(
        "Thanks, that answers it. We'll batch the requests on our side.\n\nChris\n\n\
         On Mon, Sam Lee wrote:\n> The limit is 600 requests a minute per account. Batches count as one\n\
         > request each, up to 100 calls.\n>\n> On Sun, Chris Doe wrote:\n>> Hi Sam, we keep hitting 429s\n\
         >> around noon. What are the limits, and do batches count?\n>>\n>> Chris"
            .into(),
    );
    all.push((quoted, tag("people", &["Engineering"], false)));

    let mut photos = Mail::new(GMAIL, "hike", ALEX, "Photos from the hike");
    photos.minutes_ago = 5. * 24. * 60.;
    photos.text = Some("Here are the best ones from Saturday. The view from the ridge!".into());
    photos.attachments =
        vec![("ridge.jpg", "image/jpeg", 2_400_000), ("lake.jpg", "image/jpeg", 1_900_000), ("group.jpg", "image/jpeg", 2_100_000)];
    all.push((photos, tag("people", &["Personal"], false)));

    // Newsletters and notifications, which the list bundles.
    let service: [(&'static str, (&'static str, &'static str), &'static str, f64, bool, &str, &str); 7] = [
        ("rust", ("This Week in Rust", "newsletter@this-week-in-rust.example"), "This Week in Rust 571", 720., false, "newsletter", "Crates, talks and the call for participation."),
        ("digest", ("Design Digest", "digest@designdigest.example"), "Design Digest #42: Color systems", 1_300., true, "newsletter", "How three teams built their color tokens."),
        ("brief", ("The Morning Brief", "brief@morningbrief.example"), "The Morning Brief: Tuesday", 200., true, "newsletter", "Markets, weather and five things to know today."),
        ("parcel", ("Parcel Express", "tracking@parcel.example"), "Your package is out for delivery", 60., true, "notification", "Your package 1Z 999 AA1 will arrive today by 8 pm."),
        ("security", ("Google", "no-reply@accounts.google.example"), "Security alert: new sign-in on Linux", 400., false, "notification", "A new sign-in on Linux. If this was you, you don't need to do anything."),
        ("receipt", ("Stripe", "receipts@stripe.example"), "Your receipt from Northwind #2041-1932", 1_700., false, "notification", "Amount paid: $49.00. Thanks for your business."),
        ("ci", ("GitHub", "notifications@github.example"), "[mailstream] CI passed on main", 30., true, "notification", "All checks have passed: build, test, lint."),
    ];
    for (id, from, subject, minutes, unread, category, text) in service {
        let mut mail = Mail::new(GMAIL, id, from, subject);
        mail.minutes_ago = minutes;
        mail.unread = unread;
        mail.text = Some(text.into());
        mail.html = Some(format!(
            "<div style=\"max-width:560px;margin:0 auto;font-family:sans-serif\"><h2 style=\"color:#3b5bdb\">{subject}</h2>\
             <p>{text}</p><p><a href=\"https://example.com/{id}\">Open in browser</a> · \
             <a href=\"https://example.com/unsubscribe\">Unsubscribe</a></p></div>"
        ));
        all.push((mail, tag(category, &[], false)));
    }

    // Outlook.
    let mut budget = Mail::new(OUTLOOK, "budget", DANA, "Budget review: final numbers");
    budget.minutes_ago = 130.;
    budget.unread = true;
    budget.html = Some(
        "<p>Sam,</p><p>Final numbers for Q1 are in the sheet. We're <b>4% under</b> plan overall, but travel \
         is over by $12k. Can we talk before Friday's review?</p><p>Dana</p>"
            .into(),
    );
    budget.text = Some("Sam,\n\nFinal numbers for Q1 are in the sheet. We're 4% under plan overall, but travel is over by $12k. Can we talk before Friday's review?\n\nDana".into());
    all.push((budget, tag("people", &["Finance"], true)));
    let mut offsite = Mail::new(OUTLOOK, "offsite", EVAN, "Team offsite logistics");
    offsite.minutes_ago = 2_000.;
    offsite.text = Some("Hotel is booked for the 14th to the 16th. Bus leaves the office at 8:00 sharp.".into());
    all.push((offsite, tag("people", &["Travel"], false)));
    let mut renew = Mail::new(OUTLOOK, "renew", ("Microsoft", "no-reply@microsoft.example"), "Your Microsoft 365 subscription renews soon");
    renew.minutes_ago = 3_000.;
    renew.text = Some("Your subscription renews on the 1st. No action is needed.".into());
    all.push((renew, tag("notification", &[], false)));
    all
}

/// The design review's start and end: next Thursday (today, if it is Thursday), 10:00 to 11:00.
fn design_review() -> (f64, f64) {
    let weekday = Local::now().weekday().num_days_from_monday() as i64;
    let days = (3 - weekday).rem_euclid(7);
    (at(days, 10, 0), at(days, 11, 0))
}

fn report_html() -> String {
    let rows = [
        ("Active users", "48,210", "54,003", "+12.0%"),
        ("New sign-ups", "6,120", "6,890", "+12.6%"),
        ("Churn", "2.6%", "2.1%", "−0.5 pt"),
        ("Median session", "7m 40s", "8m 05s", "+5.4%"),
        ("Support tickets", "1,204", "1,088", "−9.6%"),
        ("NPS", "41", "44", "+3"),
    ];
    let body: String = rows
        .iter()
        .map(|(metric, before, after, change)| {
            format!("<tr><td>{metric}</td><td align=\"right\">{before}</td><td align=\"right\">{after}</td><td align=\"right\">{change}</td></tr>")
        })
        .collect();
    format!(
        "<h2>October metrics</h2><p>Hi all, here are October's numbers. Growth held up after the pricing change, \
         and churn is the lowest it has been this year.</p>\
         <table border=\"1\" cellpadding=\"6\" style=\"border-collapse:collapse\"><tr><th align=\"left\">Metric</th>\
         <th>September</th><th>October</th><th>Change</th></tr>{body}</table>\
         <h3>Highlights</h3><ul><li>The onboarding checklist lifted day-7 retention by 3 points.</li>\
         <li>Search latency p95 dropped from 420 ms to 180 ms.</li><li>Mobile is now 38% of sessions.</li></ul>\
         <h3>By region</h3><p>Every region grew; APAC grew fastest after the localized onboarding shipped.</p>\
         <ul><li>North America: +9%</li><li>Europe: +11%</li><li>APAC: +21%</li><li>Latin America: +7%</li></ul>\
         <h3>Methodology</h3><p>Active users count anyone who opened the app on at least one day of the month. \
         Sessions shorter than five seconds are left out, as are internal accounts. Churn is the share of last \
         month's paying accounts that cancelled this month.</p><p>Numbers before October 3 come from the old \
         pipeline and may differ from earlier reports by up to 0.3%.</p>\
         <h3>Next month</h3><p>We'll start tracking activation by cohort. Questions welcome.</p><p>Riley</p>"
    )
}

fn write_mail(cache: &MailCache, all: &[(Mail, Option<Value>)]) {
    let now = now();
    let accounts: Vec<Account> = [(GMAIL, "google"), (OUTLOOK, "microsoft")]
        .into_iter()
        .map(|(id, provider)| parse(json!({ "name": id, "label": id, "provider": provider, "identity": { "name": ME, "email": id } })))
        .collect();
    cache.save_accounts(&accounts);
    for (account, inbox, sent) in [(GMAIL, "INBOX", "SENT"), (OUTLOOK, "AAMk-inbox", "AAMk-sent")] {
        let in_box = |mailbox: &'static str| all.iter().filter(move |(mail, _)| mail.account == account && mail.mailbox == mailbox);
        let unread = in_box(inbox).filter(|(mail, _)| mail.unread).count();
        let mut boxes = vec![
            json!({ "account": account, "id": inbox, "name": "Inbox", "role": "inbox", "unread": unread, "total": in_box(inbox).count() }),
            json!({ "account": account, "id": sent, "name": "Sent", "role": "sent" }),
            json!({ "account": account, "id": "drafts", "name": "Drafts", "role": "drafts" }),
            json!({ "account": account, "id": "junk", "name": "Spam", "role": "junk" }),
            json!({ "account": account, "id": "trash", "name": "Trash", "role": "trash" }),
        ];
        // Where Archive moves to: Gmail's All Mail, Outlook's Archive folder.
        boxes.push(if account == GMAIL {
            json!({ "account": account, "id": "ALL", "name": "All Mail", "role": "all" })
        } else {
            json!({ "account": account, "id": "AAMk-archive", "name": "Archive", "role": "archive" })
        });
        if account == GMAIL {
            boxes.extend(["Projects", "Receipts", "Travel"].map(|name| json!({ "account": account, "id": name, "name": name, "role": "other" })));
        }
        let boxes: Vec<Mailbox> = boxes.into_iter().map(parse).collect();
        cache.save_mailboxes(&boxes, account);
        for mailbox in [inbox, sent] {
            let mut mails: Vec<&Mail> = in_box(mailbox).map(|(mail, _)| mail).collect();
            mails.sort_by(|a, b| a.minutes_ago.total_cmp(&b.minutes_ago));
            let envelopes = mails.iter().map(|mail| mail.envelope(now)).collect();
            cache.save_listing(CachedListing { envelopes, next: None }, &MailboxKey::new(account, mailbox));
            for mail in mails {
                let message = MessageRef { account: account.into(), mailbox: mailbox.into(), id: mail.id.into() };
                cache.save_content(&mail.content(), &message);
            }
        }
    }
    // Each conversation's tag, under its latest message's id.
    let tags = all
        .iter()
        .filter_map(|(mail, tag)| Some((mail.message_id(), parse(tag.clone()?))))
        .collect();
    cache.save_tags(tags);
}

fn write_chat(cache: &MailCache) {
    use mailstream_engine::chat_model::{ChatAccount, ChatMessagePage, ChatSpaceKey, ChatSpaceList};
    let now = now();
    let users = [(ME, GMAIL), JORDAN, DEVON, PRIYA];
    let user = |n: usize| json!({ "name": format!("users/{n}"), "displayName": users[n].0, "email": users[n].1, "bot": false });
    let accounts: Vec<ChatAccount> = vec![parse(json!({ "name": GMAIL, "email": GMAIL }))];
    cache.save_chat_accounts(&accounts);
    let message = |space: &str, n: usize, sender: usize, minutes_ago: f64, text: &str| {
        json!({
            "name": format!("{space}/messages/m{n}"), "sender": user(sender), "created": now - minutes_ago * 60.,
            "text": text, "thread": format!("{space}/threads/t{n}"), "threadReply": false, "mine": sender == 0,
            "card": false, "attachments": [],
        })
    };
    let group = "spaces/design-team";
    let log = [
        (1, 75., "Morning! Mockups for the new reader are in the Figma file."),
        (2, 72., "Looks great. Can we try the *larger* avatars?"),
        (0, 70., "Agreed, and the quote fold could be lighter."),
        (3, 64., "I'll update the tokens: <https://example.com/tokens|design tokens>"),
        (1, 12., "Updated. Review at 3?"),
        (2, 10., "👍"),
    ];
    let messages: Vec<Value> = log.iter().enumerate().map(|(n, (sender, minutes, text))| message(group, n, *sender, *minutes, text)).collect();
    let spaces = vec![
        json!({ "name": group, "kind": "group", "title": "Design team", "members": [user(1), user(2), user(3)],
                "lastActive": now - 600., "lastRead": now - 900., "unread": 2, "latest": messages.last().cloned() }),
        json!({ "name": "spaces/jordan", "kind": "dm", "title": JORDAN.0, "members": [user(1)], "lastActive": now - 3_600.,
                "lastRead": now - 3_600., "unread": 0, "latest": message("spaces/jordan", 0, 1, 60., "Sent you the launch plan.") }),
        json!({ "name": "spaces/launch", "kind": "space", "title": "Launch war room", "members": [],
                "lastActive": now - 7_200., "lastRead": now - 7_200., "unread": 0,
                "latest": message("spaces/launch", 0, 2, 120., "Staging is green.") }),
    ];
    let list: ChatSpaceList = parse(json!({ "me": "users/0", "more": false, "spaces": spaces }));
    cache.save_chat_spaces(&list, GMAIL);
    let key = ChatSpaceKey { account: GMAIL.into(), space: group.into() };
    cache.save_chat_recent(std::slice::from_ref(&key));
    let page: ChatMessagePage = parse(json!({ "messages": messages, "more": false, "lastRead": now - 900. }));
    cache.save_chat_messages(&page, &key);
}

fn write_calendar(cache: &MailCache) {
    let mut next = 0;
    let mut event = |title: &str, (day, hour, minute): (i64, u32, u32), minutes: f64, response: Option<&str>| {
        next += 1;
        let start = at(day, hour, minute);
        let mut event = json!({
            "id": format!("e{next}"), "calendar": "work", "title": title, "start": start, "end": start + minutes * 60.,
            "mine": response.is_none() || response == Some("organizer"),
        });
        if let Some(response) = response {
            event["response"] = json!(response);
            event["organizer"] = if response == "organizer" { person((ME, GMAIL)) } else { person(JORDAN) };
            event["attendees"] = json!([
                { "name": JORDAN.0, "email": JORDAN.1, "response": "accepted" },
                { "name": DEVON.0, "email": DEVON.1, "response": "tentative" },
                { "name": ME, "email": GMAIL, "response": if response == "organizer" { "accepted" } else { response } },
            ]);
            event["meetingUrl"] = json!(format!("https://meet.example.com/e{next}"));
        }
        event
    };
    let mut events = Vec::new();
    for day in -7..=7 {
        events.push(event("Standup", (day, 9, 30), 15., Some("accepted")));
    }
    events.extend([
        event("1:1 with Jordan", (-1, 14, 0), 30., Some("accepted")),
        event("Sprint planning", (0, 11, 0), 90., Some("organizer")),
        event("Lunch", (0, 12, 30), 45., None),
        event("Interview: frontend", (0, 15, 0), 60., Some("needsAction")),
        event("Customer call", (1, 11, 0), 30., Some("tentative")),
        event("Release retro", (2, 16, 0), 60., Some("accepted")),
    ]);
    let (start, end) = design_review();
    let mut review = json!({
        "id": "design-review", "calendar": "work", "title": "Design review", "start": start, "end": end,
        "location": "Room 4B", "uid": "design-review@northwind.example", "response": "needsAction", "mine": false,
        "organizer": person(MORGAN), "meetingUrl": "https://meet.example.com/design-review",
    });
    review["attendees"] = json!([
        { "name": MORGAN.0, "email": MORGAN.1, "response": "accepted" },
        { "name": ME, "email": GMAIL, "response": "needsAction" },
    ]);
    events.push(review);
    let copy = json!({
        "fetched": now(), "format": 1, "events": events,
        "calendars": [{ "id": "work", "name": "Work", "color": "#4285f4", "primary": true, "writable": true }],
    });
    cache.save_calendar(&parse(copy), GMAIL);
    let outlook = json!({
        "fetched": now(), "format": 1,
        "calendars": [{ "id": "calendar", "name": "Calendar", "color": "#e67c73", "primary": true, "writable": true }],
        "events": [
            { "id": "o1", "calendar": "calendar", "title": "Budget review", "start": at(1, 13, 30), "end": at(1, 14, 30), "mine": true },
            { "id": "o2", "calendar": "calendar", "title": "Team offsite", "start": at(2, 0, 0), "end": at(4, 0, 0), "allDay": true,
              "startDay": day(2), "endDay": day(4), "mine": true },
        ],
    });
    cache.save_calendar(&parse(outlook), OUTLOOK);
}

/// The local day `days` from today, as `2026-11-14`.
fn day(days: i64) -> String {
    (Local::now().date_naive() + Duration::days(days)).format("%Y-%m-%d").to_string()
}

/// The sender picture cache as the engine keeps it (`FFI/examples/bench.rs`): a contact directory
/// of no accounts, a picture for Jordan and Devon, and a miss made against that directory for
/// everyone else.
fn write_avatars(root: &Path, all: &[(Mail, Option<Value>)]) {
    use sha2::{Digest, Sha256};
    let hex = |data: &[u8]| Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    std::fs::create_dir_all(root).expect("the avatar cache");
    let stamp = hex(b"{}");
    let directory = json!({ "built": now(), "accounts": [], "photos": {}, "people": [], "stamp": stamp });
    std::fs::write(root.join("directory.json"), directory.to_string()).expect("directory.json");
    let mut senders: Vec<&str> = all.iter().flat_map(|(mail, _)| [mail.from.1].into_iter().chain(mail.cc.iter().map(|cc| cc.1))).collect();
    senders.extend([MORGAN.1, PRIYA.1, GMAIL, OUTLOOK]);
    senders.sort_unstable();
    senders.dedup();
    for sender in senders {
        let key = format!("p-{}", hex(sender.to_lowercase().as_bytes()));
        let picture = [(JORDAN.1, [0x3b, 0x82, 0xf6]), (DEVON.1, [0xe8, 0x59, 0x0c])].into_iter().find(|(email, _)| *email == sender);
        match picture {
            Some((_, color)) => portrait(color).save(root.join(format!("{key}.png"))).expect("a picture"),
            None => std::fs::write(root.join(format!("{key}.none")), &stamp).expect("a miss"),
        }
    }
}

/// A plain portrait: a head and shoulders on a gradient of `color`.
fn portrait(color: [u8; 3]) -> image::RgbaImage {
    const SIZE: u32 = 128;
    image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let (fx, fy) = (x as f32 / SIZE as f32, y as f32 / SIZE as f32);
        let head = (fx - 0.5).powi(2) + (fy - 0.4).powi(2) < 0.04;
        let shoulders = (fx - 0.5).powi(2) / 0.12 + (fy - 1.05).powi(2) / 0.16 < 1.;
        if head || shoulders {
            image::Rgba([250, 240, 230, 255])
        } else {
            let shade = 0.75 + 0.25 * (1. - fy);
            image::Rgba([(color[0] as f32 * shade) as u8, (color[1] as f32 * shade) as u8, (color[2] as f32 * shade) as u8, 255])
        }
    })
}
