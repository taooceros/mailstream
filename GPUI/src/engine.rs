//! Mailstream's engine (accounts, sync, cache, threading, tagging, chat), the Rust one in
//! `Engine/`, linked into this process through the `mailstream` crate in `FFI/`. This UI asks it
//! for values and hears about changes from it; it shares the app's mail.db and sign-ins.
//!
//! Calls are futures answered on the engine's threads; they run on the background executor,
//! never blocking a thread.

use std::sync::{Arc, LazyLock};

pub use mailstream::{BatchResult, MailError, Mailstream};

static ENGINE: LazyLock<Arc<Mailstream>> = LazyLock::new(Mailstream::new);

/// The process's engine, started on first use (`main` starts it at launch).
pub fn engine() -> &'static Mailstream {
    &ENGINE
}

/// An action over conversation ids; a failure for any of them is the action's failure. Answers the
/// step `mail.undo` takes it back with, when it changed anything.
pub fn all_done(result: Result<BatchResult, MailError>) -> Result<Option<u64>, MailError> {
    let result = result?;
    match result.failed.into_iter().next() {
        None => Ok(result.undo),
        Some(failure) => {
            Err(MailError::Failed { code: failure.code.unwrap_or_else(|| "failed".into()), message: failure.message })
        }
    }
}
