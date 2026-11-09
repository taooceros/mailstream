//! Putting AI tags on the server for the tagger (`crate::tagsync`, MS-172): each conversation's
//! names go onto every message of it but drafts (Gmail labels none), and the other names sync
//! owns come off. Messages needing the same change on one account share one request. The
//! listings show the change at once; a refusal rolls it back and is reported, as flag changes are.
use std::collections::HashSet;
use std::sync::Arc;

use super::Mail;
use crate::backend::Backend;
use crate::model::{Envelope, MessageRef};
use crate::tagsync::{self, Push};

impl Envelope {
    fn retag(&mut self, add: &[String], remove: &[String]) {
        let before: Vec<String> = self.tags.iter().cloned().collect();
        self.tags = tagsync::retagged(&before, add, remove).into_iter().collect();
    }
}

/// One request: the same names added to and taken off messages of one account.
struct Change {
    account: String,
    add: Vec<String>,
    remove: Vec<String>,
    ids: Vec<String>,
    refs: HashSet<MessageRef>,
    /// The conversations (`Push::key`) it serves.
    keys: HashSet<String>,
}

impl Mail {
    /// Puts each push's names on its conversation; answers the keys whose every change landed
    /// (those that needed none included).
    pub(super) async fn push_tags(self: &Arc<Self>, pushes: Vec<Push>) -> HashSet<String> {
        let mut changes: Vec<Change> = Vec::new();
        for push in &pushes {
            for message in &push.conversation.messages {
                if message.envelope.is_draft() {
                    continue;
                }
                let (add, remove) = tagsync::change(&message.envelope.tags, &push.names);
                if add.is_empty() && remove.is_empty() {
                    continue;
                }
                let account = message.account();
                let index = match changes.iter().position(|change| change.account == account && change.add == add && change.remove == remove) {
                    Some(index) => index,
                    None => {
                        changes.push(Change {
                            account: account.to_string(),
                            add,
                            remove,
                            ids: Vec::new(),
                            refs: HashSet::new(),
                            keys: HashSet::new(),
                        });
                        changes.len() - 1
                    }
                };
                let change = &mut changes[index];
                if !change.ids.iter().any(|id| id == message.id()) {
                    change.ids.push(message.id().to_string());
                }
                change.refs.insert(message.message_ref());
                change.keys.insert(push.key.clone());
            }
        }
        let mut failed: HashSet<String> = HashSet::new();
        for change in &changes {
            self.update_envelopes(&change.refs, |envelope| envelope.retag(&change.add, &change.remove));
            let result = self
                .provider_request(
                    change.account.clone(),
                    false,
                    async |backend: Arc<Backend>| backend.set_tags(&change.ids, &change.add, &change.remove).await,
                    || self.update_envelopes(&change.refs, |envelope| envelope.retag(&change.remove, &change.add)),
                )
                .await;
            if result.is_err() {
                failed.extend(change.keys.iter().cloned());
            }
        }
        pushes.into_iter().map(|push| push.key).filter(|key| !failed.contains(key)).collect()
    }
}
