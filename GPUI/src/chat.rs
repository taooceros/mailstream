//! Google Chat through the in-process engine's `chat_*` methods: the space list (direct messages, group
//! chats, spaces), a space's log with a composer, and starting a conversation. `ChatPane` fills
//! the main window's content area; the sidebar reads `accounts()` and `unread()` from it.
//!
//! The engine keeps the state, as it does for the native reader: it polls Google for the spaces
//! (every `chat.pollSeconds`), and while a space is open (`chat_open`) it fetches its newest
//! messages, keeps them current and marks them read. The pane reads the list again on the
//! engine's `Chat` event and the open space's history on `ChatMessages`. Engine calls run on the
//! background executor.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, Focusable, FontStyle,
    FontWeight, HighlightStyle, InteractiveElement, InteractiveText, IntoElement, KeyBinding, ListAlignment, ListState,
    MouseButton, ParentElement, Render, ScrollStrategy, SharedString, Stateful, StatefulInteractiveElement,
    StrikethroughStyle, Styled, StyledText, Subscription, UniformListScrollHandle, Window, actions, div, list, px,
    relative, uniform_list,
};

use crate::design::{self, Palette, UI_FONT, chat as metrics, layout, list as rows, radius, text};
use crate::engine::{MailError, engine};
use crate::format;
use crate::input::{InputEvent, TextInput};

actions!(chat, [Back, Next, Previous, Open, NewChat, Refresh]);

const CONTEXT: &str = "Chat";
/// Typing pauses this long before people are looked up.
const SEARCH_DELAY: Duration = Duration::from_millis(120);
const SUGGESTIONS: u32 = 8;
/// Log height measured beyond the visible part, so scrolling does not pop.
const OVERDRAW: f32 = 800.0;

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the list, never while a field has focus; a field's Escape is its Cancel.
    let list = Some("Chat && !TextInput");
    let pane = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", Back, list),
        KeyBinding::new("cmd-[", Back, pane),
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("cmd-n", NewChat, pane),
        KeyBinding::new("cmd-r", Refresh, pane),
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
    kind: SharedString,
    title: SharedString,
    snippet: SharedString,
    unread: bool,
    last_active: Option<DateTime<Utc>>,
    avatar: SpaceAvatar,
    /// The row's date, derived off the main thread.
    when: SharedString,
}

impl Space {
    /// Converts off the main thread, deriving the row's date against `now`.
    fn new(space: mailstream::ChatSpace, now: DateTime<Local>) -> Self {
        let last_active = date(space.activity);
        Space {
            account: space.account.into(),
            id: space.space.into(),
            kind: space.kind.unwrap_or_default().into(),
            title: space.title.into(),
            snippet: space.preview.into(),
            unread: space.unread,
            last_active,
            avatar: SpaceAvatar { name: space.avatar_name.into(), color_key: space.avatar_color_key.into() },
            when: format::list_date(last_active, now).into(),
        }
    }

    fn key(&self) -> Key {
        Key { account: self.account.clone(), space: self.id.clone() }
    }

    fn is(&self, key: &Key) -> bool {
        self.account == key.account && self.id == key.space
    }

    fn kind_title(&self) -> &'static str {
        match self.kind.as_ref() {
            "dm" => "Direct message",
            "group" => "Group chat",
            "bot" => "App",
            _ => "Space",
        }
    }
}

#[derive(Clone, PartialEq)]
struct SpaceAvatar {
    name: SharedString,
    color_key: SharedString,
}

#[derive(Clone, PartialEq)]
struct User {
    /// `users/<id>`.
    id: SharedString,
    email: Option<SharedString>,
    label: SharedString,
}

impl From<mailstream::ChatUser> for User {
    fn from(user: mailstream::ChatUser) -> Self {
        User { id: user.id.into(), email: user.email.map(Into::into), label: user.name.into() }
    }
}

#[derive(Clone)]
struct Attachment {
    name: SharedString,
    url: Option<SharedString>,
}

impl From<mailstream::ChatAttachment> for Attachment {
    fn from(attachment: mailstream::ChatAttachment) -> Self {
        Attachment { name: attachment.name.into(), url: attachment.url.map(Into::into) }
    }
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
}

// MARK: - Message text

/// How a stretch of a message's text is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    Mention,
    Faint,
}

/// A message's text with Google Chat markup (`*bold*`, `_italic_`, `~strike~`, `` `code` ``,
/// fences, `<url|label>`, `<users/…>` mentions) turned into styled ranges; bare URLs become links.
#[derive(Clone, Default, Debug)]
struct Body {
    text: SharedString,
    /// Sorted, never overlapping.
    marks: Vec<(Range<usize>, Mark)>,
    /// The targets of the `Link` marks, in order.
    links: Vec<SharedString>,
}

impl Body {
    fn parse(text: &str, names: &HashMap<String, SharedString>, edited: bool) -> Self {
        let mut builder = BodyBuilder { out: String::with_capacity(text.len()), marks: Vec::new(), links: Vec::new(), names };
        let mut rest = text;
        while let Some(start) = rest.find("```") {
            let after = &rest[start + 3..];
            let Some(end) = after.find("```") else { break };
            builder.tokens(&rest[..start]);
            builder.push(after[..end].trim_matches('\n'), Some(Mark::Code));
            rest = &after[end + 3..];
        }
        builder.tokens(rest);
        if edited {
            builder.push(" (edited)", Some(Mark::Faint));
        }
        Body { text: builder.out.into(), marks: builder.marks, links: builder.links }
    }

    fn note(text: &str) -> Self {
        Body { text: SharedString::from(text.to_owned()), marks: vec![(0..text.len(), Mark::Italic)], links: Vec::new() }
    }

    fn link_ranges(&self) -> Vec<Range<usize>> {
        self.marks.iter().filter(|(_, mark)| *mark == Mark::Link).map(|(range, _)| range.clone()).collect()
    }
}

struct BodyBuilder<'a> {
    out: String,
    marks: Vec<(Range<usize>, Mark)>,
    links: Vec<SharedString>,
    names: &'a HashMap<String, SharedString>,
}

/// Punctuation that ends a sentence rather than a bare URL.
const URL_TRAILING: [char; 10] = ['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"'];

impl BodyBuilder<'_> {
    fn push(&mut self, s: &str, mark: Option<Mark>) {
        if s.is_empty() {
            return;
        }
        let start = self.out.len();
        self.out.push_str(s);
        if let Some(mark) = mark {
            self.marks.push((start..self.out.len(), mark));
        }
    }

    fn link(&mut self, label: &str, url: &str) {
        self.push(label, Some(Mark::Link));
        self.links.push(SharedString::from(url.to_owned()));
    }

    /// `<url|label>` links and `<users/…>` mentions; the text around them goes to `inline`.
    fn tokens(&mut self, mut rest: &str) {
        while let Some(open) = rest.find('<') {
            let after = &rest[open + 1..];
            let token = after
                .find(['>', '\n'])
                .filter(|&close| after[close..].starts_with('>'))
                .map(|close| (&after[..close], &after[close + 1..]));
            match token {
                Some((inner, tail))
                    if (inner.starts_with("http://") || inner.starts_with("https://"))
                        && !inner.split('|').next().unwrap_or_default().contains(char::is_whitespace) =>
                {
                    let (url, label) = inner.split_once('|').unwrap_or((inner, ""));
                    self.inline(&rest[..open]);
                    self.link(if label.is_empty() { url } else { label }, url);
                    rest = tail;
                }
                Some((inner, tail)) if inner.starts_with("users/") && !inner.contains(char::is_whitespace) => {
                    self.inline(&rest[..open]);
                    let name = self.names.get(inner).map_or("someone", |name| name.as_ref());
                    let mention = format!("@{name}");
                    self.push(&mention, Some(Mark::Mention));
                    rest = tail;
                }
                _ => {
                    self.inline(&rest[..=open]);
                    rest = after;
                }
            }
        }
        self.inline(rest);
    }

    /// Bare URLs become links; the rest goes to `emphasis`.
    fn inline(&mut self, mut rest: &str) {
        while let Some(start) = find_url(rest) {
            let tail = &rest[start..];
            let scheme = tail.find("//").map_or(0, |i| i + 2);
            let mut end = tail.find(char::is_whitespace).unwrap_or(tail.len());
            while end > scheme && tail[..end].ends_with(URL_TRAILING) {
                end -= 1;
            }
            if end <= scheme {
                self.emphasis(&rest[..start + scheme]);
                rest = &rest[start + scheme..];
                continue;
            }
            self.emphasis(&rest[..start]);
            self.link(&tail[..end], &tail[..end]);
            rest = &tail[end..];
        }
        self.emphasis(rest);
    }

    fn emphasis(&mut self, piece: &str) {
        let bytes = piece.as_bytes();
        let (mut plain, mut i) = (0, 0);
        while i < bytes.len() {
            // Google escapes markup characters it means literally (`thanks\!`): drop the
            // backslash, keep the character as text.
            if bytes[i] == b'\\' && bytes.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                self.push(&piece[plain..i], None);
                plain = i + 1;
                i += 2;
                continue;
            }
            let mark = match bytes[i] {
                b'*' => Mark::Bold,
                b'_' => Mark::Italic,
                b'~' => Mark::Strike,
                b'`' => Mark::Code,
                _ => {
                    i += 1;
                    continue;
                }
            };
            match closing(piece, i) {
                Some(close) => {
                    self.push(&piece[plain..i], None);
                    self.push(&piece[i + 1..close], Some(mark));
                    i = close + 1;
                    plain = i;
                }
                None => i += 1,
            }
        }
        self.push(&piece[plain..], None);
    }
}

/// The first `http://` or `https://` that starts a word.
fn find_url(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find("http").map(|i| i + from) {
        let rest = &s[i..];
        let starts_word = s[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        if starts_word && (rest.starts_with("http://") || rest.starts_with("https://")) {
            return Some(i);
        }
        from = i + 4;
    }
    None
}

/// Where the emphasis marker at `open` closes, by Google Chat's rules (`ChatMarkup`): the next
/// same marker on the line, with text that starts and ends with non-space, not inside a word.
fn closing(s: &str, open: usize) -> Option<usize> {
    let marker = char::from(s.as_bytes()[open]);
    let start = open + 1;
    let close = s[start..].find(marker)? + start;
    let content = &s[start..close];
    if content.is_empty() || content.contains('\n') {
        return None;
    }
    if marker == '`' {
        return Some(close);
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let before = s[..open].chars().next_back();
    let after = s[close + 1..].chars().next();
    let first = content.chars().next()?;
    let last = content.chars().next_back()?;
    let fits = before.is_none_or(|c| !word(c) && c != marker)
        && !first.is_whitespace()
        && !last.is_whitespace()
        && after.is_none_or(|c| !word(c) && c != marker);
    fits.then_some(close)
}

fn mark_style(mark: Mark, p: &Palette) -> HighlightStyle {
    match mark {
        Mark::Bold => HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() },
        Mark::Italic => HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() },
        Mark::Strike => {
            HighlightStyle { strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: None }), ..Default::default() }
        }
        Mark::Code => HighlightStyle { background_color: Some(p.chip), ..Default::default() },
        Mark::Link => HighlightStyle { color: Some(p.accent), ..Default::default() },
        Mark::Mention => HighlightStyle { color: Some(p.accent), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() },
        Mark::Faint => HighlightStyle { color: Some(p.faint), ..Default::default() },
    }
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
    initial: SharedString,
    color_key: SharedString,
    created: Option<DateTime<Utc>>,
    day: Option<NaiveDate>,
    day_label: SharedString,
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
    fn from_wire(message: mailstream::ChatMessage, names: &HashMap<String, SharedString>, now: DateTime<Local>) -> Self {
        let created = date(message.created);
        let local = created.map(|date| date.with_timezone(&Local));
        let sender = User::from(message.sender);
        let body = if message.card && message.markup.is_empty() {
            Body::note("Card message")
        } else {
            Body::parse(&message.markup, names, message.edited.is_some())
        };
        Msg {
            name: if message.mine { "You".into() } else { sender.label.clone() },
            initial: initial(&sender.label),
            color_key: sender.email.clone().unwrap_or_else(|| sender.id.clone()),
            sender_id: sender.id,
            id: message.id.into(),
            created,
            day: local.map(|date| date.date_naive()),
            day_label: local.map(|date| day_label(date, now)).unwrap_or_default().into(),
            time: local.map(|date| date.format("%-I:%M %p").to_string()).unwrap_or_default().into(),
            thread: message.thread.map(Into::into),
            thread_reply: message.thread_reply,
            mine: message.mine,
            body,
            attachments: message.attachments.into_iter().map(Attachment::from).collect(),
            status: Status::Sent,
            draft: None,
        }
    }

    fn convert(messages: Vec<mailstream::ChatMessage>, members: &[User]) -> Vec<Msg> {
        let mut names: HashMap<String, SharedString> =
            members.iter().map(|user| (user.id.to_string(), user.label.clone())).collect();
        for message in &messages {
            names.entry(message.sender.id.clone()).or_insert_with(|| message.sender.name.clone().into());
        }
        let now = Local::now();
        messages.into_iter().map(|message| Msg::from_wire(message, &names, now)).collect()
    }
}

/// Where a message stands in the log: a day divider above it, or part of the run above.
struct Run {
    divider: Option<SharedString>,
    continued: bool,
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
            messages: Msg::convert(thread.messages, &members),
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
    runs: Vec<Run>,
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
        let mut runs = Vec::with_capacity(self.len());
        let mut previous: Option<&Msg> = None;
        for message in self.messages.iter().chain(&self.pending) {
            let same_day = message.day.is_some() && previous.and_then(|p| p.day) == message.day;
            let close = match (previous.and_then(|p| p.created), message.created) {
                (Some(last), Some(now)) => (now - last).num_seconds() < metrics::RUN_SECONDS,
                _ => false,
            };
            let continued = same_day
                && close
                && previous.is_some_and(|p| p.sender_id == message.sender_id)
                && self.first_unread.as_ref() != Some(&message.id);
            let divider = (!same_day && !message.day_label.is_empty()).then(|| message.day_label.clone());
            runs.push(Run { divider, continued });
            previous = Some(message);
        }
        self.runs = runs;

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
    suggestions: Vec<Person>,
    /// Bumped per keystroke; a lookup answering for an older one is dropped.
    generation: u64,
    starting: bool,
    failure: Option<SharedString>,
}

pub struct ChatPane {
    focus: FocusHandle,
    composer: Entity<TextInput>,
    people_input: Entity<TextInput>,
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
        let composer = cx.new(|cx| TextInput::new("Message", cx));
        let people_input = cx.new(|cx| TextInput::new("Name or email address", cx));
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
            filter: None,
            visible: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            view: View::Spaces,
            logs: HashMap::new(),
            reply_thread: None,
            hovered_message: None,
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
        window.focus(&self.focus);
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
                let answer = engine().chat_spaces(None).await?;
                let accounts: Vec<Account> = answer.accounts.into_iter().map(Account::from).collect();
                let now = Local::now();
                let spaces: Vec<Space> = answer.spaces.into_iter().map(|space| Space::new(space, now)).collect();
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
                Ok::<_, MailError>((accounts, spaces, order))
            },
            |this, result, cx| {
                this.loading = false;
                if std::mem::take(&mut this.reload) {
                    this.load_spaces(cx);
                }
                match result {
                    Ok((accounts, spaces, order)) => {
                        this.failure = None;
                        this.loaded = true;
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
        self.composer.update(cx, |input, cx| input.clear(cx));
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
        window.focus(&self.composer.focus_handle(cx));
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_space(cx);
        self.view = View::Spaces;
        self.reply_thread = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// The open space is no longer kept current or read.
    fn leave_space(&mut self, cx: &mut Context<Self>) {
        if matches!(self.view, View::Space(_)) {
            self.background(cx, async { engine().chat_close().await.ok() }, |_, _, _| {});
        }
    }

    /// Asks the engine for `key`'s history with `request` and shows the answer, unless a newer
    /// request's answer came first.
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
        self.background(
            cx,
            async move { request.await.map(Loaded::new) },
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

    // MARK: - Sending

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let View::Space(key) = &self.view else { return };
        let key = key.clone();
        let text = self.composer.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |input, cx| input.clear(cx));
        let thread = self.reply_thread.take().map(|(thread, _)| thread);
        self.deliver(&key, text, thread, window, cx);
    }

    /// Shows `text` as pending at once and sends it; the confirmed message takes its place.
    fn deliver(&mut self, key: &Key, text: String, thread: Option<SharedString>, _: &mut Window, cx: &mut Context<Self>) {
        let Some(log) = self.logs.get_mut(key) else { return };
        self.pending_serial += 1;
        let id: SharedString = format!("pending-{}", self.pending_serial).into();
        let now = Local::now();
        let me = log.messages.iter().rev().find(|m| m.mine);
        let pending = Msg {
            id: id.clone(),
            sender_id: me.map_or_else(|| "me".into(), |m| m.sender_id.clone()),
            name: "You".into(),
            initial: me.map_or_else(|| initial(&key.account), |m| m.initial.clone()),
            color_key: me.map_or_else(|| key.account.clone(), |m| m.color_key.clone()),
            created: Some(now.with_timezone(&Utc)),
            day: Some(now.date_naive()),
            day_label: "Today".into(),
            time: now.format("%-I:%M %p").to_string().into(),
            thread: thread.clone(),
            thread_reply: thread.is_some(),
            mine: true,
            body: Body::parse(&text, &HashMap::new(), false),
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

    fn on_composer_event(&mut self, _: &Entity<TextInput>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Submit => self.send(window, cx),
            InputEvent::Cancel if self.reply_thread.is_some() => {
                self.reply_thread = None;
                cx.notify();
            }
            InputEvent::Cancel => self.close(window, cx),
            InputEvent::Changed => {}
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
        self.people_input.update(cx, |input, cx| input.clear(cx));
        window.focus(&self.people_input.focus_handle(cx));
        cx.notify();
    }

    fn on_people_event(&mut self, input: &Entity<TextInput>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Changed => {
                self.form.generation += 1;
                self.form.failure = None;
                let query = input.read(cx).text().trim().to_owned();
                if query.is_empty() {
                    self.form.suggestions.clear();
                    cx.notify();
                    return;
                }
                self.search_people(query, self.form.generation, cx);
            }
            InputEvent::Submit => {
                let typed = input.read(cx).text().trim().to_owned();
                if typed.is_empty() {
                    self.start_chat(window, cx);
                } else if typed.contains('@') && !typed.contains(char::is_whitespace) {
                    self.add_person(Person { name: None, email: typed.into() }, window, cx);
                } else if let Some(first) = self.form.suggestions.first().cloned() {
                    self.add_person(first, window, cx);
                }
            }
            InputEvent::Cancel => self.close(window, cx),
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
                let chosen: HashSet<String> = this.form.people.iter().map(|p| p.email.to_lowercase()).collect();
                this.form.suggestions = found
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| !chosen.contains(&p.email.to_lowercase()))
                    .map(Person::from)
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_person(&mut self, person: Person, window: &mut Window, cx: &mut Context<Self>) {
        if !self.form.people.iter().any(|p| p.email.eq_ignore_ascii_case(&person.email)) {
            self.form.people.push(person);
        }
        self.form.suggestions.clear();
        self.form.generation += 1;
        self.people_input.update(cx, |input, cx| input.clear(cx));
        window.focus(&self.people_input.focus_handle(cx));
        cx.notify();
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
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .bg(p.page)
            .text_color(p.text)
            .child(body)
    }
}

impl ChatPane {
    fn header(&self, p: &'static Palette) -> Div {
        div()
            .flex_shrink_0()
            .h(px(metrics::HEADER_HEIGHT))
            .px(px(rows::INSET + rows::SENDER_X - rows::DOT_X))
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
            .child(div().flex_1().min_w_0().truncate().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child(title))
            .when(!self.accounts.is_empty(), |d| {
                d.child(button("chat-new", "New Chat", p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_form(window, cx))))
            });
        let content: AnyElement = if let Some(failure) = &self.failure {
            note(failure.clone(), p).into_any_element()
        } else if !self.loaded {
            note("Loading chats…", p).into_any_element()
        } else if self.accounts.is_empty() {
            note("No Google Chat accounts. Sign in to a Google account with Chat access in Mailstream's settings.", p)
                .into_any_element()
        } else if self.visible.is_empty() {
            note("No conversations yet.", p)
                .child(button("chat-new-empty", "New Chat", p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_form(window, cx))))
                .into_any_element()
        } else {
            uniform_list(
                "chat-spaces",
                self.visible.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    range.map(|ix| this.render_row(this.visible[ix], p, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .flex_1()
            .px(px(rows::INSET))
            .py(px(6.))
            .into_any_element()
        };
        div().size_full().flex().flex_col().child(header).child(content).into_any_element()
    }

    fn render_row(&self, ix: usize, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let space = &self.spaces[ix];
        let key = space.key();
        let highlighted = self.highlight.as_ref() == Some(&key);
        let unread = space.unread;
        let weight = if unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL };
        div()
            .id(ElementId::Name(format!("chat-row:{}:{}", space.account, space.id).into()))
            .relative()
            .h(px(rows::ROW_HEIGHT))
            .w_full()
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .when(highlighted, |d| d.bg(p.selection))
            .when(!highlighted, |d| d.hover(|s| s.bg(p.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open(key.clone(), window, cx)))
            .when(unread, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(rows::DOT_X - 3.))
                        .top(px(rows::ROW_HEIGHT / 2. - 3.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(p.accent),
                )
            })
            .child(
                avatar(&space.avatar.name, &space.avatar.color_key, rows::AVATAR, p)
                    .absolute()
                    .left(px(rows::SENDER_X))
                    .top(px((rows::ROW_HEIGHT - rows::AVATAR) / 2.)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(rows::NAME_X))
                    .w(px(rows::SENDER_X + rows::SENDER_WIDTH - rows::NAME_X))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(div().min_w_0().truncate().font_weight(weight).child(space.title.clone())),
            )
            .child(
                div()
                    .absolute()
                    .left(px(rows::SUBJECT_X))
                    .right(px(rows::TRAILING_INSET + rows::DATE_WIDTH + 8.))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(space.snippet.clone())),
            )
            .child(
                div()
                    .absolute()
                    .right(px(rows::TRAILING_INSET))
                    .w(px(rows::DATE_WIDTH + 30.))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(6.))
                    .text_size(px(text::CAPTION))
                    .text_color(if unread { p.accent } else { p.muted })
                    .when_some(self.account_color(&space.account), |d, color| {
                        d.child(div().size(px(metrics::ACCOUNT_DOT)).rounded_full().bg(color))
                    })
                    .child(space.when.clone()),
            )
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
        if space.is_some_and(|s| s.kind.as_ref() == "group") && members > 0 {
            meta.push(format!("{} people", members + 1).into());
        }
        let header = self
            .header(p)
            .child(button("chat-back", "‹", p).text_size(px(text::HEADLINE)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child(title.clone()))
                    .child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(meta.join(" · "))),
            );
        let log: AnyElement = match self.logs.get(key) {
            Some(log) => {
                let list_key = key.clone();
                list(log.list.clone(), cx.processor(move |this, ix: usize, _, cx| this.render_entry(&list_key, ix, p, cx)))
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

    fn render_entry(&self, key: &Key, ix: usize, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
            .when(!continued, |d| d.child(avatar(&message.initial, &message.color_key, metrics::AVATAR, p)))
            .when(continued, |d| {
                d.child(div().text_size(px(text::MICRO)).line_height(px(22.)).text_color(time_color).child(message.time.clone()))
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
                        .text_size(px(text::MICRO))
                        .text_color(p.faint)
                        .child(status.clone().unwrap_or_else(|| message.time.clone())),
                )
        });

        let body_text = StyledText::new(message.body.text.clone())
            .with_highlights(message.body.marks.iter().map(|(range, mark)| (range.clone(), mark_style(*mark, p))));
        let links = message.body.link_ranges();
        let body: AnyElement = if links.is_empty() {
            body_text.into_any_element()
        } else {
            let urls = message.body.links.clone();
            InteractiveText::new(ElementId::Name(format!("chat-text:{}", message.id).into()), body_text)
                .on_click(links, move |index, _, cx| {
                    if let Some(url) = urls.get(index) {
                        cx.open_url(url);
                    }
                })
                .into_any_element()
        };

        let main = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .children(head)
            .when(message.thread_reply, |d| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child("↳ Reply in thread"))
            })
            .when(!message.body.text.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(text::READING))
                        .line_height(relative(metrics::LINE))
                        .when(message.thread_reply, |d| d.pl(px(10.)))
                        .when(matches!(message.status, Status::Failed(_)), |d| d.text_color(p.muted))
                        .child(body),
                )
            })
            .when(!message.attachments.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(6.)).pt(px(6.)).children(message.attachments.iter().enumerate().map(
                    |(index, attachment)| {
                        let url = attachment.url.clone();
                        div()
                            .id(ElementId::Name(format!("chat-file:{}:{index}", message.id).into()))
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(radius::CONTROL))
                            .bg(p.chip)
                            .text_size(px(text::FOOTNOTE))
                            .when(url.is_none(), |d| d.text_color(p.muted))
                            .when(url.is_some(), |d| d.cursor_pointer().hover(|s| s.bg(p.selection)))
                            .on_click(move |_, _, cx| {
                                if let Some(url) = &url {
                                    cx.open_url(url);
                                }
                            })
                            .child(format!("📎 {}", attachment.name))
                    },
                )))
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
                            .child(button(ElementId::Name(format!("retry:{}", message.id).into()), "Retry", p).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| this.retry(&retry_key, &retry_id, window, cx)),
                            ))
                            .child(button(ElementId::Name(format!("discard:{}", message.id).into()), "Delete", p).on_click(
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
                .bg(p.page)
                .child(button(ElementId::Name(format!("reply:{}", message.id).into()), "Reply in Thread", p).on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.reply_thread = Some((thread.clone(), who.clone()));
                        window.focus(&this.composer.focus_handle(cx));
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
                .when_some(run.divider.clone(), |d, label| d.child(divider(label, p.muted, p.line)))
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
                    button("chat-earlier", if log.loading_earlier { "Loading…" } else { "Load earlier messages" }, p)
                        .text_color(p.muted)
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
                .child(button("chat-unthread", "✕", p).text_color(p.muted).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
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
                        .on_click(move |_, window, cx| window.focus(&composer.focus_handle(cx)))
                        .child(self.composer.clone())
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
        let (note_text, note_color): (SharedString, _) = match &form.failure {
            Some(failure) => (failure.clone(), p.danger),
            None if form.starting => ("Starting…".into(), p.muted),
            None if count > 1 => (format!("Starts a group chat with these {count} people.").into(), p.muted),
            None if !can_start => (
                "This account can only reopen direct messages it has; sign in again to start new chats.".into(),
                p.muted,
            ),
            None => ("Opens your direct message with this person, or starts one.".into(), p.muted),
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
                    button(ElementId::Name(format!("chat-from:{}", account.id).into()), account.label.clone(), p)
                        .when(selected, |b| b.bg(p.selection))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.form.account = Some(id.clone());
                            this.form.failure = None;
                            cx.notify();
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
            .on_click(move |_, window, cx| window.focus(&people_input.focus_handle(cx)))
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
                                cx.notify();
                            }))
                            .child("✕"),
                    )
            }))
            .child(div().flex_1().min_w(px(160.)).h(px(22.)).flex().items_center().child(self.people_input.clone()));
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
                .child(avatar(&person.label(), &person.email, rows::AVATAR, p))
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
                    .child(button("chat-form-back", "‹", p).text_size(px(text::HEADLINE)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("New Chat")),
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
                        .child(div().text_size(px(text::FOOTNOTE)).text_color(note_color).child(note_text))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(button("chat-form-cancel", "Cancel", p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))))
                                .child(start),
                        ),
                ),
            )
            .into_any_element()
    }
}

// MARK: - Pieces

/// The first letter or digit of a name, uppercased (`Address::initial`).
fn initial(name: &str) -> SharedString {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect::<String>().into())
        .unwrap_or_else(|| "?".into())
}

/// A day divider's label (`ReaderPage.dayDivider`).
fn day_label(date: DateTime<Local>, now: DateTime<Local>) -> String {
    let day = date.date_naive();
    let today = now.date_naive();
    if day == today {
        "Today".into()
    } else if Some(day) == today.checked_sub_days(Days::new(1)) {
        "Yesterday".into()
    } else if date.year() == now.year() {
        date.format("%A, %B %-d").to_string()
    } else {
        date.format("%B %-d, %Y").to_string()
    }
}

fn avatar(name: &str, color_key: &str, size: f32, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(design::avatar_color(color_key, p.dark))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.43))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.on_accent)
        .child(initial(name))
}

/// A text button that keeps its click from moving the window (`ui::button`).
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .text_size(px(text::FOOTNOTE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .active(|s| s.bg(p.selection))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into())
}

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

fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .items_center()
        .justify_center()
        .px(px(32.))
        .text_color(p.muted)
        .child(div().text_center().child(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Body {
        let names = HashMap::from([("users/1".to_owned(), SharedString::from("Brent"))]);
        Body::parse(text, &names, false)
    }

    #[test]
    fn markup_becomes_styled_ranges() {
        let body = parsed("*bold* and _it_ ~gone~ `x` <https://a.example/b|site> <users/1> https://c.example/d.");
        assert_eq!(body.text.as_ref(), "bold and it gone x site @Brent https://c.example/d.");
        let kinds: Vec<Mark> = body.marks.iter().map(|(_, mark)| *mark).collect();
        assert_eq!(kinds, [Mark::Bold, Mark::Italic, Mark::Strike, Mark::Code, Mark::Link, Mark::Mention, Mark::Link]);
        assert_eq!(body.links, [SharedString::from("https://a.example/b"), SharedString::from("https://c.example/d")]);
    }

    #[test]
    fn markers_inside_words_stay_text() {
        let body = parsed("snake_case_name and 2*3*4 and a * b");
        assert_eq!(body.text.as_ref(), "snake_case_name and 2*3*4 and a * b");
        assert!(body.marks.is_empty());
    }

    #[test]
    fn escaped_markup_is_literal() {
        let body = parsed("thanks\\! give me \\~5min \\*not bold\\*");
        assert_eq!(body.text.as_ref(), "thanks! give me ~5min *not bold*");
        assert!(body.marks.is_empty());
    }

    #[test]
    fn fences_keep_their_lines() {
        let body = parsed("look:\n```\nlet a = 1;\n```\ndone");
        assert_eq!(body.text.as_ref(), "look:\nlet a = 1;\ndone");
        assert_eq!(body.marks.len(), 1);
    }
}
