//! What cu-source reports: structure snapshots, source health and frame metadata.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::device::SurfaceRef;
use super::ids::{ObjectRef, RequestId};
use super::require_nonempty;

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
    pub enabled: bool,
    pub focused: bool,
    pub frame: Rect,
    pub actions: Vec<AxActionName>,
    /// Inside the visible range of its scroll container.
    pub in_view: bool,
    /// Title of the nearest labelled group ancestor.
    pub group: Option<String>,
    pub secure: bool,
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

/// One reading of the target application's UI tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StructureSnapshot {
    pub snapshot_id: RequestId,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub clock_domain: String,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub app: AppIdentity,
    pub windows: Vec<WindowInfo>,
    /// The window that receives input. `None` means the source cannot tell,
    /// and nothing that depends on it is admitted.
    pub active_layer: Option<String>,
    /// False when a read timed out or hit its traversal budget. Objects under
    /// an incomplete read are unknown, not absent.
    pub complete: bool,
    pub incomplete_reason: Option<String>,
    pub objects: Vec<UiObject>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Screenshot,
    Ax,
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
}

/// Commands the host may send to the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceCommand {
    /// Read the structure again now.
    RefreshNow,
}

impl StructureSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("clock_domain", &self.clock_domain)?;
        require_nonempty("app.bundle_id", &self.app.bundle_id)?;
        if let Some(layer) = &self.active_layer
            && !self.windows.iter().any(|window| &window.window_id == layer)
        {
            return Err("active_layer must name one of the windows".into());
        }
        if !self.complete && self.incomplete_reason.is_none() {
            return Err("an incomplete snapshot must say why".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for object in &self.objects {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> StructureSnapshot {
        StructureSnapshot {
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
            active_layer: Some("w1".into()),
            complete: true,
            incomplete_reason: None,
            objects: vec![],
        }
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
    fn snapshot_validation_checks_references_and_completeness() {
        assert!(snapshot().validate().is_ok());
        let mut bad = snapshot();
        bad.active_layer = Some("nope".into());
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
}
