import CoreGraphics
import ApplicationServices
import Foundation

struct DeviceTestResult: Encodable { let kind = "self_test"; let passed: Int }
struct DeviceTestFailure: Error {
    let check: Int
    let file: String
    let line: UInt

    init(check: Int, file: String = #fileID, line: UInt = #line) {
        self.check = check
        self.file = file
        self.line = line
    }
}

private func optionalAXTextReadStatusTestsPass() -> Bool {
    let empty: CFTypeRef = "" as CFString
    let bounded: CFTypeRef = "title" as CFString
    let wrongType: CFTypeRef = NSNumber(value: 1)
    let oversized: CFTypeRef = String(repeating: "a", count: 513) as CFString
    return optionalAXTextReadStatus(error: .success, value: empty, timely: true) == .value("")
        && optionalAXTextReadStatus(error: .success, value: bounded, timely: true) == .value("title")
        && optionalAXTextReadStatus(error: .noValue, value: nil, timely: true) == .absent
        && optionalAXTextReadStatus(error: .attributeUnsupported, value: nil, timely: true) == .absent
        && optionalAXTextReadStatus(error: .failure, value: nil, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .cannotComplete, value: nil, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .noValue, value: bounded, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .success, value: nil, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .success, value: wrongType, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .success, value: oversized, timely: true) == .unavailable
        && optionalAXTextReadStatus(error: .noValue, value: nil, timely: false) == .unavailable
    }

private func deviceContextFingerprint(_ fingerprint: NativeBindingFingerprint) -> NativeDeviceContextFingerprint {
    NativeDeviceContextFingerprint(app: fingerprint.app, app_pid: fingerprint.app_pid,
        window_id: fingerprint.window_id, ancestor_roles: fingerprint.ancestor_roles,
        group: fingerprint.group, role: fingerprint.role, subrole: fingerprint.subrole,
        label: fingerprint.label, sibling_index: fingerprint.sibling_index, frame: fingerprint.frame)
}

private func deviceContextMenuRelation(_ scope: NativeMenuBindingScope) throws -> NativeDeviceContextMenuRelation {
    switch scope {
    case .opener(let anchor, _):
        guard let continuity = NativeDeviceContextMenuContinuity(rawValue: anchor.continuity) else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Unknown menu continuity"))
        }
        return .openerAnchor(originWindowID: anchor.origin_window_id,
            originWindowFrame: anchor.origin_window_frame,
            openerFingerprint: deviceContextFingerprint(anchor.opener_fingerprint), continuity: continuity)
    case .item(let context, _):
        guard let continuity = NativeDeviceContextMenuContinuity(rawValue: context.opener.continuity) else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Unknown menu continuity"))
        }
        let drawing = NativeDeviceContextDrawingWindow(window_id: context.drawing_window.window_id,
            owner_pid: context.drawing_window.owner_pid, layer: context.drawing_window.layer,
            frame: context.drawing_window.frame)
        return .anchoredMenu(originWindowID: context.opener.origin_window_id,
            originWindowFrame: context.opener.origin_window_frame,
            openerFingerprint: deviceContextFingerprint(context.opener.opener_fingerprint),
            continuity: continuity, menuFingerprint: deviceContextFingerprint(context.menu_fingerprint),
            menuFrame: context.menu_frame, drawingWindow: drawing)
    }
}

private func deviceContextFocusRelation(_ scope: NativeFocusedControlScope) throws -> NativeDeviceContextFocus {
    guard let relation = NativeDeviceContextFocusRelationName(rawValue: scope.context.relation),
          let composition = NativeDeviceContextComposition(rawValue: scope.context.composition.kind) else {
        throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Unknown focus witness facts"))
    }
    return NativeDeviceContextFocus(origin_window_id: scope.context.origin_window_id,
        origin_window_frame: scope.context.origin_window_frame,
        target_fingerprint: deviceContextFingerprint(scope.context.target_fingerprint),
        target_frame: scope.context.target_frame, relation: relation, composition: composition)
}

private func checkDeviceContextWitness(_ condition: Bool, passed: inout Int, offset: Int, sourceLine: UInt = #line) throws {
    guard condition else {
        FileHandle.standardError.write(Data("device self-test failed at SelfTest.swift:\(sourceLine)\n".utf8))
        throw DeviceTestFailure(check: offset + passed + 1)
    }
    passed += 1
}

func deviceContextWitnessJSON(fingerprint: NativeBindingFingerprint,
                              displayFrame: HitRect,
                              windowFrame: HitRect? = nil,
                              targetScroll: NativeDeviceContextScrollState? = nil,
                              menu: NativeDeviceContextMenuRelation? = nil,
                              focus: NativeDeviceContextFocus? = nil) throws -> [String: Any] {
    let actualWindowFrame = windowFrame ?? displayFrame
    let menuNodeFrame: HitRect?
    if case .some(.anchoredMenu(_, _, _, _, _, let frame, _)) = menu {
        menuNodeFrame = frame
    } else {
        menuNodeFrame = nil
    }
    let groupIndex = fingerprint.group.flatMap { _ in
        fingerprint.ancestor_roles.indices.reversed().first {
            fingerprint.ancestor_roles[$0] == kAXGroupRole || fingerprint.ancestor_roles[$0].hasSuffix("Group")
        }
    }
    let ancestry = fingerprint.ancestor_roles.enumerated().map { index, role in
        let frame: HitRect
        if index == 0 && [kAXWindowRole, kAXSheetRole].contains(role) {
            frame = actualWindowFrame
        } else if role == kAXMenuRole, let menuNodeFrame {
            frame = menuNodeFrame
        } else {
            frame = displayFrame
        }
        return NativeDeviceContextObject(role: role, subrole: nil,
            label: groupIndex == index ? (fingerprint.group ?? "") : "", frame: frame)
    }
    let target = NativeDeviceContextObject(role: fingerprint.role, subrole: fingerprint.subrole,
        label: fingerprint.label, frame: displayFrame)
    let targetIndex = Int(fingerprint.sibling_index)
    let siblings = (0...targetIndex).map { index in
        index == targetIndex ? target : NativeDeviceContextObject(
            role: fingerprint.role, subrole: nil, label: "sibling-\(index)", frame: displayFrame)
    }
    let scrollAncestors = ancestry.enumerated().compactMap { index, object in
        object.role == kAXScrollAreaRole
            ? NativeDeviceContextScrollAncestor(ancestry_index: UInt8(index), state: nil) : nil
    }
    let isSheet = fingerprint.ancestor_roles.first == kAXSheetRole
    let whole = NativeDeviceContextWholeStructure(
        app: NativeDeviceContextApp(bundle_id: fingerprint.app, pid: fingerprint.app_pid),
        active_layer: fingerprint.window_id,
        window: NativeDeviceContextWindow(window_id: fingerprint.window_id, title: nil,
            frame: actualWindowFrame, kind: isSheet ? .sheet : .window, modal: isSheet,
            owner_bundle_id: fingerprint.app),
        target: NativeDeviceContextTarget(object: target, group: fingerprint.group, scroll: targetScroll),
        ancestry: ancestry,
        sibling_parent_ancestry_index: ancestry.isEmpty ? nil : UInt8(ancestry.count - 1),
        siblings: siblings, target_sibling_index: UInt16(fingerprint.sibling_index),
        nearest_labelled_group_ancestry_index: groupIndex.map { UInt8($0) },
        scroll_ancestors: scrollAncestors, menu: menu, focus: focus)
    let data = try JSONEncoder().encode(NativeDeviceContextWitness.wholeStructureV1(whole))
    guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw DecodingError.typeMismatch([String: Any].self,
            .init(codingPath: [], debugDescription: "Device context witness did not encode as an object"))
    }
    return object
}

private func strictDeviceContextRoundTrip(_ wire: [String: Any]) throws -> Bool {
    guard strictNativeDeviceContextWitnessFields(wire) else { return false }
    let data = try JSONSerialization.data(withJSONObject: wire)
    let decoded = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: data)
    return decoded.valid && decoded.supportsNativeInput
}

func runDeviceContextWitnessTests(fingerprint: NativeBindingFingerprint,
                                  displayFrame: HitRect,
                                  startingAt offset: Int) throws -> Int {
    var passed = 0
    let wire = try deviceContextWitnessJSON(fingerprint: fingerprint, displayFrame: displayFrame)
    let data = try JSONSerialization.data(withJSONObject: wire)
    let decoded = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: data)
    try checkDeviceContextWitness(decoded.valid && decoded.supportsNativeInput
        && decoded.matchesBinding(fingerprint, displayFrame: displayFrame), passed: &passed, offset: offset)
    try checkDeviceContextWitness(try strictDeviceContextRoundTrip(wire), passed: &passed, offset: offset)

    let subpixelShift = HitRect(CGRect(x: displayFrame.x + 0.25, y: displayFrame.y,
        width: displayFrame.width, height: displayFrame.height))
    try checkDeviceContextWitness(!decoded.matchesBinding(fingerprint, displayFrame: subpixelShift),
        passed: &passed, offset: offset)

    var malformed = wire
    malformed["unexpected"] = true
    try checkDeviceContextWitness(!strictNativeDeviceContextWitnessFields(malformed),
        passed: &passed, offset: offset)
    var missingTitle = wire
    if var window = missingTitle["window"] as? [String: Any] {
        window.removeValue(forKey: "title")
        missingTitle["window"] = window
    }
    try checkDeviceContextWitness(!strictNativeDeviceContextWitnessFields(missingTitle),
        passed: &passed, offset: offset)

    var nullableFingerprint = fingerprint
    nullableFingerprint.ancestor_roles = [kAXWindowRole, kAXScrollAreaRole]
    nullableFingerprint.group = nil
    nullableFingerprint.role = kAXButtonRole
    let nullableWire = try deviceContextWitnessJSON(fingerprint: nullableFingerprint, displayFrame: displayFrame)
    try checkDeviceContextWitness(try strictDeviceContextRoundTrip(nullableWire),
        passed: &passed, offset: offset)

    let axis = NativeDeviceContextScrollAxis(value: 0.5, min: nil, max: nil)
    let scroll = NativeDeviceContextScrollState(horizontal: axis, vertical: nil)
    let scrollWire = try deviceContextWitnessJSON(fingerprint: nullableFingerprint,
        displayFrame: displayFrame, targetScroll: scroll)
    try checkDeviceContextWitness(try strictDeviceContextRoundTrip(scrollWire),
        passed: &passed, offset: offset)

    let nestedFingerprint = NativeDeviceContextFingerprint(app: fingerprint.app,
        app_pid: fingerprint.app_pid, window_id: fingerprint.window_id,
        ancestor_roles: [kAXWindowRole], group: nil, role: kAXMenuButtonRole,
        subrole: nil, label: "open menu", sibling_index: 0, frame: displayFrame.rounded)
    let menu = NativeDeviceContextMenuRelation.openerAnchor(originWindowID: fingerprint.window_id,
        originWindowFrame: displayFrame, openerFingerprint: nestedFingerprint, continuity: .directRead)
    let focus = NativeDeviceContextFocus(origin_window_id: fingerprint.window_id,
        origin_window_frame: displayFrame, target_fingerprint: nestedFingerprint,
        target_frame: displayFrame, relation: .exactControl, composition: .notApplicable)
    let relatedWire = try deviceContextWitnessJSON(fingerprint: nullableFingerprint,
        displayFrame: displayFrame, menu: menu, focus: focus)
    try checkDeviceContextWitness(try strictDeviceContextRoundTrip(relatedWire),
        passed: &passed, offset: offset)

    let unsupportedData = try JSONEncoder().encode(NativeDeviceContextWitness.unsupported(.tableViewport))
    let unsupported = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: unsupportedData)
    try checkDeviceContextWitness(unsupported.valid && !unsupported.supportsNativeInput,
        passed: &passed, offset: offset)
    return passed
}

private func runMenuPathBoundaryTests(startingAt offset: Int) throws -> Int {
    var passed = 0
    try checkDeviceContextWitness(menuPathRequiresTopLevel(kAXMenuRole), passed: &passed, offset: offset)
    try checkDeviceContextWitness(menuPathRequiresTopLevel(kAXMenuItemRole), passed: &passed, offset: offset)
    try checkDeviceContextWitness(!menuPathRequiresTopLevel(kAXMenuButtonRole), passed: &passed, offset: offset)
    try checkDeviceContextWitness(!menuPathRequiresTopLevel(kAXGroupRole), passed: &passed, offset: offset)
    try checkDeviceContextWitness(!menuPathRequiresTopLevel(kAXWindowRole), passed: &passed, offset: offset)
    try checkDeviceContextWitness(!menuPathRequiresTopLevel("AXUnknown"), passed: &passed, offset: offset)
    return passed
}

func runDeviceTests(goldenPath: String) throws -> DeviceTestResult {
    var passed = 0
    func check(_ condition: @autoclosure () throws -> Bool, sourceLine: UInt = #line) throws {
        guard try condition() else {
            FileHandle.standardError.write(Data("device self-test failed at SelfTest.swift:\(sourceLine)\n".utf8))
            throw DeviceTestFailure(check: passed + 1)
        }
        passed += 1
    }
    let expectedBirth = DeviceProcessStamp(seconds: 1_700_000_000, micros: 123_456)
    try check(NativeExecutionBirthStampArguments.parse([]) == .absent)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456"
    ]) == .expected(expectedBirth))
    try check(NativeExecutionBirthStampArguments.parseBackgroundAX([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456",
        "--execute-background-ax"
    ]) == expectedBirth)
    try check(NativeExecutionBirthStampArguments.parseBackgroundAX([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456"
    ]) == nil)
    try check(NativeExecutionBirthStampArguments.parseBackgroundAX([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456",
        "--execute-background-ax", "--execute-background-ax"
    ]) == nil)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456",
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "123456"
    ]) == .invalid)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-seconds", "1700000000", "--execute-start-micros"
    ]) == .invalid)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-micros", "123456", "--execute-start-seconds", "1700000000"
    ]) == .invalid)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-seconds", "01", "--execute-start-micros", "0"
    ]) == .invalid)
    try check(NativeExecutionBirthStampArguments.parse([
        "--execute-start-seconds", "1700000000", "--execute-start-micros", "010"
    ]) == .invalid)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "1", micros: "0")
        == DeviceProcessStamp(seconds: 1, micros: 0))
    try check(NativeExecutionBirthStampArguments.parseExpected(
        seconds: "9007199254740991", micros: "999999")
        == DeviceProcessStamp(seconds: NativeExecutionBirthStampArguments.maximumSafeSeconds, micros: 999_999))
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "0", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "01", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "+1", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "-1", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "1e3", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "9007199254740992", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "18446744073709551616", micros: "0") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "1", micros: "1000000") == nil)
    try check(NativeExecutionBirthStampArguments.parseExpected(seconds: "1", micros: "-1") == nil)
    func ownedBackgroundDiagnostics(_ executablePath: String = "/tmp/owned-background-guard",
                                   bundleID: String = "com.lingxi.computer-user.guard-fixture",
                                   pid: Int32 = 700,
                                   windowID: String? = "window-42",
                                   hasExpectedStamp: Bool = true,
                                   actualStartSeconds: UInt64 = 1_700_000_000,
                                   backgroundAX: Bool = true) -> Bool {
        let actualStamp = DeviceProcessStamp(seconds: actualStartSeconds, micros: 123_456)
        return ownedBackgroundAXDiagnosticModeEnabled(executablePath: executablePath,
            bundleID: bundleID, pid: pid, windowID: windowID,
            expectedStamp: hasExpectedStamp ? DeviceProcessStamp(seconds: 1_700_000_000, micros: 123_456) : nil,
            actualStamp: actualStamp, backgroundAX: backgroundAX)
    }
    try check(ownedBackgroundDiagnostics())
    try check(!ownedBackgroundDiagnostics("/tmp/device"))
    try check(!ownedBackgroundDiagnostics(bundleID: "example.editor"))
    try check(!ownedBackgroundDiagnostics(pid: 0))
    try check(!ownedBackgroundDiagnostics(windowID: nil))
    try check(!ownedBackgroundDiagnostics(hasExpectedStamp: false))
    try check(!ownedBackgroundDiagnostics(actualStartSeconds: 1_700_000_001))
    try check(!ownedBackgroundDiagnostics(backgroundAX: false))
    try check(NativeExecutionBirthStampArguments.matches(expected: nil, actual: expectedBirth))
    try check(NativeExecutionBirthStampArguments.matches(expected: expectedBirth, actual: expectedBirth))
    try check(!NativeExecutionBirthStampArguments.matches(expected: expectedBirth,
        actual: DeviceProcessStamp(seconds: expectedBirth.seconds + 1, micros: expectedBirth.micros)))
    try check(!NativeExecutionBirthStampArguments.matches(expected: expectedBirth,
        actual: DeviceProcessStamp(seconds: expectedBirth.seconds, micros: expectedBirth.micros + 1)))
    var input = InputClassifier(marker: 123)
    func notice(_ type: CGEventType, marker: Int64 = 0, key: Int64 = 0,
                flags: CGEventFlags = [], point: CGPoint = .zero) -> InputNotice {
        input.classify(type: type, marker: marker, key: key, flags: flags, point: point)
    }
    try check(notice(.keyDown, marker: 123) == .ignored)
    try check(notice(.keyDown, key: 53, flags: [.maskCommand, .maskShift]) == .stop)
    try check(notice(.keyDown, key: 53, flags: [.maskControl, .maskShift]) == .stop)
    try check(notice(.keyDown, marker: 123, key: 53, flags: [.maskCommand, .maskShift]) == .ignored)
    try check(notice(.keyDown, key: 53, flags: [.maskCommand]) == .human)
    try check(notice(.tapDisabledByTimeout, marker: 123) == .disabled)
    try check(notice(.tapDisabledByUserInput) == .disabled)
    try check(notice(.mouseMoved, point: CGPoint(x: 10, y: 10)) == .ignored)
    try check(notice(.mouseMoved, point: CGPoint(x: 11, y: 11)) == .ignored)
    try check(notice(.mouseMoved, point: CGPoint(x: 12, y: 12)) == .ignored)
    try check(notice(.mouseMoved, point: CGPoint(x: 13, y: 13)) == .human)
    try check(notice(.scrollWheel) == .human)
    let gate = GuardState(epoch: 7)
    try check(!gate.renew(lane: "host", duration: 0, now: 10))
    try check(!gate.renew(lane: "host", duration: 1501, now: 10))
    try check(!gate.renew(lane: "source", duration: 100, now: 10))
    try check(gate.renew(lane: "host", duration: 100, now: 10))
    try check(gate.renew(lane: "supervisor", duration: 100, now: 50))
    _ = gate.expire(now: 110)
    let hostLost = gate.snapshot(now: 110)
    try check(!hostLost.host_alive && hostLost.supervisor_alive && hostLost.stop_revision == 1)
    try check(gate.renew(lane: "host", duration: 100, now: 110))
    try check(gate.snapshot().stop_revision == 1)
    _ = gate.expire(now: 150)
    try check(!gate.snapshot().supervisor_alive && gate.snapshot().stop_revision == 2)
    _ = gate.expire(now: 151)
    try check(gate.snapshot().stop_revision == 2)
    let revision = gate.beginQuery(now: 200)
    gate.stop()
    try check(!gate.endQuery(revision: revision))
    let unchanged = gate.beginQuery(now: 300)
    try check(gate.endQuery(revision: unchanged))
    _ = gate.beginQuery(now: 400)
    try check(!gate.expire(now: 899))
    try check(gate.expire(now: 900))
    _ = gate.endQuery(revision: 0)
    gate.permissions(ax: true, human: true)
    let beforeLoss = gate.snapshot().stop_revision
    gate.permissions(ax: true, human: false)
    try check(!gate.snapshot().human_input && gate.snapshot().stop_revision == beforeLoss + 1)
    gate.notice(.human)
    try check(gate.snapshot().human_sequence == 1)
    gate.notice(.stop)
    try check(gate.snapshot().hotkey_sequence == 1)
    try check(gate.snapshot().lease_loss_sequence == 3)
    try check(gate.snapshot().fault_sequence == 1)
    try check(!gate.snapshot().input_available)
    gate.loopAlive(now: 1000)
    try check(!gate.expire(now: 1749))
    try check(gate.expire(now: 1750))
    gate.close()
    try check(gate.closing)
    struct Golden: Decodable {
        struct Fingerprint: Decodable { let fingerprint: NativeBindingFingerprint; let canonical: String; let digest: String }
        struct Label: Decodable { let raw: String; let normalized: String }
        struct Rounding: Decodable { let raw: [Double]; let rounded: [Int64] }
        let fingerprints: [Fingerprint]; let labels: [Label]; let rounding: [Rounding]
    }
    let golden = try JSONDecoder().decode(Golden.self, from: Data(contentsOf: URL(fileURLWithPath: goldenPath)))
    struct BackgroundContextGolden: Decodable {
        let name: String
        let bundle_id: String
        let pid: UInt32
        let source: NativeDeviceSourceBackgroundWindowContext
    }
    let backgroundGoldenURL = URL(fileURLWithPath: goldenPath)
        .deletingLastPathComponent().appendingPathComponent("background-contexts.json")
    let backgroundGoldens = try JSONDecoder().decode([BackgroundContextGolden].self,
        from: Data(contentsOf: backgroundGoldenURL))
    try check(backgroundGoldens.count == 7)
    for entry in backgroundGoldens {
        let computed = entry.source.computedTargetContextHash(bundleID: entry.bundle_id, pid: entry.pid)
        if computed != entry.source.target_context_hash {
            FileHandle.standardError.write(Data(
                "background context hash mismatch: \(entry.name) expected=\(entry.source.target_context_hash) computed=\(computed)\n".utf8))
        }
        try check(computed == entry.source.target_context_hash)
    }
    for entry in golden.fingerprints {
        try check(entry.fingerprint.valid)
        try check(entry.fingerprint.canonical == Data(entry.canonical.utf8))
        try check(entry.fingerprint.digest == entry.digest)
    }
    for entry in golden.labels { try check(bindingLabel(entry.raw) == entry.normalized) }
    for entry in golden.rounding {
        let rect = HitRect(CGRect(x: entry.raw[0], y: entry.raw[1], width: entry.raw[2], height: entry.raw[3]))
        try check(rect.rounded == entry.rounded)
    }
    var proof = golden.fingerprints[0].fingerprint
    proof.ancestor_roles = Array(repeating: "AXGroup", count: 33)
    try check(!proof.valid)
    proof = golden.fingerprints[0].fingerprint; proof.sibling_index = 256
    try check(!proof.valid)
    proof = golden.fingerprints[0].fingerprint; proof.scroll_container = "unknown"
    try check(!proof.valid)
    proof = golden.fingerprints[0].fingerprint; proof.frame = []
    try check(!proof.valid)
    proof = golden.fingerprints[0].fingerprint; proof.label = "NOT NORMALIZED"
    try check(!proof.valid)
    proof = golden.fingerprints[0].fingerprint
    proof.app = "com.lingxi.computer-user.mvp-fixture"
    proof.ancestor_roles = [kAXSheetRole]
    try check(proof.valid && proof.isRegisteredSheetBinding)
    proof.app = "unregistered.app"
    try check(!proof.valid && !proof.isRegisteredSheetBinding)
    proof.app = "com.lingxi.computer-user.mvp-fixture"
    proof.ancestor_roles = [kAXSheetRole, kAXWindowRole]
    try check(!proof.valid)
    for error in [AXError.attributeUnsupported, AXError.noValue] {
        try check(registeredSheetModalAttributeKnownNonTrue(error: error.rawValue, typeID: nil, modal: nil, timely: true))
    }
    for error in [AXError.cannotComplete, AXError.failure, AXError.apiDisabled] {
        try check(!registeredSheetModalAttributeKnownNonTrue(error: error.rawValue, typeID: nil, modal: nil, timely: true))
    }
    try check(registeredSheetModalAttributeKnownNonTrue(error: 0, typeID: CFBooleanGetTypeID(), modal: false, timely: true))
    try check(!registeredSheetModalAttributeKnownNonTrue(error: 0, typeID: CFBooleanGetTypeID(), modal: true, timely: true))
    try check(!registeredSheetModalAttributeKnownNonTrue(error: 0, typeID: CFBooleanGetTypeID(), modal: false, timely: false))
    try check(!registeredSheetModalAttributeKnownNonTrue(error: 0, typeID: CFNumberGetTypeID(), modal: false, timely: true))
    try check(registeredSheetChildRolesKnown([kAXScrollAreaRole, kAXSheetRole, kAXButtonRole]))
    try check(!registeredSheetChildRolesKnown([kAXSheetRole, nil]))
    try check(!registeredSheetChildRolesKnown([kAXSheetRole, kAXSheetRole]))
    try check(!registeredSheetChildRolesKnown([]))
    try check(!registeredSheetChildRolesKnown([kAXSheetRole, "AXPopover"]))
    try check(!registeredSheetChildRolesKnown([kAXSheetRole, "AXDialog"]))
    // Native authority is an independent, fixed deadline and terminal ledger.
    let fp = golden.fingerprints[0].fingerprint
    func continuity(_ kind: String = "notification_covered", sequence: UInt64 = 1,
                    suffix: String = String(repeating: "a", count: 64),
                    scope: [String: Any] = ["kind": "whole_structure"]) -> [String: Any] {
        ["kind": kind, "source_sequence": sequence, "context_digest": "bc1:" + suffix, "scope": scope]
    }
    let sourceProof: [String: Any] = ["binding": "bind-1", "instance": "object-1", "snapshot_id": "source-1",
        "environment_revision": 1, "layer_revision": 1,
        "fingerprint": try JSONSerialization.jsonObject(with: fp.canonical),
        "display_frame": ["x": -11, "y": 21, "width": 40, "height": 20],
        "continuity": continuity()]
    let testFrame = HitRect(CGRect(x: -11, y: 21, width: 40, height: 20))
    passed += try runBackgroundAXPolicyTests().count
    passed += try runDeviceContextWitnessTests(fingerprint: fp, displayFrame: testFrame, startingAt: passed)
    passed += try runTableColumnContextTests(startingAt: passed)
    passed += try runRowBindingContextTests(startingAt: passed)
    passed += try runRowContextWitnessTests(startingAt: passed)
    passed += try runMenuPathBoundaryTests(startingAt: passed)
    try check(optionalAXTextReadStatusTestsPass())
    let contextWitness = try deviceContextWitnessJSON(fingerprint: fp, displayFrame: testFrame)
    let body: [String: Any] = ["request_id": "probe-1", "intent_id": "intent-1", "task_id": "root-1",
        "task_revision": 1, "root_epoch": 1, "permit": ["id": "permit-1", "generation": 1],
        "stop_seq": 5, "guard_revision": 0, "clock_domain": "macos/7/wall", "surface": ["id": "main", "epoch": 7],
        "evidence_id": "evidence-1", "intent_evidence_id": "original-1", "source_sequence": 1, "observed_ms": 10,
        "valid_until_ms": 1010, "command_end_ms": 1010, "started_ms": 10, "duration_ms": 1000, "latest_start_ms": 10,
        "original_proof": sourceProof, "current_proof": sourceProof, "context_witness": contextWitness,
        "binding_digest": fp.digest,
        "dependencies": ["focus": 1, "layer": 1, "binding": 1, "authorization": 1, "geometry": 1],
        "action": "ax_press", "x": 0, "y": 25]
    func probe(_ override: [String: Any] = [:]) throws -> NativeProbe {
        let merged = body.merging(override) { _, new in new }
        return try JSONDecoder().decode(NativeProbe.self, from: JSONSerialization.data(withJSONObject: merged))
    }
    let unsupportedContextData = try JSONEncoder().encode(NativeDeviceContextWitness.unsupported(.tableViewport))
    let unsupportedContextWire = try JSONSerialization.jsonObject(with: unsupportedContextData)
    try check(try !probe(["context_witness": unsupportedContextWire]).valid)
    var windowFingerprint = fp
    windowFingerprint.role = "AXWindow"; windowFingerprint.subrole = "AXStandardWindow"; windowFingerprint.ancestor_roles = []; windowFingerprint.group = nil; windowFingerprint.sibling_index = 0
    try check(windowFingerprint.valid)
    let windowSource = sourceProof.merging(["fingerprint": try JSONSerialization.jsonObject(with: windowFingerprint.canonical)]) { _, value in value }
    let windowWitness = try deviceContextWitnessJSON(fingerprint: windowFingerprint, displayFrame: testFrame)
    let windowProbe = try probe(["action":"activate_window", "original_proof":windowSource,"current_proof":windowSource,
        "context_witness":windowWitness,"binding_digest":windowFingerprint.digest,"dependencies":["window":1,"authorization":1]])
    try check(windowProbe.valid)
    try check(try !probe(["action":"activate_window", "original_proof":windowSource,"current_proof":windowSource,
        "context_witness":windowWitness,"binding_digest":windowFingerprint.digest]).valid)
    var invalidWindow = windowFingerprint; invalidWindow.subrole = "AXDialog"; try check(!invalidWindow.valid)
    invalidWindow = windowFingerprint; invalidWindow.ancestor_roles = ["AXWindow"]; try check(!invalidWindow.valid)
    let request = try probe()
    try check(request.valid && request.hit?.valid == true)
    var coveredRefresh = sourceProof
    coveredRefresh["snapshot_id"] = "source-2"; coveredRefresh["environment_revision"] = 2
    coveredRefresh["continuity"] = continuity(sequence: 2)
    let coveredRefreshRequest = try probe(["request_id": "covered-refresh", "intent_id": "covered-refresh",
        "evidence_id": "evidence-2", "source_sequence": 2,
        "original_proof": sourceProof, "current_proof": coveredRefresh])
    try check(coveredRefreshRequest.valid)
    var changedClosure = coveredRefresh
    changedClosure["continuity"] = continuity(sequence: 2, suffix: String(repeating: "b", count: 64))
    let changedClosureRequest = try probe(["request_id": "changed-closure", "intent_id": "changed-closure",
        "evidence_id": "evidence-2", "source_sequence": 2,
        "original_proof": sourceProof, "current_proof": changedClosure])
    try check(!changedClosureRequest.valid)
    try check(strictProbeFields(body))
    var missingContinuity = sourceProof; missingContinuity.removeValue(forKey: "continuity")
    try check(!strictProbeFields(body.merging(["original_proof": missingContinuity]) { _, value in value }))
    var badContinuityProof = sourceProof
    badContinuityProof["continuity"] = continuity(suffix: String(repeating: "A", count: 64))
    try check(try !probe(["original_proof": badContinuityProof, "current_proof": badContinuityProof]).valid)
    let unknownScopeProof = sourceProof.merging(["continuity": continuity(scope: ["kind": "unknown_scope"])]) { _, value in value }
    try check(!strictProbeFields(body.merging(["original_proof": unknownScopeProof]) { _, value in value }))
    let tableScope: [String: Any] = ["kind": "table_viewport", "table_instance_ref": "table-1",
        "scroll_container_instance_ref": "scroll-1", "viewport_frame": ["x": -10.0, "y": 20.0, "width": 100.0, "height": 80.0],
        "visible_rows_complete": true, "columns_complete": true]
    let tableProof = sourceProof.merging(["continuity": continuity(scope: tableScope)]) { _, value in value }
    let tableRequest = try probe(["original_proof": tableProof, "current_proof": tableProof])
    try check(strictProbeFields(body.merging(["original_proof": tableProof, "current_proof": tableProof]) { _, value in value }))
    try check(!tableRequest.valid)
    var unknown = body; unknown["unexpected"] = true
    try check(!strictProbeFields(unknown))
    func valueProbe(_ text: String, role: String = "AXTextField", app: String = "com.apple.TextEdit", subrole: String? = nil) throws -> NativeProbe {
        var fingerprint = try JSONSerialization.jsonObject(with: fp.canonical) as! [String: Any]
        fingerprint["role"] = role; fingerprint["app"] = app; fingerprint["subrole"] = subrole ?? NSNull()
        var proof = sourceProof; proof["fingerprint"] = fingerprint
        var typedFingerprint = fp
        typedFingerprint.role = role; typedFingerprint.app = app; typedFingerprint.subrole = subrole
        return try probe(["action": "ax_set_value", "value": text,
            "original_proof": proof, "current_proof": proof,
            "context_witness": try deviceContextWitnessJSON(fingerprint: typedFingerprint,
                displayFrame: testFrame)])
    }
    try check(try valueProbe("中文\nHello 👋").validAction)
    try check(try valueProbe("中文\nHello 👋", app: "com.lingxi.computer-user.mvp-fixture").validAction)
    try check(try !valueProbe("secret", app: "com.lingxi.computer-user.mvp-fixture", subrole: "AXSecureTextField").validAction)
    try check(try valueProbe(String(repeating: "a", count: 2048)).validAction)
    try check(try !valueProbe(String(repeating: "a", count: 2049)).validAction)
    try check(try !valueProbe(String(repeating: "中", count: 683)).validAction)
    try check(try !valueProbe("secret", subrole: "AXSecureTextField").validAction)
    try check(try !valueProbe("secret", subrole: "AXUnknown").validAction)
    try check(try !valueProbe("value", app: "unregistered.app").validAction)
    try check(try !valueProbe("bad\u{0}").validAction)
    try check(try valueProbe("on", role: "AXCheckBox").validAction)
    try check(try !valueProbe("true", role: "AXCheckBox").validAction)
    var valueBody = body; valueBody["value"] = "text"
    try check(strictProbeFields(valueBody))
    try check(try !probe(["value": "unexpected"]).validAction)
    try check(AtomicClickProfile.id == "macos-atomic-click@1" && AtomicClickProfile.releaseGapMS == 0)
    for (button, down, up) in [("left", CGEventType.leftMouseDown, CGEventType.leftMouseUp),
                              ("right", .rightMouseDown, .rightMouseUp), ("middle", .otherMouseDown, .otherMouseUp)] {
        for mask in 0..<16 {
            let modifiers: [String: Bool] = ["shift": mask & 1 != 0, "control": mask & 2 != 0,
                "option": mask & 4 != 0, "command": mask & 8 != 0]
            let mouse: [String: Any] = ["button": button, "modifiers": modifiers]
            let click = try probe(["action": "click", "mouse": mouse])
            try check(click.valid && click.hit?.validFor(action: "click") == true)
            guard let pair = AtomicMousePair(request: click, marker: 123) else { throw DeviceTestFailure(check: passed + 1) }
            try check(pair.down.type == down && pair.up.type == up)
            try check(pair.down.flags == click.mouse!.modifiers.flags && pair.up.flags == pair.down.flags)
            try check(pair.down.getIntegerValueField(.eventSourceUserData) == 123 && pair.up.getIntegerValueField(.eventSourceUserData) == 123)
            try check(pair.down.location == CGPoint(x: 0, y: 25) && pair.up.location == pair.down.location)
            try check(pair.source.localEventsSuppressionInterval == 0)
            var fields = body; fields["action"] = "click"; fields["mouse"] = mouse
            try check(strictProbeFields(fields))
        }
    }
    let mouse: [String: Any] = ["button": "left", "modifiers": ["shift": false, "control": false, "option": false, "command": false]]
    try check(try !probe(["action": "click"]).validAction)
    try check(try !probe(["mouse": mouse]).validAction)
    try check(try !probe(["action": "click", "mouse": mouse, "value": "unexpected"]).validAction)
    let badMouse: [String: Any] = ["button": "back", "modifiers": mouse["modifiers"]!]
    try check(try !probe(["action": "click", "mouse": badMouse]).validAction)
    var badFields = body; badFields["mouse"] = ["button": "left", "modifiers": ["shift": false], "delay_ms": 10]
    try check(!strictProbeFields(badFields))
    func roleProbe(_ action: String, role: String, extra: [String: Any] = [:]) throws -> NativeProbe {
        var fingerprint = fp; fingerprint.role = role
        let proof = sourceProof.merging(["fingerprint": try JSONSerialization.jsonObject(with: fingerprint.canonical)]) { _, value in value }
        return try probe(["action": action, "original_proof": proof, "current_proof": proof,
            "context_witness": try deviceContextWitnessJSON(fingerprint: fingerprint, displayFrame: testFrame),
            "binding_digest": fingerprint.digest].merging(extra) { _, value in value })
    }
    for (dx, dy, valid) in [(-120, 120, true), (0, -1, true), (0, 0, false), (121, 1, false), (1, -121, false)] {
        let scroll = try roleProbe("scroll", role: "AXScrollArea", extra: ["scroll": ["dx": dx, "dy": dy]])
        try check(scroll.validAction == valid)
        if valid {
            try check(scroll.valid && scroll.hit?.validFor(action: "scroll") == true)
            guard let event = AtomicScrollEvent(request: scroll, marker: 123)?.event else { throw DeviceTestFailure(check: passed + 1) }
            try check(event.type == .scrollWheel && event.flags.isEmpty)
            try check(event.getIntegerValueField(.scrollWheelEventPointDeltaAxis2) == Int64(dx) && event.getIntegerValueField(.scrollWheelEventPointDeltaAxis1) == Int64(dy))
            try check(event.getIntegerValueField(.scrollWheelEventScrollPhase) == 0 && event.getIntegerValueField(.scrollWheelEventMomentumPhase) == 0)
            try check(event.getIntegerValueField(.eventSourceUserData) == 123)
        }
    }
    try check(try !roleProbe("scroll", role: "AXButton", extra: ["scroll": ["dx": 0, "dy": 1]]).validAction)
    try check(try !roleProbe("scroll", role: "AXScrollArea", extra: ["scroll": ["dx": 0, "dy": 1], "mouse": mouse]).validAction)
    try check(try !roleProbe("ax_show_menu", role: "AXMenuButton", extra: ["scroll": ["dx": 0, "dy": 1]]).validAction)
    for role in ["AXMenuButton", "AXPopUpButton", "AXButton", "AXMenuItem"] {
        let menu = try roleProbe("ax_show_menu", role: role)
        try check(menu.validAction == ["AXMenuButton", "AXPopUpButton"].contains(role))
        if menu.validAction { try check(menu.valid && menu.hit?.validFor(action: "ax_show_menu") == true) }
    }
    var scrollFields = body; scrollFields["scroll"] = ["dx": 0, "dy": 1]
    try check(strictProbeFields(scrollFields))
    scrollFields["scroll"] = ["dx": 0, "dy": 1, "momentum": true]
    try check(!strictProbeFields(scrollFields))
    scrollFields["scroll"] = NSNull(); try check(!strictProbeFields(scrollFields))
    passed += try runAtomicMoveConstructorTests()
    passed += try runAtomicKeyConstructorTests()
    passed += try runKeyReceiptStateTests()
    #if CU_OWNED_KEY_ACCEPTANCE
    passed += try runOwnedKeyTapTraceTests()
    #endif
    let move = try roleProbe("pointer_move", role: "AXScrollArea")
    try check(move.valid && move.hit?.validFor(action: "pointer_move") == true)
    try check(try !roleProbe("pointer_move", role: "AXTextField").validAction)
    try check(try !roleProbe("pointer_move", role: "AXScrollArea", extra: ["mouse": mouse]).validAction)
    try check(try !roleProbe("pointer_move", role: "AXScrollArea", extra: ["scroll": ["dx": 0, "dy": 1]]).validAction)
    let blockedStart = GuardState(epoch: 7, startupMouseButtonsClear: false)
    try check(!blockedStart.snapshot().startup_mouse_buttons_clear && blockedStart.admitSubmission(request).1 == "rejected")
    let delivery = GuardState(epoch: 7)
    let stamp = delivery.beginPointerDelivery(proposedStamp: 100, upType: CGEventType.leftMouseUp.rawValue)!
    delivery.observeOwnedPointer(type: CGEventType.leftMouseDown.rawValue, stamp: stamp)
    try check(!delivery.pointerUpObserved(stamp: stamp))
    delivery.observeOwnedPointer(type: CGEventType.leftMouseUp.rawValue, stamp: stamp - 1)
    try check(!delivery.pointerUpObserved(stamp: stamp))
    delivery.observeOwnedPointer(type: CGEventType.leftMouseUp.rawValue, stamp: stamp)
    try check(delivery.pointerUpObserved(stamp: stamp))
    let nextStamp = delivery.beginPointerDelivery(proposedStamp: 99, upType: CGEventType.rightMouseUp.rawValue)!
    try check(nextStamp > stamp && !delivery.pointerUpObserved(stamp: nextStamp))
    delivery.observeOwnedPointer(type: CGEventType.leftMouseUp.rawValue, stamp: stamp)
    try check(!delivery.pointerUpObserved(stamp: nextStamp))
    let drain = GuardState(epoch: 7)
    let drainStamp = drain.beginPointerDelivery(proposedStamp: 100, upType: CGEventType.leftMouseUp.rawValue)!
    drain.pointerPostsStarted(stamp: drainStamp)
    drain.close()
    try check(drain.inputClosed && !drain.closing)
    try check(drain.admitSubmission(request).1 == "rejected")
    drain.observeOwnedPointer(type: CGEventType.leftMouseUp.rawValue, stamp: drainStamp)
    try check(drain.closing)
    let staleDrain = GuardState(epoch: 7)
    let oldStamp = staleDrain.beginPointerDelivery(proposedStamp: 100, upType: CGEventType.leftMouseUp.rawValue)!
    staleDrain.pointerPostsStarted(stamp: oldStamp)
    _ = staleDrain.beginPointerDelivery(proposedStamp: 101, upType: CGEventType.leftMouseUp.rawValue)
    try check(!staleDrain.pointerDrainPending())
    let surface = NativeSurface(id: "main", epoch: 7)
    let clock = NativeAuthorityClock(origin: 1000, surface: surface, domain: "macos/7/wall", epoch: 7, wall: 1010, uptime: 500)!
    try check(clock.deadline(1010) == 1499)
    try check(clock.deadline(10) == nil)
    try check(clock.healthy(wall: 1410, uptime: 900))
    try check(!clock.healthy(wall: 1300, uptime: 900))
    try check(!clock.healthy(wall: 1009, uptime: 501))
    try check(NativeAuthorityClock(origin: 1011, surface: surface, domain: "macos/7/wall", epoch: 7, wall: 1010, uptime: 500) == nil)

    var readScopedProof = sourceProof
    readScopedProof["continuity"] = continuity("read_scoped")
    let readScopedRequest = try probe(["request_id": "scoped-one", "intent_id": "scoped-one",
        "permit": ["id": "read-scoped-permit", "generation": 1],
        "original_proof": readScopedProof, "current_proof": readScopedProof])
    try check(readScopedRequest.valid)
    var readScopedLedger = NativeAuthorityLedger(); _ = readScopedLedger.configure(clock)
    try check(readScopedLedger.admit(readScopedRequest, now: 500, wall: 1010).1 == "eligible")
    let sameReadSecondIntent = try probe(["request_id": "scoped-same-read-again", "intent_id": "scoped-same-read-again",
        "permit": ["id": "read-scoped-permit", "generation": 1],
        "original_proof": readScopedProof, "current_proof": readScopedProof])
    try check(sameReadSecondIntent.valid && readScopedLedger.admit(sameReadSecondIntent, now: 510, wall: 1020).1 == "rejected")

    var laterReadProof = readScopedProof
    laterReadProof["binding"] = "bind-2"; laterReadProof["instance"] = "object-2"
    laterReadProof["snapshot_id"] = "source-2"; laterReadProof["environment_revision"] = 2
    laterReadProof["continuity"] = continuity("read_scoped", sequence: 2, suffix: String(repeating: "b", count: 64))
    let laterReadRequest = try probe(["request_id": "scoped-next-read", "intent_id": "scoped-next-read",
        "evidence_id": "evidence-2", "intent_evidence_id": "original-2", "source_sequence": 2,
        "permit": ["id": "read-scoped-permit", "generation": 1],
        "original_proof": laterReadProof, "current_proof": laterReadProof])
    try check(laterReadRequest.valid && readScopedLedger.admit(laterReadRequest, now: 520, wall: 1030).1 == "rejected")
    let reboundReadRequest = try probe(["request_id": "scoped-new-permit", "intent_id": "scoped-new-permit",
        "evidence_id": "evidence-2", "intent_evidence_id": "original-2", "source_sequence": 2,
        "permit": ["id": "read-scoped-permit-2", "generation": 1],
        "original_proof": laterReadProof, "current_proof": laterReadProof])
    var reboundLedger = NativeAuthorityLedger(); _ = reboundLedger.configure(clock)
    try check(reboundReadRequest.valid && reboundLedger.admit(reboundReadRequest, now: 520, wall: 1030).1 == "eligible")
    let promotedPermitRequest = try probe(["request_id": "scoped-promoted", "intent_id": "scoped-promoted",
        "evidence_id": "evidence-2", "intent_evidence_id": "original-2", "source_sequence": 2,
        "permit": ["id": "read-scoped-permit", "generation": 1],
        "original_proof": coveredRefresh, "current_proof": coveredRefresh])
    try check(promotedPermitRequest.valid && readScopedLedger.admit(promotedPermitRequest, now: 530, wall: 1040).1 == "rejected")

    var ledger = NativeAuthorityLedger()
    try check(ledger.admit(request, now: 500, wall: 1010).1 == "context_unresolved")
    try check(ledger.configure(clock))
    let admitted = ledger.admit(request, now: 500, wall: 1010)
    try check(admitted.1 == "eligible")
    let ticket = admitted.0!
    let demotedPermitRequest = try probe(["request_id": "covered-demoted", "intent_id": "covered-demoted",
        "permit": ["id": "permit-1", "generation": 1],
        "original_proof": readScopedProof, "current_proof": readScopedProof])
    try check(demotedPermitRequest.valid && ledger.admit(demotedPermitRequest, now: 510, wall: 1020).1 == "rejected")
    try check(ledger.validity(ticket, now: 900, wall: 1410) == nil)
    try check(ledger.validity(ticket, now: 1500, wall: 2010) == "expired")
    let retry = try probe(["request_id": "retry"])
    try check(ledger.admit(retry, now: 600, wall: 1110).1 == "rejected")
    let oldStop = try probe(["request_id": "p2", "intent_id": "i2", "stop_seq": 4])
    try check(ledger.admit(oldStop, now: 600, wall: 1110).1 == "stop_latched")
    try check(ledger.revoke(request.permit))
    try check(ledger.validity(ticket, now: 600, wall: 1110) == "revoked")
    let higherGeneration = try probe(["request_id": "p3", "intent_id": "i3", "permit": ["id": "permit-1", "generation": 2]])
    try check(ledger.admit(higherGeneration, now: 600, wall: 1110).1 == "revoked")
    var delayed = NativeAuthorityLedger(); _ = delayed.configure(clock)
    try check(delayed.admit(request, now: 1500, wall: 2010).1 == "expired")
    var diverged = NativeAuthorityLedger(); _ = diverged.configure(clock)
    try check(!diverged.checkClock(wall: 1400, uptime: 501))
    try check(!diverged.configure(clock))
    var replacement = sourceProof; replacement["instance"] = "replacement"
    let replaced = try probe(["current_proof": replacement]); try check(!replaced.valid)
    let edge = try probe(["x": 29]); try check(!edge.valid) // exclusive right edge
    let extended = try probe(["command_end_ms": 1011]); try check(!extended.valid)
    var capacity = NativeAuthorityLedger(); _ = capacity.configure(clock)
    for i in 0..<NativeAuthorityLedger.limit {
        let result = capacity.admit(try probe(["request_id": "p-\(i)", "intent_id": "i-\(i)"]), now: 500, wall: 1010)
        guard result.0 != nil else { throw DeviceTestFailure(check: passed + 1) }
    }
    let overfull = try probe(["request_id": "overfull", "intent_id": "overfull"])
    try check(capacity.admit(overfull, now: 500, wall: 1010).1 == "rejected")
    var commits = NativeAuthorityLedger(); _ = commits.configure(clock)
    let commitTicket = commits.admit(request, now: 500, wall: 1010).0!
    try check(commits.commit(commitTicket, now: 600, wall: 1110) == nil)
    try check(commits.commit(commitTicket, now: 601, wall: 1111) == "rejected")
    try check(commits.revoke(request.permit))
    try check(commits.commit(commitTicket, now: 602, wall: 1112) == "revoked")
    let independent = GuardState(epoch: 7)
    independent.loopAlive(now: 100)
    _ = independent.beginQuery(now: 100)
    try check(!independent.hardExpired(now: 599))
    try check(independent.hardExpired(now: 600))
    _ = independent.endQuery(revision: 0)
    try check(!independent.hardExpired(now: 600))
    try check(independent.hardExpired(now: 850))
    try check(independent.admitSubmission(request).1 == "rejected")
    let lateRenewal = GuardState(epoch: 7)
    try check(lateRenewal.renew(lane: "host", duration: 100, now: 100))
    try check(lateRenewal.renew(lane: "host", duration: 100, now: 200))
    try check(lateRenewal.snapshot(now: 200).stop_revision == 1 && lateRenewal.snapshot(now: 200).lease_loss_sequence == 1)

    // A settled scoped opening is a historical fact, not a second permission.
    // These checks use typed values only; no AX query or input is performed.
    for error in [AXError.attributeUnsupported, .cannotComplete, .invalidUIElement, .failure] {
        try check(!menuWindowAttributeAllowed(error: error, sameRoot: true))
    }
    try check(menuWindowAttributeAllowed(error: .noValue, sameRoot: false))
    try check(menuWindowAttributeAllowed(error: .success, sameRoot: true))
    try check(!menuWindowAttributeAllowed(error: .success, sameRoot: false))
    var openerFP = fp; openerFP.role = kAXMenuButtonRole
    let protectedFrame = HitRect(CGRect(x: -11, y: 21, width: 40, height: 20))
    let rootFrame = HitRect(CGRect(x: -50, y: 0, width: 200, height: 200))
    let openingAnchor = NativeMenuOpenerAnchor(anchor_id: "anchor-1", source_session_id: "watch-1", anchor_revision: 1,
        process: NativeMenuProcessIdentity(pid: fp.app_pid, start_seconds: 100, start_micros: 10), origin_window_id: fp.window_id,
        origin_window_frame: rootFrame, opener_instance: "opener-before", opener_fingerprint: openerFP, opener_frame: protectedFrame,
        verified_source_sequence: 1, verified_observed_ms: 10, continuity: "direct_read")
    let openingScope = NativeMenuBindingScope.opener(openingAnchor, 2010)
    func dictionary<T: Encodable>(_ value: T) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: JSONEncoder().encode(value)) as! [String: Any]
    }
    let openingContextWitness = try deviceContextWitnessJSON(fingerprint: openerFP,
        displayFrame: protectedFrame, windowFrame: rootFrame,
        menu: try deviceContextMenuRelation(openingScope))
    var openingProof = sourceProof
    openingProof["instance"] = openingAnchor.opener_instance; openingProof["fingerprint"] = try dictionary(openerFP)
    openingProof["menu_scope"] = try dictionary(openingScope)
    let openingRequest = try probe(["request_id": "open-request", "intent_id": "open-intent", "attempt_id": "attempt-1",
        "action": "ax_show_menu", "original_proof": openingProof, "current_proof": openingProof,
        "context_witness": openingContextWitness, "binding_digest": openerFP.digest])
    try check(openingRequest.valid && openingAnchor.valid)
    try check(strictMenuScopeFields(try dictionary(openingScope)))
    try check(try !probe(["action": "ax_show_menu", "original_proof": openingProof,
        "current_proof": openingProof, "context_witness": openingContextWitness,
        "binding_digest": openerFP.digest]).valid)
    let currentAnchor = NativeMenuOpenerAnchor(anchor_id: openingAnchor.anchor_id, source_session_id: openingAnchor.source_session_id,
        anchor_revision: 1, process: openingAnchor.process, origin_window_id: fp.window_id, origin_window_frame: rootFrame,
        opener_instance: "opener-after-registry-change", opener_fingerprint: openerFP, opener_frame: protectedFrame,
        verified_source_sequence: 2, verified_observed_ms: 130, continuity: "direct_read")
    try check(openingAnchor.sameIdentity(currentAnchor))
    var menuFP = openerFP; menuFP.role = kAXMenuRole; menuFP.label = ""; menuFP.sibling_index = 0
    menuFP.ancestor_roles = openerFP.ancestor_roles + [openerFP.role]
    let menuFrame = HitRect(CGRect(x: -12, y: 20, width: 44, height: 50)); menuFP.frame = menuFrame.rounded
    let context = NativeMenuContext(context_id: "context-1", source_session_id: "watch-1", opener: currentAnchor,
        menu_instance: "menu-1", menu_fingerprint: menuFP, menu_frame: menuFrame,
        drawing_window: NativeMenuDrawingWindow(window_id: 900, owner_pid: fp.app_pid, layer: 101, frame: menuFrame))
    var itemFP = fp; itemFP.role = kAXMenuItemRole; itemFP.label = "menu choice"
    itemFP.ancestor_roles = menuFP.ancestor_roles + [kAXMenuRole]
    let selectionScope = NativeMenuBindingScope.item(context, NativeMenuOpeningRef(attempt_id: "attempt-1", intent_id: "open-intent", attempt_deadline_ms: 2010))
    var itemProof = sourceProof; itemProof["instance"] = "menu-item-1"; itemProof["snapshot_id"] = "source-2"
    itemProof["continuity"] = continuity(sequence: 2)
    itemProof["fingerprint"] = try dictionary(itemFP)
    itemProof["menu_scope"] = try dictionary(selectionScope)
    let selectionContextWitness = try deviceContextWitnessJSON(fingerprint: itemFP,
        displayFrame: protectedFrame, windowFrame: rootFrame,
        menu: try deviceContextMenuRelation(selectionScope))
    let selectionBody: [String: Any] = ["request_id": "select-request", "intent_id": "select-intent", "attempt_id": "attempt-1",
        "permit": ["id": "selection-permit", "generation": 1], "action": "ax_menu_press", "original_proof": itemProof,
        "current_proof": itemProof, "context_witness": selectionContextWitness,
        "binding_digest": itemFP.digest, "source_sequence": 2, "observed_ms": 130,
        "started_ms": 130, "latest_start_ms": 130, "duration_ms": 1000, "command_end_ms": 1130, "valid_until_ms": 1130]
    let selection = try probe(selectionBody)
    try check(selection.valid && context.valid && strictMenuScopeFields(try dictionary(selectionScope)))
    var menuLedger = NativeAuthorityLedger(); _ = menuLedger.configure(clock)
    let openTicket = menuLedger.admit(openingRequest, now: 500, wall: 1010).0!
    try check(menuLedger.commit(openTicket, now: 600, wall: 1110) == nil)
    var settledOpening = NativeSubmitResponse(session_epoch: 7, request_id: openingRequest.request_id, outcome: "applied", reason: "offline settled input")
    settledOpening.submitted = true; settledOpening.returned_ms = 120
    try check(menuLedger.recordMenuOpening(openTicket, response: settledOpening))
    try check(menuLedger.revoke(openingRequest.permit))
    // The next command may end after the opening's short command but can never
    // exceed the same immutable activated-attempt ceiling.
    try check(selection.command_end_ms > openingRequest.command_end_ms && menuLedger.menuValidity(selection) == nil)
    let selectTicket = menuLedger.admit(selection, now: 630, wall: 1140).0!
    try check(menuLedger.commit(selectTicket, now: 640, wall: 1150) == nil)
    try check(menuLedger.menuValidity(selection) != nil)
    let secondSelection = try probe(selectionBody.merging(["request_id": "select-again", "intent_id": "select-again", "permit": ["id": "second-permit", "generation": 1]]) { _, value in value })
    try check(menuLedger.admit(secondSelection, now: 650, wall: 1160).0 == nil)
    var unknownLedger = NativeAuthorityLedger(); _ = unknownLedger.configure(clock)
    let unknownTicket = unknownLedger.admit(openingRequest, now: 500, wall: 1010).0!
    try check(unknownLedger.commit(unknownTicket, now: 600, wall: 1110) == nil)
    var unknownOpening = settledOpening; unknownOpening.outcome = "unknown"
    try check(!unknownLedger.recordMenuOpening(unknownTicket, response: unknownOpening))
    try check(unknownLedger.menuValidity(selection) != nil)
    let reopening = try probe(["request_id": "open-again", "intent_id": "open-again", "attempt_id": "attempt-1",
        "permit": ["id": "reopen-permit", "generation": 1], "action": "ax_show_menu",
        "original_proof": openingProof, "current_proof": openingProof,
        "context_witness": openingContextWitness, "binding_digest": openerFP.digest])
    try check(unknownLedger.admit(reopening, now: 650, wall: 1160).0 == nil)
    var freshLedger = NativeAuthorityLedger(); _ = freshLedger.configure(clock)
    let freshOpen = freshLedger.admit(openingRequest, now: 500, wall: 1010).0!
    _ = freshLedger.commit(freshOpen, now: 600, wall: 1110); _ = freshLedger.recordMenuOpening(freshOpen, response: settledOpening)
    let stoppedSelection = try probe(selectionBody.merging(["guard_revision": 1]) { _, value in value })
    try check(freshLedger.menuValidity(stoppedSelection) != nil)
    let unscopedReopening = try probe(["request_id": "unscoped-reopen", "intent_id": "unscoped-reopen", "attempt_id": "attempt-1",
        "action": "ax_show_menu", "original_proof": openingProof.merging(["menu_scope": NSNull()]) { _, value in value },
        "current_proof": openingProof.merging(["menu_scope": NSNull()]) { _, value in value },
        "context_witness": try deviceContextWitnessJSON(fingerprint: openerFP,
            displayFrame: protectedFrame, windowFrame: rootFrame), "binding_digest": openerFP.digest])
    try check(unscopedReopening.valid && freshLedger.menuValidity(unscopedReopening) != nil)
    let changedRoot = try probe(selectionBody.merging(["root_epoch": 2]) { _, value in value })
    try check(changedRoot.valid && freshLedger.menuValidity(changedRoot) != nil)
    let extendedScope = NativeMenuBindingScope.item(context, NativeMenuOpeningRef(attempt_id: "attempt-1", intent_id: "open-intent", attempt_deadline_ms: 2011))
    var extendedProof = itemProof; extendedProof["menu_scope"] = try dictionary(extendedScope)
    let extendedSelection = try probe(selectionBody.merging(["original_proof": extendedProof, "current_proof": extendedProof]) { _, value in value })
    try check(extendedSelection.valid && freshLedger.menuValidity(extendedSelection) != nil)
    try check(!selection.original_proof.sameInstance(extendedSelection.original_proof))
    let wrongAttempt = try probe(selectionBody.merging(["attempt_id": "another-attempt"]) { _, value in value })
    try check(!wrongAttempt.valid)
    let staleSelection = try probe(selectionBody.merging(["observed_ms": 120]) { _, value in value })
    try check(!staleSelection.valid || freshLedger.menuValidity(staleSelection) != nil)
    var malformedScope = try dictionary(selectionScope); var malformedContext = malformedScope["context"] as! [String: Any]
    malformedContext["extra"] = true; malformedScope["context"] = malformedContext
    try check(!strictMenuScopeFields(malformedScope))
    var noScope = itemProof; noScope["menu_scope"] = NSNull()
    let unscopedItemWitness = try deviceContextWitnessJSON(fingerprint: itemFP,
        displayFrame: protectedFrame, windowFrame: rootFrame)
    try check(try !probe(selectionBody.merging(["original_proof": noScope, "current_proof": noScope,
        "context_witness": unscopedItemWitness]) { _, value in value }).valid)
    var keyFP = fp; keyFP.role = kAXCheckBoxRole; keyFP.subrole = nil
    let keyboard = NativeKeyboardContext(selected_input_source: nil, underlying_keyboard_layout: nil, keyboard_type: nil, caps_lock: nil)
    let focus = NativeFocusContext(source_session_id: "focus-source-1", session_epoch: 7,
        process: openingAnchor.process, origin_window_id: keyFP.window_id, target_instance: "key-object-1", focused_instance: "key-object-1",
        origin_window_frame: rootFrame, target_frame: protectedFrame, target_fingerprint: keyFP, relation: "exact_control",
        composition: NativeFocusComposition(kind: "not_applicable", proof: nil), keyboard: keyboard,
        verified_source_sequence: 1, verified_observed_ms: 10)
    let focusScope = NativeFocusedControlScope(context: focus, profile: AtomicKeyProfile.wireID, key: "Space")
    var keyProof = sourceProof; keyProof["instance"] = "key-object-1"; keyProof["fingerprint"] = try dictionary(keyFP)
    keyProof["focus_scope"] = try dictionary(focusScope)
    let keyPayload: [String: Any] = ["name": "Space", "modifiers": ["shift": false, "control": false, "option": false, "command": false]]
    let keyContextWitness = try deviceContextWitnessJSON(fingerprint: keyFP,
        displayFrame: protectedFrame, windowFrame: rootFrame,
        focus: try deviceContextFocusRelation(focusScope))
    let keyOverrides: [String: Any] = ["action": "key_press", "key": keyPayload,
        "original_proof": keyProof, "current_proof": keyProof,
        "context_witness": keyContextWitness, "binding_digest": keyFP.digest]
    let keyRequest = try probe(keyOverrides)
    try check(keyRequest.valid && focus.valid && strictFocusScopeFields(try dictionary(focusScope)))
    let decodedKeyCopy = try probe(keyOverrides)
    try check(decodedKeyCopy !== keyRequest && decodedKeyCopy == keyRequest)
    try check(MemoryLayout<NativeProbe>.size <= 16 && MemoryLayout<NativeProbeTicket>.size <= 32)
    try check(nativeKeyWorkerRejectsWithoutInput(keyRequest))
    try check(strictProbeFields(body.merging(keyOverrides) { _, value in value }))
    try check(try !probe(keyOverrides.merging(["action": "click"]) { _, value in value }).valid)
    try check(try !probe(keyOverrides.merging(["key": NSNull()]) { _, value in value }).valid)
    try check(try !probe(keyOverrides.merging(["value": "injected"]) { _, value in value }).valid)
    try check(try !probe(keyOverrides.merging(["scroll": ["dx": 0, "dy": 1]]) { _, value in value }).valid)
    try check(try !probe(keyOverrides.merging(["source_sequence": 2]) { _, value in value }).valid)
    try check(try !probe(keyOverrides.merging(["observed_ms": 11]) { _, value in value }).valid)
    // Source and native surface epochs are independent domains; the native
    // authority still compares its own surface to its configured clock.
    try check(try probe(keyOverrides.merging(["surface": ["id": "main", "epoch": 8]]) { _, value in value }).valid)
    var wrongKey = keyPayload; wrongKey["name"] = "Tab"
    try check(try !probe(keyOverrides.merging(["key": wrongKey]) { _, value in value }).valid)
    wrongKey = keyPayload; wrongKey["modifiers"] = ["shift": false, "control": false, "option": false, "command": true]
    try check(try !probe(keyOverrides.merging(["key": wrongKey]) { _, value in value }).valid)
    var scopedBoth = keyProof; scopedBoth["menu_scope"] = try dictionary(openingScope)
    try check(try !probe(keyOverrides.merging(["original_proof": scopedBoth, "current_proof": scopedBoth]) { _, value in value }).valid)
    let focusJSON = try dictionary(focus)
    func changedFocus(_ fields: [String: Any]) throws -> NativeFocusContext {
        try JSONDecoder().decode(NativeFocusContext.self, from: JSONSerialization.data(withJSONObject: focusJSON.merging(fields) { _, value in value }))
    }
    try check(try !changedFocus(["focused_instance": "same-label-replacement"]).valid)
    try check(try !changedFocus(["composition": ["kind": "unknown"]]).valid)
    try check(try !changedFocus(["relation": "ancestor"]).valid)
    try check(try !focus.sameIdentity(changedFocus(["source_session_id": "new-source"])))
    try check(try !focus.sameIdentity(changedFocus(["session_epoch": 8])))
    let otherSourceEpoch = NativeFocusedControlScope(context: try changedFocus(["session_epoch": 8]), profile: AtomicKeyProfile.wireID, key: "Space")
    var otherEpochProof = keyProof; otherEpochProof["focus_scope"] = try dictionary(otherSourceEpoch)
    try check(try probe(keyOverrides.merging(["original_proof": otherEpochProof, "current_proof": otherEpochProof]) { _, value in value }).valid)
    try check(try !focus.sameIdentity(changedFocus(["process": ["pid": fp.app_pid, "start_seconds": 101, "start_micros": 10]])))
    try check(try !focus.sameIdentity(changedFocus(["target_instance": "new-object", "focused_instance": "new-object"])))
    try check(try focus.sameIdentity(changedFocus(["verified_source_sequence": 2, "verified_observed_ms": 20])))
    var malformedFocus = try dictionary(focusScope); malformedFocus["extra"] = true
    try check(!strictFocusScopeFields(malformedFocus))
    var focusKeyboard = try dictionary(keyboard); focusKeyboard["keyboard_type"] = 40
    try check(try !focus.sameIdentity(changedFocus(["keyboard": focusKeyboard])))
    let notClear = GuardState(epoch: 7, startupKeyboardClear: false)
    try check(!notClear.snapshot().startup_keyboard_clear && !notClear.snapshot().keyboard_input_available)
    try check(strictKeyAXBoolean(kCFBooleanTrue) == true && strictKeyAXBoolean(kCFBooleanFalse) == false)
    try check(strictKeyAXBoolean(NSNumber(value: 1)) == nil && strictKeyAXBoolean(NSNumber(value: 0)) == nil)
    try check(strictKeyAXBoolean(nil) == nil && strictKeyAXBoolean("true" as CFString) == nil)
    try check(specialKeyKeyboardFactsMatch(keyboard))
    let unsupportedInputSource = NativeInputSource(id: "actual-source-fact", source_type: nil, category: nil, mode_id: nil, ascii_capable: nil, layout_data_sha256: nil)
    try check(!specialKeyKeyboardFactsMatch(NativeKeyboardContext(selected_input_source: unsupportedInputSource, underlying_keyboard_layout: nil, keyboard_type: nil, caps_lock: nil)))
    try check(!specialKeyKeyboardFactsMatch(NativeKeyboardContext(selected_input_source: nil, underlying_keyboard_layout: unsupportedInputSource, keyboard_type: nil, caps_lock: nil)))
    var expiredKeyLedger = NativeAuthorityLedger(); _ = expiredKeyLedger.configure(clock)
    let expiredKeyTicket = expiredKeyLedger.admit(keyRequest, now: 500, wall: 1010).0!
    try check(expiredKeyLedger.commit(expiredKeyTicket, now: expiredKeyTicket.deadline, wall: 2009) == "expired")
    try check(!expiredKeyLedger.committed.contains(keyRequest.intent_id))
    var stagedKeyLedger = NativeAuthorityLedger(); _ = stagedKeyLedger.configure(clock)
    try check(stagedKeyLedger.stagedKeySubmission(keyRequest, now: 500, wall: 1010).1 == "rejected")
    let stagedKeyTicket = stagedKeyLedger.admit(keyRequest, now: 500, wall: 1010).0!
    try check(stagedKeyLedger.stagedKeySubmission(keyRequest, now: 501, wall: 1011).0 == stagedKeyTicket)
    var changedSnapshotProof = keyProof; changedSnapshotProof["snapshot_id"] = "changed-stage-snapshot"
    let changedSnapshotRequest = try probe(keyOverrides.merging(["current_proof": changedSnapshotProof]) { _, value in value })
    try check(changedSnapshotRequest.valid && changedSnapshotRequest.current_proof.sameInstance(keyRequest.current_proof))
    try check(changedSnapshotRequest != keyRequest && stagedKeyLedger.stagedKeySubmission(changedSnapshotRequest, now: 501, wall: 1011).1 == "rejected")
    try check(stagedKeyLedger.admit(keyRequest, now: 501, wall: 1011).1 == "rejected")
    let alteredStageRequest = try probe(keyOverrides.merging(["request_id": "mutated-key-stage"]) { _, value in value })
    try check(alteredStageRequest.valid && stagedKeyLedger.stagedKeySubmission(alteredStageRequest, now: 501, wall: 1011).1 == "rejected")
    try check(stagedKeyLedger.stagedKeySubmission(keyRequest, now: stagedKeyTicket.deadline, wall: 2009).1 == "expired")
    try check(stagedKeyLedger.commit(stagedKeyTicket, now: 501, wall: 1011) == nil)
    try check(stagedKeyLedger.stagedKeySubmission(keyRequest, now: 502, wall: 1012).1 == "rejected")
    let refs = AXUIElementCreateApplication(12345)
    let witness = KeyFocusWitness(ticket: stagedKeyTicket, epoch: 7, receiver: refs, root: refs)
    let changedRefs = AXUIElementCreateApplication(12346)
    try check(witness.matches(receiver: refs, root: refs))
    try check(!witness.matches(receiver: changedRefs, root: refs))
    try check(!witness.matches(receiver: refs, root: changedRefs))
    let witnesses = KeyFocusWitnessStore()
    try check(witnesses.arm(witness, now: 501))
    try check(!witnesses.arm(witness, now: 502))
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 503)?.ticket == stagedKeyTicket)
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 504) == nil)
    try check(witnesses.arm(witness, now: 505))
    try check(witnesses.take(stagedKeyTicket, epoch: 8, now: 506) == nil)
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 507) == nil)
    try check(witnesses.arm(witness, now: 508))
    let alteredStageTicket = NativeProbeTicket(request: alteredStageRequest, deadline: stagedKeyTicket.deadline)
    try check(witnesses.take(alteredStageTicket, epoch: 7, now: 509) == nil)
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 510) == nil)
    try check(witnesses.arm(witness, now: 511))
    witnesses.prune { _ in false }
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 512) == nil)
    try check(witnesses.arm(witness, now: 513))
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: stagedKeyTicket.deadline) == nil)
    try check(!witnesses.arm(witness, now: stagedKeyTicket.deadline))
    try check(witnesses.arm(witness, now: 514)); witnesses.invalidate()
    try check(witnesses.take(stagedKeyTicket, epoch: 7, now: 515) == nil)
    passed += try runOwnedDocumentValueSetterTests()
    passed += try runOwnedValueStopSequenceTests()
    passed += try runOwnedTransientInventoryTests()
    return DeviceTestResult(passed: passed)
}

/// Read the real Rust envelope without issuing any protocol request or input.
/// This checks independently decoded content equality and the same serial
/// worker dispatch that previously overflowed its default stack on submission.
func runNativeKeyWireTests(path: String) throws -> DeviceTestResult {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw DeviceTestFailure(check: passed + 1) }; passed += 1
    }
    let data = try Data(contentsOf: URL(fileURLWithPath: path))
    try check(data.count <= 8192)
    guard let envelope = try JSONSerialization.jsonObject(with: data) as? [String: Any],
          Set(envelope.keys) == ["protocol_version", "session_epoch", "command", "probe"],
          let fields = envelope["probe"] as? [String: Any] else { throw DeviceTestFailure(check: passed + 1) }
    try check(strictProbeFields(fields))
    let packet = try JSONSerialization.data(withJSONObject: fields)
    let request = try JSONDecoder().decode(NativeProbe.self, from: packet)
    let copy = try JSONDecoder().decode(NativeProbe.self, from: packet)
    try check(request.valid && request.action == "key_press")
    try check(request !== copy && request == copy)
    try check(MemoryLayout<NativeProbe>.size <= 16 && MemoryLayout<NativeProbeTicket>.size <= 32)
    try check(nativeKeyWorkerRejectsWithoutInput(request))
    return DeviceTestResult(passed: passed)
}
