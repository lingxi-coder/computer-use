//! The execution supervisor, without any IO.
//!
//! [`core::ExecutionCore`] is the state machine: permits, guards, arbitration,
//! the input ledger and the stop latch, parameterised over an
//! [`backend::InputBackend`]. [`sim_backend::SimBackend`] is a scripted backend
//! and [`sim::Simulator`] is the I0 reference executor built from the two. No
//! module here connects to an operating-system input API or controls a device.
#![forbid(unsafe_code)]

pub mod backend;
pub mod core;
pub mod latch;
pub mod ledger;
pub mod sim;
pub mod sim_backend;
