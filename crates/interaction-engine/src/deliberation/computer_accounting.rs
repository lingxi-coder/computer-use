//! Independent persistent accounting for computer-use model calls.
//!
//! This ledger deliberately does not share rows or identities with S1 teacher
//! decisions or S2 planning. It stores the typed call scope and usage only;
//! prompts, images, provider frames and proposals never enter SQLite.

use interaction_contracts::desktop::computer::{ComputerFence, MAX_COMPUTER_SAFE_INTEGER};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::accounting::ModelUsage;
use crate::infrastructure::storage::{Storage, StoreError, enum_to_str};

/// Which computer-model observation step consumed one provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerCallStage {
    Begin,
    InitialScreenshot,
    FreshScreenshot,
}

/// The complete immutable scope for one generated computer call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerCallIdentity {
    pub fence: ComputerFence,
    pub plan_revision: u64,
    pub subgoal_id: String,
    pub created_ms: u64,
    pub stage: ComputerCallStage,
}

impl ComputerCallIdentity {
    pub fn validate(&self) -> Result<(), String> {
        self.fence.validate()?;
        if self.plan_revision == 0 || self.plan_revision > MAX_COMPUTER_SAFE_INTEGER {
            return Err("computer plan revision must be positive and JavaScript-safe".into());
        }
        if self.subgoal_id.trim().is_empty()
            || self.subgoal_id.len() > 256
            || self.subgoal_id.chars().any(char::is_control)
        {
            return Err("computer subgoal id must be nonblank, bounded and control-free".into());
        }
        if self.created_ms > self.fence.deadline_ms {
            return Err("computer call creation time is after its deadline".into());
        }
        Ok(())
    }
}

/// A sanitized terminal status for a computer model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerCallOutcome {
    Proposal,
    Unavailable,
    Missing,
    Conflict,
    Invalid,
    Quarantined,
    Cancelled,
    Expired,
    Revoked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerCallCompletion {
    pub identity: ComputerCallIdentity,
    pub at_ms: u64,
    pub outcome: ComputerCallOutcome,
    pub usage: ModelUsage,
}

/// Persist the complete scope before the provider worker is started.
pub fn started(
    storage: &mut Storage,
    identity: &ComputerCallIdentity,
    at_ms: u64,
    provider: &str,
    model: &str,
) -> Result<(), StoreError> {
    if identity.validate().is_err()
        || at_ms < identity.created_ms
        || at_ms >= identity.fence.deadline_ms
        || !valid_text(provider, 512)
        || !valid_text(model, 256)
    {
        return Err(StoreError::Conflict(
            "invalid computer-call identity, route or deadline".into(),
        ));
    }

    let fence = &identity.fence;
    storage.transaction(|tx| {
        let inserted = tx.execute(
            "INSERT INTO computer_model_calls(
                root_id, request_id, generation, task_revision, root_epoch, snapshot_id,
                structure_sequence, deadline_ms, plan_revision, subgoal_id, created_ms,
                stage, provider, model, started_ms, usage_json
             )
             SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16
             FROM root_tasks
             WHERE root_id=?1 AND task_revision=?4 AND root_epoch=?5
               AND plan_revision=?9 AND plan_json IS NOT NULL AND cancel_requested=0
               AND state NOT IN ('completed','failed','unverified','expired','cancelled')",
            params![
                fence.root_task_id.as_str(),
                fence.request_id.as_str(),
                fence.generation as i64,
                fence.task_revision as i64,
                fence.root_epoch as i64,
                fence.snapshot_id,
                fence.structure_sequence as i64,
                fence.deadline_ms as i64,
                identity.plan_revision as i64,
                identity.subgoal_id,
                identity.created_ms as i64,
                enum_to_str(&identity.stage),
                provider,
                model,
                at_ms as i64,
                serde_json::to_string(&ModelUsage::default())?,
            ],
        )?;
        if inserted != 1 {
            return Err(StoreError::Conflict(
                "computer call root/task/plan scope is absent or stale".into(),
            ));
        }
        Ok(())
    })
}

/// Finish the exact open identity and return its effective persisted outcome.
/// Root revocation and deadline-late close events can account final usage, but
/// can never turn into a successful proposal that Engine could install.
pub fn finished(
    storage: &mut Storage,
    completion: &ComputerCallCompletion,
) -> Result<ComputerCallOutcome, StoreError> {
    let identity = &completion.identity;
    if identity.validate().is_err() || completion.at_ms > MAX_COMPUTER_SAFE_INTEGER {
        return Err(StoreError::Conflict(
            "invalid computer-call completion scope or timestamp".into(),
        ));
    }
    let fence = &identity.fence;
    storage.transaction(|tx| {
        // A call can still close after Engine has advanced or stopped its root.
        // Check that durable scope in the same writer transaction as the CAS.
        let root_scope: Option<(i64, i64, i64, bool, String)> = tx
            .query_row(
                "SELECT task_revision,root_epoch,plan_revision,cancel_requested,state
                 FROM root_tasks WHERE root_id=?1",
                [fence.root_task_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let root_cancelled = root_scope
            .as_ref()
            .is_some_and(|(_, _, _, cancelled, state)| {
                *cancelled || matches!(state.as_str(), "cancelling" | "cancelled")
            });
        let root_scope_revoked = root_scope.as_ref().is_none_or(
            |(task_revision, root_epoch, plan_revision, cancelled, state)| {
                *task_revision != fence.task_revision as i64
                    || *root_epoch != fence.root_epoch as i64
                    || *plan_revision != identity.plan_revision as i64
                    || *cancelled
                    || matches!(
                        state.as_str(),
                        "completed" | "failed" | "unverified" | "expired" | "cancelled"
                    )
            },
        );
        let persisted_outcome = if root_scope_revoked {
            if root_cancelled {
                ComputerCallOutcome::Cancelled
            } else {
                ComputerCallOutcome::Revoked
            }
        } else if completion.at_ms < fence.deadline_ms
            || matches!(
                completion.outcome,
                ComputerCallOutcome::Expired
                    | ComputerCallOutcome::Cancelled
                    | ComputerCallOutcome::Revoked
                    | ComputerCallOutcome::Failed
            )
        {
            completion.outcome
        } else {
            ComputerCallOutcome::Expired
        };
        let changed = tx.execute(
            "UPDATE computer_model_calls
             SET finished_ms=?14, outcome=?15, usage_json=?16
             WHERE root_id=?1 AND request_id=?2 AND generation=?3
               AND task_revision=?4 AND root_epoch=?5 AND snapshot_id=?6
               AND structure_sequence=?7 AND deadline_ms=?8 AND plan_revision=?9
               AND subgoal_id=?10 AND created_ms=?11 AND stage=?12
               AND finished_ms IS NULL AND started_ms<=?14 AND started_ms>=?13",
            params![
                fence.root_task_id.as_str(),
                fence.request_id.as_str(),
                fence.generation as i64,
                fence.task_revision as i64,
                fence.root_epoch as i64,
                fence.snapshot_id,
                fence.structure_sequence as i64,
                fence.deadline_ms as i64,
                identity.plan_revision as i64,
                identity.subgoal_id,
                identity.created_ms as i64,
                enum_to_str(&identity.stage),
                identity.created_ms as i64,
                completion.at_ms as i64,
                enum_to_str(&persisted_outcome),
                serde_json::to_string(&completion.usage)?,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "computer call is absent, foreign, late or already finalized".into(),
            ));
        }
        Ok(persisted_outcome)
    })
}

/// Allocate a durable global generation. IDs may be skipped after a crash but
/// are never reconstructed from rows or reused.
pub fn allocate_generation(storage: &mut Storage) -> Result<u64, StoreError> {
    allocate_counter(storage, "next_generation")
}

/// Allocate a durable global computer batch ID with the same no-replay rule.
pub fn allocate_batch_id(storage: &mut Storage) -> Result<u64, StoreError> {
    allocate_counter(storage, "next_batch_id")
}

fn allocate_counter(storage: &mut Storage, column: &'static str) -> Result<u64, StoreError> {
    let update = format!(
        "UPDATE computer_accounting_counters
         SET {column}={column}+1
         WHERE singleton=1 AND {column} < ?1"
    );
    let select = format!("SELECT {column} FROM computer_accounting_counters WHERE singleton=1");
    storage.transaction(|tx| {
        let changed = tx.execute(&update, [MAX_COMPUTER_SAFE_INTEGER as i64])?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "computer accounting counter is missing or exhausted".into(),
            ));
        }
        let value: i64 = tx.query_row(&select, [], |row| row.get(0))?;
        Ok(value as u64)
    })
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{CreateOutcome, TaskConfig, TaskRuntime};
    use interaction_contracts::desktop::computer::ComputerFence;
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};

    fn setup() -> (Storage, RootTaskId) {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut tasks = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = tasks.create_root(&mut storage, 0, "inspect").unwrap()
        else {
            panic!("test root should start");
        };
        storage
            .transaction(|tx| {
                tx.execute(
                    "UPDATE root_tasks
                     SET root_epoch=2, plan_revision=5, plan_json='{}'
                     WHERE root_id=?1",
                    [root.as_str()],
                )?;
                Ok::<(), StoreError>(())
            })
            .unwrap();
        (storage, root)
    }

    fn identity(root: &RootTaskId, generation: u64) -> ComputerCallIdentity {
        ComputerCallIdentity {
            fence: ComputerFence {
                request_id: RequestId::new(format!("computer-call-{generation}")).unwrap(),
                generation,
                root_task_id: root.clone(),
                task_revision: 1,
                root_epoch: 2,
                snapshot_id: "snapshot-3".into(),
                structure_sequence: 4,
                deadline_ms: 1_000,
            },
            plan_revision: 5,
            subgoal_id: "subgoal-6".into(),
            created_ms: 10,
            stage: ComputerCallStage::Begin,
        }
    }

    fn completion(identity: ComputerCallIdentity, at_ms: u64) -> ComputerCallCompletion {
        ComputerCallCompletion {
            identity,
            at_ms,
            outcome: ComputerCallOutcome::Proposal,
            usage: ModelUsage {
                input_tokens: Some(12),
                output_tokens: Some(7),
                cost_micros: None,
            },
        }
    }

    #[test]
    fn started_persists_an_open_unknown_usage_row_before_finishing_once() {
        let (mut storage, root) = setup();
        let identity = identity(&root, 1);
        started(&mut storage, &identity, 11, "open_ai", "fixture-model").unwrap();

        let (finished_ms, outcome, usage): (Option<i64>, Option<String>, String) = storage
            .read()
            .query_row(
                "SELECT finished_ms,outcome,usage_json FROM computer_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(finished_ms, None);
        assert_eq!(outcome, None);
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage).unwrap(),
            ModelUsage::default()
        );

        let done = completion(identity.clone(), 20);
        finished(&mut storage, &done).unwrap();
        assert!(finished(&mut storage, &done).is_err());
        let (outcome, usage): (String, String) = storage
            .read()
            .query_row(
                "SELECT outcome,usage_json FROM computer_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(outcome, "proposal");
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage).unwrap(),
            done.usage,
            "unknown cost stays None"
        );
    }

    #[test]
    fn foreign_scopes_cannot_finalize_and_late_proposal_closes_as_expired() {
        let (mut storage, root) = setup();
        let original = identity(&root, 1);
        started(&mut storage, &original, 11, "open_ai", "fixture-model").unwrap();

        let mut wrong_stage = original.clone();
        wrong_stage.stage = ComputerCallStage::FreshScreenshot;
        assert!(finished(&mut storage, &completion(wrong_stage, 20)).is_err());
        let mut wrong_plan = original.clone();
        wrong_plan.plan_revision += 1;
        assert!(finished(&mut storage, &completion(wrong_plan, 20)).is_err());
        let mut wrong_subgoal = original.clone();
        wrong_subgoal.subgoal_id.push_str("-foreign");
        assert!(finished(&mut storage, &completion(wrong_subgoal, 20)).is_err());
        let mut wrong_root = original.clone();
        wrong_root.fence.root_task_id = RootTaskId::new("another-root").unwrap();
        let mut foreign_late = completion(wrong_root, 1_000);
        foreign_late.outcome = ComputerCallOutcome::Cancelled;
        assert!(finished(&mut storage, &foreign_late).is_err());

        let still_open: (Option<i64>, Option<String>, String) = storage
            .read()
            .query_row(
                "SELECT finished_ms,outcome,usage_json FROM computer_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(still_open.0, None);
        assert_eq!(still_open.1, None);
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&still_open.2).unwrap(),
            ModelUsage::default()
        );

        let late_proposal = completion(original.clone(), 1_000);
        assert_eq!(
            finished(&mut storage, &late_proposal).unwrap(),
            ComputerCallOutcome::Expired
        );
        assert!(finished(&mut storage, &late_proposal).is_err());
        let (outcome, finished_ms, usage): (String, i64, String) = storage
            .read()
            .query_row(
                "SELECT outcome,finished_ms,usage_json FROM computer_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(outcome, "expired");
        assert_eq!(finished_ms, 1_000);
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage).unwrap(),
            late_proposal.usage,
            "late close preserves reported usage while refusing success"
        );
    }

    #[test]
    fn late_cancel_revoke_failure_and_expiry_preserve_closing_usage() {
        let (mut storage, root) = setup();
        for (index, outcome) in [
            ComputerCallOutcome::Cancelled,
            ComputerCallOutcome::Revoked,
            ComputerCallOutcome::Failed,
            ComputerCallOutcome::Expired,
        ]
        .into_iter()
        .enumerate()
        {
            let generation = (index + 1) as u64;
            let scope = identity(&root, generation);
            started(&mut storage, &scope, 11, "open_ai", "fixture-model").unwrap();
            let mut close = completion(scope, 1_005);
            close.outcome = outcome;
            close.usage = ModelUsage {
                input_tokens: Some(20 + index as u64),
                output_tokens: None,
                cost_micros: None,
            };
            assert_eq!(finished(&mut storage, &close).unwrap(), outcome);
            let (saved_outcome, usage): (String, String) = storage
                .read()
                .query_row(
                    "SELECT outcome,usage_json FROM computer_model_calls WHERE generation=?1",
                    [generation as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(saved_outcome, enum_to_str(&outcome));
            assert_eq!(
                serde_json::from_str::<ModelUsage>(&usage).unwrap(),
                close.usage
            );
        }
    }

    #[test]
    fn stopped_or_revised_root_revokes_a_proposal_and_keeps_usage() {
        let (mut storage, root) = setup();
        let scope = identity(&root, 1);
        started(&mut storage, &scope, 11, "open_ai", "fixture-model").unwrap();
        storage
            .transaction(|tx| {
                tx.execute(
                    "UPDATE root_tasks
                     SET cancel_requested=1, root_epoch=root_epoch+1, state='cancelling'
                     WHERE root_id=?1",
                    [root.as_str()],
                )?;
                Ok::<(), StoreError>(())
            })
            .unwrap();

        let proposal = completion(scope, 20);
        assert_eq!(
            finished(&mut storage, &proposal).unwrap(),
            ComputerCallOutcome::Cancelled
        );
        let (outcome, usage): (String, String) = storage
            .read()
            .query_row(
                "SELECT outcome,usage_json FROM computer_model_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(outcome, "cancelled");
        assert_eq!(
            serde_json::from_str::<ModelUsage>(&usage).unwrap(),
            proposal.usage
        );
    }

    #[test]
    fn calls_use_an_independent_table_and_do_not_fake_usage() {
        let (mut storage, root) = setup();
        let scope = identity(&root, 1);
        started(&mut storage, &scope, 11, "open_ai", "fixture-model").unwrap();
        let mut replayed_generation = identity(&root, 1);
        replayed_generation.fence.request_id = RequestId::new("different-request").unwrap();
        assert!(
            started(
                &mut storage,
                &replayed_generation,
                12,
                "open_ai",
                "fixture-model"
            )
            .is_err()
        );
        let planning_rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM model_calls", [], |row| row.get(0))
            .unwrap();
        let teacher_rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM decision_model_calls", [], |row| {
                row.get(0)
            })
            .unwrap();
        let computer_rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM computer_model_calls", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((planning_rows, teacher_rows, computer_rows), (0, 0, 1));
    }

    #[test]
    fn durable_generation_and_batch_high_waters_allow_skips_but_never_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-accounting.db");
        {
            let mut storage = Storage::open(&path).unwrap();
            assert_eq!(allocate_generation(&mut storage).unwrap(), 1);
            assert_eq!(allocate_batch_id(&mut storage).unwrap(), 1);
            assert_eq!(allocate_generation(&mut storage).unwrap(), 2);
            assert_eq!(allocate_batch_id(&mut storage).unwrap(), 2);
        }
        let mut reopened = Storage::open(&path).unwrap();
        assert_eq!(allocate_generation(&mut reopened).unwrap(), 3);
        assert_eq!(allocate_batch_id(&mut reopened).unwrap(), 3);
    }

    #[test]
    fn invalid_scope_and_route_do_not_create_accounting_rows() {
        let (mut storage, root) = setup();
        let valid = identity(&root, 1);
        assert!(started(&mut storage, &valid, 9, "open_ai", "model").is_err());
        assert!(started(&mut storage, &valid, 1_000, "open_ai", "model").is_err());
        assert!(started(&mut storage, &valid, 11, "  ", "model").is_err());
        assert!(started(&mut storage, &valid, 11, "open_ai", "model\nkey").is_err());
        let mut no_plan = valid.clone();
        no_plan.plan_revision = 0;
        assert!(started(&mut storage, &no_plan, 11, "open_ai", "model").is_err());
        let mut no_subgoal = valid.clone();
        no_subgoal.subgoal_id = " \n".into();
        assert!(started(&mut storage, &no_subgoal, 11, "open_ai", "model").is_err());
        let mut stale_task = valid.clone();
        stale_task.fence.task_revision += 1;
        assert!(started(&mut storage, &stale_task, 11, "open_ai", "model").is_err());
        let mut stale_plan = valid.clone();
        stale_plan.plan_revision += 1;
        assert!(started(&mut storage, &stale_plan, 11, "open_ai", "model").is_err());
        let rows: u64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM computer_model_calls", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
    }
}
