//! cu-device: the process that owns the input channel (design 3.1, 5.3, 14).
//!
//! The execution core is an IO-free state machine; this crate is its shell. It
//! listens on a private socket for the host, reads stop commands from the
//! supervisor directly (so a stop never waits for the host), advances the core
//! on the shared clock, and forwards what the core reports. Losing the
//! supervisor or the host closes the submit gate, on its own, without asking
//! anyone.
//!
//! The only backend here is simulated: inputs go into a world log that the
//! source process replays. A real macOS backend replaces `SimWorldBackend`
//! in M2 and is where the platform code, and the unsafe code it needs, will
//! live.
#![forbid(unsafe_code)]

pub mod backend;
pub mod config;
pub mod shell;

pub use backend::SimWorldBackend;
pub use config::DeviceConfig;
