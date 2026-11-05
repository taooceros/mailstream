//! The running Mailstream app's control socket
//! (`~/Library/Application Support/Mailstream/control.sock`): one JSON request per connection,
//! answered with one JSON object before the app closes the connection. The Swift app stays the
//! backend (accounts, sync, cache, threading, tagging); this UI only asks it for values.
//!
//! Calls block, so they run on the background executor, never on the main thread.

use std::fmt;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// A failed command: the app's stable `code` and a message for people.
#[derive(Clone, Debug)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

impl ControlError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

static SOCKET: LazyLock<PathBuf> = LazyLock::new(|| {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/Mailstream/control.sock")
});

#[derive(Deserialize)]
struct Answer<T> {
    ok: bool,
    result: Option<T>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

/// Sends `command` with `args` and decodes the answer's `result` as `T`. Blocking.
pub fn call<T: DeserializeOwned>(command: &str, args: Value) -> Result<T, ControlError> {
    // Listing a mailbox the app has not cached yet waits on the provider.
    call_waiting(command, args, Duration::from_secs(120))
}

/// `call`, waiting up to `wait` for the answer (signing in waits on the browser for minutes).
pub fn call_waiting<T: DeserializeOwned>(command: &str, args: Value, wait: Duration) -> Result<T, ControlError> {
    let mut stream = UnixStream::connect(&*SOCKET).map_err(|_| {
        ControlError::new("unreachable", "Mailstream is not running: nothing listens on its control socket.")
    })?;
    let broken = |error: std::io::Error| {
        ControlError::new("unreachable", format!("The connection to Mailstream broke: {error}"))
    };
    stream.set_read_timeout(Some(wait)).map_err(broken)?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(broken)?;

    let mut request = serde_json::to_vec(&json!({ "command": command, "args": args }))
        .expect("a JSON value always encodes");
    request.push(b'\n');
    stream.write_all(&request).map_err(broken)?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).map_err(broken)?;
    if bytes.is_empty() {
        return Err(ControlError::new("unreachable", "Mailstream closed the connection without answering."));
    }
    let answer: Answer<T> = serde_json::from_slice(&bytes)
        .map_err(|error| ControlError::new("decode", format!("Unexpected answer to {command}: {error}")))?;
    if answer.ok {
        return answer
            .result
            .ok_or_else(|| ControlError::new("decode", format!("{command} answered without a result.")));
    }
    let error = answer.error.unwrap_or(ErrorBody { code: None, message: None });
    Err(ControlError {
        code: error.code.unwrap_or_else(|| "failed".into()),
        message: error.message.unwrap_or_else(|| "The command failed.".into()),
    })
}

/// `{"done": […], "failed": [{id, code, message}]}`, the answer of commands over conversation ids.
#[derive(Deserialize)]
pub struct Batch {
    #[serde(default)]
    pub failed: Vec<BatchFailure>,
}

#[derive(Deserialize)]
pub struct BatchFailure {
    #[serde(default)]
    pub code: Option<String>,
    pub message: String,
}

/// Runs a command over conversation ids; a failure for any of them is the command's failure.
pub fn batch(command: &str, args: Value) -> Result<(), ControlError> {
    let answer: Batch = call(command, args)?;
    match answer.failed.into_iter().next() {
        None => Ok(()),
        Some(failure) => Err(ControlError {
            code: failure.code.unwrap_or_else(|| "failed".into()),
            message: failure.message,
        }),
    }
}
