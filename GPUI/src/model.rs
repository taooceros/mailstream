//! What this UI adds to the engine's values (MS-238): GPUI uses the engine's answer types as the
//! `mailstream` crate hands them over, display strings included (MS-235), and keeps here only the
//! words it shows for them, the sidebar's and Smart Inbox's rules, and the reader's prepared
//! messages, which run on the background executor once per answer.

use std::collections::HashSet;
use std::ops::Deref;

use chrono::{DateTime, Utc};
use gpui::SharedString;
use mailstream::{Account, Address, Attachment, Category, Conversation, Mailbox, MailboxRole, Provider};

use crate::design::AttachmentKind;
use crate::format;

/// The unified mailboxes the sidebar offers, in order (`MailStore.unifiedMailboxes`).
pub const UNIFIED: [MailboxRole; 7] = [
    MailboxRole::Inbox,
    MailboxRole::Starred,
    MailboxRole::Drafts,
    MailboxRole::Sent,
    MailboxRole::Archive,
    MailboxRole::Junk,
    MailboxRole::Trash,
];

pub trait RoleExt {
    /// The mailbox's name as this app shows it: Junk, where the engine's `title` says Spam.
    fn label(self) -> &'static str;
    /// Whose name a row shows: the recipients' in mail the user wrote.
    fn shows_recipients(self) -> bool;
}

impl RoleExt for MailboxRole {
    fn label(self) -> &'static str {
        match self {
            MailboxRole::Junk => "Junk",
            role => role.title(),
        }
    }

    fn shows_recipients(self) -> bool {
        matches!(self, MailboxRole::Sent | MailboxRole::Drafts)
    }
}

pub trait CategoryExt {
    /// A Smart Inbox bundle's title.
    fn label(self) -> &'static str;
}

impl CategoryExt for Category {
    fn label(self) -> &'static str {
        match self {
            Category::People => "People",
            Category::Notification => "Notifications",
            Category::Newsletter => "Newsletters",
            Category::Invitation => "Invitations",
        }
    }
}

pub trait AccountExt {
    /// Its label, else its address.
    fn title(&self) -> SharedString;
    /// Gmail calls its user folders labels.
    fn folders_title(&self) -> &'static str;
}

impl AccountExt for Account {
    fn title(&self) -> SharedString {
        if self.label.is_empty() { self.id.clone().into() } else { self.label.clone().into() }
    }

    fn folders_title(&self) -> &'static str {
        if self.provider == Provider::Google { "Labels" } else { "Folders" }
    }
}

pub trait ProviderExt {
    /// The provider as people know it: Google, Microsoft.
    fn name(self) -> &'static str;
}

impl ProviderExt for Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }
}

pub trait MailboxExt {
    /// The sidebar badge (`MailStore.badgeCount`): unread mail, every draft; none for mailboxes
    /// whose unread mail is not news.
    fn badge(&self) -> u32;
}

impl MailboxExt for Mailbox {
    fn badge(&self) -> u32 {
        let count = match self.role {
            MailboxRole::Drafts => self.total,
            MailboxRole::Inbox | MailboxRole::Other | MailboxRole::Important | MailboxRole::Starred => self.unread,
            _ => None,
        };
        count.and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
    }
}

pub trait ConversationExt {
    /// Whom the row names: the latest message's sender (the other party when the user wrote it),
    /// else the first participant.
    fn sender_name(&self) -> SharedString;
    /// Everyone in it, for the row's first line; the other party when the user wrote the latest.
    fn senders(&self) -> SharedString;
    /// The address whose photo the row's avatar shows.
    fn avatar_email(&self) -> SharedString;
    /// Its latest message's date.
    fn time(&self) -> Option<DateTime<Utc>>;
}

impl ConversationExt for Conversation {
    fn sender_name(&self) -> SharedString {
        if self.from_me {
            if !self.peers.is_empty() {
                return self.peers.iter().map(Address::display_name).collect::<Vec<_>>().join(", ").into();
            }
            return "me".into();
        }
        match &self.from {
            Some(from) => from.display_name().into(),
            None => self.participants.first().map_or_else(|| "(unknown)".into(), |name| name.clone().into()),
        }
    }

    fn senders(&self) -> SharedString {
        if self.from_me {
            if !self.peers.is_empty() {
                return self.sender_name();
            }
            let others: Vec<&str> = self.participants.iter().map(String::as_str).filter(|p| *p != "me").collect();
            return if others.is_empty() { self.sender_name() } else { others.join(", ").into() };
        }
        if self.participants.is_empty() { self.sender_name() } else { self.participants.join(", ").into() }
    }

    fn avatar_email(&self) -> SharedString {
        self.peers
            .first()
            .or(self.from.as_ref())
            .map_or_else(|| self.sender_name(), |address| address.email.clone().into())
    }

    fn time(&self) -> Option<DateTime<Utc>> {
        self.date.as_deref()?.parse().ok()
    }
}

/// An attachment as the reader's tile shows it.
#[derive(Clone, Debug)]
pub struct Tile {
    pub attachment: Attachment,
    /// Its kind, for its mark's color and whether a picture of it shows (`AttachmentKind::of`).
    pub kind: AttachmentKind,
    /// Its tile's second line: its size and type ("1.2 MB · PDF").
    pub detail: SharedString,
    /// Its mark's label: up to four letters of its extension ("PDF", "ZIP"), else of its type.
    pub mark: SharedString,
}

impl Tile {
    fn new(attachment: Attachment) -> Self {
        let size = u64::try_from(attachment.size).unwrap_or(0);
        let (kind, label) = AttachmentKind::of(&attachment.filename, &attachment.mime, None);
        let extension = attachment.filename.rsplit_once('.').map(|(_, extension)| extension).unwrap_or_default();
        let mark = if extension.is_empty() { label.as_ref() } else { extension };
        Tile {
            mark: mark.chars().take(4).collect::<String>().to_uppercase().into(),
            detail: format!("{} · {label}", format::file_size(size)).into(),
            kind,
            attachment,
        }
    }

    /// A calendar file (`invite.ics`), which a message's invitation card already shows.
    fn calendar(&self) -> bool {
        let mime = self.attachment.mime.to_ascii_lowercase();
        mime.starts_with("text/calendar")
            || mime == "application/ics"
            || self.attachment.filename.to_ascii_lowercase().ends_with(".ics")
    }
}

/// One message of a thread as the reader shows it: the engine's message (its `text` the body as
/// read, `format::body`) and what the reader derives from it once.
#[derive(Clone, Debug)]
pub struct ShownMessage {
    pub message: mailstream::Message,
    /// The HTML body the reader draws instead of the text (`mailstream::Message::html_body`),
    /// taken out of `message` once rather than copied each frame.
    pub body_html: Option<SharedString>,
    /// Its attachments as tiles; an invitation's calendar file stays out, its card stands for it.
    pub tiles: Vec<Tile>,
    /// The first line of the body, for a folded message.
    pub preview: SharedString,
    /// `to` and `cc` by name, the user as "me": "to me, Maya Lin · cc Platform Architecture".
    pub recipients: SharedString,
    /// The full headers the recipient line opens to: From, To, Cc (`Name <address>`) and Date.
    pub headers: Vec<(&'static str, SharedString)>,
    /// The text body as the reader shows it: what was written, its `>` quotes in place as runs of
    /// their own (`format::runs`), and the quoted history and signature folded behind a toggle
    /// (`format::fold`).
    pub shown: Vec<format::Run>,
    pub quoted: Option<SharedString>,
    /// Sent from one of the user's addresses: the chat log puts it on the right (MS-326).
    pub mine: bool,
}

impl Deref for ShownMessage {
    type Target = mailstream::Message;

    fn deref(&self) -> &mailstream::Message {
        &self.message
    }
}

impl ShownMessage {
    /// Prepares `message` for the reader; `own` holds the user's addresses, lowercased, shown as
    /// "me".
    pub fn new(mut message: mailstream::Message, own: &HashSet<String>) -> Self {
        let (mut shown, mut quoted, mut preview) = (Vec::new(), None, SharedString::default());
        if let Some(text) = &message.text {
            let (written, folded) = format::fold(text);
            shown = format::runs(&written);
            quoted = folded.map(Into::into);
            let text = format::body(text);
            preview = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_owned().into();
            message.text = Some(text);
        }
        let body_html = message.html_body().is_some().then(|| message.html.take()).flatten().map(SharedString::from);
        let mut tiles: Vec<Tile> = std::mem::take(&mut message.attachments).into_iter().map(Tile::new).collect();
        if message.invitation.is_some() {
            tiles.retain(|tile| !tile.calendar());
        }
        let mut recipients = String::new();
        for (label, list) in [("to", &message.to), ("cc", &message.cc)] {
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
                .map(|a| if own.contains(&a.normalized_email()) { "me".to_owned() } else { a.display_name() })
                .collect();
            recipients.push_str(&names.join(", "));
        }
        let list = |addresses: &[Address]| addresses.iter().map(Address::text).collect::<Vec<_>>().join(", ");
        let mut headers: Vec<(&'static str, SharedString)> =
            [("From", &message.from), ("To", &message.to), ("Cc", &message.cc)]
                .into_iter()
                .filter(|(_, addresses)| !addresses.is_empty())
                .map(|(label, addresses)| (label, list(addresses).into()))
                .collect();
        if message.date.is_some() {
            headers.push(("Date", mailstream::full_date(message.date.clone()).into()));
        }
        let mine = own.contains(&message.sender().normalized_email());
        ShownMessage { message, body_html, tiles, preview, recipients: recipients.into(), headers, shown, quoted, mine }
    }

    /// Who a reply to this message goes to, as the reader can tell from its headers (Flutter's
    /// `_recipients`): its sender, or for the user's own message its recipients; a reply to all
    /// adds everyone else as Cc. The user's addresses (`own`, lowercased) stay out unless they are
    /// all there is.
    pub fn reply_recipients(&self, own: &HashSet<String>, all: bool) -> (Vec<Address>, Vec<Address>) {
        let mine = |address: &&Address| own.contains(&address.normalized_email());
        let primary = if self.from.iter().any(|address| mine(&address)) { &self.to } else { &self.from };
        let mut seen = HashSet::new();
        let mut to: Vec<Address> =
            primary.iter().filter(|address| !mine(address) && seen.insert(address.normalized_email())).cloned().collect();
        if to.is_empty() {
            to = primary.clone();
            seen.extend(to.iter().map(Address::normalized_email));
        }
        let cc = if all {
            self.to
                .iter()
                .chain(&self.cc)
                .chain(&self.from)
                .filter(|address| !mine(address) && seen.insert(address.normalized_email()))
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
        mailstream::forward_body(
            self.from.clone(),
            self.to.clone(),
            self.cc.clone(),
            self.date.clone(),
            subject.to_owned(),
            self.text.clone().unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message as the engine's `show` gives it.
    fn shown(text: &str, html: Option<&str>) -> ShownMessage {
        let message: mailstream::Message = serde_json::from_value(serde_json::json!({
            "id": "m1", "mailbox": "INBOX", "role": "inbox", "from": ["Jack251970 <notifications@github.com>"],
            "to": ["Flow-Launcher/Flow.Launcher <Flow.Launcher@noreply.github.com>"], "date": "2026-11-12T09:04:00Z",
            "subject": "Re: [Flow-Launcher/Flow.Launcher] new issue (Issue #4754)", "unread": true, "starred": false,
            "text": text, "html": html,
        }))
        .unwrap();
        ShownMessage::new(message, &HashSet::new())
    }

    /// The engine's message from `from` at `at` (RFC 3339), to the user and Maya.
    fn wire(from: &str, at: &str) -> mailstream::Message {
        serde_json::from_value(serde_json::json!({
            "id": at, "mailbox": "INBOX", "role": "inbox", "from": [from],
            "to": ["Reader Test <reader@example.com>", "Maya Lin <maya@example.com>"], "cc": ["QA <qa@example.com>"],
            "date": at, "subject": "Plan", "unread": false, "starred": false, "text": "Hi",
            "attachments": [
                { "filename": "invite.ics", "mime": "text/calendar", "size": 900, "path": "/tmp/invite.ics" },
                { "filename": "plan.pdf", "mime": "application/pdf", "size": 1_200_000, "path": "/tmp/plan.pdf" },
            ],
        }))
        .unwrap()
    }

    fn own() -> HashSet<String> {
        HashSet::from(["reader@example.com".to_owned()])
    }

    #[test]
    fn the_user_reads_as_me_and_the_headers_name_everyone() {
        let message = ShownMessage::new(wire("Alex Rivera <alex@example.com>", "2026-11-12T09:04:00Z"), &own());
        assert_eq!(message.recipients.as_ref(), "to me, Maya Lin · cc QA");
        let headers: Vec<_> = message.headers.iter().map(|(label, value)| (*label, value.as_ref())).collect();
        assert_eq!(headers[..3], [
            ("From", "Alex Rivera <alex@example.com>"),
            ("To", "Reader Test <reader@example.com>, Maya Lin <maya@example.com>"),
            ("Cc", "QA <qa@example.com>"),
        ]);
        assert_eq!(headers[3].0, "Date");
        let pdf = &message.tiles[1];
        assert_eq!((pdf.mark.as_ref(), pdf.kind), ("PDF", AttachmentKind::Pdf));
        assert!(pdf.detail.ends_with(" · PDF"), "{}", pdf.detail);
    }

    #[test]
    fn an_invitation_stands_for_its_calendar_file() {
        let plain = ShownMessage::new(wire("Alex Rivera <alex@example.com>", "2026-11-12T09:04:00Z"), &own());
        assert_eq!(plain.tiles.len(), 2);
        let mut invited = wire("Alex Rivera <alex@example.com>", "2026-11-12T09:04:00Z");
        invited.invitation = Some(serde_json::from_value(serde_json::json!({ "method": "REQUEST", "uid": "u1", "title": "Sync", "start": "2026-11-13T10:00:00Z", "end": "2026-11-13T10:30:00Z" })).unwrap());
        let invited = ShownMessage::new(invited, &HashSet::new());
        let names: Vec<_> = invited.tiles.iter().map(|tile| tile.attachment.filename.as_str()).collect();
        assert_eq!(names, ["plan.pdf"]);
    }

    #[test]
    fn github_notifications_show_their_html() {
        let html = "<p></p>\n<b>Aarush-githubuser</b> created an issue <a href=\"https://github.com/Flow-Launcher/Flow.Launcher/issues/4754\">(Flow-Launcher/Flow.Launcher#4754)</a>";
        // multipart/alternative: GitHub's text part beside its HTML.
        let both = shown("Aarush-githubuser created an issue (Flow-Launcher/Flow.Launcher#4754)", Some(html));
        assert_eq!(both.body_html.as_deref(), Some(html));
        // HTML only (as Outlook hands it out): the engine's text is the HTML's.
        let only = shown(" \nAarush-githubuser  created an issue  (Flow-Launcher/Flow.Launcher#4754)", Some(html));
        assert_eq!(only.body_html.as_deref(), Some(html));
        // Plain mail, or an HTML body with nothing in it, reads as text.
        assert_eq!(shown("Lunch at noon?", None).body_html, None);
        assert_eq!(shown("Lunch at noon?", Some(" \n")).body_html, None);
    }

    #[test]
    fn junk_keeps_its_name() {
        assert_eq!(MailboxRole::Junk.label(), "Junk");
        assert_eq!(MailboxRole::All.label(), "All Mail");
    }

    #[test]
    fn from_me_rows_name_and_avatar_the_peer() {
        let mut sent_only = mailstream::Conversation {
            id: "c1".into(),
            account: "me@example.com".into(),
            subject: "Proposal".into(),
            date: None,
            from: Some(Address { name: Some("Me".into()), email: "me@example.com".into() }),
            from_me: true,
            peers: vec![Address { name: Some("Morgan Lee".into()), email: "morgan@example.com".into() }],
            participants: vec!["me".into()],
            message_count: 1,
            unread: false,
            starred: false,
            has_attachment: false,
            mailboxes: vec![MailboxRole::Sent],
            snippet: None,
            category: None,
            waiting: false,
            labels: vec![],
            started: true,
            tag: String::new(),
            title: String::new(),
            when: String::new(),
            section: String::new(),
            avatar: Default::default(),
        };
        assert_eq!(sent_only.sender_name().as_ref(), "Morgan Lee");
        assert_eq!(sent_only.senders().as_ref(), "Morgan Lee");
        assert_eq!(sent_only.avatar_email().as_ref(), "morgan@example.com");

        sent_only.participants = vec!["Jordan Park".into(), "me".into()];
        sent_only.peers = vec![Address { name: Some("Jordan Park".into()), email: "jordan@example.com".into() }];
        sent_only.message_count = 2;
        assert_eq!(sent_only.sender_name().as_ref(), "Jordan Park");
        assert_eq!(sent_only.senders().as_ref(), "Jordan Park");
        assert_eq!(sent_only.avatar_email().as_ref(), "jordan@example.com");

        // Multiple recipients or a group address: row names all peers (or the group) and keys the avatar on the first.
        sent_only.peers = vec![
            Address { name: Some("Morgan Lee".into()), email: "morgan@example.com".into() },
            Address { name: Some("Devon Kim".into()), email: "devon@example.com".into() },
        ];
        assert_eq!(sent_only.sender_name().as_ref(), "Morgan Lee, Devon Kim");
        assert_eq!(sent_only.senders().as_ref(), "Morgan Lee, Devon Kim");
        assert_eq!(sent_only.avatar_email().as_ref(), "morgan@example.com");

        sent_only.peers = vec![Address { name: Some("Design Team".into()), email: "design@example.com".into() }];
        assert_eq!(sent_only.sender_name().as_ref(), "Design Team");
        assert_eq!(sent_only.senders().as_ref(), "Design Team");
        assert_eq!(sent_only.avatar_email().as_ref(), "design@example.com");
    }
}
