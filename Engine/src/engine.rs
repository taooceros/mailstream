//! # Engine
//!
//! The C entry point Mailstream calls in place of spawning `himalaya`. A request is the CLI's
//! argument list as a JSON array of strings; its result is what the CLI would have printed on
//! stdout, with the exit status beside it. A request that starts with `chat` goes to
//! [`crate::chat`] instead, and answers with JSON the same way.
//!
//! Each request runs on a thread of its own and answers through a callback, so the caller never
//! waits. The shared commands (mailbox, envelope, flag, message, attachment) run over a pooled
//! session: an account's open client, reused while it is warm, so a command costs its protocol
//! round-trips instead of a TCP, TLS and login handshake. A request that fails on a reused
//! session runs once more on a fresh one, unless it may have sent or stored a message.

use std::{
    collections::HashMap,
    ffi::c_void,
    fs, iter,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    slice,
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Result, anyhow};
use clap::Parser;
use pimalaya_cli::error::ErrorReport;
use pimalaya_cli::printer::Printer;
use pimalaya_config::toml::TomlConfig;

use crate::shared::{client::EmailClient, message::cli::MessageCommand};
use crate::{
    account::context::Account,
    backend::Backend,
    cli::{self, Cli, Command},
    config::Config,
    sink::{self, SinkPrinter},
};

/// Called once per request with its exit status and output, from the request's thread. The
/// output is only valid during the call.
pub type Callback =
    extern "C" fn(context: *mut c_void, status: i32, output: *const u8, length: usize);

/// A session idle this long may have been dropped by the server, so it is replaced instead.
const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
/// A session this old is replaced, so its access token (read once, at connect) never expires
/// mid-use.
const AGE_LIMIT: Duration = Duration::from_secs(40 * 60);
/// Idle sessions kept per account; the caller bounds how many run at once.
const IDLE_CAPACITY: usize = 6;

/// Prepares the process for the engine. Sockets closed by the server must fail the write that
/// meets them, not raise `SIGPIPE` and end the app.
#[unsafe(no_mangle)]
pub extern "C" fn ms_engine_init() {
    // SAFETY: installs the ignore disposition for one signal; no handler code runs.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
}

/// Runs one request on its own thread and answers through `callback`.
///
/// # Safety
///
/// `request` points to `length` readable bytes for the duration of this call. `context` is
/// handed back to `callback` untouched, exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ms_engine_run(
    request: *const u8,
    length: usize,
    context: *mut c_void,
    callback: Callback,
) {
    // SAFETY: the caller guarantees `length` readable bytes at `request`.
    let request = unsafe { slice::from_raw_parts(request, length) }.to_vec();
    let context = Context(context);
    let spawned = thread::Builder::new()
        .name("mailstream-engine".into())
        .stack_size(8 << 20)
        .spawn(move || {
            let (status, output) = handle(&request);
            context.answer(callback, status, &output);
        });
    if let Err(err) = spawned {
        let (status, output) = failure(true, anyhow!("Start engine thread error: {err}"));
        context.answer(callback, status, &output);
    }
}

/// The caller's context pointer, carried to the request's thread and back.
#[derive(Clone, Copy)]
struct Context(*mut c_void);

// SAFETY: the engine never dereferences the pointer; it only hands it back to the caller's
// callback, which owns its thread-safety.
unsafe impl Send for Context {}

impl Context {
    fn answer(self, callback: Callback, status: i32, output: &[u8]) {
        callback(self.0, status, output.as_ptr(), output.len());
    }
}

fn handle(request: &[u8]) -> (i32, Vec<u8>) {
    let args: Vec<String> = match serde_json::from_slice(request) {
        Ok(args) => args,
        Err(err) => return failure(true, anyhow!("Invalid engine request: {err}")),
    };
    #[cfg(feature = "chat")]
    if args.first().is_some_and(|first| first == "chat") {
        return match panic::catch_unwind(|| crate::chat::run(&args[1..])) {
            Ok(Ok(output)) => (0, output),
            Ok(Err(err)) => failure(true, err),
            Err(panic) => failure(true, anyhow!("Engine panicked: {}", panic_message(panic))),
        };
    }
    match attempt(&args, true) {
        Attempt::Done(json, result) => finish(json, result),
        Attempt::Stale => match attempt(&args, false) {
            Attempt::Done(json, result) => finish(json, result),
            Attempt::Stale => unreachable!("a fresh session is never stale"),
        },
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

enum Attempt {
    Done(bool, Result<()>),
    /// Failed on a reused session and safe to repeat on a fresh one.
    Stale,
}

fn attempt(args: &[String], reuse: bool) -> Attempt {
    sink::take();
    let cli = match Cli::try_parse_from(iter::once("himalaya").chain(args.iter().map(String::as_str))) {
        Ok(cli) => cli,
        Err(err) => return Attempt::Done(true, Err(anyhow!(err.to_string()))),
    };
    let json = cli.json.enabled;
    let mut printer = SinkPrinter { json };
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| execute(cli, &mut printer, reuse)));
    match outcome {
        Ok(Outcome { result: Err(_), reused: true, repeatable: true }) => Attempt::Stale,
        Ok(Outcome { result, .. }) => Attempt::Done(json, result),
        Err(panic) => Attempt::Done(json, Err(anyhow!("Engine panicked: {}", panic_message(panic)))),
    }
}

fn finish(json: bool, result: Result<()>) -> (i32, Vec<u8>) {
    match result {
        Ok(()) => (0, sink::take()),
        Err(err) => failure(json, err),
    }
}

/// The CLI's error report, JSON or text like its output.
fn failure(json: bool, err: anyhow::Error) -> (i32, Vec<u8>) {
    sink::take();
    let _ = SinkPrinter { json }.out(ErrorReport::from(err));
    (1, sink::take())
}

struct Outcome {
    result: Result<()>,
    /// Whether the command ran on a pooled session rather than one opened for it.
    reused: bool,
    /// Whether running the command again can't send or store anything twice.
    repeatable: bool,
}

impl From<Result<()>> for Outcome {
    fn from(result: Result<()>) -> Self {
        Self { result, reused: false, repeatable: false }
    }
}

fn execute(cli: Cli, printer: &mut SinkPrinter, reuse: bool) -> Outcome {
    let Some(cmd) = cli.cmd else {
        return Outcome::from(Err(anyhow!("No command given")));
    };
    let paths = cli.config.paths;
    let name = cli.account.name;
    let backend = cli.backend;
    let pooled = Pooled { paths: &paths, name: name.as_deref(), backend, reuse };
    match cmd {
        Command::Mailbox(cmd) => pooled.run(printer, true, |p, a, c| cmd.execute(p, a, c)),
        Command::Envelope(cmd) => pooled.run(printer, true, |p, a, c| cmd.execute(p, a, c)),
        Command::Flag(cmd) => pooled.run(printer, true, |p, a, c| cmd.execute(p, a, c)),
        Command::Attachment(cmd) => pooled.run(printer, true, |p, a, c| cmd.execute(p, a, c)),
        Command::Message(cmd) => {
            let repeatable = repeatable(&cmd);
            pooled.run(printer, repeatable, |p, a, c| cmd.execute(p, a, c))
        }
        cmd => Outcome::from(cmd.execute(printer, &paths, name.as_deref(), backend)),
    }
}

/// Whether a message command only reads: no send, no saved or appended copy.
fn repeatable(cmd: &MessageCommand) -> bool {
    match cmd {
        MessageCommand::Compose(cmd) => !cmd.send && cmd.save.is_none(),
        MessageCommand::Reply(cmd) => !cmd.send && cmd.save.is_none(),
        MessageCommand::Forward(cmd) => !cmd.send && cmd.save.is_none(),
        MessageCommand::Send(_) => false,
        MessageCommand::Add(_) | MessageCommand::Copy(_) => false,
        MessageCommand::Delete(_) | MessageCommand::Move(_) | MessageCommand::Read(_) => true,
    }
}

// MARK: - Session pool

/// One account's open client, as `EmailClient::new` built it.
struct Session {
    account: Account,
    client: EmailClient,
    /// The configuration file's state the session was built from.
    stamp: Option<SystemTime>,
    opened: Instant,
    used: Instant,
}

impl Session {
    fn fresh(&self, now: Instant, stamp: Option<SystemTime>) -> bool {
        self.stamp == stamp
            && now.duration_since(self.used) < IDLE_LIMIT
            && now.duration_since(self.opened) < AGE_LIMIT
    }
}

static IDLE: Mutex<Option<HashMap<String, Vec<Session>>>> = Mutex::new(None);

/// Where a shared command's session comes from.
struct Pooled<'a> {
    paths: &'a [PathBuf],
    name: Option<&'a str>,
    backend: Backend,
    reuse: bool,
}

impl Pooled<'_> {
    fn key(&self) -> String {
        format!("{:?}\u{1}{}\u{1}{:?}", self.paths, self.name.unwrap_or_default(), self.backend)
    }

    /// Runs `command` on a pooled session, or one opened for it, and pools the session again
    /// when the command succeeded.
    fn run(
        self,
        printer: &mut SinkPrinter,
        repeatable: bool,
        command: impl FnOnce(&mut SinkPrinter, &mut Account, &mut EmailClient) -> Result<()>,
    ) -> Outcome {
        let key = self.key();
        let stamp = config_stamp(self.paths);
        let pooled = if self.reuse { checkout(&key, stamp) } else { None };
        let reused = pooled.is_some();
        let mut session = match pooled.map_or_else(|| self.open(printer, stamp), Ok) {
            Ok(session) => session,
            Err(err) => return Outcome { result: Err(err), reused, repeatable },
        };
        let result = command(printer, &mut session.account, &mut session.client);
        if result.is_ok() {
            session.used = Instant::now();
            checkin(key, session);
        }
        Outcome { result, reused, repeatable }
    }

    fn open(&self, printer: &mut SinkPrinter, stamp: Option<SystemTime>) -> Result<Session> {
        let (config, name, account_config) = cli::resolve_account(printer, self.paths, self.name)?;
        let (account, client) = EmailClient::new(config, &name, account_config, self.backend)?;
        let now = Instant::now();
        Ok(Session { account, client, stamp, opened: now, used: now })
    }
}

/// The modification time of the configuration in effect, so an edited file retires the
/// sessions built from the old one.
fn config_stamp(paths: &[PathBuf]) -> Option<SystemTime> {
    let path = Config::target_path(paths).ok()?;
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn checkout(key: &str, stamp: Option<SystemTime>) -> Option<Session> {
    let now = Instant::now();
    let mut retired = Vec::new();
    let found = {
        let mut idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        let sessions = idle.get_or_insert_with(HashMap::new).get_mut(key)?;
        let mut found = None;
        while let Some(session) = sessions.pop() {
            if session.fresh(now, stamp) {
                found = Some(session);
                break;
            }
            retired.push(session);
        }
        found
    };
    // NOTE: closing connections can block, so stale sessions drop outside the lock.
    drop(retired);
    found
}

fn checkin(key: String, session: Session) {
    let surplus = {
        let mut idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        let sessions = idle.get_or_insert_with(HashMap::new).entry(key).or_default();
        if sessions.len() < IDLE_CAPACITY {
            sessions.push(session);
            None
        } else {
            Some(session)
        }
    };
    drop(surplus);
}
