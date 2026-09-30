//! cu-source: the process that reports what is on screen (design 5.4 and 6.4).
//!
//! In this build the application is a simulated settings window. The source
//! replays the same world log the device writes, so both see one application,
//! and reports its structure to the host on the shared clock. It reports what
//! it sees and nothing else: it never decides anything about the task.
#![forbid(unsafe_code)]

pub mod config;
pub mod shell;

pub use config::SourceConfig;
