//! Actions on conversations: optimistic, rolled back when the app refuses them.

use super::*;

impl MailApp {
    /// What an action applies to: the row a context menu is open on, else the open
    /// conversation, else the highlighted row. Closes the menu.
    pub fn take_target(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Conversation> {
        if let Some(menu) = self.context_menu.as_ref() {
            let id = menu.id.clone();
            self.dismiss_menu(window, cx);
            return self.conversation(&id);
        }
        self.target()
    }

    /// The open conversation, else the highlighted row.
    pub fn target(&self) -> Option<Conversation> {
        if let Some(conversation) = self.reader() {
            return Some(conversation.clone());
        }
        let id = if self.open_bundle().is_some() { self.bundle_highlight.as_ref() } else { self.highlight.as_ref() }?;
        self.conversation(id)
    }

    pub fn conversation(&self, id: &SharedString) -> Option<Conversation> {
        self.list()?.conversations.iter().find(|c| &c.id == id).cloned()
    }

    pub fn archive(&mut self, _: &Archive, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx).filter(Conversation::can_archive) {
            self.remove(conversation, "mail.archive", "archive", window, cx);
        }
    }

    pub fn trash(&mut self, _: &Trash, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx) {
            self.remove(conversation, "mail.trash", "move it to the Trash", window, cx);
        }
    }

    /// Moves trashed or junk messages back to the inbox; the conversation leaves the Trash list.
    pub fn restore(&mut self, _: &Restore, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx).filter(Conversation::can_restore) {
            self.remove(conversation, "mail.restore", "move it to the Inbox", window, cx);
        }
    }

    pub fn star(&mut self, _: &Star, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx) {
            self.set_starred(&conversation, !conversation.starred, cx);
        }
    }

    pub fn toggle_read(&mut self, _: &ToggleRead, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx) {
            self.set_unread(&conversation, !conversation.unread, cx);
        }
    }

    /// Resolves a conversation waiting on the user, or puts it back on "Waiting on you".
    pub fn toggle_waiting(&mut self, _: &ToggleWaiting, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.take_target(window, cx).filter(Conversation::can_mark_waiting) else { return };
        let id = conversation.id.clone();
        let waiting = !conversation.waiting;
        self.patch(&id, |c| c.waiting = waiting);
        self.rebuild_items();
        cx.notify();
        let arg = id.to_string();
        self.background(
            cx,
            move || control::call::<Value>("mail.tag", json!({ "ids": [arg], "waiting": waiting.to_string() })),
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.waiting = !waiting);
                    this.rebuild_items();
                    this.show_toast(format!("Couldn't update “Waiting on you”: {error}"), cx);
                }
            },
        );
    }

    /// Forgets every AI tag; the app tags the lists again and the poll picks the new tags up.
    pub fn retag(&mut self, _: &Retag, _: &mut Window, cx: &mut Context<Self>) {
        self.background(
            cx,
            || control::call::<Value>("mail.retag", json!({})),
            |this, result, cx| match result {
                Ok(_) => this.show_toast("Tagging every conversation again…", cx),
                Err(error) => this.show_toast(format!("Couldn't re-tag: {error}"), cx),
            },
        );
    }

    /// Takes the conversation out of every list at once; puts it back if the app refuses.
    pub fn remove(
        &mut self,
        conversation: Conversation,
        command: &'static str,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = conversation.id.clone();
        // The highlight moves to the row after, as in the native list.
        let (lines, in_bundle) = self.active_lines();
        let next = lines
            .iter()
            .position(|(line, _)| line == &id)
            .and_then(|at| lines.get(at + 1).or_else(|| at.checked_sub(1).and_then(|before| lines.get(before))))
            .map(|(line, _)| line.clone());
        if in_bundle {
            self.bundle_highlight = next;
        } else {
            self.highlight = next;
        }

        let mut removed: Vec<(ListKey, usize, Conversation)> = Vec::new();
        for (key, list) in &mut self.lists {
            if let Some(at) = list.conversations.iter().position(|c| c.id == id) {
                removed.push((key.clone(), at, list.conversations.remove(at)));
            }
        }
        self.layers.retain(|layer| !matches!(layer, Layer::Reader(c) if c.id == id));
        self.removing.insert(id.clone());
        self.rebuild_items();
        self.refresh_layers();
        window.focus(&self.focus);
        cx.notify();

        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch(command, json!({ "ids": [arg] })),
            move |this, result, cx| {
                this.removing.remove(&id);
                let Err(error) = result else { return };
                for (key, at, conversation) in removed {
                    let list = this.lists.entry(key).or_default();
                    if !list.conversations.iter().any(|c| c.id == conversation.id) {
                        list.conversations.insert(at.min(list.conversations.len()), conversation);
                    }
                }
                this.rebuild_items();
                let message = if error.code == "confirm" {
                    "It is already in the Trash; deleting for good is left to the app.".to_owned()
                } else {
                    format!("Couldn't {verb}: {error}")
                };
                this.show_toast(message, cx);
                cx.notify();
            },
        );
    }

    /// Applies `change` to the conversation wherever it is shown.
    fn patch(&mut self, id: &SharedString, change: impl Fn(&mut Conversation)) {
        for list in self.lists.values_mut() {
            for conversation in list.conversations.iter_mut().filter(|c| &c.id == id) {
                change(conversation);
            }
        }
        for layer in &mut self.layers {
            if let Layer::Reader(conversation) = layer
                && &conversation.id == id
            {
                change(conversation);
            }
        }
    }

    pub fn set_starred(&mut self, conversation: &Conversation, starred: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.starred = starred);
        cx.notify();
        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch("mail.star", json!({ "ids": [arg], "off": !starred })),
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.starred = !starred);
                    this.show_toast(format!("Couldn't update the star: {error}"), cx);
                }
            },
        );
    }

    pub fn set_unread(&mut self, conversation: &Conversation, unread: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.unread = unread);
        cx.notify();
        let arg = id.to_string();
        self.background(
            cx,
            move || control::batch("mail.read", json!({ "ids": [arg], "unread": unread })),
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.unread = !unread);
                    this.show_toast(format!("Couldn't mark it {}: {error}", if unread { "unread" } else { "read" }), cx);
                }
            },
        );
    }

    pub fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast_generation += 1;
        let generation = self.toast_generation;
        self.toast = Some(Toast { text: text.into(), generation });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.generation == generation) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_mailboxes(cx);
        self.load_list(self.key(), true, cx);
    }
}
