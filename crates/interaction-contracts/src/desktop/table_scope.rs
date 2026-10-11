//! Source-owned, bounded census of one exact public table scope.
//!
//! A complete census is an assertion made by the Source producer. Validation
//! here can cross-check its returned candidates against this snapshot, but it
//! cannot establish that the producer actually visited every branch.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ids::ObjectRef;
use super::source::{
    LayerKind, MAX_SOURCE_OBJECTS, Rect, StructureSnapshot, TableViewport, UiObject,
};
use super::task::RowTableScope;

const MAX_SCOPE_PATH: usize = 8;
const MAX_SCOPE_CANDIDATES: usize = 32;
const MAX_SCOPE_HEADERS: usize = 64;
const MAX_SCOPE_TEXT_BYTES: usize = 256;
const MAX_SCOPE_TABLE_VIEWPORTS: usize = 32;

/// Same-read census result for a frozen path and exact header set.
///
/// `Complete` means the Source producer claims its bounded census of the
/// requested scope finished. It does not upgrade the enclosing structure
/// snapshot's global completeness, and this DTO cannot prove that no producer
/// branch was skipped. An incomplete result never grants unique-table
/// authority, even when it contains one candidate for diagnostic purposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableScopeSearch {
    pub requested_scope: RowTableScope,
    #[schemars(range(min = 1, max = 2_147_483_647u32))]
    pub process_id: u32,
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    pub state: TableScopeSearchState,
    #[schemars(length(max = 32))]
    pub candidates: Vec<TableScopeCandidate>,
}

/// Whether Source claims it finished enumerating this exact local scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum TableScopeSearchState {
    /// Zero candidates proves no table was found in this scope; one is unique;
    /// multiple candidates are ambiguous. It says nothing about matching rows.
    Complete,
    /// The scoped census did not finish. The reason is bounded public text.
    Incomplete {
        #[schemars(length(min = 1, max = 256))]
        reason: String,
    },
}

/// One candidate whose actual scroll ancestry and complete header set match
/// the requested scope in the enclosing structure snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableScopeCandidate {
    pub table_instance_ref: ObjectRef,
    pub scroll_container_instance_ref: ObjectRef,
    /// Actual scroll ancestors in canonical outermost-to-nearest order.
    #[schemars(length(min = 1, max = 8))]
    pub scroll_path: Vec<TableScopePathElement>,
    #[schemars(length(min = 1, max = 64))]
    pub column_headers: Vec<String>,
}

/// One actual labelled AXScrollArea on the table's path to its active window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableScopePathElement {
    pub instance_ref: ObjectRef,
    #[schemars(length(min = 1, max = 256))]
    pub label: String,
    pub frame: Rect,
}

impl TableScopeSearch {
    /// Return the sole usable table only when Source claims the scoped census
    /// completed and the result contains exactly one candidate.
    pub fn unique_candidate(&self) -> Option<&TableScopeCandidate> {
        match (&self.state, self.candidates.as_slice()) {
            (TableScopeSearchState::Complete, [candidate]) => Some(candidate),
            _ => None,
        }
    }

    /// Whether a completed census found more than one matching table.
    pub fn is_ambiguous(&self) -> bool {
        matches!(&self.state, TableScopeSearchState::Complete) && self.candidates.len() > 1
    }

    /// Check that this same-read census is internally bounded and every
    /// candidate is backed by the enclosing snapshot's active window, actual
    /// AXTable/AXScrollArea objects, exact parent path, and complete headers.
    ///
    /// This deliberately does not inspect `snapshot.complete`: the global AX
    /// tree may remain partial while a Source-owned local census is complete.
    /// Producer enumeration completeness remains the Source implementation's
    /// responsibility and is not inferable from the returned DTO.
    pub fn validate_against_snapshot(&self, snapshot: &StructureSnapshot) -> Result<(), String> {
        self.requested_scope.validate()?;
        validate_text("table scope window id", &self.window_id)?;
        if self.process_id == 0
            || self.process_id > i32::MAX as u32
            || self.process_id != snapshot.app.pid
            || self.candidates.len() > MAX_SCOPE_CANDIDATES
            || snapshot.objects.len() > MAX_SOURCE_OBJECTS
            || snapshot.table_viewports.len() > MAX_SCOPE_TABLE_VIEWPORTS
        {
            return Err("invalid table scope process or candidate bound".into());
        }
        if let TableScopeSearchState::Incomplete { reason } = &self.state {
            validate_text("table scope incomplete reason", reason)?;
        }

        let complete = matches!(&self.state, TableScopeSearchState::Complete);
        let foreground_matches = matches!(
            &snapshot.global_focus.app,
            crate::desktop::source::ObservationFact::Known { value } if value == &snapshot.app
        ) && snapshot.global_focus.known_window_id()
            == Some(self.window_id.as_str());
        if !foreground_matches {
            if !complete && self.candidates.is_empty() {
                // A loss of foreground can be reported as an incomplete
                // diagnostic for the fixed requested window. It carries no
                // current-window or table authority.
                return Ok(());
            }
            return Err("complete table scope search must name the current active window".into());
        }
        let matching_windows: Vec<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.window_id == self.window_id)
            .collect();
        if matching_windows.len() != 1
            || matching_windows[0].kind != LayerKind::Window
            || matching_windows[0].modal
            || matching_windows[0].owner_bundle_id != snapshot.app.bundle_id
            || !matching_windows[0].frame.valid_bounded()
        {
            if !complete && self.candidates.is_empty() {
                return Ok(());
            }
            return Err("table scope search requires the actual active nonmodal window".into());
        }

        let objects = unique_object_map(snapshot)?;
        let window_roots: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.window_id == self.window_id
                    && object.role == "AXWindow"
                    && object.parent.is_none()
            })
            .collect();
        if window_roots.len() != 1 || window_roots[0].frame != matching_windows[0].frame {
            return Err("table scope search requires the unique actual active window root".into());
        }
        let requested_headers: BTreeSet<_> = self
            .requested_scope
            .column_headers
            .iter()
            .map(String::as_str)
            .collect();
        let mut seen_tables = BTreeSet::new();

        for candidate in &self.candidates {
            validate_candidate_shape(candidate)?;
            if !seen_tables.insert(candidate.table_instance_ref.as_str().to_owned()) {
                return Err("table scope search contains a duplicate table candidate".into());
            }
            if candidate.table_instance_ref == candidate.scroll_container_instance_ref
                || candidate
                    .scroll_path
                    .iter()
                    .any(|part| part.instance_ref == candidate.table_instance_ref)
            {
                return Err("table scope candidate reuses a table ref in its scroll path".into());
            }

            let table = objects
                .get(candidate.table_instance_ref.as_str())
                .copied()
                .ok_or("table scope candidate table ref is not in this Source read")?;
            let viewport = unique_viewport(snapshot, &candidate.table_instance_ref)?;
            if table.role != "AXTable"
                || table.secure
                || table.window_id != self.window_id
                || viewport.process_id != self.process_id
                || viewport.scroll_container_instance_ref != candidate.scroll_container_instance_ref
                || viewport.table_frame != table.frame
                || !viewport.table_frame.valid_bounded()
                || !viewport.viewport_frame.valid_bounded()
                || !matching_windows[0]
                    .frame
                    .contains_rect(&viewport.viewport_frame)
                || table.parent.as_ref() != Some(&candidate.scroll_container_instance_ref)
            {
                return Err(
                    "table scope candidate does not match its current AXTable viewport".into(),
                );
            }

            let scroll = objects
                .get(candidate.scroll_container_instance_ref.as_str())
                .copied()
                .ok_or("table scope scroll ref is not in this Source read")?;
            if scroll.role != "AXScrollArea"
                || scroll.secure
                || scroll.window_id != self.window_id
                || !scroll.frame.contains_rect(&viewport.viewport_frame)
                || !table.frame.intersects(&viewport.viewport_frame)
                || viewport.table_instance_ref != candidate.table_instance_ref
            {
                return Err(
                    "table scope candidate requires its actual current AXScrollArea".into(),
                );
            }

            let actual_path = actual_scroll_path(
                snapshot,
                &objects,
                table,
                matching_windows[0].frame,
                &self.window_id,
            )?;
            if actual_path.len() != candidate.scroll_path.len()
                || candidate.scroll_path.len() != self.requested_scope.scroll_path.len()
                || !actual_path_matches_scope(&actual_path, &self.requested_scope)
            {
                return Err("table scope candidate path does not match the requested path".into());
            }
            for ((actual, returned), requested) in actual_path
                .iter()
                .zip(&candidate.scroll_path)
                .zip(&self.requested_scope.scroll_path)
            {
                if actual.instance_ref != returned.instance_ref
                    || actual.description.as_deref() != Some(returned.label.as_str())
                    || actual.frame != returned.frame
                    || actual.role != requested.role.ax_role()
                    || actual.description.as_deref() != Some(requested.label.as_str())
                {
                    return Err(
                        "table scope candidate path does not match actual Source ancestry".into(),
                    );
                }
            }
            if candidate.scroll_path.last().map(|part| &part.instance_ref)
                != Some(&candidate.scroll_container_instance_ref)
            {
                return Err("table scope scroll ref must be the nearest path element".into());
            }

            if !viewport.columns_complete {
                return Err("table scope candidate requires complete same-read columns".into());
            }
            let viewport_headers = complete_header_set(viewport)?;
            let candidate_headers = unique_candidate_headers(candidate)?;
            if candidate_headers != requested_headers || viewport_headers != requested_headers {
                return Err(
                    "table scope candidate headers do not match the exact requested set".into(),
                );
            }
        }
        if complete {
            // Close contradictions with this snapshot's known complete
            // viewport projections only. Unseen branches remain Source's
            // census-completeness responsibility; this does not consult the
            // global completeness flag.
            let known_matching_tables = known_complete_matching_table_refs(
                snapshot,
                &objects,
                &self.requested_scope,
                self.process_id,
                &self.window_id,
                matching_windows[0].frame,
            )?;
            if seen_tables != known_matching_tables {
                return Err(
                    "complete table scope search must include every known same-read matching table"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

fn validate_candidate_shape(candidate: &TableScopeCandidate) -> Result<(), String> {
    if candidate.scroll_path.is_empty() || candidate.scroll_path.len() > MAX_SCOPE_PATH {
        return Err("table scope candidate path must contain 1 to 8 elements".into());
    }
    let mut path_refs = BTreeSet::new();
    for part in &candidate.scroll_path {
        validate_text("table scope path label", &part.label)?;
        if !part.frame.valid_bounded() || !path_refs.insert(part.instance_ref.as_str()) {
            return Err("table scope candidate has invalid geometry or duplicate path refs".into());
        }
    }
    if candidate.column_headers.is_empty() || candidate.column_headers.len() > MAX_SCOPE_HEADERS {
        return Err("table scope candidate needs 1 to 64 headers".into());
    }
    for header in &candidate.column_headers {
        validate_text("table scope candidate header", header)?;
    }
    Ok(())
}

fn unique_candidate_headers(candidate: &TableScopeCandidate) -> Result<BTreeSet<&str>, String> {
    let mut headers = BTreeSet::new();
    for header in &candidate.column_headers {
        if !headers.insert(header.as_str()) {
            return Err("table scope candidate contains duplicate headers".into());
        }
    }
    Ok(headers)
}

fn complete_header_set(viewport: &TableViewport) -> Result<BTreeSet<&str>, String> {
    if !viewport.columns_complete || viewport.columns.is_empty() {
        return Err("table scope candidate requires complete same-read headers".into());
    }
    let mut headers = BTreeSet::new();
    for column in &viewport.columns {
        let header = column
            .header
            .as_deref()
            .ok_or("table scope candidate has a column without a public header")?;
        validate_text("table scope viewport header", header)?;
        if !headers.insert(header) {
            return Err("table scope viewport contains duplicate headers".into());
        }
    }
    Ok(headers)
}

fn actual_scroll_path<'a>(
    snapshot: &StructureSnapshot,
    objects: &BTreeMap<&'a str, &'a UiObject>,
    table: &'a UiObject,
    active_window_frame: Rect,
    active_window_id: &str,
) -> Result<Vec<&'a UiObject>, String> {
    let mut current_ref = table
        .parent
        .as_ref()
        .ok_or("table scope candidate table has no actual parent")?;
    let mut visited = BTreeSet::new();
    let mut reverse_path = Vec::new();
    let mut reached_window = false;

    for _ in 0..=snapshot.objects.len().min(512) {
        if !visited.insert(current_ref.as_str()) {
            return Err("table scope candidate has a cyclic actual parent path".into());
        }
        let object = objects
            .get(current_ref.as_str())
            .copied()
            .ok_or("table scope candidate parent is absent from this Source read")?;
        if object.window_id != active_window_id || object.secure || !object.frame.valid_bounded() {
            return Err("table scope candidate parent is secure, invalid, or cross-window".into());
        }
        if object.role == "AXWindow" {
            if object.parent.is_some() || object.frame != active_window_frame {
                return Err("table scope candidate does not reach the active window root".into());
            }
            reached_window = true;
            break;
        }
        if matches!(
            object.role.as_str(),
            "AXSheet" | "AXDialog" | "AXMenu" | "AXMenuItem" | "AXMenuBar" | "AXApplication"
        ) {
            return Err("table scope candidate crosses an unsupported parent layer".into());
        }
        if object.role == "AXScrollArea" {
            if object.description.as_deref().is_none_or(|label| {
                label.is_empty() || label.len() > MAX_SCOPE_TEXT_BYTES || contains_control(label)
            }) {
                return Err("table scope candidate scroll area has no valid public label".into());
            }
            reverse_path.push(object);
            if reverse_path.len() > MAX_SCOPE_PATH {
                return Err("table scope candidate exceeds the scroll path bound".into());
            }
        }
        current_ref = object
            .parent
            .as_ref()
            .ok_or("table scope candidate parent path ends before its active window")?;
    }

    if !reached_window {
        return Err("table scope candidate parent path did not reach the active window".into());
    }
    reverse_path.reverse();
    Ok(reverse_path)
}

fn actual_path_matches_scope(path: &[&UiObject], scope: &RowTableScope) -> bool {
    path.len() == scope.scroll_path.len()
        && path
            .iter()
            .zip(&scope.scroll_path)
            .all(|(actual, requested)| {
                actual.role == requested.role.ax_role()
                    && actual.description.as_deref() == Some(requested.label.as_str())
            })
}

fn known_complete_matching_table_refs(
    snapshot: &StructureSnapshot,
    objects: &BTreeMap<&str, &UiObject>,
    requested_scope: &RowTableScope,
    process_id: u32,
    window_id: &str,
    window_frame: Rect,
) -> Result<BTreeSet<String>, String> {
    let requested_headers: BTreeSet<_> = requested_scope
        .column_headers
        .iter()
        .map(String::as_str)
        .collect();
    let mut matching = BTreeSet::new();

    for viewport in &snapshot.table_viewports {
        if viewport.process_id != process_id {
            continue;
        }
        let table = objects
            .get(viewport.table_instance_ref.as_str())
            .copied()
            .ok_or("table viewport ref is absent from this Source read")?;
        if table.window_id != window_id {
            continue;
        }
        if table.secure {
            continue;
        }
        if table.role != "AXTable"
            || table.parent.as_ref() != Some(&viewport.scroll_container_instance_ref)
        {
            return Err("same-read table viewport has an invalid actual table parent".into());
        }
        let scroll = objects
            .get(viewport.scroll_container_instance_ref.as_str())
            .copied()
            .ok_or("table viewport scroll ref is absent from this Source read")?;
        if scroll.role != "AXScrollArea" || scroll.window_id != window_id {
            return Err("same-read table viewport has an invalid actual scroll parent".into());
        }
        let Ok(path) = actual_scroll_path(snapshot, objects, table, window_frame, window_id) else {
            // A partial generic projection can leave an unrelated table's
            // farther ancestry unknown. It is not a known exact-scope match.
            continue;
        };
        if !actual_path_matches_scope(&path, requested_scope) || !viewport.columns_complete {
            continue;
        }
        if complete_header_set(viewport).is_ok_and(|headers| headers == requested_headers) {
            matching.insert(viewport.table_instance_ref.as_str().to_owned());
        }
    }
    Ok(matching)
}

fn unique_viewport<'a>(
    snapshot: &'a StructureSnapshot,
    table_ref: &ObjectRef,
) -> Result<&'a TableViewport, String> {
    let mut matches = snapshot
        .table_viewports
        .iter()
        .filter(|viewport| &viewport.table_instance_ref == table_ref);
    let viewport = matches
        .next()
        .ok_or("table scope candidate has no same-read table viewport")?;
    if matches.next().is_some() {
        return Err("table scope candidate has duplicate same-read table viewports".into());
    }
    Ok(viewport)
}

fn unique_object_map(snapshot: &StructureSnapshot) -> Result<BTreeMap<&str, &UiObject>, String> {
    let mut objects = BTreeMap::new();
    for object in &snapshot.objects {
        if objects
            .insert(object.instance_ref.as_str(), object)
            .is_some()
        {
            return Err("table scope snapshot contains duplicate Source object refs".into());
        }
    }
    Ok(objects)
}

fn validate_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_SCOPE_TEXT_BYTES || contains_control(value) {
        return Err(format!(
            "{field} must be nonempty public text within 256 bytes"
        ));
    }
    Ok(())
}

fn contains_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::device::SurfaceRef;
    use crate::desktop::ids::RequestId;
    use crate::desktop::source::{
        AppIdentity, AxEnabledObservation, ObjectIdentityContinuity, ObserverCoverage,
        TableViewportColumn, WindowInfo,
    };
    use crate::desktop::task::{RowContainerRole, RowContainerScope};

    fn reference(value: &str) -> ObjectRef {
        ObjectRef::new(value).unwrap()
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn object(reference: &str, parent: Option<&str>, role: &str, frame: Rect) -> UiObject {
        UiObject {
            instance_ref: self::reference(reference),
            parent: parent.map(self::reference),
            window_id: "window".into(),
            role: role.into(),
            subrole: (role == "AXWindow").then(|| "AXStandardWindow".into()),
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
            identity_continuity: ObjectIdentityContinuity::ReadScoped,
        }
    }

    fn fixture() -> (StructureSnapshot, TableScopeSearch) {
        let window_frame = rect(0.0, 0.0, 400.0, 300.0);
        let outer_frame = rect(0.0, 0.0, 360.0, 280.0);
        let inner_frame = rect(10.0, 10.0, 320.0, 250.0);
        let table_frame = rect(12.0, 12.0, 300.0, 700.0);
        let column_frame = rect(12.0, 12.0, 150.0, 24.0);
        let root = object("window-root", None, "AXWindow", window_frame);
        let mut outer = object(
            "outer-scroll",
            Some("window-root"),
            "AXScrollArea",
            outer_frame,
        );
        outer.description = Some("Preferences content".into());
        let mut inner = object(
            "archive-scroll",
            Some("outer-scroll"),
            "AXScrollArea",
            inner_frame,
        );
        inner.description = Some("Archive".into());
        let table = object("table-1", Some("archive-scroll"), "AXTable", table_frame);
        let column = object("column-1", Some("table-1"), "AXColumn", column_frame);
        let objects = vec![root, outer, inner, table, column];
        let viewport = TableViewport {
            table_instance_ref: reference("table-1"),
            scroll_container_instance_ref: reference("archive-scroll"),
            process_id: 42,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: inner_frame,
            visible_rows_complete: false,
            visible_rows: vec![],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(reference("column-1")),
                parent_instance_ref: reference("table-1"),
                frame: column_frame,
                header_instance_ref: None,
                header: Some("Name".into()),
                ax_index: Some(0),
            }],
        };
        let mut snapshot = StructureSnapshot {
            snapshot_id: RequestId::new("scope-snapshot").unwrap(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "surface".into(),
                epoch: 1,
            },
            clock_domain: "test/monotonic".into(),
            identity_boundary: None,
            frame_ref: None,
            source_sequence: 1,
            observed_ms: 20,
            app: AppIdentity {
                bundle_id: "test.app".into(),
                name: "Test".into(),
                pid: 42,
            },
            windows: vec![WindowInfo {
                window_id: "window".into(),
                title: Some("Settings".into()),
                frame: window_frame,
                z_index: 0,
                kind: crate::desktop::source::LayerKind::Window,
                modal: false,
                owner_bundle_id: "test.app".into(),
            }],
            global_focus: crate::desktop::source::GlobalFocusObservation {
                app: crate::desktop::source::ObservationFact::Known {
                    value: AppIdentity {
                        bundle_id: "test.app".into(),
                        name: "Test".into(),
                        pid: 42,
                    },
                },
                window_id: crate::desktop::source::ObservationFact::Known {
                    value: "window".into(),
                },
            },
            target_context: None,
            complete: false,
            incomplete_reason: Some("global AX traversal reached its node budget".into()),
            observer_coverage: ObserverCoverage::for_projected_objects(&objects),
            objects,
            menu_anchors: vec![],
            menu_context: None,
            focus_context: None,
            window_context: None,
            table_viewports: vec![viewport],
            table_scope_search: None,
            owned_document_evidence: None,
        };
        let search = TableScopeSearch {
            requested_scope: RowTableScope {
                scroll_path: vec![
                    RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Preferences content".into(),
                    },
                    RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Archive".into(),
                    },
                ],
                column_headers: vec!["Name".into()],
            },
            process_id: 42,
            window_id: "window".into(),
            state: TableScopeSearchState::Complete,
            candidates: vec![TableScopeCandidate {
                table_instance_ref: reference("table-1"),
                scroll_container_instance_ref: reference("archive-scroll"),
                scroll_path: vec![
                    TableScopePathElement {
                        instance_ref: reference("outer-scroll"),
                        label: "Preferences content".into(),
                        frame: outer_frame,
                    },
                    TableScopePathElement {
                        instance_ref: reference("archive-scroll"),
                        label: "Archive".into(),
                        frame: inner_frame,
                    },
                ],
                column_headers: vec!["Name".into()],
            }],
        };
        snapshot.table_scope_search = Some(search.clone());
        (snapshot, search)
    }

    fn add_second_matching_table(snapshot: &mut StructureSnapshot) {
        let table_frame = rect(12.0, 120.0, 300.0, 500.0);
        let column_frame = rect(12.0, 120.0, 150.0, 24.0);
        snapshot.objects.push(object(
            "table-2",
            Some("archive-scroll"),
            "AXTable",
            table_frame,
        ));
        snapshot.objects.push(object(
            "column-2",
            Some("table-2"),
            "AXColumn",
            column_frame,
        ));
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot.table_viewports.push(TableViewport {
            table_instance_ref: reference("table-2"),
            scroll_container_instance_ref: reference("archive-scroll"),
            process_id: 42,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: rect(10.0, 120.0, 320.0, 130.0),
            visible_rows_complete: false,
            visible_rows: vec![],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(reference("column-2")),
                parent_instance_ref: reference("table-2"),
                frame: column_frame,
                header_instance_ref: None,
                header: Some("Name".into()),
                ax_index: Some(0),
            }],
        });
    }

    #[test]
    fn partial_window_facts_remain_read_only_and_require_the_same_unique_scope() {
        use super::super::source::{KernelProcessIdentity, SourceWindowContext};
        let (mut snapshot, _) = fixture();
        let window = snapshot.windows[0].clone();
        snapshot.window_context = Some(SourceWindowContext {
            source_session_id: "scoped-window-session".into(),
            session_epoch: snapshot.session_epoch,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 100,
                start_micros: 200,
            },
            origin_window_id: window.window_id,
            origin_window_frame: window.frame,
            verified_source_sequence: snapshot.source_sequence,
            verified_observed_ms: snapshot.observed_ms,
        });
        snapshot.validate().unwrap();
        assert!(!snapshot.complete);
        assert!(snapshot.focus_context.is_none());

        let mut missing = snapshot.clone();
        missing.table_scope_search = None;
        assert!(missing.validate().is_err());
        let mut incomplete = snapshot.clone();
        incomplete.table_scope_search.as_mut().unwrap().state = TableScopeSearchState::Incomplete {
            reason: "deadline".into(),
        };
        assert!(incomplete.validate().is_err());
        let mut empty = snapshot.clone();
        empty
            .table_scope_search
            .as_mut()
            .unwrap()
            .candidates
            .clear();
        assert!(empty.validate().is_err());
        for field in ["sequence", "time", "window", "pid", "geometry"] {
            let mut other = snapshot.clone();
            let context = other.window_context.as_mut().unwrap();
            match field {
                "sequence" => context.verified_source_sequence += 1,
                "time" => context.verified_observed_ms += 1,
                "window" => context.origin_window_id = "other-window".into(),
                "pid" => context.process.pid += 1,
                _ => context.origin_window_frame.x += 1.0,
            }
            assert!(
                other.validate().is_err(),
                "foreign {field} must reject partial window facts"
            );
        }
    }

    #[test]
    fn complete_scoped_search_is_valid_inside_a_globally_partial_snapshot() {
        let (snapshot, search) = fixture();
        assert!(!snapshot.complete);
        assert!(search.validate_against_snapshot(&snapshot).is_ok());
        assert_eq!(search.unique_candidate(), search.candidates.first());
    }

    #[test]
    fn incomplete_census_never_grants_unique_table_authority() {
        let (snapshot, mut search) = fixture();
        search.state = TableScopeSearchState::Incomplete {
            reason: "one branch timed out".into(),
        };
        assert!(search.validate_against_snapshot(&snapshot).is_ok());
        assert!(search.unique_candidate().is_none());
    }

    #[test]
    fn incomplete_diagnostic_without_foreground_does_not_invalidate_source() {
        let (mut snapshot, mut search) = fixture();
        search.state = TableScopeSearchState::Incomplete {
            reason: "requested window is no longer foreground".into(),
        };
        search.candidates.clear();
        snapshot.global_focus.window_id = crate::desktop::source::ObservationFact::Absent;
        snapshot.table_scope_search = Some(search.clone());
        assert!(search.validate_against_snapshot(&snapshot).is_ok());
        assert!(search.unique_candidate().is_none());
        assert!(snapshot.validate().is_ok());

        let (mut snapshot, complete_search) = fixture();
        snapshot.global_focus.window_id = crate::desktop::source::ObservationFact::Absent;
        assert!(
            complete_search
                .validate_against_snapshot(&snapshot)
                .is_err()
        );
    }

    #[test]
    fn complete_empty_census_means_no_table_was_found_in_this_scope() {
        let (mut snapshot, mut search) = fixture();
        search.candidates.clear();
        snapshot.table_viewports[0].columns[0].header = Some("Other".into());
        snapshot.table_scope_search = Some(search.clone());
        assert!(search.validate_against_snapshot(&snapshot).is_ok());
        assert!(search.unique_candidate().is_none());
        assert!(!search.is_ambiguous());
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn complete_empty_result_cannot_omit_a_known_matching_viewport() {
        let (snapshot, mut search) = fixture();
        search.candidates.clear();
        assert!(search.validate_against_snapshot(&snapshot).is_err());
    }

    #[test]
    fn complete_result_cannot_hide_a_second_known_matching_viewport() {
        let (mut snapshot, search) = fixture();
        add_second_matching_table(&mut snapshot);
        assert!(search.validate_against_snapshot(&snapshot).is_err());
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn complete_multiple_matching_tables_are_valid_but_ambiguous() {
        let (mut snapshot, mut search) = fixture();
        add_second_matching_table(&mut snapshot);
        let mut second = search.candidates[0].clone();
        second.table_instance_ref = reference("table-2");
        second.column_headers = vec!["Name".into()];
        search.candidates.push(second);
        snapshot.table_scope_search = Some(search.clone());
        assert!(search.validate_against_snapshot(&snapshot).is_ok());
        assert!(snapshot.validate().is_ok());
        assert!(search.unique_candidate().is_none());
        assert!(search.is_ambiguous());
    }

    #[test]
    fn stale_refs_and_tampered_path_or_headers_are_rejected() {
        let (snapshot, search) = fixture();
        let mut stale = search.clone();
        stale.candidates[0].table_instance_ref = reference("old-read-table");
        assert!(stale.validate_against_snapshot(&snapshot).is_err());

        let mut stale_scroll = search.clone();
        stale_scroll.candidates[0].scroll_container_instance_ref = reference("old-read-scroll");
        assert!(stale_scroll.validate_against_snapshot(&snapshot).is_err());

        let mut wrong_path = search.clone();
        wrong_path.candidates[0].scroll_path[1].label = "Projects".into();
        assert!(wrong_path.validate_against_snapshot(&snapshot).is_err());

        let mut wrong_headers = search.clone();
        wrong_headers.candidates[0].column_headers = vec!["Modified".into()];
        assert!(wrong_headers.validate_against_snapshot(&snapshot).is_err());

        let mut partial_columns = snapshot.clone();
        partial_columns.table_viewports[0].columns_complete = false;
        assert!(search.validate_against_snapshot(&partial_columns).is_err());
    }

    #[test]
    fn duplicate_path_refs_and_duplicate_candidates_are_rejected() {
        let (snapshot, mut search) = fixture();
        search.candidates[0].scroll_path[1].instance_ref = reference("outer-scroll");
        assert!(search.validate_against_snapshot(&snapshot).is_err());

        let (snapshot, mut search) = fixture();
        search.candidates.push(search.candidates[0].clone());
        assert!(search.validate_against_snapshot(&snapshot).is_err());
    }

    #[test]
    fn source_field_is_optional_and_strict_on_wire() {
        let (mut snapshot, search) = fixture();
        snapshot.table_scope_search = None;
        let mut wire = serde_json::to_value(snapshot).unwrap();
        assert!(wire.get("table_scope_search").is_none());
        wire["table_scope_search"] = serde_json::to_value(search).unwrap();
        let decoded: StructureSnapshot = serde_json::from_value(wire.clone()).unwrap();
        assert!(decoded.table_scope_search.is_some());
        assert!(decoded.validate().is_ok());
        wire["table_scope_search"]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<StructureSnapshot>(wire).is_err());

        let mut wire = serde_json::to_value(fixture().1).unwrap();
        wire["candidates"][0]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<TableScopeSearch>(wire).is_err());
    }
}
