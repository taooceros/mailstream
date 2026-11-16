//! One-time pairing codes for device sync (MS-376): a code holds no group key, only the issuing
//! device's address, the group's id and a random secret. The joining device dials the issuer at
//! the group's admission door (`peer_sync.rs`, MS-380) and presents the secret; the issuer checks
//! that it issued it less than [`LIFETIME`] ago and nobody used it yet, admits the joiner and only
//! then hands over the group's ticket on that connection. A code expires because the issuer
//! forgets its secret: copied later, or used a second time, it opens nothing. The issuer keeps its
//! open codes in memory, so it must stay running until the other device joined.
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

use crate::peer_sync::{hex, unhex};

/// How long a code stays redeemable.
pub(crate) const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// What every code starts with.
const PREFIX: &str = "mailstream-pair-";

/// What a code carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Code {
    /// The issuing device and where it was reachable.
    pub issuer: EndpointAddr,
    /// The group's id, so a device already in it needs no secret.
    pub namespace: String,
    /// 32 random bytes, hex.
    secret: String,
}

impl Code {
    /// A new code for `issuer`'s group `namespace`, with `secret` from [`Invites::issue`].
    pub fn new(issuer: EndpointAddr, namespace: &str, secret: &[u8; 32]) -> Self {
        Self { issuer, namespace: namespace.to_string(), secret: hex(secret) }
    }

    pub fn secret(&self) -> Option<[u8; 32]> {
        unhex(&self.secret)?.try_into().ok()
    }
}

impl fmt::Display for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_vec(self).map_err(|_| fmt::Error)?;
        write!(formatter, "{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
    }
}

impl FromStr for Code {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.starts_with("doc") {
            return Err("This is an old device sync ticket, which no longer pairs a device. Create a new pairing code \
                        on a device already in the group."
                .into());
        }
        let not_a_code = || "Not a Mailstream pairing code.".to_string();
        let encoded = text.strip_prefix(PREFIX).ok_or_else(not_a_code)?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).map_err(|_| not_a_code())?;
        let code: Self = serde_json::from_slice(&json).map_err(|_| not_a_code())?;
        code.secret().ok_or_else(not_a_code)?;
        Ok(code)
    }
}

/// Why a code was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Never issued here, already used, or this device restarted since.
    Unknown,
    Expired,
}

impl fmt::Display for Refused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => {
                "The other device doesn't know this pairing code: it was used already, or that device restarted. \
                 Create a new one."
            }
            Self::Expired => "This pairing code expired (codes last 10 minutes). Create a new one.",
        })
    }
}

/// The codes this device issued and nobody redeemed yet, by the hash of their secret.
#[derive(Debug, Default)]
pub(crate) struct Invites {
    open: Mutex<HashMap<[u8; 32], Instant>>,
}

impl Invites {
    /// A new secret, redeemable once within [`LIFETIME`]; with when it expires, Unix ms.
    pub fn issue(&self) -> Result<([u8; 32], u64), String> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
        let now = Instant::now();
        let mut open = self.open.lock().expect("invites lock");
        open.retain(|_, issued| now.duration_since(*issued) < LIFETIME);
        open.insert(*blake3::hash(&secret).as_bytes(), now);
        Ok((secret, unix_ms() + LIFETIME.as_millis() as u64))
    }

    /// Uses up `secret`: fine once, within its lifetime.
    pub fn redeem(&self, secret: &[u8; 32]) -> Result<(), Refused> {
        let issued =
            self.open.lock().expect("invites lock").remove(blake3::hash(secret).as_bytes()).ok_or(Refused::Unknown)?;
        if issued.elapsed() >= LIFETIME {
            return Err(Refused::Expired);
        }
        Ok(())
    }
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_old_tickets_are_refused() {
        let issuer = EndpointAddr::new(iroh::SecretKey::from_bytes(&[1; 32]).public());
        let text = Code::new(issuer.clone(), "abc", &[9; 32]).to_string();
        assert!(text.starts_with(PREFIX));
        let parsed: Code = text.parse().unwrap();
        assert_eq!((parsed.secret(), parsed.namespace.as_str(), parsed.issuer), (Some([9; 32]), "abc", issuer));
        assert!("docaaaa".parse::<Code>().unwrap_err().contains("old device sync ticket"));
        assert!("mailstream-pair-!!".parse::<Code>().is_err());
    }

    #[test]
    fn a_secret_redeems_once() {
        let invites = Invites::default();
        let (secret, expires) = invites.issue().unwrap();
        assert!(expires > unix_ms() + LIFETIME.as_millis() as u64 - 5_000);
        assert_eq!(invites.redeem(&[0; 32]), Err(Refused::Unknown));
        assert_eq!(invites.redeem(&secret), Ok(()));
        assert_eq!(invites.redeem(&secret), Err(Refused::Unknown));
        // One issued longer ago than its lifetime.
        let (old, _) = invites.issue().unwrap();
        let past = Instant::now().checked_sub(LIFETIME + Duration::from_secs(1)).unwrap();
        invites.open.lock().unwrap().insert(*blake3::hash(&old).as_bytes(), past);
        assert_eq!(invites.redeem(&old), Err(Refused::Expired));
    }
}
