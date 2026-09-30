//! A simulated world for the desktop system.
//!
//! [`app::SimApp`] is a small settings application that reports its structure
//! as snapshots and reacts to the inputs a device backend applied.
//! [`policy`] holds deterministic stand-ins for the planner and the teacher
//! model. None of it is production code: it exists so the whole closed loop
//! can run, and be tested, without a real application or a real model.
#![forbid(unsafe_code)]

pub mod app;
pub mod policy;
pub mod world_log;
