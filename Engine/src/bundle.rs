//! Build-time configuration in the host app's Info.plist (scripts/bundle.sh writes it into
//! Mailstream.app; never committed), as the Swift engine's `AppConfig` reads it.
use core_foundation::bundle::CFBundle;
use core_foundation::string::CFString;

use crate::oauth::{BuiltInClients, OAuthClient, Provider};
use crate::push::PushConfig;

/// The main bundle's Info.plist item `key`; missing keys and empty strings read as absent.
fn value(key: &str) -> Option<String> {
    let info = CFBundle::main_bundle().info_dictionary();
    let text = info.find(CFString::new(key))?.downcast::<CFString>()?.to_string();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The OAuth clients the main bundle's Info.plist carries (`MSGoogleClientID`/`Secret`,
/// `MSMicrosoftClientID`/`Secret`).
pub fn built_in_clients() -> BuiltInClients {
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

/// The push relay the main bundle's Info.plist names (`MSPushURL`, `MSPushToken`,
/// `MSGraphClientState`, `MSGmailTopic`); without the first two there is no push.
pub fn push_config() -> PushConfig {
    PushConfig {
        url: value("MSPushURL"),
        token: value("MSPushToken"),
        graph_client_state: value("MSGraphClientState"),
        gmail_topic: value("MSGmailTopic"),
        timing: Default::default(),
    }
}

/// The main bundle's `CFBundleShortVersionString`; None outside an app bundle (tools, tests).
pub fn short_version() -> Option<String> {
    value("CFBundleShortVersionString")
}
