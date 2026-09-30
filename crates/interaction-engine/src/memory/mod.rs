//! Memory: published knowledge, pinned per task, with an independent
//! quarantine (design 16.4).
//!
//! A root reads the release that was current when it started and keeps it, so
//! publishing never changes a running task. A quarantine is the exception: it
//! takes effect at once, for pinned roots too, because knowledge found to be
//! wrong must not be used. Rolling back only changes what new roots will pin.

use interaction_contracts::desktop::ids::{ReleaseId, RootTaskId};
use rusqlite::{OptionalExtension, params};

use crate::infrastructure::storage::{Storage, StoreError};

const DEFAULT_KEY: &str = "memory.default_release";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub release_id: ReleaseId,
    /// Exact revisions, never floating names.
    pub revisions: Vec<String>,
}

/// What a root may use: its pinned release minus anything quarantined since.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KnowledgeRefs {
    pub release: Option<ReleaseId>,
    pub revisions: Vec<String>,
    pub quarantined: Vec<String>,
    /// Increases with every quarantine, so a consumer can tell it changed.
    pub quarantine_sequence: u64,
}

pub struct Memory;

impl Memory {
    /// Publish a release atomically: either it and all its members exist, or
    /// nothing of it is visible. A release id is never reused.
    pub fn publish(storage: &mut Storage, manifest: &ReleaseManifest) -> Result<u64, StoreError> {
        storage.transaction(|tx| {
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(published_seq), 0) + 1 FROM releases",
                [],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO releases (release_id, published_seq, manifest) VALUES (?1, ?2, ?3)",
                params![
                    manifest.release_id.as_str(),
                    seq,
                    serde_json::to_string(&manifest.revisions)?
                ],
            )?;
            for revision in &manifest.revisions {
                tx.execute(
                    "INSERT INTO release_members (release_id, revision_id) VALUES (?1, ?2)",
                    params![manifest.release_id.as_str(), revision],
                )?;
            }
            Ok(seq as u64)
        })
    }

    /// Point new roots at an older release, or back at the newest with `None`.
    pub fn set_default(
        storage: &mut Storage,
        release: Option<&ReleaseId>,
    ) -> Result<(), StoreError> {
        match release {
            Some(release) => {
                let exists: bool = storage.read().query_row(
                    "SELECT COUNT(*) > 0 FROM releases WHERE release_id = ?1",
                    [release.as_str()],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(StoreError::Conflict(format!(
                        "release {release} does not exist"
                    )));
                }
                storage.set_meta(DEFAULT_KEY, release.as_str())
            }
            None => storage.set_meta(DEFAULT_KEY, ""),
        }
    }

    /// What a root that starts now would pin.
    pub fn default_release(storage: &Storage) -> Result<Option<ReleaseId>, StoreError> {
        if let Some(chosen) = storage.meta(DEFAULT_KEY)?.filter(|value| !value.is_empty()) {
            return Ok(ReleaseId::new(chosen).ok());
        }
        let newest: Option<String> = storage
            .read()
            .query_row(
                "SELECT release_id FROM releases ORDER BY published_seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(newest.and_then(|id| ReleaseId::new(id).ok()))
    }

    /// Fix the release a root reads. Pinning again returns the same release.
    pub fn pin(storage: &mut Storage, root: &RootTaskId) -> Result<Option<ReleaseId>, StoreError> {
        if let Some(existing) = Self::pinned(storage, root)? {
            return Ok(Some(existing));
        }
        let Some(release) = Self::default_release(storage)? else {
            return Ok(None);
        };
        storage.transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO root_release_pins (root_id, release_id) VALUES (?1, ?2)",
                params![root.as_str(), release.as_str()],
            )?;
            Ok(())
        })?;
        Self::pinned(storage, root)
    }

    pub fn pinned(storage: &Storage, root: &RootTaskId) -> Result<Option<ReleaseId>, StoreError> {
        let pinned: Option<String> = storage
            .read()
            .query_row(
                "SELECT release_id FROM root_release_pins WHERE root_id = ?1",
                [root.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(pinned.and_then(|id| ReleaseId::new(id).ok()))
    }

    /// Withdraw one revision everywhere, at once.
    pub fn quarantine(
        storage: &mut Storage,
        revision: &str,
        reason: &str,
    ) -> Result<u64, StoreError> {
        storage.transaction(|tx| {
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM quarantine",
                [],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO quarantine (revision_id, reason, seq) VALUES (?1, ?2, ?3)",
                params![revision, reason, seq],
            )?;
            Ok(seq as u64)
        })
    }

    pub fn is_quarantined(storage: &Storage, revision: &str) -> Result<bool, StoreError> {
        Ok(storage.read().query_row(
            "SELECT COUNT(*) > 0 FROM quarantine WHERE revision_id = ?1",
            [revision],
            |row| row.get(0),
        )?)
    }

    pub fn refs_for(storage: &Storage, root: &RootTaskId) -> Result<KnowledgeRefs, StoreError> {
        let quarantine_sequence: i64 = storage.read().query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM quarantine",
            [],
            |row| row.get(0),
        )?;
        let Some(release) = Self::pinned(storage, root)? else {
            return Ok(KnowledgeRefs {
                quarantine_sequence: quarantine_sequence as u64,
                ..KnowledgeRefs::default()
            });
        };
        let mut statement = storage.read().prepare(
            "SELECT m.revision_id, q.revision_id IS NOT NULL
             FROM release_members m LEFT JOIN quarantine q ON q.revision_id = m.revision_id
             WHERE m.release_id = ?1 ORDER BY m.revision_id",
        )?;
        let mut rows = statement.query([release.as_str()])?;
        let (mut usable, mut withdrawn) = (Vec::new(), Vec::new());
        while let Some(row) = rows.next()? {
            let revision: String = row.get(0)?;
            if row.get::<_, bool>(1)? {
                withdrawn.push(revision);
            } else {
                usable.push(revision);
            }
        }
        Ok(KnowledgeRefs {
            release: Some(release),
            revisions: usable,
            quarantined: withdrawn,
            quarantine_sequence: quarantine_sequence as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(id: &str, revisions: &[&str]) -> ReleaseManifest {
        ReleaseManifest {
            release_id: ReleaseId::new(id).unwrap(),
            revisions: revisions.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    fn root(id: &str) -> RootTaskId {
        RootTaskId::new(id).unwrap()
    }

    #[test]
    fn a_failed_publish_leaves_nothing_visible() {
        let mut storage = Storage::open_in_memory().unwrap();
        Memory::publish(&mut storage, &release("r1", &["a@1"])).unwrap();
        // A duplicate member fails halfway through the transaction.
        assert!(Memory::publish(&mut storage, &release("r2", &["b@1", "b@1"])).is_err());
        let releases: i64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM releases", [], |r| r.get(0))
            .unwrap();
        let members: i64 = storage
            .read()
            .query_row("SELECT COUNT(*) FROM release_members", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            (releases, members),
            (1, 1),
            "the half-built release is not there"
        );
        assert_eq!(
            Memory::default_release(&storage).unwrap().unwrap().as_str(),
            "r1"
        );
        // And an id is never reused.
        assert!(Memory::publish(&mut storage, &release("r1", &["c@1"])).is_err());
    }

    #[test]
    fn a_root_keeps_the_release_it_started_with() {
        let mut storage = Storage::open_in_memory().unwrap();
        Memory::publish(&mut storage, &release("r1", &["a@1", "b@1"])).unwrap();
        assert_eq!(
            Memory::pin(&mut storage, &root("root-1"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r1"
        );
        Memory::publish(&mut storage, &release("r2", &["a@2"])).unwrap();

        assert_eq!(
            Memory::pin(&mut storage, &root("root-1"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r1",
            "pinning again changes nothing"
        );
        assert_eq!(
            Memory::refs_for(&storage, &root("root-1"))
                .unwrap()
                .revisions,
            vec!["a@1", "b@1"]
        );
        assert_eq!(
            Memory::pin(&mut storage, &root("root-2"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r2"
        );
        assert_eq!(
            Memory::refs_for(&storage, &root("root-2"))
                .unwrap()
                .revisions,
            vec!["a@2"]
        );
    }

    #[test]
    fn a_quarantine_takes_effect_at_once_even_for_a_pinned_root() {
        let mut storage = Storage::open_in_memory().unwrap();
        Memory::publish(&mut storage, &release("r1", &["a@1", "b@1"])).unwrap();
        Memory::pin(&mut storage, &root("root-1")).unwrap();
        let before = Memory::refs_for(&storage, &root("root-1")).unwrap();
        assert_eq!(before.quarantine_sequence, 0);

        let seq = Memory::quarantine(&mut storage, "b@1", "wrong label anchor").unwrap();
        assert_eq!(seq, 1);
        let after = Memory::refs_for(&storage, &root("root-1")).unwrap();
        assert_eq!(after.revisions, vec!["a@1"]);
        assert_eq!(after.quarantined, vec!["b@1"]);
        assert_eq!(after.quarantine_sequence, 1);
        assert!(Memory::is_quarantined(&storage, "b@1").unwrap());
        assert!(!Memory::is_quarantined(&storage, "a@1").unwrap());
    }

    #[test]
    fn rolling_back_changes_only_what_new_roots_pin() {
        let mut storage = Storage::open_in_memory().unwrap();
        Memory::publish(&mut storage, &release("r1", &["a@1"])).unwrap();
        Memory::publish(&mut storage, &release("r2", &["a@2"])).unwrap();
        Memory::pin(&mut storage, &root("old")).unwrap();
        assert_eq!(
            Memory::pinned(&storage, &root("old"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r2"
        );

        Memory::set_default(&mut storage, Some(&ReleaseId::new("r1").unwrap())).unwrap();
        assert_eq!(
            Memory::pinned(&storage, &root("old"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r2",
            "the running root is untouched"
        );
        assert_eq!(
            Memory::pin(&mut storage, &root("new"))
                .unwrap()
                .unwrap()
                .as_str(),
            "r1"
        );
        assert!(Memory::set_default(&mut storage, Some(&ReleaseId::new("nope").unwrap())).is_err());

        Memory::set_default(&mut storage, None).unwrap();
        assert_eq!(
            Memory::default_release(&storage).unwrap().unwrap().as_str(),
            "r2"
        );
    }

    #[test]
    fn a_root_with_no_release_has_no_knowledge_and_that_is_fine() {
        let mut storage = Storage::open_in_memory().unwrap();
        assert_eq!(Memory::pin(&mut storage, &root("root-1")).unwrap(), None);
        assert_eq!(
            Memory::refs_for(&storage, &root("root-1")).unwrap(),
            KnowledgeRefs::default()
        );
    }
}
