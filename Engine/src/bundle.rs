//! Build-time configuration in the host app's Info.plist (scripts/bundle.sh writes it into
//! Mailstream.app; never committed), as the Swift engine's `AppConfig` reads it.
use core_foundation::bundle::CFBundle;
use core_foundation::string::CFString;

use crate::oauth::{BuiltInClients, OAuthClient, Provider};

/// The OAuth clients the main bundle's Info.plist carries (`MSGoogleClientID`/`Secret`,
/// `MSMicrosoftClientID`/`Secret`). Missing keys and empty strings read as absent.
pub fn built_in_clients() -> BuiltInClients {
    let info = CFBundle::main_bundle().info_dictionary();
    let value = |key: &str| {
        let text = info.find(CFString::new(key))?.downcast::<CFString>()?.to_string();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    };
    let mut clients = BuiltInClients::new();
    for (provider, id, secret) in [
        (Provider::Google, "MSGoogleClientID", "MSGoogleClientSecret"),
        (Provider::Microsoft, "MSMicrosoftClientID", "MSMicrosoftClientSecret"),
    ] {
        if let Some(id) = value(id) {
            clients.insert(provider, OAuthClient { id, secret: value(secret) });
        }
    }
    clients
}
