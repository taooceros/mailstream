//! `contacts.search`: the people recipient fields complete to, as the Swift engine ranks them
//! (`MailStore.contacts()`, `Contact` and `RecipientCompletion` in `Store/Replies.swift` and
//! `UI/RecipientCompletion.swift`): everyone in the loaded listings and in the accounts' contact
//! lists, best recipient first.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use super::Mail;
use crate::commands::Arguments;
use crate::model::{Address, DISTANT_PAST, Date, Envelope};

/// How long `contacts.search` reuses the people it loaded; typing asks once per keystroke.
const CONTACTS_AGE: Duration = Duration::from_secs(60);
/// Most people a search offers.
const SUGGESTIONS: usize = 12;

type Load = Shared<BoxFuture<'static, Arc<Vec<Contact>>>>;

/// The people loaded for searches, shared by the searches that arrive while they load.
#[derive(Default)]
pub(super) struct ContactsLoad {
    load: Mutex<Option<(Instant, Load)>>,
}

/// Someone recipient fields complete to.
#[derive(Clone, Debug)]
pub(crate) struct Contact {
    pub address: Address,
    /// Folded name and its words, address and the parts of its local part: typing matches their start.
    keys: Vec<String>,
}

impl Contact {
    fn new(address: Address) -> Self {
        let email = fold(&address.email);
        let mut keys = vec![email.clone()];
        let local = email.split('@').find(|part| !part.is_empty()).unwrap_or("");
        keys.extend(local.split(['.', '-', '_', '+']).filter(|part| !part.is_empty()).map(String::from));
        let display = address.display_name();
        if display != address.email {
            let name = fold(&display);
            let words: Vec<String> =
                name.split(|c: char| !c.is_alphanumeric()).filter(|part| !part.is_empty()).map(String::from).collect();
            keys.push(name);
            keys.extend(words);
        }
        Self { address, keys }
    }

    /// Whether typing `needle`, already folded, offers this contact.
    fn matches(&self, needle: &str) -> bool {
        self.keys.iter().any(|key| key.starts_with(needle))
    }
}

/// `no-reply@…`, `notifications@…`: senders nobody writes to.
pub(crate) fn is_no_reply(email: &str) -> bool {
    let lowered = email.to_lowercase();
    let local = lowered.split('@').find(|part| !part.is_empty()).unwrap_or("");
    let squashed: String = local.chars().filter(|c| c.is_alphabetic()).collect();
    squashed.starts_with("noreply")
        || squashed.starts_with("donotreply")
        || ["mailerdaemon", "notification", "notifications", "bounce", "bounces"].contains(&squashed.as_str())
}

/// The case- and accent-insensitive form completions compare.
pub(crate) fn fold(text: &str) -> String {
    let stripped: String = text.nfd().filter(|c| !is_combining_mark(*c)).collect();
    stripped.to_lowercase().replace('ß', "ss")
}

/// A contact list's person: their name and lowercased addresses.
pub(crate) type ListedPerson = (Option<String>, Vec<String>);

/// Everyone in `listings` and `people`, best recipient first: people the user writes to, then the
/// contact lists' people, then the rest by how recently they appeared; the user's own addresses
/// last, and no-reply senders the lists don't name left out. A contact list's name wins over a
/// header's.
pub(crate) fn rank(listings: &[(String, Vec<Arc<Envelope>>)], people: &[ListedPerson], own: &HashSet<String>) -> Vec<Contact> {
    struct Entry {
        address: Address,
        sent: usize,
        latest: Date,
        listed: bool,
    }
    let mut seen_messages: HashSet<(&str, &str)> = HashSet::new();
    let mut by_email: HashMap<String, Entry> = HashMap::new();
    let mut note = |address: &Address, date: Option<Date>, sent: bool| {
        let key = address.normalized_email();
        if !key.contains('@') {
            return;
        }
        let entry = by_email.entry(key).or_insert_with(|| Entry {
            address: Address::new(None, &address.email),
            sent: 0,
            latest: DISTANT_PAST,
            listed: false,
        });
        let date = date.unwrap_or(DISTANT_PAST);
        // The newest name a header gave them.
        if let Some(name) = address.name.as_deref().filter(|name| !name.is_empty() && name.to_lowercase() != address.email.to_lowercase())
            && (entry.address.name.is_none() || date >= entry.latest)
        {
            entry.address.name = Some(name.to_string());
        }
        if sent {
            entry.sent += 1;
        }
        entry.latest = entry.latest.max(date);
    };
    for (account, envelopes) in listings {
        // A message filed under several labels is listed once per label.
        for envelope in envelopes {
            if !seen_messages.insert((account.as_str(), envelope.id.as_str())) {
                continue;
            }
            let from_me = envelope.from.iter().any(|address| own.contains(&address.normalized_email()));
            for address in &envelope.from {
                note(address, envelope.date, false);
            }
            for address in &envelope.to {
                note(address, envelope.date, from_me);
            }
        }
    }
    for (name, emails) in people {
        let name = name.as_deref().map(|name| name.trim_matches(|c: char| c == ' ' || c == '\t'));
        for email in emails {
            let address = Address::new(None, email);
            let key = address.normalized_email();
            if !key.contains('@') {
                continue;
            }
            let entry =
                by_email.entry(key).or_insert_with(|| Entry { address, sent: 0, latest: DISTANT_PAST, listed: false });
            entry.listed = true;
            if let Some(name) = name.filter(|name| !name.is_empty() && name.to_lowercase() != email.to_lowercase()) {
                entry.address.name = Some(name.to_string());
            }
        }
    }
    let mut entries: Vec<Entry> =
        by_email.into_values().filter(|entry| entry.sent > 0 || entry.listed || !is_no_reply(&entry.address.email)).collect();
    entries.sort_by(|a, b| {
        let (a_own, b_own) = (own.contains(&a.address.normalized_email()), own.contains(&b.address.normalized_email()));
        a_own
            .cmp(&b_own)
            .then(b.sent.cmp(&a.sent))
            .then(b.listed.cmp(&a.listed))
            .then(b.latest.total_cmp(&a.latest))
            .then(a.address.email.cmp(&b.address.email))
    });
    entries.into_iter().map(|entry| Contact::new(entry.address)).collect()
}

/// People whose name, a word of it, or address starts with `typed`, best first; an address typed
/// out in full first of all.
pub(crate) fn suggestions<'a>(contacts: &'a [Contact], typed: &str) -> Vec<&'a Contact> {
    let needle = fold(typed.trim_matches(|c: char| c == ' ' || c == '\t'));
    if needle.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<&Contact> = contacts.iter().filter(|contact| contact.matches(&needle)).take(SUGGESTIONS).collect();
    if let Some(exact) = matches.iter().position(|contact| fold(&contact.address.email) == needle) {
        let found = matches.remove(exact);
        matches.insert(0, found);
    }
    matches
}

impl Mail {
    /// People `query` completes to; `people` lists the accounts' contact lists, asked when the
    /// contacts are loaded again.
    pub(crate) async fn search_contacts(
        self: &Arc<Self>,
        args: &Arguments,
        people: impl Future<Output = Vec<ListedPerson>> + Send + 'static,
    ) -> Value {
        let query = args.string("query").unwrap_or_default();
        if query.trim_matches(|c: char| c == ' ' || c == '\t').is_empty() {
            return json!([]);
        }
        let limit = args.int("limit").unwrap_or(10).clamp(1, SUGGESTIONS as i64) as usize;
        let contacts = self.contacts(people).await;
        let found: Vec<Value> = suggestions(&contacts, &query)
            .into_iter()
            .take(limit)
            .map(|contact| {
                let display = contact.address.display_name();
                let name = (display != contact.address.email).then_some(display);
                json!({ "name": name, "email": contact.address.email })
            })
            .collect();
        Value::Array(found)
    }

    /// Everyone compose completes to, best recipient first, with whether each is one of the
    /// user's own addresses: what Mailstream.app's recipient fields match against as typing goes.
    pub(crate) async fn all_contacts(
        self: &Arc<Self>,
        people: impl Future<Output = Vec<ListedPerson>> + Send + 'static,
    ) -> Value {
        let contacts = self.contacts(people).await;
        let own = Self::own_addresses(&self.state());
        Value::Array(
            contacts
                .iter()
                .map(|contact| {
                    json!({ "address": contact.address, "own": own.contains(&contact.address.normalized_email()) })
                })
                .collect(),
        )
    }

    /// Everyone compose completes to, loaded once a minute and shared by searches that arrive
    /// while it loads.
    async fn contacts(self: &Arc<Self>, people: impl Future<Output = Vec<ListedPerson>> + Send + 'static) -> Arc<Vec<Contact>> {
        let load = {
            let mut current = self.contacts.load.lock().expect("contacts lock");
            match &*current {
                Some((started, load)) if started.elapsed() < CONTACTS_AGE => load.clone(),
                _ => {
                    let (listings, own) = {
                        let state = self.state();
                        let listings: Vec<(String, Vec<Arc<Envelope>>)> =
                            state.listings.iter().map(|(key, listing)| (key.account.clone(), listing.envelopes.clone())).collect();
                        (listings, Self::own_addresses(&state))
                    };
                    let load: Load = async move { Arc::new(rank(&listings, &people.await, &own)) }.boxed().shared();
                    *current = Some((Instant::now(), load.clone()));
                    load
                }
            }
        };
        load.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Envelope;

    fn envelope(id: &str, from: Address, to: Vec<Address>, date: f64) -> Arc<Envelope> {
        Arc::new(Envelope {
            id: id.into(),
            thread: None,
            message_id: None,
            in_reply_to: vec![],
            flags: Default::default(),
            subject: String::new(),
            from: vec![from],
            to,
            date: Some(date),
            size: 0,
            has_attachment: None,
        })
    }

    fn address(name: Option<&str>, email: &str) -> Address {
        Address::new(name.map(String::from), email)
    }

    #[test]
    fn ranks_people_the_user_writes_to_first() {
        let me = address(Some("Me"), "me@x.com");
        let listings = vec![(
            "me@x.com".to_string(),
            vec![
                envelope("1", address(Some("Émile Zola"), "emile@x.com"), vec![me.clone()], 10.0),
                envelope("2", me.clone(), vec![address(Some("Bea Bauer"), "bea.bauer@x.com")], 5.0),
                envelope("2", me.clone(), vec![address(Some("Bea Bauer"), "bea.bauer@x.com")], 5.0),
                envelope("3", address(None, "no-reply@shop.com"), vec![me.clone()], 20.0),
                envelope("4", address(Some("Emma"), "emma@x.com"), vec![me.clone()], 30.0),
            ],
        )];
        let people: Vec<ListedPerson> = vec![(Some(" Dora Directory ".into()), vec!["dora@corp.com".into()])];
        let own: HashSet<String> = ["me@x.com".to_string()].into();
        let ranked = rank(&listings, &people, &own);
        let emails: Vec<&str> = ranked.iter().map(|contact| contact.address.email.as_str()).collect();
        // Written to (once: the repeated listing counts once), listed, by recency, own last.
        assert_eq!(emails, ["bea.bauer@x.com", "dora@corp.com", "emma@x.com", "emile@x.com", "me@x.com"]);
        assert_eq!(ranked[1].address.name.as_deref(), Some("Dora Directory"));
        let found: Vec<&str> = suggestions(&ranked, "em").iter().map(|contact| contact.address.email.as_str()).collect();
        assert_eq!(found, ["emma@x.com", "emile@x.com"]);
        // Accents fold; words of the name and parts of the address match; a full address goes first.
        assert_eq!(suggestions(&ranked, "zol")[0].address.email, "emile@x.com");
        assert_eq!(suggestions(&ranked, "bauer")[0].address.email, "bea.bauer@x.com");
        assert_eq!(suggestions(&ranked, "EMILE@X.COM")[0].address.email, "emile@x.com");
        assert!(suggestions(&ranked, "  ").is_empty());
        assert!(is_no_reply("Do-Not-Reply@x.com") && is_no_reply("bounces@x.com") && !is_no_reply("bounce-team@x.com"));
        assert_eq!(fold("Straße ÅNGSTRÖM İ"), "strasse angstrom i");
    }
}
