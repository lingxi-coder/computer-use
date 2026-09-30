//! The host's single SQLite database (design 16.5 and 16.6).
//!
//! One connection, WAL, FULL synchronous commits and one writer. The modules
//! own their tables and write them through their own stores; this type only
//! hands out transactions.

use std::fmt;
use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::schema::MIGRATIONS;

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    /// The write would violate an invariant the store enforces.
    Conflict(String),
    /// A fault injected by a test at a named point.
    Fault(&'static str),
    /// Stored data that cannot be read back.
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(f, "sqlite: {error}"),
            Self::Json(error) => write!(f, "json: {error}"),
            Self::Conflict(reason) => write!(f, "conflict: {reason}"),
            Self::Fault(point) => write!(f, "injected fault at {point}"),
            Self::Corrupt(reason) => write!(f, "corrupt data: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 2000;",
        )?;
        let applied: usize = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if applied > MIGRATIONS.len() {
            return Err(StoreError::Corrupt(format!(
                "database schema {applied} is newer than this build ({})",
                MIGRATIONS.len()
            )));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(migration)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", index + 1))?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Run `f` in one write transaction. An error rolls everything back, so a
    /// failed admission leaves no partial record behind.
    pub fn transaction<T, E>(
        &mut self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Read-only access for queries outside a write.
    pub fn read(&self) -> &Connection {
        &self.conn
    }

    /// Append to the trace. Losing a trace entry never affects correctness.
    pub fn log(&mut self, at_ms: u64, root: Option<&str>, kind: &str, detail: &str) {
        let _ = self.conn.execute(
            "INSERT INTO event_log (at_ms, root_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![at_ms as i64, root, kind, detail],
        );
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let mut statement = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = statement.query([key])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }
}

/// A unit enum value as the plain string it serialises to.
pub fn enum_to_str<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        other => panic!("not a string-valued enum: {other:?}"),
    }
}

pub fn str_to_enum<T: DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(text.to_string()))
        .map_err(|error| StoreError::Corrupt(format!("unknown value {text:?}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::task::RootState;

    #[test]
    fn a_new_database_is_migrated_to_the_current_schema() {
        let storage = Storage::open_in_memory().unwrap();
        let version: usize = storage
            .read()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len());
        let tables: usize = storage
            .read()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(tables >= 12, "expected the ledgers, found {tables} tables");
    }

    #[test]
    fn an_error_inside_a_transaction_rolls_everything_back() {
        let mut storage = Storage::open_in_memory().unwrap();
        let result: Result<(), StoreError> = storage.transaction(|tx| {
            tx.execute("INSERT INTO meta (key, value) VALUES ('a', '1')", [])?;
            tx.execute("INSERT INTO meta (key, value) VALUES ('b', '2')", [])?;
            Err(StoreError::Fault("midway"))
        });
        assert!(matches!(result, Err(StoreError::Fault("midway"))));
        assert_eq!(
            storage.meta("a").unwrap(),
            None,
            "nothing half-written survives"
        );
        assert_eq!(storage.meta("b").unwrap(), None);
    }

    #[test]
    fn committed_data_survives_reopening_a_file_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.db");
        {
            let mut storage = Storage::open(&path).unwrap();
            storage.set_meta("k", "v").unwrap();
        }
        let reopened = Storage::open(&path).unwrap();
        assert_eq!(reopened.meta("k").unwrap().as_deref(), Some("v"));
        let mode: String = reopened
            .read()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = reopened
            .read()
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 2, "FULL");
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len() + 1))
                .unwrap();
        }
        assert!(matches!(Storage::open(&path), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn enums_round_trip_through_their_wire_names() {
        assert_eq!(
            enum_to_str(&RootState::AwaitingConfirmation),
            "awaiting_confirmation"
        );
        let parsed: RootState = str_to_enum("needs_revalidation").unwrap();
        assert_eq!(parsed, RootState::NeedsRevalidation);
        assert!(str_to_enum::<RootState>("nonsense").is_err());
    }
}
