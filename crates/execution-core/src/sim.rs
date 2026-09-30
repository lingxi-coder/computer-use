//! A single-device, single-input-channel reference executor with virtual time.
//!
//! Public mutation methods are trusted harness/device events, not remote APIs.
//! Calls are processed serially: revocation takes effect before subsequent calls.

use interaction_contracts::i0::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type Key = (u64, String);

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

#[derive(Debug, Clone)]
struct Dependency {
    revision: u64,
    valid: Option<bool>,
}

#[derive(Debug, Clone)]
struct Task {
    revision: u64,
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

/// The ledger includes potentially submitted downs after uncertain backend calls.
#[derive(Debug, Clone)]
struct OwnedInput {
    record: Key,
    release: Input,
}

/// No wall clock, threads, sleeps, OS input, network, or model execution.
pub struct Simulator {
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
    owned: BTreeMap<String, OwnedInput>,
    physical_held: BTreeSet<String>,
    events: Vec<BackendEvent>,
    faults: BTreeMap<usize, SubmissionFault>,
    submission_calls: usize,
    release_failure: bool,
    isolated: bool,
    connected: bool,
}

impl Default for Simulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulator {
    pub fn new() -> Self {
        Self {
            now_ms: 0,
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            dependencies: ["focus", "layer", "binding", "authorization", "geometry"]
                .into_iter()
                .map(|name| {
                    (
                        name.into(),
                        Dependency {
                            revision: 1,
                            valid: Some(true),
                        },
                    )
                })
                .collect(),
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
            owned: BTreeMap::new(),
            physical_held: BTreeSet::new(),
            events: Vec::new(),
            faults: BTreeMap::new(),
            submission_calls: 0,
            release_failure: false,
            isolated: false,
            connected: true,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }
    pub fn session_epoch(&self) -> u64 {
        self.session_epoch
    }
    pub fn clock_domain(&self) -> String {
        format!("sim/{}/monotonic", self.session_epoch)
    }
    pub fn surface(&self) -> SurfaceRef {
        self.surface.clone()
    }
    pub fn backend_events(&self) -> &[BackendEvent] {
        &self.events
    }
    pub fn held_inputs(&self) -> Vec<String> {
        self.physical_held.iter().cloned().collect()
    }
    pub fn is_isolated(&self) -> bool {
        self.isolated
    }

    pub fn dependency_versions(&self) -> BTreeMap<String, u64> {
        self.dependencies
            .iter()
            .map(|(key, value)| (key.clone(), value.revision))
            .collect()
    }

    pub fn register_task(
        &mut self,
        id: &str,
        revision: u64,
        deadline_ms: u64,
    ) -> Result<(), String> {
        if id.is_empty() || revision == 0 || deadline_ms <= self.now_ms {
            return Err("invalid task identity, revision, or deadline".into());
        }
        if let Some(task) = self.tasks.get_mut(id) {
            if task.revision == revision && task.deadline_ms == deadline_ms && task.active {
                task.authorized_epoch = self.session_epoch;
                return Ok(());
            }
            // A task cannot gain time through an automatic retry or re-registration.
            return Err("task already registered; use a new explicit root task".into());
        }
        self.tasks.insert(
            id.into(),
            Task {
                revision,
                deadline_ms,
                active: true,
                authorized_epoch: self.session_epoch,
            },
        );
        Ok(())
    }

    pub fn observe(&mut self, observation: Observation) -> Result<(), String> {
        WireMessage {
            protocol_version: 1,
            message: Message::Observation(observation.clone()),
        }
        .validate()?;
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

    pub fn grant_permit(
        &mut self,
        task_id: &str,
        target_ref: &str,
        evidence_id: &str,
        valid_until_ms: u64,
    ) -> Result<PermitRef, String> {
        self.check_channel()?;
        let task = self.tasks.get(task_id).ok_or("unknown root task")?;
        if !task.active
            || task.authorized_epoch != self.session_epoch
            || task.deadline_ms <= self.now_ms
            || target_ref.is_empty()
        {
            return Err("task inactive or target missing".into());
        }
        let observation = self
            .observations
            .get(evidence_id)
            .ok_or("unknown evidence")?;
        self.check_observation(observation, target_ref)?;
        self.check_dependencies(&observation.dependencies, false)?;
        if valid_until_ms <= self.now_ms
            || valid_until_ms > task.deadline_ms
            || valid_until_ms > observation.valid_until_ms
        {
            return Err("permit deadline exceeds task or evidence window".into());
        }
        self.next_permit = self
            .next_permit
            .checked_add(1)
            .ok_or("permit generation overflow")?;
        let reference = PermitRef {
            id: format!("permit-{}", self.next_permit),
            generation: self.next_permit,
        };
        self.permits.insert(
            reference.id.clone(),
            Permit {
                reference: reference.clone(),
                session_epoch: self.session_epoch,
                task_id: task_id.into(),
                task_revision: task.revision,
                surface: self.surface.clone(),
                target_ref: target_ref.into(),
                evidence_id: evidence_id.into(),
                dependencies: observation.dependencies.clone(),
                valid_until_ms,
                revoked: false,
            },
        );
        Ok(reference)
    }

    pub fn submit(&mut self, intent: ActionIntent) -> Receipt {
        let key = (intent.session_epoch, intent.intent_id.clone());
        if let Some(record) = self.records.get(&key) {
            return if record.intent == intent {
                record.receipt.clone()
            } else {
                self.rejected(&intent, "intent ID payload conflict")
            };
        }
        let validation = WireMessage {
            protocol_version: 1,
            message: Message::ActionIntent(intent.clone()),
        }
        .validate()
        .and_then(|_| self.check_intent(&intent, None));
        let receipt = match validation {
            Ok(()) => Receipt {
                intent_id: intent.intent_id.clone(),
                session_epoch: intent.session_epoch,
                status: ReceiptStatus::Accepted,
                reason: None,
                submitted: vec![],
                release_status: ReleaseStatus::NotNeeded,
                command_end_ms: None,
                at_ms: self.now_ms,
            },
            Err(reason) => self.rejected(&intent, &reason),
        };
        if receipt.status == ReceiptStatus::Accepted {
            self.queue.push_back(key.clone());
        }
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
                self.finish(&key, ReceiptStatus::Rejected, reason);
                return self.query_receipt(key.0, &key.1);
            }
            let record = self.records.get_mut(&key).expect("queued record");
            record.started_ms = Some(self.now_ms);
            record.receipt.command_end_ms = Some(self.now_ms + intent.duration_ms);
            record.receipt.status = ReceiptStatus::Running;
            record.receipt.at_ms = self.now_ms;
            self.active = Some(key.clone());
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
        WireMessage {
            protocol_version: 1,
            message: Message::RenewPermit(request.clone()),
        }
        .validate()?;
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
        WireMessage {
            protocol_version: 1,
            message: Message::RevokeExecution(request.clone()),
        }
        .validate()?;
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
        self.invalidate_permit(&request.permit.id, &request.reason);
        Ok(())
    }

    pub fn cancel_task(&mut self, id: &str) -> Result<(), String> {
        self.tasks.get_mut(id).ok_or("unknown task")?.active = false;
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
        self.connected = true;
        self.session_epoch
    }

    pub fn hold_external_key(&mut self, key: &str) {
        self.physical_held.insert(format!("key:{key}"));
    }
    pub fn set_release_failure(&mut self, fail: bool) {
        self.release_failure = fail;
    }
    pub fn fail_submission_on(&mut self, call_index: usize, fault: SubmissionFault) {
        self.faults.insert(call_index, fault);
    }

    /// A trusted recovery action; revoked permits remain revoked afterward.
    pub fn recover_channel(&mut self) -> Result<(), String> {
        let keys: BTreeSet<Key> = self
            .owned
            .values()
            .map(|owned| owned.record.clone())
            .collect();
        for key in keys {
            self.cleanup(&key);
        }
        if !self.owned.is_empty() {
            return Err("input release still unconfirmed".into());
        }
        self.isolated = false;
        Ok(())
    }

    fn check_channel(&self) -> Result<(), String> {
        if !self.connected {
            return Err("control channel disconnected".into());
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
        for required in ["focus", "layer", "binding", "authorization"] {
            if !dependencies.contains_key(required) {
                return Err(format!("missing required dependency: {required}"));
            }
        }
        if pointer && !dependencies.contains_key("geometry") {
            return Err("missing required dependency: geometry".into());
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
        let pointer = intent.steps.iter().any(|step| {
            matches!(
                step.input,
                Input::PointerDown { .. } | Input::PointerUp { .. } | Input::PointerMove { .. }
            )
        });
        self.check_dependencies(&intent.dependencies, pointer)?;
        for step in &intent.steps {
            if let Input::PointerMove { x, y } = step.input
                && (!(0..1000).contains(&x) || !(0..1000).contains(&y))
            {
                return Err("point outside simulated 1000x1000 surface".into());
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

    fn rejected(&self, intent: &ActionIntent, reason: &str) -> Receipt {
        Receipt {
            intent_id: intent.intent_id.clone(),
            session_epoch: intent.session_epoch,
            status: ReceiptStatus::Rejected,
            reason: Some(reason.into()),
            submitted: vec![],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: None,
            at_ms: self.now_ms,
        }
    }

    fn enforce_guards(&mut self) {
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
            self.invalidate_permit(&id, &reason);
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
            self.finish(&key, ReceiptStatus::Cancelled, reason);
        }
    }

    fn invalidate_permit(&mut self, id: &str, reason: &str) {
        let Some(permit) = self.permits.get_mut(id) else {
            return;
        };
        if permit.revoked {
            return;
        }
        // Mark once before cancelling records. Their terminal handling may
        // invalidate this same permit; it must not recurse through the queue.
        permit.revoked = true;
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
        for key in affected {
            self.finish(&key, ReceiptStatus::Cancelled, reason.into());
        }
    }

    fn finish(&mut self, key: &Key, status: ReceiptStatus, reason: String) {
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
            record.receipt.at_ms = self.now_ms;
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
                self.invalidate_permit(&id, "input channel isolated after release failure");
            }
        } else if status != ReceiptStatus::Completed {
            self.invalidate_permit(
                &permit_id,
                "attempt ended without confirmed normal completion",
            );
        }
    }

    fn cleanup(&mut self, key: &Key) -> bool {
        let tokens: Vec<String> = self
            .owned
            .iter()
            .filter(|(_, owned)| &owned.record == key)
            .map(|(token, _)| token.clone())
            .collect();
        if tokens.is_empty() {
            return true;
        }
        if self.release_failure {
            self.isolated = true;
            if let Some(record) = self.records.get_mut(key) {
                record.receipt.release_status = ReleaseStatus::Failed;
            }
            return false;
        }
        for token in tokens {
            let owned = self.owned.remove(&token).expect("owned token");
            self.apply_backend(owned.release.clone(), true);
            if let Some(record) = self.records.get_mut(key) {
                record.receipt.submitted.push(SubmittedInput {
                    at_ms: self.now_ms,
                    input: owned.release,
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
            );
        }
    }

    fn next_wakeup(&self) -> Option<u64> {
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
        times.into_iter().filter(|time| *time > self.now_ms).min()
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
                self.finish(&key, ReceiptStatus::Cancelled, reason);
                return;
            }
            if let Err(reason) = self.check_ownership(&key, &step.input) {
                let partial = !self.records[&key].receipt.submitted.is_empty();
                self.finish(
                    &key,
                    if partial {
                        ReceiptStatus::Partial
                    } else {
                        ReceiptStatus::Rejected
                    },
                    reason,
                );
                return;
            }
            self.submission_calls += 1;
            let fault = self.faults.remove(&self.submission_calls);
            if fault == Some(SubmissionFault::Rejected) {
                let partial = !self.records[&key].receipt.submitted.is_empty();
                self.finish(
                    &key,
                    if partial {
                        ReceiptStatus::Partial
                    } else {
                        ReceiptStatus::Rejected
                    },
                    "backend rejected input".into(),
                );
                return;
            }
            // A down may have reached the backend even when its acknowledgement was lost.
            self.track_down(&key, &step.input);
            if fault != Some(SubmissionFault::UnknownNotApplied) {
                self.apply_backend(step.input.clone(), false);
            }
            if matches!(
                fault,
                Some(SubmissionFault::UnknownApplied | SubmissionFault::UnknownNotApplied)
            ) {
                self.finish(
                    &key,
                    ReceiptStatus::Unknown,
                    "backend submission outcome unknown".into(),
                );
                return;
            }
            self.track_confirmed_up(&key, &step.input);
            let held = self.owned.values().any(|owned| owned.record == key);
            let record = self.records.get_mut(&key).expect("active record");
            record.next_step += 1;
            record.receipt.submitted.push(SubmittedInput {
                at_ms: self.now_ms,
                input: step.input,
                cleanup: false,
            });
            record.receipt.release_status = if held {
                ReleaseStatus::Held
            } else {
                ReleaseStatus::Released
            };
            record.receipt.at_ms = self.now_ms;
        }
    }

    fn check_ownership(&self, key: &Key, input: &Input) -> Result<(), String> {
        if let Some((token, down)) = input_token(input) {
            if down && self.physical_held.contains(&token) {
                return Err("input already held, including external input".into());
            }
            if !down
                && self
                    .owned
                    .get(&token)
                    .is_none_or(|owned| &owned.record != key)
            {
                return Err("cannot release unowned input".into());
            }
        }
        Ok(())
    }

    fn track_down(&mut self, key: &Key, input: &Input) {
        let release = match input {
            Input::KeyDown { key } => Some(Input::KeyUp { key: key.clone() }),
            Input::PointerDown { button } => Some(Input::PointerUp { button: *button }),
            _ => None,
        };
        if let (Some((token, _)), Some(release)) = (input_token(input), release) {
            self.owned.insert(
                token,
                OwnedInput {
                    record: key.clone(),
                    release,
                },
            );
        }
    }

    fn track_confirmed_up(&mut self, key: &Key, input: &Input) {
        if let Some((token, false)) = input_token(input)
            && self
                .owned
                .get(&token)
                .is_some_and(|owned| &owned.record == key)
        {
            self.owned.remove(&token);
        }
    }

    fn apply_backend(&mut self, input: Input, cleanup: bool) {
        if let Some((token, down)) = input_token(&input) {
            if down {
                self.physical_held.insert(token);
            } else {
                self.physical_held.remove(&token);
            }
        }
        self.events.push(BackendEvent {
            at_ms: self.now_ms,
            input,
            cleanup,
        });
    }
}

fn input_token(input: &Input) -> Option<(String, bool)> {
    match input {
        Input::KeyDown { key } => Some((format!("key:{key}"), true)),
        Input::KeyUp { key } => Some((format!("key:{key}"), false)),
        Input::PointerDown { button } => Some((format!("button:{}", button_name(button)), true)),
        Input::PointerUp { button } => Some((format!("button:{}", button_name(button)), false)),
        Input::PointerMove { .. } => None,
    }
}

fn button_name(button: &MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
    }
}
