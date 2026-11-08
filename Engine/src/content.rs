//! Message bodies and list previews, as the Swift engine's `ContentStore`
//! (`Sources/MailstreamKit/Store/ContentStore.swift`): one shared load per message (mail.db
//! first, else the provider, whose answer is cached), the bodies recently read in memory, and
//! the preview of every message loaded so far.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};

use crate::api::Priority;
use crate::backend::Backend;
use crate::cache::MailCache;
use crate::model::{MessageContent, MessageRef};
use crate::text;

/// Bodies kept in memory. Prefetched bodies only fill free room, as the coldest, so they never
/// push out a body a reader used.
const CAPACITY: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// An interactive load took over from this prefetch.
    Replaced,
    Failed(String),
}

type Load = Shared<BoxFuture<'static, Result<Arc<MessageContent>, LoadError>>>;

struct Task {
    load: Load,
    priority: Priority,
    id: u64,
    abort: tokio::task::AbortHandle,
}

#[derive(Default)]
struct State {
    snippets: HashMap<MessageRef, String>,
    bodies: HashMap<MessageRef, Arc<MessageContent>>,
    /// Coldest first.
    order: VecDeque<MessageRef>,
    tasks: HashMap<MessageRef, Task>,
    next_task: u64,
    /// Bumped by `forget`, so loads in flight then don't bring a removed account's mail back.
    generation: u64,
}

pub struct Contents {
    cache: Arc<MailCache>,
    state: Mutex<State>,
    /// Called when a preview arrives or changes.
    on_previews: Box<dyn Fn() + Send + Sync>,
}

impl Contents {
    pub fn new(cache: Arc<MailCache>, on_previews: impl Fn() + Send + Sync + 'static) -> Self {
        Self { cache, state: Mutex::new(State::default()), on_previews: Box::new(on_previews) }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("contents lock")
    }

    pub fn snippet(&self, message: &MessageRef) -> Option<String> {
        self.state().snippets.get(message).cloned()
    }

    /// The message's content, from memory, mail.db or the provider.
    pub async fn load(self: &Arc<Self>, message: &MessageRef, backend: Option<Arc<Backend>>) -> Result<Arc<MessageContent>, String> {
        if let Some(body) = self.state().bodies.get(message) {
            return Ok(body.clone());
        }
        let generation = self.state().generation;
        let content = self.finish(message, self.task(message, Priority::Interactive, backend)).await?;
        let mut state = self.state();
        if state.generation == generation {
            remember(&mut state, message, content.clone(), false);
        }
        Ok(content)
    }

    /// The preview of `message`, loading the message in the background when none is known.
    pub async fn preview(self: &Arc<Self>, message: &MessageRef, backend: Option<Arc<Backend>>) -> Option<String> {
        if let Some(snippet) = self.snippet(message) {
            return Some(snippet);
        }
        let load = self.task(message, Priority::Background, backend);
        match self.finish(message, load).await {
            Ok(content) => Some(content.snippet.clone()),
            Err(_) => self.snippet(message),
        }
    }

    /// Loads the latest message of each conversation listed in the background, for previews.
    pub fn prefetch(self: &Arc<Self>, messages: Vec<(MessageRef, Option<Arc<Backend>>)>) {
        for (message, backend) in messages {
            if !self.state().snippets.contains_key(&message) {
                let _ = self.task(&message, Priority::Background, backend);
            }
        }
    }

    /// Awaits `load`; a prefetch an interactive load replaced hands over to that load.
    async fn finish(&self, message: &MessageRef, mut load: Load) -> Result<Arc<MessageContent>, String> {
        loop {
            match load.await {
                Ok(content) => return Ok(content),
                Err(LoadError::Failed(message)) => return Err(message),
                Err(LoadError::Replaced) => {
                    let state = self.state();
                    if let Some(next) = state.tasks.get(message) {
                        load = next.load.clone();
                    } else if let Some(body) = state.bodies.get(message) {
                        return Ok(body.clone());
                    } else {
                        return Err("The load was cancelled.".into());
                    }
                }
            }
        }
    }

    /// One shared load per message, assembled and cached off the caller; its preview is
    /// published when it lands. An interactive request replaces a running prefetch.
    fn task(self: &Arc<Self>, message: &MessageRef, priority: Priority, backend: Option<Arc<Backend>>) -> Load {
        let mut state = self.state();
        if let Some(running) = state.tasks.get(message)
            && (running.priority == Priority::Interactive || priority == Priority::Background)
        {
            return running.load.clone();
        }
        if let Some(replaced) = state.tasks.remove(message) {
            replaced.abort.abort();
        }
        state.next_task += 1;
        let id = state.next_task;
        let (contents, target) = (self.clone(), message.clone());
        let handle = tokio::spawn(async move {
            let result = Self::fetch(&contents.cache, &target, backend, priority).await.map(Arc::new);
            let mut state = contents.state();
            if state.tasks.get(&target).is_none_or(|task| task.id != id) {
                // Replaced or forgotten meanwhile.
                return result;
            }
            state.tasks.remove(&target);
            if let Ok(content) = &result {
                let changed = state.snippets.get(&target) != Some(&content.snippet);
                if changed {
                    state.snippets.insert(target.clone(), content.snippet.clone());
                }
                if priority == Priority::Background {
                    remember(&mut state, &target, content.clone(), true);
                }
                drop(state);
                if changed {
                    (contents.on_previews)();
                }
            }
            result
        });
        let abort = handle.abort_handle();
        let load: Load = async move {
            match handle.await {
                Ok(result) => result.map_err(LoadError::Failed),
                Err(_) => Err(LoadError::Replaced),
            }
        }
        .boxed()
        .shared();
        state.tasks.insert(message.clone(), Task { load: load.clone(), priority, id, abort });
        load
    }

    async fn fetch(
        cache: &MailCache,
        message: &MessageRef,
        backend: Option<Arc<Backend>>,
        priority: Priority,
    ) -> Result<MessageContent, String> {
        if let Some(mut cached) = cache.load_content(message).await {
            // Previews follow the current `snippet`, not the one the cache was written with.
            cached.snippet = text::snippet(cached.text.as_deref().unwrap_or(""));
            return Ok(cached);
        }
        let Some(backend) = backend else { return Err("The account is no longer signed in.".into()) };
        let content = backend
            .content(&message.id, cache.message_directory(message), MailCache::part_url_base(message), priority)
            .await
            .map_err(|error| error.message)?;
        cache.save_content(&content, message);
        Ok(content)
    }

    /// Drops everything of `removed` accounts: bodies, previews and loads in flight.
    pub fn forget(&self, removed: &HashSet<String>) {
        let mut state = self.state();
        state.generation += 1;
        state.tasks.retain(|message, task| {
            let keep = !removed.contains(&message.account);
            if !keep {
                task.abort.abort();
            }
            keep
        });
        state.bodies.retain(|message, _| !removed.contains(&message.account));
        state.order.retain(|message| !removed.contains(&message.account));
        state.snippets.retain(|message, _| !removed.contains(&message.account));
    }
}

/// A body a reader used goes in as the newest; a prefetched one only into free room, as the
/// coldest.
fn remember(state: &mut State, message: &MessageRef, content: Arc<MessageContent>, prefetched: bool) {
    if prefetched {
        if state.bodies.contains_key(message) || state.order.len() >= CAPACITY {
            return;
        }
        state.order.push_front(message.clone());
    } else {
        if state.bodies.contains_key(message) {
            state.order.retain(|known| known != message);
        }
        state.order.push_back(message.clone());
    }
    state.bodies.insert(message.clone(), content);
    while state.order.len() > CAPACITY {
        if let Some(coldest) = state.order.pop_front() {
            state.bodies.remove(&coldest);
        }
    }
}
