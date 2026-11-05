//! Plain values decoded from the control socket's JSON answers. Decoding and the derived display
//! strings (`prepare`) run on the background executor.

use chrono::{DateTime, Local, Utc};
use gpui::SharedString;
use serde::{Deserialize, Deserializer};

use crate::format;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    #[serde(other)]
    Other,
}

impl Role {
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    People,
    Notification,
    Newsletter,
    Invitation,
    #[serde(other)]
    Other,
}

impl Category {
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

#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    /// The address; accounts are addressed by it.
    pub id: SharedString,
    #[serde(default)]
    pub label: SharedString,
    /// `google` or `microsoft`.
    #[serde(default)]
    pub provider: SharedString,
    /// The person's name, when the provider gave one.
    #[serde(default)]
    pub name: Option<SharedString>,
}

impl Account {
    pub fn title(&self) -> SharedString {
        if self.label.is_empty() { self.id.clone() } else { self.label.clone() }
    }

    /// Gmail calls its user folders labels.
    pub fn folders_title(&self) -> &'static str {
        if self.provider.as_ref() == "google" { "Labels" } else { "Folders" }
    }

    pub fn address(&self) -> Address {
        Address { name: Some(self.name.clone().unwrap_or_else(|| self.title())), email: self.id.clone() }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Mailbox {
    pub id: SharedString,
    #[serde(default)]
    pub name: SharedString,
    pub role: Role,
    pub unread: Option<u32>,
    pub total: Option<u32>,
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
    /// `Name <email>` or a bare address, as the control socket writes them.
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

    /// The first letter or digit of the name, uppercased (`Avatar.initials`).
    pub fn initial(&self) -> SharedString {
        let name = self.name.as_ref().unwrap_or(&self.email);
        name.chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>().into())
            .unwrap_or_else(|| "?".into())
    }
}

fn address<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Address>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(|raw| Address::parse(&raw)))
}

fn addresses<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Address>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(d)?
        .unwrap_or_default()
        .iter()
        .map(|raw| Address::parse(raw))
        .collect())
}

fn one() -> u32 {
    1
}

/// One row of `mail.list` / `mail.search`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: SharedString,
    pub account: SharedString,
    #[serde(default)]
    pub subject: SharedString,
    pub date: Option<DateTime<Utc>>,
    /// The latest incoming message's sender.
    #[serde(default, deserialize_with = "address")]
    pub from: Option<Address>,
    #[serde(default)]
    pub participants: Vec<SharedString>,
    #[serde(default = "one")]
    pub message_count: u32,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub snippet: Option<SharedString>,
    #[serde(default)]
    pub category: Option<Category>,
    #[serde(default)]
    pub waiting: bool,
    #[serde(default)]
    pub labels: Vec<SharedString>,
    /// The roles of the mailboxes its messages sit in.
    #[serde(default)]
    pub mailboxes: Vec<Role>,

    /// The subject without reply prefixes; `tag` holds leading `[list]` tags.
    #[serde(skip)]
    pub title: SharedString,
    #[serde(skip)]
    pub tag: SharedString,
    /// The row's date, as of `prepare`.
    #[serde(skip)]
    pub when: SharedString,
    #[serde(skip)]
    pub sender: SharedString,
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
}

#[derive(Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    #[serde(default)]
    pub failures: Vec<String>,
    /// The following page's cursor, from apps that page `mail.list`.
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub filename: SharedString,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub path: SharedString,
}

/// One message of `mail.show`.
#[derive(Clone, Debug, Deserialize)]
pub struct Message {
    pub id: SharedString,
    #[serde(default, deserialize_with = "addresses")]
    pub from: Vec<Address>,
    #[serde(default, deserialize_with = "addresses")]
    pub to: Vec<Address>,
    #[serde(default, deserialize_with = "addresses")]
    pub cc: Vec<Address>,
    pub date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub text: Option<SharedString>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub error: Option<SharedString>,

    #[serde(skip)]
    pub when: SharedString,
    /// The first line of the body, for a folded message.
    #[serde(skip)]
    pub preview: SharedString,
    /// `to` and `cc`, joined.
    #[serde(skip)]
    pub recipients: SharedString,
}

impl Message {
    pub fn prepare(&mut self, now: DateTime<Local>) {
        self.when = format::message_date(self.date, now).into();
        if let Some(text) = &self.text {
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
}

#[derive(Deserialize)]
pub struct Thread {
    #[serde(default)]
    pub messages: Vec<Message>,
}
