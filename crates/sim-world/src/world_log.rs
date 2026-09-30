//! One world for processes that do not share memory.
//!
//! The simulated device applies inputs to the application; the source reports
//! the application. When they are separate processes they must still see the
//! same application, so both follow one append-only log of what happened to
//! it. Each keeps its own copy and replays the log in order, which is
//! deterministic, so the copies agree whenever they have read the same lines.
//! A test can append to the log too, to put a dialog in front of the window
//! the way a surprise would.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::primitive::Primitive;
use serde::{Deserialize, Serialize};

use crate::app::{Applied, Modal, SimApp, SnapshotMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModalKind {
    ResetConfirm,
    UnsavedChanges,
}

impl From<ModalKind> for Modal {
    fn from(kind: ModalKind) -> Self {
        match kind {
            ModalKind::ResetConfirm => Modal::ResetConfirm,
            ModalKind::UnsavedChanges => Modal::UnsavedChanges,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorldEntry {
    /// An input the device applied.
    Input {
        at_ms: u64,
        primitive: Primitive,
        binding_fingerprint: Option<String>,
    },
    /// A dialog appears in front of the window, or `None` dismisses it.
    Modal { modal: Option<ModalKind> },
    /// Leaving the Notifications page with unsaved changes asks first.
    SheetOnUnsavedNavigation { enabled: bool },
}

fn meta(at_ms: u64) -> SnapshotMeta {
    SnapshotMeta {
        snapshot_id: "world-log".into(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: 0,
        observed_ms: at_ms,
    }
}

/// Apply one entry to a copy of the application.
pub fn apply_entry(app: &mut SimApp, entry: &WorldEntry) {
    match entry {
        WorldEntry::Input {
            at_ms,
            primitive,
            binding_fingerprint,
        } => {
            app.apply(
                &meta(*at_ms),
                &Applied {
                    primitive: primitive.clone(),
                    binding_fingerprint: binding_fingerprint.clone(),
                },
            );
        }
        WorldEntry::Modal { modal: Some(kind) } => app.inject_modal((*kind).into()),
        WorldEntry::Modal { modal: None } => app.dismiss_modal(),
        WorldEntry::SheetOnUnsavedNavigation { enabled } => {
            app.sheet_on_unsaved_navigation = *enabled;
        }
    }
}

/// The writing side: one line per entry, appended.
#[derive(Debug, Clone)]
pub struct WorldLog {
    path: PathBuf,
}

impl WorldLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// One write per entry, so two writers never interleave inside a line.
    pub fn append(&self, entry: &WorldEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    /// Every entry so far, for a test that wants to know what happened.
    pub fn entries(&self) -> std::io::Result<Vec<WorldEntry>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

/// The reading side: a copy of the application that replays the log.
pub struct WorldFollower {
    path: PathBuf,
    offset: u64,
    pub app: SimApp,
    /// How many entries have been applied.
    pub applied: usize,
}

impl WorldFollower {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            app: SimApp::new(),
            applied: 0,
        }
    }

    /// Apply every complete line not yet seen. A line still being written is
    /// left for next time, so a half-written entry is never applied.
    pub fn catch_up(&mut self) -> std::io::Result<usize> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        file.seek(SeekFrom::Start(self.offset))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let mut taken = 0;
        let mut consumed = 0;
        for line in text.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            consumed += line.len();
            if let Ok(entry) = serde_json::from_str::<WorldEntry>(line.trim_end()) {
                apply_entry(&mut self.app, &entry);
                taken += 1;
            }
        }
        self.offset += consumed as u64;
        self.applied += taken;
        Ok(taken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::primitive::AxActionKind;

    fn toggle(app: &SimApp) -> WorldEntry {
        let proof = app
            .fingerprint_of(&meta(0), "sw-push-daily")
            .expect("the switch exists");
        WorldEntry::Input {
            at_ms: 5,
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "off".into(),
                },
            },
            binding_fingerprint: Some(proof),
        }
    }

    #[test]
    fn two_followers_of_one_log_end_in_the_same_world() {
        let dir = tempfile::tempdir().unwrap();
        let log = WorldLog::new(dir.path().join("world.log"));
        let mut device = WorldFollower::new(log.path());
        let mut source = WorldFollower::new(log.path());
        log.append(&toggle(&device.app)).unwrap();
        log.append(&WorldEntry::Modal {
            modal: Some(ModalKind::UnsavedChanges),
        })
        .unwrap();
        assert_eq!(device.catch_up().unwrap(), 2);
        assert_eq!(source.catch_up().unwrap(), 2);
        for follower in [&device, &source] {
            assert_eq!(follower.app.switch_on("Push", "Daily picks"), Some(false));
            assert_eq!(follower.app.modal(), Some(Modal::UnsavedChanges));
        }
        // Nothing new: nothing is applied twice.
        assert_eq!(device.catch_up().unwrap(), 0);
        log.append(&WorldEntry::Modal { modal: None }).unwrap();
        assert_eq!(device.catch_up().unwrap(), 1);
        assert_eq!(device.app.modal(), None);
        assert_eq!(log.entries().unwrap().len(), 3);
    }

    #[test]
    fn a_line_still_being_written_is_not_applied_until_it_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.log");
        let mut follower = WorldFollower::new(&path);
        let entry =
            serde_json::to_string(&WorldEntry::SheetOnUnsavedNavigation { enabled: true }).unwrap();
        // Half a line: no newline yet.
        std::fs::write(&path, &entry.as_bytes()[..entry.len() / 2]).unwrap();
        assert_eq!(follower.catch_up().unwrap(), 0);
        assert!(!follower.app.sheet_on_unsaved_navigation);
        // The rest arrives.
        std::fs::write(&path, format!("{entry}\n")).unwrap();
        assert_eq!(follower.catch_up().unwrap(), 1);
        assert!(follower.app.sheet_on_unsaved_navigation);
    }

    #[test]
    fn a_missing_log_is_an_empty_world_and_damaged_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.log");
        let mut follower = WorldFollower::new(&path);
        assert_eq!(follower.catch_up().unwrap(), 0);
        std::fs::write(&path, "not json\n").unwrap();
        assert_eq!(follower.catch_up().unwrap(), 0);
        assert_eq!(WorldLog::new(&path).entries().unwrap().len(), 0);
    }
}
