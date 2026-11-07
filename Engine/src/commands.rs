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
}

const fn positional(name: &'static str) -> Argument {
    Argument { name, positional: true, required: true }
}

const fn option(name: &'static str) -> Argument {
    Argument { name, positional: false, required: false }
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

/// The ported commands. None of them has a default value yet; `Arguments` fills in defaults
/// once one does.
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
];

/// The Swift engine's other commands, ported in later phases (MS-113 to MS-118).
pub(crate) const UNPORTED: &[&str] = &[
    "status",
    "mailboxes",
    "mail.list",
    "mail.search",
    "mail.show",
    "mail.previews",
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
    "refresh",
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
        assert_eq!(spec("mail.list").err().unwrap().code, "unported");
        assert_eq!(spec("mail.nope").err().unwrap().code, "unknown_command");
    }
}
