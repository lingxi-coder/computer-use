//! The turn lock and the acceptance rule (design 12.4).
//!
//! One root has one open turn. The adapter opens it with a scope it assigned,
//! takes events in, and only at the recorder's closing barrier decides what
//! the turn amounts to. Acceptance needs exactly one valid submission,
//! counted per distinct tool-use id from results the execution path published.
//! Model text, calls nobody verified, and a slot that changed do not count,
//! and a second valid submission is a conflict even when it says the same
//! thing or leaves a single value in the slot.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::planning::{PlannerOutput, PlanningReason, PlanningRequest};

use crate::transport::{StructuredCall, StructuredCallTransport, TransportError};
use crate::turn::{EventKind, Scope, ScopedEvent, TOOL, TraceEntry, TurnResult, Usage};

/// How many closed scopes are remembered, to tell a late event from a stranger.
const SEALED_MEMORY: usize = 32;
const TRACE_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterConfig {
    /// How long a cancelled or expired turn may take to confirm its cleanup
    /// before it is isolated.
    pub barrier_grace_ms: u64,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            barrier_grace_ms: 2_000,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BeginError {
    /// A turn is open. One root has one turn at a time.
    Busy,
    /// A retry needs a new generation, newer than the last turn's for this root.
    StaleGeneration {
        last: u64,
    },
    /// The request's own deadline has already passed.
    AlreadyExpired,
    Invalid(String),
    Transport(TransportError),
}

impl std::fmt::Display for BeginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("a turn is already open"),
            Self::StaleGeneration { last } => {
                write!(f, "generation must be newer than {last}")
            }
            Self::AlreadyExpired => f.write_str("the request's deadline has passed"),
            Self::Invalid(reason) => write!(f, "invalid request: {reason}"),
            Self::Transport(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for BeginError {}

/// The JSON Schema of the one thing a turn may return.
pub fn output_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(PlannerOutput))
        .expect("a derived schema always serialises")
}

struct Turn {
    scope: Scope,
    reason: PlanningReason,
    deadline_ms: u64,
    /// Tool calls the model made; each proves only that a call was made.
    calls: BTreeSet<String>,
    /// Results the execution path published, per distinct tool-use id.
    results: BTreeMap<String, bool>,
    /// Ids whose accepted result counts as a valid submission, with the value
    /// each one stored.
    accepted: BTreeMap<String, Option<serde_json::Value>>,
    /// The slot as it stands. It is overwritten by each accepted result.
    slot: Option<serde_json::Value>,
    usage: Usage,
    /// Something that makes the turn impossible to attribute.
    breach: Option<String>,
    finished: Option<bool>,
    cancel_requested: bool,
    expired: bool,
    barrier_deadline: Option<u64>,
}

impl Turn {
    fn new(scope: Scope, reason: PlanningReason, deadline_ms: u64) -> Self {
        Self {
            scope,
            reason,
            deadline_ms,
            calls: BTreeSet::new(),
            results: BTreeMap::new(),
            accepted: BTreeMap::new(),
            slot: None,
            usage: Usage::default(),
            breach: None,
            finished: None,
            cancel_requested: false,
            expired: false,
            barrier_deadline: None,
        }
    }

    fn breach(&mut self, reason: impl Into<String>) {
        self.breach.get_or_insert_with(|| reason.into());
    }

    /// Apply one event that belongs to this turn. Returns a trace note when the
    /// event was not counted for a reason worth keeping.
    fn apply(&mut self, kind: EventKind) -> Option<(&'static str, String)> {
        match kind {
            // Shown and buffered, never counted: a model can write anything.
            EventKind::Text { .. } => None,
            EventKind::ToolCall { tool_use_id, tool } => {
                if tool != TOOL {
                    return Some(("denied_call", format!("{tool} is not a permitted tool")));
                }
                self.calls.insert(tool_use_id);
                None
            }
            EventKind::ToolResult {
                tool_use_id,
                tool,
                accepted,
                slot,
            } => {
                if tool != TOOL {
                    if accepted {
                        // The permission gate should have refused it.
                        self.breach(format!(
                            "an unlisted tool ({tool}) produced an accepted result"
                        ));
                    }
                    return Some(("denied_result", format!("{tool} is not a permitted tool")));
                }
                if !self.calls.contains(&tool_use_id) {
                    // A result for a call nobody saw cannot be attributed.
                    self.breach(format!("result {tool_use_id} has no matching call"));
                    return Some(("orphan_result", tool_use_id));
                }
                match self.results.get(&tool_use_id) {
                    // The same delivery again: counted once.
                    Some(previous) if *previous == accepted => {
                        return Some(("duplicate_result", tool_use_id));
                    }
                    Some(_) => {
                        self.breach(format!("{tool_use_id} was given two different results"));
                        return Some(("contradictory_result", tool_use_id));
                    }
                    None => {}
                }
                self.results.insert(tool_use_id.clone(), accepted);
                if accepted {
                    self.accepted.insert(tool_use_id, slot.clone());
                    self.slot = slot;
                }
                None
            }
            // A changing slot is an observation, not a submission.
            EventKind::SlotChanged { slot } => {
                self.slot = slot;
                None
            }
            EventKind::Usage(delta) => {
                self.usage.add(delta);
                None
            }
            EventKind::Finished { complete } => {
                self.finished = Some(complete);
                None
            }
        }
    }

    /// What the turn amounts to, once its barrier has passed.
    fn decide(&self) -> TurnResult {
        let scope = self.scope.clone();
        let usage = self.usage;
        if self.cancel_requested {
            return TurnResult::Cancelled { scope, usage };
        }
        if self.expired {
            return TurnResult::Expired { scope, usage };
        }
        if let Some(reason) = &self.breach {
            return TurnResult::Quarantined {
                scope,
                reason: reason.clone(),
                usage,
            };
        }
        if self.finished != Some(true) {
            return TurnResult::Quarantined {
                scope,
                reason: "the recorder could not account for the whole turn".into(),
                usage,
            };
        }
        match self.accepted.len() {
            0 => TurnResult::Missing { scope, usage },
            1 => self.single(scope, usage),
            count => TurnResult::Conflict {
                scope,
                count,
                usage,
            },
        }
    }

    fn single(&self, scope: Scope, usage: Usage) -> TurnResult {
        let invalid = |reason: &str| TurnResult::Invalid {
            scope: scope.clone(),
            reason: reason.to_string(),
            usage,
        };
        let Some((_, submitted)) = self.accepted.iter().next() else {
            return invalid("there is no accepted submission");
        };
        // The slot at the barrier must be what the one valid submission stored.
        // If something else changed it in between, what is in it is not what
        // was validated, and nothing of the turn is used.
        if &self.slot != submitted {
            return TurnResult::Quarantined {
                scope,
                reason: "the slot changed outside the accepted submission".into(),
                usage,
            };
        }
        let Some(value) = submitted.clone() else {
            return invalid("the accepted submission left no value in the slot");
        };
        let output: PlannerOutput = match serde_json::from_value(value) {
            Ok(output) => output,
            Err(error) => return invalid(&format!("the slot does not fit the schema: {error}")),
        };
        // A goal belongs to goal compilation and nowhere else, and compilation
        // takes nothing but a plan.
        let shape_ok = match (&self.reason, &output) {
            (PlanningReason::GoalCompile, PlannerOutput::Plan { proposal }) => {
                proposal.goal_draft.is_some()
            }
            (PlanningReason::GoalCompile, _) => false,
            (_, PlannerOutput::Plan { proposal }) => proposal.goal_draft.is_none(),
            _ => true,
        };
        if !shape_ok {
            return invalid("the proposal is not allowed in this phase");
        }
        if let PlannerOutput::Plan { proposal } = &output
            && let Err(reason) = proposal.bundle.validate()
        {
            return invalid(&format!("the plan is malformed: {reason}"));
        }
        TurnResult::Proposal {
            scope,
            output: Box::new(output),
            usage,
        }
    }
}

pub struct PlannerAdapter<T: StructuredCallTransport> {
    transport: T,
    cfg: AdapterConfig,
    active: Option<Turn>,
    sealed: VecDeque<Scope>,
    latest_generation: BTreeMap<RootTaskId, u64>,
    trace: Vec<TraceEntry>,
}

impl<T: StructuredCallTransport> PlannerAdapter<T> {
    pub fn new(transport: T, cfg: AdapterConfig) -> Self {
        Self {
            transport,
            cfg,
            active: None,
            sealed: VecDeque::new(),
            latest_generation: BTreeMap::new(),
            trace: Vec::new(),
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Everything that was set aside rather than counted.
    pub fn trace(&self) -> &[TraceEntry] {
        &self.trace
    }

    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    pub fn active_scope(&self) -> Option<&Scope> {
        self.active.as_ref().map(|turn| &turn.scope)
    }

    fn note(&mut self, kind: &'static str, detail: impl Into<String>) {
        self.trace.push(TraceEntry {
            kind,
            detail: detail.into(),
        });
        if self.trace.len() > TRACE_LIMIT {
            self.trace.remove(0);
        }
    }

    /// Open a turn for a request the host trusts. The scope is taken from the
    /// request; the model is never asked for, and never gives, an identity.
    pub fn begin(&mut self, request: &PlanningRequest, now_ms: u64) -> Result<Scope, BeginError> {
        request.validate().map_err(BeginError::Invalid)?;
        if self.active.is_some() {
            return Err(BeginError::Busy);
        }
        if let Some(last) = self.latest_generation.get(&request.root_task_id)
            && request.generation <= *last
        {
            return Err(BeginError::StaleGeneration { last: *last });
        }
        if now_ms >= request.deadline_ms {
            return Err(BeginError::AlreadyExpired);
        }
        let scope = Scope {
            root: request.root_task_id.clone(),
            request_id: request.request_id.clone(),
            generation: request.generation,
        };
        let call = StructuredCall {
            reason: request.reason,
            task_revision: request.task_revision,
            context: request.context.clone(),
            schema: output_schema(),
            deadline_ms: request.deadline_ms,
        };
        self.transport
            .start(&scope, &call)
            .map_err(BeginError::Transport)?;
        self.latest_generation
            .insert(request.root_task_id.clone(), request.generation);
        self.active = Some(Turn::new(
            scope.clone(),
            request.reason,
            request.deadline_ms,
        ));
        Ok(scope)
    }

    /// One event from the recorder. It counts only for the open turn, only by
    /// the scope it carries, and only until the closing barrier.
    pub fn on_event(&mut self, event: ScopedEvent) {
        let Some(scope) = event.scope.clone() else {
            self.note("unattributed", format!("{:?}", event.kind));
            return;
        };
        let note = match self.active.as_mut() {
            Some(turn) if turn.scope == scope => {
                if turn.finished.is_some() {
                    Some(("late", format!("after the barrier of {}", scope.request_id)))
                } else {
                    turn.apply(event.kind)
                }
            }
            _ if self.sealed.contains(&scope) => Some((
                "late",
                format!(
                    "for {} generation {}, already closed",
                    scope.request_id, scope.generation
                ),
            )),
            _ => Some((
                "foreign",
                format!("for {} generation {}", scope.request_id, scope.generation),
            )),
        };
        if let Some((kind, detail)) = note {
            self.note(kind, detail);
        }
    }

    /// Stop the open turn. Its cleanup still has to be confirmed by the
    /// recorder's barrier before the turn ends, and nothing it produced can be
    /// installed.
    pub fn cancel(&mut self, now_ms: u64) {
        let Some(turn) = self.active.as_mut() else {
            return;
        };
        if turn.cancel_requested {
            return;
        }
        turn.cancel_requested = true;
        if turn.finished.is_none() {
            self.transport.cancel(&turn.scope);
            turn.barrier_deadline = Some(now_ms.saturating_add(self.cfg.barrier_grace_ms));
        }
    }

    /// The result, once the turn is over. Also where deadlines are enforced:
    /// a turn past its deadline is cancelled, and if its cleanup cannot be
    /// confirmed in time the turn is isolated instead of trusted.
    pub fn poll(&mut self, now_ms: u64) -> Option<TurnResult> {
        let turn = self.active.as_mut()?;
        if turn.finished.is_none() {
            if !turn.cancel_requested && !turn.expired && now_ms > turn.deadline_ms {
                turn.expired = true;
                self.transport.cancel(&turn.scope);
                turn.barrier_deadline = Some(now_ms.saturating_add(self.cfg.barrier_grace_ms));
                self.note("expired", "the deadline passed; cancelling");
                return None;
            }
            let barrier_late = turn.barrier_deadline.is_some_and(|limit| now_ms > limit);
            if !barrier_late {
                return None;
            }
            let result = TurnResult::Quarantined {
                scope: turn.scope.clone(),
                reason: "the recorder did not confirm cleanup in time".into(),
                usage: turn.usage,
            };
            self.close();
            return Some(result);
        }
        let result = turn.decide();
        self.close();
        Some(result)
    }

    /// The turn is over: its slot is gone with it and its scope is sealed, so
    /// whatever arrives later can only be traced.
    fn close(&mut self) {
        if let Some(turn) = self.active.take() {
            self.sealed.push_back(turn.scope);
            while self.sealed.len() > SEALED_MEMORY {
                self.sealed.pop_front();
            }
        }
    }
}
