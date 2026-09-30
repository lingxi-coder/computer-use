//! Shared helpers for the crate's unit tests: a goal, a simulated app and the
//! evidence plumbing around it.

use interaction_contracts::desktop::binding::{BindRequest, BindScope, TargetDescription};
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{RequestId, RootTaskId, SemanticTargetRef};
use interaction_contracts::desktop::planning::{PlanBundle, PlannerOutput};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, GoalSpec, Predicate, RootBudgetSpec, TargetSpec, TaskScope,
};
use sim_world::app::{SimApp, SnapshotMeta};
use sim_world::policy::ScriptedPlanner;

use crate::environment::binding::{Binding, BindingReducer};
use crate::environment::pack::AppPack;
use crate::environment::state_core::{EnvSnapshot, StateCore};

pub const TARGET: &str = "push/daily-picks";

pub fn meta(sequence: u64, observed_ms: u64) -> SnapshotMeta {
    SnapshotMeta {
        snapshot_id: format!("snap-{sequence}"),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: sequence,
        observed_ms,
    }
}

pub fn goal(scope: ConfirmationScope, max: EffectClass) -> GoalSpec {
    let target = SemanticTargetRef::new(TARGET).unwrap();
    let mut checks = vec![
        Predicate::ContextMatches {
            app: "test.settings".into(),
        },
        Predicate::ReadValueEquals {
            target: target.clone(),
            expected: ExpectedValue::Bool { value: false },
            observed_after_action: true,
        },
    ];
    checks.push(match scope {
        ConfirmationScope::UiRevisit => Predicate::Revisited {
            target: target.clone(),
        },
        ConfirmationScope::Commit => Predicate::CommitConfirmed {
            target: target.clone(),
        },
    });
    GoalSpec {
        root_task_id: RootTaskId::new("root-1").unwrap(),
        task_revision: 1,
        user_text: "turn off the daily picks in Push".into(),
        scope: TaskScope {
            app: "test.settings".into(),
            account: None,
        },
        target: TargetSpec {
            semantic_target: target,
            label: "Daily picks".into(),
            group: Some("Push".into()),
            role_hint: Some("switch".into()),
        },
        desired: ExpectedValue::Bool { value: false },
        confirmation_scope: scope,
        max_effect_class: max,
        required: Predicate::And { of: checks },
        budget: RootBudgetSpec {
            deadline_ms: 60_000,
            max_attempts: 8,
            max_observations: 16,
            max_recoveries: 3,
            max_replans: 2,
        },
    }
}

pub fn bundle(text: &str) -> PlanBundle {
    match ScriptedPlanner::default().plan_text(text) {
        PlannerOutput::Plan { proposal } => proposal.bundle,
        other => panic!("expected a plan: {other:?}"),
    }
}

/// Evidence plumbing around one simulated app.
pub struct EnvRig {
    pub state: StateCore,
    pub bindings: BindingReducer,
    pub pack: AppPack,
    pub app: SimApp,
    pub sequence: u64,
    pub now_ms: u64,
}

impl EnvRig {
    pub fn new() -> Self {
        let mut state = StateCore::new();
        state.set_session(1);
        Self {
            state,
            bindings: BindingReducer::new(),
            pack: AppPack::settings_fixture(),
            app: SimApp::new(),
            sequence: 0,
            now_ms: 0,
        }
    }

    /// Observe the app as it is, `step_ms` later than the last observation.
    pub fn observe(&mut self, step_ms: u64) -> EnvSnapshot {
        self.now_ms += step_ms;
        self.sequence += 1;
        let snapshot = self.app.snapshot(&meta(self.sequence, self.now_ms));
        self.state.admit(snapshot, self.now_ms, &self.pack).unwrap();
        let env = self.state.latest().unwrap().clone();
        self.bindings.revalidate(&env);
        env
    }

    pub fn bind_goal_target(&mut self) -> Binding {
        let env = self.state.latest().unwrap().clone();
        let request = BindRequest {
            request_id: RequestId::new("bind").unwrap(),
            semantic_target: SemanticTargetRef::new(TARGET).unwrap(),
            description: TargetDescription {
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
            },
            scope: BindScope {
                app: "test.settings".into(),
                account: None,
            },
            choice: None,
            max_age_ms: u64::MAX,
        };
        self.bindings.bind(&env, &request, self.now_ms);
        self.bindings
            .current(&SemanticTargetRef::new(TARGET).unwrap())
            .expect("the target binds")
            .clone()
    }
}
