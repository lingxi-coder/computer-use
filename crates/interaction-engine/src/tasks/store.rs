//! TaskRuntime's tables: root_tasks, budget_ledger and attempts.
//! Nothing outside this module writes them.

use interaction_contracts::desktop::decision::DecisionSource;
use interaction_contracts::desktop::device::ReleaseStatus;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{AttemptId, DecisionId, RootTaskId};
use interaction_contracts::desktop::planning::PlanBundle;
use interaction_contracts::desktop::task::{AttemptQuota, AttemptState, GoalSpec, RootState};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::budget::BudgetKind;
use crate::infrastructure::storage::{StoreError, enum_to_str, str_to_enum};

#[derive(Debug, Clone, PartialEq)]
pub struct RootRow {
    pub root_id: RootTaskId,
    pub seq: u64,
    pub user_text: String,
    pub state: RootState,
    pub state_origin: Option<RootState>,
    pub task_revision: u64,
    pub root_epoch: u64,
    /// The compiled goal waiting for confirmation. Its budget deadline is a
    /// duration until it is frozen into an absolute time.
    pub draft: Option<GoalSpec>,
    /// Set when the user confirms; immutable afterwards.
    pub goal: Option<GoalSpec>,
    pub plan_revision: u64,
    pub plan: Option<PlanBundle>,
    pub created_ms: u64,
    pub frozen_ms: Option<u64>,
    pub deadline_ms: Option<u64>,
    pub cancel_requested: bool,
    pub cleanup: ReleaseStatus,
    pub activation_seq: u64,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetCounts {
    pub attempts: u32,
    pub observations: u32,
    pub recoveries: u32,
    pub replans: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    pub attempt_id: AttemptId,
    pub root_id: RootTaskId,
    pub decision_id: DecisionId,
    pub generation: u64,
    pub state: AttemptState,
    pub skill: String,
    pub subgoal: String,
    pub quota: AttemptQuota,
    pub activation_seq: u64,
    pub root_epoch: u64,
    pub task_revision: u64,
    pub plan_revision: u64,
    pub deadline_ms: u64,
    pub effect_class: EffectClass,
    pub source: DecisionSource,
    pub refunded: bool,
    pub created_ms: u64,
    pub intent_id: Option<String>,
}

fn json<T: serde::Serialize>(value: &Option<T>) -> Result<Option<String>, StoreError> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(StoreError::from)
}

fn unjson<T: serde::de::DeserializeOwned>(text: Option<String>) -> Result<Option<T>, StoreError> {
    text.map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(StoreError::from)
}

fn id<T: TryFrom<String>>(text: String, what: &str) -> Result<T, StoreError> {
    T::try_from(text).map_err(|_| StoreError::Corrupt(format!("invalid {what} in the database")))
}

pub struct TaskStore;

impl TaskStore {
    pub fn insert_root(tx: &Transaction<'_>, row: &RootRow) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO root_tasks (root_id, seq, user_text, state, state_origin, task_revision,
                root_epoch, draft_json, goal_json, plan_revision, plan_json, created_ms, frozen_ms,
                deadline_ms, cancel_requested, cleanup, activation_seq, failure)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                row.root_id.as_str(),
                row.seq as i64,
                row.user_text,
                enum_to_str(&row.state),
                row.state_origin.as_ref().map(enum_to_str),
                row.task_revision as i64,
                row.root_epoch as i64,
                json(&row.draft)?,
                json(&row.goal)?,
                row.plan_revision as i64,
                json(&row.plan)?,
                row.created_ms as i64,
                row.frozen_ms.map(|v| v as i64),
                row.deadline_ms.map(|v| v as i64),
                row.cancel_requested,
                enum_to_str(&row.cleanup),
                row.activation_seq as i64,
                row.failure,
            ],
        )?;
        Ok(())
    }

    /// Write every mutable column of an existing root.
    pub fn save_root(tx: &Transaction<'_>, row: &RootRow) -> Result<(), StoreError> {
        let changed = tx.execute(
            "UPDATE root_tasks SET state = ?2, state_origin = ?3, task_revision = ?4, root_epoch = ?5,
                draft_json = ?6, goal_json = ?7, plan_revision = ?8, plan_json = ?9, frozen_ms = ?10,
                deadline_ms = ?11, cancel_requested = ?12, cleanup = ?13, activation_seq = ?14,
                failure = ?15
             WHERE root_id = ?1",
            params![
                row.root_id.as_str(),
                enum_to_str(&row.state),
                row.state_origin.as_ref().map(enum_to_str),
                row.task_revision as i64,
                row.root_epoch as i64,
                json(&row.draft)?,
                json(&row.goal)?,
                row.plan_revision as i64,
                json(&row.plan)?,
                row.frozen_ms.map(|v| v as i64),
                row.deadline_ms.map(|v| v as i64),
                row.cancel_requested,
                enum_to_str(&row.cleanup),
                row.activation_seq as i64,
                row.failure,
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::Conflict(format!(
                "root {} does not exist",
                row.root_id
            )))
        }
    }

    fn root_from(row: &Row<'_>) -> Result<RootRow, StoreError> {
        let text = |index: usize| -> Result<String, StoreError> { Ok(row.get(index)?) };
        let origin: Option<String> = row.get(4)?;
        Ok(RootRow {
            root_id: id(text(0)?, "root id")?,
            seq: row.get::<_, i64>(1)? as u64,
            user_text: text(2)?,
            state: str_to_enum(&text(3)?)?,
            state_origin: origin.map(|s| str_to_enum(&s)).transpose()?,
            task_revision: row.get::<_, i64>(5)? as u64,
            root_epoch: row.get::<_, i64>(6)? as u64,
            draft: unjson(row.get(7)?)?,
            goal: unjson(row.get(8)?)?,
            plan_revision: row.get::<_, i64>(9)? as u64,
            plan: unjson(row.get(10)?)?,
            created_ms: row.get::<_, i64>(11)? as u64,
            frozen_ms: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
            deadline_ms: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
            cancel_requested: row.get(14)?,
            cleanup: str_to_enum(&text(15)?)?,
            activation_seq: row.get::<_, i64>(16)? as u64,
            failure: row.get(17)?,
        })
    }

    const ROOT_COLUMNS: &'static str =
        "root_id, seq, user_text, state, state_origin, task_revision,
        root_epoch, draft_json, goal_json, plan_revision, plan_json, created_ms, frozen_ms,
        deadline_ms, cancel_requested, cleanup, activation_seq, failure";

    pub fn load_root(conn: &Connection, root_id: &str) -> Result<Option<RootRow>, StoreError> {
        let sql = format!(
            "SELECT {} FROM root_tasks WHERE root_id = ?1",
            Self::ROOT_COLUMNS
        );
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query([root_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::root_from(row)?)),
            None => Ok(None),
        }
    }

    /// Every root that has not reached a final state, oldest first.
    pub fn load_unfinished(conn: &Connection) -> Result<Vec<RootRow>, StoreError> {
        let sql = format!(
            "SELECT {} FROM root_tasks
             WHERE state NOT IN ('completed', 'failed', 'unverified', 'expired', 'cancelled')
             ORDER BY seq",
            Self::ROOT_COLUMNS
        );
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::root_from(row)?);
        }
        Ok(out)
    }

    pub fn next_seq(conn: &Connection) -> Result<u64, StoreError> {
        let max: Option<i64> =
            conn.query_row("SELECT MAX(seq) FROM root_tasks", [], |row| row.get(0))?;
        Ok(max.map_or(1, |value| value as u64 + 1))
    }

    /// The next activation number of a root, persisted so it never repeats.
    pub fn next_activation_seq(tx: &Transaction<'_>, root_id: &str) -> Result<u64, StoreError> {
        tx.execute(
            "UPDATE root_tasks SET activation_seq = activation_seq + 1 WHERE root_id = ?1",
            [root_id],
        )?;
        let seq: i64 = tx.query_row(
            "SELECT activation_seq FROM root_tasks WHERE root_id = ?1",
            [root_id],
            |row| row.get(0),
        )?;
        Ok(seq as u64)
    }

    pub fn init_budget(tx: &Transaction<'_>, root_id: &str) -> Result<(), StoreError> {
        tx.execute(
            "INSERT OR IGNORE INTO budget_ledger (root_id) VALUES (?1)",
            [root_id],
        )?;
        Ok(())
    }

    pub fn load_budget(conn: &Connection, root_id: &str) -> Result<BudgetCounts, StoreError> {
        let counts = conn
            .query_row(
                "SELECT attempts_used, observations_used, recoveries_used, replans_used
                 FROM budget_ledger WHERE root_id = ?1",
                [root_id],
                |row| {
                    Ok(BudgetCounts {
                        attempts: row.get::<_, i64>(0)? as u32,
                        observations: row.get::<_, i64>(1)? as u32,
                        recoveries: row.get::<_, i64>(2)? as u32,
                        replans: row.get::<_, i64>(3)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(counts.unwrap_or_default())
    }

    fn budget_column(kind: BudgetKind) -> &'static str {
        match kind {
            BudgetKind::Attempt => "attempts_used",
            BudgetKind::Observation => "observations_used",
            BudgetKind::Recovery => "recoveries_used",
            BudgetKind::Replan => "replans_used",
        }
    }

    pub fn spend(
        tx: &Transaction<'_>,
        root_id: &str,
        kind: BudgetKind,
        amount: u32,
    ) -> Result<(), StoreError> {
        let column = Self::budget_column(kind);
        tx.execute(
            &format!("UPDATE budget_ledger SET {column} = {column} + ?2 WHERE root_id = ?1"),
            params![root_id, amount as i64],
        )?;
        Ok(())
    }

    pub fn refund(
        tx: &Transaction<'_>,
        root_id: &str,
        kind: BudgetKind,
        amount: u32,
    ) -> Result<(), StoreError> {
        let column = Self::budget_column(kind);
        tx.execute(
            &format!(
                "UPDATE budget_ledger SET {column} = MAX(0, {column} - ?2) WHERE root_id = ?1"
            ),
            params![root_id, amount as i64],
        )?;
        Ok(())
    }

    pub fn insert_attempt(tx: &Transaction<'_>, row: &AttemptRow) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO attempts (attempt_id, root_id, decision_id, generation, state, skill, subgoal,
                quota_actions, quota_observations, activation_seq, root_epoch, task_revision,
                plan_revision, deadline_ms, effect_class, source, refunded, created_ms, intent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                row.attempt_id.as_str(),
                row.root_id.as_str(),
                row.decision_id.as_str(),
                row.generation as i64,
                enum_to_str(&row.state),
                row.skill,
                row.subgoal,
                row.quota.actions as i64,
                row.quota.observations as i64,
                row.activation_seq as i64,
                row.root_epoch as i64,
                row.task_revision as i64,
                row.plan_revision as i64,
                row.deadline_ms as i64,
                enum_to_str(&row.effect_class),
                enum_to_str(&row.source),
                row.refunded,
                row.created_ms as i64,
                row.intent_id,
            ],
        )?;
        Ok(())
    }

    fn attempt_from(row: &Row<'_>) -> Result<AttemptRow, StoreError> {
        Ok(AttemptRow {
            attempt_id: id(row.get(0)?, "attempt id")?,
            root_id: id(row.get(1)?, "root id")?,
            decision_id: id(row.get(2)?, "decision id")?,
            generation: row.get::<_, i64>(3)? as u64,
            state: str_to_enum(&row.get::<_, String>(4)?)?,
            skill: row.get(5)?,
            subgoal: row.get(6)?,
            quota: AttemptQuota {
                actions: row.get::<_, i64>(7)? as u32,
                observations: row.get::<_, i64>(8)? as u32,
            },
            activation_seq: row.get::<_, i64>(9)? as u64,
            root_epoch: row.get::<_, i64>(10)? as u64,
            task_revision: row.get::<_, i64>(11)? as u64,
            plan_revision: row.get::<_, i64>(12)? as u64,
            deadline_ms: row.get::<_, i64>(13)? as u64,
            effect_class: str_to_enum(&row.get::<_, String>(14)?)?,
            source: str_to_enum(&row.get::<_, String>(15)?)?,
            refunded: row.get(16)?,
            created_ms: row.get::<_, i64>(17)? as u64,
            intent_id: row.get(18)?,
        })
    }

    const ATTEMPT_COLUMNS: &'static str =
        "attempt_id, root_id, decision_id, generation, state, skill,
        subgoal, quota_actions, quota_observations, activation_seq, root_epoch, task_revision,
        plan_revision, deadline_ms, effect_class, source, refunded, created_ms, intent_id";

    pub fn load_attempt(
        conn: &Connection,
        attempt_id: &str,
    ) -> Result<Option<AttemptRow>, StoreError> {
        let sql = format!(
            "SELECT {} FROM attempts WHERE attempt_id = ?1",
            Self::ATTEMPT_COLUMNS
        );
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query([attempt_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::attempt_from(row)?)),
            None => Ok(None),
        }
    }

    pub fn load_attempts(conn: &Connection, root_id: &str) -> Result<Vec<AttemptRow>, StoreError> {
        let sql = format!(
            "SELECT {} FROM attempts WHERE root_id = ?1 ORDER BY activation_seq",
            Self::ATTEMPT_COLUMNS
        );
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query([root_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::attempt_from(row)?);
        }
        Ok(out)
    }

    pub fn set_attempt_state(
        tx: &Transaction<'_>,
        attempt_id: &str,
        state: AttemptState,
    ) -> Result<(), StoreError> {
        tx.execute(
            "UPDATE attempts SET state = ?2 WHERE attempt_id = ?1",
            params![attempt_id, enum_to_str(&state)],
        )?;
        Ok(())
    }

    pub fn set_attempt_intent(
        tx: &Transaction<'_>,
        attempt_id: &str,
        intent_id: &str,
    ) -> Result<(), StoreError> {
        tx.execute(
            "UPDATE attempts SET intent_id = ?2 WHERE attempt_id = ?1",
            params![attempt_id, intent_id],
        )?;
        Ok(())
    }

    /// Mark the attempt refunded. Returns false if it already was, which makes
    /// giving quota back idempotent.
    pub fn mark_refunded(tx: &Transaction<'_>, attempt_id: &str) -> Result<bool, StoreError> {
        let changed = tx.execute(
            "UPDATE attempts SET refunded = 1 WHERE attempt_id = ?1 AND refunded = 0",
            [attempt_id],
        )?;
        Ok(changed == 1)
    }
}
