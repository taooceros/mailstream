//! Mail as the Swift engine models it (`Sources/MailstreamKit/Model/Models.swift`): addresses,
//! mailboxes, envelopes, conversations and message content. What mail.db and the Gmail envelope
//! cache keep is these values in the JSON the Swift engine's `JSONEncoder` writes (dates as
//! seconds since 2001, absent optionals left out), so either engine reads what the other wrote.
use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A point in time as Foundation's `Date` holds it: seconds since 2001-01-01 UTC, which is also
/// how `JSONEncoder` writes one.
pub type Date = f64;

/// Seconds from 1970 to 2001, as Foundation converts.
const SINCE_1970: f64 = 978_307_200.0;
/// `Date.distantPast`: undated mail sorts as the oldest.
pub const DISTANT_PAST: Date = -63_114_076_800.0;
/// `Date.distantFuture`.
pub const DISTANT_FUTURE: Date = 63_113_904_000.0;

pub fn date_from_unix(seconds: f64) -> Date {
    seconds - SINCE_1970
}

pub fn unix_seconds(date: Date) -> f64 {
    date + SINCE_1970
}

pub fn now() -> Date {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    date_from_unix(since.as_secs_f64())
}

/// `Date.formatted(.iso8601)`: `2026-11-07T23:05:07Z`, whole seconds in UTC.
pub fn iso8601(date: Date) -> String {
    let seconds = unix_seconds(date).floor() as i64;
    let (days, time) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", time / 3600, time % 3600 / 60, time % 60)
}

/// RFC 3339 as Graph writes it (`2026-11-07T23:05:07Z`, with or without fractional seconds or an
/// offset); nil for anything else.
pub fn parse_iso8601(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits.bytes().all(|byte| byte.is_ascii_digit()).then(|| digits.parse().ok())?
    };
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't') {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if bytes[13] != b':' || bytes[16] != b':' || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut rest = &text[19..];
    let mut fraction = 0.0;
    if let Some(after) = rest.strip_prefix('.') {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        fraction = format!("0.{}", &after[..digits]).parse().ok()?;
        rest = &after[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && matches!(&rest[..1], "+" | "-") && &rest[3..4] == ":" => {
            let minutes = rest[1..3].parse::<i64>().ok()? * 60 + rest[4..6].parse::<i64>().ok()?;
            if rest.starts_with('-') { -minutes } else { minutes }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset * 60;
    Some(date_from_unix(seconds as f64 + fraction))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(name: Option<String>, email: impl Into<String>) -> Self {
        Self { name, email: email.into() }
    }

    /// The display name when there is one, the address otherwise.
    pub fn display_name(&self) -> String {
        if let Some(name) = self.name.as_deref().map(str::trim)
            && !name.is_empty()
            && !name.eq_ignore_ascii_case(&self.email)
            && name.to_lowercase() != self.email.to_lowercase()
        {
            return name.trim_matches(|c| c == '"' || c == '\'').to_string();
        }
        self.email.clone()
    }

    pub fn normalized_email(&self) -> String {
        self.email.to_lowercase()
    }

    /// RFC 5322 mailbox form: `"Name" <address>`.
    pub fn rfc5322(&self) -> String {
        match self.name.as_deref().map(|name| name.trim_matches(' ')) {
            Some(name) if !name.is_empty() && name != self.email => {
                format!("\"{}\" <{}>", name.replace('\\', "\\\\").replace('"', "\\\""), self.email)
            }
            _ => self.email.clone(),
        }
    }

    /// `Name <address>`, or the bare address, as commands answer senders and recipients.
    pub fn text(&self) -> String {
        match &self.name {
            Some(name) => format!("{name} <{}>", self.email),
            None => self.email.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailboxRole {
    Inbox,
    Starred,
    Important,
    Sent,
    Drafts,
    Archive,
    All,
    Junk,
    Trash,
    Other,
}

impl MailboxRole {
    pub const ALL: [Self; 10] = [
        Self::Inbox,
        Self::Starred,
        Self::Important,
        Self::Sent,
        Self::Drafts,
        Self::Archive,
        Self::All,
        Self::Junk,
        Self::Trash,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Starred => "starred",
            Self::Important => "important",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Junk => "junk",
            Self::Trash => "trash",
            Self::Other => "other",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }

    pub fn sort_order(self) -> usize {
        self as usize
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Starred => "Starred",
            Self::Important => "Important",
            Self::Sent => "Sent",
            Self::Drafts => "Drafts",
            Self::Archive => "Archive",
            Self::All => "All Mail",
            Self::Junk => "Spam",
            Self::Trash => "Trash",
            Self::Other => "Folder",
        }
    }
}

/// A signed-in account as the mail store and mail.db's `accounts` keep it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The account record's id: the lowercased address.
    pub name: String,
    pub label: String,
    pub provider: crate::Provider,
    /// The address and name the account sends as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Address>,
}

impl Account {
    pub fn email(&self) -> &str {
        self.identity.as_ref().map_or(&self.name, |identity| &identity.email)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mailbox {
    pub account: String,
    pub id: String,
    pub name: String,
    pub role: MailboxRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

impl Mailbox {
    pub fn key(&self) -> MailboxKey {
        MailboxKey::new(&self.account, &self.id)
    }

    pub fn display_name(&self) -> &str {
        if self.role == MailboxRole::Other { &self.name } else { self.role.title() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxKey {
    pub account: String,
    pub mailbox: String,
}

impl MailboxKey {
    pub fn new(account: &str, mailbox: &str) -> Self {
        Self { account: account.into(), mailbox: mailbox.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    /// The provider's thread: Gmail's thread id, Graph's conversation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(rename = "messageID", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(rename = "inReplyTo")]
    pub in_reply_to: Vec<String>,
    pub flags: BTreeSet<String>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Date>,
    pub size: i64,
    #[serde(rename = "hasAttachment", default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
}

impl Envelope {
    pub fn is_seen(&self) -> bool {
        self.flags.contains("seen")
    }

    pub fn is_flagged(&self) -> bool {
        self.flags.contains("flagged")
    }

    pub fn is_draft(&self) -> bool {
        self.flags.contains("draft")
    }

    /// The flags of a message the provider reports as read, flagged and a draft.
    pub fn flags(seen: bool, flagged: bool, draft: bool) -> BTreeSet<String> {
        [(seen, "seen"), (flagged, "flagged"), (draft, "draft")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, flag)| flag.to_string())
            .collect()
    }
}

/// Where a message lists: its account, mailbox and provider id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub account: String,
    pub mailbox: String,
    pub id: String,
}

impl MessageRef {
    pub fn key(&self) -> MailboxKey {
        MailboxKey::new(&self.account, &self.mailbox)
    }
}

/// One message as a conversation holds it: its listing's mailbox, its envelope (shared with the
/// listing it came from) and the mailbox's role.
#[derive(Clone, Debug)]
pub struct MailMessage {
    pub key: Arc<MailboxKey>,
    pub envelope: Arc<Envelope>,
    pub role: MailboxRole,
}

impl MailMessage {
    pub fn account(&self) -> &str {
        &self.key.account
    }

    pub fn mailbox(&self) -> &str {
        &self.key.mailbox
    }

    pub fn id(&self) -> &str {
        &self.envelope.id
    }

    pub fn message_ref(&self) -> MessageRef {
        MessageRef { account: self.key.account.clone(), mailbox: self.key.mailbox.clone(), id: self.envelope.id.clone() }
    }

    /// Written by the user: a draft, sent mail, or from one of `own` (lowercased addresses).
    pub fn is_from_user(&self, own: &std::collections::HashSet<String>) -> bool {
        self.envelope.is_draft()
            || self.role == MailboxRole::Sent
            || self.envelope.from.iter().any(|address| own.contains(&address.normalized_email()))
    }
}

/// One thread, with what lists sort and filter by worked out when it is built.
#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: String,
    pub account: String,
    /// Ascending by date.
    pub messages: Vec<MailMessage>,
    pub latest_date: Option<Date>,
    pub subject: String,
    pub is_unread: bool,
    pub is_flagged: bool,
    pub has_attachment: bool,
    pub has_draft: bool,
}

impl Conversation {
    pub fn new(id: String, account: String, messages: Vec<MailMessage>) -> Self {
        let mut latest_date: Option<Date> = None;
        let (mut unread, mut flagged, mut attachment, mut draft) = (false, false, false, false);
        for message in &messages {
            let envelope = &message.envelope;
            if let Some(date) = envelope.date
                && date > latest_date.unwrap_or(DISTANT_PAST)
            {
                latest_date = Some(date);
            }
            unread |= !envelope.is_seen() && message.role != MailboxRole::Sent;
            flagged |= envelope.is_flagged();
            attachment |= envelope.has_attachment == Some(true);
            draft |= envelope.is_draft();
        }
        let subject = messages.iter().find(|message| !message.envelope.subject.is_empty());
        let subject = subject.map_or_else(|| "(No Subject)".to_string(), |message| message.envelope.subject.clone());
        Self {
            id,
            account,
            messages,
            latest_date,
            subject,
            is_unread: unread,
            is_flagged: flagged,
            has_attachment: attachment,
            has_draft: draft,
        }
    }

    /// The thread of `messages` in date order (ties by provider id), named after its first
    /// message so the id stays put as later messages join.
    pub fn thread(mut messages: Vec<MailMessage>) -> Self {
        messages.sort_by(|a, b| {
            let (da, db) = (a.envelope.date.unwrap_or(DISTANT_PAST), b.envelope.date.unwrap_or(DISTANT_PAST));
            da.total_cmp(&db).then_with(|| a.id().cmp(b.id()))
        });
        let first = &messages[0];
        let id = format!("{}|{}", first.account(), first.envelope.message_id.as_deref().unwrap_or(first.id()));
        let account = first.account().to_string();
        Self::new(id, account, messages)
    }

    pub fn latest(&self) -> &MailMessage {
        &self.messages[self.messages.len() - 1]
    }

    /// The latest message the user did not write; nil when they wrote them all.
    pub fn latest_incoming(&self, own: &std::collections::HashSet<String>) -> Option<&MailMessage> {
        self.messages.iter().rev().find(|message| !message.is_from_user(own))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub part: i64,
    pub filename: String,
    pub mime: String,
    pub size: i64,
    #[serde(rename = "contentID", default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    pub inline: bool,
    /// File name inside the message's cache directory.
    #[serde(rename = "storedName")]
    pub stored_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageContent {
    /// The HTML body with `cid:` references pointing at the cached parts; nil for text-only mail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub cc: Vec<Address>,
    #[serde(rename = "replyTo")]
    pub reply_to: Vec<Address>,
    pub references: Vec<String>,
    pub attachments: Vec<Attachment>,
    pub snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_dates_like_foundation() {
        let date = parse_iso8601("2026-11-07T23:05:07.683Z").unwrap();
        assert!((unix_seconds(date) - 1_794_092_707.683).abs() < 1e-6);
        assert_eq!(iso8601(date), "2026-11-07T23:05:07Z");
        assert_eq!(parse_iso8601("2024-02-29T01:00:00+01:00").map(iso8601).as_deref(), Some("2024-02-29T00:00:00Z"));
        assert_eq!(iso8601(date_from_unix(0.0)), "1970-01-01T00:00:00Z");
        assert_eq!(parse_iso8601("yesterday"), None);
    }

    #[test]
    fn encodes_envelopes_as_swift_does() {
        let envelope = Envelope {
            id: "m1".into(),
            thread: Some("t1".into()),
            message_id: None,
            in_reply_to: vec![],
            flags: Envelope::flags(true, false, false),
            subject: "Hi".into(),
            from: vec![Address::new(Some("A".into()), "a@x.com")],
            to: vec![],
            date: Some(752_000_000.5),
            size: 0,
            has_attachment: Some(false),
        };
        let text = String::from_utf8(crate::json::encode(&envelope, false, true)).unwrap();
        assert_eq!(
            text,
            r#"{"id":"m1","thread":"t1","inReplyTo":[],"flags":["seen"],"subject":"Hi","from":[{"name":"A","email":"a@x.com"}],"to":[],"date":752000000.5,"size":0,"hasAttachment":false}"#
        );
        let swift = r#"{"to":[],"size":12,"flags":[],"subject":"S","from":[],"id":"m2","inReplyTo":["p"],"messageID":"x@y","date":700000000}"#;
        let decoded: Envelope = serde_json::from_str(swift).unwrap();
        assert_eq!((decoded.message_id.as_deref(), decoded.date, decoded.has_attachment), (Some("x@y"), Some(7e8), None));
    }

    #[test]
    fn displays_names_as_swift_does() {
        assert_eq!(Address::new(Some(" \"Ann\" ".into()), "a@x.com").display_name(), "Ann");
        assert_eq!(Address::new(Some("A@X.com".into()), "a@x.com").display_name(), "a@x.com");
        assert_eq!(Address::new(None, "a@x.com").text(), "a@x.com");
    }
}
