import Foundation
import CoreGraphics

/// Pure checks for the bounded table-scope certificate. These tests do not
/// query Accessibility or perform scroll/input actions.
func runTableScopeSearchTests() throws -> [String] {
    func decodeScope(_ json: String) -> TableScopeSearchRequest? {
        try? JSONDecoder().decode(TableScopeSearchRequest.self, from: Data(json.utf8))
    }

    let validJSON = """
    {"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":["Archive","Name"]}
    """
    guard let request = decodeScope(validJSON) else {
        throw NativeFailure("self-test failed: valid table scope request should decode")
    }
    try requireTest(request.valid && request.scroll_path.count == 1
                    && request.column_headers == ["Archive", "Name"],
                    "exact bounded table scope request decodes")

    var wireRequest: [String: Any] = ["protocol_version": 1, "command": "observe",
        "bundle_id": "com.lingxi.computer-user.mvp-fixture", "pid": 42,
        "text_policy": ["kind": "registered_editor", "window_id": "window-1"],
        "table_scope": try JSONSerialization.jsonObject(with: Data(validJSON.utf8))]
    let decodedWire = try SourceMain.decodeRequest(JSONSerialization.data(withJSONObject: wireRequest))
    try requireTest(decodedWire.table_scope == request,
                    "production request whitelist accepts the typed fixed-window scope")
    wireRequest.removeValue(forKey: "text_policy")
    try requireTest((try? SourceMain.decodeRequest(JSONSerialization.data(withJSONObject: wireRequest))) == nil,
                    "production table scope request requires a fixed registered window")
    let ordinaryWire = Data("{\"protocol_version\":1,\"command\":\"permissions\",\"table_scope\":null}".utf8)
    let ordinaryRequest = try SourceMain.decodeRequest(ordinaryWire)
    try requireTest(ordinaryRequest.table_scope == nil,
                    "ordinary production requests accept an absent optional table scope")

    let rejects = [
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":["Name"],"conditions":[]}"#,
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences","instance_ref":"ax-1"}],"column_headers":["Name"]}"#,
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":["Name"],"row_value":"Secret"}"#,
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":["Name"],"ax_index":0}"#,
        #"{"scroll_path":[{"role":"AXScrollArea","label":"Preferences"}],"column_headers":["Name"]}"#,
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":["Name"," name "]}"#,
        #"{"scroll_path":[],"column_headers":["Name"]}"#,
        #"{"scroll_path":[{"role":"ax_scroll_area","label":"Preferences"}],"column_headers":[]}"#
    ]
    try requireTest(rejects.allSatisfy { decodeScope($0) == nil },
                    "table scope rejects row data, unknown keys, invalid roles, and malformed bounds")

    let requestedPath = ["Preferences", "Projects"]
    try requireTest(scopedTableHeaderRoleAllowed("AXButton", isHeaderLeaf: true)
                    && !scopedTableHeaderRoleAllowed("AXButton", isHeaderLeaf: false)
                    && !scopedTableHeaderRoleAllowed("AXScrollArea", isHeaderLeaf: true)
                    && !scopedTableHeaderRoleAllowed("AXTextField", isHeaderLeaf: true),
                    "only an actual column AXHeader leaf may use the button role")
    try requireTest(TableScopeSearchValidation.excludesEmptyGroup(role: "AXGroup",
                        childCountStatus: .success, childCount: 0, timely: true)
                    && !TableScopeSearchValidation.excludesEmptyGroup(role: "AXScrollArea",
                        childCountStatus: .success, childCount: 0, timely: true)
                    && !TableScopeSearchValidation.excludesEmptyGroup(role: "AXGroup",
                        childCountStatus: .success, childCount: 1, timely: true)
                    && !TableScopeSearchValidation.excludesEmptyGroup(role: "AXGroup",
                        childCountStatus: .attributeUnsupported, childCount: 0, timely: true)
                    && !TableScopeSearchValidation.excludesEmptyGroup(role: "AXGroup",
                        childCountStatus: .success, childCount: 0, timely: false),
                    "only a timely fully enumerated empty non-scope group can bypass its irrelevant parent mismatch")
    try requireTest(TableScopeSearchValidation.pathRelation(["Preferences"], requested: requestedPath) == .prefix
                    && TableScopeSearchValidation.tableDisposition(actualPath: ["Preferences"],
                        requestedPath: requestedPath, directParentIsNearestScroll: false) == .inspectChildren,
                    "short matching table path remains searchable for nested eligible scroll branches")
    try requireTest(TableScopeSearchValidation.pathRelation(requestedPath, requested: requestedPath) == .exact
                    && TableScopeSearchValidation.tableDisposition(actualPath: requestedPath,
                        requestedPath: requestedPath, directParentIsNearestScroll: true) == .candidateAndPrune
                    && TableScopeSearchValidation.tableDisposition(actualPath: requestedPath,
                        requestedPath: requestedPath, directParentIsNearestScroll: false) == .prune,
                    "only exact nearest-scroll parent can become a candidate")
    try requireTest(TableScopeSearchValidation.pathRelation(["Other"], requested: requestedPath) == .impossible
                    && TableScopeSearchValidation.pathRelation(["Preferences", "Projects", "Nested"],
                        requested: requestedPath) == .impossible,
                    "mismatched and over-deep scroll paths cannot match")
    try requireTest(TableScopeSearchValidation.headersMatch(["Name", "Archive"], requested: ["Archive", "Name"])
                    && !TableScopeSearchValidation.headersMatch(["name", "Archive"], requested: ["Name", "Archive"])
                    && !TableScopeSearchValidation.headersMatch(["Name"], requested: ["Name", "Archive"]),
                    "header matching is an order-independent exact set")
    let pruneScopedTable = TableScopeSearchValidation.globalWalkDecision(
        scopeRequested: true, censusComplete: true, roleIsTable: true,
        viewportRegistered: true, exactScopeReference: true)
    let pruneHeaderMismatch = !TableScopeSearchValidation.headersMatch(["Other"], requested: ["Name"])
        && (TableScopeSearchValidation.globalWalkDecision(
            scopeRequested: true, censusComplete: true, roleIsTable: true,
            viewportRegistered: true, exactScopeReference: true)
            == .pruneTableRowsAndMarkGlobalPartial)
    try requireTest(pruneScopedTable == .pruneTableRowsAndMarkGlobalPartial
                    && pruneHeaderMismatch
                    && (TableScopeSearchValidation.globalWalkDecision(
                        scopeRequested: false, censusComplete: true, roleIsTable: true,
                        viewportRegistered: true, exactScopeReference: true) == .traverseChildren)
                    && (TableScopeSearchValidation.globalWalkDecision(
                        scopeRequested: true, censusComplete: false, roleIsTable: true,
                        viewportRegistered: true, exactScopeReference: true) == .traverseChildren)
                    && (TableScopeSearchValidation.globalWalkDecision(
                        scopeRequested: true, censusComplete: true, roleIsTable: true,
                        viewportRegistered: false, exactScopeReference: true) == .traverseChildren)
                    && (TableScopeSearchValidation.globalWalkDecision(
                        scopeRequested: true, censusComplete: true, roleIsTable: true,
                        viewportRegistered: true, exactScopeReference: false) == .traverseChildren),
                    "global walk prunes eligible projections and marks the enclosing snapshot partial")

    let expectedScope = TableScopeSearchRequest(
        scroll_path: [TableScopeSearchStep(role: "ax_scroll_area", label: "Preferences")],
        column_headers: ["Name", "Archive"])
    let frame = Rect(CGRect(x: 10, y: 20, width: 300, height: 100))
    func candidate(_ table: String) -> TableScopeSearchCandidate {
        TableScopeSearchCandidate(table_instance_ref: table, scroll_container_instance_ref: "scroll",
            scroll_path: [TableScopeSearchPathItem(instance_ref: "scroll", label: "Preferences", frame: frame)],
            column_headers: ["Archive", "Name"])
    }
    func result(_ candidates: [TableScopeSearchCandidate]? = nil,
                traversalComplete: Bool = true, viewportComplete: Bool = true,
                boundaryStable: Bool = true, reason: String? = nil) -> TableScopeSearchResult {
        TableScopeSearchValidation.result(request: expectedScope, processID: 42, windowID: "window-1",
            candidates: candidates ?? [candidate("table")], traversalComplete: traversalComplete,
            viewportCrossLinksComplete: viewportComplete, boundaryStable: boundaryStable,
            incompleteReason: reason)
    }

    let twoCandidates = result([candidate("table-1"), candidate("table-2")])
    try requireTest(twoCandidates.state == .complete && twoCandidates.candidates.count == 2,
                    "certificate helper preserves multiple supplied candidates as ambiguous")
    let unknownChildren = result(traversalComplete: false, reason: "scope_children_unavailable")
    let unknownHeaders = result(traversalComplete: false, reason: "scope_table_headers_incomplete")
    let missingViewport = result(viewportComplete: false)
    let changedBoundary = result(boundaryStable: false)
    let failedTraversalWithUnknownBoundary = result(traversalComplete: false,
        boundaryStable: false, reason: "scope_children_unavailable")
    try requireTest(unknownChildren.state == .incomplete(reason: "scope_children_unavailable")
                    && unknownChildren.candidates.isEmpty
                    && unknownHeaders.state == .incomplete(reason: "scope_table_headers_incomplete")
                    && missingViewport.state == .incomplete(reason: "table_viewport_cross_link_incomplete")
                    && changedBoundary.state == .incomplete(reason: "source_window_or_process_changed")
                    && changedBoundary.candidates.isEmpty,
                    "unknown traversal, headers, viewport links, and changed window stay incomplete")
    try requireTest(failedTraversalWithUnknownBoundary.state == .incomplete(reason: "scope_children_unavailable")
                    && failedTraversalWithUnknownBoundary.candidates.isEmpty,
                    "failed census retains its observed cause when no boundary stamp was established")
    try requireTest(result(Array(repeating: candidate("table"), count: 33)).state
                    == .incomplete(reason: "table_candidate_limit_exceeded"),
                    "candidate overflow cannot produce a complete census")

    let app = AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 42)
    var structure = Structure(app: app)
    structure.complete = false
    structure.table_scope_search = result()
    let wire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(structure)) as? [String: Any]
    let scopedWire = wire?["table_scope_search"] as? [String: Any]
    let scopedState = scopedWire?["state"] as? [String: Any]
    var noScopeStructure = Structure(app: app)
    noScopeStructure.complete = false
    let noScopeWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(noScopeStructure)) as? [String: Any]
    try requireTest(wire?["complete"] as? Bool == false
                    && scopedState?["state"] as? String == "complete"
                    && noScopeWire?["table_scope_search"] == nil,
                    "scoped completeness neither promotes whole-tree completeness nor appears when unrequested")

    return [
        "table_scope_request_is_exact_and_bounded",
        "table_scope_path_keeps_nested_branches_and_requires_nearest_parent",
        "table_scope_headers_are_an_exact_order_independent_set",
        "table_scope_global_walk_prunes_only_projected_census_tables",
        "table_scope_helper_preserves_multiple_matching_candidates",
        "table_scope_unknown_source_evidence_stays_incomplete",
        "table_scope_evidence_does_not_upgrade_global_completeness"
    ]
}
