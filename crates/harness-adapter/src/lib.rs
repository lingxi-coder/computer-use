//! Turn logic for a single structured model call (design 12.4 and 12.5).
//!
//! The planner never runs as a session here. Each turn is one call whose only
//! permitted output is a typed proposal, and the host decides what a turn
//! amounts to. Which way the call is made (a full Harness assembly or a direct
//! model client) is behind [`transport::StructuredCallTransport`]; everything
//! that must hold on either route lives in [`adapter::PlannerAdapter`]:
//!
//! * the model never supplies the request identity, generation or epoch; the
//!   adapter binds every event to the scope it opened;
//! * a turn is accepted only with exactly one valid submission, counted per
//!   distinct tool-use id from results the execution path published, never
//!   from model text, unverified calls or a slot that changed;
//! * a cancelled or expired turn finishes its cleanup, its slot is cleared and
//!   its scope sealed, and whatever arrives afterwards goes to the trace only.
//!
//! The adapter is IO-free. [`model`] implements route B on a separate runtime;
//! the shell feeds its bounded events in and polls for the result.
#![forbid(unsafe_code)]

pub mod adapter;
pub mod computer;
pub mod computer_worker;
pub mod model;
pub mod teacher;
pub mod transport;
pub mod turn;

pub use adapter::{AdapterConfig, BeginError, PlannerAdapter};
pub use computer::{
    ComputerActionOutcome, ComputerBatchOutcome, ComputerModelStep, OpenAiComputerTransport,
};
pub use teacher::{
    TeacherAdapter, TeacherAnswer, TeacherCall, TeacherCallTransport, TeacherChoice, TeacherOutput,
    TeacherTurnResult,
};
pub use transport::{ScriptedTransport, StructuredCall, StructuredCallTransport, TransportError};
pub use turn::{EventKind, Scope, ScopedEvent, TOOL, TraceEntry, TurnResult, Usage};
