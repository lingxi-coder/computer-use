// Readonly relationships of the launcher-owned sibling MenuFixture process.
import AppKit
import ApplicationServices
import Darwin

private struct MenuProbeRequest: Decodable {
    let pid: Int32
    let bundle_id: String
    let executable_path: String
    let parent_pid: Int32
    let origin_window_id: UInt32
}

private func probeUptimeMS() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

@MainActor
private final class MenuRelationshipReader {
    let request: MenuProbeRequest
    let deadline = probeUptimeMS() + 2000
    var refs: [AXUIElement] = []
    var failures: [String] = []
    var nodes: [[String: Any]] = []
    var hits: [[String: Any]] = []
    var discovered: [AXUIElement] = []
    init(_ request: MenuProbeRequest) { self.request = request }
    func owned(_ element: AXUIElement) -> Bool {
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success && pid == request.pid
    }
    func ref(_ element: AXUIElement) -> String? {
        guard owned(element) else { return nil }
        if let index = refs.firstIndex(where: { CFEqual($0, element) }) { return "diagnostic-element-\(index + 1)" }
        guard refs.count < 256 else { failures.append("AX reference budget exceeded"); return nil }
        refs.append(element); return "diagnostic-element-\(refs.count)"
    }
    func value(_ element: AXUIElement, _ name: String) -> (CFTypeRef?, Int32) {
        guard owned(element) else { return (nil, AXError.cannotComplete.rawValue) }
        let now = probeUptimeMS()
        guard now < deadline else { return (nil, AXError.cannotComplete.rawValue) }
        let timeout = Float(min(25, deadline - now)) / 1000
        guard timeout > 0, AXUIElementSetMessagingTimeout(element, timeout) == .success else { return (nil, AXError.cannotComplete.rawValue) }
        var result: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &result)
        return (result, error.rawValue)
    }
    func element(_ owner: AXUIElement, _ name: String) -> (AXUIElement?, Int32) {
        let (raw, error) = value(owner, name)
        guard error == AXError.success.rawValue, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return (nil, error) }
        let result = raw as! AXUIElement
        guard owned(result) else { return (nil, AXError.invalidUIElement.rawValue) }
        return (result, error)
    }
    func frame(_ item: AXUIElement) -> CGRect? {
        guard let p = value(item, kAXPositionAttribute).0, CFGetTypeID(p) == AXValueGetTypeID(),
              let s = value(item, kAXSizeAttribute).0, CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size),
              [point.x, point.y, size.width, size.height].allSatisfy(\.isFinite), size.width > 0, size.height > 0 else { return nil }
        return CGRect(origin: point, size: size)
    }
    func box(_ frame: CGRect) -> [String: Double] {
        ["x": frame.minX, "y": frame.minY, "width": frame.width, "height": frame.height]
    }
    func role(_ item: AXUIElement) -> String? { value(item, kAXRoleAttribute).0 as? String }
    func summary(_ item: AXUIElement) -> [String: Any] {
        guard let id = ref(item) else { return ["unavailable": true] }
        var result: [String: Any] = ["diagnostic_id": id, "pid": request.pid]
        for name in [kAXRoleAttribute, kAXSubroleAttribute, kAXTitleAttribute, kAXDescriptionAttribute] {
            let (raw, error) = value(item, name)
            result[name] = ["error": error, "value": (raw as? String).map { String($0.prefix(512)) } as Any? ?? NSNull()]
        }
        let (enabled, enabledError) = value(item, kAXEnabledAttribute)
        result["AXEnabled"] = ["error": enabledError, "value": (enabled as? NSNumber)?.boolValue as Any? ?? NSNull()]
        result["frame"] = frame(item).map(box) as Any? ?? NSNull()
        var names: CFArray?
        let actionError: AXError
        if probeUptimeMS() < deadline, owned(item), AXUIElementSetMessagingTimeout(item, 0.025) == .success {
            actionError = AXUIElementCopyActionNames(item, &names)
        } else { actionError = .cannotComplete }
        result["actions"] = ["error": actionError.rawValue, "names": (names as? [String]).map { Array($0.prefix(32)) } as Any? ?? NSNull()]
        for name in [kAXWindowAttribute, kAXTopLevelUIElementAttribute, "AXRoot"] {
            let (target, error) = element(item, name)
            result[name] = ["error": error, "diagnostic_id": target.flatMap(ref) as Any? ?? NSNull(),
                "role": target.flatMap(role) as Any? ?? NSNull(), "frame": target.flatMap(frame).map(box) as Any? ?? NSNull()]
        }
        return result
    }
    func chain(_ item: AXUIElement) -> [String: Any] {
        var cursor = item, visited: [AXUIElement] = [], records: [[String: Any]] = []
        var rootWindow: String?
        var reason = "parent unavailable"
        while records.count < 32, probeUptimeMS() < deadline, owned(cursor) {
            if visited.contains(where: { CFEqual($0, cursor) }) { reason = "parent cycle"; break }
            visited.append(cursor); records.append(summary(cursor))
            if role(cursor) == kAXWindowRole { rootWindow = ref(cursor) }
            let (parent, error) = element(cursor, kAXParentAttribute)
            guard let parent else { reason = "AXParent error \(error)"; break }
            cursor = parent
        }
        if records.count == 32 { reason = "parent depth budget exceeded" }
        if probeUptimeMS() >= deadline { reason = "probe deadline exceeded" }
        return ["elements": records, "root_window_diagnostic_id": rootWindow as Any? ?? NSNull(), "end_reason": reason]
    }
    func add(_ item: AXUIElement) {
        guard owned(item), !discovered.contains(where: { CFEqual($0, item) }), discovered.count < 192 else { return }
        discovered.append(item)
    }
    func traverse(_ roots: [AXUIElement]) {
        var queue = roots.map { ($0, 0) }, visited: [AXUIElement] = []
        while !queue.isEmpty, visited.count < 128, probeUptimeMS() < deadline {
            let (item, depth) = queue.removeFirst()
            guard owned(item), !visited.contains(where: { CFEqual($0, item) }) else { continue }
            visited.append(item); add(item)
            guard depth < 8 else { continue }
            for name in [kAXChildrenAttribute, kAXWindowsAttribute, "AXShownMenu", "AXMenus"] {
                let (raw, _) = value(item, name)
                if let children = raw as? [AXUIElement] { queue.append(contentsOf: children.prefix(64).map { ($0, depth + 1) }) }
                else if let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() { queue.append((raw as! AXUIElement, depth + 1)) }
            }
        }
        if !queue.isEmpty { failures.append("AX discovery traversal incomplete within node/depth/time budgets") }
    }
    func hit(_ point: CGPoint, reason: String, system: AXUIElement, expected: AXUIElement? = nil) {
        var record: [String: Any] = ["point": ["x": point.x, "y": point.y], "candidate_origin": reason]
        guard probeUptimeMS() < deadline, AXUIElementSetMessagingTimeout(system, 0.025) == .success else {
            record["error"] = AXError.cannotComplete.rawValue; hits.append(record); return
        }
        var result: AXUIElement?
        let error = AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &result)
        record["error"] = error.rawValue
        if error == .success, let result {
            var pid: pid_t = 0
            let pidError = AXUIElementGetPid(result, &pid)
            record["returned_pid"] = pid; record["pid_error"] = pidError.rawValue
            record["same_owned_pid"] = pidError == .success && pid == request.pid
            // Never inspect titles or ancestry of an occluding foreign app.
            if pidError == .success, pid == request.pid {
                record["element"] = summary(result)
                if let expected { record["same_element"] = CFEqual(expected, result) }
                add(result)
                var cursor = result
                for _ in 0..<16 {
                    guard let parent = element(cursor, kAXParentAttribute).0 else { break }
                    add(parent); cursor = parent
                }
            }
        }
        hits.append(record)
    }
    func run() -> [String: Any] {
        let application = AXUIElementCreateApplication(request.pid), system = AXUIElementCreateSystemWide()
        let windows = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? [])
            .filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid }
        let nativeWindows: [[String: Any]] = windows.prefix(32).map {
            ["pid": request.pid, "window_id": $0[kCGWindowNumber as String] ?? NSNull(),
                "layer": $0[kCGWindowLayer as String] ?? NSNull(), "frame": $0[kCGWindowBounds as String] ?? NSNull(),
                "alpha": $0[kCGWindowAlpha as String] ?? NSNull()]
        }
        // WindowServer geometry is only a readonly discovery hint. Ownership
        // is checked on every resulting AX handle before content is read.
        for window in windows.prefix(16) {
            guard (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value != request.origin_window_id,
                  let bounds = window[kCGWindowBounds as String] as? [String: Any],
                  let frame = CGRect(dictionaryRepresentation: bounds as CFDictionary), frame.width > 0, frame.height > 0 else { continue }
            for fraction in [0.25, 0.5, 0.75] {
                hit(CGPoint(x: frame.midX, y: frame.minY + frame.height * fraction), reason: "owned_native_window_fraction_\(fraction)", system: system)
            }
        }
        traverse([application] + discovered)
        let menus = discovered.filter { role($0) == kAXMenuRole }
        traverse(menus)
        let menuItems = discovered.filter { role($0) == kAXMenuItemRole }
        for item in menuItems.prefix(32) {
            if let frame = frame(item) { hit(CGPoint(x: frame.midX, y: frame.midY), reason: "actual_AXMenuItem_center", system: system, expected: item) }
        }
        for item in (menus + menuItems).prefix(64) { nodes.append(["element": summary(item), "parent_chain": chain(item)]) }
        let focusedApplication: [String: Any]
        var focusedRaw: CFTypeRef?
        var focusError = AXError.cannotComplete
        let focusNow = probeUptimeMS()
        if focusNow < deadline,
           AXUIElementSetMessagingTimeout(system, Float(min(25, deadline - focusNow)) / 1000) == .success {
            focusError = AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &focusedRaw)
        }
        if focusError == .success, let focusedRaw, CFGetTypeID(focusedRaw) == AXUIElementGetTypeID() {
            let focused = focusedRaw as! AXUIElement
            var focusedPID: pid_t = 0
            _ = AXUIElementGetPid(focused, &focusedPID)
            focusedApplication = ["error": focusError.rawValue, "pid": focusedPID, "same_owned_pid": focusedPID == request.pid]
        } else { focusedApplication = ["error": focusError.rawValue] }
        let (focusedWindow, focusedWindowError) = element(application, kAXFocusedWindowAttribute)
        return ["kind": "menu_relationship_probe", "pid": request.pid, "bundle_id": request.bundle_id,
            "input_authorized": false, "diagnostic_ids_are_process_local_only": true,
            "origin_window_id": request.origin_window_id, "native_windows": nativeWindows,
            "focused_application": focusedApplication,
            "focused_window": ["error": focusedWindowError, "element": focusedWindow.map(summary) as Any? ?? NSNull()],
            "menus_found": menus.count, "menu_items_found": menuItems.count, "menu_relationships": nodes,
            "hit_tests": hits, "failures": failures, "deadline_exceeded": probeUptimeMS() >= deadline,
            "menu_selection_verified": false, "mvp_complete": false]
    }
}

@main
struct MenuReadProbeMain {
    @MainActor static func main() {
        do {
            let data = FileHandle.standardInput.readDataToEndOfFile()
            guard data.count <= 4096 else { throw NSError(domain: "menu-probe", code: 1) }
            let request = try JSONDecoder().decode(MenuProbeRequest.self, from: data)
            // Darwin's PROC_PIDPATHINFO_MAXSIZE macro is 4 * MAXPATHLEN, but
            // its expression is not imported into Swift by this SDK.
            var info = proc_bsdinfo(), pathBuffer = [CChar](repeating: 0, count: 4096)
            let infoSize = Int32(MemoryLayout<proc_bsdinfo>.stride)
            let pathCount = proc_pidpath(request.pid, &pathBuffer, UInt32(pathBuffer.count))
            let actualPath = pathBuffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
            let expected = URL(fileURLWithPath: request.executable_path).resolvingSymlinksInPath().standardizedFileURL.path
            guard request.pid > 0, request.parent_pid == getppid(), request.origin_window_id > 0,
                  request.bundle_id == "com.lingxi.computer-user.guard-fixture",
                  expected.contains("/target/native-macos/menu-probe-owned-"),
                  expected.hasSuffix("/MenuFixture.app/Contents/MacOS/MenuFixture"), pathCount > 0,
                  String(decoding: actualPath, as: UTF8.self) == expected,
                  proc_pidinfo(request.pid, PROC_PIDTBSDINFO, 0, &info, infoSize) == infoSize,
                  info.pbi_ppid == UInt32(request.parent_pid),
                  let app = NSRunningApplication(processIdentifier: request.pid), !app.isTerminated,
                  app.bundleIdentifier == request.bundle_id, AXIsProcessTrusted() else { throw NSError(domain: "owned menu probe scope unavailable", code: 2) }
            let reader = MenuRelationshipReader(request)
            var result = reader.run()
            var finalInfo = proc_bsdinfo()
            result["process_identity_revalidated"] = proc_pidinfo(request.pid, PROC_PIDTBSDINFO, 0, &finalInfo, infoSize) == infoSize &&
                finalInfo.pbi_start_tvsec == info.pbi_start_tvsec && finalInfo.pbi_start_tvusec == info.pbi_start_tvusec &&
                finalInfo.pbi_ppid == info.pbi_ppid && AXIsProcessTrusted()
            let output = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
            FileHandle.standardOutput.write(output + Data([10]))
        } catch {
            let result: [String: Any] = ["kind": "menu_relationship_probe", "error": String(describing: error), "input_authorized": false]
            if let output = try? JSONSerialization.data(withJSONObject: result) { FileHandle.standardOutput.write(output + Data([10])) }
            exit(2)
        }
    }
}
