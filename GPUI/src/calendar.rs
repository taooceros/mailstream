//! The calendar (Flutter's `CalendarState`, `CalendarListPane` and `CalendarEventPane`, MS-211):
//! every account's events through the in-process engine's `calendar_*` methods, shown instead of
//! the mail list while Calendar is selected. The agenda lists today on, under this week's days
//! and the meeting about to start; the open event sits beside it (over it in a narrow window),
//! answering its invitation through the account's calendar.
//!
//! The engine fetches Google Calendar and Outlook every five minutes and keeps what it fetched in
//! mail.db; the pane reads what it holds (a week back on) and reads it again on its `Calendar`
//! event, so the calendar shows at once and nothing waits on the network.

mod agenda;
mod detail;
pub mod model;

use std::collections::HashMap;
use std::time::Duration;

use chrono::{Days, Local, NaiveDate};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding, ListAlignment,
    ListState, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task, Window, actions, div, px,
    relative,
};
use mailstream::CalendarAccount;

use crate::design::{self, layout, text};
use crate::engine::engine;
use model::{Event, EventKey};

actions!(calendar, [Next, Previous, Back, Refresh]);

const CONTEXT: &str = "Calendar";

/// Days of events the agenda lists, from today.
const DAYS: i64 = 60;
/// Days back the engine fetches, read along for the views that show past days.
const PAST_DAYS: i64 = 7;
/// How often the agenda follows the clock (the Up Next card, a new day).
const CLOCK: Duration = Duration::from_secs(15);

pub fn bind_keys(cx: &mut App) {
    // Single keys act on the calendar, never while a field has focus; the window's own keys
    // (mail's) give way to these inside it.
    let list = Some("Calendar && !TextInput && !TextArea");
    let pane = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", Next, list),
        KeyBinding::new("down", Next, list),
        KeyBinding::new("k", Previous, list),
        KeyBinding::new("up", Previous, list),
        KeyBinding::new("escape", Back, list),
        KeyBinding::new("secondary-[", Back, pane),
        KeyBinding::new("secondary-r", Refresh, pane),
    ]);
}

/// A message for the window's toast.
pub struct CalendarToast(pub SharedString);

/// One line of the agenda.
enum Entry {
    /// An account whose calendar cannot show (an index into `accounts`): it must sign in again
    /// for calendar access, or its last fetch failed.
    Notice(usize),
    Day { day: NaiveDate, title: SharedString, summary: SharedString },
    /// An index into `events`, under the day it is listed on.
    Event { index: usize, day: NaiveDate },
    /// Why the agenda is empty.
    Note(SharedString),
}

pub struct CalendarPane {
    focus: FocusHandle,
    /// Calendar is in view instead of mail.
    active: bool,
    accounts: Vec<CalendarAccount>,
    /// Earliest first, as the engine lists them.
    events: Vec<Event>,
    loading: bool,
    /// `calendar_events` answered once: until then no events is not "nothing planned".
    listed: bool,
    /// The event beside the calendar.
    opened: Option<EventKey>,
    /// Answers on their way to the calendar, by event, until it takes them.
    answering: HashMap<EventKey, &'static str>,
    /// A read is under way; `read_again` asks for one more when it lands.
    reading: bool,
    read_again: bool,
    /// The agenda's lines and the order ↑ and ↓ step through its events, for the day they were
    /// built on (`built_for`); a new day builds them again.
    entries: Vec<Entry>,
    order: Vec<usize>,
    built_for: Option<NaiveDate>,
    /// Minutes booked on each day of this week, for the week strip.
    week_booked: [i64; 7],
    agenda: ListState,
    detail_scroll: ScrollHandle,
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CalendarToast> for CalendarPane {}

impl Focusable for CalendarPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CalendarPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK).await;
                let alive = this.update(cx, |this, cx| {
                    if this.active {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            focus: cx.focus_handle(),
            active: false,
            accounts: Vec::new(),
            events: Vec::new(),
            loading: false,
            listed: false,
            opened: None,
            answering: HashMap::new(),
            reading: false,
            read_again: false,
            entries: Vec::new(),
            order: Vec::new(),
            built_for: None,
            week_booked: [0; 7],
            agenda: ListState::new(0, ListAlignment::Top, px(400.)),
            detail_scroll: ScrollHandle::new(),
            _clock: clock,
            _subscriptions: vec![cx.observe_window_appearance(window, |_, _, cx| cx.notify())],
        }
    }

    /// Runs `work` on the background executor, then `done` with its result on the main thread.
    fn background<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl Future<Output = R> + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(work).await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    fn toast(&self, text: String, cx: &mut Context<Self>) {
        cx.emit(CalendarToast(text.into()));
    }

    /// The calendar comes into view, from what the engine holds (it fetches what got old itself).
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = true;
        window.focus(&self.focus);
        self.reload(cx);
        cx.notify();
    }

    /// Mail or Chat comes back into view.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.opened = None;
        cx.notify();
    }

    /// The engine's calendar (or its accounts) changed.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        if self.active || self.listed {
            self.reload(cx);
        }
    }

    /// Reads the events the engine holds; a request made meanwhile runs once after it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            self.read_again = true;
            return;
        }
        self.reading = true;
        let from = (Local::now().date_naive() - Days::new(PAST_DAYS as u64)).format("%Y-%m-%d").to_string();
        let days = (PAST_DAYS + DAYS + 1) as u32;
        self.background(cx, async move { engine().calendar_events(Some(from), days).await }, |this, result, cx| {
            this.reading = false;
            match result {
                Ok(agenda) => {
                    this.accounts = agenda.accounts;
                    this.events = agenda.events.into_iter().filter_map(Event::new).collect();
                    this.loading = agenda.loading;
                    this.listed = true;
                    if this.opened.as_ref().is_some_and(|key| this.event_index(key).is_none()) {
                        this.opened = None;
                    }
                    this.rebuild(Local::now().date_naive());
                }
                Err(error) => this.toast(format!("Could not read the calendar: {error}"), cx),
            }
            if std::mem::take(&mut this.read_again) {
                this.reload(cx);
            }
            cx.notify();
        });
    }

    /// The agenda's lines from `today` on, the strip's booked hours, and the stepping order.
    fn rebuild(&mut self, today: NaiveDate) {
        let days = model::agenda_days(&self.events, today);
        let mut entries = Vec::with_capacity(self.accounts.len() + days.len() * 4);
        for (index, account) in self.accounts.iter().enumerate() {
            if !account.granted || account.problem.is_some() {
                entries.push(Entry::Notice(index));
            }
        }
        for (day, indices) in &days {
            entries.push(Entry::Day {
                day: *day,
                title: model::agenda_title(*day, today).into(),
                summary: model::day_summary(&self.events, indices, *day).into(),
            });
            entries.extend(indices.iter().map(|&index| Entry::Event { index, day: *day }));
        }
        if days.is_empty() && !(self.loading && self.has_calendars()) {
            entries.push(Entry::Note(if self.has_calendars() {
                format!("Nothing on your calendars in the next {DAYS} days").into()
            } else {
                "No account has calendar access yet. Sign in again to show its calendar here.".into()
            }));
        }
        self.order = model::in_order(&days);
        for (slot, day) in model::week_days(model::week_of(today)).into_iter().enumerate() {
            let placed = model::layout_day(&self.events, 0..self.events.len(), day);
            self.week_booked[slot] = model::booked_minutes(&self.events, &placed);
        }
        // The line at the top stays there.
        let top = self.agenda.logical_scroll_top();
        self.entries = entries;
        self.agenda.reset(self.entries.len());
        if top.item_ix < self.entries.len() {
            self.agenda.scroll_to(top);
        }
        self.built_for = Some(today);
    }

    /// Accounts whose sign-in grants calendar access.
    fn has_calendars(&self) -> bool {
        self.accounts.iter().any(|account| account.granted)
    }

    fn event_index(&self, key: &EventKey) -> Option<usize> {
        self.events.iter().position(|event| &event.key == key)
    }

    fn account_of(&self, event: &Event) -> Option<&CalendarAccount> {
        self.accounts.iter().find(|account| account.id == event.item.account)
    }

    /// The event's color: its calendar's, else its account's.
    fn event_color(&self, event: &Event) -> gpui::Hsla {
        model::hex_color(event.item.color.as_deref()).unwrap_or_else(|| {
            let index = self.accounts.iter().position(|account| account.id == event.item.account).unwrap_or(0);
            design::account_color(index)
        })
    }

    fn reply(&self, event: &Event) -> model::ReplyState {
        model::reply_state(&event.item, self.account_of(event), self.answering.get(&event.key).copied())
    }

    pub fn open(&mut self, key: EventKey, cx: &mut Context<Self>) {
        if self.opened.as_ref() != Some(&key) {
            self.opened = Some(key);
            self.detail_scroll = ScrollHandle::new();
            cx.notify();
        }
    }

    /// Closes the open event (⌘W, Esc, its close button); nothing else.
    pub fn close_event(&mut self, cx: &mut Context<Self>) {
        if self.opened.take().is_some() {
            cx.notify();
        }
    }

    /// Answers the invitation to `key` (`accepted`, `tentative` or `declined`) through its
    /// account's calendar, which tells the organizer; that occurrence alone when it repeats. The
    /// answer shows chosen while it is sent.
    fn respond(&mut self, key: &EventKey, response: &'static str, cx: &mut Context<Self>) {
        let Some(event) = self.event_index(key).map(|index| &self.events[index]) else { return };
        if self.answering.contains_key(key) {
            return;
        }
        let (account, calendar, id, title) =
            (event.item.account.clone(), event.item.calendar.clone(), event.item.id.clone(), event.item.title.clone());
        self.answering.insert(key.clone(), response);
        cx.notify();
        let key = key.clone();
        self.background(
            cx,
            async move { engine().calendar_respond(account, Some(calendar), id, response.to_owned(), true).await },
            move |this, result, cx| {
                this.answering.remove(&key);
                match result {
                    Ok(()) => this.reload(cx),
                    Err(error) => this.toast(format!("Could not answer \"{title}\": {error}"), cx),
                }
                cx.notify();
            },
        );
    }

    /// Opens the event `step` after (1) or before (-1) the open one, bringing its row into view.
    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(index) = model::stepped(&self.order, &self.events, self.opened.as_ref(), step) else { return };
        let key = self.events[index].key.clone();
        if let Some(line) = self.entries.iter().position(|entry| matches!(entry, Entry::Event { index: at, .. } if *at == index)) {
            // A day's first event brings its header along.
            let header = line > 0 && matches!(self.entries[line - 1], Entry::Day { .. });
            self.agenda.scroll_to_reveal_item(if header && step < 0 { line - 1 } else { line });
        }
        self.open(key, cx);
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn back(&mut self, _: &Back, _: &mut Window, cx: &mut Context<Self>) {
        if self.opened.is_some() {
            self.close_event(cx);
        } else {
            cx.propagate();
        }
    }

    /// Fetches every account's events now.
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        if !self.has_calendars() {
            return;
        }
        self.background(cx, async { engine().calendar_refresh().await }, |this, result, cx| {
            if let Err(error) = result {
                this.toast(format!("Could not refresh the calendar: {error}"), cx);
            }
        });
    }
}

impl Render for CalendarPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        let today = Local::now().date_naive();
        if self.listed && self.built_for != Some(today) {
            self.rebuild(today);
        }
        let two_pane = f32::from(window.viewport_size().width) >= layout::TWO_PANE_WIDTH;
        let list = self.render_list_island(today, p, window, cx);
        let detail = self.opened.clone().and_then(|key| self.event_index(&key)).map(|index| {
            self.render_detail(index, two_pane, p, window, cx)
        });
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::refresh))
            .relative()
            .size_full()
            .flex()
            .gap(px(layout::GAP))
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .map(|d| match detail {
                // Beside the list, which keeps its share of the room.
                Some(detail) if two_pane => d
                    .child(list.h_full().flex_shrink_0().w(relative(layout::LIST_SHARE)).min_w(px(layout::MIN_LIST)))
                    .child(div().flex_1().min_w_0().h_full().child(detail)),
                // Over the list.
                Some(detail) => d.child(list.size_full()).child(div().absolute().size_full().child(detail)),
                None => d.child(list.size_full()),
            })
    }
}
