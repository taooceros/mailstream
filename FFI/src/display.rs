//! The engine's display rules (`mailstream_engine::display`, `text`, `chat_markup`; MS-235) for
//! front ends: what dates, subjects, avatars, recipient fields, forwards and chat messages read
//! as. Lists come with theirs filled in ([`Conversation::when`], [`ChatSpace::avatar`], …), so a
//! front end never asks per row while it scrolls; the functions here are for single values (a
//! reply's subject, a field being typed in, a message sent from here).
use std::collections::HashMap;

use mailstream_engine::chat_markup::{self, ChatBlock};
use mailstream_engine::display::{self, Avatar, ChatRun, DateLabels, Forwarded, RunEntry};
use mailstream_engine::model::Address;
use mailstream_engine::text::{self, Subject};

/// A subject as lists show it: leading `[list]` tags apart (empty when none), and the title
/// without reply and forward prefixes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SubjectParts {
    pub tag: String,
    pub title: String,
}

/// A recipient field read: its recipients as written, or the first part that is no address.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Recipients {
    pub recipients: Vec<String>,
    pub invalid: Option<String>,
}

/// One entry of a chat log, oldest first, for [`chat_runs`].
#[derive(Clone, Debug, uniffi::Record)]
pub struct ChatRunEntry {
    /// Who sent it: a `users/…` id, or anything that is the same for the same person.
    pub sender: String,
    /// ISO 8601.
    pub date: Option<String>,
    /// It starts a run whoever sent the entry before (the first unread one, under a "New" line).
    pub opens_run: bool,
}

fn local(date: Option<&str>) -> Option<chrono::DateTime<chrono::Local>> {
    date.and_then(display::parse_iso)
}

// MARK: - Functions

/// A list row's date (ISO 8601): "4:05 PM" today, "Nov 7" this year, "11/7/2025" before.
#[uniffi::export]
pub fn list_date(date: Option<String>) -> String {
    local(date.as_deref()).map(|date| DateLabels::now().list(&date)).unwrap_or_default()
}

/// A list's date group: Today, Yesterday, This Week, Last Week, then months; Older undated.
#[uniffi::export]
pub fn list_section(date: Option<String>) -> String {
    local(date.as_deref()).map_or_else(|| "Older".into(), |date| DateLabels::now().section(&date))
}

/// A message's date in the reader: "4:05 PM" today, "Nov 7, 4:05 PM" this year, with the year
/// before.
#[uniffi::export]
pub fn message_date(date: Option<String>) -> String {
    local(date.as_deref()).map(|date| DateLabels::now().message(&date)).unwrap_or_default()
}

/// The time of day: "4:05 PM".
#[uniffi::export]
pub fn message_time(date: Option<String>) -> String {
    local(date.as_deref()).map(|date| display::time(&date)).unwrap_or_default()
}

/// A message's full date, as its headers show it: "Mon, Nov 2, 2026 at 11:41 AM".
#[uniffi::export]
pub fn full_date(date: Option<String>) -> String {
    local(date.as_deref()).map(|date| display::full(&date)).unwrap_or_default()
}

/// A log's day divider: Today, Yesterday, "Wednesday, November 11", "November 11, 2025".
#[uniffi::export]
pub fn day_divider(date: Option<String>) -> String {
    local(date.as_deref()).map(|date| DateLabels::now().day(&date)).unwrap_or_default()
}

#[uniffi::export]
pub fn split_subject(subject: String) -> SubjectParts {
    let (tag, title) = Subject::list_parts(&subject);
    SubjectParts { tag, title }
}

/// A reply's subject: `Re: subject`, unless it already is a reply in any language.
#[uniffi::export]
pub fn reply_subject(subject: String) -> String {
    text::reply_subject(&subject)
}

/// A forward's subject: `Fwd: subject`, unless it already is a forward (`Fwd:`, `TR:`, `WG:`,
/// `转发：`, …).
#[uniffi::export]
pub fn forward_subject(subject: String) -> String {
    text::forward_subject(&subject)
}

/// The body a forward of a message starts with: a forwarded-message header (the date ISO 8601),
/// then `text`.
#[uniffi::export]
pub fn forward_body(
    from: Vec<Address>,
    to: Vec<Address>,
    cc: Vec<Address>,
    date: Option<String>,
    subject: String,
    text: String,
) -> String {
    display::forward_body(&Forwarded { from: &from, to: &to, cc: &cc, date: local(date.as_deref()), subject: &subject, text: &text })
}

/// The avatar of someone named `name` (else shown by `key`), keyed on `key`: a lowercased
/// address, else a Chat user id or the name.
#[uniffi::export]
pub fn avatar(name: String, key: String) -> Avatar {
    Avatar::new(&name, &key)
}

/// A recipient field's text split at commas and semicolons outside quotes and angle brackets;
/// the last part is what is being typed.
#[uniffi::export]
pub fn split_recipients(text: String) -> Vec<String> {
    display::split_recipients(&text).into_iter().map(String::from).collect()
}

#[uniffi::export]
pub fn parse_recipients(text: String) -> Recipients {
    match display::parse_recipients(&text) {
        Ok(recipients) => Recipients { recipients, invalid: None },
        Err(invalid) => Recipients { recipients: Vec::new(), invalid: Some(invalid) },
    }
}

/// Each entry's place in a chat log: day dividers and runs.
#[uniffi::export]
pub fn chat_runs(entries: Vec<ChatRunEntry>) -> Vec<ChatRun> {
    let dates: Vec<_> = entries.iter().map(|entry| local(entry.date.as_deref())).collect();
    let entries: Vec<RunEntry<'_>> = entries
        .iter()
        .zip(dates)
        .map(|(entry, date)| RunEntry { sender: &entry.sender, date, opens_run: entry.opens_run })
        .collect();
    display::chat_runs(&entries, &DateLabels::now())
}

/// A chat message's text (its Markdown) laid out; `names` names mentioned `users/…`.
#[uniffi::export]
pub fn chat_body(markup: String, names: HashMap<String, String>) -> Vec<ChatBlock> {
    chat_markup::parse(&markup, &|id| names.get(id).cloned())
}
