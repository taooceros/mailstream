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
}
