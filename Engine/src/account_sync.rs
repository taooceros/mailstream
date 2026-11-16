//! Signed-in accounts across device sync (MS-376): the account records of accounts.json, and the
//! refresh token each one signs in with together with the OAuth client that issued it, so a
//! device that joins a group (a headless server included) uses the accounts without a browser.
//!
//! Records are plain entries of the group (`v1/account/<address>`), merged like settings: the
//! latest change wins and removing writes a tombstone, which every device follows by signing
//! the account out. Credentials never enter the group in the clear. Each device keeps an X25519
//! credential key in its own credential store (the Keychain on macOS, the 0600 file on Linux,
//! DPAPI on Windows; [`Keychain::device_key`]) and announces its public half signed with its
//! endpoint key (`v1/devkey/<endpoint id>`). A device holding an account's refresh token seals it
//! to each other device's key (`v1/cred/<recipient>/<address>`): an ephemeral X25519 key, a
//! blake3-derived AES-256-GCM key, the recipient and account as associated data. The group's
//! store, its blobs and the cloud drop hold only ciphertext; the ticket and `peer-sync/` alone
//! open nothing.
//!
//! Refresh tokens rotate on each device on their own (Google's don't rotate; Microsoft's older
//! ones stay valid until they expire); a credential is sent again only when it was issued anew
//! (a sign-in or connecting a service), which `credentials.json` records per account.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use curve25519_dalek::montgomery::MontgomeryPoint;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::accounts::AccountRecord;
use crate::keychain::{Keychain, SignIns};
use crate::oauth::{OAuthClient, Provider};
use crate::peer_sync::{PeerSync, SealedCredential, Snapshot, hex};
use crate::{EngineError, files};

const MAGIC: &[u8; 4] = b"msc1";
const NONCE: usize = 12;
const KEY_CONTEXT: &str = "mailstream device sync credential v1";
const FILE: &str = "credentials.json";

/// What a sealed credential holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Credential {
    pub refresh_token: String,
    /// The client that issued it, the only one that can redeem it.
    pub client: OAuthClient,
}

/// Where the accounts device sync bridges live: the engine's, or a headless sync node's
/// accounts.json and sign-ins.
pub(crate) trait AccountHost: Send + Sync {
    /// The credential store the sign-ins and this device's credential key are kept in.
    fn keychain(&self) -> &Keychain;
    fn accounts(&self) -> Vec<AccountRecord>;
    /// Keeps `record` as another device has it, with the credential it sent, if any.
    fn adopt(&self, record: AccountRecord, credential: Option<Credential>) -> Result<(), String>;
    /// Signs out of `id`, which another device removed.
    fn remove(&self, id: String) -> BoxFuture<'_, Result<(), String>>;
}

/// Writes `credential` into `sign_ins` for `account` of `provider`; the client is offered for
/// the provider's next sign-in unless one is already.
pub(crate) fn adopt_sign_in(sign_ins: &mut SignIns, account: &str, provider: Provider, credential: &Credential) {
    sign_ins.refresh_tokens.insert(account.into(), credential.refresh_token.clone());
    sign_ins.clients.insert(account.into(), credential.client.clone());
    sign_ins.last_clients.entry(provider.as_str().into()).or_insert_with(|| credential.client.clone());
}

/// This device's credential key.
pub(crate) struct CredentialKey {
    secret: [u8; 32],
    public: [u8; 32],
}

impl CredentialKey {
    /// The key kept in `keychain`, made and kept the first time.
    pub fn load_or_create(keychain: &Keychain) -> Result<Self, String> {
        let secret = match keychain.device_key().map_err(|error| error.to_string())? {
            Some(secret) => secret,
            None => {
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
                keychain.store_device_key(&secret).map_err(|error| error.to_string())?;
                secret
            }
        };
        Ok(Self::from_secret(secret))
    }

    fn from_secret(secret: [u8; 32]) -> Self {
        Self { secret, public: MontgomeryPoint::mul_base_clamped(secret).to_bytes() }
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// The credential `sealed` holds for `recipient` (this device's endpoint id) and `account`;
    /// `None` for anything not sealed to this key for them.
    pub fn open(&self, recipient: &str, account: &str, sealed: &str) -> Option<Credential> {
        let data = base64::engine::general_purpose::STANDARD.decode(sealed).ok()?;
        let rest = data.strip_prefix(MAGIC.as_slice())?;
        if rest.len() < 32 + NONCE {
            return None;
        }
        let (ephemeral, rest) = rest.split_at(32);
        let (nonce, ciphertext) = rest.split_at(NONCE);
        let ephemeral: [u8; 32] = ephemeral.try_into().ok()?;
        let shared = MontgomeryPoint(ephemeral).mul_clamped(self.secret).to_bytes();
        let cipher = cipher(&shared, &ephemeral, &self.public)?;
        let aad = aad(recipient, account);
        let plain = cipher.decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad: aad.as_bytes() }).ok()?;
        serde_json::from_slice(&plain).ok()
    }
}

/// `credential` sealed to the device `recipient` whose credential key is `to`, for `account`.
pub(crate) fn seal(to: &[u8; 32], recipient: &str, account: &str, credential: &Credential) -> Result<String, String> {
    let mut ephemeral_secret = [0u8; 32];
    getrandom::fill(&mut ephemeral_secret).map_err(|error| error.to_string())?;
    let ephemeral = MontgomeryPoint::mul_base_clamped(ephemeral_secret).to_bytes();
    let shared = MontgomeryPoint(*to).mul_clamped(ephemeral_secret).to_bytes();
    let cipher = cipher(&shared, &ephemeral, to).ok_or("That device's credential key is not usable.")?;
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
    let plain = serde_json::to_vec(credential).map_err(|error| error.to_string())?;
    let aad = aad(recipient, account);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: aad.as_bytes() })
        .map_err(|_| "Couldn't encrypt the credential.".to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode([MAGIC.as_slice(), &ephemeral, &nonce, &ciphertext].concat()))
}

/// The AES key of one sealing; `None` when the exchange gave the all-zero point (a key of low
/// order, which only a forged key gives).
fn cipher(shared: &[u8; 32], ephemeral: &[u8; 32], recipient_key: &[u8; 32]) -> Option<Aes256Gcm> {
    if shared.iter().all(|byte| *byte == 0) {
        return None;
    }
    let material = [shared.as_slice(), ephemeral, recipient_key].concat();
    Some(Aes256Gcm::new(&blake3::derive_key(KEY_CONTEXT, &material).into()))
}

fn aad(recipient: &str, account: &str) -> String {
    format!("{}{recipient}/{account}", std::str::from_utf8(MAGIC).expect("ascii"))
}

/// `peer-sync/credentials.json`: when the credential this device holds for each account was
/// issued (Unix ms), whichever group it is in: by a sign-in here, or as another device sent it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Issued {
    #[serde(default)]
    issued: BTreeMap<String, u64>,
}

impl Issued {
    fn path(support_directory: &Path) -> PathBuf {
        support_directory.join("peer-sync").join(FILE)
    }

    fn read(support_directory: &Path) -> Self {
        std::fs::read(Self::path(support_directory))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn write(&self, support_directory: &Path) -> Result<(), EngineError> {
        let data = serde_json::to_vec_pretty(self).map_err(|error| EngineError::failed(error.to_string()))?;
        files::replace(&support_directory.join("peer-sync"), &Self::path(support_directory), &data)
            .map_err(|error| EngineError::failed(error.to_string()))
    }
}

/// Records that `account` signed in here at `at` (Unix ms), so its new credential goes to the
/// group's devices.
pub(crate) fn signed_in(support_directory: &Path, account: &str, at: u64) -> Result<(), EngineError> {
    let mut issued = Issued::read(support_directory);
    issued.issued.insert(account.to_string(), at);
    issued.write(support_directory)
}

/// One pass over the group's accounts, after the settings: what [`reconcile`] needs.
pub(crate) struct Pass<'a> {
    pub sync: &'a PeerSync,
    pub host: &'a dyn AccountHost,
    pub support_directory: &'a Path,
    /// This device's endpoint id.
    pub me: &'a str,
    /// The records as last reconciled (`None`: removed).
    pub bridged: &'a BTreeMap<String, Option<Value>>,
    /// Set until this device's first reconcile with a group it joined: records the group lacks
    /// are pushed as older than anything a device wrote ([`crate::device_sync`]'s `JOIN_AT`).
    pub joining: bool,
    pub join_at: u64,
}

/// What a pass did.
pub(crate) struct Reconciled {
    /// The records to bridge from now on.
    pub bridged: BTreeMap<String, Option<Value>>,
}

/// Brings this device's accounts and the group's together: announces this device's credential
/// key, pushes local record changes and removals, takes the group's (signing out of accounts
/// removed elsewhere, adopting credentials sealed to this device), and seals this device's
/// credentials to the devices that may have them.
pub(crate) async fn reconcile(
    pass: Pass<'_>,
    next_at: &mut (dyn FnMut(u64) -> u64 + Send),
) -> Result<Reconciled, EngineError> {
    let key = CredentialKey::load_or_create(pass.host.keychain()).map_err(EngineError::failed)?;
    let sync = pass.sync;
    let mut snapshot = sync.snapshot().await.map_err(map_error)?;
    let mut wrote = sync.announce_device_key(&key.public()).await.map_err(map_error)?;
    let mut issued = Issued::read(pass.support_directory);
    let local = local_records(pass.host);

    // This device's record changes and removals.
    let ids: BTreeSet<String> = local.keys().chain(pass.bridged.keys()).cloned().collect();
    for id in &ids {
        let doc_key = format!("v1/account/{id}");
        if snapshot.pending.contains(&doc_key) {
            continue;
        }
        let mine = local.get(id).cloned();
        let remote = snapshot.accounts.get(id);
        let base = pass.bridged.get(id);
        // Signed in here after another device removed it, or held here without this device ever
        // having synced it: this device's working sign-in wins over the removal.
        let revived = mine.is_some()
            && remote.is_some_and(|remote| {
                remote.value.is_none() && (base.is_none() || issued.issued.get(id).is_some_and(|at| *at > remote.at))
            });
        let should_push = revived
            || match base {
                Some(previous) => *previous != mine,
                None => mine.is_some() && remote.is_none(),
            };
        if should_push {
            let at = match base {
                Some(_) => next_at(remote.map_or(0, |remote| remote.at)),
                None if revived => next_at(remote.map_or(0, |remote| remote.at)),
                None if pass.joining => pass.join_at,
                None => next_at(0),
            };
            wrote |= sync.set_account(id, mine, at).await.map_err(map_error)?;
        }
    }
    if wrote {
        snapshot = sync.snapshot().await.map_err(map_error)?;
    }

    // The group's records, and credentials sealed to this device.
    let mut bridged = BTreeMap::new();
    let mut issued_changed = false;
    for (id, stamped) in &snapshot.accounts {
        let doc_key = format!("v1/account/{id}");
        if snapshot.pending.contains(&doc_key) {
            if let Some(previous) = pass.bridged.get(id) {
                bridged.insert(id.clone(), previous.clone());
            }
            continue;
        }
        let mine = local.get(id);
        match &stamped.value {
            None => {
                if mine.is_some() {
                    pass.host.remove(id.clone()).await.map_err(EngineError::failed)?;
                }
                bridged.insert(id.clone(), None);
            }
            Some(value) => {
                let Ok(record) = serde_json::from_value::<AccountRecord>(value.clone()) else { continue };
                if record.id != *id {
                    continue;
                }
                let slot = format!("{}/{id}", pass.me);
                let credential = snapshot
                    .credentials
                    .get(&slot)
                    .filter(|sealed| sealed.to == hex(&key.public()))
                    .filter(|sealed| issued.issued.get(id).is_none_or(|have| sealed.issued > *have))
                    .and_then(|sealed| Some((sealed.issued, key.open(pass.me, id, sealed.sealed.as_deref()?)?)));
                if mine == Some(value) && credential.is_none() {
                    bridged.insert(id.clone(), Some(value.clone()));
                    continue;
                }
                // A record this device lacks comes with a credential, or not yet.
                if mine.is_none() && credential.is_none() {
                    continue;
                }
                let at = credential.as_ref().map(|(at, _)| *at);
                pass.host.adopt(record, credential.map(|(_, credential)| credential)).map_err(EngineError::failed)?;
                if let Some(at) = at {
                    issued.issued.insert(id.clone(), at);
                    issued_changed = true;
                }
                bridged.insert(id.clone(), Some(value.clone()));
            }
        }
    }
    if issued_changed {
        issued.write(pass.support_directory)?;
    }

    // This device's credentials, sealed to each device that may have them; withdrawn for
    // accounts removed.
    let sign_ins = pass.host.keychain().load().map_err(|error| EngineError::failed(error.to_string()))?;
    let recipients = recipients(&snapshot, pass.me);
    for record in pass.host.accounts() {
        let (Some(refresh_token), Some(client)) =
            (sign_ins.refresh_tokens.get(&record.id), sign_ins.clients.get(&record.id))
        else {
            continue;
        };
        if snapshot.accounts.get(&record.id).is_none_or(|stamped| stamped.value.is_none()) {
            continue;
        }
        let credential = Credential { refresh_token: refresh_token.clone(), client: client.clone() };
        // Held from before device sync knew of it: older than any sign-in.
        let mine = issued.issued.get(&record.id).copied().unwrap_or(pass.join_at);
        for (recipient, to) in &recipients {
            let slot = format!("{recipient}/{}", record.id);
            if snapshot.pending.contains(&format!("v1/cred/{slot}")) {
                continue;
            }
            let existing = snapshot.credentials.get(&slot);
            let send = match existing {
                None => true,
                Some(existing) if existing.sealed.is_none() => mine > existing.at,
                Some(existing) => existing.issued < mine || (existing.issued == mine && existing.to != hex(to)),
            };
            if !send {
                continue;
            }
            let sealed = SealedCredential {
                to: hex(to),
                sealed: Some(seal(to, recipient, &record.id, &credential).map_err(EngineError::failed)?),
                issued: mine,
                at: next_at(existing.map_or(0, |existing| existing.at)),
            };
            sync.set_credential(recipient, &record.id, &sealed).await.map_err(map_error)?;
        }
    }
    for (slot, existing) in &snapshot.credentials {
        let Some((recipient, account)) = slot.split_once('/') else { continue };
        if existing.sealed.is_none() || snapshot.pending.contains(&format!("v1/cred/{slot}")) {
            continue;
        }
        let removed = snapshot.accounts.get(account).is_some_and(|stamped| stamped.value.is_none());
        if removed {
            let withdrawn = SealedCredential {
                to: existing.to.clone(),
                sealed: None,
                issued: existing.issued,
                at: next_at(existing.at),
            };
            sync.set_credential(recipient, account, &withdrawn).await.map_err(map_error)?;
        }
    }
    Ok(Reconciled { bridged })
}

/// The devices this device seals its credentials to, with their verified credential keys.
fn recipients(snapshot: &Snapshot, me: &str) -> Vec<(String, [u8; 32])> {
    snapshot
        .device_keys
        .iter()
        .filter(|(id, _)| id.as_str() != me && receives(snapshot, id))
        .filter_map(|(id, announced)| Some((id.clone(), announced.verified(id)?)))
        .collect()
}

/// Whether the device `id` gets the group's accounts: it announced a credential key.
pub(crate) fn receives(snapshot: &Snapshot, id: &str) -> bool {
    snapshot.device_keys.get(id).is_some_and(|announced| announced.verified(id).is_some())
}

/// This device's records as the group keeps them, by id.
fn local_records(host: &dyn AccountHost) -> BTreeMap<String, Value> {
    host.accounts()
        .into_iter()
        .filter_map(|record| Some((record.id.clone(), serde_json::to_value(&record).ok()?)))
        .collect()
}

fn map_error(error: crate::peer_sync::PeerSyncError) -> EngineError {
    EngineError::failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> Credential {
        Credential {
            refresh_token: "1//refresh".into(),
            client: OAuthClient { id: "client".into(), secret: Some("secret".into()) },
        }
    }

    #[test]
    fn only_the_recipient_opens_a_credential_for_its_slot() {
        let recipient = CredentialKey::from_secret([3; 32]);
        let other = CredentialKey::from_secret([4; 32]);
        let sealed = seal(&recipient.public(), "device-b", "me@x.com", &credential()).unwrap();
        // Nothing of the token or the client secret shows.
        let raw = base64::engine::general_purpose::STANDARD.decode(&sealed).unwrap();
        assert!(!raw.windows(7).any(|window| window == b"refresh" || window == b"secret\""));
        assert_eq!(recipient.open("device-b", "me@x.com", &sealed), Some(credential()));
        // Another device's key, another slot or another account opens nothing.
        assert_eq!(other.open("device-b", "me@x.com", &sealed), None);
        assert_eq!(recipient.open("device-c", "me@x.com", &sealed), None);
        assert_eq!(recipient.open("device-b", "you@x.com", &sealed), None);
        // A damaged one neither.
        let mut damaged = raw.clone();
        *damaged.last_mut().unwrap() ^= 1;
        let damaged = base64::engine::general_purpose::STANDARD.encode(damaged);
        assert_eq!(recipient.open("device-b", "me@x.com", &damaged), None);
        // A key of low order is refused rather than sealed to.
        assert!(seal(&[0; 32], "device-b", "me@x.com", &credential()).is_err());
    }

    #[test]
    fn keeps_the_key_in_the_credential_store() {
        let name = format!("mailstream-credkey-{}-{}", std::process::id(), crate::testkit::unique());
        let directory = std::env::temp_dir().join(&name);
        let keychain = Keychain::new(format!("dev.mailstream.test.{name}"), &directory);
        let first = CredentialKey::load_or_create(&keychain).unwrap();
        let again = CredentialKey::load_or_create(&keychain).unwrap();
        assert_eq!(first.public(), again.public());
        assert_ne!(first.public(), [0; 32]);
        keychain.forget_device_key().unwrap();
        assert_ne!(CredentialKey::load_or_create(&keychain).unwrap().public(), first.public());
        keychain.forget_device_key().unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }
}
