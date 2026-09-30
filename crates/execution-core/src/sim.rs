//! The I0 reference executor, as a facade over [`ExecutionCore`].
//!
//! It keeps the original method signatures and I0 types so the I0 regression
//! tests and the demo still run, unchanged, against the production state
//! machine. Everything it does is a conversion: the behaviour lives in the core.
//!
//! A single-device, single-input-channel executor with virtual time and no
//! wall clock, threads, OS input, network or model execution. Public mutation
//! methods are trusted harness or device events, not remote APIs.

use std::collections::BTreeMap;

use interaction_contracts::desktop::device::{self as desktop, SimFault};
use interaction_contracts::i0::{
    ActionIntent, Input, Message, Observation, PermitRef, Receipt, RenewPermit, RevokeExecution,
    SurfaceRef, WireMessage,
};

use crate::core::{CoreConfig, ExecutionCore};
use crate::sim_backend::SimBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEvent {
    pub at_ms: u64,
    pub input: Input,
    pub cleanup: bool,
}

/// Unknown outcomes deliberately distinguish hidden backend truth from receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionFault {
    Rejected,
    UnknownApplied,
    UnknownNotApplied,
}

impl From<SubmissionFault> for SimFault {
    fn from(fault: SubmissionFault) -> Self {
        match fault {
            SubmissionFault::Rejected => Self::Rejected,
            SubmissionFault::UnknownApplied => Self::UnknownApplied,
            SubmissionFault::UnknownNotApplied => Self::UnknownNotApplied,
        }
    }
}

pub struct Simulator {
    core: ExecutionCore<SimBackend>,
    events: Vec<BackendEvent>,
}

impl Default for Simulator {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_i0(message: Message) -> Result<(), String> {
    WireMessage {
        protocol_version: 1,
        message,
    }
    .validate()
}

fn to_i0(receipt: desktop::Receipt) -> Receipt {
    Receipt::try_from(receipt).expect("the I0 facade only ever submits I0 inputs")
}

impl Simulator {
    pub fn new() -> Self {
        Self {
            core: ExecutionCore::new(CoreConfig::i0_simulation(), SimBackend::new()),
            events: Vec::new(),
        }
    }

    /// Mirror new backend inputs as I0 events and drop the core's own event
    /// queue, which I0 callers never read.
    fn sync(&mut self) {
        self.core.drain_events();
        let seen = self.events.len();
        for applied in &self.core.backend().events()[seen..] {
            self.events.push(BackendEvent {
                at_ms: applied.at_ms,
                input: Input::try_from(applied.primitive.clone())
                    .expect("the I0 facade only ever submits I0 inputs"),
                cleanup: applied.cleanup,
            });
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.core.now_ms()
    }

    pub fn session_epoch(&self) -> u64 {
        self.core.session_epoch()
    }

    pub fn clock_domain(&self) -> String {
        self.core.clock_domain()
    }

    pub fn surface(&self) -> SurfaceRef {
        self.core.surface().into()
    }

    pub fn backend_events(&self) -> &[BackendEvent] {
        &self.events
    }

    pub fn held_inputs(&self) -> Vec<String> {
        self.core.backend().held_tokens()
    }

    pub fn is_isolated(&self) -> bool {
        self.core.is_isolated()
    }

    pub fn dependency_versions(&self) -> BTreeMap<String, u64> {
        self.core.dependency_versions()
    }

    pub fn register_task(
        &mut self,
        id: &str,
        revision: u64,
        deadline_ms: u64,
    ) -> Result<(), String> {
        self.core.register_root(&desktop::RootGrant {
            root_id: id.into(),
            revision,
            root_epoch: 1,
            deadline_ms,
        })
    }

    pub fn observe(&mut self, observation: Observation) -> Result<(), String> {
        validate_i0(Message::Observation(observation.clone()))?;
        let result = self.core.observe(observation.into());
        self.sync();
        result
    }

    pub fn grant_permit(
        &mut self,
        task_id: &str,
        target_ref: &str,
        evidence_id: &str,
        valid_until_ms: u64,
    ) -> Result<PermitRef, String> {
        self.core
            .grant_permit(&desktop::GrantPermit {
                acked_stop_seq: 0,
                task_id: task_id.into(),
                target_ref: target_ref.into(),
                evidence_id: evidence_id.into(),
                valid_until_ms,
            })
            .map(PermitRef::from)
            .map_err(|denial| denial.reason)
    }

    pub fn submit(&mut self, intent: ActionIntent) -> Receipt {
        let receipt = self.core.submit(intent.into());
        self.sync();
        to_i0(receipt)
    }

    /// Does not advance time. Resource contention leaves the next intent queued.
    pub fn dispatch_next(&mut self) -> Option<Receipt> {
        let receipt = self.core.dispatch_next();
        self.sync();
        receipt.map(to_i0)
    }

    /// Visit every scheduled input, deadline and command end, even on large jumps.
    pub fn advance_to(&mut self, target_ms: u64) -> Result<(), String> {
        let result = self.core.advance_to(target_ms);
        self.sync();
        result
    }

    pub fn query_receipt(&self, epoch: u64, id: &str) -> Option<Receipt> {
        self.core.query_receipt(epoch, id).map(to_i0)
    }

    pub fn update_dependency(&mut self, name: &str, valid: Option<bool>) -> u64 {
        let revision = self.core.update_dependency(name, valid);
        self.sync();
        revision
    }

    pub fn renew(&mut self, request: RenewPermit) -> Result<(), String> {
        validate_i0(Message::RenewPermit(request.clone()))?;
        let result = self.core.renew(request.into());
        self.sync();
        result
    }

    pub fn revoke(&mut self, request: RevokeExecution) -> Result<(), String> {
        validate_i0(Message::RevokeExecution(request.clone()))?;
        let result = self.core.revoke(request.into());
        self.sync();
        result
    }

    pub fn cancel_task(&mut self, id: &str) -> Result<(), String> {
        let result = self.core.cancel_root(id);
        self.sync();
        result
    }

    pub fn disconnect(&mut self) {
        self.core.disconnect();
        self.sync();
    }

    pub fn reconnect(&mut self) -> u64 {
        let epoch = self.core.reconnect();
        self.sync();
        epoch
    }

    pub fn hold_external_key(&mut self, key: &str) {
        self.core.backend_mut().hold_external_key(key);
    }

    pub fn set_release_failure(&mut self, fail: bool) {
        self.core.backend_mut().set_release_failure(fail);
    }

    pub fn fail_submission_on(&mut self, call_index: usize, fault: SubmissionFault) {
        self.core
            .backend_mut()
            .fail_submission_on(call_index as u64, fault.into());
    }

    /// A trusted recovery action; revoked permits remain revoked afterward.
    pub fn recover_channel(&mut self) -> Result<(), String> {
        let result = self.core.recover_channel();
        self.sync();
        result
    }
}
