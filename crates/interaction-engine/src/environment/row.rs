//! Resolve a frozen public row target against one current Source read.
//!
//! This module proves current semantics only. The frozen TargetSpec carries no
//! Source refs, AX indexes, private application keys, or row ordinals. Resolution
//! binds the public conditions to the current contracts' one-read selector and
//! observation. A globally partial read is usable only through its exact
//! complete table-scope census; neither path makes a cross-read identity claim.

use std::collections::BTreeSet;

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::row::{
    RowColumnCondition, RowSelectionObservation, RowSelector,
};
use interaction_contracts::desktop::source::{StructureSnapshot, TableViewport, UiObject, UiValue};
use interaction_contracts::desktop::task::{
    ExpectedValue, RowContainerRole, RowContainerScope, RowTargetSpec, TargetSpec,
};

use crate::environment::read::focused_window_id;

const MAX_ROW_FRESHNESS_MS: u64 = 10_000;
const MAX_ROW_TABLE_ANCESTORS: usize = 8;

/// Why an immutable public row target could not be resolved in the current
/// Source read. `IncompleteSearch` is retryable only after a fresh, broader
/// observation; the other failures need a different scope or selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowResolutionFailure {
    IncompleteSearch,
    NotFound,
    Ambiguous,
    InvalidScope,
}

/// A semantic resolution tied to exactly one current Source read.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRowTarget {
    pub selector: RowSelector,
    pub source: RowSelectionObservation,
}

/// Prove that this Source read is usable for the requested row-table scope.
/// A full Source tree retains its existing path; a partial tree needs a
/// complete same-read scoped census for the exact frozen scope.
pub fn row_scope_coverage(
    snapshot: &StructureSnapshot,
    scope: &interaction_contracts::desktop::task::RowTableScope,
) -> Result<(), RowResolutionFailure> {
    scope
        .validate()
        .map_err(|_| RowResolutionFailure::InvalidScope)?;
    snapshot
        .validate()
        .map_err(|_| RowResolutionFailure::IncompleteSearch)?;
    if snapshot.complete {
        return Ok(());
    }

    let search = snapshot
        .table_scope_search
        .as_ref()
        .ok_or(RowResolutionFailure::IncompleteSearch)?;
    if &search.requested_scope != scope {
        return Err(RowResolutionFailure::InvalidScope);
    }
    if !matches!(
        &search.state,
        interaction_contracts::desktop::table_scope::TableScopeSearchState::Complete
    ) {
        return Err(RowResolutionFailure::IncompleteSearch);
    }
    search
        .validate_against_snapshot(snapshot)
        .map_err(|_| RowResolutionFailure::IncompleteSearch)
}

/// Resolve an explicit row target using the active window, exact public scroll
/// ancestry and headers, and all of its public value conditions. Partial trees
/// are limited to tables in the complete same-read scoped census and never
/// establish absence outside their visible row viewport.
pub fn resolve_row_target(
    snapshot: &StructureSnapshot,
    target: &TargetSpec,
    valid_until_ms: u64,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<ResolvedRowTarget, RowResolutionFailure> {
    target
        .validate()
        .map_err(|_| RowResolutionFailure::InvalidScope)?;
    let row_spec = target
        .row
        .as_ref()
        .ok_or(RowResolutionFailure::InvalidScope)?;

    row_scope_coverage(snapshot, &row_spec.table_scope)?;
    validate_clock(snapshot, valid_until_ms, now_ms, max_age_ms)?;
    let partial = !snapshot.complete;
    let scoped_table_refs: Option<BTreeSet<String>> = if partial {
        let search = snapshot
            .table_scope_search
            .as_ref()
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        let candidates: BTreeSet<String> = search
            .candidates
            .iter()
            .map(|candidate| candidate.table_instance_ref.as_str().to_owned())
            .collect();
        if candidates.is_empty() {
            return Err(RowResolutionFailure::IncompleteSearch);
        }
        Some(candidates)
    } else {
        None
    };

    let active_window_id = focused_window_id(snapshot).ok_or(RowResolutionFailure::InvalidScope)?;
    let mut window_infos = snapshot
        .windows
        .iter()
        .filter(|window| window.window_id == active_window_id);
    let window_info = window_infos
        .next()
        .ok_or(RowResolutionFailure::InvalidScope)?;
    if window_infos.next().is_some() || window_info.owner_bundle_id != snapshot.app.bundle_id {
        return Err(RowResolutionFailure::InvalidScope);
    }

    let active_window_objects: Vec<&UiObject> = snapshot
        .objects
        .iter()
        .filter(|object| object.role == "AXWindow" && object.window_id == active_window_id)
        .collect();
    let [active_window_object] = active_window_objects.as_slice() else {
        return Err(RowResolutionFailure::IncompleteSearch);
    };

    let requested_headers: BTreeSet<String> = row_spec
        .table_scope
        .column_headers
        .iter()
        .cloned()
        .collect();
    let mut matching_tables: Vec<&TableViewport> = Vec::new();

    for viewport in &snapshot.table_viewports {
        if scoped_table_refs
            .as_ref()
            .is_some_and(|refs| !refs.contains(viewport.table_instance_ref.as_str()))
        {
            continue;
        }
        let table = object_for_ref(snapshot, viewport.table_instance_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if table.window_id != active_window_id {
            continue;
        }
        if table.role != "AXTable"
            || viewport.process_id != snapshot.app.pid
            || table.frame != viewport.table_frame
            || !window_info.frame.contains_rect(&viewport.viewport_frame)
        {
            return Err(RowResolutionFailure::InvalidScope);
        }

        let scroll = object_for_ref(snapshot, viewport.scroll_container_instance_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if scroll.role != "AXScrollArea"
            || scroll.window_id != active_window_id
            || table.parent.as_ref() != Some(&scroll.instance_ref)
            || !scroll.frame.contains_rect(&viewport.viewport_frame)
        {
            return Err(RowResolutionFailure::InvalidScope);
        }

        let actual_scroll_path = actual_scroll_path(
            snapshot,
            viewport,
            active_window_id,
            active_window_object.instance_ref.as_str(),
        )?;
        if actual_scroll_path != row_spec.table_scope.scroll_path {
            continue;
        }

        if !viewport.columns_complete || !viewport.visible_rows_complete {
            return Err(RowResolutionFailure::IncompleteSearch);
        }
        let actual_headers = actual_header_set(viewport)?;
        if actual_headers != requested_headers {
            continue;
        }
        matching_tables.push(viewport);
    }

    let viewport = match matching_tables.as_slice() {
        [] if partial => return Err(RowResolutionFailure::IncompleteSearch),
        [] => return Err(RowResolutionFailure::NotFound),
        [viewport] => *viewport,
        _ => return Err(RowResolutionFailure::Ambiguous),
    };

    validate_row_cells(viewport, &requested_headers)?;
    if partial {
        validate_partial_viewport_refs(snapshot, viewport)?;
    }
    let selector = selector_for(snapshot, viewport, row_spec)?;
    let source = RowSelectionObservation::from_snapshot(
        &selector,
        snapshot,
        valid_until_ms,
        now_ms,
        max_age_ms,
    )
    .map_err(|error| {
        let failure = classify_observation_failure(error);
        if partial && failure == RowResolutionFailure::NotFound {
            RowResolutionFailure::IncompleteSearch
        } else {
            failure
        }
    })?;

    if let Some(expected_group) = &target.group {
        let row_object = object_for_ref(snapshot, source.row_instance_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if row_object
            .group
            .as_ref()
            .is_none_or(|group| normalize_label(group) != normalize_label(expected_group))
        {
            return Err(if partial {
                RowResolutionFailure::IncompleteSearch
            } else {
                RowResolutionFailure::NotFound
            });
        }
    }

    Ok(ResolvedRowTarget { selector, source })
}

fn validate_clock(
    snapshot: &StructureSnapshot,
    valid_until_ms: u64,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<(), RowResolutionFailure> {
    if max_age_ms == 0
        || max_age_ms > MAX_ROW_FRESHNESS_MS
        || now_ms < snapshot.observed_ms
        || now_ms >= valid_until_ms
        || valid_until_ms <= snapshot.observed_ms
        || valid_until_ms - snapshot.observed_ms > MAX_ROW_FRESHNESS_MS
        || now_ms - snapshot.observed_ms >= max_age_ms
        || snapshot.session_epoch == 0
        || snapshot.surface.epoch == 0
        || snapshot.source_sequence == 0
        || snapshot.identity_boundary.is_none()
    {
        return Err(RowResolutionFailure::InvalidScope);
    }
    Ok(())
}

fn object_for_ref<'a>(snapshot: &'a StructureSnapshot, reference: &str) -> Option<&'a UiObject> {
    snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref.as_str() == reference)
}

/// Gather every scroll area from the table's nearest ancestor to the active
/// AXWindow, then expose the canonical outermost-to-nearest path.
fn actual_scroll_path(
    snapshot: &StructureSnapshot,
    viewport: &TableViewport,
    active_window_id: &str,
    active_window_ref: &str,
) -> Result<Vec<RowContainerScope>, RowResolutionFailure> {
    let mut current = object_for_ref(snapshot, viewport.scroll_container_instance_ref.as_str())
        .ok_or(RowResolutionFailure::IncompleteSearch)?;
    let mut visited = BTreeSet::new();
    let mut path = Vec::new();

    loop {
        if !visited.insert(current.instance_ref.as_str()) {
            return Err(RowResolutionFailure::InvalidScope);
        }
        if current.window_id != active_window_id {
            return Err(RowResolutionFailure::InvalidScope);
        }
        if current.role == "AXWindow" {
            if current.instance_ref.as_str() != active_window_ref {
                return Err(RowResolutionFailure::InvalidScope);
            }
            return Ok(path.into_iter().rev().collect());
        }
        if current.role == "AXScrollArea" {
            let label = current
                .description
                .as_ref()
                .filter(|label| valid_public_label(label))
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            path.push(RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: label.clone(),
            });
            if path.len() > MAX_ROW_TABLE_ANCESTORS {
                return Err(RowResolutionFailure::IncompleteSearch);
            }
        }
        let parent = current
            .parent
            .as_ref()
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        current = object_for_ref(snapshot, parent.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
    }
}

fn actual_header_set(viewport: &TableViewport) -> Result<BTreeSet<String>, RowResolutionFailure> {
    let mut exact = BTreeSet::new();
    let mut normalized = BTreeSet::new();
    for column in &viewport.columns {
        let header = column
            .header
            .as_ref()
            .filter(|header| valid_public_label(header))
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if !exact.insert(header.clone()) || !normalized.insert(normalize_name(header)) {
            return Err(RowResolutionFailure::Ambiguous);
        }
    }
    Ok(exact)
}

fn validate_row_cells(
    viewport: &TableViewport,
    headers: &BTreeSet<String>,
) -> Result<(), RowResolutionFailure> {
    if !viewport.visible_rows_complete || viewport.visible_rows.is_empty() {
        return Err(RowResolutionFailure::IncompleteSearch);
    }
    for row in &viewport.visible_rows {
        if !row.cells_complete {
            return Err(RowResolutionFailure::IncompleteSearch);
        }
        let mut row_headers = BTreeSet::new();
        for cell in &row.cells {
            let header = cell
                .column_header
                .as_ref()
                .filter(|header| valid_public_label(header))
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            if !headers.contains(header) {
                return Err(RowResolutionFailure::IncompleteSearch);
            }
            if !row_headers.insert(header) {
                return Err(RowResolutionFailure::Ambiguous);
            }
        }
    }
    Ok(())
}

/// A partial tree may resolve only rows whose same-read viewport links all
/// survive Source validation. No absent ref is replaced by an ordinal.
fn validate_partial_viewport_refs(
    snapshot: &StructureSnapshot,
    viewport: &TableViewport,
) -> Result<(), RowResolutionFailure> {
    for column in &viewport.columns {
        let column_ref = column
            .instance_ref
            .as_ref()
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        let header_ref = column
            .header_instance_ref
            .as_ref()
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        let column_object = object_for_ref(snapshot, column_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        object_for_ref(snapshot, header_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if column_object.role != "AXColumn"
            || column_object.parent.as_ref() != Some(&viewport.table_instance_ref)
            || column_object.frame != column.frame
        {
            return Err(RowResolutionFailure::InvalidScope);
        }
    }

    for row in &viewport.visible_rows {
        let row_ref = row
            .instance_ref
            .as_ref()
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        let row_object = object_for_ref(snapshot, row_ref.as_str())
            .ok_or(RowResolutionFailure::IncompleteSearch)?;
        if row_object.role != "AXRow"
            || row_object.parent.as_ref() != Some(&viewport.table_instance_ref)
            || row_object.frame != row.frame
        {
            return Err(RowResolutionFailure::InvalidScope);
        }
        for cell in &row.cells {
            let cell_ref = cell
                .instance_ref
                .as_ref()
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            let column_ref = cell
                .column_instance_ref
                .as_ref()
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            let header_ref = cell
                .header_instance_ref
                .as_ref()
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            let cell_object = object_for_ref(snapshot, cell_ref.as_str())
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            object_for_ref(snapshot, column_ref.as_str())
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            object_for_ref(snapshot, header_ref.as_str())
                .ok_or(RowResolutionFailure::IncompleteSearch)?;
            if cell.parent_instance_ref.as_ref() != Some(row_ref)
                || cell_object.role != "AXCell"
                || cell_object.parent.as_ref() != Some(row_ref)
                || cell_object.frame != cell.frame
            {
                return Err(RowResolutionFailure::InvalidScope);
            }
        }
    }
    Ok(())
}

fn selector_for(
    snapshot: &StructureSnapshot,
    viewport: &TableViewport,
    row_spec: &RowTargetSpec,
) -> Result<RowSelector, RowResolutionFailure> {
    let actual_headers = actual_header_set(viewport)?;
    let mut conditions = Vec::with_capacity(row_spec.conditions.len());
    for condition in &row_spec.conditions {
        if !actual_headers.contains(&condition.column_name) {
            return Err(RowResolutionFailure::InvalidScope);
        }
        conditions.push(RowColumnCondition {
            column_name: condition.column_name.clone(),
            value: expected_value(&condition.value)?,
        });
    }
    let selector = RowSelector {
        app_bundle_id: snapshot.app.bundle_id.clone(),
        process_id: snapshot.app.pid,
        window_id: focused_window_id(snapshot)
            .ok_or(RowResolutionFailure::InvalidScope)?
            .to_owned(),
        table_instance_ref: viewport.table_instance_ref.clone(),
        conditions,
    };
    selector
        .validate()
        .map_err(|_| RowResolutionFailure::InvalidScope)?;
    Ok(selector)
}

fn expected_value(value: &ExpectedValue) -> Result<UiValue, RowResolutionFailure> {
    match value {
        ExpectedValue::Bool { value } => Ok(UiValue::Bool(*value)),
        ExpectedValue::Text { value } => Ok(UiValue::Text(value.clone())),
        ExpectedValue::Number { value } if value.unsigned_abs() <= 1_000_000_000_000 => {
            Ok(UiValue::Number(*value as f64))
        }
        ExpectedValue::Number { .. } => Err(RowResolutionFailure::InvalidScope),
    }
}

fn classify_observation_failure(error: String) -> RowResolutionFailure {
    if error.contains("more than one") || error.contains("ambiguous") {
        RowResolutionFailure::Ambiguous
    } else if error.contains("does not match an actual visible row") {
        RowResolutionFailure::NotFound
    } else if error.contains("stale") || error.contains("future-dated") {
        RowResolutionFailure::InvalidScope
    } else if error.contains("incomplete")
        || error.contains("unknown")
        || error.contains("missing")
        || error.contains("outside the viewport")
    {
        RowResolutionFailure::IncompleteSearch
    } else {
        RowResolutionFailure::InvalidScope
    }
}

fn valid_public_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn normalize_name(value: &str) -> String {
    value.trim().chars().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::device::SurfaceRef;
    use interaction_contracts::desktop::ids::{ObjectRef, RequestId, SemanticTargetRef};
    use interaction_contracts::desktop::source::{
        AppIdentity, AxActionName, AxEnabledObservation, LayerKind, ObjectIdentityContinuity,
        ObserverCoverage, SourceIdentityBoundary, SourceState, TableViewportCell,
        TableViewportColumn, TableViewportRow, WindowInfo,
    };
    use interaction_contracts::desktop::table_scope::{
        TableScopeCandidate, TableScopePathElement, TableScopeSearch, TableScopeSearchState,
    };
    use interaction_contracts::desktop::task::{RowColumnConditionSpec, RowTableScope};
    use std::collections::BTreeMap;

    fn object_ref(value: &str) -> ObjectRef {
        ObjectRef::new(value).unwrap()
    }

    fn rect(
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> interaction_contracts::desktop::source::Rect {
        interaction_contracts::desktop::source::Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn source_object(
        reference: &str,
        parent: Option<&str>,
        role: &str,
        frame: interaction_contracts::desktop::source::Rect,
        description: Option<&str>,
        title: Option<&str>,
        value: Option<UiValue>,
    ) -> UiObject {
        UiObject {
            instance_ref: object_ref(reference),
            parent: parent.map(object_ref),
            window_id: "window-1".into(),
            role: role.into(),
            subrole: None,
            title: title.map(str::to_owned),
            description: description.map(str::to_owned),
            value,
            enabled: true,
            focused: false,
            frame,
            actions: Vec::<AxActionName>::new(),
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::ReadScoped,
        }
    }

    fn remap_ref(reference: &mut ObjectRef, mapping: &BTreeMap<String, ObjectRef>) {
        if let Some(next) = mapping.get(reference.as_str()) {
            *reference = next.clone();
        }
    }

    fn remap_optional_ref(
        reference: &mut Option<ObjectRef>,
        mapping: &BTreeMap<String, ObjectRef>,
    ) {
        if let Some(reference) = reference {
            remap_ref(reference, mapping);
        }
    }

    fn rotate_read_scoped_refs(snapshot: &mut StructureSnapshot) {
        let mapping: BTreeMap<String, ObjectRef> = snapshot
            .objects
            .iter()
            .map(|object| {
                (
                    object.instance_ref.as_str().to_owned(),
                    object_ref(&format!("{}-read-2", object.instance_ref.as_str())),
                )
            })
            .collect();
        for object in &mut snapshot.objects {
            remap_ref(&mut object.instance_ref, &mapping);
            remap_optional_ref(&mut object.parent, &mapping);
        }
        for viewport in &mut snapshot.table_viewports {
            remap_ref(&mut viewport.table_instance_ref, &mapping);
            remap_ref(&mut viewport.scroll_container_instance_ref, &mapping);
            for column in &mut viewport.columns {
                remap_optional_ref(&mut column.instance_ref, &mapping);
                remap_ref(&mut column.parent_instance_ref, &mapping);
                remap_optional_ref(&mut column.header_instance_ref, &mapping);
            }
            for row in &mut viewport.visible_rows {
                remap_optional_ref(&mut row.instance_ref, &mapping);
                remap_ref(&mut row.parent_instance_ref, &mapping);
                for cell in &mut row.cells {
                    remap_optional_ref(&mut cell.instance_ref, &mapping);
                    remap_optional_ref(&mut cell.parent_instance_ref, &mapping);
                    remap_optional_ref(&mut cell.column_instance_ref, &mapping);
                    remap_optional_ref(&mut cell.header_instance_ref, &mapping);
                }
            }
        }
    }

    fn fixture() -> (StructureSnapshot, TargetSpec) {
        let window_frame = rect(0.0, 0.0, 400.0, 300.0);
        let scroll_frame = rect(10.0, 10.0, 280.0, 180.0);
        let table_frame = scroll_frame;
        let viewport_frame = scroll_frame;
        let headers = ["Name", "Owner", "Status"];
        let column_frames = [
            rect(10.0, 10.0, 90.0, 25.0),
            rect(100.0, 10.0, 90.0, 25.0),
            rect(190.0, 10.0, 90.0, 25.0),
        ];
        let source_rows = [
            ("Monthly report", "Acme", "Open"),
            ("Monthly report", "Beta", "Open"),
        ];
        let mut objects = vec![
            source_object(
                "window-object",
                None,
                "AXWindow",
                window_frame,
                None,
                Some("Records"),
                None,
            ),
            source_object(
                "scroll",
                Some("window-object"),
                "AXScrollArea",
                scroll_frame,
                Some("Workspace"),
                None,
                None,
            ),
            source_object(
                "table",
                Some("scroll"),
                "AXTable",
                table_frame,
                None,
                None,
                None,
            ),
        ];
        let columns: Vec<TableViewportColumn> = headers
            .iter()
            .enumerate()
            .map(|(index, header)| {
                let column_ref = format!("column-{index}");
                objects.push(source_object(
                    &column_ref,
                    Some("table"),
                    "AXColumn",
                    column_frames[index],
                    None,
                    Some(header),
                    None,
                ));
                TableViewportColumn {
                    instance_ref: Some(object_ref(&column_ref)),
                    parent_instance_ref: object_ref("table"),
                    frame: column_frames[index],
                    header_instance_ref: None,
                    header: Some((*header).into()),
                    ax_index: Some(index as u32),
                }
            })
            .collect();
        let mut rows = Vec::new();
        for (row_index, (name, owner, status)) in source_rows.iter().enumerate() {
            let row_ref = format!("row-{row_index}");
            let row_frame = rect(10.0, 40.0 + row_index as f64 * 40.0, 280.0, 30.0);
            objects.push(source_object(
                &row_ref,
                Some("table"),
                "AXRow",
                row_frame,
                None,
                None,
                None,
            ));
            let values = [*name, *owner, *status];
            let cells = headers
                .iter()
                .enumerate()
                .map(|(column_index, header)| {
                    let cell_ref = format!("cell-{row_index}-{column_index}");
                    let cell_frame = rect(
                        column_frames[column_index].x,
                        row_frame.y,
                        column_frames[column_index].width,
                        row_frame.height,
                    );
                    let value = UiValue::Text(values[column_index].into());
                    objects.push(source_object(
                        &cell_ref,
                        Some(&row_ref),
                        "AXCell",
                        cell_frame,
                        None,
                        None,
                        Some(value.clone()),
                    ));
                    TableViewportCell {
                        instance_ref: Some(object_ref(&cell_ref)),
                        parent_instance_ref: Some(object_ref(&row_ref)),
                        frame: cell_frame,
                        row_ax_index: Some(42 + row_index as u32),
                        column_ax_index: Some(column_index as u32),
                        column_instance_ref: Some(object_ref(&format!("column-{column_index}"))),
                        header_instance_ref: None,
                        column_header: Some((*header).into()),
                        value: Some(value),
                    }
                })
                .collect();
            rows.push(TableViewportRow {
                instance_ref: Some(object_ref(&row_ref)),
                parent_instance_ref: object_ref("table"),
                frame: row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                ax_index: Some(42 + row_index as u32),
                accessibility_identifier: None,
                selected: None,
                ax_selected_settable: None,
                cells_complete: true,
                cells,
            });
        }
        let read_scoped_objects = objects.len() as u32;
        let snapshot = StructureSnapshot {
            snapshot_id: RequestId::new("snapshot-1").unwrap(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "test/1/monotonic".into(),
            identity_boundary: Some(SourceIdentityBoundary {
                namespace: "11111111-1111-4111-8111-111111111111".into(),
                generation: 1,
            }),
            frame_ref: None,
            source_sequence: 1,
            observed_ms: 100,
            app: AppIdentity {
                bundle_id: "test.app".into(),
                name: "Fixture".into(),
                pid: 42,
            },
            windows: vec![WindowInfo {
                window_id: "window-1".into(),
                title: Some("Records".into()),
                frame: window_frame,
                z_index: 0,
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: "test.app".into(),
            }],
            global_focus: interaction_contracts::desktop::source::GlobalFocusObservation {
                app: interaction_contracts::desktop::source::ObservationFact::Known {
                    value: interaction_contracts::desktop::source::AppIdentity {
                        bundle_id: "test.app".into(),
                        name: "Fixture".into(),
                        pid: 42,
                    },
                },
                window_id: interaction_contracts::desktop::source::ObservationFact::Known {
                    value: "window-1".into(),
                },
            },
            target_context: None,
            complete: true,
            incomplete_reason: None,
            objects,
            observer_coverage: ObserverCoverage {
                state: SourceState::Unavailable,
                covered_objects: 0,
                read_scoped_objects,
                failed_registrations: 0,
                failures: vec![],
            },
            menu_anchors: vec![],
            menu_context: None,
            focus_context: None,
            window_context: None,
            table_scope_search: None,
            table_viewports: vec![TableViewport {
                table_instance_ref: object_ref("table"),
                scroll_container_instance_ref: object_ref("scroll"),
                process_id: 42,
                ax_enabled: AxEnabledObservation::Enabled,
                table_frame,
                viewport_frame,
                visible_rows_complete: true,
                visible_rows: rows,
                columns_complete: true,
                columns,
            }],
            owned_document_evidence: None,
        };
        let target = TargetSpec {
            semantic_target: SemanticTargetRef::new("reports-row").unwrap(),
            label: "Monthly report".into(),
            group: None,
            role_hint: Some("AXRow".into()),
            row: Some(RowTargetSpec {
                table_scope: RowTableScope {
                    scroll_path: vec![RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Workspace".into(),
                    }],
                    column_headers: vec!["Name".into(), "Owner".into(), "Status".into()],
                },
                conditions: vec![
                    RowColumnConditionSpec {
                        column_name: "Name".into(),
                        value: ExpectedValue::Text {
                            value: "Monthly report".into(),
                        },
                    },
                    RowColumnConditionSpec {
                        column_name: "Owner".into(),
                        value: ExpectedValue::Text {
                            value: "Acme".into(),
                        },
                    },
                ],
            }),
        };
        (snapshot, target)
    }

    fn resolve(
        snapshot: &StructureSnapshot,
        target: &TargetSpec,
    ) -> Result<ResolvedRowTarget, RowResolutionFailure> {
        resolve_row_target(snapshot, target, 1_000, 200, 1_000)
    }

    fn owned(result: ResolvedRowTarget) -> (RowSelector, RowSelectionObservation) {
        (result.selector, result.source)
    }

    fn table_scope_candidate(
        snapshot: &StructureSnapshot,
        scope: &RowTableScope,
        table_ref: &str,
    ) -> TableScopeCandidate {
        let viewport = snapshot
            .table_viewports
            .iter()
            .find(|viewport| viewport.table_instance_ref == object_ref(table_ref))
            .unwrap();
        let scroll = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == viewport.scroll_container_instance_ref)
            .unwrap();
        TableScopeCandidate {
            table_instance_ref: viewport.table_instance_ref.clone(),
            scroll_container_instance_ref: viewport.scroll_container_instance_ref.clone(),
            scroll_path: vec![TableScopePathElement {
                instance_ref: scroll.instance_ref.clone(),
                label: scroll.description.clone().unwrap(),
                frame: scroll.frame,
            }],
            column_headers: scope.column_headers.clone(),
        }
    }

    fn attach_table_scope_search(
        snapshot: &mut StructureSnapshot,
        scope: &RowTableScope,
        table_refs: &[&str],
        state: TableScopeSearchState,
    ) {
        snapshot.complete = false;
        snapshot.incomplete_reason = Some("global structure traversal is bounded".into());
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let candidates = table_refs
            .iter()
            .map(|reference| table_scope_candidate(snapshot, scope, reference))
            .collect();
        snapshot.table_scope_search = Some(TableScopeSearch {
            requested_scope: scope.clone(),
            process_id: snapshot.app.pid,
            window_id,
            state,
            candidates,
        });
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    }

    fn attach_actual_header_refs(snapshot: &mut StructureSnapshot) {
        let headers: Vec<_> = snapshot.table_viewports[0]
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                (
                    object_ref(&format!("header-{index}")),
                    column.instance_ref.clone().unwrap(),
                    column.frame,
                    column.header.clone().unwrap(),
                )
            })
            .collect();
        for (header_ref, column_ref, frame, label) in &headers {
            snapshot.objects.push(source_object(
                header_ref.as_str(),
                Some(column_ref.as_str()),
                "AXStaticText",
                *frame,
                None,
                Some(label),
                None,
            ));
        }
        let viewport = &mut snapshot.table_viewports[0];
        for (column, (header_ref, _, _, _)) in viewport.columns.iter_mut().zip(&headers) {
            column.header_instance_ref = Some(header_ref.clone());
        }
        for row in &mut viewport.visible_rows {
            for (cell, (header_ref, _, _, _)) in row.cells.iter_mut().zip(&headers) {
                cell.header_instance_ref = Some(header_ref.clone());
            }
        }
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    }

    #[test]
    fn owner_condition_disambiguates_same_named_visible_rows_and_header_order_is_a_set() {
        let (snapshot, target) = fixture();
        let (selector, source) = owned(resolve(&snapshot, &target).unwrap());
        assert_eq!(source.row_instance_ref, object_ref("row-0"));
        assert_eq!(selector.conditions[1].column_name, "Owner");
        assert_eq!(source.selected, None);
        assert_eq!(source.ax_selected_settable, None);

        let mut reordered = target.clone();
        reordered
            .row
            .as_mut()
            .unwrap()
            .table_scope
            .column_headers
            .reverse();
        let (reordered_selector, reordered_source) = owned(resolve(&snapshot, &reordered).unwrap());
        assert_eq!(reordered_selector, selector);
        assert_eq!(reordered_source, source);
    }

    #[test]
    fn frozen_group_constraint_must_match_the_selected_row() {
        let (mut snapshot, mut target) = fixture();
        target.group = Some("Project team".into());
        assert_eq!(
            resolve(&snapshot, &target),
            Err(RowResolutionFailure::NotFound)
        );

        let row = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == object_ref("row-0"))
            .unwrap();
        row.group = Some(" project  TEAM ".into());
        assert_eq!(
            resolve(&snapshot, &target).unwrap().source.row_instance_ref,
            object_ref("row-0")
        );

        target.group = Some("Other team".into());
        assert_eq!(
            resolve(&snapshot, &target),
            Err(RowResolutionFailure::NotFound)
        );
        target.group = None;
        assert!(resolve(&snapshot, &target).is_ok());
    }

    #[test]
    fn duplicate_matching_rows_or_tables_are_ambiguous() {
        let (mut duplicate_rows, target) = fixture();
        let owner_cell_ref = duplicate_rows.table_viewports[0].visible_rows[1].cells[1]
            .instance_ref
            .clone()
            .unwrap();
        duplicate_rows.table_viewports[0].visible_rows[1].cells[1].value =
            Some(UiValue::Text("Acme".into()));
        duplicate_rows
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == owner_cell_ref)
            .unwrap()
            .value = Some(UiValue::Text("Acme".into()));
        assert_eq!(
            resolve(&duplicate_rows, &target),
            Err(RowResolutionFailure::Ambiguous)
        );

        let (mut duplicate_tables, target) = fixture();
        let original = duplicate_tables.table_viewports[0].clone();
        let second_table_ref = object_ref("table-2");
        duplicate_tables.objects.push(source_object(
            "table-2",
            Some("scroll"),
            "AXTable",
            original.table_frame,
            None,
            None,
            None,
        ));
        let mut second = original;
        second.table_instance_ref = second_table_ref.clone();
        for column in &mut second.columns {
            column.instance_ref = None;
            column.parent_instance_ref = second_table_ref.clone();
            column.header_instance_ref = None;
        }
        for (index, row) in second.visible_rows.iter_mut().enumerate() {
            let row_ref = format!("table-2-row-{index}");
            let row_object = source_object(
                &row_ref,
                Some("table-2"),
                "AXRow",
                row.frame,
                None,
                None,
                None,
            );
            duplicate_tables.objects.push(row_object);
            row.instance_ref = Some(object_ref(&row_ref));
            row.parent_instance_ref = second_table_ref.clone();
            for cell in &mut row.cells {
                cell.instance_ref = None;
                cell.parent_instance_ref = Some(object_ref(&row_ref));
                cell.column_instance_ref = None;
                cell.header_instance_ref = None;
            }
        }
        duplicate_tables.table_viewports.push(second);
        duplicate_tables.observer_coverage.read_scoped_objects =
            duplicate_tables.objects.len() as u32;
        assert_eq!(
            resolve(&duplicate_tables, &target),
            Err(RowResolutionFailure::Ambiguous)
        );
    }

    #[test]
    fn partial_rows_need_an_exact_complete_scoped_census_and_real_viewport_refs() {
        let (mut partial, target) = fixture();
        let scope = target.row.as_ref().unwrap().table_scope.clone();
        attach_actual_header_refs(&mut partial);
        attach_table_scope_search(
            &mut partial,
            &scope,
            &["table"],
            TableScopeSearchState::Complete,
        );
        assert_eq!(row_scope_coverage(&partial, &scope), Ok(()));
        assert_eq!(
            resolve(&partial, &target).unwrap().source.row_instance_ref,
            object_ref("row-0")
        );

        let mut wrong_scope = scope.clone();
        wrong_scope.scroll_path[0].label = "Another list".into();
        assert_eq!(
            row_scope_coverage(&partial, &wrong_scope),
            Err(RowResolutionFailure::InvalidScope)
        );

        let (mut missing_links, target) = fixture();
        let scope = target.row.as_ref().unwrap().table_scope.clone();
        attach_table_scope_search(
            &mut missing_links,
            &scope,
            &["table"],
            TableScopeSearchState::Complete,
        );
        assert_eq!(row_scope_coverage(&missing_links, &scope), Ok(()));
        assert_eq!(
            resolve(&missing_links, &target),
            Err(RowResolutionFailure::IncompleteSearch),
            "a unique table certificate cannot replace missing row/column/header object links"
        );

        let (mut unfinished, target) = fixture();
        let scope = target.row.as_ref().unwrap().table_scope.clone();
        attach_table_scope_search(
            &mut unfinished,
            &scope,
            &[],
            TableScopeSearchState::Incomplete {
                reason: "census budget ended".into(),
            },
        );
        assert_eq!(
            row_scope_coverage(&unfinished, &scope),
            Err(RowResolutionFailure::IncompleteSearch)
        );
    }

    #[test]
    fn a_complete_zero_candidate_census_does_not_claim_the_row_is_absent() {
        let (mut snapshot, mut target) = fixture();
        let mut scope = target.row.as_ref().unwrap().table_scope.clone();
        scope.scroll_path[0].label = "A different list".into();
        target.row.as_mut().unwrap().table_scope = scope.clone();
        attach_table_scope_search(&mut snapshot, &scope, &[], TableScopeSearchState::Complete);

        assert_eq!(row_scope_coverage(&snapshot, &scope), Ok(()));
        assert_eq!(
            resolve(&snapshot, &target),
            Err(RowResolutionFailure::IncompleteSearch),
            "zero tables only closes this scope census; it does not prove global row absence"
        );
    }

    #[test]
    fn a_complete_multi_candidate_census_keeps_table_resolution_ambiguous() {
        let (mut snapshot, target) = fixture();
        let scope = target.row.as_ref().unwrap().table_scope.clone();
        let second_table_ref = object_ref("table-2");
        let original = snapshot.table_viewports[0].clone();
        snapshot.objects.push(source_object(
            "table-2",
            Some("scroll"),
            "AXTable",
            original.table_frame,
            None,
            None,
            None,
        ));
        let mut second = original;
        second.table_instance_ref = second_table_ref;
        for column in &mut second.columns {
            column.instance_ref = None;
            column.parent_instance_ref = object_ref("table-2");
            column.header_instance_ref = None;
        }
        for (index, row) in second.visible_rows.iter_mut().enumerate() {
            let row_ref = format!("table-2-row-{index}");
            snapshot.objects.push(source_object(
                &row_ref,
                Some("table-2"),
                "AXRow",
                row.frame,
                None,
                None,
                None,
            ));
            row.instance_ref = Some(object_ref(&row_ref));
            row.parent_instance_ref = object_ref("table-2");
            for cell in &mut row.cells {
                cell.instance_ref = None;
                cell.parent_instance_ref = Some(object_ref(&row_ref));
                cell.column_instance_ref = None;
                cell.header_instance_ref = None;
            }
        }
        snapshot.table_viewports.push(second);
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        attach_table_scope_search(
            &mut snapshot,
            &scope,
            &["table", "table-2"],
            TableScopeSearchState::Complete,
        );

        assert_eq!(row_scope_coverage(&snapshot, &scope), Ok(()));
        assert_eq!(
            resolve(&snapshot, &target),
            Err(RowResolutionFailure::Ambiguous)
        );
    }

    #[test]
    fn incomplete_global_or_local_coverage_never_becomes_a_row_match() {
        let (mut incomplete, target) = fixture();
        incomplete.complete = false;
        incomplete.incomplete_reason = Some("AX traversal budget reached".into());
        assert_eq!(
            resolve(&incomplete, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );

        let (mut local_partial, target) = fixture();
        local_partial.table_viewports[0].visible_rows_complete = false;
        assert_eq!(
            resolve(&local_partial, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );

        let (mut local_columns_partial, target) = fixture();
        local_columns_partial.table_viewports[0].columns_complete = false;
        assert_eq!(
            resolve(&local_columns_partial, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );

        let (mut unknown_header, target) = fixture();
        unknown_header.table_viewports[0].columns[1].header = None;
        assert_eq!(
            resolve(&unknown_header, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );

        let (mut duplicate_header, target) = fixture();
        duplicate_header.table_viewports[0].columns[1].header = Some("Name".into());
        assert_eq!(
            resolve(&duplicate_header, &target),
            Err(RowResolutionFailure::Ambiguous)
        );

        let (mut unknown_scroll_label, target) = fixture();
        unknown_scroll_label
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == object_ref("scroll"))
            .unwrap()
            .description = None;
        assert_eq!(
            resolve(&unknown_scroll_label, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );
    }

    #[test]
    fn read_scoped_refs_are_resolved_again_from_each_fresh_snapshot() {
        let (first_snapshot, target) = fixture();
        let (_, first) = owned(resolve(&first_snapshot, &target).unwrap());
        assert_eq!(
            first_snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref == first.row_instance_ref)
                .unwrap()
                .identity_continuity,
            ObjectIdentityContinuity::ReadScoped
        );

        let (mut next_snapshot, _) = fixture();
        next_snapshot.snapshot_id = RequestId::new("snapshot-2").unwrap();
        next_snapshot.source_sequence = 2;
        next_snapshot.observed_ms = 150;
        rotate_read_scoped_refs(&mut next_snapshot);
        let (_, next) = owned(resolve(&next_snapshot, &target).unwrap());
        assert_ne!(first.row_instance_ref, next.row_instance_ref);
        assert_ne!(first.table_instance_ref, next.table_instance_ref);
        assert_eq!(next.source_sequence, 2);
    }

    #[test]
    fn wrong_window_path_and_expired_clock_fail_closed() {
        let (snapshot, target) = fixture();
        let mut wrong_path = target.clone();
        wrong_path.row.as_mut().unwrap().table_scope.scroll_path[0].label = "Other".into();
        assert_eq!(
            resolve(&snapshot, &wrong_path),
            Err(RowResolutionFailure::NotFound)
        );

        let (mut wrong_window, target) = fixture();
        let second_window_frame = rect(0.0, 0.0, 400.0, 300.0);
        wrong_window.windows.push(WindowInfo {
            window_id: "window-2".into(),
            title: Some("Other".into()),
            frame: second_window_frame,
            z_index: 1,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "test.app".into(),
        });
        let mut second_window_object = source_object(
            "window-object-2",
            None,
            "AXWindow",
            second_window_frame,
            None,
            Some("Other"),
            None,
        );
        second_window_object.window_id = "window-2".into();
        wrong_window.objects.push(second_window_object);
        wrong_window.observer_coverage.read_scoped_objects = wrong_window.objects.len() as u32;
        crate::testkit::focus_snapshot_window(&mut wrong_window, "window-2");
        assert_eq!(
            resolve(&wrong_window, &target),
            Err(RowResolutionFailure::NotFound)
        );

        assert_eq!(
            resolve_row_target(&snapshot, &target, 200, 200, 1_000),
            Err(RowResolutionFailure::InvalidScope)
        );

        let (mut wrong_parent, target) = fixture();
        wrong_parent
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == object_ref("table"))
            .unwrap()
            .parent = Some(object_ref("window-object"));
        assert_eq!(
            resolve(&wrong_parent, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );

        let (mut wrong_pid, target) = fixture();
        wrong_pid.table_viewports[0].process_id = 43;
        assert_eq!(
            resolve(&wrong_pid, &target),
            Err(RowResolutionFailure::IncompleteSearch)
        );
    }

    #[test]
    fn scroll_scope_covers_the_complete_outer_to_nearest_parent_path() {
        let (mut snapshot, mut target) = fixture();
        let frame = snapshot.table_viewports[0].viewport_frame;
        snapshot.objects.push(source_object(
            "scroll-outer",
            Some("window-object"),
            "AXScrollArea",
            frame,
            Some("Preferences content"),
            None,
            None,
        ));
        snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == object_ref("scroll"))
            .unwrap()
            .parent = Some(object_ref("scroll-outer"));
        snapshot.observer_coverage.read_scoped_objects = snapshot.objects.len() as u32;
        target.row.as_mut().unwrap().table_scope.scroll_path = vec![
            RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Preferences content".into(),
            },
            RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Workspace".into(),
            },
        ];
        let (_, source) = owned(resolve(&snapshot, &target).unwrap());
        assert_eq!(source.row_instance_ref, object_ref("row-0"));
    }

    #[test]
    fn exact_integer_conditions_convert_without_loss_and_out_of_bounds_are_invalid() {
        let (snapshot, mut target) = fixture();
        let row = target.row.as_mut().unwrap();
        row.conditions = vec![RowColumnConditionSpec {
            column_name: "Status".into(),
            value: ExpectedValue::Number {
                value: 1_000_000_000_000,
            },
        }];
        let converted = expected_value(&row.conditions[0].value).unwrap();
        assert_eq!(converted, UiValue::Number(1_000_000_000_000_f64));
        row.conditions[0].value = ExpectedValue::Number {
            value: 1_000_000_000_001,
        };
        assert_eq!(
            resolve(&snapshot, &target),
            Err(RowResolutionFailure::InvalidScope)
        );
    }
}
