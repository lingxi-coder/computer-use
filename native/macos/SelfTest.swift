import Foundation
import CoreGraphics
import CoreText
import ApplicationServices

private let visiblePrivacyEmptyGroupTestFrame = Rect(CGRect(x: 930, y: 285, width: 120, height: 40))

struct SelfTestResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "self_test"
    let passed = true
    let test_count: Int
    let tests: [String]
}

private func runBackgroundAXWireTests() throws -> [String] {
    func request(target: [String: Any], command: String = "watch",
                 additions: [String: Any] = [:]) -> NativeRequest? {
        var fields: [String: Any] = [
            "protocol_version": 1, "command": command,
            "bundle_id": "com.example.app", "pid": 42,
            "output_dir": "/tmp/source-assets", "timeout_ms": 1000,
            "interval_ms": 1000, "clock_origin_unix_ms": 10,
            "session_epoch": 5, "watch_id": "watch-background",
            "surface": ["id": "main", "epoch": 5],
            "background_target": target,
        ]
        for (key, value) in additions { fields[key] = value }
        guard let data = try? JSONSerialization.data(withJSONObject: fields) else { return nil }
        return try? SourceMain.decodeRequest(data)
    }
    let validUnpinned = request(target: ["window_id": "window-7"])
    try requireTest(validUnpinned?.background_target?.process == nil,
                    "background watch accepts a window-only request and latches process birth")
    let validPinned = request(target: ["window_id": "window-7", "process": [
        "pid": 42, "start_seconds": 1_700_000_000, "start_micros": 123_456,
    ]])
    try requireTest(validPinned?.background_target?.process?.start_micros == 123_456,
                    "background watch accepts one exact complete kernel birth pair")
    try requireTest(request(target: ["window_id": "window-07"]) == nil,
                    "background watch rejects noncanonical window ids")
    try requireTest(request(target: ["window_id": "window-7", "process": [
        "pid": 42, "start_seconds": 1_700_000_000,
    ]]) == nil, "background watch rejects partial kernel birth pairs")
    try requireTest(request(target: ["window_id": "window-7", "process": [
        "pid": 43, "start_seconds": 1_700_000_000, "start_micros": 123_456,
    ]]) == nil, "background watch rejects a process birth from another pid")
    try requireTest(request(target: ["window_id": "window-7"], command: "observe") == nil,
                    "background target is watch-only")
    let tableScope: [String: Any] = ["scroll_path": [["role": "ax_scroll_area", "label": "Archive"]],
                                     "column_headers": ["Name"]]
    try requireTest(request(target: ["window_id": "window-7"], additions: ["table_scope": tableScope]) == nil,
                    "background AX-only request cannot combine with table census")
    try requireTest(request(target: ["window_id": "window-7"], additions: ["text_policy": [
        "kind": "registered_editor", "window_id": "window-7",
    ]]) == nil, "background AX-only request cannot combine with registered text scope")
    return [
        "background_ax_watch_wire_accepts_optional_process_birth",
        "background_ax_watch_wire_rejects_noncanonical_or_partial_scope",
        "background_ax_watch_wire_rejects_wrong_pid_and_cross_scope",
    ]
}

private func runBackgroundAXActionNameTests() throws -> [String] {
    try requireTest(validBackgroundAXActionNames([]), "empty action list is a complete actual list")
    try requireTest(validBackgroundAXActionNames((0..<32).map { "AXAction\($0)" }),
                    "complete action list accepts its 32-name bound")
    try requireTest(!validBackgroundAXActionNames((0..<33).map { "AXAction\($0)" }),
                    "action list over the bound cannot be truncated")
    try requireTest(validBackgroundAXActionNames([String(repeating: "a", count: 64)]),
                    "action name accepts its exact UTF-8 bound")
    try requireTest(!validBackgroundAXActionNames([String(repeating: "a", count: 65)])
                    && !validBackgroundAXActionNames([String(repeating: "é", count: 33)]),
                    "action name over the byte bound is unknown")
    try requireTest(!validBackgroundAXActionNames(["AXPress", "AXPress"])
                    && !validBackgroundAXActionNames([""]),
                    "duplicate or empty action names are unknown")
    let ceiling = backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: 50_000_000)
    let remainder = backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: 3_000_000)
    try requireTest(ceiling.map { Double($0) <= 0.025 && $0 > 0 } == true
                    && remainder.map { Double($0) <= 0.003 && $0 > 0 } == true
                    && backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: 0) == nil,
                    "background AX timeout is capped by both 25 ms and remaining deadline")
    return ["background_ax_action_names_are_complete_bounded_and_unique",
            "background_ax_query_timeout_never_exceeds_remaining_deadline"]
}

private func runGlobalFocusReconciliationTests() throws -> [String] {
    let readApp = AppIdentity(bundle_id: "com.example.editor", name: "Read", pid: 42)
    let currentApp = AppIdentity(bundle_id: "com.example.editor", name: "Current", pid: 42)
    let read = GlobalFocusObservation(app: .known(readApp), window_id: .known("window-8"))
    let current = GlobalFocusObservation(app: .known(currentApp), window_id: .unavailable)
    let sameProcess = reconcileGlobalFocusObservation(read: read, current: current)
    guard case .known(let sameApp) = sameProcess.app,
          case .known(let sameWindow) = sameProcess.window_id else {
        throw NativeFailure("same foreground process lost its independently read focused window")
    }
    try requireTest(sameApp.pid == 42 && sameApp.bundle_id == readApp.bundle_id
                    && sameWindow == "window-8",
                    "same foreground process retains its actual read window")

    let changed = GlobalFocusObservation(
        app: .known(AppIdentity(bundle_id: "com.example.other", name: "Other", pid: 7)),
        window_id: .unavailable)
    let replaced = reconcileGlobalFocusObservation(read: read, current: changed)
    let changedWindowUnavailable: Bool
    if case .unavailable = replaced.window_id { changedWindowUnavailable = true }
    else { changedWindowUnavailable = false }
    try requireTest(changedWindowUnavailable,
                    "changed foreground app cannot retain the prior AX window")
    let withoutReadWindow = reconcileGlobalFocusObservation(
        read: GlobalFocusObservation(app: .known(readApp), window_id: .unavailable), current: current)
    let missingWindowUnavailable: Bool
    if case .unavailable = withoutReadWindow.window_id { missingWindowUnavailable = true }
    else { missingWindowUnavailable = false }
    try requireTest(missingWindowUnavailable,
                    "a missing actual AX focused-window read stays unavailable")
    return ["global_focus_window_requires_same_current_app_and_actual_read"]
}

func requireTest(_ condition: @autoclosure () -> Bool, _ name: String) throws {
    guard condition() else { throw NativeFailure("self-test failed: \(name)") }
}

/// Reusable partial-read fixture for privacy and capture consumers. Its AX IDs
/// intentionally vary by read while all semantic and geometric facts stay fixed.
func visiblePrivacySelfTestRead(
    identitySuffix: String = "first",
    selected: Bool = false,
    cellValue: UiValue? = .text("Archive"),
    childFrame: Rect = Rect(CGRect(x: -1150, y: 360, width: 300, height: 40)),
    rootChildrenSource: VisiblePrivacyChildrenSource = .visibleChildren,
    started: UInt64 = 1,
    finished: UInt64 = 4,
    proofStarted: UInt64 = 2,
    proofFinished: UInt64 = 3,
    complete: Bool = false
) -> StructureRead {
    let app = AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 1)
    let windowID = "window-1"
    let windowFrame = Rect(CGRect(x: -1200, y: 320, width: 800, height: 600))
    let scrollFrame = Rect(CGRect(x: -1180, y: 340, width: 760, height: 540))
    let tableFrame = Rect(CGRect(x: -1170, y: 350, width: 740, height: 500))
    let rowFrame = Rect(CGRect(x: -1160, y: 355, width: 700, height: 50))
    let cellFrame = Rect(CGRect(x: -1150, y: 360, width: 300, height: 40))
    let scope = TableScopeSearchRequest(
        scroll_path: [TableScopeSearchStep(role: "ax_scroll_area", label: "Archive")],
        column_headers: ["Name"])
    let rootID = "root-\(identitySuffix)"
    let scrollID = "scroll-\(identitySuffix)"
    let tableID = "table-\(identitySuffix)"
    let rowID = "row-\(identitySuffix)"
    let cellID = "cell-\(identitySuffix)"
    let columnID = "column-\(identitySuffix)"
    let headerID = "header-\(identitySuffix)"
    let path = [TableScopeSearchPathItem(instance_ref: scrollID, label: "Archive", frame: scrollFrame)]
    let candidate = TableScopeSearchCandidate(table_instance_ref: tableID,
        scroll_container_instance_ref: scrollID, scroll_path: path, column_headers: ["Name"])
    let viewport = TableViewport(
        table_instance_ref: tableID, scroll_container_instance_ref: scrollID, process_id: 1,
        table_frame: tableFrame, viewport_frame: scrollFrame, ax_enabled: .enabled,
        visible_rows_complete: true,
        visible_rows: [TableViewportRow(instance_ref: rowID, parent_instance_ref: tableID,
            frame: rowFrame, ax_index: 5, accessibility_identifier: "archive-row", ax_enabled: .enabled,
            selected: selected, ax_selected_settable: true, cells_complete: true,
            cells: [TableViewportCell(instance_ref: cellID, parent_instance_ref: rowID, frame: cellFrame,
                row_ax_index: 5, column_ax_index: 0, column_instance_ref: columnID,
                header_instance_ref: headerID, column_header: "Name", value: cellValue)])],
        columns_complete: true,
        columns: [TableViewportColumn(instance_ref: columnID, parent_instance_ref: tableID,
            frame: tableFrame, ax_index: 0, header_instance_ref: headerID, header: "Name")])

    var structure = Structure(app: app)
    structure.complete = complete
    structure.incomplete_reason = complete ? nil : scopedTableRowsOmittedReason
    structure.active_layer = windowID
    structure.windows = [WindowInfo(window_id: windowID, title: "Offline", frame: windowFrame,
        z_index: 0, kind: "window", modal: false, owner_bundle_id: app.bundle_id)]
    structure.observer_coverage = .unavailable
    structure.table_scope_search = TableScopeSearchResult(requested_scope: scope,
        process_id: app.pid, window_id: windowID, state: .complete, candidates: [candidate])
    structure.table_viewports = [viewport]
    structure.objects = [
        UiObject(instance_ref: rootID, parent: nil, window_id: windowID, role: "AXWindow",
            subrole: "AXStandardWindow", title: "Offline", description: nil, value: nil,
            enabled: true, focused: true, frame: windowFrame, actions: [], in_view: true,
            group: nil, secure: false),
        UiObject(instance_ref: scrollID, parent: rootID, window_id: windowID, role: "AXScrollArea",
            subrole: nil, title: nil, description: "Archive", value: nil, enabled: true, focused: false,
            frame: scrollFrame, actions: [], in_view: true, group: nil, secure: false),
        UiObject(instance_ref: tableID, parent: scrollID, window_id: windowID, role: "AXTable",
            subrole: nil, title: nil, description: nil, value: nil, enabled: true, focused: false,
            frame: tableFrame, actions: [], in_view: true, group: nil, secure: false),
        UiObject(instance_ref: rowID, parent: tableID, window_id: windowID, role: "AXRow",
            subrole: nil, title: "Archive", description: nil, value: nil, enabled: true, focused: false,
            frame: rowFrame, actions: [], in_view: true, group: nil, secure: false),
        UiObject(instance_ref: cellID, parent: rowID, window_id: windowID, role: "AXCell",
            subrole: nil, title: nil, description: nil, value: cellValue, enabled: true, focused: false,
            frame: childFrame, actions: [], in_view: true, group: nil, secure: false),
    ]
    let rootFact = VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil, role: "AXWindow",
        subrole: "AXStandardWindow", frame: windowFrame, childrenSource: rootChildrenSource, childCount: 1)
    let childFact = VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0, role: "AXGroup",
        subrole: nil, frame: childFrame, childrenSource: .visibleChildren, childCount: 0)
    let fingerprint = [rootFact, childFact]
    let coverage = VisiblePrivacyCoverage(bundleID: app.bundle_id, processID: app.pid,
        processStamp: ProcessStamp(seconds: 1, micros: 0), windowID: windowID, windowFrame: windowFrame,
        tableScope: scope, readStartedNS: proofStarted, readFinishedNS: proofFinished,
        visibleNodeCount: UInt32(fingerprint.count), visibleTreeFingerprint: fingerprint,
        tableViewport: VisiblePrivacyTableViewport(viewport: viewport))
    return StructureRead(structure: structure, started: started, finished: finished,
        secureFrames: [], visiblePrivacy: coverage)
}

/// Two-root fixture for the registered AXDialog exception. It is entirely
/// synthetic and is used only by offline continuity-gate tests.
func visiblePrivacyAuxiliarySelfTestRead(
    auxiliaryTitle: String = "Auxiliary",
    auxiliaryValue: UiValue? = nil,
    auxiliaryFrame: Rect? = nil
) -> StructureRead {
    let base = visiblePrivacySelfTestRead()
    guard var coverage = base.visiblePrivacy else { return base }
    var structure = base.structure
    let dialogID = "window-2"
    let dialogFrame = auxiliaryFrame ?? Rect(CGRect(x: -520, y: 500, width: 180, height: 140))
    let childFrame = Rect(CGRect(x: dialogFrame.x + 12, y: dialogFrame.y + 28,
                                 width: max(dialogFrame.width - 24, 1), height: 32))
    let dialogRootID = "dialog-root"
    let dialogChildID = "dialog-child"
    structure.windows.append(WindowInfo(window_id: dialogID, title: auxiliaryTitle,
        frame: dialogFrame, z_index: 1, kind: "window", modal: false,
        owner_bundle_id: structure.app.bundle_id))
    structure.objects.append(contentsOf: [
        UiObject(instance_ref: dialogRootID, parent: nil, window_id: dialogID, role: "AXWindow",
            subrole: "AXDialog", title: auxiliaryTitle, description: nil, value: nil,
            enabled: true, focused: false, frame: dialogFrame, actions: [], in_view: true,
            group: nil, secure: false),
        UiObject(instance_ref: dialogChildID, parent: dialogRootID, window_id: dialogID,
            role: "AXButton", subrole: nil, title: nil, description: nil, value: auxiliaryValue,
            enabled: true, focused: false, frame: childFrame, actions: ["press"], in_view: true,
            group: nil, secure: false),
    ])
    let tree = [
        VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil, role: "AXWindow",
            subrole: "AXDialog", frame: dialogFrame, childrenSource: .visibleChildren, childCount: 1),
        VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0, role: "AXButton",
            subrole: nil, frame: childFrame, childrenSource: .visibleChildren, childCount: 0),
    ]
    let auxiliary = VisiblePrivacyAuxiliaryFingerprint(windowID: dialogID, processID: coverage.processID,
        cgWindowID: 2, frame: dialogFrame, role: "AXWindow", subrole: "AXDialog",
        modal: false, focused: false, treeFingerprint: tree,
        contentSignatures: structure.objects.filter({ $0.window_id == dialogID })
            .map(VisiblePrivacyAuxiliaryContentSignature.init))
    coverage = VisiblePrivacyCoverage(bundleID: coverage.bundleID, processID: coverage.processID,
        processStamp: coverage.processStamp, windowID: coverage.windowID,
        windowFrame: coverage.windowFrame, tableScope: coverage.tableScope,
        readStartedNS: coverage.readStartedNS, readFinishedNS: coverage.readFinishedNS,
        visibleNodeCount: coverage.visibleNodeCount + UInt32(tree.count),
        visibleTreeFingerprint: coverage.visibleTreeFingerprint,
        tableViewport: coverage.tableViewport, auxiliaryWindows: [auxiliary])
    return StructureRead(structure: structure, started: base.started, finished: base.finished,
        secureFrames: base.secureFrames, visiblePrivacy: coverage,
        privacyRejection: base.privacyRejection)
}

/// Offline regression checks. The sole image is generated in memory; this does
/// not read AX, capture a display, grant permissions, or write an asset.
func runSelfTests() throws -> SelfTestResponse {
    var tests: [String] = []
    tests.append(contentsOf: try runBackgroundAXWireTests())
    tests.append(contentsOf: try runBackgroundAXActionNameTests())
    tests.append(contentsOf: try runGlobalFocusReconciliationTests())
    tests.append(contentsOf: try runBackgroundWindowContextTests())
    let frame = Rect(CGRect(x: -1200, y: 320, width: 800, height: 600))
    let normalized = CGRect(x: 0.25, y: 0.5, width: 0.5, height: 0.25)
    let expected = Rect(CGRect(x: -1000, y: 470, width: 400, height: 150))
    try requireTest(globalOcrRect(normalized, window: frame) == expected, "OCR global origin and vertical axis")
    tests.append("ocr_global_origin_and_vertical_axis")
    try requireTest(globalOcrRect(CGRect(x: -0.1, y: 0, width: 1, height: 1), window: frame) == nil,
                    "OCR rejects invalid normalized boxes")
    tests.append("ocr_rejects_invalid_normalized_boxes")
    try requireTest(!frame.intersects(Rect(CGRect(x: -400, y: 320, width: 10, height: 10))), "half-open viewport geometry")
    tests.append("half_open_viewport_geometry")
    try requireTest(isSensitiveField(role: kAXTextFieldRole, subrole: kAXSecureTextFieldSubrole), "secure subrole")
    try requireTest(isSensitiveField(role: kAXTextAreaRole, subrole: nil), "unknown text field treated sensitive")
    try requireTest(isSensitiveField(role: kAXTextFieldRole, subrole: ""), "empty text field subrole treated sensitive")
    try requireTest(isSensitiveField(role: kAXTextFieldRole, subrole: "AXUnknown"), "unknown text field subrole treated sensitive")
    try requireTest(!isSensitiveField(role: kAXButtonRole, subrole: nil), "button classification")
    tests.append("sensitive_fields_fail_closed")
    try requireTest(!isSensitiveField(role: kAXTextFieldRole, subrole: nil, registeredEditor: true), "registered text field")
    try requireTest(!isSensitiveField(role: kAXTextAreaRole, subrole: "", registeredEditor: true), "registered text area")
    try requireTest(isSensitiveField(role: kAXTextFieldRole, subrole: kAXSecureTextFieldSubrole, registeredEditor: true), "registered secure field stays private")
    try requireTest(isSensitiveField(role: kAXTextFieldRole, subrole: "AXUnknown", registeredEditor: true), "registered unknown subrole stays private")
    tests.append("registered_editor_privacy_still_rejects_secure_and_unknown_fields")
    let policy = TextReadPolicy(kind: "registered_editor", window_id: "window-1")
    try requireTest(policy.valid(bundle: "com.apple.TextEdit", pid: 1), "registered policy identity")
    try requireTest(policy.valid(bundle: "com.lingxi.computer-user.mvp-fixture", pid: 1), "registered fixture text policy identity")
    try requireTest(!policy.valid(bundle: "com.lingxi.computer-user.mvp-fixture", pid: 0), "registered fixture missing process stays private")
    try requireTest(!policy.valid(bundle: "unregistered.app", pid: 1), "unregistered app privacy")
    try requireTest(!policy.valid(bundle: "com.apple.TextEdit", pid: nil), "missing process privacy")
    try requireTest(!TextReadPolicy(kind: "registered_editor", window_id: "window-0").valid(bundle: "com.apple.TextEdit", pid: 1), "invalid window privacy")
    tests.append("registered_editor_policy_requires_app_process_and_window")
    let valueRequest = try JSONDecoder().decode(NativeRequest.self, from: Data("{\"protocol_version\":1,\"command\":\"self_test\"}".utf8))
    let valueReader = AXReader(app: AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 1), request: valueRequest)
    let exactValue = "中文\r\n👩‍👩‍👧‍👦\u{200E}\u{1}"
    try requireTest(valueReader.semanticValue(exactValue, registeredEditor: true) == exactValue && valueReader.reasons.isEmpty, "value scalars preserved")
    try requireTest(valueReader.semanticValue(String(repeating: "a", count: 2048), registeredEditor: true).utf8.count == 2048 && valueReader.reasons.isEmpty, "registered value exact byte limit")
    let truncatedValue = valueReader.semanticValue(String(repeating: "中", count: 683), registeredEditor: true)
    try requireTest(truncatedValue.utf8.count <= 2048 && !valueReader.reasons.isEmpty, "value overflow remains incomplete")
    tests.append("registered_values_preserve_exact_scalars_and_bound_utf8")
    let app = AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 1)
    func privacyRead(id: String, rect: Rect = frame, secure: Bool = false, complete: Bool = true) -> StructureRead {
        var structure = Structure(app: app)
        structure.complete = complete
        structure.incomplete_reason = complete ? nil : "test truncated"
        structure.active_layer = "window-1"
        structure.objects = [UiObject(instance_ref: id, parent: nil, window_id: "window-1", role: kAXButtonRole,
            subrole: nil, title: "Test", description: nil, value: nil, enabled: true, focused: false,
            frame: rect, actions: ["press"], in_view: true, group: nil, secure: secure)]
        return StructureRead(structure: structure, started: 1, finished: 2, secureFrames: secure ? [rect] : [])
    }
    let original = privacyRead(id: "first-read")
    try requireTest(unchangedPrivacyContext(before: original, after: privacyRead(id: "second-read")),
                    "privacy comparison excludes per-read node IDs")
    tests.append("privacy_context_ignores_per_read_node_ids")
    try requireTest(!unchangedPrivacyContext(before: original, after: privacyRead(id: "sensitive", secure: true)),
                    "sensitive field appearing rejects capture")
    tests.append("new_sensitive_field_rejects_capture")
    try requireTest(!unchangedPrivacyContext(before: original, after: privacyRead(id: "moved", rect: expected)),
                    "geometry changed rejects capture")
    tests.append("changed_geometry_rejects_capture")
    try requireTest(!unchangedPrivacyContext(before: original, after: privacyRead(id: "partial", complete: false)),
                    "incomplete privacy reread rejects capture")
    tests.append("incomplete_privacy_reread_rejects_capture")

    let scopedPrivacyRead = visiblePrivacySelfTestRead()
    guard let scopedPrivacyWindow = scopedPrivacyRead.structure.windows.first else {
        throw NativeFailure("visible privacy test window missing")
    }
    try requireTest(captureReadPrivacyStatus(scopedPrivacyRead, window: scopedPrivacyWindow) == .eligible,
                    "valid scoped read retains the existing privacy eligibility result")
    try requireTest(captureReadPrivacyEligible(scopedPrivacyRead, window: scopedPrivacyWindow),
                    "exact scoped partial read can prove visible privacy")
    let reidentifiedPrivacyRead = visiblePrivacySelfTestRead(identitySuffix: "second")
    try requireTest(unchangedPrivacyContext(before: scopedPrivacyRead, after: reidentifiedPrivacyRead),
                    "safe visible and viewport facts compare across read-local IDs")
    try requireTest(!unchangedPrivacyContext(before: scopedPrivacyRead,
                    after: visiblePrivacySelfTestRead(identitySuffix: "selection", selected: true)),
                    "visible row selection changes reject capture pair")
    try requireTest(!unchangedPrivacyContext(before: scopedPrivacyRead,
                    after: visiblePrivacySelfTestRead(identitySuffix: "cell-value", cellValue: .text("Changed"))),
                    "visible cell value changes reject capture pair")
    try requireTest(!unchangedPrivacyContext(before: scopedPrivacyRead,
                    after: visiblePrivacySelfTestRead(identitySuffix: "child-frame",
                        childFrame: Rect(CGRect(x: -1100, y: 420, width: 280, height: 40)))),
                    "visible descendant geometry changes reject capture pair")
    try requireTest(!captureReadPrivacyEligible(
                    visiblePrivacySelfTestRead(identitySuffix: "unknown-visible-list",
                        rootChildrenSource: .unknown), window: scopedPrivacyWindow),
                    "unknown visible-children read cannot create coverage")
    var missingPrivacyProof = scopedPrivacyRead
    missingPrivacyProof.visiblePrivacy = nil
    try requireTest(captureReadPrivacyStatus(missingPrivacyProof, window: scopedPrivacyWindow) == .missingCoverage
                    && !captureReadPrivacyEligible(missingPrivacyProof, window: scopedPrivacyWindow),
                    "partial read without reader proof stays closed")
    var wrongActiveLayer = scopedPrivacyRead
    wrongActiveLayer.structure.active_layer = "another-window"
    try requireTest(captureReadPrivacyStatus(wrongActiveLayer, window: scopedPrivacyWindow) == .activeLayerMismatch,
                    "partial proof cannot cross a changed active layer")

    let extendedPrivacyStart = StructureRead(structure: scopedPrivacyRead.structure,
        started: 0, finished: scopedPrivacyRead.finished, secureFrames: [],
        visiblePrivacy: scopedPrivacyRead.visiblePrivacy)
    try requireTest(captureReadPrivacyEligible(extendedPrivacyStart, window: scopedPrivacyWindow),
                    "earlier WindowContext header remains outside proof freshness interval")
    let extendedPrivacyFinish = finishAXSessionStructureRead(scopedPrivacyRead, finishedNs: 8)
    try requireTest(extendedPrivacyFinish.visiblePrivacy == scopedPrivacyRead.visiblePrivacy
                    && captureReadPrivacyEligible(extendedPrivacyFinish, window: scopedPrivacyWindow),
                    "AXSession read wrapping preserves reader-owned coverage")
    let stalePrivacyInterval = StructureRead(structure: scopedPrivacyRead.structure,
        started: scopedPrivacyRead.started, finished: 2, secureFrames: [],
        visiblePrivacy: scopedPrivacyRead.visiblePrivacy)
    try requireTest(!captureReadPrivacyEligible(stalePrivacyInterval, window: scopedPrivacyWindow),
                    "proof interval outside the enclosing read is rejected")
    let scopedSearch = scopedPrivacyRead.structure.table_scope_search!
    func withScopedSearch(_ search: TableScopeSearchResult) -> StructureRead {
        var altered = scopedPrivacyRead
        altered.structure.table_scope_search = search
        return altered
    }
    try requireTest(!captureReadPrivacyEligible(withScopedSearch(TableScopeSearchResult(
                    requested_scope: scopedSearch.requested_scope, process_id: scopedSearch.process_id,
                    window_id: scopedSearch.window_id, state: .incomplete(reason: "child enumeration unknown"),
                    candidates: scopedSearch.candidates)), window: scopedPrivacyWindow),
                    "incomplete table census cannot authorize privacy coverage")
    try requireTest(captureReadPrivacyStatus(withScopedSearch(TableScopeSearchResult(
                    requested_scope: scopedSearch.requested_scope, process_id: scopedSearch.process_id,
                    window_id: scopedSearch.window_id, state: .incomplete(reason: "child enumeration unknown"),
                    candidates: scopedSearch.candidates)), window: scopedPrivacyWindow) == .censusInvalid,
                    "incomplete census receives its fixed diagnostic tag")
    try requireTest(!captureReadPrivacyEligible(withScopedSearch(TableScopeSearchResult(
                    requested_scope: scopedSearch.requested_scope, process_id: scopedSearch.process_id,
                    window_id: scopedSearch.window_id, state: .complete, candidates: [])),
                    window: scopedPrivacyWindow),
                    "complete zero-candidate census cannot authorize a captured table")
    try requireTest(!captureReadPrivacyEligible(withScopedSearch(TableScopeSearchResult(
                    requested_scope: scopedSearch.requested_scope, process_id: scopedSearch.process_id,
                    window_id: scopedSearch.window_id, state: .complete,
                    candidates: scopedSearch.candidates + scopedSearch.candidates)),
                    window: scopedPrivacyWindow),
                    "ambiguous table census cannot authorize privacy coverage")
    let wrongScope = TableScopeSearchRequest(
        scroll_path: [TableScopeSearchStep(role: "ax_scroll_area", label: "Other")],
        column_headers: ["Name"])
    try requireTest(!captureReadPrivacyEligible(withScopedSearch(TableScopeSearchResult(
                    requested_scope: wrongScope, process_id: scopedSearch.process_id,
                    window_id: scopedSearch.window_id, state: .complete,
                    candidates: scopedSearch.candidates)), window: scopedPrivacyWindow),
                    "altered frozen table scope is rejected")
    let sourceViewport = scopedPrivacyRead.structure.table_viewports[0]
    let incompleteViewport = TableViewport(
        table_instance_ref: sourceViewport.table_instance_ref,
        scroll_container_instance_ref: sourceViewport.scroll_container_instance_ref,
        process_id: sourceViewport.process_id, table_frame: sourceViewport.table_frame,
        viewport_frame: sourceViewport.viewport_frame, ax_enabled: sourceViewport.ax_enabled,
        visible_rows_complete: false, visible_rows: sourceViewport.visible_rows,
        columns_complete: sourceViewport.columns_complete, columns: sourceViewport.columns)
    var invalidViewportRead = scopedPrivacyRead
    invalidViewportRead.structure.table_viewports = [incompleteViewport]
    try requireTest(captureReadPrivacyStatus(invalidViewportRead, window: scopedPrivacyWindow) == .viewportInvalid
                    && !captureReadPrivacyEligible(invalidViewportRead, window: scopedPrivacyWindow),
                    "incomplete visible rows receive a fixed viewport diagnostic tag")
    try requireTest(!captureReadPrivacyEligible(scopedPrivacyRead,
                    window: WindowInfo(window_id: scopedPrivacyWindow.window_id,
                        title: scopedPrivacyWindow.title, frame: Rect(CGRect(x: 0, y: 0, width: 10, height: 10)),
                        z_index: scopedPrivacyWindow.z_index, kind: scopedPrivacyWindow.kind,
                        modal: false, owner_bundle_id: scopedPrivacyWindow.owner_bundle_id)),
                    "changed captured window geometry is rejected")
    if let coverage = scopedPrivacyRead.visiblePrivacy {
        let tamperedCoverage = VisiblePrivacyCoverage(bundleID: coverage.bundleID,
            processID: coverage.processID, processStamp: coverage.processStamp,
            windowID: coverage.windowID, windowFrame: coverage.windowFrame,
            tableScope: coverage.tableScope, readStartedNS: coverage.readStartedNS,
            readFinishedNS: coverage.readFinishedNS, visibleNodeCount: coverage.visibleNodeCount + 1,
            visibleTreeFingerprint: coverage.visibleTreeFingerprint, tableViewport: coverage.tableViewport)
        var tamperedRead = scopedPrivacyRead
        tamperedRead.visiblePrivacy = tamperedCoverage
        try requireTest(!captureReadPrivacyEligible(tamperedRead, window: scopedPrivacyWindow),
                        "tampered visible node count cannot authorize capture")
    } else {
        throw NativeFailure("visible privacy fixture proof missing")
    }
    let completePrivacyRead = visiblePrivacySelfTestRead(complete: true)
    try requireTest(captureReadPrivacyStatus(completePrivacyRead, window: scopedPrivacyWindow) == .eligible
                    && captureReadPrivacyEligible(completePrivacyRead, window: scopedPrivacyWindow),
                    "complete-tree privacy keeps the preexisting no-secure-frame rule")
    try requireTest(!unchangedPrivacyContext(before: scopedPrivacyRead,
                    after: visiblePrivacySelfTestRead(identitySuffix: "full", complete: true)),
                    "partial and complete reads cannot form one capture bracket")
    let auxiliaryPrivacyRead = visiblePrivacyAuxiliarySelfTestRead(auxiliaryValue: .text("dialog value"))
    guard let auxiliaryStandardWindow = auxiliaryPrivacyRead.structure.windows.first(where: {
        $0.window_id == "window-1"
    }), let auxiliaryCoverage = auxiliaryPrivacyRead.visiblePrivacy,
       let auxiliaryFingerprint = auxiliaryCoverage.auxiliaryWindows.first else {
        throw NativeFailure("auxiliary visible privacy fixture missing")
    }
    try requireTest(captureReadPrivacyStatus(auxiliaryPrivacyRead, window: auxiliaryStandardWindow) == .eligible
                    && VisiblePrivacyGraph.validAuxiliaryFingerprint(auxiliaryFingerprint,
                        expectedProcessID: auxiliaryCoverage.processID),
                    "exact registered AXDialog tree, source objects, and two-root metadata align")
    try requireTest(!unchangedPrivacyContext(before: auxiliaryPrivacyRead,
                    after: visiblePrivacyAuxiliarySelfTestRead(
                        auxiliaryTitle: "Changed title", auxiliaryValue: .text("dialog value"))),
                    "auxiliary root content changes reject the privacy bracket")
    try requireTest(!unchangedPrivacyContext(before: auxiliaryPrivacyRead,
                    after: visiblePrivacyAuxiliarySelfTestRead(
                        auxiliaryTitle: "Auxiliary", auxiliaryValue: .text("changed value"))),
                    "auxiliary descendant value changes reject the privacy bracket")
    try requireTest(!unchangedPrivacyContext(before: auxiliaryPrivacyRead,
                    after: visiblePrivacyAuxiliarySelfTestRead(
                        auxiliaryValue: .text("dialog value"),
                        auxiliaryFrame: Rect(CGRect(x: -500, y: 500, width: 180, height: 140)))),
                    "auxiliary geometry changes reject the privacy bracket")
    var alteredAuxiliaryRead = auxiliaryPrivacyRead
    if var alteredAuxiliaryCoverage = alteredAuxiliaryRead.visiblePrivacy,
       let originalAuxiliary = alteredAuxiliaryCoverage.auxiliaryWindows.first {
        alteredAuxiliaryCoverage.auxiliaryWindows[0] = VisiblePrivacyAuxiliaryFingerprint(
            windowID: "window-9", processID: originalAuxiliary.processID, cgWindowID: 2,
            frame: originalAuxiliary.frame, role: originalAuxiliary.role,
            subrole: originalAuxiliary.subrole, modal: originalAuxiliary.modal,
            focused: originalAuxiliary.focused, treeFingerprint: originalAuxiliary.treeFingerprint,
            contentSignatures: originalAuxiliary.contentSignatures)
        alteredAuxiliaryRead.visiblePrivacy = alteredAuxiliaryCoverage
    }
    try requireTest(captureReadPrivacyStatus(alteredAuxiliaryRead,
                    window: auxiliaryStandardWindow) == .fingerprintInvalid,
                    "auxiliary coverage cannot substitute an unrelated CG window identity")
    var missingAuxiliaryVisibleNode = auxiliaryPrivacyRead
    if var coverage = missingAuxiliaryVisibleNode.visiblePrivacy,
       let original = coverage.auxiliaryWindows.first,
       original.treeFingerprint.count == 2 {
        let oldChild = original.treeFingerprint[1]
        let changedChild = VisiblePrivacyNodeFact(parentIndex: oldChild.parentIndex,
            childIndex: oldChild.childIndex, role: oldChild.role, subrole: oldChild.subrole,
            frame: Rect(CGRect(x: oldChild.frame.x + 1, y: oldChild.frame.y,
                               width: oldChild.frame.width, height: oldChild.frame.height)),
            childrenSource: oldChild.childrenSource, childCount: oldChild.childCount,
            actualParentIndex: oldChild.actualParentIndex)
        coverage.auxiliaryWindows[0] = VisiblePrivacyAuxiliaryFingerprint(
            windowID: original.windowID, processID: original.processID,
            cgWindowID: original.cgWindowID, frame: original.frame, role: original.role,
            subrole: original.subrole, modal: original.modal, focused: original.focused,
            treeFingerprint: [original.treeFingerprint[0], changedChild],
            contentSignatures: original.contentSignatures)
        missingAuxiliaryVisibleNode.visiblePrivacy = coverage
    }
    try requireTest(captureReadPrivacyStatus(missingAuxiliaryVisibleNode,
                    window: auxiliaryStandardWindow) == .fingerprintInvalid,
                    "every audited auxiliary descendant must match source content facts")
    var unknownRootRead = auxiliaryPrivacyRead
    if var coverage = unknownRootRead.visiblePrivacy,
       let original = coverage.auxiliaryWindows.first {
        coverage.auxiliaryWindows[0] = VisiblePrivacyAuxiliaryFingerprint(
            windowID: original.windowID, processID: original.processID,
            cgWindowID: original.cgWindowID, frame: original.frame, role: original.role,
            subrole: "AXStandardWindow", modal: original.modal, focused: original.focused,
            treeFingerprint: original.treeFingerprint, contentSignatures: original.contentSignatures)
        unknownRootRead.visiblePrivacy = coverage
    }
    try requireTest(captureReadPrivacyStatus(unknownRootRead,
                    window: auxiliaryStandardWindow) == .fingerprintInvalid,
                    "the auxiliary exception is restricted to the exact AXDialog root")
    var extraWindowRead = auxiliaryPrivacyRead
    extraWindowRead.structure.windows.append(WindowInfo(window_id: "window-3", title: "Unknown",
        frame: Rect(CGRect(x: 10, y: 10, width: 20, height: 20)), z_index: 2,
        kind: "window", modal: false, owner_bundle_id: auxiliaryPrivacyRead.structure.app.bundle_id))
    try requireTest(captureReadPrivacyStatus(extraWindowRead,
                    window: auxiliaryStandardWindow) == .windowMetadataMismatch,
                    "an unregistered third root/window cannot share the pixel exception")
    tests.append("scoped_visible_privacy_gate_and_cross_read_fingerprint")
    tests.append("registered_auxiliary_dialog_visible_privacy_fingerprint")
    tests.append("capture_read_privacy_status_matches_existing_eligibility_gate")

    let graphWindow = Rect(CGRect(x: -1200, y: 320, width: 800, height: 600))
    let graphChildFrame = Rect(CGRect(x: -1150, y: 360, width: 300, height: 40))
    func graphObservations(rootSource: VisiblePrivacyChildrenSource = .visibleChildren,
                           childRole: String = "AXGroup",
                           childSubrole: String? = nil,
                           childSource: VisiblePrivacyChildrenSource = .visibleChildren,
                           childFrame: Rect = graphChildFrame,
                           childIdentity: Int = 1,
                           childParentIndex: Int? = 0,
                           childParentIdentity: Int? = 0,
                           childProcessID: UInt32 = 1,
                           childParentMatches: Bool = true,
                           childFrameTypeValid: Bool = true,
                           childSensitive: Bool = false,
                           childForbiddenLayer: Bool = false) -> [VisiblePrivacyAuditObservation] {
        let root = VisiblePrivacyAuditObservation(identityOrdinal: 0, parentIdentityOrdinal: nil,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: false, fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: "AXWindow", subrole: "AXStandardWindow", frame: graphWindow,
                childrenSource: rootSource, childCount: 1))
        let child = VisiblePrivacyAuditObservation(identityOrdinal: childIdentity,
            parentIdentityOrdinal: childParentIdentity, processID: childProcessID,
            actualParentMatches: childParentMatches, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: childFrameTypeValid,
            modalTypeValid: true, sensitive: childSensitive,
            forbiddenLayer: childForbiddenLayer, modal: nil,
            fact: VisiblePrivacyNodeFact(parentIndex: childParentIndex, childIndex: 0,
                role: childRole, subrole: childSubrole, frame: childFrame,
                childrenSource: childSource, childCount: 0))
        return [root, child]
    }
    func graphIsComplete(_ observations: [VisiblePrivacyAuditObservation]) -> Bool {
        VisiblePrivacyGraph.fingerprint(observations: observations, expectedProcessID: 1,
            windowFrame: graphWindow, nodeLimit: 512) != nil
    }
    let auxiliaryGraphWindow = Rect(CGRect(x: -520, y: 500, width: 180, height: 140))
    func auxiliaryGraphObservations(
        rootRole: String = "AXWindow",
        rootSubrole: String? = "AXDialog",
        rootSource: VisiblePrivacyChildrenSource = .visibleChildren,
        rootParentMatches: Bool = true,
        rootModal: Bool? = false,
        rootIdentity: Int = 10,
        childRole: String = "AXButton",
        childSubrole: String? = nil,
        childSource: VisiblePrivacyChildrenSource = .visibleChildren,
        childFrame: Rect = Rect(CGRect(x: -508, y: 528, width: 156, height: 32)),
        childIdentity: Int = 11,
        childParentIndex: Int? = 0,
        childParentIdentity: Int? = 10,
        childIndex: Int = 0,
        childProcessID: UInt32 = 1,
        childParentMatches: Bool = true,
        childSensitive: Bool = false,
        childForbiddenLayer: Bool = false
    ) -> [VisiblePrivacyAuditObservation] {
        let root = VisiblePrivacyAuditObservation(identityOrdinal: rootIdentity, parentIdentityOrdinal: nil,
            processID: 1, actualParentMatches: rootParentMatches, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: rootModal, fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: rootRole, subrole: rootSubrole, frame: auxiliaryGraphWindow,
                childrenSource: rootSource, childCount: 1))
        let child = VisiblePrivacyAuditObservation(identityOrdinal: childIdentity,
            parentIdentityOrdinal: childParentIdentity, processID: childProcessID,
            actualParentMatches: childParentMatches, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: childSensitive,
            forbiddenLayer: childForbiddenLayer, modal: nil,
            fact: VisiblePrivacyNodeFact(parentIndex: childParentIndex, childIndex: childIndex,
                role: childRole, subrole: childSubrole, frame: childFrame,
                childrenSource: childSource, childCount: 0))
        return [root, child]
    }
    func auxiliaryGraphIsComplete(_ observations: [VisiblePrivacyAuditObservation]) -> Bool {
        VisiblePrivacyGraph.fingerprint(observations: observations, expectedProcessID: 1,
            windowFrame: auxiliaryGraphWindow, nodeLimit: 512,
            rootKind: .registeredAuxiliaryDialog) != nil
    }
    try requireTest(auxiliaryGraphIsComplete(auxiliaryGraphObservations()),
                    "the exact registered AXWindow/AXDialog root and recursively audited child are valid")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(rootSource: .unknown)),
                    "unknown auxiliary root children are not an empty list")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(rootSubrole: "AXStandardWindow")),
                    "the auxiliary walker cannot widen to an ordinary second window")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(rootParentMatches: false)),
                    "the auxiliary AXWindow must retain its actual application parent")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(rootModal: true)),
                    "a modal auxiliary root is outside the admitted inactive-dialog case")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(childRole: "AXTextField",
                    childSensitive: true)),
                    "sensitive auxiliary descendant rejects the whole-tree pixel exception")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(childIdentity: 10)),
                    "a repeated identity ordinal is rejected by the shared audit graph")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(childRole: "AXWindow")),
                    "an extra auxiliary AXWindow root cannot nest inside the audited dialog")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(childRole: "AXDialog")),
                    "nested dialog roles remain forbidden under the auxiliary root")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(
                    childParentIdentity: 99, childParentMatches: false)),
                    "auxiliary descendant actual-parent mismatch remains closed")
    try requireTest(!auxiliaryGraphIsComplete(auxiliaryGraphObservations(childIndex: 1)),
                    "auxiliary ordered child slots must match the complete parent list")
    let crossTreeIdentityAlias = auxiliaryGraphObservations(rootIdentity: 1,
        childIdentity: 2, childParentIdentity: 1)
    try requireTest(VisiblePrivacyGraph.identityOrdinalsAreDisjoint(
                    [graphObservations(), auxiliaryGraphObservations()])
                    && auxiliaryGraphIsComplete(crossTreeIdentityAlias)
                    && !VisiblePrivacyGraph.identityOrdinalsAreDisjoint(
                        [graphObservations(), crossTreeIdentityAlias]),
                    "the combined root ledger rejects aliased identities across trees")
    try requireTest(VisiblePrivacyGraph.fitsSharedNodeLimit([300, 212], nodeLimit: 512)
                    && !VisiblePrivacyGraph.fitsSharedNodeLimit([300, 213], nodeLimit: 512),
                    "the standard and auxiliary trees share the original 512-node budget")
    try requireTest(VisiblePrivacyGraph.childListPostcheckMatches(
                    expectedSource: .visibleChildren, currentSource: .visibleChildren,
                    expectedCount: 2, currentCount: 2, referencesMatchInOrder: [true, true]),
                    "unchanged raw visible-child refs preserve their exact order")
    try requireTest(!VisiblePrivacyGraph.childListPostcheckMatches(
                    expectedSource: .visibleChildren, currentSource: .visibleChildren,
                    expectedCount: 2, currentCount: 2, referencesMatchInOrder: [true, false])
                    && !VisiblePrivacyGraph.childListPostcheckMatches(
                        expectedSource: .visibleChildren, currentSource: .childrenFallback,
                        expectedCount: 2, currentCount: 2, referencesMatchInOrder: [true, true])
                    && !VisiblePrivacyGraph.childListPostcheckMatches(
                        expectedSource: .visibleChildren, currentSource: .visibleChildren,
                        expectedCount: 2, currentCount: 1, referencesMatchInOrder: [true])
                    && !VisiblePrivacyGraph.childListPostcheckMatches(
                        expectedSource: .visibleChildren, currentSource: .unknown,
                        expectedCount: 0, currentCount: 0, referencesMatchInOrder: [])
                    && !VisiblePrivacyGraph.childListPostcheckMatches(
                        expectedSource: .leafChildrenUnsupported,
                        currentSource: .leafChildrenUnsupported,
                        expectedCount: 1, currentCount: 1, referencesMatchInOrder: [true]),
                    "changed order, enumeration mode, count, or incomplete source closes the audit")
    tests.append("registered_auxiliary_tree_requires_complete_known_children_and_shared_budget")
    tests.append("visible_privacy_postchecks_every_raw_ordered_child_list")
    let scrollBarArrowWindow = Rect(CGRect(x: 900, y: 250, width: 500, height: 500))
    let scrollBarFrame = Rect(CGRect(x: 980, y: 280, width: 25, height: 140))
    let zeroArrowFrame = Rect(CGRect(x: 988, y: 305, width: 0, height: 0))
    func scrollBarArrowObservations(
        parentRole: String = "AXScrollBar",
        role: String = "AXButton",
        subrole: String? = "AXIncrementArrow",
        frame: Rect = Rect(CGRect(x: 988, y: 305, width: 0, height: 0)),
        childSource: VisiblePrivacyChildrenSource = .visibleChildren,
        childCount: Int = 0,
        childParentMatches: Bool = true,
        childProcessID: UInt32 = 1,
        childFrameTypeValid: Bool = true,
        childSensitive: Bool = false
    ) -> [VisiblePrivacyAuditObservation] {
        let root = VisiblePrivacyAuditObservation(identityOrdinal: 0, parentIdentityOrdinal: nil,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: false, fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: "AXWindow", subrole: "AXStandardWindow", frame: scrollBarArrowWindow,
                childrenSource: .visibleChildren, childCount: 1))
        let scrollBar = VisiblePrivacyAuditObservation(identityOrdinal: 1, parentIdentityOrdinal: 0,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: nil, fact: VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0,
                role: parentRole, subrole: nil, frame: scrollBarFrame,
                childrenSource: .visibleChildren, childCount: 1))
        var observations = [root, scrollBar,
            VisiblePrivacyAuditObservation(identityOrdinal: 2, parentIdentityOrdinal: 1,
                processID: childProcessID, actualParentMatches: childParentMatches,
                roleTypeValid: true, subroleTypeValid: true, frameTypeValid: childFrameTypeValid,
                modalTypeValid: true, sensitive: childSensitive, forbiddenLayer: false, modal: nil,
                fact: VisiblePrivacyNodeFact(parentIndex: 1, childIndex: 0, role: role,
                    subrole: subrole, frame: frame, childrenSource: childSource, childCount: childCount))]
        if childCount > 0 {
            observations.append(VisiblePrivacyAuditObservation(identityOrdinal: 3, parentIdentityOrdinal: 2,
                processID: childProcessID, actualParentMatches: true, roleTypeValid: true,
                subroleTypeValid: true, frameTypeValid: true, modalTypeValid: true,
                sensitive: false, forbiddenLayer: false, modal: nil,
                fact: VisiblePrivacyNodeFact(parentIndex: 2, childIndex: 0, role: "AXStaticText",
                    subrole: nil, frame: Rect(CGRect(x: 990, y: 306, width: 2, height: 2)),
                    childrenSource: .visibleChildren, childCount: 0)))
        }
        return observations
    }
    func scrollBarArrowGraphIsComplete(_ observations: [VisiblePrivacyAuditObservation]) -> Bool {
        VisiblePrivacyGraph.fingerprint(observations: observations, expectedProcessID: 1,
            windowFrame: scrollBarArrowWindow, nodeLimit: 512) != nil
    }
    let mismatchWindowFrame = Rect(CGRect(x: 900, y: 250, width: 500, height: 500))
    let mismatchButtonFrame = Rect(CGRect(x: 920, y: 270, width: 300, height: 100))
    func emptyGroupMismatchCandidate(
        expectedParentIndex: Int? = 1,
        expectedParentRole: String? = "AXButton",
        expectedParentWasAudited: Bool = true,
        expectedChildIndex: Int? = 0,
        expectedParentChildrenSource: VisiblePrivacyChildrenSource = .visibleChildren,
        actualParentIndex: Int? = 0,
        actualParentMatchesCapturedWindow: Bool = true,
        processID: UInt32 = 1,
        role: String? = "AXGroup",
        subrole: String? = nil,
        frame: Rect? = visiblePrivacyEmptyGroupTestFrame,
        roleTypeValid: Bool = true,
        subroleTypeValid: Bool = true,
        frameTypeValid: Bool = true,
        modal: Bool? = nil,
        sensitive: Bool = false,
        forbiddenLayer: Bool = false,
        axChildrenCountRead: VisiblePrivacyAXChildrenCountRead = .success(0),
        copiedAXChildrenCount: Int? = nil,
        axChildrenCopyComplete: Bool = false
    ) -> VisiblePrivacyGroupParentMismatchCandidate {
        VisiblePrivacyGroupParentMismatchCandidate(
            expectedParentIndex: expectedParentIndex, expectedParentRole: expectedParentRole,
            expectedParentWasAudited: expectedParentWasAudited, expectedChildIndex: expectedChildIndex,
            expectedParentChildrenSource: expectedParentChildrenSource,
            actualParentIndex: actualParentIndex,
            actualParentMatchesCapturedWindow: actualParentMatchesCapturedWindow,
            processID: processID, role: role, subrole: subrole, frame: frame,
            roleTypeValid: roleTypeValid, subroleTypeValid: subroleTypeValid,
            frameTypeValid: frameTypeValid, modal: modal, sensitive: sensitive,
            forbiddenLayer: forbiddenLayer, axChildrenCountRead: axChildrenCountRead,
            copiedAXChildrenCount: copiedAXChildrenCount,
            axChildrenCopyComplete: axChildrenCopyComplete)
    }
    func emptyGroupMismatchObservations(
        _ candidate: VisiblePrivacyGroupParentMismatchCandidate,
        buttonChildCount: Int = 1
    ) -> [VisiblePrivacyAuditObservation]? {
        guard let groupFact = VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
            candidate, expectedProcessID: 1) else { return nil }
        let root = VisiblePrivacyAuditObservation(identityOrdinal: 0, parentIdentityOrdinal: nil,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: false, fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: "AXWindow", subrole: "AXStandardWindow", frame: mismatchWindowFrame,
                childrenSource: .visibleChildren, childCount: 1))
        let button = VisiblePrivacyAuditObservation(identityOrdinal: 1, parentIdentityOrdinal: 0,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: nil, fact: VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0,
                role: "AXButton", subrole: nil, frame: mismatchButtonFrame,
                childrenSource: candidate.expectedParentChildrenSource, childCount: buttonChildCount))
        let group = VisiblePrivacyAuditObservation(identityOrdinal: 2, parentIdentityOrdinal: 0,
            processID: candidate.processID, actualParentMatches: false,
            roleTypeValid: candidate.roleTypeValid, subroleTypeValid: candidate.subroleTypeValid,
            frameTypeValid: candidate.frameTypeValid, modalTypeValid: true,
            sensitive: candidate.sensitive, forbiddenLayer: candidate.forbiddenLayer,
            modal: candidate.modal, fact: groupFact)
        return [root, button, group]
    }
    func emptyGroupMismatchGraphIsComplete(_ observations: [VisiblePrivacyAuditObservation]) -> Bool {
        VisiblePrivacyGraph.fingerprint(observations: observations, expectedProcessID: 1,
            windowFrame: mismatchWindowFrame, nodeLimit: 512) != nil
    }
    func windowOwnedGroupMismatchGraphIsComplete(_ observations: [VisiblePrivacyAuditObservation]) -> Bool {
        VisiblePrivacyGraph.fingerprint(observations: observations, expectedProcessID: 1,
            windowFrame: mismatchWindowFrame, nodeLimit: 512) != nil
    }
    let acceptedEmptyGroupCandidate = emptyGroupMismatchCandidate()
    let acceptedEmptyGroupFact = VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
        acceptedEmptyGroupCandidate, expectedProcessID: 1)
    try requireTest(acceptedEmptyGroupFact?.childrenSource == .emptyGroupAXChildrenSuccessCountZero
        && acceptedEmptyGroupFact?.parentIndex == 1 && acceptedEmptyGroupFact?.actualParentIndex == 0
        && acceptedEmptyGroupFact?.childIndex == 0 && acceptedEmptyGroupFact?.childCount == 0,
        "empty group mismatch preserves distinct expected and actual parents and child slot")
    if let acceptedGroupObservations = emptyGroupMismatchObservations(acceptedEmptyGroupCandidate) {
        try requireTest(emptyGroupMismatchGraphIsComplete(acceptedGroupObservations),
                        "audited empty AXGroup is reconciled as the returned button child")
        let acceptedGroupFacts = VisiblePrivacyGraph.fingerprint(
            observations: acceptedGroupObservations, expectedProcessID: 1,
            windowFrame: mismatchWindowFrame, nodeLimit: 512)
        try requireTest(acceptedGroupFacts.map {
            VisiblePrivacyGraph.validFingerprint($0, windowFrame: mismatchWindowFrame)
        } == true, "canonical privacy fingerprint preserves the empty-group audit")
        if let facts = acceptedGroupFacts, let special = facts.last {
            let wrongParent = VisiblePrivacyNodeFact(parentIndex: special.parentIndex,
                childIndex: special.childIndex, role: special.role, subrole: special.subrole,
                frame: special.frame, childrenSource: special.childrenSource,
                childCount: special.childCount, actualParentIndex: 1)
            var altered = facts
            altered[altered.count - 1] = wrongParent
            try requireTest(!VisiblePrivacyGraph.validFingerprint(altered, windowFrame: mismatchWindowFrame),
                            "canonical fingerprint rejects a changed actual-parent fact")
            let lostMarker = VisiblePrivacyNodeFact(parentIndex: special.parentIndex,
                childIndex: special.childIndex, role: special.role, subrole: special.subrole,
                frame: special.frame, childrenSource: .childrenFallback,
                childCount: special.childCount, actualParentIndex: 0)
            altered[altered.count - 1] = lostMarker
            try requireTest(!VisiblePrivacyGraph.validFingerprint(altered, windowFrame: mismatchWindowFrame),
                            "canonical fingerprint rejects loss of the successful empty-children marker")
        } else {
            try requireTest(false, "accepted empty-group audit must be fingerprinted")
        }
        if let mismatchedCountObservations = emptyGroupMismatchObservations(
            acceptedEmptyGroupCandidate, buttonChildCount: 0) {
            try requireTest(!emptyGroupMismatchGraphIsComplete(mismatchedCountObservations),
                            "empty-group exception cannot hide a returned child from parent counts")
        } else {
            try requireTest(false, "mismatched returned child count must remain represented")
        }
        tests.append("visible_privacy_empty_group_parent_mismatch_reconciles_child_count")
    } else {
        try requireTest(false, "valid empty group mismatch candidate must classify")
    }
    try requireTest(VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
                    emptyGroupMismatchCandidate(subrole: ""), expectedProcessID: 1) != nil,
                    "empty group accepts only nil or empty subrole")
    try requireTest(VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
                    emptyGroupMismatchCandidate(expectedParentChildrenSource: .childrenFallback),
                    expectedProcessID: 1) != nil,
                    "empty group can occupy a complete AXChildren fallback slot")
    let rejectedEmptyGroupCandidates: [(String, VisiblePrivacyGroupParentMismatchCandidate)] = [
        ("missing expected parent index", emptyGroupMismatchCandidate(expectedParentIndex: nil)),
        ("missing expected child index", emptyGroupMismatchCandidate(expectedChildIndex: nil)),
        ("negative expected child index", emptyGroupMismatchCandidate(expectedChildIndex: -1)),
        ("expected parent role", emptyGroupMismatchCandidate(expectedParentRole: "AXGroup")),
        ("expected parent audit", emptyGroupMismatchCandidate(expectedParentWasAudited: false)),
        ("actual parent index", emptyGroupMismatchCandidate(actualParentIndex: 1)),
        ("unknown actual parent index", emptyGroupMismatchCandidate(actualParentIndex: nil)),
        ("actual parent identity", emptyGroupMismatchCandidate(actualParentMatchesCapturedWindow: false)),
        ("process identity", emptyGroupMismatchCandidate(processID: 2)),
        ("role", emptyGroupMismatchCandidate(role: "AXButton")),
        ("custom subrole", emptyGroupMismatchCandidate(subrole: "AXCustomSubrole")),
        ("role type", emptyGroupMismatchCandidate(roleTypeValid: false)),
        ("subrole type", emptyGroupMismatchCandidate(subroleTypeValid: false)),
        ("frame type", emptyGroupMismatchCandidate(frameTypeValid: false)),
        ("missing frame", emptyGroupMismatchCandidate(frame: nil)),
        ("invalid frame", emptyGroupMismatchCandidate(frame: Rect(CGRect(x: CGFloat.nan, y: 285, width: 120, height: 40)))),
        ("sensitive child", emptyGroupMismatchCandidate(sensitive: true)),
        ("layer child", emptyGroupMismatchCandidate(forbiddenLayer: true)),
        ("modal child", emptyGroupMismatchCandidate(modal: true)),
        ("unknown parent children", emptyGroupMismatchCandidate(expectedParentChildrenSource: .unknown)),
        ("unsupported parent children", emptyGroupMismatchCandidate(expectedParentChildrenSource: .leafChildrenUnsupported)),
        ("unsupported AXChildren", emptyGroupMismatchCandidate(axChildrenCountRead: .unsupported)),
        ("AXChildren noValue", emptyGroupMismatchCandidate(axChildrenCountRead: .noValue)),
        ("failed AXChildren", emptyGroupMismatchCandidate(axChildrenCountRead: .failed)),
        ("late AXChildren", emptyGroupMismatchCandidate(axChildrenCountRead: .late)),
        ("nonempty AXChildren", emptyGroupMismatchCandidate(axChildrenCountRead: .success(1))),
        ("negative AXChildren count", emptyGroupMismatchCandidate(axChildrenCountRead: .success(-1))),
    ]
    for (reason, candidate) in rejectedEmptyGroupCandidates {
        try requireTest(VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
            candidate, expectedProcessID: 1) == nil,
            "empty-group mismatch rejects invalid \(reason) facts")
    }
    try requireTest(VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
                    acceptedEmptyGroupCandidate, expectedProcessID: 2) == nil,
                    "empty-group mismatch binds the expected process")
    tests.append("visible_privacy_empty_group_parent_mismatch_fails_closed")

    func windowOwnedGroupMismatchObservations(
        _ candidate: VisiblePrivacyGroupParentMismatchCandidate,
        includeNestedGroup: Bool = true,
        includeLeaf: Bool = true,
        nestedGroupSource: VisiblePrivacyChildrenSource = .visibleChildren,
        nestedGroupParentMatches: Bool = true,
        nestedGroupIdentity: Int = 3,
        leafSensitive: Bool = false
    ) -> [VisiblePrivacyAuditObservation]? {
        guard let groupFact = VisiblePrivacyGraph.classifyWindowOwnedGroupParentMismatch(
            candidate, expectedProcessID: 1) else { return nil }
        let root = VisiblePrivacyAuditObservation(identityOrdinal: 0, parentIdentityOrdinal: nil,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: false, fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: "AXWindow", subrole: "AXStandardWindow", frame: mismatchWindowFrame,
                childrenSource: .visibleChildren, childCount: 1))
        let button = VisiblePrivacyAuditObservation(identityOrdinal: 1, parentIdentityOrdinal: 0,
            processID: 1, actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false, forbiddenLayer: false,
            modal: nil, fact: VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0,
                role: "AXButton", subrole: nil, frame: mismatchButtonFrame,
                childrenSource: candidate.expectedParentChildrenSource, childCount: 1))
        let windowOwnedGroup = VisiblePrivacyAuditObservation(identityOrdinal: 2,
            parentIdentityOrdinal: 0, processID: 1, actualParentMatches: false,
            roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
            modalTypeValid: true, sensitive: false, forbiddenLayer: false, modal: nil,
            fact: groupFact)
        var observations = [root, button, windowOwnedGroup]
        if includeNestedGroup {
            observations.append(VisiblePrivacyAuditObservation(
                identityOrdinal: nestedGroupIdentity, parentIdentityOrdinal: 2,
                processID: 1, actualParentMatches: nestedGroupParentMatches,
                roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
                modalTypeValid: true, sensitive: false, forbiddenLayer: false, modal: nil,
                fact: VisiblePrivacyNodeFact(parentIndex: 2, childIndex: 0,
                    role: "AXGroup", subrole: nil,
                    frame: Rect(CGRect(x: 940, y: 290, width: 100, height: 30)),
                    childrenSource: nestedGroupSource, childCount: includeLeaf ? 1 : 0)))
        }
        if includeLeaf, includeNestedGroup {
            observations.append(VisiblePrivacyAuditObservation(
                identityOrdinal: 4, parentIdentityOrdinal: nestedGroupIdentity,
                processID: 1, actualParentMatches: true, roleTypeValid: true,
                subroleTypeValid: true, frameTypeValid: true, modalTypeValid: true,
                sensitive: leafSensitive, forbiddenLayer: false, modal: nil,
                fact: VisiblePrivacyNodeFact(parentIndex: 3, childIndex: 0,
                    role: "AXStaticText", subrole: nil,
                    frame: Rect(CGRect(x: 945, y: 295, width: 80, height: 20)),
                    childrenSource: .visibleChildren, childCount: 0)))
        }
        return observations
    }
    let acceptedWindowOwnedGroupCandidate = emptyGroupMismatchCandidate(
        axChildrenCountRead: .success(1), copiedAXChildrenCount: 1, axChildrenCopyComplete: true)
    if let nestedWindowOwnedGroup = windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate) {
        try requireTest(windowOwnedGroupMismatchGraphIsComplete(nestedWindowOwnedGroup),
                        "nonempty window-owned group and all nested children are audited")
        let nestedGroupFacts = VisiblePrivacyGraph.fingerprint(
            observations: nestedWindowOwnedGroup, expectedProcessID: 1,
            windowFrame: mismatchWindowFrame, nodeLimit: 512)
        try requireTest(nestedGroupFacts.map {
            VisiblePrivacyGraph.validFingerprint($0, windowFrame: mismatchWindowFrame)
        } == true, "canonical fingerprint accepts the audited nonempty group hierarchy")
        try requireTest(nestedWindowOwnedGroup[2].fact.childrenSource == .windowOwnedGroupAXChildren
            && nestedWindowOwnedGroup[2].fact.childCount == 1
            && nestedWindowOwnedGroup[3].fact.parentIndex == 2
            && nestedWindowOwnedGroup[4].fact.parentIndex == 3,
            "nonempty marker preserves the returned slot and recursively audited hierarchy")
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(
            windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate,
                includeNestedGroup: false) ?? []),
            "missing actual group child cannot satisfy the copied child count")
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(
            windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate,
                nestedGroupSource: .unknown) ?? []),
            "unknown nested children cannot authorize a complete group audit")
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(
            windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate,
                nestedGroupParentMatches: false) ?? []),
            "wrong actual parent for a recursively audited child is rejected")
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(
            windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate,
                nestedGroupIdentity: 2) ?? []),
            "an alias to an already visited group is rejected")
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(
            windowOwnedGroupMismatchObservations(acceptedWindowOwnedGroupCandidate,
                leafSensitive: true) ?? []),
            "a sensitive descendant cannot be hidden by the complete group marker")
        var markerOnOrdinaryWindowChild = nestedWindowOwnedGroup
        let rootFact = markerOnOrdinaryWindowChild[0].fact
        markerOnOrdinaryWindowChild[0] = VisiblePrivacyAuditObservation(
            identityOrdinal: 0, parentIdentityOrdinal: nil, processID: 1,
            actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false,
            forbiddenLayer: false, modal: false,
            fact: VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
                role: rootFact.role, subrole: rootFact.subrole, frame: rootFact.frame,
                childrenSource: rootFact.childrenSource, childCount: 2))
        let buttonFact = markerOnOrdinaryWindowChild[1].fact
        markerOnOrdinaryWindowChild[1] = VisiblePrivacyAuditObservation(
            identityOrdinal: 1, parentIdentityOrdinal: 0, processID: 1,
            actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false,
            forbiddenLayer: false, modal: nil,
            fact: VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0,
                role: buttonFact.role, subrole: buttonFact.subrole, frame: buttonFact.frame,
                childrenSource: buttonFact.childrenSource, childCount: 0))
        let groupFact = markerOnOrdinaryWindowChild[2].fact
        markerOnOrdinaryWindowChild[2] = VisiblePrivacyAuditObservation(
            identityOrdinal: 2, parentIdentityOrdinal: 0, processID: 1,
            actualParentMatches: true, roleTypeValid: true, subroleTypeValid: true,
            frameTypeValid: true, modalTypeValid: true, sensitive: false,
            forbiddenLayer: false, modal: nil,
            fact: VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 1,
                role: groupFact.role, subrole: groupFact.subrole, frame: groupFact.frame,
                childrenSource: groupFact.childrenSource, childCount: groupFact.childCount,
                actualParentIndex: 0))
        try requireTest(!windowOwnedGroupMismatchGraphIsComplete(markerOnOrdinaryWindowChild),
                        "complete AXChildren marker is valid only for the root-owned button group case")
    } else {
        try requireTest(false, "complete nonempty window-owned group candidate must classify")
    }
    try requireTest(VisiblePrivacyGraph.classifyWindowOwnedGroupParentMismatch(
        emptyGroupMismatchCandidate(axChildrenCountRead: .success(1),
            copiedAXChildrenCount: 0, axChildrenCopyComplete: true), expectedProcessID: 1) == nil,
        "count and copied list length must agree")
    try requireTest(VisiblePrivacyGraph.classifyWindowOwnedGroupParentMismatch(
        emptyGroupMismatchCandidate(axChildrenCountRead: .success(1),
            copiedAXChildrenCount: 1, axChildrenCopyComplete: false), expectedProcessID: 1) == nil,
        "a count without a complete copy cannot mark group children audited")
    try requireTest(VisiblePrivacyGraph.windowOwnedGroupAXChildrenPostcheckMatches(
        expectedCount: 2, countRead: .success(2), copiedCount: 2,
        referencesMatchInOrder: [true, true]),
        "same read child count and CF-reference order pass postcheck")
    for (name, countRead, copiedCount, references) in [
        ("postcheck count", VisiblePrivacyAXChildrenCountRead.success(1), 2, [true, true]),
        ("postcheck copy count", VisiblePrivacyAXChildrenCountRead.success(2), 1, [true]),
        ("postcheck identity order", VisiblePrivacyAXChildrenCountRead.success(2), 2, [true, false]),
        ("postcheck noValue", VisiblePrivacyAXChildrenCountRead.noValue, 2, [true, true]),
    ] {
        try requireTest(!VisiblePrivacyGraph.windowOwnedGroupAXChildrenPostcheckMatches(
            expectedCount: 2, countRead: countRead, copiedCount: copiedCount,
            referencesMatchInOrder: references),
            "window-owned group rejects \(name) drift")
    }
    tests.append("visible_privacy_window_owned_group_audits_all_children_and_postchecks_refs")

    let zeroVisibleChildrenRoot = VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil,
        role: "AXWindow", subrole: "AXStandardWindow", frame: graphWindow,
        childrenSource: .visibleChildren, childCount: 0)
    let zeroVisibleChildrenObservation = VisiblePrivacyAuditObservation(identityOrdinal: 0,
        parentIdentityOrdinal: nil, processID: 1, actualParentMatches: true,
        roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
        modalTypeValid: true, sensitive: false, forbiddenLayer: false, modal: false,
        fact: zeroVisibleChildrenRoot)
    try requireTest(graphIsComplete([zeroVisibleChildrenObservation]),
                    "timely explicit AXVisibleChildren count zero is valid")
    let fullChildrenFallback = graphObservations(rootSource: .childrenFallback,
        childRole: "AXButton", childSource: .leafChildrenUnsupported)
    try requireTest(graphIsComplete(fullChildrenFallback),
                    "complete AXChildren fallback and documented leaf are accepted")
    for (role, subrole) in [
        ("AXRow", "AXTableRow"),
        ("AXButton", "AXFullScreenButton"),
        ("AXButton", "AXIncrementPage"),
        ("AXButton", "AXDecrementPage"),
    ] {
        try requireTest(graphIsComplete(graphObservations(childRole: role, childSubrole: subrole)),
                        "standard \(role)/\(subrole) pair is accepted")
    }
    try requireTest(graphIsComplete(graphObservations(childRole: "AXValueIndicator")),
                    "standard AXValueIndicator role is accepted and recursively enumerated")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXButton", childSubrole: "AXTableRow")),
                    "AXTableRow subrole is rejected on non-row roles")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXGroup", childSubrole: "AXFullScreenButton")),
                    "button subroles are rejected on non-button roles")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXCustomWidget")),
                    "unknown custom roles remain rejected")
    let emptyScrollBarArrow = scrollBarArrowObservations()
    let emptyScrollBarArrowFacts = VisiblePrivacyGraph.fingerprint(
        observations: emptyScrollBarArrow, expectedProcessID: 1,
        windowFrame: scrollBarArrowWindow, nodeLimit: 512)
    try requireTest(emptyScrollBarArrowFacts?.last?.frame == zeroArrowFrame,
                    "audited empty scrollbar arrow is accepted with its observed zero frame intact")
    try requireTest(scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childSource: .leafChildrenUnsupported)),
                    "documented unsupported leaf is accepted only with complete empty structure")
    try requireTest(scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    subrole: "AXDecrementArrow")),
                    "both standard scrollbar arrow subroles share the zero-frame exception")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    frame: Rect(CGRect(x: 988, y: 305, width: 0, height: 4)))),
                    "one zero dimension does not qualify as an empty scrollbar arrow")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    frame: Rect(CGRect(x: 988, y: 305, width: 4, height: 0)))),
                    "the other single zero dimension does not qualify as an empty arrow")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    parentRole: "AXGroup")),
                    "empty arrow exception requires an audited AXScrollBar parent")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    role: "AXTextField")),
                    "empty arrow exception requires the AXButton role")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childSource: .unknown)),
                    "unknown empty-arrow children are rejected")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childCount: 1)),
                    "nonempty zero-frame arrow descendants are rejected")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childParentMatches: false)),
                    "empty arrow still requires actual AXParent identity equality")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childFrameTypeValid: false)),
                    "unavailable geometry type is rejected for empty arrow")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    frame: Rect(CGRect(x: CGFloat.nan, y: 305, width: 0, height: 0)))),
                    "nonfinite empty-arrow geometry is rejected")
    try requireTest(!scrollBarArrowGraphIsComplete(scrollBarArrowObservations(
                    childSensitive: true)),
                    "empty arrow exception does not bypass sensitive-control rejection")
    try requireTest(!graphIsComplete(graphObservations(rootSource: .unknown)),
                    "unknown child enumeration cannot become an empty list")
    try requireTest(!graphIsComplete(graphObservations(rootSource: .noValue)),
                    "AX noValue cannot be treated as an empty container")
    try requireTest(!graphIsComplete(graphObservations(rootSource: .truncated)),
                    "truncated child enumeration is rejected")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXGroup",
                    childSource: .leafChildrenUnsupported)),
                    "containers cannot use the unsupported-leaf exception")
    try requireTest(!graphIsComplete(graphObservations(childIdentity: 0)),
                    "duplicate read-local AX identities are rejected")
    try requireTest(!graphIsComplete(graphObservations(childParentMatches: false)),
                    "AXParent mismatch is rejected")
    try requireTest(!graphIsComplete(graphObservations(childParentIndex: 1,
                    childParentIdentity: 1)),
                    "cyclic visible descendant graph is rejected")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXTextField",
                    childSensitive: true)),
                    "sensitive controls are rejected")
    try requireTest(!graphIsComplete(graphObservations(childRole: "AXMenu",
                    childForbiddenLayer: true)),
                    "transient menu layers are rejected")
    try requireTest(!graphIsComplete(graphObservations(childProcessID: 2)),
                    "foreign-process visible descendants are rejected")
    try requireTest(!graphIsComplete(graphObservations(childFrame: Rect(CGRect(x: 900, y: 900,
                    width: 20, height: 20)))),
                    "visible child geometry outside the captured window is rejected")
    tests.append("visible_privacy_graph_rejects_unknown_alias_parent_secure_overlay_and_budget_facts")
    let long = boundedText(String(repeating: "界", count: 1024))
    try requireTest(long.utf8.count <= 512, "bounded UTF8 labels")
    tests.append("bounded_utf8_labels")
    var longTextOcr = Ocr(complete: true, reason: nil, blocks: [])
    appendRecognizedText(String(repeating: "界", count: 200), confidence: 1, frame: frame, to: &longTextOcr)
    try requireTest(!longTextOcr.complete && longTextOcr.reason == "OCR text byte budget exceeded"
                    && longTextOcr.blocks.count == 1 && longTextOcr.blocks[0].text.utf8.count <= 512,
                    "OCR long text is bounded and explicitly incomplete")
    tests.append("ocr_text_truncation_explicit_incomplete")
    let encoded = try JSONEncoder().encode(UiValue.bool(true))
    let value = try JSONSerialization.jsonObject(with: encoded) as? [String: Any]
    try requireTest(value?["type"] as? String == "bool" && value?["value"] as? Bool == true, "Rust value wire encoding")
    tests.append("rust_value_wire_encoding")

    for position in [0.0, 0.25, 1.0] {
        let axis = axScrollAxisState(value: NSNumber(value: position), minimum: NSNumber(value: 0.0), maximum: NSNumber(value: 1.0))
        try requireTest(axis?.value == position && axis?.min == 0 && axis?.max == 1,
                        "scroll metrics preserve observed normalized values and endpoints")
    }
    let customAxis = axScrollAxisState(value: NSNumber(value: -5.5), minimum: NSNumber(value: -10.0), maximum: NSNumber(value: 90.0))
    try requireTest(customAxis?.value == -5.5 && customAxis?.min == -10 && customAxis?.max == 90,
                    "scroll metrics preserve declared AX units rather than inventing normalized bounds")
    tests.append("scroll_metrics_preserve_observed_units_and_boundaries")

    for invalid in [Double.nan, .infinity, -.infinity] {
        try requireTest(ScrollAxisState(value: invalid, min: 0, max: 1) == nil,
                        "scroll metrics reject nonfinite value")
        try requireTest(ScrollAxisState(value: 0.5, min: invalid, max: 1) == nil,
                        "scroll metrics reject nonfinite minimum")
        try requireTest(ScrollAxisState(value: 0.5, min: 0, max: invalid) == nil,
                        "scroll metrics reject nonfinite maximum")
    }
    try requireTest(ScrollAxisState(value: -0.01, min: 0, max: 1) == nil &&
                    ScrollAxisState(value: 1.01, min: 0, max: 1) == nil &&
                    ScrollAxisState(value: 1, min: 1, max: 1) == nil &&
                    ScrollAxisState(value: 0, min: 1, max: -1) == nil,
                    "scroll metrics reject out-of-range and degenerate bounds")
    tests.append("scroll_metrics_reject_nonfinite_out_of_range_and_degenerate_values")

    try requireTest(axScrollAxisState(value: nil, minimum: NSNumber(value: 0.0), maximum: NSNumber(value: 1.0)) == nil,
                    "missing scroll value remains unknown")
    let partialAxis = axScrollAxisState(value: NSNumber(value: 0.5), minimum: nil, maximum: nil)
    try requireTest(partialAxis?.value == 0.5 && partialAxis?.min == nil && partialAxis?.max == nil,
                    "partial scrollbar observation preserves value without invented limits")
    let lowerOnly = axScrollAxisState(value: NSNumber(value: 0.5), minimum: NSNumber(value: 0.0), maximum: nil)
    let upperOnly = axScrollAxisState(value: NSNumber(value: 0.5), minimum: nil, maximum: NSNumber(value: 1.0))
    try requireTest(lowerOnly?.value == 0.5 && lowerOnly?.min == 0 && lowerOnly?.max == nil &&
                    upperOnly?.value == 0.5 && upperOnly?.min == nil && upperOnly?.max == 1,
                    "independently missing scrollbar limits remain unknown")
    try requireTest(ScrollAxisState(value: -7, min: nil, max: nil)?.value == -7 &&
                    ScrollAxisState(value: -0.01, min: 0, max: nil) == nil &&
                    ScrollAxisState(value: 1.01, min: nil, max: 1) == nil,
                    "only observed scrollbar limits constrain values")
    for malformed: AnyObject in [NSNumber(value: true), "0.5" as NSString] {
        try requireTest(axScrollAxisState(value: malformed, minimum: NSNumber(value: 0.0), maximum: NSNumber(value: 1.0)) == nil &&
                        axScrollAxisState(value: NSNumber(value: 0.5), minimum: malformed, maximum: NSNumber(value: 1.0)) == nil &&
                        axScrollAxisState(value: NSNumber(value: 0.5), minimum: NSNumber(value: 0.0), maximum: malformed) == nil,
                        "scroll metrics reject booleans and numeric text")
    }
    tests.append("scroll_metrics_preserve_partial_values_and_reject_nonnumeric_data")

    let verticalAxis = ScrollAxisState(value: 0.5, min: 0, max: 1)!
    let state = ScrollState(horizontal: nil, vertical: verticalAxis)
    let scrollWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) as? [String: Any]
    let verticalWire = scrollWire?["vertical"] as? [String: Any]
    try requireTest(scrollWire?["horizontal"] == nil && verticalWire?["value"] as? Double == 0.5 &&
                    verticalWire?["min"] as? Double == 0 && verticalWire?["max"] as? Double == 1,
                    "Rust scroll wire encoding preserves optional axes")
    let partialWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(partialAxis)) as? [String: Any]
    try requireTest(partialWire?["value"] as? Double == 0.5 && partialWire?["min"] == nil && partialWire?["max"] == nil,
                    "partial scrollbar wire preserves unknown limits")
    let objectWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(original.structure.objects[0])) as? [String: Any]
    try requireTest(objectWire?["scroll"] == nil, "unavailable scroll metrics remain absent")
    tests.append("rust_scroll_wire_encoding_preserves_unknown_axes")

    var beforeScroll = original, afterScroll = original
    beforeScroll.structure.objects[0].scroll = state
    afterScroll.structure.objects[0].scroll = ScrollState(horizontal: nil, vertical: ScrollAxisState(value: 0.75, min: 0, max: 1))
    try requireTest(unchangedPrivacyContext(before: beforeScroll, after: beforeScroll) &&
                    !unchangedPrivacyContext(before: beforeScroll, after: afterScroll),
                    "changed scroll metrics reject mismatched capture context")
    tests.append("changed_scroll_metrics_reject_mismatched_capture_context")

    guard let context = CGContext(data: nil, width: 640, height: 160, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        throw NativeFailure("offline bitmap allocation failed")
    }
    context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    context.fill(CGRect(x: 0, y: 0, width: 640, height: 160))
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName("Helvetica" as CFString, 42, nil),
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(red: 0, green: 0, blue: 0, alpha: 1)
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: "NATIVE SOURCE 42", attributes: attributes))
    context.textPosition = CGPoint(x: 40, y: 70)
    CTLineDraw(line, context)
    guard let image = context.makeImage() else { throw NativeFailure("offline bitmap unavailable") }
    let ocr = recognizeText(image: image, window: Rect(CGRect(x: 200, y: -120, width: 320, height: 80)), maxBlocks: 128)
    try requireTest(ocr.complete, "Vision offline OCR completes (\(ocr.reason ?? "unknown"))")
    let recognized = ocr.blocks.map(\.text).joined(separator: " ").uppercased()
    try requireTest(recognized.contains("NATIVE SOURCE 42"), "Vision generated text recognition")
    try requireTest(ocr.blocks.allSatisfy { $0.frame.x >= 200 && $0.frame.y >= -120 && $0.frame.width > 0 },
                    "Vision mapped global frame")
    tests.append("vision_generated_image_recognition_and_geometry")
    let capped = recognizeText(image: image, window: frame, maxBlocks: 0)
    try requireTest(!capped.complete && capped.blocks.isEmpty && capped.reason != nil, "OCR overflow explicit incomplete")
    tests.append("ocr_overflow_explicit_incomplete")
    func allowsWindowOwnedGroupParentMismatch(
        expectedParentRole: String? = kAXButtonRole,
        expectedParentWasVisited: Bool = true,
        expectedParentProcessMatches: Bool = true,
        capturedWindowIsUnique: Bool = true,
        actualParentRole: String? = kAXWindowRole,
        actualParentIsCapturedWindow: Bool = true,
        childProcessMatches: Bool = true,
        childRole: String? = kAXGroupRole,
        childSubrole: TableScopeSubroleRead = .absent,
        childFrameIsPositive: Bool = true
    ) -> Bool {
        TableScopeSearchValidation.allowsWindowOwnedGroupParentMismatch(
            expectedParentRole: expectedParentRole,
            expectedParentWasVisited: expectedParentWasVisited,
            expectedParentProcessMatches: expectedParentProcessMatches,
            capturedWindowIsUnique: capturedWindowIsUnique,
            actualParentRole: actualParentRole,
            actualParentIsCapturedWindow: actualParentIsCapturedWindow,
            childProcessMatches: childProcessMatches,
            childRole: childRole, childSubrole: childSubrole,
            childFrameIsPositive: childFrameIsPositive)
    }
    try requireTest(allowsWindowOwnedGroupParentMismatch()
                    && allowsWindowOwnedGroupParentMismatch(childSubrole: .value("")),
                    "only an audited button slot may recurse a same-process root-owned nil-subrole AXGroup")
    try requireTest(TableScopeSearchValidation.childScrollPath(
        isWindowOwnedGroup: true, expectedParentPath: ["Preferences"]).isEmpty
                    && TableScopeSearchValidation.childScrollPath(
        isWindowOwnedGroup: false, expectedParentPath: ["Preferences"]) == ["Preferences"],
        "the button's scroll ancestry is not inherited by its root-owned AXGroup child")
    let rejectedWindowOwnedGroupCases = [
        allowsWindowOwnedGroupParentMismatch(expectedParentRole: kAXGroupRole),
        allowsWindowOwnedGroupParentMismatch(expectedParentWasVisited: false),
        allowsWindowOwnedGroupParentMismatch(expectedParentProcessMatches: false),
        allowsWindowOwnedGroupParentMismatch(capturedWindowIsUnique: false),
        allowsWindowOwnedGroupParentMismatch(actualParentRole: kAXGroupRole),
        allowsWindowOwnedGroupParentMismatch(actualParentIsCapturedWindow: false),
        allowsWindowOwnedGroupParentMismatch(childProcessMatches: false),
        allowsWindowOwnedGroupParentMismatch(childRole: kAXScrollAreaRole),
        allowsWindowOwnedGroupParentMismatch(childSubrole: .value("AXSheet")),
        allowsWindowOwnedGroupParentMismatch(childSubrole: .invalid),
        allowsWindowOwnedGroupParentMismatch(childFrameIsPositive: false),
    ]
    try requireTest(rejectedWindowOwnedGroupCases.allSatisfy({ !$0 }),
                    "unknown role, subrole, identity, parent, process, or geometry cannot use group recursion")
    let scopeGroupFrameBefore = Rect(CGRect(x: 30, y: 40, width: 90, height: 35))
    try requireTest(TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
        expectedParentWasVisited: true, expectedParentRole: kAXButtonRole,
        expectedParentProcessMatches: true, childProcessMatches: true,
        childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
        childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        "group reread preserves the visited button, process, nil subrole, role, and frame")
    let rejectedWindowOwnedGroupIdentityReads = [
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: false, expectedParentRole: kAXButtonRole,
            expectedParentProcessMatches: true, childProcessMatches: true,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
            childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: true, expectedParentRole: kAXScrollAreaRole,
            expectedParentProcessMatches: true, childProcessMatches: true,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
            childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: true, expectedParentRole: kAXButtonRole,
            expectedParentProcessMatches: false, childProcessMatches: true,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
            childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: true, expectedParentRole: kAXButtonRole,
            expectedParentProcessMatches: true, childProcessMatches: false,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
            childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: true, expectedParentRole: kAXButtonRole,
            expectedParentProcessMatches: true, childProcessMatches: true,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .value("AXSheet"),
            childFrameBefore: scopeGroupFrameBefore, childFrameAfter: scopeGroupFrameBefore),
        TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
            expectedParentWasVisited: true, expectedParentRole: kAXButtonRole,
            expectedParentProcessMatches: true, childProcessMatches: true,
            childRole: kAXGroupRole, childSubroleBefore: .absent, childSubroleAfter: .absent,
            childFrameBefore: scopeGroupFrameBefore,
            childFrameAfter: Rect(CGRect(x: 31, y: 40, width: 90, height: 35))),
    ]
    try requireTest(rejectedWindowOwnedGroupIdentityReads.allSatisfy({ !$0 }),
                    "changed parent, PID, role, subrole, or frame fails the group reread")
    try requireTest(TableScopeSearchValidation.windowOwnedGroupTraversalComplete(
        mismatchWasAuthorized: true, rawChildrenCount: 1,
        recursivelyWalkedEveryRelevantChild: true),
        "a nonempty authorized group completes only after the copied branch was recursively walked")
    try requireTest(!TableScopeSearchValidation.windowOwnedGroupTraversalComplete(
        mismatchWasAuthorized: true, rawChildrenCount: nil,
        recursivelyWalkedEveryRelevantChild: true)
                    && !TableScopeSearchValidation.windowOwnedGroupTraversalComplete(
        mismatchWasAuthorized: true, rawChildrenCount: 1,
        recursivelyWalkedEveryRelevantChild: false)
                    && !TableScopeSearchValidation.windowOwnedGroupTraversalComplete(
        mismatchWasAuthorized: false, rawChildrenCount: 1,
        recursivelyWalkedEveryRelevantChild: true),
        "missing actual child enumeration, skipped recursion, or an unauthorized parent mismatch cannot complete")
    try requireTest(TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
        parentBeforeIsCapturedWindow: true, parentAfterIsCapturedWindow: true,
        countStatus: .success, expectedCount: 2, observedCount: 2,
        copyStatus: .success, copiedCount: 2, referencesMatchInOrder: [true, true]),
        "window-owned group reread requires unchanged complete ordered children")
    let rejectedGroupPostchecks = [
        TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
            parentBeforeIsCapturedWindow: false, parentAfterIsCapturedWindow: true,
            countStatus: .success, expectedCount: 1, observedCount: 1,
            copyStatus: .success, copiedCount: 1, referencesMatchInOrder: [true]),
        TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
            parentBeforeIsCapturedWindow: true, parentAfterIsCapturedWindow: false,
            countStatus: .success, expectedCount: 1, observedCount: 1,
            copyStatus: .success, copiedCount: 1, referencesMatchInOrder: [true]),
        TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
            parentBeforeIsCapturedWindow: true, parentAfterIsCapturedWindow: true,
            countStatus: .noValue, expectedCount: 1, observedCount: 1,
            copyStatus: .success, copiedCount: 1, referencesMatchInOrder: [true]),
        TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
            parentBeforeIsCapturedWindow: true, parentAfterIsCapturedWindow: true,
            countStatus: .success, expectedCount: 2, observedCount: 1,
            copyStatus: .success, copiedCount: 1, referencesMatchInOrder: [true]),
        TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
            parentBeforeIsCapturedWindow: true, parentAfterIsCapturedWindow: true,
            countStatus: .success, expectedCount: 2, observedCount: 2,
            copyStatus: .success, copiedCount: 2, referencesMatchInOrder: [true, false]),
    ]
    try requireTest(rejectedGroupPostchecks.allSatisfy({ !$0 }),
                    "group parent, count, copy, or child CF order changes keep the census incomplete")
    tests.append("scope_census_root_owned_group_is_recursively_audited_and_revalidated")
    tests.append(contentsOf: try runStreamTests())
    tests.append(contentsOf: try runWatchControlTests())
    tests.append(contentsOf: try runWatchCaptureGeometryTests())
    tests.append(contentsOf: try runAXSessionTests())
    tests.append(contentsOf: try runTableViewportTests())
    tests.append(contentsOf: try runTableScopeSearchTests())
    tests.append(contentsOf: try runAuxiliaryTransitionTests())
    tests.append(contentsOf: try runOwnedDocumentReceiverTests())
    tests.append(contentsOf: try runOwnedDocumentBootstrapTests())
    tests.append(contentsOf: try runMenuHistoryDiagnosticTests())
    tests.append(contentsOf: try runOtherWindowDestructionProofTests())
    tests.append(contentsOf: try runFocusContextTests())
    tests.append(contentsOf: try runWindowContextTests())
    return SelfTestResponse(test_count: tests.count, tests: tests)
}
