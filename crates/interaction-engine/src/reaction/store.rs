//! Reaction's tables: decisions, attempt_map and admission_outbox.
//! Nothing outside this module writes them.

use interaction_contracts::desktop::ids::{AttemptId, DecisionId, RootTaskId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::infrastructure::storage::{StoreError, enum_to_str, str_to_enum};
use crate::ports::{ConsumeOutcome, ConsumeRequest, DecisionLedger, DecisionLifecycle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    pub decision_id: DecisionId,
    pub root_id: String,
    pub generation: u64,
    pub state: DecisionLifecycle,
    pub consumed_attempt: Option<String>,
}

pub struct ReactionStore;

impl ReactionStore {
    /// Record a new pending decision. The same id cannot be opened twice.
    pub fn open_decision(
        tx: &Transaction<'_>,
        decision: &DecisionId,
        root: &RootTaskId,
        generation: u64,
        created_ms: u64,
    ) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO decisions (decision_id, root_id, generation, state, created_ms)
             VALUES (?1, ?2, ?3, 'pending', ?4)",
            params![
                decision.as_str(),
                root.as_str(),
                generation as i64,
                created_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Replace a pending request with a newer generation so the old answer can
    /// no longer be admitted. A consumed decision can never be superseded.
    pub fn supersede(
        tx: &Transaction<'_>,
        decision: &DecisionId,
        new_generation: u64,
    ) -> Result<bool, StoreError> {
        let changed = tx.execute(
            "UPDATE decisions SET generation = ?2
             WHERE decision_id = ?1 AND state = 'pending' AND generation < ?2",
            params![decision.as_str(), new_generation as i64],
        )?;
        Ok(changed == 1)
    }

    /// Close a pending decision whose answer will never be used.
    pub fn close(
        tx: &Transaction<'_>,
        decision: &DecisionId,
        to: DecisionLifecycle,
    ) -> Result<bool, StoreError> {
        debug_assert!(matches!(
            to,
            DecisionLifecycle::Superseded | DecisionLifecycle::Expired
        ));
        let changed = tx.execute(
            "UPDATE decisions SET state = ?2 WHERE decision_id = ?1 AND state = 'pending'",
            params![decision.as_str(), enum_to_str(&to)],
        )?;
        Ok(changed == 1)
    }

    pub fn load(conn: &Connection, decision: &str) -> Result<Option<DecisionRow>, StoreError> {
        let row = conn
            .query_row(
                "SELECT decision_id, root_id, generation, state, consumed_attempt
                 FROM decisions WHERE decision_id = ?1",
                [decision],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, root_id, generation, state, consumed_attempt)| {
            Ok(DecisionRow {
                decision_id: DecisionId::new(id)
                    .map_err(|_| StoreError::Corrupt("invalid decision id".into()))?,
                root_id,
                generation: generation as u64,
                state: str_to_enum(&state)?,
                consumed_attempt,
            })
        })
        .transpose()
    }

    /// The one attempt this decision was consumed by, if any.
    pub fn mapping(
        conn: &Connection,
        decision: &str,
    ) -> Result<Option<(u64, AttemptId)>, StoreError> {
        let row = conn
            .query_row(
                "SELECT generation, attempt_id FROM attempt_map WHERE decision_id = ?1",
                [decision],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(generation, attempt)| {
            Ok((
                generation as u64,
                AttemptId::new(attempt)
                    .map_err(|_| StoreError::Corrupt("invalid attempt id".into()))?,
            ))
        })
        .transpose()
    }

    pub fn count_mappings(conn: &Connection) -> Result<u64, StoreError> {
        Ok(
            conn.query_row("SELECT COUNT(*) FROM attempt_map", [], |row| {
                row.get::<_, i64>(0)
            })? as u64,
        )
    }

    /// Admission records that were committed but whose delivery is not confirmed.
    /// They are reconciled against the device, never blindly replayed.
    pub fn undelivered(conn: &Connection) -> Result<Vec<(AttemptId, String)>, StoreError> {
        let mut statement = conn.prepare(
            "SELECT attempt_id, payload FROM admission_outbox WHERE delivered = 0 ORDER BY rowid",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let attempt: String = row.get(0)?;
            out.push((
                AttemptId::new(attempt)
                    .map_err(|_| StoreError::Corrupt("invalid attempt id".into()))?,
                row.get(1)?,
            ));
        }
        Ok(out)
    }

    pub fn mark_delivered(tx: &Transaction<'_>, attempt: &AttemptId) -> Result<(), StoreError> {
        tx.execute(
            "UPDATE admission_outbox SET delivered = 1 WHERE attempt_id = ?1",
            [attempt.as_str()],
        )?;
        Ok(())
    }
}

impl DecisionLedger for ReactionStore {
    fn consume_for_admission(
        &self,
        tx: &Transaction<'_>,
        request: &ConsumeRequest<'_>,
    ) -> Result<ConsumeOutcome, StoreError> {
        let Some(row) = Self::load(tx, request.decision_id.as_str())? else {
            return Ok(ConsumeOutcome::Unknown);
        };
        if row.state != DecisionLifecycle::Pending {
            return Ok(ConsumeOutcome::NotPending(row.state));
        }
        if row.generation != request.generation {
            return Ok(ConsumeOutcome::GenerationMismatch {
                current: row.generation,
            });
        }
        // The compare-and-swap: only a still-pending row of this generation moves.
        let changed = tx.execute(
            "UPDATE decisions SET state = 'consumed', consumed_attempt = ?3, source = ?4
             WHERE decision_id = ?1 AND state = 'pending' AND generation = ?2",
            params![
                request.decision_id.as_str(),
                request.generation as i64,
                request.attempt_id.as_str(),
                enum_to_str(&request.source),
            ],
        )?;
        if changed != 1 {
            return Ok(ConsumeOutcome::NotPending(DecisionLifecycle::Consumed));
        }
        tx.execute(
            "INSERT INTO attempt_map (decision_id, generation, attempt_id) VALUES (?1, ?2, ?3)",
            params![
                request.decision_id.as_str(),
                request.generation as i64,
                request.attempt_id.as_str()
            ],
        )?;
        tx.execute(
            "INSERT INTO admission_outbox (attempt_id, payload) VALUES (?1, ?2)",
            params![request.attempt_id.as_str(), request.outbox_payload],
        )?;
        Ok(ConsumeOutcome::Consumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::storage::Storage;
    use interaction_contracts::desktop::decision::DecisionSource;

    fn ids() -> (DecisionId, RootTaskId, AttemptId) {
        (
            DecisionId::new("d-1").unwrap(),
            RootTaskId::new("root-1").unwrap(),
            AttemptId::new("a-1").unwrap(),
        )
    }

    fn request<'a>(
        d: &'a DecisionId,
        r: &'a RootTaskId,
        a: &'a AttemptId,
        generation: u64,
    ) -> ConsumeRequest<'a> {
        ConsumeRequest {
            decision_id: d,
            generation,
            attempt_id: a,
            root_id: r,
            source: DecisionSource::Rule,
            outbox_payload: "{}",
        }
    }

    fn consume(storage: &mut Storage, generation: u64, attempt: &str) -> ConsumeOutcome {
        let (d, r, _) = ids();
        let a = AttemptId::new(attempt).unwrap();
        storage
            .transaction(|tx| {
                ReactionStore.consume_for_admission(tx, &request(&d, &r, &a, generation))
            })
            .unwrap()
    }

    fn open(storage: &mut Storage) {
        let (d, r, _) = ids();
        storage
            .transaction(|tx| ReactionStore::open_decision(tx, &d, &r, 1, 10))
            .unwrap();
    }

    #[test]
    fn a_pending_decision_is_consumed_once_with_one_mapping_and_one_outbox_entry() {
        let mut storage = Storage::open_in_memory().unwrap();
        open(&mut storage);
        assert_eq!(consume(&mut storage, 1, "a-1"), ConsumeOutcome::Consumed);
        let (generation, attempt) = ReactionStore::mapping(storage.read(), "d-1")
            .unwrap()
            .unwrap();
        assert_eq!((generation, attempt.as_str()), (1, "a-1"));
        assert_eq!(ReactionStore::undelivered(storage.read()).unwrap().len(), 1);
        let row = ReactionStore::load(storage.read(), "d-1").unwrap().unwrap();
        assert_eq!(row.state, DecisionLifecycle::Consumed);
        assert_eq!(row.consumed_attempt.as_deref(), Some("a-1"));
    }

    #[test]
    fn a_consumed_decision_cannot_be_consumed_again_even_by_a_new_attempt_id() {
        let mut storage = Storage::open_in_memory().unwrap();
        open(&mut storage);
        consume(&mut storage, 1, "a-1");
        assert_eq!(
            consume(&mut storage, 1, "a-2"),
            ConsumeOutcome::NotPending(DecisionLifecycle::Consumed)
        );
        assert_eq!(ReactionStore::count_mappings(storage.read()).unwrap(), 1);
    }

    #[test]
    fn an_old_generation_cannot_be_admitted_after_a_replacement() {
        let mut storage = Storage::open_in_memory().unwrap();
        open(&mut storage);
        let (d, _, _) = ids();
        assert!(
            storage
                .transaction(|tx| ReactionStore::supersede(tx, &d, 2))
                .unwrap()
        );
        assert_eq!(
            consume(&mut storage, 1, "a-1"),
            ConsumeOutcome::GenerationMismatch { current: 2 }
        );
        assert_eq!(consume(&mut storage, 2, "a-1"), ConsumeOutcome::Consumed);
    }

    #[test]
    fn a_consumed_decision_cannot_be_superseded_into_a_second_run() {
        let mut storage = Storage::open_in_memory().unwrap();
        open(&mut storage);
        consume(&mut storage, 1, "a-1");
        let (d, _, _) = ids();
        assert!(
            !storage
                .transaction(|tx| ReactionStore::supersede(tx, &d, 2))
                .unwrap()
        );
        assert_eq!(
            consume(&mut storage, 2, "a-2"),
            ConsumeOutcome::NotPending(DecisionLifecycle::Consumed)
        );
    }

    #[test]
    fn expired_and_unknown_decisions_are_not_consumable() {
        let mut storage = Storage::open_in_memory().unwrap();
        open(&mut storage);
        let (d, _, _) = ids();
        storage
            .transaction(|tx| ReactionStore::close(tx, &d, DecisionLifecycle::Expired))
            .unwrap();
        assert_eq!(
            consume(&mut storage, 1, "a-1"),
            ConsumeOutcome::NotPending(DecisionLifecycle::Expired)
        );
        let (_, r, a) = ids();
        let ghost = DecisionId::new("ghost").unwrap();
        let outcome = storage
            .transaction(|tx| ReactionStore.consume_for_admission(tx, &request(&ghost, &r, &a, 1)))
            .unwrap();
        assert_eq!(outcome, ConsumeOutcome::Unknown);
    }

    #[test]
    fn the_same_attempt_id_cannot_be_mapped_to_two_decisions() {
        let mut storage = Storage::open_in_memory().unwrap();
        let (_, r, _) = ids();
        let second = DecisionId::new("d-2").unwrap();
        open(&mut storage);
        storage
            .transaction(|tx| ReactionStore::open_decision(tx, &second, &r, 1, 10))
            .unwrap();
        consume(&mut storage, 1, "shared");
        let attempt = AttemptId::new("shared").unwrap();
        let result = storage.transaction(|tx| {
            ReactionStore.consume_for_admission(tx, &request(&second, &r, &attempt, 1))
        });
        assert!(result.is_err(), "the unique mapping refuses it");
        let row = ReactionStore::load(storage.read(), "d-2").unwrap().unwrap();
        assert_eq!(
            row.state,
            DecisionLifecycle::Pending,
            "and the failed transaction left d-2 pending"
        );
    }
}
