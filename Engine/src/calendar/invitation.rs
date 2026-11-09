//! Meeting invitations in mail and their answers: `calendar.invitation` finds the event a
//! message's invitation names in the account's calendar (the fetched copy, else the provider,
//! remembered for [`FRESH`]) with the account's answer; `calendar.respond` answers through the
//! calendar's own API (Google Calendar, Graph), which tells the organizer, so no reply is
//! mailed from here.
use std::sync::Arc;

use serde_json::{Value, json};

use super::{CalendarStore, FRESH};
use crate::accounts::AccountRecord;
use crate::calendar_api::{CalendarEvent, matching};
use crate::commands::Arguments;
use crate::ics::{Invitation, normalized_uid};
use crate::model::now;
use crate::EngineError;

/// The answers `calendar.respond` gives, in Google's words.
pub const RESPONSES: &[&str] = &["accepted", "tentative", "declined"];

/// What remembers a provider's answer about one meeting.
fn lookup_key(invitation: &Invitation) -> Option<String> {
    let uid = invitation.uid.as_deref().map(normalized_uid);
    let named = invitation.event.clone().or(uid)?;
    Some(format!("{named}|{}|{}", invitation.start, invitation.occurrence))
}

impl CalendarStore {
    /// `calendar.invitation`: the invitation message `message` of conversation `id` carries,
    /// whether its account may read and change its calendar, and the event it names there with
    /// the account's answer (`event.response`); `target` is the event an answer goes to (the
    /// series for a recurring meeting).
    pub(super) async fn invitation(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let (account, invitation) = self.mail.invitation(&args.required("id")?, &args.required("message")?).await?;
        let record = self.accounts.record(&account);
        let granted = record.as_ref().is_some_and(AccountRecord::has_calendars);
        let mut answer = json!({
            "account": account,
            "invitation": invitation.as_ref().map(Invitation::json),
            "granted": granted,
            "editable": record.as_ref().is_some_and(AccountRecord::can_edit_calendars),
            "event": null, "calendar": null, "target": null, "problem": null,
        });
        let (Some(invitation), Some(record)) = (invitation, record.filter(|_| granted)) else { return Ok(answer) };
        let (event, calendar) = match self.find(&record, &invitation).await {
            Ok(found) => found,
            Err(message) => {
                answer["problem"] = json!(message);
                return Ok(answer);
            }
        };
        let Some(event) = event else { return Ok(answer) };
        let target = if invitation.recurring { event.series.clone().unwrap_or_else(|| event.id.clone()) } else { event.id.clone() };
        answer["calendar"] = json!(event.calendar);
        answer["target"] = json!(target);
        answer["event"] = event.json(&account, calendar.as_ref());
        Ok(answer)
    }

    /// The event `invitation` names in `record`'s calendar, with that calendar: from the
    /// fetched copy, else as the provider last answered within [`FRESH`], else asked now.
    async fn find(
        &self,
        record: &AccountRecord,
        invitation: &Invitation,
    ) -> Result<(Option<CalendarEvent>, Option<crate::calendar_api::Calendar>), String> {
        let key = lookup_key(invitation).map(|key| (record.id.clone(), key));
        {
            let state = self.state();
            if let Some(copy) = state.accounts.get(&record.id).and_then(|account| account.copy.as_ref())
                && let Some(event) = matching(&copy.events, invitation)
            {
                let calendar = copy.calendars.iter().find(|calendar| calendar.id == event.calendar).cloned();
                return Ok((Some(event.clone()), calendar));
            }
            let Some(key) = &key else { return Ok((None, None)) };
            if let Some((asked, event)) = state.lookups.get(key)
                && now() - asked < FRESH
            {
                return Ok((event.clone(), None));
            }
        }
        let Some(api) = self.mail.api(&record.id) else {
            return Err(format!("{} needs to sign in again (Settings > Accounts).", record.id));
        };
        let event = self.remote(&api, record.provider).find(invitation).await.map_err(|error| error.message)?;
        if let Some(key) = key {
            self.state().lookups.insert(key, (now(), event.clone()));
        }
        Ok((event, None))
    }

    /// `calendar.respond`: answers the invitation to `event` in `calendar` of `account`
    /// (`accepted`, `tentative` or `declined`), then shows the answer in the calendar at once
    /// and fetches the account again.
    pub(super) async fn respond(self: &Arc<Self>, account: Option<&str>, args: &Arguments) -> Result<Value, EngineError> {
        let account = account.ok_or_else(|| EngineError::usage("account is required."))?.to_string();
        let event = args.required("event")?;
        let calendar = args.string("calendar").unwrap_or_default();
        let response = args.required("response")?;
        if !RESPONSES.contains(&response.as_str()) {
            return Err(EngineError::usage("response must be accepted, tentative or declined."));
        }
        let record = self.accounts.record(&account).filter(AccountRecord::can_edit_calendars).ok_or_else(|| {
            EngineError::new("no_calendar", format!("{account} has not granted changing its calendar; sign in to it again."))
        })?;
        let api = self.mail.api(&account).ok_or_else(|| {
            EngineError::new("signed_out", format!("{account} needs to sign in again (Settings > Accounts)."))
        })?;
        if !args.bool("yes") {
            return Err(EngineError::new(
                "confirm",
                format!("Would answer {response} to event {event} for {account}, telling its organizer. Re-run with --yes."),
            ));
        }
        self.remote(&api, record.provider)
            .respond(&calendar, &event, &response)
            .await
            .map_err(|error| EngineError::new("respond_failed", error.message))?;
        let answered = |found: &mut CalendarEvent| {
            if found.id == event || found.series.as_deref() == Some(event.as_str()) {
                found.response = Some(response.clone());
                for attendee in &mut found.attendees {
                    if attendee.email.as_deref().is_some_and(|email| email.eq_ignore_ascii_case(&account)) {
                        attendee.response = Some(response.clone());
                    }
                }
            }
        };
        {
            let mut state = self.state();
            for ((owner, _), (_, found)) in state.lookups.iter_mut() {
                if owner == &account
                    && let Some(found) = found
                {
                    answered(found);
                }
            }
        }
        self.changed(&account, |copy| copy.events.iter_mut().for_each(answered));
        Ok(json!({ "responded": response }))
    }
}
