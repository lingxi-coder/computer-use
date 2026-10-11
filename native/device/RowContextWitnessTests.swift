import Foundation
import ApplicationServices

private func rowContextTestRect(_ x: Double, _ y: Double, _ width: Double, _ height: Double) -> HitRect {
    HitRect(CGRect(x: x, y: y, width: width, height: height))
}

private func rowContextTestWitness(
    tableAXEnabled: NativeAXEnabledObservation = .enabled,
    targetAXEnabled: NativeAXEnabledObservation = .enabled,
    targetSelected: Bool = false,
    targetSettable: Bool = true,
    targetPoint: NativeDeviceContextPoint = NativeDeviceContextPoint(x: 1, y: 25),
    targetAXIndex: UInt32 = 42,
    targetRole: String = kAXRowRole,
    targetScroll: NativeDeviceContextScrollState? = nil,
    columnValues: [NativeDeviceContextRowColumn]? = nil,
    selectorColumns: [NativeDeviceContextRowSelectorColumn]? = nil,
    visibleRows: [NativeDeviceContextVisibleRow]? = nil,
    scrollLabel: String = "Table content"
) -> NativeDeviceContextWitness {
    let window = rowContextTestRect(0, 0, 500, 400)
    let tableFrame = rowContextTestRect(0, 0, 300, 200)
    let viewport = rowContextTestRect(0, 0, 300, 200)
    let row42 = rowContextTestRect(0, 20, 300, 20)
    let row511 = rowContextTestRect(0, 40, 300, 20)
    let columns = columnValues ?? [
        NativeDeviceContextRowColumn(ax_index: 0, header: "Name", frame: rowContextTestRect(0, 0, 150, 20)),
        NativeDeviceContextRowColumn(ax_index: 1, header: "Owner", frame: rowContextTestRect(150, 0, 150, 20)),
    ]
    let context = NativeDeviceContextTableViewportV1(
        app: NativeDeviceContextApp(bundle_id: "com.example.rows", pid: 42),
        window: NativeDeviceContextWindow(window_id: "window-42", title: "Records",
            frame: window, kind: .window, modal: false, owner_bundle_id: "com.example.rows"),
        table: NativeDeviceContextObject(role: kAXTableRole, subrole: nil,
            label: "Records", frame: tableFrame),
        target: NativeDeviceContextTarget(
            object: NativeDeviceContextObject(role: targetRole, subrole: "AXTableRow",
                label: "row alpha", frame: row42), group: nil, scroll: nil),
        scroll_path: [NativeDeviceContextObject(role: kAXScrollAreaRole, subrole: nil,
            label: scrollLabel, frame: viewport)],
        viewport_frame: viewport,
        ordered_columns: columns,
        selector_columns: selectorColumns ?? [NativeDeviceContextRowSelectorColumn(ax_index: 0,
            header: "Name", expected_value: .text("alpha"))],
        visible_rows: visibleRows ?? [
            NativeDeviceContextVisibleRow(ax_index: 42, frame: row42, selector_values: [.text("alpha")]),
            NativeDeviceContextVisibleRow(ax_index: 511, frame: row511, selector_values: [.text("beta")]),
        ],
        target_row_ax_index: targetAXIndex,
        table_ax_enabled: tableAXEnabled,
        target_ax_enabled: targetAXEnabled,
        target_selected: targetSelected,
        target_ax_selected_settable: targetSettable,
        hit_point: targetPoint)
    if let targetScroll {
        let updated = NativeDeviceContextTableViewportV1(app: context.app, window: context.window,
            table: context.table,
            target: NativeDeviceContextTarget(object: context.target.object, group: context.target.group,
                scroll: targetScroll),
            scroll_path: context.scroll_path, viewport_frame: context.viewport_frame,
            ordered_columns: context.ordered_columns, selector_columns: context.selector_columns,
            visible_rows: context.visible_rows, target_row_ax_index: context.target_row_ax_index,
            table_ax_enabled: context.table_ax_enabled,
            target_ax_enabled: context.target_ax_enabled,
            target_selected: context.target_selected,
            target_ax_selected_settable: context.target_ax_selected_settable, hit_point: context.hit_point)
        return .tableViewportV1(updated)
    }
    return .tableViewportV1(context)
}

private func windowRootTestWitness(
    sourceWindowID: String = "window-42",
    sourceSessionID: String = "source-window-1",
    sourceEpoch: UInt64 = 3,
    sourceSequence: UInt64 = 17,
    sourceObservedMS: UInt64 = 90,
    sourceProcess: NativeMenuProcessIdentity = NativeMenuProcessIdentity(
        pid: 42, start_seconds: 1_700_000_000, start_micros: 123_456),
    title: String? = "Records",
    windowFrame: HitRect? = nil,
    rootRole: String = kAXWindowRole,
    rootSubrole: String? = kAXStandardWindowSubrole,
    rootLabel: String? = nil,
    modal: Bool = false,
    ownerBundle: String = "com.example.rows"
) -> NativeDeviceContextWitness {
    let frame = windowFrame ?? rowContextTestRect(0, 0, 500, 400)
    let app = NativeDeviceContextApp(bundle_id: "com.example.rows", pid: 42)
    let window = NativeDeviceContextWindow(window_id: sourceWindowID, title: title.map(bindingLabel),
        frame: frame, kind: .window, modal: modal, owner_bundle_id: ownerBundle)
    let root = NativeDeviceContextObject(role: rootRole, subrole: rootSubrole,
        label: rootLabel ?? bindingLabel(title ?? ""), frame: frame)
    let source = NativeDeviceSourceWindowContext(source_session_id: sourceSessionID,
        session_epoch: sourceEpoch, process: sourceProcess, origin_window_id: sourceWindowID,
        origin_window_frame: frame, verified_source_sequence: sourceSequence,
        verified_observed_ms: sourceObservedMS)
    return .windowRootV1(NativeDeviceContextWindowRootV1(app: app, window: window,
        root: root, source: source))
}

private func windowRootTestScope(
    path: [NativeDeviceContextRowContainerScope] = [
        NativeDeviceContextRowContainerScope(role: "ax_scroll_area", label: "Table content")
    ],
    headers: [String] = ["Name", "Owner"]
) -> NativeDeviceContextRowTableScope {
    NativeDeviceContextRowTableScope(scroll_path: path, column_headers: headers)
}

private func windowRootTestFingerprint(label: String = "records",
                                       frame: HitRect = rowContextTestRect(0, 0, 500, 400)) -> NativeBindingFingerprint {
    NativeBindingFingerprint(app: "com.example.rows", app_pid: 42, window_id: "window-42",
        ancestor_roles: [], group: nil, role: kAXWindowRole, subrole: kAXStandardWindowSubrole,
        label: label, sibling_index: 0, frame: frame.rounded, scroll_container: nil)
}


/// Pure wire and admission-shape tests. AX entity identity is never faked here.
func runRowContextWitnessTests(startingAt offset: Int) throws -> Int {
    var passed = 0
    func check(_ condition: Bool, sourceLine: UInt = #line) throws {
        guard condition else {
            FileHandle.standardError.write(Data("device self-test failed at RowContextWitnessTests.swift:\(sourceLine)\n".utf8))
            throw DeviceTestFailure(check: offset + passed + 1)
        }
        passed += 1
    }
    func childBoundary(_ roles: [String?]) -> ScopedWindowChildBoundaryFacts {
        let children = roles.map {
            ScopedWindowChildBoundaryEntry(isAXElement: true, sameProcess: true,
                parentMatchesWindow: true, role: $0)
        }
        let count = children.count
        return ScopedWindowChildBoundaryFacts(beforeCountError: AXError.success.rawValue,
            beforeCount: count, copyError: AXError.success.rawValue, rawIsArray: true,
            arrayCount: count, typedArrayCount: count,
            afterCountError: AXError.success.rawValue, afterCount: count,
            referencesUnique: true, timely: true, children: children)
    }
    let childBoundaryFacts = childBoundary(["AXGroup", "AXToolbar"])
    try check(scopedWindowChildrenBoundaryIsValid(childBoundaryFacts))

    // Probe02-derived shape only: seven ordinary children versus eight with AXSheet.
    // The synthetic child references here carry no live or cross-process authority.
    let probeOrdinaryChildRoles = Array(repeating: "AXGroup" as String?, count: 7)
    let probeNoSheetShape = childBoundary(probeOrdinaryChildRoles)
    let probeAttachedSheetShape = childBoundary(probeOrdinaryChildRoles + ["AXSheet" as String?])
    try check(scopedWindowChildrenBoundaryIsValid(probeNoSheetShape))
    try check(!scopedWindowChildrenBoundaryIsValid(probeAttachedSheetShape))

    for error in [AXError.attributeUnsupported, AXError.noValue] {
        var unsupportedCount = childBoundaryFacts
        unsupportedCount.beforeCountError = error.rawValue
        try check(!scopedWindowChildrenBoundaryIsValid(unsupportedCount))
        var unsupportedCopy = childBoundaryFacts
        unsupportedCopy.copyError = error.rawValue
        try check(!scopedWindowChildrenBoundaryIsValid(unsupportedCopy))
        var unsupportedAfterCount = childBoundaryFacts
        unsupportedAfterCount.afterCountError = error.rawValue
        try check(!scopedWindowChildrenBoundaryIsValid(unsupportedAfterCount))
    }
    var malformedChildren = childBoundaryFacts
    malformedChildren.rawIsArray = false
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.arrayCount = nil
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.beforeCount = 65
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.beforeCount = 3
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.afterCount = 3
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.arrayCount = 1
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.typedArrayCount = 1
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.children[0] = ScopedWindowChildBoundaryEntry(isAXElement: false,
        sameProcess: true, parentMatchesWindow: true, role: "AXGroup")
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.children[0] = ScopedWindowChildBoundaryEntry(isAXElement: true,
        sameProcess: false, parentMatchesWindow: true, role: "AXGroup")
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.children[0] = ScopedWindowChildBoundaryEntry(isAXElement: true,
        sameProcess: true, parentMatchesWindow: false, role: "AXGroup")
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.children[0] = ScopedWindowChildBoundaryEntry(isAXElement: true,
        sameProcess: true, parentMatchesWindow: true, role: nil)
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.children[0] = ScopedWindowChildBoundaryEntry(isAXElement: true,
        sameProcess: true, parentMatchesWindow: true, role: "AXFutureRole")
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    for prohibitedRole in ["AXSheet", "AXWindow", "AXDrawer"] {
        try check(!scopedWindowChildRoleIsAllowed(prohibitedRole))
    }
    try check(!scopedWindowChildRoleIsAllowed(""))
    malformedChildren = childBoundaryFacts
    malformedChildren.referencesUnique = false
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    malformedChildren = childBoundaryFacts
    malformedChildren.timely = false
    try check(!scopedWindowChildrenBoundaryIsValid(malformedChildren))
    try check(scopedReferencesAreUnique([1, 2, 3], equal: { $0 == $1 }))
    try check(!scopedReferencesAreUnique([1, 2, 1], equal: { $0 == $1 }))
    try check(scopedReferenceSetsMatch([1, 2, 3], [3, 1, 2], equal: { $0 == $1 }))
    try check(!scopedReferenceSetsMatch([1, 2, 3], [3, 1, 4], equal: { $0 == $1 }))
    try check(!scopedReferenceSetsMatch([1, 2, 3], [1, 2], equal: { $0 == $1 }))

    let witness = rowContextTestWitness()
    guard let context = witness.tableViewport else {
        throw DeviceTestFailure(check: offset + passed + 1)
    }
    let frame = context.target.object.frame
    let binding = NativeBindingFingerprint(app: context.app.bundle_id,
        app_pid: context.app.pid, window_id: context.window.window_id,
        ancestor_roles: [kAXWindowRole, kAXScrollAreaRole, kAXTableRole], group: nil,
        role: kAXRowRole, subrole: "AXTableRow", label: context.target.object.label,
        sibling_index: 0, frame: frame.rounded, scroll_container: nil)
    let hit = HitRequest(request_id: "row-hit", pid: 42, bundle_id: context.app.bundle_id,
        window_id: 42, x: context.hit_point.x, y: context.hit_point.y, frame: frame,
        binding: binding, binding_digest: binding.digest, contextWitness: witness)

    try check(context.valid)
    try check(context.table_ax_enabled == .enabled && context.target_ax_enabled == .enabled)
    try check(NativeAXEnabledObservation.attributeUnsupported.rawValue == "attribute_unsupported")
    try check(rowContextTestWitness(tableAXEnabled: .attributeUnsupported,
        targetAXEnabled: .attributeUnsupported).tableViewport?.valid == true)
    try check(witness.valid)
    try check(!witness.supportsNativeInput)
    try check(witness.supportsNativeInput(action: "ax_set_selected_true"))
    try check(witness.supportsAction("ax_set_selected_true"))
    try check(!witness.supportsAction("ax_press"))
    try check(!witness.supportsAction("click"))
    try check(witness.matchesBinding(binding, displayFrame: frame))
    // AXIndex 42 is intentionally distinct from fp1's sibling ordinal 0.
    try check(context.target_row_ax_index == 42 && binding.sibling_index == 0)
    try check(hit.validFor(action: "ax_set_selected_true"))
    try check(!hit.valid)
    try check(!hit.validFor(action: "click"))
    try check(!HitRequest.roles(action: "click").contains(kAXRowRole))
    try check(HitRequest.roles(action: "ax_set_selected_true") == [kAXRowRole])

    let sourceScope = NativeBindingProofScope.tableViewport(table: "source-table",
        scrollContainer: "source-scroll", viewportFrame: context.viewport_frame,
        visibleRowsComplete: true, columnsComplete: true)
    try check(sourceScope.supportsRowSelection(witness: witness))
    try check(!NativeBindingProofScope.wholeStructure.supportsRowSelection(witness: witness))
    try check(!NativeBindingProofScope.tableViewport(table: "source-table",
        scrollContainer: "source-scroll", viewportFrame: rowContextTestRect(0, 0, 299, 200),
        visibleRowsComplete: true, columnsComplete: true).supportsRowSelection(witness: witness))
    try check(!NativeBindingProofScope.tableViewport(table: "source-table",
        scrollContainer: "source-scroll", viewportFrame: context.viewport_frame,
        visibleRowsComplete: false, columnsComplete: true).supportsRowSelection(witness: witness))

    let windowWitness = windowRootTestWitness()
    guard case .windowRootV1(let windowContext) = windowWitness else {
        throw DeviceTestFailure(check: offset + passed + 1)
    }
    let rootBinding = windowRootTestFingerprint()
    let rootFrame = windowContext.root.frame
    let rootScope = NativeBindingProofScope.windowRoot(tableScope: windowRootTestScope())
    let rootFacts = NativeDeviceContextWindowRootFacts(app: windowContext.app,
        process: windowContext.source.process, window: windowContext.window, root: windowContext.root)
    try check(windowContext.valid && rootScope.valid)
    try check(!windowWitness.supportsNativeInput)
    try check(windowWitness.supportsNativeInput(action: "activate_window"))
    try check(!windowWitness.supportsNativeInput(action: "ax_press"))
    try check(!windowWitness.supportsNativeInput(action: "click"))
    try check(windowWitness.supportsAction("activate_window"))
    try check(!windowWitness.supportsAction("ax_set_selected_true"))
    try check(windowWitness.matchesBinding(rootBinding, displayFrame: rootFrame))
    try check(windowWitness.matchesScopes(menuScope: nil, focusScope: nil))
    try check(rootScope.supportsWindowActivation(witness: windowWitness))
    try check(windowContext.matchesSourceProvenance(sequence: 17, observedMS: 90))
    try check(!windowContext.matchesSourceProvenance(sequence: 18, observedMS: 90))
    try check(!windowContext.matchesSourceProvenance(sequence: 17, observedMS: 91))
    try check(!rootScope.supportsWindowActivation(witness: .wholeStructureV1(
        NativeDeviceContextWholeStructure(app: windowContext.app, active_layer: nil,
            window: windowContext.window, target: NativeDeviceContextTarget(object: windowContext.root,
                group: nil, scroll: nil), ancestry: [], sibling_parent_ancestry_index: nil,
            siblings: [windowContext.root], target_sibling_index: 0,
            nearest_labelled_group_ancestry_index: nil, scroll_ancestors: [], menu: nil, focus: nil))))
    try check(!rootScope.supportsWindowActivation(witness: witness))
    try check(!NativeBindingProofScope.windowRoot(tableScope: windowRootTestScope(headers: ["Name", " name "]))
        .supportsWindowActivation(witness: windowWitness))
    try check(!NativeBindingProofScope.wholeStructure.supportsWindowActivation(witness: windowWitness))
    try check(!rootScope.supportedForAtomicInput)
    try check(NativeBindingProofScope.wholeStructure.supportedForAtomicInput)
    try check(!rootScope.supportsRowSelection(witness: windowWitness))
    let wholeRootWitness = NativeDeviceContextWitness.wholeStructureV1(
        NativeDeviceContextWholeStructure(app: windowContext.app, active_layer: "window-42",
            window: windowContext.window, target: NativeDeviceContextTarget(object: windowContext.root,
                group: nil, scroll: nil), ancestry: [], sibling_parent_ancestry_index: nil,
            siblings: [windowContext.root], target_sibling_index: 0,
            nearest_labelled_group_ancestry_index: nil, scroll_ancestors: [], menu: nil, focus: nil))
    try check(wholeRootWitness.valid)
    try check(NativeBindingProofScope.wholeStructure.supportsWindowActivation(witness: wholeRootWitness))
    try check(!rootScope.supportsWindowActivation(witness: wholeRootWitness))
    try check(nativeWindowRootWitnessMatches(windowContext, actual: rootFacts,
        binding: rootBinding, displayFrame: rootFrame))

    let changedProcessFacts = NativeDeviceContextWindowRootFacts(app: rootFacts.app,
        process: NativeMenuProcessIdentity(pid: 42, start_seconds: 1_700_000_001, start_micros: 123_456),
        window: rootFacts.window, root: rootFacts.root)
    try check(!nativeWindowRootWitnessMatches(windowContext, actual: changedProcessFacts,
        binding: rootBinding, displayFrame: rootFrame))
    let movedRootFrame = rowContextTestRect(1, 0, 500, 400)
    let movedRoot = NativeDeviceContextObject(role: kAXWindowRole, subrole: kAXStandardWindowSubrole,
        label: "Records", frame: movedRootFrame)
    let movedWindow = NativeDeviceContextWindow(window_id: "window-42", title: "Records",
        frame: movedRootFrame, kind: .window, modal: false, owner_bundle_id: "com.example.rows")
    let movedFacts = NativeDeviceContextWindowRootFacts(app: rootFacts.app,
        process: rootFacts.process, window: movedWindow, root: movedRoot)
    try check(!nativeWindowRootWitnessMatches(windowContext, actual: movedFacts,
        binding: rootBinding, displayFrame: rootFrame))
    let badRoot = NativeDeviceContextObject(role: kAXWindowRole, subrole: "AXDialog",
        label: "Records", frame: rootFrame)
    try check(!nativeWindowRootWitnessMatches(windowContext, actual: NativeDeviceContextWindowRootFacts(
        app: rootFacts.app, process: rootFacts.process, window: rootFacts.window, root: badRoot),
        binding: rootBinding, displayFrame: rootFrame))
    let differentWindowID = NativeDeviceContextWindow(window_id: "window-43", title: "Records",
        frame: rootFrame, kind: .window, modal: false, owner_bundle_id: "com.example.rows")
    try check(!nativeWindowRootWitnessMatches(windowContext, actual: NativeDeviceContextWindowRootFacts(
        app: rootFacts.app, process: rootFacts.process, window: differentWindowID, root: rootFacts.root),
        binding: rootBinding, displayFrame: rootFrame))
    let modalWindow = NativeDeviceContextWindow(window_id: "window-42", title: "Records",
        frame: rootFrame, kind: .window, modal: true, owner_bundle_id: "com.example.rows")
    try check(!nativeWindowRootWitnessMatches(windowContext, actual: NativeDeviceContextWindowRootFacts(
        app: rootFacts.app, process: rootFacts.process, window: modalWindow, root: rootFacts.root),
        binding: rootBinding, displayFrame: rootFrame))
    var badRootBinding = rootBinding
    badRootBinding.ancestor_roles = [kAXWindowRole]
    try check(!windowWitness.matchesBinding(badRootBinding, displayFrame: rootFrame))

    let rootScopeData = try JSONEncoder().encode(rootScope)
    try check(try JSONDecoder().decode(NativeBindingProofScope.self, from: rootScopeData) == rootScope)
    guard var rootScopeWire = try JSONSerialization.jsonObject(with: rootScopeData) as? [String: Any],
          let encodedTableScope = rootScopeWire["table_scope"] as? [String: Any] else {
        throw DeviceTestFailure(check: offset + passed + 1)
    }
    let rootContinuity: [String: Any] = ["kind": "read_scoped", "source_sequence": UInt64(17),
        "context_digest": "bc1:" + String(repeating: "a", count: 64), "scope": rootScopeWire]
    try check(strictBindingContinuityFields(rootContinuity))
    rootScopeWire["extra"] = true
    try check(!strictBindingContinuityFields(["kind": "read_scoped", "source_sequence": UInt64(17),
        "context_digest": "bc1:" + String(repeating: "a", count: 64), "scope": rootScopeWire]))
    var invalidScopeWire = encodedTableScope
    invalidScopeWire["selector"] = "hidden-row-condition"
    try check(!strictNativeRowTableScopeFields(invalidScopeWire))
    try check(!windowRootTestScope(headers: ["Name", " name "]).valid)
    try check(!windowRootTestScope(path: [NativeDeviceContextRowContainerScope(
        role: "ax_group", label: "Table content")]).valid)

    let windowWitnessData = try JSONEncoder().encode(windowWitness)
    guard var windowWitnessWire = try JSONSerialization.jsonObject(with: windowWitnessData) as? [String: Any],
          let sourceWire = windowWitnessWire["source"] as? [String: Any],
          let processWire = sourceWire["process"] as? [String: Any] else {
        throw DeviceTestFailure(check: offset + passed + 1)
    }
    try check(strictNativeDeviceContextWitnessFields(windowWitnessWire))
    try check(try JSONDecoder().decode(NativeDeviceContextWitness.self, from: windowWitnessData) == windowWitness)
    windowWitnessWire["table_scope"] = try JSONSerialization.jsonObject(with: rootScopeData)
    try check(!strictNativeDeviceContextWitnessFields(windowWitnessWire))
    windowWitnessWire.removeValue(forKey: "table_scope")
    windowWitnessWire["extra"] = "unregistered"
    try check(!strictNativeDeviceContextWitnessFields(windowWitnessWire))
    windowWitnessWire.removeValue(forKey: "extra")
    var badSource = sourceWire
    badSource.removeValue(forKey: "verified_source_sequence")
    windowWitnessWire["source"] = badSource
    try check(!strictNativeDeviceContextWitnessFields(windowWitnessWire))
    var badProcess = processWire
    badProcess["extra"] = 1
    badSource = sourceWire
    badSource["process"] = badProcess
    windowWitnessWire["source"] = badSource
    try check(!strictNativeDeviceContextWitnessFields(windowWitnessWire))
    var wrongRootWire = try JSONSerialization.jsonObject(with: windowWitnessData) as! [String: Any]
    var wrongRoot = wrongRootWire["root"] as! [String: Any]
    wrongRoot["subrole"] = "AXDialog"
    wrongRootWire["root"] = wrongRoot
    try check(!strictNativeDeviceContextWitnessFields(wrongRootWire))
    var wrongProcessWire = try JSONSerialization.jsonObject(with: windowWitnessData) as! [String: Any]
    var wrongSourceWire = wrongProcessWire["source"] as! [String: Any]
    var wrongProcess = wrongSourceWire["process"] as! [String: Any]
    wrongProcess["pid"] = 43
    wrongSourceWire["process"] = wrongProcess
    wrongProcessWire["source"] = wrongSourceWire
    try check(!strictNativeDeviceContextWitnessFields(wrongProcessWire))
    try check(!windowRootTestWitness(sourceSessionID: "").valid)
    try check(!windowRootTestWitness(sourceEpoch: 0).valid)
    try check(!windowRootTestWitness(sourceSequence: 0).valid)
    try check(!windowRootTestWitness(sourceProcess: NativeMenuProcessIdentity(
        pid: 42, start_seconds: 0, start_micros: 123_456)).valid)
    try check(!windowRootTestWitness(sourceProcess: NativeMenuProcessIdentity(
        pid: 43, start_seconds: 1_700_000_000, start_micros: 123_456)).valid)
    try check(!windowRootTestWitness(title: nil).valid)
    try check(!windowRootTestWitness(modal: true).valid)
    try check(!windowRootTestWitness(ownerBundle: "com.example.other").valid)
    try check(!windowRootTestWitness(rootRole: "AXDialog").valid)
    try check(!windowRootTestWitness(rootSubrole: "AXDialog").valid)
    try check(!windowRootTestWitness(title: "Records", rootLabel: "Other").valid)

    var authorityLedger = NativeAuthorityLedger()
    try check(authorityLedger.consumeRowSelectionRead(10))
    try check(!authorityLedger.consumeRowSelectionRead(10))
    try check(authorityLedger.consumeRowSelectionRead(11))
    try check(!authorityLedger.consumeRowSelectionRead(0))

    let encoded = try JSONEncoder().encode(witness)
    let wire = try JSONSerialization.jsonObject(with: encoded)
    try check(strictNativeDeviceContextWitnessFields(wire))
    if var raw = wire as? [String: Any] {
        raw["table_ax_enabled"] = "enabled-ish"
        try check(!strictNativeDeviceContextWitnessFields(raw))
        raw["table_ax_enabled"] = "enabled"
        raw["target_ax_enabled"] = "unavailable"
        try check(strictNativeDeviceContextWitnessFields(raw))
        if JSONSerialization.isValidJSONObject(raw),
           let data = try? JSONSerialization.data(withJSONObject: raw),
           let decoded = try? JSONDecoder().decode(NativeDeviceContextWitness.self, from: data) {
            try check(!decoded.valid)
        } else {
            try check(false)
        }
        raw.removeValue(forKey: "table_ax_enabled")
        try check(!strictNativeDeviceContextWitnessFields(raw))
    } else {
        try check(false)
        try check(false)
        try check(false)
        try check(false)
    }
    let roundTrip = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: encoded)
    try check(roundTrip == witness && roundTrip.valid)
    if var raw = wire as? [String: Any] {
        raw["unknown_input"] = true
        try check(!strictNativeDeviceContextWitnessFields(raw))
        raw.removeValue(forKey: "unknown_input")
        raw.removeValue(forKey: "hit_point")
        try check(!strictNativeDeviceContextWitnessFields(raw))
    } else {
        try check(false)
        try check(false)
    }

    let calledTrace = NativeRowTrace.called(error: .success, started: 100, finished: 125)
    var rowResponse = NativeSubmitResponse(session_epoch: 7, request_id: "row-trace",
        outcome: "applied", reason: "effect unverified")
    rowResponse.submitted = true
    rowResponse.row_trace = calledTrace
    let rowResponseData = try JSONEncoder().encode(rowResponse)
    let rowResponseWire = try JSONSerialization.jsonObject(with: rowResponseData) as? [String: Any]
    let rowTraceWire = rowResponseWire?["row_trace"] as? [String: Any]
    try check(rowResponseWire?["kind"] as? String == "atomic_submit")
    try check(rowTraceWire?.keys.sorted() == ["ax_call_count", "ax_error_code",
        "call_finished_uptime_ns", "call_started_uptime_ns", "clock_domain", "primitive"])
    try check(rowTraceWire?["primitive"] as? String == "ax_set_selected_true")
    try check(rowTraceWire?["clock_domain"] as? String == "dispatch_time_uptime")
    try check((rowTraceWire?["ax_call_count"] as? NSNumber)?.uint8Value == 1)
    try check((rowTraceWire?["ax_error_code"] as? NSNumber)?.int32Value == AXError.success.rawValue)
    try check(try JSONDecoder().decode(NativeRowTrace.self, from: rowResponseDataForTrace(calledTrace))
        == calledTrace)

    let zeroTrace = NativeRowTrace.notCalled()
    let zeroTraceData = try JSONEncoder().encode(zeroTrace)
    let zeroTraceWire = try JSONSerialization.jsonObject(with: zeroTraceData) as? [String: Any]
    try check(zeroTraceWire?.keys.sorted() == ["ax_call_count", "clock_domain", "primitive"])
    try check((zeroTraceWire?["ax_call_count"] as? NSNumber)?.uint8Value == 0)
    var nonRowResponse = NativeSubmitResponse(session_epoch: 7, request_id: "other-action",
        outcome: "rejected", reason: "no input")
    nonRowResponse.row_trace = nil
    let nonRowData = try JSONEncoder().encode(nonRowResponse)
    let nonRowWire = try JSONSerialization.jsonObject(with: nonRowData) as? [String: Any]
    try check(nonRowWire?["row_trace"] == nil)

    let failedPressTrace = NativeAXTrace.called(error: .cannotComplete, started: 200, finished: 240)
    try check(failedPressTrace.valid)
    var pressResponse = NativeSubmitResponse(session_epoch: 7, request_id: "ax-press-trace",
        outcome: "unknown", reason: "AXPress returned an error; do not replay")
    pressResponse.submitted = true
    pressResponse.ax_trace = failedPressTrace
    let pressData = try JSONEncoder().encode(pressResponse)
    let pressWire = try JSONSerialization.jsonObject(with: pressData) as? [String: Any]
    let pressTraceWire = pressWire?["ax_trace"] as? [String: Any]
    try check(pressTraceWire?.keys.sorted() == ["ax_call_count", "ax_error_code",
        "call_finished_uptime_ns", "call_started_uptime_ns", "clock_domain", "primitive"])
    try check(pressTraceWire?["primitive"] as? String == "ax_press")
    try check(pressTraceWire?["clock_domain"] as? String == "dispatch_time_uptime")
    try check((pressTraceWire?["ax_call_count"] as? NSNumber)?.uint8Value == 1)
    try check((pressTraceWire?["ax_error_code"] as? NSNumber)?.int32Value == AXError.cannotComplete.rawValue)
    try check(try JSONDecoder().decode(NativeAXTrace.self, from: JSONEncoder().encode(failedPressTrace))
        == failedPressTrace)

    let zeroPressTrace = NativeAXTrace.notCalled()
    try check(zeroPressTrace.valid)
    let zeroPressData = try JSONEncoder().encode(zeroPressTrace)
    let zeroPressWire = try JSONSerialization.jsonObject(with: zeroPressData) as? [String: Any]
    try check(zeroPressWire?.keys.sorted() == ["ax_call_count", "clock_domain", "primitive"])
    try check((zeroPressWire?["ax_call_count"] as? NSNumber)?.uint8Value == 0)
    try check(zeroPressWire?["ax_error_code"] == nil)
    try check(try JSONDecoder().decode(NativeAXTrace.self, from: zeroPressData) == zeroPressTrace)

    var noPressTraceResponse = NativeSubmitResponse(session_epoch: 7, request_id: "unobserved",
        outcome: "context_unresolved", reason: "preflight stopped before commit gate")
    noPressTraceResponse.ax_trace = nil
    let noPressData = try JSONEncoder().encode(noPressTraceResponse)
    let noPressWire = try JSONSerialization.jsonObject(with: noPressData) as? [String: Any]
    try check(noPressWire?["ax_trace"] == nil)

    try check(!rowContextTestWitness(targetSelected: true).valid)
    try check(!rowContextTestWitness(targetSettable: false).valid)
    try check(!rowContextTestWitness(tableAXEnabled: .disabled).tableViewport!.valid)
    try check(!rowContextTestWitness(targetAXEnabled: .disabled).tableViewport!.valid)
    try check(!rowContextTestWitness(tableAXEnabled: .unavailable).tableViewport!.valid)
    try check(!rowContextTestWitness(targetAXEnabled: .unavailable).tableViewport!.valid)
    try check(!rowContextTestWitness(targetPoint: NativeDeviceContextPoint(x: 300, y: 25)).valid)
    try check(!rowContextTestWitness(targetPoint: NativeDeviceContextPoint(x: 1, y: 200)).valid)
    try check(!rowContextTestWitness(targetAXIndex: 0).valid)
    let duplicateVisible = [
        context.visible_rows[0],
        NativeDeviceContextVisibleRow(ax_index: 42, frame: context.visible_rows[1].frame,
            selector_values: [.text("beta")]),
    ]
    try check(!rowContextTestWitness(visibleRows: duplicateVisible).valid)
    let duplicateMatch = [context.visible_rows[0],
        NativeDeviceContextVisibleRow(ax_index: 511, frame: context.visible_rows[1].frame,
            selector_values: [.text("alpha")])]
    try check(!rowContextTestWitness(visibleRows: duplicateMatch).valid)
    try check(!rowContextTestWitness(targetRole: kAXCellRole).valid)
    try check(!rowContextTestWitness(targetScroll: NativeDeviceContextScrollState(horizontal: nil,
        vertical: NativeDeviceContextScrollAxis(value: 1, min: 0, max: 2))).valid)
    try check(!rowContextTestWitness(columnValues: [
        context.ordered_columns[0],
        NativeDeviceContextRowColumn(ax_index: 0, header: "Owner", frame: context.ordered_columns[1].frame),
    ]).valid)
    try check(!rowContextTestWitness(selectorColumns: [NativeDeviceContextRowSelectorColumn(
        ax_index: 0, header: "Name", expected_value: .text(" "))]).valid)
    try check(!rowContextTestWitness(selectorColumns: [NativeDeviceContextRowSelectorColumn(
        ax_index: 1, header: "Missing", expected_value: .text("alpha"))]).valid)
    try check(!rowContextTestWitness(visibleRows: [NativeDeviceContextVisibleRow(ax_index: 42,
        frame: context.visible_rows[0].frame, selector_values: [.number(.infinity)]),
        context.visible_rows[1]]).valid)
    try check(!rowContextTestWitness(scrollLabel: "").valid)
    let outsideColumn = [context.ordered_columns[0], NativeDeviceContextRowColumn(
        ax_index: 1, header: "Owner", frame: rowContextTestRect(310, 0, 150, 20))]
    try check(!rowContextTestWitness(columnValues: outsideColumn).valid)

    return passed
}

private func rowResponseDataForTrace(_ trace: NativeRowTrace) throws -> Data {
    try JSONEncoder().encode(trace)
}
