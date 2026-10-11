import AppKit
import Foundation
import ApplicationServices

struct SourceInputSource: Encodable, Sendable, Equatable {
    let id: String?
    let source_type: String?
    let category: String?
    let mode_id: String?
    let ascii_capable: Bool?
    let layout_data_sha256: String?
}

struct SourceKeyboardContext: Encodable, Sendable, Equatable {
    let selected_input_source: SourceInputSource?
    let underlying_keyboard_layout: SourceInputSource?
    let keyboard_type: UInt32?
    let caps_lock: Bool?
    // TIS is synchronous and has no deadline parameter. This phase does not
    // call it on the AX worker or invent a target app's input-context state.
    static let unknown = SourceKeyboardContext(selected_input_source: nil, underlying_keyboard_layout: nil,
        keyboard_type: nil, caps_lock: nil)
}

enum SourceCompositionProofKind: String, Encodable, Sendable {
    case axTextInputMarkedRange = "ax_text_input_marked_range"
}

struct SourceCompositionProof: Encodable, Sendable, Equatable {
    let kind: SourceCompositionProofKind
    let focused_instance: String
    var verified_source_sequence: UInt64
    var verified_observed_ms: UInt64
}

enum SourceCompositionState: Encodable, Sendable, Equatable {
    case unknown, notApplicable
    case marked(SourceCompositionProof), unmarked(SourceCompositionProof)
    private enum CodingKeys: String, CodingKey { case kind, proof }
    func encode(to encoder: any Encoder) throws {
        var value = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .unknown:
            try value.encode("unknown", forKey: .kind)
        case .notApplicable:
            try value.encode("not_applicable", forKey: .kind)
        case .marked(let proof):
            try value.encode("marked", forKey: .kind)
            try value.encode(proof, forKey: .proof)
        case .unmarked(let proof):
            try value.encode("unmarked", forKey: .kind)
            try value.encode(proof, forKey: .proof)
        }
    }

    mutating func normalizeProvenance(sequence: UInt64, observedMS: UInt64) {
        switch self {
        case .marked(var proof):
            proof.verified_source_sequence = sequence
            proof.verified_observed_ms = observedMS
            self = .marked(proof)
        case .unmarked(var proof):
            proof.verified_source_sequence = sequence
            proof.verified_observed_ms = observedMS
            self = .unmarked(proof)
        case .unknown, .notApplicable:
            break
        }
    }
}

enum SourceFocusRelation: String, Encodable, Sendable { case exactControl = "exact_control" }

struct FocusContext: Encodable, Sendable {
    let source_session_id: String
    let session_epoch: UInt64
    let process: KernelProcessIdentity
    let origin_window_id: String
    let origin_window_frame: Rect
    let target_instance: String
    let target_fingerprint: MenuFingerprint
    let target_frame: Rect
    let focused_instance: String
    let relation: SourceFocusRelation
    var composition: SourceCompositionState
    let keyboard: SourceKeyboardContext
    var verified_source_sequence: UInt64
    var verified_observed_ms: UInt64
}

func focusComposition(role: String) -> SourceCompositionState {
    [kAXButtonRole, kAXRadioButtonRole, kAXCheckBoxRole, kAXSliderRole, "AXSwitch", "AXStepper", kAXMenuButtonRole].contains(role)
        ? .notApplicable : .unknown
}

private struct TextInputMarkedRange: Equatable {
    let location: CFIndex
    let length: CFIndex

    init(_ range: CFRange) {
        location = range.location
        length = range.length
    }

    var valid: Bool {
        guard location >= 0, length >= 0 else { return false }
        let (_, overflow) = location.addingReportingOverflow(length)
        return !overflow
    }

    var cfRange: CFRange { CFRange(location: location, length: length) }
}

private func decodeTextInputMarkedRange(error: AXError, value: CFTypeRef?, withinDeadline: Bool) -> TextInputMarkedRange? {
    guard withinDeadline, error == .success, let value,
          CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
    let axValue = unsafeDowncast(value, to: AXValue.self)
    guard AXValueGetType(axValue) == .cfRange else { return nil }
    var range = CFRange(location: 0, length: 0)
    guard AXValueGetValue(axValue, .cfRange, &range) else { return nil }
    let decoded = TextInputMarkedRange(range)
    return decoded.valid ? decoded : nil
}

private func stableTextComposition(role: String, focusedInstance: String,
                                   before: TextInputMarkedRange?, after: TextInputMarkedRange?,
                                   focusStable: Bool) -> SourceCompositionState {
    if [kAXButtonRole, kAXRadioButtonRole, kAXCheckBoxRole, kAXSliderRole,
        "AXSwitch", "AXStepper", kAXMenuButtonRole].contains(role) {
        return .notApplicable
    }
    guard role == kAXTextFieldRole || role == kAXTextAreaRole,
          focusStable, let before, let after, before.valid, after.valid,
          before == after else { return .unknown }
    let proof = SourceCompositionProof(kind: .axTextInputMarkedRange, focused_instance: focusedInstance,
        verified_source_sequence: 0, verified_observed_ms: 0)
    return after.length > 0 ? .marked(proof) : .unmarked(proof)
}

func focusOriginWindow(request: NativeRequest, app: AppIdentity) -> String? {
    guard request.session_epoch.map({ $0 > 0 }) == true else { return nil }
    return menuOriginWindow(request: request, app: app)
}

/// Missing AXFocused is unknown, not the current focus's identity.
func strictFocusBoolean(_ raw: CFTypeRef?) -> Bool? {
    guard let raw, CFGetTypeID(raw) == CFBooleanGetTypeID() else { return nil }
    return CFBooleanGetValue(unsafeDowncast(raw, to: CFBoolean.self))
}

func focusOptionalStringMatches(error: AXError, value: CFTypeRef?, expected: String?) -> Bool {
    if error == .noValue || error == .attributeUnsupported { return expected == nil }
    guard error == .success, let value, CFGetTypeID(value) == CFStringGetTypeID(),
          let text = value as? String, text.utf8.count <= 512 else { return false }
    return text == expected
}

func uniqueFocusProjection(_ focused: CFTypeRef, observed: [(CFTypeRef, String)]) -> String? {
    let matches = observed.filter { CFEqual($0.0, focused) }
    return matches.count == 1 ? matches[0].1 : nil
}

func focusReadEligible(_ structure: Structure, origin: String) -> Bool {
    structure.complete && structure.active_layer == origin && structure.menu_context == nil &&
        !structure.objects.contains { [kAXMenuRole, kAXMenuItemRole, kAXMenuBarRole, kAXSheetRole, "AXDialog", "AXPopover"].contains($0.role) }
}

/// Optional focus RPCs share the original read budget. No failure here alters
/// the underlying privacy/read completeness or repairs a missing AX fact.
private struct FocusAXQueries {
    let deadline: UInt64
    func configure(_ element: AXUIElement) -> Bool {
        let now = uptimeNs()
        guard now < deadline else { return false }
        return AXUIElementSetMessagingTimeout(element, Float(min(0.04, Double(deadline - now) / 1_000_000_000))) == .success && uptimeNs() < deadline
    }
    func value(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard configure(element) else { return nil }
        var result: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &result) == .success, uptimeNs() < deadline else { return nil }
        return result
    }
    func textInputMarkedRange(_ element: AXUIElement) -> TextInputMarkedRange? {
        guard configure(element) else { return nil }
        var result: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element,
            NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue as CFString, &result)
        return decodeTextInputMarkedRange(error: error, value: result, withinDeadline: uptimeNs() < deadline)
    }
    func string(_ element: AXUIElement, _ name: String) -> String? {
        guard let raw = value(element, name), CFGetTypeID(raw) == CFStringGetTypeID(),
              let text = raw as? String, text.utf8.count <= 512 else { return nil }
        return text
    }
    func stringMatches(_ element: AXUIElement, _ name: String, expected: String?) -> Bool {
        guard configure(element) else { return false }
        var result: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &result)
        return uptimeNs() < deadline && focusOptionalStringMatches(error: error, value: result, expected: expected)
    }
    func element(_ owner: AXUIElement, _ name: String) -> AXUIElement? {
        guard let raw = value(owner, name), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        return unsafeDowncast(raw, to: AXUIElement.self)
    }
    func samePID(_ element: AXUIElement, _ expected: UInt32) -> Bool {
        guard configure(element) else { return false }
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success && pid > 0 && UInt32(pid) == expected && uptimeNs() < deadline
    }
    func frame(_ element: AXUIElement) -> Rect? {
        guard let rawPosition = value(element, kAXPositionAttribute), CFGetTypeID(rawPosition) == AXValueGetTypeID(),
              let rawSize = value(element, kAXSizeAttribute), CFGetTypeID(rawSize) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero; var size = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(rawPosition, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(rawSize, to: AXValue.self), .cgSize, &size) else { return nil }
        let rect = Rect(CGRect(origin: point, size: size))
        return rect.valid && rect.width > 0 && rect.height > 0 ? rect : nil
    }
    func uniqueChild(_ child: AXUIElement, of parent: AXUIElement) -> Bool {
        guard let actualParent = element(child, kAXParentAttribute), CFEqual(actualParent, parent), configure(parent) else { return false }
        var count = 0
        guard AXUIElementGetAttributeValueCount(parent, kAXChildrenAttribute as CFString, &count) == .success,
              count > 0, count <= 512, configure(parent) else { return false }
        var values: CFArray?
        guard AXUIElementCopyAttributeValues(parent, kAXChildrenAttribute as CFString, 0, count, &values) == .success,
              let children = values as? [AXUIElement], children.count == count, uptimeNs() < deadline else { return false }
        return children.filter { CFEqual($0, child) }.count == 1
    }
}

struct FocusReadStart {
    let application: MenuRetainedToken
    let window: MenuRetainedToken
    let focused: MenuRetainedToken
    let frame: Rect
    let process: ProcessStamp
}

func sameFocusBoundary(_ before: FocusReadStart, _ after: FocusReadStart) -> Bool {
    before.process == after.process && before.frame == after.frame &&
        before.application.matches(after.application.element) && before.window.matches(after.window.element) &&
        before.focused.matches(after.focused.element)
}

/// References remain inside one synchronous AX worker/read. Focus has no
/// cross-read retained anchor and cannot survive by a label or geometry match.
final class FocusContextReader {
    let sessionID: String
    init(sessionID: String) { self.sessionID = sessionID }

    func begin(app: AppIdentity, request: NativeRequest, process: ProcessStamp, deadline: UInt64) -> FocusReadStart? {
        guard focusOriginWindow(request: request, app: app) != nil,
              ProcessStamp.read(pid: app.pid) == process else { return nil }
        let query = FocusAXQueries(deadline: deadline)
        let system = AXUIElementCreateSystemWide()
        let application = AXUIElementCreateApplication(pid_t(app.pid))
        guard let active = query.element(system, kAXFocusedApplicationAttribute), CFEqual(active, application),
              query.samePID(active, app.pid), query.string(active, kAXRoleAttribute) == kAXApplicationRole,
              let window = query.element(active, kAXFocusedWindowAttribute), query.samePID(window, app.pid),
              query.string(window, kAXRoleAttribute) == kAXWindowRole,
              query.string(window, kAXSubroleAttribute) == kAXStandardWindowSubrole,
              strictFocusBoolean(query.value(window, kAXModalAttribute)) == false,
              let frame = query.frame(window),
              let focused = query.element(active, kAXFocusedUIElementAttribute), query.samePID(focused, app.pid),
              let systemFocused = query.element(system, kAXFocusedUIElementAttribute), CFEqual(focused, systemFocused),
              strictFocusBoolean(query.value(focused, kAXFocusedAttribute)) == true,
              ProcessStamp.read(pid: app.pid) == process, uptimeNs() < deadline else { return nil }
        return FocusReadStart(application: MenuRetainedToken(active), window: MenuRetainedToken(window),
            focused: MenuRetainedToken(focused), frame: frame, process: process)
    }

    func project(before: FocusReadStart?, reader: AXReader, structure: Structure, process: ProcessStamp) -> FocusContext? {
        guard let before, before.process == process,
              let origin = focusOriginWindow(request: reader.request, app: reader.app), focusReadEligible(structure, origin: origin),
              let epoch = reader.request.session_epoch, epoch > 0,
              let window = structure.windows.first(where: { $0.window_id == origin }), !window.modal, window.frame == before.frame,
              let id = uniqueFocusProjection(before.focused.element, observed: reader.observedElements.map { ($0.element, $0.id) }),
              let target = structure.objects.first(where: { $0.instance_ref == id }),
              target.window_id == origin, target.focused, target.enabled, target.in_view, !target.secure,
              target.parent != nil, window.frame.cgRect.contains(target.frame.cgRect),
              ![kAXWindowRole, kAXApplicationRole, kAXMenuRole, kAXMenuItemRole, kAXMenuBarRole, kAXSheetRole, "AXDialog", "AXPopover"].contains(target.role),
              let fingerprint = menuFingerprint(object: target, structure: structure) else { return nil }
        let query = FocusAXQueries(deadline: reader.deadline)
        func actual(_ object: UiObject) -> AXUIElement? { reader.observedElements.first { $0.id == object.instance_ref }?.element }
        func pathMatches() -> Bool {
            var cursor = target
            var visited = Set<String>()
            for _ in 0..<32 {
                guard visited.insert(cursor.instance_ref).inserted, cursor.window_id == origin, !cursor.secure,
                      let ref = actual(cursor), query.samePID(ref, reader.app.pid),
                      query.string(ref, kAXRoleAttribute) == cursor.role,
                      query.stringMatches(ref, kAXSubroleAttribute, expected: cursor.subrole),
                      query.frame(ref) == cursor.frame,
                      query.stringMatches(ref, kAXTitleAttribute, expected: cursor.title),
                      query.stringMatches(ref, kAXDescriptionAttribute, expected: cursor.description) else { return false }
                if cursor.role == kAXWindowRole {
                    return cursor.parent == nil && cursor.subrole == kAXStandardWindowSubrole &&
                        cursor.frame == window.frame && before.window.matches(ref)
                }
                guard ![kAXApplicationRole, kAXMenuRole, kAXMenuItemRole, kAXMenuBarRole, kAXSheetRole, "AXDialog", "AXPopover"].contains(cursor.role),
                      let parentID = cursor.parent, let parent = structure.objects.first(where: { $0.instance_ref == parentID }),
                      let parentRef = actual(parent), query.uniqueChild(ref, of: parentRef) else { return false }
                cursor = parent
            }
            return false
        }
        let focused = unsafeDowncast(before.focused.element, to: AXUIElement.self)
        guard let targetRef = actual(target), before.focused.matches(targetRef), pathMatches(),
              strictFocusBoolean(query.value(focused, kAXFocusedAttribute)) == true,
              strictFocusBoolean(query.value(focused, kAXEnabledAttribute)) == true,
              let actualWindow = query.element(focused, kAXWindowAttribute), before.window.matches(actualWindow),
              let top = query.element(focused, kAXTopLevelUIElementAttribute), before.window.matches(top) else { return nil }
        let isTextRole = target.role == kAXTextFieldRole || target.role == kAXTextAreaRole
        let markedRangeBefore = isTextRole ? query.textInputMarkedRange(focused) : nil
        guard let after = begin(app: reader.app, request: reader.request, process: process, deadline: reader.deadline),
              sameFocusBoundary(before, after), pathMatches() else { return nil }
        let markedRangeAfter = (target.role == kAXTextFieldRole || target.role == kAXTextAreaRole)
            ? query.textInputMarkedRange(focused) : nil
        guard let final = begin(app: reader.app, request: reader.request, process: process, deadline: reader.deadline),
              sameFocusBoundary(before, final) else { return nil }
        return FocusContext(source_session_id: sessionID, session_epoch: epoch,
            process: KernelProcessIdentity(pid: reader.app.pid, start_seconds: process.seconds, start_micros: process.micros),
            origin_window_id: origin, origin_window_frame: window.frame, target_instance: id,
            target_fingerprint: fingerprint, target_frame: target.frame, focused_instance: id, relation: .exactControl,
            composition: stableTextComposition(role: target.role, focusedInstance: id,
                before: markedRangeBefore, after: markedRangeAfter, focusStable: sameFocusBoundary(before, after) && sameFocusBoundary(before, final)),
            keyboard: .unknown,
            verified_source_sequence: 0, verified_observed_ms: 0)
    }
}

func runFocusContextTests() throws -> [String] {
    let app = AppIdentity(bundle_id: "com.lingxi.computer-user.guard-fixture", name: "Offline", pid: 42)
    let rootFrame = Rect(CGRect(x: 20, y: 30, width: 160, height: 100))
    let targetFrame = Rect(CGRect(x: 30, y: 40, width: 80, height: 25))
    let focused = "offline-current-focused-control" as CFString
    try requireTest(strictFocusBoolean(kCFBooleanTrue) == true && strictFocusBoolean(kCFBooleanFalse) == false &&
                    strictFocusBoolean(nil) == nil && strictFocusBoolean(NSNumber(value: Int32(0))) == nil,
                    "focus boolean must be actual CFBoolean; missing is not false identity")
    try requireTest(focusOptionalStringMatches(error: .noValue, value: nil, expected: nil) &&
                    !focusOptionalStringMatches(error: .cannotComplete, value: nil, expected: nil) &&
                    !focusOptionalStringMatches(error: .success, value: NSNumber(value: 0), expected: nil),
                    "unavailable optional descriptor never matches a genuinely absent label or subrole")
    try requireTest(uniqueFocusProjection(focused, observed: [(focused, "actual")]) == "actual" &&
                    uniqueFocusProjection(focused, observed: [("same-label-field-editor" as CFString, "field")]) == nil &&
                    uniqueFocusProjection(focused, observed: [(focused, "first"), (focused, "second")]) == nil,
                    "focus projection is unique actual CF identity, never field-editor label matching")
    let process = ProcessStamp(seconds: 10, micros: 20)
    let start = FocusReadStart(application: MenuRetainedToken("app" as CFString), window: MenuRetainedToken("root" as CFString),
        focused: MenuRetainedToken(focused), frame: rootFrame, process: process)
    for change in ["process", "application", "window", "target", "frame"] {
        let candidate = FocusReadStart(application: MenuRetainedToken((change == "application" ? "other-app" : "app") as CFString),
            window: MenuRetainedToken((change == "window" ? "other-root" : "root") as CFString),
            focused: MenuRetainedToken(change == "target" ? "replacement-control" as CFString : focused),
            frame: change == "frame" ? targetFrame : rootFrame,
            process: change == "process" ? ProcessStamp(seconds: 11, micros: 20) : process)
        try requireTest(!sameFocusBoundary(start, candidate), "focus pre/post rejects changed \(change)")
    }
    try requireTest(sameFocusBoundary(start, start), "stable actual focus boundary matches")
    func request(command: String, epoch: String) throws -> NativeRequest {
        try JSONDecoder().decode(NativeRequest.self, from: Data("{\"protocol_version\":1,\"command\":\"\(command)\",\"pid\":42,\"session_epoch\":\(epoch),\"text_policy\":{\"kind\":\"registered_editor\",\"window_id\":\"window-1\"}}".utf8))
    }
    let authorizedWatch = try request(command: "watch", epoch: "7")
    let zeroEpochWatch = try request(command: "watch", epoch: "0")
    let oneShot = try request(command: "observe", epoch: "7")
    try requireTest(focusOriginWindow(request: authorizedWatch, app: app) == "window-1" &&
                    focusOriginWindow(request: zeroEpochWatch, app: app) == nil &&
                    focusOriginWindow(request: oneShot, app: app) == nil,
                    "focus needs trusted fixed watch scope and actual positive session epoch")
    try requireTest(focusComposition(role: kAXCheckBoxRole) == .notApplicable &&
                    focusComposition(role: kAXTextFieldRole) == .unknown && focusComposition(role: kAXTextAreaRole) == .unknown &&
                    focusComposition(role: "AXUnknown") == .unknown,
                    "text and unknown roles cannot borrow a nontext or unmarked claim")
    let text = try JSONSerialization.jsonObject(with: JSONEncoder().encode(focusComposition(role: kAXTextFieldRole))) as! [String: Any]
    try requireTest(text["kind"] as? String == "unknown" && text.count == 1, "unknown composition JSON is exact contract")
    var markedRange = CFRange(location: 16, length: 2)
    guard let markedValue = AXValueCreate(.cfRange, &markedRange) else { throw NativeFailure("offline CFRange AXValue unavailable") }
    var unmarkedRange = CFRange(location: 18, length: 0)
    guard let unmarkedValue = AXValueCreate(.cfRange, &unmarkedRange) else { throw NativeFailure("offline empty CFRange AXValue unavailable") }
    let decodedMarked = decodeTextInputMarkedRange(error: .success, value: markedValue, withinDeadline: true)
    let decodedUnmarked = decodeTextInputMarkedRange(error: .success, value: unmarkedValue, withinDeadline: true)
    try requireTest(decodedMarked == TextInputMarkedRange(markedRange) && decodedUnmarked == TextInputMarkedRange(unmarkedRange),
                    "public AX marked-range decode requires typed CFRange")
    var point = CGPoint(x: 1, y: 2)
    guard let wrongAXType = AXValueCreate(.cgPoint, &point) else { throw NativeFailure("offline CGPoint AXValue unavailable") }
    var negativeRange = CFRange(location: -1, length: 1)
    guard let negativeValue = AXValueCreate(.cfRange, &negativeRange) else { throw NativeFailure("offline negative CFRange AXValue unavailable") }
    var overflowingRange = CFRange(location: CFIndex.max, length: 1)
    guard let overflowingValue = AXValueCreate(.cfRange, &overflowingRange) else { throw NativeFailure("offline overflowing CFRange AXValue unavailable") }
    try requireTest(decodeTextInputMarkedRange(error: .attributeUnsupported, value: markedValue, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .failure, value: markedValue, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .noValue, value: nil, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .success, value: "not-an-AXValue" as CFString, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .success, value: wrongAXType, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .success, value: negativeValue, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .success, value: overflowingValue, withinDeadline: true) == nil &&
                    decodeTextInputMarkedRange(error: .success, value: markedValue, withinDeadline: false) == nil,
                    "AX errors, wrong CF types, invalid range arithmetic and expired original deadline remain unknown")
    let positiveComposition = stableTextComposition(role: kAXTextAreaRole, focusedInstance: "target",
        before: decodedMarked, after: decodedMarked, focusStable: true)
    let emptyComposition = stableTextComposition(role: kAXTextAreaRole, focusedInstance: "target",
        before: decodedUnmarked, after: decodedUnmarked, focusStable: true)
    try requireTest({ if case .marked = positiveComposition { return true }; return false }() &&
                    { if case .unmarked = emptyComposition { return true }; return false }() &&
                    stableTextComposition(role: kAXTextFieldRole, focusedInstance: "target", before: decodedMarked,
                        after: decodedUnmarked, focusStable: true) == .unknown &&
                    stableTextComposition(role: kAXTextAreaRole, focusedInstance: "target", before: decodedMarked,
                        after: decodedMarked, focusStable: false) == .unknown &&
                    stableTextComposition(role: "AXUnknown", focusedInstance: "target", before: decodedMarked,
                        after: decodedMarked, focusStable: true) == .unknown,
                    "only a stable exact-focus text receiver's identical public range yields marked or unmarked")
    let compositionWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(positiveComposition)) as! [String: Any]
    let compositionProof = compositionWire["proof"] as? [String: Any]
    try requireTest(compositionWire["kind"] as? String == "marked" &&
                    compositionProof?["kind"] as? String == "ax_text_input_marked_range" &&
                    compositionProof?["focused_instance"] as? String == "target" &&
                    compositionProof?["verified_source_sequence"] as? Int == 0,
                    "composition proof wire matches the canonical Source contract before publication stamping")
    var structure = Structure(app: app)
    structure.complete = true; structure.active_layer = "window-1"
    structure.windows = [WindowInfo(window_id: "window-1", title: "Offline", frame: rootFrame, z_index: 0,
        kind: "window", modal: false, owner_bundle_id: app.bundle_id)]
    let root = UiObject(instance_ref: "root", parent: nil, window_id: "window-1", role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, title: "Offline", description: nil, value: nil, enabled: true,
        focused: false, frame: rootFrame, actions: [], in_view: true, group: nil, secure: false)
    let target = UiObject(instance_ref: "target", parent: "root", window_id: "window-1", role: kAXCheckBoxRole,
        subrole: nil, title: "Control", description: nil, value: .bool(false), enabled: true,
        focused: true, frame: targetFrame, actions: ["press"], in_view: true, group: nil, secure: false)
    structure.objects = [root, target]
    try requireTest(focusReadEligible(structure, origin: "window-1"), "normal complete focus read is eligible")
    for change in ["partial", "layer", "menu", "item", "sheet", "dialog"] {
        var changed = structure
        switch change {
        case "partial": changed.complete = false
        case "layer": changed.active_layer = "other-window"
        default:
            let role = ["menu": kAXMenuRole, "item": kAXMenuItemRole, "sheet": kAXSheetRole, "dialog": "AXDialog"][change]!
            changed.objects.append(UiObject(instance_ref: "transient", parent: "root", window_id: "window-1", role: role,
                subrole: nil, title: nil, description: nil, value: nil, enabled: true, focused: false,
                frame: targetFrame, actions: [], in_view: true, group: nil, secure: false))
        }
        try requireTest(!focusReadEligible(changed, origin: "window-1"), "focus cannot repair \(change) Source")
    }
    let fingerprint = menuFingerprint(object: target, structure: structure)!
    structure.focus_context = FocusContext(source_session_id: "offline-source-session", session_epoch: 7,
        process: KernelProcessIdentity(pid: 42, start_seconds: 10, start_micros: 20), origin_window_id: "window-1",
        origin_window_frame: rootFrame, target_instance: "target", target_fingerprint: fingerprint, target_frame: targetFrame,
        focused_instance: "target", relation: .exactControl, composition: .notApplicable, keyboard: .unknown,
        verified_source_sequence: 1, verified_observed_ms: 2)
    structure.normalizeSourceProvenance(sequence: 9, observedMS: 10)
    try requireTest(structure.focus_context?.verified_source_sequence == 9 && structure.focus_context?.verified_observed_ms == 10 &&
                    structure.focus_context?.session_epoch == 7, "focus nested provenance follows one Source publication without fabricating epoch")
    var proofContext = structure.focus_context!
    proofContext.composition = .marked(SourceCompositionProof(kind: .axTextInputMarkedRange,
        focused_instance: "target", verified_source_sequence: 0, verified_observed_ms: 0))
    structure.focus_context = proofContext
    structure.normalizeSourceProvenance(sequence: 11, observedMS: 12)
    let stampedProof: SourceCompositionProof?
    if case .marked(let proof) = structure.focus_context?.composition { stampedProof = proof }
    else { stampedProof = nil }
    try requireTest(stampedProof?.focused_instance == "target" && stampedProof?.verified_source_sequence == 11 &&
                    stampedProof?.verified_observed_ms == 12 && structure.focus_context?.verified_source_sequence == 11 &&
                    structure.focus_context?.verified_observed_ms == 12,
                    "composition proof is stamped with the same actual Source publication as focus")
    try requireTest(structure.focus_context?.keyboard.selected_input_source == nil && structure.focus_context?.keyboard.underlying_keyboard_layout == nil,
                    "selected and underlying keyboard metadata remain independently unknown")
    return ["focus_strict_boolean_and_missing_unknown", "focus_query_failure_is_not_absent_descriptor", "focus_unique_actual_cf_and_field_editor_rejection",
            "focus_pre_post_actual_process_root_target", "focus_trusted_watch_epoch_scope",
            "focus_text_composition_requires_stable_public_range", "focus_public_cf_range_composition_positive_negative_fail_closed",
            "focus_partial_menu_modal_fail_closed", "focus_nested_publication_provenance",
            "focus_keyboard_unknown_never_guesses_layout"]
}
