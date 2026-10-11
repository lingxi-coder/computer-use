//! Binding a semantic target to one live instance (design 6.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::FailureCode;
use super::ids::{AttemptId, BindingRef, ObjectRef, RequestId, SemanticTargetRef};
use super::row::{RowSelectionObservation, RowSelector};
use super::source::{
    AxActionName, AxEnabledObservation, CompositionState, FocusContext, FocusRelation, LayerKind,
    MenuAnchorContinuity, MenuContext, MenuOpenerAnchor, ObjectIdentityContinuity, Rect,
    ScrollState, SourceBackgroundWindowContext, SourceWindowContext, StructureSnapshot,
    TableViewport, TableViewportCell, TableViewportColumn, TableViewportRow, UiObject, UiValue,
    WindowInfo, validate_menu_id,
};
use super::task::RowTableScope;

pub const MAX_DEVICE_CONTEXT_ANCESTRY: usize = 32;
pub const MAX_DEVICE_CONTEXT_SIBLINGS: usize = 256;
pub const MAX_DEVICE_CONTEXT_WITNESS_BYTES: usize = 4096;
pub const MAX_DEVICE_CONTEXT_AX_ACTIONS: usize = 64;

fn canonical_background_ax_action_names(names: &[String]) -> Result<Vec<String>, String> {
    if names.is_empty()
        || names.len() > MAX_DEVICE_CONTEXT_AX_ACTIONS
        || names
            .iter()
            .any(|name| name.is_empty() || name.len() > 128 || name.chars().any(char::is_control))
    {
        return Err("background AX target action names are empty or unbounded".into());
    }
    let mut canonical = names.to_vec();
    canonical.sort();
    if canonical.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("background AX target action names contain duplicates".into());
    }
    if !canonical.iter().any(|name| name == "AXPress") {
        return Err("background AX target does not declare the actual AXPress action".into());
    }
    Ok(canonical)
}

fn valid_target_context_hash(hash: &str) -> bool {
    hash.strip_prefix("tc1:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The proof boundary for binding continuity. A table viewport is a local
/// projection and never changes the completeness of its containing snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingProofScope {
    WholeStructure,
    /// A frozen semantic row-table scope used only to bind the actual active
    /// normal window root for a WindowActivation transition.
    WindowRoot {
        table_scope: RowTableScope,
    },
    /// A separately authorized, one-AXPress background action in a fixed native
    /// window target context. It is not foreground or ordinary input authority.
    BackgroundAx {
        target_context_hash: String,
    },
    TableViewport {
        table_instance_ref: ObjectRef,
        scroll_container_instance_ref: ObjectRef,
        viewport_frame: Rect,
        visible_rows_complete: bool,
        columns_complete: bool,
    },
}

impl BindingProofScope {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::WholeStructure => Ok(()),
            Self::WindowRoot { table_scope } => table_scope.validate(),
            Self::BackgroundAx {
                target_context_hash,
            } if valid_target_context_hash(target_context_hash) => Ok(()),
            Self::BackgroundAx { .. } => {
                Err("background AX scope requires a tc1 target-context hash".into())
            }
            Self::TableViewport {
                table_instance_ref,
                scroll_container_instance_ref,
                viewport_frame,
                visible_rows_complete,
                columns_complete,
            } if table_instance_ref != scroll_container_instance_ref
                && viewport_frame.valid_bounded()
                && *visible_rows_complete
                && *columns_complete =>
            {
                Ok(())
            }
            Self::TableViewport { .. } => Err(
                "table viewport scope requires distinct actual table/scroll refs, bounded geometry, and complete local rows and columns".into(),
            ),
        }
    }
}

/// A value-only description for the Device to compare with its own live AX
/// read. It contains no Source registry references or native entity identifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceContextWitness {
    WholeStructureV1 {
        app: Box<DeviceContextApp>,
        #[schemars(length(max = 256))]
        active_layer: Option<String>,
        window: Box<DeviceContextWindow>,
        target: Box<DeviceContextTarget>,
        /// Actual target-to-window path, ordered from root to target parent.
        #[schemars(length(max = 32))]
        ancestry: Vec<DeviceContextObject>,
        /// Parent of the complete direct-child list, indexed in `ancestry`.
        #[schemars(range(max = 31))]
        sibling_parent_ancestry_index: Option<u8>,
        /// Complete actual direct-child order, including the target.
        #[schemars(length(min = 1, max = 256))]
        siblings: Vec<DeviceContextObject>,
        #[schemars(range(max = 255))]
        target_sibling_index: u16,
        #[schemars(range(max = 31))]
        nearest_labelled_group_ancestry_index: Option<u8>,
        #[schemars(length(max = 32))]
        scroll_ancestors: Vec<DeviceContextScrollAncestor>,
        table_column: Option<Box<DeviceContextTableColumnRelation>>,
        menu: Option<Box<DeviceContextMenuRelation>>,
        focus: Option<Box<DeviceContextFocusRelation>>,
    },
    /// A local, complete table viewport proof for exactly one AXSelected=true
    /// input. It never promotes the containing Source snapshot to complete.
    TableViewportV1 {
        app: Box<DeviceContextApp>,
        window: Box<DeviceContextWindow>,
        table: DeviceContextObject,
        target: Box<DeviceContextTarget>,
        #[schemars(length(min = 1, max = 8))]
        scroll_path: Vec<DeviceContextObject>,
        viewport_frame: Rect,
        #[schemars(length(min = 1, max = 64))]
        ordered_columns: Vec<DeviceContextTableColumn>,
        #[schemars(length(min = 1, max = 32))]
        selector_columns: Vec<DeviceContextRowSelectorColumn>,
        #[schemars(length(min = 1, max = 256))]
        visible_rows: Vec<DeviceContextVisibleRow>,
        #[schemars(range(max = 4_294_967_295u32))]
        target_row_ax_index: u32,
        table_ax_enabled: AxEnabledObservation,
        target_ax_enabled: AxEnabledObservation,
        target_selected: bool,
        target_ax_selected_settable: bool,
        hit_point: DeviceContextPoint,
    },
    /// A same-read, value-only normal-window root proof for one activation.
    /// It carries no Source object reference and proves no sibling closure,
    /// focused control, or general input authority.
    WindowRootV1 {
        app: Box<DeviceContextApp>,
        window: Box<DeviceContextWindow>,
        root: DeviceContextObject,
        source: Box<SourceWindowContext>,
    },
    /// A value-only background AX witness. Frames identify actual AX elements;
    /// this variant carries no global focus, hit point, or OS input authority.
    BackgroundAxV1 {
        app: Box<DeviceContextApp>,
        window: Box<DeviceContextWindow>,
        root: DeviceContextObject,
        target: Box<DeviceContextTarget>,
        target_enabled: bool,
        #[schemars(length(max = 64))]
        target_action_names: Vec<String>,
        #[schemars(length(min = 1, max = 32))]
        ancestry: Vec<DeviceContextObject>,
        #[schemars(range(max = 31))]
        sibling_parent_ancestry_index: u8,
        #[schemars(length(min = 1, max = 256))]
        siblings: Vec<DeviceContextObject>,
        #[schemars(range(max = 255))]
        target_sibling_index: u16,
        #[schemars(range(max = 31))]
        nearest_labelled_group_ancestry_index: Option<u8>,
        source: Box<SourceBackgroundWindowContext>,
        #[schemars(length(min = 68, max = 68))]
        target_context_hash: String,
    },
    /// A typed proof exists, but this local source scope has no Native input
    /// comparator. It must never be promoted to a whole-structure witness.
    Unsupported {
        reason: DeviceContextUnsupportedReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceContextUnsupportedReason {
    TableViewport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextApp {
    #[schemars(length(min = 1, max = 256))]
    pub bundle_id: String,
    #[schemars(range(min = 1))]
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextWindow {
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    #[schemars(length(max = 512))]
    pub title: Option<String>,
    pub frame: Rect,
    pub kind: LayerKind,
    pub modal: bool,
    #[schemars(length(min = 1, max = 256))]
    pub owner_bundle_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextObject {
    #[schemars(length(min = 1, max = 64))]
    pub role: String,
    #[schemars(length(max = 128))]
    pub subrole: Option<String>,
    #[schemars(length(max = 512))]
    pub label: String,
    pub frame: Rect,
}

/// The semantic subset of `InstanceFingerprint` that has no Source reference
/// fields and can therefore be compared directly by the Device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextFingerprint {
    #[schemars(length(min = 1, max = 256))]
    pub app: String,
    #[schemars(range(min = 1))]
    pub app_pid: u32,
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    #[schemars(length(max = 32))]
    pub ancestor_roles: Vec<String>,
    #[schemars(length(max = 512))]
    pub group: Option<String>,
    #[schemars(length(min = 1, max = 64))]
    pub role: String,
    #[schemars(length(max = 128))]
    pub subrole: Option<String>,
    #[schemars(length(max = 512))]
    pub label: String,
    #[schemars(range(max = 255))]
    pub sibling_index: u32,
    pub frame: [i64; 4],
}

impl DeviceContextFingerprint {
    fn from_instance(fingerprint: &InstanceFingerprint) -> Self {
        Self {
            app: fingerprint.app.clone(),
            app_pid: fingerprint.app_pid,
            window_id: fingerprint.window_id.clone(),
            ancestor_roles: fingerprint.ancestor_roles.clone(),
            group: fingerprint.group.clone(),
            role: fingerprint.role.clone(),
            subrole: fingerprint.subrole.clone(),
            label: fingerprint.label.clone(),
            sibling_index: fingerprint.sibling_index,
            frame: fingerprint.frame,
        }
    }

    fn matches_instance(&self, fingerprint: &InstanceFingerprint) -> bool {
        self == &Self::from_instance(fingerprint)
    }

    fn validate(&self) -> Result<(), String> {
        let bounded = |text: &str, limit: usize| {
            !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
        };
        if !bounded(&self.app, 256)
            || self.app_pid == 0
            || !bounded(&self.window_id, 256)
            || self.ancestor_roles.len() > MAX_DEVICE_CONTEXT_ANCESTRY
            || self.ancestor_roles.iter().any(|role| !bounded(role, 64))
            || !bounded(&self.role, 64)
            || self
                .subrole
                .as_ref()
                .is_some_and(|role| !bounded(role, 128))
            || self.group.as_ref().is_some_and(|group| {
                group.len() > 512
                    || group.chars().any(char::is_control)
                    || *group != normalize_label(group)
            })
            || self.label.len() > 512
            || self.label.chars().any(char::is_control)
            || self.label != normalize_label(&self.label)
            || self.sibling_index >= MAX_DEVICE_CONTEXT_SIBLINGS as u32
            || self
                .frame
                .iter()
                .any(|value| !(-100_000..=100_000).contains(value))
            || self.frame[2] <= 0
            || self.frame[3] <= 0
        {
            return Err("device context semantic fingerprint is invalid or unbounded".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextTarget {
    pub object: DeviceContextObject,
    #[schemars(length(max = 512))]
    pub group: Option<String>,
    pub scroll: Option<ScrollState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextScrollAncestor {
    #[schemars(range(max = 31))]
    pub ancestry_index: u8,
    pub state: Option<ScrollState>,
}

/// The exact Source table and ordered AX column indices associated with a
/// sortable header target. The Device compares this value-only relation with
/// its own current AX read; it is not a Source-to-native object mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextTableColumnRelation {
    #[schemars(range(max = 31))]
    pub table_ancestry_index: u8,
    #[schemars(length(min = 1, max = 64))]
    pub ordered_ax_indices: Vec<u8>,
    #[schemars(range(max = 63))]
    pub target_ax_index: u8,
}

/// A proof-bound candidate point in global display coordinates. This is an
/// Environment proposal constrained to the exact AXRow geometry; Source does
/// not claim that the point itself was hit-tested.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextPoint {
    pub x: f64,
    pub y: f64,
}

/// One actual AXColumn, in Source AXIndex order, with its exact public header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextTableColumn {
    #[schemars(range(max = 63))]
    pub ax_index: u8,
    #[schemars(length(min = 1, max = 256))]
    pub header: String,
    pub frame: Rect,
}

/// Public selector condition for one actual AX column. The expected value is
/// needed by the Device to independently confirm which visible row is unique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextRowSelectorColumn {
    #[schemars(range(max = 63))]
    pub ax_index: u8,
    #[schemars(length(min = 1, max = 256))]
    pub header: String,
    pub expected_value: UiValue,
}

/// Actual public values of each selector column for one currently visible AXRow.
/// `selector_values` is aligned to the witness's `selector_columns` vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextVisibleRow {
    #[schemars(range(max = 4_294_967_295u32))]
    pub ax_index: u32,
    pub frame: Rect,
    #[schemars(length(min = 1, max = 32))]
    pub selector_values: Vec<UiValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceContextMenuRelation {
    OpenerAnchor {
        origin_window_id: String,
        origin_window_frame: Rect,
        opener_fingerprint: Box<DeviceContextFingerprint>,
        continuity: MenuAnchorContinuity,
    },
    AnchoredMenu {
        origin_window_id: String,
        origin_window_frame: Rect,
        opener_fingerprint: Box<DeviceContextFingerprint>,
        continuity: MenuAnchorContinuity,
        menu_fingerprint: Box<DeviceContextFingerprint>,
        menu_frame: Rect,
        drawing_window: DeviceContextDrawingWindow,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextDrawingWindow {
    pub window_id: u32,
    pub owner_pid: u32,
    pub layer: i32,
    pub frame: Rect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceContextFocusRelation {
    pub origin_window_id: String,
    pub origin_window_frame: Rect,
    pub target_fingerprint: DeviceContextFingerprint,
    pub target_frame: Rect,
    pub relation: FocusRelation,
    pub composition: DeviceContextComposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceContextComposition {
    Unknown,
    NotApplicable,
    Marked,
    Unmarked,
}

impl DeviceContextWitness {
    pub fn supports_native_input(&self) -> bool {
        matches!(self, Self::WholeStructureV1 { .. })
    }

    pub fn supports_window_activation(&self) -> bool {
        match self {
            Self::WholeStructureV1 {
                window,
                target,
                ancestry,
                target_sibling_index,
                menu,
                focus,
                ..
            } => {
                self.validate().is_ok()
                    && window.kind == LayerKind::Window
                    && !window.modal
                    && target.object.role == "AXWindow"
                    && target.object.subrole.as_deref() == Some("AXStandardWindow")
                    && target.object.frame == window.frame
                    && target.group.is_none()
                    && target.scroll.is_none()
                    && ancestry.is_empty()
                    && *target_sibling_index == 0
                    && menu.is_none()
                    && focus.is_none()
            }
            Self::WindowRootV1 { .. } => self.validate().is_ok(),
            _ => false,
        }
    }

    pub fn supports_row_selection(&self) -> bool {
        matches!(self, Self::TableViewportV1 { .. }) && self.validate().is_ok()
    }

    pub fn supports_background_ax_press(&self) -> bool {
        let Self::BackgroundAxV1 {
            target_enabled,
            target_action_names,
            target,
            ancestry,
            source,
            ..
        } = self
        else {
            return false;
        };
        self.validate().is_ok()
            && source.is_visible_standard_window()
            && *target_enabled
            && target_action_names.iter().any(|action| action == "AXPress")
            && target.scroll.is_none()
            && target.object.role != "AXWindow"
            && !ancestry.iter().any(|node| node.role == "AXScrollArea")
    }

    pub fn supports_execution_domain(&self, domain: super::domain::ExecutionDomain) -> bool {
        match domain {
            super::domain::ExecutionDomain::ForegroundNative => {
                matches!(
                    self,
                    Self::WholeStructureV1 { .. }
                        | Self::WindowRootV1 { .. }
                        | Self::TableViewportV1 { .. }
                ) && self.validate().is_ok()
            }
            super::domain::ExecutionDomain::BackgroundAx => self.supports_background_ax_press(),
            super::domain::ExecutionDomain::HeadlessBrowser
            | super::domain::ExecutionDomain::AppIntegration => false,
            super::domain::ExecutionDomain::Simulated => {
                matches!(
                    self,
                    Self::WholeStructureV1 { .. }
                        | Self::WindowRootV1 { .. }
                        | Self::TableViewportV1 { .. }
                ) && self.validate().is_ok()
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if matches!(self, Self::TableViewportV1 { .. }) {
            validate_table_viewport_witness(self)?;
            let encoded = serde_json::to_vec(self)
                .map_err(|_| "Device context witness could not be canonicalized")?;
            return if encoded.len() <= MAX_DEVICE_CONTEXT_WITNESS_BYTES {
                Ok(())
            } else {
                Err("Device context witness exceeds the 4096-byte bound".into())
            };
        }
        if matches!(self, Self::WindowRootV1 { .. }) {
            validate_window_root_witness(self)?;
            let encoded = serde_json::to_vec(self)
                .map_err(|_| "Device context witness could not be canonicalized")?;
            return if encoded.len() <= MAX_DEVICE_CONTEXT_WITNESS_BYTES {
                Ok(())
            } else {
                Err("Device context witness exceeds the 4096-byte bound".into())
            };
        }
        if matches!(self, Self::BackgroundAxV1 { .. }) {
            validate_background_ax_witness(self)?;
            let encoded = serde_json::to_vec(self)
                .map_err(|_| "background AX Device witness could not be canonicalized")?;
            return if encoded.len() <= MAX_DEVICE_CONTEXT_WITNESS_BYTES {
                Ok(())
            } else {
                Err("background AX Device witness exceeds the 4096-byte bound".into())
            };
        }
        let Self::WholeStructureV1 {
            app,
            active_layer,
            window,
            target,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            scroll_ancestors,
            table_column,
            menu,
            focus,
        } = self
        else {
            let encoded = serde_json::to_vec(self)
                .map_err(|_| "Device context witness could not be canonicalized")?;
            return if encoded.len() <= MAX_DEVICE_CONTEXT_WITNESS_BYTES {
                Ok(())
            } else {
                Err("Device context witness exceeds the 4096-byte bound".into())
            };
        };
        let bounded = |text: &str, max: usize| {
            !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
        };
        if !bounded(&app.bundle_id, 256) || app.pid == 0 {
            return Err("device context requires a bounded application identity".into());
        }
        if active_layer
            .as_ref()
            .is_some_and(|layer| !bounded(layer, 256))
            || !bounded(&window.window_id, 256)
            || !bounded(&window.owner_bundle_id, 256)
            || window.owner_bundle_id != app.bundle_id
            || window.title.as_ref().is_some_and(|title| {
                title.len() > 512
                    || title.chars().any(char::is_control)
                    || *title != normalize_label(title)
            })
            || !window.frame.valid_bounded()
        {
            return Err("device context window facts are invalid or unbounded".into());
        }
        if ancestry.len() > MAX_DEVICE_CONTEXT_ANCESTRY
            || siblings.is_empty()
            || siblings.len() > MAX_DEVICE_CONTEXT_SIBLINGS
            || *target_sibling_index as usize >= siblings.len()
            || siblings.get(*target_sibling_index as usize) != Some(&target.object)
        {
            return Err("device context ancestry or complete sibling order is invalid".into());
        }
        for node in ancestry
            .iter()
            .chain(siblings.iter())
            .chain([&target.object])
        {
            validate_device_context_object(node)?;
        }
        if ancestry.is_empty() {
            if sibling_parent_ancestry_index.is_some() {
                return Err("root target cannot name a sibling parent in its ancestry".into());
            }
        } else if sibling_parent_ancestry_index.map(usize::from) != Some(ancestry.len() - 1) {
            return Err("sibling parent must be the target's actual final ancestry node".into());
        }
        if let Some(index) = nearest_labelled_group_ancestry_index {
            let group = ancestry
                .get(*index as usize)
                .ok_or("nearest labelled group index is outside target ancestry")?;
            if !is_group_role(&group.role) || group.label.is_empty() {
                return Err(
                    "nearest labelled group must identify a labelled actual ancestor".into(),
                );
            }
            if target
                .group
                .as_ref()
                .is_some_and(|label| label != &group.label)
            {
                return Err(
                    "target group label differs from its actual nearest labelled group".into(),
                );
            }
        } else if target.group.as_ref().is_some_and(|label| !label.is_empty()) {
            return Err("target group label has no actual labelled group ancestor".into());
        }
        let expected_group_index = ancestry
            .iter()
            .enumerate()
            .rev()
            .find(|(_, ancestor)| is_group_role(&ancestor.role) && !ancestor.label.is_empty())
            .map(|(index, _)| index);
        if expected_group_index != nearest_labelled_group_ancestry_index.map(usize::from) {
            return Err(
                "nearest labelled group index does not identify the closest actual group".into(),
            );
        }
        if target.group.as_ref().is_some_and(|label| {
            label.len() > 512
                || label.chars().any(char::is_control)
                || *label != normalize_label(label)
        }) || target.scroll.as_ref().is_some_and(|scroll| !scroll.valid())
        {
            return Err("device context target group or scroll facts are invalid".into());
        }
        let expected_scroll_indexes: Vec<_> = ancestry
            .iter()
            .enumerate()
            .filter_map(|(index, ancestor)| (ancestor.role == "AXScrollArea").then_some(index))
            .collect();
        let mut scroll_indexes = Vec::new();
        for scroll in scroll_ancestors {
            let index = scroll.ancestry_index as usize;
            if index >= ancestry.len()
                || ancestry[index].role != "AXScrollArea"
                || scroll_indexes.contains(&index)
                || scroll.state.as_ref().is_some_and(|state| !state.valid())
            {
                return Err(
                    "device context scroll ancestor is not an actual unique scroll ancestor".into(),
                );
            }
            scroll_indexes.push(index);
        }
        if scroll_indexes != expected_scroll_indexes {
            return Err("device context must include every actual scroll ancestor in order".into());
        }
        validate_device_context_table_column(table_column.as_deref(), target, ancestry)?;
        if table_column.is_some() && (menu.is_some() || focus.is_some()) {
            return Err(
                "sortable table header witness cannot include menu or focus relations".into(),
            );
        }
        let root = ancestry.first().unwrap_or(&target.object);
        if root.frame != window.frame
            || !matches!(
                (root.role.as_str(), window.kind, window.modal),
                ("AXWindow", LayerKind::Window, false)
                    | ("AXSheet", LayerKind::Sheet, true)
                    | ("AXMenu", LayerKind::Menu, _)
            )
            || root.role == "AXWindow" && root.subrole.as_deref() != Some("AXStandardWindow")
        {
            return Err("device context root does not match its actual window/layer facts".into());
        }
        if let Some(menu) = menu {
            validate_device_context_menu(menu, app)?;
        }
        if let Some(focus) = focus {
            focus.target_fingerprint.validate()?;
            if focus.origin_window_id != focus.target_fingerprint.window_id
                || focus.target_fingerprint.app != app.bundle_id
                || focus.target_fingerprint.app_pid != app.pid
                || !focus.origin_window_frame.valid_bounded()
                || !focus.target_frame.valid_bounded()
                || !focus.origin_window_frame.contains_rect(&focus.target_frame)
                || focus.target_fingerprint.frame != round_frame(focus.target_frame)
            {
                return Err(
                    "device context focus relation is inconsistent with its actual target".into(),
                );
            }
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|_| "Device context witness could not be canonicalized")?;
        if encoded.len() > MAX_DEVICE_CONTEXT_WITNESS_BYTES {
            return Err("Device context witness exceeds the 4096-byte bound".into());
        }
        Ok(())
    }

    fn validate_binding(
        &self,
        scope: &BindingProofScope,
        fingerprint: &InstanceFingerprint,
        display_frame: Rect,
        menu_scope: Option<&MenuBindingScope>,
        focus_scope: Option<&FocusedControlScope>,
    ) -> Result<(), String> {
        self.validate()?;
        match (scope, self) {
            (
                BindingProofScope::WholeStructure,
                Self::WholeStructureV1 {
                    app,
                    window,
                    target,
                    ancestry,
                    target_sibling_index,
                    menu,
                    focus,
                    ..
                },
            ) => {
                if app.bundle_id != fingerprint.app
                    || app.pid != fingerprint.app_pid
                    || window.window_id != fingerprint.window_id
                    || target.object.role != fingerprint.role
                    || target.object.subrole != fingerprint.subrole
                    || target.object.label != fingerprint.label
                    || target.group != fingerprint.group
                    || target.object.frame != display_frame
                    || round_frame(target.object.frame) != fingerprint.frame
                    || ancestry
                        .iter()
                        .map(|node| node.role.clone())
                        .collect::<Vec<_>>()
                        != fingerprint.ancestor_roles
                    || *target_sibling_index != fingerprint.sibling_index as u16
                {
                    return Err("device context target differs from its binding fingerprint".into());
                }
                if focus.as_ref().is_some_and(|witness| {
                    !witness.target_fingerprint.matches_instance(fingerprint)
                        || witness.target_frame != display_frame
                }) {
                    return Err(
                        "device context focus relation differs from its target binding".into(),
                    );
                }
                if menu_scope.is_none()
                    && menu.as_ref().is_some_and(|witness| match witness.as_ref() {
                        DeviceContextMenuRelation::OpenerAnchor {
                            opener_fingerprint, ..
                        } => !opener_fingerprint.matches_instance(fingerprint),
                        DeviceContextMenuRelation::AnchoredMenu {
                            opener_fingerprint,
                            menu_fingerprint,
                            ..
                        } => match fingerprint.role.as_str() {
                            "AXMenuButton" | "AXPopUpButton" => {
                                !opener_fingerprint.matches_instance(fingerprint)
                            }
                            "AXMenu" => !menu_fingerprint.matches_instance(fingerprint),
                            _ => false,
                        },
                    })
                {
                    return Err("unscoped menu witness does not describe its actual target".into());
                }
                match menu_scope {
                    Some(MenuBindingScope::Opener { anchor, .. }) => {
                        let matches = menu.as_ref().is_some_and(|witness| match witness.as_ref() {
                            DeviceContextMenuRelation::OpenerAnchor {
                                origin_window_id,
                                origin_window_frame,
                                opener_fingerprint,
                                continuity,
                            }
                            | DeviceContextMenuRelation::AnchoredMenu {
                                origin_window_id,
                                origin_window_frame,
                                opener_fingerprint,
                                continuity,
                                ..
                            } => {
                                origin_window_id == &anchor.origin_window_id
                                    && *origin_window_frame == anchor.origin_window_frame
                                    && *continuity == anchor.continuity
                                    && opener_fingerprint
                                        .matches_instance(&anchor.opener_fingerprint)
                            }
                        });
                        if !matches {
                            return Err(
                                "menu opener scope requires its actual value-only anchor witness"
                                    .into(),
                            );
                        }
                    }
                    Some(MenuBindingScope::Item { context, .. }) => {
                        let matches = menu.as_ref().is_some_and(|witness| match witness.as_ref() {
                            DeviceContextMenuRelation::AnchoredMenu {
                                origin_window_id,
                                origin_window_frame,
                                opener_fingerprint,
                                continuity,
                                menu_fingerprint,
                                menu_frame,
                                drawing_window,
                                ..
                            } => {
                                origin_window_id == &context.opener.origin_window_id
                                    && *origin_window_frame == context.opener.origin_window_frame
                                    && *continuity == context.opener.continuity
                                    && opener_fingerprint
                                        .matches_instance(&context.opener.opener_fingerprint)
                                    && menu_fingerprint.matches_instance(&context.menu_fingerprint)
                                    && *menu_frame == context.menu_frame
                                    && drawing_window.window_id == context.drawing_window.window_id
                                    && drawing_window.owner_pid == context.drawing_window.owner_pid
                                    && drawing_window.layer == context.drawing_window.layer
                                    && drawing_window.frame == context.drawing_window.frame
                            }
                            DeviceContextMenuRelation::OpenerAnchor { .. } => false,
                        });
                        if !matches {
                            return Err(
                                "menu item scope requires its actual anchored-menu witness".into(),
                            );
                        }
                    }
                    None => {}
                }
                if let Some(focus_scope) = focus_scope {
                    let context = &focus_scope.context;
                    let matches = focus.as_ref().is_some_and(|witness| {
                        witness.origin_window_id == context.origin_window_id
                            && witness.origin_window_frame == context.origin_window_frame
                            && witness
                                .target_fingerprint
                                .matches_instance(&context.target_fingerprint)
                            && witness.target_frame == context.target_frame
                            && witness.relation == context.relation
                            && witness.target_fingerprint.matches_instance(fingerprint)
                            && witness.target_frame == display_frame
                    });
                    if !matches {
                        return Err(
                            "focused control scope requires its actual focused-target witness"
                                .into(),
                        );
                    }
                }
                Ok(())
            }
            (
                BindingProofScope::TableViewport { .. },
                Self::Unsupported {
                    reason: DeviceContextUnsupportedReason::TableViewport,
                },
            ) => Ok(()),
            (
                BindingProofScope::TableViewport {
                    table_instance_ref,
                    scroll_container_instance_ref,
                    viewport_frame,
                    visible_rows_complete,
                    columns_complete,
                },
                Self::TableViewportV1 {
                    app,
                    window,
                    table,
                    target,
                    scroll_path,
                    viewport_frame: witness_viewport,
                    visible_rows,
                    target_row_ax_index,
                    table_ax_enabled,
                    target_ax_enabled,
                    target_selected,
                    target_ax_selected_settable,
                    hit_point,
                    ..
                },
            ) => {
                let target_row = visible_rows
                    .iter()
                    .find(|row| row.ax_index == *target_row_ax_index)
                    .ok_or("row witness target AXIndex is absent from visible rows")?;
                let point = *hit_point;
                let actual_scroll_roles: Vec<_> = fingerprint
                    .ancestor_roles
                    .iter()
                    .filter(|role| role.as_str() == "AXScrollArea")
                    .map(String::as_str)
                    .collect();
                let witnessed_scroll_roles: Vec<_> = scroll_path
                    .iter()
                    .map(|scroll| scroll.role.as_str())
                    .collect();
                if menu_scope.is_some()
                    || focus_scope.is_some()
                    || !*visible_rows_complete
                    || !*columns_complete
                    || table_instance_ref == scroll_container_instance_ref
                    || witness_viewport != viewport_frame
                    || app.bundle_id != fingerprint.app
                    || app.pid != fingerprint.app_pid
                    || window.window_id != fingerprint.window_id
                    || window.owner_bundle_id != fingerprint.app
                    || table.role != "AXTable"
                    || target.object.role != fingerprint.role
                    || target.object.subrole != fingerprint.subrole
                    || target.object.label != fingerprint.label
                    || target.group != fingerprint.group
                    || fingerprint.role != "AXRow"
                    || fingerprint.ancestor_roles.last().map(String::as_str) != Some("AXTable")
                    || !matches!(
                        (
                            window.kind,
                            window.modal,
                            fingerprint.ancestor_roles.first().map(String::as_str)
                        ),
                        (LayerKind::Window, false, Some("AXWindow"))
                            | (LayerKind::Sheet, true, Some("AXSheet"))
                    )
                    || fingerprint
                        .ancestor_roles
                        .iter()
                        .filter(|role| role.as_str() == "AXTable")
                        .count()
                        != 1
                    || actual_scroll_roles != witnessed_scroll_roles
                    || fingerprint.frame != round_frame(target_row.frame)
                    || target.object.frame != target_row.frame
                    || target.scroll.is_some()
                    || display_frame != target_row.frame
                    || !table_ax_enabled.permits_row_selection()
                    || !target_ax_enabled.permits_row_selection()
                    || *target_selected
                    || !*target_ax_selected_settable
                    || !viewport_frame.contains(point.x, point.y)
                    || !target_row.frame.contains(point.x, point.y)
                    || scroll_path.is_empty()
                {
                    return Err("table row witness differs from its table binding scope or target fingerprint".into());
                }
                Ok(())
            }
            (
                BindingProofScope::WindowRoot { .. },
                Self::WindowRootV1 {
                    app,
                    window,
                    root,
                    source,
                },
            ) => {
                if menu_scope.is_some()
                    || focus_scope.is_some()
                    || app.bundle_id != fingerprint.app
                    || app.pid != fingerprint.app_pid
                    || window.window_id != fingerprint.window_id
                    || window.owner_bundle_id != fingerprint.app
                    || window.kind != LayerKind::Window
                    || window.modal
                    || root.role != fingerprint.role
                    || root.subrole != fingerprint.subrole
                    || root.label != fingerprint.label
                    || root.frame != display_frame
                    || round_frame(root.frame) != fingerprint.frame
                    || !fingerprint.ancestor_roles.is_empty()
                    || fingerprint.group.is_some()
                    || fingerprint.scroll_container.is_some()
                    || fingerprint.sibling_index != 0
                    || root.role != "AXWindow"
                    || root.subrole.as_deref() != Some("AXStandardWindow")
                    || window.title.as_deref().unwrap_or_default() != root.label
                    || source.process.pid != app.pid
                    || source.origin_window_id != window.window_id
                    || source.origin_window_frame != window.frame
                    || source.verified_source_sequence == 0
                {
                    return Err(
                        "window-root witness differs from its exact root, scope or source context"
                            .into(),
                    );
                }
                Ok(())
            }
            (
                BindingProofScope::BackgroundAx {
                    target_context_hash: scope_hash,
                },
                Self::BackgroundAxV1 {
                    app,
                    window,
                    root,
                    target,
                    ancestry,
                    target_sibling_index,
                    source,
                    target_context_hash,
                    ..
                },
            ) => {
                if menu_scope.is_some()
                    || focus_scope.is_some()
                    || scope_hash != target_context_hash
                    || scope_hash != &source.target_context_hash
                    || app.bundle_id != fingerprint.app
                    || app.pid != fingerprint.app_pid
                    || window.window_id != fingerprint.window_id
                    || window.owner_bundle_id != fingerprint.app
                    || window.kind != LayerKind::Window
                    || window.modal
                    || root.role != "AXWindow"
                    || root.subrole.as_deref() != Some("AXStandardWindow")
                    || root.frame != window.frame
                    || target.object.role != fingerprint.role
                    || target.object.subrole != fingerprint.subrole
                    || target.object.label != fingerprint.label
                    || target.group != fingerprint.group
                    || target.scroll.is_some()
                    || target.object.frame != display_frame
                    || round_frame(target.object.frame) != fingerprint.frame
                    || ancestry
                        .iter()
                        .map(|node| node.role.clone())
                        .collect::<Vec<_>>()
                        != fingerprint.ancestor_roles
                    || fingerprint.sibling_index != u32::from(*target_sibling_index)
                    || source.window.process.pid != app.pid
                    || source.window.origin_window_id != window.window_id
                    || source.window.origin_window_frame != window.frame
                    || source.window.verified_source_sequence == 0
                {
                    return Err(
                        "background AX scope differs from its exact Source target context".into(),
                    );
                }
                Ok(())
            }
            // The Source closure can be globally complete while an actual
            // local table viewport still has no Native comparator.
            (BindingProofScope::WholeStructure, Self::Unsupported { .. }) => Ok(()),
            (BindingProofScope::TableViewport { .. }, Self::WholeStructureV1 { .. }) => Err(
                "a table viewport cannot be promoted to a whole-structure Device witness".into(),
            ),
            (BindingProofScope::WholeStructure, Self::TableViewportV1 { .. }) => {
                Err("a local table row witness cannot be promoted to whole-structure input".into())
            }
            (BindingProofScope::WindowRoot { .. }, _) => {
                Err("window-root binding scope requires its dedicated WindowRootV1 witness".into())
            }
            (BindingProofScope::BackgroundAx { .. }, _) => Err(
                "background AX binding scope requires its dedicated BackgroundAxV1 witness".into(),
            ),
            (_, Self::WindowRootV1 { .. }) => Err(
                "a WindowRootV1 witness is restricted to the window-root activation scope".into(),
            ),
            (_, Self::BackgroundAxV1 { .. }) => {
                Err("a BackgroundAxV1 witness is restricted to the background AX scope".into())
            }
        }
    }
}

fn validate_window_root_witness(witness: &DeviceContextWitness) -> Result<(), String> {
    let DeviceContextWitness::WindowRootV1 {
        app,
        window,
        root,
        source,
    } = witness
    else {
        return Err("window-root validation requires a WindowRootV1 witness".into());
    };
    let bounded = |text: &str, max: usize| {
        !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
    };
    source.validate()?;
    validate_device_context_object(root)?;
    if !bounded(&app.bundle_id, 256)
        || app.pid == 0
        || !bounded(&window.window_id, 256)
        || !bounded(&window.owner_bundle_id, 256)
        || window.owner_bundle_id != app.bundle_id
        || window.kind != LayerKind::Window
        || window.modal
        || window.title.is_none()
        || window.title.as_ref().is_some_and(|title| {
            title.len() > 512
                || title.chars().any(char::is_control)
                || *title != normalize_label(title)
        })
        || !window.frame.valid_bounded()
        || root.role != "AXWindow"
        || root.subrole.as_deref() != Some("AXStandardWindow")
        || root.frame != window.frame
        || root.label != window.title.as_deref().unwrap_or_default()
        || source.process.pid != app.pid
        || source.origin_window_id != window.window_id
        || source.origin_window_frame != window.frame
    {
        return Err("invalid or inconsistent window-root Device context witness".into());
    }
    Ok(())
}

fn validate_background_ax_witness(witness: &DeviceContextWitness) -> Result<(), String> {
    let DeviceContextWitness::BackgroundAxV1 {
        app,
        window,
        root,
        target,
        target_enabled,
        target_action_names,
        ancestry,
        sibling_parent_ancestry_index,
        siblings,
        target_sibling_index,
        nearest_labelled_group_ancestry_index,
        source,
        target_context_hash,
    } = witness
    else {
        return Err("background AX validation requires its dedicated witness".into());
    };
    source.validate_identity(&app.bundle_id, app.pid)?;
    if canonical_background_ax_action_names(target_action_names)? != *target_action_names {
        return Err("background AX target action names must be canonical and complete".into());
    }
    let source_window = &source.window;
    if !valid_target_context_hash(target_context_hash)
        || target_context_hash != &source.target_context_hash
        || app.bundle_id.is_empty()
        || app.bundle_id.len() > 256
        || app.pid == 0
        || app.pid > i32::MAX as u32
        || window.window_id != source_window.origin_window_id
        || window.frame != source_window.origin_window_frame
        || window.kind != LayerKind::Window
        || window.modal
        || window.owner_bundle_id != app.bundle_id
        || !window.frame.valid_bounded()
        || window
            .title
            .as_ref()
            .is_some_and(|title| title.len() > 512 || *title != normalize_label(title))
        || root.role != "AXWindow"
        || root.subrole.as_deref() != Some("AXStandardWindow")
        || root.label != normalize_label(window.title.as_deref().unwrap_or_default())
        || root.frame != window.frame
        || target.object.role == "AXWindow"
        || !*target_enabled
        || target.scroll.is_some()
        || ancestry.is_empty()
        || ancestry.len() > MAX_DEVICE_CONTEXT_ANCESTRY
        || ancestry.first() != Some(root)
        || ancestry.iter().any(|node| node.role == "AXScrollArea")
        || usize::from(*sibling_parent_ancestry_index) != ancestry.len() - 1
        || siblings.is_empty()
        || siblings.len() > MAX_DEVICE_CONTEXT_SIBLINGS
        || usize::from(*target_sibling_index) >= siblings.len()
        || siblings.get(usize::from(*target_sibling_index)) != Some(&target.object)
    {
        return Err("background AX witness differs from its fixed native-window metadata".into());
    }
    validate_device_context_object(root)?;
    validate_device_context_object(&target.object)?;
    for node in ancestry.iter().chain(siblings.iter()) {
        validate_device_context_object(node)?;
        if node.role == "AXSheet" || node.role == "AXDialog" || node.role == "AXMenu" {
            return Err("background AX witness rejects modal and menu ancestry".into());
        }
    }
    if let Some(index) = nearest_labelled_group_ancestry_index {
        let group = ancestry
            .get(usize::from(*index))
            .ok_or("background AX group index is outside target ancestry")?;
        if !is_group_role(&group.role)
            || group.label.is_empty()
            || target.group.as_ref() != Some(&group.label)
        {
            return Err("background AX group fact differs from its actual ancestor".into());
        }
    } else if target.group.as_ref().is_some_and(|label| !label.is_empty()) {
        return Err("background AX target group has no actual ancestor".into());
    }
    let expected_group = ancestry
        .iter()
        .enumerate()
        .rev()
        .find(|(_, ancestor)| is_group_role(&ancestor.role) && !ancestor.label.is_empty())
        .map(|(index, _)| index);
    if expected_group != nearest_labelled_group_ancestry_index.map(usize::from) {
        return Err("background AX witness does not identify its nearest actual group".into());
    }
    Ok(())
}

fn validate_device_context_object(object: &DeviceContextObject) -> Result<(), String> {
    if object.role.is_empty()
        || object.role.len() > 64
        || object.role.chars().any(char::is_control)
        || object
            .subrole
            .as_ref()
            .is_some_and(|role| role.len() > 128 || role.chars().any(char::is_control))
        || object.label.len() > 512
        || object.label.chars().any(char::is_control)
        || object.label != normalize_label(&object.label)
        || !object.frame.valid_bounded()
    {
        return Err("device context object descriptor is invalid or unbounded".into());
    }
    Ok(())
}

fn validate_table_viewport_witness(witness: &DeviceContextWitness) -> Result<(), String> {
    let DeviceContextWitness::TableViewportV1 {
        app,
        window,
        table,
        target,
        scroll_path,
        viewport_frame,
        ordered_columns,
        selector_columns,
        visible_rows,
        target_row_ax_index,
        table_ax_enabled,
        target_ax_enabled,
        target_selected,
        target_ax_selected_settable,
        hit_point,
    } = witness
    else {
        return Err("row selection requires a TableViewportV1 witness".into());
    };
    let (
        viewport_frame,
        target_row_ax_index,
        table_ax_enabled,
        target_ax_enabled,
        target_selected,
        target_ax_selected_settable,
        hit_point,
    ) = (
        *viewport_frame,
        *target_row_ax_index,
        *table_ax_enabled,
        *target_ax_enabled,
        *target_selected,
        *target_ax_selected_settable,
        *hit_point,
    );
    if app.bundle_id.trim().is_empty()
        || app.bundle_id.len() > 256
        || app.bundle_id.chars().any(char::is_control)
        || app.pid == 0
        || app.pid > i32::MAX as u32
        || window.window_id.trim().is_empty()
        || window.window_id.len() > 256
        || window.window_id.chars().any(char::is_control)
        || window.owner_bundle_id != app.bundle_id
        || window.title.as_ref().is_some_and(|title| {
            title.len() > 512
                || title.chars().any(char::is_control)
                || *title != normalize_label(title)
        })
        || !window.frame.valid_bounded()
        || !matches!(
            (window.kind, window.modal),
            (LayerKind::Window, false) | (LayerKind::Sheet, true)
        )
        || !viewport_frame.valid_bounded()
        || !window.frame.contains_rect(&viewport_frame)
        || !device_context_point_valid(hit_point)
        || table.role != "AXTable"
        || table.label.len() > 512
        || table.label.chars().any(char::is_control)
        || !table.frame.valid_bounded()
        || !window.frame.contains_rect(&table.frame)
        || !table.frame.intersects(&viewport_frame)
        || scroll_path.is_empty()
        || scroll_path.len() > 8
    {
        return Err(
            "table row witness application, window, table, viewport, or point is invalid".into(),
        );
    }

    validate_device_context_row_object(table, false)?;
    if target.object.role != "AXRow" || target.scroll.is_some() {
        return Err("row witness target must be the actual AXRow with no scroll relation".into());
    }
    validate_device_context_object(&target.object)?;
    if target.group.as_ref().is_some_and(|group| {
        group.len() > 512 || group.chars().any(char::is_control) || *group != normalize_label(group)
    }) {
        return Err("row witness target group is invalid or unbounded".into());
    }
    for (index, scroll) in scroll_path.iter().enumerate() {
        if scroll.role != "AXScrollArea"
            || !scroll.frame.valid_bounded()
            || !window.frame.contains_rect(&scroll.frame)
            || !scroll.frame.contains_rect(&viewport_frame)
            || (index > 0 && !scroll_path[index - 1].frame.contains_rect(&scroll.frame))
        {
            return Err(
                "row witness scroll path must be ordered actual labelled AXScrollAreas".into(),
            );
        }
        validate_device_context_row_object(scroll, true)?;
    }

    if ordered_columns.is_empty() || ordered_columns.len() > 64 {
        return Err("row witness requires 1 to 64 actual ordered AXColumns".into());
    }
    let mut column_indices = std::collections::BTreeSet::new();
    let mut column_headers = std::collections::BTreeSet::new();
    for column in ordered_columns {
        validate_row_context_text("table header", &column.header, 256)?;
        if column.ax_index > 63
            || !column_indices.insert(column.ax_index)
            || !column_headers.insert(row_header_key(&column.header))
            || !column.frame.valid_bounded()
            || !table.frame.intersects(&column.frame)
        {
            return Err("row witness has a missing, duplicate, or invalid actual AXColumn".into());
        }
    }

    if selector_columns.is_empty() || selector_columns.len() > 32 {
        return Err("row witness requires 1 to 32 exact selector columns".into());
    }
    let mut selector_indices = std::collections::BTreeSet::new();
    let mut selector_headers = std::collections::BTreeSet::new();
    for selector in selector_columns {
        validate_row_context_text("selector column header", &selector.header, 256)?;
        if selector.ax_index > 63
            || !selector_indices.insert(selector.ax_index)
            || !selector_headers.insert(row_header_key(&selector.header))
            || !ordered_columns.iter().any(|column| {
                column.ax_index == selector.ax_index && column.header == selector.header
            })
        {
            return Err(
                "row witness selector columns must identify exact unique actual headers".into(),
            );
        }
        validate_row_context_value(&selector.expected_value)?;
    }

    if visible_rows.is_empty() || visible_rows.len() > 256 {
        return Err("row witness requires 1 to 256 actual visible rows".into());
    }
    let mut row_indices = std::collections::BTreeSet::new();
    let mut matching_rows = Vec::new();
    let mut target_row = None;
    for row in visible_rows {
        if !row_indices.insert(row.ax_index)
            || !row.frame.valid_bounded()
            || !table.frame.contains_rect(&row.frame)
            || !viewport_frame.intersects(&row.frame)
            || row.selector_values.len() != selector_columns.len()
        {
            return Err(
                "row witness has a missing, duplicate, or invalid actual visible AXRow".into(),
            );
        }
        let mut matches_selector = true;
        for (value, selector) in row.selector_values.iter().zip(selector_columns) {
            validate_row_context_value(value)?;
            if value != &selector.expected_value {
                matches_selector = false;
            }
        }
        if matches_selector {
            matching_rows.push(row.ax_index);
        }
        if row.ax_index == target_row_ax_index {
            target_row = Some(row);
        }
    }
    let target_row = target_row.ok_or("row witness target AXIndex is not visible")?;
    if matching_rows.len() != 1
        || matching_rows.first() != Some(&target_row_ax_index)
        || !table_ax_enabled.permits_row_selection()
        || !target_ax_enabled.permits_row_selection()
        || target_selected
        || !target_ax_selected_settable
        || !viewport_frame.contains(hit_point.x, hit_point.y)
        || !target_row.frame.contains(hit_point.x, hit_point.y)
    {
        return Err("row witness selector must uniquely identify an unselected, settable target row and bound point".into());
    }

    Ok(())
}

fn validate_device_context_row_object(
    object: &DeviceContextObject,
    require_public_label: bool,
) -> Result<(), String> {
    let label_valid = object.label.len() <= 512 && !object.label.chars().any(char::is_control);
    if object.role.is_empty()
        || object.role.len() > 64
        || object.role.chars().any(char::is_control)
        || object
            .subrole
            .as_ref()
            .is_some_and(|role| role.len() > 128 || role.chars().any(char::is_control))
        || !label_valid
        || (require_public_label && object.label.trim().is_empty())
        || !object.frame.valid_bounded()
    {
        return Err("row witness object descriptor is invalid or unbounded".into());
    }
    Ok(())
}

fn validate_row_context_text(field: &str, text: &str, max_bytes: usize) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max_bytes || text.chars().any(char::is_control) {
        return Err(format!("{field} is empty, unbounded, or contains controls"));
    }
    Ok(())
}

fn row_header_key(header: &str) -> String {
    header.trim().chars().flat_map(char::to_lowercase).collect()
}

fn validate_row_context_value(value: &UiValue) -> Result<(), String> {
    match value {
        UiValue::Bool(_) => Ok(()),
        UiValue::Text(text) => validate_row_context_text("row selector value", text, 2_048),
        UiValue::Number(value) if value.is_finite() && value.abs() <= 1.0e12 => Ok(()),
        UiValue::Number(_) => Err("row selector number is non-finite or out of bounds".into()),
    }
}

fn device_context_point_valid(point: DeviceContextPoint) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.x.abs() <= 100_000.0
        && point.y.abs() <= 100_000.0
}

fn is_table_related_role(role: &str) -> bool {
    matches!(role, "AXTable" | "AXOutline")
}

fn validate_device_context_table_column(
    relation: Option<&DeviceContextTableColumnRelation>,
    target: &DeviceContextTarget,
    ancestry: &[DeviceContextObject],
) -> Result<(), String> {
    let table_ancestors: Vec<_> = ancestry
        .iter()
        .enumerate()
        .filter(|(_, node)| node.role == "AXTable")
        .collect();
    let table_related = is_table_related_role(&target.object.role)
        || (target.object.role == "AXButton"
            && target.object.subrole.as_deref() == Some("AXSortButton"))
        || ancestry
            .iter()
            .any(|node| is_table_related_role(&node.role));
    let Some(relation) = relation else {
        return if table_related {
            Err(
                "table-related whole-structure witness requires its exact sortable column relation"
                    .into(),
            )
        } else {
            Ok(())
        };
    };

    if target.object.role != "AXButton"
        || target.object.subrole.as_deref() != Some("AXSortButton")
        || relation.table_ancestry_index > 31
        || relation.ordered_ax_indices.is_empty()
        || relation.ordered_ax_indices.len() > 64
        || relation.ordered_ax_indices.iter().any(|index| *index > 63)
        || table_ancestors.len() != 1
        || table_ancestors[0].0 != usize::from(relation.table_ancestry_index)
        || ancestry.iter().any(|node| node.role == "AXOutline")
    {
        return Err(
            "sortable table column relation has an invalid target or table ancestry".into(),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    if relation
        .ordered_ax_indices
        .iter()
        .any(|index| !seen.insert(*index))
        || relation.target_ax_index > 63
        || relation
            .ordered_ax_indices
            .iter()
            .filter(|index| **index == relation.target_ax_index)
            .count()
            != 1
    {
        return Err("sortable table column relation requires unique ordered AX indices and one target index".into());
    }
    Ok(())
}

fn validate_device_context_menu(
    menu: &DeviceContextMenuRelation,
    app: &DeviceContextApp,
) -> Result<(), String> {
    let (origin_window_id, origin_window_frame, opener_fingerprint) = match menu {
        DeviceContextMenuRelation::OpenerAnchor {
            origin_window_id,
            origin_window_frame,
            opener_fingerprint,
            ..
        }
        | DeviceContextMenuRelation::AnchoredMenu {
            origin_window_id,
            origin_window_frame,
            opener_fingerprint,
            ..
        } => (origin_window_id, origin_window_frame, opener_fingerprint),
    };
    opener_fingerprint.validate()?;
    if *origin_window_id != opener_fingerprint.window_id
        || !origin_window_frame.valid_bounded()
        || opener_fingerprint.app != app.bundle_id
        || opener_fingerprint.app_pid != app.pid
        || !matches!(
            opener_fingerprint.role.as_str(),
            "AXMenuButton" | "AXPopUpButton"
        )
    {
        return Err("device context menu opener facts are inconsistent".into());
    }
    if let DeviceContextMenuRelation::AnchoredMenu {
        menu_fingerprint,
        menu_frame,
        drawing_window,
        ..
    } = menu
    {
        menu_fingerprint.validate()?;
        if menu_fingerprint.app != app.bundle_id
            || menu_fingerprint.app_pid != app.pid
            || menu_fingerprint.window_id != opener_fingerprint.window_id
            || menu_fingerprint.role != "AXMenu"
            || menu_fingerprint.frame != round_frame(*menu_frame)
            || drawing_window.window_id == 0
            || drawing_window.owner_pid != app.pid
            || drawing_window.layer != 101
            || !drawing_window.frame.valid_bounded()
            || drawing_window.frame != *menu_frame
        {
            return Err("device context anchored menu facts are inconsistent".into());
        }
    }
    Ok(())
}

fn round_frame(frame: Rect) -> [i64; 4] {
    [frame.x, frame.y, frame.width, frame.height].map(|value| value.round() as i64)
}

/// The continuity commitment and Device's value-only current-context witness,
/// compiled from one exact Source closure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingDerivationEvidence {
    pub continuity: BindingContinuityProof,
    pub device_context: DeviceContextWitness,
}

#[derive(Serialize)]
struct WindowRootBindingClosure<'a> {
    version: u8,
    continuity_kind: ObjectIdentityContinuity,
    source_instance: &'a ObjectRef,
    scope: &'a BindingProofScope,
    fingerprint: &'a InstanceFingerprint,
    display_frame: Rect,
    app: &'a DeviceContextApp,
    window: &'a DeviceContextWindow,
    root: &'a DeviceContextObject,
    source_session_id: &'a str,
    source_session_epoch: u64,
    process: &'a super::source::KernelProcessIdentity,
    origin_window_id: &'a str,
    origin_window_frame: Rect,
}

fn window_root_context_digest(
    source_instance: &ObjectRef,
    continuity_kind: ObjectIdentityContinuity,
    scope: &BindingProofScope,
    fingerprint: &InstanceFingerprint,
    display_frame: Rect,
    witness: &DeviceContextWitness,
) -> Result<String, String> {
    let DeviceContextWitness::WindowRootV1 {
        app,
        window,
        root,
        source,
    } = witness
    else {
        return Err("window-root digest requires its dedicated Device witness".into());
    };
    let closure = WindowRootBindingClosure {
        version: 1,
        continuity_kind,
        source_instance,
        scope,
        fingerprint,
        display_frame,
        app,
        window,
        root,
        source_session_id: &source.source_session_id,
        source_session_epoch: source.session_epoch,
        process: &source.process,
        origin_window_id: &source.origin_window_id,
        origin_window_frame: source.origin_window_frame,
    };
    let canonical = serde_json::to_vec(&closure)
        .map_err(|_| "window-root binding context could not be canonicalized")?;
    let digest = Sha256::digest(canonical);
    let mut context_digest = String::from("bc1:");
    for byte in digest {
        context_digest.push_str(&format!("{byte:02x}"));
    }
    Ok(context_digest)
}

#[derive(Serialize)]
struct BackgroundAxBindingClosure<'a> {
    version: u8,
    continuity_kind: ObjectIdentityContinuity,
    source_instance: &'a ObjectRef,
    scope: &'a BindingProofScope,
    fingerprint: &'a InstanceFingerprint,
    display_frame: Rect,
    app: &'a DeviceContextApp,
    window: &'a DeviceContextWindow,
    root: &'a DeviceContextObject,
    target: &'a DeviceContextTarget,
    target_enabled: bool,
    target_action_names: &'a [String],
    ancestry: &'a [DeviceContextObject],
    sibling_parent_ancestry_index: u8,
    siblings: &'a [DeviceContextObject],
    target_sibling_index: u16,
    nearest_labelled_group_ancestry_index: Option<u8>,
    source_session_id: &'a str,
    source_session_epoch: u64,
    process: &'a super::source::KernelProcessIdentity,
    origin_window_id: &'a str,
    origin_window_frame: Rect,
    hidden: &'a super::source::ObservationFact<bool>,
    minimized: &'a super::source::ObservationFact<bool>,
    target_context_hash: &'a str,
}

fn background_ax_context_digest(
    source_instance: &ObjectRef,
    continuity_kind: ObjectIdentityContinuity,
    scope: &BindingProofScope,
    fingerprint: &InstanceFingerprint,
    display_frame: Rect,
    witness: &DeviceContextWitness,
) -> Result<String, String> {
    let (
        app,
        window,
        root,
        target,
        target_enabled,
        target_action_names,
        ancestry,
        sibling_parent_ancestry_index,
        siblings,
        target_sibling_index,
        nearest_labelled_group_ancestry_index,
        source,
        target_context_hash,
    ) = match witness {
        DeviceContextWitness::BackgroundAxV1 {
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        } => (
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        ),
        _ => return Err("background AX digest requires its dedicated Device witness".into()),
    };
    let BindingProofScope::BackgroundAx {
        target_context_hash: scope_hash,
    } = scope
    else {
        return Err("background AX digest requires its dedicated binding scope".into());
    };
    if scope_hash != target_context_hash || scope_hash != &source.target_context_hash {
        return Err("background AX target-context hash differs across scope and witness".into());
    }
    let closure = BackgroundAxBindingClosure {
        version: 1,
        continuity_kind,
        source_instance,
        scope,
        fingerprint,
        display_frame,
        app,
        window,
        root,
        target,
        target_enabled: *target_enabled,
        target_action_names,
        ancestry,
        sibling_parent_ancestry_index: *sibling_parent_ancestry_index,
        siblings,
        target_sibling_index: *target_sibling_index,
        nearest_labelled_group_ancestry_index: *nearest_labelled_group_ancestry_index,
        source_session_id: &source.window.source_session_id,
        source_session_epoch: source.window.session_epoch,
        process: &source.window.process,
        origin_window_id: &source.window.origin_window_id,
        origin_window_frame: source.window.origin_window_frame,
        hidden: &source.hidden,
        minimized: &source.minimized,
        target_context_hash,
    };
    let canonical = serde_json::to_vec(&closure)
        .map_err(|_| "background AX binding context could not be canonicalized")?;
    let mut context_digest = String::from("bc1:");
    for byte in Sha256::digest(canonical) {
        context_digest.push_str(&format!("{byte:02x}"));
    }
    Ok(context_digest)
}

struct CompiledBindingDerivation {
    continuity: BindingContinuityProof,
    device_context: Result<DeviceContextWitness, String>,
}

struct WholeStructureWitnessInput<'a> {
    snapshot: &'a StructureSnapshot,
    target: &'a UiObject,
    window: &'a WindowInfo,
    ancestry: &'a [&'a UiObject],
    direct_siblings: &'a [&'a UiObject],
    nearest_labelled_group: Option<&'a UiObject>,
    target_is_menu_related: bool,
    table_column: Option<DeviceContextTableColumnRelation>,
    menu_context: Option<&'a MenuContext>,
    menu_anchors: &'a [&'a MenuOpenerAnchor],
}

/// Continuity evidence for the exact object/context closure used by one binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingContinuityProof {
    pub kind: ObjectIdentityContinuity,
    pub source_sequence: u64,
    pub context_digest: String,
    pub scope: BindingProofScope,
}

impl BindingContinuityProof {
    pub fn validate(&self) -> Result<(), String> {
        let valid_digest = self.context_digest.strip_prefix("bc1:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if self.source_sequence == 0 || !valid_digest {
            return Err("binding continuity requires a positive source sequence and bc1 lowercase SHA-256 digest".into());
        }
        self.scope.validate()
    }

    /// A covered proof can refresh its source sequence only while the exact
    /// closure remains unchanged. Read-scoped proofs expire on the next read.
    pub fn same_closure(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.context_digest == other.context_digest
            && self.scope == other.scope
            && (self.kind == ObjectIdentityContinuity::NotificationCovered
                || self.source_sequence == other.source_sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FocusedKeyProfile {
    MacosAtomicSpecialKeyV1,
}

/// Exact public KeyPress names; no case folding, aliases or numeric keycodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LayoutIndependentSpecialKey {
    Space,
    Return,
    Tab,
    Escape,
    Backspace,
    DeleteForward,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    PageUp,
    PageDown,
}

impl LayoutIndependentSpecialKey {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::Space => "Space",
            Self::Return => "Return",
            Self::Tab => "Tab",
            Self::Escape => "Escape",
            Self::Backspace => "Backspace",
            Self::DeleteForward => "DeleteForward",
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
        }
    }
}

/// Trusted execution profile plus the exact resolved layout-independent key.
/// This contract does not grant a native capability for every listed key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusedControlScope {
    pub context: FocusContext,
    pub profile: FocusedKeyProfile,
    pub key: LayoutIndependentSpecialKey,
}

impl FocusedControlScope {
    pub fn validate(&self) -> Result<(), String> {
        self.context.validate()?;
        if self.context.composition != CompositionState::NotApplicable {
            return Err(
                "the initial nontext special-key profile requires actual nontext composition scope"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.profile == other.profile
            && self.key == other.key
            && self.context.same_identity(&other.context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuOpeningRef {
    pub attempt_id: AttemptId,
    pub intent_id: String,
    pub attempt_deadline_ms: u64,
}

impl MenuOpeningRef {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("menu opening intent id", &self.intent_id)?;
        if self.attempt_deadline_ms == 0 {
            return Err("menu opening must retain a nonzero attempt deadline".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
// Both variants retain a bounded, value-only source proof. Keeping that proof
// inline avoids an additional allocation for each short-lived input binding.
#[allow(clippy::large_enum_variant)]
pub enum MenuBindingScope {
    Opener {
        anchor: MenuOpenerAnchor,
        attempt_deadline_ms: u64,
    },
    Item {
        context: MenuContext,
        opened_by: MenuOpeningRef,
    },
}

impl MenuBindingScope {
    pub fn source_anchor(&self) -> &MenuOpenerAnchor {
        match self {
            Self::Opener { anchor, .. } => anchor,
            Self::Item { context, .. } => &context.opener,
        }
    }

    pub fn attempt_deadline_ms(&self) -> u64 {
        match self {
            Self::Opener {
                attempt_deadline_ms,
                ..
            } => *attempt_deadline_ms,
            Self::Item { opened_by, .. } => opened_by.attempt_deadline_ms,
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Opener {
                    anchor: a,
                    attempt_deadline_ms: a_deadline,
                },
                Self::Opener {
                    anchor: b,
                    attempt_deadline_ms: b_deadline,
                },
            ) => a.same_identity(b) && a_deadline == b_deadline,
            (
                Self::Item {
                    context: a,
                    opened_by: a_open,
                },
                Self::Item {
                    context: b,
                    opened_by: b_open,
                },
            ) => a.same_identity(b) && a_open == b_open,
            _ => false,
        }
    }
}

/// The source instance and complete descriptor a permit was admitted for.
/// A digest alone cannot distinguish identical replacement instances.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingProof {
    pub binding: BindingRef,
    pub instance: ObjectRef,
    pub snapshot_id: RequestId,
    #[schemars(range(min = 1))]
    pub environment_revision: u64,
    #[schemars(range(min = 1))]
    pub layer_revision: u64,
    pub fingerprint: InstanceFingerprint,
    /// Retain subpoint geometry; fp1's rounded frame cannot reconstruct it.
    pub display_frame: Rect,
    pub continuity: BindingContinuityProof,
    pub device_context: DeviceContextWitness,
    pub menu_scope: Option<MenuBindingScope>,
    pub focus_scope: Option<FocusedControlScope>,
}

impl BindingProof {
    pub fn supports_native_input(&self) -> bool {
        self.device_context.supports_native_input()
    }

    /// Window activation accepts the existing complete-tree witness or the
    /// dedicated frozen-scope normal-window root witness. Neither grants the
    /// general native-input capability.
    pub fn supports_window_activation(&self) -> bool {
        let root_fingerprint = self.fingerprint.role == "AXWindow"
            && self.fingerprint.subrole.as_deref() == Some("AXStandardWindow")
            && self.fingerprint.ancestor_roles.is_empty()
            && self.fingerprint.group.is_none()
            && self.fingerprint.scroll_container.is_none()
            && self.fingerprint.sibling_index == 0;
        let paired = matches!(
            (&self.continuity.scope, &self.device_context),
            (
                BindingProofScope::WholeStructure,
                DeviceContextWitness::WholeStructureV1 { .. }
            ) | (
                BindingProofScope::WindowRoot { .. },
                DeviceContextWitness::WindowRootV1 { .. }
            )
        );
        paired
            && self.device_context.supports_window_activation()
            && root_fingerprint
            && self.menu_scope.is_none()
            && self.focus_scope.is_none()
            && self.validate().is_ok()
    }

    pub fn supports_background_ax_press(&self) -> bool {
        matches!(
            (&self.continuity.scope, &self.device_context),
            (
                BindingProofScope::BackgroundAx { .. },
                DeviceContextWitness::BackgroundAxV1 { .. }
            )
        ) && self.menu_scope.is_none()
            && self.focus_scope.is_none()
            && self.device_context.supports_background_ax_press()
            && self.validate().is_ok()
    }

    pub fn supports_execution_domain(&self, domain: super::domain::ExecutionDomain) -> bool {
        match domain {
            super::domain::ExecutionDomain::ForegroundNative => {
                self.device_context.supports_execution_domain(domain) && self.validate().is_ok()
            }
            super::domain::ExecutionDomain::BackgroundAx => self.supports_background_ax_press(),
            super::domain::ExecutionDomain::HeadlessBrowser
            | super::domain::ExecutionDomain::AppIntegration => false,
            super::domain::ExecutionDomain::Simulated => {
                self.device_context.supports_execution_domain(domain) && self.validate().is_ok()
            }
        }
    }

    /// True only for the dedicated local AXSelected=true row-selection route.
    /// Whole-structure and generic pointer/AX paths remain separate.
    pub fn supports_row_selection(&self) -> bool {
        matches!(
            &self.continuity.scope,
            BindingProofScope::TableViewport { .. }
        ) && self.menu_scope.is_none()
            && self.focus_scope.is_none()
            && self.device_context.supports_row_selection()
            && self.validate().is_ok()
    }

    /// The exact candidate point committed by the row-selection witness.
    pub fn row_selection_point(&self) -> Option<(f64, f64)> {
        self.supports_row_selection()
            .then(|| match &self.device_context {
                DeviceContextWitness::TableViewportV1 { hit_point, .. } => {
                    (hit_point.x, hit_point.y)
                }
                _ => unreachable!("supports_row_selection checked the witness variant"),
            })
    }

    pub fn validate(&self) -> Result<(), String> {
        self.fingerprint.validate()?;
        self.continuity.validate()?;
        if self.menu_scope.is_some() && self.focus_scope.is_some() {
            return Err("menu and focused-control scopes are mutually exclusive".into());
        }
        self.device_context.validate_binding(
            &self.continuity.scope,
            &self.fingerprint,
            self.display_frame,
            self.menu_scope.as_ref(),
            self.focus_scope.as_ref(),
        )?;
        if let DeviceContextWitness::WindowRootV1 { source, .. } = &self.device_context {
            if self.continuity.source_sequence != source.verified_source_sequence {
                return Err(
                    "window-root source sequence differs from its continuity provenance".into(),
                );
            }
            let expected = window_root_context_digest(
                &self.instance,
                self.continuity.kind,
                &self.continuity.scope,
                &self.fingerprint,
                self.display_frame,
                &self.device_context,
            )?;
            if self.continuity.context_digest != expected {
                return Err("window-root continuity hash differs from its scope or witness".into());
            }
        }
        if let DeviceContextWitness::BackgroundAxV1 { source, .. } = &self.device_context {
            if self.continuity.source_sequence != source.window.verified_source_sequence {
                return Err(
                    "background AX source sequence differs from its continuity provenance".into(),
                );
            }
            let expected = background_ax_context_digest(
                &self.instance,
                self.continuity.kind,
                &self.continuity.scope,
                &self.fingerprint,
                self.display_frame,
                &self.device_context,
            )?;
            if self.continuity.context_digest != expected {
                return Err(
                    "background AX continuity hash differs from its scope or witness".into(),
                );
            }
        }
        if let Some(scope) = &self.focus_scope {
            scope.validate()?;
            if self.instance != scope.context.target_instance
                || self.fingerprint != scope.context.target_fingerprint
                || self.display_frame != scope.context.target_frame
            {
                return Err("focused binding must match its actual current Source receiver".into());
            }
        }
        let f = self.display_frame;
        let values = [f.x, f.y, f.width, f.height];
        if self.environment_revision == 0
            || self.layer_revision == 0
            || values.iter().any(|v| !v.is_finite() || v.abs() > 100_000.0)
            || f.width <= 0.0
            || f.height <= 0.0
            || values.map(|v| v.round() as i64) != self.fingerprint.frame
        {
            return Err("invalid binding proof revision or display frame".into());
        }
        match &self.menu_scope {
            Some(MenuBindingScope::Opener {
                anchor,
                attempt_deadline_ms,
            }) => {
                anchor.validate()?;
                if *attempt_deadline_ms <= anchor.verified_observed_ms
                    || self.instance != anchor.opener_instance
                    || self.fingerprint != anchor.opener_fingerprint
                    || self.display_frame != anchor.opener_frame
                {
                    return Err("opener binding must match its current source anchor".into());
                }
            }
            Some(MenuBindingScope::Item { context, opened_by }) => {
                context.validate()?;
                opened_by.validate()?;
                let menu = &context.menu_fingerprint;
                let mut roles = menu.ancestor_roles.clone();
                roles.push("AXMenu".into());
                let fp = &self.fingerprint;
                if opened_by.attempt_deadline_ms <= context.opener.verified_observed_ms
                    || self.instance == context.menu_instance
                    || self.instance == context.opener.opener_instance
                    || fp.role != "AXMenuItem"
                    || fp.app != menu.app
                    || fp.app_pid != menu.app_pid
                    || fp.window_id != menu.window_id
                    || fp.ancestor_roles != roles
                    || fp.scroll_container.is_some()
                    || !context.menu_frame.contains_rect(&self.display_frame)
                {
                    return Err(
                        "item binding must describe a visible direct child of the anchored menu"
                            .into(),
                    );
                }
            }
            None => {
                if matches!(
                    self.fingerprint.role.as_str(),
                    "AXMenu" | "AXMenuItem" | "AXMenuBar"
                ) || self
                    .fingerprint
                    .ancestor_roles
                    .iter()
                    .any(|role| role == "AXMenu" || role == "AXMenuBar")
                {
                    return Err("menu items require an explicit menu binding scope".into());
                }
            }
        }
        Ok(())
    }

    /// New evidence may refresh provenance, never switch the admitted instance,
    /// group, layer or geometry. Unrelated environment revisions may advance.
    pub fn same_instance(&self, other: &Self) -> bool {
        self.binding == other.binding
            && self.instance == other.instance
            && self.layer_revision == other.layer_revision
            && self.fingerprint == other.fingerprint
            && self.display_frame == other.display_frame
            && self.continuity.same_closure(&other.continuity)
            && match (&self.device_context, &other.device_context) {
                (
                    DeviceContextWitness::WindowRootV1 {
                        app: a_app,
                        window: a_window,
                        root: a_root,
                        source: a_source,
                    },
                    DeviceContextWitness::WindowRootV1 {
                        app: b_app,
                        window: b_window,
                        root: b_root,
                        source: b_source,
                    },
                ) => {
                    a_app == b_app
                        && a_window == b_window
                        && a_root == b_root
                        && a_source.source_session_id == b_source.source_session_id
                        && a_source.session_epoch == b_source.session_epoch
                        && a_source.process == b_source.process
                        && a_source.origin_window_id == b_source.origin_window_id
                        && a_source.origin_window_frame == b_source.origin_window_frame
                }
                (
                    DeviceContextWitness::BackgroundAxV1 {
                        app: a_app,
                        window: a_window,
                        root: a_root,
                        target: a_target,
                        ancestry: a_ancestry,
                        sibling_parent_ancestry_index: a_parent_index,
                        siblings: a_siblings,
                        target_sibling_index: a_target_index,
                        nearest_labelled_group_ancestry_index: a_group_index,
                        source: a_source,
                        target_context_hash: a_hash,
                        target_enabled: a_enabled,
                        target_action_names: a_actions,
                    },
                    DeviceContextWitness::BackgroundAxV1 {
                        app: b_app,
                        window: b_window,
                        root: b_root,
                        target: b_target,
                        ancestry: b_ancestry,
                        sibling_parent_ancestry_index: b_parent_index,
                        siblings: b_siblings,
                        target_sibling_index: b_target_index,
                        nearest_labelled_group_ancestry_index: b_group_index,
                        source: b_source,
                        target_context_hash: b_hash,
                        target_enabled: b_enabled,
                        target_action_names: b_actions,
                    },
                ) => {
                    a_app == b_app
                        && a_enabled == b_enabled
                        && a_actions == b_actions
                        && a_window == b_window
                        && a_root == b_root
                        && a_target == b_target
                        && a_ancestry == b_ancestry
                        && a_parent_index == b_parent_index
                        && a_siblings == b_siblings
                        && a_target_index == b_target_index
                        && a_group_index == b_group_index
                        && a_hash == b_hash
                        && a_source.target_context_hash == b_source.target_context_hash
                        && a_source.window.source_session_id == b_source.window.source_session_id
                        && a_source.window.session_epoch == b_source.window.session_epoch
                        && a_source.window.process == b_source.window.process
                        && a_source.window.origin_window_id == b_source.window.origin_window_id
                        && a_source.window.origin_window_frame
                            == b_source.window.origin_window_frame
                        && a_source.hidden == b_source.hidden
                        && a_source.minimized == b_source.minimized
                }
                (DeviceContextWitness::WindowRootV1 { .. }, _)
                | (_, DeviceContextWitness::WindowRootV1 { .. }) => false,
                (DeviceContextWitness::BackgroundAxV1 { .. }, _)
                | (_, DeviceContextWitness::BackgroundAxV1 { .. }) => false,
                _ => self.device_context == other.device_context,
            }
            && match (&self.focus_scope, &other.focus_scope) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.same_identity(b)
                        && self.environment_revision <= other.environment_revision
                        && a.context.verified_source_sequence <= b.context.verified_source_sequence
                        && a.context.verified_observed_ms <= b.context.verified_observed_ms
                }
                _ => false,
            }
            && match (&self.menu_scope, &other.menu_scope) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.same_identity(b)
                        && self.environment_revision <= other.environment_revision
                        && a.source_anchor().verified_source_sequence
                            <= b.source_anchor().verified_source_sequence
                        && a.source_anchor().verified_observed_ms
                            <= b.source_anchor().verified_observed_ms
                }
                _ => false,
            }
    }
}

/// Everything that identifies one instance across observations. It is a
/// continuity check and a pre-submit proof, not a long-lived identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceFingerprint {
    pub app: String,
    /// A replacement process with the same bundle and layout is a new context.
    pub app_pid: u32,
    pub window_id: String,
    pub ancestor_roles: Vec<String>,
    /// Normalised title of the nearest labelled group, so the same label in two
    /// groups never shares a fingerprint even when layouts are reused.
    pub group: Option<String>,
    pub role: String,
    pub subrole: Option<String>,
    /// Normalised title or description.
    pub label: String,
    pub sibling_index: u32,
    /// Frame in display points, rounded to whole points.
    pub frame: [i64; 4],
    pub scroll_container: Option<String>,
}

impl InstanceFingerprint {
    pub fn validate(&self) -> Result<(), String> {
        let bounded = |s: &str, limit: usize| !s.is_empty() && s.len() <= limit;
        if !bounded(&self.app, 256)
            || self.app_pid == 0
            || !bounded(&self.window_id, 256)
            || self.ancestor_roles.len() > 32
            || self.ancestor_roles.iter().any(|v| !bounded(v, 64))
            || !bounded(&self.role, 64)
            || self.subrole.as_ref().is_some_and(|s| !bounded(s, 128))
            || self.label.len() > 512
            || self.label != normalize_label(&self.label)
            || self
                .group
                .as_ref()
                .is_some_and(|s| s.len() > 512 || *s != normalize_label(s))
            || self.sibling_index >= 256
            || self.frame.iter().any(|v| !(-100_000..=100_000).contains(v))
            || self.frame[2] <= 0
            || self.frame[3] <= 0
            || self
                .scroll_container
                .as_ref()
                .is_some_and(|s| !bounded(s, 256))
        {
            return Err("invalid or unbounded instance fingerprint".into());
        }
        Ok(())
    }

    /// A stable digest. Field order is fixed by the struct, so the same
    /// fingerprint always yields the same string on the host and the device.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("a fingerprint always serialises");
        let hash = Sha256::digest(&canonical);
        let mut out = String::from("fp1:");
        for byte in hash {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// Lower-case, collapse whitespace: the text form used in labels and matching.
pub fn normalize_label(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The fingerprint of one object as it appears in one snapshot.
///
/// The host computes it when binding and a device backend computes it for
/// whatever is under the pointer, so both must use exactly this function.
pub fn instance_fingerprint(
    snapshot: &StructureSnapshot,
    object: &UiObject,
) -> InstanceFingerprint {
    let mut ancestor_roles = Vec::new();
    let mut cursor = object.parent.as_ref();
    while let Some(parent_ref) = cursor {
        let Some(parent) = snapshot
            .objects
            .iter()
            .find(|candidate| &candidate.instance_ref == parent_ref)
        else {
            break;
        };
        ancestor_roles.push(parent.role.clone());
        cursor = parent.parent.as_ref();
        if ancestor_roles.len() > 32 {
            break;
        }
    }
    ancestor_roles.reverse();
    let sibling_index = snapshot
        .objects
        .iter()
        .filter(|candidate| {
            candidate.parent == object.parent && candidate.window_id == object.window_id
        })
        .position(|candidate| candidate.instance_ref == object.instance_ref)
        .unwrap_or(0) as u32;
    let label = object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("");
    InstanceFingerprint {
        app: snapshot.app.bundle_id.clone(),
        app_pid: snapshot.app.pid,
        window_id: object.window_id.clone(),
        ancestor_roles,
        group: object.group.as_deref().map(normalize_label),
        role: object.role.clone(),
        subrole: object.subrole.clone(),
        label: normalize_label(label),
        sibling_index,
        frame: [
            object.frame.x.round() as i64,
            object.frame.y.round() as i64,
            object.frame.width.round() as i64,
            object.frame.height.round() as i64,
        ],
        scroll_container: None,
    }
}

#[derive(Serialize)]
struct ClosureNode<'a> {
    instance_ref: &'a ObjectRef,
    parent: Option<&'a ObjectRef>,
    window_id: &'a str,
    role: &'a str,
    subrole: &'a Option<String>,
    label: String,
    group: Option<String>,
    frame: Rect,
    scroll: Option<&'a ScrollState>,
}

#[derive(Serialize)]
struct SiblingNode<'a> {
    instance_ref: &'a ObjectRef,
    parent: Option<&'a ObjectRef>,
    window_id: &'a str,
    role: &'a str,
    subrole: &'a Option<String>,
    label: String,
    frame: Rect,
}

#[derive(Serialize)]
struct TableViewportClosure<'a> {
    table_instance_ref: &'a ObjectRef,
    scroll_container_instance_ref: &'a ObjectRef,
    process_id: u32,
    ax_enabled: AxEnabledObservation,
    table_frame: Rect,
    viewport_frame: Rect,
    visible_rows_complete: bool,
    columns_complete: bool,
    visible_row_order: Vec<Option<&'a ObjectRef>>,
    visible_row_ax_indices: Vec<Option<u32>>,
    column_order: Vec<Option<&'a ObjectRef>>,
    ordered_ax_indices: Vec<Option<u32>>,
    target_row: Option<TableRowClosure<'a>>,
    target_column: Option<TableColumnClosure<'a>>,
    target_cell: Option<TableCellClosure<'a>>,
}

#[derive(Clone, Serialize)]
struct RowSelectionConditionClosure {
    ax_index: u8,
    header: String,
    expected_value: UiValue,
}

#[derive(Clone, Serialize)]
struct RowSelectionVisibleRowClosure {
    ax_index: u32,
    frame: Rect,
    selector_values: Vec<UiValue>,
}

#[derive(Clone, Serialize)]
struct RowSelectionColumnClosure {
    ax_index: u8,
    header: String,
    frame: Rect,
}

#[derive(Clone, Serialize)]
struct RowSelectionContextClosure {
    table: DeviceContextObject,
    table_ax_enabled: AxEnabledObservation,
    target_ax_enabled: AxEnabledObservation,
    scroll_path: Vec<DeviceContextObject>,
    viewport_frame: Rect,
    ordered_columns: Vec<RowSelectionColumnClosure>,
    selector_columns: Vec<RowSelectionConditionClosure>,
    visible_rows: Vec<RowSelectionVisibleRowClosure>,
    target_row_ax_index: u32,
    target_selected: bool,
    target_ax_selected_settable: bool,
    hit_point: DeviceContextPoint,
}

struct RowSelectionCompiledContext {
    closure: RowSelectionContextClosure,
    witness: DeviceContextWitness,
}

#[derive(Clone, Copy)]
struct RowSelectionCompileInput<'a> {
    selector: &'a RowSelector,
    observation: &'a RowSelectionObservation,
    hit_point: DeviceContextPoint,
    now_ms: u64,
    max_age_ms: u64,
}

#[derive(Serialize)]
struct TableRowClosure<'a> {
    instance_ref: &'a Option<ObjectRef>,
    parent_instance_ref: &'a ObjectRef,
    frame: Rect,
    accessibility_identifier: Option<&'a str>,
    ax_index: &'a Option<u32>,
    ax_enabled: AxEnabledObservation,
    cells_complete: bool,
    cells: Vec<TableCellClosure<'a>>,
}

#[derive(Serialize)]
struct TableColumnClosure<'a> {
    instance_ref: &'a Option<ObjectRef>,
    parent_instance_ref: &'a ObjectRef,
    frame: Rect,
    header_instance_ref: &'a Option<ObjectRef>,
    header: Option<String>,
    ax_index: &'a Option<u32>,
}

#[derive(Serialize)]
struct TableCellClosure<'a> {
    instance_ref: &'a Option<ObjectRef>,
    parent_instance_ref: &'a Option<ObjectRef>,
    frame: Rect,
    row_ax_index: &'a Option<u32>,
    column_ax_index: &'a Option<u32>,
    column_instance_ref: &'a Option<ObjectRef>,
    header_instance_ref: &'a Option<ObjectRef>,
    column_header: Option<String>,
    value: &'a Option<super::source::UiValue>,
}

#[derive(Serialize)]
struct BindingContextClosure<'a> {
    version: u8,
    app_bundle_id: &'a str,
    app_pid: u32,
    identity_boundary: &'a Option<super::source::SourceIdentityBoundary>,
    global_focus: &'a super::source::GlobalFocusObservation,
    target_context: &'a Option<super::source::TargetContext>,
    window: &'a WindowInfo,
    menu_anchors: Vec<&'a MenuOpenerAnchor>,
    menu_context: Option<&'a MenuContext>,
    scope: &'a BindingProofScope,
    fingerprint: InstanceFingerprint,
    target: ClosureNode<'a>,
    ancestry: Vec<ClosureNode<'a>>,
    siblings: Vec<SiblingNode<'a>>,
    nearest_labelled_group: Option<ClosureNode<'a>>,
    scroll_ancestors: Vec<ClosureNode<'a>>,
    table_viewport: Option<TableViewportClosure<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    row_selection: Option<RowSelectionContextClosure>,
}

/// Builds a stable digest from the target's actual Source context closure.
///
/// It includes the target, its actual ancestry, all direct siblings in Source
/// order (the input to `sibling_index`), the nearest labelled group, and only
/// table/scroll projections that actually contain or scope the target. It
/// deliberately omits effect-only fields such as the target value and focus.
pub fn derive_binding_continuity(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    scope: BindingProofScope,
) -> Result<BindingContinuityProof, String> {
    compile_binding_context(snapshot, target, scope).map(|derived| derived.continuity)
}

/// Default Environment candidate: half a display point from the row's left
/// edge (or half the row width when narrower), vertically centered. It is a
/// proposal only; the Native Device performs the actual point query.
pub fn row_selection_candidate_point(row_frame: Rect) -> Result<DeviceContextPoint, String> {
    if !row_frame.valid_bounded() {
        return Err("row selection candidate requires a positive bounded AXRow frame".into());
    }
    let point = DeviceContextPoint {
        x: row_frame.x + 0.5_f64.min(row_frame.width / 2.0),
        y: row_frame.y + row_frame.height / 2.0,
    };
    if !row_frame.contains(point.x, point.y) || !device_context_point_valid(point) {
        return Err("default row selection point falls outside the actual AXRow frame".into());
    }
    Ok(point)
}

/// Derive a row-scoped continuity proof and value-only Device witness from
/// one exact Source snapshot. Read-scoped identities remain read-scoped; this
/// evidence authorizes only the matching single AXSelected=true operation.
pub fn derive_row_selection_binding_evidence(
    snapshot: &StructureSnapshot,
    selector: &RowSelector,
    observation: &RowSelectionObservation,
    hit_point: DeviceContextPoint,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<BindingDerivationEvidence, String> {
    selector.validate()?;
    snapshot.validate()?;
    observation.validate_against_snapshot(selector, snapshot, now_ms, max_age_ms)?;
    if observation.selected != Some(false)
        || observation.ax_selected_settable != Some(true)
        || !observation.table_ax_enabled.permits_row_selection()
        || !observation.row_ax_enabled.permits_row_selection()
        || !device_context_point_valid(hit_point)
    {
        return Err(
            "row selection requires a fresh unselected, settable row and bounded candidate point"
                .into(),
        );
    }
    let viewport = snapshot
        .table_viewports
        .iter()
        .find(|viewport| viewport.table_instance_ref == selector.table_instance_ref)
        .ok_or("row selector table has no actual Source viewport")?;
    let target = snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == observation.row_instance_ref)
        .ok_or("row selection target is absent from the current Source snapshot")?;
    let scope = BindingProofScope::TableViewport {
        table_instance_ref: viewport.table_instance_ref.clone(),
        scroll_container_instance_ref: viewport.scroll_container_instance_ref.clone(),
        viewport_frame: viewport.viewport_frame,
        visible_rows_complete: viewport.visible_rows_complete,
        columns_complete: viewport.columns_complete,
    };
    let derived = compile_binding_context_with_row_selection(
        snapshot,
        target,
        scope,
        Some(RowSelectionCompileInput {
            selector,
            observation,
            hit_point,
            now_ms,
            max_age_ms,
        }),
    )?;
    let device_context = derived.device_context?;
    if !matches!(device_context, DeviceContextWitness::TableViewportV1 { .. }) {
        return Err("row selection did not compile to its dedicated table viewport witness".into());
    }
    device_context.validate()?;
    Ok(BindingDerivationEvidence {
        continuity: derived.continuity,
        device_context,
    })
}

/// Produces a Source continuity commitment and a bounded value-only witness
/// from the same target closure. The Device witness is an expected description
/// to compare with a Device-owned live read, not a mapping to Source refs.
pub fn derive_binding_evidence(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    scope: BindingProofScope,
) -> Result<BindingDerivationEvidence, String> {
    let derived = compile_binding_context(snapshot, target, scope)?;
    let device_context = derived.device_context?;
    device_context.validate()?;
    Ok(BindingDerivationEvidence {
        continuity: derived.continuity,
        device_context,
    })
}

/// Derive a dedicated one-AXPress witness for a complete target graph in a
/// fixed native window context. This proof deliberately does not use global
/// focus or the Source active layer as authority; the target context is taken
/// from this exact snapshot and independently committed into the proof scope.
pub fn derive_background_ax_binding_evidence(
    snapshot: &StructureSnapshot,
    target: &UiObject,
) -> Result<BindingDerivationEvidence, String> {
    snapshot.validate()?;
    if !snapshot.complete {
        return Err("background AX binding requires a complete Source traversal".into());
    }
    if snapshot.source_sequence == 0 {
        return Err("background AX binding requires a positive Source sequence".into());
    }

    let (target_app, source) = match snapshot.target_context.as_ref() {
        Some(super::source::TargetContext::NativeWindow { app, source }) => (app, source),
        _ => {
            return Err(
                "background AX binding requires the exact native-window target context".into(),
            );
        }
    };
    source.validate(target_app)?;
    if target_app != &snapshot.app
        || source.window.session_epoch != snapshot.session_epoch
        || source.window.verified_source_sequence != snapshot.source_sequence
        || source.window.verified_observed_ms != snapshot.observed_ms
        || source.window.process.pid != snapshot.app.pid
        || !source.is_visible_standard_window()
    {
        return Err(
            "background AX binding requires current visible target-context provenance".into(),
        );
    }
    let target_action_names = canonical_background_ax_action_names(
        target
            .ax_action_names
            .as_deref()
            .ok_or("background AX binding requires the exact raw AXActionNames set")?,
    )?;
    if !matches!(
        target.role.as_str(),
        "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
    ) || !target.enabled
        || !target.in_view
        || target.secure
        || target.scroll.is_some()
        || target.window_id != source.window.origin_window_id
    {
        return Err(
            "background AX binding requires an enabled visible AXPress target in the fixed window"
                .into(),
        );
    }

    // Reuse the complete-tree relationship compiler for exact ancestry,
    // sibling order, labels, and target membership. Its foreground layer is
    // intentionally discarded; only its complete structural closure is used.
    let compiled = compile_binding_context(snapshot, target, BindingProofScope::WholeStructure)?;
    let DeviceContextWitness::WholeStructureV1 {
        window,
        target: whole_target,
        ancestry,
        sibling_parent_ancestry_index: Some(sibling_parent_ancestry_index),
        siblings,
        target_sibling_index,
        nearest_labelled_group_ancestry_index,
        scroll_ancestors,
        table_column,
        menu,
        ..
    } = compiled.device_context?
    else {
        return Err("background AX binding requires a complete native target graph".into());
    };
    let root = ancestry
        .first()
        .cloned()
        .ok_or("background AX target graph must reach its actual window root")?;
    if window.kind != LayerKind::Window
        || window.modal
        || window.owner_bundle_id != target_app.bundle_id
        || window.window_id != source.window.origin_window_id
        || window.frame != source.window.origin_window_frame
        || root.role != "AXWindow"
        || root.subrole.as_deref() != Some("AXStandardWindow")
        || root.frame != window.frame
        || scroll_ancestors.iter().any(|ancestor| {
            ancestry
                .get(usize::from(ancestor.ancestry_index))
                .is_some_and(|node| node.role == "AXScrollArea")
        })
        || ancestry.iter().any(|node| {
            matches!(
                node.role.as_str(),
                "AXApplication" | "AXSheet" | "AXDialog" | "AXMenu" | "AXMenuItem" | "AXMenuBar"
            ) || is_table_related_role(&node.role)
                || node.role == "AXScrollArea"
        })
        || menu.is_some()
        || table_column.is_some()
        || whole_target.scroll.is_some()
    {
        return Err(
            "background AX binding rejects nonstandard, modal, menu, table or scroll target context"
                .into(),
        );
    }

    let source = source.clone();
    let target_context_hash = source.target_context_hash.clone();
    let scope = BindingProofScope::BackgroundAx {
        target_context_hash: target_context_hash.clone(),
    };
    let witness = DeviceContextWitness::BackgroundAxV1 {
        app: Box::new(DeviceContextApp {
            bundle_id: target_app.bundle_id.clone(),
            pid: target_app.pid,
        }),
        window,
        root,
        target: whole_target,
        target_enabled: target.enabled,
        target_action_names,
        ancestry,
        sibling_parent_ancestry_index,
        siblings,
        target_sibling_index,
        nearest_labelled_group_ancestry_index,
        source: Box::new(source),
        target_context_hash,
    };
    witness.validate()?;

    let fingerprint = instance_fingerprint(snapshot, target);
    let context_digest = background_ax_context_digest(
        &target.instance_ref,
        compiled.continuity.kind,
        &scope,
        &fingerprint,
        target.frame,
        &witness,
    )?;
    let continuity = BindingContinuityProof {
        kind: compiled.continuity.kind,
        source_sequence: snapshot.source_sequence,
        context_digest,
        scope,
    };
    continuity.validate()?;
    Ok(BindingDerivationEvidence {
        continuity,
        device_context: witness,
    })
}

/// Derive a normal-window root witness from one exact Source read. This is a
/// separate path from generic binding: a scoped census never supplies the
/// missing whole-tree ancestry or sibling closure required by input binding.
pub fn derive_window_root_binding_evidence(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    table_scope: &RowTableScope,
) -> Result<BindingDerivationEvidence, String> {
    snapshot.validate()?;
    table_scope.validate()?;
    if snapshot.source_sequence == 0 {
        return Err("window-root binding requires a positive Source sequence".into());
    }
    if snapshot.menu_context.is_some()
        || snapshot.objects.iter().any(|object| {
            object.secure
                || matches!(
                    object.role.as_str(),
                    "AXMenu" | "AXMenuItem" | "AXMenuBar" | "AXSheet"
                )
        })
        || snapshot.windows.iter().any(|window| window.modal)
    {
        return Err("window-root binding rejects menu, modal or secure Source facts".into());
    }
    let source = snapshot
        .window_context
        .as_ref()
        .ok_or("window-root binding requires same-read Source window context")?;
    source.validate()?;
    if source.session_epoch != snapshot.session_epoch
        || source.verified_source_sequence != snapshot.source_sequence
        || source.verified_observed_ms != snapshot.observed_ms
        || source.process.pid != snapshot.app.pid
        || snapshot.global_focus.known_window_for_app(&snapshot.app)
            != Some(source.origin_window_id.as_str())
    {
        return Err("window-root binding requires exact current Source provenance".into());
    }

    let census = snapshot
        .table_scope_search
        .as_ref()
        .ok_or("window-root binding requires the frozen same-read table census")?;
    if &census.requested_scope != table_scope
        || census.process_id != snapshot.app.pid
        || census.window_id != source.origin_window_id
    {
        return Err(
            "window-root binding table census differs from its frozen scope or source".into(),
        );
    }
    census.validate_against_snapshot(snapshot)?;
    let _candidate = census
        .unique_candidate()
        .ok_or("window-root binding requires one complete unique frozen-scope table")?;

    let windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.window_id == source.origin_window_id)
        .collect();
    let roots: Vec<_> = snapshot
        .objects
        .iter()
        .filter(|object| object.window_id == source.origin_window_id && object.parent.is_none())
        .collect();
    if windows.len() != 1 || roots.len() != 1 {
        return Err("window-root binding requires one actual active window and root".into());
    }
    let window = windows[0];
    let root = roots[0];
    if window.kind != LayerKind::Window
        || window.modal
        || window.owner_bundle_id != snapshot.app.bundle_id
        || window.frame != source.origin_window_frame
        || root.role != "AXWindow"
        || root.subrole.as_deref() != Some("AXStandardWindow")
        || root.frame != window.frame
        || root.title != window.title
        || !root.in_view
        || root.secure
        || root.group.is_some()
        || root.scroll.is_some()
        || target != root
    {
        return Err(
            "window-root binding target differs from the actual standard window root".into(),
        );
    }

    let scope = BindingProofScope::WindowRoot {
        table_scope: table_scope.clone(),
    };
    let fingerprint = instance_fingerprint(snapshot, root);
    let device_context = DeviceContextWitness::WindowRootV1 {
        app: Box::new(DeviceContextApp {
            bundle_id: snapshot.app.bundle_id.clone(),
            pid: snapshot.app.pid,
        }),
        window: Box::new(DeviceContextWindow {
            window_id: window.window_id.clone(),
            title: window.title.as_deref().map(normalize_label),
            frame: window.frame,
            kind: window.kind,
            modal: window.modal,
            owner_bundle_id: window.owner_bundle_id.clone(),
        }),
        root: device_context_object(root),
        source: Box::new(source.clone()),
    };
    device_context.validate()?;
    let context_digest = window_root_context_digest(
        &root.instance_ref,
        root.identity_continuity,
        &scope,
        &fingerprint,
        root.frame,
        &device_context,
    )?;
    let continuity = BindingContinuityProof {
        kind: root.identity_continuity,
        source_sequence: snapshot.source_sequence,
        context_digest,
        scope,
    };
    continuity.validate()?;
    Ok(BindingDerivationEvidence {
        continuity,
        device_context,
    })
}

/// Produces binding evidence for an explicitly focused-control scope.
/// Ordinary evidence deliberately excludes focus, which is a changing effect
/// fact rather than part of stable object identity.
pub fn derive_focused_binding_evidence(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    scope: BindingProofScope,
) -> Result<BindingDerivationEvidence, String> {
    if !matches!(&scope, BindingProofScope::WholeStructure) {
        return Err("focused-control evidence requires a whole-structure scope".into());
    }
    let derived = compile_binding_context(snapshot, target, scope)?;
    let mut device_context = derived.device_context?;
    let focus = device_context_focus_relation(snapshot, target)?;
    let DeviceContextWitness::WholeStructureV1 {
        focus: witness_focus,
        ..
    } = &mut device_context
    else {
        return Err("focused-control evidence requires a supported whole-structure witness".into());
    };
    *witness_focus = Some(Box::new(focus));
    device_context.validate()?;
    Ok(BindingDerivationEvidence {
        continuity: derived.continuity,
        device_context,
    })
}

fn device_context_focus_relation(
    snapshot: &StructureSnapshot,
    target: &UiObject,
) -> Result<DeviceContextFocusRelation, String> {
    let context = snapshot
        .focus_context
        .as_ref()
        .filter(|context| context.target_instance == target.instance_ref)
        .ok_or("focused-control evidence requires current Source focus for this exact target")?;
    if context.target_fingerprint != instance_fingerprint(snapshot, target)
        || context.target_frame != target.frame
        || context.focused_instance != target.instance_ref
    {
        return Err("focused-control evidence differs from the current target facts".into());
    }
    Ok(DeviceContextFocusRelation {
        origin_window_id: context.origin_window_id.clone(),
        origin_window_frame: context.origin_window_frame,
        target_fingerprint: DeviceContextFingerprint::from_instance(&context.target_fingerprint),
        target_frame: context.target_frame,
        relation: context.relation,
        composition: device_context_composition(&context.composition),
    })
}

fn compile_binding_context(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    scope: BindingProofScope,
) -> Result<CompiledBindingDerivation, String> {
    compile_binding_context_with_row_selection(snapshot, target, scope, None)
}

fn compile_binding_context_with_row_selection(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    scope: BindingProofScope,
    row_selection: Option<RowSelectionCompileInput<'_>>,
) -> Result<CompiledBindingDerivation, String> {
    snapshot.validate()?;
    if snapshot.source_sequence == 0 {
        return Err("binding continuity requires a positive Source sequence".into());
    }
    match &scope {
        BindingProofScope::WholeStructure if !snapshot.complete => {
            return Err(
                "whole-structure binding continuity requires complete Source traversal".into(),
            );
        }
        BindingProofScope::WholeStructure => {}
        BindingProofScope::TableViewport { .. } => scope.validate()?,
        BindingProofScope::WindowRoot { .. } => {
            return Err("window-root evidence must use derive_window_root_binding_evidence".into());
        }
        BindingProofScope::BackgroundAx { .. } => {
            return Err(
                "background AX evidence must use derive_background_ax_binding_evidence".into(),
            );
        }
    }

    if let Some(row_input) = row_selection {
        row_input.selector.validate()?;
        row_input.observation.validate_against_snapshot(
            row_input.selector,
            snapshot,
            row_input.now_ms,
            row_input.max_age_ms,
        )?;
        if row_input.observation.selected != Some(false)
            || row_input.observation.ax_selected_settable != Some(true)
            || !row_input
                .observation
                .table_ax_enabled
                .permits_row_selection()
            || !row_input.observation.row_ax_enabled.permits_row_selection()
            || row_input.observation.row_instance_ref != target.instance_ref
        {
            return Err(
                "row selection input does not describe this unselected, settable target".into(),
            );
        }
    }

    let target = snapshot
        .objects
        .iter()
        .find(|candidate| candidate.instance_ref == target.instance_ref)
        .filter(|candidate| *candidate == target)
        .ok_or("binding target must be the exact object from this Source snapshot")?;
    let objects: std::collections::BTreeMap<_, _> = snapshot
        .objects
        .iter()
        .map(|object| (object.instance_ref.as_str(), object))
        .collect();

    let mut ancestry = Vec::new();
    let mut seen = std::collections::BTreeSet::from([target.instance_ref.as_str()]);
    let mut parent_ref = target.parent.as_ref();
    while let Some(reference) = parent_ref {
        if ancestry.len() >= 32 || !seen.insert(reference.as_str()) {
            return Err("binding target ancestry is cyclic or exceeds 32 levels".into());
        }
        let parent = objects
            .get(reference.as_str())
            .copied()
            .ok_or("binding target ancestry is incomplete in this Source snapshot")?;
        if parent.window_id != target.window_id {
            return Err("binding target ancestry crosses a Source window boundary".into());
        }
        parent_ref = parent.parent.as_ref();
        ancestry.push(parent);
    }
    ancestry.reverse();
    let window_root = ancestry
        .first()
        .filter(|root| root.parent.is_none())
        .copied()
        .or_else(|| {
            (target.parent.is_none()
                && matches!(target.role.as_str(), "AXWindow" | "AXSheet" | "AXMenu"))
            .then_some(target)
        })
        .ok_or("binding target ancestry must reach its actual AXWindow root")?;
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.window_id == target.window_id)
        .ok_or("binding target window is absent from Source")?;
    if window_root.frame != window.frame || window.owner_bundle_id != snapshot.app.bundle_id {
        return Err("binding target ancestry root does not match its actual Source window".into());
    }
    match window_root.role.as_str() {
        "AXWindow" if window.kind == LayerKind::Window && !window.modal => {}
        "AXSheet" if window.kind == LayerKind::Sheet && window.modal => {}
        "AXMenu"
            if window.kind == LayerKind::Menu
                && snapshot
                    .menu_context
                    .as_ref()
                    .is_some_and(|context| context.menu_instance == window_root.instance_ref) => {}
        _ => {
            return Err(
                "binding ancestry root is not a supported actual window, sheet, or anchored menu"
                    .into(),
            );
        }
    }

    let direct_siblings: Vec<_> = snapshot
        .objects
        .iter()
        .filter(|candidate| {
            candidate.parent == target.parent && candidate.window_id == target.window_id
        })
        .collect();
    if !direct_siblings
        .iter()
        .any(|candidate| candidate.instance_ref == target.instance_ref)
    {
        return Err("binding target is missing from its actual ordered sibling set".into());
    }

    let nearest_labelled_group = ancestry.iter().rev().find(|ancestor| {
        is_group_role(&ancestor.role)
            && ancestor
                .title
                .as_deref()
                .or(ancestor.description.as_deref())
                .is_some_and(|label| !normalize_label(label).is_empty())
    });
    let target_group = target.group.as_deref().map(normalize_label);
    let actual_group = nearest_labelled_group.and_then(|group| {
        group
            .title
            .as_deref()
            .or(group.description.as_deref())
            .map(normalize_label)
    });
    let unresolved_group = target_group.is_some() && target_group != actual_group;

    let selected_viewport = select_table_viewport(snapshot, target, &ancestry, &scope)?;
    let (table_viewport, viewport_refs, unresolved_table_relation) = match selected_viewport {
        Some(viewport) => {
            let (mut projection, refs, unresolved) =
                table_viewport_closure(viewport, target, &objects, &ancestry, &scope)?;
            if row_selection.is_some()
                && let Some(target_row) = &mut projection.target_row
            {
                // Row selection commits every selector value across the full
                // visible range below; unrelated cells and app-private row
                // identifiers are not part of this action's identity closure.
                target_row.accessibility_identifier = None;
                target_row.cells.clear();
            }
            (Some(projection), refs, unresolved)
        }
        None => (None, Vec::new(), false),
    };
    let target_is_menu_related = matches!(target.role.as_str(), "AXMenu" | "AXMenuItem")
        || ancestry
            .iter()
            .any(|object| object.role == "AXMenu" || object.role == "AXMenuItem")
        || snapshot
            .menu_anchors
            .iter()
            .any(|anchor| anchor.opener_instance == target.instance_ref)
        || snapshot
            .menu_context
            .as_ref()
            .is_some_and(|context| context.opener.opener_instance == target.instance_ref);
    let menu_context = if target_is_menu_related {
        snapshot.menu_context.as_ref().filter(|context| {
            target.instance_ref == context.menu_instance
                || target.instance_ref == context.opener.opener_instance
                || ancestry
                    .iter()
                    .any(|object| object.instance_ref == context.menu_instance)
        })
    } else {
        None
    };
    if matches!(target.role.as_str(), "AXMenu" | "AXMenuItem") && menu_context.is_none() {
        return Err("menu target requires its current actual menu and opener context".into());
    }
    let menu_anchors: Vec<_> = snapshot
        .menu_anchors
        .iter()
        .filter(|anchor| {
            anchor.opener_instance == target.instance_ref
                || menu_context.is_some_and(|context| anchor.anchor_id == context.opener.anchor_id)
        })
        .collect();

    let row_selection_data = if let Some(row_input) = row_selection {
        if target_is_menu_related
            || menu_context.is_some()
            || !menu_anchors.is_empty()
            || snapshot
                .focus_context
                .as_ref()
                .is_some_and(|context| context.target_instance == target.instance_ref)
        {
            return Err("row selection cannot include menu or focus relations".into());
        }
        let viewport =
            selected_viewport.ok_or("row selection requires its exact local table viewport")?;
        if target.role != "AXRow"
            || viewport.table_instance_ref != row_input.selector.table_instance_ref
            || row_input.observation.table_instance_ref != viewport.table_instance_ref
            || row_input.observation.viewport_frame != viewport.viewport_frame
        {
            return Err("row selection target or table differs from its actual viewport".into());
        }
        Some(build_row_selection_context(
            snapshot, target, window, &ancestry, viewport, row_input,
        )?)
    } else {
        None
    };

    let mut closure_objects: Vec<&UiObject> = Vec::new();
    closure_objects.push(target);
    closure_objects.extend(ancestry.iter().copied());
    closure_objects.extend(direct_siblings.iter().copied());
    closure_objects.extend(
        viewport_refs
            .iter()
            .filter_map(|reference| objects.get(reference.as_str()).copied()),
    );
    closure_objects.sort_by(|a, b| a.instance_ref.as_str().cmp(b.instance_ref.as_str()));
    closure_objects.dedup_by(|a, b| a.instance_ref == b.instance_ref);

    let relevant_failures = snapshot
        .observer_coverage
        .failures
        .iter()
        .filter(|failure| {
            closure_objects.iter().any(|object| {
                object.instance_ref == failure.instance_ref
                    && (failure.notification
                        == super::source::ObserverNotification::UiElementDestroyed
                        || (matches!(object.role.as_str(), "AXWindow" | "AXSheet")
                            && matches!(
                                failure.notification,
                                super::source::ObserverNotification::Moved
                                    | super::source::ObserverNotification::Resized
                            )))
            })
        })
        .count();
    let kind = if unresolved_group
        || unresolved_table_relation
        || relevant_failures > 0
        || closure_objects
            .iter()
            .any(|object| object.identity_continuity == ObjectIdentityContinuity::ReadScoped)
        || viewport_refs
            .iter()
            .any(|reference| !objects.contains_key(reference.as_str()))
    {
        ObjectIdentityContinuity::ReadScoped
    } else {
        ObjectIdentityContinuity::NotificationCovered
    };

    let mut scroll_ancestors = ancestry
        .iter()
        .filter(|ancestor| ancestor.role == "AXScrollArea")
        .map(|ancestor| closure_node(ancestor))
        .collect::<Vec<_>>();
    if let Some(viewport) = selected_viewport
        && let Some(scroll) = objects.get(viewport.scroll_container_instance_ref.as_str())
        && !scroll_ancestors
            .iter()
            .any(|entry| entry.instance_ref == &scroll.instance_ref)
    {
        scroll_ancestors.push(closure_node(scroll));
    }
    let context = BindingContextClosure {
        version: 1,
        app_bundle_id: &snapshot.app.bundle_id,
        app_pid: snapshot.app.pid,
        identity_boundary: &snapshot.identity_boundary,
        global_focus: &snapshot.global_focus,
        target_context: &snapshot.target_context,
        window,
        menu_anchors: menu_anchors.clone(),
        menu_context,
        scope: &scope,
        fingerprint: instance_fingerprint(snapshot, target),
        target: closure_node(target),
        ancestry: ancestry.iter().map(|node| closure_node(node)).collect(),
        siblings: direct_siblings
            .iter()
            .map(|sibling| SiblingNode {
                instance_ref: &sibling.instance_ref,
                parent: sibling.parent.as_ref(),
                window_id: &sibling.window_id,
                role: &sibling.role,
                subrole: &sibling.subrole,
                label: normalize_label(
                    sibling
                        .title
                        .as_deref()
                        .or(sibling.description.as_deref())
                        .unwrap_or(""),
                ),
                frame: sibling.frame,
            })
            .collect(),
        nearest_labelled_group: nearest_labelled_group.map(|group| closure_node(group)),
        scroll_ancestors,
        table_viewport,
        row_selection: row_selection_data
            .as_ref()
            .map(|row_data| row_data.closure.clone()),
    };
    let canonical = serde_json::to_vec(&context)
        .map_err(|_| "binding context closure could not be canonicalized")?;
    let digest = Sha256::digest(&canonical);
    let mut context_digest = String::from("bc1:");
    for byte in digest {
        context_digest.push_str(&format!("{byte:02x}"));
    }
    let device_context = if let Some(row_data) = row_selection_data {
        Ok(row_data.witness)
    } else {
        match &scope {
            BindingProofScope::TableViewport { .. } => Ok(DeviceContextWitness::Unsupported {
                reason: DeviceContextUnsupportedReason::TableViewport,
            }),
            BindingProofScope::WindowRoot { .. } => {
                Err("window-root evidence cannot be compiled from a generic target closure".into())
            }
            BindingProofScope::BackgroundAx { .. } => Err(
                "background AX evidence cannot be compiled from a generic target closure".into(),
            ),
            BindingProofScope::WholeStructure => {
                if let Some(viewport) = selected_viewport {
                    if unresolved_group {
                        Ok(DeviceContextWitness::Unsupported {
                            reason: DeviceContextUnsupportedReason::TableViewport,
                        })
                    } else if let Some(table_column) = sortable_table_column_relation(
                        snapshot,
                        target,
                        viewport,
                        &ancestry,
                        target_is_menu_related,
                        menu_context,
                        &menu_anchors,
                    ) {
                        build_whole_structure_witness(WholeStructureWitnessInput {
                            snapshot,
                            target,
                            window,
                            ancestry: &ancestry,
                            direct_siblings: &direct_siblings,
                            nearest_labelled_group: nearest_labelled_group.copied(),
                            target_is_menu_related,
                            table_column: Some(table_column),
                            menu_context,
                            menu_anchors: &menu_anchors,
                        })
                    } else {
                        Ok(DeviceContextWitness::Unsupported {
                            reason: DeviceContextUnsupportedReason::TableViewport,
                        })
                    }
                } else if unresolved_group {
                    Err("Device context witness requires every target relation to resolve".into())
                } else if unresolved_table_relation
                    || is_table_related_role(&target.role)
                    || (target.role == "AXButton"
                        && target.subrole.as_deref() == Some("AXSortButton"))
                    || ancestry
                        .iter()
                        .any(|object| is_table_related_role(&object.role))
                {
                    Ok(DeviceContextWitness::Unsupported {
                        reason: DeviceContextUnsupportedReason::TableViewport,
                    })
                } else {
                    build_whole_structure_witness(WholeStructureWitnessInput {
                        snapshot,
                        target,
                        window,
                        ancestry: &ancestry,
                        direct_siblings: &direct_siblings,
                        nearest_labelled_group: nearest_labelled_group.copied(),
                        target_is_menu_related,
                        table_column: None,
                        menu_context,
                        menu_anchors: &menu_anchors,
                    })
                }
            }
        }
    };
    let proof = BindingContinuityProof {
        kind,
        source_sequence: snapshot.source_sequence,
        context_digest,
        scope: scope.clone(),
    };
    proof.validate()?;
    Ok(CompiledBindingDerivation {
        continuity: proof,
        device_context,
    })
}

fn build_row_selection_context(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    window: &WindowInfo,
    ancestry: &[&UiObject],
    viewport: &TableViewport,
    input: RowSelectionCompileInput<'_>,
) -> Result<RowSelectionCompiledContext, String> {
    let selector = input.selector;
    let observation = input.observation;
    if !viewport.visible_rows_complete
        || !viewport.columns_complete
        || viewport.visible_rows.is_empty()
        || viewport.columns.is_empty()
        || !device_context_point_valid(input.hit_point)
    {
        return Err("row selection requires a complete bounded local table viewport".into());
    }
    if target.instance_ref != observation.row_instance_ref
        || target.role != "AXRow"
        || target.window_id != window.window_id
        || target.frame != observation.row_frame
    {
        return Err("row selection target does not match its actual Source AXRow".into());
    }

    let objects: std::collections::BTreeMap<_, _> = snapshot
        .objects
        .iter()
        .map(|object| (object.instance_ref.as_str(), object))
        .collect();
    let table = objects
        .get(viewport.table_instance_ref.as_str())
        .copied()
        .ok_or("row selection table is absent from actual Source objects")?;
    if table.role != "AXTable"
        || table.window_id != window.window_id
        || table.frame != viewport.table_frame
    {
        return Err("row selection viewport does not resolve to its actual AXTable".into());
    }
    let table_ancestry: Vec<_> = ancestry
        .iter()
        .enumerate()
        .filter(|(_, object)| object.instance_ref == viewport.table_instance_ref)
        .collect();
    if table_ancestry.len() != 1 {
        return Err("row selection table must appear exactly once in the target ancestry".into());
    }
    let table_ancestry_index = table_ancestry[0].0;
    let scroll_path_objects: Vec<_> = ancestry[..table_ancestry_index]
        .iter()
        .copied()
        .filter(|object| object.role == "AXScrollArea")
        .collect();
    if scroll_path_objects.is_empty()
        || scroll_path_objects.len() > 8
        || scroll_path_objects
            .last()
            .is_none_or(|object| object.instance_ref != viewport.scroll_container_instance_ref)
    {
        return Err(
            "row selection needs the actual bounded outer-to-inner AXScrollArea path".into(),
        );
    }
    let scroll_path: Vec<_> = scroll_path_objects
        .iter()
        .map(|object| DeviceContextObject {
            role: object.role.clone(),
            subrole: object.subrole.clone(),
            // Row scope matching uses the exact public AXDescription spelling.
            label: object.description.clone().unwrap_or_default(),
            frame: object.frame,
        })
        .collect();

    let mut ordered_columns = Vec::with_capacity(viewport.columns.len());
    let mut column_by_header = std::collections::BTreeMap::new();
    let mut column_by_index = std::collections::BTreeMap::new();
    let mut context_refs = std::collections::BTreeSet::from([
        viewport.table_instance_ref.as_str(),
        viewport.scroll_container_instance_ref.as_str(),
    ]);
    for column in &viewport.columns {
        let index = column
            .ax_index
            .filter(|index| *index <= 63)
            .and_then(|index| u8::try_from(index).ok())
            .ok_or("row selector requires every actual AXColumn index to be known in 0..63")?;
        let header = column
            .header
            .as_deref()
            .ok_or("row selector requires every actual AXColumn public header")?;
        validate_row_context_text("actual AXColumn header", header, 256)?;
        let column_ref = column
            .instance_ref
            .as_ref()
            .ok_or("row selector requires every actual AXColumn reference for Source closure")?;
        let header_ref = column
            .header_instance_ref
            .as_ref()
            .ok_or("row selector requires every actual AXColumn header reference")?;
        if column.parent_instance_ref != viewport.table_instance_ref
            || !context_refs.insert(column_ref.as_str())
            || !context_refs.insert(header_ref.as_str())
            || column_by_header.contains_key(header)
            || column_by_index.contains_key(&index)
        {
            return Err(
                "row selector table columns have ambiguous refs, parents, headers, or indices"
                    .into(),
            );
        }
        let actual_column = objects
            .get(column_ref.as_str())
            .copied()
            .ok_or("row selector AXColumn is absent from actual Source objects")?;
        let actual_header = objects
            .get(header_ref.as_str())
            .copied()
            .ok_or("row selector AXColumn header is absent from actual Source objects")?;
        let actual_header_label = actual_header
            .title
            .as_deref()
            .or(actual_header.description.as_deref())
            .or_else(|| {
                actual_header.value.as_ref().and_then(|value| match value {
                    UiValue::Text(text) => Some(text.as_str()),
                    _ => None,
                })
            });
        if actual_column.role != "AXColumn"
            || actual_column.parent.as_ref() != Some(&viewport.table_instance_ref)
            || actual_column.window_id != window.window_id
            || actual_column.frame != column.frame
            || !matches!(actual_header.role.as_str(), "AXStaticText" | "AXButton")
            || actual_header.window_id != window.window_id
            || actual_header_label != Some(header)
        {
            return Err(
                "row selector header and column must match their actual Source AX objects".into(),
            );
        }
        ordered_columns.push(DeviceContextTableColumn {
            ax_index: index,
            header: header.to_owned(),
            frame: column.frame,
        });
        column_by_header.insert(header.to_owned(), (index, column, header_ref));
        column_by_index.insert(index, (column_ref, header_ref, header));
    }

    let mut selector_columns = Vec::with_capacity(selector.conditions.len());
    let mut selector_source_columns = Vec::with_capacity(selector.conditions.len());
    for condition in &selector.conditions {
        let (index, column, header_ref) = column_by_header
            .get(&condition.column_name)
            .copied()
            .ok_or("row selector condition does not name one exact actual AXColumn header")?;
        selector_columns.push(DeviceContextRowSelectorColumn {
            ax_index: index,
            header: condition.column_name.clone(),
            expected_value: condition.value.clone(),
        });
        selector_source_columns.push((index, column, header_ref, &condition.value));
    }

    let mut visible_rows = Vec::with_capacity(viewport.visible_rows.len());
    let mut closure_rows = Vec::with_capacity(viewport.visible_rows.len());
    let mut row_indices = std::collections::BTreeSet::new();
    let mut matching_row_refs = Vec::new();
    let mut target_row_ax_index = None;
    let mut target_row_projection = None;
    for row in &viewport.visible_rows {
        let row_ax_index = row
            .ax_index
            .ok_or("row selection requires every visible AXRow global AXIndex")?;
        if !row_indices.insert(row_ax_index) {
            return Err("row selection viewport contains duplicate actual AXRow indices".into());
        }
        let row_ref = row
            .instance_ref
            .as_ref()
            .ok_or("row selection requires every visible AXRow Source reference")?;
        if !context_refs.insert(row_ref.as_str()) {
            return Err(
                "row selection viewport contains a duplicate actual Source row reference".into(),
            );
        }
        let actual_row = objects
            .get(row_ref.as_str())
            .copied()
            .ok_or("visible AXRow is absent from actual Source objects")?;
        if actual_row.role != "AXRow"
            || actual_row.parent.as_ref() != Some(&viewport.table_instance_ref)
            || actual_row.window_id != window.window_id
            || actual_row.frame != row.frame
        {
            return Err("visible row projection does not match its actual AXRow".into());
        }

        let mut values = Vec::with_capacity(selector_source_columns.len());
        let mut closure_values = Vec::with_capacity(selector_source_columns.len());
        let mut matches_selector = true;
        for (column_index, column, header_ref, expected) in &selector_source_columns {
            let column_ref = column
                .instance_ref
                .as_ref()
                .ok_or("selector AXColumn is missing its Source reference")?;
            let matching_cells: Vec<_> = row
                .cells
                .iter()
                .filter(|cell| {
                    cell.column_ax_index == Some(u32::from(*column_index))
                        || cell.column_instance_ref.as_ref() == Some(column_ref)
                        || cell.column_header.as_deref()
                            == Some(column.header.as_deref().unwrap_or(""))
                })
                .collect();
            if matching_cells.len() != 1 {
                return Err("each visible row must have exactly one selector cell with its actual AXColumn index".into());
            }
            let cell = matching_cells[0];
            let cell_ref = cell
                .instance_ref
                .as_ref()
                .ok_or("selector cell is missing its actual Source reference")?;
            let actual_cell = objects
                .get(cell_ref.as_str())
                .copied()
                .ok_or("selector cell is absent from actual Source objects")?;
            let value = cell
                .value
                .as_ref()
                .ok_or("selector cell value is unknown for a visible row")?;
            if cell.row_ax_index != Some(row_ax_index)
                || cell.column_header.as_deref() != Some(column.header.as_deref().unwrap_or(""))
                || cell.header_instance_ref.as_ref() != Some(*header_ref)
                || cell.parent_instance_ref.as_ref() != Some(row_ref)
                || !context_refs.insert(cell_ref.as_str())
                || actual_cell.role != "AXCell"
                || actual_cell.parent.as_ref() != Some(row_ref)
                || actual_cell.window_id != window.window_id
                || actual_cell.frame != cell.frame
            {
                return Err(
                    "selector cell AXIndex, header, parent, or Source relation is inconsistent"
                        .into(),
                );
            }
            validate_row_context_value(value)?;
            if value != *expected {
                matches_selector = false;
            }
            values.push(value.clone());
            closure_values.push(value.clone());
        }
        if matches_selector {
            matching_row_refs.push(row_ref.clone());
        }
        if row_ref == &observation.row_instance_ref {
            target_row_ax_index = Some(row_ax_index);
            target_row_projection = Some(row);
        }
        visible_rows.push(DeviceContextVisibleRow {
            ax_index: row_ax_index,
            frame: row.frame,
            selector_values: values,
        });
        closure_rows.push(RowSelectionVisibleRowClosure {
            ax_index: row_ax_index,
            frame: row.frame,
            selector_values: closure_values,
        });
    }
    let target_row_ax_index =
        target_row_ax_index.ok_or("unique selector row is absent from actual visible AXRows")?;
    let target_row_projection =
        target_row_projection.ok_or("unique selector row has no actual visible row projection")?;
    if matching_row_refs.len() != 1
        || matching_row_refs.first() != Some(&observation.row_instance_ref)
        || !viewport.ax_enabled.permits_row_selection()
        || !target_row_projection.ax_enabled.permits_row_selection()
        || target_row_projection.selected != Some(false)
        || target_row_projection.ax_selected_settable != Some(true)
        || observation.table_ax_enabled != viewport.ax_enabled
        || observation.row_ax_enabled != target_row_projection.ax_enabled
        || observation.selected != target_row_projection.selected
        || observation.ax_selected_settable != target_row_projection.ax_selected_settable
        || !target_row_projection
            .frame
            .contains(input.hit_point.x, input.hit_point.y)
        || !viewport
            .viewport_frame
            .contains(input.hit_point.x, input.hit_point.y)
    {
        return Err(
            "row selector must uniquely identify the current unselected, settable row and point"
                .into(),
        );
    }

    let root = ancestry
        .first()
        .filter(|root| root.parent.is_none())
        .copied()
        .ok_or("row selection ancestry must reach its actual window root")?;
    let table_descriptor = DeviceContextObject {
        role: table.role.clone(),
        subrole: table.subrole.clone(),
        label: table.description.clone().unwrap_or_default(),
        frame: table.frame,
    };
    let ordered_column_closure = ordered_columns
        .iter()
        .map(|column| RowSelectionColumnClosure {
            ax_index: column.ax_index,
            header: column.header.clone(),
            frame: column.frame,
        })
        .collect();
    let witness = DeviceContextWitness::TableViewportV1 {
        app: Box::new(DeviceContextApp {
            bundle_id: snapshot.app.bundle_id.clone(),
            pid: snapshot.app.pid,
        }),
        window: Box::new(DeviceContextWindow {
            window_id: window.window_id.clone(),
            title: root.title.as_deref().map(normalize_label),
            frame: window.frame,
            kind: window.kind,
            modal: window.modal,
            owner_bundle_id: window.owner_bundle_id.clone(),
        }),
        table: table_descriptor.clone(),
        target: Box::new(DeviceContextTarget {
            object: device_context_object(target),
            group: target.group.as_deref().map(normalize_label),
            scroll: target.scroll.clone(),
        }),
        scroll_path: scroll_path.clone(),
        viewport_frame: viewport.viewport_frame,
        ordered_columns,
        selector_columns: selector_columns.clone(),
        visible_rows,
        target_row_ax_index,
        table_ax_enabled: viewport.ax_enabled,
        target_ax_enabled: target_row_projection.ax_enabled,
        target_selected: false,
        target_ax_selected_settable: true,
        hit_point: input.hit_point,
    };
    let closure = RowSelectionContextClosure {
        table: table_descriptor,
        table_ax_enabled: viewport.ax_enabled,
        target_ax_enabled: target_row_projection.ax_enabled,
        scroll_path,
        viewport_frame: viewport.viewport_frame,
        ordered_columns: ordered_column_closure,
        selector_columns: selector_columns
            .into_iter()
            .map(|column| RowSelectionConditionClosure {
                ax_index: column.ax_index,
                header: column.header,
                expected_value: column.expected_value,
            })
            .collect(),
        visible_rows: closure_rows,
        target_row_ax_index,
        target_selected: false,
        target_ax_selected_settable: true,
        hit_point: input.hit_point,
    };
    Ok(RowSelectionCompiledContext { closure, witness })
}

fn sortable_table_column_relation(
    snapshot: &StructureSnapshot,
    target: &UiObject,
    viewport: &TableViewport,
    ancestry: &[&UiObject],
    target_is_menu_related: bool,
    menu_context: Option<&MenuContext>,
    menu_anchors: &[&MenuOpenerAnchor],
) -> Option<DeviceContextTableColumnRelation> {
    if !snapshot.complete
        || !viewport.columns_complete
        || viewport.columns.is_empty()
        || ancestry.iter().any(|node| node.role == "AXOutline")
        || target_is_menu_related
        || menu_context.is_some()
        || !menu_anchors.is_empty()
        || snapshot
            .focus_context
            .as_ref()
            .is_some_and(|context| context.target_instance == target.instance_ref)
        || target.role != "AXButton"
        || target.subrole.as_deref() != Some("AXSortButton")
        || !target.enabled
        || !target.actions.contains(&AxActionName::Press)
    {
        return None;
    }

    let table_ancestry: Vec<_> = ancestry
        .iter()
        .enumerate()
        .filter(|(_, object)| object.role == "AXTable")
        .collect();
    if table_ancestry.len() != 1 {
        return None;
    }
    let (table_ancestry_index, table) = table_ancestry[0];
    if table.instance_ref != viewport.table_instance_ref
        || table_ancestry_index > 31
        || target.window_id != table.window_id
    {
        return None;
    }

    let objects: std::collections::BTreeMap<_, _> = snapshot
        .objects
        .iter()
        .map(|object| (object.instance_ref.as_str(), object))
        .collect();
    let mut seen_refs = std::collections::BTreeSet::new();
    let mut seen_indices = std::collections::BTreeSet::new();
    let mut ordered_ax_indices = Vec::with_capacity(viewport.columns.len());
    let mut target_matches = 0usize;

    for column in &viewport.columns {
        let column_ref = column.instance_ref.as_ref()?;
        let header_ref = column.header_instance_ref.as_ref()?;
        if !seen_refs.insert(column_ref.as_str())
            || !seen_refs.insert(header_ref.as_str())
            || column.parent_instance_ref != viewport.table_instance_ref
        {
            return None;
        }
        let column_object = objects.get(column_ref.as_str()).copied()?;
        let header_object = objects.get(header_ref.as_str()).copied()?;
        if column_object.role != "AXColumn"
            || column_object.parent.as_ref() != Some(&viewport.table_instance_ref)
            || column_object.window_id != table.window_id
            || column_object.window_id != target.window_id
            || column_object.frame != column.frame
            || header_object.window_id != table.window_id
        {
            return None;
        }
        let ax_index = column.ax_index?;
        if ax_index > 63 || !seen_indices.insert(ax_index) {
            return None;
        }
        ordered_ax_indices.push(u8::try_from(ax_index).ok()?);
        if header_ref == &target.instance_ref {
            target_matches += 1;
        }
    }

    if target_matches != 1 {
        return None;
    }
    let target_column = viewport.columns.iter().find(|column| {
        column
            .header_instance_ref
            .as_ref()
            .is_some_and(|header| header == &target.instance_ref)
    })?;
    let target_ax_index = u8::try_from(target_column.ax_index?).ok()?;
    Some(DeviceContextTableColumnRelation {
        table_ancestry_index: u8::try_from(table_ancestry_index).ok()?,
        ordered_ax_indices,
        target_ax_index,
    })
}

fn build_whole_structure_witness(
    WholeStructureWitnessInput {
        snapshot,
        target,
        window,
        ancestry,
        direct_siblings,
        nearest_labelled_group,
        target_is_menu_related,
        table_column,
        menu_context,
        menu_anchors,
    }: WholeStructureWitnessInput<'_>,
) -> Result<DeviceContextWitness, String> {
    let root = ancestry.first().copied().unwrap_or(target);
    let target_sibling_index = direct_siblings
        .iter()
        .position(|sibling| sibling.instance_ref == target.instance_ref)
        .ok_or("binding target is missing from its actual ordered sibling set")?;
    if direct_siblings.len() > MAX_DEVICE_CONTEXT_SIBLINGS {
        return Err("Device context direct sibling set exceeds 256 objects".into());
    }
    let sibling_parent_ancestry_index = if ancestry.is_empty() {
        None
    } else {
        Some(
            u8::try_from(ancestry.len() - 1)
                .map_err(|_| "Device context ancestry exceeds 32 objects")?,
        )
    };
    let nearest_labelled_group_ancestry_index = nearest_labelled_group
        .map(|group| {
            ancestry
                .iter()
                .position(|ancestor| ancestor.instance_ref == group.instance_ref)
                .ok_or("nearest labelled group is not in actual target ancestry")
                .and_then(|index| {
                    u8::try_from(index).map_err(|_| "nearest labelled group exceeds ancestry bound")
                })
        })
        .transpose()?;
    let scroll_ancestors = ancestry
        .iter()
        .enumerate()
        .filter(|(_, ancestor)| ancestor.role == "AXScrollArea")
        .map(|(index, ancestor)| {
            Ok(DeviceContextScrollAncestor {
                ancestry_index: u8::try_from(index)
                    .map_err(|_| "scroll ancestor exceeds ancestry bound")?,
                state: ancestor.scroll.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let menu = if let Some(context) = menu_context {
        Some(DeviceContextMenuRelation::AnchoredMenu {
            origin_window_id: context.opener.origin_window_id.clone(),
            origin_window_frame: context.opener.origin_window_frame,
            opener_fingerprint: Box::new(DeviceContextFingerprint::from_instance(
                &context.opener.opener_fingerprint,
            )),
            continuity: context.opener.continuity,
            menu_fingerprint: Box::new(DeviceContextFingerprint::from_instance(
                &context.menu_fingerprint,
            )),
            menu_frame: context.menu_frame,
            drawing_window: DeviceContextDrawingWindow {
                window_id: context.drawing_window.window_id,
                owner_pid: context.drawing_window.owner_pid,
                layer: context.drawing_window.layer,
                frame: context.drawing_window.frame,
            },
        })
    } else if menu_anchors.len() == 1 {
        let anchor = menu_anchors[0];
        Some(DeviceContextMenuRelation::OpenerAnchor {
            origin_window_id: anchor.origin_window_id.clone(),
            origin_window_frame: anchor.origin_window_frame,
            opener_fingerprint: Box::new(DeviceContextFingerprint::from_instance(
                &anchor.opener_fingerprint,
            )),
            continuity: anchor.continuity,
        })
    } else if target_is_menu_related || !menu_anchors.is_empty() {
        return Err("Device context menu relation is missing or ambiguous".into());
    } else {
        None
    };
    let witness = DeviceContextWitness::WholeStructureV1 {
        app: Box::new(DeviceContextApp {
            bundle_id: snapshot.app.bundle_id.clone(),
            pid: snapshot.app.pid,
        }),
        active_layer: snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .map(str::to_owned),
        window: Box::new(DeviceContextWindow {
            window_id: window.window_id.clone(),
            title: root.title.as_deref().map(normalize_label),
            frame: window.frame,
            kind: window.kind,
            modal: window.modal,
            owner_bundle_id: window.owner_bundle_id.clone(),
        }),
        target: Box::new(DeviceContextTarget {
            object: device_context_object(target),
            group: target.group.as_deref().map(normalize_label),
            scroll: target.scroll.clone(),
        }),
        ancestry: ancestry
            .iter()
            .map(|object| device_context_object(object))
            .collect(),
        sibling_parent_ancestry_index,
        siblings: direct_siblings
            .iter()
            .map(|object| device_context_object(object))
            .collect(),
        target_sibling_index: u16::try_from(target_sibling_index)
            .map_err(|_| "Device context target sibling index exceeds 255")?,
        nearest_labelled_group_ancestry_index,
        scroll_ancestors,
        table_column: table_column.map(Box::new),
        menu: menu.map(Box::new),
        focus: None,
    };
    witness.validate()?;
    Ok(witness)
}

fn device_context_object(object: &UiObject) -> DeviceContextObject {
    DeviceContextObject {
        role: object.role.clone(),
        subrole: object.subrole.clone(),
        label: normalize_label(
            object
                .title
                .as_deref()
                .or(object.description.as_deref())
                .unwrap_or(""),
        ),
        frame: object.frame,
    }
}

fn device_context_composition(composition: &CompositionState) -> DeviceContextComposition {
    match composition {
        CompositionState::Unknown => DeviceContextComposition::Unknown,
        CompositionState::NotApplicable => DeviceContextComposition::NotApplicable,
        CompositionState::Marked { .. } => DeviceContextComposition::Marked,
        CompositionState::Unmarked { .. } => DeviceContextComposition::Unmarked,
    }
}

fn is_group_role(role: &str) -> bool {
    role == "AXGroup" || role.ends_with("Group")
}

fn closure_node(object: &UiObject) -> ClosureNode<'_> {
    ClosureNode {
        instance_ref: &object.instance_ref,
        parent: object.parent.as_ref(),
        window_id: &object.window_id,
        role: &object.role,
        subrole: &object.subrole,
        label: normalize_label(
            object
                .title
                .as_deref()
                .or(object.description.as_deref())
                .unwrap_or(""),
        ),
        group: object.group.as_deref().map(normalize_label),
        frame: object.frame,
        scroll: object.scroll.as_ref(),
    }
}

fn select_table_viewport<'a>(
    snapshot: &'a StructureSnapshot,
    target: &UiObject,
    ancestry: &[&'a UiObject],
    scope: &BindingProofScope,
) -> Result<Option<&'a TableViewport>, String> {
    let explicit = match scope {
        BindingProofScope::WholeStructure => None,
        BindingProofScope::WindowRoot { .. } => {
            return Err("window-root scope has no generic target table viewport".into());
        }
        BindingProofScope::BackgroundAx { .. } => {
            return Err("background AX scope has no generic target table viewport".into());
        }
        BindingProofScope::TableViewport {
            table_instance_ref,
            scroll_container_instance_ref,
            viewport_frame,
            visible_rows_complete,
            columns_complete,
        } => {
            scope.validate()?;
            let matching: Vec<_> = snapshot
                .table_viewports
                .iter()
                .filter(|viewport| {
                    &viewport.table_instance_ref == table_instance_ref
                        && &viewport.scroll_container_instance_ref == scroll_container_instance_ref
                        && viewport.viewport_frame == *viewport_frame
                        && viewport.visible_rows_complete == *visible_rows_complete
                        && viewport.columns_complete == *columns_complete
                })
                .collect();
            if matching.len() != 1 {
                return Err(
                    "table viewport scope must identify one exact actual Source projection".into(),
                );
            }
            Some(matching[0])
        }
    };

    if let Some(viewport) = explicit {
        return Ok(Some(viewport));
    }

    let object_refs: std::collections::BTreeSet<_> = std::iter::once(target.instance_ref.as_str())
        .chain(ancestry.iter().map(|object| object.instance_ref.as_str()))
        .collect();
    let related: Vec<_> = snapshot
        .table_viewports
        .iter()
        .filter(|viewport| {
            object_refs.contains(viewport.table_instance_ref.as_str())
                || object_refs.contains(viewport.scroll_container_instance_ref.as_str())
                || viewport_mentions_ref(viewport, target.instance_ref.as_str())
        })
        .collect();
    if related.len() > 1 {
        return Err("target relates to more than one table viewport".into());
    }
    Ok(related.first().copied())
}

fn viewport_mentions_ref(viewport: &TableViewport, reference: &str) -> bool {
    viewport.visible_rows.iter().any(|row| {
        row.instance_ref
            .as_ref()
            .is_some_and(|item| item.as_str() == reference)
            || row.cells.iter().any(|cell| {
                cell.instance_ref
                    .as_ref()
                    .is_some_and(|item| item.as_str() == reference)
            })
    }) || viewport.columns.iter().any(|column| {
        column
            .instance_ref
            .as_ref()
            .is_some_and(|item| item.as_str() == reference)
            || column
                .header_instance_ref
                .as_ref()
                .is_some_and(|item| item.as_str() == reference)
    })
}

fn table_viewport_closure<'a>(
    viewport: &'a TableViewport,
    target: &UiObject,
    objects: &std::collections::BTreeMap<&str, &UiObject>,
    ancestry: &[&UiObject],
    scope: &BindingProofScope,
) -> Result<(TableViewportClosure<'a>, Vec<ObjectRef>, bool), String> {
    let _table = objects
        .get(viewport.table_instance_ref.as_str())
        .ok_or("table viewport table object is absent from Source")?;
    let _scroll = objects
        .get(viewport.scroll_container_instance_ref.as_str())
        .ok_or("table viewport scroll object is absent from Source")?;
    let target_ref = target.instance_ref.as_str();
    let target_ancestors: std::collections::BTreeSet<_> = ancestry
        .iter()
        .map(|object| object.instance_ref.as_str())
        .collect();
    let table_or_scroll_in_ancestry = target_ancestors
        .contains(viewport.table_instance_ref.as_str())
        || target_ancestors.contains(viewport.scroll_container_instance_ref.as_str());
    let mut target_rows: Vec<(usize, &TableViewportRow)> = viewport
        .visible_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.instance_ref
                .as_ref()
                .is_some_and(|reference| reference.as_str() == target_ref)
        })
        .collect();
    let mut target_columns: Vec<(usize, &TableViewportColumn)> = viewport
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| {
            column
                .instance_ref
                .as_ref()
                .is_some_and(|reference| reference.as_str() == target_ref)
                || column
                    .header_instance_ref
                    .as_ref()
                    .is_some_and(|reference| reference.as_str() == target_ref)
                || column
                    .instance_ref
                    .as_ref()
                    .is_some_and(|reference| target_ancestors.contains(reference.as_str()))
                || column
                    .header_instance_ref
                    .as_ref()
                    .is_some_and(|reference| target_ancestors.contains(reference.as_str()))
        })
        .collect();
    let mut target_cells: Vec<(usize, usize, &TableViewportCell)> = viewport
        .visible_rows
        .iter()
        .enumerate()
        .flat_map(|(row_index, row)| {
            row.cells
                .iter()
                .enumerate()
                .filter_map(move |(cell_index, cell)| {
                    cell.instance_ref
                        .as_ref()
                        .is_some_and(|reference| reference.as_str() == target_ref)
                        .then_some((row_index, cell_index, cell))
                })
        })
        .collect();

    let exact_target_relations = target_rows.len() + target_columns.len() + target_cells.len();
    let is_table = target_ref == viewport.table_instance_ref.as_str();
    let is_scroll = target_ref == viewport.scroll_container_instance_ref.as_str();
    if matches!(scope, BindingProofScope::TableViewport { .. })
        && exact_target_relations != 1
        && !is_table
        && !is_scroll
    {
        return Err("table viewport binding target needs one exact actual row, column, cell, or header relation".into());
    }
    if exact_target_relations > 1 && matches!(scope, BindingProofScope::TableViewport { .. }) {
        return Err("table viewport target has ambiguous row/column/cell relations".into());
    }
    if matches!(scope, BindingProofScope::TableViewport { .. })
        && (target_rows.iter().any(|(_, row)| !row.cells_complete)
            || target_cells
                .iter()
                .any(|(row_index, _, _)| !viewport.visible_rows[*row_index].cells_complete))
    {
        return Err(
            "table viewport target requires complete actual cell evidence for its row".into(),
        );
    }
    if exact_target_relations == 0 && !is_table && !is_scroll && !table_or_scroll_in_ancestry {
        return Err("target is not in the actual table/scroll context".into());
    }

    let target_row = (target_rows.len() == 1)
        .then(|| target_rows.pop().map(|(_, row)| row))
        .flatten()
        .map(|row| TableRowClosure {
            instance_ref: &row.instance_ref,
            parent_instance_ref: &row.parent_instance_ref,
            frame: row.frame,
            accessibility_identifier: row.accessibility_identifier.as_deref(),
            ax_index: &row.ax_index,
            ax_enabled: row.ax_enabled,
            cells_complete: row.cells_complete,
            cells: row.cells.iter().map(table_cell_closure).collect(),
        });
    let target_column = (target_columns.len() == 1)
        .then(|| target_columns.pop().map(|(_, column)| column))
        .flatten()
        .map(|column| TableColumnClosure {
            instance_ref: &column.instance_ref,
            parent_instance_ref: &column.parent_instance_ref,
            frame: column.frame,
            header_instance_ref: &column.header_instance_ref,
            header: column.header.as_deref().map(normalize_label),
            ax_index: &column.ax_index,
        });
    let target_cell = (target_cells.len() == 1)
        .then(|| target_cells.pop().map(|(_, _, cell)| cell))
        .flatten()
        .map(table_cell_closure);

    let mut relevant_refs = vec![
        viewport.table_instance_ref.clone(),
        viewport.scroll_container_instance_ref.clone(),
    ];
    relevant_refs.extend(
        viewport
            .visible_rows
            .iter()
            .filter_map(|row| row.instance_ref.clone()),
    );
    relevant_refs.extend(
        viewport
            .columns
            .iter()
            .filter_map(|column| column.instance_ref.clone()),
    );
    relevant_refs.extend(
        viewport
            .columns
            .iter()
            .filter_map(|column| column.header_instance_ref.clone()),
    );
    relevant_refs.extend(viewport.visible_rows.iter().flat_map(|row| {
        row.cells.iter().flat_map(|cell| {
            [
                cell.instance_ref.clone(),
                cell.column_instance_ref.clone(),
                cell.header_instance_ref.clone(),
            ]
            .into_iter()
            .flatten()
        })
    }));
    relevant_refs.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    relevant_refs.dedup_by(|a, b| a == b);

    let unresolved = viewport
        .visible_rows
        .iter()
        .any(|row| row.instance_ref.is_none())
        || viewport
            .columns
            .iter()
            .any(|column| column.instance_ref.is_none() || column.header_instance_ref.is_none())
        || viewport
            .visible_rows
            .iter()
            .any(|row| row.cells.iter().any(|cell| cell.instance_ref.is_none()))
        || relevant_refs
            .iter()
            .any(|reference| !objects.contains_key(reference.as_str()))
        || exact_target_relations > 1
        || (exact_target_relations == 0 && !is_table && !is_scroll);

    Ok((
        TableViewportClosure {
            table_instance_ref: &viewport.table_instance_ref,
            scroll_container_instance_ref: &viewport.scroll_container_instance_ref,
            process_id: viewport.process_id,
            ax_enabled: viewport.ax_enabled,
            table_frame: viewport.table_frame,
            viewport_frame: viewport.viewport_frame,
            visible_rows_complete: viewport.visible_rows_complete,
            columns_complete: viewport.columns_complete,
            visible_row_order: viewport
                .visible_rows
                .iter()
                .map(|row| row.instance_ref.as_ref())
                .collect(),
            visible_row_ax_indices: viewport
                .visible_rows
                .iter()
                .map(|row| row.ax_index)
                .collect(),
            column_order: viewport
                .columns
                .iter()
                .map(|column| column.instance_ref.as_ref())
                .collect(),
            ordered_ax_indices: viewport
                .columns
                .iter()
                .map(|column| column.ax_index)
                .collect(),
            target_row,
            target_column,
            target_cell,
        },
        relevant_refs,
        unresolved,
    ))
}

fn table_cell_closure(cell: &TableViewportCell) -> TableCellClosure<'_> {
    TableCellClosure {
        instance_ref: &cell.instance_ref,
        parent_instance_ref: &cell.parent_instance_ref,
        frame: cell.frame,
        row_ax_index: &cell.row_ax_index,
        column_ax_index: &cell.column_ax_index,
        column_instance_ref: &cell.column_instance_ref,
        header_instance_ref: &cell.header_instance_ref,
        column_header: cell.column_header.as_deref().map(normalize_label),
        value: &cell.value,
    }
}

/// How a task describes the object it wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetDescription {
    #[schemars(length(min = 1))]
    pub label: String,
    pub group: Option<String>,
    pub role_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindScope {
    pub app: String,
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindRequest {
    pub request_id: RequestId,
    pub semantic_target: SemanticTargetRef,
    pub description: TargetDescription,
    pub scope: BindScope,
    /// The pick Reaction made among candidates of an earlier ambiguous answer.
    pub choice: Option<ObjectRef>,
    /// Oldest evidence the caller accepts.
    pub max_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub snapshot_id: RequestId,
    pub environment_revision: u64,
    pub observed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindCandidate {
    pub instance: ObjectRef,
    pub role: String,
    pub label: String,
    pub group: Option<String>,
    pub score_milli: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingOutcome {
    Bound {
        binding: BindingRef,
        instance: ObjectRef,
        fingerprint_digest: String,
        evidence: EvidenceRef,
        active_layer_revision: u64,
        rule: String,
    },
    /// Several instances qualify. Reaction observes further or asks S1.
    Ambiguous {
        candidates: Vec<BindCandidate>,
    },
    /// Nothing qualified. `observation_complete` separates "the scope was
    /// fully observed and it is not there" from "we could not see everything".
    NotFound {
        observation_complete: bool,
    },
    Rejected {
        code: FailureCode,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingResult {
    pub request_id: RequestId,
    pub outcome: BindingOutcome,
    pub environment_revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::domain::ExecutionDomain;

    #[test]
    fn native_binding_golden_matches_rust_normalization_rounding_and_digest() {
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/desktop-v1/native-binding/golden.json"
        ))
        .unwrap();
        for entry in golden["fingerprints"].as_array().unwrap() {
            let proof: InstanceFingerprint =
                serde_json::from_value(entry["fingerprint"].clone()).unwrap();
            assert_eq!(
                serde_json::to_string(&proof).unwrap(),
                entry["canonical"].as_str().unwrap()
            );
            assert_eq!(proof.digest(), entry["digest"].as_str().unwrap());
        }
        for entry in golden["labels"].as_array().unwrap() {
            assert_eq!(
                normalize_label(entry["raw"].as_str().unwrap()),
                entry["normalized"].as_str().unwrap()
            );
        }
        for entry in golden["rounding"].as_array().unwrap() {
            let actual: Vec<_> = entry["raw"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap().round() as i64)
                .collect();
            let expected: Vec<_> = entry["rounded"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
            assert_eq!(actual, expected);
        }
    }

    fn fingerprint() -> InstanceFingerprint {
        InstanceFingerprint {
            app: "test.app".into(),
            app_pid: 42,
            window_id: "w1".into(),
            ancestor_roles: vec!["window".into(), "group".into()],
            group: Some("push".into()),
            role: "switch".into(),
            subrole: None,
            label: "daily picks".into(),
            sibling_index: 2,
            frame: [10, 20, 40, 20],
            scroll_container: None,
        }
    }

    #[test]
    fn digest_is_stable_and_sensitive_to_every_field() {
        let base = fingerprint();
        assert_eq!(base.digest(), base.clone().digest());
        assert!(base.digest().starts_with("fp1:"));
        assert_eq!(base.digest().len(), 4 + 64);

        let mut moved = base.clone();
        moved.frame = [10, 21, 40, 20];
        assert_ne!(
            base.digest(),
            moved.digest(),
            "a moved element is a different proof"
        );
        let mut relabelled = base.clone();
        relabelled.label = "weekly picks".into();
        assert_ne!(base.digest(), relabelled.digest());
        let mut restarted = base.clone();
        restarted.app_pid += 1;
        assert_ne!(base.digest(), restarted.digest());
        let mut regrouped = base.clone();
        regrouped.ancestor_roles = vec!["window".into()];
        assert_ne!(base.digest(), regrouped.digest());
    }

    fn object(reference: &str, parent: Option<&str>, role: &str, title: &str) -> UiObject {
        use super::super::source::Rect;
        UiObject {
            instance_ref: ObjectRef::new(reference).unwrap(),
            parent: parent.map(|p| ObjectRef::new(p).unwrap()),
            window_id: "w1".into(),
            role: role.into(),
            subrole: None,
            title: Some(title.into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: Rect {
                x: 10.4,
                y: 20.6,
                width: 40.0,
                height: 20.0,
            },
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        }
    }

    fn snapshot(objects: Vec<UiObject>) -> StructureSnapshot {
        use super::super::device::SurfaceRef;
        use super::super::source::{
            AppIdentity, GlobalFocusObservation, LayerKind, ObservationFact, ObserverCoverage,
            Rect, SourceState, WindowInfo,
        };
        StructureSnapshot {
            focus_context: None,
            window_context: None,
            menu_anchors: vec![],
            menu_context: None,
            snapshot_id: RequestId::new("s").unwrap(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            identity_boundary: None,
            frame_ref: None,
            owned_document_evidence: None,
            source_sequence: 1,
            observed_ms: 1,
            app: AppIdentity {
                bundle_id: "test.app".into(),
                name: "T".into(),
                pid: 1,
            },
            windows: vec![WindowInfo {
                window_id: "w1".into(),
                title: None,
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
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
                        name: "T".into(),
                        pid: 1,
                    },
                },
                window_id: ObservationFact::Known { value: "w1".into() },
            },
            target_context: None,
            complete: true,
            incomplete_reason: None,
            observer_coverage: ObserverCoverage {
                state: SourceState::Available,
                covered_objects: objects.len() as u32,
                read_scoped_objects: 0,
                failed_registrations: 0,
                failures: vec![],
            },
            objects,
            table_scope_search: None,
            table_viewports: vec![],
        }
    }

    fn identity_snapshot() -> StructureSnapshot {
        let mut root = object("window-root", None, "AXWindow", "Root");
        root.subrole = Some("AXStandardWindow".into());
        root.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let mut group = object("group", Some("window-root"), "AXGroup", "Push");
        group.group = None;
        let mut target = object("target", Some("group"), "AXSwitch", "Daily picks");
        target.group = Some("Push".into());
        let sibling = object(
            "sibling",
            Some("group"),
            "AXStaticText",
            "Unrelated heading",
        );
        snapshot(vec![root, group, target, sibling])
    }

    fn table_column_snapshot(ax_index: Option<u32>) -> StructureSnapshot {
        use super::super::source::{TableViewport, TableViewportColumn};

        let window_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let mut root = object("window-root", None, "AXWindow", "Root");
        root.frame = window_frame;
        root.subrole = Some("AXStandardWindow".into());
        let mut scroll = object("scroll", Some("window-root"), "AXScrollArea", "Scroll");
        scroll.frame = window_frame;
        let mut table = object("table", Some("scroll"), "AXTable", "Table");
        table.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 90.0,
            height: 80.0,
        };
        let mut column = object("column", Some("table"), "AXColumn", "Name");
        column.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 80.0,
        };
        let mut header = object("header", Some("column"), "AXStaticText", "Name");
        header.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 20.0,
        };
        let table_frame = table.frame;
        let column_frame = column.frame;
        let viewport_frame = scroll.frame;
        let mut result = snapshot(vec![root, scroll, table, column, header]);
        result.complete = false;
        result.incomplete_reason = Some("tree read was bounded to the table viewport".into());
        let process_id = result.app.pid;
        result.table_viewports.push(TableViewport {
            table_instance_ref: ObjectRef::new("table").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll").unwrap(),
            process_id,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame,
            visible_rows_complete: true,
            visible_rows: vec![],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(ObjectRef::new("column").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: column_frame,
                header_instance_ref: Some(ObjectRef::new("header").unwrap()),
                header: Some("Name".into()),
                ax_index,
            }],
        });
        update_identity_coverage(&mut result);
        result
    }

    fn sortable_table_snapshot() -> StructureSnapshot {
        use super::super::source::{AxActionName, TableViewport, TableViewportColumn};

        let window_frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 200.0,
        };
        let mut root = object("window-root", None, "AXWindow", "Root");
        root.frame = window_frame;
        root.subrole = Some("AXStandardWindow".into());
        let mut scroll = object("scroll", Some("window-root"), "AXScrollArea", "Scroll");
        scroll.frame = window_frame;
        let mut table = object("table", Some("scroll"), "AXTable", "Table");
        table.frame = Rect {
            x: 10.0,
            y: 20.0,
            width: 280.0,
            height: 150.0,
        };
        let mut first_column = object("column-a", Some("table"), "AXColumn", "Name");
        first_column.frame = Rect {
            x: 10.0,
            y: 20.0,
            width: 140.0,
            height: 150.0,
        };
        let mut first_header = object("header-a", Some("column-a"), "AXButton", "Name");
        first_header.subrole = Some("AXSortButton".into());
        first_header.actions = vec![AxActionName::Press];
        first_header.frame = Rect {
            x: 10.0,
            y: 20.0,
            width: 140.0,
            height: 24.0,
        };
        let mut second_column = object("column-b", Some("table"), "AXColumn", "Status");
        second_column.frame = Rect {
            x: 150.0,
            y: 20.0,
            width: 140.0,
            height: 150.0,
        };
        let mut second_header = object("header-b", Some("column-b"), "AXStaticText", "Status");
        second_header.frame = Rect {
            x: 150.0,
            y: 20.0,
            width: 140.0,
            height: 24.0,
        };

        let mut result = snapshot(vec![
            root,
            scroll,
            table,
            first_column,
            first_header,
            second_column,
            second_header,
        ]);
        result.windows[0].frame = window_frame;
        let process_id = result.app.pid;
        let table_frame = result.objects[2].frame;
        let viewport_frame = result.objects[1].frame;
        result.table_viewports.push(TableViewport {
            table_instance_ref: ObjectRef::new("table").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll").unwrap(),
            process_id,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame,
            visible_rows_complete: false,
            visible_rows: vec![],
            columns_complete: true,
            columns: vec![
                TableViewportColumn {
                    instance_ref: Some(ObjectRef::new("column-a").unwrap()),
                    parent_instance_ref: ObjectRef::new("table").unwrap(),
                    frame: result.objects[3].frame,
                    header_instance_ref: Some(ObjectRef::new("header-a").unwrap()),
                    header: Some("Name".into()),
                    ax_index: Some(2),
                },
                TableViewportColumn {
                    instance_ref: Some(ObjectRef::new("column-b").unwrap()),
                    parent_instance_ref: ObjectRef::new("table").unwrap(),
                    frame: result.objects[5].frame,
                    header_instance_ref: Some(ObjectRef::new("header-b").unwrap()),
                    header: Some("Status".into()),
                    ax_index: Some(7),
                },
            ],
        });
        update_identity_coverage(&mut result);
        result
    }

    fn indexed_row_table_snapshot(
        row_ax_index: Option<u32>,
        cell_row_ax_index: Option<u32>,
        column_ax_index: Option<u32>,
        cell_column_ax_index: Option<u32>,
    ) -> StructureSnapshot {
        use super::super::source::{TableViewportCell, TableViewportRow, UiValue};

        let mut result = sortable_table_snapshot();
        let row_frame = Rect {
            x: 12.0,
            y: 48.0,
            width: 276.0,
            height: 22.0,
        };
        let cell_frame = Rect {
            x: 12.0,
            y: 48.0,
            width: 136.0,
            height: 22.0,
        };
        let second_row_frame = Rect {
            x: 12.0,
            y: 74.0,
            width: 276.0,
            height: 22.0,
        };
        let mut row_object = object("row", Some("table"), "AXRow", "Row");
        row_object.frame = row_frame;
        let mut second_row_object = object("row-b", Some("table"), "AXRow", "Row B");
        second_row_object.frame = second_row_frame;
        let mut cell_object = object("cell", Some("row"), "AXCell", "Cell");
        cell_object.frame = cell_frame;
        result
            .objects
            .extend([row_object, second_row_object, cell_object]);
        let viewport = &mut result.table_viewports[0];
        viewport.visible_rows_complete = true;
        viewport.columns[0].ax_index = column_ax_index;
        viewport.visible_rows = vec![
            TableViewportRow {
                instance_ref: Some(ObjectRef::new("row").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: row_ax_index,
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells: vec![TableViewportCell {
                    instance_ref: Some(ObjectRef::new("cell").unwrap()),
                    parent_instance_ref: Some(ObjectRef::new("row").unwrap()),
                    frame: cell_frame,
                    row_ax_index: cell_row_ax_index,
                    column_ax_index: cell_column_ax_index,
                    column_instance_ref: Some(ObjectRef::new("column-a").unwrap()),
                    header_instance_ref: Some(ObjectRef::new("header-a").unwrap()),
                    column_header: Some("Name".into()),
                    value: Some(UiValue::Text("Quarterly report".into())),
                }],
            },
            TableViewportRow {
                instance_ref: Some(ObjectRef::new("row-b").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: second_row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: Some(99),
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells: vec![],
            },
        ];
        update_identity_coverage(&mut result);
        result
    }

    fn row_selection_snapshot() -> StructureSnapshot {
        use super::super::source::{
            SourceIdentityBoundary, TableViewportCell, TableViewportRow, UiValue,
        };

        let mut result = sortable_table_snapshot();
        result.complete = false;
        result.incomplete_reason = Some("table viewport is locally complete".into());
        result.identity_boundary = Some(SourceIdentityBoundary {
            namespace: "00000000-0000-4000-8000-000000000001".into(),
            generation: 1,
        });
        result.objects[1].title = None;
        result.objects[1].description = Some("Projects".into());
        result.table_viewports[0].columns[0].ax_index = Some(2);
        result.table_viewports[0].columns[1].ax_index = Some(7);

        let first_frame = Rect {
            x: 12.0,
            y: 48.0,
            width: 276.0,
            height: 22.0,
        };
        let second_frame = Rect {
            x: 12.0,
            y: 74.0,
            width: 276.0,
            height: 22.0,
        };
        let first_cell_frame = Rect {
            x: 12.0,
            y: 48.0,
            width: 136.0,
            height: 22.0,
        };
        let second_cell_frame = Rect {
            x: 12.0,
            y: 74.0,
            width: 136.0,
            height: 22.0,
        };
        let first_status_frame = Rect {
            x: 150.0,
            y: 48.0,
            width: 138.0,
            height: 22.0,
        };
        let second_status_frame = Rect {
            x: 150.0,
            y: 74.0,
            width: 138.0,
            height: 22.0,
        };
        let mut first_row = object("row", Some("table"), "AXRow", "Acme row");
        first_row.frame = first_frame;
        let mut second_row = object("row-b", Some("table"), "AXRow", "Other row");
        second_row.frame = second_frame;
        let mut first_cell = object("cell-a", Some("row"), "AXCell", "Acme");
        first_cell.frame = first_cell_frame;
        let mut second_cell = object("cell-b", Some("row-b"), "AXCell", "Other");
        second_cell.frame = second_cell_frame;
        let mut first_status = object("cell-a-status", Some("row"), "AXCell", "Active");
        first_status.frame = first_status_frame;
        let mut second_status = object("cell-b-status", Some("row-b"), "AXCell", "Inactive");
        second_status.frame = second_status_frame;
        result.objects.extend([
            first_row,
            second_row,
            first_cell,
            second_cell,
            first_status,
            second_status,
        ]);

        result.table_viewports[0].visible_rows_complete = true;
        result.table_viewports[0].visible_rows = vec![
            TableViewportRow {
                instance_ref: Some(ObjectRef::new("row").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: first_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: Some(42),
                selected: Some(false),
                ax_selected_settable: Some(true),
                cells_complete: true,
                cells: vec![
                    TableViewportCell {
                        instance_ref: Some(ObjectRef::new("cell-a").unwrap()),
                        parent_instance_ref: Some(ObjectRef::new("row").unwrap()),
                        frame: first_cell_frame,
                        row_ax_index: Some(42),
                        column_ax_index: Some(2),
                        column_instance_ref: Some(ObjectRef::new("column-a").unwrap()),
                        header_instance_ref: Some(ObjectRef::new("header-a").unwrap()),
                        column_header: Some("Name".into()),
                        value: Some(UiValue::Text("Acme".into())),
                    },
                    TableViewportCell {
                        instance_ref: Some(ObjectRef::new("cell-a-status").unwrap()),
                        parent_instance_ref: Some(ObjectRef::new("row").unwrap()),
                        frame: first_status_frame,
                        row_ax_index: Some(42),
                        column_ax_index: Some(7),
                        column_instance_ref: Some(ObjectRef::new("column-b").unwrap()),
                        header_instance_ref: Some(ObjectRef::new("header-b").unwrap()),
                        column_header: Some("Status".into()),
                        value: Some(UiValue::Text("Active".into())),
                    },
                ],
            },
            TableViewportRow {
                instance_ref: Some(ObjectRef::new("row-b").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: second_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: Some(99),
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells: vec![
                    TableViewportCell {
                        instance_ref: Some(ObjectRef::new("cell-b").unwrap()),
                        parent_instance_ref: Some(ObjectRef::new("row-b").unwrap()),
                        frame: second_cell_frame,
                        row_ax_index: Some(99),
                        column_ax_index: Some(2),
                        column_instance_ref: Some(ObjectRef::new("column-a").unwrap()),
                        header_instance_ref: Some(ObjectRef::new("header-a").unwrap()),
                        column_header: Some("Name".into()),
                        value: Some(UiValue::Text("Other".into())),
                    },
                    TableViewportCell {
                        instance_ref: Some(ObjectRef::new("cell-b-status").unwrap()),
                        parent_instance_ref: Some(ObjectRef::new("row-b").unwrap()),
                        frame: second_status_frame,
                        row_ax_index: Some(99),
                        column_ax_index: Some(7),
                        column_instance_ref: Some(ObjectRef::new("column-b").unwrap()),
                        header_instance_ref: Some(ObjectRef::new("header-b").unwrap()),
                        column_header: Some("Status".into()),
                        value: Some(UiValue::Text("Inactive".into())),
                    },
                ],
            },
        ];
        for object in &mut result.objects {
            object.identity_continuity = ObjectIdentityContinuity::ReadScoped;
        }
        result.observer_coverage.state = super::super::source::SourceState::Unavailable;
        result.observer_coverage.covered_objects = 0;
        result.observer_coverage.read_scoped_objects = result.objects.len() as u32;
        result.observer_coverage.failed_registrations = 0;
        result.observer_coverage.failures.clear();
        result.validate().unwrap();
        result
    }

    fn row_selector() -> RowSelector {
        use super::super::row::RowColumnCondition;
        use super::super::source::UiValue;
        RowSelector {
            app_bundle_id: "test.app".into(),
            process_id: 1,
            window_id: "w1".into(),
            table_instance_ref: ObjectRef::new("table").unwrap(),
            conditions: vec![RowColumnCondition {
                column_name: "Name".into(),
                value: UiValue::Text("Acme".into()),
            }],
        }
    }

    fn row_selection_observation(
        snapshot: &StructureSnapshot,
        selector: &RowSelector,
    ) -> RowSelectionObservation {
        RowSelectionObservation::from_snapshot(selector, snapshot, 50, 2, 100).unwrap()
    }

    fn continuity_for(snapshot: &StructureSnapshot, target_ref: &str) -> BindingContinuityProof {
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == target_ref)
            .unwrap();
        derive_binding_continuity(snapshot, target, BindingProofScope::WholeStructure).unwrap()
    }

    fn update_identity_coverage(snapshot: &mut StructureSnapshot) {
        snapshot.observer_coverage.covered_objects = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.identity_continuity == ObjectIdentityContinuity::NotificationCovered
            })
            .count() as u32;
        snapshot.observer_coverage.read_scoped_objects =
            snapshot.objects.len() as u32 - snapshot.observer_coverage.covered_objects;
        snapshot.observer_coverage.state = if snapshot.observer_coverage.read_scoped_objects == 0
            && snapshot.observer_coverage.failed_registrations == 0
        {
            super::super::source::SourceState::Available
        } else {
            super::super::source::SourceState::Degraded
        };
    }

    fn proof_from_evidence(
        snapshot: &StructureSnapshot,
        target: &UiObject,
        evidence: BindingDerivationEvidence,
    ) -> BindingProof {
        BindingProof {
            binding: BindingRef::new("test-binding").unwrap(),
            instance: target.instance_ref.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            environment_revision: 1,
            layer_revision: 1,
            fingerprint: instance_fingerprint(snapshot, target),
            display_frame: target.frame,
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            menu_scope: None,
            focus_scope: None,
        }
    }

    /// The original replay predates GlobalFocus. The paired synthetic fixture
    /// adds a test-only focus fact and is not a live Source proof.
    fn scoped_window_fixture() -> (StructureSnapshot, RowTableScope, UiObject) {
        let snapshot: StructureSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json"
        )))
        .unwrap();
        let context = snapshot.window_context.as_ref().unwrap();
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.window_id == context.origin_window_id && object.parent.is_none())
            .unwrap()
            .clone();
        let scope = snapshot
            .table_scope_search
            .as_ref()
            .unwrap()
            .requested_scope
            .clone();
        (snapshot, scope, target)
    }

    #[test]
    fn device_context_and_continuity_share_the_exact_target_closure() {
        let mut before = identity_snapshot();
        before.objects[2].value = Some(super::super::source::UiValue::Bool(false));
        let target = &before.objects[2];
        let evidence =
            derive_binding_evidence(&before, target, BindingProofScope::WholeStructure).unwrap();
        assert_eq!(
            evidence.continuity,
            derive_binding_continuity(&before, target, BindingProofScope::WholeStructure).unwrap()
        );
        let DeviceContextWitness::WholeStructureV1 {
            app,
            active_layer,
            window,
            target: witness_target,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            scroll_ancestors,
            table_column,
            menu,
            focus,
        } = &evidence.device_context
        else {
            panic!("complete ordinary Source closure should have a whole witness");
        };
        assert_eq!(app.bundle_id, "test.app");
        assert_eq!(app.pid, 1);
        assert_eq!(active_layer.as_deref(), Some("w1"));
        assert_eq!(window.window_id, "w1");
        assert_eq!(witness_target.object.role, "AXSwitch");
        assert_eq!(witness_target.object.label, "daily picks");
        assert_eq!(witness_target.object.frame, target.frame);
        assert_eq!(witness_target.group.as_deref(), Some("push"));
        assert_eq!(
            ancestry
                .iter()
                .map(|node| node.role.as_str())
                .collect::<Vec<_>>(),
            vec!["AXWindow", "AXGroup"]
        );
        assert_eq!(*sibling_parent_ancestry_index, Some(1));
        assert_eq!(siblings.len(), 2);
        assert_eq!(usize::from(*target_sibling_index), 0);
        assert_eq!(*nearest_labelled_group_ancestry_index, Some(1));
        assert!(scroll_ancestors.is_empty());
        assert!(table_column.is_none());
        assert!(menu.is_none());
        assert!(focus.is_none());
        assert!(evidence.device_context.supports_native_input());
        let wire = serde_json::to_value(&evidence.device_context).unwrap();
        assert_eq!(wire["scope"], "whole_structure_v1");
        for field in [
            "app",
            "active_layer",
            "window",
            "target",
            "ancestry",
            "siblings",
        ] {
            assert!(
                wire.get(field).is_some(),
                "missing flat witness field {field}"
            );
        }
        fn contains_key(value: &serde_json::Value, expected: &str) -> bool {
            match value {
                serde_json::Value::Object(fields) => fields
                    .iter()
                    .any(|(key, value)| key == expected || contains_key(value, expected)),
                serde_json::Value::Array(values) => {
                    values.iter().any(|value| contains_key(value, expected))
                }
                _ => false,
            }
        }
        for source_only_field in [
            "instance_ref",
            "parent",
            "scroll_container",
            "identity_boundary",
            "source_session_id",
        ] {
            assert!(
                !contains_key(&wire, source_only_field),
                "{source_only_field}"
            );
        }
        let proof = proof_from_evidence(&before, target, evidence.clone());
        proof.validate().unwrap();

        before.objects[2].value = Some(super::super::source::UiValue::Bool(true));
        before.objects[2].focused = true;
        let after = derive_binding_evidence(
            &before,
            &before.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        assert_eq!(
            after.continuity.context_digest,
            evidence.continuity.context_digest
        );
        assert_eq!(after.device_context, evidence.device_context);
    }

    #[test]
    fn device_context_window_title_comes_from_actual_ax_root_title() {
        let mut ordinary = identity_snapshot();
        ordinary.windows[0].title = Some("Window metadata title".into());
        ordinary.objects[0].title = Some("AX Native Title".into());
        ordinary.objects[0].description = Some("AX Description".into());
        let ordinary_evidence = derive_binding_evidence(
            &ordinary,
            &ordinary.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        let DeviceContextWitness::WholeStructureV1 {
            window: ordinary_window,
            ..
        } = &ordinary_evidence.device_context
        else {
            panic!("ordinary complete Source closure should have a whole witness");
        };
        assert_eq!(ordinary_window.title.as_deref(), Some("ax native title"));

        let mut changed_metadata = ordinary.clone();
        changed_metadata.windows[0].title = Some("Different metadata title".into());
        let changed_evidence = derive_binding_evidence(
            &changed_metadata,
            &changed_metadata.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        let DeviceContextWitness::WholeStructureV1 {
            window: changed_window,
            ..
        } = &changed_evidence.device_context
        else {
            panic!("ordinary complete Source closure should have a whole witness");
        };
        assert_eq!(changed_window.title.as_deref(), Some("ax native title"));
        assert_ne!(
            ordinary_evidence.continuity.context_digest, changed_evidence.continuity.context_digest,
            "WindowInfo metadata remains committed by bc1"
        );

        let mut sheet = identity_snapshot();
        sheet.objects[0].role = "AXSheet".into();
        sheet.objects[0].subrole = None;
        sheet.objects[0].title = Some("AX Sheet Title".into());
        sheet.objects[0].description = Some("Sheet Description".into());
        sheet.windows[0].title = None;
        sheet.windows[0].kind = LayerKind::Sheet;
        sheet.windows[0].modal = true;
        let sheet_evidence =
            derive_binding_evidence(&sheet, &sheet.objects[2], BindingProofScope::WholeStructure)
                .unwrap();
        let DeviceContextWitness::WholeStructureV1 {
            window: sheet_window,
            ..
        } = &sheet_evidence.device_context
        else {
            panic!("actual AXSheet closure should have a whole witness");
        };
        assert_eq!(sheet_window.title.as_deref(), Some("ax sheet title"));
    }

    #[test]
    fn binding_proofs_require_a_device_context_witness() {
        let snapshot = identity_snapshot();
        let target = &snapshot.objects[2];
        let evidence =
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).unwrap();
        let proof = proof_from_evidence(&snapshot, target, evidence);
        proof.validate().unwrap();

        let mut wire = serde_json::to_value(proof).unwrap();
        wire.as_object_mut().unwrap().remove("device_context");
        assert!(serde_json::from_value::<BindingProof>(wire).is_err());
    }

    #[test]
    fn whole_witness_preserves_binding_to_an_inactive_window() {
        let mut snapshot = identity_snapshot();
        let mut inactive = object("other-window", None, "AXWindow", "Other window");
        inactive.window_id = "w2".into();
        inactive.frame = Rect {
            x: 200.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        snapshot.objects.push(inactive);
        snapshot.windows.push(WindowInfo {
            window_id: "w2".into(),
            title: Some("Other window".into()),
            frame: snapshot.objects.last().unwrap().frame,
            z_index: 1,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: snapshot.app.bundle_id.clone(),
        });
        snapshot.global_focus.window_id =
            super::super::source::ObservationFact::Known { value: "w2".into() };
        update_identity_coverage(&mut snapshot);
        snapshot.validate().unwrap();

        let target = &snapshot.objects[2];
        let evidence =
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).unwrap();
        let DeviceContextWitness::WholeStructureV1 {
            active_layer,
            window,
            ..
        } = &evidence.device_context
        else {
            panic!("inactive ordinary window should remain supported");
        };
        assert_eq!(window.window_id, "w1");
        assert_eq!(active_layer.as_deref(), Some("w2"));
        proof_from_evidence(&snapshot, target, evidence)
            .validate()
            .unwrap();
    }

    #[test]
    fn menu_opener_witness_preserves_the_actual_anchor_without_source_refs() {
        use super::super::source::{KernelProcessIdentity, MenuAnchorContinuity, MenuOpenerAnchor};

        let mut root = object("window-root", None, "AXWindow", "Main");
        root.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        root.subrole = Some("AXStandardWindow".into());
        let mut opener = object(
            "opaque-source-opener-ref",
            Some("window-root"),
            "AXMenuButton",
            "Fixture menu",
        );
        opener.frame = Rect {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        opener.actions = vec![super::super::source::AxActionName::ShowMenu];
        let mut snapshot = snapshot(vec![root, opener]);
        let anchor = MenuOpenerAnchor {
            anchor_id: "anchor-1".into(),
            source_session_id: "watch-1".into(),
            anchor_revision: 1,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 10,
                start_micros: 1,
            },
            origin_window_id: "w1".into(),
            origin_window_frame: snapshot.windows[0].frame,
            opener_instance: snapshot.objects[1].instance_ref.clone(),
            opener_fingerprint: instance_fingerprint(&snapshot, &snapshot.objects[1]),
            opener_frame: snapshot.objects[1].frame,
            verified_source_sequence: snapshot.source_sequence,
            verified_observed_ms: snapshot.observed_ms,
            continuity: MenuAnchorContinuity::DirectRead,
        };
        snapshot.menu_anchors.push(anchor.clone());
        snapshot.validate().unwrap();

        let target = &snapshot.objects[1];
        let evidence =
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).unwrap();
        assert!(matches!(
            &evidence.device_context,
            DeviceContextWitness::WholeStructureV1 {
                menu: Some(menu),
                ..
            } if matches!(menu.as_ref(), DeviceContextMenuRelation::OpenerAnchor { .. })
        ));
        let wire = serde_json::to_string(&evidence.device_context).unwrap();
        assert!(!wire.contains("opaque-source-opener-ref"));
        let mut proof = proof_from_evidence(&snapshot, target, evidence);
        proof.menu_scope = Some(MenuBindingScope::Opener {
            anchor,
            attempt_deadline_ms: 10,
        });
        proof.validate().unwrap();
    }

    #[test]
    fn focused_witness_requires_the_actual_target_relationship() {
        use super::super::source::{
            FocusContext, FocusRelation, KernelProcessIdentity, SourceKeyboardContext,
        };

        let mut snapshot = identity_snapshot();
        snapshot.objects[0].subrole = Some("AXStandardWindow".into());
        snapshot.objects[2].instance_ref = ObjectRef::new("private-focused-ref").unwrap();
        snapshot.objects[2].focused = true;
        let target = &snapshot.objects[2];
        let context = FocusContext {
            source_session_id: "focus-source-1".into(),
            session_epoch: snapshot.session_epoch,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 10,
                start_micros: 1,
            },
            origin_window_id: "w1".into(),
            origin_window_frame: snapshot.windows[0].frame,
            target_instance: target.instance_ref.clone(),
            target_fingerprint: instance_fingerprint(&snapshot, target),
            target_frame: target.frame,
            focused_instance: target.instance_ref.clone(),
            relation: FocusRelation::ExactControl,
            composition: CompositionState::NotApplicable,
            keyboard: SourceKeyboardContext {
                selected_input_source: None,
                underlying_keyboard_layout: None,
                keyboard_type: None,
                caps_lock: None,
            },
            verified_source_sequence: snapshot.source_sequence,
            verified_observed_ms: snapshot.observed_ms,
        };
        snapshot.focus_context = Some(context.clone());
        snapshot.validate().unwrap();

        let evidence =
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).unwrap();
        assert!(matches!(
            &evidence.device_context,
            DeviceContextWitness::WholeStructureV1 { focus: None, .. }
        ));
        let focused_evidence =
            derive_focused_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure)
                .unwrap();
        assert_eq!(focused_evidence.continuity, evidence.continuity);
        let DeviceContextWitness::WholeStructureV1 {
            focus: Some(focus), ..
        } = &focused_evidence.device_context
        else {
            panic!("explicit focused derivation should include actual focus facts");
        };
        assert_eq!(focus.relation, FocusRelation::ExactControl);
        assert_eq!(focus.composition, DeviceContextComposition::NotApplicable);
        let wire = serde_json::to_string(&evidence.device_context).unwrap();
        assert!(!wire.contains("private-focused-ref"));
        let mut proof = proof_from_evidence(&snapshot, target, focused_evidence);
        proof.focus_scope = Some(FocusedControlScope {
            context,
            profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
            key: LayoutIndependentSpecialKey::Space,
        });
        proof.validate().unwrap();

        let other_target = snapshot.objects[3].clone();
        assert!(
            derive_focused_binding_evidence(
                &snapshot,
                &other_target,
                BindingProofScope::WholeStructure,
            )
            .is_err()
        );
        let mut missing_focus = snapshot.clone();
        missing_focus.focus_context = None;
        let missing_target = missing_focus.objects[2].clone();
        assert!(
            derive_focused_binding_evidence(
                &missing_focus,
                &missing_target,
                BindingProofScope::WholeStructure,
            )
            .is_err()
        );
    }

    #[test]
    fn table_viewport_is_explicitly_unsupported_without_changing_source_scope() {
        use super::super::source::{TableViewport, TableViewportRow};

        let mut root = object("window-root", None, "AXWindow", "Root");
        root.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        root.subrole = Some("AXStandardWindow".into());
        let mut scroll = object("scroll", Some("window-root"), "AXScrollArea", "Scroll");
        scroll.frame = root.frame;
        let mut table = object("table", Some("scroll"), "AXTable", "Table");
        table.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 80.0,
        };
        let mut row = object("row", Some("table"), "AXRow", "Row");
        row.frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        };
        let mut snapshot = snapshot(vec![root, scroll, table, row]);
        let viewport = TableViewport {
            table_instance_ref: ObjectRef::new("table").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll").unwrap(),
            process_id: snapshot.app.pid,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame: snapshot.objects[2].frame,
            viewport_frame: snapshot.objects[1].frame,
            visible_rows_complete: true,
            visible_rows: vec![TableViewportRow {
                instance_ref: Some(ObjectRef::new("row").unwrap()),
                parent_instance_ref: ObjectRef::new("table").unwrap(),
                frame: snapshot.objects[3].frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: None,
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells: vec![],
            }],
            columns_complete: true,
            columns: vec![],
        };
        snapshot.table_viewports.push(viewport.clone());
        snapshot.validate().unwrap();
        let table_scope = BindingProofScope::TableViewport {
            table_instance_ref: viewport.table_instance_ref,
            scroll_container_instance_ref: viewport.scroll_container_instance_ref,
            viewport_frame: viewport.viewport_frame,
            visible_rows_complete: true,
            columns_complete: true,
        };
        let target = &snapshot.objects[3];
        let continuity = derive_binding_continuity(&snapshot, target, table_scope.clone()).unwrap();
        let evidence = derive_binding_evidence(&snapshot, target, table_scope).unwrap();
        assert_eq!(evidence.continuity, continuity);
        assert!(matches!(
            evidence.device_context,
            DeviceContextWitness::Unsupported {
                reason: DeviceContextUnsupportedReason::TableViewport
            }
        ));
        assert!(!evidence.device_context.supports_native_input());
        assert!(!evidence.device_context.supports_row_selection());
        let proof = proof_from_evidence(&snapshot, target, evidence);
        proof.validate().unwrap();
        assert!(!proof.supports_row_selection());

        let whole =
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).unwrap();
        assert_eq!(whole.continuity.scope, BindingProofScope::WholeStructure);
        assert!(!whole.device_context.supports_native_input());
        assert!(!whole.device_context.supports_row_selection());
    }

    #[test]
    fn row_selection_compiles_one_local_read_scoped_value_only_witness() {
        let snapshot = row_selection_snapshot();
        let selector = row_selector();
        let observation = row_selection_observation(&snapshot, &selector);
        let point = row_selection_candidate_point(observation.row_frame).unwrap();
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == observation.row_instance_ref)
            .unwrap();
        let evidence = derive_row_selection_binding_evidence(
            &snapshot,
            &selector,
            &observation,
            point,
            2,
            100,
        )
        .unwrap();

        assert!(!snapshot.complete);
        assert_eq!(
            snapshot.observer_coverage.state,
            super::super::source::SourceState::Unavailable,
            "a current, complete local viewport does not require global observer availability"
        );
        assert_eq!(
            evidence.continuity.kind,
            ObjectIdentityContinuity::ReadScoped
        );
        let DeviceContextWitness::TableViewportV1 {
            scroll_path,
            ordered_columns,
            selector_columns,
            visible_rows,
            target_row_ax_index,
            target_selected,
            target_ax_selected_settable,
            hit_point,
            ..
        } = &evidence.device_context
        else {
            panic!("row input must have a dedicated table viewport witness");
        };
        assert_eq!(scroll_path[0].label, "Projects");
        assert_eq!(
            ordered_columns
                .iter()
                .map(|c| c.ax_index)
                .collect::<Vec<_>>(),
            vec![2_u8, 7]
        );
        assert_eq!(selector_columns[0].header, "Name");
        assert_eq!(
            visible_rows.iter().map(|r| r.ax_index).collect::<Vec<_>>(),
            vec![42_u32, 99]
        );
        assert_eq!(*target_row_ax_index, 42);
        assert!(!*target_selected);
        assert!(*target_ax_selected_settable);
        assert_eq!((hit_point.x, hit_point.y), (point.x, point.y));
        assert!(!evidence.device_context.supports_native_input());
        assert!(evidence.device_context.supports_row_selection());

        let wire = serde_json::to_value(&evidence.device_context).unwrap();
        assert_eq!(wire["scope"], "table_viewport_v1");
        let mut missing_enabled = wire.clone();
        missing_enabled
            .as_object_mut()
            .unwrap()
            .remove("target_ax_enabled");
        assert!(serde_json::from_value::<DeviceContextWitness>(missing_enabled).is_err());
        let wire_text = wire.to_string();
        for forbidden_source_key in [
            "instance_ref",
            "parent_instance_ref",
            "table_instance_ref",
            "scroll_container_instance_ref",
        ] {
            assert!(!wire_text.contains(forbidden_source_key));
        }

        let proof = proof_from_evidence(&snapshot, target, evidence);
        proof.validate().unwrap();
        assert!(proof.supports_row_selection());
        assert!(!proof.supports_native_input());
        assert_eq!(proof.row_selection_point(), Some((point.x, point.y)));
    }

    #[test]
    fn row_ax_enabled_facts_are_digest_bound_and_gate_only_the_specialized_input() {
        let base = row_selection_snapshot();
        let selector = row_selector();
        let derive = |snapshot: &StructureSnapshot| {
            let observation = row_selection_observation(snapshot, &selector);
            let point = row_selection_candidate_point(observation.row_frame).unwrap();
            derive_row_selection_binding_evidence(snapshot, &selector, &observation, point, 2, 100)
        };
        let base_evidence = derive(&base).unwrap();
        let mut disabled_table_witness = base_evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 {
            table_ax_enabled, ..
        } = &mut disabled_table_witness
        {
            *table_ax_enabled = AxEnabledObservation::Disabled;
        }
        assert!(disabled_table_witness.validate().is_err());
        let mut unavailable_target_witness = base_evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 {
            target_ax_enabled, ..
        } = &mut unavailable_target_witness
        {
            *target_ax_enabled = AxEnabledObservation::Unavailable;
        }
        assert!(unavailable_target_witness.validate().is_err());

        let mut unsupported = base.clone();
        unsupported.table_viewports[0].ax_enabled = AxEnabledObservation::AttributeUnsupported;
        unsupported.table_viewports[0].visible_rows[0].ax_enabled =
            AxEnabledObservation::AttributeUnsupported;
        for object in &mut unsupported.objects {
            if matches!(object.instance_ref.as_str(), "table" | "row") {
                object.enabled = false;
            }
        }
        assert!(unsupported.validate().is_ok());
        let unsupported_evidence = derive(&unsupported).unwrap();
        assert_ne!(
            base_evidence.continuity.context_digest, unsupported_evidence.continuity.context_digest,
            "the AXEnabled observations are part of the row input closure"
        );
        assert!(matches!(
            unsupported_evidence.device_context,
            DeviceContextWitness::TableViewportV1 {
                table_ax_enabled: AxEnabledObservation::AttributeUnsupported,
                target_ax_enabled: AxEnabledObservation::AttributeUnsupported,
                ..
            }
        ));

        for (reference, state) in [
            ("table", AxEnabledObservation::Disabled),
            ("row", AxEnabledObservation::Disabled),
            ("table", AxEnabledObservation::Unavailable),
            ("row", AxEnabledObservation::Unavailable),
        ] {
            let mut denied = base.clone();
            if reference == "table" {
                denied.table_viewports[0].ax_enabled = state;
            } else {
                denied.table_viewports[0].visible_rows[0].ax_enabled = state;
            }
            denied
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == reference)
                .unwrap()
                .enabled = false;
            assert!(denied.validate().is_ok());
            assert!(
                derive(&denied).is_err(),
                "{reference} {state:?} must refuse input"
            );
        }
    }

    #[test]
    fn row_selection_closure_commits_selector_values_and_column_order_but_not_effect_value() {
        let before = row_selection_snapshot();
        let selector = row_selector();
        let derive = |snapshot: &StructureSnapshot| {
            let observation = row_selection_observation(snapshot, &selector);
            let point = row_selection_candidate_point(observation.row_frame).unwrap();
            derive_row_selection_binding_evidence(snapshot, &selector, &observation, point, 2, 100)
                .unwrap()
        };
        let before_proof = derive(&before).continuity;

        let mut changed_visible_value = before.clone();
        changed_visible_value.table_viewports[0].visible_rows[1].cells[0].value =
            Some(super::super::source::UiValue::Text("Different".into()));
        assert_ne!(
            before_proof.context_digest,
            derive(&changed_visible_value).continuity.context_digest,
            "every visible selector-column value affects this row-selection closure"
        );

        let mut changed_unselected_column = before.clone();
        changed_unselected_column.table_viewports[0].visible_rows[0].cells[1].value =
            Some(super::super::source::UiValue::Text("Pending".into()));
        assert_eq!(
            before_proof.context_digest,
            derive(&changed_unselected_column).continuity.context_digest,
            "unrelated non-selector values do not expand the row identity closure"
        );
        let mut unknown_unselected_column = before.clone();
        unknown_unselected_column.table_viewports[0].visible_rows[0].cells[1].value = None;
        assert!(
            derive(&unknown_unselected_column)
                .device_context
                .supports_row_selection()
        );

        let mut reordered_columns = before.clone();
        reordered_columns.table_viewports[0].columns.reverse();
        assert_ne!(
            before_proof.context_digest,
            derive(&reordered_columns).continuity.context_digest,
            "the complete actual AXColumn order is committed"
        );

        let mut relabelled_scroll = before.clone();
        relabelled_scroll.objects[1].description = Some("projects".into());
        assert_ne!(
            before_proof.context_digest,
            derive(&relabelled_scroll).continuity.context_digest,
            "row scope commits exact public scroll descriptions, including case"
        );

        let mut effect_value_only = before.clone();
        let target = effect_value_only
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "row")
            .unwrap();
        target.value = Some(super::super::source::UiValue::Text("effect-only".into()));
        assert_eq!(
            before_proof.context_digest,
            derive(&effect_value_only).continuity.context_digest,
            "ordinary UiObject.value remains outside the selector identity closure"
        );
    }

    #[test]
    fn row_selection_witness_rejects_unknown_ambiguous_out_of_scope_and_oversized_rows() {
        let snapshot = row_selection_snapshot();
        let selector = row_selector();
        let observation = row_selection_observation(&snapshot, &selector);
        let point = row_selection_candidate_point(observation.row_frame).unwrap();
        let evidence = derive_row_selection_binding_evidence(
            &snapshot,
            &selector,
            &observation,
            point,
            2,
            100,
        )
        .unwrap();

        let mut duplicate_index = evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 { visible_rows, .. } = &mut duplicate_index {
            visible_rows[1].ax_index = visible_rows[0].ax_index;
        }
        assert!(duplicate_index.validate().is_err());

        let mut point_outside = evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 { hit_point, .. } = &mut point_outside {
            hit_point.x = -1.0;
        }
        assert!(point_outside.validate().is_err());

        let mut ambiguous = evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 {
            selector_columns,
            visible_rows,
            ..
        } = &mut ambiguous
        {
            visible_rows[1].selector_values[0] = selector_columns[0].expected_value.clone();
        }
        assert!(ambiguous.validate().is_err());

        let mut oversized = evidence.device_context.clone();
        if let DeviceContextWitness::TableViewportV1 { visible_rows, .. } = &mut oversized {
            for extra_index in 0..3_u32 {
                let mut row = visible_rows[1].clone();
                row.ax_index = 200 + extra_index;
                row.selector_values[0] = super::super::source::UiValue::Text("x".repeat(2_000));
                visible_rows.push(row);
            }
        }
        assert!(
            oversized.validate().is_err(),
            "row witness retains the 4 KiB wire cap"
        );

        let mut missing_row_index = row_selection_snapshot();
        missing_row_index.table_viewports[0].visible_rows[1].ax_index = None;
        missing_row_index.table_viewports[0].visible_rows[1].cells[0].row_ax_index = None;
        missing_row_index.validate().unwrap();
        let fresh_observation = row_selection_observation(&missing_row_index, &selector);
        let fresh_target = missing_row_index
            .objects
            .iter()
            .find(|object| object.instance_ref == fresh_observation.row_instance_ref)
            .unwrap();
        assert!(
            derive_row_selection_binding_evidence(
                &missing_row_index,
                &selector,
                &fresh_observation,
                point,
                2,
                100,
            )
            .is_err()
        );
        assert_eq!(fresh_target.role, "AXRow");

        let mut unknown_value = row_selection_snapshot();
        unknown_value.table_viewports[0].visible_rows[1].cells[0].value = None;
        assert!(
            RowSelectionObservation::from_snapshot(&selector, &unknown_value, 50, 2, 100).is_err()
        );

        let mut ambiguous_source = row_selection_snapshot();
        ambiguous_source.table_viewports[0].visible_rows[1].cells[0].value =
            Some(super::super::source::UiValue::Text("Acme".into()));
        assert!(
            RowSelectionObservation::from_snapshot(&selector, &ambiguous_source, 50, 2, 100)
                .is_err()
        );
    }

    #[test]
    fn row_selection_proof_allows_only_its_dedicated_ax_action_and_observation_purpose() {
        use super::super::device::{
            ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
            ScheduledPrimitive, SourceFreshness, SurfaceRef,
        };
        use super::super::primitive::{AxActionKind, Primitive};

        let snapshot = row_selection_snapshot();
        let selector = row_selector();
        let observation = row_selection_observation(&snapshot, &selector);
        let point = row_selection_candidate_point(observation.row_frame).unwrap();
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == observation.row_instance_ref)
            .unwrap();
        let evidence = derive_row_selection_binding_evidence(
            &snapshot,
            &selector,
            &observation,
            point,
            2,
            100,
        )
        .unwrap();
        let proof = proof_from_evidence(&snapshot, target, evidence);
        let target_ref = proof.binding.as_str().to_owned();
        let surface = SurfaceRef {
            id: "main".into(),
            epoch: 1,
        };
        let intent = ActionIntent {
            intent_id: "row-intent".into(),
            attempt_id: None,
            task_id: "task".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: 1,
            execution_domain: ExecutionDomain::ForegroundNative,
            permit: PermitRef {
                id: "permit".into(),
                generation: 1,
            },
            surface: surface.clone(),
            target_ref: target_ref.clone(),
            binding_fingerprint: Some(proof.fingerprint.digest()),
            binding_proof: Some(proof.clone()),
            owned_document_proof: None,
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies: Default::default(),
            evidence_id: "row-evidence".into(),
            latest_start_ms: 2,
            duration_ms: 100,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                },
            }],
        };
        intent.validate().unwrap();

        let mut generic_press = intent.clone();
        generic_press.steps[0].primitive = Primitive::AxAction {
            action: AxActionKind::Press,
        };
        assert!(generic_press.validate().is_err());
        let mut unbound_selection = intent.clone();
        unbound_selection.binding_proof = None;
        unbound_selection.binding_fingerprint = None;
        assert!(unbound_selection.validate().is_err());

        let mut source_observation = Observation {
            observation_id: "row-observation".into(),
            session_epoch: 1,
            surface,
            target_ref,
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence: snapshot.source_sequence,
            observed_ms: 1,
            valid_until_ms: 20,
            freshness: SourceFreshness::Verified,
            execution_domain: ExecutionDomain::ForegroundNative,
            purpose: ObservationPurpose::RowSelection,
            binding_proof: Some(proof),
            owned_document_proof: None,
            dependencies: Default::default(),
        };
        source_observation.validate().unwrap();
        source_observation.purpose = ObservationPurpose::TargetInput;
        assert!(source_observation.validate().is_err());
    }

    #[test]
    fn binding_context_digest_tracks_actual_target_column_ax_index() {
        let unknown = table_column_snapshot(None);
        let indexed = table_column_snapshot(Some(2));
        let reindexed = table_column_snapshot(Some(3));
        let derive = |snapshot: &StructureSnapshot| {
            let viewport = &snapshot.table_viewports[0];
            let target = snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref.as_str() == "column")
                .unwrap();
            derive_binding_continuity(
                snapshot,
                target,
                BindingProofScope::TableViewport {
                    table_instance_ref: viewport.table_instance_ref.clone(),
                    scroll_container_instance_ref: viewport.scroll_container_instance_ref.clone(),
                    viewport_frame: viewport.viewport_frame,
                    visible_rows_complete: viewport.visible_rows_complete,
                    columns_complete: viewport.columns_complete,
                },
            )
            .unwrap()
        };

        let unknown_proof = derive(&unknown);
        let indexed_proof = derive(&indexed);
        let reindexed_proof = derive(&reindexed);
        assert_eq!(
            unknown_proof.kind,
            ObjectIdentityContinuity::NotificationCovered,
            "unknown AXIndex remains an identity fact, not an inferred ordinal"
        );
        assert_eq!(indexed_proof.kind, unknown_proof.kind);
        assert_ne!(unknown_proof.context_digest, indexed_proof.context_digest);
        assert_ne!(indexed_proof.context_digest, reindexed_proof.context_digest);
    }

    #[test]
    fn row_and_cell_actual_ax_indices_change_the_table_closure_digest() {
        let derive = |snapshot: &StructureSnapshot| {
            snapshot.validate().unwrap();
            let row = snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref.as_str() == "row")
                .unwrap();
            derive_binding_continuity(snapshot, row, BindingProofScope::WholeStructure).unwrap()
        };

        for actual_index in [42, 511] {
            let snapshot = indexed_row_table_snapshot(
                Some(actual_index),
                Some(actual_index),
                Some(2),
                Some(2),
            );
            assert_eq!(
                snapshot.table_viewports[0].visible_rows[0].ax_index,
                Some(actual_index)
            );
            let proof = derive(&snapshot);
            assert_eq!(proof.kind, ObjectIdentityContinuity::NotificationCovered);
        }

        let unknown = indexed_row_table_snapshot(None, None, None, None);
        let known = indexed_row_table_snapshot(Some(42), Some(42), None, Some(7));
        let unknown_proof = derive(&unknown);
        let known_proof = derive(&known);
        assert_ne!(
            unknown_proof.context_digest, known_proof.context_digest,
            "unknown indices stay null and are never replaced with visible-row ordinal zero"
        );

        let no_cell_column_index = indexed_row_table_snapshot(Some(42), Some(42), None, None);
        let known_cell_column_index = indexed_row_table_snapshot(Some(42), Some(42), None, Some(7));
        assert_ne!(
            derive(&no_cell_column_index).context_digest,
            derive(&known_cell_column_index).context_digest,
            "cell AXColumn index is committed even when its resolved Source column index is unknown"
        );

        let before_other_visible_row =
            indexed_row_table_snapshot(Some(42), Some(42), Some(2), Some(2));
        let mut changed_other_visible_row = before_other_visible_row.clone();
        changed_other_visible_row.table_viewports[0].visible_rows[1].ax_index = Some(100);
        assert_ne!(
            derive(&before_other_visible_row).context_digest,
            derive(&changed_other_visible_row).context_digest,
            "the full visible-row AXIndex order is part of the table closure"
        );
    }

    fn sortable_header_evidence(snapshot: &StructureSnapshot) -> BindingDerivationEvidence {
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "header-a")
            .unwrap();
        derive_binding_evidence(snapshot, target, BindingProofScope::WholeStructure).unwrap()
    }

    #[test]
    fn sortable_header_witness_carries_bounded_actual_table_column_relation() {
        let snapshot = sortable_table_snapshot();
        snapshot.validate().unwrap();
        let evidence = sortable_header_evidence(&snapshot);
        let DeviceContextWitness::WholeStructureV1 {
            table_column: Some(relation),
            menu: None,
            focus: None,
            ..
        } = &evidence.device_context
        else {
            panic!("complete exact sortable header should have a typed table relation");
        };
        assert_eq!(relation.table_ancestry_index, 2);
        assert_eq!(relation.ordered_ax_indices, vec![2, 7]);
        assert_eq!(relation.target_ax_index, 2);

        let wire = serde_json::to_value(&evidence.device_context).unwrap();
        let encoded = serde_json::to_vec(&wire).unwrap();
        assert!(encoded.len() <= MAX_DEVICE_CONTEXT_WITNESS_BYTES);
        assert_eq!(
            wire["table_column"]["ordered_ax_indices"],
            serde_json::json!([2, 7])
        );
        let wire_text = String::from_utf8(encoded).unwrap();
        for source_ref in ["column-a", "column-b", "header-a", "header-b"] {
            assert!(
                !wire_text.contains(source_ref),
                "witness leaked Source reference {source_ref}"
            );
        }
        evidence.device_context.validate().unwrap();

        for invalid in [
            "table_index",
            "duplicate_index",
            "missing_target_index",
            "wrong_target",
        ] {
            let mut corrupted = evidence.device_context.clone();
            let DeviceContextWitness::WholeStructureV1 {
                target,
                table_column: Some(relation),
                ..
            } = &mut corrupted
            else {
                unreachable!();
            };
            match invalid {
                "table_index" => relation.table_ancestry_index = 1,
                "duplicate_index" => relation.ordered_ax_indices[1] = 2,
                "missing_target_index" => relation.target_ax_index = 63,
                "wrong_target" => target.object.subrole = None,
                _ => unreachable!(),
            }
            assert!(corrupted.validate().is_err(), "invalid relation: {invalid}");
        }

        let mut missing_relation = evidence.device_context.clone();
        let DeviceContextWitness::WholeStructureV1 { table_column, .. } = &mut missing_relation
        else {
            unreachable!();
        };
        *table_column = None;
        assert!(missing_relation.validate().is_err());

        let ordinary = identity_snapshot();
        let ordinary_target = &ordinary.objects[2];
        let ordinary_evidence = derive_binding_evidence(
            &ordinary,
            ordinary_target,
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        let mut non_table_relation = ordinary_evidence.device_context;
        let DeviceContextWitness::WholeStructureV1 { table_column, .. } = &mut non_table_relation
        else {
            unreachable!();
        };
        *table_column = Some(Box::new(DeviceContextTableColumnRelation {
            table_ancestry_index: 0,
            ordered_ax_indices: vec![2],
            target_ax_index: 2,
        }));
        assert!(non_table_relation.validate().is_err());
    }

    #[test]
    fn sortable_header_without_a_table_and_nested_outline_are_not_supported() {
        let mut orphan = identity_snapshot();
        orphan.objects[2].role = "AXButton".into();
        orphan.objects[2].subrole = Some("AXSortButton".into());
        orphan.objects[2].actions = vec![AxActionName::Press];
        let evidence = derive_binding_evidence(
            &orphan,
            &orphan.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        assert!(!evidence.device_context.supports_native_input());

        let snapshot = sortable_table_snapshot();
        let mut nested = sortable_header_evidence(&snapshot).device_context;
        let DeviceContextWitness::WholeStructureV1 {
            ancestry,
            scroll_ancestors,
            ..
        } = &mut nested
        else {
            unreachable!();
        };
        // Keep all other whole-structure facts valid; only the ancestor kind changes.
        ancestry[1].role = "AXOutline".into();
        scroll_ancestors.clear();
        assert!(nested.validate().is_err());
    }

    #[test]
    fn sortable_header_ax_index_and_column_order_change_digest_and_witness() {
        let before = sortable_table_snapshot();
        let before_evidence = sortable_header_evidence(&before);
        let DeviceContextWitness::WholeStructureV1 {
            table_column: Some(before_relation),
            ..
        } = &before_evidence.device_context
        else {
            panic!("expected sortable table relation");
        };

        let mut changed_target_index = before.clone();
        changed_target_index.table_viewports[0].columns[0].ax_index = Some(3);
        let changed_index_evidence = sortable_header_evidence(&changed_target_index);
        let DeviceContextWitness::WholeStructureV1 {
            table_column: Some(changed_index_relation),
            ..
        } = &changed_index_evidence.device_context
        else {
            panic!("changed actual AXIndex must retain the sortable relation");
        };
        assert_eq!(changed_index_relation.target_ax_index, 3);
        assert_ne!(
            before_evidence.continuity.context_digest,
            changed_index_evidence.continuity.context_digest
        );

        let mut reordered_columns = before.clone();
        reordered_columns.table_viewports[0].columns.reverse();
        let reordered_evidence = sortable_header_evidence(&reordered_columns);
        let DeviceContextWitness::WholeStructureV1 {
            table_column: Some(reordered_relation),
            ..
        } = &reordered_evidence.device_context
        else {
            panic!("reordered complete columns must retain their typed relation");
        };
        assert_eq!(reordered_relation.ordered_ax_indices, vec![7, 2]);
        assert_eq!(
            reordered_relation.target_ax_index,
            before_relation.target_ax_index
        );
        assert_ne!(
            before_evidence.continuity.context_digest, reordered_evidence.continuity.context_digest,
            "the full actual AX column order is part of the Source closure"
        );
    }

    #[test]
    fn incomplete_or_non_sortable_table_relations_never_create_native_witnesses() {
        let cases = [
            "missing_ax_index",
            "duplicate_ax_index",
            "missing_header",
            "duplicate_header",
            "disabled",
            "no_press",
            "wrong_subrole",
            "row",
            "cell",
        ];
        for case in cases {
            let mut snapshot = sortable_table_snapshot();
            match case {
                "missing_ax_index" => snapshot.table_viewports[0].columns[0].ax_index = None,
                "duplicate_ax_index" => snapshot.table_viewports[0].columns[1].ax_index = Some(2),
                "missing_header" => {
                    snapshot.table_viewports[0].columns[0].header_instance_ref = None
                }
                "duplicate_header" => {
                    snapshot.table_viewports[0].columns[1].header_instance_ref =
                        Some(ObjectRef::new("header-a").unwrap())
                }
                "disabled" => snapshot.objects[4].enabled = false,
                "no_press" => snapshot.objects[4].actions.clear(),
                "wrong_subrole" => snapshot.objects[4].subrole = Some("AXButton".into()),
                "row" => {
                    snapshot.objects[4].role = "AXRow".into();
                    snapshot.objects[4].subrole = None;
                }
                "cell" => {
                    snapshot.objects[4].role = "AXCell".into();
                    snapshot.objects[4].subrole = None;
                }
                _ => unreachable!(),
            }
            update_identity_coverage(&mut snapshot);
            snapshot.validate().unwrap_or_else(|error| {
                panic!("negative fixture {case} should be valid Source data: {error}")
            });
            let evidence = sortable_header_evidence(&snapshot);
            assert!(
                matches!(
                    evidence.device_context,
                    DeviceContextWitness::Unsupported {
                        reason: DeviceContextUnsupportedReason::TableViewport
                    }
                ),
                "negative table case {case} must not produce a native witness"
            );
        }

        let mut partial = sortable_table_snapshot();
        partial.complete = false;
        partial.incomplete_reason = Some("bounded partial traversal".into());
        let target = partial
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "header-a")
            .unwrap();
        assert!(
            derive_binding_evidence(&partial, target, BindingProofScope::WholeStructure).is_err()
        );
    }

    #[test]
    fn oversized_device_witness_rejects_only_device_evidence() {
        let mut snapshot = identity_snapshot();
        for index in 0..16 {
            let mut sibling = object(
                &format!("large-sibling-{index}"),
                Some("group"),
                "AXStaticText",
                &"s".repeat(512),
            );
            sibling.frame.x += index as f64;
            snapshot.objects.push(sibling);
        }
        update_identity_coverage(&mut snapshot);
        let target = &snapshot.objects[2];
        let continuity =
            derive_binding_continuity(&snapshot, target, BindingProofScope::WholeStructure)
                .unwrap();
        assert_eq!(continuity.source_sequence, snapshot.source_sequence);
        assert!(
            derive_binding_evidence(&snapshot, target, BindingProofScope::WholeStructure).is_err()
        );
    }

    #[test]
    fn binding_context_digest_ignores_value_only_effect_changes() {
        let mut before = identity_snapshot();
        before.objects[2].value = Some(super::super::source::UiValue::Bool(false));
        let old = continuity_for(&before, "target");

        before.objects[2].value = Some(super::super::source::UiValue::Bool(true));
        before.objects[2].focused = true;
        let after = continuity_for(&before, "target");

        assert_eq!(old.kind, ObjectIdentityContinuity::NotificationCovered);
        assert_eq!(old.context_digest, after.context_digest);
    }

    #[test]
    fn binding_context_digest_tracks_group_and_fingerprint_sibling_order() {
        let original = identity_snapshot();
        let original_evidence = derive_binding_evidence(
            &original,
            &original.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        let original_digest = original_evidence.continuity.context_digest.clone();

        let mut regrouped = original.clone();
        regrouped.objects[1].title = Some("Email".into());
        regrouped.objects[2].group = Some("Email".into());
        let regrouped_evidence = derive_binding_evidence(
            &regrouped,
            &regrouped.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        assert_ne!(
            original_digest,
            regrouped_evidence.continuity.context_digest
        );
        assert_ne!(
            original_evidence.device_context,
            regrouped_evidence.device_context
        );

        let mut reordered = original.clone();
        reordered.objects.swap(2, 3);
        let reordered_target = reordered
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "target")
            .unwrap();
        let reordered_evidence = derive_binding_evidence(
            &reordered,
            reordered_target,
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        assert_ne!(
            original_digest,
            reordered_evidence.continuity.context_digest
        );
        assert_ne!(
            original_evidence.device_context,
            reordered_evidence.device_context
        );

        let mut relabelled_sibling = original.clone();
        relabelled_sibling.objects[3].title = Some("Changed heading".into());
        assert_ne!(
            original_digest,
            continuity_for(&relabelled_sibling, "target").context_digest
        );

        let mut moved_sibling = original;
        moved_sibling.objects[3].frame.x += 1.0;
        assert_ne!(
            original_digest,
            continuity_for(&moved_sibling, "target").context_digest
        );
    }

    #[test]
    fn unsupported_target_ancestry_or_sibling_downgrades_the_binding_closure() {
        let mut snapshot = identity_snapshot();
        snapshot.objects[1].identity_continuity = ObjectIdentityContinuity::ReadScoped;
        update_identity_coverage(&mut snapshot);
        assert_eq!(
            continuity_for(&snapshot, "target").kind,
            ObjectIdentityContinuity::ReadScoped
        );
        let same_read = derive_binding_evidence(
            &snapshot,
            &snapshot.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        assert_eq!(
            same_read.continuity.kind,
            ObjectIdentityContinuity::ReadScoped
        );
        assert!(same_read.device_context.supports_native_input());

        let mut snapshot = identity_snapshot();
        snapshot.objects[3].identity_continuity = ObjectIdentityContinuity::ReadScoped;
        update_identity_coverage(&mut snapshot);
        assert_eq!(
            continuity_for(&snapshot, "target").kind,
            ObjectIdentityContinuity::ReadScoped,
            "a sibling affects fp1's index even though its label is outside the closure"
        );

        let mut unresolved_group = identity_snapshot();
        unresolved_group.objects[2].group = Some("unobserved group".into());
        assert_eq!(
            continuity_for(&unresolved_group, "target").kind,
            ObjectIdentityContinuity::ReadScoped
        );
        assert!(
            derive_binding_evidence(
                &unresolved_group,
                &unresolved_group.objects[2],
                BindingProofScope::WholeStructure
            )
            .is_err()
        );
    }

    #[test]
    fn read_scoped_binding_proofs_expire_on_the_next_source_sequence() {
        let mut first = identity_snapshot();
        first.objects[2].identity_continuity = ObjectIdentityContinuity::ReadScoped;
        update_identity_coverage(&mut first);
        let evidence =
            derive_binding_evidence(&first, &first.objects[2], BindingProofScope::WholeStructure)
                .unwrap();
        let proof = evidence.continuity.clone();

        let mut second = first.clone();
        second.source_sequence += 1;
        second.snapshot_id = RequestId::new("next-read").unwrap();
        second.objects[2].instance_ref = ObjectRef::new("target-next-read").unwrap();
        let next_evidence = derive_binding_evidence(
            &second,
            &second.objects[2],
            BindingProofScope::WholeStructure,
        )
        .unwrap();
        let next_proof = next_evidence.continuity;
        assert!(!proof.same_closure(&next_proof));
        assert_ne!(proof.context_digest, next_proof.context_digest);
        assert_eq!(evidence.device_context, next_evidence.device_context);
    }

    #[test]
    fn continuity_proof_rejects_missing_or_stale_metadata() {
        let mut proof = continuity_for(&identity_snapshot(), "target");
        proof.source_sequence = 0;
        assert!(proof.validate().is_err());
        proof.source_sequence = 1;
        proof.context_digest = "bc1:ABC".into();
        assert!(proof.validate().is_err());
        proof.context_digest =
            "bc1:0000000000000000000000000000000000000000000000000000000000000000".into();
        proof.scope = BindingProofScope::TableViewport {
            table_instance_ref: ObjectRef::new("same").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("same").unwrap(),
            viewport_frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            visible_rows_complete: true,
            columns_complete: true,
        };
        assert!(proof.validate().is_err());
    }

    #[test]
    fn an_objects_fingerprint_follows_its_ancestors_label_and_position() {
        let snap = snapshot(vec![
            object("g", None, "group", "Push"),
            object("a", Some("g"), "switch", "  Daily   Picks "),
            object("b", Some("g"), "switch", "Weekly"),
        ]);
        let fp = instance_fingerprint(&snap, &snap.objects[1]);
        assert_eq!(fp.ancestor_roles, vec!["group".to_string()]);
        assert_eq!(fp.label, "daily picks");
        assert_eq!(fp.sibling_index, 0);
        assert_eq!(
            fp.frame,
            [10, 21, 40, 20],
            "frames are rounded to whole points"
        );
        assert_eq!(
            instance_fingerprint(&snap, &snap.objects[2]).sibling_index,
            1
        );
        assert_ne!(
            instance_fingerprint(&snap, &snap.objects[1]).digest(),
            instance_fingerprint(&snap, &snap.objects[2]).digest()
        );
    }

    #[test]
    fn the_same_label_under_different_groups_has_different_fingerprints() {
        let mut first = object("a", Some("g1"), "switch", "Daily picks");
        first.group = Some("Push".into());
        let mut second = object("b", Some("g2"), "switch", "Daily picks");
        second.group = Some("Email".into());
        let snap = snapshot(vec![
            object("g1", None, "group", "Push"),
            object("g2", None, "group", "Email"),
            first,
            second,
        ]);
        let push = instance_fingerprint(&snap, &snap.objects[2]);
        let email = instance_fingerprint(&snap, &snap.objects[3]);
        assert_eq!(
            push.frame, email.frame,
            "a reused layout puts them at the same place"
        );
        assert_eq!(push.sibling_index, email.sibling_index);
        assert_ne!(
            push.digest(),
            email.digest(),
            "only the group tells them apart"
        );
    }

    #[test]
    fn labels_are_normalised_for_matching() {
        assert_eq!(normalize_label("  Daily   Picks \n"), "daily picks");
        assert_eq!(normalize_label("每日  推荐"), "每日 推荐");
    }

    #[test]
    fn window_root_binding_uses_test_projection_census_without_promoting_input() {
        let (snapshot, table_scope, target) = scoped_window_fixture();
        assert!(!snapshot.complete);
        let evidence =
            derive_window_root_binding_evidence(&snapshot, &target, &table_scope).unwrap();
        assert_eq!(
            evidence.continuity.scope,
            BindingProofScope::WindowRoot {
                table_scope: table_scope.clone()
            }
        );
        let DeviceContextWitness::WindowRootV1 {
            app,
            window,
            root,
            source,
        } = &evidence.device_context
        else {
            panic!("partial source root must use its dedicated witness")
        };
        assert_eq!(app.pid, snapshot.app.pid);
        assert_eq!(
            window.window_id,
            snapshot.window_context.as_ref().unwrap().origin_window_id
        );
        assert_eq!(root.role, "AXWindow");
        assert_eq!(root.subrole.as_deref(), Some("AXStandardWindow"));
        assert_eq!(source.verified_source_sequence, snapshot.source_sequence);
        assert!(!evidence.device_context.supports_native_input());
        assert!(!evidence.device_context.supports_row_selection());
        assert!(evidence.device_context.supports_window_activation());
        let witness_json = serde_json::to_string(&evidence.device_context).unwrap();
        assert!(!witness_json.contains(target.instance_ref.as_str()));

        let proof = proof_from_evidence(&snapshot, &target, evidence);
        proof.validate().unwrap();
        assert!(proof.supports_window_activation());
        assert!(!proof.supports_native_input());
        assert!(!proof.supports_row_selection());

        let mut next = snapshot.clone();
        next.source_sequence += 1;
        next.observed_ms += 10;
        next.snapshot_id = RequestId::new("scoped-window-next-read").unwrap();
        let next_source = next.window_context.as_mut().unwrap();
        next_source.verified_source_sequence = next.source_sequence;
        next_source.verified_observed_ms = next.observed_ms;
        let next_target = next
            .objects
            .iter()
            .find(|object| object.instance_ref == target.instance_ref)
            .unwrap()
            .clone();
        let next_proof = proof_from_evidence(
            &next,
            &next_target,
            derive_window_root_binding_evidence(&next, &next_target, &table_scope).unwrap(),
        );
        assert!(proof.continuity.same_closure(&next_proof.continuity));
        assert!(proof.same_instance(&next_proof));
    }

    #[test]
    fn window_root_binding_rejects_wrong_scope_source_root_and_tampered_provenance() {
        let (snapshot, table_scope, target) = scoped_window_fixture();
        let mut wrong_scope = table_scope.clone();
        wrong_scope.column_headers[0] = "Different header".into();
        assert!(derive_window_root_binding_evidence(&snapshot, &target, &wrong_scope).is_err());

        let mut no_candidate = snapshot.clone();
        no_candidate
            .table_scope_search
            .as_mut()
            .unwrap()
            .candidates
            .clear();
        assert!(derive_window_root_binding_evidence(&no_candidate, &target, &table_scope).is_err());
        let mut multiple_candidates = snapshot.clone();
        let duplicate = multiple_candidates
            .table_scope_search
            .as_ref()
            .unwrap()
            .candidates[0]
            .clone();
        multiple_candidates
            .table_scope_search
            .as_mut()
            .unwrap()
            .candidates
            .push(duplicate);
        assert!(
            derive_window_root_binding_evidence(&multiple_candidates, &target, &table_scope)
                .is_err()
        );

        let mut wrong_kernel_pid = snapshot.clone();
        wrong_kernel_pid
            .window_context
            .as_mut()
            .unwrap()
            .process
            .pid += 1;
        assert!(
            derive_window_root_binding_evidence(&wrong_kernel_pid, &target, &table_scope).is_err()
        );

        let mut wrong_window = snapshot.clone();
        wrong_window
            .window_context
            .as_mut()
            .unwrap()
            .origin_window_id = "window-other".into();
        assert!(derive_window_root_binding_evidence(&wrong_window, &target, &table_scope).is_err());

        let mut wrong_root = target.clone();
        wrong_root.title = Some("Replacement root".into());
        assert!(derive_window_root_binding_evidence(&snapshot, &wrong_root, &table_scope).is_err());

        let proof = proof_from_evidence(
            &snapshot,
            &target,
            derive_window_root_binding_evidence(&snapshot, &target, &table_scope).unwrap(),
        );
        let mut tampered_scope = proof.clone();
        if let BindingProofScope::WindowRoot { table_scope } = &mut tampered_scope.continuity.scope
        {
            table_scope.column_headers[0] = "Different header".into();
        }
        assert!(tampered_scope.validate().is_err());

        let mut tampered_kernel = proof.clone();
        if let DeviceContextWitness::WindowRootV1 { source, .. } =
            &mut tampered_kernel.device_context
        {
            source.process.start_seconds += 1;
        }
        assert!(tampered_kernel.validate().is_err());

        let mut tampered_sequence = proof;
        if let DeviceContextWitness::WindowRootV1 { source, .. } =
            &mut tampered_sequence.device_context
        {
            source.verified_source_sequence += 1;
        }
        assert!(tampered_sequence.validate().is_err());
    }

    #[test]
    fn window_root_witness_requires_a_known_title_for_native_comparison() {
        let (snapshot, table_scope, target) = scoped_window_fixture();
        let mut witness = derive_window_root_binding_evidence(&snapshot, &target, &table_scope)
            .unwrap()
            .device_context;
        if let DeviceContextWitness::WindowRootV1 { window, root, .. } = &mut witness {
            window.title = None;
            root.label.clear();
        }
        assert!(witness.validate().is_err());
        assert!(!witness.supports_window_activation());
    }

    #[test]
    fn window_root_observation_and_intent_are_activation_only_and_fresh() {
        use super::super::device::{
            ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
            ScheduledPrimitive, SourceFreshness,
        };
        use super::super::primitive::Primitive;
        use std::collections::BTreeMap;

        let (snapshot, table_scope, target) = scoped_window_fixture();
        let proof = proof_from_evidence(
            &snapshot,
            &target,
            derive_window_root_binding_evidence(&snapshot, &target, &table_scope).unwrap(),
        );
        let source = match &proof.device_context {
            DeviceContextWitness::WindowRootV1 { source, .. } => source,
            _ => unreachable!(),
        };
        let source_epoch = source.session_epoch;
        let source_sequence = source.verified_source_sequence;
        let source_observed_ms = source.verified_observed_ms;
        let dependencies =
            BTreeMap::from([("authorization".to_string(), 1), ("window".to_string(), 1)]);
        let mut observation = Observation {
            observation_id: "window-root-observation".into(),
            session_epoch: source_epoch,
            surface: snapshot.surface.clone(),
            target_ref: proof.binding.as_str().to_owned(),
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence,
            observed_ms: source_observed_ms,
            valid_until_ms: source_observed_ms + 1_000,
            freshness: SourceFreshness::Verified,
            execution_domain: ExecutionDomain::ForegroundNative,
            purpose: ObservationPurpose::WindowActivation,
            binding_proof: Some(proof.clone()),
            owned_document_proof: None,
            dependencies: dependencies.clone(),
        };
        observation.validate().unwrap();
        observation.valid_until_ms = observation.observed_ms + 2_000;
        observation.validate().unwrap();

        let mut expired = observation.clone();
        expired.valid_until_ms = expired.observed_ms + 2_001;
        assert!(expired.validate().is_err());
        let mut stale_sequence = observation.clone();
        stale_sequence.source_sequence += 1;
        assert!(stale_sequence.validate().is_err());
        let mut stale_observed = observation.clone();
        stale_observed.observed_ms += 1;
        assert!(stale_observed.validate().is_err());
        let mut stale_epoch = observation.clone();
        stale_epoch.session_epoch += 1;
        assert!(stale_epoch.validate().is_err());
        let mut uncertain = observation.clone();
        uncertain.freshness = SourceFreshness::Uncertain;
        assert!(uncertain.validate().is_err());
        let mut wrong_dependencies = observation.clone();
        wrong_dependencies.dependencies.insert("focus".into(), 1);
        assert!(wrong_dependencies.validate().is_err());
        observation.purpose = ObservationPurpose::TargetInput;
        assert!(observation.validate().is_err());
        observation.purpose = ObservationPurpose::RowSelection;
        assert!(observation.validate().is_err());

        let mut intent = ActionIntent {
            intent_id: "window-root-intent".into(),
            attempt_id: Some("window-attempt".into()),
            task_id: "task".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: source_epoch,
            execution_domain: ExecutionDomain::ForegroundNative,
            permit: PermitRef {
                id: "permit".into(),
                generation: 1,
            },
            surface: snapshot.surface.clone(),
            target_ref: proof.binding.as_str().to_owned(),
            binding_fingerprint: Some(proof.fingerprint.digest()),
            binding_proof: Some(proof),
            owned_document_proof: None,
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies,
            evidence_id: "window-root-evidence".into(),
            latest_start_ms: source_observed_ms,
            duration_ms: 1,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::ActivateWindow {
                    app: snapshot.app.bundle_id.clone(),
                },
            }],
        };
        intent.validate().unwrap();
        intent.steps[0].primitive = Primitive::KeyPress {
            key: "A".into(),
            modifiers: Default::default(),
        };
        assert!(intent.validate().is_err());
        intent.steps[0].primitive = Primitive::ActivateWindow {
            app: snapshot.app.bundle_id.clone(),
        };
        intent.session_epoch += 1;
        assert!(intent.validate().is_err());
    }
}

#[cfg(test)]
mod background_ax_contract_tests {
    use super::*;
    use crate::desktop::device::{
        ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
        ScheduledPrimitive, SourceFreshness, SurfaceRef,
    };
    use crate::desktop::domain::ExecutionDomain;
    use crate::desktop::primitive::{AxActionKind, Primitive};
    use crate::desktop::source::{
        AppIdentity, KernelProcessIdentity, ObservationFact, SourceBackgroundWindowContext,
        SourceWindowContext,
    };
    use std::collections::BTreeMap;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn background_snapshot() -> StructureSnapshot {
        use crate::desktop::source::{
            GlobalFocusObservation, ObserverCoverage, SourceState, TargetContext,
        };

        let app = AppIdentity {
            bundle_id: "com.example.settings".into(),
            name: "Settings".into(),
            pid: 42,
        };
        let window_frame = rect(0.0, 0.0, 400.0, 300.0);
        let mut snapshot = StructureSnapshot {
            snapshot_id: RequestId::new("background-snapshot-7").unwrap(),
            session_epoch: 3,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "native/3/monotonic".into(),
            identity_boundary: None,
            frame_ref: None,
            source_sequence: 7,
            observed_ms: 1_000,
            app: app.clone(),
            windows: vec![WindowInfo {
                window_id: "w1".into(),
                title: Some("Settings".into()),
                frame: window_frame,
                z_index: 0,
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: app.bundle_id.clone(),
            }],
            global_focus: GlobalFocusObservation {
                app: ObservationFact::Known {
                    value: AppIdentity {
                        bundle_id: "com.example.other".into(),
                        name: "Other App".into(),
                        pid: 99,
                    },
                },
                window_id: ObservationFact::Unavailable,
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
            menu_anchors: vec![],
            menu_context: None,
            focus_context: None,
            window_context: None,
            table_viewports: vec![],
            table_scope_search: None,
            owned_document_evidence: None,
        };
        let mut root = UiObject {
            instance_ref: ObjectRef::new("root").unwrap(),
            parent: None,
            window_id: "w1".into(),
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            title: Some("Settings".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: window_frame,
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        };
        root.focused = false;
        let group = UiObject {
            instance_ref: ObjectRef::new("group").unwrap(),
            parent: Some(root.instance_ref.clone()),
            window_id: "w1".into(),
            role: "AXGroup".into(),
            subrole: None,
            title: Some("Actions".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: rect(10.0, 10.0, 200.0, 100.0),
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        };
        let target = UiObject {
            instance_ref: ObjectRef::new("save").unwrap(),
            parent: Some(group.instance_ref.clone()),
            window_id: "w1".into(),
            role: "AXButton".into(),
            subrole: None,
            title: Some("Save".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: rect(20.0, 20.0, 80.0, 30.0),
            actions: vec![AxActionName::Press],
            ax_action_names: Some(vec!["AXPress".into()]),
            in_view: true,
            group: Some("Actions".into()),
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        };
        let sibling = UiObject {
            instance_ref: ObjectRef::new("cancel").unwrap(),
            parent: Some(group.instance_ref.clone()),
            window_id: "w1".into(),
            role: "AXButton".into(),
            subrole: None,
            title: Some("Cancel".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: rect(110.0, 20.0, 80.0, 30.0),
            actions: vec![AxActionName::Press],
            ax_action_names: None,
            in_view: true,
            group: Some("Actions".into()),
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        };
        snapshot.objects = vec![root, group, target, sibling];
        snapshot.observer_coverage.covered_objects = snapshot.objects.len() as u32;

        let mut source = SourceBackgroundWindowContext {
            window: SourceWindowContext {
                source_session_id: "source-session".into(),
                session_epoch: snapshot.session_epoch,
                process: KernelProcessIdentity {
                    pid: app.pid,
                    start_seconds: 100,
                    start_micros: 20,
                },
                origin_window_id: "w1".into(),
                origin_window_frame: window_frame,
                verified_source_sequence: snapshot.source_sequence,
                verified_observed_ms: snapshot.observed_ms,
            },
            hidden: ObservationFact::Known { value: false },
            minimized: ObservationFact::Known { value: false },
            target_context_hash: String::new(),
        };
        source.target_context_hash = source.computed_target_context_hash(&app).unwrap();
        snapshot.target_context = Some(TargetContext::NativeWindow { app, source });
        snapshot
    }

    fn background_proof() -> BindingProof {
        let app = Box::new(DeviceContextApp {
            bundle_id: "com.example.settings".into(),
            pid: 42,
        });
        let window_frame = rect(0.0, 0.0, 400.0, 300.0);
        let window = Box::new(DeviceContextWindow {
            window_id: "w1".into(),
            title: Some("settings".into()),
            frame: window_frame,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: app.bundle_id.clone(),
        });
        let root = DeviceContextObject {
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            label: "settings".into(),
            frame: window_frame,
        };
        let group = DeviceContextObject {
            role: "AXGroup".into(),
            subrole: None,
            label: "actions".into(),
            frame: rect(10.0, 10.0, 200.0, 100.0),
        };
        let target = DeviceContextObject {
            role: "AXButton".into(),
            subrole: None,
            label: "save".into(),
            frame: rect(20.0, 20.0, 80.0, 30.0),
        };
        let mut source = SourceBackgroundWindowContext {
            window: SourceWindowContext {
                source_session_id: "source-session".into(),
                session_epoch: 3,
                process: KernelProcessIdentity {
                    pid: app.pid,
                    start_seconds: 100,
                    start_micros: 20,
                },
                origin_window_id: window.window_id.clone(),
                origin_window_frame: window_frame,
                verified_source_sequence: 7,
                verified_observed_ms: 1_000,
            },
            hidden: ObservationFact::Known { value: false },
            minimized: ObservationFact::Known { value: false },
            target_context_hash: String::new(),
        };
        source.target_context_hash = source
            .computed_target_context_hash_for(&app.bundle_id, app.pid)
            .unwrap();
        let target_context_hash = source.target_context_hash.clone();
        let scope = BindingProofScope::BackgroundAx {
            target_context_hash: target_context_hash.clone(),
        };
        let witness = DeviceContextWitness::BackgroundAxV1 {
            app: app.clone(),
            window,
            root: root.clone(),
            target: Box::new(DeviceContextTarget {
                object: target.clone(),
                group: Some("actions".into()),
                scroll: None,
            }),
            target_enabled: true,
            target_action_names: vec!["AXPress".into()],
            ancestry: vec![root, group],
            sibling_parent_ancestry_index: 1,
            siblings: vec![target],
            target_sibling_index: 0,
            nearest_labelled_group_ancestry_index: Some(1),
            source: Box::new(source),
            target_context_hash,
        };
        let instance = ObjectRef::new("save-button").unwrap();
        let fingerprint = InstanceFingerprint {
            app: app.bundle_id.clone(),
            app_pid: app.pid,
            window_id: "w1".into(),
            ancestor_roles: vec!["AXWindow".into(), "AXGroup".into()],
            group: Some("actions".into()),
            role: "AXButton".into(),
            subrole: None,
            label: "save".into(),
            sibling_index: 0,
            frame: [20, 20, 80, 30],
            scroll_container: None,
        };
        let display_frame = rect(20.0, 20.0, 80.0, 30.0);
        let continuity_kind = ObjectIdentityContinuity::NotificationCovered;
        let context_digest = background_ax_context_digest(
            &instance,
            continuity_kind,
            &scope,
            &fingerprint,
            display_frame,
            &witness,
        )
        .unwrap();
        BindingProof {
            binding: BindingRef::new("save-binding").unwrap(),
            instance,
            snapshot_id: RequestId::new("snapshot-7").unwrap(),
            environment_revision: 2,
            layer_revision: 4,
            fingerprint,
            display_frame,
            continuity: BindingContinuityProof {
                kind: continuity_kind,
                source_sequence: 7,
                context_digest,
                scope,
            },
            device_context: witness,
            menu_scope: None,
            focus_scope: None,
        }
    }

    fn background_dependencies() -> BTreeMap<String, u64> {
        BTreeMap::from([
            ("authorization".into(), 1),
            ("background_scope".into(), 1),
            ("binding".into(), 1),
            ("geometry".into(), 1),
            ("window".into(), 1),
        ])
    }

    #[test]
    fn background_ax_has_a_separate_visible_native_target_proof_domain() {
        let proof = background_proof();
        assert!(proof.validate().is_ok(), "{:?}", proof.validate());
        assert!(proof.supports_background_ax_press());
        assert!(proof.supports_execution_domain(ExecutionDomain::BackgroundAx));
        assert!(!proof.supports_execution_domain(ExecutionDomain::ForegroundNative));
        assert!(!proof.supports_execution_domain(ExecutionDomain::Simulated));

        let mut hidden = proof.device_context.clone();
        if let DeviceContextWitness::BackgroundAxV1 { source, .. } = &mut hidden {
            source.hidden = ObservationFact::Unavailable;
        }
        assert!(hidden.validate().is_ok());
        assert!(!hidden.supports_background_ax_press());

        let mut scroll_target = proof.device_context.clone();
        if let DeviceContextWitness::BackgroundAxV1 { ancestry, .. } = &mut scroll_target {
            ancestry[1].role = "AXScrollArea".into();
        }
        assert!(scroll_target.validate().is_err());

        let mut wrong_scope = proof;
        wrong_scope.continuity.scope = BindingProofScope::WholeStructure;
        assert!(wrong_scope.validate().is_err());
    }

    #[test]
    fn background_ax_derivation_uses_complete_target_context_without_global_focus() {
        let snapshot = background_snapshot();
        snapshot.validate().unwrap();
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "save")
            .unwrap();
        let evidence = derive_background_ax_binding_evidence(&snapshot, target).unwrap();
        assert!(matches!(
            evidence.continuity.scope,
            BindingProofScope::BackgroundAx { .. }
        ));
        let DeviceContextWitness::BackgroundAxV1 {
            source,
            target_context_hash,
            ..
        } = &evidence.device_context
        else {
            panic!("background derive must create the dedicated AX witness");
        };
        assert_eq!(
            source.window.verified_source_sequence,
            snapshot.source_sequence
        );
        assert_eq!(source.window.verified_observed_ms, snapshot.observed_ms);
        assert_eq!(target_context_hash, &source.target_context_hash);
        assert!(evidence.device_context.supports_background_ax_press());

        let mut no_target_context = snapshot.clone();
        no_target_context.target_context = None;
        assert!(
            derive_background_ax_binding_evidence(
                &no_target_context,
                &no_target_context.objects[2]
            )
            .is_err()
        );

        let mut stale_source = snapshot.clone();
        stale_source.source_sequence += 1;
        assert!(
            derive_background_ax_binding_evidence(&stale_source, &stale_source.objects[2]).is_err()
        );

        let mut hidden = snapshot.clone();
        if let Some(super::super::source::TargetContext::NativeWindow { source, .. }) =
            &mut hidden.target_context
        {
            source.hidden = ObservationFact::Unavailable;
        }
        assert!(derive_background_ax_binding_evidence(&hidden, &hidden.objects[2]).is_err());

        let mut incomplete = snapshot.clone();
        incomplete.complete = false;
        incomplete.incomplete_reason = Some("tree budget reached".into());
        assert!(
            derive_background_ax_binding_evidence(&incomplete, &incomplete.objects[2]).is_err()
        );

        let mut scrolled = snapshot.clone();
        scrolled.objects[1].role = "AXScrollArea".into();
        assert!(derive_background_ax_binding_evidence(&scrolled, &scrolled.objects[2]).is_err());
    }

    #[test]
    fn background_ax_observation_and_intent_allow_only_one_exact_ax_press() {
        let proof = background_proof();
        let surface = SurfaceRef {
            id: "main".into(),
            epoch: 1,
        };
        let dependencies = background_dependencies();
        let observation = Observation {
            observation_id: "background-observation".into(),
            session_epoch: 3,
            surface: surface.clone(),
            target_ref: proof.binding.as_str().into(),
            clock_domain: "native/3/monotonic".into(),
            source_sequence: 7,
            observed_ms: 1_000,
            valid_until_ms: 2_000,
            freshness: SourceFreshness::Verified,
            execution_domain: ExecutionDomain::BackgroundAx,
            purpose: ObservationPurpose::BackgroundAxAction,
            binding_proof: Some(proof.clone()),
            owned_document_proof: None,
            dependencies: dependencies.clone(),
        };
        assert!(
            observation.validate().is_ok(),
            "{:?}",
            observation.validate()
        );
        let mut cross_domain_observation = observation.clone();
        cross_domain_observation.execution_domain = ExecutionDomain::ForegroundNative;
        assert!(cross_domain_observation.validate().is_err());
        cross_domain_observation.execution_domain = ExecutionDomain::BackgroundAx;
        cross_domain_observation.purpose = ObservationPurpose::RowSelection;
        assert!(cross_domain_observation.validate().is_err());
        let mut intent = ActionIntent {
            intent_id: "background-intent".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: "task-1".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: 3,
            execution_domain: ExecutionDomain::BackgroundAx,
            permit: PermitRef {
                id: "permit-1".into(),
                generation: 1,
            },
            surface,
            target_ref: proof.binding.as_str().into(),
            binding_fingerprint: Some(proof.fingerprint.digest()),
            binding_proof: Some(proof),
            owned_document_proof: None,
            coordinate_space: CoordinateSpace::TargetObject,
            dependencies,
            evidence_id: observation.observation_id.clone(),
            latest_start_ms: 1_000,
            duration_ms: 100,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::AxAction {
                    action: AxActionKind::Press,
                },
            }],
        };
        assert!(intent.validate_against_observation(&observation).is_ok());
        let mut cross_domain_intent = intent.clone();
        cross_domain_intent.execution_domain = ExecutionDomain::ForegroundNative;
        assert!(
            cross_domain_intent
                .validate_against_observation(&observation)
                .is_err()
        );

        for primitive in [
            Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "changed".into(),
                },
            },
            Primitive::AxAction {
                action: AxActionKind::SetSelectedTrue,
            },
            Primitive::KeyPress {
                key: "Return".into(),
                modifiers: Default::default(),
            },
            Primitive::Scroll { dx: 0, dy: 1 },
        ] {
            intent.steps[0].primitive = primitive;
            assert!(intent.validate().is_err());
        }
        intent.steps[0].primitive = Primitive::AxAction {
            action: AxActionKind::Press,
        };
        intent.coordinate_space = CoordinateSpace::DisplayPoints;
        assert!(intent.validate().is_err());
        intent.coordinate_space = CoordinateSpace::TargetObject;
        intent.dependencies.insert("focus".into(), 1);
        assert!(intent.validate_against_observation(&observation).is_err());
    }
}
