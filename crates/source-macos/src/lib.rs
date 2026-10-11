//! cu-source: the process that reports what is on screen (design 5.4 and 6.4).
//!
//! Fixture mode replays the simulated application's world log. Explicit native
//! mode delegates bounded observations to a macOS helper without producing input.
#![forbid(unsafe_code)]

pub mod config;
mod images;
pub mod native;
pub mod shell;

pub use config::SourceConfig;
