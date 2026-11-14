//! The control socket, `control.sock` in the support directory (`~/Library/Application
//! Support/Mailstream`), which the `mailstream` command line and agents use. Whichever front end
//! starts first serves it, Mailstream.app (Swift) included; the others leave it alone and take it
//! over once it goes away. Mode 0600 inside the directory made 0700, so only the user's own
//! processes reach it. On Windows it is a named pipe instead, named after the support directory
//! (local clients only; the default security lets only the user, administrators and the system
//! write to it).
//!
//! One request per connection: the client writes one JSON object and a newline (or closes its
//! side), the engine answers with one JSON object and a newline, and closes.
//! Request: `{"command": "mail.list", "args": {…}}`.
//! Answer: `{"ok": true, "result": …}` or `{"ok": false, "error": {"code": …, "message": …}}`.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub use self::transport::socket_path;
use crate::EngineError;

/// What answers the socket's requests: the engine, or a headless sync node (`sync_node`).
pub(crate) trait Handle: Send + Sync + 'static {
    fn handle(&self, command: &str, args: Value) -> impl Future<Output = Result<Value, EngineError>> + Send;
}

/// Requests larger than this are refused.
pub const MAX_REQUEST: usize = 16 << 20;
/// Answers larger than this are read as no answer: bodies of `mail show` make answers much
/// larger than any request.
pub const MAX_ANSWER: usize = 256 << 20;
/// How often a process that does not serve the socket checks whether the one that did went away.
const CHECK: Duration = Duration::from_secs(5);

/// Serves the socket whenever no other process does, for the life of the runtime it is called on.
pub(crate) fn serve(core: Arc<impl Handle>, support_directory: PathBuf) {
    tokio::spawn(async move {
        let path = socket_path(&support_directory);
        loop {
            if let Some(listener) = transport::claim(&support_directory, &path).await {
                accept(listener, &core).await;
            }
            tokio::time::sleep(CHECK).await;
        }
    });
}

/// Serves connections, each on its own task, until the socket fails.
async fn accept(mut listener: transport::Listener, core: &Arc<impl Handle>) {
    while let Ok(stream) = listener.accept().await {
        let core = core.clone();
        tokio::spawn(async move {
            let _ = connection(stream, &*core).await;
        });
    }
}

async fn connection(mut stream: impl AsyncRead + AsyncWrite + Unpin, core: &impl Handle) -> std::io::Result<()> {
    let Some(request) = receive(&mut stream).await? else { return Ok(()) };
    let mut answer = answer(core, &request).await;
    answer.push(b'\n');
    stream.write_all(&answer).await?;
    stream.shutdown().await
}

/// Bytes up to the first newline or the end of the stream; None past `MAX_REQUEST`.
async fn receive(stream: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<Vec<u8>>> {
    let mut message = Vec::new();
    let mut buffer = vec![0; 64 << 10];
    while message.len() <= MAX_REQUEST {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(Some(message));
        }
        if let Some(newline) = buffer[..count].iter().position(|&byte| byte == b'\n') {
            message.extend_from_slice(&buffer[..newline]);
            return Ok((message.len() <= MAX_REQUEST).then_some(message));
        }
        message.extend_from_slice(&buffer[..count]);
    }
    Ok(None)
}

/// One request's answer, encoded as the Swift engine's `ControlService.handle` encodes it.
async fn answer(core: &impl Handle, request: &[u8]) -> Vec<u8> {
    let answer = match run(core, request).await {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(error) => json!({ "ok": false, "error": { "code": error.code, "message": error.message } }),
    };
    crate::json::encode(&answer, false, false)
}

async fn run(core: &impl Handle, request: &[u8]) -> Result<Value, EngineError> {
    let mut request = serde_json::from_slice::<Value>(request).unwrap_or(Value::Null);
    let Some(command) = request.get("command").and_then(Value::as_str).map(str::to_owned) else {
        return Err(EngineError::new("bad_request", "Expected {\"command\": …, \"args\": {…}}."));
    };
    let args = match request.get_mut("args").map(Value::take) {
        Some(Value::Object(args)) => args,
        _ => Map::new(),
    };
    core.handle(&command, Value::Object(args)).await
}


#[cfg(unix)]
mod transport {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tokio::net::{UnixListener, UnixStream};

    /// The socket in `support_directory`.
    pub fn socket_path(support_directory: &Path) -> PathBuf {
        support_directory.join("control.sock")
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub async fn accept(&mut self) -> std::io::Result<UnixStream> {
            self.0.accept().await.map(|(stream, _)| stream)
        }
    }

    /// A listener on the socket, or None while another process serves it (a connect, which
    /// answers at once, tells). A stale socket file is replaced.
    pub async fn claim(directory: &Path, path: &Path) -> Option<Listener> {
        if UnixStream::connect(path).await.is_ok() {
            return None;
        }
        std::fs::create_dir_all(directory).ok()?;
        // The kernel refuses other users at the private directory from the moment the socket is
        // bound.
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).ok()?;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).ok()?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok()?;
        Some(Listener(listener))
    }
}

#[cfg(windows)]
mod transport {
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    /// The pipe of the engines sharing `support_directory`: `\\.\pipe\dev.mailstream.control.<hash>`.
    pub fn socket_path(support_directory: &Path) -> PathBuf {
        let digest = Sha256::digest(support_directory.to_string_lossy().to_lowercase().as_bytes());
        let hash: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
        PathBuf::from(format!(r"\\.\pipe\dev.mailstream.control.{hash}"))
    }

    /// Two waiting instances, so a client arriving while a connection is handed off still finds
    /// one (with a lone instance it would get `ERROR_PIPE_BUSY`); a client may reach either.
    pub struct Listener {
        path: PathBuf,
        next: NamedPipeServer,
        spare: NamedPipeServer,
    }

    impl Listener {
        pub async fn accept(&mut self) -> std::io::Result<NamedPipeServer> {
            // `connect` is cancel safe: the branch that loses keeps its client for the next call.
            let connected = tokio::select! {
                result = self.next.connect() => result.map(|()| &mut self.next),
                result = self.spare.connect() => result.map(|()| &mut self.spare),
            }?;
            let fresh = instance(&self.path, false)?;
            Ok(std::mem::replace(connected, fresh))
        }
    }

    /// The pipe's first instances, or None while another process has the pipe.
    pub async fn claim(_directory: &Path, path: &Path) -> Option<Listener> {
        let next = instance(path, true).ok()?;
        let spare = instance(path, false).ok()?;
        Some(Listener { path: path.to_path_buf(), next, spare })
    }

    fn instance(path: &Path, first: bool) -> std::io::Result<NamedPipeServer> {
        ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true).create(path)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::path::Path;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testkit::{cleanup, config};
    use crate::{Engine, OAuthConfig};

    /// One request over the socket, answered as text.
    #[cfg(unix)]
    fn ask(path: &Path, request: &str) -> String {
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    /// One request over the pipe, answered as text. A pipe has no half-close, so the request
    /// ends with a newline.
    #[cfg(windows)]
    fn ask(path: &Path, request: &str) -> String {
        let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
        pipe.write_all(request.trim_end_matches('\n').as_bytes()).unwrap();
        pipe.write_all(b"\n").unwrap();
        let mut answer = String::new();
        pipe.read_to_string(&mut answer).unwrap();
        answer
    }

    fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        #[cfg(unix)]
        let open = || std::os::unix::net::UnixStream::connect(path).is_ok();
        #[cfg(windows)]
        let open = || std::fs::OpenOptions::new().read(true).write(true).open(path).is_ok();
        while !open() {
            assert!(std::time::Instant::now() < deadline, "the socket opens");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn serves_commands_and_leaves_a_served_socket_alone() {
        let mut config = config(OAuthConfig::default());
        config.control_socket = true;
        let directory = config.support_directory.clone();
        let path = socket_path(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        // A stale socket file from a process that went away is replaced.
        #[cfg(unix)]
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let engine = Engine::start(config.clone());
        wait_for(&path);
        #[cfg(unix)]
        {
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!((mode(&directory), mode(&path)), (0o700, 0o600));
        }

        let settings = serde_json::to_string(&directory.join("settings.json").display().to_string()).unwrap();
        let answer = ask(&path, "{\"command\": \"config.path\", \"args\": {}}\n");
        assert_eq!(answer, format!("{{\"ok\":true,\"result\":{{\"exists\":false,\"path\":{settings}}}}}\n"));
        let answer = ask(&path, "{\"command\": \"config.set\", \"args\": {\"key\": \"sync.pollSeconds\", \"value\": \"120\"}}");
        assert!(answer.starts_with("{\"ok\":true,\"result\":{"), "{answer}");
        assert!(answer.contains("\"value\":120"), "{answer}");
        assert_eq!(
            ask(&path, "{\"command\": \"mail.nope\"}\n"),
            "{\"error\":{\"code\":\"unknown_command\",\"message\":\"Unknown command \\\"mail.nope\\\"; `mailstream commands` lists them.\"},\"ok\":false}\n"
        );
        assert_eq!(
            ask(&path, "not json\n"),
            "{\"error\":{\"code\":\"bad_request\",\"message\":\"Expected {\\\"command\\\": …, \\\"args\\\": {…}}.\"},\"ok\":false}\n"
        );
        let answer = ask(&path, "{\"command\": \"accounts.rename\", \"args\": {\"account\": \"a@b.c\"}}\n");
        assert!(answer.contains("\"message\":\"accounts rename: <label> is required.\""), "{answer}");

        // A second engine on the same data leaves the socket to the first: the file stays the one
        // the first bound.
        #[cfg(unix)]
        let inode = |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
        #[cfg(unix)]
        let bound = inode(&path);
        let second = Engine::start(config.clone());
        std::thread::sleep(Duration::from_millis(300));
        #[cfg(unix)]
        assert_eq!(inode(&path), bound);
        assert!(ask(&path, "{\"command\": \"config.path\"}\n").starts_with("{\"ok\":true"));
        drop(second);
        drop(engine);
        cleanup(&config);
    }
}
