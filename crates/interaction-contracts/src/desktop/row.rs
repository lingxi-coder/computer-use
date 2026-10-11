//! Typed, viewport-scoped contracts for selecting an actual table row.
//!
//! These value-only contracts are deliberately independent of Task plans,
//! device primitives, and generic bindings. A selector names business-visible
//! column values; it never names a private application record key or row index.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::device::SurfaceRef;
use super::ids::{AttemptId, ObjectRef, RequestId};
use super::require_nonempty;
use super::verification::TimeSpan;
use crate::desktop::source::{
    AxEnabledObservation, ObjectIdentityContinuity, Rect, SourceIdentityBoundary,
    StructureSnapshot, TableViewport, TableViewportRow, UiObject, UiValue,
};
#[cfg(test)]
use crate::desktop::source::{ObserverCoverage, SourceState};

const MAX_ROW_CONDITIONS: usize = 32;
const MAX_ROW_TEXT_BYTES: usize = 2_048;
const MAX_SCOPE_TEXT_BYTES: usize = 256;
const MAX_ROW_FRESHNESS_MS: u64 = 10_000;

/// Exact match against one actual accessibility column header and AX value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowColumnCondition {
    #[schemars(length(min = 1, max = 256))]
    pub column_name: String,
    pub value: UiValue,
}

impl RowColumnCondition {
    fn validate(&self) -> Result<(), String> {
        validate_bounded_text(
            "row selector column name",
            &self.column_name,
            MAX_SCOPE_TEXT_BYTES,
        )?;
        validate_ui_value(&self.value)
    }
}

/// Identifies a visible row by application/window/table scope and one or more
/// exact, business-visible column values. Selection always means selected=true.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowSelector {
    #[schemars(length(min = 1, max = 256))]
    pub app_bundle_id: String,
    #[schemars(range(min = 1, max = 2147483647))]
    pub process_id: u32,
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    pub table_instance_ref: ObjectRef,
    #[schemars(length(min = 1, max = 32))]
    pub conditions: Vec<RowColumnCondition>,
}

impl RowSelector {
    pub fn validate(&self) -> Result<(), String> {
        validate_bounded_text(
            "row selector application bundle id",
            &self.app_bundle_id,
            MAX_SCOPE_TEXT_BYTES,
        )?;
        validate_bounded_text(
            "row selector window id",
            &self.window_id,
            MAX_SCOPE_TEXT_BYTES,
        )?;
        if self.process_id == 0 || self.process_id > i32::MAX as u32 {
            return Err("row selector process id must be a supported positive pid".into());
        }
        if self.conditions.is_empty() || self.conditions.len() > MAX_ROW_CONDITIONS {
            return Err("row selector requires 1 to 32 column conditions".into());
        }

        let mut seen = std::collections::BTreeSet::new();
        for condition in &self.conditions {
            condition.validate()?;
            let normalized: String = condition
                .column_name
                .trim()
                .chars()
                .flat_map(char::to_lowercase)
                .collect();
            if !seen.insert(normalized) {
                return Err("row selector contains duplicate column names".into());
            }
        }
        Ok(())
    }
}

/// The only initially defined row input profile sets the actual AXSelected
/// attribute to true. AXPress is intentionally not implied by AXRow role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowSelectionCapabilityKind {
    AxSetSelectedTrueV1,
    /// A wire-compatible explicit rejection value; it never authorizes input.
    Unsupported,
}

/// Source facts that tie one row ref to one current table viewport and read.
/// The required identity boundary and row ref make unknown identity fail closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowSelectionObservation {
    #[schemars(length(min = 1, max = 256))]
    pub app_bundle_id: String,
    #[schemars(range(min = 1, max = 2147483647))]
    pub process_id: u32,
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    pub table_instance_ref: ObjectRef,
    pub row_instance_ref: ObjectRef,
    pub snapshot_id: RequestId,
    pub surface: SurfaceRef,
    #[schemars(range(min = 1))]
    pub session_epoch: u64,
    pub identity_boundary: SourceIdentityBoundary,
    #[schemars(length(min = 1, max = 256))]
    pub clock_domain: String,
    #[schemars(range(min = 1))]
    pub source_sequence: u64,
    #[schemars(range(min = 0))]
    pub observed_ms: u64,
    #[schemars(range(min = 1))]
    pub valid_until_ms: u64,
    pub table_frame: Rect,
    pub viewport_frame: Rect,
    pub row_frame: Rect,
    /// Direct AXEnabled facts for the actual table and target row. These are
    /// separate from generic `UiObject.enabled`, which remains fail-closed.
    pub table_ax_enabled: AxEnabledObservation,
    pub row_ax_enabled: AxEnabledObservation,
    /// `None` stays unknown; it is never coerced to false or true.
    pub selected: Option<bool>,
    /// Direct Source result of IsAttributeSettable(AXSelected); unknown is not
    /// an input capability.
    pub ax_selected_settable: Option<bool>,
}

impl RowSelectionObservation {
    /// Derive the observation from a unique actual row in the current Source
    /// read. The caller supplies its existing evidence expiry; this constructor
    /// does not renew the snapshot or grant any input authority.
    pub fn from_snapshot(
        selector: &RowSelector,
        snapshot: &StructureSnapshot,
        valid_until_ms: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<Self, String> {
        selector.validate()?;
        snapshot.validate()?;
        let viewport = snapshot
            .table_viewports
            .iter()
            .find(|viewport| viewport.table_instance_ref == selector.table_instance_ref)
            .ok_or("row selector table has no actual Source viewport")?;
        let row = unique_matching_row(viewport, selector)?;
        let observation = Self {
            app_bundle_id: snapshot.app.bundle_id.clone(),
            process_id: snapshot.app.pid,
            window_id: selector.window_id.clone(),
            table_instance_ref: viewport.table_instance_ref.clone(),
            row_instance_ref: row
                .instance_ref
                .clone()
                .ok_or("matching row has no actual Source reference")?,
            snapshot_id: snapshot.snapshot_id.clone(),
            surface: snapshot.surface.clone(),
            session_epoch: snapshot.session_epoch,
            identity_boundary: snapshot
                .identity_boundary
                .clone()
                .ok_or("row selection requires a trusted Source identity boundary")?,
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence: snapshot.source_sequence,
            observed_ms: snapshot.observed_ms,
            valid_until_ms,
            table_frame: viewport.table_frame,
            viewport_frame: viewport.viewport_frame,
            row_frame: row.frame,
            table_ax_enabled: viewport.ax_enabled,
            row_ax_enabled: row.ax_enabled,
            selected: row.selected,
            ax_selected_settable: row.ax_selected_settable,
        };
        observation.validate_against_snapshot(selector, snapshot, now_ms, max_age_ms)?;
        Ok(observation)
    }

    /// Validate the clock bounds without claiming that the row is selected.
    pub fn validate_freshness(&self, now_ms: u64, max_age_ms: u64) -> Result<(), String> {
        if max_age_ms == 0
            || max_age_ms > MAX_ROW_FRESHNESS_MS
            || self.session_epoch == 0
            || self.surface.epoch == 0
            || self.source_sequence == 0
            || self.valid_until_ms <= self.observed_ms
            || self.valid_until_ms - self.observed_ms > MAX_ROW_FRESHNESS_MS
            || now_ms < self.observed_ms
            || now_ms >= self.valid_until_ms
            || now_ms - self.observed_ms >= max_age_ms
        {
            return Err(
                "row source evidence is stale, future-dated, or outside bounded freshness".into(),
            );
        }
        self.identity_boundary.validate()
    }

    /// Bind this claim to an exact Source snapshot and a unique visible row.
    /// `StructureSnapshot::complete` is intentionally not promoted: only the
    /// actual row/cell viewport must have complete local coverage here.
    pub fn validate_against_snapshot(
        &self,
        selector: &RowSelector,
        snapshot: &StructureSnapshot,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        selector.validate()?;
        snapshot.validate()?;
        self.validate_freshness(now_ms, max_age_ms)?;
        if self.app_bundle_id != selector.app_bundle_id
            || self.process_id != selector.process_id
            || self.window_id != selector.window_id
            || self.table_instance_ref != selector.table_instance_ref
        {
            return Err(
                "row observation does not match the selector application/window/table scope".into(),
            );
        }
        let snapshot_identity = snapshot
            .identity_boundary
            .as_ref()
            .ok_or("row selection requires a trusted Source identity boundary")?;
        if self.snapshot_id != snapshot.snapshot_id
            || self.session_epoch != snapshot.session_epoch
            || self.surface != snapshot.surface
            || self.identity_boundary != *snapshot_identity
            || self.clock_domain != snapshot.clock_domain
            || self.source_sequence != snapshot.source_sequence
            || self.observed_ms != snapshot.observed_ms
            || snapshot.session_epoch == 0
            || snapshot.source_sequence == 0
            || snapshot.app.bundle_id != selector.app_bundle_id
            || snapshot.app.pid != selector.process_id
            || !matches!(
                &snapshot.global_focus.app,
                crate::desktop::source::ObservationFact::Known { value } if value == &snapshot.app
            )
            || snapshot.global_focus.known_window_id() != Some(selector.window_id.as_str())
        {
            return Err(
                "row observation does not match the current snapshot, pid, window, or epoch".into(),
            );
        }

        let mut windows = snapshot
            .windows
            .iter()
            .filter(|window| window.window_id == selector.window_id);
        let window = windows
            .next()
            .ok_or("row selector window is absent from the source snapshot")?;
        if windows.next().is_some() || window.owner_bundle_id != selector.app_bundle_id {
            return Err(
                "row selector window is duplicated or belongs to another application".into(),
            );
        }

        let mut viewports = snapshot
            .table_viewports
            .iter()
            .filter(|viewport| viewport.table_instance_ref == selector.table_instance_ref);
        let viewport = viewports
            .next()
            .ok_or("row selector table has no actual Source viewport")?;
        if viewports.next().is_some() {
            return Err("row selector table has duplicate Source viewports".into());
        }
        validate_viewport_scope(viewport, selector, snapshot, window.frame)?;

        let matching_rows = unique_matching_row(viewport, selector)?;
        if matching_rows.instance_ref.as_ref() != Some(&self.row_instance_ref)
            || matching_rows.frame != self.row_frame
            || viewport.table_frame != self.table_frame
            || viewport.viewport_frame != self.viewport_frame
            || viewport.ax_enabled != self.table_ax_enabled
            || matching_rows.ax_enabled != self.row_ax_enabled
            || matching_rows.selected != self.selected
            || matching_rows.ax_selected_settable != self.ax_selected_settable
        {
            return Err("row observation does not describe the unique matching Source row".into());
        }

        let row_objects: Vec<&UiObject> = snapshot
            .objects
            .iter()
            .filter(|object| object.instance_ref == self.row_instance_ref)
            .collect();
        if row_objects.len() != 1 {
            return Err("row reference must resolve to exactly one actual Source object".into());
        }
        let row_object = row_objects[0];
        if row_object.role != "AXRow"
            || row_object.window_id != selector.window_id
            || row_object.parent.as_ref() != Some(&selector.table_instance_ref)
            || row_object.frame != self.row_frame
        {
            return Err("row reference does not resolve to the actual scoped AXRow".into());
        }
        Ok(())
    }
}

/// An explicit, single-row input capability plus the Source proof it is scoped
/// to. This declaration alone does not grant a native device permit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowSelectionCapability {
    pub kind: RowSelectionCapabilityKind,
    pub selector: RowSelector,
    pub source: RowSelectionObservation,
}

impl RowSelectionCapability {
    pub fn validate_at(
        &self,
        snapshot: &StructureSnapshot,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if self.kind != RowSelectionCapabilityKind::AxSetSelectedTrueV1 {
            return Err("unsupported row selection capability".into());
        }
        if self.source.ax_selected_settable != Some(true) {
            return Err("AXSelected=true is not explicitly reported settable for this row".into());
        }
        if self.source.selected != Some(false) {
            return Err("AXSelected=true capability requires an explicitly unselected row".into());
        }
        if !self.source.table_ax_enabled.permits_row_selection()
            || !self.source.row_ax_enabled.permits_row_selection()
        {
            return Err(
                "row selection requires AXEnabled true or explicitly unsupported on both table and row".into(),
            );
        }
        self.source
            .validate_against_snapshot(&self.selector, snapshot, now_ms, max_age_ms)
    }
}

/// Post-input evidence for the MVP row-selected=true goal. Both reads must
/// preserve the same trusted row identity; unknown selection is not success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowSelectedEffectEvidence {
    pub attempt_id: AttemptId,
    #[schemars(length(min = 1, max = 256))]
    pub intent_id: String,
    pub capability: RowSelectionCapability,
    pub after: RowSelectionObservation,
    pub same_row: bool,
    pub input_span: TimeSpan,
}

impl RowSelectedEffectEvidence {
    pub fn validate_at(
        &self,
        before_snapshot: &StructureSnapshot,
        after_snapshot: &StructureSnapshot,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        validate_bounded_text(
            "row-selection intent id",
            &self.intent_id,
            MAX_SCOPE_TEXT_BYTES,
        )?;
        let before = &self.capability.source;
        if self.input_span.to_ms < self.input_span.from_ms
            || self.input_span.from_ms < before.observed_ms
            || self.input_span.to_ms > now_ms
            || self.input_span.to_ms >= self.after.observed_ms
            || before.clock_domain != self.after.clock_domain
        {
            return Err(
                "row input span is future-dated, reversed, or outside the source read interval"
                    .into(),
            );
        }
        self.capability
            .validate_at(before_snapshot, self.input_span.from_ms, max_age_ms)?;
        before.validate_freshness(self.input_span.to_ms, max_age_ms)?;
        self.after.validate_against_snapshot(
            &self.capability.selector,
            after_snapshot,
            now_ms,
            max_age_ms,
        )?;
        require_row_identity_continuity(before, before_snapshot)?;
        require_row_identity_continuity(&self.after, after_snapshot)?;

        if !self.same_row
            || before.row_instance_ref != self.after.row_instance_ref
            || before.table_instance_ref != self.after.table_instance_ref
            || before.process_id != self.after.process_id
            || before.app_bundle_id != self.after.app_bundle_id
            || before.window_id != self.after.window_id
            || before.identity_boundary != self.after.identity_boundary
            || before.session_epoch != self.after.session_epoch
            || before.surface != self.after.surface
            || before.snapshot_id == self.after.snapshot_id
            || self.after.source_sequence <= before.source_sequence
            || self.after.observed_ms <= before.observed_ms
            || self.after.selected != Some(true)
        {
            return Err(
                "RowSelected evidence lacks a fresh post-input read of the same selected row"
                    .into(),
            );
        }
        Ok(())
    }
}

fn require_row_identity_continuity(
    observation: &RowSelectionObservation,
    snapshot: &StructureSnapshot,
) -> Result<(), String> {
    for reference in [
        &observation.table_instance_ref,
        &observation.row_instance_ref,
    ] {
        let object = snapshot
            .objects
            .iter()
            .find(|object| &object.instance_ref == reference)
            .ok_or("row effect identity reference is absent from Source")?;
        if object.identity_continuity != ObjectIdentityContinuity::NotificationCovered {
            return Err(
                "same-row effect evidence requires notification-covered table and row identity"
                    .into(),
            );
        }
    }
    Ok(())
}

fn validate_viewport_scope(
    viewport: &TableViewport,
    selector: &RowSelector,
    snapshot: &StructureSnapshot,
    window_frame: Rect,
) -> Result<(), String> {
    if viewport.process_id != selector.process_id
        || viewport.process_id != snapshot.app.pid
        || !viewport.visible_rows_complete
        || !viewport.columns_complete
        || viewport.visible_rows.is_empty()
        || viewport.visible_rows.len() > 256
        || !viewport.table_frame.valid_bounded()
        || !viewport.viewport_frame.valid_bounded()
        || !viewport.table_frame.intersects(&viewport.viewport_frame)
        || !window_frame.contains_rect(&viewport.viewport_frame)
    {
        return Err(
            "row selection requires a complete, current viewport inside its scoped window".into(),
        );
    }

    let tables: Vec<&UiObject> = snapshot
        .objects
        .iter()
        .filter(|object| object.instance_ref == selector.table_instance_ref)
        .collect();
    let scrolls: Vec<&UiObject> = snapshot
        .objects
        .iter()
        .filter(|object| object.instance_ref == viewport.scroll_container_instance_ref)
        .collect();
    if tables.len() != 1
        || scrolls.len() != 1
        || tables[0].role != "AXTable"
        || scrolls[0].role != "AXScrollArea"
        || tables[0].window_id != selector.window_id
        || scrolls[0].window_id != selector.window_id
        || tables[0].frame != viewport.table_frame
        || !scrolls[0].frame.contains_rect(&viewport.viewport_frame)
    {
        return Err("table viewport refs or geometry do not match actual Source objects".into());
    }
    Ok(())
}

fn unique_matching_row<'a>(
    viewport: &'a TableViewport,
    selector: &RowSelector,
) -> Result<&'a TableViewportRow, String> {
    let mut matched = None;
    for row in &viewport.visible_rows {
        if !row.cells_complete
            || row.parent_instance_ref != selector.table_instance_ref
            || !row.frame.valid_bounded()
            || !viewport.table_frame.contains_rect(&row.frame)
            || !row.frame.intersects(&viewport.viewport_frame)
        {
            return Err("visible row/cell coverage is incomplete or malformed".into());
        }
        if row_matches_selector(row, selector)? {
            if matched.is_some() {
                return Err("row selector matches more than one visible row".into());
            }
            if row.instance_ref.is_none() || !viewport.viewport_frame.contains_rect(&row.frame) {
                return Err(
                    "matching row identity is missing or its frame is outside the viewport".into(),
                );
            }
            matched = Some(row);
        }
    }
    matched.ok_or_else(|| "row selector does not match an actual visible row".into())
}

fn row_matches_selector(row: &TableViewportRow, selector: &RowSelector) -> Result<bool, String> {
    for condition in &selector.conditions {
        let cells: Vec<_> = row
            .cells
            .iter()
            .filter(|cell| cell.column_header.as_deref() == Some(condition.column_name.as_str()))
            .collect();
        match cells.as_slice() {
            [] => return Ok(false),
            [cell] => match cell.value.as_ref() {
                Some(value) if ui_values_equal(value, &condition.value) => {}
                Some(_) => return Ok(false),
                None => {
                    return Err("a selector column value is unknown in the visible viewport".into());
                }
            },
            _ => return Err("a selector column header is ambiguous in the visible row".into()),
        }
    }
    Ok(true)
}

fn ui_values_equal(actual: &UiValue, expected: &UiValue) -> bool {
    match (actual, expected) {
        (UiValue::Number(a), UiValue::Number(b)) => a == b,
        _ => actual == expected,
    }
}

fn validate_ui_value(value: &UiValue) -> Result<(), String> {
    match value {
        UiValue::Bool(_) => Ok(()),
        UiValue::Text(text) => {
            validate_bounded_text("row selector value", text, MAX_ROW_TEXT_BYTES)
        }
        UiValue::Number(number) if number.is_finite() && number.abs() <= 1.0e12 => Ok(()),
        UiValue::Number(_) => Err("row selector number must be finite and bounded".into()),
    }
}

fn validate_bounded_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    require_nonempty(field, value)?;
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds its byte bound"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::source::{
        AppIdentity, LayerKind, TableViewportCell, TableViewportColumn, WindowInfo,
    };

    const OBSERVED_MS: u64 = 1_000;
    const NOW_MS: u64 = 1_100;
    const MAX_AGE_MS: u64 = 1_000;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn object(
        reference: &str,
        parent: Option<&str>,
        role: &str,
        window_id: &str,
        frame: Rect,
    ) -> UiObject {
        UiObject {
            instance_ref: ObjectRef::new(reference).unwrap(),
            parent: parent.map(|value| ObjectRef::new(value).unwrap()),
            window_id: window_id.into(),
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

    fn snapshot(
        snapshot_id: &str,
        sequence: u64,
        observed_ms: u64,
        selected: Option<bool>,
    ) -> StructureSnapshot {
        let table_frame = rect(0.0, 0.0, 300.0, 200.0);
        let viewport_frame = rect(0.0, 0.0, 300.0, 100.0);
        let row_one_frame = rect(0.0, 0.0, 300.0, 30.0);
        let row_two_frame = rect(0.0, 30.0, 300.0, 30.0);
        let mut result = StructureSnapshot {
            snapshot_id: RequestId::new(snapshot_id).unwrap(),
            session_epoch: 7,
            surface: SurfaceRef {
                id: "display-main".into(),
                epoch: 4,
            },
            clock_domain: "host/monotonic".into(),
            identity_boundary: Some(SourceIdentityBoundary {
                namespace: "00112233-4455-6677-8899-aabbccddeeff".into(),
                generation: 3,
            }),
            frame_ref: None,
            source_sequence: sequence,
            observed_ms,
            app: AppIdentity {
                bundle_id: "com.example.records".into(),
                name: "Records".into(),
                pid: 42,
            },
            windows: vec![WindowInfo {
                window_id: "window-main".into(),
                title: Some("Records".into()),
                frame: rect(0.0, 0.0, 400.0, 300.0),
                z_index: 0,
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: "com.example.records".into(),
            }],
            global_focus: crate::desktop::source::GlobalFocusObservation {
                app: crate::desktop::source::ObservationFact::Known {
                    value: AppIdentity {
                        bundle_id: "com.example.records".into(),
                        name: "Records".into(),
                        pid: 42,
                    },
                },
                window_id: crate::desktop::source::ObservationFact::Known {
                    value: "window-main".into(),
                },
            },
            target_context: None,
            complete: false,
            incomplete_reason: Some("bounded tree read".into()),
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
            table_scope_search: None,
            table_viewports: vec![],
            owned_document_evidence: None,
        };
        result.objects = vec![
            object(
                "window-object",
                None,
                "AXWindow",
                "window-main",
                rect(0.0, 0.0, 400.0, 300.0),
            ),
            object(
                "scroll-object",
                Some("window-object"),
                "AXScrollArea",
                "window-main",
                viewport_frame,
            ),
            object(
                "table-object",
                Some("scroll-object"),
                "AXTable",
                "window-main",
                table_frame,
            ),
            object(
                "row-one",
                Some("table-object"),
                "AXRow",
                "window-main",
                row_one_frame,
            ),
            object(
                "row-two",
                Some("table-object"),
                "AXRow",
                "window-main",
                row_two_frame,
            ),
        ];
        result.observer_coverage.covered_objects = result.objects.len() as u32;
        result.table_viewports = vec![TableViewport {
            table_instance_ref: ObjectRef::new("table-object").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll-object").unwrap(),
            process_id: 42,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame,
            visible_rows_complete: true,
            visible_rows: vec![
                TableViewportRow {
                    instance_ref: Some(ObjectRef::new("row-one").unwrap()),
                    parent_instance_ref: ObjectRef::new("table-object").unwrap(),
                    frame: row_one_frame,
                    ax_enabled: AxEnabledObservation::Enabled,
                    ax_index: None,
                    accessibility_identifier: None,
                    selected,
                    ax_selected_settable: Some(true),
                    cells_complete: true,
                    cells: vec![
                        cell("Owner", "Mina", "row-one"),
                        cell("Status", "Review", "row-one"),
                    ],
                },
                TableViewportRow {
                    instance_ref: Some(ObjectRef::new("row-two").unwrap()),
                    parent_instance_ref: ObjectRef::new("table-object").unwrap(),
                    frame: row_two_frame,
                    ax_enabled: AxEnabledObservation::Enabled,
                    ax_index: None,
                    accessibility_identifier: None,
                    selected: Some(false),
                    ax_selected_settable: Some(true),
                    cells_complete: true,
                    cells: vec![
                        cell("Owner", "Mina", "row-two"),
                        cell("Status", "Draft", "row-two"),
                    ],
                },
            ],
            columns_complete: true,
            columns: vec![column("Owner"), column("Status")],
        }];
        result
    }

    fn cell(header: &str, text: &str, row_ref: &str) -> TableViewportCell {
        let y = if row_ref == "row-one" { 0.0 } else { 30.0 };
        TableViewportCell {
            instance_ref: None,
            parent_instance_ref: Some(ObjectRef::new(row_ref).unwrap()),
            frame: rect(0.0, y, 150.0, 30.0),
            column_instance_ref: None,
            header_instance_ref: None,
            column_header: Some(header.into()),
            row_ax_index: None,
            column_ax_index: None,
            value: Some(UiValue::Text(text.into())),
        }
    }

    fn column(header: &str) -> TableViewportColumn {
        TableViewportColumn {
            instance_ref: None,
            parent_instance_ref: ObjectRef::new("table-object").unwrap(),
            frame: rect(0.0, 0.0, 150.0, 200.0),
            header_instance_ref: None,
            header: Some(header.into()),
            ax_index: None,
        }
    }

    fn selector() -> RowSelector {
        RowSelector {
            app_bundle_id: "com.example.records".into(),
            process_id: 42,
            window_id: "window-main".into(),
            table_instance_ref: ObjectRef::new("table-object").unwrap(),
            conditions: vec![
                RowColumnCondition {
                    column_name: "Owner".into(),
                    value: UiValue::Text("Mina".into()),
                },
                RowColumnCondition {
                    column_name: "Status".into(),
                    value: UiValue::Text("Review".into()),
                },
            ],
        }
    }

    fn observation(
        snapshot: &StructureSnapshot,
        row_id: &str,
        selected: Option<bool>,
    ) -> RowSelectionObservation {
        let ax_selected_settable = snapshot.table_viewports[0]
            .visible_rows
            .iter()
            .find(|row| {
                row.instance_ref
                    .as_ref()
                    .is_some_and(|reference| reference.as_str() == row_id)
            })
            .and_then(|row| row.ax_selected_settable);
        RowSelectionObservation {
            app_bundle_id: snapshot.app.bundle_id.clone(),
            process_id: snapshot.app.pid,
            window_id: "window-main".into(),
            table_instance_ref: ObjectRef::new("table-object").unwrap(),
            row_instance_ref: ObjectRef::new(row_id).unwrap(),
            snapshot_id: snapshot.snapshot_id.clone(),
            surface: snapshot.surface.clone(),
            session_epoch: snapshot.session_epoch,
            identity_boundary: snapshot.identity_boundary.clone().unwrap(),
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence: snapshot.source_sequence,
            observed_ms: snapshot.observed_ms,
            valid_until_ms: snapshot.observed_ms + 2_000,
            table_frame: rect(0.0, 0.0, 300.0, 200.0),
            viewport_frame: rect(0.0, 0.0, 300.0, 100.0),
            row_frame: if row_id == "row-one" {
                rect(0.0, 0.0, 300.0, 30.0)
            } else {
                rect(0.0, 30.0, 300.0, 30.0)
            },
            table_ax_enabled: snapshot.table_viewports[0].ax_enabled,
            row_ax_enabled: snapshot.table_viewports[0]
                .visible_rows
                .iter()
                .find(|row| {
                    row.instance_ref
                        .as_ref()
                        .is_some_and(|reference| reference.as_str() == row_id)
                })
                .unwrap()
                .ax_enabled,
            selected,
            ax_selected_settable,
        }
    }

    fn capability(snapshot: &StructureSnapshot) -> RowSelectionCapability {
        RowSelectionCapability {
            kind: RowSelectionCapabilityKind::AxSetSelectedTrueV1,
            selector: selector(),
            source: observation(
                snapshot,
                "row-one",
                snapshot.table_viewports[0].visible_rows[0].selected,
            ),
        }
    }

    #[test]
    fn selector_requires_unique_bounded_exact_column_conditions() {
        assert!(selector().validate().is_ok());
        let mut duplicate = selector();
        duplicate.conditions[1].column_name = " owner ".into();
        assert!(duplicate.validate().is_err());

        let mut empty = selector();
        empty.conditions.clear();
        assert!(empty.validate().is_err());

        let mut unbounded_value = selector();
        unbounded_value.conditions[0].value = UiValue::Number(f64::INFINITY);
        assert!(unbounded_value.validate().is_err());
    }

    #[test]
    fn observation_is_derived_from_current_unique_row_without_promoting_tree_coverage() {
        let mut source = snapshot("current-read", 7, OBSERVED_MS, None);
        source.table_viewports[0].visible_rows[0].ax_index = Some(42);
        source.table_viewports[0].visible_rows[0].ax_selected_settable = None;
        let observed = RowSelectionObservation::from_snapshot(
            &selector(),
            &source,
            OBSERVED_MS + 500,
            NOW_MS,
            MAX_AGE_MS,
        )
        .unwrap();
        assert_eq!(observed.row_instance_ref.as_str(), "row-one");
        assert_eq!(observed.snapshot_id, source.snapshot_id);
        assert_eq!(observed.source_sequence, 7);
        assert_eq!(observed.observed_ms, OBSERVED_MS);
        assert_eq!(observed.valid_until_ms, OBSERVED_MS + 500);
        assert_eq!(observed.selected, None);
        assert_eq!(observed.ax_selected_settable, None);
        assert!(!source.complete);

        assert!(
            RowSelectionObservation::from_snapshot(
                &selector(),
                &source,
                NOW_MS,
                NOW_MS,
                MAX_AGE_MS,
            )
            .is_err(),
            "caller expiry must not be renewed"
        );
        source.table_viewports[0].visible_rows[1].cells[1].value =
            Some(UiValue::Text("Review".into()));
        assert!(
            RowSelectionObservation::from_snapshot(
                &selector(),
                &source,
                OBSERVED_MS + 500,
                NOW_MS,
                MAX_AGE_MS,
            )
            .is_err(),
            "a current duplicate must not produce an observation"
        );
    }

    #[test]
    fn observation_derivation_rejects_missing_identity_and_incomplete_viewport() {
        for missing_identity in [true, false] {
            let mut source = snapshot("invalid-read", 1, OBSERVED_MS, Some(false));
            if missing_identity {
                source.identity_boundary = None;
            } else {
                source.table_viewports[0].visible_rows_complete = false;
            }
            assert!(
                RowSelectionObservation::from_snapshot(
                    &selector(),
                    &source,
                    OBSERVED_MS + 500,
                    NOW_MS,
                    MAX_AGE_MS,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn capability_requires_supported_fresh_scoped_unique_visible_row() {
        let snapshot = snapshot("snap-before", 1, OBSERVED_MS, Some(false));
        let valid = capability(&snapshot);
        assert!(valid.validate_at(&snapshot, NOW_MS, MAX_AGE_MS).is_ok());

        let mut unsupported = valid.clone();
        unsupported.kind = RowSelectionCapabilityKind::Unsupported;
        assert!(
            unsupported
                .validate_at(&snapshot, NOW_MS, MAX_AGE_MS)
                .is_err()
        );

        let mut unknown_settable = valid.clone();
        unknown_settable.source.ax_selected_settable = None;
        assert!(
            unknown_settable
                .validate_at(&snapshot, NOW_MS, MAX_AGE_MS)
                .is_err()
        );

        let mut source_unknown_settable = snapshot.clone();
        source_unknown_settable.table_viewports[0].visible_rows[0].ax_selected_settable = None;
        let mut source_unknown_capability = capability(&source_unknown_settable);
        assert!(
            source_unknown_capability
                .validate_at(&source_unknown_settable, NOW_MS, MAX_AGE_MS)
                .is_err()
        );
        source_unknown_capability.source.ax_selected_settable = Some(true);
        assert!(
            source_unknown_capability
                .validate_at(&source_unknown_settable, NOW_MS, MAX_AGE_MS)
                .is_err(),
            "a caller cannot claim settable when Source observed unknown"
        );

        let mut stale = valid.clone();
        stale.source.valid_until_ms = NOW_MS;
        assert!(stale.validate_at(&snapshot, NOW_MS, MAX_AGE_MS).is_err());

        let mut missing_row = valid.clone();
        missing_row.source.row_instance_ref = ObjectRef::new("row-missing").unwrap();
        assert!(
            missing_row
                .validate_at(&snapshot, NOW_MS, MAX_AGE_MS)
                .is_err()
        );

        let mut wrong_pid = snapshot.clone();
        wrong_pid.app.pid = 43;
        assert!(valid.validate_at(&wrong_pid, NOW_MS, MAX_AGE_MS).is_err());

        let mut wrong_window = snapshot.clone();
        wrong_window.global_focus.window_id = crate::desktop::source::ObservationFact::Known {
            value: "window-other".into(),
        };
        assert!(
            valid
                .validate_at(&wrong_window, NOW_MS, MAX_AGE_MS)
                .is_err()
        );

        let mut wrong_snapshot = valid.clone();
        wrong_snapshot.source.snapshot_id = RequestId::new("stale-snapshot").unwrap();
        assert!(
            wrong_snapshot
                .validate_at(&snapshot, NOW_MS, MAX_AGE_MS)
                .is_err()
        );

        let mut partial = snapshot.clone();
        partial.table_viewports[0].visible_rows_complete = false;
        assert!(valid.validate_at(&partial, NOW_MS, MAX_AGE_MS).is_err());

        let mut outside = snapshot.clone();
        outside.table_viewports[0].visible_rows[0].frame = rect(0.0, 110.0, 300.0, 30.0);
        outside.objects[3].frame = rect(0.0, 110.0, 300.0, 30.0);
        let mut outside_observation = observation(&outside, "row-one", None);
        outside_observation.row_frame = rect(0.0, 110.0, 300.0, 30.0);
        let outside_capability = RowSelectionCapability {
            source: outside_observation,
            ..valid
        };
        assert!(
            outside_capability
                .validate_at(&outside, NOW_MS, MAX_AGE_MS)
                .is_err()
        );
    }

    #[test]
    fn row_input_requires_positive_axenabled_or_exact_unsupported_and_keeps_readonly_facts() {
        let mut unsupported = snapshot("unsupported-enabled", 1, OBSERVED_MS, Some(false));
        unsupported.table_viewports[0].ax_enabled = AxEnabledObservation::AttributeUnsupported;
        unsupported.table_viewports[0].visible_rows[0].ax_enabled =
            AxEnabledObservation::AttributeUnsupported;
        for object in &mut unsupported.objects {
            if matches!(object.instance_ref.as_str(), "table-object" | "row-one") {
                object.enabled = false;
            }
        }
        assert!(unsupported.validate().is_ok());
        let valid_unsupported = capability(&unsupported);
        assert!(
            valid_unsupported
                .validate_at(&unsupported, NOW_MS, MAX_AGE_MS)
                .is_ok()
        );

        for unavailable in [
            AxEnabledObservation::Disabled,
            AxEnabledObservation::Unavailable,
        ] {
            let mut source = snapshot("unavailable-enabled", 1, OBSERVED_MS, Some(false));
            source.table_viewports[0].ax_enabled = unavailable;
            source.table_viewports[0].visible_rows[0].ax_enabled = unavailable;
            for object in &mut source.objects {
                if matches!(object.instance_ref.as_str(), "table-object" | "row-one") {
                    object.enabled = false;
                }
            }
            assert!(source.validate().is_ok());
            let observation = observation(&source, "row-one", Some(false));
            assert!(
                observation
                    .validate_against_snapshot(&selector(), &source, NOW_MS, MAX_AGE_MS,)
                    .is_ok(),
                "disabled and unavailable Source facts remain observable"
            );
            assert!(
                capability(&source)
                    .validate_at(&source, NOW_MS, MAX_AGE_MS)
                    .is_err(),
                "{unavailable:?} must not authorize the row setter"
            );
        }

        let mut already_selected = snapshot("readonly-enabled", 1, OBSERVED_MS, Some(true));
        already_selected.table_viewports[0].ax_enabled = AxEnabledObservation::Unavailable;
        already_selected.table_viewports[0].visible_rows[0].ax_enabled =
            AxEnabledObservation::Unavailable;
        for object in &mut already_selected.objects {
            if matches!(object.instance_ref.as_str(), "table-object" | "row-one") {
                object.enabled = false;
            }
        }
        let readonly = observation(&already_selected, "row-one", Some(true));
        assert!(
            readonly
                .validate_against_snapshot(&selector(), &already_selected, NOW_MS, MAX_AGE_MS,)
                .is_ok(),
            "already-selected semantic evidence stays read-only"
        );
        assert!(
            capability(&already_selected)
                .validate_at(&already_selected, NOW_MS, MAX_AGE_MS)
                .is_err()
        );
    }

    #[test]
    fn row_selected_effect_requires_same_identity_and_fresh_true_read_after_input() {
        let before = snapshot("snap-before", 1, 1_000, Some(false));
        let after = snapshot("snap-after", 2, 1_200, Some(true));
        let mut effect = RowSelectedEffectEvidence {
            attempt_id: AttemptId::new("attempt-1").unwrap(),
            intent_id: "intent-1".into(),
            capability: capability(&before),
            after: observation(&after, "row-one", Some(true)),
            same_row: true,
            input_span: TimeSpan {
                from_ms: 1_050,
                to_ms: 1_100,
            },
        };
        assert!(
            effect
                .validate_at(&before, &after, 1_300, MAX_AGE_MS)
                .is_ok()
        );

        effect.after.selected = None;
        assert!(
            effect
                .validate_at(&before, &after, 1_300, MAX_AGE_MS)
                .is_err()
        );
        effect.after.selected = Some(true);
        effect.after.row_instance_ref = ObjectRef::new("row-two").unwrap();
        effect.after.row_frame = rect(0.0, 30.0, 300.0, 30.0);
        assert!(
            effect
                .validate_at(&before, &after, 1_300, MAX_AGE_MS)
                .is_err()
        );
    }

    #[test]
    fn row_input_freshness_uses_submission_time_not_effect_completion_time() {
        let before = snapshot("snap-before-zero", 1, 0, Some(false));
        let after = snapshot("snap-after-five-hundred", 2, 500, Some(true));
        let mut effect = RowSelectedEffectEvidence {
            attempt_id: AttemptId::new("attempt-time-boundary").unwrap(),
            intent_id: "intent-time-boundary".into(),
            capability: capability(&before),
            after: observation(&after, "row-one", Some(true)),
            same_row: true,
            input_span: TimeSpan {
                from_ms: 10,
                to_ms: 20,
            },
        };

        assert!(effect.validate_at(&before, &after, 510, 100).is_ok());

        effect.input_span = TimeSpan {
            from_ms: 100,
            to_ms: 110,
        };
        assert!(effect.validate_at(&before, &after, 510, 100).is_err());

        effect.input_span = TimeSpan {
            from_ms: 10,
            to_ms: 511,
        };
        assert!(effect.validate_at(&before, &after, 510, 100).is_err());

        effect.input_span = TimeSpan {
            from_ms: 20,
            to_ms: 10,
        };
        assert!(effect.validate_at(&before, &after, 510, 100).is_err());
    }

    #[test]
    fn same_reference_and_same_row_flag_do_not_promote_read_scoped_identity() {
        for reference in ["table-object", "row-one"] {
            for change_before in [true, false] {
                let mut before = snapshot("before-read", 1, 1_000, Some(false));
                let mut after = snapshot("after-read", 2, 1_200, Some(true));
                let changed = if change_before {
                    &mut before
                } else {
                    &mut after
                };
                changed
                    .objects
                    .iter_mut()
                    .find(|object| object.instance_ref.as_str() == reference)
                    .unwrap()
                    .identity_continuity = ObjectIdentityContinuity::ReadScoped;
                changed.observer_coverage =
                    ObserverCoverage::for_projected_objects(&changed.objects);
                assert!(changed.validate().is_ok());
                let effect = RowSelectedEffectEvidence {
                    attempt_id: AttemptId::new("attempt-read-scoped").unwrap(),
                    intent_id: "intent-read-scoped".into(),
                    capability: capability(&before),
                    after: observation(&after, "row-one", Some(true)),
                    same_row: true,
                    input_span: TimeSpan {
                        from_ms: 1_050,
                        to_ms: 1_100,
                    },
                };
                assert!(
                    effect
                        .validate_at(&before, &after, 1_300, MAX_AGE_MS)
                        .unwrap_err()
                        .contains("notification-covered"),
                    "{reference} must have explicit cross-read continuity in both reads"
                );
            }
        }
    }

    #[test]
    fn wire_shapes_reject_unknown_fields() {
        let mut value = serde_json::to_value(selector()).unwrap();
        value["row_index"] = serde_json::json!(0);
        assert!(serde_json::from_value::<RowSelector>(value).is_err());

        let source = snapshot("snap-ax-enabled", 1, OBSERVED_MS, Some(false));
        let mut observation_wire =
            serde_json::to_value(observation(&source, "row-one", Some(false))).unwrap();
        observation_wire
            .as_object_mut()
            .unwrap()
            .remove("table_ax_enabled");
        assert!(serde_json::from_value::<RowSelectionObservation>(observation_wire).is_err());

        let mut capability =
            serde_json::to_value(capability(&snapshot("snap-1", 1, 1_000, None))).unwrap();
        capability["private_record_id"] = serde_json::json!("secret");
        assert!(serde_json::from_value::<RowSelectionCapability>(capability).is_err());
    }
}
