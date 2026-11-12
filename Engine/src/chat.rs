//! Google Chat state and the `chat.*` commands, as the Swift engine's `ChatStore` and its
//! control commands keep and answer them (`Sources/MailstreamKit/Store/ChatStore.swift`,
//! `Control/ControlService.swift`): chat accounts (Google accounts granted Chat access), each
//! account's most recently active spaces, and the messages of spaces the user opened. The engine
//! holds the open space and its history; front ends re-read them on `chat` and `chatMessages`.
//!
//! Spaces are polled every `chat.pollSeconds` (`ChatRemote` refetches only spaces whose activity
//! changed). Reading and sending are optimistic: opening a space clears its unread count at
//! once, and a sent message shows as pending until Google returns it. A failed send is marked
//! failed, never retried. Space lists and the messages of the 20 most recently opened spaces
//! are kept in mail.db, so the list and those spaces paint before Google answers.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::{Value, json};

use crate::accounts::{AccountRecord, Accounts};
use crate::api::{Api, ApiError};
use crate::avatars::Avatars;
use crate::cache::{MailCache, hash};
use crate::chat_api::{ChatRemote, chat_error};
use crate::chat_model::{
    ChatAccount, ChatMessage, ChatMessagePage, ChatRow, ChatSpace, ChatSpaceKey, ChatSpaceKind, ChatSpaceList, ChatUser,
    DateLabels, sort_newest_first,
};
use crate::commands::Arguments;
use crate::mail::Mail;
use crate::model::{DISTANT_PAST, Date, iso8601, now};
use crate::settings::{Setting, Settings};
use crate::{EngineError, Events, Provider};

const SPACES_PAGE: usize = 60;
const MESSAGES_PAGE: usize = 100;
/// Spaces whose messages stay on disk, most recently opened first.
const RECENT_LIMIT: usize = 20;
/// Messages kept on disk per recent space.
const CACHED_MESSAGES: usize = 150;
/// The scope that makes a Google account a chat account.
const CHAT_SCOPE: &str = "https://www.googleapis.com/auth/chat.messages.readonly";
/// The scope that lets an account start direct messages and group chats (`spaces.setup`).
const CREATE_SCOPE: &str = "https://www.googleapis.com/auth/chat.spaces.create";


struct Listing {
    list: ChatSpaceList,
    /// `limit` of the next poll; `chat.more` raises it.
    limit: usize,
}

/// A message sent from here that Google has not confirmed yet, or that failed to send.
#[derive(Clone, Debug, PartialEq)]
struct Pending {
    id: String,
    text: String,
    thread: Option<String>,
    created: Date,
    /// Why sending failed; nil while sending.
    failure: Option<String>,
}

/// The loaded part of one space's history.
#[derive(Clone, Debug, Default, PartialEq)]
struct Thread {
    /// Ascending by creation.
    messages: Vec<ChatMessage>,
    /// Older messages exist beyond the first one here.
    more: bool,
    last_read: Option<Date>,
    /// Answered by Google this session (not only read from disk).
    loaded: bool,
    /// Why the last fetch failed, while there is nothing to show.
    failure: Option<String>,
    pending: Vec<Pending>,
}

/// A space's newest-page fetch: one asked for while it runs runs after it.
#[derive(Default)]
struct Fetch {
    again: bool,
}

#[derive(Default)]
struct State {
    accounts: Vec<ChatAccount>,
    listings: HashMap<String, Listing>,
    /// Every account's spaces as list rows, newest first; rebuilt when a listing changes.
    rows: Vec<ChatRow>,
    loading: bool,
    /// The space in the reader: polling keeps its messages current and read.
    open: Option<ChatSpaceKey>,
    threads: HashMap<ChatSpaceKey, Thread>,
    recent: Vec<ChatSpaceKey>,
    /// When the user last read a space here; polls answered before Google saw it keep it read.
    read_marks: HashMap<ChatSpaceKey, Date>,
    /// Bumped by each mark-read, so only the latest one may roll its optimistic mark back.
    read_revisions: HashMap<ChatSpaceKey, u64>,
    polling: bool,
    loading_more: bool,
    fetches: HashMap<ChatSpaceKey, Fetch>,
    earlier: HashSet<ChatSpaceKey>,
    /// The latest failure, until a poll succeeds.
    problem: Option<String>,
}

impl State {
    fn has_account(&self, name: &str) -> bool {
        self.accounts.iter().any(|account| account.name == name)
    }

    fn space(&self, key: &ChatSpaceKey) -> Option<&ChatSpace> {
        self.listings.get(&key.account)?.list.spaces.iter().find(|space| space.name == key.space)
    }

    fn rebuild(&mut self) {
        let labels = DateLabels::now();
        let mut rows: Vec<ChatRow> = self
            .accounts
            .iter()
            .filter_map(|account| self.listings.get(&account.name).map(|listing| (account, listing)))
            .flat_map(|(account, listing)| listing.list.spaces.iter().map(|space| ChatRow::new(space, &account.name, &labels)))
            .collect();
        sort_newest_first(&mut rows, |row| row.activity);
        self.rows = rows;
    }
}

pub struct Chat {
    remote: ChatRemote,
    mail: Arc<Mail>,
    cache: Arc<MailCache>,
    accounts: Arc<Accounts>,
    settings: Arc<Settings>,
    events: Arc<Events>,
    state: Mutex<State>,
    /// One account sync at a time.
    syncing: tokio::sync::Mutex<()>,
    /// Restarts the poll's wait when `chat.pollSeconds` changes.
    interval_changed: tokio::sync::Notify,
    /// Takes the pictures Google Chat publishes for the people the rows and threads show.
    avatars: Arc<Avatars>,
}

fn is_chat(record: &AccountRecord) -> bool {
    record.provider == Provider::Google && record.scopes.iter().any(|scope| scope == CHAT_SCOPE)
}

fn failed(error: &ApiError) -> EngineError {
    EngineError::new("failed", error.message.clone())
}

/// A random UUID as Foundation writes one: `E621E1F8-C36C-495A-93FC-0C247A3E6E5F`.
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the system's random source works");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// `name` as one path component.
fn file_name(name: &str) -> String {
    let safe = name.replace(['/', ':'], "-");
    if safe.is_empty() || safe.starts_with('.') { format!("attachment{safe}") } else { safe }
}

impl Chat {
    pub fn new(
        mail: Arc<Mail>,
        root: String,
        accounts: Arc<Accounts>,
        settings: Arc<Settings>,
        events: Arc<Events>,
        avatars: Arc<Avatars>,
    ) -> Self {
        Self {
            remote: ChatRemote::new(root),
            cache: mail.cache(),
            mail,
            accounts,
            settings,
            events,
            state: Mutex::new(State::default()),
            syncing: tokio::sync::Mutex::new(()),
            interval_changed: tokio::sync::Notify::new(),
            avatars,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("chat lock")
    }

    #[cfg(test)]
    pub async fn flush(&self) {
        self.cache.flush().await;
    }

    // MARK: - Startup and polling

    /// Paints the last known accounts and spaces from mail.db, follows the signed-in chat
    /// accounts, fetches their spaces, then polls every `chat.pollSeconds`.
    pub async fn start(self: Arc<Self>) {
        self.restore_from_cache().await;
        self.sync_accounts().await;
        self.poll(false);
        let chat = self.clone();
        tokio::spawn(async move {
            loop {
                let seconds = chat.settings.value(Setting::ChatPollSeconds).as_u64().unwrap_or(20).max(1);
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(seconds)) => chat.poll(true),
                    () = chat.interval_changed.notified() => {}
                }
            }
        });
    }

    /// settings.json changed: a changed `chat.pollSeconds` starts the wait afresh.
    pub fn settings_changed(&self) {
        self.interval_changed.notify_waiters();
    }

    /// Signed-in accounts changed: removed ones drop their spaces and messages, added ones load.
    pub fn accounts_changed(self: &Arc<Self>) {
        let chat = self.clone();
        tokio::spawn(async move {
            chat.sync_accounts().await;
            chat.poll(false);
        });
    }

    async fn restore_from_cache(&self) {
        let saved_recent = self.cache.load_chat_recent().await.unwrap_or_default();
        {
            let mut state = self.state();
            if state.recent.is_empty() {
                state.recent = saved_recent;
            }
        }
        let Some(saved) = self.cache.load_chat_accounts().await.filter(|saved| !saved.is_empty()) else { return };
        {
            let mut state = self.state();
            if !state.accounts.is_empty() {
                return;
            }
            state.accounts = saved.clone();
        }
        self.load_cached_spaces(saved.iter().map(|account| account.name.clone()).collect()).await;
        self.list_changed(&mut self.state());
    }

    async fn load_cached_spaces(&self, names: Vec<String>) {
        let loads = names.into_iter().map(|name| async move { (self.cache.load_chat_spaces(&name).await, name) });
        for (list, name) in futures::future::join_all(loads).await {
            let Some(list) = list else { continue };
            let mut state = self.state();
            if state.listings.contains_key(&name) || !state.has_account(&name) {
                continue;
            }
            let limit = SPACES_PAGE.max(list.spaces.len());
            state.listings.insert(name, Listing { list, limit });
        }
    }

    /// Takes the chat accounts from accounts.json; removed accounts' spaces, messages and
    /// fetches disappear. One pass at a time.
    async fn sync_accounts(&self) {
        let _pass = self.syncing.lock().await;
        let fresh: Vec<ChatAccount> = self
            .accounts
            .list()
            .iter()
            .filter(|record| is_chat(record))
            .map(|record| ChatAccount { name: record.id.clone(), email: Some(record.id.clone()) })
            .collect();
        let load = {
            let mut state = self.state();
            if fresh == state.accounts {
                return;
            }
            let names: HashSet<&str> = fresh.iter().map(|account| account.name.as_str()).collect();
            for gone in state.accounts.iter().filter(|account| !names.contains(account.name.as_str())) {
                self.remote.forget(&gone.name);
            }
            state.accounts = fresh.clone();
            self.prune(&mut state);
            fresh.iter().map(|account| account.name.clone()).filter(|name| !state.listings.contains_key(name)).collect()
        };
        self.load_cached_spaces(load).await;
        self.cache.save_chat_accounts(&fresh);
        self.list_changed(&mut self.state());
    }

    /// Drops whatever belongs to accounts no longer in `accounts`, cached messages included.
    fn prune(&self, state: &mut State) {
        let names: HashSet<String> = state.accounts.iter().map(|account| account.name.clone()).collect();
        state.listings.retain(|name, _| names.contains(name));
        state.threads.retain(|key, _| names.contains(&key.account));
        state.fetches.retain(|key, _| names.contains(&key.account));
        state.earlier.retain(|key| names.contains(&key.account));
        state.read_marks.retain(|key, _| names.contains(&key.account));
        state.read_revisions.retain(|key, _| names.contains(&key.account));
        let (kept, gone): (Vec<ChatSpaceKey>, Vec<ChatSpaceKey>) =
            state.recent.drain(..).partition(|key| names.contains(&key.account));
        state.recent = kept;
        if !gone.is_empty() {
            gone.iter().for_each(|key| self.cache.remove_chat_messages(key));
            self.cache.save_chat_recent(&state.recent);
        }
        if state.open.as_ref().is_some_and(|open| !names.contains(&open.account)) {
            state.open = None;
            self.events.emit_soon("chatMessages");
        }
    }

    /// The account's requests, while it is a chat account.
    fn api(&self, account: &str) -> Result<Arc<Api>, ApiError> {
        self.accounts
            .record(account)
            .filter(is_chat)
            .and_then(|_| self.mail.api(account))
            .ok_or_else(|| chat_error(format!("No Google Chat account named {account}")))
    }

    /// Fetches every account's spaces now; `quiet` polls (the timer's) show no progress.
    fn poll(self: &Arc<Self>, quiet: bool) {
        let jobs: Vec<(String, usize)> = {
            let mut state = self.state();
            let jobs: Vec<(String, usize)> = state
                .accounts
                .iter()
                .map(|account| {
                    (account.name.clone(), state.listings.get(&account.name).map_or(SPACES_PAGE, |listing| listing.limit))
                })
                .collect();
            if jobs.is_empty() {
                return;
            }
            if !quiet {
                self.set_loading(&mut state, true);
            }
            if state.polling {
                return;
            }
            state.polling = true;
            jobs
        };
        let chat = self.clone();
        tokio::spawn(async move {
            let mut work: FuturesUnordered<_> = jobs
                .into_iter()
                .map(|(name, limit)| {
                    let chat = &chat;
                    async move {
                        let result = match chat.api(&name) {
                            Ok(api) => chat.remote.spaces(&api, &name, limit).await,
                            Err(error) => Err(error),
                        };
                        (name, limit, result)
                    }
                })
                .collect();
            let mut failed = false;
            while let Some((name, limit, result)) = work.next().await {
                match result {
                    Ok(list) => chat.apply(list, &name, limit),
                    Err(error) => {
                        failed = true;
                        chat.report(&error, &name);
                    }
                }
            }
            drop(work);
            let mut state = chat.state();
            state.polling = false;
            if !failed && state.problem.take().is_some() {
                chat.events.emit_soon("problems");
            }
            let more = state.loading_more;
            chat.set_loading(&mut state, more);
        });
    }

    /// Takes in one account's spaces; nothing changes when nothing did, so an idle poll costs
    /// nothing.
    fn apply(self: &Arc<Self>, fresh: ChatSpaceList, account: &str, limit: usize) {
        let mut fetch = None;
        {
            let mut state = self.state();
            // A poll that outlived its account's sign-out.
            if !state.has_account(account) {
                return;
            }
            // A poll started before the limit was raised would shrink the list back.
            if state.listings.get(account).is_some_and(|listing| listing.limit != limit) {
                return;
            }
            let previous: HashMap<&str, Option<Date>> = state
                .listings
                .get(account)
                .map(|listing| listing.list.spaces.iter().map(|space| (space.name.as_str(), space.activity())).collect())
                .unwrap_or_default();
            let mut list = fresh;
            for space in &mut list.spaces {
                let key = ChatSpaceKey::new(account, &space.name);
                if let Some(mark) = state.read_marks.get(&key)
                    && space.latest.as_ref().map(|latest| latest.created).or(space.activity()).unwrap_or(DISTANT_PAST) <= *mark
                {
                    space.unread = 0;
                }
                if state.open.as_ref() == Some(&key) && previous.get(space.name.as_str()).copied().flatten() != space.activity() {
                    fetch = Some(key);
                }
            }
            let unchanged = state.listings.get(account).is_some_and(|listing| listing.list == list);
            if !unchanged {
                self.cache.save_chat_spaces(&list, account);
                state.listings.insert(account.to_string(), Listing { list, limit });
                self.list_changed(&mut state);
            }
        }
        if let Some(key) = fetch {
            self.fetch_latest(key);
        }
    }

    fn list_changed(&self, state: &mut State) {
        state.rebuild();
        for row in &state.rows {
            if let Some(url) = &row.avatar_url {
                self.avatars.publish(url, &row.avatar_key);
            }
        }
        self.events.emit_soon("chat");
    }

    fn set_loading(&self, state: &mut State, loading: bool) {
        if state.loading != loading {
            state.loading = loading;
            self.events.emit_soon("chat");
        }
    }

    fn can_load_more(state: &State, account: Option<&str>) -> bool {
        !state.loading_more
            && state.listings.iter().any(|(name, listing)| account.is_none_or(|account| account == name) && listing.list.more)
    }

    /// Raises the spaces limit of the accounts in view by a page.
    fn load_more(self: &Arc<Self>, account: Option<&str>) {
        let names: Vec<String> = {
            let mut state = self.state();
            if !Self::can_load_more(&state, account) {
                return;
            }
            let names = state
                .accounts
                .iter()
                .map(|account| account.name.clone())
                .filter(|name| {
                    account.is_none_or(|account| account == name)
                        && state.listings.get(name).is_some_and(|listing| listing.list.more)
                })
                .collect();
            state.loading_more = true;
            self.set_loading(&mut state, true);
            names
        };
        let chat = self.clone();
        tokio::spawn(async move {
            for name in names {
                let limit = chat.state().listings.get(&name).map_or(SPACES_PAGE, |listing| listing.limit) + SPACES_PAGE;
                let result = match chat.api(&name) {
                    Ok(api) => chat.remote.spaces(&api, &name, limit).await,
                    Err(error) => Err(error),
                };
                match result {
                    Ok(list) => {
                        if let Some(listing) = chat.state().listings.get_mut(&name) {
                            listing.limit = limit;
                        }
                        chat.apply(list, &name, limit);
                    }
                    Err(error) => chat.report(&error, &name),
                }
            }
            let mut state = chat.state();
            state.loading_more = false;
            let polling = state.polling;
            chat.set_loading(&mut state, polling);
            // `more` of `chat.spaces` follows `loading_more`.
            chat.events.emit_soon("chat");
        });
    }

    // MARK: - Opening

    /// Something the reader shows of `key` changed.
    fn thread_changed(&self, state: &State, key: &ChatSpaceKey) {
        if state.open.as_ref() == Some(key) {
            self.events.emit_soon("chatMessages");
        }
    }

    /// Reads a recent space's messages from disk into memory, so opening it paints them first.
    fn preload(self: &Arc<Self>, key: &ChatSpaceKey) {
        {
            let mut state = self.state();
            if state.threads.contains_key(key) {
                return;
            }
            state.threads.insert(key.clone(), Thread::default());
        }
        let (chat, key) = (self.clone(), key.clone());
        tokio::spawn(async move {
            let Some(saved) = chat.cache.load_chat_messages(&key).await else { return };
            let mut state = chat.state();
            let Some(thread) = state.threads.get_mut(&key) else { return };
            if thread.loaded || !thread.messages.is_empty() {
                return;
            }
            thread.messages = saved.messages;
            thread.more = saved.more;
            thread.last_read = saved.last_read;
            chat.thread_changed(&state, &key);
        });
    }

    /// The reader shows `key`: its cached messages at once, the newest from Google when they
    /// arrive, and it is marked read.
    fn open(self: &Arc<Self>, key: &ChatSpaceKey) {
        {
            let mut state = self.state();
            state.open = Some(key.clone());
            self.remember(&mut state, key);
            self.events.emit_soon("chatMessages");
        }
        self.preload(key);
        self.fetch_latest(key.clone());
        self.mark_read(key);
    }

    fn close(&self) {
        let mut state = self.state();
        if state.open.take().is_some() {
            self.events.emit_soon("chatMessages");
        }
    }

    fn fetch_latest(self: &Arc<Self>, key: ChatSpaceKey) {
        {
            let mut state = self.state();
            if let Some(fetch) = state.fetches.get_mut(&key) {
                fetch.again = true;
                return;
            }
            state.fetches.insert(key.clone(), Fetch::default());
        }
        let chat = self.clone();
        tokio::spawn(async move {
            loop {
                chat.fetch_pass(&key).await;
                let mut state = chat.state();
                match state.fetches.get_mut(&key) {
                    Some(fetch) if fetch.again => fetch.again = false,
                    _ => {
                        state.fetches.remove(&key);
                        return;
                    }
                }
            }
        });
    }

    /// Asked for while open: a space closed before an extra fetch began needs none.
    async fn fetch_pass(self: &Arc<Self>, key: &ChatSpaceKey) {
        if self.state().open.as_ref() != Some(key) {
            return;
        }
        let result = match self.api(&key.account) {
            Ok(api) => self.remote.messages(&api, &key.account, &key.space, MESSAGES_PAGE, None).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(page) => self.merge_latest(page, key),
            Err(error) => {
                {
                    let mut state = self.state();
                    if !state.has_account(&key.account) {
                        return;
                    }
                    let thread = state.threads.entry(key.clone()).or_default();
                    if thread.messages.is_empty() {
                        thread.failure = Some(error.message.clone());
                        self.thread_changed(&state, key);
                    }
                }
                self.report(&error, &key.account);
            }
        }
    }

    /// The newest page replaces what it covers; older loaded messages stay below it.
    fn merge_latest(self: &Arc<Self>, page: ChatMessagePage, key: &ChatSpaceKey) {
        let read = {
            let mut state = self.state();
            if !state.has_account(&key.account) {
                return;
            }
            let thread = state.threads.entry(key.clone()).or_default();
            // Messages newer than the page's newest were confirmed (sent from here) after this
            // fetch left; the page can't have them, so they stay.
            let newest = page.messages.last().map_or(DISTANT_PAST, |message| message.created);
            let names: HashSet<&str> = page.messages.iter().map(|message| message.name.as_str()).collect();
            let newer: Vec<ChatMessage> = thread
                .messages
                .iter()
                .filter(|message| !names.contains(message.name.as_str()) && message.created > newest)
                .cloned()
                .collect();
            if page.more
                && let Some(oldest) = page.messages.first().map(|message| message.created)
            {
                let older: Vec<ChatMessage> = thread
                    .messages
                    .iter()
                    .filter(|message| !names.contains(message.name.as_str()) && message.created < oldest)
                    .cloned()
                    .collect();
                if older.is_empty() {
                    thread.more = true;
                }
                thread.messages = older.into_iter().chain(page.messages.iter().cloned()).chain(newer).collect();
            } else {
                thread.messages = page.messages.iter().cloned().chain(newer).collect();
                thread.more = page.more;
            }
            thread.last_read = page.last_read.or(thread.last_read);
            thread.loaded = true;
            thread.failure = None;
            let newest_incoming = thread.messages.iter().rev().find(|message| !message.mine).map(|message| message.created);
            self.persist(&state, key);
            self.thread_changed(&state, key);
            // While open, a message from someone else newer than the read mark is read on arrival.
            state.open.as_ref() == Some(key)
                && newest_incoming.is_some_and(|created| created > state.read_marks.get(key).copied().unwrap_or(DISTANT_PAST))
        };
        if read {
            self.mark_read(key);
        }
    }

    /// Fetches the page before the oldest loaded message.
    fn load_earlier(self: &Arc<Self>, key: &ChatSpaceKey) {
        let oldest = {
            let mut state = self.state();
            if state.earlier.contains(key) {
                return;
            }
            let Some(thread) = state.threads.get(key).filter(|thread| thread.more) else { return };
            let Some(oldest) = thread.messages.first().map(|message| message.created) else { return };
            state.earlier.insert(key.clone());
            self.thread_changed(&state, key);
            oldest
        };
        let (chat, key) = (self.clone(), key.clone());
        tokio::spawn(async move {
            let result = match chat.api(&key.account) {
                Ok(api) => chat.remote.messages(&api, &key.account, &key.space, MESSAGES_PAGE, Some(oldest)).await,
                Err(error) => Err(error),
            };
            let mut state = chat.state();
            state.earlier.remove(&key);
            match result {
                Ok(page) => {
                    if let Some(thread) = state.threads.get_mut(&key) {
                        let known: HashSet<String> = thread.messages.iter().map(|message| message.name.clone()).collect();
                        let mut messages: Vec<ChatMessage> =
                            page.messages.into_iter().filter(|message| !known.contains(&message.name)).collect();
                        messages.append(&mut thread.messages);
                        messages.sort_by(|a, b| a.created.partial_cmp(&b.created).unwrap_or(std::cmp::Ordering::Equal));
                        thread.messages = messages;
                        thread.more = page.more;
                    }
                    chat.thread_changed(&state, &key);
                }
                Err(error) => {
                    chat.thread_changed(&state, &key);
                    drop(state);
                    chat.report(&error, &key.account);
                }
            }
        });
    }

    // MARK: - Reading and sending

    /// Clears the unread count at once; restores it if Google refuses.
    fn mark_read(self: &Arc<Self>, key: &ChatSpaceKey) {
        let (revision, previous_unread) = {
            let mut state = self.state();
            let mark = state.read_marks.get(key).copied().unwrap_or(DISTANT_PAST).max(now());
            state.read_marks.insert(key.clone(), mark);
            let revision = state.read_revisions.get(key).copied().unwrap_or(0) + 1;
            state.read_revisions.insert(key.clone(), revision);
            let previous_unread = state.space(key).map(|space| space.unread);
            if previous_unread.is_some_and(|unread| unread != 0) {
                self.update_space(&mut state, key, |space| space.unread = 0);
            }
            (revision, previous_unread)
        };
        let (chat, key) = (self.clone(), key.clone());
        tokio::spawn(async move {
            let result = match chat.api(&key.account) {
                Ok(api) => chat.remote.mark_read(&api, &key.account, &key.space).await,
                Err(error) => Err(error),
            };
            let mut state = chat.state();
            match result {
                Ok(last_read) => {
                    if !state.has_account(&key.account) {
                        return;
                    }
                    let mark = state.read_marks.get(&key).copied().unwrap_or(last_read).max(last_read);
                    state.read_marks.insert(key.clone(), mark);
                    if let Some(thread) = state.threads.get_mut(&key)
                        && thread.last_read != Some(last_read)
                    {
                        thread.last_read = Some(last_read);
                        chat.thread_changed(&state, &key);
                    }
                    chat.update_space(&mut state, &key, |space| space.last_read = Some(last_read));
                }
                Err(error) => {
                    // A later mark-read owns the state now; this failure must not undo it.
                    if state.read_revisions.get(&key) != Some(&revision) {
                        return;
                    }
                    state.read_marks.remove(&key);
                    if let Some(unread) = previous_unread.filter(|unread| *unread > 0) {
                        chat.update_space(&mut state, &key, |space| space.unread = unread);
                    }
                    drop(state);
                    chat.report(&error, &key.account);
                }
            }
        });
    }

    /// Shows `text` as a pending message at once and sends it, in `thread` when given. Returns
    /// the pending message's id and the send, which ends once Google answered: the message
    /// joined the thread, or its pending copy is marked failed.
    fn send(
        self: &Arc<Self>,
        text: &str,
        thread: Option<String>,
        key: &ChatSpaceKey,
    ) -> Option<(String, tokio::task::JoinHandle<()>)> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let pending = Pending {
            id: format!("pending-{}", uuid()),
            text: trimmed.to_string(),
            thread,
            created: now(),
            failure: None,
        };
        {
            let mut state = self.state();
            state.threads.entry(key.clone()).or_default().pending.push(pending.clone());
            self.thread_changed(&state, key);
        }
        let (chat, key, id) = (self.clone(), key.clone(), pending.id.clone());
        let task = tokio::spawn(async move {
            let result = match chat.api(&key.account) {
                Ok(api) => chat.remote.send(&api, &key.account, &key.space, &pending.text, pending.thread.as_deref()).await,
                Err(error) => Err(error),
            };
            let mut state = chat.state();
            match result {
                Ok(message) => {
                    // A send that outlived its account's sign-out.
                    if !state.has_account(&key.account) {
                        return;
                    }
                    let thread = state.threads.entry(key.clone()).or_default();
                    thread.pending.retain(|item| item.id != pending.id);
                    if !thread.messages.iter().any(|known| known.name == message.name) {
                        thread.messages.push(message.clone());
                        thread.messages.sort_by(|a, b| a.created.partial_cmp(&b.created).unwrap_or(std::cmp::Ordering::Equal));
                    }
                    chat.persist(&state, &key);
                    chat.thread_changed(&state, &key);
                    chat.update_space(&mut state, &key, |space| {
                        if space.latest.as_ref().map_or(DISTANT_PAST, |latest| latest.created) <= message.created {
                            space.latest = Some(message.clone());
                        }
                        space.last_active = Some(space.last_active.unwrap_or(message.created).max(message.created));
                    });
                }
                Err(error) => {
                    if let Some(item) = state
                        .threads
                        .get_mut(&key)
                        .and_then(|thread| thread.pending.iter_mut().find(|item| item.id == pending.id))
                    {
                        item.failure = Some(error.message);
                        chat.thread_changed(&state, &key);
                    }
                }
            }
        });
        Some((id, task))
    }

    // MARK: - Starting

    /// Whether `account`'s sign-in lets it create direct messages and group chats; accounts
    /// signed in before Mailstream asked for that can only reopen the direct messages they have.
    fn can_start_chats(&self, account: &str) -> bool {
        self.accounts.record(account).is_some_and(|record| record.scopes.iter().any(|scope| scope == CREATE_SCOPE))
    }

    /// People `account` chats with, most recently active first: the other members of its
    /// direct messages and group chats that Google names by address.
    fn people(&self, account: &str) -> Vec<ChatUser> {
        let state = self.state();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let spaces = state.listings.get(account).map(|listing| listing.list.spaces.as_slice()).unwrap_or_default();
        for space in spaces.iter().filter(|space| matches!(space.kind, ChatSpaceKind::Dm | ChatSpaceKind::Group)) {
            for member in space.members.iter().filter(|member| !member.bot) {
                let Some(email) = member.email.as_ref().map(|email| email.to_lowercase()) else { continue };
                if seen.insert(email) {
                    out.push(member.clone());
                }
            }
        }
        out
    }

    /// The conversation with exactly `emails` in `account`: the direct message with one person
    /// or the group chat with several, found in the list or at Google, else a new one Google
    /// creates. It joins the list; the returned key opens it.
    async fn start_chat(&self, account: &str, emails: &[String]) -> Result<ChatSpaceKey, ApiError> {
        if !self.state().has_account(account) {
            return Err(chat_error(format!("{account} has no Google Chat")));
        }
        let own = account.to_lowercase();
        let mut seen = HashSet::new();
        let people: Vec<String> = emails
            .iter()
            .map(|email| email.to_lowercase())
            .filter(|email| *email != own && seen.insert(email.clone()))
            .collect();
        if people.is_empty() {
            return Err(chat_error("Add someone to chat with"));
        }
        if people.len() > 49 {
            return Err(chat_error("A group chat holds at most 49 other people"));
        }
        let known = self.state().listings.get(account).and_then(|listing| {
            listing.list.spaces.iter().find(|space| Self::is_with(space, &people)).map(|space| space.name.clone())
        });
        if let Some(name) = known {
            return Ok(ChatSpaceKey::new(account, &name));
        }
        let api = self.api(account)?;
        if people.len() == 1
            && let Some(found) = self.remote.direct_message(&api, account, &people[0]).await?
        {
            return Ok(self.insert(found, account));
        }
        if !self.can_start_chats(account) {
            return Err(chat_error(format!(
                "To start new chats, sign in to {account} again in Settings > Accounts and allow Mailstream to create Google Chat conversations."
            )));
        }
        let space = self.remote.set_up(&api, account, &people, &uuid()).await?;
        Ok(self.insert(space, account))
    }

    /// A direct message or unnamed group chat whose other members are exactly `people`.
    fn is_with(space: &ChatSpace, people: &[String]) -> bool {
        let kind = if people.len() == 1 { ChatSpaceKind::Dm } else { ChatSpaceKind::Group };
        if space.kind != kind || space.members.len() != people.len() {
            return false;
        }
        let members: HashSet<String> =
            space.members.iter().filter_map(|member| member.email.as_ref().map(|email| email.to_lowercase())).collect();
        members == people.iter().cloned().collect::<HashSet<_>>()
    }

    /// Puts a space the user just found or made into its account's list, newest first. One
    /// without activity yet (made just now) counts as active now, so it shows at the top.
    fn insert(&self, found: ChatSpace, account: &str) -> ChatSpaceKey {
        let key = ChatSpaceKey::new(account, &found.name);
        let mut state = self.state();
        if !state.has_account(account) {
            return key;
        }
        let mut space = found;
        if space.activity().is_none() {
            space.last_active = Some(now());
        }
        let listing = state
            .listings
            .entry(account.to_string())
            .or_insert_with(|| Listing { list: ChatSpaceList::default(), limit: SPACES_PAGE });
        listing.list.spaces.retain(|known| known.name != space.name);
        listing.list.spaces.push(space);
        sort_newest_first(&mut listing.list.spaces, ChatSpace::activity);
        self.cache.save_chat_spaces(&listing.list, account);
        self.list_changed(&mut state);
        key
    }

    /// Changes one space in its listing, keeping the list newest first, and republishes the rows.
    fn update_space(&self, state: &mut State, key: &ChatSpaceKey, change: impl FnOnce(&mut ChatSpace)) {
        let Some(listing) = state.listings.get_mut(&key.account) else { return };
        let Some(index) = listing.list.spaces.iter().position(|space| space.name == key.space) else { return };
        let before = listing.list.spaces[index].clone();
        change(&mut listing.list.spaces[index]);
        if listing.list.spaces[index] == before {
            return;
        }
        if listing.list.spaces[index].activity() != before.activity() {
            sort_newest_first(&mut listing.list.spaces, ChatSpace::activity);
        }
        self.cache.save_chat_spaces(&listing.list, &key.account);
        self.list_changed(state);
    }

    // MARK: - Disk cache of opened spaces

    fn remember(&self, state: &mut State, key: &ChatSpaceKey) {
        state.recent.retain(|known| known != key);
        state.recent.insert(0, key.clone());
        while state.recent.len() > RECENT_LIMIT {
            if let Some(evicted) = state.recent.pop() {
                self.cache.remove_chat_messages(&evicted);
            }
        }
        self.cache.save_chat_recent(&state.recent);
    }

    fn persist(&self, state: &State, key: &ChatSpaceKey) {
        if !state.recent.contains(key) {
            return;
        }
        let Some(thread) = state.threads.get(key) else { return };
        let kept = thread.messages[thread.messages.len().saturating_sub(CACHED_MESSAGES)..].to_vec();
        let more = thread.more || kept.len() < thread.messages.len();
        self.cache.save_chat_messages(&ChatMessagePage { messages: kept, more, last_read: thread.last_read }, key);
    }

    // MARK: - Attachments

    /// An uploaded attachment as a file in the cache (`chat-media/`), downloaded once; named
    /// `name` so players and Quick Look know its kind.
    async fn attachment_file(&self, account: &str, resource: &str, name: &str) -> Result<PathBuf, ApiError> {
        let file = self.cache.root.join("chat-media").join(hash(&[account, resource])).join(file_name(name));
        if file.exists() {
            return Ok(file);
        }
        let api = self.api(account)?;
        let data = self.remote.attachment(&api, resource).await?;
        let written = file.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let directory = written.parent().expect("the file is in a folder");
            std::fs::create_dir_all(directory)?;
            let partial = directory.join(format!(".{}.partial", uuid()));
            std::fs::write(&partial, data)?;
            std::fs::rename(&partial, &written)
        })
        .await
        .map_err(|error| chat_error(error.to_string()))?
        .map_err(|error| chat_error(error.to_string()))?;
        Ok(file)
    }

    // MARK: - Reporting

    fn report(&self, error: &ApiError, context: &str) {
        self.state().problem = Some(format!("{context}: {}", error.message));
        self.events.emit_soon("problems");
    }

    // MARK: - Commands

    pub(crate) async fn run(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "chat.spaces" => {
                let account = args.string("account").map(|account| self.chat_account(&account)).transpose()?;
                Ok(self.spaces_json(account.as_deref()))
            }
            "chat.more" => {
                let account = args.string("account").map(|account| self.chat_account(&account)).transpose()?;
                self.load_more(account.as_deref());
                Ok(json!({ "started": true }))
            }
            "chat.refresh" => {
                self.poll(false);
                Ok(json!({ "started": true }))
            }
            "chat.open" => {
                let key = self.chat_space(args)?;
                self.open(&key);
                Ok(self.thread_json(&key))
            }
            "chat.close" => {
                self.close();
                Ok(json!({ "closed": true }))
            }
            "chat.messages" => Ok(self.thread_json(&self.chat_space(args)?)),
            "chat.earlier" => {
                let key = self.chat_space(args)?;
                self.load_earlier(&key);
                Ok(self.thread_json(&key))
            }
            "chat.send" => self.send_command(args).await,
            "chat.people" => {
                let account = self.chat_account(&args.required("account")?)?;
                let people: Vec<Value> = self.people(&account).iter().map(ChatUser::json).collect();
                Ok(json!({ "canStart": self.can_start_chats(&account), "people": people }))
            }
            "chat.start" => {
                let account = self.chat_account(&args.required("account")?)?;
                let key = self.start_chat(&account, &args.list("people").unwrap_or_default()).await.map_err(|error| failed(&error))?;
                Ok(json!({ "account": key.account, "space": key.space }))
            }
            "chat.attachment" => {
                let account = self.chat_account(&args.required("account")?)?;
                let name = args.string("name").unwrap_or_else(|| "attachment".into());
                let file = self.attachment_file(&account, &args.required("resource")?, &name).await.map_err(|error| failed(&error))?;
                Ok(json!({ "path": file.to_string_lossy() }))
            }
            _ => unreachable!("{command} is a chat command"),
        }
    }

    fn chat_account(&self, id: &str) -> Result<String, EngineError> {
        let id = id.to_lowercase();
        if !self.state().has_account(&id) {
            return Err(EngineError::new("unknown_account", format!("No chat account {id}; `mailstream chat spaces` lists them.")));
        }
        Ok(id)
    }

    fn chat_space(&self, args: &Arguments) -> Result<ChatSpaceKey, EngineError> {
        let account = self.chat_account(&args.required("account")?)?;
        Ok(ChatSpaceKey::new(&account, &args.required("space")?))
    }

    fn spaces_json(&self, account: Option<&str>) -> Value {
        let state = self.state();
        let accounts: Vec<Value> = state
            .accounts
            .iter()
            .filter(|chat| account.is_none_or(|account| account == chat.name))
            .map(|chat| {
                let name = &chat.name;
                let unread = state
                    .listings
                    .get(name)
                    .map_or(0, |listing| listing.list.spaces.iter().filter(|space| space.unread > 0).count());
                json!({
                    "id": name, "label": self.accounts.record(name).map_or_else(|| name.clone(), |record| record.label),
                    "canStart": self.can_start_chats(name), "unread": unread,
                })
            })
            .collect();
        let spaces: Vec<Value> = state
            .rows
            .iter()
            .filter(|row| account.is_none_or(|account| account == row.key.account))
            .map(ChatRow::json)
            .collect();
        json!({ "accounts": accounts, "loading": state.loading, "more": Self::can_load_more(&state, account), "spaces": spaces })
    }

    /// What the store holds of `key`'s history. Senders' pictures go to the avatar store, so
    /// `mail.avatars` finds them by address.
    fn thread_json(&self, key: &ChatSpaceKey) -> Value {
        let state = self.state();
        let empty = Thread::default();
        let thread = state.threads.get(key).unwrap_or(&empty);
        for message in &thread.messages {
            if let Some(url) = &message.sender.avatar_url {
                self.avatars.publish(url, &message.sender.avatar_key());
            }
        }
        let space = state.space(key);
        let me = state.listings.get(&key.account).and_then(|listing| listing.list.me.clone());
        json!({
            "account": key.account, "space": key.space, "title": space.map(|space| &space.title), "me": me,
            "members": space.map_or_else(Vec::new, |space| space.members.iter().map(ChatUser::json).collect()),
            "lastRead": thread.last_read.map(iso8601),
            "messages": thread.messages.iter().map(ChatMessage::json).collect::<Vec<_>>(),
            "pending": thread.pending.iter().map(|pending| json!({
                "id": pending.id, "text": pending.text, "created": iso8601(pending.created), "failure": pending.failure,
            })).collect::<Vec<_>>(),
            "more": thread.more, "loaded": thread.loaded, "failure": thread.failure,
            "loadingEarlier": state.earlier.contains(key),
        })
    }

    async fn send_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let key = self.chat_space(args)?;
        let text = args.required("text")?;
        if text.trim().is_empty() {
            return Err(EngineError::usage("--text is empty."));
        }
        if !args.bool("yes") {
            use unicode_segmentation::UnicodeSegmentation;
            return Err(EngineError::new(
                "confirm",
                format!(
                    "Would send {} characters to {} from {}. Re-run with --yes.",
                    text.graphemes(true).count(),
                    key.space,
                    key.account
                ),
            ));
        }
        let Some((id, task)) = self.send(&text, args.string("thread"), &key) else {
            return Err(EngineError::usage("--text is empty."));
        };
        let _ = task.await;
        let failure = {
            let mut state = self.state();
            let failure = state
                .threads
                .get_mut(&key)
                .and_then(|thread| {
                    let index = thread.pending.iter().position(|pending| pending.id == id && pending.failure.is_some())?;
                    Some(thread.pending.remove(index))
                })
                .and_then(|pending| pending.failure);
            if failure.is_some() {
                self.thread_changed(&state, &key);
            }
            failure
        };
        if let Some(failure) = failure {
            return Err(EngineError::new("provider", failure));
        }
        Ok(json!({ "sent": true, "account": key.account, "space": key.space }))
    }
}
