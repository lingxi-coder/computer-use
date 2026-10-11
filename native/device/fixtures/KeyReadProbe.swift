// Readonly focus, keyboard-source and marked-attribute feasibility probe.
import AppKit
import ApplicationServices
import Carbon
import CryptoKit
import Darwin

private struct KeyProbePoint: Decodable {
    let name: String
    let x: Double
    let y: Double
}
private struct KeyProbeRequest: Decodable {
    let pid: Int32
    let bundle_id: String
    let executable_path: String
    let parent_pid: Int32
    let window_id: UInt32
    let setup: String
    let points: [KeyProbePoint]?
}
private func keyProbeTime() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

@MainActor
private final class KeyAXReader {
    let pid: Int32
    let deadline = keyProbeTime() + 2500
    var refs: [AXUIElement] = []
    init(pid: Int32) { self.pid = pid }
    func owner(_ item: AXUIElement) -> Int32? {
        var result: pid_t = 0
        return AXUIElementGetPid(item, &result) == .success ? result : nil
    }
    func timeout(_ item: AXUIElement) -> Bool {
        let now = keyProbeTime()
        return now < deadline && AXUIElementSetMessagingTimeout(item, Float(min(25, deadline - now)) / 1000) == .success
    }
    func id(_ item: AXUIElement) -> String? {
        guard owner(item) == pid else { return nil }
        if let index = refs.firstIndex(where: { CFEqual(item, $0) }) { return "probe-local-\(index + 1)" }
        guard refs.count < 128 else { return nil }; refs.append(item); return "probe-local-\(refs.count)"
    }
    func value(_ item: AXUIElement, _ name: String) -> (CFTypeRef?, AXError) {
        guard owner(item) == pid, timeout(item) else { return (nil, .cannotComplete) }
        var raw: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(item, name as CFString, &raw)
        return (raw, error)
    }
    func scalar(_ item: AXUIElement, _ name: String) -> [String: Any] {
        let (raw, error) = value(item, name)
        var result: [String: Any] = ["error": error.rawValue, "present": raw != nil]
        guard error == .success, let raw else { return result }
        result["cf_type_id"] = CFGetTypeID(raw)
        result["cf_type_name"] = CFCopyTypeIDDescription(CFGetTypeID(raw)) as Any? ?? NSNull()
        if CFGetTypeID(raw) == CFBooleanGetTypeID() {
            result["strict_cf_boolean"] = CFBooleanGetValue((raw as! CFBoolean))
        } else if CFGetTypeID(raw) == CFStringGetTypeID(), !name.contains("Marked") {
            result["string"] = String((raw as! String).prefix(512))
        } else if CFGetTypeID(raw) == AXValueGetTypeID() {
            let ax = raw as! AXValue; result["ax_value_type"] = AXValueGetType(ax).rawValue
            if AXValueGetType(ax) == .cfRange {
                var range = CFRange(location: 0, length: 0)
                if AXValueGetValue(ax, .cfRange, &range) { result["range"] = ["location": range.location, "length": range.length] }
            }
        }
        // Opaque marked text markers are never copied as data or strings.
        return result
    }
    func publicTextInputMarkedRange(_ item: AXUIElement, queryAllowed: Bool) -> [String: Any] {
        guard queryAllowed else {
            return ["attribute_symbol": "NSAccessibilityTextInputMarkedRangeAttribute",
                "query_skipped": "setup, exact owned window, current focus, or CF receiver scope did not verify"]
        }
        let attribute = NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue
        let (raw, error) = value(item, attribute)
        var result: [String: Any] = [
            "attribute_symbol": "NSAccessibilityTextInputMarkedRangeAttribute",
            "attribute_name": attribute,
            "error_code": error.rawValue,
            "present": raw != nil,
            "range_conversion_attempted": false
        ]
        guard error == .success, let raw else { return result }
        result["cf_type_id"] = CFGetTypeID(raw)
        guard CFGetTypeID(raw) == AXValueGetTypeID() else {
            result["ax_value_type_match"] = false
            return result
        }
        result["ax_value_type_match"] = true
        let axValue = unsafeDowncast(raw, to: AXValue.self)
        let valueType = AXValueGetType(axValue)
        result["ax_value_type_raw"] = valueType.rawValue
        guard valueType == .cfRange else { return result }
        result["range_conversion_attempted"] = true
        var range = CFRange(location: 0, length: 0)
        let converted = AXValueGetValue(axValue, .cfRange, &range)
        result["range_conversion_succeeded"] = converted
        if converted {
            result["location_decimal_string"] = String(range.location)
            result["length_decimal_string"] = String(range.length)
        }
        return result
    }
    func element(_ item: AXUIElement, _ name: String) -> (AXUIElement?, AXError) {
        let (raw, error) = value(item, name)
        guard error == .success, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return (nil, error) }
        let target = raw as! AXUIElement
        return owner(target) == pid ? (target, error) : (nil, .invalidUIElement)
    }
    func frame(_ item: AXUIElement) -> [String: CGFloat]? {
        guard let p = value(item, kAXPositionAttribute).0, CFGetTypeID(p) == AXValueGetTypeID(),
              let s = value(item, kAXSizeAttribute).0, CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
        return ["x": point.x, "y": point.y, "width": size.width, "height": size.height]
    }
    func describe(_ item: AXUIElement) -> [String: Any] {
        guard let identifier = id(item) else { return ["unavailable": true] }
        var names: CFArray?
        let namesError = timeout(item) ? AXUIElementCopyAttributeNames(item, &names) : .cannotComplete
        var result: [String: Any] = ["diagnostic_id": identifier, "pid": pid,
            "role": scalar(item, kAXRoleAttribute), "subrole": scalar(item, kAXSubroleAttribute),
            "focused": scalar(item, kAXFocusedAttribute), "enabled": scalar(item, kAXEnabledAttribute),
            "frame": frame(item) as Any? ?? NSNull(), "attribute_names_error": namesError.rawValue,
            "attribute_names": (names as? [String]).map { Array($0.prefix(128)) } as Any? ?? NSNull()]
        for attribute in [kAXWindowAttribute, kAXTopLevelUIElementAttribute] {
            let (target, error) = element(item, attribute)
            result[attribute] = ["error": error.rawValue, "diagnostic_id": target.flatMap(id) as Any? ?? NSNull(),
                "pid": target.flatMap(owner) as Any? ?? NSNull(), "frame": target.flatMap(frame) as Any? ?? NSNull()]
        }
        for attribute in ["AXMarkedTextRange", "AXMarkedRange", "AXHasMarkedText", "AXTextInputMarkedTextMarkerRange", kAXSelectedTextRangeAttribute] {
            result[attribute] = scalar(item, attribute)
        }
        let (shared, sharedError) = value(item, "AXSharedFocusElements")
        result["AXSharedFocusElements"] = ["error": sharedError.rawValue,
            "diagnostic_ids": (shared as? [AXUIElement]).map { $0.prefix(16).compactMap(id) } as Any? ?? NSNull()]
        return result
    }
    func parents(_ item: AXUIElement) -> [[String: Any]] {
        var cursor = item, visited: [AXUIElement] = [], result: [[String: Any]] = []
        for _ in 0..<16 {
            guard keyProbeTime() < deadline, owner(cursor) == pid,
                  !visited.contains(where: { CFEqual($0, cursor) }) else { break }
            visited.append(cursor)
            let (parent, parentError) = element(cursor, kAXParentAttribute)
            var record: [String: Any] = ["diagnostic_id": id(cursor) as Any? ?? NSNull(), "pid": pid,
                "role": scalar(cursor, kAXRoleAttribute), "frame": frame(cursor) as Any? ?? NSNull(), "parent_error": parentError.rawValue,
                "parent_diagnostic_id": parent.flatMap(id) as Any? ?? NSNull()]
            if let parent {
                let (children, error) = value(parent, kAXChildrenAttribute)
                let matches = (children as? [AXUIElement])?.filter { owner($0) == pid && CFEqual($0, cursor) }.count
                record["parent_children_error"] = error.rawValue
                record["actual_cf_membership_count"] = matches as Any? ?? NSNull()
            }
            result.append(record)
            guard let parent else { break }; cursor = parent
        }
        return result
    }
    func tree(_ application: AXUIElement) -> [AXUIElement] {
        var queue = [(application, 0)], result: [AXUIElement] = []
        while !queue.isEmpty, result.count < 48, keyProbeTime() < deadline {
            let (item, depth) = queue.removeFirst()
            guard owner(item) == pid, !result.contains(where: { CFEqual($0, item) }) else { continue }
            result.append(item); guard depth < 6 else { continue }
            for attribute in [kAXWindowsAttribute, kAXChildrenAttribute] {
                if let children = value(item, attribute).0 as? [AXUIElement] { queue.append(contentsOf: children.prefix(32).map { ($0, depth + 1) }) }
            }
        }
        return result
    }
}

private func tisDescription(_ unmanaged: Unmanaged<TISInputSource>?) -> [String: Any] {
    guard let source = unmanaged?.takeRetainedValue() else { return ["available": false] }
    func property(_ name: CFString) -> CFTypeRef? {
        guard let pointer = TISGetInputSourceProperty(source, name) else { return nil }
        return Unmanaged<CFTypeRef>.fromOpaque(pointer).takeUnretainedValue()
    }
    var result: [String: Any] = ["available": true]
    for (label, name) in [("source_id", kTISPropertyInputSourceID), ("source_type", kTISPropertyInputSourceType),
        ("source_category", kTISPropertyInputSourceCategory), ("input_mode_id", kTISPropertyInputModeID)] {
        result[label] = (property(name!) as? String) as Any? ?? NSNull()
    }
    if let raw = property(kTISPropertyInputSourceIsASCIICapable), CFGetTypeID(raw) == CFBooleanGetTypeID() {
        result["ascii_capable"] = CFBooleanGetValue((raw as! CFBoolean))
    }
    if let raw = property(kTISPropertyUnicodeKeyLayoutData), CFGetTypeID(raw) == CFDataGetTypeID() {
        let data = raw as! CFData, count = CFDataGetLength(data)
        result["unicode_layout_byte_length"] = count
        if count <= 64 * 1024 { result["unicode_layout_sha256"] = SHA256.hash(data: data as Data).map { String(format: "%02x", $0) }.joined() }
        else { result["unicode_layout_hash_unavailable"] = "layout exceeds 64 KiB read budget" }
    } else { result["unicode_layout_byte_length"] = NSNull() }
    return result
}
private func tisSnapshot() -> [String: Any] {
    let flags = CGEventSource.flagsState(.combinedSessionState)
    return ["selected_input_source": tisDescription(TISCopyCurrentKeyboardInputSource()),
        "keyboard_layout_source": tisDescription(TISCopyCurrentKeyboardLayoutInputSource()),
        "keyboard_type": UInt32(LMGetKbdType()), "keyboard_type_api": "LMGetKbdType",
        "caps_lock": flags.contains(.maskAlphaShift), "modifier_flags": flags.rawValue,
        "modifier_flags_source": "combinedSessionState"]
}

/// Each source sample makes exactly 128 bounded key-state queries. These are
/// actual system reads, independent of the helper's held-input ledger. Core
/// Graphics offers no atomic multi-key snapshot or per-query error result.
private func systemKeyboardState(_ state: CGEventSourceStateID, name: String) -> [String: Any] {
    let started = DispatchTime.now().uptimeNanoseconds
    let flagsBefore = CGEventSource.flagsState(state)
    var pressed: [UInt16] = [], queryCount = 0
    for code: UInt16 in 0...127 {
        if CGEventSource.keyState(state, key: code) { pressed.append(code) }
        queryCount += 1
    }
    let flagsAfter = CGEventSource.flagsState(state)
    let ended = DispatchTime.now().uptimeNanoseconds
    return ["source": name, "source_id": state.rawValue,
        "capture_started_uptime_ns": String(started), "capture_ended_uptime_ns": String(ended),
        "capture_complete": queryCount == 128, "atomic_snapshot": false,
        "queried_virtual_code_min": 0, "queried_virtual_code_max": 127, "key_query_count": queryCount,
        "pressed_virtual_codes": pressed, "flags_before": flagsBefore.rawValue, "flags_after": flagsAfter.rawValue,
        "flags_unchanged": flagsBefore == flagsAfter, "key_query_error_reporting_available": false]
}
private func systemKeyboardSnapshot() -> [String: Any] {
    ["combinedSessionState": systemKeyboardState(.combinedSessionState, name: "combinedSessionState"),
        "hidSystemState": systemKeyboardState(.hidSystemState, name: "hidSystemState")]
}
private func cgWindowFrame(_ window: [String: Any]) -> CGRect? {
    guard let raw = window[kCGWindowBounds as String] as? NSDictionary else { return nil }
    var rect = CGRect.zero
    return CGRectMakeWithDictionaryRepresentation(raw, &rect) ? rect : nil
}
private func sameFrame(_ lhs: [String: CGFloat]?, _ rhs: CGRect?) -> Bool {
    guard let lhs, let rhs else { return false }
    return abs(lhs["x"]! - rhs.origin.x) <= 0.75 && abs(lhs["y"]! - rhs.origin.y) <= 0.75 &&
        abs(lhs["width"]! - rhs.width) <= 0.75 && abs(lhs["height"]! - rhs.height) <= 0.75
}

@main
struct KeyReadProbeMain {
    @MainActor static func main() {
        do {
            let input = FileHandle.standardInput.readDataToEndOfFile()
            guard input.count <= 8192 else { throw NSError(domain: "key-probe request budget", code: 1) }
            let request = try JSONDecoder().decode(KeyProbeRequest.self, from: input)
            var stamp = proc_bsdinfo(), buffer = [CChar](repeating: 0, count: 4096)
            let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
            let expected = URL(fileURLWithPath: request.executable_path).resolvingSymlinksInPath().standardizedFileURL.path
            guard request.pid > 0, request.parent_pid == getppid(), request.window_id > 0,
                  request.bundle_id == "com.lingxi.computer-user.guard-fixture", expected.contains("/target/native-macos/"),
                  expected.hasSuffix("/KeyFixture.app/Contents/MacOS/KeyFixture"),
                  proc_pidinfo(request.pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size, stamp.pbi_ppid == UInt32(request.parent_pid),
                  proc_pidpath(request.pid, &buffer, UInt32(buffer.count)) > 0,
                  String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self) == expected,
                  let app = NSRunningApplication(processIdentifier: request.pid), !app.isTerminated,
                  app.bundleIdentifier == request.bundle_id, AXIsProcessTrusted(),
                  ["focus-toggle", "focus-text", "mark-text", "unmark-text", "focus-secure", "focus-none", "replace-control"].contains(request.setup),
                  (request.points?.count ?? 0) <= 8,
                  request.points?.allSatisfy({ $0.name.utf8.count <= 64 && $0.x.isFinite && $0.y.isFinite && abs($0.x) <= 100_000 && abs($0.y) <= 100_000 }) ?? true
            else { throw NSError(domain: "owned key probe scope unavailable", code: 2) }
            let nativeWindows = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []).filter {
                ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == request.pid
            }
            let scopedWindows = nativeWindows.filter {
                ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == request.window_id &&
                    ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0
            }
            guard scopedWindows.count == 1 else {
                throw NSError(domain: "owned key origin window absent", code: 3)
            }
            let keyboardBefore = systemKeyboardSnapshot()
            let tisBefore = tisSnapshot(), reader = KeyAXReader(pid: request.pid)
            let application = AXUIElementCreateApplication(request.pid), system = AXUIElementCreateSystemWide()
            var rawApplication: CFTypeRef?
            let systemError = reader.timeout(system) ? AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &rawApplication) : .cannotComplete
            var focusedAppPID: Int32?
            if let rawApplication, CFGetTypeID(rawApplication) == AXUIElementGetTypeID() { focusedAppPID = reader.owner(rawApplication as! AXUIElement) }
            let (focusedWindow, windowError) = reader.element(application, kAXFocusedWindowAttribute)
            let (focused, focusError) = reader.element(application, kAXFocusedUIElementAttribute)
            // Read the primary focus facts first; optional discovery cannot
            // spend the budget and then silently substitute unknown fields.
            let focusedWindowRead: [String: Any] = ["error": windowError.rawValue, "element": focusedWindow.map(reader.describe) as Any? ?? NSNull()]
            let focusedElementRead: [String: Any] = ["error": focusError.rawValue, "element": focused.map(reader.describe) as Any? ?? NSNull(),
                "parent_chain": focused.map(reader.parents) as Any? ?? NSNull()]
            var pointReads: [[String: Any]] = []
            for point in request.points ?? [] {
                var item: AXUIElement?
                let error = reader.timeout(system) ? AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &item) : .cannotComplete
                var record: [String: Any] = ["name": point.name, "x": point.x, "y": point.y, "error": error.rawValue]
                if let item {
                    record["returned_pid"] = reader.owner(item) as Any? ?? NSNull()
                    if reader.owner(item) == request.pid {
                        record["element"] = reader.describe(item); record["parent_chain"] = reader.parents(item)
                        record["same_as_focused_cf"] = focused.map { CFEqual($0, item) } as Any? ?? NSNull()
                    }
                }
                pointReads.append(record)
            }
            let controlItems = reader.tree(application).filter { item in
                let role = reader.value(item, kAXRoleAttribute).0 as? String
                return [kAXCheckBoxRole, kAXTextFieldRole, kAXTextAreaRole].contains(role ?? "")
            }
            let controls = controlItems.prefix(16).map { item -> [String: Any] in
                ["element": reader.describe(item), "parent_chain": reader.parents(item),
                    "same_as_focused_cf": focused.map { CFEqual($0, item) } as Any? ?? NSNull()]
            }
            let exactFocusedTextControls = controlItems.filter { item in
                guard focused.map({ CFEqual($0, item) }) == true else { return false }
                let role = reader.value(item, kAXRoleAttribute).0 as? String
                return [kAXTextFieldRole, kAXTextAreaRole].contains(role ?? "")
            }
            let focusedReceiverWindow = focused.map { reader.element($0, kAXWindowAttribute).0 } ?? nil
            let focusWindowFrameMatches: Bool
            if let focusedWindow {
                focusWindowFrameMatches = sameFrame(reader.frame(focusedWindow), scopedWindows.first.flatMap(cgWindowFrame))
            } else {
                focusWindowFrameMatches = false
            }
            let exactFocusScopeVerified = ["focus-text", "mark-text", "unmark-text"].contains(request.setup) &&
                request.window_id > 0 && scopedWindows.count == 1 &&
                focusedAppPID == request.pid && systemError == .success && focusedWindow != nil && windowError == .success &&
                focusedWindow.map { appWindow in
                    focusedReceiverWindow.map { CFEqual($0, appWindow) } ?? false
                } == true && exactFocusedTextControls.count == 1 && focusError == .success && focusWindowFrameMatches
            let publicMarkedRange = reader.publicTextInputMarkedRange(
                focused ?? AXUIElementCreateSystemWide(), queryAllowed: exactFocusScopeVerified)
            let tisAfter = tisSnapshot()
            let keyboardAfter = systemKeyboardSnapshot()
            var finalStamp = proc_bsdinfo()
            let unchanged = proc_pidinfo(request.pid, PROC_PIDTBSDINFO, 0, &finalStamp, size) == size &&
                finalStamp.pbi_start_tvsec == stamp.pbi_start_tvsec && finalStamp.pbi_start_tvusec == stamp.pbi_start_tvusec &&
                finalStamp.pbi_ppid == stamp.pbi_ppid && AXIsProcessTrusted()
            let result: [String: Any] = ["kind": "key_read_probe", "pid": request.pid, "bundle_id": request.bundle_id,
                "input_authorized": false, "mvp_complete": false, "process_identity_revalidated": unchanged,
                "process_start_seconds": stamp.pbi_start_tvsec, "process_start_micros": stamp.pbi_start_tvusec,
                "parent_pid": stamp.pbi_ppid, "executable_path": expected,
                "focused_application": ["error": systemError.rawValue, "pid": focusedAppPID as Any? ?? NSNull(), "same_owned_pid": focusedAppPID == request.pid],
                "setup": request.setup,
                "public_text_input_marked_range": publicMarkedRange,
                "public_text_input_marked_range_scope": [
                    "query_allowed": exactFocusScopeVerified,
                    "request_window_id": request.window_id,
                    "matching_on_screen_standard_cg_window_count": scopedWindows.count,
                    "focused_ax_window_error": windowError.rawValue,
                    "focused_receiver_error": focusError.rawValue,
                    "focused_receiver_window_same_cf": focusedWindow.map { window in
                        focusedReceiverWindow.map { CFEqual($0, window) } ?? false
                    } ?? false,
                    "focused_receiver_text_control_same_cf_count": exactFocusedTextControls.count,
                    "focused_window_frame_matches_requested_cg_window": focusWindowFrameMatches
                ],
                "focused_window": focusedWindowRead, "focused_element": focusedElementRead,
                "point_reads": pointReads, "control_reads": controls,
                "native_windows": nativeWindows.prefix(16).map { ["id": $0[kCGWindowNumber as String] ?? NSNull(), "layer": $0[kCGWindowLayer as String] ?? NSNull(), "frame": $0[kCGWindowBounds as String] ?? NSNull()] },
                "tis_before": tisBefore, "tis_after": tisAfter, "tis_metadata_unchanged": NSDictionary(dictionary: tisBefore).isEqual(to: tisAfter),
                "system_keyboard_before": keyboardBefore, "system_keyboard_after": keyboardAfter,
                "deadline_exceeded": keyProbeTime() >= reader.deadline,
                "composition_verified": false, "selected_range_is_not_composition_evidence": true,
                "diagnostic_ids_are_process_local_only": true]
            let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
            FileHandle.standardOutput.write(data + Data([10]))
        } catch {
            let result: [String: Any] = ["kind": "key_read_probe", "error": String(describing: error), "input_authorized": false]
            if let data = try? JSONSerialization.data(withJSONObject: result) { FileHandle.standardOutput.write(data + Data([10])) }
            exit(2)
        }
    }
}
