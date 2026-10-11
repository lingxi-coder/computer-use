import AppKit
import ApplicationServices
import Darwin
import Foundation

/// Read-only evaluator. It launches and owns its only target; no AX actions.
@main @MainActor
struct BackgroundTreeProbe {
    static func birth(_ pid: Int32) -> [UInt64]? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return [info.pbi_start_tvsec, info.pbi_start_tvusec]
    }
    static func main() throws {
        let args = CommandLine.arguments
        guard args.count == 3, args[1].hasSuffix("/Background AX Fixture.app/Contents/MacOS/BackgroundAXFixture"),
              args[2].hasPrefix("/"), !FileManager.default.fileExists(atPath: args[2]),
              AXIsProcessTrusted() else { exit(2) }
        let child = Process()
        child.executableURL = URL(fileURLWithPath: args[1])
        child.arguments = ["--snapshot", args[2]]
        let pipe = Pipe()
        child.standardOutput = pipe
        try child.run()
        defer { if child.isRunning { child.terminate() }; child.waitUntilExit() }
        let ready = pipe.fileHandleForReading.availableData
        guard let row = try JSONSerialization.jsonObject(with: ready) as? [String: Any],
              (row["pid"] as? NSNumber)?.int32Value == child.processIdentifier,
              let number = (row["window_number"] as? NSNumber)?.uint32Value,
              let target = NSRunningApplication(processIdentifier: child.processIdentifier),
              target.bundleIdentifier == "com.lingxi.computer-user.guard-fixture",
              target.executableURL?.standardizedFileURL == child.executableURL?.standardizedFileURL,
              let stamp = birth(child.processIdentifier) else { throw NSError(domain: "owned-background-probe", code: 3) }
        Thread.sleep(forTimeInterval: 3)
        let pid = child.processIdentifier
        let application = AXUIElementCreateApplication(pid)
        var deadline = DispatchTime.now().uptimeNanoseconds + 2_000_000_000
        func timed(_ element: AXUIElement) -> Bool {
            let now = DispatchTime.now().uptimeNanoseconds
            return now < deadline && AXUIElementSetMessagingTimeout(element,
                Float(min(25_000_000, deadline - now)) / 1_000_000_000) == .success
        }
        func read(_ element: AXUIElement, _ key: String) -> (AXError, CFTypeRef?) {
            guard timed(element) else { return (.cannotComplete, nil) }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, key as CFString, &raw)
            return (error, raw)
        }
        func frame(_ element: AXUIElement) -> CGRect? {
            let (_, p) = read(element, kAXPositionAttribute)
            let (_, s) = read(element, kAXSizeAttribute)
            guard let p, let s, CFGetTypeID(p) == AXValueGetTypeID(), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
            var point = CGPoint.zero, size = CGSize.zero
            guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
            return CGRect(origin: point, size: size)
        }
        var cg: [[String: Any]] = []
        for _ in 0..<100 {
            cg = (CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []).filter {
                ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid &&
                ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == number
            }
            if cg.count == 1, (cg[0][kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true { break }
            Thread.sleep(forTimeInterval: 0.01)
        }
        guard cg.count == 1, let bounds = cg[0][kCGWindowBounds as String] as? [String: Any],
              let expectedFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary),
              (cg[0][kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true,
              (cg[0][kCGWindowLayer as String] as? NSNumber)?.intValue == 0 else {
            let failure: [String: Any] = ["stage":"cg_window", "count":cg.count,
                "onscreen":(cg.first?[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue ?? false,
                "layer":(cg.first?[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1,
                "bounds_present":cg.first?[kCGWindowBounds as String] is [String: Any]]
            FileHandle.standardError.write(try JSONSerialization.data(withJSONObject: failure))
            throw NSError(domain: "owned-background-probe", code: 4)
        }
        deadline = DispatchTime.now().uptimeNanoseconds + 1_000_000_000
        let (_, windowsRaw) = read(application, kAXWindowsAttribute)
        guard let windows = windowsRaw as? [AXUIElement], windows.count == 1,
              let window = windows.first, let actualFrame = frame(window),
              zip([actualFrame.minX,actualFrame.minY,actualFrame.width,actualFrame.height],
                  [expectedFrame.minX,expectedFrame.minY,expectedFrame.width,expectedFrame.height]).allSatisfy({ abs($0 - $1) < 0.5 }) else { throw NSError(domain: "owned-background-probe", code: 5) }
        let allowed: Set<String> = ["AXWindow","AXGroup","AXButton","AXCheckBox","AXStaticText"]
        var stack: [(AXUIElement, AXUIElement, Int)] = [(window, application, 0)]
        var visited: [AXUIElement] = []
        var records: [[String: Any]] = []
        while let (element, expectedParent, depth) = stack.popLast(), records.count < 128,
              depth <= 12, DispatchTime.now().uptimeNanoseconds < deadline {
            guard timed(element) else { break }
            var actualPID: Int32 = 0
            let pidError = AXUIElementGetPid(element, &actualPID)
            let (parentError, parent) = read(element, kAXParentAttribute)
            let (roleError, roleRaw) = read(element, kAXRoleAttribute)
            let role = roleRaw as? String ?? "unknown"
            _ = read(element, kAXSubroleAttribute)
            _ = read(element, kAXTitleAttribute)
            _ = read(element, kAXDescriptionAttribute)
            let (windowError, nodeWindow) = read(element, kAXWindowAttribute)
            let parentMatches = parent.map { CFGetTypeID($0) == AXUIElementGetTypeID() && CFEqual($0, expectedParent) } ?? false
            let duplicate = visited.contains { CFEqual($0, element) }
            visited.append(element)
            var count: CFIndex = 0
            let countError = timed(element) ? AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count) : .cannotComplete
            records.append(["index":records.count,"depth":depth,"role":allowed.contains(role) ? role : "other",
                "pid_error":pidError.rawValue,"pid_matches":actualPID == pid,"parent_error":parentError.rawValue,
                "parent_matches":parentMatches,"parent_matches_window":parent.map { CFEqual($0, window) } ?? false,
                "parent_matches_application":parent.map { CFEqual($0, application) } ?? false,
                "parent_known_node":parent.map { value in visited.firstIndex { CFEqual(value, $0) } ?? -1 } ?? -1,
                "role_error":roleError.rawValue,"role_supported":allowed.contains(role),
                "window_error":windowError.rawValue,"window_matches":nodeWindow.map { CFEqual($0, window) } ?? false,
                "duplicate":duplicate,"children_count_error":countError.rawValue,"children_count":count])
            if !duplicate, countError == .success, count > 0, count <= 128, timed(element) {
                var raw: CFArray?
                let error = AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, count, &raw)
                if error == .success, let children = raw as? [AXUIElement], children.count == count {
                    for item in children.reversed() { stack.append((item, element, depth + 1)) }
                }
            }
        }
        let snapshot = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: args[2])))
        let birthUnchanged = birth(pid) == stamp
        if child.isRunning { child.terminate() }
        child.waitUntilExit()
        let result: [String: Any] = ["kind":"owned_background_tree_readonly","records":records,
            "fixture_pid":pid,"fixture_exit_confirmed":!child.isRunning,
            "process_birth_unchanged":birthUnchanged,"traversal_complete":stack.isEmpty,
            "provider_calls":0,"target_input_calls":0,"fixture_state":snapshot]
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        print(String(decoding: data, as: UTF8.self))
    }
}
