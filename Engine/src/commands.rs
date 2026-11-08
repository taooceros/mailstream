//! The command table and argument checks: the one table both the `mailstream` command line
//! (parsing, help, `mailstream commands`) and the engine (the control socket and FFI requests)
//! read, so they cannot drift. It matches the Swift engine's `CommandSpec` and `Arguments`
//! (`Sources/MailstreamKit/Control/Commands.swift`, `ControlService.swift`) command for command,
//! which Mailstream.app keeps until it moves to this engine (MS-120).
use serde_json::{Map, Value, json};

use crate::EngineError;

/// What an argument holds, as the command line reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    String,
    Int,
    Bool,
    /// Repeatable, or comma-separated.
    List,
    Choice(&'static [&'static str]),
}

/// One argument of a command.
#[derive(Clone, Copy, Debug)]
pub struct Argument {
    pub name: &'static str,
    pub kind: Kind,
    pub summary: &'static str,
    /// Given by position rather than `--name` on the command line.
    pub positional: bool,
    /// A positional that takes every remaining word.
    pub variadic: bool,
    pub required: bool,
    /// Filled in when the request leaves the argument out.
    pub default: Option<i64>,
}

impl Argument {
    const fn new(name: &'static str, kind: Kind, summary: &'static str) -> Self {
        Self { name, kind, summary, positional: false, variadic: false, required: false, default: None }
    }

    /// Given by position; every positional is required.
    const fn positional(self) -> Self {
        Self { positional: true, required: true, ..self }
    }

    const fn variadic(self) -> Self {
        Self { variadic: true, ..self }
    }

    /// A `--name` argument the request must carry.
    const fn required(self) -> Self {
        Self { required: true, ..self }
    }

    const fn default(self, value: i64) -> Self {
        Self { default: Some(value), ..self }
    }

    /// How usage text and errors name it: `<id>` or `--limit`.
    pub fn label(&self) -> String {
        if self.positional { format!("<{}>", self.name) } else { format!("--{}", self.name) }
    }
}

/// One command.
#[derive(Debug)]
pub struct Spec {
    /// The request's `command`: `mail.list`.
    pub id: &'static str,
    pub summary: &'static str,
    pub arguments: &'static [Argument],
    /// Has effects Mailstream cannot undo (sending, signing out): the request must carry `yes: true`.
    pub confirm: bool,
}

impl Spec {
    const fn new(id: &'static str, summary: &'static str, arguments: &'static [Argument]) -> Self {
        Self { id, summary, arguments, confirm: false }
    }

    const fn confirmed(self) -> Self {
        Self { confirm: true, ..self }
    }

    /// Space-separated words, as the command line and messages name the command: `config set`.
    pub fn name(&self) -> String {
        self.id.replace('.', " ")
    }

    /// Runs on settings.json itself, so the command line needs no running app for it.
    pub fn local(&self) -> bool {
        self.id.starts_with("config.")
    }

    /// Usage text for one command.
    pub fn help(&self) -> String {
        let mut usage = vec![format!("mailstream {}", self.name())];
        for argument in self.arguments.iter().filter(|argument| argument.positional) {
            usage.push(if argument.variadic { format!("{}...", argument.label()) } else { argument.label() });
        }
        let options = if self.arguments.iter().any(|argument| !argument.positional) { " [options]" } else { "" };
        let mut lines = vec![format!("{}{options}", usage.join(" ")), String::new(), self.summary.to_owned()];
        if !self.arguments.is_empty() {
            lines.push(String::new());
        }
        for argument in self.arguments {
            let mut detail = argument.summary.to_owned();
            if let Kind::Choice(values) = argument.kind {
                detail += &format!(" One of: {}.", values.join(", "));
            }
            if argument.required && !argument.positional {
                detail += " Required.";
            }
            if let Some(value) = argument.default {
                detail += &format!(" Default: {value}.");
            }
            lines.push(row(&argument.label(), &detail));
        }
        if self.local() {
            lines.extend([String::new(), "Works without the app running.".to_owned()]);
        }
        lines.join("\n")
    }

    /// The command as JSON, for `mailstream commands`.
    pub fn json(&self) -> Value {
        let arguments: Vec<Value> = self
            .arguments
            .iter()
            .map(|argument| {
                let mut out = json!({
                    "name": argument.name, "summary": argument.summary,
                    "positional": argument.positional, "required": argument.required,
                });
                out["type"] = json!(match argument.kind {
                    Kind::String | Kind::Choice(_) => "string",
                    Kind::Int => "integer",
                    Kind::Bool => "boolean",
                    Kind::List => "array",
                });
                if let Kind::Choice(values) = argument.kind {
                    out["enum"] = json!(values);
                }
                if argument.variadic {
                    out["variadic"] = json!(true);
                }
                if let Some(value) = argument.default {
                    out["default"] = json!(value);
                }
                out
            })
            .collect();
        json!({
            "name": self.name(), "summary": self.summary, "confirm": self.confirm,
            "needsApp": !self.local(), "arguments": arguments,
        })
    }
}

/// One line of a two-column help listing: the term padded (or cut) to 18 characters.
pub fn row(term: &str, description: &str) -> String {
    let term: String = term.chars().chain(std::iter::repeat(' ')).take(18).collect();
    format!("  {term} {description}")
}

const YES: Argument =
    Argument::new("yes", Kind::Bool, "Confirm; without it nothing is done and the error says what would happen.");
const CONVERSATION_IDS: Argument = Argument::new(
    "ids",
    Kind::List,
    "Conversation ids from `mail list`, `mail search` or `mail show`.",
)
.positional()
.variadic();
const CONVERSATION_ID: Argument = Argument::new("id", Kind::String, "Conversation id.").positional();
const MAILBOX: Argument = Argument::new(
    "mailbox",
    Kind::List,
    "Mailbox roles (inbox, starred, important, sent, drafts, archive, all, junk, trash), or with --account a mailbox id or name. Comma-separated for several.",
);
const ACCOUNT: Argument = Argument::new("account", Kind::String, "Only this account (its address).");
const ACCOUNT_ADDRESS: Argument = Argument::new("account", Kind::String, "Account address.").positional();
const PROVIDER: Argument = Argument::new("provider", Kind::Choice(&["google", "microsoft"]), "Provider.").positional();
const LIMIT: Argument = Argument::new("limit", Kind::Int, "Most conversations returned (1-200).").default(25);
const SETTING_KEY: Argument = Argument::new("key", Kind::String, "Setting key.").positional();
const CHAT_ACCOUNT: Argument = Argument::new("account", Kind::String, "Chat account (its address).").positional();
const CHAT_SPACE: Argument = Argument::new("space", Kind::String, "Space name from `chat spaces` (spaces/…).").positional();
/// The AI categories (`tagging::Category`).
const CATEGORIES: Kind = Kind::Choice(&["people", "notification", "newsletter", "invitation"]);
const ATTACHMENTS: Argument = Argument::new("attachments", Kind::List, "A file to attach; repeat for several.");

/// Every command, in the order help lists them.
pub const COMMANDS: &[Spec] = &[
    Spec::new("status", "The app's state: version, accounts, sync, push, AI tagging, settings problems.", &[]),
    Spec::new("config.list", "Every setting with its value, default, type and description.", &[]),
    Spec::new(
        "config.get",
        "One setting.",
        &[Argument::new("key", Kind::String, "Setting key, e.g. sync.pollSeconds.").positional()],
    ),
    Spec::new(
        "config.set",
        "Changes a setting; the running app applies it at once.",
        &[
            SETTING_KEY,
            Argument::new(
                "value",
                Kind::String,
                "true/false, a number, text, or for lists a JSON array or comma-separated values.",
            )
            .positional(),
        ],
    ),
    Spec::new("config.reset", "Puts a setting back to its default.", &[SETTING_KEY]),
    Spec::new(
        "config.add",
        "Adds values to a list setting.",
        &[
            Argument::new("key", Kind::String, "List setting key, e.g. remoteContent.trustedSenders.").positional(),
            Argument::new("values", Kind::List, "Values to add.").positional().variadic(),
        ],
    ),
    Spec::new(
        "config.remove",
        "Removes values from a list setting.",
        &[
            Argument::new("key", Kind::String, "List setting key.").positional(),
            Argument::new("values", Kind::List, "Values to remove.").positional().variadic(),
        ],
    ),
    Spec::new("config.schema", "JSON Schema of settings.json.", &[]),
    Spec::new("config.path", "Where settings.json lives; editing it directly works too.", &[]),
    Spec::new("accounts.list", "Signed-in accounts: address, label, provider, services.", &[]),
    Spec::new(
        "accounts.rename",
        "Changes the label an account shows in the sidebar; empty shows the address.",
        &[ACCOUNT_ADDRESS, Argument::new("label", Kind::String, "New label.").positional()],
    ),
    Spec::new(
        "accounts.client",
        "The OAuth client a sign-in uses unless told otherwise: the one used last for the provider, else the build's. Its id, and whether a secret is saved with it (the secret itself stays in the Keychain).",
        &[PROVIDER],
    ),
    Spec::new(
        "accounts.add",
        "Signs in a new account through the browser and waits (up to 5 minutes) for the person to finish.",
        &[
            PROVIDER,
            Argument::new(
                "client-id",
                Kind::String,
                "OAuth client id; defaults to the one used last (or the build's).",
            ),
            Argument::new(
                "client-secret",
                Kind::String,
                "OAuth client secret; Google requires one. Without it, a --client-id that is the default client's uses that client's saved secret.",
            ),
        ],
    ),
    Spec::new(
        "accounts.remove",
        "Signs an account out and deletes its saved sign-in. Mail on the server is untouched.",
        &[ACCOUNT_ADDRESS, YES],
    )
    .confirmed(),
    Spec::new("mailboxes", "Each account's mailboxes with role, id and counts.", &[ACCOUNT]),
    Spec::new(
        "mail.list",
        "Conversations in mailboxes, newest first, with AI category and waiting-on-you flag. The answer's `next`, when there is more, passed as --cursor gives the following page.",
        &[
            MAILBOX,
            ACCOUNT,
            Argument::new("unread", Kind::Bool, "Only unread conversations."),
            Argument::new("starred", Kind::Bool, "Only starred conversations."),
            Argument::new("category", CATEGORIES, "Only this AI category."),
            Argument::new("waiting", Kind::Bool, "Only conversations waiting on your reply or action."),
            LIMIT,
            Argument::new(
                "cursor",
                Kind::String,
                "The previous page's `next`: lists the conversations after it, with the same filters.",
            ),
            Argument::new(
                "cached",
                Kind::Bool,
                "Lists the mail already loaded without asking the providers for their newest first.",
            ),
        ],
    ),
    Spec::new(
        "mail.search",
        "Searches every mailbox but trash and junk with the providers' own search (subject, people, body; Gmail operators such as from: work), newest first.",
        &[
            Argument::new("query", Kind::String, "Free text, or the provider's search syntax.").positional(),
            ACCOUNT,
            LIMIT,
        ],
    ),
    Spec::new(
        "mail.show",
        "A conversation's messages with headers, text bodies and attachments.",
        &[
            CONVERSATION_ID,
            Argument::new("max-chars", Kind::Int, "Longest text body returned per message.").default(20_000),
            Argument::new(
                "html",
                Kind::Bool,
                "Also return each message's HTML body, when it has one, under `html`; inline images point at files in the cache.",
            ),
        ],
    ),
    Spec::new(
        "mail.previews",
        "The preview line of each conversation's latest message, loading the messages that have none yet.",
        &[CONVERSATION_IDS],
    ),
    Spec::new(
        "mail.avatars",
        "Each sender's picture as a PNG file in the cache (contact photo, BIMI logo, Gravatar or, for automated senders, the site icon), looking up those not cached yet; null when there is none.",
        &[
            Argument::new("addresses", Kind::List, "Sender addresses.").positional().variadic(),
            Argument::new(
                "automated",
                Kind::List,
                "Those of the addresses that send automated mail, for which a site icon may stand in.",
            ),
        ],
    ),
    Spec::new(
        "mail.read",
        "Marks conversations read (or with --unread, unread).",
        &[CONVERSATION_IDS, Argument::new("unread", Kind::Bool, "Mark unread instead.")],
    ),
    Spec::new(
        "mail.star",
        "Stars conversations (or with --off, unstars them).",
        &[CONVERSATION_IDS, Argument::new("off", Kind::Bool, "Remove the star instead.")],
    ),
    Spec::new(
        "mail.archive",
        "Moves conversations out of the inbox (or the mailbox they were listed from) into Archive / All Mail.",
        &[CONVERSATION_IDS],
    ),
    Spec::new(
        "mail.trash",
        "Moves conversations to the trash. Messages already in the trash are deleted for good, which needs --yes.",
        &[CONVERSATION_IDS, YES],
    ),
    Spec::new(
        "mail.restore",
        "Moves conversations' messages in the trash or junk back to the inbox.",
        &[CONVERSATION_IDS],
    ),
    Spec::new(
        "mail.tag",
        "Corrects the AI tag: category, waiting-on-you, topic labels.",
        &[
            CONVERSATION_IDS,
            Argument::new("category", CATEGORIES, "New category."),
            Argument::new(
                "waiting",
                Kind::Choice(&["true", "false"]),
                "Whether it waits on your reply or action (not newsletters).",
            ),
            Argument::new("labels", Kind::List, "Topic labels (at most two)."),
        ],
    ),
    Spec::new("mail.retag", "Forgets every AI tag and tags the current list again.", &[]),
    Spec::new(
        "mail.send",
        "Sends a new plain-text message, with files attached if given.",
        &[
            Argument::new("from", Kind::String, "Sending account (its address).").required(),
            Argument::new("to", Kind::List, "Recipients.").required(),
            Argument::new("cc", Kind::List, "Cc recipients."),
            Argument::new("bcc", Kind::List, "Bcc recipients."),
            Argument::new("subject", Kind::String, "Subject.").required(),
            Argument::new("body", Kind::String, "Plain-text body; `-` reads standard input.").required(),
            ATTACHMENTS,
            YES,
        ],
    )
    .confirmed(),
    Spec::new(
        "mail.reply",
        "Replies to a conversation's latest message, quoting it like the app does.",
        &[
            CONVERSATION_ID,
            Argument::new("body", Kind::String, "Plain-text reply; `-` reads standard input.").required(),
            Argument::new("all", Kind::Bool, "Reply to everyone."),
            Argument::new("to", Kind::List, "Override reply recipients; omitted uses the reply plan."),
            Argument::new("cc", Kind::List, "Override Cc; an empty list clears it."),
            Argument::new("bcc", Kind::List, "Bcc recipients; an empty list clears it."),
            Argument::new("subject", Kind::String, "Override the reply subject; an empty string clears it."),
            ATTACHMENTS,
            YES,
        ],
    )
    .confirmed(),
    Spec::new("refresh", "Fetches new mail and counts for the list on screen now.", &[]),
    Spec::new(
        "contacts.search",
        "People whose name or address starts with the query, best first, as compose completes them: the accounts' Google and Outlook directories and recent correspondents.",
        &[
            Argument::new("query", Kind::String, "The start of a name, a word of it, or an address.").positional(),
            Argument::new("limit", Kind::Int, "Most people returned (1-12).").default(10),
        ],
    ),
    Spec::new(
        "chat.spaces",
        "Google Chat: the chat accounts and their spaces (direct messages, group chats, spaces) as the list shows them, most recently active first, from what the app has loaded.",
        &[ACCOUNT],
    ),
    Spec::new("chat.more", "Loads another page of older spaces for one chat account, or all of them.", &[ACCOUNT]),
    Spec::new("chat.refresh", "Fetches every chat account's spaces now.", &[]),
    Spec::new(
        "chat.open",
        "Opens a space as the reader does: its cached messages at once, the newest from Google after, kept current and read while it stays open. Answers what is loaded now.",
        &[CHAT_ACCOUNT, CHAT_SPACE],
    ),
    Spec::new("chat.close", "Closes the open space: it is no longer kept current.", &[]),
    Spec::new(
        "chat.messages",
        "A space's loaded messages, oldest first, with those being sent from here.",
        &[CHAT_ACCOUNT, CHAT_SPACE],
    ),
    Spec::new(
        "chat.earlier",
        "Loads the page of messages before the oldest loaded one.",
        &[CHAT_ACCOUNT, CHAT_SPACE],
    ),
    Spec::new(
        "chat.send",
        "Sends a plain-text message to a space and waits for Google to take it.",
        &[
            CHAT_ACCOUNT,
            CHAT_SPACE,
            Argument::new("text", Kind::String, "Message text; `-` reads standard input.").required(),
            Argument::new("thread", Kind::String, "Reply in this thread (`spaces/…/threads/…`)."),
            YES,
        ],
    )
    .confirmed(),
    Spec::new(
        "chat.people",
        "People a chat account talks with, most recently active first, and whether it may start new conversations.",
        &[ACCOUNT_ADDRESS],
    ),
    Spec::new(
        "chat.start",
        "Finds or starts the direct message with one person, or the group chat with several, and answers its space.",
        &[
            ACCOUNT_ADDRESS,
            Argument::new("people", Kind::List, "Email addresses of the people to chat with.").positional().variadic(),
        ],
    ),
    Spec::new(
        "chat.attachment",
        "Downloads a file uploaded to a chat message (its `resource` from `chat messages`) into the cache once and answers its path.",
        &[
            CHAT_ACCOUNT,
            Argument::new("resource", Kind::String, "The attachment's `resource` from `chat messages`.").positional(),
            Argument::new("name", Kind::String, "The file name to keep it under (its kind follows the extension)."),
        ],
    ),
];

/// What `mailstream help` prints above the command list.
pub const OVERVIEW: &str = "mailstream — control the Mailstream mail app from a shell or an AI agent.

Every command prints JSON on stdout. Failures print {\"error\": {\"code\", \"message\"}} on stderr
and exit 1 (2 for usage errors, 3 when the app cannot be reached).

`config …` reads and writes ~/Library/Application Support/Mailstream/settings.json directly;
the running app applies changes at once. Every other command talks to the running app over
a private socket, launching the app in the background if needed.

Conversations are addressed by the `id` that `mail list`, `mail search` and `mail show`
return (e.g. \"you@gmail.com|<message-id>\"). Accounts are addressed by their email address.
Google Chat spaces are addressed by their account and the `space` that `chat spaces` returns
(e.g. \"spaces/AAAA…\"). Sending mail or chat messages and signing accounts out require
--yes; without it the error says what would happen.

`mailstream commands` prints every command and argument as JSON.
`mailstream help <command>` describes one command.";

/// The command with request id `id`.
pub fn named(id: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|spec| spec.id == id)
}

/// The spec of `command`, or the error the request answers.
pub(crate) fn spec(command: &str) -> Result<&'static Spec, EngineError> {
    named(command).ok_or_else(|| {
        EngineError::new(
            "unknown_command",
            format!("Unknown command \"{command}\"; `mailstream commands` lists them."),
        )
    })
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
        let defaults = args("mail.list", json!({ "mailbox": "inbox, sent" })).unwrap();
        assert_eq!((defaults.int("limit"), defaults.list("mailbox")), (Some(25), Some(vec!["inbox".into(), "sent".into()])));
        assert_eq!(args("mail.show", json!({ "id": "x", "max-chars": "40" })).unwrap().int("max-chars"), Some(40));
        assert_eq!(spec("mail.nope").err().unwrap().code, "unknown_command");
    }
}
