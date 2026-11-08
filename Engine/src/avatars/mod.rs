//! Sender pictures, found and cached as the Swift engine's `AvatarResolver`
//! (`Sources/MailstreamKit/Store/Avatars.swift`) does, in the same files, so either engine uses
//! what the other cached.
//!
//! Sources, best first: the photo of a Google or Outlook contact (accounts granted contacts
//! access), the picture a person publishes themselves (Google Chat's `avatarUrl`, handed in
//! through [`Avatars::publish`]), the sender domain's BIMI logo, Gravatar, and for automated mail
//! the domain's site icon. Each becomes one 96 px PNG in `<caches>/avatars`. A cached picture
//! answers before anything else is read or asked; one older than 14 days still answers while it
//! is looked up again in the background. A miss holds 3 days, or until the contact directory
//! changes.
mod directory;
mod picture;
pub mod web;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

pub use self::directory::DirectoryPerson;
use self::directory::{Directory, PhotoAccount, PhotoSource};
use self::picture::Style;
use self::web::Web;
use crate::api::{self, Api, Priority, Request};
use crate::commands::Arguments;
use crate::{EngineError, Provider};

/// The row size: rows and the reader draw at most 34 pt, so 96 px covers Retina.
const SIDE: u32 = 96;
/// Age after which a cached picture is looked up again, behind the one shown.
const HIT: Duration = Duration::from_secs(14 * 86_400);
const MISS: Duration = Duration::from_secs(3 * 86_400);
const DIRECTORY_AGE: f64 = 86_400.0;
/// Wait after a directory refresh that could not ask every account before trying again.
const DIRECTORY_RETRY: Duration = Duration::from_secs(300);
/// The largest picture fetched; BIMI logos are limited to 128 KB.
const MAX_PICTURE: usize = 2 * 1024 * 1024;
const MAX_LOGO: usize = 128 * 1024;
/// How much of a home page is searched for icon links.
const MAX_PAGE: usize = 512 * 1024;

/// Where the contact lists are read.
#[derive(Clone)]
pub struct Roots {
    /// Google People: `https://people.googleapis.com/v1/`.
    pub people: String,
    /// Microsoft Graph: `https://graph.microsoft.com/v1.0/`.
    pub graph: String,
}

type Png = Option<Vec<u8>>;

/// The accounts whose contacts give photos, as they are now.
type PhotoAccounts = Box<dyn Fn() -> Vec<PhotoAccount> + Send + Sync>;

pub struct Avatars {
    root: PathBuf,
    web: Arc<dyn Web>,
    roots: Roots,
    photo_accounts: PhotoAccounts,
    /// Lookups at once.
    permits: Semaphore,
    /// Address or Chat user id (`users/…`) → the picture that person publishes.
    published: Mutex<HashMap<String, String>>,
    contacts: Mutex<Contacts>,
    /// Pictures past their age being looked up again.
    revalidating: Mutex<HashSet<String>>,
    /// Domain-wide lookups in flight; finished ones live in the disk cache.
    domains: Mutex<HashMap<String, Shared<BoxFuture<'static, Png>>>>,
}

/// The contact directory lookups use; an old one, or one built for other accounts, keeps
/// answering while a refresh replaces it.
#[derive(Default)]
struct Contacts {
    accounts: Vec<PhotoAccount>,
    directory: Option<Arc<Directory>>,
    /// Reading the saved directory, shared by the lookups that start before it is in.
    saved: Option<Shared<BoxFuture<'static, Option<Arc<Directory>>>>>,
    refresh: Option<Shared<BoxFuture<'static, Arc<Directory>>>>,
    /// Bumped when the photo accounts change, so a refresh against the old ones is dropped.
    generation: u64,
    /// When a refresh last failed to ask every account.
    last_incomplete: Option<Instant>,
}

/// A picture, whether younger than `HIT`; or a miss younger than `MISS` with the stamp it was
/// made against.
enum Cached {
    Hit { fresh: bool },
    Miss(String),
    Unknown,
}

impl Avatars {
    /// `root` is `<caches>/avatars`; `photo_accounts` names the accounts whose contacts give
    /// photos, read whenever the directory is consulted.
    pub(crate) fn new(
        root: PathBuf,
        web: Arc<dyn Web>,
        roots: Roots,
        photo_accounts: impl Fn() -> Vec<PhotoAccount> + Send + Sync + 'static,
    ) -> Self {
        let _ = std::fs::create_dir_all(&root);
        Self {
            root,
            web,
            roots,
            photo_accounts: Box::new(photo_accounts),
            permits: Semaphore::new(6),
            published: Mutex::default(),
            contacts: Mutex::default(),
            revalidating: Mutex::default(),
            domains: Mutex::default(),
        }
    }

    /// `mail.avatars`: each sender's picture file, looked up first when not cached; null when
    /// there is none. `automated` names the addresses a site icon may stand in for.
    pub(crate) async fn command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let mut addresses: Vec<String> = args
            .list("addresses")
            .unwrap_or_default()
            .into_iter()
            .map(|address| address.to_lowercase())
            .filter(|address| address.contains('@'))
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        if addresses.is_empty() {
            return Err(EngineError::usage("Give at least one address."));
        }
        let automated: HashSet<String> =
            args.list("automated").unwrap_or_default().into_iter().map(|address| address.to_lowercase()).collect();
        let files = futures::future::join_all(
            addresses.iter().map(|address| self.picture_file(address, automated.contains(address))),
        )
        .await;
        let out: Map<String, Value> = addresses
            .into_iter()
            .zip(files)
            .map(|(address, file)| (address, file.map_or(Value::Null, |file| Value::String(file_url(&file)))))
            .collect();
        Ok(Value::Object(out))
    }

    /// Makes `url`, the picture a person publishes (Google Chat's `avatarUrl`), a source for
    /// `key`: their address, or their Chat user id when there is none.
    pub fn publish(&self, url: &str, key: &str) {
        let key = key.to_lowercase();
        if !key.is_empty() {
            lock(&self.published).insert(key, url.to_string());
        }
    }

    /// The picture `key` publishes, if any.
    #[cfg(test)]
    pub(crate) fn published(&self, key: &str) -> Option<String> {
        lock(&self.published).get(key).cloned()
    }

    /// Everyone the accounts' contact lists name, for completing addresses, without waiting on
    /// the providers: with no directory in hand or saved yet, a refresh starts and this lists no
    /// one.
    pub async fn people(self: &Arc<Self>) -> Vec<DirectoryPerson> {
        if let Some(directory) = self.current().await {
            return directory.people.clone().unwrap_or_default();
        }
        let mut contacts = lock(&self.contacts);
        if contacts.refresh.is_none() {
            // The refresh runs on its own; this lookup does not wait for it.
            drop(self.start_refresh(&mut contacts));
        }
        Vec::new()
    }

    /// The file holding `key`'s picture, looked up first if it is not cached; nil when there is
    /// none. `key` is a lowercased address, or a Chat user id when a published picture is the
    /// only source. The cache entry is per published picture: one found without it may be a
    /// lower source, such as a domain logo.
    pub async fn picture_file(self: &Arc<Self>, key: &str, automated: bool) -> Option<PathBuf> {
        let published = lock(&self.published).get(key).cloned();
        let automated = automated || looks_automated(key);
        let cache_key = cache_key(key, published.as_deref());
        let file = self.root.join(format!("{cache_key}.png"));
        let miss_stamp = match self.cached(&cache_key) {
            Cached::Hit { fresh } => {
                if !fresh {
                    self.revalidate(key, published, automated, cache_key);
                }
                return Some(file);
            }
            Cached::Miss(stamp) => Some(stamp),
            Cached::Unknown => None,
        };
        let directory = self.contacts().await;
        if miss_stamp == Some(stamp(&directory, published.as_deref())) {
            return None;
        }
        self.look_up(key, published.as_deref(), automated, &cache_key, &directory, false).await.map(|_| file)
    }

    /// Looks `key` up and caches what it finds at `cache_key`. A miss is cached only when
    /// nothing was there: a picture being replaced stays rather than give way to a failed fetch.
    async fn look_up(
        self: &Arc<Self>,
        key: &str,
        published: Option<&str>,
        automated: bool,
        cache_key: &str,
        directory: &Directory,
        replacing: bool,
    ) -> Png {
        let png = {
            // Closed only when the engine stops: nothing was looked up, so no miss is cached.
            let _permit = self.permits.acquire().await.ok()?;
            self.find(key, published, automated, directory).await
        };
        if png.is_some() || !replacing {
            self.store(cache_key, png.as_deref(), Some(&stamp(directory, published)));
        }
        png
    }

    /// Looks up again a picture past its age, which keeps answering meanwhile.
    fn revalidate(self: &Arc<Self>, key: &str, published: Option<String>, automated: bool, cache_key: String) {
        if !lock(&self.revalidating).insert(cache_key.clone()) {
            return;
        }
        let (avatars, key) = (self.clone(), key.to_string());
        tokio::spawn(async move {
            let directory = avatars.contacts().await;
            avatars.look_up(&key, published.as_deref(), automated, &cache_key, &directory, true).await;
            lock(&avatars.revalidating).remove(&cache_key);
        });
    }

    async fn find(self: &Arc<Self>, key: &str, published: Option<&str>, automated: bool, directory: &Directory) -> Png {
        if let Some(source) = directory.photos.get(key)
            && let Some(png) = self.contact_photo(source).await
        {
            return Some(png);
        }
        if let Some(published) = published
            && let Some(fetched) = self.fetch(&sized(published), MAX_PICTURE).await
            && let Some(png) = normalize(fetched, Style::Logo).await
        {
            return Some(png);
        }
        if !key.contains('@') {
            return None;
        }
        let domain = key.split('@').rfind(|part| !part.is_empty()).unwrap_or_default();
        let company = !domain.is_empty() && !FREEMAIL.contains(&domain);
        if automated
            && company
            && let Some(png) = self.domain_logo(domain).await
        {
            return Some(png);
        }
        if let Some(png) = self.gravatar(key).await {
            return Some(png);
        }
        if !automated && company {
            return self.bimi(domain).await;
        }
        None
    }

    // MARK: - Disk cache

    fn cached(&self, key: &str) -> Cached {
        let age = |path: &Path| {
            let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()?;
            Some(SystemTime::now().duration_since(modified).unwrap_or_default())
        };
        let png = self.root.join(format!("{key}.png"));
        if png.is_file() {
            return Cached::Hit { fresh: age(&png).is_some_and(|age| age < HIT) };
        }
        let none = self.root.join(format!("{key}.none"));
        if age(&none).is_some_and(|age| age < MISS) {
            return Cached::Miss(std::fs::read_to_string(&none).unwrap_or_default());
        }
        Cached::Unknown
    }

    /// `stamp` ties a miss to the directory it was made against; nil for domain-wide results.
    fn store(&self, key: &str, png: Option<&[u8]>, stamp: Option<&str>) {
        let png_file = self.root.join(format!("{key}.png"));
        let none_file = self.root.join(format!("{key}.none"));
        let (write, data, remove) = match png {
            Some(png) => (png_file, png, none_file),
            None => (none_file, stamp.unwrap_or_default().as_bytes(), png_file),
        };
        let _ = crate::files::replace(&self.root, &write, data);
        let _ = std::fs::remove_file(remove);
    }

    // MARK: - Contacts

    fn directory_file(&self) -> PathBuf {
        self.root.join("directory.json")
    }

    /// Takes the current photo accounts; the directory in hand keeps answering until one built
    /// for them replaces it.
    fn follow_accounts(&self, contacts: &mut Contacts) {
        let fresh = (self.photo_accounts)();
        let same = fresh.len() == contacts.accounts.len()
            && fresh.iter().zip(&contacts.accounts).all(|(a, b)| a.name == b.name && a.directory == b.directory);
        if same {
            return;
        }
        contacts.accounts = fresh;
        contacts.generation += 1;
        contacts.refresh = None;
        contacts.last_incomplete = None;
    }

    /// The contact directory: the one in hand, else the one saved on disk, whatever its age. One
    /// that is a day old or was built for other accounts answers while a refresh replaces it;
    /// only a lookup with nothing saved at all waits for the providers.
    async fn contacts(self: &Arc<Self>) -> Arc<Directory> {
        if let Some(directory) = self.current().await {
            return directory;
        }
        let refresh = {
            let mut contacts = lock(&self.contacts);
            match &contacts.refresh {
                Some(refresh) => refresh.clone(),
                None => self.start_refresh(&mut contacts),
            }
        };
        refresh.await
    }

    /// The directory in hand or saved, starting a refresh when it is due.
    async fn current(self: &Arc<Self>) -> Option<Arc<Directory>> {
        let saved = {
            let mut contacts = lock(&self.contacts);
            self.follow_accounts(&mut contacts);
            if contacts.directory.is_some() {
                None
            } else {
                let file = self.directory_file();
                let read = contacts.saved.get_or_insert_with(|| {
                    tokio::task::spawn_blocking(move || Directory::read(&file).map(Arc::new))
                        .map(|read| read.ok().flatten())
                        .boxed()
                        .shared()
                });
                Some(read.clone())
            }
        };
        if let Some(saved) = saved {
            let loaded = saved.await;
            let mut contacts = lock(&self.contacts);
            if contacts.directory.is_none() {
                contacts.directory = loaded;
            }
        }
        let mut contacts = lock(&self.contacts);
        let directory = contacts.directory.clone()?;
        if contacts.refresh.is_none() && needs_refresh(&contacts, &directory) {
            // The refresh runs on its own; this lookup does not wait for it.
            drop(self.start_refresh(&mut contacts));
        }
        Some(directory)
    }

    fn start_refresh(self: &Arc<Self>, contacts: &mut Contacts) -> Shared<BoxFuture<'static, Arc<Directory>>> {
        let (avatars, generation, accounts) = (self.clone(), contacts.generation, contacts.accounts.clone());
        let task = tokio::spawn(async move { avatars.refresh_directory(generation, accounts).await });
        let refresh = async move {
            task.await.unwrap_or_else(|_| Arc::new(Directory::new(Vec::new(), BTreeMap::new(), Vec::new())))
        }
        .boxed()
        .shared();
        contacts.refresh = Some(refresh.clone());
        refresh
    }

    /// Asks every photo account; a directory that reached them all replaces the one in hand and
    /// is saved, one that did not is used only when there is nothing else.
    async fn refresh_directory(&self, generation: u64, accounts: Vec<PhotoAccount>) -> Arc<Directory> {
        let fresh = Arc::new(self.fetch_directory(&accounts).await);
        let complete = {
            let mut contacts = lock(&self.contacts);
            // The accounts changed meanwhile: this refresh was dropped.
            if generation != contacts.generation {
                return fresh;
            }
            contacts.refresh = None;
            let complete = fresh.asked() == contacts.accounts.iter().map(|account| account.name.as_str()).collect();
            if complete {
                contacts.last_incomplete = None;
                contacts.directory = Some(fresh.clone());
            } else {
                contacts.last_incomplete = Some(Instant::now());
                if contacts.directory.is_none() {
                    contacts.directory = Some(fresh.clone());
                }
            }
            complete
        };
        if complete {
            let (saved, file) = (fresh.clone(), self.directory_file());
            let _ = tokio::task::spawn_blocking(move || saved.save(&file)).await;
        }
        fresh
    }

    async fn fetch_directory(&self, accounts: &[PhotoAccount]) -> Directory {
        let mut photos = BTreeMap::new();
        let mut people = Vec::new();
        let mut asked = Vec::new();
        for account in accounts {
            let found = match account.provider {
                Provider::Google => directory::google(&account.api, &self.roots.people, account.directory).await,
                Provider::Microsoft => directory::graph(&account.api, &self.roots.graph).await,
            };
            let Ok(found) = found else { continue };
            asked.push(account.name.clone());
            if let Some((found_photos, found_people)) = found {
                for (address, source) in found_photos {
                    photos.entry(address).or_insert(source);
                }
                people.extend(found_people);
            }
        }
        Directory::new(asked, photos, people)
    }

    async fn contact_photo(&self, source: &PhotoSource) -> Png {
        let fetched = match source {
            PhotoSource::Url { url } => self.fetch(&sized(url), MAX_PICTURE).await?,
            PhotoSource::GraphContact { account, id } => {
                let api = lock(&self.contacts).accounts.iter().find(|photo| &photo.name == account)?.api.clone();
                let url = api::url(&self.roots.graph, &format!("me/contacts/{id}/photo/$value"), &[]);
                let answer = api.send(Request::get(url), Priority::Background, None, 0).await.ok()?;
                Some(answer.body).filter(|body| body.len() <= MAX_PICTURE)?
            }
        };
        normalize(fetched, Style::Photo).await
    }

    // MARK: - Public sources

    async fn gravatar(&self, email: &str) -> Png {
        let fetched = self.fetch(&format!("https://gravatar.com/avatar/{}?s={SIDE}&d=404", hash(email)), MAX_PICTURE).await?;
        normalize(fetched, Style::Photo).await
    }

    /// The BIMI logo, else the site icon, shared by every sender at the domain.
    async fn domain_logo(self: &Arc<Self>, domain: &str) -> Png {
        if let Some(png) = self.bimi(domain).await {
            return Some(png);
        }
        let (avatars, organization) = (self.clone(), organization(domain));
        self.shared_per_domain("i-", domain, async move { avatars.site_icon(&organization).await }.boxed()).await
    }

    async fn bimi(self: &Arc<Self>, domain: &str) -> Png {
        let (avatars, name) = (self.clone(), domain.to_string());
        self.shared_per_domain("b-", domain, async move { avatars.look_up_bimi(&name).await }.boxed()).await
    }

    /// One lookup per domain and kind, in memory while it runs and on disk. A logo past its age
    /// is fetched again, and kept if that finds nothing.
    async fn shared_per_domain(self: &Arc<Self>, kind: &str, domain: &str, body: BoxFuture<'static, Png>) -> Png {
        let key = format!("{kind}{}", hash(domain));
        let task = lock(&self.domains)
            .entry(key.clone())
            .or_insert_with(|| {
                let (avatars, key) = (self.clone(), key.clone());
                async move {
                    let mut old = None;
                    match avatars.cached(&key) {
                        Cached::Hit { fresh } => {
                            old = std::fs::read(avatars.root.join(format!("{key}.png"))).ok();
                            if fresh && old.is_some() {
                                return old;
                            }
                        }
                        Cached::Miss(_) => return None,
                        Cached::Unknown => {}
                    }
                    let Some(png) = body.await else {
                        if old.is_none() {
                            avatars.store(&key, None, None);
                        }
                        return old;
                    };
                    avatars.store(&key, Some(&png), None);
                    Some(png)
                }
                .boxed()
                .shared()
            })
            .clone();
        let png = task.await;
        lock(&self.domains).remove(&key);
        png
    }

    /// `default._bimi.<domain>`, falling back to the organizational domain as the spec does.
    async fn look_up_bimi(&self, domain: &str) -> Png {
        let mut names = vec![domain.to_string()];
        let organization = organization(domain);
        if organization != domain {
            names.push(organization);
        }
        for name in names {
            let records = self.web.txt(format!("default._bimi.{name}")).await;
            let Some(record) = records.iter().find(|record| record.starts_with("v=BIMI1")) else { continue };
            let link = record.split(';').map(|part| part.trim_matches([' ', '\t'])).find_map(|part| part.strip_prefix("l="));
            let link = link.filter(|link| link.starts_with("https://") && reqwest::Url::parse(link).is_ok())?;
            let fetched = self.fetch(link, MAX_LOGO).await?;
            return normalize(fetched, Style::Logo).await;
        }
        None
    }

    /// The home page's touch icon or largest declared icon, else `/favicon.ico`; icons smaller
    /// than 32 px are too blurry to use.
    async fn site_icon(&self, domain: &str) -> Png {
        let home = format!("https://{domain}/");
        let mut candidates = Vec::new();
        if let Some(page) = self.web.get(home, MAX_PAGE).await {
            let head = &page.body[..page.body.len().min(MAX_PAGE)];
            candidates = web::icon_links(&String::from_utf8_lossy(head), &page.url);
        }
        candidates.push(format!("https://{domain}/favicon.ico"));
        for url in candidates.into_iter().take(4) {
            if let Some(fetched) = self.fetch(&url, MAX_PICTURE).await
                && let Some(png) = normalize(fetched, Style::Icon).await
            {
                return Some(png);
            }
        }
        None
    }

    /// The body of a successful anonymous GET; nil on any failure or past `limit` bytes.
    async fn fetch(&self, url: &str, limit: usize) -> Option<Vec<u8>> {
        let page = self.web.get(url.to_string(), limit).await?;
        (page.body.len() <= limit).then_some(page.body)
    }
}

fn needs_refresh(contacts: &Contacts, directory: &Directory) -> bool {
    let accounts: HashSet<&str> = contacts.accounts.iter().map(|account| account.name.as_str()).collect();
    let due = directory.people.is_none()
        || directory.asked() != accounts
        || directory::now() - directory.built >= DIRECTORY_AGE;
    // Not on every lookup while the providers cannot be reached.
    due && contacts.last_incomplete.is_none_or(|last| last.elapsed() >= DIRECTORY_RETRY)
}

/// Google accounts with `contacts.readonly` (and the domain directory with
/// `directory.readonly`) and Microsoft accounts with `Contacts.Read`, as signed in now.
pub(crate) fn photo_accounts(records: &[crate::accounts::AccountRecord], api: impl Fn(&str) -> Option<Arc<Api>>) -> Vec<PhotoAccount> {
    records
        .iter()
        .filter_map(|record| {
            let has = |scope: &str| record.scopes.iter().any(|granted| granted == scope);
            let directory = match record.provider {
                Provider::Google if has(directory::GOOGLE_CONTACTS) => has(directory::GOOGLE_DIRECTORY),
                Provider::Microsoft if has(directory::GRAPH_CONTACTS) => false,
                _ => return None,
            };
            Some(PhotoAccount { name: record.id.clone(), provider: record.provider, directory, api: api(&record.id)? })
        })
        .collect()
}

/// `data` as a 96 px PNG in `style`, decoded off the runtime's workers so lookups holding
/// permits decode in parallel.
async fn normalize(data: Vec<u8>, style: Style) -> Png {
    tokio::task::spawn_blocking(move || picture::normalize(&data, style, SIDE)).await.ok().flatten()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The row-size cache entry of `key`, per published picture.
fn cache_key(key: &str, published: Option<&str>) -> String {
    let named = match published {
        Some(url) => format!("{key}|{url}"),
        None => key.to_string(),
    };
    format!("p-{}", hash(&named))
}

/// What a miss is made against: the directory, and the published picture if any, since a miss
/// made without it, or with another one, does not stand for a lookup with it.
fn stamp(directory: &Directory, published: Option<&str>) -> String {
    match published {
        Some(url) => format!("{}|{}", directory.stamp, hash(url)),
        None => directory.stamp.clone(),
    }
}

/// Google serves its pictures at any size (`=s96-c` after the path, replacing whatever options
/// the link has, such as Chat's `=k-no`): ask for the one drawn. Other links stay as they are.
fn sized(url: &str) -> String {
    static OPTIONS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"=[^/=]*$").expect("pattern"));
    let google = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.ends_with("googleusercontent.com")))
        .unwrap_or(false);
    if !google {
        return url.to_string();
    }
    format!("{}=s{SIDE}-c", OPTIONS.replace(url, ""))
}

const FREEMAIL: &[&str] = &[
    "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com", "ymail.com",
    "icloud.com", "me.com", "mac.com", "aol.com", "proton.me", "protonmail.com", "gmx.com", "gmx.de", "yandex.ru",
    "qq.com", "foxmail.com", "163.com", "126.com", "sina.com", "yeah.net", "naver.com",
];

fn looks_automated(email: &str) -> bool {
    static AUTOMATED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(no-?reply|do-?not-?reply|notifications?|alerts?|news(letter)?|updates?|info|hello|team|support|mailer|digest|account|billing|security|service)\b")
            .expect("pattern")
    });
    let local = email.split('@').find(|part| !part.is_empty()).unwrap_or_default().to_lowercase();
    AUTOMATED.is_match(&local)
}

/// The registrable domain, roughly: the last two labels, or three under a short second level
/// such as `co.uk`.
fn organization(domain: &str) -> String {
    let labels: Vec<&str> = domain.split('.').filter(|label| !label.is_empty()).collect();
    if labels.len() <= 2 {
        return domain.to_string();
    }
    let count = labels.len();
    let keep = if labels[count - 1].chars().count() == 2 && labels[count - 2].chars().count() <= 3 { 3 } else { 2 };
    labels[count - keep..].join(".")
}

/// Hex SHA-256 of `text` trimmed of spaces and lowercased, the way Gravatar keys addresses.
fn hash(text: &str) -> String {
    let trimmed = text.trim_matches(|character: char| character.is_whitespace() && !matches!(character, '\n'..='\r' | '\u{85}' | '\u{2028}' | '\u{2029}'));
    hex_sha256(trimmed.to_lowercase().as_bytes())
}

fn hex_sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `path` as Foundation's `URL(fileURLWithPath:).absoluteString` writes it.
fn file_url(path: &Path) -> String {
    const PATH: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'!')
        .remove(b'$')
        .remove(b'&')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')')
        .remove(b'*')
        .remove(b'+')
        .remove(b',')
        .remove(b';')
        .remove(b'=')
        .remove(b':')
        .remove(b'@')
        .remove(b'/');
    format!("file://{}", percent_encoding::utf8_percent_encode(&crate::files::url_path(path), PATH))
}

#[cfg(test)]
pub(crate) mod tests;
