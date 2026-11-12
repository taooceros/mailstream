//! Outgoing mail held against the Swift engine's (`scripts/outgoing-mail-interop.sh`): for the
//! inputs of `Tests/Fixtures/outgoing-mail-interop.json`, what this engine renders, plans and
//! ranks must equal what the Swift engine wrote to the file `MAILSTREAM_OUTGOING_INTEROP` names
//! (`OutgoingMailInteropTests.swift`). Messages compare with their Date, Message-ID and boundaries
//! made neutral. Without the variable the test does nothing.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use base64::Engine as _;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::contacts::{ListedPerson, rank, suggestions};
use super::sending::{ReplyPlan, attribution, quote, reply_html, reply_recipients, reply_subject};
use crate::mime::{OutgoingMessage, render};
use crate::model::{Address, Envelope, MailMessage, MailboxKey, MailboxRole, MessageContent};

#[derive(Deserialize)]
struct File {
    name: String,
    base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    from: Address,
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    subject: String,
    text: String,
    html: Option<String>,
    attachments: Option<Vec<File>>,
    in_reply_to: Option<String>,
    references: Option<Vec<String>>,
    thread: Option<String>,
}

#[derive(Deserialize)]
struct Reference {
    account: String,
    mailbox: String,
}

#[derive(Deserialize)]
struct Source {
    #[serde(rename = "ref")]
    reference: Reference,
    role: MailboxRole,
    envelope: Envelope,
}

#[derive(Deserialize)]
struct Reply {
    source: Source,
    content: Option<MessageContent>,
    own: Vec<String>,
    all: bool,
    reply: String,
}

#[derive(Deserialize)]
struct Listing {
    account: String,
    envelopes: Vec<Envelope>,
}

#[derive(Deserialize)]
struct Person {
    name: Option<String>,
    emails: Vec<String>,
}

#[derive(Deserialize)]
struct Contacts {
    own: Vec<String>,
    listings: Vec<Listing>,
    people: Vec<Person>,
    queries: Vec<String>,
}

#[derive(Deserialize)]
struct Fixture {
    messages: Vec<Message>,
    replies: Vec<Reply>,
    contacts: Contacts,
}

fn text(address: &Address) -> String {
    format!("{}|{}", address.name.as_deref().unwrap_or(""), address.email)
}

/// `message` with what differs from one rendering to the next made neutral: the Date and the
/// Message-ID, and boundaries numbered in the order they first appear.
fn neutral(message: &str) -> String {
    let date = Regex::new(r"(?m)^Date: [^\r\n]*").unwrap();
    let id = Regex::new(r"Message-ID: <[0-9a-f-]{36}@mailstream\.local>").unwrap();
    let boundary = Regex::new(r"=_ms_[0-9a-f]{32}").unwrap();
    let message = date.replace(message, "Date: D");
    let message = id.replace(&message, "Message-ID: <ID@mailstream.local>");
    let mut numbers: HashMap<String, usize> = HashMap::new();
    boundary
        .replace_all(&message, |found: &regex::Captures| {
            let next = numbers.len() + 1;
            format!("=_ms_B{}", numbers.entry(found[0].to_string()).or_insert(next))
        })
        .into_owned()
}

fn answers(fixture: Fixture, folder: &std::path::Path) -> Value {
    let mime: Vec<String> = fixture
        .messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let directory = folder.join(index.to_string());
            std::fs::create_dir_all(&directory).unwrap();
            let attachments = message
                .attachments
                .unwrap_or_default()
                .into_iter()
                .map(|file| {
                    let path = directory.join(&file.name);
                    std::fs::write(&path, base64::engine::general_purpose::STANDARD.decode(&file.base64).unwrap()).unwrap();
                    path
                })
                .collect();
            let outgoing = OutgoingMessage {
                message_id: crate::mime::message_id(),
                from: message.from,
                to: message.to,
                cc: message.cc,
                bcc: message.bcc,
                subject: message.subject,
                text: message.text,
                html: message.html,
                attachments,
                in_reply_to: message.in_reply_to,
                references: message.references.unwrap_or_default(),
                thread: message.thread,
            };
            neutral(&String::from_utf8(render(&outgoing).unwrap()).unwrap())
        })
        .collect();

    let replies: Vec<Value> = fixture
        .replies
        .into_iter()
        .map(|reply| {
            let key = Arc::new(MailboxKey::new(&reply.source.reference.account, &reply.source.reference.mailbox));
            let source = MailMessage { key, envelope: Arc::new(reply.source.envelope), role: reply.source.role };
            let own: HashSet<String> = reply.own.into_iter().collect();
            let (to, cc) = reply_recipients(&source, reply.content.as_ref(), &own, reply.all);
            let plan = ReplyPlan {
                to,
                cc,
                subject: reply_subject(&source.envelope.subject),
                quoted_text: quote(reply.content.as_ref().and_then(|content| content.text.as_deref()).unwrap_or("")),
                attribution: attribution(&source),
            };
            json!({
                "to": plan.to.iter().map(text).collect::<Vec<_>>(), "cc": plan.cc.iter().map(text).collect::<Vec<_>>(),
                "subject": plan.subject, "quotedText": plan.quoted_text, "attribution": plan.attribution,
                "plainBody": plan.plain_body(&reply.reply),
                "html": reply_html(&reply.reply, &plan.attribution, reply.content.as_ref()),
            })
        })
        .collect();

    let contacts = fixture.contacts;
    let listings: Vec<(String, Vec<Arc<Envelope>>)> = contacts
        .listings
        .into_iter()
        .map(|listing| (listing.account, listing.envelopes.into_iter().map(Arc::new).collect()))
        .collect();
    let people: Vec<ListedPerson> = contacts.people.into_iter().map(|person| (person.name, person.emails)).collect();
    let ranked = rank(&listings, &people, &contacts.own.into_iter().collect());
    let completions: BTreeMap<String, Vec<String>> = contacts
        .queries
        .into_iter()
        .map(|query| {
            let found = suggestions(&ranked, &query).into_iter().map(|contact| contact.address.email.clone()).collect();
            (query, found)
        })
        .collect();
    json!({
        "mime": mime,
        "replies": replies,
        "contacts": { "ranked": ranked.iter().map(|contact| text(&contact.address)).collect::<Vec<_>>(), "suggestions": completions },
    })
}

#[test]
fn renders_plans_and_ranks_what_the_swift_engine_does() {
    let Ok(swift_file) = std::env::var("MAILSTREAM_OUTGOING_INTEROP") else { return };
    let fixture_file = concat!(env!("CARGO_MANIFEST_DIR"), "/../Tests/Fixtures/outgoing-mail-interop.json");
    let fixture: Fixture = serde_json::from_slice(&std::fs::read(fixture_file).unwrap()).unwrap();
    let folder = std::env::temp_dir().join(format!("mailstream-outgoing-{}", std::process::id()));
    let mine = answers(fixture, &folder);
    let _ = std::fs::remove_dir_all(&folder);
    let mut swift: Value = serde_json::from_slice(&std::fs::read(swift_file).unwrap()).unwrap();
    for message in swift["mime"].as_array_mut().unwrap() {
        *message = json!(neutral(message.as_str().unwrap()));
    }
    let mut differences = Vec::new();
    for (index, (a, b)) in mine["mime"].as_array().unwrap().iter().zip(swift["mime"].as_array().unwrap()).enumerate() {
        if a != b {
            differences.push(format!("mime {index}:\n--- rust\n{}\n--- swift\n{}", a.as_str().unwrap(), b.as_str().unwrap()));
        }
    }
    for (index, (a, b)) in mine["replies"].as_array().unwrap().iter().zip(swift["replies"].as_array().unwrap()).enumerate() {
        for (field, value) in a.as_object().unwrap() {
            if Some(value) != b.get(field) {
                differences.push(format!("reply {index} {field}:\n--- rust\n{value}\n--- swift\n{}", b[field]));
            }
        }
    }
    if mine["contacts"] != swift["contacts"] {
        differences.push(format!("contacts:\n--- rust\n{:#}\n--- swift\n{:#}", mine["contacts"], swift["contacts"]));
    }
    assert_eq!(mine["mime"].as_array().unwrap().len(), swift["mime"].as_array().unwrap().len());
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
