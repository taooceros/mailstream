//! Adding, changing and deleting events on Google Calendar and Outlook (Graph), for
//! `calendar.create`, `calendar.update` and `calendar.delete`. Times go to the provider as
//! wall-clock times in a named zone, which it keeps with the event; the event it answers is what
//! mail.db's copy takes. Guests hear of every change (Google's `sendUpdates=all`; Graph tells
//! them itself).
use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde_json::{Map, Value, json};

use super::{CalendarEvent, CalendarRemote, GoogleEvent, GraphEvent};
use crate::EngineError;
use crate::api::{self, ApiError, Request};
use crate::commands::Arguments;
use crate::oauth::Provider;

const DAY: &str = "%Y-%m-%d";
const TIME: &str = "%Y-%m-%dT%H:%M:%S";

/// When an event takes place.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// From `start` to `end`, wall-clock times in the IANA zone `zone`.
    Timed { start: NaiveDateTime, end: NaiveDateTime, zone: String },
    /// Whole days, `first` through `last`.
    AllDay { first: NaiveDate, last: NaiveDate },
}

/// A new event, or what an edit changes: a field left `None` stays as it is; an empty location
/// or notes clears it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventFields {
    pub title: Option<String>,
    pub when: Option<When>,
    pub location: Option<String>,
    pub notes: Option<String>,
    /// The guests' addresses, the whole list after the edit.
    pub attendees: Option<Vec<String>>,
}

/// This computer's time zone (`America/Los_Angeles`), which times are in unless one is named.
pub fn local_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into())
}

fn parse_time(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim();
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
}

fn parse_day(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), DAY).ok()
}

fn usage(message: impl Into<String>) -> EngineError {
    EngineError::new("usage", message)
}

impl EventFields {
    /// The fields `args` give; `new`: for a new event, which needs a title and a start.
    pub fn from_args(args: &Arguments, new: bool) -> Result<Self, EngineError> {
        let title = args.string("title").map(|title| title.trim().to_string());
        if title.as_deref() == Some("") || (new && title.is_none()) {
            return Err(usage("An event needs a title."));
        }
        let all_day = args.bool("all-day");
        let when = match args.string("start") {
            None if all_day || args.string("end").is_some() || args.string("time-zone").is_some() => {
                return Err(usage("--end, --all-day and --time-zone go with --start."));
            }
            None if new => return Err(usage("An event needs a --start.")),
            None => None,
            Some(start) if all_day => {
                let first = parse_day(&start).ok_or_else(|| usage("An all-day event's --start is a day: 2026-11-08."))?;
                let last = match args.string("end") {
                    Some(end) => parse_day(&end).ok_or_else(|| usage("An all-day event's --end is its last day: 2026-11-09."))?,
                    None => first,
                };
                if last < first {
                    return Err(usage("An event cannot end before it starts."));
                }
                Some(When::AllDay { first, last })
            }
            Some(start) => {
                let start = parse_time(&start).ok_or_else(|| usage("--start is a time: 2026-11-08T10:00."))?;
                let end = match args.string("end") {
                    Some(end) => parse_time(&end).ok_or_else(|| usage("--end is a time: 2026-11-08T11:00."))?,
                    None => start + Duration::hours(1),
                };
                if end < start {
                    return Err(usage("An event cannot end before it starts."));
                }
                let zone = args.string("time-zone").map(|zone| zone.trim().to_string()).filter(|zone| !zone.is_empty());
                Some(When::Timed { start, end, zone: zone.unwrap_or_else(local_zone) })
            }
        };
        let attendees = match args.list("attendees") {
            Some(list) => {
                let mut kept: Vec<String> = Vec::new();
                for address in list.iter().map(|address| address.trim()).filter(|address| !address.is_empty()) {
                    if !address.contains('@') || address.contains(char::is_whitespace) {
                        return Err(usage(format!("\"{address}\" is not an email address.")));
                    }
                    if !kept.iter().any(|known| known.eq_ignore_ascii_case(address)) {
                        kept.push(address.to_string());
                    }
                }
                Some(kept)
            }
            None => None,
        };
        Ok(Self {
            title,
            when,
            location: args.string("location").map(|location| location.trim().to_string()),
            notes: args.string("notes").map(|notes| notes.trim_end().to_string()),
            attendees,
        })
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The guest entries to send for `addresses`: the event's current ones (`existing`, as the
/// provider wrote them, with their answers) for those kept and those `kept` always keeps (rooms,
/// the organizer's own entry), new ones made by `entry`.
fn guests(
    addresses: &[String],
    existing: &[Value],
    email: fn(&Value) -> Option<&str>,
    kept: impl Fn(&Value) -> bool,
    entry: impl Fn(&str) -> Value,
) -> Value {
    let listed = |guest: &Value| email(guest).is_some_and(|email| addresses.iter().any(|address| address.eq_ignore_ascii_case(email)));
    let mut out: Vec<Value> = existing.iter().filter(|guest| kept(guest) || listed(guest)).cloned().collect();
    for address in addresses {
        if !existing.iter().any(|guest| email(guest).is_some_and(|email| email.eq_ignore_ascii_case(address))) {
            out.push(entry(address));
        }
    }
    Value::Array(out)
}

/// Google's event resource for `fields`; `patch` clears the other kind of time when the event
/// changes between timed and all-day.
fn google_body(fields: &EventFields, existing_guests: &[Value], patch: bool) -> Value {
    let mut body = Map::new();
    if let Some(title) = &fields.title {
        body.insert("summary".into(), json!(title));
    }
    match &fields.when {
        Some(When::Timed { start, end, zone }) => {
            let time = |time: &NaiveDateTime| {
                let mut value = json!({ "dateTime": time.format(TIME).to_string(), "timeZone": zone });
                if patch {
                    value["date"] = Value::Null;
                }
                value
            };
            body.insert("start".into(), time(start));
            body.insert("end".into(), time(end));
        }
        Some(When::AllDay { first, last }) => {
            let day = |day: NaiveDate| {
                let mut value = json!({ "date": day.format(DAY).to_string() });
                if patch {
                    value["dateTime"] = Value::Null;
                    value["timeZone"] = Value::Null;
                }
                value
            };
            body.insert("start".into(), day(*first));
            body.insert("end".into(), day(*last + Duration::days(1)));
        }
        None => {}
    }
    if let Some(location) = &fields.location {
        body.insert("location".into(), json!(location));
    }
    if let Some(notes) = &fields.notes {
        body.insert("description".into(), json!(notes));
    }
    if let Some(addresses) = &fields.attendees {
        let kept = |guest: &Value| [&guest["resource"], &guest["self"], &guest["organizer"]].iter().any(|flag| flag.as_bool() == Some(true));
        let email: fn(&Value) -> Option<&str> = |guest| guest["email"].as_str();
        body.insert("attendees".into(), guests(addresses, existing_guests, email, kept, |email| json!({ "email": email })));
    }
    Value::Object(body)
}

/// Graph's event for `fields`. All-day events run from midnight to midnight in this computer's
/// zone, as Outlook makes them.
fn graph_body(fields: &EventFields, existing_guests: &[Value]) -> Value {
    let mut body = Map::new();
    if let Some(title) = &fields.title {
        body.insert("subject".into(), json!(title));
    }
    let time = |time: NaiveDateTime, zone: &str| json!({ "dateTime": time.format(TIME).to_string(), "timeZone": zone });
    match &fields.when {
        Some(When::Timed { start, end, zone }) => {
            body.insert("start".into(), time(*start, zone));
            body.insert("end".into(), time(*end, zone));
            body.insert("isAllDay".into(), json!(false));
        }
        Some(When::AllDay { first, last }) => {
            let zone = local_zone();
            let midnight = |day: NaiveDate| day.and_hms_opt(0, 0, 0).expect("midnight exists");
            body.insert("start".into(), time(midnight(*first), &zone));
            body.insert("end".into(), time(midnight(*last + Duration::days(1)), &zone));
            body.insert("isAllDay".into(), json!(true));
        }
        None => {}
    }
    if let Some(location) = &fields.location {
        body.insert("location".into(), json!({ "displayName": location }));
    }
    if let Some(notes) = &fields.notes {
        body.insert("body".into(), json!({ "contentType": "text", "content": notes }));
    }
    if let Some(addresses) = &fields.attendees {
        let kept = |guest: &Value| guest["type"].as_str() == Some("resource");
        let email: fn(&Value) -> Option<&str> = |guest| guest["emailAddress"]["address"].as_str();
        let entry = |email: &str| json!({ "emailAddress": { "address": email }, "type": "required" });
        body.insert("attendees".into(), guests(addresses, existing_guests, email, kept, entry));
    }
    Value::Object(body)
}

impl CalendarRemote<'_> {
    fn google_event_url(&self, calendar: &str, id: Option<&str>, query: &[(&str, &str)]) -> String {
        let mut path = format!("calendars/{}/events", api::percent_encoded(calendar));
        if let Some(id) = id {
            path += &format!("/{}", api::percent_encoded(id));
        }
        api::url(self.google, &path, query)
    }

    fn graph_event_url(&self, id: &str, query: &[(&str, &str)]) -> String {
        api::url(self.graph, &format!("me/events/{}", api::percent_encoded(id)), query)
    }

    async fn google_event(&self, request: Request, calendar: &str) -> Result<CalendarEvent, ApiError> {
        let event: GoogleEvent = self.get(request).await?;
        event.event(calendar).ok_or_else(|| ApiError::malformed("Google Calendar answered an event without a time."))
    }

    /// Sends a Graph write and reads the event back in UTC, as the fetch reads it: Graph answers
    /// a write in the zone the request named, whatever `Prefer` asks.
    async fn graph_event(&self, request: Request, calendar: &str) -> Result<CalendarEvent, ApiError> {
        let written: Value = self.get(request).await?;
        let id = written["id"].as_str().ok_or_else(|| ApiError::malformed("Outlook answered an event without an id."))?;
        let url = self.graph_event_url(id, &[("$select", super::GRAPH_EVENT_FIELDS)]);
        let event: GraphEvent = self.get(Request::get(url).header("Prefer", "outlook.timezone=\"UTC\"")).await?;
        event.event(calendar).ok_or_else(|| ApiError::malformed("Outlook answered an event without a time."))
    }

    /// The guests the event has now, as the provider writes them.
    async fn current_guests(&self, calendar: &str, id: &str) -> Result<Vec<Value>, ApiError> {
        let url = match self.provider {
            Provider::Google => self.google_event_url(calendar, Some(id), &[("fields", "attendees")]),
            Provider::Microsoft => self.graph_event_url(id, &[("$select", "attendees")]),
        };
        let event: Value = self.get(Request::get(url)).await?;
        Ok(event["attendees"].as_array().cloned().unwrap_or_default())
    }

    /// Adds an event to `calendar` and invites its guests; it answers the event as the provider
    /// made it. Never sent twice: a retry could add it twice.
    pub async fn create(&self, calendar: &str, fields: &EventFields) -> Result<CalendarEvent, ApiError> {
        match self.provider {
            Provider::Google => {
                let url = self.google_event_url(calendar, None, &[("sendUpdates", "all")]);
                let request = Self::json_request(reqwest::Method::POST, url, &google_body(fields, &[], false));
                self.google_event(request, calendar).await
            }
            Provider::Microsoft => {
                let url = api::url(self.graph, &format!("me/calendars/{}/events", api::percent_encoded(calendar)), &[]);
                self.graph_event(Self::json_request(reqwest::Method::POST, url, &graph_body(fields, &[])), calendar).await
            }
        }
    }

    /// Changes what `fields` give of event `id` on `calendar`. A new guest list keeps the
    /// answers of the guests who stay, and the rooms.
    pub async fn update(&self, calendar: &str, id: &str, fields: &EventFields) -> Result<CalendarEvent, ApiError> {
        let existing = if fields.attendees.is_some() { self.current_guests(calendar, id).await? } else { Vec::new() };
        match self.provider {
            Provider::Google => {
                let url = self.google_event_url(calendar, Some(id), &[("sendUpdates", "all")]);
                let request = Self::json_request(reqwest::Method::PATCH, url, &google_body(fields, &existing, true));
                self.google_event(request, calendar).await
            }
            Provider::Microsoft => {
                let url = self.graph_event_url(id, &[]);
                self.graph_event(Self::json_request(reqwest::Method::PATCH, url, &graph_body(fields, &existing)), calendar).await
            }
        }
    }

    /// Deletes event `id` from `calendar`, telling its guests; one already gone counts as deleted.
    pub async fn delete(&self, calendar: &str, id: &str) -> Result<(), ApiError> {
        let url = match self.provider {
            Provider::Google => self.google_event_url(calendar, Some(id), &[("sendUpdates", "all")]),
            Provider::Microsoft => self.graph_event_url(id, &[]),
        };
        let mut request = Request::get(url);
        request.method = reqwest::Method::DELETE;
        match self.api.send(request, self.priority, None, 0).await {
            Ok(_) => Ok(()),
            Err(error) if matches!(error.status, 404 | 410) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_guest_list_keeps_the_answers_of_those_who_stay_the_organizer_and_the_rooms() {
        let existing = [
            json!({ "email": "me@x.com", "organizer": true, "self": true, "responseStatus": "accepted" }),
            json!({ "email": "Ann@x.com", "responseStatus": "accepted" }),
            json!({ "email": "bo@x.com", "responseStatus": "declined" }),
            json!({ "email": "room@resource", "resource": true }),
        ];
        let fields = EventFields { attendees: Some(vec!["ann@x.com".into(), "cy@x.com".into()]), ..EventFields::default() };
        assert_eq!(
            google_body(&fields, &existing, true)["attendees"],
            json!([
                { "email": "me@x.com", "organizer": true, "self": true, "responseStatus": "accepted" },
                { "email": "Ann@x.com", "responseStatus": "accepted" },
                { "email": "room@resource", "resource": true },
                { "email": "cy@x.com" },
            ])
        );
        let existing = [json!({ "type": "required", "emailAddress": { "address": "bo@x.com" }, "status": { "response": "accepted" } })];
        let fields = EventFields { attendees: Some(vec!["BO@x.com".into(), "cy@x.com".into()]), ..EventFields::default() };
        assert_eq!(
            graph_body(&fields, &existing)["attendees"],
            json!([existing[0], { "emailAddress": { "address": "cy@x.com" }, "type": "required" }])
        );
    }

    #[test]
    fn switching_between_timed_and_all_day_clears_the_other_kind_of_time() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let all_day = EventFields { when: Some(When::AllDay { first: day, last: day }), ..EventFields::default() };
        assert_eq!(google_body(&all_day, &[], true)["end"], json!({ "date": "2026-11-09", "dateTime": null, "timeZone": null }));
        assert_eq!(google_body(&all_day, &[], false)["start"], json!({ "date": "2026-11-08" }));
        let timed = EventFields {
            when: Some(When::Timed {
                start: day.and_hms_opt(10, 0, 0).unwrap(),
                end: day.and_hms_opt(10, 30, 0).unwrap(),
                zone: "Europe/Berlin".into(),
            }),
            ..EventFields::default()
        };
        assert_eq!(
            google_body(&timed, &[], true)["start"],
            json!({ "dateTime": "2026-11-08T10:00:00", "timeZone": "Europe/Berlin", "date": null })
        );
        let graph = graph_body(&timed, &[]);
        assert_eq!(
            (graph["end"].clone(), graph["isAllDay"].clone()),
            (json!({ "dateTime": "2026-11-08T10:30:00", "timeZone": "Europe/Berlin" }), json!(false))
        );
        let graph = graph_body(&all_day, &[]);
        assert_eq!((graph["end"]["dateTime"].as_str(), graph["isAllDay"].as_bool()), (Some("2026-11-09T00:00:00"), Some(true)));
    }
}
