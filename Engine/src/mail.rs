//! Receiving mail, as the Swift engine's `MailStore` and its control commands do it
//! (`Sources/MailstreamKit/Store/MailStore.swift`, `Control/MailStore+Control.swift`,
//! `Control/ControlService.swift`): the accounts with their identities and mailboxes, every
//! mailbox's listing (memory, then mail.db, then the provider), conversations threaded from them,
//! and the commands `mailboxes`, `mail.list`, `mail.search`, `mail.show` and `mail.previews`.
//!
//! Listings paint from mail.db before any request: a listing read from disk is kept in memory,
//! fetched pages merge into it and are written back whole, so mail once fetched stays listed and
//! a cached listing is served without asking the provider again.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, Shared};
use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt;
use serde_json::{Map, Value, json};

use crate::accounts::Accounts;
use crate::answers;
use crate::api::{Api, ApiError, Priority};
use crate::backend::{Backend, Endpoints};
use crate::cache::{CachedListing, MailCache};
use crate::commands::Arguments;
use crate::content::Contents;
use crate::model::{
    Account, Address, Conversation, DISTANT_FUTURE, DISTANT_PAST, Date, Envelope, MailMessage, Mailbox, MailboxKey,
    MailboxRole, date_from_unix, iso8601, now, unix_seconds,
};
use crate::settings::{Setting, Settings};
use crate::tokens::Tokens;
use crate::tagging::{Candidates, Category, ConversationTag, Tagging};
use crate::{EngineError, Events, threader};

mod actions;
mod autoread;
pub use autoread::AutoReadReason;
mod contacts;
mod pending;
mod sending;
mod started;
mod tagsync;
mod undo;
#[cfg(test)]
mod action_tests;
#[cfg(test)]
mod autoread_tests;
#[cfg(test)]
mod interop_tests;
#[cfg(test)]
mod pending_tests;
#[cfg(test)]
mod started_tests;
#[cfg(test)]
mod undo_tests;

pub const PAGE_SIZE: usize = 50;
/// Page fetches one listing request may make past the mail already kept, so a rare filter
/// cannot page through a whole mailbox.
const AGENT_PAGE_ROUNDS: usize = 6;
/// Conversations `mail.*` commands remember by id while the engine runs.
const KNOWN_LIMIT: usize = 5000;
/// How long push notifications for one account gather before the pass that picks them up.
const PUSH_GATHER: std::time::Duration = std::time::Duration::from_millis(500);
/// How long after a push pass the account is looked at once more. A notification can come
/// before the provider lists what it reports, and Gmail drops a mailbox's notifications beyond
/// one a second: mail sent from another client is a burst of changes (the draft saved, then
/// gone, the sent copy added), so its sent copy often lists only after the pass for the draft's
/// notification, and no notification of its own follows (MS-282).
const PUSH_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone, Debug, Default)]
struct Listing {
    envelopes: Vec<Arc<Envelope>>,
    /// The cursor of the page after `envelopes`; nil at the end.
    next: Option<String>,
    /// The latest first page's cursor, which paging falls back to when `next`, kept on disk,
    /// has gone stale.
    restart: Option<String>,
}

impl Listing {
    /// Adds the envelopes not listed yet after the listed ones, in the order given.
    fn append_unknown(&mut self, more: Vec<Envelope>) {
        let known: HashSet<String> = self.envelopes.iter().map(|envelope| envelope.id.clone()).collect();
        self.envelopes.extend(more.into_iter().filter(|envelope| !known.contains(&envelope.id)).map(Arc::new));
    }
}

/// A conversation a command handed out, with the mailboxes it was listed from (what Archive
/// moves it out of).
#[derive(Clone, Debug)]
struct Known {
    conversation: Conversation,
    primary: HashSet<MailboxKey>,
}

#[derive(Default)]
struct State {
    accounts: Vec<Account>,
    mailboxes: HashMap<String, Vec<Mailbox>>,
    listings: HashMap<MailboxKey, Listing>,
    /// First-page requests made per mailbox; only the latest one's answer is applied.
    first_page_requests: HashMap<MailboxKey, u64>,
    apis: HashMap<String, Arc<Api>>,
    backends: HashMap<String, Arc<Backend>>,
    /// Conversations handed out by commands, so later ones can address them by id.
    known: HashMap<String, Known>,
    known_order: VecDeque<String>,
    /// The latest failure, until a refresh clears it.
    problem: Option<String>,
    /// Why each account's mail could not load (`ApiError::cuts_off`), until it loads again: the
    /// account's own "can't connect", which `status` hands every front end.
    account_problems: HashMap<String, String>,
    /// Accounts whose identity and mailboxes are loading (`connect_account`).
    connecting: HashSet<String>,
    /// Refresh passes running (the Swift engine's `isLoading` while any runs): the `sync` event
    /// follows the count leaving and reaching zero, and polling skips a turn while it is not.
    passes: usize,
    /// When the default list last finished refreshing, for `status`.
    last_refresh: Option<Date>,
    /// Every account's first load finished: identities are known and a reconnecting push socket
    /// has a list to catch up.
    connected: bool,
    /// Accounts push reported a change of, waiting out the coalescing delay.
    account_refreshes: HashSet<String>,
    /// Push passes started per account; only an account's latest one looks again (`PUSH_SETTLE`).
    push_passes: HashMap<String, u64>,
    /// The conversations the user started that the inbox lists.
    started: started::Started,
    /// Mail sent from here that Sent doesn't list yet, listed with it.
    pending: Vec<pending::Pending>,
}

type CacheRead = Shared<BoxFuture<'static, ()>>;

/// Conversations for a command: one page of them, the mailboxes they came from, the providers
/// that failed and where the next page starts.
struct AgentListing {
    conversations: Vec<Conversation>,
    primary: HashSet<MailboxKey>,
    failures: Vec<String>,
    next: Option<Cursor>,
    /// Conversations the user started that the provider is to be asked about.
    due: Vec<started::ThreadKey>,
}

/// The position after a conversation in a newest-first listing: its date and id, written
/// `<milliseconds since 1970>|<conversation id>`.
struct Cursor {
    date: f64,
    id: String,
}

impl Cursor {
    fn after(conversation: &Conversation) -> Self {
        Self { date: conversation.latest_date.unwrap_or(DISTANT_PAST), id: conversation.id.clone() }
    }

    fn parse(text: &str) -> Option<Self> {
        let (millis, id) = text.split_once('|')?;
        let millis: i64 = millis.parse().ok()?;
        Some(Self { date: date_from_unix(millis as f64 / 1000.0), id: id.into() })
    }

    fn text(&self) -> String {
        format!("{}|{}", (unix_seconds(self.date) * 1000.0).round() as i64, self.id)
    }

    /// Where the page after this cursor starts in `conversations`.
    fn start(&self, conversations: &[Conversation]) -> usize {
        if let Some(index) = conversations.iter().position(|conversation| conversation.id == self.id) {
            return index + 1;
        }
        conversations
            .iter()
            .position(|conversation| conversation.latest_date.unwrap_or(DISTANT_PAST) < self.date)
            .unwrap_or(conversations.len())
    }
}

pub struct Mail {
    cache: Arc<MailCache>,
    contents: Arc<Contents>,
    tokens: Arc<Tokens>,
    accounts: Arc<Accounts>,
    settings: Arc<Settings>,
    events: Arc<Events>,
    endpoints: Endpoints,
    state: Mutex<State>,
    /// Reads of mailbox listings from mail.db, in flight or done, so each is read at most once
    /// and a later caller awaits the read in flight.
    cache_reads: Mutex<HashMap<MailboxKey, CacheRead>>,
    /// The AI tags of the default list's conversations.
    tagging: Arc<Tagging>,
    /// `contacts.search`'s people, loaded again once they are a minute old.
    contacts: contacts::ContactsLoad,
    /// Marking stale notifications and newsletters read (`autoRead.*`).
    auto_read: autoread::AutoRead,
    /// The steps `mail.undo` can take back.
    undo: undo::Undo,
}

impl Mail {
    /// Starts mail.db's writer on the current runtime. `tool_environment` and `apple_tagger` are
    /// the AI tagger's (`Config::tool_environment`, `Config::apple_tagger`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database: PathBuf,
        cache_root: PathBuf,
        endpoints: Endpoints,
        tokens: Arc<Tokens>,
        accounts: Arc<Accounts>,
        settings: Arc<Settings>,
        events: Arc<Events>,
        tool_environment: Option<HashMap<String, String>>,
        apple_tagger: PathBuf,
    ) -> Self {
        let cache = Arc::new(MailCache::new(database, cache_root));
        let previews = events.clone();
        let contents = Arc::new(Contents::new(cache.clone(), move || previews.emit_soon("previews")));
        let tagging =
            Tagging::new(cache.clone(), contents.clone(), settings.clone(), events.clone(), tool_environment, apple_tagger);
        Self {
            cache,
            contents,
            tokens,
            accounts,
            settings,
            events,
            endpoints,
            state: Mutex::new(State::default()),
            cache_reads: Mutex::new(HashMap::new()),
            tagging,
            contacts: contacts::ContactsLoad::default(),
            auto_read: autoread::AutoRead::new(),
            undo: undo::Undo::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("mail lock")
    }

    #[cfg(test)]
    pub async fn flush(&self) {
        self.cache.flush().await;
    }

    // MARK: - Startup and accounts

    /// Paints the signed-in accounts from mail.db (identities, mailboxes, tags, listings) and
    /// fetches what it held afresh, while every account loads its identity and mailboxes and
    /// fetches its inboxes' first pages on its own (MS-278).
    pub async fn start(self: Arc<Self>) {
        let weak = Arc::downgrade(&self);
        let pushing = Arc::downgrade(&self);
        self.tagging.start(
            move || weak.upgrade().map(|mail| mail.tag_candidates()).unwrap_or_default(),
            move |pushes| {
                let mail = pushing.upgrade();
                async move {
                    match mail {
                        Some(mail) => mail.push_tags(pushes).await,
                        None => HashSet::new(),
                    }
                }
                .boxed()
            },
        );
        self.start_auto_read();
        // As each rebuild of the Swift engine's list does (`MailStore.prefetchCount`): the default
        // list's newest conversations get their preview lines loaded, so a list read a moment
        // later carries them instead of leaving them to `mail.previews`.
        let weak = Arc::downgrade(&self);
        self.events.listen(move |event| {
            if event == "conversations"
                && let Some(mail) = weak.upgrade()
            {
                mail.prefetch_default_snippets();
            }
        });
        self.tagging.restore().await;
        self.restore_started().await;
        let cached = self.cache.load_accounts().await.unwrap_or_default();
        let restored = self.signed_in_accounts(&cached);
        {
            let mut state = self.state();
            state.accounts = restored.clone();
            // Nothing to connect: accounts signed in from now on are the user's as they load.
            state.connected = restored.is_empty();
        }
        if restored.is_empty() {
            return;
        }
        self.events.emit_soon("accounts");
        let names: Vec<String> = restored.iter().map(|account| account.name.clone()).collect();
        self.load_cached_mailboxes(&names).await;
        let held = Self::default_keys(&self.state());
        self.clone().refresh_keys(held.clone());
        self.connect(names, held.into_iter().collect()).await;
    }

    /// Connects every account of `names` (`connect_account`), all at once; `fetching`: the
    /// mailboxes a refresh already fetches.
    async fn connect(self: Arc<Self>, names: Vec<String>, fetching: HashSet<MailboxKey>) {
        futures::future::join_all(names.into_iter().map(|name| self.clone().connect_account(name, &fetching))).await;
        self.state().connected = true;
        self.tagging.schedule();
    }

    /// Loads `name`'s identity and mailboxes, then fetches its default-list mailboxes but
    /// `fetching`'s and its counts: each account on its own, so one that is slow or can't connect
    /// holds up no other. An account already connecting is left to that.
    async fn connect_account(self: Arc<Self>, name: String, fetching: &HashSet<MailboxKey>) {
        if !self.state().connecting.insert(name.clone()) {
            return;
        }
        self.load_account(&name).await;
        let (accounts, keys) = {
            let mut state = self.state();
            state.connecting.remove(&name);
            let keys: Vec<MailboxKey> = Self::default_keys(&state)
                .into_iter()
                .filter(|key| key.account == name && !fetching.contains(key))
                .collect();
            (state.accounts.clone(), keys)
        };
        self.cache.save_accounts(&accounts);
        self.refresh_account_counts(name);
        self.refresh_keys(keys);
    }

    /// The default list changed: front ends list again, the tagger looks at it a moment later
    /// (and finds nothing to tag until every account has connected). Callers may hold the lock.
    fn conversations_changed(&self) {
        self.events.emit_soon("conversations");
        self.tagging.schedule();
    }

    /// Loads the preview lines of the default list's `PREFETCH_COUNT` newest conversations in the
    /// background.
    fn prefetch_default_snippets(&self) {
        const PREFETCH_COUNT: usize = 40;
        let latest: Vec<crate::model::MessageRef> = {
            let state = self.state();
            let primary = Self::unified_mailboxes(&state, MailboxRole::Inbox);
            let context: Vec<MailboxKey> =
                Self::default_keys(&state).into_iter().filter(|key| !primary.contains(key)).collect();
            let fetched: Vec<(MailboxKey, Vec<Arc<Envelope>>)> =
                primary.iter().map(|key| (key.clone(), Self::loaded_envelopes(&state, key))).collect();
            let listed = Self::threads(&state, &fetched, &context, Vec::new(), true);
            listed.conversations.iter().take(PREFETCH_COUNT).map(|conversation| conversation.latest().message_ref()).collect()
        };
        let messages = latest
            .into_iter()
            .map(|message| {
                let backend = self.backend(&message.account);
                (message, backend)
            })
            .collect();
        self.contents.prefetch(messages);
    }

    /// What the tagger may tag: the default list's conversations with a message in an inbox,
    /// once every account has connected (`MailStore.scheduleTagging`).
    fn tag_candidates(&self) -> Candidates {
        let state = self.state();
        if !state.connected {
            return Candidates::default();
        }
        let primary = Self::unified_mailboxes(&state, MailboxRole::Inbox);
        let context: Vec<MailboxKey> = Self::default_keys(&state).into_iter().filter(|key| !primary.contains(key)).collect();
        let fetched: Vec<(MailboxKey, Vec<Arc<Envelope>>)> =
            primary.iter().map(|key| (key.clone(), Self::loaded_envelopes(&state, key))).collect();
        let listed = Self::threads(&state, &fetched, &context, Vec::new(), true);
        let conversations = listed
            .conversations
            .into_iter()
            .filter(|conversation| {
                conversation.messages.iter().any(|message| message.role == MailboxRole::Inbox && primary.contains(&message.key))
            })
            .collect();
        Candidates { conversations, own: Self::own_addresses(&state) }
    }

    pub(crate) fn tagging(&self) -> &Arc<Tagging> {
        &self.tagging
    }

    /// `status`'s accounts: the mail store's, whether their mailboxes have loaded, and why their
    /// mail could not load just now.
    pub(crate) fn account_statuses(&self) -> Vec<answers::AccountStatus> {
        let state = self.state();
        state
            .accounts
            .iter()
            .map(|account| answers::AccountStatus {
                id: account.name.clone(),
                label: account.label.clone(),
                provider: account.provider,
                mailboxes_loaded: state.mailboxes.get(&account.name).is_some_and(|list| !list.is_empty()),
                problem: state.account_problems.get(&account.name).cloned(),
            })
            .collect()
    }

    /// The latest failure, until a refresh clears it.
    pub(crate) fn problem(&self) -> Option<String> {
        self.state().problem.clone()
    }

    /// The signed-in accounts in sidebar order, with the identities `known` has, else the name
    /// and address the sign-in reported.
    fn signed_in_accounts(&self, known: &[Account]) -> Vec<Account> {
        let identities: HashMap<&str, &Address> = known
            .iter()
            .rev()
            .filter_map(|account| account.identity.as_ref().map(|identity| (account.name.as_str(), identity)))
            .collect();
        self.accounts
            .list()
            .into_iter()
            .map(|record| Account {
                identity: Some(
                    identities
                        .get(record.id.as_str())
                        .map(|identity| (*identity).clone())
                        .unwrap_or_else(|| Address::new(record.display_name.clone(), &record.id)),
                ),
                name: record.id,
                label: record.label,
                provider: record.provider,
            })
            .collect()
    }

    /// Follows sign-ins, removals and renames: added accounts load, removed ones drop their
    /// mailboxes, listings, content and backends. An account without mailboxes loads again.
    pub fn accounts_changed(self: &Arc<Self>) {
        let (load, removed) = {
            let mut state = self.state();
            let next = self.signed_in_accounts(&state.accounts);
            let before: HashSet<String> = state.accounts.iter().map(|account| account.name.clone()).collect();
            let load: Vec<String> = next
                .iter()
                .map(|account| account.name.clone())
                .filter(|name| !before.contains(name) || state.mailboxes.get(name).is_none_or(Vec::is_empty))
                .collect();
            let mut removed = HashSet::new();
            if next != state.accounts {
                removed = before.into_iter().filter(|name| !next.iter().any(|account| &account.name == name)).collect();
                state.accounts = next;
                self.cache.save_accounts(&state.accounts);
                for name in &removed {
                    state.mailboxes.remove(name);
                    state.apis.remove(name);
                    state.backends.remove(name);
                    state.account_problems.remove(name);
                }
                state.listings.retain(|key, _| !removed.contains(&key.account));
                state.pending.retain(|pending| !removed.contains(&pending.key.account));
                state.first_page_requests.retain(|key, _| !removed.contains(&key.account));
                state.known.retain(|_, known| !removed.contains(&known.conversation.account));
                let known = &state.known;
                let order: VecDeque<String> = state.known_order.iter().filter(|id| known.contains_key(*id)).cloned().collect();
                state.known_order = order;
            }
            (load, removed)
        };
        if !removed.is_empty() {
            self.cache_reads.lock().expect("cache reads").retain(|key, _| !removed.contains(&key.account));
            self.contents.forget(&removed);
            self.events.emit_soon("mailboxes");
            self.conversations_changed();
        }
        if load.is_empty() {
            return;
        }
        let mail = self.clone();
        tokio::spawn(async move {
            mail.load_cached_mailboxes(&load).await;
            let none = HashSet::new();
            futures::future::join_all(load.into_iter().map(|name| mail.clone().connect_account(name, &none))).await;
        });
    }

    async fn load_cached_mailboxes(&self, names: &[String]) {
        let loads = names.iter().map(|name| async move { (name, self.cache.load_mailboxes(name).await) });
        let mut changed = false;
        for (name, list) in futures::future::join_all(loads).await {
            let Some(list) = list else { continue };
            let mut state = self.state();
            if !state.mailboxes.contains_key(name) {
                state.mailboxes.insert(name.clone(), sorted(list));
                changed = true;
            }
        }
        if changed {
            self.events.emit_soon("mailboxes");
        }
    }

    /// Where attachments, inline parts and Gmail's envelope caches live.
    pub fn cache_root(&self) -> PathBuf {
        self.cache.root.clone()
    }

    /// mail.db and the cache folder, which other stores (chat) keep their data in too.
    pub fn cache(&self) -> Arc<MailCache> {
        self.cache.clone()
    }

    /// The account's authorized requests, one per account (their concurrency limits and quota
    /// budget are the account's); nil once it is signed out. Other stores (contacts, avatars,
    /// chat) send through this too.
    pub fn api(&self, account: &str) -> Option<Arc<Api>> {
        if let Some(api) = self.state().apis.get(account) {
            return Some(api.clone());
        }
        let record = self.accounts.record(account)?;
        let api = Arc::new(crate::backend::api(account, record.provider, self.tokens.clone()));
        Some(self.state().apis.entry(account.to_string()).or_insert(api).clone())
    }

    /// The account's mail backend; nil once it is signed out.
    pub fn backend(&self, account: &str) -> Option<Arc<Backend>> {
        if let Some(backend) = self.state().backends.get(account) {
            return Some(backend.clone());
        }
        let record = self.accounts.record(account)?;
        let api = self.api(account)?;
        let backend = Arc::new(crate::backend::make(api, record.provider, &self.endpoints, &self.cache.root));
        Some(self.state().backends.entry(account.to_string()).or_insert(backend).clone())
    }

    async fn load_account(&self, name: &str) {
        let Some(backend) = self.backend(name) else { return };
        let (identity, fresh) = tokio::join!(backend.identity(), backend.mailboxes(false));
        match fresh {
            Ok(fresh) => {
                let mut state = self.state();
                if !state.accounts.iter().any(|account| account.name == name) {
                    return;
                }
                let previous: HashMap<String, Mailbox> =
                    state.mailboxes.get(name).into_iter().flatten().map(|mailbox| (mailbox.id.clone(), mailbox.clone())).collect();
                let built: Vec<Mailbox> = fresh
                    .into_iter()
                    .map(|mut mailbox| {
                        let old = previous.get(&mailbox.id);
                        mailbox.unread = mailbox.unread.or(old.and_then(|old| old.unread));
                        mailbox.total = mailbox.total.or(old.and_then(|old| old.total));
                        mailbox
                    })
                    .collect();
                self.cache.save_mailboxes(&built, name);
                state.mailboxes.insert(name.to_string(), sorted(built));
                drop(state);
                self.events.emit_soon("mailboxes");
            }
            Err(error) => {
                self.report(&error, Some(name));
                if error.cuts_off() {
                    self.account_failed(name, &error);
                }
            }
        }
        if let Ok(identity) = identity {
            let mut state = self.state();
            if let Some(account) = state.accounts.iter_mut().find(|account| account.name == name)
                && account.identity.as_ref() != Some(&identity)
            {
                account.identity = Some(identity);
                drop(state);
                // Conversations say "me" for the user's own addresses.
                self.events.emit_soon("accounts");
                self.conversations_changed();
            }
        }
    }

    /// Every account's mailbox counts, fetched now.
    pub fn refresh_counts(self: &Arc<Self>) {
        let names: Vec<String> = self.state().accounts.iter().map(|account| account.name.clone()).collect();
        for name in names {
            self.refresh_account_counts(name);
        }
    }

    fn refresh_account_counts(self: &Arc<Self>, name: String) {
        let Some(backend) = self.backend(&name) else { return };
        let mail = self.clone();
        tokio::spawn(async move {
            let fresh = match backend.mailboxes(true).await {
                Ok(fresh) => fresh,
                Err(error) => {
                    if error.cuts_off() {
                        mail.account_failed(&name, &error);
                    }
                    return;
                }
            };
            let counts: HashMap<String, (Option<i64>, Option<i64>)> =
                fresh.into_iter().map(|mailbox| (mailbox.id, (mailbox.unread, mailbox.total))).collect();
            let mut state = mail.state();
            let Some(list) = state.mailboxes.get(&name) else { return };
            let mut updated = list.clone();
            for mailbox in &mut updated {
                if let Some((unread, total)) = counts.get(&mailbox.id) {
                    mailbox.unread = unread.or(mailbox.unread);
                    mailbox.total = total.or(mailbox.total);
                }
            }
            if Some(&updated) == state.mailboxes.get(&name) {
                return;
            }
            mail.cache.save_mailboxes(&updated, &name);
            state.mailboxes.insert(name, updated);
            drop(state);
            mail.events.emit_soon("mailboxes");
        });
    }

    /// A refresh of the default list is running.
    pub fn is_loading(&self) -> bool {
        self.state().passes > 0
    }

    /// When the default list last finished refreshing.
    pub fn last_refresh(&self) -> Option<Date> {
        self.state().last_refresh
    }

    /// Every account's first load has finished.
    pub fn is_connected(&self) -> bool {
        self.state().connected
    }

    // MARK: - Lookup

    fn mailbox<'a>(state: &'a State, key: &MailboxKey) -> Option<&'a Mailbox> {
        state.mailboxes.get(&key.account)?.iter().find(|mailbox| mailbox.id == key.mailbox)
    }

    fn mailbox_with_role<'a>(state: &'a State, account: &str, role: MailboxRole) -> Option<&'a Mailbox> {
        state.mailboxes.get(account)?.iter().find(|mailbox| mailbox.role == role)
    }

    fn role(state: &State, key: &MailboxKey) -> MailboxRole {
        Self::mailbox(state, key).map_or(MailboxRole::Other, |mailbox| mailbox.role)
    }

    /// Each account's mailbox for `role`; Gmail has no Archive, so its All Mail stands in.
    fn unified_mailboxes(state: &State, role: MailboxRole) -> Vec<MailboxKey> {
        state
            .accounts
            .iter()
            .filter_map(|account| {
                Self::mailbox_with_role(state, &account.name, role)
                    .or_else(|| (role == MailboxRole::Archive).then(|| Self::mailbox_with_role(state, &account.name, MailboxRole::All)).flatten())
                    .map(Mailbox::key)
            })
            .collect()
    }

    /// Lowercased addresses of every account, to say "me".
    fn own_addresses(state: &State) -> HashSet<String> {
        state.accounts.iter().flat_map(|account| [account.name.clone(), account.email().to_lowercase()]).collect()
    }

    /// The mailboxes the default list shows (every inbox) and those that thread into it (sent).
    fn default_keys(state: &State) -> Vec<MailboxKey> {
        let primary = Self::unified_mailboxes(state, MailboxRole::Inbox);
        let context = Self::context_keys(state, &primary);
        primary.into_iter().chain(context).collect()
    }

    /// Inbox and sent of the accounts of `keys`, so conversations show both directions; none
    /// for trash, junk, drafts and All Mail alone.
    fn context_keys(state: &State, keys: &[MailboxKey]) -> Vec<MailboxKey> {
        let roles: HashSet<MailboxRole> = keys.iter().map(|key| Self::role(state, key)).collect();
        if roles.iter().all(|role| matches!(role, MailboxRole::Trash | MailboxRole::Junk | MailboxRole::Drafts | MailboxRole::All)) {
            return Vec::new();
        }
        let mut accounts: Vec<&str> = keys.iter().map(|key| key.account.as_str()).collect();
        accounts.sort();
        accounts.dedup();
        accounts
            .into_iter()
            .flat_map(|account| {
                [MailboxRole::Inbox, MailboxRole::Sent]
                    .into_iter()
                    .filter_map(move |role| Self::mailbox_with_role(state, account, role).map(Mailbox::key))
            })
            .filter(|key| !keys.contains(key))
            .collect()
    }

    /// The listing of `key` as held, with the mail sent from here that it doesn't list yet.
    fn loaded_envelopes(state: &State, key: &MailboxKey) -> Vec<Arc<Envelope>> {
        let mut envelopes = state.listings.get(key).map(|listing| listing.envelopes.clone()).unwrap_or_default();
        envelopes.extend(state.pending.iter().filter(|pending| pending.key == *key).map(|pending| pending.envelope.clone()));
        envelopes
    }

    fn has_next_page(state: &State, key: &MailboxKey) -> bool {
        state.listings.get(key).is_some_and(|listing| listing.next.is_some())
    }

    /// The tag `conversation` lists under while tagging is on: its AI tag, its waiting flag
    /// lapsed by now, else the provisional one the provider's sorting gives (`tagging::provisional`).
    fn tag(&self, conversation: &Conversation) -> Option<ConversationTag> {
        self.tagging.listed_tag(conversation)
    }

    // MARK: - Listings

    /// Reads the mail.db listings of mailboxes not loaded yet, all at once, and awaits reads
    /// already in flight.
    async fn load_cached_listings(self: &Arc<Self>, keys: &[MailboxKey]) {
        let reads: Vec<CacheRead> = {
            let state = self.state();
            let mut reads = self.cache_reads.lock().expect("cache reads");
            keys.iter()
                .filter_map(|key| {
                    if let Some(read) = reads.get(key) {
                        return Some(read.clone());
                    }
                    if state.listings.contains_key(key) {
                        return None;
                    }
                    let (mail, target) = (self.clone(), key.clone());
                    let key = target.clone();
                    let read: CacheRead = tokio::spawn(async move {
                        let Some(cached) = mail.cache.load_listing(&key).await else { return };
                        let mut state = mail.state();
                        if state.listings.contains_key(&key) || !state.accounts.iter().any(|account| account.name == key.account) {
                            return;
                        }
                        state.listings.insert(key.clone(), Listing { envelopes: cached.envelopes, next: cached.next, restart: None });
                        let shown = Self::default_keys(&state).contains(&key);
                        drop(state);
                        if shown {
                            mail.conversations_changed();
                        }
                    })
                    .map(drop)
                    .boxed()
                    .shared();
                    reads.insert(target, read.clone());
                    Some(read)
                })
                .collect()
        };
        futures::future::join_all(reads).await;
    }

    /// The default list's mailboxes fetched afresh, each account's on its own, and accounts whose
    /// mailboxes never loaded tried again (`connect_account`).
    pub fn refresh(self: Arc<Self>) {
        let (keys, unloaded) = {
            let mut state = self.state();
            if state.problem.take().is_some() {
                self.events.emit_soon("problems");
            }
            let unloaded: Vec<String> = state
                .accounts
                .iter()
                .map(|account| account.name.clone())
                .filter(|name| state.mailboxes.get(name).is_none_or(Vec::is_empty) && !state.connecting.contains(name))
                .collect();
            (Self::default_keys(&state), unloaded)
        };
        for name in unloaded {
            let mail = self.clone();
            tokio::spawn(async move {
                let none = HashSet::new();
                mail.connect_account(name, &none).await;
            });
        }
        self.refresh_keys(keys);
    }

    /// One refresh pass: `keys`' first pages fetched afresh, after reading those not loaded yet
    /// from mail.db. `is_loading` holds while any pass runs.
    fn refresh_keys(self: Arc<Self>, keys: Vec<MailboxKey>) {
        if keys.is_empty() {
            return;
        }
        {
            let mut state = self.state();
            state.passes += 1;
            if state.passes == 1 {
                self.events.emit_soon("sync");
            }
        }
        tokio::spawn(async move {
            self.load_cached_listings(&keys).await;
            self.fetch_first_pages(&keys).await;
            let mut state = self.state();
            state.passes -= 1;
            if state.passes == 0 {
                state.last_refresh = Some(now());
                drop(state);
                self.events.emit_soon("sync");
            }
        });
    }

    /// Picks up a change push reported for `account`: its mailboxes in the default list and its
    /// counts. Bursts (a sync touching many messages) coalesce into one pass half a second later,
    /// and the latest pass looks again once the change has settled (`PUSH_SETTLE`).
    pub fn refresh_account(self: &Arc<Self>, account: &str) {
        if !self.state().account_refreshes.insert(account.to_string()) {
            return;
        }
        let (mail, account) = (self.clone(), account.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(PUSH_GATHER).await;
            let pass = {
                let mut state = mail.state();
                state.account_refreshes.remove(&account);
                let passes = state.push_passes.entry(account.clone()).or_default();
                *passes += 1;
                *passes
            };
            if !mail.refresh_account_now(&account).await {
                return;
            }
            tokio::time::sleep(PUSH_SETTLE).await;
            // A later notification's pass looks again after itself.
            if mail.state().push_passes.get(&account) == Some(&pass) {
                mail.refresh_account_now(&account).await;
            }
        });
    }

    /// `account`'s mailboxes in the default list and its counts, fetched now; false when the
    /// account is gone.
    async fn refresh_account_now(self: &Arc<Self>, account: &str) -> bool {
        let keys: Vec<MailboxKey> = {
            let state = self.state();
            if !state.accounts.iter().any(|known| known.name == account) {
                return false;
            }
            Self::default_keys(&state).into_iter().filter(|key| key.account == account).collect()
        };
        self.refresh_account_counts(account.to_string());
        self.fetch_first_pages(&keys).await;
        true
    }

    /// First pages of `keys`, and the keys that failed. Each page is taken in as it arrives, so a
    /// slow or failing mailbox holds up no other (MS-278); an account can't connect from its first
    /// answer that says so (`account_failed`) and has loaded once its pages are all in without one
    /// (`account_loaded`). A key's page is taken in only if no later request for it was made, so a
    /// slow older pass never overwrites a newer one; its answer still tells whether the account
    /// connects.
    async fn fetch_first_pages(self: &Arc<Self>, keys: &[MailboxKey]) -> HashMap<MailboxKey, ApiError> {
        let mut tickets = HashMap::new();
        {
            let mut state = self.state();
            for key in keys {
                let ticket = state.first_page_requests.entry(key.clone()).or_insert(0);
                *ticket += 1;
                tickets.insert(key.clone(), *ticket);
            }
        }
        let asked: Vec<(MailboxKey, Arc<Backend>)> =
            keys.iter().filter_map(|key| self.backend(&key.account).map(|backend| (key.clone(), backend))).collect();
        // Per account: its pages still out, and whether one was cut off.
        let mut accounts: HashMap<String, (usize, bool)> = HashMap::new();
        for (key, _) in &asked {
            accounts.entry(key.account.clone()).or_default().0 += 1;
        }
        let mut jobs: FuturesUnordered<_> = asked
            .into_iter()
            .map(|(key, backend)| async move {
                let result = backend.envelopes(&key.mailbox, None, PAGE_SIZE, Priority::Interactive).await;
                (key, result)
            })
            .collect();
        let mut failures = HashMap::new();
        while let Some((key, result)) = jobs.next().await {
            // Only the latest request's page is taken in; any answer says whether the account
            // connects.
            let result = {
                let mut state = self.state();
                if !state.accounts.iter().any(|account| account.name == key.account) {
                    continue;
                }
                let latest = state.first_page_requests.get(&key) == tickets.get(&key);
                result.map(|page| {
                    if latest {
                        let merged = self.merge_first_page(&mut state, page.envelopes, page.next, &key);
                        let settled = self.settle_pending(&mut state, &key);
                        if (merged || settled) && Self::default_keys(&state).contains(&key) {
                            self.conversations_changed();
                        }
                    }
                })
            };
            let tally = accounts.get_mut(&key.account).expect("every asked account is counted");
            tally.0 -= 1;
            if let Err(error) = result {
                self.report(&error, Some(&key.account));
                // Said at once: the account's other pages may wait out their own timeouts.
                if error.cuts_off() {
                    tally.1 = true;
                    self.account_failed(&key.account, &error);
                }
                failures.insert(key.clone(), error);
            }
            if *tally == (0, false) {
                self.account_loaded(&key.account);
            }
        }
        failures
    }

    /// Returns whether anything changed, so an unchanged mailbox costs nothing.
    fn merge_first_page(&self, state: &mut State, envelopes: Vec<Envelope>, next: Option<String>, key: &MailboxKey) -> bool {
        let before = state.listings.get(key);
        let mut listing = Listing { envelopes: envelopes.into_iter().map(Arc::new).collect(), next: next.clone(), restart: next.clone() };
        if next.is_some()
            && let Some(before) = before
        {
            let oldest = listing.envelopes.iter().filter_map(|envelope| envelope.date).reduce(f64::min).unwrap_or(DISTANT_PAST);
            let ids: HashSet<&str> = listing.envelopes.iter().map(|envelope| envelope.id.as_str()).collect();
            let older: Vec<Arc<Envelope>> = before
                .envelopes
                .iter()
                .filter(|envelope| !ids.contains(envelope.id.as_str()) && envelope.date.unwrap_or(DISTANT_PAST) < oldest)
                .cloned()
                .collect();
            // Paging resumes after the older mail kept when the first page reaches it. Past a
            // gap (more new mail than a page) it goes on from the first page.
            if !older.is_empty()
                && let Some(next) = &before.next
                && before.envelopes.iter().any(|envelope| ids.contains(envelope.id.as_str()))
            {
                listing.next = Some(next.clone());
            }
            listing.envelopes.extend(older);
        }
        let unchanged = before.is_some_and(|before| {
            before.next == listing.next
                && before.envelopes.len() == listing.envelopes.len()
                && before.envelopes.iter().zip(&listing.envelopes).all(|(a, b)| a == b)
        });
        if unchanged {
            if let Some(kept) = state.listings.get_mut(key) {
                kept.restart = next;
            }
            return false;
        }
        self.cache.save_listing(CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() }, key);
        state.listings.insert(key.clone(), listing);
        true
    }

    /// The next page of each of `keys` that has one, fetched at once and kept in mail.db with
    /// the cursor after it; answers the keys whose page failed.
    async fn fetch_next_pages(self: &Arc<Self>, keys: &[MailboxKey]) -> HashMap<MailboxKey, ApiError> {
        let jobs: Vec<(MailboxKey, String)> = {
            let state = self.state();
            keys.iter()
                .filter_map(|key| state.listings.get(key).and_then(|listing| listing.next.clone()).map(|next| (key.clone(), next)))
                .collect()
        };
        let mut running: FuturesUnordered<_> = jobs
            .into_iter()
            .filter_map(|(key, cursor)| self.backend(&key.account).map(|backend| (key, cursor, backend)))
            .map(|(key, cursor, backend)| async move {
                let result = backend.envelopes(&key.mailbox, Some(&cursor), PAGE_SIZE, Priority::Interactive).await;
                (key, cursor, result)
            })
            .collect();
        let mut grew = false;
        let mut failures = HashMap::new();
        while let Some((key, cursor, result)) = running.next().await {
            let mut state = self.state();
            let Some(listing) = state.listings.get_mut(&key) else { continue };
            if listing.next.as_deref() != Some(cursor.as_str()) {
                continue;
            }
            match result {
                Ok(page) => {
                    let count = listing.envelopes.len();
                    listing.append_unknown(page.envelopes);
                    listing.next = page.next;
                    grew |= listing.envelopes.len() != count;
                    let kept = CachedListing { envelopes: listing.envelopes.clone(), next: listing.next.clone() };
                    self.cache.save_listing(kept, &key);
                    grew |= self.settle_pending(&mut state, &key);
                }
                Err(error) => {
                    // A cursor kept on disk went stale: page on from the first page's instead.
                    if [400, 404, 410].contains(&error.status)
                        && let Some(restart) = listing.restart.clone()
                        && restart != cursor
                    {
                        listing.next = Some(restart);
                    } else {
                        drop(state);
                        self.report(&error, Some(&key.account));
                        if error.cuts_off() {
                            self.account_failed(&key.account, &error);
                        }
                        failures.insert(key, error);
                    }
                }
            }
        }
        if grew {
            let shown = Self::default_keys(&self.state());
            if keys.iter().any(|key| shown.contains(key)) {
                self.conversations_changed();
            }
        }
        failures
    }

    fn messages(state: &State, key: &MailboxKey, envelopes: &[Arc<Envelope>]) -> Vec<MailMessage> {
        let role = Self::role(state, key);
        let shared = Arc::new(key.clone());
        envelopes.iter().map(|envelope| MailMessage { key: shared.clone(), envelope: envelope.clone(), role }).collect()
    }

    /// Threads `fetched` (the listed mailboxes) with `context`'s loaded mail. `inboxes` (every
    /// listed mailbox is an inbox) adds the conversations the user started and folds one
    /// sender's repeats.
    fn threads(
        state: &State,
        fetched: &[(MailboxKey, Vec<Arc<Envelope>>)],
        context: &[MailboxKey],
        failures: Vec<String>,
        inboxes: bool,
    ) -> AgentListing {
        let mut primary_keys: Vec<&(MailboxKey, Vec<Arc<Envelope>>)> = fetched.iter().collect();
        primary_keys.sort_by(|a, b| a.0.cmp(&b.0));
        let primary: Vec<MailMessage> =
            primary_keys.iter().flat_map(|(key, envelopes)| Self::messages(state, key, envelopes)).collect();
        let context: Vec<MailMessage> =
            context.iter().flat_map(|key| Self::messages(state, key, &Self::loaded_envelopes(state, key))).collect();
        let due = std::cell::RefCell::new(Vec::new());
        // Inboxes also list the conversations the user started.
        let admit = |conversation: &Conversation| {
            inboxes && started::started_key(conversation).is_some_and(|key| state.started.lists(&key, &mut due.borrow_mut()))
        };
        let threaded = threader::conversations(primary, context, &admit);
        let conversations = if inboxes { threader::merge_repeats(threaded) } else { threaded };
        AgentListing {
            conversations,
            primary: fetched.iter().map(|(key, _)| key.clone()).collect(),
            failures,
            next: None,
            due: due.into_inner(),
        }
    }

    /// One page of `keys`' conversations that pass `include`, newest first. Unless `cached`, the
    /// first page fetches the providers' first pages, and every page fetches further pages until
    /// it is complete: every mailbox with more mail has been listed back past the page's oldest
    /// conversation, Sent too for inboxes, which list the conversations the user started. A
    /// mailbox whose page failed is not asked again for this listing. `cached` answers what is
    /// held without a request, so a front end's list never waits on a provider (MS-278).
    async fn agent_listing(
        self: &Arc<Self>,
        keys: &[MailboxKey],
        cursor: Option<&Cursor>,
        limit: usize,
        cached: bool,
        include: &(dyn Fn(&State, &Conversation) -> bool + Sync),
    ) -> AgentListing {
        let mut failures: Vec<String> = keys
            .iter()
            .filter(|key| self.accounts.record(&key.account).is_none())
            .map(|key| format!("{}: signed out", key.account))
            .collect();
        let context = Self::context_keys(&self.state(), keys);
        let all: Vec<MailboxKey> = keys.iter().chain(&context).cloned().collect();
        self.load_cached_listings(&all).await;
        if cursor.is_none() && !cached {
            let failed = self.fetch_first_pages(&all).await;
            let state = self.state();
            for key in keys {
                if let Some(error) = failed.get(key) {
                    let name = Self::mailbox(&state, key).map_or(key.mailbox.as_str(), Mailbox::display_name);
                    failures.push(format!("{} {}: {}", key.account, name, error.message));
                }
            }
        }
        failures.sort();
        let mut given_up: HashSet<MailboxKey> = HashSet::new();
        let mut round = 0;
        loop {
            let step = {
                let state = self.state();
                let inboxes = !keys.is_empty() && keys.iter().all(|key| Self::role(&state, key) == MailboxRole::Inbox);
                let paged: Vec<&MailboxKey> = keys
                    .iter()
                    .chain(context.iter().filter(|key| inboxes && Self::role(&state, key) == MailboxRole::Sent))
                    .collect();
                let fetched: Vec<(MailboxKey, Vec<Arc<Envelope>>)> =
                    keys.iter().map(|key| (key.clone(), Self::loaded_envelopes(&state, key))).collect();
                let listed = Self::threads(&state, &fetched, &context, failures.clone(), inboxes);
                let matching: Vec<Conversation> =
                    listed.conversations.iter().filter(|conversation| include(&state, conversation)).cloned().collect();
                let start = cursor.map_or(0, |cursor| cursor.start(&matching));
                let end = (start + limit).min(matching.len());
                // The page's oldest date when the page is full; a short page needs every
                // mailbox exhausted.
                let floor = (start + limit <= matching.len()).then(|| matching[end - 1].latest_date.unwrap_or(DISTANT_PAST));
                let short: Vec<MailboxKey> = paged
                    .iter()
                    .copied()
                    .filter(|key| {
                        if cached || given_up.contains(*key) || !Self::has_next_page(&state, key) {
                            return false;
                        }
                        let Some(floor) = floor else { return true };
                        let oldest = Self::loaded_envelopes(&state, key)
                            .iter()
                            .filter_map(|envelope| envelope.date)
                            .reduce(f64::min)
                            .unwrap_or(DISTANT_FUTURE);
                        oldest > floor
                    })
                    .cloned()
                    .collect();
                if short.is_empty() || round == AGENT_PAGE_ROUNDS {
                    let page: Vec<Conversation> = matching[start..end].to_vec();
                    let more = matching.len() > end || paged.iter().any(|key| Self::has_next_page(&state, key));
                    let next = if more { page.last().map(Cursor::after) } else { None };
                    Ok(AgentListing { conversations: page, next, ..listed })
                } else {
                    Err(short)
                }
            };
            match step {
                Ok(mut listing) => {
                    self.check_started(std::mem::take(&mut listing.due));
                    return listing;
                }
                Err(short) => {
                    let failed = self.fetch_next_pages(&short).await;
                    let state = self.state();
                    for (key, error) in failed {
                        let name = Self::mailbox(&state, &key).map_or(key.mailbox.as_str(), Mailbox::display_name);
                        failures.push(format!("{} {}: {}", key.account, name, error.message));
                        given_up.insert(key);
                    }
                    failures.sort();
                    round += 1;
                }
            }
        }
    }

    /// The providers' search over every mailbox of `accounts` but trash and junk; with `held`,
    /// the mail already loaded instead (`held_search`), and with `quick`, Gmail's answer without
    /// the attachment check of messages it hasn't shown before (MS-352).
    async fn agent_search(
        self: &Arc<Self>,
        accounts: &[String],
        query: &str,
        page_size: usize,
        held: bool,
        quick: bool,
    ) -> AgentListing {
        let jobs: Vec<(String, Arc<Backend>)> =
            accounts.iter().filter_map(|account| self.backend(account).map(|backend| (account.clone(), backend))).collect();
        let mut failures: Vec<String> = accounts
            .iter()
            .filter(|account| !jobs.iter().any(|(name, _)| name == *account))
            .map(|account| format!("{account}: signed out"))
            .collect();
        // The screenshot mode's stand-in provider holds no mail (MS-331).
        #[cfg(feature = "shots")]
        let held = held || crate::shots::running();
        let mut running: FuturesUnordered<_> = jobs
            .into_iter()
            .map(|(account, backend)| async move {
                if held {
                    let hits = self.held_search(&account, query).await;
                    return (account, Ok(hits));
                }
                let result = backend.search(query, page_size, quick).await;
                (account, result)
            })
            .collect();
        let mut fetched: Vec<(MailboxKey, Vec<Arc<Envelope>>)> = Vec::new();
        while let Some((account, result)) = running.next().await {
            match result {
                Ok(hits) => {
                    let state = self.state();
                    for hit in hits {
                        let key = MailboxKey::new(&account, &hit.mailbox);
                        let Some(role) = Self::mailbox(&state, &key).map(|mailbox| mailbox.role) else { continue };
                        if matches!(role, MailboxRole::Trash | MailboxRole::Junk) {
                            continue;
                        }
                        match fetched.iter_mut().find(|(known, _)| *known == key) {
                            Some((_, envelopes)) => envelopes.push(Arc::new(hit.envelope)),
                            None => fetched.push((key, vec![Arc::new(hit.envelope)])),
                        }
                    }
                }
                Err(error) => failures.push(format!("{account}: {}", error.message)),
            }
        }
        failures.sort();
        let primary: Vec<MailboxKey> = fetched.iter().map(|(key, _)| key.clone()).collect();
        let context: Vec<MailboxKey> = Self::context_keys(&self.state(), &primary)
            .into_iter()
            .filter(|key| !primary.contains(key))
            .collect();
        self.load_cached_listings(&context).await;
        Self::threads(&self.state(), &fetched, &context, failures, false)
    }

    /// What mail.db lists of `account` that matches `query` (`held_match`): no provider is asked.
    async fn held_search(self: &Arc<Self>, account: &str, query: &str) -> Vec<crate::backend::SearchHit> {
        let keys: Vec<MailboxKey> =
            self.state().mailboxes.get(account).into_iter().flatten().map(|mailbox| MailboxKey::new(account, &mailbox.id)).collect();
        self.load_cached_listings(&keys).await;
        let state = self.state();
        keys.iter()
            .filter_map(|key| Some((key, state.listings.get(key)?)))
            .flat_map(|(key, listing)| listing.envelopes.iter().map(move |envelope| (key, envelope)))
            .filter(|(_, envelope)| held_match(envelope, query))
            .map(|(key, envelope)| crate::backend::SearchHit { mailbox: key.mailbox.clone(), envelope: Envelope::clone(envelope) })
            .collect()
    }

    // MARK: - Commands

    pub(crate) async fn run(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "mailboxes" => self.mailboxes_command(args),
            "mail.list" => self.list(args, None).await,
            "mail.search" => {
                let query = args.required("query")?;
                self.list(args, Some(query)).await
            }
            "mail.show" => self.show(args).await,
            "mail.previews" => self.previews(args).await,
            "mail.read" | "mail.star" | "mail.archive" | "mail.trash" | "mail.restore" => self.act(command, args).await,
            "mail.autoread" | "mail.autoread.undo" => self.auto_read_command(command, args).await,
            "mail.undo" => self.undo_command(args).await,
            "mail.send" => self.send_command(args).await,
            "mail.reply" => self.reply_command(args).await,
            _ => unreachable!("{command} is a mail command"),
        }
    }

    fn unknown_account(id: &str) -> EngineError {
        EngineError::new("unknown_account", format!("No account {id}; `mailstream accounts list` shows them."))
    }

    /// The `account` filter's account, when the mail store has it.
    fn account_filter(&self, args: &Arguments) -> Result<Option<String>, EngineError> {
        let Some(id) = args.string("account").map(|id| id.to_lowercase()) else { return Ok(None) };
        if !self.state().accounts.iter().any(|account| account.name == id) {
            return Err(Self::unknown_account(&id));
        }
        Ok(Some(id))
    }

    fn account_names(&self, args: &Arguments) -> Result<Vec<String>, EngineError> {
        Ok(match self.account_filter(args)? {
            Some(id) => vec![id],
            None => self.state().accounts.iter().map(|account| account.name.clone()).collect(),
        })
    }

    fn mailboxes_command(&self, args: &Arguments) -> Result<Value, EngineError> {
        let names = self.account_names(args)?;
        let state = self.state();
        let mut out = Map::new();
        for name in names {
            let boxes: Vec<answers::Mailbox> = state
                .mailboxes
                .get(&name)
                .into_iter()
                .flatten()
                .map(|mailbox| answers::Mailbox {
                    id: mailbox.id.clone(),
                    name: mailbox.display_name().into(),
                    role: mailbox.role,
                    unread: mailbox.unread,
                    total: mailbox.total,
                })
                .collect();
            out.insert(name, answers::json(&boxes));
        }
        Ok(Value::Object(out))
    }

    fn mailbox_keys(&self, args: &Arguments) -> Result<Vec<MailboxKey>, EngineError> {
        let account = self.account_filter(args)?;
        let names = args.list("mailbox").unwrap_or_else(|| vec!["inbox".into()]);
        let state = self.state();
        let mut keys: Vec<MailboxKey> = Vec::new();
        for name in names {
            match MailboxRole::named(&name.to_lowercase()).filter(|role| *role != MailboxRole::Other) {
                Some(role) => {
                    let boxes: Vec<MailboxKey> = Self::unified_mailboxes(&state, role)
                        .into_iter()
                        .filter(|key| account.as_ref().is_none_or(|account| &key.account == account))
                        .collect();
                    if boxes.is_empty() {
                        let within = account.as_ref().map(|account| format!(" in {account}")).unwrap_or_default();
                        return Err(EngineError::new("unknown_mailbox", format!("No {} mailbox{within}.", role.as_str())));
                    }
                    keys.extend(boxes);
                }
                None => {
                    let matches: Vec<MailboxKey> = state
                        .accounts
                        .iter()
                        .filter(|known| account.as_ref().is_none_or(|account| &known.name == account))
                        .flat_map(|known| state.mailboxes.get(&known.name).into_iter().flatten())
                        .filter(|mailbox| mailbox.id == name || mailbox.name.to_lowercase() == name.to_lowercase())
                        .map(Mailbox::key)
                        .collect();
                    if matches.is_empty() {
                        return Err(EngineError::new(
                            "unknown_mailbox",
                            format!("No mailbox \"{name}\"; use a role or an id from `mailstream mailboxes`."),
                        ));
                    }
                    keys.extend(matches);
                }
            }
        }
        let mut seen = HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));
        Ok(keys)
    }

    async fn list(self: &Arc<Self>, args: &Arguments, query: Option<String>) -> Result<Value, EngineError> {
        let limit = args.int("limit").unwrap_or(25).clamp(1, 200) as usize;
        // Unknown categories filter nothing, as the Swift engine's `MailCategory(rawValue:)` reads them.
        let category = args.string("category").and_then(|name| Category::named(&name));
        let (unread, starred, waiting) = (args.bool("unread"), args.bool("starred"), args.bool("waiting"));
        let include = move |_: &State, conversation: &Conversation| {
            if unread && !conversation.is_unread {
                return false;
            }
            if starred && !conversation.is_flagged {
                return false;
            }
            if category.is_none() && !waiting {
                return true;
            }
            let tag = self.tag(conversation);
            if let Some(category) = category
                && tag.as_ref().map(|tag| tag.category) != Some(category)
            {
                return false;
            }
            !waiting || tag.is_some_and(|tag| tag.to_respond)
        };
        let listing = match &query {
            Some(query) => {
                let accounts = self.account_names(args)?;
                let (held, quick) = (args.bool("cached"), args.bool("quick"));
                let mut listing = self.agent_search(&accounts, query, limit.clamp(25, 200), held, quick).await;
                let state = self.state();
                listing.conversations =
                    listing.conversations.into_iter().filter(|conversation| include(&state, conversation)).take(limit).collect();
                drop(state);
                listing
            }
            None => {
                let cursor = match args.string("cursor") {
                    Some(text) => {
                        Some(Cursor::parse(&text).ok_or_else(|| EngineError::usage("--cursor: not a `next` from mail list."))?)
                    }
                    None => None,
                };
                let keys = self.mailbox_keys(args)?;
                self.agent_listing(&keys, cursor.as_ref(), limit, args.bool("cached"), &include).await
            }
        };
        if listing.primary.is_empty() && !listing.failures.is_empty() {
            return Err(EngineError::new("provider", listing.failures.join("; ")));
        }
        let conversations: Vec<answers::Conversation> = {
            let mut state = self.state();
            for conversation in &listing.conversations {
                remember(&mut state, conversation, &listing.primary);
            }
            listing.conversations.iter().map(|conversation| self.summary(&state, conversation, &listing.primary)).collect()
        };
        // Previews missing now load in the background; `mail.previews` waits for them. A search's
        // don't: its results are mostly mail not loaded, and Gmail's quota would go on bodies of
        // rows nobody looks at; the front end asks for the rows it shows (MS-352).
        if query.is_none() {
            let prefetch = listing
                .conversations
                .iter()
                .map(|conversation| {
                    let latest = conversation.latest().message_ref();
                    let backend = self.backend(&latest.account);
                    (latest, backend)
                })
                .collect();
            self.contents.prefetch(prefetch);
        }
        let mut answer = answers::json(&answers::Listing {
            conversations,
            failures: listing.failures,
            next: listing.next.map(|cursor| cursor.text()),
        });
        // Searches have no pages.
        if query.is_some() {
            answer.as_object_mut().expect("a listing is an object").remove("next");
        }
        Ok(answer)
    }

    /// `conversation` as lists show it, listed from the mailboxes `primary`.
    fn summary(&self, state: &State, conversation: &Conversation, primary: &HashSet<MailboxKey>) -> answers::Conversation {
        let own = Self::own_addresses(state);
        let latest = conversation.latest();
        let mut seen = HashSet::new();
        let participants: Vec<String> = conversation
            .messages
            .iter()
            .flat_map(|message| message.envelope.from.iter())
            .filter(|address| seen.insert(address.normalized_email()))
            .map(|address| if own.contains(&address.normalized_email()) { "me".into() } else { address.display_name() })
            .collect();
        let mut roles: Vec<MailboxRole> = conversation.messages.iter().map(|message| message.role).collect();
        roles.sort_by_key(|role| role.as_str());
        roles.dedup();
        let tag = self.tag(conversation);
        answers::Conversation {
            id: conversation.id.clone(),
            account: conversation.account.clone(),
            subject: conversation.subject.clone(),
            date: conversation.latest_date.map(iso8601),
            from: latest.envelope.from.first().cloned(),
            from_me: latest.is_from_user(&own),
            peers: conversation_peers(conversation, &own),
            participants,
            message_count: conversation.messages.len() as u32,
            unread: conversation.is_unread,
            starred: conversation.is_flagged,
            has_attachment: conversation.has_attachment,
            mailboxes: roles,
            snippet: self.contents.snippet(&conversation.latest().message_ref()),
            category: tag.as_ref().map(|tag| tag.category),
            waiting: tag.as_ref().is_some_and(|tag| tag.to_respond),
            labels: tag.map(|tag| tag.labels).unwrap_or_default(),
            started: started::is_started(conversation, primary),
            tag: String::new(),
            title: String::new(),
            when: String::new(),
            section: String::new(),
            avatar: Default::default(),
        }
    }

    /// A conversation handed out before, or one of the default list's.
    pub(crate) fn lookup(&self, id: &str) -> Result<Conversation, EngineError> {
        self.known(id).map(|known| known.conversation)
    }

    /// `lookup`'s conversation with the mailboxes it was listed from: the default list's for
    /// one of its conversations.
    fn known(&self, id: &str) -> Result<Known, EngineError> {
        let state = self.state();
        if let Some(found) = state.known.get(id) {
            return Ok(found.clone());
        }
        let keys = Self::default_keys(&state);
        let primary = Self::unified_mailboxes(&state, MailboxRole::Inbox);
        let context: Vec<MailboxKey> = keys.iter().filter(|key| !primary.contains(key)).cloned().collect();
        let fetched: Vec<(MailboxKey, Vec<Arc<Envelope>>)> =
            primary.iter().map(|key| (key.clone(), Self::loaded_envelopes(&state, key))).collect();
        let listed = Self::threads(&state, &fetched, &context, Vec::new(), true);
        if let Some(conversation) = listed.conversations.into_iter().find(|conversation| conversation.id == id) {
            return Ok(Known { conversation, primary: primary.into_iter().collect() });
        }
        Err(EngineError::new(
            "unknown_conversation",
            format!(
                "Unknown conversation {id}. Ids come from `mail list`, `mail search` or `mail show`, and are remembered while the app runs."
            ),
        ))
    }

    async fn previews(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let ids = args.list("ids").unwrap_or_default();
        if ids.is_empty() {
            return Err(EngineError::usage("Give at least one conversation id."));
        }
        let mut jobs = Vec::with_capacity(ids.len());
        for id in ids {
            let latest = self.lookup(&id)?.latest().message_ref();
            jobs.push((id, latest));
        }
        let answers = futures::future::join_all(jobs.into_iter().map(|(id, latest)| async move {
            let backend = self.backend(&latest.account);
            (id, self.contents.preview(&latest, backend).await)
        }))
        .await;
        Ok(Value::Object(answers.into_iter().map(|(id, snippet)| (id, json!(snippet))).collect()))
    }

    async fn show(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let known = self.known(&args.required("id")?)?;
        let conversation = &known.conversation;
        let max_chars = args.int("max-chars").unwrap_or(20_000).max(0) as usize;
        let html = args.bool("html");
        let bodies = futures::future::join_all(conversation.messages.iter().map(|message| async move {
            let reference = message.message_ref();
            let backend = self.backend(&reference.account);
            self.contents.load(&reference, backend).await
        }))
        .await;
        let summary = {
            let state = self.state();
            self.summary(&state, conversation, &known.primary)
        };
        let messages: Vec<Value> = conversation
            .messages
            .iter()
            .zip(bodies)
            .map(|(message, body)| self.message_json(message, body, max_chars, html))
            .collect();
        let mut out = answers::json(&summary);
        out["messages"] = Value::Array(messages);
        Ok(out)
    }

    fn message_json(
        &self,
        message: &MailMessage,
        body: Result<Arc<crate::model::MessageContent>, String>,
        max_chars: usize,
        html: bool,
    ) -> Value {
        let envelope = &message.envelope;
        let mut item = answers::Message {
            id: envelope.id.clone(),
            mailbox: message.mailbox().into(),
            role: message.role,
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            cc: Vec::new(),
            date: envelope.date.map(iso8601),
            subject: envelope.subject.clone(),
            unread: !envelope.is_seen(),
            starred: envelope.is_flagged(),
            text: None,
            truncated: false,
            attachments: Vec::new(),
            html: None,
            error: None,
            invitation: None,
            when: String::new(),
            time: String::new(),
            avatar: Default::default(),
            divider: String::new(),
            continued: false,
        };
        match body {
            Ok(content) => {
                let text = content
                    .text
                    .clone()
                    .filter(|text| !text.is_empty())
                    .or_else(|| content.html.as_deref().map(crate::text::strip_html))
                    .unwrap_or_default();
                let (shown, truncated) = crate::text::prefix_characters(&text, max_chars);
                let reference = message.message_ref();
                item.cc = content.cc.clone();
                item.text = Some(shown.into());
                item.truncated = truncated;
                if html && let Some(body) = &content.html {
                    // Inline parts point at the reader's `mscid:` scheme; other clients read the cached files.
                    item.html = Some(
                        body.replace(&MailCache::part_url_base(&reference), &self.cache.message_directory_url(&reference)),
                    );
                }
                item.attachments = content
                    .attachments
                    .iter()
                    .filter(|attachment| !attachment.inline)
                    .map(|attachment| answers::Attachment {
                        filename: attachment.filename.clone(),
                        mime: attachment.mime.clone(),
                        size: attachment.size,
                        path: self.cache.attachment_path(&attachment.stored_name, &reference).to_string_lossy().into(),
                    })
                    .collect();
                item.invitation = self.invitation_of(&content, &reference).as_ref().map(crate::ics::Invitation::answer);
                answers::json(&item)
            }
            Err(message) => {
                item.error = Some(message);
                // A body that did not load has none of its fields.
                let mut json = answers::json(&item);
                let fields = json.as_object_mut().expect("a message is an object");
                for field in ["cc", "truncated", "attachments"] {
                    fields.remove(field);
                }
                json
            }
        }
    }

    /// The invitation message `message` of conversation `id` carries, and the account it came to.
    pub(crate) async fn invitation(
        self: &Arc<Self>,
        id: &str,
        message: &str,
    ) -> Result<(String, Option<crate::ics::Invitation>), EngineError> {
        let conversation = self.lookup(id)?;
        let found = conversation.messages.iter().find(|candidate| candidate.id() == message).ok_or_else(|| {
            EngineError::new("unknown_message", format!("No message {message} in conversation {id}."))
        })?;
        let reference = found.message_ref();
        let content = self
            .contents
            .load(&reference, self.backend(&reference.account))
            .await
            .map_err(|error| EngineError::new("content_failed", error))?;
        let invitation = self.invitation_of(&content, &reference);
        Ok((reference.account, invitation))
    }

    /// The invitation read when the message was fetched, or from its `.ics` part when it was
    /// fetched before invitations were read (or by the Swift engine).
    fn invitation_of(&self, content: &crate::model::MessageContent, reference: &crate::model::MessageRef) -> Option<crate::ics::Invitation> {
        content.invitation.clone().or_else(|| {
            let part = content.attachments.iter().find(|part| crate::ics::is_calendar(&part.mime, &part.filename))?;
            crate::ics::parse(&std::fs::read(self.cache.attachment_path(&part.stored_name, reference)).ok()?)
        })
    }

    // MARK: - Remote content

    /// Lowercased addresses the user has written to in loaded listings: recipients of sent mail.
    fn correspondents(state: &State) -> HashSet<String> {
        let own = Self::own_addresses(state);
        let mut out = HashSet::new();
        for (key, listing) in &state.listings {
            let sent = Self::role(state, key) == MailboxRole::Sent;
            for envelope in &listing.envelopes {
                if sent || envelope.from.iter().any(|address| own.contains(&address.normalized_email())) {
                    out.extend(envelope.to.iter().map(Address::normalized_email));
                }
            }
        }
        out
    }

    /// Whether remote images and fonts of conversation `id` load on their own, and the senders
    /// that keep them from it: the "known senders" policy of most mail clients. Mail from the
    /// user's own addresses, from anyone they have written to, or from a sender they chose to
    /// trust (`remoteContent.trustedSenders`) loads remote content; junk never does.
    pub fn remote_content(&self, id: &str) -> Result<(bool, Vec<Address>), EngineError> {
        let conversation = self.lookup(id)?;
        let state = self.state();
        let own = Self::own_addresses(&state);
        let trusted: HashSet<String> = match self.settings.value(Setting::TrustedSenders) {
            Value::Array(senders) => senders.iter().filter_map(|sender| sender.as_str().map(str::to_lowercase)).collect(),
            _ => HashSet::new(),
        };
        // Replies inside this thread count too, so a search result's sender is known before the
        // sent mailbox holding the reply is loaded.
        let mut known = Self::correspondents(&state);
        for message in conversation.messages.iter().filter(|message| message.is_from_user(&own)) {
            known.extend(message.envelope.to.iter().map(Address::normalized_email));
        }
        let mut seen = HashSet::new();
        let untrusted: Vec<Address> = conversation
            .messages
            .iter()
            .flat_map(|message| message.envelope.from.iter())
            .filter(|sender| {
                let email = sender.normalized_email();
                !own.contains(&email) && !known.contains(&email) && !trusted.contains(&email) && seen.insert(email)
            })
            .cloned()
            .collect();
        let junk = conversation.messages.iter().any(|message| message.role == MailboxRole::Junk);
        Ok((!junk && untrusted.is_empty(), untrusted))
    }

    // MARK: - Reporting

    fn report(&self, error: &ApiError, context: Option<&str>) {
        let message = match context {
            Some(context) => format!("{context}: {}", error.message),
            None => error.message.clone(),
        };
        self.state().problem = Some(message);
        self.events.emit_soon("problems");
    }

    /// `account` can't get its mail (`ApiError::cuts_off`): `status` says so for it alone until
    /// its mail loads again; `problems` announces the change.
    fn account_failed(&self, account: &str, error: &ApiError) {
        let mut state = self.state();
        if !state.accounts.iter().any(|known| known.name == account) {
            return;
        }
        if state.account_problems.insert(account.into(), error.message.clone()).as_ref() != Some(&error.message) {
            drop(state);
            self.events.emit_soon("problems");
        }
    }

    /// `account`'s mail loaded: its problem, if it had one, goes.
    fn account_loaded(&self, account: &str) {
        if self.state().account_problems.remove(account).is_some() {
            self.events.emit_soon("problems");
        }
    }
}

fn remember(state: &mut State, conversation: &Conversation, primary: &HashSet<MailboxKey>) {
    let known = Known { conversation: conversation.clone(), primary: primary.clone() };
    if state.known.insert(conversation.id.clone(), known).is_none() {
        state.known_order.push_back(conversation.id.clone());
    }
    while state.known_order.len() > KNOWN_LIMIT {
        if let Some(oldest) = state.known_order.pop_front() {
            state.known.remove(&oldest);
        }
    }
}
/// Whom the user's latest message went to when `from_me` is true: its non-own recipients in
/// order (deduplicated), or the last other sender in the thread if it only names the user.
fn conversation_peers(conversation: &Conversation, own: &HashSet<String>) -> Vec<Address> {
    let latest = conversation.latest();
    if !latest.is_from_user(own) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let recipients: Vec<Address> = latest
        .envelope
        .to
        .iter()
        .filter(|address| !own.contains(&address.normalized_email()) && seen.insert(address.normalized_email()))
        .cloned()
        .collect();
    if !recipients.is_empty() {
        return recipients;
    }
    conversation
        .messages
        .iter()
        .rev()
        .flat_map(|message| message.envelope.from.iter())
        .find(|address| !own.contains(&address.normalized_email()))
        .cloned()
        .into_iter()
        .collect()
}


/// Whether held mail, `envelope`, matches a search for `query` (MS-352): each of its words is in
/// the subject or a sender's or recipient's name or address, ignoring case; with an `OR` between
/// words, any one of them. `from:`, `to:`, `cc:` and `subject:` read as their value; any other
/// operator (`is:unread`, `has:attachment`) asks what headers don't hold, so nothing matches and
/// the provider's answer is the first.
fn held_match(envelope: &Envelope, query: &str) -> bool {
    let mut words: Vec<String> = Vec::new();
    let mut any = false;
    for word in query.split_whitespace() {
        match word {
            "OR" => any = true,
            "AND" => {}
            _ => {
                let word = word.trim_matches(|c| matches!(c, '"' | '(' | ')' | '{' | '}'));
                let word = match word.split_once(':') {
                    Some(("from" | "to" | "cc" | "subject", value)) => value,
                    Some(_) => return false,
                    None => word,
                };
                if !word.is_empty() {
                    words.push(word.to_lowercase());
                }
            }
        }
    }
    let people = envelope.from.iter().chain(&envelope.to);
    let fields: Vec<String> = std::iter::once(envelope.subject.as_str())
        .chain(people.flat_map(|person| person.name.as_deref().into_iter().chain([person.email.as_str()])))
        .map(str::to_lowercase)
        .collect();
    let found = |word: &String| fields.iter().any(|field| field.contains(word.as_str()));
    !words.is_empty() && if any { words.iter().any(found) } else { words.iter().all(found) }
}

/// Sidebar order: by role, then name.
fn sorted(mut list: Vec<Mailbox>) -> Vec<Mailbox> {
    list.sort_by(|a, b| {
        (a.role.sort_order(), a.display_name().to_lowercase()).cmp(&(b.role.sort_order(), b.display_name().to_lowercase()))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_cursors() {
        let cursor = Cursor { date: date_from_unix(1_700_000_000.1234), id: "a@b.c|<m@x>".into() };
        assert_eq!(cursor.text(), "1700000000123|a@b.c|<m@x>");
        let parsed = Cursor::parse(&cursor.text()).unwrap();
        assert_eq!(parsed.id, "a@b.c|<m@x>");
        assert!((unix_seconds(parsed.date) - 1_700_000_000.123).abs() < 1e-6);
        assert!(Cursor::parse("nope").is_none());
    }

    #[test]
    fn matches_held_mail_by_subject_and_people() {
        let envelope = Envelope {
            id: "m1".into(),
            thread: None,
            message_id: None,
            in_reply_to: Vec::new(),
            flags: Default::default(),
            subject: "Lunch on Friday".into(),
            from: vec![Address::new(Some("Ann Lee".into()), "ann@x.com")],
            to: vec![Address::new(None, "me@y.com")],
            date: None,
            size: 0,
            has_attachment: None,
            tags: Default::default(),
            provider_category: None,
            important: false,
        };
        assert!(held_match(&envelope, "lunch"));
        assert!(held_match(&envelope, "Friday ann"));
        assert!(held_match(&envelope, "from:ann@x.com"));
        assert!(held_match(&envelope, "dinner OR \"lunch\""));
        assert!(!held_match(&envelope, "lunch dinner"));
        assert!(!held_match(&envelope, "lunch is:unread"));
        assert!(!held_match(&envelope, "  "));
    }

    #[test]
    fn conversation_peers_picks_recipients_or_group_before_prior_sender() {
        let msg = |id: &str, role: MailboxRole, from: Vec<Address>, to: Vec<Address>| MailMessage {
            key: Arc::new(MailboxKey::new("me@x.com", if role == MailboxRole::Sent { "SENT" } else { "INBOX" })),
            envelope: Arc::new(Envelope {
                id: id.into(),
                thread: None,
                message_id: None,
                in_reply_to: Vec::new(),
                flags: Default::default(),
                subject: "Subj".into(),
                from,
                to,
                date: None,
                size: 0,
                has_attachment: None,
                tags: Default::default(),
                provider_category: None,
                important: false,
            }),
            role,
        };
        let me = Address::new(Some("Me".into()), "me@x.com");
        let alice = Address::new(Some("Alice".into()), "alice@x.com");
        let bob = Address::new(Some("Bob".into()), "bob@x.com");
        let group = Address::new(Some("Design Team".into()), "design@x.com");
        let own: HashSet<String> = ["me@x.com".to_string()].into_iter().collect();

        // Sent to multiple people: lists every recipient, deduplicated and without me.
        let multi = Conversation::thread(vec![msg(
            "m1",
            MailboxRole::Sent,
            vec![me.clone()],
            vec![me.clone(), alice.clone(), bob.clone(), Address::new(None, "ALICE@x.com")],
        )]);
        assert_eq!(conversation_peers(&multi, &own), vec![alice.clone(), bob.clone()]);

        // Reply in a group-addressed thread: shows the group rather than the earlier sender.
        let grouped = Conversation::thread(vec![
            msg("m1", MailboxRole::Inbox, vec![alice.clone()], vec![group.clone(), me.clone()]),
            msg("m2", MailboxRole::Sent, vec![me.clone()], vec![group.clone(), me.clone()]),
        ]);
        assert_eq!(conversation_peers(&grouped, &own), vec![group.clone()]);

        // Sent with only self in To: falls back to the last other sender.
        let fallback = Conversation::thread(vec![
            msg("m1", MailboxRole::Inbox, vec![bob.clone()], vec![me.clone()]),
            msg("m2", MailboxRole::Sent, vec![me.clone()], vec![me.clone()]),
        ]);
        assert_eq!(conversation_peers(&fallback, &own), vec![bob]);

        // Incoming latest message: peers stays empty.
        let incoming = Conversation::thread(vec![msg("m1", MailboxRole::Inbox, vec![alice], vec![me])]);
        assert!(conversation_peers(&incoming, &own).is_empty());
    }
}
