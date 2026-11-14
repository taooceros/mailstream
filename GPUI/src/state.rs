//! `MailApp`'s state and everything that changes it: what is selected, the lists served from
//! memory, the layers over the list, the reader's threads, and the optimistic actions that roll
//! back when the engine refuses them. Rendering is in `ui`.

mod actions;
mod invitations;
mod nav;
mod selection;
mod split;

pub use actions::Removal;
pub use invitations::InvitationCard;
pub use selection::Selection;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::component::menu::PopupMenu;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, FocusHandle, Focusable, KeyBinding, ListAlignment, ListOffset,
    ListState, Modifiers, Pixels,
    Point, ScrollHandle, SharedString, Subscription, Task, Window, actions, px,
};
use mailstream::{Account, Category, Change, Conversation, Event, EventListener, ListQuery, Listing, Mailbox, MailboxRole};

use crate::bench::Bench;
use crate::calendar::{CalendarPane, CalendarToast};
use crate::chat::{ChatPane, UnreadSpace};
use crate::compose::{ComposeColumn, ReplyBox};
use crate::engine::{MailError, engine};
use crate::frame_rate::FrameRateMeter;
use crate::model::{ConversationExt, RoleExt, ShownMessage};
use crate::swipe::{BackSwipe, MouseDrag, Preferences, Slide};

actions!(
    mail,
    [
        Next,
        Previous,
        ExtendNext,
        ExtendPrevious,
        SelectAll,
        Open,
        Back,
        Close,
        Archive,
        Trash,
        Restore,
        Star,
        ToggleRead,
        ToggleWaiting,
        Undo,
        Reply,
        ReplyAll,
        Forward,
        NewMessage,
        NewChat,
        FocusSearch,
        Refresh,
        ToggleSidebar,
        Retag,
        ToggleFrameRate,
        Quit
    ]
);

pub const CONTEXT: &str = "Mail";

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the list, never while a text field has focus, in Chat or the calendar
    // (which have their own), or in a draft.
    let list = Some("Mail && !Input && !Chat && !Calendar && !Compose");
    let window = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("shift-down", ExtendNext, list),
        KeyBinding::new("shift-up", ExtendPrevious, list),
        KeyBinding::new(secondary!("a"), SelectAll, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("space", Open, list),
        KeyBinding::new("escape", Back, list),
        KeyBinding::new(secondary!("["), Back, window),
        KeyBinding::new(secondary!("w"), Close, window),
        KeyBinding::new("e", Archive, list),
        KeyBinding::new("ctrl-cmd-a", Archive, window),
        KeyBinding::new("backspace", Trash, list),
        KeyBinding::new("delete", Trash, list),
        KeyBinding::new(secondary!("backspace"), Trash, list),
        KeyBinding::new("s", Star, list),
        KeyBinding::new(secondary!("shift-l"), Star, window),
        KeyBinding::new("u", ToggleRead, list),
        KeyBinding::new(secondary!("shift-u"), ToggleRead, window),
        KeyBinding::new("d", ToggleWaiting, list),
        KeyBinding::new(secondary!("shift-d"), ToggleWaiting, window),
        KeyBinding::new(secondary!("z"), Undo, list),
        KeyBinding::new("r", Reply, list),
        KeyBinding::new(secondary!("r"), Reply, window),
        KeyBinding::new("a", ReplyAll, list),
        KeyBinding::new(secondary!("shift-r"), ReplyAll, window),
        KeyBinding::new("f", Forward, list),
        KeyBinding::new(secondary!("shift-f"), Forward, window),
        KeyBinding::new(secondary!("n"), NewMessage, window),
        KeyBinding::new("/", FocusSearch, list),
        KeyBinding::new(secondary!("f"), FocusSearch, window),
        KeyBinding::new(secondary!("shift-n"), Refresh, window),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, window),
        KeyBinding::new(secondary!("alt-p"), ToggleFrameRate, window),
        KeyBinding::new(secondary!("q"), Quit, None),
    ]);
}

/// Conversations asked for per page (the engine's most).
const PAGE: u32 = 100;
/// A list this young is served from memory when shown again; the engine's change events reload
/// the one on screen sooner.
const FRESH: Duration = Duration::from_secs(90);

/// What the list shows.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    /// One role across every account.
    Unified(MailboxRole),
    Mailbox { account: String, id: String, name: SharedString, role: MailboxRole },
}

impl Source {
    pub fn role(&self) -> MailboxRole {
        match self {
            Source::Unified(role) | Source::Mailbox { role, .. } => *role,
        }
    }

    pub fn title(&self) -> SharedString {
        match self {
            Source::Unified(role) => role.label().into(),
            Source::Mailbox { name, .. } => name.clone(),
        }
    }

    fn account(&self) -> Option<&String> {
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
    /// One page of this list from the engine, prepared for display. `cached`: what the engine
    /// holds, at once, without asking a provider; a search always asks.
    async fn fetch(self, cursor: Option<String>, cached: bool) -> Result<Listing, MailError> {
        let account = self.source.account().cloned();
        match &self.query {
            Some(query) => engine().search(query.to_string(), account, PAGE).await,
            None => {
                let mailbox = match &self.source {
                    Source::Unified(role) => role.as_str().to_owned(),
                    Source::Mailbox { id, .. } => id.clone(),
                };
                let query = ListQuery { mailboxes: vec![mailbox], account, limit: PAGE, cursor, cached, ..ListQuery::default() };
                engine().list(query).await
            }
        }
    }

    /// The Smart Inbox folds bundles in the inbox, outside search.
    pub fn bundles(&self) -> bool {
        self.query.is_none() && self.source.role() == MailboxRole::Inbox
    }
}

#[derive(Default)]
pub struct List {
    pub conversations: Vec<Conversation>,
    /// The following page's cursor.
    next: Option<String>,
    /// When the providers last answered for it.
    fetched: Option<Instant>,
    /// A read of what the engine holds (a search: the search) is under way.
    pub loading: bool,
    /// The engine changed it while a read was under way: read again when that one lands.
    stale: bool,
    /// The providers are asked for its newest (`fetch_list`); what arrives meanwhile shows as
    /// the engine's change events bring it.
    pub fetching: bool,
    loading_more: bool,
    /// The view and its length that asked for the last page. A page that leaves that view's
    /// length unchanged (all of it folded into bundles) asks for no more from that view.
    paged_at: Option<(PagedView, usize)>,
    /// The engine refused the last answer (no such mailbox yet).
    pub error: Option<SharedString>,
    /// The mailboxes whose providers failed when last asked.
    pub failures: Option<SharedString>,
}

/// What a list answer was asked as.
#[derive(Clone, Copy, PartialEq)]
enum Arrival {
    /// What the engine holds (a search's results).
    Read,
    /// The providers' newest.
    Fetch,
    /// The page after the last.
    More,
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
    /// Chat's spaces with unread messages, folded into one row over the bundles (MS-330).
    Chat(Rc<ChatBundle>),
}

/// What a bundle folds: service mail of one category, or Chat's unread spaces (MS-330).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BundleKind {
    Mail(Category),
    Chat,
}

impl BundleKind {
    /// Its row's id in the list.
    pub fn id(self) -> String {
        match self {
            BundleKind::Mail(category) => format!("bundle:{}", category.as_str()),
            BundleKind::Chat => "bundle:chat".into(),
        }
    }
}

/// The inbox's Chat bundle: the spaces of the list's accounts with unread messages.
pub struct ChatBundle {
    pub spaces: Vec<UnreadSpace>,
    /// Their titles, for the row.
    pub titles: SharedString,
}

impl ChatBundle {
    fn new(spaces: Vec<UnreadSpace>) -> Self {
        let titles: Vec<&str> = spaces.iter().take(6).map(|space| space.title.as_ref()).collect();
        Self { titles: titles.join(", ").into(), spaces }
    }
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
        let mut senders: Vec<SharedString> = Vec::new();
        let mut subject = SharedString::default();
        for conversation in shown {
            if subject.is_empty() {
                subject = conversation.title.clone().into();
            }
            let sender = conversation.sender_name();
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
    Bundle(BundleKind),
    Reader(Box<Conversation>),
}

impl Layer {
    pub fn id(&self) -> SharedString {
        match self {
            Layer::Bundle(kind) => kind.id().into(),
            Layer::Reader(conversation) => format!("reader:{}", conversation.id).into(),
        }
    }
}

/// Where a conversation beside the list's island rests (two panes), in the content area's
/// points from its left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beside {
    /// The list island's width: how far a bundle over it travels.
    pub list: f32,
    /// Where the conversation's island starts.
    pub edge: f32,
    /// How far the conversation travels between in and out.
    pub travel: f32,
}

pub enum ThreadState {
    Loading,
    Loaded(Rc<[ShownMessage]>),
    Failed(SharedString),
}

pub struct ThreadEntry {
    pub state: ThreadState,
    /// The conversation's message count when loaded: a newer count means a newer thread.
    count: u32,
    /// The latest request for it; older answers are dropped.
    request: u64,
}

/// Where the open conversation holds its scroll while its messages load and grow, until the
/// reader scrolls it or opens a fold (MS-70, MS-255).
#[derive(Clone, PartialEq)]
pub enum ReaderPin {
    /// Where the reader left it.
    Free,
    /// Just opened: where it opens is decided once its messages are in (`pin_opening`).
    Opening,
    /// The head of this message just under the toolbar: the first unread (else the last) of a
    /// thread whose first message isn't it. A single mail, or a thread opening at its first
    /// message, stays at its top.
    Message(String),
    /// Its end: the reply box being written, or the reply just sent.
    End,
}

/// How far the open conversation is scrolled, as the toolbar above it shows: a scrim behind the
/// toolbar once anything passes under it, and the subject between its capsules once the title has.
#[derive(Clone, Copy, PartialEq)]
pub enum ReaderTop {
    Top,
    Scrolled,
    PastTitle,
}

/// A right-click menu over a row: gpui-kit's `PopupMenu` (`ui/menu.rs`), at the pointer.
pub struct ContextMenu {
    pub id: String,
    pub position: Point<Pixels>,
    pub menu: Entity<PopupMenu>,
    /// Closes it here when the kit dismisses it (Escape, a press outside, an entry run).
    pub _dismissed: Subscription,
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
    pub search: Entity<InputState>,
    pub accounts: Vec<Account>,
    pub mailboxes: HashMap<String, Vec<Mailbox>>,
    /// The accounts that can't connect just now, and why (the engine's `status`, MS-278).
    pub account_problems: HashMap<String, SharedString>,
    /// The account notice's message when the user closed it (MS-322): it stays hidden until the
    /// message changes.
    pub dismissed_account_notice: Option<String>,
    pub source: Source,
    pub query: Option<SharedString>,
    pub lists: HashMap<ListKey, List>,
    /// The current list's lines.
    pub items: Vec<Item>,
    /// The keyboard highlight in the list and in the open bundle: a conversation id or a bundle's
    /// `bundle:<category>`.
    pub highlight: Option<String>,
    pub bundle_highlight: Option<String>,
    /// Conversations picked together (MS-305); `selection()` reads the list on screen's.
    selection: Selection,
    pub layers: Vec<Layer>,
    pub threads: HashMap<String, ThreadEntry>,
    /// Folded messages the user opened.
    pub unfolded: HashSet<String>,
    /// Messages whose quoted history the user opened.
    pub quotes_open: HashSet<String>,
    /// Messages whose full headers the user opened from their recipient line.
    pub headers_open: HashSet<String>,
    /// Meeting invitations' cards, by message.
    pub invitations: HashMap<String, InvitationCard>,
    pub reader_pin: ReaderPin,
    pub reader_top: ReaderTop,
    /// The chat log's message under the pointer, which shows its reply buttons.
    pub hovered_message: Option<String>,
    thread_requests: u64,
    /// Conversations being archived or trashed: refreshes that still list them leave them out.
    removing: HashSet<String>,
    /// A previews request is out; `previews_again` asks once more when it lands.
    previews_asking: bool,
    previews_again: bool,
    /// The window, for the toasts (`show_toast`) asked for from a task without one at hand.
    pub window: AnyWindowHandle,
    /// The row under the pointer, which shows its actions.
    pub hovered_row: Option<String>,
    /// The list's and the open bundle's rows (rows and group titles differ in height).
    pub list_scroll: ListState,
    pub bundle_scroll: ListState,
    pub reader_scroll: ScrollHandle,
    /// Sidebar nodes the user opened, by key.
    pub expanded: HashSet<SharedString>,
    pub sidebar_open: bool,
    /// The docked compose column folds the sidebar to the rail this frame: the window is narrower
    /// than `compose::SIDEBAR_WIDTH`.
    pub sidebar_folded: bool,
    /// The user opened the sidebar while the column folded it.
    pub sidebar_beside_compose: bool,
    pub swipe: BackSwipe,
    /// A mouse drag on a layer (MS-266).
    pub drag: MouseDrag,
    /// How the sliding layer sits over what lies beneath.
    pub slide: Slide,
    /// The layer `slide` belongs to: the top one, or the one a gesture took under the pointer
    /// (MS-299); a layer gone or replaced takes the top one at rest.
    pub slide_layer: Option<SharedString>,
    /// The layer the gesture in progress closes: the one under the pointer as it began (MS-299).
    pub gesture_layer: Option<SharedString>,
    /// How far a layer over the list's island travels between in and out, as last laid out.
    pub content_width: f32,
    /// A conversation beside the list, as last laid out (two panes).
    pub beside: Option<Beside>,
    pub context_menu: Option<ContextMenu>,
    /// The reader's renderer, for this run (the Mac).
    #[cfg(target_os = "macos")]
    pub renderer: Renderer,
    /// Google Chat, shown instead of the mail list while `chat_view` is set.
    pub chat: Entity<ChatPane>,
    /// The chat account in view (None: every account), while Chat is selected.
    pub chat_view: Option<Option<SharedString>>,
    /// Chat's spaces with unread messages, for the inbox's Chat bundle (MS-330).
    chat_unread: Vec<UnreadSpace>,
    /// The calendar, shown instead of the mail list while `calendar_view` is set.
    pub calendar: Entity<CalendarPane>,
    pub calendar_view: bool,
    /// The inline reply under a conversation in the reader.
    pub reply: Option<ReplyDraft>,
    /// The drafts being written, in a column of the window.
    pub compose: Entity<ComposeColumn>,
    /// A conversation to forward once its messages arrive.
    forward_pending: Option<String>,
    /// The frame measurement, when the environment asks for one (`bench.rs`).
    pub bench: Option<Bench>,
    /// The list's share of the room beside the navigation next to an open conversation
    /// (`gpui.listWidth`), and the calendar's of its room next to an open event
    /// (`gpui.calendarWidth`); see `split.rs`.
    pub list_share: f32,
    pub calendar_share: f32,
    /// Where the list starts, the room it and the page beside it share (less a docked compose
    /// column), and the room beside the navigation, as last laid out, for a drag of its edge.
    pub split_left: f32,
    pub split_mail: f32,
    pub split_room: f32,
    /// Writes the dragged share once the drag rests.
    share_save: Option<Task<()>>,
    /// The frame rate meter, while `gpui.showFrameRate` is on.
    pub frame_meter: Option<Entity<FrameRateMeter>>,
    _subscriptions: Vec<Subscription>,
}

pub struct ReplyDraft {
    pub all: bool,
    pub conversation: String,
    /// The message it answers; the conversation's latest when `None`.
    pub message: Option<String>,
    pub view: Entity<ReplyBox>,
    _subscription: Subscription,
}

impl MailApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let chat = cx.new(|cx| ChatPane::new(window, cx));
        let calendar = cx.new(|cx| CalendarPane::new(window, cx));
        let compose = cx.new(|_| ComposeColumn::new());
        let subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            // The kit's components follow the appearance with the app (`design::sync_kit_theme`),
            // and so does the Mac window's glass tint.
            cx.observe_window_appearance(window, |_, window, cx| {
                crate::design::sync_kit_theme(window.appearance(), cx);
                #[cfg(target_os = "macos")]
                crate::glass::follow_appearance(window);
                cx.notify()
            }),
            // Chat accounts and unread counts feed the sidebar, and unread spaces the inbox's
            // Chat bundle.
            cx.observe(&chat, |this, chat, cx| {
                let unread = chat.read(cx).unread_spaces();
                if unread != this.chat_unread {
                    this.chat_unread = unread;
                    this.rebuild_items();
                    this.refresh_layers(cx);
                }
                cx.notify()
            }),
            cx.subscribe(&calendar, |this, _, toast: &CalendarToast, cx| this.show_toast(toast.0.clone(), cx)),
            cx.observe(&calendar, |_, _, cx| cx.notify()),
            // The column's placement and width lay the window out.
            cx.observe(&compose, |_, _, cx| cx.notify()),
            cx.subscribe_in(&compose, window, Self::on_compose_event),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            search,
            accounts: Vec::new(),
            mailboxes: HashMap::new(),
            account_problems: HashMap::new(),
            dismissed_account_notice: None,
            source: Source::Unified(MailboxRole::Inbox),
            query: None,
            lists: HashMap::new(),
            items: Vec::new(),
            highlight: None,
            bundle_highlight: None,
            layers: Vec::new(),
            threads: HashMap::new(),
            unfolded: HashSet::new(),
            quotes_open: HashSet::new(),
            headers_open: HashSet::new(),
            invitations: HashMap::new(),
            reader_pin: ReaderPin::Free,
            reader_top: ReaderTop::Top,
            hovered_message: None,
            thread_requests: 0,
            removing: HashSet::new(),
            selection: Selection::default(),
            previews_asking: false,
            previews_again: false,
            window: window.window_handle(),
            hovered_row: None,
            list_scroll: list_state(0),
            bundle_scroll: list_state(0),
            reader_scroll: ScrollHandle::new(),
            expanded: ["unified:inbox", "chat", "section:accounts"].into_iter().map(SharedString::from).collect(),
            sidebar_open: true,
            sidebar_folded: false,
            sidebar_beside_compose: false,
            swipe: BackSwipe::default(),
            drag: MouseDrag::default(),
            slide: Slide::Rest,
            slide_layer: None,
            gesture_layer: None,
            content_width: 0.0,
            beside: None,
            context_menu: None,
            #[cfg(target_os = "macos")]
            renderer: Renderer::from_name(std::env::var("MAILSTREAM_READER_RENDERER").ok().as_deref()),
            chat,
            chat_view: None,
            chat_unread: Vec::new(),
            calendar,
            calendar_view: false,
            reply: None,
            compose,
            forward_pending: None,
            bench: Bench::from_env(cx),
            list_share: crate::split::shares(crate::split::LIST_WIDTH).default,
            calendar_share: crate::split::shares(crate::split::CALENDAR_WIDTH).default,
            split_left: 0.,
            split_mail: 0.,
            split_room: 0.,
            share_save: None,
            frame_meter: None,
            _subscriptions: subscriptions,
        };
        this.load_mailboxes(cx);
        this.load_problems(cx);
        this.load_list(this.key(), false, cx);
        this.follow_engine(cx);
        this.load_layout_settings(cx);
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
    /// (other lists count as stale), counts on mailbox and account changes, previews as they load,
    /// the accounts that can't connect.
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
                    if batch.iter().any(|e| matches!(e, Event::Calendar | Event::Accounts)) {
                        this.calendar.update(cx, |pane, cx| pane.changed(cx));
                    }
                    if batch.contains(&Event::Settings) {
                        this.load_layout_settings(cx);
                    }
                    let mailboxes = batch.iter().any(|e| matches!(e, Event::Accounts | Event::Mailboxes));
                    if mailboxes {
                        this.load_mailboxes(cx);
                    }
                    if batch.iter().any(|e| matches!(e, Event::Accounts | Event::Problems)) {
                        this.load_problems(cx);
                    }
                    // A list asked for before the engine knew the mailboxes failed; it can load now.
                    let failed = mailboxes && this.list().is_some_and(|list| list.error.is_some());
                    if failed {
                        this.load_list(this.key(), true, cx);
                    } else if batch.iter().any(|e| matches!(e, Event::Conversations | Event::Tags)) {
                        let key = this.key();
                        for (other, list) in &mut this.lists {
                            if other != &key {
                                list.fetched = None;
                            }
                        }
                        this.read_list(key, cx);
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
                        .collect::<HashMap<String, Vec<Mailbox>>>(),
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

    /// The accounts that can't connect, as the engine's `status` has them.
    fn load_problems(&mut self, cx: &mut Context<Self>) {
        self.background(cx, engine().status(), |this, result, cx| {
            let Ok(status) = result else { return };
            let problems: HashMap<String, SharedString> =
                status.accounts.into_iter().filter_map(|account| Some((account.id, account.problem?.into()))).collect();
            if problems != this.account_problems {
                this.account_problems = problems;
                cx.notify();
            }
        });
    }

    /// Shows `key`'s list: what the engine holds at once (`read_list`), and unless a fresh copy is
    /// in memory (or `force`), the providers' newest (`fetch_list`), which the engine takes in
    /// mailbox by mailbox, each showing as its change event arrives: a slow or failing account
    /// holds up neither the others nor the list (MS-278).
    pub fn load_list(&mut self, key: ListKey, force: bool, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if !force && list.fetched.is_some_and(|at| at.elapsed() < FRESH) {
            return;
        }
        self.read_list(key.clone(), cx);
        if key.query.is_none() {
            self.fetch_list(key, cx);
        }
    }

    /// Reads `key`'s first page as the engine holds it (a search asks the providers). A read asked
    /// for while one is under way runs again once that one lands.
    fn read_list(&mut self, key: ListKey, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if list.loading {
            list.stale = true;
            return;
        }
        list.loading = true;
        list.stale = false;
        cx.notify();
        self.background(cx, key.clone().fetch(None, true), move |this, result, cx| {
            this.list_arrived(key, result, Arrival::Read, cx)
        });
    }

    /// Asks the providers for `key`'s newest, unless that is under way; its answer, which waits for
    /// every account, lands like a read, with the accounts that failed.
    fn fetch_list(&mut self, key: ListKey, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if list.fetching {
            return;
        }
        list.fetching = true;
        cx.notify();
        self.background(cx, key.clone().fetch(None, false), move |this, result, cx| {
            this.list_arrived(key, result, Arrival::Fetch, cx)
        });
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
        self.background(cx, key.clone().fetch(Some(cursor), false), move |this, result, cx| {
            this.list_arrived(key, result, Arrival::More, cx)
        });
    }

    fn list_arrived(&mut self, key: ListKey, result: Result<Listing, MailError>, arrival: Arrival, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        match arrival {
            Arrival::Read => list.loading = false,
            Arrival::Fetch => list.fetching = false,
            Arrival::More => list.loading_more = false,
        }
        let again = arrival == Arrival::Read && std::mem::take(&mut list.stale);
        // The providers answered: a fetch, or a search.
        let asked = arrival == Arrival::Fetch || (arrival == Arrival::Read && key.query.is_some());
        match result {
            Ok(mut listing) => {
                listing.conversations.retain(|c| !self.removing.contains(&c.id));
                if arrival == Arrival::More {
                    let known: HashSet<String> = list.conversations.iter().map(|c| c.id.clone()).collect();
                    list.conversations.extend(listing.conversations.into_iter().filter(|c| !known.contains(&c.id)));
                    list.next = listing.next;
                } else {
                    // A refreshed first page replaces its span; older pages already loaded stay,
                    // with the cursor past them.
                    let oldest = listing.conversations.last().and_then(ConversationExt::time);
                    let fresh: HashSet<String> = listing.conversations.iter().map(|c| c.id.clone()).collect();
                    let tail: Vec<Conversation> = std::mem::take(&mut list.conversations)
                        .into_iter()
                        .filter(|c| !fresh.contains(&c.id) && oldest.is_some_and(|oldest| c.time().is_some_and(|d| d < oldest)))
                        .collect();
                    list.conversations = listing.conversations;
                    list.paged_at = None;
                    if asked {
                        list.fetched = Some(Instant::now());
                    }
                    if tail.is_empty() {
                        list.next = listing.next;
                    } else {
                        list.conversations.extend(tail);
                    }
                }
                list.error = None;
                // What is held says nothing of the providers: their failures stay until they answer.
                if asked || !listing.failures.is_empty() {
                    list.failures = (!listing.failures.is_empty()).then(|| listing.failures.join("; ").into());
                }
            }
            Err(error) => list.error = Some(error.to_string().into()),
        }
        if key == self.key() {
            self.rebuild_items();
            self.refresh_layers(cx);
        }
        self.load_previews(&key, cx);
        if again {
            self.read_list(key, cx);
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
                            if let Some(preview) = previews.get(&conversation.id) {
                                conversation.snippet = Some(preview.clone().unwrap_or_default());
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
        let mut items = Vec::with_capacity(pinned.len() + waiting.len() + rows.len() + 10);
        // Chat's unread spaces of the list's accounts sit over the bundles.
        let chats: Vec<UnreadSpace> = if bundles {
            self.chat_unread
                .iter()
                .filter(|space| match &key.source {
                    Source::Mailbox { account, .. } => *account == space.account.as_ref(),
                    Source::Unified(_) => true,
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if !chats.is_empty() {
            items.push(Item::Chat(Rc::new(ChatBundle::new(chats))));
        }
        items.extend(pinned.into_iter().map(|category| {
            let members = members.remove(&category).unwrap_or_default();
            Item::Bundle(Rc::new(Bundle::new(category, members, &list.conversations)))
        }));
        if !waiting.is_empty() {
            items.push(Item::Waiting(waiting.len()));
            items.extend(waiting.into_iter().map(Item::Row));
        }
        let mut current: Option<&String> = None;
        for index in rows {
            if sections {
                let section = &list.conversations[index].section;
                if current != Some(section) {
                    items.push(Item::Section(section.clone().into()));
                    current = Some(section);
                }
            }
            items.push(Item::Row(index));
        }
        // The row at the top stays there, wherever the new lines put it; at the list's very top,
        // the Chat bundle row arriving over it shows (MS-330).
        let top = self.list_scroll.logical_scroll_top();
        let at_start = top.item_ix == 0 && top.offset_in_item == px(0.);
        let kept = self.items.get(top.item_ix).map(|item| self.item_id(item));
        self.items = items;
        self.list_scroll.reset(self.items.len());
        let at = kept.and_then(|id| self.items.iter().position(|item| self.item_id(item) == id));
        let at = if at_start && matches!(self.items.first(), Some(Item::Chat(_))) { Some(0) } else { at };
        self.list_scroll.scroll_to(match at {
            Some(item_ix) => ListOffset { item_ix, offset_in_item: top.offset_in_item },
            None => ListOffset::default(),
        });
        self.trim_selection();
    }

    /// Drops a bundle layer whose bundle is gone, and keeps open readers' conversations current:
    /// one with new messages reads its thread again, showing the old one until it arrives (MS-79).
    fn refresh_layers(&mut self, cx: &mut Context<Self>) {
        let bundles: HashSet<BundleKind> = self
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Bundle(bundle) => Some(BundleKind::Mail(bundle.category)),
                Item::Chat(_) => Some(BundleKind::Chat),
                _ => None,
            })
            .collect();
        let mut layers = std::mem::take(&mut self.layers);
        let current: HashMap<&String, &Conversation> = self
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
        let arg = id.clone();
        let own = self.own_addresses();
        self.background(
            cx,
            async move {
                let thread = engine().show(arg, 200_000, true).await?;
                let messages: Vec<ShownMessage> =
                    thread.messages.into_iter().map(|message| ShownMessage::new(message, &own)).collect();
                Ok::<_, MailError>(messages)
            },
            move |this, result, cx| {
                let Some(thread) = this.threads.get_mut(&id).filter(|thread| thread.request == request) else { return };
                let mut loaded = None;
                match result {
                    Ok(messages) => {
                        let messages: Rc<[ShownMessage]> = messages.into();
                        thread.state = ThreadState::Loaded(messages.clone());
                        loaded = Some(messages);
                    }
                    Err(_) if matches!(thread.state, ThreadState::Loaded(_)) => {}
                    Err(error) => thread.state = ThreadState::Failed(error.to_string().into()),
                }
                if let Some(messages) = loaded {
                    this.read_invitations(&id, &messages, cx);
                    this.pin_opening(&id);
                }
                if this.forward_pending.as_ref() == Some(&id) {
                    this.forward_pending = None;
                    this.forward_loaded(&id, None, cx);
                }
                cx.notify();
            },
        );
    }

    /// Decides where the conversation just opened holds, once its messages are in: the head of
    /// its first unread message, else of its last; its top when that is its first message (a
    /// single mail always), where the title shows.
    pub(crate) fn pin_opening(&mut self, id: &str) {
        if self.reader_pin != ReaderPin::Opening || self.reader().is_none_or(|open| open.id != id) {
            return;
        }
        let Some(ThreadState::Loaded(messages)) = self.threads.get(id).map(|thread| &thread.state) else { return };
        let target = messages.iter().position(|message| message.unread).unwrap_or(messages.len().saturating_sub(1));
        self.reader_pin = match messages.get(target) {
            Some(message) if target > 0 => ReaderPin::Message(message.id.clone()),
            _ => ReaderPin::Free,
        };
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
