//! Meeting times, sizes and text bodies as the reader shows them; dates, subjects and avatars
//! come from the engine (`mailstream`, MS-235).

use chrono::{DateTime, Datelike, Local, NaiveDate};

/// When a meeting is, as an invitation's card says it (Flutter's `InvitationCard._when`): its day
/// and times, or the days of an all-day one. `start` and `end` are RFC 3339; `days` are an
/// all-day meeting's first day and the day after its last (`2026-11-08`).
pub fn meeting_when(all_day: bool, days: (Option<&str>, Option<&str>), start: &str, end: &str, now: DateTime<Local>) -> String {
    let day = |date: NaiveDate| {
        date.format(if date.year() == now.year() { "%A, %B %-d" } else { "%A, %B %-d, %Y" }).to_string()
    };
    if all_day
        && let (Some(first), Some(after)) = days
        && let (Ok(first), Ok(after)) = (first.parse::<NaiveDate>(), after.parse::<NaiveDate>())
    {
        let last = after.pred_opt().unwrap_or(after);
        return if last <= first { format!("{} · All day", day(first)) } else { format!("{} – {}", day(first), day(last)) };
    }
    let parse = |date: &str| DateTime::parse_from_rfc3339(date).ok().map(|date| date.with_timezone(&Local));
    let (Some(start), Some(end)) = (parse(start), parse(end)) else { return String::new() };
    let time = |date: DateTime<Local>| date.format("%-I:%M %p").to_string();
    if start.date_naive() == end.date_naive() {
        format!("{} · {} – {}", day(start.date_naive()), time(start), time(end))
    } else {
        format!("{}, {} – {}, {}", day(start.date_naive()), time(start), day(end.date_naive()), time(end))
    }
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
    fn meetings_read_as_their_day_and_times() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 11, 11, 9, 0, 0).unwrap();
        let days = (Some("2026-11-20"), Some("2026-11-21"));
        assert_eq!(meeting_when(true, days, "", "", now), "Friday, November 20 · All day");
        let days = (Some("2026-11-20"), Some("2026-11-23"));
        assert_eq!(meeting_when(true, days, "", "", now), "Friday, November 20 – Sunday, November 22");
        let at = |day, hour| Local.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap().to_rfc3339();
        assert_eq!(meeting_when(false, (None, None), &at(13, 15), &at(13, 16), now), "Friday, November 13 · 3:00 PM – 4:00 PM");
        assert_eq!(
            meeting_when(false, (None, None), &at(13, 22), &at(14, 1), now),
            "Friday, November 13, 10:00 PM – Saturday, November 14, 1:00 AM"
        );
        let last_year = Local.with_ymd_and_hms(2025, 1, 1, 9, 0, 0).unwrap();
        assert_eq!(
            meeting_when(false, (None, None), &at(13, 15), &at(13, 16), last_year),
            "Friday, November 13, 2026 · 3:00 PM – 4:00 PM"
        );
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
