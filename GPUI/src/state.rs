//! `MailApp`'s state and everything that changes it: what is selected, the lists served from
//! memory, the layers over the list, the reader's threads, and the optimistic actions that roll
//! back when the engine refuses them. Rendering is in `ui`.

mod actions;
mod invitations;
mod nav;

pub use actions::Removal;
pub use invitations::InvitationCard;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Local;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, KeyBinding, ListAlignment, ListOffset, ListState, Pixels,
    Point, ScrollHandle, SharedString, Subscription, Window, actions, px,
};
use mailstream::{Change, Event, EventListener, ListQuery};

use crate::bench::Bench;
use crate::chat::ChatPane;
use crate::compose::ReplyBox;
use crate::engine::{MailError, engine};
use crate::format;
use crate::input::{InputEvent, TextInput};
use crate::model::{Account, Category, Conversation, Listing, Mailbox, Message, Role};
use crate::swipe::{BackSwipe, Preferences};

actions!(
    mail,
    [
        Next,
        Previous,
        Open,
        Back,
        Close,
        Archive,
        Trash,
        Restore,
        Star,
        ToggleRead,
        ToggleWaiting,
        Reply,
        ReplyAll,
        Forward,
        NewMessage,
        NewChat,
        FocusSearch,
        Refresh,
        ToggleSidebar,
        Retag,
        CloseMenu,
        Quit
    ]
);

pub const CONTEXT: &str = "Mail";

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the list, never while a text field has focus or in Chat, which has its own.
    let list = Some("Mail && !TextInput && !TextArea && !Chat");
    let window = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("space", Open, list),
        KeyBinding::new("escape", Back, list),
        KeyBinding::new("secondary-[", Back, window),
        KeyBinding::new("secondary-w", Close, window),
        KeyBinding::new("e", Archive, list),
        KeyBinding::new("ctrl-cmd-a", Archive, window),
        KeyBinding::new("backspace", Trash, list),
        KeyBinding::new("delete", Trash, list),
        KeyBinding::new("secondary-backspace", Trash, list),
        KeyBinding::new("s", Star, list),
        KeyBinding::new("secondary-shift-l", Star, window),
        KeyBinding::new("u", ToggleRead, list),
        KeyBinding::new("secondary-shift-u", ToggleRead, window),
        KeyBinding::new("d", ToggleWaiting, list),
        KeyBinding::new("secondary-shift-d", ToggleWaiting, window),
        KeyBinding::new("r", Reply, list),
        KeyBinding::new("secondary-r", Reply, window),
        KeyBinding::new("a", ReplyAll, list),
        KeyBinding::new("secondary-shift-r", ReplyAll, window),
        KeyBinding::new("f", Forward, list),
        KeyBinding::new("secondary-shift-f", Forward, window),
        KeyBinding::new("secondary-n", NewMessage, window),
        KeyBinding::new("/", FocusSearch, list),
        KeyBinding::new("secondary-f", FocusSearch, window),
        KeyBinding::new("secondary-shift-n", Refresh, window),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, window),
        KeyBinding::new("escape", CloseMenu, Some("ContextMenu")),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
}

/// Conversations asked for per page (the engine's most).
const PAGE: u32 = 100;
/// A list this young is served from memory when shown again; the engine's change events reload
/// the one on screen sooner.
const FRESH: Duration = Duration::from_secs(90);
const TOAST: Duration = Duration::from_secs(4);

/// What the list shows.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    /// One role across every account.
    Unified(Role),
    Mailbox { account: SharedString, id: SharedString, name: SharedString, role: Role },
}

impl Source {
    pub fn role(&self) -> Role {
        match self {
            Source::Unified(role) | Source::Mailbox { role, .. } => *role,
        }
    }

    pub fn title(&self) -> SharedString {
        match self {
            Source::Unified(role) => role.title().into(),
            Source::Mailbox { name, .. } => name.clone(),
        }
    }

    fn account(&self) -> Option<&SharedString> {
        match self {
            Source::Unified(_) => None,
            Source::Mailbox { account, .. } => Some(account),
        }
    }
}

/// A list's identity: its source, and the search over it, if any.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ListKey {
    pub source: Source,
    pub query: Option<SharedString>,
}

impl ListKey {
    /// One page of this list from the engine, prepared for display.
    async fn fetch(self, cursor: Option<String>) -> Result<Listing, MailError> {
        let account = self.source.account().map(|a| a.to_string());
        let listing = match &self.query {
            Some(query) => engine().search(query.to_string(), account, PAGE).await?,
            None => {
                let mailbox = match &self.source {
                    Source::Unified(role) => role.arg().to_owned(),
                    Source::Mailbox { id, .. } => id.to_string(),
                };
                let query = ListQuery { mailboxes: vec![mailbox], account, limit: PAGE, cursor, ..ListQuery::default() };
                engine().list(query).await?
            }
        };
        let mut listing = Listing::from(listing);
        let now = Local::now();
        for conversation in &mut listing.conversations {
            conversation.prepare(now);
        }
        Ok(listing)
    }

    /// The Smart Inbox folds bundles in the inbox, outside search.
    pub fn bundles(&self) -> bool {
        self.query.is_none() && self.source.role() == Role::Inbox
    }
}

#[derive(Default)]
pub struct List {
    pub conversations: Vec<Conversation>,
    /// The following page's cursor.
    next: Option<String>,
    fetched: Option<Instant>,
    pub loading: bool,
    /// The engine changed it while a load was under way: load again when that one lands.
    stale: bool,
    loading_more: bool,
    /// The view and its length that asked for the last page. A page that leaves that view's
    /// length unchanged (all of it folded into bundles) asks for no more from that view.
    paged_at: Option<(PagedView, usize)>,
    pub error: Option<SharedString>,
}

/// A scrolling view that pages the list when its end comes into view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagedView {
    List,
    Bundle,
}

/// One line of the list.
#[derive(Clone)]
pub enum Item {
    Section(SharedString),
    /// The Smart Inbox's "Waiting on you" header, over this many conversations.
    Waiting(usize),
    /// An index into the list's conversations.
    Row(usize),
    /// Service mail of one category, folded at its newest member's place.
    Bundle(Rc<Bundle>),
}

pub struct Bundle {
    pub category: Category,
    /// Indexes into the list's conversations, newest first.
    pub members: Vec<usize>,
    /// Who sent what is new (what came last once all is read), for the row.
    pub senders: SharedString,
    /// What the newest of that is about.
    pub subject: SharedString,
    pub unread: usize,
}

impl Bundle {
    fn new(category: Category, members: Vec<usize>, conversations: &[Conversation]) -> Self {
        let unread = members.iter().filter(|&&ix| conversations[ix].unread).count();
        let shown = members.iter().map(|&ix| &conversations[ix]).filter(|c| unread == 0 || c.unread);
        let mut senders: Vec<&str> = Vec::new();
        let mut subject = SharedString::default();
        for conversation in shown {
            if subject.is_empty() {
                subject = conversation.title.clone();
            }
            let sender = conversation.sender.as_ref();
            if !sender.is_empty() && !senders.contains(&sender) {
                senders.push(sender);
            }
            if senders.len() == 6 {
                break;
            }
        }
        Self { category, unread, senders: senders.join(", ").into(), subject, members }
    }
}

/// What lies over the list, bottom to top.
#[derive(Clone)]
pub enum Layer {
    Bundle(Category),
    Reader(Box<Conversation>),
}

impl Layer {
    pub fn id(&self) -> SharedString {
        match self {
            Layer::Bundle(category) => format!("bundle:{}", category.key()).into(),
            Layer::Reader(conversation) => format!("reader:{}", conversation.id).into(),
        }
    }
}

pub enum ThreadState {
    Loading,
    Loaded(Rc<[Message]>),
    Failed(SharedString),
}

pub struct ThreadEntry {
    pub state: ThreadState,
    /// The conversation's message count when loaded: a newer count means a newer thread.
    count: u32,
    /// The latest request for it; older answers are dropped.
    request: u64,
}

pub struct Toast {
    pub text: SharedString,
    generation: u64,
}

/// How the top layer sits over what lies beneath.
#[derive(Clone, Copy)]
pub enum Slide {
    Rest,
    /// Sliding in from the right since then.
    Opening(Instant),
    /// Under the user's fingers, `offset` points to the right.
    Drag(f32),
    /// Released (or sent back by a key): from `from` to `to`; `back` pops the layer on arrival.
    Settle { from: f32, to: f32, start: Instant, back: bool },
}

/// A right-click menu over a row.
pub struct ContextMenu {
    pub id: SharedString,
    pub position: Point<Pixels>,
    pub focus: FocusHandle,
}

/// What draws an HTML body in the reader on the Mac, in the switch's order (Flutter's
/// `ReaderRenderer`): Blitz on every launch, switched only by hand for this run;
/// `MAILSTREAM_READER_RENDERER` (`blitz` or `webKit`) picks the first one, for the bench.
/// Elsewhere Blitz is the only renderer.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Renderer {
    Blitz,
    WebKit,
}

#[cfg(target_os = "macos")]
impl Renderer {
    pub const ALL: [Renderer; 2] = [Renderer::Blitz, Renderer::WebKit];

    pub fn title(self) -> &'static str {
        match self {
            Renderer::Blitz => "Blitz",
            Renderer::WebKit => "WebView (original)",
        }
    }

    /// The one `name` names, case aside; Blitz when unset or unknown.
    pub fn from_name(name: Option<&str>) -> Renderer {
        match name {
            Some(name) if name.eq_ignore_ascii_case("webKit") => Renderer::WebKit,
            _ => Renderer::Blitz,
        }
    }
}

pub struct MailApp {
    pub focus: FocusHandle,
    pub search: Entity<TextInput>,
    pub accounts: Vec<Account>,
    pub mailboxes: HashMap<SharedString, Vec<Mailbox>>,
    pub source: Source,
    pub query: Option<SharedString>,
    pub lists: HashMap<ListKey, List>,
    /// The current list's lines.
    pub items: Vec<Item>,
    /// The keyboard highlight in the list and in the open bundle: a conversation id or a bundle's
    /// `bundle:<category>`.
    pub highlight: Option<SharedString>,
    pub bundle_highlight: Option<SharedString>,
    pub layers: Vec<Layer>,
    pub threads: HashMap<SharedString, ThreadEntry>,
    /// Folded messages the user opened.
    pub unfolded: HashSet<SharedString>,
    /// Messages whose quoted history the user opened.
    pub quotes_open: HashSet<SharedString>,
    /// Meeting invitations' cards, by message.
    pub invitations: HashMap<SharedString, InvitationCard>,
    /// The open conversation stays scrolled to its end (its latest message) while bodies load and
    /// grow, until the reader scrolls it or opens a fold (Flutter's `EndPin`, MS-70).
    pub reader_pinned: bool,
    /// The chat log's message under the pointer, which shows its reply buttons.
    pub hovered_message: Option<SharedString>,
    thread_requests: u64,
    /// Conversations being archived or trashed: refreshes that still list them leave them out.
    removing: HashSet<SharedString>,
    /// A previews request is out; `previews_again` asks once more when it lands.
    previews_asking: bool,
    previews_again: bool,
    pub toast: Option<Toast>,
    /// The row under the pointer, which shows its actions.
    pub hovered_row: Option<SharedString>,
    toast_generation: u64,
    /// The list's and the open bundle's rows (rows and group titles differ in height).
    pub list_scroll: ListState,
    pub bundle_scroll: ListState,
    pub reader_scroll: ScrollHandle,
    /// Sidebar nodes the user opened, by key.
    pub expanded: HashSet<SharedString>,
    pub sidebar_open: bool,
    pub swipe: BackSwipe,
    pub slide: Slide,
    /// The top layer `slide` belongs to; a different top layer rests.
    pub slide_layer: Option<SharedString>,
    /// How far the top layer travels between in and out, as last laid out: the list island's
    /// width for a layer over it, the room the conversation takes beside the list.
    pub content_width: f32,
    pub context_menu: Option<ContextMenu>,
    /// The reader's renderer, for this run (the Mac).
    #[cfg(target_os = "macos")]
    pub renderer: Renderer,
    /// Google Chat, shown instead of the mail list while `chat_view` is set.
    pub chat: Entity<ChatPane>,
    /// The chat account in view (None: every account), while Chat is selected.
    pub chat_view: Option<Option<SharedString>>,
    /// The inline reply under a conversation in the reader.
    pub reply: Option<ReplyDraft>,
    /// A conversation to forward once its messages arrive.
    forward_pending: Option<SharedString>,
    /// The frame measurement, when the environment asks for one (`bench.rs`).
    pub bench: Option<Bench>,
    _subscriptions: Vec<Subscription>,
}

pub struct ReplyDraft {
    pub all: bool,
    pub conversation: SharedString,
    pub view: Entity<ReplyBox>,
    _subscription: Subscription,
}

impl MailApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search", cx));
        let chat = cx.new(|cx| ChatPane::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            // Chat accounts and unread counts feed the sidebar.
            cx.observe(&chat, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            search,
            accounts: Vec::new(),
            mailboxes: HashMap::new(),
            source: Source::Unified(Role::Inbox),
            query: None,
            lists: HashMap::new(),
            items: Vec::new(),
            highlight: None,
            bundle_highlight: None,
            layers: Vec::new(),
            threads: HashMap::new(),
            unfolded: HashSet::new(),
            quotes_open: HashSet::new(),
            invitations: HashMap::new(),
            reader_pinned: false,
            hovered_message: None,
            thread_requests: 0,
            removing: HashSet::new(),
            previews_asking: false,
            previews_again: false,
            toast: None,
            hovered_row: None,
            toast_generation: 0,
            list_scroll: list_state(0),
            bundle_scroll: list_state(0),
            reader_scroll: ScrollHandle::new(),
            expanded: ["unified:inbox", "chat", "section:accounts"].into_iter().map(SharedString::from).collect(),
            sidebar_open: true,
            swipe: BackSwipe::default(),
            slide: Slide::Rest,
            slide_layer: None,
            content_width: 0.0,
            context_menu: None,
            #[cfg(target_os = "macos")]
            renderer: Renderer::from_name(std::env::var("MAILSTREAM_READER_RENDERER").ok().as_deref()),
            chat,
            chat_view: None,
            reply: None,
            forward_pending: None,
            bench: Bench::from_env(cx),
            _subscriptions: subscriptions,
        };
        this.load_mailboxes(cx);
        this.load_list(this.key(), false, cx);
        this.follow_engine(cx);
        this.background(cx, async { Preferences::read() }, |this, preferences, _| this.swipe.preferences = preferences);
        this
    }

    pub fn key(&self) -> ListKey {
        ListKey { source: self.source.clone(), query: self.query.clone() }
    }

    pub fn list(&self) -> Option<&List> {
        self.lists.get(&self.key())
    }

    /// Runs `work` on the background executor and hands its result to `done` on the main thread.
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

    // MARK: - Loading

    /// Re-reads what the engine says changed: the list on screen on new mail, moves and tags
    /// (other lists count as stale), counts on mailbox and account changes, previews as they load.
    fn follow_engine(&self, cx: &mut Context<Self>) {
        let (sender, mut events) = unbounded();
        // The listener only queues the event, so it runs on the engine's thread: GPUI's own loop
        // never drains the main-thread queue `subscribe` posts to on Linux (GLib's).
        engine().subscribe_any_thread(Arc::new(Changes(sender)));
        cx.spawn(async move |this, cx| {
            while let Some(first) = events.next().await {
                // A sync sends bursts; one pass answers everything queued.
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let alive = this.update(cx, |this, cx| {
                    let (chat, messages) = (batch.contains(&Event::Chat), batch.contains(&Event::ChatMessages));
                    if chat || messages {
                        this.chat.update(cx, |pane, cx| pane.follow_engine(chat, messages, cx));
                    }
                    if batch.contains(&Event::Calendar) {
                        this.calendar_changed(cx);
                    }
                    let mailboxes = batch.iter().any(|e| matches!(e, Event::Accounts | Event::Mailboxes));
                    if mailboxes {
                        this.load_mailboxes(cx);
                    }
                    // A list asked for before the engine knew the mailboxes failed; it can load now.
                    let failed = mailboxes && this.list().is_some_and(|list| list.error.is_some());
                    if failed || batch.iter().any(|e| matches!(e, Event::Conversations | Event::Tags)) {
                        let key = this.key();
                        for (other, list) in &mut this.lists {
                            if other != &key {
                                list.fetched = None;
                            }
                        }
                        this.load_list(key, true, cx);
                    } else if batch.contains(&Event::Previews) {
                        this.load_previews(&this.key(), cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn load_mailboxes(&mut self, cx: &mut Context<Self>) {
        self.background(
            cx,
            async {
                let accounts = engine().accounts().await?;
                let mailboxes = engine().mailboxes(None).await?;
                Ok::<_, MailError>((
                    accounts.into_iter().map(Account::from).collect::<Vec<_>>(),
                    mailboxes
                        .into_iter()
                        .map(|(account, list)| (account.into(), list.into_iter().map(Mailbox::from).collect()))
                        .collect::<HashMap<SharedString, Vec<Mailbox>>>(),
                ))
            },
            |this, result, cx| {
                if let Ok((accounts, mailboxes)) = result {
                    this.accounts = accounts;
                    this.mailboxes = mailboxes;
                    cx.notify();
                }
            },
        );
    }

    /// Asks for `key`'s first page unless a fresh copy is in memory (or `force`). A forced load
    /// while one is under way runs again once that one lands.
    pub fn load_list(&mut self, key: ListKey, force: bool, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if list.loading {
            list.stale |= force;
            return;
        }
        if !force && list.fetched.is_some_and(|at| at.elapsed() < FRESH) {
            return;
        }
        list.loading = true;
        list.stale = false;
        cx.notify();
        self.background(cx, key.clone().fetch(None), move |this, result, cx| this.list_arrived(key, result, false, cx));
    }

    /// Asks for the page after the current list's last, when there is one and none is loading.
    /// `shown` is the length of the view whose end the user reached.
    pub fn load_more(&mut self, view: PagedView, shown: usize, cx: &mut Context<Self>) {
        let key = self.key();
        let Some(list) = self.lists.get_mut(&key) else { return };
        let Some(cursor) = list.next.clone() else { return };
        if list.loading || list.loading_more || list.paged_at == Some((view, shown)) {
            return;
        }
        list.loading_more = true;
        list.paged_at = Some((view, shown));
        self.background(cx, key.clone().fetch(Some(cursor)), move |this, result, cx| {
            this.list_arrived(key, result, true, cx)
        });
    }

    fn list_arrived(&mut self, key: ListKey, result: Result<Listing, MailError>, more: bool, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if !more {
            list.loading = false;
        }
        list.loading_more = false;
        let again = !more && std::mem::take(&mut list.stale);
        match result {
            Ok(mut listing) => {
                listing.conversations.retain(|c| !self.removing.contains(&c.id));
                if more {
                    let known: HashSet<SharedString> = list.conversations.iter().map(|c| c.id.clone()).collect();
                    list.conversations.extend(listing.conversations.into_iter().filter(|c| !known.contains(&c.id)));
                    list.next = listing.next;
                } else {
                    // A refreshed first page replaces its span; older pages already loaded stay,
                    // with the cursor past them.
                    let oldest = listing.conversations.last().and_then(|c| c.date);
                    let fresh: HashSet<SharedString> = listing.conversations.iter().map(|c| c.id.clone()).collect();
                    let tail: Vec<Conversation> = std::mem::take(&mut list.conversations)
                        .into_iter()
                        .filter(|c| !fresh.contains(&c.id) && oldest.is_some_and(|oldest| c.date.is_some_and(|d| d < oldest)))
                        .collect();
                    list.conversations = listing.conversations;
                    list.fetched = Some(Instant::now());
                    list.paged_at = None;
                    if tail.is_empty() {
                        list.next = listing.next;
                    } else {
                        list.conversations.extend(tail);
                    }
                }
                list.error = (!listing.failures.is_empty()).then(|| listing.failures.join("; ").into());
            }
            Err(error) => list.error = Some(error.to_string().into()),
        }
        if key == self.key() {
            self.rebuild_items();
            self.refresh_layers(cx);
        }
        self.load_previews(&key, cx);
        if again {
            self.load_list(key, true, cx);
        }
        cx.notify();
    }

    /// Previews missing from a listing, which the engine loads on demand; one request at a time.
    fn load_previews(&mut self, key: &ListKey, cx: &mut Context<Self>) {
        if self.previews_asking {
            self.previews_again = true;
            return;
        }
        let Some(list) = self.lists.get(key) else { return };
        let ids: Vec<String> =
            list.conversations.iter().filter(|c| c.snippet.is_none()).map(|c| c.id.to_string()).take(PAGE as usize).collect();
        if ids.is_empty() {
            return;
        }
        self.previews_asking = true;
        self.background(
            cx,
            engine().previews(ids),
            |this, result, cx| {
                this.previews_asking = false;
                if let Ok(previews) = result {
                    // A conversation without one shows none rather than being asked for again.
                    for list in this.lists.values_mut() {
                        for conversation in &mut list.conversations {
                            if let Some(preview) = previews.get(conversation.id.as_ref()) {
                                conversation.snippet = Some(preview.clone().unwrap_or_default().into());
                            }
                        }
                    }
                    cx.notify();
                }
                if std::mem::take(&mut this.previews_again) {
                    this.load_previews(&this.key(), cx);
                }
            },
        );
    }

    /// The current list's lines. In the Smart Inbox service mail folds into bundles pinned above
    /// the rest, newest bundle first, and what waits on the user's reply or action follows under
    /// its own header, as in the native app's Smart Inbox; date headers group the remaining rows.
    pub fn rebuild_items(&mut self) {
        let key = self.key();
        let bundles = key.bundles();
        let sections = key.query.is_none();
        let Some(list) = self.lists.get(&key) else {
            self.items.clear();
            self.list_scroll.reset(0);
            return;
        };
        let mut rows = Vec::with_capacity(list.conversations.len());
        let mut pinned: Vec<Category> = Vec::new();
        let mut members: HashMap<Category, Vec<usize>> = HashMap::new();
        let mut waiting = Vec::new();
        for (index, conversation) in list.conversations.iter().enumerate() {
            if bundles && conversation.waiting {
                waiting.push(index);
                continue;
            }
            match conversation.bundle().filter(|_| bundles) {
                None => rows.push(index),
                Some(category) => {
                    let group = members.entry(category).or_default();
                    if group.is_empty() {
                        pinned.push(category);
                    }
                    group.push(index);
                }
            }
        }
        let now = Local::now();
        let mut items = Vec::with_capacity(pinned.len() + waiting.len() + rows.len() + 9);
        items.extend(pinned.into_iter().map(|category| {
            let members = members.remove(&category).unwrap_or_default();
            Item::Bundle(Rc::new(Bundle::new(category, members, &list.conversations)))
        }));
        if !waiting.is_empty() {
            items.push(Item::Waiting(waiting.len()));
            items.extend(waiting.into_iter().map(Item::Row));
        }
        let mut current: Option<String> = None;
        for index in rows {
            if sections {
                let section = format::list_section(list.conversations[index].date, now);
                if current.as_ref() != Some(&section) {
                    items.push(Item::Section(section.clone().into()));
                    current = Some(section);
                }
            }
            items.push(Item::Row(index));
        }
        // The row at the top stays there, wherever the new lines put it.
        let top = self.list_scroll.logical_scroll_top();
        let kept = self.items.get(top.item_ix).map(|item| self.item_id(item));
        self.items = items;
        self.list_scroll.reset(self.items.len());
        let at = kept.and_then(|id| self.items.iter().position(|item| self.item_id(item) == id));
        self.list_scroll.scroll_to(match at {
            Some(item_ix) => ListOffset { item_ix, offset_in_item: top.offset_in_item },
            None => ListOffset::default(),
        });
    }

    /// Drops a bundle layer whose bundle is gone, and keeps open readers' conversations current:
    /// one with new messages reads its thread again, showing the old one until it arrives (MS-79).
    fn refresh_layers(&mut self, cx: &mut Context<Self>) {
        let bundles: HashSet<Category> = self
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Bundle(bundle) => Some(bundle.category),
                _ => None,
            })
            .collect();
        let mut layers = std::mem::take(&mut self.layers);
        let current: HashMap<&SharedString, &Conversation> = self
            .list()
            .map(|list| list.conversations.iter().map(|c| (&c.id, c)).collect())
            .unwrap_or_default();
        let mut stale = Vec::new();
        for layer in &mut layers {
            if let Layer::Reader(conversation) = layer
                && let Some(fresh) = current.get(&conversation.id)
            {
                **conversation = (*fresh).clone();
                if self.threads.get(&fresh.id).is_some_and(|t| t.count != fresh.message_count) {
                    stale.push((*fresh).clone());
                }
            }
        }
        // A bundle that emptied closes, with whatever lies over it.
        if let Some(gone) = layers.iter().position(|l| matches!(l, Layer::Bundle(c) if !bundles.contains(c))) {
            layers.truncate(gone);
        }
        self.layers = layers;
        for conversation in stale {
            self.load_thread(&conversation, true, cx);
        }
    }

    /// Reads the conversation's messages, unless they are loaded for its message count; `again`
    /// reads them anyway. A thread on screen stays until the new one arrives, and stays if that
    /// fails.
    pub(crate) fn load_thread(&mut self, conversation: &Conversation, again: bool, cx: &mut Context<Self>) {
        self.thread_requests += 1;
        let request = self.thread_requests;
        let id = conversation.id.clone();
        match self.threads.get_mut(&id) {
            Some(thread)
                if !again
                    && thread.count == conversation.message_count
                    && !matches!(thread.state, ThreadState::Failed(_)) =>
            {
                return;
            }
            Some(thread) if matches!(thread.state, ThreadState::Loaded(_)) => {
                thread.count = conversation.message_count;
                thread.request = request;
            }
            _ => {
                let entry = ThreadEntry { state: ThreadState::Loading, count: conversation.message_count, request };
                self.threads.insert(id.clone(), entry);
            }
        }
        let arg = id.to_string();
        self.background(
            cx,
            async move {
                let thread = engine().show(arg, 200_000, true).await?;
                let now = Local::now();
                Ok::<_, MailError>(
                    thread
                        .messages
                        .into_iter()
                        .map(|message| {
                            let mut message = Message::from(message);
                            message.prepare(now);
                            message
                        })
                        .collect::<Vec<_>>(),
                )
            },
            move |this, result, cx| {
                let Some(thread) = this.threads.get_mut(&id).filter(|thread| thread.request == request) else { return };
                let mut loaded = None;
                match result {
                    Ok(messages) => {
                        let messages: Rc<[Message]> = messages.into();
                        thread.state = ThreadState::Loaded(messages.clone());
                        loaded = Some(messages);
                    }
                    Err(_) if matches!(thread.state, ThreadState::Loaded(_)) => {}
                    Err(error) => thread.state = ThreadState::Failed(error.to_string().into()),
                }
                if let Some(messages) = loaded {
                    this.read_invitations(&id, &messages, cx);
                }
                if this.forward_pending.as_ref() == Some(&id) {
                    this.forward_pending = None;
                    this.forward_loaded(&id, cx);
                }
                cx.notify();
            },
        );
    }
}

/// A fresh list of `count` rows, scrolled to the top.
pub fn list_state(count: usize) -> ListState {
    ListState::new(count, ListAlignment::Top, px(400.))
}

/// Hands the engine's change events to the window's task.
struct Changes(UnboundedSender<Event>);

impl EventListener for Changes {
    fn on_event(&self, change: Change) {
        let _ = self.0.unbounded_send(change.event);
    }
}
