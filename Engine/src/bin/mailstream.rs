//! `mailstream`: Mailstream from a shell or an AI agent. `config …` works on settings.json
//! directly; every other command goes to the control socket of the running front end or
//! `mailstream daemon` (`mailstream_engine::control`), starting Mailstream GPUI in the background
//! when none runs and a desktop can show it. Commands, arguments, help and output are the command
//! table's (`mailstream_engine::commands`).
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mailstream_engine::commands::{self, COMMANDS, Handler, Kind, Spec};
use mailstream_engine::sync_node::{self, SyncNode, SyncNodeConfig};
use mailstream_engine::{EngineError, control, daemon, json};
use serde_json::{Map, Value, json};

/// The front end started when none serves the socket: Mailstream GPUI, by bundle id when this
/// binary is not inside its bundle.
#[cfg(target_os = "macos")]
const FRONT_END: &str = "dev.mailstream.MailstreamGPUI";
/// Linux: Mailstream GPUI beside this executable, first choice first: the launcher of the
/// installed folder (`~/.local/opt/mailstream-gpui`, which sets the library path NixOS needs),
/// then the bare binary of a build folder (`target/release`).
#[cfg(all(unix, not(target_os = "macos")))]
const FRONT_ENDS: [&str; 2] = ["mailstream-gpui.sh", "mailstream-gpui"];
/// How long an answer may take: `accounts add` waits up to 5 minutes for the browser sign-in.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(420);
/// How long a started front end may take to open the socket.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// What to run where no app can serve the socket (a server without a desktop).
#[cfg(all(unix, not(target_os = "macos")))]
const DAEMON_HINT: &str = "start the headless engine with `mailstream daemon` (for example as a systemd service), then try again";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match execute(&arguments) {
        Ok(text) => {
            if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut line =
                json::encode(&json!({ "error": { "code": error.code, "message": error.message } }), true, false);
            line.push(b'\n');
            let _ = std::io::stderr().write_all(&line);
            ExitCode::from(match error.code.as_str() {
                "usage" => 2,
                "unavailable" | "not_running" => 3,
                _ => 1,
            })
        }
    }
}

/// The text to print on success.
fn execute(arguments: &[String]) -> Result<String, EngineError> {
    let (data_dir, arguments) = extract_data_dir(arguments)?;
    if arguments.first().is_none_or(|first| ["help", "--help", "-h"].contains(&first.as_str())) {
        let rest = arguments.get(1..).unwrap_or_default();
        return Ok(if rest.is_empty() {
            general_help()
        } else if let Some((_, help)) = daemon_command(rest).filter(|(length, _)| *length == rest.len()) {
            help.to_owned()
        } else {
            matching(rest)?.0.help()
        });
    }
    if arguments == ["commands"] {
        return Ok(text(&Value::Array(COMMANDS.iter().map(Spec::json).collect())));
    }
    if let Some((length, help)) = daemon_command(&arguments) {
        let rest = &arguments[length..];
        if rest.iter().any(|word| word == "--help" || word == "-h") {
            return Ok(help.to_owned());
        }
        let options = parse_daemon_args(rest, &arguments[..length].join(" "))?;
        let (join, cloud) = (options.join.as_deref(), options.cloud.as_deref());
        let print = |event: &Value| println!("{}", single_line(event));
        if length == 1 {
            daemon::run(data_dir, join, cloud, print)?;
        } else {
            sync_node::run_daemon(data_dir, join, cloud, print)?;
        }
        return Ok(String::new());
    }
    let (spec, rest) = matching(&arguments)?;
    if rest.iter().any(|word| word == "--help" || word == "-h") {
        return Ok(spec.help());
    }
    let args = parse(rest, spec)?;
    let result = if spec.local() {
        mailstream_engine::run_settings_command(&data_dir, spec.id, &args)?
    } else {
        send(spec, args, &data_dir)?
    };
    if let Some(failed) = result.get("failed").and_then(Value::as_array)
        && !failed.is_empty()
    {
        println!("{}", text(&result));
        let count = failed.len();
        let plural = if count == 1 { "" } else { "s" };
        return Err(EngineError::new("partial", format!("{count} item{plural} failed; see \"failed\".")));
    }
    Ok(text(&result))
}

const DAEMON_HELP: &str = "mailstream daemon [--data-dir <path>] [--join <ticket>] [--cloud <url>]

Run Mailstream's whole engine without the GUI, for servers and the agents on them: it keeps the
signed-in accounts' mail, calendars and Google Chat in sync and answers every command (status,
accounts, mailboxes, mail, calendar, chat, config, sync) on the control socket of its data
directory. Prints JSON lines on stdout and shuts down cleanly on SIGTERM/SIGINT. One Mailstream
(this daemon or the app) serves a data directory at a time.

  --data-dir <path>  Data directory: settings.json, accounts.json, mail.db, peer-sync/, the
                     sign-ins file, cache/ and control.sock (default: $MAILSTREAM_DATA_DIR or the
                     app's own folder). Other commands reach the daemon with the same --data-dir.
  --join <ticket>    Join this device-sync group on startup if not already paired with it
  --cloud <url>      Also keep an encrypted settings copy on this Cloudflare Worker (SyncDrop/)

`mailstream accounts add google|microsoft` prints the provider's sign-in page as a `signIn` line
here; open it in a browser that reaches http://localhost:49152 on this machine (from another one,
forward the port first: ssh -L 49152:localhost:49152 <server>).";

const SYNC_DAEMON_HELP: &str = "mailstream sync daemon [--data-dir <path>] [--join <ticket>] [--cloud <url>]

Run a headless cross-device settings sync node without the Mailstream GUI or mail accounts
(`mailstream daemon` runs the whole engine instead). Prints JSON lines on stdout and shuts down
cleanly on SIGTERM/SIGINT.

  --data-dir <path>  Sync data directory (default: $MAILSTREAM_DATA_DIR or platform data dir)
  --join <ticket>    Join this sync group on startup if not already paired with it
  --cloud <url>      Also keep an encrypted copy on this Cloudflare Worker (SyncDrop/), for devices
                     that are never online together; `mailstream sync cloud-disable` turns it off";

/// The daemon `words` start with (`daemon`, `sync daemon`): how many words name it, and its help.
fn daemon_command(words: &[String]) -> Option<(usize, &'static str)> {
    match words {
        [first, ..] if first == "daemon" => Some((1, DAEMON_HELP)),
        [first, second, ..] if first == "sync" && second == "daemon" => Some((2, SYNC_DAEMON_HELP)),
        _ => None,
    }
}

fn single_line(value: &Value) -> String {
    String::from_utf8(json::encode(value, false, false)).expect("JSON is UTF-8")
}

fn extract_data_dir(arguments: &[String]) -> Result<(PathBuf, Vec<String>), EngineError> {
    let mut explicit: Option<String> = None;
    let mut rest = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        let arg = &arguments[index];
        if arg == "--data-dir" {
            let value = arguments
                .get(index + 1)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| usage("`--data-dir` requires a directory path."))?;
            explicit = Some(value.clone());
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--data-dir=") {
            if value.is_empty() {
                return Err(usage("`--data-dir` requires a directory path."));
            }
            explicit = Some(value.to_owned());
            index += 1;
        } else {
            rest.push(arg.clone());
            index += 1;
        }
    }
    let dir = match explicit {
        Some(raw) => PathBuf::from(absolute(&raw)),
        None => match std::env::var("MAILSTREAM_DATA_DIR").ok().filter(|v| !v.trim().is_empty()) {
            Some(raw) => PathBuf::from(absolute(raw.trim())),
            None => mailstream_engine::support_directory(),
        },
    };
    Ok((dir, rest))
}

/// The daemons' options.
#[derive(Debug, Default, PartialEq)]
struct DaemonArgs {
    join: Option<String>,
    cloud: Option<String>,
}

fn parse_daemon_args(rest: &[String], command: &str) -> Result<DaemonArgs, EngineError> {
    let mut parsed = DaemonArgs::default();
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let (slot, what) = match name {
            "--join" => (&mut parsed.join, "a pairing ticket"),
            "--cloud" => (&mut parsed.cloud, "the Cloudflare Worker's URL"),
            _ => return Err(usage(format!("Unknown argument \"{arg}\" for `mailstream {command}`."))),
        };
        let value = match inline {
            Some(value) => {
                index += 1;
                value
            }
            None => {
                index += 2;
                rest.get(index - 1).cloned().unwrap_or_default()
            }
        };
        if value.is_empty() {
            return Err(usage(format!("`{name}` requires {what}.")));
        }
        *slot = Some(value);
    }
    Ok(parsed)
}

/// Pretty, key-sorted JSON without escaped slashes, as the Swift command line printed it.
fn text(value: &Value) -> String {
    String::from_utf8(json::encode(value, true, false)).expect("JSON is UTF-8")
}

fn usage(message: impl Into<String>) -> EngineError {
    EngineError::new("usage", message)
}

fn general_help() -> String {
    let mut lines = vec![commands::OVERVIEW.to_owned(), String::new(), "Commands:".to_owned()];
    lines.extend(COMMANDS.iter().map(|spec| commands::row(&spec.name(), spec.summary)));
    lines.push(commands::row(
        "daemon",
        "Run the whole engine without the GUI, serving every command (for servers / systemd).",
    ));
    lines.push(commands::row(
        "sync daemon",
        "Run a headless device-sync node without the GUI (for servers / systemd).",
    ));
    lines.join("\n")
}

/// The command whose name is the longest prefix of `words`, and the words after it.
fn matching(words: &[String]) -> Result<(&'static Spec, &[String]), EngineError> {
    COMMANDS
        .iter()
        .filter_map(|spec| {
            let name = spec.name();
            let length = name.split(' ').count();
            (words.len() >= length
                && name
                    .split(' ')
                    .zip(words)
                    .all(|(part, word)| !word.starts_with('-') && word.replace('-', "_") == part))
                .then_some((spec, length))
        })
        .max_by_key(|(_, length)| *length)
        .map(|(spec, length)| (spec, &words[length..]))
        .ok_or_else(|| usage(format!("Unknown command \"{}\". `mailstream help` lists commands.", words.join(" "))))
}

/// The request's arguments from the words after the command.
fn parse(words: &[String], spec: &Spec) -> Result<Map<String, Value>, EngineError> {
    let mut values = Map::new();
    let mut positionals = spec.arguments.iter().filter(|argument| argument.positional).peekable();
    let option = |name: &str| spec.arguments.iter().find(|argument| argument.name == name && !argument.positional);
    let append = |values: &mut Map<String, Value>, argument: &commands::Argument, raw: &str| {
        let value = convert(raw, argument)?;
        match (argument.kind, value, values.get_mut(argument.name)) {
            (Kind::List, Value::Array(items), Some(Value::Array(existing))) => existing.extend(items),
            (_, value, _) => {
                values.insert(argument.name.into(), value);
            }
        }
        Ok::<_, EngineError>(())
    };
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let Some(flag) = word.strip_prefix("--").filter(|flag| !flag.is_empty()) else {
            let Some(argument) = positionals.peek().copied() else {
                return Err(usage(format!(
                    "Unexpected \"{word}\". `mailstream help {}` shows the usage.",
                    spec.name()
                )));
            };
            append(&mut values, argument, word)?;
            if !argument.variadic {
                positionals.next();
            }
            continue;
        };
        let (mut name, inline) = match flag.split_once('=') {
            Some((name, inline)) => (name, Some(inline)),
            None => (flag, None),
        };
        let mut negated = false;
        if option(name).is_none()
            && let Some(rest) = name.strip_prefix("no-")
        {
            name = rest;
            negated = true;
        }
        let Some(argument) = option(name) else {
            return Err(usage(format!(
                "{} has no option --{name}. `mailstream help {}` lists its options.",
                spec.name(),
                spec.name()
            )));
        };
        if argument.kind == Kind::Bool {
            let value = match inline {
                Some(inline) => convert(inline, argument)?,
                None => Value::Bool(!negated),
            };
            values.insert(name.into(), value);
            continue;
        }
        if negated {
            return Err(usage(format!("--no-{name} only works for on/off options.")));
        }
        match inline {
            Some(inline) => append(&mut values, argument, inline)?,
            None => {
                let Some(value) = words.next() else { return Err(usage(format!("--{name} needs a value."))) };
                append(&mut values, argument, value)?;
            }
        }
    }
    if let Some(missing) = spec.arguments.iter().find(|argument| argument.required && !values.contains_key(argument.name))
    {
        return Err(usage(format!(
            "{}: {} is required. `mailstream help {}` shows the usage.",
            spec.name(),
            missing.label(),
            spec.name()
        )));
    }
    Ok(values)
}

fn convert(raw: &str, argument: &commands::Argument) -> Result<Value, EngineError> {
    Ok(match argument.kind {
        Kind::String if raw == "-" && (argument.name == "body" || argument.name == "text") => {
            let mut data = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut data);
            Value::String(String::from_utf8_lossy(&data).into_owned())
        }
        Kind::String => Value::String(raw.to_owned()),
        Kind::Int => match raw.parse::<i64>() {
            Ok(value) => Value::from(value),
            Err(_) => return Err(usage(format!("--{} must be a whole number, not \"{raw}\".", argument.name))),
        },
        Kind::Bool => match boolean(raw) {
            Some(value) => Value::Bool(value),
            None => return Err(usage(format!("--{} must be true or false, not \"{raw}\".", argument.name))),
        },
        // Settings values and conversation ids are taken whole; recipient and label lists split on
        // commas. Attachments are paths, taken whole and made absolute here: the app resolves them.
        Kind::List if argument.positional => json!([raw]),
        Kind::List if argument.name == "attachments" => json!([absolute(raw)]),
        Kind::List => {
            Value::Array(raw.split(',').map(str::trim).filter(|value| !value.is_empty()).map(Value::from).collect())
        }
        Kind::Choice(choices) => {
            if !choices.contains(&raw) {
                let name = if argument.positional { argument.name.to_owned() } else { format!("--{}", argument.name) };
                return Err(usage(format!("{name} must be one of {}, not \"{raw}\".", choices.join(", "))));
            }
            Value::String(raw.to_owned())
        }
    })
}

/// `true`/`yes`/`on`/`1` or their opposites, any case.
fn boolean(word: &str) -> Option<bool> {
    match word.to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// `raw` against the working directory, with `.` and `..` resolved (symbolic links are not).
fn absolute(raw: &str) -> String {
    let path = Path::new(raw);
    let full = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::new();
    for component in full.components() {
        match component {
            // `/`, or on Windows `C:` then `\`.
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
        }
    }
    out.to_string_lossy().into_owned()
}

/// The running front end's answer to one request.
fn send(spec: &Spec, args: Map<String, Value>, data_dir: &Path) -> Result<Value, EngineError> {
    if spec.handler == Handler::Sync {
        let path = control::socket_path(data_dir);
        if let Some(stream) = connection::open(&path) {
            return request_over(stream, spec, args);
        }
        return run_sync_headless(spec, &args, data_dir);
    }
    let stream = connect_or_launch(data_dir)?;
    request_over(stream, spec, args)
}

fn run_sync_headless(spec: &Spec, args: &Map<String, Value>, data_dir: &Path) -> Result<Value, EngineError> {
    if spec.id == "sync.join" {
        let ticket = args
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| usage("sync join: <ticket> is required."))?;
        return sync_node::run_join(data_dir.to_path_buf(), ticket);
    }
    let node = SyncNode::open(SyncNodeConfig::new(data_dir.to_path_buf()))?;
    node.call_sync(spec.id, Value::Object(args.clone()))
}

fn request_over(mut stream: connection::Connection, spec: &Spec, args: Map<String, Value>) -> Result<Value, EngineError> {
    let mut request = json::encode(&json!({ "command": spec.id, "args": args }), false, false);
    request.push(b'\n');
    if stream.write_all(&request).is_err() {
        return Err(EngineError::new("unavailable", "Could not write to Mailstream's control socket."));
    }
    connection::sent(&stream, ANSWER_TIMEOUT);
    let closed = || EngineError::new("unavailable", "Mailstream closed the connection without answering.");
    let message = receive(&mut stream).filter(|message| !message.is_empty()).ok_or_else(closed)?;
    let answer: Value = serde_json::from_slice(&message).map_err(|_| closed())?;
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(answer.get("result").cloned().unwrap_or(Value::Null));
    }
    let error = answer.get("error");
    let field = |name: &str| error.and_then(|error| error.get(name)).and_then(Value::as_str).map(str::to_owned);
    Err(EngineError::new(
        &field("code").unwrap_or_else(|| "failed".into()),
        field("message").unwrap_or_else(|| "Mailstream reported a failure.".into()),
    ))
}

/// Bytes up to the first newline (or the end), at most `control::MAX_ANSWER`.
fn receive(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut message = Vec::new();
    let mut buffer = vec![0; 64 << 10];
    while message.len() <= control::MAX_ANSWER {
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if count == 0 {
            break;
        }
        if let Some(newline) = buffer[..count].iter().position(|&byte| byte == b'\n') {
            message.extend_from_slice(&buffer[..newline]);
            return Some(message);
        }
        message.extend_from_slice(&buffer[..count]);
    }
    (message.len() <= control::MAX_ANSWER).then_some(message)
}

/// A connection to the running front end or daemon; starts Mailstream GPUI (in the background)
/// when none serves the socket.
fn connect_or_launch(data_dir: &Path) -> Result<connection::Connection, EngineError> {
    let path = control::socket_path(data_dir);
    if let Some(stream) = connection::open(&path) {
        return Ok(stream);
    }
    // The app serves only its own folder: only a daemon answers for another one.
    if data_dir != mailstream_engine::support_directory() {
        return Err(EngineError::new(
            "not_running",
            format!(
                "Nothing serves {}; start the headless engine with `mailstream daemon --data-dir {}`, then try again.",
                data_dir.display(),
                data_dir.display()
            ),
        ));
    }
    let name = launch()?;
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(stream) = connection::open(&path) {
            return Ok(stream);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(EngineError::new("not_running", format!("Started {name}, but it did not open its control socket within 20 s.")))
}

/// Starts Mailstream GPUI in the background; answers what it started.
#[cfg(target_os = "macos")]
fn launch() -> Result<String, EngineError> {
    use std::process::{Command, Stdio};

    let not_running = |message: String| EngineError::new("not_running", message);
    let mut open = Command::new("/usr/bin/open");
    open.arg("-g").stdout(Stdio::null()).stderr(Stdio::null());
    let name = match app_bundle() {
        Some(app) => {
            open.arg(&app);
            app.display().to_string()
        }
        None => {
            open.args(["-b", FRONT_END]);
            "Mailstream GPUI".to_owned()
        }
    };
    match open.status() {
        Ok(status) if status.success() => Ok(name),
        Ok(_) => Err(not_running(format!("Mailstream is not running, and {name} could not be started."))),
        Err(error) => Err(not_running(format!("Could not start {name}: {error}"))),
    }
}

/// Starts Mailstream GPUI in the background, from the folder this executable (or the target of
/// a symlink such as ~/.local/bin/mailstream) lives in; answers what it started.
#[cfg(all(unix, not(target_os = "macos")))]
fn launch() -> Result<String, EngineError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let not_running = |message: String| EngineError::new("not_running", message);
    let executable = std::env::current_exe().and_then(|path| path.canonicalize());
    let front_end = executable.ok().and_then(|path| linux_front_end(path.parent()?));
    let Some(front_end) = front_end else {
        let primary = FRONT_ENDS[0];
        return Err(not_running(format!(
            "Mailstream is not running, and there is no {primary} beside this command to start; start the app, or {DAEMON_HINT}."
        )));
    };
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        return Err(not_running(format!(
            "Mailstream is not running, and there is no desktop session to show Mailstream GPUI in; {DAEMON_HINT}."
        )));
    }
    let name = front_end.display().to_string();
    // A process group of its own, so the app outlives this command and its terminal's signals.
    let started = Command::new(&front_end)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match started {
        Ok(_) => Ok(name),
        Err(error) => Err(not_running(format!("Could not start {name}: {error}"))),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_front_end(directory: &Path) -> Option<PathBuf> {
    FRONT_ENDS.iter().map(|candidate| directory.join(candidate)).find(|path| path.is_file())
}

/// Windows has no app to start by name: the person starts Mailstream.
#[cfg(windows)]
fn launch() -> Result<String, EngineError> {
    Err(EngineError::new("not_running", "Mailstream is not running; start it, then try again."))
}

/// The .app this executable lives in, following a symlink such as ~/.local/bin/mailstream.
#[cfg(target_os = "macos")]
fn app_bundle() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    executable.ancestors().skip(1).find(|path| path.extension().is_some_and(|extension| extension == "app")).map(Path::to_path_buf)
}

/// The control socket's client side.
#[cfg(unix)]
mod connection {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    pub type Connection = UnixStream;

    pub fn open(path: &Path) -> Option<Connection> {
        UnixStream::connect(path).ok()
    }

    /// The request is written: the answer may take up to `timeout`.
    pub fn sent(stream: &Connection, timeout: Duration) {
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.shutdown(Shutdown::Write);
    }
}

/// The control pipe's client side.
#[cfg(windows)]
mod connection {
    use std::path::Path;
    use std::time::{Duration, Instant};

    pub type Connection = std::fs::File;

    /// `ERROR_PIPE_BUSY`: the engine is opening the pipe's next instance.
    const PIPE_BUSY: i32 = 231;

    pub fn open(path: &Path) -> Option<Connection> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(path) {
                Ok(pipe) => return Some(pipe),
                Err(error) if error.raw_os_error() == Some(PIPE_BUSY) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
    }

    /// A pipe has neither a half-close nor a read timeout: the request's newline ends it.
    pub fn sent(_pipe: &Connection, _timeout: Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_front_end_prefers_launcher_and_falls_back_to_binary() {
        use std::fs;
        let root = std::env::temp_dir().join(format!("mailstream-cli-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        assert_eq!(linux_front_end(&root), None);

        // Ignores Flutter's old executable even if present beside the CLI.
        fs::write(root.join("mailstream_ui"), b"").unwrap();
        assert_eq!(linux_front_end(&root), None);

        // Falls back to the bare binary in a build folder (target/release).
        let binary = root.join("mailstream-gpui");
        fs::write(&binary, b"").unwrap();
        assert_eq!(linux_front_end(&root).as_deref(), Some(binary.as_path()));

        // Prefers the installed NixOS launcher when both exist.
        let launcher = root.join("mailstream-gpui.sh");
        fs::write(&launcher, b"").unwrap();
        assert_eq!(linux_front_end(&root).as_deref(), Some(launcher.as_path()));

        let _ = fs::remove_dir_all(&root);
    }

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn extracts_data_dir_and_matches_hyphenated_sync_commands() {
        let (dir, rest) = extract_data_dir(&words(&["sync", "daemon", "--data-dir", "/tmp/ms-sync", "--join", "doc1"]))
            .unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/ms-sync"));
        assert_eq!(rest, words(&["sync", "daemon", "--join", "doc1"]));
        assert_eq!(
            parse_daemon_args(&rest[2..], "sync daemon").unwrap(),
            DaemonArgs { join: Some("doc1".to_owned()), cloud: None }
        );
        let both = words(&["--cloud=https://drop.example.workers.dev", "--join", "doc1"]);
        assert_eq!(
            parse_daemon_args(&both, "sync daemon").unwrap(),
            DaemonArgs { join: Some("doc1".to_owned()), cloud: Some("https://drop.example.workers.dev".to_owned()) }
        );
        assert_eq!(parse_daemon_args(&words(&["--cloud"]), "sync daemon").unwrap_err().code, "usage");

        let (dir2, rest2) = extract_data_dir(&words(&["--data-dir=/tmp/ms-two", "sync", "pair-ticket"])).unwrap();
        assert_eq!(dir2, PathBuf::from("/tmp/ms-two"));
        let (spec, trailing) = matching(&rest2).unwrap();
        assert_eq!(spec.id, "sync.pair_ticket");
        assert!(trailing.is_empty());
        let cloud = words(&["sync", "cloud-enable", "https://drop.example.workers.dev"]);
        let (spec, trailing) = matching(&cloud).unwrap();
        assert_eq!(spec.id, "sync.cloud_enable");
        assert_eq!(trailing, words(&["https://drop.example.workers.dev"]));
        let bare_cloud = words(&["sync", "cloud-enable"]);
        let (spec, trailing) = matching(&bare_cloud).unwrap();
        assert_eq!(spec.id, "sync.cloud_enable");
        assert!(parse(trailing, spec).unwrap().is_empty());
    }

    #[test]
    fn names_both_daemons_and_sends_other_data_directories_to_the_daemon() {
        assert_eq!(daemon_command(&words(&["daemon", "--join", "doc1"])), Some((1, DAEMON_HELP)));
        assert_eq!(daemon_command(&words(&["sync", "daemon"])), Some((2, SYNC_DAEMON_HELP)));
        assert_eq!(daemon_command(&words(&["sync", "status"])), None);
        assert_eq!(execute(&words(&["help", "daemon"])).unwrap(), DAEMON_HELP);
        assert_eq!(execute(&words(&["daemon", "--help"])).unwrap(), DAEMON_HELP);
        assert_eq!(execute(&words(&["help", "sync", "daemon"])).unwrap(), SYNC_DAEMON_HELP);
        assert!(general_help().contains("\n  daemon "), "{}", general_help());
        let error = execute(&words(&["daemon", "--verbose"])).unwrap_err();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("usage", "Unknown argument \"--verbose\" for `mailstream daemon`."));

        // Nothing serves this folder and no app would: the command names the daemon at once
        // instead of starting Mailstream GPUI and waiting 20 s for a socket it never opens.
        let folder = std::env::temp_dir().join(format!("mailstream-cli-nodaemon-{}", std::process::id()));
        let started = Instant::now();
        let error = execute(&words(&["--data-dir", folder.to_str().unwrap(), "mail", "list"])).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(error.code, "not_running");
        assert!(error.message.contains(&format!("`mailstream daemon --data-dir {}`", folder.display())), "{}", error.message);
    }
}
