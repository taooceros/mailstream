//! Device sync's Cloudflare drop (MS-374): for devices of one sync group that are never online at
//! the same time and have no always-on node (`mailstream sync daemon`) between them. Off until
//! the user names their own Worker (`SyncDrop/worker.js`). Then each device keeps its copy of the
//! group's state (trusted senders with their tombstones, the synced settings, the devices) on the
//! Worker and merges the other devices' copies into its replica, each entry at its own time, so
//! the usual rule (the latest `at` wins, tombstones included) decides as if the devices had met.
//!
//! Everything the Worker sees is derived from the group's write secret, which only the group's
//! devices hold, and once a device was removed, from the drop key only the devices still in the
//! group get (`PeerSync::drop_secret`, MS-396), so the drop then moves where the removed device
//! can't follow and each device deletes its copy at the old place:
//! - the group's address (64 hex), and per device an opaque slot (32 hex, keyed by the device's
//!   endpoint id);
//! - the bearer token whose SHA-256 the group's first write registers, so nobody else can read
//!   or replace the group's copies;
//! - the AES-256-GCM key each copy is sealed with (`msd1` ‖ 12-byte nonce ‖ ciphertext, the
//!   group and slot as associated data): the Worker stores ciphertext only.
//!
//! An exchange lists the group's copies, merges the others', then uploads this device's when it
//! changed since the last upload or the Worker lost it. Exchanges run after every local or synced
//! change, on start, after a network change or wake, and every few minutes (`DeviceSync`).
//!
//! `peer-sync/cloud.json` caches the active sync group's Worker URL and enabled flag (replicated
//! via `v1/cloud` in the sync group document, MS-419), and records what was last uploaded where
//! (with its token), so turning it off, resetting, or leaving the group deletes that copy.
/// The `SyncDrop/` Worker deployed for this project's devices (MS-374): offered by default so a
/// device does not have to type a URL, still off until turned on.
pub(crate) const DEFAULT_WORKER_URL: &str = "https://mailstream-sync-drop.ync-rop.workers.dev";

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload as Sealed};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answers::SyncCloud;
use crate::peer_sync::{CloudConfig, Device, DeviceKey, PeerSync, PeerSyncError, SealedCredential, Snapshot, Stamped};
use crate::{EngineError, api, files, model};

const FILE: &str = "cloud.json";
const MAGIC: &[u8; 4] = b"msd1";
const NONCE: usize = 12;
/// How often an idle device looks for other devices' changes.
pub(crate) const POLL: Duration = Duration::from_secs(300);
/// How soon a failed exchange is tried again.
pub(crate) const RETRY: Duration = Duration::from_secs(60);

/// `cloud.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudFile {
    /// The sync group whose `enabled` and `url` are cached here; `None` only in pre-MS-419 files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upload: Option<Upload>,
}

/// The copy this device last uploaded: where, with which token, and a digest of what it held.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Upload {
    url: String,
    group: String,
    slot: String,
    token: String,
    digest: String,
}

/// What the Worker knows a group by, derived from its write secret.
struct Keys {
    group: String,
    token: String,
    slot_key: [u8; 32],
    cipher: Aes256Gcm,
}

impl Keys {
    fn derive(secret: &[u8; 32]) -> Self {
        let derive = |context: &str| blake3::derive_key(context, secret);
        Self {
            group: hex(&derive("mailstream device sync drop v1 group")),
            token: hex(&derive("mailstream device sync drop v1 token")),
            slot_key: derive("mailstream device sync drop v1 slot"),
            cipher: Aes256Gcm::new(&derive("mailstream device sync drop v1 cipher").into()),
        }
    }

    /// The slot of the device `endpoint_id`.
    fn slot(&self, endpoint_id: &str) -> String {
        hex(blake3::keyed_hash(&self.slot_key, endpoint_id.as_bytes()).as_bytes())[..32].to_string()
    }

    fn aad(&self, slot: &str) -> String {
        format!("{}/{slot}", self.group)
    }

    fn seal(&self, slot: &str, plain: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let aad = self.aad(slot);
        let sealed = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), Sealed { msg: plain, aad: aad.as_bytes() })
            .map_err(|_| "Couldn't encrypt this device's copy.".to_string())?;
        Ok([MAGIC.as_slice(), &nonce, &sealed].concat())
    }

    /// `None` for anything not sealed by a device of this group for `slot`.
    fn open(&self, slot: &str, data: &[u8]) -> Option<Vec<u8>> {
        let rest = data.strip_prefix(MAGIC.as_slice())?;
        if rest.len() < NONCE {
            return None;
        }
        let (nonce, sealed) = rest.split_at(NONCE);
        let aad = self.aad(slot);
        self.cipher.decrypt(Nonce::from_slice(nonce), Sealed { msg: sealed, aad: aad.as_bytes() }).ok()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One device's copy, as sealed.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Contents {
    #[serde(default)]
    trusted: BTreeMap<String, TrustedEntry>,
    #[serde(default)]
    settings: BTreeMap<String, SettingEntry>,
    #[serde(default)]
    devices: BTreeMap<String, Device>,
    /// Account records and sealed credentials (MS-376): the credentials are sealed to each
    /// device's own key besides this copy's group key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    accounts: BTreeMap<String, AccountEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    device_keys: BTreeMap<String, DeviceKey>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    credentials: BTreeMap<String, SealedCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud: Option<CloudConfig>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct AccountEntry {
    record: Option<Value>,
    at: u64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct TrustedEntry {
    trusted: bool,
    at: u64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct SettingEntry {
    value: Value,
    at: u64,
}

impl Contents {
    fn of(snapshot: Snapshot) -> Self {
        Self {
            trusted: snapshot
                .trusted
                .into_iter()
                .map(|(address, stamped)| (address, TrustedEntry { trusted: stamped.value, at: stamped.at }))
                .collect(),
            settings: snapshot
                .settings
                .into_iter()
                .map(|(key, stamped)| (key, SettingEntry { value: stamped.value, at: stamped.at }))
                .collect(),
            devices: snapshot.devices,
            accounts: snapshot
                .accounts
                .into_iter()
                .map(|(id, stamped)| (id, AccountEntry { record: stamped.value, at: stamped.at }))
                .collect(),
            device_keys: snapshot.device_keys,
            credentials: snapshot.credentials,
            cloud: snapshot.cloud,
        }
    }

    fn into_snapshot(self) -> Snapshot {
        Snapshot {
            trusted: self
                .trusted
                .into_iter()
                .map(|(address, entry)| (address, Stamped { value: entry.trusted, at: entry.at }))
                .collect(),
            settings: self
                .settings
                .into_iter()
                .map(|(key, entry)| (key, Stamped { value: entry.value, at: entry.at }))
                .collect(),
            devices: self.devices,
            // Admissions and removals cross only between admitted devices (MS-380), never
            // through the drop.
            admitted: Default::default(),
            removed: Default::default(),
            // Sealed to each device's key for the drop's own keys: never in the drop (MS-396).
            drop_keys: Default::default(),
            accounts: self
                .accounts
                .into_iter()
                .map(|(id, entry)| (id, Stamped { value: entry.record, at: entry.at }))
                .collect(),
            device_keys: self.device_keys,
            credentials: self.credentials,
            cloud: self.cloud,
            pending: Default::default(),
        }
    }
}

/// The Worker's answer to listing a group.
#[derive(Deserialize)]
struct Listing {
    slots: Vec<Slot>,
}

#[derive(Deserialize)]
struct Slot {
    slot: String,
    data: String,
}

/// What an exchange did.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Exchange {
    /// Another device's copy brought something this replica lacked.
    pub merged: bool,
    /// The status changed (an upload, something received, a problem appeared or went away).
    pub changed: bool,
}

#[derive(Default)]
struct Meta {
    last_upload: Option<u64>,
    last_received: Option<u64>,
    problem: Option<String>,
}

/// The drop of one support folder.
pub(crate) struct CloudDrop {
    directory: PathBuf,
    file: Mutex<CloudFile>,
    meta: Mutex<Meta>,
    /// One exchange at a time, so two never upload the same copy or race a delete.
    exchanging: tokio::sync::Mutex<()>,
}

impl CloudDrop {
    pub(crate) fn new(support_directory: &Path) -> Self {
        let directory = support_directory.join("peer-sync");
        let (file, problem) = match std::fs::read(directory.join(FILE)) {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(file) => (file, None),
                Err(error) => (CloudFile::default(), Some(format!("peer-sync/{FILE}: {error}"))),
            },
            Err(_) => (CloudFile::default(), None),
        };
        Self {
            directory,
            file: Mutex::new(file),
            meta: Mutex::new(Meta { problem, ..Meta::default() }),
            exchanging: tokio::sync::Mutex::new(()),
        }
    }

    fn save(&self, file: &CloudFile) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let data = serde_json::to_vec_pretty(file).map_err(|error| error.to_string())?;
        files::replace(&self.directory, &self.directory.join(FILE), &data).map_err(|error| error.to_string())
    }

    fn update(&self, change: impl FnOnce(&mut CloudFile)) -> Result<(), String> {
        let mut file = self.file.lock().expect("cloud file lock");
        let mut next = file.clone();
        change(&mut next);
        if next != *file {
            self.save(&next)?;
            *file = next;
        }
        Ok(())
    }

    pub(crate) fn status(&self, namespace: Option<&str>) -> SyncCloud {
        let file = self.file.lock().expect("cloud file lock");
        let meta = self.meta.lock().expect("cloud meta lock");
        let for_group = namespace.is_some() && (file.namespace.is_none() || file.namespace.as_deref() == namespace);
        if !for_group {
            return SyncCloud {
                enabled: false,
                url: Some(DEFAULT_WORKER_URL.to_owned()),
                last_upload: None,
                last_received: None,
                problem: if namespace.is_some() { meta.problem.clone() } else { None },
            };
        }
        SyncCloud {
            enabled: file.enabled,
            url: Some(file.url.clone().unwrap_or_else(|| DEFAULT_WORKER_URL.to_owned())),
            last_upload: meta.last_upload.map(iso_ms),
            last_received: meta.last_received.map(iso_ms),
            problem: meta.problem.clone(),
        }
    }

    /// Pre-MS-419 `cloud.json` had no `namespace` field; returns its enabled Worker URL once so
    /// the device's existing sync group adopts it on first reconcile.
    pub(crate) fn legacy_enabled_url(&self) -> Option<String> {
        let file = self.file.lock().expect("cloud file lock");
        if file.namespace.is_none() && file.enabled {
            file.url.as_deref().and_then(|url| worker_url(url).ok())
        } else {
            None
        }
    }

    /// Whether an uploaded slot from a disabled or previous sync group is still waiting to be
    /// deleted from the Worker.
    pub(crate) fn has_pending_cleanup(&self, namespace: &str) -> bool {
        let file = self.file.lock().expect("cloud file lock");
        file.upload.is_some() && (!file.enabled || file.namespace.as_deref() != Some(namespace))
    }

    /// Mirrors the sync group's `v1/cloud` setting into `cloud.json`. Returns `true` when the
    /// effective state for this group changed.
    pub(crate) fn apply_group(&self, namespace: &str, config: Option<&CloudConfig>) -> Result<bool, EngineError> {
        let url = config.and_then(|c| c.url.as_deref()).and_then(|u| worker_url(u).ok());
        let enabled = config.is_some_and(|c| c.enabled) && url.is_some();
        self.set_group(namespace, enabled, url, false)
    }

    /// Turns the drop on for `namespace` with the validated Worker at `url`; the next exchange uploads.
    pub(crate) fn enable_for_group(&self, namespace: &str, url: String) -> Result<(), EngineError> {
        self.set_group(namespace, true, Some(url), false).map(drop)
    }

    /// Records the drop as off for `namespace` in `cloud.json`. Call while holding the bridge
    /// lock right after writing `v1/cloud`, then follow with [`Self::cleanup_disabled_upload`].
    pub(crate) fn mark_disabled_for_group(&self, namespace: &str, url: Option<String>) -> Result<(), EngineError> {
        self.set_group(namespace, false, url, true).map(drop)
    }

    fn set_group(&self, namespace: &str, enabled: bool, url: Option<String>, always_reset: bool) -> Result<bool, EngineError> {
        let mut changed = false;
        self.update(|file| {
            changed = file.namespace.as_deref() != Some(namespace) || file.enabled != enabled || file.url != url;
            file.namespace = Some(namespace.to_owned());
            file.enabled = enabled;
            file.url = url;
        })
        .map_err(EngineError::failed)?;
        let mut meta = self.meta.lock().expect("cloud meta lock");
        if changed || always_reset {
            meta.last_upload = None;
            meta.last_received = None;
            meta.problem = None;
        } else if enabled {
            meta.problem = None;
        }
        Ok(changed)
    }

    /// Deletes this device's uploaded copy from the Worker if the drop is still off for `namespace`;
    /// a copy the Worker couldn't delete shows as the problem.
    pub(crate) async fn cleanup_disabled_upload(&self, namespace: &str) {
        let _exchanging = self.exchanging.lock().await;
        let still_disabled = {
            let file = self.file.lock().expect("cloud file lock");
            !file.enabled || file.namespace.as_deref() != Some(namespace)
        };
        if !still_disabled {
            return;
        }
        let result = self.delete_upload().await;
        let mut meta = self.meta.lock().expect("cloud meta lock");
        if let Err(problem) = result {
            meta.problem = Some(problem);
        }
    }

    /// Deletes the copy `cloud.json` records, if any, and forgets it once the Worker did.
    async fn delete_upload(&self) -> Result<(), String> {
        let upload = self.file.lock().expect("cloud file lock").upload.clone();
        let Some(upload) = upload else { return Ok(()) };
        let response = api::HTTP
            .delete(slot_url(&upload.url, &upload.group, &upload.slot))
            .bearer_auth(&upload.token)
            .send()
            .await
            .map_err(|error| format!("Couldn't reach the Cloudflare Worker: {error}"))?;
        // 403: the group's token changed, so this copy can't be the one that was uploaded.
        if !response.status().is_success() && response.status() != reqwest::StatusCode::FORBIDDEN {
            return Err(refused(response.status()));
        }
        self.update(|file| file.upload = None)
    }

    /// Lists the group's copies, merges the other devices' into `sync`, and uploads this
    /// device's when it changed, under keys derived with `drop_key` once a device was removed
    /// (MS-396). Does nothing while the drop is off or sync isn't running.
    pub(crate) async fn exchange(&self, sync: &PeerSync, drop_key: Option<&[u8; 32]>) -> Exchange {
        let _exchanging = self.exchanging.lock().await;
        let active_namespace = sync.status().await.namespace;
        let (url, has_upload) = {
            let file = self.file.lock().expect("cloud file lock");
            let active = active_namespace.is_some()
                && file.namespace.as_deref() == active_namespace.as_deref()
                && file.enabled;
            (active.then(|| file.url.clone()).flatten(), file.upload.is_some())
        };
        let Some(url) = url else {
            if has_upload {
                // Delete a copy left behind when the group turned cloud fallback off or this
                // device switched/reset groups (MS-419).
                self.delete_upload().await.ok();
            }
            return Exchange::default();
        };
        let result = match sync.drop_secret(drop_key).await {
            Ok(Some(secret)) => {
                let endpoint_id = sync.status().await.endpoint_id;
                self.exchange_with(sync, &url, &Keys::derive(&secret), &endpoint_id).await
            }
            Ok(None) => return Exchange::default(),
            Err(error) => Err(error.to_string()),
        };
        let mut meta = self.meta.lock().expect("cloud meta lock");
        let now = now_ms();
        let (exchange, problem) = match result {
            Ok((merged, uploaded)) => {
                if merged {
                    meta.last_received = Some(now);
                }
                if uploaded {
                    meta.last_upload = Some(now);
                }
                (Exchange { merged, changed: merged || uploaded }, None)
            }
            Err(problem) => (Exchange::default(), Some(problem)),
        };
        let changed = exchange.changed || meta.problem != problem;
        meta.problem = problem;
        Exchange { changed, ..exchange }
    }

    /// (merged, uploaded)
    async fn exchange_with(&self, sync: &PeerSync, url: &str, keys: &Keys, endpoint_id: &str) -> Result<(bool, bool), String> {
        let own = keys.slot(endpoint_id);
        let previous = self.file.lock().expect("cloud file lock").upload.clone();
        if previous.as_ref().is_some_and(|upload| upload.url != url || upload.group != keys.group || upload.slot != own) {
            // A copy of a group this device left, or on another Worker.
            self.delete_upload().await.ok();
        }

        let response = api::HTTP
            .get(format!("{url}/v1/groups/{}", keys.group))
            .bearer_auth(&keys.token)
            .send()
            .await
            .map_err(|error| format!("Couldn't reach the Cloudflare Worker: {error}"))?;
        if !response.status().is_success() {
            return Err(refused(response.status()));
        }
        let body = response.bytes().await.map_err(|error| format!("The Cloudflare Worker's answer: {error}"))?;
        let listing: Listing =
            serde_json::from_slice(&body).map_err(|error| format!("The Cloudflare Worker's answer: {error}"))?;

        let mut merged = false;
        let mut own_present = false;
        let mut unreadable = 0;
        for slot in listing.slots {
            if slot.slot == own {
                own_present = true;
                continue;
            }
            let copy = base64::engine::general_purpose::STANDARD
                .decode(&slot.data)
                .ok()
                .and_then(|data| keys.open(&slot.slot, &data))
                .and_then(|plain| serde_json::from_slice::<Contents>(&plain).ok());
            match copy {
                Some(copy) => merged |= sync.merge(&copy.into_snapshot()).await.map_err(peer_error)?,
                None => unreadable += 1,
            }
        }

        let snapshot = sync.snapshot().await.map_err(peer_error)?;
        let plain = serde_json::to_vec(&Contents::of(snapshot)).map_err(|error| error.to_string())?;
        let digest = blake3::hash(&plain).to_hex().to_string();
        let current = previous.filter(|upload| upload.url == url && upload.group == keys.group && upload.slot == own);
        let uploaded = if own_present && current.is_some_and(|upload| upload.digest == digest) {
            false
        } else {
            let sealed = keys.seal(&own, &plain)?;
            let response = api::HTTP
                .put(slot_url(url, &keys.group, &own))
                .bearer_auth(&keys.token)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(sealed)
                .send()
                .await
                .map_err(|error| format!("Couldn't reach the Cloudflare Worker: {error}"))?;
            if !response.status().is_success() {
                return Err(refused(response.status()));
            }
            let upload = Upload {
                url: url.to_string(),
                group: keys.group.clone(),
                slot: own,
                token: keys.token.clone(),
                digest,
            };
            self.update(|file| file.upload = Some(upload))?;
            true
        };
        if unreadable > 0 {
            return Err(format!(
                "{unreadable} cop{} on the Cloudflare Worker couldn't be decrypted with this sync group's key.",
                if unreadable == 1 { "y" } else { "ies" }
            ));
        }
        Ok((merged, uploaded))
    }
}

/// Validates `url` as a Worker URL, returning the normalized address without a trailing slash.
pub(crate) fn validate_url(url: &str) -> Result<String, EngineError> {
    worker_url(url).map_err(|message| EngineError::new("invalid", message))
}

/// `url` without a trailing slash, when it is an https URL (or http on this machine, for a local
/// `wrangler dev`) without a query.
fn worker_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(trimmed).map_err(|error| format!("Not a URL: {error}"))?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        return Err("The Worker's URL must start with https://.".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || parsed.host_str().is_none() {
        return Err("The Worker's URL is its address alone, like https://mailstream-sync-drop.you.workers.dev.".into());
    }
    Ok(trimmed.to_string())
}

fn slot_url(url: &str, group: &str, slot: &str) -> String {
    format!("{url}/v1/groups/{group}/slots/{slot}")
}

fn refused(status: reqwest::StatusCode) -> String {
    let reason = match status.as_u16() {
        401 | 403 => "refused this sync group's key",
        404 => "has no sync drop at this URL",
        409 => "holds too many devices for this sync group",
        413 => "refused a copy this large",
        _ => "failed",
    };
    format!("The Cloudflare Worker {reason} (HTTP {}).", status.as_u16())
}

fn peer_error(error: PeerSyncError) -> String {
    error.to_string()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn iso_ms(ms: u64) -> String {
    model::iso8601(model::date_from_unix(ms as f64 / 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_copies_open_only_for_their_group_and_slot() {
        let keys = Keys::derive(&[7; 32]);
        let other = Keys::derive(&[8; 32]);
        let slot = keys.slot("device-a");
        assert_eq!(keys.group.len(), 64);
        assert_eq!(slot.len(), 32);
        assert_ne!(keys.group, other.group);
        assert_ne!(keys.token, other.token);
        let sealed = keys.seal(&slot, b"{\"trusted\":{}}").unwrap();
        assert!(!sealed.windows(9).any(|window| window == b"\"trusted\""));
        assert_eq!(keys.open(&slot, &sealed).unwrap(), b"{\"trusted\":{}}");
        assert!(keys.open(&keys.slot("device-b"), &sealed).is_none());
        assert!(other.open(&slot, &sealed).is_none());
        assert!(keys.open(&slot, &sealed[..10]).is_none());
    }

    #[test]
    fn worker_urls_are_https_or_local() {
        assert_eq!(worker_url(" https://drop.you.workers.dev/ ").unwrap(), "https://drop.you.workers.dev");
        assert_eq!(worker_url("http://127.0.0.1:8787").unwrap(), "http://127.0.0.1:8787");
        assert!(worker_url("http://drop.you.workers.dev").is_err());
        assert!(worker_url("https://drop.you.workers.dev/?token=1").is_err());
        assert!(worker_url("drop.you.workers.dev").is_err());
    }

    #[test]
    fn default_worker_url_is_offered_while_off() {
        let directory = std::env::temp_dir().join(format!("mailstream-cloud-drop-default-{}", std::process::id()));
        let drop = CloudDrop::new(&directory);
        let status = drop.status(None);
        assert!(!status.enabled);
        assert_eq!(status.url.as_deref(), Some(DEFAULT_WORKER_URL));
    }
}
