//! Environment-owned, read-only authority for one explicitly installed
//! browser page scope. Browser snapshots remain distinct from native
//! `StructureSnapshot` values and never need synthetic window geometry.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use interaction_contracts::desktop::browser::{
    BrowserCheckboxBinding, BrowserPageSnapshot, BrowserReadScope, BrowserSourceInvalidation,
    derive_browser_checkbox_binding,
};
use interaction_contracts::desktop::ids::{BindingRef, SemanticTargetRef};
use sha2::{Digest, Sha256};

const MAX_BROWSER_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_BINDING_AGE_MS: u64 = 2_000;
const MAX_SCOPE_STARTUP_AGE_MS: u64 = 30_000;
const MAX_SNAPSHOT_IDS_PER_SCOPE: usize = 16_384;
const MAX_LIVE_BINDINGS: usize = 256;

/// Browser evidence admitted by Environment, tagged with the authority that
/// admitted it. This is a source observation, not an input permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEnvSnapshot {
    pub snapshot: BrowserPageSnapshot,
    pub revision: u64,
    pub scope_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserAdmitted {
    pub revision: u64,
    pub generation: u64,
    pub source_sequence: u64,
}

/// A live semantic reference to one source-only browser checkbox binding.
/// The proof establishes target identity; it grants no browser input authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLiveBinding {
    pub binding_ref: BindingRef,
    pub semantic_target: SemanticTargetRef,
    pub proof: BrowserCheckboxBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserReject {
    NoActiveScope,
    InvalidScope(String),
    ScopeFromFuture,
    ScopeExpired,
    SessionMismatch { expected: u64, got: u64 },
    GenerationNotIncreasing { floor: u64, got: u64 },
    ScopeIssuedBeforeFloor,
    ForeignScope,
    InvalidSnapshot(String),
    TimeNotSafe,
    ReadBeforeScope,
    ReadBeforeFloor,
    ReadFromFuture,
    ReadExpired,
    ReadOverlapsPrevious,
    OutOfOrder { last: u64, got: u64 },
    SnapshotIdReused,
    SnapshotHistoryFull,
    BindingHistoryFull,
    RevisionExhausted,
}

impl fmt::Display for BrowserReject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveScope => f.write_str("there is no installed browser read scope"),
            Self::InvalidScope(reason) => write!(f, "browser read scope is invalid: {reason}"),
            Self::ScopeFromFuture => f.write_str("browser read scope was issued in the future"),
            Self::ScopeExpired => f.write_str("browser read scope is expired"),
            Self::SessionMismatch { expected, got } => {
                write!(
                    f,
                    "browser scope session {got} does not match trusted session {expected}"
                )
            }
            Self::GenerationNotIncreasing { floor, got } => {
                write!(
                    f,
                    "browser scope generation {got} does not exceed floor {floor}"
                )
            }
            Self::ScopeIssuedBeforeFloor => {
                f.write_str("browser read scope predates the latest source time floor")
            }
            Self::ForeignScope => {
                f.write_str("browser evidence does not belong to the installed scope")
            }
            Self::InvalidSnapshot(reason) => {
                write!(f, "browser page snapshot is invalid: {reason}")
            }
            Self::TimeNotSafe => f.write_str("browser time is not a JavaScript-safe integer"),
            Self::ReadBeforeScope => f.write_str("browser read began before its scope was issued"),
            Self::ReadBeforeFloor => {
                f.write_str("browser page read did not start after the source floor")
            }
            Self::ReadFromFuture => f.write_str("browser page read finished in the future"),
            Self::ReadExpired => f.write_str("browser page read is stale"),
            Self::ReadOverlapsPrevious => {
                f.write_str("browser page read overlaps or predates the prior read")
            }
            Self::OutOfOrder { last, got } => {
                write!(f, "browser source sequence {got} does not follow {last}")
            }
            Self::SnapshotIdReused => {
                f.write_str("browser snapshot id was already admitted in this scope")
            }
            Self::SnapshotHistoryFull => {
                f.write_str("browser snapshot history bound is exhausted for this scope")
            }
            Self::BindingHistoryFull => f.write_str("browser live binding bound is exhausted"),
            Self::RevisionExhausted => f.write_str("browser environment revision is exhausted"),
        }
    }
}

impl std::error::Error for BrowserReject {}

/// One Environment-owned tracker. A scope can only be installed by a trusted
/// caller; a page snapshot can never create or renew its own scope.
#[derive(Debug, Default)]
pub(crate) struct BrowserState {
    authority_epoch: Option<u64>,
    scope: Option<BrowserReadScope>,
    scope_generation_floor: Option<u64>,
    /// A read must start strictly after this floor to produce a live binding.
    invalidation_floor_ms: Option<u64>,
    last_read_finished_ms: Option<u64>,
    last_sequence: Option<u64>,
    snapshot_ids: BTreeSet<String>,
    latest: Option<BrowserEnvSnapshot>,
    revision: u64,
    live_bindings: BTreeMap<SemanticTargetRef, BrowserLiveBinding>,
}

impl BrowserState {
    /// Scope generations and replay floors are scoped to the trusted desktop
    /// authority epoch. A new epoch clears all old browser evidence.
    pub(crate) fn set_authority_epoch(&mut self, epoch: u64) {
        if self.authority_epoch == Some(epoch) {
            return;
        }
        *self = Self {
            authority_epoch: Some(epoch),
            ..Self::default()
        };
    }

    pub(crate) fn install_scope(
        &mut self,
        scope: BrowserReadScope,
        now_ms: u64,
        configured_epoch: Option<u64>,
    ) -> Result<(), BrowserReject> {
        scope.validate().map_err(BrowserReject::InvalidScope)?;
        if now_ms > MAX_BROWSER_SAFE_INTEGER {
            return Err(BrowserReject::TimeNotSafe);
        }
        if scope.issued_ms > now_ms {
            return Err(BrowserReject::ScopeFromFuture);
        }
        if now_ms - scope.issued_ms >= MAX_SCOPE_STARTUP_AGE_MS {
            return Err(BrowserReject::ScopeExpired);
        }

        let expected_epoch = configured_epoch.or(self.authority_epoch);
        if let Some(expected) = expected_epoch
            && scope.context.session_epoch != expected
        {
            return Err(BrowserReject::SessionMismatch {
                expected,
                got: scope.context.session_epoch,
            });
        }
        if let Some(floor) = self.scope_generation_floor
            && scope.generation <= floor
        {
            return Err(BrowserReject::GenerationNotIncreasing {
                floor,
                got: scope.generation,
            });
        }

        let time_floor = self
            .invalidation_floor_ms
            .into_iter()
            .chain(self.last_read_finished_ms)
            .chain(self.scope.as_ref().map(|active| active.issued_ms))
            .max();
        if time_floor.is_some_and(|floor| scope.issued_ms <= floor) {
            return Err(BrowserReject::ScopeIssuedBeforeFloor);
        }
        let revision = self.next_revision()?;

        let scope_epoch = scope.context.session_epoch;
        self.authority_epoch = expected_epoch.or(Some(scope_epoch));
        self.scope_generation_floor = Some(scope.generation);
        self.scope = Some(scope);
        self.last_sequence = None;
        self.snapshot_ids.clear();
        self.latest = None;
        self.live_bindings.clear();
        self.revision = revision;
        Ok(())
    }

    pub(crate) fn invalidate_scope(
        &mut self,
        scope_id: &str,
        generation: u64,
        _reason: BrowserSourceInvalidation,
        now_ms: u64,
    ) -> Result<(), BrowserReject> {
        let matches_active = self
            .scope
            .as_ref()
            .is_some_and(|scope| scope.scope_id == scope_id && scope.generation == generation);
        if !matches_active {
            // A stop can race with an install acknowledgement. Retiring an
            // older generation is an idempotent no-op even after a newer scope
            // was installed; it cannot revoke that newer scope.
            let generation_is_retired = self
                .scope_generation_floor
                .is_some_and(|floor| generation <= floor);
            let does_not_match_current_generation = self
                .scope
                .as_ref()
                .is_none_or(|active| generation < active.generation);
            if generation_is_retired && does_not_match_current_generation {
                return Ok(());
            }
            return Err(BrowserReject::ForeignScope);
        }

        let revision = self.next_revision().unwrap_or(MAX_BROWSER_SAFE_INTEGER);
        if now_ms > MAX_BROWSER_SAFE_INTEGER {
            self.invalidation_floor_ms = Some(MAX_BROWSER_SAFE_INTEGER);
            self.scope = None;
            self.latest = None;
            self.live_bindings.clear();
            self.revision = revision;
            return Err(BrowserReject::TimeNotSafe);
        }

        self.invalidation_floor_ms = Some(now_ms.max(self.last_read_finished_ms.unwrap_or(0)));
        self.scope_generation_floor = Some(
            self.scope_generation_floor
                .unwrap_or(generation)
                .max(generation),
        );
        self.scope = None;
        self.latest = None;
        self.live_bindings.clear();
        self.revision = revision;
        Ok(())
    }

    /// Require a later read under the same trusted scope without retiring the
    /// scope or discarding the latest snapshot used to request that read.
    pub(crate) fn require_read_after(&mut self, floor_ms: u64) {
        self.invalidation_floor_ms = Some(
            self.invalidation_floor_ms
                .map_or(floor_ms, |current| current.max(floor_ms)),
        );
        self.live_bindings.clear();
        if self.revision < MAX_BROWSER_SAFE_INTEGER {
            self.revision += 1;
        }
    }

    pub(crate) fn admit_snapshot(
        &mut self,
        snapshot: BrowserPageSnapshot,
        now_ms: u64,
    ) -> Result<BrowserAdmitted, BrowserReject> {
        let Some(scope) = self.scope.clone() else {
            return Err(BrowserReject::NoActiveScope);
        };

        // A different target or clock is foreign evidence. It cannot replace
        // or invalidate the read currently owned by this scope.
        if snapshot.context != scope.context || snapshot.clock_domain != scope.clock_domain {
            return Err(BrowserReject::ForeignScope);
        }

        if let Err(error) = self.validate_current_snapshot(&snapshot, &scope, now_ms) {
            if error != BrowserReject::ReadBeforeFloor {
                self.clear_current();
            }
            return Err(error);
        }
        if self.snapshot_ids.len() >= MAX_SNAPSHOT_IDS_PER_SCOPE {
            self.clear_current();
            return Err(BrowserReject::SnapshotHistoryFull);
        }

        let revision = match self.next_revision() {
            Ok(revision) => revision,
            Err(error) => {
                self.clear_current();
                return Err(error);
            }
        };
        self.live_bindings.clear();
        self.last_sequence = Some(snapshot.source_sequence);
        self.last_read_finished_ms = Some(snapshot.read_finished_ms);
        self.snapshot_ids.insert(snapshot.snapshot_id.clone());
        let generation = scope.generation;
        let scope_id = scope.scope_id;
        let source_sequence = snapshot.source_sequence;
        self.latest = Some(BrowserEnvSnapshot {
            snapshot,
            revision,
            scope_id,
            generation,
        });
        self.revision = revision;
        Ok(BrowserAdmitted {
            revision,
            generation,
            source_sequence,
        })
    }

    fn validate_current_snapshot(
        &self,
        snapshot: &BrowserPageSnapshot,
        scope: &BrowserReadScope,
        now_ms: u64,
    ) -> Result<(), BrowserReject> {
        snapshot
            .validate()
            .map_err(BrowserReject::InvalidSnapshot)?;
        if now_ms > MAX_BROWSER_SAFE_INTEGER {
            return Err(BrowserReject::TimeNotSafe);
        }
        if snapshot.observed_ms < scope.issued_ms {
            return Err(BrowserReject::ReadBeforeScope);
        }
        if self
            .invalidation_floor_ms
            .is_some_and(|floor| snapshot.observed_ms <= floor)
        {
            return Err(BrowserReject::ReadBeforeFloor);
        }
        if snapshot.read_finished_ms > now_ms {
            return Err(BrowserReject::ReadFromFuture);
        }
        if now_ms - snapshot.observed_ms >= MAX_BINDING_AGE_MS {
            return Err(BrowserReject::ReadExpired);
        }
        if self
            .last_read_finished_ms
            .is_some_and(|finished| snapshot.observed_ms < finished)
        {
            return Err(BrowserReject::ReadOverlapsPrevious);
        }
        if let Some(last) = self.last_sequence
            && snapshot.source_sequence <= last
        {
            return Err(BrowserReject::OutOfOrder {
                last,
                got: snapshot.source_sequence,
            });
        }
        if self.snapshot_ids.contains(&snapshot.snapshot_id) {
            return Err(BrowserReject::SnapshotIdReused);
        }
        Ok(())
    }

    pub(crate) fn latest(&self) -> Option<&BrowserEnvSnapshot> {
        self.latest.as_ref()
    }

    pub(crate) fn scope(&self) -> Option<&BrowserReadScope> {
        self.scope.as_ref()
    }

    pub(crate) fn checkbox_binding(
        &mut self,
        semantic_target: SemanticTargetRef,
        name: &str,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<BrowserLiveBinding> {
        if max_age_ms == 0 {
            self.live_bindings.remove(&semantic_target);
            return None;
        }
        let scope = self.scope.as_ref()?.clone();
        let latest = self.latest.as_ref()?;
        let snapshot = &latest.snapshot;
        let age_limit = max_age_ms.min(MAX_BINDING_AGE_MS);
        if now_ms > MAX_BROWSER_SAFE_INTEGER
            || snapshot.read_finished_ms > now_ms
            || self
                .invalidation_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || now_ms.saturating_sub(snapshot.observed_ms) >= age_limit
        {
            self.clear_current();
            return None;
        }

        let proof = match derive_browser_checkbox_binding(snapshot, name) {
            Ok(binding) => binding,
            Err(_) => {
                self.live_bindings.remove(&semantic_target);
                return None;
            }
        };
        let binding_ref = self.binding_ref(&scope, latest, &semantic_target, name)?;
        let live = BrowserLiveBinding {
            binding_ref,
            semantic_target: semantic_target.clone(),
            proof,
        };
        if !self.live_bindings.contains_key(&semantic_target)
            && self.live_bindings.len() >= MAX_LIVE_BINDINGS
        {
            return None;
        }
        self.live_bindings.insert(semantic_target, live.clone());
        Some(live)
    }

    pub(crate) fn current_binding(&self, binding_ref: &BindingRef) -> Option<&BrowserLiveBinding> {
        self.live_bindings
            .values()
            .find(|binding| &binding.binding_ref == binding_ref)
    }

    fn binding_ref(
        &self,
        scope: &BrowserReadScope,
        latest: &BrowserEnvSnapshot,
        semantic_target: &SemanticTargetRef,
        name: &str,
    ) -> Option<BindingRef> {
        let mut hasher = Sha256::new();
        for bytes in [
            scope.scope_id.as_bytes(),
            latest.snapshot.snapshot_id.as_bytes(),
            semantic_target.as_str().as_bytes(),
            name.as_bytes(),
        ] {
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        hasher.update(scope.generation.to_be_bytes());
        hasher.update(latest.revision.to_be_bytes());
        BindingRef::new(format!("browser-bind-{:x}", hasher.finalize())).ok()
    }

    fn clear_current(&mut self) {
        let changed = self.latest.take().is_some() || !self.live_bindings.is_empty();
        self.live_bindings.clear();
        if changed && self.revision < MAX_BROWSER_SAFE_INTEGER {
            self.revision += 1;
        }
    }

    fn next_revision(&self) -> Result<u64, BrowserReject> {
        self.revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_BROWSER_SAFE_INTEGER)
            .ok_or(BrowserReject::RevisionExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::super::state_core::StateCore;
    use super::*;
    use interaction_contracts::desktop::browser::{
        BrowserObservedBool, BrowserPageNode, BrowserTargetContext, browser_target_context_hash,
    };
    use interaction_contracts::desktop::device::{DeviceIdentity, SurfaceRef};

    fn context(epoch: u64) -> BrowserTargetContext {
        BrowserTargetContext {
            browser_session_id: "session-1".into(),
            browser_context_id: "context-1".into(),
            page_target_id: "target-1".into(),
            cdp_session_id: "cdp-1".into(),
            frame_id: "frame-1".into(),
            document_loader_id: "loader-1".into(),
            fixture_file_url: "file:///tmp/fixtures/headless/readonly-page.html".into(),
            session_epoch: epoch,
        }
    }

    fn snapshot(
        sequence: u64,
        snapshot_id: &str,
        observed_ms: u64,
        read_finished_ms: u64,
        page_context: BrowserTargetContext,
    ) -> BrowserPageSnapshot {
        BrowserPageSnapshot {
            snapshot_id: snapshot_id.into(),
            source_sequence: sequence,
            observed_ms,
            read_finished_ms,
            clock_domain: "headless-clock".into(),
            target_context_hash: browser_target_context_hash(&page_context).unwrap(),
            context: page_context,
            root_ax_node_id: "root".into(),
            nodes: vec![
                BrowserPageNode {
                    ax_node_id: "root".into(),
                    backend_dom_node_id: Some(1),
                    parent_ax_node_id: None,
                    child_ax_node_ids: vec!["checkbox".into()],
                    ignored: false,
                    role: Some("rootWebArea".into()),
                    name: Some("Fixture".into()),
                    disabled: BrowserObservedBool::Unknown,
                    checked: BrowserObservedBool::Unknown,
                },
                BrowserPageNode {
                    ax_node_id: "checkbox".into(),
                    backend_dom_node_id: Some(2),
                    parent_ax_node_id: Some("root".into()),
                    child_ax_node_ids: vec![],
                    ignored: false,
                    role: Some("checkbox".into()),
                    name: Some("Background option".into()),
                    disabled: BrowserObservedBool::False,
                    checked: BrowserObservedBool::False,
                },
            ],
        }
    }

    fn scope(epoch: u64, generation: u64, issued_ms: u64) -> BrowserReadScope {
        BrowserReadScope {
            scope_id: "00000000-0000-4000-8000-000000000001".into(),
            generation,
            context: context(epoch),
            clock_domain: "headless-clock".into(),
            issued_ms,
        }
    }

    fn target() -> SemanticTargetRef {
        SemanticTargetRef::new("browser.fixture.background_option").unwrap()
    }

    fn device_identity(epoch: u64) -> DeviceIdentity {
        DeviceIdentity {
            session_epoch: epoch,
            surface: SurfaceRef {
                id: "main".into(),
                epoch,
            },
            clock_domain: format!("test/{epoch}/wall"),
            dependencies: Default::default(),
        }
    }

    #[test]
    fn snapshots_require_a_fresh_installed_scope_and_do_not_need_native_state() {
        let mut state = BrowserState::default();
        let first = snapshot(1, "snapshot-1", 11, 12, context(7));
        assert_eq!(
            state.admit_snapshot(first.clone(), 12),
            Err(BrowserReject::NoActiveScope)
        );

        state.set_authority_epoch(7);
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();
        let admitted = state.admit_snapshot(first, 12).unwrap();
        assert_eq!(admitted.source_sequence, 1);
        assert!(state.latest().is_some());
    }

    #[test]
    fn state_core_owns_browser_evidence_without_a_native_snapshot() {
        let mut state = StateCore::new();
        state.set_session(7);
        state.install_browser_scope(scope(7, 1, 10), 10).unwrap();
        state
            .admit_browser_snapshot(snapshot(1, "snapshot-1", 11, 12, context(7)), 12)
            .unwrap();

        assert!(state.latest().is_none());
        let browser = state.browser_latest().unwrap();
        assert_eq!(browser.snapshot.snapshot_id, "snapshot-1");
        assert_eq!(browser.scope_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(browser.generation, 1);
    }

    #[test]
    fn trusted_session_change_clears_browser_scope_and_its_generation_floor() {
        let mut state = StateCore::new();
        state.set_session(7);
        state.install_browser_scope(scope(7, 1, 10), 10).unwrap();
        state
            .admit_browser_snapshot(snapshot(1, "snapshot-1", 11, 12, context(7)), 12)
            .unwrap();

        state.set_session(8);
        assert!(state.browser_scope().is_none());
        assert!(state.browser_latest().is_none());
        state.install_browser_scope(scope(8, 1, 20), 20).unwrap();
        assert_eq!(state.browser_scope().unwrap().context.session_epoch, 8);
    }

    #[test]
    fn device_identity_epoch_change_retires_browser_scope_and_old_context_reads() {
        let mut state = StateCore::new();
        assert!(state.set_identity(&device_identity(7), false));
        state.install_browser_scope(scope(7, 1, 10), 10).unwrap();
        state
            .admit_browser_snapshot(snapshot(1, "old-session", 11, 12, context(7)), 12)
            .unwrap();
        let old_binding = state
            .browser_checkbox_binding(target(), "Background option", 13, 2_000)
            .unwrap();
        assert!(
            state
                .current_browser_binding(&old_binding.binding_ref)
                .is_some()
        );

        assert!(state.set_identity(&device_identity(8), false));
        assert!(state.browser_scope().is_none());
        assert!(state.browser_latest().is_none());
        assert!(
            state
                .current_browser_binding(&old_binding.binding_ref)
                .is_none()
        );
        assert_eq!(
            state.admit_browser_snapshot(snapshot(2, "late-old-session", 20, 21, context(7)), 21,),
            Err(BrowserReject::NoActiveScope),
            "the previous browser context cannot self-install after identity changes"
        );

        let mut new_scope = scope(8, 1, 22);
        new_scope.scope_id = "00000000-0000-4000-8000-000000000002".into();
        state.install_browser_scope(new_scope, 22).unwrap();
        state
            .admit_browser_snapshot(snapshot(1, "new-session", 23, 24, context(8)), 24)
            .unwrap();
        assert_eq!(
            state.browser_latest().unwrap().snapshot.snapshot_id,
            "new-session"
        );
    }

    #[test]
    fn wrong_context_is_foreign_and_cannot_replace_the_current_snapshot() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        assert_eq!(
            state.install_scope(scope(8, 1, 10), 10, Some(7)),
            Err(BrowserReject::SessionMismatch {
                expected: 7,
                got: 8
            })
        );
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();
        state
            .admit_snapshot(snapshot(1, "snapshot-1", 11, 12, context(7)), 12)
            .unwrap();

        let mut foreign_context = context(7);
        foreign_context.page_target_id = "another-target".into();
        let foreign = snapshot(99, "foreign", 20, 21, foreign_context);
        assert_eq!(
            state.admit_snapshot(foreign, 21),
            Err(BrowserReject::ForeignScope)
        );
        assert_eq!(state.latest().unwrap().snapshot.snapshot_id, "snapshot-1");
    }

    #[test]
    fn malformed_replay_and_out_of_order_reads_clear_current_binding_evidence() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();
        let first = snapshot(1, "snapshot-1", 11, 12, context(7));
        state.admit_snapshot(first.clone(), 12).unwrap();
        let live = state
            .checkbox_binding(target(), "Background option", 13, 2_000)
            .unwrap();
        assert!(state.current_binding(&live.binding_ref).is_some());

        let reused_id = snapshot(2, "snapshot-1", 13, 14, context(7));
        assert_eq!(
            state.admit_snapshot(reused_id, 14),
            Err(BrowserReject::SnapshotIdReused)
        );
        assert!(state.latest().is_none());
        assert!(state.current_binding(&live.binding_ref).is_none());
        assert_eq!(
            state.admit_snapshot(first, 14),
            Err(BrowserReject::ReadOverlapsPrevious)
        );
        assert_eq!(
            state.admit_snapshot(snapshot(1, "snapshot-replay", 13, 14, context(7)), 14),
            Err(BrowserReject::OutOfOrder { last: 1, got: 1 })
        );
    }

    #[test]
    fn bad_context_hash_clears_live_binding_without_rolling_back_order() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();
        state
            .admit_snapshot(snapshot(1, "snapshot-1", 11, 12, context(7)), 12)
            .unwrap();
        let live = state
            .checkbox_binding(target(), "Background option", 13, 2_000)
            .unwrap();

        let mut wrong_hash = snapshot(2, "snapshot-2", 13, 14, context(7));
        wrong_hash.target_context_hash = format!("tc1:{}", "0".repeat(64));
        assert!(matches!(
            state.admit_snapshot(wrong_hash, 14),
            Err(BrowserReject::InvalidSnapshot(_))
        ));
        assert!(state.latest().is_none());
        assert!(state.current_binding(&live.binding_ref).is_none());
        assert_eq!(
            state.admit_snapshot(snapshot(1, "snapshot-replay", 13, 14, context(7)), 14),
            Err(BrowserReject::OutOfOrder { last: 1, got: 1 })
        );
        assert!(
            state
                .admit_snapshot(snapshot(2, "snapshot-3", 13, 14, context(7)), 14)
                .is_ok()
        );
    }

    #[test]
    fn read_end_must_not_be_after_now_and_age_is_strict() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();

        let before_scope = snapshot(1, "before", 9, 10, context(7));
        assert_eq!(
            state.admit_snapshot(before_scope, 10),
            Err(BrowserReject::ReadBeforeScope)
        );
        let future = snapshot(1, "future", 11, 13, context(7));
        assert_eq!(
            state.admit_snapshot(future, 12),
            Err(BrowserReject::ReadFromFuture)
        );

        let fresh = snapshot(1, "fresh", 11, 12, context(7));
        state.admit_snapshot(fresh, 12).unwrap();
        assert!(
            state
                .checkbox_binding(target(), "Background option", 2_011, 3_000)
                .is_none()
        );
        assert!(state.latest().is_none(), "age 2,000 ms is expired");
    }

    #[test]
    fn scope_install_has_a_thirty_second_startup_bound_but_reads_stay_two_seconds() {
        let mut slow_start = BrowserState::default();
        slow_start.set_authority_epoch(7);
        slow_start
            .install_scope(scope(7, 1, 10), 30_009, Some(7))
            .expect("a scope may survive the bounded Host/Device startup handshake");
        slow_start
            .admit_snapshot(
                snapshot(1, "late-start-read", 30_008, 30_009, context(7)),
                30_009,
            )
            .expect("the read itself is fresh even though its scope is older");

        let mut too_late = BrowserState::default();
        too_late.set_authority_epoch(7);
        assert_eq!(
            too_late.install_scope(scope(7, 1, 10), 30_010, Some(7)),
            Err(BrowserReject::ScopeExpired)
        );

        let stale_read = snapshot(2, "stale-read", 30_009, 30_010, context(7));
        assert_eq!(
            slow_start.admit_snapshot(stale_read, 32_009),
            Err(BrowserReject::ReadExpired)
        );
    }

    #[test]
    fn stop_and_duplicate_retirement_cannot_reopen_or_clear_a_new_scope() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        let first_scope = scope(7, 1, 10);
        state
            .install_scope(first_scope.clone(), 10, Some(7))
            .unwrap();
        let old = snapshot(1, "snapshot-old", 11, 12, context(7));
        state.admit_snapshot(old.clone(), 12).unwrap();
        state
            .invalidate_scope(
                &first_scope.scope_id,
                first_scope.generation,
                BrowserSourceInvalidation::Stopped,
                20,
            )
            .unwrap();
        assert!(state.latest().is_none());
        assert_eq!(
            state.admit_snapshot(old, 21),
            Err(BrowserReject::NoActiveScope)
        );
        assert_eq!(
            state.install_scope(scope(7, 2, 20), 20, Some(7)),
            Err(BrowserReject::ScopeIssuedBeforeFloor)
        );

        state.install_scope(scope(7, 2, 21), 21, Some(7)).unwrap();
        state
            .invalidate_scope(
                &first_scope.scope_id,
                first_scope.generation,
                BrowserSourceInvalidation::Stopped,
                22,
            )
            .unwrap();
        assert_eq!(state.scope().unwrap().generation, 2);
        let new = snapshot(1, "snapshot-new", 22, 23, context(7));
        state.admit_snapshot(new, 23).unwrap();
    }

    #[test]
    fn binding_ref_is_stable_for_one_snapshot_and_keeps_caller_semantics() {
        let mut state = BrowserState::default();
        state.set_authority_epoch(7);
        state.install_scope(scope(7, 1, 10), 10, Some(7)).unwrap();
        state
            .admit_snapshot(snapshot(1, "snapshot-1", 11, 12, context(7)), 12)
            .unwrap();
        let semantic_target = target();
        let first = state
            .checkbox_binding(semantic_target.clone(), "Background option", 13, 1_000)
            .unwrap();
        let second = state
            .checkbox_binding(semantic_target.clone(), "Background option", 14, 1_000)
            .unwrap();
        assert_eq!(first.binding_ref, second.binding_ref);
        assert_eq!(first.semantic_target, semantic_target);
        assert_eq!(first.proof.snapshot_id, "snapshot-1");
        assert!(state.current_binding(&first.binding_ref).is_some());
    }
}
