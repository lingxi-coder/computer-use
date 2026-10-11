use super::WatchTracker;
use crate::config::{NativeCaptureMode, NativeConfig};
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::{
    AxEnabledObservation, FrameStatus, SourceKind, SourceState,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const ORIGIN_MS: u64 = 1_000_000;
const UPTIME_NS: u64 = 10_000_000_000;
const IMAGE: &[u8] = b"bounded mock PNG bytes";

fn menu_tracker(assets: &Path) -> WatchTracker {
    let mut config = config(assets);
    config.bundle_id = "com.lingxi.computer-user.guard-fixture".into();
    config.text_policy = Some(crate::config::TextReadPolicy::RegisteredEditor {
        window_id: "window-1".into(),
    });
    WatchTracker::new(config, 7, "macos/7/wall".into(), ORIGIN_MS, 100)
}

#[test]
fn partial_window_facts_keep_raw_provenance_and_the_exact_requested_table_domain() {
    let scope = json!({"scroll_path":[{"role":"ax_scroll_area","label":"Archive"}],"column_headers":["Name"]});
    let mut record = menu_record(1, 100, "window-scope-session", false, "window-tree");
    let root = record["structure"]["objects"][0].clone();
    let root_ref = root["instance_ref"].clone();
    let root_frame = root["frame"].clone();
    let make_object = |id: &str, parent: Value, role: &str, title: Option<&str>| {
        let mut object = root.clone();
        object["instance_ref"] = json!(id);
        object["parent"] = parent;
        object["role"] = json!(role);
        object["subrole"] = Value::Null;
        object["title"] = json!(title);
        if role == "AXScrollArea" {
            object["description"] = json!(title);
        }
        object
    };
    record["structure"]["objects"] = json!([
        root,
        make_object("scope-scroll", root_ref, "AXScrollArea", Some("Archive")),
        make_object("scope-table", json!("scope-scroll"), "AXTable", None),
        make_object(
            "scope-column",
            json!("scope-table"),
            "AXColumn",
            Some("Name")
        )
    ]);
    record["structure"]["observer_coverage"]["read_scoped_objects"] = json!(4);
    record["structure"]["complete"] = json!(false);
    record["structure"]["incomplete_reason"] = json!("scoped table descendants were pruned");
    record["structure"]["menu_anchors"] = json!([]);
    record["structure"]["menu_context"] = Value::Null;
    record["structure"]["table_viewports"] = json!([{
        "table_instance_ref":"scope-table","scroll_container_instance_ref":"scope-scroll",
        "process_id":42,"ax_enabled":"enabled","table_frame":root_frame,"viewport_frame":root_frame,
        "visible_rows_complete":false,"visible_rows":[],"columns_complete":true,
        "columns":[{"instance_ref":"scope-column","parent_instance_ref":"scope-table","frame":root_frame,
            "header_instance_ref":null,"header":"Name","ax_index":0}]
    }]);
    record["structure"]["table_scope_search"] = json!({
        "requested_scope":scope,"process_id":42,"window_id":"window-1","state":{"state":"complete"},
        "candidates":[{"table_instance_ref":"scope-table","scroll_container_instance_ref":"scope-scroll",
            "scroll_path":[{"instance_ref":"scope-scroll","label":"Archive","frame":root_frame}],"column_headers":["Name"]}]
    });
    record["structure"]["window_context"] = json!({
        "source_session_id":"window-scope-session","session_epoch":7,
        "process":{"pid":42,"start_seconds":100,"start_micros":200},
        "origin_window_id":"window-1","origin_window_frame":root_frame,
        "verified_source_sequence":1,"verified_observed_ms":ORIGIN_MS+100
    });
    let assets = assets();
    let make_tracker = || {
        let mut config = config(assets.path());
        config.bundle_id = "com.lingxi.computer-user.guard-fixture".into();
        config.text_policy = Some(crate::config::TextReadPolicy::RegisteredEditor {
            window_id: "window-1".into(),
        });
        config.table_scope = Some(serde_json::from_value(scope.clone()).unwrap());
        WatchTracker::new(config, 7, "macos/7/wall".into(), ORIGIN_MS, 100)
    };
    let report = make_tracker()
        .ingest(record.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap();
    let snapshot = report.snapshot.unwrap();
    assert!(!snapshot.complete);
    assert_eq!(
        snapshot
            .window_context
            .as_ref()
            .unwrap()
            .verified_observed_ms,
        snapshot.observed_ms
    );
    assert_eq!(snapshot.observed_ms, 75);
    assert!(snapshot.focus_context.is_none());
    assert!(report.frame_meta.is_none());
    for field in ["sequence", "time", "window", "scope", "census"] {
        let mut other = record.clone();
        match field {
            "sequence" => {
                other["structure"]["window_context"]["verified_source_sequence"] = json!(2)
            }
            "time" => {
                other["structure"]["window_context"]["verified_observed_ms"] = json!(ORIGIN_MS + 99)
            }
            "window" => {
                other["structure"]["window_context"]["origin_window_id"] = json!("window-2")
            }
            "scope" => {
                other["structure"]["table_scope_search"]["requested_scope"]["column_headers"] =
                    json!(["Other"])
            }
            _ => {
                other["structure"]["table_scope_search"]["state"] =
                    json!({"state":"incomplete","reason":"deadline"})
            }
        }
        assert!(
            make_tracker()
                .ingest(other, assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn watch_table_census_requires_the_request_and_preserves_partial_read_identity() {
    let scope = json!({"scroll_path":[{"role":"ax_scroll_area", "label":"Archive"}], "column_headers":["Name"]});
    let mut record = menu_record(1, 100, "scope-session", false, "scope-tree");
    record["structure"]["complete"] = json!(false);
    record["structure"]["incomplete_reason"] = json!("AX node budget exceeded");
    record["structure"]["menu_anchors"] = json!([]);
    record["structure"]["menu_context"] = Value::Null;
    record["structure"]["table_scope_search"] = json!({
        "requested_scope":scope, "process_id":42, "window_id":"window-1",
        "state":{"state":"complete"}, "candidates":[]
    });
    let assets = assets();
    let error = menu_tracker(assets.path())
        .ingest(record.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap_err();
    assert!(
        error.contains("native table census was not requested"),
        "{error}"
    );
    let make_tracker = || {
        let mut config = config(assets.path());
        config.bundle_id = "com.lingxi.computer-user.guard-fixture".into();
        config.text_policy = Some(crate::config::TextReadPolicy::RegisteredEditor {
            window_id: "window-1".into(),
        });
        config.table_scope = Some(serde_json::from_value(scope.clone()).unwrap());
        WatchTracker::new(config, 7, "macos/7/wall".into(), ORIGIN_MS, 100)
    };
    let report = make_tracker()
        .ingest(record.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap();
    let snapshot = report.snapshot.unwrap();
    assert!(!snapshot.complete);
    assert_eq!(snapshot.source_sequence, 1);
    assert_eq!(snapshot.clock_domain, "macos/7/wall");
    assert!(
        snapshot
            .table_scope_search
            .unwrap()
            .unique_candidate()
            .is_none()
    );
    for (field, value) in [
        ("process_id", json!(43)),
        ("window_id", json!("window-2")),
        (
            "requested_scope",
            json!({"scroll_path":[{"role":"ax_scroll_area", "label":"Other"}], "column_headers":["Name"]}),
        ),
    ] {
        let mut wrong = record.clone();
        wrong["structure"]["table_scope_search"][field] = value;
        assert!(
            make_tracker()
                .ingest(wrong, assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "{field}"
        );
    }
}

fn owned_tracker(assets: &Path) -> WatchTracker {
    let mut config = config(assets);
    config.bundle_id = "com.apple.TextEdit".into();
    config.text_policy = None;
    config.owned_document_bootstrap = Some(Box::new(OwnedDocumentRegistration {
        version: 1,
        registration_id: "11111111-1111-4111-8111-111111111111".into(),
        pid: 42,
        start_seconds: 100,
        start_micros: 200,
        executable_path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit".into(),
        bundle_id: "com.apple.TextEdit".into(),
        document_path: "/private/tmp/owned-test/document.txt".into(),
        document_sha256: "a".repeat(64),
        document_dev: 1,
        document_ino: 2,
        directory_dev: 1,
        directory_ino: 3,
        issued_unix_ms: ORIGIN_MS,
    }));
    WatchTracker::new(config, 7, "macos/7/wall".into(), ORIGIN_MS, 100)
}

fn owned_sidecar(sequence: u64, callback_ms: u64, generation: u64) -> Value {
    let read_start = ORIGIN_MS + callback_ms - 25;
    let snapshot_id = format!("stream-a-tree-{sequence}");
    let receiver = "owned-receiver";
    let window = "owned-window";
    json!({
        "registration_id":"11111111-1111-4111-8111-111111111111",
        "profile":"owned_plain_text_value_setter_v1",
        "provenance":{
            "identity_boundary":{"namespace":"22222222-2222-4222-8222-222222222222","generation":generation},
            "snapshot_id":snapshot_id,"source_sequence":sequence,"observed_ms":read_start,
            "valid_until_ms":read_start+2_000,"clock_domain":"native_unix_ms"
        },
        "process":{"pid":42,"start_seconds":100,"start_micros":200},
        "window_id":"window-1","window_object_ref":window,
        "window_frame":{"x":20.0,"y":30.0,"width":160.0,"height":100.0},
        "actual_object_ref":receiver,
        "receiver_frame":{"x":30.0,"y":40.0,"width":100.0,"height":60.0},
        "application_focused_object_ref":receiver,"system_focused_object_ref":receiver,
        "ancestry":[
            {"object_ref":"owned-scroll","role":"AXScrollArea"},
            {"object_ref":window,"role":"AXWindow"}
        ],
        "scope_complete":true,"profile_eligible":true,"marked_range_stable_unmarked":true,
        "observer_state":"continuous","secure":false,"document_binding_verified":true,
        "document_binding":{
            "before":{"status":"value","api_code":0,"cf_type_id":7,"observed_ms":read_start+5,"timely":true,"registered_path_matches":true},
            "after":{"status":"value","api_code":0,"cf_type_id":7,"observed_ms":read_start+20,"timely":true,"registered_path_matches":true}
        },
        "title":{"status":"value","value":"Document"},
        "role":{"status":"value","value":"AXTextArea"},
        "subrole":{"status":"unsupported","code":-25205},
        "enabled":{"status":"unsupported","code":-25205},
        "focused":{"status":"value","value":true},
        "value_settable":{"status":"value","value":true},
        "description":{"status":"failure","code":-25200},
        "marked_range_before":{
            "receiver_object_ref":receiver,"receiver_same_cf_as_actual_object_ref":true,
            "observed_ms":read_start+8,"observation":{"status":"value","value":{"location":66,"length":0}}
        },
        "marked_range_after":{
            "receiver_object_ref":receiver,"receiver_same_cf_as_actual_object_ref":true,
            "observed_ms":read_start+15,"observation":{"status":"value","value":{"location":66,"length":0}}
        }
    })
}

fn owned_record(sequence: u64, callback_ms: u64, generation: u64) -> Value {
    let mut value = menu_record(sequence, callback_ms, "owned-source", false, "owned-tree");
    value["structure"]["app"]["bundle_id"] = json!("com.apple.TextEdit");
    value["structure"]["windows"][0]["owner_bundle_id"] = json!("com.apple.TextEdit");
    value["structure"]["complete"] = json!(false);
    value["structure"]["incomplete_reason"] =
        json!("private scope does not claim generic completeness");
    value["structure"]
        .as_object_mut()
        .unwrap()
        .remove("identity_boundary");
    value["structure"]["menu_anchors"] = json!([]);
    value["structure"]["menu_context"] = Value::Null;
    value["structure"]["owned_document_evidence"] =
        owned_sidecar(sequence, callback_ms, generation);
    value["structure_started_unix_ms"] = json!(ORIGIN_MS + callback_ms - 25);
    value["structure_finished_unix_ms"] = json!(ORIGIN_MS + callback_ms);
    value
}

fn owned_value_readback(observed_ms: u64) -> Value {
    json!({
        "receiver_object_ref":"owned-receiver",
        "receiver_same_cf_as_actual_object_ref":true,
        "observed_ms":observed_ms,
        "sha256":"abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        "utf8_byte_count":7
    })
}

fn menu_record(
    sequence: u64,
    callback_ms: u64,
    session: &str,
    open: bool,
    generation: &str,
) -> Value {
    let app = "com.lingxi.computer-user.guard-fixture";
    let root = format!("{generation}-root-read-{sequence}");
    let group = format!("{generation}-group-read-{sequence}");
    let opener = format!("{generation}-opener-read-{sequence}");
    let menu = format!("{generation}-menu-read-{sequence}");
    let root_frame = json!({"x":20.0,"y":30.0,"width":160.0,"height":100.0});
    let opener_frame = json!({"x":30.0,"y":40.0,"width":80.0,"height":25.0});
    let menu_frame = json!({"x":30.0,"y":65.0,"width":100.0,"height":54.0});
    let object = |id: String,
                  parent: Option<String>,
                  role: &str,
                  title: Option<&str>,
                  frame: Value,
                  actions: Value| {
        json!({
            "instance_ref":id,"parent":parent,"window_id":"window-1","role":role,
            "subrole":if role == "AXWindow" {Some("AXStandardWindow")} else {None},
            "title":title,"description":null,"value":null,"enabled":true,"focused":false,
            "frame":frame,"actions":actions,"in_view":true,
            "group":if ["AXMenuButton","AXMenu","AXMenuItem"].contains(&role) {Some("Menu fixture group")} else {None},
            "secure":false,"scroll":null,"identity_continuity":"read_scoped"
        })
    };
    let mut objects = vec![
        object(
            root.clone(),
            None,
            "AXWindow",
            Some("Fixture"),
            root_frame.clone(),
            json!([]),
        ),
        object(
            group.clone(),
            Some(root),
            "AXGroup",
            Some("Menu fixture group"),
            root_frame.clone(),
            json!([]),
        ),
        object(
            opener.clone(),
            Some(group),
            "AXMenuButton",
            Some("Fixture native menu"),
            opener_frame.clone(),
            json!(["show_menu"]),
        ),
    ];
    let fingerprint = |role: &str, roles: Value, label: &str, frame: Value| {
        json!({
            "app":app,"app_pid":42,"window_id":"window-1","ancestor_roles":roles,
            "group":"menu fixture group","role":role,"subrole":null,"label":label,
            "sibling_index":0,"frame":frame,"scroll_container":null
        })
    };
    let anchor = json!({
        "anchor_id":"anchor-a","source_session_id":session,"anchor_revision":1,
        "process":{"pid":42,"start_seconds":100,"start_micros":200},
        "origin_window_id":"window-1","origin_window_frame":root_frame,
        "opener_instance":opener,"opener_fingerprint":fingerprint("AXMenuButton",json!(["AXWindow","AXGroup"]),"fixture native menu",json!([30,40,80,25])),
        "opener_frame":opener_frame,"verified_source_sequence":sequence,
        "verified_observed_ms":ORIGIN_MS+callback_ms,"continuity":"direct_read"
    });
    let context = if open {
        objects.push(object(
            menu.clone(),
            Some(opener),
            "AXMenu",
            None,
            menu_frame.clone(),
            json!([]),
        ));
        for (index, y) in [(1, 70.0), (2, 92.0)] {
            objects.push(object(
                format!("{generation}-item-{index}"),
                Some(menu.clone()),
                "AXMenuItem",
                Some(if index == 1 { "First" } else { "Second" }),
                json!({"x":30.0,"y":y,"width":100.0,"height":22.0}),
                json!(["press"]),
            ));
        }
        json!({"context_id":"context-a","source_session_id":session,"opener":anchor,
            "menu_instance":menu,"menu_fingerprint":fingerprint("AXMenu",json!(["AXWindow","AXGroup","AXMenuButton"]),"",json!([30,65,100,54])),
            "menu_frame":menu_frame,"drawing_window":{"window_id":7,"owner_pid":42,"layer":101,"frame":menu_frame}})
    } else {
        Value::Null
    };
    let read_scoped_objects = objects.len();
    let mut record = record("stream-a", sequence, callback_ms, Value::Null);
    record["kind"] = json!("observation");
    record["permissions"] = json!({"accessibility":true,"screen_capture":true});
    record["structure"] = json!({"app":{"bundle_id":app,"name":"Fixture","pid":42},
        "identity_boundary":{"namespace":"11111111-1111-4111-8111-111111111111","generation":1},
        "windows":[{"window_id":"window-1","title":"Fixture","frame":root_frame,"z_index":0,"kind":"window","modal":false,"owner_bundle_id":app}],
        "global_focus":{"app":{"state":"known","value":{"bundle_id":app,"name":"Fixture","pid":42}},
            "window_id":{"state":"known","value":"window-1"}},
        "active_layer":"window-1","complete":true,"incomplete_reason":null,"objects":objects,
        "observer_coverage":{"state":"unavailable","covered_objects":0,
            "read_scoped_objects":read_scoped_objects,"failed_registrations":0,"failures":[]},
        "menu_anchors":[anchor],"menu_context":context});
    record["structure_started_uptime_ns"] = json!(UPTIME_NS + (callback_ms - 25) * 1_000_000);
    record["structure_finished_uptime_ns"] = json!(UPTIME_NS + callback_ms * 1_000_000);
    record["structure_started_unix_ms"] = json!(ORIGIN_MS + callback_ms - 25);
    record["structure_finished_unix_ms"] = json!(ORIGIN_MS + callback_ms);
    record["frame_ref"] = Value::Null;
    record["tree_revision"] = json!(sequence);
    record["observer_revision"] = json!(sequence);
    record["observer_available"] = json!(false);
    record["observer_reason"] = json!("destroy notification unsupported; direct reads");
    record["ocr"] = json!({"complete":false,"reason":"no menu pixels claimed","blocks":[]});
    record["errors"] = json!([]);
    record
}

fn table_viewport_record(ax_index: Value) -> Value {
    let mut record = menu_record(1, 100, "table-source-session", false, "stable-tree");
    let root = "stable-tree-root-read-1";
    let scroll_frame = json!({"x":20.0,"y":30.0,"width":160.0,"height":100.0});
    let table_frame = scroll_frame.clone();
    let column_frame = json!({"x":30.0,"y":40.0,"width":80.0,"height":20.0});
    let row_frame = json!({"x":30.0,"y":40.0,"width":120.0,"height":20.0});
    let cell_frame = json!({"x":30.0,"y":40.0,"width":80.0,"height":20.0});
    record["structure"]["objects"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({
                "instance_ref":"table-scroll","parent":root,"window_id":"window-1",
                "role":"AXScrollArea","subrole":null,"title":null,"description":null,
                "value":null,"enabled":true,"focused":false,"frame":scroll_frame,
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null,
                "identity_continuity":"read_scoped"
            }),
            json!({
                "instance_ref":"table-object","parent":"table-scroll","window_id":"window-1",
                "role":"AXTable","subrole":null,"title":null,"description":null,
                "value":null,"enabled":true,"focused":false,"frame":table_frame,
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null,
                "identity_continuity":"read_scoped"
            }),
            json!({
                "instance_ref":"table-row","parent":"table-object","window_id":"window-1",
                "role":"AXRow","subrole":null,"title":null,"description":null,
                "value":null,"enabled":true,"focused":false,"frame":row_frame,
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null,
                "identity_continuity":"read_scoped"
            }),
            json!({
                "instance_ref":"table-cell","parent":"table-row","window_id":"window-1",
                "role":"AXCell","subrole":null,"title":null,"description":null,
                "value":{"type":"text","value":"Fixture row"},"enabled":true,"focused":false,"frame":cell_frame,
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null,
                "identity_continuity":"read_scoped"
            }),
            json!({
                "instance_ref":"table-column","parent":"table-object","window_id":"window-1",
                "role":"AXColumn","subrole":null,"title":"Name","description":null,
                "value":null,"enabled":true,"focused":false,"frame":column_frame,
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null,
                "identity_continuity":"read_scoped"
            }),
        ]);
    record["structure"]["observer_coverage"]["read_scoped_objects"] = json!(8);
    record["structure"]["table_viewports"] = json!([{
        "table_instance_ref":"table-object",
        "scroll_container_instance_ref":"table-scroll",
        "process_id":42,
        "table_frame":table_frame,
        "viewport_frame":scroll_frame,
        "ax_enabled":"enabled",
        "visible_rows_complete":true,"visible_rows":[{
            "instance_ref":"table-row","parent_instance_ref":"table-object",
            "frame":row_frame,"accessibility_identifier":null,"ax_enabled":"enabled","selected":false,
            "ax_selected_settable":true,"ax_index":42,"cells_complete":true,
            "cells":[{
                "instance_ref":"table-cell","parent_instance_ref":"table-row",
                "frame":cell_frame,"row_ax_index":42,"column_ax_index":2,
                "column_instance_ref":"table-column","header_instance_ref":null,
                "column_header":"Name","value":{"type":"text","value":"Fixture row"}
            }]
        }],
        "columns_complete":true,
        "columns":[{
            "instance_ref":"table-column","parent_instance_ref":"table-object",
            "frame":column_frame,"header_instance_ref":null,"header":"Name",
            "ax_index":ax_index
        }]
    }]);
    record
}

#[test]
fn native_watch_identity_boundary_namespace_is_fixed_and_generation_is_monotonic() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let first = menu_record(1, 100, "same-source-session", false, "stable-tree");
    let first_snapshot = tracker
        .ingest(first, assets.path(), ORIGIN_MS + 150)
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(
        first_snapshot
            .identity_boundary
            .as_ref()
            .unwrap()
            .generation,
        1
    );

    let same = menu_record(2, 200, "same-source-session", false, "stable-tree");
    assert_eq!(
        tracker
            .ingest(same, assets.path(), ORIGIN_MS + 250)
            .unwrap()
            .snapshot
            .unwrap()
            .identity_boundary,
        first_snapshot.identity_boundary
    );

    let mut advanced = menu_record(3, 300, "same-source-session", false, "stable-tree");
    advanced["structure"]["identity_boundary"]["generation"] = json!(2);
    let advanced_boundary = tracker
        .ingest(advanced, assets.path(), ORIGIN_MS + 350)
        .unwrap()
        .snapshot
        .unwrap()
        .identity_boundary
        .unwrap();
    assert_eq!(
        advanced_boundary.namespace,
        first_snapshot.identity_boundary.as_ref().unwrap().namespace
    );
    assert_eq!(advanced_boundary.generation, 2);

    let mut regressed = menu_record(4, 400, "same-source-session", false, "stable-tree");
    regressed["structure"]["identity_boundary"]["generation"] = json!(1);
    assert!(
        tracker
            .ingest(regressed, assets.path(), ORIGIN_MS + 450)
            .unwrap_err()
            .contains("generation moved backwards")
    );

    let mut changed_namespace = menu_record(5, 500, "same-source-session", false, "stable-tree");
    changed_namespace["structure"]["identity_boundary"]["namespace"] =
        json!("22222222-2222-4222-8222-222222222222");
    changed_namespace["structure"]["identity_boundary"]["generation"] = json!(3);
    assert!(
        tracker
            .ingest(changed_namespace, assets.path(), ORIGIN_MS + 550)
            .unwrap_err()
            .contains("namespace changed")
    );
}

#[test]
fn native_watch_table_viewport_preserves_actual_row_and_cell_indexes() {
    let sample_assets_1 = assets();
    let mut tracker = menu_tracker(sample_assets_1.path());
    let snapshot = tracker
        .ingest(
            table_viewport_record(json!(2)),
            sample_assets_1.path(),
            ORIGIN_MS + 150,
        )
        .unwrap()
        .snapshot
        .unwrap();
    let row = &snapshot.table_viewports[0].visible_rows[0];
    assert_eq!(
        snapshot.table_viewports[0].ax_enabled,
        AxEnabledObservation::Enabled
    );
    assert_eq!(row.ax_enabled, AxEnabledObservation::Enabled);
    assert_eq!(row.ax_index, Some(42));
    assert_eq!(row.cells[0].row_ax_index, Some(42));
    assert_eq!(row.cells[0].column_ax_index, Some(2));
    assert_eq!(snapshot.table_viewports[0].columns[0].ax_index, Some(2));

    let sample_assets_2 = assets();
    let mut tracker = menu_tracker(sample_assets_2.path());
    let unknown = tracker
        .ingest(
            table_viewport_record(Value::Null),
            sample_assets_2.path(),
            ORIGIN_MS + 150,
        )
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(unknown.table_viewports[0].columns[0].ax_index, None);
    assert_eq!(
        unknown.table_viewports[0].ax_enabled,
        AxEnabledObservation::Enabled
    );
    assert_eq!(
        unknown.table_viewports[0].visible_rows[0].ax_index,
        Some(42)
    );
    assert_eq!(
        unknown.table_viewports[0].visible_rows[0].ax_enabled,
        AxEnabledObservation::Enabled
    );
    assert_eq!(
        unknown.table_viewports[0].visible_rows[0].cells[0].row_ax_index,
        Some(42)
    );
    assert_eq!(
        unknown.table_viewports[0].visible_rows[0].cells[0].column_ax_index,
        Some(2)
    );

    for (wire, generic_enabled, expected) in [
        ("enabled", true, AxEnabledObservation::Enabled),
        ("disabled", false, AxEnabledObservation::Disabled),
        (
            "attribute_unsupported",
            false,
            AxEnabledObservation::AttributeUnsupported,
        ),
        ("unavailable", false, AxEnabledObservation::Unavailable),
    ] {
        let mut record = table_viewport_record(json!(2));
        record["structure"]["table_viewports"][0]["ax_enabled"] = json!(wire);
        record["structure"]["table_viewports"][0]["visible_rows"][0]["ax_enabled"] = json!(wire);
        for reference in ["table-object", "table-row"] {
            let object = record["structure"]["objects"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|object| object["instance_ref"] == reference)
                .unwrap();
            object["enabled"] = json!(generic_enabled);
        }
        let sample_assets = assets();
        let mut tracker = menu_tracker(sample_assets.path());
        let snapshot = tracker
            .ingest(record, sample_assets.path(), ORIGIN_MS + 150)
            .unwrap()
            .snapshot
            .unwrap();
        assert_eq!(
            snapshot.table_viewports[0].ax_enabled, expected,
            "table {wire}"
        );
        assert_eq!(
            snapshot.table_viewports[0].visible_rows[0].ax_enabled, expected,
            "row {wire}"
        );
    }

    for scope in ["table", "row"] {
        let mut record = table_viewport_record(json!(2));
        let projection = if scope == "table" {
            &mut record["structure"]["table_viewports"][0]
        } else {
            &mut record["structure"]["table_viewports"][0]["visible_rows"][0]
        };
        projection.as_object_mut().unwrap().remove("ax_enabled");
        let sample_assets = assets();
        let mut tracker = menu_tracker(sample_assets.path());
        assert!(
            tracker
                .ingest(record, sample_assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "missing table viewport {scope}.ax_enabled must be rejected"
        );
    }

    for (target, field) in [
        ("row", "ax_index"),
        ("cell", "row_ax_index"),
        ("cell", "column_ax_index"),
    ] {
        for omit_field in [false, true] {
            let mut record = table_viewport_record(json!(2));
            let object = if target == "row" {
                &mut record["structure"]["table_viewports"][0]["visible_rows"][0]
            } else {
                &mut record["structure"]["table_viewports"][0]["visible_rows"][0]["cells"][0]
            };
            if omit_field {
                object.as_object_mut().unwrap().remove(field);
            } else {
                object[field] = Value::Null;
            }
            if field == "column_ax_index" {
                let cell =
                    &mut record["structure"]["table_viewports"][0]["visible_rows"][0]["cells"][0];
                cell["column_instance_ref"] = Value::Null;
                cell["column_header"] = Value::Null;
            }
            let sample_assets = assets();
            let mut tracker = menu_tracker(sample_assets.path());
            let snapshot = tracker
                .ingest(record, sample_assets.path(), ORIGIN_MS + 150)
                .unwrap()
                .snapshot
                .unwrap();
            let row = &snapshot.table_viewports[0].visible_rows[0];
            assert_eq!(
                row.ax_index,
                if field == "ax_index" { None } else { Some(42) }
            );
            assert_eq!(
                row.cells[0].row_ax_index,
                if field == "row_ax_index" {
                    None
                } else {
                    Some(42)
                }
            );
            assert_eq!(
                row.cells[0].column_ax_index,
                if field == "column_ax_index" {
                    None
                } else {
                    Some(2)
                }
            );
        }
    }

    let sample_assets_3 = assets();
    let mut tracker = menu_tracker(sample_assets_3.path());
    assert!(
        tracker
            .ingest(
                table_viewport_record(json!(64)),
                sample_assets_3.path(),
                ORIGIN_MS + 150
            )
            .unwrap_err()
            .contains("invalid native AX snapshot")
    );
}

#[test]
fn observation_frame_ref_is_not_inferred_from_the_last_complete_frame() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    tracker
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();

    let snapshot = tracker
        .ingest(
            menu_record(2, 200, "same-source-session", false, "stable-tree"),
            assets.path(),
            ORIGIN_MS + 200,
        )
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(snapshot.frame_ref, None);
}

fn capture_command() -> interaction_contracts::desktop::source::SourceCommand {
    interaction_contracts::desktop::source::SourceCommand::CaptureNow {
        request_id: interaction_contracts::desktop::ids::RequestId::new("capture-1").unwrap(),
        watch_id: "stream-a".into(),
        session_epoch: 7,
        surface: interaction_contracts::desktop::device::SurfaceRef {
            id: "main".into(),
            epoch: 7,
        },
        after_ms: 200,
        deadline_ms: 500,
    }
}

fn capture_result_record() -> Value {
    let png = include_bytes!("../../../../../contracts/desktop-v1/image-channel/fixture.png");
    let mut observation = menu_record(2, 250, "same-source-session", false, "capture-tree");
    observation["frame_ref"] = json!("capture-image");
    json!({ "protocol_version":1,"kind":"capture_result","stream_id":"stream-a","sequence":2,
        "received_uptime_ns":UPTIME_NS+250_000_000,"received_unix_ms":ORIGIN_MS+250,
        "watch_id":"stream-a","request_id":"capture-1","session_epoch":7,"surface":{"id":"main","epoch":7},
        "status":"complete","requested_after_ms":200,"deadline_ms":500,
        "capture_started_uptime_ns":UPTIME_NS+205_000_000,"capture_finished_uptime_ns":UPTIME_NS+215_000_000,
        "capture_started_unix_ms":ORIGIN_MS+205,"capture_finished_unix_ms":ORIGIN_MS+215,"image_frame_id":"capture-image",
        "frame":{"geometry_revision":1,"width_px":320,"height_px":200,"scale_factor":2.0,"content_scale":2.0,
            "content_rect":{"x":0.0,"y":0.0,"width":160.0,"height":100.0},"window_id":"window-1","pid":42,
            "frame":{"x":20.0,"y":30.0,"width":160.0,"height":100.0},"byte_length":png.len(),"png_file":"capture.png","frame_id":"capture-event"},
        "observation":observation })
}

#[test]
fn same_watch_capture_projects_one_exact_pair_and_keeps_original_stream_pixels() {
    let assets = assets();
    let png = include_bytes!("../../../../../contracts/desktop-v1/image-channel/fixture.png");
    fs::write(assets.path().join("capture.png"), png).unwrap();
    let mut tracker = menu_tracker(assets.path());
    tracker.bind_watch("stream-a".into());
    let mut initial = complete(1, 100, "stream-image");
    initial["frame"]["byte_length"] = json!(png.len());
    tracker
        .ingest(initial, assets.path(), ORIGIN_MS + 100)
        .unwrap();
    assert!(
        tracker
            .control_record(&capture_command(), 200)
            .unwrap()
            .is_some()
    );
    let report = tracker
        .ingest(capture_result_record(), assets.path(), ORIGIN_MS + 250)
        .unwrap();
    let frame = report.frame_meta.unwrap();
    assert!(frame.stream_info().is_none());
    assert_eq!(frame.source_ms, 203);
    assert_eq!(frame.capture_info().unwrap().capture_finished_ms, 217);
    assert_eq!(
        report.snapshot.unwrap().frame_ref.as_deref(),
        frame.image_frame_id()
    );
    assert_eq!(report.image.unwrap().as_ref(), png);
    assert!(
        !report
            .health
            .iter()
            .any(|health| health.source == SourceKind::Screenshot)
    );
    let idle = tracker
        .ingest(
            idle(3, 300, Some("stream-image")),
            assets.path(),
            ORIGIN_MS + 300,
        )
        .unwrap()
        .frame_meta
        .unwrap();
    assert_eq!(idle.source_ms, 90);
    assert_eq!(idle.image_frame_id(), Some("stream-image"));
}

#[test]
fn capture_ax_proofs_keep_their_read_stamp_when_png_publication_finishes_later() {
    let assets = assets();
    let png = include_bytes!("../../../../../contracts/desktop-v1/image-channel/fixture.png");
    fs::write(assets.path().join("capture.png"), png).unwrap();
    for mutation in ["valid", "stale-ax", "clock-skew"] {
        let mut tracker = menu_tracker(assets.path());
        tracker.bind_watch("stream-a".into());
        let mut initial = complete(1, 100, "stream-image");
        initial["frame"]["byte_length"] = json!(png.len());
        tracker
            .ingest(initial, assets.path(), ORIGIN_MS + 100)
            .unwrap();
        tracker.control_record(&capture_command(), 200).unwrap();
        let mut capture = capture_result_record();
        capture["received_uptime_ns"] = json!(UPTIME_NS + 265_000_000);
        capture["received_unix_ms"] = json!(ORIGIN_MS + 265);
        if mutation == "stale-ax" {
            capture["observation"]["structure_started_uptime_ns"] = json!(UPTIME_NS + 200_000_000);
            capture["observation"]["structure_started_unix_ms"] = json!(ORIGIN_MS + 200);
        } else if mutation == "clock-skew" {
            capture["observation"]["received_unix_ms"] = json!(ORIGIN_MS + 265);
            capture["observation"]["structure_started_unix_ms"] = json!(ORIGIN_MS + 240);
            capture["observation"]["structure_finished_unix_ms"] = json!(ORIGIN_MS + 265);
            capture["observation"]["structure"]["menu_anchors"][0]["verified_observed_ms"] =
                json!(ORIGIN_MS + 265);
        }
        let result = tracker.ingest(capture, assets.path(), ORIGIN_MS + 265);
        assert_eq!(result.is_ok(), mutation == "valid", "{mutation}");
        if let Ok(report) = result {
            let snapshot = report.snapshot.unwrap();
            assert_eq!(snapshot.observed_ms, 225);
            assert_eq!(snapshot.menu_anchors[0].verified_observed_ms, 225);
            assert!(report.frame_meta.unwrap().capture_info().is_some());
        }
    }
}

#[test]
fn capture_failure_is_correlated_without_health_and_cancel_discards_late_pixels() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    tracker.bind_watch("stream-a".into());
    tracker
        .ingest(
            complete(1, 100, "stream-image"),
            assets.path(),
            ORIGIN_MS + 100,
        )
        .unwrap();
    tracker.control_record(&capture_command(), 200).unwrap();
    let mut failure = capture_result_record();
    failure["status"] = json!("failed");
    failure["code"] = json!("capture_unavailable");
    for key in [
        "frame",
        "observation",
        "image_frame_id",
        "capture_started_uptime_ns",
        "capture_finished_uptime_ns",
        "capture_started_unix_ms",
        "capture_finished_unix_ms",
    ] {
        failure.as_object_mut().unwrap().remove(key);
    }
    let report = tracker
        .ingest(failure, assets.path(), ORIGIN_MS + 250)
        .unwrap();
    assert_eq!(
        report.capture_failure.unwrap().request_id.as_str(),
        "capture-1"
    );
    assert!(report.health.is_empty());
    assert!(report.frame_meta.is_none());
    tracker.control_record(&capture_command(), 260).unwrap();
    tracker
        .control_record(
            &interaction_contracts::desktop::source::SourceCommand::CancelCapture {
                request_id: interaction_contracts::desktop::ids::RequestId::new("capture-1")
                    .unwrap(),
                watch_id: "stream-a".into(),
            },
            270,
        )
        .unwrap();
    let mut late = capture_result_record();
    late["sequence"] = json!(3);
    let report = tracker
        .ingest(late, assets.path(), ORIGIN_MS + 280)
        .unwrap();
    assert!(report.frame_meta.is_none());
    assert!(report.image.is_none());
    assert!(report.snapshot.is_none());
}

#[test]
fn complete_native_watch_rejects_missing_or_invalid_identity_boundary() {
    for (missing, namespace, generation) in [
        (true, "11111111-1111-4111-8111-111111111111", 1),
        (false, "11111111-1111-4111-8111-111111111111", 0),
        (false, "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA", 1),
        (false, "not-a-uuid", 1),
    ] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        let mut record = menu_record(1, 100, "same-source-session", false, "stable-tree");
        if missing {
            record["structure"]
                .as_object_mut()
                .unwrap()
                .remove("identity_boundary");
        } else {
            record["structure"]["identity_boundary"]["namespace"] = json!(namespace);
            record["structure"]["identity_boundary"]["generation"] = json!(generation);
        }
        assert!(
            tracker
                .ingest(record, assets.path(), ORIGIN_MS + 150)
                .is_err()
        );
    }
}

#[test]
fn native_watch_validates_object_coverage_and_bounds_failures_to_read_scoped_objects() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let mut partial = menu_record(1, 100, "partial-coverage-session", false, "partial-tree");
    partial["observer_available"] = json!(true);
    partial["structure"]["objects"][0]["identity_continuity"] = json!("notification_covered");
    let failed_ref = partial["structure"]["objects"][2]["instance_ref"].clone();
    partial["structure"]["observer_coverage"] = json!({
        "state":"degraded", "covered_objects":1, "read_scoped_objects":2,
        "failed_registrations":1,
        "failures":[{"instance_ref":failed_ref,
            "notification":"ui_element_destroyed", "error_code":-25205}]
    });
    let report = tracker
        .ingest(partial.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap();
    assert!(report.snapshot.as_ref().unwrap().complete);
    assert_eq!(
        report
            .snapshot
            .as_ref()
            .unwrap()
            .observer_coverage
            .covered_objects,
        1
    );
    assert_eq!(
        report
            .snapshot
            .as_ref()
            .unwrap()
            .observer_coverage
            .read_scoped_objects,
        2
    );
    assert_eq!(
        report
            .health
            .iter()
            .find(|health| health.source == SourceKind::Ax)
            .unwrap()
            .state,
        SourceState::Available,
        "partial observer coverage does not downgrade bounded tree traversal"
    );
    assert_eq!(
        report
            .health
            .iter()
            .find(|health| health.source == SourceKind::AxObserver)
            .unwrap()
            .state,
        SourceState::Degraded
    );

    let mut missing_object = partial.clone();
    let mut missing_object_tracker = menu_tracker(assets.path());
    missing_object["structure"]["observer_coverage"]["failures"][0]["instance_ref"] =
        json!("not-in-the-projection");
    assert!(
        missing_object_tracker
            .ingest(missing_object, assets.path(), ORIGIN_MS + 250)
            .is_err()
    );

    let mut covered_failure = partial;
    let mut covered_failure_tracker = menu_tracker(assets.path());
    let covered_ref = covered_failure["structure"]["objects"][0]["instance_ref"].clone();
    covered_failure["structure"]["observer_coverage"]["failures"][0]["instance_ref"] = covered_ref;
    assert!(
        covered_failure_tracker
            .ingest(covered_failure, assets.path(), ORIGIN_MS + 350)
            .is_err()
    );
}

#[test]
fn read_scoped_watch_tokens_cannot_be_reused_across_structure_sequences() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let mut first = menu_record(1, 100, "read-scope-session", false, "stable-tree");
    let first_root = first["structure"]["objects"][0]["instance_ref"].clone();
    tracker
        .ingest(first.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap();

    first["sequence"] = json!(2);
    first["structure"]["menu_anchors"][0]["verified_source_sequence"] = json!(2);
    let replayed_read = tracker
        .ingest(first, assets.path(), ORIGIN_MS + 150)
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(replayed_read.source_sequence, 2);
    assert_eq!(
        replayed_read.objects[0].instance_ref.as_str(),
        first_root
            .as_str()
            .expect("the saved read-scoped instance ref is a string")
    );

    tracker
        .ingest(idle(3, 200, None), assets.path(), ORIGIN_MS + 250)
        .unwrap();
    let mut second = menu_record(4, 300, "read-scope-session", false, "stable-tree");
    second["structure"]["objects"][0]["instance_ref"] = first_root.clone();
    second["structure"]["objects"][1]["parent"] = first_root;
    assert!(
        tracker
            .ingest(second, assets.path(), ORIGIN_MS + 350)
            .unwrap_err()
            .contains("reused a read-scoped object token")
    );
}

#[test]
fn read_scoped_duplicate_index_evicts_after_eight_distinct_ax_reads() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let mut first_root = None;
    for sequence in 1..=9 {
        let callback_ms = sequence * 100;
        let mut record = menu_record(
            sequence,
            callback_ms,
            "bounded-history-session",
            false,
            "bounded-history-tree",
        );
        if sequence == 1 {
            first_root = Some(record["structure"]["objects"][0]["instance_ref"].clone());
        }
        record["structure"]["menu_anchors"] = json!([]);
        tracker
            .ingest(record, assets.path(), ORIGIN_MS + callback_ms + 50)
            .unwrap();
        assert_eq!(
            tracker.read_scoped_reads.len(),
            (sequence as usize).min(super::MAX_TRACKED_AX_READ_IDENTITIES),
            "the recent duplicate index stays bounded"
        );
    }

    let old_root = first_root.unwrap();
    let mut after_eviction = menu_record(
        10,
        1_000,
        "bounded-history-session",
        false,
        "bounded-history-tree",
    );
    after_eviction["structure"]["menu_anchors"] = json!([]);
    after_eviction["structure"]["objects"][0]["instance_ref"] = old_root.clone();
    after_eviction["structure"]["objects"][1]["parent"] = old_root;
    tracker
        .ingest(after_eviction, assets.path(), ORIGIN_MS + 1_050)
        .unwrap();
    assert_eq!(
        tracker.read_scoped_reads.len(),
        super::MAX_TRACKED_AX_READ_IDENTITIES
    );
}

#[test]
fn native_watch_rejects_missing_unknown_or_mismatched_observer_coverage() {
    for mutation in [
        "missing",
        "unknown_state",
        "mismatched_count",
        "channel_mismatch",
        "missing_read_stamp",
        "mismatched_read_stamp",
    ] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        let mut record = menu_record(1, 100, "coverage-schema-session", false, "coverage-tree");
        match mutation {
            "missing" => {
                record["structure"]
                    .as_object_mut()
                    .unwrap()
                    .remove("observer_coverage");
            }
            "unknown_state" => {
                record["structure"]["observer_coverage"]["state"] = json!("unknown");
            }
            "mismatched_count" => {
                record["structure"]["observer_coverage"]["read_scoped_objects"] = json!(2);
            }
            "channel_mismatch" => record["observer_available"] = json!(true),
            "missing_read_stamp" => {
                record
                    .as_object_mut()
                    .unwrap()
                    .remove("structure_started_unix_ms");
            }
            "mismatched_read_stamp" => {
                record["structure_started_unix_ms"] = json!(ORIGIN_MS + 100 - 225);
            }
            _ => unreachable!(),
        }
        assert!(
            tracker
                .ingest(record, assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "{mutation}"
        );
    }
}

fn focus_record(sequence: u64, callback_ms: u64) -> Value {
    let mut value = menu_record(
        sequence,
        callback_ms,
        "focus-session",
        false,
        "focus-generation",
    );
    let anchor = value["structure"]["menu_anchors"][0].clone();
    value["structure"]["objects"][2]["focused"] = json!(true);
    value["structure"]["menu_anchors"] = json!([]);
    value["structure"]["focus_context"] = json!({
        "source_session_id":"focus-session", "session_epoch":7,
        "process":anchor["process"], "origin_window_id":anchor["origin_window_id"],
        "origin_window_frame":anchor["origin_window_frame"],
        "target_instance":anchor["opener_instance"], "target_fingerprint":anchor["opener_fingerprint"],
        "target_frame":anchor["opener_frame"], "focused_instance":anchor["opener_instance"],
        "relation":"exact_control", "composition":{"kind":"not_applicable"},
        "keyboard":{"selected_input_source":null,"underlying_keyboard_layout":null,"keyboard_type":null,"caps_lock":null},
        "verified_source_sequence":sequence,"verified_observed_ms":ORIGIN_MS+callback_ms
    });
    value
}

fn text_composition_record(sequence: u64, callback_ms: u64, composition: &str) -> Value {
    let mut value = focus_record(sequence, callback_ms);
    let instance = value["structure"]["focus_context"]["focused_instance"].clone();
    value["structure"]["objects"][2]["role"] = json!("AXTextArea");
    value["structure"]["focus_context"]["target_fingerprint"]["role"] = json!("AXTextArea");
    value["structure"]["focus_context"]["composition"] = json!({
        "kind": composition,
        "proof": {
            "kind":"ax_text_input_marked_range",
            "focused_instance":instance,
            "verified_source_sequence":sequence,
            "verified_observed_ms":ORIGIN_MS+callback_ms
        }
    });
    value
}

#[test]
fn focus_raw_provenance_is_verified_then_normalized_to_one_outer_publication() {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let mut snapshot = tracker
        .ingest(focus_record(1, 100), assets.path(), ORIGIN_MS + 150)
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(snapshot.observed_ms, 75);
    assert_eq!(
        snapshot
            .focus_context
            .as_ref()
            .unwrap()
            .verified_observed_ms,
        75
    );
    assert_eq!(snapshot.focus_context.as_ref().unwrap().session_epoch, 7);
    crate::native::normalize_snapshot_provenance(&mut snapshot, 99);
    assert_eq!(
        snapshot
            .focus_context
            .as_ref()
            .unwrap()
            .verified_source_sequence,
        99
    );
    assert_eq!(
        snapshot
            .focus_context
            .as_ref()
            .unwrap()
            .verified_observed_ms,
        snapshot.observed_ms
    );
    snapshot.validate().unwrap();
}

#[test]
fn text_composition_verifies_raw_proof_before_stamping_to_snapshot_clock() {
    for composition in ["marked", "unmarked"] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        let raw = text_composition_record(1, 100, composition);
        assert_eq!(
            raw["structure"]["focus_context"]["verified_source_sequence"],
            1
        );
        assert_eq!(
            raw["structure"]["focus_context"]["verified_observed_ms"],
            ORIGIN_MS + 100
        );
        let mut snapshot = tracker
            .ingest(raw, assets.path(), ORIGIN_MS + 150)
            .unwrap()
            .snapshot
            .unwrap();
        assert_eq!(snapshot.observed_ms, 75);
        let focus = snapshot.focus_context.as_ref().unwrap();
        assert_eq!(focus.verified_source_sequence, 1);
        assert_eq!(focus.verified_observed_ms, 75);
        let proof = match &focus.composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => proof,
            other => panic!("expected positive text composition, got {other:?}"),
        };
        assert_eq!(proof.focused_instance, focus.focused_instance);
        assert_eq!(
            proof.verified_source_sequence,
            focus.verified_source_sequence
        );
        assert_eq!(proof.verified_observed_ms, focus.verified_observed_ms);
        snapshot.validate().unwrap();

        crate::native::normalize_snapshot_provenance(&mut snapshot, 99);
        let focus = snapshot.focus_context.as_ref().unwrap();
        let proof = match &focus.composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => proof,
            other => panic!("expected positive text composition, got {other:?}"),
        };
        assert_eq!(focus.verified_source_sequence, 99);
        assert_eq!(focus.verified_observed_ms, snapshot.observed_ms);
        assert_eq!(proof.verified_source_sequence, 99);
        assert_eq!(proof.verified_observed_ms, snapshot.observed_ms);
        snapshot.validate().unwrap();

        let mut malformed = snapshot.clone();
        let focus = malformed.focus_context.as_mut().unwrap();
        match &mut focus.composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => {
                proof.verified_observed_ms += 1;
            }
            _ => unreachable!(),
        }
        let proof_time_before = match &malformed.focus_context.as_ref().unwrap().composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => {
                proof.verified_observed_ms
            }
            _ => unreachable!(),
        };
        crate::native::normalize_snapshot_provenance(&mut malformed, 100);
        let malformed_focus = malformed.focus_context.as_ref().unwrap();
        let proof_time_after = match &malformed_focus.composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => {
                proof.verified_observed_ms
            }
            _ => unreachable!(),
        };
        assert_eq!(
            proof_time_after, proof_time_before,
            "normalization must not repair a bad raw proof"
        );
        assert!(malformed.validate().is_err());
    }
}

#[test]
fn malformed_text_composition_proofs_are_rejected_before_normalization() {
    for mutation in ["kind", "receiver", "time", "sequence", "nontext-role"] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        let mut value = text_composition_record(1, 100, "marked");
        match mutation {
            "kind" => {
                value["structure"]["focus_context"]["composition"]["proof"]["kind"] =
                    json!("ax_marked_text_marker_range")
            }
            "receiver" => {
                value["structure"]["focus_context"]["composition"]["proof"]["focused_instance"] =
                    json!("unrelated-control")
            }
            "time" => {
                value["structure"]["focus_context"]["composition"]["proof"]["verified_observed_ms"] =
                    json!(ORIGIN_MS + 99)
            }
            "sequence" => {
                value["structure"]["focus_context"]["composition"]["proof"]["verified_source_sequence"] =
                    json!(2)
            }
            "nontext-role" => {
                value["structure"]["objects"][2]["role"] = json!("AXButton");
                value["structure"]["focus_context"]["target_fingerprint"]["role"] =
                    json!("AXButton");
            }
            _ => unreachable!(),
        }
        assert!(
            tracker
                .ingest(value, assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn focus_rejects_bad_raw_stamps_scope_field_editor_and_privacy_without_repair() {
    for mutation in [
        "sequence",
        "time",
        "epoch",
        "process",
        "window",
        "partial",
        "unfocused",
        "secure",
        "field-editor",
        "frame",
        "text-composition",
    ] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        let mut value = focus_record(1, 100);
        match mutation {
            "sequence" => {
                value["structure"]["focus_context"]["verified_source_sequence"] = json!(2)
            }
            "time" => {
                value["structure"]["focus_context"]["verified_observed_ms"] = json!(ORIGIN_MS + 99)
            }
            "epoch" => value["structure"]["focus_context"]["session_epoch"] = json!(8),
            "process" => value["structure"]["focus_context"]["process"]["pid"] = json!(43),
            "window" => value["structure"]["focus_context"]["origin_window_id"] = json!("window-2"),
            "partial" => value["structure"]["complete"] = json!(false),
            "unfocused" => value["structure"]["objects"][2]["focused"] = json!(false),
            "secure" => value["structure"]["objects"][2]["secure"] = json!(true),
            "field-editor" => {
                value["structure"]["focus_context"]["focused_instance"] =
                    json!("different-actual-field-editor")
            }
            "frame" => value["structure"]["objects"][2]["frame"]["x"] = json!(31.0),
            "text-composition" => {
                value["structure"]["objects"][2]["role"] = json!("AXTextField");
                value["structure"]["focus_context"]["target_fingerprint"]["role"] =
                    json!("AXTextField");
            }
            _ => unreachable!(),
        }
        assert!(
            tracker
                .ingest(value, assets.path(), ORIGIN_MS + 150)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn focus_session_and_kernel_stamp_cannot_be_replaced_within_one_watch() {
    for mutation in ["session", "kernel"] {
        let assets = assets();
        let mut tracker = menu_tracker(assets.path());
        tracker
            .ingest(focus_record(1, 100), assets.path(), ORIGIN_MS + 150)
            .unwrap();
        let mut next = focus_record(2, 200);
        if mutation == "session" {
            next["structure"]["focus_context"]["source_session_id"] =
                json!("another-owned-session");
        } else {
            next["structure"]["focus_context"]["process"]["start_seconds"] = json!(11);
        }
        assert!(
            tracker
                .ingest(next, assets.path(), ORIGIN_MS + 250)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn focus_requires_fixed_authorized_window_and_unknown_text_composition_stays_unknown() {
    let assets = assets();
    let mut unregistered = tracker(assets.path());
    assert!(
        unregistered
            .ingest(focus_record(1, 100), assets.path(), ORIGIN_MS + 150)
            .is_err()
    );
    let mut allowed = menu_tracker(assets.path());
    let mut text = focus_record(1, 100);
    text["structure"]["objects"][2]["role"] = json!("AXTextField");
    text["structure"]["focus_context"]["target_fingerprint"]["role"] = json!("AXTextField");
    text["structure"]["focus_context"]["composition"] = json!({"kind":"unknown"});
    let snapshot = allowed
        .ingest(text, assets.path(), ORIGIN_MS + 150)
        .unwrap()
        .snapshot
        .unwrap();
    assert!(matches!(
        snapshot.focus_context.unwrap().composition,
        interaction_contracts::desktop::source::CompositionState::Unknown
    ));
}

#[test]
fn menu_anchor_retention_survives_ordinary_registry_generation_and_source_normalizes_all_provenance()
 {
    let assets = assets();
    let mut tracker = menu_tracker(assets.path());
    let first = tracker
        .ingest(
            menu_record(1, 100, "ax-session", false, "g1"),
            assets.path(),
            ORIGIN_MS + 150,
        )
        .unwrap()
        .snapshot
        .unwrap();
    let mut current = tracker
        .ingest(
            menu_record(2, 200, "ax-session", true, "g2"),
            assets.path(),
            ORIGIN_MS + 250,
        )
        .unwrap()
        .snapshot
        .unwrap();
    assert!(first.menu_anchors[0].same_identity(&current.menu_anchors[0]));
    assert_ne!(
        first.menu_anchors[0].opener_instance,
        current.menu_anchors[0].opener_instance
    );
    assert_eq!(current.observed_ms, 175);
    crate::native::normalize_snapshot_provenance(&mut current, 99);
    assert_eq!(current.menu_anchors[0].verified_source_sequence, 99);
    assert_eq!(
        current.menu_anchors[0].verified_observed_ms,
        current.observed_ms
    );
    assert_eq!(
        current.menu_context.as_ref().unwrap().opener,
        current.menu_anchors[0]
    );
    current.validate().unwrap();
}

#[test]
fn native_menu_provenance_rejects_untrusted_scope_stale_raw_stamps_session_change_and_false_drawing()
 {
    let assets = assets();
    let mut untrusted = tracker(assets.path());
    let record = menu_record(1, 100, "ax-session", false, "g1");
    assert!(
        untrusted
            .ingest(record.clone(), assets.path(), ORIGIN_MS + 150)
            .is_err()
    );
    for field in ["verified_source_sequence", "verified_observed_ms"] {
        let mut tracker = menu_tracker(assets.path());
        let mut bad = record.clone();
        bad["structure"]["menu_anchors"][0][field] = json!(999);
        assert!(tracker.ingest(bad, assets.path(), ORIGIN_MS + 150).is_err());
    }
    let mut tracker = menu_tracker(assets.path());
    tracker
        .ingest(record.clone(), assets.path(), ORIGIN_MS + 150)
        .unwrap();
    assert!(
        tracker
            .ingest(
                menu_record(2, 200, "new-ax-session", true, "g2"),
                assets.path(),
                ORIGIN_MS + 250
            )
            .is_err()
    );
    let mut tracker = menu_tracker(assets.path());
    let mut bad = menu_record(1, 100, "ax-session", true, "g1");
    bad["structure"]["menu_context"]["drawing_window"]["frame"]["x"] = json!(31.0);
    assert!(tracker.ingest(bad, assets.path(), ORIGIN_MS + 150).is_err());
    for mutation in ["capacity", "process", "parent", "partial"] {
        let mut tracker = menu_tracker(assets.path());
        let mut bad = record.clone();
        match mutation {
            "capacity" => {
                bad["structure"]["menu_anchors"] =
                    json!(vec![bad["structure"]["menu_anchors"][0].clone(); 9])
            }
            "process" => bad["structure"]["menu_anchors"][0]["process"]["start_seconds"] = json!(0),
            "parent" => bad["structure"]["objects"][2]["parent"] = json!("unknown-parent"),
            "partial" => {
                bad["structure"]["complete"] = json!(false);
                bad["structure"]["incomplete_reason"] = json!("AX query failed");
            }
            _ => unreachable!(),
        }
        assert!(
            tracker.ingest(bad, assets.path(), ORIGIN_MS + 150).is_err(),
            "{mutation}"
        );
    }
}

fn config(assets: &Path) -> NativeConfig {
    NativeConfig {
        helper_path: assets.join("unused-helper"),
        bundle_id: "test.app".into(),
        pid: Some(42),
        assets_dir: assets.into(),
        timeout_ms: 1_000,
        mode: NativeCaptureMode::Watch,
        table_scope: None,
        background_target: None,
        text_policy: None,
        owned_document_bootstrap: None,
    }
}

fn tracker(assets: &Path) -> WatchTracker {
    WatchTracker::new(config(assets), 7, "macos/7/wall".into(), ORIGIN_MS, 100)
}

fn assets() -> tempfile::TempDir {
    let assets = tempfile::tempdir().unwrap();
    fs::write(assets.path().join("capture.png"), IMAGE).unwrap();
    assets
}

fn frame(status: &str, image_id: Option<&str>, callback_ms: u64) -> Value {
    let complete = status == "complete";
    json!({
        "status": status,
        "display_time_ticks": complete.then_some(UPTIME_NS + (callback_ms - 10) * 1_000_000),
        "display_time_uptime_ns": complete.then_some(UPTIME_NS + (callback_ms - 10) * 1_000_000),
        "source_unix_ms": complete.then_some(ORIGIN_MS + callback_ms - 10),
        "geometry_revision": 1,
        "width_px": 320,
        "height_px": 200,
        "scale_factor": 2.0,
        "content_scale": 2.0,
        "content_rect": {"x":0.0,"y":0.0,"width":160.0,"height":100.0},
        "dirty_rects": [],
        "frame_id": image_id,
        "window_id": "window-1",
        "pid": 42,
        "frame": {"x":20.0,"y":30.0,"width":160.0,"height":100.0},
        "byte_length": if complete { IMAGE.len() as u64 } else { 0 },
        "png_file": complete.then_some("capture.png"),
    })
}

fn record(stream: &str, sequence: u64, callback_ms: u64, frame: Value) -> Value {
    json!({
        "protocol_version": 1,
        "kind": "frame",
        "stream_id": stream,
        "sequence": sequence,
        "received_uptime_ns": UPTIME_NS + callback_ms * 1_000_000,
        "received_unix_ms": ORIGIN_MS + callback_ms,
        "frame": frame,
    })
}

fn complete(sequence: u64, callback_ms: u64, image_id: &str) -> Value {
    record(
        "stream-a",
        sequence,
        callback_ms,
        frame("complete", Some(image_id), callback_ms),
    )
}

fn idle(sequence: u64, callback_ms: u64, image_id: Option<&str>) -> Value {
    record(
        "stream-a",
        sequence,
        callback_ms,
        frame("idle", image_id, callback_ms),
    )
}

#[test]
fn idle_preserves_original_pixels_and_only_advances_callback_provenance() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    let first = tracker
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 150)
        .unwrap();
    let original = first.frame_meta.unwrap();
    assert_eq!(original.source_ms, 90);
    assert_eq!(original.byte_length, IMAGE.len() as u64);
    assert_eq!(original.checksum, format!("{:x}", Sha256::digest(IMAGE)));
    let original_stream = original.stream_info().unwrap().clone();

    let mut idle = idle(2, 900, Some("image-a"));
    // SDK IDLE metadata is not a new capture timestamp and must not refresh the image.
    idle["frame"]["display_time_ticks"] = json!(UPTIME_NS + 900_000_000);
    idle["frame"]["display_time_uptime_ns"] = json!(UPTIME_NS + 900_000_000);
    idle["frame"]["source_unix_ms"] = json!(ORIGIN_MS + 900);
    let report = tracker
        .ingest(idle, assets.path(), ORIGIN_MS + 1_200)
        .unwrap();
    let meta = report.frame_meta.unwrap();
    assert_eq!(meta.status, FrameStatus::Idle);
    assert_eq!(meta.source_ms, original.source_ms);
    assert_eq!(meta.checksum, original.checksum);
    assert_eq!(meta.byte_length, 0);
    assert_eq!(meta.source_sequence, 2);
    let stream = meta.stream_info().unwrap().clone();
    assert_eq!(stream.image_frame_id, original_stream.image_frame_id);
    assert_eq!(
        stream.display_time_ticks,
        original_stream.display_time_ticks
    );
    assert_eq!(stream.display_time_ns, original_stream.display_time_ns);
    assert_eq!(stream.callback_ms, 900);
    assert_eq!(stream.received_uptime_ns, UPTIME_NS + 900_000_000);
    assert_eq!(report.health[0].state, SourceState::Available);
}

#[test]
fn first_idle_and_wrong_image_idle_cannot_establish_current_pixels() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    let report = tracker
        .ingest(
            idle(1, 100, Some("unknown-image")),
            assets.path(),
            ORIGIN_MS + 100,
        )
        .unwrap();
    let meta = report.frame_meta.unwrap();
    assert!(meta.stream_info().unwrap().clone().image_frame_id.is_none());
    assert!(meta.checksum.is_empty());
    assert_eq!(report.health[0].state, SourceState::Unavailable);

    tracker
        .ingest(complete(2, 200, "image-a"), assets.path(), ORIGIN_MS + 200)
        .unwrap();
    let report = tracker
        .ingest(
            idle(3, 300, Some("image-b")),
            assets.path(),
            ORIGIN_MS + 300,
        )
        .unwrap();
    assert!(
        report
            .frame_meta
            .unwrap()
            .stream_info()
            .unwrap()
            .clone()
            .image_frame_id
            .is_none()
    );
    assert_eq!(report.health[0].state, SourceState::Unavailable);
}

#[test]
fn geometry_changes_on_idle_clear_pixels_until_a_new_complete_image() {
    let assets = assets();
    let changes = [
        ("window_id", json!("window-2")),
        ("width_px", json!(321)),
        ("scale_factor", json!(1.0)),
        ("content_scale", json!(1.0)),
        (
            "frame",
            json!({"x":21.0,"y":30.0,"width":160.0,"height":100.0}),
        ),
        (
            "content_rect",
            json!({"x":1.0,"y":0.0,"width":160.0,"height":100.0}),
        ),
    ];
    for (field, value) in changes {
        let mut tracker = tracker(assets.path());
        tracker
            .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
            .unwrap();
        let mut changed = idle(2, 200, Some("image-a"));
        changed["frame"]["geometry_revision"] = json!(2);
        changed["frame"][field] = value.clone();
        let report = tracker
            .ingest(changed, assets.path(), ORIGIN_MS + 200)
            .unwrap();
        assert!(
            report
                .frame_meta
                .unwrap()
                .stream_info()
                .unwrap()
                .clone()
                .image_frame_id
                .is_none(),
            "{field}"
        );
        assert_eq!(report.health[0].state, SourceState::Unavailable, "{field}");

        let mut replacement = complete(3, 300, "image-b");
        replacement["frame"]["geometry_revision"] = json!(2);
        replacement["frame"][field] = value;
        let report = tracker
            .ingest(replacement, assets.path(), ORIGIN_MS + 300)
            .unwrap();
        assert_eq!(
            report
                .frame_meta
                .unwrap()
                .stream_info()
                .unwrap()
                .clone()
                .image_frame_id
                .as_deref(),
            Some("image-b")
        );
        assert_eq!(report.health[0].state, SourceState::Available);
    }
}

#[test]
fn sequence_must_be_contiguous_and_the_stream_identity_cannot_change() {
    let assets = assets();
    for (stream, sequence) in [
        ("stream-a", 0),
        ("stream-a", 1),
        ("stream-a", 3),
        ("stream-b", 2),
    ] {
        let mut tracker = tracker(assets.path());
        tracker
            .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
            .unwrap();
        let next = record(stream, sequence, 200, frame("idle", Some("image-a"), 200));
        assert!(
            tracker
                .ingest(next, assets.path(), ORIGIN_MS + 200)
                .is_err(),
            "{stream}/{sequence}"
        );
    }
}

#[test]
fn a_helper_session_must_start_at_sequence_one() {
    let assets = assets();
    for sequence in [0, 2, u64::MAX] {
        assert!(
            tracker(assets.path())
                .ingest(idle(sequence, 100, None), assets.path(), ORIGIN_MS + 100)
                .is_err(),
            "initial sequence {sequence}"
        );
    }
    let mut tracker = tracker(assets.path());
    tracker
        .ingest(idle(1, 100, None), assets.path(), ORIGIN_MS + 100)
        .unwrap();
    tracker
        .ingest(idle(2, 200, None), assets.path(), ORIGIN_MS + 200)
        .unwrap();
    assert!(
        tracker
            .ingest(idle(1, 300, None), assets.path(), ORIGIN_MS + 300)
            .is_err()
    );
}

#[test]
fn completed_images_reject_replayed_identity_display_time_and_inconsistent_source_time() {
    let assets = assets();
    let mut cases = Vec::new();
    cases.push(complete(2, 200, "image-a"));
    for display_ns in [
        UPTIME_NS + 90_000_000,
        UPTIME_NS + 89_000_000,
        UPTIME_NS + 201_000_000,
    ] {
        let mut value = complete(2, 200, "image-b");
        value["frame"]["display_time_uptime_ns"] = json!(display_ns);
        cases.push(value);
    }
    let mut inconsistent = complete(2, 200, "image-b");
    inconsistent["frame"]["source_unix_ms"] = json!(ORIGIN_MS + 195);
    cases.push(inconsistent);
    for value in cases {
        let mut tracker = tracker(assets.path());
        tracker
            .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
            .unwrap();
        assert!(
            tracker
                .ingest(value, assets.path(), ORIGIN_MS + 200)
                .is_err()
        );
    }
}

#[test]
fn clock_discontinuities_and_stale_or_future_callbacks_are_rejected() {
    let assets = assets();
    for received_unix_ms in [ORIGIN_MS + 50, ORIGIN_MS + 350] {
        let mut tracker = tracker(assets.path());
        tracker
            .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
            .unwrap();
        let mut next = idle(2, 200, Some("image-a"));
        next["received_unix_ms"] = json!(received_unix_ms);
        assert!(
            tracker
                .ingest(next, assets.path(), ORIGIN_MS + 400)
                .is_err()
        );
    }
    for now in [ORIGIN_MS + 99, ORIGIN_MS + 3_101] {
        assert!(
            tracker(assets.path())
                .ingest(idle(1, 100, None), assets.path(), now)
                .is_err()
        );
    }
    let mut predates_origin = idle(1, 100, None);
    predates_origin["received_unix_ms"] = json!(ORIGIN_MS - 1);
    assert!(
        tracker(assets.path())
            .ingest(predates_origin, assets.path(), ORIGIN_MS)
            .is_err()
    );
}

#[test]
fn matching_backward_wall_and_uptime_clocks_are_still_rejected() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    tracker
        .ingest(complete(1, 200, "image-a"), assets.path(), ORIGIN_MS + 200)
        .unwrap();
    assert!(
        tracker
            .ingest(
                idle(2, 100, Some("image-a")),
                assets.path(),
                ORIGIN_MS + 200
            )
            .is_err()
    );
}

#[test]
fn ordered_records_can_carry_older_callbacks_from_a_different_source_lane() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    tracker
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();

    let mut observation = record("stream-a", 2, 300, Value::Null);
    observation["kind"] = json!("observation");
    observation["permissions"] = json!({"accessibility":true,"screen_capture":true});
    observation["structure"] = json!({
        "app":{"bundle_id":"test.app","name":"Test","pid":42},
        "identity_boundary":{"namespace":"11111111-1111-4111-8111-111111111111","generation":1},
        "windows":[], "active_layer":null, "complete":true,
        "incomplete_reason":null, "objects":[],
        "observer_coverage":{"state":"available","covered_objects":0,
            "read_scoped_objects":0,"failed_registrations":0,"failures":[]},
        "menu_anchors":[], "menu_context":null,
    });
    observation["structure_started_uptime_ns"] = json!(UPTIME_NS + 250_000_000);
    observation["structure_finished_uptime_ns"] = json!(UPTIME_NS + 300_000_000);
    observation["frame_ref"] = json!("image-a");
    observation["tree_revision"] = json!(1);
    observation["observer_revision"] = json!(1);
    observation["observer_available"] = json!(true);
    observation["observer_reason"] = Value::Null;
    observation["ocr"] = json!({"complete":true,"reason":null,"blocks":[]});
    observation["errors"] = json!([]);
    observation["observer_available"] = json!(false);
    observation["observer_reason"] = json!("destroy notification unsupported");
    observation["structure"]["observer_coverage"] = json!({"state":"unavailable",
        "covered_objects":0,"read_scoped_objects":0,"failed_registrations":0,"failures":[]});
    let report = tracker
        .ingest(observation, assets.path(), ORIGIN_MS + 400)
        .unwrap();
    assert_eq!(
        report
            .health
            .iter()
            .find(|h| h.source == SourceKind::Ax)
            .unwrap()
            .state,
        SourceState::Available
    );
    let observer = report
        .health
        .iter()
        .find(|h| h.source == SourceKind::AxObserver)
        .unwrap();
    assert_eq!(observer.state, SourceState::Unavailable);
    assert_eq!(
        observer.reason.as_deref(),
        Some("AX notification observer is unavailable")
    );
    let snapshot = report.snapshot.unwrap();
    assert_eq!(snapshot.observed_ms, 250);
    assert_eq!(snapshot.frame_ref.as_deref(), Some("image-a"));

    let mut health = record("stream-a", 3, 300, Value::Null);
    health["kind"] = json!("health");
    health["source"] = json!("ax");
    health["state"] = json!("available");
    health["reason"] = Value::Null;
    let report = tracker
        .ingest(health, assets.path(), ORIGIN_MS + 400)
        .unwrap();
    assert_eq!(report.health[0].at_ms, 300);

    // The queued IDLE callback is newer than the previous frame callback but
    // older than the intervening AX read; its wall/uptime mapping still agrees.
    let report = tracker
        .ingest(
            idle(4, 200, Some("image-a")),
            assets.path(),
            ORIGIN_MS + 400,
        )
        .unwrap();
    let meta = report.frame_meta.unwrap();
    assert_eq!(meta.source_ms, 90);
    assert_eq!(meta.stream_info().unwrap().clone().callback_ms, 200);
    assert_eq!(report.health[0].state, SourceState::Available);
}

#[test]
fn a_new_tracker_starts_without_the_previous_stream_image_or_drop_counts() {
    let assets = assets();
    let mut old = tracker(assets.path());
    old.ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();
    let mut gap = idle(2, 200, None);
    gap["kind"] = json!("gap");
    gap["dropped_frames"] = json!(4);
    old.ingest(gap, assets.path(), ORIGIN_MS + 200).unwrap();
    let mut fresh = WatchTracker::new(
        config(assets.path()),
        8,
        "macos/8/wall".into(),
        ORIGIN_MS,
        100,
    );
    let mut first = idle(1, 300, Some("image-a"));
    first["stream_id"] = json!("stream-b");
    let report = fresh.ingest(first, assets.path(), ORIGIN_MS + 300).unwrap();
    let meta = report.frame_meta.unwrap();
    assert_eq!(meta.session_epoch, 8);
    assert_eq!(meta.surface.epoch, 8);
    let stream = meta.stream_info().unwrap().clone();
    assert_eq!(stream.stream_id, "stream-b");
    assert!(stream.image_frame_id.is_none());
    assert_eq!(stream.dropped_frames, 0);
    assert_eq!(report.health[0].state, SourceState::Unavailable);
}

#[test]
fn gap_counts_accumulate_and_a_gap_revokes_the_cached_complete_image() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    tracker
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();
    for (sequence, callback_ms, dropped) in [(2, 200, 4), (4, 400, 2)] {
        let mut gap = idle(sequence, callback_ms, None);
        gap["kind"] = json!("gap");
        gap["dropped_frames"] = json!(dropped);
        let report = tracker
            .ingest(gap, assets.path(), ORIGIN_MS + callback_ms)
            .unwrap();
        assert_eq!(report.health.len(), 4);
        assert!(
            report
                .health
                .iter()
                .all(|health| health.state == SourceState::Unavailable)
        );
        if sequence == 2 {
            let report = tracker
                .ingest(
                    idle(3, 300, Some("image-a")),
                    assets.path(),
                    ORIGIN_MS + 300,
                )
                .unwrap();
            let stream = report.frame_meta.unwrap().stream_info().unwrap().clone();
            assert_eq!(stream.dropped_frames, 4);
            assert!(stream.image_frame_id.is_none());
        }
    }
    let report = tracker
        .ingest(complete(5, 500, "image-b"), assets.path(), ORIGIN_MS + 500)
        .unwrap();
    assert_eq!(
        report
            .frame_meta
            .unwrap()
            .stream_info()
            .unwrap()
            .clone()
            .dropped_frames,
        6
    );
}

#[test]
fn wrong_target_and_invalid_frame_geometry_never_publish_metadata() {
    let assets = assets();
    for (field, value) in [
        ("pid", json!(0)),
        ("pid", json!(43)),
        ("window_id", json!("")),
        ("width_px", json!(0)),
        ("width_px", json!(4_097)),
        ("height_px", json!(4_097)),
        ("geometry_revision", json!(0)),
        ("scale_factor", json!(0.0)),
        ("content_scale", json!(-1.0)),
        (
            "frame",
            json!({"x":0.0,"y":0.0,"width":-1.0,"height":100.0}),
        ),
        (
            "dirty_rects",
            json!(vec![json!({"x":0.0,"y":0.0,"width":1.0,"height":1.0}); 257]),
        ),
    ] {
        let mut value_record = complete(1, 100, "image-a");
        value_record["frame"][field] = value;
        assert!(
            tracker(assets.path())
                .ingest(value_record, assets.path(), ORIGIN_MS + 100)
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn complete_assets_require_bounded_regular_files_and_checksum_the_actual_bytes() {
    let assets = assets();
    for (field, value) in [
        ("png_file", json!("../capture.png")),
        ("png_file", json!("/tmp/capture.png")),
        ("png_file", Value::Null),
        ("byte_length", json!(0)),
        ("byte_length", json!(IMAGE.len() + 1)),
        ("byte_length", json!(12 * 1024 * 1024 + 1)),
    ] {
        let mut record = complete(1, 100, "image-a");
        record["frame"][field] = value;
        assert!(
            tracker(assets.path())
                .ingest(record, assets.path(), ORIGIN_MS + 100)
                .is_err(),
            "{field}"
        );
    }
    let outside = assets.path().join("target.png");
    fs::write(&outside, IMAGE).unwrap();
    std::os::unix::fs::symlink(&outside, assets.path().join("link.png")).unwrap();
    let mut link = complete(1, 100, "image-a");
    link["frame"]["png_file"] = json!("link.png");
    assert!(
        tracker(assets.path())
            .ingest(link, assets.path(), ORIGIN_MS + 100)
            .is_err()
    );

    let report = tracker(assets.path())
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();
    assert_eq!(
        report.frame_meta.unwrap().checksum,
        format!("{:x}", Sha256::digest(IMAGE))
    );
}

#[test]
fn idle_cannot_supply_new_bytes_and_nonimage_statuses_revoke_existing_pixels() {
    let assets = assets();
    for field in ["byte_length", "png_file"] {
        let mut record = idle(1, 100, None);
        record["frame"][field] = if field == "byte_length" {
            json!(1)
        } else {
            json!("capture.png")
        };
        assert!(
            tracker(assets.path())
                .ingest(record, assets.path(), ORIGIN_MS + 100)
                .is_err()
        );
    }
    for status in ["blank", "suspended", "started", "stopped"] {
        let mut tracker = tracker(assets.path());
        tracker
            .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
            .unwrap();
        let report = tracker
            .ingest(
                record("stream-a", 2, 200, frame(status, Some("image-a"), 200)),
                assets.path(),
                ORIGIN_MS + 200,
            )
            .unwrap();
        assert!(
            report
                .frame_meta
                .unwrap()
                .stream_info()
                .unwrap()
                .clone()
                .image_frame_id
                .is_none(),
            "{status}"
        );
        assert_eq!(report.health[0].state, SourceState::Unavailable, "{status}");
    }
}

#[test]
fn health_uses_the_native_callback_clock_and_identity_instead_of_arrival_time() {
    let assets = assets();
    let report = tracker(assets.path())
        .ingest(
            complete(1, 100, "image-a"),
            assets.path(),
            ORIGIN_MS + 1_500,
        )
        .unwrap();
    let health = &report.health[0];
    assert_eq!(health.source, SourceKind::Screenshot);
    assert_eq!(health.at_ms, 100);
    let stamp = health.provenance.as_ref().unwrap();
    assert_eq!(stamp.session_epoch, 7);
    assert_eq!(stamp.stream_id, "stream-a");
    assert_eq!(stamp.source_sequence, 1);
    assert_eq!(
        report
            .frame_meta
            .unwrap()
            .stream_info()
            .unwrap()
            .clone()
            .callback_ms,
        100
    );
}

#[test]
fn clock_rollback_failure_uses_the_maximum_accepted_callback_without_refreshing_pixels() {
    let assets = assets();
    let mut tracker = tracker(assets.path());
    tracker
        .ingest(complete(1, 100, "image-a"), assets.path(), ORIGIN_MS + 100)
        .unwrap();

    let mut observation = record("stream-a", 2, 200, Value::Null);
    observation["kind"] = json!("observation");
    observation["permissions"] = json!({"accessibility":true,"screen_capture":true});
    observation["structure"] = json!({
        "app":{"bundle_id":"test.app","name":"Test","pid":42},
        "identity_boundary":{"namespace":"11111111-1111-4111-8111-111111111111","generation":1},
        "windows":[], "active_layer":null, "complete":true,
        "incomplete_reason":null, "objects":[],
        "observer_coverage":{"state":"available","covered_objects":0,
            "read_scoped_objects":0,"failed_registrations":0,"failures":[]},
        "menu_anchors":[], "menu_context":null,
    });
    observation["structure_started_uptime_ns"] = json!(UPTIME_NS + 175_000_000);
    observation["structure_finished_uptime_ns"] = json!(UPTIME_NS + 200_000_000);
    observation["frame_ref"] = json!("image-a");
    observation["tree_revision"] = json!(1);
    observation["observer_revision"] = json!(1);
    observation["observer_available"] = json!(true);
    observation["observer_reason"] = Value::Null;
    observation["ocr"] = json!({"complete":true,"reason":null,"blocks":[]});
    observation["errors"] = json!([]);
    let ax = tracker
        .ingest(observation, assets.path(), ORIGIN_MS + 250)
        .unwrap();
    assert!(ax.health.iter().all(|health| health.at_ms == 200));

    let meta = tracker
        .ingest(
            idle(3, 150, Some("image-a")),
            assets.path(),
            ORIGIN_MS + 250,
        )
        .unwrap()
        .frame_meta
        .unwrap();
    assert_eq!(meta.source_ms, 90);
    assert_eq!(meta.stream_info().unwrap().callback_ms, 150);
    let original_complete = tracker.last_complete.clone();

    for now_unix_ms in [ORIGIN_MS + 50, ORIGIN_MS - 1] {
        let fault = tracker.failure_report("native wall clock mapping changed", now_unix_ms);
        assert_eq!(fault.stream_id, "stream-a");
        assert_eq!(fault.sequence, 4);
        let stopped = fault.frame_meta.as_ref().unwrap();
        assert_eq!(stopped.status, FrameStatus::Stopped);
        assert_eq!(stopped.source_sequence, 4);
        assert_eq!(stopped.source_ms, meta.source_ms);
        assert_eq!(stopped.checksum, meta.checksum);
        assert_eq!(stopped.byte_length, 0);
        assert_eq!(stopped.geometry_revision, meta.geometry_revision);
        assert_eq!(stopped.width_px, meta.width_px);
        assert_eq!(stopped.height_px, meta.height_px);
        let stopped_stream = stopped.stream_info().unwrap();
        let original_stream = meta.stream_info().unwrap();
        assert!(stopped_stream.image_frame_id.is_none());
        assert_eq!(stopped_stream.callback_ms, 150);
        assert_eq!(
            stopped_stream.received_unix_ms,
            original_stream.received_unix_ms
        );
        assert_eq!(
            stopped_stream.received_uptime_ns,
            original_stream.received_uptime_ns
        );
        assert_eq!(
            stopped_stream.display_time_ns,
            original_stream.display_time_ns
        );
        assert_eq!(
            stopped_stream.display_time_ticks,
            original_stream.display_time_ticks
        );
        assert!(
            fault.snapshot.is_none(),
            "failure invents no AX observation"
        );
        assert_eq!(fault.health.len(), 4);
        for health in &fault.health {
            assert_eq!(health.state, SourceState::Unavailable);
            assert_eq!(
                health.at_ms, 200,
                "the accepted AX callback remains the time floor"
            );
            let stamp = health.provenance.as_ref().unwrap();
            assert_eq!(stamp.session_epoch, 7);
            assert_eq!(stamp.stream_id, "stream-a");
            assert_eq!(stamp.source_sequence, 4);
        }
        assert_eq!(tracker.sequence(), 3);
        assert_eq!(tracker.last_meta.as_ref(), Some(&meta));
        assert_eq!(tracker.last_complete, original_complete);
    }
}

#[test]
fn owned_document_sidecar_maps_its_own_clock_without_upgrading_generic_health() {
    let assets = assets();
    let mut tracker = owned_tracker(assets.path());
    let report = tracker
        .ingest(owned_record(1, 1_000, 3), assets.path(), ORIGIN_MS + 1_010)
        .unwrap();
    let snapshot = report.snapshot.unwrap();
    let evidence = snapshot.owned_document_evidence.as_ref().unwrap();
    assert!(!snapshot.complete);
    assert!(snapshot.identity_boundary.is_none());
    assert_eq!(snapshot.observer_coverage.state, SourceState::Unavailable);
    assert_eq!(snapshot.observed_ms, 975);
    assert_eq!(evidence.provenance.clock_domain, "macos/7/wall");
    assert_eq!(evidence.provenance.snapshot_id.as_str(), "stream-a-tree-1");
    assert_eq!(evidence.provenance.source_sequence, 1);
    assert_eq!(evidence.provenance.observed_ms, 975);
    assert_eq!(evidence.document_binding.before.observed_ms, 980);
    assert_eq!(evidence.document_binding.after.observed_ms, 995);
    assert_eq!(evidence.marked_range_before.observed_ms, 983);
    assert_eq!(evidence.marked_range_after.observed_ms, 990);
    assert!(evidence.value_readback.is_none());
    assert_eq!(report.health[0].state, SourceState::Degraded);
    assert_eq!(
        report
            .health
            .iter()
            .find(|health| health.source == SourceKind::AxObserver)
            .unwrap()
            .state,
        SourceState::Unavailable,
        "private owned scope does not upgrade generic observer health"
    );
}

#[test]
fn owned_value_readback_maps_from_native_time_and_stays_optional() {
    let assets = assets();
    let mut tracker = owned_tracker(assets.path());
    let mut record = owned_record(1, 1_000, 3);
    record["structure"]["owned_document_evidence"]["value_readback"] =
        owned_value_readback(ORIGIN_MS + 993);
    let report = tracker
        .ingest(record, assets.path(), ORIGIN_MS + 1_010)
        .unwrap();
    let snapshot = report.snapshot.unwrap();
    let evidence = snapshot.owned_document_evidence.as_ref().unwrap();
    let readback = evidence.value_readback.as_ref().unwrap();
    assert_eq!(readback.observed_ms, 993);
    assert_eq!(evidence.provenance.clock_domain, "macos/7/wall");
    assert_eq!(readback.utf8_byte_count, 7);
    assert_eq!(readback.sha256.len(), 64);
    assert!(!snapshot.complete);
    assert_eq!(report.health[0].state, SourceState::Degraded);
}

#[test]
fn owned_value_readback_rejects_out_of_scope_times_and_invalid_receiver_digest_or_size() {
    for mutation in [
        "before_start",
        "after_finish",
        "after_receive",
        "wrong_receiver",
        "wrong_cf",
        "bad_hash",
        "oversized",
    ] {
        let assets = assets();
        let mut tracker = owned_tracker(assets.path());
        let mut record = owned_record(1, 1_000, 3);
        let readback = match mutation {
            // Keep the value read inside provenance validity so rejection
            // specifically exercises the structure read window.
            "before_start" => {
                record["structure"]["owned_document_evidence"]["provenance"]["observed_ms"] =
                    json!(ORIGIN_MS + 970);
                owned_value_readback(ORIGIN_MS + 974)
            }
            // Extend the receipt wall stamp by 10ms (within the clock-jump
            // tolerance) so +1001 is not merely rejected as future.
            "after_finish" => {
                record["received_unix_ms"] = json!(ORIGIN_MS + 1_010);
                owned_value_readback(ORIGIN_MS + 1_001)
            }
            "after_receive" => owned_value_readback(ORIGIN_MS + 1_011),
            "wrong_receiver" => {
                let mut value = owned_value_readback(ORIGIN_MS + 993);
                value["receiver_object_ref"] = json!("other-receiver");
                value
            }
            "wrong_cf" => {
                let mut value = owned_value_readback(ORIGIN_MS + 993);
                value["receiver_same_cf_as_actual_object_ref"] = json!(false);
                value
            }
            "bad_hash" => {
                let mut value = owned_value_readback(ORIGIN_MS + 993);
                value["sha256"] = json!("A".repeat(64));
                value
            }
            "oversized" => {
                let mut value = owned_value_readback(ORIGIN_MS + 993);
                value["utf8_byte_count"] = json!(2_049);
                value
            }
            _ => unreachable!(),
        };
        record["structure"]["owned_document_evidence"]["value_readback"] = readback;
        assert!(
            tracker
                .ingest(record, assets.path(), ORIGIN_MS + 1_010)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn owned_document_sidecar_rejects_unregistered_mismatched_and_stale_records() {
    let unregistered_assets = assets();
    let mut unregistered = config(unregistered_assets.path());
    unregistered.bundle_id = "com.apple.TextEdit".into();
    let mut tracker = WatchTracker::new(unregistered, 7, "macos/7/wall".into(), ORIGIN_MS, 100);
    assert!(
        tracker
            .ingest(
                owned_record(1, 1_000, 3),
                unregistered_assets.path(),
                ORIGIN_MS + 1_010,
            )
            .is_err()
    );

    for mutation in [
        "registration",
        "process",
        "snapshot",
        "clock",
        "expiry",
        "read_after_finish",
    ] {
        let assets = assets();
        let mut tracker = owned_tracker(assets.path());
        let mut value = owned_record(1, 1_000, 3);
        match mutation {
            "registration" => {
                value["structure"]["owned_document_evidence"]["registration_id"] =
                    json!("33333333-3333-4333-8333-333333333333")
            }
            "process" => {
                value["structure"]["owned_document_evidence"]["process"]["start_micros"] =
                    json!(201)
            }
            "snapshot" => {
                value["structure"]["owned_document_evidence"]["provenance"]["snapshot_id"] =
                    json!("stream-a-tree-9")
            }
            "clock" => {
                value["structure"]["owned_document_evidence"]["provenance"]["clock_domain"] =
                    json!("sim/7/monotonic")
            }
            "expiry" => {
                value["structure"]["owned_document_evidence"]["provenance"]["valid_until_ms"] =
                    json!(ORIGIN_MS + 1_000)
            }
            "read_after_finish" => {
                value["structure"]["owned_document_evidence"]["document_binding"]["after"]["observed_ms"] =
                    json!(ORIGIN_MS + 1_001)
            }
            _ => unreachable!(),
        }
        assert!(
            tracker
                .ingest(value, assets.path(), ORIGIN_MS + 1_010)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn owned_document_private_identity_boundary_is_fixed_and_generation_monotonic() {
    let assets = assets();
    let mut tracker = owned_tracker(assets.path());
    tracker
        .ingest(owned_record(1, 1_000, 3), assets.path(), ORIGIN_MS + 1_010)
        .unwrap();
    tracker
        .ingest(owned_record(2, 1_100, 4), assets.path(), ORIGIN_MS + 1_110)
        .unwrap();
    let backwards = owned_record(3, 1_200, 3);
    assert!(
        tracker
            .ingest(backwards.clone(), assets.path(), ORIGIN_MS + 1_210)
            .unwrap_err()
            .contains("identity boundary changed or moved backwards")
    );
    let mut changed_namespace = owned_record(4, 1_300, 5);
    changed_namespace["structure"]["owned_document_evidence"]["provenance"]["identity_boundary"]
        ["namespace"] = json!("44444444-4444-4444-8444-444444444444");
    assert!(
        tracker
            .ingest(changed_namespace, assets.path(), ORIGIN_MS + 1_310)
            .unwrap_err()
            .contains("identity boundary changed or moved backwards")
    );
}
