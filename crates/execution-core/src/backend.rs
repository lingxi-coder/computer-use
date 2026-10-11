//! The seam between the execution state machine and whatever produces input.

use interaction_contracts::desktop::browser_execution::{
    BrowserAction, BrowserActionIntent, BrowserExecutionObservation,
};
use interaction_contracts::desktop::device::{ActionIntent, Observation, PermitRef};
use interaction_contracts::desktop::primitive::Primitive;

/// The current authority, calculated by the core immediately before a step.
/// Backends must not extend it with their own timeout or a later observation.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionAuthority {
    pub permit: PermitRef,
    pub stop_seq: u64,
    pub valid_until_ms: u64,
    pub command_end_ms: u64,
    pub evidence: Observation,
}

/// Browser authority remains typed separately from the native evidence path.
/// It is calculated by the core immediately before a browser action.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSubmissionAuthority {
    pub permit: PermitRef,
    pub stop_seq: u64,
    pub valid_until_ms: u64,
    pub command_end_ms: u64,
    pub evidence: BrowserExecutionObservation,
}

/// Immutable browser action and its core-issued authority. This context exposes
/// no generic CDP command, JavaScript, selector, or native input primitive.
pub struct BrowserSubmitContext<'a> {
    pub intent: &'a BrowserActionIntent,
    pub authority: BrowserSubmissionAuthority,
}

/// What the backend knows about one submission.
///
/// `Unknown` means the outcome could not be established, for example because
/// an acknowledgement was lost. The core then keeps the input in its ledger
/// and never replays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// From `submit`, the backend synchronously completed this input call.
    /// This confirms the bounded input returned, not that the application's
    /// business effect is visible or correct. The core timestamps the return
    /// in its logical clock.
    Applied,
    /// Definitely not applied.
    Rejected,
    Unknown,
    /// Enqueued once; final outcome arrives through poll_completion. Only
    /// atomic primitives may use this path in the current contract.
    Pending {
        sequence: u64,
        reply_deadline_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitCompletion {
    pub sequence: u64,
    pub session_epoch: u64,
    pub intent_id: String,
    /// Must be a terminal outcome, never Pending.
    pub outcome: SubmitOutcome,
    /// Known backend action start, including a call with an unknown result.
    /// Absent for a definite rejection or a lost transport acknowledgement.
    pub submitted_at_ms: Option<u64>,
    /// Backend action return time, independent of the fixed authority window.
    /// This does not claim the application's business effect completed.
    pub input_end_ms: Option<u64>,
    pub failure_code: Option<interaction_contracts::desktop::error::FailureCode>,
}

/// Whether the operating system currently has a key or button held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldState {
    Held,
    NotHeld,
    Unknown,
}

/// The backend could not release an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFailure;

/// What the submission is for. An AX action acts on an element, not on a
/// point, so the backend needs to know which one.
pub struct SubmitContext<'a> {
    /// Immutable original action, including root/session, dependencies,
    /// source ObjectRef, full descriptor and its digest.
    pub intent: &'a ActionIntent,
    pub authority: SubmissionAuthority,
}

pub struct HitTestRequest<'a> {
    /// The same context will be passed to submit; a diagnostic must never
    /// detach a digest from the authority it was tested under.
    pub context: &'a SubmitContext<'a>,
    pub primitive: &'a Primitive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTestOutcome {
    /// The digest of whatever is actually there.
    Element(String),
    /// The backend could not tell in time.
    Unresolved,
    /// This backend checks and commits on the same native handle inside
    /// submit. This does not claim that a hit has already matched.
    CheckedDuringSubmit,
}

/// The device-specific part of execution. Implementations must not block the
/// caller for longer than their own declared bound, and must refuse a
/// submission whenever the shell's submit gate is closed.
pub trait InputBackend {
    /// Submit one primitive. Atomic primitives are applied as a single unit.
    /// Return `Applied` only after the synchronous input call has returned;
    /// return `Pending` when it was only enqueued and needs a later receipt.
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome;

    /// Submit one closed browser operation through its private resource port.
    /// Implementations must not translate it into a native primitive or expose
    /// arbitrary browser commands. Unsupported backends refuse by default.
    fn submit_browser(
        &mut self,
        _at_ms: u64,
        _action: &BrowserAction,
        _context: &BrowserSubmitContext<'_>,
    ) -> SubmitOutcome {
        SubmitOutcome::Rejected
    }

    /// Nonblocking, at most one final response per enqueued submission.
    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        None
    }
    /// Synchronously close and retire any transport with an outstanding
    /// `Pending` action before returning. Backends that can return `Pending`
    /// must override this; the default no-op is valid only for backends that
    /// never return `Pending`.
    fn abandon_pending(&mut self) {}

    /// Ordered, nonblocking revocation before cancellation receipts are sent.
    fn revoke(&mut self, _permit: &PermitRef) {}

    /// Undo a held input this system owns. Cleanup never sends new presses.
    fn release(&mut self, at_ms: u64, release: &Primitive) -> Result<(), ReleaseFailure>;

    /// Whether anything, including someone else, holds this ledger token.
    fn held(&self, token: &str) -> HeldState;

    /// What is under the pointer or the element an AX action would hit.
    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome;
}
