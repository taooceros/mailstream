//! Classifying conversations with the user's `omp` CLI, as the Swift engine's `OmpTagger`,
//! `ToolEnvironment` and `ProcessRunner` do it (`Sources/MailstreamKit/Backend/OmpTagger.swift`,
//! `Tools.swift`, `ProcessRunner.swift`): a one-shot, tool-less run of the model the
//! `aiTagging.model` setting names, with the same arguments, prompt and lenient reading of the
//! answer.
//!
//! With `aiTagging.backend` set to `apple`, Apple Intelligence's on-device model answers instead,
//! through `mailstream-apple-tagger` (`Engine/apple/AppleTagger.swift`), a helper the front ends
//! bundle beside their executable: the same prompt in, the same JSON answer out, read the same way.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
use unicode_segmentation::UnicodeSegmentation;

use crate::model::{Date, ProviderCategory, date_from_unix, unix_seconds};
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
    /// How the provider itself sorted that message (Gmail's inbox category, Outlook's Focused
    /// Inbox): a hint the model weighs, never a verdict (MS-336).
    pub provider_category: Option<ProviderCategory>,
    /// Gmail marks that message Important.
    pub important: bool,
}

/// Who answers (`aiTagging.backend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Omp,
    /// Apple Intelligence's on-device model, through `APPLE_TAGGER`.
    Apple,
}

impl Backend {
    /// The backend a valid `aiTagging.backend` value names.
    pub fn named(name: &str) -> Self {
        if name == "apple" { Self::Apple } else { Self::Omp }
    }
}

/// The `aiTagging.*` settings a run uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub backend: Backend,
    /// `--model` value.
    pub model: String,
    /// The omp to run; empty picks the newest on PATH.
    pub executable: String,
}

/// The Apple Intelligence helper's file name, beside the front end's executable.
pub const APPLE_TAGGER: &str = "mailstream-apple-tagger";
/// Its exit status when the model declined a batch (guardrails, a language it lacks). Any other
/// failure, the model being unavailable included, is its last stderr line.
const APPLE_DECLINED: i32 = 4;

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
    /// `mailstream-apple-tagger` (`Config::apple_tagger`).
    apple_tagger: PathBuf,
}

impl Tagger {
    pub fn new(tools: Arc<Tools>, working_directory: PathBuf, apple_tagger: PathBuf) -> Self {
        Self { tools, working_directory, executables: Mutex::new(HashMap::new()), apple_tagger }
    }

    /// Tags keyed by `Item::id`. Items the model skipped or answered invalidly are absent.
    pub async fn tag(&self, items: &[Item], options: &Options) -> Result<HashMap<String, ConversationTag>, String> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let now = crate::model::now();
        match options.backend {
            Backend::Omp => self.tag_with_omp(items, options, now).await,
            Backend::Apple => self.tag_with_apple(items, now).await,
        }
    }

    async fn tag_with_omp(
        &self,
        items: &[Item],
        options: &Options,
        now: Date,
    ) -> Result<HashMap<String, ConversationTag>, String> {
        let executable = self.resolved_executable(&options.executable).await?;
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
            return Err(failure(&output, "omp"));
        }
        parse(&output.stdout, items, now, "omp")
    }

    /// One helper run per batch. A batch the model declines is split in halves until the
    /// conversation it declines stands alone; that one stays untagged rather than holding up the
    /// rest. Any other failure, the model being unavailable included, fails the batch: tagging
    /// never falls back to omp.
    async fn tag_with_apple(&self, items: &[Item], now: Date) -> Result<HashMap<String, ConversationTag>, String> {
        if !is_executable(&self.apple_tagger) {
            return Err(format!("Apple Intelligence tagging is not bundled with this app ({} is missing)", APPLE_TAGGER));
        }
        let arguments = [SYSTEM_PROMPT.to_string(), prompt(items, now)];
        let environment: HashMap<String, String> = std::env::vars().collect();
        let timeout = Duration::from_secs(MAX_TIME + EXIT_MARGIN);
        let output = run(&self.apple_tagger, &arguments, &environment, None, true, timeout).await?;
        match output.status {
            0 => parse(&output.stdout, items, now, "Apple Intelligence"),
            APPLE_DECLINED if items.len() > 1 => {
                let (first, second) = items.split_at(items.len() / 2);
                let mut tags = Box::pin(self.tag_with_apple(first, now)).await?;
                tags.extend(Box::pin(self.tag_with_apple(second, now)).await?);
                Ok(tags)
            }
            APPLE_DECLINED => Ok(HashMap::new()),
            _ => Err(failure(&output, APPLE_TAGGER)),
        }
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

/// A failed run's last stderr line, else its exit status.
fn failure(output: &Output, name: &str) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    match stderr.lines().rev().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.chars().take(160).collect(),
        None => format!("{name} exited with status {}", output.status),
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
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    let Ok(text) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return false };
    // SAFETY: `text` is a NUL-terminated path that outlives the call.
    unsafe { libc::access(text.as_ptr(), libc::X_OK) == 0 }
}

/// Windows runs any file with an executable extension; the candidates carry one.
#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The file names `name` runs as: itself, or on Windows with the extensions `omp` installs with.
fn executable_names(name: &str) -> Vec<String> {
    if cfg!(windows) { ["exe", "cmd", "bat"].map(|extension| format!("{name}.{extension}")).to_vec() } else { vec![name.into()] }
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
by the user); providerCategory (only when the mail provider sorts mail itself: how it filed that \
message, Gmail's personal, social, promotions, updates or forums tab, or Outlook's focused or \
other); important (only when Gmail marked that message important).

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

providerCategory and important are the provider's own guesses: a hint that weighs less than what \
the sender, subject and snippet show. Gmail files much human mail under updates or forums, and \
Outlook's other holds both notifications and newsletters.

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
            if let Some(category) = item.provider_category {
                entry.insert("providerCategory".into(), json!(category.as_str()));
            }
            if item.important {
                entry.insert("important".into(), json!(true));
            }
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

/// `source` names who answered in failures.
fn parse(data: &[u8], items: &[Item], now: Date, source: &str) -> Result<HashMap<String, ConversationTag>, String> {
    let text = String::from_utf8_lossy(data);
    let answers = match (text.find('['), text.rfind(']')) {
        (Some(start), Some(end)) if start < end => serde_json::from_str::<Vec<Value>>(&text[start..=end]).ok(),
        _ => None,
    };
    let Some(answers) = answers else {
        let preview: String = text.trim().chars().take(80).collect();
        return Err(if preview.is_empty() {
            format!("{source} returned nothing")
        } else {
            format!("unexpected {source} output: {preview}")
        });
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
            synced: false,
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
        let names = executable_names(name);
        let mut seen = std::collections::HashSet::new();
        std::env::split_paths(&path)
            .filter(|directory| !directory.as_os_str().is_empty())
            .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
            .filter(|candidate| {
                is_executable(candidate)
                    && seen.insert(std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone()))
            })
            .collect()
    }
}

/// Windows apps get the user's PATH from the system; there is no login shell to ask.
#[cfg(windows)]
async fn recover() -> HashMap<String, String> {
    let mut environment: HashMap<String, String> = std::env::vars().collect();
    environment.insert("NO_COLOR".into(), "1".into());
    environment
}

#[cfg(unix)]
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

#[cfg(unix)]
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
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(2);

/// A child still running when its run ends early (timeout, or the caller dropping the future):
/// it gets SIGTERM, then SIGKILL if it outlives `KILL_GRACE`; on Windows it is terminated.
struct Running(Option<tokio::process::Child>);

impl Drop for Running {
    #[cfg(windows)]
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
        }
    }

    #[cfg(unix)]
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
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        status.code().or_else(|| status.signal()).unwrap_or(-1)
    };
    #[cfg(windows)]
    let status = status.code().unwrap_or(-1);
    Ok(Output { status, stdout, stderr })
}

// The fake omp is a shell script.
#[cfg(all(test, unix))]
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
            provider_category: None,
            important: false,
        }
    }

    #[test]
    fn writes_the_prompt_as_the_swift_engine() {
        let now = date_from_unix(Local.with_ymd_and_hms(2026, 11, 7, 15, 0, 0).unwrap().timestamp() as f64);
        let mut first = item("k1", false);
        first.date = Some(now - 3.0 * 86_400.0);
        first.provider_category = Some(ProviderCategory::Promotions);
        first.important = true;
        let mut second = item("k2", true);
        second.provider_category = Some(ProviderCategory::Focused);
        second.sender_name = "  ".into();
        second.snippet = String::new();
        second.date = None;
        let text = prompt(&[first, second], now);
        assert_eq!(
            text,
            "today: 2026-11-07 (Saturday)\n[\n\
             {\"ageDays\":3,\"count\":2,\"date\":\"2026-11-04\",\"from\":\"Ann Lee <ann@x.com>\",\"id\":\"1\",\"important\":true,\"lastFromMe\":false,\"providerCategory\":\"promotions\",\"replied\":false,\"routineUntil\":\"2026-11-18\",\"snippet\":\"Are you free / on Friday\",\"subject\":\"Lunch?\",\"toMe\":true},\n\
             {\"count\":2,\"from\":\"ann@x.com\",\"id\":\"2\",\"lastFromMe\":true,\"providerCategory\":\"focused\",\"replied\":false,\"subject\":\"Lunch?\",\"toMe\":true}\n]"
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
        let tags = parse(answer.as_bytes(), &items, now, "omp").unwrap();
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
        let tag = &parse(one.as_bytes(), &items, now, "omp").unwrap()["a"];
        assert_eq!(tag.labels, ["Work", "Travel Plans"]);
        assert!(tag.to_respond);
        let until = tag.waiting_until.unwrap();
        assert!(until > now && until - now <= 2.0 * 86_400.0 + 3_600.0);

        assert_eq!(parse(b"  \n", &items, now, "omp").err().unwrap(), "omp returned nothing");
        assert_eq!(
            parse(b"I can't help with that.", &items, now, "omp").err().unwrap(),
            "unexpected omp output: I can't help with that."
        );
        assert_eq!(version("omp/12.3.4 darwin-arm64"), Some(vec![12, 3, 4]));
        assert_eq!(title_case("  credit  card "), "Credit Card");
    }

    #[tokio::test]
    async fn runs_omp_and_reports_its_failures() {
        let dir = std::env::temp_dir().join(format!("mailstream-omp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let environment: HashMap<String, String> =
            [("PATH".to_string(), format!("{}:{}", dir.display(), crate::testkit::tools_path()))].into_iter().collect();
        let tools = Arc::new(Tools::fixed(environment));
        let tagger = Tagger::new(tools.clone(), dir.join("work"), dir.join("no-apple-tagger"));
        let options =
            |executable: &str| Options { backend: Backend::Omp, model: String::new(), executable: executable.into() };

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
            [("PATH".to_string(), format!("{}:{}:{}", dir.display(), newer.display(), crate::testkit::tools_path()))].into_iter().collect(),
        ));
        let tagger_on_path = Tagger::new(tools, dir.join("work"), dir.join("no-apple-tagger"));
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
        let tools_only: HashMap<String, String> = [("PATH".to_string(), crate::testkit::tools_path())].into_iter().collect();
        let error = run(&slow, &[], &tools_only, None, false, Duration::from_millis(300)).await.err().unwrap();
        assert_eq!(error, "slow timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(dir.join("slow.pid")).unwrap().trim().parse().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        // SAFETY: probing a pid with signal 0.
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "the timed-out child is gone");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn runs_apple_intelligence_without_falling_back_to_omp() {
        let dir = std::env::temp_dir().join(format!("mailstream-apple-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // An omp on PATH that must never run.
        script(&dir.join("omp"), &format!("touch {}/omp-ran; exit 1", dir.display()));
        let tools = Arc::new(Tools::fixed(
            [("PATH".to_string(), format!("{}:{}", dir.display(), crate::testkit::tools_path()))].into_iter().collect(),
        ));
        let helper = dir.join(APPLE_TAGGER);
        let tagger = Tagger::new(tools, dir.join("work"), helper.clone());
        let options = Options { backend: Backend::Apple, model: "smol".into(), executable: String::new() };
        let mut spam = item("b", false);
        spam.subject = "Spam".into();
        let items = [item("a", false), spam, item("c", false), item("d", false)];

        let error = tagger.tag(&items, &options).await.err().unwrap();
        assert_eq!(error, "Apple Intelligence tagging is not bundled with this app (mailstream-apple-tagger is missing)");

        // The fake helper records each run, declines any batch holding the spam and otherwise
        // calls every conversation people.
        script(
            &helper,
            &format!(
                "printf '%s\\n' \"$1\" | head -1 >> {dir}/instructions\n\
                 echo run >> {dir}/runs\n\
                 case \"$2\" in *Spam*) echo 'Apple Intelligence declined to answer' >&2; exit 4;; esac\n\
                 n=$(printf '%s\\n' \"$2\" | grep -c '\"id\"')\n\
                 out='['; i=1\n\
                 while [ $i -le $n ]; do [ $i -gt 1 ] && out=\"$out,\"; out=\"$out{{\\\"id\\\":\\\"$i\\\",\\\"category\\\":\\\"people\\\",\\\"toRespond\\\":false,\\\"labels\\\":[]}}\"; i=$((i+1)); done\n\
                 echo \"$out]\"",
                dir = dir.display()
            ),
        );
        let tags = tagger.tag(&items, &options).await.unwrap();
        let mut tagged: Vec<&str> = tags.keys().map(String::as_str).collect();
        tagged.sort();
        assert_eq!(tagged, ["a", "c", "d"], "the declined conversation alone stays untagged");
        // All four, then halves [a, spam] and [c, d], then a and the spam alone.
        assert_eq!(std::fs::read_to_string(dir.join("runs")).unwrap().lines().count(), 5);
        let instructions = std::fs::read_to_string(dir.join("instructions")).unwrap();
        assert!(instructions.lines().all(|line| SYSTEM_PROMPT.starts_with(line)), "{instructions}");

        // Unavailable: its message is the failure, and omp does not stand in.
        script(&helper, "echo 'Apple Intelligence is off; turn it on in System Settings > Apple Intelligence & Siri' >&2; exit 3");
        let error = tagger.tag(&items, &options).await.err().unwrap();
        assert_eq!(error, "Apple Intelligence is off; turn it on in System Settings > Apple Intelligence & Siri");
        script(&helper, "echo 'Hello' ; exit 0");
        assert_eq!(tagger.tag(&items, &options).await.err().unwrap(), "unexpected Apple Intelligence output: Hello");
        assert!(!dir.join("omp-ran").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
