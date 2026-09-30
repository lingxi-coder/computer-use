//! StateCore: the single reducer that admits observations (design 6.3).
//!
//! A snapshot is accepted only when its session, order and time make sense.
//! Anything stale or out of order is refused and never overwrites what is
//! current. Evidence age is always computed from the original observation
//! time, so re-sending or re-reading a snapshot can never make it fresher.

use std::collections::{BTreeMap, VecDeque};

use interaction_contracts::desktop::source::{
    SourceHealth, SourceKind, SourceState, StructureSnapshot,
};

use super::pack::AppPack;

const HISTORY: usize = 32;

/// One admitted snapshot together with the revisions it established.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvSnapshot {
    /// Increases with every admitted snapshot.
    pub revision: u64,
    /// Increases when the active layer or the set of windows changes.
    pub layer_revision: u64,
    pub snapshot: StructureSnapshot,
    /// The page the main window shows, when the pack can tell.
    pub page: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    SessionMismatch { expected: u64, got: u64 },
    OutOfOrder { last: u64, got: u64 },
    FromTheFuture { observed_ms: u64, now_ms: u64 },
    Invalid(String),
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionMismatch { expected, got } => {
                write!(f, "snapshot is from session {got}, expected {expected}")
            }
            Self::OutOfOrder { last, got } => {
                write!(f, "snapshot sequence {got} does not follow {last}")
            }
            Self::FromTheFuture {
                observed_ms,
                now_ms,
            } => {
                write!(f, "snapshot claims time {observed_ms}, now is {now_ms}")
            }
            Self::Invalid(reason) => write!(f, "snapshot is invalid: {reason}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    pub revision: u64,
    pub layer_changed: bool,
    pub page_changed: bool,
    /// The read was cut short, so objects it lacks are unknown, not absent.
    pub incomplete: bool,
}

#[derive(Debug, Default)]
pub struct StateCore {
    expected_session: Option<u64>,
    last_sequence: Option<u64>,
    revision: u64,
    layer_revision: u64,
    latest: Option<EnvSnapshot>,
    history: VecDeque<EnvSnapshot>,
    health: BTreeMap<String, SourceHealth>,
}

fn health_key(kind: SourceKind) -> String {
    format!("{kind:?}")
}

impl StateCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new source session began. Evidence from the old one proves nothing
    /// about the new one, so it is dropped.
    pub fn set_session(&mut self, epoch: u64) {
        if self.expected_session != Some(epoch) {
            self.expected_session = Some(epoch);
            self.last_sequence = None;
            self.latest = None;
            self.history.clear();
        }
    }

    pub fn admit(
        &mut self,
        snapshot: StructureSnapshot,
        now_ms: u64,
        pack: &AppPack,
    ) -> Result<Admitted, Reject> {
        snapshot.validate().map_err(Reject::Invalid)?;
        if let Some(expected) = self.expected_session
            && snapshot.session_epoch != expected
        {
            return Err(Reject::SessionMismatch {
                expected,
                got: snapshot.session_epoch,
            });
        }
        if let Some(last) = self.last_sequence
            && snapshot.source_sequence <= last
        {
            return Err(Reject::OutOfOrder {
                last,
                got: snapshot.source_sequence,
            });
        }
        if snapshot.observed_ms > now_ms {
            return Err(Reject::FromTheFuture {
                observed_ms: snapshot.observed_ms,
                now_ms,
            });
        }
        let windows: Vec<&str> = snapshot
            .windows
            .iter()
            .map(|w| w.window_id.as_str())
            .collect();
        let layer_changed = match &self.latest {
            Some(previous) => {
                let before: Vec<&str> = previous
                    .snapshot
                    .windows
                    .iter()
                    .map(|w| w.window_id.as_str())
                    .collect();
                previous.snapshot.active_layer != snapshot.active_layer || before != windows
            }
            None => true,
        };
        let page = pack.page_of(&snapshot);
        let page_changed = self
            .latest
            .as_ref()
            .is_some_and(|previous| previous.page != page);
        self.last_sequence = Some(snapshot.source_sequence);
        self.revision += 1;
        if layer_changed {
            self.layer_revision += 1;
        }
        let incomplete = !snapshot.complete;
        let admitted = EnvSnapshot {
            revision: self.revision,
            layer_revision: self.layer_revision,
            snapshot,
            page,
        };
        if let Some(previous) = self.latest.replace(admitted) {
            self.history.push_back(previous);
            while self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        Ok(Admitted {
            revision: self.revision,
            layer_changed,
            page_changed,
            incomplete,
        })
    }

    pub fn latest(&self) -> Option<&EnvSnapshot> {
        self.latest.as_ref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every retained snapshot, oldest first, ending with the latest.
    pub fn retained(&self) -> impl Iterator<Item = &EnvSnapshot> {
        self.history.iter().chain(self.latest.iter())
    }

    /// The newest snapshot observed at or after `floor_ms`.
    pub fn latest_since(&self, floor_ms: u64) -> Option<&EnvSnapshot> {
        self.latest
            .as_ref()
            .filter(|env| env.snapshot.observed_ms >= floor_ms)
    }

    /// How old the latest evidence is, from when it was actually observed.
    pub fn age_ms(&self, now_ms: u64) -> Option<u64> {
        self.latest
            .as_ref()
            .map(|env| now_ms.saturating_sub(env.snapshot.observed_ms))
    }

    pub fn note_health(&mut self, health: SourceHealth) {
        self.health.insert(health_key(health.source), health);
    }

    pub fn health(&self, kind: SourceKind) -> Option<&SourceHealth> {
        self.health.get(&health_key(kind))
    }

    /// The structure source cannot be relied on right now.
    pub fn structure_unavailable(&self) -> bool {
        self.health(SourceKind::Ax)
            .is_some_and(|h| h.state == SourceState::Unavailable)
    }

    /// Fresh enough, complete, and from a source that works.
    pub fn usable(&self, now_ms: u64, max_age_ms: u64) -> bool {
        !self.structure_unavailable()
            && self.latest.as_ref().is_some_and(|env| {
                env.snapshot.complete
                    && env.snapshot.active_layer.is_some()
                    && now_ms.saturating_sub(env.snapshot.observed_ms) <= max_age_ms
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::device::SurfaceRef;
    use sim_world::app::{Modal, SimApp, SnapshotMeta};

    fn meta(sequence: u64, observed_ms: u64) -> SnapshotMeta {
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

    fn core() -> StateCore {
        let mut core = StateCore::new();
        core.set_session(1);
        core
    }

    #[test]
    fn a_first_snapshot_establishes_the_revisions() {
        let mut core = core();
        let admitted = core
            .admit(
                SimApp::new().snapshot(&meta(1, 10)),
                10,
                &AppPack::settings_fixture(),
            )
            .unwrap();
        assert_eq!(
            admitted,
            Admitted {
                revision: 1,
                layer_changed: true,
                page_changed: false,
                incomplete: false
            }
        );
        assert_eq!(
            core.latest().unwrap().page.as_deref(),
            Some("Notifications")
        );
    }

    #[test]
    fn order_session_and_time_are_enforced() {
        let pack = AppPack::settings_fixture();
        let app = SimApp::new();
        let mut core = core();
        core.admit(app.snapshot(&meta(5, 10)), 10, &pack).unwrap();
        assert!(matches!(
            core.admit(app.snapshot(&meta(5, 11)), 11, &pack),
            Err(Reject::OutOfOrder { .. })
        ));
        assert!(matches!(
            core.admit(app.snapshot(&meta(4, 11)), 11, &pack),
            Err(Reject::OutOfOrder { .. })
        ));
        assert!(matches!(
            core.admit(app.snapshot(&meta(6, 500)), 11, &pack),
            Err(Reject::FromTheFuture { .. })
        ));
        let mut other_session = app.snapshot(&meta(7, 12));
        other_session.session_epoch = 2;
        assert!(matches!(
            core.admit(other_session, 12, &pack),
            Err(Reject::SessionMismatch { .. })
        ));
        assert_eq!(core.revision(), 1, "none of the refused snapshots counted");
        assert_eq!(core.latest().unwrap().snapshot.source_sequence, 5);
    }

    #[test]
    fn a_layer_change_and_a_page_change_are_reported_separately() {
        let pack = AppPack::settings_fixture();
        let mut app = SimApp::new();
        let mut core = core();
        core.admit(app.snapshot(&meta(1, 10)), 10, &pack).unwrap();
        let plain = core.admit(app.snapshot(&meta(2, 20)), 20, &pack).unwrap();
        assert!(
            !plain.layer_changed && !plain.page_changed,
            "an identical read changes nothing"
        );

        app.inject_modal(Modal::UnsavedChanges);
        let modal = core.admit(app.snapshot(&meta(3, 30)), 30, &pack).unwrap();
        assert!(modal.layer_changed);
        assert_eq!(core.latest().unwrap().layer_revision, 2);
        app.dismiss_modal();
        let back = core.admit(app.snapshot(&meta(4, 40)), 40, &pack).unwrap();
        assert!(back.layer_changed, "the sheet going away is a change too");
        assert_eq!(core.latest().unwrap().layer_revision, 3);

        let tab = app.fingerprint_of(&meta(5, 50), "tab-general").unwrap();
        app.apply(
            &meta(5, 50),
            &sim_world::app::Applied {
                primitive: interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                },
                binding_fingerprint: Some(tab),
            },
        );
        let page = core.admit(app.snapshot(&meta(6, 60)), 60, &pack).unwrap();
        assert!(page.page_changed);
        assert_eq!(core.latest().unwrap().page.as_deref(), Some("General"));
    }

    #[test]
    fn age_comes_from_the_observation_time_and_a_repeat_does_not_refresh_it() {
        let pack = AppPack::settings_fixture();
        let app = SimApp::new();
        let mut core = core();
        core.admit(app.snapshot(&meta(1, 100)), 100, &pack).unwrap();
        assert_eq!(core.age_ms(350), Some(250));
        // The same content re-sent later is refused as out of order, so it
        // cannot make the old evidence look new.
        assert!(core.admit(app.snapshot(&meta(1, 340)), 340, &pack).is_err());
        assert_eq!(core.age_ms(350), Some(250));
        assert!(core.usable(350, 300));
        assert!(!core.usable(450, 300));
    }

    #[test]
    fn an_incomplete_read_is_admitted_but_never_usable_as_evidence() {
        let pack = AppPack::settings_fixture();
        let mut core = core();
        let admitted = core
            .admit(SimApp::new().partial_snapshot(&meta(1, 10), 3), 10, &pack)
            .unwrap();
        assert!(admitted.incomplete);
        assert!(!core.usable(10, 1_000));
    }

    #[test]
    fn a_new_session_drops_the_old_evidence() {
        let pack = AppPack::settings_fixture();
        let mut core = core();
        core.admit(SimApp::new().snapshot(&meta(1, 10)), 10, &pack)
            .unwrap();
        core.set_session(2);
        assert!(core.latest().is_none());
        assert_eq!(core.retained().count(), 0);
        let mut fresh = SimApp::new().snapshot(&meta(1, 20));
        fresh.session_epoch = 2;
        assert!(
            core.admit(fresh, 20, &pack).is_ok(),
            "sequences restart with the session"
        );
    }

    #[test]
    fn an_unavailable_structure_source_makes_everything_unusable() {
        let pack = AppPack::settings_fixture();
        let mut core = core();
        core.admit(SimApp::new().snapshot(&meta(1, 10)), 10, &pack)
            .unwrap();
        assert!(core.usable(10, 100));
        core.note_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("timeouts".into()),
            at_ms: 11,
        });
        assert!(!core.usable(11, 100));
        core.note_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 12,
        });
        assert!(core.usable(12, 100));
    }

    #[test]
    fn history_is_bounded_and_ordered() {
        let pack = AppPack::settings_fixture();
        let app = SimApp::new();
        let mut core = core();
        for sequence in 1..=(HISTORY as u64 + 10) {
            core.admit(app.snapshot(&meta(sequence, sequence)), sequence, &pack)
                .unwrap();
        }
        let sequences: Vec<u64> = core
            .retained()
            .map(|e| e.snapshot.source_sequence)
            .collect();
        assert_eq!(sequences.len(), HISTORY + 1);
        assert!(sequences.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(*sequences.last().unwrap(), HISTORY as u64 + 10);
        assert!(core.latest_since(HISTORY as u64 + 10).is_some());
        assert!(core.latest_since(HISTORY as u64 + 11).is_none());
    }
}
