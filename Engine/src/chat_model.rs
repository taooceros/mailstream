//! Google Chat values as the Swift engine keeps them (`Backend/Chat.swift`, `Store/ChatStore.swift`):
//! accounts, users, messages and spaces, in the JSON mail.db holds (dates as seconds since 2001,
//! absent optionals left out), so either engine reads what the other wrote; and what the list and
//! the commands show of them: rows with their date labels and previews, and message text without
//! Chat's markup (`ChatMarkup.plain` in `UI/ChatPage.swift`).
use std::sync::LazyLock;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::model::{DISTANT_PAST, Date, iso8601, unix_seconds};

/// A Google account with Chat access; `name` and `email` are both its address (the account id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatAccount {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// One space of one chat account.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatSpaceKey {
    pub account: String,
    pub space: String,
}

impl ChatSpaceKey {
    pub fn new(account: &str, space: &str) -> Self {
        Self { account: account.into(), space: space.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUser {
    /// `users/<id>`, stable across spaces.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

/// Trimmed of spaces and tabs, as Swift's `trimmingCharacters(in: .whitespaces)`.
fn blank(text: &str) -> bool {
    text.chars().all(|character| character.is_whitespace() && character != '\n' && character != '\r')
}

impl ChatUser {
    /// Display name, else address, else a generic label (Google withholds both for some people).
    pub fn label(&self) -> String {
        if let Some(name) = self.display_name.as_deref().filter(|name| !blank(name)) {
            return name.into();
        }
        if let Some(email) = self.email.as_deref().filter(|email| !email.is_empty()) {
            return email.into();
        }
        if self.bot { "App" } else { "Unknown" }.into()
    }

    /// First name, for previews ("Brent: …").
    pub fn short_label(&self) -> String {
        match self.display_name.as_deref().filter(|name| !name.is_empty()) {
            Some(name) => name.split(' ').find(|word| !word.is_empty()).unwrap_or(name).into(),
            None => self.label(),
        }
    }

    /// What avatars and avatar colors key on: the lowercased address, else the resource name.
    pub fn avatar_key(&self) -> String {
        self.email.as_ref().map_or_else(|| self.name.clone(), |email| email.to_lowercase())
    }

    /// As commands answer a user.
    pub fn json(&self) -> Value {
        json!({
            "id": self.name, "name": self.label(), "email": self.email, "avatarKey": self.avatar_key(),
            "avatarUrl": self.avatar_url, "bot": self.bot,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The uploaded file's `attachmentDataRef`, which `chat.attachment` downloads; nil for Drive
    /// files and for messages cached before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// `spaces/x/messages/y`.
    pub name: String,
    pub sender: ChatUser,
    pub created: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<Date>,
    /// Plain text with Google Chat markup (`*bold*`, `_italic_`, `` `code` ``, fences, `<url|label>`).
    #[serde(default)]
    pub text: String,
    /// `spaces/x/threads/z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// A reply inside a thread, not the thread's first message.
    #[serde(default)]
    pub thread_reply: bool,
    #[serde(default)]
    pub mine: bool,
    /// A bot card; `text` holds its fallback text or "".
    #[serde(default)]
    pub card: bool,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
}

impl ChatMessage {
    /// As `chat.messages` answers one.
    pub fn json(&self) -> Value {
        json!({
            "id": self.name, "sender": self.sender.json(), "created": iso8601(self.created),
            "edited": self.edited.map(iso8601), "text": markup_plain(&self.text), "markup": self.text,
            "thread": self.thread, "threadReply": self.thread_reply, "mine": self.mine, "card": self.card,
            "attachments": self.attachments.iter().map(|attachment| json!({
                "name": attachment.name, "url": attachment.url,
                "contentType": attachment.content_type.as_deref().filter(|kind| !kind.is_empty()),
                "resource": attachment.resource,
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatSpaceKind {
    Dm,
    Group,
    Space,
    Bot,
}

impl ChatSpaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dm => "dm",
            Self::Group => "group",
            Self::Space => "space",
            Self::Bot => "bot",
        }
    }
}

impl Serialize for ChatSpaceKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// An unknown kind reads as a space, as in the Swift engine.
impl<'de> Deserialize<'de> for ChatSpaceKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "dm" => Self::Dm,
            "group" => Self::Group,
            "bot" => Self::Bot,
            _ => Self::Space,
        })
    }
}

fn space_kind() -> ChatSpaceKind {
    ChatSpaceKind::Space
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSpace {
    /// `spaces/x`.
    pub name: String,
    #[serde(default = "space_kind")]
    pub kind: ChatSpaceKind,
    /// Never empty: the space's name, the DM peer, or the members' names.
    #[serde(default)]
    pub title: String,
    /// Other people (not the user) in a DM or group; empty for named spaces.
    #[serde(default)]
    pub members: Vec<ChatUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<Date>,
    /// Messages after `last_read` not sent by the user, capped at the summary's message count.
    #[serde(default)]
    pub unread: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<ChatMessage>,
}

impl ChatSpace {
    /// Newest activity: `last_active`, else the latest message's time.
    pub fn activity(&self) -> Option<Date> {
        self.last_active.or_else(|| self.latest.as_ref().map(|message| message.created))
    }
}

/// Newest first, undated last; stable, so equal dates keep their order.
pub fn sort_newest_first<T>(items: &mut [T], date: impl Fn(&T) -> Option<Date>) {
    items.sort_by(|a, b| {
        let (a, b) = (date(a).unwrap_or(DISTANT_PAST), date(b).unwrap_or(DISTANT_PAST));
        b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// One account's spaces; also its row in mail.db.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatSpaceList {
    /// The user's `users/<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    pub more: bool,
    pub spaces: Vec<ChatSpace>,
}

/// A page of messages, ascending; also a recent space's row in mail.db.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePage {
    pub messages: Vec<ChatMessage>,
    pub more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<Date>,
}

// MARK: - Rows

/// What one space's list row shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatRow {
    pub key: ChatSpaceKey,
    pub kind: ChatSpaceKind,
    pub title: String,
    /// "Brent: see you" / "You: thanks".
    pub preview: String,
    pub date: String,
    /// Date group header, as in mail lists.
    pub section: String,
    pub activity: Option<Date>,
    pub unread: bool,
    pub attachment: bool,
    pub avatar_name: String,
    /// The person's lowercased address, else their Chat user id; empty when the row shows no one.
    pub avatar_key: String,
    pub avatar_url: Option<String>,
    pub avatar_color_key: String,
}

impl ChatRow {
    pub fn new(space: &ChatSpace, account: &str, labels: &DateLabels) -> Self {
        let title = if space.title.is_empty() {
            space.members.iter().map(ChatUser::label).collect::<Vec<_>>().join(", ")
        } else {
            space.title.clone()
        };
        let activity = space.activity();
        let (date, section) = labels.labels(activity);
        let latest = space.latest.as_ref();
        let incoming = latest.filter(|message| !message.mine).map(|message| &message.sender);
        let person = match space.kind {
            ChatSpaceKind::Dm | ChatSpaceKind::Bot => space.members.first().or(incoming),
            ChatSpaceKind::Group => incoming.or(space.members.first()),
            ChatSpaceKind::Space => incoming,
        };
        let (avatar_key, avatar_url, avatar_color_key, avatar_name) = match person {
            Some(person) => (person.avatar_key(), person.avatar_url.clone(), person.avatar_key(), person.label()),
            None => (String::new(), None, space.name.clone(), title.clone()),
        };
        Self {
            key: ChatSpaceKey::new(account, &space.name),
            kind: space.kind,
            preview: latest.map(preview).unwrap_or_default(),
            title,
            date,
            section,
            activity,
            unread: space.unread > 0,
            attachment: latest.is_some_and(|message| !message.attachments.is_empty()),
            avatar_name,
            avatar_key,
            avatar_url,
            avatar_color_key,
        }
    }

    /// As `chat.spaces` answers it.
    pub fn json(&self) -> Value {
        json!({
            "account": self.key.account, "space": self.key.space, "kind": self.kind.as_str(), "title": self.title,
            "preview": self.preview, "date": self.date, "section": self.section, "activity": self.activity.map(iso8601),
            "unread": self.unread, "attachment": self.attachment, "avatarKey": self.avatar_key,
            "avatarName": self.avatar_name, "avatarColorKey": self.avatar_color_key, "avatarUrl": self.avatar_url,
        })
    }
}

/// A message as a row's preview.
pub fn preview(message: &ChatMessage) -> String {
    let mut body = markup_plain(&message.text);
    if body.is_empty() {
        body = match message.attachments.first() {
            Some(attachment) => format!("Attachment: {}", attachment.name),
            None if message.card => "Card".into(),
            None => String::new(),
        };
    }
    let who = if message.mine { "You".to_string() } else { message.sender.short_label() };
    if body.is_empty() { who } else { format!("{who}: {body}") }
}

// MARK: - Date labels

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// A list's date and date group of a moment, as the Swift engine's `ConversationRow.dateLabels`
/// writes them for the en_US locale (weeks start on Sunday): "4:00 PM" today, "Nov 7" this year,
/// "11/7/2025" before; groups Today, Yesterday, This Week, Last Week, then months.
pub struct DateLabels {
    today: NaiveDate,
}

impl DateLabels {
    pub fn now() -> Self {
        Self { today: Local::now().date_naive() }
    }

    #[cfg(test)]
    pub fn at(today: NaiveDate) -> Self {
        Self { today }
    }

    pub fn labels(&self, date: Option<Date>) -> (String, String) {
        let Some(moment) = date.and_then(local) else { return (String::new(), "Older".into()) };
        (self.format(&moment), self.section(&moment))
    }

    fn format(&self, moment: &DateTime<Local>) -> String {
        let day = moment.date_naive();
        if day == self.today {
            let hour = moment.hour() % 12;
            let half = if moment.hour() < 12 { "AM" } else { "PM" };
            return format!("{}:{:02}\u{202F}{half}", if hour == 0 { 12 } else { hour }, moment.minute());
        }
        if day.year() == self.today.year() {
            return format!("{} {}", &MONTHS[day.month0() as usize][..3], day.day());
        }
        format!("{}/{}/{}", day.month(), day.day(), day.year())
    }

    fn section(&self, moment: &DateTime<Local>) -> String {
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
}

fn local(date: Date) -> Option<DateTime<Local>> {
    let unix = unix_seconds(date);
    let seconds = unix.floor();
    let nanos = (((unix - seconds) * 1e9) as u32).min(999_999_999);
    Local.timestamp_opt(seconds as i64, nanos).earliest()
}

// MARK: - Markup

static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<(https?://[^|>\s]+)(?:\|([^>\n]*))?>|<(users/[^>\s]+)>").expect("the token pattern compiles")
});

/// One line of text for list previews: links as their labels, mentions as "@", emphasis
/// markers dropped, whitespace runs as one space.
pub fn markup_plain(text: &str) -> String {
    let mut out = TOKEN
        .replace_all(text, |captures: &regex::Captures<'_>| match captures.get(1) {
            Some(url) => {
                let label = captures.get(2).map_or("", |label| label.as_str());
                if label.is_empty() { url.as_str().to_string() } else { label.to_string() }
            }
            None => "@".to_string(),
        })
        .replace("```", " ");
    for marker in ['*', '_', '~'] {
        out = strip_emphasis(&out, marker);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// `(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])` (for `*`; likewise `_` and `~`) replaced by its
/// group, matches taken leftmost and without overlap as NSRegularExpression replaces them.
fn strip_emphasis(text: &str, marker: char) -> String {
    if !text.contains(marker) {
        return text.to_string();
    }
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == marker
            && let Some(end) = emphasis_end(&characters, index, marker)
        {
            out.extend(&characters[index + 1..end]);
            index = end + 1;
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    out
}

/// Where the emphasis opened at `start` closes, if it does.
fn emphasis_end(characters: &[char], start: usize, marker: char) -> Option<usize> {
    let edge = |character: char| character == marker || is_word(character);
    if start > 0 && edge(characters[start - 1]) {
        return None;
    }
    if characters.get(start + 1)?.is_whitespace() {
        return None;
    }
    let mut last = start + 1;
    loop {
        let character = *characters.get(last)?;
        if !character.is_whitespace()
            && characters.get(last + 1) == Some(&marker)
            && characters.get(last + 2).is_none_or(|after| !edge(*after))
        {
            return Some(last + 1);
        }
        if character == marker || character == '\n' {
            return None;
        }
        last += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::date_from_unix;

    #[test]
    fn strips_markup_like_the_swift_engine() {
        assert_eq!(markup_plain("see <https://x.com/a|the doc> and <https://y.com>"), "see the doc and https://y.com");
        assert_eq!(markup_plain("hi <users/123>,\n\n*bold* _it_ ~gone~ snake_case_name"), "hi @, bold it gone snake_case_name");
        assert_eq!(markup_plain("```\ncode\n``` 2*3*4 * not *"), "code 2*3*4 * not *");
        assert_eq!(markup_plain("*ab**"), "ab*");
        assert_eq!(markup_plain("**x**"), "**x**");
        // What NSRegularExpression makes of these, checked against the Swift engine's patterns.
        assert_eq!(markup_plain("*a *b* c*"), "*a b c*");
        assert_eq!(markup_plain("_a_b_ x"), "_a_b_ x");
        assert_eq!(markup_plain("*multi\nline*"), "*multi line*");
        assert_eq!(markup_plain("~ x~ *y *z*"), "~ x~ *y z");
        assert_eq!(markup_plain("é*ü*é (*ok*)"), "é*ü*é (ok)");
        assert_eq!(markup_plain("*see <https://x.com|it>*"), "see it");
    }

    fn user(display: Option<&str>, email: Option<&str>) -> ChatUser {
        ChatUser {
            name: "users/1".into(),
            display_name: display.map(String::from),
            email: email.map(String::from),
            avatar_url: None,
            bot: false,
        }
    }

    #[test]
    fn labels_users_and_previews() {
        assert_eq!(user(Some("Brent Smith"), None).short_label(), "Brent");
        assert_eq!(user(Some("  "), Some("a@x.com")).label(), "a@x.com");
        assert_eq!(user(None, Some("A@X.com")).avatar_key(), "a@x.com");
        assert_eq!(user(None, None).label(), "Unknown");
        let message = ChatMessage {
            name: "spaces/a/messages/1".into(),
            sender: user(Some("Brent Smith"), None),
            created: 0.0,
            edited: None,
            text: String::new(),
            thread: None,
            thread_reply: false,
            mine: false,
            card: false,
            attachments: vec![ChatAttachment { name: "a.png".into(), content_type: None, url: None, resource: None }],
        };
        assert_eq!(preview(&message), "Brent: Attachment: a.png");
    }

    #[test]
    fn labels_dates_like_the_swift_engine() {
        // A Wednesday.
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let labels = DateLabels::at(today);
        let at = |day: NaiveDate, hour: u32| {
            let local = Local.from_local_datetime(&day.and_hms_opt(hour, 5, 0).unwrap()).earliest().unwrap();
            Some(date_from_unix(local.timestamp() as f64))
        };
        assert_eq!(labels.labels(at(today, 16)), ("4:05\u{202F}PM".into(), "Today".into()));
        assert_eq!(labels.labels(at(today, 0)).0, "12:05\u{202F}AM");
        assert_eq!(labels.labels(at(today.pred_opt().unwrap(), 9)), ("Nov 10".into(), "Yesterday".into()));
        assert_eq!(labels.labels(at(NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(), 9)).1, "This Week");
        assert_eq!(labels.labels(at(NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(), 9)).1, "Last Week");
        assert_eq!(labels.labels(at(NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(), 9)).1, "October");
        assert_eq!(
            labels.labels(at(NaiveDate::from_ymd_opt(2025, 3, 4).unwrap(), 9)),
            ("3/4/2025".into(), "March 2025".into())
        );
        assert_eq!(labels.labels(None), (String::new(), "Older".into()));
    }

    #[test]
    fn reads_what_the_swift_engine_writes() {
        // ChatSpaceList as Swift's JSONEncoder writes it, with an unknown kind and a missing title.
        let swift = r#"{"me":"users\/9","more":true,"spaces":[{"name":"spaces\/A","kind":"future","members":[],"unread":2,
            "lastActive":815000000.5,"latest":{"name":"spaces\/A\/messages\/1","sender":{"name":"users\/2","bot":false},
            "created":815000000.5,"text":"hi","threadReply":false,"mine":false,"card":false,"attachments":[]}}]}"#;
        let list: ChatSpaceList = serde_json::from_str(swift).unwrap();
        assert_eq!(list.spaces[0].kind, ChatSpaceKind::Space);
        assert_eq!(list.spaces[0].title, "");
        assert_eq!(list.spaces[0].activity(), Some(815_000_000.5));
        let written = String::from_utf8(crate::json::encode(&list, false, true)).unwrap();
        assert!(written.contains(r#""kind":"space""#) && !written.contains("lastRead"), "{written}");
    }
}
