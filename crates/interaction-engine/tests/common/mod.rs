//! Helpers shared by the engine's integration tests.
#![allow(dead_code)]

pub mod world;

use std::path::Path;

use interaction_contracts::desktop::decision::DecisionSource;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{AttemptId, DecisionId, RootTaskId};
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle, PlannerOutput};
use interaction_contracts::desktop::task::{AdmitAttempt, AttemptQuota, GoalSpec, RootBudgetSpec};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::reaction::store::ReactionStore;
use interaction_engine::tasks::{ConfirmOutcome, CreateOutcome, TaskConfig, TaskRuntime};
use sim_world::policy::ScriptedPlanner;

pub const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;

pub fn config() -> TaskConfig {
    TaskConfig::new(vec![AppPack::settings_fixture()])
}

pub fn config_with_budget(budget: RootBudgetSpec) -> TaskConfig {
    let mut config = config();
    config.default_budget = budget;
    config
}

pub fn draft_and_bundle(text: &str, planner: &ScriptedPlanner) -> (GoalDraft, PlanBundle) {
    match planner.plan_text(text) {
        PlannerOutput::Plan { proposal } => (
            proposal.goal_draft.expect("the planner drafted a goal"),
            proposal.bundle,
        ),
        other => panic!("expected a plan: {other:?}"),
    }
}

pub struct Rig {
    pub storage: Storage,
    pub runtime: TaskRuntime,
    pub root: RootTaskId,
    pub goal: GoalSpec,
}

/// A root that is confirmed, frozen and RUNNING, on a database at `path`.
pub fn running(path: Option<&Path>, config: TaskConfig) -> Rig {
    let mut storage = match path {
        Some(path) => Storage::open(path).unwrap(),
        None => Storage::open_in_memory().unwrap(),
    };
    let mut runtime = TaskRuntime::new(config);
    let CreateOutcome::Started(root) = runtime.create_root(&mut storage, 0, TEXT).unwrap() else {
        panic!("the first root starts at once");
    };
    let (draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    let request = runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap();
    let ConfirmOutcome::GoalFrozen { goal, .. } = runtime
        .answer_confirmation(&mut storage, 20, &request.confirmation_id, true)
        .unwrap()
    else {
        panic!("the goal freezes when confirmed");
    };
    runtime.admission_passed(&mut storage, 30).unwrap();
    Rig {
        storage,
        runtime,
        root,
        goal: *goal,
    }
}

pub fn decision(storage: &mut Storage, root: &RootTaskId, id: &str, generation: u64) -> DecisionId {
    let decision = DecisionId::new(id).unwrap();
    storage
        .transaction(|tx| ReactionStore::open_decision(tx, &decision, root, generation, 40))
        .unwrap();
    decision
}

pub fn request(
    root: &RootTaskId,
    decision: &DecisionId,
    attempt: &str,
    generation: u64,
) -> AdmitAttempt {
    AdmitAttempt {
        attempt_id: AttemptId::new(attempt).unwrap(),
        decision_id: decision.clone(),
        generation,
        root_task_id: root.clone(),
        root_epoch: 1,
        task_revision: 1,
        plan_revision: 1,
        subgoal: "ensure".into(),
        skill: "ui.ensure_value".into(),
        quota: AttemptQuota {
            actions: 1,
            observations: 2,
        },
        deadline_ms: 10_000,
        effect_class: EffectClass::ReversibleEdit,
        source: DecisionSource::Rule,
    }
}

pub fn count(storage: &Storage, table: &str) -> i64 {
    storage
        .read()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
