//! AI tags synced with the server (MS-172), for the accounts `aiTagging.syncAccounts` lists: a
//! conversation's tag lives on its messages as provider tags (Gmail user labels, Outlook
//! categories) named as the app shows them: People, Notifications, Newsletters, Invitations and
//! Waiting on you. There is no prefix: names match the account's labels in any case, so a label
//! of that name the user already has is the one used. Labels and categories are never deleted;
//! a change only puts them on messages or takes them off.
//!
//! Both ways, going by the conversation's latest message (the tag's key, `tagging::key`):
//! - the server has a tag this engine lacks: it is taken from there, and the AI tagger leaves
//!   the conversation alone, so each conversation is tagged once across the user's devices;
//! - a tag made or corrected here (`synced` false) goes to the server, onto every message of the
//!   conversation, replacing the other names above there;
//! - the server changed a tag already synced (another device, or the user in Gmail or Outlook):
//!   the server's wins; one taken off there altogether stays here and isn't put back;
//! - an ask that lapsed here (`waitingUntil`) takes Waiting on you off the server.
//!
//! Topic labels, the waiting deadline and every other label stay as they are.
use std::collections::BTreeSet;

use crate::model::{Conversation, Date};
use crate::tagging::{Category, ConversationTag};

/// The provider tag of a conversation waiting on the user (the list's "Waiting on you").
pub const WAITING: &str = "Waiting on you";

/// Every name sync puts on messages or takes off; other labels are never touched.
pub const NAMES: [&str; 5] = ["People", "Notifications", "Newsletters", "Invitations", WAITING];

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

/// What a message's provider tags say: the categories named (in `Category::ALL` order) and
/// whether it waits on the user.
#[derive(Debug, PartialEq)]
pub struct ServerTag {
    pub categories: Vec<Category>,
    pub waiting: bool,
}

pub fn server_tag(tags: &BTreeSet<String>) -> ServerTag {
    let named = |name: &str| tags.iter().any(|tag| same(tag, name));
    ServerTag {
        categories: Category::ALL.into_iter().filter(|category| named(category_name(*category))).collect(),
        waiting: named(WAITING),
    }
}

/// The names `tag` puts on the server at `now`: its category, and Waiting on you while it waits.
pub fn names(tag: &ConversationTag, now: Date) -> Vec<&'static str> {
    let mut names = vec![category_name(tag.category)];
    if tag.is_waiting(now) {
        names.push(WAITING);
    }
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
            labels: Vec::new(),
            waiting_until: None,
            // Two categories on the server: this one goes back alone.
            synced: server.categories.len() == 1,
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
    let tag = ConversationTag {
        category,
        to_respond,
        labels: local.labels.clone(),
        // A deadline only this device knows holds while the server agrees the ask waits.
        waiting_until: if to_respond && waiting { local.waiting_until } else { None },
        synced: true,
    };
    let unchanged = tag.category == local.category && to_respond == waiting;
    if unchanged { Step::Keep } else { Step::Adopt(tag) }
}

/// One message's change towards `desired`: the names to add, and those of its tags to take off
/// (as the server spells them), names matching in any case.
pub fn change(current: &BTreeSet<String>, desired: &[&str]) -> (Vec<String>, Vec<String>) {
    let add = desired.iter().filter(|name| !current.iter().any(|tag| same(tag, name))).map(|name| name.to_string()).collect();
    let remove = current
        .iter()
        .filter(|tag| NAMES.iter().any(|name| same(tag, name)) && !desired.iter().any(|name| same(tag, name)))
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
    pub names: Vec<&'static str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Date = 800_000_000.0;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn tag(category: Category, to_respond: bool, synced: bool) -> ConversationTag {
        ConversationTag { category, to_respond, labels: vec!["Finance".into()], waiting_until: None, synced }
    }

    #[test]
    fn reads_the_servers_tags_in_any_case() {
        assert_eq!(
            server_tag(&set(&["newsletters", "Work", "WAITING ON YOU"])),
            ServerTag { categories: vec![Category::Newsletter], waiting: true }
        );
        assert_eq!(server_tag(&set(&["Receipts"])), ServerTag { categories: vec![], waiting: false });
    }

    #[test]
    fn adopts_what_the_server_has_and_pushes_what_it_lacks() {
        // Untagged here, tagged there: taken over, so the tagger leaves it alone.
        let adopted = reconcile(None, &set(&["people", WAITING]), NOW, true);
        assert_eq!(
            adopted,
            Step::Adopt(ConversationTag {
                category: Category::People,
                to_respond: true,
                labels: vec![],
                waiting_until: None,
                synced: true,
            })
        );
        // Newsletters never wait; retagging tags afresh; nothing there leaves it to the tagger.
        assert!(matches!(
            reconcile(None, &set(&["Newsletters", WAITING]), NOW, true),
            Step::Adopt(ConversationTag { to_respond: false, .. })
        ));
        assert_eq!(reconcile(None, &set(&["People"]), NOW, false), Step::Keep);
        assert_eq!(reconcile(None, &set(&["Work"]), NOW, true), Step::Keep);
        // A tag made or corrected here goes there, whatever the server has.
        assert_eq!(reconcile(Some(&tag(Category::People, false, false)), &set(&["Notifications"]), NOW, true), Step::Push);
        assert_eq!(reconcile(Some(&tag(Category::People, false, false)), &set(&[]), NOW, true), Step::Push);
    }

    #[test]
    fn follows_the_server_once_synced() {
        let synced = tag(Category::People, false, true);
        assert_eq!(reconcile(Some(&synced), &set(&["People"]), NOW, true), Step::Keep);
        // Taken off there altogether: left here, never put back.
        assert_eq!(reconcile(Some(&synced), &set(&["Work"]), NOW, true), Step::Keep);
        // Changed there: the server wins, topic labels stay.
        assert_eq!(
            reconcile(Some(&synced), &set(&["Notifications", WAITING]), NOW, true),
            Step::Adopt(ConversationTag {
                category: Category::Notification,
                to_respond: true,
                labels: vec!["Finance".into()],
                waiting_until: None,
                synced: true,
            })
        );
        // Resolved on another device.
        let mut waiting = tag(Category::People, true, true);
        waiting.waiting_until = Some(NOW + 3600.0);
        assert!(matches!(
            reconcile(Some(&waiting), &set(&["People"]), NOW, true),
            Step::Adopt(ConversationTag { to_respond: false, waiting_until: None, .. })
        ));
        // Still waiting there: the deadline only this device knows stays.
        assert_eq!(reconcile(Some(&waiting), &set(&["People", WAITING]), NOW, true), Step::Keep);
        // Lapsed here: Waiting on you comes off the server.
        waiting.waiting_until = Some(NOW - 1.0);
        assert_eq!(reconcile(Some(&waiting), &set(&["People", WAITING]), NOW, true), Step::Push);
        assert_eq!(reconcile(Some(&waiting), &set(&["People"]), NOW, true), Step::Keep);
        // Two categories (two devices at once): ours goes back alone.
        assert_eq!(reconcile(Some(&synced), &set(&["People", "Newsletters"]), NOW, true), Step::Push);
    }

    #[test]
    fn changes_only_the_names_sync_owns() {
        let current = set(&["notifications", "Work", WAITING]);
        assert_eq!(
            change(&current, &["People"]),
            (vec!["People".to_string()], vec![WAITING.to_string(), "notifications".to_string()])
        );
        assert_eq!(change(&current, &["Notifications", WAITING]), (vec![], vec![]));
        let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert_eq!(
            retagged(&names(&["Red category", "newsletters"]), &names(&["People", "red CATEGORY"]), &names(&["Newsletters"])),
            names(&["Red category", "People"])
        );
    }
}
