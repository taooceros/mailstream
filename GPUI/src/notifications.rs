//! OS notification bridge for Mailstream GPUI across macOS, Linux, and Android (MS-443).
//!
//! Subscribes to the engine's `NewMail` and `NewChat` broadcast streams, applies `notifications.*`
//! filtering and persistent deduplication (`notified.json`), posts native OS notifications via
//! `gpui::App::show_system_notification`, and routes body clicks and quick actions (`mark_read`,
//! `archive`) back into `MailApp` / `Engine`.

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{
    App, SystemNotification, SystemNotificationAction, SystemNotificationResponse, WeakEntity, WindowHandle,
};

use crate::engine::engine;
use crate::state::MailApp;

enum Incoming {
    Mail(mailstream::NewMail),
    Chat(mailstream::NewChat),
}

/// Starts OS notification delivery and response routing for the main window.
pub fn start(window: WindowHandle<gpui::base::Root>, app: WeakEntity<MailApp>, cx: &mut App) {
    #[cfg(feature = "shots")]
    if mailstream_engine::shots::running() {
        return;
    }

    let (tx, mut rx) = unbounded::<Incoming>();
    let tx_mail = tx.clone();
    engine().on_new_mail(move |mail| {
        let _ = tx_mail.unbounded_send(Incoming::Mail(mail));
    });
    engine().on_new_chat(move |chat| {
        let _ = tx.unbounded_send(Incoming::Chat(chat));
    });
    let app_for_incoming = app.clone();
    cx.spawn(async move |cx| {
        while let Some(incoming) = rx.next().await {
            let _ = cx.update(|cx| {
                handle_incoming(incoming, window, &app_for_incoming, cx);
            });
        }
    })
    .detach();

    cx.on_system_notification_response(move |response, cx| {
        handle_response(response, window, &app, cx);
    });
}

fn handle_incoming(
    incoming: Incoming,
    window: WindowHandle<gpui::base::Root>,
    app: &WeakEntity<MailApp>,
    cx: &mut App,
) {
    let window_active = window.update(cx, |_, window, _| window.is_window_active()).unwrap_or(false);
    match incoming {
        Incoming::Mail(mail) => {
            let Some(item) = engine().format_mail_notification(&mail) else {
                return;
            };
            let viewing = window_active
                && app
                    .read_with(cx, |app, _| {
                        app.mail_in_view() && app.reader().is_some_and(|c| c.id == mail.conversation_id)
                    })
                    .unwrap_or(false);
            if viewing {
                // Record as seen so a later background wake never re-notifies it.
                let _ = engine().claim_notification(&item.dedup_id);
                return;
            }
            if !engine().claim_notification(&item.dedup_id) {
                return;
            }
            cx.show_system_notification(SystemNotification {
                tag: item.tag.into(),
                title: item.title.into(),
                body: item.body.into(),
                actions: vec![
                    SystemNotificationAction { id: "mark_read".into(), label: "Mark Read".into() },
                    SystemNotificationAction { id: "archive".into(), label: "Archive".into() },
                ],
            });
        }
        Incoming::Chat(chat) => {
            let Some(item) = engine().format_chat_notification(&chat) else {
                return;
            };
            let viewing = window_active
                && app
                    .read_with(cx, |app, cx| app.is_viewing_chat_space(&chat.account, &chat.space, cx))
                    .unwrap_or(false);
            if viewing {
                let _ = engine().claim_notification(&item.dedup_id);
                return;
            }
            if !engine().claim_notification(&item.dedup_id) {
                return;
            }
            cx.show_system_notification(SystemNotification {
                tag: item.tag.into(),
                title: item.title.into(),
                body: item.body.into(),
                actions: Vec::new(),
            });
        }
    }
}

fn handle_response(
    response: SystemNotificationResponse,
    window: WindowHandle<gpui::base::Root>,
    app: &WeakEntity<MailApp>,
    cx: &mut App,
) {
    let tag = response.tag.as_ref();
    if let Some(conv_id) = tag.strip_prefix("mail:") {
        let conv_id = conv_id.to_string();
        match response.action_id.as_deref() {
            Some("mark_read") => {
                cx.dismiss_system_notification(tag);
                cx.background_executor()
                    .spawn(async move {
                        let _ = engine().set_read(vec![conv_id], true).await;
                    })
                    .detach();
                return;
            }
            Some("archive") => {
                cx.dismiss_system_notification(tag);
                cx.background_executor()
                    .spawn(async move {
                        let _ = engine().archive(vec![conv_id]).await;
                    })
                    .detach();
                return;
            }
            _ => {
                cx.activate(true);
                let _ = window.update(cx, |_, window, cx| {
                    window.activate_window();
                    let _ = app.update(cx, |app, cx| {
                        app.open_notification_mail(conv_id, window, cx);
                    });
                });
                return;
            }
        }
    }

    if let Some(rest) = tag.strip_prefix("chat:")
        && let Some((account, space)) = rest.split_once('\n')
    {
        let (account, space) = (account.to_string(), space.to_string());
        cx.activate(true);
        let _ = window.update(cx, |_, window, cx| {
            window.activate_window();
            let _ = app.update(cx, |app, cx| {
                app.open_notification_chat(account, space, window, cx);
            });
        });
    }
}
