//! Attempt admission (design 11.2): one transaction, idempotent, never replayed.
//! Covers TSK-02, TSK-03 and TSK-04 of the clause index.

mod common;

use common::*;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::operator::ActionSummary;
use interaction_contracts::desktop::task::{
    AdmitOutcome, AdmitRejection, AttemptState, RootBudgetSpec, RootState,
};
use interaction_engine::reaction::store::ReactionStore;
use interaction_engine::tasks::budget::BudgetKind;
use interaction_engine::tasks::store::TaskStore;
use interaction_engine::tasks::{AdmitFault, TaskError};

fn admit(
    rig: &mut Rig,
    request: &interaction_contracts::desktop::task::AdmitAttempt,
    now: u64,
) -> Result<AdmitOutcome, TaskError> {
    rig.runtime
        .admit_attempt(&mut rig.storage, &ReactionStore, request, now, "{}")
}

fn rejected(outcome: Result<AdmitOutcome, TaskError>) -> AdmitRejection {
    match outcome.expect("a rejection is an outcome, not an error") {
        AdmitOutcome::Rejected { reason } => reason,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// Nothing about an admission exists: no attempt, no spend, no mapping, no outbox.
fn assert_no_trace(rig: &Rig) {
    assert_eq!(count(&rig.storage, "attempts"), 0, "attempts");
    assert_eq!(count(&rig.storage, "attempt_map"), 0, "attempt_map");
    assert_eq!(count(&rig.storage, "admission_outbox"), 0, "outbox");
    let spent = TaskStore::load_budget(rig.storage.read(), rig.root.as_str()).unwrap();
    assert_eq!(spent.attempts, 0, "budget");
    assert_eq!(
        rig.runtime.active().unwrap().budget.unwrap().attempts_used,
        0,
        "in-memory budget"
    );
}

#[test]
fn a_valid_admission_activates_and_records_everything_together() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    let outcome = admit(&mut rig, &request(&root, &d, "a-1", 1), 100).unwrap();
    let AdmitOutcome::Activated { activation } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(activation.activation_seq, 1);
    assert_eq!(activation.granted.actions, 1);

    let attempt = TaskStore::load_attempt(rig.storage.read(), "a-1")
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, AttemptState::Active);
    let (generation, mapped) = ReactionStore::mapping(rig.storage.read(), "d-1")
        .unwrap()
        .unwrap();
    assert_eq!((generation, mapped.as_str()), (1, "a-1"));
    assert_eq!(count(&rig.storage, "admission_outbox"), 1);
    let spent = TaskStore::load_budget(rig.storage.read(), rig.root.as_str()).unwrap();
    assert_eq!(spent.attempts, 1);
    assert_eq!(
        rig.runtime.active().unwrap().budget.unwrap().attempts_used,
        1
    );
}

#[test]
fn a_repeated_request_returns_the_same_activation_and_spends_nothing_more() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    let req = request(&root, &d, "a-1", 1);
    let first = admit(&mut rig, &req, 100).unwrap();
    let second = admit(&mut rig, &req, 500).unwrap();
    assert_eq!(first, second);
    assert_eq!(count(&rig.storage, "attempts"), 1);
    assert_eq!(count(&rig.storage, "attempt_map"), 1);
    assert_eq!(
        TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
            .unwrap()
            .attempts,
        1
    );
}

#[test]
fn an_attempt_id_reused_for_another_decision_is_a_conflict() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let first = decision(&mut rig.storage, &root, "d-1", 1);
    let second = decision(&mut rig.storage, &root, "d-2", 1);
    admit(&mut rig, &request(&root, &first, "a-1", 1), 100).unwrap();
    let conflict = admit(&mut rig, &request(&root, &second, "a-1", 1), 110);
    assert_eq!(rejected(conflict), AdmitRejection::AttemptIdConflict);
    assert_eq!(count(&rig.storage, "attempts"), 1);
    assert_eq!(
        ReactionStore::load(rig.storage.read(), "d-2")
            .unwrap()
            .unwrap()
            .state,
        interaction_engine::ports::DecisionLifecycle::Pending,
        "the refused decision is still usable"
    );
}

#[test]
fn every_rejection_leaves_no_trace() {
    // Stale root epoch.
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    let mut stale = request(&root, &d, "a-1", 1);
    stale.root_epoch = 2;
    assert_eq!(
        rejected(admit(&mut rig, &stale, 100)),
        AdmitRejection::RootEpochStale
    );
    assert_no_trace(&rig);

    // Wrong task or plan revision.
    let mut wrong = request(&root, &d, "a-1", 1);
    wrong.plan_revision = 9;
    assert_eq!(
        rejected(admit(&mut rig, &wrong, 100)),
        AdmitRejection::VersionMismatch
    );
    let mut wrong = request(&root, &d, "a-1", 1);
    wrong.task_revision = 9;
    assert_eq!(
        rejected(admit(&mut rig, &wrong, 100)),
        AdmitRejection::VersionMismatch
    );
    assert_no_trace(&rig);

    // Past the root deadline, or an attempt that would outlive it.
    let root_deadline = rig.goal.budget.deadline_ms;
    assert_eq!(
        rejected(admit(
            &mut rig,
            &request(&root, &d, "a-1", 1),
            root_deadline
        )),
        AdmitRejection::DeadlineExpired
    );
    let mut long = request(&root, &d, "a-1", 1);
    long.deadline_ms = root_deadline + 1;
    assert_eq!(
        rejected(admit(&mut rig, &long, 100)),
        AdmitRejection::DeadlineExpired
    );
    assert_no_trace(&rig);

    // Effects the goal does not cover, never-allowed classes.
    for class in [
        EffectClass::Commit,
        EffectClass::Destructive,
        EffectClass::External,
        EffectClass::Unknown,
    ] {
        let mut effect = request(&root, &d, "a-1", 1);
        effect.effect_class = class;
        assert_eq!(
            rejected(admit(&mut rig, &effect, 100)),
            AdmitRejection::EffectNotAuthorized,
            "{class:?}"
        );
    }
    assert_no_trace(&rig);

    // A decision that is not pending leaves the task-side writes rolled back too.
    let closed = decision(&mut rig.storage, &root, "d-closed", 1);
    rig.storage
        .transaction(|tx| {
            ReactionStore::close(
                tx,
                &closed,
                interaction_engine::ports::DecisionLifecycle::Expired,
            )
        })
        .unwrap();
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &closed, "a-2", 1), 100)),
        AdmitRejection::DecisionNotPending
    );
    let ghost = interaction_contracts::desktop::ids::DecisionId::new("ghost").unwrap();
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &ghost, "a-3", 1), 100)),
        AdmitRejection::DecisionNotPending
    );
    assert_no_trace(&rig);
}

#[test]
fn an_exhausted_budget_refuses_the_next_attempt() {
    let budget = RootBudgetSpec {
        deadline_ms: 60_000,
        max_attempts: 1,
        max_observations: 4,
        max_recoveries: 1,
        max_replans: 1,
    };
    let mut rig = running(None, config_with_budget(budget));
    let root = rig.root.clone();
    let first = decision(&mut rig.storage, &root, "d-1", 1);
    let second = decision(&mut rig.storage, &root, "d-2", 1);
    admit(&mut rig, &request(&root, &first, "a-1", 1), 100).unwrap();
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &second, "a-2", 1), 110)),
        AdmitRejection::BudgetExhausted
    );
    assert_eq!(count(&rig.storage, "attempts"), 1);
}

#[test]
fn a_crash_at_each_point_of_the_transaction_leaves_nothing_behind_and_a_retry_works() {
    for fault in [
        AdmitFault::AfterTaskWrites,
        AdmitFault::AfterReactionWrites,
        AdmitFault::BeforeCommit,
    ] {
        let mut rig = running(None, config());
        let root = rig.root.clone();
        let d = decision(&mut rig.storage, &root, "d-1", 1);
        let req = request(&root, &d, "a-1", 1);

        rig.runtime.inject_admit_fault(Some(fault));
        assert!(
            matches!(admit(&mut rig, &req, 100), Err(TaskError::Store(_))),
            "{fault:?}"
        );
        assert_no_trace(&rig);
        assert_eq!(
            ReactionStore::load(rig.storage.read(), "d-1")
                .unwrap()
                .unwrap()
                .state,
            interaction_engine::ports::DecisionLifecycle::Pending,
            "{fault:?}: the decision is still pending"
        );

        rig.runtime.inject_admit_fault(None);
        assert!(
            matches!(
                admit(&mut rig, &req, 110),
                Ok(AdmitOutcome::Activated { .. })
            ),
            "{fault:?}"
        );
        assert_eq!(
            count(&rig.storage, "attempts"),
            1,
            "{fault:?}: exactly one attempt"
        );
        assert_eq!(
            TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
                .unwrap()
                .attempts,
            1
        );
    }
}

#[test]
fn an_old_generation_cannot_start_a_skill_after_its_replacement() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    rig.storage
        .transaction(|tx| ReactionStore::supersede(tx, &d, 2))
        .unwrap();
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &d, "a-1", 1), 100)),
        AdmitRejection::DecisionNotPending
    );
    assert_no_trace(&rig);
    assert!(matches!(
        admit(&mut rig, &request(&root, &d, "a-1", 2), 110),
        Ok(AdmitOutcome::Activated { .. })
    ));
    // The answer to the old generation arrives late and is refused, and a
    // fresh attempt id does not get around the consumed decision.
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &d, "a-2", 1), 120)),
        AdmitRejection::DecisionAlreadyConsumed
    );
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &d, "a-3", 2), 130)),
        AdmitRejection::DecisionAlreadyConsumed
    );
    assert_eq!(count(&rig.storage, "attempt_map"), 1);
}

#[test]
fn a_restart_never_replays_an_admitted_attempt_and_the_budget_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    let root;
    let req;
    {
        let mut rig = running(Some(&path), config());
        root = rig.root.clone();
        let d = decision(&mut rig.storage, &root, "d-1", 1);
        req = request(&root, &d, "a-1", 1);
        admit(&mut rig, &req, 100).unwrap();
        // The attempt was handed to the device, then the host died.
        rig.runtime
            .attempt_dispatched(&mut rig.storage, &req.attempt_id, "intent-a-1")
            .unwrap();
    }

    let mut storage = interaction_engine::infrastructure::storage::Storage::open(&path).unwrap();
    let mut runtime = interaction_engine::tasks::TaskRuntime::new(config());
    let report = runtime.recover(&mut storage, 200).unwrap();
    assert_eq!(report.revalidating, vec![root.clone()]);
    assert_eq!(report.unknown_attempts.len(), 1);
    assert_eq!(report.unknown_attempts[0].attempt_id.as_str(), "a-1");
    assert_eq!(runtime.active_state(), Some(RootState::NeedsRevalidation));
    let recovered = runtime.active().unwrap();
    assert_eq!(
        recovered.budget.unwrap().attempts_used,
        1,
        "the spend survived the restart"
    );
    assert_eq!(
        TaskStore::load_attempt(storage.read(), "a-1")
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Unknown,
        "an in-flight attempt is unknown, not retried"
    );

    // A repeat of the admitted request is never read as permission to start
    // again: its outcome is unknown, so the decision counts as consumed.
    let again = runtime
        .admit_attempt(&mut storage, &ReactionStore, &req, 210, "{}")
        .unwrap();
    assert_eq!(
        again,
        AdmitOutcome::Rejected {
            reason: AdmitRejection::DecisionAlreadyConsumed
        }
    );
    runtime.revalidation_passed(&mut storage, 220).unwrap();
    let replay = runtime
        .admit_attempt(&mut storage, &ReactionStore, &req, 230, "{}")
        .unwrap();
    assert_eq!(
        replay,
        AdmitOutcome::Rejected {
            reason: AdmitRejection::DecisionAlreadyConsumed
        }
    );
    assert_eq!(count(&storage, "attempts"), 1);
    assert_eq!(
        TaskStore::load_budget(storage.read(), root.as_str())
            .unwrap()
            .attempts,
        1
    );
    assert_eq!(ReactionStore::count_mappings(storage.read()).unwrap(), 1);
}

#[test]
fn stopping_or_pausing_closes_admission_at_once() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    rig.runtime.pause_requested(&mut rig.storage, 100).unwrap();
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &d, "a-1", 1), 110)),
        AdmitRejection::RootNotRunning
    );

    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    rig.runtime.stop_requested(&mut rig.storage, 100).unwrap();
    assert_eq!(
        rig.runtime.active().unwrap().row.root_epoch,
        2,
        "the cancellation epoch moved on"
    );
    assert_eq!(
        rejected(admit(&mut rig, &request(&root, &d, "a-1", 1), 110)),
        AdmitRejection::RootNotRunning
    );
    assert_no_trace(&rig);
}

#[test]
fn an_attempt_that_never_produced_input_gets_its_quota_back_exactly_once() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    let req = request(&root, &d, "a-1", 1);
    admit(&mut rig, &req, 100).unwrap();
    rig.runtime
        .attempt_ended(
            &mut rig.storage,
            &req.attempt_id,
            AttemptState::Settled,
            true,
        )
        .unwrap();
    rig.runtime
        .attempt_ended(
            &mut rig.storage,
            &req.attempt_id,
            AttemptState::Settled,
            true,
        )
        .unwrap();
    assert_eq!(
        TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
            .unwrap()
            .attempts,
        0
    );
    assert_eq!(
        rig.runtime.active().unwrap().budget.unwrap().attempts_used,
        0
    );
}

#[test]
fn an_attempt_with_an_unknown_outcome_keeps_its_quota() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    let req = request(&root, &d, "a-1", 1);
    admit(&mut rig, &req, 100).unwrap();
    rig.runtime
        .attempt_ended(
            &mut rig.storage,
            &req.attempt_id,
            AttemptState::Unknown,
            false,
        )
        .unwrap();
    assert_eq!(
        TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
            .unwrap()
            .attempts,
        1
    );
    // A later, definite answer can still settle it.
    rig.runtime
        .attempt_ended(
            &mut rig.storage,
            &req.attempt_id,
            AttemptState::Settled,
            false,
        )
        .unwrap();
    assert_eq!(
        TaskStore::load_attempt(rig.storage.read(), "a-1")
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Settled
    );
    assert_eq!(
        TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
            .unwrap()
            .attempts,
        1
    );
}

#[test]
fn a_commit_outside_the_goal_waits_for_the_users_approval_of_that_one_action() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-apply", 1);
    let mut apply = request(&root, &d, "a-apply", 1);
    apply.effect_class = EffectClass::Commit;
    assert_eq!(
        rejected(admit(&mut rig, &apply, 100)),
        AdmitRejection::EffectNotAuthorized
    );

    let ask = rig
        .runtime
        .request_effect_confirmation(
            &mut rig.storage,
            110,
            d.clone(),
            ActionSummary {
                skill: "ui.activate".into(),
                target_label: "Apply".into(),
                description: "press Apply".into(),
                effect_class: EffectClass::Commit,
            },
        )
        .unwrap();
    assert_eq!(
        rig.runtime.active_state(),
        Some(RootState::NeedsInput),
        "input is released while asking"
    );
    assert_eq!(
        rejected(admit(&mut rig, &apply, 120)),
        AdmitRejection::RootNotRunning
    );

    let outcome = rig
        .runtime
        .answer_confirmation(&mut rig.storage, 130, &ask.confirmation_id, true)
        .unwrap();
    assert!(matches!(
        outcome,
        interaction_engine::tasks::ConfirmOutcome::EffectApproved { .. }
    ));
    assert_eq!(rig.runtime.active_state(), Some(RootState::Running));
    assert!(matches!(
        admit(&mut rig, &apply, 140),
        Ok(AdmitOutcome::Activated { .. })
    ));

    // The approval covered that decision only.
    let other = decision(&mut rig.storage, &root, "d-apply-2", 1);
    let mut again = request(&root, &other, "a-apply-2", 1);
    again.effect_class = EffectClass::Commit;
    assert_eq!(
        rejected(admit(&mut rig, &again, 150)),
        AdmitRejection::EffectNotAuthorized
    );
}

#[test]
fn an_approval_never_makes_external_or_unknown_effects_executable() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    for (index, class) in [EffectClass::External, EffectClass::Unknown]
        .into_iter()
        .enumerate()
    {
        let d = decision(&mut rig.storage, &root, &format!("d-{index}"), 1);
        let ask = rig
            .runtime
            .request_effect_confirmation(
                &mut rig.storage,
                110,
                d.clone(),
                ActionSummary {
                    skill: "x".into(),
                    target_label: "x".into(),
                    description: "x".into(),
                    effect_class: class,
                },
            )
            .unwrap();
        rig.runtime
            .answer_confirmation(&mut rig.storage, 120, &ask.confirmation_id, true)
            .unwrap();
        let mut req = request(&root, &d, &format!("a-{index}"), 1);
        req.effect_class = class;
        assert_eq!(
            rejected(admit(&mut rig, &req, 130)),
            AdmitRejection::EffectNotAuthorized,
            "{class:?}"
        );
    }
}

#[test]
fn an_observation_budget_is_spent_and_persisted() {
    let budget = RootBudgetSpec {
        deadline_ms: 60_000,
        max_attempts: 4,
        max_observations: 2,
        max_recoveries: 1,
        max_replans: 1,
    };
    let mut rig = running(None, config_with_budget(budget));
    assert!(
        rig.runtime
            .spend(&mut rig.storage, BudgetKind::Observation, 1)
            .unwrap()
    );
    assert!(
        rig.runtime
            .spend(&mut rig.storage, BudgetKind::Observation, 1)
            .unwrap()
    );
    assert!(
        !rig.runtime
            .spend(&mut rig.storage, BudgetKind::Observation, 1)
            .unwrap(),
        "the third is refused"
    );
    assert_eq!(
        TaskStore::load_budget(rig.storage.read(), rig.root.as_str())
            .unwrap()
            .observations,
        2
    );
}
