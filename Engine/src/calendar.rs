//! Calendars and the `calendar.*` commands: every account whose sign-in grants calendar access
//! (Google Calendar, Outlook through Graph) with every calendar in its list (its own, and others'
//! shared with it or subscribed to) and the events of those shown from a week back to two months
//! on, recurring events expanded. Which calendars show is the person's choice (`calendar.show`),
//! kept with the account; until they choose, as the provider shows them. Only shown calendars
//! are fetched. Each account's list and each calendar's events are kept in mail.db, so the
//! calendar paints at once; while that copy is younger than [`FRESH`] nothing is fetched. A
//! timer fetches accounts whose copy got older (a Google calendar asks only for what changed
//! since, by its sync token); `calendar.refresh` fetches now. Front ends re-read on `calendar`.
//! Meeting invitations in mail are matched to their events and answered in [`invitation`];
//! events the account organizes are added, changed and deleted in [`edit`].
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::accounts::{AccountRecord, Accounts};
use crate::answers::{self, CalendarAccount, CalendarAgenda, CalendarDays, CalendarItem};
use crate::api::{ApiError, Priority};
use crate::calendar_api::{CalendarEvent, CalendarInfo, CalendarPart, CalendarRemote, local_day, local_midnight, local_zone};
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

/// What mail.db keeps per account: its calendars and the person's choices of which show, in
/// one row, and each calendar's events (`parts`, by calendar id) in a row of its own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalendarCopy {
    pub fetched: Date,
    pub calendars: Vec<CalendarInfo>,
    /// Calendars the person turned on (true) or off here, by id, over what the provider shows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, bool>,
    /// [`COPY_FORMAT`] when it was fetched: a copy from an older one paints, then is fetched.
    #[serde(default)]
    pub format: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parts: HashMap<String, CalendarPart>,
}

impl CalendarCopy {
    /// The events of the calendars shown.
    pub fn events(&self) -> impl Iterator<Item = &CalendarEvent> {
        self.calendars.iter().filter(|calendar| calendar.shown).filter_map(|calendar| self.parts.get(&calendar.id)).flat_map(|part| &part.events)
    }

    /// The account's own calendar's events, while it is shown: where its invitations are.
    pub fn own_events(&self) -> &[CalendarEvent] {
        self.calendars
            .iter()
            .find(|calendar| calendar.primary && calendar.shown)
            .and_then(|calendar| self.parts.get(&calendar.id))
            .map_or(&[], |part| &part.events)
    }

    /// The account's own calendar's id.
    pub fn own_calendar(&self) -> Option<&str> {
        self.calendars.iter().find(|calendar| calendar.primary).map(|calendar| calendar.id.as_str())
    }

    /// The row mail.db keeps the list in: everything but the events.
    pub fn list_row(&self) -> impl Serialize + '_ {
        #[derive(Serialize)]
        struct Row<'a> {
            fetched: Date,
            calendars: &'a [CalendarInfo],
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            choices: &'a BTreeMap<String, bool>,
            format: u32,
        }
        Row { fetched: self.fetched, calendars: &self.calendars, choices: &self.choices, format: self.format }
    }
}

/// Bumped when fetches start keeping something copies made before lack (1: which calendars
/// may be written and which events the account organizes; 2: every calendar in the list, each
/// with its own events and sync token).
const COPY_FORMAT: u32 = 2;

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
                if let Some(copy) = state.accounts.remove(name).and_then(|account| account.copy) {
                    self.cache.remove_calendar(name, copy.calendars.iter().map(|calendar| calendar.id.as_str()));
                }
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

    /// Fetches every account whose copy is missing, older than [`FRESH`] or lacks a shown
    /// calendar's events of today's window, and those `force` picks whatever their age.
    fn fetch_due(self: &Arc<Self>, force: impl Fn(&str) -> bool) {
        let records: HashMap<String, crate::Provider> =
            self.calendar_accounts().into_iter().map(|record| (record.id, record.provider)).collect();
        let (from, until) = window();
        let due: Vec<(String, crate::Provider, bool)> = {
            let mut state = self.state();
            let time = now();
            let mut due = Vec::new();
            for (name, account) in state.accounts.iter_mut() {
                let Some(&provider) = records.get(name) else { continue };
                let old = account.copy.as_ref().is_none_or(|copy| {
                    time - copy.fetched >= FRESH
                        || copy.fetched > time
                        || copy.format < COPY_FORMAT
                        || copy.calendars.iter().filter(|calendar| calendar.shown).any(|calendar| {
                            copy.parts.get(&calendar.id).is_none_or(|part| part.from != from || part.until != until)
                        })
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
            let Some((writes, choices, mut parts)) = self.state().accounts.get(&name).map(|account| {
                let copy = account.copy.as_ref();
                (account.writes, copy.map(|copy| copy.choices.clone()).unwrap_or_default(), copy.map(|copy| copy.parts.clone()).unwrap_or_default())
            }) else {
                return;
            };
            let (from, until) = window();
            let result = match self.mail.api(&name) {
                Some(api) => {
                    let remote = CalendarRemote {
                        api: &api,
                        provider,
                        google: &self.google,
                        graph: &self.graph,
                        priority: if interactive { Priority::Interactive } else { Priority::Background },
                    };
                    fetch_calendars(&remote, &choices, &mut parts, &from, &until).await.map_err(|error| error.message)
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
                Ok((calendars, fetched)) => {
                    // Calendars gone from the list (unsubscribed, sharing taken back) go with their events.
                    let listed: HashSet<&str> = calendars.iter().map(|calendar| calendar.id.as_str()).collect();
                    let gone: Vec<String> = parts.keys().filter(|id| !listed.contains(id.as_str())).cloned().collect();
                    for id in gone {
                        parts.remove(&id);
                        self.cache.remove_calendar_part(&name, &id);
                    }
                    let copy = CalendarCopy { fetched: now(), calendars, choices, format: COPY_FORMAT, parts };
                    self.cache.save_calendar_list(&copy, &name);
                    for id in &fetched {
                        self.cache.save_calendar_part(&copy.parts[id], &name, id);
                    }
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
            "calendar.create" | "calendar.update" | "calendar.delete" | "calendar.show" => {
                let account = only.ok_or_else(|| EngineError::usage("account is required."))?;
                if command == "calendar.show" {
                    return self.show(&account, &args.required("calendar")?, !args.bool("off"));
                }
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

    /// `calendar.show`: shows `calendar` of `account` (or hides it), keeping the choice; a
    /// calendar shown without events of today's window is fetched now.
    fn show(self: &Arc<Self>, account: &str, calendar: &str, shown: bool) -> Result<Value, EngineError> {
        let unknown = || EngineError::new("not_found", format!("{account} has no calendar {calendar}; `mailstream calendar events` lists them."));
        {
            let mut state = self.state();
            let entry = state.accounts.get_mut(account).ok_or_else(|| {
                EngineError::new("no_calendar", format!("{account} has not granted calendar access; sign in to it again."))
            })?;
            let copy = entry.copy.as_mut().ok_or_else(unknown)?;
            let info = copy.calendars.iter_mut().find(|info| info.id == calendar).ok_or_else(unknown)?;
            info.shown = shown;
            copy.choices.insert(calendar.to_string(), shown);
            // A fetch under way asks again with this choice.
            entry.writes += 1;
            self.cache.save_calendar_list(copy, account);
        }
        self.events.emit_soon("calendar");
        self.fetch_due(|_| false);
        Ok(json!({ "calendar": calendar, "shown": shown }))
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
            for event in copy.events() {
                if event.overlaps(start, end) {
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
            synced: {
                let (from, until) = window();
                CalendarDays { from, until }
            },
        }))
    }
}

/// The days fetched, from a week back up to, not including, 61 days on: `2026-11-08`.
fn window() -> (String, String) {
    (local_day(-PAST_DAYS), local_day(FUTURE_DAYS + 1))
}

/// The account's calendar list, each shown as `choices` say (else as the provider shows it),
/// and the events of those shown, each from its copy in `parts` (Google asks only what changed
/// since), which takes them; with the ids of the calendars fetched. A calendar the account may
/// no longer read (sharing taken back) is left empty rather than failing the rest.
async fn fetch_calendars(
    remote: &CalendarRemote<'_>,
    choices: &BTreeMap<String, bool>,
    parts: &mut HashMap<String, CalendarPart>,
    from: &str,
    until: &str,
) -> Result<(Vec<CalendarInfo>, Vec<String>), ApiError> {
    let mut calendars = remote.calendars().await?;
    for calendar in &mut calendars {
        if let Some(&shown) = choices.get(&calendar.id) {
            calendar.shown = shown;
        }
    }
    let shown: Vec<&CalendarInfo> = calendars.iter().filter(|calendar| calendar.shown).collect();
    let asked: Vec<_> = shown.iter().map(|calendar| remote.events(calendar, from, until, parts.get(&calendar.id).cloned())).collect();
    let results = futures::future::join_all(asked).await;
    let mut fetched = Vec::with_capacity(shown.len());
    for (calendar, result) in shown.iter().zip(results) {
        let part = match result {
            Ok(part) => part,
            Err(error) if matches!(error.status, 403 | 404) => CalendarPart::empty(from, until),
            Err(error) => return Err(error),
        };
        parts.insert(calendar.id.clone(), part);
        fetched.push(calendar.id.clone());
    }
    Ok((calendars, fetched))
}

/// Whether `record` may change or delete `event`: it organizes it, on a calendar it may write
/// to, and its sign-in lets it change events.
fn editable(record: &AccountRecord, calendar: Option<&CalendarInfo>, event: &CalendarEvent) -> bool {
    event.mine && calendar.is_some_and(|calendar| calendar.writable) && record.can_edit_calendars()
}
