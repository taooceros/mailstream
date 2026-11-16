//! `mailstream daemon` end to end (MS-375): the whole engine on one data directory answering
//! every kind of command over `control.sock`, its event lines (sign-in pages included), one
//! daemon per folder, and the data directory holding every file.
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mail_tests::{graph_server, ids};
use crate::testkit::{free_port, mail_account, server_config, sign_in_all, wait_until};
use crate::{Config, control, daemon};

#[test]
fn a_data_directory_holds_every_file_but_the_apps_own_keeps_its_places() {
    let app = Config::for_this_app();
    let folder = PathBuf::from("/srv/mailstream");
    let moved = app.clone().in_directory(folder.clone());
    assert_eq!(moved.support_directory, folder);
    assert_eq!(moved.cache_directory, folder.join("cache"));
    assert_eq!(moved.keychain_directory, folder);
    assert_eq!(moved.keychain_service, app.keychain_service);

    let same = app.clone().in_directory(app.support_directory.clone());
    assert_eq!(
        (same.support_directory, same.cache_directory, same.keychain_directory),
        (app.support_directory, app.cache_directory, app.keychain_directory)
    );
}

/// One request on the socket and its answer, as `mailstream` sends it.
#[cfg(unix)]
fn ask(socket: &Path, command: &str, args: Value) -> Value {
    use std::io::{Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream.write_all(format!("{}\n", json!({ "command": command, "args": args })).as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    serde_json::from_str(&answer).unwrap()
}

/// The result of a request that must succeed.
#[cfg(unix)]
fn result(socket: &Path, command: &str, args: Value) -> Value {
    let answer = ask(socket, command, args);
    assert_eq!(answer["ok"], json!(true), "{command}: {answer}");
    answer["result"].clone()
}

/// The next event line named `name`, skipping others.
fn next(events: &mpsc::Receiver<Value>, name: &str) -> Value {
    loop {
        let event = events.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| panic!("no {name} event"));
        if event["event"] == name {
            return event;
        }
    }
}

#[cfg(unix)]
#[test]
fn serves_every_command_on_its_socket_logs_sign_ins_and_stops() {
    let server = graph_server(Arc::new(AtomicUsize::new(0)));
    let mut config = server_config(&server);
    sign_in_all(&config, &[mail_account("me@outlook.com", "microsoft")]);
    // Nobody answers the sign-in page: the daemon logs it and the sign-in gives up after 1 s.
    config.oauth.redirect_port = free_port();
    config.oauth.open_url = Arc::new(|_| {});
    config.oauth.sign_in_timeout = Duration::from_secs(1);
    let socket = control::socket_path(&config.support_directory);

    let (sender, events) = mpsc::channel::<Value>();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let daemon_config: Config = (*config).clone();
    let runner = std::thread::spawn(move || {
        daemon::run_with(
            daemon_config,
            None,
            None,
            async {
                let _ = stopped.await;
            },
            move |event| {
                let _ = sender.send(event.clone());
            },
        )
    });

    let started = next(&events, "started");
    assert_eq!(started["socket"], json!(socket.display().to_string()));
    assert_eq!(started["dataDir"], json!(config.support_directory.display().to_string()));
    assert!(started["status"]["accounts"].is_array(), "{started}");
    assert_eq!(started["sync"]["available"], json!(false));

    // Every handler answers: core, mail, calendar, chat, settings and sync.
    assert_eq!(result(&socket, "status", json!({}))["pid"], json!(std::process::id()));
    assert_eq!(result(&socket, "accounts.list", json!({}))[0]["id"], json!("me@outlook.com"));
    wait_until("mailboxes", || {
        result(&socket, "mailboxes", json!({}))["me@outlook.com"].as_array().is_some_and(|boxes| !boxes.is_empty())
    });
    let listing = result(&socket, "mail.list", json!({}));
    assert_eq!(ids(&listing), ["me@outlook.com|g1@outlook", "me@outlook.com|g2@outlook"]);
    let shown = result(&socket, "mail.show", json!({ "id": "me@outlook.com|g2@outlook" }));
    assert_eq!(shown["id"], json!("me@outlook.com|g2@outlook"));
    // The account signed in for mail only: its calendar is not connected, and it has no chat.
    let calendar = result(&socket, "calendar.events", json!({ "days": 7 }));
    assert_eq!((&calendar["accounts"][0]["id"], &calendar["accounts"][0]["granted"]), (&json!("me@outlook.com"), &json!(false)));
    assert_eq!(result(&socket, "chat.spaces", json!({}))["spaces"], json!([]));
    assert_eq!(result(&socket, "sync.status", json!({}))["available"], json!(false));
    let entry = result(&socket, "config.set", json!({ "key": "sync.pollSeconds", "value": "120" }));
    assert_eq!(entry["value"], json!(120));
    assert_eq!(next(&events, "settings")["sync"]["available"], json!(false));

    // A sign-in prints the provider's page, which the server's person opens elsewhere.
    let refused = ask(&socket, "accounts.add", json!({ "provider": "microsoft", "client-id": "server-client" }));
    assert_eq!(refused["ok"], json!(false), "{refused}");
    let sign_in = next(&events, "signIn");
    let page = sign_in["url"].as_str().unwrap();
    assert!(page.contains("client_id=server-client"), "{page}");
    let redirect = format!("http://localhost:{}", config.oauth.redirect_port);
    assert_eq!(sign_in["redirect"], json!(redirect));

    // One Mailstream per folder: a second daemon there refuses at once.
    let second = daemon::run_with((*config).clone(), None, None, std::future::pending(), |_| {});
    assert_eq!(second.unwrap_err().code, "busy");

    stop.send(()).unwrap();
    runner.join().unwrap().unwrap();
    assert_eq!(next(&events, "stopping")["status"]["accounts"][0]["id"], json!("me@outlook.com"));
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_err(), "the stopped daemon still answers");
}
