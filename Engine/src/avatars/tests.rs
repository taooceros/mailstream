//! Sender pictures against a web answered from memory and fake providers: nothing reaches the
//! network.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use futures::executor::block_on;
use serde_json::{Value, json};

use super::picture::tests::png;
use super::web::{Page, Web};
use super::*;
use crate::Engine;
use crate::testkit::{FakeServer, Reply, cleanup, server_config, sign_in_all, unique};

/// The public web as a table: URL → body, DNS name → TXT records. Everything else fails.
#[derive(Default)]
pub(crate) struct FakeWeb {
    pages: Mutex<HashMap<String, Vec<u8>>>,
    records: Mutex<HashMap<String, Vec<String>>>,
    /// Every URL and DNS name asked for, in order.
    asked: Mutex<Vec<String>>,
}

impl FakeWeb {
    fn page(&self, url: &str, body: Vec<u8>) {
        self.pages.lock().unwrap().insert(url.into(), body);
    }

    fn txt(&self, name: &str, record: &str) {
        self.records.lock().unwrap().insert(name.into(), vec![record.into()]);
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Web for FakeWeb {
    fn get(&self, url: String, cap: usize) -> BoxFuture<'static, Option<Page>> {
        self.asked.lock().unwrap().push(url.clone());
        let page = self.pages.lock().unwrap().get(&url).map(|body| Page { body: body[..body.len().min(cap + 1)].to_vec(), url, max_age: None });
        Box::pin(async move { page })
    }

    fn txt(&self, name: String) -> BoxFuture<'static, Vec<String>> {
        self.asked.lock().unwrap().push(format!("dns:{name}"));
        let records = self.records.lock().unwrap().get(&name).cloned().unwrap_or_default();
        Box::pin(async move { records })
    }
}

fn temporary_root() -> PathBuf {
    std::env::temp_dir().join(format!("mailstream-avatars-{}-{}", std::process::id(), unique())).join("avatars")
}

fn avatars(web: &Arc<FakeWeb>) -> Arc<Avatars> {
    let roots = Roots { people: "http://127.0.0.1:9/people/v1/".into(), graph: "http://127.0.0.1:9/v1.0/".into() };
    Arc::new(Avatars::new(temporary_root(), web.clone(), roots, Vec::new))
}

async fn files(avatars: &Arc<Avatars>, args: Value) -> Result<Value, EngineError> {
    let Value::Object(values) = args else { unreachable!() };
    let arguments = Arguments::new(crate::commands::spec("mail.avatars")?, &values)?;
    avatars.command(&arguments).await
}

fn gravatar(email: &str) -> String {
    format!("https://gravatar.com/avatar/{}?s=96&d=404", hash(email))
}

#[test]
fn keys_and_names_match_the_swift_engine() {
    // Swift: SHA256 of the trimmed, lowercased text, hex.
    assert_eq!(hash(" Ann@Example.COM\t"), hex_sha256(b"ann@example.com"));
    assert_eq!(cache_key("ann@example.com", None), format!("p-{}", hex_sha256(b"ann@example.com")));
    assert_eq!(cache_key("ann@x.com", Some("https://A/b")), format!("p-{}", hex_sha256(b"ann@x.com|https://a/b")));
    assert_eq!(organization("mail.shop.example.co.uk"), "example.co.uk");
    assert_eq!(organization("news.github.com"), "github.com");
    assert_eq!(organization("example.com"), "example.com");
    assert!(looks_automated("no-reply@x.com") && looks_automated("Newsletter.weekly@x.com"));
    assert!(!looks_automated("ann@x.com") && !looks_automated("infodesk@x.com"));
    assert_eq!(
        sized("https://lh3.googleusercontent.com/a/ACg8oc=k-no"),
        "https://lh3.googleusercontent.com/a/ACg8oc=s96-c"
    );
    assert_eq!(sized("https://lh3.googleusercontent.com/a/ACg8oc"), "https://lh3.googleusercontent.com/a/ACg8oc=s96-c");
    assert_eq!(sized("https://example.com/a=b"), "https://example.com/a=b");
    #[cfg(not(windows))]
    assert_eq!(file_url(Path::new("/Users/a b/Caches/p-1.png")), "file:///Users/a%20b/Caches/p-1.png");
    #[cfg(windows)]
    assert_eq!(file_url(Path::new(r"C:\Users\a b\Caches\p-1.png")), "file:///C:/Users/a%20b/Caches/p-1.png");
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_gravatars_caches_them_and_remembers_misses() {
    let web = Arc::new(FakeWeb::default());
    web.page(&gravatar("ann@gmail.com"), png(120, 120, [200, 10, 10, 255]));
    let avatars = avatars(&web);
    assert_eq!(files(&avatars, json!({ "addresses": [] })).await.unwrap_err().code, "usage");
    let answer = files(&avatars, json!({ "addresses": ["Ann@Gmail.com", "bob@gmail.com", "not an address"] })).await.unwrap();
    let ann = avatars.root.join(format!("p-{}.png", hash("ann@gmail.com")));
    assert_eq!(answer, json!({ "ann@gmail.com": file_url(&ann), "bob@gmail.com": null }));
    let picture = image::open(&ann).unwrap().to_rgba8();
    assert_eq!((picture.dimensions(), picture.get_pixel(48, 48).0), ((96, 96), [200, 10, 10, 255]));
    // Bob's miss is kept with the (empty) directory's stamp; freemail domains get no BIMI lookup.
    let miss = std::fs::read_to_string(avatars.root.join(format!("p-{}.none", hash("bob@gmail.com")))).unwrap();
    assert_eq!(miss, Directory::new(Vec::new(), BTreeMap::new(), Vec::new()).stamp);
    assert_eq!(web.asked(), [gravatar("ann@gmail.com"), gravatar("bob@gmail.com")]);
    // Both answer from disk now, without asking anyone.
    let again = files(&avatars, json!({ "addresses": "ann@gmail.com, bob@gmail.com" })).await.unwrap();
    assert_eq!(again, answer);
    assert_eq!(web.asked().len(), 2);
    // A fresh engine reads the same cache.
    let roots = avatars.roots.clone();
    let relaunched = Arc::new(Avatars::new(avatars.root.clone(), web.clone(), roots, Vec::new));
    assert_eq!(files(&relaunched, json!({ "addresses": ["ann@gmail.com", "bob@gmail.com"] })).await.unwrap(), answer);
    assert_eq!(web.asked().len(), 2);
    let _ = std::fs::remove_dir_all(avatars.root.parent().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_senders_share_the_domains_bimi_logo_or_site_icon() {
    let web = Arc::new(FakeWeb::default());
    // BIMI only at the organizational domain.
    web.txt("default._bimi.example.co.uk", "v=BIMI1; l=https://cdn.example.co.uk/logo.svg; a=");
    let logo = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#00ff00"/></svg>"##;
    web.page("https://cdn.example.co.uk/logo.svg", logo.to_vec());
    // No BIMI, a home page declaring icons; the touch icon is too small, the next one does.
    web.page(
        "https://shop.test/",
        br#"<link rel="apple-touch-icon" href="/tiny.png"><link rel="icon" sizes="64x64" href="https://static.shop.test/i.png">"#.to_vec(),
    );
    web.page("https://shop.test/tiny.png", png(16, 16, [0, 0, 0, 255]));
    web.page("https://static.shop.test/i.png", png(64, 64, [0, 0, 255, 255]));
    let avatars = avatars(&web);
    let answer = files(
        &avatars,
        json!({ "addresses": ["noreply@mail.example.co.uk", "ann@shop.test", "news@shop.test"], "automated": ["ANN@shop.test"] }),
    )
    .await
    .unwrap();
    for address in ["noreply@mail.example.co.uk", "ann@shop.test", "news@shop.test"] {
        assert!(answer[address].is_string(), "{address}: {answer}");
    }
    let logo = image::open(avatars.root.join(format!("p-{}.png", hash("noreply@mail.example.co.uk")))).unwrap().to_rgba8();
    assert_eq!(logo.get_pixel(1, 1).0, [0, 255, 0, 255]);
    let icon = image::open(avatars.root.join(format!("p-{}.png", hash("news@shop.test")))).unwrap().to_rgba8();
    assert_eq!((icon.get_pixel(2, 2).0, icon.get_pixel(48, 48).0), ([255, 255, 255, 255], [0, 0, 255, 255]));
    // Domain-wide results are cached on their own: BIMI's under the sender's domain, the
    // site icon's under the domain too, a domain without BIMI as a miss.
    assert!(avatars.root.join(format!("b-{}.png", hash("mail.example.co.uk"))).is_file());
    assert!(avatars.root.join(format!("i-{}.png", hash("shop.test"))).is_file());
    assert!(avatars.root.join(format!("b-{}.none", hash("shop.test"))).is_file());
    // The two shop.test senders asked the home page once between them.
    assert_eq!(web.asked().iter().filter(|url| *url == "https://shop.test/").count(), 1);
    let asked = web.asked().len();
    // Another sender at a known domain answers from the domain's cache.
    let more = files(&avatars, json!({ "addresses": ["alerts@shop.test"] })).await.unwrap();
    assert!(more["alerts@shop.test"].is_string());
    assert!(!web.asked()[asked..].iter().any(|url| url.contains("shop.test/") || url.starts_with("dns:")));
    let _ = std::fs::remove_dir_all(avatars.root.parent().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn people_who_write_get_bimi_after_gravatar_and_published_pictures_first() {
    let web = Arc::new(FakeWeb::default());
    web.txt("default._bimi.bank.test", "v=BIMI1; l=https://bank.test/bimi.svg");
    let logo = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20" fill="#123456"/></svg>"##;
    web.page("https://bank.test/bimi.svg", logo.to_vec());
    web.page("https://lh3.googleusercontent.com/chat/abc=s96-c", png(96, 96, [9, 9, 9, 255]));
    let avatars = avatars(&web);
    let answer = files(&avatars, json!({ "addresses": ["ann@bank.test"] })).await.unwrap();
    assert!(answer["ann@bank.test"].is_string());
    assert_eq!(web.asked(), [gravatar("ann@bank.test"), "dns:default._bimi.bank.test".into(), "https://bank.test/bimi.svg".into()]);
    // A picture Chat publishes outranks the logo, under its own cache entry.
    avatars.publish("https://lh3.googleusercontent.com/chat/abc=k-no", "Ann@Bank.test");
    let published = files(&avatars, json!({ "addresses": ["ann@bank.test"] })).await.unwrap();
    let file = avatars.root.join(format!("{}.png", cache_key("ann@bank.test", Some("https://lh3.googleusercontent.com/chat/abc=k-no"))));
    assert_eq!(published["ann@bank.test"], json!(file_url(&file)));
    assert_eq!(image::open(&file).unwrap().to_rgba8().get_pixel(48, 48).0, [9, 9, 9, 255]);
    let _ = std::fs::remove_dir_all(avatars.root.parent().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn old_pictures_answer_while_they_are_looked_up_again() {
    let web = Arc::new(FakeWeb::default());
    let avatars = avatars(&web);
    let key = cache_key("ann@gmail.com", None);
    let file = avatars.root.join(format!("{key}.png"));
    std::fs::write(&file, png(96, 96, [1, 1, 1, 255])).unwrap();
    let old = SystemTime::now() - Duration::from_secs(15 * 86_400);
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(old).unwrap();
    // Gravatar now has a new one; the old one answers at once, then gives way.
    web.page(&gravatar("ann@gmail.com"), png(96, 96, [2, 2, 2, 255]));
    let answer = files(&avatars, json!({ "addresses": ["ann@gmail.com"] })).await.unwrap();
    assert_eq!(answer["ann@gmail.com"], json!(file_url(&file)));
    let start = Instant::now();
    while image::open(&file).ok().map(|image| image.to_rgba8().get_pixel(0, 0).0) != Some([2, 2, 2, 255]) {
        assert!(start.elapsed() < Duration::from_secs(10), "the old picture was not replaced");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A failed lookup leaves an old picture in place rather than a miss.
    web.pages.lock().unwrap().clear();
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(old).unwrap();
    files(&avatars, json!({ "addresses": ["ann@gmail.com"] })).await.unwrap();
    let start = Instant::now();
    while web.asked().len() < 2 || !lock(&avatars.revalidating).is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10), "no second lookup");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(file.is_file() && !avatars.root.join(format!("{key}.none")).exists());
    let _ = std::fs::remove_dir_all(avatars.root.parent().unwrap());
}

/// Google and Outlook accounts granted contacts, against a fake People API and Graph.
#[test]
fn contact_photos_come_first_and_the_directory_lists_everyone() {
    let server = FakeServer::start(|request| match request.path.as_str() {
        "/token" => Reply::json(json!({ "access_token": "at", "expires_in": 3600 })),
        "/people/v1/people/me/connections" if request.param("pageToken").is_none() => Reply::json(json!({
            "connections": [
                { "names": [{ "displayName": "" }, { "displayName": "Ann Lee" }],
                  "emailAddresses": [{ "value": "Ann@Example.com" }],
                  "photos": [{ "url": "http://127.0.0.1:1/default.png", "default": true }, { "url": "https://lh3.googleusercontent.com/a/ann=s100" }] },
            ],
            "nextPageToken": "p2",
        })),
        "/people/v1/people/me/connections" => Reply::json(json!({
            "connections": [{ "names": [{ "displayName": "No Mail" }] }, { "emailAddresses": [{ "value": "cy@example.com" }] }],
        })),
        "/people/v1/otherContacts" => Reply::status(403, json!({ "error": { "message": "no" } })),
        "/v1.0/me/contacts" => Reply::json(json!({
            "value": [{ "id": "c1", "displayName": "", "emailAddresses": [{ "name": "Bo", "address": "BO@corp.test" }] }],
        })),
        "/v1.0/me/contacts/c1/photo/$value" => Reply::bytes(200, png(64, 64, [8, 8, 8, 255])),
        _ => Reply::status(404, json!({ "error": { "message": "not found" } })),
    });
    let mut config = server_config(&server);
    sign_in_all(
        &config,
        &[
            json!({ "id": "me@gmail.com", "provider": "google", "label": "me@gmail.com", "scopes": ["openid", crate::oauth::GOOGLE_CONTACTS] }),
            json!({ "id": "me@outlook.com", "provider": "microsoft", "label": "me@outlook.com", "scopes": ["openid", crate::oauth::GRAPH_CONTACTS] }),
            json!({ "id": "other@gmail.com", "provider": "google", "label": "other@gmail.com", "scopes": ["openid"] }),
        ],
    );
    // Google photos are fetched anonymously, at the drawn size.
    let web = Arc::new(FakeWeb::default());
    web.page("https://lh3.googleusercontent.com/a/ann=s96-c", png(50, 50, [5, 6, 7, 255]));
    config.avatar_web = web.clone();
    let engine = Engine::start(config.clone());
    let answer =
        block_on(engine.call("mail.avatars", json!({ "addresses": ["ann@example.com", "bo@corp.test", "cy@example.com"] }))).unwrap();
    let file = |address: &str| config.cache_directory.join("avatars").join(format!("p-{}.png", hash(address)));
    assert_eq!(answer["ann@example.com"], json!(file_url(&file("ann@example.com"))));
    assert_eq!(answer["bo@corp.test"], json!(file_url(&file("bo@corp.test"))));
    assert_eq!(image::open(file("ann@example.com")).unwrap().to_rgba8().get_pixel(48, 48).0, [5, 6, 7, 255]);
    assert_eq!(image::open(file("bo@corp.test")).unwrap().to_rgba8().get_pixel(48, 48).0, [8, 8, 8, 255]);
    // Cy is listed without a photo: Gravatar is next, then the domain's BIMI.
    assert_eq!(answer["cy@example.com"], Value::Null);
    assert!(web.asked().contains(&gravatar("cy@example.com")));
    // The directory is saved as Swift saves it, naming the accounts it asked.
    let saved = Directory::read(&config.cache_directory.join("avatars/directory.json")).unwrap();
    assert_eq!(saved.accounts, ["me@gmail.com", "me@outlook.com"]);
    assert_eq!(saved.photos["bo@corp.test"], PhotoSource::GraphContact { account: "me@outlook.com".into(), id: "c1".into() });
    assert_eq!(saved.photos["ann@example.com"], PhotoSource::Url { url: "https://lh3.googleusercontent.com/a/ann=s100".into() });
    let avatars = engine.core.avatars.clone();
    let people = engine.runtime.block_on(async move { avatars.people().await });
    assert_eq!(
        people,
        [
            DirectoryPerson { name: Some("Ann Lee".into()), emails: vec!["ann@example.com".into()] },
            DirectoryPerson { name: None, emails: vec!["cy@example.com".into()] },
            DirectoryPerson { name: Some("Bo".into()), emails: vec!["bo@corp.test".into()] },
        ]
    );
    assert!(server.requests.lock().unwrap().iter().all(|request| request.header("authorization") == Some("Bearer at") || request.path == "/token"));
    drop(engine);
    cleanup(&config);
}
