//! The public web avatars read without an account: anonymous GETs (Gravatar, site icons, BIMI
//! logos, the pictures Chat publishes) and DNS TXT lookups (BIMI records). Behind a trait so
//! tests answer from memory and never reach the network.
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::future::BoxFuture;
use regex::Regex;

/// A successful (200) answer: the body, at most the `cap` asked for plus one byte, the URL it
/// came from after redirects, and how long it may be reused (`Cache-Control`: `max-age`, zero for
/// `no-store`/`no-cache`; none when the answer doesn't say).
pub struct Page {
    pub body: Vec<u8>,
    pub url: String,
    pub max_age: Option<Duration>,
}

pub trait Web: Send + Sync {
    /// GETs `url`; nil on any failure or status but 200. Reads at most `cap` + 1 bytes, so a
    /// caller can tell an answer past `cap` without downloading all of it.
    fn get(&self, url: String, cap: usize) -> BoxFuture<'static, Option<Page>>;

    /// The TXT records at `name`, each record's strings joined; empty on failure or after 4 s.
    fn txt(&self, name: String) -> BoxFuture<'static, Vec<String>>;
}

/// The real web: 10 s per request, `User-Agent: Mailstream`, no cookies (Swift's ephemeral
/// session), and the system's DNS resolver.
pub struct Internet;

impl Internet {
    pub fn shared() -> Arc<dyn Web> {
        Arc::new(Self)
    }
}

/// The web as a mail's images, stylesheets and fonts are asked for by a browser: as [`Internet`],
/// with Safari's user agent. Google Fonts serves split `woff2` files with a `unicode-range` each only
/// to browsers it knows, so a reader that downloads just the parts a mail's text needs can; to
/// `Mailstream` it gives one whole TrueType file per weight (MS-164).
pub struct Browser;

const SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

fn client(user_agent: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(user_agent)
        .build()
        .expect("the HTTP client builds")
}

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| client("Mailstream"));
static BROWSER: LazyLock<reqwest::Client> = LazyLock::new(|| client(SAFARI));

/// Built from the system's resolver configuration once; nil when it cannot be read.
static RESOLVER: LazyLock<Option<hickory_resolver::TokioResolver>> =
    LazyLock::new(|| hickory_resolver::Resolver::builder_tokio().ok()?.build().ok());

fn get(client: &'static reqwest::Client, url: String, cap: usize) -> BoxFuture<'static, Option<Page>> {
    Box::pin(async move {
        let mut response = client.get(&url).send().await.ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let url = response.url().to_string();
        let max_age = response.headers().get(reqwest::header::CACHE_CONTROL).and_then(|value| max_age(value.to_str().ok()?));
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > cap {
                body.truncate(cap + 1);
                break;
            }
        }
        Some(Page { body, url, max_age })
    })
}

/// How long a `Cache-Control` header lets an answer be reused: zero for `no-store` or `no-cache`,
/// else its `max-age`.
fn max_age(cache_control: &str) -> Option<Duration> {
    let mut age = None;
    for directive in cache_control.split(',').map(str::trim) {
        let (name, value) = directive.split_once('=').unwrap_or((directive, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "no-store" | "no-cache" => return Some(Duration::ZERO),
            "max-age" => age = value.trim().trim_matches('"').parse().ok().map(Duration::from_secs),
            _ => {}
        }
    }
    age
}

impl Web for Browser {
    fn get(&self, url: String, cap: usize) -> BoxFuture<'static, Option<Page>> {
        get(&BROWSER, url, cap)
    }

    fn txt(&self, name: String) -> BoxFuture<'static, Vec<String>> {
        Internet.txt(name)
    }
}

impl Web for Internet {
    fn get(&self, url: String, cap: usize) -> BoxFuture<'static, Option<Page>> {
        get(&HTTP, url, cap)
    }

    fn txt(&self, name: String) -> BoxFuture<'static, Vec<String>> {
        Box::pin(async move {
            let Some(resolver) = RESOLVER.as_ref() else { return Vec::new() };
            let Ok(Ok(lookup)) = tokio::time::timeout(Duration::from_secs(4), resolver.txt_lookup(name)).await else {
                return Vec::new();
            };
            lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    // Character-strings, concatenated into one record.
                    hickory_resolver::proto::rr::RData::TXT(txt) => {
                        let bytes: Vec<u8> = txt.txt_data.iter().flat_map(|part| part.iter().copied()).collect();
                        Some(String::from_utf8_lossy(&bytes).into_owned())
                    }
                    _ => None,
                })
                .collect()
        })
    }
}

static LINK_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<link\b[^>]*>").expect("pattern"));

/// The icons a home page declares, best first: touch icons, then the largest, never mask icons.
pub fn icon_links(html: &str, base: &str) -> Vec<String> {
    let Ok(base) = reqwest::Url::parse(base) else { return Vec::new() };
    let mut scored: Vec<(i64, String)> = Vec::new();
    for tag in LINK_TAG.find_iter(html).map(|found| found.as_str()) {
        let Some(rel) = attribute("rel", tag).map(|rel| rel.to_lowercase()) else { continue };
        if !rel.contains("icon") {
            continue;
        }
        let Some(url) = attribute("href", tag).and_then(|href| base.join(&href).ok()) else { continue };
        let size = attribute("sizes", tag)
            .and_then(|sizes| sizes.split('x').next().and_then(|first| first.parse::<i64>().ok()))
            .unwrap_or(0);
        let score = if rel.contains("apple-touch-icon") { 10_000 } else { 0 }
            + if rel.contains("mask") { -20_000 } else { 0 }
            + size;
        scored.push((score, url.to_string()));
    }
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, url)| url).collect()
}

/// The value of attribute `name` in `tag`, quoted or not.
fn attribute(name: &str, tag: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r#"(?i)\b{name}\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))"#)).ok()?;
    let captures = pattern.captures(tag)?;
    (2..=4).find_map(|index| captures.get(index)).map(|found| found.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_declared_icons() {
        let html = r#"<html><head>
            <link rel="icon" href="/favicon-16.png" sizes="16x16">
            <LINK REL='mask-icon' href='/mask.svg'>
            <link rel="stylesheet" href="/site.css">
            <link href=/big.png rel="icon" sizes="192x192">
            <link rel="apple-touch-icon" href="https://cdn.example.com/touch.png">
        </head></html>"#;
        assert_eq!(
            icon_links(html, "https://www.example.com/home/"),
            [
                "https://cdn.example.com/touch.png",
                "https://www.example.com/big.png",
                "https://www.example.com/favicon-16.png",
                "https://www.example.com/mask.svg",
            ]
        );
    }

    #[test]
    fn reads_how_long_an_answer_may_be_reused() {
        assert_eq!(max_age("public, max-age=31536000"), Some(Duration::from_secs(31_536_000)));
        assert_eq!(max_age("private, max-age=86400"), Some(Duration::from_secs(86_400)));
        assert_eq!(max_age("max-age=600, no-store"), Some(Duration::ZERO));
        assert_eq!(max_age("no-cache"), Some(Duration::ZERO));
        assert_eq!(max_age("public"), None);
    }
}
