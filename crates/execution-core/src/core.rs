//! The execution supervisor: a single-channel state machine with virtual time.
//!
//! Every method is a trusted device event, not a remote API, and calls are
//! processed serially, so a revocation takes effect before the next call. The
//! core performs no IO and reads no clock: the shell feeds it time through
//! [`ExecutionCore::advance_to`] and collects what happened through
//! [`ExecutionCore::drain_events`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use interaction_contracts::desktop::browser::BrowserReadScope;
use interaction_contracts::desktop::browser_execution::{
    BrowserAction, BrowserActionIntent, BrowserExecutionObservation,
};
use interaction_contracts::desktop::device::{
    ActionIntent, DeviceIdentity, ExecutionIntent, ExecutionObservation, GrantPermit, Observation,
    ObservationPurpose, PermitRef, Receipt, ReceiptStatus, ReleaseStatus, RenewPermit, ResumeAck,
    ResumeCommand, RevokeExecution, RootGrant, SourceFreshness, StopAck, StopCommand,
    StopDisposition, StopScope, StopSource, SubmitIntent, SubmittedAction, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::primitive::{Primitive, PrimitivePolicy};

use crate::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, SubmissionAuthority, SubmitContext,
    SubmitOutcome,
};
use crate::latch::{ResumeError, StopLatch};
use crate::ledger::{InputLedger, Key};

/// Pending events are bounded so a shell that never drains cannot exhaust memory.
const MAX_PENDING_EVENTS: usize = 65_536;
const OWNED_DOCUMENT_PROOF_MAX_AGE_MS: u64 = 2_000;
const OWNED_DOCUMENT_DEPENDENCIES: [&str; 4] = ["authorization", "focus", "window", "owned_scope"];

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// Selected by the trusted device configuration, never by an observation.
    pub execution_domain: ExecutionDomain,
    /// Whether held-class primitives may be submitted.
    pub policy: PrimitivePolicy,
    /// Clock domains read `{prefix}/{session_epoch}/{kind}`.
    pub clock_domain_prefix: String,
    /// A clock's stated kind must match its actual implementation. Native
    /// observation currently uses the shared wall offset, not a boot clock.
    pub clock_domain_kind: String,
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
    /// Enable the dedicated owned-document setter proof path.
    pub allow_owned_document_value_setter: bool,
    /// Enable the dedicated owned-document Save proof path.
    pub allow_owned_document_save: bool,
    /// The exact Main-installed browser read scope for a headless profile.
    /// Native and simulated profiles must leave this unset.
    pub browser_scope: Option<Box<BrowserReadScope>>,
}

impl CoreConfig {
    /// The configuration of the I0 reference simulator.
    pub fn i0_simulation() -> Self {
        Self {
            execution_domain: ExecutionDomain::Simulated,
            policy: PrimitivePolicy::AllowHeld,
            clock_domain_prefix: "sim".into(),
            clock_domain_kind: "monotonic".into(),
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
            allow_owned_document_value_setter: false,
            allow_owned_document_save: false,
            browser_scope: None,
        }
    }

    /// Phase 1 production: atomic primitives only, binding proofs and a lease required.
    pub fn production(clock_domain_prefix: impl Into<String>) -> Self {
        let mut dependencies = Self::i0_simulation().dependencies;
        dependencies.push("window".into());
        dependencies.push("menu".into());
        Self {
            execution_domain: ExecutionDomain::ForegroundNative,
            policy: PrimitivePolicy::AtomicOnly,
            clock_domain_prefix: clock_domain_prefix.into(),
            surface_bounds: None,
            require_hit_test: true,
            require_lease: true,
            dependencies,
            ..Self::i0_simulation()
        }
    }

    /// Production gates with one additional private owned-document scope.
    /// Generic production configuration remains unchanged and rejects that
    /// purpose unless this explicit profile is selected.
    pub fn owned_document_setter(clock_domain_prefix: impl Into<String>) -> Self {
        let mut config = Self::production(clock_domain_prefix);
        if !config.dependencies.iter().any(|name| name == "owned_scope") {
            config.dependencies.push("owned_scope".into());
        }
        config.allow_owned_document_value_setter = true;
        config
    }

    /// A separately authorized native element action with no global focus or
    /// pointer authority. Unsupported domains have no production profile.
    pub fn background_ax(clock_domain_prefix: impl Into<String>) -> Self {
        let mut config = Self::production(clock_domain_prefix);
        config.execution_domain = ExecutionDomain::BackgroundAx;
        config.dependencies = [
            "authorization",
            "binding",
            "window",
            "geometry",
            "background_scope",
        ]
        .map(String::from)
        .to_vec();
        config.required_dependencies = config.dependencies.clone();
        config
    }

    /// Explicit owned-document profile with both one-step SetValue and Save actions.
    /// The setter-only profile remains unable to issue SaveOwnedDocument.
    pub fn owned_document_actions(clock_domain_prefix: impl Into<String>) -> Self {
        let mut config = Self::owned_document_setter(clock_domain_prefix);
        config.allow_owned_document_save = true;
        config
    }

    /// A closed browser profile bound to one trusted Main-installed scope.
    /// The core starts in the scope's epoch; reconnects require a newly
    /// constructed profile and scope rather than rebinding browser identity.
    pub fn headless_browser(scope: BrowserReadScope) -> Result<Self, String> {
        scope.validate()?;
        let expected_clock = format!("headless/{}/wall", scope.context.session_epoch);
        if scope.clock_domain != expected_clock {
            return Err("browser scope clock domain differs from the trusted profile".into());
        }
        let mut config = Self::production("headless");
        config.execution_domain = ExecutionDomain::HeadlessBrowser;
        config.clock_domain_kind = "wall".into();
        config.surface_id = scope.context.page_target_id.clone();
        config.surface_bounds = None;
        config.dependencies = ["authorization", "binding", "document", "browser_scope"]
            .map(String::from)
            .to_vec();
        config.required_dependencies = config.dependencies.clone();
        config.geometry_dependency = "browser_scope".into();
        config.require_hit_test = false;
        config.policy = PrimitivePolicy::AtomicOnly;
        config.allow_owned_document_value_setter = false;
        config.allow_owned_document_save = false;
        config.browser_scope = Some(Box::new(scope));
        Ok(config)
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
    /// A row-selection permit can admit only its first immutable intent.
    /// Dispatch revalidation may repeat that same intent, never another one.
    row_intent_id: Option<String>,
}

/// A row read may grant only one permit, even if the same read is aliased under
/// another evidence id. Session/surface namespaces keep reconnects distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RowReadKey {
    session_epoch: u64,
    surface_id: String,
    surface_epoch: u64,
    snapshot_id: String,
    source_sequence: u64,
}

#[derive(Debug, Clone)]
struct Record {
    intent: ExecutionIntent,
    receipt: Receipt,
    started_ms: Option<u64>,
    next_step: usize,
}

struct InFlight {
    sequence: u64,
    reply_deadline_ms: u64,
    key: Key,
    action: PendingAction,
    valid_until_ms: u64,
    cancelled: Option<(ReceiptStatus, String, Option<FailureCode>)>,
    completion: Option<crate::backend::SubmitCompletion>,
}

#[derive(Debug, Clone)]
enum PendingAction {
    Native(Primitive),
    Browser(BrowserAction),
}

impl PendingAction {
    fn submitted(&self, at_ms: u64, cleanup: bool) -> SubmittedAction {
        match self {
            Self::Native(primitive) => SubmittedAction::Native {
                at_ms,
                primitive: primitive.clone(),
                cleanup,
            },
            Self::Browser(action) => SubmittedAction::Browser {
                at_ms,
                action: *action,
            },
        }
    }

    fn native(&self) -> Option<&Primitive> {
        match self {
            Self::Native(primitive) => Some(primitive),
            Self::Browser(_) => None,
        }
    }

    fn is_held(&self) -> bool {
        self.native().is_some_and(Primitive::is_held_class)
    }
}

fn action_count(intent: &ExecutionIntent) -> usize {
    intent.native().map_or(1, |native| native.steps.len())
}

fn next_action_offset(intent: &ExecutionIntent, index: usize) -> Option<u64> {
    match intent.native() {
        Some(native) => native.steps.get(index).map(|step| step.offset_ms),
        None if index == 0 => Some(0),
        None => None,
    }
}

fn pending_action_at(intent: &ExecutionIntent, index: usize) -> Option<(PendingAction, u64)> {
    match intent {
        ExecutionIntent::Native { intent } => intent.steps.get(index).map(|step| {
            (
                PendingAction::Native(step.primitive.clone()),
                step.offset_ms,
            )
        }),
        ExecutionIntent::Browser { intent } if index == 0 => {
            Some((PendingAction::Browser(intent.action), 0))
        }
        ExecutionIntent::Browser { .. } => None,
    }
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
    observations: BTreeMap<String, ExecutionObservation>,
    latest_observation_by_target: BTreeMap<String, String>,
    spent_row_reads: BTreeSet<RowReadKey>,
    last_source_sequence: Option<u64>,
    last_observed_ms: Option<u64>,
    source_verified: bool,
    permits: BTreeMap<String, Permit>,
    next_permit: u64,
    records: BTreeMap<Key, Record>,
    queue: VecDeque<Key>,
    active: Option<Key>,
    in_flight: Option<InFlight>,
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
        let initial_session_epoch = config
            .browser_scope
            .as_deref()
            .map_or(1, |scope| scope.context.session_epoch);
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
            epoch: initial_session_epoch,
        };
        Self {
            config,
            backend,
            now_ms: 0,
            session_epoch: initial_session_epoch,
            surface,
            dependencies,
            tasks: BTreeMap::new(),
            observations: BTreeMap::new(),
            latest_observation_by_target: BTreeMap::new(),
            spent_row_reads: BTreeSet::new(),
            last_source_sequence: None,
            last_observed_ms: None,
            source_verified: false,
            permits: BTreeMap::new(),
            next_permit: 0,
            records: BTreeMap::new(),
            queue: VecDeque::new(),
            active: None,
            in_flight: None,
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
            "{}/{}/{}",
            self.config.clock_domain_prefix, self.session_epoch, self.config.clock_domain_kind
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

    /// Who this device is in the current session.
    pub fn identity(&self) -> DeviceIdentity {
        DeviceIdentity {
            session_epoch: self.session_epoch,
            surface: self.surface.clone(),
            clock_domain: self.clock_domain(),
            dependencies: self.dependency_versions(),
        }
    }

    pub fn dependency_versions(&self) -> BTreeMap<String, u64> {
        self.dependencies
            .iter()
            .map(|(key, value)| (key.clone(), value.revision))
            .collect()
    }

    fn is_exact_row_observation_alias(&self, observation: &ExecutionObservation) -> bool {
        let Some(native) = observation.native() else {
            return false;
        };
        let Some(read) = row_read_key(native) else {
            return false;
        };
        self.observations.values().any(|existing| {
            let Some(existing_native) = existing.native() else {
                return false;
            };
            if row_read_key(existing_native).as_ref() != Some(&read) {
                return false;
            }
            let mut alias = native.clone();
            alias.observation_id = existing_native.observation_id.clone();
            existing_native == &alias
        })
    }

    pub fn dependency_validity(&self) -> BTreeMap<String, Option<bool>> {
        self.dependencies
            .iter()
            .map(|(key, value)| (key.clone(), value.valid))
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
            record.intent.permit().id == id && record.receipt.status == ReceiptStatus::Running
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

    pub fn observe(&mut self, observation: impl Into<ExecutionObservation>) -> Result<(), String> {
        let observation = observation.into();
        observation.validate()?;
        match (&observation, self.config.browser_scope.as_deref()) {
            (ExecutionObservation::Native { observation }, None) => {
                self.validate_native_observation(observation)?;
            }
            (ExecutionObservation::Browser { observation }, Some(scope)) => {
                self.validate_browser_observation(observation, scope)?;
            }
            _ => return Err("observation kind differs from the trusted device profile".into()),
        }
        if observation.session_epoch() != self.session_epoch
            || observation.surface() != &self.surface
        {
            return Err("observation context mismatch".into());
        }
        if observation.clock_domain() != self.clock_domain()
            || observation.observed_ms() > self.now_ms
        {
            return Err("observation clock mismatch or future observation".into());
        }
        if let Some(existing) = self.observations.get(observation.observation_id()) {
            return if existing == &observation {
                Ok(())
            } else {
                Err("observation ID conflict".into())
            };
        }
        let target_key = observation_target_key(&observation);
        let exact_row_alias = self.last_source_sequence == Some(observation.source_sequence())
            && self.last_observed_ms == Some(observation.observed_ms())
            && self.is_exact_row_observation_alias(&observation);
        if (self
            .last_source_sequence
            .is_some_and(|value| observation.source_sequence() <= value)
            && !exact_row_alias)
            || self
                .last_observed_ms
                .is_some_and(|value| observation.observed_ms() < value)
        {
            return Err("out-of-order observation".into());
        }
        if exact_row_alias {
            self.latest_observation_by_target
                .insert(target_key.clone(), observation.observation_id().to_string());
            self.observations
                .insert(observation.observation_id().to_string(), observation);
            self.enforce_guards();
            return Ok(());
        }
        let rolled_back = self
            .latest_observation_by_target
            .get(&target_key)
            .and_then(|id| self.observations.get(id))
            .is_some_and(|old| binding_revision_rolled_back(old, &observation));
        if rolled_back {
            let affected: Vec<_> = self
                .permits
                .values()
                .filter(|permit| {
                    !permit.revoked
                        && self
                            .observations
                            .get(&permit.evidence_id)
                            .is_some_and(|evidence| observation_target_key(evidence) == target_key)
                })
                .map(|permit| permit.reference.id.clone())
                .collect();
            for id in affected {
                self.invalidate_permit(
                    &id,
                    "source binding environment revision rolled back",
                    None,
                );
            }
            return Err("source binding environment revision rolled back".into());
        }
        self.last_source_sequence = Some(observation.source_sequence());
        self.last_observed_ms = Some(observation.observed_ms());
        self.source_verified = observation.freshness() == SourceFreshness::Verified;
        self.latest_observation_by_target
            .insert(target_key, observation.observation_id().to_string());
        self.observations
            .insert(observation.observation_id().to_string(), observation);
        self.enforce_guards();
        Ok(())
    }

    fn validate_native_observation(&self, observation: &Observation) -> Result<(), String> {
        observation.validate()?;
        if observation.execution_domain != self.config.execution_domain {
            return Err("observation execution domain differs from the trusted profile".into());
        }
        if self.config.browser_scope.is_some() {
            return Err("native observation is not accepted by a browser profile".into());
        }
        if matches!(
            observation.purpose,
            ObservationPurpose::OwnedDocumentValueSetter | ObservationPurpose::OwnedDocumentSave
        ) {
            let enabled = match observation.purpose {
                ObservationPurpose::OwnedDocumentValueSetter => {
                    self.config.allow_owned_document_value_setter
                }
                ObservationPurpose::OwnedDocumentSave => self.config.allow_owned_document_save,
                _ => false,
            };
            if !enabled {
                return Err("owned document action profile is disabled".into());
            }
            observation
                .owned_document_proof
                .as_deref()
                .ok_or("owned document proof missing")?
                .validate_at(self.now_ms, OWNED_DOCUMENT_PROOF_MAX_AGE_MS)?;
        }
        if observation.purpose == ObservationPurpose::RowSelection {
            let proof = observation
                .binding_proof
                .as_ref()
                .ok_or("row selection requires a table viewport binding proof")?;
            if !proof.supports_row_selection() || observation.owned_document_proof.is_some() {
                return Err("row selection requires its dedicated table viewport proof".into());
            }
            if proof.continuity.source_sequence != observation.source_sequence {
                return Err(
                    "row selection proof sequence differs from its Source observation".into(),
                );
            }
        }
        Ok(())
    }

    fn validate_browser_observation(
        &self,
        observation: &BrowserExecutionObservation,
        configured_scope: &BrowserReadScope,
    ) -> Result<(), String> {
        observation.validate()?;
        if self.config.execution_domain != ExecutionDomain::HeadlessBrowser
            || self.config.browser_scope.as_deref() != Some(configured_scope)
        {
            return Err("browser observation has no matching trusted browser profile".into());
        }
        if observation.scope.as_ref() != configured_scope
            || observation.surface != self.surface
            || observation.scope.context.session_epoch != self.session_epoch
            || observation.scope.clock_domain != self.clock_domain()
            || observation.scope.issued_ms > self.now_ms
            || observation.binding.disabled
                != interaction_contracts::desktop::browser::BrowserObservedBool::False
            || observation.valid_until_ms <= self.now_ms
        {
            return Err(
                "browser observation differs from its fixed session, scope or target".into(),
            );
        }
        if observation.binding.read_finished_ms > self.now_ms {
            return Err("browser read has a future finish time".into());
        }
        let names: BTreeSet<_> = observation
            .dependencies
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<_> = self
            .config
            .required_dependencies
            .iter()
            .map(String::as_str)
            .collect();
        if names != expected {
            return Err("browser dependencies are incomplete or broadened".into());
        }
        self.check_dependencies(&observation.dependencies, false)
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
        self.check_observation_dependencies(observation)?;
        let row_read = if observation
            .native()
            .is_some_and(|native| native.purpose == ObservationPurpose::RowSelection)
        {
            let read = row_read_key(observation.native().expect("checked native observation"))
                .ok_or_else(|| {
                    Denial::plain("row selection evidence has no Source read identity")
                })?;
            if self.spent_row_reads.contains(&read) {
                return Err(Denial::plain(
                    "this Source read has already granted a row-selection permit",
                ));
            }
            Some(read)
        } else {
            None
        };
        if request.valid_until_ms <= self.now_ms
            || request.valid_until_ms > task.deadline_ms
            || request.valid_until_ms > observation.valid_until_ms()
        {
            return Err(Denial::plain(
                "permit deadline exceeds task or evidence window",
            ));
        }
        let next_permit = self
            .next_permit
            .checked_add(1)
            .ok_or_else(|| Denial::plain("permit generation overflow"))?;
        let reference = PermitRef {
            id: format!("permit-{next_permit}"),
            generation: next_permit,
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
            dependencies: observation.dependencies().clone(),
            valid_until_ms: request.valid_until_ms,
            revoked: false,
            row_intent_id: None,
        };
        if let Some(read) = row_read {
            self.spent_row_reads.insert(read);
        }
        self.next_permit = next_permit;
        self.permits.insert(reference.id.clone(), permit);
        Ok(reference)
    }

    pub fn submit(&mut self, request: SubmitIntent) -> Receipt {
        let SubmitIntent {
            acked_stop_seq,
            intent,
        } = request;
        let key = (intent.session_epoch(), intent.intent_id().to_string());
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
            .and_then(|()| self.check_latch(acked_stop_seq, intent.task_id()))
            .and_then(|()| self.check_intent(&intent, None).map_err(Denial::plain));
        let receipt = match validation {
            Ok(()) => Receipt {
                intent_id: intent.intent_id().to_string(),
                attempt_id: intent.attempt_id().map(str::to_string),
                session_epoch: intent.session_epoch(),
                status: ReceiptStatus::Accepted,
                reason_code: None,
                reason: None,
                submitted: vec![],
                release_status: ReleaseStatus::NotNeeded,
                command_end_ms: None,
                input_end_ms: None,
                at_ms: self.now_ms,
            },
            Err(denial) => self.rejected(&intent, &denial.reason, denial.code),
        };
        if receipt.status == ReceiptStatus::Accepted {
            if self
                .observations
                .get(intent.evidence_id())
                .and_then(ExecutionObservation::native)
                .is_some_and(|evidence| evidence.purpose == ObservationPurpose::RowSelection)
                && let Some(permit) = self.permits.get_mut(&intent.permit().id)
            {
                permit.row_intent_id = Some(intent.intent_id().to_string());
            }
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
        if self.active.is_some() || self.in_flight.is_some() || self.isolated {
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
            record.receipt.command_end_ms = Some(self.now_ms + intent.duration_ms());
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
        self.poll_backend_completion();
        self.complete_ended_action();
        self.run_due_steps();
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
        if self
            .observations
            .get(&permit.evidence_id)
            .and_then(ExecutionObservation::native)
            .is_some_and(|evidence| evidence.purpose == ObservationPurpose::RowSelection)
            || self
                .observations
                .get(&request.evidence_id)
                .and_then(ExecutionObservation::native)
                .is_some_and(|evidence| evidence.purpose == ObservationPurpose::RowSelection)
        {
            return Err("row-selection permits are single-use and cannot be renewed".into());
        }
        self.check_permit(permit, &request.permit)?;
        let observation = self
            .observations
            .get(&request.evidence_id)
            .ok_or("unknown renewal evidence")?;
        self.check_observation(observation, &permit.target_ref)?;
        if observation.dependencies() != &permit.dependencies {
            return Err("renewal dependencies changed".into());
        }
        let old = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("missing prior evidence")?;
        if !same_binding_identity(old, observation) {
            return Err("renewal cannot replace the admitted source binding".into());
        }
        if request.valid_until_ms > permit.valid_until_ms
            && (observation.source_sequence() <= old.source_sequence()
                || observation.observed_ms() <= old.observed_ms())
        {
            return Err("renewal extension requires genuinely newer evidence".into());
        }
        let task = self.tasks.get(&permit.task_id).ok_or("unknown task")?;
        if request.valid_until_ms <= self.now_ms
            || request.valid_until_ms > task.deadline_ms
            || request.valid_until_ms > observation.valid_until_ms()
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
        self.latest_observation_by_target.clear();
        self.spent_row_reads.clear();
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
        if self.in_flight.is_some() {
            return Err("submission outcome still pending".into());
        }
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
            source: command.source,
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
        if let Some(pending) = &self.in_flight {
            times.push(pending.reply_deadline_ms);
        }
        if let Some(at) = self
            .in_flight
            .as_ref()
            .and_then(|p| p.completion.as_ref())
            .and_then(|c| c.input_end_ms.or(c.submitted_at_ms))
        {
            times.push(at);
        }
        if let Some(record) = self.active.as_ref().and_then(|key| self.records.get(key)) {
            if let Some(end) = record.receipt.command_end_ms {
                times.push(end);
            }
            if let (Some(start), Some(offset)) = (
                record.started_ms,
                next_action_offset(&record.intent, record.next_step),
            ) {
                times.push(start + offset);
            }
        }
        for permit in self.permits.values().filter(|permit| !permit.revoked) {
            times.push(permit.valid_until_ms);
            if let Some(task) = self.tasks.get(&permit.task_id) {
                times.push(task.deadline_ms);
            }
            if let Some(observation) = self.observations.get(&permit.evidence_id) {
                times.push(observation.valid_until_ms());
            }
        }
        for record in self
            .records
            .values()
            .filter(|record| record.receipt.status == ReceiptStatus::Accepted)
        {
            if let Some(expired) = record.intent.latest_start_ms().checked_add(1) {
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
                self.records.get(*key).is_some_and(|record| {
                    roots
                        .iter()
                        .any(|root| root.as_str() == record.intent.task_id())
                })
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

    fn check_policy(&self, intent: &ExecutionIntent) -> Result<(), Denial> {
        let Some(native) = intent.native() else {
            if self.config.execution_domain != ExecutionDomain::HeadlessBrowser
                || self.config.browser_scope.is_none()
                || intent.browser().is_none()
                || intent.duration_ms() > 1_500
            {
                return Err(Denial::coded(
                    FailureCode::InputRejected,
                    "browser action is outside the fixed headless action profile",
                ));
            }
            return Ok(());
        };
        if self.config.browser_scope.is_some() {
            return Err(Denial::coded(
                FailureCode::InputRejected,
                "native intent is not accepted by a browser profile",
            ));
        }
        self.check_native_policy(native)
    }

    fn check_native_policy(&self, intent: &ActionIntent) -> Result<(), Denial> {
        let owned_action = self.owned_document_action_purpose(intent);
        if intent.owned_document_proof.is_some() && owned_action.is_none() {
            return Err(Denial::coded(
                FailureCode::InputRejected,
                "owned document proof does not authorize this action",
            ));
        }
        if has_owned_document_save(intent)
            && owned_action != Some(ObservationPurpose::OwnedDocumentSave)
        {
            return Err(Denial::coded(
                FailureCode::InputRejected,
                "owned document Save requires its dedicated proof and profile",
            ));
        }
        if self.config.require_hit_test
            && intent.binding_proof.is_none()
            && owned_action.is_none()
            && intent.steps.iter().any(|s| s.primitive.needs_hit_test())
        {
            return Err(Denial::coded(
                FailureCode::ContextUnresolved,
                "complete binding proof required for a pointer or AX action",
            ));
        }
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

    fn owned_document_action_purpose(&self, intent: &ActionIntent) -> Option<ObservationPurpose> {
        if intent.owned_document_proof.is_none()
            || intent.binding_proof.is_some()
            || intent.binding_fingerprint.is_some()
        {
            return None;
        }
        let [step] = intent.steps.as_slice() else {
            return None;
        };
        if step.offset_ms != 0 {
            return None;
        }
        match &step.primitive {
            Primitive::AxAction {
                action: interaction_contracts::desktop::primitive::AxActionKind::SetValue { .. },
            } if self.config.allow_owned_document_value_setter => {
                Some(ObservationPurpose::OwnedDocumentValueSetter)
            }
            Primitive::AxAction {
                action:
                    interaction_contracts::desktop::primitive::AxActionKind::SaveOwnedDocument {
                        expected_value,
                    },
            } if self.config.allow_owned_document_save
                && expected_value.len() <= 2048
                && !expected_value.contains('\0') =>
            {
                Some(ObservationPurpose::OwnedDocumentSave)
            }
            _ => None,
        }
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

    fn check_observation(
        &self,
        observation: &ExecutionObservation,
        target: &str,
    ) -> Result<(), String> {
        if observation.session_epoch() != self.session_epoch
            || observation.surface() != &self.surface
            || observation.target_ref() != target
        {
            return Err("evidence context or target mismatch".into());
        }
        if observation.clock_domain() != self.clock_domain()
            || observation.observed_ms() > self.now_ms
            || observation.valid_until_ms() <= self.now_ms
            || observation
                .browser()
                .is_some_and(|browser| browser.binding.read_finished_ms > self.now_ms)
            || observation.freshness() != SourceFreshness::Verified
        {
            return Err("evidence expired, uncertain, incomplete, or clock mismatch".into());
        }
        self.check_observation_dependencies(observation)
    }

    fn check_observation_dependencies(
        &self,
        observation: &ExecutionObservation,
    ) -> Result<(), String> {
        match observation {
            ExecutionObservation::Native { observation } => {
                self.check_native_observation_dependencies(observation)
            }
            ExecutionObservation::Browser { observation } => {
                let scope = self
                    .config
                    .browser_scope
                    .as_deref()
                    .ok_or("browser observation has no trusted browser profile")?;
                self.validate_browser_observation(observation, scope)
            }
        }
    }

    fn check_native_observation_dependencies(
        &self,
        observation: &Observation,
    ) -> Result<(), String> {
        observation.validate()?;
        if observation.execution_domain != self.config.execution_domain
            || self.config.browser_scope.is_some()
        {
            return Err("observation execution domain differs from the device profile".into());
        }
        match observation.purpose {
            ObservationPurpose::TargetInput => {
                return self.check_dependencies(&observation.dependencies, false);
            }
            ObservationPurpose::MenuSelection => {
                if !observation.dependencies.contains_key("menu") {
                    return Err("menu selection dependency missing".into());
                }
                return self.check_dependencies(&observation.dependencies, true);
            }
            ObservationPurpose::FocusedControl => {
                return self.check_dependencies(&observation.dependencies, true);
            }
            ObservationPurpose::RowSelection => {
                if !observation
                    .binding_proof
                    .as_ref()
                    .is_some_and(|proof| proof.supports_row_selection())
                    || observation.owned_document_proof.is_some()
                {
                    return Err("row selection requires its dedicated table viewport proof".into());
                }
                return self.check_dependencies(&observation.dependencies, true);
            }
            ObservationPurpose::BackgroundAxAction => {
                if self.config.execution_domain != ExecutionDomain::BackgroundAx
                    || !observation
                        .binding_proof
                        .as_ref()
                        .is_some_and(|proof| proof.supports_background_ax_press())
                {
                    return Err("background AX requires its dedicated profile and proof".into());
                }
                return self.check_dependencies(&observation.dependencies, true);
            }
            ObservationPurpose::HeadlessBrowserAction
            | ObservationPurpose::AppIntegrationAction => {
                return Err(
                    "this device profile has no headless or integration input backend".into(),
                );
            }
            ObservationPurpose::OwnedDocumentValueSetter
            | ObservationPurpose::OwnedDocumentSave => {
                let enabled = match observation.purpose {
                    ObservationPurpose::OwnedDocumentValueSetter => {
                        self.config.allow_owned_document_value_setter
                    }
                    ObservationPurpose::OwnedDocumentSave => self.config.allow_owned_document_save,
                    _ => false,
                };
                if !enabled {
                    return Err("owned document action profile is disabled".into());
                }
                let names = observation
                    .dependencies
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>();
                if names != std::collections::BTreeSet::from(OWNED_DOCUMENT_DEPENDENCIES) {
                    return Err("owned document dependency scope is incomplete or broadened".into());
                }
                observation
                    .owned_document_proof
                    .as_deref()
                    .ok_or("owned document proof missing")?
                    .validate_at(self.now_ms, OWNED_DOCUMENT_PROOF_MAX_AGE_MS)?;
                for name in OWNED_DOCUMENT_DEPENDENCIES {
                    let revision = observation
                        .dependencies
                        .get(name)
                        .ok_or_else(|| format!("missing owned document dependency: {name}"))?;
                    let current = self
                        .dependencies
                        .get(name)
                        .ok_or_else(|| format!("owned document dependency unsupported: {name}"))?;
                    if current.valid != Some(true) || current.revision != *revision {
                        return Err(format!(
                            "owned document dependency invalid, unknown, or changed: {name}"
                        ));
                    }
                }
                return Ok(());
            }
            ObservationPurpose::WindowActivation => {}
        }
        for name in ["window", "authorization"] {
            let revision = observation
                .dependencies
                .get(name)
                .ok_or("window dependency missing")?;
            let current = self
                .dependencies
                .get(name)
                .ok_or("window activation unsupported")?;
            if current.valid != Some(true) || current.revision != *revision {
                return Err("window dependency invalid or changed".into());
            }
        }
        Ok(())
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
        let observation = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown permit evidence")?;
        let evidence_key = observation_target_key(observation);
        if let Some(latest) = self
            .latest_observation_by_target
            .get(&evidence_key)
            .and_then(|id| self.observations.get(id))
            && !same_binding_identity(observation, latest)
        {
            return Err("new source evidence invalidated the permit binding".into());
        }
        self.check_observation(observation, &permit.target_ref)
    }

    fn check_intent(
        &self,
        intent: &ExecutionIntent,
        started_ms: Option<u64>,
    ) -> Result<(), String> {
        match intent {
            ExecutionIntent::Native { intent } => self.check_native_intent(intent, started_ms),
            ExecutionIntent::Browser { intent } => self.check_browser_intent(intent, started_ms),
        }
    }

    fn check_browser_intent(
        &self,
        intent: &BrowserActionIntent,
        started_ms: Option<u64>,
    ) -> Result<(), String> {
        intent.validate()?;
        if self.config.execution_domain != ExecutionDomain::HeadlessBrowser
            || self.config.browser_scope.is_none()
        {
            return Err("browser intent is not enabled by the trusted device profile".into());
        }
        if intent.session_epoch != self.session_epoch || intent.evidence.surface != self.surface {
            return Err("browser intent session or surface mismatch".into());
        }
        let permit = self
            .permits
            .get(&intent.permit.id)
            .ok_or("unknown permit")?;
        self.check_permit(permit, &intent.permit)?;
        if intent.task_id != permit.task_id
            || intent.task_revision != permit.task_revision
            || intent.evidence.target_ref() != permit.target_ref
            || intent.evidence.surface != permit.surface
            || intent.evidence.dependencies != permit.dependencies
        {
            return Err("browser intent does not match granted permit scope".into());
        }
        if intent.root_epoch != permit.root_epoch {
            return Err("browser intent root epoch does not match the permit".into());
        }
        if started_ms.is_none() && self.now_ms > intent.latest_start_ms {
            return Err("latest start deadline missed".into());
        }
        let issue = self
            .observations
            .get(intent.evidence.observation_id())
            .ok_or("unknown browser issue evidence")?;
        let issue_browser = issue
            .browser()
            .ok_or("browser intent references native issue evidence")?;
        let tagged: ExecutionIntent = intent.clone().into();
        tagged.validate_against_observation(issue)?;
        if issue_browser != intent.evidence.as_ref() {
            return Err("browser intent differs from immutable issue evidence".into());
        }
        let permit_evidence = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown permit evidence")?;
        let permit_browser = permit_evidence
            .browser()
            .ok_or("browser permit references native evidence")?;
        if !same_binding_identity(issue, permit_evidence)
            || permit_browser.binding.checked != issue_browser.binding.checked
        {
            return Err("browser target or observed state changed after permit admission".into());
        }
        let latest = self
            .latest_observation_by_target
            .get(&observation_target_key(issue))
            .and_then(|id| self.observations.get(id))
            .ok_or("browser target has no latest admitted evidence")?;
        let latest_browser = latest
            .browser()
            .ok_or("browser target has native latest evidence")?;
        if !same_binding_identity(issue, latest)
            || latest_browser.binding.checked != issue_browser.binding.checked
        {
            return Err("browser target or checked state changed after evidence admission".into());
        }
        let end = started_ms
            .unwrap_or(self.now_ms)
            .checked_add(intent.duration_ms)
            .ok_or("command end overflow")?;
        let task = self.tasks.get(&intent.task_id).ok_or("unknown task")?;
        if end > permit.valid_until_ms
            || end > permit_evidence.valid_until_ms()
            || end > task.deadline_ms
        {
            return Err("command end exceeds evidence, permit, or root deadline".into());
        }
        Ok(())
    }

    fn check_native_intent(
        &self,
        intent: &ActionIntent,
        started_ms: Option<u64>,
    ) -> Result<(), String> {
        if intent.execution_domain != self.config.execution_domain {
            return Err("intent execution domain differs from the device profile".into());
        }
        if intent.session_epoch != self.session_epoch || intent.surface != self.surface {
            return Err("intent session/surface mismatch".into());
        }
        let permit = self
            .permits
            .get(&intent.permit.id)
            .ok_or("unknown permit")?;
        self.check_permit(permit, &intent.permit)?;
        if permit
            .row_intent_id
            .as_deref()
            .is_some_and(|admitted| admitted != intent.intent_id)
        {
            return Err("row-selection permit already admitted another intent".into());
        }
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
        if self.config.require_hit_test
            && intent.steps.iter().any(|s| s.primitive.needs_hit_test())
            && intent.binding_proof.is_none()
            && self.owned_document_action_purpose(intent).is_none()
        {
            return Err("complete binding proof required for a pointer or AX action".into());
        }
        let owned_action = self.owned_document_action_purpose(intent);
        if intent.owned_document_proof.is_some() && owned_action.is_none() {
            return Err("owned document proof does not authorize this action".into());
        }
        if has_owned_document_save(intent)
            && owned_action != Some(ObservationPurpose::OwnedDocumentSave)
        {
            return Err("owned document Save requires its dedicated proof and profile".into());
        }
        let pointer = intent
            .steps
            .iter()
            .any(|step| step.primitive.is_pointer_class());
        let issue_execution = self
            .observations
            .get(&intent.evidence_id)
            .ok_or("unknown issue evidence")?;
        let issue = issue_execution
            .native()
            .ok_or("native intent references browser issue evidence")?;
        let permit_execution = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown permit evidence")?;
        let permit_evidence = permit_execution
            .native()
            .ok_or("native permit references browser evidence")?;
        let tagged: ExecutionIntent = intent.clone().into();
        tagged.validate_against_observation(issue_execution)?;
        if permit_evidence.execution_domain != intent.execution_domain {
            return Err("permit evidence execution domain differs from the intent".into());
        }
        if (issue.purpose == ObservationPurpose::RowSelection
            || permit_evidence.purpose == ObservationPurpose::RowSelection)
            && (issue.purpose != ObservationPurpose::RowSelection
                || permit_evidence.purpose != ObservationPurpose::RowSelection
                || intent.evidence_id != permit.evidence_id
                || intent.binding_proof.as_ref() != issue.binding_proof.as_ref()
                || intent.binding_proof.as_ref() != permit_evidence.binding_proof.as_ref())
        {
            return Err(
                "row-selection intent must use the permit's exact evidence and Source proof".into(),
            );
        }
        let table_viewport_scope = intent.binding_proof.as_ref().is_some_and(|proof| {
            matches!(
                &proof.continuity.scope,
                interaction_contracts::desktop::binding::BindingProofScope::TableViewport { .. }
            )
        });
        if (table_viewport_scope && issue.purpose != ObservationPurpose::RowSelection)
            || (issue.purpose == ObservationPurpose::RowSelection && !table_viewport_scope)
        {
            return Err("table viewport proof is restricted to row selection".into());
        }
        let single = match intent.steps.as_slice() {
            [step] if step.offset_ms == 0 => Some(&step.primitive),
            _ => None,
        };
        let scope = intent
            .binding_proof
            .as_ref()
            .and_then(|proof| proof.menu_scope.as_ref());
        let focus_scope = intent
            .binding_proof
            .as_ref()
            .and_then(|proof| proof.focus_scope.as_ref());
        use interaction_contracts::desktop::binding::MenuBindingScope;
        use interaction_contracts::desktop::primitive::AxActionKind;
        let purpose_authorizes = match issue.purpose {
            ObservationPurpose::TargetInput => {
                intent.owned_document_proof.is_none()
                    && focus_scope.is_none()
                    && !intent.steps.iter().any(|step| matches!(
                        step.primitive,
                        Primitive::AxAction {
                            action: interaction_contracts::desktop::primitive::AxActionKind::SetSelectedTrue
                        }
                    ))
                    && !intent
                    .steps
                    .iter()
                    .any(|step| matches!(step.primitive, Primitive::ActivateWindow { .. } | Primitive::KeyPress { .. }))
                    && match scope {
                        None => true,
                        Some(MenuBindingScope::Opener { .. }) => matches!(
                            single,
                            Some(Primitive::AxAction {
                                action: AxActionKind::ShowMenu
                            })
                        ),
                        Some(MenuBindingScope::Item { .. }) => false,
                    }
            }
            ObservationPurpose::MenuSelection => {
                focus_scope.is_none() && matches!(
                    single,
                    Some(Primitive::AxAction {
                        action: AxActionKind::Press
                    })
                ) && matches!(scope, Some(MenuBindingScope::Item { opened_by, .. })
                        if intent.attempt_id.as_deref() == Some(opened_by.attempt_id.as_str()) && opened_by.intent_id != intent.intent_id)
            }
            ObservationPurpose::WindowActivation => {
                scope.is_none() && focus_scope.is_none()
                    && matches!(single, Some(Primitive::ActivateWindow { app })
                    if issue.binding_proof.as_ref().is_some_and(|p| &p.fingerprint.app == app))
            }
            ObservationPurpose::FocusedControl => {
                scope.is_none() && focus_scope.is_some_and(|focus|
                    matches!(single, Some(Primitive::KeyPress { key, modifiers })
                        if key == focus.key.canonical_name()
                        && *modifiers == interaction_contracts::desktop::primitive::Modifiers::default()))
            }
            ObservationPurpose::RowSelection => {
                scope.is_none()
                    && focus_scope.is_none()
                    && intent.owned_document_proof.is_none()
                    && issue.owned_document_proof.is_none()
                    && issue
                        .binding_proof
                        .as_ref()
                        .is_some_and(|proof| proof.supports_row_selection())
                    && intent
                        .binding_proof
                        .as_ref()
                        .is_some_and(|proof| proof.supports_row_selection())
                    && matches!(
                        single,
                        Some(Primitive::AxAction {
                            action: AxActionKind::SetSelectedTrue
                        })
                    )
            }
            ObservationPurpose::OwnedDocumentValueSetter => {
                scope.is_none()
                    && focus_scope.is_none()
                    && self.owned_document_action_purpose(intent)
                        == Some(ObservationPurpose::OwnedDocumentValueSetter)
                    && issue.owned_document_proof == intent.owned_document_proof
            }
            ObservationPurpose::OwnedDocumentSave => {
                scope.is_none()
                    && focus_scope.is_none()
                    && self.owned_document_action_purpose(intent)
                        == Some(ObservationPurpose::OwnedDocumentSave)
                    && issue.owned_document_proof == intent.owned_document_proof
            }
            ObservationPurpose::BackgroundAxAction => {
                scope.is_none() && focus_scope.is_none()
                    && intent.owned_document_proof.is_none()
                    && issue.binding_proof.as_ref()
                        .is_some_and(|proof| proof.supports_background_ax_press())
                    && intent.binding_proof.as_ref()
                        .is_some_and(|proof| proof.supports_background_ax_press())
                    && matches!(single, Some(Primitive::AxAction { action: AxActionKind::Press }))
            }
            ObservationPurpose::HeadlessBrowserAction | ObservationPurpose::AppIntegrationAction => false,
        };
        if !purpose_authorizes {
            return Err("observation purpose, menu scope and primitive differ".into());
        }
        if let Some(MenuBindingScope::Item { context, opened_by }) = scope {
            let opening = self
                .records
                .get(&(intent.session_epoch, opened_by.intent_id.clone()))
                .ok_or("menu opening receipt is unknown in this device session")?;
            let opening_native = opening
                .intent
                .native()
                .ok_or("menu opening intent is not native")?;
            let opening_scope = opening_native
                .binding_proof
                .as_ref()
                .and_then(|proof| proof.menu_scope.as_ref())
                .ok_or("menu opening intent has no opener source anchor")?;
            if !matches!(opening_scope, MenuBindingScope::Opener { .. }) {
                return Err("menu opening intent has no opener source anchor".into());
            }
            let opening_anchor = opening_scope.source_anchor();
            let opening_deadline = opening_scope.attempt_deadline_ms();
            let input_end = opening
                .receipt
                .input_end_ms
                .ok_or("menu opening has no known input end")?;
            if opening.receipt.status != ReceiptStatus::Completed
                || opening.intent.task_id() != intent.task_id.as_str()
                || opening.intent.task_revision() != intent.task_revision
                || opening.intent.root_epoch() != intent.root_epoch
                || opening.intent.attempt_id() != intent.attempt_id.as_deref()
                || opened_by.attempt_deadline_ms != opening_deadline
                || !opening_anchor.same_identity(&context.opener)
                || issue.observed_ms <= input_end
                || issue.source_sequence <= opening_anchor.verified_source_sequence
            {
                return Err(
                    "menu selection does not follow a settled opening in the same root and attempt"
                        .into(),
                );
            }
        }
        if pointer {
            self.check_dependencies(&intent.dependencies, true)?;
        }
        if let Some((width, height)) = self.config.surface_bounds {
            for step in &intent.steps {
                let point = match step.primitive {
                    Primitive::PointerMove { x, y }
                    | Primitive::PointerPosition { x, y }
                    | Primitive::Click { x, y, .. } => Some((x, y)),
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
        let original_execution = self
            .observations
            .get(&intent.evidence_id)
            .ok_or("unknown issue evidence")?;
        let original = original_execution
            .native()
            .ok_or("native intent references browser issue evidence")?;
        if original.session_epoch != intent.session_epoch
            || original.surface != intent.surface
            || original.target_ref != intent.target_ref
            || original.dependencies != intent.dependencies
            || original.freshness != SourceFreshness::Verified
        {
            return Err("issue evidence mismatch".into());
        }
        if intent.binding_proof.is_some() && original.binding_proof != intent.binding_proof {
            return Err("intent binding differs from immutable issue evidence".into());
        }
        if intent.owned_document_proof.is_some()
            && original.owned_document_proof != intent.owned_document_proof
        {
            return Err("intent owned proof differs from immutable issue evidence".into());
        }
        let end = started_ms
            .unwrap_or(self.now_ms)
            .checked_add(intent.duration_ms)
            .ok_or("command end overflow")?;
        let attempt_ceiling = match scope {
            Some(MenuBindingScope::Opener {
                attempt_deadline_ms,
                ..
            }) => Some(*attempt_deadline_ms),
            Some(MenuBindingScope::Item { opened_by, .. }) => Some(opened_by.attempt_deadline_ms),
            None => None,
        };
        if attempt_ceiling.is_some_and(|ceiling| end > ceiling || permit.valid_until_ms > ceiling) {
            return Err("menu command or permit exceeds its owning attempt deadline".into());
        }
        let evidence_execution = self
            .observations
            .get(&permit.evidence_id)
            .ok_or("unknown current evidence")?;
        let evidence = evidence_execution
            .native()
            .ok_or("native permit references browser current evidence")?;
        if let Some(issue_proof) = issue.owned_document_proof.as_deref() {
            let current_proof = evidence
                .owned_document_proof
                .as_deref()
                .ok_or("current owned document evidence is unavailable")?;
            if !issue_proof.same_instance(current_proof)
                || current_proof.environment_revision < issue_proof.environment_revision
            {
                return Err("current owned document evidence changed its receiver identity".into());
            }
            current_proof.validate_at(self.now_ms, OWNED_DOCUMENT_PROOF_MAX_AGE_MS)?;
        }
        let task = self.tasks.get(&intent.task_id).ok_or("unknown task")?;
        if intent.binding_proof.is_some()
            && !same_binding_identity(original_execution, evidence_execution)
        {
            return Err("current evidence changed the admitted binding instance".into());
        }
        if intent.owned_document_proof.is_some()
            && !same_binding_identity(original_execution, evidence_execution)
        {
            return Err("current evidence changed the owned document receiver instance".into());
        }
        if end > permit.valid_until_ms || end > evidence.valid_until_ms || end > task.deadline_ms {
            return Err("command end exceeds evidence, permit, or root deadline".into());
        }
        Ok(())
    }

    fn rejected(
        &self,
        intent: &ExecutionIntent,
        reason: &str,
        code: Option<FailureCode>,
    ) -> Receipt {
        Receipt {
            intent_id: intent.intent_id().to_string(),
            attempt_id: intent.attempt_id().map(str::to_string),
            session_epoch: intent.session_epoch(),
            status: ReceiptStatus::Rejected,
            reason_code: code,
            reason: Some(reason.into()),
            submitted: vec![],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: None,
            input_end_ms: None,
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
                record.intent.permit().id == id
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
        self.backend.revoke(&self.permits[id].reference);
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
        let permit_id = record.intent.permit().id.clone();
        if let Some(pending) = self.in_flight.as_mut().filter(|p| &p.key == key) {
            // The backend may have entered its commit gate already. Stop and
            // cancellation cannot make that definitely unsubmitted.
            pending.cancelled = Some((status, reason.clone(), code));
            let record = self.records.get_mut(key).expect("pending record");
            record.receipt.status = ReceiptStatus::Unknown;
            record.receipt.reason = Some(format!("{reason}; submission in flight"));
            record.receipt.reason_code = Some(FailureCode::EffectUnknown);
            record.receipt.at_ms = self.now_ms;
            let receipt = record.receipt.clone();
            self.push_event(CoreEvent::ReceiptChanged(receipt));
            self.queue.retain(|queued| queued != key);
            self.active = None;
            self.invalidate_permit(&permit_id, "pending attempt cancelled", code);
            // Once an accepted backend submission is stopped in flight, its
            // outcome is unknown to the caller. Fence the shared channel now;
            // a later backend reply cannot grant fresh authority by itself.
            self.isolate_unknown_submission("submission outcome unknown after stop");
            return;
        }
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
                record.receipt.submitted.push(SubmittedAction::Native {
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
            if self.in_flight.as_ref().is_some_and(|p| p.key == key) {
                return;
            }
            if self.records[&key].next_step < action_count(&self.records[&key].intent) {
                self.reject_step(
                    &key,
                    "remaining inputs missed the fixed command deadline".into(),
                    Some(FailureCode::InputRejected),
                );
                return;
            }
            self.finish(
                &key,
                ReceiptStatus::Completed,
                "bounded input ended; effect not verified".into(),
                None,
            );
        }
    }

    fn run_due_steps(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(key) = self.active.clone() else {
            return;
        };
        loop {
            let record = self.records.get(&key).expect("active record");
            if record.receipt.status != ReceiptStatus::Running {
                return;
            }
            let Some((action, offset_ms)) = pending_action_at(&record.intent, record.next_step)
            else {
                return;
            };
            let start = record.started_ms.expect("running start");
            if start + offset_ms > self.now_ms {
                return;
            }
            if let Err(reason) = self.check_intent(&record.intent, Some(start)) {
                self.finish(&key, ReceiptStatus::Cancelled, reason, None);
                return;
            }
            let intent = self.records[&key].intent.clone();
            let permit = self.permits[&intent.permit().id].clone();
            let evidence = self.observations[&permit.evidence_id].clone();
            let command_end_ms = start + intent.duration_ms();
            let valid_until_ms = permit
                .valid_until_ms
                .min(evidence.valid_until_ms())
                .min(self.tasks[intent.task_id()].deadline_ms)
                .min(self.lease_until.unwrap_or(u64::MAX))
                .min(command_end_ms);
            let stop_seq = self.latch.floor();
            let has_one_atomic_action = action_count(&intent) == 1 && !action.is_held();
            let mut native_tracked = false;
            let outcome = match &action {
                PendingAction::Native(primitive) => {
                    if let Err(reason) = self.check_ownership(&key, primitive) {
                        self.reject_step(&key, reason, Some(FailureCode::InputRejected));
                        return;
                    }
                    let Some(native_intent) = intent.native() else {
                        self.reject_step(
                            &key,
                            "native action has no native intent".into(),
                            Some(FailureCode::InputRejected),
                        );
                        return;
                    };
                    let Some(native_evidence) = evidence.native() else {
                        self.reject_step(
                            &key,
                            "native permit has no native observation".into(),
                            Some(FailureCode::InputRejected),
                        );
                        return;
                    };
                    let context = SubmitContext {
                        intent: native_intent,
                        authority: SubmissionAuthority {
                            permit: permit.reference.clone(),
                            stop_seq,
                            valid_until_ms,
                            command_end_ms,
                            evidence: native_evidence.clone(),
                        },
                    };
                    if primitive.needs_hit_test()
                        && context.intent.execution_domain != ExecutionDomain::BackgroundAx
                        && self.owned_document_action_purpose(context.intent).is_none()
                        && let Err((code, reason)) = self.pre_submit_hit_test(&context, primitive)
                    {
                        self.reject_step(&key, reason, Some(code));
                        return;
                    }
                    // Account for a down before submission so a lost acknowledgement
                    // still leaves cleanup owed in the shared ledger.
                    self.ledger.track_down(&key, primitive);
                    native_tracked = true;
                    self.backend.submit(self.now_ms, primitive, &context)
                }
                PendingAction::Browser(browser_action) => {
                    let Some(browser_intent) = intent.browser() else {
                        self.reject_step(
                            &key,
                            "browser action has no browser intent".into(),
                            Some(FailureCode::InputRejected),
                        );
                        return;
                    };
                    let Some(browser_evidence) = evidence.browser() else {
                        self.reject_step(
                            &key,
                            "browser permit has no browser observation".into(),
                            Some(FailureCode::InputRejected),
                        );
                        return;
                    };
                    let context = crate::backend::BrowserSubmitContext {
                        intent: browser_intent,
                        authority: crate::backend::BrowserSubmissionAuthority {
                            permit: permit.reference.clone(),
                            stop_seq,
                            valid_until_ms,
                            command_end_ms,
                            evidence: browser_evidence.clone(),
                        },
                    };
                    self.backend
                        .submit_browser(self.now_ms, browser_action, &context)
                }
            };
            match outcome {
                SubmitOutcome::Rejected => {
                    if native_tracked && let PendingAction::Native(primitive) = &action {
                        self.ledger.untrack_down(&key, primitive);
                    }
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
                    self.isolate_unknown_submission("backend submission outcome unknown");
                    return;
                }
                SubmitOutcome::Applied => {}
                SubmitOutcome::Pending {
                    sequence,
                    reply_deadline_ms,
                } => {
                    self.in_flight = Some(InFlight {
                        sequence,
                        reply_deadline_ms: reply_deadline_ms
                            .min(valid_until_ms.saturating_add(500)),
                        key: key.clone(),
                        action,
                        valid_until_ms,
                        cancelled: None,
                        completion: None,
                    });
                    if self.in_flight.as_ref().unwrap().action.is_held() {
                        self.isolate_unknown_submission("asynchronous held input is unsupported");
                    }
                    return;
                }
            }
            if let PendingAction::Native(primitive) = &action {
                self.ledger.confirm_up(&key, primitive);
            }
            let held = self.ledger.holds(&key);
            let record = self.records.get_mut(&key).expect("active record");
            record.next_step += 1;
            record
                .receipt
                .submitted
                .push(action.submitted(self.now_ms, false));
            record.receipt.release_status = match &action {
                PendingAction::Browser(_) => ReleaseStatus::NotNeeded,
                PendingAction::Native(_) if held => ReleaseStatus::Held,
                PendingAction::Native(_)
                    if record.receipt.submitted.iter().any(|entry| {
                        entry
                            .native_primitive()
                            .is_some_and(Primitive::is_held_class)
                    }) =>
                {
                    ReleaseStatus::Released
                }
                PendingAction::Native(_) => ReleaseStatus::NotNeeded,
            };
            record.receipt.at_ms = self.now_ms;
            if has_one_atomic_action {
                // Applied is a terminal synchronous return. The command end
                // is only an authority bound; this bounded call ended now.
                record.receipt.input_end_ms = Some(self.now_ms);
            }
            if has_one_atomic_action {
                self.finish(
                    &key,
                    ReceiptStatus::Completed,
                    match action {
                        PendingAction::Native(_) => {
                            "atomic input returned; effect not verified".into()
                        }
                        PendingAction::Browser(_) => {
                            "browser action returned; effect not verified".into()
                        }
                    },
                    None,
                );
                return;
            }
        }
    }

    fn isolate_unknown_submission(&mut self, reason: &str) {
        if !self.isolated {
            self.isolated = true;
            self.push_event(CoreEvent::Isolated {
                reason: reason.into(),
            });
        }
        let ids: Vec<_> = self.permits.keys().cloned().collect();
        for id in ids {
            self.invalidate_permit(&id, reason, Some(FailureCode::EffectUnknown));
        }
    }

    /// Reconcile a response even after a stop cancelled the owning record.
    /// The in-flight slot fences the channel until its outcome is accounted.
    fn poll_backend_completion(&mut self) {
        if self.in_flight.is_none() {
            return;
        }
        let (completion, key, valid) = {
            let pending = self
                .in_flight
                .as_mut()
                .expect("checked in-flight submission");
            let completion = pending
                .completion
                .take()
                .or_else(|| self.backend.poll_completion());
            let completion = match completion {
                Some(completion) => completion,
                None if self.now_ms >= pending.reply_deadline_ms => {
                    // This is the only path that retires an absent reply: the
                    // backend contract requires abandon_pending to close its
                    // transport before the shared channel can be recovered.
                    self.backend.abandon_pending();
                    crate::backend::SubmitCompletion {
                        sequence: pending.sequence,
                        session_epoch: pending.key.0,
                        intent_id: pending.key.1.clone(),
                        outcome: SubmitOutcome::Unknown,
                        submitted_at_ms: None,
                        input_end_ms: None,
                        failure_code: Some(FailureCode::EffectUnknown),
                    }
                }
                None => return,
            };
            let key = pending.key.clone();
            let started_ms = self.records[&key]
                .started_ms
                .expect("in-flight submissions have a start time");
            let valid = completion.sequence == pending.sequence
                && completion.session_epoch == key.0
                && completion.intent_id == key.1
                && !matches!(completion.outcome, SubmitOutcome::Pending { .. })
                && match completion.outcome {
                    SubmitOutcome::Applied => {
                        completion.submitted_at_ms.is_some() && completion.failure_code.is_none()
                    }
                    SubmitOutcome::Rejected => completion.submitted_at_ms.is_none(),
                    SubmitOutcome::Unknown => true,
                    SubmitOutcome::Pending { .. } => false,
                }
                && completion
                    .submitted_at_ms
                    .is_none_or(|at| at >= started_ms && at < pending.valid_until_ms)
                && completion.input_end_ms.is_none_or(|end| {
                    end <= pending.reply_deadline_ms
                        && completion.submitted_at_ms.is_some_and(|start| end >= start)
                });
            if valid
                && completion
                    .input_end_ms
                    .or(completion.submitted_at_ms)
                    .is_some_and(|at| at > self.now_ms)
            {
                // A reply can arrive a millisecond after the shell captured
                // time. Keep it until that clock catches up; do not extend
                // authority.
                pending.completion = Some(completion);
                return;
            }
            (completion, key, valid)
        };
        if !valid {
            // Consume the malformed frame, but keep the expected in-flight
            // identity fenced. Recovery must wait for its matching reply or
            // the fixed-deadline transport abandonment path.
            let reply_deadline_reached = self
                .in_flight
                .as_ref()
                .is_some_and(|pending| self.now_ms >= pending.reply_deadline_ms);
            if let Some(pending) = self.in_flight.as_mut() {
                pending.cancelled.get_or_insert((
                    ReceiptStatus::Unknown,
                    "invalid backend completion; do not replay".into(),
                    Some(FailureCode::EffectUnknown),
                ));
            }
            if let Some(record) = self.records.get_mut(&key) {
                record.receipt.status = ReceiptStatus::Unknown;
                record.receipt.reason = Some("invalid backend completion; do not replay".into());
                record.receipt.reason_code = Some(FailureCode::EffectUnknown);
                record.receipt.at_ms = self.now_ms;
            }
            self.active = None;
            self.isolate_unknown_submission("invalid backend completion");
            self.push_event(CoreEvent::ReceiptChanged(
                self.records[&key].receipt.clone(),
            ));
            if reply_deadline_reached {
                // A malformed reply at the deadline cannot substitute for the
                // expected result. Retire only after the backend synchronously
                // closes the old transport, and keep the receipt Unknown.
                self.backend.abandon_pending();
                self.in_flight.take();
            }
            return;
        }
        let pending = self.in_flight.take().unwrap();
        if let Some(at_ms) = completion.submitted_at_ms {
            let record = self.records.get_mut(&key).unwrap();
            record
                .receipt
                .submitted
                .push(pending.action.submitted(at_ms, false));
            if let Some(end) = completion.input_end_ms {
                record.receipt.input_end_ms =
                    Some(record.receipt.input_end_ms.unwrap_or(0).max(end));
            }
        }
        if let Some(primitive) = pending.action.native() {
            if completion.outcome == SubmitOutcome::Rejected {
                self.ledger.untrack_down(&key, primitive);
            } else if completion.outcome == SubmitOutcome::Applied {
                self.ledger.confirm_up(&key, primitive);
            }
        }
        if completion.outcome == SubmitOutcome::Applied {
            self.records.get_mut(&key).unwrap().next_step += 1;
        }
        if pending.cancelled.is_some() {
            let record = self.records.get_mut(&key).unwrap();
            // Stop already made this receipt terminally unknown. Preserve the
            // eventual submission log above, but never let a late completion
            // turn it into success, cancellation, or permission to replay.
            record.receipt.status = ReceiptStatus::Unknown;
            record.receipt.reason_code = Some(FailureCode::EffectUnknown);
            record.receipt.at_ms = self.now_ms;
            let receipt = record.receipt.clone();
            self.push_event(CoreEvent::ReceiptChanged(receipt));
            if completion.outcome == SubmitOutcome::Unknown {
                self.isolate_unknown_submission("cancelled submission outcome unknown");
            }
            return;
        }
        match completion.outcome {
            SubmitOutcome::Applied => {
                let one_atomic_action_returned = match &pending.action {
                    PendingAction::Browser(_) => true,
                    PendingAction::Native(primitive) => matches!(
                        primitive,
                        Primitive::AxAction { .. }
                            | Primitive::Click { .. }
                            | Primitive::Scroll { .. }
                            | Primitive::PointerPosition { .. }
                            | Primitive::ActivateWindow { .. }
                            | Primitive::KeyPress { .. }
                    ),
                };
                if completion.input_end_ms.is_some()
                    && action_count(&self.records[&key].intent) == 1
                    && one_atomic_action_returned
                {
                    // The bounded native or browser call returned. Its fixed
                    // command end is an authority bound, not a held duration.
                    self.finish(
                        &key,
                        ReceiptStatus::Completed,
                        match pending.action {
                            PendingAction::Native(_) => {
                                "atomic input returned; effect not verified".into()
                            }
                            PendingAction::Browser(_) => {
                                "browser action returned; effect not verified".into()
                            }
                        },
                        None,
                    );
                    return;
                }
                let record = self.records.get_mut(&key).unwrap();
                record.receipt.release_status = ReleaseStatus::NotNeeded;
                record.receipt.at_ms = self.now_ms;
                let receipt = record.receipt.clone();
                self.push_event(CoreEvent::ReceiptChanged(receipt));
            }
            SubmitOutcome::Rejected => self.reject_step(
                &key,
                "backend rejected asynchronous input".into(),
                Some(
                    completion
                        .failure_code
                        .unwrap_or(FailureCode::InputRejected),
                ),
            ),
            SubmitOutcome::Unknown => {
                self.finish(
                    &key,
                    ReceiptStatus::Unknown,
                    "submission outcome unknown; do not replay".into(),
                    Some(FailureCode::EffectUnknown),
                );
                self.isolate_unknown_submission("submission outcome unknown");
            }
            SubmitOutcome::Pending { .. } => unreachable!(),
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
        context: &SubmitContext<'_>,
        primitive: &Primitive,
    ) -> Result<(), (FailureCode, String)> {
        let Some(expected) = &context.intent.binding_fingerprint else {
            return if self.config.require_hit_test {
                Err((
                    FailureCode::ContextUnresolved,
                    "no binding proof for a pointer or AX action".into(),
                ))
            } else {
                Ok(())
            };
        };
        let outcome = self
            .backend
            .hit_test(&HitTestRequest { context, primitive });
        match outcome {
            HitTestOutcome::CheckedDuringSubmit => Ok(()),
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

/// Evidence refreshes may advance provenance, but may never swap a permit's
/// instance (even if its digest is identical) or roll back environment state.
fn binding_revision_rolled_back(old: &ExecutionObservation, new: &ExecutionObservation) -> bool {
    match (old.native(), new.native()) {
        (Some(old), Some(new)) => {
            old.binding_proof
                .as_ref()
                .zip(new.binding_proof.as_ref())
                .is_some_and(|(previous, next)| {
                    next.environment_revision < previous.environment_revision
                })
                || old
                    .owned_document_proof
                    .as_deref()
                    .zip(new.owned_document_proof.as_deref())
                    .is_some_and(|(previous, next)| {
                        let generation_rolled_back =
                            previous.receiver.provenance.identity_boundary.namespace
                                == next.receiver.provenance.identity_boundary.namespace
                                && next.receiver.provenance.identity_boundary.generation
                                    < previous.receiver.provenance.identity_boundary.generation;
                        generation_rolled_back
                            || previous.same_instance(next)
                                && next.environment_revision < previous.environment_revision
                    })
        }
        (None, None) => old
            .browser()
            .zip(new.browser())
            .is_some_and(|(previous, next)| {
                next.scope.generation < previous.scope.generation
                    || next.environment_revision < previous.environment_revision
            }),
        _ => false,
    }
}

/// A source target key is stable across refreshed reads. Native bindings keep
/// their existing target namespace; browser bindings are snapshot-scoped refs,
/// so key them by the fixed document context and checkbox target instead.
fn observation_target_key(observation: &ExecutionObservation) -> String {
    match observation {
        ExecutionObservation::Native { observation } => {
            format!("native:{}", observation.target_ref)
        }
        ExecutionObservation::Browser { observation } => format!(
            "browser:{}:{}:{}",
            observation.scope.scope_id,
            observation.scope.generation,
            observation.binding.target_context_hash
        ),
    }
}

fn same_binding_identity(old: &ExecutionObservation, new: &ExecutionObservation) -> bool {
    match (old.native(), new.native()) {
        (Some(old), Some(new)) => same_native_binding_identity(old, new),
        (None, None) => old.browser().zip(new.browser()).is_some_and(|(old, new)| {
            old.scope == new.scope
                && old.surface == new.surface
                && old.binding.context == new.binding.context
                && old.binding.target_context_hash == new.binding.target_context_hash
                && old.binding.ax_node_id == new.binding.ax_node_id
                && old.binding.backend_dom_node_id == new.binding.backend_dom_node_id
                && old.binding.role == new.binding.role
                && old.binding.name == new.binding.name
                && new.environment_revision >= old.environment_revision
        }),
        _ => false,
    }
}

fn same_native_binding_identity(old: &Observation, new: &Observation) -> bool {
    if old.purpose != new.purpose || old.execution_domain != new.execution_domain {
        return false;
    }
    match old.purpose {
        ObservationPurpose::OwnedDocumentValueSetter => old
            .owned_document_proof
            .as_deref()
            .zip(new.owned_document_proof.as_deref())
            .is_some_and(|(a, b)| {
                a.same_instance(b) && b.environment_revision >= a.environment_revision
            }),
        ObservationPurpose::OwnedDocumentSave => old
            .owned_document_proof
            .as_deref()
            .zip(new.owned_document_proof.as_deref())
            .is_some_and(|(a, b)| {
                a.same_instance(b) && b.environment_revision >= a.environment_revision
            }),
        ObservationPurpose::RowSelection => same_row_binding_identity(old, new),
        _ => match (&old.binding_proof, &new.binding_proof) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.same_instance(b) && b.environment_revision >= a.environment_revision
            }
            _ => false,
        },
    }
}

fn row_read_key(observation: &Observation) -> Option<RowReadKey> {
    if observation.purpose != ObservationPurpose::RowSelection {
        return None;
    }
    let proof = observation.binding_proof.as_ref()?;
    Some(RowReadKey {
        session_epoch: observation.session_epoch,
        surface_id: observation.surface.id.clone(),
        surface_epoch: observation.surface.epoch,
        snapshot_id: proof.snapshot_id.as_str().to_owned(),
        source_sequence: proof.continuity.source_sequence,
    })
}

fn same_row_binding_identity(old: &Observation, new: &Observation) -> bool {
    let Some((a, b)) = old.binding_proof.as_ref().zip(new.binding_proof.as_ref()) else {
        return false;
    };
    if old.source_sequence != new.source_sequence
        || old.observed_ms != new.observed_ms
        || old.clock_domain != new.clock_domain
        || old.session_epoch != new.session_epoch
        || old.surface != new.surface
        || old.target_ref != new.target_ref
        || old.dependencies != new.dependencies
        || a.continuity.source_sequence != old.source_sequence
        || b.continuity.source_sequence != new.source_sequence
        || a.snapshot_id != b.snapshot_id
        || a.continuity != b.continuity
        || !a.supports_row_selection()
        || !b.supports_row_selection()
    {
        return false;
    }
    a.same_instance(b) && b.environment_revision >= a.environment_revision
}

fn has_owned_document_save(intent: &ActionIntent) -> bool {
    intent.steps.iter().any(|step| {
        matches!(
            step.primitive,
            Primitive::AxAction {
                action: interaction_contracts::desktop::primitive::AxActionKind::SaveOwnedDocument {
                    ..
                }
            }
        )
    })
}
