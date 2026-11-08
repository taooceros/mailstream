//! `mail.send` and `mail.reply`, as the Swift engine's `ControlService` and `MailStore`
//! (`Store/Replies.swift`) do them: new plain-text mail with attachments, and replies that work
//! out their recipients, subject and quote the way mail clients do, threaded under the message
//! replied to, with an HTML alternative quoting its HTML as Gmail lays it out. Both need `yes`;
//! without it the error says what would be sent.
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

use super::Mail;
use crate::cache::SCHEME;
use crate::commands::Arguments;
use crate::mime::OutgoingMessage;
use crate::model::{Address, Date, MailMessage, MailboxRole, MessageContent, unix_seconds};
use crate::text::{self, Prefix, Subject};
use crate::EngineError;

/// What a reply goes out with, worked out before it is sent.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplyPlan {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// The source's text with every line `> `-quoted, without the attribution line.
    pub quoted_text: String,
    /// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:"
    pub attribution: String,
}

impl ReplyPlan {
    /// The plain-text body: the written text above the attributed quote.
    pub fn plain_body(&self, text: &str) -> String {
        if self.quoted_text.is_empty() { text.to_string() } else { format!("{text}\n\n{}\n{}", self.attribution, self.quoted_text) }
    }
}

/// The subject of a reply to `subject`: unchanged when it already starts as a reply.
pub(crate) fn reply_subject(subject: &str) -> String {
    let raw = subject.trim();
    if Subject::parse(raw).prefix == Some(Prefix::Reply) { raw.to_string() } else { format!("Re: {raw}") }
}

/// Recipients the way mail clients derive them: `Reply-To`, else `From`, or for one of the user's
/// own messages its recipients; reply-all copies everyone else on the source except the user's
/// own addresses.
pub(crate) fn reply_recipients(
    source: &MailMessage,
    content: Option<&MessageContent>,
    own: &std::collections::HashSet<String>,
    all: bool,
) -> (Vec<Address>, Vec<Address>) {
    let envelope = &source.envelope;
    let reply_to = content.map(|content| content.reply_to.clone()).unwrap_or_default();
    let mut to = if source.is_from_user(own) {
        envelope.to.clone()
    } else if reply_to.is_empty() {
        envelope.from.clone()
    } else {
        reply_to
    };
    if !all {
        return (to, Vec::new());
    }
    let mut seen: std::collections::HashSet<String> = to.iter().map(Address::normalized_email).collect();
    seen.extend(own.iter().cloned());
    let cc_source = content.map(|content| content.cc.as_slice()).unwrap_or_default();
    let mut cc: Vec<Address> =
        envelope.to.iter().chain(cc_source).filter(|address| seen.insert(address.normalized_email())).cloned().collect();
    if to.is_empty() && !cc.is_empty() {
        to = vec![cc.remove(0)];
    }
    (to, cc)
}

/// "On Mon, Nov 2, 2026 at 11:41 AM, Josh Patrick wrote:", the date in the local time zone as
/// macOS writes it for English (a narrow no-break space before AM/PM).
pub(crate) fn attribution(source: &MailMessage) -> String {
    let envelope = &source.envelope;
    let sender = envelope.from.first().map_or_else(|| "Someone".to_string(), |from| from.name.clone().unwrap_or_else(|| from.email.clone()));
    let when = envelope.date.map(|date| format!("{}, ", attribution_date(date))).unwrap_or_default();
    format!("On {when}{sender} wrote:")
}

fn attribution_date(date: Date) -> String {
    let seconds = unix_seconds(date);
    let moment = chrono::DateTime::from_timestamp(seconds.floor() as i64, 0).unwrap_or_default();
    moment.with_timezone(&chrono::Local).format("%a, %b %-d, %Y at %-I:%M\u{202F}%p").to_string()
}

/// The line breaks of `CharacterSet.newlines`.
fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

pub(crate) fn quote(text: &str) -> String {
    let body = text.replace("\r\n", "\n");
    let body = body.trim_matches(is_newline);
    if body.is_empty() {
        return String::new();
    }
    body.split('\n').map(|line| if line.starts_with('>') { format!(">{line}") } else { format!("> {line}") }).collect::<Vec<_>>().join("\n")
}

/// The HTML alternative of a reply, laid out the way Gmail does so recipients' clients fold the
/// quote.
pub(crate) fn reply_html(reply: &str, attribution: &str, source: Option<&MessageContent>) -> String {
    let written = reply
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| if line.is_empty() { "<div><br></div>".to_string() } else { format!("<div>{}</div>", text::linkify(line)) })
        .collect::<Vec<_>>()
        .join("\n");
    let mut html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\n<div dir=\"auto\">{written}</div>\n");
    let quote = match source {
        Some(MessageContent { html: Some(body), .. }) => Some(body_contents(body)),
        Some(MessageContent { text: Some(body), .. }) => {
            Some(format!("<div style=\"white-space:pre-wrap\">{}</div>", text::plain_to_html(body)))
        }
        _ => None,
    };
    if let Some(quote) = quote {
        html.push_str(&format!(
            "<br><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">{}<br></div>\n\
             <blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">\n\
             {quote}\n</blockquote></div>\n",
            text::escape_html(attribution)
        ));
    }
    html + "</body></html>\n"
}

/// The inside of `<body>`, without images of inline parts: those are not re-attached, so they
/// would arrive broken.
fn body_contents(html: &str) -> String {
    static OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)<body[^>]*>").unwrap());
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)</body>").unwrap());
    static FRAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)<head[^>]*>[\s\S]*?</head>|<!doctype[^>]*>|</?html[^>]*>").unwrap());
    static INLINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#"(?i)<img[^>]*src\s*=\s*["']?{SCHEME}:[^>]*>"#)).unwrap());
    let inner = match OPEN.find(html) {
        Some(open) => {
            let rest = &html[open.end()..];
            match CLOSE.find_iter(rest).last() {
                Some(close) => rest[..close.start()].to_string(),
                None => rest.to_string(),
            }
        }
        None => FRAME.replace_all(html, "").into_owned(),
    };
    INLINE.replace_all(&inner, "").into_owned()
}

/// The addresses as a confirmation names them.
fn emails(list: &[Address]) -> String {
    list.iter().map(|address| address.email.as_str()).collect::<Vec<_>>().join(", ")
}

fn texts(list: &[Address]) -> Vec<String> {
    list.iter().map(Address::text).collect()
}

fn confirm(message: String) -> EngineError {
    EngineError::new("confirm", message)
}

impl Mail {
    pub(super) async fn send_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let from = args.required("from")?.to_lowercase();
        if !self.state().accounts.iter().any(|account| account.name == from) {
            return Err(Self::unknown_account(&from));
        }
        let (to, cc, bcc) = (addresses(args, "to")?, addresses(args, "cc")?, addresses(args, "bcc")?);
        if to.is_empty() {
            return Err(EngineError::usage("--to needs at least one address."));
        }
        let (subject, body) = (args.required("subject")?, args.required("body")?);
        let attachments = files(&args.list("attachments").unwrap_or_default(), "attachments")?;
        let attached = match attachments.len() {
            0 => String::new(),
            1 => ", 1 attachment".into(),
            count => format!(", {count} attachments"),
        };
        if !args.bool("yes") {
            let everyone: Vec<Address> = to.iter().chain(&cc).chain(&bcc).cloned().collect();
            return Err(confirm(format!(
                "Would send \"{subject}\" ({} characters{attached}) from {from} to {}. Re-run with --yes.",
                body.graphemes(true).count(),
                emails(&everyone)
            )));
        }
        let message = self.outgoing(&from, None, to.clone(), cc, bcc, subject.clone(), body, None, attachments).await;
        self.deliver(&message, &from).await?;
        Ok(json!({ "sent": true, "from": from, "to": texts(&to), "subject": subject }))
    }

    pub(super) async fn reply_command(self: &Arc<Self>, args: &Arguments) -> Result<Value, EngineError> {
        let conversation = self.lookup(&args.required("id")?)?;
        let source =
            conversation.messages.iter().rev().find(|message| message.role != MailboxRole::Drafts).unwrap_or_else(|| conversation.latest()).clone();
        let mut plan = self.reply_plan(&source, args.bool("all")).await?;
        if args.list("to").is_some() {
            plan.to = addresses(args, "to")?;
        }
        if args.list("cc").is_some() {
            plan.cc = addresses(args, "cc")?;
        }
        let bcc = addresses(args, "bcc")?;
        if let Some(subject) = args.string("subject") {
            plan.subject = subject;
        }
        let recipients: Vec<Address> = plan.to.iter().chain(&plan.cc).chain(&bcc).cloned().collect();
        if recipients.is_empty() {
            return Err(EngineError::usage("A reply needs at least one recipient."));
        }
        let attachments = files(&args.list("attachments").unwrap_or_default(), "attachments")?;
        let body = args.required("body")?;
        let account = source.account().to_string();
        if !args.bool("yes") {
            return Err(confirm(format!(
                "Would send \"{}\" from {account} to {}, {} attachments. Re-run with --yes.",
                plan.subject,
                emails(&recipients),
                attachments.len()
            )));
        }
        let content = self.contents.load(&source.message_ref(), self.backend(&account)).await.ok();
        let html = reply_html(&body, &plan.attribution, content.as_deref());
        let message = self
            .outgoing(
                &account,
                Some(&source),
                plan.to.clone(),
                plan.cc.clone(),
                bcc,
                plan.subject.clone(),
                plan.plain_body(&body),
                Some(html),
                attachments,
            )
            .await;
        self.deliver(&message, &account).await?;
        Ok(json!({ "sent": true, "from": account, "to": texts(&recipients), "subject": plan.subject }))
    }

    async fn reply_plan(self: &Arc<Self>, source: &MailMessage, all: bool) -> Result<ReplyPlan, EngineError> {
        let content =
            self.contents.load(&source.message_ref(), self.backend(source.account())).await.map_err(EngineError::failed)?;
        let own = Self::own_addresses(&self.state());
        let (to, cc) = reply_recipients(source, Some(&content), &own, all);
        Ok(ReplyPlan {
            to,
            cc,
            subject: reply_subject(&source.envelope.subject),
            quoted_text: quote(content.text.as_deref().unwrap_or("")),
            attribution: attribution(source),
        })
    }

    /// The message `account` sends, from its identity; a reply to `source` is threaded under it.
    #[allow(clippy::too_many_arguments)]
    async fn outgoing(
        self: &Arc<Self>,
        account: &str,
        source: Option<&MailMessage>,
        to: Vec<Address>,
        cc: Vec<Address>,
        bcc: Vec<Address>,
        subject: String,
        text: String,
        html: Option<String>,
        attachments: Vec<PathBuf>,
    ) -> OutgoingMessage {
        let from = self
            .state()
            .accounts
            .iter()
            .find(|known| known.name == account)
            .and_then(|known| known.identity.clone())
            .unwrap_or_else(|| Address::new(None, account));
        let mut message = OutgoingMessage { from, to, cc, bcc, subject, text, html, attachments, ..OutgoingMessage::default() };
        let Some(source) = source else { return message };
        let references = match self.contents.load(&source.message_ref(), self.backend(account)).await {
            Ok(content) => content.references.clone(),
            Err(_) => Vec::new(),
        };
        message.thread = source.envelope.thread.clone();
        match &source.envelope.message_id {
            Some(id) => {
                message.in_reply_to = Some(id.clone());
                message.references = references.into_iter().filter(|reference| reference != id).chain([id.clone()]).collect();
            }
            None => message.references = references,
        }
        message
    }

    /// Sends from `account`, then picks up the sent copy a moment later.
    async fn deliver(self: &Arc<Self>, message: &OutgoingMessage, account: &str) -> Result<(), EngineError> {
        let backend = self.backend(account).ok_or_else(|| EngineError::failed("The account is no longer signed in."))?;
        backend.send(message).await.map_err(|error| EngineError::failed(error.message))?;
        let (mail, account) = (self.clone(), account.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            mail.refresh_counts();
            let keys: Vec<_> = Self::default_keys(&mail.state()).into_iter().filter(|key| key.account == account).collect();
            mail.fetch_first_pages(&keys).await;
        });
        Ok(())
    }
}

/// The addresses of list argument `name`; `usage` names the first that is not one.
fn addresses(args: &Arguments, name: &str) -> Result<Vec<Address>, EngineError> {
    args.list(name)
        .unwrap_or_default()
        .into_iter()
        .map(|raw| {
            Address::parse(&raw)
                .filter(|address| address.email.contains('@'))
                .ok_or_else(|| EngineError::usage(format!("--{name}: \"{raw}\" is not an email address.")))
        })
        .collect()
}

/// The files named by full path, each one a readable file rather than a folder.
fn files(paths: &[String], argument: &str) -> Result<Vec<PathBuf>, EngineError> {
    paths
        .iter()
        .map(|path| {
            let readable = Path::new(path).is_absolute()
                && std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir())
                && std::fs::File::open(path).is_ok();
            if readable {
                Ok(PathBuf::from(path))
            } else {
                Err(EngineError::usage(format!("--{argument}: \"{path}\" is not a readable file (give its full path).")))
            }
        })
        .collect()
}
