//! How every front end shows mail and chat (MS-235): the labels of dates in lists, readers and
//! chat logs, avatars' initials and color slots, the recipients a compose field holds, the body a
//! forward starts with and where a chat log's runs and day dividers fall. Front ends draw what
//! these answer instead of keeping rules of their own; subjects are in [`crate::text`].
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};

use crate::model::{Address, Date, unix_seconds};

// MARK: - Dates

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// Dates as the Swift engine's `ConversationRow.dateLabels` and `ReaderPage` write them for the
/// en_US locale (weeks start on Sunday, a narrow no-break space before AM/PM), relative to one
/// day: build one per answer, not per row.
pub struct DateLabels {
    today: NaiveDate,
}

impl DateLabels {
    pub fn now() -> Self {
        Self { today: Local::now().date_naive() }
    }

    pub fn at(today: NaiveDate) -> Self {
        Self { today }
    }

    /// A list row's date and date group (`labels(None)` is the group of undated rows).
    pub fn labels(&self, date: Option<Date>) -> (String, String) {
        let Some(moment) = date.and_then(local) else { return (String::new(), "Older".into()) };
        (self.list(&moment), self.section(&moment))
    }

    /// A list row's date: "4:05 PM" today, "Nov 7" this year, "11/7/2025" before.
    pub fn list(&self, moment: &DateTime<Local>) -> String {
        let day = moment.date_naive();
        if day == self.today {
            return time(moment);
        }
        if day.year() == self.today.year() {
            return short_day(day);
        }
        format!("{}/{}/{}", day.month(), day.day(), day.year())
    }

    /// A list's date group: Today, Yesterday, This Week, Last Week, then months.
    pub fn section(&self, moment: &DateTime<Local>) -> String {
        let day = moment.date_naive();
        let week = |date: NaiveDate| date - chrono::Days::new(u64::from(date.weekday().num_days_from_sunday()));
        if day == self.today {
            return "Today".into();
        }
        if self.today.pred_opt() == Some(day) {
            return "Yesterday".into();
        }
        if week(day) == week(self.today) {
            return "This Week".into();
        }
        if week(day) == week(self.today - chrono::Days::new(7)) {
            return "Last Week".into();
        }
        let month = MONTHS[day.month0() as usize];
        if day.year() == self.today.year() { month.into() } else { format!("{month} {}", day.year()) }
    }

    /// A message's date in the reader: "4:05 PM" today, "Nov 7, 4:05 PM" this year,
    /// "Nov 7, 2025, 4:05 PM" before.
    pub fn message(&self, moment: &DateTime<Local>) -> String {
        let day = moment.date_naive();
        if day == self.today {
            time(moment)
        } else if day.year() == self.today.year() {
            format!("{}, {}", short_day(day), time(moment))
        } else {
            format!("{}, {}, {}", short_day(day), day.year(), time(moment))
        }
    }

    /// A log's day divider: Today, Yesterday, "Wednesday, November 11" this year,
    /// "November 11, 2025" before.
    pub fn day(&self, moment: &DateTime<Local>) -> String {
        let day = moment.date_naive();
        if day == self.today {
            "Today".into()
        } else if self.today.pred_opt() == Some(day) {
            "Yesterday".into()
        } else if day.year() == self.today.year() {
            format!("{}, {} {}", WEEKDAYS[day.weekday().num_days_from_monday() as usize], MONTHS[day.month0() as usize], day.day())
        } else {
            format!("{} {}, {}", MONTHS[day.month0() as usize], day.day(), day.year())
        }
    }
}

/// The time of day: "4:05 PM".
pub fn time(moment: &DateTime<Local>) -> String {
    let hour = moment.hour() % 12;
    let half = if moment.hour() < 12 { "AM" } else { "PM" };
    format!("{}:{:02}\u{202F}{half}", if hour == 0 { 12 } else { hour }, moment.minute())
}

/// "Mon, Nov 2, 2026 at 11:41 AM", as Gmail dates replies' attributions and forwards.
pub fn full(moment: &DateTime<Local>) -> String {
    let day = moment.date_naive();
    let weekday = &WEEKDAYS[day.weekday().num_days_from_monday() as usize][..3];
    format!("{weekday}, {}, {} at {}", short_day(day), day.year(), time(moment))
}

/// "Nov 7".
fn short_day(day: NaiveDate) -> String {
    format!("{} {}", &MONTHS[day.month0() as usize][..3], day.day())
}

/// An engine date in local time.
pub fn local(date: Date) -> Option<DateTime<Local>> {
    let unix = unix_seconds(date);
    let seconds = unix.floor();
    let nanos = (((unix - seconds) * 1e9) as u32).min(999_999_999);
    Local.timestamp_opt(seconds as i64, nanos).earliest()
}

/// An ISO 8601 date as commands answer them (`2026-11-07T23:05:07Z`), in local time.
pub fn parse_iso(date: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(date).ok().map(|date| date.with_timezone(&Local))
}

// MARK: - Avatars

/// The colors avatars take: front ends' avatar palettes hold this many, and [`Avatar::slot`]
/// picks one.
pub const AVATAR_SLOTS: u32 = 11;

/// A person's avatar where there is no picture: their initial in the color of their slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Avatar {
    pub initial: String,
    /// In `0..AVATAR_SLOTS`, the same on every launch.
    pub slot: u32,
}

impl Avatar {
    /// For `name` (else `key`) keyed on `key`: a lowercased address, else a Chat user id or the
    /// name itself.
    pub fn new(name: &str, key: &str) -> Self {
        let shown = if name.trim().is_empty() { key } else { name };
        let initial = shown.chars().find(|c| c.is_alphanumeric()).map_or_else(|| "?".into(), |c| c.to_uppercase().collect());
        let key = if key.is_empty() { name } else { key };
        Self { initial, slot: stable_slot(&key.to_lowercase(), AVATAR_SLOTS) }
    }

    /// For an address: keyed on the address, its name's initial.
    pub fn of(address: &Address) -> Self {
        Self::new(address.name.as_deref().unwrap_or_default(), &address.email)
    }
}

/// A slot in `0..count` for `key`, the same on every launch and in the native app (FNV-1a 64
/// over UTF-8).
pub fn stable_slot(key: &str, count: u32) -> u32 {
    let mut hash: u64 = 1_469_598_103_934_665_603;
    for byte in key.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211);
    }
    (hash % u64::from(count.max(1))) as u32
}

// MARK: - Recipients

/// A recipient field's text split at commas and semicolons outside quotes and angle brackets;
/// the last part is what is being typed.
pub fn split_recipients(text: &str) -> Vec<&str> {
    let (mut parts, mut start, mut quoted, mut angle) = (Vec::new(), 0, false, false);
    for (index, character) in text.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A field's recipients as written (`Name <address>` or a bare address), or the first part that
/// is not an address.
pub fn parse_recipients(text: &str) -> Result<Vec<String>, String> {
    split_recipients(text)
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| match Address::parse(part) {
            Some(address) if valid_email(&address.email) => Ok(part.to_string()),
            _ => Err(part.to_string()),
        })
        .collect()
}

/// Whether `email` can be sent to: something before the last `@`, a dotted domain, nothing that
/// separates recipients.
pub fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"'))
}

/// `Name <address>`, or the bare address when there is no other name.
fn written(address: &Address) -> String {
    match address.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && !name.eq_ignore_ascii_case(&address.email) => format!("{name} <{}>", address.email),
        _ => address.email.clone(),
    }
}

// MARK: - Forwards

/// What a forward of a message quotes.
pub struct Forwarded<'a> {
    pub from: &'a [Address],
    pub to: &'a [Address],
    pub cc: &'a [Address],
    pub date: Option<DateTime<Local>>,
    pub subject: &'a str,
    pub text: &'a str,
}

/// The body a forward starts with, as Gmail writes it: a forwarded-message header, then the
/// message's text.
pub fn forward_body(message: &Forwarded<'_>) -> String {
    let list = |addresses: &[Address]| addresses.iter().map(written).collect::<Vec<_>>().join(", ");
    let mut header = vec!["---------- Forwarded message ---------".to_string(), format!("From: {}", list(message.from))];
    if let Some(date) = &message.date {
        header.push(format!("Date: {}", full(date)));
    }
    header.push(format!("Subject: {}", message.subject));
    header.push(format!("To: {}", list(message.to)));
    if !message.cc.is_empty() {
        header.push(format!("Cc: {}", list(message.cc)));
    }
    let header = header.join("\n");
    let text = message.text.trim();
    if text.is_empty() { header } else { format!("{header}\n\n{text}") }
}

// MARK: - Chat logs

/// A sender's next message within this many seconds, on the same day, continues their run.
pub const RUN_SECONDS: i64 = 3600;

/// One entry of a chat log, oldest first.
pub struct RunEntry<'a> {
    pub sender: &'a str,
    pub date: Option<DateTime<Local>>,
    /// Starts a run whoever sent the entry before it (the first unread message under a "New" line).
    pub opens_run: bool,
}

/// Where an entry stands in its log (`ReaderPage.chatRuns`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The day divider above it; empty on the day of the entry before.
    pub divider: String,
    /// It continues the run above: same sender, same day, within [`RUN_SECONDS`].
    pub continued: bool,
}

/// Each entry's place: an entry without a date opens no day and breaks the run.
pub fn chat_runs(entries: &[RunEntry<'_>], labels: &DateLabels) -> Vec<Run> {
    let mut runs = Vec::with_capacity(entries.len());
    let mut previous: Option<&RunEntry<'_>> = None;
    for entry in entries {
        let last = previous.and_then(|previous| previous.date);
        let same_day = matches!((entry.date, last), (Some(day), Some(last)) if day.date_naive() == last.date_naive());
        let close = matches!((entry.date, last), (Some(day), Some(last)) if (day - last).num_seconds() < RUN_SECONDS);
        let continued = same_day && close && !entry.opens_run && previous.is_some_and(|previous| previous.sender == entry.sender);
        let divider = if same_day { String::new() } else { entry.date.map(|date| labels.day(&date)).unwrap_or_default() };
        runs.push(Run { divider, continued });
        previous = Some(entry);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::date_from_unix;

    fn at(day: NaiveDate, hour: u32, minute: u32) -> DateTime<Local> {
        Local.from_local_datetime(&day.and_hms_opt(hour, minute, 0).unwrap()).earliest().unwrap()
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn labels_dates_like_the_swift_engine() {
        // A Wednesday.
        let today = day(2026, 11, 11);
        let labels = DateLabels::at(today);
        let date = |day: NaiveDate, hour: u32| Some(date_from_unix(at(day, hour, 5).timestamp() as f64));
        assert_eq!(labels.labels(date(today, 16)), ("4:05\u{202F}PM".into(), "Today".into()));
        assert_eq!(labels.labels(date(today, 0)).0, "12:05\u{202F}AM");
        assert_eq!(labels.labels(date(today.pred_opt().unwrap(), 9)), ("Nov 10".into(), "Yesterday".into()));
        assert_eq!(labels.labels(date(day(2026, 11, 8), 9)).1, "This Week");
        assert_eq!(labels.labels(date(day(2026, 11, 7), 9)).1, "Last Week");
        assert_eq!(labels.labels(date(day(2026, 10, 31), 9)).1, "October");
        assert_eq!(labels.labels(date(day(2025, 3, 4), 9)), ("3/4/2025".into(), "March 2025".into()));
        assert_eq!(labels.labels(None), (String::new(), "Older".into()));

        assert_eq!(labels.message(&at(today, 9, 30)), "9:30\u{202F}AM");
        assert_eq!(labels.message(&at(day(2026, 3, 4), 13, 0)), "Mar 4, 1:00\u{202F}PM");
        assert_eq!(labels.message(&at(day(2025, 3, 4), 13, 0)), "Mar 4, 2025, 1:00\u{202F}PM");
        assert_eq!(labels.day(&at(today, 9, 0)), "Today");
        assert_eq!(labels.day(&at(day(2026, 11, 10), 9, 0)), "Yesterday");
        assert_eq!(labels.day(&at(day(2026, 11, 6), 9, 0)), "Friday, November 6");
        assert_eq!(labels.day(&at(day(2025, 11, 6), 9, 0)), "November 6, 2025");
        assert_eq!(full(&at(day(2026, 11, 2), 11, 41)), "Mon, Nov 2, 2026 at 11:41\u{202F}AM");
        assert_eq!(parse_iso("2026-11-07T23:05:07Z").map(|date| date.timestamp()), Some(1_794_092_707));
    }

    #[test]
    fn avatars_take_an_initial_and_a_stable_slot() {
        let avatar = Avatar::new("  ", "Ann@X.com");
        assert_eq!(avatar.initial, "A");
        assert_eq!(avatar, Avatar::new("", "ann@x.com"));
        assert_eq!(Avatar::new("“Émile” Zola", "e@x.com").initial, "É");
        assert_eq!(Avatar::new("…", "").initial, "?");
        // The Swift app's FNV-1a slots for these keys.
        assert_eq!(stable_slot("ann@x.com", 11), stable_slot("ann@x.com", 11));
        assert_eq!(stable_slot("", 11), (1_469_598_103_934_665_603u64 % 11) as u32);
        assert_eq!(Avatar::of(&Address::new(Some("Bo".into()), "b@x.com")).initial, "B");
    }

    #[test]
    fn recipients_split_outside_quotes_and_angles() {
        assert_eq!(split_recipients("\"Doe, Jane\" <j@x.com>; a@b.co, "), ["\"Doe, Jane\" <j@x.com>", " a@b.co", " "]);
        assert_eq!(parse_recipients("\"Doe, Jane\" <j@x.com>, a@b.co"), Ok(vec!["\"Doe, Jane\" <j@x.com>".into(), "a@b.co".into()]));
        assert_eq!(parse_recipients("a@b.co, Jane"), Err("Jane".into()));
        assert_eq!(parse_recipients("x@y."), Err("x@y.".into()));
        assert_eq!(parse_recipients(" , "), Ok(vec![]));
    }

    #[test]
    fn forwards_start_with_a_header() {
        let from = [Address::new(Some("Ann".into()), "ann@x.com")];
        let to = [Address::new(None, "me@x.com"), Address::new(Some("bo@x.com".into()), "bo@x.com")];
        let date = Some(at(day(2026, 11, 2), 11, 41));
        let body = forward_body(&Forwarded { from: &from, to: &to, cc: &[], date, subject: "TR: plan", text: "  Hi\n" });
        assert_eq!(
            body,
            "---------- Forwarded message ---------\nFrom: Ann <ann@x.com>\nDate: Mon, Nov 2, 2026 at 11:41\u{202F}AM\n\
             Subject: TR: plan\nTo: me@x.com, bo@x.com\n\nHi"
        );
    }

    #[test]
    fn chat_runs_break_on_sender_day_and_hour() {
        let labels = DateLabels::at(day(2026, 11, 11));
        let entry = |sender, date, opens_run| RunEntry { sender, date, opens_run };
        let runs = chat_runs(
            &[
                entry("a", Some(at(day(2026, 11, 10), 23, 0)), false),
                entry("a", Some(at(day(2026, 11, 11), 0, 10)), false),
                entry("a", Some(at(day(2026, 11, 11), 0, 50)), false),
                entry("b", Some(at(day(2026, 11, 11), 0, 51)), false),
                entry("b", Some(at(day(2026, 11, 11), 2, 0)), false),
                entry("b", Some(at(day(2026, 11, 11), 2, 1)), true),
                entry("b", None, false),
            ],
            &labels,
        );
        let shown: Vec<(&str, bool)> = runs.iter().map(|run| (run.divider.as_str(), run.continued)).collect();
        assert_eq!(
            shown,
            [("Yesterday", false), ("Today", false), ("", true), ("", false), ("", false), ("", false), ("", false)]
        );
    }
}
