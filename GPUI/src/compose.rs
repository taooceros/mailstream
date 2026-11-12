//! Writing mail: the drafts of the compose column (`column.rs`; a new message, a forward or a
//! reply moved out of the reader, with every field editable) and the reader's inline reply
//! (`mail.reply`). Clicking Send is the confirmation the engine asks for, so both pass `confirm`.
//! Recipients complete from `contacts.search` when the running app offers it. Both carry
//! attachments (`attachments.rs`).

mod attachments;
mod column;

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, EntityInputHandler, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, actions, deferred,
    div, px, relative,
};
use crate::avatars;
use crate::controls::{ButtonKind, ControlSize, button, icon_button, popover};
use gpui::component::button::Button;
use crate::design::{self, Palette, compose as metrics, editor, radius, space, text};
use crate::engine::engine;
use crate::icons::Icon;
use crate::input::{InputEvent, TextInput};
use crate::model::{Account, Address};
use crate::textarea::{TextArea, TextAreaEvent};
use attachments::{Attachments, Remove};
pub use column::{ColumnEvent, ComposeColumn, ResizeColumn};

actions!(
    compose,
    [Send, Cancel, Close, FocusNext, FocusPrevious, SuggestionNext, SuggestionPrevious, Attach, MoveToColumn]
);

const CONTEXT: &str = "Compose";
const RECIPIENTS: &str = "Recipients";
const REPLY_CONTEXT: &str = "ReplyBox";
/// The groups files are dragged over.
const COMPOSE_DROP: &str = "compose-drop";
const REPLY_DROP: &str = "reply-drop";
const ATTACH_TIP: &str = "Attach Files (⇧⌘A), or drop or paste them here";
const MOVE_TIP: &str = "Move to the Compose Column to Change Recipients or Subject (⇧⌘O)";

pub fn bind_keys(cx: &mut App) {
    let window = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new(secondary!("enter"), Send, window),
        KeyBinding::new("escape", Cancel, window),
        KeyBinding::new(secondary!("w"), Close, window),
        KeyBinding::new("tab", FocusNext, window),
        KeyBinding::new("shift-tab", FocusPrevious, window),
        KeyBinding::new(secondary!("shift-a"), Attach, window),
        KeyBinding::new("down", SuggestionNext, Some(RECIPIENTS)),
        KeyBinding::new("up", SuggestionPrevious, Some(RECIPIENTS)),
        KeyBinding::new("escape", Cancel, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("shift-a"), Attach, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("shift-o"), MoveToColumn, Some(REPLY_CONTEXT)),
    ]);
}

/// What a draft starts as. A forward's `subject` is the original's; `quoted` is the original as
/// text, put under the caret. A reply moved out of the reader keeps its text and files; its `to`
/// and `cc` are who the engine will send it to, unless they are edited.
pub enum ComposeMode {
    New { from: Option<SharedString> },
    Forward { from: SharedString, subject: SharedString, quoted: SharedString },
    Reply(MovedReply),
}

/// A reply the reader moved to the compose column.
pub struct MovedReply {
    pub conversation: SharedString,
    pub all: bool,
    pub from: SharedString,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: SharedString,
    pub body: String,
    pub attachments: Vec<PathBuf>,
}

/// One person `contacts.search` offers.
#[derive(Clone)]
struct Person {
    name: Option<SharedString>,
    email: SharedString,
}

impl From<mailstream::Person> for Person {
    fn from(person: mailstream::Person) -> Self {
        Self { name: person.name.map(SharedString::from), email: person.email.into() }
    }
}

impl Person {
    fn address(&self) -> Address {
        Address { name: self.name.clone().filter(|name| !name.is_empty()), email: self.email.clone() }
    }

    /// `Name <email>`, quoting a name the recipient list would split.
    fn formatted(&self) -> String {
        match self.name.as_ref().filter(|name| !name.is_empty()) {
            Some(name) if name.contains([',', ';', '"', '<', '>']) => {
                format!("\"{}\" <{}>", name.replace('"', "'"), self.email)
            }
            Some(name) => format!("{name} <{}>", self.email),
            None => self.email.to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    To,
    Cc,
    Bcc,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::To => "To",
            Field::Cc => "Cc",
            Field::Bcc => "Bcc",
        }
    }
}

struct Suggestions {
    field: Field,
    people: Vec<Person>,
    highlighted: usize,
}

enum Status {
    Idle,
    Sending,
    Failed(SharedString),
}

/// What Send hands the engine: a message, or a reply that keeps its thread.
enum Outgoing {
    Message(mailstream::Draft),
    Reply { id: String, body: String, all: bool, overrides: mailstream::ReplyOverrides },
}

/// The recipient list split at commas and semicolons outside quotes and angle brackets.
fn split_recipients(text: &str) -> Vec<&str> {
    let (mut parts, mut start, mut quoted, mut angle) = (Vec::new(), 0, false, false);
    for (ix, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                parts.push(&text[start..ix]);
                start = ix + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"'))
}

/// A field's recipients, or the first one that is not an address.
fn parse_recipients(text: &str) -> Result<Vec<String>, String> {
    split_recipients(text)
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| if valid_email(&Address::parse(part).email) { Ok(part.to_owned()) } else { Err(part.to_owned()) })
        .collect()
}

/// `Fwd: subject`, unless it already says so.
fn forward_subject(subject: &str) -> SharedString {
    let lower = subject.trim_start().to_lowercase();
    if lower.starts_with("fwd:") || lower.starts_with("fw:") {
        subject.to_owned().into()
    } else {
        format!("Fwd: {subject}").into()
    }
}

/// The accent Send button with its shortcut; dimmed and inert while sending.
fn send_button(id: &'static str, sending: bool, p: &Palette) -> Button {
    button(id, if sending { "Sending…" } else { "Send" }, ButtonKind::Primary, ControlSize::Regular, !sending, p).when(
        !sending,
        |b| {
            b.child(
                div()
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::MEDIUM)
                    .opacity(metrics::KEY_HINT_OPACITY)
                    .child("⌘↩"),
            )
        },
    )
}

fn status_line(status: &Status, p: &Palette) -> Option<Div> {
    let (note, color) = match status {
        Status::Idle => return None,
        Status::Sending => ("Sending…".into(), p.muted),
        Status::Failed(message) => (message.clone(), p.danger),
    };
    Some(div().flex_1().min_w_0().text_size(px(text::FOOTNOTE)).text_color(color).child(note))
}

// MARK: - A draft

/// What a draft tells its column.
enum DraftEvent {
    Sent,
    /// Sent, discarded, or closed with nothing written.
    Closed,
    /// Escape over a written draft: the keyboard goes back to the list and the conversation.
    Unfocus,
}

/// One message in the compose column, one tab: kept, with its fields, files and sending state,
/// until it is sent or discarded, through mailbox and conversation changes and while the column
/// is minimized.
struct Compose {
    focus: FocusHandle,
    /// "New Message", "Forward", "Reply" or "Reply All", and the tab's glyph for it.
    title: SharedString,
    glyph: Icon,
    accounts: Vec<Account>,
    from: Option<SharedString>,
    /// A forward and a reply keep the account it came to.
    from_locked: bool,
    /// The conversation a moved reply answers, and whether it answers everyone: sent with
    /// `mail.reply`, which keeps the thread and quotes the original.
    reply: Option<(SharedString, bool)>,
    picking_from: bool,
    to: Entity<TextInput>,
    cc: Entity<TextInput>,
    bcc: Entity<TextInput>,
    show_bcc: bool,
    subject: Entity<TextInput>,
    body: Entity<TextArea>,
    body_scroll: ScrollHandle,
    /// What the draft opened with, to tell an edited draft from an untouched one, and a reply's
    /// recipients the engine planned from the ones the user changed.
    initial_to: String,
    initial_cc: String,
    initial_subject: SharedString,
    initial_body: SharedString,
    attachments: Attachments,
    /// Fields the last Send attempt refused.
    invalid: Vec<Field>,
    suggestions: Option<Suggestions>,
    /// The latest completion lookup; older answers are dropped.
    lookup: usize,
    /// `contacts.search` answered `unknown_command`: completion stays off.
    contacts_unavailable: bool,
    status: Status,
    confirming_discard: bool,
    _subscriptions: Vec<Subscription>,
}

impl Compose {
    fn new(mode: ComposeMode, accounts: Vec<Account>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (title, glyph): (SharedString, Icon) = match &mode {
            ComposeMode::New { .. } => ("New Message".into(), Icon::Compose),
            ComposeMode::Forward { .. } => ("Forward".into(), Icon::Forward),
            ComposeMode::Reply(reply) if reply.all => ("Reply All".into(), Icon::ReplyAll),
            ComposeMode::Reply(_) => ("Reply".into(), Icon::Reply),
        };
        let to = cx.new(|cx| TextInput::new("", cx));
        let cc = cx.new(|cx| TextInput::new("", cx));
        let bcc = cx.new(|cx| TextInput::new("", cx));
        let subject = cx.new(|cx| TextInput::new("Subject", cx));
        let body_scroll = ScrollHandle::new();
        let body = cx.new(|cx| {
            let mut body = TextArea::new("", metrics::BODY_MIN_ROWS, cx);
            body.scroll_with(body_scroll.clone());
            body
        });

        let mut attachments = Attachments::default();
        let mut reply = None;
        let (mut initial_to, mut initial_cc, mut text) = (String::new(), String::new(), String::new());
        let (from, from_locked, initial_subject, initial_body) = match mode {
            ComposeMode::New { from } => {
                let from = from.or_else(|| accounts.first().map(|account| account.id.clone()));
                (from, false, SharedString::default(), SharedString::default())
            }
            ComposeMode::Forward { from, subject, quoted } => {
                (Some(from), true, forward_subject(&subject), format!("\n\n{quoted}").into())
            }
            ComposeMode::Reply(moved) => {
                let list = |addresses: &[Address]| addresses.iter().map(Address::header_form).collect::<Vec<_>>().join(", ");
                initial_to = list(&moved.to);
                initial_cc = list(&moved.cc);
                text = moved.body;
                // Files that went missing since are left out.
                let _ = attachments.add(moved.attachments);
                reply = Some((moved.conversation, moved.all));
                (Some(moved.from), true, moved.subject, SharedString::default())
            }
        };
        for (input, value) in [(&to, initial_to.as_str()), (&cc, initial_cc.as_str()), (&subject, initial_subject.as_ref())] {
            if !value.is_empty() {
                input.update(cx, |input, cx| {
                    input.select_everything(cx);
                    input.replace_text_in_range(None, value, window, cx);
                });
            }
        }
        let text = if text.is_empty() { initial_body.to_string() } else { text };
        if !text.is_empty() {
            body.update(cx, |body, cx| body.set_text(text, cx));
        }

        let mut subscriptions = Vec::new();
        for (field, input) in [(Field::To, &to), (Field::Cc, &cc), (Field::Bcc, &bcc)] {
            subscriptions.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Changed => this.recipients_changed(field, cx),
                    InputEvent::Submit => {
                        if !this.accept_suggestion(window, cx) {
                            this.move_focus(1, window, cx);
                        }
                    }
                    InputEvent::Cancel => this.escape(window, cx),
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&subject, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => this.move_focus(1, window, cx),
            InputEvent::Cancel => this.escape(window, cx),
            InputEvent::Changed => this.edited(cx),
        }));
        subscriptions.push(cx.subscribe_in(&body, window, |this, _, event: &TextAreaEvent, window, cx| match event {
            TextAreaEvent::Submit => this.send(window, cx),
            TextAreaEvent::Changed => this.edited(cx),
        }));

        Self {
            focus: cx.focus_handle(),
            title,
            glyph,
            accounts,
            from,
            from_locked,
            reply,
            picking_from: false,
            to,
            cc,
            bcc,
            show_bcc: false,
            subject,
            body,
            body_scroll,
            initial_to,
            initial_cc,
            initial_subject,
            initial_body,
            attachments,
            invalid: Vec::new(),
            suggestions: None,
            lookup: 0,
            contacts_unavailable: false,
            status: Status::Idle,
            confirming_discard: false,
            _subscriptions: subscriptions,
        }
    }

    fn input(&self, field: Field) -> &Entity<TextInput> {
        match field {
            Field::To => &self.to,
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
        }
    }

    /// The fields Tab walks, top to bottom.
    fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
        let mut order = vec![self.to.focus_handle(cx), self.cc.focus_handle(cx)];
        if self.show_bcc {
            order.push(self.bcc.focus_handle(cx));
        }
        order.push(self.subject.focus_handle(cx));
        order.push(self.body.focus_handle(cx));
        order
    }

    /// To when it is empty, otherwise the body.
    fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle =
            if self.to.read(cx).text().is_empty() { self.to.focus_handle(cx) } else { self.body.focus_handle(cx) };
        window.focus(&handle, cx);
    }

    fn move_focus(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.focus_order(cx);
        let current = order.iter().position(|handle| handle.is_focused(window));
        let next = match current {
            Some(ix) => (ix as isize + delta).rem_euclid(order.len() as isize) as usize,
            None => 0,
        };
        self.suggestions = None;
        window.focus(&order[next], cx);
        cx.notify();
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accept_suggestion(window, cx) {
            self.move_focus(1, window, cx);
        }
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(-1, window, cx);
    }

    /// Clears a failed Send's note; the column follows the subject and To for the tab's title.
    fn edited(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Failed(_)) {
            self.status = Status::Idle;
        }
        cx.notify();
    }

    /// Whether closing would lose something the user wrote.
    fn has_draft(&self, cx: &App) -> bool {
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_owned();
        text(&self.to) != self.initial_to
            || text(&self.cc) != self.initial_cc
            || !text(&self.bcc).is_empty()
            || *self.subject.read(cx).text() != self.initial_subject
            || self.body.read(cx).text() != self.initial_body.as_ref()
            || !self.attachments.is_empty()
    }

    /// The tab's name: the subject, else who it goes to, else what it is.
    fn tab_title(&self, cx: &App) -> SharedString {
        let subject = self.subject.read(cx).text().trim();
        if !subject.is_empty() {
            return subject.to_owned().into();
        }
        let to = self.to.read(cx).text().trim();
        if to.is_empty() { self.title.clone() } else { format!("To: {to}").into() }
    }

    fn sending(&self) -> bool {
        matches!(self.status, Status::Sending)
    }

    // MARK: Attachments

    fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        self.status = match self.attachments.add(paths) {
            Ok(()) => Status::Idle,
            Err(message) => Status::Failed(message.into()),
        };
        cx.notify();
    }

    fn pick_files(&mut self, _: &Attach, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        attachments::pick(cx, |this, picked, cx| match picked {
            Ok(paths) => this.attach(paths, cx),
            Err(message) => this.fail(message, cx),
        });
    }

    /// Ctrl+V (⌘V) with files or an image on the clipboard attaches them; text goes on to the
    /// field.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(pasted) = attachments::pasted(cx) else { return };
        cx.stop_propagation();
        match pasted {
            Ok(paths) => self.attach(paths, cx),
            Err(message) => self.fail(message, cx),
        }
    }

    fn remover(cx: &mut Context<Self>) -> Remove {
        let this = cx.entity().downgrade();
        Rc::new(move |index, _, cx| {
            this.update(cx, |this, cx| {
                this.attachments.remove(index);
                cx.notify();
            })
            .ok();
        })
    }

    /// Closes at once when nothing would be lost; otherwise asks in the confirm bar.
    fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.sending() {
            return;
        }
        if self.has_draft(cx) {
            self.confirming_discard = true;
            cx.notify();
        } else {
            cx.emit(DraftEvent::Closed);
        }
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.escape(window, cx);
    }

    /// Escape closes what floats first (completion, the From menu, the confirm bar), then a draft
    /// with nothing written; a written one stays, and the keyboard goes back to the mail.
    fn escape(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions.is_some() || self.picking_from || self.confirming_discard {
            self.suggestions = None;
            self.picking_from = false;
            self.confirming_discard = false;
            cx.notify();
        } else if self.has_draft(cx) || self.sending() {
            cx.emit(DraftEvent::Unfocus);
        } else {
            cx.emit(DraftEvent::Closed);
        }
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        self.request_close(cx);
    }

    // MARK: Completion

    fn recipients_changed(&mut self, field: Field, cx: &mut Context<Self>) {
        self.invalid.retain(|invalid| *invalid != field);
        self.edited(cx);
        if self.contacts_unavailable {
            return;
        }
        self.lookup += 1;
        let generation = self.lookup;
        let query = split_recipients(self.input(field).read(cx).text()).last().map_or("", |last| last.trim()).to_owned();
        if query.is_empty() {
            if self.suggestions.take().is_some() {
                cx.notify();
            }
            return;
        }
        let taken: Vec<String> = [&self.to, &self.cc, &self.bcc]
            .iter()
            .flat_map(|input| split_recipients(input.read(cx).text()).into_iter().map(|part| Address::parse(part).email))
            .map(|email| email.to_lowercase())
            .collect();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(metrics::COMPLETION_DELAY).await;
            if this.read_with(cx, |this, _| this.lookup != generation).unwrap_or(true) {
                return;
            }
            let limit = (metrics::SUGGESTIONS + taken.len()) as u32;
            let answer = cx
                .background_executor()
                .spawn(async move { engine().search_contacts(query, limit).await })
                .await;
            this.update(cx, |this, cx| {
                if this.lookup != generation {
                    return;
                }
                this.suggestions = match answer {
                    Ok(people) => {
                        let people: Vec<Person> = people
                            .into_iter()
                            .map(Person::from)
                            .filter(|person| !taken.contains(&person.email.to_lowercase()))
                            .take(metrics::SUGGESTIONS)
                            .collect();
                        (!people.is_empty()).then_some(Suggestions { field, people, highlighted: 0 })
                    }
                    Err(error) => {
                        // An engine without the command: no completion, and no error either.
                        this.contacts_unavailable = error.code() == "unknown_command";
                        None
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn suggestion_next(&mut self, _: &SuggestionNext, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(1, cx);
    }

    fn suggestion_previous(&mut self, _: &SuggestionPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(-1, cx);
    }

    fn highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(suggestions) = &mut self.suggestions else {
            cx.propagate();
            return;
        };
        let count = suggestions.people.len() as isize;
        suggestions.highlighted = (suggestions.highlighted as isize + delta).rem_euclid(count) as usize;
        cx.notify();
    }

    /// Replaces the field's last, unfinished recipient with the highlighted person.
    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(suggestions) = &self.suggestions else { return false };
        if !self.input(suggestions.field).focus_handle(cx).is_focused(window) {
            return false;
        }
        let Some(suggestions) = self.suggestions.take() else { return false };
        self.pick(suggestions.field, &suggestions.people[suggestions.highlighted], window, cx);
        true
    }

    fn pick(&mut self, field: Field, person: &Person, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input(field).clone();
        let current = input.read(cx).text().clone();
        let parts = split_recipients(&current);
        let mut text: Vec<String> =
            parts[..parts.len() - 1].iter().map(|part| part.trim()).filter(|part| !part.is_empty()).map(str::to_owned).collect();
        text.push(person.formatted());
        let text = format!("{}, ", text.join(", "));
        input.update(cx, |input, cx| {
            input.select_everything(cx);
            input.replace_text_in_range(None, &text, window, cx);
        });
        self.suggestions = None;
        window.focus(&input.focus_handle(cx), cx);
        cx.notify();
    }

    // MARK: Sending

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Status::Failed(message.into());
        cx.notify();
    }

    fn send_action(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        self.send(window, cx);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        self.confirming_discard = false;
        self.suggestions = None;
        let Some(from) = self.from.clone() else {
            return self.fail("Add an account in Mailstream to send from.", cx);
        };
        self.invalid.clear();
        let mut lists = Vec::with_capacity(3);
        for field in [Field::To, Field::Cc, Field::Bcc] {
            match parse_recipients(self.input(field).read(cx).text()) {
                Ok(list) => lists.push(list),
                Err(bad) => {
                    self.invalid.push(field);
                    if field == Field::Bcc {
                        self.show_bcc = true;
                    }
                    return self.fail(format!("{}: “{bad}” is not an email address.", field.label()), cx);
                }
            }
        }
        let [to, cc, bcc]: [Vec<String>; 3] = lists.try_into().expect("three recipient lists");
        if to.is_empty() {
            self.invalid.push(Field::To);
            window.focus(&self.to.focus_handle(cx), cx);
            return self.fail("Add at least one recipient.", cx);
        }
        let subject = self.subject.read(cx).text().trim().to_string();
        let body = self.body.read(cx).text().to_string();
        let attachments = self.attachments.paths();
        let request = match self.reply.clone() {
            // Fields left as they opened keep the engine's reply plan.
            Some((conversation, all)) => {
                let changed = |input: &Entity<TextInput>, initial: &str| input.read(cx).text().trim() != initial;
                let overrides = mailstream::ReplyOverrides {
                    to: changed(&self.to, &self.initial_to).then_some(to),
                    cc: changed(&self.cc, &self.initial_cc).then_some(cc),
                    bcc: (!bcc.is_empty()).then_some(bcc),
                    subject: (subject != self.initial_subject.as_ref()).then_some(subject),
                    attachments: (!attachments.is_empty()).then_some(attachments),
                };
                Outgoing::Reply { id: conversation.to_string(), body, all, overrides }
            }
            None => Outgoing::Message(mailstream::Draft { from: from.to_string(), to, cc, bcc, subject, body, attachments }),
        };
        self.status = Status::Sending;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    match request {
                        Outgoing::Message(draft) => engine().send(draft, true).await,
                        Outgoing::Reply { id, body, all, overrides } => {
                            engine().reply(id, body, all, Some(overrides), true).await
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| match answer {
                Ok(_) => {
                    this.status = Status::Idle;
                    cx.emit(DraftEvent::Sent);
                }
                Err(error) => this.fail(error.to_string(), cx),
            })
            .ok();
        })
        .detach();
    }

    // MARK: Rendering

    /// One header line over a hairline: a right-aligned label, then the field.
    fn header_row(&self, label: Option<&'static str>, invalid: bool, p: &Palette) -> Div {
        div()
            .relative()
            .flex_shrink_0()
            .h(px(metrics::ROW_HEIGHT))
            .ml(px(metrics::MARGIN))
            .pr(px(metrics::MARGIN))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.line)
            .when_some(label, |row, label| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .w(px(metrics::LABEL_WIDTH))
                        .flex()
                        .justify_end()
                        .text_color(if invalid { p.danger } else { p.muted })
                        .child(format!("{label}:")),
                )
            })
    }

    fn render_from(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let account = self.from.as_ref().and_then(|from| self.accounts.iter().find(|account| &account.id == from));
        let (title, address) = match (account, &self.from) {
            (Some(account), _) if !account.label.is_empty() => (account.label.clone(), Some(account.id.clone())),
            (_, Some(from)) => (from.clone(), None),
            _ => ("No account".into(), None),
        };
        let pickable = !self.from_locked && self.accounts.len() > 1;
        let picker = div()
            .id("from")
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().truncate().child(title))
            .when_some(address, |d, address| d.child(div().truncate().text_color(p.muted).child(address)))
            .when(pickable, |d| {
                d.cursor_pointer()
                    .child(div().text_color(p.muted).child("▾"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.picking_from = !this.picking_from;
                        cx.notify();
                    }))
            });
        let menu = self.picking_from.then(|| {
            let rows = self.accounts.iter().enumerate().map(|(index, account)| {
                let id = account.id.clone();
                let selected = self.from.as_ref() == Some(&account.id);
                div()
                    .id(ElementId::Name(format!("from:{}", account.id).into()))
                    .h(px(metrics::MENU_ROW))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(p.selection))
                    .hover(|s| s.bg(p.hover))
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(design::account_color(index)))
                    .child(div().flex_1().min_w_0().truncate().child(account.title()))
                    .when(!account.label.is_empty(), |d| {
                        d.child(div().flex_shrink_0().text_color(p.muted).text_size(px(text::FOOTNOTE)).child(account.id.clone()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.from = Some(id.clone());
                        this.picking_from = false;
                        cx.notify();
                    }))
            });
            deferred(
                menu(p)
                    .id("from-menu")
                    .top(px(metrics::ROW_HEIGHT))
                    .left(px(metrics::LABEL_WIDTH + 8.))
                    .children(rows)
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.picking_from = false;
                        cx.notify();
                    })),
            )
            .with_priority(1)
        });
        self.header_row(Some("From"), false, p).child(picker).children(menu)
    }

    fn render_recipients(&self, field: Field, window: &Window, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let input = self.input(field);
        let suggestions = self
            .suggestions
            .as_ref()
            .filter(|suggestions| suggestions.field == field && input.focus_handle(cx).is_focused(window))
            .map(|suggestions| {
                let rows = suggestions.people.iter().enumerate().map(|(ix, person)| {
                    let address = person.address();
                    let picked = person.clone();
                    let photo = avatars::avatar(&address, metrics::MENU_AVATAR, p, cx);
                    div()
                        .id(ElementId::NamedInteger("suggestion".into(), ix as u64))
                        .h(px(metrics::MENU_ROW))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(radius::SMALL))
                        .cursor_pointer()
                        .when(ix == suggestions.highlighted, |d| d.bg(p.selection))
                        .hover(|s| s.bg(p.hover))
                        .child(photo)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().child(address.display_name()))
                                .when(address.name.is_some(), |d| {
                                    d.child(
                                        div()
                                            .truncate()
                                            .text_size(px(text::FOOTNOTE))
                                            .text_color(p.muted)
                                            .child(address.email.clone()),
                                    )
                                }),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.pick(field, &picked, window, cx);
                            }),
                        )
                });
                deferred(
                    menu(p)
                        .id(ElementId::Name(format!("suggestions:{}", field.label()).into()))
                        .top(px(metrics::ROW_HEIGHT))
                        .left(px(metrics::LABEL_WIDTH + 8.))
                        .children(rows),
                )
                .with_priority(1)
            });
        self.header_row(Some(field.label()), self.invalid.contains(&field), p)
            .key_context(RECIPIENTS)
            .child(div().flex_1().min_w_0().flex().child(input.clone()))
            .children(suggestions)
    }

    fn render_confirm(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_shrink_0()
            .px(px(metrics::MARGIN))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.fill)
            .child(div().flex_1().min_w_0().truncate().child("Discard this message?"))
            .child(button("keep", "Keep Editing", ButtonKind::Secondary, ControlSize::Regular, true, p).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.confirming_discard = false;
                    window.focus(&this.body.focus_handle(cx), cx);
                    cx.notify();
                }),
            ))
            .child(
                button("discard", "Discard", ButtonKind::Destructive, ControlSize::Regular, true, p)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DraftEvent::Closed))),
            )
    }
}

impl EventEmitter<DraftEvent> for Compose {}

/// A floating menu under a header field.
fn menu(p: &Palette) -> Div {
    popover(p)
        .absolute()
        .w(px(metrics::MENU_WIDTH))
        .p(px(space::S4))
        .flex()
        .flex_col()
        .occlude()
        // A click in the menu leaves the keyboard in the field.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

impl Render for Compose {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let sending = matches!(self.status, Status::Sending);
        let body = self.body.clone();
        let attached = self.attachments.render("compose-attachment", !sending, &Self::remover(cx), p);
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::send_action))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::suggestion_next))
            .on_action(cx.listener(Self::suggestion_previous))
            .on_action(cx.listener(Self::pick_files))
            .capture_action(cx.listener(|this, _: &crate::input::Paste, _, cx| this.paste(cx)))
            .capture_action(cx.listener(|this, _: &crate::textarea::Paste, _, cx| this.paste(cx)))
            .group(COMPOSE_DROP)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach(paths.paths().to_vec(), cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                    .child(self.render_from(p, cx))
                    .child(self.render_recipients(Field::To, window, p, cx))
                    .child(self.render_recipients(Field::Cc, window, p, cx).when(!self.show_bcc, |row| {
                        row.child(button("show-bcc", "Bcc", ButtonKind::Plain(p.accent), ControlSize::Small, true, p).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.show_bcc = true;
                                window.focus(&this.bcc.focus_handle(cx), cx);
                                cx.notify();
                            },
                        )))
                    }))
                    .when(self.show_bcc, |d| d.child(self.render_recipients(Field::Bcc, window, p, cx)))
                    .child(
                        self.header_row(None, false, p)
                            .h(px(metrics::SUBJECT_HEIGHT))
                            .text_size(px(text::PANE_TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.subject.clone()),
                    )
                    .when_some(attached, |d, attached| {
                        d.child(
                            div()
                                .ml(px(metrics::MARGIN))
                                .pr(px(metrics::MARGIN))
                                .py(px(space::S8))
                                .border_b_1()
                                .border_color(p.line)
                                .child(attached),
                        )
                    }),
            )
            .child(
                div()
                    .id("compose-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.body_scroll)
                    .cursor_text()
                    // The window's own focus would otherwise take the click (`prevent_default` keeps it).
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&body.focus_handle(cx), cx);
                        window.prevent_default();
                    })
                    .child(
                        div()
                            .px(px(metrics::MARGIN))
                            .py(px(metrics::BODY_PADDING))
                            .text_size(px(text::READING))
                            .line_height(relative(editor::LEADING))
                            .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                            .child(self.body.clone()),
                    ),
            )
            .when_some(status_line(&self.status, p), |d, status| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(metrics::MARGIN))
                        .py(px(8.))
                        .flex()
                        .border_t_1()
                        .border_color(p.line)
                        .child(status),
                )
            })
            .when(self.confirming_discard, |d| d.child(self.render_confirm(p, cx)))
            .child(attachments::drop_target(COMPOSE_DROP, radius::PANE, p))
    }
}

// MARK: - Inline reply

pub enum ReplyEvent {
    Sent,
    Discarded,
    /// Moved to the compose column to change its recipients or subject, with what it holds.
    Moved { body: String, attachments: Vec<PathBuf> },
}

/// The reader's reply card: a mail reply (or reply all) to a conversation's latest message, with
/// files picked, dropped or pasted on it.
pub struct ReplyBox {
    conversation_id: SharedString,
    all: bool,
    recipients: SharedString,
    text: Entity<TextArea>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    attachments: Attachments,
    status: Status,
    _subscription: Subscription,
}

impl EventEmitter<ReplyEvent> for ReplyBox {}

impl ReplyBox {
    /// `recipients_preview` is who the reply goes to, as the reader shows it.
    pub fn new(
        conversation_id: SharedString,
        all: bool,
        recipients_preview: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let scroll = ScrollHandle::new();
        let text = cx.new(|cx| {
            let mut text = TextArea::new(if all { "Reply to everyone…" } else { "Reply…" }, metrics::REPLY_MIN_ROWS, cx);
            text.scroll_with(scroll.clone());
            text
        });
        let focus = text.focus_handle(cx);
        window.focus(&focus, cx);
        let subscription = cx.subscribe_in(&text, window, |this, _, event: &TextAreaEvent, window, cx| match event {
            TextAreaEvent::Submit => this.send(window, cx),
            TextAreaEvent::Changed => {
                if matches!(this.status, Status::Failed(_)) {
                    this.status = Status::Idle;
                    cx.notify();
                }
            }
        });
        Self {
            conversation_id,
            all,
            recipients: recipients_preview,
            text,
            focus,
            scroll,
            attachments: Attachments::default(),
            status: Status::Idle,
            _subscription: subscription,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        let body = self.text.read(cx).text().to_owned();
        if body.trim().is_empty() && self.attachments.is_empty() {
            self.status = Status::Failed("Write something to send.".into());
            cx.notify();
            return;
        }
        let (id, all) = (self.conversation_id.to_string(), self.all);
        let overrides = (!self.attachments.is_empty()).then(|| mailstream::ReplyOverrides {
            to: None,
            cc: None,
            bcc: None,
            subject: None,
            attachments: Some(self.attachments.paths()),
        });
        self.status = Status::Sending;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { engine().reply(id, body, all, overrides, true).await })
                .await;
            this.update(cx, |this, cx| {
                match answer {
                    Ok(_) => {
                        this.status = Status::Idle;
                        cx.emit(ReplyEvent::Sent);
                    }
                    Err(error) => this.status = Status::Failed(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Escape leaves an empty reply; written text and files stay until Send or Discard.
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.text.read(cx).text().trim().is_empty()
            && self.attachments.is_empty()
            && !matches!(self.status, Status::Sending)
        {
            cx.emit(ReplyEvent::Discarded);
        }
    }

    /// Hands the reply to the compose column, where its recipients and subject can change.
    fn move_to_column(&mut self, _: &MoveToColumn, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        let body = self.text.read(cx).text().to_owned();
        cx.emit(ReplyEvent::Moved { body, attachments: self.attachments.take() });
    }

    fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        self.status = match self.attachments.add(paths) {
            Ok(()) => Status::Idle,
            Err(message) => Status::Failed(message.into()),
        };
        cx.notify();
    }

    fn pick_files(&mut self, _: &Attach, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        attachments::pick(cx, |this, picked, cx| match picked {
            Ok(paths) => this.attach(paths, cx),
            Err(message) => {
                this.status = Status::Failed(message.into());
                cx.notify();
            }
        });
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(pasted) = attachments::pasted(cx) else { return };
        cx.stop_propagation();
        match pasted {
            Ok(paths) => self.attach(paths, cx),
            Err(message) => {
                self.status = Status::Failed(message.into());
                cx.notify();
            }
        }
    }

    fn remover(cx: &mut Context<Self>) -> Remove {
        let this = cx.entity().downgrade();
        Rc::new(move |index, _, cx| {
            this.update(cx, |this, cx| {
                this.attachments.remove(index);
                cx.notify();
            })
            .ok();
        })
    }
}

impl Render for ReplyBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let sending = matches!(self.status, Status::Sending);
        let mut ring = p.accent;
        ring.a = metrics::FOCUS_RING_ALPHA;
        let focus = self.focus.clone();
        let attached = self.attachments.render("reply-attachment", !sending, &Self::remover(cx), p);
        div()
            .key_context(REPLY_CONTEXT)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::pick_files))
            .on_action(cx.listener(Self::move_to_column))
            .capture_action(cx.listener(|this, _: &crate::textarea::Paste, _, cx| this.paste(cx)))
            .group(REPLY_DROP)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach(paths.paths().to_vec(), cx)))
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(radius::CARD))
            .border_1()
            .border_color(if self.focus.is_focused(window) { ring } else { p.line })
            .bg(p.background)
            .shadow(p.popover_shadow.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .pt(px(8.))
                    .px(px(metrics::REPLY_PADDING))
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .child(
                        div()
                            .flex_shrink_0()
                            .mr(px(8.))
                            .text_color(p.accent)
                            .font_weight(FontWeight::MEDIUM)
                            .child(if self.all { "↩ Reply All" } else { "↩ Reply" }),
                    )
                    .child(div().flex_shrink_0().text_color(p.faint).child("To"))
                    .child(div().flex_1().min_w_0().truncate().child(self.recipients.clone())),
            )
            .child(
                div()
                    .id("reply-text")
                    .max_h(px(metrics::REPLY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&focus, cx);
                        window.prevent_default();
                    })
                    .px(px(metrics::REPLY_PADDING))
                    .py(px(8.))
                    .text_size(px(text::READING))
                    .line_height(relative(editor::LEADING))
                    .text_color(p.text)
                    .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                    .child(self.text.clone()),
            )
            .when_some(attached, |d, attached| d.child(div().px(px(metrics::REPLY_PADDING)).pb(px(8.)).child(attached)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(metrics::REPLY_PADDING))
                    .pr(px(8.))
                    .pb(px(8.))
                    .child(status_line(&self.status, p).unwrap_or_else(|| div().flex_1()))
                    .child(
                        icon_button("reply-attach", Icon::Attachment, ATTACH_TIP, !sending, p.muted).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.pick_files(&Attach, window, cx)),
                        ),
                    )
                    .child(
                        icon_button("reply-move", Icon::Dock, MOVE_TIP, !sending, p.muted)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.move_to_column(&MoveToColumn, window, cx)
                            })),
                    )
                    .child(
                        button("reply-discard", "Discard", ButtonKind::Secondary, ControlSize::Regular, !sending, p)
                            .when(!sending, |b| {
                                b.on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ReplyEvent::Discarded)))
                            }),
                    )
                    .child(
                        send_button("reply-send", sending, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx))),
                    ),
            )
            .child(attachments::drop_target(REPLY_DROP, radius::CARD, p))
    }
}
