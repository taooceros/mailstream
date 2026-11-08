//! OAuth access tokens by account, refreshed from the refresh tokens kept in the Keychain, as the
//! Swift engine's `Tokens` (`Sources/MailstreamKit/Backend/Tokens.swift`). Concurrent callers for
//! one account share a single refresh; the Keychain item is read once and again before each
//! change, because other processes hosting an engine write it too.
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};

use crate::keychain::{Keychain, KeychainError, SignIns};
use crate::oauth::{BuiltInClients, OAuth, OAuthClient, OAuthError, Provider, TokenResponse};

/// Why there is no token.
#[derive(Clone, Debug)]
pub enum TokenError {
    /// No usable sign-in: none stored, or the provider refused it (`invalid_grant`). The Swift
    /// engine answers these as a 401.
    SignInAgain(String),
    /// The provider refused otherwise or did not answer, or the Keychain failed.
    Failed(String),
}

impl TokenError {
    fn sign_in_again(account: &str) -> Self {
        Self::SignInAgain(format!("{account} needs to sign in again (Settings > Accounts)."))
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignInAgain(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl From<KeychainError> for TokenError {
    fn from(error: KeychainError) -> Self {
        Self::Failed(error.to_string())
    }
}

struct Access {
    token: String,
    expiry: Instant,
}

type Refresh = Shared<BoxFuture<'static, Result<TokenResponse, OAuthError>>>;

/// A refresh in flight, shared by every caller for that account.
struct Flight {
    id: u64,
    refresh: Refresh,
}

/// A signed-in account as refreshing needs it: whose token endpoint, and what its sign-in granted.
#[derive(Clone, Debug)]
pub struct SignedIn {
    pub provider: Provider,
    pub scopes: Vec<String>,
}

struct State {
    accounts: HashMap<String, SignedIn>,
    access: HashMap<String, Access>,
    flights: HashMap<String, Flight>,
    next_flight: u64,
    /// The Keychain item's contents once read.
    sign_ins: Option<SignIns>,
}

pub struct Tokens {
    keychain: Keychain,
    oauth: Arc<OAuth>,
    built_in: BuiltInClients,
    state: Mutex<State>,
}

/// A token closer than this to expiring is refreshed first.
const MARGIN: Duration = Duration::from_secs(300);

impl Tokens {
    /// `accounts` are the signed-in accounts: whose token endpoint each refreshes against, and
    /// the scopes it granted.
    pub fn new(
        keychain: Keychain,
        oauth: Arc<OAuth>,
        built_in: BuiltInClients,
        accounts: HashMap<String, SignedIn>,
    ) -> Self {
        let state =
            State { accounts, access: HashMap::new(), flights: HashMap::new(), next_flight: 0, sign_ins: None };
        Self { keychain, oauth, built_in, state: Mutex::new(state) }
    }

    fn built_in(&self, provider: Provider) -> Option<OAuthClient> {
        self.built_in.get(&provider).cloned()
    }

    /// A current access token for `account`. Runs on the engine's runtime.
    pub async fn token(&self, account: &str) -> Result<String, TokenError> {
        let (id, refresh) = {
            let mut state = self.state.lock().expect("tokens lock");
            if let Some(cached) = state.access.get(account)
                && cached.expiry.saturating_duration_since(Instant::now()) > MARGIN
            {
                return Ok(cached.token.clone());
            }
            let Some(SignedIn { provider, scopes }) = state.accounts.get(account).cloned() else {
                return Err(TokenError::sign_in_again(account));
            };
            if let Some(flight) = state.flights.get(account) {
                (flight.id, flight.refresh.clone())
            } else {
                let stored = state.stored(&self.keychain)?.clone();
                let Some(refresh_token) = stored.refresh_tokens.get(account).cloned() else {
                    return Err(TokenError::sign_in_again(account));
                };
                // Only the client that issued a refresh token can redeem it. Tokens from before
                // clients were stored came from the build's client, which only Mailstream.app's
                // Info.plist carries: storing it lets every host of the engine redeem them.
                let Some(client) = stored.clients.get(account).cloned().or_else(|| self.built_in(provider)) else {
                    return Err(TokenError::sign_in_again(account));
                };
                if !stored.clients.contains_key(account) {
                    state.set_client(&self.keychain, &client, account)?;
                }
                let oauth = self.oauth.clone();
                let task = tokio::spawn(async move { oauth.refresh(&refresh_token, provider, &client, &scopes).await });
                let refresh: Refresh =
                    async move { task.await.unwrap_or_else(|error| Err(OAuthError::new("failed", error.to_string()))) }
                        .boxed()
                        .shared();
                state.next_flight += 1;
                let id = state.next_flight;
                state.flights.insert(account.to_string(), Flight { id, refresh: refresh.clone() });
                (id, refresh)
            }
        };
        let result = refresh.await;
        let mut state = self.state.lock().expect("tokens lock");
        let current = state.flights.get(account).is_some_and(|flight| flight.id == id);
        match result {
            Err(error) => {
                if current {
                    state.flights.remove(account);
                }
                if error.code == "invalid_grant" {
                    return Err(TokenError::sign_in_again(account));
                }
                Err(TokenError::Failed(error.message))
            }
            Ok(response) => {
                // The first waiter back records the result; a sign-in or removal meanwhile supersedes it.
                if current {
                    state.flights.remove(account);
                    if let Some(rotated) = &response.refresh_token {
                        state.set_refresh_token(&self.keychain, rotated, account)?;
                    }
                    let lifetime = Duration::from_secs(response.expires_in.unwrap_or(3600).max(0) as u64);
                    state.access.insert(
                        account.to_string(),
                        Access { token: response.access_token.clone(), expiry: Instant::now() + lifetime },
                    );
                }
                Ok(response.access_token)
            }
        }
    }

    /// Starts over from a fresh sign-in: stores `refresh_token` with the `client` that issued it,
    /// remembers `client` for the provider's next sign-in, and caches `access_token`.
    pub fn install(
        &self,
        account: &str,
        signed_in: SignedIn,
        client: &OAuthClient,
        refresh_token: &str,
        access_token: &str,
        expires_in: i64,
    ) -> Result<(), TokenError> {
        let mut state = self.state.lock().expect("tokens lock");
        let mut sign_ins = state.fresh(&self.keychain)?;
        sign_ins.refresh_tokens.insert(account.into(), refresh_token.into());
        sign_ins.clients.insert(account.into(), client.clone());
        sign_ins.last_clients.insert(signed_in.provider.as_str().into(), client.clone());
        state.save(&self.keychain, sign_ins)?;
        state.accounts.insert(account.into(), signed_in);
        state.flights.remove(account);
        let expiry = Instant::now() + Duration::from_secs(expires_in.max(0) as u64);
        state.access.insert(account.into(), Access { token: access_token.into(), expiry });
        Ok(())
    }

    /// The client to offer for `provider`'s next sign-in: the one used last, else the build's.
    pub fn suggested_client(&self, provider: Provider) -> Result<Option<OAuthClient>, TokenError> {
        let mut state = self.state.lock().expect("tokens lock");
        let last = state.stored(&self.keychain)?.last_clients.get(provider.as_str()).cloned();
        Ok(last.or_else(|| self.built_in(provider)))
    }

    /// Forgets `token` after a provider refused it (401), so the next `token` refreshes.
    pub fn reject(&self, token: &str, account: &str) {
        let mut state = self.state.lock().expect("tokens lock");
        if state.access.get(account).is_some_and(|access| access.token == token) {
            state.access.remove(account);
        }
    }

    /// Signs `account` out: deletes its refresh token and forgets its access token.
    pub fn forget(&self, account: &str) {
        let mut state = self.state.lock().expect("tokens lock");
        if let Ok(mut sign_ins) = state.fresh(&self.keychain) {
            sign_ins.refresh_tokens.remove(account);
            sign_ins.clients.remove(account);
            let _ = state.save(&self.keychain, sign_ins);
        }
        state.accounts.remove(account);
        state.flights.remove(account);
        state.access.remove(account);
    }
}

impl State {
    fn stored(&mut self, keychain: &Keychain) -> Result<&SignIns, KeychainError> {
        if self.sign_ins.is_none() {
            self.sign_ins = Some(keychain.load()?);
        }
        Ok(self.sign_ins.as_ref().expect("just loaded"))
    }

    /// The item as it is now, for a change to start from.
    fn fresh(&mut self, keychain: &Keychain) -> Result<SignIns, KeychainError> {
        let loaded = keychain.load()?;
        self.sign_ins = Some(loaded.clone());
        Ok(loaded)
    }

    fn set_refresh_token(&mut self, keychain: &Keychain, token: &str, account: &str) -> Result<(), KeychainError> {
        let mut sign_ins = self.fresh(keychain)?;
        if sign_ins.refresh_tokens.get(account).map(String::as_str) == Some(token) {
            return Ok(());
        }
        sign_ins.refresh_tokens.insert(account.into(), token.into());
        self.save(keychain, sign_ins)
    }

    fn set_client(&mut self, keychain: &Keychain, client: &OAuthClient, account: &str) -> Result<(), KeychainError> {
        let mut sign_ins = self.fresh(keychain)?;
        if sign_ins.clients.contains_key(account) {
            return Ok(());
        }
        sign_ins.clients.insert(account.into(), client.clone());
        self.save(keychain, sign_ins)
    }

    fn save(&mut self, keychain: &Keychain, contents: SignIns) -> Result<(), KeychainError> {
        if self.sign_ins.as_ref() == Some(&contents) {
            return Ok(());
        }
        keychain.store(&contents)?;
        self.sign_ins = Some(contents);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::oauth::OAuthConfig;
    use crate::oauth::tests::TokenServer;

    /// A Keychain item of its own, deleted when the test ends.
    struct TestKeychain(Keychain);

    impl TestKeychain {
        fn new() -> Self {
            Self(Keychain::new(format!("dev.mailstream.test.{}.{}", std::process::id(), crate::tests::unique())))
        }
    }

    impl Drop for TestKeychain {
        fn drop(&mut self) {
            let _ = self.0.store(&SignIns::default());
        }
    }

    fn tokens(keychain: &TestKeychain, url: &str, built_in: BuiltInClients) -> Tokens {
        let oauth = OAuth::new(OAuthConfig { token_endpoint: Some(url.into()), ..OAuthConfig::default() });
        let microsoft = |scopes: &[&str]| SignedIn {
            provider: Provider::Microsoft,
            scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
        };
        let accounts = HashMap::from([
            ("g@x.com".to_string(), SignedIn { provider: Provider::Google, scopes: vec![] }),
            ("m@x.com".to_string(), microsoft(&["openid", "Mail.ReadWrite", "User.Read"])),
            ("c@x.com".to_string(), microsoft(&["openid", "Calendars.ReadWrite", "Mail.ReadWrite"])),
        ]);
        Tokens::new(keychain.0.clone(), Arc::new(oauth), built_in, accounts)
    }

    fn client(id: &str) -> OAuthClient {
        OAuthClient { id: id.into(), secret: None }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refreshes_once_for_concurrent_callers_and_keeps_rotated_tokens() {
        let keychain = TestKeychain::new();
        let mut sign_ins = SignIns::default();
        sign_ins.refresh_tokens.insert("m@x.com".into(), "old".into());
        sign_ins.clients.insert("m@x.com".into(), client("ms-client"));
        keychain.0.store(&sign_ins).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let server = TokenServer::start(move |_| {
            let call = counted.fetch_add(1, Ordering::SeqCst) + 1;
            (200, format!(r#"{{"access_token":"at{call}","expires_in":3600,"refresh_token":"new"}}"#))
        })
        .await;
        let shared = Arc::new(tokens(&keychain, &server.url, BuiltInClients::new()));
        let (first, second) = tokio::join!(shared.token("m@x.com"), shared.token("m@x.com"));
        assert_eq!((first.unwrap(), second.unwrap()), ("at1".to_string(), "at1".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Cached until it nears expiry; Microsoft's rotated refresh token went back to the item.
        assert_eq!(shared.token("m@x.com").await.unwrap(), "at1");
        assert_eq!(keychain.0.load().unwrap().refresh_tokens["m@x.com"], "new");
        let form = server.forms.lock().unwrap()[0].clone();
        assert_eq!((form["grant_type"].as_str(), form["refresh_token"].as_str()), ("refresh_token", "old"));
        assert_eq!(form["client_id"], "ms-client");
        assert!(form["scope"].contains("offline_access"));
        // Signed in before calendars were asked for: Microsoft would refuse a refresh asking for them.
        assert!(!form["scope"].contains("Calendars"), "{}", form["scope"]);
        // Another process (or the next launch) redeems the rotated token.
        let other = tokens(&keychain, &server.url, BuiltInClients::new());
        assert_eq!(other.token("m@x.com").await.unwrap(), "at2");
        assert_eq!(server.forms.lock().unwrap()[1]["refresh_token"], "new");
        // An account whose sign-in granted calendars keeps them.
        let mut sign_ins = keychain.0.load().unwrap();
        sign_ins.refresh_tokens.insert("c@x.com".into(), "rc".into());
        sign_ins.clients.insert("c@x.com".into(), client("ms-client"));
        keychain.0.store(&sign_ins).unwrap();
        let fresh = tokens(&keychain, &server.url, BuiltInClients::new());
        fresh.token("c@x.com").await.unwrap();
        assert!(server.forms.lock().unwrap()[2]["scope"].contains("https://graph.microsoft.com/Calendars.ReadWrite"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_the_built_in_client_and_asks_to_sign_in_again_on_invalid_grant() {
        let keychain = TestKeychain::new();
        // The format from before clients were stored.
        let mut sign_ins = SignIns::default();
        sign_ins.refresh_tokens.insert("g@x.com".into(), "rt".into());
        keychain.0.store(&sign_ins).unwrap();
        let server = TokenServer::start(|_| {
            (400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into())
        })
        .await;
        let built_in =
            BuiltInClients::from([(Provider::Google, OAuthClient { id: "built-in".into(), secret: Some("s".into()) })]);
        let tokens = tokens(&keychain, &server.url, built_in);
        let error = tokens.token("g@x.com").await.unwrap_err();
        assert!(matches!(&error, TokenError::SignInAgain(_)), "{error:?}");
        assert_eq!(error.to_string(), "g@x.com needs to sign in again (Settings > Accounts).");
        assert_eq!(keychain.0.load().unwrap().clients["g@x.com"].id, "built-in");
        assert_eq!(server.forms.lock().unwrap()[0]["client_secret"], "s");
        // No refresh token at all, or an account not signed in.
        assert!(matches!(tokens.token("m@x.com").await, Err(TokenError::SignInAgain(_))));
        assert!(matches!(tokens.token("nobody@x.com").await, Err(TokenError::SignInAgain(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn installs_suggests_and_forgets() {
        let keychain = TestKeychain::new();
        let tokens = tokens(&keychain, "http://127.0.0.1:9/token", BuiltInClients::new());
        assert!(tokens.suggested_client(Provider::Google).unwrap().is_none());
        let picked = OAuthClient { id: "mine".into(), secret: Some("s".into()) };
        let signed_in = SignedIn { provider: Provider::Google, scopes: vec![] };
        tokens.install("new@x.com", signed_in, &picked, "rt", "at", 3600).unwrap();
        assert_eq!(tokens.token("new@x.com").await.unwrap(), "at");
        assert_eq!(tokens.suggested_client(Provider::Google).unwrap(), Some(picked.clone()));
        tokens.forget("new@x.com");
        let left = keychain.0.load().unwrap();
        assert!(left.refresh_tokens.is_empty() && left.clients.is_empty());
        // The client stays remembered for the next sign-in.
        assert_eq!(left.last_clients["google"], picked);
    }
}
