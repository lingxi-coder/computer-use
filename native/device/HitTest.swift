import AppKit
import ApplicationServices
import Darwin

struct DeviceProcessStamp: Equatable, Sendable {
    let seconds: UInt64, micros: UInt64
    static func read(_ pid: Int32) -> DeviceProcessStamp? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard pid > 0, proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return DeviceProcessStamp(seconds: info.pbi_start_tvsec, micros: info.pbi_start_tvusec)
    }
}

struct HitRect: Codable, Sendable, Equatable {
    let x: Double, y: Double, width: Double, height: Double
    var rect: CGRect { CGRect(x: x, y: y, width: width, height: height) }
    var valid: Bool {
        [x, y, width, height].allSatisfy { $0.isFinite && abs($0) <= 100_000 } && width > 0 && height > 0
    }
    func contains(x: Double, y: Double) -> Bool { x >= self.x && x < self.x + width && y >= self.y && y < self.y + height }
    var rounded: [Int64] { [x, y, width, height].map { Int64($0.rounded(.toNearestOrAwayFromZero)) } }
    func matches(_ other: HitRect) -> Bool {
        zip([x, y, width, height], [other.x, other.y, other.width, other.height]).allSatisfy { abs($0 - $1) < 0.5 }
    }
    init(_ rect: CGRect) { x = rect.minX; y = rect.minY; width = rect.width; height = rect.height }
}

enum OptionalAXTextReadStatus: Equatable {
    case value(String)
    case absent
    case unavailable
}

func optionalAXTextReadStatus(error: AXError, value: CFTypeRef?, timely: Bool) -> OptionalAXTextReadStatus {
    guard timely else { return .unavailable }
    if error == .noValue || error == .attributeUnsupported {
        return value == nil ? .absent : .unavailable
    }
    guard error == .success, let value, CFGetTypeID(value) == CFStringGetTypeID(),
          let text = value as? String, text.utf8.count <= 512 else { return .unavailable }
    return .value(text)
}

struct HitRequest: Decodable, Sendable {
    let request_id: String
    let pid: Int32
    let bundle_id: String
    let window_id: UInt32
    let x: Double, y: Double
    let frame: HitRect
    let binding: NativeBindingFingerprint
    let binding_digest: String
    let contextWitness: NativeDeviceContextWitness
    var valid: Bool {
        validFor(action: "ax_press")
    }
    func validFor(action: String) -> Bool {
        !request_id.isEmpty && request_id.utf8.count <= 128 && pid > 0 && window_id > 0 &&
        bundle_id.utf8.count <= 256 && !bundle_id.isEmpty && binding.valid &&
        contextWitness.valid && contextWitness.supportsNativeInput(action: action) &&
        contextWitness.supportsAction(action) &&
        contextWitness.matchesBinding(binding, displayFrame: frame) &&
        binding.app == bundle_id && binding.app_pid == UInt32(pid) && binding.window_id == "window-\(window_id)" &&
        frame.valid && binding.frame == frame.rounded && binding.digest == binding_digest &&
        Self.roles(action: action).contains(binding.role) && x.isFinite && y.isFinite && abs(x) <= 100_000 && abs(y) <= 100_000 &&
        inputPointMatchesWitness(action: action) &&
        frame.contains(x: x, y: y)
    }
    private func inputPointMatchesWitness(action: String) -> Bool {
        guard action == "ax_set_selected_true" else { return true }
        guard let context = contextWitness.tableViewport else { return false }
        return context.hit_point.x == x && context.hit_point.y == y
    }
    // Diagnostic hit tests do not read editable or secure field labels/values.
    static let allowedRoles: Set<String> = [kAXButtonRole, kAXCheckBoxRole, kAXRadioButtonRole, kAXMenuItemRole]
    static func roles(action: String) -> Set<String> {
        if action == "ax_menu_press" { return [kAXMenuItemRole] }
        if action == "pointer_move" { return allowedRoles.union([kAXScrollAreaRole]) }
        if action == "scroll" { return [kAXScrollAreaRole] }
        if action == "ax_show_menu" { return [kAXMenuButtonRole, kAXPopUpButtonRole] }
        if action == "ax_set_value" { return [kAXCheckBoxRole, kAXRadioButtonRole, kAXTextFieldRole, kAXTextAreaRole] }
        if action == "click" { return allowedRoles.union([kAXTextFieldRole, kAXTextAreaRole]) }
        if action == "ax_set_selected_true" { return [kAXRowRole] }
        return allowedRoles
    }
}

struct HitResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "hit_test"
    let session_epoch: UInt64
    let request_id: String
    var outcome: String
    var reason: String
    var binding_digest: String?
    let input_authorized = false
}

func registeredSheetModalAttributeKnownNonTrue(error: Int32, typeID: CFTypeID?, modal: Bool?, timely: Bool) -> Bool {
    guard timely else { return false }
    if error == AXError.attributeUnsupported.rawValue || error == AXError.noValue.rawValue {
        return typeID == nil && modal == nil
    }
    return error == AXError.success.rawValue && typeID == CFBooleanGetTypeID() && modal == false
}

func registeredSheetChildRolesKnown(_ roles: [String?]) -> Bool {
    !roles.isEmpty && roles.count <= 256 && roles.allSatisfy { $0 != nil && $0 != "" }
        && roles.filter { $0 == kAXSheetRole }.count == 1
        && !roles.contains { ["AXDialog", "AXPopover", kAXMenuRole].contains($0 ?? "") }
}

/// Dedicated TableViewportV1 comparator. It accepts only a current system-wide
/// point query that directly returns the AXRow. It never promotes a cell or
/// text child to its row ancestor and never uses the generic sibling-closure
/// reader for a partial viewport.
private func diagnosticRowHitTest(_ request: HitRequest, epoch: UInt64,
                                  deadlineMS: UInt64,
                                  onMatched: ((AXUIElement) -> Void)?) -> HitResponse {
    func result(_ outcome: String, _ reason: String) -> HitResponse {
        HitResponse(session_epoch: epoch, request_id: request.request_id,
            outcome: outcome, reason: reason)
    }
    guard request.validFor(action: "ax_set_selected_true"), AXIsProcessTrusted(),
          let context = request.contextWitness.tableViewport,
          let app = NSRunningApplication(processIdentifier: request.pid),
          app.bundleIdentifier == request.bundle_id, !app.isTerminated,
          let stamp = DeviceProcessStamp.read(request.pid) else {
        return result("context_unresolved", "row request, AX permission, or process identity unavailable")
    }
    let deadline = min(monotonicMS() + 150, deadlineMS)
    let system = AXUIElementCreateSystemWide()
    guard AXUIElementSetMessagingTimeout(system, 0.025) == .success else {
        return result("context_unresolved", "row point-query timeout unavailable")
    }

    func hitAtWitnessPoint() -> AXUIElement? {
        guard monotonicMS() < deadline else { return nil }
        var hit: AXUIElement?
        guard AXUIElementCopyElementAtPosition(system, Float(context.hit_point.x),
            Float(context.hit_point.y), &hit) == .success,
              let hit, AXUIElementSetMessagingTimeout(hit, 0.025) == .success else { return nil }
        return hit
    }

    func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard monotonicMS() < deadline,
              AXUIElementSetMessagingTimeout(element, 0.025) == .success,
              monotonicMS() < deadline else { return nil }
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success
            ? value : nil
    }

    func directRow(_ candidate: AXUIElement) -> Bool {
        var owner: pid_t = 0
        guard monotonicMS() < deadline,
              AXUIElementGetPid(candidate, &owner) == .success, owner == request.pid,
              let role = attribute(candidate, kAXRoleAttribute) as? String,
              role == kAXRowRole,
              let subrole = attribute(candidate, kAXSubroleAttribute) as? String,
              subrole == "AXTableRow" else { return false }
        return monotonicMS() < deadline
    }

    func frame(_ element: AXUIElement) -> HitRect? {
        guard let rawPosition = attribute(element, kAXPositionAttribute),
              CFGetTypeID(rawPosition) == AXValueGetTypeID(),
              let rawSize = attribute(element, kAXSizeAttribute),
              CFGetTypeID(rawSize) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        var size = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(rawPosition, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(rawSize, to: AXValue.self), .cgSize, &size) else { return nil }
        let result = HitRect(CGRect(origin: point, size: size))
        return result.valid ? result : nil
    }

    func selectedCGWindow(_ windowFrame: HitRect) -> Bool {
        guard monotonicMS() < deadline,
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                  kCGNullWindowID) as? [[String: Any]] else { return false }
        let menuVisible = windows.contains {
            ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid
                && ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 101
                && (($0[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 0) > 0
        }
        guard !menuVisible else { return false }
        let matches = windows.filter {
            guard ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid,
                  ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
                  let bounds = $0[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
            return HitRect(rect).matches(windowFrame)
        }
        return matches.count == 1
            && (matches[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == request.window_id
            && monotonicMS() < deadline
    }

    func focusedWindowIs(_ window: AXUIElement) -> Bool {
        guard app.isActive, monotonicMS() < deadline,
              let focusedApp = attribute(system, kAXFocusedApplicationAttribute),
              CFGetTypeID(focusedApp) == AXUIElementGetTypeID() else { return false }
        let focusedApplication = unsafeDowncast(focusedApp, to: AXUIElement.self)
        var focusedPID: pid_t = 0
        guard AXUIElementSetMessagingTimeout(focusedApplication, 0.025) == .success,
              AXUIElementGetPid(focusedApplication, &focusedPID) == .success,
              focusedPID == request.pid,
              let focusedWindow = attribute(focusedApplication, kAXFocusedWindowAttribute),
              CFGetTypeID(focusedWindow) == AXUIElementGetTypeID(),
              CFEqual(unsafeDowncast(focusedWindow, to: AXUIElement.self), window) else { return false }
        return monotonicMS() < deadline
    }

    guard let row = hitAtWitnessPoint(), directRow(row) else {
        return result("context_unresolved", "witness point did not directly hit the current AXRow")
    }
    guard let parentValue = attribute(row, kAXParentAttribute),
          CFGetTypeID(parentValue) == AXUIElementGetTypeID(),
          let ownerWindowValue = attribute(row, kAXWindowAttribute),
          CFGetTypeID(ownerWindowValue) == AXUIElementGetTypeID() else {
        return result("context_unresolved", "point-hit row parent or selected window unavailable")
    }
    let table = unsafeDowncast(parentValue, to: AXUIElement.self)
    let window = unsafeDowncast(ownerWindowValue, to: AXUIElement.self)
    guard attribute(table, kAXRoleAttribute) as? String == kAXTableRole,
          let windowFrame = frame(window), selectedCGWindow(windowFrame),
          focusedWindowIs(window),
          let modalValue = attribute(window, kAXModalAttribute),
          CFGetTypeID(modalValue) == CFBooleanGetTypeID(),
          let modal = (modalValue as? NSNumber)?.boolValue,
          (context.window.kind == .window && !modal && !context.window.modal)
              || (context.window.kind == .sheet && modal && context.window.modal) else {
        return result("context_unresolved", "selected active AX window, modal state, or table changed")
    }
    let firstReader = NativeRowBindingReader(pid: request.pid, bundle: request.bundle_id,
        selectedWindow: window, table: table, deadline: deadline, pointHitRow: row)
    guard let first = firstReader.read(context: context, binding: request.binding) else {
        return result("context_unresolved", firstReader.failureReason)
    }
    let secondReader = NativeRowBindingReader(pid: request.pid, bundle: request.bundle_id,
        selectedWindow: window, table: table, deadline: deadline, pointHitRow: row)
    guard let second = secondReader.read(context: context, binding: request.binding),
          first.sameWorkerGraph(as: second) else {
        return result("context_unresolved", secondReader.failureReason == "row context unavailable"
            ? "independent row context reads differ" : secondReader.failureReason)
    }

    // After both reads, query the same proof-bound point again. AXRow equality
    // is local to this Device worker and is never serialized or compared with
    // Source handles.
    var finalSettable = DarwinBoolean(false)
    guard let finalHit = hitAtWitnessPoint(), directRow(finalHit), CFEqual(finalHit, row),
          AXUIElementSetMessagingTimeout(finalHit, 0.025) == .success,
          let finalOwner = attribute(finalHit, kAXWindowAttribute),
          CFGetTypeID(finalOwner) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(finalOwner, to: AXUIElement.self), window),
          let finalParent = attribute(finalHit, kAXParentAttribute),
          CFGetTypeID(finalParent) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(finalParent, to: AXUIElement.self), table),
          let finalIndex = attribute(finalHit, kAXIndexAttribute),
          NativeRowBindingValidation.rowAXIndex(finalIndex) == context.target_row_ax_index,
          let finalSelected = attribute(finalHit, kAXSelectedAttribute),
          CFGetTypeID(finalSelected) == CFBooleanGetTypeID(),
          (finalSelected as? NSNumber)?.boolValue == false,
          readNativeAXEnabledObservation(table, deadline: deadline) == context.table_ax_enabled,
          context.table_ax_enabled.permitsRowSelection,
          readNativeAXEnabledObservation(finalHit, deadline: deadline) == context.target_ax_enabled,
          context.target_ax_enabled.permitsRowSelection,
          AXUIElementIsAttributeSettable(finalHit, kAXSelectedAttribute as CFString,
              &finalSettable) == .success, finalSettable.boolValue,
          let finalRowFrame = frame(finalHit), finalRowFrame == context.target.object.frame,
          let finalTableFrame = frame(table), finalTableFrame == context.table.frame,
          let finalFrame = frame(window), finalFrame == context.window.frame,
          selectedCGWindow(finalFrame), focusedWindowIs(window),
          monotonicMS() < deadline, AXIsProcessTrusted(),
          let finalApp = NSRunningApplication(processIdentifier: request.pid),
          !finalApp.isTerminated, finalApp.bundleIdentifier == request.bundle_id,
          DeviceProcessStamp.read(request.pid) == stamp else {
        return result("context_unresolved", "point-hit row or active window changed after independent reads")
    }
    onMatched?(row)
    var matched = result("matched", "complete local row selector and active window matched; no input authorization")
    matched.binding_digest = request.binding_digest
    return matched
}


// Prove the registered parent's current tree has exactly the selected sheet.
// Only structural attributes are read; labels and editable values stay private.
private func registeredSheetTreeKnown(root: AXUIElement, sheet: AXUIElement,
                                      pid: Int32, deadline: UInt64, failure: inout String) -> Bool {
    var seen: [AXUIElement] = []
    var sheetCount = 0
    func raw(_ element: AXUIElement, _ key: String) -> (AXError, CFTypeRef?) {
        guard monotonicMS() < deadline else { return (.cannotComplete, nil) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, key as CFString, &value)
        return monotonicMS() < deadline ? (error, value) : (.cannotComplete, nil)
    }
    func walk(_ element: AXUIElement, depth: Int) -> Bool {
        var owner: pid_t = 0
        guard monotonicMS() < deadline, depth <= 12, seen.count < 128,
              !seen.contains(where: { CFEqual($0, element) }),
              AXUIElementGetPid(element, &owner) == .success, owner == pid else {
            failure = "registered sheet tree budget or process unresolved"; return false
        }
        seen.append(element)
        let (roleError, roleRaw) = raw(element, kAXRoleAttribute)
        guard roleError == .success, let role = roleRaw as? String, !role.isEmpty,
              !["AXDialog", "AXPopover", kAXMenuRole].contains(role) else {
            failure = "registered sheet tree role unresolved or transient"; return false
        }
        let (subroleError, subroleRaw) = raw(element, kAXSubroleAttribute)
        guard (subroleError == .success && (subroleRaw as? String) != nil)
                || ([AXError.attributeUnsupported, .noValue].contains(subroleError) && subroleRaw == nil),
              !["AXDialog", "AXPopover", kAXSheetRole].contains((subroleRaw as? String) ?? "") else {
            failure = "registered sheet tree subrole unresolved or transient"; return false
        }
        if role == kAXSheetRole {
            guard depth == 1, CFEqual(element, sheet) else {
                failure = "registered sheet tree has another sheet"; return false
            }
            sheetCount += 1
        }
        let (modalError, modalRaw) = raw(element, kAXModalAttribute)
        guard registeredSheetModalAttributeKnownNonTrue(error: modalError.rawValue,
            typeID: modalRaw.map(CFGetTypeID), modal: (modalRaw as? NSNumber)?.boolValue,
            timely: monotonicMS() < deadline) else {
            failure = "registered sheet tree modal attribute unresolved"; return false
        }
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count)
        guard monotonicMS() < deadline else { return false }
        if [AXError.attributeUnsupported, .noValue].contains(countError) { return true }
        guard countError == .success, count >= 0, count <= 128 - seen.count else {
            failure = "registered sheet tree child count unresolved"; return false
        }
        if count == 0 { return true }
        var childrenRaw: CFArray?
        guard AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, count, &childrenRaw) == .success,
              monotonicMS() < deadline, let children = childrenRaw as? [AXUIElement], children.count == count else {
            failure = "registered sheet tree children unresolved"; return false
        }
        return children.allSatisfy { walk($0, depth: depth + 1) }
    }
    return walk(root, depth: 0) && sheetCount == 1 && monotonicMS() < deadline
}

/// Constructed on one worker thread. A hit-tested AX ref never leaves that
/// thread. It checks all fp1 fields on a bounded ancestor path twice. Only the
/// worker-local onMatched closure can enter the separate ordered commit gate;
/// diagnostic replies never authorize a later action.
func diagnosticHitTest(_ request: HitRequest, epoch: UInt64, requireActive: Bool = false, deadlineMS: UInt64 = UInt64.max,
                       action: String = "ax_press",
                       menuAnchor: NativeMenuOpenerAnchor? = nil,
                       onMatched: ((AXUIElement) -> Void)? = nil) -> HitResponse {
    if action == "ax_set_selected_true" {
        return diagnosticRowHitTest(request, epoch: epoch, deadlineMS: deadlineMS,
            onMatched: onMatched)
    }
    func result(_ outcome: String, _ reason: String) -> HitResponse {
        HitResponse(session_epoch: epoch, request_id: request.request_id, outcome: outcome, reason: reason)
    }
    guard request.validFor(action: action), AXIsProcessTrusted() else { return result("context_unresolved", "invalid request or AX permission unavailable") }
    guard let app = NSRunningApplication(processIdentifier: request.pid), app.bundleIdentifier == request.bundle_id,
          !app.isTerminated else { return result("context_unresolved", "target application unavailable") }
    // Launch Services' launchDate can be nil for a directly launched fixture.
    // The kernel start stamp also detects PID reuse independently of AppKit.
    guard let stamp = DeviceProcessStamp.read(request.pid) else { return result("context_unresolved", "target process identity unavailable") }
    let deadline = min(monotonicMS() + 150, deadlineMS)
    let system = AXUIElementCreateSystemWide()
    guard AXUIElementSetMessagingTimeout(system, 0.025) == .success else { return result("context_unresolved", "AX timeout could not be set") }
    var element: AXUIElement?
    guard AXUIElementCopyElementAtPosition(system, Float(request.x), Float(request.y), &element) == .success,
          let element else { return result("context_unresolved", "point hit test unavailable") }
    var pid: pid_t = 0
    guard AXUIElementGetPid(element, &pid) == .success else { return result("context_unresolved", "hit process unavailable") }
    // Do not read titles or any other content from an occluding foreign app.
    guard pid == request.pid else { return result("target_occluded", "point belongs to another process") }
    func attribute(_ item: AXUIElement, _ key: String) -> CFTypeRef? {
        guard monotonicMS() < deadline else { return nil }
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(item, key as CFString, &value) == .success ? value : nil
    }
    func optionalText(_ item: AXUIElement, _ key: String) -> OptionalAXTextReadStatus {
        guard monotonicMS() < deadline else { return .unavailable }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(item, key as CFString, &value)
        return optionalAXTextReadStatus(error: error, value: value, timely: monotonicMS() < deadline)
    }
    func label(_ item: AXUIElement) -> String? {
        switch optionalText(item, kAXTitleAttribute) {
        case .value(let value): return value
        case .unavailable: return nil
        case .absent:
            switch optionalText(item, kAXDescriptionAttribute) {
            case .value(let value): return value
            case .absent: return ""
            case .unavailable: return nil
            }
        }
    }
    func rect(_ item: AXUIElement) -> HitRect? {
        guard let p = attribute(item, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = attribute(item, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
        return HitRect(CGRect(origin: point, size: size))
    }
    guard let role = attribute(element, kAXRoleAttribute) as? String else { return result("context_unresolved", "role unavailable") }
    guard role == request.binding.role else { return result("target_occluded", "role changed") }
    let textEntry = [kAXTextFieldRole, kAXTextAreaRole].contains(role)
    if textEntry {
        // The known editor policy is checked before reading labels/content.
        let subrole = attribute(element, kAXSubroleAttribute) as? String
        guard ["ax_set_value", "click"].contains(action),
              ["com.lingxi.computer-user.guard-fixture", "com.lingxi.computer-user.mvp-fixture", "com.apple.TextEdit"].contains(request.bundle_id),
              (subrole == nil || subrole == ""), subrole == request.binding.subrole else { return result("context_unresolved", "editable field policy unavailable") }
    }
    guard let frame = rect(element) else { return result("context_unresolved", "element frame unavailable") }
    guard frame.matches(request.frame) else { return result("target_occluded", "element geometry changed") }
    guard let label = label(element) else { return result("context_unresolved", "bounded label unavailable") }
    guard bindingLabel(label) == request.binding.label else { return result("target_occluded", "element label changed") }
    func windowID(_ frame: HitRect) -> UInt32? {
        guard monotonicMS() < deadline,
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        let matching = windows.filter {
            guard ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid,
                  ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
                  let bounds = $0[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
            return menuAnchor == nil ? HitRect(rect).matches(frame) : HitRect(rect) == frame
        }
        guard matching.count == 1, monotonicMS() < deadline else { return nil }
        return (matching.first?[kCGWindowNumber as String] as? NSNumber)?.uint32Value
    }
    guard let ownerWindowValue = attribute(element, kAXWindowAttribute), CFGetTypeID(ownerWindowValue) == AXUIElementGetTypeID() else {
        return result("context_unresolved", "AX owner window unavailable")
    }
    let ownerWindow = ownerWindowValue as! AXUIElement
    let windowValue: AXUIElement
    let sheetParent: AXUIElement?
    if request.binding.isRegisteredSheetBinding {
        // A sheet's AXWindow points at the background. The independent
        // top-level sheet and its CG window are the only input layer here.
        guard request.bundle_id == "com.lingxi.computer-user.mvp-fixture",
              ["ax_press", "click", "pointer_move"].contains(action), role == kAXButtonRole,
              menuAnchor == nil,
              let top = attribute(element, kAXTopLevelUIElementAttribute),
              CFGetTypeID(top) == AXUIElementGetTypeID(),
              attribute(top as! AXUIElement, kAXRoleAttribute) as? String == kAXSheetRole else {
            return result("context_unresolved", "registered sheet action scope unavailable")
        }
        windowValue = top as! AXUIElement
        sheetParent = ownerWindow
    } else {
        windowValue = ownerWindow
        sheetParent = nil
    }
    guard let windowFrame = rect(windowValue), let id = windowID(windowFrame) else {
        return result("context_unresolved", "window identity unavailable or ambiguous")
    }
    guard id == request.window_id else { return result("target_occluded", "window changed") }
    if let anchor = menuAnchor {
        guard action == "ax_show_menu", anchor.valid, anchor.opener_fingerprint == request.binding,
              anchor.process.stamp == stamp, frame == anchor.opener_frame,
              windowFrame == anchor.origin_window_frame else { return result("context_unresolved", "exact menu opener anchor changed") }
    }
    func activeContextFailure() -> String? {
        guard let appValue = attribute(system, kAXFocusedApplicationAttribute), CFGetTypeID(appValue) == AXUIElementGetTypeID() else { return "focused application unavailable" }
        var owner: pid_t = 0
        guard AXUIElementGetPid(appValue as! AXUIElement, &owner) == .success, owner == request.pid else { return "focused application changed" }
        guard let focused = attribute(appValue as! AXUIElement, kAXFocusedWindowAttribute), CFEqual(focused, windowValue) else { return "focused window unavailable or changed" }
        guard let enabled = attribute(element, kAXEnabledAttribute) as? NSNumber, enabled.boolValue else { return "enabled state unavailable or false" }
        if request.contextWitness.tableColumn != nil {
            guard action == "ax_press", role == kAXButtonRole,
                  attribute(element, kAXSubroleAttribute) as? String == "AXSortButton" else {
                return "sortable table header target changed"
            }
        }
        if let sheetParent {
            var rawSheetModal: CFTypeRef?
            let sheetModalError = AXUIElementCopyAttributeValue(windowValue, kAXModalAttribute as CFString, &rawSheetModal)
            guard registeredSheetModalAttributeKnownNonTrue(error: sheetModalError.rawValue,
                typeID: rawSheetModal.map(CFGetTypeID), modal: (rawSheetModal as? NSNumber)?.boolValue,
                timely: monotonicMS() < deadline) else { return "registered sheet modal attribute unavailable or changed" }
            guard attribute(windowValue, kAXRoleAttribute) as? String == kAXSheetRole,
                  let sheetFocused = attribute(windowValue, kAXFocusedAttribute) as? NSNumber, sheetFocused.boolValue,
                  attribute(sheetParent, kAXRoleAttribute) as? String == kAXWindowRole,
                  attribute(sheetParent, kAXSubroleAttribute) as? String == kAXStandardWindowSubrole,
                  let parentModal = attribute(sheetParent, kAXModalAttribute) as? NSNumber, !parentModal.boolValue,
                  let parent = attribute(windowValue, kAXParentAttribute), CFEqual(parent, sheetParent),
                  let currentTop = attribute(element, kAXTopLevelUIElementAttribute), CFEqual(currentTop, windowValue),
                  let currentOwner = attribute(element, kAXWindowAttribute), CFEqual(currentOwner, sheetParent),
                  let windows = attribute(appValue as! AXUIElement, kAXWindowsAttribute) as? [AXUIElement],
                  windows.count == 1, CFEqual(windows[0], sheetParent),
                  let children = attribute(sheetParent, kAXChildrenAttribute) as? [AXUIElement], children.count <= 256,
                  registeredSheetChildRolesKnown(children.map { attribute($0, kAXRoleAttribute) as? String }),
                  children.filter({ CFEqual($0, windowValue) }).count == 1,
                  let parentFrame = rect(sheetParent), let parentID = windowID(parentFrame), parentID != id,
                  let visible = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
                return "registered sheet ownership or focus changed"
            }
            var treeFailure = "registered sheet tree incomplete or transient"
            guard registeredSheetTreeKnown(root: sheetParent, sheet: windowValue, pid: request.pid,
                deadline: deadline, failure: &treeFailure) else {
                return treeFailure
            }
            let own = visible.filter {
                ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid
            }
            guard own.count == 2,
                  own.allSatisfy({ ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0 }),
                  (own[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == id,
                  (own[1][kCGWindowNumber as String] as? NSNumber)?.uint32Value == parentID,
                  windowID(windowFrame) == id else { return "registered sheet drawing context changed" }
        } else {
            guard let modal = attribute(windowValue, kAXModalAttribute) as? NSNumber, !modal.boolValue else { return "modal state unavailable or true" }
        }
        if let menu = nativeMenuInputBlockReason(root: windowValue, pid: request.pid, deadline: deadline) { return menu }
        if action == "ax_set_value" {
            var settable = DarwinBoolean(false)
            guard monotonicMS() < deadline, AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable) == .success,
                  settable.boolValue else { return "value is not settable" }
            if textEntry {
                // AppKit field editors may be distinct AX objects. The target's
                // focused attribute is the typed ownership claim; app/window
                // focus has already been independently checked above.
                guard let focused = attribute(element, kAXFocusedAttribute) as? NSNumber, focused.boolValue else { return "text target does not hold focus" }
            }
            return monotonicMS() < deadline ? nil : "active context expired"
        }
        if ["click", "scroll", "pointer_move"].contains(action) { return monotonicMS() < deadline ? nil : "active context expired" }
        let requiredAction = action == "ax_show_menu" ? kAXShowMenuAction : kAXPressAction
        var actions: CFArray?
        guard monotonicMS() < deadline, AXUIElementCopyActionNames(element, &actions) == .success,
              let names = actions as? [String], names.count <= 16, names.contains(requiredAction) else { return "required AX action unavailable" }
        return monotonicMS() < deadline ? nil : "active context expired"
    }
    if requireActive, let failure = activeContextFailure() { return result("context_unresolved", failure) }
    let proofReader = NativeBindingReader(pid: request.pid, bundle: request.bundle_id,
        windowID: request.window_id, window: windowValue, deadline: deadline,
        allowedRoles: HitRequest.roles(action: action), sheetParent: sheetParent,
        tableColumn: request.contextWitness.tableColumn)
    guard let initialContext = proofReader.readContext(element) else {
        return result("context_unresolved", proofReader.failureReason)
    }
    let proof = initialContext.fingerprint
    guard proof == request.binding else { return result("target_occluded", "binding fingerprint changed") }
    guard initialContext.matches(request.contextWitness, binding: request.binding,
            displayFrame: request.frame,
            activeLayer: requireActive ? request.binding.window_id : nil,
            verifyActiveLayer: requireActive) else {
        return result("context_unresolved", "current Device context witness differs from the live AX closure")
    }
    var finalElement: AXUIElement?
    guard monotonicMS() < deadline,
          AXUIElementCopyElementAtPosition(system, Float(request.x), Float(request.y), &finalElement) == .success,
          let finalElement, CFEqual(element, finalElement),
          let finalFrame = rect(finalElement), finalFrame.matches(frame),
          let finalWindowFrame = rect(windowValue), finalWindowFrame.matches(windowFrame),
          (menuAnchor == nil || (finalFrame == menuAnchor!.opener_frame && finalWindowFrame == menuAnchor!.origin_window_frame)),
          windowID(finalWindowFrame) == id,
          let finalContext = proofReader.readContext(finalElement),
          finalContext.fingerprint == proof,
          finalContext.matches(request.contextWitness, binding: request.binding,
              displayFrame: request.frame,
              activeLayer: requireActive ? request.binding.window_id : nil,
              verifyActiveLayer: requireActive),
          initialContext.sameWorkerGraph(as: finalContext) else {
        return result("context_unresolved", "hit context changed during query")
    }
    guard monotonicMS() < deadline, AXIsProcessTrusted(),
          let finalApp = NSRunningApplication(processIdentifier: request.pid), !finalApp.isTerminated,
          finalApp.bundleIdentifier == request.bundle_id, DeviceProcessStamp.read(request.pid) == stamp else {
        return result("context_unresolved", "hit test expired or process changed")
    }
    if requireActive, let failure = activeContextFailure() { return result("context_unresolved", failure) }
    onMatched?(element)
    var matched = result("matched", "complete fp1 fields matched; no input authorization")
    matched.binding_digest = proof.digest
    return matched
}
