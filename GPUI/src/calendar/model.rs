//! What the calendar shows, worked out from the engine's events (Flutter's `calendar.dart` and
//! `calendar_grid.dart` helpers): when each event runs in local time, the agenda's days, the
//! meeting about to start, how a day's timed events sit side by side and how long they book it,
//! and what the account can answer.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Days, Duration, Local, NaiveDate, TimeZone, Timelike};
use gpui::{Hsla, SharedString, rgb};
use mailstream::{CalendarAccount, CalendarItem, CalendarPerson};

/// How long before a meeting the agenda offers to join it.
pub const UP_NEXT_LEAD: Duration = Duration::minutes(30);

/// Minutes in a day by the clock.
const DAY_MINUTES: i64 = 24 * 60;

/// The shortest an event is drawn, so a short one still has room for its title.
pub const MIN_EVENT_MINUTES: i64 = 20;

/// One event, by account, calendar and its id there.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EventKey {
    pub account: SharedString,
    pub calendar: SharedString,
    pub id: SharedString,
}

/// An event as the calendar shows it: the engine's item and when it runs, local time; all-day
/// ones from the midnight of their first day to the midnight after their last.
pub struct Event {
    pub item: CalendarItem,
    pub key: EventKey,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
}

impl Event {
    /// None when the engine's times don't read.
    pub fn new(item: CalendarItem) -> Option<Event> {
        let days = (item.start_day.as_deref(), item.end_day.as_deref());
        let (start, end) = match days {
            (Some(first), Some(after)) if item.all_day => {
                (midnight(first.parse().ok()?), midnight(after.parse().ok()?))
            }
            _ => (parse_time(&item.start)?, parse_time(&item.end)?),
        };
        let key = EventKey {
            account: item.account.clone().into(),
            calendar: item.calendar.clone().into(),
            id: item.id.clone().into(),
        };
        Some(Event { item, key, start, end })
    }

    /// The last day it covers: ending at midnight, the day before; a moment-long one, its own.
    pub fn last_day(&self) -> NaiveDate {
        if self.end > self.start { (self.end - Duration::microseconds(1)).date_naive() } else { self.start.date_naive() }
    }

    pub fn declined(&self) -> bool {
        self.item.response.as_deref() == Some("declined")
    }

    /// It goes in the all-day row over the hours: all-day events and those lasting a day or more.
    pub fn in_all_day_row(&self) -> bool {
        self.item.all_day || self.end - self.start >= Duration::hours(24)
    }
}

fn parse_time(time: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(time).ok().map(|time| time.with_timezone(&Local))
}

/// `day`'s local midnight (the first moment of it, where daylight saving skips midnight).
pub fn midnight(day: NaiveDate) -> DateTime<Local> {
    let start = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    Local.from_local_datetime(&start).earliest().unwrap_or_else(|| Local.from_utc_datetime(&start))
}

/// The day after `day`.
pub fn next_day(day: NaiveDate) -> NaiveDate {
    day + Days::new(1)
}

/// The first day of the week `day` is in: Sunday, as weeks start in the US.
pub fn week_of(day: NaiveDate) -> NaiveDate {
    day - Days::new(u64::from(day.weekday().num_days_from_sunday()))
}

/// The seven days from `start`.
pub fn week_days(start: NaiveDate) -> [NaiveDate; 7] {
    std::array::from_fn(|i| start + Days::new(i as u64))
}

/// The days from `from` on that have events, each event (an index into `events`) under every day
/// it covers, earliest first; on each day the all-day ones first, then by start.
pub fn agenda_days(events: &[Event], from: NaiveDate) -> Vec<(NaiveDate, Vec<usize>)> {
    let mut by_day: BTreeMap<NaiveDate, Vec<usize>> = BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        let last = event.last_day();
        let mut day = event.start.date_naive().max(from);
        while day <= last {
            by_day.entry(day).or_default().push(index);
            day = next_day(day);
        }
    }
    by_day
        .into_iter()
        .map(|(day, mut indices)| {
            indices.sort_by(|&a, &b| {
                let (a, b) = (&events[a], &events[b]);
                b.item.all_day.cmp(&a.item.all_day).then(a.start.cmp(&b.start))
            });
            (day, indices)
        })
        .collect()
}

/// The events `days` lists, in the order ↑ and ↓ step through them: day by day as listed, an
/// event over several days once, on its first day listed; each with that day.
pub fn in_order(days: &[(NaiveDate, Vec<usize>)]) -> Vec<(usize, NaiveDate)> {
    let mut seen = std::collections::HashSet::new();
    days.iter()
        .flat_map(|(day, indices)| indices.iter().map(move |&index| (index, *day)))
        .filter(|(index, _)| seen.insert(*index))
        .collect()
}

/// The event `step` after (1) or before (-1) `opened` in `order` ([`in_order`]), staying at the
/// ends; with none open, or the open one not in it, the first (or the last).
pub fn stepped(order: &[usize], events: &[Event], opened: Option<&EventKey>, step: isize) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    let at = opened.and_then(|key| order.iter().position(|&index| &events[index].key == key));
    let target = match at {
        None => {
            if step > 0 {
                0
            } else {
                order.len() - 1
            }
        }
        Some(at) => (at as isize + step).clamp(0, order.len() as isize - 1) as usize,
    };
    Some(order[target])
}

/// An agenda day's header: Today, Tomorrow, else the weekday and date.
pub fn agenda_title(day: NaiveDate, today: NaiveDate) -> String {
    let date = day.format("%A, %B %-d");
    if day == today {
        format!("Today · {date}")
    } else if day == next_day(today) {
        format!("Tomorrow · {date}")
    } else if day.year() == today.year() {
        date.to_string()
    } else {
        day.format("%A, %B %-d, %Y").to_string()
    }
}

/// An agenda day's summary: its events, and the hours its timed ones book (overlaps once,
/// declined ones left out): 4 events · 3h 30m.
pub fn day_summary(events: &[Event], indices: &[usize], day: NaiveDate) -> String {
    let count = if indices.len() == 1 { "1 event".to_owned() } else { format!("{} events", indices.len()) };
    let booked = booked_minutes(events, &layout_day(events, indices.iter().copied(), day));
    if booked > 0 { format!("{count} · {}", short_duration(booked)) } else { count }
}

/// A timed event's stretch of one day, in minutes from that day's midnight by the clock
/// (`bottom` excluded), and where it sits among the events it overlaps: in `column` of
/// `columns`, `span` columns wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub event: usize,
    pub top: i64,
    pub bottom: i64,
    pub column: usize,
    pub columns: usize,
    pub span: usize,
}

/// Minutes since midnight by the clock, so a day with a daylight-saving change still has its
/// hours where the clock shows them.
fn clock_minutes(time: DateTime<Local>) -> i64 {
    i64::from(time.hour() * 60 + time.minute())
}

/// The timed events of `candidates` on `day`, laid out side by side where they overlap: earliest
/// first (the longer of two starting together first), each overlapping group sharing the width,
/// each event in the first column free when it starts and widened over the columns to its right
/// that stay free while it lasts. An event over midnight is drawn on each of its days.
pub fn layout_day(events: &[Event], candidates: impl IntoIterator<Item = usize>, day: NaiveDate) -> Vec<Placement> {
    let (start, next) = (midnight(day), midnight(next_day(day)));
    let mut pieces: Vec<(usize, i64, i64)> = Vec::new();
    for index in candidates {
        let event = &events[index];
        if event.in_all_day_row() {
            continue;
        }
        let moment = event.end <= event.start;
        let on_day =
            if moment { event.start >= start && event.start < next } else { event.end > start && event.start < next };
        if !on_day {
            continue;
        }
        let top = (if event.start < start { 0 } else { clock_minutes(event.start) }).min(DAY_MINUTES - MIN_EVENT_MINUTES);
        let end = if event.end < next { clock_minutes(event.end) } else { DAY_MINUTES };
        pieces.push((index, top, end.max(top + MIN_EVENT_MINUTES).min(DAY_MINUTES)));
    }
    pieces.sort_by(|a, b| {
        a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then_with(|| events[a.0].item.title.cmp(&events[b.0].item.title))
    });

    let mut out = Vec::with_capacity(pieces.len());
    let mut group: Vec<((usize, i64, i64), usize)> = Vec::new();
    let mut column_ends: Vec<i64> = Vec::new();
    let mut group_end = 0;
    let flush = |group: &mut Vec<((usize, i64, i64), usize)>, column_ends: &mut Vec<i64>, out: &mut Vec<Placement>| {
        let columns = column_ends.len();
        for &((event, top, bottom), column) in group.iter() {
            let mut span = 1;
            while column + span < columns
                && !group.iter().any(|&((_, t, b), c)| c == column + span && t < bottom && b > top)
            {
                span += 1;
            }
            out.push(Placement { event, top, bottom, column, columns, span });
        }
        group.clear();
        column_ends.clear();
    };
    for piece in pieces {
        if !group.is_empty() && piece.1 >= group_end {
            flush(&mut group, &mut column_ends, &mut out);
        }
        let column = match column_ends.iter().position(|&end| end <= piece.1) {
            Some(column) => {
                column_ends[column] = piece.2;
                column
            }
            None => {
                column_ends.push(piece.2);
                column_ends.len() - 1
            }
        };
        group.push((piece, column));
        group_end = if group.len() == 1 { piece.2 } else { group_end.max(piece.2) };
    }
    if !group.is_empty() {
        flush(&mut group, &mut column_ends, &mut out);
    }
    out
}

/// Minutes a day is booked by its placed events, overlaps counted once and declined ones left out.
pub fn booked_minutes(events: &[Event], placements: &[Placement]) -> i64 {
    let mut spans: Vec<(i64, i64)> =
        placements.iter().filter(|p| !events[p.event].declined()).map(|p| (p.top, p.bottom)).collect();
    spans.sort_unstable();
    let (mut minutes, mut end) = (0, 0);
    for (top, bottom) in spans {
        let from = top.max(end);
        if bottom > from {
            minutes += bottom - from;
        }
        end = end.max(bottom);
    }
    minutes
}

/// Which of a day's placed events clash: two the account is committed to (it accepted, organizes
/// it, or it has no guests) whose times overlap, not merely their drawn heights.
pub fn day_conflicts(events: &[Event], placements: &[Placement]) -> Vec<bool> {
    let committed = |event: &Event| matches!(event.item.response.as_deref(), None | Some("accepted" | "organizer"));
    let mut out = vec![false; placements.len()];
    for (i, a) in placements.iter().enumerate() {
        let first = &events[a.event];
        if a.columns < 2 || !committed(first) {
            continue;
        }
        for (j, b) in placements.iter().enumerate().skip(i + 1) {
            let second = &events[b.event];
            if b.columns < 2 || !committed(second) {
                continue;
            }
            if first.start < second.end && second.start < first.end {
                out[i] = true;
                out[j] = true;
            }
        }
    }
    out
}

/// An event in the all-day row: from day `first` to day `last` (indices into the days shown), in
/// `row`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllDayPlacement {
    pub event: usize,
    pub first: usize,
    pub last: usize,
    pub row: usize,
}

/// The all-day row over `days` (consecutive): each all-day or day-long event of `candidates`
/// across the days it covers, the earlier and then the longer ones in the upper rows. Answers the
/// placements and how many rows they take.
pub fn layout_all_day(
    events: &[Event],
    candidates: impl IntoIterator<Item = usize>,
    days: &[NaiveDate],
) -> (Vec<AllDayPlacement>, usize) {
    let Some(&start) = days.first() else { return (Vec::new(), 0) };
    let mut placed: Vec<(usize, i64, i64)> = Vec::new();
    for index in candidates {
        let event = &events[index];
        if !event.in_all_day_row() {
            continue;
        }
        let first = (event.start.date_naive() - start).num_days();
        let last = (event.last_day() - start).num_days();
        if last < 0 || first >= days.len() as i64 {
            continue;
        }
        placed.push((index, first.max(0), last.min(days.len() as i64 - 1)));
    }
    placed.sort_by(|a, b| {
        a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then_with(|| events[a.0].item.title.cmp(&events[b.0].item.title))
    });
    let mut row_ends: Vec<i64> = Vec::new();
    let mut out = Vec::with_capacity(placed.len());
    for (event, first, last) in placed {
        let row = match row_ends.iter().position(|&end| end < first) {
            Some(row) => {
                row_ends[row] = last;
                row
            }
            None => {
                row_ends.push(last);
                row_ends.len() - 1
            }
        };
        out.push(AllDayPlacement { event, first: first as usize, last: last as usize, row });
    }
    (out, row_ends.len())
}

/// How an event's card on the timeline reads: a white card for what is still ahead, marked
/// inside a dashed border while the invitation waits for an answer (or got a maybe), flattened
/// into the tray once over or declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Upcoming,
    Invited,
    Past,
    Declined,
}

pub fn look(event: &Event, now: DateTime<Local>) -> Look {
    if event.declined() {
        Look::Declined
    } else if event.end <= now {
        Look::Past
    } else if matches!(event.item.response.as_deref(), Some("needsAction" | "tentative")) {
        Look::Invited
    } else {
        Look::Upcoming
    }
}

/// From `start` to `end`, the period once when both share it: 10:15 – 11:30 AM.
pub fn time_range(start: DateTime<Local>, end: DateTime<Local>) -> String {
    if (start.hour() < 12) == (end.hour() < 12) {
        format!("{} – {}", start.format("%-I:%M"), clock(end))
    } else {
        format!("{} – {}", clock(start), clock(end))
    }
}

/// The day view's day: Wednesday, November 11, 2026.
pub fn day_title(day: NaiveDate) -> String {
    day.format("%A, %B %-d, %Y").to_string()
}

/// The week from `start`: November 8 – 14, 2026.
pub fn week_title(start: NaiveDate) -> String {
    let end = start + Days::new(6);
    if start.year() != end.year() {
        format!("{} – {}", start.format("%B %-d, %Y"), end.format("%B %-d, %Y"))
    } else if start.month() != end.month() {
        format!("{} – {}", start.format("%B %-d"), end.format("%B %-d, %Y"))
    } else {
        format!("{} – {}, {}", start.format("%B %-d"), end.day(), end.year())
    }
}

/// What the engine fetched, for days shown outside it: Only Nov 4 – Jan 10 is synced.
pub fn synced_note(from: NaiveDate, until: NaiveDate) -> String {
    let last = until.pred_opt().unwrap_or(until);
    format!("Only {} – {} is synced", from.format("%b %-d"), last.format("%b %-d"))
}

/// An event's length, short: 15m, 1h, 1h 30m.
pub fn short_duration(minutes: i64) -> String {
    let (hours, minutes) = (minutes / 60, minutes % 60);
    match (hours, minutes) {
        (0, minutes) => format!("{minutes}m"),
        (hours, 0) => format!("{hours}h"),
        (hours, minutes) => format!("{hours}h {minutes}m"),
    }
}

/// A time of day: 9:30 AM.
pub fn clock(time: DateTime<Local>) -> String {
    time.format("%-I:%M %p").to_string()
}

/// The video call's service: Zoom, Teams, Webex, else Meet.
pub fn meeting_label(url: &str) -> &'static str {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    if host.contains("zoom.") {
        "Zoom"
    } else if host.contains("teams.") {
        "Teams"
    } else if host.contains("webex.") {
        "Webex"
    } else {
        "Meet"
    }
}

/// The meeting the Up Next card offers at `now`: the soonest one starting within
/// [`UP_NEXT_LEAD`], else the one under way that started last. Meetings have a call or guests;
/// all-day and declined events never count.
pub fn up_next(events: &[Event], now: DateTime<Local>) -> Option<usize> {
    let mut soonest: Option<usize> = None;
    let mut current: Option<usize> = None;
    for (index, event) in events.iter().enumerate() {
        if event.item.all_day || event.declined() || event.end <= now {
            continue;
        }
        if event.item.meeting_url.is_none() && event.item.attendees.is_empty() {
            continue;
        }
        if event.start > now {
            if event.start - now <= UP_NEXT_LEAD && soonest.is_none_or(|s| event.start < events[s].start) {
                soonest = Some(index);
            }
        } else if current.is_none_or(|c| event.start > events[c].start) {
            current = Some(index);
        }
    }
    soonest.or(current)
}

/// When the Up Next meeting starts, from `now`: In 7m (a started minute counts whole), or Now.
pub fn up_next_label(event: &Event, now: DateTime<Local>) -> String {
    if event.start <= now {
        return "Now".into();
    }
    let seconds = (event.start - now).num_seconds();
    format!("In {}m", (seconds + 59) / 60)
}

/// The answers an invitation takes, as the engine names them, with their labels.
pub const REPLIES: [(&str, &str); 3] = [("accepted", "Going"), ("tentative", "Maybe"), ("declined", "Decline")];

/// What the account can do about an event's invitation. `answerable`: it is a guest (not the
/// organizer) and its sign-in may change its calendar; `chosen`: the answer to show picked, the
/// one being sent (`sending`) else the calendar's; `invited`: it has not answered yet, so the
/// agenda's row offers the answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyState {
    pub answerable: bool,
    pub chosen: Option<&'static str>,
    pub sending: Option<&'static str>,
    pub invited: bool,
}

/// `item`'s [`ReplyState`] for its `account`, with the answer being sent, if any.
pub fn reply_state(item: &CalendarItem, account: Option<&CalendarAccount>, sending: Option<&'static str>) -> ReplyState {
    let response = item.response.as_deref();
    let answered = REPLIES.iter().map(|(value, _)| *value).find(|value| Some(*value) == response);
    let guest = response == Some("needsAction") || answered.is_some();
    let answerable = guest && account.is_some_and(|account| account.granted && account.writable);
    ReplyState {
        answerable,
        chosen: sending.or(answered),
        sending: if answerable { sending } else { None },
        invited: answerable && response == Some("needsAction"),
    }
}

/// An answer as the calendar shows it.
pub fn response_label(response: Option<&str>) -> &'static str {
    match response {
        Some("accepted") => "Going",
        Some("tentative") => "Maybe",
        Some("declined") => "Declined",
        Some("needsAction") => "Invited",
        Some("organizer") => "Organizer",
        _ => "",
    }
}

/// `item`'s guests, and its organizer first when the guests leave them out; each with whether
/// they organize it.
pub fn event_guests(item: &CalendarItem) -> Vec<(&CalendarPerson, bool)> {
    let organizer = item.organizer.as_ref();
    let key = organizer.and_then(|o| o.email.as_deref()).map(str::to_lowercase);
    let is_organizer =
        |person: &CalendarPerson| key.as_ref().is_some_and(|key| person.email.as_deref().map(str::to_lowercase).as_ref() == Some(key));
    let mut out = Vec::with_capacity(item.attendees.len() + 1);
    if let Some(organizer) = organizer.filter(|_| !item.attendees.iter().any(is_organizer)) {
        out.push((organizer, true));
    }
    out.extend(item.attendees.iter().map(|person| (person, is_organizer(person))));
    out
}

/// The guests to show on a row: everyone invited but the account.
pub fn others(item: &CalendarItem) -> Vec<&CalendarPerson> {
    let account = item.account.to_lowercase();
    item.attendees.iter().filter(|person| person.email.as_deref().map(str::to_lowercase) != Some(account.clone())).collect()
}

/// The guests' answers counted, in the order Going, Maybe, Declined, Invited: 4 Going · 1 Maybe.
pub fn guest_summary(guests: &[CalendarPerson]) -> String {
    let mut counts = [0usize; 4];
    for person in guests {
        let slot = match person.response.as_deref() {
            Some("accepted") => 0,
            Some("tentative") => 1,
            Some("declined") => 2,
            _ => 3,
        };
        counts[slot] += 1;
    }
    ["Going", "Maybe", "Declined", "Invited"]
        .iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `#rrggbb` as a color.
pub fn hex_color(hex: Option<&str>) -> Option<Hsla> {
    let digits = hex?.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    u32::from_str_radix(digits, 16).ok().map(|value| rgb(value).into())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn item(id: &str, start: &str, end: &str) -> CalendarItem {
        CalendarItem {
            account: "me@example.com".into(),
            calendar: "primary".into(),
            calendar_name: None,
            color: None,
            id: id.into(),
            title: id.into(),
            start: start.into(),
            end: end.into(),
            all_day: false,
            start_day: None,
            end_day: None,
            location: None,
            meeting_url: None,
            organizer: None,
            attendees: Vec::new(),
            response: None,
            notes: None,
            notes_cut: false,
            link: None,
            uid: None,
            series: None,
            mine: false,
            editable: false,
        }
    }

    fn local(day: NaiveDate, hour: u32, minute: u32) -> String {
        Local.from_local_datetime(&day.and_hms_opt(hour, minute, 0).unwrap()).earliest().unwrap().to_rfc3339()
    }

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, n).unwrap()
    }

    pub(crate) fn timed(id: &str, on: NaiveDate, from: (u32, u32), to: (u32, u32)) -> Event {
        Event::new(item(id, &local(on, from.0, from.1), &local(on, to.0, to.1))).unwrap()
    }

    #[test]
    fn agenda_lists_each_day_an_event_covers_all_day_ones_first() {
        let mut trip = item("trip", "", "");
        trip.all_day = true;
        trip.start_day = Some("2026-11-10".into());
        trip.end_day = Some("2026-11-13".into());
        let events = vec![
            timed("standup", day(11), (9, 0), (9, 15)),
            Event::new(trip).unwrap(),
            timed("old", day(9), (9, 0), (10, 0)),
            timed("late", day(12), (23, 0), (24 - 1, 59)),
        ];
        let days = agenda_days(&events, day(11));
        let listed: Vec<(u32, Vec<&str>)> = days
            .iter()
            .map(|(d, ix)| (d.day(), ix.iter().map(|&i| events[i].item.title.as_str()).collect()))
            .collect();
        assert_eq!(listed, vec![(11, vec!["trip", "standup"]), (12, vec!["trip", "late"])]);
        assert_eq!(in_order(&days).iter().map(|&(i, _)| events[i].item.title.as_str()).collect::<Vec<_>>(), [
            "trip", "standup", "late"
        ]);
    }

    #[test]
    fn steps_stay_at_the_ends() {
        let events = vec![timed("a", day(11), (9, 0), (10, 0)), timed("b", day(11), (11, 0), (12, 0))];
        let order = vec![0, 1];
        assert_eq!(stepped(&order, &events, None, 1), Some(0));
        assert_eq!(stepped(&order, &events, None, -1), Some(1));
        assert_eq!(stepped(&order, &events, Some(&events[1].key), 1), Some(1));
        assert_eq!(stepped(&order, &events, Some(&events[1].key), -1), Some(0));
    }

    #[test]
    fn overlapping_events_share_the_width_and_book_once() {
        let events = vec![
            timed("long", day(11), (9, 0), (11, 0)),
            timed("short", day(11), (9, 30), (10, 0)),
            timed("after", day(11), (10, 30), (11, 30)),
            timed("alone", day(11), (14, 0), (14, 5)),
        ];
        let placed = layout_day(&events, 0..events.len(), day(11));
        let columns: Vec<(usize, usize, usize, usize)> = placed.iter().map(|p| (p.event, p.column, p.columns, p.span)).collect();
        assert_eq!(columns, [(0, 0, 2, 1), (1, 1, 2, 1), (2, 1, 2, 1), (3, 0, 1, 1)]);
        // 9:00–11:30, then the five-minute meeting drawn (and counted) as twenty.
        assert_eq!(booked_minutes(&events, &placed), 150 + MIN_EVENT_MINUTES);
        assert_eq!(day_summary(&events, &[0, 1, 2, 3], day(11)), "4 events · 2h 50m");
    }

    #[test]
    fn up_next_is_the_soonest_meeting_then_the_one_under_way() {
        let mut events = vec![
            timed("now", day(11), (9, 0), (10, 0)),
            timed("soon", day(11), (9, 50), (10, 30)),
            timed("later", day(11), (11, 0), (12, 0)),
        ];
        for event in &mut events {
            event.item.meeting_url = Some("https://meet.google.com/abc".into());
        }
        let at = |h, m| Local.from_local_datetime(&day(11).and_hms_opt(h, m, 30).unwrap()).earliest().unwrap();
        assert_eq!(up_next(&events, at(9, 10)), Some(0));
        assert_eq!(up_next(&events, at(9, 25)), Some(1));
        assert_eq!(up_next_label(&events[1], at(9, 25)), "In 25m");
        assert_eq!(up_next_label(&events[0], at(9, 25)), "Now");
        events[1].item.response = Some("declined".into());
        assert_eq!(up_next(&events, at(9, 25)), Some(0));
    }

    #[test]
    fn replies_follow_the_account_and_the_answer_being_sent() {
        let account = CalendarAccount {
            id: "me@example.com".into(),
            label: "Me".into(),
            provider: mailstream::Provider::Google,
            granted: true,
            writable: true,
            loading: false,
            problem: None,
            fetched: None,
            calendars: Vec::new(),
        };
        let mut invite = item("x", "", "");
        invite.response = Some("needsAction".into());
        let state = reply_state(&invite, Some(&account), None);
        assert!(state.answerable && state.invited && state.chosen.is_none());
        let state = reply_state(&invite, Some(&account), Some("tentative"));
        assert_eq!((state.chosen, state.sending), (Some("tentative"), Some("tentative")));
        invite.response = Some("organizer".into());
        assert!(!reply_state(&invite, Some(&account), None).answerable);
        invite.response = Some("accepted".into());
        let read_only = CalendarAccount { writable: false, ..account };
        let state = reply_state(&invite, Some(&read_only), None);
        assert_eq!((state.answerable, state.chosen), (false, Some("accepted")));
    }

    #[test]
    fn guests_count_their_answers_and_the_organizer_leads() {
        let person = |email: &str, response: Option<&str>| CalendarPerson {
            name: None,
            email: Some(email.into()),
            response: response.map(Into::into),
        };
        let mut event = item("x", "", "");
        event.organizer = Some(person("boss@example.com", None));
        event.attendees =
            vec![person("a@example.com", Some("accepted")), person("b@example.com", Some("tentative")), person("c@example.com", None)];
        let guests = event_guests(&event);
        assert_eq!(guests.len(), 4);
        assert!(guests[0].1);
        assert_eq!(guest_summary(&event.attendees), "1 Going · 1 Maybe · 1 Invited");
        assert_eq!(meeting_label("https://us02web.zoom.us/j/1"), "Zoom");
        assert_eq!(meeting_label("https://meet.google.com/abc"), "Meet");
        assert_eq!(short_duration(90), "1h 30m");
        assert_eq!(week_of(day(11)), day(8));
    }

    #[test]
    fn all_day_row_stacks_overlapping_bars_and_cuts_them_to_the_days_shown() {
        let all_day = |id: &str, first: u32, after: u32| {
            let mut event = item(id, "", "");
            event.all_day = true;
            event.start_day = Some(day(first).to_string());
            event.end_day = Some(day(after).to_string());
            Event::new(event).unwrap()
        };
        let events = vec![
            all_day("trip", 6, 10),
            all_day("holiday", 9, 10),
            all_day("later", 12, 13),
            timed("meeting", day(9), (9, 0), (10, 0)),
        ];
        let week = week_days(week_of(day(11)));
        let (placed, rows) = layout_all_day(&events, 0..events.len(), &week);
        let bars: Vec<(usize, usize, usize, usize)> = placed.iter().map(|p| (p.event, p.first, p.last, p.row)).collect();
        assert_eq!(bars, [(0, 0, 1, 0), (1, 1, 1, 1), (2, 4, 4, 0)]);
        assert_eq!(rows, 2);
    }

    #[test]
    fn clashes_count_only_what_the_account_attends() {
        let mut events = vec![
            timed("a", day(11), (9, 0), (10, 0)),
            timed("b", day(11), (9, 30), (10, 30)),
            timed("c", day(11), (9, 45), (10, 15)),
        ];
        events[2].item.response = Some("declined".into());
        let placed = layout_day(&events, 0..3, day(11));
        assert_eq!(day_conflicts(&events, &placed), [true, true, false]);
        let at = |h, m| Local.from_local_datetime(&day(11).and_hms_opt(h, m, 0).unwrap()).earliest().unwrap();
        assert_eq!(time_range(at(10, 15), at(11, 30)), "10:15 – 11:30 AM");
        assert_eq!(time_range(at(11, 30), at(13, 0)), "11:30 AM – 1:00 PM");
        assert_eq!(week_title(day(8)), "November 8 – 14, 2026");
        assert_eq!(week_title(day(29)), "November 29 – December 5, 2026");
    }
}
