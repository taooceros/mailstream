//! Groups messages into conversations as the Swift engine's `Threader` does
//! (`Sources/MailstreamKit/Store/Threader.swift`): the provider's thread, `Message-ID` /
//! `In-Reply-To` links, and the normalized subject for replies whose parent was not loaded.
//!
//! `primary` messages decide which conversations exist (the listed mailbox); `context` messages
//! (the account's inbox and sent mail) only join conversations a primary message started, so a
//! conversation shows both sides of an exchange.
use std::collections::{HashMap, HashSet};

use crate::model::{Conversation, DISTANT_PAST, MailMessage};
use crate::text::Subject;

const SUBJECT_WINDOW: f64 = 45.0 * 24.0 * 3600.0;

pub fn conversations(primary: Vec<MailMessage>, context: Vec<MailMessage>) -> Vec<Conversation> {
    let mut seen: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut chosen: Vec<(usize, bool)> = Vec::with_capacity(primary.len() + context.len());
    let all: Vec<(&MailMessage, bool)> =
        primary.iter().map(|message| (message, true)).chain(context.iter().map(|message| (message, false))).collect();
    for (index, (message, is_primary)) in all.iter().enumerate() {
        if seen.insert((message.account(), message.mailbox(), message.id())) {
            chosen.push((index, *is_primary));
        }
    }
    if chosen.is_empty() {
        return Vec::new();
    }
    let messages: Vec<&MailMessage> = chosen.iter().map(|(index, _)| all[*index].0).collect();
    let is_primary: Vec<bool> = chosen.iter().map(|(_, primary)| *primary).collect();

    let mut parent: Vec<usize> = (0..messages.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }

    // Message-ID links, scoped per account.
    let mut by_message_id: HashMap<(&str, &str), usize> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let Some(id) = message.envelope.message_id.as_deref() else { continue };
        match by_message_id.get(&(message.account(), id)) {
            Some(&existing) => union(&mut parent, existing, index),
            None => {
                by_message_id.insert((message.account(), id), index);
            }
        }
    }
    for (index, message) in messages.iter().enumerate() {
        for parent_id in &message.envelope.in_reply_to {
            if let Some(&target) = by_message_id.get(&(message.account(), parent_id.as_str())) {
                union(&mut parent, index, target);
            }
        }
    }

    // The provider's own threads, scoped per account.
    let mut by_thread: HashMap<(&str, &str), usize> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let Some(thread) = message.envelope.thread.as_deref() else { continue };
        match by_thread.get(&(message.account(), thread)) {
            Some(&existing) => union(&mut parent, existing, index),
            None => {
                by_thread.insert((message.account(), thread), index);
            }
        }
    }

    // Subject fallback, oldest first so a reply joins the latest earlier message on that subject.
    let date = |index: usize| messages[index].envelope.date.unwrap_or(DISTANT_PAST);
    let mut order: Vec<usize> = (0..messages.len()).collect();
    order.sort_by(|a, b| date(*a).total_cmp(&date(*b)));
    let mut last_by_subject: HashMap<(&str, String), usize> = HashMap::new();
    for index in order {
        let envelope = &messages[index].envelope;
        let (normalized, had_prefix) = thread_key(&envelope.subject);
        if normalized.chars().count() < 4 {
            continue;
        }
        let key = (messages[index].account(), normalized);
        if let Some(&previous) = last_by_subject.get(&key)
            && (had_prefix || !envelope.in_reply_to.is_empty())
            && date(index) - date(previous) < SUBJECT_WINDOW
        {
            union(&mut parent, index, previous);
        }
        last_by_subject.insert(key, index);
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..messages.len() {
        groups.entry(find(&mut parent, index)).or_default().push(index);
    }
    let mut out: Vec<Conversation> = Vec::with_capacity(groups.len());
    for mut members in groups.into_values() {
        if !members.iter().any(|index| is_primary[*index]) {
            continue;
        }
        // Collapse copies of one message living in several mailboxes (Gmail labels share the
        // provider id), preferring the primary copy.
        members.sort_by_key(|index| !is_primary[*index]);
        let (mut ids, mut message_ids) = (HashSet::new(), HashSet::new());
        let mut unique = Vec::with_capacity(members.len());
        for index in members {
            let envelope = &messages[index].envelope;
            if !ids.insert(envelope.id.as_str()) {
                continue;
            }
            if let Some(id) = envelope.message_id.as_deref()
                && !message_ids.insert(id)
            {
                continue;
            }
            unique.push(messages[index].clone());
        }
        out.push(Conversation::thread(unique));
    }
    sort_newest_first(&mut out);
    out
}

fn sort_newest_first(conversations: &mut [Conversation]) {
    conversations.sort_by(|a, b| {
        b.latest_date.unwrap_or(DISTANT_PAST).total_cmp(&a.latest_date.unwrap_or(DISTANT_PAST))
    });
}

/// Folds conversations that are one sender repeating itself (every message from the same
/// address, on the same subject: security alerts, listing digests the provider threads apart)
/// into one, so the inbox shows that stream once at its latest message.
pub fn merge_repeats(conversations: Vec<Conversation>) -> Vec<Conversation> {
    let mut index: HashMap<(String, String, String), usize> = HashMap::new();
    let mut groups: Vec<Vec<Conversation>> = Vec::new();
    for conversation in conversations {
        let Some(key) = repeat_key(&conversation) else {
            groups.push(vec![conversation]);
            continue;
        };
        match index.get(&key) {
            Some(&existing) => groups[existing].push(conversation),
            None => {
                index.insert(key, groups.len());
                groups.push(vec![conversation]);
            }
        }
    }
    // Newest first, so each group sits where its latest member did.
    groups
        .into_iter()
        .map(|mut group| {
            if group.len() == 1 {
                group.pop().unwrap()
            } else {
                Conversation::thread(group.into_iter().flat_map(|conversation| conversation.messages).collect())
            }
        })
        .collect()
}

fn repeat_key(conversation: &Conversation) -> Option<(String, String, String)> {
    let sender = conversation.messages.first()?.envelope.from.first()?.email.to_lowercase();
    if conversation
        .messages
        .iter()
        .any(|message| message.envelope.from.first().map(|address| address.email.to_lowercase()) != Some(sender.clone()))
    {
        return None;
    }
    let (subject, _) = thread_key(&conversation.subject);
    if subject.is_empty() {
        return None;
    }
    Some((conversation.account.clone(), sender, subject))
}

/// The subject as threading compares it: list tags and title lowercased, whitespace collapsed;
/// and whether it had a reply or forward prefix.
fn thread_key(raw: &str) -> (String, bool) {
    let subject = Subject::parse(raw);
    let text = if subject.list.is_empty() { subject.title } else { format!("{} {}", subject.list, subject.title) };
    (text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" "), subject.prefix.is_some())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::{Address, Envelope, MailboxKey, MailboxRole};

    fn message(mailbox: &str, id: &str, subject: &str, from: &str, date: f64, extra: impl FnOnce(&mut Envelope)) -> MailMessage {
        let mut envelope = Envelope {
            id: id.into(),
            thread: None,
            message_id: Some(format!("{id}@x")),
            in_reply_to: vec![],
            flags: Envelope::flags(true, false, false),
            subject: subject.into(),
            from: vec![Address::new(None, from)],
            to: vec![],
            date: Some(date),
            size: 0,
            has_attachment: None,
        };
        extra(&mut envelope);
        let role = if mailbox == "SENT" { MailboxRole::Sent } else { MailboxRole::Inbox };
        MailMessage { key: Arc::new(MailboxKey::new("me@x.com", mailbox)), envelope: Arc::new(envelope), role }
    }

    #[test]
    fn threads_by_reference_thread_and_subject() {
        let primary = vec![
            message("INBOX", "a", "Plans for Friday", "bob@x.com", 100.0, |_| {}),
            message("INBOX", "c", "Re: Plans for Friday", "bob@x.com", 300.0, |_| {}),
            message("INBOX", "d", "Unrelated", "eve@x.com", 400.0, |e| e.thread = Some("t".into())),
            message("INBOX", "a", "Plans for Friday", "bob@x.com", 100.0, |_| {}),
        ];
        let context = vec![
            message("SENT", "b", "Re: Plans for Friday", "me@x.com", 200.0, |e| e.in_reply_to = vec!["a@x".into()]),
            message("SENT", "z", "Other", "me@x.com", 500.0, |_| {}),
        ];
        let threads = conversations(primary, context);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].id, "me@x.com|d@x");
        assert_eq!(threads[1].id, "me@x.com|a@x");
        let ids: Vec<&str> = threads[1].messages.iter().map(MailMessage::id).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(threads[1].latest_date, Some(300.0));
    }

    #[test]
    fn folds_one_senders_repeats() {
        let threads = vec![
            Conversation::thread(vec![message("INBOX", "n2", "Security alert", "no@x.com", 20.0, |_| {})]),
            Conversation::thread(vec![message("INBOX", "p", "Hi", "ann@x.com", 15.0, |_| {})]),
            Conversation::thread(vec![message("INBOX", "n1", "Security alert", "NO@x.com", 10.0, |_| {})]),
        ];
        let merged = merge_repeats(threads);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].messages.len(), 2);
        assert_eq!(merged[0].id, "me@x.com|n1@x");
    }
}
