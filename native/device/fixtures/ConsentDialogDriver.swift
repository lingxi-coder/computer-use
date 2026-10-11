// Manual product smoke only. Can press the known consent dialog in its own
// Electron parent; it cannot select another process or an arbitrary control.
import AppKit
import ApplicationServices
import Darwin

private let diagnosticRoleNames = [
    "AXApplication", "AXWindow", "AXSheet", "AXDialog", "AXButton", "AXStaticText",
    "AXTextField", "AXTextArea", "AXGroup", "AXScrollArea", "AXUnknown", "missing", "other",
]
private let diagnosticSubroleNames = ["AXStandardWindow", "AXSystemDialog", "AXDialog", "AXUnknown", "missing", "other"]

private struct ScannedAXNode {
    let element: AXUIElement
    let role: String?
    let subrole: String?
}

private struct ConsentDiagnostics {
    var windowsCount = 0
    var windowsCountCapped = false
    var windowsAPIError: Int32 = 0
    var modalCount = 0
    var matchedMessageCount = 0
    var matchingButtonCount = 0
    var matchingEnabledButtonCount = 0
    var scannedRoleCounts = Dictionary(uniqueKeysWithValues: diagnosticRoleNames.map { ($0, 0) })
    var scannedSubroleCounts = Dictionary(uniqueKeysWithValues: diagnosticSubroleNames.map { ($0, 0) })

    mutating func resetAttempt() {
        windowsCount = 0
        windowsCountCapped = false
        windowsAPIError = 0
        modalCount = 0
        matchedMessageCount = 0
        matchingButtonCount = 0
        matchingEnabledButtonCount = 0
        for key in diagnosticRoleNames { scannedRoleCounts[key] = 0 }
        for key in diagnosticSubroleNames { scannedSubroleCounts[key] = 0 }
    }

    mutating func recordRole(_ role: String?) {
        let key = role.flatMap { diagnosticRoleNames.contains($0) ? $0 : nil } ?? (role == nil ? "missing" : "other")
        scannedRoleCounts[key, default: 0] += 1
    }

    mutating func recordSubrole(_ subrole: String?) {
        let key = subrole.flatMap { diagnosticSubroleNames.contains($0) ? $0 : nil } ?? (subrole == nil ? "missing" : "other")
        scannedSubroleCounts[key, default: 0] += 1
    }
}

func consentMessageMatches(_ candidate: String?, expected: String) -> Bool {
    guard let candidate, candidate.utf8.count <= 8_192 else { return false }
    if candidate == expected { return true }
    guard candidate.hasPrefix(expected) else { return false }
    let detail = candidate.dropFirst(expected.count)
    guard let separator = detail.first, separator.isWhitespace else { return false }
    return detail.dropFirst().contains { !$0.isWhitespace }
}

if CommandLine.arguments.count == 2, CommandLine.arguments[1] == "--self-test" {
    let expected = "Allow Computer Use to use this model for the selected purposes?"
    let cases: [(String, String?, Bool)] = [
        ("exact message", expected, true),
        ("message with inline detail", expected + " Service: https://example.invalid/v1", true),
        ("message with multiline detail", expected + "\nService: https://example.invalid/v1\nModel: owned-consent", true),
        ("missing separator", expected + "Service: https://example.invalid/v1", false),
        ("different message", "Allow Computer Use to use a different model?", false),
        ("leading text", "Notice: " + expected, false),
        ("whitespace without detail", expected + " \n\t", false),
        ("missing value", nil, false),
        ("oversized value", expected + " " + String(repeating: "x", count: 8_192), false),
    ]
    let failed = cases.filter { name, candidate, expectedResult in
        let actual = consentMessageMatches(candidate, expected: expected)
        if actual != expectedResult { fputs("consent matcher self-test failed: \(name)\n", stderr) }
        return actual != expectedResult
    }
    guard failed.isEmpty else { exit(1) }
    print("{\"ok\":true,\"matcher_cases\":\(cases.count)}")
    exit(0)
}

func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
    AXUIElementSetMessagingTimeout(element, 0.2)
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, key as CFString, &value) == .success else { return nil }
    return value
}
let action = CommandLine.arguments.dropFirst().first ?? ""
let kind = CommandLine.arguments.count == 3 ? CommandLine.arguments[2] : "window"
guard (2...3).contains(CommandLine.arguments.count), ["cancel", "allow"].contains(action),
      ["window", "background-ax", "model", "owned-edit", "owned-save"].contains(kind),
      let parent = NSRunningApplication(processIdentifier: getppid()),
      parent.bundleIdentifier == "com.github.Electron", AXIsProcessTrusted() else { exit(2) }
let app = AXUIElementCreateApplication(parent.processIdentifier)
AXUIElementSetMessagingTimeout(app, 0.2)
let expected: String
let allowButton: String
switch kind {
case "background-ax":
    expected = "Allow Computer Use to press verified controls in this background window?"
    allowButton = "Allow background AXPress"
case "model":
    expected = "Allow Computer Use to use this model for the selected purposes?"
    allowButton = "Allow selected purposes"
case "owned-edit":
    expected = "Allow Computer Use to edit its temporary TextEdit document?"
    allowButton = "Allow editing"
case "owned-save":
    expected = "Allow Computer Use to save and reopen its temporary TextEdit document?"
    allowButton = "Allow saving and reopening"
default:
    expected = "Allow Computer Use to read ordinary text and edit registered controls in this window?"
    allowButton = "Allow this window"
}
let button = action == "allow" ? allowButton : "Cancel"
let deadline = Date().addingTimeInterval(8)
private func descendants(_ roots: [AXUIElement], diagnostics: inout ConsentDiagnostics) -> [ScannedAXNode] {
    var pending = roots.map { ($0, 0) }
    var nodes: [ScannedAXNode] = []
    while let (element, depth) = pending.popLast(), nodes.count < 128, Date() < deadline {
        let role = attribute(element, kAXRoleAttribute) as? String
        if role == "AXWebArea" { continue }
        diagnostics.recordRole(role)
        let subrole = attribute(element, kAXSubroleAttribute) as? String
        diagnostics.recordSubrole(subrole)
        nodes.append(ScannedAXNode(element: element, role: role, subrole: subrole))
        if depth < 12 {
            let children = attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
            let sheets = attribute(element, "AXSheets") as? [AXUIElement] ?? []
            pending.append(contentsOf: (children + sheets).map { ($0, depth + 1) })
        }
    }
    return nodes
}
var lastPressError: Int32?
private var diagnostics = ConsentDiagnostics()
while Date() < deadline {
    diagnostics.resetAttempt()
    var windowsValue: CFTypeRef?
    let windowsResult = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windowsValue)
    diagnostics.windowsAPIError = windowsResult.rawValue
    let windows = windowsResult == .success ? (windowsValue as? [AXUIElement] ?? []) : []
    diagnostics.windowsCount = min(windows.count, 128)
    diagnostics.windowsCountCapped = windows.count > 128
    let modals = descendants(windows, diagnostics: &diagnostics).filter { node in
        node.role == "AXSheet"
            || ["AXDialog", "AXSystemDialog"].contains(node.subrole ?? "")
    }
    diagnostics.modalCount = modals.count
    for modal in modals where Date() < deadline {
        let nodes = descendants([modal.element], diagnostics: &diagnostics)
        let hasMessage = nodes.contains { node in
            [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute].contains {
                consentMessageMatches(attribute(node.element, $0) as? String, expected: expected)
            }
        }
        if hasMessage { diagnostics.matchedMessageCount += 1 }
        var target: AXUIElement?
        for node in nodes where node.role == kAXButtonRole && Date() < deadline {
            guard attribute(node.element, kAXTitleAttribute) as? String == button else { continue }
            diagnostics.matchingButtonCount += 1
            if attribute(node.element, kAXEnabledAttribute) as? Bool == true {
                diagnostics.matchingEnabledButtonCount += 1
                if target == nil { target = node.element }
            }
        }
        if hasMessage, let target {
            // Closing a native sheet can wait for AppKit/Electron dispatch.
            // The short per-attribute lookup budget is not an action budget.
            AXUIElementSetMessagingTimeout(target, 2)
            let result = AXUIElementPerformAction(target, kAXPressAction as CFString)
            if result == .success { print("{\"status\":\"pressed\",\"button\":\"\(button)\"}"); exit(0) }
            lastPressError = result.rawValue
        }
    }
    usleep(50_000)
}
func countObject(_ counts: [String: Int]) -> String {
    let entries = counts.keys.sorted().compactMap { key in counts[key].map { "\"\(key)\":\($0)" } }
    return "{\(entries.joined(separator: ","))}"
}
fputs("owned native consent dialog was not found or ready; last AXPress error \(lastPressError.map(String.init) ?? "none")\n", stderr)
fputs("{\"kind\":\"owned_native_consent_failure\",\"last_ax_press_error\":\(lastPressError.map(String.init) ?? "null"),\"windows_count\":\(diagnostics.windowsCount),\"windows_count_capped\":\(diagnostics.windowsCountCapped),\"windows_api_error\":\(diagnostics.windowsAPIError),\"modal_count\":\(diagnostics.modalCount),\"scanned_role_counts\":\(countObject(diagnostics.scannedRoleCounts)),\"scanned_subrole_counts\":\(countObject(diagnostics.scannedSubroleCounts)),\"matched_message_count\":\(diagnostics.matchedMessageCount),\"matching_button_count\":\(diagnostics.matchingButtonCount),\"matching_enabled_button_count\":\(diagnostics.matchingEnabledButtonCount)}\n", stderr)
exit(4)
