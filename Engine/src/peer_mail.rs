//! Mail bodies between the user's own devices (MS-394): before asking Gmail or Outlook for a
//! message's body, ask the group's devices online now whether they hold it already, so it costs
//! no provider quota (Gmail charges 60 units a message in full and 20 an attachment).
//!
//! The protocol, ALPN `mailstream/mail/1`, rides on device sync's endpoint behind its gate
//! (`peer_sync::Service`): only devices the group admitted (MS-380) connect, and each request is
//! checked again, so a removed device's lasting connection gets nothing more. Mail never enters
//! the group's document, its blobs or the cloud drop. One bidirectional stream per request:
//! - `Have`: `{account, mailbox, id}` of up to [`HAVE_LIMIT`] messages and this device's
//!   [`CONTENT_VERSION`]; the answer is a JSON array of booleans in the same order.
//! - `Get`: one message; the answer is a big-endian `u32` length, a [`GetAnswer`] of that length
//!   and, when found, the bytes of its files in the order it lists them.
//!
//! A device answers only from what it stored (mail.db's content and the message's folder in
//! `messages-v2/`) for accounts signed in here and not removed from the group, at the same
//! `CONTENT_VERSION`. Content is stored by `hash(account, mailbox, id)`, so a message stored under
//! another mailbox of the account is found by trying each. It serves [`SERVING`] bodies at once
//! (more get `Busy`) and at most [`ANSWER_LIMIT`] bytes each.
//!
//! Asking: [`PeerMail::fetch`] sends `Have` to every device connected now (and, for background
//! loads, dials the others), takes the body from the first that says it has it, then the next,
//! and gives up for the provider when none does in time ([`Wait`]). Only messages the provider
//! listed are asked for, and nothing is pushed: deletions, labels and read state still follow
//! the provider alone. Background `Have`s within [`BATCH_WINDOW`] travel as one request.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use futures::stream::{FuturesUnordered, StreamExt};
use iroh::EndpointId;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use serde::{Deserialize, Serialize};
use tokio::sync::{OnceCell, Semaphore, oneshot};

use crate::accounts::Accounts;
use crate::answers::SyncMail;
use crate::api::Priority;
use crate::cache::{CONTENT_VERSION, MailCache};
use crate::model::{MessageContent, MessageRef};
use crate::peer_sync::{Link, Service};

pub const ALPN: &[u8] = b"mailstream/mail/1";
/// The most a request holds.
const REQUEST_LIMIT: usize = 256 * 1024;
/// The most messages one `Have` asks about.
const HAVE_LIMIT: usize = 500;
/// The most a `Have` answer holds: `true,` per message and the brackets.
const HAVE_ANSWER_LIMIT: usize = HAVE_LIMIT * 6 + 2;
/// The most one body with its files takes; a larger one comes from the provider.
const ANSWER_LIMIT: u64 = 32 * 1024 * 1024;
/// Bodies a device sends at once.
const SERVING: usize = 4;
/// How long dialing a device may take, and how long one that didn't answer is left alone.
const DIAL_WAIT: Duration = Duration::from_secs(5);
const DIAL_BACKOFF: Duration = Duration::from_secs(60);
/// How long background `Have`s wait for others to travel with.
const BATCH_WINDOW: Duration = Duration::from_millis(20);
/// Gmail's units for a message in full and for each attachment it would have fetched.
const GMAIL_MESSAGE_UNITS: u64 = 60;
const GMAIL_ATTACHMENT_UNITS: u64 = 20;

/// How long a load waits on the devices before the provider.
struct Wait {
    /// For a device to say it has the message.
    have: Duration,
    /// For the body once one said so.
    get: Duration,
    /// Whether to dial devices not connected yet.
    dial: bool,
}

impl Wait {
    fn of(priority: Priority) -> Self {
        match priority {
            // The reader waits: only devices connected already, briefly.
            Priority::Interactive => Self { have: Duration::from_millis(500), get: Duration::from_secs(5), dial: false },
            Priority::Background => Self { have: Duration::from_secs(3), get: Duration::from_secs(30), dial: true },
        }
    }
}

/// A message as the provider names it: the same on every device signed in to the account.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Wanted {
    account: String,
    mailbox: String,
    id: String,
}

impl Wanted {
    fn of(message: &MessageRef) -> Self {
        Self { account: message.account.clone(), mailbox: message.mailbox.clone(), id: message.id.clone() }
    }

    fn in_mailbox(&self, mailbox: &str) -> MessageRef {
        MessageRef { account: self.account.clone(), mailbox: mailbox.to_string(), id: self.id.clone() }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "ask")]
enum Request {
    Have { version: u32, messages: Vec<Wanted> },
    Get { version: u32, message: Wanted },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "answer")]
enum GetAnswer {
    /// `key` is the sender's [`MailCache::message_key`], which its HTML's inline parts name.
    Found { key: String, content: MessageContent, files: Vec<File> },
    Missing,
    /// Serving [`SERVING`] bodies already.
    Busy,
}

#[derive(Debug, Serialize, Deserialize)]
struct File {
    name: String,
    size: u64,
}

/// A device connected on [`ALPN`], with the background `Have`s waiting to travel together.
#[derive(Clone)]
struct Peer {
    connection: Connection,
    batch: Arc<Mutex<Vec<(Wanted, oneshot::Sender<bool>)>>>,
}

type Dial = Shared<BoxFuture<'static, Option<Peer>>>;

#[derive(Default)]
struct Client {
    link: Option<Link>,
    peers: HashMap<EndpointId, Peer>,
    dialing: HashMap<EndpointId, Dial>,
    /// When dialing each device last failed.
    failed: HashMap<EndpointId, Instant>,
}

pub struct PeerMail {
    cache: Arc<MailCache>,
    accounts: Arc<Accounts>,
    this: Weak<PeerMail>,
    client: Mutex<Client>,
    serving: Arc<Semaphore>,
    /// The accounts the group removed, as of its change count.
    removed: Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    /// What came from other devices, kept in mail.db.
    received: OnceCell<Mutex<SyncMail>>,
}

impl PeerMail {
    pub fn new(cache: Arc<MailCache>, accounts: Arc<Accounts>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            cache,
            accounts,
            this: this.clone(),
            client: Mutex::new(Client::default()),
            serving: Arc::new(Semaphore::new(SERVING)),
            removed: Mutex::new(None),
            received: OnceCell::new(),
        })
    }

    fn client(&self) -> std::sync::MutexGuard<'_, Client> {
        self.client.lock().expect("peer mail lock")
    }

    async fn counts(&self) -> &Mutex<SyncMail> {
        self.received.get_or_init(|| async { Mutex::new(self.cache.load_peer_mail().await.unwrap_or_default()) }).await
    }

    /// What this device took from the group's other devices so far.
    pub async fn received(&self) -> SyncMail {
        self.counts().await.lock().expect("peer mail counts").clone()
    }

    // MARK: Asking

    /// `message`'s content from a device of the group that holds it, written to this device's
    /// mail.db and parts folder as the provider's answer would be; `None` when none does in
    /// time. `gmail`: the account is Gmail's, whose units it saves.
    pub async fn fetch(&self, message: &MessageRef, gmail: bool, priority: Priority) -> Option<MessageContent> {
        let link = self.client().link.clone()?;
        let wait = Wait::of(priority);
        let (dial, batch) = (wait.dial, priority == Priority::Background);
        let wanted = Wanted::of(message);
        let mut asked: FuturesUnordered<_> = link
            .members()
            .into_iter()
            .map(|id| {
                let (link, wanted) = (link.clone(), wanted.clone());
                async move {
                    let peer = self.peer(&link, id, dial).await?;
                    have(&peer, wanted, batch).await.then_some(peer)
                }
            })
            .collect();
        let deadline = tokio::time::Instant::now() + wait.have;
        while let Ok(Some(answer)) = tokio::time::timeout_at(deadline, asked.next()).await {
            let Some(peer) = answer else { continue };
            let got = tokio::time::timeout(wait.get, self.get(&peer.connection, &wanted, message)).await;
            if let Ok(Some(content)) = got {
                self.count(&content, gmail).await;
                return Some(content);
            }
        }
        None
    }

    /// The connection to `id`: the one open, else (with `dial`) a new one, dialed once for every
    /// caller and not again within [`DIAL_BACKOFF`] of a failure.
    async fn peer(&self, link: &Link, id: EndpointId, dial: bool) -> Option<Peer> {
        let dialing = {
            let mut client = self.client();
            if let Some(peer) = client.peers.get(&id) {
                if peer.connection.close_reason().is_none() {
                    return Some(peer.clone());
                }
                client.peers.remove(&id);
            }
            if !dial {
                return None;
            }
            match client.dialing.get(&id) {
                Some(dialing) => dialing.clone(),
                None => {
                    if client.failed.get(&id).is_some_and(|at| at.elapsed() < DIAL_BACKOFF) {
                        return None;
                    }
                    let (this, link) = (self.this.clone(), link.clone());
                    let task = tokio::spawn(async move {
                        let connection = tokio::time::timeout(DIAL_WAIT, link.connect(id, ALPN)).await.ok().and_then(Result::ok);
                        this.upgrade()?.dialed(id, connection)
                    });
                    let dialing = async move { task.await.ok().flatten() }.boxed().shared();
                    client.dialing.insert(id, dialing.clone());
                    dialing
                }
            }
        };
        dialing.await
    }

    fn dialed(&self, id: EndpointId, connection: Option<Connection>) -> Option<Peer> {
        let mut client = self.client();
        client.dialing.remove(&id);
        let Some(connection) = connection else {
            client.failed.insert(id, Instant::now());
            return None;
        };
        if client.link.is_none() {
            // Sync closed meanwhile.
            connection.close(VarInt::from_u32(0), b"");
            return None;
        }
        client.failed.remove(&id);
        let peer = Peer { connection, batch: Arc::default() };
        client.peers.insert(id, peer.clone());
        Some(peer)
    }

    /// Takes `wanted` from the device of `connection`, stored for `message` here.
    async fn get(&self, connection: &Connection, wanted: &Wanted, message: &MessageRef) -> Option<MessageContent> {
        let (mut send, mut recv) = connection.open_bi().await.ok()?;
        let request = Request::Get { version: CONTENT_VERSION, message: wanted.clone() };
        send.write_all(&serde_json::to_vec(&request).ok()?).await.ok()?;
        send.finish().ok()?;
        let mut length = [0; 4];
        recv.read_exact(&mut length).await.ok()?;
        let length = u64::from(u32::from_be_bytes(length));
        if length > ANSWER_LIMIT {
            return None;
        }
        let mut header = vec![0; length as usize];
        recv.read_exact(&mut header).await.ok()?;
        let GetAnswer::Found { key, mut content, files } = serde_json::from_slice(&header).ok()? else { return None };
        let total = files.iter().try_fold(length, |total, file| total.checked_add(file.size))?;
        let names: HashSet<&str> = files.iter().map(|file| file.name.as_str()).collect();
        if !is_key(&key)
            || total > ANSWER_LIMIT
            || !files.iter().all(|file| is_file_name(&file.name))
            || names.len() != files.len()
            || !content.attachments.iter().all(|attachment| names.contains(attachment.stored_name.as_str()))
        {
            return None;
        }
        let directory = self.cache.message_directory(message);
        std::fs::create_dir_all(&directory).ok()?;
        for file in &files {
            let mut data = vec![0; file.size as usize];
            recv.read_exact(&mut data).await.ok()?;
            std::fs::write(directory.join(&file.name), &data).ok()?;
        }
        // Inline parts point at the sender's folder; this device's has them now.
        content.html = content.html.map(|html| html.replace(&MailCache::key_url_base(&key), &MailCache::part_url_base(message)));
        self.cache.save_content(&content, message);
        Some(content)
    }

    async fn count(&self, content: &MessageContent, gmail: bool) {
        let mut counts = self.counts().await.lock().expect("peer mail counts");
        counts.messages += 1;
        if gmail {
            counts.gmail_units += GMAIL_MESSAGE_UNITS + GMAIL_ATTACHMENT_UNITS * content.attachments.len() as u64;
        }
        self.cache.save_peer_mail(&*counts);
    }

    // MARK: Answering

    async fn serve_connection(self: Arc<Self>, connection: Connection, link: Link) {
        let remote = connection.remote_id();
        while let Ok((send, recv)) = connection.accept_bi().await {
            if !link.allows(&remote) {
                connection.close(VarInt::from_u32(403), b"not admitted to this sync group");
                return;
            }
            let (this, link) = (self.clone(), link.clone());
            tokio::spawn(async move { this.answer(send, recv, &link).await });
        }
    }

    async fn answer(&self, mut send: SendStream, mut recv: RecvStream, link: &Link) {
        let Ok(request) = recv.read_to_end(REQUEST_LIMIT).await else { return };
        match serde_json::from_slice(&request) {
            Ok(Request::Have { version, messages }) => {
                let held = if version == CONTENT_VERSION && messages.len() <= HAVE_LIMIT {
                    self.held(&messages, link).await
                } else {
                    vec![false; messages.len()]
                };
                if let Ok(answer) = serde_json::to_vec(&held) {
                    let _ = send.write_all(&answer).await;
                }
                let _ = send.finish();
            }
            Ok(Request::Get { version, message }) => {
                let Ok(_permit) = self.serving.clone().try_acquire_owned() else {
                    let _ = send_answer(&mut send, &GetAnswer::Busy, &[]).await;
                    let _ = send.finish();
                    return;
                };
                let stored = if version == CONTENT_VERSION { self.stored(&message, link).await } else { None };
                let _ = match stored {
                    Some((answer, paths)) => send_answer(&mut send, &answer, &paths).await,
                    None => send_answer(&mut send, &GetAnswer::Missing, &[]).await,
                };
                let _ = send.finish();
            }
            Err(_) => {
                let _ = send.finish();
            }
        }
    }

    /// Which of `messages` this device would serve.
    async fn held(&self, messages: &[Wanted], link: &Link) -> Vec<bool> {
        let removed = self.removed(link).await;
        let mut mailboxes: HashMap<String, Vec<String>> = HashMap::new();
        let mut candidates = Vec::new();
        for wanted in messages {
            if self.serves(&wanted.account, &removed) && !mailboxes.contains_key(&wanted.account) {
                mailboxes.insert(wanted.account.clone(), self.mailboxes(&wanted.account).await);
            }
            if let Some(boxes) = mailboxes.get(&wanted.account) {
                candidates.extend(locations(wanted, boxes));
            }
        }
        let stored = self.cache.stored_contents(&candidates).await;
        messages
            .iter()
            .map(|wanted| {
                mailboxes.get(&wanted.account).is_some_and(|boxes| {
                    locations(wanted, boxes).any(|location| stored.contains(&MailCache::message_key(&location)))
                })
            })
            .collect()
    }

    /// `wanted`'s stored content and the files of its folder, as a `Found` answer, with the
    /// files' paths in the answer's order.
    async fn stored(&self, wanted: &Wanted, link: &Link) -> Option<(GetAnswer, Vec<std::path::PathBuf>)> {
        let removed = self.removed(link).await;
        if !self.serves(&wanted.account, &removed) {
            return None;
        }
        let boxes = self.mailboxes(&wanted.account).await;
        let candidates: Vec<MessageRef> = locations(wanted, &boxes).collect();
        let stored = self.cache.stored_contents(&candidates).await;
        let location = candidates.into_iter().find(|location| stored.contains(&MailCache::message_key(location)))?;
        let content = self.cache.load_content(&location).await?;
        let directory = self.cache.message_directory(&location);
        let mut files = Vec::new();
        let mut paths = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let Ok(metadata) = entry.metadata() else { continue };
                let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
                if metadata.is_file() && is_file_name(&name) {
                    files.push(File { name, size: metadata.len() });
                    paths.push(entry.path());
                }
            }
        }
        let names: HashSet<&str> = files.iter().map(|file| file.name.as_str()).collect();
        let total: u64 = files.iter().map(|file| file.size).sum();
        if total > ANSWER_LIMIT || !content.attachments.iter().all(|attachment| names.contains(attachment.stored_name.as_str())) {
            return None;
        }
        let key = MailCache::message_key(&location);
        Some((GetAnswer::Found { key, content, files }, paths))
    }

    /// Whether this device answers for `account`: signed in here and not removed from the group.
    fn serves(&self, account: &str, removed: &HashSet<String>) -> bool {
        self.accounts.record(account).is_some() && !removed.contains(account)
    }

    /// The account's mailboxes as stored here.
    async fn mailboxes(&self, account: &str) -> Vec<String> {
        self.cache.load_mailboxes(account).await.unwrap_or_default().into_iter().map(|mailbox| mailbox.id).collect()
    }

    /// The accounts the group removed, read again once the group changed.
    async fn removed(&self, link: &Link) -> Arc<HashSet<String>> {
        let changes = link.changes();
        if let Some((at, removed)) = &*self.removed.lock().expect("peer mail removed")
            && *at == changes
        {
            return removed.clone();
        }
        let removed: HashSet<String> = match link.snapshot().await {
            Ok(snapshot) => snapshot.accounts.into_iter().filter(|(_, record)| record.value.is_none()).map(|(id, _)| id).collect(),
            Err(_) => HashSet::new(),
        };
        let removed = Arc::new(removed);
        *self.removed.lock().expect("peer mail removed") = Some((changes, removed.clone()));
        removed
    }
}

impl Service for PeerMail {
    fn alpn(&self) -> &'static [u8] {
        ALPN
    }

    fn serve(&self, connection: Connection, link: Link) -> BoxFuture<'static, ()> {
        match self.this.upgrade() {
            Some(this) => this.serve_connection(connection, link).boxed(),
            None => async {}.boxed(),
        }
    }

    fn online(&self, link: Option<Link>) {
        let mut client = self.client();
        let Some(link) = link else {
            client.link = None;
            client.dialing.clear();
            client.failed.clear();
            for (_, peer) in client.peers.drain() {
                peer.connection.close(VarInt::from_u32(0), b"");
            }
            return;
        };
        client.link = Some(link.clone());
        // The network may have changed: dial again whoever isn't connected, so a reader's load
        // finds them connected.
        client.failed.clear();
        drop(client);
        let Some(this) = self.this.upgrade() else { return };
        tokio::spawn(async move {
            let dials = link.members().into_iter().map(|id| this.peer(&link, id, true));
            futures::future::join_all(dials).await;
        });
    }
}

/// Where `wanted` may be stored: under the mailbox asked for first, then the account's others.
fn locations<'a>(wanted: &'a Wanted, mailboxes: &'a [String]) -> impl Iterator<Item = MessageRef> + 'a {
    std::iter::once(wanted.mailbox.as_str())
        .chain(mailboxes.iter().map(String::as_str).filter(move |mailbox| *mailbox != wanted.mailbox))
        .map(|mailbox| wanted.in_mailbox(mailbox))
}

/// Asks whether the device of `peer` has `wanted`; `batch`: with the other `Have`s of the next
/// [`BATCH_WINDOW`].
async fn have(peer: &Peer, wanted: Wanted, batch: bool) -> bool {
    if !batch {
        return ask_have(&peer.connection, vec![wanted]).await.is_some_and(|held| held.first() == Some(&true));
    }
    let (sender, answer) = oneshot::channel();
    let first = {
        let mut queue = peer.batch.lock().expect("peer mail batch");
        queue.push((wanted, sender));
        queue.len() == 1
    };
    if first {
        let peer = peer.clone();
        tokio::spawn(async move {
            tokio::time::sleep(BATCH_WINDOW).await;
            let mut queued = std::mem::take(&mut *peer.batch.lock().expect("peer mail batch"));
            while !queued.is_empty() {
                let rest = queued.split_off(queued.len().min(HAVE_LIMIT));
                let (wanted, senders): (Vec<_>, Vec<_>) = queued.into_iter().unzip();
                let held = ask_have(&peer.connection, wanted).await.unwrap_or_default();
                for (index, sender) in senders.into_iter().enumerate() {
                    let _ = sender.send(held.get(index).copied().unwrap_or(false));
                }
                queued = rest;
            }
        });
    }
    answer.await.unwrap_or(false)
}

async fn ask_have(connection: &Connection, messages: Vec<Wanted>) -> Option<Vec<bool>> {
    let (mut send, mut recv) = connection.open_bi().await.ok()?;
    let request = Request::Have { version: CONTENT_VERSION, messages };
    send.write_all(&serde_json::to_vec(&request).ok()?).await.ok()?;
    send.finish().ok()?;
    serde_json::from_slice(&recv.read_to_end(HAVE_ANSWER_LIMIT).await.ok()?).ok()
}

/// Writes `answer`'s length and itself, then the files at `paths`.
async fn send_answer(send: &mut SendStream, answer: &GetAnswer, paths: &[std::path::PathBuf]) -> Option<()> {
    let header = serde_json::to_vec(answer).ok()?;
    send.write_all(&u32::try_from(header.len()).ok()?.to_be_bytes()).await.ok()?;
    send.write_all(&header).await.ok()?;
    let GetAnswer::Found { files, .. } = answer else { return Some(()) };
    for (file, path) in files.iter().zip(paths) {
        let data = tokio::fs::read(path).await.ok()?;
        if data.len() as u64 != file.size {
            // Changed since it was listed: the asking device drops the answer.
            send.reset(VarInt::from_u32(1)).ok()?;
            return None;
        }
        send.write_all(&data).await.ok()?;
    }
    Some(())
}

/// A [`MailCache::message_key`]: 32 lowercase hex digits.
fn is_key(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// A file name of a message's folder, which the asking device writes into its own.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}
