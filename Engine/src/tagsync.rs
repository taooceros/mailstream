//! AI tags synced with the server (MS-172), for every Gmail and Outlook account but those
//! `aiTagging.localOnlyAccounts` lists (MS-341): a conversation's tag lives on its messages as
//! provider tags (Gmail user labels, Outlook categories) named as the app shows them: People,
//! Notifications, Newsletters, Invitations and Waiting on you, and each topic label as
//! `Mailstream/<topic>` (in Gmail a label nested under Mailstream). The five names have no prefix:
//! names match the account's labels in any case, so a label of that name the user already has
//! is the one used. Labels and categories are never deleted; a change only puts them on messages
//! or takes them off.
//!
//! Both ways, going by the conversation's latest message (the tag's key, `tagging::key`):
//! - the server has a tag this engine lacks: it is taken from there, topic labels included, and
//!   the AI tagger leaves the conversation alone, so each conversation is tagged once across the
//!   user's devices;
//! - a tag made or corrected here (`synced` false, topic labels included) goes to the server,
//!   onto every message of the conversation, replacing the other names sync owns there;
//! - the server changed a tag already synced (another device, or the user in Gmail or Outlook):
//!   the server's category, ask and topic labels win; a tag taken off there altogether stays here
//!   and isn't put back;
//! - an ask that lapsed here (`waitingUntil`) takes Waiting on you off the server.
//!
//! The waiting deadline stays on the device that tagged (a label per date would flood the
//! label list), and every label sync doesn't own stays as it is.
use std::collections::BTreeSet;

use crate::model::{Conversation, Date};
use crate::tagging::{Category, ConversationTag, label_text};

/// The provider tag of a conversation waiting on the user (the list's "Waiting on you").
pub const WAITING: &str = "Waiting on you";

/// The names sync puts on messages or takes off besides topic labels (`TOPIC_PREFIX`); other
/// labels are never touched.
pub const NAMES: [&str; 5] = ["People", "Notifications", "Newsletters", "Invitations", WAITING];

/// What a topic label's provider tag starts with: `Mailstream/Finance`.
pub const TOPIC_PREFIX: &str = "Mailstream/";

/// The provider tag of `category`, as the list's sections name it.
pub fn category_name(category: Category) -> &'static str {
    match category {
        Category::People => NAMES[0],
        Category::Notification => NAMES[1],
        Category::Newsletter => NAMES[2],
        Category::Invitation => NAMES[3],
    }
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The topic label a provider tag names (`Mailstream/<topic>`, the prefix in any case).
fn topic(tag: &str) -> Option<String> {
    let rest = tag.get(..TOPIC_PREFIX.len()).filter(|prefix| prefix.eq_ignore_ascii_case(TOPIC_PREFIX))?;
    label_text(&tag[rest.len()..])
}

/// Sync puts `tag` on messages or takes it off: one of `NAMES` or a topic label.
fn owned(tag: &str) -> bool {
    NAMES.iter().any(|name| same(tag, name)) || topic(tag).is_some()
}

/// The same topic labels, in any order and case.
pub(crate) fn same_topics(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|label| b.iter().any(|other| same(label, other)))
}

/// What a message's provider tags say: the categories named (in `Category::ALL` order), whether
/// it waits on the user, and its topic labels (at most two, in the server's order).
#[derive(Debug, PartialEq)]
pub struct ServerTag {
    pub categories: Vec<Category>,
    pub waiting: bool,
    pub topics: Vec<String>,
}

pub fn server_tag(tags: &BTreeSet<String>) -> ServerTag {
    let named = |name: &str| tags.iter().any(|tag| same(tag, name));
    let mut topics: Vec<String> = Vec::new();
    for label in tags.iter().filter_map(|tag| topic(tag)) {
        if topics.len() < 2 && !topics.iter().any(|kept| same(kept, &label)) {
            topics.push(label);
        }
    }
    ServerTag {
        categories: Category::ALL.into_iter().filter(|category| named(category_name(*category))).collect(),
        waiting: named(WAITING),
        topics,
    }
}

/// The names `tag` puts on the server at `now`: its category, Waiting on you while it waits, and
/// its topic labels.
pub fn names(tag: &ConversationTag, now: Date) -> Vec<String> {
    let mut names = vec![category_name(tag.category).to_string()];
    if tag.is_waiting(now) {
        names.push(WAITING.to_string());
    }
    names.extend(tag.labels.iter().map(|label| format!("{TOPIC_PREFIX}{label}")));
    names
}

/// What one conversation needs.
#[derive(Debug, PartialEq)]
pub enum Step {
    Keep,
    /// Take this tag from the server.
    Adopt(ConversationTag),
    /// Put the local tag on the server.
    Push,
}

/// The step for a conversation tagged `local` here whose latest message carries `server`.
/// `adopt_untagged` is false while retagging, when the tagger tags afresh and its tags replace
/// the server's.
pub fn reconcile(local: Option<&ConversationTag>, server: &BTreeSet<String>, now: Date, adopt_untagged: bool) -> Step {
    let server = server_tag(server);
    let Some(local) = local else {
        let Some(&category) = server.categories.first() else { return Step::Keep };
        if !adopt_untagged {
            return Step::Keep;
        }
        return Step::Adopt(ConversationTag {
            category,
            to_respond: server.waiting && category != Category::Newsletter,
            labels: server.topics,
            waiting_until: None,
            // Two categories on the server: this one goes back alone.
            synced: server.categories.len() == 1,
            manual: false,
        });
    };
    if !local.synced {
        return Step::Push;
    }
    // Taken off on the server: left here, and not put back.
    if server.categories.is_empty() {
        return Step::Keep;
    }
    let waiting = local.is_waiting(now);
    if server.categories == [local.category] {
        if server.waiting && local.to_respond && !waiting {
            return Step::Push;
        }
    } else if server.categories.contains(&local.category) {
        // Two devices tagged at once: ours goes back alone; the other takes it from there.
        return Step::Push;
    }
    let category = if server.categories.contains(&local.category) { local.category } else { server.categories[0] };
    let to_respond = server.waiting && category != Category::Newsletter;
    let topics_unchanged = same_topics(&local.labels, &server.topics);
    let tag = ConversationTag {
        category,
        to_respond,
        labels: if topics_unchanged { local.labels.clone() } else { server.topics },
        // A deadline only this device knows holds while the server agrees the ask waits.
        waiting_until: if to_respond && waiting { local.waiting_until } else { None },
        synced: true,
        // Only Waiting on you changed on the server: the user's manual choice here stays theirs.
        manual: local.manual && category == local.category && topics_unchanged,
    };
    let unchanged = tag.category == local.category && to_respond == waiting && topics_unchanged;
    if unchanged { Step::Keep } else { Step::Adopt(tag) }
}

/// One message's change towards `desired`: the names to add, and those of its tags sync owns
/// (`NAMES`, topic labels) to take off (as the server spells them), names matching in any case.
pub fn change(current: &BTreeSet<String>, desired: &[String]) -> (Vec<String>, Vec<String>) {
    let add = desired.iter().filter(|name| !current.iter().any(|tag| same(tag, name))).cloned().collect();
    let remove = current
        .iter()
        .filter(|tag| owned(tag) && !desired.iter().any(|name| same(tag, name)))
        .cloned()
        .collect();
    (add, remove)
}

/// `before` with `remove` taken off and `add` put on, names matching in any case; what stays
/// keeps its place and spelling.
pub fn retagged(before: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let mut after: Vec<String> =
        before.iter().filter(|tag| !remove.iter().any(|name| same(tag, name))).cloned().collect();
    for name in add {
        if !after.iter().any(|tag| same(tag, name)) {
            after.push(name.clone());
        }
    }
    after
}

/// A conversation whose tag goes to the server.
#[derive(Clone, Debug)]
pub struct Push {
    pub key: String,
    /// The tag pushed; it counts as synced only while the local tag is still this one.
    pub tag: ConversationTag,
    pub conversation: Conversation,
    /// The names it puts on every message (`names`).
    pub names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Date = 800_000_000.0;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn tag(category: Category, to_respond: bool, synced: bool) -> ConversationTag {
        ConversationTag { category, to_respond, labels: vec!["Finance".into()], waiting_until: None, synced, manual: false }
    }

    #[test]
    fn reads_the_servers_tags_in_any_case() {
        assert_eq!(
            server_tag(&set(&["newsletters", "Work", "WAITING ON YOU", "mailstream/Travel", "Mailstream/", "Mailstream"])),
            ServerTag { categories: vec![Category::Newsletter], waiting: true, topics: strings(&["Travel"]) }
        );
        assert_eq!(server_tag(&set(&["Receipts"])), ServerTag { categories: vec![], waiting: false, topics: vec![] });
        // At most two topics, each once.
        assert_eq!(
            server_tag(&set(&["Mailstream/A", "Mailstream/a", "Mailstream/B", "Mailstream/C"])).topics,
            strings(&["A", "B"])
        );
    }

    #[test]
    fn names_carry_the_topic_labels() {
        let mut waiting = tag(Category::People, true, false);
        waiting.labels.push("Travel".into());
        assert_eq!(names(&waiting, NOW), strings(&["People", WAITING, "Mailstream/Finance", "Mailstream/Travel"]));
        assert_eq!(names(&tag(Category::Newsletter, false, false), NOW), strings(&["Newsletters", "Mailstream/Finance"]));
    }

    #[test]
    fn adopts_what_the_server_has_and_pushes_what_it_lacks() {
        // Untagged here, tagged there: taken over with its topics, so the tagger leaves it alone.
        let adopted = reconcile(None, &set(&["people", WAITING, "Mailstream/Travel"]), NOW, true);
        assert_eq!(
            adopted,
            Step::Adopt(ConversationTag {
                category: Category::People,
                to_respond: true,
                labels: strings(&["Travel"]),
                waiting_until: None,
                synced: true,
                manual: false,
            })
        );
        // Newsletters never wait; retagging tags afresh; nothing there leaves it to the tagger,
        // topics alone included.
        assert!(matches!(
            reconcile(None, &set(&["Newsletters", WAITING]), NOW, true),
            Step::Adopt(ConversationTag { to_respond: false, .. })
        ));
        assert_eq!(reconcile(None, &set(&["People"]), NOW, false), Step::Keep);
        assert_eq!(reconcile(None, &set(&["Work"]), NOW, true), Step::Keep);
        assert_eq!(reconcile(None, &set(&["Mailstream/Travel"]), NOW, true), Step::Keep);
        // A tag made or corrected here goes there, whatever the server has.
        assert_eq!(reconcile(Some(&tag(Category::People, false, false)), &set(&["Notifications"]), NOW, true), Step::Push);
        assert_eq!(reconcile(Some(&tag(Category::People, false, false)), &set(&[]), NOW, true), Step::Push);
        assert_eq!(
            reconcile(Some(&tag(Category::People, false, false)), &set(&["People", "Mailstream/Finance"]), NOW, true),
            Step::Push
        );
    }

    #[test]
    fn follows_the_server_once_synced() {
        let synced = tag(Category::People, false, true);
        assert_eq!(reconcile(Some(&synced), &set(&["People", "mailstream/FINANCE"]), NOW, true), Step::Keep);
        // Taken off there altogether: left here, never put back.
        assert_eq!(reconcile(Some(&synced), &set(&["Work"]), NOW, true), Step::Keep);
        assert_eq!(reconcile(Some(&synced), &set(&["Mailstream/Travel"]), NOW, true), Step::Keep);
        // Changed there: the server wins, topic labels as they are there.
        assert_eq!(
            reconcile(Some(&synced), &set(&["Notifications", WAITING, "Mailstream/Finance"]), NOW, true),
            Step::Adopt(ConversationTag {
                category: Category::Notification,
                to_respond: true,
                labels: strings(&["Finance"]),
                waiting_until: None,
                synced: true,
                manual: false,
            })
        );
        // A topic added, or taken off, on another device.
        let mut two = synced.clone();
        two.labels = strings(&["Finance", "Travel"]);
        assert_eq!(
            reconcile(Some(&synced), &set(&["People", "Mailstream/Finance", "Mailstream/Travel"]), NOW, true),
            Step::Adopt(ConversationTag { labels: strings(&["Finance", "Travel"]), ..synced.clone() })
        );
        assert_eq!(reconcile(Some(&two), &set(&["People", "Mailstream/Travel", "Mailstream/Finance"]), NOW, true), Step::Keep);
        assert_eq!(
            reconcile(Some(&two), &set(&["People", "Mailstream/Travel"]), NOW, true),
            Step::Adopt(ConversationTag { labels: strings(&["Travel"]), ..synced.clone() })
        );
        assert_eq!(
            reconcile(Some(&synced), &set(&["People"]), NOW, true),
            Step::Adopt(ConversationTag { labels: vec![], ..synced.clone() })
        );
        // Resolved on another device.
        let mut waiting = ConversationTag { manual: true, ..tag(Category::People, true, true) };
        waiting.waiting_until = Some(NOW + 3600.0);
        assert!(matches!(
            reconcile(Some(&waiting), &set(&["People", "Mailstream/Finance"]), NOW, true),
            Step::Adopt(ConversationTag { to_respond: false, waiting_until: None, manual: true, .. })
        ));
        // Still waiting there: the deadline only this device knows stays.
        assert_eq!(reconcile(Some(&waiting), &set(&["People", WAITING, "Mailstream/Finance"]), NOW, true), Step::Keep);
        // Lapsed here: Waiting on you comes off the server.
        waiting.waiting_until = Some(NOW - 1.0);
        assert_eq!(reconcile(Some(&waiting), &set(&["People", WAITING, "Mailstream/Finance"]), NOW, true), Step::Push);
        assert_eq!(reconcile(Some(&waiting), &set(&["People", "Mailstream/Finance"]), NOW, true), Step::Keep);
        // Two categories (two devices at once): ours goes back alone.
        assert_eq!(reconcile(Some(&synced), &set(&["People", "Newsletters"]), NOW, true), Step::Push);
    }

    #[test]
    fn changes_only_the_names_sync_owns() {
        let current = set(&["notifications", "Work", WAITING, "Mailstream/Old", "mailstream/finance", "Mailstream"]);
        assert_eq!(
            change(&current, &strings(&["People", "Mailstream/Finance"])),
            (strings(&["People"]), strings(&["Mailstream/Old", WAITING, "notifications"]))
        );
        assert_eq!(
            change(&current, &strings(&["Notifications", WAITING, "Mailstream/Old", "Mailstream/Finance"])),
            (vec![], vec![])
        );
        assert_eq!(
            retagged(&strings(&["Red category", "newsletters"]), &strings(&["People", "red CATEGORY"]), &strings(&["Newsletters"])),
            strings(&["Red category", "People"])
        );
    }
}
