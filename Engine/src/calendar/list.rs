//! `calendar.add` and `calendar.remove` (MS-310): someone else's Google calendar, shared with the
//! account or public, added to its list (a person's address names their own calendar), and one
//! the account doesn't own taken off it. Google keeps the list, so the change reaches Google
//! Calendar too; the copy changes at once (`changed`), then the account is fetched again, which
//! brings an added calendar's events.
use std::sync::Arc;

use serde_json::{Value, json};

use super::CalendarStore;
use crate::accounts::AccountRecord;
use crate::answers;
use crate::commands::Arguments;
use crate::{EngineError, Provider};

impl CalendarStore {
    /// `account`'s record, when its sign-in lets it change its calendar list.
    fn list_owner(&self, account: &str) -> Result<AccountRecord, EngineError> {
        let record = self.accounts.record(account).expect("run checked the account");
        if record.provider != Provider::Google {
            return Err(EngineError::new(
                "unsupported",
                format!("{account} is an Outlook account: add or remove others' calendars in Outlook."),
            ));
        }
        if !record.has_calendars() || !record.can_change_calendar_list() {
            return Err(EngineError::new(
                "no_calendar",
                format!("{account} has not allowed changing its calendar list; connect Calendar for it again (Settings > Accounts)."),
            ));
        }
        Ok(record)
    }

    fn api_of(&self, account: &str) -> Result<Arc<crate::api::Api>, EngineError> {
        self.mail
            .api(account)
            .ok_or_else(|| EngineError::new("signed_out", format!("{account} needs to sign in again (Settings > Accounts).")))
    }

    /// `calendar.add`: adds calendar `calendar` to `account`'s list, shown; answers it as
    /// `calendar events` lists it. One listed already is shown and answered as it is.
    pub(super) async fn add(self: &Arc<Self>, account: &str, args: &Arguments) -> Result<Value, EngineError> {
        let record = self.list_owner(account)?;
        let id = args.required("calendar")?.trim().to_string();
        if id.is_empty() {
            return Err(EngineError::usage("calendar must name a calendar: a person's email address or a calendar id."));
        }
        let listed = {
            let state = self.state();
            let copy = state.accounts.get(account).and_then(|entry| entry.copy.as_ref());
            copy.and_then(|copy| copy.calendars.iter().find(|calendar| calendar.id.eq_ignore_ascii_case(&id)).cloned())
        };
        if let Some(listed) = listed {
            if !listed.shown {
                self.show(account, &listed.id, true)?;
            }
            return Ok(answers::json(&crate::calendar_api::CalendarInfo { shown: true, ..listed }));
        }
        let api = self.api_of(account)?;
        let added = self.remote(&api, record.provider).add_calendar(&id).await.map_err(|error| match error.status {
            // Google answers 404 for a calendar it doesn't have and for one not shared with the account.
            404 => EngineError::new(
                "not_found",
                format!("{account} cannot see a calendar {id}: its owner must share it with {account} (or make it public) first."),
            ),
            _ => EngineError::new("provider", error.message),
        })?;
        let answer = answers::json(&added);
        self.changed(account, move |copy| {
            copy.calendars.retain(|calendar| calendar.id != added.id);
            copy.choices.insert(added.id.clone(), true);
            copy.calendars.push(added);
        });
        Ok(answer)
    }

    /// `calendar.remove`: takes `calendar`, one `account` doesn't own, off its list, with its
    /// events and the choice to show it.
    pub(super) async fn remove(self: &Arc<Self>, account: &str, args: &Arguments) -> Result<Value, EngineError> {
        let record = self.list_owner(account)?;
        let id = args.required("calendar")?;
        let calendar = {
            let state = self.state();
            let copy = state.accounts.get(account).and_then(|entry| entry.copy.as_ref());
            copy.and_then(|copy| copy.calendars.iter().find(|calendar| calendar.id == id).cloned())
        }
        .ok_or_else(|| EngineError::new("not_found", format!("{account} has no calendar {id}; `mailstream calendar events` lists them.")))?;
        if calendar.owned() {
            return Err(EngineError::new(
                "own_calendar",
                format!("{} is {account}'s own calendar; only calendars shared with it or subscribed to can be removed.", calendar.name),
            ));
        }
        if !args.bool("yes") {
            return Err(EngineError::new(
                "confirm",
                format!(
                    "Would remove {} from {account}'s calendars, in Google Calendar too; its owner keeps it, and `calendar add` brings it back. Re-run with --yes.",
                    calendar.name
                ),
            ));
        }
        let api = self.api_of(account)?;
        self.remote(&api, record.provider).remove_calendar(&id).await.map_err(|error| EngineError::new("provider", error.message))?;
        self.changed(account, |copy| {
            copy.calendars.retain(|kept| kept.id != id);
            copy.choices.remove(&id);
            copy.parts.remove(&id);
        });
        self.cache.remove_calendar_part(account, &id);
        Ok(json!({ "removed": true }))
    }
}
