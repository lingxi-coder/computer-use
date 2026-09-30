//! The execution supervisor, without any IO.
//!
//! [`sim`] is the I0 reference executor with virtual time and a simulated
//! input backend. No module here connects to an operating-system input API or
//! controls a real device.
#![forbid(unsafe_code)]

pub mod sim;
