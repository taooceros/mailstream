//! # Mailstream engine
//!
//! himalaya 2.2.1 as a library linked into Mailstream. The modules below are himalaya's own, plus
//! [`chat`] (Google Chat, Mailstream's); its binary entry point is replaced by [`engine`], which
//! runs the same command lines in-process and keeps each account's connections open between them.

mod account;
mod backend;
#[cfg(feature = "chat")]
mod chat;
mod cli;
mod config;
mod email;
mod engine;
#[cfg(feature = "gmail")]
mod gmail;
#[cfg(feature = "imap")]
mod imap;
#[cfg(feature = "jmap")]
mod jmap;
mod json_schema;
#[cfg(feature = "m2dir")]
mod m2dir;
#[cfg(feature = "maildir")]
mod maildir;
#[cfg(feature = "mbox")]
mod mbox;
#[cfg(feature = "msgraph")]
mod msgraph;
#[cfg(feature = "pimdir")]
mod pimdir;
mod shared;
#[cfg(feature = "sieve")]
mod sieve;
mod sink;
#[cfg(feature = "smtp")]
mod smtp;
mod wizard;
