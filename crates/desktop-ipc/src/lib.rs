//! Plumbing shared by cu-host, cu-device, cu-source and the supervisor.
//!
//! Everything here is blocking IO over standard streams and Unix sockets, with
//! a reader thread per stream feeding one channel, so each process can run a
//! single-threaded loop over the IO-free state machines. The messages are the
//! desktop v2 envelopes, length-prefixed, exactly as the contract defines them.
#![forbid(unsafe_code)]

pub mod browser_control;
pub mod clock;
pub mod computer_control;
pub mod computer_image;
pub mod handshake;
pub mod image;
pub mod link;
pub mod socket;

pub use clock::SharedClock;
pub use handshake::{
    Connection, HandshakeFailure, accept_hello, make_hello, read_ready, send_hello,
};
pub use link::{FramedReader, Inbound, Link, ReadError, spawn_reader};
