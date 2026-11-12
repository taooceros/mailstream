//! Automatic updates on the Mac (MS-219), as Mailstream Flutter's (`Flutter/README.md`,
//! Application updates): Sparkle 2.10.1 owns scheduling, downloading, verification, installation
//! on quit and relaunch. `scripts/gpui-bundle.sh` puts the framework in the app's Frameworks and
//! the feed, public key and channel in its Info.plist; it is loaded at run time, so the binary
//! links nothing new and runs without it (`cargo run` outside a bundle has no updater). The
//! application menu gets Check for Updates… and Automatically Check for Updates.

use std::cell::OnceCell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenuItem};
use objc2_foundation::{NSBundle, NSNumber, NSSet, NSString};

/// Starts Sparkle and adds its menu items, when this is a configured app bundle. Call after
/// `set_menus`.
pub fn start() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let bundle = NSBundle::mainBundle();
    let Some(frameworks) = bundle.privateFrameworksPath() else { return };
    let Some(sparkle) = NSBundle::bundleWithPath(&NSString::from_str(&format!("{frameworks}/Sparkle.framework")))
    else {
        return;
    };
    let settings = Settings {
        key: string(&bundle, "SUPublicEDKey"),
        feed: string(&bundle, "SUFeedURL"),
        verify_archive: number(&bundle, "SUVerifyUpdateBeforeExtraction").is_some_and(|n| n.as_bool()),
        signed_feed: number(&bundle, "SURequireSignedFeed").is_some_and(|n| n.as_bool()),
        expiry: number(&bundle, "SUSignedFeedFailureExpirationInterval").map(|n| n.as_i64()),
        channel: string(&bundle, "MSUpdateChannel"),
    };
    if let Some(problem) = configuration_error(&settings) {
        eprintln!("updates off: {problem}");
        return;
    }
    let Some(class) = unsafe { sparkle.load() }.then(|| AnyClass::get(c"SPUStandardUpdaterController")).flatten()
    else {
        eprintln!("updates off: Sparkle.framework did not load");
        return;
    };
    let delegate = Delegate::new(mtm, settings.channel.as_deref() == Some("prerelease"));
    let controller: Retained<AnyObject> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![allocated, initWithStartingUpdater: false, updaterDelegate: &*delegate, userDriverDelegate: None::<&AnyObject>]
    };
    let updater: Retained<AnyObject> = unsafe { msg_send![&*controller, updater] };
    let mut error: *mut AnyObject = std::ptr::null_mut();
    let started: bool = unsafe { msg_send![&*updater, startUpdater: &mut error as *mut *mut AnyObject] };
    if !started {
        let reason = unsafe { error.as_ref() }
            .map(|error| unsafe { msg_send![error, localizedDescription] })
            .map(|text: Retained<NSString>| text.to_string())
            .unwrap_or_default();
        eprintln!("updates off: Sparkle did not start: {reason}");
        return;
    }
    delegate.ivars().updater.set(updater).ok();

    let app = NSApplication::sharedApplication(mtm);
    if let Some(menu) = app.mainMenu().and_then(|bar| bar.itemAtIndex(0)).and_then(|item| item.submenu()) {
        let check = item(mtm, "Check for Updates…", sel!(checkForUpdates:));
        let automatic = item(mtm, "Automatically Check for Updates", sel!(toggleAutomaticChecks:));
        unsafe {
            // The controller enables Check for Updates… only while a check can start.
            check.setTarget(Some(&controller));
            automatic.setTarget(Some(&delegate));
        }
        menu.insertItem_atIndex(&check, 0);
        menu.insertItem_atIndex(&automatic, 1);
        menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), 2);
    }
    // Both live as long as the app; menu targets and Sparkle's delegate are weak references.
    std::mem::forget(controller);
    std::mem::forget(delegate);
}

fn item(mtm: MainThreadMarker, title: &str, action: objc2::runtime::Sel) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::new(),
        )
    }
}

fn string(bundle: &NSBundle, key: &str) -> Option<String> {
    let value = bundle.objectForInfoDictionaryKey(&NSString::from_str(key))?;
    Some(value.downcast::<NSString>().ok()?.to_string())
}

fn number(bundle: &NSBundle, key: &str) -> Option<Retained<NSNumber>> {
    bundle.objectForInfoDictionaryKey(&NSString::from_str(key))?.downcast::<NSNumber>().ok()
}

/// The Info.plist items the updater requires (Flutter's `AppUpdater.configurationError`).
struct Settings {
    key: Option<String>,
    feed: Option<String>,
    verify_archive: bool,
    signed_feed: bool,
    expiry: Option<i64>,
    channel: Option<String>,
}

/// Why this build must not update itself, if it must not: a missing Ed25519 public key, a
/// feed that isn't plain HTTPS, archives or feeds that need not be signed, no known channel.
fn configuration_error(settings: &Settings) -> Option<&'static str> {
    // 32 bytes in base64: 43 characters and one `=`.
    let key = settings.key.as_deref().unwrap_or_default();
    let valid_key = key.len() == 44
        && key.ends_with('=')
        && key[..43].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    if !valid_key {
        return Some("no update signing public key (SUPublicEDKey)");
    }
    let secure = settings.feed.as_deref().and_then(|feed| url::Url::parse(feed).ok()).is_some_and(|url| {
        url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
    });
    if !secure {
        return Some("the update feed (SUFeedURL) is not a plain HTTPS URL");
    }
    if !settings.verify_archive {
        return Some("update archives are not verified before extraction");
    }
    if !settings.signed_feed || settings.expiry != Some(0) {
        return Some("the update feed need not be signed");
    }
    if !matches!(settings.channel.as_deref(), Some("stable" | "prerelease")) {
        return Some("no update channel (MSUpdateChannel)");
    }
    None
}

struct Ivars {
    /// A prerelease build also accepts items tagged `prerelease`; a stable one only untagged ones.
    prerelease: bool,
    updater: OnceCell<Retained<AnyObject>>,
}

define_class!(
    /// Sparkle's updater delegate and the target of Automatically Check for Updates.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MailstreamGPUIUpdaterDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    impl Delegate {
        #[unsafe(method_id(allowedChannelsForUpdater:))]
        fn allowed_channels(&self, _updater: &AnyObject) -> Retained<NSSet<NSString>> {
            if self.ivars().prerelease {
                NSSet::from_retained_slice(&[NSString::from_str("prerelease")])
            } else {
                NSSet::new()
            }
        }

        #[unsafe(method(toggleAutomaticChecks:))]
        fn toggle_automatic_checks(&self, _sender: Option<&AnyObject>) {
            if let Some(updater) = self.ivars().updater.get() {
                unsafe {
                    let on: bool = msg_send![&**updater, automaticallyChecksForUpdates];
                    let _: () = msg_send![&**updater, setAutomaticallyChecksForUpdates: !on];
                }
            }
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> Bool {
            let Some(updater) = self.ivars().updater.get() else { return Bool::NO };
            unsafe {
                let on: bool = msg_send![&**updater, automaticallyChecksForUpdates];
                let _: () = msg_send![item, setState: on as isize];
            }
            Bool::YES
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, prerelease: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { prerelease, updater: OnceCell::new() });
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Settings {
        Settings {
            key: Some("eIwn7DyQN0GOIdjlzo6AKRm5GVCOorozSzfbC+4XG4s=".into()),
            feed: Some("https://github.com/taooceros/mailstream/releases/download/updates/appcast-gpui.xml".into()),
            verify_archive: true,
            signed_feed: true,
            expiry: Some(0),
            channel: Some("prerelease".into()),
        }
    }

    #[test]
    fn production_configuration_is_accepted() {
        assert_eq!(configuration_error(&valid()), None);
    }

    #[test]
    fn insecure_or_unconfigured_builds_fail_closed() {
        for feed in ["", "http://localhost/appcast.xml", "file:///tmp/appcast.xml", "https://name:password@example.com/a.xml"] {
            let settings = Settings { feed: Some(feed.into()), ..valid() };
            assert!(configuration_error(&settings).is_some(), "{feed}");
        }
        for key in [None, Some(String::new()), Some("not a key".into())] {
            assert!(configuration_error(&Settings { key, ..valid() }).is_some());
        }
        assert!(configuration_error(&Settings { verify_archive: false, ..valid() }).is_some());
        assert!(configuration_error(&Settings { signed_feed: false, ..valid() }).is_some());
        assert!(configuration_error(&Settings { expiry: Some(1728000), ..valid() }).is_some());
        assert!(configuration_error(&Settings { channel: Some("nightly".into()), ..valid() }).is_some());
    }
}
