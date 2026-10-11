//! What cu-source reports: structure snapshots, source health and frame metadata.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::binding::{InstanceFingerprint, instance_fingerprint};
use super::device::SurfaceRef;
use super::ids::{ObjectRef, RequestId};
use super::owned_document::OwnedDocumentReceiverEvidence;
use super::require_nonempty;
use super::table_scope::TableScopeSearch;
use super::task::RowTableScope;

const SOURCE_FRAME_REF_PATTERN: &str = r"^(?=.*\S)[^\u0000-\u001F\u007F-\u009F]+$";
const SOURCE_MACH_ABSOLUTE_CLOCK: &str = "mach_absolute";
const MAX_SOURCE_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Maximum number of AX objects projected in one structure snapshot.
pub const MAX_SOURCE_OBJECTS: usize = 512;
/// The bounded notification vocabulary used for one projected AX object.
pub const MAX_OBSERVER_NOTIFICATION_KINDS_PER_OBJECT: usize = 8;
/// Only the first bounded set of real registration failures is retained.
pub const MAX_OBSERVER_FAILURE_REASONS: usize = 16;
/// Maximum number of attempted registration failures in one bounded snapshot.
pub const MAX_OBSERVER_REGISTRATION_FAILURES: usize =
    MAX_SOURCE_OBJECTS * MAX_OBSERVER_NOTIFICATION_KINDS_PER_OBJECT;
/// Maximum AXIndex exposed by the bounded table-column projection.
pub const MAX_TABLE_VIEWPORT_COLUMN_INDEX: u32 = 63;
/// Largest actual AXIndex representable by the bounded row projection.
/// This is an AX global index, not an ordinal within the visible-row slice.
pub const MAX_TABLE_VIEWPORT_ROW_AX_INDEX: u32 = u32::MAX;

/// A rectangle in global display points, origin at the top-left of the main display.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn valid_bounded(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 100_000.0)
            && self.width > 0.0
            && self.height > 0.0
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        self.valid_bounded()
            && other.valid_bounded()
            && other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum UiValue {
    Bool(bool),
    Text(String),
    Number(f64),
}

/// The accessibility actions an element declares. Values are not part of the
/// declaration; they belong to the action that is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxActionName {
    Press,
    SetValue,
    ShowMenu,
    ScrollToVisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScrollAxisState {
    pub value: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}
impl ScrollAxisState {
    pub fn valid(self) -> bool {
        self.value.is_finite()
            && self
                .min
                .is_none_or(|min| min.is_finite() && self.value >= min)
            && self
                .max
                .is_none_or(|max| max.is_finite() && self.value <= max)
            && self.min.zip(self.max).is_none_or(|(min, max)| min < max)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScrollState {
    pub horizontal: Option<ScrollAxisState>,
    pub vertical: Option<ScrollAxisState>,
}
impl ScrollState {
    pub fn valid(&self) -> bool {
        (self.horizontal.is_some() || self.vertical.is_some())
            && self.horizontal.is_none_or(ScrollAxisState::valid)
            && self.vertical.is_none_or(ScrollAxisState::valid)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiObject {
    pub instance_ref: ObjectRef,
    pub parent: Option<ObjectRef>,
    pub window_id: String,
    #[schemars(length(min = 1))]
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Absent for secure fields: the value is never read, sent or stored.
    pub value: Option<UiValue>,
    /// Generic input eligibility stays fail-closed when AX does not report this.
    /// Table row selection uses the separate typed viewport observation.
    pub enabled: bool,
    pub focused: bool,
    pub frame: Rect,
    pub actions: Vec<AxActionName>,
    /// Complete raw AX action declaration from this read. This is independent
    /// from projected operations such as a settable value attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub ax_action_names: Option<Vec<String>>,
    /// Inside the visible range of its scroll container.
    pub in_view: bool,
    /// Title of the nearest labelled group ancestor.
    pub group: Option<String>,
    pub secure: bool,
    /// Actual source-owned scrollbar range; absent means unavailable.
    pub scroll: Option<ScrollState>,
    /// Cross-read continuity explicitly established for this projected object.
    pub identity_continuity: ObjectIdentityContinuity,
}

/// Whether Source can prove that this exact AX object remains the same across reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectIdentityContinuity {
    NotificationCovered,
    ReadScoped,
}

/// Direct AXEnabled observation for row-selection scopes. This is kept
/// separate from `UiObject.enabled`, whose required boolean remains
/// fail-closed when AX does not report the attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxEnabledObservation {
    /// The source read an actual AX Boolean `true`.
    Enabled,
    /// The source read an actual AX Boolean `false`; row input must refuse.
    Disabled,
    /// AX returned exactly `attributeUnsupported` for AXEnabled.
    AttributeUnsupported,
    /// AX returned no value, another error, a malformed value, or timed out.
    Unavailable,
}

impl AxEnabledObservation {
    /// Row selection may use the specific AXSelected setter capability when
    /// AXEnabled is explicitly unsupported. Other unknown/error states do not
    /// establish eligibility.
    pub const fn permits_row_selection(self) -> bool {
        matches!(self, Self::Enabled | Self::AttributeUnsupported)
    }

    const fn matches_generic_enabled(self, enabled: bool) -> bool {
        enabled == matches!(self, Self::Enabled)
    }
}

/// Bounded, stable categories for AX observer notification registration.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ObserverNotification {
    UiElementDestroyed,
    Created,
    Moved,
    Resized,
    LayoutChanged,
    ValueChanged,
    TitleChanged,
    RowCountChanged,
}

/// One real, failed registration attempt. Platform text and labels are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObserverRegistrationFailure {
    /// Must identify an object present in the final projected snapshot.
    pub instance_ref: ObjectRef,
    pub notification: ObserverNotification,
    /// Nonzero platform status from an actual registration attempt.
    pub error_code: i32,
}

/// Observer health and per-object coverage, independent of tree completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObserverCoverage {
    pub state: SourceState,
    #[schemars(range(max = 512))]
    pub covered_objects: u32,
    #[schemars(range(max = 512))]
    pub read_scoped_objects: u32,
    /// Counts unique, attempted registrations that returned a nonzero failure.
    #[schemars(range(max = 4096))]
    pub failed_registrations: u32,
    #[schemars(length(max = 16))]
    pub failures: Vec<ObserverRegistrationFailure>,
}

impl ObserverCoverage {
    /// Build a synthetic observer summary from the explicit per-object modes.
    /// This never infers notification coverage; it only counts metadata already
    /// carried by each projected object.
    pub fn for_projected_objects(objects: &[UiObject]) -> Self {
        let covered_objects = objects
            .iter()
            .filter(|object| {
                object.identity_continuity == ObjectIdentityContinuity::NotificationCovered
            })
            .count() as u32;
        let read_scoped_objects = objects.len() as u32 - covered_objects;
        Self {
            state: if read_scoped_objects == 0 {
                SourceState::Available
            } else {
                SourceState::Degraded
            },
            covered_objects,
            read_scoped_objects,
            failed_registrations: 0,
            failures: Vec::new(),
        }
    }

    fn validate(&self, objects: &[UiObject]) -> Result<(), String> {
        if objects.len() > MAX_SOURCE_OBJECTS
            || self.covered_objects as usize
                != objects
                    .iter()
                    .filter(|object| {
                        object.identity_continuity == ObjectIdentityContinuity::NotificationCovered
                    })
                    .count()
            || self.read_scoped_objects as usize
                != objects
                    .iter()
                    .filter(|object| {
                        object.identity_continuity == ObjectIdentityContinuity::ReadScoped
                    })
                    .count()
            || self.covered_objects as usize + self.read_scoped_objects as usize != objects.len()
            || self.failed_registrations as usize > MAX_OBSERVER_REGISTRATION_FAILURES
            || self.failures.len()
                != (self.failed_registrations as usize).min(MAX_OBSERVER_FAILURE_REASONS)
        {
            return Err(
                "observer coverage counts do not match the bounded projected objects".into(),
            );
        }

        for (index, failure) in self.failures.iter().enumerate() {
            let Some(object) = objects
                .iter()
                .find(|object| object.instance_ref == failure.instance_ref)
            else {
                return Err("observer failure must reference a final projected object".into());
            };
            let lifetime_coverage_failed = failure.notification
                == ObserverNotification::UiElementDestroyed
                || (matches!(object.role.as_str(), "AXWindow" | "AXSheet")
                    && matches!(
                        failure.notification,
                        ObserverNotification::Moved | ObserverNotification::Resized
                    ));
            if failure.error_code == 0
                || self.failures[..index].iter().any(|prior| {
                    prior.instance_ref == failure.instance_ref
                        && prior.notification == failure.notification
                })
                || (lifetime_coverage_failed
                    && object.identity_continuity != ObjectIdentityContinuity::ReadScoped)
            {
                return Err("observer failure must be unique, nonzero, mapped to a projected object, and reflected in its continuity mode".into());
            }
        }

        match self.state {
            SourceState::Available
                if self.read_scoped_objects != 0 || self.failed_registrations != 0 =>
            {
                Err("available observer coverage requires every object covered and no failed registrations".into())
            }
            SourceState::Degraded
                if self.read_scoped_objects == 0 && self.failed_registrations == 0 =>
            {
                Err("degraded observer coverage requires a real partial coverage gap".into())
            }
            SourceState::Unavailable if self.covered_objects != 0 => {
                Err("unavailable observer coverage cannot claim covered objects".into())
            }
            _ => Ok(()),
        }
    }
}

/// Source-owned, row-bounded projection of one real AXTable viewport.
/// `visible_rows_complete` applies only to the actual AXVisibleRows read and
/// does not change `StructureSnapshot::complete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableViewport {
    pub table_instance_ref: ObjectRef,
    pub scroll_container_instance_ref: ObjectRef,
    pub process_id: u32,
    /// Actual AXEnabled state of this AXTable; not inferred from its rows.
    pub ax_enabled: AxEnabledObservation,
    pub table_frame: Rect,
    pub viewport_frame: Rect,
    pub visible_rows_complete: bool,
    #[schemars(length(max = 256))]
    pub visible_rows: Vec<TableViewportRow>,
    pub columns_complete: bool,
    #[schemars(length(max = 64))]
    pub columns: Vec<TableViewportColumn>,
}

/// An actual AXRow from the current AXVisibleRows enumeration. A missing AX
/// identifier or selection attribute remains unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableViewportRow {
    pub instance_ref: Option<ObjectRef>,
    pub parent_instance_ref: ObjectRef,
    pub frame: Rect,
    /// Actual AXEnabled state of this AXRow, kept separate from the generic
    /// object's fail-closed boolean projection.
    pub ax_enabled: AxEnabledObservation,
    pub accessibility_identifier: Option<String>,
    /// Actual global AXIndex from the source row. This is never inferred from
    /// the row's position in `visible_rows`; `None` remains unknown.
    #[serde(default)]
    #[schemars(range(max = 4_294_967_295u32))]
    pub ax_index: Option<u32>,
    pub selected: Option<bool>,
    /// Actual AXUIElementIsAttributeSettable(AXSelected) result. Unknown is not
    /// selection authority; the specialized row capability must cross-check it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ax_selected_settable: Option<bool>,
    pub cells_complete: bool,
    #[schemars(length(max = 128))]
    pub cells: Vec<TableViewportCell>,
}

/// One actual AXColumn and whatever header context the current AX read found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableViewportColumn {
    pub instance_ref: Option<ObjectRef>,
    pub parent_instance_ref: ObjectRef,
    pub frame: Rect,
    pub header_instance_ref: Option<ObjectRef>,
    pub header: Option<String>,
    #[schemars(range(max = 63))]
    pub ax_index: Option<u32>,
}

/// One actual AXCell, preserving its parent, optional column/header links, and
/// the exact AXValue if available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableViewportCell {
    pub instance_ref: Option<ObjectRef>,
    pub parent_instance_ref: Option<ObjectRef>,
    pub frame: Rect,
    /// Actual AXIndex of this cell's singleton AXRow range, when reported.
    /// It is never inferred from visible row order.
    #[serde(default)]
    #[schemars(range(max = 4_294_967_295u32))]
    pub row_ax_index: Option<u32>,
    /// Actual AXIndex of the cell's AXColumn, when reported. A known index is
    /// bounded to the source's 64-column projection and is not an ordinal
    /// fallback.
    #[serde(default)]
    #[schemars(range(max = 63u32))]
    pub column_ax_index: Option<u32>,
    pub column_instance_ref: Option<ObjectRef>,
    pub header_instance_ref: Option<ObjectRef>,
    pub column_header: Option<String>,
    pub value: Option<UiValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    Window,
    Sheet,
    Dialog,
    Popover,
    Menu,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowInfo {
    #[schemars(length(min = 1))]
    pub window_id: String,
    pub title: Option<String>,
    pub frame: Rect,
    /// 0 is the frontmost window.
    pub z_index: u32,
    pub kind: LayerKind,
    pub modal: bool,
    pub owner_bundle_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppIdentity {
    #[schemars(length(min = 1))]
    pub bundle_id: String,
    pub name: String,
    pub pid: u32,
}

/// A source fact that distinguishes a known value from a known absence and
/// from a source that could not answer the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationFact<T> {
    Known { value: T },
    Absent,
    Unavailable,
}

/// System-wide focus reported by this exact Source snapshot. App and window
/// availability are independent: an app can be known while its focused window
/// is absent or unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalFocusObservation {
    pub app: ObservationFact<AppIdentity>,
    pub window_id: ObservationFact<String>,
}

impl GlobalFocusObservation {
    pub fn known_window_id(&self) -> Option<&str> {
        match &self.window_id {
            ObservationFact::Known { value } => Some(value),
            ObservationFact::Absent | ObservationFact::Unavailable => None,
        }
    }

    pub fn known_window_for_app(&self, app: &AppIdentity) -> Option<&str> {
        if !matches!(&self.app, ObservationFact::Known { value } if value == app) {
            return None;
        }
        self.known_window_id()
    }

    pub fn validate(&self) -> Result<(), String> {
        if let ObservationFact::Known { value } = &self.app {
            validate_menu_id("global-focus app bundle id", &value.bundle_id)?;
            if value.pid == 0 || value.name.len() > 512 || value.name.chars().any(char::is_control)
            {
                return Err("global-focus app identity is invalid or unbounded".into());
            }
        }
        if let ObservationFact::Known { value } = &self.window_id {
            validate_menu_id("global-focus window id", value)?;
        }
        Ok(())
    }
}

/// Window metadata for a native target context. The nested normal-window facts
/// retain Source session, epoch, process birth identity, geometry and exact
/// read provenance without claiming that this window owns global focus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceBackgroundWindowContext {
    pub window: SourceWindowContext,
    pub hidden: ObservationFact<bool>,
    pub minimized: ObservationFact<bool>,
    #[schemars(length(min = 68, max = 68))]
    pub target_context_hash: String,
}

impl SourceBackgroundWindowContext {
    pub fn computed_target_context_hash(&self, app: &AppIdentity) -> Result<String, String> {
        self.computed_target_context_hash_for(&app.bundle_id, app.pid)
    }

    pub fn computed_target_context_hash_for(
        &self,
        bundle_id: &str,
        pid: u32,
    ) -> Result<String, String> {
        if !self.window.origin_window_frame.valid_bounded() {
            return Err("target context requires finite bounded window geometry".into());
        }
        // Hash encoding is independent from a language's decimal float formatter.
        // Geometry remains numeric in the public payload; only this stable
        // commitment uses fixed-width IEEE754 bits, with either zero normalized.
        #[derive(Serialize)]
        struct HashFrame {
            x: String,
            y: String,
            width: String,
            height: String,
        }
        let bits = |value: f64| format!("{:016x}", if value == 0.0 { 0 } else { value.to_bits() });
        let frame = self.window.origin_window_frame;
        #[derive(Serialize)]
        struct StableTargetContext<'a> {
            version: u8,
            kind: &'static str,
            bundle_id: &'a str,
            pid: u32,
            process: &'a KernelProcessIdentity,
            source_session_id: &'a str,
            session_epoch: u64,
            window_id: &'a str,
            window_frame: HashFrame,
        }
        let stable = StableTargetContext {
            version: 1,
            kind: "native_window",
            bundle_id,
            pid,
            process: &self.window.process,
            source_session_id: &self.window.source_session_id,
            session_epoch: self.window.session_epoch,
            window_id: &self.window.origin_window_id,
            window_frame: HashFrame {
                x: bits(frame.x),
                y: bits(frame.y),
                width: bits(frame.width),
                height: bits(frame.height),
            },
        };
        let canonical = serde_json::to_vec(&stable)
            .map_err(|_| "native target context could not be canonicalized")?;
        let mut hash = String::from("tc1:");
        for byte in Sha256::digest(canonical) {
            hash.push_str(&format!("{byte:02x}"));
        }
        Ok(hash)
    }

    pub fn validate(&self, app: &AppIdentity) -> Result<(), String> {
        self.validate_identity(&app.bundle_id, app.pid)
    }

    pub fn validate_identity(&self, bundle_id: &str, pid: u32) -> Result<(), String> {
        self.window.validate()?;
        validate_menu_id("background target app", bundle_id)?;
        if pid == 0
            || self.window.process.pid != pid
            || self.window.session_epoch == 0
            || self.window.verified_source_sequence == 0
        {
            return Err("background window context has mismatched app or source identity".into());
        }
        if self.target_context_hash != self.computed_target_context_hash_for(bundle_id, pid)? {
            return Err(
                "background window context hash differs from its stable Source identity".into(),
            );
        }
        Ok(())
    }

    pub fn is_visible_standard_window(&self) -> bool {
        matches!(self.hidden, ObservationFact::Known { value: false })
            && matches!(self.minimized, ObservationFact::Known { value: false })
    }
}

/// A logical target context is independent from the system-wide focused app
/// and window. Browser and application-integration variants intentionally carry
/// no native window identifier or display geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetContext {
    NativeWindow {
        app: AppIdentity,
        source: SourceBackgroundWindowContext,
    },
    BrowserPage {
        browser_session_id: String,
        context_id: String,
        page_id: String,
        document_id: String,
        source_sequence: u64,
        observed_ms: u64,
        target_context_hash: String,
    },
    AppResource {
        integration_id: String,
        resource_id: String,
        source_sequence: u64,
        observed_ms: u64,
        target_context_hash: String,
    },
    Simulated {
        source_session_id: String,
        session_epoch: u64,
        source_sequence: u64,
        observed_ms: u64,
        target_context_hash: String,
    },
}

impl TargetContext {
    pub fn target_context_hash(&self) -> &str {
        match self {
            Self::NativeWindow { source, .. } => &source.target_context_hash,
            Self::BrowserPage {
                target_context_hash,
                ..
            }
            | Self::AppResource {
                target_context_hash,
                ..
            }
            | Self::Simulated {
                target_context_hash,
                ..
            } => target_context_hash,
        }
    }

    pub fn validate(&self, snapshot: &StructureSnapshot) -> Result<(), String> {
        let valid_hash = |hash: &str| {
            hash.strip_prefix("tc1:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        };
        match self {
            Self::NativeWindow { app, source } => {
                source.validate(app)?;
                if app != &snapshot.app
                    || source.window.session_epoch != snapshot.session_epoch
                    || source.window.verified_source_sequence != snapshot.source_sequence
                    || source.window.verified_observed_ms != snapshot.observed_ms
                {
                    return Err("native target context does not match its Source snapshot".into());
                }
                let windows: Vec<_> = snapshot
                    .windows
                    .iter()
                    .filter(|window| window.window_id == source.window.origin_window_id)
                    .collect();
                let [window] = windows.as_slice() else {
                    return Err("native target context requires one actual Source window".into());
                };
                if window.kind != LayerKind::Window
                    || window.modal
                    || window.owner_bundle_id != app.bundle_id
                    || window.frame != source.window.origin_window_frame
                {
                    return Err(
                        "native target context requires its actual nonmodal standard window".into(),
                    );
                }
            }
            Self::BrowserPage {
                browser_session_id,
                context_id,
                page_id,
                document_id,
                source_sequence,
                observed_ms,
                target_context_hash,
            } => {
                for (name, value) in [
                    ("browser session id", browser_session_id),
                    ("browser context id", context_id),
                    ("browser page id", page_id),
                    ("browser document id", document_id),
                ] {
                    validate_menu_id(name, value)?;
                }
                if *source_sequence != snapshot.source_sequence
                    || *observed_ms != snapshot.observed_ms
                    || !valid_hash(target_context_hash)
                {
                    return Err("browser target context has stale Source provenance or hash".into());
                }
            }
            Self::AppResource {
                integration_id,
                resource_id,
                source_sequence,
                observed_ms,
                target_context_hash,
            } => {
                validate_menu_id("app integration id", integration_id)?;
                validate_menu_id("app resource id", resource_id)?;
                if *source_sequence != snapshot.source_sequence
                    || *observed_ms != snapshot.observed_ms
                    || !valid_hash(target_context_hash)
                {
                    return Err("app resource context has stale Source provenance or hash".into());
                }
            }
            Self::Simulated {
                source_session_id,
                session_epoch,
                source_sequence,
                observed_ms,
                target_context_hash,
            } => {
                validate_menu_id("simulated source session id", source_session_id)?;
                if *session_epoch != snapshot.session_epoch
                    || *source_sequence != snapshot.source_sequence
                    || *observed_ms != snapshot.observed_ms
                    || !valid_hash(target_context_hash)
                {
                    return Err("simulated target context has stale provenance or hash".into());
                }
            }
        }
        Ok(())
    }
}

/// One actual persistent AX registry identity. This is independent of the
/// per-read source sequence and of the visible tree's structural revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentityBoundary {
    #[schemars(length(min = 36, max = 36))]
    pub namespace: String,
    #[schemars(range(min = 1))]
    pub generation: u64,
}

impl SourceIdentityBoundary {
    pub fn validate(&self) -> Result<(), String> {
        let bytes = self.namespace.as_bytes();
        let canonical_uuid = bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
            });
        if !canonical_uuid || self.generation == 0 {
            return Err(
                "identity boundary requires a lowercase canonical UUID and positive generation"
                    .into(),
            );
        }
        Ok(())
    }
}

/// What the source has actually verified about a retained opener reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MenuAnchorContinuity {
    DirectRead,
    ObservedLifetime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KernelProcessIdentity {
    pub pid: u32,
    pub start_seconds: u64,
    pub start_micros: u64,
}

impl KernelProcessIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if self.pid == 0
            || self.pid > i32::MAX as u32
            || self.start_seconds == 0
            || self.start_micros >= 1_000_000
        {
            return Err("invalid process kernel identity".into());
        }
        Ok(())
    }
}

/// Actual AX focused-element relationship. The first profile requires CFEqual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FocusRelation {
    ExactControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompositionProofKind {
    AxTextInputMarkedRange,
}

/// A positive composition reading must belong to the actual focused element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompositionProof {
    pub kind: CompositionProofKind,
    pub focused_instance: ObjectRef,
    pub verified_source_sequence: u64,
    pub verified_observed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompositionState {
    Unknown,
    NotApplicable,
    Marked { proof: CompositionProof },
    Unmarked { proof: CompositionProof },
}

/// Selected input source and underlying keyboard layout are independent TIS
/// observations. ASCII capability and layout data never prove IME composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceInputSource {
    pub id: Option<String>,
    pub source_type: Option<String>,
    pub category: Option<String>,
    pub mode_id: Option<String>,
    pub ascii_capable: Option<bool>,
    pub layout_data_sha256: Option<String>,
}

impl SourceInputSource {
    pub fn validate(&self) -> Result<(), String> {
        for value in [&self.id, &self.source_type, &self.category, &self.mode_id]
            .into_iter()
            .flatten()
        {
            validate_menu_id("keyboard input-source fact", value)?;
        }
        if self.layout_data_sha256.as_ref().is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err("keyboard layout data requires a lowercase SHA256 digest".into());
        }
        if self.id.is_none()
            && self.source_type.is_none()
            && self.category.is_none()
            && self.mode_id.is_none()
            && self.ascii_capable.is_none()
            && self.layout_data_sha256.is_none()
        {
            return Err("an input-source record must contain an actual observed fact".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceKeyboardContext {
    pub selected_input_source: Option<SourceInputSource>,
    pub underlying_keyboard_layout: Option<SourceInputSource>,
    pub keyboard_type: Option<u32>,
    pub caps_lock: Option<bool>,
}

impl SourceKeyboardContext {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(source) = &self.selected_input_source {
            source.validate()?;
        }
        if let Some(layout) = &self.underlying_keyboard_layout {
            layout.validate()?;
        }
        Ok(())
    }
}

/// Current Source-owned focus facts, never a hit-test or inferred focus claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusContext {
    pub source_session_id: String,
    pub session_epoch: u64,
    pub process: KernelProcessIdentity,
    pub origin_window_id: String,
    pub origin_window_frame: Rect,
    pub target_instance: ObjectRef,
    pub target_fingerprint: InstanceFingerprint,
    pub target_frame: Rect,
    pub focused_instance: ObjectRef,
    pub relation: FocusRelation,
    pub composition: CompositionState,
    pub keyboard: SourceKeyboardContext,
    pub verified_source_sequence: u64,
    pub verified_observed_ms: u64,
}

/// Current normal-window kernel/root facts. This is not focus evidence or an
/// input capability, and remains representable when no focused element exists.
/// A partial snapshot may carry these producer-verified facts only within a
/// complete unique table scope. The census is not a capture privacy proof;
/// Native Source separately audits its visible window before publishing them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceWindowContext {
    pub source_session_id: String,
    pub session_epoch: u64,
    pub process: KernelProcessIdentity,
    pub origin_window_id: String,
    pub origin_window_frame: Rect,
    pub verified_source_sequence: u64,
    pub verified_observed_ms: u64,
}

impl SourceWindowContext {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("window source session id", &self.source_session_id)?;
        validate_menu_id("window origin id", &self.origin_window_id)?;
        self.process.validate()?;
        if self.session_epoch == 0
            || self.verified_source_sequence == 0
            || !self.origin_window_frame.valid_bounded()
        {
            return Err("invalid normal-window context epoch, provenance or geometry".into());
        }
        Ok(())
    }
}

pub fn known_nontext_focus_role(role: &str) -> bool {
    matches!(
        role,
        "AXButton"
            | "AXRadioButton"
            | "AXCheckBox"
            | "AXSlider"
            | "AXSwitch"
            | "AXStepper"
            | "AXMenuButton"
    )
}

impl FocusContext {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("focus source session id", &self.source_session_id)?;
        validate_menu_id("focus origin window id", &self.origin_window_id)?;
        self.process.validate()?;
        self.target_fingerprint.validate()?;
        self.keyboard.validate()?;
        let fp = &self.target_fingerprint;
        if self.session_epoch == 0
            || self.verified_source_sequence == 0
            || self.target_instance != self.focused_instance
            || !self.origin_window_frame.contains_rect(&self.target_frame)
            || !fingerprint_matches_frame(fp, self.target_frame)
            || fp.app_pid != self.process.pid
            || fp.window_id != self.origin_window_id
            || fp.role == "AXSecureTextField"
            || fp.subrole.as_deref() == Some("AXSecureTextField")
            || fp.ancestor_roles.first().map(String::as_str) != Some("AXWindow")
            || fp.ancestor_roles.iter().skip(1).any(|role| {
                matches!(
                    role.as_str(),
                    "AXWindow"
                        | "AXMenu"
                        | "AXMenuItem"
                        | "AXMenuBar"
                        | "AXApplication"
                        | "AXSheet"
                        | "AXDialog"
                )
            })
            || matches!(
                fp.role.as_str(),
                "AXWindow"
                    | "AXMenu"
                    | "AXMenuItem"
                    | "AXMenuBar"
                    | "AXApplication"
                    | "AXSheet"
                    | "AXDialog"
            )
        {
            return Err(
                "focus context requires exact control identity and a fixed standard-window scope"
                    .into(),
            );
        }
        match &self.composition {
            CompositionState::NotApplicable if !known_nontext_focus_role(&fp.role) => {
                return Err(
                    "composition not-applicable requires an explicitly known nontext role".into(),
                );
            }
            CompositionState::Marked { proof } | CompositionState::Unmarked { proof } => {
                if proof.focused_instance != self.focused_instance
                    || proof.verified_source_sequence != self.verified_source_sequence
                    || proof.verified_observed_ms != self.verified_observed_ms
                {
                    return Err(
                        "composition proof must belong to this actual focused Source read".into(),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A current read may advance provenance without replacing its receiver.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.source_session_id == other.source_session_id
            && self.session_epoch == other.session_epoch
            && self.process == other.process
            && self.origin_window_id == other.origin_window_id
            && self.origin_window_frame == other.origin_window_frame
            && self.target_instance == other.target_instance
            && self.target_fingerprint == other.target_fingerprint
            && self.target_frame == other.target_frame
            && self.focused_instance == other.focused_instance
            && self.relation == other.relation
            && self.composition == other.composition
            && self.keyboard == other.keyboard
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuOpenerAnchor {
    pub anchor_id: String,
    pub source_session_id: String,
    pub anchor_revision: u64,
    pub process: KernelProcessIdentity,
    pub origin_window_id: String,
    pub origin_window_frame: Rect,
    /// This read's ordinary registry instance, not the retained CF identity.
    pub opener_instance: ObjectRef,
    pub opener_fingerprint: InstanceFingerprint,
    pub opener_frame: Rect,
    pub verified_source_sequence: u64,
    pub verified_observed_ms: u64,
    pub continuity: MenuAnchorContinuity,
}

pub(crate) fn validate_menu_id(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("invalid or unbounded {name}"));
    }
    Ok(())
}

pub(crate) fn fingerprint_matches_frame(fp: &InstanceFingerprint, frame: Rect) -> bool {
    frame.valid_bounded()
        && [frame.x, frame.y, frame.width, frame.height].map(|v| v.round() as i64) == fp.frame
}

impl MenuOpenerAnchor {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("menu anchor id", &self.anchor_id)?;
        validate_menu_id("menu source session id", &self.source_session_id)?;
        validate_menu_id("menu origin window id", &self.origin_window_id)?;
        self.process.validate()?;
        self.opener_fingerprint.validate()?;
        let fp = &self.opener_fingerprint;
        if self.anchor_revision == 0
            || self.verified_source_sequence == 0
            || !self.origin_window_frame.contains_rect(&self.opener_frame)
            || !fingerprint_matches_frame(fp, self.opener_frame)
            || fp.app_pid != self.process.pid
            || fp.window_id != self.origin_window_id
            || !matches!(fp.role.as_str(), "AXMenuButton" | "AXPopUpButton")
            || fp.ancestor_roles.first().map(String::as_str) != Some("AXWindow")
            || fp.ancestor_roles.iter().skip(1).any(|role| {
                matches!(
                    role.as_str(),
                    "AXWindow"
                        | "AXMenu"
                        | "AXMenuItem"
                        | "AXMenuBar"
                        | "AXApplication"
                        | "AXSheet"
                        | "AXDialog"
                )
            })
            || fp.scroll_container.is_some()
        {
            return Err("invalid menu opener anchor descriptor or provenance".into());
        }
        Ok(())
    }

    /// Fresh reads and ordinary registry generations do not change the retained
    /// source identity. Every protected descriptor still has to match exactly.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.anchor_id == other.anchor_id
            && self.source_session_id == other.source_session_id
            && self.anchor_revision == other.anchor_revision
            && self.process == other.process
            && self.origin_window_id == other.origin_window_id
            && self.origin_window_frame == other.origin_window_frame
            && self.opener_fingerprint == other.opener_fingerprint
            && self.opener_frame == other.opener_frame
            && self.continuity == other.continuity
    }
}

/// A drawing window is visibility evidence for one menu, never an input root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuDrawingWindow {
    pub window_id: u32,
    pub owner_pid: u32,
    pub layer: i32,
    pub frame: Rect,
}

impl MenuDrawingWindow {
    pub fn validate(&self) -> Result<(), String> {
        if self.window_id == 0
            || self.owner_pid == 0
            || self.owner_pid > i32::MAX as u32
            || self.layer != 101
            || !self.frame.valid_bounded()
        {
            return Err("invalid or unsupported menu drawing window profile".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuContext {
    pub context_id: String,
    pub source_session_id: String,
    pub opener: MenuOpenerAnchor,
    pub menu_instance: ObjectRef,
    pub menu_fingerprint: InstanceFingerprint,
    pub menu_frame: Rect,
    pub drawing_window: MenuDrawingWindow,
}

impl MenuContext {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("menu context id", &self.context_id)?;
        validate_menu_id("menu context source session id", &self.source_session_id)?;
        self.opener.validate()?;
        self.menu_fingerprint.validate()?;
        self.drawing_window.validate()?;
        let opener = &self.opener.opener_fingerprint;
        let fp = &self.menu_fingerprint;
        let mut expected_roles = opener.ancestor_roles.clone();
        expected_roles.push(opener.role.clone());
        if self.source_session_id != self.opener.source_session_id
            || self.menu_instance == self.opener.opener_instance
            || fp.app != opener.app
            || fp.app_pid != opener.app_pid
            || fp.window_id != opener.window_id
            || fp.role != "AXMenu"
            || fp.ancestor_roles != expected_roles
            || fp.scroll_container.is_some()
            || !fingerprint_matches_frame(fp, self.menu_frame)
            || self.drawing_window.owner_pid != self.opener.process.pid
            || self.drawing_window.frame != self.menu_frame
            || self.opener.origin_window_id == format!("window-{}", self.drawing_window.window_id)
        {
            return Err("menu context does not describe one anchored level-one menu".into());
        }
        Ok(())
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.context_id == other.context_id
            && self.source_session_id == other.source_session_id
            && self.opener.same_identity(&other.opener)
            && self.menu_fingerprint == other.menu_fingerprint
            && self.menu_frame == other.menu_frame
            && self.drawing_window == other.drawing_window
    }
}

/// One reading of the target application's UI tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StructureSnapshot {
    pub snapshot_id: RequestId,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub clock_domain: String,
    /// Present only when this source read carries an actual persistent identity
    /// registry boundary; simulation and one-shot sources leave it absent.
    pub identity_boundary: Option<SourceIdentityBoundary>,
    /// PNG content ID that this exact structure read was paired with. This is
    /// the same value as `FrameMeta.stream.image_frame_id`, not the public
    /// `FrameMeta.frame_id` event ID. Ordinary AX and synthetic snapshots may
    /// omit it; computer capture admission requires an exact present match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
    pub frame_ref: Option<String>,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub app: AppIdentity,
    pub windows: Vec<WindowInfo>,
    /// System-wide focus facts from this exact read. This is never inferred from
    /// the logical target context below.
    pub global_focus: GlobalFocusObservation,
    /// The logical Source context being observed, independent from system focus.
    pub target_context: Option<TargetContext>,
    /// False when a read timed out or hit its traversal budget. Objects under
    /// an incomplete read are unknown, not absent.
    pub complete: bool,
    pub incomplete_reason: Option<String>,
    #[schemars(length(max = 512))]
    pub objects: Vec<UiObject>,
    /// Identity-observer state for exactly these projected objects.
    pub observer_coverage: ObserverCoverage,
    #[schemars(length(max = 8))]
    pub menu_anchors: Vec<MenuOpenerAnchor>,
    pub menu_context: Option<MenuContext>,
    pub focus_context: Option<FocusContext>,
    pub window_context: Option<SourceWindowContext>,
    /// Row-bounded actual AXVisibleRows context. This local projection never
    /// changes the meaning of `complete` for the whole accessibility tree.
    #[serde(default)]
    #[schemars(length(max = 32))]
    pub table_viewports: Vec<TableViewport>,
    /// Source-owned bounded census for one exact requested table scope. This
    /// same-read local result never changes global `complete`; its producer
    /// owns proving that a claimed complete census visited every scoped branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_scope_search: Option<TableScopeSearch>,
    /// Independent, Host-private owned-document proof. It never upgrades the
    /// generic structure completeness or source-health state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_document_evidence: Option<Box<OwnedDocumentReceiverEvidence>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Screenshot,
    Ax,
    /// Coverage of AX change/destroy notifications, independent of a current read.
    AxObserver,
    Ocr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Available,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceHealth {
    pub source: SourceKind,
    pub state: SourceState,
    pub reason: Option<String>,
    pub at_ms: u64,
    pub provenance: Option<SourceStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceStamp {
    pub session_epoch: u64,
    pub stream_id: String,
    pub source_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrameStatus {
    Complete,
    /// The display did not change; the source is alive.
    Idle,
    Blank,
    Suspended,
    Started,
    Stopped,
}

/// Metadata of one captured frame. The pixels travel on a separate bounded
/// binary channel and are not part of this message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameMeta {
    #[schemars(length(min = 1))]
    pub frame_id: String,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    pub clock_domain: String,
    pub source_sequence: u64,
    pub source_ms: u64,
    pub status: FrameStatus,
    pub geometry_revision: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub scale_factor: f64,
    pub byte_length: u64,
    pub checksum: String,
    /// Typed capture provenance. Ordinary/synthetic records may omit it;
    /// Computer use requires one explicit, complete provenance branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FrameProvenance>,
}

/// The two supported ways a frame can be associated with a live Source watch.
/// Stream callbacks retain SCK display timing; one-shot capture records carry
/// their own SCScreenshotManager request interval and never synthesize SCK time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrameProvenance {
    Stream { stream: FrameStreamInfo },
    SingleCapture { capture: SingleCaptureFrameInfo },
}

/// One bounded, same-watch `SCScreenshotManager` capture and its mapped clock
/// bounds. The capture start is a conservative lower bound; finish is an upper
/// bound. `image_frame_id` identifies PNG content, not the FrameMeta event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SingleCaptureFrameInfo {
    #[schemars(length(min = 1, max = 256))]
    #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
    pub watch_id: String,
    pub request_id: RequestId,
    #[schemars(length(min = 1, max = 256))]
    #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
    pub image_frame_id: String,
    pub pid: u32,
    #[schemars(length(min = 1, max = 256))]
    #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
    pub window_id: String,
    pub window_frame: Rect,
    pub content_rect: Rect,
    pub content_scale: f64,
    #[schemars(pattern(r"^mach_absolute$"))]
    pub source_clock: String,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub capture_started_uptime_ns: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub capture_finished_uptime_ns: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub capture_started_ms: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub capture_finished_ms: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub clock_uncertainty_ms: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub requested_after_ms: u64,
    #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
    pub deadline_ms: u64,
}

impl SingleCaptureFrameInfo {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("capture watch id", &self.watch_id)?;
        validate_menu_id("capture image id", &self.image_frame_id)?;
        validate_menu_id("capture window id", &self.window_id)?;
        if self.pid == 0 || self.pid > i32::MAX as u32 {
            return Err("single capture requires a valid process id".into());
        }
        if self.source_clock != SOURCE_MACH_ABSOLUTE_CLOCK {
            return Err("single capture requires mach_absolute source time".into());
        }
        if !self.window_frame.valid_bounded()
            || !self.content_rect.valid_bounded()
            || !self.content_scale.is_finite()
            || self.content_scale <= 0.0
        {
            return Err("single capture has invalid or unbounded window geometry".into());
        }
        for (field, value) in [
            ("capture_started_uptime_ns", self.capture_started_uptime_ns),
            (
                "capture_finished_uptime_ns",
                self.capture_finished_uptime_ns,
            ),
            ("capture_started_ms", self.capture_started_ms),
            ("capture_finished_ms", self.capture_finished_ms),
            ("clock_uncertainty_ms", self.clock_uncertainty_ms),
            ("requested_after_ms", self.requested_after_ms),
            ("deadline_ms", self.deadline_ms),
        ] {
            if value > MAX_SOURCE_SAFE_INTEGER {
                return Err(format!(
                    "single capture {field} exceeds the exact JSON integer range"
                ));
            }
        }
        if self.deadline_ms <= self.requested_after_ms
            || self.capture_started_uptime_ns == 0
            || self.clock_uncertainty_ms > 10
            || self.capture_started_uptime_ns > self.capture_finished_uptime_ns
            || self.capture_started_ms <= self.requested_after_ms
            || self.capture_started_ms > self.capture_finished_ms
            || self.capture_finished_ms > self.deadline_ms
        {
            return Err(
                "single capture timing is outside its requested barrier and deadline".into(),
            );
        }
        let native_span_ns = self.capture_finished_uptime_ns - self.capture_started_uptime_ns;
        let mapped_span_ms = self.capture_finished_ms - self.capture_started_ms;
        if mapped_span_ms < native_span_ns / 1_000_000
            || mapped_span_ms
                > native_span_ns
                    .div_ceil(1_000_000)
                    .saturating_add(self.clock_uncertainty_ms.saturating_mul(2))
                    .saturating_add(2)
        {
            return Err(
                "single capture clock bounds do not contain its native request interval".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameStreamInfo {
    pub stream_id: String,
    /// Last COMPLETE image attested by this callback; IDLE carries its identity
    /// without changing the original capture time or creating fresh pixels.
    pub image_frame_id: Option<String>,
    pub display_time_ticks: u64,
    pub display_time_ns: u64,
    pub received_uptime_ns: u64,
    /// Time of the source callback, not arrival at the consumer.
    pub received_unix_ms: u64,
    /// Callback time mapped by the producer to the declared host clock domain.
    /// This is independent of the COMPLETE image's original `source_ms`.
    pub callback_ms: u64,
    pub source_clock: String,
    pub window_id: String,
    pub pid: u32,
    /// Captured window bounds in global upper-left display points.
    pub window_frame: Rect,
    pub content_rect: Rect,
    pub content_scale: f64,
    pub dirty_rects: Vec<Rect>,
    /// Cumulative discarded frames/records within this stream.
    pub dropped_frames: u64,
    pub capture_interval_ms: u64,
}

/// Normalized window geometry borrowed from either provenance branch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameWindowGeometry<'a> {
    pub window_id: &'a str,
    pub pid: u32,
    pub window_frame: Rect,
    pub content_rect: Rect,
    pub content_scale: f64,
}

impl FrameMeta {
    pub fn stream_info(&self) -> Option<&FrameStreamInfo> {
        match self.provenance.as_ref()? {
            FrameProvenance::Stream { stream } => Some(stream),
            FrameProvenance::SingleCapture { .. } => None,
        }
    }

    pub fn stream_info_mut(&mut self) -> Option<&mut FrameStreamInfo> {
        match self.provenance.as_mut()? {
            FrameProvenance::Stream { stream } => Some(stream),
            FrameProvenance::SingleCapture { .. } => None,
        }
    }

    pub fn capture_info(&self) -> Option<&SingleCaptureFrameInfo> {
        match self.provenance.as_ref()? {
            FrameProvenance::Stream { .. } => None,
            FrameProvenance::SingleCapture { capture } => Some(capture),
        }
    }

    pub fn watch_id(&self) -> Option<&str> {
        self.stream_info()
            .map(|stream| stream.stream_id.as_str())
            .or_else(|| self.capture_info().map(|capture| capture.watch_id.as_str()))
    }

    pub fn image_frame_id(&self) -> Option<&str> {
        self.stream_info()
            .and_then(|stream| stream.image_frame_id.as_deref())
            .or_else(|| {
                self.capture_info()
                    .map(|capture| capture.image_frame_id.as_str())
            })
    }

    pub fn window_geometry(&self) -> Option<FrameWindowGeometry<'_>> {
        if let Some(stream) = self.stream_info() {
            return Some(FrameWindowGeometry {
                window_id: &stream.window_id,
                pid: stream.pid,
                window_frame: stream.window_frame,
                content_rect: stream.content_rect,
                content_scale: stream.content_scale,
            });
        }
        self.capture_info().map(|capture| FrameWindowGeometry {
            window_id: &capture.window_id,
            pid: capture.pid,
            window_frame: capture.window_frame,
            content_rect: capture.content_rect,
            content_scale: capture.content_scale,
        })
    }

    /// Validate provenance-specific timing/status facts. Stream liveness and
    /// stream callback policy remain owned by the watcher/consumer.
    pub fn validate_provenance(&self) -> Result<(), String> {
        if let Some(capture) = self.capture_info() {
            capture.validate()?;
            if self.status != FrameStatus::Complete
                || self.source_ms != capture.capture_started_ms
                || self.session_epoch == 0
                || self.surface.epoch == 0
                || self.source_sequence == 0
                || self.geometry_revision == 0
            {
                return Err(
                    "single capture FrameMeta must be complete and use its start lower bound"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

/// Commands the host may send to the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceCommand {
    /// Read the structure again now.
    RefreshNow {
        /// Optional public table ancestry/header scope; contains no row selector.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_scope: Option<RowTableScope>,
    },
    /// Ask the current watch to capture its retained SCWindow without restarting it.
    CaptureNow {
        request_id: RequestId,
        #[schemars(length(min = 1, max = 256))]
        #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
        watch_id: String,
        #[schemars(range(min = 1, max = MAX_SOURCE_SAFE_INTEGER))]
        session_epoch: u64,
        surface: SurfaceRef,
        #[schemars(range(max = MAX_SOURCE_SAFE_INTEGER))]
        after_ms: u64,
        #[schemars(range(min = 1, max = MAX_SOURCE_SAFE_INTEGER))]
        deadline_ms: u64,
    },
    /// Cancel one pending capture without changing watch authority.
    CancelCapture {
        request_id: RequestId,
        #[schemars(length(min = 1, max = 256))]
        #[schemars(pattern(SOURCE_FRAME_REF_PATTERN))]
        watch_id: String,
    },
}

impl SourceCommand {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::RefreshNow { table_scope } => {
                if let Some(scope) = table_scope {
                    scope.validate()?;
                }
                Ok(())
            }
            Self::CaptureNow {
                watch_id,
                session_epoch,
                surface,
                after_ms,
                deadline_ms,
                ..
            } => {
                validate_menu_id("capture watch id", watch_id)?;
                validate_menu_id("capture surface id", &surface.id)?;
                if *session_epoch == 0
                    || *session_epoch > MAX_SOURCE_SAFE_INTEGER
                    || surface.epoch == 0
                    || surface.epoch > MAX_SOURCE_SAFE_INTEGER
                    || *deadline_ms <= *after_ms
                    || *after_ms > MAX_SOURCE_SAFE_INTEGER
                    || *deadline_ms > MAX_SOURCE_SAFE_INTEGER
                {
                    return Err("invalid or unbounded capture request scope/deadline".into());
                }
                Ok(())
            }
            Self::CancelCapture { watch_id, .. } => validate_menu_id("capture watch id", watch_id),
        }
    }
}

#[cfg(test)]
mod source_command_scope_tests {
    use super::*;
    use crate::desktop::task::{RowContainerRole, RowContainerScope};

    fn scope() -> RowTableScope {
        RowTableScope {
            scroll_path: vec![RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Archive".into(),
            }],
            column_headers: vec!["Name".into()],
        }
    }

    #[test]
    fn refresh_scope_is_optional_and_validated_without_serializing_none() {
        let omitted = SourceCommand::RefreshNow { table_scope: None };
        assert!(omitted.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&omitted).unwrap(),
            serde_json::json!({"command": "refresh_now"})
        );
        assert_eq!(
            serde_json::from_value::<SourceCommand>(serde_json::json!({
                "command": "refresh_now"
            }))
            .unwrap(),
            omitted
        );

        let requested = SourceCommand::RefreshNow {
            table_scope: Some(scope()),
        };
        assert!(requested.validate().is_ok());
        assert_eq!(
            serde_json::from_value::<SourceCommand>(serde_json::to_value(&requested).unwrap())
                .unwrap(),
            requested
        );

        let invalid = SourceCommand::RefreshNow {
            table_scope: Some(RowTableScope {
                scroll_path: Vec::new(),
                column_headers: vec!["Name".into()],
            }),
        };
        assert!(invalid.validate().is_err());
    }
}

impl StructureSnapshot {
    fn validate_window_context(&self) -> Result<(), String> {
        let Some(context) = &self.window_context else {
            return Ok(());
        };
        context.validate()?;
        if !self.complete {
            let scope = self
                .table_scope_search
                .as_ref()
                .ok_or("partial window facts require a same-read unique table scope")?;
            scope.validate_against_snapshot(self)?;
            if scope.unique_candidate().is_none()
                || scope.window_id != context.origin_window_id
                || scope.process_id != context.process.pid
                || self.focus_context.is_some()
                || self.objects.iter().any(|object| object.secure)
            {
                return Err("partial window facts require their exact scoped normal window".into());
            }
        }
        if self.menu_context.is_some()
            || self
                .objects
                .iter()
                .any(|object| matches!(object.role.as_str(), "AXMenu" | "AXMenuItem" | "AXMenuBar"))
            || context.session_epoch != self.session_epoch
            || context.verified_source_sequence != self.source_sequence
            || context.verified_observed_ms != self.observed_ms
            || context.process.pid != self.app.pid
            || !matches!(
                &self.global_focus.app,
                ObservationFact::Known { value } if value == &self.app
            )
            || !matches!(
                &self.global_focus.window_id,
                ObservationFact::Known { value } if value == &context.origin_window_id
            )
        {
            return Err(
                "window context requires normal Source with exact current provenance".into(),
            );
        }
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window.window_id == context.origin_window_id)
            .collect();
        if windows.len() != 1
            || windows[0].kind != LayerKind::Window
            || windows[0].modal
            || windows[0].owner_bundle_id != self.app.bundle_id
            || windows[0].frame != context.origin_window_frame
        {
            return Err(
                "window context requires its unique actual nonmodal standard window".into(),
            );
        }
        let roots: Vec<_> = self
            .objects
            .iter()
            .filter(|object| {
                object.window_id == context.origin_window_id && object.parent.is_none()
            })
            .collect();
        if roots.len() != 1
            || roots[0].role != "AXWindow"
            || roots[0].subrole.as_deref() != Some("AXStandardWindow")
            || !roots[0].in_view
            || roots[0].secure
            || roots[0].frame != context.origin_window_frame
        {
            return Err("window context requires its exact visible nonsecure Source root".into());
        }
        if self.focus_context.as_ref().is_some_and(|focus| {
            focus.source_session_id != context.source_session_id
                || focus.session_epoch != context.session_epoch
                || focus.process != context.process
                || focus.origin_window_id != context.origin_window_id
                || focus.origin_window_frame != context.origin_window_frame
                || focus.verified_source_sequence != context.verified_source_sequence
                || focus.verified_observed_ms != context.verified_observed_ms
        }) {
            return Err(
                "focus and normal-window facts must come from the same actual Source read".into(),
            );
        }
        Ok(())
    }

    fn validate_focus(&self) -> Result<(), String> {
        let Some(context) = &self.focus_context else {
            return Ok(());
        };
        context.validate()?;
        if !self.complete
            || self.menu_context.is_some()
            || self
                .objects
                .iter()
                .any(|object| matches!(object.role.as_str(), "AXMenu" | "AXMenuItem" | "AXMenuBar"))
            || context.session_epoch != self.session_epoch
            || context.verified_source_sequence != self.source_sequence
            || context.verified_observed_ms != self.observed_ms
            || context.process.pid != self.app.pid
            || context.target_fingerprint.app != self.app.bundle_id
            || !matches!(
                &self.global_focus.app,
                ObservationFact::Known { value } if value == &self.app
            )
            || !matches!(
                &self.global_focus.window_id,
                ObservationFact::Known { value } if value == &context.origin_window_id
            )
        {
            return Err(
                "focus requires a complete normal Source read with exact current provenance".into(),
            );
        }
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window.window_id == context.origin_window_id)
            .collect();
        if windows.len() != 1
            || windows[0].kind != LayerKind::Window
            || windows[0].modal
            || windows[0].owner_bundle_id != self.app.bundle_id
            || windows[0].frame != context.origin_window_frame
        {
            return Err("focus requires its actual fixed standard window".into());
        }
        let target = self
            .objects
            .iter()
            .find(|object| object.instance_ref == context.target_instance)
            .ok_or("focused target is absent from the current Source tree")?;
        if !target.focused
            || !target.enabled
            || !target.in_view
            || target.secure
            || target.window_id != context.origin_window_id
            || target.frame != context.target_frame
            || instance_fingerprint(self, target) != context.target_fingerprint
        {
            return Err("focus does not match the exact actual current target descriptor".into());
        }
        let mut cursor = target;
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..=32 {
            if !visited.insert(cursor.instance_ref.as_str())
                || cursor.window_id != context.origin_window_id
                || cursor.secure
            {
                return Err("cyclic, secure, or cross-window focused parent chain".into());
            }
            if cursor.role == "AXWindow" {
                if cursor.parent.is_some()
                    || cursor.subrole.as_deref() != Some("AXStandardWindow")
                    || cursor.frame != context.origin_window_frame
                {
                    return Err(
                        "focused parent chain requires its actual standard-window root".into(),
                    );
                }
                return Ok(());
            }
            if matches!(
                cursor.role.as_str(),
                "AXMenu" | "AXMenuItem" | "AXMenuBar" | "AXApplication" | "AXSheet" | "AXDialog"
            ) {
                return Err("unsupported focused parent layer".into());
            }
            cursor = self
                .objects
                .iter()
                .find(|object| Some(&object.instance_ref) == cursor.parent.as_ref())
                .ok_or("focused parent chain does not reach its Source window")?;
        }
        Err("focused parent chain exceeds its traversal bound".into())
    }

    fn validate_menu_anchor(&self, anchor: &MenuOpenerAnchor) -> Result<(), String> {
        anchor.validate()?;
        if !self.complete
            || self.source_sequence == 0
            || anchor.verified_source_sequence != self.source_sequence
            || anchor.verified_observed_ms != self.observed_ms
            || anchor.process.pid != self.app.pid
            || anchor.opener_fingerprint.app != self.app.bundle_id
            || !matches!(
                &self.global_focus.app,
                ObservationFact::Known { value } if value == &self.app
            )
            || self.global_focus.known_window_id() != Some(anchor.origin_window_id.as_str())
        {
            return Err("menu anchor lacks this complete active source read's provenance".into());
        }
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter(|w| w.window_id == anchor.origin_window_id)
            .collect();
        if windows.len() != 1
            || windows[0].kind != LayerKind::Window
            || windows[0].modal
            || windows[0].owner_bundle_id != self.app.bundle_id
            || windows[0].frame != anchor.origin_window_frame
        {
            return Err("menu anchor requires the exact original standard window".into());
        }
        let opener = self
            .objects
            .iter()
            .find(|o| o.instance_ref == anchor.opener_instance)
            .ok_or("menu opener instance is absent from the current source tree")?;
        if !opener.enabled
            || !opener.in_view
            || opener.secure
            || opener.window_id != anchor.origin_window_id
            || !opener.actions.contains(&AxActionName::ShowMenu)
            || opener.frame != anchor.opener_frame
            || instance_fingerprint(self, opener) != anchor.opener_fingerprint
        {
            return Err("menu opener does not match its actual current source object".into());
        }
        let mut cursor = opener;
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..=32 {
            if !visited.insert(cursor.instance_ref.as_str())
                || cursor.window_id != anchor.origin_window_id
                || cursor.secure
            {
                return Err("cyclic, secure, or cross-window menu opener parent chain".into());
            }
            if cursor.role == "AXWindow" {
                if cursor.parent.is_some()
                    || cursor.subrole.as_deref() != Some("AXStandardWindow")
                    || cursor.frame != anchor.origin_window_frame
                {
                    return Err(
                        "menu opener chain must end at its actual original standard window root"
                            .into(),
                    );
                }
                return Ok(());
            }
            if matches!(
                cursor.role.as_str(),
                "AXMenu" | "AXMenuItem" | "AXMenuBar" | "AXApplication" | "AXSheet" | "AXDialog"
            ) {
                return Err("unsupported layer in menu opener parent chain".into());
            }
            cursor = self
                .objects
                .iter()
                .find(|o| Some(&o.instance_ref) == cursor.parent.as_ref())
                .ok_or("menu opener parent chain does not reach its source window")?;
        }
        Err("menu opener parent chain exceeds its traversal bound".into())
    }

    fn validate_menus(&self) -> Result<(), String> {
        if self.menu_anchors.len() > 8 {
            return Err("at most eight source-owned menu anchors are allowed".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut openers = std::collections::BTreeSet::new();
        for anchor in &self.menu_anchors {
            self.validate_menu_anchor(anchor)?;
            if !ids.insert(anchor.anchor_id.as_str())
                || !openers.insert(anchor.opener_instance.as_str())
                || self
                    .menu_anchors
                    .first()
                    .is_some_and(|a| a.source_session_id != anchor.source_session_id)
            {
                return Err(
                    "menu anchors must have unique identities and openers in one source session"
                        .into(),
                );
            }
        }
        let menus: Vec<_> = self.objects.iter().filter(|o| o.role == "AXMenu").collect();
        let Some(context) = &self.menu_context else {
            if self
                .objects
                .iter()
                .any(|o| matches!(o.role.as_str(), "AXMenu" | "AXMenuItem" | "AXMenuBar"))
                && (self.complete || !self.menu_anchors.is_empty())
            {
                return Err("an unproved menu cannot retain complete normal input evidence or opener anchors".into());
            }
            return Ok(());
        };
        context.validate()?;
        self.validate_menu_anchor(&context.opener)?;
        if !self
            .menu_anchors
            .iter()
            .any(|anchor| anchor == &context.opener)
            || menus.len() != 1
            || menus[0].instance_ref != context.menu_instance
            || self.windows.iter().any(|w| {
                w.modal
                    || matches!(
                        w.kind,
                        LayerKind::Menu | LayerKind::Sheet | LayerKind::Dialog | LayerKind::Popover
                    )
            })
            || self
                .objects
                .iter()
                .any(|o| matches!(o.role.as_str(), "AXMenuBar" | "AXSheet" | "AXDialog"))
        {
            return Err("menu context must name the unique source menu and one current registered anchor without competing layers".into());
        }
        let menu = menus[0];
        if menu.parent.as_ref() != Some(&context.opener.opener_instance)
            || menu.window_id != context.opener.origin_window_id
            || menu.secure
            || !menu.enabled
            || !menu.in_view
            || menu.frame != context.menu_frame
            || instance_fingerprint(self, menu) != context.menu_fingerprint
        {
            return Err("menu context does not match the actual level-one source menu".into());
        }
        let mut items = 0;
        for object in &self.objects {
            if object.role == "AXMenuItem" {
                items += 1;
                if object.parent.as_ref() != Some(&context.menu_instance)
                    || object.window_id != context.opener.origin_window_id
                    || object.secure
                    || object.in_view && !context.menu_frame.contains_rect(&object.frame)
                {
                    return Err("menu items must be nonsecure direct children of the current original-window menu".into());
                }
            }
        }
        if items == 0 {
            return Err("a current menu context requires actual menu items".into());
        }
        Ok(())
    }

    fn validate_table_viewports(&self) -> Result<(), String> {
        if self.table_viewports.len() > 32 {
            return Err("at most 32 table viewport projections are allowed".into());
        }
        let objects: std::collections::BTreeMap<_, _> = self
            .objects
            .iter()
            .map(|object| (object.instance_ref.as_str(), object))
            .collect();
        let mut tables = std::collections::BTreeSet::new();

        for viewport in &self.table_viewports {
            if !tables.insert(viewport.table_instance_ref.as_str())
                || viewport.process_id != self.app.pid
                || viewport.process_id == 0
                || viewport.process_id > i32::MAX as u32
                || viewport.visible_rows.len() > 256
                || viewport.columns.len() > 64
                || !viewport.table_frame.valid_bounded()
                || !viewport.viewport_frame.valid_bounded()
            {
                return Err(
                    "invalid or duplicate table viewport identity, process, geometry, or size"
                        .into(),
                );
            }

            let table = objects
                .get(viewport.table_instance_ref.as_str())
                .ok_or("table viewport must reference its actual AXTable source object")?;
            let scroll = objects
                .get(viewport.scroll_container_instance_ref.as_str())
                .ok_or("table viewport must reference its actual AXScrollArea source object")?;
            if table.role != "AXTable"
                || scroll.role != "AXScrollArea"
                || table.window_id != scroll.window_id
                || table.frame != viewport.table_frame
                || !viewport.ax_enabled.matches_generic_enabled(table.enabled)
                || !scroll.frame.contains_rect(&viewport.viewport_frame)
                || !table.frame.intersects(&viewport.viewport_frame)
                || !self.source_ancestor_reaches(table, &viewport.scroll_container_instance_ref)
            {
                return Err("table viewport frames or actual table/scroll parent context do not match Source".into());
            }

            let mut context_refs = std::collections::BTreeSet::from([
                viewport.table_instance_ref.as_str(),
                viewport.scroll_container_instance_ref.as_str(),
            ]);
            let mut identifier_set = std::collections::BTreeSet::new();
            let mut column_refs = std::collections::BTreeSet::new();
            let mut column_ax_indices = std::collections::BTreeMap::new();
            for column in &viewport.columns {
                if column.instance_ref.as_ref().is_some_and(|reference| {
                    !context_refs.insert(reference.as_str())
                        || !column_refs.insert(reference.as_str())
                }) || !column.frame.valid_bounded()
                    || column
                        .ax_index
                        .is_some_and(|index| index > MAX_TABLE_VIEWPORT_COLUMN_INDEX)
                    || !(column.frame.intersects(&viewport.table_frame)
                        || column.frame.intersects(&scroll.frame))
                    || !matches!(
                        column.parent_instance_ref.as_str(),
                        parent if parent == viewport.table_instance_ref.as_str()
                            || parent == viewport.scroll_container_instance_ref.as_str()
                    )
                    || !optional_table_text_is_valid(column.header.as_deref(), 2_048)
                {
                    return Err(
                        "invalid table column identity, parent, geometry, or header context".into(),
                    );
                }
                if let Some(reference) = &column.instance_ref {
                    column_ax_indices.insert(reference.as_str(), column.ax_index);
                    let object = objects
                        .get(reference.as_str())
                        .ok_or("table column reference is absent from actual Source objects")?;
                    if object.role != "AXColumn"
                        || object.parent.as_ref() != Some(&column.parent_instance_ref)
                        || object.frame != column.frame
                        || object.window_id != table.window_id
                    {
                        return Err(
                            "table column context does not match its actual Source object".into(),
                        );
                    }
                }
                if let Some(header) = &column.header_instance_ref {
                    let object = objects.get(header.as_str()).ok_or(
                        "table column header reference is absent from actual Source objects",
                    )?;
                    if object.window_id != table.window_id {
                        return Err("table column header belongs to another source window".into());
                    }
                }
            }

            let mut total_cells = 0usize;
            for row in &viewport.visible_rows {
                if row
                    .instance_ref
                    .as_ref()
                    .is_some_and(|reference| !context_refs.insert(reference.as_str()))
                    || row.parent_instance_ref != viewport.table_instance_ref
                    || !row.frame.valid_bounded()
                    || !viewport.table_frame.contains_rect(&row.frame)
                    || !row.frame.intersects(&viewport.viewport_frame)
                    || row.cells.len() > 128
                    || !optional_table_text_is_valid(row.accessibility_identifier.as_deref(), 256)
                {
                    return Err(
                        "invalid actual visible row identity, parent, frame, or size".into(),
                    );
                }
                if let Some(identifier) = &row.accessibility_identifier
                    && !identifier_set.insert(identifier.as_str())
                {
                    return Err("duplicate AXIdentifier in one visible table viewport".into());
                }
                if let Some(reference) = &row.instance_ref {
                    let object = objects
                        .get(reference.as_str())
                        .ok_or("table row reference is absent from actual Source objects")?;
                    if object.role != "AXRow"
                        || object.parent.as_ref() != Some(&row.parent_instance_ref)
                        || object.frame != row.frame
                        || object.window_id != table.window_id
                        || !row.ax_enabled.matches_generic_enabled(object.enabled)
                    {
                        return Err(
                            "table row context does not match its actual Source object".into()
                        );
                    }
                }

                total_cells = total_cells.saturating_add(row.cells.len());
                if total_cells > 2_048 {
                    return Err("table viewport exceeds its cell context budget".into());
                }
                for cell in &row.cells {
                    if cell.instance_ref.is_some() && row.instance_ref.is_none() {
                        return Err(
                            "a referenced table cell requires its actual referenced row".into()
                        );
                    }
                    if cell
                        .column_ax_index
                        .is_some_and(|index| index > MAX_TABLE_VIEWPORT_COLUMN_INDEX)
                    {
                        return Err("table cell AXColumn index exceeds the 64-column bound".into());
                    }
                    if row
                        .ax_index
                        .zip(cell.row_ax_index)
                        .is_some_and(|(row_index, cell_row_index)| row_index != cell_row_index)
                    {
                        return Err("table cell row AXIndex does not match its actual AXRow".into());
                    }
                    if let (Some(cell_index), Some(column_ref)) =
                        (cell.column_ax_index, cell.column_instance_ref.as_ref())
                        && column_ax_indices
                            .get(column_ref.as_str())
                            .copied()
                            .flatten()
                            .is_some_and(|column_index| column_index != cell_index)
                    {
                        return Err(
                            "table cell column AXIndex does not match its actual AXColumn".into(),
                        );
                    }
                    if cell
                        .instance_ref
                        .as_ref()
                        .is_some_and(|reference| !context_refs.insert(reference.as_str()))
                        || cell.parent_instance_ref != row.instance_ref
                        || !cell.frame.valid_bounded()
                        || !row.frame.contains_rect(&cell.frame)
                        || cell
                            .column_instance_ref
                            .as_ref()
                            .is_some_and(|column| !column_refs.contains(column.as_str()))
                        || !optional_table_text_is_valid(cell.column_header.as_deref(), 2_048)
                        || !cell.value.as_ref().is_none_or(valid_table_value)
                    {
                        return Err(
                            "invalid table cell identity, parent, frame, value, or column context"
                                .into(),
                        );
                    }
                    if let Some(reference) = &cell.instance_ref {
                        let object = objects
                            .get(reference.as_str())
                            .ok_or("table cell reference is absent from actual Source objects")?;
                        if object.role != "AXCell"
                            || object.parent.as_ref() != cell.parent_instance_ref.as_ref()
                            || object.frame != cell.frame
                            || object.window_id != table.window_id
                        {
                            return Err(
                                "table cell context does not match its actual Source object".into(),
                            );
                        }
                    }
                    for header in [
                        cell.header_instance_ref.as_ref(),
                        cell.column_instance_ref.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        let object = objects
                            .get(header.as_str())
                            .ok_or("table cell column/header reference is absent from actual Source objects")?;
                        if object.window_id != table.window_id {
                            return Err(
                                "table cell context belongs to another source window".into()
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn source_ancestor_reaches(&self, object: &UiObject, ancestor: &ObjectRef) -> bool {
        let objects: std::collections::BTreeMap<_, _> = self
            .objects
            .iter()
            .map(|object| (object.instance_ref.as_str(), object))
            .collect();
        let mut current = object.parent.as_ref();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..=self.objects.len().min(512) {
            let Some(reference) = current else {
                return false;
            };
            if reference == ancestor {
                return true;
            }
            if !seen.insert(reference.as_str()) {
                return false;
            }
            current = objects
                .get(reference.as_str())
                .and_then(|parent| parent.parent.as_ref());
        }
        false
    }

    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("clock_domain", &self.clock_domain)?;
        require_nonempty("app.bundle_id", &self.app.bundle_id)?;
        if let Some(frame_ref) = &self.frame_ref {
            validate_menu_id("frame_ref", frame_ref)?;
        }
        if let Some(evidence) = &self.owned_document_evidence {
            evidence.validate()?;
            let provenance = &evidence.provenance;
            if self.app.bundle_id != "com.apple.TextEdit"
                || evidence.process.pid != self.app.pid
                || provenance.snapshot_id != self.snapshot_id
                || provenance.source_sequence != self.source_sequence
                || provenance.clock_domain != self.clock_domain
                || provenance.observed_ms > self.observed_ms
                || self.observed_ms >= provenance.valid_until_ms
            {
                return Err("owned document evidence does not match its Source snapshot".into());
            }
        }
        if let Some(identity_boundary) = &self.identity_boundary {
            identity_boundary.validate()?;
        }
        if !self.complete && self.incomplete_reason.is_none() {
            return Err("an incomplete snapshot must say why".into());
        }
        self.observer_coverage.validate(&self.objects)?;
        let mut seen = std::collections::BTreeSet::new();
        for object in &self.objects {
            if object
                .ax_action_names
                .as_ref()
                .is_some_and(|names| object.secure || !valid_raw_ax_action_names(names))
            {
                return Err(
                    "raw AX actions must be a complete bounded nonsecure declaration".into(),
                );
            }
            if object.scroll.as_ref().is_some_and(|scroll| {
                object.role != "AXScrollArea" || object.secure || !scroll.valid()
            }) {
                return Err(
                    "scroll metrics require a nonsecure scroll area and finite actual ranges"
                        .into(),
                );
            }
            if !seen.insert(object.instance_ref.as_str()) {
                return Err(format!("duplicate object {}", object.instance_ref));
            }
            if !self
                .windows
                .iter()
                .any(|window| window.window_id == object.window_id)
            {
                return Err(format!(
                    "object {} names an unknown window",
                    object.instance_ref
                ));
            }
        }
        self.validate_table_viewports()?;
        if let Some(search) = &self.table_scope_search {
            search.validate_against_snapshot(self)?;
        }
        self.global_focus.validate()?;
        if let (
            ObservationFact::Known { value: focus_app },
            ObservationFact::Known {
                value: focus_window,
            },
        ) = (&self.global_focus.app, &self.global_focus.window_id)
            && focus_app == &self.app
            && !self
                .windows
                .iter()
                .any(|window| &window.window_id == focus_window)
        {
            return Err("known focused window is absent from the same-app Source windows".into());
        }
        if let Some(target_context) = &self.target_context {
            target_context.validate(self)?;
        }
        self.validate_menus()?;
        self.validate_focus()?;
        self.validate_window_context()
    }
}

fn valid_raw_ax_action_names(names: &[String]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    names.len() <= 32
        && names.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 64
                && !name.chars().any(char::is_control)
                && seen.insert(name)
        })
}

fn optional_table_text_is_valid(value: Option<&str>, maximum_bytes: usize) -> bool {
    value.is_none_or(|text| text.len() <= maximum_bytes && !text.chars().any(char::is_control))
}

fn valid_table_value(value: &UiValue) -> bool {
    match value {
        UiValue::Bool(_) => true,
        UiValue::Number(number) => number.is_finite(),
        UiValue::Text(text) => text.len() <= 2_048 && !text.chars().any(char::is_control),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> StructureSnapshot {
        StructureSnapshot {
            window_context: None,
            owned_document_evidence: None,
            focus_context: None,
            menu_anchors: vec![],
            menu_context: None,
            identity_boundary: None,
            frame_ref: None,
            snapshot_id: RequestId::new("snap-1").unwrap(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: 1,
            observed_ms: 10,
            app: AppIdentity {
                bundle_id: "test.app".into(),
                name: "Test".into(),
                pid: 42,
            },
            windows: vec![WindowInfo {
                window_id: "w1".into(),
                title: Some("Settings".into()),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 400.0,
                    height: 300.0,
                },
                z_index: 0,
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: "test.app".into(),
            }],
            global_focus: GlobalFocusObservation {
                app: ObservationFact::Known {
                    value: AppIdentity {
                        bundle_id: "test.app".into(),
                        name: "Test".into(),
                        pid: 42,
                    },
                },
                window_id: ObservationFact::Known { value: "w1".into() },
            },
            target_context: None,
            complete: true,
            incomplete_reason: None,
            objects: vec![],
            observer_coverage: ObserverCoverage {
                state: SourceState::Available,
                covered_objects: 0,
                read_scoped_objects: 0,
                failed_registrations: 0,
                failures: vec![],
            },
            table_viewports: vec![],
            table_scope_search: None,
        }
    }

    fn table_test_object(
        instance_ref: &str,
        parent: Option<&str>,
        role: &str,
        frame: Rect,
    ) -> UiObject {
        UiObject {
            instance_ref: ObjectRef::new(instance_ref).unwrap(),
            parent: parent.map(|parent| ObjectRef::new(parent).unwrap()),
            window_id: "w1".into(),
            role: role.into(),
            subrole: None,
            title: None,
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame,
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        }
    }

    fn table_viewport_snapshot() -> StructureSnapshot {
        let mut snapshot = snapshot();
        let root_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 300.0,
        };
        let scroll_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 100.0,
        };
        let table_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 500.0,
        };
        let row_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 30.0,
        };
        let cell_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 150.0,
            height: 30.0,
        };
        let root = table_test_object("root", None, "AXWindow", root_frame);
        let scroll = table_test_object("scroll", Some("root"), "AXScrollArea", scroll_frame);
        let table = table_test_object("table", Some("scroll"), "AXTable", table_frame);
        let row = table_test_object("row", Some("table"), "AXRow", row_frame);
        let cell = table_test_object("cell", Some("row"), "AXCell", cell_frame);
        let column = table_test_object("column", Some("table"), "AXColumn", cell_frame);
        let header = table_test_object("header", Some("table"), "AXButton", cell_frame);
        snapshot.objects = vec![root, scroll, table, row, cell, column, header];
        snapshot.observer_coverage.covered_objects = snapshot.objects.len() as u32;
        snapshot.complete = false;
        snapshot.incomplete_reason = Some("full-tree budget reached".into());
        snapshot.table_viewports = vec![TableViewport {
            table_instance_ref: ObjectRef::new("table").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll").unwrap(),
            process_id: snapshot.app.pid,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: scroll_frame,
            visible_rows_complete: false,
            visible_rows: vec![TableViewportRow {
                instance_ref: Some(ObjectRef::new("row").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: Some("opaque-row-id".into()),
                ax_index: None,
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells: vec![TableViewportCell {
                    instance_ref: Some(ObjectRef::new("cell").unwrap()),
                    parent_instance_ref: Some(ObjectRef::new("row").unwrap()),
                    frame: cell_frame,
                    row_ax_index: None,
                    column_ax_index: None,
                    column_instance_ref: Some(ObjectRef::new("column").unwrap()),
                    header_instance_ref: Some(ObjectRef::new("header").unwrap()),
                    column_header: Some("Name".into()),
                    value: Some(UiValue::Text("Quarterly report".into())),
                }],
            }],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(ObjectRef::new("column").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: cell_frame,
                header_instance_ref: Some(ObjectRef::new("header").unwrap()),
                header: Some("Name".into()),
                ax_index: None,
            }],
        }];
        snapshot
    }

    #[test]
    fn row_ax_enabled_observation_is_typed_and_mandatory() {
        let observations = [
            (AxEnabledObservation::Enabled, "enabled"),
            (AxEnabledObservation::Disabled, "disabled"),
            (
                AxEnabledObservation::AttributeUnsupported,
                "attribute_unsupported",
            ),
            (AxEnabledObservation::Unavailable, "unavailable"),
        ];
        for (observation, expected) in observations {
            let wire = serde_json::to_value(observation).unwrap();
            assert_eq!(wire, serde_json::Value::String(expected.into()));
            assert_eq!(
                serde_json::from_value::<AxEnabledObservation>(wire).unwrap(),
                observation
            );
        }

        let mut missing_table = serde_json::to_value(table_viewport_snapshot()).unwrap();
        missing_table["table_viewports"][0]
            .as_object_mut()
            .unwrap()
            .remove("ax_enabled");
        assert!(serde_json::from_value::<StructureSnapshot>(missing_table).is_err());

        let mut missing_row = serde_json::to_value(table_viewport_snapshot()).unwrap();
        missing_row["table_viewports"][0]["visible_rows"][0]
            .as_object_mut()
            .unwrap()
            .remove("ax_enabled");
        assert!(serde_json::from_value::<StructureSnapshot>(missing_row).is_err());
    }

    #[test]
    fn table_and_row_ax_enabled_facts_match_the_generic_fail_closed_projection() {
        let valid = table_viewport_snapshot();
        assert!(valid.validate().is_ok());

        let mut table_mismatch = valid.clone();
        table_mismatch.table_viewports[0].ax_enabled = AxEnabledObservation::AttributeUnsupported;
        assert!(table_mismatch.validate().is_err());

        let mut row_mismatch = valid.clone();
        row_mismatch.table_viewports[0].visible_rows[0].ax_enabled = AxEnabledObservation::Disabled;
        assert!(row_mismatch.validate().is_err());

        let mut unsupported = valid;
        unsupported.table_viewports[0].ax_enabled = AxEnabledObservation::AttributeUnsupported;
        unsupported.table_viewports[0].visible_rows[0].ax_enabled =
            AxEnabledObservation::AttributeUnsupported;
        for object in &mut unsupported.objects {
            if matches!(object.instance_ref.as_str(), "table" | "row") {
                object.enabled = false;
            }
        }
        assert!(unsupported.validate().is_ok());
    }

    #[test]
    fn rect_geometry_is_half_open() {
        let rect = Rect {
            x: 10.0,
            y: 10.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(rect.contains(10.0, 10.0));
        assert!(!rect.contains(20.0, 15.0));
        assert_eq!(rect.center(), (15.0, 15.0));
        let touching = Rect {
            x: 20.0,
            y: 10.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(!rect.intersects(&touching));
        let overlapping = Rect {
            x: 19.0,
            y: 10.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(rect.intersects(&overlapping));
    }

    #[test]
    fn table_viewport_refs_must_resolve_and_unknown_refs_stay_nil() {
        let valid = table_viewport_snapshot();
        assert!(!valid.complete);
        assert!(!valid.table_viewports[0].visible_rows_complete);
        assert!(valid.validate().is_ok());

        for missing in ["row", "cell", "column", "header"] {
            let mut snapshot = valid.clone();
            snapshot
                .objects
                .retain(|object| object.instance_ref.as_str() != missing);
            assert!(snapshot.validate().is_err(), "dangling reference {missing}");
        }

        let mut unknown = valid.clone();
        let viewport = &mut unknown.table_viewports[0];
        viewport.visible_rows[0].instance_ref = None;
        viewport.visible_rows[0].cells[0].instance_ref = None;
        viewport.visible_rows[0].cells[0].parent_instance_ref = None;
        viewport.visible_rows[0].cells[0].column_instance_ref = None;
        viewport.visible_rows[0].cells[0].header_instance_ref = None;
        viewport.columns[0].instance_ref = None;
        viewport.columns[0].header_instance_ref = None;
        assert!(
            unknown.validate().is_ok(),
            "nil references preserve read-only context"
        );

        let mut cell_without_row_ref = valid;
        let row = &mut cell_without_row_ref.table_viewports[0].visible_rows[0];
        row.instance_ref = None;
        row.cells[0].parent_instance_ref = None;
        assert!(cell_without_row_ref.validate().is_err());
    }

    #[test]
    fn table_column_ax_index_is_bounded_and_unknown_remains_a_read_only_fact() {
        let unknown = table_viewport_snapshot();
        assert_eq!(unknown.table_viewports[0].columns[0].ax_index, None);
        assert!(unknown.validate().is_ok());

        let mut wire = serde_json::to_value(&unknown).unwrap();
        wire["table_viewports"][0]["columns"][0]
            .as_object_mut()
            .unwrap()
            .remove("ax_index");
        let omitted_index: StructureSnapshot = serde_json::from_value(wire).unwrap();
        assert_eq!(omitted_index.table_viewports[0].columns[0].ax_index, None);
        assert!(omitted_index.validate().is_ok());

        for index in [0, MAX_TABLE_VIEWPORT_COLUMN_INDEX] {
            let mut known = table_viewport_snapshot();
            known.table_viewports[0].columns[0].ax_index = Some(index);
            assert!(known.validate().is_ok(), "valid AXIndex {index}");
        }

        let mut out_of_range = table_viewport_snapshot();
        out_of_range.table_viewports[0].columns[0].ax_index =
            Some(MAX_TABLE_VIEWPORT_COLUMN_INDEX + 1);
        assert!(out_of_range.validate().is_err());
    }

    #[test]
    fn actual_row_and_cell_ax_indices_are_never_inferred_from_visible_ordinal() {
        for index in [42, 511, MAX_TABLE_VIEWPORT_ROW_AX_INDEX] {
            let mut known = table_viewport_snapshot();
            let viewport = &mut known.table_viewports[0];
            viewport.visible_rows[0].ax_index = Some(index);
            viewport.visible_rows[0].cells[0].row_ax_index = Some(index);
            viewport.columns[0].ax_index = Some(7);
            viewport.visible_rows[0].cells[0].column_ax_index = Some(7);
            assert_eq!(
                known.table_viewports[0].visible_rows[0].ax_index,
                Some(index)
            );
            assert!(
                known.validate().is_ok(),
                "actual AXIndex {index} at ordinal zero"
            );
        }

        let mut unknown = table_viewport_snapshot();
        unknown.table_viewports[0].columns[0].ax_index = None;
        assert!(unknown.validate().is_ok());
        let mut wire = serde_json::to_value(&unknown).unwrap();
        wire["table_viewports"][0]["visible_rows"][0]
            .as_object_mut()
            .unwrap()
            .remove("ax_index");
        wire["table_viewports"][0]["visible_rows"][0]["cells"][0]
            .as_object_mut()
            .unwrap()
            .remove("row_ax_index");
        wire["table_viewports"][0]["visible_rows"][0]["cells"][0]
            .as_object_mut()
            .unwrap()
            .remove("column_ax_index");
        let omitted: StructureSnapshot = serde_json::from_value(wire).unwrap();
        assert_eq!(omitted.table_viewports[0].visible_rows[0].ax_index, None);
        assert_eq!(
            omitted.table_viewports[0].visible_rows[0].cells[0].row_ax_index,
            None
        );
        assert_eq!(
            omitted.table_viewports[0].visible_rows[0].cells[0].column_ax_index,
            None
        );
        assert!(omitted.validate().is_ok());

        let mut null_indices = serde_json::to_value(table_viewport_snapshot()).unwrap();
        null_indices["table_viewports"][0]["visible_rows"][0]["ax_index"] = serde_json::Value::Null;
        null_indices["table_viewports"][0]["visible_rows"][0]["cells"][0]["row_ax_index"] =
            serde_json::Value::Null;
        null_indices["table_viewports"][0]["visible_rows"][0]["cells"][0]["column_ax_index"] =
            serde_json::Value::Null;
        let explicit_null: StructureSnapshot = serde_json::from_value(null_indices).unwrap();
        assert_eq!(
            explicit_null.table_viewports[0].visible_rows[0].ax_index,
            None
        );
        assert_eq!(
            explicit_null.table_viewports[0].visible_rows[0].cells[0].row_ax_index,
            None
        );
        assert_eq!(
            explicit_null.table_viewports[0].visible_rows[0].cells[0].column_ax_index,
            None
        );
        assert!(explicit_null.validate().is_ok());

        let mut mismatched_row = table_viewport_snapshot();
        mismatched_row.table_viewports[0].visible_rows[0].ax_index = Some(42);
        mismatched_row.table_viewports[0].visible_rows[0].cells[0].row_ax_index = Some(511);
        assert!(mismatched_row.validate().is_err());

        let mut mismatched_column = table_viewport_snapshot();
        mismatched_column.table_viewports[0].columns[0].ax_index = Some(7);
        mismatched_column.table_viewports[0].visible_rows[0].cells[0].column_ax_index = Some(8);
        assert!(mismatched_column.validate().is_err());

        let mut out_of_range_column = table_viewport_snapshot();
        out_of_range_column.table_viewports[0].visible_rows[0].cells[0].column_ax_index = Some(64);
        assert!(out_of_range_column.validate().is_err());

        let mut out_of_range_row = serde_json::to_value(table_viewport_snapshot()).unwrap();
        out_of_range_row["table_viewports"][0]["visible_rows"][0]["ax_index"] =
            serde_json::json!(u64::from(u32::MAX) + 1);
        assert!(serde_json::from_value::<StructureSnapshot>(out_of_range_row).is_err());
    }

    #[test]
    fn observer_coverage_is_mandatory_bounded_and_matches_projected_objects() {
        let mut covered = snapshot();
        covered.objects.push(table_test_object(
            "root",
            None,
            "AXWindow",
            covered.windows[0].frame,
        ));
        covered.observer_coverage.covered_objects = 1;
        assert!(covered.validate().is_ok());

        let mut missing_coverage = serde_json::to_value(&covered).unwrap();
        missing_coverage
            .as_object_mut()
            .unwrap()
            .remove("observer_coverage");
        assert!(serde_json::from_value::<StructureSnapshot>(missing_coverage).is_err());

        let mut missing_mode = serde_json::to_value(&covered).unwrap();
        missing_mode["objects"][0]
            .as_object_mut()
            .unwrap()
            .remove("identity_continuity");
        assert!(serde_json::from_value::<StructureSnapshot>(missing_mode).is_err());

        let mut read_scoped = covered.clone();
        read_scoped.objects[0].identity_continuity = ObjectIdentityContinuity::ReadScoped;
        read_scoped.observer_coverage.state = SourceState::Degraded;
        read_scoped.observer_coverage.covered_objects = 0;
        read_scoped.observer_coverage.read_scoped_objects = 1;
        assert!(read_scoped.validate().is_ok());

        let mut mixed = covered.clone();
        let mut target =
            table_test_object("target", Some("root"), "AXButton", covered.windows[0].frame);
        target.identity_continuity = ObjectIdentityContinuity::ReadScoped;
        mixed.objects.push(target);
        mixed.observer_coverage.state = SourceState::Degraded;
        mixed.observer_coverage.covered_objects = 1;
        mixed.observer_coverage.read_scoped_objects = 1;
        assert!(
            mixed.validate().is_ok(),
            "partial reads count projected objects"
        );

        let mut mismatched = read_scoped.clone();
        mismatched.observer_coverage.read_scoped_objects = 0;
        assert!(mismatched.validate().is_err());
        mismatched = read_scoped.clone();
        mismatched.observer_coverage.state = SourceState::Available;
        assert!(mismatched.validate().is_err());
        mismatched = covered.clone();
        mismatched.observer_coverage.state = SourceState::Unavailable;
        assert!(mismatched.validate().is_err());
        mismatched = read_scoped.clone();
        mismatched.observer_coverage.state = SourceState::Unavailable;
        assert!(mismatched.validate().is_ok());
        mismatched = covered.clone();
        mismatched.observer_coverage.state = SourceState::Degraded;
        assert!(
            mismatched.validate().is_err(),
            "degraded needs an actual gap"
        );

        read_scoped.observer_coverage.failed_registrations = 1;
        read_scoped.observer_coverage.failures = vec![ObserverRegistrationFailure {
            instance_ref: ObjectRef::new("root").unwrap(),
            notification: ObserverNotification::UiElementDestroyed,
            error_code: -25204,
        }];
        assert!(read_scoped.validate().is_ok());
        read_scoped.observer_coverage.failed_registrations = 2;
        let duplicate_failure = read_scoped.observer_coverage.failures[0].clone();
        read_scoped
            .observer_coverage
            .failures
            .push(duplicate_failure);
        assert!(
            read_scoped.validate().is_err(),
            "duplicate reasons do not count twice"
        );
        read_scoped.observer_coverage.failed_registrations = 1;
        read_scoped.observer_coverage.failures.truncate(1);
        read_scoped.observer_coverage.failures[0].error_code = 0;
        assert!(
            read_scoped.validate().is_err(),
            "zero is not an AX failure code"
        );
    }

    #[test]
    fn optional_frame_ref_is_bounded_and_missing_values_do_not_imply_pairing() {
        let mut snapshot = snapshot();
        assert!(snapshot.frame_ref.is_none());
        assert!(snapshot.validate().is_ok());

        snapshot.frame_ref = Some("png-content-1".into());
        assert!(snapshot.validate().is_ok());

        let mut missing = serde_json::to_value(&snapshot).unwrap();
        missing.as_object_mut().unwrap().remove("frame_ref");
        let decoded: StructureSnapshot = serde_json::from_value(missing).unwrap();
        assert_eq!(decoded.frame_ref, None);

        for invalid in ["", " \t", "png\ncontent"] {
            let mut malformed = snapshot.clone();
            malformed.frame_ref = Some(invalid.into());
            assert!(malformed.validate().is_err(), "{invalid:?}");
        }
        let mut oversized = snapshot;
        oversized.frame_ref = Some("x".repeat(257));
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn capture_commands_are_typed_bounded_and_deadline_ordered() {
        let valid = SourceCommand::CaptureNow {
            request_id: RequestId::new("capture-request-1").unwrap(),
            watch_id: "watch-1".into(),
            session_epoch: 7,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 7,
            },
            after_ms: 100,
            deadline_ms: 500,
        };
        assert!(valid.validate().is_ok());
        let wire = serde_json::to_value(&valid).unwrap();
        assert_eq!(wire["command"], "capture_now");
        assert_eq!(wire["request_id"], "capture-request-1");

        let mut invalid = valid.clone();
        if let SourceCommand::CaptureNow { after_ms, .. } = &mut invalid {
            *after_ms = 500;
        }
        assert!(invalid.validate().is_err());
        let mut invalid = valid.clone();
        if let SourceCommand::CaptureNow { session_epoch, .. } = &mut invalid {
            *session_epoch = 0;
        }
        assert!(invalid.validate().is_err());
        let mut invalid = valid;
        if let SourceCommand::CaptureNow { watch_id, .. } = &mut invalid {
            *watch_id = "watch\n1".into();
        }
        assert!(invalid.validate().is_err());

        let cancel = SourceCommand::CancelCapture {
            request_id: RequestId::new("capture-request-1").unwrap(),
            watch_id: "watch-1".into(),
        };
        assert!(cancel.validate().is_ok());
        assert_eq!(
            serde_json::to_value(cancel).unwrap()["command"],
            "cancel_capture"
        );
    }

    #[test]
    fn table_viewport_continuity_is_local_and_cannot_upgrade_incomplete_structure() {
        use super::super::binding::{BindingProofScope, derive_binding_continuity};

        let mut snapshot = table_viewport_snapshot();
        let viewport = &mut snapshot.table_viewports[0];
        viewport.visible_rows_complete = true;
        let scope = BindingProofScope::TableViewport {
            table_instance_ref: viewport.table_instance_ref.clone(),
            scroll_container_instance_ref: viewport.scroll_container_instance_ref.clone(),
            viewport_frame: viewport.viewport_frame,
            visible_rows_complete: true,
            columns_complete: true,
        };
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "row")
            .unwrap();
        let proof = derive_binding_continuity(&snapshot, target, scope.clone()).unwrap();
        assert!(proof.validate().is_ok());
        assert!(!snapshot.complete);
        assert!(snapshot.table_viewports[0].visible_rows_complete);

        let mut stale_scope = scope;
        if let BindingProofScope::TableViewport { viewport_frame, .. } = &mut stale_scope {
            viewport_frame.x += 1.0;
        }
        assert!(derive_binding_continuity(&snapshot, target, stale_scope).is_err());
    }

    #[test]
    fn source_identity_boundary_requires_canonical_namespace_and_positive_generation() {
        let valid = SourceIdentityBoundary {
            namespace: "00112233-4455-6677-8899-aabbccddeeff".into(),
            generation: 1,
        };
        assert!(valid.validate().is_ok());
        let mut complete_synthetic = snapshot();
        assert!(complete_synthetic.identity_boundary.is_none());
        assert!(complete_synthetic.validate().is_ok());
        complete_synthetic.identity_boundary = Some(valid.clone());
        assert!(complete_synthetic.validate().is_ok());

        let mut invalid_namespaces = Vec::new();
        for namespace in [
            "00112233-4455-6677-8899-AABBCCDDEEFF",
            "00112233445566778899aabbccddeeff",
            "00112233-4455-6677-8899-aabbccddeezz",
        ] {
            let mut invalid = valid.clone();
            invalid.namespace = namespace.into();
            assert!(invalid.validate().is_err(), "{namespace}");
            invalid_namespaces.push(invalid);
        }
        let mut zero_generation = valid;
        zero_generation.generation = 0;
        assert!(zero_generation.validate().is_err());
        for invalid in invalid_namespaces.into_iter().chain([zero_generation]) {
            complete_synthetic.identity_boundary = Some(invalid);
            assert!(complete_synthetic.validate().is_err());
        }
    }

    #[test]
    fn scroll_range_evidence_requires_finite_ordered_values_and_scroll_area() {
        let axis = ScrollAxisState {
            value: 0.5,
            min: Some(0.0),
            max: Some(1.0),
        };
        assert!(axis.valid());
        assert!(
            ScrollAxisState {
                min: None,
                max: None,
                ..axis
            }
            .valid()
        );
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(!ScrollAxisState { value, ..axis }.valid());
        }
        assert!(
            !ScrollAxisState {
                min: Some(1.0),
                ..axis
            }
            .valid()
        );
        let mut view = snapshot();
        view.objects.push(UiObject {
            instance_ref: ObjectRef::new("area").unwrap(),
            parent: None,
            window_id: "w1".into(),
            role: "AXScrollArea".into(),
            subrole: None,
            title: Some("List".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: view.windows[0].frame,
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: Some(ScrollState {
                horizontal: None,
                vertical: Some(axis),
            }),
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        });
        view.observer_coverage.covered_objects = 1;
        assert!(view.validate().is_ok());
        view.objects[0].secure = true;
        assert!(view.validate().is_err());
        view.objects[0].secure = false;
        view.objects[0].role = "AXButton".into();
        assert!(view.validate().is_err());
        view.objects[0].role = "AXScrollArea".into();
        view.objects[0].scroll.as_mut().unwrap().vertical = None;
        assert!(view.validate().is_err());
    }

    #[test]
    fn snapshot_validation_checks_references_and_completeness() {
        assert!(snapshot().validate().is_ok());
        let mut bad = snapshot();
        bad.global_focus.window_id = ObservationFact::Known {
            value: "nope".into(),
        };
        assert!(bad.validate().is_err());
        bad = snapshot();
        bad.complete = false;
        assert!(
            bad.validate().is_err(),
            "an incomplete read must be explained"
        );
        bad.incomplete_reason = Some("ax timeout".into());
        assert!(bad.validate().is_ok());
    }

    fn native_target_context(
        snapshot: &StructureSnapshot,
        hidden: ObservationFact<bool>,
        minimized: ObservationFact<bool>,
    ) -> TargetContext {
        let mut source = SourceBackgroundWindowContext {
            window: SourceWindowContext {
                source_session_id: "source-1".into(),
                session_epoch: snapshot.session_epoch,
                process: KernelProcessIdentity {
                    pid: snapshot.app.pid,
                    start_seconds: 1,
                    start_micros: 0,
                },
                origin_window_id: "w1".into(),
                origin_window_frame: snapshot.windows[0].frame,
                verified_source_sequence: snapshot.source_sequence,
                verified_observed_ms: snapshot.observed_ms,
            },
            hidden,
            minimized,
            target_context_hash: String::new(),
        };
        source.target_context_hash = source.computed_target_context_hash(&snapshot.app).unwrap();
        TargetContext::NativeWindow {
            app: snapshot.app.clone(),
            source,
        }
    }

    #[test]
    fn target_context_does_not_infer_global_focus_and_keeps_visibility_uncertainty() {
        let mut view = snapshot();
        view.global_focus.app = ObservationFact::Known {
            value: AppIdentity {
                bundle_id: "com.example.other".into(),
                name: "Other App".into(),
                pid: 99,
            },
        };
        view.global_focus.window_id = ObservationFact::Unavailable;
        let target_context = native_target_context(
            &view,
            ObservationFact::Known { value: false },
            ObservationFact::Unavailable,
        );
        view.target_context = Some(target_context);

        assert!(view.validate().is_ok());
        assert_eq!(view.global_focus.known_window_id(), None);
        let Some(TargetContext::NativeWindow { source, .. }) = &view.target_context else {
            panic!("expected a native target context");
        };
        assert!(!source.is_visible_standard_window());
    }

    #[test]
    fn native_target_context_requires_current_kernel_window_and_hash_identity() {
        let mut view = snapshot();
        let target_context = native_target_context(
            &view,
            ObservationFact::Known { value: false },
            ObservationFact::Known { value: false },
        );
        view.target_context = Some(target_context);
        assert!(view.validate().is_ok());

        let mut stale = view.clone();
        if let Some(TargetContext::NativeWindow { source, .. }) = &mut stale.target_context {
            source.window.verified_source_sequence += 1;
        }
        assert!(stale.validate().is_err());

        let mut wrong_process = view.clone();
        if let Some(TargetContext::NativeWindow { source, .. }) = &mut wrong_process.target_context
        {
            source.window.process.start_seconds += 1;
        }
        assert!(wrong_process.validate().is_err());

        let mut wrong_hash = view.clone();
        if let Some(TargetContext::NativeWindow { source, .. }) = &mut wrong_hash.target_context {
            source.target_context_hash.push('x');
        }
        assert!(wrong_hash.validate().is_err());

        let mut wrong_frame = view;
        wrong_frame.windows[0].frame.width += 1.0;
        assert!(wrong_frame.validate().is_err());
    }
}
