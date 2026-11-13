//! Browser sign-in and token refresh, hand-rolled as in the Swift engine
//! (`Sources/MailstreamKit/Backend/OAuth.swift`): authorization code with PKCE, redirected to a
//! one-shot listener on `http://localhost:49152`, the only redirect Microsoft's app registration
//! allows.
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Microsoft,
}

const GMAIL: &str = "https://mail.google.com/";
/// What sign-in asks for: the account and its mail, nothing more (MS-306). The rest is a
/// [`Service`] the person connects later.
const GOOGLE_MAIL: &[&str] = &["openid", "email", "profile", GMAIL];
pub const GOOGLE_CONTACTS: &str = "https://www.googleapis.com/auth/contacts.readonly";
pub const GOOGLE_DIRECTORY: &str = "https://www.googleapis.com/auth/directory.readonly";
const GOOGLE_CONTACT_SCOPES: &[&str] =
    &[GOOGLE_CONTACTS, "https://www.googleapis.com/auth/contacts.other.readonly", GOOGLE_DIRECTORY];
/// The scope that makes a Google account a chat account.
pub const GOOGLE_CHAT: &str = "https://www.googleapis.com/auth/chat.messages.readonly";
/// The scope that lets an account start direct messages and group chats (`spaces.setup`).
pub const GOOGLE_CHAT_CREATE: &str = "https://www.googleapis.com/auth/chat.spaces.create";
const GOOGLE_CHAT_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/chat.spaces.readonly",
    GOOGLE_CHAT_CREATE,
    GOOGLE_CHAT,
    "https://www.googleapis.com/auth/chat.messages.create",
    "https://www.googleapis.com/auth/chat.memberships.readonly",
    "https://www.googleapis.com/auth/chat.users.readstate",
];
/// Calendars and their events (the calendar view), and answering invitations and editing events.
const GOOGLE_CALENDAR_SCOPES: &[&str] = &[GOOGLE_CALENDARS, GOOGLE_EVENTS];
pub const GOOGLE_CALENDARS: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const GOOGLE_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
const MICROSOFT_MAIL: &[&str] = &[
    "openid",
    "email",
    "profile",
    "offline_access",
    "https://graph.microsoft.com/User.Read",
    MICROSOFT_MAIL_READ_WRITE,
    "https://graph.microsoft.com/Mail.Send",
];
const MICROSOFT_MAIL_READ_WRITE: &str = "https://graph.microsoft.com/Mail.ReadWrite";
pub const GRAPH_CONTACTS: &str = "https://graph.microsoft.com/Contacts.Read";
pub const MICROSOFT_CALENDARS: &str = "https://graph.microsoft.com/Calendars.ReadWrite";
/// The read-only calendar scope accounts signed in before Mailstream edited events were granted.
pub const MICROSOFT_CALENDARS_READ: &str = "https://graph.microsoft.com/Calendars.Read";

/// Whether `granted` (an account's scopes as the provider listed them, with or without Graph's
/// `https://graph.microsoft.com/` prefix) holds `scope`.
pub fn has_scope(granted: &[String], scope: &str) -> bool {
    let short = |scope: &str| scope.trim_end_matches('/').rsplit('/').next().unwrap_or(scope).to_lowercase();
    granted.iter().any(|candidate| candidate == scope || short(candidate) == short(scope))
}

/// What an account may use beyond mail. Sign-in asks for none of them: each is a second consent
/// for the account (`accounts.connect`) when the person turns it on, so an organization that lets
/// people allow mail alone, or a person who unticks a box, still gets a working mail account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// Contact photos and the Workspace directory.
    Contacts,
    /// Google Chat.
    Chat,
    /// The calendar view, invitations and editing events.
    Calendar,
}

impl Service {
    /// In the order accounts list them.
    pub const ALL: [Self; 3] = [Self::Contacts, Self::Chat, Self::Calendar];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.as_str() == name)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contacts => "contacts",
            Self::Chat => "chat",
            Self::Calendar => "calendar",
        }
    }

    /// The scopes connecting it asks `provider` for; none when the provider has no such service.
    pub fn scopes(self, provider: Provider) -> &'static [&'static str] {
        match (provider, self) {
            (Provider::Google, Self::Contacts) => GOOGLE_CONTACT_SCOPES,
            (Provider::Google, Self::Chat) => GOOGLE_CHAT_SCOPES,
            (Provider::Google, Self::Calendar) => GOOGLE_CALENDAR_SCOPES,
            (Provider::Microsoft, Self::Contacts) => &[GRAPH_CONTACTS],
            (Provider::Microsoft, Self::Chat) => &[],
            (Provider::Microsoft, Self::Calendar) => &[MICROSOFT_CALENDARS],
        }
    }
}

/// Connecting a service to an account already signed in: whose sign-in it extends, and what that
/// sign-in granted so far.
#[derive(Clone, Copy, Debug)]
pub struct Connect<'a> {
    pub account: &'a str,
    pub granted: &'a [String],
    pub service: Service,
}

impl Provider {
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "google" => Some(Self::Google),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }

    fn authorize_url(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        }
    }

    fn token_url(self) -> &'static str {
        match self {
            Self::Google => "https://oauth2.googleapis.com/token",
            Self::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        }
    }

    /// Requested at sign-in: the account and its mail.
    fn mail_scopes(self) -> &'static [&'static str] {
        match self {
            Self::Google => GOOGLE_MAIL,
            Self::Microsoft => MICROSOFT_MAIL,
        }
    }

    /// The scope without which an account has no mail.
    fn mail_scope(self) -> &'static str {
        match self {
            Self::Google => GMAIL,
            Self::Microsoft => MICROSOFT_MAIL_READ_WRITE,
        }
    }

    /// What a sign-in asks for. Connecting a service asks Google for the account and that
    /// service, as `include_granted_scopes` keeps the rest; Microsoft's consent covers what it
    /// names, so it names everything the account keeps too.
    fn requested_scopes(self, connect: Option<Connect<'_>>) -> Vec<&'static str> {
        let Some(connect) = connect else { return self.mail_scopes().to_vec() };
        let mut scopes = match self {
            Self::Google => vec!["openid", "email"],
            Self::Microsoft => self.refresh_scopes(connect.granted),
        };
        for scope in connect.service.scopes(self) {
            if !scopes.contains(scope) {
                scopes.push(scope);
            }
        }
        scopes
    }

    /// What a Microsoft refresh asks for: mail and the services the account's grant has.
    /// Microsoft refuses a refresh that asks for more than was granted.
    fn refresh_scopes(self, granted: &[String]) -> Vec<&'static str> {
        let optional = Service::ALL.into_iter().flat_map(|service| service.scopes(self).iter().copied());
        let mut scopes = self.mail_scopes().to_vec();
        scopes.extend(optional.filter(|scope| has_scope(granted, scope)));
        scopes
    }

    /// Authorization parameters beyond the standard ones: Google issues a refresh token only with
    /// offline access and consent, and keeps earlier grants with `include_granted_scopes`;
    /// Microsoft lets the person pick which account, unless a connect names it.
    fn authorize_extras(self, connect: Option<Connect<'_>>) -> Vec<(&'static str, String)> {
        let mut extras: Vec<(&'static str, String)> = match self {
            Self::Google => vec![
                ("access_type", "offline".into()),
                ("prompt", "consent".into()),
                ("include_granted_scopes", "true".into()),
            ],
            Self::Microsoft if connect.is_none() => vec![("prompt", "select_account".into())],
            Self::Microsoft => Vec::new(),
        };
        if let Some(connect) = connect {
            extras.push(("login_hint", connect.account.into()));
        }
        extras
    }

    /// Microsoft's token endpoint takes the scopes again; Google's ignores them.
    fn token_extras(self, scopes: &[&str]) -> Vec<(&'static str, String)> {
        match self {
            Self::Google => Vec::new(),
            Self::Microsoft => vec![("scope", scopes.join(" "))],
        }
    }
}

/// An OAuth client registered with the provider: the build's, or one the person entered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthClient {
    pub id: String,
    /// Google's desktop clients have one; a Microsoft public client has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// The clients this build carries in its Info.plist (`MSGoogleClientID`, …, which
/// scripts/bundle.sh writes into Mailstream.app): the default offered before anything was
/// entered, and what accounts signed in before clients were stored refresh with.
pub type BuiltInClients = HashMap<Provider, OAuthClient>;

/// A token endpoint answer.
#[derive(Clone, Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: Option<i64>,
    /// Always present after sign-in; on refresh only when the provider rotated it (Microsoft).
    pub refresh_token: Option<String>,
    /// Granted scopes, space separated.
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

/// A sign-in or token endpoint failure. `code` is the OAuth error code (`invalid_grant`, …), or
/// one of this engine's: `listener`, `timeout`, `response`, `network`, `failed`.
#[derive(Clone, Debug)]
pub struct OAuthError {
    pub code: String,
    pub message: String,
}

impl OAuthError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// What a completed sign-in yields.
#[derive(Clone, Debug)]
pub struct Grant {
    /// Lowercased address.
    pub id: String,
    pub name: Option<String>,
    pub scopes: Vec<String>,
    pub refresh_token: String,
    pub access_token: String,
    pub expires_in: i64,
}

/// Where sign-ins redirect and how they reach the browser.
#[derive(Clone)]
pub struct OAuthConfig {
    /// 49152; tests listen elsewhere.
    pub redirect_port: u16,
    /// Every token request goes here instead of the provider's endpoint (tests).
    pub token_endpoint: Option<String>,
    pub sign_in_timeout: Duration,
    /// Opens the provider's sign-in page in the browser.
    pub open_url: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            redirect_port: 49152,
            token_endpoint: None,
            sign_in_timeout: Duration::from_secs(300),
            open_url: Arc::new(open_in_browser),
        }
    }
}

/// The default browser, as `open` picks it.
#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || std::process::Command::new("/usr/bin/open").arg(url).status());
}

/// The default browser, as the desktop's `xdg-open` picks it.
#[cfg(all(unix, not(target_os = "macos")))]
fn open_in_browser(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || std::process::Command::new("xdg-open").arg(url).status());
}

/// The default browser, as the shell's "open" verb picks it.
#[cfg(windows)]
fn open_in_browser(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, url) = (wide("open"), wide(url));
    std::thread::spawn(move || {
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
    });
}

pub struct OAuth {
    config: OAuthConfig,
}

impl OAuth {
    pub fn new(config: OAuthConfig) -> Self {
        Self { config }
    }

    fn redirect(&self) -> String {
        format!("http://localhost:{}", self.config.redirect_port)
    }

    /// Opens the provider's sign-in page once the redirect listener is up, waits (up to five
    /// minutes) for the redirect, then redeems the code with `client`. A new sign-in asks for
    /// mail; `connect` adds a service to an account signed in already, and fails when the person
    /// picked another account.
    pub async fn sign_in(
        &self,
        provider: Provider,
        client: &OAuthClient,
        connect: Option<Connect<'_>>,
    ) -> Result<Grant, OAuthError> {
        let verifier = random_token();
        let state = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let redirect = self.redirect();
        let requested = provider.requested_scopes(connect);
        let mut parameters: BTreeMap<&str, String> = BTreeMap::from([
            ("client_id", client.id.clone()),
            ("redirect_uri", redirect.clone()),
            ("response_type", "code".into()),
            ("scope", requested.join(" ")),
            ("state", state.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
        ]);
        for (name, value) in provider.authorize_extras(connect) {
            parameters.entry(name).or_insert(value);
        }
        let url = format!("{}?{}", provider.authorize_url(), crate::api::form_encoded(&parameters));

        let listener = RedirectListener::bind(self.config.redirect_port, state).await?;
        (self.config.open_url)(&url);
        let code = listener.code(self.config.sign_in_timeout).await?;

        let form = BTreeMap::from([
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
        ]);
        let response = self.exchange(form, provider, client, &requested).await?;
        let Some(refresh_token) = response.refresh_token.clone() else {
            return Err(OAuthError::new("response", "The sign-in did not grant offline access; try again."));
        };
        let claims = response.id_token.as_deref().and_then(Claims::of);
        let email = claims
            .as_ref()
            .and_then(|claims| claims.email.clone().or_else(|| claims.preferred_username.clone()))
            .map(|email| email.to_lowercase())
            .filter(|email| !email.is_empty());
        let Some(id) = email else {
            return Err(OAuthError::new("response", "The sign-in did not say which account it was for."));
        };
        if let Some(connect) = connect
            && id != connect.account
        {
            return Err(OAuthError::new(
                "wrong_account",
                format!("You signed in as {id}; pick {} to connect it.", connect.account),
            ));
        }
        // Google lets the person untick what they don't allow; `scope` says what they did.
        let scopes: Vec<String> = match &response.scope {
            Some(scope) => scope.split(' ').filter(|scope| !scope.is_empty()).map(String::from).collect(),
            None => {
                let earlier = connect.map(|connect| connect.granted.to_vec()).unwrap_or_default();
                let asked = requested.iter().map(|scope| scope.to_string()).filter(|scope| !earlier.contains(scope));
                earlier.iter().cloned().chain(asked).collect()
            }
        };
        if connect.is_none() && !has_scope(&scopes, provider.mail_scope()) {
            return Err(OAuthError::new(
                "no_mail",
                "Mailstream needs access to your mail: sign in again and allow it to read, send and organize mail.",
            ));
        }
        Ok(Grant {
            id,
            name: claims.and_then(|claims| claims.name),
            scopes,
            refresh_token,
            access_token: response.access_token,
            expires_in: response.expires_in.unwrap_or(3600),
        })
    }

    /// A new access token from `refresh_token`, redeemed with the client that issued it, for an
    /// account whose sign-in granted `granted`.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        provider: Provider,
        client: &OAuthClient,
        granted: &[String],
    ) -> Result<TokenResponse, OAuthError> {
        let form =
            BTreeMap::from([("grant_type", "refresh_token".to_string()), ("refresh_token", refresh_token.to_string())]);
        self.exchange(form, provider, client, &provider.refresh_scopes(granted)).await
    }

    /// POSTs `form` (plus the client credentials) to the token endpoint. Failures answer
    /// `{"error": …, "error_description": …}`. `scopes`: what Microsoft is asked for again.
    async fn exchange(
        &self,
        mut form: BTreeMap<&'static str, String>,
        provider: Provider,
        client: &OAuthClient,
        scopes: &[&str],
    ) -> Result<TokenResponse, OAuthError> {
        form.entry("client_id").or_insert_with(|| client.id.clone());
        if let Some(secret) = client.secret.as_ref().filter(|secret| !secret.is_empty()) {
            form.entry("client_secret").or_insert_with(|| secret.clone());
        }
        for (name, value) in provider.token_extras(scopes) {
            form.entry(name).or_insert(value);
        }
        let url = self.config.token_endpoint.as_deref().unwrap_or(provider.token_url());
        let response = crate::api::HTTP
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(crate::api::form_encoded(&form))
            .send()
            .await
            .map_err(|error| OAuthError::new("network", error.to_string()))?;
        let status = response.status().as_u16();
        let data = response.bytes().await.map_err(|error| OAuthError::new("network", error.to_string()))?;
        if (200..300).contains(&status)
            && let Ok(tokens) = serde_json::from_slice::<TokenResponse>(&data)
        {
            return Ok(tokens);
        }
        #[derive(Deserialize)]
        struct Failure {
            error: Option<String>,
            error_description: Option<String>,
        }
        let failure = serde_json::from_slice::<Failure>(&data).ok();
        let error = failure.as_ref().and_then(|failure| failure.error.clone());
        // Microsoft appends trace and correlation ids on further lines.
        let description = failure
            .as_ref()
            .and_then(|failure| failure.error_description.as_deref())
            .and_then(|description| description.lines().find(|line| !line.is_empty()))
            .map(String::from);
        let host = reqwest::Url::parse(url).ok().and_then(|url| url.host_str().map(String::from));
        let message = description
            .or_else(|| error.clone())
            .unwrap_or_else(|| format!("{} answered {status}.", host.as_deref().unwrap_or("The sign-in server")));
        Err(OAuthError::new(error.as_deref().unwrap_or("response"), message))
    }
}

/// The id token's claims. Its signature is not checked: it came straight from the token endpoint
/// over TLS.
#[derive(Deserialize)]
struct Claims {
    email: Option<String>,
    preferred_username: Option<String>,
    name: Option<String>,
}

impl Claims {
    fn of(jwt: &str) -> Option<Self> {
        let payload = jwt.split('.').filter(|part| !part.is_empty()).nth(1)?;
        serde_json::from_slice(&base64url_decode(payload)?).ok()
    }
}

/// Base64url or plain base64, padded or not; characters outside the alphabet are skipped.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter_map(|char| match char {
            '+' => Some('-'),
            '/' => Some('_'),
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => Some(char),
            _ => None,
        })
        .collect();
    URL_SAFE_NO_PAD.decode(cleaned).ok()
}

/// 256 random bits, base64url: a PKCE verifier (43 characters) or a `state`.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system's random source works");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// One-shot HTTP listener on the loopback interface for the OAuth redirect. Answers the browser
/// with a small page and stops at the first sign-in result or after the timeout.
struct RedirectListener {
    state: Arc<String>,
    listeners: Vec<TcpListener>,
}

impl RedirectListener {
    /// Listens on `localhost` (IPv4 and, where it can, IPv6) at `port`.
    async fn bind(port: u16, state: String) -> Result<Self, OAuthError> {
        let failure = |error: std::io::Error| {
            OAuthError::new("listener", format!("Could not listen for the sign-in on port {port}: {error}"))
        };
        let ipv4 = Self::listen(std::net::Ipv4Addr::LOCALHOST.into(), port).map_err(failure)?;
        // The browser may resolve `localhost` to ::1 first; a port 0 test listener stays IPv4 only.
        let mut listeners = vec![ipv4];
        if port != 0
            && let Ok(ipv6) = Self::listen(std::net::Ipv6Addr::LOCALHOST.into(), port)
        {
            listeners.push(ipv6);
        }
        Ok(Self { state: Arc::new(state), listeners })
    }

    fn listen(address: std::net::IpAddr, port: u16) -> std::io::Result<TcpListener> {
        let socket = if address.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
        socket.set_reuseaddr(true)?;
        socket.bind((address, port).into())?;
        socket.listen(16)
    }

    /// Serves connections until one carries the sign-in's result, and returns its code.
    async fn code(self, timeout: Duration) -> Result<String, OAuthError> {
        let (report, mut outcomes) = mpsc::unbounded_channel();
        let mut accepting = tokio::task::JoinSet::new();
        for listener in self.listeners {
            let (report, state) = (report.clone(), self.state.clone());
            accepting.spawn(async move {
                let mut serving = tokio::task::JoinSet::new();
                while let Ok((connection, _)) = listener.accept().await {
                    let (report, state) = (report.clone(), state.clone());
                    serving.spawn(async move {
                        if let Some(outcome) = serve(connection, &state).await {
                            let _ = report.send(outcome);
                        }
                    });
                }
            });
        }
        // Leaving drops the accept loops (and the connections they serve), closing the port.
        tokio::select! {
            outcome = outcomes.recv() => outcome.unwrap_or_else(|| Err(OAuthError::new("listener", "The sign-in listener stopped."))),
            () = tokio::time::sleep(timeout) => Err(OAuthError::new("timeout", "The sign-in timed out after five minutes.")),
        }
    }
}

/// Reads up to the end of the request head and answers it; returns the sign-in's result when
/// this was the redirect. A connection that fails or ends early is dropped.
async fn serve(mut connection: TcpStream, state: &str) -> Option<Result<String, OAuthError>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16_384];
    loop {
        let count = connection.read(&mut chunk).await.ok()?;
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let (status, text, outcome) = answer(&head, state);
            let _ = respond(&mut connection, status, text.as_deref()).await;
            return outcome;
        }
        if count == 0 || buffer.len() > 65_536 {
            return None;
        }
    }
}

/// Accepts only `GET /?code=…&state=…` (or the provider's `error=…`) with this sign-in's state;
/// anything else (a favicon, a stale tab) is answered and ignored. The page's status and text
/// (`None`: the success page), and the sign-in's result when there is one.
fn answer(head: &str, state: &str) -> (&'static str, Option<String>, Option<Result<String, OAuthError>>) {
    let request_line = head.split("\r\n").next().unwrap_or("");
    let parts: Vec<&str> = request_line.split(' ').filter(|part| !part.is_empty()).collect();
    let target = match parts.as_slice() {
        ["GET", target, _] => *target,
        _ => return ("404 Not Found", Some("Not found.".into()), None),
    };
    let target = target.split('#').next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return ("404 Not Found", Some("Not found.".into()), None);
    }
    let mut items: HashMap<String, String> = HashMap::new();
    for item in query.split('&').filter(|item| !item.is_empty()) {
        let (name, value) = item.split_once('=').unwrap_or((item, ""));
        let decode = |text: &str| percent_decode_str(text).decode_utf8_lossy().into_owned();
        items.entry(decode(name)).or_insert_with(|| decode(value));
    }
    if items.get("state").map(String::as_str) != Some(state) {
        return (
            "400 Bad Request",
            Some("This sign-in page is out of date. Start again from Mailstream.".into()),
            None,
        );
    }
    if let Some(code) = items.get("code").filter(|code| !code.is_empty()) {
        return ("200 OK", None, Some(Ok(code.clone())));
    }
    let error = items.get("error").cloned();
    let reason = items
        .get("error_description")
        .filter(|text| !text.is_empty())
        .cloned()
        .or_else(|| error.clone())
        .unwrap_or_else(|| "The provider sent no authorization code.".into());
    let message = format!("Sign-in failed: {reason}");
    let failure = OAuthError::new(error.as_deref().unwrap_or("response"), message.clone());
    ("200 OK", Some(message), Some(Err(failure)))
}

/// `text` `None`: the success page.
async fn respond(connection: &mut TcpStream, status: &str, text: Option<&str>) -> std::io::Result<()> {
    let message = text.unwrap_or("You can close this tab and return to Mailstream.");
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Mailstream</title></head><body style=\"font: 15px -apple-system, sans-serif; margin: 4em auto; max-width: 32em; text-align: center\"><p>{}</p></body></html>",
        crate::text::escape_html(message)
    );
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    connection.write_all(head.as_bytes()).await?;
    connection.write_all(body.as_bytes()).await?;
    connection.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{FakeServer, Reply, free_port, redirecting_browser};

    #[test]
    fn answers_only_the_redirect_with_this_state() {
        let (status, text, outcome) = answer("GET /favicon.ico HTTP/1.1\r\nHost: localhost", "s");
        assert_eq!((status, text.as_deref(), outcome.is_none()), ("404 Not Found", Some("Not found."), true));
        let (status, _, outcome) = answer("GET /?code=c&state=old HTTP/1.1", "s");
        assert_eq!((status, outcome.is_none()), ("400 Bad Request", true));
        let (status, text, outcome) = answer("GET /?state=s&code=4%2F0A HTTP/1.1", "s");
        assert_eq!((status, text, outcome.unwrap().unwrap()), ("200 OK", None, "4/0A".to_string()));
        let (_, text, outcome) = answer("GET /?state=s&error=access_denied&error_description= HTTP/1.1", "s");
        assert_eq!(text.as_deref(), Some("Sign-in failed: access_denied"));
        assert_eq!(outcome.unwrap().unwrap_err().code, "access_denied");
        assert_eq!(answer("POST /?state=s&code=c HTTP/1.1", "s").0, "404 Not Found");
    }

    #[test]
    fn reads_the_account_from_the_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"preferred_username":"Me@Outlook.com","name":"Me"}"#);
        let claims = Claims::of(&format!("e30.{payload}.sig")).unwrap();
        assert_eq!((claims.email, claims.preferred_username.as_deref()), (None, Some("Me@Outlook.com")));
    }

    #[tokio::test]
    async fn signs_in_through_the_redirect() {
        let id_token =
            format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(br#"{"email":"Person@Example.com","name":"Person"}"#));
        let server = FakeServer::start(move |_| {
            Reply::json(serde_json::json!({
                "access_token": "at", "expires_in": 3599, "refresh_token": "rt", "scope": format!("openid {GMAIL}"), "id_token": id_token,
            }))
        });
        let port = free_port();
        let oauth = OAuth::new(OAuthConfig {
            redirect_port: port,
            token_endpoint: Some(format!("{}/token", server.base)),
            sign_in_timeout: Duration::from_secs(10),
            open_url: redirecting_browser(port, "code=the-code"),
        });
        let client = OAuthClient { id: "client".into(), secret: Some("secret".into()) };
        let grant = oauth.sign_in(Provider::Google, &client, None).await.unwrap();
        assert_eq!(
            (grant.id.as_str(), grant.name.as_deref(), grant.refresh_token.as_str()),
            ("person@example.com", Some("Person"), "rt")
        );
        assert_eq!(grant.scopes, vec!["openid".to_string(), GMAIL.to_string()]);
        let form = server.forms("/token")[0].clone();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "the-code");
        assert_eq!(form["redirect_uri"], format!("http://localhost:{port}"));
        assert_eq!((form["client_id"].as_str(), form["client_secret"].as_str()), ("client", "secret"));
        assert_eq!(form["code_verifier"].len(), 43);
    }

    #[tokio::test]
    async fn times_out_without_a_redirect() {
        let oauth = OAuth::new(OAuthConfig {
            redirect_port: free_port(),
            token_endpoint: Some("http://127.0.0.1:9/token".into()),
            sign_in_timeout: Duration::from_millis(100),
            open_url: Arc::new(|_| {}),
        });
        let error =
            oauth.sign_in(Provider::Microsoft, &OAuthClient { id: "c".into(), secret: None }, None).await.unwrap_err();
        assert_eq!(error.code, "timeout");
    }

    /// The query of each sign-in page opened, before redirecting as `redirecting_browser` does.
    type Opened = Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>;

    fn recording_browser(port: u16) -> (Opened, Arc<dyn Fn(&str) + Send + Sync>) {
        let opened = Opened::default();
        let (kept, redirect) = (opened.clone(), redirecting_browser(port, "code=c"));
        let browser = Arc::new(move |url: &str| {
            let query = reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect();
            kept.lock().unwrap().push(query);
            redirect(url);
        });
        (opened, browser)
    }

    /// A token answer for `email` that granted `scope`.
    fn granted(email: &str, scope: &str) -> serde_json::Value {
        let claims = serde_json::json!({ "email": email });
        let id_token = format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()));
        serde_json::json!({ "access_token": "at", "expires_in": 3599, "refresh_token": "rt", "scope": scope, "id_token": id_token })
    }

    #[tokio::test]
    async fn asks_for_mail_first_and_connects_services_later() {
        let calendar = format!("openid email {GMAIL} {GOOGLE_CALENDARS} {GOOGLE_EVENTS}");
        let answers = std::sync::Mutex::new(vec![
            granted("me@gmail.com", "openid email"),
            granted("other@gmail.com", &calendar),
            granted("me@gmail.com", &calendar),
            granted("me@gmail.com", &format!("openid email {GMAIL}")),
        ]);
        let server = FakeServer::start(move |_| Reply::json(answers.lock().unwrap().remove(0)));
        let port = free_port();
        let (opened, browser) = recording_browser(port);
        let oauth = OAuth::new(OAuthConfig {
            redirect_port: port,
            token_endpoint: Some(format!("{}/token", server.base)),
            sign_in_timeout: Duration::from_secs(10),
            open_url: browser,
        });
        let client = OAuthClient { id: "client".into(), secret: Some("secret".into()) };
        // Sign-in asks for mail alone; one that leaves mail unticked is no account.
        let error = oauth.sign_in(Provider::Google, &client, None).await.unwrap_err();
        assert_eq!(error.code, "no_mail");
        let first = opened.lock().unwrap()[0].clone();
        assert_eq!(first["scope"], format!("openid email profile {GMAIL}"));
        assert_eq!(first["prompt"], "consent");
        assert!(!first.contains_key("login_hint"));

        let earlier = vec!["openid".to_string(), GMAIL.to_string()];
        let connect = Connect { account: "me@gmail.com", granted: &earlier, service: Service::Calendar };
        // Another account picked in the browser connects nothing.
        let error = oauth.sign_in(Provider::Google, &client, Some(connect)).await.unwrap_err();
        assert_eq!(error.code, "wrong_account");
        let grant = oauth.sign_in(Provider::Google, &client, Some(connect)).await.unwrap();
        let asked = opened.lock().unwrap()[2].clone();
        assert_eq!(asked["scope"], format!("openid email {GOOGLE_CALENDARS} {GOOGLE_EVENTS}"));
        assert_eq!((asked["include_granted_scopes"].as_str(), asked["login_hint"].as_str()), ("true", "me@gmail.com"));
        assert!(has_scope(&grant.scopes, GOOGLE_EVENTS) && has_scope(&grant.scopes, GMAIL));

        // Unticked in Google's consent: the grant says so, and the account keeps working.
        let chat = Connect { account: "me@gmail.com", granted: &grant.scopes, service: Service::Chat };
        let grant = oauth.sign_in(Provider::Google, &client, Some(chat)).await.unwrap();
        assert!(!has_scope(&grant.scopes, GOOGLE_CHAT));
    }

    #[test]
    fn asks_microsoft_for_what_the_account_keeps() {
        let microsoft = Provider::Microsoft;
        let signed_in = microsoft.requested_scopes(None);
        assert!(signed_in.contains(&MICROSOFT_MAIL_READ_WRITE) && signed_in.contains(&"offline_access"));
        assert!(!signed_in.contains(&GRAPH_CONTACTS) && !signed_in.contains(&MICROSOFT_CALENDARS));
        assert_eq!(microsoft.authorize_extras(None), vec![("prompt", "select_account".to_string())]);
        // As Graph lists them: without the prefix.
        let granted: Vec<String> = ["Mail.ReadWrite", "Mail.Send", "User.Read", "Contacts.Read"].map(String::from).into();
        assert_eq!(microsoft.refresh_scopes(&granted), [MICROSOFT_MAIL, &[GRAPH_CONTACTS]].concat());
        let connect = Connect { account: "me@outlook.com", granted: &granted, service: Service::Calendar };
        assert_eq!(microsoft.requested_scopes(Some(connect)), [MICROSOFT_MAIL, &[GRAPH_CONTACTS, MICROSOFT_CALENDARS]].concat());
        assert_eq!(microsoft.authorize_extras(Some(connect)), vec![("login_hint", "me@outlook.com".to_string())]);
        assert!(Service::Chat.scopes(microsoft).is_empty());
    }
}
