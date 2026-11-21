//! Cross-platform OS notification formatting, filtering, and persistent deduplication (MS-443).
//!
//! Both live front-end subscriptions (macOS/Linux/Android GPUI) and cold-start Android FCM wake
//! passes route through here so every platform applies the same `notifications.*` rules and never
//! notifies the same message twice across process restarts.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::answers::{Category, NewChat, NewMail};
use crate::files;
use crate::settings::{Setting, Settings};

const NOTIFIED_FILE: &str = "notified.json";
const MAX_REMEMBERED: usize = 500;

/// One formatted OS notification ready for macOS, Linux, or Android.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct NotificationItem {
    /// Stable platform tag (`mail:<conversation_id>` or `chat:<account>\n<space>`).
    pub tag: String,
    /// Stable per-message deduplication key (`mail:<message_id>` or `chat:<message_id>`).
    pub dedup_id: String,
    /// Android notification channel id (`"mail"` or `"chat"`).
    pub channel: String,
    pub title: String,
    pub body: String,
    pub account: String,
    pub conversation_id: Option<String>,
    pub chat_space: Option<String>,
}

/// Whether incoming inbox mail with `(category, waiting)` passes `notifications.mail` (`mode`).
///
/// - `"off"`: never notify
/// - `"all"`: notify every incoming unread inbox message
/// - `"important"` (default): notify `People`, `Waiting on you`, and untagged mail; skip
///   `Newsletter` and `Notification` bundles unless `waiting` is true.
pub fn should_notify_mail(mode: &str, category: Option<Category>, waiting: bool) -> bool {
    match mode.trim().to_ascii_lowercase().as_str() {
        "off" => false,
        "all" => true,
        _ => waiting || !matches!(category, Some(Category::Newsletter | Category::Notification)),
    }
}

/// Formats `mail` as an OS notification if allowed by `settings`.
pub fn format_mail(settings: &Settings, mail: &NewMail) -> Option<NotificationItem> {
    if !settings.value(Setting::NotificationsEnabled).as_bool().unwrap_or(true) {
        return None;
    }
    let mode = settings
        .value(Setting::NotificationsMail)
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| "important".into());
    if !should_notify_mail(&mode, mail.category, mail.waiting) {
        return None;
    }
    let title = mail
        .from
        .as_ref()
        .map(|addr| {
            let name = addr.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
            name.unwrap_or(addr.email.as_str()).to_string()
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| mail.account.clone());
    let subject = mail.subject.trim();
    let snippet = mail.snippet.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let body = match (subject.is_empty(), snippet) {
        (false, Some(snippet)) => format!("{subject} — {snippet}"),
        (false, None) => subject.to_string(),
        (true, Some(snippet)) => snippet.to_string(),
        (true, None) => "New message".into(),
    };
    Some(NotificationItem {
        tag: format!("mail:{}", mail.conversation_id),
        dedup_id: format!("mail:{}:{}", mail.account, mail.message_id),
        channel: "mail".into(),
        title,
        body,
        account: mail.account.clone(),
        conversation_id: Some(mail.conversation_id.clone()),
        chat_space: None,
    })
}

/// Formats `chat` as an OS notification if allowed by `settings`.
pub fn format_chat(settings: &Settings, chat: &NewChat) -> Option<NotificationItem> {
    if !settings.value(Setting::NotificationsEnabled).as_bool().unwrap_or(true) {
        return None;
    }
    if !settings.value(Setting::NotificationsChat).as_bool().unwrap_or(true) {
        return None;
    }
    let title = if chat.is_direct || chat.space_name.trim().is_empty() || chat.space_name == chat.sender {
        chat.sender.clone()
    } else {
        format!("{} · {}", chat.sender, chat.space_name)
    };
    let text = chat.text.trim();
    let body = if text.is_empty() { "New message".into() } else { text.to_string() };
    Some(NotificationItem {
        tag: format!("chat:{}\n{}", chat.account, chat.space),
        dedup_id: format!("chat:{}:{}", chat.account, chat.message_id),
        channel: "chat".into(),
        title,
        body,
        account: chat.account.clone(),
        conversation_id: None,
        chat_space: Some(chat.space.clone()),
    })
}

/// Persisted ring buffer of recently notified message keys (`notified.json`) so cold-start
/// background wakes and live foreground listeners never double-notify the same message.
pub struct NotifiedLog {
    directory: PathBuf,
    path: PathBuf,
    seen: Mutex<VecDeque<String>>,
}

#[derive(Default, Serialize, Deserialize)]
struct NotifiedDisk {
    #[serde(default)]
    ids: Vec<String>,
}

impl NotifiedLog {
    pub fn new(directory: &Path) -> Self {
        let path = directory.join(NOTIFIED_FILE);
        let ids = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<NotifiedDisk>(&bytes).ok())
            .map(|disk| disk.ids)
            .unwrap_or_default();
        let mut seen: VecDeque<String> = ids.into_iter().collect();
        while seen.len() > MAX_REMEMBERED {
            seen.pop_front();
        }
        Self { directory: directory.to_path_buf(), path, seen: Mutex::new(seen) }
    }

    /// Claims `dedup_id` for notification delivery. Returns `true` the first time it is seen and
    /// persists the updated log; returns `false` if already notified.
    pub fn claim(&self, dedup_id: &str) -> bool {
        let mut seen = self.seen.lock().expect("notified log lock");
        if seen.iter().any(|existing| existing == dedup_id) {
            return false;
        }
        seen.push_back(dedup_id.to_string());
        while seen.len() > MAX_REMEMBERED {
            seen.pop_front();
        }
        let disk = NotifiedDisk { ids: seen.iter().cloned().collect() };
        if let Ok(bytes) = serde_json::to_vec(&disk) {
            let _ = files::replace(&self.directory, &self.path, &bytes);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answers::Address;
    use serde_json::json;

    #[test]
    fn mail_filter_modes_respect_categories_and_waiting() {
        assert!(should_notify_mail("important", Some(Category::People), false));
        assert!(should_notify_mail("important", Some(Category::Invitation), false));
        assert!(should_notify_mail("important", None, false));
        assert!(!should_notify_mail("important", Some(Category::Newsletter), false));
        assert!(!should_notify_mail("important", Some(Category::Notification), false));
        assert!(should_notify_mail("important", Some(Category::Notification), true));

        assert!(should_notify_mail("all", Some(Category::Newsletter), false));
        assert!(!should_notify_mail("off", Some(Category::People), true));
    }

    #[test]
    fn format_mail_and_notified_log_dedup_persist_across_restarts() {
        let dir = std::env::temp_dir().join(format!(
            "mailstream-notif-test-{}-{}",
            std::process::id(),
            crate::testkit::unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings::new(&dir);
        let msg = NewMail {
            conversation_id: "conv-1".into(),
            message_id: "m-1".into(),
            account: "me@gmail.com".into(),
            subject: "Dinner tonight?".into(),
            date: None,
            from: Some(Address { name: Some("Alice".into()), email: "alice@x.com".into() }),
            unread: true,
            starred: false,
            has_attachment: false,
            snippet: Some("7pm works?".into()),
            category: Some(Category::People),
            waiting: false,
            labels: vec![],
        };
        let item = format_mail(&settings, &msg).expect("important mail notifies");
        assert_eq!(item.tag, "mail:conv-1");
        assert_eq!(item.title, "Alice");
        assert_eq!(item.body, "Dinner tonight? — 7pm works?");

        // Newsletter is skipped under default `important` mode.
        let mut newsletter = msg.clone();
        newsletter.category = Some(Category::Newsletter);
        assert!(format_mail(&settings, &newsletter).is_none());

        let mut args = serde_json::Map::new();
        args.insert("key".into(), json!("notifications.mail"));
        args.insert("value".into(), json!("all"));
        settings.run("config.set", &args).unwrap();
        settings.reload();
        assert!(format_mail(&settings, &newsletter).is_some());

        // Deduplication persists across process restarts.
        let log1 = NotifiedLog::new(&dir);
        assert!(log1.claim(&item.dedup_id));
        assert!(!log1.claim(&item.dedup_id));
        let log2 = NotifiedLog::new(&dir);
        assert!(!log2.claim(&item.dedup_id));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
