//! Wire contracts shared by every process.
//!
//! [`desktop`] is the protocol between the desktop processes (desktop v1).
//!
//! [`i0`] is the I0 wire v1 protocol. It is frozen: its serialized form must
//! keep matching `contracts/i0-v1/interaction.schema.json` and the fixtures
//! next to it. New messages go into a separate protocol namespace instead of
//! changing this module.
#![forbid(unsafe_code)]

pub mod desktop;
pub mod i0;
