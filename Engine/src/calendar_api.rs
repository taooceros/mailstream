//! Google Calendar and Outlook calendars (Microsoft Graph) as the calendar store reads them: an
//! account's calendars, and their events within a window with recurring ones expanded into
//! occurrences (Google's `singleEvents`, Graph's `calendarView`). Both answer the same
//! [`CalendarEvent`]s, which mail.db keeps as they are. Adding, changing and deleting events is
//! in [`write`].
use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{self, Api, ApiError, Priority, Request};
use crate::ics::{Invitation, normalized_uid};
use crate::model::{Date, date_from_unix, iso8601, now, parse_iso8601, unix_seconds};
use crate::oauth::Provider;

mod write;

pub use write::{EventFields, local_zone};

/// Google's name for the account's own calendar.
const PRIMARY: &str = "primary";
pub const GOOGLE_ROOT: &str = "https://www.googleapis.com/calendar/v3/";
/// Most pages of events read from one calendar per fetch.
const PAGES: usize = 20;
/// Longest description kept per event.
const NOTES_LIMIT: usize = 4000;

/// One of an account's calendars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: String,
    pub name: String,
    /// `#rrggbb` as the provider shows it; none when it picks no color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The account's own calendar.
    #[serde(default)]
    pub primary: bool,
    /// The account may add and change events on it (Google's owner or writer access, Graph's
    /// `canEdit`).
    #[serde(default)]
    pub writable: bool,
}

/// An organizer or attendee.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// `accepted`, `tentative`, `declined` or `needsAction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

/// One event, or one occurrence of a recurring event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub calendar: String,
    pub title: String,
    /// For all-day events, local midnight of `start_day` and `end_day`.
    pub start: Date,
    pub end: Date,
    #[serde(default)]
    pub all_day: bool,
    /// An all-day event's first day and the day after its last, as `2026-11-08`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The video call to join (Meet, Teams, a link in the location).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Person>,
    /// The account's answer to the invitation: `accepted`, `tentative`, `declined`,
    /// `needsAction`, or `organizer` for its own meeting; none for an event without guests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// The description, as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// `notes` is cut short of the provider's description (Graph keeps a preview of it only), so
    /// it cannot be edited from here without losing the rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notes_cut: bool,
    /// The event in the provider's web calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Its iCalendar UID, as [`crate::ics::normalized_uid`] writes it: what an invitation in
    /// mail names it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// For an occurrence of a recurring event, the series' id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// The account organizes it (Google's organizer `self` on an ordinary event, Graph's
    /// `isOrganizer`): only these may be changed or deleted, on a writable calendar.
    #[serde(default)]
    pub mine: bool,
}

impl CalendarEvent {
    /// As `calendar.events` answers it, with its calendar's name and color.
    pub fn json(&self, account: &str, calendar: Option<&Calendar>) -> Value {
        let mut value = serde_json::to_value(self).expect("events encode");
        value["account"] = json!(account);
        value["start"] = json!(iso8601(self.start));
        value["end"] = json!(iso8601(self.end));
        value["calendarName"] = json!(calendar.map(|calendar| calendar.name.as_str()));
        value["color"] = json!(calendar.and_then(|calendar| calendar.color.as_deref()));
        value
    }
}

/// An account's calendars and their events from `start` to `end`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fetched {
    pub calendars: Vec<Calendar>,
    pub events: Vec<CalendarEvent>,
}

/// `2026-11-08`'s local midnight.
pub fn local_midnight(day: &str) -> Option<Date> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let midnight = Local.from_local_datetime(&date.and_hms_opt(0, 0, 0)?).earliest()?;
    Some(date_from_unix(midnight.timestamp() as f64))
}

/// Today's date here, `days` days on (or back), as `2026-11-08`.
pub fn local_day(days: i64) -> String {
    (Local::now().date_naive() + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()
}

/// The UTC day nearest `date`: an all-day event Graph moved off midnight into UTC lands back on
/// its own day, whichever side of UTC its time zone is.
fn nearest_day(date: Date) -> String {
    let seconds = (unix_seconds(date) + 43_200.0).div_euclid(86_400.0) * 86_400.0;
    iso8601(date_from_unix(seconds))[..10].to_string()
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

/// The description as plain text, and whether writing that text back would lose part of it: cut
/// at [`NOTES_LIMIT`], or links whose addresses the text drops.
fn notes(text: Option<String>, html: bool) -> (Option<String>, bool) {
    let Some(text) = text else { return (None, false) };
    let links = html && text.to_ascii_lowercase().contains("<a ");
    let plain = if html && text.contains('<') { crate::text::strip_html(&text) } else { text };
    let lines: Vec<&str> = plain.lines().map(str::trim_end).collect();
    let joined = lines.join("\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        return (None, false);
    }
    let (kept, cut) = crate::text::prefix_characters(trimmed, NOTES_LIMIT);
    (Some(if cut { format!("{kept}…") } else { kept.to_string() }), cut || links)
}

/// The calls for one account, by its provider.
pub struct CalendarRemote<'a> {
    pub api: &'a Api,
    pub provider: Provider,
    /// Google Calendar's root.
    pub google: &'a str,
    /// Graph's root.
    pub graph: &'a str,
    pub priority: Priority,
}

impl CalendarRemote<'_> {
    /// Every calendar the account shows and their events overlapping `start`…`end`.
    pub async fn fetch(&self, start: Date, end: Date) -> Result<Fetched, ApiError> {
        match self.provider {
            Provider::Google => self.google(start, end).await,
            Provider::Microsoft => self.graph(start, end).await,
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, request: Request) -> Result<T, ApiError> {
        self.api.decode(request, self.priority, 0).await
    }

    /// Fetches each calendar's events; a calendar the account may no longer read (shared ones
    /// taken back) is left out rather than failing the rest.
    async fn each_calendar<F, Fut>(&self, calendars: &[Calendar], events: F) -> Result<Vec<CalendarEvent>, ApiError>
    where
        F: Fn(Calendar) -> Fut,
        Fut: Future<Output = Result<Vec<CalendarEvent>, ApiError>>,
    {
        let results = futures::future::join_all(calendars.iter().cloned().map(events)).await;
        let mut found = Vec::new();
        for result in results {
            match result {
                Ok(events) => found.extend(events),
                Err(error) if matches!(error.status, 403 | 404) => {}
                Err(error) => return Err(error),
            }
        }
        found.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| b.all_day.cmp(&a.all_day)));
        Ok(found)
    }

    // MARK: Google

    async fn google(&self, start: Date, end: Date) -> Result<Fetched, ApiError> {
        let list = |token: Option<&str>| {
            let mut query = vec![("minAccessRole", "reader"), ("maxResults", "250")];
            if let Some(token) = token {
                query.push(("pageToken", token));
            }
            api::url(self.google, "users/me/calendarList", &query)
        };
        let entries = api::pages(
            list(None),
            PAGES,
            |url| async move { self.get::<GoogleList<GoogleCalendar>>(Request::get(url)).await.map(Some) },
            |page| page.next_page_token.as_deref().map(|token| list(Some(token))),
            |page| page.items.unwrap_or_default(),
        )
        .await?
        .unwrap_or_default();
        // The calendars Google Calendar shows: the primary one and those ticked in its list.
        let calendars: Vec<Calendar> = entries
            .into_iter()
            .filter(|entry| entry.primary == Some(true) || (entry.selected == Some(true) && entry.hidden != Some(true)))
            .map(|entry| Calendar {
                name: non_empty(entry.summary_override)
                    .or_else(|| non_empty(entry.summary))
                    .unwrap_or_else(|| entry.id.clone()),
                id: entry.id,
                color: non_empty(entry.background_color),
                primary: entry.primary == Some(true),
                writable: matches!(entry.access_role.as_deref(), Some("owner" | "writer")),
            })
            .collect();
        let (time_min, time_max) = (iso8601(start), iso8601(end));
        let events = self
            .each_calendar(&calendars, |calendar| {
                let (time_min, time_max) = (time_min.clone(), time_max.clone());
                async move {
                    let path = format!("calendars/{}/events", api::percent_encoded(&calendar.id));
                    let page = |token: Option<&str>| {
                        let mut query = vec![
                            ("timeMin", time_min.as_str()),
                            ("timeMax", time_max.as_str()),
                            ("singleEvents", "true"),
                            ("orderBy", "startTime"),
                            ("maxResults", "2500"),
                        ];
                        if let Some(token) = token {
                            query.push(("pageToken", token));
                        }
                        api::url(self.google, &path, &query)
                    };
                    let items = api::pages(
                        page(None),
                        PAGES,
                        |url| async move { self.get::<GoogleList<GoogleEvent>>(Request::get(url)).await.map(Some) },
                        |list| list.next_page_token.as_deref().map(|token| page(Some(token))),
                        |list| list.items.unwrap_or_default(),
                    )
                    .await?
                    .unwrap_or_default();
                    Ok(items.into_iter().filter_map(|item| item.event(&calendar.id)).collect())
                }
            })
            .await?;
        Ok(Fetched { calendars, events })
    }

    // MARK: Graph

    async fn graph(&self, start: Date, end: Date) -> Result<Fetched, ApiError> {
        let first =
            api::url(self.graph, "me/calendars", &[("$top", "100"), ("$select", "id,name,hexColor,isDefaultCalendar,canEdit")]);
        let found = api::pages(
            first,
            PAGES,
            |url| async move { self.get::<GraphList<GraphCalendar>>(Request::get(url)).await.map(Some) },
            |page| page.next.clone(),
            |page| page.value,
        )
        .await?
        .unwrap_or_default();
        let calendars: Vec<Calendar> = found
            .into_iter()
            .map(|calendar| Calendar {
                name: non_empty(calendar.name).unwrap_or_else(|| calendar.id.clone()),
                id: calendar.id,
                color: non_empty(calendar.hex_color),
                primary: calendar.is_default_calendar == Some(true),
                writable: calendar.can_edit == Some(true),
            })
            .collect();
        let (from, to) = (iso8601(start), iso8601(end));
        let events = self
            .each_calendar(&calendars, |calendar| {
                let (from, to) = (from.clone(), to.clone());
                async move {
                    let first = api::url(
                        self.graph,
                        &format!("me/calendars/{}/calendarView", api::percent_encoded(&calendar.id)),
                        &[
                            ("startDateTime", from.as_str()),
                            ("endDateTime", to.as_str()),
                            ("$top", "250"),
                            ("$select", GRAPH_EVENT_FIELDS),
                        ],
                    );
                    let items = api::pages(
                        first,
                        PAGES,
                        |url| async move {
                            let request = Request::get(url).header("Prefer", "outlook.timezone=\"UTC\"");
                            self.get::<GraphList<GraphEvent>>(request).await.map(Some)
                        },
                        |page| page.next.clone(),
                        |page| page.value,
                    )
                    .await?
                    .unwrap_or_default();
                    Ok(items.into_iter().filter_map(|item| item.event(&calendar.id)).collect())
                }
            })
            .await?;
        Ok(Fetched { calendars, events })
    }

    // MARK: Invitations

    /// A request with a JSON body.
    fn json_request(method: reqwest::Method, url: String, body: &Value) -> Request {
        let mut request = Request::post(url, "application/json", serde_json::to_vec(body).expect("JSON encodes"));
        request.method = method;
        request
    }

    /// The event `invitation` names, asked of the provider (one outside the fetched window, or
    /// fetched before the invitation came); nil when the account's calendar does not have it.
    pub async fn find(&self, invitation: &Invitation) -> Result<Option<CalendarEvent>, ApiError> {
        let found = match self.provider {
            Provider::Google => self.google_find(invitation).await,
            Provider::Microsoft => self.graph_find(invitation).await,
        };
        match found {
            Err(error) if matches!(error.status, 404 | 410) => Ok(None),
            found => found,
        }
    }

    async fn google_find(&self, invitation: &Invitation) -> Result<Option<CalendarEvent>, ApiError> {
        let Some(uid) = &invitation.uid else { return Ok(None) };
        let url = api::url(self.google, "calendars/primary/events", &[("iCalUID", uid.as_str())]);
        let list: GoogleList<GoogleEvent> = self.get(Request::get(url)).await?;
        let events: Vec<CalendarEvent> = list.items.unwrap_or_default().into_iter().filter_map(|item| item.event(PRIMARY)).collect();
        let found = matching(&events, invitation).cloned();
        let moved = found.as_ref().is_some_and(|event| (event.start - invitation.start).abs() <= 60.0);
        if !invitation.occurrence || moved {
            return Ok(found);
        }
        // An occurrence answered alone: among its series' instances.
        let Some(series) = events.iter().find(|event| event.series.is_none()) else { return Ok(found) };
        let path = format!("calendars/primary/events/{}/instances", api::percent_encoded(&series.id));
        let (from, to) = (iso8601(invitation.start - 60.0), iso8601(invitation.start + 60.0));
        let url = api::url(self.google, &path, &[("timeMin", from.as_str()), ("timeMax", to.as_str())]);
        let list: GoogleList<GoogleEvent> = self.get(Request::get(url)).await?;
        Ok(list.items.unwrap_or_default().into_iter().find_map(|item| item.event(PRIMARY)).or(found))
    }

    async fn graph_find(&self, invitation: &Invitation) -> Result<Option<CalendarEvent>, ApiError> {
        let utc = |url: String| Request::get(url).header("Prefer", "outlook.timezone=\"UTC\"");
        if let Some(id) = &invitation.event {
            let url = api::url(self.graph, &format!("me/events/{}", api::percent_encoded(id)), &[("$select", GRAPH_EVENT_FIELDS)]);
            return Ok(self.get::<GraphEvent>(utc(url)).await?.event(""));
        }
        if invitation.uid.is_none() {
            return Ok(None);
        }
        // Graph cannot be asked by UID: the days around the meeting, by UID.
        let (from, to) = (iso8601(invitation.start - 86_400.0), iso8601(invitation.end.max(invitation.start) + 86_400.0));
        let url = api::url(
            self.graph,
            "me/calendarView",
            &[("startDateTime", from.as_str()), ("endDateTime", to.as_str()), ("$top", "250"), ("$select", GRAPH_EVENT_FIELDS)],
        );
        let list: GraphList<GraphEvent> = self.get(utc(url)).await?;
        let events: Vec<CalendarEvent> = list.value.into_iter().filter_map(|event| event.event("")).collect();
        Ok(matching(&events, invitation).cloned())
    }

    /// Answers the invitation to `event` in `calendar` (`accepted`, `tentative` or `declined`)
    /// through the calendar, which tells the organizer.
    pub async fn respond(&self, calendar: &str, event: &str, response: &str) -> Result<(), ApiError> {
        let request = match self.provider {
            Provider::Google => {
                let calendar = if calendar.is_empty() { PRIMARY } else { calendar };
                let path = format!("calendars/{}/events/{}", api::percent_encoded(calendar), api::percent_encoded(event));
                // Google takes the guest list whole: the one read, with this account's answer changed.
                let current: Value = self.get(Request::get(api::url(self.google, &path, &[]))).await?;
                let mut attendees = current["attendees"].as_array().cloned().unwrap_or_default();
                let me = attendees.iter_mut().find(|attendee| {
                    attendee["self"].as_bool() == Some(true)
                        || attendee["email"].as_str().is_some_and(|email| email.eq_ignore_ascii_case(&self.api.account))
                });
                let Some(me) = me else {
                    return Err(ApiError::new(400, "This account is not on the meeting's guest list."));
                };
                me["responseStatus"] = json!(response);
                let url = api::url(self.google, &path, &[("sendUpdates", "all")]);
                Self::json_request(reqwest::Method::PATCH, url, &json!({ "attendees": attendees }))
            }
            Provider::Microsoft => {
                let action = match response {
                    "accepted" => "accept",
                    "tentative" => "tentativelyAccept",
                    _ => "decline",
                };
                let url = api::url(self.graph, &format!("me/events/{}/{action}", api::percent_encoded(event)), &[]);
                Self::json_request(reqwest::Method::POST, url, &json!({ "sendResponse": true }))
            }
        };
        self.api.send(request, self.priority, None, 0).await.map(drop)
    }
}

/// The event among `events` that `invitation` names: by Outlook's event id or the meeting's
/// UID; for one occurrence, the one at its time; for a series, its next occurrence.
pub fn matching<'a>(events: &'a [CalendarEvent], invitation: &Invitation) -> Option<&'a CalendarEvent> {
    let uid = invitation.uid.as_deref().map(normalized_uid);
    let named = |event: &&CalendarEvent| {
        invitation.event.as_deref().is_some_and(|id| event.id == id || event.series.as_deref() == Some(id))
            || (uid.is_some() && event.uid == uid)
    };
    let found: Vec<&CalendarEvent> = events.iter().filter(named).collect();
    let at_its_time = found.iter().find(|event| (event.start - invitation.start).abs() <= 60.0);
    let next = || found.iter().filter(|event| event.end > now()).min_by(|a, b| a.start.total_cmp(&b.start));
    if invitation.recurring { next().or(at_its_time) } else { at_its_time.or_else(next) }.or(found.first()).copied()
}

/// The invitation an Outlook meeting message carries (an `eventMessage`, read with its `event`
/// expanded); nil for other messages and for answers to the user's own meetings.
pub fn graph_invitation(message: &Value) -> Option<Invitation> {
    let method = match message["meetingMessageType"].as_str()? {
        "meetingRequest" => "request",
        "meetingCancelled" => "cancel",
        _ => return None,
    };
    let event = serde_json::from_value::<GraphEvent>(message["event"].clone()).ok();
    let kind = event.as_ref().and_then(|event| event.kind.clone()).or_else(|| message["type"].as_str().map(String::from));
    let (recurring, occurrence) = match kind.as_deref() {
        Some("seriesMaster") => (true, false),
        Some("occurrence" | "exception") => (false, true),
        _ => (false, false),
    };
    let id = event.as_ref().map(|event| event.id.clone());
    if let Some(event) = event.and_then(|event| event.event("")) {
        return Some(Invitation {
            method: method.into(),
            uid: event.uid,
            title: event.title,
            start: event.start,
            end: event.end,
            all_day: event.all_day,
            start_day: event.start_day,
            end_day: event.end_day,
            location: event.location,
            organizer: event.organizer,
            recurring,
            occurrence,
            event: id,
        });
    }
    // A cancelled meeting is gone from the calendar: the message's own words.
    let time = |key: &str| serde_json::from_value::<GraphTime>(message[key].clone()).ok()?.date();
    let start = time("startDateTime")?;
    let end = time("endDateTime").unwrap_or(start).max(start);
    let (start, end, start_day, end_day) = if message["isAllDay"].as_bool() == Some(true) {
        let (first, after) = (nearest_day(start), nearest_day(end));
        (local_midnight(&first)?, local_midnight(&after)?, Some(first), Some(after))
    } else {
        (start, end, None, None)
    };
    let organizer = serde_json::from_value::<GraphRecipient>(message["from"].clone()).ok();
    Some(Invitation {
        method: method.into(),
        uid: None,
        title: non_empty(message["subject"].as_str().map(String::from)).unwrap_or_else(|| "(No title)".into()),
        start,
        end,
        all_day: start_day.is_some(),
        start_day,
        end_day,
        location: non_empty(message["location"]["displayName"].as_str().map(String::from)),
        organizer: organizer.map(|organizer| graph_person(organizer.email_address, None)),
        recurring,
        occurrence,
        event: id,
    })
}

// MARK: - Google's answers

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleList<T> {
    items: Option<Vec<T>>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCalendar {
    id: String,
    summary: Option<String>,
    summary_override: Option<String>,
    background_color: Option<String>,
    primary: Option<bool>,
    selected: Option<bool>,
    hidden: Option<bool>,
    /// `owner`, `writer`, `reader` or `freeBusyReader`.
    access_role: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTime {
    date: Option<String>,
    date_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GooglePerson {
    email: Option<String>,
    display_name: Option<String>,
    response_status: Option<String>,
    #[serde(rename = "self")]
    me: Option<bool>,
    resource: Option<bool>,
}

impl GooglePerson {
    fn person(&self) -> Person {
        Person {
            name: non_empty(self.display_name.clone()),
            email: non_empty(self.email.clone()),
            response: non_empty(self.response_status.clone()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleEntryPoint {
    entry_point_type: Option<String>,
    uri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleConference {
    entry_points: Option<Vec<GoogleEntryPoint>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleEvent {
    id: String,
    status: Option<String>,
    /// `default`; `birthday`, `fromGmail`, `outOfOffice` and the like.
    event_type: Option<String>,
    summary: Option<String>,
    location: Option<String>,
    description: Option<String>,
    html_link: Option<String>,
    hangout_link: Option<String>,
    conference_data: Option<GoogleConference>,
    start: Option<GoogleTime>,
    end: Option<GoogleTime>,
    organizer: Option<GooglePerson>,
    attendees: Option<Vec<GooglePerson>>,
    #[serde(rename = "iCalUID")]
    ical_uid: Option<String>,
    recurring_event_id: Option<String>,
}

impl GoogleEvent {
    /// Nil for a cancelled occurrence or one without a time.
    fn event(self, calendar: &str) -> Option<CalendarEvent> {
        if self.status.as_deref() == Some("cancelled") {
            return None;
        }
        let (start, end) = (self.start?, self.end?);
        let (start_value, end_value, start_day, end_day) = match (start.date, end.date) {
            (Some(first), Some(after)) => (local_midnight(&first)?, local_midnight(&after)?, Some(first), Some(after)),
            _ => (parse_iso8601(start.date_time.as_deref()?)?, parse_iso8601(end.date_time.as_deref()?)?, None, None),
        };
        let attendees: Vec<&GooglePerson> =
            self.attendees.iter().flatten().filter(|person| person.resource != Some(true)).collect();
        let organizer_is_me = self.organizer.as_ref().and_then(|organizer| organizer.me) == Some(true);
        // Birthdays, events from Gmail and the like are Google's to change.
        let mine = organizer_is_me && self.event_type.as_deref().is_none_or(|kind| kind == "default");
        let response = match attendees.iter().find(|person| person.me == Some(true)) {
            _ if organizer_is_me && !attendees.is_empty() => Some("organizer".to_string()),
            Some(me) => non_empty(me.response_status.clone()),
            None => None,
        };
        let video = self.conference_data.and_then(|conference| {
            conference.entry_points?.into_iter().find(|point| point.entry_point_type.as_deref() == Some("video"))?.uri
        });
        let (notes, notes_cut) = notes(self.description, true);
        Some(CalendarEvent {
            id: self.id,
            calendar: calendar.to_string(),
            title: non_empty(self.summary).unwrap_or_else(|| "(No title)".into()),
            start: start_value,
            end: end_value.max(start_value),
            all_day: start_day.is_some(),
            start_day,
            end_day,
            location: non_empty(self.location),
            meeting_url: non_empty(self.hangout_link).or(non_empty(video)),
            organizer: self.organizer.as_ref().map(GooglePerson::person),
            attendees: attendees.iter().map(|person| person.person()).collect(),
            response,
            notes,
            notes_cut,
            link: non_empty(self.html_link),
            uid: non_empty(self.ical_uid).map(|uid| crate::ics::normalized_uid(&uid)),
            series: non_empty(self.recurring_event_id),
            mine,
        })
    }
}

// MARK: - Graph's answers

const GRAPH_EVENT_FIELDS: &str = "id,subject,start,end,isAllDay,isCancelled,isOrganizer,location,onlineMeeting,\
onlineMeetingUrl,organizer,attendees,responseStatus,webLink,bodyPreview,iCalUId,seriesMasterId,type";
/// The most characters of the description Graph's `bodyPreview` holds.
const GRAPH_PREVIEW: usize = 255;

#[derive(Deserialize)]
struct GraphList<T> {
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphCalendar {
    id: String,
    name: Option<String>,
    hex_color: Option<String>,
    is_default_calendar: Option<bool>,
    can_edit: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphTime {
    date_time: String,
}

impl GraphTime {
    /// Graph writes `2026-11-08T10:00:00.0000000` in the zone the request asked for: UTC.
    fn date(&self) -> Option<Date> {
        let text = &self.date_time;
        if text.ends_with('Z') || text.contains('+') { parse_iso8601(text) } else { parse_iso8601(&format!("{text}Z")) }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphRecipient {
    email_address: Option<GraphAddress>,
}

#[derive(Deserialize)]
struct GraphStatus {
    response: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphAttendee {
    email_address: Option<GraphAddress>,
    status: Option<GraphStatus>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphLocation {
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMeeting {
    join_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEvent {
    id: String,
    subject: Option<String>,
    start: Option<GraphTime>,
    end: Option<GraphTime>,
    is_all_day: Option<bool>,
    is_cancelled: Option<bool>,
    is_organizer: Option<bool>,
    location: Option<GraphLocation>,
    online_meeting: Option<GraphMeeting>,
    online_meeting_url: Option<String>,
    organizer: Option<GraphRecipient>,
    attendees: Option<Vec<GraphAttendee>>,
    response_status: Option<GraphStatus>,
    web_link: Option<String>,
    body_preview: Option<String>,
    #[serde(rename = "iCalUId")]
    ical_uid: Option<String>,
    series_master_id: Option<String>,
    /// `singleInstance`, `occurrence`, `exception` or `seriesMaster`.
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// Graph's answers in Google's words.
fn graph_response(response: Option<&str>) -> Option<String> {
    Some(
        match response? {
            "accepted" => "accepted",
            "tentativelyAccepted" => "tentative",
            "declined" => "declined",
            "notResponded" => "needsAction",
            "organizer" => "organizer",
            _ => return None,
        }
        .to_string(),
    )
}

fn graph_person(address: Option<GraphAddress>, response: Option<String>) -> Person {
    let (name, email) = address.map_or((None, None), |address| (address.name, address.address));
    Person { name: non_empty(name), email: non_empty(email), response }
}

impl GraphEvent {
    fn event(self, calendar: &str) -> Option<CalendarEvent> {
        if self.is_cancelled == Some(true) {
            return None;
        }
        let (start, end) = (self.start?.date()?, self.end?.date()?);
        let all_day = self.is_all_day == Some(true);
        let (start, end, start_day, end_day) = if all_day {
            let (first, after) = (nearest_day(start), nearest_day(end));
            (local_midnight(&first)?, local_midnight(&after)?, Some(first), Some(after))
        } else {
            (start, end, None, None)
        };
        let attendees: Vec<Person> = self
            .attendees
            .unwrap_or_default()
            .into_iter()
            .filter(|attendee| attendee.kind.as_deref() != Some("resource"))
            .map(|attendee| {
                graph_person(attendee.email_address, graph_response(attendee.status.and_then(|status| status.response).as_deref()))
            })
            .collect();
        let mine = self.is_organizer == Some(true);
        let response = if mine {
            (!attendees.is_empty()).then(|| "organizer".to_string())
        } else {
            graph_response(self.response_status.and_then(|status| status.response).as_deref())
        };
        // Graph previews the description in its first 255 characters.
        let preview = self.body_preview.as_deref().map_or(0, |preview| preview.chars().count());
        let (notes, notes_cut) = notes(self.body_preview, false);
        Some(CalendarEvent {
            id: self.id,
            calendar: calendar.to_string(),
            title: non_empty(self.subject).unwrap_or_else(|| "(No title)".into()),
            start,
            end: end.max(start),
            all_day,
            start_day,
            end_day,
            location: non_empty(self.location.and_then(|location| location.display_name)),
            meeting_url: non_empty(self.online_meeting.and_then(|meeting| meeting.join_url)).or(non_empty(self.online_meeting_url)),
            organizer: self.organizer.map(|organizer| graph_person(organizer.email_address, None)),
            attendees,
            response,
            notes,
            notes_cut: notes_cut || preview >= GRAPH_PREVIEW,
            link: non_empty(self.web_link),
            uid: non_empty(self.ical_uid).map(|uid| crate::ics::normalized_uid(&uid)),
            series: non_empty(self.series_master_id),
            mine,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_all_day_events_keep_their_day_wherever_they_were_made() {
        // Midnight in UTC+5 and UTC-8, as Graph answers in UTC.
        for (start, end) in [("2026-11-07T19:00:00.0000000", "2026-11-08T19:00:00.0000000"), ("2026-11-08T08:00:00.0000000", "2026-11-09T08:00:00.0000000")] {
            let event: GraphEvent = serde_json::from_value(json!({
                "id": "e", "subject": "Holiday", "isAllDay": true,
                "start": { "dateTime": start, "timeZone": "UTC" }, "end": { "dateTime": end, "timeZone": "UTC" },
            }))
            .unwrap();
            let event = event.event("c").unwrap();
            assert_eq!((event.start_day.as_deref(), event.end_day.as_deref()), (Some("2026-11-08"), Some("2026-11-09")));
            assert_eq!(event.start, local_midnight("2026-11-08").unwrap());
        }
    }
}
