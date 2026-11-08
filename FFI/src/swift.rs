//! The Swift engine Mailstream.app still has (`libMailstreamKit.dylib`), for comparing it with
//! the Rust one (`examples/bench.rs`): built only with the `swift-engine` feature, and run by a
//! process started with `MAILSTREAM_ENGINE=swift`. Requests cross its C API
//! (`Sources/MailstreamKit/FFI/Exports.swift`) as the control socket's JSON; the engine runs on
//! the main thread's run loop, where it calls listeners too. It goes with the Swift engine (MS-120).
use std::ffi::c_void;
use std::sync::{Arc, LazyLock};

use futures_channel::oneshot;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Change, Event, EventListener, MailError};

type Callback = extern "C" fn(*mut c_void, *const u8, usize);

unsafe extern "C" {
    fn mailstream_start();
    fn mailstream_call(request: *const u8, length: usize, context: *mut c_void, reply: Callback);
    fn mailstream_subscribe(context: *mut c_void, event: Callback);
    fn mailstream_run_main_loop();
}

/// Whether this process runs the Swift engine instead of the Rust one, decided once at start.
pub(crate) static USE_SWIFT_ENGINE: LazyLock<bool> =
    LazyLock::new(|| std::env::var("MAILSTREAM_ENGINE").is_ok_and(|engine| engine == "swift"));

pub(crate) fn start() {
    unsafe { mailstream_start() };
}

pub(crate) fn subscribe(listener: Arc<dyn EventListener>) {
    let context = Box::into_raw(Box::new(listener)).cast::<c_void>();
    unsafe { mailstream_subscribe(context, deliver_event) };
}

/// The main thread's run loop, which the Swift engine works on; never returns.
pub(crate) fn run_main_loop() {
    unsafe { mailstream_run_main_loop() };
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
    result: Option<Value>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

/// The Swift engine's result for one request, through its C API.
pub(crate) async fn call(command: &str, args: Value) -> Result<Value, MailError> {
    let request = serde_json::to_vec(&json!({ "command": command, "args": args })).expect("JSON values encode");
    let (sender, receiver) = oneshot::channel::<Vec<u8>>();
    let context = Box::into_raw(Box::new(sender)).cast::<c_void>();
    unsafe { mailstream_call(request.as_ptr(), request.len(), context, deliver_answer) };
    let bytes = receiver.await.map_err(|_| MailError::new("unreachable", "The engine dropped the request."))?;
    let answer: Answer = serde_json::from_slice(&bytes)
        .map_err(|error| MailError::new("decode", format!("Unexpected answer to {command}: {error}")))?;
    if !answer.ok {
        let error = answer.error.unwrap_or(ErrorBody { code: None, message: None });
        return Err(MailError::Failed {
            code: error.code.unwrap_or_else(|| "failed".into()),
            message: error.message.unwrap_or_else(|| format!("{command} failed.")),
        });
    }
    Ok(answer.result.unwrap_or(Value::Null))
}

/// The engine's answer to one request: the context is that request's sender, used once.
extern "C" fn deliver_answer(context: *mut c_void, bytes: *const u8, length: usize) {
    let sender = unsafe { Box::from_raw(context.cast::<oneshot::Sender<Vec<u8>>>()) };
    let _ = sender.send(unsafe { bytes_of(bytes, length) }.to_vec());
}

/// One engine event: the context is a subscriber's listener, kept for the life of the process.
extern "C" fn deliver_event(context: *mut c_void, bytes: *const u8, length: usize) {
    let listener = unsafe { &*context.cast::<Arc<dyn EventListener>>() };
    #[derive(Deserialize)]
    struct Message {
        event: String,
    }
    if let Ok(message) = serde_json::from_slice::<Message>(unsafe { bytes_of(bytes, length) }) {
        listener.on_event(Change { event: Event::named(&message.event) });
    }
}

unsafe fn bytes_of<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    if bytes.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(bytes, length) } }
}
