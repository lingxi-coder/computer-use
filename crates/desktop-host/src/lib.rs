//! cu-host: the composition root (design 3.1 and 4.6).
//!
//! The engine is an IO-free state machine; this crate is its shell. It owns
//! the SQLite ledger, connects to the device and the source over private
//! sockets, relays decision requests to the decision worker through the
//! supervisor, and answers the operator. Nothing here decides anything about a
//! task: it turns messages into engine inputs and engine outputs into
//! messages.
#![forbid(unsafe_code)]

pub mod bridge;
pub mod config;
pub mod shell;

pub use config::HostConfig;
