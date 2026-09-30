//! TaskRuntime: the owner of root tasks, their budget and attempt admission
//! (design 11.2, 13 and 16.6). One root is active at a time; it is processed
//! serially, so every decision here sees a single consistent state.

use std::collections::{BTreeSet, VecDeque};

use interaction_contracts::desktop::device::{ReleaseStatus, RootGrant};
use interaction_contracts::desktop::ids::{AttemptId, ConfirmationId, DecisionId, RootTaskId};
use interaction_contracts::desktop::operator::{
    ActionSummary, ConfirmationKind, ConfirmationRequest, RootView,
};
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle};
use interaction_contracts::desktop::task::{
    Activation, AdmitAttempt, AdmitOutcome, AdmitRejection, AttemptState, GoalSpec, RootBudgetSpec,
    RootEvent, RootState,
};
use rusqlite::Transaction;

use super::budget::{BudgetKind, BudgetState};
use super::goal::{CompileError, PlanRejection, compile_goal, confirmation_for, validate_plan};
use super::state_machine::{TransitionError, transition};
use super::store::{AttemptRow, RootRow, TaskStore};
use crate::environment::pack::AppPack;
use crate::infrastructure::storage::{Storage, StoreError};
use crate::ports::{ConsumeOutcome, ConsumeRequest, DecisionLedger, DecisionLifecycle};

#[derive(Debug, Clone)]
pub struct TaskConfig {
    pub packs: Vec<AppPack>,
    pub registered_skills: Vec<String>,
    /// `deadline_ms` is the window a root gets once the user confirms it.
    pub default_budget: RootBudgetSpec,
}

impl TaskConfig {
    pub fn new(packs: Vec<AppPack>) -> Self {
        Self {
            packs,
            registered_skills: [
                "ui.read_value",
                "ui.ensure_value",
                "ui.enter_text",
                "ui.scroll_to",
                "ui.activate",
                "ui.activate_window",
                "ui.revisit",
            ]
            .map(String::from)
            .to_vec(),
            default_budget: RootBudgetSpec {
                deadline_ms: 120_000,
                max_attempts: 12,
                max_observations: 40,
                max_recoveries: 4,
                max_replans: 2,
            },
        }
    }
}

#[derive(Debug)]
pub enum TaskError {
    Transition(TransitionError),
    Store(StoreError),
    NoActiveRoot,
    UnknownConfirmation,
    Invalid(String),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transition(error) => write!(f, "{error}"),
            Self::Store(error) => write!(f, "{error}"),
            Self::NoActiveRoot => f.write_str("there is no active root task"),
            Self::UnknownConfirmation => f.write_str("no such confirmation is pending"),
            Self::Invalid(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl From<TransitionError> for TaskError {
    fn from(error: TransitionError) -> Self {
        Self::Transition(error)
    }
}

impl From<StoreError> for TaskError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for TaskError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sql(error))
    }
}

#[derive(Debug)]
pub enum PlanInstallError {
    Rejected(PlanRejection),
    Task(TaskError),
}

impl From<TaskError> for PlanInstallError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

impl From<PlanRejection> for PlanInstallError {
    fn from(rejection: PlanRejection) -> Self {
        Self::Rejected(rejection)
    }
}

#[derive(Debug)]
pub enum DraftError {
    Compile(CompileError),
    Task(TaskError),
}

impl From<TaskError> for DraftError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

impl From<CompileError> for DraftError {
    fn from(error: CompileError) -> Self {
        Self::Compile(error)
    }
}

#[derive(Debug, Clone)]
pub enum Pending {
    Goal(ConfirmationRequest),
    Effect {
        request: ConfirmationRequest,
        decision: DecisionId,
    },
}

impl Pending {
    pub fn request(&self) -> &ConfirmationRequest {
        match self {
            Self::Goal(request) | Self::Effect { request, .. } => request,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveRoot {
    pub row: RootRow,
    /// Present once the goal is frozen.
    pub budget: Option<BudgetState>,
    pub pending: Option<Pending>,
    pub pack: Option<AppPack>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    Started(RootTaskId),
    Queued,
}

#[derive(Debug, PartialEq)]
pub enum ConfirmOutcome {
    GoalFrozen {
        goal: Box<GoalSpec>,
        grant: RootGrant,
    },
    GoalDeclined,
    EffectApproved {
        decision: DecisionId,
    },
    EffectDenied,
}

/// What stopping a root requires of the rest of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDirective {
    /// Nothing had reached the device: the root is already cancelled.
    Finished,
    /// The device must confirm that input is released.
    AwaitRelease,
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Frozen roots that must be revalidated and explicitly resumed.
    pub revalidating: Vec<RootTaskId>,
    /// Roots that never reached the device and were simply cancelled.
    pub cancelled: Vec<RootTaskId>,
    /// Roots whose cancellation continues.
    pub cancelling: Vec<RootTaskId>,
    /// Attempts whose outcome is unknown; their quota stays spent.
    pub unknown_attempts: Vec<AttemptRow>,
}

/// Where a test may make the admission transaction fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitFault {
    AfterTaskWrites,
    AfterReactionWrites,
    BeforeCommit,
}

enum AdmitTxError {
    Rejected(AdmitRejection),
    Store(StoreError),
}

impl From<rusqlite::Error> for AdmitTxError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sql(error))
    }
}

impl From<StoreError> for AdmitTxError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

fn activation_of(row: &AttemptRow) -> Activation {
    Activation {
        attempt_id: row.attempt_id.clone(),
        root_task_id: row.root_id.clone(),
        root_epoch: row.root_epoch,
        activation_seq: row.activation_seq,
        granted: row.quota,
        deadline_ms: row.deadline_ms,
    }
}

pub struct TaskRuntime {
    config: TaskConfig,
    active: Option<ActiveRoot>,
    finished: Option<RootRow>,
    queue: VecDeque<String>,
    /// Effect confirmations the user approved, by the decision they covered.
    approved: BTreeSet<DecisionId>,
    fault: Option<AdmitFault>,
}

impl TaskRuntime {
    pub fn new(config: TaskConfig) -> Self {
        Self {
            config,
            active: None,
            finished: None,
            queue: VecDeque::new(),
            approved: BTreeSet::new(),
            fault: None,
        }
    }

    pub fn config(&self) -> &TaskConfig {
        &self.config
    }

    pub fn active(&self) -> Option<&ActiveRoot> {
        self.active.as_ref()
    }

    pub fn active_state(&self) -> Option<RootState> {
        self.active.as_ref().map(|root| root.row.state)
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// The most recent root that reached a final state.
    pub fn last_finished(&self) -> Option<&RootRow> {
        self.finished.as_ref()
    }

    pub fn next_queued(&mut self) -> Option<String> {
        self.queue.pop_front()
    }

    pub fn inject_admit_fault(&mut self, fault: Option<AdmitFault>) {
        self.fault = fault;
    }

    /// A view of the current root, or of the last one that finished.
    pub fn view(&self) -> Option<RootView> {
        let (row, budget) = match (&self.active, &self.finished) {
            (Some(active), _) => (&active.row, active.budget.as_ref()),
            (None, Some(row)) => (row, None),
            _ => return None,
        };
        Some(RootView {
            root_task_id: row.root_id.clone(),
            task_revision: row.task_revision,
            root_epoch: row.root_epoch,
            state: row.state,
            headline: row
                .goal
                .as_ref()
                .map(|goal| goal.user_text.clone())
                .unwrap_or_else(|| row.user_text.clone()),
            attempts_used: budget.map_or(0, |b| b.attempts_used),
            attempts_max: budget.map_or(0, |b| b.spec.max_attempts),
        })
    }

    fn finish_active(&mut self) {
        if let Some(active) = self.active.take() {
            self.finished = Some(active.row);
        }
        self.approved.clear();
    }

    /// Apply one event: check the table, run `mutate`, persist, log.
    fn step(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        event: RootEvent,
        mutate: impl FnOnce(&mut RootRow),
        extra: impl FnOnce(&Transaction<'_>) -> Result<(), StoreError>,
    ) -> Result<RootState, TaskError> {
        let active = self.active.as_mut().ok_or(TaskError::NoActiveRoot)?;
        let next = transition(active.row.state, event, active.row.state_origin)?;
        let mut row = active.row.clone();
        row.state = next;
        mutate(&mut row);
        storage.transaction::<_, StoreError>(|tx| {
            TaskStore::save_root(tx, &row)?;
            extra(tx)
        })?;
        storage.log(
            now_ms,
            Some(row.root_id.as_str()),
            "root_state",
            &format!("{event:?} -> {next:?}"),
        );
        active.row = row;
        if next.is_terminal() {
            self.finish_active();
        }
        Ok(next)
    }

    fn plain_step(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        event: RootEvent,
    ) -> Result<RootState, TaskError> {
        self.step(storage, now_ms, event, |_| {}, |_| Ok(()))
    }

    // ---- Creating and compiling -------------------------------------------

    pub fn create_root(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        text: &str,
    ) -> Result<CreateOutcome, TaskError> {
        if text.trim().is_empty() {
            return Err(TaskError::Invalid("a task needs some text".into()));
        }
        if self.active.is_some() {
            self.queue.push_back(text.to_string());
            return Ok(CreateOutcome::Queued);
        }
        let seq = TaskStore::next_seq(storage.read())?;
        let root_id = RootTaskId::new(format!("root-{seq}"))
            .map_err(|error| TaskError::Invalid(error.to_string()))?;
        let next = transition(RootState::Draft, RootEvent::SubmitGoal, None)?;
        let mut row = RootRow {
            root_id: root_id.clone(),
            seq,
            user_text: text.to_string(),
            state: RootState::Draft,
            state_origin: None,
            task_revision: 1,
            root_epoch: 1,
            draft: None,
            goal: None,
            plan_revision: 0,
            plan: None,
            created_ms: now_ms,
            frozen_ms: None,
            deadline_ms: None,
            cancel_requested: false,
            cleanup: ReleaseStatus::NotNeeded,
            activation_seq: 0,
            failure: None,
        };
        storage.transaction::<_, StoreError>(|tx| {
            TaskStore::insert_root(tx, &row)?;
            row.state = next;
            TaskStore::save_root(tx, &row)
        })?;
        storage.log(
            now_ms,
            Some(root_id.as_str()),
            "root_state",
            "SubmitGoal -> CompilingGoal",
        );
        self.active = Some(ActiveRoot {
            row,
            budget: None,
            pending: None,
            pack: None,
        });
        Ok(CreateOutcome::Started(root_id))
    }

    /// The planner answered a goal-compile request. On success the goal waits
    /// for the user; nothing runs before they confirm it.
    pub fn accept_draft(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        draft: &GoalDraft,
        bundle: &PlanBundle,
    ) -> Result<ConfirmationRequest, DraftError> {
        let root = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        transition(root.row.state, RootEvent::DraftPassed, None).map_err(TaskError::from)?;
        let (goal, pack) = compile_goal(
            &root.row.root_id,
            &root.row.user_text,
            draft,
            self.config.default_budget,
            &self.config.packs,
        )?;
        validate_plan(&goal, bundle, &pack, &self.config.registered_skills)
            .map_err(CompileError::Plan)?;
        let request = confirmation_for(&goal);
        self.step(
            storage,
            now_ms,
            RootEvent::DraftPassed,
            |row| {
                row.draft = Some(goal.clone());
                row.plan = Some(bundle.clone());
            },
            |_| Ok(()),
        )?;
        let active = self.active.as_mut().expect("the step kept the root active");
        active.pending = Some(Pending::Goal(request.clone()));
        active.pack = Some(pack);
        Ok(request)
    }

    pub fn fail_compile(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        reason: &str,
    ) -> Result<(), TaskError> {
        let reason = reason.to_string();
        self.step(
            storage,
            now_ms,
            RootEvent::CompileFailed,
            |row| row.failure = Some(reason),
            |_| Ok(()),
        )?;
        Ok(())
    }

    // ---- Confirmation -------------------------------------------------------

    /// Ask the user to approve one action whose effect the goal does not cover.
    /// The root releases its input and waits.
    pub fn request_effect_confirmation(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        decision: DecisionId,
        action: ActionSummary,
    ) -> Result<ConfirmationRequest, TaskError> {
        let root = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        let request = ConfirmationRequest {
            confirmation_id: ConfirmationId::new(format!("conf-{}-{}", root.row.root_id, decision))
                .map_err(|error| TaskError::Invalid(error.to_string()))?,
            root_task_id: root.row.root_id.clone(),
            task_revision: root.row.task_revision,
            kind: ConfirmationKind::Effect { action },
        };
        let origin = root.row.state;
        self.step(
            storage,
            now_ms,
            RootEvent::NeedUserInfo,
            |row| row.state_origin = Some(origin),
            |_| Ok(()),
        )?;
        self.active.as_mut().expect("still active").pending = Some(Pending::Effect {
            request: request.clone(),
            decision,
        });
        Ok(request)
    }

    pub fn answer_confirmation(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        id: &ConfirmationId,
        approved: bool,
    ) -> Result<ConfirmOutcome, TaskError> {
        let pending = self
            .active
            .as_ref()
            .and_then(|root| root.pending.clone())
            .filter(|pending| &pending.request().confirmation_id == id)
            .ok_or(TaskError::UnknownConfirmation)?;
        self.active.as_mut().expect("checked").pending = None;
        match (pending, approved) {
            (Pending::Goal(_), true) => self.freeze(storage, now_ms),
            (Pending::Goal(_), false) => {
                // Nothing reached the device, so a declined goal is simply cancelled.
                self.stop_requested(storage, now_ms)?;
                Ok(ConfirmOutcome::GoalDeclined)
            }
            (Pending::Effect { decision, .. }, true) => {
                self.plain_step(storage, now_ms, RootEvent::UserAnswered)?;
                self.approved.insert(decision.clone());
                Ok(ConfirmOutcome::EffectApproved { decision })
            }
            (Pending::Effect { .. }, false) => {
                self.stop_requested(storage, now_ms)?;
                Ok(ConfirmOutcome::EffectDenied)
            }
        }
    }

    /// The user confirmed the goal: freeze it. From here its success criteria,
    /// scope and budget never change.
    fn freeze(&mut self, storage: &mut Storage, now_ms: u64) -> Result<ConfirmOutcome, TaskError> {
        let root = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        let mut goal = root
            .row
            .draft
            .clone()
            .ok_or_else(|| TaskError::Invalid("there is no goal to freeze".into()))?;
        goal.budget.deadline_ms = now_ms.saturating_add(goal.budget.deadline_ms);
        let grant = RootGrant {
            root_id: root.row.root_id.as_str().to_string(),
            revision: goal.task_revision,
            root_epoch: root.row.root_epoch,
            deadline_ms: goal.budget.deadline_ms,
        };
        let root_id = root.row.root_id.clone();
        let frozen = goal.clone();
        self.step(
            storage,
            now_ms,
            RootEvent::UserConfirmed,
            |row| {
                row.draft = None;
                row.deadline_ms = Some(frozen.budget.deadline_ms);
                row.goal = Some(frozen);
                row.frozen_ms = Some(now_ms);
                row.plan_revision = 1;
            },
            |tx| TaskStore::init_budget(tx, root_id.as_str()),
        )?;
        self.active.as_mut().expect("still active").budget = Some(BudgetState::new(goal.budget));
        Ok(ConfirmOutcome::GoalFrozen {
            goal: Box::new(goal),
            grant,
        })
    }

    pub fn admission_passed(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::AdmissionPassed)
    }

    // ---- Waiting ------------------------------------------------------------

    pub fn await_evidence(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::AwaitEvidence)
    }

    pub fn evidence_arrived(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::EvidenceArrived)
    }

    // ---- Planning -----------------------------------------------------------

    /// The root needs a new plan. A replan is spent when it is asked for, not
    /// when an answer arrives, so a planner that never answers still costs
    /// something. `false` means the replan budget is gone and nothing changed.
    pub fn await_plan(&mut self, storage: &mut Storage, now_ms: u64) -> Result<bool, TaskError> {
        let active = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        let budget = active
            .budget
            .as_ref()
            .ok_or_else(|| TaskError::Invalid("the goal is not frozen yet".into()))?;
        if !budget.can_spend(BudgetKind::Replan, 1) {
            return Ok(false);
        }
        let root = active.row.root_id.clone();
        self.step(
            storage,
            now_ms,
            RootEvent::AwaitPlan,
            |_| {},
            |tx| TaskStore::spend(tx, root.as_str(), BudgetKind::Replan, 1),
        )?;
        if let Some(budget) = self.active.as_mut().and_then(|a| a.budget.as_mut()) {
            budget.spend(BudgetKind::Replan, 1).expect("checked above");
        }
        Ok(true)
    }

    /// The planner answered. A plan is checked against the frozen goal exactly
    /// like the first one; it may refine the way to the goal, never widen it.
    /// `None` means the planner reviewed the situation and left the plan as it
    /// is. A refused plan leaves the root waiting, and the caller decides what
    /// that means.
    pub fn install_plan(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        bundle: Option<&PlanBundle>,
    ) -> Result<RootState, PlanInstallError> {
        let active = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        if active.row.state != RootState::WaitingPlan {
            return Err(TaskError::Invalid("no plan is being waited for".into()).into());
        }
        let revision = if let Some(bundle) = bundle {
            let goal = active
                .row
                .goal
                .as_ref()
                .ok_or_else(|| TaskError::Invalid("the goal is not frozen yet".into()))?;
            let pack = active
                .pack
                .as_ref()
                .ok_or_else(|| TaskError::Invalid("the goal has no environment pack".into()))?;
            validate_plan(goal, bundle, pack, &self.config.registered_skills)?;
            active.row.plan_revision + 1
        } else {
            active.row.plan_revision
        };
        let bundle = bundle.cloned();
        Ok(self.step(
            storage,
            now_ms,
            RootEvent::PlanInstalled,
            |row| {
                row.plan_revision = revision;
                if let Some(bundle) = bundle {
                    row.plan = Some(bundle);
                }
            },
            |_| Ok(()),
        )?)
    }

    // ---- Budget -------------------------------------------------------------

    /// Spend one unit of a non-attempt budget, such as an observation.
    pub fn spend(
        &mut self,
        storage: &mut Storage,
        kind: BudgetKind,
        amount: u32,
    ) -> Result<bool, TaskError> {
        let active = self.active.as_mut().ok_or(TaskError::NoActiveRoot)?;
        let budget = active
            .budget
            .as_mut()
            .ok_or_else(|| TaskError::Invalid("the goal is not frozen yet".into()))?;
        if !budget.can_spend(kind, amount) {
            return Ok(false);
        }
        let root = active.row.root_id.clone();
        storage
            .transaction::<_, StoreError>(|tx| TaskStore::spend(tx, root.as_str(), kind, amount))?;
        budget.spend(kind, amount).expect("checked above");
        Ok(true)
    }

    // ---- Attempt admission ----------------------------------------------------

    /// Admit one attempt. Everything is checked against the authoritative
    /// state, then the quota, the attempt and the decision's consumption are
    /// written in **one transaction** (design 11.2): either all of it exists
    /// afterwards, or none of it does.
    pub fn admit_attempt(
        &mut self,
        storage: &mut Storage,
        ledger: &dyn DecisionLedger,
        request: &AdmitAttempt,
        now_ms: u64,
        outbox_payload: &str,
    ) -> Result<AdmitOutcome, TaskError> {
        let reject = |reason| Ok(AdmitOutcome::Rejected { reason });

        // A repeated request is answered with what already happened. The old
        // activation is only repeated while the attempt is still in flight: for
        // one that ended, or whose outcome is unknown after a restart, it could
        // be mistaken for permission to start input again.
        if let Some(existing) =
            TaskStore::load_attempt(storage.read(), request.attempt_id.as_str())?
        {
            let same = existing.decision_id == request.decision_id
                && existing.generation == request.generation
                && existing.root_id == request.root_task_id;
            return if !same {
                reject(AdmitRejection::AttemptIdConflict)
            } else if matches!(
                existing.state,
                AttemptState::Active | AttemptState::Dispatched
            ) {
                Ok(AdmitOutcome::Activated {
                    activation: activation_of(&existing),
                })
            } else {
                reject(AdmitRejection::DecisionAlreadyConsumed)
            };
        }

        let Some(active) = self.active.as_ref() else {
            return reject(AdmitRejection::RootNotRunning);
        };
        let row = &active.row;
        if row.root_id != request.root_task_id
            || !row.state.may_produce_input()
            || row.cancel_requested
        {
            return reject(AdmitRejection::RootNotRunning);
        }
        if request.root_epoch != row.root_epoch {
            return reject(AdmitRejection::RootEpochStale);
        }
        if request.task_revision != row.task_revision || request.plan_revision != row.plan_revision
        {
            return reject(AdmitRejection::VersionMismatch);
        }
        let root_deadline = row.deadline_ms.unwrap_or(0);
        if now_ms >= root_deadline || request.deadline_ms > root_deadline {
            return reject(AdmitRejection::DeadlineExpired);
        }
        let Some(budget) = active.budget.as_ref() else {
            return reject(AdmitRejection::RootNotRunning);
        };
        let Some(granted) = budget.grantable(request.quota) else {
            return reject(AdmitRejection::BudgetExhausted);
        };
        let Some(goal) = row.goal.as_ref() else {
            return reject(AdmitRejection::RootNotRunning);
        };
        use interaction_contracts::desktop::effect::EffectClass;
        let authorised = match request.effect_class {
            EffectClass::Unknown | EffectClass::External => false,
            class if class <= goal.max_effect_class => true,
            _ => self.approved.contains(&request.decision_id),
        };
        if !authorised {
            return reject(AdmitRejection::EffectNotAuthorized);
        }

        let fault = self.fault;
        let root_id = row.root_id.clone();
        let outcome: Result<Activation, AdmitTxError> = storage.transaction(|tx| {
            let activation_seq = TaskStore::next_activation_seq(tx, root_id.as_str())?;
            let attempt = AttemptRow {
                attempt_id: request.attempt_id.clone(),
                root_id: root_id.clone(),
                decision_id: request.decision_id.clone(),
                generation: request.generation,
                state: AttemptState::Active,
                skill: request.skill.clone(),
                subgoal: request.subgoal.clone(),
                quota: granted,
                activation_seq,
                root_epoch: request.root_epoch,
                task_revision: request.task_revision,
                plan_revision: request.plan_revision,
                deadline_ms: request.deadline_ms,
                effect_class: request.effect_class,
                source: request.source,
                refunded: false,
                created_ms: now_ms,
                intent_id: None,
            };
            TaskStore::insert_attempt(tx, &attempt)?;
            TaskStore::spend(tx, root_id.as_str(), BudgetKind::Attempt, 1)?;
            if fault == Some(AdmitFault::AfterTaskWrites) {
                return Err(StoreError::Fault("after task writes").into());
            }
            let consumed = ledger.consume_for_admission(
                tx,
                &ConsumeRequest {
                    decision_id: &request.decision_id,
                    generation: request.generation,
                    attempt_id: &request.attempt_id,
                    root_id: &root_id,
                    source: request.source,
                    outbox_payload,
                },
            )?;
            match consumed {
                ConsumeOutcome::Consumed => {}
                ConsumeOutcome::NotPending(DecisionLifecycle::Consumed) => {
                    return Err(AdmitTxError::Rejected(
                        AdmitRejection::DecisionAlreadyConsumed,
                    ));
                }
                ConsumeOutcome::NotPending(_)
                | ConsumeOutcome::GenerationMismatch { .. }
                | ConsumeOutcome::Unknown => {
                    return Err(AdmitTxError::Rejected(AdmitRejection::DecisionNotPending));
                }
            }
            if fault == Some(AdmitFault::AfterReactionWrites) {
                return Err(StoreError::Fault("after reaction writes").into());
            }
            if fault == Some(AdmitFault::BeforeCommit) {
                return Err(StoreError::Fault("before commit").into());
            }
            Ok(activation_of(&attempt))
        });
        match outcome {
            Ok(activation) => {
                let active = self.active.as_mut().expect("checked above");
                if let Some(budget) = active.budget.as_mut() {
                    budget
                        .spend(BudgetKind::Attempt, 1)
                        .expect("grantable was checked");
                }
                storage.log(
                    now_ms,
                    Some(root_id.as_str()),
                    "attempt_admitted",
                    request.attempt_id.as_str(),
                );
                Ok(AdmitOutcome::Activated { activation })
            }
            Err(AdmitTxError::Rejected(reason)) => reject(reason),
            Err(AdmitTxError::Store(error)) => Err(TaskError::Store(error)),
        }
    }

    pub fn attempt_dispatched(
        &mut self,
        storage: &mut Storage,
        attempt: &AttemptId,
        intent_id: &str,
    ) -> Result<(), TaskError> {
        storage.transaction::<_, StoreError>(|tx| {
            TaskStore::set_attempt_state(tx, attempt.as_str(), AttemptState::Dispatched)?;
            TaskStore::set_attempt_intent(tx, attempt.as_str(), intent_id)
        })?;
        Ok(())
    }

    /// Record how an attempt ended. When it never produced input its quota is
    /// given back, exactly once.
    pub fn attempt_ended(
        &mut self,
        storage: &mut Storage,
        attempt: &AttemptId,
        state: AttemptState,
        produced_no_input: bool,
    ) -> Result<(), TaskError> {
        let Some(existing) = TaskStore::load_attempt(storage.read(), attempt.as_str())? else {
            return Err(TaskError::Invalid(format!("unknown attempt {attempt}")));
        };
        if existing.state.is_final() {
            return Ok(());
        }
        let refund = produced_no_input && state.is_final();
        let refunded = storage.transaction::<_, StoreError>(|tx| {
            TaskStore::set_attempt_state(tx, attempt.as_str(), state)?;
            if refund && TaskStore::mark_refunded(tx, attempt.as_str())? {
                TaskStore::refund(tx, existing.root_id.as_str(), BudgetKind::Attempt, 1)?;
                return Ok(true);
            }
            Ok(false)
        })?;
        if refunded && let Some(budget) = self.active.as_mut().and_then(|a| a.budget.as_mut()) {
            budget.refund(BudgetKind::Attempt, 1);
        }
        Ok(())
    }

    // ---- Completion -----------------------------------------------------------

    pub fn predicates_satisfied(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::PredicatesSatisfied)
    }

    pub fn input_ended_unverified(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        reason: &str,
    ) -> Result<RootState, TaskError> {
        let reason = reason.to_string();
        self.step(
            storage,
            now_ms,
            RootEvent::InputEndedUnverified,
            |row| row.failure = Some(reason),
            |_| Ok(()),
        )
    }

    pub fn unrecoverable(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        reason: &str,
    ) -> Result<RootState, TaskError> {
        let reason = reason.to_string();
        self.step(
            storage,
            now_ms,
            RootEvent::Unrecoverable,
            |row| row.failure = Some(reason),
            |_| Ok(()),
        )
    }

    pub fn deadline_expired(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.step(
            storage,
            now_ms,
            RootEvent::DeadlineExpired,
            |row| {
                row.cancel_requested = true;
                row.root_epoch += 1;
            },
            |_| Ok(()),
        )
    }

    // ---- Stopping, pausing and resuming ----------------------------------------

    /// Cancel the root. The epoch moves on at once, so nothing admitted under
    /// the old one can start, and the root stays in CANCELLING until the
    /// device confirms it released its input.
    pub fn stop_requested(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<StopDirective, TaskError> {
        let frozen = self
            .active
            .as_ref()
            .ok_or(TaskError::NoActiveRoot)?
            .row
            .goal
            .is_some();
        self.step(
            storage,
            now_ms,
            RootEvent::StopRequested,
            |row| {
                row.cancel_requested = true;
                if frozen {
                    row.root_epoch += 1;
                }
            },
            |_| Ok(()),
        )?;
        if frozen {
            Ok(StopDirective::AwaitRelease)
        } else {
            self.plain_step(storage, now_ms, RootEvent::CancelFinished)?;
            Ok(StopDirective::Finished)
        }
    }

    pub fn pause_requested(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::PauseRequested)
    }

    /// The device reported how releasing input went. A confirmed release
    /// finishes a pause or a cancellation; anything else is recorded and the
    /// root stays where it is, with the stop unconfirmed.
    pub fn inputs_released(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        cleanup: ReleaseStatus,
    ) -> Result<RootState, TaskError> {
        let state = self.active_state().ok_or(TaskError::NoActiveRoot)?;
        let event = match state {
            RootState::Pausing => RootEvent::InputsReleased,
            RootState::Cancelling => RootEvent::CancelFinished,
            other => return Ok(other),
        };
        if matches!(cleanup, ReleaseStatus::Released | ReleaseStatus::NotNeeded) {
            self.step(
                storage,
                now_ms,
                event,
                |row| row.cleanup = cleanup,
                |_| Ok(()),
            )
        } else {
            let root = self.active.as_mut().expect("checked");
            let mut row = root.row.clone();
            row.cleanup = cleanup;
            storage.transaction::<_, StoreError>(|tx| TaskStore::save_root(tx, &row))?;
            root.row = row;
            Ok(state)
        }
    }

    pub fn resume_requested(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::ResumeRequested)
    }

    pub fn revalidation_passed(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.plain_step(storage, now_ms, RootEvent::RevalidationPassed)
    }

    // ---- Recovery ------------------------------------------------------------------

    /// Load what survived a restart. Nothing resumes by itself: a frozen root
    /// waits for revalidation, its unknown attempts keep their quota, and a
    /// root that never reached the device is just cancelled.
    pub fn recover(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RecoveryReport, TaskError> {
        let mut report = RecoveryReport::default();
        for mut row in TaskStore::load_unfinished(storage.read())? {
            let root_id = row.root_id.clone();
            let frozen = row.goal.is_some();
            if row.state == RootState::Cancelling {
                report.cancelling.push(root_id);
            } else if !frozen {
                // It never produced input: cancel it cleanly.
                row.state = RootState::Cancelled;
                row.cancel_requested = true;
                storage.transaction::<_, StoreError>(|tx| TaskStore::save_root(tx, &row))?;
                storage.log(
                    now_ms,
                    Some(row.root_id.as_str()),
                    "recovery",
                    "cancelled an unfrozen root",
                );
                report.cancelled.push(root_id);
                self.finished = Some(row);
                continue;
            } else {
                row.state = transition(row.state, RootEvent::ProcessRestarted, row.state_origin)?;
                storage.transaction::<_, StoreError>(|tx| TaskStore::save_root(tx, &row))?;
                storage.log(
                    now_ms,
                    Some(row.root_id.as_str()),
                    "recovery",
                    "needs revalidation",
                );
                report.revalidating.push(root_id);
            }
            // Attempts that were in flight have an unknown outcome.
            let in_flight: Vec<AttemptRow> =
                TaskStore::load_attempts(storage.read(), row.root_id.as_str())?
                    .into_iter()
                    .filter(|attempt| {
                        matches!(
                            attempt.state,
                            AttemptState::Active | AttemptState::Dispatched
                        )
                    })
                    .collect();
            for attempt in &in_flight {
                storage.transaction::<_, StoreError>(|tx| {
                    TaskStore::set_attempt_state(
                        tx,
                        attempt.attempt_id.as_str(),
                        AttemptState::Unknown,
                    )
                })?;
            }
            report
                .unknown_attempts
                .extend(in_flight.into_iter().map(|mut attempt| {
                    attempt.state = AttemptState::Unknown;
                    attempt
                }));
            let budget = row.goal.as_ref().map(|goal| {
                let counts = TaskStore::load_budget(storage.read(), row.root_id.as_str())
                    .unwrap_or_default();
                let mut state = BudgetState::new(goal.budget);
                state.attempts_used = counts.attempts;
                state.observations_used = counts.observations;
                state.recoveries_used = counts.recoveries;
                state.replans_used = counts.replans;
                state
            });
            let pack = row
                .goal
                .as_ref()
                .and_then(|goal| {
                    self.config
                        .packs
                        .iter()
                        .find(|p| p.bundle_id == goal.scope.app)
                })
                .cloned();
            self.active = Some(ActiveRoot {
                row,
                budget,
                pending: None,
                pack,
            });
        }
        Ok(report)
    }

    pub fn attempts(&self, storage: &Storage) -> Result<Vec<AttemptRow>, TaskError> {
        match &self.active {
            Some(active) => Ok(TaskStore::load_attempts(
                storage.read(),
                active.row.root_id.as_str(),
            )?),
            None => Ok(Vec::new()),
        }
    }
}
