//! AI tags of the inbox conversations, as the Swift engine's `TagStore` keeps them
//! (`Sources/MailstreamKit/Store/TagStore.swift`, `Model/Tags.swift`, the `mail.tag` and
//! `mail.retag` commands of `Control/ControlService.swift`): the user's `omp` CLI tags the
//! default list's untagged conversations, newest first, one pass at a time, a second after the
//! list last changed, in batches of `aiTagging.batchSize` up to `aiTagging.passLimit` per pass,
//! waiting `aiTagging.backoffSeconds` after a failure. It runs as the `aiTagging.*` settings say,
//! following settings.json as it changes.
//!
//! Every Mailstream process keeps its own copy of the tags in memory, writes only what it
//! changed to mail.db's `tags` table and reads them all again when any process says it wrote
//! (`ProcessSignal::TAGS`), so a tag set in one engine shows in the others, Swift or Rust.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cache::MailCache;
use crate::commands::Arguments;
use crate::content::Contents;
use crate::model::{Conversation, Date, now};
use crate::omp::{self, Item, Options, Tagger, Tools};
use crate::settings::{Setting, Settings};
use crate::signal::ProcessSignal;
use crate::{EngineError, Events};

/// Smart Inbox bucket assigned by the AI tagger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
}

impl ConversationTag {
    /// `to_respond`, unless the ask has lapsed by `now`.
    pub fn is_waiting(&self, now: Date) -> bool {
        self.to_respond && self.waiting_until.is_none_or(|until| now < until)
    }
}

/// Why tag edits and retagging are refused while tagging is off.
pub const OFF_MESSAGE: &str = "AI tagging is off (aiTagging.enabled).";

/// Settings whose change deserves another try at once: a fixed omp or model.
const RETRY_SETTINGS: [Setting; 4] =
    [Setting::AiTaggingModel, Setting::AiTaggingOmpPath, Setting::AiTaggingPassLimit, Setting::AiTaggingBackoffSeconds];

/// A list change waits this long for the next before tagging.
const DEBOUNCE: Duration = Duration::from_secs(1);

/// What a pass may tag: the default list's conversations with a message in a listed inbox,
/// newest first, and the user's lowercased addresses.
#[derive(Default)]
pub struct Candidates {
    pub conversations: Vec<Conversation>,
    pub own: HashSet<String>,
}

type Source = Box<dyn Fn() -> Candidates + Send + Sync>;

#[derive(Default)]
struct State {
    /// Keyed by `key`, so a new message in a conversation re-tags it; read through `tag_for`.
    tags: HashMap<String, ConversationTag>,
    /// Short user-facing progress or failure text; None when idle.
    status: Option<String>,
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
}

impl Tagging {
    /// Call on the engine's runtime. Follows the `aiTagging.*` settings from now on. omp runs in
    /// `tool_environment`, else the login shell's (`Config::tool_environment`).
    pub fn new(
        cache: Arc<MailCache>,
        contents: Arc<Contents>,
        settings: Arc<Settings>,
        events: Arc<Events>,
        tool_environment: Option<HashMap<String, String>>,
    ) -> Arc<Self> {
        let tools = Arc::new(tool_environment.map_or_else(Tools::new, Tools::fixed));
        let tagger = Tagger::new(tools.clone(), cache.root.clone());
        let state = State {
            enabled: settings.value(Setting::AiTaggingEnabled) == Value::Bool(true),
            retry_values: RETRY_SETTINGS.iter().map(|setting| settings.value(*setting)).collect(),
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

    /// Starts offering `source`'s conversations and reading the tags other processes write.
    pub fn start(self: &Arc<Self>, source: impl Fn() -> Candidates + Send + Sync + 'static) {
        if self.source.set(Box::new(source)).is_err() {
            return;
        }
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
        let retry = {
            let mut state = self.state();
            let changed = state.retry_values != values;
            state.retry_values = values;
            changed
        };
        self.apply(enabled);
        if retry {
            self.retry();
        }
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
            state.status = None;
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
            return;
        }
        if let Some(debounce) = state.debounce.take() {
            debounce.abort();
        }
        let weak = Arc::downgrade(self);
        state.debounce = Some(self.runtime.spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if let Some(tagging) = weak.upgrade() {
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

    /// Tags the newest untagged candidates in batches, one omp process at a time.
    async fn pass(&self) {
        let (enabled, backoff) = {
            let state = self.state();
            (state.enabled, state.backoff_until)
        };
        if !enabled || backoff.is_some_and(|until| until > Instant::now()) {
            return;
        }
        let text = |setting| self.settings.value(setting).as_str().unwrap_or_default().to_string();
        let number = |setting| self.settings.value(setting).as_u64().unwrap_or(1).max(1) as usize;
        let options = Options { model: text(Setting::AiTaggingModel), executable: text(Setting::AiTaggingOmpPath) };
        let items = self.untagged_items(number(Setting::AiTaggingPassLimit));
        let mut remaining = items.len();
        for batch in items.chunks(number(Setting::AiTaggingBatchSize)) {
            self.state().status = Some(format!("Tagging {remaining} conversation{}…", if remaining == 1 { "" } else { "s" }));
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
                    let mut state = self.state();
                    state.status = Some(format!("AI tagging failed: {message}"));
                    state.backoff_until = Some(Instant::now() + Duration::from_secs(backoff));
                    return;
                }
            }
        }
        self.state().status = None;
    }

    fn untagged_items(&self, limit: usize) -> Vec<Item> {
        let Some(source) = self.source.get() else { return Vec::new() };
        let candidates = source();
        let state = self.state();
        let mut keys = HashSet::new();
        let mut items = Vec::new();
        for conversation in &candidates.conversations {
            if items.len() >= limit {
                break;
            }
            let key = key(conversation);
            if state.tags.contains_key(&key) || state.attempted.contains(&key) || !keys.insert(key.clone()) {
                continue;
            }
            items.push(self.item(conversation, key, &candidates.own));
        }
        items
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
    /// has not judged needs a category, and newsletters never wait on the user.
    pub fn edit(
        &self,
        conversation: &Conversation,
        category: Option<Category>,
        waiting: Option<bool>,
        labels: Option<&[String]>,
    ) -> Result<ConversationTag, String> {
        let mut state = self.state();
        if !state.enabled {
            return Err(OFF_MESSAGE.into());
        }
        let key = key(conversation);
        let existing = state.tags.get(&key).cloned();
        let Some(mut tag) = existing.or_else(|| {
            category.map(|category| ConversationTag { category, to_respond: false, labels: Vec::new(), waiting_until: None })
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
        state.tags.insert(key.clone(), tag.clone());
        state.attempted.insert(key.clone());
        self.save(&mut state, HashMap::from([(key, tag.clone())]));
        drop(state);
        self.events.emit_soon("tags");
        tag.to_respond = tag.is_waiting(now());
        Ok(tag)
    }

    /// Forgets every tag and tags the list again, ignoring any failure backoff.
    pub fn retag_all(self: &Arc<Self>) {
        {
            let mut state = self.state();
            state.tags.clear();
            state.attempted.clear();
            state.backoff_until = None;
            state.edits += 1;
            self.cache.clear_tags();
            if let Some(debounce) = state.debounce.take() {
                debounce.abort();
            }
            // `CoalescedTask.restart`: the pass in flight stops, a fresh one follows.
            if let Some(pass) = state.pass.take() {
                pass.abort();
            }
            state.status = None;
        }
        self.events.emit_soon("tags");
        self.request();
    }

    // MARK: - Commands

    /// `mail.tag` and `mail.retag`; `lookup` finds a conversation by the id a listing gave.
    pub(crate) fn run(
        self: &Arc<Self>,
        command: &str,
        args: &Arguments,
        lookup: impl Fn(&str) -> Result<Conversation, EngineError>,
    ) -> Result<Value, EngineError> {
        match command {
            "mail.tag" => {
                let category = match args.string("category") {
                    Some(name) => Some(
                        Category::named(&name).ok_or_else(|| EngineError::usage(format!("Unknown category {name}.")))?,
                    ),
                    None => None,
                };
                let waiting = args.string("waiting").map(|waiting| waiting == "true");
                let labels = args.list("labels");
                if category.is_none() && waiting.is_none() && labels.is_none() {
                    return Err(EngineError::usage("Give --category, --waiting or --labels."));
                }
                let (mut done, mut failed) = (Vec::new(), Vec::new());
                for id in args.list("ids").unwrap_or_default() {
                    let edited = lookup(&id)
                        .map_err(|error| error.message)
                        .and_then(|conversation| self.edit(&conversation, category, waiting, labels.as_deref()));
                    match edited {
                        Ok(tag) => done.push(json!({
                            "id": id, "category": tag.category, "waiting": tag.to_respond, "labels": tag.labels,
                        })),
                        Err(message) => failed.push(json!({ "id": id, "message": message })),
                    }
                }
                Ok(json!({ "done": done, "failed": failed }))
            }
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
