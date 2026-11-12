//! Actions on conversations: optimistic, rolled back when the engine refuses them; replying,
//! forwarding and writing new mail.

use super::*;
use crate::compose::{ColumnEvent, ComposeColumn, ComposeMode, MovedReply, ReplyEvent};
use crate::engine::{self, BatchResult};
use crate::{controls, design};
use gpui::component::WindowExt;

/// What takes a conversation out of the list.
#[derive(Clone, Copy)]
pub enum Removal {
    Archive,
    Trash,
    Restore,
}

impl Removal {
    /// What the toast says could not be done.
    fn verb(self) -> &'static str {
        match self {
            Removal::Archive => "archive",
            Removal::Trash => "move it to the Trash",
            Removal::Restore => "move it to the Inbox",
        }
    }

    async fn run(self, ids: Vec<String>) -> Result<BatchResult, MailError> {
        match self {
            Removal::Archive => engine().archive(ids).await,
            // Deleting for good from the Trash is left to the app.
            Removal::Trash => engine().trash(ids, false).await,
            Removal::Restore => engine().restore(ids).await,
        }
    }
}

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

    /// The open conversation, else the highlighted row; nothing while Chat or the calendar is
    /// shown.
    pub fn target(&self) -> Option<Conversation> {
        if !self.mail_in_view() {
            return None;
        }
        if let Some(conversation) = self.reader() {
            return Some(conversation.clone());
        }
        let id = if self.open_bundle().is_some() { self.bundle_highlight.as_ref() } else { self.highlight.as_ref() }?;
        self.conversation(id)
    }

    pub fn conversation(&self, id: &str) -> Option<Conversation> {
        self.list()?.conversations.iter().find(|c| c.id == id).cloned()
    }

    /// The user's addresses, lowercased: the accounts'.
    pub fn own_addresses(&self) -> HashSet<String> {
        self.accounts.iter().map(|account| account.id.to_lowercase()).collect()
    }

    /// Message `id` of the loaded thread `conversation`.
    fn loaded_message(&self, conversation: &str, id: &str) -> Option<&ShownMessage> {
        match self.threads.get(conversation).map(|t| &t.state) {
            Some(ThreadState::Loaded(messages)) => messages.iter().find(|message| message.id == id),
            _ => None,
        }
    }

    pub fn reply(&mut self, _: &Reply, window: &mut Window, cx: &mut Context<Self>) {
        self.start_reply(false, None, window, cx);
    }

    pub fn reply_all(&mut self, _: &ReplyAll, window: &mut Window, cx: &mut Context<Self>) {
        self.start_reply(true, None, window, cx);
    }

    /// A reply (to everyone, with `all`) to message `message` of the open conversation, rather
    /// than its latest: the chat log's hover buttons.
    pub fn reply_to_message(&mut self, message: String, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.start_reply(all, Some(message), window, cx);
    }

    /// Opens the conversation in the reader, if it is not, with a reply box under its messages:
    /// a reply to `message`, else to its latest.
    fn start_reply(&mut self, all: bool, message: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.take_target(window, cx) else { return };
        if self.reader().is_none_or(|open| open.id != conversation.id) {
            self.activate(&conversation.id, window, cx);
        }
        if self.reader().is_none_or(|open| open.id != conversation.id) {
            return;
        }
        // The same reply already open keeps what was written.
        if let Some(draft) =
            self.reply.as_ref().filter(|d| d.conversation == conversation.id && d.all == all && d.message == message)
        {
            draft.view.update(cx, |reply, cx| reply.focus(window, cx));
            return;
        }
        // Who it goes to is the app's to decide; the box names them for the user.
        let chosen = message.as_ref().and_then(|message| self.loaded_message(&conversation.id, message));
        let preview: SharedString = if let Some(chosen) = chosen {
            let (to, cc) = chosen.reply_recipients(&self.own_addresses(), all);
            to.iter().chain(&cc).map(|address| address.display_name()).collect::<Vec<_>>().join(", ").into()
        } else if all || conversation.from_me {
            // Answering their own latest message goes to its recipients, not to them.
            let people: Vec<&str> =
                conversation.participants.iter().map(String::as_str).filter(|p| *p != "me").collect();
            if people.is_empty() { conversation.sender_name() } else { people.join(", ").into() }
        } else {
            conversation.sender_name()
        };
        let id = conversation.id.clone();
        let view = cx.new(|cx| ReplyBox::new(id.clone(), message.clone(), all, preview, window, cx));
        let target = message.clone();
        let replied = id.clone();
        let subscription = cx.subscribe_in(&view, window, move |this, _, event: &ReplyEvent, window, cx| {
            this.reply = None;
            window.focus(&this.focus, cx);
            match event {
                ReplyEvent::Sent => {
                    this.show_toast("Sent", cx);
                    this.reload_replied(&replied, cx);
                }
                ReplyEvent::Discarded => {}
                ReplyEvent::Moved { body, attachments } => {
                    this.move_reply(&replied, target.clone(), all, body.clone(), attachments.clone(), window, cx);
                }
            }
            cx.notify();
        });
        self.reply = Some(super::ReplyDraft { conversation: id, message, all, view, _subscription: subscription });
        // The box opens under the last message: the reader holds its end while it is written.
        self.reader_pin = ReaderPin::End;
        cx.notify();
    }

    /// The thread grows by a reply: read it again (showing it meanwhile) if it is open, and the
    /// lists with it.
    fn reload_replied(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(open) = self.reader().filter(|open| open.id == id).cloned() {
            self.reader_pin = ReaderPin::End;
            self.load_thread(&open, true, cx);
        }
        self.load_list(self.key(), true, cx);
    }

    /// Moves the inline reply (to `message`, else the latest) to the compose column, where its
    /// recipients and subject can change; it still goes out with `mail.reply`, keeping the thread.
    #[allow(clippy::too_many_arguments)]
    fn move_reply(
        &mut self,
        id: &str,
        message: Option<String>,
        all: bool,
        body: String,
        attachments: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conversation = self.conversation(id).or_else(|| self.reader().filter(|c| c.id == id).cloned());
        let Some(conversation) = conversation else { return };
        let own = self.own_addresses();
        let (to, cc) = match self.threads.get(id).map(|t| &t.state) {
            Some(ThreadState::Loaded(messages)) => messages
                .iter()
                .find(|m| Some(&m.id) == message.as_ref())
                .or(messages.last())
                .map(|source| source.reply_recipients(&own, all))
                .unwrap_or_default(),
            _ => Default::default(),
        };
        let subject = mailstream::reply_subject(conversation.subject.to_string());
        let moved = MovedReply {
            conversation: id.to_owned(),
            message,
            all,
            from: conversation.account.clone(),
            to,
            cc,
            subject: subject.into(),
            body,
            attachments,
        };
        let accounts = self.accounts.clone();
        self.compose.update(cx, |column, cx| column.open(ComposeMode::Reply(moved), accounts, window, cx));
    }

    pub fn forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.take_target(window, cx) else { return };
        let id = conversation.id.clone();
        if self.threads.get(&id).is_some_and(|t| matches!(t.state, ThreadState::Loaded(_))) {
            self.forward_loaded(&id, None, cx);
        } else {
            // The draft opens once the messages it quotes arrive.
            self.forward_pending = Some(id);
            self.load_thread(&conversation, false, cx);
        }
    }

    /// A forward of message `message` of the open conversation: the chat log's hover button.
    pub fn forward_message(&mut self, message: String, cx: &mut Context<Self>) {
        let Some(id) = self.reader().map(|open| open.id.clone()) else { return };
        self.forward_loaded(&id, Some(&message), cx);
    }

    /// Opens a forward of the conversation's message `message`, else its newest, whose thread is
    /// loaded, as the window draws next.
    pub(super) fn forward_loaded(&mut self, id: &str, message: Option<&str>, cx: &mut Context<Self>) {
        let conversation = self.conversation(id).or_else(|| self.reader().filter(|c| c.id == id).cloned());
        let Some(conversation) = conversation else { return };
        let Some(ThreadState::Loaded(messages)) = self.threads.get(id).map(|t| &t.state) else { return };
        let Some(source) = messages.iter().find(|m| Some(m.id.as_str()) == message).or(messages.last()) else { return };
        let quoted = source.forwarded(&conversation.subject).into();
        let mode = ComposeMode::Forward { from: conversation.account.clone(), subject: conversation.subject.clone(), quoted };
        let accounts = self.accounts.clone();
        self.compose.update(cx, |column, cx| column.queue(mode, accounts, cx));
    }

    /// A new message, from the account whose mailbox is shown.
    pub fn new_message(&mut self, _: &NewMessage, window: &mut Window, cx: &mut Context<Self>) {
        let from = match &self.source {
            Source::Mailbox { account, .. } => Some(account.clone()),
            Source::Unified(_) => None,
        };
        let accounts = self.accounts.clone();
        self.compose.update(cx, |column, cx| column.new_message(from, accounts, window, cx));
    }

    pub(super) fn on_compose_event(
        &mut self,
        _: &Entity<ComposeColumn>,
        event: &ColumnEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ColumnEvent::Sent { reply } => {
                self.show_toast("Sent", cx);
                if let Some(id) = reply {
                    self.reload_replied(id, cx);
                }
            }
            ColumnEvent::Unfocus => window.focus(&self.focus, cx),
            ColumnEvent::NewDraft => self.new_message(&NewMessage, window, cx),
        }
    }

    /// Shows Chat and its New Chat form.
    pub fn new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.select_chat(None, window, cx);
        self.chat.update(cx, |chat, cx| chat.new_chat(&crate::chat::NewChat, window, cx));
    }

    pub fn archive(&mut self, _: &Archive, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx).filter(Conversation::can_archive) {
            self.remove(conversation, Removal::Archive, window, cx);
        }
    }

    pub fn trash(&mut self, _: &Trash, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx) {
            self.remove(conversation, Removal::Trash, window, cx);
        }
    }

    /// Moves trashed or junk messages back to the inbox; the conversation leaves the Trash list.
    pub fn restore(&mut self, _: &Restore, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.take_target(window, cx).filter(Conversation::can_restore) {
            self.remove(conversation, Removal::Restore, window, cx);
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
        self.set_waiting(&conversation, !conversation.waiting, cx);
    }

    /// Puts a conversation on "Waiting on you" or resolves it; rolls back if the engine refuses.
    pub fn set_waiting(&mut self, conversation: &Conversation, waiting: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.waiting = waiting);
        self.rebuild_items();
        cx.notify();
        let ids = vec![id.to_string()];
        self.background(
            cx,
            async move {
                let result = engine().tag(ids, None, Some(waiting), None).await?;
                match result.failed.into_iter().next() {
                    None => Ok(()),
                    Some(failure) => Err(failure.message),
                }
                .map_err(|message| MailError::Failed { code: "failed".into(), message })
            },
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.waiting = !waiting);
                    this.rebuild_items();
                    this.show_toast(format!("Couldn't update “Waiting on you”: {error}"), cx);
                }
            },
        );
    }

    /// Forgets every AI tag; the engine tags the lists again and says so with change events.
    pub fn retag(&mut self, _: &Retag, _: &mut Window, cx: &mut Context<Self>) {
        self.background(
            cx,
            engine().retag(),
            |this, result, cx| match result {
                Ok(_) => this.show_toast("Tagging every conversation again…", cx),
                Err(error) => this.show_toast(format!("Couldn't re-tag: {error}"), cx),
            },
        );
    }

    /// Takes the conversation out of every list at once; puts it back if the engine refuses.
    pub fn remove(&mut self, conversation: Conversation, removal: Removal, window: &mut Window, cx: &mut Context<Self>) {
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
        self.refresh_layers(cx);
        window.focus(&self.focus, cx);
        cx.notify();

        let ids = vec![id.to_string()];
        self.background(
            cx,
            async move { engine::all_done(removal.run(ids).await) },
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
                let message = if error.code() == "confirm" {
                    "It is already in the Trash; deleting for good is left to the app.".to_owned()
                } else {
                    format!("Couldn't {}: {error}", removal.verb())
                };
                this.show_toast(message, cx);
                cx.notify();
            },
        );
    }

    /// Applies `change` to the conversation wherever it is shown.
    fn patch(&mut self, id: &str, change: impl Fn(&mut Conversation)) {
        for list in self.lists.values_mut() {
            for conversation in list.conversations.iter_mut().filter(|c| c.id == id) {
                change(conversation);
            }
        }
        for layer in &mut self.layers {
            if let Layer::Reader(conversation) = layer
                && conversation.id == id
            {
                change(conversation);
            }
        }
    }

    pub fn set_starred(&mut self, conversation: &Conversation, starred: bool, cx: &mut Context<Self>) {
        let id = conversation.id.clone();
        self.patch(&id, |c| c.starred = starred);
        cx.notify();
        let ids = vec![id.to_string()];
        self.background(
            cx,
            async move { engine::all_done(engine().set_starred(ids, starred).await) },
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
        let ids = vec![id.to_string()];
        self.background(
            cx,
            async move { engine::all_done(engine().set_read(ids, !unread).await) },
            move |this, result, cx| {
                if let Err(error) = result {
                    this.patch(&id, |c| c.unread = !unread);
                    this.show_toast(format!("Couldn't mark it {}: {error}", if unread { "unread" } else { "read" }), cx);
                }
            },
        );
    }

    /// Shows `text` in a toast over the bottom of the window (`controls::toast`), in place of the
    /// one shown. Pushed once this update is over: the window can't be reached from inside it.
    /// Saves a copy of a message's attachment (the engine's file at `path`) where the user picks,
    /// offering their Downloads folder.
    pub fn save_attachment(&mut self, path: SharedString, name: SharedString, cx: &mut Context<Self>) {
        let downloads = std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Downloads")).unwrap_or_default();
        let answer = cx.prompt_for_new_path(&downloads, Some(name.as_ref()));
        cx.spawn(async move |this, cx| {
            let target = match answer.await {
                Ok(Ok(Some(target))) => target,
                // Cancelled.
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    let text = format!("Could not ask where to save {name}: {error}");
                    this.update(cx, |this, cx| this.show_toast(text, cx)).ok();
                    return;
                }
            };
            let source = path.to_string();
            let copied = cx.background_executor().spawn(async move { std::fs::copy(source, target) }).await;
            if let Err(error) = copied {
                this.update(cx, |this, cx| this.show_toast(format!("Could not save {name}: {error}"), cx)).ok();
            }
        })
        .detach();
    }

    pub fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        let window = self.window;
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let p = design::palette(window.appearance());
                    window.push_notification(controls::toast(text, p), cx);
                })
                .ok();
        });
    }

    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_mailboxes(cx);
        self.load_list(self.key(), true, cx);
    }
}
