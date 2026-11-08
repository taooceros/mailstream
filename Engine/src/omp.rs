//! Classifying conversations with the user's `omp` CLI, as the Swift engine's `OmpTagger`,
//! `ToolEnvironment` and `ProcessRunner` do it (`Sources/MailstreamKit/Backend/OmpTagger.swift`,
//! `Tools.swift`, `ProcessRunner.swift`): a one-shot, tool-less run of the model the
//! `aiTagging.model` setting names, with the same arguments, prompt and lenient reading of the
//! answer.
use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
use unicode_segmentation::UnicodeSegmentation;

use crate::model::{Date, date_from_unix, unix_seconds};
use crate::tagging::{Category, ConversationTag};

/// One conversation as the model sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Opaque key echoed back in the result.
    pub id: String,
    pub sender_name: String,
    pub sender_email: String,
    /// The user is a direct To recipient, not cc'd or reached through a list.
    pub user_in_to: bool,
    pub subject: String,
    /// The message's one-line snippet; empty when not loaded yet. Cut to `MAX_SNIPPET` here.
    pub snippet: String,
    /// When that message arrived.
    pub date: Option<Date>,
    pub message_count: usize,
    pub user_replied: bool,
    pub latest_from_user: bool,
}

/// The `aiTagging.*` settings a run uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `--model` value.
    pub model: String,
    /// The omp to run; empty picks the newest on PATH.
    pub executable: String,
}

const MAX_SNIPPET: usize = 200;
/// Days after which a routine ask no longer waits on the user.
const ROUTINE_DAYS: u64 = 14;
/// Longest topic label kept, from the model or a correction.
pub const MAX_LABEL_LENGTH: usize = 16;
/// omp's own limit; the process is killed if it overruns it by `EXIT_MARGIN`.
const MAX_TIME: u64 = 120;
const EXIT_MARGIN: u64 = 30;

pub struct Tagger {
    tools: Arc<Tools>,
    working_directory: PathBuf,
    /// The omp found for an `Options::executable`, so PATH is searched once per choice.
    executables: Mutex<HashMap<String, PathBuf>>,
}

impl Tagger {
    pub fn new(tools: Arc<Tools>, working_directory: PathBuf) -> Self {
        Self { tools, working_directory, executables: Mutex::new(HashMap::new()) }
    }

    /// Tags keyed by `Item::id`. Items the model skipped or answered invalidly are absent.
    pub async fn tag(&self, items: &[Item], options: &Options) -> Result<HashMap<String, ConversationTag>, String> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let executable = self.resolved_executable(&options.executable).await?;
        let now = crate::model::now();
        let arguments = arguments(&prompt(items, now), &options.model);
        let _ = std::fs::create_dir_all(&self.working_directory);
        let output = run(
            &executable,
            &arguments,
            self.tools.environment().await,
            Some(&self.working_directory),
            true,
            Duration::from_secs(MAX_TIME + EXIT_MARGIN),
        )
        .await?;
        if output.status != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr.lines().rev().map(str::trim).find(|line| !line.is_empty());
            return Err(match line {
                Some(line) => line.chars().take(160).collect(),
                None => format!("omp exited with status {}", output.status),
            });
        }
        parse(&output.stdout, items, now)
    }

    async fn resolved_executable(&self, configured: &str) -> Result<PathBuf, String> {
        if let Some(known) = self.executables.lock().expect("executables lock").get(configured) {
            return Ok(known.clone());
        }
        let found = if configured.is_empty() {
            locate(&self.tools).await.ok_or_else(|| "omp not found on PATH; set aiTagging.ompPath".to_string())?
        } else {
            let path = expand_tilde(configured);
            if !is_executable(&path) {
                return Err(format!("aiTagging.ompPath {configured} is not an executable"));
            }
            path
        };
        self.executables.lock().expect("executables lock").insert(configured.to_string(), found.clone());
        Ok(found)
    }
}

/// The PATH candidate reporting the highest `--version`; ties and unversioned candidates keep
/// PATH order.
async fn locate(tools: &Tools) -> Option<PathBuf> {
    let environment = tools.environment().await;
    let candidates = tools.executables("omp").await;
    let versions = futures::future::join_all(candidates.iter().map(|candidate| async move {
        let output = run_quickly(candidate, &["--version"], environment).await;
        output.and_then(|text| version(&text)).unwrap_or_default()
    }))
    .await;
    let mut best: Option<usize> = None;
    for (index, version) in versions.iter().enumerate() {
        if best.is_none_or(|best| version > &versions[best]) {
            best = Some(index);
        }
    }
    best.map(|index| candidates[index].clone())
}

/// `omp/1.2.3` in `--version`'s output, as numbers.
fn version(output: &str) -> Option<Vec<u64>> {
    static PATTERN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"omp/(\d+)\.(\d+)\.(\d+)").expect("version pattern"));
    let found = PATTERN.captures(output)?;
    Some((1..=3).filter_map(|group| found.get(group)?.as_str().parse().ok()).collect())
}

fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            dirs::home_dir().map_or_else(|| PathBuf::from(path), |home| home.join(rest.trim_start_matches('/')))
        }
        _ => PathBuf::from(path),
    }
}

/// `FileManager.isExecutableFile`: the process may execute it.
fn is_executable(path: &Path) -> bool {
    let Ok(text) = CString::new(path.as_os_str().as_encoded_bytes()) else { return false };
    // SAFETY: `text` is a NUL-terminated path that outlives the call.
    unsafe { libc::access(text.as_ptr(), libc::X_OK) == 0 }
}

// MARK: - Prompt

fn arguments(prompt: &str, model: &str) -> Vec<String> {
    let model = if model.is_empty() { "smol" } else { model };
    [
        "-p",
        "--no-session",
        "--no-tools",
        "--no-skills",
        "--no-rules",
        "--no-context-files",
        "--no-extensions",
        "--no-lsp",
        "--no-title",
        "--thinking",
        "off",
        "--model",
        model,
        "--max-time",
        &MAX_TIME.to_string(),
        "--system-prompt",
        SYSTEM_PROMPT,
        prompt,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

const SYSTEM_PROMPT: &str = "\
You sort email conversations for a mail client. The user message starts with today's date (the \
user's), then a JSON array with one object per conversation: id; from (sender of the latest \
incoming message); toMe (true if the user is a direct To recipient, false if cc'd or reached via a \
list); subject; snippet (start of that message, may be absent); date (when that message arrived) \
and ageDays (days since then); routineUntil (date plus 14 days); count (messages in \
thread); replied (the user already replied in the thread); lastFromMe (the latest message was sent \
by the user).

Reply with ONLY a JSON array, no prose or code fences, one object per input id:
{\"id\":\"<id>\",\"category\":\"<category>\",\"toRespond\":<bool>,\"until\":<\"YYYY-MM-DD\" or null>,\"labels\":[<string>...]}

category, exactly one of:
- people: written by a human to the user or a small group: colleagues, friends, family, a \
recruiter or support agent writing personally. Human-written mail is people even from a company \
address or when cc'd.
- notification: automated transactional or service mail: receipts, orders, shipping, bills, bank \
and security alerts, codes, password resets, account notices, CI/GitHub/issue-tracker activity, \
social-network alerts.
- newsletter: bulk content or marketing: newsletters, digests, promotions, product announcements, \
surveys, mass mailing-list bulletins.
- invitation: calendar invitations, event RSVPs and their responses or updates.

toRespond: true when the user must personally reply to or act on the latest incoming message; \
always false when lastFromMe is true. An earlier reply (replied) does not answer a newer ask.
- people: a human directly asks the user a question or makes a request (send, review, confirm, \
decide, attend). False for FYI mail.
- notification: only when the mail needs a step from the user: approve, sign or pay something; a \
payment failed or a card or account is blocked or about to lapse; a security alert about a sign-in, \
device or app access the user should check; a person messaged, mentioned or asked the user through \
a service (Slack, Teams, an issue or pull request, review comments or suggestions on the user's own \
document or pull request); a deadline or a form the user must complete. False for receipts, orders, \
shipping, statements or balances that are merely available, codes and password resets, \
confirmations of something the user just did (submission received, booking confirmed, agreement \
signed), social-network and job alerts, digests, CI and bot activity, broadcast mentions \
(@everyone, @channel), and repository activity that does not ask the user anything.
- invitation: an unanswered invitation sent to the user personally (toMe) that needs an RSVP. False \
for responses, updates, cancellations and mass event announcements.
- newsletter: always false.

Asks expire, and the list must not fill with old business. toRespond is false when the deadline, \
due date, event, meeting, RSVP-by or offer window the mail names is before today; when the ask \
is routine and ageDays is over 14: chat and Slack pings, comments or suggestions, \
RSVPs and event sign-ups, verification or confirmation links, reminders, surveys, course \
evaluations, conference and HotCRP notices; and whenever ageDays is over 90. Mail older than \
14 days stays toRespond only when it is genuinely important and plausibly still open: \
money owed or a failed payment, a blocked account or lapsing coverage, legal, tax, visa, \
employment or degree paperwork still required, or a person waiting on a direct personal question \
or request.

until: null when toRespond is false. Otherwise the last day the ask still needs the user: the \
deadline, due date, event or meeting date the mail names; else routineUntil for a routine ask; \
null only for an important matter that stays open until handled.

labels: 0-2 English Title Case topic words, each at most 16 characters, e.g. \"Finance\", \"Travel\", \
\"Work\", \"Shopping\", \"Security\", \"Health\", \"Education\", \"Tech\". Prefer broad, reusable topics; use [] \
when unclear.";

/// `date` in the user's time zone.
fn local(date: Date) -> DateTime<Local> {
    let seconds = unix_seconds(date);
    let whole = seconds.floor();
    let nanos = ((seconds - whole) * 1e9) as u32;
    DateTime::from_timestamp(whole as i64, nanos.min(999_999_999)).unwrap_or_default().with_timezone(&Local)
}

/// A calendar day as the model reads and writes it: `2026-11-07`.
fn day(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Today's date, then the items numbered from 1 so the model echoes short ids; `parse` maps them back.
fn prompt(items: &[Item], now: Date) -> String {
    let today = local(now);
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let name = item.sender_name.trim();
            let from = if name.is_empty() || name == item.sender_email {
                item.sender_email.clone()
            } else {
                format!("{name} <{}>", item.sender_email)
            };
            let mut entry = Map::new();
            entry.insert("id".into(), json!((index + 1).to_string()));
            entry.insert("from".into(), json!(from));
            entry.insert("toMe".into(), json!(item.user_in_to));
            entry.insert("subject".into(), json!(item.subject));
            let (snippet, _) = crate::text::prefix_characters(&item.snippet, MAX_SNIPPET);
            if !snippet.is_empty() {
                entry.insert("snippet".into(), json!(snippet));
            }
            if let Some(date) = item.date.map(local) {
                let age = (today.date_naive() - date.date_naive()).num_days();
                entry.insert("date".into(), json!(day(date.date_naive())));
                entry.insert("ageDays".into(), json!(age.max(0)));
                if let Some(until) = date.date_naive().checked_add_days(Days::new(ROUTINE_DAYS)) {
                    entry.insert("routineUntil".into(), json!(day(until)));
                }
            }
            entry.insert("count".into(), json!(item.message_count));
            entry.insert("replied".into(), json!(item.user_replied));
            entry.insert("lastFromMe".into(), json!(item.latest_from_user));
            String::from_utf8(crate::json::encode(&Value::Object(entry), false, false)).expect("JSON is UTF-8")
        })
        .collect();
    format!("today: {} ({})\n[\n{}\n]", day(today.date_naive()), today.format("%A"), lines.join(",\n"))
}

// MARK: - Parsing

/// One entry of the model's answer. Lenient: a malformed entry reads empty and is skipped
/// instead of failing the whole batch.
struct Answer {
    number: Option<i64>,
    category: Option<Category>,
    to_respond: bool,
    /// "YYYY-MM-DD", the last day the ask needs the user.
    until: Option<String>,
    labels: Vec<String>,
}

impl Answer {
    fn read(value: &Value) -> Self {
        let field = |name: &str| value.as_object().and_then(|object| object.get(name));
        let number = field("id").and_then(|id| match id {
            Value::Number(number) => {
                number.as_i64().or_else(|| number.as_f64().filter(|value| value.fract() == 0.0).map(|value| value as i64))
            }
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        });
        let labels = match field("labels") {
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                items.iter().filter_map(|item| item.as_str().map(String::from)).collect()
            }
            _ => Vec::new(),
        };
        Self {
            number,
            category: field("category").and_then(Value::as_str).and_then(category),
            to_respond: field("toRespond").and_then(Value::as_bool).unwrap_or(false),
            until: field("until").and_then(Value::as_str).map(String::from),
            labels,
        }
    }
}

fn category(raw: &str) -> Option<Category> {
    let name = raw.trim().to_lowercase();
    Category::named(&name).or_else(|| name.strip_suffix('s').and_then(Category::named))
}

fn title_case(raw: &str) -> String {
    raw.split_whitespace()
        .map(|word| {
            let mut graphemes = word.graphemes(true);
            let first = graphemes.next().unwrap_or_default();
            first.to_uppercase() + graphemes.as_str()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The local midnight starting the day after `last_day`: when an ask lapses.
fn day_after(last_day: &str) -> Option<Date> {
    let date = NaiveDate::parse_from_str(last_day.trim(), "%Y-%m-%d").ok()?.checked_add_days(Days::new(1))?;
    let midnight = Local.from_local_datetime(&date.and_hms_opt(0, 0, 0)?).earliest()?;
    Some(date_from_unix(midnight.timestamp() as f64))
}

fn parse(data: &[u8], items: &[Item], now: Date) -> Result<HashMap<String, ConversationTag>, String> {
    let text = String::from_utf8_lossy(data);
    let answers = match (text.find('['), text.rfind(']')) {
        (Some(start), Some(end)) if start < end => serde_json::from_str::<Vec<Value>>(&text[start..=end]).ok(),
        _ => None,
    };
    let Some(answers) = answers else {
        let preview: String = text.trim().chars().take(80).collect();
        return Err(if preview.is_empty() { "omp returned nothing".into() } else { format!("unexpected omp output: {preview}") });
    };
    let mut result = HashMap::new();
    for answer in answers.iter().map(Answer::read) {
        let (Some(number), Some(category)) = (answer.number, answer.category) else { continue };
        let Some(item) = usize::try_from(number - 1).ok().and_then(|index| items.get(index)) else { continue };
        let mut labels: Vec<String> = Vec::new();
        for raw in &answer.labels {
            let label = title_case(raw);
            if label.is_empty() || label.graphemes(true).count() > MAX_LABEL_LENGTH || labels.contains(&label) {
                continue;
            }
            labels.push(label);
            if labels.len() == 2 {
                break;
            }
        }
        let mut tag = ConversationTag {
            category,
            to_respond: answer.to_respond && category != Category::Newsletter && !item.latest_from_user,
            labels,
            waiting_until: answer.until.as_deref().and_then(day_after),
        };
        if !tag.is_waiting(now) {
            tag.to_respond = false;
            tag.waiting_until = None;
        }
        result.insert(item.id.clone(), tag);
    }
    Ok(result)
}

// MARK: - Environment

/// The environment omp needs. A GUI app inherits launchd's minimal PATH, so the user's
/// login-shell PATH is recovered once, in the background from `start` on.
pub struct Tools {
    environment: OnceCell<HashMap<String, String>>,
}

impl Tools {
    pub fn new() -> Self {
        Self { environment: OnceCell::new() }
    }

    /// An environment given as is (`Config::tool_environment`).
    pub fn fixed(environment: HashMap<String, String>) -> Self {
        Self { environment: OnceCell::new_with(Some(environment)) }
    }

    /// Starts recovering the environment on the current runtime.
    pub fn start(self: &Arc<Self>) {
        if self.environment.initialized() {
            return;
        }
        let tools = self.clone();
        tokio::spawn(async move {
            tools.environment().await;
        });
    }

    /// The recovered environment, recovering it first if no one has yet.
    pub async fn environment(&self) -> &HashMap<String, String> {
        self.environment.get_or_init(recover).await
    }

    /// Executable files called `name` on the recovered PATH, in PATH order, one per resolved target.
    async fn executables(&self, name: &str) -> Vec<PathBuf> {
        let path = self.environment().await.get("PATH").cloned().unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        path.split(':')
            .filter(|directory| !directory.is_empty())
            .map(|directory| Path::new(directory).join(name))
            .filter(|candidate| {
                is_executable(candidate)
                    && seen.insert(std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone()))
            })
            .collect()
    }
}

async fn recover() -> HashMap<String, String> {
    let mut environment: HashMap<String, String> = std::env::vars().collect();
    let home = dirs::home_dir().unwrap_or_default().display().to_string();
    let mut directories: Vec<String> = Vec::new();
    if let Some(login) = login_shell_path(&environment).await {
        directories.extend(login.split(':').map(String::from));
    }
    directories.extend(
        [format!("{home}/.local/bin")]
            .into_iter()
            .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from)),
    );
    directories.extend(environment.get("PATH").map(|path| path.split(':').map(String::from).collect::<Vec<_>>()).unwrap_or_default());
    let mut seen = std::collections::HashSet::new();
    directories.retain(|directory| !directory.is_empty() && seen.insert(directory.clone()));
    environment.insert("PATH".into(), directories.join(":"));
    environment.insert("NO_COLOR".into(), "1".into());
    environment
}

async fn login_shell_path(environment: &HashMap<String, String>) -> Option<String> {
    let shell = environment.get("SHELL").cloned().unwrap_or_else(|| "/bin/zsh".into());
    let text = run_quickly(
        Path::new(&shell),
        &["-l", "-c", "printf '__MS_PATH__%s__MS_PATH__' \"$PATH\""],
        environment,
    )
    .await
    .unwrap_or_default();
    let parts: Vec<&str> = text.split("__MS_PATH__").collect();
    (parts.len() >= 3).then(|| parts[1].to_string())
}

/// Runs a short-lived command for its output, giving up after three seconds.
async fn run_quickly(executable: &Path, arguments: &[&str], environment: &HashMap<String, String>) -> Option<String> {
    let arguments: Vec<String> = arguments.iter().map(|argument| argument.to_string()).collect();
    let output = run(executable, &arguments, environment, None, false, Duration::from_secs(3)).await.ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

// MARK: - Processes

pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    /// Empty unless the run captured it.
    pub stderr: Vec<u8>,
}

/// SIGTERM before SIGKILL for a child the run gave up on.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// A child still running when its run ends early (timeout, or the caller dropping the future):
/// it gets SIGTERM, then SIGKILL if it outlives `KILL_GRACE`.
struct Running(Option<tokio::process::Child>);

impl Drop for Running {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        if let Some(pid) = child.id() {
            // SAFETY: plain signal to the child this run started and has not reaped.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            let _ = child.start_kill();
            return;
        };
        runtime.spawn(async move {
            if tokio::time::timeout(KILL_GRACE, child.wait()).await.is_err() {
                let _ = child.kill().await;
            }
        });
    }
}

/// Runs a child process with only `environment`, its pipes drained as it writes (so none fills
/// up and stalls it). Returns once it has exited and its pipes hit EOF; the timeout or dropping
/// the future ends the run at once and stops the child.
pub async fn run(
    executable: &Path,
    arguments: &[String],
    environment: &HashMap<String, String>,
    current_directory: Option<&Path>,
    captures_stderr: bool,
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = tokio::process::Command::new(executable);
    command.args(arguments).env_clear().envs(environment).stdin(Stdio::null()).stdout(Stdio::piped());
    command.stderr(if captures_stderr { Stdio::piped() } else { Stdio::null() });
    if let Some(directory) = current_directory {
        command.current_dir(directory);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let (mut stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let mut running = Running(Some(child));
    let finished = async {
        let read_out = async {
            let mut data = Vec::new();
            if let Some(pipe) = stdout.as_mut() {
                pipe.read_to_end(&mut data).await?;
            }
            Ok::<_, std::io::Error>(data)
        };
        let read_err = async {
            let mut data = Vec::new();
            if let Some(mut pipe) = stderr {
                pipe.read_to_end(&mut data).await?;
            }
            Ok::<_, std::io::Error>(data)
        };
        let child = running.0.as_mut().expect("the child runs until the run ends");
        let (status, out, err) = tokio::join!(child.wait(), read_out, read_err);
        Ok::<_, std::io::Error>((status?, out?, err?))
    };
    let name = executable.file_name().map_or_else(|| executable.display().to_string(), |name| name.to_string_lossy().into());
    let (status, stdout, stderr) = match tokio::time::timeout(timeout, finished).await {
        Ok(result) => result.map_err(|error| error.to_string())?,
        Err(_) => return Err(format!("{name} timed out")),
    };
    running.0 = None;
    use std::os::unix::process::ExitStatusExt;
    let status = status.code().or_else(|| status.signal()).unwrap_or(-1);
    Ok(Output { status, stdout, stderr })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// An executable shell script at `path`.
    pub(crate) fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn item(id: &str, latest_from_user: bool) -> Item {
        Item {
            id: id.into(),
            sender_name: "Ann Lee".into(),
            sender_email: "ann@x.com".into(),
            user_in_to: true,
            subject: "Lunch?".into(),
            snippet: "Are you free / on Friday".into(),
            date: Some(crate::model::now() - 3.0 * 86_400.0),
            message_count: 2,
            user_replied: false,
            latest_from_user,
        }
    }

    #[test]
    fn writes_the_prompt_as_the_swift_engine() {
        let now = date_from_unix(Local.with_ymd_and_hms(2026, 11, 7, 15, 0, 0).unwrap().timestamp() as f64);
        let mut first = item("k1", false);
        first.date = Some(now - 3.0 * 86_400.0);
        let mut second = item("k2", true);
        second.sender_name = "  ".into();
        second.snippet = String::new();
        second.date = None;
        let text = prompt(&[first, second], now);
        assert_eq!(
            text,
            "today: 2026-11-07 (Saturday)\n[\n\
             {\"ageDays\":3,\"count\":2,\"date\":\"2026-11-04\",\"from\":\"Ann Lee <ann@x.com>\",\"id\":\"1\",\"lastFromMe\":false,\"replied\":false,\"routineUntil\":\"2026-11-18\",\"snippet\":\"Are you free / on Friday\",\"subject\":\"Lunch?\",\"toMe\":true},\n\
             {\"count\":2,\"from\":\"ann@x.com\",\"id\":\"2\",\"lastFromMe\":true,\"replied\":false,\"subject\":\"Lunch?\",\"toMe\":true}\n]"
        );
        let args = arguments("p", "");
        assert_eq!(args[..13].join(" "), "-p --no-session --no-tools --no-skills --no-rules --no-context-files --no-extensions --no-lsp --no-title --thinking off --model smol");
        assert_eq!(&args[13..16], ["--max-time", "120", "--system-prompt"]);
        assert_eq!(args.last().map(String::as_str), Some("p"));
    }

    #[test]
    fn reads_answers_leniently() {
        let items = [item("a", false), item("b", true), item("c", false), item("d", false)];
        let now = crate::model::now();
        let tomorrow = day(local(now).date_naive().checked_add_days(Days::new(1)).unwrap());
        let answer = format!(
            "Sure! ```json\n[{{\"id\":1,\"category\":\"People \",\"toRespond\":true,\"until\":\"{tomorrow}\",\"labels\":[\"work\",\"travel plans\",\"a very long label here\",\"x\"]}},\
             {{\"id\":\"2\",\"category\":\"notifications\",\"toRespond\":true,\"until\":null,\"labels\":[\"Work\",\"Work\"]}},\
             {{\"id\":3,\"category\":\"newsletter\",\"toRespond\":true,\"labels\":[1]}},\
             {{\"id\":4,\"category\":\"spam\"}}, 7, {{\"id\":9,\"category\":\"people\"}},\
             {{\"id\":\"1\",\"category\":\"invitation\",\"toRespond\":true,\"until\":\"2001-01-01\"}}]\n```"
        );
        let tags = parse(answer.as_bytes(), &items, now).unwrap();
        // A later answer for the same id wins, as the Swift dictionary does; an until in the
        // past ends the wait.
        let a = &tags["a"];
        assert_eq!((a.category, a.to_respond, a.waiting_until), (Category::Invitation, false, None));
        let b = &tags["b"];
        assert_eq!((b.category, b.to_respond, b.labels.clone()), (Category::Notification, false, vec!["Work".to_string()]));
        let c = &tags["c"];
        assert_eq!((c.category, c.to_respond, c.labels.len()), (Category::Newsletter, false, 0));
        assert!(!tags.contains_key("d"));

        let one = format!("[{{\"id\":1,\"category\":\"people\",\"toRespond\":true,\"until\":\"{tomorrow}\",\"labels\":[\"work\",\"travel plans\",\"a very long label here\",\"x\"]}}]");
        let tag = &parse(one.as_bytes(), &items, now).unwrap()["a"];
        assert_eq!(tag.labels, ["Work", "Travel Plans"]);
        assert!(tag.to_respond);
        let until = tag.waiting_until.unwrap();
        assert!(until > now && until - now <= 2.0 * 86_400.0 + 3_600.0);

        assert_eq!(parse(b"  \n", &items, now).err().unwrap(), "omp returned nothing");
        assert_eq!(parse(b"I can't help with that.", &items, now).err().unwrap(), "unexpected omp output: I can't help with that.");
        assert_eq!(version("omp/12.3.4 darwin-arm64"), Some(vec![12, 3, 4]));
        assert_eq!(title_case("  credit  card "), "Credit Card");
    }

    #[tokio::test]
    async fn runs_omp_and_reports_its_failures() {
        let dir = std::env::temp_dir().join(format!("mailstream-omp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let environment: HashMap<String, String> =
            [("PATH".to_string(), format!("{}:/usr/bin:/bin", dir.display()))].into_iter().collect();
        let tools = Arc::new(Tools::fixed(environment));
        let tagger = Tagger::new(tools.clone(), dir.join("work"));
        let options = |executable: &str| Options { model: String::new(), executable: executable.into() };

        // Two omps on PATH: the newer one wins. It records its arguments and answers.
        let newer = dir.join("newer");
        std::fs::create_dir_all(&newer).unwrap();
        script(&dir.join("omp"), "echo omp/1.2.3; exit 0");
        script(
            &newer.join("omp"),
            &format!(
                "if [ \"$1\" = --version ]; then echo omp/1.10.0; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {}/args\n\
                 echo '[{{\"id\":\"1\",\"category\":\"people\",\"toRespond\":false,\"labels\":[]}}]'",
                dir.display()
            ),
        );
        let tools = Arc::new(Tools::fixed(
            [("PATH".to_string(), format!("{}:{}:/usr/bin:/bin", dir.display(), newer.display()))].into_iter().collect(),
        ));
        let tagger_on_path = Tagger::new(tools, dir.join("work"));
        let tags = tagger_on_path.tag(&[item("k", false)], &options("")).await.unwrap();
        assert_eq!(tags["k"].category, Category::People);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(args.starts_with("-p\n--no-session\n"), "{args}");

        let failing = dir.join("failing");
        script(&failing, "echo 'progress' >&2; echo '  Model smol is not available  ' >&2; echo >&2; exit 3");
        let error = tagger.tag(&[item("k", false)], &options(&failing.display().to_string())).await.err().unwrap();
        assert_eq!(error, "Model smol is not available");
        let silent = dir.join("silent");
        script(&silent, "exit 4");
        let error = tagger.tag(&[item("k", false)], &options(&silent.display().to_string())).await.err().unwrap();
        assert_eq!(error, "omp exited with status 4");
        let error = tagger.tag(&[item("k", false)], &options("/nonexistent/omp")).await.err().unwrap();
        assert_eq!(error, "aiTagging.ompPath /nonexistent/omp is not an executable");

        // A child that overruns its time is stopped.
        let slow = dir.join("slow");
        script(&slow, &format!("echo $$ > {}/slow.pid; exec sleep 30", dir.display()));
        let started = std::time::Instant::now();
        let error = run(&slow, &[], &HashMap::new(), None, false, Duration::from_millis(300)).await.err().unwrap();
        assert_eq!(error, "slow timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(dir.join("slow.pid")).unwrap().trim().parse().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        // SAFETY: probing a pid with signal 0.
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "the timed-out child is gone");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
