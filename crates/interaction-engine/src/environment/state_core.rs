//! StateCore: the single reducer that admits observations (design 6.3).
//!
//! A snapshot is accepted only when its session, order and time make sense.
//! Anything stale or out of order is refused and never overwrites what is
//! current. Evidence age is always computed from the original observation
//! time, so re-sending or re-reading a snapshot can never make it fresher.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use interaction_contracts::desktop::binding::derive_window_root_binding_evidence;
use interaction_contracts::desktop::browser::{
    BrowserPageSnapshot, BrowserReadScope, BrowserSourceInvalidation,
};
use interaction_contracts::desktop::computer::ComputerFrameRequest;
use interaction_contracts::desktop::device::DeviceIdentity;
use interaction_contracts::desktop::ids::{BindingRef, SemanticTargetRef};
use interaction_contracts::desktop::operator::{
    BackgroundWindowTarget, MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE, ObservationView,
    ObservedTarget, ScopedWindowTarget,
};
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::{
    FrameMeta, FrameStatus, SourceHealth, SourceIdentityBoundary, SourceKind, SourceStamp,
    SourceState, StructureSnapshot, TargetContext,
};

use super::browser::{
    BrowserAdmitted, BrowserEnvSnapshot, BrowserLiveBinding, BrowserReject, BrowserState,
};
use super::computer_observation::{self, FrozenComputerObservation};
use super::frame::{FrameChange, FrameTracker};
use super::owned_document::{OwnedDocumentBinding, OwnedDocumentBindingTracker};
use super::pack::AppPack;
use super::read::focused_window_id;

const HISTORY: usize = 32;
const MAX_ROW_SCOPE_FRESHNESS_MS: u64 = 10_000;
const AX_SCOPED_ROW_OMISSION_REASON: &str = "AX table row subtree omitted for scoped census";

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

#[derive(Debug)]
pub struct StateCore {
    expected_session: Option<u64>,
    last_sequence: Option<u64>,
    revision: u64,
    layer_revision: u64,
    latest: Option<EnvSnapshot>,
    history: VecDeque<EnvSnapshot>,
    health: BTreeMap<String, SourceHealth>,
    identity: Option<DeviceIdentity>,
    frames: FrameTracker,
    require_frames: bool,
    content_expired: bool,
    structure_floor_ms: Option<u64>,
    frame_floor_ms: Option<u64>,
    frame_floor_stamp: Option<SourceStamp>,
    /// Foreground changes end execution authority without ending readonly
    /// screen capture. A later Complete and AX read must establish it again.
    execution_frame_floor_ms: Option<u64>,
    ax_metadata_floor_ms: Option<u64>,
    /// Row-scope freshness floor. AX Degraded alone does not advance it;
    /// a validated scoped census may be admitted after that generic-only floor.
    row_structure_floor_ms: Option<u64>,
    retired_health_streams: BTreeMap<String, BTreeSet<String>>,
    health_stamps: BTreeMap<String, SourceStamp>,
    refresh_generation: u64,
    refresh_pending: Option<u64>,
    /// Latest admitted read when the old watcher retirement was acknowledged.
    /// Only a later same-session read can reopen execution evidence.
    refresh_ack_floor: Option<(u64, u64)>,
    browser: BrowserState,
    owned_document: OwnedDocumentBindingTracker,
    owned_document_max_age_ms: u64,
}

impl Default for StateCore {
    fn default() -> Self {
        Self::with_frame_timeout(2_000)
    }
}

fn health_key(kind: SourceKind) -> String {
    format!("{kind:?}")
}

impl StateCore {
    /// Focus transitions rely on current window metadata, separately from
    /// foreground pixel evidence. All normal target evidence stays unchanged.
    pub fn window_root(
        &self,
        app: &str,
        pid: u32,
        window: &str,
        now: u64,
        max_age: u64,
    ) -> Option<&interaction_contracts::desktop::source::UiObject> {
        if self.refresh_evidence_pending() {
            return None;
        }
        let env = self.latest.as_ref()?;
        let snapshot = &env.snapshot;
        if snapshot.menu_context.is_some()
            || !snapshot.complete
            || self.structure_unavailable()
            || snapshot.app.bundle_id != app
            || snapshot.app.pid != pid
            || snapshot.observed_ms > now
            || now.saturating_sub(snapshot.observed_ms) >= max_age
            || self
                .ax_metadata_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || snapshot.windows.iter().any(|w| w.modal)
        {
            return None;
        }
        let info = snapshot.windows.iter().find(|w| {
            w.window_id == window
                && w.owner_bundle_id == app
                && w.kind == interaction_contracts::desktop::source::LayerKind::Window
        })?;
        snapshot.objects.iter().find(|o| {
            o.parent.is_none()
                && o.window_id == info.window_id
                && o.role == "AXWindow"
                && o.subrole.as_deref() == Some("AXStandardWindow")
                && o.in_view
                && !o.secure
                && [
                    o.frame.x - info.frame.x,
                    o.frame.y - info.frame.y,
                    o.frame.width - info.frame.width,
                    o.frame.height - info.frame.height,
                ]
                .into_iter()
                .all(|delta| delta.abs() < 0.5)
        })
    }

    /// Read the active normal-window root through an exact frozen row scope.
    /// This is metadata only and may feed the dedicated WindowActivation proof
    /// when that row goal has a partial Source tree; generic control paths still
    /// require `window_root` and its globally complete AX tree.
    pub fn row_window_root(
        &self,
        scope: &interaction_contracts::desktop::task::RowTableScope,
        app: &str,
        pid: u32,
        window: &str,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<&interaction_contracts::desktop::source::UiObject> {
        if self.refresh_evidence_pending()
            || max_age_ms == 0
            || max_age_ms > MAX_ROW_SCOPE_FRESHNESS_MS
        {
            return None;
        }
        let snapshot = &self.latest.as_ref()?.snapshot;
        if snapshot.menu_context.is_some()
            || snapshot.app.bundle_id != app
            || snapshot.app.pid != pid
            || focused_window_id(snapshot) != Some(window)
            || snapshot.observer_coverage.state == SourceState::Unavailable
            || self.health(SourceKind::Ax).is_some_and(|health| {
                health.state == SourceState::Unavailable
                    || health.state == SourceState::Degraded && snapshot.complete
            })
            || snapshot.observed_ms > now_ms
            || now_ms - snapshot.observed_ms >= max_age_ms
            || self
                .row_structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || self
                .ax_metadata_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || snapshot.windows.iter().any(|candidate| candidate.modal)
            || super::row::row_scope_coverage(snapshot, scope).is_err()
        {
            return None;
        }

        let context = snapshot.window_context.as_ref()?;
        if context.session_epoch != snapshot.session_epoch
            || context.verified_source_sequence != snapshot.source_sequence
            || context.verified_observed_ms != snapshot.observed_ms
            || context.process.pid != pid
            || context.origin_window_id != window
            || context.origin_window_frame
                != snapshot
                    .windows
                    .iter()
                    .find(|candidate| candidate.window_id == window)?
                    .frame
        {
            return None;
        }

        if !snapshot.complete {
            let search = snapshot.table_scope_search.as_ref()?;
            if &search.requested_scope != scope
                || search.process_id != pid
                || search.window_id != window
                || search.unique_candidate().is_none()
            {
                return None;
            }
        }

        let windows: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|candidate| candidate.window_id == window)
            .collect();
        let [info] = windows.as_slice() else {
            return None;
        };
        if info.kind != interaction_contracts::desktop::source::LayerKind::Window
            || info.modal
            || info.owner_bundle_id != app
            || context.origin_window_frame != info.frame
        {
            return None;
        }

        let roots: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| object.window_id == window && object.parent.is_none())
            .collect();
        let [root] = roots.as_slice() else {
            return None;
        };
        if root.role != "AXWindow"
            || root.subrole.as_deref() != Some("AXStandardWindow")
            || !root.in_view
            || root.secure
            || root.frame != info.frame
        {
            return None;
        }
        Some(*root)
    }

    /// Whether the latest complete Source snapshot proves one fresh native
    /// background target context. Global focus and active-layer facts are
    /// deliberately independent and are not used by this gate.
    pub fn background_ax_usable(
        &self,
        app_identity: (&str, u32),
        window: &str,
        process_birth: Option<(u64, u64)>,
        expected_target_context_hash: Option<&str>,
        now_ms: u64,
        max_age_ms: u64,
    ) -> bool {
        let (app, pid) = app_identity;
        if self.refresh_evidence_pending()
            || max_age_ms == 0
            || max_age_ms > 2_000
            || self.structure_unavailable()
        {
            return false;
        }
        let Some(env) = self.latest.as_ref() else {
            return false;
        };
        let snapshot = &env.snapshot;
        if !snapshot.complete
            || snapshot.menu_context.is_some()
            || snapshot.app.bundle_id != app
            || snapshot.app.pid != pid
            || snapshot.observer_coverage.state == SourceState::Unavailable
            || snapshot.observed_ms > now_ms
            || now_ms.saturating_sub(snapshot.observed_ms) >= max_age_ms
            || self
                .structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || self
                .ax_metadata_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || self
                .row_structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || snapshot.validate().is_err()
        {
            return false;
        }

        let Some(interaction_contracts::desktop::source::TargetContext::NativeWindow {
            app: target_app,
            source,
        }) = snapshot.target_context.as_ref()
        else {
            return false;
        };
        let window_context = &source.window;
        if target_app.bundle_id != app
            || target_app.pid != pid
            || target_app != &snapshot.app
            || window_context.session_epoch != snapshot.session_epoch
            || window_context.verified_source_sequence != snapshot.source_sequence
            || window_context.verified_observed_ms != snapshot.observed_ms
            || window_context.process.pid != pid
            || window_context.origin_window_id != window
            || !source.is_visible_standard_window()
            || expected_target_context_hash
                .is_some_and(|expected| expected != source.target_context_hash)
            || process_birth.is_some_and(|(seconds, micros)| {
                window_context.process.start_seconds != seconds
                    || window_context.process.start_micros != micros
            })
        {
            return false;
        }

        let windows: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|candidate| candidate.window_id == window)
            .collect();
        let [info] = windows.as_slice() else {
            return false;
        };
        if info.kind != interaction_contracts::desktop::source::LayerKind::Window
            || info.modal
            || info.owner_bundle_id != app
            || info.frame != window_context.origin_window_frame
        {
            return false;
        }
        let roots: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| object.window_id == window && object.parent.is_none())
            .collect();
        matches!(
            roots.as_slice(),
            [root]
                if root.role == "AXWindow"
                    && root.subrole.as_deref() == Some("AXStandardWindow")
                    && root.in_view
                    && !root.secure
                    && root.frame == info.frame
        )
    }

    /// Build a read-only consent candidate from one complete local table
    /// census inside the current partial Source snapshot. This metadata never
    /// replaces the whole-tree `observed_target` or ordinary input evidence.
    fn scoped_window_target(&self, now_ms: u64) -> Option<ScopedWindowTarget> {
        let identity = self.identity.as_ref()?;
        let snapshot = &self.latest.as_ref()?.snapshot;
        if snapshot.complete
            || self.expected_session != Some(snapshot.session_epoch)
            || identity.session_epoch != snapshot.session_epoch
            || identity.surface != snapshot.surface
            || identity.clock_domain != snapshot.clock_domain
            || identity.clock_domain != format!("macos/{}/wall", identity.session_epoch)
            || snapshot.app.pid == 0
            || snapshot.source_sequence == 0
            || snapshot.source_sequence > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || snapshot.observed_ms > now_ms
            || now_ms.saturating_sub(snapshot.observed_ms) >= 2_000
            || snapshot.objects.iter().any(|object| object.secure)
        {
            return None;
        }

        let valid_until_ms = snapshot.observed_ms.checked_add(2_000)?;
        let census = snapshot.table_scope_search.as_ref()?;
        census.unique_candidate()?;
        let table_scope = &census.requested_scope;
        let source = snapshot.window_context.as_ref()?;
        let window_id = source.origin_window_id.as_str();
        let root = self.row_window_root(
            table_scope,
            &snapshot.app.bundle_id,
            snapshot.app.pid,
            window_id,
            now_ms,
            2_000,
        )?;
        let evidence = derive_window_root_binding_evidence(snapshot, root, table_scope).ok()?;
        let root_identity_hash = evidence.continuity.context_digest.strip_prefix("bc1:")?;
        if root_identity_hash.len() != 64
            || !root_identity_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }

        let windows: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.window_id == window_id)
            .collect();
        let [window] = windows.as_slice() else {
            return None;
        };
        let window_title = window.title.clone()?;
        if window_title.trim().is_empty() {
            return None;
        }
        let target = ObservedTarget {
            snapshot_id: snapshot.snapshot_id.clone(),
            structure_sequence: snapshot.source_sequence,
            session_epoch: snapshot.session_epoch,
            app: snapshot.app.bundle_id.clone(),
            pid: snapshot.app.pid,
            window_id: window.window_id.clone(),
            window_title: Some(window_title),
            frame_points: Some(
                [
                    window.frame.x,
                    window.frame.y,
                    window.frame.width,
                    window.frame.height,
                ]
                .map(|value| value.round() as i64),
            ),
            observed_ms: snapshot.observed_ms,
            valid_until_ms,
            contains_private_fields: false,
        };
        let scoped = ScopedWindowTarget {
            target,
            table_scope: table_scope.clone(),
            source: source.clone(),
            root_identity_hash: root_identity_hash.to_owned(),
        };
        scoped.validate().ok()?;
        Some(scoped)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_frame_timeout(timeout_ms: u64) -> Self {
        Self {
            expected_session: None,
            last_sequence: None,
            revision: 0,
            layer_revision: 0,
            latest: None,
            history: VecDeque::new(),
            health: BTreeMap::new(),
            identity: None,
            frames: FrameTracker::new(timeout_ms),
            require_frames: false,
            content_expired: false,
            structure_floor_ms: None,
            frame_floor_ms: None,
            frame_floor_stamp: None,
            execution_frame_floor_ms: None,
            ax_metadata_floor_ms: None,
            row_structure_floor_ms: None,
            retired_health_streams: BTreeMap::new(),
            health_stamps: BTreeMap::new(),
            refresh_generation: 0,
            refresh_pending: None,
            refresh_ack_floor: None,
            browser: BrowserState::default(),
            owned_document: OwnedDocumentBindingTracker::default(),
            owned_document_max_age_ms: 2_000,
        }
    }

    /// Dependency revisions are not device identity changes.
    pub fn set_identity(&mut self, identity: &DeviceIdentity, require_frames: bool) -> bool {
        let mut identity = identity.clone();
        identity.dependencies.clear();
        let session_changed = self.expected_session != Some(identity.session_epoch);
        let changed = session_changed
            || self.identity.as_ref() != Some(&identity)
            || self.require_frames != require_frames;
        if changed {
            self.expected_session = Some(identity.session_epoch);
            self.browser.set_authority_epoch(identity.session_epoch);
            self.last_sequence = None;
            self.latest = None;
            self.refresh_ack_floor = None;
            self.history.clear();
            self.health.clear();
            self.retired_health_streams.clear();
            self.health_stamps.clear();
            self.structure_floor_ms = None;
            self.frame_floor_ms = None;
            self.frame_floor_stamp = None;
            self.execution_frame_floor_ms = None;
            self.ax_metadata_floor_ms = None;
            self.row_structure_floor_ms = None;
            self.content_expired = false;
            self.frames.reset();
            self.frames.set_identity(&identity);
            self.owned_document.reset_identity();
            self.identity = Some(identity);
            self.require_frames = require_frames;
        }
        changed
    }

    /// A trusted execution-domain change retires Source facts admitted under
    /// the previous authority profile. The next snapshot must have started
    /// after this floor before any domain can use it.
    pub(crate) fn require_fresh_source_after(&mut self, at_ms: u64) {
        self.structure_floor_ms = Some(
            self.structure_floor_ms
                .map_or(at_ms, |floor| floor.max(at_ms)),
        );
        self.row_structure_floor_ms = Some(
            self.row_structure_floor_ms
                .map_or(at_ms, |floor| floor.max(at_ms)),
        );
        self.ax_metadata_floor_ms = Some(
            self.ax_metadata_floor_ms
                .map_or(at_ms, |floor| floor.max(at_ms)),
        );
    }

    pub fn requires_frames(&self) -> bool {
        self.require_frames
    }

    /// The newest admitted COMPLETE screenshot, only while its pixels and
    /// normal Source structure are currently usable.
    pub fn current_complete_frame(&self, now_ms: u64, max_age_ms: u64) -> Option<&FrameMeta> {
        (self.usable(now_ms, max_age_ms) && self.frame_deadline(now_ms, max_age_ms).is_some())
            .then(|| self.frames.latest_content())
            .flatten()
    }

    /// Read-only capture context. Expired pixels are never execution evidence,
    /// but an independently healthy watch can obtain a replacement image.
    pub fn current_watch_frame(&self, now_ms: u64) -> Option<&FrameMeta> {
        (!self.refresh_evidence_pending()
            && self.frames.healthy(now_ms)
            && !self.structure_unavailable()
            && self
                .health(SourceKind::Screenshot)
                .is_none_or(|health| health.state == SourceState::Available)
            && self.frame_matches_structure())
        .then(|| self.frames.latest_content())
        .flatten()
    }

    /// This proves only that the original immutable model image still belongs
    /// to the same watch. Input and outbound screenshots use the stronger
    /// fresh-observation validator below.
    pub fn validate_frozen_computer_watch(
        &self,
        frozen: &FrozenComputerObservation,
        now_ms: u64,
    ) -> Result<(), String> {
        if now_ms >= frozen.request().fence.deadline_ms {
            return Err("frozen computer request deadline has elapsed".into());
        }
        let frame = self
            .current_watch_frame(now_ms)
            .ok_or("computer watch is no longer healthy or in the same Source context")?;
        computer_observation::validate_live_stream(frozen, frame)
    }

    /// Whether a new source identity boundary makes every prior binding stale.
    /// The first snapshot is a baseline; native snapshots without a boundary
    /// are treated as unknown continuity rather than as a stable identity.
    pub fn identity_boundary_changed(&self, incoming: Option<&SourceIdentityBoundary>) -> bool {
        let Some(previous) = self
            .latest
            .as_ref()
            .map(|env| env.snapshot.identity_boundary.as_ref())
        else {
            return false;
        };
        match (previous, incoming) {
            (Some(previous), Some(incoming)) => previous != incoming,
            (None, None) => self.require_frames,
            _ => true,
        }
    }

    /// A queued refresh can still retire otherwise fresh facts. No execution
    /// evidence is usable until Source confirms that transition is past.
    pub fn begin_refresh(&mut self) -> Option<u64> {
        let generation = self.next_refresh_generation()?;
        self.refresh_pending = Some(generation);
        self.owned_document.clear_current();
        Some(generation)
    }

    /// Allocate a unique Source acknowledgement generation without starting a
    /// business refresh or retiring any current evidence. Lifecycle scope
    /// cleanup uses this so its acknowledgement cannot complete a later read.
    pub fn next_refresh_generation(&mut self) -> Option<u64> {
        let generation = self.refresh_generation.checked_add(1)?;
        self.refresh_generation = generation;
        Some(generation)
    }

    pub fn refresh_started(&mut self, generation: u64) -> bool {
        if self.refresh_pending != Some(generation) {
            return false;
        }
        self.refresh_ack_floor = self
            .latest
            .as_ref()
            .map(|env| (env.snapshot.session_epoch, env.snapshot.source_sequence));
        self.refresh_pending = None;
        true
    }

    pub fn refresh_pending(&self) -> bool {
        self.refresh_pending.is_some()
    }

    fn refresh_evidence_pending(&self) -> bool {
        self.refresh_pending() || self.refresh_ack_floor.is_some()
    }

    pub fn admit_frame(
        &mut self,
        meta: FrameMeta,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<FrameChange, String> {
        let complete = meta.status == FrameStatus::Complete;
        if matches!(meta.status, FrameStatus::Complete | FrameStatus::Idle)
            && let (Some(snapshot), Some(stream)) = (
                self.latest.as_ref().map(|env| &env.snapshot),
                meta.window_geometry(),
            )
            && (snapshot.app.pid != stream.pid
                || !snapshot
                    .windows
                    .iter()
                    .any(|window| window.window_id == stream.window_id))
        {
            return Err("screen frame does not match the target PID or known window".into());
        }
        if complete {
            if self.frame_floor_ms.is_some_and(|floor| {
                meta.source_ms <= floor
                    || meta
                        .stream_info()
                        .is_some_and(|stream| stream.callback_ms <= floor)
            }) {
                return Err("complete frame predates the source loss or expiry".into());
            }
            if let (Some(floor), Some(watch_id)) = (&self.frame_floor_stamp, meta.watch_id())
                && floor.stream_id == watch_id
                && meta.source_sequence <= floor.source_sequence
            {
                return Err("complete frame predates the failed source status".into());
            }
        }
        let mut change = self.frames.admit(meta, now_ms)?;
        if change.invalidated || change.context_changed {
            self.structure_floor_ms = Some(now_ms);
            if self.require_frames {
                self.row_structure_floor_ms = Some(now_ms);
            }
        }
        if complete
            && self.frames.healthy(now_ms)
            && self
                .frames
                .content_deadline(max_age_ms)
                .is_some_and(|deadline| now_ms < deadline)
        {
            change.recovered |= self.content_expired;
            self.content_expired = false;
        }
        Ok(change)
    }

    /// Freeze the exact latest complete image together with the current Source
    /// structure that the host paired with this computer request. A later
    /// frame or AX read cannot be substituted for either half of the pair.
    pub fn freeze_computer_observation(
        &self,
        request: ComputerFrameRequest,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<FrozenComputerObservation, String> {
        if self.refresh_evidence_pending() || now_ms >= request.fence.deadline_ms {
            return Err("computer observation request is pending refresh or expired".into());
        }
        if !self.usable(now_ms, max_age_ms) || self.frame_deadline(now_ms, max_age_ms).is_none() {
            return Err("no current healthy Source structure and screenshot can be frozen".into());
        }
        let env = self
            .latest
            .as_ref()
            .ok_or("no admitted Source structure can be frozen")?;
        let frame = self
            .frames
            .latest_content()
            .ok_or("no complete screenshot image can be frozen")?;
        computer_observation::freeze(request, frame, env)
    }

    /// Check that a retained computer batch still belongs to the same live
    /// screenshot stream and full-window geometry. New images within that
    /// stream are allowed; a restarted stream or changed geometry closes it.
    pub fn validate_frozen_computer_observation(
        &self,
        frozen: &FrozenComputerObservation,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if self.refresh_evidence_pending() {
            return Err("frozen computer batch is blocked by a pending Source refresh".into());
        }
        if now_ms >= frozen.request().fence.deadline_ms {
            return Err("frozen computer request deadline has elapsed".into());
        }
        if self.frame_deadline(now_ms, max_age_ms).is_none() {
            return Err("frozen computer batch no longer has healthy current pixels".into());
        }
        let frame = self
            .frames
            .latest_content()
            .ok_or("current computer image is unavailable")?;
        computer_observation::validate_live_stream(frozen, frame)
    }

    /// Content expiry closes execution without misreporting healthy IDLE as a
    /// frozen stream. Both transitions are emitted once, before input handling.
    pub fn expire_sources(&mut self, now_ms: u64, max_age_ms: u64) -> Option<&'static str> {
        let heartbeat_expired = self.frames.expire(now_ms);
        let content_expired = !self.content_expired
            && self
                .frames
                .content_deadline(max_age_ms)
                .is_some_and(|deadline| now_ms >= deadline);
        if content_expired {
            self.content_expired = true;
        }
        let reason = if heartbeat_expired {
            Some("the screen stream stopped reporting callbacks")
        } else if content_expired {
            Some("the captured pixels expired; a new complete frame is required")
        } else {
            None
        };
        if reason.is_some() {
            self.structure_floor_ms = Some(now_ms);
            self.frame_floor_ms = Some(now_ms);
            if self.require_frames {
                self.row_structure_floor_ms = Some(now_ms);
            }
        }
        reason
    }

    /// The extra lease a native observation must obey. IDLE does not extend
    /// the original content deadline.
    pub fn frame_deadline(&self, now_ms: u64, max_age_ms: u64) -> Option<u64> {
        if !self.frames.healthy(now_ms)
            || self.content_expired
            || self
                .health(SourceKind::Screenshot)
                .is_some_and(|health| health.state != SourceState::Available)
            || self.require_frames
                && (self.structure_unavailable()
                    || self.latest.as_ref().is_none_or(|env| {
                        !env.snapshot.complete
                            || self
                                .structure_floor_ms
                                .is_some_and(|floor| env.snapshot.observed_ms <= floor)
                    })
                    || !self.frame_matches_structure()
                    || self.execution_frame_floor_ms.is_some_and(|floor| {
                        self.frames
                            .latest_content()
                            .is_none_or(|frame| frame.source_ms <= floor)
                    }))
        {
            return None;
        }
        let mut deadline = self
            .frames
            .content_deadline(max_age_ms)?
            .min(self.frames.heartbeat_deadline()?);
        if self.require_frames {
            deadline = deadline.min(
                self.latest
                    .as_ref()?
                    .snapshot
                    .observed_ms
                    .saturating_add(max_age_ms),
            );
        }
        (now_ms < deadline).then_some(deadline)
    }

    /// Whether a complete frozen row scope remains usable on the current
    /// Source read. This separate gate can admit a verified scoped census from
    /// a globally partial snapshot while leaving generic `usable` unchanged.
    pub fn row_scope_usable(
        &self,
        scope: &interaction_contracts::desktop::task::RowTableScope,
        now_ms: u64,
        max_age_ms: u64,
    ) -> bool {
        if self.refresh_evidence_pending()
            || max_age_ms == 0
            || max_age_ms > MAX_ROW_SCOPE_FRESHNESS_MS
        {
            return false;
        }
        let Some(env) = self.latest.as_ref() else {
            return false;
        };
        let snapshot = &env.snapshot;
        if snapshot.menu_context.is_some()
            || focused_window_id(snapshot).is_none()
            || snapshot.observer_coverage.state == SourceState::Unavailable
            || self
                .health(SourceKind::Ax)
                .is_some_and(|health| health.state == SourceState::Unavailable)
            || self
                .health(SourceKind::Ax)
                .is_some_and(|health| health.state == SourceState::Degraded)
                && snapshot.complete
            || snapshot.observed_ms > now_ms
            || now_ms - snapshot.observed_ms >= max_age_ms
            || self
                .row_structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || super::row::row_scope_coverage(snapshot, scope).is_err()
        {
            return false;
        }
        !self.require_frames
            || self
                .row_scope_frame_deadline(scope, now_ms, max_age_ms)
                .is_some()
    }

    /// Return the existing screenshot lease for a validated row scope. Unlike
    /// the generic frame gate this permits the scope's verified partial tree,
    /// while retaining the same real FrameTracker, geometry, health, floor,
    /// content-age, and heartbeat requirements.
    pub fn row_scope_frame_deadline(
        &self,
        scope: &interaction_contracts::desktop::task::RowTableScope,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<u64> {
        if self.refresh_evidence_pending()
            || max_age_ms == 0
            || max_age_ms > MAX_ROW_SCOPE_FRESHNESS_MS
            || self.content_expired
            || !self.frames.healthy(now_ms)
            || self
                .health(SourceKind::Screenshot)
                .is_some_and(|health| health.state != SourceState::Available)
        {
            return None;
        }
        let env = self.latest.as_ref()?;
        let snapshot = &env.snapshot;
        if snapshot.menu_context.is_some()
            || focused_window_id(snapshot).is_none()
            || snapshot.observer_coverage.state == SourceState::Unavailable
            || self
                .health(SourceKind::Ax)
                .is_some_and(|health| health.state == SourceState::Unavailable)
            || self
                .health(SourceKind::Ax)
                .is_some_and(|health| health.state == SourceState::Degraded)
                && snapshot.complete
            || snapshot.observed_ms > now_ms
            || now_ms - snapshot.observed_ms >= max_age_ms
            || self
                .row_structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || super::row::row_scope_coverage(snapshot, scope).is_err()
            || !self.frame_matches_structure()
        {
            return None;
        }
        let frame = self.frames.latest_content()?;
        if self.frame_floor_ms.is_some_and(|floor| {
            frame.source_ms <= floor
                || frame
                    .stream_info()
                    .is_some_and(|stream| stream.callback_ms <= floor)
        }) || self.frame_floor_stamp.as_ref().is_some_and(|floor| {
            frame.watch_id() == Some(floor.stream_id.as_str())
                && frame.source_sequence <= floor.source_sequence
        }) || self
            .execution_frame_floor_ms
            .is_some_and(|floor| frame.source_ms <= floor)
        {
            return None;
        }
        let deadline = self
            .frames
            .content_deadline(max_age_ms)?
            .min(self.frames.heartbeat_deadline()?)
            .min(snapshot.observed_ms.checked_add(max_age_ms)?);
        (now_ms < deadline).then_some(deadline)
    }

    fn frame_matches_structure(&self) -> bool {
        let Some(snapshot) = self.latest.as_ref().map(|env| &env.snapshot) else {
            return false;
        };
        let Some(stream) = self
            .frames
            .latest_content()
            .and_then(|frame| frame.window_geometry())
        else {
            return false;
        };
        focused_window_id(snapshot) == Some(stream.window_id)
            && Self::frame_matches_window(snapshot, stream)
    }

    fn frame_matches_window(
        snapshot: &StructureSnapshot,
        stream: interaction_contracts::desktop::source::FrameWindowGeometry<'_>,
    ) -> bool {
        snapshot.app.pid == stream.pid
            && snapshot.windows.iter().any(|window| {
                window.window_id == stream.window_id && {
                    let a = window.frame;
                    let b = stream.window_frame;
                    [
                        (a.x, b.x),
                        (a.y, b.y),
                        (a.width, b.width),
                        (a.height, b.height),
                    ]
                    .iter()
                    .all(|(a, b)| a.is_finite() && b.is_finite() && (a - b).abs() < 0.5)
                }
            })
    }

    pub fn observation_view(&self, now_ms: u64) -> Option<ObservationView> {
        let frame = self.frames.view(now_ms);
        if self.latest.is_none() && self.health.is_empty() && frame.is_none() {
            return None;
        }
        let mut sources: Vec<SourceHealth> = self.health.values().cloned().collect();
        if let Some(view) = &frame
            && self.require_frames
            && (!view.healthy || self.content_expired)
        {
            let state = if view.healthy {
                SourceState::Degraded
            } else {
                SourceState::Unavailable
            };
            let reason = if view.healthy {
                "captured pixels expired; a new complete frame is required"
            } else {
                match view.status {
                    FrameStatus::Started => "screen stream is waiting for a complete frame",
                    FrameStatus::Stopped => "screen stream stopped",
                    FrameStatus::Suspended => "screen stream suspended",
                    FrameStatus::Blank => "screen stream produced a blank frame",
                    FrameStatus::Complete | FrameStatus::Idle => {
                        "screen stream callback lease expired"
                    }
                }
            };
            let projected = SourceHealth {
                source: SourceKind::Screenshot,
                state,
                reason: Some(reason.into()),
                at_ms: now_ms,
                provenance: None,
            };
            if let Some(old) = sources
                .iter_mut()
                .find(|health| health.source == SourceKind::Screenshot)
            {
                *old = projected;
            } else {
                sources.push(projected);
            }
        }
        Some(ObservationView {
            target_app: self
                .latest
                .as_ref()
                .map(|env| env.snapshot.app.bundle_id.clone()),
            target_name: self
                .latest
                .as_ref()
                .map(|env| env.snapshot.app.name.clone()),
            active_window: self
                .latest
                .as_ref()
                .and_then(|env| focused_window_id(&env.snapshot).map(str::to_owned)),
            observed_target: self.observed_target(now_ms),
            scoped_window_target: self.scoped_window_target(now_ms),
            background_window_target: self.background_window_target(now_ms),
            sources,
            frame,
        })
    }

    /// Project one complete, same-read logical native window context. It is
    /// independent of foreground GlobalFocus and `observed_target`;
    /// the exact Source frame remains the geometry anchor for downstream UI.
    fn background_window_target(&self, now_ms: u64) -> Option<BackgroundWindowTarget> {
        let identity = self.identity.as_ref()?;
        let snapshot = &self.latest.as_ref()?.snapshot;
        if !snapshot.complete
            || self.expected_session != Some(snapshot.session_epoch)
            || identity.session_epoch != snapshot.session_epoch
            || identity.surface != snapshot.surface
            || identity.clock_domain != snapshot.clock_domain
            || identity.clock_domain != format!("macos/{}/wall", identity.session_epoch)
            || snapshot.app.pid == 0
            || snapshot.source_sequence == 0
            || snapshot.source_sequence > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || snapshot.observed_ms > now_ms
            || now_ms.saturating_sub(snapshot.observed_ms) >= 2_000
            || self.structure_unavailable()
            || snapshot.validate().is_err()
        {
            return None;
        }
        let valid_until_ms = snapshot.observed_ms.checked_add(2_000)?;
        if valid_until_ms > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE {
            return None;
        }
        let TargetContext::NativeWindow { app, source } = snapshot.target_context.as_ref()? else {
            return None;
        };
        source.validate(app).ok()?;
        if app != &snapshot.app || !source.is_visible_standard_window() {
            return None;
        }
        let source_window = &source.window;
        if !self.background_ax_usable(
            (&app.bundle_id, app.pid),
            &source_window.origin_window_id,
            Some((
                source_window.process.start_seconds,
                source_window.process.start_micros,
            )),
            Some(source.target_context_hash.as_str()),
            now_ms,
            2_000,
        ) {
            return None;
        }

        let windows: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.window_id == source_window.origin_window_id)
            .collect();
        let [window] = windows.as_slice() else {
            return None;
        };
        if window.kind != interaction_contracts::desktop::source::LayerKind::Window
            || window.modal
            || window.owner_bundle_id != app.bundle_id
            || window.frame != source_window.origin_window_frame
        {
            return None;
        }
        let roots: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.window_id == source_window.origin_window_id && object.parent.is_none()
            })
            .collect();
        let [root] = roots.as_slice() else {
            return None;
        };
        if root.role != "AXWindow"
            || root.subrole.as_deref() != Some("AXStandardWindow")
            || !root.in_view
            || root.secure
            || root.frame != window.frame
        {
            return None;
        }
        let frame = source_window.origin_window_frame;
        let candidate = BackgroundWindowTarget {
            snapshot_id: snapshot.snapshot_id.clone(),
            app: app.bundle_id.clone(),
            pid: app.pid,
            window_id: source_window.origin_window_id.clone(),
            window_title: window.title.clone(),
            frame_points: [frame.x, frame.y, frame.width, frame.height]
                .map(|value| value.round() as i64),
            structure_sequence: snapshot.source_sequence,
            session_epoch: snapshot.session_epoch,
            observed_ms: snapshot.observed_ms,
            valid_until_ms,
            source: source.clone(),
        };
        candidate.validate().ok()?;
        Some(candidate)
    }

    fn observed_target(
        &self,
        now_ms: u64,
    ) -> Option<interaction_contracts::desktop::operator::ObservedTarget> {
        use interaction_contracts::desktop::{operator::ObservedTarget, source::LayerKind};
        let identity = self.identity.as_ref()?;
        if identity.clock_domain != format!("macos/{}/wall", identity.session_epoch)
            || self.structure_unavailable()
        {
            return None;
        }
        let snapshot = &self.latest.as_ref()?.snapshot;
        let valid_until_ms = snapshot.observed_ms.checked_add(2000)?;
        if !snapshot.complete
            || snapshot.app.pid == 0
            || snapshot.source_sequence == 0
            || snapshot.source_sequence > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || now_ms >= valid_until_ms
        {
            return None;
        }
        let eligible = |window: &&interaction_contracts::desktop::source::WindowInfo| {
            !window.modal
                && window.owner_bundle_id == snapshot.app.bundle_id
                && window.kind == LayerKind::Window
                && snapshot.objects.iter().any(|o| {
                    o.window_id == window.window_id
                        && o.parent.is_none()
                        && o.role == "AXWindow"
                        && o.subrole.as_deref() == Some("AXStandardWindow")
                })
        };
        let window = if let Some(active) = focused_window_id(snapshot) {
            snapshot
                .windows
                .iter()
                .filter(eligible)
                .find(|w| w.window_id == active)?
        } else {
            let mut windows = snapshot.windows.iter().filter(eligible);
            let only = windows.next()?;
            if windows.next().is_some() {
                return None;
            }
            only
        };
        Some(ObservedTarget {
            snapshot_id: snapshot.snapshot_id.clone(),
            structure_sequence: snapshot.source_sequence,
            session_epoch: snapshot.session_epoch,
            app: snapshot.app.bundle_id.clone(),
            pid: snapshot.app.pid,
            window_id: window.window_id.clone(),
            window_title: window.title.clone(),
            frame_points: Some(
                [
                    window.frame.x,
                    window.frame.y,
                    window.frame.width,
                    window.frame.height,
                ]
                .map(|v| v.round() as i64),
            ),
            observed_ms: snapshot.observed_ms,
            valid_until_ms,
            contains_private_fields: snapshot
                .objects
                .iter()
                .any(|o| o.window_id == window.window_id && o.secure),
        })
    }

    /// A new source session began. Evidence from the old one proves nothing
    /// about the new one, so it is dropped.
    pub fn set_session(&mut self, epoch: u64) {
        if self.expected_session != Some(epoch) {
            self.expected_session = Some(epoch);
            self.browser.set_authority_epoch(epoch);
            self.last_sequence = None;
            self.latest = None;
            self.refresh_ack_floor = None;
            self.history.clear();
            self.health.clear();
            self.retired_health_streams.clear();
            self.health_stamps.clear();
            self.frames.reset();
            self.owned_document.reset_identity();
            self.structure_floor_ms = None;
            self.frame_floor_ms = None;
            self.frame_floor_stamp = None;
            self.execution_frame_floor_ms = None;
            self.row_structure_floor_ms = None;
            self.content_expired = false;
        }
    }

    /// Install the trusted scope for exactly one browser target/document.
    /// A page snapshot cannot install or renew this authority by itself.
    pub fn install_browser_scope(
        &mut self,
        scope: BrowserReadScope,
        now_ms: u64,
    ) -> Result<(), BrowserReject> {
        self.browser
            .install_scope(scope, now_ms, self.expected_session)
    }

    /// End browser observation authority. A stale or foreign invalidation never
    /// clears a newer scope.
    pub fn invalidate_browser_scope(
        &mut self,
        scope_id: &str,
        generation: u64,
        reason: BrowserSourceInvalidation,
        now_ms: u64,
    ) -> Result<(), BrowserReject> {
        self.browser
            .invalidate_scope(scope_id, generation, reason, now_ms)
    }

    /// Require the next browser page read to start strictly after `floor_ms`
    /// while preserving the installed read scope and latest refresh anchor.
    pub fn require_fresh_browser_read_after(&mut self, floor_ms: u64) {
        self.browser.require_read_after(floor_ms);
    }

    /// Admit one complete page read under the currently installed read scope.
    pub fn admit_browser_snapshot(
        &mut self,
        snapshot: BrowserPageSnapshot,
        now_ms: u64,
    ) -> Result<BrowserAdmitted, BrowserReject> {
        self.browser.admit_snapshot(snapshot, now_ms)
    }

    pub fn browser_latest(&self) -> Option<&BrowserEnvSnapshot> {
        self.browser.latest()
    }

    pub fn browser_scope(&self) -> Option<&BrowserReadScope> {
        self.browser.scope()
    }

    /// Bind a unique, enabled checkbox from the current fresh browser read.
    /// The caller provides the semantic identity; the live binding is scoped to
    /// the installed browser generation and admitted snapshot.
    pub fn browser_checkbox_binding(
        &mut self,
        semantic_target: SemanticTargetRef,
        name: &str,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<BrowserLiveBinding> {
        self.browser
            .checkbox_binding(semantic_target, name, now_ms, max_age_ms)
    }

    pub fn current_browser_binding(&self, binding_ref: &BindingRef) -> Option<&BrowserLiveBinding> {
        self.browser.current_binding(binding_ref)
    }

    pub fn admit(
        &mut self,
        snapshot: StructureSnapshot,
        now_ms: u64,
        pack: &AppPack,
    ) -> Result<Admitted, Reject> {
        let result = self.admit_inner(snapshot, now_ms, pack);
        if result.is_err() {
            self.owned_document.clear_current();
        }
        result
    }

    fn admit_inner(
        &mut self,
        snapshot: StructureSnapshot,
        now_ms: u64,
        pack: &AppPack,
    ) -> Result<Admitted, Reject> {
        snapshot.validate().map_err(Reject::Invalid)?;
        let row_source_unavailable = snapshot.observer_coverage.state == SourceState::Unavailable;
        if let Some(expected) = self.expected_session
            && snapshot.session_epoch != expected
        {
            return Err(Reject::SessionMismatch {
                expected,
                got: snapshot.session_epoch,
            });
        }
        if self.identity.as_ref().is_some_and(|identity| {
            snapshot.surface != identity.surface || snapshot.clock_domain != identity.clock_domain
        }) {
            return Err(Reject::Invalid(
                "snapshot is from another surface or clock".into(),
            ));
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
        if row_source_unavailable {
            self.row_structure_floor_ms = Some(now_ms);
        }
        if self.require_frames
            && self.latest.as_ref().is_some_and(|previous| {
                previous.snapshot.app.pid != snapshot.app.pid
                    || focused_window_id(&previous.snapshot) != focused_window_id(&snapshot)
                    || previous.snapshot.complete != snapshot.complete
                    || focused_window_id(&previous.snapshot).is_some_and(|active| {
                        let before = previous
                            .snapshot
                            .windows
                            .iter()
                            .find(|window| window.window_id == active);
                        let after = snapshot
                            .windows
                            .iter()
                            .find(|window| window.window_id == active);
                        before.map(|window| window.frame) != after.map(|window| window.frame)
                            && self.frames.latest_content().is_none_or(|frame| {
                                frame.window_geometry().is_none_or(|stream| {
                                    focused_window_id(&snapshot) != Some(stream.window_id)
                                        || !Self::frame_matches_window(&snapshot, stream)
                                })
                            })
                    })
            })
        {
            self.execution_frame_floor_ms = Some(now_ms);
            self.structure_floor_ms = Some(now_ms);
            self.row_structure_floor_ms = Some(now_ms);
        }
        let layer_changed = match &self.latest {
            Some(previous) => {
                let active_context = |read: &StructureSnapshot| {
                    focused_window_id(read)
                        .and_then(|id| read.windows.iter().find(|w| w.window_id == id))
                        .map(|w| {
                            (
                                w.window_id.clone(),
                                w.owner_bundle_id.clone(),
                                w.kind,
                                w.modal,
                            )
                        })
                };
                previous.snapshot.app.pid != snapshot.app.pid
                    || previous.snapshot.app.bundle_id != snapshot.app.bundle_id
                    || active_context(&previous.snapshot) != active_context(&snapshot)
                    || match (&previous.snapshot.menu_context, &snapshot.menu_context) {
                        (None, None) => false,
                        (Some(before), Some(after)) => !before.same_identity(after),
                        _ => true,
                    }
            }
            None => true,
        };
        let page = pack.page_of(&snapshot);
        let page_changed = self
            .latest
            .as_ref()
            .is_some_and(|previous| previous.page != page);
        if self.refresh_ack_floor.is_some_and(|(session, sequence)| {
            snapshot.session_epoch == session && snapshot.source_sequence > sequence
        }) {
            self.refresh_ack_floor = None;
        }
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
        let ax_unavailable = self
            .health(SourceKind::Ax)
            .is_some_and(|health| health.state == SourceState::Unavailable);
        if !self.refresh_evidence_pending()
            && !ax_unavailable
            && let Some(current) = self.latest.as_ref()
        {
            self.owned_document.observe_admitted(
                &current.snapshot,
                now_ms,
                self.owned_document_max_age_ms,
            );
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

    pub fn note_health(&mut self, health: SourceHealth, now_ms: u64) -> Result<bool, String> {
        if health.source == SourceKind::Ax && health.state == SourceState::Unavailable {
            self.owned_document.clear_current();
        }
        if health.at_ms > now_ms {
            return Err("source health is from the future".into());
        }
        let key = health_key(health.source);
        if self.require_frames
            && health.provenance.is_none()
            && health.state != SourceState::Unavailable
        {
            return Err("native source health has no provenance".into());
        }
        if let Some(stamp) = &health.provenance
            && (self
                .expected_session
                .is_some_and(|epoch| stamp.session_epoch != epoch)
                || stamp.stream_id.trim().is_empty()
                || stamp.stream_id.len() > 256
                || stamp.stream_id.chars().any(char::is_control)
                || stamp.source_sequence == 0
                || self
                    .retired_health_streams
                    .get(&key)
                    .is_some_and(|streams| streams.contains(&stamp.stream_id)))
        {
            return Err("source health belongs to an old session or stream".into());
        }
        let authoritative_fault = health.state == SourceState::Unavailable
            && (health.provenance.is_none()
                || self.health_stamps.get(&key).is_some_and(|previous| {
                    health.provenance.as_ref().is_some_and(|next| {
                        previous.stream_id == next.stream_id
                            && next.source_sequence > previous.source_sequence
                    })
                }));
        if let Some(previous) = self.health.get(&key) {
            if health.at_ms < previous.at_ms && !authoritative_fault
                || previous.provenance.is_some()
                    && health.at_ms == previous.at_ms
                    && health.provenance.is_none()
                    && health.state == previous.state
            {
                return Err("source health is out of order".into());
            }
            if let (Some(old), Some(next)) = (self.health_stamps.get(&key), &health.provenance) {
                if old.stream_id == next.stream_id && next.source_sequence <= old.source_sequence {
                    return Err("source health sequence was replayed".into());
                }
                if old.stream_id != next.stream_id {
                    let retired = self.retired_health_streams.entry(key.clone()).or_default();
                    if retired.len() >= 128 {
                        return Err("source health restart budget exhausted".into());
                    }
                    retired.insert(old.stream_id.clone());
                }
            }
            if previous.provenance.is_none()
                && previous.state == SourceState::Unavailable
                && health.state != SourceState::Unavailable
                && health.at_ms <= previous.at_ms
            {
                return Err("source recovery predates the loss".into());
            }
        }
        let intentional_scoped_row_omission = health.source == SourceKind::Ax
            && health.state == SourceState::Degraded
            && health.reason.as_deref() == Some(AX_SCOPED_ROW_OMISSION_REASON);
        let failed = health.state != SourceState::Available;
        let invalidates = failed
            && (health.source == SourceKind::Ax
                || self.require_frames && health.source == SourceKind::Screenshot);
        if invalidates {
            self.structure_floor_ms = Some(now_ms);
            if health.source == SourceKind::Ax && health.state == SourceState::Unavailable
                || health.source == SourceKind::Screenshot && self.require_frames
            {
                self.row_structure_floor_ms = Some(now_ms.max(health.at_ms));
            }
            if health.source == SourceKind::Screenshot {
                self.frames.invalidate();
                self.frame_floor_ms = Some(now_ms);
                self.frame_floor_stamp = health.provenance.clone();
            }
        }
        let mut health = health;
        if authoritative_fault && let Some(previous) = self.health.get(&key) {
            // A fault closes authority even when the helper's wall clock
            // moved backwards. Its ordering floor must not move backwards.
            health.at_ms = health.at_ms.max(previous.at_ms);
        }
        if let Some(stamp) = &health.provenance {
            self.health_stamps.insert(key.clone(), stamp.clone());
        }
        health.reason = health
            .reason
            .map(|reason| reason.chars().take(1_024).collect());
        if health.source == SourceKind::Ax
            && health.state != SourceState::Available
            && !intentional_scoped_row_omission
        {
            self.ax_metadata_floor_ms = Some(now_ms.max(health.at_ms));
        }
        self.health.insert(key, health);
        Ok(invalidates)
    }

    pub fn health(&self, kind: SourceKind) -> Option<&SourceHealth> {
        self.health.get(&health_key(kind))
    }

    /// The newest accepted stream stamp for this source, retained across an
    /// Unavailable update that carries no provenance.
    pub(crate) fn health_stamp(&self, kind: SourceKind) -> Option<&SourceStamp> {
        self.health_stamps.get(&health_key(kind))
    }

    /// Install Host-private startup registration. Invalid identity is treated
    /// as absent, so no Source proof can establish an owned binding.
    pub fn install_owned_document_registration(
        &mut self,
        registration: Option<Box<OwnedDocumentRegistration>>,
        max_age_ms: u64,
    ) {
        self.owned_document.install_registration(registration);
        self.owned_document_max_age_ms = max_age_ms;
    }

    /// Drop current private evidence while preserving identity high-water state.
    pub fn clear_owned_document_binding(&mut self) {
        self.owned_document.clear_current();
    }

    pub fn expire_owned_document_binding(&mut self, now_ms: u64, max_age_ms: u64) {
        self.owned_document.expire(now_ms, max_age_ms);
    }

    pub fn owned_document_binding(
        &self,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<&OwnedDocumentBinding> {
        if self.refresh_evidence_pending() {
            return None;
        }
        self.owned_document.get(
            self.latest.as_ref().map(|env| &env.snapshot),
            now_ms,
            max_age_ms,
        )
    }

    /// The structure source cannot be relied on right now.
    pub fn structure_unavailable(&self) -> bool {
        self.health(SourceKind::Ax)
            .is_some_and(|h| h.state != SourceState::Available)
            || self
                .latest
                .as_ref()
                .is_some_and(|env| env.snapshot.observer_coverage.state == SourceState::Unavailable)
    }

    /// Fresh enough, complete, and from a source that works.
    pub fn usable(&self, now_ms: u64, max_age_ms: u64) -> bool {
        if self.refresh_evidence_pending() {
            return false;
        }
        !self.structure_unavailable()
            && self.latest.as_ref().is_some_and(|env| {
                env.snapshot.complete
                    && env.snapshot.menu_context.is_none()
                    && focused_window_id(&env.snapshot).is_some()
                    && self
                        .structure_floor_ms
                        .is_none_or(|floor| env.snapshot.observed_ms > floor)
                    && now_ms.saturating_sub(env.snapshot.observed_ms) <= max_age_ms
            })
            && (!self.require_frames
                || self.frame_deadline(now_ms, max_age_ms).is_some()
                    && self.frame_matches_structure())
    }

    /// AX menu input has its own explicit evidence domain. The original
    /// window stream still needs to be healthy and match the window; these
    /// pixels do not claim to include its separately drawn menu.
    pub fn menu_usable(&self, now_ms: u64, max_age_ms: u64) -> bool {
        if self.refresh_evidence_pending() || self.structure_unavailable() {
            return false;
        }
        let Some(env) = self.latest.as_ref() else {
            return false;
        };
        let snapshot = &env.snapshot;
        snapshot.menu_context.is_some()
            && snapshot.complete
            && focused_window_id(snapshot).is_some()
            && snapshot.observed_ms <= now_ms
            && now_ms.saturating_sub(snapshot.observed_ms) <= max_age_ms
            && self
                .structure_floor_ms
                .is_none_or(|floor| snapshot.observed_ms > floor)
            && snapshot.validate().is_ok()
            && (!self.require_frames || self.menu_frame_deadline(now_ms, max_age_ms).is_some())
    }

    /// A menu AX proof uses the current root stream callback and geometry,
    /// with an explicit content-age bound. It cannot repair a source loss.
    pub fn menu_frame_deadline(&self, now_ms: u64, max_age_ms: u64) -> Option<u64> {
        let snapshot = &self.latest.as_ref()?.snapshot;
        let context = snapshot.menu_context.as_ref()?;
        if self.refresh_evidence_pending()
            || !snapshot.complete
            || self.structure_unavailable()
            || !self.frames.healthy(now_ms)
            || !self.frame_matches_structure()
            || self
                .structure_floor_ms
                .is_some_and(|floor| snapshot.observed_ms <= floor)
            || self.frame_floor_ms.is_some_and(|floor| {
                self.frames
                    .latest_content()
                    .is_none_or(|frame| frame.source_ms <= floor)
            })
            || focused_window_id(snapshot) != Some(context.opener.origin_window_id.as_str())
        {
            return None;
        }
        let deadline = self
            .frames
            .content_deadline(max_age_ms)?
            .min(self.frames.heartbeat_deadline()?)
            .min(snapshot.observed_ms.checked_add(max_age_ms)?);
        (now_ms < deadline).then_some(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::device::{DeviceIdentity, SurfaceRef};
    use interaction_contracts::desktop::ids::{ObjectRef, RequestId};
    use interaction_contracts::desktop::source::{
        AxEnabledObservation, FrameProvenance, FrameStreamInfo, KernelProcessIdentity, LayerKind,
        ObserverCoverage, Rect, SourceWindowContext, TableViewport, TableViewportColumn,
    };
    use interaction_contracts::desktop::table_scope::{
        TableScopeCandidate, TableScopePathElement, TableScopeSearch, TableScopeSearchState,
    };
    use interaction_contracts::desktop::task::{
        RowContainerRole, RowContainerScope, RowTableScope,
    };
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

    fn scoped_source_snapshot() -> StructureSnapshot {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json"
        )))
        .expect("the test-only scoped-window synthetic fixture is valid")
    }

    fn state_for_scoped_source(snapshot: &StructureSnapshot) -> StateCore {
        let identity = DeviceIdentity {
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            dependencies: BTreeMap::new(),
        };
        let mut state = StateCore::new();
        state.set_identity(&identity, false);
        state
    }

    fn state_with_scoped_source() -> (StateCore, StructureSnapshot) {
        let snapshot = scoped_source_snapshot();
        let pack = AppPack::registered_native(&snapshot.app.bundle_id)
            .expect("the replay fixture uses a registered public app profile");
        let mut state = state_for_scoped_source(&snapshot);
        state
            .admit(snapshot.clone(), snapshot.observed_ms, &pack)
            .expect("the test-only synthetic snapshot fixture is admitted");
        (state, snapshot)
    }

    fn native_ax_health(
        state: SourceState,
        reason: &str,
        at_ms: u64,
        health_sequence: u64,
    ) -> SourceHealth {
        SourceHealth {
            source: SourceKind::Ax,
            state,
            reason: Some(reason.into()),
            at_ms,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "native-health-stream".into(),
                source_sequence: health_sequence,
            }),
        }
    }

    fn assert_scoped_source_edit_is_not_projected(edit: impl FnOnce(&mut StructureSnapshot)) {
        let (mut state, snapshot) = state_with_scoped_source();
        edit(&mut state.latest.as_mut().unwrap().snapshot);
        assert!(
            state
                .observation_view(snapshot.observed_ms)
                .unwrap()
                .scoped_window_target
                .is_none()
        );
    }

    fn row_scope() -> RowTableScope {
        RowTableScope {
            scroll_path: vec![RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Workspace".into(),
            }],
            column_headers: vec!["Name".into()],
        }
    }

    fn stream_frame(
        snapshot: &StructureSnapshot,
        sequence: u64,
        source_ms: u64,
        callback_ms: u64,
    ) -> FrameMeta {
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let window_frame = snapshot
            .windows
            .iter()
            .find(|window| window.window_id == window_id)
            .unwrap()
            .frame;
        FrameMeta {
            frame_id: format!("row-frame-{sequence}"),
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence: sequence,
            source_ms,
            status: FrameStatus::Complete,
            geometry_revision: 1,
            width_px: 100,
            height_px: 100,
            scale_factor: 1.0,
            byte_length: 100,
            checksum: format!("sha256:row-frame-{sequence}"),
            provenance: Some(FrameProvenance::Stream {
                stream: FrameStreamInfo {
                    stream_id: "row-screen-stream".into(),
                    image_frame_id: Some(format!("row-image-{sequence}")),
                    display_time_ticks: source_ms * 1_000_000,
                    display_time_ns: source_ms * 1_000_000,
                    received_uptime_ns: callback_ms * 1_000_000,
                    received_unix_ms: 100_000 + callback_ms,
                    callback_ms,
                    source_clock: "mach_absolute".into(),
                    window_id,
                    pid: snapshot.app.pid,
                    window_frame,
                    content_rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: window_frame.width,
                        height: window_frame.height,
                    },
                    content_scale: 1.0,
                    dirty_rects: vec![],
                    dropped_frames: 0,
                    capture_interval_ms: 100,
                },
            }),
        }
    }

    fn native_row_core(
        timeout_ms: u64,
    ) -> (StateCore, RowTableScope, StructureSnapshot, FrameMeta) {
        let scope = row_scope();
        let snapshot = scoped_partial_snapshot(1, 10, &scope, TableScopeSearchState::Complete);
        let identity = DeviceIdentity {
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            dependencies: BTreeMap::new(),
        };
        let mut core = StateCore::with_frame_timeout(timeout_ms);
        core.set_session(snapshot.session_epoch);
        core.set_identity(&identity, true);
        core.admit(
            snapshot.clone(),
            snapshot.observed_ms,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        let frame = stream_frame(&snapshot, 1, 10, 11);
        (core, scope, snapshot, frame)
    }

    fn scoped_partial_snapshot(
        sequence: u64,
        observed_ms: u64,
        scope: &RowTableScope,
        state: TableScopeSearchState,
    ) -> StructureSnapshot {
        let mut snapshot = SimApp::new().partial_snapshot(&meta(sequence, observed_ms), 3);
        let window_frame = snapshot.windows[0].frame;
        let root = snapshot
            .objects
            .iter_mut()
            .find(|object| object.parent.is_none())
            .unwrap();
        root.role = "AXWindow".into();
        root.subrole = Some("AXStandardWindow".into());
        root.frame = window_frame;
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let process_id = snapshot.app.pid;
        snapshot.table_scope_search = Some(TableScopeSearch {
            requested_scope: scope.clone(),
            process_id,
            window_id,
            state,
            candidates: Vec::new(),
        });
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot
            .validate()
            .expect("the test's local census is valid");
        snapshot
    }

    fn partial_window_snapshot(
        sequence: u64,
        observed_ms: u64,
        scope: &RowTableScope,
    ) -> StructureSnapshot {
        let mut snapshot = scoped_partial_snapshot(
            sequence,
            observed_ms,
            scope,
            TableScopeSearchState::Complete,
        );
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let window_frame = snapshot.windows[0].frame;
        let app_bundle = snapshot.app.bundle_id.clone();
        let process_id = snapshot.app.pid;
        let root_ref = ObjectRef::new("partial-window-root").unwrap();
        let scroll_ref = ObjectRef::new("partial-window-scroll").unwrap();
        let table_ref = ObjectRef::new("partial-window-table").unwrap();
        let column_ref = ObjectRef::new("partial-window-column").unwrap();
        let root_template = snapshot
            .objects
            .iter()
            .find(|object| object.parent.is_none())
            .unwrap()
            .clone();

        let mut root = root_template.clone();
        root.instance_ref = root_ref.clone();
        root.parent = None;
        root.window_id = window_id.clone();
        root.role = "AXWindow".into();
        root.subrole = Some("AXStandardWindow".into());
        root.frame = window_frame;
        root.in_view = true;
        root.secure = false;

        let scroll_frame = Rect {
            x: window_frame.x + 20.0,
            y: window_frame.y + 20.0,
            width: window_frame.width - 40.0,
            height: window_frame.height - 40.0,
        };
        let table_frame = Rect {
            height: scroll_frame.height + 80.0,
            ..scroll_frame
        };
        let column_frame = Rect {
            height: 24.0,
            ..scroll_frame
        };

        let mut scroll = root_template.clone();
        scroll.instance_ref = scroll_ref.clone();
        scroll.parent = Some(root_ref.clone());
        scroll.window_id = window_id.clone();
        scroll.role = "AXScrollArea".into();
        scroll.subrole = None;
        scroll.description = Some(scope.scroll_path[0].label.clone());
        scroll.frame = scroll_frame;
        scroll.enabled = true;
        scroll.in_view = true;
        scroll.secure = false;

        let mut table = root_template.clone();
        table.instance_ref = table_ref.clone();
        table.parent = Some(scroll_ref.clone());
        table.window_id = window_id.clone();
        table.role = "AXTable".into();
        table.subrole = None;
        table.description = None;
        table.frame = table_frame;
        table.enabled = true;
        table.in_view = true;
        table.secure = false;

        let mut column = root_template;
        column.instance_ref = column_ref.clone();
        column.parent = Some(table_ref.clone());
        column.window_id = window_id.clone();
        column.role = "AXColumn".into();
        column.subrole = None;
        column.description = None;
        column.frame = column_frame;
        column.enabled = true;
        column.in_view = true;
        column.secure = false;

        snapshot.objects = vec![root, scroll, table, column];
        snapshot.table_viewports = vec![TableViewport {
            table_instance_ref: table_ref.clone(),
            scroll_container_instance_ref: scroll_ref.clone(),
            process_id,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: scroll_frame,
            visible_rows_complete: true,
            visible_rows: vec![],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(column_ref),
                parent_instance_ref: table_ref.clone(),
                frame: column_frame,
                header_instance_ref: None,
                header: scope.column_headers.first().cloned(),
                ax_index: Some(0),
            }],
        }];
        snapshot.table_scope_search = Some(TableScopeSearch {
            requested_scope: scope.clone(),
            process_id,
            window_id: window_id.clone(),
            state: TableScopeSearchState::Complete,
            candidates: vec![TableScopeCandidate {
                table_instance_ref: table_ref,
                scroll_container_instance_ref: scroll_ref.clone(),
                scroll_path: vec![TableScopePathElement {
                    instance_ref: scroll_ref,
                    label: scope.scroll_path[0].label.clone(),
                    frame: scroll_frame,
                }],
                column_headers: scope.column_headers.clone(),
            }],
        });
        snapshot.windows[0].kind = LayerKind::Window;
        snapshot.windows[0].modal = false;
        snapshot.windows[0].owner_bundle_id = app_bundle;
        snapshot.window_context = Some(SourceWindowContext {
            source_session_id: "partial-window-test-session".into(),
            session_epoch: snapshot.session_epoch,
            process: KernelProcessIdentity {
                pid: process_id,
                start_seconds: 100,
                start_micros: 200,
            },
            origin_window_id: window_id,
            origin_window_frame: window_frame,
            verified_source_sequence: sequence,
            verified_observed_ms: observed_ms,
        });
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot
            .validate()
            .expect("partial window fixture carries one exact same-read table scope");
        snapshot
    }

    #[test]
    fn a_new_snapshot_cannot_overtake_a_pending_refresh_transition() {
        let mut core = core();
        let app = SimApp::new();
        let pack = AppPack::settings_fixture();
        core.admit(app.snapshot(&meta(1, 10)), 10, &pack).unwrap();
        assert!(core.usable(10, 500));
        let first = core.begin_refresh().unwrap();
        core.admit(app.snapshot(&meta(2, 20)), 20, &pack).unwrap();
        assert!(!core.usable(20, 500));
        let second = core.begin_refresh().unwrap();
        assert!(second > first);
        assert!(!core.refresh_started(first));
        assert!(!core.usable(20, 500));
        assert!(core.refresh_started(second));
        assert!(!core.usable(20, 500));
        assert!(
            !core.refresh_pending(),
            "ACK is complete; new evidence is still missing"
        );
        core.admit(app.snapshot(&meta(3, 21)), 21, &pack).unwrap();
        assert!(!core.refresh_pending());
        assert!(core.usable(21, 500));
        assert!(!core.refresh_started(second));
    }

    #[test]
    fn a_device_identity_change_does_not_confirm_a_source_transition() {
        let mut core = core();
        let first = core.begin_refresh().unwrap();
        let id = DeviceIdentity {
            session_epoch: 2,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 2,
            },
            clock_domain: "sim/2/monotonic".into(),
            dependencies: BTreeMap::new(),
        };
        core.set_identity(&id, false);
        assert!(core.refresh_pending());
        let second = core.begin_refresh().unwrap();
        assert!(second > first);
        assert!(!core.refresh_started(first));
        assert!(core.refresh_pending());
        assert!(core.refresh_started(second));
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
    fn a_partial_complete_scope_is_local_authority_even_while_generic_use_stays_closed() {
        let scope = row_scope();
        let mut core = core();
        let snapshot = scoped_partial_snapshot(1, 10, &scope, TableScopeSearchState::Complete);
        core.admit(snapshot, 10, &AppPack::settings_fixture())
            .unwrap();
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Degraded,
                reason: Some("bounded global walk".into()),
                at_ms: 10,
                provenance: None,
            },
            10,
        )
        .unwrap();

        assert!(core.row_scope_usable(&scope, 10, 1_000));
        assert!(core.row_scope_frame_deadline(&scope, 10, 1_000).is_none());
        assert!(
            !core.usable(10, 1_000),
            "a scoped census never makes the globally partial Source usable"
        );
    }

    #[test]
    fn an_available_complete_tree_keeps_the_existing_row_scope_path() {
        let scope = row_scope();
        let mut core = core();
        core.admit(
            SimApp::new().snapshot(&meta(1, 10)),
            10,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Available,
                reason: None,
                at_ms: 10,
                provenance: None,
            },
            10,
        )
        .unwrap();

        assert!(core.row_scope_usable(&scope, 10, 1_000));
        assert!(core.usable(10, 1_000));
    }

    #[test]
    fn row_scope_recovers_from_ax_unavailable_only_with_a_strictly_new_scoped_read() {
        let scope = row_scope();
        let mut core = core();
        core.admit(
            scoped_partial_snapshot(1, 10, &scope, TableScopeSearchState::Complete),
            10,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Unavailable,
                reason: Some("observer lost".into()),
                at_ms: 11,
                provenance: None,
            },
            11,
        )
        .unwrap();
        assert!(!core.row_scope_usable(&scope, 11, 1_000));

        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Degraded,
                reason: Some("bounded global walk".into()),
                at_ms: 12,
                provenance: None,
            },
            12,
        )
        .unwrap();
        assert!(
            !core.row_scope_usable(&scope, 12, 1_000),
            "health recovery cannot revive the pre-loss local certificate"
        );

        core.admit(
            scoped_partial_snapshot(2, 12, &scope, TableScopeSearchState::Complete),
            12,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        assert!(core.row_scope_usable(&scope, 12, 1_000));
        assert!(!core.usable(12, 1_000));
    }

    #[test]
    fn ax_degraded_does_not_authorize_an_old_complete_snapshot_without_a_scope_census() {
        let scope = row_scope();
        let mut core = core();
        core.admit(
            SimApp::new().snapshot(&meta(1, 10)),
            10,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Degraded,
                reason: Some("bounded global walk".into()),
                at_ms: 11,
                provenance: None,
            },
            11,
        )
        .unwrap();

        assert!(!core.row_scope_usable(&scope, 11, 1_000));
        assert!(!core.usable(11, 1_000));
    }

    #[test]
    fn row_scope_rejects_mismatch_incomplete_stale_and_pending_refresh_evidence() {
        let scope = row_scope();
        let mut core = core();
        core.admit(
            scoped_partial_snapshot(1, 10, &scope, TableScopeSearchState::Complete),
            10,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        assert!(core.row_scope_usable(&scope, 10, 1_000));
        assert!(!core.row_scope_usable(&scope, 1_010, 1_000));
        assert!(!core.row_scope_usable(&scope, 10, 10_001));

        let mut other_scope = scope.clone();
        other_scope.column_headers[0] = "Owner".into();
        assert!(!core.row_scope_usable(&other_scope, 10, 1_000));

        let mut incomplete_core = StateCore::new();
        incomplete_core.set_session(1);
        incomplete_core
            .admit(
                scoped_partial_snapshot(
                    1,
                    10,
                    &scope,
                    TableScopeSearchState::Incomplete {
                        reason: "local census limit".into(),
                    },
                ),
                10,
                &AppPack::settings_fixture(),
            )
            .unwrap();
        assert!(!incomplete_core.row_scope_usable(&scope, 10, 1_000));

        let refresh = core.begin_refresh().unwrap();
        assert!(!core.row_scope_usable(&scope, 10, 1_000));
        assert!(core.refresh_started(refresh));
        assert!(!core.row_scope_usable(&scope, 10, 1_000));
        core.admit(
            scoped_partial_snapshot(2, 11, &scope, TableScopeSearchState::Complete),
            11,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        assert!(core.row_scope_usable(&scope, 11, 1_000));
    }

    #[test]
    fn native_row_scope_never_invents_a_frame_when_pixels_are_missing() {
        let scope = row_scope();
        let mut core = core();
        core.set_identity(
            &DeviceIdentity {
                session_epoch: 1,
                surface: SurfaceRef {
                    id: "main".into(),
                    epoch: 1,
                },
                clock_domain: "sim/1/monotonic".into(),
                dependencies: BTreeMap::new(),
            },
            true,
        );
        core.admit(
            scoped_partial_snapshot(1, 10, &scope, TableScopeSearchState::Complete),
            10,
            &AppPack::settings_fixture(),
        )
        .unwrap();

        assert!(!core.row_scope_usable(&scope, 10, 1_000));
        assert!(core.row_scope_frame_deadline(&scope, 10, 1_000).is_none());
        assert!(!core.usable(10, 1_000));
    }

    #[test]
    fn native_partial_row_scope_gets_a_frame_lease_without_generic_frame_authority() {
        let (mut core, scope, _, frame) = native_row_core(2_000);
        core.admit_frame(frame, 11, 1_000).unwrap();

        assert_eq!(
            core.row_scope_frame_deadline(&scope, 12, 1_000),
            Some(1_010),
            "the lease is capped by the original image and Source-read ages"
        );
        assert!(core.row_scope_usable(&scope, 12, 1_000));
        assert!(
            core.frame_deadline(12, 1_000).is_none(),
            "a scoped lease does not open the generic incomplete-tree frame gate"
        );
        assert!(!core.usable(12, 1_000));
    }

    #[test]
    fn native_row_scope_frame_lease_rejects_pid_window_and_geometry_mismatches() {
        let (mut wrong_pid, scope, _, frame) = native_row_core(2_000);
        let mut frame = frame;
        frame.stream_info_mut().unwrap().pid += 1;
        assert!(wrong_pid.admit_frame(frame, 11, 1_000).is_err());
        assert!(
            wrong_pid
                .row_scope_frame_deadline(&scope, 12, 1_000)
                .is_none()
        );

        let (mut wrong_window, scope, _, mut frame) = native_row_core(2_000);
        frame.stream_info_mut().unwrap().window_id = "unobserved-window".into();
        assert!(wrong_window.admit_frame(frame, 11, 1_000).is_err());
        assert!(
            wrong_window
                .row_scope_frame_deadline(&scope, 12, 1_000)
                .is_none()
        );

        let (mut wrong_geometry, scope, _, frame) = native_row_core(2_000);
        let stream = wrong_geometry
            .latest()
            .unwrap()
            .snapshot
            .global_focus
            .known_window_for_app(&wrong_geometry.latest().unwrap().snapshot.app)
            .unwrap()
            .to_owned();
        let expected_window = wrong_geometry
            .latest()
            .unwrap()
            .snapshot
            .windows
            .iter()
            .find(|window| window.window_id == stream)
            .unwrap()
            .frame;
        let mut frame = frame;
        frame.stream_info_mut().unwrap().window_frame.x += 1.0;
        assert_eq!(
            frame.stream_info().unwrap().window_frame,
            Rect {
                x: expected_window.x + 1.0,
                ..expected_window
            }
        );
        wrong_geometry.admit_frame(frame, 11, 1_000).unwrap();
        assert!(
            wrong_geometry
                .row_scope_frame_deadline(&scope, 12, 1_000)
                .is_none(),
            "valid frame metadata with different window geometry cannot match this Source read"
        );
    }

    #[test]
    fn screenshot_loss_expiry_and_heartbeat_cannot_reuse_old_row_frames() {
        let (mut unavailable, scope, _, frame) = native_row_core(2_000);
        unavailable.admit_frame(frame.clone(), 11, 1_000).unwrap();
        unavailable
            .note_health(
                SourceHealth {
                    source: SourceKind::Screenshot,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 11,
                    provenance: Some(SourceStamp {
                        session_epoch: 1,
                        stream_id: "row-screen-stream".into(),
                        source_sequence: 1,
                    }),
                },
                11,
            )
            .unwrap();
        assert!(
            unavailable
                .row_scope_frame_deadline(&scope, 12, 1_000)
                .is_some()
        );
        unavailable
            .note_health(
                SourceHealth {
                    source: SourceKind::Screenshot,
                    state: SourceState::Unavailable,
                    reason: Some("capture stream stopped".into()),
                    at_ms: 13,
                    provenance: Some(SourceStamp {
                        session_epoch: 1,
                        stream_id: "row-screen-stream".into(),
                        source_sequence: 2,
                    }),
                },
                13,
            )
            .unwrap();
        assert!(
            unavailable
                .row_scope_frame_deadline(&scope, 13, 1_000)
                .is_none()
        );
        unavailable
            .note_health(
                SourceHealth {
                    source: SourceKind::Screenshot,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 14,
                    provenance: Some(SourceStamp {
                        session_epoch: 1,
                        stream_id: "row-screen-stream".into(),
                        source_sequence: 3,
                    }),
                },
                14,
            )
            .unwrap();
        unavailable
            .admit(
                scoped_partial_snapshot(2, 14, &scope, TableScopeSearchState::Complete),
                14,
                &AppPack::settings_fixture(),
            )
            .unwrap();
        assert!(
            unavailable
                .row_scope_frame_deadline(&scope, 14, 1_000)
                .is_none(),
            "healthy status cannot resurrect content invalidated by Source loss"
        );
        assert!(unavailable.admit_frame(frame, 15, 1_000).is_err());

        let (mut expired, scope, _, frame) = native_row_core(2_000);
        expired.admit_frame(frame.clone(), 11, 1_000).unwrap();
        assert_eq!(
            expired.expire_sources(1_010, 1_000),
            Some("the captured pixels expired; a new complete frame is required")
        );
        assert!(
            expired
                .row_scope_frame_deadline(&scope, 1_010, 1_000)
                .is_none()
        );
        assert!(expired.admit_frame(frame, 1_011, 1_000).is_err());

        let (mut heartbeat, scope, _, frame) = native_row_core(100);
        heartbeat.admit_frame(frame, 11, 1_000).unwrap();
        assert_eq!(
            heartbeat.expire_sources(111, 1_000),
            Some("the screen stream stopped reporting callbacks")
        );
        assert!(
            heartbeat
                .row_scope_frame_deadline(&scope, 111, 1_000)
                .is_none()
        );
    }

    #[test]
    fn execution_frame_floor_requires_new_pixels_after_partial_tree_transition() {
        let scope = row_scope();
        let pack = AppPack::settings_fixture();
        let first = SimApp::new().snapshot(&meta(1, 10));
        let identity = DeviceIdentity {
            session_epoch: first.session_epoch,
            surface: first.surface.clone(),
            clock_domain: first.clock_domain.clone(),
            dependencies: BTreeMap::new(),
        };
        let mut core = StateCore::new();
        core.set_session(first.session_epoch);
        core.set_identity(&identity, true);
        core.admit(first.clone(), 10, &pack).unwrap();
        let old_frame = stream_frame(&first, 1, 10, 11);
        core.admit_frame(old_frame, 11, 1_000).unwrap();

        core.admit(
            scoped_partial_snapshot(2, 20, &scope, TableScopeSearchState::Complete),
            20,
            &pack,
        )
        .unwrap();
        core.admit(
            scoped_partial_snapshot(3, 21, &scope, TableScopeSearchState::Complete),
            21,
            &pack,
        )
        .unwrap();

        assert!(!core.row_scope_usable(&scope, 21, 1_000));
        assert!(
            core.row_scope_frame_deadline(&scope, 21, 1_000).is_none(),
            "an old image cannot cross the execution floor created by the completeness transition"
        );
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
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Unavailable,
                reason: Some("timeouts".into()),
                at_ms: 11,
                provenance: None,
            },
            11,
        )
        .unwrap();
        assert!(!core.usable(11, 100));
        core.note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Available,
                reason: None,
                at_ms: 12,
                provenance: None,
            },
            12,
        )
        .unwrap();
        assert!(
            !core.usable(12, 100),
            "health alone cannot revive an old read"
        );
        core.admit(SimApp::new().snapshot(&meta(2, 13)), 13, &pack)
            .unwrap();
        assert!(core.usable(13, 100));
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

    #[test]
    fn identity_boundary_changes_are_distinct_from_source_sequence() {
        let pack = AppPack::settings_fixture();
        let app = SimApp::new();
        let mut core = core();
        let first_boundary = SourceIdentityBoundary {
            namespace: "11111111-1111-4111-8111-111111111111".into(),
            generation: 4,
        };
        let mut first = app.snapshot(&meta(1, 10));
        first.identity_boundary = Some(first_boundary.clone());
        assert!(!core.identity_boundary_changed(first.identity_boundary.as_ref()));
        core.admit(first, 10, &pack).unwrap();

        let mut same_boundary = app.snapshot(&meta(2, 20));
        same_boundary.identity_boundary = Some(first_boundary.clone());
        assert!(!core.identity_boundary_changed(same_boundary.identity_boundary.as_ref()));
        core.admit(same_boundary, 20, &pack).unwrap();

        let mut changed_generation = app.snapshot(&meta(3, 30));
        changed_generation.identity_boundary = Some(SourceIdentityBoundary {
            generation: 5,
            ..first_boundary.clone()
        });
        assert!(core.identity_boundary_changed(changed_generation.identity_boundary.as_ref()));

        let mut changed_namespace = app.snapshot(&meta(3, 30));
        changed_namespace.identity_boundary = Some(SourceIdentityBoundary {
            namespace: "22222222-2222-4222-8222-222222222222".into(),
            generation: 1,
        });
        assert!(core.identity_boundary_changed(changed_namespace.identity_boundary.as_ref()));
    }

    #[test]
    fn native_missing_identity_boundary_is_unknown_after_first_baseline() {
        let mut core = core();
        let identity = DeviceIdentity {
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            dependencies: Default::default(),
        };
        core.set_identity(&identity, true);
        assert!(!core.identity_boundary_changed(None));
        let pack = AppPack::settings_fixture();
        let mut first = SimApp::new().snapshot(&meta(1, 10));
        first.clock_domain = identity.clock_domain.clone();
        core.admit(first, 10, &pack).unwrap();
        assert!(core.identity_boundary_changed(None));
    }

    #[test]
    fn activation_metadata_is_scoped_fresh_and_does_not_make_background_editing_usable() {
        let pack = AppPack::settings_fixture();
        let mut snapshot = SimApp::new().snapshot(&meta(1, 10));
        crate::testkit::focus_window_absent(&mut snapshot);
        let window = snapshot.windows[0].window_id.clone();
        let app = snapshot.app.bundle_id.clone();
        let pid = snapshot.app.pid;
        let root = snapshot
            .objects
            .iter_mut()
            .find(|o| o.parent.is_none())
            .unwrap();
        root.role = "AXWindow".into();
        root.subrole = Some("AXStandardWindow".into());
        root.enabled = false;
        root.frame = snapshot.windows[0].frame;
        let mut state = core();
        state.admit(snapshot.clone(), 10, &pack).unwrap();
        assert!(state.window_root(&app, pid, &window, 10, 2000).is_some());
        assert!(!state.usable(10, 2000));
        assert!(
            state
                .window_root(&app, pid + 1, &window, 10, 2000)
                .is_none()
        );
        assert!(state.window_root(&app, pid, &window, 2010, 2000).is_none());
        state
            .note_health(
                SourceHealth {
                    source: SourceKind::Ax,
                    state: SourceState::Unavailable,
                    reason: None,
                    at_ms: 11,
                    provenance: None,
                },
                11,
            )
            .unwrap();
        state
            .note_health(
                SourceHealth {
                    source: SourceKind::Ax,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 12,
                    provenance: None,
                },
                12,
            )
            .unwrap();
        assert!(
            state.window_root(&app, pid, &window, 12, 2000).is_none(),
            "health alone cannot revive stale activation metadata"
        );
        snapshot.source_sequence = 2;
        snapshot.observed_ms = 13;
        state.admit(snapshot.clone(), 13, &pack).unwrap();
        assert!(state.window_root(&app, pid, &window, 13, 2000).is_some());
        snapshot.windows[0].modal = true;
        snapshot.source_sequence = 3;
        snapshot.observed_ms = 14;
        state.admit(snapshot, 14, &pack).unwrap();
        assert!(state.window_root(&app, pid, &window, 14, 2000).is_none());
    }

    #[test]
    fn partial_row_window_root_is_readonly_and_requires_the_exact_fresh_scope() {
        let pack = AppPack::settings_fixture();
        let scope = row_scope();
        let snapshot = partial_window_snapshot(1, 10, &scope);
        let app = snapshot.app.bundle_id.clone();
        let pid = snapshot.app.pid;
        let window = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let mut state = core();
        state.admit(snapshot, 10, &pack).unwrap();

        let root = state
            .row_window_root(&scope, &app, pid, &window, 10, 2_000)
            .expect("a current unique scoped window root is available for read-only metadata");
        assert_eq!(root.role, "AXWindow");
        assert!(
            state.window_root(&app, pid, &window, 10, 2_000).is_none(),
            "partial scoped metadata must not enter the complete-tree activation path"
        );

        let mut other_scope = scope.clone();
        other_scope.column_headers = vec!["Other".into()];
        assert!(
            state
                .row_window_root(&other_scope, &app, pid, &window, 10, 2_000)
                .is_none()
        );
        assert!(
            state
                .row_window_root(&scope, "other.app", pid, &window, 10, 2_000)
                .is_none()
        );
        assert!(
            state
                .row_window_root(&scope, &app, pid + 1, &window, 10, 2_000)
                .is_none()
        );
        assert!(
            state
                .row_window_root(&scope, &app, pid, "other-window", 10, 2_000)
                .is_none()
        );
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 2_010, 2_000)
                .is_none()
        );

        state.begin_refresh().unwrap();
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 11, 2_000)
                .is_none(),
            "an in-flight refresh revokes the scoped window read"
        );
    }

    #[test]
    fn partial_row_window_root_requires_source_context_and_a_post_fault_read() {
        let pack = AppPack::settings_fixture();
        let scope = row_scope();
        let mut without_context = partial_window_snapshot(1, 10, &scope);
        without_context.window_context = None;
        let app = without_context.app.bundle_id.clone();
        let pid = without_context.app.pid;
        let window = without_context
            .global_focus
            .known_window_for_app(&without_context.app)
            .unwrap()
            .to_owned();
        let mut state = core();
        state.admit(without_context, 10, &pack).unwrap();
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 10, 2_000)
                .is_none(),
            "a row census alone cannot assert the native window root"
        );

        let mut state = core();
        state
            .admit(partial_window_snapshot(1, 10, &scope), 10, &pack)
            .unwrap();
        state
            .note_health(
                SourceHealth {
                    source: SourceKind::Ax,
                    state: SourceState::Unavailable,
                    reason: Some("lost source".into()),
                    at_ms: 11,
                    provenance: None,
                },
                11,
            )
            .unwrap();
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 11, 2_000)
                .is_none()
        );
        state
            .note_health(
                SourceHealth {
                    source: SourceKind::Ax,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 12,
                    provenance: None,
                },
                12,
            )
            .unwrap();
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 12, 2_000)
                .is_none(),
            "health recovery cannot revive pre-fault window facts"
        );

        state
            .admit(partial_window_snapshot(2, 13, &scope), 13, &pack)
            .unwrap();
        assert!(
            state
                .row_window_root(&scope, &app, pid, &window, 13, 2_000)
                .is_some()
        );
    }

    #[test]
    fn scoped_window_consent_metadata_uses_the_real_partial_source_read_only() {
        let (state, snapshot) = state_with_scoped_source();
        let view = state
            .observation_view(snapshot.observed_ms)
            .expect("an admitted Source snapshot has an observation view");
        assert!(
            view.observed_target.is_none(),
            "the existing whole-structure target stays unavailable for a partial tree"
        );
        let scoped = view
            .scoped_window_target
            .expect("the complete same-read local census supplies scoped consent metadata");
        let source = snapshot.window_context.as_ref().unwrap();
        let table_scope = &snapshot
            .table_scope_search
            .as_ref()
            .unwrap()
            .requested_scope;
        let window = snapshot
            .windows
            .iter()
            .find(|window| window.window_id == source.origin_window_id)
            .unwrap();
        assert_eq!(&scoped.table_scope, table_scope);
        assert_eq!(&scoped.source, source);
        assert_eq!(scoped.target.snapshot_id, snapshot.snapshot_id);
        assert_eq!(scoped.target.structure_sequence, snapshot.source_sequence);
        assert_eq!(scoped.target.session_epoch, snapshot.session_epoch);
        assert_eq!(scoped.target.app, snapshot.app.bundle_id);
        assert_eq!(scoped.target.pid, snapshot.app.pid);
        assert_eq!(scoped.target.window_id, source.origin_window_id);
        assert_eq!(
            scoped.target.window_title.as_deref(),
            window.title.as_deref()
        );
        assert_eq!(
            scoped.target.frame_points,
            Some(
                [
                    window.frame.x,
                    window.frame.y,
                    window.frame.width,
                    window.frame.height
                ]
                .map(|value| value.round() as i64)
            )
        );
        assert_eq!(scoped.target.observed_ms, snapshot.observed_ms);
        assert_eq!(scoped.target.valid_until_ms, snapshot.observed_ms + 2_000);
        assert!(!scoped.target.contains_private_fields);
        assert_eq!(scoped.root_identity_hash.len(), 64);
        assert!(
            scoped
                .root_identity_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        let root = state
            .row_window_root(
                table_scope,
                &snapshot.app.bundle_id,
                snapshot.app.pid,
                &source.origin_window_id,
                snapshot.observed_ms,
                2_000,
            )
            .unwrap();
        let evidence = derive_window_root_binding_evidence(&snapshot, root, table_scope).unwrap();
        assert_eq!(
            scoped.root_identity_hash,
            evidence
                .continuity
                .context_digest
                .strip_prefix("bc1:")
                .unwrap()
        );
    }

    #[test]
    fn native_scoped_omission_health_before_snapshot_keeps_only_the_scoped_window_path_open() {
        let snapshot = scoped_source_snapshot();
        assert_eq!(
            snapshot.incomplete_reason.as_deref(),
            Some(AX_SCOPED_ROW_OMISSION_REASON)
        );
        let received_ms = snapshot.observed_ms + 30;
        let mut state = state_for_scoped_source(&snapshot);
        state
            .note_health(
                native_ax_health(
                    SourceState::Degraded,
                    AX_SCOPED_ROW_OMISSION_REASON,
                    received_ms,
                    1,
                ),
                received_ms,
            )
            .unwrap();

        assert_eq!(state.structure_floor_ms, Some(received_ms));
        assert_eq!(state.ax_metadata_floor_ms, None);
        assert!(state.scoped_window_target(received_ms).is_none());

        let pack = AppPack::registered_native(&snapshot.app.bundle_id).unwrap();
        state.admit(snapshot.clone(), received_ms, &pack).unwrap();
        let source = snapshot.window_context.as_ref().unwrap();
        assert_ne!("native-health-stream", source.source_session_id);
        assert!(
            state
                .row_window_root(
                    &snapshot
                        .table_scope_search
                        .as_ref()
                        .unwrap()
                        .requested_scope,
                    &snapshot.app.bundle_id,
                    snapshot.app.pid,
                    &source.origin_window_id,
                    received_ms,
                    2_000,
                )
                .is_some()
        );
        let observation = state.observation_view(received_ms).unwrap();
        assert!(observation.scoped_window_target.is_some());
        assert!(observation.observed_target.is_none());
        assert!(
            !state.usable(received_ms, 2_000),
            "the marker keeps generic full-tree admission closed"
        );
        assert!(
            state
                .window_root(
                    &snapshot.app.bundle_id,
                    snapshot.app.pid,
                    &source.origin_window_id,
                    received_ms,
                    2_000,
                )
                .is_none()
        );
    }

    #[test]
    fn scoped_omission_marker_alone_does_not_override_context_or_other_ax_failures() {
        let no_context = scoped_source_snapshot();
        let received_ms = no_context.observed_ms + 20;
        let mut no_context_snapshot = no_context.clone();
        no_context_snapshot.window_context = None;
        let mut no_context_state = state_for_scoped_source(&no_context_snapshot);
        no_context_state
            .note_health(
                native_ax_health(
                    SourceState::Degraded,
                    AX_SCOPED_ROW_OMISSION_REASON,
                    received_ms,
                    1,
                ),
                received_ms,
            )
            .unwrap();
        let pack = AppPack::registered_native(&no_context_snapshot.app.bundle_id).unwrap();
        no_context_state
            .admit(no_context_snapshot.clone(), received_ms, &pack)
            .unwrap();
        assert!(
            no_context_state
                .observation_view(received_ms)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        for (state_kind, reason) in [
            (SourceState::Unavailable, AX_SCOPED_ROW_OMISSION_REASON),
            (
                SourceState::Degraded,
                "AX table row subtree omitted for scoped census; visible privacy coverage unavailable: visible_child_parent_mismatch",
            ),
            (
                SourceState::Degraded,
                "AX child list truncated; AX node budget exceeded",
            ),
        ] {
            let snapshot = scoped_source_snapshot();
            let received_ms = snapshot.observed_ms + 20;
            let mut state = state_for_scoped_source(&snapshot);
            state
                .note_health(
                    native_ax_health(state_kind, reason, received_ms, 1),
                    received_ms,
                )
                .unwrap();
            assert_eq!(state.ax_metadata_floor_ms, Some(received_ms));
            let pack = AppPack::registered_native(&snapshot.app.bundle_id).unwrap();
            state.admit(snapshot.clone(), received_ms, &pack).unwrap();
            assert!(
                state
                    .observation_view(received_ms)
                    .unwrap()
                    .scoped_window_target
                    .is_none()
            );
        }
    }

    #[test]
    fn scoped_omission_after_a_genuine_floor_requires_a_strictly_newer_source_snapshot() {
        let mut snapshot = scoped_source_snapshot();
        let pack = AppPack::registered_native(&snapshot.app.bundle_id).unwrap();
        let mut state = state_for_scoped_source(&snapshot);
        state
            .admit(snapshot.clone(), snapshot.observed_ms, &pack)
            .unwrap();

        let genuine_floor_ms = snapshot.observed_ms + 20;
        state
            .note_health(
                native_ax_health(
                    SourceState::Degraded,
                    "AX node budget exceeded",
                    genuine_floor_ms,
                    1,
                ),
                genuine_floor_ms,
            )
            .unwrap();
        let marker_received_ms = genuine_floor_ms + 10;
        state
            .note_health(
                native_ax_health(
                    SourceState::Degraded,
                    AX_SCOPED_ROW_OMISSION_REASON,
                    marker_received_ms,
                    2,
                ),
                marker_received_ms,
            )
            .unwrap();
        assert_eq!(state.ax_metadata_floor_ms, Some(genuine_floor_ms));
        assert!(
            state
                .observation_view(marker_received_ms)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        let mut at_floor = snapshot.clone();
        at_floor.source_sequence += 1;
        at_floor.observed_ms = genuine_floor_ms;
        at_floor.snapshot_id = RequestId::new("scoped-window-at-metadata-floor").unwrap();
        let at_floor_sequence = at_floor.source_sequence;
        let at_floor_observed_ms = at_floor.observed_ms;
        {
            let context = at_floor.window_context.as_mut().unwrap();
            context.verified_source_sequence = at_floor_sequence;
            context.verified_observed_ms = at_floor_observed_ms;
        }
        state
            .admit(at_floor.clone(), marker_received_ms + 1, &pack)
            .unwrap();
        assert!(
            state
                .observation_view(marker_received_ms + 1)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        snapshot.source_sequence = at_floor.source_sequence + 1;
        snapshot.observed_ms = genuine_floor_ms + 1;
        snapshot.snapshot_id = RequestId::new("scoped-window-after-metadata-floor").unwrap();
        let source_sequence = snapshot.source_sequence;
        let observed_ms = snapshot.observed_ms;
        {
            let context = snapshot.window_context.as_mut().unwrap();
            context.verified_source_sequence = source_sequence;
            context.verified_observed_ms = observed_ms;
        }
        state
            .admit(snapshot.clone(), marker_received_ms + 2, &pack)
            .unwrap();
        let view = state.observation_view(marker_received_ms + 2).unwrap();
        assert!(view.scoped_window_target.is_some());
        assert!(view.observed_target.is_none());
        assert_eq!(state.ax_metadata_floor_ms, Some(genuine_floor_ms));
    }

    #[test]
    fn scoped_window_consent_metadata_rejects_wrong_scope_census_root_and_source() {
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot
                .table_scope_search
                .as_mut()
                .unwrap()
                .requested_scope
                .column_headers[0] = "Different header".into();
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot
                .table_scope_search
                .as_mut()
                .unwrap()
                .candidates
                .clear();
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            let search = snapshot.table_scope_search.as_mut().unwrap();
            let duplicate = search.candidates[0].clone();
            search.candidates.push(duplicate);
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            let window = snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .unwrap()
                .to_owned();
            let root = snapshot
                .objects
                .iter_mut()
                .find(|object| object.window_id == window && object.parent.is_none())
                .unwrap();
            root.subrole = Some("AXDialog".into());
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot.window_context.as_mut().unwrap().origin_window_id =
                "window-from-another-read".into();
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot.window_context.as_mut().unwrap().process.pid += 1;
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot
                .window_context
                .as_mut()
                .unwrap()
                .process
                .start_seconds = 0;
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot
                .window_context
                .as_mut()
                .unwrap()
                .verified_source_sequence += 1;
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot
                .window_context
                .as_mut()
                .unwrap()
                .verified_observed_ms += 1;
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            let window = snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .unwrap()
                .to_owned();
            let root = snapshot
                .objects
                .iter_mut()
                .find(|object| object.window_id == window && object.parent.is_none())
                .unwrap();
            root.title = Some("A different title".into());
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            let window = snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .unwrap()
                .to_owned();
            snapshot
                .windows
                .iter_mut()
                .find(|info| info.window_id == window)
                .unwrap()
                .title = None;
        });
    }

    #[test]
    fn scoped_window_consent_metadata_respects_floors_refresh_health_and_ttl() {
        let (mut row_floored, snapshot) = state_with_scoped_source();
        row_floored.row_structure_floor_ms = Some(snapshot.observed_ms);
        assert!(
            row_floored
                .observation_view(snapshot.observed_ms)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        let (mut ax_floored, snapshot) = state_with_scoped_source();
        ax_floored.ax_metadata_floor_ms = Some(snapshot.observed_ms);
        assert!(
            ax_floored
                .observation_view(snapshot.observed_ms)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        let (mut refreshing, snapshot) = state_with_scoped_source();
        refreshing.begin_refresh().unwrap();
        assert!(
            refreshing
                .observation_view(snapshot.observed_ms + 1)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        let (stale, snapshot) = state_with_scoped_source();
        assert!(
            stale
                .observation_view(snapshot.observed_ms + 2_000)
                .unwrap()
                .scoped_window_target
                .is_none()
        );

        assert_scoped_source_edit_is_not_projected(|snapshot| {
            let active = snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .unwrap()
                .to_owned();
            snapshot
                .windows
                .iter_mut()
                .find(|window| window.window_id == active)
                .unwrap()
                .modal = true;
        });
        assert_scoped_source_edit_is_not_projected(|snapshot| {
            snapshot.observer_coverage.state = SourceState::Unavailable;
        });
    }

    #[test]
    fn malformed_partial_window_scope_or_root_facts_are_not_admitted() {
        let pack = AppPack::settings_fixture();
        let scope = row_scope();
        let mut no_candidate = partial_window_snapshot(1, 10, &scope);
        no_candidate
            .table_scope_search
            .as_mut()
            .unwrap()
            .candidates
            .clear();
        assert!(no_candidate.validate().is_err());

        let mut changed_root = partial_window_snapshot(1, 10, &scope);
        changed_root
            .window_context
            .as_mut()
            .unwrap()
            .origin_window_frame
            .x += 1.0;
        assert!(changed_root.validate().is_err());

        let mut state = core();
        assert!(state.admit(no_candidate, 10, &pack).is_err());
        let mut state = core();
        assert!(state.admit(changed_root, 10, &pack).is_err());
    }
}
