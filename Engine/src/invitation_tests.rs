//! Meeting invitations in mail end to end: an engine in a throwaway folder, signed in through a
//! test Keychain item, against fake Gmail + Google Calendar and Outlook (Graph) servers. The
//! reader's card reads `mail.show`'s invitation and `calendar.invitation`; answers go through
//! the calendar APIs (`calendar.respond`), never as mail. Nothing reaches a provider.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::calendar_api::{local_day, local_midnight};
use crate::mail_tests::{BASE_MS, encode, gmail_batch, graph_message, header};
use crate::model::iso8601;
use crate::oauth::{GOOGLE_CALENDARS, GOOGLE_EVENTS};
use crate::testkit::{FakeRequest, FakeServer, Reply, call, cleanup, server_config, sign_in_all, wait_until};
use crate::{Config, Engine};

const GOOGLE: &str = "me@gmail.com";
const OUTLOOK: &str = "me@outlook.com";
const EXCHANGE_UID: &str = "040000008200E00074C5B7101A82E0080000000010B2C3D4E5F6A7B8000000000000000010000000AABBCCDD";

/// `hour` o'clock local time, `days` from today, as the providers write it (UTC).
fn at(days: i64, hour: f64) -> String {
    iso8601(local_midnight(&local_day(days)).unwrap() + hour * 3600.0)
}

/// `at` as iCalendar writes a UTC time: `20261110T150000Z`.
fn ics_time(days: i64, hour: f64) -> String {
    at(days, hour).replace(['-', ':'], "")
}

/// An engine on `server` signed in to `accounts` (`(id, provider)`), each with mail and every
/// calendar scope.
fn engine_for(server: &FakeServer, accounts: &[(&str, &str)]) -> (Engine, Config) {
    let config = server_config(server);
    let records: Vec<Value> = accounts
        .iter()
        .map(|(id, provider)| {
            let scopes = match *provider {
                "google" => json!(["openid", "https://mail.google.com/", GOOGLE_CALENDARS, GOOGLE_EVENTS]),
                _ => json!(["openid", "Mail.ReadWrite", "Calendars.ReadWrite"]),
            };
            json!({ "id": id, "provider": provider, "label": id, "scopes": scopes })
        })
        .collect();
    sign_in_all(&config, &records);
    (Engine::start(config.clone()), config)
}

fn count(server: &FakeServer, matches: impl Fn(&FakeRequest) -> bool) -> usize {
    server.requests.lock().unwrap().iter().filter(|request| matches(request)).count()
}

// MARK: - Gmail and Google Calendar

/// A Google Calendar invitation as Gmail delivers it: the text/calendar alternative and the
/// invite.ics attachment.
fn google_invitation(uid: &str, start: &str, end: &str, rule: Option<&str>) -> String {
    let rule = rule.map(|rule| format!("RRULE:{rule}\r\n")).unwrap_or_default();
    format!(
        "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
DTSTART:{start}\r\nDTEND:{end}\r\n{rule}ORGANIZER;CN=Ann Lee:mailto:ann@x.com\r\nUID:{uid}\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;CN=me@gmail.com:mailto:me@gmail.com\r\n\
SUMMARY:Design review\r\nLOCATION:Room 4\r\nSEQUENCE:0\r\nSTATUS:CONFIRMED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn gmail_message(id: &str, subject: &str, hours: i64) -> Value {
    json!({
        "id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX", "UNREAD"],
        "internalDate": (BASE_MS - hours * 3_600_000).to_string(), "sizeEstimate": 100,
        "payload": { "mimeType": "multipart/mixed", "headers": [
            header("From", "Ann Lee <ann@x.com>"), header("To", "me@gmail.com"), header("Subject", subject),
            header("Message-ID", &format!("<{id}@x>")),
        ]},
    })
}

fn gmail_full(id: &str, ics: &str) -> Value {
    json!({ "id": id, "payload": {
        "mimeType": "multipart/mixed",
        "parts": [
            { "mimeType": "multipart/alternative", "parts": [
                { "mimeType": "text/plain", "body": { "data": encode("You have been invited to Design review.") } },
                { "mimeType": "text/html", "body": { "data": encode("<p>You have been invited to Design review.</p>") } },
                { "mimeType": "text/calendar", "headers": [header("Content-Type", "text/calendar; charset=UTF-8; method=REQUEST")],
                  "body": { "data": encode(ics) } },
            ]},
            { "mimeType": "application/ics", "filename": "invite.ics", "body": { "data": encode(ics) } },
        ],
    }})
}

/// The account's own answer on the fake Google Calendar, by event.
type Answers = Arc<Mutex<HashMap<String, String>>>;

fn google_event(id: &str, uid: &str, start: &str, end: &str, answer: &str) -> Value {
    json!({
        "id": id, "iCalUID": uid, "status": "confirmed", "summary": "Design review", "location": "Room 4",
        "htmlLink": format!("https://calendar.google.com/e/{id}"),
        "start": { "dateTime": start }, "end": { "dateTime": end },
        "organizer": { "email": "ann@x.com", "displayName": "Ann Lee" },
        "attendees": [
            { "email": "ann@x.com", "displayName": "Ann Lee", "organizer": true, "responseStatus": "accepted" },
            { "email": "me@gmail.com", "self": true, "responseStatus": answer },
        ],
    })
}

fn google_server(answers: Answers, patches: Arc<Mutex<Vec<(String, Value)>>>) -> FakeServer {
    let metadata: HashMap<String, Value> = HashMap::from([
        ("inv1".to_string(), gmail_message("inv1", "Invitation: Design review", 1)),
        ("inv2".to_string(), gmail_message("inv2", "Invitation: Planning (weekly)", 2)),
    ]);
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
        }
        if path == "/batch/gmail/v1" {
            return gmail_batch(request, &metadata);
        }
        if let Some(resource) = path.strip_prefix("/gmail/v1/users/me/") {
            return match resource {
                "profile" => Reply::json(json!({ "emailAddress": GOOGLE, "historyId": "100" })),
                "settings/sendAs" => Reply::json(json!({ "sendAs": [{ "sendAsEmail": GOOGLE, "isPrimary": true }] })),
                "labels" => Reply::json(json!({ "labels": [
                    { "id": "INBOX", "name": "INBOX", "type": "system" }, { "id": "SENT", "name": "SENT", "type": "system" },
                ]})),
                "history" => Reply::json(json!({ "historyId": "101" })),
                "messages" => {
                    let ids: Vec<Value> = match request.param("labelIds") {
                        Some("INBOX") => vec![json!({ "id": "inv1" }), json!({ "id": "inv2" })],
                        _ => vec![],
                    };
                    Reply::json(json!({ "resultSizeEstimate": ids.len(), "messages": ids }))
                }
                "messages/inv1" => Reply::json(gmail_full("inv1", &google_invitation("design@google.com", &ics_time(2, 15.0), &ics_time(2, 16.0), None))),
                "messages/inv2" => Reply::json(gmail_full(
                    "inv2",
                    &google_invitation("planning@google.com", &ics_time(90, 9.0), &ics_time(90, 9.5), Some("FREQ=WEEKLY;BYDAY=MO")),
                )),
                _ => Reply::status(404, json!({ "error": { "message": "Not Found" } })),
            };
        }
        let Some(resource) = path.strip_prefix("/calendar/v3/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        let answer = |id: &str| answers.lock().unwrap().get(id).cloned().unwrap_or_else(|| "needsAction".into());
        match (request.method.as_str(), resource) {
            ("GET", "users/me/calendarList") => Reply::json(json!({ "items": [
                { "id": GOOGLE, "summary": GOOGLE, "backgroundColor": "#9fe1e7", "primary": true, "selected": true },
            ]})),
            ("GET", "calendars/me@gmail.com/events") => Reply::json(json!({ "items": [
                google_event("ev1", "design@google.com", &at(2, 15.0), &at(2, 16.0), &answer("ev1")),
            ]})),
            // Asked by UID: the weekly meeting, months out, is outside the fetched window.
            ("GET", "calendars/primary/events") => {
                assert_eq!(request.param("iCalUID"), Some("planning@google.com"));
                Reply::json(json!({ "items": [google_event("series2", "planning@google.com", &at(90, 9.0), &at(90, 9.5), &answer("series2"))] }))
            }
            ("GET", "calendars/me@gmail.com/events/ev1") => {
                Reply::json(google_event("ev1", "design@google.com", &at(2, 15.0), &at(2, 16.0), &answer("ev1")))
            }
            ("GET", "calendars/primary/events/series2") => {
                Reply::json(google_event("series2", "planning@google.com", &at(90, 9.0), &at(90, 9.5), &answer("series2")))
            }
            ("PATCH", event) => {
                assert_eq!(request.param("sendUpdates"), Some("all"), "the organizer hears the answer");
                assert_eq!(request.header("Content-Type"), Some("application/json"));
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let id = event.rsplit('/').next().unwrap().to_string();
                let mine = body["attendees"].as_array().unwrap().iter().find(|a| a["self"] == json!(true)).unwrap();
                answers.lock().unwrap().insert(id.clone(), mine["responseStatus"].as_str().unwrap().to_string());
                patches.lock().unwrap().push((event.to_string(), body));
                Reply::json(json!({ "id": id }))
            }
            _ => Reply::status(404, json!({ "error": { "message": "No such path" } })),
        }
    })
}

fn invitation(engine: &Engine, id: &str, message: &str) -> Value {
    call(engine, "calendar.invitation", json!({ "id": id, "message": message })).unwrap()
}

#[test]
fn answers_google_invitations_through_google_calendar() {
    let answers: Answers = Arc::default();
    let patches = Arc::new(Mutex::new(Vec::new()));
    let server = google_server(answers.clone(), patches.clone());
    let (engine, config) = engine_for(&server, &[(GOOGLE, "google")]);
    let design = "me@gmail.com|inv1@x";
    let planning = "me@gmail.com|inv2@x";
    wait_until("the invitations listed", || {
        call(&engine, "mail.list", json!({})).is_ok_and(|list| list["conversations"].as_array().is_some_and(|c| c.len() == 2))
    });

    // The reader gets the invitation with the message.
    let shown = call(&engine, "mail.show", json!({ "id": design })).unwrap();
    let card = &shown["messages"][0]["invitation"];
    assert_eq!((card["method"].as_str(), card["title"].as_str(), card["location"].as_str()), (Some("request"), Some("Design review"), Some("Room 4")));
    assert_eq!((card["start"].as_str(), card["end"].as_str()), (Some(at(2, 15.0).as_str()), Some(at(2, 16.0).as_str())));
    assert_eq!(card["organizer"], json!({ "name": "Ann Lee", "email": "ann@x.com" }));
    assert_eq!((card["recurring"].clone(), card["occurrence"].clone()), (json!(false), json!(false)));

    // Matched to the fetched event by its UID, with the account's answer.
    wait_until("the calendar fetched", || !invitation(&engine, design, "inv1")["event"].is_null());
    let found = invitation(&engine, design, "inv1");
    assert_eq!((found["granted"].clone(), found["editable"].clone()), (json!(true), json!(true)));
    assert_eq!((found["target"].as_str(), found["calendar"].as_str()), (Some("ev1"), Some(GOOGLE)));
    assert_eq!((found["event"]["response"].as_str(), found["event"]["calendarName"].as_str()), (Some("needsAction"), Some(GOOGLE)));

    // Answering asks for confirmation, then patches the guest list, the organizer told.
    let respond = |event: &str, calendar: &str, response: &str, yes: bool| {
        call(&engine, "calendar.respond", json!({ "account": GOOGLE, "event": event, "calendar": calendar, "response": response, "yes": yes }))
    };
    assert_eq!(respond("ev1", GOOGLE, "accepted", false).unwrap_err().code, "confirm");
    assert_eq!(respond("ev1", GOOGLE, "maybe", true).unwrap_err().code, "usage");
    assert!(patches.lock().unwrap().is_empty());
    let fetches = count(&server, |request| request.path == "/calendar/v3/calendars/me@gmail.com/events");
    assert_eq!(respond("ev1", GOOGLE, "accepted", true).unwrap(), json!({ "responded": "accepted" }));
    {
        let patches = patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].0, "calendars/me@gmail.com/events/ev1");
        // The rest of the guest list goes back as it was.
        assert_eq!(patches[0].1["attendees"][0], json!({ "email": "ann@x.com", "displayName": "Ann Lee", "organizer": true, "responseStatus": "accepted" }));
        assert_eq!(patches[0].1["attendees"][1]["responseStatus"], "accepted");
    }
    // The card and the calendar show it at once, and the account is fetched again.
    assert_eq!(invitation(&engine, design, "inv1")["event"]["response"], "accepted");
    let day = call(&engine, "calendar.events", json!({ "from": local_day(2), "days": 1 })).unwrap();
    assert_eq!(day["events"][0]["response"], "accepted");
    wait_until("the calendar fetched again", || {
        count(&server, |request| request.path == "/calendar/v3/calendars/me@gmail.com/events") > fetches
    });
    assert_eq!(invitation(&engine, design, "inv1")["event"]["response"], "accepted");
    assert!(
        count(&server, |request| request.method == "POST" && request.path.contains("messages/send")) == 0,
        "no reply is mailed"
    );

    // A weekly meeting months out: asked of Google by UID once, answered as a series.
    let card = &call(&engine, "mail.show", json!({ "id": planning })).unwrap()["messages"][0]["invitation"];
    assert_eq!(card["recurring"], json!(true));
    let found = invitation(&engine, planning, "inv2");
    assert_eq!((found["target"].as_str(), found["calendar"].as_str()), (Some("series2"), Some("primary")));
    assert_eq!(found["event"]["response"], "needsAction");
    invitation(&engine, planning, "inv2");
    let by_uid = |server: &FakeServer| count(server, |request| request.path == "/calendar/v3/calendars/primary/events");
    assert_eq!(by_uid(&server), 1, "the answer is remembered");
    respond("series2", "primary", "tentative", true).unwrap();
    assert_eq!(patches.lock().unwrap()[1].0, "calendars/primary/events/series2");
    assert_eq!(invitation(&engine, planning, "inv2")["event"]["response"], "tentative");
    assert_eq!(by_uid(&server), 1);

    assert_eq!(call(&engine, "calendar.invitation", json!({ "id": design, "message": "nope" })).unwrap_err().code, "unknown_message");
    drop(engine);
    cleanup(&config);
}

// MARK: - Outlook (Graph)

fn graph_event(answer: &str) -> Value {
    let strip = |time: String| time.trim_end_matches('Z').to_string() + ".0000000";
    json!({
        "id": "ev-o", "iCalUId": EXCHANGE_UID, "type": "singleInstance", "subject": "Planning", "isAllDay": false,
        "isOrganizer": false, "start": { "dateTime": strip(at(3, 10.0)), "timeZone": "UTC" },
        "end": { "dateTime": strip(at(3, 11.0)), "timeZone": "UTC" }, "location": { "displayName": "Teams" },
        "organizer": { "emailAddress": { "name": "Cy", "address": "cy@x.com" } },
        "attendees": [{ "type": "required", "emailAddress": { "name": "Me", "address": OUTLOOK }, "status": { "response": answer } }],
        "responseStatus": { "response": answer },
    })
}

fn graph_server(answer: Arc<Mutex<String>>, actions: Arc<Mutex<Vec<(String, Value)>>>) -> FakeServer {
    FakeServer::start(move |request| {
        let path = request.path.as_str();
        if path == "/token" {
            return Reply::json(json!({ "access_token": "at", "expires_in": 3600 }));
        }
        let Some(resource) = path.strip_prefix("/v1.0/") else {
            return Reply::status(404, json!({ "error": { "message": "No such path" } }));
        };
        let folder = |id: &str, name: &str| json!({ "id": id, "displayName": name, "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 1 });
        let expanded = request.param("$expand") == Some("microsoft.graph.eventMessage/event");
        if expanded {
            // Read without immutable ids, so the event's id is the one the calendar lists.
            assert!(!request.header("Prefer").unwrap_or_default().contains("ImmutableId"));
            assert!(request.header("Prefer").unwrap_or_default().contains("outlook.timezone=\"UTC\""));
        }
        let strip = |time: String| time.trim_end_matches('Z').to_string() + ".0000000";
        match (request.method.as_str(), resource) {
            ("GET", "me") => Reply::json(json!({ "displayName": "Me Outlook", "mail": OUTLOOK })),
            ("GET", "me/mailFolders") => Reply::json(json!({ "value": [folder("F-inbox", "Inbox"), folder("F-sent", "Sent Items")] })),
            ("GET", "me/mailFolders/inbox") => Reply::json(json!({ "id": "F-inbox" })),
            ("GET", "me/mailFolders/sentitems") => Reply::json(json!({ "id": "F-sent" })),
            ("GET", "me/mailFolders/F-inbox/messages") => {
                let mut request = graph_message("m1", "c1", ("Cy", "cy@x.com"), "Planning", 0);
                request["@odata.type"] = json!("#microsoft.graph.eventMessageRequest");
                let mut cancelled = graph_message("m2", "c2", ("Cy", "cy@x.com"), "Canceled: Old sync", 10);
                cancelled["@odata.type"] = json!("#microsoft.graph.eventMessage");
                Reply::json(json!({ "value": [cancelled, request] }))
            }
            ("GET", "me/messages/m1") if expanded => Reply::json(json!({
                "@odata.type": "#microsoft.graph.eventMessageRequest", "meetingMessageType": "meetingRequest",
                "subject": "Planning", "event": graph_event("notResponded"),
            })),
            ("GET", "me/messages/m2") if expanded => Reply::json(json!({
                "@odata.type": "#microsoft.graph.eventMessage", "meetingMessageType": "meetingCancelled",
                "subject": "Canceled: Old sync", "isAllDay": false, "type": "singleInstance",
                "startDateTime": { "dateTime": strip(at(4, 9.0)), "timeZone": "UTC" },
                "endDateTime": { "dateTime": strip(at(4, 9.5)), "timeZone": "UTC" },
                "location": { "displayName": "Room 1" }, "from": { "emailAddress": { "name": "Cy", "address": "cy@x.com" } },
                "event": null,
            })),
            ("GET", "me/messages/m1" | "me/messages/m2") => Reply::json(json!({
                "@odata.type": if path.ends_with("m1") { "#microsoft.graph.eventMessageRequest" } else { "#microsoft.graph.eventMessage" },
                "body": { "contentType": "html", "content": "<p>Let's plan.</p>" }, "ccRecipients": [], "replyTo": [], "hasAttachments": false,
            })),
            ("GET", "me/calendars") => Reply::json(json!({ "value": [{ "id": "cal-1", "name": "Calendar", "isDefaultCalendar": true }] })),
            ("GET", "me/calendars/cal-1/calendarView") => Reply::json(json!({ "value": [graph_event(&answer.lock().unwrap())] })),
            ("POST", action) if action.starts_with("me/events/ev-o/") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let name = action.rsplit('/').next().unwrap().to_string();
                *answer.lock().unwrap() = match name.as_str() {
                    "accept" => "accepted",
                    "tentativelyAccept" => "tentativelyAccepted",
                    _ => "declined",
                }
                .into();
                actions.lock().unwrap().push((name, body));
                Reply::status(202, json!({}))
            }
            ("GET", other) if other.starts_with("me/mailFolders/") && other.ends_with("/messages") => Reply::json(json!({ "value": [] })),
            _ => Reply::status(404, json!({ "error": { "message": "Not found" } })),
        }
    })
}

#[test]
fn answers_outlook_meeting_requests_through_graph() {
    let answer = Arc::new(Mutex::new("notResponded".to_string()));
    let actions = Arc::new(Mutex::new(Vec::new()));
    let server = graph_server(answer.clone(), actions.clone());
    let (engine, config) = engine_for(&server, &[(OUTLOOK, "microsoft")]);
    let planning = "me@outlook.com|m1@outlook";
    let cancelled = "me@outlook.com|m2@outlook";
    wait_until("the meeting messages listed", || {
        call(&engine, "mail.list", json!({})).is_ok_and(|list| list["conversations"].as_array().is_some_and(|c| c.len() == 2))
    });

    // Outlook's meeting request names its event; the card reads it from there.
    let card = &call(&engine, "mail.show", json!({ "id": planning })).unwrap()["messages"][0]["invitation"];
    assert_eq!((card["method"].as_str(), card["title"].as_str(), card["event"].as_str()), (Some("request"), Some("Planning"), Some("ev-o")));
    assert_eq!((card["start"].as_str(), card["location"].as_str()), (Some(at(3, 10.0).as_str()), Some("Teams")));
    assert_eq!(card["organizer"]["email"], "cy@x.com");

    wait_until("the calendar fetched", || !invitation(&engine, planning, "m1")["event"].is_null());
    let found = invitation(&engine, planning, "m1");
    assert_eq!((found["target"].as_str(), found["calendar"].as_str()), (Some("ev-o"), Some("cal-1")));
    assert_eq!(found["event"]["response"], "needsAction");

    let respond = |response: &str| {
        call(&engine, "calendar.respond", json!({ "account": OUTLOOK, "event": "ev-o", "calendar": "cal-1", "response": response, "yes": true }))
    };
    respond("tentative").unwrap();
    assert_eq!(actions.lock().unwrap()[0], ("tentativelyAccept".to_string(), json!({ "sendResponse": true })));
    assert_eq!(invitation(&engine, planning, "m1")["event"]["response"], "tentative");
    respond("declined").unwrap();
    assert_eq!(actions.lock().unwrap()[1].0, "decline");
    let fetches = |server: &FakeServer| count(server, |request| request.path.ends_with("/calendarView"));
    let before = fetches(&server);
    wait_until("the calendar fetched again", || fetches(&server) > before);
    wait_until("the fetch settled", || {
        call(&engine, "calendar.events", json!({ "from": local_day(3), "days": 1 })).unwrap()["loading"] == json!(false)
    });
    assert_eq!(invitation(&engine, planning, "m1")["event"]["response"], "declined");

    // A cancellation: its event is gone; the card shows the message's own words and no answer.
    let card = &call(&engine, "mail.show", json!({ "id": cancelled })).unwrap()["messages"][0]["invitation"];
    assert_eq!((card["method"].as_str(), card["title"].as_str(), card["location"].as_str()), (Some("cancel"), Some("Canceled: Old sync"), Some("Room 1")));
    assert_eq!(card["start"], json!(at(4, 9.0)));
    assert!(invitation(&engine, cancelled, "m2")["event"].is_null());
    assert_eq!(count(&server, |request| request.method == "POST" && request.path.contains("sendMail")), 0, "no reply is mailed");
    drop(engine);
    cleanup(&config);
}
