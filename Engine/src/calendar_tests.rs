//! The calendar commands end to end: an engine in a throwaway folder, signed in through a test
//! Keychain item, against fake Google Calendar and Graph servers answering what the providers
//! answer. Nothing reaches a provider; no calendar is changed.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::executor::block_on;
use serde_json::{Value, json};

use crate::calendar_api::{local_day, local_midnight, local_zone};
use crate::fake::{FakeRequest, FakeServer, Reply};
use crate::keychain::{Keychain, SignIns};
use crate::mail_tests::{call, wait_until};
use crate::model::iso8601;
use crate::tests::{cleanup, config};
use crate::{Config, Engine, OAuthClient, OAuthConfig, backend, gmail};

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
            assert_eq!((request.param("singleEvents"), request.param("orderBy")), (Some("true"), Some("startTime")));
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

fn engine_for(server: &FakeServer) -> Config {
    let mut config = config(OAuthConfig { token_endpoint: Some(format!("{}/token", server.base)), ..OAuthConfig::default() });
    config.endpoints = backend::Endpoints {
        gmail: gmail::GmailEndpoints {
            root: format!("{}/gmail/v1/users/me/", server.base),
            batch: format!("{}/batch/gmail/v1", server.base),
        },
        graph: format!("{}/v1.0/", server.base),
        chat: format!("{}/chat/v1/", server.base),
        people: format!("{}/people/v1/", server.base),
        calendar: format!("{}/calendar/v3/", server.base),
    };
    std::fs::create_dir_all(&config.support_directory).unwrap();
    let google = ["openid", "https://mail.google.com/", "https://www.googleapis.com/auth/calendar.readonly"];
    let records = json!([
        { "id": GOOGLE, "provider": "google", "label": "Personal", "scopes": google },
        { "id": OUTLOOK, "provider": "microsoft", "label": "Work", "scopes": ["openid", "Mail.ReadWrite", "Calendars.ReadWrite"] },
        { "id": OLD, "provider": "google", "label": OLD, "scopes": ["openid", "https://mail.google.com/"] },
    ]);
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(&records).unwrap()).unwrap();
    let mut sign_ins = SignIns::default();
    for id in [GOOGLE, OUTLOOK, OLD] {
        sign_ins.refresh_tokens.insert(id.into(), "rt".into());
        sign_ins.clients.insert(id.into(), OAuthClient { id: "client".into(), secret: Some("secret".into()) });
    }
    Keychain::new(config.keychain_service.clone()).store(&sign_ins).unwrap();
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
    let config = engine_for(&server);
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
    let names: Vec<&str> = accounts[0]["calendars"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Me", "Team", "Gone"], "the unticked calendar is left out");
    assert_eq!(list["loading"], json!(false));

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

/// Puts `body`'s fields into `event`, as a PATCH does: `null` clears.
fn merge(event: &mut Value, body: &Value) {
    for (key, value) in body.as_object().unwrap() {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.retain(|_, field| !field.is_null());
            // Google answers wall-clock times with their offset: here, as if in UTC.
            if let Some(Value::String(time)) = fields.get_mut("dateTime")
                && !time.ends_with('Z')
            {
                time.push('Z');
            }
        }
        event[key] = value;
    }
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
            assert_eq!(request.header("Prefer"), Some("outlook.timezone=\"UTC\""));
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let mut event = json!({ "id": "review", "isOrganizer": true, "bodyPreview": body["body"]["content"] });
            merge(&mut event, &body);
            events.push(event.clone());
            Reply::json(event)
        }
        (method, path) if path.starts_with("/v1.0/me/events/") => {
            let Some(index) = held(events, path) else { return Reply::status(404, json!({ "error": { "message": "Not found" } })) };
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
    let config = engine_for(&server);
    // This Google sign-in may change events too.
    let google = ["openid", "https://www.googleapis.com/auth/calendar.readonly", "https://www.googleapis.com/auth/calendar.events"];
    let records = json!([
        { "id": GOOGLE, "provider": "google", "label": "Personal", "scopes": google },
        { "id": OUTLOOK, "provider": "microsoft", "label": "Work", "scopes": ["openid", "Calendars.ReadWrite"] },
        { "id": OLD, "provider": "google", "label": OLD, "scopes": ["openid", "https://mail.google.com/"] },
    ]);
    std::fs::write(config.support_directory.join("accounts.json"), serde_json::to_vec(&records).unwrap()).unwrap();
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
    let (_, body) = sent(&server, "POST", "/v1.0/me/calendars/cal-1/events");
    assert_eq!(body["start"], json!({ "dateTime": format!("{day}T15:00:00"), "timeZone": local_zone() }));
    assert_eq!((&body["isAllDay"], &body["body"]), (&json!(false), &json!({ "contentType": "text", "content": "Agenda" })));
    call(&engine, "calendar.update", json!({ "account": OUTLOOK, "calendar": "cal-1", "id": "own", "attendees": ["cy@x.com"], "location": "Room 2" })).unwrap();
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
