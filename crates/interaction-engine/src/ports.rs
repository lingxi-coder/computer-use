//! Narrow ports between module owners. A module writes its own tables only;
//! when an operation has to touch two owners' tables in one transaction, the
//! second owner exposes a port and the first calls it.

use interaction_contracts::desktop::decision::DecisionSource;
use interaction_contracts::desktop::ids::{AttemptId, DecisionId, RootTaskId};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};

use crate::infrastructure::storage::StoreError;

/// Where a logical decision is in its life. It is consumed at most once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionLifecycle {
    Pending,
    Consumed,
    Superseded,
    Expired,
}

pub struct ConsumeRequest<'a> {
    pub decision_id: &'a DecisionId,
    pub generation: u64,
    pub attempt_id: &'a AttemptId,
    pub root_id: &'a RootTaskId,
    pub source: DecisionSource,
    /// What must be delivered to the device once the attempt is active.
    pub outbox_payload: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Consumed,
    /// The decision is not pending: already consumed, superseded or expired.
    NotPending(DecisionLifecycle),
    /// A newer generation replaced the request this answer belongs to.
    GenerationMismatch {
        current: u64,
    },
    /// The decision is unknown to the ledger.
    Unknown,
}

/// Reaction's side of the admission transaction.
pub trait DecisionLedger {
    /// Compare-and-swap one pending decision to consumed, and record the
    /// unique decision-to-attempt mapping and the outbox entry, all inside the
    /// caller's transaction.
    fn consume_for_admission(
        &self,
        tx: &Transaction<'_>,
        request: &ConsumeRequest<'_>,
    ) -> Result<ConsumeOutcome, StoreError>;
}
