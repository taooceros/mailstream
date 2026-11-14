//! Auto-read (MS-149): while `autoRead.enabled` is on, stale notifications and newsletters (ads,
//! promotions) in the default list are marked read as `mail.read` marks them, going by the AI tags
//! alone. A conversation qualifies when it is unread, tagged notification or newsletter, not
//! waiting on the user (an ask the tagger dated counts once it lapsed), not starred, holds nothing
//! the user wrote, and is stale: its latest message is `autoRead.days` old, or it is a newsletter
//! whose sender has sent a newer one. A pass runs a moment after the tags, the list or the
//! settings change, and hourly as mail ages.
//!
//! Each conversation is auto-read once per latest message: one the user marks unread again, or
//! undoes with `mail.autoread.undo`, stays unread until a new message arrives. What was marked
//! read and the messages done with are kept in mail.db, so undo and the log outlive a relaunch;
//! front ends hear `autoRead` after each pass that marked something and after an undo.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Mail;
use crate::answers::{self, AutoReadEntry, AutoReadLog, BatchFailure, BatchResult};
use crate::commands::Arguments;
use crate::model::{Conversation, Date, iso8601, now};
use crate::settings::Setting;
use crate::tagging::{self, Candidates, Category, ConversationTag};
use crate::EngineError;

/// A burst of changes (tags landing batch by batch) makes one pass.
const DEBOUNCE: Duration = Duration::from_secs(2);
/// Mail turns stale with time alone; a pass looks again this often.
const RECHECK: Duration = Duration::from_secs(3600);
/// Latest-message keys remembered as done with, oldest dropped first.
const HANDLED_LIMIT: usize = 5000;
/// Conversations the log keeps for undo, newest first.
const LOG_LIMIT: usize = 200;
const DAY: f64 = 86_400.0;

/// Why a conversation counted as stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "lowercase")]
pub enum AutoReadReason {
    /// Its latest message is `autoRead.days` old.
    Age,
    /// A newsletter whose sender sent a newer one.
    Superseded,
}

/// One conversation auto-read and not undone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// The conversation's id, as listings give it.
    id: String,
    /// Its latest message (`tagging::key`) when it was marked read.
    key: String,
    account: String,
    sender: String,
    subject: String,
    category: Category,
    reason: AutoReadReason,
    at: Date,
    /// The pass that marked it; undo without ids takes the latest.
    batch: u64,
}

/// The mail.db row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Saved {
    /// Latest-message keys auto-read once, oldest first; never auto-read again.
    handled: VecDeque<String>,
    /// Newest first.
    entries: Vec<Entry>,
}

impl Saved {
    fn remember(&mut self, key: String) {
        self.handled.push_back(key);
        while self.handled.len() > HANDLED_LIMIT {
            self.handled.pop_front();
        }
    }
}

/// A conversation a pass marks read.
struct Pick {
    conversation: Conversation,
    key: String,
    category: Category,
    reason: AutoReadReason,
}

pub(super) struct AutoRead {
    runtime: tokio::runtime::Handle,
    debounce: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Keys whose marking the provider refused; left alone until the app restarts.
    refused: Mutex<HashSet<String>>,
    /// Held by the pass or undo running, so they never overlap.
    running: tokio::sync::Mutex<()>,
}

impl AutoRead {
    /// Call on the engine's runtime.
    pub(super) fn new() -> Self {
        Self {
            runtime: tokio::runtime::Handle::current(),
            debounce: Mutex::new(None),
            refused: Mutex::new(HashSet::new()),
            running: tokio::sync::Mutex::new(()),
        }
    }
}

/// The candidates that are stale at `now`, newest first. `tag_for` gives the tag as the list
/// shows it (waiting lapsed); `done` says a latest-message key is not to be auto-read.
fn stale(
    candidates: &Candidates,
    tag_for: impl Fn(&Conversation) -> Option<ConversationTag>,
    done: impl Fn(&str) -> bool,
    now: Date,
    days: f64,
) -> Vec<Pick> {
    let cutoff = now - days * DAY;
    let mut newsletter_senders: HashSet<String> = HashSet::new();
    let mut picks = Vec::new();
    for conversation in &candidates.conversations {
        let Some(tag) = tag_for(conversation) else { continue };
        let sender = conversation
            .latest_incoming(&candidates.own)
            .and_then(|message| message.envelope.from.first())
            .map(|address| address.normalized_email());
        // Newest first: a sender seen already sent a newer newsletter.
        let superseded = tag.category == Category::Newsletter
            && sender.as_ref().is_some_and(|sender| !newsletter_senders.insert(sender.clone()));
        if !matches!(tag.category, Category::Notification | Category::Newsletter)
            || tag.to_respond
            || !conversation.is_unread
            || conversation.is_flagged
            || conversation.messages.iter().any(|message| message.is_from_user(&candidates.own))
        {
            continue;
        }
        let key = tagging::key(conversation);
        if done(&key) {
            continue;
        }
        let reason = if conversation.latest_date.is_some_and(|date| date <= cutoff) {
            AutoReadReason::Age
        } else if superseded {
            AutoReadReason::Superseded
        } else {
            continue;
        };
        picks.push(Pick { conversation: conversation.clone(), key, category: tag.category, reason });
    }
    picks
}

impl Mail {
    /// Runs passes as the tags, the list and the settings change, and hourly.
    pub(super) fn start_auto_read(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.events.listen(move |event| {
            if matches!(event, "tags" | "conversations" | "settings")
                && let Some(mail) = weak.upgrade()
            {
                mail.schedule_auto_read();
            }
        });
        let weak = Arc::downgrade(self);
        self.auto_read.runtime.spawn(async move {
            loop {
                tokio::time::sleep(RECHECK).await;
                let Some(mail) = weak.upgrade() else { return };
                mail.schedule_auto_read();
            }
        });
    }

    fn auto_read_enabled(&self) -> bool {
        self.settings.value(Setting::AutoReadEnabled) == Value::Bool(true)
    }

    /// A pass a moment after the last call, while auto-read is on.
    fn schedule_auto_read(self: &Arc<Self>) {
        if !self.auto_read_enabled() {
            return;
        }
        let mut debounce = self.auto_read.debounce.lock().expect("auto-read lock");
        if let Some(previous) = debounce.take() {
            previous.abort();
        }
        let weak = Arc::downgrade(self);
        *debounce = Some(self.auto_read.runtime.spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if let Some(mail) = weak.upgrade() {
                // Detached from the debounce, so a later call cannot abort it halfway.
                tokio::spawn(async move { mail.auto_read_pass().await });
            }
        }));
    }

    /// The mail.db row, with this process's queued writes landed first (reads go to their own
    /// connection).
    async fn saved_auto_read(&self) -> Saved {
        self.cache.flush().await;
        self.cache.load_auto_read().await.unwrap_or_default()
    }

    async fn auto_read_pass(self: &Arc<Self>) {
        let _running = self.auto_read.running.lock().await;
        if !self.auto_read_enabled() || !self.tagging.is_enabled() {
            return;
        }
        let days = self.settings.value(Setting::AutoReadDays).as_f64().unwrap_or(3.0);
        let saved = self.saved_auto_read().await;
        let handled: HashSet<&str> = saved.handled.iter().map(String::as_str).collect();
        let candidates = self.tag_candidates();
        let picks = {
            let refused = self.auto_read.refused.lock().expect("auto-read lock");
            stale(
                &candidates,
                |conversation| self.tagging.tag_for(conversation),
                |key| handled.contains(key) || refused.contains(key),
                now(),
                days,
            )
        };
        drop(handled);
        if picks.is_empty() {
            return;
        }
        // One request per account's mailboxes; a refusal leaves only that account's unread.
        let mut by_account: Vec<(String, Vec<Pick>)> = Vec::new();
        for pick in picks {
            let account = pick.conversation.account.clone();
            match by_account.iter_mut().find(|(known, _)| *known == account) {
                Some((_, picks)) => picks.push(pick),
                None => by_account.push((account, vec![pick])),
            }
        }
        let mut read = Vec::new();
        for (_, picks) in by_account {
            let conversations: Vec<Conversation> = picks.iter().map(|pick| pick.conversation.clone()).collect();
            match self.set_read_all(&conversations, true).await {
                Ok(()) => read.extend(picks),
                Err(_) => self.auto_read.refused.lock().expect("auto-read lock").extend(picks.into_iter().map(|pick| pick.key)),
            }
        }
        if read.is_empty() {
            return;
        }
        // Read again: another process may have written meanwhile.
        let mut saved = self.saved_auto_read().await;
        let batch = saved.entries.iter().map(|entry| entry.batch).max().unwrap_or(0) + 1;
        let at = now();
        let mut entries: Vec<Entry> = read
            .into_iter()
            .map(|pick| {
                let sender = pick
                    .conversation
                    .latest_incoming(&candidates.own)
                    .and_then(|message| message.envelope.from.first())
                    .map(|address| address.display_name())
                    .unwrap_or_default();
                saved.remember(pick.key.clone());
                Entry {
                    id: pick.conversation.id.clone(),
                    key: pick.key,
                    account: pick.conversation.account.clone(),
                    sender,
                    subject: pick.conversation.subject.clone(),
                    category: pick.category,
                    reason: pick.reason,
                    at,
                    batch,
                }
            })
            .collect();
        saved.entries.retain(|entry| !entries.iter().any(|new| new.id == entry.id));
        entries.append(&mut saved.entries);
        entries.truncate(LOG_LIMIT);
        saved.entries = entries;
        self.cache.save_auto_read(&saved);
        self.events.emit_soon("autoRead");
    }

    // MARK: - Commands

    /// `mail.autoread` and `mail.autoread.undo`.
    pub(super) async fn auto_read_command(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        match command {
            "mail.autoread" => {
                let saved = self.saved_auto_read().await;
                let entries = saved
                    .entries
                    .iter()
                    .map(|entry| AutoReadEntry {
                        id: entry.id.clone(),
                        account: entry.account.clone(),
                        sender: entry.sender.clone(),
                        subject: entry.subject.clone(),
                        category: entry.category,
                        reason: entry.reason,
                        at: iso8601(entry.at),
                        batch: entry.batch,
                    })
                    .collect();
                Ok(answers::json(&AutoReadLog {
                    enabled: self.auto_read_enabled(),
                    days: self.settings.value(Setting::AutoReadDays).as_i64().unwrap_or_default(),
                    entries,
                }))
            }
            "mail.autoread.undo" => self.undo_auto_read(args.list("ids")).await,
            _ => unreachable!("{command} is an auto-read command"),
        }
    }

    /// Marks auto-read conversations unread again: `ids`, else the latest pass's. They stay done
    /// with, so no pass reads them again until a new message arrives.
    async fn undo_auto_read(self: &Arc<Self>, ids: Option<Vec<String>>) -> Result<Value, EngineError> {
        let _running = self.auto_read.running.lock().await;
        let saved = self.saved_auto_read().await;
        let ids = ids.filter(|ids| !ids.is_empty()).unwrap_or_else(|| {
            let latest = saved.entries.iter().map(|entry| entry.batch).max();
            saved.entries.iter().filter(|entry| Some(entry.batch) == latest).map(|entry| entry.id.clone()).collect()
        });
        let mut failed: Vec<BatchFailure> = Vec::new();
        let mut by_account: HashMap<String, Vec<Conversation>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for id in &ids {
            if !saved.entries.iter().any(|entry| entry.id == *id) {
                failed.push(BatchFailure { id: id.clone(), code: Some("not_found".into()), message: format!("{id} was not auto-read.") });
                continue;
            }
            match self.known(id) {
                Ok(known) => {
                    let account = known.conversation.account.clone();
                    if !by_account.contains_key(&account) {
                        order.push(account.clone());
                    }
                    by_account.entry(account).or_default().push(known.conversation);
                }
                Err(error) => failed.push(BatchFailure::of(id, &error)),
            }
        }
        let mut done: Vec<String> = Vec::new();
        for account in order {
            let conversations = by_account.remove(&account).unwrap_or_default();
            match self.set_read_all(&conversations, false).await {
                Ok(()) => done.extend(conversations.into_iter().map(|conversation| conversation.id)),
                Err(error) => failed.extend(conversations.iter().map(|conversation| BatchFailure::of(&conversation.id, &error))),
            }
        }
        if !done.is_empty() {
            let mut saved = self.saved_auto_read().await;
            saved.entries.retain(|entry| !done.contains(&entry.id));
            self.cache.save_auto_read(&saved);
            self.events.emit_soon("autoRead");
        }
        Ok(answers::json(&BatchResult { done, failed, undo: None }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::model::{Address, Envelope, MailMessage, MailboxKey, MailboxRole};

    const NOW: Date = 800_000_000.0;

    fn message(id: &str, from: &str, days_ago: f64, seen: bool, flagged: bool, role: MailboxRole) -> MailMessage {
        let envelope = Envelope {
            id: id.into(),
            thread: Some(format!("t-{id}")),
            in_reply_to: Vec::new(),
            flags: Envelope::flags(seen, flagged, false),
            subject: format!("Subject {id}"),
            from: vec![Address::new(None, from)],
            to: vec![Address::new(None, "me@x.com")],
            date: Some(NOW - days_ago * DAY),
            size: 0,
            has_attachment: Some(false),
            tags: Default::default(),
            provider_category: None,
            important: false,
            message_id: Some(format!("{id}@x")),
        };
        let mailbox = if role == MailboxRole::Sent { "SENT" } else { "INBOX" };
        MailMessage {
            key: Arc::new(MailboxKey { account: "me@x.com".into(), mailbox: mailbox.into() }),
            envelope: Arc::new(envelope),
            role,
        }
    }

    fn conversation(messages: Vec<MailMessage>) -> Conversation {
        let id = format!("me@x.com|{}", messages[0].envelope.id);
        Conversation::new(id, "me@x.com".into(), messages)
    }

    fn tag(category: Category, to_respond: bool) -> ConversationTag {
        ConversationTag { category, to_respond, labels: Vec::new(), waiting_until: None, synced: false, manual: false }
    }

    #[test]
    fn picks_only_stale_unread_notifications_and_newsletters() {
        let inbox = |id: &str, from: &str, days: f64| conversation(vec![message(id, from, days, false, false, MailboxRole::Inbox)]);
        let conversations = vec![
            inbox("n1", "deals@shop.com", 0.1),
            inbox("n2", "deals@shop.com", 1.0),
            inbox("a1", "alerts@bank.com", 5.0),
            inbox("a2", "noreply@github.com", 5.0),
            conversation(vec![message("a3", "ship@x.com", 5.0, false, true, MailboxRole::Inbox)]),
            inbox("p1", "ann@x.com", 5.0),
            inbox("a4", "news@store.com", 1.0),
            conversation(vec![message("a5", "old@x.com", 6.0, true, false, MailboxRole::Inbox)]),
            conversation(vec![
                message("a6", "ci@x.com", 6.0, false, false, MailboxRole::Inbox),
                message("s6", "me@x.com", 5.9, true, false, MailboxRole::Sent),
            ]),
            inbox("a7", "untagged@x.com", 6.0),
            inbox("a8", "done@x.com", 6.0),
            inbox("i1", "calendar@x.com", 6.0),
        ];
        let tags: HashMap<&str, ConversationTag> = HashMap::from([
            ("n1@x", tag(Category::Newsletter, false)),
            ("n2@x", tag(Category::Newsletter, false)),
            ("a1@x", tag(Category::Notification, false)),
            ("a2@x", tag(Category::Notification, true)),
            ("a3@x", tag(Category::Notification, false)),
            ("p1@x", tag(Category::People, false)),
            ("a4@x", tag(Category::Notification, false)),
            ("a5@x", tag(Category::Notification, false)),
            ("s6@x", tag(Category::Notification, false)),
            ("a8@x", tag(Category::Notification, false)),
            ("i1@x", tag(Category::Invitation, false)),
        ]);
        let candidates = Candidates { conversations, own: HashSet::from(["me@x.com".to_string()]) };
        let picks = stale(
            &candidates,
            |conversation| tags.get(tagging::key(conversation).as_str()).cloned(),
            |key| key == "a8@x",
            NOW,
            3.0,
        );
        let picked: Vec<(&str, AutoReadReason)> = picks.iter().map(|pick| (pick.key.as_str(), pick.reason)).collect();
        assert_eq!(picked, [("n2@x", AutoReadReason::Superseded), ("a1@x", AutoReadReason::Age)]);
        assert_eq!(picks[1].category, Category::Notification);
        // A longer wait leaves only the superseded newsletter.
        let picks = stale(&candidates, |c| tags.get(tagging::key(c).as_str()).cloned(), |_| false, NOW, 7.0);
        assert_eq!(picks.iter().map(|pick| pick.key.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["n2@x"]));
    }
}
