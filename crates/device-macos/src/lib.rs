//! cu-device: the process that owns the input channel (design 3.1, 5.3, 14).
//!
//! The execution core is an IO-free state machine; this crate is its shell. It
//! listens on a private socket for the host, reads stop commands from the
//! supervisor directly (so a stop never waits for the host), advances the core
//! on the shared clock, and forwards what the core reports. Losing the
//! supervisor or the host closes the submit gate, on its own, without asking
//! anyone.
//!
//! Simulated sessions send inputs to a world log. Native observation sessions
//! explicitly refuse input; foreground fixed-target and separately consented
//! background AXPress sessions use distinct guarded backends.
#![forbid(unsafe_code)]

pub mod backend;
pub mod config;
pub mod guard;
pub mod owned_document_bootstrap;
pub mod shell;

pub use backend::{DeviceBackend, SimWorldBackend};
pub use config::{DeviceBackendMode, DeviceConfig};
