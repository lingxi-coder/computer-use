//! The execution supervisor: a single-channel state machine with virtual time.
//!
//! Every method is a trusted device event, not a remote API, and calls are
//! processed serially, so a revocation takes effect before the next call. The
//! core performs no IO and reads no clock: the shell feeds it time through
//! [`ExecutionCore::advance_to`] and collects what happened through
//! [`ExecutionCore::drain_events`].

use std::collections::{BTreeMap, VecDeque};

use interaction_contracts::desktop::device::{
    ActionIntent, GrantPermit, Observation, PermitRef, Receipt, ReceiptStatus, ReleaseStatus,
    RenewPermit, ResumeAck, ResumeCommand, RevokeExecution, RootGrant, SourceFreshness, StopAck,
    StopCommand, StopDisposition, StopScope, StopSource, SubmitIntent, SubmittedPrimitive,
    SurfaceRef,
};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::primitive::{Primitive, PrimitivePolicy};

use crate::backend::{HeldState, HitTestOutcome, HitTestRequest, InputBackend, SubmitOutcome};
use crate::latch::{ResumeError, StopLatch};
use crate::ledger::{InputLedger, Key};

/// Pending events are bounded so a shell that never drains cannot exhaust memory.
const MAX_PENDING_EVENTS: usize = 65_536;

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// Whether held-class primitives may be submitted.
    pub policy: PrimitivePolicy,
    /// Clock domains read `{prefix}/{session_epoch}/monotonic`.
    pub clock_domain_prefix: String,
    pub surface_id: String,
    /// Point bounds of the surface, when it has fixed ones.
    pub surface_bounds: Option<(i32, i32)>,
    /// Every dependency the core tracks a revision for.
    pub dependencies: Vec<String>,
    /// Dependencies every observation and permit must carry.
    pub required_dependencies: Vec<String>,
    /// Also required when a primitive depends on coordinates.
    pub geometry_dependency: String,
    /// Refuse pointer and AX primitives that come without a binding proof.
    pub require_hit_test: bool,
    /// Refuse everything until the host has established a control lease.
    pub require_lease: bool,
}

impl CoreConfig {
    /// The configuration of the I0 reference simulator.
    pub fn i0_simulation() -> Self {
        Self {
            policy: PrimitivePolicy::AllowHeld,
            clock_domain_prefix: "sim".into(),
            surface_id: "main".into(),
            surface_bounds: Some((1000, 1000)),
            dependencies: ["focus", "layer", "binding", "authorization", "geometry"]
                .map(String::from)
                .to_vec(),
            required_dependencies: ["focus", "layer", "binding", "authorization"]
                .map(String::from)
                .to_vec(),
            geometry_dependency: "geometry".into(),
            require_hit_test: false,
            require_lease: false,
        }
    }

    /// Phase 1 production: atomic primitives only, binding proofs and a lease required.
    pub fn production(clock_domain_prefix: impl Into<String>) -> Self {
        Self {
            policy: PrimitivePolicy::AtomicOnly,
            clock_domain_prefix: clock_domain_prefix.into(),
            surface_bounds: None,
            require_hit_test: true,
            require_lease: true,
            ..Self::i0_simulation()
        }
    }
}

/// A refusal with an optional machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub code: Option<FailureCode>,
    pub reason: String,
}

impl Denial {
    fn plain(reason: impl Into<String>) -> Self {
        Self {
            code: None,
            reason: reason.into(),
        }
    }

    fn coded(code: FailureCode, reason: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            reason: reason.into(),
        }
    }
}

impl From<String> for Denial {
    fn from(reason: String) -> Self {
        Self::plain(reason)
    }
}

/// Something the shell needs to forward or react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    ReceiptChanged(Receipt),
    PermitRevoked { permit: PermitRef, reason: String },
    Isolated { reason: String },
    Recovered,
    Stopped(StopAck),
    Resumed { scope: StopScope },
    LeaseLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitState {
    Issued,
    InUse,
    Revoked,
    Expired,
}

#[derive(Debug, Clone)]
struct Dependency {
    revision: u64,
    valid: Option<bool>,
}

#[derive(Debug, Clone)]
struct Task {
    revision: u64,
    root_epoch: u64,
    deadline_ms: u64,
    active: bool,
    authorized_epoch: u64,
}

#[derive(Debug, Clone)]
struct Permit {
    reference: PermitRef,
    session_epoch: u64,
    task_id: String,
    task_revision: u64,
    root_epoch: u64,
    surface: SurfaceRef,
    target_ref: String,
    evidence_id: String,
    dependencies: BTreeMap<String, u64>,
    valid_until_ms: u64,
    revoked: bool,
}

#[derive(Debug, Clone)]
struct Record {
    intent: ActionIntent,
    receipt: Receipt,
    started_ms: Option<u64>,
    next_step: usize,
}

/// No wall clock, threads, sleeps, OS input, network or model execution.
pub struct ExecutionCore<B: InputBackend> {
    config: CoreConfig,
    backend: B,
    now_ms: u64,
    session_epoch: u64,
    surface: SurfaceRef,
    dependencies: BTreeMap<String, Dependency>,
    tasks: BTreeMap<String, Task>,
    observations: BTreeMap<String, Observation>,
    last_source_sequence: Option<u64>,
    last_observed_ms: Option<u64>,
    source_verified: bool,
    permits: BTreeMap<String, Permit>,
    next_permit: u64,
    records: BTreeMap<Key, Record>,
    queue: VecDeque<Key>,
    active: Option<Key>,
    ledger: InputLedger,
    isolated: bool,
    connected: bool,
    latch: StopLatch,
    lease_until: Option<u64>,
    lease_lost: bool,
    events: Vec<CoreEvent>,
}

impl<B: InputBackend> ExecutionCore<B> {
    pub fn new(config: CoreConfig, backend: B) -> Self {
        let dependencies = config
            .dependencies
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    Dependency {
                        revision: 1,
                        valid: Some(true),
                    },
                )
            })
            .collect();
        let surface = SurfaceRef {
            id: config.surface_id.clone(),
            epoch: 1,
        };
        Self {
            config,
            backend,
            now_ms: 0,
            session_epoch: 1,
            surface,
            dependencies,
            tasks: BTreeMap::new(),
            observations: BTreeMap::new(),
            last_source_sequence: None,
            last_observed_ms: None,
            source_verified: false,
            permits: BTreeMap::new(),
            next_permit: 0,
            records: BTreeMap::new(),
            queue: VecDeque::new(),
            active: None,
            ledger: InputLedger::default(),
            isolated: false,
            connected: true,
            latch: StopLatch::new(),
            lease_until: None,
            lease_lost: false,
            events: Vec::new(),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    pub fn latch(&self) -> &StopLatch {
        &self.latch
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn session_epoch(&self) -> u64 {
        self.session_epoch
    }

    pub fn clock_domain(&self) -> String {
        format!(
            "{}/{}/monotonic",
            self.config.clock_domain_prefix, self.session_epoch
        )
    }

    pub fn surface(&self) -> SurfaceRef {
        self.surface.clone()
    }

    pub fn is_isolated(&self) -> bool {
        self.isolated
    }

    pub fn held_input_count(&self) -> usize {
        self.ledger.len()
    }

    pub fn dependency_versions(&self) -> BTreeMap<String, u64> {
        self.dependencies
            .iter()
            .map(|(key, value)| (key.clone(), value.revision))
            .collect()
    }

    /// Everything that happened since the last call, in order.
    pub fn drain_events(&mut self) -> Vec<CoreEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn permit_state(&self, id: &str) -> Option<PermitState> {
        let permit = self.permits.get(id)?;
        Some(if permit.revoked {
            PermitState::Revoked
        } else if permit.valid_until_ms <= self.now_ms {
            PermitState::Expired
        } else if self.records.values().any(|record| {
            record.intent.permit.id == id && record.receipt.status == ReceiptStatus::Running
        }) {
            PermitState::InUse
        } else {
            PermitState::Issued
        })
    }

    pub fn register_root(&mut self, grant: &RootGrant) -> Result<(), String> {
        if grant.root_id.is_empty()
            || grant.revision == 0
            || grant.root_epoch == 0
            || grant.deadline_ms <= self.now_ms
        {
            return Err("invalid task identity, revision, or deadline".into());
        }
        if self.latch.is_cancelled(&grant.root_id) {
            return Err("root was cancelled; use a new explicit root task".into());
        }
        if let Some(task) = self.tasks.get_mut(&grant.root_id) {
            if task.revision == grant.revision
                && task.root_epoch == grant.root_epoch
                && task.deadline_ms == grant.deadline_ms
                && task.active
            {
                task.authorized_epoch = self.session_epoch;
                return Ok(());
            }
            // A task cannot gain time through an automatic retry or re-registration.
            return Err("task already registered; use a new explicit root task".into());
        }
        self.tasks.insert(
            grant.root_id.clone(),
            Task {
                revision: grant.revision,
                root_epoch: grant.root_epoch,
                deadline_ms: grant.deadline_ms,
                active: true,
                authorized_epoch: self.session_epoch,
            },
        );
        Ok(())
    }

    pub fn observe(&mut self, observation: Observation) -> Result<(), String> {
        observation.validate()?;
        if observation.session_epoch != self.session_epoch || observation.surface != self.surface {
            return Err("observation context mismatch".into());
        }
        if observation.clock_domain != self.clock_domain() || observation.observed_ms > self.now_ms
        {
            return Err("observation clock mismatch or future observation".into());
        }
        if let Some(existing) = self.observations.get(&observation.observation_id) {
            return if existing == &observation {
                Ok(())
            } else {
                Err("observation ID conflict".into())
            };
        }
        if self
            .last_source_sequence
            .is_some_and(|value| observation.source_sequence <= value)
            || self
                .last_observed_ms
                .is_some_and(|value| observation.observed_ms < value)
        {
            return Err("out-of-order observation".into());
        }
        self.last_source_sequence = Some(observation.source_sequence);
        self.last_observed_ms = Some(observation.observed_ms);
        self.source_verified = observation.freshness == SourceFreshness::Verified;
        self.observations
            .insert(observation.observation_id.clone(), observation);
        self.enforce_guards();
        Ok(())
    }

    pub fn grant_permit(&mut self, request: &GrantPermit) -> Result<PermitRef, Denial> {
        self.check_latch(request.acked_stop_seq, &request.task_id)?;
        self.check_channel()?;
        let task = self
            .tasks
            .get(&request.task_id)
            .ok_or_else(|| Denial::plain("unknown root task"))?;
        if !task.active
            || task.authorized_epoch != self.session_epoch
            || task.deadline_ms <= self.now_ms
            || request.target_ref.is_empty()
        {
            return Err(Denial::plain("task inactive or target missing"));
        }
        let observation = self
            .observations
            .get(&request.evidence_id)
            .ok_or_else(|| Denial::plain("unknown evidence"))?;
        self.check_observation(observation, &request.target_ref)?;
        self.check_dependencies(&observation.dependencies, false)?;
        if request.valid_until_ms <= self.now_ms
            || request.valid_until_ms > task.deadline_ms
            || request.valid_until_ms > observation.valid_until_ms
        {
            return Err(Denial::plain(
                "permit deadline exceeds task or evidence window",
            ));
        }
        self.next_permit = self
            .next_permit
            .checked_add(1)
            .ok_or_else(|| Denial::plain("permit generation overflow"))?;
        let reference = PermitRef {
            id: format!("permit-{}", self.next_permit),
            generation: self.next_permit,
        };
        let permit = Permit {
            reference: reference.clone(),
            session_epoch: self.session_epoch,
            task_id: request.task_id.clone(),
            task_revision: task.revision,
            root_epoch: task.root_epoch,
            surface: self.surface.clone(),
            target_ref: request.target_ref.clone(),
            evidence_id: request.evidence_id.clone(),
            dependencies: observation.dependencies.clone(),
            valid_until_ms: request.valid_until_ms,
            revoked: false,
        };
        self.permits.insert(reference.id.clone(), permit);
        Ok(reference)
    }

    pub fn submit(&mut self, request: SubmitIntent) -> Receipt {
        let SubmitIntent {
            acked_stop_seq,
            intent,
        } = request;
        let key = (intent.session_epoch, intent.intent_id.clone());
        if let Some(record) = self.records.get(&key) {
            return if record.intent == intent {
                record.receipt.clone()
            } else {
                self.rejected(&intent, "intent ID payload conflict", None)
            };
        }
        let validation = intent
            .validate()
            .map_err(Denial::plain)
            .and_then(|()| self.check_policy(&intent))
            .and_then(|()| self.check_latch(acked_stop_seq, &intent.task_id))
            .and_then(|()| self.check_intent(&intent, None).map_err(Denial::plain));
        let receipt = match validation {
            Ok(()) => Receipt {
                intent_id: intent.intent_id.clone(),
                attempt_id: intent.attempt_id.clone(),
                session_epoch: intent.session_epoch,
                status: ReceiptStatus::Accepted,
                reason_code: None,
                reason: None,
                submitted: vec![],
                release_status: ReleaseStatus::NotNeeded,
                command_end_ms: None,
                at_ms: self.now_ms,
            },
            Err(denial) => self.rejected(&intent, &denial.reason, denial.code),
        };
        if receipt.status == ReceiptStatus::Accepted {
            self.queue.push_back(key.clone());
        }
        self.push_event(CoreEvent::ReceiptChanged(receipt.clone()));
        self.records.insert(
            key,
            Record {
                intent,
                receipt: receipt.clone(),
                started_ms: None,
                next_step: 0,
            },
        );
        receipt
    }

    /// Does not advance time. Resource contention leaves the next intent queued.
    pub fn dispatch_next(&mut self) -> Option<Receipt> {
        self.enforce_guards();
        if self.active.is_some() || self.isolated {
            return None;
        }
        while let Some(key) = self.queue.pop_front() {
            let record = self.records.get(&key)?;
            if record.receipt.status != ReceiptStatus::Accepted {
                continue;
            }
            let intent = record.intent.clone();
            if let Err(reason) = self.check_intent(&intent, None) {
                self.finish(&key, ReceiptStatus::Rejected, reason, None);
                return self.query_receipt(key.0, &key.1);
            }
            let record = self.records.get_mut(&key).expect("queued record");
            record.started_ms = Some(self.now_ms);
            record.receipt.command_end_ms = Some(self.now_ms + intent.duration_ms);
            record.receipt.status = ReceiptStatus::Running;
            record.receipt.at_ms = self.now_ms;
            let running = record.receipt.clone();
            self.active = Some(key.clone());
            self.push_event(CoreEvent::ReceiptChanged(running));
            self.run_due_steps();
            return self.query_receipt(key.0, &key.1);
        }
        None
    }

    /// Visit every scheduled input, deadline and command end, even on large jumps.
    pub fn advance_to(&mut self, target_ms: u64) -> Result<(), String> {
        if target_ms < self.now_ms {
            return Err("virtual clock cannot move backwards".into());
        }
        while self.now_ms < target_ms {
            let next = self
                .next_wakeup()
                .filter(|time| *time <= target_ms)
                .unwrap_or(target_ms);
            self.now_ms = next;
            self.complete_ended_action();
            self.enforce_guards();
            self.run_due_steps();
        }
        self.complete_ended_action();
        self.enforce_guards();
        Ok(())
    }

    pub fn query_receipt(&self, epoch: u64, id: &str) -> Option<Receipt> {
        self.records
            .get(&(epoch, id.into()))
            .map(|record| record.receipt.clone())
    }

    pub fn update_dependency(&mut self, name: &str, valid: Option<bool>) -> u64 {
        let entry = self.dependencies.entry(name.into()).or_insert(Dependency {
            revision: 0,
            valid: None,
        });
        entry.revision = entry
            .revision
            .checked_add(1)
            .expect("dependency revision exhausted");
        entry.valid = valid;
        let revision = entry.revision;
        self.enforce_guards();
        revision
    }

    pub fn renew(&mut self, request: RenewPermit) -> Result<(), String> {
        self.enforce_guards();
        self.check_channel()?;
        if request.session_epoch != self.session_epoch {
            return Err("stale renewal session".into());
        }
        let permit = self
            .permits
            .get(&request.permit.id)
            .ok_or("unknown permit")?;
        self.check_permit(permit, &request.permit)?;
        let observation = self
            .observations
            .get(&request.evidence_id)
            .ok_or("unknown renewal evidence")?;
        self.check_observation(observation, &permit.target_ref)?;
        if observation.dependencies != permit.dependencies {
            return Err("renewal dependencies changed".into());
        }
        let old = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("missing prior evidence")?;
        if request.valid_until_ms > permit.valid_until_ms
            && (observation.source_sequence <= old.source_sequence
                || observation.observed_ms <= old.observed_ms)
        {
            return Err("renewal extension requires genuinely newer evidence".into());
        }
        let task = self.tasks.get(&permit.task_id).ok_or("unknown task")?;
        if request.valid_until_ms <= self.now_ms
            || request.valid_until_ms > task.deadline_ms
            || request.valid_until_ms > observation.valid_until_ms
        {
            return Err("renewal deadline exceeds authority".into());
        }
        let permit = self
            .permits
            .get_mut(&request.permit.id)
            .expect("checked permit");
        permit.evidence_id = request.evidence_id;
        permit.valid_until_ms = request.valid_until_ms;
        // Deliberately never change Record.command_end_ms.
        self.enforce_guards();
        Ok(())
    }

    pub fn revoke(&mut self, request: RevokeExecution) -> Result<(), String> {
        if request.session_epoch != self.session_epoch {
            return Err("stale revocation session".into());
        }
        let permit = self
            .permits
            .get(&request.permit.id)
            .ok_or("unknown permit")?;
        if permit.reference != request.permit {
            return Err("permit generation mismatch".into());
        }
        self.invalidate_permit(&request.permit.id, &request.reason, None);
        Ok(())
    }

    /// Tombstone a root and release everything it had.
    pub fn cancel_root(&mut self, id: &str) -> Result<(), String> {
        self.tasks.get_mut(id).ok_or("unknown task")?.active = false;
        self.latch.tombstone(id);
        self.enforce_guards();
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.enforce_guards();
    }

    pub fn reconnect(&mut self) -> u64 {
        self.disconnect();
        self.session_epoch = self
            .session_epoch
            .checked_add(1)
            .expect("session epoch exhausted");
        self.surface.epoch = self
            .surface
            .epoch
            .checked_add(1)
            .expect("surface epoch exhausted");
        // Preserve root deadlines and cancellation tombstones. Re-authorization
        // must explicitly use the original revision and deadline in the new epoch.
        self.observations.clear();
        self.last_source_sequence = None;
        self.last_observed_ms = None;
        self.source_verified = false;
        self.lease_until = None;
        self.lease_lost = false;
        self.connected = true;
        self.session_epoch
    }

    /// A trusted recovery action; revoked permits remain revoked afterward.
    pub fn recover_channel(&mut self) -> Result<(), String> {
        for key in self.ledger.record_keys() {
            self.cleanup(&key);
        }
        if !self.ledger.is_empty() {
            return Err("input release still unconfirmed".into());
        }
        if self.isolated {
            self.isolated = false;
            self.push_event(CoreEvent::Recovered);
        }
        Ok(())
    }

    /// The host keeps its control lease alive with this.
    pub fn renew_lease(&mut self, valid_for_ms: u64) {
        self.lease_until = Some(self.now_ms.saturating_add(valid_for_ms));
        self.lease_lost = false;
    }

    /// Latch a stop, revoke what is in scope and release held input. Nothing
    /// here waits for the host, a model or a verification result.
    pub fn stop(&mut self, command: &StopCommand) -> StopAck {
        let roots = self.roots_in_scope(&command.scope);
        let owed_before = self.owned_in_scope(&roots);
        let seq = self.latch.latch(command.disposition, &roots);
        let code = if command.source == StopSource::HumanInput {
            FailureCode::HumanInputDetected
        } else {
            FailureCode::Cancelled
        };
        let permit_ids: Vec<String> = self
            .permits
            .iter()
            .filter(|(_, permit)| !permit.revoked && roots.contains(&permit.task_id))
            .map(|(id, _)| id.clone())
            .collect();
        let revoked_permits = permit_ids.len() as u32;
        for id in permit_ids {
            self.invalidate_permit(&id, &format!("stop: {}", command.reason), Some(code));
        }
        if command.disposition == StopDisposition::Cancel {
            for root in &roots {
                if let Some(task) = self.tasks.get_mut(root) {
                    task.active = false;
                }
            }
        }
        self.enforce_guards();
        let cleanup = if owed_before == 0 {
            ReleaseStatus::NotNeeded
        } else if self.owned_in_scope(&roots) == 0 {
            ReleaseStatus::Released
        } else {
            ReleaseStatus::Failed
        };
        let ack = StopAck {
            stop_seq: seq,
            stop_floor: self.latch.floor(),
            scope: command.scope.clone(),
            disposition: command.disposition,
            revoked_permits,
            cleanup,
            at_ms: self.now_ms,
        };
        self.push_event(CoreEvent::Stopped(ack.clone()));
        ack
    }

    /// A person touched the keyboard or pointer while the system was acting.
    pub fn human_input_detected(&mut self) -> StopAck {
        self.stop(&StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Pause,
            source: StopSource::HumanInput,
            reason: "human input detected".into(),
        })
    }

    /// Only an explicit resume lifts a pause; a cancelled root never resumes.
    pub fn resume(&mut self, command: &ResumeCommand) -> ResumeAck {
        let roots = match &command.scope {
            StopScope::All => self.latch.paused_roots(),
            StopScope::Root { root_id } => vec![root_id.clone()],
        };
        match self.latch.resume(&roots) {
            Err(ResumeError::Cancelled(root)) => ResumeAck {
                resumed: false,
                reason: Some(format!("root {root} was cancelled; start a new task")),
            },
            Ok(resumed) if resumed.is_empty() => ResumeAck {
                resumed: false,
                reason: Some("nothing was paused".into()),
            },
            Ok(_) => {
                self.push_event(CoreEvent::Resumed {
                    scope: command.scope.clone(),
                });
                ResumeAck {
                    resumed: true,
                    reason: None,
                }
            }
        }
    }

    /// The next virtual time at which the core needs to run.
    pub fn next_wakeup(&self) -> Option<u64> {
        let mut times = Vec::new();
        if let Some(record) = self.active.as_ref().and_then(|key| self.records.get(key)) {
            if let Some(end) = record.receipt.command_end_ms {
                times.push(end);
            }
            if let (Some(start), Some(step)) =
                (record.started_ms, record.intent.steps.get(record.next_step))
            {
                times.push(start + step.offset_ms);
            }
        }
        for permit in self.permits.values().filter(|permit| !permit.revoked) {
            times.push(permit.valid_until_ms);
            if let Some(task) = self.tasks.get(&permit.task_id) {
                times.push(task.deadline_ms);
            }
            if let Some(observation) = self.observations.get(&permit.evidence_id) {
                times.push(observation.valid_until_ms);
            }
        }
        for record in self
            .records
            .values()
            .filter(|record| record.receipt.status == ReceiptStatus::Accepted)
        {
            if let Some(expired) = record.intent.latest_start_ms.checked_add(1) {
                times.push(expired);
            }
        }
        if let Some(until) = self.lease_until
            && !self.lease_lost
        {
            times.push(until);
        }
        times.into_iter().filter(|time| *time > self.now_ms).min()
    }

    fn push_event(&mut self, event: CoreEvent) {
        if self.events.len() >= MAX_PENDING_EVENTS {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    fn roots_in_scope(&self, scope: &StopScope) -> Vec<String> {
        match scope {
            StopScope::All => self
                .tasks
                .iter()
                .filter(|(_, task)| task.active)
                .map(|(id, _)| id.clone())
                .collect(),
            StopScope::Root { root_id } => vec![root_id.clone()],
        }
    }

    /// Held inputs owned by records that belong to these roots.
    fn owned_in_scope(&self, roots: &[String]) -> usize {
        self.ledger
            .record_keys()
            .iter()
            .filter(|key| {
                self.records
                    .get(*key)
                    .is_some_and(|record| roots.contains(&record.intent.task_id))
            })
            .map(|key| self.ledger.tokens_of(key).len())
            .sum()
    }

    fn check_latch(&self, acked_stop_seq: u64, root: &str) -> Result<(), Denial> {
        match self.latch.check(acked_stop_seq, root).refusal(root) {
            Some((code, reason)) => Err(Denial::coded(code, reason)),
            None => Ok(()),
        }
    }

    fn check_policy(&self, intent: &ActionIntent) -> Result<(), Denial> {
        if self.config.policy == PrimitivePolicy::AtomicOnly
            && intent
                .steps
                .iter()
                .any(|step| step.primitive.is_held_class())
        {
            return Err(Denial::coded(
                FailureCode::InputRejected,
                "held-class primitives are not enabled",
            ));
        }
        Ok(())
    }

    fn check_channel(&self) -> Result<(), String> {
        if !self.connected {
            return Err("control channel disconnected".into());
        }
        if self.config.require_lease {
            match self.lease_until {
                None => return Err("control lease not established".into()),
                Some(until) if self.now_ms >= until => return Err("control lease expired".into()),
                Some(_) => {}
            }
        }
        if self.isolated {
            return Err("input channel isolated".into());
        }
        if !self.source_verified {
            return Err("source freshness unknown or failed".into());
        }
        Ok(())
    }

    fn check_observation(&self, observation: &Observation, target: &str) -> Result<(), String> {
        if observation.session_epoch != self.session_epoch
            || observation.surface != self.surface
            || observation.target_ref != target
        {
            return Err("evidence context or target mismatch".into());
        }
        if observation.clock_domain != self.clock_domain()
            || observation.observed_ms > self.now_ms
            || observation.valid_until_ms <= self.now_ms
            || observation.freshness != SourceFreshness::Verified
        {
            return Err("evidence expired, uncertain, or clock mismatch".into());
        }
        self.check_dependencies(&observation.dependencies, false)
    }

    fn check_dependencies(
        &self,
        dependencies: &BTreeMap<String, u64>,
        pointer: bool,
    ) -> Result<(), String> {
        for required in &self.config.required_dependencies {
            if !dependencies.contains_key(required) {
                return Err(format!("missing required dependency: {required}"));
            }
        }
        if pointer && !dependencies.contains_key(&self.config.geometry_dependency) {
            return Err(format!(
                "missing required dependency: {}",
                self.config.geometry_dependency
            ));
        }
        for (key, revision) in dependencies {
            let current = self
                .dependencies
                .get(key)
                .ok_or_else(|| format!("unknown dependency: {key}"))?;
            if current.valid != Some(true) || current.revision != *revision {
                return Err(format!("dependency invalid, unknown, or changed: {key}"));
            }
        }
        Ok(())
    }

    fn check_permit(&self, permit: &Permit, reference: &PermitRef) -> Result<(), String> {
        self.check_channel()?;
        if permit.reference != *reference
            || permit.session_epoch != self.session_epoch
            || permit.revoked
        {
            return Err("permit revoked, stale, or generation mismatch".into());
        }
        let task = self.tasks.get(&permit.task_id).ok_or("unknown root task")?;
        if !task.active
            || task.authorized_epoch != self.session_epoch
            || task.revision != permit.task_revision
            || task.deadline_ms <= self.now_ms
            || permit.valid_until_ms <= self.now_ms
        {
            return Err("task or permit inactive/expired".into());
        }
        self.check_dependencies(&permit.dependencies, false)?;
        let observation = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown permit evidence")?;
        self.check_observation(observation, &permit.target_ref)
    }

    fn check_intent(&self, intent: &ActionIntent, started_ms: Option<u64>) -> Result<(), String> {
        if intent.session_epoch != self.session_epoch || intent.surface != self.surface {
            return Err("intent session/surface mismatch".into());
        }
        let permit = self
            .permits
            .get(&intent.permit.id)
            .ok_or("unknown permit")?;
        self.check_permit(permit, &intent.permit)?;
        if intent.task_id != permit.task_id
            || intent.task_revision != permit.task_revision
            || intent.target_ref != permit.target_ref
            || intent.surface != permit.surface
            || intent.dependencies != permit.dependencies
        {
            return Err("intent does not match granted permit scope".into());
        }
        if intent.root_epoch != permit.root_epoch {
            return Err("intent root epoch does not match the permit".into());
        }
        let pointer = intent
            .steps
            .iter()
            .any(|step| step.primitive.is_pointer_class());
        self.check_dependencies(&intent.dependencies, pointer)?;
        if let Some((width, height)) = self.config.surface_bounds {
            for step in &intent.steps {
                let point = match step.primitive {
                    Primitive::PointerMove { x, y } | Primitive::Click { x, y, .. } => Some((x, y)),
                    _ => None,
                };
                if let Some((x, y)) = point
                    && (!(0..width).contains(&x) || !(0..height).contains(&y))
                {
                    return Err(format!("point outside simulated {width}x{height} surface"));
                }
            }
        }
        if started_ms.is_none() && self.now_ms > intent.latest_start_ms {
            return Err("latest start deadline missed".into());
        }
        // An original evidence reference is immutable provenance, never arbitrary text.
        let original = self
            .observations
            .get(&intent.evidence_id)
            .ok_or("unknown issue evidence")?;
        if original.session_epoch != intent.session_epoch
            || original.surface != intent.surface
            || original.target_ref != intent.target_ref
            || original.dependencies != intent.dependencies
            || original.freshness != SourceFreshness::Verified
        {
            return Err("issue evidence mismatch".into());
        }
        let end = started_ms
            .unwrap_or(self.now_ms)
            .checked_add(intent.duration_ms)
            .ok_or("command end overflow")?;
        let evidence = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown current evidence")?;
        let task = self.tasks.get(&intent.task_id).ok_or("unknown task")?;
        if end > permit.valid_until_ms || end > evidence.valid_until_ms || end > task.deadline_ms {
            return Err("command end exceeds evidence, permit, or root deadline".into());
        }
        Ok(())
    }

    fn rejected(&self, intent: &ActionIntent, reason: &str, code: Option<FailureCode>) -> Receipt {
        Receipt {
            intent_id: intent.intent_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            session_epoch: intent.session_epoch,
            status: ReceiptStatus::Rejected,
            reason_code: code,
            reason: Some(reason.into()),
            submitted: vec![],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: None,
            at_ms: self.now_ms,
        }
    }

    fn enforce_guards(&mut self) {
        if let Some(until) = self.lease_until
            && self.now_ms >= until
            && !self.lease_lost
        {
            self.lease_lost = true;
            self.push_event(CoreEvent::LeaseLost);
        }
        let invalid: Vec<(String, String)> = self
            .permits
            .iter()
            .filter(|(_, permit)| !permit.revoked)
            .filter_map(|(id, permit)| {
                self.check_permit(permit, &permit.reference)
                    .err()
                    .map(|reason| (id.clone(), reason))
            })
            .collect();
        for (id, reason) in invalid {
            self.invalidate_permit(&id, &reason, None);
        }
        let invalid_records: Vec<(Key, String)> = self
            .records
            .iter()
            .filter(|(_, record)| {
                matches!(
                    record.receipt.status,
                    ReceiptStatus::Accepted | ReceiptStatus::Running
                )
            })
            .filter_map(|(key, record)| {
                self.check_intent(&record.intent, record.started_ms)
                    .err()
                    .map(|reason| (key.clone(), reason))
            })
            .collect();
        for (key, reason) in invalid_records {
            self.finish(&key, ReceiptStatus::Cancelled, reason, None);
        }
    }

    fn invalidate_permit(&mut self, id: &str, reason: &str, code: Option<FailureCode>) {
        let Some(permit) = self.permits.get_mut(id) else {
            return;
        };
        if permit.revoked {
            return;
        }
        // Mark once before cancelling records. Their terminal handling may
        // invalidate this same permit; it must not recurse through the queue.
        permit.revoked = true;
        let revoked = permit.reference.clone();
        let affected: Vec<Key> = self
            .records
            .iter()
            .filter(|(_, record)| {
                record.intent.permit.id == id
                    && matches!(
                        record.receipt.status,
                        ReceiptStatus::Accepted | ReceiptStatus::Running
                    )
            })
            .map(|(key, _)| key.clone())
            .collect();
        self.push_event(CoreEvent::PermitRevoked {
            permit: revoked,
            reason: reason.to_string(),
        });
        for key in affected {
            self.finish(&key, ReceiptStatus::Cancelled, reason.into(), code);
        }
    }

    fn finish(
        &mut self,
        key: &Key,
        status: ReceiptStatus,
        reason: String,
        code: Option<FailureCode>,
    ) {
        let Some(record) = self.records.get(key) else {
            return;
        };
        if !matches!(
            record.receipt.status,
            ReceiptStatus::Accepted | ReceiptStatus::Running
        ) {
            return;
        }
        let permit_id = record.intent.permit.id.clone();
        let release_ok = self.cleanup(key);
        if let Some(record) = self.records.get_mut(key) {
            record.receipt.status = if release_ok {
                status
            } else {
                ReceiptStatus::Unknown
            };
            record.receipt.reason = Some(if release_ok {
                reason
            } else {
                format!("{reason}; release failed")
            });
            record.receipt.reason_code = if release_ok {
                code
            } else {
                Some(FailureCode::ReleaseFailed)
            };
            record.receipt.at_ms = self.now_ms;
            let receipt = record.receipt.clone();
            self.push_event(CoreEvent::ReceiptChanged(receipt));
        }
        self.queue.retain(|queued| queued != key);
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
        if !release_ok {
            // All intents share the physical input channel. A failed release
            // makes every old permit on that channel unusable after recovery.
            let ids: Vec<String> = self.permits.keys().cloned().collect();
            for id in ids {
                self.invalidate_permit(&id, "input channel isolated after release failure", None);
            }
        } else if status != ReceiptStatus::Completed {
            self.invalidate_permit(
                &permit_id,
                "attempt ended without confirmed normal completion",
                None,
            );
        }
    }

    fn cleanup(&mut self, key: &Key) -> bool {
        let tokens = self.ledger.tokens_of(key);
        if tokens.is_empty() {
            return true;
        }
        for token in tokens {
            let release = self
                .ledger
                .release_of(&token)
                .expect("a listed token is owned");
            if self.backend.release(self.now_ms, &release).is_err() {
                if !self.isolated {
                    self.isolated = true;
                    self.push_event(CoreEvent::Isolated {
                        reason: "input release could not be confirmed".into(),
                    });
                }
                if let Some(record) = self.records.get_mut(key) {
                    record.receipt.release_status = ReleaseStatus::Failed;
                }
                return false;
            }
            self.ledger.take(&token);
            if let Some(record) = self.records.get_mut(key) {
                record.receipt.submitted.push(SubmittedPrimitive {
                    at_ms: self.now_ms,
                    primitive: release,
                    cleanup: true,
                });
                record.receipt.release_status = ReleaseStatus::Released;
                record.receipt.at_ms = self.now_ms;
            }
        }
        true
    }

    fn complete_ended_action(&mut self) {
        if let Some(key) = self.active.clone()
            && self
                .records
                .get(&key)
                .and_then(|record| record.receipt.command_end_ms)
                .is_some_and(|end| end <= self.now_ms)
        {
            self.finish(
                &key,
                ReceiptStatus::Completed,
                "bounded input ended; effect not verified".into(),
                None,
            );
        }
    }

    fn run_due_steps(&mut self) {
        let Some(key) = self.active.clone() else {
            return;
        };
        loop {
            let record = self.records.get(&key).expect("active record");
            if record.receipt.status != ReceiptStatus::Running {
                return;
            }
            let Some(step) = record.intent.steps.get(record.next_step).cloned() else {
                return;
            };
            let start = record.started_ms.expect("running start");
            if start + step.offset_ms > self.now_ms {
                return;
            }
            if let Err(reason) = self.check_intent(&record.intent, Some(start)) {
                self.finish(&key, ReceiptStatus::Cancelled, reason, None);
                return;
            }
            if let Err(reason) = self.check_ownership(&key, &step.primitive) {
                self.reject_step(&key, reason, Some(FailureCode::InputRejected));
                return;
            }
            if step.primitive.needs_hit_test() {
                let intent = self.records[&key].intent.clone();
                if let Err((code, reason)) = self.pre_submit_hit_test(&intent, &step.primitive) {
                    self.reject_step(&key, reason, Some(code));
                    return;
                }
            }
            // The ledger owes the release from before the down is submitted, so
            // a lost acknowledgement still leaves the input accounted for.
            self.ledger.track_down(&key, &step.primitive);
            let outcome = self.backend.submit(self.now_ms, &step.primitive);
            match outcome {
                SubmitOutcome::Rejected => {
                    self.ledger.untrack_down(&key, &step.primitive);
                    self.reject_step(
                        &key,
                        "backend rejected input".into(),
                        Some(FailureCode::InputRejected),
                    );
                    return;
                }
                SubmitOutcome::Unknown => {
                    self.finish(
                        &key,
                        ReceiptStatus::Unknown,
                        "backend submission outcome unknown".into(),
                        Some(FailureCode::EffectUnknown),
                    );
                    return;
                }
                SubmitOutcome::Applied => {}
            }
            self.ledger.confirm_up(&key, &step.primitive);
            let held = self.ledger.holds(&key);
            let record = self.records.get_mut(&key).expect("active record");
            record.next_step += 1;
            record.receipt.submitted.push(SubmittedPrimitive {
                at_ms: self.now_ms,
                primitive: step.primitive,
                cleanup: false,
            });
            // Atomic primitives leave nothing behind, so they never owe a release.
            record.receipt.release_status = if held {
                ReleaseStatus::Held
            } else if record
                .receipt
                .submitted
                .iter()
                .any(|entry| entry.primitive.is_held_class())
            {
                ReleaseStatus::Released
            } else {
                ReleaseStatus::NotNeeded
            };
            record.receipt.at_ms = self.now_ms;
        }
    }

    /// A step that cannot be submitted ends the action as rejected, or as
    /// partial when earlier steps already went out.
    fn reject_step(&mut self, key: &Key, reason: String, code: Option<FailureCode>) {
        let partial = !self.records[key].receipt.submitted.is_empty();
        let status = if partial {
            ReceiptStatus::Partial
        } else {
            ReceiptStatus::Rejected
        };
        self.finish(key, status, reason, code);
    }

    fn check_ownership(&self, key: &Key, primitive: &Primitive) -> Result<(), String> {
        if let Some((token, down)) = primitive.held_token() {
            if down && self.backend.held(&token) != HeldState::NotHeld {
                return Err("input already held, including external input".into());
            }
            if !down && self.ledger.owner_of(&token) != Some(key) {
                return Err("cannot release unowned input".into());
            }
        }
        Ok(())
    }

    /// Compare what is under the pointer with the instance the action was
    /// admitted for. Anything short of a match refuses the submission.
    fn pre_submit_hit_test(
        &mut self,
        intent: &ActionIntent,
        primitive: &Primitive,
    ) -> Result<(), (FailureCode, String)> {
        let Some(expected) = &intent.binding_fingerprint else {
            return if self.config.require_hit_test {
                Err((
                    FailureCode::ContextUnresolved,
                    "no binding proof for a pointer or AX action".into(),
                ))
            } else {
                Ok(())
            };
        };
        let outcome = self.backend.hit_test(&HitTestRequest {
            target_ref: &intent.target_ref,
            expected_fingerprint: expected,
            primitive,
        });
        match outcome {
            HitTestOutcome::Element(found) if found == *expected => Ok(()),
            HitTestOutcome::Element(_) => Err((
                FailureCode::TargetOccluded,
                "target is covered or replaced under the pointer".into(),
            )),
            HitTestOutcome::Unresolved => Err((
                FailureCode::ContextUnresolved,
                "hit test could not resolve the point".into(),
            )),
        }
    }
}
