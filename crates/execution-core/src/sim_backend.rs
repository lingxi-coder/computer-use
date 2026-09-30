//! A scripted input backend. It produces no real input: it keeps a log of
//! what would have been applied and lets a test or a shell inject faults.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::device::{SimFault, SimHitTest, SimOp};
use interaction_contracts::desktop::primitive::Primitive;

use crate::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitOutcome,
};

/// One input the simulated operating system received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedInput {
    pub at_ms: u64,
    pub primitive: Primitive,
    pub cleanup: bool,
}

#[derive(Debug)]
pub struct SimBackend {
    events: Vec<AppliedInput>,
    physical_held: BTreeSet<String>,
    faults: BTreeMap<u64, SimFault>,
    submission_calls: u64,
    release_failure: bool,
    hit_test: SimHitTest,
}

impl Default for SimBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SimBackend {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            physical_held: BTreeSet::new(),
            faults: BTreeMap::new(),
            submission_calls: 0,
            release_failure: false,
            hit_test: SimHitTest::Match,
        }
    }

    /// What the simulated system received, in order.
    pub fn events(&self) -> &[AppliedInput] {
        &self.events
    }

    /// Ledger tokens currently held, by anyone.
    pub fn held_tokens(&self) -> Vec<String> {
        self.physical_held.iter().cloned().collect()
    }

    pub fn hold_external_key(&mut self, key: &str) {
        self.physical_held.insert(format!("key:{key}"));
    }

    pub fn set_release_failure(&mut self, fail: bool) {
        self.release_failure = fail;
    }

    /// Fail the `call_index`-th submission that reaches the backend (1-based).
    pub fn fail_submission_on(&mut self, call_index: u64, fault: SimFault) {
        self.faults.insert(call_index, fault);
    }

    pub fn set_hit_test(&mut self, result: SimHitTest) {
        self.hit_test = result;
    }

    /// Apply a scripted control operation. Returns false for operations the
    /// backend cannot perform itself, such as injecting human input.
    pub fn apply_control(&mut self, op: &SimOp) -> bool {
        match op {
            SimOp::HoldExternalKey { key } => self.hold_external_key(key),
            SimOp::SetReleaseFailure { fail } => self.set_release_failure(*fail),
            SimOp::FailSubmissionOn { call_index, fault } => {
                self.fail_submission_on(*call_index, *fault);
            }
            SimOp::SetHitTest { result } => self.set_hit_test(result.clone()),
            SimOp::InjectHumanInput => return false,
        }
        true
    }

    fn apply(&mut self, at_ms: u64, primitive: Primitive, cleanup: bool) {
        if let Some((token, down)) = primitive.held_token() {
            if down {
                self.physical_held.insert(token);
            } else {
                self.physical_held.remove(&token);
            }
        }
        self.events.push(AppliedInput {
            at_ms,
            primitive,
            cleanup,
        });
    }
}

impl InputBackend for SimBackend {
    fn submit(&mut self, at_ms: u64, primitive: &Primitive) -> SubmitOutcome {
        self.submission_calls += 1;
        let fault = self.faults.remove(&self.submission_calls);
        if fault == Some(SimFault::Rejected) {
            return SubmitOutcome::Rejected;
        }
        // A down may have reached the backend even when its acknowledgement was lost.
        if fault != Some(SimFault::UnknownNotApplied) {
            self.apply(at_ms, primitive.clone(), false);
        }
        if matches!(
            fault,
            Some(SimFault::UnknownApplied | SimFault::UnknownNotApplied)
        ) {
            SubmitOutcome::Unknown
        } else {
            SubmitOutcome::Applied
        }
    }

    fn release(&mut self, at_ms: u64, release: &Primitive) -> Result<(), ReleaseFailure> {
        if self.release_failure {
            return Err(ReleaseFailure);
        }
        self.apply(at_ms, release.clone(), true);
        Ok(())
    }

    fn held(&self, token: &str) -> HeldState {
        if self.physical_held.contains(token) {
            HeldState::Held
        } else {
            HeldState::NotHeld
        }
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        match &self.hit_test {
            SimHitTest::Match => HitTestOutcome::Element(request.expected_fingerprint.to_string()),
            SimHitTest::Element { fingerprint } => HitTestOutcome::Element(fingerprint.clone()),
            SimHitTest::Unresolved => HitTestOutcome::Unresolved,
        }
    }
}
