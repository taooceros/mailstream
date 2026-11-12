//! Calendars and the `calendar.*` commands: every account whose sign-in grants calendar access
//! (Google Calendar, Outlook through Graph) with its events from a week back to two months on,
//! recurring events expanded. Each account's calendars and events are kept in mail.db, so the
//! calendar paints at once; while that copy is younger than [`FRESH`] nothing is fetched. A
//! timer fetches accounts whose copy got older; `calendar.refresh` fetches now. Front ends
//! re-read on `calendar`. Meeting invitations in mail are matched to their events and answered
//! in [`invitation`]; events the account organizes are added, changed and deleted in [`edit`].
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::accounts::{AccountRecord, Accounts};
use crate::answers::{self, CalendarAccount, CalendarAgenda, CalendarDays, CalendarItem};
use crate::api::Priority;
use crate::calendar_api::{CalendarEvent, CalendarInfo, CalendarRemote, local_day, local_midnight, local_zone};
use crate::cache::MailCache;
use crate::commands::Arguments;
use crate::mail::Mail;
use crate::model::{Date, iso8601, now};
use crate::{EngineError, Events};

mod edit;
mod invitation;

/// How long a fetched copy serves before the timer fetches it again.
const FRESH: f64 = 5.0 * 60.0;
/// How often the timer looks for copies that got old.
const CHECK: Duration = Duration::from_secs(60);
/// The window fetched: from this many days back to this many on, counted from today.
const PAST_DAYS: i64 = 7;
const FUTURE_DAYS: i64 = 60;

/// What mail.db keeps per account: its calendars and their events, as last fetched.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalendarCopy {
    pub fetched: Date,
    pub calendars: Vec<CalendarInfo>,
    pub events: Vec<CalendarEvent>,
    /// [`COPY_FORMAT`] when it was fetched: a copy from an older one paints, then is fetched.
    #[serde(default)]
    pub format: u32,
}

/// Bumped when fetches start keeping something copies made before lack (1: which calendars
/// may be written and which events the account organizes).
const COPY_FORMAT: u32 = 1;

#[derive(Default)]
struct AccountState {
    copy: Option<CalendarCopy>,
    fetching: bool,
    /// The last fetch's failure, until one succeeds.
    problem: Option<String>,
    /// Changes made from here: a fetch that started before one may answer without it.
    writes: u64,
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, AccountState>,
    /// Events asked of the provider for invitations the fetched window does not hold, by
    /// account and meeting, with when they were asked.
    lookups: HashMap<(String, String), (Date, Option<CalendarEvent>)>,
}

pub struct CalendarStore {
    mail: Arc<Mail>,
    cache: Arc<MailCache>,
    accounts: Arc<Accounts>,
    events: Arc<Events>,
    google: String,
    graph: String,
    state: Mutex<State>,
    /// Wakes the timer: accounts changed.
    wake: tokio::sync::Notify,
}

impl CalendarStore {
    pub fn new(mail: Arc<Mail>, google: String, graph: String, accounts: Arc<Accounts>, events: Arc<Events>) -> Self {
        Self {
            cache: mail.cache(),
            mail,
            accounts,
            events,
            google,
            graph,
            state: Mutex::new(State::default()),
            wake: tokio::sync::Notify::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("calendar lock")
    }

    #[cfg(test)]
    pub async fn flush(&self) {
        self.cache.flush().await;
    }

    /// The accounts whose sign-in grants calendar access, in the sidebar's order.
    fn calendar_accounts(&self) -> Vec<AccountRecord> {
        self.accounts.list().into_iter().filter(AccountRecord::has_calendars).collect()
    }

    /// Paints every calendar account from mail.db, then fetches those whose copy is missing or
    /// old, and again whenever one gets old.
    pub async fn start(self: Arc<Self>) {
        self.sync_accounts().await;
        let store = self.clone();
        tokio::spawn(async move {
            loop {
                store.fetch_due(|_| false);
                tokio::select! {
                    () = tokio::time::sleep(CHECK) => {}
                    () = store.wake.notified() => {}
                }
            }
        });
    }

    /// Signed-in accounts changed: removed ones drop their copy, added ones paint and fetch.
    pub fn accounts_changed(self: &Arc<Self>) {
        let store = self.clone();
        tokio::spawn(async move {
            store.sync_accounts().await;
            store.wake.notify_one();
        });
    }

    /// Follows accounts.json: drops what belongs to accounts gone or no longer granted, and
    /// reads the copies of those new here.
    async fn sync_accounts(&self) {
        let names: HashSet<String> = self.calendar_accounts().into_iter().map(|record| record.id).collect();
        let (gone, added): (usize, Vec<String>) = {
            let mut state = self.state();
            let gone: Vec<String> = state.accounts.keys().filter(|name| !names.contains(*name)).cloned().collect();
            for name in &gone {
                state.accounts.remove(name);
                self.cache.remove_calendar(name);
            }
            state.lookups.retain(|(account, _), _| names.contains(account));
            (gone.len(), names.iter().filter(|name| !state.accounts.contains_key(*name)).cloned().collect())
        };
        if gone == 0 && added.is_empty() {
            return;
        }
        for name in added {
            let copy = self.cache.load_calendar(&name).await;
            let mut state = self.state();
            if names.contains(&name) && !state.accounts.contains_key(&name) {
                state.accounts.insert(name, AccountState { copy, ..AccountState::default() });
            }
        }
        self.events.emit_soon("calendar");
    }

    /// Fetches every account whose copy is missing or older than [`FRESH`], and those `force`
    /// picks whatever their age.
    fn fetch_due(self: &Arc<Self>, force: impl Fn(&str) -> bool) {
        let records: HashMap<String, crate::Provider> =
            self.calendar_accounts().into_iter().map(|record| (record.id, record.provider)).collect();
        let due: Vec<(String, crate::Provider, bool)> = {
            let mut state = self.state();
            let time = now();
            let mut due = Vec::new();
            for (name, account) in state.accounts.iter_mut() {
                let Some(&provider) = records.get(name) else { continue };
                let old = account.copy.as_ref().is_none_or(|copy| {
                    time - copy.fetched >= FRESH || copy.fetched > time || copy.format < COPY_FORMAT
                });
                let forced = force(name);
                if account.fetching || !(forced || old) {
                    continue;
                }
                account.fetching = true;
                due.push((name.clone(), provider, forced));
            }
            due
        };
        if due.is_empty() {
            return;
        }
        self.events.emit_soon("calendar");
        for (name, provider, forced) in due {
            let store = self.clone();
            tokio::spawn(async move { store.fetch(name, provider, forced).await });
        }
    }

    async fn fetch(&self, name: String, provider: crate::Provider, interactive: bool) {
        loop {
            let Some(writes) = self.state().accounts.get(&name).map(|account| account.writes) else { return };
            let start = local_midnight(&local_day(-PAST_DAYS)).unwrap_or_else(|| now() - PAST_DAYS as f64 * 86_400.0);
            let end = local_midnight(&local_day(FUTURE_DAYS + 1)).unwrap_or_else(|| now() + (FUTURE_DAYS + 1) as f64 * 86_400.0);
            let result = match self.mail.api(&name) {
                Some(api) => {
                    let remote = CalendarRemote {
                        api: &api,
                        provider,
                        google: &self.google,
                        graph: &self.graph,
                        priority: if interactive { Priority::Interactive } else { Priority::Background },
                    };
                    remote.fetch(start, end).await.map_err(|error| error.message)
                }
                None => Err(format!("{name} needs to sign in again (Settings > Accounts).")),
            };
            let mut state = self.state();
            // A fetch that outlived its account's sign-out.
            let Some(account) = state.accounts.get_mut(&name) else { return };
            if account.writes != writes {
                // Changed from here meanwhile: this answer may predate the change.
                drop(state);
                continue;
            }
            account.fetching = false;
            match result {
                Ok(fetched) => {
                    let copy = CalendarCopy {
                        fetched: now(),
                        calendars: fetched.calendars,
                        events: fetched.events,
                        format: COPY_FORMAT,
                    };
                    self.cache.save_calendar(&copy, &name);
                    account.copy = Some(copy);
                    account.problem = None;
                }
                Err(message) => account.problem = Some(message),
            }
            drop(state);
            self.events.emit_soon("calendar");
            return;
        }
    }

    /// The calls for `api`'s account, asked for by someone waiting on them.
    fn remote<'a>(&'a self, api: &'a crate::api::Api, provider: crate::Provider) -> CalendarRemote<'a> {
        CalendarRemote { api, provider, google: &self.google, graph: &self.graph, priority: Priority::Interactive }
    }

    /// An account's events changed from here: `edit` brings its copy up to date at once, and a
    /// fetch follows (one already under way fetches again).
    fn changed(self: &Arc<Self>, account: &str, edit: impl FnOnce(&mut CalendarCopy)) {
        {
            let mut state = self.state();
            let Some(entry) = state.accounts.get_mut(account) else { return };
            entry.writes += 1;
            if let Some(copy) = &mut entry.copy {
                edit(copy);
                self.cache.save_calendar(copy, account);
            }
        }
        self.events.emit_soon("calendar");
        self.fetch_due(|name| name == account);
    }

    // MARK: - Commands

    pub async fn run(self: &Arc<Self>, command: &str, args: &Arguments) -> Result<Value, EngineError> {
        let only = match args.string("account") {
            Some(account) => {
                let id = account.to_lowercase();
                if self.accounts.record(&id).is_none() {
                    return Err(EngineError::new(
                        "unknown_account",
                        format!("No account {id}; `mailstream accounts list` shows them."),
                    ));
                }
                Some(id)
            }
            None => None,
        };
        match command {
            "calendar.events" => self.list(only.as_deref(), args),
            "calendar.invitation" => self.invitation(args).await,
            "calendar.respond" => self.respond(only.as_deref(), args).await,
            "calendar.create" | "calendar.update" | "calendar.delete" => {
                let account = only.ok_or_else(|| EngineError::usage("account is required."))?;
                self.edit(command, &account, args).await
            }
            "calendar.refresh" => {
                if let Some(id) = &only
                    && !self.state().accounts.contains_key(id)
                {
                    return Err(EngineError::new(
                        "no_calendar",
                        format!("{id} has not granted calendar access; sign in to it again."),
                    ));
                }
                self.fetch_due(|name| only.as_deref().is_none_or(|only| only == name));
                Ok(json!({ "started": true }))
            }
            _ => unreachable!("{command} is a calendar command"),
        }
    }

    /// The accounts (with whether each may read its calendars, and change them) and the events
    /// overlapping `days` days from `from` (today by default), earliest first, each saying
    /// whether the account may change it (`editable`); this computer's time zone, which new
    /// events take; and the days fetched (`synced`, from `from` up to, not including, `until`):
    /// no others hold events.
    fn list(&self, only: Option<&str>, args: &Arguments) -> Result<Value, EngineError> {
        let from = args.string("from").unwrap_or_else(|| local_day(0));
        let days = args.int("days").unwrap_or(FUTURE_DAYS).clamp(1, 366);
        let (Some(start), Some(end)) = (local_midnight(&from), chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d").ok())
        else {
            return Err(EngineError::usage("from must be a day: 2026-11-08."));
        };
        let end = local_midnight(&(end + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()).unwrap_or(start);
        let state = self.state();
        let mut accounts = Vec::new();
        let mut events: Vec<(Date, CalendarItem)> = Vec::new();
        for record in self.accounts.list() {
            if only.is_some_and(|only| only != record.id) {
                continue;
            }
            let account = state.accounts.get(&record.id);
            let copy = account.and_then(|account| account.copy.as_ref());
            accounts.push(CalendarAccount {
                id: record.id.clone(),
                label: record.label.clone(),
                provider: record.provider,
                granted: account.is_some(),
                writable: account.is_some() && record.can_edit_calendars(),
                loading: account.is_some_and(|account| account.fetching),
                problem: account.and_then(|account| account.problem.clone()),
                fetched: copy.map(|copy| iso8601(copy.fetched)),
                calendars: copy.map_or(Vec::new(), |copy| copy.calendars.clone()),
            });
            let Some(copy) = copy else { continue };
            let calendars: HashMap<&str, &CalendarInfo> =
                copy.calendars.iter().map(|calendar| (calendar.id.as_str(), calendar)).collect();
            for event in &copy.events {
                let overlaps =
                    if event.end > event.start { event.end > start && event.start < end } else { event.start >= start && event.start < end };
                if overlaps {
                    let calendar = calendars.get(event.calendar.as_str()).copied();
                    events.push((event.start, event.item(&record.id, calendar, editable(&record, calendar, event))));
                }
            }
        }
        // All-day events first on their day, then by start.
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.all_day.cmp(&a.1.all_day)));
        let loading = state.accounts.values().any(|account| account.fetching);
        Ok(answers::json(&CalendarAgenda {
            accounts,
            loading,
            events: events.into_iter().map(|(_, event)| event).collect(),
            time_zone: local_zone(),
            synced: CalendarDays { from: local_day(-PAST_DAYS), until: local_day(FUTURE_DAYS + 1) },
        }))
    }
}

/// Whether `record` may change or delete `event`: it organizes it, on a calendar it may write
/// to, and its sign-in lets it change events.
fn editable(record: &AccountRecord, calendar: Option<&CalendarInfo>, event: &CalendarEvent) -> bool {
    event.mine && calendar.is_some_and(|calendar| calendar.writable) && record.can_edit_calendars()
}
