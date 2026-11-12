//! The contact directory: every address the photo accounts' contact lists name with where its
//! photo is, plus everyone listed for address completion. Read from Google People (saved
//! contacts, other contacts and, on Workspace accounts, the domain directory) and Outlook
//! contacts, and saved as `avatars/directory.json` in the Swift engine's format.
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Provider;
use crate::api::{self, Api, ApiError, GooglePage, GraphPage, Priority};

/// Where a contact's photo is; encoded as Swift encodes the enum (`{"url":{"_0":…}}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PhotoSource {
    /// A Google photo, served at any size.
    Url {
        #[serde(rename = "_0")]
        url: String,
    },
    /// An Outlook contact; whether it has a photo is only known when it is fetched.
    GraphContact { account: String, id: String },
}

/// Someone the contact directory lists (a saved contact, an other contact, a directory
/// profile), whom recipient fields complete to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryPerson {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    /// Lowercased.
    pub emails: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Directory {
    /// Seconds since 2001-01-01, as Swift's `JSONEncoder` writes a `Date`.
    pub built: f64,
    /// The accounts it asked, whether they answered or refused; any difference from the current
    /// photo accounts (one added, removed, or unreachable last time) rebuilds it at once.
    pub accounts: Vec<String>,
    pub photos: BTreeMap<String, PhotoSource>,
    /// Everyone the lists named, photo or not; nil in a directory saved before it kept them,
    /// which is rebuilt at once.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub people: Option<Vec<DirectoryPerson>>,
    /// A digest of `photos`: changes whenever any contact photo does, retiring cached misses.
    pub stamp: String,
}

/// Seconds between 1970 and 2001, Foundation's reference date.
const REFERENCE_DATE: f64 = 978_307_200.0;

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64()) - REFERENCE_DATE
}

impl Directory {
    pub fn new(accounts: Vec<String>, photos: BTreeMap<String, PhotoSource>, people: Vec<DirectoryPerson>) -> Self {
        // Swift's `JSONEncoder` with sorted keys: compact, slashes escaped.
        let encoded = crate::json::encode(&serde_json::to_value(&photos).unwrap_or_default(), false, true);
        let stamp = super::hex_sha256(&encoded);
        Self { built: now(), accounts, photos, people: Some(people), stamp }
    }

    pub fn read(path: &std::path::Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn save(&self, path: &std::path::Path) {
        let data = crate::json::encode(self, false, true);
        if let Some(directory) = path.parent() {
            let _ = crate::files::replace(directory, path, &data);
        }
    }

    pub fn asked(&self) -> HashSet<&str> {
        self.accounts.iter().map(String::as_str).collect()
    }
}

/// An account whose contacts give sender photos.
#[derive(Clone)]
pub struct PhotoAccount {
    pub name: String,
    pub provider: Provider,
    /// Also read the Workspace domain's directory (Google).
    pub directory: bool,
    pub api: Arc<Api>,
}

pub const GOOGLE_CONTACTS: &str = "https://www.googleapis.com/auth/contacts.readonly";
pub const GOOGLE_DIRECTORY: &str = "https://www.googleapis.com/auth/directory.readonly";
pub const GRAPH_CONTACTS: &str = "https://graph.microsoft.com/Contacts.Read";

/// Contact photos and people found in one account.
pub type Found = (BTreeMap<String, PhotoSource>, Vec<DirectoryPerson>);

/// `request`'s answer; nil when the provider refused it (no permission, no such list), failed
/// when it could not be asked (offline, no token yet, throttled, server error).
async fn unless_refused<Page>(request: impl Future<Output = Result<Page, ApiError>>) -> Result<Option<Page>, ApiError> {
    match request.await {
        Ok(page) => Ok(Some(page)),
        Err(error) if (400..500).contains(&error.status) && error.status != 429 => Ok(None),
        Err(error) => Err(error),
    }
}

/// Saved contacts, "other contacts" (people the user has written with) and, on Workspace
/// accounts, the organization's directory: their photos and everyone named. Nil when every one
/// of them is refused; failed when the account could not be asked.
pub async fn google(api: &Api, people_root: &str, directory: bool) -> Result<Option<Found>, ApiError> {
    const FIELDS: &str = "names,emailAddresses,photos";
    let mut lists: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("people/me/connections", vec![("personFields", FIELDS)]),
        ("otherContacts", vec![("readMask", FIELDS)]),
    ];
    if directory {
        lists.push((
            "people:listDirectoryPeople",
            vec![("readMask", FIELDS), ("sources", "DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE")],
        ));
    }
    let mut found: Vec<Person> = Vec::new();
    let mut answered = false;
    for (path, mut query) in lists {
        query.push(("pageSize", "1000"));
        let first = api::url(people_root, path, &query);
        let listed = api::pages(first, 30, |url| async move {
            unless_refused(api.get::<GooglePage<Person>>(&url, Priority::Background, 0)).await
        })
        .await?;
        let Some(listed) = listed else { continue };
        answered = true;
        found.extend(listed);
    }
    if !answered {
        return Ok(None);
    }
    // The first list naming an address wins.
    let mut photos = BTreeMap::new();
    for person in &found {
        let Some(link) = person.photos.iter().flatten().find(|photo| photo.default != Some(true)).and_then(|photo| photo.url.clone())
        else {
            continue;
        };
        for email in person.emails() {
            photos.entry(email).or_insert_with(|| PhotoSource::Url { url: link.clone() });
        }
    }
    let people = found
        .iter()
        .filter_map(|person| {
            let emails = person.emails();
            if emails.is_empty() {
                return None;
            }
            let name = person
                .names
                .iter()
                .flatten()
                .filter_map(|name| name.display_name.clone())
                .find(|name| !name.is_empty());
            Some(DirectoryPerson { name, emails })
        })
        .collect();
    Ok(Some((photos, people)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Person {
    photos: Option<Vec<Photo>>,
    email_addresses: Option<Vec<Email>>,
    names: Option<Vec<Name>>,
}

impl Person {
    fn emails(&self) -> Vec<String> {
        self.email_addresses.iter().flatten().filter_map(|email| email.value.as_ref().map(|value| value.to_lowercase())).collect()
    }
}

#[derive(Deserialize)]
struct Photo {
    url: Option<String>,
    default: Option<bool>,
}

#[derive(Deserialize)]
struct Email {
    value: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Name {
    display_name: Option<String>,
}

/// Outlook contacts: their photos and everyone named. Nil when the request is refused; failed
/// when the account could not be asked.
pub async fn graph(api: &Api, graph_root: &str) -> Result<Option<Found>, ApiError> {
    let first = api::url(graph_root, "me/contacts", &[("$select", "displayName,emailAddresses"), ("$top", "999")]);
    let found = api::pages(
        first,
        30,
        |url| async move { unless_refused(api.get::<GraphPage<GraphContact>>(&url, Priority::Background, 0)).await },
    )
    .await?;
    let Some(found) = found else { return Ok(None) };
    let mut photos = BTreeMap::new();
    let mut people = Vec::new();
    for contact in found {
        let emails: Vec<String> =
            contact.email_addresses.iter().flatten().filter_map(|email| email.address.as_ref().map(|value| value.to_lowercase())).collect();
        if emails.is_empty() {
            continue;
        }
        for email in &emails {
            photos
                .entry(email.clone())
                .or_insert_with(|| PhotoSource::GraphContact { account: api.account.clone(), id: contact.id.clone() });
        }
        let name = contact.display_name.filter(|name| !name.is_empty()).or_else(|| {
            contact.email_addresses.iter().flatten().filter_map(|email| email.name.clone()).find(|name| !name.is_empty())
        });
        people.push(DirectoryPerson { name, emails });
    }
    Ok(Some((photos, people)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphContact {
    id: String,
    display_name: Option<String>,
    email_addresses: Option<Vec<GraphEmail>>,
}

#[derive(Deserialize)]
struct GraphEmail {
    name: Option<String>,
    address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_stamps_like_the_swift_engine() {
        // What Swift's `JSONEncoder` (sorted keys) writes for these photos.
        let swift = r#"{"B@x.com":{"url":{"_0":"https:\/\/x\/%C3%A9"}},"a10@x.com":{"url":{"_0":"https:\/\/lh3.googleusercontent.com\/a\/b=s100"}},"a9@x.com":{"graphContact":{"account":"me@o.com","id":"AQ=="}},"a_b@x":{"url":{"_0":"https:\/\/y"}}}"#;
        let photos: BTreeMap<String, PhotoSource> = serde_json::from_str(swift).unwrap();
        assert_eq!(photos["a9@x.com"], PhotoSource::GraphContact { account: "me@o.com".into(), id: "AQ==".into() });
        let directory = Directory::new(vec!["me@o.com".into()], photos, Vec::new());
        assert_eq!(directory.stamp, super::super::hex_sha256(swift.as_bytes()));
        // A directory Swift saved (no `people` yet) reads, keeping its stamp.
        let saved = r#"{"built":784000000.5,"accounts":["me@o.com"],"photos":{},"stamp":"abc"}"#;
        let directory: Directory = serde_json::from_str(saved).unwrap();
        assert_eq!((directory.built, directory.people.is_none(), directory.stamp.as_str()), (784000000.5, true, "abc"));
    }
}
