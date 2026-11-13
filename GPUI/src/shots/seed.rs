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
//!   font) Sam answered last, so its row shows Sam's reply as from "me"; a text mail with a long
//!   quoted history; newsletters and notifications, which the list folds into bundles: a GitHub
//!   notification (a transparent logo, a grey panel, a green button), a newsletter on its own
//!   grey page with a brand band and a pastel callout, a morning brief with a photo, a logo and a
//!   table of coloured rows; and a card painted on a paper texture (which dark mode keeps on
//!   paper, MS-267). Every conversation with mail in the inbox carries its AI tag, so nothing is
//!   tagged during a run.
//! - Conversations Sam started that nobody answered yet, which list in the inbox from Sent alone:
//!   the newest of all in Gmail, and one in Outlook.
//! - Chat: a direct message, a group and a space, the group's recent messages.
//! - The calendars: this week's meetings around today, the design review among them; it and
//!   Sprint planning (which Sam organizes, so it can be edited) carry a place, notes with links,
//!   a link to the provider's page and guests with every kind of answer. Jordan's calendar,
//!   shared with Sam read-only, shows beside Sam's in its own color; a holiday calendar Sam
//!   subscribed to is listed but off (MS-302).
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
/// `mail_only`: the accounts' sign-ins granted mail alone, as a sign-in asks now (MS-306).
pub fn write(support: &Path, caches: &Path, mail_only: bool) {
    std::fs::create_dir_all(support).expect("the data folder");
    write_accounts(support, mail_only);
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

fn write_accounts(support: &Path, mail_only: bool) {
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
    // Mail alone: what each provider lists for a sign-in that asks for nothing more.
    let (google, microsoft) = if mail_only { (&google[..4], &microsoft[..5]) } else { (&google[..], &microsoft[..]) };
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
    chinese.text = Some("Sam 你好，\n\n这周六晚上大家一起吃个饭吧？地点在老地方，六点半。能来的话回我一下。\n\n小明".into());
    all.push((chinese, None));
    // Sam answered last: the row shows the reply, from "me", at its time.
    let mut dinner_reply = Mail::sent(GMAIL, "dinner-2", "Re: 周末聚餐");
    dinner_reply.thread = "dinner";
    dinner_reply.to = vec![XIAOMING];
    dinner_reply.reply_to = Some("dinner");
    dinner_reply.minutes_ago = 12.;
    dinner_reply.text = Some("好呀，我六点半到，带一瓶酒过去。\n\nSam".into());
    all.push((dinner_reply, tag("people", &["Social"], false)));

    // Conversations Sam started that nobody has answered: in the inbox from Sent alone.
    let mut proposal = Mail::sent(GMAIL, "tokens", "Proposal: one set of design tokens");
    proposal.to = vec![MORGAN, DEVON];
    proposal.minutes_ago = 6.;
    proposal.text = Some(
        "Morgan, Devon,\n\nThe app and the site each keep their own colors and type sizes. I'd like us to move \
         both onto one set of design tokens before the launch. Rough plan:\n\n- one file of tokens, generated \
         for both\n- the site switches first, the app in the next release\n\nCan we talk it through on \
         Thursday?\n\nSam"
            .into(),
    );
    all.push((proposal, None));

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
        mail.html = Some(match id {
            "ci" => github_html(),
            "digest" => digest_html(),
            "brief" => brief_html(),
            _ => format!(
                "<div style=\"max-width:560px;margin:0 auto;font-family:sans-serif\"><h2 style=\"color:#3b5bdb\">{subject}</h2>\
                 <p>{text}</p><p><a href=\"https://example.com/{id}\">Open in browser</a> · \
                 <a href=\"https://example.com/unsubscribe\">Unsubscribe</a></p></div>"
            ),
        });
        all.push((mail, tag(category, &[], false)));
    }

    // A card painted on a paper texture: dark mode can't move its colours, so it stays on paper.
    let mut card = Mail::new(GMAIL, "card", PRIYA, "A postcard from Lisbon");
    card.minutes_ago = 2_600.;
    card.text = Some("Greetings from Lisbon! The trams, the tiles, the pastéis de nata. Wish you were here. Priya".into());
    card.html = Some(card_html());
    all.push((card, tag("people", &["Personal"], false)));

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
    let mut vendors = Mail::sent(OUTLOOK, "vendors", "Vendor shortlist for Q1");
    vendors.to = vec![DANA];
    vendors.minutes_ago = 320.;
    vendors.text = Some("Dana,\n\nHere are the three vendors I'd shortlist for Q1, with their quotes. Could you check them against the budget?\n\nSam".into());
    all.push((vendors, None));
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

/// A GitHub notification as GitHub writes it: a transparent black logo on white, a bordered box, a
/// grey panel of checks, a green button and a footer with a plain link.
fn github_html() -> String {
    let logo = png_data(&image::RgbaImage::from_fn(64, 64, |x, y| {
        let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
        let inside = dx * dx + dy * dy < 31. * 31.;
        let eye = (dx.abs() - 10.).powi(2) + (dy + 4.).powi(2) < 25.;
        if inside && !eye { image::Rgba([0x1f, 0x23, 0x28, 255]) } else { image::Rgba([0, 0, 0, 0]) }
    }));
    let check = |name: &str, time: &str| {
        format!(
            "<tr><td style=\"padding:6px 12px;color:#1a7f37;width:16px\">✓</td><td style=\"padding:6px 0\">{name}</td>\
             <td align=\"right\" style=\"padding:6px 12px;color:#59636e\">{time}</td></tr>"
        )
    };
    format!(
        "<div style=\"background:#ffffff;color:#1f2328;font-family:-apple-system,'Segoe UI',sans-serif;font-size:14px\">\
         <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:600px\"><tr><td style=\"padding:8px 0 16px\">\
         <img src=\"{logo}\" width=\"32\" height=\"32\" alt=\"GitHub\"></td></tr>\
         <tr><td style=\"border:1px solid #d0d7de;border-radius:6px;padding:16px\">\
         <h2 style=\"font-weight:400;font-size:20px;margin:0 0 8px\">[taooceros/mailstream] CI passed on main</h2>\
         <p style=\"color:#59636e;margin:0 0 16px\">All checks have passed for <code>ae417aa</code>: build, test, lint.</p>\
         <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#f6f8fa;border-radius:6px;margin-bottom:16px\">\
         {}{}{}</table>\
         <a href=\"https://github.example/taooceros/mailstream/actions\" style=\"display:inline-block;background:#1f883d;color:#ffffff;\
         padding:6px 16px;border-radius:6px;text-decoration:none;font-weight:600\">View workflow run</a></td></tr>\
         <tr><td style=\"color:#59636e;font-size:12px;padding-top:16px\">You are receiving this because you are watching this repository. \
         <a href=\"https://github.example/notifications/unsubscribe\">Unsubscribe</a> or manage your notification settings.</td></tr>\
         </table></div>",
        check("build (ubuntu-latest)", "4m 12s"),
        check("test (ubuntu-latest)", "6m 03s"),
        check("lint", "1m 40s"),
    )
}

/// A newsletter on its own grey page: a white card with a brand band, a pastel callout, a yellow
/// button, a hairline and a muted footer.
fn digest_html() -> String {
    "<table width=\"100%\" bgcolor=\"#f2f0eb\" cellpadding=\"0\" cellspacing=\"0\" style=\"font-family:Georgia,serif\"><tr>\
     <td align=\"center\" style=\"padding:24px 12px\">\
     <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"#ffffff\" style=\"max-width:520px;border-radius:8px\">\
     <tr><td style=\"background:#3b5bdb;color:#ffffff;padding:20px 24px;border-radius:8px 8px 0 0\">\
     <div style=\"font-size:12px;letter-spacing:2px;color:#dbe4ff\">DESIGN DIGEST · ISSUE 42</div>\
     <div style=\"font-size:24px;font-weight:bold\">Color systems</div></td></tr>\
     <tr><td style=\"padding:20px 24px;color:#333333;font-size:15px;line-height:1.5\">\
     <p>How three teams built their color tokens, and what they would do again.</p>\
     <div style=\"background:#e7f5ff;border-left:4px solid #339af0;padding:12px 16px;color:#1c4f82\">\
     <b>This week:</b> semantic tokens beat raw palettes once a second theme ships.</div>\
     <h3 style=\"color:#212529\">1. Name roles, not hues</h3>\
     <p>Call it <i>surface</i> and <i>on-surface</i>, and the dark theme becomes a second table of values.</p>\
     <p style=\"text-align:center;margin:24px 0\"><a href=\"https://designdigest.example/42\" style=\"background:#ffd43b;\
     color:#212529;padding:10px 20px;border-radius:20px;text-decoration:none;font-weight:bold\">Read the issue</a></p>\
     <hr style=\"border:0;border-top:1px solid #dee2e6\">\
     <p style=\"color:#868e96;font-size:12px\">You subscribed at designdigest.example. \
     <a href=\"https://designdigest.example/unsubscribe\" style=\"color:#868e96\">Unsubscribe</a></p>\
     </td></tr></table></td></tr></table>"
        .into()
}

/// A morning brief with images: a transparent dark wordmark, an opaque photo, and a table of
/// markets on alternating row backgrounds with coloured changes.
fn brief_html() -> String {
    let wordmark = png_data(&image::RgbaImage::from_fn(180, 28, |x, y| {
        let letter = (x / 14) % 2 == 0 && x < 168 && (4..24).contains(&y);
        let bar = y >= 25 && x < 168;
        if letter || bar { image::Rgba([0x14, 0x1a, 0x2e, 255]) } else { image::Rgba([0, 0, 0, 0]) }
    }));
    let photo = png_data(&image::RgbaImage::from_fn(560, 200, |x, y| {
        let (fx, fy) = (x as f32 / 560., y as f32 / 200.);
        let ridge = 0.55 + 0.12 * (fx * 9.).sin() + 0.06 * (fx * 23.).cos();
        let sun = (fx - 0.75).powi(2) * 4. + (fy - 0.3).powi(2) * 14. < 0.04;
        let pixel = if sun {
            [255, 214, 120]
        } else if fy > ridge {
            [(40. + 30. * fy) as u8, (70. + 40. * fy) as u8, (50. + 20. * fx) as u8]
        } else {
            [(250. - 90. * fy) as u8, (170. + 40. * fy) as u8, (120. + 100. * fy) as u8]
        };
        image::Rgba([pixel[0], pixel[1], pixel[2], 255])
    }));
    let row = |name: &str, value: &str, change: &str, up: bool, shade: bool| {
        let color = if up { "#2b8a3e" } else { "#c92a2a" };
        let background = if shade { " style=\"background:#f8f9fa\"" } else { "" };
        format!(
            "<tr{background}><td style=\"padding:6px 8px\">{name}</td><td align=\"right\" style=\"padding:6px 8px\">{value}</td>\
             <td align=\"right\" style=\"padding:6px 8px;color:{color}\">{change}</td></tr>"
        )
    };
    format!(
        "<div style=\"max-width:560px;font-family:Helvetica,Arial,sans-serif;color:#222222\">\
         <p><img src=\"{wordmark}\" width=\"180\" height=\"28\" alt=\"The Morning Brief\"></p>\
         <img src=\"{photo}\" width=\"560\" height=\"200\" alt=\"Sunrise over the ridge\" style=\"display:block;width:100%;height:auto\">\
         <h2 style=\"margin:16px 0 4px\">Good morning. Here's Tuesday.</h2>\
         <p style=\"color:#555555\">Markets, weather and five things to know today.</p>\
         <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"border-collapse:collapse;border-top:2px solid #222222\">\
         <tr style=\"background:#e9ecef\"><th align=\"left\" style=\"padding:6px 8px\">Market</th><th align=\"right\" style=\"padding:6px 8px\">Close</th>\
         <th align=\"right\" style=\"padding:6px 8px\">Change</th></tr>{}{}{}{}</table>\
         <p>Weather: <span style=\"background:#fff3bf;padding:0 4px\">sunny, 21°C</span> in the city, light wind.</p>\
         <p style=\"font-size:12px;color:#999999\">The Morning Brief · <a href=\"https://morningbrief.example/unsubscribe\">Unsubscribe</a></p></div>",
        row("S&amp;P 500", "6,012.4", "+0.8%", true, false),
        row("Nasdaq", "19,880.1", "+1.2%", true, true),
        row("Oil (Brent)", "71.30", "−1.4%", false, false),
        row("EUR/USD", "1.0842", "−0.2%", false, true),
    )
}

/// A postcard painted on a paper texture, dark ink on it: its page is an image.
fn card_html() -> String {
    let paper = png_data(&image::RgbaImage::from_fn(64, 64, |x, y| {
        let grain = ((x * 7 + y * 13) % 11) as u8;
        image::Rgba([246 - grain, 238 - grain, 222 - grain, 255])
    }));
    format!(
        "<body background=\"{paper}\" style=\"font-family:Georgia,serif;color:#3d2b1f\">\
         <div style=\"padding:24px\"><h2 style=\"color:#7a3e1d;margin-top:0\">Greetings from Lisbon!</h2>\
         <p>The trams, the tiles, the pastéis de nata. We walked up to the castle at sunset and the whole city turned gold.</p>\
         <p>Wish you were here.</p><p style=\"font-style:italic\">— Priya</p></div></body>"
    )
}

/// `image` as a PNG `data:` URL.
fn png_data(image: &image::RgbaImage) -> String {
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).expect("a PNG encodes");
    format!("data:image/png;base64,{}", base64(png.get_ref()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &byte)| n | (byte as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
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
        (2, 72., "Looks great. Can we try the **larger** avatars, *maybe* 48 px?"),
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
    let mut planning = event("Sprint planning", (0, 11, 0), 90., Some("organizer"));
    planning["location"] = json!("Room 2A");
    planning["link"] = json!("https://calendar.google.com/calendar/event?eid=planning");
    planning["notes"] = json!(
        "Pick the sprint's work from the board: https://board.example.com/sprint-24\n\n\
         Bring estimates for your tickets. Can't make it? Tell jordan.park@northwind.example."
    );
    planning["attendees"] = json!([
        { "name": ME, "email": GMAIL, "response": "accepted" },
        { "name": JORDAN.0, "email": JORDAN.1, "response": "accepted" },
        { "name": DEVON.0, "email": DEVON.1, "response": "tentative" },
        { "name": PRIYA.0, "email": PRIYA.1, "response": "accepted" },
        { "name": RILEY.0, "email": RILEY.1, "response": "declined" },
        { "name": TAYLOR.0, "email": TAYLOR.1, "response": "needsAction" },
    ]);
    events.extend([
        event("1:1 with Jordan", (-1, 14, 0), 30., Some("accepted")),
        planning,
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
        "link": "https://calendar.google.com/calendar/event?eid=design-review",
        "notes": "We'll walk through the new onboarding flow and settle the open questions.\n\n\
                  Mockups: https://www.figma.com/file/onboarding-v3\n\
                  Spec: https://docs.example.com/onboarding-spec\n\n\
                  1. Welcome screen copy\n2. Account linking\n3. Empty states\n\n\
                  Questions before then to morgan.lee@northwind.example.",
    });
    review["attendees"] = json!([
        { "name": MORGAN.0, "email": MORGAN.1, "response": "accepted" },
        { "name": ME, "email": GMAIL, "response": "needsAction" },
        { "name": ALEX.0, "email": ALEX.1, "response": "accepted" },
        { "name": PRIYA.0, "email": PRIYA.1, "response": "tentative" },
        { "name": XIAOMING.0, "email": XIAOMING.1, "response": "accepted" },
        { "name": DANA.0, "email": DANA.1, "response": "declined" },
        { "name": EVAN.0, "email": EVAN.1, "response": "needsAction" },
    ]);
    events.push(review);
    // Jordan's calendar, shared with Sam read-only (MS-302): Jordan's own meetings, one of them
    // with Sam (Jordan's answer, not Sam's), in Jordan's color.
    let jordan = |id: &str, title: &str, (day, hour, minute): (i64, u32, u32), minutes: f64| {
        let start = at(day, hour, minute);
        json!({ "id": id, "calendar": JORDAN.1, "title": title, "start": start, "end": start + minutes * 60.,
                "organizer": person(JORDAN), "mine": true })
    };
    let mut hiring = jordan("j3", "Hiring sync", (1, 15, 0), 45.);
    hiring["response"] = json!("organizer");
    hiring["attendees"] = json!([
        { "name": JORDAN.0, "email": JORDAN.1, "response": "accepted" },
        { "name": PRIYA.0, "email": PRIYA.1, "response": "needsAction" },
    ]);
    let jordans = vec![
        jordan("j1", "Focus time", (0, 13, 30), 90.),
        jordan("j2", "Dentist", (2, 9, 0), 60.),
        hiring,
        jordan("j4", "Roadmap draft", (-1, 10, 0), 120.),
    ];
    let personal = vec![
        json!({ "id": "p1", "calendar": "personal", "title": "Evening run", "start": at(-1, 17, 30), "end": at(-1, 18, 15), "mine": true }),
        json!({ "id": "p2", "calendar": "personal", "title": "Book club", "start": at(1, 18, 0), "end": at(1, 19, 0), "mine": true }),
    ];
    let team = vec![json!({
        "id": "t1", "calendar": "design-team", "title": "Design critique", "start": at(1, 14, 0), "end": at(1, 14, 45),
        "organizer": person(DEVON), "mine": false,
    })];
    // A holiday calendar Sam subscribed to and left off, shown with its swatch checkbox.
    let holidays = vec![json!({
        "id": "h1", "calendar": "holidays", "title": "Founders' Day", "start": at(1, 0, 0), "end": at(2, 0, 0), "allDay": true,
        "startDay": day(1), "endDay": day(2),
    })];
    let part = |events: Vec<Value>| json!({ "from": day(-7), "until": day(61), "events": events });
    let copy = json!({
        "fetched": now(), "format": 2,
        "calendars": [
            { "id": "work", "name": "Work", "color": "#4285f4", "primary": true, "writable": true, "access": "owner",
              "selected": true, "shown": true },
            { "id": "personal", "name": "Personal", "color": "#33b679", "writable": true, "access": "owner",
              "selected": true, "shown": true },
            { "id": "design-team", "name": "Design team", "color": "#f4511e", "writable": true, "access": "writer",
              "selected": true, "shown": true },
            { "id": JORDAN.1, "name": JORDAN.0, "color": "#8e24aa", "access": "reader", "selected": true, "shown": true },
            { "id": "holidays", "name": "Holidays", "color": "#0b8043", "access": "reader", "shown": false },
        ],
        "parts": {
            "work": part(events),
            "personal": part(personal),
            "design-team": part(team),
            (JORDAN.1): part(jordans),
            "holidays": part(holidays),
        },
    });
    cache.save_calendar(&parse(copy), GMAIL);
    let outlook = json!({
        "fetched": now(), "format": 2,
        "calendars": [
            { "id": "calendar", "name": "Calendar", "color": "#e67c73", "primary": true, "writable": true,
              "access": "owner", "selected": true, "shown": true },
            { "id": DANA.1, "name": DANA.0, "color": "#7986cb", "access": "freeBusyReader", "selected": true, "shown": true },
        ],
        "parts": {
            "calendar": part(vec![
                json!({ "id": "o1", "calendar": "calendar", "title": "Budget review", "start": at(1, 13, 30), "end": at(1, 14, 30), "mine": true }),
                json!({ "id": "o2", "calendar": "calendar", "title": "Team offsite", "start": at(2, 0, 0), "end": at(4, 0, 0), "allDay": true,
                        "startDay": day(2), "endDay": day(4), "mine": true }),
            ]),
            (DANA.1): part(vec![
                json!({ "id": "d1", "calendar": DANA.1, "title": "Busy", "start": at(0, 16, 30), "end": at(0, 17, 15), "mine": true }),
            ]),
        },
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
