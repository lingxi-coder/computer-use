//! Deliberation owns model-call accounting. Raw provider frames, prompts,
//! credentials and proposals are deliberately absent from this record.

use interaction_contracts::desktop::decision::DecisionRequest;
use interaction_contracts::desktop::ids::{DecisionId, RequestId, RootTaskId};
use interaction_contracts::desktop::planning::{PlanningReason, PlanningRequest};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::infrastructure::storage::{Storage, StoreError, enum_to_str};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOutcome {
    Proposal,
    Unavailable,
    Missing,
    Conflict,
    Invalid,
    Quarantined,
    Cancelled,
    Expired,
}

pub struct ModelCompletion {
    pub root: RootTaskId,
    pub request: RequestId,
    pub generation: u64,
    pub at_ms: u64,
    pub outcome: ModelOutcome,
    pub usage: ModelUsage,
}

/// The Host-owned identity of a planner answer considered for plan installation.
/// Storage verifies this key and reason against a completed S2 accounting row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningRequestAttribution {
    pub root: RootTaskId,
    pub request: RequestId,
    pub generation: u64,
    pub reason: PlanningReason,
}

/// The terminal state of one S1 decision call. This record is separate from
/// S2 planning so its identity and generation cannot affect planner sequencing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionCallOutcome {
    Proposal,
    Failed,
    Unavailable,
    Quarantined,
    Cancelled,
    Expired,
}

pub struct DecisionCallCompletion {
    pub root: RootTaskId,
    pub decision: DecisionId,
    pub generation: u64,
    pub at_ms: u64,
    pub outcome: DecisionCallOutcome,
    pub usage: ModelUsage,
}

/// Record a privately authorized S1 call before opening its SDK worker.
pub fn decision_call_started(
    storage: &mut Storage,
    request: &DecisionRequest,
    at_ms: u64,
    provider: &str,
    model: &str,
) -> Result<(), StoreError> {
    if provider.is_empty()
        || provider.trim().is_empty()
        || provider.len() > 512
        || provider.chars().any(char::is_control)
        || model.is_empty()
        || model.trim().is_empty()
        || model.len() > 256
        || model.chars().any(char::is_control)
        || request.validate().is_err()
        || request.request_generation == 0
        || request.root_task_id != request.state.root_task_id
        || request.task_revision != request.state.task_revision
        || at_ms < request.created_ms
        || at_ms >= request.result_deadline_ms
    {
        return Err(StoreError::Conflict(
            "invalid decision-call identity or deadline".into(),
        ));
    }
    storage.transaction(|tx| {
        tx.execute(
            "INSERT INTO decision_model_calls(root_id, decision_id, generation, provider, model, started_ms, usage_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                request.root_task_id.as_str(),
                request.decision_id.as_str(),
                request.request_generation,
                provider,
                model,
                at_ms,
                serde_json::to_string(&ModelUsage::default())?
            ],
        )?;
        Ok(())
    })
}

/// Finalize exactly the open row for the original decision scope. A late or
/// duplicate completion cannot overwrite a newer generation or terminal row.
pub fn decision_call_finished(
    storage: &mut Storage,
    completion: &DecisionCallCompletion,
) -> Result<(), StoreError> {
    storage.transaction(|tx| {
        let changed = tx.execute(
            "UPDATE decision_model_calls SET finished_ms=?4, outcome=?5, usage_json=?6 WHERE root_id=?1 AND decision_id=?2 AND generation=?3 AND finished_ms IS NULL AND started_ms<=?4",
            params![
                completion.root.as_str(),
                completion.decision.as_str(),
                completion.generation,
                completion.at_ms,
                enum_to_str(&completion.outcome),
                serde_json::to_string(&completion.usage)?
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "decision call is absent, late, or already finalized".into(),
            ));
        }
        Ok(())
    })
}

pub fn highest_generation(storage: &Storage) -> Result<u64, StoreError> {
    Ok(storage.read().query_row(
        "SELECT COALESCE(MAX(generation),0) FROM model_calls",
        [],
        |row| row.get(0),
    )?)
}

pub fn started(
    storage: &mut Storage,
    request: &PlanningRequest,
    at_ms: u64,
    provider: &str,
    model: &str,
) -> Result<(), StoreError> {
    if provider.is_empty()
        || provider.trim().is_empty()
        || provider.len() > 512
        || model.is_empty()
        || model.trim().is_empty()
        || model.len() > 256
        || request.validate().is_err()
        || at_ms > request.deadline_ms
    {
        return Err(StoreError::Conflict(
            "invalid model-call identity or deadline".into(),
        ));
    }
    storage.transaction(|tx| {
        tx.execute("INSERT INTO model_calls(root_id, request_id, generation, provider, model, started_ms, usage_json, reason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![request.root_task_id.as_str(), request.request_id.as_str(), request.generation, provider, model, at_ms, serde_json::to_string(&ModelUsage::default())?, enum_to_str(&request.reason)])?;
        Ok(())
    })
}

pub fn finished(storage: &mut Storage, completion: &ModelCompletion) -> Result<(), StoreError> {
    storage.transaction(|tx| {
        let changed = tx.execute("UPDATE model_calls SET finished_ms=?4, outcome=?5, usage_json=?6 WHERE root_id=?1 AND request_id=?2 AND generation=?3 AND finished_ms IS NULL AND started_ms<=?4",
            params![completion.root.as_str(), completion.request.as_str(), completion.generation, completion.at_ms, enum_to_str(&completion.outcome), serde_json::to_string(&completion.usage)?])?;
        if changed != 1 { return Err(StoreError::Conflict("model call is absent or already finalized".into())); }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{CreateOutcome, TaskConfig, TaskRuntime};
    use interaction_contracts::desktop::decision::{
        Candidate, DecisionState, Precondition, Question, QuestionKind, QuestionOption,
    };
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::ids::{CandidateId, SemanticTargetRef};
    use interaction_contracts::desktop::planning::{PlanningContext, PlanningReason};
    fn setup() -> (Storage, PlanningRequest) {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut tasks = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = tasks.create_root(&mut storage, 0, "inspect").unwrap()
        else {
            panic!()
        };
        let request = PlanningRequest {
            root_task_id: root,
            request_id: RequestId::new("model-call").unwrap(),
            generation: 1,
            task_revision: 1,
            root_epoch: 1,
            reason: PlanningReason::GoalCompile,
            deadline_ms: 1000,
            context: PlanningContext {
                user_text: "inspect".into(),
                allowed_scope: None,
                unknown_items: vec![],
                allowed_skills: vec![],
                recent_failures: vec![],
                notes: vec![],
            },
        };
        (storage, request)
    }

    fn decision_request(root: &RootTaskId, generation: u64, id: &str) -> DecisionRequest {
        DecisionRequest {
            decision_id: DecisionId::new(id).unwrap(),
            lane_id: "teacher-accounting-test".into(),
            request_generation: generation,
            root_task_id: root.clone(),
            task_revision: 1,
            plan_revision: 1,
            root_epoch: 1,
            decision_program: "ui.step@1".into(),
            calibration_revision: "none".into(),
            state: DecisionState {
                root_task_id: root.clone(),
                task_revision: 1,
                goal_summary: "inspect".into(),
                subgoal: "choose".into(),
                snapshot_revision: 1,
                objects: vec![],
                candidates: vec![Candidate {
                    candidate_id: CandidateId::new("candidate-1").unwrap(),
                    skill: "ui.revisit".into(),
                    semantic_target: Some(SemanticTargetRef::new("target-1").unwrap()),
                    binding: None,
                    parameters: Default::default(),
                    effect_class: EffectClass::ReadOnly,
                    precondition: Precondition::Executable,
                    description: "revisit".into(),
                }],
                recent_results: vec![],
                missing_fields: vec![],
                omitted_items: vec![],
                candidate_set_digest: "sha256:fixture".into(),
            },
            questions: vec![Question {
                id: "q1".into(),
                kind: QuestionKind::Choice,
                prompt: "choose".into(),
                options: vec![QuestionOption {
                    key: "candidate-1".into(),
                    description: "revisit".into(),
                }],
            }],
            created_ms: 5,
            result_deadline_ms: 100,
        }
    }
    #[test]
    fn cancelled_calls_keep_unknown_cost_and_cannot_be_finalized_twice() {
        let (mut storage, request) = setup();
        started(&mut storage, &request, 10, "open_ai", "fixture-model").unwrap();
        let mut completion = ModelCompletion {
            root: request.root_task_id.clone(),
            request: request.request_id.clone(),
            generation: 1,
            at_ms: 20,
            outcome: ModelOutcome::Cancelled,
            usage: ModelUsage {
                input_tokens: Some(12),
                output_tokens: None,
                cost_micros: None,
            },
        };
        finished(&mut storage, &completion).unwrap();
        let (outcome, usage): (String, String) = storage
            .read()
            .query_row("SELECT outcome,usage_json FROM model_calls", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(outcome, "cancelled");
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage)
                .unwrap()
                .cost_micros,
            None
        );
        assert!(finished(&mut storage, &completion).is_err());
        completion.generation = 2;
        assert!(finished(&mut storage, &completion).is_err());
    }
    #[test]
    fn an_interrupted_call_stays_open_with_unknown_usage() {
        let (mut storage, request) = setup();
        started(&mut storage, &request, 10, "open_ai", "fixture-model").unwrap();
        let (ended, reason): (Option<u64>, Option<String>) = storage
            .read()
            .query_row("SELECT finished_ms, reason FROM model_calls", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ended, None);
        assert_eq!(reason.as_deref(), Some("goal_compile"));
    }

    #[test]
    fn a_planner_call_cannot_start_after_its_deadline_or_finish_before_start() {
        let (mut storage, request) = setup();
        assert!(
            started(
                &mut storage,
                &request,
                request.deadline_ms + 1,
                "open_ai",
                "fixture-model"
            )
            .is_err()
        );
        assert!(started(&mut storage, &request, 10, "   ", "fixture-model").is_err());
        started(&mut storage, &request, 10, "open_ai", "fixture-model").unwrap();

        let completion = ModelCompletion {
            root: request.root_task_id.clone(),
            request: request.request_id.clone(),
            generation: request.generation,
            at_ms: 9,
            outcome: ModelOutcome::Cancelled,
            usage: ModelUsage::default(),
        };
        assert!(finished(&mut storage, &completion).is_err());
        let ended: Option<u64> = storage
            .read()
            .query_row("SELECT finished_ms FROM model_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(ended, None);
    }

    #[test]
    fn restarting_uses_new_scopes_without_replaying_an_unfinished_call() {
        let (mut storage, mut request) = setup();
        request.generation = 4;
        started(&mut storage, &request, 10, "open_ai", "fixture-model").unwrap();
        let mut coordinator = crate::deliberation::PlanningCoordinator::new();
        coordinator.advance_past(highest_generation(&storage).unwrap());
        assert!(coordinator.inflight().is_none());
        let next = coordinator.request(
            &request.root_task_id,
            1,
            2,
            PlanningReason::Escalation,
            request.context.clone(),
            20,
            1000,
        );
        assert_eq!(next.generation, 5);
        assert_eq!(next.request_id.as_str(), "plan-5");
        started(&mut storage, &next, 20, "open_ai", "fixture-model").unwrap();
        let ended: Option<u64> = storage
            .read()
            .query_row(
                "SELECT finished_ms FROM model_calls WHERE generation=4",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ended, None);
    }

    #[test]
    fn decision_calls_have_independent_identity_and_preserve_unknown_usage() {
        let (mut storage, planning) = setup();
        let request = decision_request(&planning.root_task_id, 3, "decision-1");
        decision_call_started(&mut storage, &request, 10, "open_ai", "teacher-fixture").unwrap();
        let completion = DecisionCallCompletion {
            root: request.root_task_id.clone(),
            decision: request.decision_id.clone(),
            generation: request.request_generation,
            at_ms: 20,
            outcome: DecisionCallOutcome::Proposal,
            usage: ModelUsage {
                input_tokens: Some(13),
                output_tokens: Some(8),
                cost_micros: None,
            },
        };
        decision_call_finished(&mut storage, &completion).unwrap();
        let (provider, outcome, usage): (String, String, String) = storage
            .read()
            .query_row(
                "SELECT provider,outcome,usage_json FROM decision_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(provider, "open_ai");
        assert_eq!(outcome, "proposal");
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage).unwrap(),
            completion.usage
        );
        let planner_rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM model_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(planner_rows, 0);
        assert!(decision_call_finished(&mut storage, &completion).is_err());
    }

    #[test]
    fn crash_rows_stay_unknown_and_late_other_scope_cannot_overwrite_cancel() {
        let (mut storage, planning) = setup();
        let interrupted = decision_request(&planning.root_task_id, 1, "decision-open");
        decision_call_started(&mut storage, &interrupted, 10, "open_ai", "teacher-fixture")
            .unwrap();
        let open: (Option<u64>, Option<String>, String) = storage
            .read()
            .query_row(
                "SELECT finished_ms,outcome,usage_json FROM decision_model_calls WHERE decision_id=?1",
                [interrupted.decision_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(open.0, None);
        assert_eq!(open.1, None);
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&open.2).unwrap(),
            ModelUsage::default()
        );

        let cancelled = decision_request(&planning.root_task_id, 2, "decision-cancel");
        decision_call_started(&mut storage, &cancelled, 10, "open_ai", "teacher-fixture").unwrap();
        let completion = DecisionCallCompletion {
            root: cancelled.root_task_id.clone(),
            decision: cancelled.decision_id.clone(),
            generation: cancelled.request_generation,
            at_ms: 20,
            outcome: DecisionCallOutcome::Cancelled,
            usage: ModelUsage::default(),
        };
        decision_call_finished(&mut storage, &completion).unwrap();
        let mut late = DecisionCallCompletion {
            outcome: DecisionCallOutcome::Proposal,
            ..completion
        };
        late.generation += 1;
        assert!(decision_call_finished(&mut storage, &late).is_err());
        let outcome: String = storage
            .read()
            .query_row(
                "SELECT outcome FROM decision_model_calls WHERE decision_id=?1",
                [cancelled.decision_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outcome, "cancelled");
    }

    #[test]
    fn invalid_decision_route_or_scope_does_not_start_a_call() {
        let (mut storage, planning) = setup();
        let request = decision_request(&planning.root_task_id, 1, "decision-invalid");
        assert!(decision_call_started(&mut storage, &request, 4, "open_ai", "model").is_err());
        assert!(decision_call_started(&mut storage, &request, 10, "", "model").is_err());
        assert!(
            decision_call_started(&mut storage, &request, 10, "open_ai", "model\nkey").is_err()
        );
        let rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM decision_model_calls", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
    }
}
