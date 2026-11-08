//! What the engine has fetched, kept as the Swift engine's `MailCache` and `MailDatabase` keep
//! it (`Sources/MailstreamKit/Store/MailCache.swift`, `MailDatabase.swift`): accounts,
//! mailboxes, every listed envelope with its paging cursor, message content, AI tags and Google
//! Chat (accounts, space lists, recent spaces' messages) in the
//! Turso database Application Support/Mailstream/mail.db, with the same tables, keys and JSON
//! values; attachments and inline parts as files under ~/Library/Caches. Every process with an
//! engine opens the file through Turso's multi-process WAL, as the Swift engine does.
//!
//! Writes are queued in call order to one writer task, which also encodes, so the last write
//! for a row wins and callers never wait. Reads go to their own connection.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::sync::{OnceCell, mpsc, oneshot};
use turso::Value as SqlValue;

use crate::calendar::CalendarCopy;
use crate::chat_model::{ChatAccount, ChatMessagePage, ChatSpaceKey, ChatSpaceList};
use crate::model::{Account, Envelope, Mailbox, MailboxKey, MessageContent, MessageRef};
use crate::signal::ProcessSignal;
use crate::tagging::ConversationTag;

pub const CONTENT_VERSION: u32 = 3;
pub const SCHEME: &str = "mscid";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS mailboxes (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS contents (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS chat_spaces (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS chat_messages (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS calendars (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS tags (key TEXT PRIMARY KEY, value BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS listings (key TEXT PRIMARY KEY, next TEXT);
    CREATE TABLE IF NOT EXISTS envelopes (
        listing TEXT NOT NULL, position INTEGER NOT NULL, id TEXT NOT NULL, value BLOB NOT NULL,
        PRIMARY KEY (listing, id));
    CREATE INDEX IF NOT EXISTS envelopes_order ON envelopes (listing, position);
";

/// Tables holding one JSON value per key.
#[derive(Clone, Copy)]
enum Table {
    State,
    Mailboxes,
    Contents,
    ChatSpaces,
    ChatMessages,
    Calendars,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Mailboxes => "mailboxes",
            Self::Contents => "contents",
            Self::ChatSpaces => "chat_spaces",
            Self::ChatMessages => "chat_messages",
            Self::Calendars => "calendars",
        }
    }
}

/// The first 16 bytes of the SHA-256 of `parts` joined by U+001F, in hex: how the Swift engine
/// names rows and folders.
pub fn hash(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\u{1F}").as_bytes());
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A mailbox's listing as last loaded: its envelopes, newest first, and the cursor of the page
/// after them.
#[derive(Clone, Debug, Default)]
pub struct CachedListing {
    pub envelopes: Vec<Arc<Envelope>>,
    pub next: Option<String>,
}

enum Write {
    Value { table: Table, key: String, value: Vec<u8> },
    Remove { table: Table, key: String },
    Listing { key: String, listing: CachedListing },
    /// AI tags, encoded, written over the same keys.
    Tags(Vec<(String, Vec<u8>)>),
    ClearTags,
    Flush(oneshot::Sender<()>),
}

/// The opened database, or why it could not open (it is then left alone, as the Swift engine
/// leaves it).
type Opened = Option<turso::Database>;

struct Database {
    path: PathBuf,
    opened: OnceCell<Opened>,
    reader: tokio::sync::Mutex<Option<turso::Connection>>,
}

impl Database {
    async fn database(&self) -> Option<&turso::Database> {
        self.opened
            .get_or_init(|| async {
                match open(&self.path).await {
                    Ok(database) => Some(database),
                    Err(error) => {
                        eprintln!("Mailstream database {}: {error}", self.path.display());
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    async fn connection(&self) -> Option<turso::Connection> {
        let connection = self.database().await?.connect().ok()?;
        // Another process writing holds the WAL briefly; wait for it rather than fail.
        let _ = connection.busy_timeout(Duration::from_millis(2000));
        Some(connection)
    }

    /// `body` on the reading connection; nil when the database cannot open or `body` fails.
    async fn read<T>(&self, body: impl AsyncFnOnce(&turso::Connection) -> turso::Result<T>) -> Option<T> {
        let mut reader = self.reader.lock().await;
        if reader.is_none() {
            *reader = self.connection().await;
        }
        let connection = reader.as_ref()?;
        match body(connection).await {
            Ok(value) => Some(value),
            Err(error) => {
                eprintln!("Mailstream database: {error}");
                None
            }
        }
    }
}

async fn open(path: &Path) -> Result<turso::Database, String> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let path = path.to_str().ok_or("the path is not UTF-8")?;
    let builder = turso::Builder::new_local(path);
    // Every Mailstream process on a Mac shares mail.db; Turso's Windows file layer has no shared
    // WAL, and there the front end is the only process that opens it.
    #[cfg(not(windows))]
    let builder = builder.experimental_multiprocess_wal(true);
    let database = builder
        .build()
        .await
        .map_err(|error| error.to_string())?;
    let connection = database.connect().map_err(|error| error.to_string())?;
    let _ = connection.busy_timeout(Duration::from_millis(2000));
    connection.execute_batch(SCHEMA).await.map_err(|error| error.to_string())?;
    Ok(database)
}

fn blob(value: SqlValue) -> Option<Vec<u8>> {
    match value {
        SqlValue::Blob(bytes) => Some(bytes),
        SqlValue::Text(text) => Some(text.into_bytes()),
        _ => None,
    }
}

fn text(value: SqlValue) -> Option<String> {
    match value {
        SqlValue::Text(text) => Some(text),
        SqlValue::Blob(bytes) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// JSON as Swift's `JSONEncoder` writes it.
fn encode<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    crate::json::encode(value, false, true)
}

pub struct MailCache {
    /// Files: attachments and inline parts, the Gmail envelope caches.
    pub root: PathBuf,
    database: Arc<Database>,
    writes: mpsc::UnboundedSender<Write>,
}

impl MailCache {
    /// `database` is mail.db; `root` the folder of files (`~/Library/Caches/dev.mailstream.Mailstream`).
    /// Starts the writer on the current runtime.
    pub fn new(database: PathBuf, root: PathBuf) -> Self {
        let database =
            Arc::new(Database { path: database, opened: OnceCell::new(), reader: tokio::sync::Mutex::new(None) });
        let (writes, queue) = mpsc::unbounded_channel();
        tokio::spawn(Self::writer(database.clone(), queue));
        Self { root, database, writes }
    }

    async fn writer(database: Arc<Database>, mut queue: mpsc::UnboundedReceiver<Write>) {
        let mut connection: Option<turso::Connection> = None;
        while let Some(write) = queue.recv().await {
            if let Write::Flush(done) = write {
                let _ = done.send(());
                continue;
            }
            // Tag writes tell every Mailstream process to read the tags again, landed or not,
            // as the Swift engine's `MailCache.saveTags` does.
            let signals = matches!(write, Write::Tags(_) | Write::ClearTags);
            if connection.is_none() {
                connection = database.connection().await;
            }
            let Some(db) = connection.as_ref() else {
                if signals {
                    ProcessSignal::TAGS.post();
                }
                continue;
            };
            let result = match write {
                Write::Value { table, key, value } => db
                    .execute(
                        &format!(
                            "INSERT INTO {} (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                            table.name()
                        ),
                        vec![SqlValue::Text(key), SqlValue::Blob(value)],
                    )
                    .await
                    .map(drop),
                Write::Remove { table, key } => db
                    .execute(&format!("DELETE FROM {} WHERE key = ?", table.name()), vec![SqlValue::Text(key)])
                    .await
                    .map(drop),
                Write::Listing { key, listing } => Self::replace_listing(db, key, listing).await,
                Write::Tags(tags) => Self::set_tags(db, tags).await,
                Write::ClearTags => db.execute("DELETE FROM tags", ()).await.map(drop),
                Write::Flush(_) => unreachable!("answered above"),
            };
            if let Err(error) = result {
                eprintln!("Mailstream database: {error}");
            }
            if signals {
                ProcessSignal::TAGS.post();
            }
        }
    }

    /// Writes these tags, leaving the others: each process writes only what it changed, so one
    /// never overwrites another's changes with its older copy.
    async fn set_tags(db: &turso::Connection, tags: Vec<(String, Vec<u8>)>) -> turso::Result<()> {
        db.execute("BEGIN", ()).await?;
        let result = async {
            let mut insert = db.prepare("INSERT OR REPLACE INTO tags (key, value) VALUES (?, ?)").await?;
            for (key, value) in tags {
                insert.execute(vec![SqlValue::Text(key), SqlValue::Blob(value)]).await?;
            }
            db.execute("COMMIT", ()).await.map(drop)
        }
        .await;
        if result.is_err() {
            let _ = db.execute("ROLLBACK", ()).await;
        }
        result
    }

    async fn replace_listing(db: &turso::Connection, key: String, listing: CachedListing) -> turso::Result<()> {
        db.execute("BEGIN", ()).await?;
        let result = async {
            db.execute(
                "INSERT INTO listings (key, next) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET next = excluded.next",
                vec![SqlValue::Text(key.clone()), listing.next.map_or(SqlValue::Null, SqlValue::Text)],
            )
            .await?;
            db.execute("DELETE FROM envelopes WHERE listing = ?", vec![SqlValue::Text(key.clone())]).await?;
            let mut seen = std::collections::HashSet::new();
            let mut insert = db.prepare("INSERT INTO envelopes (listing, position, id, value) VALUES (?, ?, ?, ?)").await?;
            for (position, envelope) in listing.envelopes.iter().enumerate() {
                if !seen.insert(envelope.id.as_str()) {
                    continue;
                }
                insert
                    .execute(vec![
                        SqlValue::Text(key.clone()),
                        SqlValue::Integer(position as i64),
                        SqlValue::Text(envelope.id.clone()),
                        SqlValue::Blob(encode(envelope.as_ref())),
                    ])
                    .await?;
            }
            db.execute("COMMIT", ()).await.map(drop)
        }
        .await;
        if result.is_err() {
            let _ = db.execute("ROLLBACK", ()).await;
        }
        result
    }

    /// Returns once every write queued before the call has landed.
    pub async fn flush(&self) {
        let (done, landed) = oneshot::channel();
        if self.writes.send(Write::Flush(done)).is_ok() {
            let _ = landed.await;
        }
    }

    async fn value(&self, table: Table, key: &str) -> Option<Vec<u8>> {
        let sql = format!("SELECT value FROM {} WHERE key = ?", table.name());
        let key = key.to_string();
        self.database
            .read(async move |db| {
                let mut rows = db.query(&sql, vec![SqlValue::Text(key)]).await?;
                Ok(match rows.next().await? {
                    Some(row) => blob(row.get_value(0)?),
                    None => None,
                })
            })
            .await
            .flatten()
    }

    async fn read<T: DeserializeOwned>(&self, table: Table, key: &str) -> Option<T> {
        let data = self.value(table, key).await?;
        serde_json::from_slice(&data).ok()
    }

    fn write<T: Serialize + ?Sized>(&self, value: &T, table: Table, key: String) {
        let _ = self.writes.send(Write::Value { table, key, value: encode(value) });
    }

    // MARK: Accounts

    pub async fn load_accounts(&self) -> Option<Vec<Account>> {
        self.read(Table::State, "accounts").await
    }

    pub fn save_accounts(&self, accounts: &[Account]) {
        self.write(accounts, Table::State, "accounts".into());
    }

    // MARK: Auto-read

    /// What auto-read marked read and may still undo, and the messages it is done with
    /// (`mail::autoread`); one row every process shares.
    pub async fn load_auto_read<T: DeserializeOwned>(&self) -> Option<T> {
        self.read(Table::State, "autoRead").await
    }

    pub fn save_auto_read<T: Serialize>(&self, value: &T) {
        self.write(value, Table::State, "autoRead".into());
    }

    // MARK: Tags

    pub async fn load_tags(&self) -> HashMap<String, ConversationTag> {
        let rows = self
            .database
            .read(async |db| {
                let mut rows = db.query("SELECT key, value FROM tags", ()).await?;
                let mut out = Vec::new();
                while let Some(row) = rows.next().await? {
                    if let (Some(key), Some(value)) = (text(row.get_value(0)?), blob(row.get_value(1)?)) {
                        out.push((key, value));
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        rows.into_iter().filter_map(|(key, value)| Some((key, serde_json::from_slice(&value).ok()?))).collect()
    }

    /// Writes the tags that changed, then tells every Mailstream process to read them again.
    pub fn save_tags(&self, changed: HashMap<String, ConversationTag>) {
        let rows = changed.into_iter().map(|(key, tag)| (key, encode(&tag))).collect();
        let _ = self.writes.send(Write::Tags(rows));
    }

    /// Forgets every tag, then tells every Mailstream process.
    pub fn clear_tags(&self) {
        let _ = self.writes.send(Write::ClearTags);
    }

    // MARK: Listings

    fn listing_key(key: &MailboxKey) -> String {
        hash(&[&key.account, &key.mailbox])
    }

    pub async fn load_listing(&self, key: &MailboxKey) -> Option<CachedListing> {
        let key = Self::listing_key(key);
        let (values, next) = self
            .database
            .read(async move |db| {
                let mut listing = db.query("SELECT next FROM listings WHERE key = ?", vec![SqlValue::Text(key.clone())]).await?;
                let Some(row) = listing.next().await? else { return Ok(None) };
                let next = text(row.get_value(0)?);
                drop(listing);
                let mut rows = db
                    .query("SELECT value FROM envelopes WHERE listing = ? ORDER BY position", vec![SqlValue::Text(key)])
                    .await?;
                let mut values = Vec::new();
                while let Some(row) = rows.next().await? {
                    if let Some(value) = blob(row.get_value(0)?) {
                        values.push(value);
                    }
                }
                Ok(Some((values, next)))
            })
            .await
            .flatten()?;
        let envelopes =
            values.iter().filter_map(|value| serde_json::from_slice::<Envelope>(value).ok().map(Arc::new)).collect();
        Some(CachedListing { envelopes, next })
    }

    pub fn save_listing(&self, listing: CachedListing, key: &MailboxKey) {
        let _ = self.writes.send(Write::Listing { key: Self::listing_key(key), listing });
    }

    // MARK: Mailboxes

    pub async fn load_mailboxes(&self, account: &str) -> Option<Vec<Mailbox>> {
        self.read(Table::Mailboxes, &hash(&[account])).await
    }

    pub fn save_mailboxes(&self, mailboxes: &[Mailbox], account: &str) {
        self.write(mailboxes, Table::Mailboxes, hash(&[account]));
    }

    // MARK: Message content

    pub fn message_key(message: &MessageRef) -> String {
        hash(&[&message.account, &message.mailbox, &message.id])
    }

    pub fn message_directory(&self, message: &MessageRef) -> PathBuf {
        self.root.join("messages-v2").join(Self::message_key(message))
    }

    /// Where the reader's scheme handler serves `message`'s inline parts from.
    pub fn part_url_base(message: &MessageRef) -> String {
        format!("{SCHEME}://{}/", Self::message_key(message))
    }

    /// The message's directory as a `file:` URL with a trailing slash, as `URL.absoluteString`
    /// writes a directory.
    pub fn message_directory_url(&self, message: &MessageRef) -> String {
        let path = self.message_directory(message);
        format!("file://{}/", crate::assembly::path_encoded(&crate::files::url_path(&path)))
    }

    fn content_key(message: &MessageRef) -> String {
        format!("v{CONTENT_VERSION}:{}", Self::message_key(message))
    }

    pub async fn load_content(&self, message: &MessageRef) -> Option<MessageContent> {
        self.read(Table::Contents, &Self::content_key(message)).await
    }

    pub fn save_content(&self, content: &MessageContent, message: &MessageRef) {
        self.write(content, Table::Contents, Self::content_key(message));
    }

    pub fn attachment_path(&self, stored_name: &str, message: &MessageRef) -> PathBuf {
        self.message_directory(message).join(stored_name)
    }

    // MARK: Chat

    pub async fn load_chat_accounts(&self) -> Option<Vec<ChatAccount>> {
        self.read(Table::State, "chatAccounts").await
    }

    pub fn save_chat_accounts(&self, accounts: &[ChatAccount]) {
        self.write(accounts, Table::State, "chatAccounts".into());
    }

    pub async fn load_chat_spaces(&self, account: &str) -> Option<ChatSpaceList> {
        self.read(Table::ChatSpaces, &hash(&[account])).await
    }

    pub fn save_chat_spaces(&self, list: &ChatSpaceList, account: &str) {
        self.write(list, Table::ChatSpaces, hash(&[account]));
    }

    /// Spaces whose messages are kept, most recently opened first.
    pub async fn load_chat_recent(&self) -> Option<Vec<ChatSpaceKey>> {
        self.read(Table::State, "chatRecent").await
    }

    pub fn save_chat_recent(&self, keys: &[ChatSpaceKey]) {
        self.write(keys, Table::State, "chatRecent".into());
    }

    fn chat_messages_key(key: &ChatSpaceKey) -> String {
        hash(&[&key.account, &key.space])
    }

    pub async fn load_chat_messages(&self, key: &ChatSpaceKey) -> Option<ChatMessagePage> {
        self.read(Table::ChatMessages, &Self::chat_messages_key(key)).await
    }

    pub fn save_chat_messages(&self, page: &ChatMessagePage, key: &ChatSpaceKey) {
        self.write(page, Table::ChatMessages, Self::chat_messages_key(key));
    }

    pub fn remove_chat_messages(&self, key: &ChatSpaceKey) {
        let _ = self.writes.send(Write::Remove { table: Table::ChatMessages, key: Self::chat_messages_key(key) });
    }

    // MARK: Calendars

    /// An account's calendars and events as last fetched.
    pub async fn load_calendar(&self, account: &str) -> Option<CalendarCopy> {
        self.read(Table::Calendars, &hash(&[account])).await
    }

    pub fn save_calendar(&self, copy: &CalendarCopy, account: &str) {
        self.write(copy, Table::Calendars, hash(&[account]));
    }

    pub fn remove_calendar(&self, account: &str) {
        let _ = self.writes.send(Write::Remove { table: Table::Calendars, key: hash(&[account]) });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Address, MailboxRole};

    pub(crate) fn envelope(id: &str, date: f64) -> Envelope {
        Envelope {
            id: id.into(),
            thread: None,
            message_id: Some(format!("{id}@x")),
            in_reply_to: vec![],
            flags: Envelope::flags(false, false, false),
            subject: format!("Subject {id}"),
            from: vec![Address::new(Some("Ann".into()), "ann@x.com")],
            to: vec![Address::new(None, "me@x.com")],
            date: Some(date),
            size: 0,
            has_attachment: Some(false),
        }
    }

    #[test]
    fn names_rows_as_the_swift_engine() {
        // MailCache.hash("a@b.c", "INBOX") in the Swift engine.
        assert_eq!(hash(&["a@b.c", "INBOX"]).len(), 32);
        assert_eq!(hash(&["x"]), "2d711642b726b04401627ca9fbac32f5");
    }

    #[tokio::test]
    async fn keeps_listings_mailboxes_and_content() {
        let directory = std::env::temp_dir().join(format!("mailstream-cache-{}-{}", std::process::id(), crate::tests::unique()));
        let cache = MailCache::new(directory.join("mail.db"), directory.join("files"));
        let key = MailboxKey::new("me@x.com", "INBOX");
        assert!(cache.load_listing(&key).await.is_none());
        let envelopes = vec![Arc::new(envelope("b", 20.0)), Arc::new(envelope("a", 10.0)), Arc::new(envelope("b", 20.0))];
        cache.save_listing(CachedListing { envelopes, next: Some("page2".into()) }, &key);
        let boxes = vec![Mailbox {
            account: "me@x.com".into(),
            id: "INBOX".into(),
            name: "Inbox".into(),
            role: MailboxRole::Inbox,
            unread: Some(3),
            total: None,
        }];
        cache.save_mailboxes(&boxes, "me@x.com");
        cache.flush().await;
        let listing = cache.load_listing(&key).await.unwrap();
        let ids: Vec<&str> = listing.envelopes.iter().map(|envelope| envelope.id.as_str()).collect();
        assert_eq!((ids, listing.next.as_deref()), (vec!["b", "a"], Some("page2")));
        assert_eq!(cache.load_mailboxes("me@x.com").await.unwrap(), boxes);
        cache.save_listing(CachedListing { envelopes: vec![], next: None }, &key);
        cache.flush().await;
        let listing = cache.load_listing(&key).await.unwrap();
        assert!(listing.envelopes.is_empty() && listing.next.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// mail.db as the Swift engine keeps it, checked across engines by `scripts/mail-db-interop.sh`:
    /// each engine writes `Tests/Fixtures/mail-db-interop.json` into a throwaway home
    /// (`MAILSTREAM_INTEROP=write`), then reads the home the other one wrote (`=read`); the Swift
    /// side is `MailDatabaseInteropTests`. Without the variable the test does nothing, so it never
    /// touches the person's own mail.db.
    #[tokio::test]
    async fn reads_and_writes_what_the_swift_engine_does() {
        #[derive(serde::Deserialize)]
        struct Key {
            account: String,
            mailbox: String,
        }
        #[derive(serde::Deserialize)]
        struct Listing {
            key: Key,
            envelopes: Vec<Envelope>,
            next: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Ref {
            account: String,
            mailbox: String,
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct Content {
            r#ref: Ref,
            content: MessageContent,
        }
        #[derive(serde::Deserialize)]
        struct ChatPage {
            key: ChatSpaceKey,
            page: ChatMessagePage,
        }
        #[derive(serde::Deserialize)]
        struct Chat {
            accounts: Vec<ChatAccount>,
            recent: Vec<ChatSpaceKey>,
            spaces: HashMap<String, ChatSpaceList>,
            messages: Vec<ChatPage>,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            accounts: Vec<Account>,
            mailboxes: HashMap<String, Vec<Mailbox>>,
            listings: Vec<Listing>,
            tags: HashMap<String, ConversationTag>,
            contents: Vec<Content>,
            chat: Chat,
        }
        let Ok(mode) = std::env::var("MAILSTREAM_INTEROP") else { return };
        let home = std::env::var("HOME").unwrap();
        let config = crate::Config::for_this_app();
        assert!(config.support_directory.starts_with(&home), "mail.db must be in the throwaway home");
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../Tests/Fixtures/mail-db-interop.json");
        let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let cache = MailCache::new(config.support_directory.join("mail.db"), config.cache_directory.clone());
        let message = |item: &Content| MessageRef {
            account: item.r#ref.account.clone(),
            mailbox: item.r#ref.mailbox.clone(),
            id: item.r#ref.id.clone(),
        };
        if mode == "write" {
            cache.save_accounts(&fixture.accounts);
            for (account, mailboxes) in &fixture.mailboxes {
                cache.save_mailboxes(mailboxes, account);
            }
            for listing in &fixture.listings {
                let envelopes = listing.envelopes.iter().cloned().map(Arc::new).collect();
                let key = MailboxKey::new(&listing.key.account, &listing.key.mailbox);
                cache.save_listing(CachedListing { envelopes, next: listing.next.clone() }, &key);
            }
            for item in &fixture.contents {
                cache.save_content(&item.content, &message(item));
            }
            cache.save_chat_accounts(&fixture.chat.accounts);
            cache.save_chat_recent(&fixture.chat.recent);
            for (account, list) in &fixture.chat.spaces {
                cache.save_chat_spaces(list, account);
            }
            for item in &fixture.chat.messages {
                cache.save_chat_messages(&item.page, &item.key);
            }
            cache.save_tags(fixture.tags.clone());
            cache.flush().await;
        }
        assert_eq!(cache.load_accounts().await.as_ref(), Some(&fixture.accounts));
        for (account, mailboxes) in &fixture.mailboxes {
            assert_eq!(cache.load_mailboxes(account).await.as_ref(), Some(mailboxes), "mailboxes of {account}");
        }
        assert_eq!(cache.load_tags().await, fixture.tags);
        for listing in &fixture.listings {
            let loaded = cache.load_listing(&MailboxKey::new(&listing.key.account, &listing.key.mailbox)).await.unwrap();
            let envelopes: Vec<Envelope> = loaded.envelopes.iter().map(|envelope| (**envelope).clone()).collect();
            assert_eq!((envelopes, loaded.next), (listing.envelopes.clone(), listing.next.clone()), "{}", listing.key.mailbox);
        }
        for item in &fixture.contents {
            assert_eq!(cache.load_content(&message(item)).await.as_ref(), Some(&item.content), "{}", item.r#ref.id);
        }
        assert_eq!(cache.load_chat_accounts().await.as_ref(), Some(&fixture.chat.accounts));
        assert_eq!(cache.load_chat_recent().await.as_ref(), Some(&fixture.chat.recent));
        for (account, list) in &fixture.chat.spaces {
            assert_eq!(cache.load_chat_spaces(account).await.as_ref(), Some(list), "chat spaces of {account}");
        }
        for item in &fixture.chat.messages {
            assert_eq!(cache.load_chat_messages(&item.key).await.as_ref(), Some(&item.page), "chat messages of {}", item.key.space);
        }
    }
}
