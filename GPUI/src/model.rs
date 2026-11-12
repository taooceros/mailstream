//! The UI's values, converted from the engine's records (`mailstream` crate). Conversion and the
//! derived display strings (`prepare`) run on the background executor.

use std::collections::HashSet;

use chrono::{DateTime, Local, Utc};
use gpui::SharedString;

use crate::format;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Role {
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

impl Role {
    /// A role as the engine names it; unknown names are user folders.
    pub fn named(name: &str) -> Role {
        match name {
            "inbox" => Role::Inbox,
            "starred" => Role::Starred,
            "important" => Role::Important,
            "sent" => Role::Sent,
            "drafts" => Role::Drafts,
            "archive" => Role::Archive,
            "all" => Role::All,
            "junk" => Role::Junk,
            "trash" => Role::Trash,
            _ => Role::Other,
        }
    }

    /// The unified mailboxes the sidebar offers, in order (`MailStore.unifiedMailboxes`).
    pub const UNIFIED: [Role; 7] =
        [Role::Inbox, Role::Starred, Role::Drafts, Role::Sent, Role::Archive, Role::Junk, Role::Trash];

    pub fn title(self) -> &'static str {
        match self {
            Role::Inbox => "Inbox",
            Role::Starred => "Starred",
            Role::Important => "Important",
            Role::Sent => "Sent",
            Role::Drafts => "Drafts",
            Role::Archive => "Archive",
            Role::All => "All Mail",
            Role::Junk => "Junk",
            Role::Trash => "Trash",
            Role::Other => "Folder",
        }
    }

    /// The name `mail.list --mailbox` takes.
    pub fn arg(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Starred => "starred",
            Role::Important => "important",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Archive => "archive",
            Role::All => "all",
            Role::Junk => "junk",
            Role::Trash => "trash",
            Role::Other => "other",
        }
    }

    /// Whose name a row shows: the recipients' in mail the user wrote.
    pub fn shows_recipients(self) -> bool {
        matches!(self, Role::Sent | Role::Drafts)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    People,
    Notification,
    Newsletter,
    Invitation,
    Other,
}

impl Category {
    pub fn named(name: &str) -> Category {
        match name {
            "people" => Category::People,
            "notification" => Category::Notification,
            "newsletter" => Category::Newsletter,
            "invitation" => Category::Invitation,
            _ => Category::Other,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Category::People => "People",
            Category::Notification => "Notifications",
            Category::Newsletter => "Newsletters",
            Category::Invitation => "Invitations",
            Category::Other => "Other",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Category::People => "people",
            Category::Notification => "notification",
            Category::Newsletter => "newsletter",
            Category::Invitation => "invitation",
            Category::Other => "other",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Account {
    /// The address; accounts are addressed by it.
    pub id: SharedString,
    pub label: SharedString,
    /// `google` or `microsoft`.
    pub provider: SharedString,
}

impl From<mailstream::Account> for Account {
    fn from(account: mailstream::Account) -> Self {
        Account {
            id: account.id.into(),
            label: account.label.into(),
            provider: account.provider.into(),
        }
    }
}

impl Account {
    pub fn title(&self) -> SharedString {
        if self.label.is_empty() { self.id.clone() } else { self.label.clone() }
    }

    /// Gmail calls its user folders labels.
    pub fn folders_title(&self) -> &'static str {
        if self.provider.as_ref() == "google" { "Labels" } else { "Folders" }
    }
}

#[derive(Clone, Debug)]
pub struct Mailbox {
    pub id: SharedString,
    pub name: SharedString,
    pub role: Role,
    pub unread: Option<u32>,
    pub total: Option<u32>,
}

impl From<mailstream::Mailbox> for Mailbox {
    fn from(mailbox: mailstream::Mailbox) -> Self {
        Mailbox {
            id: mailbox.id.into(),
            name: mailbox.name.into(),
            role: Role::named(&mailbox.role),
            unread: mailbox.unread.and_then(|n| u32::try_from(n).ok()),
            total: mailbox.total.and_then(|n| u32::try_from(n).ok()),
        }
    }
}

impl Mailbox {
    /// The sidebar badge (`MailStore.badgeCount`): unread mail, every draft; none for mailboxes
    /// whose unread mail is not news.
    pub fn badge(&self) -> u32 {
        match self.role {
            Role::Drafts => self.total.unwrap_or(0),
            Role::Inbox | Role::Other | Role::Important | Role::Starred => self.unread.unwrap_or(0),
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub name: Option<SharedString>,
    pub email: SharedString,
}

impl Address {
    /// `Name <email>` or a bare address, as the engine writes them.
    pub fn parse(raw: &str) -> Address {
        let raw = raw.trim();
        if let (Some(open), true) = (raw.rfind('<'), raw.ends_with('>')) {
            let email = raw[open + 1..raw.len() - 1].trim();
            let mut name = raw[..open].trim();
            if name.len() >= 2 && name.starts_with('"') && name.ends_with('"') {
                name = &name[1..name.len() - 1];
            }
            return Address {
                name: (!name.is_empty()).then(|| SharedString::from(name.to_owned())),
                email: email.to_owned().into(),
            };
        }
        Address { name: None, email: raw.to_owned().into() }
    }

    pub fn display_name(&self) -> SharedString {
        self.name.clone().unwrap_or_else(|| self.email.clone())
    }

    /// `Name <email>`, or the bare address.
    pub fn header_form(&self) -> String {
        match &self.name {
            Some(name) if name != &self.email => format!("{name} <{}>", self.email),
            _ => self.email.to_string(),
        }
    }

    /// The first letter or digit of the name, uppercased (`Avatar.initials`).
    pub fn initial(&self) -> SharedString {
        let name = self.name.as_ref().unwrap_or(&self.email);
        name.chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>().into())
            .unwrap_or_else(|| "?".into())
    }
}

/// An ISO 8601 date from the engine.
fn date(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw?.parse().ok()
}

fn addresses(raw: Vec<String>) -> Vec<Address> {
    raw.iter().map(|raw| Address::parse(raw)).collect()
}

/// One row of a listing or search.
#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: SharedString,
    pub account: SharedString,
    pub subject: SharedString,
    pub date: Option<DateTime<Utc>>,
    /// The latest incoming message's sender.
    pub from: Option<Address>,
    pub participants: Vec<SharedString>,
    pub message_count: u32,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    pub snippet: Option<SharedString>,
    pub category: Option<Category>,
    pub waiting: bool,
    pub labels: Vec<SharedString>,
    /// The roles of the mailboxes its messages sit in.
    pub mailboxes: Vec<Role>,

    /// The subject without reply prefixes; `tag` holds leading `[list]` tags.
    pub title: SharedString,
    pub tag: SharedString,
    /// The row's date, as of `prepare`.
    pub when: SharedString,
    pub sender: SharedString,
    /// Everyone in it, for the row's first line; the sender when the engine names no one.
    pub senders: SharedString,
}

impl From<mailstream::Conversation> for Conversation {
    fn from(c: mailstream::Conversation) -> Self {
        Conversation {
            id: c.id.into(),
            account: c.account.into(),
            subject: c.subject.into(),
            date: date(c.date),
            from: c.from.as_deref().map(Address::parse),
            participants: c.participants.into_iter().map(Into::into).collect(),
            message_count: u32::try_from(c.message_count).unwrap_or(1).max(1),
            unread: c.unread,
            starred: c.starred,
            has_attachment: c.has_attachment,
            snippet: c.snippet.map(Into::into),
            category: c.category.as_deref().map(Category::named),
            waiting: c.waiting,
            labels: c.labels.into_iter().map(Into::into).collect(),
            mailboxes: c.mailboxes.iter().map(|role| Role::named(role)).collect(),
            title: SharedString::default(),
            tag: SharedString::default(),
            when: SharedString::default(),
            sender: SharedString::default(),
            senders: SharedString::default(),
        }
    }
}

impl Conversation {
    /// Fills the display strings, once per answer rather than once per frame.
    pub fn prepare(&mut self, now: DateTime<Local>) {
        let (tag, title) = format::split_subject(&self.subject);
        self.tag = tag.into();
        self.title = title.into();
        self.when = format::list_date(self.date, now).into();
        self.sender = match &self.from {
            Some(from) => from.display_name(),
            None => self.participants.first().cloned().unwrap_or_else(|| "(unknown)".into()),
        };
        self.senders =
            if self.participants.is_empty() { self.sender.clone() } else { self.participants.join(", ").into() };
    }

    /// Service mail the Smart Inbox folds into a bundle; anything waiting on the user stays out.
    pub fn bundle(&self) -> Option<Category> {
        match self.category {
            Some(c @ (Category::Notification | Category::Newsletter | Category::Invitation)) if !self.waiting => {
                Some(c)
            }
            _ => None,
        }
    }

    /// The address the avatar is drawn for.
    pub fn avatar(&self) -> Address {
        self.from.clone().unwrap_or_else(|| Address { name: Some(self.sender.clone()), email: self.sender.clone() })
    }

    /// Something sits in an inbox-like mailbox (`MailStore.canArchive`).
    pub fn can_archive(&self) -> bool {
        self.mailboxes
            .iter()
            .any(|role| !matches!(role, Role::Sent | Role::All | Role::Archive | Role::Trash | Role::Drafts))
    }

    /// Something sits in the trash or junk (`MailStore.canRestore`).
    pub fn can_restore(&self) -> bool {
        self.mailboxes.iter().any(|role| matches!(role, Role::Trash | Role::Junk))
    }

    /// "Waiting on you" can be set or cleared: tagged, and not a newsletter (`canMarkWaiting`).
    pub fn can_mark_waiting(&self) -> bool {
        self.category.is_some_and(|c| c != Category::Newsletter)
    }

    /// A conversation between people reads as a chat log (Flutter's `Reader.chat`, MS-73): more
    /// than one message, and tagged People or not tagged at all.
    pub fn chat_log(&self) -> bool {
        self.message_count > 1 && self.category.is_none_or(|c| c == Category::People)
    }
}

pub struct Listing {
    pub conversations: Vec<Conversation>,
    pub failures: Vec<String>,
    /// The following page's cursor.
    pub next: Option<String>,
}

impl From<mailstream::Listing> for Listing {
    fn from(listing: mailstream::Listing) -> Self {
        Listing {
            conversations: listing.conversations.into_iter().map(Conversation::from).collect(),
            failures: listing.failures,
            next: listing.next,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub filename: SharedString,
    pub size: u64,
    pub path: SharedString,
}

/// One message of a thread.
#[derive(Clone, Debug)]
pub struct Message {
    pub id: SharedString,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub date: Option<DateTime<Utc>>,
    pub unread: bool,
    pub text: Option<SharedString>,
    /// The HTML body, when the message has one; the reader shows it in a web view.
    pub html: Option<SharedString>,
    pub truncated: bool,
    pub attachments: Vec<Attachment>,
    pub error: Option<SharedString>,
    /// The meeting invitation it carries; the reader's card asks the calendar about it.
    pub invitation: Option<mailstream::MeetingInvitation>,

    pub when: SharedString,
    /// The first line of the body, for a folded message.
    pub preview: SharedString,
    /// `to` and `cc`, joined.
    pub recipients: SharedString,
    /// The text body as the reader shows it: what was written, and the quoted history and
    /// signature folded behind a toggle (`format::fold`).
    pub shown: SharedString,
    pub quoted: Option<SharedString>,
    /// The time of day, for the chat log.
    pub time: SharedString,
}

impl From<mailstream::Message> for Message {
    fn from(m: mailstream::Message) -> Self {
        Message {
            id: m.id.into(),
            from: addresses(m.from),
            to: addresses(m.to),
            cc: addresses(m.cc),
            date: date(m.date),
            unread: m.unread,
            text: m.text.map(Into::into),
            html: m.html.map(Into::into),
            truncated: m.truncated,
            attachments: m
                .attachments
                .into_iter()
                .map(|a| Attachment {
                    filename: a.filename.into(),
                    size: u64::try_from(a.size).unwrap_or(0),
                    path: a.path.into(),
                })
                .collect(),
            error: m.error.map(Into::into),
            invitation: m.invitation,
            when: SharedString::default(),
            preview: SharedString::default(),
            recipients: SharedString::default(),
            shown: SharedString::default(),
            quoted: None,
            time: SharedString::default(),
        }
    }
}

impl Message {
    pub fn prepare(&mut self, now: DateTime<Local>) {
        self.when = format::message_date(self.date, now).into();
        self.time = format::message_time(self.date).into();
        if let Some(text) = &self.text {
            let (shown, quoted) = format::fold(text);
            self.shown = shown.into();
            self.quoted = quoted.map(Into::into);
            let text = format::body(text);
            self.preview = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_owned().into();
            self.text = Some(text.into());
        }
        let mut recipients = String::new();
        for (label, list) in [("To", &self.to), ("Cc", &self.cc)] {
            if list.is_empty() {
                continue;
            }
            if !recipients.is_empty() {
                recipients.push_str("   ");
            }
            recipients.push_str(label);
            recipients.push(' ');
            let names: Vec<_> = list.iter().map(|a| a.display_name()).collect();
            recipients.push_str(&names.join(", "));
        }
        self.recipients = recipients.into();
    }

    pub fn sender(&self) -> Address {
        self.from.first().cloned().unwrap_or(Address { name: None, email: "(unknown)".into() })
    }

    /// Who a reply to this message goes to, as the reader can tell from its headers (Flutter's
    /// `_recipients`): its sender, or for the user's own message its recipients; a reply to all
    /// adds everyone else as Cc. The user's addresses (`own`, lowercased) stay out unless they are
    /// all there is.
    pub fn reply_recipients(&self, own: &HashSet<String>, all: bool) -> (Vec<Address>, Vec<Address>) {
        let mine = |address: &&Address| own.contains(&address.email.to_lowercase());
        let primary = if self.from.iter().any(|address| mine(&address)) { &self.to } else { &self.from };
        let mut seen = HashSet::new();
        let mut to: Vec<Address> = primary
            .iter()
            .filter(|address| !mine(address) && seen.insert(address.email.to_lowercase()))
            .cloned()
            .collect();
        if to.is_empty() {
            to = primary.clone();
            seen.extend(to.iter().map(|address| address.email.to_lowercase()));
        }
        let cc = if all {
            self.to
                .iter()
                .chain(&self.cc)
                .chain(&self.from)
                .filter(|address| !mine(address) && seen.insert(address.email.to_lowercase()))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        (to, cc)
    }

    /// The body a forward of this message starts with (`Replies.forward`): a header block, then
    /// the text.
    pub fn forwarded(&self, subject: &str) -> String {
        let list = |addresses: &[Address]| addresses.iter().map(Address::header_form).collect::<Vec<_>>().join(", ");
        let mut header = vec!["---------- Forwarded message ---------".to_owned(), format!("From: {}", list(&self.from))];
        if let Some(date) = self.date {
            header.push(format!("Date: {}", date.with_timezone(&Local).format("%a, %b %-d, %Y at %-I:%M %p")));
        }
        header.push(format!("Subject: {subject}"));
        header.push(format!("To: {}", list(&self.to)));
        if !self.cc.is_empty() {
            header.push(format!("Cc: {}", list(&self.cc)));
        }
        format!("{}\n\n{}", header.join("\n"), self.text.as_ref().map_or("", |text| text.as_ref()))
    }
}

