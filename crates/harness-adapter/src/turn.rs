//! What a turn sees and what it can end as.

use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use interaction_contracts::desktop::planning::PlannerOutput;

/// The only tool a turn may use.
pub const TOOL: &str = "StructuredOutput";

/// Who a piece of the conversation belongs to. The adapter assigns it when it
/// opens the turn; a model has no way to say or change it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scope {
    pub root: RootTaskId,
    pub request_id: RequestId,
    pub generation: u64,
}

/// What the call cost. Each part may be unknown, and unknown stays unknown:
/// it is never counted as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
}

impl Usage {
    pub fn add(&mut self, delta: Usage) {
        fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            }
        }
        self.input_tokens = sum(self.input_tokens, delta.input_tokens);
        self.output_tokens = sum(self.output_tokens, delta.output_tokens);
        self.cost_micros = sum(self.cost_micros, delta.cost_micros);
    }

    pub fn is_complete(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some() && self.cost_micros.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// Streamed text. For display and buffering only; it never counts as output.
    Text {
        delta: String,
    },
    /// The model asked to use a tool. Not verified: it proves nothing.
    ToolCall {
        tool_use_id: String,
        tool: String,
    },
    /// The execution path's result for a tool call. `accepted` means it
    /// validated the arguments; `slot` is the value it stored.
    ToolResult {
        tool_use_id: String,
        tool: String,
        accepted: bool,
        slot: Option<serde_json::Value>,
    },
    /// The value in the output slot changed. It is an observation only: a slot
    /// that changes is not a submission, and never counts as one.
    SlotChanged {
        slot: Option<serde_json::Value>,
    },
    Usage(Usage),
    /// The recorder's closing barrier: nothing more belongs to this scope.
    /// `complete` is false when the recorder could not account for everything.
    Finished {
        complete: bool,
    },
}

/// An event and who it says it belongs to. `None` means the recorder could not
/// attribute it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedEvent {
    pub scope: Option<Scope>,
    pub kind: EventKind,
}

impl ScopedEvent {
    pub fn new(scope: &Scope, kind: EventKind) -> Self {
        Self {
            scope: Some(scope.clone()),
            kind,
        }
    }
}

/// How a turn ended. Only `Proposal` can change anything.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnResult {
    Proposal {
        scope: Scope,
        output: Box<PlannerOutput>,
        usage: Usage,
    },
    /// No valid submission.
    Missing {
        scope: Scope,
        usage: Usage,
    },
    /// More than one valid submission, even if they said the same thing.
    Conflict {
        scope: Scope,
        count: usize,
        usage: Usage,
    },
    /// A submission that does not pass the strict schema or the phase rules.
    Invalid {
        scope: Scope,
        reason: String,
        usage: Usage,
    },
    /// Who did what could not be verified, so nothing of the turn is used.
    Quarantined {
        scope: Scope,
        reason: String,
        usage: Usage,
    },
    Cancelled {
        scope: Scope,
        usage: Usage,
    },
    /// The deadline passed before the turn finished.
    Expired {
        scope: Scope,
        usage: Usage,
    },
}

impl TurnResult {
    pub fn scope(&self) -> &Scope {
        match self {
            Self::Proposal { scope, .. }
            | Self::Missing { scope, .. }
            | Self::Conflict { scope, .. }
            | Self::Invalid { scope, .. }
            | Self::Quarantined { scope, .. }
            | Self::Cancelled { scope, .. }
            | Self::Expired { scope, .. } => scope,
        }
    }

    pub fn usage(&self) -> &Usage {
        match self {
            Self::Proposal { usage, .. }
            | Self::Missing { usage, .. }
            | Self::Conflict { usage, .. }
            | Self::Invalid { usage, .. }
            | Self::Quarantined { usage, .. }
            | Self::Cancelled { usage, .. }
            | Self::Expired { usage, .. } => usage,
        }
    }

    pub fn is_proposal(&self) -> bool {
        matches!(self, Self::Proposal { .. })
    }
}

/// Something that happened to a turn or near it. Late and unattributed events
/// end up here and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    pub kind: &'static str,
    pub detail: String,
}
