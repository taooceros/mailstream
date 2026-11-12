//! The UI's values, converted from the engine's records (`mailstream` crate), which come with
//! their display strings (dates, subjects, avatars; MS-235). Conversion and the strings derived
//! here (`prepare`) run on the background executor.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use gpui::SharedString;

use crate::design::AttachmentKind;
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

    /// The engine's avatar for it: the name's initial, the address's color.
    pub fn avatar(&self) -> mailstream::Avatar {
        mailstream::avatar(self.name.as_deref().unwrap_or_default().to_owned(), self.email.to_string())
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
    /// The row's date and its date group, as of the engine's answer.
    pub when: SharedString,
    pub section: SharedString,
    pub avatar: mailstream::Avatar,
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
            title: c.title.into(),
            tag: c.tag.into(),
            when: c.when.into(),
            section: c.section.into(),
            avatar: c.avatar,
            sender: SharedString::default(),
            senders: SharedString::default(),
        }
    }
}

impl Conversation {
    /// Fills the display strings, once per answer rather than once per frame.
    pub fn prepare(&mut self) {
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

    /// The address whose photo the row's avatar shows.
    pub fn avatar_email(&self) -> SharedString {
        self.from.as_ref().map_or_else(|| self.sender.clone(), |from| from.email.clone())
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
    pub mime: SharedString,
    pub path: SharedString,
    /// Its kind, for its mark's color and whether a picture of it shows (`AttachmentKind::of`).
    pub kind: AttachmentKind,
    /// Its tile's second line: its size and type ("1.2 MB · PDF").
    pub detail: SharedString,
    /// Its mark's label: up to four letters of its extension ("PDF", "ZIP"), else of its type.
    pub mark: SharedString,
}

impl From<mailstream::Attachment> for Attachment {
    fn from(a: mailstream::Attachment) -> Self {
        let size = u64::try_from(a.size).unwrap_or(0);
        let (kind, label) = AttachmentKind::of(&a.filename, &a.mime, None);
        let extension = a.filename.rsplit_once('.').map(|(_, extension)| extension).unwrap_or_default();
        let mark = if extension.is_empty() { label.as_ref() } else { extension };
        Attachment {
            mark: mark.chars().take(4).collect::<String>().to_uppercase().into(),
            detail: format!("{} · {label}", format::file_size(size)).into(),
            filename: a.filename.into(),
            mime: a.mime.into(),
            path: a.path.into(),
            kind,
        }
    }
}

impl Attachment {
    /// A calendar file (`invite.ics`), which a message's invitation card already shows.
    fn calendar(&self) -> bool {
        let mime = self.mime.to_ascii_lowercase();
        mime.starts_with("text/calendar")
            || mime == "application/ics"
            || self.filename.to_ascii_lowercase().ends_with(".ics")
    }
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
    pub starred: bool,
    /// Plain text, or the HTML body as text (the engine derives it when the mail has no plain
    /// part); `None` when the body could not load.
    pub text: Option<SharedString>,
    /// The HTML body, when the message has one; the reader draws it rather than the text.
    pub html: Option<SharedString>,
    pub truncated: bool,
    pub attachments: Vec<Attachment>,
    pub error: Option<SharedString>,
    /// The meeting invitation it carries; the reader's card asks the calendar about it.
    pub invitation: Option<mailstream::MeetingInvitation>,

    /// The date and the time of day (for the chat log), as of the engine's answer.
    pub when: SharedString,
    pub time: SharedString,
    /// The sender's avatar.
    pub avatar: mailstream::Avatar,
    /// The day divider above it in a chat log; empty on the day of the message before
    /// (`Message::divide`).
    pub divider: SharedString,
    /// The first line of the body, for a folded message.
    pub preview: SharedString,
    /// `to` and `cc` by name, the user as "me": "to me, Maya Lin · cc Platform Architecture".
    pub recipients: SharedString,
    /// The full headers the recipient line opens to: From, To, Cc (`Name <address>`) and Date.
    pub headers: Vec<(&'static str, SharedString)>,
    /// In a chat log, the message continues the run of the one above it: the same sender, the
    /// same day, within the hour (`Message::divide`).
    pub continued: bool,
    /// The text body as the reader shows it: what was written, its `>` quotes in place as runs of
    /// their own (`format::runs`), and the quoted history and signature folded behind a toggle
    /// (`format::fold`).
    pub shown: Vec<format::Run>,
    pub quoted: Option<SharedString>,
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
            starred: m.starred,
            text: m.text.map(Into::into),
            html: m.html.map(Into::into),
            truncated: m.truncated,
            attachments: m.attachments.into_iter().map(Attachment::from).collect(),
            error: m.error.map(Into::into),
            invitation: m.invitation,
            when: m.when.into(),
            time: m.time.into(),
            avatar: m.avatar,
            divider: SharedString::default(),
            preview: SharedString::default(),
            recipients: SharedString::default(),
            headers: Vec::new(),
            continued: false,
            shown: Vec::new(),
            quoted: None,
        }
    }
}

impl Message {
    /// Fills the display strings; `own` holds the user's addresses, lowercased, shown as "me".
    pub fn prepare(&mut self, own: &HashSet<String>) {
        if let Some(text) = &self.text {
            let (shown, quoted) = format::fold(text);
            self.shown = format::runs(&shown);
            self.quoted = quoted.map(Into::into);
            let text = format::body(text);
            self.preview = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_owned().into();
            self.text = Some(text.into());
        }
        // The invitation card stands for its calendar file.
        if self.invitation.is_some() {
            self.attachments.retain(|attachment| !attachment.calendar());
        }
        let mut recipients = String::new();
        for (label, list) in [("to", &self.to), ("cc", &self.cc)] {
            if list.is_empty() {
                continue;
            }
            if !recipients.is_empty() {
                recipients.push_str(" · ");
            }
            recipients.push_str(label);
            recipients.push(' ');
            let names: Vec<_> = list
                .iter()
                .map(|a| if own.contains(&a.email.to_lowercase()) { "me".into() } else { a.display_name() })
                .collect();
            recipients.push_str(&names.join(", "));
        }
        self.recipients = recipients.into();
        let list = |addresses: &[Address]| addresses.iter().map(Address::header_form).collect::<Vec<_>>().join(", ");
        self.headers = [("From", &self.from), ("To", &self.to), ("Cc", &self.cc)]
            .into_iter()
            .filter(|(_, addresses)| !addresses.is_empty())
            .map(|(label, addresses)| (label, list(addresses).into()))
            .collect();
        if let Some(date) = self.date {
            self.headers.push(("Date", mailstream::full_date(Some(date.to_rfc3339())).into()));
        }
    }

    /// The HTML body the reader draws instead of the text: whenever the message has one, in the
    /// message cards and the chat log alike (MS-244). The text is no sign of a plain mail: the
    /// engine derives it from the HTML when the mail has no plain part, and GitHub's
    /// notifications carry both.
    pub fn html_body(&self) -> Option<&SharedString> {
        self.html.as_ref().filter(|html| !html.trim().is_empty())
    }

    /// Places the day dividers of `messages`, oldest first, as a chat log shows them, and marks
    /// those continuing the run above them: the same sender, the same day, within the hour.
    pub fn divide(messages: &mut [Message]) {
        let entries = messages
            .iter()
            .map(|message| mailstream::ChatRunEntry {
                sender: message.sender().email.to_lowercase(),
                date: message.date.map(|date| date.to_rfc3339()),
                opens_run: false,
            })
            .collect();
        for (message, run) in messages.iter_mut().zip(mailstream::chat_runs(entries)) {
            message.divider = run.divider.into();
            message.continued = run.continued;
        }
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

    /// The body a forward of this message starts with: the engine's forwarded-message header,
    /// then the text.
    pub fn forwarded(&self, subject: &str) -> String {
        let list = |addresses: &[Address]| addresses.iter().map(Address::header_form).collect();
        mailstream::forward_body(
            list(&self.from),
            list(&self.to),
            list(&self.cc),
            self.date.map(|date| date.to_rfc3339()),
            subject.to_owned(),
            self.text.as_ref().map_or_else(String::new, ToString::to_string),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message as the engine's `show` gives it.
    fn shown(text: &str, html: Option<&str>) -> Message {
        let message: mailstream::Message = serde_json::from_value(serde_json::json!({
            "id": "m1", "mailbox": "INBOX", "role": "inbox", "from": ["Jack251970 <notifications@github.com>"],
            "to": ["Flow-Launcher/Flow.Launcher <Flow.Launcher@noreply.github.com>"], "date": "2026-11-12T09:04:00Z",
            "subject": "Re: [Flow-Launcher/Flow.Launcher] new issue (Issue #4754)", "unread": true, "starred": false,
            "text": text, "html": html,
        }))
        .unwrap();
        let mut message = Message::from(message);
        message.prepare(&HashSet::new());
        message
    }

    /// A message of a chat log from `from` at `at` (RFC 3339), to the user and Maya.
    fn sent(from: &str, at: &str) -> Message {
        let message: mailstream::Message = serde_json::from_value(serde_json::json!({
            "id": at, "mailbox": "INBOX", "role": "inbox", "from": [from],
            "to": ["Reader Test <reader@example.com>", "Maya Lin <maya@example.com>"], "cc": ["QA <qa@example.com>"],
            "date": at, "subject": "Plan", "unread": false, "starred": false, "text": "Hi",
            "attachments": [
                { "filename": "invite.ics", "mime": "text/calendar", "size": 900, "path": "/tmp/invite.ics" },
                { "filename": "plan.pdf", "mime": "application/pdf", "size": 1_200_000, "path": "/tmp/plan.pdf" },
            ],
        }))
        .unwrap();
        let mut message = Message::from(message);
        message.prepare(&HashSet::from(["reader@example.com".to_owned()]));
        message
    }

    #[test]
    fn the_user_reads_as_me_and_the_headers_name_everyone() {
        let message = sent("Alex Rivera <alex@example.com>", "2026-11-12T09:04:00Z");
        assert_eq!(message.recipients.as_ref(), "to me, Maya Lin · cc QA");
        let headers: Vec<_> = message.headers.iter().map(|(label, value)| (*label, value.as_ref())).collect();
        assert_eq!(headers[..3], [
            ("From", "Alex Rivera <alex@example.com>"),
            ("To", "Reader Test <reader@example.com>, Maya Lin <maya@example.com>"),
            ("Cc", "QA <qa@example.com>"),
        ]);
        assert_eq!(headers[3].0, "Date");
        let pdf = &message.attachments[1];
        assert_eq!((pdf.mark.as_ref(), pdf.kind), ("PDF", AttachmentKind::Pdf));
        assert!(pdf.detail.ends_with(" · PDF"), "{}", pdf.detail);
    }

    #[test]
    fn an_invitation_stands_for_its_calendar_file() {
        let plain = sent("Alex Rivera <alex@example.com>", "2026-11-12T09:04:00Z");
        assert_eq!(plain.attachments.len(), 2);
        let mut invited = plain.clone();
        invited.invitation = Some(serde_json::from_value(serde_json::json!({ "method": "REQUEST", "uid": "u1", "title": "Sync", "start": "2026-11-13T10:00:00Z", "end": "2026-11-13T10:30:00Z" })).unwrap());
        invited.prepare(&HashSet::new());
        let names: Vec<_> = invited.attachments.iter().map(|a| a.filename.as_ref()).collect();
        assert_eq!(names, ["plan.pdf"]);
    }

    #[test]
    fn a_senders_messages_within_the_hour_join_one_run() {
        let mut log = [
            sent("Alex Rivera <alex@example.com>", "2026-11-12T12:16:00+00:00"),
            sent("Alex <ALEX@example.com>", "2026-11-12T12:18:00+00:00"),
            sent("Alex Rivera <alex@example.com>", "2026-11-12T13:30:00+00:00"),
            sent("Jordan Lee <jordan@example.com>", "2026-11-12T13:31:00+00:00"),
            sent("Alex Rivera <alex@example.com>", "2026-11-12T13:32:00+00:00"),
        ];
        Message::divide(&mut log);
        let runs: Vec<bool> = log.iter().map(|message| message.continued).collect();
        assert_eq!(runs, [false, true, false, false, false]);
    }

    #[test]
    fn github_notifications_show_their_html() {
        let html = "<p></p>\n<b>Aarush-githubuser</b> created an issue <a href=\"https://github.com/Flow-Launcher/Flow.Launcher/issues/4754\">(Flow-Launcher/Flow.Launcher#4754)</a>";
        // multipart/alternative: GitHub's text part beside its HTML.
        let both = shown("Aarush-githubuser created an issue (Flow-Launcher/Flow.Launcher#4754)", Some(html));
        assert_eq!(both.html_body().map(|html| html.as_ref()), Some(html));
        // HTML only (as Outlook hands it out): the engine's text is the HTML's.
        let only = shown(" \nAarush-githubuser  created an issue  (Flow-Launcher/Flow.Launcher#4754)", Some(html));
        assert_eq!(only.html_body().map(|html| html.as_ref()), Some(html));
        // Plain mail, or an HTML body with nothing in it, reads as text.
        assert_eq!(shown("Lunch at noon?", None).html_body(), None);
        assert_eq!(shown("Lunch at noon?", Some(" \n")).html_body(), None);
    }
}

