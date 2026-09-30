//! Root task lifecycle: compiling, confirming, freezing, stopping, pausing and
//! recovering (design 13.3 and 13.5).

mod common;

use common::*;
use interaction_contracts::desktop::device::ReleaseStatus;
use interaction_contracts::desktop::task::RootState;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::budget::BudgetKind;
use interaction_engine::tasks::goal::{CompileError, PlanRejection};
use interaction_engine::tasks::store::TaskStore;
use interaction_engine::tasks::{
    ConfirmOutcome, CreateOutcome, DraftError, PlanInstallError, StopDirective, TaskError,
    TaskRuntime,
};
use sim_world::policy::ScriptedPlanner;

fn fresh() -> (Storage, TaskRuntime) {
    (
        Storage::open_in_memory().unwrap(),
        TaskRuntime::new(config()),
    )
}

fn started(storage: &mut Storage, runtime: &mut TaskRuntime) {
    assert!(matches!(
        runtime.create_root(storage, 0, TEXT).unwrap(),
        CreateOutcome::Started(_)
    ));
}

#[test]
fn a_task_compiles_then_waits_for_the_user_and_only_then_freezes() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    assert_eq!(runtime.active_state(), Some(RootState::CompilingGoal));

    let (draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    let request = runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap();
    assert_eq!(
        runtime.active_state(),
        Some(RootState::AwaitingConfirmation)
    );
    assert!(
        runtime.active().unwrap().row.goal.is_none(),
        "nothing is frozen before the user agrees"
    );
    assert!(runtime.active().unwrap().row.draft.is_some());

    let ConfirmOutcome::GoalFrozen { goal, grant } = runtime
        .answer_confirmation(&mut storage, 20, &request.confirmation_id, true)
        .unwrap()
    else {
        panic!("confirming freezes the goal");
    };
    assert_eq!(runtime.active_state(), Some(RootState::Ready));
    assert_eq!(
        goal.budget.deadline_ms,
        20 + 120_000,
        "the window starts at confirmation"
    );
    assert_eq!(grant.deadline_ms, goal.budget.deadline_ms);
    assert_eq!((grant.revision, grant.root_epoch), (1, 1));

    let row = TaskStore::load_root(storage.read(), "root-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        row.goal.as_ref(),
        Some(&*goal),
        "the frozen goal is what was stored"
    );
    assert!(row.draft.is_none());
    assert_eq!(row.plan_revision, 1);

    // A frozen goal cannot be replaced by another draft.
    runtime.admission_passed(&mut storage, 30).unwrap();
    assert!(
        runtime
            .accept_draft(&mut storage, 40, &draft, &bundle)
            .is_err()
    );
}

#[test]
fn declining_the_goal_cancels_without_anything_reaching_the_device() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    let (draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    let request = runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap();
    let outcome = runtime
        .answer_confirmation(&mut storage, 20, &request.confirmation_id, false)
        .unwrap();
    assert_eq!(outcome, ConfirmOutcome::GoalDeclined);
    assert!(runtime.active().is_none());
    assert_eq!(runtime.last_finished().unwrap().state, RootState::Cancelled);
}

#[test]
fn an_unknown_confirmation_is_refused() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    let (draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap();
    let wrong = interaction_contracts::desktop::ids::ConfirmationId::new("conf-other").unwrap();
    assert!(matches!(
        runtime.answer_confirmation(&mut storage, 20, &wrong, true),
        Err(TaskError::UnknownConfirmation)
    ));
    assert_eq!(
        runtime.active_state(),
        Some(RootState::AwaitingConfirmation)
    );
}

#[test]
fn a_second_task_waits_until_the_first_one_ends() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    assert_eq!(
        runtime
            .create_root(&mut storage, 5, "another task")
            .unwrap(),
        CreateOutcome::Queued
    );
    assert_eq!(runtime.queue_len(), 1);
    runtime.fail_compile(&mut storage, 10, "give up").unwrap();
    assert!(runtime.active().is_none());
    assert_eq!(runtime.next_queued().as_deref(), Some("another task"));
    assert!(matches!(
        runtime.create_root(&mut storage, 20, "another task").unwrap(),
        CreateOutcome::Started(id) if id.as_str() == "root-2"
    ));
}

#[test]
fn blank_task_text_is_refused() {
    let (mut storage, mut runtime) = fresh();
    assert!(matches!(
        runtime.create_root(&mut storage, 0, "   "),
        Err(TaskError::Invalid(_))
    ));
}

#[test]
fn a_draft_for_an_app_without_a_pack_does_not_compile() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    let (mut draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    draft.app = "com.other.app".into();
    let error = runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap_err();
    assert!(
        matches!(
            error,
            DraftError::Compile(CompileError::AppNotExecutable(_))
        ),
        "{error:?}"
    );
    assert_eq!(
        runtime.active_state(),
        Some(RootState::CompilingGoal),
        "the root waits for the caller to fail it"
    );
    runtime
        .fail_compile(&mut storage, 20, "app not executable")
        .unwrap();
    let finished = runtime.last_finished().unwrap();
    assert_eq!(finished.state, RootState::Failed);
    assert_eq!(finished.failure.as_deref(), Some("app not executable"));
}

#[test]
fn a_plan_that_reaches_beyond_the_goal_is_not_installed() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    let overreaching = ScriptedPlanner { overreach: true };
    let (draft, bundle) = draft_and_bundle(TEXT, &overreaching);
    let error = runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap_err();
    assert!(
        matches!(
            error,
            DraftError::Compile(CompileError::Plan(PlanRejection::ExceedsGoal { .. }))
        ),
        "{error:?}"
    );
    assert!(
        runtime.active().unwrap().row.draft.is_none(),
        "nothing of the plan was kept"
    );
}

#[test]
fn a_late_draft_cannot_be_accepted_twice() {
    let (mut storage, mut runtime) = fresh();
    started(&mut storage, &mut runtime);
    let (draft, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    runtime
        .accept_draft(&mut storage, 10, &draft, &bundle)
        .unwrap();
    assert!(matches!(
        runtime.accept_draft(&mut storage, 20, &draft, &bundle),
        Err(DraftError::Task(TaskError::Transition(_)))
    ));
}

#[test]
fn stopping_a_frozen_root_waits_for_the_device_to_confirm_release() {
    let mut rig = running(None, config());
    assert_eq!(
        rig.runtime.stop_requested(&mut rig.storage, 100).unwrap(),
        StopDirective::AwaitRelease
    );
    assert_eq!(rig.runtime.active_state(), Some(RootState::Cancelling));
    assert_eq!(rig.runtime.active().unwrap().row.root_epoch, 2);

    // An unconfirmed release keeps the root in CANCELLING and is recorded.
    let state = rig
        .runtime
        .inputs_released(&mut rig.storage, 110, ReleaseStatus::Failed)
        .unwrap();
    assert_eq!(state, RootState::Cancelling);
    assert_eq!(
        rig.runtime.active().unwrap().row.cleanup,
        ReleaseStatus::Failed
    );

    let state = rig
        .runtime
        .inputs_released(&mut rig.storage, 120, ReleaseStatus::Released)
        .unwrap();
    assert_eq!(state, RootState::Cancelled);
    assert!(rig.runtime.active().is_none());
    let row = TaskStore::load_root(rig.storage.read(), "root-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.state, row.cleanup, row.cancel_requested),
        (RootState::Cancelled, ReleaseStatus::Released, true)
    );
}

#[test]
fn pausing_keeps_the_progress_and_only_an_explicit_resume_continues_it() {
    let mut rig = running(None, config());
    let root = rig.root.clone();
    let d = decision(&mut rig.storage, &root, "d-1", 1);
    rig.runtime
        .admit_attempt(
            &mut rig.storage,
            &interaction_engine::reaction::store::ReactionStore,
            &request(&root, &d, "a-1", 1),
            100,
            "{}",
        )
        .unwrap();

    rig.runtime.pause_requested(&mut rig.storage, 110).unwrap();
    assert_eq!(rig.runtime.active_state(), Some(RootState::Pausing));
    rig.runtime
        .inputs_released(&mut rig.storage, 120, ReleaseStatus::Released)
        .unwrap();
    assert_eq!(rig.runtime.active_state(), Some(RootState::Suspended));

    rig.runtime.resume_requested(&mut rig.storage, 130).unwrap();
    assert_eq!(
        rig.runtime.active_state(),
        Some(RootState::NeedsRevalidation)
    );
    rig.runtime
        .revalidation_passed(&mut rig.storage, 140)
        .unwrap();
    assert_eq!(rig.runtime.active_state(), Some(RootState::Running));

    let active = rig.runtime.active().unwrap();
    assert_eq!(
        active.row.root_epoch, 1,
        "a pause does not cancel, so the epoch is unchanged"
    );
    assert_eq!(
        active.budget.unwrap().attempts_used,
        1,
        "the budget is not reset by a pause"
    );
    assert_eq!(
        active.row.deadline_ms,
        Some(rig.goal.budget.deadline_ms),
        "nor is the deadline"
    );
}

#[test]
fn the_deadline_ends_the_root_and_stops_admission() {
    let mut rig = running(None, config());
    rig.runtime
        .deadline_expired(&mut rig.storage, 999_999)
        .unwrap();
    assert!(rig.runtime.active().is_none());
    assert_eq!(
        rig.runtime.last_finished().unwrap().state,
        RootState::Expired
    );
}

#[test]
fn completion_and_failure_are_final_and_free_the_slot() {
    let mut rig = running(None, config());
    rig.runtime
        .predicates_satisfied(&mut rig.storage, 100)
        .unwrap();
    assert_eq!(
        rig.runtime.last_finished().unwrap().state,
        RootState::Completed
    );
    assert!(rig.runtime.active().is_none());

    let mut rig = running(None, config());
    rig.runtime
        .unrecoverable(&mut rig.storage, 100, "budget exhausted")
        .unwrap();
    assert_eq!(
        rig.runtime.last_finished().unwrap().failure.as_deref(),
        Some("budget exhausted")
    );

    let mut rig = running(None, config());
    rig.runtime
        .input_ended_unverified(&mut rig.storage, 100, "could not confirm")
        .unwrap();
    assert_eq!(
        rig.runtime.last_finished().unwrap().state,
        RootState::Unverified
    );
}

#[test]
fn the_view_reports_progress_and_survives_the_end_of_the_root() {
    let mut rig = running(None, config());
    let view = rig.runtime.view().unwrap();
    assert_eq!(view.state, RootState::Running);
    assert_eq!(view.attempts_max, 12);
    rig.runtime
        .spend(&mut rig.storage, BudgetKind::Observation, 1)
        .unwrap();
    rig.runtime
        .predicates_satisfied(&mut rig.storage, 100)
        .unwrap();
    assert_eq!(rig.runtime.view().unwrap().state, RootState::Completed);
}

// ---- Recovery ---------------------------------------------------------------

#[test]
fn recovery_cancels_a_root_that_never_reached_the_device() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    {
        let mut storage = Storage::open(&path).unwrap();
        let mut runtime = TaskRuntime::new(config());
        started(&mut storage, &mut runtime);
    }
    let mut storage = Storage::open(&path).unwrap();
    let mut runtime = TaskRuntime::new(config());
    let report = runtime.recover(&mut storage, 100).unwrap();
    assert_eq!(report.cancelled.len(), 1);
    assert!(report.revalidating.is_empty());
    assert!(runtime.active().is_none());
    assert_eq!(runtime.last_finished().unwrap().state, RootState::Cancelled);
    // And it stays cancelled on the next start.
    let again = TaskRuntime::new(config())
        .recover(&mut storage, 200)
        .unwrap();
    assert!(again.cancelled.is_empty() && again.revalidating.is_empty());
}

#[test]
fn recovery_continues_a_cancellation_that_was_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    {
        let mut rig = running(Some(&path), config());
        rig.runtime.stop_requested(&mut rig.storage, 100).unwrap();
    }
    let mut storage = Storage::open(&path).unwrap();
    let mut runtime = TaskRuntime::new(config());
    let report = runtime.recover(&mut storage, 200).unwrap();
    assert_eq!(report.cancelling.len(), 1);
    assert_eq!(runtime.active_state(), Some(RootState::Cancelling));
    assert_eq!(
        runtime.active().unwrap().row.root_epoch,
        2,
        "the moved epoch survived"
    );
    runtime
        .inputs_released(&mut storage, 210, ReleaseStatus::Released)
        .unwrap();
    assert_eq!(runtime.last_finished().unwrap().state, RootState::Cancelled);
}

#[test]
fn recovery_never_restores_a_root_to_running_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    {
        let _rig = running(Some(&path), config());
    }
    let mut storage = Storage::open(&path).unwrap();
    let mut runtime = TaskRuntime::new(config());
    runtime.recover(&mut storage, 200).unwrap();
    assert_eq!(runtime.active_state(), Some(RootState::NeedsRevalidation));
    assert!(
        runtime.active().unwrap().row.goal.is_some(),
        "the frozen goal came back intact"
    );
    assert!(runtime.active().unwrap().pack.is_some());
}

// ---- Waiting for a plan -----------------------------------------------------

#[test]
fn a_replan_is_paid_for_when_asked_and_installs_under_a_new_revision() {
    let mut rig = running(None, config());
    let (_, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());

    assert!(rig.runtime.await_plan(&mut rig.storage, 100).unwrap());
    assert_eq!(rig.runtime.active_state(), Some(RootState::WaitingPlan));
    let counts = TaskStore::load_budget(rig.storage.read(), rig.root.as_str()).unwrap();
    assert_eq!(counts.replans, 1, "asking costs a replan, answer or not");

    let state = rig
        .runtime
        .install_plan(&mut rig.storage, 110, Some(&bundle))
        .unwrap();
    assert_eq!(state, RootState::Running);
    let row = TaskStore::load_root(rig.storage.read(), rig.root.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(row.plan_revision, 2);
    assert_eq!(row.plan.as_ref(), Some(&bundle));

    // A decision made under the old plan can no longer start anything.
    let old = decision(&mut rig.storage, &rig.root, "d-old", 1);
    let outcome = rig
        .runtime
        .admit_attempt(
            &mut rig.storage,
            &interaction_engine::reaction::store::ReactionStore,
            &request(&rig.root, &old, "a-old", 1),
            120,
            "{}",
        )
        .unwrap();
    assert_eq!(
        outcome,
        interaction_contracts::desktop::task::AdmitOutcome::Rejected {
            reason: interaction_contracts::desktop::task::AdmitRejection::VersionMismatch
        }
    );
}

#[test]
fn a_plan_beyond_the_goal_is_refused_and_the_root_keeps_waiting() {
    let mut rig = running(None, config());
    let (_, overreaching) = draft_and_bundle(TEXT, &ScriptedPlanner { overreach: true });
    rig.runtime.await_plan(&mut rig.storage, 100).unwrap();
    let refused = rig
        .runtime
        .install_plan(&mut rig.storage, 110, Some(&overreaching));
    assert!(
        matches!(
            refused,
            Err(PlanInstallError::Rejected(
                PlanRejection::ExceedsGoal { .. }
            ))
        ),
        "{refused:?}"
    );
    assert_eq!(rig.runtime.active_state(), Some(RootState::WaitingPlan));
    let row = TaskStore::load_root(rig.storage.read(), rig.root.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(row.plan_revision, 1, "nothing was installed");
}

#[test]
fn a_review_that_changes_nothing_resumes_the_root_on_the_same_plan() {
    let mut rig = running(None, config());
    rig.runtime.await_plan(&mut rig.storage, 100).unwrap();
    rig.runtime
        .install_plan(&mut rig.storage, 110, None)
        .unwrap();
    assert_eq!(rig.runtime.active_state(), Some(RootState::Running));
    let row = TaskStore::load_root(rig.storage.read(), rig.root.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(row.plan_revision, 1);
}

#[test]
fn replans_run_out_and_a_plan_cannot_be_installed_unasked() {
    let mut rig = running(None, config());
    let (_, bundle) = draft_and_bundle(TEXT, &ScriptedPlanner::default());
    assert!(
        rig.runtime
            .install_plan(&mut rig.storage, 90, Some(&bundle))
            .is_err(),
        "nothing is waiting for a plan"
    );
    for round in 0..2 {
        assert!(
            rig.runtime
                .await_plan(&mut rig.storage, 100 + round)
                .unwrap()
        );
        rig.runtime
            .install_plan(&mut rig.storage, 110 + round, None)
            .unwrap();
    }
    assert!(
        !rig.runtime.await_plan(&mut rig.storage, 200).unwrap(),
        "the third replan is refused"
    );
    assert_eq!(rig.runtime.active_state(), Some(RootState::Running));
}
