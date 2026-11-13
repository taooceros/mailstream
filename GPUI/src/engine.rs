//! Mailstream's engine (accounts, sync, cache, threading, tagging, chat), the Rust one in
//! `Engine/`, linked into this process through the `mailstream` crate in `FFI/`. This UI asks it
//! for values and hears about changes from it; it shares the app's mail.db and sign-ins.
//!
//! Calls are futures answered on the engine's threads; they run on the background executor,
//! never blocking a thread.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

pub use mailstream::{BatchFailure, BatchResult, MailError, Mailstream};

static ENGINE: LazyLock<Arc<Mailstream>> = LazyLock::new(Mailstream::new);

/// The process's engine, started on first use (`main` starts it at launch).
pub fn engine() -> &'static Mailstream {
    &ENGINE
}

/// What an action over conversation ids came to (MS-305): the step `mail.undo` takes back what it
/// did with (none when it changed nothing), the ids it failed for and the first failure.
pub struct Outcome {
    pub undo: Option<u64>,
    pub failed: HashSet<String>,
    pub error: Option<MailError>,
}

/// The outcome of an action over `ids` the engine answered `result` to: the ids it failed for, and
/// the step it took.
pub fn outcome(result: Result<(Vec<BatchFailure>, Option<u64>), MailError>, ids: &[String]) -> Outcome {
    match result {
        Err(error) => Outcome { undo: None, failed: ids.iter().cloned().collect(), error: Some(error) },
        Ok((failed, undo)) => {
            let error = failed.first().map(|failure| MailError::Failed {
                code: failure.code.clone().unwrap_or_else(|| "failed".into()),
                message: failure.message.clone(),
            });
            Outcome { undo, failed: failed.into_iter().map(|failure| failure.id).collect(), error }
        }
    }
}
