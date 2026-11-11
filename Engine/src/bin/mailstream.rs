//! `mailstream`: Mailstream from a shell or an AI agent. `config …` works on settings.json
//! directly; every other command goes to the control socket of the running front end
//! (`mailstream_engine::control`), starting Mailstream Flutter in the background when none runs.
//! Commands, arguments, help and output are the command table's (`mailstream_engine::commands`).
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mailstream_engine::commands::{self, COMMANDS, Kind, Spec};
use mailstream_engine::{EngineError, control, json};
use serde_json::{Map, Value, json};

/// The front end started when none serves the socket: Mailstream Flutter, by bundle id when this
/// binary is not inside its bundle.
#[cfg(target_os = "macos")]
const FRONT_END: &str = "dev.mailstream.MailstreamFlutter";
/// Linux: Mailstream Flutter's executable, beside this one in the release bundle.
#[cfg(all(unix, not(target_os = "macos")))]
const FRONT_END: &str = "mailstream_ui";
/// How long an answer may take: `accounts add` waits up to 5 minutes for the browser sign-in.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(420);
/// How long a started front end may take to open the socket.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match execute(&arguments) {
        Ok(text) => {
            println!("{text}");
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
    if arguments.first().is_none_or(|first| ["help", "--help", "-h"].contains(&first.as_str())) {
        let rest = arguments.get(1..).unwrap_or_default();
        return Ok(if rest.is_empty() { general_help() } else { matching(rest)?.0.help() });
    }
    if arguments == ["commands"] {
        return Ok(text(&Value::Array(COMMANDS.iter().map(Spec::json).collect())));
    }
    let (spec, rest) = matching(arguments)?;
    if rest.iter().any(|word| word == "--help" || word == "-h") {
        return Ok(spec.help());
    }
    let args = parse(rest, spec)?;
    let result = if spec.local() {
        mailstream_engine::run_settings_command(&mailstream_engine::support_directory(), spec.id, &args)?
    } else {
        send(spec, args)?
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
    lines.join("\n")
}

/// The command whose name is the longest prefix of `words`, and the words after it.
fn matching(words: &[String]) -> Result<(&'static Spec, &[String]), EngineError> {
    COMMANDS
        .iter()
        .filter_map(|spec| {
            let name = spec.name();
            let length = name.split(' ').count();
            (words.len() >= length && name.split(' ').zip(words).all(|(part, word)| part == word))
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
fn send(spec: &Spec, args: Map<String, Value>) -> Result<Value, EngineError> {
    let mut stream = connect_or_launch()?;
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

/// A connection to the running front end; starts Mailstream Flutter (in the background) when
/// none serves the socket.
fn connect_or_launch() -> Result<connection::Connection, EngineError> {
    let path = control::socket_path(&mailstream_engine::support_directory());
    if let Some(stream) = connection::open(&path) {
        return Ok(stream);
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

/// Starts Mailstream Flutter in the background; answers what it started.
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
            "Mailstream Flutter".to_owned()
        }
    };
    match open.status() {
        Ok(status) if status.success() => Ok(name),
        Ok(_) => Err(not_running(format!("Mailstream is not running, and {name} could not be started."))),
        Err(error) => Err(not_running(format!("Could not start {name}: {error}"))),
    }
}

/// Starts Mailstream Flutter in the background, from the bundle this executable (or the target of
/// a symlink such as ~/.local/bin/mailstream) lives in; answers what it started.
#[cfg(all(unix, not(target_os = "macos")))]
fn launch() -> Result<String, EngineError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let not_running = |message: String| EngineError::new("not_running", message);
    let executable = std::env::current_exe().and_then(|path| path.canonicalize());
    let front_end = executable.ok().and_then(|path| Some(path.parent()?.join(FRONT_END))).filter(|path| path.is_file());
    let Some(front_end) = front_end else {
        return Err(not_running(format!("Mailstream is not running, and there is no {FRONT_END} beside this command to start; start it, then try again.")));
    };
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
