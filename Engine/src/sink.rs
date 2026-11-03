//! # Sink
//!
//! Where a command's output goes: the buffer of the request running on the current thread, in
//! place of the CLI's stdout. Every request runs on a thread of its own, so a thread-local buffer
//! is exactly one request's output.

use std::{cell::RefCell, fmt, mem};

use anyhow::Result;
use pimalaya_cli::printer::Printer;
use serde::Serialize;

thread_local! {
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Appends bytes to the current request's output.
pub fn write(bytes: &[u8]) {
    OUTPUT.with_borrow_mut(|output| output.extend_from_slice(bytes));
}

/// Takes the current request's output, leaving it empty.
pub fn take() -> Vec<u8> {
    OUTPUT.with_borrow_mut(mem::take)
}

/// The printer handed to commands: compact JSON or plain text, into the sink.
pub struct SinkPrinter {
    pub json: bool,
}

impl Printer for SinkPrinter {
    fn out<T: fmt::Display + Serialize>(&mut self, data: T) -> Result<()> {
        if self.json {
            OUTPUT.with_borrow_mut(|output| serde_json::to_writer(output, &data))?;
        } else {
            write(data.to_string().as_bytes());
        }
        Ok(())
    }

    fn is_json(&self) -> bool {
        self.json
    }
}
