//! # Google Chat
//!
//! The `chat` requests Mailstream sends beside himalaya's: list chat accounts, list an account's
//! spaces with their latest message and unread count, page a space's messages, mark a space
//! read, and send. Each answers with one JSON document (see `ChatCommand`).
//!
//! The space list is polled, so it is kept cheap: each account's space summaries live in memory
//! keyed by the space's last activity, and a poll only refetches the spaces whose activity
//! changed (plus the read state of the ones still unread, which another client may have read).

mod account;
mod api;
mod http;

use std::{
    collections::HashMap,
    iter,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use anyhow::{Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;

use self::api::{Message, Space, User};

/// Messages read per space for its summary: the latest one, and enough to count what is unread.
const SUMMARY_MESSAGES: usize = 25;
/// Space summaries fetched at once.
const FAN_OUT: usize = 6;

#[derive(Parser)]
#[command(name = "chat", disable_help_subcommand = true)]
enum ChatCommand {
    /// `{"accounts": [{"name", "email"}]}`
    Accounts,
    /// `{"me", "more", "spaces": [Summary]}`: the most recently active spaces, newest first.
    Spaces {
        account: String,
        #[arg(long, default_value_t = 60)]
        limit: usize,
    },
    /// `{"messages": [ChatMessage], "more", "lastRead"}`: ascending.
    Messages {
        account: String,
        space: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        before: Option<String>,
    },
    /// `{"lastRead"}`
    Read { account: String, space: String },
    /// `{"message": ChatMessage}`
    Send {
        account: String,
        space: String,
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        #[arg(long)]
        thread: Option<String>,
    },
}

/// Runs one `chat` request (its arguments after `chat`) and returns its JSON output.
pub fn run(args: &[String]) -> Result<Vec<u8>> {
    let command = ChatCommand::try_parse_from(iter::once("chat").chain(args.iter().map(String::as_str)))
        .map_err(|err| anyhow!(err.to_string()))?;
    let json = match command {
        ChatCommand::Accounts => serde_json::json!({ "accounts": account::list()? }),
        ChatCommand::Spaces { account, limit } => {
            account::check(&account)?;
            serde_json::to_value(spaces(&account, limit.max(1))?)?
        }
        ChatCommand::Messages { account, space, limit, before } => {
            account::check(&account)?;
            http::check_name(&space, &["spaces"])?;
            serde_json::to_value(messages(&account, &space, limit, before.as_deref())?)?
        }
        ChatCommand::Read { account, space } => {
            account::check(&account)?;
            http::check_name(&space, &["spaces"])?;
            serde_json::json!({ "lastRead": read(&account, &space)? })
        }
        ChatCommand::Send { account, space, text, thread } => {
            account::check(&account)?;
            http::check_name(&space, &["spaces"])?;
            if let Some(thread) = thread.as_deref() {
                http::check_name(thread, &["spaces", "threads"])?;
                if !thread.starts_with(&format!("{space}/")) {
                    return Err(anyhow!("Thread {thread} is not in {space}"));
                }
            }
            if text.trim().is_empty() {
                return Err(anyhow!("Message is empty"));
            }
            serde_json::json!({ "message": send(&account, &space, &text, thread.as_deref())? })
        }
    };
    Ok(serde_json::to_vec(&json)?)
}

// MARK: - Output

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatUser {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
    bot: bool,
}

impl From<User> for ChatUser {
    fn from(user: User) -> Self {
        Self {
            bot: user.kind.as_deref() == Some("BOT"),
            name: user.name,
            display_name: user.display_name.filter(|n| !n.trim().is_empty()),
            email: user.email.filter(|e| !e.is_empty()),
            avatar_url: user.avatar_url.filter(|u| !u.is_empty()),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatAttachment {
    name: String,
    content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    name: String,
    sender: ChatUser,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    edited: Option<String>,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<String>,
    thread_reply: bool,
    mine: bool,
    card: bool,
    attachments: Vec<ChatAttachment>,
}

impl ChatMessage {
    fn new(message: Message, me: Option<&str>) -> Self {
        let sender = ChatUser::from(message.sender.unwrap_or_default());
        let card = message.cards_v2.as_ref().is_some_and(|c| !c.is_null()) || message.cards.as_ref().is_some_and(|c| !c.is_null());
        // `text` has the markup stripped; `formattedText` keeps it (`*bold*`, `<url|label>`,
        // `<users/…>` mentions), with a trailing newline.
        let text = message
            .formatted_text
            .map(|t| t.trim_end_matches('\n').to_owned())
            .filter(|t| !t.is_empty())
            .or(message.text.filter(|t| !t.is_empty()))
            .or(message.fallback_text)
            .unwrap_or_default();
        let created = message.create_time.unwrap_or_default();
        // The API sets `lastUpdateTime` on every message; only a later one is an edit.
        let edited = message.last_update_time.filter(|t| instant(Some(t)) > instant(Some(&created)));
        Self {
            mine: me.is_some_and(|me| sender.name == me),
            sender,
            name: message.name,
            created,
            edited,
            text,
            thread: message.thread.map(|t| t.name),
            thread_reply: message.thread_reply,
            card,
            attachments: message
                .attachment
                .into_iter()
                .map(|a| ChatAttachment {
                    name: a.content_name.unwrap_or_else(|| "Attachment".into()),
                    content_type: a.content_type.unwrap_or_default(),
                    url: a.download_uri.filter(|u| !u.is_empty()).or_else(|| {
                        a.drive_data_ref
                            .and_then(|d| d.drive_file_id)
                            .map(|id| format!("https://drive.google.com/open?id={id}"))
                    }),
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Summary {
    name: String,
    kind: &'static str,
    title: String,
    members: Vec<ChatUser>,
    last_active: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read: Option<String>,
    unread: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<ChatMessage>,
    /// Its unread count went out of date; the next poll recounts it.
    #[serde(skip)]
    stale: bool,
}

#[derive(Serialize)]
struct SpacesOutput {
    me: Option<String>,
    more: bool,
    spaces: Vec<Summary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagesOutput {
    messages: Vec<ChatMessage>,
    more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read: Option<String>,
}

// MARK: - Per-account memory

#[derive(Default)]
struct Memory {
    /// The user's own `users/{id}`, learned from a read state.
    me: Option<String>,
    summaries: HashMap<String, Summary>,
    /// Members of DMs and unnamed group chats, which name them.
    members: HashMap<String, Vec<User>>,
}

static MEMORY: Mutex<Option<HashMap<String, Memory>>> = Mutex::new(None);

fn with_memory<T>(account: &str, f: impl FnOnce(&mut Memory) -> T) -> T {
    let mut memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
    f(memory.get_or_insert_with(HashMap::new).entry(account.to_owned()).or_default())
}

// MARK: - Commands

fn spaces(account: &str, limit: usize) -> Result<SpacesOutput> {
    let mut all = api::spaces(account)?;
    all.sort_by_cached_key(|space| std::cmp::Reverse(instant(space.last_active_time.as_deref())));
    let more = all.len() > limit;
    all.truncate(limit);

    // What memory can't answer: spaces new or active since, and unread ones (read elsewhere?).
    let (stale, recheck): (Vec<&Space>, Vec<&Space>) = with_memory(account, |memory| {
        let mut stale = Vec::new();
        let mut recheck = Vec::new();
        for space in &all {
            match memory.summaries.get(&space.name) {
                Some(known) if !known.stale && Some(&known.last_active) == space.last_active_time.as_ref() => {
                    if known.unread > 0 {
                        recheck.push(space);
                    }
                }
                _ => stale.push(space),
            }
        }
        (stale, recheck)
    });

    let fresh = fan_out(&stale, |space| summarize(account, space));
    let rechecked = fan_out(&recheck, |space| api::read_state(account, &space.name));

    let mut failures = Vec::new();
    let summaries = with_memory(account, |memory| {
        for result in fresh {
            match result {
                Ok(summary) => {
                    memory.summaries.insert(summary.name.clone(), summary);
                }
                Err(err) => failures.push(err),
            }
        }
        for (space, result) in recheck.iter().zip(rechecked) {
            let Ok(state) = result else { continue };
            let Some(summary) = memory.summaries.get_mut(&space.name) else { continue };
            if state.last_read_time.is_none() || state.last_read_time == summary.last_read {
                continue;
            }
            if instant(state.last_read_time.as_deref()) >= instant(Some(&summary.last_active)) {
                summary.unread = 0;
                summary.last_read = state.last_read_time;
            } else {
                // Read part of the way elsewhere: shown as it was until the next poll recounts it.
                summary.stale = true;
            }
        }
        all.iter().filter_map(|space| memory.summaries.get(&space.name).cloned()).collect::<Vec<_>>()
    });
    // A partial list is better than none, unless nothing at all could be read.
    if summaries.is_empty()
        && let Some(err) = failures.into_iter().next()
    {
        return Err(err);
    }
    let me = with_memory(account, |memory| memory.me.clone());
    Ok(SpacesOutput { me, more, spaces: summaries })
}

/// Runs `f` over `items` on up to `FAN_OUT` threads; results keep `items`' order.
fn fan_out<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> Result<R> + Sync) -> Vec<Result<R>> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<R>>>> = Mutex::new(items.iter().map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..items.len().min(FAN_OUT) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    let result = f(item);
                    results.lock().unwrap_or_else(|p| p.into_inner())[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err(anyhow!("Space summary did not run"))))
        .collect()
}

fn summarize(account: &str, space: &Space) -> Result<Summary> {
    let state = api::read_state(account, &space.name)?;
    let me = match state.user() {
        Some(me) => {
            with_memory(account, |memory| memory.me = Some(me.clone()));
            Some(me)
        }
        None => with_memory(account, |memory| memory.me.clone()),
    };
    let page = api::messages(account, &space.name, SUMMARY_MESSAGES, None)?;
    let last_read = state.last_read_time;
    let unread = page
        .messages
        .iter()
        .filter(|m| m.sender.as_ref().map(|s| &s.name) != me.as_ref())
        .filter(|m| last_read.is_none() || instant(m.create_time.as_deref()) > instant(last_read.as_deref()))
        .count();

    let kind = match space.space_type.as_str() {
        "DIRECT_MESSAGE" if space.single_user_bot_dm => "bot",
        "DIRECT_MESSAGE" => "dm",
        "GROUP_CHAT" => "group",
        _ => "space",
    };
    let named = !space.display_name.trim().is_empty();
    let members: Vec<User> = if named {
        Vec::new()
    } else {
        let known = with_memory(account, |memory| memory.members.get(&space.name).cloned());
        match known {
            Some(members) => members,
            None => {
                let members: Vec<User> = api::members(account, &space.name)?
                    .into_iter()
                    .filter(|user| me.as_ref() != Some(&user.name))
                    .collect();
                with_memory(account, |memory| memory.members.insert(space.name.clone(), members.clone()));
                members
            }
        }
    };
    // Google withholds names of some members (apps, people without "affinity"), but not of a
    // message's sender: fill them in from the messages at hand.
    let members: Vec<User> = members
        .into_iter()
        .map(|member| match member.display_name {
            Some(_) => member,
            None => page
                .messages
                .iter()
                .filter_map(|m| m.sender.as_ref())
                .find(|sender| sender.name == member.name && sender.display_name.is_some())
                .cloned()
                .unwrap_or(member),
        })
        .collect();
    let title = if named { space.display_name.trim().to_owned() } else { title(&members, kind) };
    Ok(Summary {
        name: space.name.clone(),
        kind,
        title,
        members: members.into_iter().map(ChatUser::from).collect(),
        last_active: space.last_active_time.clone().unwrap_or_default(),
        last_read,
        unread,
        latest: page.messages.into_iter().next().map(|m| ChatMessage::new(m, me.as_deref())),
        stale: false,
    })
}

/// An unnamed space is named after the people in it.
fn title(members: &[User], kind: &str) -> String {
    let names: Vec<&str> = members
        .iter()
        .filter_map(|m| m.display_name.as_deref().or(m.email.as_deref()))
        .filter(|n| !n.trim().is_empty())
        .collect();
    match (names.len(), kind) {
        (0, "bot") => "App".into(),
        (0, "dm") => "Direct message".into(),
        (0, _) => "Group chat".into(),
        (1..=4, _) => names.join(", "),
        (n, _) => format!("{}, +{}", names[..3].join(", "), n - 3),
    }
}

fn messages(account: &str, space: &str, limit: usize, before: Option<&str>) -> Result<MessagesOutput> {
    let (page, state) = thread::scope(|scope| {
        let state = scope.spawn(|| api::read_state(account, space));
        let page = api::messages(account, space, limit, before);
        (page, state.join().unwrap_or_else(|_| Err(anyhow!("Read state request panicked"))))
    });
    let page = page?;
    let state = state.ok();
    let me = match state.as_ref().and_then(|s| s.user()) {
        Some(me) => {
            with_memory(account, |memory| memory.me = Some(me.clone()));
            Some(me)
        }
        None => with_memory(account, |memory| memory.me.clone()),
    };
    let mut messages: Vec<ChatMessage> = page.messages.into_iter().map(|m| ChatMessage::new(m, me.as_deref())).collect();
    messages.reverse();
    Ok(MessagesOutput {
        messages,
        more: page.next_page_token.is_some_and(|t| !t.is_empty()),
        last_read: state.and_then(|s| s.last_read_time),
    })
}

fn read(account: &str, space: &str) -> Result<String> {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
    let state = api::mark_read(account, space, &now)?;
    let last_read = state.last_read_time.unwrap_or(now);
    with_memory(account, |memory| {
        if let Some(summary) = memory.summaries.get_mut(space) {
            summary.unread = 0;
            summary.last_read = Some(last_read.clone());
        }
    });
    Ok(last_read)
}

fn send(account: &str, space: &str, text: &str, thread: Option<&str>) -> Result<ChatMessage> {
    let message = api::create_message(account, space, text, thread)?;
    let me = message.sender.as_ref().map(|s| s.name.clone());
    // The space's activity moved: its summary is refetched on the next poll.
    with_memory(account, |memory| {
        if let Some(summary) = memory.summaries.get_mut(space) {
            summary.stale = true;
        }
        if memory.me.is_none() {
            memory.me = me.clone();
        }
    });
    Ok(ChatMessage { mine: true, ..ChatMessage::new(message, me.as_deref()) })
}

/// An RFC 3339 time as an instant. Google varies the number of fractional digits, so the strings
/// don't compare in time order themselves.
fn instant(time: Option<&str>) -> Option<DateTime<Utc>> {
    time.and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc))
}
