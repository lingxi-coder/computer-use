//! The call itself. One trait, because the route is chosen later (design 12.4):
//! a full Harness assembly or a direct model client, both with cancellation
//! and billing, neither giving the model a session or any tool but one.

use interaction_contracts::desktop::planning::{PlanningContext, PlanningReason};

use crate::turn::Scope;

/// Everything one call is given: a complete, bounded context the host
/// assembled again for this turn. Turns keep no state between them.
#[derive(Debug, Clone, PartialEq)]
pub struct StructuredCall {
    pub reason: PlanningReason,
    pub task_revision: u64,
    pub context: PlanningContext,
    /// The JSON Schema of the one thing the call may return.
    pub schema: serde_json::Value,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transport: {}", self.0)
    }
}

impl std::error::Error for TransportError {}

pub trait StructuredCallTransport {
    /// Begin one call. Its events come back through the adapter, each tagged
    /// with `scope`.
    fn start(&mut self, scope: &Scope, call: &StructuredCall) -> Result<(), TransportError>;

    /// Ask the call to stop. The transport still delivers the closing barrier
    /// once it has cleaned up; cancellation is not completion.
    fn cancel(&mut self, scope: &Scope);
}

/// A transport that only remembers what it was asked. Tests feed the events
/// themselves, in whatever order and with whatever duplicates they want.
#[derive(Debug, Default)]
pub struct ScriptedTransport {
    pub started: Vec<(Scope, StructuredCall)>,
    pub cancelled: Vec<Scope>,
    /// Refuse to start while this is set.
    pub refuse: Option<String>,
}

impl StructuredCallTransport for ScriptedTransport {
    fn start(&mut self, scope: &Scope, call: &StructuredCall) -> Result<(), TransportError> {
        if let Some(reason) = &self.refuse {
            return Err(TransportError(reason.clone()));
        }
        self.started.push((scope.clone(), call.clone()));
        Ok(())
    }

    fn cancel(&mut self, scope: &Scope) {
        self.cancelled.push(scope.clone());
    }
}
