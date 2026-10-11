import AppKit
import ApplicationServices

struct NativeMenuProcessIdentity: Codable, Sendable, Equatable {
    let pid: UInt32, start_seconds: UInt64, start_micros: UInt64
    var valid: Bool { pid > 0 && pid <= UInt32(Int32.max) && start_seconds > 0 && start_micros < 1_000_000 }
    var stamp: DeviceProcessStamp { DeviceProcessStamp(seconds: start_seconds, micros: start_micros) }
}

struct NativeMenuOpenerAnchor: Codable, Sendable, Equatable {
    let anchor_id: String, source_session_id: String
    let anchor_revision: UInt64
    let process: NativeMenuProcessIdentity
    let origin_window_id: String
    let origin_window_frame: HitRect
    let opener_instance: String
    let opener_fingerprint: NativeBindingFingerprint
    let opener_frame: HitRect
    let verified_source_sequence: UInt64, verified_observed_ms: UInt64
    let continuity: String
    static let keys: Set<String> = ["anchor_id", "source_session_id", "anchor_revision", "process", "origin_window_id", "origin_window_frame", "opener_instance", "opener_fingerprint", "opener_frame", "verified_source_sequence", "verified_observed_ms", "continuity"]
    var valid: Bool {
        let fp = opener_fingerprint
        return [anchor_id, source_session_id, origin_window_id, opener_instance].allSatisfy(nativeID) &&
            anchor_revision > 0 && verified_source_sequence > 0 && process.valid && fp.valid &&
            ["direct_read", "observed_lifetime"].contains(continuity) && origin_window_frame.valid &&
            opener_frame.valid && origin_window_frame.contains(opener_frame) && fp.frame == opener_frame.rounded &&
            fp.app_pid == process.pid && fp.window_id == origin_window_id &&
            [kAXMenuButtonRole, kAXPopUpButtonRole].contains(fp.role) && fp.ancestor_roles.first == kAXWindowRole &&
            !fp.ancestor_roles.dropFirst().contains(where: { [kAXWindowRole, kAXMenuRole, kAXMenuItemRole, kAXMenuBarRole, kAXApplicationRole, kAXSheetRole, "AXDialog"].contains($0) })
    }
    func sameIdentity(_ other: Self) -> Bool {
        anchor_id == other.anchor_id && source_session_id == other.source_session_id &&
            anchor_revision == other.anchor_revision && process == other.process &&
            origin_window_id == other.origin_window_id && origin_window_frame == other.origin_window_frame &&
            opener_fingerprint == other.opener_fingerprint && opener_frame == other.opener_frame && continuity == other.continuity
    }
}

struct NativeMenuDrawingWindow: Codable, Sendable, Equatable {
    let window_id: UInt32, owner_pid: UInt32
    let layer: Int32
    let frame: HitRect
    var valid: Bool { window_id > 0 && owner_pid > 0 && owner_pid <= UInt32(Int32.max) && layer == 101 && frame.valid }
}

struct NativeMenuContext: Codable, Sendable, Equatable {
    let context_id: String, source_session_id: String
    let opener: NativeMenuOpenerAnchor
    let menu_instance: String
    let menu_fingerprint: NativeBindingFingerprint
    let menu_frame: HitRect
    let drawing_window: NativeMenuDrawingWindow
    static let keys: Set<String> = ["context_id", "source_session_id", "opener", "menu_instance", "menu_fingerprint", "menu_frame", "drawing_window"]
    var valid: Bool {
        let fp = menu_fingerprint, root = opener.opener_fingerprint
        return [context_id, source_session_id, menu_instance].allSatisfy(nativeID) && opener.valid && fp.valid && drawing_window.valid && menu_frame.valid &&
            source_session_id == opener.source_session_id && menu_instance != opener.opener_instance &&
            fp.app == root.app && fp.app_pid == root.app_pid && fp.window_id == root.window_id && fp.role == kAXMenuRole &&
            fp.ancestor_roles == root.ancestor_roles + [root.role] && fp.frame == menu_frame.rounded &&
            drawing_window.owner_pid == opener.process.pid && drawing_window.frame == menu_frame &&
            opener.origin_window_id != "window-\(drawing_window.window_id)"
    }
    func sameIdentity(_ other: Self) -> Bool {
        context_id == other.context_id && source_session_id == other.source_session_id && opener.sameIdentity(other.opener) &&
            menu_fingerprint == other.menu_fingerprint && menu_frame == other.menu_frame && drawing_window == other.drawing_window
    }
}

struct NativeMenuOpeningRef: Codable, Sendable, Equatable {
    let attempt_id: String, intent_id: String
    let attempt_deadline_ms: UInt64
    var valid: Bool { nativeID(attempt_id) && nativeID(intent_id) && attempt_deadline_ms > 0 }
}

enum NativeMenuBindingScope: Codable, Sendable, Equatable {
    case opener(NativeMenuOpenerAnchor, UInt64)
    case item(NativeMenuContext, NativeMenuOpeningRef)
    private enum Keys: String, CodingKey { case kind, anchor, context, opened_by, attempt_deadline_ms }
    init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Keys.self)
        switch try fields.decode(String.self, forKey: .kind) {
        case "opener": self = .opener(try fields.decode(NativeMenuOpenerAnchor.self, forKey: .anchor), try fields.decode(UInt64.self, forKey: .attempt_deadline_ms))
        case "item": self = .item(try fields.decode(NativeMenuContext.self, forKey: .context), try fields.decode(NativeMenuOpeningRef.self, forKey: .opened_by))
        default: throw DecodingError.dataCorruptedError(forKey: .kind, in: fields, debugDescription: "unsupported menu scope")
        }
    }
    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Keys.self)
        switch self {
        case .opener(let anchor, let deadline):
            try fields.encode("opener", forKey: .kind); try fields.encode(anchor, forKey: .anchor); try fields.encode(deadline, forKey: .attempt_deadline_ms)
        case .item(let context, let opening):
            try fields.encode("item", forKey: .kind); try fields.encode(context, forKey: .context); try fields.encode(opening, forKey: .opened_by)
        }
    }
    var anchor: NativeMenuOpenerAnchor {
        switch self { case .opener(let anchor, _): anchor; case .item(let context, _): context.opener }
    }
    var attemptDeadline: UInt64 { switch self { case .opener(_, let deadline): deadline; case .item(_, let opening): opening.attempt_deadline_ms } }
    func sameIdentity(_ other: Self) -> Bool {
        switch (self, other) {
        case (.opener(let a, let deadline), .opener(let b, let otherDeadline)): a.sameIdentity(b) && deadline == otherDeadline
        case (.item(let a, let opening), .item(let b, let otherOpening)): a.sameIdentity(b) && opening == otherOpening
        default: false
        }
    }
    func allows(_ proof: NativeSourceProof) -> Bool {
        switch self {
        case .opener(let anchor, let deadline):
            return anchor.valid && deadline > anchor.verified_observed_ms && proof.instance == anchor.opener_instance && proof.fingerprint == anchor.opener_fingerprint && proof.display_frame == anchor.opener_frame
        case .item(let context, let opening):
            let fp = proof.fingerprint, menu = context.menu_fingerprint
            return context.valid && opening.valid && opening.attempt_deadline_ms > context.opener.verified_observed_ms && proof.instance != context.menu_instance && proof.instance != context.opener.opener_instance &&
                fp.role == kAXMenuItemRole && fp.app == menu.app && fp.app_pid == menu.app_pid && fp.window_id == menu.window_id &&
                fp.ancestor_roles == menu.ancestor_roles + [kAXMenuRole] && context.menu_frame.contains(proof.display_frame)
        }
    }
}

extension HitRect {
    func contains(_ other: Self) -> Bool {
        valid && other.valid && other.x >= x && other.y >= y &&
            other.x + other.width <= x + width && other.y + other.height <= y + height
    }
}

func menuWindowAttributeAllowed(error: AXError, sameRoot: Bool) -> Bool {
    error == .noValue || (error == .success && sameRoot)
}

func strictMenuScopeFields(_ value: Any) -> Bool {
    if value is NSNull { return true }
    func frame(_ value: Any?) -> Bool { (value as? [String: Any]).map { Set($0.keys) == ["x", "y", "width", "height"] } ?? false }
    func fingerprint(_ value: Any?) -> Bool { (value as? [String: Any]).map { Set($0.keys) == NativeBindingFingerprint.keys } ?? false }
    func anchor(_ value: Any?) -> Bool {
        guard let fields = value as? [String: Any], Set(fields.keys) == NativeMenuOpenerAnchor.keys,
              let process = fields["process"] as? [String: Any], Set(process.keys) == ["pid", "start_seconds", "start_micros"] else { return false }
        return frame(fields["origin_window_frame"]) && frame(fields["opener_frame"]) && fingerprint(fields["opener_fingerprint"])
    }
    guard let scope = value as? [String: Any], let kind = scope["kind"] as? String else { return false }
    if kind == "opener" { return Set(scope.keys) == ["kind", "anchor", "attempt_deadline_ms"] && anchor(scope["anchor"]) }
    guard kind == "item", Set(scope.keys) == ["kind", "context", "opened_by"],
          let context = scope["context"] as? [String: Any], Set(context.keys) == NativeMenuContext.keys,
          let drawing = context["drawing_window"] as? [String: Any], Set(drawing.keys) == ["window_id", "owner_pid", "layer", "frame"],
          let opening = scope["opened_by"] as? [String: Any], Set(opening.keys) == ["attempt_id", "intent_id", "attempt_deadline_ms"] else { return false }
    return anchor(context["opener"]) && fingerprint(context["menu_fingerprint"]) && frame(context["menu_frame"]) && frame(drawing["frame"])
}

/// Only on-screen drawing candidates trigger traversal. Static application
/// menu-bar trees and closed control menus do not block ordinary input. A
/// candidate we cannot classify within this query's budget fails closed.
func nativeMenuInputBlockReason(root: AXUIElement, pid: Int32, deadline: UInt64) -> String? {
    guard monotonicMS() < deadline,
          let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return "native menu visibility unavailable" }
    let candidates = windows.filter {
        ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid &&
            ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 101 &&
            (($0[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 0) > 0
    }
    guard !candidates.isEmpty else { return monotonicMS() < deadline ? nil : "native menu visibility query expired" }
    func setQueryTimeout(_ element: AXUIElement) -> Bool {
        let now = monotonicMS()
        guard now < deadline else { return false }
        let remainingMS = deadline - now
        let timeoutMS = min(UInt64(25), remainingMS)
        guard timeoutMS > 0,
              AXUIElementSetMessagingTimeout(element, Float(timeoutMS) / 1000) == .success else { return false }
        return monotonicMS() < deadline
    }
    var pending = [root], visited: [AXUIElement] = [], openers = 0
    while let element = pending.popLast(), visited.count < 128, monotonicMS() < deadline {
        guard !visited.contains(where: { CFEqual($0, element) }) else { return "native menu tree ambiguous" }
        visited.append(element)
        var owner: pid_t = 0
        guard AXUIElementGetPid(element, &owner) == .success, owner == pid else { return "native menu tree process changed" }
        var rawRole: CFTypeRef?
        guard setQueryTimeout(element),
              AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &rawRole) == .success,
              monotonicMS() < deadline,
              let role = rawRole as? String else { return "native menu role unavailable" }
        if role == kAXMenuRole { return "native menu is active" }
        if [kAXMenuButtonRole, kAXPopUpButtonRole].contains(role) {
            openers += 1; if openers > 8 { return "native menu opener budget exceeded" }
        } else if ![kAXWindowRole, kAXGroupRole, kAXSplitGroupRole, kAXToolbarRole, kAXScrollAreaRole, kAXTabGroupRole].contains(role) { continue }
        var count = 0
        guard setQueryTimeout(element) else { return "native menu child budget unavailable" }
        let error = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count)
        guard monotonicMS() < deadline else { return "native menu child budget unavailable" }
        if error == .attributeUnsupported || error == .noValue { continue }
        guard error == .success, count >= 0, count <= 128 - visited.count else { return "native menu child budget unavailable" }
        if count == 0 { continue }
        var children: CFArray?
        guard setQueryTimeout(element) else { return "native menu children unavailable" }
        guard AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, count, &children) == .success,
              monotonicMS() < deadline,
              let values = children as? [AXUIElement], values.count == count else { return "native menu children unavailable" }
        pending.append(contentsOf: values)
    }
    return "unresolved native menu drawing is active"
}

/// All refs are query-local. Source owns cross-read CF continuity; this reader
/// independently proves the current item/menu/opener/root relationship.
private final class NativeMenuRelationReader {
    let pid: Int32, bundle: String, deadline: UInt64
    init(pid: Int32, bundle: String, deadline: UInt64) { self.pid = pid; self.bundle = bundle; self.deadline = deadline }
    func owner(_ element: AXUIElement) -> Bool {
        var value: pid_t = 0
        return monotonicMS() < deadline && AXUIElementGetPid(element, &value) == .success && value == pid && monotonicMS() < deadline
    }
    func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard owner(element) else { return nil }
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success && monotonicMS() < deadline ? value : nil
    }
    func element(_ element: AXUIElement, _ name: String) -> AXUIElement? {
        guard let value = attribute(element, name), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return (value as! AXUIElement)
    }
    func frame(_ element: AXUIElement) -> HitRect? {
        guard let p = attribute(element, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = attribute(element, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
        let frame = HitRect(CGRect(origin: point, size: size)); return frame.valid ? frame : nil
    }
    func children(_ element: AXUIElement, maximum: Int, allowMissing: Bool = false) -> [AXUIElement]? {
        guard owner(element) else { return nil }
        var count = 0
        let error = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count)
        if allowMissing, error == .noValue || error == .attributeUnsupported { return [] }
        guard error == .success, count >= 0, count <= maximum, monotonicMS() < deadline else { return nil }
        if count == 0 { return [] }
        var values: CFArray?
        guard AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, count, &values) == .success,
              let values = values as? [AXUIElement], values.count == count, monotonicMS() < deadline else { return nil }
        return values
    }
    func contains(_ child: AXUIElement, parent: AXUIElement, maximum: Int) -> Bool {
        guard let children = children(parent, maximum: maximum) else { return false }
        return children.filter { CFEqual($0, child) }.count == 1
    }
    func windows() -> [[String: Any]]? {
        guard monotonicMS() < deadline,
              let result = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
              monotonicMS() < deadline else { return nil }
        return result.filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid }
    }
    func windowFrame(_ info: [String: Any]) -> HitRect? {
        guard let value = info[kCGWindowBounds as String] as? [String: Any], let rect = CGRect(dictionaryRepresentation: value as CFDictionary) else { return nil }
        return HitRect(rect)
    }
    func visibleWindowsMatch(_ context: NativeMenuContext) -> Bool {
        guard let windows = windows(), let rootID = UInt32(context.opener.origin_window_id.dropFirst(7)) else { return false }
        let roots = windows.filter { ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0 && windowFrame($0) == context.opener.origin_window_frame }
        let menus = windows.filter {
            ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 101 &&
                (($0[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 0) > 0
        }
        return roots.count == 1 && (roots[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == rootID &&
            menus.count == 1 && (menus[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == context.drawing_window.window_id &&
            windowFrame(menus[0]) == context.menu_frame
    }
    func match(item: AXUIElement, menu: AXUIElement, opener: AXUIElement, root: AXUIElement,
               request: NativeProbe, context: NativeMenuContext, system: AXUIElement) -> Bool {
        guard owner(item), owner(menu), owner(opener), owner(root),
              attribute(item, kAXRoleAttribute) as? String == kAXMenuItemRole,
              attribute(menu, kAXRoleAttribute) as? String == kAXMenuRole,
              attribute(opener, kAXRoleAttribute) as? String == context.opener.opener_fingerprint.role,
              attribute(root, kAXRoleAttribute) as? String == kAXWindowRole,
              attribute(root, kAXSubroleAttribute) as? String == kAXStandardWindowSubrole,
              let itemParent = element(item, kAXParentAttribute), CFEqual(itemParent, menu),
              let menuParent = element(menu, kAXParentAttribute), CFEqual(menuParent, opener),
              contains(item, parent: menu, maximum: 128), contains(menu, parent: opener, maximum: 8),
              let itemChildren = children(item, maximum: 1, allowMissing: true), itemChildren.isEmpty,
              let openerChildren = children(opener, maximum: 8),
              openerChildren.filter({ attribute($0, kAXRoleAttribute) as? String == kAXMenuRole }).count == 1,
              frame(item) == request.original_proof.display_frame, frame(menu) == context.menu_frame,
              frame(opener) == context.opener.opener_frame, frame(root) == context.opener.origin_window_frame,
              let top = element(item, kAXTopLevelUIElementAttribute), CFEqual(top, root),
              let openerWindow = element(opener, kAXWindowAttribute), CFEqual(openerWindow, root),
              let modal = attribute(root, kAXModalAttribute) as? NSNumber, !modal.boolValue,
              let enabled = attribute(item, kAXEnabledAttribute) as? NSNumber, enabled.boolValue,
              let openerEnabled = attribute(opener, kAXEnabledAttribute) as? NSNumber, openerEnabled.boolValue,
              let focusedAppRaw = rawSystem(system, kAXFocusedApplicationAttribute), CFGetTypeID(focusedAppRaw) == AXUIElementGetTypeID() else { return false }
        let focusedApp = focusedAppRaw as! AXUIElement
        guard owner(focusedApp), let focusedRoot = element(focusedApp, kAXFocusedWindowAttribute), CFEqual(focusedRoot, root),
              visibleWindowsMatch(context), let rootID = UInt32(context.opener.origin_window_id.dropFirst(7)) else { return false }
        let ordinary = NativeBindingReader(pid: pid, bundle: bundle, windowID: rootID, window: root,
                                          deadline: deadline, allowedRoles: [kAXMenuButtonRole, kAXPopUpButtonRole])
        let menuReader = NativeBindingReader(pid: pid, bundle: bundle, windowID: rootID, window: root,
                                            deadline: deadline, allowedRoles: [kAXMenuRole, kAXMenuItemRole])
        guard ordinary.read(opener) == context.opener.opener_fingerprint,
              menuReader.readMenuElement(menu) == context.menu_fingerprint,
              menuReader.readMenuElement(item) == request.original_proof.fingerprint,
              owner(item), monotonicMS() < deadline else { return false }
        var actions: CFArray?
        guard AXUIElementCopyActionNames(item, &actions) == .success, let names = actions as? [String],
              names.count <= 16, names.contains(kAXPressAction), monotonicMS() < deadline else { return false }
        return true
    }
    func rawSystem(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard monotonicMS() < deadline else { return nil }
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success && monotonicMS() < deadline ? value : nil
    }
}

func diagnosticMenuHitTest(_ ticket: NativeProbeTicket, epoch: UInt64,
                           onMatched: ((AXUIElement) -> Void)? = nil) -> HitResponse {
    let request = ticket.request
    func refusal(_ reason: String, outcome: String = "context_unresolved") -> HitResponse {
        HitResponse(session_epoch: epoch, request_id: request.request_id, outcome: outcome, reason: reason)
    }
    guard request.valid, request.action == "ax_menu_press", AXIsProcessTrusted(),
          case .item(let context, _) = request.current_proof.menu_scope,
          let hit = request.hit, hit.validFor(action: "ax_menu_press"),
          request.x == request.original_proof.display_frame.rect.midX, request.y == request.original_proof.display_frame.rect.midY,
          let app = NSRunningApplication(processIdentifier: hit.pid), !app.isTerminated, app.bundleIdentifier == hit.bundle_id,
          DeviceProcessStamp.read(hit.pid) == context.opener.process.stamp else { return refusal("menu source or process context unavailable") }
    let deadline = min(ticket.deadline, monotonicMS() + 150)
    let reader = NativeMenuRelationReader(pid: hit.pid, bundle: hit.bundle_id, deadline: deadline)
    let system = AXUIElementCreateSystemWide()
    guard AXUIElementSetMessagingTimeout(system, 0.025) == .success else { return refusal("menu query timeout unavailable") }
    var value: AXUIElement?
    guard monotonicMS() < deadline,
          AXUIElementCopyElementAtPosition(system, Float(request.x), Float(request.y), &value) == .success,
          let item = value else { return refusal("menu item center hit unavailable") }
    guard reader.owner(item) else { return refusal("menu point belongs to another process", outcome: "target_occluded") }
    guard let menu = reader.element(item, kAXParentAttribute), let opener = reader.element(menu, kAXParentAttribute),
          let root = reader.element(item, kAXTopLevelUIElementAttribute),
          reader.match(item: item, menu: menu, opener: opener, root: root, request: request, context: context, system: system) else {
        return refusal("menu item relationship or complete fingerprint changed")
    }
    guard let rootID = UInt32(context.opener.origin_window_id.dropFirst(7)) else {
        return refusal("menu root identity is malformed")
    }
    let contextReader = NativeBindingReader(pid: hit.pid, bundle: hit.bundle_id,
        windowID: rootID, window: root, deadline: deadline, allowedRoles: [kAXMenuItemRole])
    guard let initialContext = contextReader.readMenuContext(item),
          initialContext.matches(request.context_witness,
              binding: request.original_proof.fingerprint,
              displayFrame: request.original_proof.display_frame,
              activeLayer: context.opener.origin_window_id,
              verifyActiveLayer: true) else {
        return refusal(contextReader.failureReason)
    }
    var final: AXUIElement?
    guard monotonicMS() < deadline,
          AXUIElementCopyElementAtPosition(system, Float(request.x), Float(request.y), &final) == .success,
          let final, CFEqual(final, item), reader.owner(final),
          reader.match(item: final, menu: menu, opener: opener, root: root, request: request, context: context, system: system),
          let finalContext = contextReader.readMenuContext(final),
          finalContext.matches(request.context_witness,
              binding: request.original_proof.fingerprint,
              displayFrame: request.original_proof.display_frame,
              activeLayer: context.opener.origin_window_id,
              verifyActiveLayer: true),
          initialContext.sameWorkerGraph(as: finalContext),
          DeviceProcessStamp.read(hit.pid) == context.opener.process.stamp, AXIsProcessTrusted(), !app.isTerminated,
          monotonicMS() < deadline else { return refusal("menu relation changed during final query") }
    onMatched?(final)
    var response = refusal("level-one menu relation and complete fingerprints matched; no input authorization", outcome: "matched")
    response.binding_digest = request.binding_digest
    return response
}
