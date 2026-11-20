//! `MailApp`'s state and everything that changes it: what is selected, the lists served from
//! memory, the layers over the list, the reader's threads, and the optimistic actions that roll
//! back when the engine refuses them. Rendering is in `ui`.

mod actions;
mod invitations;
mod nav;
mod selection;
mod split;
mod tags;

pub use actions::Removal;
pub use invitations::InvitationCard;
pub use selection::Selection;
pub use tags::TagEditor;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::component::input::{Escape, InputEvent, InputState};
use gpui::component::menu::PopupMenu;
use gpui::component::notification::NotificationList;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, FocusHandle, Focusable, KeyBinding, ListAlignment, ListOffset,
    ListState, Modifiers, Pixels,
    Point, ScrollHandle, SharedString, Subscription, Task, Window, actions, px,
};
use mailstream::{Account, Category, Change, Conversation, Event, EventListener, ListQuery, Listing, Mailbox, MailboxRole};

use crate::bench::Bench;
use crate::calendar::{CalendarPane, CalendarToast};
use crate::chat::{ChatPane, MailWith, UnreadSpace};
use crate::compose::{ComposeColumn, ReplyBox};
use crate::engine::{MailError, engine};
use crate::frame_rate::FrameRateMeter;
use crate::model::{ConversationExt, RoleExt, ShownMessage};
use crate::swipe::{BackSwipe, DrawerSlide, MouseDrag, Preferences, Slide};

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
        EditTags,
        CloseTagEditor,
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
/// Key context of the mail list and reader (sibling of Compose, Chat, Calendar, and popovers).
/// Mail navigation and conversation shortcuts live here so they never fire while composing mail,
/// chatting, using the calendar, or editing tags (MS-412).
pub const BROWSE_CONTEXT: &str = "MailBrowse";
pub const SEARCH_CONTEXT: &str = "SearchField";
pub const TAG_EDITOR_CONTEXT: &str = "TagEditor";

/// Whether keyboard focus is currently inside a text field, inline reply, compose draft, search
/// field, or tag editor. Used as a defense-in-depth guard on `MailBrowse` action handlers so
/// menu-bar or programmatic dispatches cannot mutate mail while the user is typing (MS-412).
pub fn in_text_or_reply_context(window: &Window) -> bool {
    window.context_stack().iter().any(|ctx| {
        ctx.contains("Input")
            || ctx.contains("ReplyBox")
            || ctx.contains(SEARCH_CONTEXT)
            || ctx.contains("Compose")
            || ctx.contains(TAG_EDITOR_CONTEXT)
    })
}

pub fn bind_keys(cx: &mut App) {
    // Mail list and reader shortcuts require `MailBrowse` and stay out of any text field, inline
    // reply, search field, focused sidebar expander, or compact inline Settings overlay.
    let browse = Some("MailBrowse && !Input && !ReplyBox && !SearchField && !Expander && !Settings");
    let search = Some("MailBrowse && !ReplyBox && !Settings");
    let close_chat = Some("Chat && !Input && !ChatComposer && !VideoLightbox");
    let close_calendar = Some("Calendar && !Input && !EventEditor");
    let window = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", Next, browse),
        KeyBinding::new("down", Next, browse),
        KeyBinding::new("k", Previous, browse),
        KeyBinding::new("up", Previous, browse),
        KeyBinding::new("shift-down", ExtendNext, browse),
        KeyBinding::new("shift-up", ExtendPrevious, browse),
        KeyBinding::new(secondary!("a"), SelectAll, browse),
        KeyBinding::new("enter", Open, browse),
        KeyBinding::new("space", Open, browse),
        KeyBinding::new("escape", Back, browse),
        KeyBinding::new(secondary!("["), Back, browse),
        KeyBinding::new(secondary!("w"), Close, browse),
        KeyBinding::new(secondary!("w"), Close, close_chat),
        KeyBinding::new(secondary!("w"), Close, close_calendar),
        KeyBinding::new("e", Archive, browse),
        KeyBinding::new("ctrl-cmd-a", Archive, browse),
        KeyBinding::new(secondary!("backspace"), Trash, browse),
        KeyBinding::new("s", Star, browse),
        KeyBinding::new(secondary!("shift-l"), Star, browse),
        KeyBinding::new("u", ToggleRead, browse),
        KeyBinding::new(secondary!("shift-u"), ToggleRead, browse),
        KeyBinding::new("d", ToggleWaiting, browse),
        KeyBinding::new(secondary!("shift-d"), ToggleWaiting, browse),
        KeyBinding::new("l", EditTags, browse),
        KeyBinding::new("escape", CloseTagEditor, Some(TAG_EDITOR_CONTEXT)),
        KeyBinding::new(secondary!("z"), Undo, browse),
        KeyBinding::new("r", Reply, browse),
        KeyBinding::new(secondary!("r"), Reply, browse),
        KeyBinding::new("a", ReplyAll, browse),
        KeyBinding::new(secondary!("shift-r"), ReplyAll, browse),
        KeyBinding::new("f", Forward, browse),
        KeyBinding::new(secondary!("shift-f"), Forward, browse),
        KeyBinding::new(secondary!("n"), NewMessage, window),
        KeyBinding::new("/", FocusSearch, browse),
        KeyBinding::new(secondary!("f"), FocusSearch, search),
        KeyBinding::new(secondary!("shift-n"), Refresh, window),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, window),
        KeyBinding::new(secondary!("alt-p"), ToggleFrameRate, window),
        KeyBinding::new(secondary!("q"), Quit, None),
    ]);
}

/// Conversations asked for per page (the engine's most).
const PAGE: u32 = 100;
/// Rows before the end of a list or bundle at which the next older page begins loading.
pub(crate) const MORE_AHEAD: usize = 20;
/// Consecutive older-mail pages that add no visible lines to the current view (e.g. all folded
/// into Smart Inbox bundles) before automatic paging pauses and leaves the rest to the footer's
/// "Load older mail" button.
const AUTO_EMPTY_PAGES: u8 = 3;
/// A list this young is served from memory when shown again; the engine's change events reload
/// the one on screen sooner.
const FRESH: Duration = Duration::from_secs(90);
/// A search's first answer from each account: one batch of Gmail's metadata requests, which its
/// quota lets out at once (MS-352).
const FIRST_SCREEN: u32 = 25;

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
    /// One page of this list from the engine, prepared for display; a search's answers come
    /// from `MailApp::search_list`. `cached`: what the engine holds, at once, without asking a
    /// provider.
    async fn fetch(self, cursor: Option<String>, cached: bool) -> Result<Listing, MailError> {
        let account = self.source.account().cloned();
        let mailbox = match &self.source {
            Source::Unified(role) => role.as_str().to_owned(),
            Source::Mailbox { id, .. } => id.clone(),
        };
        let query = ListQuery { mailboxes: vec![mailbox], account, limit: PAGE, cursor, cached, ..ListQuery::default() };
        engine().list(query).await
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
    pub next: Option<String>,
    /// When the providers last answered for it.
    fetched: Option<Instant>,
    /// A read of what the engine holds (a search: any of its answers) is under way.
    pub loading: bool,
    /// The engine changed it while a read was under way: read again when that one lands.
    stale: bool,
    /// The providers are asked for its newest (`fetch_list`); what arrives meanwhile shows as
    /// the engine's change events bring it.
    pub fetching: bool,
    /// A page of older conversations (`load_more`) is under way.
    pub loading_more: bool,
    /// Consecutive pages that left the asking view's length unchanged (all folded into bundles).
    empty_pages: Option<(PagedView, usize, u8)>,
    /// Why the last older-mail page failed, shown in the footer until the next attempt.
    pub more_error: Option<SharedString>,
    /// The engine refused the last answer (no such mailbox yet).
    pub error: Option<SharedString>,
    /// The mailboxes whose providers failed when last asked.
    pub failures: Option<SharedString>,
    /// A search's answers so far (MS-352).
    search: Option<SearchRun>,
}

impl List {
    /// Whether `view` (currently `shown` lines long, excluding the footer) may ask for the next
    /// older page automatically (`manual = false`) or on click (`manual = true`).
    fn next_cursor(&self, view: PagedView, shown: usize, manual: bool) -> Option<String> {
        let cursor = self.next.as_ref()?;
        if self.loading_more {
            return None;
        }
        if !manual {
            if self.more_error.is_some() {
                return None;
            }
            if matches!(self.empty_pages, Some((v, len, count)) if v == view && len == shown && count >= AUTO_EMPTY_PAGES) {
                return None;
            }
        }
        Some(cursor.clone())
    }

    /// Merges a refreshed first page (`Arrival::Read` / `Arrival::Fetch`), keeping older pages
    /// already loaded below it and preserving `next` whenever the engine reports more mail.
    fn apply_first_page(&mut self, conversations: Vec<Conversation>, next: Option<String>, asked: bool) {
        let oldest = conversations.last().and_then(ConversationExt::time);
        let fresh: HashSet<String> = conversations.iter().map(|c| c.id.clone()).collect();
        let tail: Vec<Conversation> = std::mem::take(&mut self.conversations)
            .into_iter()
            .filter(|c| !fresh.contains(&c.id) && oldest.is_some_and(|oldest| c.time().is_none_or(|d| d <= oldest)))
            .collect();
        self.conversations = conversations;
        self.empty_pages = None;
        self.more_error = None;
        if asked {
            self.fetched = Some(Instant::now());
        }
        if tail.is_empty() {
            self.next = next;
        } else {
            self.conversations.extend(tail);
            if self.next.is_none() {
                self.next = next;
            }
        }
    }

    /// Appends an older page (`Arrival::More`), returning whether any new conversation was added.
    fn apply_more_page(&mut self, conversations: Vec<Conversation>, next: Option<String>, failures: &[String]) -> bool {
        let known: HashSet<String> = self.conversations.iter().map(|c| c.id.clone()).collect();
        let before = self.conversations.len();
        self.conversations.extend(conversations.into_iter().filter(|c| !known.contains(&c.id)));
        let added = self.conversations.len() > before;
        self.next = next;
        self.more_error = (!added && !failures.is_empty()).then(|| failures.join("; ").into());
        added
    }

    /// Updates consecutive-empty-page tracking after `Arrival::More` rebuilt `view`; returns
    /// whether automatic paging should immediately continue with the next page.
    fn finish_more_view(&mut self, view: PagedView, before_shown: usize, after_shown: usize) -> bool {
        if after_shown > before_shown || self.next.is_none() {
            self.empty_pages = None;
            return false;
        }
        let count = match self.empty_pages {
            Some((v, len, count)) if v == view && len == after_shown => count.saturating_add(1),
            _ => 1,
        };
        self.empty_pages = Some((view, after_shown, count));
        self.more_error.is_none() && count < AUTO_EMPTY_PAGES
    }
}

/// One run of a search (MS-352): the mail already loaded that matches shows at once, then each
/// account's provider answers on its own, a screenful (`FIRST_SCREEN`) first and then the rest,
/// each answer taking the place of that account's rows, so no account waits for another.
#[derive(Default)]
struct SearchRun {
    /// Which run this is; answers of an earlier one are dropped.
    run: u64,
    /// Answers still out.
    pending: usize,
    /// Accounts whose provider answered ("": every account, before the accounts are known):
    /// their rows are the provider's from then on.
    answered: HashSet<String>,
    /// Why each account's provider failed, from its latest answer.
    failures: BTreeMap<String, String>,
}

/// Which answer of a search arrived: the mail already loaded, or an account's provider's
/// (None: every account's) first screenful or the rest.
enum SearchStep {
    Held,
    First(Option<String>),
    Full(Option<String>),
}

/// What a list answer was asked as.
#[derive(Clone, Copy, PartialEq)]
enum Arrival {
    /// What the engine holds.
    Read,
    /// The providers' newest.
    Fetch,
    /// The page after the last, asked by `view` when it showed `shown` lines.
    More(PagedView, usize),
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
    /// Bottom footer while older mail can still be loaded ("Loading older mail…" / "Load older mail").
    More,
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
    Search,
    Reader(Box<Conversation>),
}

impl Layer {
    pub fn id(&self) -> SharedString {
        match self {
            Layer::Bundle(kind) => kind.id().into(),
            Layer::Search => "search".into(),
            Layer::Reader(conversation) => format!("reader:{}", conversation.id).into(),
        }
    }
}

/// The list a search lies over (MS-343): its own lines, scroll and highlight, drawn under the
/// search's layer, so a swipe that closes the search shows the list it goes back to, and back on
/// screen once the search ends. Its lines follow its list as the list on screen's do.
pub struct Beneath {
    pub source: Source,
    pub items: Vec<Item>,
    pub scroll: ListState,
    pub highlight: Option<String>,
}

impl Beneath {
    pub fn key(&self) -> ListKey {
        ListKey { source: self.source.clone(), query: None }
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
    /// The current list's lines: while a search is up, its results (on its layer).
    pub items: Vec<Item>,
    /// The list under the search's layer, while a search is up.
    pub beneath: Option<Beneath>,
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
    /// Whether each conversation's remote content loads on its own, as the engine last said
    /// (`mail.remote`), and the conversations whose remote content the person loaded (MS-364).
    pub remote: HashMap<String, mailstream::RemoteContent>,
    pub remote_loaded: HashSet<String>,
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
    /// The search results drawn since the search started: only their previews load (MS-352).
    drawn_rows: HashSet<String>,
    /// The window, for the toasts (`show_toast`) asked for from a task without one at hand.
    pub window: AnyWindowHandle,
    /// Toasts over the window, painted in a deferred layer above any native Mac web view.
    pub notifications: Entity<NotificationList>,
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
    /// Animated slide-over navigation drawer state in compact (small-screen) mode (MS-439).
    pub drawer_slide: DrawerSlide,
    /// Last laid-out compact drawer width in logical points.
    pub drawer_width: f32,
    /// Whether a left-edge swipe may open the drawer on the current compact screen.
    pub drawer_edge_open_allowed: bool,
    /// Two-finger / Android touch swipe gesture for the compact navigation drawer.
    pub drawer_swipe: BackSwipe,
    /// Mouse drag gesture for the compact navigation drawer (panel, scrim, or left edge).
    pub drawer_drag: MouseDrag,
    /// Drawer offset (`[-drawer_width, 0.0]`) when the current drawer gesture started.
    pub drawer_gesture_base: std::cell::Cell<f32>,
    /// Whether the drawer was logically open when the current drawer gesture started.
    pub drawer_gesture_started_open: std::cell::Cell<bool>,
    /// Whether the inline search bar is expanded in compact (small-screen) mode.
    pub compact_search: bool,
    /// Inline Settings overlay shown on compact viewports and single-window mobile hosts (Android).
    pub(crate) settings: Option<Entity<crate::settings::Settings>>,
    pub swipe: BackSwipe,
    /// A mouse drag on a layer (MS-266).
    pub drag: MouseDrag,
    /// How the sliding layer sits over what lies beneath.
    pub slide: Slide,
    /// The layer `slide` belongs to: the top one, or the one a gesture took under the pointer
    /// (MS-299); a layer gone or replaced takes the top one at rest.
    pub slide_layer: Option<SharedString>,
    /// The layer the gesture in progress closes: the one under the pointer as it began (MS-299).
    pub gesture_layer: std::cell::RefCell<Option<SharedString>>,
    /// How far a layer over the list's island travels between in and out, as last laid out.
    pub content_width: f32,
    /// A conversation beside the list, as last laid out (two panes).
    pub beside: Option<Beside>,
    pub context_menu: Option<ContextMenu>,
    /// The tag editor (MS-339), over everything while it is open.
    pub tag_editor: Option<TagEditor>,
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
        let compose = cx.new(ComposeColumn::new);
        let notifications = cx.new(|cx| NotificationList::new(window, cx));
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
            // A direct message's "Mail with …" searches mail for the other person (MS-331).
            cx.subscribe_in(&chat, window, |this, _, MailWith(address): &MailWith, window, cx| {
                this.search_mail_with(address.clone(), window, cx)
            }),
            cx.subscribe(&chat, |this, _, _: &crate::chat::ChatOpenDrawer, cx| this.open_drawer(cx)),
            cx.subscribe(&chat, |this, _, toast: &crate::chat::ChatToast, cx| this.show_toast(toast.0.clone(), cx)),
            cx.subscribe(&calendar, |this, _, toast: &CalendarToast, cx| this.show_toast(toast.0.clone(), cx)),
            cx.subscribe(&calendar, |this, _, _: &crate::calendar::CalendarOpenDrawer, cx| this.open_drawer(cx)),
            cx.observe(&calendar, |_, _, cx| cx.notify()),
            // The column's placement and width lay the window out.
            cx.observe(&compose, |_, _, cx| cx.notify()),
            cx.subscribe_in(&compose, window, Self::on_compose_event),
            cx.observe(&notifications, |_, _, cx| cx.notify()),
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
            beneath: None,
            highlight: None,
            bundle_highlight: None,
            layers: Vec::new(),
            threads: HashMap::new(),
            unfolded: HashSet::new(),
            quotes_open: HashSet::new(),
            headers_open: HashSet::new(),
            remote: HashMap::new(),
            remote_loaded: HashSet::new(),
            invitations: HashMap::new(),
            reader_pin: ReaderPin::Free,
            reader_top: ReaderTop::Top,
            hovered_message: None,
            thread_requests: 0,
            removing: HashSet::new(),
            selection: Selection::default(),
            previews_asking: false,
            previews_again: false,
            drawn_rows: HashSet::new(),
            window: window.window_handle(),
            hovered_row: None,
            list_scroll: Self::new_list_scroll(PagedView::List, cx),
            bundle_scroll: Self::new_list_scroll(PagedView::Bundle, cx),
            reader_scroll: ScrollHandle::new(),
            expanded: ["unified:inbox", "chat", "section:accounts"].into_iter().map(SharedString::from).collect(),
            sidebar_open: !cfg!(target_os = "android"),
            sidebar_folded: false,
            sidebar_beside_compose: false,
            drawer_slide: DrawerSlide::Closed,
            drawer_width: crate::design::compact::DRAWER_WIDTH,
            drawer_edge_open_allowed: false,
            drawer_swipe: BackSwipe::default(),
            drawer_drag: MouseDrag::default(),
            drawer_gesture_base: std::cell::Cell::new(-crate::design::compact::DRAWER_WIDTH),
            drawer_gesture_started_open: std::cell::Cell::new(false),
            compact_search: false,
            settings: None,
            swipe: BackSwipe::default(),
            drag: MouseDrag::default(),
            slide: Slide::Rest,
            slide_layer: None,
            gesture_layer: std::cell::RefCell::new(None),
            content_width: 0.0,
            beside: None,
            context_menu: None,
            tag_editor: None,
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
            notifications,
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
        this.background(cx, async { Preferences::read() }, |this, preferences, _| {
            this.swipe.preferences = preferences;
            this.drawer_swipe.preferences = preferences;
        });
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
                        // A sender trusted here or on another device (`remoteContent.trustedSenders`).
                        let open: Vec<String> = this
                            .layers
                            .iter()
                            .filter_map(|layer| match layer {
                                Layer::Reader(conversation) => Some(conversation.id.clone()),
                                _ => None,
                            })
                            .collect();
                        for id in open {
                            this.check_remote(id, cx);
                        }
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
                        // A search on screen stays as answered (MS-352): asking the providers
                        // again on every sync spent Gmail's quota and kept the spinner going. A
                        // refresh or another search asks again.
                        if key.query.is_none() {
                            this.read_list(key, cx);
                        } else if batch.contains(&Event::Previews) {
                            this.load_previews(&key, cx);
                        }
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
                    this.refresh_reply_plan(cx);
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

    /// Reads `key`'s first page as the engine holds it; a search runs anew (`search_list`). A read
    /// asked for while one is under way runs again once that one lands.
    fn read_list(&mut self, key: ListKey, cx: &mut Context<Self>) {
        if key.query.is_some() {
            self.search_list(key, cx);
            return;
        }
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

    /// Runs `key`'s search (MS-352): the mail the engine holds that matches shows at once; each
    /// account's provider then answers on its own, a screenful first, each answer showing as it
    /// lands. Answers of an earlier run of it are dropped.
    fn search_list(&mut self, key: ListKey, cx: &mut Context<Self>) {
        let Some(query) = key.query.as_ref().map(SharedString::to_string) else { return };
        let accounts: Vec<Option<String>> = match key.source.account() {
            Some(account) => vec![Some(account.clone())],
            None if self.mailboxes.is_empty() => vec![None],
            None => {
                let mut accounts: Vec<Option<String>> = self.mailboxes.keys().cloned().map(Some).collect();
                accounts.sort();
                accounts
            }
        };
        let list = self.lists.entry(key.clone()).or_default();
        let run = list.search.as_ref().map_or(0, |search| search.run) + 1;
        list.search = Some(SearchRun { run, pending: 1 + 2 * accounts.len(), ..SearchRun::default() });
        list.loading = true;
        cx.notify();
        let held = engine().search(query, key.source.account().cloned(), PAGE, true, false);
        let held_key = key.clone();
        self.background(cx, held, move |this, result, cx| this.search_arrived(held_key, run, SearchStep::Held, result, cx));
        for account in accounts {
            self.ask_search(key.clone(), run, account, true, cx);
        }
    }

    /// Asks `account`'s provider (None: every account's) for run `run` of `key`'s search: a
    /// screenful, quickly, when `first`, else all of it.
    fn ask_search(&mut self, key: ListKey, run: u64, account: Option<String>, first: bool, cx: &mut Context<Self>) {
        let Some(query) = key.query.as_ref().map(SharedString::to_string) else { return };
        let limit = if first { FIRST_SCREEN } else { PAGE };
        let asked = engine().search(query, account.clone(), limit, false, first);
        let step = if first { SearchStep::First(account) } else { SearchStep::Full(account) };
        self.background(cx, asked, move |this, result, cx| this.search_arrived(key, run, step, result, cx));
    }

    /// One answer of run `run` of `key`'s search: the mail held stands in for the accounts no
    /// provider answered yet; a provider's answer takes the place of its account's rows, and its
    /// first screenful asks for the rest.
    fn search_arrived(
        &mut self,
        key: ListKey,
        run: u64,
        step: SearchStep,
        result: Result<Listing, MailError>,
        cx: &mut Context<Self>,
    ) {
        let Some(list) = self.lists.get_mut(&key) else { return };
        let Some(search) = list.search.as_mut().filter(|search| search.run == run) else { return };
        search.pending -= 1;
        let removing = &self.removing;
        let first = matches!(step, SearchStep::First(_));
        let mut rest = None;
        match step {
            // What is held only gives the providers' answers a head start: a failure shows nothing.
            SearchStep::Held => {
                if let Ok(mut listing) = result
                    && !search.answered.contains("")
                {
                    let answered = &search.answered;
                    listing.conversations.retain(|c| !answered.contains(&c.account) && !removing.contains(&c.id));
                    merge_answer(&mut list.conversations, listing.conversations, |c| !answered.contains(&c.account));
                }
            }
            SearchStep::First(account) | SearchStep::Full(account) => {
                let name = account.clone().unwrap_or_default();
                search.failures.remove(&name);
                match result {
                    Ok(mut listing) => {
                        if !listing.failures.is_empty() {
                            search.failures.insert(name.clone(), listing.failures.join("; "));
                        }
                        search.answered.insert(name);
                        listing.conversations.retain(|c| !removing.contains(&c.id));
                        merge_answer(&mut list.conversations, listing.conversations, |c| {
                            account.as_ref().is_none_or(|account| *account == c.account)
                        });
                        if first {
                            rest = Some(account);
                        }
                    }
                    Err(error) => {
                        search.failures.insert(name, error.to_string());
                        // Nor is its rest asked for.
                        if first {
                            search.pending -= 1;
                        }
                    }
                }
            }
        }
        list.loading = search.pending > 0;
        list.failures =
            (!search.failures.is_empty()).then(|| search.failures.values().cloned().collect::<Vec<_>>().join("; ").into());
        if search.pending == 0 && !search.answered.is_empty() {
            list.fetched = Some(Instant::now());
        }
        list.error = None;
        if let Some(account) = rest {
            self.ask_search(key.clone(), run, account, false, cx);
        }
        if key == self.key() {
            self.rebuild_items();
            self.refresh_layers(cx);
        }
        self.load_previews(&key, cx);
        cx.notify();
    }

    /// Search result line `ix` of the list on screen was drawn: its preview loads if it has none
    /// (MS-352), so Gmail's quota goes on the rows looked at.
    pub fn row_drawn(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Item::Row(index)) = self.items.get(ix) else { return };
        let Some(conversation) = self.list().and_then(|list| list.conversations.get(*index)) else { return };
        if conversation.snippet.is_none() && !self.drawn_rows.contains(&conversation.id) {
            self.drawn_rows.insert(conversation.id.clone());
            self.load_previews(&self.key(), cx);
        }
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

    /// A fresh `ListState` for `view` whose scroll handler pages older mail near the bottom even
    /// when `gpui-fast`'s scroll layer reuses cached rows without re-invoking `render_item`.
    pub(crate) fn new_list_scroll(view: PagedView, cx: &mut Context<Self>) -> ListState {
        let state = list_state(0);
        let weak = cx.weak_entity();
        state.set_scroll_handler(move |event, _, cx| {
            if event.count > 0 && event.visible_range.end + MORE_AHEAD >= event.count {
                // Read-only precheck so scrolling near the bottom (or in short mailboxes with no
                // next page) never bumps MailApp's entity write generation and invalidates Scroll
                // Layers on every frame.
                let can_load = weak
                    .read_with(cx, |this, _| {
                        let shown = this.view_item_count(view);
                        this.lists
                            .get(&this.key())
                            .and_then(|list| list.next_cursor(view, shown, false))
                            .is_some()
                    })
                    .unwrap_or(false);
                if can_load {
                    let _ = weak.update(cx, |this, cx| {
                        let shown = this.view_item_count(view);
                        this.load_more(view, shown, false, cx);
                    });
                }
            }
        });
        state
    }

    /// Visible line count of `view`, excluding the trailing `More` footer.
    pub fn view_item_count(&self, view: PagedView) -> usize {
        match view {
            PagedView::List => self.items.iter().filter(|item| !matches!(item, Item::More)).count(),
            PagedView::Bundle => match self.open_bundle() {
                Some(BundleKind::Mail(category)) => self.bundle_members(category).map_or(0, |b| b.members.len()),
                _ => 0,
            },
        }
    }

    /// Asks for the page after the current list's last, when there is one and none is loading.
    /// `shown` is the length of the view whose end the user reached (excluding the footer);
    /// `manual` is true when triggered by clicking the footer's "Load older mail" button.
    pub fn load_more(&mut self, view: PagedView, shown: usize, manual: bool, cx: &mut Context<Self>) {
        let key = self.key();
        let Some(list) = self.lists.get_mut(&key) else { return };
        let Some(cursor) = list.next_cursor(view, shown, manual) else { return };
        list.loading_more = true;
        list.more_error = None;
        if manual {
            list.empty_pages = None;
        }
        cx.notify();
        self.background(cx, key.clone().fetch(Some(cursor), false), move |this, result, cx| {
            this.list_arrived(key, result, Arrival::More(view, shown), cx)
        });
    }

    fn list_arrived(&mut self, key: ListKey, result: Result<Listing, MailError>, arrival: Arrival, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        match arrival {
            Arrival::Read => list.loading = false,
            Arrival::Fetch => list.fetching = false,
            Arrival::More(_, _) => list.loading_more = false,
        }
        let again = arrival == Arrival::Read && std::mem::take(&mut list.stale);
        // The providers answered.
        let asked = arrival == Arrival::Fetch;
        let mut more_succeeded = false;
        match result {
            Ok(mut listing) => {
                listing.conversations.retain(|c| !self.removing.contains(&c.id));
                match arrival {
                    Arrival::More(_, _) => {
                        list.apply_more_page(listing.conversations, listing.next, &listing.failures);
                        more_succeeded = true;
                    }
                    Arrival::Read | Arrival::Fetch => {
                        list.apply_first_page(listing.conversations, listing.next, asked);
                    }
                }
                list.error = None;
                // What is held says nothing of the providers: their failures stay until they answer.
                if asked || !listing.failures.is_empty() {
                    list.failures = (!listing.failures.is_empty()).then(|| listing.failures.join("; ").into());
                }
            }
            Err(error) => match arrival {
                Arrival::More(_, _) => list.more_error = Some(error.to_string().into()),
                Arrival::Read | Arrival::Fetch => list.error = Some(error.to_string().into()),
            },
        }
        let current = key == self.key();
        if current {
            self.rebuild_items();
            self.refresh_layers(cx);
        } else if self.beneath.as_ref().is_some_and(|beneath| beneath.key() == key) {
            self.rebuild_beneath();
        }
        self.load_previews(&key, cx);
        if again {
            self.read_list(key.clone(), cx);
        }
        if current && more_succeeded && let Arrival::More(view, before_shown) = arrival {
            let after_shown = self.view_item_count(view);
            let keep_going = self
                .lists
                .get_mut(&key)
                .is_some_and(|list| list.finish_more_view(view, before_shown, after_shown));
            if keep_going {
                self.load_more(view, after_shown, false, cx);
            }
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
        // A search's load only for rows drawn: most of its results are never looked at (MS-352).
        let ids: Vec<String> = list
            .conversations
            .iter()
            .filter(|c| c.snippet.is_none() && (key.query.is_none() || self.drawn_rows.contains(&c.id)))
            .map(|c| c.id.to_string())
            .take(PAGE as usize)
            .collect();
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

    /// The current list's lines, and the list's under a search (MS-343), which index the same
    /// lists: whatever changes a list rebuilds both.
    pub fn rebuild_items(&mut self) {
        let key = self.key();
        let lines = self.lines(&key);
        relist(&self.list_scroll, &mut self.items, lines, self.lists.get(&key));
        self.rebuild_beneath();
        self.trim_selection();
    }

    /// The lines of the list under a search, if one is up.
    fn rebuild_beneath(&mut self) {
        let Some(key) = self.beneath.as_ref().map(Beneath::key) else { return };
        let lines = self.lines(&key);
        if let Some(beneath) = &mut self.beneath {
            relist(&beneath.scroll, &mut beneath.items, lines, self.lists.get(&key));
        }
    }

    /// `key`'s lines, if its list is loaded. In the Smart Inbox service mail folds into bundles
    /// pinned above the rest, newest bundle first, and what waits on the user's reply or action
    /// follows under its own header, as in the native app's Smart Inbox; date headers group the
    /// remaining rows.
    fn lines(&self, key: &ListKey) -> Option<Vec<Item>> {
        let bundles = key.bundles();
        let sections = key.query.is_none();
        let list = self.lists.get(key)?;
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
        if key.query.is_none() && list.next.is_some() && !list.conversations.is_empty() {
            items.push(Item::More);
        }
        Some(items)
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
    /// fails. Whether its remote content loads is asked again either way.
    pub(crate) fn load_thread(&mut self, conversation: &Conversation, again: bool, cx: &mut Context<Self>) {
        self.check_remote(conversation.id.clone(), cx);
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
                    if this.reply.as_ref().is_some_and(|reply| reply.conversation == id) {
                        this.refresh_reply_plan(cx);
                    }
                }
                if this.forward_pending.as_ref() == Some(&id) {
                    this.forward_pending = None;
                    this.forward_loaded(&id, None, cx);
                }
                cx.notify();
            },
        );
    }

    /// Asks the engine whether conversation `id`'s remote images, stylesheets and fonts load on
    /// their own (`mail.remote`, MS-364); until it says, the reader holds them back.
    pub(crate) fn check_remote(&mut self, id: String, cx: &mut Context<Self>) {
        let arg = id.clone();
        self.background(cx, engine().remote_content(arg), move |this, result, cx| {
            let Ok(remote) = result else { return };
            if this.remote.get(&id) != Some(&remote) {
                this.remote.insert(id, remote);
                cx.notify();
            }
        });
    }

    /// Whether the reader lets conversation `id`'s bodies read the web: the engine says its
    /// senders are known, or the person loaded it.
    pub fn remote_allowed(&self, id: &str) -> bool {
        self.remote_loaded.contains(id) || self.remote.get(id).is_some_and(|remote| remote.allowed)
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

/// The setting of the senders whose remote content loads on its own.
const TRUSTED_SENDERS: &str = "remoteContent.trustedSenders";

/// A fresh list of `count` rows, scrolled to the top.
pub fn list_state(count: usize) -> ListState {
    ListState::new(count, ListAlignment::Top, px(400.))
}

/// Puts `arrived`, a search's answer, in place of the rows of `rows` it `answers_for`, newest
/// first; the previews already loaded stay with their conversations (MS-352).
fn merge_answer(rows: &mut Vec<Conversation>, mut arrived: Vec<Conversation>, answers_for: impl Fn(&Conversation) -> bool) {
    let (replaced, kept): (Vec<Conversation>, Vec<Conversation>) = std::mem::take(rows).into_iter().partition(|c| answers_for(c));
    let mut previews: HashMap<String, String> = replaced.into_iter().filter_map(|c| Some((c.id, c.snippet?))).collect();
    for conversation in &mut arrived {
        if conversation.snippet.is_none() {
            conversation.snippet = previews.remove(&conversation.id);
        }
    }
    *rows = kept;
    rows.extend(arrived);
    rows.sort_by_cached_key(|c| std::cmp::Reverse(c.time()));
}

/// Puts `lines` (none: the list isn't loaded) in place of `shown`, the lines `scroll` shows of
/// `list`. The row at the top stays there, wherever the new lines put it; at the list's very top,
/// the Chat bundle row arriving over it shows (MS-330).
fn relist(scroll: &ListState, shown: &mut Vec<Item>, lines: Option<Vec<Item>>, list: Option<&List>) {
    let Some(lines) = lines else {
        shown.clear();
        scroll.reset(0);
        return;
    };
    let top = scroll.logical_scroll_top();
    let at_start = top.item_ix == 0 && top.offset_in_item == px(0.);
    let kept = shown.get(top.item_ix).map(|item| line_id(item, list));
    *shown = lines;
    scroll.reset(shown.len());
    let at = kept.and_then(|id| shown.iter().position(|item| line_id(item, list) == id));
    let at = if at_start && matches!(shown.first(), Some(Item::Chat(_))) { Some(0) } else { at };
    scroll.scroll_to(match at {
        Some(item_ix) => ListOffset { item_ix, offset_in_item: top.offset_in_item },
        None => ListOffset::default(),
    });
}

/// The id of `item`, a line of `list`: a conversation's, a bundle's or a header's.
pub fn line_id(item: &Item, list: Option<&List>) -> String {
    match item {
        Item::Section(title) => format!("section:{title}"),
        Item::Waiting(_) => "section:waiting".into(),
        Item::Row(index) => list.and_then(|l| l.conversations.get(*index)).map(|c| c.id.clone()).unwrap_or_default(),
        Item::Bundle(bundle) => BundleKind::Mail(bundle.category).id(),
        Item::Chat(_) => BundleKind::Chat.id(),
        Item::More => "footer:more".into(),
    }
}

/// Hands the engine's change events to the window's task.
struct Changes(UnboundedSender<Event>);

impl EventListener for Changes {
    fn on_event(&self, change: Change) {
        let _ = self.0.unbounded_send(change.event);
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;

    fn conversation(id: &str, date: Option<&str>) -> Conversation {
        Conversation {
            id: id.into(),
            account: "me@example.com".into(),
            subject: id.into(),
            date: date.map(String::from),
            from: None,
            from_me: false,
            peers: Vec::new(),
            participants: Vec::new(),
            message_count: 1,
            unread: false,
            starred: false,
            has_attachment: false,
            mailboxes: vec![MailboxRole::Inbox],
            snippet: None,
            category: Some(Category::People),
            waiting: false,
            labels: Vec::new(),
            started: false,
            tag: String::new(),
            title: id.into(),
            when: String::new(),
            section: String::new(),
            avatar: Default::default(),
        }
    }

    #[test]
    fn bundled_pages_auto_continue_up_to_cap_then_allow_manual_load() {
        let mut list = List {
            conversations: vec![conversation("c0", Some("2026-11-19T12:00:00Z"))],
            next: Some("cur1".into()),
            ..List::default()
        };
        // Suppose Smart Inbox keeps visible length at 20 across 3 pages of bundled mail.
        for step in 1..=AUTO_EMPTY_PAGES {
            assert_eq!(list.next_cursor(PagedView::List, 20, false).as_deref(), Some(format!("cur{step}").as_str()));
            list.apply_more_page(
                vec![conversation(&format!("b{step}"), Some("2026-11-18T12:00:00Z"))],
                Some(format!("cur{}", step + 1)),
                &[],
            );
            let keep_going = list.finish_more_view(PagedView::List, 20, 20);
            assert_eq!(keep_going, step < AUTO_EMPTY_PAGES);
        }
        // Automatic paging pauses at the cap, while manual "Load older mail" still proceeds.
        assert_eq!(list.next_cursor(PagedView::List, 20, false), None);
        assert_eq!(list.next_cursor(PagedView::List, 20, true).as_deref(), Some("cur4"));
        // Once a page adds visible rows, automatic paging resumes.
        assert!(!list.finish_more_view(PagedView::List, 20, 25));
        assert_eq!(list.next_cursor(PagedView::List, 25, false).as_deref(), Some("cur4"));
    }

    #[test]
    fn more_error_pauses_auto_paging_and_first_page_keeps_same_second_and_undated_tail() {
        let mut list = List {
            conversations: vec![
                conversation("c1", Some("2026-11-19T10:00:00Z")),
                conversation("c2", Some("2026-11-19T10:00:00Z")),
                conversation("c3", None),
            ],
            next: Some("cur_old".into()),
            ..List::default()
        };
        // Empty page with provider failure preserves `next`, records `more_error`, pauses auto,
        // and still allows manual retry.
        list.apply_more_page(Vec::new(), Some("cur_old".into()), &["gmail: timeout".into()]);
        assert!(list.more_error.is_some());
        assert_eq!(list.next_cursor(PagedView::List, 3, false), None);
        assert_eq!(list.next_cursor(PagedView::List, 3, true).as_deref(), Some("cur_old"));

        // A refreshed first page whose oldest shares the same second as `c2` keeps both `c2` and
        // undated `c3` in the tail.
        list.next = None;
        list.apply_first_page(vec![conversation("c1", Some("2026-11-19T10:00:00Z"))], Some("cur_fresh".into()), true);
        let ids: Vec<&str> = list.conversations.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "c2", "c3"]);
        assert_eq!(list.next.as_deref(), Some("cur_fresh"));
        assert!(list.more_error.is_none());
    }
}
