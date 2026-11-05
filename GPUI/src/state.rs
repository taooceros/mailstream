//! `MailApp`'s state and everything that changes it: what is selected, the lists served from
//! memory, the layers over the list, the reader's threads, and the optimistic actions that roll
//! back when the app refuses them. Rendering is in `ui`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, KeyBinding, ScrollHandle, ScrollStrategy, SharedString,
    Subscription, UniformListScrollHandle, Window, actions,
};
use serde_json::{Value, json};

use crate::control::{self, ControlError};
use crate::format;
use crate::input::{InputEvent, TextInput};
use crate::model::{Account, Category, Conversation, Listing, Mailbox, Message, Role, Thread};

actions!(
    mail,
    [Next, Previous, Open, Back, Archive, Trash, Star, ToggleRead, FocusSearch, Refresh, Quit]
);

pub const CONTEXT: &str = "Mail";

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the list, never while the search field has focus.
    let list = Some("Mail && !TextInput");
    let window = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("space", Open, list),
        KeyBinding::new("escape", Back, list),
        KeyBinding::new("cmd-[", Back, window),
        KeyBinding::new("e", Archive, list),
        KeyBinding::new("backspace", Trash, list),
        KeyBinding::new("delete", Trash, list),
        KeyBinding::new("s", Star, list),
        KeyBinding::new("u", ToggleRead, list),
        KeyBinding::new("/", FocusSearch, list),
        KeyBinding::new("cmd-f", FocusSearch, window),
        KeyBinding::new("cmd-r", Refresh, window),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
}

/// Conversations asked for per page.
const PAGE: usize = 100;
/// A list or count this young is served from memory without asking the app again.
const FRESH: Duration = Duration::from_secs(90);
/// How often stale lists and counts are looked at.
const POLL: Duration = Duration::from_secs(30);
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
    fn request(&self, cursor: Option<&str>) -> (&'static str, Value) {
        let account = self.source.account().map(|a| a.to_string());
        if let Some(query) = &self.query {
            return ("mail.search", json!({ "query": query.as_ref(), "account": account, "limit": PAGE }));
        }
        let mailbox = match &self.source {
            Source::Unified(role) => role.arg().to_owned(),
            Source::Mailbox { id, .. } => id.to_string(),
        };
        let mut args = json!({ "mailbox": [mailbox], "account": account, "limit": PAGE });
        if let Some(cursor) = cursor {
            args["cursor"] = cursor.into();
        }
        ("mail.list", args)
    }

    /// The Smart Inbox folds bundles in the inbox, outside search.
    pub fn bundles(&self) -> bool {
        self.query.is_none() && self.source.role() == Role::Inbox
    }
}

#[derive(Default)]
pub struct List {
    pub conversations: Vec<Conversation>,
    /// The following page's cursor, from apps that page.
    next: Option<String>,
    fetched: Option<Instant>,
    pub loading: bool,
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
    /// An index into the list's conversations.
    Row(usize),
    /// Service mail of one category, folded at its newest member's place.
    Bundle(Rc<Bundle>),
}

pub struct Bundle {
    pub category: Category,
    /// Indexes into the list's conversations, newest first.
    pub members: Vec<usize>,
    /// The latest senders, for the row.
    pub senders: SharedString,
    pub unread: bool,
}

impl Bundle {
    fn new(category: Category, members: Vec<usize>, conversations: &[Conversation]) -> Self {
        let mut senders: Vec<&str> = Vec::new();
        for &ix in &members {
            let sender = conversations[ix].sender.as_ref();
            if !senders.contains(&sender) {
                senders.push(sender);
            }
            if senders.len() == 4 {
                break;
            }
        }
        Self {
            category,
            unread: members.iter().any(|&ix| conversations[ix].unread),
            senders: senders.join(", ").into(),
            members,
        }
    }
}

/// What lies over the list, bottom to top.
#[derive(Clone)]
pub enum Layer {
    Bundle(Category),
    Reader(Box<Conversation>),
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
}

pub struct Toast {
    pub text: SharedString,
    generation: u64,
}

pub struct MailApp {
    pub focus: FocusHandle,
    pub search: Entity<TextInput>,
    pub accounts: Vec<Account>,
    pub mailboxes: HashMap<SharedString, Vec<Mailbox>>,
    mailboxes_fetched: Option<Instant>,
    /// Set when the app could not be reached at all.
    pub offline: Option<SharedString>,
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
    /// Conversations being archived or trashed: refreshes that still list them leave them out.
    removing: HashSet<SharedString>,
    previews_supported: bool,
    pub toast: Option<Toast>,
    /// The row under the pointer, which shows its actions.
    pub hovered_row: Option<SharedString>,
    toast_generation: u64,
    pub list_scroll: UniformListScrollHandle,
    pub bundle_scroll: UniformListScrollHandle,
    pub reader_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl MailApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search", cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            search,
            accounts: Vec::new(),
            mailboxes: HashMap::new(),
            mailboxes_fetched: None,
            offline: None,
            source: Source::Unified(Role::Inbox),
            query: None,
            lists: HashMap::new(),
            items: Vec::new(),
            highlight: None,
            bundle_highlight: None,
            layers: Vec::new(),
            threads: HashMap::new(),
            unfolded: HashSet::new(),
            removing: HashSet::new(),
            previews_supported: true,
            toast: None,
            hovered_row: None,
            toast_generation: 0,
            list_scroll: UniformListScrollHandle::new(),
            bundle_scroll: UniformListScrollHandle::new(),
            reader_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.load_mailboxes(cx);
        this.load_list(this.key(), false, cx);
        this.poll(cx);
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
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    // MARK: - Loading

    fn poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this.update(cx, |this, cx| {
                    this.load_list(this.key(), false, cx);
                    if this.mailboxes_fetched.is_none_or(|at| at.elapsed() >= FRESH) {
                        this.load_mailboxes(cx);
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
        self.mailboxes_fetched = Some(Instant::now());
        self.background(
            cx,
            || -> Result<_, ControlError> {
                let accounts: Vec<Account> = control::call("accounts.list", json!({}))?;
                let mailboxes: HashMap<SharedString, Vec<Mailbox>> = control::call("mailboxes", json!({}))?;
                Ok((accounts, mailboxes))
            },
            |this, result, cx| {
                match result {
                    Ok((accounts, mailboxes)) => {
                        let was_offline = this.offline.take().is_some();
                        this.accounts = accounts;
                        this.mailboxes = mailboxes;
                        if was_offline {
                            this.load_list(this.key(), true, cx);
                        }
                    }
                    Err(error) => {
                        this.mailboxes_fetched = None;
                        if error.code == "unreachable" {
                            this.offline = Some(error.message.into());
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Asks for `key`'s first page unless a fresh copy is in memory (or `force`).
    pub fn load_list(&mut self, key: ListKey, force: bool, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        if list.loading || (!force && list.fetched.is_some_and(|at| at.elapsed() < FRESH)) {
            return;
        }
        list.loading = true;
        cx.notify();
        let (command, args) = key.request(None);
        self.background(cx, move || fetch(command, args), move |this, result, cx| this.list_arrived(key, result, false, cx));
    }

    /// Asks for the page after the current list's last, when the app pages and none is loading.
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
        let (command, args) = key.request(Some(&cursor));
        self.background(cx, move || fetch(command, args), move |this, result, cx| this.list_arrived(key, result, true, cx));
    }

    fn list_arrived(&mut self, key: ListKey, result: Result<Listing, ControlError>, more: bool, cx: &mut Context<Self>) {
        let list = self.lists.entry(key.clone()).or_default();
        list.loading = false;
        list.loading_more = false;
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
                self.offline = None;
            }
            Err(error) => {
                if error.code == "unreachable" {
                    self.offline = Some(error.message.clone().into());
                }
                list.error = Some(error.message.into());
            }
        }
        if key == self.key() {
            self.rebuild_items();
            self.refresh_layers();
        }
        self.load_previews(&key, cx);
        cx.notify();
    }

    /// Previews missing from a listing, from apps that offer `mail.previews`.
    fn load_previews(&mut self, key: &ListKey, cx: &mut Context<Self>) {
        if !self.previews_supported {
            return;
        }
        let Some(list) = self.lists.get(key) else { return };
        let ids: Vec<String> =
            list.conversations.iter().filter(|c| c.snippet.is_none()).map(|c| c.id.to_string()).take(PAGE).collect();
        if ids.is_empty() {
            return;
        }
        self.background(
            cx,
            move || control::call::<HashMap<SharedString, Option<SharedString>>>("mail.previews", json!({ "ids": ids })),
            |this, result, cx| match result {
                Ok(previews) => {
                    for list in this.lists.values_mut() {
                        for conversation in &mut list.conversations {
                            if let Some(Some(preview)) = previews.get(&conversation.id) {
                                conversation.snippet = Some(preview.clone());
                            }
                        }
                    }
                    cx.notify();
                }
                Err(error) if error.code == "unknown_command" || error.code == "usage" => {
                    this.previews_supported = false;
                }
                Err(_) => {}
            },
        );
    }

    /// The current list's lines: bundles folded at their newest member's place, date headers.
    pub fn rebuild_items(&mut self) {
        let key = self.key();
        let bundles = key.bundles();
        let sections = key.query.is_none();
        let Some(list) = self.lists.get(&key) else {
            self.items.clear();
            return;
        };
        let mut lines: Vec<(usize, Option<Category>)> = Vec::with_capacity(list.conversations.len());
        let mut members: HashMap<Category, Vec<usize>> = HashMap::new();
        for (index, conversation) in list.conversations.iter().enumerate() {
            match conversation.bundle().filter(|_| bundles) {
                None => lines.push((index, None)),
                Some(category) => {
                    let group = members.entry(category).or_default();
                    if group.is_empty() {
                        lines.push((index, Some(category)));
                    }
                    group.push(index);
                }
            }
        }
        let now = Local::now();
        let mut items = Vec::with_capacity(lines.len() + 8);
        let mut current: Option<String> = None;
        for (index, bundle) in lines {
            if sections {
                let section = format::list_section(list.conversations[index].date, now);
                if current.as_ref() != Some(&section) {
                    items.push(Item::Section(section.clone().into()));
                    current = Some(section);
                }
            }
            items.push(match bundle {
                None => Item::Row(index),
                Some(category) => Item::Bundle(Rc::new(Bundle::new(
                    category,
                    members.remove(&category).unwrap_or_default(),
                    &list.conversations,
                ))),
            });
        }
        self.items = items;
    }

    /// Drops a bundle layer whose bundle is gone, and keeps open readers' conversations current.
    fn refresh_layers(&mut self) {
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
        let mut stale_threads = Vec::new();
        for layer in &mut layers {
            if let Layer::Reader(conversation) = layer
                && let Some(fresh) = current.get(&conversation.id)
            {
                **conversation = (*fresh).clone();
                if self.threads.get(&fresh.id).is_some_and(|t| t.count != fresh.message_count) {
                    stale_threads.push(fresh.id.clone());
                }
            }
        }
        // A bundle that emptied closes, with whatever lies over it.
        if let Some(gone) = layers.iter().position(|l| matches!(l, Layer::Bundle(c) if !bundles.contains(c))) {
            layers.truncate(gone);
        }
        self.layers = layers;
        for id in stale_threads {
            self.threads.remove(&id);
        }
    }

    fn load_thread(&mut self, conversation: &Conversation, cx: &mut Context<Self>) {
        if let Some(thread) = self.threads.get(&conversation.id)
            && thread.count == conversation.message_count
            && !matches!(thread.state, ThreadState::Failed(_))
        {
            return;
        }
        let id = conversation.id.clone();
        self.threads.insert(id.clone(), ThreadEntry { state: ThreadState::Loading, count: conversation.message_count });
        let arg = id.to_string();
        self.background(
            cx,
            move || {
                control::call::<Thread>("mail.show", json!({ "id": arg, "max-chars": 200_000 })).map(|mut thread| {
                    let now = Local::now();
                    for message in &mut thread.messages {
                        message.prepare(now);
                    }
                    thread.messages
                })
            },
            move |this, result, cx| {
                let Some(thread) = this.threads.get_mut(&id) else { return };
                thread.state = match result {
                    Ok(messages) => ThreadState::Loaded(messages.into()),
                    Err(error) => ThreadState::Failed(error.message.into()),
                };
                cx.notify();
            },
        );
    }

    // MARK: - Selection and navigation

    pub fn select(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.source == source && self.query.is_none();
        self.source = source;
        self.query = None;
        self.search.update(cx, |search, cx| search.clear(cx));
        self.layers.clear();
        if !same {
            self.highlight = None;
            self.list_scroll = UniformListScrollHandle::new();
        }
        self.rebuild_items();
        self.load_list(self.key(), false, cx);
        window.focus(&self.focus);
        cx.notify();
    }

    fn on_search_event(
        &mut self,
        search: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Submit => {
                let text = search.read(cx).text().trim().to_owned();
                if text.is_empty() {
                    self.end_search(cx);
                } else {
                    self.query = Some(text.into());
                    self.layers.clear();
                    self.highlight = None;
                    self.list_scroll = UniformListScrollHandle::new();
                    self.rebuild_items();
                    self.load_list(self.key(), false, cx);
                }
                window.focus(&self.focus);
            }
            InputEvent::Cancel => {
                search.update(cx, |search, cx| search.clear(cx));
                self.end_search(cx);
                window.focus(&self.focus);
            }
            InputEvent::Changed => {
                if search.read(cx).text().is_empty() && self.query.is_some() {
                    self.end_search(cx);
                }
            }
        }
        cx.notify();
    }

    fn end_search(&mut self, cx: &mut Context<Self>) {
        if self.query.take().is_some() {
            self.layers.clear();
            self.highlight = None;
            self.list_scroll = UniformListScrollHandle::new();
            self.rebuild_items();
            self.load_list(self.key(), false, cx);
        }
    }

    pub fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.focus_handle(cx);
        window.focus(&focus);
        self.search.update(cx, |search, cx| search.select_everything(cx));
    }

    pub fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        // The app syncs every account; the lists are asked again once it has had a moment.
        self.background(
            cx,
            || {
                let started = control::call::<Value>("refresh", json!({}));
                std::thread::sleep(Duration::from_secs(2));
                started
            },
            |this, result, cx| {
                if let Err(error) = result {
                    this.show_toast(format!("Couldn't refresh: {error}"), cx);
                }
                this.load_mailboxes(cx);
                this.load_list(this.key(), true, cx);
            },
        );
        self.load_list(self.key(), true, cx);
    }

    /// The list the keyboard moves in: the open bundle's, else the main one. Each line is an id
    /// and the row's index in its list view.
    fn active_lines(&self) -> (Vec<(SharedString, usize)>, bool) {
        if let Some(bundle) = self.open_bundle_members() {
            let Some(list) = self.list() else { return (Vec::new(), true) };
            let lines =
                bundle.members.iter().enumerate().map(|(row, &ix)| (list.conversations[ix].id.clone(), row)).collect();
            return (lines, true);
        }
        let lines = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(position, item)| match item {
                Item::Section(_) => None,
                _ => Some((self.item_id(item), position)),
            })
            .collect();
        (lines, false)
    }

    pub fn item_id(&self, item: &Item) -> SharedString {
        match item {
            Item::Section(title) => format!("section:{title}").into(),
            Item::Row(index) => self.list().map(|l| l.conversations[*index].id.clone()).unwrap_or_default(),
            Item::Bundle(bundle) => format!("bundle:{}", bundle.category.key()).into(),
        }
    }

    pub fn open_bundle(&self) -> Option<Category> {
        self.layers.iter().rev().find_map(|l| match l {
            Layer::Bundle(category) => Some(*category),
            Layer::Reader(_) => None,
        })
    }

    /// The bundle lying over the list.
    pub fn open_bundle_members(&self) -> Option<Rc<Bundle>> {
        let category = self.open_bundle()?;
        self.items.iter().find_map(|item| match item {
            Item::Bundle(bundle) if bundle.category == category => Some(bundle.clone()),
            _ => None,
        })
    }

    pub fn reader(&self) -> Option<&Conversation> {
        match self.layers.last() {
            Some(Layer::Reader(conversation)) => Some(conversation),
            _ => None,
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let (lines, in_bundle) = self.active_lines();
        if lines.is_empty() {
            return;
        }
        let current = if in_bundle { &self.bundle_highlight } else { &self.highlight };
        let position = current.as_ref().and_then(|id| lines.iter().position(|(line, _)| line == id));
        let target = match position {
            None => if delta > 0 { 0 } else { lines.len() - 1 },
            Some(at) => (at as isize + delta).clamp(0, lines.len() as isize - 1) as usize,
        };
        let (id, scroll_index) = lines[target].clone();
        let reading = self.reader().is_some();
        if in_bundle {
            self.bundle_highlight = Some(id.clone());
            self.bundle_scroll.scroll_to_item(scroll_index, ScrollStrategy::Top);
        } else {
            self.highlight = Some(id.clone());
            self.list_scroll.scroll_to_item(scroll_index, ScrollStrategy::Top);
        }
        // In the reader, the keys page through the conversations under it.
        if reading && Some(target) != position {
            self.layers.pop();
            self.activate(&id, window, cx);
        }
        cx.notify();
    }

    pub fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        self.step(1, window, cx);
    }

    pub fn previous(&mut self, _: &Previous, window: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, window, cx);
    }

    pub fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader().is_some() {
            return;
        }
        let current = if self.open_bundle().is_some() { &self.bundle_highlight } else { &self.highlight };
        if let Some(id) = current.clone() {
            self.activate(&id, window, cx);
        }
    }

    /// Opens a row: a conversation in the reader, or a bundle over the list.
    pub fn activate(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if let Some(category) = id.strip_prefix("bundle:") {
            let Some(category) = self.items.iter().find_map(|item| match item {
                Item::Bundle(bundle) if bundle.category.key() == category => Some(bundle.category),
                _ => None,
            }) else {
                return;
            };
            self.highlight = Some(id.clone());
            self.bundle_highlight = None;
            self.bundle_scroll = UniformListScrollHandle::new();
            self.layers.push(Layer::Bundle(category));
            cx.notify();
            return;
        }
        let Some(conversation) = self.list().and_then(|l| l.conversations.iter().find(|c| &c.id == id)).cloned() else {
            return;
        };
        if self.open_bundle().is_some() {
            self.bundle_highlight = Some(id.clone());
        } else {
            self.highlight = Some(id.clone());
        }
        self.reader_scroll = ScrollHandle::new();
        self.load_thread(&conversation, cx);
        let unread = conversation.unread;
        self.layers.push(Layer::Reader(Box::new(conversation.clone())));
        if unread {
            self.set_unread(&conversation, false, cx);
        }
        cx.notify();
    }

    pub fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        if self.layers.pop().is_none() && self.query.is_some() {
            self.search.update(cx, |search, cx| search.clear(cx));
            self.end_search(cx);
        }
        window.focus(&self.focus);
        cx.notify();
    }

    pub fn toggle_fold(&mut self, message: &SharedString, cx: &mut Context<Self>) {
        if !self.unfolded.remove(message) {
            self.unfolded.insert(message.clone());
        }
        cx.notify();
    }

    // MARK: - Actions

    /// What an action applies to: the open conversation, else the highlighted row.
    fn target(&self) -> Option<Conversation> {
        if let Some(conversation) = self.reader() {
            return Some(conversation.clone());
        }
        let id = if self.open_bundle().is_some() { self.bundle_highlight.as_ref() } else { self.highlight.as_ref() }?;
        self.list()?.conversations.iter().find(|c| &c.id == id).cloned()
    }

    pub fn archive(&mut self, _: &Archive, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.target() {
            self.remove(conversation, "mail.archive", "archive", window, cx);
        }
    }

    pub fn trash(&mut self, _: &Trash, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.target() {
            self.remove(conversation, "mail.trash", "move it to the Trash", window, cx);
        }
    }

    pub fn star(&mut self, _: &Star, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.target() {
            self.set_starred(&conversation, !conversation.starred, cx);
        }
    }

    pub fn toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.target() {
            self.set_unread(&conversation, !conversation.unread, cx);
        }
    }

    /// Takes the conversation out of every list at once; puts it back if the app refuses.
    pub fn remove(
        &mut self,
        conversation: Conversation,
        command: &'static str,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = conversation.id.clone();
        // The highlight moves to the row after, as in the native list.
        let (lines, in_bundle) = self.active_lines();
        let next = lines
            .iter()
            .position(|(line, _)| line == &id)
            .and_then(|at| lines.get(at + 1).or_else(|| at.checked_sub(1).and_then(|before| lines.get(before))))
            .map(|(line, _)| line.clone());
        if in_bundle {
            self.bundle_highlight = next;
        } else {
            self.highlight = next;
        }

        let mut removed: Vec<(ListKey, usize, Conversation)> = Vec::new();
        for (key, list) in &mut self.lists {
            if let Some(at) = list.conversations.iter().position(|c| c.id == id) {
                removed.push((key.clone(), at, list.conversations.remove(at)));
            }
        }
        self.layers.retain(|layer| !matches!(layer, Layer::Reader(c) if c.id == id));
        self.removing.insert(id.clone());
        self.rebuild_items();
        self.refresh_layers();
        window.focus(&self.focus);
        cx.notify();

        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch(command, json!({ "ids": [arg] })),
            move |this, result, cx| {
                this.removing.remove(&id);
                let Err(error) = result else { return };
                for (key, at, conversation) in removed {
                    let list = this.lists.entry(key).or_default();
                    if !list.conversations.iter().any(|c| c.id == conversation.id) {
                        list.conversations.insert(at.min(list.conversations.len()), conversation);
                    }
                }
                this.rebuild_items();
                let message = if error.code == "confirm" {
                    "It is already in the Trash; deleting for good is left to the app.".to_owned()
                } else {
                    format!("Couldn't {verb}: {error}")
                };
                this.show_toast(message, cx);
                cx.notify();
            },
        );
    }

    /// Applies `change` to the conversation wherever it is shown.
    fn patch(&mut self, id: &SharedString, change: impl Fn(&mut Conversation)) {
        for list in self.lists.values_mut() {
            for conversation in list.conversations.iter_mut().filter(|c| &c.id == id) {
                change(conversation);
            }
        }
        for layer in &mut self.layers {
            if let Layer::Reader(conversation) = layer
                && &conversation.id == id
            {
                change(conversation);
            }
        }
    }

    pub fn set_starred(&mut self, conversation: &Conversation, starred: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.starred = starred);
        cx.notify();
        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch("mail.star", json!({ "ids": [arg], "off": !starred })),
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.starred = !starred);
                    this.show_toast(format!("Couldn't update the star: {error}"), cx);
                }
            },
        );
    }

    pub fn set_unread(&mut self, conversation: &Conversation, unread: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.unread = unread);
        cx.notify();
        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch("mail.read", json!({ "ids": [arg], "unread": unread })),
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.unread = !unread);
                    this.show_toast(format!("Couldn't mark it {}: {error}", if unread { "unread" } else { "read" }), cx);
                }
            },
        );
    }

    pub fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast_generation += 1;
        let generation = self.toast_generation;
        self.toast = Some(Toast { text: text.into(), generation });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.generation == generation) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_mailboxes(cx);
        self.load_list(self.key(), true, cx);
    }
}

/// One page of a list, decoded and prepared for display. Blocking.
fn fetch(command: &'static str, args: Value) -> Result<Listing, ControlError> {
    let mut listing: Listing = control::call(command, args)?;
    let now = Local::now();
    for conversation in &mut listing.conversations {
        conversation.prepare(now);
    }
    Ok(listing)
}
