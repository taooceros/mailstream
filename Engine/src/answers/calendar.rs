//! Calendar answers: accounts with their calendars, events and meeting invitations.
use serde::{Deserialize, Serialize};

use super::Provider;
pub use crate::calendar_api::{CalendarInfo, CalendarPerson};

/// An account as the calendar shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct CalendarAccount {
    pub id: String,
    pub label: String,
    pub provider: Provider,
    /// Whether its sign-in grants calendar access; until the person connects Calendar for it
    /// (`accounts.connect`), it has none.
    pub granted: bool,
    /// Whether its sign-in lets it add, change and delete events.
    #[serde(default)]
    pub writable: bool,
    pub loading: bool,
    /// Why its last fetch failed; its events are the ones fetched before.
    pub problem: Option<String>,
    /// When its events were fetched (RFC 3339).
    pub fetched: Option<String>,
    /// Every calendar in its list, its own and others' (shared or subscribed); only those
    /// `shown` have events here.
    pub calendars: Vec<CalendarInfo>,
}

/// One event, or one occurrence of a recurring event. (Not `CalendarEvent`: Dart names the
/// FFI's `Event::Calendar` variant that.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    pub account: String,
    pub calendar: String,
    pub calendar_name: Option<String>,
    /// The calendar's color, `#rrggbb`.
    pub color: Option<String>,
    pub id: String,
    pub title: String,
    /// RFC 3339 in UTC; for all-day events, local midnight of `start_day` and `end_day`.
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
    /// An all-day event's first day and the day after its last: `2026-11-08`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The video call to join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<CalendarPerson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<CalendarPerson>,
    /// The account's answer: `accepted`, `tentative`, `declined`, `needsAction`, or
    /// `organizer` for its own meeting; none for an event without guests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// The description, as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// `notes` is shorter than the provider's description, so editing them here would lose the
    /// rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notes_cut: bool,
    /// The event in the provider's web calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Its iCalendar UID, what an invitation in mail names it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// For an occurrence of a recurring event, the series' id: a change or deletion touches
    /// that occurrence alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// The account organizes it.
    #[serde(default)]
    pub mine: bool,
    /// The account organizes it on a calendar it may write to, with a sign-in that may change
    /// events: `calendar.update` and `calendar.delete` take it. (Left out of
    /// `calendar.invitation`'s JSON.)
    #[serde(default)]
    pub editable: bool,
    /// On one of the account's own calendars, so `response` is its own answer, which
    /// `calendar.respond` gives. On someone else's (shared with it or subscribed to) the answer
    /// is theirs, which only they give.
    #[serde(default)]
    pub own: bool,
}

/// Every account and the events of the days asked for, earliest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAgenda {
    pub accounts: Vec<CalendarAccount>,
    pub loading: bool,
    pub events: Vec<CalendarItem>,
    /// This computer's time zone (`America/Los_Angeles`), which new events' times are in.
    pub time_zone: String,
    /// The days the engine fetches; days outside them show no events because none are fetched.
    pub synced: CalendarDays,
}

/// Days from `from` up to, not including, `until`, as `2026-11-08`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct CalendarDays {
    pub from: String,
    pub until: String,
}

/// A meeting invitation (or its cancellation) as a message states it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct MeetingInvitation {
    /// `request`, or `cancel` for a cancelled meeting.
    pub method: String,
    /// The meeting's iCalendar UID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub title: String,
    /// RFC 3339 in UTC; for all-day events, local midnight of `start_day` and `end_day`.
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<CalendarPerson>,
    /// A recurring meeting, answered as a whole.
    #[serde(default)]
    pub recurring: bool,
    /// One occurrence of a recurring meeting.
    #[serde(default)]
    pub occurrence: bool,
    /// The event's id in the account's calendar, when the provider names it (Outlook's meeting
    /// requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// A message's invitation and the event it names in its account's calendar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct InvitationStatus {
    pub account: String,
    pub invitation: Option<MeetingInvitation>,
    /// Whether the account's sign-in grants calendar access.
    pub granted: bool,
    /// Whether it may answer (change events): Calendar is connected in full.
    pub editable: bool,
    /// The event, with the account's answer (`response`); none when the calendar lacks it.
    pub event: Option<CalendarItem>,
    /// What `calendar.respond` takes: the event's calendar and the event an answer goes to (a
    /// recurring meeting's series).
    pub calendar: Option<String>,
    pub target: Option<String>,
    /// Why the calendar could not be asked.
    pub problem: Option<String>,
}
