//! Writing mail: the drafts of the compose column (`column.rs`; a new message, a forward or a
//! reply moved out of the reader, with every field editable) and the reader's inline reply
//! (`mail.reply`). Clicking Send is the confirmation the engine asks for, so both pass `confirm`.
//! Recipients complete from `contacts.search` when the running app offers it. Both carry
//! attachments (`attachments.rs`).

mod attachments;
mod column;

use std::path::PathBuf;
use std::rc::Rc;

use gpui::component::button::Button;
use gpui::component::input::{EditorState, Escape, InputEvent, InputState, Paste};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, deferred, div, px, relative,
};
use crate::avatars;
use crate::controls::{ButtonKind, ControlSize, button, icon_button, popover};
use crate::design::{self, Palette, compose as metrics, editor, radius, space, text};
use crate::engine::engine;
use crate::icons::Icon;
use crate::input::{self, AREA_INSET, insert_link, wrap_selection};
use crate::markup_highlight::{MarkupFlavor, markup_highlighter_factory};
use crate::model::AccountExt;
use mailstream::{Account, Address, Person};
use attachments::{Attachments, Remove};
pub use column::{ColumnEvent, ComposeColumn, ResizeColumn};

actions!(
    compose,
    [
        Send,
        Cancel,
        Close,
        FocusNext,
        FocusPrevious,
        SuggestionNext,
        SuggestionPrevious,
        Attach,
        MoveToColumn,
        FormatBold,
        FormatItalic,
        FormatLink,
        FormatStrike,
        FormatCode,
        PastePlainText
    ]
);

const CONTEXT: &str = "Compose";
const BODY_CONTEXT: &str = "ComposeBody";
const RECIPIENTS: &str = "Recipients";
const REPLY_CONTEXT: &str = "ReplyBox";
/// The groups files are dragged over.
const COMPOSE_DROP: &str = "compose-drop";
const REPLY_DROP: &str = "reply-drop";
const ATTACH_TIP: &str = "Attach Files (⇧⌘A), or drop or paste them here";
const MOVE_TIP: &str = "Move to the Compose Column to Change Recipients or Subject (⇧⌘O)";

#[cfg(target_os = "macos")]
const PASTE_PLAIN_KEY: &str = "cmd-alt-shift-v";
#[cfg(not(target_os = "macos"))]
const PASTE_PLAIN_KEY: &str = "ctrl-shift-v";

pub fn bind_keys(cx: &mut App) {
    let window = Some(CONTEXT);
    // The kit's text fields take these keys themselves (a newline, an indent); in a draft they
    // send and walk the fields. Bound after the kit's, so they win inside its fields.
    let field = Some("Compose > Input");
    let body_field = Some("ComposeBody > Input");
    let reply_field = Some("ReplyBox > Input");
    cx.bind_keys([
        KeyBinding::new(secondary!("enter"), Send, window),
        KeyBinding::new(secondary!("enter"), Send, field),
        KeyBinding::new("escape", Cancel, window),
        KeyBinding::new(secondary!("w"), Close, window),
        KeyBinding::new("tab", FocusNext, window),
        KeyBinding::new("tab", FocusNext, field),
        KeyBinding::new("shift-tab", FocusPrevious, window),
        KeyBinding::new("shift-tab", FocusPrevious, field),
        KeyBinding::new(secondary!("shift-a"), Attach, window),
        KeyBinding::new("down", SuggestionNext, Some("Recipients > Input")),
        KeyBinding::new("up", SuggestionPrevious, Some("Recipients > Input")),
        KeyBinding::new("escape", Cancel, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("w"), Cancel, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("enter"), Send, reply_field),
        KeyBinding::new(secondary!("shift-a"), Attach, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("shift-o"), MoveToColumn, Some(REPLY_CONTEXT)),
        KeyBinding::new(secondary!("b"), FormatBold, body_field),
        KeyBinding::new(secondary!("i"), FormatItalic, body_field),
        KeyBinding::new(secondary!("k"), FormatLink, body_field),
        KeyBinding::new(secondary!("shift-x"), FormatStrike, body_field),
        KeyBinding::new(secondary!("e"), FormatCode, body_field),
        KeyBinding::new(PASTE_PLAIN_KEY, PastePlainText, body_field),
        KeyBinding::new(secondary!("b"), FormatBold, reply_field),
        KeyBinding::new(secondary!("i"), FormatItalic, reply_field),
        KeyBinding::new(secondary!("k"), FormatLink, reply_field),
        KeyBinding::new(secondary!("shift-x"), FormatStrike, reply_field),
        KeyBinding::new(secondary!("e"), FormatCode, reply_field),
        KeyBinding::new(PASTE_PLAIN_KEY, PastePlainText, reply_field),
    ]);
}

/// Pastes clipboard HTML as Markdown into `editor` when present and non-empty; returns whether
/// the paste was handled.
fn paste_rich_into_editor(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) -> bool {
    let Some(item) = cx.read_from_clipboard() else { return false };
    let Some(html) = item.html() else { return false };
    let markdown = mailstream::html_to_markdown(html);
    if markdown.trim().is_empty() {
        return false;
    }
    editor.update(cx, |input, cx| input.replace(markdown, window, cx));
    true
}

/// Pastes clipboard plain text directly into `editor`, bypassing HTML-to-Markdown conversion.
fn paste_plain_into_editor(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) -> bool {
    let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return false };
    if text.is_empty() {
        return false;
    }
    editor.update(cx, |input, cx| input.replace(text, window, cx));
    true
}

/// What a draft starts as. A forward's `subject` is the original's; `quoted` is the original as
/// text, put under the caret. A reply moved out of the reader keeps its text and files; its `to`
/// and `cc` are who the engine will send it to, unless they are edited.
pub enum ComposeMode {
    New { from: Option<String> },
    Forward { from: String, conversation: String, message: String, subject: String, quoted: SharedString },
    Reply(MovedReply),
}

/// A reply the reader moved to the compose column.
pub struct MovedReply {
    pub conversation: String,
    /// The message it answers; the conversation's latest when `None`.
    pub message: Option<String>,
    pub all: bool,
    pub from: String,
    pub from_chosen: bool,
    pub from_resolved: bool,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: SharedString,
    pub body: String,
    pub attachments: Vec<PathBuf>,
}

#[derive(Clone)]
struct FromOption {
    account_index: usize,
    account_title: SharedString,
    name: Option<String>,
    email: String,
}

fn from_options(accounts: &[Account]) -> Vec<FromOption> {
    let mut options = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (account_index, account) in accounts.iter().enumerate() {
        let account_title = account.title();
        for identity in account.send_identities() {
            let key = identity.normalized_email();
            if seen.insert(key) {
                options.push(FromOption {
                    account_index,
                    account_title: account_title.clone(),
                    name: identity.name.clone().filter(|name| !name.is_empty()),
                    email: identity.email,
                });
            }
        }
    }
    options
}

fn selected_from_option<'a>(options: &'a [FromOption], from: Option<&str>) -> Option<&'a FromOption> {
    let from = from?;
    options.iter().find(|option| option.email.eq_ignore_ascii_case(from))
}

/// One person `contacts.search` offers.
/// `person` as a recipient field takes them: `Name <email>`, quoting a name the list would split.
fn formatted(person: &Person) -> String {
    match person.name.as_ref().filter(|name| !name.is_empty()) {
        Some(name) if name.contains([',', ';', '"', '<', '>']) => {
            format!("\"{}\" <{}>", name.replace('"', "'"), person.email)
        }
        Some(name) => format!("{name} <{}>", person.email),
        None => person.email.clone(),
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

/// The accent Send button with its shortcut; dimmed and inert while sending.
fn send_button(id: &'static str, sending: bool, show_shortcut: bool, compact: bool, p: &Palette) -> Button {
    button(id, if sending { "Sending…" } else { "Send" }, ButtonKind::Primary, ControlSize::regular(compact), !sending, p).when(
        !sending && show_shortcut,
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
    from: Option<String>,
    /// Only forwards keep the account it came to locked.
    from_locked: bool,
    from_chosen: bool,
    from_resolved: bool,
    /// The conversation a moved reply answers, the message in it (else its latest), and whether
    /// it answers everyone: sent with `mail.reply`, which keeps the thread and quotes the original.
    reply: Option<(String, Option<String>, bool)>,
    /// The conversation and message a forward quotes, so the engine sends its HTML quote too.
    forward: Option<(String, String)>,
    picking_from: bool,
    to: Entity<InputState>,
    cc: Entity<InputState>,
    bcc: Entity<InputState>,
    show_bcc: bool,
    subject: Entity<InputState>,
    body: Entity<EditorState>,
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
        let to = cx.new(|cx| InputState::new(window, cx));
        let cc = cx.new(|cx| InputState::new(window, cx));
        let bcc = cx.new(|cx| InputState::new(window, cx));
        let subject = cx.new(|cx| InputState::new(window, cx).placeholder("Subject").autocorrect(true));
        let body = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .soft_wrap(true)
                .searchable(false)
                .auto_close(false)
                .smart_indent(false)
                .folding(false)
                .indent_guides(false)
                .placeholder("Write in Markdown…")
                .autocorrect(true);
            state.set_highlighter_factory(markup_highlighter_factory(MarkupFlavor::Markdown), cx);
            state.set_highlighter("markdown", cx);
            state
        });

        let mut attachments = Attachments::default();
        let (mut reply, mut forward) = (None, None);
        let (mut initial_to, mut initial_cc, mut text) = (String::new(), String::new(), String::new());
        let (from, from_locked, from_chosen, from_resolved, initial_subject, initial_body) = match mode {
            ComposeMode::New { from } => {
                let from = from
                    .and_then(|id| {
                        accounts.iter().find(|account| account.id.eq_ignore_ascii_case(&id)).map(|a| a.primary_email().to_owned())
                    })
                    .or_else(|| accounts.first().map(|account| account.primary_email().to_owned()));
                (from, false, true, true, SharedString::default(), SharedString::default())
            }
            ComposeMode::Forward { from, conversation, message, subject, quoted } => {
                forward = Some((conversation, message));
                (Some(from), true, true, true, mailstream::forward_subject(subject.to_string()).into(), format!("\n\n{quoted}").into())
            }
            ComposeMode::Reply(moved) => {
                let list = |addresses: &[Address]| addresses.iter().map(Address::text).collect::<Vec<_>>().join(", ");
                initial_to = list(&moved.to);
                initial_cc = list(&moved.cc);
                text = moved.body;
                // Files that went missing since are left out.
                let _ = attachments.add(moved.attachments);
                reply = Some((moved.conversation, moved.message, moved.all));
                (Some(moved.from), false, moved.from_chosen, moved.from_resolved, moved.subject, SharedString::default())
            }
        };
        for (input, value) in [(&to, initial_to.as_str()), (&cc, initial_cc.as_str()), (&subject, initial_subject.as_ref())] {
            if !value.is_empty() {
                input.update(cx, |input, cx| input.set_value(value.to_owned(), window, cx));
            }
        }
        let text = if text.is_empty() { initial_body.to_string() } else { text };
        if !text.is_empty() {
            // The caret starts above a quote.
            body.update(cx, |body, cx| body.set_value(text, window, cx));
        }

        let mut subscriptions = Vec::new();
        for (field, input) in [(Field::To, &to), (Field::Cc, &cc), (Field::Bcc, &bcc)] {
            subscriptions.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => this.recipients_changed(field, cx),
                    InputEvent::PressEnter { secondary: false, .. } => {
                        if !this.accept_suggestion(window, cx) {
                            this.move_focus(1, window, cx);
                        }
                    }
                    _ => {}
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&subject, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { secondary: false, .. } => this.move_focus(1, window, cx),
            InputEvent::Change => this.edited(cx),
            _ => {}
        }));
        subscriptions.push(cx.subscribe_in(&body, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.edited(cx);
            }
        }));
        // The highlighter takes its colors when it parses: made again in the new palette.
        subscriptions.push(cx.observe_window_appearance(window, |this, _, cx| {
            this.body.update(cx, |state, cx| state.set_highlighter("markdown", cx));
        }));

        Self {
            focus: cx.focus_handle(),
            title,
            glyph,
            accounts,
            from,
            from_locked,
            from_chosen,
            from_resolved,
            reply,
            forward,
            picking_from: false,
            to,
            cc,
            bcc,
            show_bcc: false,
            subject,
            body,
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

    fn input(&self, field: Field) -> &Entity<InputState> {
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
            if self.to.read(cx).value().is_empty() { self.to.focus_handle(cx) } else { self.body.focus_handle(cx) };
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
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        text(&self.to) != self.initial_to
            || text(&self.cc) != self.initial_cc
            || !text(&self.bcc).is_empty()
            || self.subject.read(cx).value() != self.initial_subject
            || self.body.read(cx).value() != self.initial_body
            || !self.attachments.is_empty()
    }

    /// The tab's name: the subject, else who it goes to, else what it is.
    fn tab_title(&self, cx: &App) -> SharedString {
        let subject = self.subject.read(cx).value();
        let subject = subject.trim();
        if !subject.is_empty() {
            return subject.to_owned().into();
        }
        let to = self.to.read(cx).value();
        let to = to.trim();
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

    /// Ctrl+V (⌘V) with files or an image attaches them; rich HTML in the body converts to
    /// Markdown, while plain text (and header fields) go on to the field.
    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pasted) = attachments::pasted(cx) {
            cx.stop_propagation();
            match pasted {
                Ok(paths) => self.attach(paths, cx),
                Err(message) => self.fail(message, cx),
            }
            return;
        }
        if !self.sending() && self.body.focus_handle(cx).is_focused(window) && paste_rich_into_editor(&self.body, window, cx) {
            cx.stop_propagation();
        }
    }

    fn paste_plain_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sending() && self.body.focus_handle(cx).is_focused(window) && paste_plain_into_editor(&self.body, window, cx) {
            cx.stop_propagation();
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
        let typed = mailstream::split_recipients(self.input(field).read(cx).value().to_string());
        let query = typed.last().map_or("", |last| last.trim()).to_owned();
        if query.is_empty() {
            if self.suggestions.take().is_some() {
                cx.notify();
            }
            return;
        }
        let taken: Vec<String> = [&self.to, &self.cc, &self.bcc]
            .iter()
            .flat_map(|input| {
                let text = input.read(cx).value();
                mailstream::split_recipients(text.to_string()).iter().map(|part| mailstream::address_text::read(part).email).collect::<Vec<_>>()
            })
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
        let current = input.read(cx).value();
        let parts = mailstream::split_recipients(current.to_string());
        let mut text: Vec<String> =
            parts[..parts.len() - 1].iter().map(|part| part.trim()).filter(|part| !part.is_empty()).map(str::to_owned).collect();
        text.push(formatted(person));
        let text = format!("{}, ", text.join(", "));
        // Replaced as an edit, so Undo brings back what was typed.
        input.update(cx, |input, cx| input.replace_all(text, window, cx));
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
            match mailstream::parse_recipients(self.input(field).read(cx).value().to_string()) {
                mailstream::Recipients { recipients, invalid: None } => lists.push(recipients),
                mailstream::Recipients { invalid: Some(bad), .. } => {
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
        let subject = self.subject.read(cx).value().trim().to_string();
        let body = self.body.read(cx).value().to_string();
        let attachments = self.attachments.paths();
        let request = match self.reply.clone() {
            // Fields left as they opened keep the engine's reply plan.
            Some((conversation, message, all)) => {
                let changed = |input: &Entity<InputState>, initial: &str| input.read(cx).value().trim() != initial;
                let overrides = mailstream::ReplyOverrides {
                    from: (self.from_chosen || self.from_resolved).then_some(from),
                    to: changed(&self.to, &self.initial_to).then_some(to),
                    cc: changed(&self.cc, &self.initial_cc).then_some(cc),
                    bcc: (!bcc.is_empty()).then_some(bcc),
                    subject: (subject != self.initial_subject.as_ref()).then_some(subject),
                    attachments: (!attachments.is_empty()).then_some(attachments),
                    message: message.map(Into::into),
                };
                Outgoing::Reply { id: conversation.to_string(), body, all, overrides }
            }
            None => {
                let (forward, forward_message) = self
                    .forward
                    .as_ref()
                    .filter(|_| body.contains("---------- Forwarded message ---------"))
                    .map(|(conversation, message)| (Some(conversation.clone()), Some(message.clone())))
                    .unwrap_or((None, None));
                Outgoing::Message(mailstream::Draft {
                    from: from.to_string(),
                    to,
                    cc,
                    bcc,
                    subject,
                    body,
                    attachments,
                    forward,
                    forward_message,
                })
            }
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
    fn header_row(&self, label: Option<&'static str>, invalid: bool, compact: bool, p: &Palette) -> Div {
        let row_h = if compact { 44. } else { metrics::ROW_HEIGHT };
        let margin = if compact { design::compact::READER_PADDING } else { metrics::MARGIN };
        let label_w = if compact { 56. } else { metrics::LABEL_WIDTH };
        div()
            .relative()
            .flex_shrink_0()
            .h(px(row_h))
            .ml(px(margin))
            .pr(px(margin))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.line)
            .when_some(label, |row, label| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .w(px(label_w))
                        .flex()
                        .justify_end()
                        .text_color(if invalid { p.danger } else { p.muted })
                        .child(format!("{label}:")),
                )
            })
    }

    fn render_from(&self, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let options = from_options(&self.accounts);
        let selected = selected_from_option(&options, self.from.as_deref());
        let (title, address): (SharedString, Option<SharedString>) = match (selected, &self.from) {
            (Some(option), _) => {
                let label = option.name.as_deref().unwrap_or(option.account_title.as_ref());
                if !label.is_empty() && !label.eq_ignore_ascii_case(&option.email) {
                    (label.to_owned().into(), Some(option.email.clone().into()))
                } else {
                    (option.email.clone().into(), None)
                }
            }
            (_, Some(from)) => (from.clone().into(), None),
            _ => ("No account".into(), None),
        };
        let pickable = !self.from_locked && options.len() > 1;
        let picker = div()
            .id("from")
            .debug_selector(|| "from".into())
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .when_some(selected, |d, option| {
                d.child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(design::account_color(option.account_index)))
            })
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
            let rows = options.into_iter().map(|option| {
                let email = option.email.clone();
                let is_selected = self.from.as_deref().is_some_and(|from| from.eq_ignore_ascii_case(&email));
                let title: SharedString = match option.name.as_deref() {
                    Some(name) if !name.eq_ignore_ascii_case(&email) => name.to_owned().into(),
                    _ if !option.account_title.eq_ignore_ascii_case(&email) => option.account_title.clone(),
                    _ => email.clone().into(),
                };
                let has_subtitle = !title.eq_ignore_ascii_case(&email);
                let row_sel = format!("from:{email}");
                div()
                    .id(ElementId::Name(row_sel.clone().into()))
                    .debug_selector(move || row_sel.clone())
                    .h(px(metrics::MENU_ROW))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(p.selection))
                    .hover(|s| s.bg(p.hover))
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(design::account_color(option.account_index)))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(has_subtitle, |d| {
                        d.child(div().flex_shrink_0().text_color(p.muted).text_size(px(text::FOOTNOTE)).child(email.clone()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.from = Some(email.clone());
                        this.from_chosen = true;
                        this.picking_from = false;
                        this.focus_first(window, cx);
                        cx.notify();
                    }))
            });
            deferred(
                menu(p)
                    .id("from-menu")
                    .debug_selector(|| "from-menu".into())
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
        self.header_row(Some("From"), false, compact, p).child(picker).children(menu)
    }

    fn render_recipients(&self, field: Field, compact: bool, window: &Window, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let input = self.input(field);
        let suggestions = self
            .suggestions
            .as_ref()
            .filter(|suggestions| suggestions.field == field && input.focus_handle(cx).is_focused(window))
            .map(|suggestions| {
                let rows = suggestions.people.iter().enumerate().map(|(ix, person)| {
                    let address = Address::new(person.name.clone().filter(|name| !name.is_empty()), person.email.clone());
                    let picked = person.clone();
                    let photo = avatars::avatar(&address.email, &person.avatar, metrics::MENU_AVATAR, p, cx);
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
        let focus_target = input.focus_handle(cx);
        self.header_row(Some(field.label()), self.invalid.contains(&field), compact, p)
            .key_context(RECIPIENTS)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus_target.focus(window, cx);
                window.request_virtual_keyboard();
            })
            .child(div().flex_1().min_w_0().child(input::field(input)))
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
        let compact = f32::from(window.viewport_size().width) < crate::design::layout::COMPACT_WIDTH;
        let ts = design::type_scale(compact);
        let sending = matches!(self.status, Status::Sending);
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
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
            .capture_action(cx.listener(|this, _: &Paste, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &PastePlainText, window, cx| this.paste_plain_text(window, cx)))
            .group(COMPOSE_DROP)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach(paths.paths().to_vec(), cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(ts.body))
            .text_color(p.text)
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                    .child(self.render_from(compact, p, cx))
                    .child(self.render_recipients(Field::To, compact, window, p, cx))
                    .child(self.render_recipients(Field::Cc, compact, window, p, cx).when(!self.show_bcc, |row| {
                        row.child(button("show-bcc", "Bcc", ButtonKind::Plain(p.accent), ControlSize::small(compact), true, p).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.show_bcc = true;
                                window.focus(&this.bcc.focus_handle(cx), cx);
                                cx.notify();
                            },
                        )))
                    }))
                    .when(self.show_bcc, |d| d.child(self.render_recipients(Field::Bcc, compact, window, p, cx)))
                    .child(
                        self.header_row(None, false, compact, p)
                            .h(px(if compact { 50. } else { metrics::SUBJECT_HEIGHT }))
                            .text_size(px(ts.pane_title))
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_mouse_down(MouseButton::Left, {
                                let subject_focus = self.subject.focus_handle(cx);
                                move |_, window, cx| {
                                    subject_focus.focus(window, cx);
                                    window.request_virtual_keyboard();
                                }
                            })
                            .child(div().flex_1().min_w_0().child(input::field(&self.subject))),
                    )
                    .when_some(attached, |d, attached| {
                        let margin = if compact { design::compact::READER_PADDING } else { metrics::MARGIN };
                        d.child(
                            div()
                                .ml(px(margin))
                                .pr(px(margin))
                                .py(px(space::S8))
                                .border_b_1()
                                .border_color(p.line)
                                .child(attached),
                        )
                    }),
            )
            .child(
                div()
                    .key_context(BODY_CONTEXT)
                    .on_action(cx.listener(|this, _: &FormatBold, window, cx| {
                        this.body.update(cx, |input, cx| wrap_selection(input, "**", window, cx));
                    }))
                    .on_action(cx.listener(|this, _: &FormatItalic, window, cx| {
                        this.body.update(cx, |input, cx| wrap_selection(input, "*", window, cx));
                    }))
                    .on_action(cx.listener(|this, _: &FormatLink, window, cx| {
                        this.body.update(cx, |input, cx| insert_link(input, window, cx));
                    }))
                    .on_action(cx.listener(|this, _: &FormatStrike, window, cx| {
                        this.body.update(cx, |input, cx| wrap_selection(input, "~~", window, cx));
                    }))
                    .on_action(cx.listener(|this, _: &FormatCode, window, cx| {
                        this.body.update(cx, |input, cx| wrap_selection(input, "`", window, cx));
                    }))
                    .flex_1()
                    .min_h_0()
                    .pl(px(if compact { design::compact::READER_PADDING } else { metrics::MARGIN } - AREA_INSET))
                    .pr(px(if compact { design::compact::READER_PADDING } else { metrics::MARGIN } - AREA_INSET))
                    .py(px(metrics::BODY_PADDING))
                    .text_size(px(ts.reading))
                    .line_height(relative(editor::LEADING))
                    .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                    .child(input::editor(&self.body).full()),
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
    Moved { from: String, from_chosen: bool, from_resolved: bool, body: String, attachments: Vec<PathBuf> },
}

/// The reader's reply card: a mail reply (or reply all) to a conversation's latest message, or
/// the one the reader picked, with files picked, dropped or pasted on it.
pub struct ReplyBox {
    conversation_id: String,
    message: Option<String>,
    all: bool,
    recipients: SharedString,
    accounts: Vec<Account>,
    from: String,
    from_chosen: bool,
    from_resolved: bool,
    picking_from: bool,
    text: Entity<EditorState>,
    focus: FocusHandle,
    container_focus: FocusHandle,
    attachments: Attachments,
    status: Status,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<ReplyEvent> for ReplyBox {}

impl ReplyBox {
    /// `recipients_preview` is who the reply goes to, as the reader shows it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation_id: String,
        message: Option<String>,
        all: bool,
        recipients_preview: SharedString,
        accounts: Vec<Account>,
        from: String,
        from_resolved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .soft_wrap(true)
                .searchable(false)
                .auto_close(false)
                .smart_indent(false)
                .folding(false)
                .indent_guides(false)
                .placeholder(if all { "Reply to everyone in Markdown…" } else { "Reply in Markdown…" })
                .autocorrect(true);
            state.set_highlighter_factory(markup_highlighter_factory(MarkupFlavor::Markdown), cx);
            state.set_highlighter("markdown", cx);
            state
        });
        let focus = text.focus_handle(cx);
        window.focus(&focus, cx);
        let subscriptions = [
            cx.subscribe_in(&text, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && matches!(this.status, Status::Failed(_)) {
                    this.status = Status::Idle;
                    cx.notify();
                }
            }),
            // The highlighter takes its colors when it parses: made again in the new palette.
            cx.observe_window_appearance(window, |this, _, cx| {
                this.text.update(cx, |state, cx| state.set_highlighter("markdown", cx));
            }),
        ];
        Self {
            conversation_id,
            message,
            all,
            recipients: recipients_preview,
            accounts,
            from,
            from_chosen: false,
            from_resolved,
            picking_from: false,
            text,
            focus,
            container_focus: cx.focus_handle(),
            attachments: Attachments::default(),
            status: Status::Idle,
            _subscriptions: subscriptions,
        }
    }

    pub fn update_plan(
        &mut self,
        accounts: Vec<Account>,
        from: String,
        from_resolved: bool,
        recipients: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.accounts = accounts;
        self.recipients = recipients;
        if !self.from_chosen {
            self.from = from;
            self.from_resolved = from_resolved;
        }
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Sending) {
            return;
        }
        let body = self.text.read(cx).value().to_string();
        if body.trim().is_empty() && self.attachments.is_empty() {
            self.status = Status::Failed("Write something to send.".into());
            cx.notify();
            return;
        }
        let (id, all) = (self.conversation_id.clone(), self.all);
        let from = (self.from_chosen || self.from_resolved).then(|| self.from.clone());
        let overrides = (from.is_some() || !self.attachments.is_empty() || self.message.is_some()).then(|| {
            mailstream::ReplyOverrides {
                from,
                to: None,
                cc: None,
                bcc: None,
                subject: None,
                attachments: (!self.attachments.is_empty()).then(|| self.attachments.paths()),
                message: self.message.as_ref().map(ToString::to_string),
            }
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

    /// Escape (here or in the text, as the kit's `Escape`) leaves an empty reply; written text and
    /// files stay until Send or Discard.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.picking_from {
            self.picking_from = false;
            cx.notify();
            return;
        }
        if self.text.read(cx).value().trim().is_empty()
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
        let body = self.text.read(cx).value().to_string();
        cx.emit(ReplyEvent::Moved {
            from: self.from.clone(),
            from_chosen: self.from_chosen,
            from_resolved: self.from_resolved,
            body,
            attachments: self.attachments.take(),
        });
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

    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pasted) = attachments::pasted(cx) {
            cx.stop_propagation();
            match pasted {
                Ok(paths) => self.attach(paths, cx),
                Err(message) => {
                    self.status = Status::Failed(message.into());
                    cx.notify();
                }
            }
            return;
        }
        if !matches!(self.status, Status::Sending)
            && self.text.focus_handle(cx).is_focused(window)
            && paste_rich_into_editor(&self.text, window, cx)
        {
            cx.stop_propagation();
        }
    }

    fn paste_plain_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.status, Status::Sending)
            && self.text.focus_handle(cx).is_focused(window)
            && paste_plain_into_editor(&self.text, window, cx)
        {
            cx.stop_propagation();
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
        let attached = self.attachments.render("reply-attachment", !sending, &Self::remover(cx), p);
        let options = from_options(&self.accounts);
        let selected = selected_from_option(&options, Some(&self.from));
        let pickable = !sending && options.len() > 1;
        let from_picker = (!self.from.is_empty()).then(|| {
            div()
                .id("reply-from")
                .debug_selector(|| "reply-from".into())
                .flex_shrink_0()
                .max_w(px(240.))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(radius::SMALL))
                .flex()
                .items_center()
                .gap(px(5.))
                .when_some(selected, |d, option| {
                    d.child(div().flex_shrink_0().size(px(6.)).rounded_full().bg(design::account_color(option.account_index)))
                })
                .child(div().min_w_0().truncate().text_color(p.muted).child(self.from.clone()))
                .when(pickable, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .child(div().flex_shrink_0().text_color(p.faint).child("▾"))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.picking_from = !this.picking_from;
                            cx.notify();
                        }))
                })
        });
        let from_menu = self.picking_from.then(|| {
            let rows = options.into_iter().map(|option| {
                let email = option.email.clone();
                let is_selected = self.from.eq_ignore_ascii_case(&email);
                let title: SharedString = match option.name.as_deref() {
                    Some(name) if !name.eq_ignore_ascii_case(&email) => name.to_owned().into(),
                    _ if !option.account_title.eq_ignore_ascii_case(&email) => option.account_title.clone(),
                    _ => email.clone().into(),
                };
                let has_subtitle = !title.eq_ignore_ascii_case(&email);
                let row_sel = format!("reply-from:{email}");
                div()
                    .id(ElementId::Name(row_sel.clone().into()))
                    .debug_selector(move || row_sel.clone())
                    .h(px(metrics::MENU_ROW))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(radius::SMALL))
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(p.selection))
                    .hover(|s| s.bg(p.hover))
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(design::account_color(option.account_index)))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(has_subtitle, |d| {
                        d.child(div().flex_shrink_0().text_color(p.muted).text_size(px(text::FOOTNOTE)).child(email.clone()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.from = email.clone();
                        this.from_chosen = true;
                        this.picking_from = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }))
            });
            deferred(
                menu(p)
                    .id("reply-from-menu")
                    .debug_selector(|| "reply-from-menu".into())
                    .top(px(metrics::ROW_HEIGHT))
                    .right(px(metrics::REPLY_PADDING))
                    .children(rows)
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.picking_from = false;
                        cx.notify();
                    })),
            )
            .with_priority(1)
        });
        div()
            .key_context(REPLY_CONTEXT)
            .track_focus(&self.container_focus)
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
            .on_action(cx.listener(|this, _: &Escape, _, cx| this.cancel(cx)))
            .on_action(cx.listener(|this, _: &Send, window, cx| this.send(window, cx)))
            .on_action(cx.listener(Self::pick_files))
            .on_action(cx.listener(Self::move_to_column))
            .on_action(cx.listener(|this, _: &FormatBold, window, cx| {
                this.text.update(cx, |input, cx| wrap_selection(input, "**", window, cx));
            }))
            .on_action(cx.listener(|this, _: &FormatItalic, window, cx| {
                this.text.update(cx, |input, cx| wrap_selection(input, "*", window, cx));
            }))
            .on_action(cx.listener(|this, _: &FormatLink, window, cx| {
                this.text.update(cx, |input, cx| insert_link(input, window, cx));
            }))
            .on_action(cx.listener(|this, _: &FormatStrike, window, cx| {
                this.text.update(cx, |input, cx| wrap_selection(input, "~~", window, cx));
            }))
            .on_action(cx.listener(|this, _: &FormatCode, window, cx| {
                this.text.update(cx, |input, cx| wrap_selection(input, "`", window, cx));
            }))
            .capture_action(cx.listener(|this, _: &Paste, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &PastePlainText, window, cx| this.paste_plain_text(window, cx)))
            .group(REPLY_DROP)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach(paths.paths().to_vec(), cx)))
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(radius::CARD))
            .border_1()
            .border_color(if self.focus.is_focused(window) || self.container_focus.contains_focused(window, cx) { ring } else { p.line })
            .bg(p.background)
            .shadow(p.popover_shadow.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .pt(px(8.))
                    .px(px(metrics::REPLY_PADDING))
                    .text_size(px(design::type_scale(f32::from(window.viewport_size().width) < crate::design::layout::COMPACT_WIDTH).footnote))
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
                    .child(div().flex_1().min_w_0().truncate().child(self.recipients.clone()))
                    .children(from_picker),
            )
            .children(from_menu)
            .child(
                div()
                    .debug_selector(|| "reply-editor-frame".into())
                    .pl(px(metrics::REPLY_PADDING - AREA_INSET))
                    .pr(px(metrics::REPLY_PADDING - AREA_INSET))
                    .py(px(8.))
                    .text_size(px(design::type_scale(f32::from(window.viewport_size().width) < crate::design::layout::COMPACT_WIDTH).reading))
                    .line_height(relative(editor::LEADING))
                    .text_color(p.text)
                    .when(sending, |d| d.opacity(metrics::SENT_TEXT_OPACITY))
                    .child(input::editor(&self.text).auto_grow(metrics::REPLY_MIN_ROWS, metrics::REPLY_MAX_ROWS)),
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
                        icon_button("reply-attach", Icon::Attachment, ATTACH_TIP, !sending, p.glyph).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.pick_files(&Attach, window, cx)),
                        ),
                    )
                    .child(
                        icon_button("reply-move", Icon::Dock, MOVE_TIP, !sending, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.move_to_column(&MoveToColumn, window, cx)
                            })),
                    )
                    .child(
                        button("reply-discard", "Discard", ButtonKind::Secondary, ControlSize::regular(f32::from(window.viewport_size().width) < crate::design::layout::COMPACT_WIDTH), !sending, p)
                            .when(!sending, |b| {
                                b.on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ReplyEvent::Discarded)))
                            }),
                    )
                    .child({
                        let compact = f32::from(window.viewport_size().width) < crate::design::layout::COMPACT_WIDTH;
                        let show_shortcut = !cfg!(target_os = "android") && !compact;
                        send_button("reply-send", sending, show_shortcut, compact, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
                    }),
            )
            .child(attachments::drop_target(REPLY_DROP, radius::CARD, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::ops::Range;

    use gpui::component::input::{FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope};
    use gpui::{HighlightStyle, TestAppContext, WindowAppearance};

    use crate::markup_highlight::MarkupHighlighter;

    struct EditorHarness {
        editor: Entity<EditorState>,
        focus: FocusHandle,
    }

    impl Render for EditorHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .key_context(BODY_CONTEXT)
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &FormatBold, window, cx| {
                    this.editor.update(cx, |input, cx| wrap_selection(input, "**", window, cx));
                }))
                .on_action(cx.listener(|this, _: &FormatItalic, window, cx| {
                    this.editor.update(cx, |input, cx| wrap_selection(input, "*", window, cx));
                }))
                .on_action(cx.listener(|this, _: &FormatLink, window, cx| {
                    this.editor.update(cx, |input, cx| insert_link(input, window, cx));
                }))
                .on_action(cx.listener(|this, _: &FormatStrike, window, cx| {
                    this.editor.update(cx, |input, cx| wrap_selection(input, "~~", window, cx));
                }))
                .on_action(cx.listener(|this, _: &FormatCode, window, cx| {
                    this.editor.update(cx, |input, cx| wrap_selection(input, "`", window, cx));
                }))
                .child(input::editor(&self.editor).full())
        }
    }

    #[gpui::test]
    fn markdown_formatting_shortcuts_wrap_and_insert_links(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| {
                let mut state = EditorState::new(window, cx).line_number(false).soft_wrap(true);
                state.set_highlighter_factory(markup_highlighter_factory(MarkupFlavor::Markdown), cx);
                state.set_highlighter("markdown", cx);
                state
            });
            let focus = editor.focus_handle(cx);
            EditorHarness { editor, focus }
        });
        let editor = window.read_with(cx, |view, _| view.editor.clone()).unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            let focus = editor.focus_handle(cx);
            window.focus(&focus, cx);
            editor.update(cx, |input, cx| {
                input.set_value("hello world", window, cx);
                input.set_selected_range(0..5, cx);
            });
        })
        .unwrap();

        cx.simulate_keystrokes(window.into(), secondary!("b"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "**hello** world");
        assert_eq!(editor.read_with(cx, |input, _| input.selected_range()), 2..7);

        cx.simulate_keystrokes(window.into(), secondary!("i"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "***hello*** world");

        // Pressing Cmd+I again unwraps the inner italic marker.
        cx.simulate_keystrokes(window.into(), secondary!("i"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "**hello** world");

        // Cmd+K wraps selection into [hello](url) and selects `url`.
        cx.simulate_keystrokes(window.into(), secondary!("k"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "**[hello](url)** world");
        assert_eq!(editor.read_with(cx, |input, _| input.selected_range()), 10..13);

        // Cmd+Shift+X and Cmd+E on empty selection.
        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |input, cx| input.set_value("", window, cx));
        })
        .unwrap();
        cx.simulate_keystrokes(window.into(), secondary!("shift-x"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "~~~~");
        assert_eq!(editor.read_with(cx, |input, _| input.selected_range()), 2..2);

        cx.simulate_keystrokes(window.into(), secondary!("shift-x"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "");

        cx.simulate_keystrokes(window.into(), secondary!("e"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |input, _| input.value().to_string()), "``");
        assert_eq!(editor.read_with(cx, |input, _| input.selected_range()), 1..1);
    }

    /// The real highlighter, noting the window's appearance at each parse.
    struct AppearanceProbe {
        inner: MarkupHighlighter,
        parsed_in: Rc<RefCell<Vec<WindowAppearance>>>,
    }

    impl InputHighlighter for AppearanceProbe {
        fn language(&self) -> SharedString {
            self.inner.language()
        }

        fn update(&mut self, edit: Option<InputEdit>, text: &Rope, folding: bool, window: &mut Window, cx: &mut Context<EditorState>) {
            self.parsed_in.borrow_mut().push(window.appearance());
            self.inner.update(edit, text, folding, window, cx);
        }

        fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
            self.inner.styles(range, resolver)
        }

        fn fold_ranges(&self, text: &Rope) -> Vec<FoldRange> {
            self.inner.fold_ranges(text)
        }
    }

    /// The reply's Markdown colors follow the system into dark without another keystroke (MS-347).
    #[gpui::test]
    fn reply_highlighting_follows_the_appearance(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
        });
        let (reply, cx) = cx.add_window_view(|window, cx| {
            ReplyBox::new("conversation".into(), None, false, "someone@example.com".into(), Vec::new(), "me@example.com".into(), false, window, cx)
        });
        let parsed_in = Rc::new(RefCell::new(Vec::new()));
        let probe = parsed_in.clone();
        reply.update_in(cx, |reply, window, cx| {
            reply.text.update(cx, |state, cx| {
                state.set_highlighter_factory(
                    Rc::new(move |_| {
                        Some(Box::new(AppearanceProbe { inner: MarkupHighlighter::new(MarkupFlavor::Markdown), parsed_in: probe.clone() }))
                    }),
                    cx,
                );
                state.set_highlighter("markdown", cx);
                state.set_value("**bold** and https://example.com", window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(parsed_in.borrow().last(), Some(&WindowAppearance::Light));

        cx.simulate_appearance_change(WindowAppearance::Dark);
        cx.run_until_parked();
        assert_eq!(parsed_in.borrow().last(), Some(&WindowAppearance::Dark));
    }

    /// A single long paragraph without `\n` wraps and grows the reply box up to `REPLY_MAX_ROWS`.
    #[gpui::test]
    fn reply_grows_on_wrapped_paragraph_and_caps_at_max_rows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
        });
        let (reply, cx) = cx.add_window_view(|window, cx| {
            ReplyBox::new("conversation".into(), None, false, "someone@example.com".into(), Vec::new(), "me@example.com".into(), false, window, cx)
        });
        cx.run_until_parked();
        let text = reply.read_with(cx, |r, _| r.text.clone());
        let empty_height = cx.debug_bounds("reply-editor-frame").unwrap().size.height;

        // ~6-8 wrapped lines in the default test window width, with zero newline characters.
        let medium = "word ".repeat(140);
        assert!(!medium.contains('\n'));
        text.update_in(cx, |state, window, cx| state.set_value(&medium, window, cx));
        cx.run_until_parked();
        let wrapped_rows = text.read_with(cx, |state, _| state.display_map().wrap_row_count());
        assert!(
            wrapped_rows > metrics::REPLY_MIN_ROWS && wrapped_rows < metrics::REPLY_MAX_ROWS,
            "expected {wrapped_rows} wrapped rows between {} and {}",
            metrics::REPLY_MIN_ROWS,
            metrics::REPLY_MAX_ROWS,
        );
        let medium_height = cx.debug_bounds("reply-editor-frame").unwrap().size.height;
        assert!(
            medium_height > empty_height,
            "reply should grow on wrapped text: {medium_height:?} vs {empty_height:?}",
        );

        // Exceeding REPLY_MAX_ROWS caps the height and does not grow further.
        let long = "word ".repeat(1000);
        assert!(!long.contains('\n'));
        text.update_in(cx, |state, window, cx| state.set_value(&long, window, cx));
        cx.run_until_parked();
        let max_rows = text.read_with(cx, |state, _| state.display_map().wrap_row_count());
        assert!(max_rows > metrics::REPLY_MAX_ROWS, "expected > {} wrapped rows, got {max_rows}", metrics::REPLY_MAX_ROWS);
        let capped_height = cx.debug_bounds("reply-editor-frame").unwrap().size.height;
        assert!(capped_height > medium_height, "reply should keep growing up to max rows");

        let longer = "word ".repeat(1500);
        text.update_in(cx, |state, window, cx| state.set_value(&longer, window, cx));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("reply-editor-frame").unwrap().size.height, capped_height);
    }

    fn test_accounts() -> Vec<Account> {
        vec![
            Account {
                id: "me@gmail.com".into(),
                label: "Personal".into(),
                provider: mailstream::Provider::Google,
                name: Some("Me".into()),
                services: vec!["mail".into()],
                connectable: vec![],
                identities: vec![
                    Address::new(Some("Me".into()), "me@gmail.com"),
                    Address::new(Some("Team".into()), "team@custom.org"),
                ],
            },
            Account {
                id: "work@outlook.com".into(),
                label: "Work".into(),
                provider: mailstream::Provider::Microsoft,
                name: Some("Work Me".into()),
                services: vec!["mail".into()],
                connectable: vec![],
                identities: vec![Address::new(Some("Work Me".into()), "work@outlook.com")],
            },
        ]
    }

    /// ReplyBox displays the resolved From address, lets the user pick another account or alias,
    /// closes its From menu on Escape before discarding, and keeps manual choices across updates.
    #[gpui::test]
    fn reply_box_shows_and_switches_from_address(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let accounts = test_accounts();
        let (reply, cx) = cx.add_window_view(|window, cx| {
            ReplyBox::new(
                "conv".into(),
                None,
                false,
                "Bob".into(),
                accounts.clone(),
                "me@gmail.com".into(),
                false,
                window,
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reply-from").is_some());
        assert!(cx.debug_bounds("reply-from-menu").is_none());

        // Thread headers arrive with a more specific alias -> updates From while unchosen.
        reply.update(cx, |r, cx| {
            r.update_plan(accounts.clone(), "team@custom.org".into(), true, "Bob".into(), cx);
        });
        cx.run_until_parked();
        assert_eq!(reply.read_with(cx, |r, _| r.from.clone()), "team@custom.org");

        // Open the From menu and pick the second account.
        let bounds = cx.debug_bounds("reply-from").unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reply-from-menu").is_some());

        // Escape closes the menu without discarding.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reply-from-menu").is_none());

        // Re-open and select work@outlook.com.
        let bounds = cx.debug_bounds("reply-from").unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        let item = cx.debug_bounds("reply-from:work@outlook.com").unwrap();
        cx.simulate_mouse_down(item.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(item.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(reply.read_with(cx, |r, _| (r.from.clone(), r.from_chosen)), ("work@outlook.com".into(), true));
        assert!(cx.debug_bounds("reply-from-menu").is_none());

        // Subsequent plan refreshes preserve the user's explicit choice.
        reply.update(cx, |r, cx| {
            r.update_plan(accounts, "me@gmail.com".into(), true, "Bob".into(), cx);
        });
        assert_eq!(reply.read_with(cx, |r, _| r.from.clone()), "work@outlook.com");
    }

    /// Moving a reply to Compose keeps From unlocked and lets the user switch between accounts
    /// and send-as aliases.
    #[gpui::test]
    fn moved_reply_in_compose_unlocks_and_switches_from(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let accounts = test_accounts();
        let moved = MovedReply {
            conversation: "conv".into(),
            message: None,
            all: false,
            from: "team@custom.org".into(),
            from_chosen: false,
            from_resolved: true,
            to: vec![Address::new(Some("Bob".into()), "bob@x.com")],
            cc: vec![],
            subject: "Re: Hi".into(),
            body: "Hello".into(),
            attachments: vec![],
        };
        let (compose, cx) = cx.add_window_view(|window, cx| Compose::new(ComposeMode::Reply(moved), accounts, window, cx));
        cx.run_until_parked();
        assert_eq!(compose.read_with(cx, |c, _| (c.from.clone(), c.from_locked)), (Some("team@custom.org".into()), false));

        let bounds = cx.debug_bounds("from").unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        let item = cx.debug_bounds("from:work@outlook.com").unwrap();
        cx.simulate_mouse_down(item.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(item.center(), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(compose.read_with(cx, |c, _| (c.from.clone(), c.from_chosen)), (Some("work@outlook.com".into()), true));
    }

    #[gpui::test]
    fn pasting_rich_html_into_body_and_reply_converts_to_markdown(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::component::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
            bind_keys(cx);
        });
        let (compose, cx) = cx.add_window_view(|window, cx| {
            Compose::new(ComposeMode::New { from: Some("me@example.com".into()) }, vec![], window, cx)
        });
        cx.run_until_parked();

        let rich = gpui::ClipboardItem::new_string_with_html(
            "Hello Alice\n\n• One\n• Two".to_string(),
            r#"<p>Hello <strong>Alice</strong>, see <a href="https://example.com">link</a></p><ul><li>One</li><li>Two</li></ul>"#.to_string(),
        );
        cx.update(|_, cx| cx.write_to_clipboard(rich));

        // Body focused: Cmd/Ctrl+V converts HTML to Markdown.
        let body = compose.read_with(cx, |c, _| c.body.clone());
        body.update_in(cx, |input, window, cx| input.focus(window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes(secondary!("v"));
        cx.run_until_parked();
        assert_eq!(
            body.read_with(cx, |input, _| input.value().to_string()),
            "Hello **Alice**, see [link](https://example.com)\n\n- One\n- Two"
        );

        // Undo reverts the paste atomically.
        cx.simulate_keystrokes(secondary!("z"));
        cx.run_until_parked();
        assert_eq!(body.read_with(cx, |input, _| input.value().to_string()), "");

        // PastePlainText inserts plain text without Markdown syntax.
        cx.simulate_keystrokes(PASTE_PLAIN_KEY);
        cx.run_until_parked();
        assert_eq!(
            body.read_with(cx, |input, _| input.value().to_string()),
            "Hello Alice\n\n• One\n• Two"
        );

        // Subject focused: keeps plain text (no Markdown conversion).
        let subject = compose.read_with(cx, |c, _| c.subject.clone());
        subject.update_in(cx, |input, window, cx| input.focus(window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes(secondary!("v"));
        cx.run_until_parked();
        assert_eq!(subject.read_with(cx, |input, _| input.value().to_string()), "Hello Alice• One• Two");

        // ReplyBox also converts clipboard HTML to Markdown.
        let (reply, cx) = cx.add_window_view(|window, cx| {
            ReplyBox::new("conv".into(), None, false, "alice@example.com".into(), vec![], "me@example.com".into(), false, window, cx)
        });
        cx.run_until_parked();
        let reply_text = reply.read_with(cx, |r, _| r.text.clone());
        reply_text.update_in(cx, |input, window, cx| input.focus(window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes(secondary!("v"));
        cx.run_until_parked();
        assert_eq!(
            reply_text.read_with(cx, |input, _| input.value().to_string()),
            "Hello **Alice**, see [link](https://example.com)\n\n- One\n- Two"
        );
    }

    /// Mirrors `MailApp`'s `Mail` root + `MailBrowse` child (holding `ReplyBox`) + sibling
    /// `ComposeColumn` layout to verify keyboard and menu-bar shortcut isolation (MS-412).
    struct ShortcutIsolationHost {
        browse_focus: FocusHandle,
        reply: Entity<ReplyBox>,
        column: Entity<ComposeColumn>,
        fired: Vec<&'static str>,
    }

    impl Render for ShortcutIsolationHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use crate::state;
            div()
                .key_context(state::CONTEXT)
                .on_action(cx.listener(|this, _: &state::Close, window, _| {
                    if !window.context_stack().iter().any(|c| {
                        c.contains("Input") || c.contains("Compose") || c.contains("ReplyBox")
                    }) {
                        this.fired.push("close");
                    }
                }))
                .on_action(cx.listener(|this, _: &state::FocusSearch, window, _| {
                    if !window.context_stack().iter().any(|c| c.contains("Compose") || c.contains("ReplyBox")) {
                        this.fired.push("focus_search");
                    }
                }))
                .size_full()
                .flex()
                .child(
                    div()
                        .key_context(state::BROWSE_CONTEXT)
                        .track_focus(&self.browse_focus)
                        .on_action(cx.listener(|this, _: &state::Trash, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("trash");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Archive, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("archive");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Reply, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("reply");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::ReplyAll, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("reply_all");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Forward, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("forward");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Star, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("star");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::ToggleRead, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("toggle_read");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::ToggleWaiting, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("toggle_waiting");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::EditTags, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("edit_tags");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Next, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("next");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Previous, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("previous");
                            }
                        }))
                        .on_action(cx.listener(|this, _: &state::Back, window, _| {
                            if !state::in_text_or_reply_context(window) {
                                this.fired.push("back");
                            }
                        }))
                        .flex_1()
                        .h_full()
                        .child(self.reply.clone()),
                )
                .child(div().w(px(360.)).h_full().child(self.column.clone()))
        }
    }

    fn setup_shortcut_host(cx: &mut TestAppContext) -> (Entity<ShortcutIsolationHost>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui::init(cx);
            design::sync_kit_theme(WindowAppearance::Light, cx);
            bind_keys(cx);
            crate::state::bind_keys(cx);
        });
        cx.add_window_view(|window, cx| {
            let reply = cx.new(|cx| {
                ReplyBox::new(
                    "conv".into(),
                    None,
                    false,
                    "alice@example.com".into(),
                    vec![],
                    "me@example.com".into(),
                    true,
                    window,
                    cx,
                )
            });
            let column = cx.new(ComposeColumn::new);
            ShortcutIsolationHost { browse_focus: cx.focus_handle(), reply, column, fired: Vec::new() }
        })
    }

    #[gpui::test]
    fn reply_box_absorbs_extra_backspaces_and_blocks_mail_shortcuts(cx: &mut TestAppContext) {
        let (host, cx) = setup_shortcut_host(cx);
        cx.run_until_parked();

        let (reply, reply_text, reply_container) = host.read_with(cx, |h, app| {
            let r = h.reply.read(app);
            (h.reply.clone(), r.text.clone(), r.container_focus.clone())
        });
        reply_text.update_in(cx, |input, window, cx| input.focus(window, cx));
        cx.run_until_parked();

        // Typing letters bound to mail shortcuts and hitting Backspace repeatedly past offset 0
        // must never trigger Trash or any other mail-browse action, and Backspace at offset 0
        // must be consumed (propagate == false) so macOS NSMenu never sees it.
        cx.simulate_input("ab");
        cx.run_until_parked();
        for _ in 0..5 {
            let result = cx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke::parse("backspace").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    }),
                    cx,
                )
            });
            assert!(!result.propagate, "backspace at offset 0 must not propagate out of ReplyBox");
        }
        cx.run_until_parked();
        assert_eq!(reply_text.read_with(cx, |i, _| i.value().to_string()), "");
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()), "no mail action fired on backspace");

        // Single-key and Cmd-modified mail shortcuts while typing in ReplyBox editor.
        cx.simulate_input("erasudlrfjk/");
        for key in [
            secondary!("backspace"),
            "ctrl-cmd-a",
            secondary!("r"),
            secondary!("shift-r"),
            secondary!("shift-f"),
            secondary!("shift-l"),
            secondary!("shift-u"),
            secondary!("shift-d"),
            secondary!("["),
            secondary!("w"),
            secondary!("f"),
        ] {
            cx.simulate_keystrokes(key);
        }
        cx.run_until_parked();
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()), "no mail action fired in reply editor");

        // Even when focus sits on ReplyBox's container (outside the text editor), mail shortcuts
        // and menu-bar Trash dispatch are blocked.
        cx.update(|window, cx| window.focus(&reply_container, cx));
        cx.run_until_parked();
        for key in ["e", "r", "a", "f", "s", "u", "d", "l", "j", "k", "/", "backspace", secondary!("backspace"), secondary!("["), secondary!("w")] {
            cx.simulate_keystrokes(key);
        }
        cx.update(|window, cx| window.dispatch_action(Box::new(crate::state::Trash), cx));
        cx.run_until_parked();
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()), "no mail action fired on ReplyBox container");
        drop(reply);
    }

    #[gpui::test]
    fn compose_column_isolates_drafts_from_mail_browse_and_menu_actions(cx: &mut TestAppContext) {
        let (host, cx) = setup_shortcut_host(cx);
        let column = host.read_with(cx, |h, _| h.column.clone());
        column.update_in(cx, |col, window, cx| {
            col.open(ComposeMode::New { from: Some("me@example.com".into()) }, vec![], window, cx);
        });
        cx.run_until_parked();

        // Because ComposeColumn is a sibling of MailBrowse, macOS NSMenu validation reports
        // mail-conversation actions as unavailable while composing.
        let available = cx.update(|window, cx| {
            (
                window.is_action_available(&crate::state::Trash, cx),
                window.is_action_available(&crate::state::Archive, cx),
                window.is_action_available(&crate::state::Reply, cx),
            )
        });
        assert_eq!(available, (false, false, false));

        // Extra Backspaces in To field and modifier shortcuts never fire mail actions.
        for _ in 0..3 {
            let result = cx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke::parse("backspace").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    }),
                    cx,
                )
            });
            assert!(!result.propagate, "backspace at offset 0 must not propagate out of Compose field");
        }
        for key in [
            secondary!("backspace"),
            "ctrl-cmd-a",
            secondary!("r"),
            secondary!("shift-r"),
            secondary!("shift-f"),
            secondary!("["),
            secondary!("f"),
        ] {
            cx.simulate_keystrokes(key);
        }
        cx.run_until_parked();
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()));

        // Focus on ComposeColumn chrome (outside inputs) still blocks all mail shortcuts.
        let col_focus = column.read_with(cx, |col, _| col.focus.clone());
        cx.update(|window, cx| window.focus(&col_focus, cx));
        cx.run_until_parked();
        for key in ["e", "r", "a", "f", "s", "u", "d", "l", "j", "k", "/", "backspace", secondary!("backspace"), secondary!("[")] {
            cx.simulate_keystrokes(key);
        }
        cx.run_until_parked();
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()));
    }

    #[gpui::test]
    fn mail_browse_shortcuts_work_and_trash_requires_secondary_backspace(cx: &mut TestAppContext) {
        let (host, cx) = setup_shortcut_host(cx);
        let browse = host.read_with(cx, |h, _| h.browse_focus.clone());
        cx.update(|window, cx| window.focus(&browse, cx));
        cx.run_until_parked();

        assert!(cx.update(|window, cx| window.is_action_available(&crate::state::Trash, cx)));

        // Bare backspace and delete no longer trash mail.
        cx.simulate_keystrokes("backspace");
        cx.simulate_keystrokes("delete");
        cx.run_until_parked();
        assert!(host.read_with(cx, |h, _| h.fired.is_empty()), "bare backspace/delete must not trash mail");

        // Secondary+Backspace trashes, and normal browse shortcuts still work.
        cx.simulate_keystrokes(secondary!("backspace"));
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("j");
        cx.simulate_keystrokes("k");
        cx.simulate_keystrokes("r");
        cx.simulate_keystrokes("s");
        cx.run_until_parked();
        assert_eq!(
            host.read_with(cx, |h, _| h.fired.clone()),
            vec!["trash", "archive", "next", "previous", "reply", "star"]
        );
    }
}
