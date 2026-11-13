//! The calendar commands end to end: an engine in a throwaway folder, signed in through a test
//! Keychain item, against fake Google Calendar and Graph servers answering what the providers
//! answer. Nothing reaches a provider; no calendar is changed.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::executor::block_on;
use serde_json::{Value, json};

use crate::calendar_api::{local_day, local_midnight, local_zone};
use crate::model::iso8601;
use crate::oauth::{GOOGLE_CALENDARS, GOOGLE_EVENTS};
use crate::testkit::{FakeRequest, FakeServer, Reply, call, cleanup, server_config, sign_in_all, wait_until, write_accounts};
use crate::{Config, Engine};

const GOOGLE: &str = "me@gmail.com";
const OUTLOOK: &str = "me@outlook.com";
/// Signed in before calendars were asked for.
const OLD: &str = "old@gmail.com";

/// `hour` o'clock local time, `days` from today, as the providers write it (UTC).
fn at(days: i64, hour: f64) -> String {
    iso8601(local_midnight(&local_day(days)).unwrap() + hour * 3600.0)
}

fn google_reply(request: &FakeRequest) -> Reply {
    let path = request.path.strip_prefix("/calendar/v3/").unwrap_or_default();
    match path {
        "users/me/calendarList" => Reply::json(json!({ "items": [
            { "id": "me@gmail.com", "summary": "me@gmail.com", "summaryOverride": "Me", "backgroundColor": "#9fe1e7", "primary": true, "selected": true },
            { "id": "team@group", "summary": "Team", "backgroundColor": "#f83a22", "selected": true },
            { "id": "hidden@group", "summary": "Hidden", "selected": false },
            { "id": "gone@group", "summary": "Gone", "selected": true },
        ]})),
        "calendars/me@gmail.com/events" => {
            // No order: Google gives a sync token only to queries it could repeat with one.
            assert_eq!((request.param("singleEvents"), request.param("orderBy")), (Some("true"), None));
            assert!(request.param("timeMin").is_some_and(|time| time.ends_with('Z')));
            Reply::json(json!({ "items": [
                {
                    "id": "standup_20261109", "status": "confirmed", "summary": "Standup", "location": "Room 4",
                    "description": "<p>Agenda:<br>notes &amp; demos</p>", "htmlLink": "https://calendar.google.com/e/1",
                    "hangoutLink": "https://meet.google.com/abc", "start": { "dateTime": at(1, 10.0) }, "end": { "dateTime": at(1, 10.5) },
                    "organizer": { "email": "ann@x.com", "displayName": "Ann" },
                    "attendees": [
                        { "email": "ann@x.com", "displayName": "Ann", "responseStatus": "accepted", "organizer": true },
                        { "email": "me@gmail.com", "responseStatus": "accepted", "self": true },
                        { "email": "room@resource", "responseStatus": "accepted", "resource": true },
                    ],
                },
                { "id": "trip", "summary": "Trip", "start": { "date": local_day(3) }, "end": { "date": local_day(5) } },
                { "id": "dropped", "status": "cancelled" },
                { "id": "mine", "summary": "", "start": { "dateTime": at(0, 9.0) }, "end": { "dateTime": at(0, 9.25) },
                  "organizer": { "email": "me@gmail.com", "self": true } },
            ]}))
        }
        "calendars/team@group/events" => Reply::json(json!({ "items": [
            { "id": "review", "summary": "Review", "start": { "dateTime": at(1, 14.0) }, "end": { "dateTime": at(1, 15.0) },
              "organizer": { "email": "me@gmail.com", "self": true },
              "attendees": [{ "email": "me@gmail.com", "self": true, "responseStatus": "accepted" }, { "email": "bo@x.com", "responseStatus": "needsAction" }] },
        ]})),
        "calendars/gone@group/events" => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
        _ => Reply::status(404, json!({ "error": { "message": "No such path" } })),
    }
}

fn graph_reply(request: &FakeRequest) -> Reply {
    match request.path.as_str() {
        "/v1.0/me/calendars" => Reply::json(json!({ "value": [
            { "id": "cal-1", "name": "Calendar", "hexColor": "", "isDefaultCalendar": true },
        ]})),
        "/v1.0/me/calendars/cal-1/calendarView" => {
            assert_eq!(request.header("Prefer"), Some("outlook.timezone=\"UTC\""));
            let strip = |time: String| time.trim_end_matches('Z').to_string() + ".0000000";
            if request.param("page") == Some("2") {
                return Reply::json(json!({ "value": [
                    { "id": "holiday", "subject": "Holiday", "isAllDay": true,
                      "start": { "dateTime": strip(format!("{}T00:00:00Z", local_day(2))), "timeZone": "UTC" },
                      "end": { "dateTime": strip(format!("{}T00:00:00Z", local_day(3))), "timeZone": "UTC" } },
                ]}));
            }
            assert!(request.param("startDateTime").is_some() && request.param("endDateTime").is_some());
            Reply::json(json!({
                "value": [
                    { "id": "sync", "subject": "Sync", "isAllDay": false, "isOrganizer": false,
                      "start": { "dateTime": strip(at(1, 11.0)), "timeZone": "UTC" }, "end": { "dateTime": strip(at(1, 12.0)), "timeZone": "UTC" },
                      "location": { "displayName": "Teams" }, "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/x" },
                      "organizer": { "emailAddress": { "name": "Cy", "address": "cy@x.com" } },
                      "attendees": [{ "type": "required", "emailAddress": { "name": "Me", "address": "me@outlook.com" }, "status": { "response": "tentativelyAccepted" } }],
                      "responseStatus": { "response": "tentativelyAccepted" }, "webLink": "https://outlook.office.com/e/1", "bodyPreview": "Weekly sync" },
                    { "id": "off", "subject": "Off", "isCancelled": true,
                      "start": { "dateTime": strip(at(1, 8.0)), "timeZone": "UTC" }, "end": { "dateTime": strip(at(1, 9.0)), "timeZone": "UTC" } },
                ],
                "@odata.nextLink": format!("http://{}/v1.0/me/calendars/cal-1/calendarView?page=2", request.header("Host").unwrap()),
            }))
        }
        _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
    }
}

/// A folder on `server` with a Google and an Outlook account that read calendars and one Google
/// account signed in before calendars were asked for, all signed in.
fn config_for(server: &FakeServer) -> Config {
    let config = server_config(server);
    sign_in_all(
        &config,
        &[
            json!({ "id": GOOGLE, "provider": "google", "label": "Personal", "scopes": ["openid", "https://mail.google.com/", GOOGLE_CALENDARS] }),
            json!({ "id": OUTLOOK, "provider": "microsoft", "label": "Work", "scopes": ["openid", "Mail.ReadWrite", "Calendars.ReadWrite"] }),
            json!({ "id": OLD, "provider": "google", "label": OLD, "scopes": ["openid", "https://mail.google.com/"] }),
        ],
    );
    config
}

fn calendar_requests(server: &FakeServer) -> usize {
    let requests = server.requests.lock().unwrap();
    requests.iter().filter(|request| request.path.starts_with("/calendar/") || request.path.contains("/me/calendars")).count()
}

fn events(engine: &Engine, args: Value) -> Value {
    call(engine, "calendar.events", args).unwrap()
}

fn titles(list: &Value) -> Vec<String> {
    list["events"].as_array().unwrap().iter().map(|event| event["title"].as_str().unwrap().to_string()).collect()
}

#[test]
fn lists_every_accounts_events_from_the_cache_and_fetches_only_when_old_or_asked() {
    let offline = Arc::new(AtomicBool::new(false));
    let switch = offline.clone();
    let server = FakeServer::start(move |request| {
        if request.path == "/token" {
            return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
        }
        if switch.load(Ordering::SeqCst) {
            return Reply::status(503, json!({ "error": { "message": "Offline" } }));
        }
        if request.path.starts_with("/calendar/") { google_reply(request) } else { graph_reply(request) }
    });
    let config = config_for(&server);
    let engine = Engine::start(config.clone());
    let heard = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let log = heard.clone();
    engine.subscribe(move |event| log.lock().unwrap().push(event));

    // From a week back: today's own event, both accounts' tomorrow, the trip and the holiday.
    let week = json!({ "from": local_day(-7), "days": 30 });
    wait_until("both accounts' events", || events(&engine, week.clone())["events"].as_array().unwrap().len() == 6);
    let list = events(&engine, week.clone());
    assert_eq!(titles(&list), ["(No title)", "Standup", "Sync", "Review", "Holiday", "Trip"]);
    assert!(heard.lock().unwrap().contains(&"calendar"));
    let accounts = list["accounts"].as_array().unwrap();
    let granted: Vec<(&str, bool)> = accounts.iter().map(|a| (a["id"].as_str().unwrap(), a["granted"].as_bool().unwrap())).collect();
    assert_eq!(granted, [(GOOGLE, true), (OUTLOOK, true), (OLD, false)]);
    let names: Vec<(&str, bool)> =
        accounts[0]["calendars"].as_array().unwrap().iter().map(|c| (c["name"].as_str().unwrap(), c["shown"].as_bool().unwrap())).collect();
    assert_eq!(names, [("Me", true), ("Team", true), ("Hidden", false), ("Gone", true)], "the unticked calendar is listed, not shown");
    assert!(
        !server.requests.lock().unwrap().iter().any(|request| request.path.contains("hidden@group")),
        "a calendar not shown is not fetched"
    );
    assert_eq!(list["loading"], json!(false));
    assert_eq!(list["synced"], json!({ "from": local_day(-7), "until": local_day(61) }), "a week back to 60 days on is fetched");

    let standup = &list["events"][1];
    assert_eq!((standup["account"].as_str(), standup["calendarName"].as_str(), standup["color"].as_str()), (Some(GOOGLE), Some("Me"), Some("#9fe1e7")));
    assert_eq!((standup["start"].as_str(), standup["end"].as_str()), (Some(at(1, 10.0).as_str()), Some(at(1, 10.5).as_str())));
    assert_eq!((standup["location"].as_str(), standup["meetingUrl"].as_str()), (Some("Room 4"), Some("https://meet.google.com/abc")));
    assert_eq!((standup["response"].as_str(), standup["notes"].as_str()), (Some("accepted"), Some("Agenda:\nnotes & demos")));
    assert_eq!(standup["organizer"], json!({ "name": "Ann", "email": "ann@x.com" }));
    assert_eq!(standup["attendees"].as_array().unwrap().len(), 2, "rooms are not guests");
    assert_eq!(standup["allDay"], json!(false));
    // Your own event with guests answers as the organizer's; one without guests has no answer.
    assert_eq!(list["events"][3]["response"], json!("organizer"));
    assert_eq!(list["events"][0].get("response"), None);
    let trip = &list["events"][5];
    assert_eq!((trip["allDay"].as_bool(), trip["startDay"].as_str(), trip["endDay"].as_str()), (Some(true), Some(local_day(3).as_str()), Some(local_day(5).as_str())));
    let sync = &list["events"][2];
    assert_eq!((sync["response"].as_str(), sync["meetingUrl"].as_str(), sync["notes"].as_str()), (Some("tentative"), Some("https://teams.microsoft.com/l/x"), Some("Weekly sync")));
    assert_eq!(sync["start"], json!(at(1, 11.0)));
    assert_eq!((list["events"][4]["startDay"].as_str(), list["events"][4]["color"].clone()), (Some(local_day(2).as_str()), Value::Null));

    // Days and accounts narrow the answer.
    let day = events(&engine, json!({ "from": local_day(1), "days": 1 }));
    assert_eq!(titles(&day), ["Standup", "Sync", "Review"]);
    let work = events(&engine, json!({ "account": "Me@Outlook.com", "from": local_day(-7), "days": 30 }));
    assert_eq!(titles(&work), ["Sync", "Holiday"]);
    assert_eq!(call(&engine, "calendar.events", json!({ "account": "x@y.z" })).unwrap_err().code, "unknown_account");
    assert_eq!(call(&engine, "calendar.events", json!({ "from": "soon" })).unwrap_err().code, "usage");
    assert_eq!(call(&engine, "calendar.refresh", json!({ "account": OLD })).unwrap_err().code, "no_calendar");

    // A fresh copy serves without asking again, here and after a restart.
    let fetched = calendar_requests(&server);
    std::thread::sleep(std::time::Duration::from_millis(200));
    events(&engine, week.clone());
    assert_eq!(calendar_requests(&server), fetched);
    block_on(engine.core.calendar.flush());
    drop(engine);
    offline.store(true, Ordering::SeqCst);
    let engine = Engine::start(config.clone());
    wait_until("the cached events", || events(&engine, week.clone())["events"].as_array().unwrap().len() == 6);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(calendar_requests(&server), fetched, "a fresh copy is not fetched again");

    // Asking fetches now; a failure keeps the copy and says why.
    call(&engine, "calendar.refresh", json!({ "account": GOOGLE })).unwrap();
    wait_until("the failure", || events(&engine, week.clone())["accounts"][0]["problem"].is_string());
    let list = events(&engine, week.clone());
    assert_eq!(list["accounts"][0]["problem"], json!("Offline"));
    assert_eq!(list["accounts"][1]["problem"], Value::Null, "only the account asked for was fetched");
    assert_eq!(titles(&list).len(), 6);

    // Signing out drops the account's events and its copy.
    call(&engine, "accounts.remove", json!({ "account": GOOGLE, "yes": true })).unwrap();
    wait_until("the account to go", || titles(&events(&engine, week.clone())) == ["Sync", "Holiday"]);
    block_on(engine.core.calendar.flush());
    let cache = engine.core.mail.cache();
    assert!(engine.runtime.block_on(cache.load_calendar(GOOGLE)).is_none());
    assert!(engine.runtime.block_on(cache.load_calendar(OUTLOOK)).is_some());
    drop(engine);
    cleanup(&config);
}

/// What the fake providers hold of the events they let be changed, changed by what is sent.
struct Held {
    google: Vec<Value>,
    graph: Vec<Value>,
}

/// A time sent as wall clock in this computer's zone, as the providers keep it: in UTC.
fn in_utc(time: &mut Value) {
    let Some(zone) = time["timeZone"].as_str().filter(|zone| *zone != "UTC") else { return };
    assert_eq!(zone, local_zone());
    let wall = chrono::NaiveDateTime::parse_from_str(time["dateTime"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S").unwrap();
    let utc = chrono::TimeZone::from_local_datetime(&chrono::Local, &wall).earliest().unwrap().with_timezone(&chrono::Utc);
    *time = json!({ "dateTime": utc.format("%Y-%m-%dT%H:%M:%SZ").to_string(), "timeZone": "UTC" });
}

/// Puts `body`'s fields into `event`, as a PATCH does: `null` clears; times are kept in UTC.
fn merge(event: &mut Value, body: &Value) {
    for (key, value) in body.as_object().unwrap() {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.retain(|_, field| !field.is_null());
        }
        if key == "start" || key == "end" {
            in_utc(&mut value);
        }
        event[key] = value;
    }
}

/// What Graph answers a write with: the event, its times in the zone the request named (not
/// in UTC, whatever `Prefer` asks).
fn as_written(event: &Value, body: &Value) -> Value {
    let mut written = event.clone();
    for key in ["start", "end"] {
        if !body[key].is_null() {
            written[key] = body[key].clone();
        }
    }
    written
}

/// Where in `events` the event the path's last part names is.
fn held(events: &[Value], path: &str) -> Option<usize> {
    let id = path.rsplit('/').next()?;
    events.iter().position(|event| event["id"] == id)
}

fn google_write_reply(request: &FakeRequest, held_events: &std::sync::Mutex<Held>) -> Reply {
    let path = request.path.strip_prefix("/calendar/v3/").unwrap_or_default();
    let mut store = held_events.lock().unwrap();
    let events = &mut store.google;
    match (request.method.as_str(), path) {
        ("GET", "users/me/calendarList") => Reply::json(json!({ "items": [
            { "id": GOOGLE, "summary": GOOGLE, "primary": true, "selected": true, "accessRole": "owner" },
            { "id": "team@group", "summary": "Team", "selected": true, "accessRole": "reader" },
        ]})),
        ("GET", "calendars/me@gmail.com/events") => Reply::json(json!({ "items": events })),
        ("GET", "calendars/team@group/events") => Reply::json(json!({ "items": [
            { "id": "offsite", "summary": "Offsite", "start": { "dateTime": at(1, 15.0) }, "end": { "dateTime": at(1, 16.0) },
              "organizer": { "email": "team@group", "self": true } },
        ]})),
        ("POST", "calendars/me@gmail.com/events") => {
            let mut event = json!({ "id": "lunch", "organizer": { "email": GOOGLE, "self": true } });
            merge(&mut event, &serde_json::from_slice(&request.body).unwrap());
            events.push(event.clone());
            Reply::json(event)
        }
        (method, path) if path.starts_with("calendars/me@gmail.com/events/") => {
            let Some(index) = held(events, path) else { return Reply::status(404, json!({ "error": { "message": "Not Found" } })) };
            match method {
                "PATCH" => merge(&mut events[index], &serde_json::from_slice(&request.body).unwrap()),
                "DELETE" => {
                    events.remove(index);
                    return Reply::status(204, Value::Null);
                }
                _ => {}
            }
            Reply::json(events[index].clone())
        }
        _ => Reply::status(404, json!({ "error": { "message": "No such path" } })),
    }
}

fn graph_write_reply(request: &FakeRequest, held_events: &std::sync::Mutex<Held>) -> Reply {
    let mut store = held_events.lock().unwrap();
    let events = &mut store.graph;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1.0/me/calendars") => {
            assert!(request.param("$select").is_some_and(|fields| fields.contains("canEdit")));
            Reply::json(json!({ "value": [{ "id": "cal-1", "name": "Calendar", "isDefaultCalendar": true, "canEdit": true }] }))
        }
        ("GET", "/v1.0/me/calendars/cal-1/calendarView") => Reply::json(json!({ "value": events })),
        ("POST", "/v1.0/me/calendars/cal-1/events") => {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let mut event = json!({ "id": "review", "isOrganizer": true, "bodyPreview": body["body"]["content"] });
            merge(&mut event, &body);
            events.push(event.clone());
            Reply::json(as_written(&event, &body))
        }
        (method, path) if path.starts_with("/v1.0/me/events/") => {
            let Some(index) = held(events, path) else { return Reply::status(404, json!({ "error": { "message": "Not found" } })) };
            match method {
                "PATCH" => {
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    merge(&mut events[index], &body);
                    return Reply::json(as_written(&events[index], &body));
                }
                "DELETE" => {
                    events.remove(index);
                    return Reply::status(204, Value::Null);
                }
                _ => {}
            }
            if request.param("$select") != Some("attendees") {
                assert_eq!(request.header("Prefer"), Some("outlook.timezone=\"UTC\""), "an event is read back in UTC");
            }
            Reply::json(events[index].clone())
        }
        _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
    }
}

/// The last request sent with `method` to a path ending in `path`, with its JSON body.
fn sent(server: &FakeServer, method: &str, path: &str) -> (FakeRequest, Value) {
    let requests = server.requests.lock().unwrap();
    let request = requests.iter().rev().find(|request| request.method == method && request.path.ends_with(path)).unwrap().clone();
    let body = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    (request, body)
}

fn find<'a>(list: &'a Value, title: &str) -> &'a Value {
    list["events"].as_array().unwrap().iter().find(|event| event["title"] == title).unwrap()
}

#[test]
fn adds_changes_and_deletes_the_accounts_own_events_and_keeps_the_copy_up_to_date() {
    let strip = |time: String| time.trim_end_matches('Z').to_string() + ".0000000";
    let me = json!({ "email": GOOGLE, "organizer": true, "self": true, "responseStatus": "accepted" });
    let store = Arc::new(std::sync::Mutex::new(Held {
        google: vec![
            json!({ "id": "plan", "summary": "Plan", "start": { "dateTime": at(1, 9.0) }, "end": { "dateTime": at(1, 10.0) },
                    "organizer": { "email": GOOGLE, "self": true },
                    "attendees": [me, { "email": "bo@x.com", "responseStatus": "accepted" }, { "email": "cy@x.com", "responseStatus": "declined" }] }),
            json!({ "id": "standup", "summary": "Standup", "start": { "dateTime": at(1, 10.0) }, "end": { "dateTime": at(1, 10.5) },
                    "organizer": { "email": "ann@x.com" }, "attendees": [{ "email": GOOGLE, "self": true, "responseStatus": "accepted" }] }),
            json!({ "id": "bday", "summary": "Birthday", "eventType": "birthday", "start": { "date": local_day(2) }, "end": { "date": local_day(3) },
                    "organizer": { "email": GOOGLE, "self": true } }),
        ],
        graph: vec![
            json!({ "id": "own", "subject": "1:1", "isOrganizer": true,
                    "start": { "dateTime": strip(at(1, 13.0)), "timeZone": "UTC" }, "end": { "dateTime": strip(at(1, 13.5)), "timeZone": "UTC" },
                    "attendees": [{ "type": "required", "emailAddress": { "address": "bo@x.com" }, "status": { "response": "accepted" } }] }),
            json!({ "id": "sync", "subject": "Sync", "isOrganizer": false,
                    "start": { "dateTime": strip(at(1, 11.0)), "timeZone": "UTC" }, "end": { "dateTime": strip(at(1, 12.0)), "timeZone": "UTC" } }),
        ],
    }));
    let offline = Arc::new(AtomicBool::new(false));
    let (held_events, switch) = (store.clone(), offline.clone());
    let server = FakeServer::start(move |request| {
        if request.path == "/token" {
            return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
        }
        if switch.load(Ordering::SeqCst) {
            return Reply::status(503, json!({ "error": { "message": "Offline" } }));
        }
        if request.path.starts_with("/calendar/") { google_write_reply(request, &held_events) } else { graph_write_reply(request, &held_events) }
    });
    let config = config_for(&server);
    // This Google sign-in may change events too.
    write_accounts(
        &config,
        &[
            json!({ "id": GOOGLE, "provider": "google", "label": "Personal", "scopes": ["openid", GOOGLE_CALENDARS, GOOGLE_EVENTS] }),
            json!({ "id": OUTLOOK, "provider": "microsoft", "label": "Work", "scopes": ["openid", "Calendars.ReadWrite"] }),
            json!({ "id": OLD, "provider": "google", "label": OLD, "scopes": ["openid", "https://mail.google.com/"] }),
        ],
    );
    let engine = Engine::start(config.clone());
    let week = json!({ "from": local_day(-7), "days": 30 });

    // Only the account's own ordinary events on calendars it may write to can be changed.
    wait_until("both accounts' events", || titles(&events(&engine, week.clone())).len() == 6);
    let list = events(&engine, week.clone());
    for (title, editable) in [("Plan", true), ("Standup", false), ("Birthday", false), ("Offsite", false), ("1:1", true), ("Sync", false)] {
        assert_eq!(find(&list, title)["editable"], json!(editable), "{title}");
    }
    let writable: Vec<&Value> = list["accounts"].as_array().unwrap().iter().map(|account| &account["writable"]).collect();
    assert_eq!(writable, [&json!(true), &json!(true), &json!(false)]);
    assert_eq!(list["timeZone"], json!(local_zone()));
    let calendars = &list["accounts"][0]["calendars"];
    assert_eq!((&calendars[0]["writable"], &calendars[1]["writable"]), (&json!(true), &json!(false)));

    // A new Google event: an hour long unless said, its guests invited, in the calendar at once.
    let day = local_day(2);
    let lunch = call(
        &engine,
        "calendar.create",
        json!({ "account": GOOGLE, "title": " Lunch ", "start": format!("{day}T12:00"), "location": "Cafe", "attendees": ["dee@x.com"] }),
    )
    .unwrap();
    assert_eq!((&lunch["title"], &lunch["editable"], &lunch["calendar"], &lunch["location"]), (&json!("Lunch"), &json!(true), &json!(GOOGLE), &json!("Cafe")));
    assert_eq!((&lunch["start"], &lunch["end"]), (&json!(at(2, 12.0)), &json!(at(2, 13.0))));
    let (request, body) = sent(&server, "POST", "calendars/me@gmail.com/events");
    assert_eq!(request.param("sendUpdates"), Some("all"));
    assert_eq!(body["start"], json!({ "dateTime": format!("{day}T12:00:00"), "timeZone": local_zone() }));
    assert_eq!(body["end"]["dateTime"], json!(format!("{day}T13:00:00")));
    assert_eq!(body["attendees"], json!([{ "email": "dee@x.com" }]));
    assert!(titles(&events(&engine, week.clone())).contains(&"Lunch".to_string()));

    // A change sends only what changed; guests who stay keep their answers, the organizer stays.
    let plan = call(
        &engine,
        "calendar.update",
        json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "plan", "title": "Planning", "attendees": ["bo@x.com", "dee@x.com"] }),
    )
    .unwrap();
    assert_eq!(plan["title"], json!("Planning"));
    let (request, body) = sent(&server, "PATCH", "calendars/me@gmail.com/events/plan");
    assert_eq!(request.param("sendUpdates"), Some("all"));
    assert_eq!(body.as_object().unwrap().keys().collect::<Vec<_>>(), ["attendees", "summary"]);
    assert_eq!(body["attendees"], json!([me, { "email": "bo@x.com", "responseStatus": "accepted" }, { "email": "dee@x.com" }]));
    assert_eq!(find(&events(&engine, week.clone()), "Planning")["attendees"].as_array().unwrap().len(), 3);

    // Timed to all-day clears the times.
    call(&engine, "calendar.update", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "plan", "start": local_day(3), "all-day": true })).unwrap();
    let (_, body) = sent(&server, "PATCH", "calendars/me@gmail.com/events/plan");
    assert_eq!(body["start"], json!({ "date": local_day(3), "dateTime": null, "timeZone": null }));
    assert_eq!(body["end"], json!({ "date": local_day(4), "dateTime": null, "timeZone": null }));
    let planning = find(&events(&engine, week.clone()), "Planning").clone();
    assert_eq!((&planning["allDay"], &planning["startDay"]), (&json!(true), &json!(local_day(3))));

    // Refused before anything is sent (the fetches after the changes above may still read).
    let writes = || server.requests.lock().unwrap().iter().filter(|request| request.method != "GET").count();
    let asked = writes();
    let refused = |command: &str, args: Value| call(&engine, command, args).unwrap_err().code;
    assert_eq!(refused("calendar.update", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "standup", "title": "Mine" })), "not_organizer");
    assert_eq!(refused("calendar.update", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "bday", "title": "Mine" })), "not_organizer");
    assert_eq!(refused("calendar.update", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "plan" })), "usage");
    assert_eq!(refused("calendar.update", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "gone", "title": "X" })), "not_found");
    assert_eq!(refused("calendar.create", json!({ "account": GOOGLE, "calendar": "team@group", "title": "X", "start": format!("{day}T09:00") })), "read_only");
    assert_eq!(refused("calendar.create", json!({ "account": GOOGLE, "title": "X" })), "usage");
    assert_eq!(refused("calendar.create", json!({ "account": GOOGLE, "title": "X", "start": format!("{day}T09:00"), "end": format!("{day}T08:00") })), "usage");
    assert_eq!(refused("calendar.create", json!({ "account": GOOGLE, "title": "X", "start": format!("{day}T09:00"), "attendees": ["bo"] })), "usage");
    assert_eq!(refused("calendar.create", json!({ "account": OLD, "title": "X", "start": format!("{day}T09:00") })), "no_calendar");
    let unconfirmed = call(&engine, "calendar.delete", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "plan" })).unwrap_err();
    assert_eq!(unconfirmed.code, "confirm");
    assert!(unconfirmed.message.contains("\"Planning\"") && unconfirmed.message.contains("its 2 guests"), "{}", unconfirmed.message);
    assert_eq!(writes(), asked);

    // Deleting tells the guests and drops it from the calendar at once.
    assert_eq!(call(&engine, "calendar.delete", json!({ "account": GOOGLE, "calendar": GOOGLE, "id": "plan", "yes": true })).unwrap(), json!({ "deleted": true }));
    let (request, _) = sent(&server, "DELETE", "calendars/me@gmail.com/events/plan");
    assert_eq!(request.param("sendUpdates"), Some("all"));
    assert!(!titles(&events(&engine, week.clone())).contains(&"Planning".to_string()));

    // Outlook: the same through Graph.
    let review = call(
        &engine,
        "calendar.create",
        json!({ "account": OUTLOOK, "title": "Review", "start": format!("{day}T15:00"), "end": format!("{day}T15:30"), "notes": "Agenda" }),
    )
    .unwrap();
    assert_eq!((&review["title"], &review["editable"], &review["notes"]), (&json!("Review"), &json!(true), &json!("Agenda")));
    // Graph answers the write in the zone sent; the event is read back in UTC.
    assert_eq!((&review["start"], &review["end"]), (&json!(at(2, 15.0)), &json!(at(2, 15.5))));
    let (_, body) = sent(&server, "POST", "/v1.0/me/calendars/cal-1/events");
    assert_eq!(body["start"], json!({ "dateTime": format!("{day}T15:00:00"), "timeZone": local_zone() }));
    assert_eq!((&body["isAllDay"], &body["body"]), (&json!(false), &json!({ "contentType": "text", "content": "Agenda" })));
    let own = call(
        &engine,
        "calendar.update",
        json!({ "account": OUTLOOK, "calendar": "cal-1", "id": "own", "attendees": ["cy@x.com"], "location": "Room 2", "start": format!("{day}T16:00") }),
    )
    .unwrap();
    assert_eq!((&own["start"], &own["end"]), (&json!(at(2, 16.0)), &json!(at(2, 17.0))));
    let (_, body) = sent(&server, "PATCH", "/v1.0/me/events/own");
    assert_eq!(body["attendees"], json!([{ "emailAddress": { "address": "cy@x.com" }, "type": "required" }]));
    assert_eq!(body["location"], json!({ "displayName": "Room 2" }));
    assert_eq!(refused("calendar.update", json!({ "account": OUTLOOK, "calendar": "cal-1", "id": "sync", "title": "Mine" })), "not_organizer");
    call(&engine, "calendar.delete", json!({ "account": OUTLOOK, "calendar": "cal-1", "id": "own", "yes": true })).unwrap();
    sent(&server, "DELETE", "/v1.0/me/events/own");

    // The fetches that follow agree, and the copy outlives a restart.
    wait_until("the fetches after the changes", || !events(&engine, week.clone())["loading"].as_bool().unwrap());
    let mut now_listed = titles(&events(&engine, week.clone()));
    now_listed.sort();
    assert_eq!(now_listed, ["Birthday", "Lunch", "Offsite", "Review", "Standup", "Sync"]);
    block_on(engine.core.calendar.flush());
    drop(engine);
    offline.store(true, Ordering::SeqCst);
    let engine = Engine::start(config.clone());
    wait_until("the saved copy", || {
        let mut listed = titles(&events(&engine, week.clone()));
        listed.sort();
        listed == now_listed
    });
    drop(engine);
    cleanup(&config);
}

const ANN: &str = "ann@x.com";
const HOLIDAYS: &str = "holidays@group";

/// The requests for `calendar`'s events so far: whole (with times) and by sync token.
fn event_requests(server: &FakeServer, calendar: &str) -> (usize, Vec<String>) {
    let path = format!("/calendar/v3/calendars/{calendar}/events");
    let requests = server.requests.lock().unwrap();
    let asked: Vec<&FakeRequest> = requests.iter().filter(|request| request.path == path).collect();
    let tokens = asked.iter().filter_map(|request| request.param("syncToken").map(String::from)).collect::<Vec<_>>();
    (asked.len() - tokens.len(), tokens)
}

fn calendar_of<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["accounts"][0]["calendars"].as_array().unwrap().iter().find(|calendar| calendar["id"] == id).unwrap()
}

#[test]
fn shows_others_calendars_read_only_in_their_colours_and_syncs_each_by_its_token() {
    // Ann's calendar, shared read-only: the standup (with Ann's answer) and her focus time;
    // after `changed`, the focus time is gone and a dentist visit added.
    let changed = Arc::new(AtomicBool::new(false));
    let expired = Arc::new(AtomicBool::new(false));
    let (after, gone) = (changed.clone(), expired.clone());
    let server = FakeServer::start(move |request| {
        if request.path == "/token" {
            return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
        }
        let path = request.path.strip_prefix("/calendar/v3/").unwrap_or_default();
        let token = request.param("syncToken");
        if path.ends_with("/events") {
            assert_eq!(request.param("singleEvents"), Some("true"));
            assert_eq!(request.param("timeMin").is_some(), token.is_none(), "a sync token stands for the whole query: {path}");
        }
        let standup = |me: &str, answer: &str| {
            json!({ "id": "standup", "summary": "Standup", "iCalUID": "standup@x.com",
                    "start": { "dateTime": at(1, 10.0) }, "end": { "dateTime": at(1, 10.5) },
                    "organizer": { "email": "bo@x.com" },
                    "attendees": [{ "email": me, "self": true, "responseStatus": answer }, { "email": "bo@x.com", "organizer": true }] })
        };
        let focus = json!({ "id": "focus", "summary": "Focus", "start": { "dateTime": at(1, 13.0) }, "end": { "dateTime": at(1, 15.0) },
                            "organizer": { "email": ANN, "self": true } });
        let dentist = json!({ "id": "dentist", "summary": "Dentist", "start": { "dateTime": at(2, 9.0) }, "end": { "dateTime": at(2, 10.0) },
                              "organizer": { "email": ANN, "self": true } });
        let changed = after.load(Ordering::SeqCst);
        match (path, token) {
            ("users/me/calendarList", _) => Reply::json(json!({ "items": [
                { "id": GOOGLE, "summary": GOOGLE, "backgroundColor": "#9fe1e7", "primary": true, "selected": true, "accessRole": "owner" },
                { "id": ANN, "summary": ANN, "summaryOverride": "Ann Lee", "backgroundColor": "#f83a22", "selected": true, "accessRole": "reader" },
                { "id": HOLIDAYS, "summary": "Holidays", "backgroundColor": "#16a765", "accessRole": "reader" },
                { "id": "boss@x.com", "summary": "Boss", "selected": true, "hidden": true, "accessRole": "freeBusyReader" },
            ]})),
            ("calendars/me@gmail.com/events", None) => {
                Reply::json(json!({ "items": [standup(GOOGLE, "needsAction")], "nextSyncToken": "me-1" }))
            }
            ("calendars/me@gmail.com/events", Some(_)) => Reply::json(json!({ "items": [], "nextSyncToken": "me-1" })),
            ("calendars/ann@x.com/events", None) => {
                let last = if changed { dentist } else { focus };
                Reply::json(json!({ "items": [standup(ANN, "accepted"), last], "nextSyncToken": "ann-1" }))
            }
            ("calendars/ann@x.com/events", Some("ann-1")) if changed => Reply::json(json!({
                "items": [{ "id": "focus", "status": "cancelled" }, dentist], "nextSyncToken": "ann-2",
            })),
            ("calendars/ann@x.com/events", Some("ann-1")) => Reply::json(json!({ "items": [], "nextSyncToken": "ann-1" })),
            // A token Google no longer takes.
            ("calendars/ann@x.com/events", Some(_)) if gone.load(Ordering::SeqCst) => {
                Reply::status(410, json!({ "error": { "message": "Sync token is no longer valid" } }))
            }
            ("calendars/ann@x.com/events", Some(token)) => Reply::json(json!({ "items": [], "nextSyncToken": token })),
            ("calendars/holidays@group/events", None) => Reply::json(json!({
                "items": [{ "id": "day", "summary": "Holiday", "start": { "date": local_day(3) }, "end": { "date": local_day(4) } }],
                "nextSyncToken": "h-1",
            })),
            ("calendars/holidays@group/events", Some(_)) => Reply::json(json!({ "items": [], "nextSyncToken": "h-1" })),
            _ => Reply::status(404, json!({ "error": { "message": "No such path" } })),
        }
    });
    let config = server_config(&server);
    let scopes = ["openid", "https://mail.google.com/", GOOGLE_CALENDARS, GOOGLE_EVENTS];
    sign_in_all(&config, &[json!({ "id": GOOGLE, "provider": "google", "label": "Personal", "scopes": scopes })]);
    let engine = Engine::start(config.clone());
    let week = json!({ "from": local_day(-7), "days": 30 });

    // Both shown calendars' events, each in its calendar's colour; only the account's own
    // answer can be given.
    wait_until("both calendars' events", || titles(&events(&engine, week.clone())).len() == 3);
    let list = events(&engine, week.clone());
    assert_eq!(titles(&list), ["Standup", "Standup", "Focus"]);
    let of = |calendar: &str, title: &str| {
        list["events"].as_array().unwrap().iter().find(|event| event["calendar"] == calendar && event["title"] == title).unwrap().clone()
    };
    let (mine, anns, focus) = (of(GOOGLE, "Standup"), of(ANN, "Standup"), of(ANN, "Focus"));
    assert_eq!((mine["response"].as_str(), mine["own"].as_bool(), mine["color"].as_str()), (Some("needsAction"), Some(true), Some("#9fe1e7")));
    assert_eq!((anns["response"].as_str(), anns["own"].as_bool(), anns["color"].as_str()), (Some("accepted"), Some(false), Some("#f83a22")));
    assert_eq!((focus["calendarName"].as_str(), focus["mine"].as_bool(), focus["editable"].as_bool()), (Some("Ann Lee"), Some(true), Some(false)));
    let calendars: Vec<(&str, &str, bool, bool, bool, bool)> = list["accounts"][0]["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let flag = |name: &str| c[name].as_bool().unwrap();
            (c["name"].as_str().unwrap(), c["access"].as_str().unwrap(), flag("selected"), flag("hidden"), flag("writable"), flag("shown"))
        })
        .collect();
    assert_eq!(
        calendars,
        [
            (GOOGLE, "owner", true, false, true, true),
            ("Ann Lee", "reader", true, false, false, true),
            ("Holidays", "reader", false, false, false, false),
            ("Boss", "freeBusyReader", true, true, false, false),
        ]
    );
    assert_eq!(event_requests(&server, HOLIDAYS), (0, vec![]), "calendars not shown are not fetched");
    assert_eq!(event_requests(&server, "boss@x.com"), (0, vec![]));

    // Nothing answers or changes Ann's events from here.
    let answer = json!({ "account": GOOGLE, "calendar": ANN, "event": "standup", "response": "declined", "yes": true });
    assert_eq!(call(&engine, "calendar.respond", answer).unwrap_err().code, "read_only");
    let delete = json!({ "account": GOOGLE, "calendar": ANN, "id": "focus", "yes": true });
    assert_eq!(call(&engine, "calendar.delete", delete).unwrap_err().code, "read_only");

    // A refresh asks each calendar only for what changed since its last fetch.
    changed.store(true, Ordering::SeqCst);
    call(&engine, "calendar.refresh", json!({ "account": GOOGLE })).unwrap();
    wait_until("Ann's changes", || titles(&events(&engine, week.clone())) == ["Standup", "Standup", "Dentist"]);
    assert_eq!(event_requests(&server, ANN), (1, vec!["ann-1".to_string()]));
    assert_eq!(event_requests(&server, GOOGLE), (1, vec!["me-1".to_string()]));

    // Turning the holidays on fetches them alone whole; turning Ann off hides hers at once and
    // fetches them no more.
    call(&engine, "calendar.show", json!({ "account": GOOGLE, "calendar": HOLIDAYS })).unwrap();
    wait_until("the holidays", || titles(&events(&engine, week.clone())).contains(&"Holiday".to_string()));
    assert_eq!(event_requests(&server, HOLIDAYS), (1, vec![]));
    assert_eq!(event_requests(&server, ANN), (1, vec!["ann-1".to_string(), "ann-2".to_string()]), "the others by their tokens");
    call(&engine, "calendar.show", json!({ "account": GOOGLE, "calendar": ANN, "off": true })).unwrap();
    assert_eq!(titles(&events(&engine, week.clone())), ["Standup", "Holiday"]);
    wait_until("the fetch to settle", || events(&engine, week.clone())["loading"] == json!(false));
    call(&engine, "calendar.refresh", json!({ "account": GOOGLE })).unwrap();
    wait_until("the refresh", || event_requests(&server, HOLIDAYS).1.len() == 1);
    wait_until("the fetch to settle", || events(&engine, week.clone())["loading"] == json!(false));
    assert_eq!(event_requests(&server, ANN).1.len(), 2, "a hidden calendar is not fetched");
    assert_eq!(call(&engine, "calendar.show", json!({ "account": GOOGLE, "calendar": "nope" })).unwrap_err().code, "not_found");

    // The choices outlive a restart, and so does each calendar's copy.
    block_on(engine.core.calendar.flush());
    drop(engine);
    let engine = Engine::start(config.clone());
    wait_until("the cached calendars", || titles(&events(&engine, week.clone())) == ["Standup", "Holiday"]);
    let list = events(&engine, week.clone());
    assert_eq!((calendar_of(&list, ANN)["shown"].as_bool(), calendar_of(&list, HOLIDAYS)["shown"].as_bool()), (Some(false), Some(true)));

    // Ann's again: her copy paints at once; Google no longer takes its token, so it is fetched whole.
    call(&engine, "calendar.show", json!({ "account": GOOGLE, "calendar": ANN })).unwrap();
    assert_eq!(titles(&events(&engine, week.clone())), ["Standup", "Standup", "Dentist", "Holiday"]);
    expired.store(true, Ordering::SeqCst);
    call(&engine, "calendar.refresh", json!({ "account": GOOGLE })).unwrap();
    wait_until("Ann's whole fetch", || event_requests(&server, ANN).0 == 2);
    assert_eq!(event_requests(&server, ANN).1, ["ann-1", "ann-2", "ann-2"]);
    wait_until("the fetch to settle", || events(&engine, week.clone())["loading"] == json!(false));
    assert_eq!(titles(&events(&engine, week.clone())), ["Standup", "Standup", "Dentist", "Holiday"]);
    drop(engine);
    cleanup(&config);
}
