//! Compares the Swift and the Rust engine through the path front ends use (`Mailstream::new`,
//! then commands through `mailstream::call`): time from start to the first answer, each
//! command's latency after warm-up (median, p99) and throughput (one at a time, and 32 in
//! flight), and memory. `CONFIG=release ./scripts/ffi.sh` first, then
//!
//! ```sh
//! cd FFI && MAILSTREAM_KIT_CONFIG=release cargo run --release --example bench -- [options]
//! ```
//!
//! `--rounds N` (3), `--engines swift,rust`, `--cases config.list,…` (default: every case),
//! `--scale F` (iterations × F; 1), `--out FILE` (every result as JSON lines).
//!
//! Each engine runs in a process of its own (the engine is chosen once per process), started
//! from this one with a fresh home folder holding the same settings.json, accounts.json and
//! mail.db, and a Keychain item of the run's own (`MAILSTREAM_KEYCHAIN_SERVICE`), so the
//! person's mail, settings and sign-ins are never touched. The accounts have no refresh token,
//! so no engine reaches a provider: the mail cases read what mail.db holds. Rounds alternate
//! which engine goes first. Results are lines `BENCH {json}`, one kind of line for every phase:
//! a command ported later gets a row in `CASES` (with data in `seed_home` if it needs some) and
//! is measured the same way. Each case's answer is compared across the engines too (paths into
//! the home folder aside), and the report lists the cases whose answers differ.
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures_executor::block_on;
use mailstream::{Change, EventListener, Mailstream, call, run_main_loop};
use serde_json::{Value, json};

/// One command as measured: its arguments for the `i`th call (writes alternate values so each
/// call changes the file), and how many calls to time.
struct Case {
    name: &'static str,
    command: &'static str,
    args: fn(usize) -> Value,
    /// Writes a file: fewer iterations.
    write: bool,
}

const SETTING_KEYS: [&str; 4] =
    ["sync.pollSeconds", "remoteContent.trustedSenders", "aiTagging.model", "chat.pollSeconds"];

const CASES: &[Case] = &[
    Case { name: "config.list", command: "config.list", args: |_| json!({}), write: false },
    Case { name: "config.get", command: "config.get", args: |i| json!({ "key": SETTING_KEYS[i % 4] }), write: false },
    Case { name: "config.schema", command: "config.schema", args: |_| json!({}), write: false },
    Case { name: "config.path", command: "config.path", args: |_| json!({}), write: false },
    Case { name: "accounts.list", command: "accounts.list", args: |_| json!({}), write: false },
    Case {
        name: "accounts.client",
        command: "accounts.client",
        args: |i| json!({ "provider": (["google", "microsoft"][i % 2]) }),
        write: false,
    },
    Case {
        name: "config.set",
        command: "config.set",
        args: |i| json!({ "key": "sync.pollSeconds", "value": (120 + i % 2).to_string() }),
        write: true,
    },
    Case {
        name: "accounts.rename",
        command: "accounts.rename",
        args: |i| json!({ "account": "bench0@gmail.com", "label": (["Bench A", "Bench B"][i % 2]) }),
        write: true,
    },
    Case { name: "mailboxes", command: "mailboxes", args: |_| json!({}), write: false },
    // All Inboxes of the five accounts, threaded with their sent mail, from mail.db.
    Case { name: "mail.list", command: "mail.list", args: |_| json!({ "limit": 50, "cached": true }), write: false },
    Case {
        name: "mail.list.page",
        command: "mail.list",
        args: |i| json!({ "limit": 50, "cached": true, "cursor": page_cursor(i) }),
        write: false,
    },
    Case {
        name: "mail.list.filter",
        command: "mail.list",
        args: |i| {
            json!({ "account": bench_account(3 + i % 2).0, "mailbox": "inbox", "unread": true, "limit": 25, "cached": true })
        },
        write: false,
    },
    Case { name: "mail.show", command: "mail.show", args: |i| json!({ "id": conversation_id(i) }), write: false },
    Case {
        name: "mail.previews",
        command: "mail.previews",
        args: |i| json!({ "ids": (0..20).map(|n| conversation_id(i + n * 5)).collect::<Vec<_>>() }),
        write: false,
    },
    // Starts a refresh and answers at once; with no refresh token the fetch stops at the token.
    Case { name: "refresh", command: "refresh", args: |_| json!({}), write: false },
    // The three Gmail accounts' chat spaces as the list shows them, from mail.db.
    Case { name: "chat.spaces", command: "chat.spaces", args: |_| json!({}), write: false },
    Case {
        name: "chat.spaces.account",
        command: "chat.spaces",
        args: |i| json!({ "account": bench_account(i % CHAT_ACCOUNTS).0 }),
        write: false,
    },
    Case { name: "chat.people", command: "chat.people", args: |i| json!({ "account": bench_account(i % CHAT_ACCOUNTS).0 }), write: false },
    // An opened space's history (150 messages from mail.db), as the reader re-reads it.
    Case {
        name: "chat.messages",
        command: "chat.messages",
        args: |i| json!({ "account": bench_account(0).0, "space": chat_space(i % CHAT_RECENT) }),
        write: false,
    },
];

/// Bench account `n` (0 to 4) and its provider: three Gmail, two Outlook.
fn bench_account(n: usize) -> (String, &'static str) {
    if n < 3 { (format!("bench{n}@gmail.com"), "google") } else { (format!("bench{n}@outlook.com"), "microsoft") }
}

const ACCOUNTS: usize = 5;
/// Envelopes in each account's inbox and sent listings in mail.db.
const INBOX: usize = 500;
const SENT: usize = 100;
/// Inbox messages, newest first, whose content mail.db holds (and every sent one's).
const CACHED_CONTENT: usize = 60;

/// The id of a conversation whose messages' content is cached: inbox message `i / 5 % 60` of
/// account `i % 5`, the oldest message of its thread.
fn conversation_id(i: usize) -> String {
    let (account, _) = bench_account(i % ACCOUNTS);
    // Message 49 is a repeated alert, merged into the older alerts' row (another id): skip it.
    let n = i / ACCOUNTS % (CACHED_CONTENT - 1);
    format!("{account}|{}", message_id(i % ACCOUNTS, if n >= 49 { n + 1 } else { n }))
}

fn message_id(account: usize, n: usize) -> String {
    format!("m{n}.a{account}@bench")
}

/// Seconds since 2001 (as Swift keeps dates) of account `a`'s inbox message `n`: every three
/// hours, back from late October 2026, a minute apart between accounts so no two conversations
/// tie (the Swift engine orders ties by dictionary order, which changes from run to run).
fn inbox_date(a: usize, n: usize) -> f64 {
    815_000_000.0 - (n as f64) * 10_800.0 - (a as f64) * 60.0
}

/// A cursor somewhere in the second to fifth page of All Inboxes: after inbox message
/// `50 + i % 4 * 50` of bench0.
fn page_cursor(i: usize) -> String {
    let n = 50 + i % 4 * 50;
    // Unix milliseconds, then the conversation id.
    let millis = ((inbox_date(0, n) + 978_307_200.0) * 1000.0).round() as i64;
    format!("{millis}|{}|{}", bench_account(0).0, message_id(0, n))
}

/// The Gmail accounts, which are chat accounts too.
const CHAT_ACCOUNTS: usize = 3;
/// Spaces in each chat account's list in mail.db.
const CHAT_SPACES: usize = 60;
/// Spaces of bench0 whose messages mail.db keeps, and how many each.
const CHAT_RECENT: usize = 4;
const CHAT_MESSAGES: usize = 150;

fn chat_space(n: usize) -> String {
    format!("spaces/AAAA{n:03}")
}

/// A Chat user: the account's own `users/0`, else person `n` of 40.
fn chat_user(n: usize) -> Value {
    if n == 0 {
        return json!({ "name": "users/0", "bot": false });
    }
    json!({
        "name": format!("users/{n}"), "displayName": format!("Person {n}"), "email": format!("person{n}@example.com"),
        "avatarUrl": format!("https://lh3.googleusercontent.com/a/p{n}=s64"), "bot": false,
    })
}

/// In-flight calls for the concurrent throughput.
const CONCURRENCY: usize = 32;
const KEYCHAIN_PREFIX: &str = "dev.mailstream.bench.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        child(&args[1..]);
    } else {
        parent(&args);
    }
}

// MARK: - Parent: data, processes, report

struct Options {
    rounds: usize,
    engines: Vec<String>,
    cases: Vec<String>,
    scale: f64,
    out: Option<PathBuf>,
}

fn options(args: &[String]) -> Options {
    let mut options = Options {
        rounds: 3,
        engines: vec!["swift".into(), "rust".into()],
        cases: CASES.iter().map(|case| case.name.to_string()).collect(),
        scale: 1.0,
        out: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(&format!("{arg} needs a value"))).clone();
        match arg.as_str() {
            "--rounds" => options.rounds = value().parse().unwrap_or_else(|_| fail("--rounds takes a number")),
            "--engines" => options.engines = value().split(',').map(String::from).collect(),
            "--cases" => options.cases = value().split(',').map(String::from).collect(),
            "--scale" => options.scale = value().parse().unwrap_or_else(|_| fail("--scale takes a number")),
            "--out" => options.out = Some(value().into()),
            _ => fail(&format!("unknown option {arg}; see the header of FFI/examples/bench.rs")),
        }
    }
    if let Some(unknown) = options.engines.iter().find(|engine| !["swift", "rust"].contains(&engine.as_str())) {
        fail(&format!("unknown engine {unknown}"));
    }
    if let Some(unknown) = options.cases.iter().find(|name| !CASES.iter().any(|case| case.name == *name)) {
        fail(&format!("unknown case {unknown}"));
    }
    options
}

fn fail(message: &str) -> ! {
    eprintln!("bench: {message}");
    std::process::exit(2)
}

/// The run's temporary folders and Keychain item, removed when it ends.
struct Scratch {
    root: PathBuf,
    service: String,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = security_framework::passwords::delete_generic_password(&self.service, "refresh-tokens");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn parent(args: &[String]) {
    let options = options(args);
    let engine = engine_library();
    println!("engine library: {} (built {})", engine.display(), modified(&engine));
    let root = std::env::temp_dir().join(format!("mailstream-bench-{}", std::process::id()));
    let scratch = Scratch { root, service: format!("{KEYCHAIN_PREFIX}{}", std::process::id()) };
    seed_keychain(&scratch.service);
    let executable = std::env::current_exe().expect("this program's path");
    let mut lines: Vec<Value> = Vec::new();
    for round in 1..=options.rounds {
        let mut order = options.engines.clone();
        if round % 2 == 0 {
            order.reverse();
        }
        for engine in order {
            let home = scratch.root.join(format!("{round}-{engine}"));
            seed_home(&home);
            let load = load_average();
            eprintln!("round {round}: {engine} (load {load:.2})");
            let mut command = Command::new(&executable);
            command
                .args(["--child", &engine, &round.to_string(), &options.scale.to_string(), &options.cases.join(",")])
                .env("HOME", &home)
                .env("CFFIXED_USER_HOME", &home)
                .env("MAILSTREAM_KEYCHAIN_SERVICE", &scratch.service)
                .env_remove("MAILSTREAM_ENGINE")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if engine == "rust" {
                command.env("MAILSTREAM_ENGINE", "rust");
            }
            let output = command.output().expect("the engine process starts");
            if !output.status.success() {
                fail(&format!("the {engine} run failed ({})", output.status));
            }
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some(json) = line.strip_prefix("BENCH ") {
                    let mut value: Value = serde_json::from_str(json).expect("the child writes JSON");
                    value["load"] = json!((load * 100.0).round() / 100.0);
                    lines.push(value);
                }
            }
        }
    }
    if let Some(out) = &options.out {
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(out, text).unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", out.display())));
    }
    report(&lines, &options);
}

/// The `libMailstreamKit.dylib` this process loaded: it must be the one this checkout built.
fn engine_library() -> PathBuf {
    unsafe extern "C" {
        fn mailstream_start();
    }
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::dladdr(mailstream_start as *const c_void, &mut info) };
    if found == 0 || info.dli_fname.is_null() {
        fail("cannot tell which libMailstreamKit.dylib is loaded");
    }
    let path = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned());
    let path = path.canonicalize().unwrap_or(path);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").canonicalize().expect("the repository exists");
    if !path.starts_with(repository.join(".build")) {
        fail(&format!("loaded {} instead of this checkout's .build engine", path.display()));
    }
    path
}

fn modified(path: &Path) -> String {
    let age =
        std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok().and_then(|time| time.elapsed().ok());
    age.map_or_else(|| "at an unknown time".into(), |age| format!("{} min ago", age.as_secs() / 60))
}

/// A sign-ins item with the clients last used for each provider and no refresh tokens, so no
/// engine asks a provider for a token. Created by this program, which its child processes are,
/// so reading it asks nobody.
fn seed_keychain(service: &str) {
    let contents = br#"{"clients":{},"lastClients":{"google":{"id":"bench-google.apps.googleusercontent.com","secret":"bench"},"microsoft":{"id":"00000000-0000-0000-0000-000000000000"}},"refreshTokens":{}}"#;
    let mut options = security_framework::passwords::PasswordOptions::new_generic_password(service, "refresh-tokens");
    options.set_label("Mailstream sign-ins (bench)");
    security_framework::passwords::set_generic_password_options(contents, options)
        .unwrap_or_else(|error| fail(&format!("cannot create the bench Keychain item: {error}")));
}

/// The same files for every engine and round: a few changed settings (AI tagging off, so no
/// engine runs omp), five accounts, and their mail in mail.db.
fn seed_home(home: &Path) {
    let support = home.join("Library/Application Support/Mailstream");
    std::fs::create_dir_all(&support).expect("the bench home is writable");
    let senders: Vec<String> = (0..50).map(|n| format!("sender{n:02}@example.com")).collect();
    let settings = json!({
        "aiTagging.enabled": false,
        "remoteContent.trustedSenders": senders,
        "sync.pollSeconds": 120,
    });
    std::fs::write(support.join("settings.json"), format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()))
        .unwrap();
    let google = [
        "openid",
        "email",
        "profile",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/chat.messages.readonly",
    ];
    let microsoft = ["openid", "email", "profile", "offline_access", "https://graph.microsoft.com/Mail.ReadWrite"];
    let accounts: Vec<Value> = (0..ACCOUNTS)
        .map(|n| {
            let (id, provider) = bench_account(n);
            let scopes = if provider == "google" { &google[..] } else { &microsoft[..] };
            json!({ "id": id, "provider": provider, "label": id, "displayName": format!("Bench {n}"), "scopes": scopes })
        })
        .collect();
    std::fs::write(support.join("accounts.json"), serde_json::to_string_pretty(&accounts).unwrap()).unwrap();
    seed_mail(&support.join("mail.db"), &home.join("Library/Caches/dev.mailstream.Mailstream"));
}

/// mail.db as both engines keep it (`scripts/mail-db-interop.sh` checks they agree), written
/// through the Rust engine's cache layer: each account's mailboxes, an inbox of `INBOX` envelopes
/// from 80 senders (a few of them a repeated alert, which the inbox shows as one row), a sent box
/// of `SENT` replies threading into the inbox, and the content of the newest `CACHED_CONTENT`
/// inbox messages and every sent one.
fn seed_mail(database: &Path, caches: &Path) {
    use mailstream_engine::cache::{CachedListing, MailCache};
    use mailstream_engine::model::{Account, Envelope, Mailbox, MailboxKey, MessageContent, MessageRef};
    fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).expect("bench data has the engine's shape")
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let cache = MailCache::new(database.to_path_buf(), caches.to_path_buf());
        let accounts: Vec<Account> = (0..ACCOUNTS)
            .map(|a| {
                let (id, provider) = bench_account(a);
                parse(json!({ "name": id, "label": id, "provider": provider, "identity": { "name": format!("Bench {a}"), "email": id } }))
            })
            .collect();
        cache.save_accounts(&accounts);
        for a in 0..ACCOUNTS {
            let (account, provider) = bench_account(a);
            let (inbox, sent) = if provider == "google" { ("INBOX", "SENT") } else { ("AAMk-inbox", "AAMk-sent") };
            let mut boxes = vec![
                json!({ "account": account, "id": inbox, "name": "Inbox", "role": "inbox", "unread": 40, "total": INBOX }),
                json!({ "account": account, "id": sent, "name": "Sent", "role": "sent" }),
                json!({ "account": account, "id": "drafts", "name": "Drafts", "role": "drafts" }),
                json!({ "account": account, "id": "junk", "name": "Spam", "role": "junk" }),
                json!({ "account": account, "id": "trash", "name": "Trash", "role": "trash" }),
            ];
            boxes.extend((0..8).map(|n| json!({ "account": account, "id": format!("label{n}"), "name": format!("Project {n}"), "role": "other" })));
            let boxes: Vec<Mailbox> = boxes.into_iter().map(parse).collect();
            cache.save_mailboxes(&boxes, &account);
            let me = json!({ "name": format!("Bench {a}"), "email": account });
            let sender = |n: usize| {
                if n % 50 == 49 {
                    json!({ "name": "Alerts", "email": "alerts@example.org" })
                } else {
                    json!({ "name": format!("Sender {}", n % 80), "email": format!("sender{:02}@example.com", n % 80) })
                }
            };
            let subject = |n: usize| if n % 50 == 49 { "Security alert".to_string() } else { format!("Project update {n}") };
            let envelopes: Vec<Arc<Envelope>> = (0..INBOX)
                .map(|n| {
                    let flags: Vec<&str> = match n % 12 {
                        0 => vec![],
                        5 => vec!["seen", "flagged"],
                        _ => vec!["seen"],
                    };
                    Arc::new(parse(json!({
                        "id": format!("in{n}.a{a}"), "thread": format!("t{n}.a{a}"), "messageID": message_id(a, n),
                        "inReplyTo": [], "flags": flags, "subject": subject(n), "from": [sender(n)], "to": [me],
                        "date": inbox_date(a, n), "size": 4000 + n, "hasAttachment": n % 7 == 0,
                    })))
                })
                .collect();
            let next = if provider == "google" { "page2".to_string() } else { format!("https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages?$skip={INBOX}") };
            cache.save_listing(CachedListing { envelopes, next: Some(next) }, &MailboxKey::new(&account, inbox));
            let replies: Vec<Arc<Envelope>> = (0..SENT)
                .map(|n| {
                    let parent = n * 3;
                    Arc::new(parse(json!({
                        "id": format!("out{n}.a{a}"), "thread": format!("t{parent}.a{a}"), "messageID": format!("r{n}.a{a}@bench"),
                        "inReplyTo": [message_id(a, parent)], "flags": ["seen"], "subject": format!("Re: {}", subject(parent)),
                        "from": [me], "to": [sender(parent)], "date": inbox_date(a, parent) + 1800.0, "size": 2000,
                        "hasAttachment": false,
                    })))
                })
                .collect();
            cache.save_listing(CachedListing { envelopes: replies, next: None }, &MailboxKey::new(&account, sent));
            let content = |n: usize, reply: bool| -> MessageContent {
                let html = format!(
                    "<p>Hello from message {n}.</p><p><img src=\"https://images.example.com/{n}.png\"> and a <a href=\"https://example.com/{n}\">link</a></p>"
                );
                let text = if reply { format!("Thanks!\n\nOn Monday, someone wrote:\n> Hello from message {n}.") } else { format!("Hello from message {n}.\nAnd a link: https://example.com/{n}") };
                parse(json!({
                    "html": if reply { Value::Null } else { json!(html) }, "text": text, "cc": [], "replyTo": [], "references": [],
                    "attachments": [], "snippet": format!("Hello from message {n}."),
                }))
            };
            for n in 0..CACHED_CONTENT {
                let message = MessageRef { account: account.clone(), mailbox: inbox.into(), id: format!("in{n}.a{a}") };
                cache.save_content(&content(n, false), &message);
            }
            for n in 0..SENT {
                let message = MessageRef { account: account.clone(), mailbox: sent.into(), id: format!("out{n}.a{a}") };
                cache.save_content(&content(n * 3, true), &message);
            }
        }
        cache.flush().await;
        seed_chat(&cache);
        cache.flush().await;
    });
}

/// Each Gmail account's chat list in mail.db (`CHAT_SPACES` direct messages, group chats and
/// spaces, a quarter of them unread), and the messages of bench0's `CHAT_RECENT` most recently
/// opened spaces.
fn seed_chat(cache: &mailstream_engine::cache::MailCache) {
    use mailstream_engine::chat_model::{ChatAccount, ChatMessagePage, ChatSpaceKey, ChatSpaceList};
    fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).expect("bench data has the engine's shape")
    }
    let accounts: Vec<ChatAccount> = (0..CHAT_ACCOUNTS)
        .map(|a| parse(json!({ "name": bench_account(a).0, "email": bench_account(a).0 })))
        .collect();
    cache.save_chat_accounts(&accounts);
    let message = |space: &str, n: usize, sender: usize, date: f64| {
        json!({
            "name": format!("{space}/messages/m{n}"), "sender": chat_user(sender), "created": date,
            "text": format!("Message {n} with *some* <https://example.com/{n}|markup>"), "thread": format!("{space}/threads/t{}", n / 3),
            "threadReply": n % 3 != 0, "mine": sender == 0, "card": false,
            "attachments": if n % 10 == 0 { json!([{ "name": format!("file{n}.png"), "contentType": "image/png", "resource": format!("r/{n}") }]) } else { json!([]) },
        })
    };
    for a in 0..CHAT_ACCOUNTS {
        let account = bench_account(a).0;
        let spaces: Vec<Value> = (0..CHAT_SPACES)
            .map(|n| {
                let name = chat_space(n);
                let active = 815_000_000.0 - (n as f64) * 7_200.0 - (a as f64) * 60.0;
                let (kind, title, members) = match n % 3 {
                    0 => ("dm", format!("Person {}", n % 40 + 1), vec![chat_user(n % 40 + 1)]),
                    1 => ("group", format!("Person {}, Person {}", n % 40 + 1, (n + 7) % 40 + 1), vec![chat_user(n % 40 + 1), chat_user((n + 7) % 40 + 1)]),
                    _ => ("space", format!("Project {n}"), vec![]),
                };
                let sender = if n % 5 == 0 { 0 } else { n % 40 + 1 };
                json!({
                    "name": name, "kind": kind, "title": title, "members": members, "lastActive": active,
                    "lastRead": active - if n % 4 == 0 { 600.0 } else { 0.0 }, "unread": if n % 4 == 0 { 2 } else { 0 },
                    "latest": message(&name, 1000, sender, active),
                })
            })
            .collect();
        let list: ChatSpaceList = parse(json!({ "me": "users/0", "more": true, "spaces": spaces }));
        cache.save_chat_spaces(&list, &account);
    }
    let account = bench_account(0).0;
    let recent: Vec<ChatSpaceKey> =
        (0..CHAT_RECENT).map(|n| ChatSpaceKey { account: account.clone(), space: chat_space(n) }).collect();
    cache.save_chat_recent(&recent);
    for key in &recent {
        let newest = 815_000_000.0;
        let messages: Vec<Value> = (0..CHAT_MESSAGES)
            .map(|n| message(&key.space, n, if n % 4 == 0 { 0 } else { n % 40 + 1 }, newest - ((CHAT_MESSAGES - n) as f64) * 300.0))
            .collect();
        let page: ChatMessagePage = parse(json!({ "messages": messages, "more": true, "lastRead": newest - 900.0 }));
        cache.save_chat_messages(&page, key);
    }
}

fn load_average() -> f64 {
    let mut loads = [0f64; 3];
    if unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) } < 1 { f64::NAN } else { loads[0] }
}

/// A table per round, then the medians over the rounds with Rust's time as a share of Swift's.
fn report(lines: &[Value], options: &Options) {
    fn lines_of<'a>(lines: &'a [Value], kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        lines.iter().filter(move |line| line["kind"] == kind)
    }
    let of = |kind: &'static str| lines_of(lines, kind);
    println!();
    println!(
        "{:<6} {:>5} {:<16} {:>10} {:>10} {:>10} {:>12} {:>12}",
        "engine", "round", "case", "median µs", "p99 µs", "mean µs", "seq calls/s", "×32 calls/s"
    );
    for line in of("case") {
        println!(
            "{:<6} {:>5} {:<16} {:>10.1} {:>10.1} {:>10.1} {:>12.0} {:>12.0}",
            str(&line["engine"]),
            line["round"],
            str(&line["case"]),
            num(&line["median_us"]),
            num(&line["p99_us"]),
            num(&line["mean_us"]),
            num(&line["sequential_per_s"]),
            num(&line["concurrent_per_s"]),
        );
    }
    println!();
    println!(
        "{:<6} {:>5} {:>7} {:>12} {:>12} {:>12} {:>12} {:>12} {:>7}",
        "engine", "round", "load", "new() µs", "first ms", "base MB", "ready MB", "end MB", "events"
    );
    for line in of("process") {
        println!(
            "{:<6} {:>5} {:>7.2} {:>12.1} {:>12.2} {:>12.1} {:>12.1} {:>12.1} {:>7}",
            str(&line["engine"]),
            line["round"],
            num(&line["load"]),
            num(&line["new_us"]),
            num(&line["first_answer_ms"]),
            num(&line["footprint_base_mb"]),
            num(&line["footprint_ready_mb"]),
            num(&line["footprint_end_mb"]),
            line["events"],
        );
    }
    if !(options.engines.contains(&"swift".to_string()) && options.engines.contains(&"rust".to_string())) {
        return;
    }
    let median_of = |kind: &str, engine: &str, case: Option<&str>, field: &str| {
        let mut values: Vec<f64> = lines_of(lines, kind)
            .filter(|line| line["engine"] == engine && case.is_none_or(|case| line["case"] == case))
            .map(|line| num(&line[field]))
            .collect();
        values.sort_by(f64::total_cmp);
        if values.is_empty() { f64::NAN } else { values[values.len() / 2] }
    };
    println!();
    println!("medians over {} rounds (rust/swift: below 1 means Rust is faster or smaller)", options.rounds);
    println!(
        "{:<16} {:>11} {:>11} {:>8} {:>11} {:>11} {:>8} {:>10} {:>10} {:>8}",
        "case", "swift med", "rust med", "ratio", "swift p99", "rust p99", "ratio", "swift ×32", "rust ×32", "ratio"
    );
    for name in &options.cases {
        let row =
            |field: &str| (median_of("case", "swift", Some(name), field), median_of("case", "rust", Some(name), field));
        let (sm, rm) = row("median_us");
        let (sp, rp) = row("p99_us");
        let (sc, rc) = row("concurrent_per_s");
        println!(
            "{name:<16} {sm:>11.1} {rm:>11.1} {:>8.2} {sp:>11.1} {rp:>11.1} {:>8.2} {sc:>10.0} {rc:>10.0} {:>8.2}",
            rm / sm,
            rp / sp,
            sc / rc
        );
    }
    for (label, field) in [
        ("first answer ms", "first_answer_ms"),
        ("ready MB", "footprint_ready_mb"),
        ("end MB", "footprint_end_mb"),
        ("peak MB", "footprint_peak_mb"),
    ] {
        let (swift, rust) = (median_of("process", "swift", None, field), median_of("process", "rust", None, field));
        println!("{label:<16} {swift:>11.2} {rust:>11.2} {:>8.2}", rust / swift);
    }
    println!();
    let mut differing = 0;
    for name in &options.cases {
        let answer = |engine: &str| {
            lines_of(lines, "sample").find(|line| line["engine"] == engine && line["case"] == name.as_str()).map(|line| &line["answer"])
        };
        if let (Some(swift), Some(rust)) = (answer("swift"), answer("rust"))
            && let Some(difference) = difference(swift, rust, "")
        {
            differing += 1;
            println!("answers differ: {name}: {difference}");
        }
    }
    if differing == 0 {
        println!("answers: the same from both engines for every case");
    }
}

/// Where `a` and `b` first differ, as a JSON path with both values; nil when they are equal.
fn difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|key| {
                difference(x.get(key).unwrap_or(&Value::Null), y.get(key).unwrap_or(&Value::Null), &format!("{path}.{key}"))
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).enumerate().find_map(|(index, (x, y))| difference(x, y, &format!("{path}[{index}]")))
        }
        _ if a == b => None,
        _ => {
            let short = |value: &Value| value.to_string().chars().take(160).collect::<String>();
            Some(format!("{path}: swift {} / rust {}", short(a), short(b)))
        }
    }
}

fn num(value: &Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

fn str(value: &Value) -> &str {
    value.as_str().unwrap_or("?")
}

// MARK: - Child: one engine, measured

struct Count(Arc<AtomicU64>);

impl EventListener for Count {
    fn on_event(&self, _change: Change) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn child(args: &[String]) {
    let [engine, round, scale, cases] = args else { fail("--child takes engine, round, scale and cases") };
    let (engine, round, scale) = (engine.clone(), round.parse::<u64>().unwrap(), scale.parse::<f64>().unwrap());
    let cases: Vec<&'static Case> =
        cases.split(',').filter_map(|name| CASES.iter().find(|case| case.name == name)).collect();
    // Never run against the person's data: the parent sets all three.
    let home = std::env::var("HOME").unwrap_or_default();
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE").unwrap_or_default();
    if !home.contains("mailstream-bench-")
        || std::env::var("CFFIXED_USER_HOME").ok().as_deref() != Some(home.as_str())
        || !service.starts_with(KEYCHAIN_PREFIX)
        || (std::env::var("MAILSTREAM_ENGINE").as_deref() == Ok("rust")) != (engine == "rust")
    {
        fail(&format!("refusing to run outside a bench home ({home}, temp {temp}, service {service})"));
    }
    engine_library();
    std::thread::spawn(move || {
        let base = footprint();
        let start = Instant::now();
        let mail = Mailstream::new();
        let new_us = start.elapsed().as_secs_f64() * 1e6;
        let events = Arc::new(AtomicU64::new(0));
        mail.subscribe(Arc::new(Count(events.clone())));
        let accounts: Value =
            block_on(call("accounts.list", json!({}))).unwrap_or_else(|error| fail(&format!("{engine}: {error}")));
        let first_answer_ms = start.elapsed().as_secs_f64() * 1e3;
        let ready = footprint();
        // Writes go nowhere but the bench home.
        let path: Value = block_on(call("config.path", json!({}))).unwrap();
        if !path["path"].as_str().is_some_and(|path| path.starts_with(&home)) {
            fail(&format!("{engine} keeps settings at {path}, outside the bench home"));
        }
        if accounts.as_array().map(Vec::len) != Some(5) {
            fail(&format!("{engine} lists {accounts} instead of the 5 bench accounts"));
        }
        if cases.iter().any(|case| case.command.starts_with("mail")) {
            mail_ready(&engine);
        }
        if cases.iter().any(|case| case.command.starts_with("chat")) {
            chat_ready(&engine);
        }
        let canonical = std::fs::canonicalize(&home).map(|path| path.to_string_lossy().into_owned()).unwrap_or(home.clone());
        for case in cases {
            let line = measure(case, scale);
            let head = || json!({ "engine": engine, "round": round, "case": case.name, "command": case.command });
            let mut case_head = head();
            case_head["kind"] = json!("case");
            emit(case_head, line);
            // The answer, with paths into this home made alike, for the report to compare.
            let answer: Value = block_on(call(case.command, (case.args)(1))).unwrap();
            let text = answer.to_string().replace(&canonical, "~").replace(&home, "~");
            let mut sample_head = head();
            sample_head["kind"] = json!("sample");
            emit(sample_head, json!({ "answer": serde_json::from_str::<Value>(&text).unwrap() }));
        }
        let end = footprint();
        emit(
            json!({ "kind": "process", "engine": engine, "round": round }),
            json!({
                "new_us": new_us, "first_answer_ms": first_answer_ms,
                "footprint_base_mb": base.0, "footprint_ready_mb": ready.0, "footprint_end_mb": end.0, "footprint_peak_mb": end.1,
                "events": events.load(Ordering::Relaxed),
            }),
        );
        drop(mail);
        std::process::exit(0);
    });
    // The Swift engine works on the main run loop and both deliver events there, as in an app.
    run_main_loop();
}

/// Waits until the engine has painted every account's mailboxes from mail.db, then lists All
/// Inboxes once in full so `mail.show` and `mail.previews` know the conversation ids, as a front
/// end's list would have.
fn mail_ready(engine: &str) {
    let start = Instant::now();
    loop {
        let boxes: Value = block_on(call("mailboxes", json!({}))).unwrap_or_else(|error| fail(&format!("{engine}: {error}")));
        let painted = (0..ACCOUNTS).all(|a| boxes[bench_account(a).0.as_str()].as_array().is_some_and(|list| !list.is_empty()));
        if painted {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            fail(&format!("{engine} did not load the mailboxes from mail.db: {boxes}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Pages of at most 200, the engines' limit.
    let (mut count, mut cursor) = (0, Value::Null);
    loop {
        let page: Value = block_on(call("mail.list", json!({ "limit": 200, "cached": true, "cursor": cursor })))
            .unwrap_or_else(|error| fail(&format!("{engine}: {error}")));
        count += page["conversations"].as_array().map_or(0, Vec::len);
        cursor = page["next"].clone();
        if cursor.is_null() {
            break;
        }
    }
    if count < ACCOUNTS * CACHED_CONTENT {
        fail(&format!("{engine} lists {count} conversations from mail.db"));
    }
    eprintln!("{engine}: mail.db painted in {:.0} ms, {count} conversations", start.elapsed().as_secs_f64() * 1e3);
}

/// Waits until the engine lists every chat account's spaces from mail.db, then opens bench0's
/// recent spaces once, as the reader would, so their messages are loaded from mail.db (the
/// fetch from Google fails: there is no sign-in), and closes the reader again.
fn chat_ready(engine: &str) {
    let start = Instant::now();
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        while !done() {
            if start.elapsed() > Duration::from_secs(20) {
                fail(&format!("{engine} did not load {what} from mail.db"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let answer = |command: &str, args: Value| -> Value {
        block_on(call(command, args)).unwrap_or_else(|error| fail(&format!("{engine}: {command}: {error}")))
    };
    wait("the chat spaces", &|| {
        answer("chat.spaces", json!({}))["spaces"].as_array().map(Vec::len) == Some(CHAT_ACCOUNTS * CHAT_SPACES)
    });
    for n in 0..CHAT_RECENT {
        let args = json!({ "account": bench_account(0).0, "space": chat_space(n) });
        answer("chat.open", args.clone());
        wait("a recent space's messages", &|| {
            answer("chat.messages", args.clone())["messages"].as_array().map(Vec::len) == Some(CHAT_MESSAGES)
        });
    }
    answer("chat.close", json!({}));
    eprintln!("{engine}: chat painted in {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
}

/// Warm-up, then timed calls one at a time, then calls `CONCURRENCY` at a time.
fn measure(case: &Case, scale: f64) -> Value {
    let (warm, sequential, concurrent) = if case.write { (30, 300, 300) } else { (200, 2000, 4000) };
    let scaled = |count: usize| ((count as f64 * scale) as usize).max(10);
    let once = |i: usize| async move {
        let _: Value =
            call(case.command, (case.args)(i)).await.unwrap_or_else(|error| fail(&format!("{}: {error}", case.name)));
    };
    for i in 0..scaled(warm) {
        block_on(once(i));
    }
    let count = scaled(sequential);
    let mut times = Vec::with_capacity(count);
    let all = Instant::now();
    for i in 0..count {
        let started = Instant::now();
        block_on(once(i));
        times.push(started.elapsed());
    }
    let sequential_per_s = count as f64 / all.elapsed().as_secs_f64();
    let count = scaled(concurrent);
    let all = Instant::now();
    block_on(futures::stream::iter(0..count).map(once).buffer_unordered(CONCURRENCY).collect::<Vec<()>>());
    let concurrent_per_s = count as f64 / all.elapsed().as_secs_f64();
    times.sort();
    let us = |duration: Duration| duration.as_secs_f64() * 1e6;
    let p99 = times[(times.len() * 99).div_ceil(100) - 1];
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    json!({
        "n": times.len(), "median_us": us(times[times.len() / 2]), "p99_us": us(p99), "mean_us": us(mean),
        "max_us": us(*times.last().unwrap()), "sequential_per_s": sequential_per_s,
        "concurrent_per_s": concurrent_per_s, "concurrency": CONCURRENCY,
    })
}

fn emit(mut head: Value, body: Value) {
    let fields: BTreeMap<String, Value> = serde_json::from_value(body).unwrap();
    for (key, value) in fields {
        head[key] = value;
    }
    println!("BENCH {head}");
}

/// This process's physical footprint (what Activity Monitor shows as Memory) and its peak so
/// far, in MB.
fn footprint() -> (f64, f64) {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let pointer = (&raw mut info).cast::<libc::rusage_info_t>();
    if unsafe { libc::proc_pid_rusage(std::process::id() as i32, libc::RUSAGE_INFO_V4, pointer) } != 0 {
        return (f64::NAN, f64::NAN);
    }
    let mb = |bytes: u64| (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0;
    (mb(info.ri_phys_footprint), mb(info.ri_lifetime_max_phys_footprint))
}
