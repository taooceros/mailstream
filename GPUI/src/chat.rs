//! Google Chat through the in-process engine's `chat_*` methods: the space list (direct messages, group
//! chats, spaces; older pages load near its end), a space's log with a composer (pictures inline,
//! other files as tiles; a thread's replies folded under its first message, the thread opening on
//! an island of its own with its own composer, MS-329), and starting a conversation. `ChatPane`
//! fills the main window's content area as mail does (MS-328): the list's island, and the open
//! space (or the new-chat form) on an
//! island beside it in a wide window, over it in a narrow one; the list is as wide as mail's
//! (`set_split`), and its title row, search field and the space's toolbar are mail's (`controls`).
//! The sidebar reads `accounts()` and `unread()` from it. A direct message's toolbar asks the
//! window for the mail exchanged with the other person ([`MailWith`], MS-331).
//!
//! The engine keeps the state, as it does for the native reader: it polls Google for the spaces
//! (every `chat.pollSeconds`), and while a space is open (`chat_open`) it fetches its newest
//! messages, keeps them current and marks them read. The pane reads the list again on the
//! engine's `Chat` event and the open space's history on `ChatMessages`. Engine calls run on the
//! background executor.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::component::input::{EditorState, Enter, Escape, InputEvent, InputState};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, ClipboardItem, Context, DispatchPhase, Div, ElementId, Entity,
    EntityInputHandler as _, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle,
    HitboxBehavior, ImgResourceLoader, InteractiveElement, InteractiveText, IntoElement, KeyBinding, ListAlignment,
    ListState, LongPressEvent, MouseButton, MouseDownEvent, NavigationDirection, ObjectFit, ParentElement, Pixels,
    Point, Render, Resource, ScrollEvent, ScrollStrategy, SharedString, StatefulInteractiveElement,
    StrikethroughStyle, Styled, StyledImage, StyledText, Subscription, TouchPhase, UniformListScrollHandle, Window,
    actions, canvas, div, hsla, img, list, px, relative, uniform_list,
};

use crate::avatars;
use crate::controls::{
    ButtonKind, ControlSize, Tone, button, capsule, centered_note, compact_capsule, compact_icon_button, hairline,
    icon_button, island, list_header, list_row, mark_dot, notice, page_scrim, page_toolbar, pill, raised_capsule,
    row_layout, scrim, search_field_for,
};
use crate::design::{self, AttachmentKind, Palette, chat as metrics, icon as icon_size, layout, list as rows, log as log_metrics, radius, space, stroke, text};
use crate::message_log;
use crate::icons::{Icon, icon};
use crate::engine::{MailError, engine};
use crate::input::{self, wrap_selection};
use crate::markup_highlight::{MarkupFlavor, markup_highlighter_factory};
use crate::split;
use crate::swipe::{self, BackSwipe, MouseDrag, Pointer, Preferences, Slide, Step};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::video::{self, ChatVideo, VideoLightbox};
use crate::state::ReaderTop;

actions!(chat, [Back, Next, Previous, Open, NewChat, Refresh, FormatBold, FormatItalic, FormatStrike, FormatCode]);

const CONTEXT: &str = "Chat";
const COMPOSER_CONTEXT: &str = "ChatComposer";
/// Typing pauses this long before people are looked up.
const SEARCH_DELAY: Duration = Duration::from_millis(120);
const SUGGESTIONS: u32 = 8;
/// Log height measured beyond the visible part, so scrolling does not pop.
const OVERDRAW: f32 = 800.0;

pub fn bind_keys(cx: &mut App) {
    // Single keys and Back act on the list/space, never while typing in a field or composer.
    let list = Some("Chat && !Input && !ChatComposer && !Expander && !VideoLightbox");
    let pane = Some("Chat && !VideoLightbox");
    let composer = Some("ChatComposer > Input");
    cx.bind_keys([
        KeyBinding::new("escape", Back, list),
        KeyBinding::new(secondary!("["), Back, list),
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new(secondary!("n"), NewChat, pane),
        KeyBinding::new(secondary!("r"), Refresh, pane),
        KeyBinding::new(secondary!("b"), FormatBold, composer),
        KeyBinding::new(secondary!("i"), FormatItalic, composer),
        KeyBinding::new(secondary!("shift-x"), FormatStrike, composer),
        KeyBinding::new(secondary!("e"), FormatCode, composer),
    ]);
}

// MARK: - What the engine returns

/// A Google Chat account, for the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatAccountInfo {
    pub id: SharedString,
    pub label: SharedString,
}

/// A Chat account as the pane keeps it: what the sidebar shows plus whether it may start chats.
#[derive(Clone, PartialEq)]
struct Account {
    id: SharedString,
    label: SharedString,
    /// Its sign-in may create direct messages and group chats.
    can_start: bool,
}

impl From<mailstream::ChatAccount> for Account {
    fn from(account: mailstream::ChatAccount) -> Self {
        Account { id: account.id.into(), label: account.label.into(), can_start: account.can_start }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Key {
    account: SharedString,
    space: SharedString,
}

#[derive(Clone, PartialEq)]
struct Space {
    account: SharedString,
    id: SharedString,
    kind: mailstream::ChatSpaceKind,
    title: SharedString,
    snippet: SharedString,
    unread: bool,
    last_active: Option<DateTime<Utc>>,
    avatar: mailstream::Avatar,
    /// The address whose photo the row shows: the person's, when the engine knows it.
    avatar_email: SharedString,
    /// The row's date, as the engine labels it.
    when: SharedString,
}

impl Space {
    fn new(space: mailstream::ChatSpace) -> Self {
        let last_active = date(space.activity);
        Space {
            account: space.account.into(),
            id: space.space.into(),
            kind: space.kind,
            title: space.title.into(),
            snippet: space.preview.into(),
            unread: space.unread,
            last_active,
            avatar_email: if space.avatar_key.contains('@') { space.avatar_key.into() } else { SharedString::default() },
            avatar: space.avatar,
            when: space.date.into(),
        }
    }

    fn key(&self) -> Key {
        Key { account: self.account.clone(), space: self.id.clone() }
    }

    fn is(&self, key: &Key) -> bool {
        self.account == key.account && self.id == key.space
    }

    fn kind_title(&self) -> &'static str {
        match self.kind {
            mailstream::ChatSpaceKind::Dm => "Direct message",
            mailstream::ChatSpaceKind::Group => "Group chat",
            mailstream::ChatSpaceKind::Bot => "App",
            mailstream::ChatSpaceKind::Space => "Space",
        }
    }
}

/// A space with unread messages, as the inbox's Chat bundle lists it (MS-330).
#[derive(Clone, PartialEq)]
pub struct UnreadSpace {
    pub account: SharedString,
    pub space: SharedString,
    pub title: SharedString,
    pub snippet: SharedString,
    pub avatar: mailstream::Avatar,
    /// Whose photo the row shows, as Chat's own row (empty: the monogram).
    pub avatar_email: SharedString,
    pub when: SharedString,
}

impl UnreadSpace {
    /// Its row's id in the bundle's list.
    pub fn row_id(&self) -> String {
        format!("chat:{}:{}", self.account, self.space)
    }
}

/// Chat asks the window to show the mail exchanged with this address (MS-331): a direct message's
/// "Mail with …" button.
pub struct MailWith(pub SharedString);

impl EventEmitter<MailWith> for ChatPane {}

/// Chat asks the window to open the slide-over navigation drawer in compact mode (MS-408).
pub struct ChatOpenDrawer;

impl EventEmitter<ChatOpenDrawer> for ChatPane {}

/// Chat asks the window to show a transient toast (e.g. "Copied", MS-436).
pub struct ChatToast(pub String);

impl EventEmitter<ChatToast> for ChatPane {}

#[derive(Clone, PartialEq)]
struct User {
    /// `users/<id>`.
    id: SharedString,
    label: SharedString,
    avatar: mailstream::Avatar,
    /// Their address, for their photo; empty when Chat doesn't give it.
    email: SharedString,
}

impl From<mailstream::ChatUser> for User {
    fn from(user: mailstream::ChatUser) -> Self {
        User { id: user.id.into(), label: user.name.into(), avatar: user.avatar, email: user.email.unwrap_or_default().into() }
    }
}

#[derive(Clone)]
struct Attachment {
    name: SharedString,
    /// Where it opens in the browser (Chat's download link, or the Drive file); None when the API
    /// gives none.
    url: Option<SharedString>,
    /// A file uploaded to Chat, which `chat_attachment` downloads; None for Drive files.
    resource: Option<SharedString>,
    kind: AttachmentKind,
    /// The tile's second line: "Image", "PDF", "Google Drive", the file's extension.
    label: SharedString,
}

impl From<mailstream::ChatAttachment> for Attachment {
    fn from(attachment: mailstream::ChatAttachment) -> Self {
        let (kind, label) =
            AttachmentKind::of(&attachment.name, attachment.content_type.as_deref().unwrap_or_default(), attachment.url.as_deref());
        Attachment {
            name: attachment.name.into(),
            url: attachment.url.map(Into::into),
            resource: attachment.resource.map(Into::into),
            kind,
            label,
        }
    }
}

impl Attachment {
    /// An uploaded picture shows in the log; everything else is a tile, except an uploaded video,
    /// which plays in the log where the platform has a player (`video.rs`, Linux and macOS).
    fn inline(&self) -> bool {
        self.kind == AttachmentKind::Image && self.resource.is_some()
    }
}

/// An uploaded attachment: its account and resource.
type FileKey = (SharedString, SharedString);

/// An uploaded attachment's download (`chat_attachment`); the engine keeps the file once fetched.
enum FileState {
    Loading,
    Ready(Arc<Path>),
    Failed(SharedString),
}

/// An ISO 8601 time from the engine; None when absent or unreadable.
fn date(raw: Option<String>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&raw?).ok().map(|date| date.with_timezone(&Utc))
}

/// Someone `search_contacts` found, or an address typed in full.
#[derive(Clone, PartialEq)]
struct Person {
    name: Option<SharedString>,
    email: SharedString,
}

impl From<mailstream::Person> for Person {
    fn from(person: mailstream::Person) -> Self {
        Person { name: person.name.map(Into::into), email: person.email.into() }
    }
}

impl Person {
    fn label(&self) -> SharedString {
        self.name.clone().unwrap_or_else(|| self.email.clone())
    }

    /// A Chat partner, offered only when Chat knows their address.
    fn partner(user: mailstream::ChatUser) -> Option<Self> {
        let email = user.email?;
        Some(Person { name: (!user.name.is_empty()).then(|| user.name.into()), email: email.into() })
    }
}

// MARK: - Message text

/// How a run of a message's text is drawn, as the engine laid it out (`mailstream::ChatSpan`).
#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Mark {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    mention: bool,
    faint: bool,
}

/// A paragraph's text with its styled ranges and the targets of its links, in order.
#[derive(Clone, Default, Debug)]
struct Inline {
    text: SharedString,
    /// Sorted, never overlapping.
    marks: Vec<(Range<usize>, Mark)>,
    links: Vec<SharedString>,
}

impl Inline {
    fn new(spans: &[mailstream::ChatSpan]) -> Self {
        let mut text = String::new();
        let (mut marks, mut links) = (Vec::new(), Vec::new());
        for span in spans {
            let start = text.len();
            text.push_str(&span.text);
            let mark = Mark {
                bold: span.style.bold,
                italic: span.style.italic,
                strike: span.style.strike,
                code: span.style.code,
                link: span.style.link.is_some(),
                mention: span.style.mention,
                faint: false,
            };
            if mark != Mark::default() {
                marks.push((start..text.len(), mark));
            }
            if let Some(link) = &span.style.link {
                links.push(SharedString::from(link.clone()));
            }
        }
        Inline { text: text.into(), marks, links }
    }

    fn push(&mut self, text: &str, mark: Mark) {
        let mut owned = self.text.to_string();
        let start = owned.len();
        owned.push_str(text);
        self.marks.push((start..owned.len(), mark));
        self.text = owned.into();
    }

    fn link_ranges(&self) -> Vec<Range<usize>> {
        self.marks.iter().filter(|(_, mark)| mark.link).map(|(range, _)| range.clone()).collect()
    }

    /// The text, its links opening in the browser.
    fn element(&self, id: ElementId, p: &Palette) -> AnyElement {
        let styled = StyledText::new(self.text.clone()).with_highlights(self.marks.iter().map(|(range, mark)| (range.clone(), mark_style(*mark, p))));
        let links = self.link_ranges();
        if links.is_empty() {
            return styled.into_any_element();
        }
        let urls = self.links.clone();
        InteractiveText::new(id, styled)
            .on_click(links, move |index, _, cx| {
                if let Some(url) = urls.get(index) {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    }
}

#[derive(Clone, Debug)]
enum PieceKind {
    Text,
    Heading(u8),
    Code,
    Rule,
    /// Rows of cells, the header row first.
    Table(Vec<Vec<Inline>>),
}

/// One block of a message (`mailstream::ChatBlock`).
#[derive(Clone, Debug)]
struct Piece {
    kind: PieceKind,
    inline: Inline,
    quote: u32,
    indent: u32,
    marker: Option<SharedString>,
    tight: bool,
}

/// A message's text, laid out by the engine (`chat_body`): blocks one below the other.
#[derive(Clone, Default, Debug)]
struct Body {
    pieces: Vec<Piece>,
}

impl Body {
    fn new(blocks: &[mailstream::ChatBlock], edited: bool) -> Self {
        let mut pieces: Vec<Piece> = blocks
            .iter()
            .map(|block| Piece {
                kind: match &block.kind {
                    mailstream::ChatBlockKind::Paragraph => PieceKind::Text,
                    mailstream::ChatBlockKind::Heading { level } => PieceKind::Heading(*level),
                    mailstream::ChatBlockKind::Code => PieceKind::Code,
                    mailstream::ChatBlockKind::Rule => PieceKind::Rule,
                    mailstream::ChatBlockKind::Table => PieceKind::Table(
                        block.rows.iter().map(|row| row.iter().map(|cell| Inline::new(cell)).collect()).collect(),
                    ),
                },
                inline: Inline::new(&block.spans),
                quote: block.quote,
                indent: block.indent,
                marker: block.marker.clone().map(Into::into),
                tight: block.tight,
            })
            .collect();
        if edited {
            let faint = Mark { faint: true, ..Mark::default() };
            match pieces.last_mut().filter(|piece| matches!(piece.kind, PieceKind::Text)) {
                Some(last) => last.inline.push(" (edited)", faint),
                None => pieces.push(Piece::text(Inline::default(), "(edited)", faint)),
            }
        }
        Body { pieces }
    }

    /// One line in italics, for what has no text (a card).
    fn note(text: &str) -> Self {
        Body { pieces: vec![Piece::text(Inline::default(), text, Mark { italic: true, ..Mark::default() })] }
    }

    fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Plain text of the message body, for copying and the mobile action sheet preview (MS-436).
    fn plain_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for piece in &self.pieces {
            match &piece.kind {
                PieceKind::Rule => {}
                PieceKind::Table(rows) => {
                    for row in rows {
                        let line = row.iter().map(|cell| cell.text.as_ref()).collect::<Vec<_>>().join("\t");
                        if !line.trim().is_empty() {
                            lines.push(line);
                        }
                    }
                }
                PieceKind::Text | PieceKind::Heading(_) | PieceKind::Code => {
                    let text = piece.inline.text.as_ref();
                    if !text.is_empty() {
                        lines.push(text.to_owned());
                    }
                }
            }
        }
        lines.join("\n")
    }

    /// The blocks, `id` naming their links' elements.
    fn element(&self, id: &str, p: &Palette) -> Div {
        div().max_w_full().min_w_0().flex().flex_col().children(self.pieces.iter().enumerate().map(|(index, piece)| {
            let element_id = ElementId::Name(format!("chat-text:{id}:{index}").into());
            let gap = if index == 0 { 0. } else if piece.tight { 2. } else { 6. };
            let content: AnyElement = match &piece.kind {
                PieceKind::Text => piece.inline.element(element_id, p),
                PieceKind::Heading(level) => div()
                    .min_w_0()
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(text::READING * match level { 1 => 1.3, 2 => 1.18, 3 => 1.08, _ => 1.0 }))
                    .child(piece.inline.element(element_id, p))
                    .into_any_element(),
                PieceKind::Code => div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(radius::CONTROL))
                    .bg(p.chip)
                    .font_family(design::mono_font())
                    .text_size(px(text::READING * 0.86))
                    .child(piece.inline.text.clone())
                    .into_any_element(),
                PieceKind::Rule => div().w_full().my(px(4.)).h(px(1.)).bg(p.line).into_any_element(),
                PieceKind::Table(rows) => div()
                    .w_full()
                    .max_w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(p.line)
                    .overflow_hidden()
                    .children(rows.iter().enumerate().map(|(r, row)| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .when(r == 0, |d| d.bg(p.chip).font_weight(FontWeight::SEMIBOLD))
                            .when(r > 0, |d| d.border_t_1().border_color(p.line))
                            .children(row.iter().enumerate().map(|(c, cell)| {
                                let cell_id = ElementId::Name(format!("chat-text:{id}:{index}:{r}:{c}").into());
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .px(px(10.))
                                    .py(px(4.))
                                    .when(c > 0, |d| d.border_l_1().border_color(p.line))
                                    .child(cell.element(cell_id, p))
                            }))
                    }))
                    .into_any_element(),
            };
            let mut block = div().w_full().min_w_0().child(content);
            if piece.indent > 0 {
                let marker = div()
                    .flex_shrink_0()
                    .w(px(20.))
                    .text_color(p.muted)
                    .children(piece.marker.clone());
                block = div().w_full().min_w_0().pl(px(20. * (piece.indent - 1) as f32)).flex().child(marker).child(div().flex_1().min_w_0().child(block));
            }
            for _ in 0..piece.quote {
                block = div().w_full().min_w_0().border_l(px(3.)).border_color(p.line).pl(px(10.)).text_color(p.muted).child(block);
            }
            block.mt(px(gap))
        }))
    }
}

impl Piece {
    fn text(mut inline: Inline, text: &str, mark: Mark) -> Self {
        inline.push(text, mark);
        Piece { kind: PieceKind::Text, inline, quote: 0, indent: 0, marker: None, tight: false }
    }
}

fn mark_style(mark: Mark, p: &Palette) -> HighlightStyle {
    let mut style = HighlightStyle::default();
    if mark.bold {
        style.font_weight = Some(FontWeight::BOLD);
    }
    if mark.italic {
        style.font_style = Some(FontStyle::Italic);
    }
    if mark.strike {
        style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
    }
    if mark.code {
        style.background_color = Some(p.chip);
    }
    if mark.link {
        style.color = Some(p.accent);
    }
    if mark.mention {
        style.color = Some(p.accent);
        style.font_weight = Some(FontWeight::SEMIBOLD);
    }
    if mark.faint {
        style.color = Some(p.faint);
    }
    style
}

// MARK: - The log

enum Status {
    Sent,
    Sending,
    Failed(SharedString),
}

/// One message as the log draws it, derived off the main thread.
struct Msg {
    id: SharedString,
    sender_id: SharedString,
    /// "You" for the user's own messages.
    name: SharedString,
    avatar: mailstream::Avatar,
    /// The sender's address, for their photo; empty when Chat doesn't give it.
    email: SharedString,
    created: Option<DateTime<Utc>>,
    time: SharedString,
    thread: Option<SharedString>,
    thread_reply: bool,
    mine: bool,
    body: Body,
    attachments: Vec<Attachment>,
    status: Status,
    /// Sent text, kept to send again after a failure.
    draft: Option<(String, Option<SharedString>)>,
}

impl Msg {
    fn from_wire(message: mailstream::ChatMessage) -> Self {
        let created = date(message.created.clone());
        let time = mailstream::message_time(message.created).into();
        let sender = User::from(message.sender);
        let body = if message.card && message.body.is_empty() {
            Body::note("Card message")
        } else {
            Body::new(&message.body, message.edited.is_some())
        };
        Msg {
            name: if message.mine { "You".into() } else { sender.label.clone() },
            avatar: sender.avatar,
            email: sender.email,
            sender_id: sender.id,
            id: message.id.into(),
            created,
            time,
            thread: message.thread.map(Into::into),
            thread_reply: message.thread_reply,
            mine: message.mine,
            body,
            attachments: message.attachments.into_iter().map(Attachment::from).collect(),
            status: Status::Sent,
            draft: None,
        }
    }

    /// Who mentions in a message from here name: members and anyone who wrote in the space.
    fn names(log: &Log) -> HashMap<String, String> {
        let mut names: HashMap<String, String> = log.members.iter().map(|user| (user.id.to_string(), user.label.to_string())).collect();
        for message in &log.messages {
            names.entry(message.sender_id.to_string()).or_insert_with(|| message.name.to_string());
        }
        names
    }
}

/// A space's history as the engine holds it, converted off the main thread.
struct Loaded {
    title: Option<SharedString>,
    members: Vec<User>,
    messages: Vec<Msg>,
    last_read: Option<DateTime<Utc>>,
    more: bool,
    loaded: bool,
    failure: Option<SharedString>,
    loading_earlier: bool,
}

impl Loaded {
    fn new(thread: mailstream::ChatThread) -> Self {
        let members: Vec<User> = thread.members.into_iter().map(User::from).collect();
        Loaded {
            title: thread.title.map(Into::into),
            messages: thread.messages.into_iter().map(Msg::from_wire).collect(),
            members,
            last_read: date(thread.last_read),
            more: thread.more,
            loaded: thread.loaded,
            failure: thread.failure.map(Into::into),
            loading_earlier: thread.loading_earlier,
        }
    }
}

/// A thread's replies, drawn under the message that started it rather than in the log (MS-329).
struct Replies {
    /// The replies' indices (`Log::entry`), oldest first, pending ones last.
    entries: Vec<usize>,
    /// When the latest came, as the list dates its rows ("4:05 PM", "Nov 7").
    last: SharedString,
    /// Who replied, the latest first, each once and at most `metrics::THREAD_FACES`: their
    /// address (for the photo) and avatar.
    faces: Vec<(SharedString, mailstream::Avatar)>,
}

/// Each entry's id and how many replies it shows, as a list last measured them: an entry whose
/// pair changed is measured again.
type Measured = Vec<(SharedString, usize)>;

/// A reply its log draws under its thread's first message (loaded, among `starters`) instead of
/// in the log itself; a reply to a message not loaded stays in the log.
fn folded(message: &Msg, starters: &HashSet<&SharedString>) -> bool {
    message.thread_reply && message.thread.as_ref().is_some_and(|thread| starters.contains(thread))
}

/// The threads (by name) whose first message is among `messages`.
fn starters<'a>(messages: impl IntoIterator<Item = &'a Msg>) -> HashSet<&'a SharedString> {
    messages.into_iter().filter(|m| !m.thread_reply).filter_map(|m| m.thread.as_ref()).collect()
}

/// Tells `list` (item 0 its top, then one item per entry) which entries changed from `before` to
/// `after`, so only those are measured again; the top is measured again always.
fn splice(list: &ListState, before: &[(SharedString, usize)], after: &[(SharedString, usize)]) {
    let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let room = before.len().min(after.len()) - prefix;
    let mut suffix = before.iter().rev().zip(after.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let changed = before.len() != after.len() || prefix < before.len();
    // The first entry after a change may join or leave a run: it is measured again too.
    if changed && suffix > 0 {
        suffix -= 1;
    }
    if changed {
        list.splice(1 + prefix..1 + before.len() - suffix, after.len() - prefix - suffix);
    }
    list.splice(0..1, 1);
}

/// Each of `entries`' place in a log (`mailstream::chat_runs`), the one with id `opens` starting
/// a run of its own.
fn runs<'a>(entries: impl Iterator<Item = &'a Msg>, opens: Option<&SharedString>) -> Vec<mailstream::ChatRun> {
    mailstream::chat_runs(
        entries
            .map(|message| mailstream::ChatRunEntry {
                sender: message.sender_id.to_string(),
                date: message.created.map(|date| date.to_rfc3339()),
                opens_run: opens == Some(&message.id),
            })
            .collect(),
    )
}

/// The thread open beside the log (MS-329): the message that started it, when it is loaded, then
/// its replies.
struct ThreadView {
    /// The thread's name (`spaces/…/threads/…`).
    id: SharedString,
    /// Indices (`Log::entry`) of its messages, oldest first.
    entries: Vec<usize>,
    /// `entries` starts with the message that started the thread.
    starter: bool,
    runs: Vec<mailstream::ChatRun>,
    measured: Measured,
    /// Item 0 is the top (under the toolbar); items 1… are the entries.
    list: ListState,
}

/// One opened space's loaded messages and its list state.
struct Log {
    title: Option<SharedString>,
    members: Vec<User>,
    /// Ascending by creation, as the engine holds them.
    messages: Vec<Msg>,
    /// Sent from here, not confirmed yet (or failed); drawn after `messages`.
    pending: Vec<Msg>,
    /// The entries the log shows (indices for `entry`): every message but the replies folded
    /// under their thread's first message.
    shown: Vec<usize>,
    /// Each shown entry's place in the log (`mailstream::chat_runs`).
    runs: Vec<mailstream::ChatRun>,
    /// The shown entries as the list last measured them.
    measured: Measured,
    /// Every thread with replies loaded, by name.
    threads: HashMap<SharedString, Replies>,
    /// The thread open beside the log.
    thread: Option<ThreadView>,
    /// Older messages exist before the first one here.
    more: bool,
    loaded: bool,
    loading_earlier: bool,
    failure: Option<SharedString>,
    /// The first message that was unread when the space opened, under a "New" line.
    first_unread: Option<SharedString>,
    /// The space was opened and no history with messages has arrived since: the next one places
    /// the "New" line.
    place_new_line: bool,
    /// The newest request for the history, and the newest one applied; older answers are dropped.
    requested: u64,
    applied: u64,
    /// Item 0 is the top (earlier messages, or the start); items 1… are the shown entries.
    list: ListState,
}

impl Log {
    fn new() -> Self {
        Log {
            title: None,
            members: Vec::new(),
            messages: Vec::new(),
            pending: Vec::new(),
            shown: Vec::new(),
            runs: Vec::new(),
            measured: Vec::new(),
            threads: HashMap::new(),
            thread: None,
            more: false,
            loaded: false,
            loading_earlier: false,
            failure: None,
            first_unread: None,
            place_new_line: false,
            requested: 0,
            applied: 0,
            list: ListState::new(1, ListAlignment::Bottom, px(OVERDRAW)),
        }
    }

    fn len(&self) -> usize {
        self.messages.len() + self.pending.len()
    }

    fn entry(&self, ix: usize) -> Option<&Msg> {
        self.messages.get(ix).or_else(|| self.pending.get(ix.checked_sub(self.messages.len())?))
    }

    /// The replies drawn under `message`, the first of its thread.
    fn replies(&self, message: &Msg) -> Option<&Replies> {
        self.threads.get(message.thread.as_ref().filter(|_| !message.thread_reply)?)
    }

    /// Opens `thread` beside the log.
    fn open_thread(&mut self, thread: SharedString) {
        self.thread = Some(ThreadView {
            id: thread,
            entries: Vec::new(),
            starter: false,
            runs: Vec::new(),
            measured: Vec::new(),
            list: ListState::new(1, ListAlignment::Bottom, px(OVERDRAW)),
        });
        self.relayout();
    }

    /// Groups the replies under their threads, recomputes the runs and tells the lists which
    /// entries changed since they were last laid out, so only those are measured again.
    fn relayout(&mut self) {
        let all = || self.messages.iter().chain(&self.pending);
        let starters = starters(all());
        let mut shown = Vec::with_capacity(self.len());
        let mut replies: HashMap<SharedString, Vec<usize>> = HashMap::new();
        for (ix, message) in all().enumerate() {
            if let Some(thread) = message.thread.as_ref().filter(|_| message.thread_reply) {
                replies.entry(thread.clone()).or_default().push(ix);
            }
            if !folded(message, &starters) {
                shown.push(ix);
            }
        }
        let threads: HashMap<SharedString, Replies> = replies
            .into_iter()
            .map(|(thread, entries)| {
                let latest = entries.iter().rev().filter_map(|&ix| self.entry(ix));
                let last = latest.clone().next().and_then(|m| m.created).map(|date| mailstream::list_date(Some(date.to_rfc3339())));
                let mut seen = HashSet::new();
                let faces = latest
                    .filter(|m| seen.insert(m.sender_id.clone()))
                    .take(metrics::THREAD_FACES)
                    .map(|m| (m.email.clone(), m.avatar.clone()))
                    .collect();
                (thread, Replies { entries, last: last.unwrap_or_default().into(), faces })
            })
            .collect();

        self.runs = runs(shown.iter().filter_map(|&ix| self.entry(ix)), self.first_unread.as_ref());
        let measured: Measured = shown
            .iter()
            .filter_map(|&ix| self.entry(ix))
            .map(|m| {
                let replies = m.thread.as_ref().filter(|_| !m.thread_reply).and_then(|t| threads.get(t)).map_or(0, |r| r.entries.len());
                (m.id.clone(), replies)
            })
            .collect();
        splice(&self.list, &self.measured, &measured);
        self.measured = measured;
        self.shown = shown;

        if let Some(view) = &self.thread {
            let starter = all().position(|m| !m.thread_reply && m.thread.as_ref() == Some(&view.id));
            let entries: Vec<usize> =
                starter.into_iter().chain(threads.get(&view.id).into_iter().flat_map(|r| r.entries.iter().copied())).collect();
            let runs = runs(entries.iter().filter_map(|&ix| self.entry(ix)), None);
            let measured: Measured = entries.iter().filter_map(|&ix| self.entry(ix)).map(|m| (m.id.clone(), 0)).collect();
            splice(&view.list, &view.measured, &measured);
            if let Some(view) = &mut self.thread {
                view.starter = starter.is_some();
                view.entries = entries;
                view.runs = runs;
                view.measured = measured;
            }
        }
        self.threads = threads;
    }

    /// Takes the engine's history in place of what was loaded.
    fn apply(&mut self, loaded: Loaded) {
        if self.place_new_line && !loaded.messages.is_empty() {
            self.place_new_line = false;
            // Among what the log shows: a reply folded under its thread opens no run there.
            let starters = starters(&loaded.messages);
            self.first_unread = loaded.last_read.and_then(|read| {
                loaded
                    .messages
                    .iter()
                    .find(|m| !m.mine && m.created.is_some_and(|c| c > read) && !folded(m, &starters))
                    .map(|m| m.id.clone())
            });
        }
        self.title = loaded.title;
        self.members = loaded.members;
        self.messages = loaded.messages;
        self.more = loaded.more;
        self.loaded = loaded.loaded;
        self.failure = loaded.failure;
        self.loading_earlier = loaded.loading_earlier;
        self.relayout();
    }
}

// MARK: - The pane

/// What the page beside (or over) the list shows.
enum View {
    /// Nothing: the list takes the whole pane.
    Spaces,
    Space(Key),
    NewChat,
}

/// Where a message is drawn, and which composer writes there: the space's log, or the thread
/// open beside it (MS-329).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Surface {
    Log,
    Thread,
}

/// The new-chat form.
#[derive(Default)]
struct NewChatForm {
    account: Option<SharedString>,
    people: Vec<Person>,
    /// The account's Chat partners (`chat_people`), most recently active first.
    partners: Vec<Person>,
    /// What the contact lookup found for the text typed now.
    contacts: Vec<Person>,
    suggestions: Vec<Person>,
    /// Bumped per keystroke; a lookup answering for an older one is dropped.
    generation: u64,
    starting: bool,
    failure: Option<SharedString>,
}

/// Which surface inside Chat a back-swipe, mouse drag, or slide animation targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SwipeTarget {
    /// The open space or new-chat form (closes back to `View::Spaces`).
    Page,
    /// The open thread inside the open space (closes back to the space).
    Thread,
}

/// The message whose Android/mobile action sheet is open (MS-436).
#[derive(Clone, PartialEq, Eq)]
struct MessageSheet {
    key: Key,
    surface: Surface,
    message_id: SharedString,
}

pub struct ChatPane {
    focus: FocusHandle,
    composer: Entity<EditorState>,
    /// The open thread's composer, which replies in it (MS-329).
    thread_composer: Entity<EditorState>,
    people_input: Entity<InputState>,
    /// Why the space list failed to load, until a load succeeds.
    failure: Option<SharedString>,
    /// The first space list arrived.
    loaded: bool,
    loading: bool,
    /// The engine changed the list while it was being read: read it again after.
    reload: bool,
    chat_accounts: Vec<Account>,
    accounts: Vec<ChatAccountInfo>,
    /// With no chat account: the Google accounts that could connect Chat (MS-306), as
    /// `ChatAccountInfo`s.
    chat_offers: Vec<ChatAccountInfo>,
    /// The account whose Chat is being connected in the browser, and why the last try failed.
    connecting: Option<SharedString>,
    connect_failure: Option<SharedString>,
    /// Every signed-in account in sidebar order, for account colors.
    account_order: Vec<SharedString>,
    spaces: Vec<Space>,
    /// Older spaces exist beyond the loaded ones (`chat_more`).
    more: bool,
    loading_more: bool,
    /// The account in view; None shows every account's spaces.
    filter: Option<SharedString>,
    /// Indices into `spaces` of the rows in view.
    visible: Vec<usize>,
    highlight: Option<Key>,
    scroll: UniformListScrollHandle,
    view: View,
    /// The list's search field, and what it holds, lowercased: the rows in view match it.
    search: Entity<InputState>,
    query: String,
    /// The pane's width, the list's width beside the page, and whether the page sits beside the
    /// list (a wide window) or over it, as the window lays out mail (`set_split`).
    width: f32,
    list_width: f32,
    two_pane: bool,
    /// Back-swipe (two-finger trackpad / Android touch) and mouse drag closing the open page or
    /// thread, matching mail detail (`swipe.rs`, MS-435).
    swipe: BackSwipe,
    drag: MouseDrag,
    slide: Slide,
    gesture_target: RefCell<Option<SwipeTarget>>,
    slide_target: Option<SwipeTarget>,
    predictive_back: crate::swipe::PredictiveBack,
    /// How far the open space's log is scrolled, for its toolbar, and the height of its heading
    /// (the title and its pills) as last laid out.
    log_top: ReaderTop,
    head_height: Rc<Cell<f32>>,
    logs: HashMap<Key, Log>,
    /// The message under the pointer, and where.
    hovered_message: Option<(Surface, SharedString)>,
    /// The message whose Android/mobile action sheet is open (MS-436).
    message_sheet: Option<MessageSheet>,
    files: HashMap<FileKey, FileState>,
    /// Files to open in the system's default app once downloaded.
    open_when_ready: HashSet<FileKey>,
    /// Videos waiting for their file, which they play once downloaded (MS-221).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    video_waiting: HashMap<FileKey, gpui::WeakEntity<ChatVideo>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    video_lightbox: Option<Entity<VideoLightbox>>,
    form: NewChatForm,
    pending_serial: u64,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ChatPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChatPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::new_without_load(window, cx);
        this.load_spaces(cx);
        this.background(cx, async { Preferences::read() }, |this, preferences, _| this.swipe.preferences = preferences);
        this
    }

    fn new_without_load(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = Self::new_composer("Message", window, cx);
        let thread_composer = Self::new_composer("Reply in thread", window, cx);
        let people_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name or email address"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search conversations"));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.subscribe_in(&thread_composer, window, Self::on_composer_event),
            cx.subscribe_in(&people_input, window, Self::on_people_event),
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.observe_window_appearance(window, |this, _, cx| {
                for composer in [&this.composer, &this.thread_composer] {
                    composer.update(cx, |state, cx| state.set_highlighter("google-chat", cx));
                }
                cx.notify();
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            composer,
            thread_composer,
            people_input,
            failure: None,
            loaded: false,
            loading: false,
            reload: false,
            chat_accounts: Vec::new(),
            accounts: Vec::new(),
            chat_offers: Vec::new(),
            connecting: None,
            connect_failure: None,
            account_order: Vec::new(),
            spaces: Vec::new(),
            more: false,
            loading_more: false,
            filter: None,
            visible: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            view: View::Spaces,
            search,
            query: String::new(),
            width: layout::MIN_LIST + layout::GAP + layout::MIN_DETAIL,
            list_width: layout::MIN_LIST,
            two_pane: true,
            swipe: BackSwipe::default(),
            drag: MouseDrag::default(),
            slide: Slide::Rest,
            gesture_target: RefCell::new(None),
            slide_target: None,
            predictive_back: crate::swipe::PredictiveBack::default(),
            log_top: ReaderTop::Top,
            head_height: Rc::new(Cell::new(0.)),
            logs: HashMap::new(),
            hovered_message: None,
            message_sheet: None,
            files: HashMap::new(),
            open_when_ready: HashSet::new(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            video_waiting: HashMap::new(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            video_lightbox: None,
            form: NewChatForm::default(),
            pending_serial: 0,
            _subscriptions: subscriptions,
        }
    }

    /// A composer: a field growing with its text that sends on Return, its Google Chat markup
    /// highlighted as typed (MS-316).
    fn new_composer(placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
        cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .indent_guides(false)
                .smart_indent(false)
                .auto_close(false)
                .soft_wrap(true)
                .searchable(false)
                .scroll_beyond_last_line(Some(0))
                .placeholder(placeholder)
                .submit_on_enter(true)
                .autocorrect(true);
            state.set_highlighter_factory(markup_highlighter_factory(MarkupFlavor::GoogleChat), cx);
            state.set_highlighter("google-chat", cx);
            state
        })
    }

    /// Shows the space list of every chat account (None) or of one.
    pub fn show(&mut self, account: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_video_lightbox(window, cx);
        self.leave_space(cx);
        self.filter = account;
        self.view = View::Spaces;
        self.slide = Slide::Rest;
        self.slide_target = None;
        self.rebuild_visible();
        window.focus(&self.focus, cx);
        // Selecting Chat fetches every account's spaces now, as the native app does.
        self.refresh(&Refresh, window, cx);
        cx.notify();
    }

    /// Leaves Chat for mail or the calendar: closes the open space in the engine so fast polling
    /// and automatic read marks stop while Chat is hidden.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.leave_space(cx);
        self.view = View::Spaces;
        self.slide = Slide::Rest;
        self.slide_target = None;
    }

    /// Whether the sidebar lists Chat: an account uses it, or a Google account could connect it.
    pub fn shown(&self) -> bool {
        !self.accounts.is_empty() || !self.chat_offers.is_empty()
    }

    pub fn accounts(&self) -> &[ChatAccountInfo] {
        &self.accounts
    }

    /// Spaces with unread messages, of one account or all.
    pub fn unread(&self, account: Option<&str>) -> usize {
        self.spaces
            .iter()
            .filter(|space| space.unread && account.is_none_or(|a| space.account.as_ref() == a))
            .count()
    }

    /// The spaces with unread messages, in the list's order (newest activity first), for the
    /// inbox's Chat bundle (MS-330).
    pub fn unread_spaces(&self) -> Vec<UnreadSpace> {
        self.spaces
            .iter()
            .filter(|space| space.unread)
            .map(|space| UnreadSpace {
                account: space.account.clone(),
                space: space.id.clone(),
                title: space.title.clone(),
                snippet: space.snippet.clone(),
                avatar: space.avatar.clone(),
                avatar_email: space.avatar_email.clone(),
                when: space.when.clone(),
            })
            .collect()
    }

    /// Opens `account`'s `space` from outside the pane (the inbox's Chat bundle); the caller
    /// shows the pane.
    pub fn open_space(&mut self, account: SharedString, space: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Key { account, space }, window, cx);
    }


    /// True when the reader is currently showing `account`'s `space`.
    pub fn open_space_matches(&self, account: &str, space: &str) -> bool {
        matches!(&self.view, View::Space(open) if open.account.as_ref() == account && open.space.as_ref() == space)
    }
    /// How the window lays the pane out, as it lays out mail (MS-328): the pane `width` wide, the
    /// list `list_width` wide beside the open page in a wide window (`two_pane`), else the page
    /// over the list.
    pub fn set_split(&mut self, width: f32, list_width: f32, two_pane: bool, cx: &mut Context<Self>) {
        if (width, list_width, two_pane) != (self.width, self.list_width, self.two_pane) {
            self.width = width;
            self.list_width = list_width;
            self.two_pane = two_pane;
            cx.notify();
        }
    }

    /// Whether Chat currently has an in-app back target (message sheet, video lightbox, open
    /// thread, open space/new-chat form, or active search query, MS-449).
    pub fn has_back_target(&self) -> bool {
        self.message_sheet.is_some()
            || self.has_video_lightbox()
            || self.has_open_thread()
            || !matches!(self.view, View::Spaces)
            || !self.query.is_empty()
    }

    /// Drives Chat's open thread or space/form page with Android 14+ predictive back events,
    /// guaranteeing `Invoked` commits exit even when `progress < 1/3` (MS-449).
    pub fn handle_predictive_back(
        &mut self,
        event: crate::swipe::PredictiveBackEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::swipe::PredictiveBackEvent;
        if matches!(event, PredictiveBackEvent::Started { .. } | PredictiveBackEvent::Invoked) {
            window.cancel_touch_momentum();
        }
        if self.message_sheet.is_some() || self.has_video_lightbox() {
            if matches!(event, PredictiveBackEvent::Invoked) {
                self.predictive_back.reset();
                let _ = self.close_message_sheet(cx) || self.close_video_lightbox(window, cx);
            }
            return;
        }
        let target = if self.has_open_thread() {
            Some(SwipeTarget::Thread)
        } else if !matches!(self.view, View::Spaces) {
            Some(SwipeTarget::Page)
        } else {
            None
        };
        if matches!(event, PredictiveBackEvent::Started { .. }) {
            self.finish_closing(window, cx);
            *self.gesture_target.borrow_mut() = target;
        }
        if let Some(target) = target {
            let travel = self.travel(target).max(1.0);
            let step = self.predictive_back.feed(event, travel);
            self.follow(step, cx);
            return;
        }
        if matches!(event, PredictiveBackEvent::Invoked) {
            self.predictive_back.reset();
            self.back(&Back, window, cx);
        } else if matches!(event, PredictiveBackEvent::Cancelled) {
            self.predictive_back.reset();
        }
    }

    /// Whether a space or the new-chat form is open over the list (hides the compact bottom bar).
    pub fn has_open_page(&self) -> bool {
        !matches!(self.view, View::Spaces)
    }

    /// Opens the new-chat form from outside the pane (the compact FAB).
    pub fn start_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_form(window, cx);
    }

    /// Runs `work` on the background executor, then `done` with its result on the main thread.
    fn background<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl Future<Output = R> + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(work).await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    /// What the engine says changed: the chat accounts or their spaces (`list`), the open
    /// space's history (`messages`).
    pub fn follow_engine(&mut self, list: bool, messages: bool, cx: &mut Context<Self>) {
        if list {
            self.load_spaces(cx);
        }
        if let (true, View::Space(key)) = (messages, &self.view) {
            let key = key.clone();
            let (account, space) = (key.account.to_string(), key.space.to_string());
            self.load_thread(&key, async move { engine().chat_messages(account, space).await }, cx);
        }
    }

    /// Asks `account`'s provider, in the browser, to let Mailstream use its Google Chat
    /// (MS-306); the engine's chat list changes once the person allows it.
    fn connect_chat(&mut self, account: SharedString, cx: &mut Context<Self>) {
        if self.connecting.is_some() {
            return;
        }
        self.connecting = Some(account.clone());
        self.connect_failure = None;
        cx.notify();
        self.background(cx, async move { engine().connect_account(account.to_string(), "chat".into()).await }, |this, result, cx| {
            this.connecting = None;
            match result {
                Ok(_) => this.load_spaces(cx),
                Err(error) => this.connect_failure = Some(format!("Could not connect Chat: {error}").into()),
            }
            cx.notify();
        });
    }

    // MARK: - Spaces

    fn load_spaces(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            self.reload = true;
            return;
        }
        self.loading = true;
        let known = self.account_order.clone();
        self.background(
            cx,
            async move {
                let answer = match fixture::spaces() {
                    Some(answer) => answer,
                    None => engine().chat_spaces(None).await?,
                };
                let more = answer.more;
                let accounts: Vec<Account> = answer.accounts.into_iter().map(Account::from).collect();
                let spaces: Vec<Space> = answer.spaces.into_iter().map(Space::new).collect();
                // Account colors follow the sidebar's order of every account; asked for again only
                // when a chat account is not in the known order.
                let order = if accounts.iter().all(|a| known.contains(&a.id)) {
                    None
                } else {
                    engine()
                        .accounts()
                        .await
                        .ok()
                        .map(|list| list.into_iter().map(|a| SharedString::from(a.id)).collect::<Vec<_>>())
                };
                // No chat account: which accounts could connect Chat.
                let offers: Vec<ChatAccountInfo> = if accounts.is_empty() {
                    let list = engine().accounts().await.unwrap_or_default();
                    list.into_iter()
                        .filter(|a| a.connectable.iter().any(|s| s == "chat"))
                        .map(|a| ChatAccountInfo { id: a.id.into(), label: a.label.into() })
                        .collect()
                } else {
                    Vec::new()
                };
                Ok::<_, MailError>((accounts, spaces, order, more, offers))
            },
            |this, result, cx| {
                this.loading = false;
                if std::mem::take(&mut this.reload) {
                    this.load_spaces(cx);
                }
                match result {
                    Ok((accounts, spaces, order, more, offers)) => {
                        this.failure = None;
                        this.chat_offers = offers;
                        this.loaded = true;
                        this.more = more;
                        if let Some(order) = order {
                            this.account_order = order;
                        }
                        if accounts != this.chat_accounts {
                            this.accounts = accounts
                                .iter()
                                .map(|a| ChatAccountInfo { id: a.id.clone(), label: a.label.clone() })
                                .collect();
                            this.chat_accounts = accounts;
                        }
                        if spaces != this.spaces {
                            this.spaces = spaces;
                            this.rebuild_visible();
                        }
                    }
                    Err(error) => this.failure = Some(error.to_string().into()),
                }
                cx.notify();
            },
        );
    }

    /// The next page of older spaces, as the list nears its end; the engine's `Chat` event (and
    /// the read after it) brings them. After a failure the next list read offers paging again.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.more || self.loading_more {
            return;
        }
        self.loading_more = true;
        self.background(
            cx,
            async {
                match fixture::more() {
                    true => Ok(()),
                    false => engine().chat_more(None).await,
                }
            },
            |this, result, cx| {
                this.loading_more = false;
                match result {
                    Ok(()) => this.load_spaces(cx),
                    Err(_) => this.more = false,
                }
            },
        );
    }

    fn rebuild_visible(&mut self) {
        let query = &self.query;
        self.visible = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(_, space)| self.filter.as_ref().is_none_or(|a| &space.account == a))
            .filter(|(_, space)| {
                query.is_empty()
                    || space.title.to_lowercase().contains(query)
                    || space.snippet.to_lowercase().contains(query)
            })
            .map(|(ix, _)| ix)
            .collect();
    }

    /// Typing in the list's search field keeps only the conversations whose title or latest
    /// message holds the text.
    fn on_search_event(&mut self, input: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let InputEvent::Change = event {
            let query = input.read(cx).value().trim().to_lowercase();
            if query != self.query {
                self.query = query;
                self.rebuild_visible();
                if !self.visible.is_empty() {
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            }
        }
    }

    /// Esc in the search field clears it and gives the keys back to the list.
    fn cancel_search(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.clean(window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn space(&self, key: &Key) -> Option<&Space> {
        self.spaces.iter().find(|space| space.is(key))
    }

    fn account_color(&self, account: &str) -> Option<gpui::Hsla> {
        if self.filter.is_some() || self.accounts.len() < 2 {
            return None;
        }
        let index = self.account_order.iter().position(|a| a.as_ref() == account)?;
        Some(design::account_color(index))
    }

    fn account_label(&self, account: &str) -> SharedString {
        self.accounts
            .iter()
            .find(|a| a.id.as_ref() == account)
            .map_or_else(|| SharedString::from(account.to_owned()), |a| a.label.clone())
    }

    // MARK: - Opening

    /// Opens `key` beside the list (over it in a narrow window), its composer taking the keys.
    fn open(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        self.show_space(key, true, window, cx);
    }

    /// Opens `key` in the page, in place of what it showed; `type_here` gives its composer the
    /// keys, else the list keeps them (paging through the spaces with the keys, MS-328). The
    /// space open already only takes the keys.
    fn show_space(&mut self, key: Key, type_here: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.highlight = Some(key.clone());
        if matches!(&self.view, View::Space(open) if *open == key) {
            if self.closing() && self.slide_target == Some(SwipeTarget::Page) {
                self.slide = Slide::Rest;
                self.slide_target = None;
            }
            if type_here {
                window.focus(&self.composer.focus_handle(cx), cx);
            }
            cx.notify();
            return;
        }
        let opening = matches!(self.view, View::Spaces) || (self.closing() && self.slide_target == Some(SwipeTarget::Page));
        // The new-chat form gives way; a space open before is replaced by `chat_open`.
        self.view = View::Space(key.clone());
        self.slide = if opening { Slide::opening() } else { Slide::Rest };
        self.slide_target = opening.then_some(SwipeTarget::Page);
        self.hovered_message = None;
        self.message_sheet = None;
        self.log_top = ReaderTop::Top;
        // Downloads that failed are tried again.
        self.files.retain(|_, file| !matches!(file, FileState::Failed(_)));
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        self.thread_composer.update(cx, |input, cx| input.clean(window, cx));
        let log = self.logs.entry(key.clone()).or_insert_with(Log::new);
        // A new "New" line each time the space is opened, and no thread open beside it.
        log.first_unread = None;
        log.place_new_line = true;
        log.thread = None;
        log.relayout();
        // The engine answers what it holds now and fetches the newest (`ChatMessages` follows),
        // keeps the space current and marks it read while it stays open.
        let (account, space) = (key.account.to_string(), key.space.to_string());
        self.load_thread(&key, async move { engine().chat_open(account, space).await }, cx);
        if type_here {
            window.focus(&self.composer.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.cancel_touch_momentum();
        self.close_video_lightbox(window, cx);
        self.message_sheet = None;
        self.leave_space(cx);
        self.view = View::Spaces;
        self.slide = Slide::Rest;
        self.slide_target = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens `thread` of the open space beside its log (over it when the page is narrow), its
    /// composer taking the keys (MS-329).
    fn open_thread(&mut self, thread: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let closing_thread = self.closing() && self.slide_target == Some(SwipeTarget::Thread);
        let View::Space(key) = &self.view else { return };
        let Some(log) = self.logs.get_mut(key) else { return };
        let opening = log.thread.is_none() || closing_thread;
        self.message_sheet = None;
        if log.thread.as_ref().is_none_or(|view| view.id != thread) {
            log.open_thread(thread);
            self.thread_composer.update(cx, |input, cx| input.clean(window, cx));
        }
        if opening {
            self.slide = Slide::opening();
            self.slide_target = Some(SwipeTarget::Thread);
        }
        window.focus(&self.thread_composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// Closes the thread open beside the open space's log, the space's composer taking the keys.
    fn close_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.cancel_touch_momentum();
        let View::Space(key) = &self.view else { return };
        self.message_sheet = None;
        if let Some(log) = self.logs.get_mut(key) {
            log.thread = None;
        }
        if self.slide_target == Some(SwipeTarget::Thread) {
            self.slide = Slide::Rest;
            self.slide_target = None;
        }
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// The open thread sits beside the space's log: the page has room for both.
    fn thread_beside(&self) -> bool {
        let page = if self.two_pane { self.width - self.list_width - layout::GAP } else { self.width };
        page >= metrics::BESIDE_THREAD + layout::GAP + metrics::THREAD_WIDTH
    }

    /// The open space is no longer kept current or read.
    fn leave_space(&mut self, cx: &mut Context<Self>) {
        if matches!(self.view, View::Space(_)) {
            self.background(cx, async { engine().chat_close().await.ok() }, |_, _, _| {});
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.video_lightbox = None;
        }
    }

    /// Asks the engine (or the fixture) for `key`'s history with `request` and shows the answer,
    /// unless a newer request's answer came first.
    fn load_thread(
        &mut self,
        key: &Key,
        request: impl Future<Output = Result<mailstream::ChatThread, MailError>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(log) = self.logs.get_mut(key) else { return };
        log.requested += 1;
        let serial = log.requested;
        let key = key.clone();
        let fixture = fixture::thread(&key.account, &key.space);
        self.background(
            cx,
            async move {
                match fixture {
                    Some(thread) => thread,
                    None => request.await,
                }
                .map(Loaded::new)
            },
            move |this, result, cx| {
                let Some(log) = this.logs.get_mut(&key) else { return };
                if serial < log.applied {
                    return;
                }
                log.applied = serial;
                match result {
                    Ok(loaded) => log.apply(loaded),
                    Err(error) => {
                        log.loading_earlier = false;
                        if log.messages.is_empty() {
                            log.failure = Some(error.to_string().into());
                        }
                        log.list.splice(0..1, 1);
                    }
                }
                cx.notify();
            },
        );
    }

    fn load_earlier(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        if log.loading_earlier || !log.more {
            return;
        }
        log.loading_earlier = true;
        log.list.splice(0..1, 1);
        let (account, space) = (key.account.to_string(), key.space.to_string());
        self.load_thread(key, async move { engine().chat_earlier(account, space).await }, cx);
    }

    // MARK: - Attachments

    /// The message of item `ix` of `surface`'s list (item 0 is its top).
    fn item(&self, key: &Key, surface: Surface, ix: usize) -> Option<&Msg> {
        let log = self.logs.get(key)?;
        let ix = ix.checked_sub(1)?;
        let entry = match surface {
            Surface::Log => log.shown.get(ix),
            Surface::Thread => log.thread.as_ref()?.entries.get(ix),
        };
        log.entry(*entry?)
    }

    /// Downloads the pictures of item `ix` of `surface`'s list as it comes into view.
    fn fetch_pictures(&mut self, key: &Key, surface: Surface, ix: usize, cx: &mut Context<Self>) {
        let Some(message) = self.item(key, surface, ix) else { return };
        let wanted: Vec<Attachment> = message
            .attachments
            .iter()
            .filter(|a| a.inline() && a.resource.as_ref().is_some_and(|r| !self.files.contains_key(&(key.account.clone(), r.clone()))))
            .cloned()
            .collect();
        for attachment in wanted {
            self.fetch_file(&key.account, &attachment, false, cx);
        }
    }

    /// Opens an attachment in the system's default app: an uploaded file once downloaded, else its
    /// link in the browser.
    fn open_attachment(&mut self, account: &SharedString, attachment: &Attachment, cx: &mut Context<Self>) {
        if attachment.resource.is_some() {
            self.fetch_file(account, attachment, true, cx);
        } else if let Some(url) = &attachment.url {
            cx.open_url(url);
        }
    }

    /// Downloads an uploaded attachment unless it is downloading or here (or failed, unless
    /// `open`); `open` opens it in the system's default app once it is here.
    fn fetch_file(&mut self, account: &SharedString, attachment: &Attachment, open: bool, cx: &mut Context<Self>) {
        let Some(resource) = attachment.resource.clone() else { return };
        let key: FileKey = (account.clone(), resource);
        match self.files.get(&key) {
            Some(FileState::Ready(path)) => {
                if open {
                    cx.open_with_system(path);
                }
                return;
            }
            Some(FileState::Loading) => {
                if open {
                    self.open_when_ready.insert(key);
                    cx.notify();
                }
                return;
            }
            Some(FileState::Failed(_)) if !open => return,
            Some(FileState::Failed(_)) | None => {}
        }
        self.files.insert(key.clone(), FileState::Loading);
        if open {
            self.open_when_ready.insert(key.clone());
        }
        cx.notify();
        let (account, resource, name) = (key.0.to_string(), key.1.to_string(), attachment.name.to_string());
        let fixture = fixture::file(&resource);
        self.background(
            cx,
            async move {
                match fixture {
                    Some(path) => path,
                    None => engine().chat_attachment(account, resource, name).await,
                }
            },
            move |this, result, cx| {
                let file = match result {
                    Ok(path) => FileState::Ready(Path::new(&path).into()),
                    Err(error) => FileState::Failed(error.to_string().into()),
                };
                if let (true, FileState::Ready(path)) = (this.open_when_ready.remove(&key), &file) {
                    cx.open_with_system(path);
                }
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(video) = this.video_waiting.remove(&key).and_then(|video| video.upgrade()) {
                    let answer = match &file {
                        FileState::Ready(path) => Ok(path.clone()),
                        FileState::Failed(reason) => Err(reason.clone()),
                        FileState::Loading => unreachable!("a download that finished"),
                    };
                    video.update(cx, |video, cx| video.load(answer, cx));
                }
                this.files.insert(key, file);
                cx.notify();
            },
        );
    }

    /// A video's poster was clicked: hands it its file, downloading it first unless it is here;
    /// a download that failed is tried again.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn fetch_video(&mut self, account: &SharedString, attachment: &Attachment, video: Entity<ChatVideo>, cx: &mut Context<Self>) {
        let Some(resource) = attachment.resource.clone() else { return };
        let key: FileKey = (account.clone(), resource);
        match self.files.get(&key) {
            Some(FileState::Ready(path)) => {
                let path = path.clone();
                video.update(cx, |video, cx| video.load(Ok(path), cx));
                return;
            }
            Some(FileState::Failed(_)) => {
                self.files.remove(&key);
            }
            Some(FileState::Loading) | None => {}
        }
        self.video_waiting.insert(key, video.downgrade());
        self.fetch_file(account, attachment, false, cx);
    }

    // MARK: - Sending

    fn composer(&self, surface: Surface) -> &Entity<EditorState> {
        match surface {
            Surface::Log => &self.composer,
            Surface::Thread => &self.thread_composer,
        }
    }

    /// Sends what `surface`'s composer holds: to the space, or as a reply in the open thread.
    fn send(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        let View::Space(key) = &self.view else { return };
        let key = key.clone();
        let thread = match surface {
            Surface::Log => None,
            Surface::Thread => match self.logs.get(&key).and_then(|log| log.thread.as_ref()) {
                Some(view) => Some(view.id.clone()),
                None => return,
            },
        };
        let composer = self.composer(surface).clone();
        let text = composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        composer.update(cx, |input, cx| input.clean(window, cx));
        self.deliver(&key, text, thread, window, cx);
    }

    /// Shows `text` as pending at once and sends it; the confirmed message takes its place.
    fn deliver(&mut self, key: &Key, text: String, thread: Option<SharedString>, _: &mut Window, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        self.pending_serial += 1;
        let id: SharedString = format!("pending-{}", self.pending_serial).into();
        let now = Utc::now();
        let me = log.messages.iter().rev().find(|m| m.mine);
        let pending = Msg {
            id: id.clone(),
            sender_id: me.map_or_else(|| "me".into(), |m| m.sender_id.clone()),
            name: "You".into(),
            avatar: me.map_or_else(|| mailstream::avatar(String::new(), key.account.to_string()), |m| m.avatar.clone()),
            email: me.map_or_else(|| key.account.clone(), |m| m.email.clone()),
            created: Some(now),
            time: mailstream::message_time(Some(now.to_rfc3339())).into(),
            thread: thread.clone(),
            thread_reply: thread.is_some(),
            mine: true,
            body: Body::new(&mailstream::chat_draft_body(text.clone(), Msg::names(log)), false),
            attachments: Vec::new(),
            status: Status::Sending,
            draft: Some((text.clone(), thread.clone())),
        };
        log.pending.push(pending);
        log.relayout();
        // Into view where it shows: the open thread, else the log.
        match &log.thread {
            Some(view) if thread.as_ref() == Some(&view.id) => {
                view.list.scroll_to_reveal_item(view.entries.len());
            }
            _ => log.list.scroll_to_reveal_item(log.shown.len()),
        }
        cx.notify();

        let (account, space) = (key.account.to_string(), key.space.to_string());
        let thread_id = thread.as_ref().map(ToString::to_string);
        let key = key.clone();
        self.background(
            cx,
            async move {
                // Pressing Send is the user's confirmation.
                engine().chat_send(account.clone(), space.clone(), text, thread_id, true).await?;
                // The engine's history holds the message now; it replaces the pending copy.
                engine().chat_messages(account, space).await.map(Loaded::new)
            },
            move |this, result, cx| {
                let Some(log) = this.logs.get_mut(&key) else { return };
                match result {
                    Ok(loaded) => {
                        log.pending.retain(|m| m.id != id);
                        // Read after every earlier request was answered: newer than any of them.
                        log.requested += 1;
                        log.applied = log.requested;
                        log.apply(loaded);
                    }
                    Err(error) => {
                        if let Some(pending) = log.pending.iter_mut().find(|m| m.id == id) {
                            pending.status = Status::Failed(error.to_string().into());
                        }
                        log.relayout();
                    }
                }
                cx.notify();
            },
        );
    }

    fn retry(&mut self, key: &Key, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        let Some(at) = log.pending.iter().position(|m| &m.id == id) else { return };
        let failed = log.pending.remove(at);
        log.relayout();
        if let Some((text, thread)) = failed.draft {
            self.deliver(key, text, thread, window, cx);
        }
    }

    fn discard(&mut self, key: &Key, id: &SharedString, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        log.pending.retain(|m| &m.id != id);
        log.relayout();
        cx.notify();
    }

    fn on_composer_event(
        &mut self,
        composer: &Entity<EditorState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // The send button enables with the first character and disables once the text is cleared.
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { shift: false, .. }
                if !composer.update(cx, |input, cx| input.marked_text_range(window, cx).is_some()) =>
            {
                let surface = if *composer == self.thread_composer { Surface::Thread } else { Surface::Log };
                self.send(surface, window, cx)
            }
            _ => {}
        }
    }

    fn format_composer(&mut self, surface: Surface, marker: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer(surface).clone().update(cx, |input, cx| wrap_selection(input, marker, window, cx));
    }

    /// Esc in the space's composer, beside the list, gives the keys to the list (the space stays
    /// open, ↑/↓ page through the spaces in it, Return comes back and Esc closes it, as mail's
    /// reader); over the list it closes the space. In the thread's composer it closes the thread.
    fn composer_escape(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_message_sheet(cx) {
            return;
        }
        if surface == Surface::Thread {
            self.slide_off(SwipeTarget::Thread, window, cx);
        } else if self.two_pane {
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            self.slide_off(SwipeTarget::Page, window, cx);
        }
    }

    // MARK: - Starting a conversation

    fn start_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.accounts.is_empty() {
            return;
        }
        let account = self
            .filter
            .clone()
            .or_else(|| self.chat_accounts.iter().find(|a| a.can_start).map(|a| a.id.clone()))
            .or_else(|| self.accounts.first().map(|a| a.id.clone()));
        let opening = matches!(self.view, View::Spaces) || (self.closing() && self.slide_target == Some(SwipeTarget::Page));
        self.leave_space(cx);
        self.form = NewChatForm { account, ..Default::default() };
        self.view = View::NewChat;
        self.slide = if opening { Slide::opening() } else { Slide::Rest };
        self.slide_target = opening.then_some(SwipeTarget::Page);
        window.focus(&self.people_input.focus_handle(cx), cx);
        self.load_partners(cx);
        cx.notify();
    }

    /// The form's account's Chat partners, offered before anyone the contact lookup finds.
    fn load_partners(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.form.account.clone() else { return };
        let fixture = fixture::people();
        let id = account.to_string();
        self.background(
            cx,
            async move {
                match fixture {
                    Some(people) => Ok(people),
                    None => engine().chat_people(id).await,
                }
            },
            move |this, result, cx| {
                // Without them only the contact lookup completes; typed addresses still work.
                if let (Ok(people), true) = (result, this.form.account.as_ref() == Some(&account)) {
                    this.form.partners = people.people.into_iter().filter_map(Person::partner).collect();
                    this.suggest(cx);
                }
            },
        );
    }

    /// People to complete to, as the native New Chat sheet offers them: the account's Chat
    /// partners first (all of them while nothing is typed), then those the contact lookup found;
    /// an address typed out in full first of all.
    fn suggest(&mut self, cx: &mut Context<Self>) {
        let query = self.people_input.read(cx).value().trim().to_lowercase();
        let form = &mut self.form;
        let mut seen: HashSet<String> = form.people.iter().map(|p| p.email.to_lowercase()).collect();
        let partners = form.partners.iter().filter(|person| {
            query.is_empty()
                || person.email.to_lowercase().contains(&query)
                || person.name.as_ref().is_some_and(|name| name.to_lowercase().contains(&query))
        });
        let contacts = form.contacts.iter().filter(|_| !query.is_empty());
        let mut people: Vec<Person> =
            partners.chain(contacts).filter(|person| seen.insert(person.email.to_lowercase())).cloned().collect();
        if let Some(exact) = people.iter().position(|person| person.email.to_lowercase() == query) {
            let person = people.remove(exact);
            people.insert(0, person);
        }
        people.truncate(SUGGESTIONS as usize);
        form.suggestions = people;
        cx.notify();
    }

    fn on_people_event(&mut self, input: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.form.generation += 1;
                self.form.failure = None;
                self.form.contacts.clear();
                self.suggest(cx);
                let query = input.read(cx).value().trim().to_owned();
                if !query.is_empty() {
                    self.search_people(query, self.form.generation, cx);
                }
            }
            InputEvent::PressEnter { secondary: false, .. } => {
                let typed = input.read(cx).value().trim().to_owned();
                if typed.is_empty() {
                    self.start_chat(window, cx);
                } else if typed.contains('@') && !typed.contains(char::is_whitespace) {
                    self.add_person(Person { name: None, email: typed.into() }, window, cx);
                } else if let Some(first) = self.form.suggestions.first().cloned() {
                    self.add_person(first, window, cx);
                }
            }
            _ => {}
        }
    }

    fn search_people(&mut self, query: String, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let current = this.update(cx, |this, _| this.form.generation == generation).unwrap_or(false);
            if !current {
                return;
            }
            let found = cx
                .background_executor()
                .spawn(async move { engine().search_contacts(query, SUGGESTIONS).await })
                .await;
            this.update(cx, |this, cx| {
                if this.form.generation != generation {
                    return;
                }
                // An app without people lookup offers none; typed addresses still work.
                this.form.contacts = found.unwrap_or_default().into_iter().map(Person::from).collect();
                this.suggest(cx);
            })
            .ok();
        })
        .detach();
    }

    fn add_person(&mut self, person: Person, window: &mut Window, cx: &mut Context<Self>) {
        if !self.form.people.iter().any(|p| p.email.eq_ignore_ascii_case(&person.email)) {
            self.form.people.push(person);
        }
        self.form.generation += 1;
        self.form.contacts.clear();
        self.people_input.update(cx, |input, cx| input.clean(window, cx));
        window.focus(&self.people_input.focus_handle(cx), cx);
        self.suggest(cx);
    }

    fn start_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.form.account.clone() else { return };
        if self.form.people.is_empty() || self.form.starting {
            return;
        }
        self.form.starting = true;
        self.form.failure = None;
        cx.notify();
        let emails: Vec<String> = self.form.people.iter().map(|p| p.email.to_string()).collect();
        let account = account.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let space = engine().chat_start(account, emails).await?;
                    Ok::<_, MailError>(Key { account: space.account.into(), space: space.space.into() })
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.form.starting = false;
                match result {
                    // The list shows a new space once the engine's `Chat` event arrives.
                    Ok(key) => {
                        // Left the form meanwhile: the space waits in the list.
                        if matches!(this.view, View::NewChat) {
                            this.open(key, window, cx);
                        }
                    }
                    Err(error) => this.form.failure = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // MARK: - Actions

    /// Closes the open space or the new-chat form (⌘W, which the window binds); nothing on the
    /// space list.
    pub fn close_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_message_sheet(cx) {
            return;
        }
        if self.close_video_lightbox(window, cx) {
            return;
        }
        if !matches!(self.view, View::Spaces) {
            self.close(window, cx);
        }
    }

    /// The open video lightbox over the window, if any.
    pub fn video_lightbox(&self) -> Option<AnyElement> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            return self.video_lightbox.as_ref().map(|lightbox| lightbox.clone().into_any_element());
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        None
    }

    pub fn has_video_lightbox(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            return self.video_lightbox.is_some();
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        false
    }

    /// Closes the video lightbox (exiting fullscreen if active) and returns `true` if one was open.
    pub fn close_video_lightbox(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(lightbox) = self.video_lightbox.take() {
            lightbox.update(cx, |lightbox, cx| lightbox.finish(window, cx));
            cx.notify();
            return true;
        }
        let _ = (window, cx);
        false
    }

    /// Closes the message action sheet (MS-436) and returns `true` if one was open.
    pub fn close_message_sheet(&mut self, cx: &mut Context<Self>) -> bool {
        if self.message_sheet.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    fn message_for(&self, sheet: &MessageSheet) -> Option<(&Log, &Msg)> {
        let log = self.logs.get(&sheet.key)?;
        let message = log.messages.iter().chain(log.pending.iter()).find(|m| m.id == sheet.message_id)?;
        Some((log, message))
    }

    fn message_has_sheet_actions(surface: Surface, message: &Msg) -> bool {
        let can_reply = surface == Surface::Log && matches!(message.status, Status::Sent) && message.thread.is_some();
        let can_copy = !message.body.plain_text().trim().is_empty();
        let failed = matches!(message.status, Status::Failed(_));
        can_reply || can_copy || failed
    }

    /// Opens the mobile/context action sheet for `message_id` when it has at least one action.
    fn open_message_sheet(&mut self, key: Key, surface: Surface, message_id: SharedString, cx: &mut Context<Self>) -> bool {
        let sheet = MessageSheet { key, surface, message_id };
        let Some((_, message)) = self.message_for(&sheet) else { return false };
        if !Self::message_has_sheet_actions(surface, message) {
            return false;
        }
        self.hovered_message = None;
        self.message_sheet = Some(sheet);
        cx.notify();
        true
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn open_video_lightbox(&mut self, video: Entity<ChatVideo>, window: &mut Window, cx: &mut Context<Self>) {
        if self.video_lightbox.as_ref().is_some_and(|open| open.read(cx).video() == &video) {
            return;
        }
        let previous_focus = window.focused(cx).or_else(|| Some(self.focus.clone()));
        let pane = cx.entity().downgrade();
        let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
            pane.update(cx, |pane, cx| {
                pane.video_lightbox = None;
                cx.notify();
            })
            .ok();
        });
        let lightbox = cx.new(|cx| VideoLightbox::new(video, previous_focus, on_close, window, cx));
        self.video_lightbox = Some(lightbox);
        cx.notify();
    }

    /// Esc on the list: the open space or form closes, else the search ends, else the window
    /// takes it.
    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        window.cancel_touch_momentum();
        if self.close_message_sheet(cx) {
            return;
        }
        self.finish_closing(window, cx);
        if self.has_open_thread() {
            self.slide_off(SwipeTarget::Thread, window, cx);
            return;
        }
        match self.view {
            View::Space(_) | View::NewChat => self.slide_off(SwipeTarget::Page, window, cx),
            View::Spaces if !self.query.is_empty() => self.cancel_search(&Escape, window, cx),
            View::Spaces => cx.propagate(),
        }
    }

    fn has_open_thread(&self) -> bool {
        matches!(&self.view, View::Space(key) if self.logs.get(key).is_some_and(|log| log.thread.is_some()))
    }

    fn surface_live(&self, target: SwipeTarget) -> bool {
        match target {
            SwipeTarget::Thread => self.has_open_thread(),
            SwipeTarget::Page => !matches!(self.view, View::Spaces) && (!self.has_open_thread() || self.thread_beside()),
        }
    }

    fn target_at(&self, x: f32) -> Option<SwipeTarget> {
        let has_page = !matches!(self.view, View::Spaces);
        chat_gesture_target(has_page, self.has_open_thread(), self.two_pane, self.thread_beside(), self.width, self.list_width, x)
    }

    fn travel(&self, target: SwipeTarget) -> f32 {
        match target {
            SwipeTarget::Page => {
                if self.two_pane {
                    (self.width - self.list_width).max(1.)
                } else {
                    self.width.max(1.)
                }
            }
            SwipeTarget::Thread => {
                if self.thread_beside() {
                    metrics::THREAD_WIDTH
                } else if self.two_pane {
                    (self.width - self.list_width - layout::GAP).max(1.)
                } else {
                    self.width.max(1.)
                }
            }
        }
    }

    fn closing(&self) -> bool {
        matches!(self.slide, Slide::Settle { back: true, .. })
    }

    fn finish_closing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing() {
            return;
        }
        if let Some(target) = self.slide_target.take() {
            self.slide = Slide::Rest;
            match target {
                SwipeTarget::Thread => self.close_thread(window, cx),
                SwipeTarget::Page => self.close(window, cx),
            }
        }
    }

    fn finish_closing_state(&mut self, cx: &mut Context<Self>) {
        if !self.closing() {
            return;
        }
        if let Some(target) = self.slide_target.take() {
            self.slide = Slide::Rest;
            match target {
                SwipeTarget::Thread => {
                    if let View::Space(key) = &self.view
                        && let Some(log) = self.logs.get_mut(key)
                    {
                        log.thread = None;
                    }
                }
                SwipeTarget::Page => {
                    self.leave_space(cx);
                    self.view = View::Spaces;
                }
            }
        }
    }

    fn slide_off(&mut self, target: SwipeTarget, window: &mut Window, cx: &mut Context<Self>) {
        let travel = self.travel(target);
        if travel <= 0.0 {
            match target {
                SwipeTarget::Thread => self.close_thread(window, cx),
                SwipeTarget::Page => self.close(window, cx),
            }
            return;
        }
        let from = if self.slide_target == Some(target) { self.slide.offset(travel) } else { 0. };
        self.slide_target = Some(target);
        self.slide = Slide::settle(from, travel, true);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn slide_offset(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(SwipeTarget, f32)> {
        let target = self.slide_target?;
        if !self.surface_live(target) {
            self.slide = Slide::Rest;
            self.slide_target = None;
            return None;
        }
        let travel = self.travel(target);
        match self.slide.advance(travel, window, cx) {
            Some(offset) => Some((target, offset)),
            None => {
                self.slide_target = None;
                match target {
                    SwipeTarget::Thread => self.close_thread(window, cx),
                    SwipeTarget::Page => self.close(window, cx),
                }
                None
            }
        }
    }

    fn peek_swipe(&self, event: &ScrollEvent, at: Option<Point<Pixels>>) -> Step {
        let blocked = self.has_video_lightbox() || self.message_sheet.is_some();
        if event.touch_phase == TouchPhase::Started {
            *self.gesture_target.borrow_mut() =
                (!blocked).then(|| at.and_then(|at| self.target_at(f32::from(at.x)))).flatten();
        }
        let can_start = self.gesture_target.borrow().is_some() && !blocked;
        self.swipe.feed(event, can_start)
    }

    fn press_surface(&mut self, target: SwipeTarget, at: Point<Pixels>) {
        let live = !self.has_video_lightbox() && self.message_sheet.is_none() && self.surface_live(target);
        *self.gesture_target.borrow_mut() = live.then_some(target);
        self.drag.press(at, live);
    }

    fn press_surface_listener(
        target: SwipeTarget,
        cx: &mut Context<Self>,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, _| {
            if event.click_count == 1 {
                this.press_surface(target, event.position);
            }
        })
    }

    fn feed_drag(&mut self, pointer: Pointer, cx: &mut Context<Self>) -> bool {
        let step = self.drag.feed(pointer);
        self.follow(step, cx)
    }

    fn follow(&mut self, step: Step, cx: &mut Context<Self>) -> bool {
        let moves = !matches!(step, Step::Pass | Step::Consume);
        if matches!(step, Step::Drag(_)) {
            let target = *self.gesture_target.borrow();
            if self.slide_target != target {
                self.finish_closing_state(cx);
                self.slide_target = target;
            }
        }
        let Some(target) = self.slide_target.filter(|&target| self.surface_live(target)) else {
            return !matches!(step, Step::Pass);
        };
        let took = self.slide.follow(step, self.travel(target));
        if moves {
            cx.notify();
        }
        took
    }

    /// ↑/↓ on the list: the highlight moves; with a space open, the one highlighted opens in its
    /// place, the list keeping the keys (as mail's reader pages through the list).
    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let current = self
            .highlight
            .as_ref()
            .and_then(|key| self.visible.iter().position(|&ix| self.spaces[ix].is(key)));
        let next = match current {
            None => 0,
            Some(at) => at.saturating_add_signed(delta).min(self.visible.len() - 1),
        };
        let key = self.spaces[self.visible[next]].key();
        self.scroll.scroll_to_item(next, ScrollStrategy::Center);
        if matches!(self.view, View::Space(_)) {
            self.show_space(key, false, window, cx);
        } else {
            self.highlight = Some(key);
            cx.notify();
        }
    }

    fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        self.step(1, window, cx);
    }

    fn previous(&mut self, _: &Previous, window: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, window, cx);
    }

    /// Return on the list opens the highlighted space, or goes back to typing in the open one.
    fn open_highlighted(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.highlight.clone() else { return };
        if self.space(&key).is_some() {
            self.open(key, window, cx);
        }
    }

    pub fn new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.start_form(window, cx);
    }

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.background(cx, async { engine().chat_refresh().await.ok() }, |_, _, _| {});
        self.load_spaces(cx);
        // Opening again fetches the open space's newest messages now.
        if let View::Space(key) = &self.view {
            let key = key.clone();
            let (account, space) = (key.account.to_string(), key.space.to_string());
            self.load_thread(&key, async move { engine().chat_open(account, space).await }, cx);
        }
    }
}

// MARK: - Rendering

fn chat_gesture_target(
    has_page: bool,
    has_thread: bool,
    two_pane: bool,
    thread_beside: bool,
    width: f32,
    list_width: f32,
    x: f32,
) -> Option<SwipeTarget> {
    if !has_page {
        return None;
    }
    let page_left = if two_pane { list_width + layout::GAP } else { 0. };
    if x < page_left {
        return None;
    }
    if has_thread {
        if !thread_beside {
            return Some(SwipeTarget::Thread);
        }
        let thread_left = width - metrics::THREAD_WIDTH;
        if x >= thread_left {
            return Some(SwipeTarget::Thread);
        }
        if x >= thread_left - layout::GAP {
            return None;
        }
    }
    Some(SwipeTarget::Page)
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let sliding = self.slide_offset(window, cx);
        let page_shift = match sliding {
            Some((SwipeTarget::Page, offset)) => offset,
            _ => 0.,
        };
        let thread_shift = match sliding {
            Some((SwipeTarget::Thread, offset)) => offset,
            _ => 0.,
        };
        let list = self.render_list_island(p, window, cx);
        let page = match &self.view {
            View::Spaces => None,
            View::Space(key) => {
                let key = key.clone();
                Some(self.render_space(&key, thread_shift, p, window, cx))
            }
            View::NewChat => Some(self.render_form(p, window, cx)),
        };
        let (two_pane, list_width, width) = (self.two_pane, self.list_width, self.width);
        let swipe = swipe::scroll_capture(cx.entity(), Self::peek_swipe, Self::follow).absolute().inset_0();
        let (drag_under, drag_over) = swipe::mouse_capture(cx.entity(), Self::feed_drag);
        let body = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open_highlighted))
            .on_action(cx.listener(Self::new_chat))
            .on_action(cx.listener(Self::refresh))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.back(&Back, window, cx)),
            )
            .relative()
            .size_full()
            .font_family(design::ui_font())
            .text_size(px(design::type_scale(compact).body))
            .text_color(p.text)
            .map(|d| match page {
                // Beside the list, which widens back to full width as the page slides off, as mail's
                // reader does (`ui/mod.rs`).
                Some(page) if two_pane => {
                    let travel = (width - list_width).max(1.);
                    let (cur_list_width, page_left) = if page_shift >= 0. {
                        let split = (1. - page_shift / travel).clamp(0., 1.);
                        let lw = width + (list_width - width) * split;
                        (lw, lw + layout::GAP)
                    } else {
                        (list_width, list_width + layout::GAP + page_shift)
                    };
                    d.when(page_shift != 0., |d| d.overflow_hidden())
                        .child(list.absolute().left_0().top_0().bottom_0().w(px(cur_list_width)))
                        .when(page_shift == 0., |d| {
                            d.child(split::list_handle("chat-split").left(px(cur_list_width + layout::GAP / 2.)))
                        })
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(page_left))
                                .w(px((width - list_width - layout::GAP).max(0.)))
                                .child(page),
                        )
                }
                // Over the list, which sits back and darkens beneath it as the page slides.
                Some(page) => {
                    let travel = self.travel(SwipeTarget::Page);
                    let (beneath, dim) = design::motion::beneath(1. - page_shift.abs() / travel.max(1.), travel, p.dark);
                    let scrim_radius = if compact { 0. } else { radius::PANE };
                    d.when(beneath != 0. || page_shift != 0., |d| d.overflow_hidden())
                        .child(div().absolute().top_0().bottom_0().w_full().left(px(beneath)).child(list.size_full()))
                        .when(dim > 0., |d| d.child(scrim(dim, scrim_radius)))
                        .child(div().absolute().top_0().bottom_0().w_full().left(px(page_shift)).child(page))
                }
                None => d.child(list.size_full()),
            });
        div()
            .relative()
            .size_full()
            .child(drag_under.absolute().size_0())
            .child(body)
            .child(swipe)
            .child(drag_over.absolute().size_0())
            .children(self.render_message_sheet(p, window, cx))
    }
}

impl ChatPane {
    /// The list's island, as mail's (MS-328): its title row (the account or "Chat", how many
    /// conversations are unread, refresh and New Chat on a capsule), the search field, the rows.
    fn render_list_island(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let title = self.filter.as_ref().map_or_else(|| SharedString::from("Chat"), |a| self.account_label(a));
        let unread = self.unread(self.filter.as_deref());
        let subtitle: Option<SharedString> = if !self.loaded && self.failure.is_none() {
            Some("Loading…".into())
        } else if !self.query.is_empty() {
            Some(match self.visible.len() {
                1 => "1 conversation".into(),
                found => format!("{found} conversations").into(),
            })
        } else {
            (unread > 0).then(|| format!("{unread} unread").into())
        };
        let ts = design::type_scale(compact);
        let actions = if compact {
            compact_capsule(p).child(
                compact_icon_button("chat-refresh", Icon::Refresh, "Refresh", true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
            )
        } else {
            capsule(p)
                .child(
                    icon_button("chat-refresh", Icon::Refresh, "Refresh", true, p.glyph)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
                )
                .child(
                    icon_button("chat-new", Icon::Compose, "New Chat", !self.accounts.is_empty(), p.glyph)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_form(window, cx))),
                )
        };
        let leading = compact.then(|| {
            compact_capsule(p).child(
                compact_icon_button("chat-drawer", Icon::Menu, "Mailboxes", true, p.glyph)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ChatOpenDrawer))),
            )
        });
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let surface = if compact { div().relative().overflow_hidden().bg(p.tray) } else { island(p.tray, p) };
        let header = if compact {
            div()
                .flex_shrink_0()
                .h(px(design::compact::TOP_BAR_HEIGHT))
                .px(px(space::S12))
                .flex()
                .items_center()
                .gap(px(space::S10))
                .children(leading)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(ts.header_title))
                                .line_height(relative(1.15))
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                        .when_some(subtitle, |d, subtitle| {
                            d.child(div().truncate().text_size(px(ts.caption)).text_color(p.muted).child(subtitle))
                        }),
                )
                .child(actions)
                .into_any_element()
        } else {
            list_header("chat-list-header", None, title, subtitle, actions, p, window, cx).into_any_element()
        };
        surface.occlude().child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(header)
                .child(search_field_for("chat-search", search_focused, compact, input::field(&self.search), cx.listener(Self::cancel_search), p))
                .child(hairline(p))
                .child(div().flex_1().min_h_0().child(self.render_spaces(compact, p, cx))),
        )
    }

    /// The rows, or what stands in for them.
    fn render_spaces(&self, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        if let Some(failure) = &self.failure {
            centered_note(failure.clone(), p).into_any_element()
        } else if !self.loaded {
            centered_note("Loading chats…", p).into_any_element()
        } else if self.accounts.is_empty() {
            let idle = self.connecting.is_none();
            let note = if self.chat_offers.is_empty() {
                "No Google Chat accounts. Add a Google account in Settings to use Chat."
            } else {
                "Google Chat is not connected. Connect it for an account to see its conversations here."
            };
            centered_note(note, p)
                .children(self.chat_offers.iter().map(|offer| {
                    let id = offer.id.clone();
                    let label = if self.connecting.as_ref() == Some(&offer.id) {
                        "Waiting for Browser…".to_owned()
                    } else {
                        format!("Connect Chat for {}…", offer.label)
                    };
                    button(ElementId::Name(format!("connect-chat:{}", offer.id).into()), label, ButtonKind::Secondary, ControlSize::Small, idle, p)
                        .when(idle, |b| b.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.connect_chat(id.clone(), cx))))
                }))
                .when_some(self.connect_failure.clone(), |d, failure| d.child(div().text_center().text_color(p.danger).child(failure)))
                .into_any_element()
        } else if self.visible.is_empty() && !self.query.is_empty() {
            centered_note("No conversations match.", p).into_any_element()
        } else if self.visible.is_empty() {
            centered_note("No conversations yet.", p)
                .child(button("chat-new-empty", "New Chat", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_form(window, cx))))
                .into_any_element()
        } else {
            uniform_list(
                "chat-spaces",
                self.visible.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    if range.end + metrics::MORE_AHEAD >= this.visible.len() {
                        this.load_more(cx);
                    }
                    range.map(|position| this.render_row(position, compact, p, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .px(px(rows::INSET))
            .pt(px(space::S6))
            .pb(px(if compact { design::compact::DOCK_CLEARANCE } else { space::S6 }))
            .into_any_element()
        }
    }

    /// The `position`th space in view.
    fn render_row(&self, position: usize, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let space = &self.spaces[self.visible[position]];
        let key = space.key();
        let selected = self.highlight.as_ref() == Some(&key);
        let unread = space.unread;
        let next = self.visible.get(position + 1).map(|&next| self.spaces[next].key());
        let rule = next.is_some_and(|next| self.highlight.as_ref() != Some(&next));
        let ts = design::type_scale(compact);
        let title = div()
            .min_w_0()
            .truncate()
            .when(compact, |d| d.text_size(px(ts.reading)))
            .font_weight(if unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
            .child(space.title.clone());
        let meta = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(if compact { ts.caption } else { text::FOOTNOTE }))
            .text_color(if unread { p.accent } else { p.muted })
            .when_some(self.account_color(&space.account), |d, color| d.child(mark_dot(color)))
            .child(space.when.clone());
        let subtitle = div()
            .min_w_0()
            .truncate()
            .when(compact, |d| d.text_size(px(ts.footnote)))
            .text_color(p.muted)
            .child(space.snippet.clone());
        list_row(ElementId::Name(format!("chat-row:{}:{}", space.account, space.id).into()), selected, rule, p)
            .when(compact, |d| d.h(px(design::compact::bundle_row_height())))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open(key.clone(), window, cx)))
            .child(row_layout(
                unread,
                avatars::avatar(&space.avatar_email, &space.avatar, if compact { design::compact::AVATAR } else { rows::AVATAR }, p, cx),
                title,
                meta,
                subtitle,
                Vec::new(),
                p,
            ))
            .into_any_element()
    }

    /// The space's title: the list's, else its history's, else its id.
    fn space_title(&self, key: &Key) -> SharedString {
        self.space(key)
            .map(|s| s.title.clone())
            .or_else(|| self.logs.get(key).and_then(|l| l.title.clone()))
            .unwrap_or_else(|| key.space.clone())
    }

    /// The other person's address in a direct message, for the mail exchanged with them
    /// (MS-331): the one the list's row keys its photo on, else the history's member's. None in
    /// groups, spaces and apps, and when Chat gives no address.
    fn partner_address(&self, key: &Key) -> Option<SharedString> {
        let space = self.space(key).filter(|space| space.kind == mailstream::ChatSpaceKind::Dm)?;
        let member = || self.logs.get(key)?.members.first().map(|member| member.email.clone());
        Some(space.avatar_email.clone()).filter(|address| !address.is_empty()).or_else(member).filter(|address| !address.is_empty())
    }

    /// The open space's island, as mail's reader (MS-328): its log scrolls under the reader's
    /// toolbar (✕ beside the list, ‹ over it), which shows the title once the log's heading has
    /// scrolled under it; the composer below. An open thread's island sits beside it when the page
    /// has room, else over it (MS-329).
    fn render_space(&self, key: &Key, thread_shift: f32, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let title = self.space_title(key);
        let log: AnyElement = match self.logs.get(key) {
            Some(log) => {
                let list_key = key.clone();
                list(
                    log.list.clone(),
                    cx.processor(move |this, ix: usize, window, cx| {
                        this.fetch_pictures(&list_key, Surface::Log, ix, cx);
                        this.render_entry(&list_key, ix, window, p, cx)
                    }),
                )
                .size_full()
                .into_any_element()
            }
            None => div().into_any_element(),
        };
        // Where the log is, once this frame has laid it out (opened, grown, scrolled).
        cx.on_next_frame(window, |this, _, cx| this.note_log_top(cx));
        let (glyph, tip) = if self.two_pane { (Icon::Close, "Close (Esc)") } else { (Icon::Back, "Back (Esc)") };
        let close = icon_button("chat-close", glyph, tip, true, p.glyph)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.slide_off(SwipeTarget::Page, window, cx)));
        let toolbar_title = (self.log_top == ReaderTop::PastTitle).then(|| title.clone());
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let space = (if compact { div().relative().overflow_hidden().bg(p.background) } else { island(p.background, p) })
            .occlude()
            .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Page, cx))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .on_scroll(cx.listener(|_, _: &ScrollEvent, window, cx| {
                        cx.on_next_frame(window, |this, _, cx| this.note_log_top(cx));
                    }))
                    .child(log)
                    .when(self.log_top != ReaderTop::Top, |d| d.child(page_scrim(p)))
                    .child(
                        page_toolbar("chat-toolbar", close, toolbar_title, p, window, cx)
                            .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Page, cx))
                            .when_some(
                                self.partner_address(key),
                        |bar, address| {
                            let mail = icon_button("chat-mail", Icon::Mail, format!("Mail with {title}"), true, p.glyph)
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(MailWith(address.clone()))));
                            bar.child(capsule(p).child(mail))
                        },
                    )),
            )
            .child(self.render_composer(Surface::Log, compact, format!("Send to {title}").into(), p, cx));
        let Some(view) = self.logs.get(key).and_then(|log| log.thread.as_ref()) else { return space.into_any_element() };
        let beside = self.thread_beside();
        let thread = self.render_thread(key, view, beside, p, window, cx);
        if beside {
            div()
                .size_full()
                .flex()
                .gap(px(layout::GAP))
                .when(thread_shift != 0., |d| d.overflow_hidden())
                .child(div().flex_1().min_w_0().h_full().child(space))
                .child(div().relative().flex_shrink_0().w(px(metrics::THREAD_WIDTH)).h_full().left(px(thread_shift)).child(thread))
                .into_any_element()
        } else {
            let travel = self.travel(SwipeTarget::Thread);
            let (beneath, dim) = design::motion::beneath(1. - thread_shift.abs() / travel.max(1.), travel, p.dark);
            let scrim_radius = if compact { 0. } else { radius::PANE };
            div()
                .relative()
                .size_full()
                .when(beneath != 0. || thread_shift != 0., |d| d.overflow_hidden())
                .child(div().absolute().top_0().bottom_0().w_full().left(px(beneath)).child(space))
                .when(dim > 0., |d| d.child(scrim(dim, scrim_radius)))
                .child(div().absolute().top_0().bottom_0().w_full().left(px(thread_shift)).child(thread))
                .into_any_element()
        }
    }

    /// The open thread's island (MS-329): the message that started it, a line with how many
    /// replies, the replies, and a composer that replies in it; under a toolbar that closes it
    /// (✕ beside the space's log, ‹ over it).
    fn render_thread(&self, key: &Key, view: &ThreadView, beside: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list_key = key.clone();
        let entries = list(
            view.list.clone(),
            cx.processor(move |this, ix: usize, window, cx| {
                this.fetch_pictures(&list_key, Surface::Thread, ix, cx);
                this.render_thread_entry(&list_key, ix, window, p, cx)
            }),
        )
        .size_full();
        let (glyph, tip) = if beside { (Icon::Close, "Close thread (Esc)") } else { (Icon::Back, "Back to the conversation (Esc)") };
        let close = icon_button("chat-thread-close", glyph, tip, true, p.glyph)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.slide_off(SwipeTarget::Thread, window, cx)));
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        (if compact { div().relative().overflow_hidden().bg(p.background) } else { island(p.background, p) })
            .occlude()
            .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Thread, cx))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(entries)
                    // The page's own surface under the toolbar's band until something scrolls there.
                    .child(page_scrim(p))
                    .child(
                        page_toolbar("chat-thread-toolbar", close, Some("Thread".into()), p, window, cx)
                            .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Thread, cx)),
                    ),
            )
            .child(self.render_composer(Surface::Thread, compact, "Reply in thread".into(), p, cx))
            .into_any_element()
    }

    /// Notes how far the open space's log is scrolled, for its toolbar: past its top, and past its
    /// heading (the title and its pills, under the toolbar's band at the top of the log).
    fn note_log_top(&mut self, cx: &mut Context<Self>) {
        let View::Space(key) = &self.view else { return };
        let Some(log) = self.logs.get(key) else { return };
        let scrolled = -f32::from(log.list.scroll_px_offset_for_scrollbar().y);
        let top = if scrolled <= 0.5 {
            ReaderTop::Top
        } else if scrolled > space::S16 + self.head_height.get() {
            ReaderTop::PastTitle
        } else {
            ReaderTop::Scrolled
        };
        if top != self.log_top {
            self.log_top = top;
            cx.notify();
        }
    }

    /// A column as wide as the reader's text, centered; narrower margins on compact viewports and
    /// inside the thread's island.
    fn column(surface: Surface, compact: bool, child: impl IntoElement) -> Div {
        let margin = match surface {
            Surface::Log if compact => design::compact::READER_PADDING,
            Surface::Log => 32.,
            Surface::Thread => space::S12,
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(div().w_full().max_w(px(layout::READING_WIDTH)).min_w_0().px(px(margin)).child(child))
    }

    /// Item `ix` of the space's log: its head (`ix` 0), else a shown message, under a "New" line
    /// when it is the first one unread.
    fn render_entry(&self, key: &Key, ix: usize, window: &mut Window, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(log) = self.logs.get(key) else { return div().into_any_element() };
        if ix == 0 {
            return self.render_top(key, log, window, p, cx);
        }
        let (Some(message), Some(run)) = (self.item(key, Surface::Log, ix), log.runs.get(ix - 1)) else {
            return div().into_any_element();
        };
        let marker = (log.first_unread.as_ref() == Some(&message.id)).then(|| message_log::divider("New".into(), p.accent, p.accent.opacity(0.45)));
        self.render_message(key, log, message, run, Surface::Log, marker, window, p, cx)
    }

    /// Item `ix` of the open thread: the room under its toolbar (`ix` 0, with a note when the
    /// message that started it isn't loaded), else one of its messages, a line telling how many
    /// replies above the first reply.
    fn render_thread_entry(&self, key: &Key, ix: usize, window: &mut Window, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some((log, view)) = self.logs.get(key).and_then(|log| Some((log, log.thread.as_ref()?))) else {
            return div().into_any_element();
        };
        if ix == 0 {
            let note = (!view.starter).then(|| {
                div().pb(px(space::S8)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child("The message that started this thread is older than the messages loaded.")
            });
            let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
            return Self::column(Surface::Thread, compact, div().pt(px(layout::HEADER_HEIGHT + space::S8)).children(note))
                .into_any_element();
        }
        let (Some(message), Some(run)) = (self.item(key, Surface::Thread, ix), view.runs.get(ix - 1)) else {
            return div().into_any_element();
        };
        let replies = view.entries.len() - usize::from(view.starter);
        let marker = (view.starter && ix == 2).then(|| {
            let label = if replies == 1 { "1 reply".to_owned() } else { format!("{replies} replies") };
            message_log::divider(label.into(), p.muted, p.line)
        });
        self.render_message(key, log, message, run, Surface::Thread, marker, window, p, cx)
    }

    /// A message as a row of the log mail between people shares (`message_log`, MS-326): the
    /// sender's photo, name and time over a run of their messages, the user's own on the right;
    /// under its day's line and `marker` (the "New" line, the thread's count of replies). In the
    /// log, the first message of a thread with replies tells how many under it, a reply to a
    /// message not loaded says so, and hovering a message offers to reply in its thread; each
    /// opens the thread (MS-329).
    #[allow(clippy::too_many_arguments)]
    fn render_message(
        &self,
        key: &Key,
        log: &Log,
        message: &Msg,
        run: &mailstream::ChatRun,
        surface: Surface,
        marker: Option<Div>,
        window: &mut Window,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The thread's island shows the message that started it a second time: its elements' ids
        // are its own.
        let tag = match surface {
            Surface::Log => "",
            Surface::Thread => "thread:",
        };
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        // Touch / mobile never shows desktop hover washes or floating "Reply in Thread" capsules
        // (MS-436); actions live in the long-press bottom sheet instead.
        let touch_mode = compact || window.last_input_was_touch() || cfg!(target_os = "android");
        let continued = run.continued;
        let sheet_selected = self.message_sheet.as_ref().is_some_and(|sheet| {
            sheet.key == *key && sheet.surface == surface && sheet.message_id == message.id
        });
        let hovered = !touch_mode && self.hovered_message.as_ref().is_some_and(|(at, id)| *at == surface && *id == message.id);
        let highlighted = hovered || sheet_selected;
        let hover_id = message.id.clone();

        let lead = if continued {
            let color = if highlighted { p.muted } else { gpui::transparent_black() };
            message_log::run_time(message.time.clone(), color, false).into_any_element()
        } else {
            // The user's own address is the account's when Chat doesn't give it.
            let email = if message.email.is_empty() && message.mine { &key.account } else { &message.email };
            avatars::avatar(email, &message.avatar, log_metrics::AVATAR, p, cx)
        };
        let time: SharedString = match &message.status {
            Status::Sent => message.time.clone(),
            Status::Sending => "Sending…".into(),
            Status::Failed(_) => "Not sent".into(),
        };
        let head = (!continued).then(|| message_log::head(message.name.clone(), time, None::<AnyElement>, message.mine, p));
        // The thread a click here opens.
        let thread = message.thread.clone().filter(|_| surface == Surface::Log && matches!(message.status, Status::Sent));

        let mut content: Vec<AnyElement> = Vec::new();
        if let (true, Some(thread)) = (message.thread_reply && surface == Surface::Log, message.thread.clone()) {
            content.push(
                div()
                    .id(ElementId::Name(format!("chat-in-thread:{}", message.id).into()))
                    .flex()
                    .gap(px(space::S6))
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .cursor_pointer()
                    .child("↳ Replied in a thread")
                    .child(div().text_color(p.accent).child("View thread"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_thread(thread.clone(), window, cx)))
                    .into_any_element(),
            );
        }
        if !message.body.is_empty() {
            content.push(
                div()
                    .max_w_full()
                    .min_w_0()
                    .text_size(px(design::type_scale(f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH).reading))
                    .line_height(relative(text::READING_LEADING))
                    .when(matches!(message.status, Status::Failed(_)), |d| d.text_color(p.muted))
                    .child(message.body.element(&format!("{tag}{}", message.id), p))
                    .into_any_element(),
            );
        }
        if !message.attachments.is_empty() {
            content.push(
                div()
                    .max_w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .when(message.mine, |d| d.justify_end())
                    .items_start()
                    .gap(px(space::S8))
                    .pt(px(space::S6))
                    .children(message.attachments.iter().enumerate().map(|(index, attachment)| {
                        let id = ElementId::Name(format!("chat-file:{tag}{}:{index}", message.id).into());
                        self.render_attachment(&key.account, id, attachment, window, p, cx)
                    }))
                    .into_any_element(),
            );
        }
        if let Status::Failed(reason) = &message.status {
            let (retry_key, retry_id) = (key.clone(), message.id.clone());
            let (discard_key, discard_id) = (key.clone(), message.id.clone());
            content.push(
                // Wraps within the message's column, now narrower than the log (MS-350).
                div()
                    .max_w_full()
                    .flex()
                    .flex_wrap()
                    .when(message.mine, |d| d.justify_end())
                    .items_center()
                    .gap(px(6.))
                    .pt(px(4.))
                    .text_size(px(text::FOOTNOTE))
                    .child(div().text_color(p.danger).child(format!("Not sent: {reason}")))
                    .child(button(ElementId::Name(format!("retry:{tag}{}", message.id).into()), "Retry", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.retry(&retry_key, &retry_id, window, cx)),
                    ))
                    .child(button(ElementId::Name(format!("discard:{tag}{}", message.id).into()), "Delete", ButtonKind::Plain(p.accent), ControlSize::Small, true, p).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.discard(&discard_key, &discard_id, cx)),
                    ))
                    .into_any_element(),
            );
        }
        if let (Surface::Log, Some(replies)) = (surface, log.replies(message)) {
            content.push(self.thread_summary(message, replies, compact, p, cx));
        }

        let reply = (!touch_mode).then_some(thread).flatten().map(|thread| {
            raised_capsule(p)
                .child(button(ElementId::Name(format!("reply:{}", message.id).into()), "Reply in Thread", ButtonKind::Plain(p.accent), ControlSize::Small, true, p).on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| this.open_thread(thread.clone(), window, cx)),
                ))
                .into_any_element()
        });

        let lp_entity = cx.entity();
        let (lp_key, lp_id) = (key.clone(), message.id.clone());
        let long_press = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_bounds, hitbox, window, _| {
                let lp_entity = lp_entity.clone();
                let lp_key = lp_key.clone();
                let lp_id = lp_id.clone();
                window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.phase == TouchPhase::Started
                        && !window.default_prevented()
                        && hitbox.is_hovered(window)
                    {
                        let opened = lp_entity.update(cx, |this, cx| {
                            this.open_message_sheet(lp_key.clone(), surface, lp_id.clone(), cx)
                        });
                        if opened {
                            design::compact::haptic_long_press();
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }
                });
            },
        )
        .absolute()
        .inset_0();

        let (rc_key, rc_id) = (key.clone(), message.id.clone());
        let row = message_log::Row { id: ElementId::Name(format!("chat-msg:{tag}{}", message.id).into()), mine: message.mine, continued, hovered: highlighted };
        let debug_msg_id = format!("chat-msg:{tag}{}", message.id);
        let row = message_log::message(row, lead, head, content, reply, p)
            .debug_selector(move || debug_msg_id.clone())
            .child(long_press)
            .when(matches!(message.status, Status::Sending), |d| d.opacity(metrics::PENDING_OPACITY))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    if this.open_message_sheet(rc_key.clone(), surface, rc_id.clone(), cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_hover(cx.listener(move |this, hovering: &bool, window, cx| {
                let touch_mode = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH
                    || window.last_input_was_touch()
                    || cfg!(target_os = "android");
                let here = this.hovered_message.as_ref().is_some_and(|(at, id)| *at == surface && *id == hover_id);
                if *hovering && !touch_mode {
                    this.hovered_message = Some((surface, hover_id.clone()));
                } else if here {
                    this.hovered_message = None;
                } else {
                    return;
                }
                cx.notify();
            }));

        Self::column(
            surface,
            compact,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .when(!run.divider.is_empty(), |d| d.child(message_log::day_divider(run.divider.clone().into(), p)))
                .children(marker)
                .child(row),
        )
        .into_any_element()
    }

    /// Under the first message of a thread with replies, as Google Chat shows it: the faces of
    /// who replied, how many replies and when the last came; a click opens the thread (MS-329).
    fn thread_summary(&self, message: &Msg, replies: &Replies, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let count = replies.entries.len();
        let label = if count == 1 { "1 reply".to_owned() } else { format!("{count} replies") };
        let faces = div().flex().children(replies.faces.iter().enumerate().map(|(index, (email, avatar))| {
            div()
                .when(index > 0, |d| d.ml(px(-metrics::THREAD_FACE_OVERLAP)))
                .rounded_full()
                .border(px(stroke::RIM * 2.))
                .border_color(p.background)
                .child(avatars::avatar(email, avatar, metrics::THREAD_FACE, p, cx))
        }));
        let thread = message.thread.clone().unwrap_or_default();
        div()
            .id(ElementId::Name(format!("chat-replies:{}", message.id).into()))
            .mt(px(space::S4))
            // The wash reaches past the faces, the text staying in line with the message's.
            .map(|d| if message.mine { d.mr(px(-space::S6)) } else { d.ml(px(-space::S6)) })
            .px(px(space::S6))
            .py(px(if compact { space::S6 } else { space::S2 }))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .hover(|d| d.bg(p.hover))
            .text_size(px(text::FOOTNOTE))
            .child(faces)
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.accent).child(label))
            .when(!replies.last.is_empty(), |d| d.child(div().text_color(p.muted).child(format!("Last reply {}", replies.last))))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_thread(thread.clone(), window, cx)))
            .into_any_element()
    }

    /// Android/mobile-style bottom action sheet opened by long-pressing (or right-clicking) a chat
    /// message (MS-436): shows a compact preview of the message and large touch-friendly actions
    /// (Reply in Thread, Copy Text, Retry/Delete on failed sends).
    fn render_message_sheet(&self, p: &'static Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.message_sheet.as_ref()?.clone();
        let (log, message) = self.message_for(&sheet)?;
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        let ts = design::type_scale(compact);
        let plain_text = message.body.plain_text();
        let preview_text: SharedString = {
            let trimmed = plain_text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !trimmed.is_empty() {
                trimmed.into()
            } else if message.attachments.len() == 1 {
                "Attachment".into()
            } else if !message.attachments.is_empty() {
                format!("{} attachments", message.attachments.len()).into()
            } else {
                "Message".into()
            }
        };
        let row_h = if compact { design::compact::drawer_row_height() } else { 42. };
        let glyph_size = if compact { 20. } else { icon_size::TOOLBAR };

        let action_row = |id: &'static str, glyph: Icon, glyph_color: gpui::Hsla, label: &'static str, label_color: gpui::Hsla, trailing: Option<SharedString>| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .h(px(row_h))
                .px(px(space::S12))
                .rounded(px(radius::CONTROL))
                .flex()
                .items_center()
                .gap(px(space::S12))
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .active(|s| s.bg(p.selection))
                .child(icon(glyph, glyph_size, glyph_color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(ts.body))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(label_color)
                        .child(label),
                )
                .when_some(trailing, |d, note| {
                    d.child(div().flex_shrink_0().text_size(px(ts.footnote)).text_color(p.muted).child(note))
                        .child(icon(Icon::Disclosure, icon_size::INLINE, p.muted))
                })
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        if sheet.surface == Surface::Log
            && matches!(message.status, Status::Sent)
            && let Some(thread) = message.thread.clone()
        {
            let reply_count = log.replies(message).map(|r| {
                let n = r.entries.len();
                SharedString::from(if n == 1 { "1 reply".to_owned() } else { format!("{n} replies") })
            });
            rows.push(
                action_row("chat-sheet-reply", Icon::Reply, p.accent, "Reply in Thread", p.text, reply_count)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.close_message_sheet(cx);
                        this.open_thread(thread.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if !plain_text.trim().is_empty() {
            let copy_text = plain_text.clone();
            rows.push(
                action_row("chat-sheet-copy", Icon::Copy, p.glyph, "Copy Text", p.text, None)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        this.close_message_sheet(cx);
                        cx.emit(ChatToast("Copied".into()));
                    }))
                    .into_any_element(),
            );
        }
        if matches!(message.status, Status::Failed(_)) {
            let (retry_key, retry_id) = (sheet.key.clone(), sheet.message_id.clone());
            let (discard_key, discard_id) = (sheet.key.clone(), sheet.message_id.clone());
            rows.push(
                action_row("chat-sheet-retry", Icon::Refresh, p.accent, "Retry", p.text, None)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.close_message_sheet(cx);
                        this.retry(&retry_key, &retry_id, window, cx);
                    }))
                    .into_any_element(),
            );
            rows.push(
                action_row("chat-sheet-delete", Icon::Delete, p.danger, "Delete", p.danger, None)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.close_message_sheet(cx);
                        this.discard(&discard_key, &discard_id, cx);
                    }))
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            return None;
        }

        let scrim_alpha = if p.dark { 0.54 } else { 0.32 };
        let bottom_inset = if compact { design::compact::safe_area_insets().1 + space::S12 } else { space::S8 };
        let panel = div()
            .id("chat-message-sheet-panel")
            .debug_selector(|| "chat-message-sheet-panel".into())
            .flex()
            .flex_col()
            .bg(p.window)
            .border(px(stroke::HAIRLINE))
            .border_color(p.island_rim)
            .shadow(p.popover_shadow.clone())
            .overflow_hidden()
            .pb(px(bottom_inset))
            .map(|d| {
                if compact {
                    d.w_full().rounded_t(px(radius::PANE + 8.))
                } else {
                    d.w(px(360.)).max_w(relative(0.92)).rounded(px(radius::PANE + 4.))
                }
            })
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w_full()
                    .pt(px(space::S10))
                    .pb(px(space::S6))
                    .flex()
                    .justify_center()
                    .child(div().w(px(36.)).h(px(4.)).rounded_full().bg(p.muted.opacity(0.32))),
            )
            .child(
                div()
                    .px(px(space::S16))
                    .pt(px(space::S2))
                    .pb(px(space::S12))
                    .flex()
                    .flex_col()
                    .gap(px(space::S4))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(space::S6))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(ts.footnote))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.text)
                                    .child(message.name.clone()),
                            )
                            .when(!message.time.is_empty(), |d| {
                                d.child(div().flex_shrink_0().text_size(px(ts.caption)).text_color(p.muted).child(message.time.clone()))
                            }),
                    )
                    .child(div().w_full().min_w_0().truncate().text_size(px(ts.footnote)).text_color(p.muted).child(preview_text)),
            )
            .child(hairline(p))
            .child(div().p(px(space::S8)).flex().flex_col().gap(px(space::S2)).children(rows));

        Some(
            div()
                .id("chat-message-sheet-backdrop")
                .debug_selector(|| "chat-message-sheet".into())
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .when(!compact, |d| d.items_center().pb(px(space::S20)))
                .bg(hsla(0., 0., 0., scrim_alpha))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.close_message_sheet(cx);
                }))
                .child(panel)
                .into_any_element(),
        )
    }

    /// The log's head, under the toolbar's band: the space's heading (as the reader's title, in the
    /// messages' column, MS-328), then what keeps the log empty (loading, a failure, no messages)
    /// or the button for earlier messages. Over messages already shown (from the cache) neither
    /// loading nor a failed refresh takes a line here; the heading tells of the failure (MS-326).
    fn render_top(&self, key: &Key, log: &Log, window: &mut Window, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let empty = log.len() == 0;
        let content: AnyElement = if let Some(failure) = log.failure.as_ref().filter(|_| empty) {
            div().py(px(24.)).text_color(p.danger).child(format!("Could not load messages: {failure}")).into_any_element()
        } else if !log.loaded && empty {
            div().py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element()
        } else if log.more {
            let key = key.clone();
            div()
                .py(px(12.))
                .flex()
                .justify_center()
                .child(
                    button("chat-earlier", if log.loading_earlier { "Loading…" } else { "Load earlier messages" }, ButtonKind::Plain(p.muted), ControlSize::Small, true, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.load_earlier(&key, cx))),
                )
                .into_any_element()
        } else if empty {
            div().py(px(24.)).text_color(p.muted).child("No messages yet.").into_any_element()
        } else {
            div().pt(px(24.)).into_any_element()
        };
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        Self::column(
            Surface::Log,
            compact,
            div().pt(px(layout::HEADER_HEIGHT + space::S16)).child(self.render_heading(key, log, p)).child(content),
        )
        .into_any_element()
    }

    /// The space's title, large, over its pills: its kind, its account (with several), how many
    /// people are in a group; a refresh that failed over the messages shown, in red.
    fn render_heading(&self, key: &Key, log: &Log, p: &'static Palette) -> Div {
        let space = self.space(key);
        let mut pills = vec![pill(space.map_or("Space", Space::kind_title), None, p)];
        if self.accounts.len() > 1 {
            let dot = self.account_color(&key.account).map(|color| mark_dot(color).into_any_element());
            pills.push(pill(self.account_label(&key.account), dot, p));
        }
        if space.is_some_and(|s| s.kind == mailstream::ChatSpaceKind::Group) && !log.members.is_empty() {
            pills.push(pill(format!("{} people", log.members.len() + 1), None, p));
        }
        let refresh_failure = log.failure.clone().filter(|_| log.len() > 0);
        // Its height, for the toolbar to tell when it has scrolled under it.
        let height = self.head_height.clone();
        let measure = canvas(move |bounds, _, _| height.set(f32::from(bounds.size.height)), |_, _, _, _| {});
        div()
            .relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(px(text::DISPLAY))
                    .line_height(relative(1.2))
                    .font_weight(FontWeight::BOLD)
                    .child(self.space_title(key)),
            )
            .child(div().mt(px(space::S12)).flex().flex_wrap().items_center().gap(px(space::S8)).children(pills))
            .children(refresh_failure.map(|failure| {
                div().mt(px(space::S8)).text_size(px(text::FOOTNOTE)).text_color(p.danger).child(format!("Couldn't update: {failure}"))
            }))
            .child(measure.absolute().top_0().left_0().size_full())
    }

    /// An attachment in the log: an uploaded picture as itself once downloaded (Flutter's
    /// `_ChatImage`), an uploaded video as its player on Linux (`_ChatVideo`); anything else, or a
    /// picture that can't be shown, as a tile.
    fn render_attachment(
        &self,
        account: &SharedString,
        id: ElementId,
        attachment: &Attachment,
        window: &mut Window,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let file = attachment.resource.as_ref().and_then(|resource| self.files.get(&(account.clone(), resource.clone())));
        if attachment.inline() {
            match file {
                None | Some(FileState::Loading) => return picture_loading(p),
                Some(FileState::Ready(path)) => match window.use_asset::<ImgResourceLoader>(&Resource::Path(path.clone()), cx) {
                    None => return picture_loading(p),
                    Some(Ok(image)) => return picture(id, path.clone(), image),
                    Some(Err(_)) => {}
                },
                Some(FileState::Failed(_)) => {}
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if attachment.kind == AttachmentKind::Video && attachment.resource.is_some() {
            return self.render_video(account, id, attachment, window, cx);
        }
        self.render_tile(account, id, attachment, file, p, cx)
    }

    /// A file as Chat on the web shows it (`_AttachmentTile`): its kind's glyph in its color, its
    /// name and kind. Clicking opens it in the system's default app: an uploaded file once
    /// downloaded, else its link in the browser; one with neither shows dimmed.
    fn render_tile(
        &self,
        account: &SharedString,
        id: ElementId,
        attachment: &Attachment,
        file: Option<&FileState>,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let opens = attachment.resource.is_some() || attachment.url.is_some();
        let waiting = attachment
            .resource
            .as_ref()
            .is_some_and(|resource| self.open_when_ready.contains(&(account.clone(), resource.clone())));
        let (detail, detail_color) = match file {
            Some(FileState::Failed(reason)) => (SharedString::from(format!("Could not download: {reason}")), p.danger),
            _ if waiting => ("Downloading…".into(), p.muted),
            _ => (attachment.label.clone(), p.muted),
        };
        let (account, chosen) = (account.clone(), attachment.clone());
        div()
            .id(id)
            .min_w(px(metrics::TILE_MIN_WIDTH))
            .max_w(px(metrics::TILE_MAX_WIDTH))
            .px(px(space::S12))
            .py(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S10))
            .rounded(px(radius::CARD))
            .bg(p.fill)
            .border(px(stroke::HAIRLINE))
            .border_color(p.line)
            .when(!opens, |d| d.opacity(metrics::DIMMED))
            .when(opens, |d| d.cursor_pointer())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_attachment(&account, &chosen, cx)))
            .child(icon(Icon::Attachment, metrics::TILE_GLYPH, design::attachment_color(attachment.kind, p.dark)))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(attachment.name.clone()))
                    .child(div().truncate().text_size(px(text::FOOTNOTE)).text_color(detail_color).child(detail)),
            )
            .into_any_element()
    }

    /// An uploaded video's player, kept while its row shows: scrolled away, it stops and shows its
    /// poster again when back (the file stays downloaded).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn render_video(
        &self,
        account: &SharedString,
        id: ElementId,
        attachment: &Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = cx.entity().downgrade();
        let zoom_pane = pane.clone();
        let (account, chosen) = (account.clone(), attachment.clone());
        window
            .use_keyed_state(id, cx, move |window, cx| {
                let name = chosen.name.clone();
                let fetch: video::Fetch = Rc::new(move |video, cx| {
                    pane.update(cx, |pane, cx| pane.fetch_video(&account, &chosen, video, cx)).ok();
                });
                let on_zoom: video::Zoom = Rc::new(move |video, window, cx| {
                    zoom_pane.update(cx, |pane, cx| pane.open_video_lightbox(video, window, cx)).ok();
                });
                ChatVideo::new(name, fetch, on_zoom, window, cx)
            })
            .into_any_element()
    }

    /// `surface`'s composer: the space's, or the open thread's (MS-329); `send_tip` names where its
    /// send button sends.
    fn render_composer(
        &self,
        surface: Surface,
        compact: bool,
        send_tip: SharedString,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let composer = self.composer(surface).clone();
        let can_send = !composer.read(cx).value().trim().is_empty();
        let (id, send_id) = match surface {
            Surface::Log => ("chat-composer", "chat-send"),
            Surface::Thread => ("chat-thread-composer", "chat-thread-send"),
        };
        let field = composer.clone();
        let ime_shift = if compact { design::compact::ime_insets().bar_shift_y() } else { 0.0 };
        Self::column(
            surface,
            compact,
            div()
                .relative()
                .when(ime_shift != 0.0, |d| d.top(px(ime_shift)))
                .pt(px(8.))
                .pb(px(16.))
                .child(
                    div()
                        .id(id)
                    .key_context(COMPOSER_CONTEXT)
                    .min_h(px(metrics::COMPOSER_HEIGHT))
                    .pl(px(12. - input::AREA_INSET))
                    .pr(px(12.))
                    .py(px(metrics::COMPOSER_PADDING_Y))
                    .flex()
                    .items_end()
                    .gap(px(8.))
                    .rounded(px(radius::CARD))
                    .bg(p.chip)
                    .text_size(px(text::READING))
                    .line_height(relative(1.55))
                    .cursor_text()
                    .on_click(move |_, window, cx| window.focus(&field.focus_handle(cx), cx))
                    .on_action(cx.listener(move |this, _: &Escape, window, cx| this.composer_escape(surface, window, cx)))
                    .on_action(cx.listener(|_, action: &Enter, _, cx| {
                        if action.shift {
                            cx.propagate();
                        }
                    }))
                    .on_action(cx.listener(move |this, _: &FormatBold, window, cx| this.format_composer(surface, "*", window, cx)))
                    .on_action(cx.listener(move |this, _: &FormatItalic, window, cx| this.format_composer(surface, "_", window, cx)))
                    .on_action(cx.listener(move |this, _: &FormatStrike, window, cx| this.format_composer(surface, "~", window, cx)))
                    .on_action(cx.listener(move |this, _: &FormatCode, window, cx| this.format_composer(surface, "`", window, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(input::editor(&composer).auto_grow(metrics::COMPOSER_MIN_ROWS, metrics::COMPOSER_MAX_ROWS)),
                    )
                    .child(
                        div().flex_shrink_0().h(px(text::READING * 1.55)).flex().items_center().child(
                            icon_button(send_id, Icon::Sent, send_tip, can_send, p.accent)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.send(surface, window, cx))),
                        ),
                    ),
            ),
        )
        .into_any_element()
    }

    fn render_form(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let form = &self.form;
        let can_start = form
            .account
            .as_ref()
            .and_then(|id| self.chat_accounts.iter().find(|a| &a.id == id))
            .is_none_or(|a| a.can_start);
        let count = form.people.len();
        // Something failed or stands in the way: a notice; otherwise what Start will do.
        let (note_text, tone): (SharedString, Option<Tone>) = match &form.failure {
            Some(failure) => (failure.clone(), Some(Tone::Error)),
            None if form.starting => ("Starting…".into(), None),
            None if count > 1 => (format!("Starts a group chat with these {count} people.").into(), None),
            None if !can_start => (
                "This account can only reopen direct messages it has; connect Chat in full (Settings › Accounts) to start new chats.".into(),
                Some(Tone::Action),
            ),
            None => ("Opens your direct message with this person, or starts one.".into(), None),
        };
        let accounts = (self.accounts.len() > 1).then(|| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(div().text_color(p.muted).child("From"))
                .children(self.accounts.iter().map(|account| {
                    let id = account.id.clone();
                    let selected = form.account.as_ref() == Some(&account.id);
                    button(ElementId::Name(format!("chat-from:{}", account.id).into()), account.label.clone(), ButtonKind::Secondary, ControlSize::Small, true, p)
                        .when(selected, |b| b.bg(p.selection))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.form.account = Some(id.clone());
                            this.form.failure = None;
                            this.form.partners.clear();
                            this.suggest(cx);
                            this.load_partners(cx);
                        }))
                }))
        });
        let people_input = self.people_input.clone();
        let field = div()
            .id("chat-people")
            .min_h(px(metrics::COMPOSER_HEIGHT))
            .px(px(10.))
            .py(px(6.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .rounded(px(radius::CARD))
            .bg(p.chip)
            .cursor_text()
            .on_click(move |_, window, cx| window.focus(&people_input.focus_handle(cx), cx))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close(window, cx)))
            .children(form.people.iter().enumerate().map(|(index, person)| {
                let email = person.email.clone();
                div()
                    .id(ElementId::Name(format!("chat-person:{index}").into()))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .py(px(2.))
                    .rounded(px(radius::CONTROL))
                    .bg(p.selection)
                    .text_size(px(text::FOOTNOTE))
                    .child(person.label())
                    .child(
                        div()
                            .id(ElementId::Name(format!("chat-person-remove:{index}").into()))
                            .px(px(4.))
                            .rounded(px(radius::SMALL))
                            .text_color(p.muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.form.people.retain(|p| p.email != email);
                                this.suggest(cx);
                            }))
                            .child("✕"),
                    )
            }))
            .child(div().flex_1().min_w(px(160.)).h(px(22.)).flex().items_center().child(input::field(&self.people_input)));
        let suggestions = div().flex().flex_col().children(form.suggestions.iter().enumerate().map(|(index, person)| {
            let chosen = person.clone();
            div()
                .id(ElementId::Name(format!("chat-suggestion:{index}").into()))
                .h(px(rows::ROW_HEIGHT))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(radius::CONTROL))
                .cursor_pointer()
                .when(index == 0, |d| d.bg(p.selection))
                .when(index != 0, |d| d.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.add_person(chosen.clone(), window, cx)))
                .child(avatars::avatar(&person.email, &mailstream::avatar(person.label().to_string(), person.email.to_string()), rows::AVATAR, p, cx))
                .child(div().min_w_0().truncate().child(person.label()))
                .when(person.name.is_some(), |d| d.child(div().min_w_0().truncate().text_color(p.muted).child(person.email.clone())))
        }));
        let start_enabled = count > 0 && !form.starting && form.account.is_some();
        let start = div()
            .id("chat-start")
            .h(px(28.))
            .px(px(14.))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .text_size(px(text::FOOTNOTE))
            .font_weight(FontWeight::MEDIUM)
            .bg(if start_enabled { p.accent } else { p.chip })
            .text_color(if start_enabled { p.on_accent } else { p.faint })
            .when(start_enabled, |d| d.cursor_pointer())
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_chat(window, cx)))
            .child("Start");
        let (glyph, tip) = if self.two_pane { (Icon::Close, "Close (Esc)") } else { (Icon::Back, "Back (Esc)") };
        let close = icon_button("chat-form-close", glyph, tip, true, p.glyph)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.slide_off(SwipeTarget::Page, window, cx)));
        let compact = f32::from(window.viewport_size().width) < layout::COMPACT_WIDTH;
        (if compact { div().relative().overflow_hidden().bg(p.background) } else { island(p.background, p) })
            .occlude()
            .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Page, cx))
            .size_full()
            .child(
                div().size_full().flex().justify_center().pt(px(layout::HEADER_HEIGHT + space::S16)).child(
                    div()
                        .w_full()
                        .max_w(px(metrics::FORM_WIDTH))
                        .px(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(
                            div()
                                .pb(px(space::S8))
                                .text_size(px(text::DISPLAY))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight::BOLD)
                                .child("New Chat"),
                        )
                        .children(accounts)
                        .child(field)
                        .child(suggestions)
                        .child(match tone {
                            Some(tone) => notice(tone, None, note_text, false, p),
                            None => div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(note_text),
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(button("chat-form-cancel", "Cancel", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
                                .child(start),
                        ),
                ),
            )
            .child(
                page_toolbar("chat-form-toolbar", close, None, p, window, cx)
                    .on_mouse_down(MouseButton::Left, Self::press_surface_listener(SwipeTarget::Page, cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    struct ComposerHarness {
        composer: Entity<EditorState>,
        sent: Vec<String>,
        _subscription: Subscription,
    }

    impl ComposerHarness {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let composer = cx.new(|cx| {
                let mut state = EditorState::new(window, cx)
                    .line_number(false)
                    .folding(false)
                    .indent_guides(false)
                    .smart_indent(false)
                    .auto_close(false)
                    .soft_wrap(true)
                    .searchable(false)
                    .scroll_beyond_last_line(Some(0))
                    .placeholder("Message")
                    .submit_on_enter(true);
                state.set_highlighter_factory(markup_highlighter_factory(MarkupFlavor::GoogleChat), cx);
                state.set_highlighter("google-chat", cx);
                state
            });
            let subscription = cx.subscribe_in(&composer, window, |this, composer, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event
                    && !composer.update(cx, |input, cx| input.marked_text_range(window, cx).is_some())
                {
                    let text = composer.read(cx).value().trim().to_owned();
                    if !text.is_empty() {
                        this.sent.push(text);
                        composer.update(cx, |input, cx| input.clean(window, cx));
                    }
                }
            });
            Self { composer, sent: Vec::new(), _subscription: subscription }
        }
    }

    impl Render for ComposerHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .key_context(CONTEXT)
                .child(
                    div()
                        .key_context(COMPOSER_CONTEXT)
                        .text_size(px(text::READING))
                        .line_height(relative(1.55))
                        .on_action(cx.listener(|_, action: &Enter, _, cx| {
                            if action.shift {
                                cx.propagate();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &FormatBold, window, cx| {
                            this.composer.update(cx, |input, cx| wrap_selection(input, "*", window, cx));
                        }))
                        .on_action(cx.listener(|this, _: &FormatItalic, window, cx| {
                            this.composer.update(cx, |input, cx| wrap_selection(input, "_", window, cx));
                        }))
                        .on_action(cx.listener(|this, _: &FormatStrike, window, cx| {
                            this.composer.update(cx, |input, cx| wrap_selection(input, "~", window, cx));
                        }))
                        .on_action(cx.listener(|this, _: &FormatCode, window, cx| {
                            this.composer.update(cx, |input, cx| wrap_selection(input, "`", window, cx));
                        }))
                        .child(
                            div()
                                .debug_selector(|| "chat-composer-frame".into())
                                .child(input::editor(&self.composer).auto_grow(metrics::COMPOSER_MIN_ROWS, metrics::COMPOSER_MAX_ROWS)),
                        ),
                )
        }
    }

    fn with_composer(
        cx: &mut TestAppContext,
        run: impl FnOnce(Entity<ComposerHarness>, Entity<EditorState>, &mut gpui::VisualTestContext),
    ) {
        cx.update(|cx| {
            gpui::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let (view, cx) = cx.add_window_view(ComposerHarness::new);
        let composer = view.read_with(cx, |h, _| h.composer.clone());
        composer.update_in(cx, |input, window, cx| input.focus(window, cx));
        cx.run_until_parked();
        run(view, composer, cx);
    }

    #[gpui::test]
    fn enter_sends_shift_enter_inserts_newline_and_grows(cx: &mut TestAppContext) {
        with_composer(cx, |view, composer, cx| {
            composer.update_in(cx, |input, window, cx| input.set_value("hello", window, cx));
            composer.update_in(cx, |input, _, cx| input.set_selected_range(5..5, cx));
            cx.run_until_parked();

            let single_height = cx.debug_bounds("chat-composer-frame").unwrap().size.height;
            cx.simulate_keystrokes("shift-enter");
            cx.run_until_parked();
            composer.update_in(cx, |input, window, cx| input.insert("world", window, cx));
            cx.run_until_parked();

            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "hello\nworld");
            assert!(view.read_with(cx, |h, _| h.sent.is_empty()));
            let two_line_height = cx.debug_bounds("chat-composer-frame").unwrap().size.height;
            assert!(two_line_height > single_height, "composer should grow on newline: {two_line_height:?} vs {single_height:?}");

            // Growing past COMPOSER_MAX_ROWS caps the height and scrolls instead.
            for _ in 0..8 {
                cx.simulate_keystrokes("shift-enter");
            }
            cx.run_until_parked();
            let capped_height = cx.debug_bounds("chat-composer-frame").unwrap().size.height;
            composer.update_in(cx, |input, _, _| {
                assert!(input.display_map().wrap_row_count() > metrics::COMPOSER_MAX_ROWS);
            });
            cx.simulate_keystrokes("shift-enter");
            cx.run_until_parked();
            assert_eq!(cx.debug_bounds("chat-composer-frame").unwrap().size.height, capped_height);
            // Plain Enter sends and clears.
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert_eq!(view.read_with(cx, |h, _| h.sent.len()), 1);
            assert!(view.read_with(cx, |h, _| h.sent[0].starts_with("hello\nworld")));
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "");
        });
    }

    #[gpui::test]
    fn enter_never_sends_while_ime_composing(cx: &mut TestAppContext) {
        with_composer(cx, |view, composer, cx| {
            composer.update_in(cx, |input, window, cx| {
                input.replace_and_mark_text_in_range(None, "nihao", None, window, cx);
                assert!(input.marked_text_range(window, cx).is_some());
            });
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(view.read_with(cx, |h, _| h.sent.is_empty()));
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "nihao");

            // Committing IME composition clears marked text; Enter then sends.
            composer.update_in(cx, |input, window, cx| {
                input.replace_text_in_range(None, "你好", window, cx);
                assert!(input.marked_text_range(window, cx).is_none());
            });
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert_eq!(view.read_with(cx, |h, _| h.sent.clone()), vec!["你好"]);
        });
    }

    #[gpui::test]
    fn formatting_shortcuts_wrap_and_toggle_selection(cx: &mut TestAppContext) {
        with_composer(cx, |_, composer, cx| {
            composer.update_in(cx, |input, window, cx| input.set_value("hello 你好", window, cx));
            composer.update_in(cx, |input, _, cx| input.set_selected_range(0..5, cx));
            cx.run_until_parked();

            cx.simulate_keystrokes(secondary!("b"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "*hello* 你好");
            assert_eq!(composer.read_with(cx, |input, _| input.selected_range()), 1..6);

            cx.simulate_keystrokes(secondary!("i"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "*_hello_* 你好");

            // Pressing Cmd+I again unwraps the inner underscore.
            cx.simulate_keystrokes(secondary!("i"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "*hello* 你好");

            // Wrapping CJK text and testing Cmd+Shift+X and Cmd+E.
            let end = composer.read_with(cx, |input, _| input.text().len());
            let cjk_start = end - "你好".len();
            composer.update_in(cx, |input, _, cx| input.set_selected_range(cjk_start..end, cx));
            cx.simulate_keystrokes(secondary!("shift-x"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "*hello* ~你好~");

            // Empty selection inserts paired markers with cursor inside.
            composer.update_in(cx, |input, window, cx| input.set_value("", window, cx));
            cx.simulate_keystrokes(secondary!("e"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "``");
            assert_eq!(composer.read_with(cx, |input, _| input.selected_range()), 1..1);
            cx.simulate_keystrokes(secondary!("e"));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "");
        });
    }

    #[test]
    fn pending_body_uses_google_chat_syntax() {
        let blocks = mailstream::chat_draft_body("*bold* _it_ ~gone~ `code`\nnext".into(), Default::default());
        let body = Body::new(&blocks, false);
        assert_eq!(body.pieces.len(), 1);
        let inline = &body.pieces[0].inline;
        assert_eq!(inline.text.as_ref(), "bold it gone code\nnext");
        assert!(inline.marks.iter().any(|(_, m)| m.bold));
        assert!(inline.marks.iter().any(|(_, m)| m.italic));
        assert!(inline.marks.iter().any(|(_, m)| m.strike));
        assert!(inline.marks.iter().any(|(_, m)| m.code));
    }

    #[gpui::test]
    fn live_google_chat_highlighting_preserves_markers_and_selection(cx: &mut TestAppContext) {
        with_composer(cx, |_, composer, cx| {
            let sample = "*粗体* _斜体_ ~删除线~ `code`";
            composer.update_in(cx, |input, window, cx| input.set_value(sample, window, cx));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), sample);

            // Caret movement and selection over highlighted CJK + markers keep exact UTF-8 offsets.
            let bold_start = "*".len();
            let bold_end = bold_start + "粗体".len();
            composer.update_in(cx, |input, _, cx| input.set_selected_range(bold_start..bold_end, cx));
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.selected_range()), bold_start..bold_end);

            // IME composition inside highlighted text keeps markers intact.
            composer.update_in(cx, |input, window, cx| {
                input.replace_and_mark_text_in_range(None, "jiacu", None, window, cx);
                assert!(input.marked_text_range(window, cx).is_some());
                input.replace_text_in_range(None, "加粗", window, cx);
                assert!(input.marked_text_range(window, cx).is_none());
            });
            cx.run_until_parked();
            assert_eq!(composer.read_with(cx, |input, _| input.value().to_string()), "*加粗* _斜体_ ~删除线~ `code`");
        });
    }

    /// A message `id` from `sender`, `minutes` after the first, in the thread started by
    /// `thread` (its own when None).
    fn msg(id: &str, sender: &str, minutes: i64, thread: Option<&str>) -> Msg {
        Msg {
            id: id.to_owned().into(),
            sender_id: sender.to_owned().into(),
            name: sender.to_owned().into(),
            avatar: mailstream::avatar(sender.into(), sender.into()),
            email: format!("{sender}@example.com").into(),
            created: Some(DateTime::from_timestamp(1_800_000_000 + minutes * 60, 0).unwrap()),
            time: SharedString::default(),
            thread: Some(format!("t-{}", thread.unwrap_or(id)).into()),
            thread_reply: thread.is_some(),
            mine: false,
            body: Body::default(),
            attachments: Vec::new(),
            status: Status::Sent,
            draft: None,
        }
    }

    #[test]
    fn replies_fold_under_their_threads_first_message() {
        let mut log = Log::new();
        log.messages = vec![
            msg("a", "ann", 0, None),
            msg("b", "bo", 1, None),
            msg("a1", "bo", 2, Some("a")),
            msg("a2", "cy", 3, Some("a")),
            msg("x1", "cy", 4, Some("gone")),
            msg("a3", "bo", 5, Some("a")),
        ];
        log.relayout();
        let shown: Vec<&str> = log.shown.iter().map(|&ix| log.entry(ix).unwrap().id.as_ref()).collect();
        // Replies to "a" fold under it; a reply to a message not loaded stays in the log.
        assert_eq!(shown, ["a", "b", "x1"]);
        let replies = log.replies(&log.messages[0]).expect("a's replies");
        assert_eq!(replies.entries, [2, 3, 5]);
        // The latest repliers first, each once.
        let faces: Vec<&str> = replies.faces.iter().map(|(email, _)| email.as_ref()).collect();
        assert_eq!(faces, ["bo@example.com", "cy@example.com"]);
        assert!(log.replies(&log.messages[1]).is_none());
        assert_eq!(log.runs.len(), 3);

        // The open thread shows its first message, then its replies, a pending one last.
        log.open_thread("t-a".into());
        log.pending.push(msg("p", "me", 6, Some("a")));
        log.relayout();
        let view = log.thread.as_ref().unwrap();
        assert!(view.starter);
        assert_eq!(view.entries, [0, 2, 3, 5, 6]);
        assert_eq!(log.shown.len(), 3);
        assert_eq!(log.replies(&log.messages[0]).unwrap().entries.len(), 4);
    }

    struct BodyLayoutHarness {
        short: Body,
        wide: Body,
        col: Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>,
        short_bounds: Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>,
        wide_bounds: Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>,
    }

    impl Render for BodyLayoutHarness {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let p = design::palette(window.appearance());
            let measure = |slot: Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>| {
                canvas(move |bounds, _, _| {
                    *slot.borrow_mut() = Some(bounds);
                }, |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            };
            let row = |id: &'static str, body: &Body, slot: Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>| {
                let content = [div()
                    .relative()
                    .max_w_full()
                    .min_w_0()
                    .text_size(px(text::READING))
                    .line_height(relative(text::READING_LEADING))
                    .child(body.element(id, p))
                    .child(measure(slot))
                    .into_any_element()];
                message_log::message(
                    message_log::Row { id: ElementId::Name(id.into()), mine: true, continued: true, hovered: false },
                    message_log::run_time("2:43 PM".into(), gpui::transparent_black(), false),
                    None,
                    content,
                    None,
                    p,
                )
            };
            div()
                .size_full()
                .flex()
                .justify_center()
                .font_family(design::ui_font())
                .child(
                    div()
                        .relative()
                        .w_full()
                        .max_w(px(layout::READING_WIDTH))
                        .min_w_0()
                        .px(px(32.))
                        .flex()
                        .flex_col()
                        .child(row("short", &self.short, self.short_bounds.clone()))
                        .child(row("wide", &self.wide, self.wide_bounds.clone()))
                        .child(measure(self.col.clone())),
                )
        }
    }

    #[gpui::test]
    fn mine_body_with_wide_table_and_code_stays_inside_column(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
        });
        let short_blocks = mailstream::chat_draft_body("one sec".into(), Default::default());
        let wide_markup = "Intro paragraph that should wrap within the right-aligned message column.\n\n\
            | Col1 | Col2 | Col3 | Col4 |\n|---|---|---|---|\n\
            | `long_identifier_one` | `long_identifier_two` | `long_identifier_three` | `long_identifier_four` |\n\n\
            ```\nhttps://example.com/releases/very_long_unbroken_artifact_filename_without_spaces_for_wrapping_check.tar.gz\n```";
        let wide_blocks = mailstream::chat_body(wide_markup.into(), Default::default());
        let col_slot = Rc::new(std::cell::RefCell::new(None));
        let short_slot = Rc::new(std::cell::RefCell::new(None));
        let wide_slot = Rc::new(std::cell::RefCell::new(None));
        let (c, s, w) = (col_slot.clone(), short_slot.clone(), wide_slot.clone());
        let (_, cx) = cx.add_window_view(move |_, _| BodyLayoutHarness {
            short: Body::new(&short_blocks, false),
            wide: Body::new(&wide_blocks, false),
            col: c,
            short_bounds: s,
            wide_bounds: w,
        });
        cx.run_until_parked();

        let col = col_slot.borrow().expect("chat-col");
        let short = short_slot.borrow().expect("body-short");
        let wide = wide_slot.borrow().expect("body-wide");

        assert!(wide.left() >= col.left(), "wide body overflowed left of column: {wide:?} vs {col:?}");
        assert!(wide.size.width <= col.size.width * log_metrics::SHARE, "wide body exceeded column share: {wide:?} vs {col:?}");
        assert!(short.size.width < px(200.), "short own message should hug content, not stretch: {short:?}");
        assert!((short.right() - wide.right()).abs() < px(1.5), "own messages should align at right edge: {short:?} vs {wide:?}");
    }

    #[test]
    fn gesture_target_picks_the_surface_under_the_pointer() {
        // No page open.
        assert_eq!(chat_gesture_target(false, false, false, false, 400., 400., 120.), None);
        // Compact / single-pane: page covers whole width.
        assert_eq!(chat_gesture_target(true, false, false, false, 400., 400., 50.), Some(SwipeTarget::Page));
        // Compact with thread covering space: thread takes the gesture.
        assert_eq!(chat_gesture_target(true, true, false, false, 400., 400., 50.), Some(SwipeTarget::Thread));
        // Two-pane (list 0..340, gap 340..352, page 352..900): over list or gap does nothing.
        assert_eq!(chat_gesture_target(true, false, true, false, 900., 340., 120.), None);
        assert_eq!(chat_gesture_target(true, false, true, false, 900., 340., 345.), None);
        assert_eq!(chat_gesture_target(true, false, true, false, 900., 340., 500.), Some(SwipeTarget::Page));
        // Two-pane with thread beside space on right (thread starts at width - THREAD_WIDTH).
        let width = 1200.;
        let thread_left = width - metrics::THREAD_WIDTH;
        assert_eq!(chat_gesture_target(true, true, true, true, width, 340., 500.), Some(SwipeTarget::Page));
        assert_eq!(chat_gesture_target(true, true, true, true, width, 340., thread_left - 4.), None);
        assert_eq!(chat_gesture_target(true, true, true, true, width, 340., thread_left + 10.), Some(SwipeTarget::Thread));
    }

    #[gpui::test]
    fn compact_swipe_back_exits_open_chat_space_and_thread(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
        });
        let (pane, cx) = cx.add_window_view(|window, cx| ChatPane::new(window, cx));
        let key = Key { account: "sam@example.com".into(), space: "spaces/demo".into() };
        pane.update(cx, |chat, cx| {
            chat.set_split(390., 390., false, cx);
            chat.view = View::Space(key.clone());
            chat.logs.insert(key.clone(), Log::new());
            chat.logs.get_mut(&key).unwrap().open_thread("threads/1".into());
        });
        cx.run_until_parked();

        let swipe = |cx: &mut gpui::VisualTestContext,
                     pane: &Entity<ChatPane>,
                     dx: f32,
                     dy: f32,
                     mid_check: &dyn Fn(&ChatPane)| {
            let at = gpui::point(px(195.), px(240.));
            let event = |touch_phase, x: f32, y: f32| ScrollEvent {
                position: at,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(x), px(y))),
                modifiers: gpui::Modifiers::default(),
                touch_phase,
                ..Default::default()
            };
            let feed = |cx: &mut gpui::VisualTestContext, ev: ScrollEvent| {
                let step = pane.read_with(cx, |chat, _| chat.peek_swipe(&ev, Some(at)));
                if !matches!(step, Step::Pass | Step::Consume) {
                    pane.update(cx, |chat, cx| {
                        chat.follow(step, cx);
                    });
                }
            };
            feed(cx, event(TouchPhase::Started, 0., 0.));
            for _ in 0..8 {
                feed(cx, event(TouchPhase::Moved, dx / 8., dy / 8.));
            }
            pane.read_with(cx, |chat, _| mid_check(chat));
            feed(cx, event(TouchPhase::Ended, 0., 0.));
            let _ = cx.refresh();
            std::thread::sleep(Duration::from_millis(10));
            pane.update(cx, |chat, cx| {
                if let Slide::Settle { start, .. } = &mut chat.slide {
                    *start -= Duration::from_secs(2);
                    cx.notify();
                }
            });
            let _ = cx.refresh();
        };

        // Vertical scroll must not start a back-swipe.
        swipe(cx, &pane, 2., 180., &|chat| {
            assert!(!matches!(chat.slide, Slide::Drag(_)), "vertical scroll must not drag");
        });
        pane.read_with(cx, |chat, _| {
            assert!(chat.has_open_thread(), "vertical scroll should keep thread open");
            assert!(matches!(chat.view, View::Space(_)), "vertical scroll should keep space open");
        });

        // Horizontal left-swipe with thread open closes only the thread (past 1/3 of 390 = 130).
        swipe(cx, &pane, -180., 0., &|chat| {
            assert_eq!(chat.slide_target, Some(SwipeTarget::Thread));
            assert!(matches!(chat.slide, Slide::Drag(off) if (off + 180.).abs() < 1.));
        });
        pane.read_with(cx, |chat, _| {
            assert!(!chat.has_open_thread(), "thread should close once swipe-back settles");
            assert!(matches!(chat.view, View::Space(_)), "space should remain open after closing thread");
        });

        // Short horizontal swipe springs back without exiting the space.
        swipe(cx, &pane, 40., 0., &|chat| {
            assert_eq!(chat.slide_target, Some(SwipeTarget::Page));
            assert!(matches!(chat.slide, Slide::Drag(off) if (off - 40.).abs() < 1.));
        });
        pane.read_with(cx, |chat, _| {
            assert!(matches!(chat.view, View::Space(_)), "short swipe must spring back and keep space open");
        });

        // Horizontal left-swipe past threshold exits the open space back to View::Spaces.
        swipe(cx, &pane, -180., 0., &|chat| {
            assert_eq!(chat.slide_target, Some(SwipeTarget::Page));
            assert!(matches!(chat.slide, Slide::Drag(off) if (off + 180.).abs() < 1.));
        });
        pane.read_with(cx, |chat, _| {
            assert!(matches!(chat.view, View::Spaces), "left swipe-back should exit Chat space to space list");
        });

        // Horizontal right-swipe past threshold also exits the open space back to View::Spaces.
        pane.update(cx, |chat, cx| {
            chat.view = View::Space(key.clone());
            cx.notify();
        });
        cx.run_until_parked();
        swipe(cx, &pane, 180., 0., &|chat| {
            assert_eq!(chat.slide_target, Some(SwipeTarget::Page));
            assert!(matches!(chat.slide, Slide::Drag(off) if (off - 180.).abs() < 1.));
        });
        pane.read_with(cx, |chat, _| {
            assert!(matches!(chat.view, View::Spaces), "right swipe-back should exit Chat space to space list");
        });
    }

    fn test_message(id: &str, text: &str, thread: Option<&str>) -> Msg {
        let blocks = mailstream::chat_body(text.into(), Default::default());
        Msg {
            id: id.into(),
            sender_id: "users/1".into(),
            name: "Alex".into(),
            avatar: mailstream::Avatar::default(),
            email: "alex@example.com".into(),
            created: None,
            time: "10:14".into(),
            thread: thread.map(Into::into),
            thread_reply: false,
            mine: false,
            body: Body::new(&blocks, false),
            attachments: Vec::new(),
            status: Status::Sent,
            draft: None,
        }
    }

    fn seed_chat_pane<'a>(cx: &'a mut TestAppContext, width: f32, height: f32) -> (Entity<ChatPane>, &'a mut gpui::VisualTestContext, Key) {
        cx.update(|cx| {
            gpui::init(cx);
            avatars::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let key = Key { account: "sam@example.com".into(), space: "spaces/design".into() };
        let (pane, cx) = cx.add_window_view(|window, cx| ChatPane::new_without_load(window, cx));
        cx.update(|window, cx| {
            window.resize(gpui::size(px(width), px(height)));
            window.bounds_changed(cx);
        });
        pane.update_in(cx, |pane, window, cx| {
            pane.set_split(width, layout::MIN_LIST, width >= layout::COMPACT_WIDTH, cx);
            pane.view = View::Space(key.clone());
            let mut log = Log::new();
            log.loaded = true;
            log.title = Some("Design team".into());
            log.messages = vec![test_message("m1", "Can you take a look at the PR?", Some("spaces/design/threads/t1"))];
            log.relayout();
            pane.logs.insert(key.clone(), log);
            window.focus(&pane.focus, cx);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (pane, cx, key)
    }

    #[test]
    fn plain_text_extracts_paragraphs_and_tables() {
        let blocks = mailstream::chat_body("Hello *world*\n\n| A | B |\n|---|---|\n| 1 | 2 |".into(), Default::default());
        let body = Body::new(&blocks, false);
        assert_eq!(body.plain_text(), "Hello world\nA\tB\n1\t2");
    }

    #[gpui::test]
    fn touch_input_suppresses_on_hover_listeners(cx: &mut TestAppContext) {
        use gpui::{PlatformInput, TouchEvent, TouchId, point};
        use std::cell::RefCell;

        struct TouchHoverView {
            transitions: Rc<RefCell<Vec<bool>>>,
        }

        impl Render for TouchHoverView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let transitions = self.transitions.clone();
                div().size_full().child(
                    div()
                        .id("touch-hover-target")
                        .ml(px(40.))
                        .mt(px(40.))
                        .size(px(80.))
                        .on_hover(move |hovered, _, _| transitions.borrow_mut().push(*hovered)),
                )
            }
        }

        let transitions = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let transitions = transitions.clone();
            move |_, _| TouchHoverView { transitions }
        });
        let pos = point(px(60.), px(60.));

        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            for phase in [TouchPhase::Started, TouchPhase::Ended] {
                window.dispatch_event(
                    PlatformInput::Touch(TouchEvent {
                        id: TouchId(1),
                        phase,
                        position: pos,
                        predicted_position: None,
                        force: None,
                        timestamp: None,
                    }),
                    cx,
                );
            }
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        assert!(transitions.borrow().is_empty(), "touch tap must not trigger on_hover");

        // Mouse hover activates; subsequent touch clears it.
        cx.simulate_mouse_move(pos, None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*transitions.borrow(), [true]);

        cx.update(|window, cx| {
            for phase in [TouchPhase::Started, TouchPhase::Ended] {
                window.dispatch_event(
                    PlatformInput::Touch(TouchEvent {
                        id: TouchId(2),
                        phase,
                        position: pos,
                        predicted_position: None,
                        force: None,
                        timestamp: None,
                    }),
                    cx,
                );
            }
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        assert_eq!(*transitions.borrow(), [true, false]);
    }

    #[gpui::test]
    fn touch_tap_never_triggers_hover_or_reply_capsule(cx: &mut TestAppContext) {
        use gpui::{PlatformInput, TouchEvent, TouchId};

        let (pane, cx, _) = seed_chat_pane(cx, 390., 844.);
        let pos = cx.debug_bounds("chat-msg:m1").expect("chat message row bounds").center();
        cx.update(|window, cx| {
            for phase in [TouchPhase::Started, TouchPhase::Ended] {
                window.dispatch_event(
                    PlatformInput::Touch(TouchEvent {
                        id: TouchId(1),
                        phase,
                        position: pos,
                        predicted_position: None,
                        force: None,
                        timestamp: None,
                    }),
                    cx,
                );
            }
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();

        pane.read_with(cx, |pane, _| {
            assert!(pane.hovered_message.is_none(), "touch tap must not set hovered_message");
            assert!(pane.message_sheet.is_none(), "light touch tap must not open action sheet");
        });
        assert!(cx.debug_bounds("chat-message-sheet").is_none());
    }

    #[gpui::test]
    fn long_press_opens_message_sheet_copy_and_reply_in_thread(cx: &mut TestAppContext) {
        use gpui::PlatformInput;
        use std::sync::atomic::{AtomicBool, Ordering};

        let (pane, cx, key) = seed_chat_pane(cx, 390., 844.);
        let copied_toast = Arc::new(AtomicBool::new(false));
        let toast_flag = copied_toast.clone();
        let _sub = cx.update(|_, cx| {
            cx.subscribe(&pane, move |_, toast: &ChatToast, _| {
                if toast.0 == "Copied" {
                    toast_flag.store(true, Ordering::SeqCst);
                }
            })
        });

        // Long-press on the message opens the bottom action sheet (not hover reply capsule).
        let pos = cx.debug_bounds("chat-msg:m1").expect("chat message row bounds").center();
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::LongPress(LongPressEvent {
                    phase: TouchPhase::Started,
                    start_position: pos,
                    position: pos,
                }),
                cx,
            );
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();

        pane.read_with(cx, |pane, _| {
            assert!(pane.message_sheet.is_some(), "long-press should open message action sheet");
        });
        assert!(cx.debug_bounds("chat-message-sheet").is_some(), "sheet backdrop should render");
        let copy_bounds = cx.debug_bounds("chat-sheet-copy").expect("Copy Text action should render");
        assert!(cx.debug_bounds("chat-sheet-reply").is_some(), "Reply in Thread action should render");

        // Tapping Copy Text copies plain text, emits Copied toast, and closes the sheet.
        cx.simulate_click(copy_bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(pane.message_sheet.is_none(), "Copy Text should close sheet");
        });
        assert!(copied_toast.load(Ordering::SeqCst), "Copy Text should emit Copied toast");
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("Can you take a look at the PR?"));

        // Re-open via long-press, then Back closes the sheet without leaving the space.
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::LongPress(LongPressEvent {
                    phase: TouchPhase::Started,
                    start_position: pos,
                    position: pos,
                }),
                cx,
            );
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("chat-message-sheet").is_some());

        pane.update_in(cx, |pane, window, cx| pane.back(&Back, window, cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(pane.message_sheet.is_none(), "Back should dismiss the sheet first");
            assert!(matches!(&pane.view, View::Space(open) if *open == key), "space should remain open");
        });

        // Re-open and tap Reply in Thread -> opens thread and dismisses sheet.
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::LongPress(LongPressEvent {
                    phase: TouchPhase::Started,
                    start_position: pos,
                    position: pos,
                }),
                cx,
            );
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        let reply_bounds = cx.debug_bounds("chat-sheet-reply").expect("Reply in Thread row");
        cx.simulate_click(reply_bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(pane.message_sheet.is_none(), "Reply in Thread should close sheet");
            let thread = pane.logs.get(&key).and_then(|log| log.thread.as_ref());
            assert_eq!(thread.map(|t| t.id.as_ref()), Some("spaces/design/threads/t1"));
        });
    }
}

// MARK: - Pieces

/// A downloaded picture at its own size, shrunk to fit the log's media box; clicking opens it in
/// the system's image viewer.
fn picture(id: ElementId, path: Arc<Path>, image: Arc<gpui::RenderImage>) -> AnyElement {
    let size = image.size(0);
    let (width, height) = (size.width.0.max(1) as f32, size.height.0.max(1) as f32);
    let scale = (metrics::MEDIA_WIDTH / width).min(metrics::MEDIA_HEIGHT / height).min(1.0);
    div()
        .id(id)
        .flex_shrink_0()
        .cursor_pointer()
        .on_click(move |_, _, cx| cx.open_with_system(&path))
        .child(img(image).w(px(width * scale)).h(px(height * scale)).rounded(px(radius::PANE)).object_fit(ObjectFit::Contain))
        .into_any_element()
}

/// Where a picture shows once it is downloaded.
fn picture_loading(p: &Palette) -> AnyElement {
    let (width, height) = metrics::MEDIA_LOADING;
    div()
        .flex_shrink_0()
        .w(px(width))
        .h(px(height))
        .rounded(px(radius::PANE))
        .bg(p.fill)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(text::FOOTNOTE))
        .text_color(p.faint)
        .child("Loading…")
        .into_any_element()
}

// MARK: - A synthetic chat

/// A chat without an account, for looking at the pane (`MAILSTREAM_CHAT_FIXTURE=<file.json>`):
/// the spaces, histories, Chat partners and downloaded files come from the file instead of the
/// engine; everything else (sending, starting, contact lookup) still goes to the engine, which
/// has no chat account then. The file holds `spaces` (a `ChatSpaces`, shown `page` spaces at a time
/// while `chat_more` pages), `threads` (`ChatThread`s), `people` (`ChatUser`s) and `files` (each
/// `resource`'s path).
mod fixture {
    use std::collections::HashMap;
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mailstream::{ChatPeople, ChatSpaces, ChatThread, ChatUser, MailError};
    use serde_json::Value;

    struct Fixture {
        spaces: ChatSpaces,
        page: usize,
        threads: Vec<ChatThread>,
        people: Vec<ChatUser>,
        files: HashMap<String, String>,
    }

    static FIXTURE: LazyLock<Option<Fixture>> = LazyLock::new(|| {
        let path = std::env::var_os("MAILSTREAM_CHAT_FIXTURE")?;
        let file = std::fs::read(&path).expect("MAILSTREAM_CHAT_FIXTURE: unreadable");
        let value: Value = serde_json::from_slice(&file).expect("MAILSTREAM_CHAT_FIXTURE: not JSON");
        let field = |name: &str| value.get(name).cloned().unwrap_or(Value::Null);
        Some(Fixture {
            spaces: serde_json::from_value(field("spaces")).expect("MAILSTREAM_CHAT_FIXTURE: spaces"),
            page: value["page"].as_u64().map_or(usize::MAX, |page| page as usize),
            threads: serde_json::from_value(field("threads")).expect("MAILSTREAM_CHAT_FIXTURE: threads"),
            people: serde_json::from_value(field("people")).expect("MAILSTREAM_CHAT_FIXTURE: people"),
            files: serde_json::from_value(field("files")).expect("MAILSTREAM_CHAT_FIXTURE: files"),
        })
    });

    /// Pages of spaces shown.
    static PAGES: AtomicUsize = AtomicUsize::new(1);

    fn missing(what: &str) -> MailError {
        MailError::Failed { code: "notFound".into(), message: format!("The chat fixture has no {what}") }
    }

    pub fn spaces() -> Option<ChatSpaces> {
        let fixture = FIXTURE.as_ref()?;
        let mut answer = fixture.spaces.clone();
        let shown = fixture.page.saturating_mul(PAGES.load(Ordering::Relaxed));
        answer.more = shown < answer.spaces.len();
        answer.spaces.truncate(shown);
        answer.dress();
        Some(answer)
    }

    /// Shows another page of spaces; false without a fixture.
    pub fn more() -> bool {
        if FIXTURE.is_none() {
            return false;
        }
        PAGES.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub fn thread(account: &str, space: &str) -> Option<Result<ChatThread, MailError>> {
        let fixture = FIXTURE.as_ref()?;
        let thread = fixture.threads.iter().find(|thread| thread.account == account && thread.space == space);
        Some(
            thread
                .cloned()
                .map(|mut thread| {
                    thread.dress();
                    thread
                })
                .ok_or_else(|| missing(space)),
        )
    }

    pub fn people() -> Option<ChatPeople> {
        let mut people = FIXTURE.as_ref()?.people.clone();
        for person in &mut people {
            person.dress();
        }
        Some(ChatPeople { can_start: true, people })
    }

    pub fn file(resource: &str) -> Option<Result<String, MailError>> {
        Some(FIXTURE.as_ref()?.files.get(resource).cloned().ok_or_else(|| missing(resource)))
    }
}

