//! # Chat accounts
//!
//! A chat account is an ortie account named `*-chat`, the same convention Mailstream's avatars
//! use for `*-contacts`: its scopes are Google Chat's, its `login_hint` is the user's address. The
//! access token comes from `ortie -a <name> token show`, which refreshes it when it has expired.

use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

const SUFFIX: &str = "-chat";
/// Google access tokens last an hour; one this old is asked for again (ortie hands back the same
/// one, or refreshes it).
const TOKEN_AGE_LIMIT: Duration = Duration::from_secs(40 * 60);

static TOKENS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

#[derive(Serialize)]
pub struct ChatAccount {
    pub name: String,
    pub email: Option<String>,
}

/// `ORTIE_CONFIG`'s first path, or ortie's default.
fn config_path() -> Option<PathBuf> {
    if let Some(paths) = env::var_os("ORTIE_CONFIG") {
        let first = paths.to_string_lossy().split(':').next().unwrap_or_default().to_owned();
        if !first.is_empty() {
            return Some(PathBuf::from(shellexpand::tilde(&first).into_owned()));
        }
    }
    dirs::home_dir().map(|home| home.join(".config/ortie/config.toml"))
}

/// Every `*-chat` account, in the config's order. No config means no accounts.
pub fn list() -> Result<Vec<ChatAccount>> {
    let Some(path) = config_path() else { return Ok(Vec::new()) };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("Read {} error", path.display())),
    };
    let config: toml::Table = toml::from_str(&text).with_context(|| format!("Parse {} error", path.display()))?;
    let Some(accounts) = config.get("accounts").and_then(|a| a.as_table()) else { return Ok(Vec::new()) };
    Ok(accounts
        .iter()
        .filter(|(name, _)| name.ends_with(SUFFIX))
        .map(|(name, account)| ChatAccount {
            name: name.clone(),
            email: account
                .get("extras")
                .and_then(|extras| extras.get("login_hint"))
                .and_then(|hint| hint.as_str())
                .map(str::to_owned),
        })
        .collect())
}

/// Fails unless `name` is a configured chat account, so a request can't borrow another ortie
/// account's token.
pub fn check(name: &str) -> Result<()> {
    if !name.ends_with(SUFFIX) || !list()?.iter().any(|account| account.name == name) {
        bail!("No Google Chat account named {name}");
    }
    Ok(())
}

/// The account's access token, from memory while it is young, else from ortie.
pub fn token(name: &str, fresh: bool) -> Result<String> {
    if !fresh {
        let tokens = TOKENS.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some((token, at)) = tokens.as_ref().and_then(|tokens| tokens.get(name))
            && at.elapsed() < TOKEN_AGE_LIMIT
        {
            return Ok(token.clone());
        }
    }
    let output = Command::new("ortie")
        .args(["-a", name, "token", "show"])
        .output()
        .context("Run ortie error (is it on PATH?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ortie could not give a token for {name}: {}", stderr.trim());
    }
    let token = String::from_utf8(output.stdout).context("ortie printed a non-UTF-8 token")?.trim().to_owned();
    if token.is_empty() {
        bail!("ortie printed an empty token for {name}");
    }
    let mut tokens = TOKENS.lock().unwrap_or_else(|poison| poison.into_inner());
    tokens.get_or_insert_with(HashMap::new).insert(name.to_owned(), (token.clone(), Instant::now()));
    Ok(token)
}
