import AppKit
import ApplicationServices
import Carbon
import Foundation

struct NativeInputSource: Codable, Sendable, Equatable {
    let id: String?, source_type: String?, category: String?, mode_id: String?
    let ascii_capable: Bool?
    let layout_data_sha256: String?
    var valid: Bool {
        [id, source_type, category, mode_id].allSatisfy { $0 == nil || nativeID($0!) } &&
            (id != nil || source_type != nil || category != nil || mode_id != nil || ascii_capable != nil || layout_data_sha256 != nil) &&
            (layout_data_sha256 == nil || (layout_data_sha256!.utf8.count == 64 && layout_data_sha256!.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }))
    }
    private enum Keys: String, CodingKey { case id, source_type, category, mode_id, ascii_capable, layout_data_sha256 }
    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Keys.self)
        try fields.encode(id, forKey: .id); try fields.encode(source_type, forKey: .source_type)
        try fields.encode(category, forKey: .category); try fields.encode(mode_id, forKey: .mode_id)
        try fields.encode(ascii_capable, forKey: .ascii_capable); try fields.encode(layout_data_sha256, forKey: .layout_data_sha256)
    }
}

struct NativeKeyboardContext: Codable, Sendable, Equatable {
    let selected_input_source: NativeInputSource?, underlying_keyboard_layout: NativeInputSource?
    let keyboard_type: UInt32?, caps_lock: Bool?
    var valid: Bool { selected_input_source?.valid != false && underlying_keyboard_layout?.valid != false }
    private enum Keys: String, CodingKey { case selected_input_source, underlying_keyboard_layout, keyboard_type, caps_lock }
    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Keys.self)
        try fields.encode(selected_input_source, forKey: .selected_input_source)
        try fields.encode(underlying_keyboard_layout, forKey: .underlying_keyboard_layout)
        try fields.encode(keyboard_type, forKey: .keyboard_type); try fields.encode(caps_lock, forKey: .caps_lock)
    }
}

struct NativeCompositionProof: Codable, Sendable, Equatable {
    let kind: String, focused_instance: String
    let verified_source_sequence: UInt64, verified_observed_ms: UInt64
}
struct NativeFocusComposition: Codable, Sendable, Equatable {
    let kind: String
    let proof: NativeCompositionProof?
    var notApplicable: Bool { kind == "not_applicable" && proof == nil }
}

struct NativeFocusContext: Codable, Sendable, Equatable {
    let source_session_id: String
    let session_epoch: UInt64
    let process: NativeMenuProcessIdentity
    let origin_window_id: String, target_instance: String, focused_instance: String
    let origin_window_frame: HitRect, target_frame: HitRect
    let target_fingerprint: NativeBindingFingerprint
    let relation: String
    let composition: NativeFocusComposition
    let keyboard: NativeKeyboardContext
    let verified_source_sequence: UInt64, verified_observed_ms: UInt64
    static let keys: Set<String> = ["source_session_id", "session_epoch", "process", "origin_window_id", "origin_window_frame", "target_instance", "target_fingerprint", "target_frame", "focused_instance", "relation", "composition", "keyboard", "verified_source_sequence", "verified_observed_ms"]
    var valid: Bool {
        let fp = target_fingerprint
        return [source_session_id, origin_window_id, target_instance, focused_instance].allSatisfy(nativeID) &&
            session_epoch > 0 && process.valid && verified_source_sequence > 0 && fp.valid &&
            relation == "exact_control" && target_instance == focused_instance &&
            origin_window_frame.valid && target_frame.valid && origin_window_frame.contains(target_frame) &&
            fp.frame == target_frame.rounded && fp.app_pid == process.pid && fp.window_id == origin_window_id &&
            AtomicKeyProfile.receiverRoles.contains(fp.role) && (fp.subrole == nil || fp.subrole == "") &&
            !fp.ancestor_roles.dropFirst().contains(where: { [kAXWindowRole, kAXApplicationRole, kAXMenuRole, kAXMenuBarRole, kAXMenuItemRole, kAXSheetRole, "AXDialog"].contains($0) }) &&
            composition.notApplicable && keyboard.valid
    }
    func sameIdentity(_ other: Self) -> Bool {
        source_session_id == other.source_session_id && session_epoch == other.session_epoch && process == other.process &&
            origin_window_id == other.origin_window_id && origin_window_frame == other.origin_window_frame &&
            target_instance == other.target_instance && focused_instance == other.focused_instance &&
            target_fingerprint == other.target_fingerprint && target_frame == other.target_frame && relation == other.relation &&
            composition == other.composition && keyboard == other.keyboard
    }
}

struct NativeFocusedControlScope: Codable, Sendable, Equatable {
    let context: NativeFocusContext
    let profile: String, key: String
    func allows(_ proof: NativeSourceProof) -> Bool {
        context.valid && profile == AtomicKeyProfile.wireID && AtomicKeyProfile.keyCode(key) != nil &&
            proof.instance == context.target_instance && proof.fingerprint == context.target_fingerprint &&
            proof.display_frame == context.target_frame
    }
    func sameIdentity(_ other: Self) -> Bool { profile == other.profile && key == other.key && context.sameIdentity(other.context) }
}

/// Only the serial AX worker creates, retains and destroys these references.
/// No AX reference or pointer representation enters GuardState, IPC or public
/// authority. This is a one-shot binding witness, never an input permission.
struct KeyFocusWitness {
    let ticket: NativeProbeTicket
    let epoch: UInt64
    let receiver: AXUIElement, root: AXUIElement
    func matches(receiver: AXUIElement, root: AXUIElement) -> Bool {
        CFEqual(self.receiver, receiver) && CFEqual(self.root, root)
    }
}

final class KeyFocusWitnessStore {
    private var witness: KeyFocusWitness?
    func prune(valid: (NativeProbeTicket) -> Bool) {
        if let witness, !valid(witness.ticket) { self.witness = nil }
    }
    func arm(_ witness: KeyFocusWitness, now: UInt64 = monotonicMS()) -> Bool {
        guard self.witness == nil, witness.ticket.request.action == "key_press",
              witness.ticket.request.current_proof.focus_scope?.context.session_epoch == witness.epoch,
              now < witness.ticket.deadline else { return false }
        self.witness = witness; return true
    }
    func take(_ ticket: NativeProbeTicket, epoch: UInt64, now: UInt64 = monotonicMS()) -> KeyFocusWitness? {
        let stored = witness
        witness = nil // A mismatch consumes the slot too; it cannot be revived.
        guard let stored, stored.epoch == epoch, stored.ticket == ticket, now < stored.ticket.deadline else { return nil }
        return stored
    }
    func invalidate() { witness = nil }
}

func strictFocusScopeFields(_ value: Any) -> Bool {
    if value is NSNull { return true }
    guard let scope = value as? [String: Any], Set(scope.keys) == ["context", "profile", "key"],
          let context = scope["context"] as? [String: Any], Set(context.keys) == NativeFocusContext.keys,
          let process = context["process"] as? [String: Any], Set(process.keys) == ["pid", "start_seconds", "start_micros"],
          let fingerprint = context["target_fingerprint"] as? [String: Any], Set(fingerprint.keys) == NativeBindingFingerprint.keys else { return false }
    for field in ["origin_window_frame", "target_frame"] {
        guard let frame = context[field] as? [String: Any], Set(frame.keys) == ["x", "y", "width", "height"] else { return false }
    }
    guard let composition = context["composition"] as? [String: Any], Set(composition.keys) == ["kind"],
          let kind = composition["kind"] as? String, ["not_applicable", "unknown"].contains(kind),
          let keyboard = context["keyboard"] as? [String: Any],
          Set(keyboard.keys) == ["selected_input_source", "underlying_keyboard_layout", "keyboard_type", "caps_lock"] else { return false }
    for field in ["selected_input_source", "underlying_keyboard_layout"] {
        if keyboard[field] is NSNull { continue }
        guard let source = keyboard[field] as? [String: Any], Set(source.keys) == ["id", "source_type", "category", "mode_id", "ascii_capable", "layout_data_sha256"] else { return false }
    }
    return true
}

func specialKeyKeyboardFactsMatch(_ context: NativeKeyboardContext) -> Bool {
    // Space on this registered nontext receiver has no layout/IME dependency.
    // Unknown Source facts stay unknown. TIS needs a separate bounded profile;
    // this AX worker never invokes it or substitutes local facts into Source.
    guard context.selected_input_source == nil, context.underlying_keyboard_layout == nil else { return false }
    if let type = context.keyboard_type, type != UInt32(LMGetKbdType()) { return false }
    if let caps = context.caps_lock, caps != CGEventSource.flagsState(.combinedSessionState).contains(.maskAlphaShift) { return false }
    return true
}

func strictKeyAXBoolean(_ value: CFTypeRef?) -> Bool? {
    guard let value, CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
    return CFBooleanGetValue((value as! CFBoolean))
}

/// Worker-local AX references only. A center point is never the keyboard
/// receiver. CG window visibility is a separate check of the fixed root.
func diagnosticKeyFocus(_ ticket: NativeProbeTicket, epoch: UInt64, witness: KeyFocusWitness? = nil,
                        onMatched: ((AXUIElement, AXUIElement) -> Void)? = nil) -> NativeProbeResponse {
    let request = ticket.request
    func response(_ reason: String) -> NativeProbeResponse {
        NativeProbeResponse(session_epoch: epoch, request_id: request.request_id, outcome: "context_unresolved", reason: reason)
    }
    guard request.valid, request.action == "key_press", let scope = request.current_proof.focus_scope,
          AXIsProcessTrusted(), scope.context.session_epoch == epoch else { return response("key focus scope or trusted source/device generation unavailable") }
    let context = scope.context, pid = Int32(context.process.pid), fp = request.current_proof.fingerprint
    let began = monotonicMS(), deadline = min(ticket.deadline, began + 150)
    guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated, app.bundleIdentifier == fp.app,
          DeviceProcessStamp.read(pid) == context.process.stamp,
          let number = UInt32(context.origin_window_id.dropFirst(7)) else { return response("focused process identity changed") }
    let system = AXUIElementCreateSystemWide()
    func prepare(_ item: AXUIElement, systemWide: Bool = false) -> Bool {
        let now = monotonicMS(); guard now < deadline else { return false }
        if !systemWide {
            var owner: pid_t = 0
            guard AXUIElementGetPid(item, &owner) == .success, owner == pid else { return false }
        }
        return AXUIElementSetMessagingTimeout(item, Float(min(25, deadline - now)) / 1000) == .success && monotonicMS() < deadline
    }
    func attribute(_ item: AXUIElement, _ key: String, systemWide: Bool = false) -> CFTypeRef? {
        guard prepare(item, systemWide: systemWide) else { return nil }
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(item, key as CFString, &value) == .success, monotonicMS() < deadline else { return nil }
        return value
    }
    func element(_ item: AXUIElement, _ key: String, systemWide: Bool = false) -> AXUIElement? {
        guard let value = attribute(item, key, systemWide: systemWide), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        let element = value as! AXUIElement; var owner: pid_t = 0
        guard AXUIElementGetPid(element, &owner) == .success, owner == pid else { return nil }; return element
    }
    func rect(_ item: AXUIElement) -> HitRect? {
        guard let position = attribute(item, kAXPositionAttribute), CFGetTypeID(position) == AXValueGetTypeID(),
              let size = attribute(item, kAXSizeAttribute), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, extent = CGSize.zero
        guard AXValueGetValue(position as! AXValue, .cgPoint, &point), AXValueGetValue(size as! AXValue, .cgSize, &extent) else { return nil }
        let frame = HitRect(CGRect(origin: point, size: extent)); return frame.valid ? frame : nil
    }
    func visibleRoot(_ frame: HitRect) -> Bool {
        guard monotonicMS() < deadline,
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]], windows.count <= 1024 else { return false }
        let matching = windows.filter { window in
            guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
                  (window[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
                  let bounds = window[kCGWindowBounds as String] as? [String: Any],
                  let rectangle = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
            return HitRect(rectangle) == frame
        }
        return matching.count == 1 && (matching[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == number
    }
    guard let application = element(system, kAXFocusedApplicationAttribute, systemWide: true),
          let root = element(application, kAXFocusedWindowAttribute),
          attribute(root, kAXRoleAttribute) as? String == kAXWindowRole,
          attribute(root, kAXSubroleAttribute) as? String == kAXStandardWindowSubrole,
          strictKeyAXBoolean(attribute(root, kAXModalAttribute)) == false,
          let rootFrame = rect(root), rootFrame == context.origin_window_frame, visibleRoot(rootFrame),
          let receiver = element(application, kAXFocusedUIElementAttribute),
          attribute(receiver, kAXRoleAttribute) as? String == fp.role,
          let systemReceiver = element(system, kAXFocusedUIElementAttribute, systemWide: true), CFEqual(systemReceiver, receiver),
          strictKeyAXBoolean(attribute(receiver, kAXEnabledAttribute)) == true,
          strictKeyAXBoolean(attribute(receiver, kAXFocusedAttribute)) == true,
          let receiverWindow = element(receiver, kAXWindowAttribute), CFEqual(receiverWindow, root),
          let receiverFrame = rect(receiver), receiverFrame == context.target_frame else { return response("actual focused receiver or fixed root unavailable") }
    if let witness {
        guard witness.epoch == epoch, witness.ticket == ticket,
              witness.matches(receiver: receiver, root: root) else { return response("one-shot focused receiver or root witness changed") }
    }
    if let reason = nativeMenuInputBlockReason(root: root, pid: pid, deadline: deadline) { return response(reason) }
    func optionalText(_ item: AXUIElement, _ key: String) -> (Bool, String?) {
        guard prepare(item) else { return (false, nil) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(item, key as CFString, &value)
        guard monotonicMS() < deadline else { return (false, nil) }
        if error == .attributeUnsupported || error == .noValue { return (true, nil) }
        guard error == .success, let text = value as? String, text.utf8.count <= 512 else { return (false, nil) }
        return (true, text)
    }
    func label(_ item: AXUIElement) -> (Bool, String?) {
        let title = optionalText(item, kAXTitleAttribute); guard title.0 else { return (false, nil) }
        if let title = title.1 { return (true, title) }
        let description = optionalText(item, kAXDescriptionAttribute); return description
    }
    func binding(_ receiver: AXUIElement) -> NativeBindingFingerprint? {
        guard let role = attribute(receiver, kAXRoleAttribute) as? String, AtomicKeyProfile.receiverRoles.contains(role),
              let ownerWindow = element(receiver, kAXWindowAttribute), CFEqual(ownerWindow, root), let frame = rect(receiver) else { return nil }
        let subrole = optionalText(receiver, kAXSubroleAttribute), text = label(receiver)
        guard subrole.0, subrole.1 == nil || subrole.1 == "", text.0 else { return nil }
        var cursor = receiver, seen = [receiver], roles: [String] = [], group: String?, sibling: UInt32?
        while !CFEqual(cursor, root) {
            guard roles.count < 32, let parent = element(cursor, kAXParentAttribute),
                  !seen.contains(where: { CFEqual($0, parent) }), let parentRole = attribute(parent, kAXRoleAttribute) as? String,
                  prepare(parent) else { return nil }
            var count = 0
            guard AXUIElementGetAttributeValueCount(parent, kAXChildrenAttribute as CFString, &count) == .success,
                  monotonicMS() < deadline, count > 0, count <= 256, prepare(parent) else { return nil }
            var array: CFArray?
            guard AXUIElementCopyAttributeValues(parent, kAXChildrenAttribute as CFString, 0, count, &array) == .success,
                  monotonicMS() < deadline, let children = array as? [AnyObject], children.count == count else { return nil }
            var found: UInt32?
            for (index, child) in children.enumerated() {
                guard CFGetTypeID(child) == AXUIElementGetTypeID() else { return nil }
                if CFEqual(child, cursor) { guard found == nil else { return nil }; found = UInt32(index) }
            }
            guard let found else { return nil }
            if sibling == nil { sibling = found }
            if group == nil && parentRole == kAXGroupRole { let title = label(parent); guard title.0 else { return nil }; group = title.1 }
            if parentRole == kAXWindowRole && !CFEqual(parent, root) { return nil }
            seen.append(parent); roles.append(parentRole); cursor = parent
        }
        guard let sibling else { return nil }
        let result = NativeBindingFingerprint(app: fp.app, app_pid: context.process.pid, window_id: context.origin_window_id,
            ancestor_roles: roles.reversed(), group: group.map(bindingLabel), role: role, subrole: subrole.1,
            label: bindingLabel(text.1 ?? ""), sibling_index: sibling, frame: frame.rounded, scroll_container: nil)
        return result.valid && monotonicMS() < deadline ? result : nil
    }
    let contextReader = NativeBindingReader(pid: pid, bundle: fp.app, windowID: number,
        window: root, deadline: deadline, allowedRoles: AtomicKeyProfile.receiverRoles)
    guard let proof = binding(receiver), proof == fp,
          let initialDeviceContext = contextReader.readContext(receiver),
          initialDeviceContext.matches(request.context_witness,
              binding: fp, displayFrame: context.target_frame,
              activeLayer: context.origin_window_id, verifyActiveLayer: true),
          specialKeyKeyboardFactsMatch(context.keyboard), monotonicMS() < deadline else { return response("focused receiver fp1 or supported keyboard facts unavailable") }
    func receiverVisible() -> Bool {
        guard prepare(system, systemWide: true) else { return false }
        var hit: AXUIElement?
        guard AXUIElementCopyElementAtPosition(system, Float(request.x), Float(request.y), &hit) == .success,
              let hit, monotonicMS() < deadline else { return false }
        var owner: pid_t = 0
        return AXUIElementGetPid(hit, &owner) == .success && owner == pid && CFEqual(hit, receiver)
    }
    // This independent visibility check never supplies the focused receiver;
    // that identity was obtained from AXFocusedUIElement above.
    guard receiverVisible() else { return response("focused receiver is occluded at its authorized point") }
    guard let finalApplication = element(system, kAXFocusedApplicationAttribute, systemWide: true), CFEqual(finalApplication, application),
          let finalRoot = element(finalApplication, kAXFocusedWindowAttribute), CFEqual(finalRoot, root),
          let finalReceiver = element(finalApplication, kAXFocusedUIElementAttribute), CFEqual(finalReceiver, receiver),
          let finalSystemReceiver = element(system, kAXFocusedUIElementAttribute, systemWide: true), CFEqual(finalSystemReceiver, finalReceiver),
          strictKeyAXBoolean(attribute(finalReceiver, kAXEnabledAttribute)) == true,
          strictKeyAXBoolean(attribute(finalReceiver, kAXFocusedAttribute)) == true,
          strictKeyAXBoolean(attribute(finalRoot, kAXModalAttribute)) == false,
          rect(finalReceiver) == context.target_frame, rect(finalRoot) == context.origin_window_frame,
          visibleRoot(context.origin_window_frame), receiverVisible(), binding(finalReceiver) == proof,
          let finalDeviceContext = contextReader.readContext(finalReceiver),
          finalDeviceContext.matches(request.context_witness,
              binding: fp, displayFrame: context.target_frame,
              activeLayer: context.origin_window_id, verifyActiveLayer: true),
          initialDeviceContext.sameWorkerGraph(as: finalDeviceContext),
          specialKeyKeyboardFactsMatch(context.keyboard), DeviceProcessStamp.read(pid) == context.process.stamp,
          AXIsProcessTrusted(), monotonicMS() < deadline else { return response("focused receiver changed during final verification") }
    if let reason = nativeMenuInputBlockReason(root: root, pid: pid, deadline: deadline) { return response(reason) }
    guard monotonicMS() < deadline else { return response("focused receiver query expired") }
    if let witness, !witness.matches(receiver: finalReceiver, root: finalRoot) { return response("final focused binding witness changed") }
    onMatched?(finalReceiver, finalRoot)
    var result = NativeProbeResponse(session_epoch: epoch, request_id: request.request_id, outcome: "eligible", reason: "actual focused receiver and complete fp1 verified; no input authorization")
    result.binding_digest = proof.digest; return result
}
