//! An end-to-end check of the in-process engine without accounts or private mail, for CI on a
//! fresh machine (`cargo run --release --example smoke`): it starts the engine and calls
//! listeners on this process's main loop, reads its status and settings, writes a setting and
//! waits for the engine to hear the file change, asks for the saved sign-ins' client, lists the
//! inbox from the cache, looks up public sender pictures (network, DNS and image decoding) and
//! starts a sign-in to check the redirect listener answers on 127.0.0.1:49152. Exits 0 when every
//! step passed, 1 otherwise.
//!
//! Do not run it against a profile with real sign-ins: it writes `sync.pollSeconds` (then resets
//! it) and opens a browser tab for a sign-in that never completes.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use futures_executor::block_on;
use mailstream::{Change, Event, EventListener, ListQuery, Mailstream, SettingValue, run_main_loop};

struct Forward(Sender<Event>);

impl EventListener for Forward {
    fn on_event(&self, change: Change) {
        let _ = self.0.send(change.event);
    }
}

fn main() {
    let engine = Mailstream::new();
    let (events, received) = channel();
    engine.subscribe(Arc::new(Forward(events)));
    std::thread::spawn(move || {
        let mut failed = 0;
        let mut check = |name: &str, result: Result<String, String>| match result {
            Ok(detail) => println!("ok   {name}: {detail}"),
            Err(detail) => {
                failed += 1;
                println!("FAIL {name}: {detail}");
            }
        };

        let status = block_on(engine.status()).map_err(|e| e.to_string());
        check("status", status.as_ref().map(|s| format!("version {}, {} accounts, settings at {}", s.version, s.accounts.len(), s.settings_file)).map_err(Clone::clone));

        let settings = block_on(engine.settings()).map_err(|e| e.to_string());
        check("settings", settings.map(|s| format!("{} settings, issues {:?}", s.settings.len(), s.issues)));

        // The engine follows settings.json on disk; a write through it must come back as an event.
        while received.try_recv().is_ok() {}
        let set = block_on(engine.set_setting("sync.pollSeconds".into(), SettingValue::Number { value: 123 }));
        let heard = wait_for(&received, Event::Settings, Duration::from_secs(10));
        check("settings write and watch", match (set, heard) {
            (Ok(entry), true) => Ok(format!("{entry:?}")),
            (Ok(_), false) => Err("no settings event within 10 s".into()),
            (Err(error), _) => Err(error.to_string()),
        });
        let reset = block_on(engine.reset_setting("sync.pollSeconds".into()));
        check("settings reset", reset.map(|e| format!("{e:?}")).map_err(|e| e.to_string()));

        // Reads the saved sign-ins (none on a fresh machine).
        let client = block_on(engine.suggested_client("google".into()));
        check("saved sign-ins", client.map(|c| format!("{c:?}")).map_err(|e| e.to_string()));

        // No accounts, so no inbox to list: the engine says so rather than failing otherwise.
        let listing = block_on(engine.list(ListQuery { limit: 5, cached: true, ..ListQuery::default() }));
        check("cache listing", match listing {
            Ok(listing) => Ok(format!("{} conversations", listing.conversations.len())),
            Err(error) if error.code() == "unknown_mailbox" => Ok(format!("no accounts: {error}")),
            Err(error) => Err(format!("{}: {error}", error.code())),
        });

        // Gravatar over HTTPS, the BIMI logo through a DNS TXT lookup, a site icon: decoded and
        // redrawn as PNG files.
        let addresses = vec!["service@paypal.com".to_string(), "noreply@github.com".to_string()];
        let avatars = block_on(engine.avatars(addresses.clone(), addresses.clone()));
        check("avatars", match avatars {
            Ok(found) => {
                let files: Vec<_> = found.values().flatten().cloned().collect();
                let pngs = files.iter().filter(|url| is_png(url)).count();
                if pngs > 0 { Ok(format!("{pngs} PNG files: {files:?}")) } else { Err(format!("no picture: {found:?}")) }
            }
            Err(error) => Err(error.to_string()),
        });

        // A sign-in opens the browser and listens for the redirect; a request with a stale state
        // must be answered with the "out of date" page (the sign-in itself keeps waiting).
        let signer = Mailstream::new();
        std::thread::spawn(move || {
            let _ = block_on(signer.add_account("google".into(), Some("smoke-test.apps.googleusercontent.com".into()), Some("none".into())));
        });
        check("sign-in redirect listener", probe_redirect());

        println!("{}", if failed == 0 { "smoke: all passed" } else { "smoke: failures" });
        std::process::exit(if failed == 0 { 0 } else { 1 });
    });
    run_main_loop();
}

fn wait_for(received: &std::sync::mpsc::Receiver<Event>, event: Event, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while let Ok(next) = received.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if next == event {
            return true;
        }
    }
    false
}

fn is_png(url: &str) -> bool {
    let path = url.strip_prefix("file:///").map(|p| p.replace("%20", " "));
    let path = match path {
        // file:///C:/… on Windows, file:///Users/… elsewhere.
        Some(p) if cfg!(windows) => p,
        Some(p) => format!("/{p}"),
        None => return false,
    };
    std::fs::read(&path).map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n")).unwrap_or(false)
}

fn probe_redirect() -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match TcpStream::connect("127.0.0.1:49152") {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
                stream
                    .write_all(b"GET /?state=stale&code=x HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .map_err(|e| e.to_string())?;
                let mut answer = String::new();
                stream.read_to_string(&mut answer).map_err(|e| e.to_string())?;
                let first = answer.lines().next().unwrap_or("").to_string();
                return if first.contains("400") && answer.contains("out of date") {
                    Ok(first)
                } else {
                    Err(format!("unexpected answer: {answer:?}"))
                };
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(format!("nothing listening: {error}")),
        }
    }
}
