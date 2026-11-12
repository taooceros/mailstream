//! AI tagging and `status` end to end: an engine in a throwaway folder against the fake Gmail
//! server of `mail_tests`, tagging with fake `omp` scripts (never the user's omp).
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::executor::block_on;
use serde_json::{Value, json};

use crate::Engine;
use crate::cache::MailCache;
use crate::mail_tests::{GmailCounters, gmail_server};
use crate::omp::tests::script;
use crate::tagging::{Category, ConversationTag};
use crate::testkit::{call, cleanup, engine_for, wait_until};

/// A fake omp answering every batch: its first conversation is people waiting on the user, the
/// second people whose ask lapsed long ago, the third it skips, the rest are notifications.
/// Each run appends how many conversations it was asked about to `<dir>/counts`.
pub(crate) fn fake_omp(dir: &Path) -> PathBuf {
    let path = dir.join("omp");
    script(
        &path,
        &format!(
            "for last; do :; done\n\
             n=$(printf '%s\\n' \"$last\" | grep -c '\"id\":\"')\n\
             echo $n >> {dir}/counts\n\
             printf 'Here you go:\\n['; i=1; sep=''\n\
             while [ $i -le $n ]; do\n\
               case $i in\n\
                 1) printf '%s{{\"id\":\"%d\",\"category\":\"people\",\"toRespond\":true,\"until\":null,\"labels\":[\"work\"]}}' \"$sep\" $i ;;\n\
                 2) printf '%s{{\"id\":%d,\"category\":\"People\",\"toRespond\":true,\"until\":\"2001-01-01\",\"labels\":[]}}' \"$sep\" $i ;;\n\
                 3) printf '%s{{\"id\":\"%d\",\"category\":\"unsure\"}}' \"$sep\" $i ;;\n\
                 *) printf '%s{{\"id\":\"%d\",\"category\":\"notifications\",\"toRespond\":false,\"labels\":[]}}' \"$sep\" $i ;;\n\
               esac\n\
               sep=','; i=$((i+1))\n\
             done\n\
             printf ']'",
            dir = dir.display()
        ),
    );
    path
}

pub(crate) fn counts(dir: &Path) -> Vec<usize> {
    std::fs::read_to_string(dir.join("counts"))
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect()
}

/// The default list as kept: the 59 conversations of the inbox's two pages.
fn list(engine: &Engine, args: Value) -> Vec<Value> {
    let mut args = args;
    args["limit"] = json!(100);
    args["cached"] = json!(true);
    call(engine, "mail.list", args).unwrap()["conversations"].as_array().unwrap().clone()
}

fn ids(conversations: &[Value]) -> Vec<String> {
    conversations.iter().map(|conversation| conversation["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn tags_the_inbox_with_omp_and_shares_corrections() {
    let counters =
        Arc::new(GmailCounters { tokens: AtomicUsize::new(0), labels: AtomicUsize::new(0), listings: AtomicUsize::new(0) });
    let server = gmail_server(counters);
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    let dir = config.support_directory.parent().unwrap().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    let omp = fake_omp(&dir);
    let events = Arc::new(Mutex::new(Vec::new()));
    let heard = events.clone();
    engine.subscribe(move |event| heard.lock().unwrap().push(event));
    // Off until the whole inbox is listed, so the passes below see all of it.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    for (key, value) in [
        ("aiTagging.ompPath", omp.display().to_string()),
        ("aiTagging.batchSize", "20".into()),
        ("aiTagging.passLimit", "45".into()),
    ] {
        call(&engine, "config.set", json!({ "key": key, "value": value })).unwrap();
    }
    let mail = engine.core.mail.clone();
    // A page longer than the provider's first pages through the inbox (a cached list never asks).
    wait_until("both pages", || {
        let listed = call(&engine, "mail.list", json!({ "limit": 100 }));
        listed.is_ok_and(|listed| listed["conversations"].as_array().unwrap().len() == 59)
    });
    assert_eq!(counts(&dir), [0usize; 0]);

    // Switched on, a pass tags the 45 newest of the 59 conversations in batches of 20; the
    // next look at the list tags the other 14.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    wait_until("the first pass", || counts(&dir).len() == 3);
    mail.tagging().schedule();
    wait_until("the second pass", || counts(&dir).len() == 4);
    assert_eq!(counts(&dir), [20, 20, 5, 14]);
    wait_until("tags", || list(&engine, json!({}))[58]["category"] == "notification");
    assert!(events.lock().unwrap().contains(&"tags"));
    let all = list(&engine, json!({}));
    // The newest thread ends with the user's reply: it never waits on them.
    assert_eq!((&all[0]["category"], &all[0]["waiting"], &all[0]["labels"]), (&json!("people"), &json!(false), &json!(["Work"])));
    assert_eq!((&all[1]["category"], &all[1]["waiting"]), (&json!("people"), &json!(false)));
    // The model skipped it: no tag.
    assert_eq!((&all[2]["category"], &all[2]["waiting"], &all[2]["labels"]), (&Value::Null, &json!(false), &json!([])));
    let waiting = list(&engine, json!({ "waiting": true }));
    assert_eq!(ids(&waiting), [&all[20]["id"], &all[40]["id"], &all[45]["id"]].map(|id| id.as_str().unwrap().to_string()));
    assert_eq!(list(&engine, json!({ "category": "people" })).len(), 8);
    assert_eq!(list(&engine, json!({ "category": "spam" })).len(), 59);
    let id = |n: usize| all[n]["id"].as_str().unwrap().to_string();

    // Corrections, as the Swift engine answers them.
    let tag = |args: Value| call(&engine, "mail.tag", args);
    assert_eq!(
        tag(json!({ "ids": [id(5)], "waiting": "true" })).unwrap(),
        json!({ "done": [{ "id": id(5), "category": "notification", "waiting": true, "labels": [] }], "failed": [], "undo": 1 })
    );
    // Undo puts the tag back as it was.
    let undone = call(&engine, "mail.undo", json!({})).unwrap();
    assert_eq!(undone, json!({ "step": 1, "action": "tag", "done": [id(5)], "failed": [] }));
    assert_eq!(list(&engine, json!({}))[5]["waiting"], json!(false));
    assert_eq!(call(&engine, "mail.undo", json!({})).unwrap_err().code, "nothing");
    tag(json!({ "ids": [id(5)], "waiting": true })).unwrap();
    // The FFI sends a boolean; the command line sends `true|false`.
    assert_eq!(tag(json!({ "ids": [id(5)], "waiting": false })).unwrap()["done"][0]["waiting"], json!(false));
    assert_eq!(tag(json!({ "ids": [id(5)], "waiting": true })).unwrap()["done"][0]["waiting"], json!(true));
    assert_eq!(tag(json!({ "ids": id(5), "category": "newsletter" })).unwrap()["done"][0]["waiting"], json!(false));
    let answer = tag(json!({ "ids": [id(5), "nope", id(2)], "waiting": true })).unwrap();
    assert_eq!(answer["done"], json!([]));
    assert_eq!(
        answer["failed"],
        json!([
            { "id": id(5), "message": "Newsletters cannot wait on you." },
            { "id": "nope", "message": "Unknown conversation nope. Ids come from `mail list`, `mail search` or `mail show`, and are remembered while the app runs." },
            { "id": id(2), "message": format!("{} has no AI tag yet; give a category.", id(2)) },
        ])
    );
    let answer = tag(json!({ "ids": [id(2)], "category": "invitation", "labels": ["  Conference trip planning ", "", "Travel", "x"] }));
    assert_eq!(
        answer.unwrap()["done"][0],
        json!({ "id": id(2), "category": "invitation", "waiting": false, "labels": ["Conference trip ", "Travel"] })
    );
    let error = tag(json!({ "ids": [id(5)] })).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("usage", "Give --category, --waiting or --labels."));
    assert_eq!(tag(json!({ "ids": [id(5)], "category": "spam" })).unwrap_err().message, "Unknown category spam.");
    assert_eq!(tag(json!({ "category": "people" })).unwrap_err().message, "mail tag: <ids> is required.");

    // What one process writes, the others read: the corrections are in mail.db, and a tag
    // another process writes shows here once its signal arrives.
    let key = |n: usize| crate::tagging::key(&mail.lookup(&id(n)).unwrap());
    let (database, files) = (config.support_directory.join("mail.db"), config.cache_directory.clone());
    let saved = block_on(engine.runtime.spawn({
        let (mail, database, files) = (mail.clone(), database.clone(), files.clone());
        async move {
            mail.flush().await;
            MailCache::new(database, files).load_tags().await
        }
    }))
    .unwrap();
    assert_eq!(saved[&key(2)].category, Category::Invitation);
    assert_eq!(saved[&key(5)].category, Category::Newsletter);
    // Every conversation but the three the model skipped and nobody corrected.
    assert_eq!(saved.len(), 56);
    let lapsed = Some(crate::model::now() - 60.0);
    let written = HashMap::from([
        (key(46), ConversationTag { category: Category::People, to_respond: true, labels: vec![], waiting_until: None, synced: false }),
        (key(47), ConversationTag { category: Category::People, to_respond: true, labels: vec![], waiting_until: lapsed, synced: false }),
    ]);
    block_on(engine.runtime.spawn(async move {
        let other = MailCache::new(database, files);
        other.save_tags(written);
        other.flush().await;
    }))
    .unwrap();
    wait_until("the other process's tag", || list(&engine, json!({}))[46]["waiting"] == json!(true));
    // An ask past its day no longer waits.
    let listed = list(&engine, json!({}));
    assert_eq!((&listed[47]["category"], &listed[47]["waiting"]), (&json!("people"), &json!(false)));
    // Conversations tagged, skipped or corrected are not sent to omp again.
    mail.tagging().schedule();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(counts(&dir).len(), 4);

    // Retagging forgets every tag and tags the list again.
    assert_eq!(call(&engine, "mail.retag", json!({})).unwrap(), json!({ "started": true }));
    wait_until("retagging", || counts(&dir).len() >= 7);
    assert_eq!(counts(&dir)[..7], [20, 20, 5, 14, 20, 20, 5]);
    wait_until("new tags", || list(&engine, json!({}))[44]["category"] == "notification");
    assert_eq!(list(&engine, json!({}))[2]["category"], Value::Null);

    // Off: no tags show, and corrections and retagging are refused.
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    assert_eq!(list(&engine, json!({}))[0]["category"], Value::Null);
    let error = call(&engine, "mail.retag", json!({})).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("disabled", "AI tagging is off (aiTagging.enabled)."));
    assert_eq!(tag(json!({ "ids": [id(0)], "category": "people" })).unwrap()["failed"][0]["message"], "AI tagging is off (aiTagging.enabled).");
    assert_eq!(
        call(&engine, "status", json!({})).unwrap()["aiTagging"],
        json!({ "enabled": false, "status": null, "failed": false })
    );

    // A failing omp: the pass stops with its last error line, announced as a problem, and waits
    // out the backoff; a changed omp setting tries again at once.
    let failing = dir.join("failing");
    script(&failing, &format!("echo run >> {}/failures; echo 'Error: quota exceeded' >&2; exit 1", dir.display()));
    call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": failing.display().to_string() })).unwrap();
    events.lock().unwrap().clear();
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    let failures = || std::fs::read_to_string(dir.join("failures")).unwrap_or_default().lines().count();
    wait_until("the failure", || {
        call(&engine, "status", json!({})).unwrap()["aiTagging"]
            == json!({ "enabled": true, "status": "AI tagging failed: Error: quota exceeded", "failed": true })
    });
    assert_eq!(failures(), 1);
    wait_until("the problem", || events.lock().unwrap().contains(&"problems"));
    mail.tagging().schedule();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(failures(), 1);
    call(&engine, "config.set", json!({ "key": "aiTagging.backoffSeconds", "value": "30" })).unwrap();
    wait_until("the retry", || failures() == 2);

    // Apple Intelligence (aiTagging.backend apple): unavailable, the run fails on the helper's
    // message and omp does not stand in; once it answers, it tags and the problem clears.
    let helper = &config.apple_tagger;
    let off = "Apple Intelligence is off; turn it on in System Settings > Apple Intelligence & Siri";
    script(helper, &format!("echo run >> {}/apple; echo '{off}' >&2; exit 3", dir.display()));
    call(&engine, "config.set", json!({ "key": "aiTagging.backend", "value": "apple" })).unwrap();
    wait_until("Apple Intelligence's failure", || {
        call(&engine, "status", json!({})).unwrap()["aiTagging"]
            == json!({ "enabled": true, "status": format!("AI tagging failed: {off}"), "failed": true })
    });
    assert_eq!(std::fs::read_to_string(dir.join("apple")).unwrap().lines().count(), 1);
    assert_eq!(failures(), 2);
    std::fs::remove_file(helper).unwrap();
    std::fs::copy(&omp, helper).unwrap();
    events.lock().unwrap().clear();
    call(&engine, "mail.retag", json!({})).unwrap();
    wait_until("tagging with Apple Intelligence", || counts(&dir).len() >= 10);
    assert_eq!(counts(&dir)[7..10], [20, 20, 5]);
    wait_until("the problem clears", || {
        call(&engine, "status", json!({})).unwrap()["aiTagging"] == json!({ "enabled": true, "status": null, "failed": false })
    });
    wait_until("the problem gone", || events.lock().unwrap().contains(&"problems"));
    assert_eq!(failures(), 2);

    drop(engine);
    cleanup(&config);
}

/// The real model through the engine, on the fake server's synthetic inbox: one pass of 20
/// conversations in one batch, timed. Not run by default. Apple Intelligence:
/// `MAILSTREAM_APPLE_TAGGER=<built mailstream-apple-tagger> cargo test on_device -- --ignored --nocapture`;
/// omp instead: `MAILSTREAM_OMP=<omp path>`.
#[test]
#[ignore = "runs a real model: MAILSTREAM_APPLE_TAGGER or MAILSTREAM_OMP"]
fn tags_synthetic_mail_on_device() {
    let counters =
        Arc::new(GmailCounters { tokens: AtomicUsize::new(0), labels: AtomicUsize::new(0), listings: AtomicUsize::new(0) });
    let server = gmail_server(counters);
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "false" })).unwrap();
    let backend = match (std::env::var("MAILSTREAM_APPLE_TAGGER"), std::env::var("MAILSTREAM_OMP")) {
        (Ok(helper), _) => {
            std::fs::copy(helper, &config.apple_tagger).unwrap();
            "apple"
        }
        (_, Ok(omp)) => {
            call(&engine, "config.set", json!({ "key": "aiTagging.ompPath", "value": omp })).unwrap();
            "omp"
        }
        _ => panic!("set MAILSTREAM_APPLE_TAGGER or MAILSTREAM_OMP"),
    };
    for (key, value) in [("aiTagging.backend", backend), ("aiTagging.batchSize", "20"), ("aiTagging.passLimit", "20")] {
        call(&engine, "config.set", json!({ "key": key, "value": value })).unwrap();
    }
    // A page longer than the provider's first pages through the inbox (a cached list never asks).
    wait_until("both pages", || {
        let listed = call(&engine, "mail.list", json!({ "limit": 100 }));
        listed.is_ok_and(|listed| listed["conversations"].as_array().unwrap().len() == 59)
    });
    let started = std::time::Instant::now();
    call(&engine, "config.set", json!({ "key": "aiTagging.enabled", "value": "true" })).unwrap();
    let tagged = || list(&engine, json!({})).iter().filter(|conversation| !conversation["category"].is_null()).count();
    let status = || call(&engine, "status", json!({})).unwrap()["aiTagging"].clone();
    while started.elapsed() < Duration::from_secs(150) && (tagged() == 0 || !status()["status"].is_null()) {
        assert_ne!(status()["failed"], json!(true), "{}", status());
        std::thread::sleep(Duration::from_millis(100));
    }
    let elapsed = started.elapsed().as_secs_f64();
    let count = tagged();
    println!("{backend}: {count} of 20 conversations tagged in {elapsed:.1} s, {:.2} s each", elapsed / 20.0);
    for conversation in list(&engine, json!({})).iter().take(20) {
        println!("  {} → {} {} {}", conversation["subject"], conversation["category"], conversation["waiting"], conversation["labels"]);
    }
    assert!(count > 0);
    drop(engine);
    cleanup(&config);
}

#[test]
fn reports_status() {
    let counters =
        Arc::new(GmailCounters { tokens: AtomicUsize::new(0), labels: AtomicUsize::new(0), listings: AtomicUsize::new(0) });
    let server = gmail_server(counters);
    let (engine, config) = engine_for(&server, &[("me@gmail.com", "google")]);
    let settings = config.support_directory.join("settings.json");
    std::fs::write(&settings, "{\n  \"aiTagging.enabled\" : false,\n  \"nope\" : 1\n}\n").unwrap();
    wait_until("settings", || call(&engine, "status", json!({})).unwrap()["settingsIssues"] != json!([]));
    wait_until("mailboxes", || call(&engine, "status", json!({})).unwrap()["accounts"][0]["mailboxesLoaded"] == json!(true));
    let status = call(&engine, "status", json!({})).unwrap();
    assert_eq!(status["version"], "dev");
    assert_eq!(status["pid"], json!(std::process::id()));
    assert_eq!(status["settingsFile"], settings.display().to_string());
    assert_eq!(status["settingsIssues"], json!(["Unknown setting \"nope\" is ignored."]));
    assert_eq!(
        status["accounts"],
        json!([{ "id": "me@gmail.com", "label": "me@gmail.com", "provider": "google", "mailboxesLoaded": true, "problem": null }])
    );
    let sync = status["sync"].as_object().unwrap();
    let mut keys: Vec<&str> = sync.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["lastError", "lastRefresh", "loading", "pushConnected"]);
    assert_eq!(sync["pushConnected"], json!(false));
    assert_eq!(status["aiTagging"], json!({ "enabled": false, "status": null, "failed": false }));
    drop(engine);
    cleanup(&config);
}
