//! Causal local-mutation overlay and per-account FIFO turn queue for [`super::Mail`] (MS-429).
//!
//! Every optimistic user mutation (read/unread, star/unstar, archive/trash/restore/undo, tag push)
//! is stamped on a monotonic `seq` clock when it starts (`settled_at: None`) and again when the
//! provider confirms it (`settled_at: Some(done)`), and takes a FIFO [`Turn`] for its account in
//! the same lock acquisition so provider requests always run in exact user-issued order.
//!
//! - Any listing or unread-count fetch started at `fetch_started_at` sees every mutation still in
//!   flight or settled after `fetch_started_at` (`settled_at.is_none_or(|d| d > fetch_started_at)`),
//!   preventing stale background refreshes from clobbering newer edits while new mail still appears.
//! - Once a fresh first-page fetch started after `settled_at` lands, settled entries for that
//!   mailbox are retired automatically — no timers or heuristics.
//! - A failed mutation only rolls back targets still owned by its `seq`, so an older failure can
//!   never revert a newer edit on the same message.
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::backend::Flag;
use crate::model::{DISTANT_PAST, Envelope, MailMessage, MailboxKey, MessageRef};

/// A per-account FIFO slot acquired under `Mail::state` right alongside an overlay mutation,
/// guaranteeing provider writes reach the server in exact `seq` order.
pub(super) struct Turn {
    prev: Option<oneshot::Receiver<()>>,
    done: oneshot::Sender<()>,
}

impl Turn {
    /// Waits for every earlier mutation on the same account; dropping the returned token wakes the
    /// next waiter (even on cancellation or panic).
    pub(super) async fn acquire(self) -> oneshot::Sender<()> {
        if let Some(prev) = self.prev {
            let _ = prev.await;
        }
        self.done
    }
}

/// Overlay sequence number paired with the account's FIFO send turn.
pub(super) struct Ticket {
    pub(super) seq: u64,
    pub(super) turn: Turn,
}

#[derive(Clone, Debug)]
struct Entry<T> {
    value: T,
    seq: u64,
    settled_at: Option<u64>,
}

impl<T> Entry<T> {
    fn new(value: T, seq: u64) -> Self {
        Self { value, seq, settled_at: None }
    }

    fn active_for(&self, fetch_started_at: u64) -> bool {
        self.settled_at.is_none_or(|done| done > fetch_started_at)
    }
}

#[derive(Clone, Debug)]
enum Move {
    Removed,
    Inserted(Arc<Envelope>),
}

#[derive(Clone, Debug)]
struct TagDelta {
    add: Vec<String>,
    remove: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct UnreadGuard {
    in_flight: usize,
    latest_seq: u64,
}

#[derive(Default)]
pub(super) struct LocalOverlay {
    seq: u64,
    flags: HashMap<(MessageRef, Flag), Entry<bool>>,
    moves: HashMap<MessageRef, Entry<Move>>,
    tags: HashMap<MessageRef, Vec<Entry<TagDelta>>>,
    unread: HashMap<String, UnreadGuard>,
    turns: HashMap<String, oneshot::Receiver<()>>,
}

impl LocalOverlay {
    /// Current causal clock value; snapshot before starting any provider listing or count fetch.
    pub(super) fn clock(&self) -> u64 {
        self.seq
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn take_turn(&mut self, account: &str) -> Turn {
        let (done, next) = oneshot::channel();
        let prev = self.turns.insert(account.to_string(), next);
        Turn { prev, done }
    }

    fn begin_unread(&mut self, account: &str, seq: u64) {
        let guard = self.unread.entry(account.to_string()).or_default();
        guard.in_flight += 1;
        guard.latest_seq = guard.latest_seq.max(seq);
    }

    fn finish_unread(&mut self, account: &str, seq: u64) {
        let guard = self.unread.entry(account.to_string()).or_default();
        guard.in_flight = guard.in_flight.saturating_sub(1);
        guard.latest_seq = guard.latest_seq.max(seq);
    }

    /// Whether an account's unread count fetched starting at `fetch_started_at` may predate an
    /// in-flight or recently settled local unread change.
    pub(super) fn unread_stale(&self, account: &str, fetch_started_at: u64) -> bool {
        self.unread
            .get(account)
            .is_some_and(|guard| guard.in_flight > 0 || guard.latest_seq > fetch_started_at)
    }

    pub(super) fn begin_flag(&mut self, account: &str, messages: &[MailMessage], flag: Flag, on: bool) -> Ticket {
        let seq = self.next_seq();
        for message in messages {
            self.flags.insert((message.message_ref(), flag), Entry::new(on, seq));
        }
        if flag == Flag::Seen && !messages.is_empty() {
            self.begin_unread(account, seq);
        }
        Ticket { seq, turn: self.take_turn(account) }
    }

    /// Completes a flag mutation started at `seq`. When `!ok`, removes only entries still owned by
    /// `seq` and returns those messages so callers roll back nothing a newer edit superseded.
    pub(super) fn finish_flag(
        &mut self,
        account: &str,
        messages: &[MailMessage],
        flag: Flag,
        seq: u64,
        ok: bool,
    ) -> Vec<MailMessage> {
        let done = self.next_seq();
        if flag == Flag::Seen && !messages.is_empty() {
            self.finish_unread(account, done);
        }
        let rolled = settle(&mut self.flags, messages.iter().map(|m| (m.message_ref(), flag)), seq, done, ok);
        if rolled.is_empty() {
            return Vec::new();
        }
        let rolled: HashSet<MessageRef> = rolled.into_iter().map(|(reference, _)| reference).collect();
        messages.iter().filter(|m| rolled.contains(&m.message_ref())).cloned().collect()
    }

    pub(super) fn begin_move(&mut self, account: &str, messages: &[MailMessage], removed: bool) -> Ticket {
        let seq = self.next_seq();
        for message in messages {
            let state = if removed { Move::Removed } else { Move::Inserted(message.envelope.clone()) };
            self.moves.insert(message.message_ref(), Entry::new(state, seq));
        }
        if messages.iter().any(|message| !message.envelope.is_seen()) {
            self.begin_unread(account, seq);
        }
        Ticket { seq, turn: self.take_turn(account) }
    }

    /// Completes a move/delete mutation started at `seq`. When `!ok`, removes only entries still
    /// owned by `seq` and returns those messages for rollback.
    pub(super) fn finish_move(
        &mut self,
        account: &str,
        messages: &[MailMessage],
        seq: u64,
        ok: bool,
    ) -> Vec<MailMessage> {
        let done = self.next_seq();
        if messages.iter().any(|message| !message.envelope.is_seen()) {
            self.finish_unread(account, done);
        }
        let rolled: HashSet<MessageRef> =
            settle(&mut self.moves, messages.iter().map(MailMessage::message_ref), seq, done, ok).into_iter().collect();
        if rolled.is_empty() {
            return Vec::new();
        }
        messages.iter().filter(|m| rolled.contains(&m.message_ref())).cloned().collect()
    }

    pub(super) fn begin_tags(
        &mut self,
        account: &str,
        refs: &HashSet<MessageRef>,
        add: &[String],
        remove: &[String],
    ) -> Ticket {
        let seq = self.next_seq();
        let delta = TagDelta { add: add.to_vec(), remove: remove.to_vec() };
        for reference in refs {
            self.tags.entry(reference.clone()).or_default().push(Entry::new(delta.clone(), seq));
        }
        Ticket { seq, turn: self.take_turn(account) }
    }

    /// Completes a tag mutation started at `seq`. On failure, removes its delta and returns only
    /// references with no newer tag edit on top of `seq` so an older failure never reverts newer tags.
    pub(super) fn finish_tags(&mut self, refs: &HashSet<MessageRef>, seq: u64, ok: bool) -> HashSet<MessageRef> {
        let done = self.next_seq();
        let mut rollback = HashSet::new();
        for reference in refs {
            let Some(entries) = self.tags.get_mut(reference) else { continue };
            if ok {
                if let Some(entry) = entries.iter_mut().find(|entry| entry.seq == seq) {
                    entry.settled_at = Some(done);
                }
            } else {
                let is_latest = entries.last().is_some_and(|entry| entry.seq == seq);
                entries.retain(|entry| entry.seq != seq);
                if entries.is_empty() {
                    self.tags.remove(reference);
                }
                if is_latest {
                    rollback.insert(reference.clone());
                }
            }
        }
        rollback
    }

    /// Applies every local mutation on `key` that is still in flight or settled after
    /// `fetch_started_at` onto `envelopes`. When `prune` is true (a fresh first-page fetch),
    /// settled entries predating `fetch_started_at` are retired.
    pub(super) fn apply(
        &mut self,
        key: &MailboxKey,
        envelopes: &mut Vec<Arc<Envelope>>,
        fetch_started_at: u64,
        prune: bool,
    ) {
        if prune {
            self.moves.retain(|reference, entry| reference.key() != *key || entry.active_for(fetch_started_at));
            self.flags.retain(|(reference, _), entry| reference.key() != *key || entry.active_for(fetch_started_at));
            self.tags.retain(|reference, entries| {
                if reference.key() == *key {
                    entries.retain(|entry| entry.active_for(fetch_started_at));
                    !entries.is_empty()
                } else {
                    true
                }
            });
        }

        // 1. Moves: strip locally removed messages and re-insert locally restored/undone ones.
        let mut removed_ids: HashSet<&str> = HashSet::new();
        let mut inserted: Vec<Arc<Envelope>> = Vec::new();
        for (reference, entry) in &self.moves {
            if reference.key() != *key || !entry.active_for(fetch_started_at) {
                continue;
            }
            match &entry.value {
                Move::Removed => {
                    removed_ids.insert(reference.id.as_str());
                }
                Move::Inserted(envelope) => inserted.push(envelope.clone()),
            }
        }
        if !removed_ids.is_empty() {
            envelopes.retain(|envelope| !removed_ids.contains(envelope.id.as_str()));
        }
        for envelope in inserted {
            if envelopes.iter().any(|existing| existing.id == envelope.id) {
                continue;
            }
            let date = envelope.date.unwrap_or(DISTANT_PAST);
            let at = envelopes
                .iter()
                .position(|existing| existing.date.unwrap_or(DISTANT_PAST) < date)
                .unwrap_or(envelopes.len());
            envelopes.insert(at, envelope);
        }

        // 2. Flags and tags on the resulting envelopes.
        let mut flag_edits: HashMap<&str, Vec<(Flag, bool)>> = HashMap::new();
        for ((reference, flag), entry) in &self.flags {
            if reference.key() == *key && entry.active_for(fetch_started_at) {
                flag_edits.entry(reference.id.as_str()).or_default().push((*flag, entry.value));
            }
        }
        let mut tag_edits: HashMap<&str, Vec<&TagDelta>> = HashMap::new();
        for (reference, entries) in &self.tags {
            if reference.key() == *key {
                let active: Vec<&TagDelta> =
                    entries.iter().filter(|entry| entry.active_for(fetch_started_at)).map(|entry| &entry.value).collect();
                if !active.is_empty() {
                    tag_edits.insert(reference.id.as_str(), active);
                }
            }
        }
        if flag_edits.is_empty() && tag_edits.is_empty() {
            return;
        }
        for envelope in envelopes.iter_mut() {
            let flags = flag_edits.get(envelope.id.as_str());
            let tags = tag_edits.get(envelope.id.as_str());
            if flags.is_none() && tags.is_none() {
                continue;
            }
            let mut changed = (**envelope).clone();
            if let Some(flags) = flags {
                for &(flag, on) in flags {
                    changed.set_flag(flag, on);
                }
            }
            if let Some(deltas) = tags {
                for delta in deltas {
                    changed.retag(&delta.add, &delta.remove);
                }
            }
            *envelope = Arc::new(changed);
        }
    }

    pub(super) fn forget_accounts(&mut self, removed: &HashSet<String>) {
        self.flags.retain(|(reference, _), _| !removed.contains(&reference.account));
        self.moves.retain(|reference, _| !removed.contains(&reference.account));
        self.tags.retain(|reference, _| !removed.contains(&reference.account));
        self.unread.retain(|account, _| !removed.contains(account));
        self.turns.retain(|account, _| !removed.contains(account));
    }
}

fn settle<K: Eq + Hash, V>(
    map: &mut HashMap<K, Entry<V>>,
    keys: impl IntoIterator<Item = K>,
    seq: u64,
    done: u64,
    ok: bool,
) -> Vec<K> {
    let mut rolled_back = Vec::new();
    for key in keys {
        if ok {
            if let Some(entry) = map.get_mut(&key)
                && entry.seq == seq
            {
                entry.settled_at = Some(done);
            }
        } else if map.get(&key).is_some_and(|entry| entry.seq == seq) {
            map.remove(&key);
            rolled_back.push(key);
        }
    }
    rolled_back
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::model::MailboxRole;

    fn envelope(id: &str, seen: bool, date: f64) -> Arc<Envelope> {
        Arc::new(Envelope {
            id: id.into(),
            thread: None,
            message_id: None,
            in_reply_to: vec![],
            flags: Envelope::flags(seen, false, false),
            subject: id.into(),
            from: vec![],
            to: vec![],
            date: Some(date),
            size: 0,
            has_attachment: None,
            tags: BTreeSet::new(),
            provider_category: None,
            important: false,
        })
    }

    fn msg(key: &MailboxKey, env: Arc<Envelope>) -> MailMessage {
        MailMessage { key: Arc::new(key.clone()), envelope: env, role: MailboxRole::Inbox }
    }

    #[test]
    fn protects_in_flight_and_post_fetch_edits_then_prunes_after_fresh_fetch() {
        let key = MailboxKey::new("me@x.com", "INBOX");
        let mut overlay = LocalOverlay::default();
        let m1 = [msg(&key, envelope("m1", false, 100.0))];
        let m2 = [msg(&key, envelope("m2", false, 200.0))];

        // Fetch #1 starts before user acts.
        let fetch1 = overlay.clock();
        let t_flag = overlay.begin_flag("me@x.com", &m1, Flag::Seen, true);
        let t_move = overlay.begin_move("me@x.com", &m2, true);
        assert!(overlay.unread_stale("me@x.com", fetch1));

        // Even after both mutations settle, fetch #1 predates their settlement so overlay wins.
        assert!(overlay.finish_flag("me@x.com", &m1, Flag::Seen, t_flag.seq, true).is_empty());
        assert!(overlay.finish_move("me@x.com", &m2, t_move.seq, true).is_empty());
        assert!(overlay.unread_stale("me@x.com", fetch1));

        let mut page1 = vec![envelope("m1", false, 100.0), envelope("m2", false, 200.0), envelope("m3", false, 300.0)];
        overlay.apply(&key, &mut page1, fetch1, true);
        assert_eq!(page1.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["m1", "m3"]);
        assert!(page1[0].is_seen(), "m1 must stay read against stale fetch #1");

        // Fetch #2 starts AFTER settlement: server truth takes over and settled entries prune.
        let fetch2 = overlay.clock();
        assert!(!overlay.unread_stale("me@x.com", fetch2));
        let mut page2 = vec![envelope("m1", false, 100.0), envelope("m3", false, 300.0)];
        overlay.apply(&key, &mut page2, fetch2, true);
        assert!(!page2[0].is_seen(), "fresh post-settle server state takes over");
        assert!(overlay.flags.is_empty() && overlay.moves.is_empty());
    }

    #[test]
    fn stale_failure_never_rolls_back_newer_edit() {
        let key = MailboxKey::new("me@x.com", "INBOX");
        let mut overlay = LocalOverlay::default();
        let m1 = [msg(&key, envelope("m1", false, 100.0))];

        let t1 = overlay.begin_flag("me@x.com", &m1, Flag::Flagged, true);
        let t2 = overlay.begin_flag("me@x.com", &m1, Flag::Flagged, false);

        // t1 fails, but t2 already superseded it: rollback must be empty and t2 stays active.
        let rollback = overlay.finish_flag("me@x.com", &m1, Flag::Flagged, t1.seq, false);
        assert!(rollback.is_empty());
        assert_eq!(overlay.flags[&(m1[0].message_ref(), Flag::Flagged)].seq, t2.seq);
    }

    #[test]
    fn sequential_tag_deltas_compose_and_prune_cleanly() {
        let key = MailboxKey::new("me@x.com", "INBOX");
        let mut overlay = LocalOverlay::default();
        let r1 = MessageRef { account: "me@x.com".into(), mailbox: "INBOX".into(), id: "m1".into() };
        let set1 = HashSet::from([r1.clone()]);

        let fetch1 = overlay.clock();
        let t1 = overlay.begin_tags("me@x.com", &set1, &["Newsletters".into()], &["People".into()]);
        let t2 = overlay.begin_tags("me@x.com", &set1, &["Mailstream/Finance".into()], &[]);
        assert!(overlay.finish_tags(&set1, t1.seq, true).is_empty());
        assert!(overlay.finish_tags(&set1, t2.seq, true).is_empty());

        let mut env = (*envelope("m1", false, 100.0)).clone();
        env.tags = BTreeSet::from(["People".into(), "Receipts".into()]);
        let mut page = vec![Arc::new(env)];
        overlay.apply(&key, &mut page, fetch1, true);
        assert_eq!(page[0].tags, BTreeSet::from(["Mailstream/Finance".into(), "Newsletters".into(), "Receipts".into()]));

        // Post-settle fetch prunes both deltas.
        let fetch2 = overlay.clock();
        overlay.apply(&key, &mut page, fetch2, true);
        assert!(overlay.tags.is_empty());
    }
}
