//! New, changed and deleted events (Flutter's `calendar_editor.dart`, MS-215). The editor is a
//! gpui-kit dialog over the window: the title, the calendar (a new event's), all day, when it
//! starts and ends, where, the guests (completed from contacts) and the description. Saving sends
//! everything of a new event and only what changed of an existing one, through the engine's
//! `calendar_create` / `calendar_update`; the calendar emails the guests. Deleting asks first,
//! saying who hears of it.

use std::time::Duration as Wait;

use chrono::{Duration, Local, NaiveDateTime, NaiveTime, Timelike};
use gpui::component::WindowExt;
use gpui::component::button::ButtonVariant;
use gpui::component::checkbox::Checkbox;
use gpui::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use gpui::component::time_field::{HourCycle, TimeField, TimeFieldEvent, TimeFieldState};
use gpui::component::{Disableable, IndexPath};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, WeakEntity, Window, actions, div, px,
};
use mailstream::{CalendarAccount, CalendarDraft, CalendarItem, Provider};

use super::CalendarPane;
use super::model::EventKey;
use crate::controls::{ButtonKind, ControlSize, button, mark_dot, pill};
use crate::design::{self, Palette, space, text};
use crate::engine::engine;

actions!(calendar_editor, [Save]);

const CONTEXT: &str = "EventEditor";
/// The editor's width, as Flutter's sheet.
const WIDTH: f32 = 540.;
/// The labels' column.
const LABEL: f32 = 76.;
/// People offered as guests while an address is typed.
const SUGGESTIONS: u32 = 6;
/// How long typing pauses before the contacts are looked up.
const SEARCH_DELAY: Wait = Wait::from_millis(150);

pub fn bind_keys(cx: &mut App) {
    // After the kit's own, so ⌘↩ inside a field saves rather than reaching the field.
    cx.bind_keys([
        KeyBinding::new(secondary!("enter"), Save, Some(CONTEXT)),
        KeyBinding::new(secondary!("enter"), Save, Some("EventEditor > Input")),
    ]);
}

/// One calendar a new event can go on.
#[derive(Clone)]
struct Choice {
    index: usize,
    account: String,
    calendar: String,
    label: SharedString,
    color: Hsla,
    provider: Provider,
}

impl SelectItem for Choice {
    type Value = usize;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(choice_label(self).into_any_element())
    }

    fn value(&self) -> &usize {
        &self.index
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        choice_label(self)
    }
}

fn choice_label(choice: &Choice) -> impl IntoElement {
    div().flex().items_center().gap(px(space::S6)).child(mark_dot(choice.color)).child(choice.label.clone())
}

/// What the form holds, to work out what to send.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Form {
    pub title: String,
    pub location: String,
    pub notes: String,
    pub all_day: bool,
    /// When it starts and ends; all day, their days are the first and the last.
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub guests: Vec<String>,
}

/// The start, end and all-day as the engine takes them.
pub(super) fn when(all_day: bool, start: NaiveDateTime, end: NaiveDateTime) -> [String; 3] {
    if all_day {
        [start.format("%Y-%m-%d").to_string(), end.format("%Y-%m-%d").to_string(), "all-day".into()]
    } else {
        [start.format("%Y-%m-%dT%H:%M").to_string(), end.format("%Y-%m-%dT%H:%M").to_string(), "timed".into()]
    }
}

fn normalized(emails: &[String]) -> Vec<String> {
    let mut emails: Vec<String> = emails.iter().map(|email| email.to_lowercase()).collect();
    emails.sort();
    emails
}

/// What to send: everything for a new event; for an existing one (`event`, which had `initial`
/// as its form), what changed.
pub(super) fn draft(form: &Form, event: Option<(&CalendarItem, &Form)>) -> CalendarDraft {
    let title = form.title.trim().to_owned();
    let location = form.location.trim().to_owned();
    let notes = form.notes.trim_end().to_owned();
    let [start, end, _] = when(form.all_day, form.start, form.end);
    let Some((event, initial)) = event else {
        return CalendarDraft {
            title: Some(title),
            start: Some(start),
            end: Some(end),
            all_day: form.all_day,
            location: (!location.is_empty()).then_some(location),
            notes: (!notes.is_empty()).then_some(notes),
            attendees: (!form.guests.is_empty()).then(|| form.guests.clone()),
        };
    };
    let time_changed = when(form.all_day, form.start, form.end) != when(initial.all_day, initial.start, initial.end);
    let guests_changed = normalized(&form.guests) != normalized(&initial.guests);
    CalendarDraft {
        title: (title != event.title).then_some(title),
        start: time_changed.then_some(start),
        end: time_changed.then_some(end),
        all_day: time_changed && form.all_day,
        location: (location != event.location.as_deref().unwrap_or("")).then_some(location),
        notes: (!event.notes_cut && notes != event.notes.as_deref().unwrap_or("")).then_some(notes),
        attendees: guests_changed.then(|| form.guests.clone()),
    }
}

pub(super) fn is_empty(draft: &CalendarDraft) -> bool {
    draft.title.is_none()
        && draft.start.is_none()
        && draft.location.is_none()
        && draft.notes.is_none()
        && draft.attendees.is_none()
}

/// Why the form can't be saved as it is.
pub(super) fn problem(form: &Form) -> Option<&'static str> {
    if form.title.trim().is_empty() {
        Some("Give the event a title.")
    } else if if form.all_day { form.end.date() < form.start.date() } else { form.end <= form.start } {
        Some("The event ends before it starts.")
    } else {
        None
    }
}

/// An address as a guest takes it: `name@host.domain`, nothing that separates addresses.
pub(super) fn is_email(text: &str) -> bool {
    if text.contains(|c: char| c.is_whitespace() || ",;<>".contains(c)) {
        return false;
    }
    let mut parts = text.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    let mut labels = domain.rsplitn(2, '.');
    !local.is_empty() && matches!((labels.next(), labels.next()), (Some(top), Some(host)) if !top.is_empty() && !host.is_empty())
}

/// A new event's start: the next full hour.
fn next_hour() -> NaiveDateTime {
    let now = Local::now().naive_local();
    now.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0)).unwrap_or(now) + Duration::hours(1)
}

/// Which of the four pickers changed.
#[derive(Clone, Copy)]
enum Moment {
    StartDate,
    StartTime,
    EndDate,
    EndTime,
}

pub(super) struct EventEditor {
    pane: WeakEntity<CalendarPane>,
    /// The event changed; none for a new one.
    event: Option<CalendarItem>,
    /// What the event had, to send only what changed.
    initial: Option<Form>,
    /// `google` or `microsoft`, for what the notes say.
    provider: Option<Provider>,
    time_zone: String,
    focus: FocusHandle,
    title: Entity<InputState>,
    choices: Vec<Choice>,
    calendar: Option<Entity<SelectState<Vec<Choice>>>>,
    chosen: Option<usize>,
    all_day: bool,
    start: NaiveDateTime,
    end: NaiveDateTime,
    start_date: Entity<DatePickerState>,
    start_time: Entity<TimeFieldState>,
    end_date: Entity<DatePickerState>,
    end_time: Entity<TimeFieldState>,
    location: Entity<InputState>,
    guests: Vec<String>,
    guest_input: Entity<InputState>,
    /// Contacts matching what is typed in the guests' field.
    suggestions: Vec<mailstream::Person>,
    generation: u64,
    notes: Entity<TextareaState>,
    saving: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for EventEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEditor {
    fn new(
        pane: WeakEntity<CalendarPane>,
        accounts: Vec<CalendarAccount>,
        time_zone: String,
        event: Option<CalendarItem>,
        span: Option<(NaiveDateTime, NaiveDateTime)>,
        start: Option<NaiveDateTime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (all_day, start, end, guests) = match (&event, span) {
            (Some(event), Some((first, after))) if event.all_day => {
                // The days shown are the first and the last; the times, if made timed, 9 to 10.
                let last = (after - Duration::days(1)).date().max(first.date());
                let nine = NaiveTime::from_hms_opt(9, 0, 0).expect("a time");
                let ten = NaiveTime::from_hms_opt(10, 0, 0).expect("a time");
                (true, first.date().and_time(nine), last.and_time(ten), guests_of(event))
            }
            (Some(event), Some((start, end))) => (false, start, end, guests_of(event)),
            _ => {
                let start = start.unwrap_or_else(next_hour);
                (false, start, start + Duration::hours(1), Vec::new())
            }
        };
        let provider = accounts
            .iter()
            .find(|account| event.as_ref().is_some_and(|event| event.account == account.id))
            .map(|account| account.provider);

        // Every calendar a new event can go on, account by account; the account's own first.
        let many_accounts = accounts.iter().filter(|a| a.granted && a.writable).count() > 1;
        let mut choices = Vec::new();
        for (index, account) in accounts.iter().enumerate() {
            if !(account.granted && account.writable) {
                continue;
            }
            for calendar in account.calendars.iter().filter(|c| c.writable) {
                let label = if many_accounts && calendar.name != account.id {
                    format!("{} · {}", calendar.name, account.id)
                } else {
                    calendar.name.clone()
                };
                choices.push(Choice {
                    index: choices.len(),
                    account: account.id.clone(),
                    calendar: calendar.id.clone(),
                    label: label.into(),
                    provider: account.provider,
                    color: super::model::hex_color(calendar.color.as_deref()).unwrap_or_else(|| design::account_color(index)),
                });
            }
        }
        let primary = |choice: &Choice| {
            accounts.iter().any(|a| a.id == choice.account && a.calendars.iter().any(|c| c.id == choice.calendar && c.primary))
        };
        let chosen = choices.iter().position(primary).or((!choices.is_empty()).then_some(0));
        let calendar = (event.is_none() && !choices.is_empty()).then(|| {
            cx.new(|cx| SelectState::new(choices.clone(), chosen.map(IndexPath::new), window, cx))
        });

        let item = event.as_ref();
        let title = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Event title").default_value(item.map(|e| e.title.clone()).unwrap_or_default())
        });
        let location = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add a place or a link")
                .default_value(item.and_then(|e| e.location.clone()).unwrap_or_default())
        });
        let guest_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name or email address"));
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Add a description")
                .auto_grow(3, 8)
                .default_value(item.and_then(|e| e.notes.clone()).unwrap_or_default())
        });
        let date = |at: NaiveDateTime, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut state = DatePickerState::new(window, cx).date_format("%a, %b %-d, %Y");
                state.set_date_time(at, window, cx);
                state
            })
        };
        let time = |at: NaiveDateTime, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut state = TimeFieldState::new(window, cx).hour_cycle(HourCycle::H12);
                state.set_time(at.time(), window, cx);
                state
            })
        };
        let start_date = date(start, window, cx);
        let end_date = date(end, window, cx);
        let start_time = time(start, window, cx);
        let end_time = time(end, window, cx);

        let mut subscriptions = vec![
            cx.subscribe_in(&title, window, Self::on_title_event),
            cx.subscribe_in(&location, window, Self::on_title_event),
            cx.subscribe_in(&guest_input, window, Self::on_guest_event),
            cx.subscribe_in(&start_date, window, |this, _, DatePickerEvent::Change(value), window, cx| {
                if let Some(at) = value.start() {
                    this.moved(Moment::StartDate, at, window, cx);
                }
            }),
            cx.subscribe_in(&end_date, window, |this, _, DatePickerEvent::Change(value), window, cx| {
                if let Some(at) = value.start() {
                    this.moved(Moment::EndDate, at, window, cx);
                }
            }),
            cx.subscribe_in(&start_time, window, |this, _, TimeFieldEvent::Change(time), window, cx| {
                this.moved(Moment::StartTime, this.start.date().and_time(*time), window, cx);
            }),
            cx.subscribe_in(&end_time, window, |this, _, TimeFieldEvent::Change(time), window, cx| {
                this.moved(Moment::EndTime, this.end.date().and_time(*time), window, cx);
            }),
        ];
        if let Some(select) = &calendar {
            subscriptions.push(cx.subscribe(select, |this, _, event: &SelectEvent<Vec<Choice>>, cx| {
                let SelectEvent::Confirm(choice) = event;
                if choice.is_some() {
                    this.chosen = *choice;
                    cx.notify();
                }
            }));
        }

        let initial = event.as_ref().map(|_| Form {
            title: String::new(),
            location: String::new(),
            notes: String::new(),
            all_day,
            start,
            end,
            guests: guests.clone(),
        });
        Self {
            pane,
            event,
            initial,
            provider,
            time_zone,
            focus: cx.focus_handle(),
            title,
            choices,
            calendar,
            chosen,
            all_day,
            start,
            end,
            start_date,
            start_time,
            end_date,
            end_time,
            location,
            guests,
            guest_input,
            suggestions: Vec::new(),
            generation: 0,
            notes,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn form(&self, cx: &App) -> Form {
        Form {
            title: self.title.read(cx).value().to_string(),
            location: self.location.read(cx).value().to_string(),
            notes: self.notes.read(cx).value().to_string(),
            all_day: self.all_day,
            start: self.start,
            end: self.end,
            guests: self.guests.clone(),
        }
    }

    /// A picker changed: a new start keeps the length the event had.
    fn moved(&mut self, moment: Moment, at: NaiveDateTime, window: &mut Window, cx: &mut Context<Self>) {
        match moment {
            Moment::StartDate | Moment::StartTime => {
                let at = match moment {
                    Moment::StartDate => at.date().and_time(self.start.time()),
                    _ => at,
                };
                let length = self.end - self.start;
                self.start = at;
                self.end = at + length;
                let end = self.end;
                self.end_date.update(cx, |state, cx| state.set_date_time(end, window, cx));
                self.end_time.update(cx, |state, cx| state.set_time(end.time(), window, cx));
            }
            Moment::EndDate => self.end = at.date().and_time(self.end.time()),
            Moment::EndTime => self.end = at,
        }
        self.error = None;
        cx.notify();
    }

    fn on_title_event(&mut self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let InputEvent::Change = event {
            self.error = None;
            cx.notify();
        }
    }

    /// ↩ (the dialog's Confirm): in the guests' field it takes what is typed there (a name, the
    /// first person found for it), elsewhere it saves.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guest_input.focus_handle(cx).is_focused(window) {
            self.save(&Save, window, cx);
            return;
        }
        let typed = self.guest_input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        if !typed.contains('@') {
            if let Some(first) = self.suggestions.first().map(|person| person.email.clone()) {
                self.add_guest(first, window, cx);
                return;
            }
        }
        self.commit_typed(window, cx);
    }

    fn on_guest_event(&mut self, input: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.generation += 1;
                self.suggestions.clear();
                self.error = None;
                let query = input.read(cx).value().trim().to_owned();
                if !query.is_empty() {
                    self.search(query, self.generation, cx);
                }
                cx.notify();
            }
            InputEvent::PressEnter { secondary: true, .. } => self.save(&Save, window, cx),
            _ => {}
        }
    }

    fn search(&mut self, query: String, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            if !this.update(cx, |this, _| this.generation == generation).unwrap_or(false) {
                return;
            }
            let found = cx.background_executor().spawn(async move { engine().search_contacts(query, SUGGESTIONS).await }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                // Without people lookup none are offered; typed addresses still work.
                let taken = normalized(&this.guests);
                this.suggestions = found
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|person| !taken.contains(&person.email.to_lowercase()))
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Takes the typed addresses (split at commas, semicolons and spaces) into the guests; what
    /// isn't an address stays typed. False when something was left.
    fn commit_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let typed = self.guest_input.read(cx).value().to_string();
        let parts: Vec<&str> = typed.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return true;
        }
        let bad: Vec<&str> = parts.iter().copied().filter(|part| !is_email(part)).collect();
        for part in parts.iter().filter(|part| is_email(part)) {
            if !self.guests.iter().any(|g| g.eq_ignore_ascii_case(part)) {
                self.guests.push((*part).to_owned());
            }
        }
        let left = bad.join(" ");
        self.guest_input.update(cx, |input, cx| input.set_value(left, window, cx));
        self.suggestions.clear();
        self.error = bad.first().map(|bad| format!("\"{bad}\" is not an email address.").into());
        cx.notify();
        bad.is_empty()
    }

    fn add_guest(&mut self, email: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guests.iter().any(|g| g.eq_ignore_ascii_case(&email)) {
            self.guests.push(email);
        }
        self.generation += 1;
        self.suggestions.clear();
        self.error = None;
        self.guest_input.update(cx, |input, cx| input.clean(window, cx));
        window.focus(&self.guest_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.commit_typed(window, cx) {
            return;
        }
        let form = self.form(cx);
        if let Some(problem) = problem(&form) {
            self.error = Some(problem.into());
            cx.notify();
            return;
        }
        let existing = self.event.as_ref().zip(self.initial.as_ref());
        let draft = draft(&form, existing);
        if self.event.is_some() && is_empty(&draft) {
            window.close_dialog(cx);
            return;
        }
        let target = match &self.event {
            Some(event) => Err((event.account.clone(), event.calendar.clone(), event.id.clone())),
            None => match self.chosen.and_then(|index| self.choices.get(index)) {
                Some(choice) => Ok((choice.account.clone(), choice.calendar.clone())),
                None => return,
            },
        };
        self.saving = true;
        self.error = None;
        cx.notify();
        let created = target.is_ok();
        let pane = self.pane.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match target {
                        Ok((account, calendar)) => engine().calendar_create(account, Some(calendar), draft).await,
                        Err((account, calendar, id)) => engine().calendar_update(account, calendar, id, draft).await,
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(saved) => {
                    pane.update(cx, |pane, cx| pane.saved(&saved, created, cx)).ok();
                    window.close_dialog(cx);
                }
                Err(error) => {
                    this.saving = false;
                    this.error = Some(error.to_string().into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn row(label: &'static str, top: bool, child: impl IntoElement, p: &Palette) -> gpui::Div {
        div()
            .flex()
            .when(top, |d| d.items_start())
            .when(!top, |d| d.items_center())
            .gap(px(space::S10))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(LABEL))
                    .when(top, |d| d.pt(px(5.)))
                    .text_right()
                    .text_color(p.muted)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(child))
    }

    fn moment(&self, date: &Entity<DatePickerState>, time: &Entity<TimeFieldState>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(div().w(px(200.)).child(DatePicker::new(date).disabled(self.saving)))
            .when(!self.all_day, |d| d.child(div().w(px(120.)).child(TimeField::new(time).disabled(self.saving))))
    }

    /// The service that emails the guests and holds the description.
    fn service(&self) -> &'static str {
        let provider = match &self.event {
            Some(_) => self.provider,
            None => self.chosen.and_then(|index| self.choices.get(index)).map(|choice| choice.provider),
        };
        if provider == Some(Provider::Microsoft) { "Outlook" } else { "Google Calendar" }
    }
}

impl Render for EventEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let saving = self.saving;
        let notes_cut = self.event.as_ref().is_some_and(|event| event.notes_cut);
        let series = self.event.as_ref().is_some_and(|event| event.series.is_some());

        let calendar: AnyElement = match (&self.event, &self.calendar) {
            (Some(event), _) => {
                let name = event.calendar_name.clone().unwrap_or_else(|| event.calendar.clone());
                let label = if name == event.account { name } else { format!("{name} · {}", event.account) };
                div().child(label).into_any_element()
            }
            (None, Some(select)) => Select::new(select).disabled(saving).into_any_element(),
            (None, None) => div().text_color(p.muted).child("No calendar here can take new events.").into_any_element(),
        };
        let all_day = Checkbox::new("event-all-day").checked(self.all_day).disabled(saving).on_click(cx.listener(
            |this, on: &bool, _, cx| {
                this.all_day = *on;
                this.error = None;
                cx.notify();
            },
        ));

        let guests = div()
            .flex()
            .flex_col()
            .gap(px(space::S6))
            .when(!self.guests.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(space::S6)).children(self.guests.iter().enumerate().map(|(index, email)| {
                    div()
                        .id(("guest", index))
                        .when(!saving, |d| {
                            d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if index < this.guests.len() {
                                    this.guests.remove(index);
                                    cx.notify();
                                }
                            }))
                        })
                        .tooltip(crate::controls::tooltip("Remove"))
                        .child(pill(format!("{email}  ×"), None, p))
                })))
            })
            .child(Input::new(&self.guest_input).disabled(saving))
            .when(!self.suggestions.is_empty(), |d| {
                d.child(crate::controls::popover(p).py(px(space::S4)).flex().flex_col().children(
                    self.suggestions.iter().enumerate().map(|(index, person)| {
                        let email = person.email.clone();
                        div()
                            .id(("guest-suggestion", index))
                            .px(px(space::S10))
                            .py(px(space::S4))
                            .flex()
                            .gap(px(space::S8))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.add_guest(email.clone(), window, cx)))
                            .children(person.name.clone().map(|name| div().font_weight(FontWeight::MEDIUM).child(name)))
                            .child(div().text_color(p.muted).child(person.email.clone()))
                    }),
                ))
            });

        let notes = div()
            .flex()
            .flex_col()
            .gap(px(space::S4))
            .child(Textarea::new(&self.notes).disabled(saving || notes_cut))
            .when(notes_cut, |d| {
                d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(format!(
                    "The description is longer than shown here; change it in {}.",
                    self.service()
                )))
            });

        let guests_hear = !self.guests.is_empty() || self.event.as_ref().is_some_and(|event| !event.attendees.is_empty());
        let mut footnote = Vec::new();
        if series {
            footnote.push("Only this occurrence changes.".to_owned());
        }
        if !self.all_day && !self.time_zone.is_empty() {
            footnote.push(format!("Times in {}.", self.time_zone));
        }
        if guests_hear {
            let what = if self.event.is_none() { "invitation" } else { "change" };
            footnote.push(format!("{} emails the {what} to the guests.", self.service()));
        }

        let can_save = !saving
            && !self.title.read(cx).value().trim().is_empty()
            && (self.event.is_some() || self.chosen.is_some());
        let save_label = if saving { "Saving" } else if self.event.is_none() { "Add event" } else { "Save" };

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::save))
            .flex()
            .flex_col()
            .gap(px(space::S10))
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .child(Self::row("Title", false, Input::new(&self.title).font_weight(FontWeight::SEMIBOLD).disabled(saving), p))
            .child(Self::row("Calendar", false, calendar, p))
            .child(Self::row("All day", false, all_day, p))
            .child(Self::row("Starts", false, self.moment(&self.start_date, &self.start_time), p))
            .child(Self::row("Ends", false, self.moment(&self.end_date, &self.end_time), p))
            .child(Self::row("Location", false, Input::new(&self.location).disabled(saving), p))
            .child(Self::row("Guests", !self.guests.is_empty(), guests, p))
            .child(Self::row("Notes", true, notes, p))
            .when(!footnote.is_empty(), |d| {
                d.child(div().pl(px(LABEL + space::S10)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(footnote.join(" ")))
            })
            .when_some(self.error.clone(), |d, error| d.child(div().pl(px(LABEL + space::S10)).text_color(p.danger).child(error)))
            .child(
                div()
                    .pt(px(space::S6))
                    .flex()
                    .justify_end()
                    .gap(px(space::S8))
                    .child(
                        button("event-cancel", "Cancel", ButtonKind::Secondary, ControlSize::Regular, !saving, p)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        button("event-save", save_label, ButtonKind::Primary, ControlSize::Regular, can_save, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(&Save, window, cx))),
                    ),
            )
    }
}

impl CalendarPane {
    /// Some account may add events to one of its calendars.
    pub(super) fn can_create(&self) -> bool {
        self.accounts.iter().any(|account| account.granted && account.writable && account.calendars.iter().any(|c| c.writable))
    }

    /// Opens the editor for a new event from `start` (an hour long), the next full hour without one.
    pub(super) fn create_event(&mut self, start: Option<NaiveDateTime>, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_create() {
            self.open_editor(None, start, window, cx);
        }
    }

    /// Opens the editor on the event at `index`, which the account organizes.
    pub(super) fn edit_event(&mut self, key: &EventKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.event_index(key).filter(|&index| self.events[index].item.editable) {
            self.open_editor(Some(index), None, window, cx);
        }
    }

    fn open_editor(&mut self, index: Option<usize>, start: Option<NaiveDateTime>, window: &mut Window, cx: &mut Context<Self>) {
        let pane = cx.entity().downgrade();
        let event = index.map(|index| &self.events[index]);
        let item = event.map(|event| event.item.clone());
        let span = event.map(|event| (event.start.naive_local(), event.end.naive_local()));
        let title = if item.is_some() { "Edit event" } else { "New event" };
        let (accounts, time_zone) = (self.accounts.clone(), self.time_zone.clone());
        let editor = cx.new(|cx| EventEditor::new(pane, accounts, time_zone, item, span, start, window, cx));
        let content = editor.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let editor = content.clone();
            dialog.title(title).w(px(WIDTH)).overlay_closable(false).child(content.clone()).on_ok(move |_, window, cx| {
                // The editor closes the dialog itself once the calendar took the event.
                editor.update(cx, |editor, cx| editor.confirm(window, cx));
                false
            })
        });
        let field = editor.read(cx).title.focus_handle(cx);
        window.focus(&field, cx);
    }

    /// The editor saved `saved`: the calendar reads it in, a new one opened beside it.
    fn saved(&mut self, saved: &CalendarItem, created: bool, cx: &mut Context<Self>) {
        if created {
            self.open_after = Some(EventKey {
                account: saved.account.clone().into(),
                calendar: saved.calendar.clone().into(),
                id: saved.id.clone().into(),
            });
        }
        self.reload(cx);
    }

    /// Asks before deleting the event at `index`, saying who hears of it, and deletes it when the
    /// person agrees.
    pub(super) fn confirm_delete(&mut self, key: &EventKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.event_index(key).map(|index| &self.events[index]).filter(|event| event.item.editable) else { return };
        let item = &event.item;
        let guests = item.attendees.iter().filter(|p| !p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&item.account))).count();
        let mut message = vec![format!(
            "It is removed from the “{}” calendar.",
            item.calendar_name.as_deref().unwrap_or(&item.calendar)
        )];
        if item.series.is_some() {
            message.push("Only this occurrence is deleted; the rest of the series stays.".into());
        }
        match guests {
            0 => {}
            1 => message.push("Its guest is told it is cancelled.".into()),
            n => message.push(format!("Its {n} guests are told it is cancelled.")),
        }
        let title: SharedString = format!("Delete “{}”?", item.title).into();
        let message: SharedString = message.join(" ").into();
        let key = event.key.clone();
        let pane = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (pane, key) = (pane.clone(), key.clone());
            alert
                .title(title.clone())
                .description(message.clone())
                .show_cancel(true)
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    pane.update(cx, |pane, cx| pane.delete(&key, cx)).ok();
                    true
                })
        });
    }

    /// Deletes the event the person confirmed, telling its guests; closes it.
    fn delete(&mut self, key: &EventKey, cx: &mut Context<Self>) {
        let Some(event) = self.event_index(key).map(|index| &self.events[index]) else { return };
        let (account, calendar, id, title) =
            (event.item.account.clone(), event.item.calendar.clone(), event.item.id.clone(), event.item.title.clone());
        let key = key.clone();
        self.background(
            cx,
            async move { engine().calendar_delete(account, calendar, id, true).await },
            move |this, result, cx| match result {
                Ok(()) => {
                    if this.opened.as_ref() == Some(&key) {
                        this.close_event(cx);
                    }
                    this.reload(cx);
                }
                Err(error) => this.toast(format!("Could not delete \"{title}\": {error}"), cx),
            },
        );
    }
}

/// The guests' addresses, the account's own left out.
fn guests_of(event: &CalendarItem) -> Vec<String> {
    event
        .attendees
        .iter()
        .filter_map(|person| person.email.clone())
        .filter(|email| !email.eq_ignore_ascii_case(&event.account))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M").unwrap()
    }

    fn form() -> Form {
        Form {
            title: " Standup ".into(),
            location: String::new(),
            notes: "Agenda\n".into(),
            all_day: false,
            start: at("2026-11-12T10:00"),
            end: at("2026-11-12T10:30"),
            guests: vec!["a@example.com".into()],
        }
    }

    fn item() -> CalendarItem {
        serde_json::from_value(serde_json::json!({
            "account": "me@example.com", "calendar": "primary", "id": "e1", "title": "Standup",
            "start": "2026-11-12T18:00:00Z", "end": "2026-11-12T18:30:00Z", "notes": "Agenda",
        }))
        .unwrap()
    }

    #[test]
    fn a_new_event_sends_everything_given() {
        let draft = draft(&form(), None);
        assert_eq!(draft.title.as_deref(), Some("Standup"));
        assert_eq!(draft.start.as_deref(), Some("2026-11-12T10:00"));
        assert_eq!(draft.end.as_deref(), Some("2026-11-12T10:30"));
        assert_eq!(draft.location, None);
        assert_eq!(draft.notes.as_deref(), Some("Agenda"));
        assert_eq!(draft.attendees, Some(vec!["a@example.com".to_owned()]));
    }

    #[test]
    fn a_change_sends_only_what_changed() {
        let initial = form();
        let mut changed = form();
        changed.guests = vec!["A@example.com".into()];
        assert!(is_empty(&draft(&changed, Some((&item(), &initial)))));
        changed.start = at("2026-11-12T11:00");
        changed.all_day = true;
        let draft = draft(&changed, Some((&item(), &initial)));
        assert_eq!(draft.start.as_deref(), Some("2026-11-12"));
        assert!(draft.all_day);
        assert_eq!(draft.title, None);
    }

    #[test]
    fn an_untitled_or_backwards_event_is_refused() {
        let mut f = form();
        assert_eq!(problem(&f), None);
        f.end = f.start;
        assert_eq!(problem(&f), Some("The event ends before it starts."));
        f.all_day = true;
        assert_eq!(problem(&f), None);
        f.title = "  ".into();
        assert_eq!(problem(&f), Some("Give the event a title."));
    }

    #[test]
    fn only_whole_addresses_are_guests() {
        assert!(is_email("a.b@example.co.uk"));
        assert!(!is_email("a@example"));
        assert!(!is_email("ann"));
        assert!(!is_email("a@b@c.com"));
        assert!(!is_email("a,b@c.com"));
    }
}
