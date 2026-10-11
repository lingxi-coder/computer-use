// Read-only window-server evidence for the owned headless browser diagnostic.
import AppKit
import ApplicationServices
import Darwin
import Foundation

func reject(_ stage: String) -> Never {
    fputs("headless-display-probe: \(stage)\n", stderr)
    exit(2)
}
guard CommandLine.arguments.count <= 2 else { reject("arguments") }
let ownedPID: pid_t?
if CommandLine.arguments.count == 2 {
    guard let pid = Int32(CommandLine.arguments[1]), pid > 0 else { reject("owned-browser") }
    var pathBuffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &pathBuffer, UInt32(pathBuffer.count)) > 0,
          let end = pathBuffer.firstIndex(of: 0) else { reject("owned-browser") }
    let pathBytes = pathBuffer[..<end].map { UInt8(bitPattern: $0) }
    guard let executablePath = String(bytes: pathBytes, encoding: .utf8),
          URL(fileURLWithPath: executablePath).lastPathComponent == "Google Chrome" else { reject("owned-browser") }
    ownedPID = pid
} else { ownedPID = nil }
guard let foregroundPID = NSWorkspace.shared.frontmostApplication?.processIdentifier,
      let raw = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID),
      let inventory = raw as? [[String: Any]], !inventory.isEmpty, inventory.count <= 512 else { reject("display-inventory") }
var foregroundWindows: [UInt32] = []
var ownedWindows = 0
for item in inventory {
    guard let owner = item[kCGWindowOwnerPID as String] as? NSNumber,
          let layer = item[kCGWindowLayer as String] as? NSNumber,
          let id = item[kCGWindowNumber as String] as? NSNumber,
          id.uint32Value > 0,
          let onScreen = item[kCGWindowIsOnscreen as String] as? NSNumber,
          onScreen.boolValue else { reject("inventory-fields") }
    if owner.int32Value == foregroundPID && layer.intValue == 0 { foregroundWindows.append(id.uint32Value) }
    if owner.int32Value == ownedPID { ownedWindows += 1 }
}
// A foreground application may own no ordinary visible window. The fully
// inspected inventory then proves an empty set; it is not missing evidence.
guard foregroundPID > 0,
      NSWorkspace.shared.frontmostApplication?.processIdentifier == foregroundPID else { reject("foreground-continuity") }
let record: [String: Any] = [
    "foreground_pid": foregroundPID,
    "foreground_visible_window_ids": foregroundWindows.sorted(),
    "foreground_visible_window_state": foregroundWindows.isEmpty ? "none" : "present",
    "foreground_evidence": "NSWorkspace.frontmostApplication+CGWindowList(onScreen)",
    "focused_window_evidence": "unavailable-no-AX-focus-query",
    "owned_browser_pid": ownedPID.map { $0 as Any } ?? NSNull(),
    "owned_browser_onscreen_window_count": ownedPID.map { _ in ownedWindows as Any } ?? NSNull(),
]
guard let data = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]),
      let text = String(data: data, encoding: .utf8) else { reject("encoding") }
print(text)
