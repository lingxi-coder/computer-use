//! The seam between the execution state machine and whatever produces input.

use interaction_contracts::desktop::primitive::Primitive;

/// What the backend knows about one submission.
///
/// `Unknown` means the outcome could not be established, for example because
/// an acknowledgement was lost. The core then keeps the input in its ledger
/// and never replays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Applied,
    /// Definitely not applied.
    Rejected,
    Unknown,
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

pub struct HitTestRequest<'a> {
    pub target_ref: &'a str,
    /// The digest of the instance the intent was admitted for.
    pub expected_fingerprint: &'a str,
    pub primitive: &'a Primitive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTestOutcome {
    /// The digest of whatever is actually there.
    Element(String),
    /// The backend could not tell in time.
    Unresolved,
}

/// The device-specific part of execution. Implementations must not block the
/// caller for longer than their own declared bound, and must refuse a
/// submission whenever the shell's submit gate is closed.
pub trait InputBackend {
    /// Submit one primitive. Atomic primitives are applied as a single unit.
    fn submit(&mut self, at_ms: u64, primitive: &Primitive) -> SubmitOutcome;

    /// Undo a held input this system owns. Cleanup never sends new presses.
    fn release(&mut self, at_ms: u64, release: &Primitive) -> Result<(), ReleaseFailure>;

    /// Whether anything, including someone else, holds this ledger token.
    fn held(&self, token: &str) -> HeldState;

    /// What is under the pointer or the element an AX action would hit.
    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome;
}
