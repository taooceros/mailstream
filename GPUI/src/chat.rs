//! Google Chat through the in-process engine's `chat_*` methods: the space list (direct messages, group
//! chats, spaces; older pages load near its end), a space's log with a composer (pictures inline,
//! other files as tiles), and starting a conversation. `ChatPane` fills the main window's content
//! area; the sidebar reads `accounts()` and `unread()` from it.
//!
//! The engine keeps the state, as it does for the native reader: it polls Google for the spaces
//! (every `chat.pollSeconds`), and while a space is open (`chat_open`) it fetches its newest
//! messages, keeps them current and marks them read. The pane reads the list again on the
//! engine's `Chat` event and the open space's history on `ChatMessages`. Engine calls run on the
//! background executor.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, Focusable, FontStyle,
    FontWeight, HighlightStyle, ImgResourceLoader, InteractiveElement, InteractiveText, IntoElement, KeyBinding,
    ListAlignment, ListState, ObjectFit, ParentElement, Render, Resource, ScrollStrategy, SharedString,
    StatefulInteractiveElement, StrikethroughStyle, Styled, StyledImage, StyledText, Subscription,
    UniformListScrollHandle, Window, actions, div, img, list, px, relative, uniform_list,
};

use crate::controls::{
    ButtonKind, ControlSize, Tone, button, centered_note, icon_button, list_row, mark_dot, monogram, notice, row_layout,
};
use crate::design::{self, AttachmentKind, Palette, chat as metrics, layout, list as rows, radius, space, stroke, text};
use crate::icons::{Icon, icon};
use crate::engine::{MailError, engine};
use crate::input;

actions!(chat, [Back, Next, Previous, Open, NewChat, Refresh]);

const CONTEXT: &str = "Chat";
/// Typing pauses this long before people are looked up.
const SEARCH_DELAY: Duration = Duration::from_millis(120);
const SUGGESTIONS: u32 = 8;
/// Log height measured beyond the visible part, so scrolling does not pop.
const OVERDRAW: f32 = 800.0;

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the list, never while a field has focus; a field's Escape closes it.
    let list = Some("Chat && !Input");
    let pane = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", Back, list),
        KeyBinding::new(secondary!("["), Back, pane),
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new(secondary!("n"), NewChat, pane),
        KeyBinding::new(secondary!("r"), Refresh, pane),
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

#[derive(Clone, PartialEq)]
struct User {
    /// `users/<id>`.
    id: SharedString,
    label: SharedString,
    avatar: mailstream::Avatar,
}

impl From<mailstream::ChatUser> for User {
    fn from(user: mailstream::ChatUser) -> Self {
        User { id: user.id.into(), label: user.name.into(), avatar: user.avatar }
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
    /// An uploaded picture shows in the log; everything else (videos too) is a tile.
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

    /// The blocks, `id` naming their links' elements.
    fn element(&self, id: &str, p: &Palette) -> Div {
        div().flex().flex_col().children(self.pieces.iter().enumerate().map(|(index, piece)| {
            let element_id = ElementId::Name(format!("chat-text:{id}:{index}").into());
            let gap = if index == 0 { 0. } else if piece.tight { 2. } else { 6. };
            let content: AnyElement = match &piece.kind {
                PieceKind::Text => piece.inline.element(element_id, p),
                PieceKind::Heading(level) => div()
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(text::READING * match level { 1 => 1.3, 2 => 1.18, 3 => 1.08, _ => 1.0 }))
                    .child(piece.inline.element(element_id, p))
                    .into_any_element(),
                PieceKind::Code => div()
                    .w_full()
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
                    .flex()
                    .flex_col()
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(p.line)
                    .overflow_hidden()
                    .children(rows.iter().enumerate().map(|(r, row)| {
                        div().flex().when(r == 0, |d| d.bg(p.chip).font_weight(FontWeight::SEMIBOLD)).when(r > 0, |d| d.border_t_1().border_color(p.line)).children(
                            row.iter().enumerate().map(|(c, cell)| {
                                let cell_id = ElementId::Name(format!("chat-text:{id}:{index}:{r}:{c}").into());
                                div().flex_1().min_w_0().px(px(10.)).py(px(4.)).when(c > 0, |d| d.border_l_1().border_color(p.line)).child(cell.element(cell_id, p))
                            }),
                        )
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

/// One opened space's loaded messages and its list state.
struct Log {
    title: Option<SharedString>,
    members: Vec<User>,
    /// Ascending by creation, as the engine holds them.
    messages: Vec<Msg>,
    /// Sent from here, not confirmed yet (or failed); drawn after `messages`.
    pending: Vec<Msg>,
    /// Each entry's place in the log (`mailstream::chat_runs`).
    runs: Vec<mailstream::ChatRun>,
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
    /// Item 0 is the top (earlier messages, or the start); items 1… are the entries.
    list: ListState,
}

impl Log {
    fn new() -> Self {
        Log {
            title: None,
            members: Vec::new(),
            messages: Vec::new(),
            pending: Vec::new(),
            runs: Vec::new(),
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

    fn ids(&self) -> Vec<SharedString> {
        self.messages.iter().chain(&self.pending).map(|m| m.id.clone()).collect()
    }

    /// Recomputes runs and tells the list which entries changed since `before` (the ids then), so
    /// only those are measured again.
    fn relayout(&mut self, before: &[SharedString]) {
        let entries = self
            .messages
            .iter()
            .chain(&self.pending)
            .map(|message| mailstream::ChatRunEntry {
                sender: message.sender_id.to_string(),
                date: message.created.map(|date| date.to_rfc3339()),
                opens_run: self.first_unread.as_ref() == Some(&message.id),
            })
            .collect();
        self.runs = mailstream::chat_runs(entries);

        let after = self.ids();
        let prefix = before.iter().zip(&after).take_while(|(a, b)| a == b).count();
        let room = before.len().min(after.len()) - prefix;
        let mut suffix = before.iter().rev().zip(after.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
        let changed = before.len() != after.len() || prefix < before.len();
        // The first entry after a change may join or leave a run: it is measured again too.
        if changed && suffix > 0 {
            suffix -= 1;
        }
        if changed {
            self.list.splice(1 + prefix..1 + before.len() - suffix, after.len() - prefix - suffix);
        }
        self.list.splice(0..1, 1);
    }

    /// Takes the engine's history in place of what was loaded.
    fn apply(&mut self, loaded: Loaded) {
        let before = self.ids();
        if self.place_new_line && !loaded.messages.is_empty() {
            self.place_new_line = false;
            self.first_unread = loaded.last_read.and_then(|read| {
                loaded.messages.iter().find(|m| !m.mine && m.created.is_some_and(|c| c > read)).map(|m| m.id.clone())
            });
        }
        self.title = loaded.title;
        self.members = loaded.members;
        self.messages = loaded.messages;
        self.more = loaded.more;
        self.loaded = loaded.loaded;
        self.failure = loaded.failure;
        self.loading_earlier = loaded.loading_earlier;
        self.relayout(&before);
    }
}

// MARK: - The pane

enum View {
    Spaces,
    Space(Key),
    NewChat,
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

pub struct ChatPane {
    focus: FocusHandle,
    composer: Entity<InputState>,
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
    logs: HashMap<Key, Log>,
    /// The thread the next message replies in, and who started it.
    reply_thread: Option<(SharedString, SharedString)>,
    hovered_message: Option<SharedString>,
    files: HashMap<FileKey, FileState>,
    /// Files to open in the system's default app once downloaded.
    open_when_ready: HashSet<FileKey>,
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
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message"));
        let people_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name or email address"));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.subscribe_in(&people_input, window, Self::on_people_event),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            composer,
            people_input,
            failure: None,
            loaded: false,
            loading: false,
            reload: false,
            chat_accounts: Vec::new(),
            accounts: Vec::new(),
            account_order: Vec::new(),
            spaces: Vec::new(),
            more: false,
            loading_more: false,
            filter: None,
            visible: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            view: View::Spaces,
            logs: HashMap::new(),
            reply_thread: None,
            hovered_message: None,
            files: HashMap::new(),
            open_when_ready: HashSet::new(),
            form: NewChatForm::default(),
            pending_serial: 0,
            _subscriptions: subscriptions,
        };
        this.load_spaces(cx);
        this
    }

    /// Shows the space list of every chat account (None) or of one.
    pub fn show(&mut self, account: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_space(cx);
        self.filter = account;
        self.view = View::Spaces;
        self.reply_thread = None;
        self.rebuild_visible();
        window.focus(&self.focus, cx);
        // Selecting Chat fetches every account's spaces now, as the native app does.
        self.refresh(&Refresh, window, cx);
        cx.notify();
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
                Ok::<_, MailError>((accounts, spaces, order, more))
            },
            |this, result, cx| {
                this.loading = false;
                if std::mem::take(&mut this.reload) {
                    this.load_spaces(cx);
                }
                match result {
                    Ok((accounts, spaces, order, more)) => {
                        this.failure = None;
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
        self.visible = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(_, space)| self.filter.as_ref().is_none_or(|a| &space.account == a))
            .map(|(ix, _)| ix)
            .collect();
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

    fn open(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        self.highlight = Some(key.clone());
        self.view = View::Space(key.clone());
        self.reply_thread = None;
        self.hovered_message = None;
        // Downloads that failed are tried again.
        self.files.retain(|_, file| !matches!(file, FileState::Failed(_)));
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        let first_open = !self.logs.contains_key(&key);
        let log = self.logs.entry(key.clone()).or_insert_with(Log::new);
        // A new "New" line each time the space is opened.
        log.first_unread = None;
        log.place_new_line = true;
        if !first_open {
            let before = log.ids();
            log.relayout(&before);
        }
        // The engine answers what it holds now and fetches the newest (`ChatMessages` follows),
        // keeps the space current and marks it read while it stays open.
        let (account, space) = (key.account.to_string(), key.space.to_string());
        self.load_thread(&key, async move { engine().chat_open(account, space).await }, cx);
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_space(cx);
        self.view = View::Spaces;
        self.reply_thread = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The open space is no longer kept current or read.
    fn leave_space(&mut self, cx: &mut Context<Self>) {
        if matches!(self.view, View::Space(_)) {
            self.background(cx, async { engine().chat_close().await.ok() }, |_, _, _| {});
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

    /// Downloads the pictures of the log's entry `ix` as it comes into view.
    fn fetch_pictures(&mut self, key: &Key, ix: usize, cx: &mut Context<Self>) {
        let Some(message) = ix.checked_sub(1).and_then(|ix| self.logs.get(key)?.entry(ix)) else { return };
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
                this.files.insert(key, file);
                cx.notify();
            },
        );
    }

    // MARK: - Sending

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let View::Space(key) = &self.view else { return };
        let key = key.clone();
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        let thread = self.reply_thread.take().map(|(thread, _)| thread);
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
            created: Some(now),
            time: mailstream::message_time(Some(now.to_rfc3339())).into(),
            thread: thread.clone(),
            thread_reply: thread.is_some(),
            mine: true,
            body: Body::new(&mailstream::chat_body(text.clone(), Msg::names(log)), false),
            attachments: Vec::new(),
            status: Status::Sending,
            draft: Some((text.clone(), thread.clone())),
        };
        let before = log.ids();
        log.pending.push(pending);
        log.relayout(&before);
        log.list.scroll_to_reveal_item(log.len());
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
                        let before = log.ids();
                        if let Some(pending) = log.pending.iter_mut().find(|m| m.id == id) {
                            pending.status = Status::Failed(error.to_string().into());
                        }
                        log.relayout(&before);
                    }
                }
                cx.notify();
            },
        );
    }

    fn retry(&mut self, key: &Key, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        let Some(at) = log.pending.iter().position(|m| &m.id == id) else { return };
        let before = log.ids();
        let failed = log.pending.remove(at);
        log.relayout(&before);
        if let Some((text, thread)) = failed.draft {
            self.deliver(key, text, thread, window, cx);
        }
    }

    fn discard(&mut self, key: &Key, id: &SharedString, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        let before = log.ids();
        log.pending.retain(|m| &m.id != id);
        log.relayout(&before);
        cx.notify();
    }

    fn on_composer_event(&mut self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let InputEvent::PressEnter { secondary: false, .. } = event {
            self.send(window, cx);
        }
    }

    /// Esc in the composer leaves the thread it replies in, else closes the space.
    fn composer_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.reply_thread.take().is_some() {
            cx.notify();
        } else {
            self.close(window, cx);
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
        self.form = NewChatForm { account, ..Default::default() };
        self.view = View::NewChat;
        self.people_input.update(cx, |input, cx| input.clean(window, cx));
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
        if !matches!(self.view, View::Spaces) {
            self.close(window, cx);
        }
    }

    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        match self.view {
            View::Spaces => cx.propagate(),
            View::Space(_) | View::NewChat => self.close(window, cx),
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !matches!(self.view, View::Spaces) || self.visible.is_empty() {
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
        self.highlight = Some(self.spaces[self.visible[next]].key());
        self.scroll.scroll_to_item(next, ScrollStrategy::Center);
        cx.notify();
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn open_highlighted(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.highlight.clone() else { return };
        if matches!(self.view, View::Spaces) && self.space(&key).is_some() {
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

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let body = match &self.view {
            View::Spaces => self.render_spaces(p, cx),
            View::Space(key) => {
                let key = key.clone();
                self.render_space(&key, p, cx)
            }
            View::NewChat => self.render_form(p, cx),
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open_highlighted))
            .on_action(cx.listener(Self::new_chat))
            .on_action(cx.listener(Self::refresh))
            .size_full()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .bg(p.background)
            .text_color(p.text)
            .child(body)
    }
}

impl ChatPane {
    fn header(&self, p: &'static Palette) -> Div {
        div()
            .flex_shrink_0()
            .h(px(metrics::HEADER_HEIGHT))
            .px(px(rows::INSET + rows::SECTION_X))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.line)
    }

    fn render_spaces(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let title = self.filter.as_ref().map_or_else(|| SharedString::from("Chat"), |a| self.account_label(a));
        let header = self
            .header(p)
            .child(div().flex_1().min_w_0().truncate().text_size(px(text::PANE_TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
            .when(!self.accounts.is_empty(), |d| {
                d.child(button("chat-new", "New Chat", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_form(window, cx))))
            });
        let content: AnyElement = if let Some(failure) = &self.failure {
            centered_note(failure.clone(), p).into_any_element()
        } else if !self.loaded {
            centered_note("Loading chats…", p).into_any_element()
        } else if self.accounts.is_empty() {
            centered_note("No Google Chat accounts. Sign in to a Google account with Chat access in Mailstream's settings.", p)
                .into_any_element()
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
                    range.map(|position| this.render_row(position, p, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .px(px(rows::INSET))
            .py(px(6.))
            .into_any_element()
        };
        div().size_full().flex().flex_col().child(header).child(content).into_any_element()
    }

    /// The `position`th space in view.
    fn render_row(&self, position: usize, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let space = &self.spaces[self.visible[position]];
        let key = space.key();
        let selected = self.highlight.as_ref() == Some(&key);
        let unread = space.unread;
        let next = self.visible.get(position + 1).map(|&next| self.spaces[next].key());
        let rule = next.is_some_and(|next| self.highlight.as_ref() != Some(&next));
        let title = div()
            .min_w_0()
            .truncate()
            .font_weight(if unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
            .child(space.title.clone());
        let meta = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(text::FOOTNOTE))
            .text_color(if unread { p.accent } else { p.muted })
            .when_some(self.account_color(&space.account), |d, color| d.child(mark_dot(color)))
            .child(space.when.clone());
        let subtitle = div().min_w_0().truncate().text_color(p.muted).child(space.snippet.clone());
        list_row(ElementId::Name(format!("chat-row:{}:{}", space.account, space.id).into()), selected, rule, p)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open(key.clone(), window, cx)))
            .child(row_layout(
                unread,
                monogram(&space.avatar, rows::AVATAR, p),
                title,
                meta,
                subtitle,
                Vec::new(),
                p,
            ))
            .into_any_element()
    }

    fn render_space(&self, key: &Key, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let space = self.space(key);
        let log = self.logs.get(key);
        let title = space
            .map(|s| s.title.clone())
            .or_else(|| log.and_then(|l| l.title.clone()))
            .unwrap_or_else(|| key.space.clone());
        let mut meta = vec![SharedString::from(space.map_or("Space", Space::kind_title))];
        if self.accounts.len() > 1 {
            meta.push(self.account_label(&key.account));
        }
        let members = log.map_or(0, |l| l.members.len());
        if space.is_some_and(|s| s.kind == mailstream::ChatSpaceKind::Group) && members > 0 {
            meta.push(format!("{} people", members + 1).into());
        }
        let header = self
            .header(p)
            .child(icon_button("chat-back", Icon::Back, "Back", true, p.glyph).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(text::PANE_TITLE)).font_weight(FontWeight::SEMIBOLD).child(title.clone()))
                    .child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(meta.join(" · "))),
            );
        let log: AnyElement = match self.logs.get(key) {
            Some(log) => {
                let list_key = key.clone();
                list(
                    log.list.clone(),
                    cx.processor(move |this, ix: usize, window, cx| {
                        this.fetch_pictures(&list_key, ix, cx);
                        this.render_entry(&list_key, ix, window, p, cx)
                    }),
                )
                    .flex_1()
                    .w_full()
                    .into_any_element()
            }
            None => div().flex_1().into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(log)
            .child(self.render_composer(&title, p, cx))
            .into_any_element()
    }

    /// A column as wide as the reader's text, centered.
    fn column(child: impl IntoElement) -> Div {
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(div().w_full().max_w(px(layout::READING_WIDTH)).px(px(32.)).child(child))
    }

    fn render_entry(&self, key: &Key, ix: usize, window: &mut Window, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(log) = self.logs.get(key) else { return div().into_any_element() };
        if ix == 0 {
            return self.render_top(key, log, p, cx);
        }
        let (Some(message), Some(run)) = (log.entry(ix - 1), log.runs.get(ix - 1)) else {
            return div().into_any_element();
        };
        let new_line = log.first_unread.as_ref() == Some(&message.id);
        let continued = run.continued;
        let hovered = self.hovered_message.as_ref() == Some(&message.id);
        let hover_id = message.id.clone();
        let time_color = if hovered { p.faint } else { gpui::transparent_black() };

        let gutter = div()
            .flex_shrink_0()
            .w(px(metrics::GUTTER))
            .flex()
            .justify_center()
            .when(!continued, |d| d.child(monogram(&message.avatar, metrics::AVATAR, p)))
            .when(continued, |d| {
                d.child(div().text_size(px(text::TAG)).line_height(px(22.)).text_color(time_color).child(message.time.clone()))
            });

        let status: Option<SharedString> = match &message.status {
            Status::Sent => None,
            Status::Sending => Some("Sending…".into()),
            Status::Failed(_) => Some("Not sent".into()),
        };
        let head = (!continued).then(|| {
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .min_w_0()
                .child(
                    div()
                        .max_w(relative(0.6))
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(message.mine, |d| d.text_color(p.accent))
                        .child(message.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(text::TAG))
                        .text_color(p.faint)
                        .child(status.clone().unwrap_or_else(|| message.time.clone())),
                )
        });

        let main = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .children(head)
            .when(message.thread_reply, |d| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child("↳ Reply in thread"))
            })
            .when(!message.body.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(text::READING))
                        .line_height(relative(metrics::LINE))
                        .when(message.thread_reply, |d| d.pl(px(10.)))
                        .when(matches!(message.status, Status::Failed(_)), |d| d.text_color(p.muted))
                        .child(message.body.element(&message.id, p)),
                )
            })
            .when(!message.attachments.is_empty(), |d| {
                d.child(div().flex().flex_wrap().items_start().gap(px(space::S8)).pt(px(space::S6)).children(
                    message.attachments.iter().enumerate().map(|(index, attachment)| {
                        let id = ElementId::Name(format!("chat-file:{}:{index}", message.id).into());
                        self.render_attachment(&key.account, id, attachment, window, p, cx)
                    }),
                ))
            })
            .when_some(
                match &message.status {
                    Status::Failed(reason) => Some(reason.clone()),
                    _ => None,
                },
                |d, reason| {
                    let (retry_key, retry_id) = (key.clone(), message.id.clone());
                    let (discard_key, discard_id) = (key.clone(), message.id.clone());
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(4.))
                            .text_size(px(text::FOOTNOTE))
                            .child(div().text_color(p.danger).child(format!("Not sent: {reason}")))
                            .child(button(ElementId::Name(format!("retry:{}", message.id).into()), "Retry", ButtonKind::Secondary, ControlSize::Small, true, p).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| this.retry(&retry_key, &retry_id, window, cx)),
                            ))
                            .child(button(ElementId::Name(format!("discard:{}", message.id).into()), "Delete", ButtonKind::Plain(p.accent), ControlSize::Small, true, p).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.discard(&discard_key, &discard_id, cx)),
                            )),
                    )
                },
            );

        let reply = message.thread.clone().filter(|_| hovered && matches!(message.status, Status::Sent)).map(|thread| {
            let starter = log.messages.iter().find(|m| m.thread.as_ref() == Some(&thread) && !m.thread_reply);
            let who: SharedString = match starter {
                Some(m) if m.mine => "your message".into(),
                Some(m) => m.name.clone(),
                None => message.name.clone(),
            };
            div()
                .absolute()
                .top(px(-10.))
                .right(px(10.))
                .rounded(px(radius::CONTROL))
                .border_1()
                .border_color(p.line)
                .bg(p.background)
                .child(button(ElementId::Name(format!("reply:{}", message.id).into()), "Reply in Thread", ButtonKind::Plain(p.accent), ControlSize::Small, true, p).on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.reply_thread = Some((thread.clone(), who.clone()));
                        window.focus(&this.composer.focus_handle(cx), cx);
                        cx.notify();
                    }),
                ))
        });

        let row = div()
            .id(ElementId::Name(format!("chat-msg:{}", message.id).into()))
            .relative()
            .flex()
            .gap(px(metrics::GAP))
            .mx(px(-10.))
            .px(px(10.))
            .pt(px(if continued { 2. } else { 8. }))
            .pb(px(6.))
            .rounded(px(radius::CARD))
            .hover(|s| s.bg(p.hover))
            .when(matches!(message.status, Status::Sending), |d| d.opacity(metrics::PENDING_OPACITY))
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered_message = Some(hover_id.clone());
                } else if this.hovered_message.as_ref() == Some(&hover_id) {
                    this.hovered_message = None;
                }
                cx.notify();
            }))
            .child(gutter)
            .child(main)
            .children(reply);

        Self::column(
            div()
                .flex()
                .flex_col()
                .when(!run.divider.is_empty(), |d| d.child(divider(run.divider.clone().into(), p.muted, p.line)))
                .when(new_line, |d| d.child(divider("New".into(), p.accent, p.accent.opacity(0.45))))
                .child(row),
        )
        .into_any_element()
    }

    fn render_top(&self, key: &Key, log: &Log, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let content: AnyElement = if let Some(failure) = &log.failure {
            div().py(px(24.)).text_color(p.danger).child(format!("Could not load messages: {failure}")).into_any_element()
        } else if !log.loaded {
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
        } else if log.len() == 0 {
            div().py(px(24.)).text_color(p.muted).child("No messages yet.").into_any_element()
        } else {
            div().pt(px(24.)).into_any_element()
        };
        Self::column(content).into_any_element()
    }

    /// An attachment in the log: an uploaded picture as itself once downloaded (Flutter's
    /// `_ChatImage`); anything else, or a picture that can't be shown, as a tile.
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

    fn render_composer(&self, title: &SharedString, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.reply_thread.as_ref().map(|(_, who)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pb(px(6.))
                .text_size(px(text::FOOTNOTE))
                .text_color(p.muted)
                .child(format!("↳ Replying in thread to {who}"))
                .child(icon_button("chat-unthread", Icon::Close, "Reply in the space", true, p.glyph).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.reply_thread = None;
                    cx.notify();
                })))
        });
        let composer = self.composer.clone();
        Self::column(
            div()
                .pt(px(8.))
                .pb(px(16.))
                .children(thread)
                .child(
                    div()
                        .id("chat-composer")
                        .h(px(metrics::COMPOSER_HEIGHT))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(radius::CARD))
                        .bg(p.chip)
                        .text_size(px(text::READING))
                        .cursor_text()
                        .on_click(move |_, window, cx| window.focus(&composer.focus_handle(cx), cx))
                        .on_action(cx.listener(Self::composer_escape))
                        .child(div().flex_1().min_w_0().child(input::field(&self.composer)))
                        .child(div().flex_shrink_0().text_size(px(text::CAPTION)).text_color(p.faint).child("↩")),
                )
                .child(
                    div()
                        .pt(px(4.))
                        .text_size(px(text::CAPTION))
                        .text_color(p.faint)
                        .child(format!("Message {title} · Return sends · Esc goes back")),
                ),
        )
        .into_any_element()
    }

    fn render_form(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
                "This account can only reopen direct messages it has; sign in again to start new chats.".into(),
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
                .child(monogram(&mailstream::avatar(person.label().to_string(), person.email.to_string()), rows::AVATAR, p))
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
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                self.header(p)
                    .child(icon_button("chat-form-back", Icon::Back, "Back", true, p.glyph).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
                    .child(div().text_size(px(text::PANE_TITLE)).font_weight(FontWeight::SEMIBOLD).child("New Chat")),
            )
            .child(
                div().flex_1().flex().justify_center().pt(px(32.)).child(
                    div()
                        .w_full()
                        .max_w(px(metrics::FORM_WIDTH))
                        .px(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
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
            .into_any_element()
    }
}

// MARK: - Pieces

/// A line across the log with a label in it.
fn divider(label: SharedString, color: gpui::Hsla, line: gpui::Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .mt(px(16.))
        .mb(px(6.))
        .text_size(px(text::CAPTION))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(div().flex_1().h(px(1.)).bg(line))
        .child(label)
        .child(div().flex_1().h(px(1.)).bg(line))
}

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

