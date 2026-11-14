//! What the engine's tests share: throwaway engine folders signed in through a test Keychain
//! item, calling and waiting on an engine, and the stand-ins for the providers (`fake_server`).
//! Nothing reaches a provider.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub use crate::fake_server::{FakeRequest, FakeServer, Reply, endpoints, serve};
use crate::keychain::{Keychain, SignIns};
use crate::{BuiltInClients, Config, Engine, EngineError, OAuthClient, OAuthConfig, avatars, omp, push};

// MARK: - Engines

/// A number no other test in this process gets, for temporary names.
pub fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A throwaway engine folder and Keychain item that cleans itself up on drop (including panics).
pub struct TestConfig(Config);

impl std::ops::Deref for TestConfig {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.0
    }
}

impl std::ops::DerefMut for TestConfig {
    fn deref_mut(&mut self) -> &mut Config {
        &mut self.0
    }
}

impl Drop for TestConfig {
    fn drop(&mut self) {
        cleanup(&self.0);
    }
}

/// A throwaway engine folder and Keychain item whose providers are all unreachable.
pub fn config(oauth: OAuthConfig) -> TestConfig {
    let name = format!("mailstream-engine-{}-{}", std::process::id(), unique());
    let root = std::env::temp_dir().join(name.clone());
    TestConfig(Config {
        support_directory: root.join("support"),
        cache_directory: root.join("caches"),
        keychain_service: format!("dev.mailstream.test.{name}"),
        keychain_directory: root.join("support"),
        built_in_clients: BuiltInClients::new(),
        oauth,
        // Nothing listens there: no test reaches a real provider.
        endpoints: endpoints("http://127.0.0.1:9"),
        // No relay: no test registers push with a provider unless it sets a fake one.
        push: push::PushConfig::default(),
        // A PATH without omp (the tools the fake scripts use only): tagging runs only the fake
        // omp a test names.
        tool_environment: Some(HashMap::from([("PATH".to_string(), tools_path())])),
        // No Apple Intelligence: only a fake helper a test writes there runs.
        apple_tagger: root.join(omp::APPLE_TAGGER),
        avatar_web: Arc::new(avatars::tests::FakeWeb::default()),
        // The test of the socket turns it on.
        control_socket: false,
        device_sync: None,
    })
}

/// A PATH holding just the folders of the tools the fake scripts run (`grep`, `head`, `sleep`,
/// `touch`), each found on the tests' own PATH in a folder without an `omp`, so a child sees
/// none of the user's: NixOS has no such tools in `/usr/bin` or `/bin`.
pub fn tools_path() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut directories: Vec<std::path::PathBuf> = Vec::new();
    for tool in ["grep", "head", "sleep", "touch"] {
        let directory = std::env::split_paths(&path)
            .find(|directory| directory.join(tool).is_file() && !directory.join("omp").exists())
            .unwrap_or_else(|| panic!("no {tool} on PATH outside a folder holding omp"));
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    std::env::join_paths(directories).unwrap().into_string().unwrap()
}

/// `config` with every provider and the token endpoint (`/token`) on `server`.
pub fn server_config(server: &FakeServer) -> TestConfig {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = endpoints(&server.base);
    config
}

/// A mail account's accounts.json record: labelled with its id, mail scopes only.
pub fn mail_account(id: &str, provider: &str) -> Value {
    json!({ "id": id, "provider": provider, "label": id, "scopes": ["openid", "mail"] })
}

/// Writes `records` as the folder's accounts.json.
pub fn write_accounts(config: &Config, records: &[Value]) {
    std::fs::create_dir_all(&config.support_directory).unwrap();
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(records).unwrap()).unwrap();
}

/// Signs `ids` in through the folder's test Keychain item: refresh token `rt-<id>`, a client of
/// the test's.
pub fn sign_in(config: &Config, ids: &[&str]) {
    let mut sign_ins = SignIns::default();
    for id in ids {
        sign_ins.refresh_tokens.insert(id.to_string(), format!("rt-{id}"));
        sign_ins.clients.insert(id.to_string(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    }
    Keychain::new(config.keychain_service.clone(), config.keychain_directory.clone()).store(&sign_ins).unwrap();
}

/// `records` written and every one of them signed in.
pub fn sign_in_all(config: &Config, records: &[Value]) {
    write_accounts(config, records);
    let ids: Vec<&str> = records.iter().map(|record| record["id"].as_str().unwrap()).collect();
    sign_in(config, &ids);
}

/// An engine on `server`, signed in to `accounts` (`(id, provider)`) as mail accounts.
pub fn engine_for(server: &FakeServer, accounts: &[(&str, &str)]) -> (Engine, TestConfig) {
    let config = server_config(server);
    let records: Vec<Value> = accounts.iter().map(|(id, provider)| mail_account(id, provider)).collect();
    sign_in_all(&config, &records);
    (Engine::start(config.clone()), config)
}

/// Empties the test Keychain item and deletes the folder.
pub fn cleanup(config: &Config) {
    let _ = Keychain::new(config.keychain_service.clone(), config.keychain_directory.clone()).store(&SignIns::default());
    let _ = std::fs::remove_dir_all(config.support_directory.parent().unwrap());
}

pub fn call(engine: &Engine, command: &str, args: Value) -> Result<Value, EngineError> {
    block_on(engine.call(command, args))
}

/// Waits up to 10 s for `done`.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The token endpoint's answer: a fresh access token (`at1`, `at2`, …) per call.
pub fn token_reply(calls: &AtomicUsize) -> Reply {
    let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
    Reply::json(json!({ "access_token": format!("at{call}"), "expires_in": 3600 }))
}

// MARK: - Sign-in

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The browser's part: follows the sign-in page's redirect to `port` with `query` and the state
/// the page carries.
pub fn redirecting_browser(port: u16, query: &'static str) -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(move |url: &str| {
        let state = url.split('&').find_map(|pair| pair.strip_prefix("state=")).unwrap().to_string();
        tokio::spawn(async move {
            let mut connection = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let request = format!("GET /?{query}&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            connection.write_all(request.as_bytes()).await.unwrap();
            let mut page = String::new();
            connection.read_to_string(&mut page).await.unwrap();
            assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_ins_stay_in_temporary_folder_and_clean_up_on_drop() {
        let (root, file, default_file, config_copy) = {
            let config = config(OAuthConfig::default());
            sign_in(&config, &["me@gmail.com"]);
            let file = config.keychain_directory.join(format!("{}.signins", config.keychain_service));
            let default_file = crate::keychain::default_directory().join(format!("{}.signins", config.keychain_service));
            assert!(!default_file.exists(), "test sign-ins must not touch default_directory");
            #[cfg(not(target_os = "macos"))]
            assert!(file.exists(), "test sign-ins must live in the temporary keychain_directory");
            (config.support_directory.parent().unwrap().to_path_buf(), file, default_file, config.clone())
        };
        assert!(!file.exists());
        assert!(!default_file.exists());
        assert!(!root.exists());
        assert_eq!(
            Keychain::new(config_copy.keychain_service, config_copy.keychain_directory).load().unwrap(),
            SignIns::default()
        );
    }
}
