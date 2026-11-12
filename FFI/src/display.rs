//! The engine's display rules (`mailstream_engine::display`, `text`, `chat_markup`; MS-235) for
//! front ends: what dates, subjects, avatars, recipient fields, forwards and chat messages read
//! as. Lists come with theirs filled in ([`Conversation::when`], [`ChatSpace::avatar`], …), so a
//! front end never asks per row while it scrolls; the functions here are for single values (a
//! reply's subject, a field being typed in, a message sent from here).
use std::collections::HashMap;

use mailstream_engine::chat_markup::{self, BlockKind};
use mailstream_engine::display::{self, DateLabels, Forwarded, RunEntry};
use mailstream_engine::model::Address;
use mailstream_engine::text::{self, Subject};

use crate::{ChatMessage, ChatPeople, ChatSpace, ChatSpaces, ChatThread, ChatUser, Conversation, Message, Person, Thread};

/// A person's avatar where there is no picture: their initial, in the color of `slot` (the
/// front end's avatar palette holds [`display::AVATAR_SLOTS`] colors).
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct Avatar {
    pub initial: String,
    pub slot: u32,
}

impl From<display::Avatar> for Avatar {
    fn from(avatar: display::Avatar) -> Self {
        Self { initial: avatar.initial, slot: avatar.slot }
    }
}

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

/// Where an entry stands in its log.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatRun {
    /// The day divider above it; empty on the day of the entry before.
    pub divider: String,
    /// It continues the run above (same sender, same day, within the hour): no head.
    pub continued: bool,
}

/// A run of a chat message's text and how it is drawn.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// A person mentioned; the text is `@Name`.
    pub mention: bool,
    /// Where it links to.
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChatBlockKind {
    Paragraph,
    /// `level` 1 to 6.
    Heading { level: u8 },
    /// Monospace, as written: one span.
    Code,
    Rule,
    /// Its rows, the header row first.
    Table,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatTableCell {
    pub spans: Vec<ChatSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatTableRow {
    pub cells: Vec<ChatTableCell>,
}

/// One block of a chat message, in order: blocks are drawn one below the other.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatBlock {
    pub kind: ChatBlockKind,
    pub spans: Vec<ChatSpan>,
    pub rows: Vec<ChatTableRow>,
    /// How many quotes it sits in (a bar each at its start).
    pub quote: u32,
    /// How many lists it sits in (an indent each).
    pub indent: u32,
    /// The first block of a list item carries the item's marker: `•` or `3.`.
    pub marker: Option<String>,
    /// In a tight list: a line's gap above it rather than a paragraph's.
    pub tight: bool,
}

fn span(span: chat_markup::Span) -> ChatSpan {
    let style = span.style;
    ChatSpan {
        text: span.text,
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        code: style.code,
        mention: style.mention,
        link: style.link,
    }
}

fn block(block: chat_markup::Block) -> ChatBlock {
    ChatBlock {
        kind: match block.kind {
            BlockKind::Paragraph => ChatBlockKind::Paragraph,
            BlockKind::Heading(level) => ChatBlockKind::Heading { level },
            BlockKind::Code => ChatBlockKind::Code,
            BlockKind::Rule => ChatBlockKind::Rule,
            BlockKind::Table => ChatBlockKind::Table,
        },
        spans: block.spans.into_iter().map(span).collect(),
        rows: block
            .rows
            .into_iter()
            .map(|row| ChatTableRow {
                cells: row.into_iter().map(|cell| ChatTableCell { spans: cell.into_iter().map(span).collect() }).collect(),
            })
            .collect(),
        quote: block.quote,
        indent: block.indent,
        marker: block.marker,
        tight: block.tight,
    }
}

fn local(date: Option<&str>) -> Option<chrono::DateTime<chrono::Local>> {
    date.and_then(display::parse_iso)
}

fn address(raw: &str) -> Address {
    Address::parse(raw).unwrap_or_else(|| Address::new(None, raw.trim()))
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

/// The body a forward of a message starts with: a forwarded-message header (addresses as
/// `Name <address>`, the date ISO 8601), then `text`.
#[uniffi::export]
pub fn forward_body(
    from: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
    date: Option<String>,
    subject: String,
    text: String,
) -> String {
    let list = |raw: &[String]| raw.iter().map(|raw| address(raw)).collect::<Vec<_>>();
    let (from, to, cc) = (list(&from), list(&to), list(&cc));
    display::forward_body(&Forwarded { from: &from, to: &to, cc: &cc, date: local(date.as_deref()), subject: &subject, text: &text })
}

/// The avatar of someone named `name` (else shown by `key`), keyed on `key`: a lowercased
/// address, else a Chat user id or the name.
#[uniffi::export]
pub fn avatar(name: String, key: String) -> Avatar {
    display::Avatar::new(&name, &key).into()
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
        .into_iter()
        .map(|run| ChatRun { divider: run.divider, continued: run.continued })
        .collect()
}

/// A chat message's text (its Markdown) laid out; `names` names mentioned `users/…`.
#[uniffi::export]
pub fn chat_body(markup: String, names: HashMap<String, String>) -> Vec<ChatBlock> {
    chat_markup::parse(&markup, &|id| names.get(id).cloned()).into_iter().map(block).collect()
}

// MARK: - Answers filled in
//
// The methods answering these fill them in; `dress` is public for Rust hosts that make values of
// their own (GPUI's synthetic chat).

impl Conversation {
    pub(crate) fn dress(&mut self, labels: &DateLabels) {
        let (tag, title) = Subject::list_parts(&self.subject);
        (self.tag, self.title) = (tag, title);
        let date = local(self.date.as_deref());
        self.when = date.map(|date| labels.list(&date)).unwrap_or_default();
        self.section = date.map_or_else(|| "Older".into(), |date| labels.section(&date));
        self.avatar = match (&self.from, self.participants.first()) {
            (Some(from), _) => display::Avatar::of(&address(from)),
            (None, Some(name)) => display::Avatar::new(name, name),
            (None, None) => display::Avatar::new("", "?"),
        }
        .into();
    }
}

impl crate::Listing {
    pub(crate) fn dress(&mut self) {
        let labels = DateLabels::now();
        for conversation in &mut self.conversations {
            conversation.dress(&labels);
        }
    }
}

impl Message {
    pub(crate) fn dress(&mut self, labels: &DateLabels) {
        let date = local(self.date.as_deref());
        self.when = date.map(|date| labels.message(&date)).unwrap_or_default();
        self.time = date.map(|date| display::time(&date)).unwrap_or_default();
        self.avatar = self.from.first().map(|from| display::Avatar::of(&address(from))).unwrap_or_else(|| display::Avatar::new("", "?")).into();
    }
}

impl Thread {
    pub(crate) fn dress(&mut self) {
        let labels = DateLabels::now();
        self.conversation.dress(&labels);
        let senders: Vec<String> =
            self.messages.iter().map(|message| message.from.first().map(|from| address(from).normalized_email()).unwrap_or_default()).collect();
        let dates: Vec<_> = self.messages.iter().map(|message| local(message.date.as_deref())).collect();
        let entries: Vec<RunEntry<'_>> =
            senders.iter().zip(&dates).map(|(sender, date)| RunEntry { sender, date: *date, opens_run: false }).collect();
        let runs = display::chat_runs(&entries, &labels);
        for (message, run) in self.messages.iter_mut().zip(runs) {
            message.dress(&labels);
            message.divider = run.divider;
        }
    }
}

impl ChatUser {
    pub fn dress(&mut self) {
        self.avatar = display::Avatar::new(&self.name, &self.avatar_key).into();
    }
}

impl ChatPeople {
    pub fn dress(&mut self) {
        for person in &mut self.people {
            person.dress();
        }
    }
}

impl ChatSpaces {
    pub fn dress(&mut self) {
        for space in &mut self.spaces {
            space.dress();
        }
    }
}

impl ChatSpace {
    fn dress(&mut self) {
        let key = if self.avatar_key.is_empty() { &self.space } else { &self.avatar_key };
        self.avatar = display::Avatar::new(&self.avatar_name, key).into();
    }
}

impl ChatThread {
    pub fn dress(&mut self) {
        // Mentions name members and anyone who wrote here.
        let mut names: HashMap<&str, &str> = self.members.iter().map(|user| (user.id.as_str(), user.name.as_str())).collect();
        for message in &self.messages {
            names.entry(message.sender.id.as_str()).or_insert(message.sender.name.as_str());
        }
        let names: HashMap<String, String> = names.into_iter().map(|(id, name)| (id.into(), name.into())).collect();
        for user in &mut self.members {
            user.dress();
        }
        let labels = DateLabels::now();
        let dates: Vec<_> = self.messages.iter().map(|message| local(message.created.as_deref())).collect();
        let entries: Vec<RunEntry<'_>> = self
            .messages
            .iter()
            .zip(&dates)
            .map(|(message, date)| RunEntry { sender: &message.sender.id, date: *date, opens_run: false })
            .collect();
        let runs = display::chat_runs(&entries, &labels);
        for ((message, run), date) in self.messages.iter_mut().zip(runs).zip(dates) {
            message.dress(&names, date.map(|date| display::time(&date)).unwrap_or_default(), run);
        }
    }
}

impl ChatMessage {
    fn dress(&mut self, names: &HashMap<String, String>, time: String, run: display::Run) {
        self.sender.dress();
        self.body = chat_markup::parse(&self.markup, &|id| names.get(id).cloned()).into_iter().map(block).collect();
        self.time = time;
        (self.divider, self.continued) = (run.divider, run.continued);
    }
}

impl Person {
    pub(crate) fn dress(&mut self) {
        self.avatar = display::Avatar::new(self.name.as_deref().unwrap_or_default(), &self.email).into();
    }
}
