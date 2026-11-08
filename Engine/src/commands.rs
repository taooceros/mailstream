//! The command table and argument checks, as the Swift engine's `CommandSpec` and `Arguments`
//! (`Sources/MailstreamKit/Control/Commands.swift`, `ControlService.swift`) define them for the
//! commands ported so far. Commands the Swift engine has and this one does not yet answer
//! `unported`.
use serde_json::{Map, Value};

use crate::EngineError;

/// One argument of a command.
pub(crate) struct Argument {
    pub name: &'static str,
    /// Given by position rather than `--name` on the command line; only changes how errors name it.
    pub positional: bool,
    pub required: bool,
    /// Filled in when the request leaves the argument out.
    pub default: Option<i64>,
}

const fn positional(name: &'static str) -> Argument {
    Argument { name, positional: true, required: true, default: None }
}

const fn option(name: &'static str) -> Argument {
    Argument { name, positional: false, required: false, default: None }
}

const fn number(name: &'static str, default: i64) -> Argument {
    Argument { name, positional: false, required: false, default: Some(default) }
}

pub(crate) struct Spec {
    pub id: &'static str,
    pub arguments: &'static [Argument],
}

impl Spec {
    /// Space-separated words, as messages name the command: `config set`.
    fn name(&self) -> String {
        self.id.replace('.', " ")
    }
}

/// The ported commands, with the arguments and defaults of the Swift engine's `CommandSpec`.
pub(crate) const PORTED: &[Spec] = &[
    Spec { id: "config.list", arguments: &[] },
    Spec { id: "config.get", arguments: &[positional("key")] },
    Spec { id: "config.set", arguments: &[positional("key"), positional("value")] },
    Spec { id: "config.reset", arguments: &[positional("key")] },
    Spec { id: "config.add", arguments: &[positional("key"), positional("values")] },
    Spec { id: "config.remove", arguments: &[positional("key"), positional("values")] },
    Spec { id: "config.schema", arguments: &[] },
    Spec { id: "config.path", arguments: &[] },
    Spec { id: "accounts.list", arguments: &[] },
    Spec { id: "accounts.rename", arguments: &[positional("account"), positional("label")] },
    Spec { id: "accounts.client", arguments: &[positional("provider")] },
    Spec { id: "accounts.add", arguments: &[positional("provider"), option("client-id"), option("client-secret")] },
    Spec { id: "accounts.remove", arguments: &[positional("account"), option("yes")] },
    Spec { id: "mailboxes", arguments: &[option("account")] },
    Spec {
        id: "mail.list",
        arguments: &[
            option("mailbox"),
            option("account"),
            option("unread"),
            option("starred"),
            option("category"),
            option("waiting"),
            number("limit", 25),
            option("cursor"),
            option("cached"),
        ],
    },
    Spec { id: "mail.search", arguments: &[positional("query"), option("account"), number("limit", 25)] },
    Spec { id: "mail.show", arguments: &[positional("id"), number("max-chars", 20_000), option("html")] },
    Spec { id: "mail.previews", arguments: &[positional("ids")] },
    Spec { id: "refresh", arguments: &[] },
];

/// The Swift engine's other commands, ported in later phases (MS-114 to MS-118).
pub(crate) const UNPORTED: &[&str] = &[
    "status",
    "mail.avatars",
    "mail.read",
    "mail.star",
    "mail.archive",
    "mail.trash",
    "mail.restore",
    "mail.tag",
    "mail.retag",
    "mail.send",
    "mail.reply",
    "contacts.search",
    "chat.spaces",
    "chat.more",
    "chat.refresh",
    "chat.open",
    "chat.close",
    "chat.messages",
    "chat.earlier",
    "chat.send",
    "chat.people",
    "chat.start",
    "chat.attachment",
];

/// The spec of a ported `command`, or the error the request answers.
pub(crate) fn spec(command: &str) -> Result<&'static Spec, EngineError> {
    if let Some(spec) = PORTED.iter().find(|spec| spec.id == command) {
        return Ok(spec);
    }
    if UNPORTED.contains(&command) {
        return Err(EngineError::new(
            "unported",
            format!("{command} is not in the Rust engine yet (MAILSTREAM_ENGINE=rust)."),
        ));
    }
    Err(EngineError::new(
        "unknown_command",
        format!("Unknown command \"{command}\"; `mailstream commands` lists them."),
    ))
}

/// A request's arguments, checked against the command's spec. A `null` value counts as absent.
pub(crate) struct Arguments {
    values: Map<String, Value>,
}

impl Arguments {
    pub fn new(spec: &Spec, values: &Map<String, Value>) -> Result<Self, EngineError> {
        let mut out = Map::new();
        for argument in spec.arguments {
            match values.get(argument.name) {
                Some(value) if !value.is_null() => {
                    out.insert(argument.name.into(), value.clone());
                }
                _ if argument.default.is_some() => {
                    out.insert(argument.name.into(), Value::from(argument.default.unwrap_or_default()));
                }
                _ if argument.required => {
                    let label = if argument.positional {
                        format!("<{}>", argument.name)
                    } else {
                        format!("--{}", argument.name)
                    };
                    return Err(EngineError::usage(format!("{}: {label} is required.", spec.name())));
                }
                _ => {}
            }
        }
        if let Some(unknown) = values.keys().find(|key| !spec.arguments.iter().any(|argument| argument.name == *key)) {
            return Err(EngineError::usage(format!("{} has no argument \"{unknown}\".", spec.name())));
        }
        Ok(Self { values: out })
    }

    /// Text, numbers and booleans as text.
    pub fn string(&self, name: &str) -> Option<String> {
        match self.values.get(name)? {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => value.as_i64().map(|value| value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        }
    }

    pub fn required(&self, name: &str) -> Result<String, EngineError> {
        self.string(name).ok_or_else(|| EngineError::usage(format!("{name} is required.")))
    }

    pub fn bool(&self, name: &str) -> bool {
        self.values.get(name).and_then(Value::as_bool).unwrap_or_else(|| self.string(name).as_deref() == Some("true"))
    }

    /// A number, or text holding one.
    pub fn int(&self, name: &str) -> Option<i64> {
        let value = self.values.get(name)?;
        value
            .as_i64()
            .or_else(|| value.as_f64().filter(|number| number.fract() == 0.0 && number.abs() < 1e15).map(|number| number as i64))
            .or_else(|| self.string(name).and_then(|text| text.parse().ok()))
    }

    /// An array's strings, or comma-separated text (trimmed, empty values dropped).
    pub fn list(&self, name: &str) -> Option<Vec<String>> {
        match self.values.get(name)? {
            Value::Array(items) => Some(items.iter().filter_map(|item| item.as_str().map(String::from)).collect()),
            Value::String(text) => {
                Some(text.split(',').map(str::trim).filter(|value| !value.is_empty()).map(String::from).collect())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(command: &str, values: Value) -> Result<Arguments, EngineError> {
        Arguments::new(spec(command).unwrap(), values.as_object().unwrap())
    }

    #[test]
    fn checks_arguments_like_the_swift_engine() {
        let error = args("config.set", json!({ "key": "sync.pollSeconds" })).err().unwrap();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("usage", "config set: <value> is required."));
        let error = args("accounts.add", json!({ "provider": "google", "nope": 1 })).err().unwrap();
        assert_eq!(error.message, "accounts add has no argument \"nope\".");
        let given = args("accounts.add", json!({ "provider": "google", "client-id": null })).unwrap();
        assert_eq!(given.string("client-id"), None);
        assert!(args("accounts.remove", json!({ "account": "a@b.c", "yes": "true" })).unwrap().bool("yes"));
        assert_eq!(spec("mail.read").err().unwrap().code, "unported");
        let defaults = args("mail.list", json!({ "mailbox": "inbox, sent" })).unwrap();
        assert_eq!((defaults.int("limit"), defaults.list("mailbox")), (Some(25), Some(vec!["inbox".into(), "sent".into()])));
        assert_eq!(args("mail.show", json!({ "id": "x", "max-chars": "40" })).unwrap().int("max-chars"), Some(40));
        assert_eq!(spec("mail.nope").err().unwrap().code, "unknown_command");
    }
}
