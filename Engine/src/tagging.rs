//! AI tags of the inbox conversations, as the Swift engine's `TagStore` keeps them
//! (`Sources/MailstreamKit/Store/TagStore.swift`, `Model/Tags.swift`, the `mail.tag` and
//! `mail.retag` commands of `Control/ControlService.swift`): the user's `omp` CLI, or Apple
//! Intelligence as `aiTagging.backend` says (`omp.rs`), tags the default list's untagged
//! conversations, newest first, one pass at a time, a second after the list last changed, in
//! batches of `aiTagging.batchSize` up to `aiTagging.passLimit` per pass, waiting
//! `aiTagging.backoffSeconds` after a failure, which `status` reports and the `problems` event
//! announces. It runs as the `aiTagging.*` settings say, following settings.json as it changes.
//!
//! Every Mailstream process keeps its own copy of the tags in memory, writes only what it
//! changed to mail.db's `tags` table and reads them all again when any process says it wrote
//! (`ProcessSignal::TAGS`), so a tag set in one engine shows in the others, Swift or Rust.
//!
//! For the accounts `aiTagging.syncAccounts` lists, each pass first syncs the tags with the
//! server as Gmail labels or Outlook categories (`tagsync`): a conversation the server already
//! tags takes that tag and is not sent to the model, and tags made here go there.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::answers::{BatchFailure, TagResult, Tagged};
use crate::cache::MailCache;
use crate::commands::Arguments;
use crate::content::Contents;
use crate::model::{Conversation, Date, now};
use crate::omp::{self, Backend, Item, Options, Tagger, Tools};
use crate::settings::{Setting, Settings};
use crate::signal::ProcessSignal;
use crate::tagsync::{self, Push, Step};
use crate::{EngineError, Events};

/// Smart Inbox bucket assigned by the AI tagger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Written by a person to the user; stays in the main list.
    People,
    /// Automated: receipts, alerts, security notices, shipping, service mail.
    Notification,
    /// Marketing, promotions, digests, mailing-list newsletters.
    Newsletter,
    /// Calendar invitations, event RSVPs and their responses.
    Invitation,
}

impl Category {
    pub const ALL: [Category; 4] = [Self::People, Self::Notification, Self::Newsletter, Self::Invitation];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::People => "people",
            Self::Notification => "notification",
            Self::Newsletter => "newsletter",
            Self::Invitation => "invitation",
        }
    }

    /// The category whose raw value is exactly `name`.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.as_str() == name)
    }
}

/// The tagger's verdict on a conversation, as mail.db keeps it (the Swift engine's
/// `ConversationTag`, dates as seconds since 2001).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationTag {
    pub category: Category,
    /// The latest incoming message needs the user's reply or action and is not from the user.
    /// Newsletters never do.
    #[serde(rename = "toRespond")]
    pub to_respond: bool,
    /// At most two short topic labels, e.g. "Finance", "Travel".
    pub labels: Vec<String>,
    /// When the ask lapses: the day after its deadline or event, or after a routine ask goes
    /// stale. None keeps it waiting until resolved or answered.
    #[serde(rename = "waitingUntil", default, skip_serializing_if = "Option::is_none")]
    pub waiting_until: Option<Date>,
    /// The server has this tag as it is here (`tagsync`); false for a tag made or corrected
    /// here and not yet on the server, and for every tag of an account that doesn't sync.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synced: bool,
}

impl ConversationTag {
    /// `to_respond`, unless the ask has lapsed by `now`.
    pub fn is_waiting(&self, now: Date) -> bool {
        self.to_respond && self.waiting_until.is_none_or(|until| now < until)
    }
}

/// Why tag edits and retagging are refused while tagging is off.
pub const OFF_MESSAGE: &str = "AI tagging is off (aiTagging.enabled).";

/// Settings whose change deserves another try at once: a fixed omp or model, another backend.
const RETRY_SETTINGS: [Setting; 5] = [
    Setting::AiTaggingBackend,
    Setting::AiTaggingModel,
    Setting::AiTaggingOmpPath,
    Setting::AiTaggingPassLimit,
    Setting::AiTaggingBackoffSeconds,
];

/// A list change waits this long for the next before tagging.
const DEBOUNCE: Duration = Duration::from_secs(1);
/// After a push, the server's tags of that conversation are not taken back for this long: a
/// listing fetched before the push may still land and show them as they were.
const PUSH_GRACE: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 60 });
/// A push the server refused is tried again after this long, or once the tag changes.
const PUSH_RETRY: Duration = Duration::from_secs(600);

/// What a pass may tag: the default list's conversations with a message in a listed inbox,
/// newest first, and the user's lowercased addresses.
#[derive(Default)]
pub struct Candidates {
    pub conversations: Vec<Conversation>,
    pub own: HashSet<String>,
}

type Source = Box<dyn Fn() -> Candidates + Send + Sync>;
/// Puts tags on the server (`Mail::push_tags`); answers the keys that landed.
type Pusher = Box<dyn Fn(Vec<Push>) -> BoxFuture<'static, HashSet<String>> + Send + Sync>;

#[derive(Default)]
struct State {
    /// Keyed by `key`, so a new message in a conversation re-tags it; read through `tag_for`.
    tags: HashMap<String, ConversationTag>,
    /// Short user-facing progress or failure text; None when idle.
    status: Option<String>,
    /// `status` is the last run's failure.
    failed: bool,
    enabled: bool,
    /// The `RETRY_SETTINGS` values last seen.
    retry_values: Vec<Value>,
    /// Keys the model was asked about but skipped; automatic passes leave them alone.
    attempted: HashSet<String>,
    /// Counts this process's changes, so a reload that raced one drops its older copy.
    edits: u64,
    backoff_until: Option<Instant>,
    debounce: Option<tokio::task::JoinHandle<()>>,
    /// The pass in flight, and whether another was requested since it started (`CoalescedTask`).
    pass: Option<tokio::task::JoinHandle<()>>,
    pending: bool,
    /// What passes offer, as of the last `schedule` (`TagStore.candidates`); threading the list
    /// is not redone for each pass, so a retag costs no more than in the Swift engine.
    candidates: Arc<Candidates>,
    /// `candidates` reflects the list's last change; false until first read, and after a
    /// change while tagging was off.
    fresh: bool,
    /// The `aiTagging.syncAccounts` value last seen.
    sync_value: Value,
    /// Keys pushed lately (`PUSH_GRACE`).
    pushed: HashMap<String, Instant>,
    /// Pushes the server refused: the tag, and when (`PUSH_RETRY`).
    refused: HashMap<String, (ConversationTag, Instant)>,
    /// `retag_all` cleared the tags and the tagger has not caught up: the server's tags are
    /// not taken meanwhile, so the new ones replace them.
    retagging: bool,
}

pub struct Tagging {
    cache: Arc<MailCache>,
    contents: Arc<Contents>,
    settings: Arc<Settings>,
    events: Arc<Events>,
    tools: Arc<Tools>,
    tagger: Tagger,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    /// The conversations to offer, set once mail starts.
    source: OnceLock<Source>,
    pusher: OnceLock<Pusher>,
    /// Held by the pass running. A pass aborted between awaits finishes that stretch before it
    /// stops; the next one waits for it, so passes never overlap (`CoalescedTask`).
    running: tokio::sync::Mutex<()>,
}

impl Tagging {
    /// Call on the engine's runtime. Follows the `aiTagging.*` settings from now on. omp runs in
    /// `tool_environment`, else the login shell's (`Config::tool_environment`); `apple_tagger`
    /// is the Apple Intelligence helper (`Config::apple_tagger`).
    pub fn new(
        cache: Arc<MailCache>,
        contents: Arc<Contents>,
        settings: Arc<Settings>,
        events: Arc<Events>,
        tool_environment: Option<HashMap<String, String>>,
        apple_tagger: std::path::PathBuf,
    ) -> Arc<Self> {
        let tools = Arc::new(tool_environment.map_or_else(Tools::new, Tools::fixed));
        let tagger = Tagger::new(tools.clone(), cache.root.clone(), apple_tagger);
        let state = State {
            enabled: settings.value(Setting::AiTaggingEnabled) == Value::Bool(true),
            retry_values: RETRY_SETTINGS.iter().map(|setting| settings.value(*setting)).collect(),
            sync_value: settings.value(Setting::AiTaggingSyncAccounts),
            ..State::default()
        };
        let tagging = Arc::new(Self {
            cache,
            contents,
            settings,
            events: events.clone(),
            tools,
            tagger,
            runtime: tokio::runtime::Handle::current(),
            state: Mutex::new(state),
            source: OnceLock::new(),
            pusher: OnceLock::new(),
            running: tokio::sync::Mutex::new(()),
        });
        let weak: Weak<Self> = Arc::downgrade(&tagging);
        events.listen(move |event| {
            if event == "settings"
                && let Some(tagging) = weak.upgrade()
            {
                tagging.settings_changed();
            }
        });
        tagging
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("tagging lock")
    }

    /// Starts offering `source`'s conversations, putting the sync accounts' tags on the server
    /// through `pusher`, and reading the tags other processes write.
    pub fn start(
        self: &Arc<Self>,
        source: impl Fn() -> Candidates + Send + Sync + 'static,
        pusher: impl Fn(Vec<Push>) -> BoxFuture<'static, HashSet<String>> + Send + Sync + 'static,
    ) {
        if self.source.set(Box::new(source)).is_err() {
            return;
        }
        let _ = self.pusher.set(Box::new(pusher));
        if self.state().enabled {
            self.tools.start();
        }
        let weak = Arc::downgrade(self);
        self.runtime.spawn(async move {
            let Some(mut posts) = ProcessSignal::TAGS.posts() else { return };
            while posts.next().await {
                let Some(tagging) = weak.upgrade() else { return };
                tagging.reload().await;
            }
        });
    }

    /// Adds the tags saved in mail.db, keeping any already made.
    pub async fn restore(&self) {
        let saved = self.cache.load_tags().await;
        if saved.is_empty() {
            return;
        }
        let mut state = self.state();
        for (key, tag) in saved {
            state.tags.entry(key).or_insert(tag);
        }
        drop(state);
        self.events.emit_soon("tags");
    }

    /// Reads the tags again from mail.db, where every process saves its changes as it makes them.
    async fn reload(&self) {
        self.cache.flush().await;
        let edits = self.state().edits;
        let saved = self.cache.load_tags().await;
        let mut state = self.state();
        // A change made meanwhile is still on its way to disk; its save signals another reload.
        if edits != state.edits || saved == state.tags {
            return;
        }
        state.tags = saved;
        drop(state);
        self.events.emit_soon("tags");
    }

    /// Writes `changed` to mail.db; the cache then tells every process.
    fn save(&self, state: &mut State, changed: HashMap<String, ConversationTag>) {
        state.edits += 1;
        self.cache.save_tags(changed);
    }

    /// The `aiTagging.enabled` setting as last read.
    pub fn is_enabled(&self) -> bool {
        self.state().enabled
    }

    /// What tagging is doing or why it stopped; None while idle.
    pub fn status(&self) -> Option<String> {
        self.state().status.clone()
    }

    /// `status` is a failure: the last run failed, and passes wait out the backoff.
    pub fn failed(&self) -> bool {
        self.state().failed
    }

    /// Sets the status line; a failure showing up or going away is a `problems` event.
    fn set_status(&self, state: &mut State, status: Option<String>, failed: bool) {
        state.status = status;
        if state.failed != failed {
            state.failed = failed;
            self.events.emit_soon("problems");
        }
    }

    /// The conversation's tag while tagging is on, with `to_respond` false once the ask lapsed.
    pub fn tag_for(&self, conversation: &Conversation) -> Option<ConversationTag> {
        let state = self.state();
        if !state.enabled {
            return None;
        }
        let mut tag = state.tags.get(&key(conversation))?.clone();
        tag.to_respond = tag.is_waiting(now());
        Some(tag)
    }

    // MARK: - Settings

    fn settings_changed(self: &Arc<Self>) {
        let enabled = self.settings.value(Setting::AiTaggingEnabled) == Value::Bool(true);
        let values: Vec<Value> = RETRY_SETTINGS.iter().map(|setting| self.settings.value(*setting)).collect();
        let sync_value = self.settings.value(Setting::AiTaggingSyncAccounts);
        let (retry, sync) = {
            let mut state = self.state();
            let changed = state.retry_values != values;
            state.retry_values = values;
            let sync = state.sync_value != sync_value;
            state.sync_value = sync_value;
            (changed, sync)
        };
        self.apply(enabled);
        if retry {
            self.retry();
        } else if sync && self.is_enabled() {
            self.request();
        }
    }

    /// The lowercased addresses of the accounts whose tags sync with the server.
    fn sync_accounts(&self) -> HashSet<String> {
        let value = self.settings.value(Setting::AiTaggingSyncAccounts);
        value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase).collect()
    }

    fn apply(self: &Arc<Self>, enabled: bool) {
        let mut state = self.state();
        if state.enabled == enabled {
            return;
        }
        state.enabled = enabled;
        state.attempted.clear();
        state.backoff_until = None;
        if let Some(debounce) = state.debounce.take() {
            debounce.abort();
        }
        if enabled {
            drop(state);
            self.tools.start();
            self.request();
        } else {
            Self::cancel(&mut state);
            self.set_status(&mut state, None, false);
        }
    }

    fn retry(self: &Arc<Self>) {
        let enabled = {
            let mut state = self.state();
            state.attempted.clear();
            state.backoff_until = None;
            state.enabled
        };
        if enabled {
            self.request();
        }
    }

    // MARK: - Passes

    /// Tags the list a second after the last call, when tagging is on.
    pub fn schedule(self: &Arc<Self>) {
        let mut state = self.state();
        if !state.enabled {
            state.fresh = false;
            return;
        }
        if let Some(debounce) = state.debounce.take() {
            debounce.abort();
        }
        let weak = Arc::downgrade(self);
        state.debounce = Some(self.runtime.spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if let Some(tagging) = weak.upgrade() {
                tagging.state().fresh = false;
                tagging.state().debounce = None;
                tagging.request();
            }
        }));
    }

    /// Runs a pass soon, or once more after the one in flight.
    fn request(self: &Arc<Self>) {
        let mut state = self.state();
        state.pending = true;
        if state.pass.is_some() {
            return;
        }
        let tagging = self.clone();
        state.pass = Some(self.runtime.spawn(async move {
            let _running = tagging.running.lock().await;
            loop {
                {
                    let mut state = tagging.state();
                    if !state.pending {
                        state.pass = None;
                        return;
                    }
                    state.pending = false;
                }
                tagging.pass().await;
            }
        }));
    }

    /// Stops the pass in flight, its omp with it, and any requested after it.
    fn cancel(state: &mut State) {
        state.pending = false;
        if let Some(pass) = state.pass.take() {
            pass.abort();
        }
    }

    /// Syncs the tags with the server, then tags the newest untagged candidates in batches, one
    /// omp process at a time, and puts the new tags on the server.
    async fn pass(&self) {
        let (enabled, backoff) = {
            let state = self.state();
            (state.enabled, state.backoff_until)
        };
        if !enabled {
            return;
        }
        self.sync_server().await;
        if backoff.is_some_and(|until| until > Instant::now()) {
            return;
        }
        let text = |setting| self.settings.value(setting).as_str().unwrap_or_default().to_string();
        let number = |setting| self.settings.value(setting).as_u64().unwrap_or(1).max(1) as usize;
        let options = Options {
            backend: Backend::named(&text(Setting::AiTaggingBackend)),
            model: text(Setting::AiTaggingModel),
            executable: text(Setting::AiTaggingOmpPath),
        };
        let items = self.untagged_items(number(Setting::AiTaggingPassLimit));
        if items.is_empty() {
            self.state().retagging = false;
        }
        let mut remaining = items.len();
        for batch in items.chunks(number(Setting::AiTaggingBatchSize)) {
            let progress = format!("Tagging {remaining} conversation{}…", if remaining == 1 { "" } else { "s" });
            self.set_status(&mut self.state(), Some(progress), false);
            match self.tagger.tag(batch, &options).await {
                Ok(result) => {
                    let mut state = self.state();
                    state.tags.extend(result.clone());
                    state.attempted.extend(batch.iter().map(|item| item.id.clone()));
                    self.save(&mut state, result);
                    drop(state);
                    self.events.emit_soon("tags");
                    remaining -= batch.len();
                }
                Err(message) => {
                    let backoff = self.settings.value(Setting::AiTaggingBackoffSeconds).as_u64().unwrap_or(0);
                    {
                        let mut state = self.state();
                        self.set_status(&mut state, Some(format!("AI tagging failed: {message}")), true);
                        state.backoff_until = Some(Instant::now() + Duration::from_secs(backoff));
                    }
                    self.sync_server().await;
                    return;
                }
            }
        }
        self.set_status(&mut self.state(), None, false);
        if !items.is_empty() {
            self.sync_server().await;
        }
    }

    /// What passes offer, read again after the list changed.
    fn candidates(&self) -> Arc<Candidates> {
        let Some(source) = self.source.get() else { return Arc::default() };
        if !self.state().fresh {
            let candidates = Arc::new(source());
            let mut state = self.state();
            state.candidates = candidates;
            state.fresh = true;
        }
        self.state().candidates.clone()
    }

    fn untagged_items(&self, limit: usize) -> Vec<Item> {
        let candidates = self.candidates();
        // Picks under the lock, builds the items (snippets, addresses) after it, so commands
        // that take the lock meanwhile (a burst of retags) don't wait on that work.
        let picked = {
            let state = self.state();
            let mut keys = HashSet::new();
            let mut picked = Vec::new();
            for (index, conversation) in candidates.conversations.iter().enumerate() {
                if picked.len() >= limit {
                    break;
                }
                let key = key(conversation);
                if state.tags.contains_key(&key) || state.attempted.contains(&key) || !keys.insert(key.clone()) {
                    continue;
                }
                picked.push((index, key));
            }
            picked
        };
        picked
            .into_iter()
            .map(|(index, key)| self.item(&candidates.conversations[index], key, &candidates.own))
            .collect()
    }

    /// Brings the sync accounts' tags and the server's together (`tagsync`): takes the tags the
    /// server has and this engine lacks or has older, then puts this engine's new ones there.
    async fn sync_server(&self) {
        let accounts = self.sync_accounts();
        if accounts.is_empty() {
            return;
        }
        let candidates = self.candidates();
        let at = now();
        let pushes = {
            let mut guard = self.state();
            let state = &mut *guard;
            state.pushed.retain(|_, pushed| pushed.elapsed() < PUSH_GRACE);
            state.refused.retain(|_, (_, refused)| refused.elapsed() < PUSH_RETRY);
            let mut adopted = HashMap::new();
            let mut pushes = Vec::new();
            let mut keys = HashSet::new();
            for conversation in &candidates.conversations {
                if !accounts.contains(&conversation.account.to_lowercase()) {
                    continue;
                }
                let key = key(conversation);
                if !keys.insert(key.clone()) {
                    continue;
                }
                let local = state.tags.get(&key).cloned();
                if local.as_ref().is_some_and(|tag| tag.synced) && state.pushed.contains_key(&key) {
                    continue;
                }
                match tagsync::reconcile(local.as_ref(), &conversation.latest().envelope.tags, at, !state.retagging) {
                    Step::Keep => {}
                    Step::Adopt(tag) => {
                        state.tags.insert(key.clone(), tag.clone());
                        adopted.insert(key, tag);
                    }
                    Step::Push => {
                        let Some(tag) = local else { continue };
                        if state.refused.get(&key).is_some_and(|(refused, _)| *refused == tag) {
                            continue;
                        }
                        let names = tagsync::names(&tag, at);
                        pushes.push(Push { key, tag, conversation: conversation.clone(), names });
                    }
                }
            }
            if !adopted.is_empty() {
                self.save(state, adopted);
                self.events.emit_soon("tags");
            }
            pushes
        };
        let Some(pusher) = self.pusher.get() else { return };
        if pushes.is_empty() {
            return;
        }
        let sent: Vec<(String, ConversationTag)> = pushes.iter().map(|push| (push.key.clone(), push.tag.clone())).collect();
        let landed = pusher(pushes).await;
        let mut state = self.state();
        let mut synced = HashMap::new();
        for (key, tag) in sent {
            if !landed.contains(&key) {
                state.refused.insert(key, (tag, Instant::now()));
                continue;
            }
            state.refused.remove(&key);
            state.pushed.insert(key.clone(), Instant::now());
            // A correction made meanwhile is still to go.
            if let Some(current) = state.tags.get_mut(&key)
                && *current == tag
                && !current.synced
            {
                current.synced = true;
                synced.insert(key, current.clone());
            }
        }
        // The listed envelopes now carry the pushed tags.
        state.fresh = false;
        if !synced.is_empty() {
            self.save(&mut state, synced);
        }
    }

    fn item(&self, conversation: &Conversation, key: String, own: &HashSet<String>) -> Item {
        let from_user: Vec<bool> = conversation.messages.iter().map(|message| message.is_from_user(own)).collect();
        let first_incoming = from_user.iter().position(|from_user| !from_user);
        let incoming = conversation.latest_incoming(own).unwrap_or_else(|| conversation.latest());
        let sender = incoming.envelope.from.first();
        Item {
            id: key,
            sender_name: sender.map(|sender| sender.display_name()).unwrap_or_default(),
            sender_email: sender.map(|sender| sender.email.clone()).unwrap_or_default(),
            user_in_to: incoming.envelope.to.iter().any(|address| own.contains(&address.normalized_email())),
            subject: conversation.subject.clone(),
            snippet: self.contents.snippet(&incoming.message_ref()).unwrap_or_default(),
            date: incoming.envelope.date,
            message_count: conversation.messages.len(),
            user_replied: first_incoming.is_some_and(|index| from_user[index + 1..].contains(&true)),
            latest_from_user: from_user.last().copied().unwrap_or(false),
        }
    }

    // MARK: - Corrections

    /// Corrects the conversation's tag; None leaves a part as it is. A conversation the tagger
    /// has not judged needs a category, and newsletters never wait on the user. For an account
    /// that syncs, the correction goes to the server in a pass that follows.
    pub fn edit(
        self: &Arc<Self>,
        conversation: &Conversation,
        category: Option<Category>,
        waiting: Option<bool>,
        labels: Option<&[String]>,
    ) -> Result<ConversationTag, String> {
        let state = self.state();
        if !state.enabled {
            return Err(OFF_MESSAGE.into());
        }
        let key = key(conversation);
        let existing = state.tags.get(&key).cloned();
        let Some(mut tag) = existing.clone().or_else(|| {
            category.map(|category| ConversationTag {
                category,
                to_respond: false,
                labels: Vec::new(),
                waiting_until: None,
                synced: false,
            })
        }) else {
            return Err(format!("{} has no AI tag yet; give a category.", conversation.id));
        };
        if let Some(category) = category {
            tag.category = category;
        }
        if waiting == Some(true) && tag.category == Category::Newsletter {
            return Err("Newsletters cannot wait on you.".into());
        }
        if let Some(waiting) = waiting {
            tag.to_respond = waiting;
            tag.waiting_until = None;
        }
        if tag.category == Category::Newsletter {
            tag.to_respond = false;
        }
        if let Some(labels) = labels {
            tag.labels = labels
                .iter()
                .map(|label| crate::text::prefix_characters(label.trim(), omp::MAX_LABEL_LENGTH).0.to_string())
                .filter(|label| !label.is_empty())
                .take(2)
                .collect();
        }
        if existing.as_ref() != Some(&tag) {
            tag.synced = false;
        }
        self.store(state, conversation, key, tag.clone());
        tag.to_respond = tag.is_waiting(now());
        Ok(tag)
    }

    /// The tag kept for the conversation, its waiting flag as stored (not lapsed).
    fn stored(&self, conversation: &Conversation) -> Option<ConversationTag> {
        self.state().tags.get(&key(conversation)).cloned()
    }

    /// Puts back `before`, a tag a correction replaced (`mail.undo`, MS-280); for an account that
    /// syncs, it goes to the server again in a pass that follows.
    pub fn put_back(self: &Arc<Self>, conversation: &Conversation, mut before: ConversationTag) -> Result<(), String> {
        let state = self.state();
        if !state.enabled {
            return Err(OFF_MESSAGE.into());
        }
        let key = key(conversation);
        if state.tags.get(&key) == Some(&before) {
            return Ok(());
        }
        before.synced = false;
        self.store(state, conversation, key, before);
        Ok(())
    }

    /// Keeps a tag made here (a correction or its undo), tells the front ends, and sends it to
    /// the server when the account syncs and it is not there yet.
    fn store(self: &Arc<Self>, mut state: MutexGuard<'_, State>, conversation: &Conversation, key: String, tag: ConversationTag) {
        state.tags.insert(key.clone(), tag.clone());
        state.attempted.insert(key.clone());
        self.save(&mut state, HashMap::from([(key, tag.clone())]));
        drop(state);
        self.events.emit_soon("tags");
        if !tag.synced && self.sync_accounts().contains(&conversation.account.to_lowercase()) {
            self.request();
        }
    }

    /// Forgets every tag and tags the list again, ignoring any failure backoff.
    pub fn retag_all(self: &Arc<Self>) {
        {
            let mut state = self.state();
            state.tags.clear();
            state.attempted.clear();
            state.backoff_until = None;
            state.edits += 1;
            state.retagging = true;
            self.cache.clear_tags();
            if let Some(debounce) = state.debounce.take() {
                debounce.abort();
            }
            // `CoalescedTask.restart`: the pass in flight stops, a fresh one follows.
            if let Some(pass) = state.pass.take() {
                pass.abort();
            }
            self.set_status(&mut state, None, false);
        }
        self.events.emit_soon("tags");
        self.request();
    }

    // MARK: - Commands

    /// `mail.tag`; `lookup` finds a conversation by the id a listing gave. Answers with the
    /// conversations whose tag changed and the tag each had before, for undo.
    pub(crate) fn tag_command(
        self: &Arc<Self>,
        args: &Arguments,
        lookup: impl Fn(&str) -> Result<Conversation, EngineError>,
    ) -> Result<(TagResult, Vec<(Conversation, ConversationTag)>), EngineError> {
        let category = match args.string("category") {
            Some(name) => {
                Some(Category::named(&name).ok_or_else(|| EngineError::usage(format!("Unknown category {name}.")))?)
            }
            None => None,
        };
        let waiting = args.flag("waiting");
        let labels = args.list("labels");
        if category.is_none() && waiting.is_none() && labels.is_none() {
            return Err(EngineError::usage("Give --category, --waiting or --labels."));
        }
        let (mut done, mut failed, mut replaced) = (Vec::new(), Vec::new(), Vec::new());
        for id in args.list("ids").unwrap_or_default() {
            let edited = lookup(&id).map_err(|error| error.message).and_then(|conversation| {
                let before = self.stored(&conversation);
                let tag = self.edit(&conversation, category, waiting, labels.as_deref())?;
                Ok((conversation, before, tag))
            });
            match edited {
                Ok((conversation, before, tag)) => {
                    if let Some(before) = before
                        && self.stored(&conversation).as_ref() != Some(&before)
                    {
                        replaced.push((conversation, before));
                    }
                    done.push(Tagged { id, category: tag.category, waiting: tag.to_respond, labels: tag.labels })
                }
                Err(message) => failed.push(BatchFailure { id, code: None, message }),
            }
        }
        Ok((TagResult { done, failed, undo: None }, replaced))
    }

    /// `mail.retag`.
    pub(crate) fn run(self: &Arc<Self>, command: &str) -> Result<Value, EngineError> {
        match command {
            "mail.retag" => {
                if !self.is_enabled() {
                    return Err(EngineError::new("disabled", OFF_MESSAGE));
                }
                self.retag_all();
                Ok(json!({ "started": true }))
            }
            _ => unreachable!("{command} is a tagging command"),
        }
    }
}

/// The conversation's latest message, so a new reply re-tags it.
pub fn key(conversation: &Conversation) -> String {
    let latest = conversation.latest();
    latest
        .envelope
        .message_id
        .clone()
        .unwrap_or_else(|| format!("{}/{}/{}", latest.account(), latest.mailbox(), latest.id()))
}
