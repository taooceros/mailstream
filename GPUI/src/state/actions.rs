//! Actions on conversations: optimistic, rolled back when the engine refuses them, and once done
//! offered for undo in a toast (the engine takes them back, MS-280); replying, forwarding and
//! writing new mail.

use super::*;
use crate::compose::{ColumnEvent, ComposeColumn, ComposeMode, MovedReply, ReplyEvent};
use crate::engine::{self, BatchResult};
use crate::{controls, design};
use gpui::component::WindowExt;
use gpui::component::notification::Notification;
use mailstream::{UndoAction, Undone};

/// What takes a conversation out of the list.
#[derive(Clone, Copy)]
pub enum Removal {
    Archive,
    Trash,
    Restore,
}

impl Removal {
    /// What the toast says could not be done to `count` conversations.
    fn verb(self, count: usize) -> String {
        match (self, count) {
            (Removal::Archive, 1) => "archive".into(),
            (Removal::Archive, count) => format!("archive {count} conversations"),
            (Removal::Trash, 1) => "move it to the Trash".into(),
            (Removal::Trash, count) => format!("move {count} conversations to the Trash"),
            (Removal::Restore, 1) => "move it to the Inbox".into(),
            (Removal::Restore, count) => format!("move {count} conversations to the Inbox"),
        }
    }

    /// What the toast offering undo says was done to `count` conversations.
    fn done(self, count: usize) -> SharedString {
        match (self, count) {
            (Removal::Archive, 1) => "Archived".into(),
            (Removal::Archive, count) => format!("Archived {count} conversations").into(),
            (Removal::Trash, 1) => "Moved to the Trash".into(),
            (Removal::Trash, count) => format!("Moved {count} conversations to the Trash").into(),
            (Removal::Restore, 1) => "Moved to the Inbox".into(),
            (Removal::Restore, count) => format!("Moved {count} conversations to the Inbox").into(),
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

/// `one` for a single conversation, else `many` with `{n}` the count.
fn counted(count: usize, one: &str, many: &str) -> SharedString {
    if count == 1 { one.to_owned().into() } else { many.replace("{n}", &count.to_string()).into() }
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

    /// What an action that can take several conversations applies to: the selection (MS-305)
    /// when no context menu is open or it is open on a picked row, else `take_target`'s one.
    /// Closes the menu.
    pub fn take_targets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Conversation> {
        let picked = match self.context_menu.as_ref() {
            Some(menu) => self.selection().is_some_and(|selection| selection.contains(&menu.id)),
            None => self.selection().is_some(),
        };
        if picked {
            self.dismiss_menu(window, cx);
            return self.selected_conversations();
        }
        self.take_target(window, cx).into_iter().collect()
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
        } else if conversation.from_me && !conversation.peers.is_empty() {
            conversation.sender_name()
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

    /// The thread grew by a reply, which the engine lists from the moment it went out (MS-291) and
    /// the lists read on its change event: the open reader reads it again (showing it meanwhile),
    /// held at its end.
    fn reload_replied(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(open) = self.reader().filter(|open| open.id == id).cloned() {
            self.reader_pin = ReaderPin::End;
            self.load_thread(&open, true, cx);
        }
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
        let mode = ComposeMode::Forward {
            from: conversation.account.clone(),
            conversation: conversation.id.clone(),
            message: source.id.clone(),
            subject: conversation.subject.clone(),
            quoted,
        };
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
        let targets: Vec<Conversation> = self.take_targets(window, cx).into_iter().filter(Conversation::can_archive).collect();
        self.remove(targets, Removal::Archive, window, cx);
    }

    pub fn trash(&mut self, _: &Trash, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.take_targets(window, cx);
        self.remove(targets, Removal::Trash, window, cx);
    }

    /// Moves trashed or junk messages back to the inbox; the conversations leave the Trash list.
    pub fn restore(&mut self, _: &Restore, window: &mut Window, cx: &mut Context<Self>) {
        let targets: Vec<Conversation> = self.take_targets(window, cx).into_iter().filter(Conversation::can_restore).collect();
        self.remove(targets, Removal::Restore, window, cx);
    }

    /// Stars the conversations, or unstars them when all are starred.
    pub fn star(&mut self, _: &Star, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.take_targets(window, cx);
        let starred = !targets.iter().all(|conversation| conversation.starred);
        self.set_starred(&targets, starred, cx);
    }

    /// Marks the conversations read, or unread when none is unread.
    pub fn toggle_read(&mut self, _: &ToggleRead, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.take_targets(window, cx);
        let unread = !targets.iter().any(|conversation| conversation.unread);
        self.set_unread(&targets, unread, cx);
    }

    /// Resolves conversations waiting on the user, or puts them on "Waiting on you" when none is.
    pub fn toggle_waiting(&mut self, _: &ToggleWaiting, window: &mut Window, cx: &mut Context<Self>) {
        let targets: Vec<Conversation> =
            self.take_targets(window, cx).into_iter().filter(Conversation::can_mark_waiting).collect();
        let waiting = !targets.iter().any(|conversation| conversation.waiting);
        self.set_waiting(&targets, waiting, cx);
    }

    /// Puts conversations on "Waiting on you" or resolves them, with one call to the engine;
    /// those it refuses roll back.
    pub fn set_waiting(&mut self, conversations: &[Conversation], waiting: bool, cx: &mut Context<Self>) {
        let ids = self.change_each(conversations, cx, move |c| c.waiting = waiting);
        if ids.is_empty() {
            return;
        }
        self.rebuild_items();
        let asked = ids.clone();
        self.background(
            cx,
            async move {
                let answer = engine().tag(asked.clone(), None, Some(waiting), None, None, None).await;
                engine::outcome(answer.map(|r| (r.failed, r.undo)), &asked)
            },
            move |this, outcome, cx| {
                let done = ids.len() - outcome.failed.len();
                let text = if waiting {
                    counted(done, "Marked as waiting on you", "Marked {n} conversations as waiting on you")
                } else {
                    counted(done, "Resolved", "Resolved {n} conversations")
                };
                this.settle(outcome, text, cx, move |c| c.waiting = !waiting, |error| format!("Couldn't update “Waiting on you”: {error}"));
                this.rebuild_items();
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

    /// Takes the conversations out of every list at once, with one call to the engine; puts back
    /// those it refuses. Ends a selection.
    pub fn remove(&mut self, conversations: Vec<Conversation>, removal: Removal, window: &mut Window, cx: &mut Context<Self>) {
        if conversations.is_empty() {
            return;
        }
        let ids: Vec<String> = conversations.into_iter().map(|conversation| conversation.id).collect();
        let gone: HashSet<&str> = ids.iter().map(String::as_str).collect();
        // The highlight moves to the row after the last one removed, as in the native list, else
        // to the one before the first.
        let (lines, in_bundle) = self.active_lines();
        let staying = |line: &&(String, usize)| !gone.contains(line.0.as_str());
        let last = lines.iter().rposition(|(line, _)| gone.contains(line.as_str()));
        let first = lines.iter().position(|(line, _)| gone.contains(line.as_str()));
        let next = last
            .and_then(|last| lines[last + 1..].iter().find(staying))
            .or_else(|| first.and_then(|first| lines[..first].iter().rev().find(staying)))
            .map(|(line, _)| line.clone());
        if in_bundle {
            self.bundle_highlight = next;
        } else {
            self.highlight = next;
        }

        // Where each left each list, in the lists' order, to put back what the engine refuses.
        let mut removed: Vec<(ListKey, usize, Conversation)> = Vec::new();
        for (key, list) in &mut self.lists {
            let mut at = 0;
            list.conversations.retain(|conversation| {
                let leaving = gone.contains(conversation.id.as_str());
                if leaving {
                    removed.push((key.clone(), at, conversation.clone()));
                }
                at += 1;
                !leaving
            });
        }
        self.layers.retain(|layer| !matches!(layer, Layer::Reader(c) if gone.contains(c.id.as_str())));
        self.removing.extend(ids.iter().cloned());
        self.selection.clear();
        self.rebuild_items();
        self.refresh_layers(cx);
        window.focus(&self.focus, cx);
        cx.notify();

        let asked = ids.clone();
        self.background(
            cx,
            async move { engine::outcome(removal.run(asked.clone()).await.map(|r| (r.failed, r.undo)), &asked) },
            move |this, outcome, cx| {
                for id in &ids {
                    this.removing.remove(id);
                }
                let done = ids.len() - outcome.failed.len();
                if let Some(error) = &outcome.error {
                    for (key, at, conversation) in removed.into_iter().filter(|(_, _, c)| outcome.failed.contains(&c.id)) {
                        let list = this.lists.entry(key).or_default();
                        if !list.conversations.iter().any(|c| c.id == conversation.id) {
                            list.conversations.insert(at.min(list.conversations.len()), conversation);
                        }
                    }
                    this.rebuild_items();
                    let message = if error.code() == "confirm" {
                        "It is already in the Trash; deleting for good is left to the app.".to_owned()
                    } else {
                        format!("Couldn't {}: {error}", removal.verb(outcome.failed.len()))
                    };
                    this.show_toast(message, cx);
                    cx.notify();
                    return;
                }
                this.offer_undo(removal.done(done), outcome.undo, cx);
            },
        );
    }

    /// Applies `change` to the conversation wherever it is shown.
    pub(super) fn patch(&mut self, id: &str, change: impl Fn(&mut Conversation)) {
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

    /// Shows `change` on each of `conversations` at once; answers their ids.
    fn change_each(&mut self, conversations: &[Conversation], cx: &mut Context<Self>, change: impl Fn(&mut Conversation)) -> Vec<String> {
        let ids: Vec<String> = conversations.iter().map(|conversation| conversation.id.clone()).collect();
        for id in &ids {
            self.patch(id, &change);
        }
        cx.notify();
        ids
    }

    /// An action shown at once came to `outcome`: what the engine refused takes `undo` back and
    /// a toast says why (`failure`), else a toast says `text` and offers undo.
    fn settle(
        &mut self,
        outcome: engine::Outcome,
        text: SharedString,
        cx: &mut Context<Self>,
        undo: impl Fn(&mut Conversation),
        failure: impl FnOnce(&MailError) -> String,
    ) {
        match &outcome.error {
            Some(error) => {
                for id in &outcome.failed {
                    self.patch(id, &undo);
                }
                self.show_toast(failure(error), cx);
                cx.notify();
            }
            None => self.offer_undo(text, outcome.undo, cx),
        }
    }

    /// Stars or unstars the conversations with one call to the engine; those it refuses roll back.
    pub fn set_starred(&mut self, conversations: &[Conversation], starred: bool, cx: &mut Context<Self>) {
        let ids = self.change_each(conversations, cx, move |c| c.starred = starred);
        if ids.is_empty() {
            return;
        }
        let asked = ids.clone();
        self.background(
            cx,
            async move { engine::outcome(engine().set_starred(asked.clone(), starred).await.map(|r| (r.failed, r.undo)), &asked) },
            move |this, outcome, cx| {
                let done = ids.len() - outcome.failed.len();
                let text = if starred {
                    counted(done, "Starred", "Starred {n} conversations")
                } else {
                    counted(done, "Unstarred", "Unstarred {n} conversations")
                };
                this.settle(outcome, text, cx, move |c| c.starred = !starred, |error| format!("Couldn't update the star: {error}"));
            },
        );
    }

    /// Marks the conversations read or unread with one call to the engine; those it refuses roll
    /// back.
    pub fn set_unread(&mut self, conversations: &[Conversation], unread: bool, cx: &mut Context<Self>) {
        let ids = self.change_each(conversations, cx, move |c| c.unread = unread);
        if ids.is_empty() {
            return;
        }
        let asked = ids.clone();
        self.background(
            cx,
            async move { engine::outcome(engine().set_read(asked.clone(), !unread).await.map(|r| (r.failed, r.undo)), &asked) },
            move |this, outcome, cx| {
                let done = ids.len() - outcome.failed.len();
                let text = if unread {
                    counted(done, "Marked as unread", "Marked {n} conversations as unread")
                } else {
                    counted(done, "Marked as read", "Marked {n} conversations as read")
                };
                let state = if unread { "unread" } else { "read" };
                this.settle(outcome, text, cx, move |c| c.unread = !unread, |error| format!("Couldn't mark it {state}: {error}"));
            },
        );
    }

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

    /// Shows `text` in a toast over the bottom of the window (`controls::toast`), in place of the
    /// one shown.
    pub fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        self.push_toast(cx, move |p| controls::toast(text, p));
    }

    /// Says what an action did in a toast with an Undo button that takes back `step`, the
    /// engine's step for it (none when it changed nothing: no toast then). A later action's toast
    /// takes its place; ⌘Z takes back the latest step meanwhile and after.
    pub(super) fn offer_undo(&mut self, text: SharedString, step: Option<u64>, cx: &mut Context<Self>) {
        let Some(step) = step else { return };
        let app = cx.entity().downgrade();
        self.push_toast(cx, move |p| {
            let undo = move |_: &mut Window, cx: &mut App| {
                app.update(cx, |app, cx| app.take_back(Some(step), cx)).ok();
            };
            controls::toast_with_action(text, "Undo", undo, p)
        });
    }

    /// Pushes the toast `make` builds in the window's palette, once this update is over: the
    /// window can't be reached from inside it.
    fn push_toast(&mut self, cx: &mut Context<Self>, make: impl FnOnce(&'static design::Palette) -> Notification + 'static) {
        let window = self.window;
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let p = design::palette(window.appearance());
                    window.push_notification(make(p), cx);
                })
                .ok();
        });
    }

    /// ⌘Z: takes back the latest action on mail.
    pub fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.take_back(None, cx);
    }

    /// Has the engine take back step `step`, else the latest; the lists follow its change events.
    /// A conversation back in the list is highlighted again, as it was when it left.
    fn take_back(&mut self, step: Option<u64>, cx: &mut Context<Self>) {
        self.background(
            cx,
            async move { engine().undo(step).await },
            |this, result: Result<Undone, MailError>, cx| match result {
                Ok(undone) => match undone.failed.into_iter().next() {
                    None => {
                        if matches!(undone.action, UndoAction::Archive | UndoAction::Trash | UndoAction::Restore)
                            && let Some(id) = undone.done.into_iter().next()
                        {
                            if this.open_bundle().is_some() {
                                this.bundle_highlight = Some(id);
                            } else {
                                this.highlight = Some(id);
                            }
                            cx.notify();
                        }
                        this.show_toast(format!("{} undone", undo_title(undone.action)), cx);
                    }
                    Some(failure) => this.show_toast(format!("Couldn't undo: {}", failure.message), cx),
                },
                Err(error) if error.code() == "nothing" => this.show_toast("Nothing to undo", cx),
                Err(error) => this.show_toast(format!("Couldn't undo: {error}"), cx),
            },
        );
    }

    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_mailboxes(cx);
        self.load_list(self.key(), true, cx);
    }
}

/// How the toast after an undo names what was taken back.
fn undo_title(action: UndoAction) -> &'static str {
    match action {
        UndoAction::Read => "Mark as Read",
        UndoAction::Unread => "Mark as Unread",
        UndoAction::Star => "Star",
        UndoAction::Unstar => "Unstar",
        UndoAction::Archive => "Archive",
        UndoAction::Trash => "Move to Trash",
        UndoAction::Restore => "Move to Inbox",
        UndoAction::Tag => "Tag change",
    }
}
