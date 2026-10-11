// Read-only foreground identity evidence for the owned background diagnostic.
import AppKit
import ApplicationServices
import Darwin
import Foundation

func reject(_ stage: String) -> Never {
    fputs("background-focus-probe: \(stage)\n", stderr)
    exit(2)
}
guard CommandLine.arguments.count == 1 else { reject("arguments") }
guard NSRunningApplication(processIdentifier: getppid())?.bundleIdentifier == "com.github.Electron" else { reject("parent") }
guard AXIsProcessTrusted() else { reject("ax-trust") }
guard let frontmost = NSWorkspace.shared.frontmostApplication else { reject("frontmost") }

// Establish the window-server query context before querying system AX focus,
// as required by the same read-only probes used by native Source.
guard CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) != nil else { reject("window-server") }

func copy(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    guard AXUIElementSetMessagingTimeout(element, 0.1) == .success else { return nil }
    var value: CFTypeRef?
    let result = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    if result != .success { fputs("background-focus-probe: \(name) error \(result.rawValue)\n", stderr) }
    return result == .success ? value : nil
}

// NSWorkspace reports the actual key-event receiving application. Independently
// require its AXFrontmost fact and actual AXFocusedWindow; no target PID or
// window supplied by the diagnostic caller is used as a focus fact.
let pid = frontmost.processIdentifier
let app = AXUIElementCreateApplication(pid)
guard copy(app, kAXFrontmostAttribute) as? Bool == true,
      let rawWindow = copy(app, kAXFocusedWindowAttribute),
      CFGetTypeID(rawWindow) == AXUIElementGetTypeID() else { reject("focused-window-or-pid") }
let window = unsafeDowncast(rawWindow, to: AXUIElement.self)
var windowPID: pid_t = 0
guard AXUIElementGetPid(window, &windowPID) == .success, windowPID == pid,
      let rawPosition = copy(window, kAXPositionAttribute),
      let rawSize = copy(window, kAXSizeAttribute),
      CFGetTypeID(rawPosition) == AXValueGetTypeID(), CFGetTypeID(rawSize) == AXValueGetTypeID() else { reject("window-geometry") }
var point = CGPoint.zero
var size = CGSize.zero
guard AXValueGetValue(unsafeDowncast(rawPosition, to: AXValue.self), .cgPoint, &point),
      AXValueGetValue(unsafeDowncast(rawSize, to: AXValue.self), .cgSize, &size),
      [point.x, point.y, size.width, size.height].allSatisfy({ $0.isFinite }),
      size.width > 0, size.height > 0,
      let inventory = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
      inventory.count <= 256 else { reject("cg-inventory") }
let matches = inventory.filter { item in
    guard (item[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
          (item[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
          let bounds = item[kCGWindowBounds as String] as? [String: NSNumber] else { return false }
    return bounds["X"]?.doubleValue == point.x && bounds["Y"]?.doubleValue == point.y
        && bounds["Width"]?.doubleValue == size.width && bounds["Height"]?.doubleValue == size.height
}
guard matches.count == 1, let id = matches[0][kCGWindowNumber as String] as? NSNumber,
      id.uint32Value > 0, NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
      copy(app, kAXFrontmostAttribute) as? Bool == true,
      let secondWindow = copy(app, kAXFocusedWindowAttribute), CFEqual(secondWindow, window) else { reject("unique-window-and-focus-continuity") }
let record: [String: Any] = ["pid": pid, "window_id": "window-\(id.uint32Value)",
    "app_evidence": "NSWorkspace.frontmostApplication+AXFrontmost", "window_evidence": "AXFocusedWindow+CGWindow"]
if let data = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]),
   let text = String(data: data, encoding: .utf8) { print(text) } else { exit(2) }
