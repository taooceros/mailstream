//! Meeting invitations in mail: the iCalendar part (`text/calendar`, an `.ics` file) an
//! invitation carries, read into the [`Invitation`] the reader's card shows. Times written in a
//! `TZID` zone are placed with the file's own `VTIMEZONE` rules, as every provider sends them.
use std::collections::HashMap;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::calendar_api::{Person, local_midnight};
use crate::model::{Date, date_from_unix, iso8601};

/// A meeting invitation, or the notice that one was cancelled, as the message states it. Whether
/// and how the account answered comes from its calendar (`calendar.invitation`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    /// `request`, or `cancel` for a cancelled meeting.
    pub method: String,
    /// The meeting's iCalendar UID, the same in every calendar it is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    /// A recurring meeting, answered as a whole.
    #[serde(default)]
    pub recurring: bool,
    /// One occurrence of a recurring meeting.
    #[serde(default)]
    pub occurrence: bool,
    /// The event's id in the account's calendar, when the provider names it (Outlook's
    /// meeting requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

impl Invitation {
    /// As the reader gets it in `mail.show`.
    pub fn json(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("invitations encode");
        value["start"] = json!(iso8601(self.start));
        value["end"] = json!(iso8601(self.end));
        value
    }
}

/// Whether a part of type `mime` named `filename` is an iCalendar file.
pub fn is_calendar(mime: &str, filename: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    mime == "text/calendar" || mime == "application/ics" || filename.to_ascii_lowercase().ends_with(".ics")
}

/// The invitation (or cancellation) `bytes` hold; nil for any other calendar file (a published
/// event, an answer to the user's own meeting) or one without a time.
pub fn parse(bytes: &[u8]) -> Option<Invitation> {
    let text = String::from_utf8_lossy(bytes);
    let root = component(&unfold(&text))?;
    let calendar = if root.name == "VCALENDAR" { &root } else { root.children.iter().find(|child| child.name == "VCALENDAR")? };
    let method = match calendar.value("METHOD").map(str::to_ascii_uppercase).as_deref() {
        Some("REQUEST") => "request",
        Some("CANCEL") => "cancel",
        _ => return None,
    };
    let zones: HashMap<String, Zone> = calendar
        .children
        .iter()
        .filter(|child| child.name == "VTIMEZONE")
        .filter_map(|child| Some((child.value("TZID")?.to_string(), Zone::new(child))))
        .collect();
    let events: Vec<&Component> = calendar.children.iter().filter(|child| child.name == "VEVENT").collect();
    let event = events.iter().find(|event| event.property("RECURRENCE-ID").is_none()).or(events.first())?;
    let start = instant(event.property("DTSTART")?, &zones)?;
    let end = match (event.property("DTEND"), event.value("DURATION").and_then(duration)) {
        (Some(end), _) => instant(end, &zones),
        (None, Some(seconds)) => Some(match &start.day {
            Some(day) => day_instant(&add_days(day, (seconds / 86_400).max(1))?)?,
            None => Instant { date: start.date + seconds as f64, day: None },
        }),
        (None, None) => None,
    };
    let end = match (end, &start.day) {
        (Some(end), _) => end,
        (None, Some(day)) => day_instant(&add_days(day, 1)?)?,
        (None, None) => Instant { date: start.date, day: None },
    };
    let all_day = start.day.is_some();
    Some(Invitation {
        method: method.into(),
        uid: event.text("UID"),
        title: event.text("SUMMARY").unwrap_or_else(|| "(No title)".into()),
        start: start.date,
        end: end.date.max(start.date),
        all_day,
        end_day: if all_day { end.day.or_else(|| add_days(start.day.as_deref()?, 1)) } else { None },
        start_day: start.day,
        location: event.text("LOCATION"),
        organizer: event.property("ORGANIZER").map(person),
        recurring: event.property("RRULE").is_some(),
        occurrence: event.property("RECURRENCE-ID").is_some(),
        event: None,
    })
}

/// `uid` as both a calendar and the invitation can be compared on. Exchange writes UIDs as a
/// hex `GlobalObjectId`: one made elsewhere is wrapped (`vCal-Uid`) and unwrapped here, and an
/// occurrence's carries its date, cleared here so it matches its series.
pub fn normalized_uid(uid: &str) -> String {
    const EXCHANGE: &str = "040000008200E00074C5B7101A82E008";
    const WRAPPED: &[u8] = b"vCal-Uid\x01\x00\x00\x00";
    let uid = uid.trim();
    let hex = uid.len() >= 40 && uid.len().is_multiple_of(2) && uid.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !(hex && uid[..32].eq_ignore_ascii_case(EXCHANGE)) {
        return uid.to_string();
    }
    let bytes: Vec<u8> = (0..uid.len()).step_by(2).filter_map(|at| u8::from_str_radix(&uid[at..at + 2], 16).ok()).collect();
    if let Some(at) = bytes.windows(WRAPPED.len()).position(|window| window == WRAPPED) {
        let rest = &bytes[at + WRAPPED.len()..];
        let end = rest.iter().position(|&byte| byte == 0).unwrap_or(rest.len());
        return String::from_utf8_lossy(&rest[..end]).into_owned();
    }
    let upper = uid.to_ascii_uppercase();
    format!("{}00000000{}", &upper[..32], &upper[40..])
}

// MARK: - Lines and components

struct Property {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct Component {
    name: String,
    properties: Vec<Property>,
    children: Vec<Component>,
}

impl Component {
    fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|property| property.name == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.property(name).map(|property| property.value.trim())
    }

    /// A text value unescaped, nil when empty.
    fn text(&self, name: &str) -> Option<String> {
        self.value(name).map(unescape).filter(|text| !text.trim().is_empty()).map(|text| text.trim().to_string())
    }
}

/// Lines with their continuations (a leading space or tab) joined back.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if line.is_empty() => {}
            _ => lines.push(line.to_string()),
        }
    }
    lines
}

/// `NAME;PARAM=value;…:value`; colons inside quoted parameters belong to them.
fn property(line: &str) -> Option<Property> {
    let mut quoted = false;
    let mut cuts = Vec::new();
    let mut colon = None;
    for (at, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => cuts.push(at),
            ':' if !quoted => {
                colon = Some(at);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let mut bounds = vec![0];
    bounds.extend(cuts.iter().map(|cut| cut + 1));
    let mut ends = cuts.clone();
    ends.push(colon);
    let mut pieces = bounds.into_iter().zip(ends).map(|(from, to)| &line[from..to]);
    let name = pieces.next()?.trim().to_ascii_uppercase();
    let params = pieces
        .filter_map(|piece| {
            let (key, value) = piece.split_once('=')?;
            Some((key.trim().to_ascii_uppercase(), value.trim().trim_matches('"').to_string()))
        })
        .collect();
    Some(Property { name, params, value: line[colon + 1..].to_string() })
}

/// The outermost component of `lines`.
fn component(lines: &[String]) -> Option<Component> {
    let mut stack: Vec<Component> = Vec::new();
    for line in lines {
        let Some(property) = property(line) else { continue };
        match property.name.as_str() {
            "BEGIN" => stack.push(Component { name: property.value.trim().to_ascii_uppercase(), ..Component::default() }),
            "END" => {
                let done = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Some(done),
                }
            }
            _ => stack.last_mut()?.properties.push(property),
        }
    }
    // A file cut short: what was read so far.
    let mut done = stack.pop()?;
    while let Some(mut parent) = stack.pop() {
        parent.children.push(done);
        done = parent;
    }
    Some(done)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn person(property: &Property) -> Person {
    let value = property.value.trim();
    let email = value.get(..7).filter(|scheme| scheme.eq_ignore_ascii_case("mailto:")).map_or(value, |_| &value[7..]);
    let non_empty = |text: &str| Some(text.trim().to_string()).filter(|text| !text.is_empty());
    Person { name: property.param("CN").and_then(non_empty), email: non_empty(email), response: None }
}

// MARK: - Times

struct Instant {
    date: Date,
    /// For a date without a time, that day.
    day: Option<String>,
}

fn day_instant(day: &str) -> Option<Instant> {
    Some(Instant { date: local_midnight(day)?, day: Some(day.to_string()) })
}

fn add_days(day: &str, days: i64) -> Option<String> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some((date + chrono::Duration::days(days)).format("%Y-%m-%d").to_string())
}

/// A `DTSTART`/`DTEND`: a day, a UTC time, or a wall time in its `TZID` zone (here when the
/// file does not say).
fn instant(property: &Property, zones: &HashMap<String, Zone>) -> Option<Instant> {
    let value = property.value.trim();
    if property.param("VALUE").is_some_and(|kind| kind.eq_ignore_ascii_case("DATE")) || value.len() == 8 {
        let date = NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()?;
        return day_instant(&date.format("%Y-%m-%d").to_string());
    }
    let wall = NaiveDateTime::parse_from_str(value.trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S").ok()?;
    let offset = if value.ends_with(['Z', 'z']) {
        Some(0)
    } else {
        match property.param("TZID") {
            Some(zone) if zones.contains_key(zone) => zones[zone].offset(wall),
            Some(zone) if ["UTC", "GMT", "Z", "Etc/UTC", "Etc/GMT"].iter().any(|utc| zone.eq_ignore_ascii_case(utc)) => Some(0),
            _ => None,
        }
    };
    let seconds = match offset {
        Some(offset) => wall.and_utc().timestamp() - i64::from(offset),
        None => Local.from_local_datetime(&wall).earliest()?.timestamp(),
    };
    Some(Instant { date: date_from_unix(seconds as f64), day: None })
}

/// `P1DT2H30M`, `PT45M`, `P2W` in seconds.
fn duration(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut rest = text.strip_prefix('P')?;
    let mut seconds = 0i64;
    let mut time = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('T') {
            time = true;
            rest = after;
            continue;
        }
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let number: i64 = rest[..digits].parse().ok()?;
        let unit = match (rest[digits..].chars().next()?, time) {
            ('W', _) => 604_800,
            ('D', _) => 86_400,
            ('H', true) => 3_600,
            ('M', true) => 60,
            ('S', true) => 1,
            _ => return None,
        };
        seconds += number * unit;
        rest = &rest[digits + 1..];
    }
    Some(sign * seconds)
}

/// A `VTIMEZONE`: when each of its standard and daylight offsets starts.
struct Zone {
    rules: Vec<ZoneRule>,
}

struct ZoneRule {
    /// Seconds east of UTC from its onset.
    offset: i32,
    /// The first onset, in the wall time before it.
    first: NaiveDateTime,
    /// A yearly onset: month, week of the month (negative from its end) and weekday.
    yearly: Option<(u32, i32, Weekday)>,
}

impl Zone {
    fn new(component: &Component) -> Self {
        let rules = component
            .children
            .iter()
            .filter(|child| child.name == "STANDARD" || child.name == "DAYLIGHT")
            .filter_map(|child| {
                Some(ZoneRule {
                    offset: utc_offset(child.value("TZOFFSETTO")?)?,
                    first: NaiveDateTime::parse_from_str(child.value("DTSTART")?.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?,
                    yearly: child.value("RRULE").and_then(yearly),
                })
            })
            .collect();
        Self { rules }
    }

    /// The offset in force at wall time `wall`: the rule with the latest onset before it.
    fn offset(&self, wall: NaiveDateTime) -> Option<i32> {
        let mut latest: Option<(NaiveDateTime, i32)> = None;
        for rule in &self.rules {
            for year in [wall.year(), wall.year() - 1] {
                let Some(onset) = rule.onset(year) else { continue };
                if onset <= wall && onset >= rule.first && latest.is_none_or(|(at, _)| onset > at) {
                    latest = Some((onset, rule.offset));
                }
            }
        }
        latest.map(|(_, offset)| offset).or_else(|| self.rules.first().map(|rule| rule.offset))
    }
}

impl ZoneRule {
    fn onset(&self, year: i32) -> Option<NaiveDateTime> {
        let Some((month, week, weekday)) = self.yearly else {
            return (year == self.first.year()).then_some(self.first);
        };
        let day = if week > 0 {
            let first = NaiveDate::from_ymd_opt(year, month, 1)?;
            let ahead = (7 + weekday.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7;
            first + chrono::Duration::days(i64::from(ahead) + 7 * i64::from(week - 1))
        } else {
            let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(year, month + 1, 1)? };
            let last = next.pred_opt()?;
            let back = (7 + last.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
            last - chrono::Duration::days(i64::from(back) + 7 * i64::from(-week - 1))
        };
        (day.month() == month).then(|| day.and_time(self.first.time()))
    }
}

/// `+0530`, `-0800` in seconds.
fn utc_offset(text: &str) -> Option<i32> {
    let (sign, digits) = match text.trim().split_at_checked(1)? {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return None,
    };
    let hours: i32 = digits.get(..2)?.parse().ok()?;
    let minutes: i32 = digits.get(2..4).unwrap_or("00").parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// `FREQ=YEARLY;BYMONTH=3;BYDAY=2SU` (or `-1SU`) as month, week and weekday.
fn yearly(rule: &str) -> Option<(u32, i32, Weekday)> {
    let parts: HashMap<String, String> = rule
        .split(';')
        .filter_map(|part| part.split_once('=').map(|(key, value)| (key.to_ascii_uppercase(), value.to_string())))
        .collect();
    if parts.get("FREQ").map(String::as_str) != Some("YEARLY") {
        return None;
    }
    let month: u32 = parts.get("BYMONTH")?.parse().ok()?;
    let day = parts.get("BYDAY")?;
    let split = day.len().checked_sub(2)?;
    let weekday = match &day[split..] {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    };
    let week = match &day[..split] {
        "" => parts.get("BYSETPOS").and_then(|position| position.parse().ok()).unwrap_or(1),
        number => number.trim_start_matches('+').parse().ok()?,
    };
    Some((month, week, weekday))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_iso8601;

    const GOOGLE: &str = "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nCALSCALE:GREGORIAN\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:America/New_York\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nTZNAME:EDT\r\nDTSTART:19700308T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nTZNAME:EST\r\nDTSTART:19701101T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nDTSTART;TZID=America/New_York:20261030T100000\r\nDTEND;TZID=America/New_York:20261030T110000\r\nDTSTAMP:20261008T120000Z\r\nORGANIZER;CN=\"Ann Lee\":mailto:ann@example.com\r\nUID:abc123@google.com\r\nATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;CN=me@\r\n example.com;X-NUM-GUESTS=0:mailto:me@example.com\r\nSUMMARY:Design review\\, Q4\r\nLOCATION:Room 4\\; Building B\r\nSEQUENCE:0\r\nSTATUS:CONFIRMED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_a_google_invitation_in_its_own_zone() {
        let invitation = parse(GOOGLE.as_bytes()).unwrap();
        assert_eq!(invitation.method, "request");
        assert_eq!(invitation.uid.as_deref(), Some("abc123@google.com"));
        assert_eq!(invitation.title, "Design review, Q4");
        assert_eq!(invitation.location.as_deref(), Some("Room 4; Building B"));
        let organizer = invitation.organizer.unwrap();
        assert_eq!((organizer.name.as_deref(), organizer.email.as_deref()), (Some("Ann Lee"), Some("ann@example.com")));
        // October 30th is still daylight time (UTC-4); November 2nd is standard (UTC-5).
        assert_eq!(invitation.start, parse_iso8601("2026-10-30T14:00:00Z").unwrap());
        assert_eq!(invitation.end, parse_iso8601("2026-10-30T15:00:00Z").unwrap());
        let winter = GOOGLE.replace("20261030T1", "20261102T1");
        assert_eq!(parse(winter.as_bytes()).unwrap().start, parse_iso8601("2026-11-02T15:00:00Z").unwrap());
        assert!(!invitation.recurring && !invitation.occurrence && !invitation.all_day);
    }

    #[test]
    fn reads_outlook_zones_all_day_events_and_cancellations() {
        let outlook = "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VTIMEZONE\nTZID:W. Europe Standard Time\nBEGIN:STANDARD\nDTSTART:16010101T030000\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:16010101T020000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE\nBEGIN:VEVENT\nRRULE:FREQ=WEEKLY;BYDAY=MO\nUID:040000008200E00074C5B7101A82E00800000000A0B1\nSUMMARY;LANGUAGE=en-US:Weekly sync\nDTSTART;TZID=W. Europe Standard Time:20261109T090000\nDTEND;TZID=W. Europe Standard Time:20261109T093000\nORGANIZER;CN=Bo:MAILTO:bo@example.com\nEND:VEVENT\nEND:VCALENDAR\n";
        let invitation = parse(outlook.as_bytes()).unwrap();
        assert_eq!(invitation.start, parse_iso8601("2026-11-09T08:00:00Z").unwrap());
        assert!(invitation.recurring);
        assert_eq!(invitation.organizer.unwrap().email.as_deref(), Some("bo@example.com"));
        let summer = outlook.replace("20261109T09", "20260615T09");
        assert_eq!(parse(summer.as_bytes()).unwrap().start, parse_iso8601("2026-06-15T07:00:00Z").unwrap());

        let all_day = "BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:x\nDTSTART;VALUE=DATE:20261110\nSUMMARY:Offsite\nEND:VEVENT\nEND:VCALENDAR";
        let invitation = parse(all_day.as_bytes()).unwrap();
        assert_eq!(invitation.method, "cancel");
        assert!(invitation.all_day);
        assert_eq!((invitation.start_day.as_deref(), invitation.end_day.as_deref()), (Some("2026-11-10"), Some("2026-11-11")));
        assert_eq!(invitation.start, local_midnight("2026-11-10").unwrap());

        let utc = "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nUID:y\nRECURRENCE-ID:20261111T150000Z\nDTSTART:20261111T160000Z\nDURATION:PT45M\nEND:VEVENT\nEND:VCALENDAR";
        let invitation = parse(utc.as_bytes()).unwrap();
        assert!(invitation.occurrence);
        assert_eq!(invitation.end - invitation.start, 45.0 * 60.0);
        assert_eq!(invitation.title, "(No title)");

        // Published events and answers are no invitation.
        assert!(parse(all_day.replace("METHOD:CANCEL", "METHOD:REPLY").as_bytes()).is_none());
        assert!(parse(b"not a calendar").is_none());
    }

    #[test]
    fn compares_exchange_uids_with_their_series_and_origin() {
        // An occurrence's id carries its date; its series' does not.
        let occurrence = "040000008200E00074C5B7101A82E008".to_string() + "07EA0B09" + "C0B6D8B5A1F0D901000000000000000010000000AABBCCDD";
        let series = "040000008200e00074c5b7101a82e008".to_string() + "00000000" + "c0b6d8b5a1f0d901000000000000000010000000aabbccdd";
        assert_eq!(normalized_uid(&occurrence), normalized_uid(&series));
        // A UID made by Google, as Exchange wraps it.
        let wrapped = "040000008200E00074C5B7101A82E00800000000000000000000000000000000000000002D0000007643616C2D556964010000006162633132334067\
6F6F676C652E636F6D00";
        assert_eq!(normalized_uid(wrapped), "abc123@google.com");
        assert_eq!(normalized_uid(" abc@x "), "abc@x");
    }
}
