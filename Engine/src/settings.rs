//! settings.json and the `config.*` commands, as the Swift engine has them
//! (`Sources/MailstreamKit/Control/Settings.swift`). The file holds only the settings changed
//! from their default, as a flat object of dotted keys; it is followed as it changes, whoever
//! edits it (a front end, `mailstream config`, an agent or an editor).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::EngineError;
use crate::files;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Setting {
    AiTaggingEnabled,
    AiTaggingBackend,
    AiTaggingModel,
    AiTaggingOmpPath,
    AiTaggingBatchSize,
    AiTaggingPassLimit,
    AiTaggingBackoffSeconds,
    TrustedSenders,
    PollSeconds,
    PushPollSeconds,
    ChatPollSeconds,
    FlutterShowFrameRate,
}

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int(i64, i64),
    Text,
    /// One of the given words.
    Choice(&'static [&'static str]),
    /// Lowercased email addresses, kept sorted and unique.
    Addresses,
}

/// The valid values settings.json sets, by setting.
pub type Values = BTreeMap<Setting, Value>;

impl Setting {
    pub const ALL: [Setting; 12] = [
        Self::AiTaggingEnabled,
        Self::AiTaggingBackend,
        Self::AiTaggingModel,
        Self::AiTaggingOmpPath,
        Self::AiTaggingBatchSize,
        Self::AiTaggingPassLimit,
        Self::AiTaggingBackoffSeconds,
        Self::TrustedSenders,
        Self::PollSeconds,
        Self::PushPollSeconds,
        Self::ChatPollSeconds,
        Self::FlutterShowFrameRate,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::AiTaggingEnabled => "aiTagging.enabled",
            Self::AiTaggingBackend => "aiTagging.backend",
            Self::AiTaggingModel => "aiTagging.model",
            Self::AiTaggingOmpPath => "aiTagging.ompPath",
            Self::AiTaggingBatchSize => "aiTagging.batchSize",
            Self::AiTaggingPassLimit => "aiTagging.passLimit",
            Self::AiTaggingBackoffSeconds => "aiTagging.backoffSeconds",
            Self::TrustedSenders => "remoteContent.trustedSenders",
            Self::PollSeconds => "sync.pollSeconds",
            Self::PushPollSeconds => "sync.pushPollSeconds",
            Self::ChatPollSeconds => "chat.pollSeconds",
            Self::FlutterShowFrameRate => "flutter.showFrameRate",
        }
    }

    /// What the key holds, its default and its description: the one place a setting is described.
    fn descriptor(self) -> (Kind, Value, &'static str) {
        match self {
            Self::AiTaggingEnabled => (
                Kind::Bool,
                json!(true),
                "Sort the inbox into People, Notifications, Newsletters and Invitations with AI, and flag mail waiting on your reply or action.",
            ),
            Self::AiTaggingBackend => (
                Kind::Choice(&["omp", "apple"]),
                json!("omp"),
                "Who tags: omp runs the model aiTagging.model names; apple runs Apple Intelligence's on-device model (macOS 26 or later, Apple Intelligence on).",
            ),
            Self::AiTaggingModel => {
                (Kind::Text, json!("smol"), "omp model role or model id the tagger runs (`omp --model`).")
            }
            Self::AiTaggingOmpPath => (
                Kind::Text,
                json!(""),
                "Path of the omp executable; empty picks the newest omp on the login shell's PATH.",
            ),
            Self::AiTaggingBatchSize => (Kind::Int(1, 100), json!(20), "Conversations sent to the model per run."),
            Self::AiTaggingPassLimit => {
                (Kind::Int(1, 2000), json!(150), "Most conversations one tagging pass looks at, newest first.")
            }
            Self::AiTaggingBackoffSeconds => (
                Kind::Int(0, 86_400),
                json!(300),
                "Wait after a failed tagging run before tagging automatically again.",
            ),
            Self::TrustedSenders => (
                Kind::Addresses,
                json!([]),
                "Senders whose remote images and fonts always load, besides your own addresses and people you wrote to.",
            ),
            Self::PollSeconds => (Kind::Int(15, 3600), json!(90), "Mail polling interval while push is not connected."),
            Self::PushPollSeconds => {
                (Kind::Int(60, 86_400), json!(900), "Safety-net polling interval while push is connected.")
            }
            Self::ChatPollSeconds => (Kind::Int(5, 3600), json!(20), "Google Chat polling interval."),
            Self::FlutterShowFrameRate => (
                Kind::Bool,
                json!(false),
                "Show the frame rate and the slowest recent frame in a corner of Mailstream Flutter's window (also View ▸ Show Frame Rate).",
            ),
        }
    }

    fn kind(self) -> Kind {
        self.descriptor().0
    }

    pub fn default_value(self) -> Value {
        self.descriptor().1
    }

    pub fn named(key: &str) -> Option<Setting> {
        Self::ALL.into_iter().find(|setting| setting.key() == key)
    }

    /// The setting, or a message naming close matches.
    fn find(key: &str) -> Result<Setting, String> {
        Self::named(key).ok_or_else(|| {
            let lowered = key.to_lowercase();
            let close: Vec<_> = Self::ALL
                .iter()
                .map(|setting| setting.key())
                .filter(|name| name.to_lowercase().contains(&lowered))
                .collect();
            let hint = if close.is_empty() { String::new() } else { format!(" Did you mean {}?", close.join(", ")) };
            format!("Unknown setting \"{key}\".{hint} `mailstream config list` shows them all.")
        })
    }

    /// The value in effect given the file's valid `values`: the default when the key is absent.
    pub fn value(self, values: &Values) -> Value {
        values.get(&self).cloned().unwrap_or_else(|| self.default_value())
    }

    /// `value` checked against the kind and normalized.
    pub fn validate(self, value: &Value) -> Result<Value, EngineError> {
        let key = self.key();
        match self.kind() {
            Kind::Bool => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| EngineError::invalid_setting(format!("{key} must be true or false."))),
            Kind::Int(low, high) => {
                let int =
                    integer(value).ok_or_else(|| EngineError::invalid_setting(format!("{key} must be an integer.")))?;
                if !(low..=high).contains(&int) {
                    return Err(EngineError::invalid_setting(format!("{key} must be between {low} and {high}.")));
                }
                Ok(json!(int))
            }
            Kind::Text => value
                .as_str()
                .map(|text| json!(text.trim()))
                .ok_or_else(|| EngineError::invalid_setting(format!("{key} must be a string."))),
            Kind::Choice(words) => value
                .as_str()
                .map(|text| text.trim().to_lowercase())
                .filter(|word| words.contains(&word.as_str()))
                .map(Value::String)
                .ok_or_else(|| EngineError::invalid_setting(format!("{key} must be one of {}.", words.join(", ")))),
            Kind::Addresses => {
                let strings: Option<Vec<&str>> =
                    value.as_array().and_then(|items| items.iter().map(Value::as_str).collect());
                let strings = strings.ok_or_else(|| {
                    EngineError::invalid_setting(format!("{key} must be an array of email addresses."))
                })?;
                let mut addresses: Vec<String> = strings.iter().map(|text| text.trim().to_lowercase()).collect();
                if let Some(bad) =
                    addresses.iter().find(|address| !address.contains('@') || address.contains(char::is_whitespace))
                {
                    return Err(EngineError::invalid_setting(format!("{key}: \"{bad}\" is not an email address.")));
                }
                addresses.sort();
                addresses.dedup();
                Ok(json!(addresses))
            }
        }
    }

    /// A value typed on a command line: `true`/`off`, `120`, text, or for addresses a JSON array
    /// or comma-separated list.
    pub fn parse(self, text: &str) -> Result<Value, EngineError> {
        let trimmed = text.trim();
        let key = self.key();
        match self.kind() {
            Kind::Bool => typed_bool(trimmed)
                .map(Value::Bool)
                .ok_or_else(|| EngineError::invalid_setting(format!("{key} must be true or false, not \"{text}\"."))),
            Kind::Int(..) => {
                let int: i64 = trimmed
                    .parse()
                    .map_err(|_| EngineError::invalid_setting(format!("{key} must be an integer, not \"{text}\".")))?;
                self.validate(&json!(int))
            }
            Kind::Text | Kind::Choice(_) => self.validate(&json!(text)),
            Kind::Addresses => {
                if trimmed.starts_with('[')
                    && let Ok(parsed) = serde_json::from_str::<Value>(trimmed)
                {
                    return self.validate(&parsed);
                }
                let parts: Vec<&str> = trimmed.split(',').filter(|part| !part.trim().is_empty()).collect();
                self.validate(&json!(parts))
            }
        }
    }

    /// JSON Schema of this setting.
    fn schema(self) -> Value {
        let (kind, default, summary) = self.descriptor();
        let mut out = Map::new();
        out.insert("description".into(), json!(summary));
        out.insert("default".into(), default);
        match kind {
            Kind::Bool => {
                out.insert("type".into(), json!("boolean"));
            }
            Kind::Int(low, high) => {
                out.insert("type".into(), json!("integer"));
                out.insert("minimum".into(), json!(low));
                out.insert("maximum".into(), json!(high));
            }
            Kind::Text => {
                out.insert("type".into(), json!("string"));
            }
            Kind::Choice(words) => {
                out.insert("type".into(), json!("string"));
                out.insert("enum".into(), json!(words));
            }
            Kind::Addresses => {
                out.insert("type".into(), json!("array"));
                out.insert("items".into(), json!({ "type": "string", "format": "email" }));
                out.insert("uniqueItems".into(), json!(true));
            }
        }
        Value::Object(out)
    }

    /// JSON Schema of the whole file.
    pub fn file_schema() -> Value {
        let properties: Map<String, Value> =
            Self::ALL.iter().map(|setting| (setting.key().to_string(), setting.schema())).collect();
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Mailstream settings",
            "description": "~/Library/Application Support/Mailstream/settings.json. Only settings changed from their default need to be present; the app applies edits immediately.",
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
        })
    }

    /// The setting as `config` commands answer it, given the file's valid `values`.
    fn entry(self, values: &Values) -> Value {
        let (kind, default, summary) = self.descriptor();
        let mut out = Map::new();
        out.insert("key".into(), json!(self.key()));
        out.insert("value".into(), self.value(values));
        out.insert("default".into(), default);
        out.insert("changed".into(), json!(values.contains_key(&self)));
        out.insert("summary".into(), json!(summary));
        match kind {
            Kind::Bool => {
                out.insert("type".into(), json!("boolean"));
            }
            Kind::Int(low, high) => {
                out.insert("type".into(), json!("integer"));
                out.insert("minimum".into(), json!(low));
                out.insert("maximum".into(), json!(high));
            }
            Kind::Text => {
                out.insert("type".into(), json!("string"));
            }
            Kind::Choice(words) => {
                out.insert("type".into(), json!("string"));
                out.insert("choices".into(), json!(words));
            }
            Kind::Addresses => {
                out.insert("type".into(), json!("array of email addresses"));
            }
        }
        Value::Object(out)
    }
}

/// An integer, or a double that holds one exactly (as the Swift engine's `JSON.int`).
fn integer(value: &Value) -> Option<i64> {
    if let Some(int) = value.as_i64() {
        return Some(int);
    }
    let double = value.as_f64()?;
    (double.round() == double && double.abs() < 1e15).then_some(double as i64)
}

/// `true`/`yes`/`on`/`1` or their opposites, any case.
pub fn typed_bool(word: &str) -> Option<bool> {
    match word.to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Reading and writing settings.json.
pub struct SettingsFile {
    directory: PathBuf,
    path: PathBuf,
}

impl SettingsFile {
    pub fn new(directory: &Path) -> Self {
        Self { directory: directory.to_path_buf(), path: directory.join("settings.json") }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's object as written; empty when there is no file.
    pub fn read_raw(&self) -> Result<Map<String, Value>, EngineError> {
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(EngineError::failed(error.to_string())),
        };
        if data.iter().all(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t')) {
            return Ok(Map::new());
        }
        let path = self.path.display();
        match serde_json::from_slice::<Value>(&data) {
            Ok(Value::Object(object)) => Ok(object),
            Ok(_) => Err(EngineError::invalid_setting(format!("{path} must hold a JSON object."))),
            Err(error) => Err(EngineError::invalid_setting(format!("{path} is not valid JSON: {error}"))),
        }
    }

    /// Valid values by setting, plus a message for each key that is unknown or invalid (those
    /// fall back to their default).
    pub fn read(&self) -> (Values, Vec<String>) {
        let raw = match self.read_raw() {
            Ok(raw) => raw,
            Err(error) => return (Values::new(), vec![error.message]),
        };
        let mut values = Values::new();
        let mut issues = Vec::new();
        for (key, value) in &raw {
            let Some(setting) = Setting::named(key) else {
                issues.push(format!("Unknown setting \"{key}\" is ignored."));
                continue;
            };
            match setting.validate(value) {
                Ok(valid) => {
                    values.insert(setting, valid);
                }
                Err(error) => issues.push(format!("{} The default applies.", error.message)),
            }
        }
        (values, issues)
    }

    /// Sets (or with `None`, removes) one key, keeping everything else in the file as it was.
    /// Writing a default value removes the key, so the file lists only real changes.
    pub fn update(&self, setting: Setting, value: Option<&Value>) -> Result<(), EngineError> {
        let mut raw = self.read_raw()?;
        let normalized = value.map(|value| setting.validate(value)).transpose()?;
        match normalized {
            Some(normalized) if normalized != setting.default_value() => {
                raw.insert(setting.key().into(), normalized);
            }
            _ => {
                raw.remove(setting.key());
            }
        }
        let mut data = crate::json::encode(&raw, true, false);
        data.push(b'\n');
        files::replace(&self.directory, &self.path, &data).map_err(|error| EngineError::failed(error.to_string()))
    }
}

/// The settings in effect, following settings.json as it changes.
pub struct Settings {
    file: SettingsFile,
    state: Mutex<(Values, Vec<String>)>,
}

impl Settings {
    pub fn new(directory: &Path) -> Self {
        let file = SettingsFile::new(directory);
        let state = Mutex::new(file.read());
        Self { file, state }
    }

    pub fn file(&self) -> &SettingsFile {
        &self.file
    }

    /// Problems with the file as last read: unknown keys, invalid values, broken JSON.
    pub fn issues(&self) -> Vec<String> {
        self.state.lock().expect("settings lock").1.clone()
    }

    /// `setting`'s value in effect: the file's, else the default.
    pub fn value(&self, setting: Setting) -> Value {
        setting.value(&self.state.lock().expect("settings lock").0)
    }

    /// Reads the file again; true when the values or the issues changed.
    pub fn reload(&self) -> bool {
        let next = self.file.read();
        let mut state = self.state.lock().expect("settings lock");
        if *state == next {
            return false;
        }
        *state = next;
        true
    }

    /// A `config.*` command. Its arguments are read as the request gave them, like the Swift
    /// engine's `CommandID.Local.run`.
    pub fn run(&self, command: &str, args: &Map<String, Value>) -> Result<Value, EngineError> {
        let setting =
            || Setting::find(args.get("key").and_then(Value::as_str).unwrap_or("")).map_err(EngineError::usage);
        let file = &self.file;
        Ok(match command {
            "config.list" => {
                let (values, issues) = file.read();
                json!({
                    "file": file.path.display().to_string(),
                    "settings": Setting::ALL.iter().map(|setting| setting.entry(&values)).collect::<Vec<_>>(),
                    "issues": issues,
                })
            }
            "config.get" => setting()?.entry(&file.read().0),
            "config.set" => {
                let setting = setting()?;
                let value = setting.parse(args.get("value").and_then(Value::as_str).unwrap_or(""))?;
                file.update(setting, Some(&value))?;
                setting.entry(&file.read().0)
            }
            "config.reset" => {
                let setting = setting()?;
                file.update(setting, None)?;
                setting.entry(&file.read().0)
            }
            "config.add" | "config.remove" => {
                let setting = setting()?;
                if !matches!(setting.kind(), Kind::Addresses) {
                    return Err(EngineError::usage(format!("{} is not a list setting.", setting.key())));
                }
                let words: Vec<&str> = args
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .flat_map(|word| word.split(',').filter(|part| !part.is_empty()))
                    .collect();
                let given = strings(&setting.validate(&json!(words))?);
                let current = strings(&setting.value(&file.read().0));
                let next: Vec<String> = if command == "config.add" {
                    current.into_iter().chain(given).collect()
                } else {
                    current.into_iter().filter(|address| !given.contains(address)).collect()
                };
                file.update(setting, Some(&json!(next)))?;
                setting.entry(&file.read().0)
            }
            "config.schema" => Setting::file_schema(),
            "config.path" => json!({ "path": file.path.display().to_string(), "exists": file.path.exists() }),
            _ => unreachable!("{command} is a config command"),
        })
    }
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mailstream-settings-{}-{}", std::process::id(), crate::tests::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(settings: &Settings, command: &str, args: Value) -> Result<Value, EngineError> {
        settings.run(command, args.as_object().unwrap())
    }

    #[test]
    fn validates_and_normalizes_like_the_swift_engine() {
        let poll = Setting::PollSeconds;
        assert_eq!(poll.validate(&json!(120.0)).unwrap(), json!(120));
        assert_eq!(poll.validate(&json!(5)).err().unwrap().message, "sync.pollSeconds must be between 15 and 3600.");
        assert_eq!(poll.validate(&json!("x")).err().unwrap().message, "sync.pollSeconds must be an integer.");
        assert_eq!(poll.parse(" abc ").err().unwrap().message, "sync.pollSeconds must be an integer, not \" abc \".");
        assert_eq!(Setting::AiTaggingEnabled.parse("Off").unwrap(), json!(false));
        assert_eq!(
            Setting::AiTaggingEnabled.parse("maybe").err().unwrap().message,
            "aiTagging.enabled must be true or false, not \"maybe\"."
        );
        assert_eq!(Setting::AiTaggingModel.parse("  big \n").unwrap(), json!("big"));
        assert_eq!(Setting::AiTaggingBackend.parse(" Apple ").unwrap(), json!("apple"));
        assert_eq!(
            Setting::AiTaggingBackend.validate(&json!("gpt")).err().unwrap().message,
            "aiTagging.backend must be one of omp, apple."
        );
        let senders = Setting::TrustedSenders;
        assert_eq!(senders.parse("B@x.com, a@x.com,,b@X.com").unwrap(), json!(["a@x.com", "b@x.com"]));
        assert_eq!(senders.parse(r#"["Z@y.org"]"#).unwrap(), json!(["z@y.org"]));
        let error = senders.parse("a@x.com, nope").err().unwrap();
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            ("invalid_setting", "remoteContent.trustedSenders: \"nope\" is not an email address.")
        );
        assert_eq!(
            senders.validate(&json!(["a@b.c", 1])).err().unwrap().message,
            "remoteContent.trustedSenders must be an array of email addresses."
        );
        let error = Setting::find("poll").err().unwrap();
        assert_eq!(
            error,
            "Unknown setting \"poll\". Did you mean sync.pollSeconds, sync.pushPollSeconds, chat.pollSeconds? `mailstream config list` shows them all."
        );
    }

    #[test]
    fn writes_only_changes_and_keeps_unknown_keys() {
        let dir = temp();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{\"other\": [1, 2], \"sync.pollSeconds\": 7}").unwrap();
        let settings = Settings::new(&dir);
        assert_eq!(
            settings.issues(),
            vec![
                "Unknown setting \"other\" is ignored.".to_string(),
                "sync.pollSeconds must be between 15 and 3600. The default applies.".to_string(),
            ]
        );
        let entry = run(&settings, "config.set", json!({ "key": "sync.pollSeconds", "value": "120" })).unwrap();
        assert_eq!((entry["value"].clone(), entry["changed"].clone()), (json!(120), json!(true)));
        run(&settings, "config.add", json!({ "key": "remoteContent.trustedSenders", "values": ["B@x.com,a@x.com"] }))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"other\" : [\n    1,\n    2\n  ],\n  \"remoteContent.trustedSenders\" : [\n    \"a@x.com\",\n    \"b@x.com\"\n  ],\n  \"sync.pollSeconds\" : 120\n}\n"
        );
        // A default value deletes the key, as does reset.
        run(&settings, "config.set", json!({ "key": "sync.pollSeconds", "value": "90" })).unwrap();
        run(
            &settings,
            "config.remove",
            json!({ "key": "remoteContent.trustedSenders", "values": ["a@x.com", "b@x.com"] }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n  \"other\" : [\n    1,\n    2\n  ]\n}\n");
        assert!(settings.reload());
        assert_eq!(settings.issues(), vec!["Unknown setting \"other\" is ignored.".to_string()]);
        let error =
            run(&settings, "config.add", json!({ "key": "sync.pollSeconds", "values": ["a@b.c"] })).err().unwrap();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("usage", "sync.pollSeconds is not a list setting."));
        std::fs::write(&path, "{ broken").unwrap();
        let error = run(&settings, "config.reset", json!({ "key": "sync.pollSeconds" })).err().unwrap();
        assert_eq!(error.code, "invalid_setting");
        assert!(settings.reload());
        assert!(settings.issues()[0].contains("is not valid JSON"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lists_every_setting_and_the_schema() {
        let dir = temp();
        let settings = Settings::new(&dir);
        let list = run(&settings, "config.list", json!({})).unwrap();
        assert_eq!(list["settings"].as_array().unwrap().len(), 12);
        assert_eq!(
            list["settings"][1],
            json!({
                "key": "aiTagging.backend", "value": "omp", "default": "omp", "changed": false,
                "summary": Setting::AiTaggingBackend.descriptor().2, "type": "string", "choices": ["omp", "apple"],
            })
        );
        assert_eq!(
            list["settings"][4],
            json!({
                "key": "aiTagging.batchSize", "value": 20, "default": 20, "changed": false,
                "summary": "Conversations sent to the model per run.", "type": "integer", "minimum": 1, "maximum": 100,
            })
        );
        assert_eq!(run(&settings, "config.path", json!({})).unwrap()["exists"], json!(false));
        let schema = run(&settings, "config.schema", json!({})).unwrap();
        assert_eq!(schema["properties"]["remoteContent.trustedSenders"]["items"]["format"], json!("email"));
        assert_eq!(schema["properties"]["aiTagging.backend"]["enum"], json!(["omp", "apple"]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
