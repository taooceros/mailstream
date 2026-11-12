//! `calendar.create`, `calendar.update` and `calendar.delete`: events the account organizes, on
//! calendars it may write to, added, changed and deleted through Google Calendar or Graph. The
//! provider's answer goes into the account's copy at once (`changed`), then the account is
//! fetched again. One occurrence of a recurring event changes or goes alone: its id names that
//! occurrence at both providers.
use std::sync::Arc;

use serde_json::{Value, json};

use super::{CalendarStore, editable};
use crate::EngineError;
use crate::answers;
use crate::calendar_api::{CalendarEvent, CalendarInfo, EventFields};
use crate::commands::Arguments;
use crate::model::unix_seconds;

fn provider(error: crate::api::ApiError) -> EngineError {
    EngineError::new("provider", error.message)
}

impl CalendarStore {
    /// The calendar `args` name (the account's own one by default) and, for a change or a
    /// deletion, its event `id`, as last fetched.
    fn target(&self, account: &str, command: &str, args: &Arguments) -> Result<(CalendarInfo, Option<CalendarEvent>), EngineError> {
        let state = self.state();
        let copy = state.accounts.get(account).and_then(|entry| entry.copy.as_ref()).ok_or_else(|| {
            EngineError::new("no_calendar", format!("{account}'s calendars have not been fetched yet; try again shortly."))
        })?;
        let named = args.string("calendar").filter(|id| !id.is_empty());
        let calendar = match named.as_deref() {
            Some(id) => copy.calendars.iter().find(|calendar| calendar.id == id),
            None => copy.calendars.iter().find(|calendar| calendar.primary),
        }
        .cloned()
        .ok_or_else(|| EngineError::new("not_found", format!("{account} has no calendar {}.", named.as_deref().unwrap_or("of its own"))))?;
        if command == "calendar.create" {
            return Ok((calendar, None));
        }
        let id = args.required("id")?;
        let event = copy.events.iter().find(|event| event.calendar == calendar.id && event.id == id).cloned().ok_or_else(|| {
            EngineError::new("not_found", format!("No event {id} on {}; `mailstream calendar events` lists them.", calendar.name))
        })?;
        Ok((calendar, Some(event)))
    }

    /// `calendar.create`, `calendar.update` and `calendar.delete` for `account`.
    pub(super) async fn edit(self: &Arc<Self>, command: &str, account: &str, args: &Arguments) -> Result<Value, EngineError> {
        let record = self.accounts.record(account).expect("run checked the account");
        if !record.has_calendars() || !record.can_edit_calendars() {
            return Err(EngineError::new(
                "no_calendar",
                format!("{account} has not granted changing its calendar; sign in to it again."),
            ));
        }
        let (calendar, event) = self.target(account, command, args)?;
        if !calendar.writable {
            return Err(EngineError::new("read_only", format!("{account} may not change events on {}.", calendar.name)));
        }
        if let Some(event) = &event
            && !editable(&record, Some(&calendar), event)
        {
            return Err(EngineError::new(
                "not_organizer",
                format!("\"{}\" is someone else's event; only its organizer can change it.", event.title),
            ));
        }
        let fields = match command {
            "calendar.delete" => EventFields::default(),
            _ => EventFields::from_args(args, event.is_none())?,
        };
        if command == "calendar.update" && fields.is_empty() {
            return Err(EngineError::usage("Nothing to change: give --title, --start, --location, --attendees or --notes."));
        }
        if let Some(event) = &event
            && command == "calendar.delete"
            && !args.bool("yes")
        {
            let when = match &event.start_day {
                Some(day) => day.clone(),
                None => chrono::DateTime::from_timestamp(unix_seconds(event.start) as i64, 0)
                    .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
            };
            let guests = match event.attendees.iter().filter(|guest| guest.email.as_deref() != Some(account)).count() {
                0 => String::new(),
                1 => ", telling its guest it is cancelled".into(),
                count => format!(", telling its {count} guests it is cancelled"),
            };
            return Err(EngineError::new(
                "confirm",
                format!("Would delete \"{}\" ({when}) from {}{guests}. Re-run with --yes.", event.title, calendar.name),
            ));
        }
        let api = self.mail.api(account).ok_or_else(|| {
            EngineError::new("signed_out", format!("{account} needs to sign in again (Settings > Accounts)."))
        })?;
        let remote = self.remote(&api, record.provider);
        let saved = match (command, &event) {
            ("calendar.create", _) => remote.create(&calendar.id, &fields).await.map_err(provider)?,
            ("calendar.update", Some(event)) => remote.update(&calendar.id, &event.id, &fields).await.map_err(provider)?,
            (_, Some(event)) => {
                remote.delete(&calendar.id, &event.id).await.map_err(provider)?;
                let id = event.id.clone();
                self.changed(account, |copy| copy.events.retain(|kept| !(kept.calendar == calendar.id && kept.id == id)));
                return Ok(json!({ "deleted": true }));
            }
            _ => unreachable!("a change has its event"),
        };
        let answer = answers::json(&saved.item(account, Some(&calendar), editable(&record, Some(&calendar), &saved)));
        self.changed(account, move |copy| {
            copy.events.retain(|kept| !(kept.calendar == saved.calendar && kept.id == saved.id));
            let at = copy.events.partition_point(|kept| kept.start < saved.start);
            copy.events.insert(at, saved);
        });
        Ok(answer)
    }
}
