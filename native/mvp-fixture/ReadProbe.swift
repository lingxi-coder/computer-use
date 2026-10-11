// Read-only relationships for a fresh, driver-owned Interaction Fixture process.
import AppKit
import ApplicationServices
import Darwin

private func fixtureReadNotification(_ observer: AXObserver, _ element: AXUIElement,
                                     _ name: CFString, _ context: UnsafeMutableRawPointer?) {}

@MainActor
private final class FixtureReadProbe {
    let pid: pid_t
    let inspectNotifications: Bool
    let deadline = DispatchTime.now().uptimeNanoseconds + 2_000_000_000
    var refs: [AXUIElement] = []
    var nodes: [[String: Any]] = []
    var truncated = false
    var attributeStatus: [String: [String: Int]] = [:]

    init(pid: pid_t, inspectNotifications: Bool) {
        self.pid = pid; self.inspectNotifications = inspectNotifications
    }

    func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
        var owner: pid_t = 0
        guard DispatchTime.now().uptimeNanoseconds < deadline,
              AXUIElementGetPid(element, &owner) == .success, owner == pid,
              AXUIElementSetMessagingTimeout(element, 0.02) == .success else { return nil }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, key as CFString, &value)
        attributeStatus[key] = ["error": Int(error.rawValue), "type": value.map { Int(CFGetTypeID($0)) } ?? 0]
        return error == .success ? value : nil
    }

    func identity(_ element: AXUIElement) -> Int? {
        var owner: pid_t = 0
        guard AXUIElementGetPid(element, &owner) == .success, owner == pid else { return nil }
        if let index = refs.firstIndex(where: { CFEqual($0, element) }) { return index + 1 }
        guard refs.count < 512 else { truncated = true; return nil }
        refs.append(element)
        return refs.count
    }

    func frame(_ element: AXUIElement) -> [Double]? {
        guard let p = attribute(element, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = attribute(element, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point),
              AXValueGetValue(s as! AXValue, .cgSize, &size),
              [point.x, point.y, size.width, size.height].allSatisfy(\.isFinite) else { return nil }
        return [point.x, point.y, size.width, size.height]
    }

    func walk(_ element: AXUIElement, parent: Int?, relation: String, depth: Int) {
        guard DispatchTime.now().uptimeNanoseconds < deadline, nodes.count < 384, depth <= 12 else {
            truncated = true; return
        }
        guard let id = identity(element), !nodes.contains(where: { $0["id"] as? Int == id }) else { return }
        attributeStatus = [:]
        let role = attribute(element, kAXRoleAttribute) as? String ?? "unknown"
        let secure = role == "AXSecureTextField" || attribute(element, kAXSubroleAttribute) as? String == kAXSecureTextFieldSubrole
        var node: [String: Any] = ["id": id, "parent": parent as Any? ?? NSNull(), "relation": relation,
                                   "role": role, "frame": frame(element) as Any? ?? NSNull()]
        for key in [kAXModalAttribute, kAXEnabledAttribute, kAXFocusedAttribute, kAXSelectedAttribute] {
            node[key] = (attribute(element, key) as? NSNumber) as Any? ?? NSNull()
        }
        if [kAXRowRole, kAXColumnRole, kAXCellRole, kAXTableRole].contains(role) {
            node["AXIndex"] = (attribute(element, kAXIndexAttribute) as? NSNumber) as Any? ?? NSNull()
            for key in [kAXRowIndexRangeAttribute, kAXColumnIndexRangeAttribute] {
                if let raw = attribute(element, key), CFGetTypeID(raw) == AXValueGetTypeID() {
                    var range = CFRange()
                    if AXValueGetValue(raw as! AXValue, .cfRange, &range) {
                        node[key] = [range.location, range.length]
                    }
                }
            }
            var settable = DarwinBoolean(false)
            let error = AXUIElementIsAttributeSettable(element, kAXSelectedAttribute as CFString, &settable)
            node["selected_settable_error"] = Int(error.rawValue)
            node["selected_settable"] = error == .success ? settable.boolValue as Any : NSNull()
        }
        for key in [kAXWindowAttribute, kAXTopLevelUIElementAttribute] {
            if let raw = attribute(element, key), CFGetTypeID(raw) == AXUIElementGetTypeID() {
                node[key] = identity(raw as! AXUIElement) as Any? ?? NSNull()
            }
        }
        if !secure {
            for key in [kAXTitleAttribute, kAXDescriptionAttribute, kAXIdentifierAttribute] {
                node[key] = (attribute(element, key) as? String).map { String($0.prefix(256)) } as Any? ?? NSNull()
            }
            let value = attribute(element, kAXValueAttribute)
            node["AXValue"] = (value as? String).map { String($0.prefix(2048)) } as Any?
                ?? (value as? NSNumber) as Any? ?? NSNull()
        }
        var actions: CFArray?
        if DispatchTime.now().uptimeNanoseconds < deadline,
           AXUIElementCopyActionNames(element, &actions) == .success {
            node["actions"] = Array((actions as? [String] ?? []).prefix(16))
        }
        node["attribute_status"] = attributeStatus
        nodes.append(node)
        for key in [kAXChildrenAttribute, "AXSheets"] {
            let children = attribute(element, key) as? [AXUIElement] ?? []
            if children.count > 256 { truncated = true }
            for child in children.prefix(256) { walk(child, parent: id, relation: key, depth: depth + 1) }
        }
    }

    func run() -> [String: Any] {
        let app = AXUIElementCreateApplication(pid)
        let windows = attribute(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
        let focused = attribute(app, kAXFocusedWindowAttribute)
        var focusedID: Int?
        if let focused, CFGetTypeID(focused) == AXUIElementGetTypeID() { focusedID = identity(focused as! AXUIElement) }
        for window in windows.prefix(16) { walk(window, parent: nil, relation: "AXWindows", depth: 0) }
        if inspectNotifications {
            var observer: AXObserver?
            let createError = AXObserverCreate(pid, fixtureReadNotification, &observer)
            for index in nodes.indices {
                guard DispatchTime.now().uptimeNanoseconds < deadline,
                      let id = nodes[index]["id"] as? Int, id > 0, id <= refs.count else {
                    truncated = true; break
                }
                nodes[index]["destroyed_registration_error"] = Int(createError.rawValue)
                if let observer, createError == .success {
                    let element = refs[id - 1]
                    nodes[index]["destroyed_registration_error"] = Int(AXObserverAddNotification(
                        observer, element, kAXUIElementDestroyedNotification as CFString, nil).rawValue)
                }
            }
        }
        let cg = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]] ?? []).filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid }
            .prefix(16).map { item in
                ["id": item[kCGWindowNumber as String] ?? NSNull(),
                 "layer": item[kCGWindowLayer as String] ?? NSNull(),
                 "frame": item[kCGWindowBounds as String] ?? NSNull()]
            }
        return ["version": 1, "pid": pid,
                "frontmost_is_target": NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
                "focused_window": focusedID as Any? ?? NSNull(),
                "ax_window_count": windows.count, "cg_windows": cg, "nodes": nodes,
                "truncated": truncated, "physical_inputs": 0]
    }
}

@main
struct FixtureReadProbeMain {
    @MainActor
    static func main() {
        let arguments = CommandLine.arguments
        guard arguments.count == 2 || (arguments.count == 3 && arguments[2] == "--notifications"),
              let pid = Int32(arguments[1]), pid > 0,
              let target = NSRunningApplication(processIdentifier: pid), !target.isTerminated,
              target.bundleIdentifier == "com.lingxi.computer-user.mvp-fixture",
              AXIsProcessTrusted() else { exit(2) }
        let result = FixtureReadProbe(pid: pid, inspectNotifications: arguments.count == 3).run()
        guard let data = try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]),
              data.count <= 256 * 1024 else { exit(2) }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([10]))
    }
}
