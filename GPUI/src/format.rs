//! Dates, subjects and sizes as the native app shows them (`ConversationRow.format`, `Subject`).

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};

fn local(date: DateTime<Utc>) -> DateTime<Local> {
    date.with_timezone(&Local)
}

/// A row's date: time today, month and day this year, else numeric.
pub fn list_date(date: Option<DateTime<Utc>>, now: DateTime<Local>) -> String {
    let Some(date) = date.map(local) else { return String::new() };
    if date.date_naive() == now.date_naive() {
        date.format("%-I:%M %p").to_string()
    } else if date.year() == now.year() {
        date.format("%b %-d").to_string()
    } else {
        date.format("%-m/%-d/%y").to_string()
    }
}

/// A row's group header.
pub fn list_section(date: Option<DateTime<Utc>>, now: DateTime<Local>) -> String {
    let Some(date) = date.map(local) else { return "Older".into() };
    let day = date.date_naive();
    let today = now.date_naive();
    if day == today {
        return "Today".into();
    }
    if Some(day) == today.checked_sub_days(Days::new(1)) {
        return "Yesterday".into();
    }
    let week: NaiveDate = today - Days::new(u64::from(today.weekday().num_days_from_sunday()));
    if day >= week {
        return "This Week".into();
    }
    if day >= week - Days::new(7) {
        return "Last Week".into();
    }
    if date.year() == now.year() { date.format("%B").to_string() } else { date.format("%B %Y").to_string() }
}

/// A message's date in the reader.
pub fn message_date(date: Option<DateTime<Utc>>, now: DateTime<Local>) -> String {
    let Some(date) = date.map(local) else { return String::new() };
    if date.date_naive() == now.date_naive() {
        date.format("%-I:%M %p").to_string()
    } else if date.year() == now.year() {
        date.format("%b %-d, %-I:%M %p").to_string()
    } else {
        date.format("%b %-d, %Y, %-I:%M %p").to_string()
    }
}

/// A message's time of day, for the reader's chat log.
pub fn message_time(date: Option<DateTime<Utc>>) -> String {
    date.map(local).map(|date| date.format("%-I:%M %p").to_string()).unwrap_or_default()
}

/// A day divider's label in a log (`ReaderPage.dayDivider`, Flutter's `dayDivider`).
pub fn day_divider(date: DateTime<Local>, now: DateTime<Local>) -> String {
    let day = date.date_naive();
    let today = now.date_naive();
    if day == today {
        "Today".into()
    } else if Some(day) == today.checked_sub_days(Days::new(1)) {
        "Yesterday".into()
    } else if date.year() == now.year() {
        date.format("%A, %B %-d").to_string()
    } else {
        date.format("%B %-d, %Y").to_string()
    }
}

const REPLY_PREFIXES: [&str; 11] = ["re", "fwd", "fw", "aw", "wg", "sv", "vs", "antw", "回复", "答复", "转发"];

/// The length of a reply or forward prefix (`Re:`, `Fwd[2]:`, `回复：`) at the start of `s`.
fn reply_prefix(s: &str) -> Option<usize> {
    let lower = s.to_lowercase();
    // Lowercasing these prefixes keeps byte lengths, so offsets in `lower` hold in `s`.
    for prefix in REPLY_PREFIXES {
        let Some(rest) = lower.strip_prefix(prefix) else { continue };
        let mut at = prefix.len();
        let mut rest = rest;
        if let Some(counted) = rest.strip_prefix('[') {
            let digits = counted.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && counted[digits..].starts_with(']') {
                at += digits + 2;
                rest = &counted[digits + 1..];
            }
        }
        let trimmed = rest.trim_start();
        at += rest.len() - trimmed.len();
        for colon in [':', '：'] {
            if trimmed.starts_with(colon) {
                let after = &trimmed[colon.len_utf8()..];
                return Some(at + colon.len_utf8() + (after.len() - after.trim_start().len()));
            }
        }
    }
    None
}

/// A subject split for the list: reply and forward prefixes dropped, leading `[list]` tags kept
/// apart so the row can dim them.
pub fn split_subject(subject: &str) -> (String, String) {
    let mut rest = subject.trim();
    let mut tags: Vec<&str> = Vec::new();
    loop {
        if let Some(end) = reply_prefix(rest) {
            rest = &rest[end..];
            continue;
        }
        if rest.starts_with('[')
            && let Some(close) = rest.find(']')
            && (2..=41).contains(&rest[..close].chars().count())
            && close + 1 < rest.len()
        {
            tags.push(&rest[..=close]);
            rest = rest[close + 1..].trim_start();
            continue;
        }
        break;
    }
    let title = if rest.is_empty() { "(no subject)".to_owned() } else { rest.to_owned() };
    (tags.join(" "), title)
}

pub fn file_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", (bytes as f64 / 1024.0).round()),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// A text body for display: line endings normalized, trailing blanks and runs of blank lines
/// dropped.
pub fn body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// A text body as the reader shows it (Flutter's `FoldedText`): what was written, and the quoted
/// history and signature that fold behind a toggle, if any. Lines lose the indentation and empty
/// cells text taken from HTML layouts keeps, runs of blank lines go, and hard-wrapped lines (a long
/// line stopping mid-sentence before one running on in lower case) join, so they reflow.
pub fn fold(text: &str) -> (String, Option<String>) {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        if let Some(last) = lines.last_mut()
            && wrapped(last, line)
        {
            last.push(' ');
            last.push_str(line);
            continue;
        }
        lines.push(line.to_owned());
    }
    let cut = lines.iter().skip(1).position(|line| folds(line)).map_or(lines.len(), |i| i + 1);
    let join = |part: &[String]| part.join("\n").trim().to_owned();
    let folded = join(&lines[cut..]);
    (join(&lines[..cut]), (!folded.is_empty()).then_some(folded))
}

/// Whether `line` was hard-wrapped before `next`.
fn wrapped(line: &str, next: &str) -> bool {
    line.chars().count() >= 50
        && !line.starts_with('>')
        && !line.ends_with(['.', '!', '?', ':', ';', ')', '>'])
        && next.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '(')
}

/// Whether quoted history or a signature starts at `line` (trimmed).
fn folds(line: &str) -> bool {
    let lower = line.to_lowercase();
    let length = line.chars().count();
    (line.starts_with("On ") && line.ends_with("wrote:") && (13..=250).contains(&length))
        || (line.starts_with('在') && (line.ends_with("写道:") || line.ends_with("写道：")) && length >= 5)
        || (line.starts_with("--") && {
            let inner = lower.trim_start_matches('-').trim_start();
            ["original message", "forwarded message"]
                .iter()
                .any(|title| inner.strip_prefix(title).is_some_and(|rest| rest.trim_start().starts_with("--")))
        })
        || (length >= 10 && line.chars().all(|c| c == '_'))
        || line.starts_with('>')
        || line == "--"
        || lower.starts_with("sent from my ")
        || lower.starts_with("get outlook for ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_drop_reply_prefixes_and_keep_list_tags() {
        assert_eq!(split_subject("Re: Fwd[2]: hello"), (String::new(), "hello".into()));
        assert_eq!(split_subject("回复：会议"), (String::new(), "会议".into()));
        assert_eq!(split_subject("[rust-lang/rust] Re: PR #1"), ("[rust-lang/rust]".into(), "PR #1".into()));
        assert_eq!(split_subject("[only]"), (String::new(), "[only]".into()));
        assert_eq!(split_subject("Reply all"), (String::new(), "Reply all".into()));
        assert_eq!(split_subject("  "), (String::new(), "(no subject)".into()));
    }

    #[test]
    fn quoted_history_and_signatures_fold() {
        let reply = "Sounds good, see you then.\n\nOn Mon, Nov 10, 2026 at 9:00 AM Dan <dan@example.org> wrote:\n> Lunch?\n> Dan";
        assert_eq!(
            fold(reply),
            ("Sounds good, see you then.".into(), Some("On Mon, Nov 10, 2026 at 9:00 AM Dan <dan@example.org> wrote:\n> Lunch?\n> Dan".into()))
        );
        assert_eq!(fold("好的\n在 2026年11月10日 写道：\n原文").1.as_deref(), Some("在 2026年11月10日 写道：\n原文"));
        assert_eq!(fold("Hi\n-----Original Message-----\nFrom: x").1.as_deref(), Some("-----Original Message-----\nFrom: x"));
        assert_eq!(fold("Thanks\n-- \nPriya").1.as_deref(), Some("--\nPriya"));
        assert_eq!(fold("Thanks\nSent from my iPhone").1.as_deref(), Some("Sent from my iPhone"));
        // The first line never folds, so a body is never only a toggle; mail without quotes keeps
        // everything.
        assert_eq!(fold("> all quote\n> more"), ("> all quote".into(), Some("> more".into())));
        assert_eq!(fold("  Indented\n\n\n\nGap  "), ("Indented\n\nGap".into(), None));
    }

    #[test]
    fn hard_wrapped_lines_join() {
        let wrapped = "This line was wrapped by the sender's mail program at about\nseventy-two columns, as plain text mail often is.\nNext sentence.";
        assert_eq!(
            fold(wrapped).0,
            "This line was wrapped by the sender's mail program at about seventy-two columns, as plain text mail often is.\nNext sentence."
        );
    }
}
