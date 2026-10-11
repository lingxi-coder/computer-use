// Read-only failure diagnostics for the owned Operator smoke's fixture.
import AppKit
import ApplicationServices
import Darwin

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]),
      let parent = NSRunningApplication(processIdentifier: getppid()), parent.bundleIdentifier == "com.github.Electron",
      let target = NSRunningApplication(processIdentifier: pid), !target.isTerminated,
      target.bundleIdentifier == "com.lingxi.computer-user.guard-fixture", AXIsProcessTrusted() else { exit(2) }
let element = AXUIElementCreateApplication(pid)
AXUIElementSetMessagingTimeout(element, 0.2)
var before = 0, after = 0
let beforeError = AXUIElementGetAttributeValueCount(element, kAXWindowsAttribute as CFString, &before)
var value: CFTypeRef?
let valueError = AXUIElementCopyAttributeValue(element, kAXWindowsAttribute as CFString, &value)
let afterError = AXUIElementGetAttributeValueCount(element, kAXWindowsAttribute as CFString, &after)
func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
    AXUIElementSetMessagingTimeout(element, 0.04)
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, key as CFString, &value) == .success ? value : nil
}
let windows = (value as? [AXUIElement] ?? []).prefix(8).map { window -> [String: Any] in
    var point = CGPoint.zero, size = CGSize.zero
    if let value = attribute(window, kAXPositionAttribute), CFGetTypeID(value) == AXValueGetTypeID() {
        _ = AXValueGetValue(unsafeDowncast(value, to: AXValue.self), .cgPoint, &point)
    }
    if let value = attribute(window, kAXSizeAttribute), CFGetTypeID(value) == AXValueGetTypeID() {
        _ = AXValueGetValue(unsafeDowncast(value, to: AXValue.self), .cgSize, &size)
    }
    return ["role": attribute(window, kAXRoleAttribute) as? String ?? "unknown",
        "subrole": attribute(window, kAXSubroleAttribute) as? String ?? "unknown",
        "frame": [point.x, point.y, size.width, size.height]]
}
let nativeWindows = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? [])
    .filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid }.prefix(8).map { window -> [String: Any] in
        ["id": window[kCGWindowNumber as String] ?? -1, "layer": window[kCGWindowLayer as String] ?? -1,
            "frame": window[kCGWindowBounds as String] ?? [:]]
    }
let result: [String: Any] = ["pid": pid, "count_before": before, "count_before_error": beforeError.rawValue,
    "value_count": (value as? [AXUIElement])?.count ?? -1, "value_error": valueError.rawValue,
    "count_after": after, "count_after_error": afterError.rawValue, "windows": windows, "on_screen_windows": nativeWindows]
if let data = try? JSONSerialization.data(withJSONObject: result), let json = String(data: data, encoding: .utf8) { print(json) }
