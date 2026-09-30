//! The seven modules of the interaction loop, as IO-free state machines.
//!
//! Environment, TaskRuntime, Reaction, Deliberation, Execution (the host's
//! view of the device), Verification and Memory each own their state and
//! their SQLite tables, and talk through typed messages and narrow ports. The
//! shell feeds events in and carries effects out; nothing here reads a clock
//! or touches a socket.
#![forbid(unsafe_code)]

pub mod deliberation;
pub mod environment;
pub mod infrastructure;
pub mod memory;
pub mod ports;
pub mod reaction;
pub mod tasks;
pub mod verification;

#[cfg(test)]
pub(crate) mod testkit;
