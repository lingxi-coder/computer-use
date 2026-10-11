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
mod browser_reference;
pub mod computer_image;
mod computer_session;
pub mod config;
pub mod model_bootstrap;
mod native_reference;
pub mod owned_document_bootstrap;
mod owned_reference;
mod owned_revisit_control;
pub mod shell;
mod source_capture;
mod source_refresh;
pub mod teacher_observation;

pub use config::HostConfig;

#[doc(hidden)]
pub fn open_owned_revisit_control_fd3() -> Result<std::os::unix::net::UnixStream, String> {
    owned_revisit_control::open_inherited_fd3()
}
