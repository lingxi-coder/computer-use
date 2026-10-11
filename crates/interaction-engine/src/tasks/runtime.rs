//! TaskRuntime: the owner of root tasks, their budget and attempt admission
//! (design 11.2, 13 and 16.6). One root is active at a time; it is processed
//! serially, so every decision here sees a single consistent state.

use std::collections::{BTreeSet, VecDeque};

use interaction_contracts::desktop::device::{ReleaseStatus, RootGrant};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::ids::{AttemptId, ConfirmationId, DecisionId, RootTaskId};
use interaction_contracts::desktop::operator::{
    ActionSummary, ConfirmationKind, ConfirmationRequest, RootView,
};
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle};
use interaction_contracts::desktop::task::{
    Activation, AdmitAttempt, AdmitOutcome, AdmitRejection, AllowedControlScope, AttemptState,
    FrozenTaskProfile, GoalSpec, RootBudgetSpec, RootEvent, RootState,
};
use rusqlite::Transaction;

use super::budget::{BudgetKind, BudgetState};
use super::goal::{
    CompileError, HostGoalCompileContext, PlanRejection, compile_goal_with_context,
    confirmation_for, validate_plan,
};
use super::state_machine::{TransitionError, transition};
use super::store::{AttemptRow, RootRow, TaskStore};
use crate::deliberation::accounting::PlanningRequestAttribution;
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
                "ui.select_by_label",
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

/// A queued request keeps its immutable scope and Host-resolved application
/// binding until the existing serial root slot becomes available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTask {
    pub text: String,
    pub allowed_scope: Option<AllowedControlScope>,
    pub allowed_scope_app: Option<String>,
    pub frozen_profile: Option<FrozenTaskProfile>,
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
    /// What the finished root had spent, so its view still tells the truth.
    finished_budget: Option<BudgetState>,
    queue: VecDeque<QueuedTask>,
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
            finished_budget: None,
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

    pub fn next_queued_task(&mut self) -> Option<QueuedTask> {
        self.queue.pop_front()
    }

    pub fn inject_admit_fault(&mut self, fault: Option<AdmitFault>) {
        self.fault = fault;
    }

    /// A view of the current root, or of the last one that finished.
    pub fn view(&self) -> Option<RootView> {
        let (row, budget) = match (&self.active, &self.finished) {
            (Some(active), _) => (&active.row, active.budget.as_ref()),
            (None, Some(row)) => (row, self.finished_budget.as_ref()),
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
            self.finished_budget = active.budget;
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
        self.create_root_from_request(
            storage,
            now_ms,
            QueuedTask {
                text: text.to_string(),
                allowed_scope: None,
                allowed_scope_app: None,
                frozen_profile: None,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn create_scoped_root(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        objective: &str,
        allowed_scope: AllowedControlScope,
        allowed_scope_app: String,
    ) -> Result<CreateOutcome, TaskError> {
        self.create_root_from_request(
            storage,
            now_ms,
            QueuedTask {
                text: objective.to_string(),
                allowed_scope: Some(allowed_scope),
                allowed_scope_app: Some(allowed_scope_app),
                frozen_profile: None,
            },
        )
    }

    pub(crate) fn create_root_from_request(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        request: QueuedTask,
    ) -> Result<CreateOutcome, TaskError> {
        let QueuedTask {
            text,
            allowed_scope,
            allowed_scope_app,
            frozen_profile,
        } = request;
        interaction_contracts::desktop::task::validate_user_text(&text)
            .map_err(TaskError::Invalid)?;
        match (&allowed_scope, &allowed_scope_app) {
            (Some(scope), Some(app)) => {
                scope.validate().map_err(TaskError::Invalid)?;
                if app.trim().is_empty() || app.len() > 256 || app.chars().any(char::is_control) {
                    return Err(TaskError::Invalid(
                        "invalid trusted application binding".into(),
                    ));
                }
            }
            (None, None) => {}
            _ => {
                return Err(TaskError::Invalid(
                    "incomplete allowed control scope binding".into(),
                ));
            }
        }
        if let Some(profile) = &frozen_profile {
            profile.validate().map_err(TaskError::Invalid)?;
            if allowed_scope.is_none() {
                return Err(TaskError::Invalid(
                    "frozen profile requires an immutable control scope".into(),
                ));
            }
        }
        if self.active.is_some() {
            self.queue.push_back(QueuedTask {
                text,
                allowed_scope,
                allowed_scope_app,
                frozen_profile,
            });
            return Ok(CreateOutcome::Queued);
        }
        let seq = TaskStore::next_seq(storage.read())?;
        let root_id = RootTaskId::new(format!("root-{seq}"))
            .map_err(|error| TaskError::Invalid(error.to_string()))?;
        let next = transition(RootState::Draft, RootEvent::SubmitGoal, None)?;
        let mut row = RootRow {
            root_id: root_id.clone(),
            seq,
            user_text: text,
            allowed_scope,
            allowed_scope_app,
            frozen_profile,
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
        execution_domain: ExecutionDomain,
        bundle: &PlanBundle,
    ) -> Result<ConfirmationRequest, DraftError> {
        self.accept_draft_with_profile(storage, now_ms, draft, execution_domain, None, bundle)
    }

    /// Accept a draft using a page profile supplied by the trusted Host, not
    /// by the planner's GoalDraft.
    pub fn accept_draft_with_profile(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        draft: &GoalDraft,
        execution_domain: ExecutionDomain,
        browser_page_profile: Option<interaction_contracts::desktop::browser::BrowserPageProfile>,
        bundle: &PlanBundle,
    ) -> Result<ConfirmationRequest, DraftError> {
        self.accept_draft_with_profile_attribution(
            storage,
            now_ms,
            draft,
            execution_domain,
            browser_page_profile,
            bundle,
            None,
        )
    }

    /// Accept a GoalCompile proposal while retaining its Host request identity
    /// for the plan installation that happens atomically with user confirmation.
    // Keep Host attribution separate from the planner's draft and bundle.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_draft_with_profile_attribution(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        draft: &GoalDraft,
        execution_domain: ExecutionDomain,
        browser_page_profile: Option<interaction_contracts::desktop::browser::BrowserPageProfile>,
        bundle: &PlanBundle,
        attribution: Option<&PlanningRequestAttribution>,
    ) -> Result<ConfirmationRequest, DraftError> {
        let root = self.active.as_ref().ok_or(TaskError::NoActiveRoot)?;
        transition(root.row.state, RootEvent::DraftPassed, None).map_err(TaskError::from)?;
        let compile_context = match (
            root.row.allowed_scope.as_ref(),
            root.row.allowed_scope_app.as_deref(),
        ) {
            (None, None) => {
                HostGoalCompileContext::free_text(execution_domain, browser_page_profile)
            }
            (Some(scope), Some(app)) => {
                HostGoalCompileContext::scoped(execution_domain, browser_page_profile, scope, app)
            }
            _ => {
                return Err(TaskError::Invalid(
                    "root has an incomplete allowed control scope binding".into(),
                )
                .into());
            }
        };
        let (goal, pack) = compile_goal_with_context(
            &root.row.root_id,
            &root.row.user_text,
            draft,
            compile_context,
            self.config.default_budget,
            &self.config.packs,
        )?;
        validate_plan(&goal, bundle, &pack, &self.config.registered_skills)
            .map_err(CompileError::Plan)?;
        let request = confirmation_for(&goal);
        let root_id = root.row.root_id.clone();
        self.step(
            storage,
            now_ms,
            RootEvent::DraftPassed,
            |row| {
                row.draft = Some(goal.clone());
                row.plan = Some(bundle.clone());
            },
            |tx| TaskStore::insert_plan_installation(tx, &root_id, 1, None, attribution),
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
            |tx| {
                TaskStore::init_budget(tx, root_id.as_str())?;
                TaskStore::install_proposed_plan(tx, &root_id, 1, now_ms)
            },
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
        self.install_plan_with_attribution(storage, now_ms, bundle, None)
    }

    /// Install an accepted escalation plan with the exact S2 request identity.
    /// The ledger write shares the root revision transaction; any mismatch
    /// rolls back both writes.
    pub fn install_plan_from_request(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        bundle: &PlanBundle,
        attribution: &PlanningRequestAttribution,
    ) -> Result<RootState, PlanInstallError> {
        self.install_plan_with_attribution(storage, now_ms, Some(bundle), Some(attribution))
    }

    fn install_plan_with_attribution(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        bundle: Option<&PlanBundle>,
        attribution: Option<&PlanningRequestAttribution>,
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
        let installs_new_plan = bundle.is_some();
        let bundle = bundle.cloned();
        let root_id = active.row.root_id.clone();
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
            |tx| {
                if installs_new_plan {
                    TaskStore::insert_plan_installation(
                        tx,
                        &root_id,
                        revision,
                        Some(now_ms),
                        attribution,
                    )?;
                }
                Ok(())
            },
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

    /// The device is gone, so no release can be confirmed. The root is
    /// suspended anyway with its cleanup unknown (design 14.6): nothing can
    /// run without a device, and the person decides about going on.
    pub fn suspend_unconfirmed(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<RootState, TaskError> {
        self.step(
            storage,
            now_ms,
            RootEvent::InputsReleased,
            |row| row.cleanup = ReleaseStatus::Unknown,
            |_| Ok(()),
        )
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
                self.finished_budget = None;
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
                            AttemptState::Active | AttemptState::Dispatched | AttemptState::Unknown
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

#[cfg(test)]
mod scoped_task_tests {
    use super::*;

    fn scope() -> AllowedControlScope {
        AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![interaction_contracts::desktop::task::AllowedControlTarget {
                control: "Background option".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        }
    }

    #[test]
    fn scoped_scope_and_trusted_app_binding_persist_and_survive_queueing() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![]));
        let allowed_scope = scope();
        let CreateOutcome::Started(root_id) = runtime
            .create_scoped_root(
                &mut storage,
                1,
                "Turn off the background option",
                allowed_scope.clone(),
                "browser.fixture".into(),
            )
            .unwrap()
        else {
            panic!("first scoped task should start")
        };

        let row = TaskStore::load_root(storage.read(), root_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(row.allowed_scope, Some(allowed_scope.clone()));
        assert_eq!(row.allowed_scope_app.as_deref(), Some("browser.fixture"));
        assert_eq!(row.user_text, "Turn off the background option");

        assert_eq!(
            runtime
                .create_scoped_root(
                    &mut storage,
                    2,
                    "Turn on the background option",
                    allowed_scope.clone(),
                    "browser.fixture".into(),
                )
                .unwrap(),
            CreateOutcome::Queued,
        );
        let queued = runtime
            .next_queued_task()
            .expect("queued scope is retained");
        assert_eq!(queued.text, "Turn on the background option");
        assert_eq!(queued.allowed_scope, Some(allowed_scope));
        assert_eq!(queued.allowed_scope_app.as_deref(), Some("browser.fixture"));
    }

    fn profile() -> FrozenTaskProfile {
        FrozenTaskProfile {
            catalog_id: "a".repeat(64),
            selector: "b".repeat(64),
            source_freeze_sha256: "c".repeat(64),
            source_request_sha256: "d".repeat(64),
            goals_sha256: "e".repeat(64),
            suite: "fixture".into(),
            task_id: "SET-02".into(),
            seed: 1,
        }
    }

    fn frozen_request() -> QueuedTask {
        QueuedTask {
            text: "Turn off the background option".into(),
            allowed_scope: Some(scope()),
            allowed_scope_app: Some("browser.fixture".into()),
            frozen_profile: Some(profile()),
        }
    }

    #[test]
    fn frozen_provenance_is_immutable_and_survives_queue_and_recovery() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = runtime
            .create_root_from_request(&mut storage, 1, frozen_request())
            .unwrap()
        else {
            panic!("first root must start")
        };
        let mut row = TaskStore::load_root(storage.read(), root.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(row.frozen_profile, Some(profile()));
        row.frozen_profile.as_mut().unwrap().selector = "f".repeat(64);
        storage
            .transaction::<_, StoreError>(|tx| TaskStore::save_root(tx, &row))
            .unwrap();
        assert_eq!(
            TaskStore::load_root(storage.read(), root.as_str())
                .unwrap()
                .unwrap()
                .frozen_profile,
            Some(profile())
        );
        assert_eq!(
            runtime
                .create_root_from_request(&mut storage, 2, frozen_request())
                .unwrap(),
            CreateOutcome::Queued
        );
        assert_eq!(
            runtime.next_queued_task().unwrap().frozen_profile,
            Some(profile())
        );
        let mut restarted = TaskRuntime::new(TaskConfig::new(vec![]));
        restarted.recover(&mut storage, 3).unwrap();
        assert_eq!(
            TaskStore::load_root(storage.read(), root.as_str())
                .unwrap()
                .unwrap()
                .frozen_profile,
            Some(profile())
        );
    }

    #[test]
    fn invalid_or_unscoped_frozen_provenance_has_no_admission_side_effect() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![]));
        for invalid in [
            {
                let mut r = frozen_request();
                r.frozen_profile.as_mut().unwrap().seed = 6;
                r
            },
            {
                let mut r = frozen_request();
                r.allowed_scope = None;
                r.allowed_scope_app = None;
                r
            },
        ] {
            assert!(matches!(
                runtime.create_root_from_request(&mut storage, 1, invalid),
                Err(TaskError::Invalid(_))
            ));
            assert!(runtime.active().is_none());
            assert!(runtime.next_queued_task().is_none());
        }
        let roots: i64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM root_tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(roots, 0);
        storage.read().execute_batch("CREATE TRIGGER fail_admission BEFORE INSERT ON root_tasks BEGIN SELECT RAISE(ABORT, 'fail admission'); END;").unwrap();
        assert!(
            runtime
                .create_root_from_request(&mut storage, 2, frozen_request())
                .is_err()
        );
        assert!(runtime.active().is_none());
        assert_eq!(
            storage
                .read()
                .query_row("SELECT COUNT(*) FROM root_tasks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod plan_installation_ledger_tests {
    use super::*;
    use crate::deliberation::accounting::PlanningRequestAttribution;
    use crate::environment::pack::AppPack;
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::ids::RequestId;
    use interaction_contracts::desktop::planning::{GoalDraft, PlanningReason, Subgoal};
    use interaction_contracts::desktop::task::{ConfirmationScope, ExpectedValue, TargetSpec};

    fn attribution(
        root: &RootTaskId,
        request: &str,
        generation: u64,
    ) -> PlanningRequestAttribution {
        PlanningRequestAttribution {
            root: root.clone(),
            request: RequestId::new(request).unwrap(),
            generation,
            reason: PlanningReason::Escalation,
        }
    }

    #[test]
    fn a_completed_exact_s2_request_is_retained_through_goal_freeze() {
        let mut storage = Storage::open_in_memory().unwrap();
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![AppPack::browser_fixture()]));
        let allowed_scope = AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![interaction_contracts::desktop::task::AllowedControlTarget {
                control: profile.label().into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        let CreateOutcome::Started(root) = runtime
            .create_scoped_root(
                &mut storage,
                1,
                "turn off the background option",
                allowed_scope,
                profile.app().into(),
            )
            .unwrap()
        else {
            panic!("root should start")
        };
        let request = PlanningRequestAttribution {
            root: root.clone(),
            request: RequestId::new("plan-request").unwrap(),
            generation: 4,
            reason: PlanningReason::GoalCompile,
        };
        storage
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO model_calls(root_id, request_id, generation, provider, model,
                        started_ms, finished_ms, outcome, usage_json, reason)
                     VALUES (?1, ?2, ?3, 'open_ai', 'fixture', 10, 20, 'proposal',
                        '{\"input_tokens\":null,\"output_tokens\":null,\"cost_micros\":null}', 'goal_compile')",
                    rusqlite::params![root.as_str(), request.request.as_str(), request.generation as i64],
                )?;
                Ok(())
            })
            .unwrap();
        let target = TargetSpec {
            semantic_target: interaction_contracts::desktop::ids::SemanticTargetRef::new(
                profile.semantic_target(),
            )
            .unwrap(),
            label: profile.label().into(),
            group: None,
            role_hint: Some(profile.role().into()),
            row: None,
        };
        let draft = GoalDraft {
            app: profile.app().into(),
            account: None,
            target: target.clone(),
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
        };
        let bundle = PlanBundle {
            subgoals: vec![Subgoal {
                id: "ensure".into(),
                description: "ensure target value".into(),
                skill: "ui.ensure_value".into(),
                target: Some(target.semantic_target.clone()),
                expects: vec![],
                scroll: None,
                select: None,
            }],
            dependencies: vec![],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec![],
        };
        let confirmation = runtime
            .accept_draft_with_profile_attribution(
                &mut storage,
                21,
                &draft,
                ExecutionDomain::HeadlessBrowser,
                Some(profile),
                &bundle,
                Some(&request),
            )
            .unwrap();
        runtime
            .answer_confirmation(&mut storage, 30, &confirmation.confirmation_id, true)
            .unwrap();
        assert_eq!(
            TaskStore::load_root(storage.read(), root.as_str())
                .unwrap()
                .unwrap()
                .plan_revision,
            1,
        );
        let (state, attribution_status, request_id, generation, reason, installed_ms): (
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<i64>,
        ) = storage
            .read()
            .query_row(
                "SELECT state, attribution_status, request_id, generation, reason, installed_ms
                 FROM plan_installations WHERE root_id = ?1 AND plan_revision = 1",
                [root.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, "installed");
        assert_eq!(attribution_status, "model_request");
        assert_eq!(request_id.as_deref(), Some("plan-request"));
        assert_eq!(generation, Some(4));
        assert_eq!(reason.as_deref(), Some("goal_compile"));
        assert_eq!(installed_ms, Some(30));
    }

    #[test]
    fn a_request_with_the_same_root_and_id_but_wrong_generation_rolls_back_installation() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = runtime.create_root(&mut storage, 1, "inspect").unwrap()
        else {
            panic!("root should start")
        };
        storage
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO model_calls(root_id, request_id, generation, provider, model,
                        started_ms, finished_ms, outcome, usage_json, reason)
                     VALUES (?1, 'plan-request', 3, 'open_ai', 'fixture', 10, 20, 'proposal',
                        '{\"input_tokens\":null,\"output_tokens\":null,\"cost_micros\":null}', 'escalation')",
                    [root.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let mut changed_root = TaskStore::load_root(storage.read(), root.as_str())
            .unwrap()
            .unwrap();
        changed_root.plan_revision = 1;
        let wrong_generation = attribution(&root, "plan-request", 4);
        let result = storage.transaction::<_, StoreError>(|tx| {
            TaskStore::save_root(tx, &changed_root)?;
            TaskStore::insert_plan_installation(tx, &root, 1, Some(25), Some(&wrong_generation))
        });
        assert!(matches!(result, Err(StoreError::Conflict(_))));
        assert_eq!(
            TaskStore::load_root(storage.read(), root.as_str())
                .unwrap()
                .unwrap()
                .plan_revision,
            0,
            "a rejected S2 identity rolls back the plan revision update"
        );
        let installations: i64 = storage
            .read()
            .query_row(
                "SELECT COUNT(*) FROM plan_installations WHERE root_id = ?1",
                [root.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(installations, 0);
    }

    #[test]
    fn missing_model_accounting_remains_explicitly_unknown() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut runtime = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = runtime.create_root(&mut storage, 1, "inspect").unwrap()
        else {
            panic!("root should start")
        };
        let missing = attribution(&root, "unrecorded-request", 7);
        storage
            .transaction::<_, StoreError>(|tx| {
                TaskStore::insert_plan_installation(tx, &root, 1, Some(25), Some(&missing))
            })
            .unwrap();
        let (status, request_id, generation, reason): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
        ) = storage
            .read()
            .query_row(
                "SELECT attribution_status, request_id, generation, reason
                 FROM plan_installations WHERE root_id = ?1 AND plan_revision = 1",
                [root.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "unknown");
        assert_eq!((request_id, generation, reason), (None, None, None));
    }
}
