import CoreGraphics
import ApplicationServices
import Foundation

private func tableColumnTestObject(_ role: String, _ label: String, _ frame: HitRect,
                                   subrole: String? = nil) -> NativeDeviceContextObject {
    NativeDeviceContextObject(role: role, subrole: subrole, label: label, frame: frame)
}

private func tableColumnTestWitness(
    tableColumn: NativeDeviceContextTableColumn?,
    ancestryRoles: [String] = [kAXWindowRole, kAXTableRole, kAXGroupRole],
    targetRole: String = kAXButtonRole,
    targetSubrole: String? = "AXSortButton",
    menu: NativeDeviceContextMenuRelation? = nil,
    focus: NativeDeviceContextFocus? = nil
) -> NativeDeviceContextWitness {
    let windowFrame = HitRect(CGRect(x: 0, y: 0, width: 1000, height: 800))
    let tableFrame = HitRect(CGRect(x: 40, y: 100, width: 700, height: 500))
    let headerGroupFrame = HitRect(CGRect(x: 40, y: 55, width: 700, height: 40))
    let targetFrame = HitRect(CGRect(x: 80, y: 60, width: 160, height: 24))
    let ancestry = ancestryRoles.enumerated().map { index, role in
        switch role {
        case kAXWindowRole:
            tableColumnTestObject(role, "document", windowFrame, subrole: kAXStandardWindowSubrole)
        case kAXTableRole, "AXOutline":
            tableColumnTestObject(role, "table", tableFrame)
        case kAXGroupRole:
            tableColumnTestObject(role, "headers", headerGroupFrame)
        default:
            tableColumnTestObject(role, "ancestor \(index)", headerGroupFrame)
        }
    }
    let targetObject = tableColumnTestObject(targetRole, "name", targetFrame, subrole: targetSubrole)
    let bundleID = "com.example.table"
    let windowID = "window-42"
    let app = NativeDeviceContextApp(bundle_id: bundleID, pid: 42)
    let window = NativeDeviceContextWindow(window_id: windowID, title: "document",
        frame: windowFrame, kind: .window, modal: false, owner_bundle_id: bundleID)
    let target = NativeDeviceContextTarget(object: targetObject,
        group: ancestry.last?.role == kAXGroupRole ? "headers" : nil, scroll: nil)
    let siblings = [targetObject]
    let parentIndex = ancestry.isEmpty ? nil : UInt8(ancestry.count - 1)
    let groupIndex = ancestry.indices.last(where: { ancestry[$0].role == kAXGroupRole }).map { UInt8($0) }
    return .wholeStructureV1(NativeDeviceContextWholeStructure(app: app, active_layer: nil,
        window: window, target: target, ancestry: ancestry,
        sibling_parent_ancestry_index: parentIndex, siblings: siblings, target_sibling_index: 0,
        nearest_labelled_group_ancestry_index: groupIndex, scroll_ancestors: [], menu: menu,
        focus: focus, table_column: tableColumn))
}

private func tableColumnTestWire(_ witness: NativeDeviceContextWitness) throws -> [String: Any] {
    let data = try JSONEncoder().encode(witness)
    guard let wire = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw DeviceTestFailure(check: 1)
    }
    return wire
}

/// Pure wire and value-graph tests. Actual AX ref equality stays in the native
/// worker path and is covered by separately owned live diagnostics.
func runTableColumnContextTests(startingAt offset: Int) throws -> Int {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw DeviceTestFailure(check: offset + passed + 1) }
        passed += 1
    }

    let small = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: [0, 1, 2], target_ax_index: 1)
    let witness = tableColumnTestWitness(tableColumn: small)
    try check(small.valid && witness.valid)
    try check(witness.supportsAction("ax_press") && !witness.supportsAction("click")
        && !witness.supportsAction("pointer_move") && !witness.supportsAction("scroll")
        && !witness.supportsAction("key_press"))
    let wire = try tableColumnTestWire(witness)
    try check(strictNativeDeviceContextWitnessFields(wire))
    let roundTrip = try JSONDecoder().decode(NativeDeviceContextWitness.self,
        from: JSONSerialization.data(withJSONObject: wire))
    try check(roundTrip == witness && roundTrip.valid && roundTrip.supportsNativeInput)

    let allColumns = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: (0..<64).map { UInt8($0) }, target_ax_index: 42)
    let maximumWitness = tableColumnTestWitness(tableColumn: allColumns)
    let maximumWire = try tableColumnTestWire(maximumWitness)
    let maximumData = try JSONSerialization.data(withJSONObject: maximumWire)
    try check(allColumns.valid && maximumWitness.valid && maximumData.count <= 4096
        && strictNativeDeviceContextWitnessFields(maximumWire))

    let duplicate = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: [0, 1, 1], target_ax_index: 1)
    let duplicateWire = try tableColumnTestWire(tableColumnTestWitness(tableColumn: duplicate))
    try check(!duplicate.valid && !strictNativeDeviceContextWitnessFields(duplicateWire))
    let missingTarget = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: [0, 1], target_ax_index: 2)
    try check(!tableColumnTestWitness(tableColumn: missingTarget).valid)
    let outOfRange = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: [0, 64], target_ax_index: 0)
    try check(!tableColumnTestWitness(tableColumn: outOfRange).valid)
    let tooMany = NativeDeviceContextTableColumn(table_ancestry_index: 1,
        ordered_ax_indices: (0...64).map { UInt8($0) }, target_ax_index: 0)
    try check(!tableColumnTestWitness(tableColumn: tooMany).valid)
    let badAncestry = NativeDeviceContextTableColumn(table_ancestry_index: 0,
        ordered_ax_indices: [0, 1, 2], target_ax_index: 1)
    try check(!tableColumnTestWitness(tableColumn: badAncestry).valid)
    try check(!tableColumnTestWitness(tableColumn: small,
        ancestryRoles: [kAXWindowRole, "AXOutline", kAXTableRole, kAXGroupRole]).valid)

    let noTable = tableColumnTestWitness(tableColumn: nil,
        ancestryRoles: [kAXWindowRole, kAXGroupRole], targetRole: kAXButtonRole, targetSubrole: nil)
    try check(noTable.valid)
    let tableWithoutRelation = tableColumnTestWitness(tableColumn: nil)
    let sortButtonWithoutRelation = tableColumnTestWitness(tableColumn: nil,
        ancestryRoles: [kAXWindowRole, kAXGroupRole])
    try check(!tableWithoutRelation.valid && !sortButtonWithoutRelation.valid)
    for role in [kAXColumnRole, kAXRowRole, kAXCellRole] {
        let orphan = tableColumnTestWitness(tableColumn: nil,
            ancestryRoles: [kAXWindowRole, kAXGroupRole], targetRole: role, targetSubrole: nil)
        let orphanWire = try tableColumnTestWire(orphan)
        let fingerprint = NativeBindingFingerprint(app: "com.example.table", app_pid: 42,
            window_id: "window-42", ancestor_roles: [kAXWindowRole, kAXGroupRole],
            group: "headers", role: role, subrole: nil, label: "name", sibling_index: 0,
            frame: [80, 60, 160, 24], scroll_container: nil)
        let request = HitRequest(request_id: "read-only-row", pid: 42,
            bundle_id: "com.example.table", window_id: 42, x: 100, y: 70,
            frame: HitRect(CGRect(x: 80, y: 60, width: 160, height: 24)),
            binding: fingerprint, binding_digest: fingerprint.digest, contextWitness: orphan)
        // These observations support destination visibility; their roles grant no input.
        try check(orphan.valid && strictNativeDeviceContextWitnessFields(orphanWire)
            && !request.validFor(action: "ax_press") && !request.validFor(action: "click")
            && !request.validFor(action: "scroll"))
    }

    let menuFingerprint = NativeDeviceContextFingerprint(app: "com.example.table", app_pid: 42,
        window_id: "window-42", ancestor_roles: [kAXWindowRole], group: nil,
        role: kAXMenuButtonRole, subrole: nil, label: "options", sibling_index: 1,
        frame: [10, 10, 20, 20])
    let menu = NativeDeviceContextMenuRelation.openerAnchor(originWindowID: "window-42",
        originWindowFrame: HitRect(CGRect(x: 0, y: 0, width: 1000, height: 800)),
        openerFingerprint: menuFingerprint, continuity: .directRead)
    try check(!tableColumnTestWitness(tableColumn: small, menu: menu).valid)
    let focusFingerprint = NativeDeviceContextFingerprint(app: "com.example.table", app_pid: 42,
        window_id: "window-42", ancestor_roles: [kAXWindowRole, kAXTableRole, kAXGroupRole],
        group: "headers", role: kAXButtonRole, subrole: "AXSortButton", label: "name",
        sibling_index: 0, frame: [80, 60, 160, 24])
    let focus = NativeDeviceContextFocus(origin_window_id: "window-42",
        origin_window_frame: HitRect(CGRect(x: 0, y: 0, width: 1000, height: 800)),
        target_fingerprint: focusFingerprint,
        target_frame: HitRect(CGRect(x: 80, y: 60, width: 160, height: 24)),
        relation: .exactControl, composition: .notApplicable)
    try check(!tableColumnTestWitness(tableColumn: small, focus: focus).valid)

    var missingColumnKey = wire
    missingColumnKey.removeValue(forKey: "table_column")
    try check(!strictNativeDeviceContextWitnessFields(missingColumnKey))
    var unknownColumnKey = wire
    if var table = unknownColumnKey["table_column"] as? [String: Any] {
        table["column_position"] = 1
        unknownColumnKey["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(unknownColumnKey))
    var fractionalIndex = wire
    if var table = fractionalIndex["table_column"] as? [String: Any] {
        table["ordered_ax_indices"] = [0, 1.5, 2]
        fractionalIndex["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(fractionalIndex))
    var booleanIndex = wire
    if var table = booleanIndex["table_column"] as? [String: Any] {
        table["ordered_ax_indices"] = [0, true, 2]
        booleanIndex["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(booleanIndex))
    var stringIndex = wire
    if var table = stringIndex["table_column"] as? [String: Any] {
        table["ordered_ax_indices"] = [0, "1", 2]
        stringIndex["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(stringIndex))
    var nanIndex = wire
    if var table = nanIndex["table_column"] as? [String: Any] {
        table["ordered_ax_indices"] = [0, NSNumber(value: Double.nan), 2]
        nanIndex["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(nanIndex))
    var overflowIndex = wire
    if var table = overflowIndex["table_column"] as? [String: Any] {
        table["ordered_ax_indices"] = [0, NSNumber(value: Double.greatestFiniteMagnitude), 2]
        overflowIndex["table_column"] = table
    }
    try check(!strictNativeDeviceContextWitnessFields(overflowIndex))

    var oversized = maximumWire
    let largeSibling: [String: Any] = ["role": kAXButtonRole, "subrole": NSNull(),
        "label": String(repeating: "x", count: 512),
        "frame": ["x": 1, "y": 1, "width": 10, "height": 10]]
    let targetObject = (maximumWire["target"] as! [String: Any])["object"]!
    oversized["siblings"] = [targetObject] + Array(repeating: largeSibling as Any, count: 12)
    let oversizedData = try JSONSerialization.data(withJSONObject: oversized)
    try check(oversizedData.count > 4096 && !strictNativeDeviceContextWitnessFields(oversized))
    let oversizedTyped = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: oversizedData)
    try check(!oversizedTyped.valid)

    let unsupported = NativeDeviceContextWitness.unsupported(.tableViewport)
    try check(unsupported.valid && !unsupported.supportsNativeInput)
    return passed
}
