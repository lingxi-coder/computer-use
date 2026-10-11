// Bounded, read-only AX/CG/TIS observations for a launcher-owned TextEdit instance.
import AppKit
import ApplicationServices
import Carbon
import CryptoKit
import Darwin

private struct UnicodeProbeRequest: Decodable {
    let target_pid: Int32
    let probe_parent_pid: Int32
    let launcher_pid: Int32
    let launcher_id: String
    let executable_path: String
    let bundle_id: String
    let document_path: String
    let document_sha256: String
    let expected_text: String
    let run_id: String
    let launch_receipt: UnicodeLaunchReceipt
    let activation_observation: UnicodeActivationObservation?
}

private struct UnicodeActivationObservation: Decodable {
    let attempted: Bool
    let attempt_count: Int
    let return_value: Bool?
    let status: String
    let window_verified: Bool
    let cg_window_count: Int
    let ax_window_count: Int
    let cg_window_id: UInt32?
    let cg_title: String?
    let ax_title: String?
    let frames_match: Bool
    let system_focused_application_pid_before_activation: Int32?
}

private struct UnicodeKernelIdentity: Decodable {
    let pid: Int32
    let parent_pid: Int32
    let start_seconds: Int64
    let start_microseconds: Int64
    let executable_path: String
    let parent_executable_path: String?
}

private struct UnicodeLaunchReceipt: Decodable {
    let version: Int
    let run_id: String
    let launcher_id: String
    let launcher_identity: UnicodeKernelIdentity
    let target_identity: UnicodeKernelIdentity
    let bundle_identifier: String
    let application_path: String
    let executable_path: String
    let document_path: String
    let document_sha256: String
    let preexisting_textedit_pids: [Int32]
    let launch_start_unix_milliseconds: Int64
    let launch_finish_unix_milliseconds: Int64
    let new_pid_excluded_from_preexisting_set: Bool
    let target_started_within_launch_interval: Bool
    let creates_new_application_instance: Bool
    let adds_to_recent_items: Bool
    let activates_new_instance: Bool
    let arguments: [String]
    let target_parent_is_launchd: Bool
    let no_input_authority: Bool
}

private func uptimeMs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

private func normalizedPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
}

private func processIdentity(_ pid: Int32) -> [String: Any]? {
    var stamp = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
    guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size else { return nil }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return [
        "pid": pid,
        "ppid": stamp.pbi_ppid,
        "start_seconds": stamp.pbi_start_tvsec,
        "start_microseconds": stamp.pbi_start_tvusec,
        "executable_path": normalizedPath(String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)),
        "parent_executable_path": pathForPID(Int32(stamp.pbi_ppid)) as Any? ?? NSNull()
    ]
}

private func pathForPID(_ pid: Int32) -> String? {
    guard pid > 0 else { return nil }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return normalizedPath(String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
}

private func matches(_ actual: [String: Any]?, _ expected: UnicodeKernelIdentity) -> Bool {
    guard let actual else { return false }
    return (actual["pid"] as? NSNumber)?.int32Value == expected.pid &&
        (actual["ppid"] as? NSNumber)?.int32Value == expected.parent_pid &&
        (actual["start_seconds"] as? NSNumber)?.int64Value == expected.start_seconds &&
        (actual["start_microseconds"] as? NSNumber)?.int64Value == expected.start_microseconds &&
        actual["executable_path"] as? String == expected.executable_path &&
        (actual["parent_executable_path"] as? String) == expected.parent_executable_path
}

private func identityRecord(_ identity: UnicodeKernelIdentity) -> [String: Any] {
    [
        "pid": identity.pid,
        "parent_pid": identity.parent_pid,
        "start_seconds": identity.start_seconds,
        "start_microseconds": identity.start_microseconds,
        "executable_path": identity.executable_path,
        "parent_executable_path": identity.parent_executable_path as Any? ?? NSNull()
    ]
}

private func jsonValue(_ value: Any) -> Any {
    if value is NSNull { return NSNull() }
    if let string = value as? String { return string }
    if let number = value as? NSNumber { return number }
    if let array = value as? [Any] { return array.map(jsonValue) }
    if let dictionary = value as? [String: Any] { return dictionary.mapValues(jsonValue) }
    if let dictionary = value as? NSDictionary {
        var result: [String: Any] = [:]
        for (key, child) in dictionary {
            guard let name = key as? String else { continue }
            result[name] = jsonValue(child)
        }
        return result
    }
    return String(describing: value)
}

private func cgWindows(pid: Int32) -> [[String: Any]]? {
    guard let values = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
    return values.filter {
        ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid &&
            ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0
    }.map { jsonValue($0) as? [String: Any] ?? [:] }
}

private func windowID(_ window: [String: Any]) -> UInt32? {
    (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value
}

private func windowBounds(_ window: [String: Any]) -> CGRect? {
    guard let raw = window[kCGWindowBounds as String] as? NSDictionary else { return nil }
    var rect = CGRect.zero
    return CGRectMakeWithDictionaryRepresentation(raw, &rect) ? rect : nil
}

private func sameFrame(_ first: CGRect?, _ second: CGRect?) -> Bool {
    guard let first, let second else { return false }
    return abs(first.origin.x - second.origin.x) <= 0.75 &&
        abs(first.origin.y - second.origin.y) <= 0.75 &&
        abs(first.width - second.width) <= 0.75 &&
        abs(first.height - second.height) <= 0.75
}

@MainActor
private final class UnicodeAXReader {
    let pid: Int32
    let deadline: UInt64
    private var references: [AXUIElement] = []

    init(pid: Int32, budgetMs: UInt64 = 7000) {
        self.pid = pid
        self.deadline = uptimeMs() + budgetMs
    }

    func owner(_ item: AXUIElement) -> Int32? {
        var result: pid_t = 0
        return AXUIElementGetPid(item, &result) == .success ? result : nil
    }

    private func ready(_ item: AXUIElement) -> Bool {
        let now = uptimeMs()
        guard now < deadline else { return false }
        return AXUIElementSetMessagingTimeout(item, Float(min(100, deadline - now)) / 1000) == .success
    }

    func identity(_ item: AXUIElement) -> String? {
        guard owner(item) == pid else { return nil }
        if let index = references.firstIndex(where: { CFEqual($0, item) }) { return "ax-local-\(index + 1)" }
        guard references.count < 160 else { return nil }
        references.append(item)
        return "ax-local-\(references.count)"
    }

    func value(_ item: AXUIElement, _ attribute: String) -> (CFTypeRef?, AXError) {
        guard owner(item) == pid, ready(item) else { return (nil, .cannotComplete) }
        var result: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(item, attribute as CFString, &result)
        return (result, error)
    }

    func relatedElement(_ item: AXUIElement, _ attribute: String) -> (AXUIElement?, AXError) {
        let (raw, error) = value(item, attribute)
        guard error == .success, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return (nil, error) }
        let target = raw as! AXUIElement
        return owner(target) == pid ? (target, error) : (nil, .invalidUIElement)
    }

    func elements(_ item: AXUIElement, _ attribute: String) -> ([AXUIElement]?, AXError) {
        let (raw, error) = value(item, attribute)
        guard error == .success, let raw, CFGetTypeID(raw) == CFArrayGetTypeID() else { return (nil, error) }
        return ((raw as! [Any]).compactMap { candidate in
            guard CFGetTypeID(candidate as CFTypeRef) == AXUIElementGetTypeID() else { return nil }
            let element = candidate as! AXUIElement
            guard owner(element) == pid else { return nil }
            return element
        }, error)
    }

    func frame(_ item: AXUIElement) -> CGRect? {
        let (position, positionError) = value(item, kAXPositionAttribute)
        let (size, sizeError) = value(item, kAXSizeAttribute)
        guard positionError == .success, sizeError == .success,
              let position, CFGetTypeID(position) == AXValueGetTypeID(),
              let size, CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        var dimensions = CGSize.zero
        guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
              AXValueGetValue(size as! AXValue, .cgSize, &dimensions) else { return nil }
        return CGRect(origin: point, size: dimensions)
    }

    func string(_ item: AXUIElement, _ attribute: String) -> (String?, AXError) {
        let (raw, error) = value(item, attribute)
        guard error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID() else { return (nil, error) }
        return (raw as? String, error)
    }

    private func boundedStringProperty(_ item: AXUIElement, _ attribute: String, maxUTF8Bytes: Int) -> [String: Any] {
        let (raw, error) = value(item, attribute)
        var result: [String: Any] = [
            "error": error.rawValue,
            "present": raw != nil,
            "max_utf8_bytes": maxUTF8Bytes
        ]
        guard error == .success, let raw else { return result }
        result["cf_type_id"] = CFGetTypeID(raw)
        guard CFGetTypeID(raw) == CFStringGetTypeID(), let text = raw as? String else {
            result["is_cf_string"] = false
            result["value"] = NSNull()
            return result
        }
        let byteCount = Data(text.utf8).count
        result["is_cf_string"] = true
        result["utf8_bytes"] = byteCount
        result["within_utf8_budget"] = byteCount <= maxUTF8Bytes
        result["value"] = byteCount <= maxUTF8Bytes ? text as Any : NSNull()
        if byteCount > maxUTF8Bytes { result["content_omitted"] = true }
        return result
    }

    private func valueSettable(_ item: AXUIElement, _ attribute: String) -> [String: Any] {
        guard owner(item) == pid, ready(item) else {
            return ["error": AXError.cannotComplete.rawValue, "is_settable": NSNull(), "query_skipped": true]
        }
        var settable = DarwinBoolean(false)
        let error = AXUIElementIsAttributeSettable(item, attribute as CFString, &settable)
        return [
            "error": error.rawValue,
            "is_settable": error == .success ? settable.boolValue as Any : NSNull(),
            "query_skipped": false
        ]
    }

    private func strictBooleanAttribute(_ item: AXUIElement, _ attribute: String) -> [String: Any] {
        let (raw, error) = value(item, attribute)
        var result: [String: Any] = ["error": error.rawValue, "present": raw != nil]
        guard error == .success, let raw else { return result }
        result["cf_type_id"] = CFGetTypeID(raw)
        if CFGetTypeID(raw) == CFBooleanGetTypeID() {
            result["strict_cf_boolean"] = CFBooleanGetValue(unsafeDowncast(raw, to: CFBoolean.self))
            result["strict_cf_boolean_available"] = true
        } else {
            result["strict_cf_boolean"] = NSNull()
            result["strict_cf_boolean_available"] = false
        }
        return result
    }

    private func advertisedAttributeNames(_ item: AXUIElement) -> [String: Any] {
        guard owner(item) == pid, ready(item) else {
            return ["error": AXError.cannotComplete.rawValue, "query_skipped": true,
                    "names": [], "name_limit": 64, "per_name_max_utf8_bytes": 512,
                    "attribute_values_read": false]
        }
        var raw: CFArray?
        let error = AXUIElementCopyAttributeNames(item, &raw)
        var result: [String: Any] = [
            "error": error.rawValue,
            "query_skipped": false,
            "names": [],
            "name_limit": 64,
            "per_name_max_utf8_bytes": 512,
            "attribute_values_read": false
        ]
        guard error == .success, let raw, CFGetTypeID(raw) == CFArrayGetTypeID() else {
            if let raw { result["cf_type_id"] = CFGetTypeID(raw) }
            return result
        }
        let values = raw as! [Any]
        var names: [String] = []
        var oversizedNameCount = 0
        var nonStringNameCount = 0
        for candidate in values.prefix(64) {
            guard CFGetTypeID(candidate as CFTypeRef) == CFStringGetTypeID(),
                  let name = candidate as? String else {
                nonStringNameCount += 1
                continue
            }
            guard name.utf8.count <= 512 else {
                oversizedNameCount += 1
                continue
            }
            names.append(name)
        }
        result["advertised_name_count"] = values.count
        result["names"] = names
        result["names_truncated"] = values.count > 64
        result["oversized_name_count_within_limit"] = oversizedNameCount
        result["non_string_name_count_within_limit"] = nonStringNameCount
        return result
    }

    func exactReceiverReadonlyAPIFacts(_ item: AXUIElement, queryAllowed: Bool) -> [String: Any] {
        guard queryAllowed else {
            let skipped: [String: Any] = ["query_skipped": "receiver was not verified for the exact owned document and current system focus"]
            return ["attribute_names": skipped, "ax_enabled": skipped, "ax_focused": skipped,
                    "input_or_mutation_calls": 0]
        }
        return [
            "attribute_names": advertisedAttributeNames(item),
            "ax_enabled": strictBooleanAttribute(item, kAXEnabledAttribute as String),
            "ax_focused": strictBooleanAttribute(item, kAXFocusedAttribute as String),
            "input_or_mutation_calls": 0
        ]
    }

    func textTargetCapabilities(_ item: AXUIElement, queryAllowed: Bool) -> [String: Any] {
        let maxLabelUTF8Bytes = 512
        guard queryAllowed else {
            let skipped = ["query_skipped": "target was not verified as the exact current receiver in the unique temporary-document window"] as [String: Any]
            return [
                "ax_title": skipped,
                "ax_description": skipped,
                "ax_value_settable": skipped,
                "max_label_utf8_bytes": maxLabelUTF8Bytes,
                "control_label_semantics": "Unknown",
                "window_title_used_as_control_label": false
            ]
        }
        return [
            "ax_title": boundedStringProperty(item, kAXTitleAttribute as String, maxUTF8Bytes: maxLabelUTF8Bytes),
            "ax_description": boundedStringProperty(item, kAXDescriptionAttribute as String, maxUTF8Bytes: maxLabelUTF8Bytes),
            "ax_value_settable": valueSettable(item, kAXValueAttribute as String),
            "max_label_utf8_bytes": maxLabelUTF8Bytes,
            "control_label_semantics": "Unknown; AX attributes are reported without assigning a semantic target name",
            "window_title_used_as_control_label": false
        ]
    }

    func describe(_ item: AXUIElement) -> [String: Any] {
        guard let id = identity(item) else { return ["available": false] }
        let (role, roleError) = string(item, kAXRoleAttribute as String)
        let (subrole, subroleError) = string(item, kAXSubroleAttribute as String)
        let (title, titleError) = string(item, kAXTitleAttribute as String)
        let (focusedRaw, focusedError) = value(item, kAXFocusedAttribute)
        var focusedValue: Any = NSNull()
        if let focusedRaw, CFGetTypeID(focusedRaw) == CFBooleanGetTypeID() {
            focusedValue = CFBooleanGetValue((focusedRaw as! CFBoolean))
        }
        return [
            "diagnostic_id": id,
            "pid": pid,
            "role": ["error": roleError.rawValue, "value": role as Any? ?? NSNull()],
            "subrole": ["error": subroleError.rawValue, "value": subrole as Any? ?? NSNull()],
            "title": ["error": titleError.rawValue, "value": title as Any? ?? NSNull()],
            "focused": ["error": focusedError.rawValue, "strict_cf_boolean": focusedValue],
            "frame": frame(item).map { ["x": $0.origin.x, "y": $0.origin.y, "width": $0.width, "height": $0.height] } as Any? ?? NSNull()
        ]
    }

    func parentChain(_ item: AXUIElement) -> [[String: Any]] {
        var cursor = item
        var visited: [AXUIElement] = []
        var result: [[String: Any]] = []
        for _ in 0..<16 {
            guard uptimeMs() < deadline, owner(cursor) == pid,
                  !visited.contains(where: { CFEqual($0, cursor) }) else { break }
            visited.append(cursor)
            let (parent, parentError) = relatedElement(cursor, kAXParentAttribute as String)
            var record: [String: Any] = ["element": describe(cursor), "parent_error": parentError.rawValue]
            if let parent {
                record["parent_diagnostic_id"] = identity(parent) as Any? ?? NSNull()
                let (children, childrenError) = elements(parent, kAXChildrenAttribute as String)
                record["parent_children_error"] = childrenError.rawValue
                record["actual_cf_membership_count"] = (children ?? []).filter { CFEqual($0, cursor) }.count
            }
            result.append(record)
            guard let parent else { break }
            cursor = parent
        }
        return result
    }

    func tree(_ application: AXUIElement) -> [AXUIElement] {
        var queue: [(AXUIElement, Int)] = [(application, 0)]
        var result: [AXUIElement] = []
        while !queue.isEmpty, result.count < 96, uptimeMs() < deadline {
            let (current, depth) = queue.removeFirst()
            guard owner(current) == pid, !result.contains(where: { CFEqual($0, current) }) else { continue }
            result.append(current)
            guard depth < 6 else { continue }
            for attribute in [kAXWindowsAttribute as String, kAXChildrenAttribute as String] {
                if let children = elements(current, attribute).0 {
                    queue.append(contentsOf: children.prefix(24).map { ($0, depth + 1) })
                }
            }
        }
        return result
    }

    func numeric(_ item: AXUIElement, _ attribute: String) -> (Int?, AXError) {
        let (raw, error) = value(item, attribute)
        guard error == .success, let raw, CFGetTypeID(raw) == CFNumberGetTypeID() else { return (nil, error) }
        return ((raw as! NSNumber).intValue, error)
    }

    func range(_ item: AXUIElement, _ attribute: String) -> [String: Any] {
        let (raw, error) = value(item, attribute)
        var result: [String: Any] = ["error": error.rawValue, "present": raw != nil]
        guard error == .success, let raw else { return result }
        result["cf_type_id"] = CFGetTypeID(raw)
        result["cf_type_name"] = CFCopyTypeIDDescription(CFGetTypeID(raw)) as Any? ?? NSNull()
        if CFGetTypeID(raw) == AXValueGetTypeID() {
            let axValue = raw as! AXValue
            result["ax_value_type"] = AXValueGetType(axValue).rawValue
            if AXValueGetType(axValue) == .cfRange {
                var value = CFRange(location: 0, length: 0)
                if AXValueGetValue(axValue, .cfRange, &value) {
                    result["range"] = ["location": value.location, "length": value.length]
                }
            }
        } else if CFGetTypeID(raw) == CFBooleanGetTypeID() {
            result["strict_cf_boolean"] = CFBooleanGetValue((raw as! CFBoolean))
        }
        // Marker objects are deliberately reported by type only; no opaque marker data is read.
        return result
    }

    func markedState(_ item: AXUIElement, queryAllowed: Bool) -> [String: Any] {
        let attributes = ["AXMarkedTextRange", "AXMarkedRange", "AXHasMarkedText", "AXTextInputMarkedTextMarkerRange"]
        var marked: [String: Any] = [:]
        if queryAllowed {
            for attribute in attributes { marked[attribute] = range(item, attribute) }
            marked["AXSelectedTextRange"] = range(item, kAXSelectedTextRangeAttribute as String)
        } else {
            for attribute in attributes + [kAXSelectedTextRangeAttribute as String] {
                marked[attribute] = ["query_skipped": "receiver was not verified as the current system-focused element"]
            }
        }
        marked["composition_conclusion"] = "Unknown: selection range and TIS source metadata do not establish marked-text state"
        return marked
    }

    func publicTextInputMarkedRange(_ item: AXUIElement, queryAllowed: Bool) -> [String: Any] {
        guard queryAllowed else {
            return [
                "attribute_symbol": "NSAccessibilityTextInputMarkedRangeAttribute",
                "query_skipped": "receiver was not verified as the current system-focused element in the unique temporary-document window"
            ]
        }
        let attribute = NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue
        let (raw, error) = value(item, attribute)
        var result: [String: Any] = [
            "attribute_symbol": "NSAccessibilityTextInputMarkedRangeAttribute",
            "attribute_name": attribute,
            "error": error.rawValue,
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
        var rangeValue = CFRange(location: 0, length: 0)
        let converted = AXValueGetValue(axValue, .cfRange, &rangeValue)
        result["range_conversion_succeeded"] = converted
        if converted {
            result["location_decimal_string"] = String(rangeValue.location)
            result["length_decimal_string"] = String(rangeValue.length)
        }
        return result
    }

    func readback(_ item: AXUIElement, expected: String, queryAllowed: Bool) -> [String: Any] {
        let maxCharacters = 4096
        let maxUtf8Bytes = 16 * 1024
        var record: [String: Any] = [
            "number_of_characters": NSNull(),
            "number_of_characters_error": NSNull(),
            "max_characters_before_value_read": maxCharacters,
            "max_utf8_bytes_reported": maxUtf8Bytes,
            "value_read_performed": false,
            "status": "Unknown"
        ]
        guard queryAllowed else {
            record["status"] = "read_skipped_until_exact_current_receiver_and_temp_window_are_verified"
            return record
        }
        let (count, countError) = numeric(item, "AXNumberOfCharacters")
        record["number_of_characters"] = count as Any? ?? NSNull()
        record["number_of_characters_error"] = countError.rawValue
        guard countError == .success, let count, count >= 0, count <= maxCharacters else {
            if let count, count > maxCharacters { record["status"] = "bounded_read_skipped" }
            return record
        }
        let (raw, error) = value(item, kAXValueAttribute as String)
        record["value_error"] = error.rawValue
        guard error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID(), let text = raw as? String else { return record }
        record["value_read_performed"] = true
        let bytes = Data(text.utf8)
        record["utf8_byte_length"] = bytes.count
        record["unicode_scalar_count"] = text.unicodeScalars.count
        record["sha256"] = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        record["fits_utf8_budget"] = bytes.count <= maxUtf8Bytes
        record["equals_expected_fixture_text"] = bytes.count <= maxUtf8Bytes ? text == expected : NSNull() as Any
        if bytes.count <= maxUtf8Bytes { record["text"] = text }
        record["status"] = bytes.count <= maxUtf8Bytes ? "complete_within_budget" : "value_exceeded_utf8_budget_content_omitted"
        return record
    }
}

private func tisDescription(_ unmanaged: Unmanaged<TISInputSource>?) -> [String: Any] {
    guard let source = unmanaged?.takeRetainedValue() else { return ["available": false] }
    func property(_ name: CFString) -> CFTypeRef? {
        guard let pointer = TISGetInputSourceProperty(source, name) else { return nil }
        return Unmanaged<CFTypeRef>.fromOpaque(pointer).takeUnretainedValue()
    }
    var result: [String: Any] = ["available": true]
    for (label, key) in [
        ("source_id", kTISPropertyInputSourceID),
        ("source_type", kTISPropertyInputSourceType),
        ("source_category", kTISPropertyInputSourceCategory),
        ("input_mode_id", kTISPropertyInputModeID)
    ] {
        if let key, let value = property(key) as? String { result[label] = value }
        else { result[label] = NSNull() }
    }
    if let raw = property(kTISPropertyInputSourceIsASCIICapable), CFGetTypeID(raw) == CFBooleanGetTypeID() {
        result["ascii_capable"] = CFBooleanGetValue((raw as! CFBoolean))
    } else { result["ascii_capable"] = NSNull() }
    if let raw = property(kTISPropertyUnicodeKeyLayoutData), CFGetTypeID(raw) == CFDataGetTypeID() {
        let data = raw as! CFData
        let count = CFDataGetLength(data)
        result["unicode_layout_byte_length"] = count
        if count <= 64 * 1024 {
            result["unicode_layout_sha256"] = SHA256.hash(data: data as Data).map { String(format: "%02x", $0) }.joined()
        } else {
            result["unicode_layout_sha256"] = NSNull()
            result["unicode_layout_hash_status"] = "skipped_over_64_KiB_budget"
        }
    } else {
        result["unicode_layout_byte_length"] = NSNull()
        result["unicode_layout_sha256"] = NSNull()
    }
    return result
}

private func tisSnapshot() -> [String: Any] {
    [
        "selected_input_source": tisDescription(TISCopyCurrentKeyboardInputSource()),
        "keyboard_layout_source": tisDescription(TISCopyCurrentKeyboardLayoutInputSource()),
        "keyboard_type": UInt32(LMGetKbdType()),
        "keyboard_type_api": "LMGetKbdType"
    ]
}

@MainActor
private func systemFocusedApplicationPID() -> (Int32?, Int32) {
    let system = AXUIElementCreateSystemWide()
    _ = AXUIElementSetMessagingTimeout(system, 0.1)
    var raw: CFTypeRef?
    let error = AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &raw)
    guard error == .success, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return (nil, error.rawValue) }
    var pid: pid_t = 0
    let ownerError = AXUIElementGetPid(raw as! AXUIElement, &pid)
    return ownerError == .success ? (pid, error.rawValue) : (nil, ownerError.rawValue)
}

@MainActor
private func tempWindowReadiness(_ request: UnicodeProbeRequest) -> [String: Any] {
    let started = uptimeMs()
    let deadline = started + 3000
    let receipt = request.launch_receipt
    let expectedTitle = URL(fileURLWithPath: request.document_path).lastPathComponent
    var lastCGWindows: [[String: Any]] = []
    var lastCGCandidates: [[String: Any]] = []
    var lastAXWindowCount: Int?
    var lastAXError: Int32 = AXError.cannotComplete.rawValue
    var lastCGTitle: String?
    var lastAXTitle: String?
    var lastFramesMatch = false
    var focusedPID: Int32?
    var focusedError: Int32?
    var verified = false
    var identityRevalidated = false

    while uptimeMs() < deadline {
        guard let identity = processIdentity(request.target_pid), matches(identity, receipt.target_identity),
              !receipt.preexisting_textedit_pids.contains(request.target_pid) else { break }
        lastCGWindows = cgWindows(pid: request.target_pid) ?? []
        lastCGCandidates = lastCGWindows.filter { windowID($0) != nil && windowBounds($0) != nil }
        let cgWindow = lastCGCandidates.count == 1 ? lastCGCandidates[0] : nil
        lastCGTitle = cgWindow?[kCGWindowName as String] as? String

        let remaining = deadline > uptimeMs() ? deadline - uptimeMs() : 1
        let reader = UnicodeAXReader(pid: request.target_pid, budgetMs: remaining)
        let app = AXUIElementCreateApplication(request.target_pid)
        let (windows, axError) = reader.elements(app, kAXWindowsAttribute as String)
        lastAXError = axError.rawValue
        lastAXWindowCount = windows?.count
        let matching = (windows ?? []).filter { sameFrame(reader.frame($0), cgWindow.flatMap(windowBounds)) }
        let axWindow = matching.count == 1 ? matching[0] : nil
        lastAXTitle = axWindow.flatMap { reader.string($0, kAXTitleAttribute as String).0 }
        lastFramesMatch = cgWindow != nil && axWindow != nil && matching.count == 1
        let titlesMatch = (lastCGTitle?.contains(expectedTitle) ?? false) &&
            (lastAXTitle?.contains(expectedTitle) ?? false)
        let currentIdentity = processIdentity(request.target_pid)
        identityRevalidated = currentIdentity.map { matches($0, receipt.target_identity) } ?? false
        if identityRevalidated && lastCGCandidates.count == 1 && windows?.count == 1 &&
            lastFramesMatch && titlesMatch && uptimeMs() < deadline {
            let focused = systemFocusedApplicationPID()
            focusedPID = focused.0
            focusedError = focused.1
            verified = true
            break
        }
        Thread.sleep(forTimeInterval: 0.05)
    }

    return [
        "kind": "unicode_temp_window_readiness",
        "run_id": request.run_id,
        "target_pid": request.target_pid,
        "window_wait_budget_ms": 3000,
        "window_wait_elapsed_ms": uptimeMs() - started,
        "window_verified": verified,
        "process_identity_revalidated": identityRevalidated,
        "target_pid_was_preexisting": receipt.preexisting_textedit_pids.contains(request.target_pid),
        "cg_window_count": lastCGCandidates.count,
        "ax_window_count": lastAXWindowCount as Any? ?? NSNull(),
        "cg_window_id": lastCGCandidates.count == 1 ? windowID(lastCGCandidates[0]) as Any? ?? NSNull() : NSNull(),
        "expected_document_title": expectedTitle,
        "cg_title": lastCGTitle as Any? ?? NSNull(),
        "ax_title": lastAXTitle as Any? ?? NSNull(),
        "ax_windows_error": lastAXError,
        "frames_match": lastFramesMatch,
        "cg_windows": lastCGWindows,
        "system_focused_application_pid_at_window_check": focusedPID as Any? ?? NSNull(),
        "system_focused_application_error_at_window_check": focusedError as Any? ?? NSNull(),
        "input_authorized": false,
        "input_posted": false,
        "mvp_complete": false
    ]
}

@main
struct UnicodeReceiverProbeMain {
    @MainActor static func main() {
        var runID = "unknown"
        var observedTargetPID: Int32?
        do {
            let input = FileHandle.standardInput.readDataToEndOfFile()
            guard input.count <= 32 * 1024 else { throw NSError(domain: "unicode probe request budget", code: 1) }
            let request = try JSONDecoder().decode(UnicodeProbeRequest.self, from: input)
            runID = request.run_id
            observedTargetPID = request.target_pid
            if CommandLine.arguments.contains("--decode-only") {
                let receipt = request.launch_receipt
                let result: [String: Any] = [
                    "kind": "unicode_probe_protocol_check",
                    "decoded": true,
                    "run_id": request.run_id,
                    "document_sha256": request.document_sha256,
                    "target_pid": request.target_pid,
                    "probe_parent_pid": request.probe_parent_pid,
                    "launcher_id": request.launcher_id,
                    "receipt_run_id": receipt.run_id,
                    "receipt_launcher_id": receipt.launcher_id,
                    "receipt_document_sha256": receipt.document_sha256,
                    "activation_attempted": request.activation_observation?.attempted as Any? ?? NSNull(),
                    "activation_attempt_count": request.activation_observation?.attempt_count as Any? ?? NSNull(),
                    "activation_return_value": request.activation_observation?.return_value as Any? ?? NSNull(),
                    "preexisting_textedit_pids": receipt.preexisting_textedit_pids,
                    "launcher_pid": receipt.launcher_identity.pid,
                    "launcher_parent_pid": receipt.launcher_identity.parent_pid,
                    "target_start_seconds": receipt.target_identity.start_seconds,
                    "target_start_microseconds": receipt.target_identity.start_microseconds,
                    "target_parent_pid": receipt.target_identity.parent_pid,
                    "target_executable_path": receipt.target_identity.executable_path,
                    "creates_new_application_instance": receipt.creates_new_application_instance,
                    "adds_to_recent_items": receipt.adds_to_recent_items,
                    "no_input_authority": receipt.no_input_authority,
                    "input_authorized": false,
                    "mvp_complete": false
                ]
                let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
                FileHandle.standardOutput.write(data + Data([10]))
                return
            }
            let expectedPath = normalizedPath(request.executable_path)
            let expectedDocument = normalizedPath(request.document_path)
            let receipt = request.launch_receipt
            let identityBefore = request.target_pid > 0 ? processIdentity(request.target_pid) : nil
            let launcherIdentityBefore = request.launcher_pid > 0 ? processIdentity(request.launcher_pid) : nil
            let targetStartMilliseconds = receipt.target_identity.start_seconds * 1000 + receipt.target_identity.start_microseconds / 1000
            guard request.target_pid > 0, request.probe_parent_pid == getppid(),
                  expectedPath == "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
                  request.bundle_id == "com.apple.TextEdit",
                  URL(fileURLWithPath: expectedDocument).lastPathComponent == "owned-unicode-fixture.txt",
                  FileManager.default.fileExists(atPath: expectedDocument),
                  Data(request.expected_text.utf8).count <= 4096,
                  request.launcher_id == receipt.launcher_id,
                  receipt.version == 1, receipt.run_id == request.run_id,
                  receipt.launcher_identity.pid == request.launcher_pid,
                  receipt.launcher_identity.parent_pid == request.probe_parent_pid,
                  matches(launcherIdentityBefore, receipt.launcher_identity),
                  receipt.target_identity.pid == request.target_pid,
                  receipt.bundle_identifier == request.bundle_id,
                  normalizedPath(receipt.application_path) == "/System/Applications/TextEdit.app",
                  normalizedPath(receipt.executable_path) == expectedPath,
                  normalizedPath(receipt.document_path) == expectedDocument,
                  receipt.document_sha256 == request.document_sha256,
                  !receipt.preexisting_textedit_pids.contains(request.target_pid),
                  receipt.new_pid_excluded_from_preexisting_set,
                  receipt.target_started_within_launch_interval,
                  receipt.launch_start_unix_milliseconds <= targetStartMilliseconds + 1000,
                  targetStartMilliseconds <= receipt.launch_finish_unix_milliseconds + 10_000,
                  receipt.creates_new_application_instance, !receipt.adds_to_recent_items,
                  !receipt.activates_new_instance,
                  receipt.arguments == ["-ApplePersistenceIgnoreState", "YES"],
                  receipt.target_parent_is_launchd == (receipt.target_identity.parent_pid == 1),
                  receipt.no_input_authority,
                  let identityBefore,
                  matches(identityBefore, receipt.target_identity),
                  identityBefore["executable_path"] as? String == expectedPath,
                  let runningApplication = NSRunningApplication(processIdentifier: request.target_pid),
                  !runningApplication.isTerminated,
                  runningApplication.bundleIdentifier == request.bundle_id
            else { throw NSError(domain: "owned TextEdit process identity unavailable", code: 2) }

            if CommandLine.arguments.contains("--window-ready-only") {
                let readiness = tempWindowReadiness(request)
                let data = try JSONSerialization.data(withJSONObject: jsonValue(readiness), options: [.sortedKeys])
                FileHandle.standardOutput.write(data + Data([10]))
                return
            }

            let trust = AXIsProcessTrusted()
            var cgBefore = cgWindows(pid: request.target_pid)
            var candidates = (cgBefore ?? []).filter { windowID($0) != nil && windowBounds($0) != nil }
            let windowDeadline = uptimeMs() + 5000
            while candidates.count != 1 && uptimeMs() < windowDeadline {
                Thread.sleep(forTimeInterval: 0.1)
                cgBefore = cgWindows(pid: request.target_pid)
                candidates = (cgBefore ?? []).filter { windowID($0) != nil && windowBounds($0) != nil }
            }
            let uniqueCGWindow = candidates.count == 1 ? candidates[0] : nil
            let application = AXUIElementCreateApplication(request.target_pid)
            let reader = UnicodeAXReader(pid: request.target_pid)
            let (axWindows, axWindowsError) = reader.elements(application, kAXWindowsAttribute as String)
            let matchingAXWindows = (axWindows ?? []).filter { sameFrame(reader.frame($0), uniqueCGWindow.flatMap(windowBounds)) }
            let selectedWindow = matchingAXWindows.count == 1 ? matchingAXWindows[0] : nil
            let expectedTitle = URL(fileURLWithPath: expectedDocument).lastPathComponent
            let cgTitle = uniqueCGWindow?[kCGWindowName as String] as? String
            let axTitle = selectedWindow.flatMap { reader.string($0, kAXTitleAttribute as String).0 }
            let oneDocumentWindow = axWindows?.count == 1 &&
                (cgTitle?.contains(expectedTitle) ?? false) && (axTitle?.contains(expectedTitle) ?? false)
            let (focusedWindow, focusedWindowError) = reader.relatedElement(application, kAXFocusedWindowAttribute as String)
            let (receiver, receiverError) = reader.relatedElement(application, kAXFocusedUIElementAttribute as String)
            let (frontmostPID, frontmostError) = systemFocusedApplicationPID()
            let tree = reader.tree(application)
            let textAreas = tree.filter { reader.string($0, kAXRoleAttribute as String).0 == kAXTextAreaRole as String }
            let textFields = tree.filter { reader.string($0, kAXRoleAttribute as String).0 == kAXTextFieldRole as String }
            let receiverInTreeCount = receiver.map { focus in tree.filter { CFEqual($0, focus) }.count } ?? 0
            let receiverBelongsToCurrentWindow = receiver.flatMap { focus in
                reader.relatedElement(focus, kAXWindowAttribute as String).0.map { window in selectedWindow.map { CFEqual($0, window) } }
            } ?? nil
            let receiverIsCurrent = receiver != nil && frontmostPID == request.target_pid && receiverInTreeCount == 1 && receiverBelongsToCurrentWindow == true
            let currentReceiverForTempDocument = receiverIsCurrent && oneDocumentWindow
            let sharedFocusSources = ([receiver].compactMap { $0 } + textFields).prefix(12)
            let sharedFocusRelations: [[String: Any]] = sharedFocusSources.map { source in
                let (elements, error) = reader.elements(source, "AXSharedFocusElements")
                return [
                    "source": reader.describe(source),
                    "attribute_error": error.rawValue,
                    "elements": (elements ?? []).prefix(16).map { candidate in
                        [
                            "element": reader.describe(candidate),
                            "same_as_receiver_cf": receiver.map { CFEqual($0, candidate) } as Any? ?? NSNull(),
                            "same_as_any_text_area_cf": textAreas.contains { CFEqual($0, candidate) }
                        ]
                    }
                ]
            }

            let tisBefore = tisSnapshot()
            let receiverDetails: [String: Any]? = receiver.map { item in
                [
                    "element": reader.describe(item),
                    "parent_chain": reader.parentChain(item),
                    "public_text_input_marked_range": reader.publicTextInputMarkedRange(item, queryAllowed: currentReceiverForTempDocument),
                    "readonly_api_facts": reader.exactReceiverReadonlyAPIFacts(item, queryAllowed: currentReceiverForTempDocument),
                    "text_target_accessibility_capabilities": reader.textTargetCapabilities(item, queryAllowed: currentReceiverForTempDocument),
                    "marked_properties_and_selection": reader.markedState(item, queryAllowed: currentReceiverForTempDocument),
                    "unicode_readback": reader.readback(item, expected: request.expected_text, queryAllowed: currentReceiverForTempDocument),
                    "same_cf_as_text_areas": textAreas.enumerated().map { index, area in
                        ["text_area_index": index, "diagnostic_id": reader.identity(area) as Any? ?? NSNull(), "same_cf": CFEqual(item, area)]
                    }
                ]
            }
            let textAreaDetails = textAreas.prefix(8).enumerated().map { index, area in
                [
                    "index": index,
                    "element": reader.describe(area),
                    "same_cf_as_receiver": receiver.map { CFEqual($0, area) } as Any? ?? NSNull(),
                    "text_target_accessibility_capabilities": reader.textTargetCapabilities(area, queryAllowed: currentReceiverForTempDocument && (receiver.map { CFEqual($0, area) } ?? false)),
                    "marked_properties_and_selection": reader.markedState(area, queryAllowed: currentReceiverForTempDocument && (receiver.map { CFEqual($0, area) } ?? false)),
                    "unicode_readback": reader.readback(area, expected: request.expected_text, queryAllowed: currentReceiverForTempDocument && (receiver.map { CFEqual($0, area) } ?? false))
                ] as [String: Any]
            }
            let textFieldDetails = textFields.prefix(8).enumerated().map { index, field in
                [
                    "index": index,
                    "element": reader.describe(field),
                    "same_cf_as_receiver": receiver.map { CFEqual($0, field) } as Any? ?? NSNull(),
                    "marked_properties_and_selection": reader.markedState(field, queryAllowed: currentReceiverForTempDocument && (receiver.map { CFEqual($0, field) } ?? false))
                ] as [String: Any]
            }
            let receiverRole = receiver.flatMap { reader.string($0, kAXRoleAttribute as String).0 }
            let fieldEditorReport: [String: Any] = [
                "status": "Unknown",
                "reason": "Public cross-process AX/TIS reads do not expose NSTextInputClient.hasMarkedText or identify an AppKit field editor as a field-editor instance.",
                "shared_focus_elements_are_reported_as_cf_relationships_only": true,
                "relations": sharedFocusRelations
            ]

            let tisAfter = tisSnapshot()
            let (frontmostPIDAfter, frontmostErrorAfter) = systemFocusedApplicationPID()
            let (receiverAfter, receiverAfterError) = reader.relatedElement(application, kAXFocusedUIElementAttribute as String)
            let (focusedWindowAfter, focusedWindowAfterError) = reader.relatedElement(application, kAXFocusedWindowAttribute as String)
            let cgAfter = cgWindows(pid: request.target_pid)
            let identityAfter = processIdentity(request.target_pid)
            let launcherIdentityAfter = processIdentity(request.launcher_pid)
            let windowUnchanged = uniqueCGWindow != nil && candidates.count == 1 &&
                cgAfter?.count == 1 && NSDictionary(dictionary: uniqueCGWindow!).isEqual(to: cgAfter?.first ?? [:])
            let processUnchanged = identityAfter.map { NSDictionary(dictionary: identityBefore).isEqual(to: $0) } ?? false
            let launcherUnchanged = matches(launcherIdentityAfter, receipt.launcher_identity)
            let trustAfter = AXIsProcessTrusted()
            let receiverUnchanged = receiver.flatMap { original in receiverAfter.map { CFEqual(original, $0) } } ?? false
            let axWindowUnchanged = selectedWindow.flatMap { original in
                focusedWindowAfter.map { CFEqual(original, $0) }
            } ?? false
            let result: [String: Any] = [
                "kind": "unicode_receiver_readonly_probe",
                "run_id": request.run_id,
                "started_uptime_ms": uptimeMs(),
                "target": [
                    "pid": request.target_pid,
                    "bundle_id": request.bundle_id,
                    "executable_path": expectedPath,
                    "document_path": expectedDocument,
                    "document_argument_is_this_run_temp_file": true
                ],
                "launcher_receipt": [
                    "version": receipt.version,
                    "run_id": receipt.run_id,
                    "launcher_id": receipt.launcher_id,
                    "launcher_identity": identityRecord(receipt.launcher_identity),
                    "target_identity": identityRecord(receipt.target_identity),
                    "preexisting_textedit_pids": receipt.preexisting_textedit_pids,
                    "launch_start_unix_milliseconds": receipt.launch_start_unix_milliseconds,
                    "launch_finish_unix_milliseconds": receipt.launch_finish_unix_milliseconds,
                    "new_pid_excluded_from_preexisting_set": receipt.new_pid_excluded_from_preexisting_set,
                    "target_started_within_launch_interval": receipt.target_started_within_launch_interval,
                    "creates_new_application_instance": receipt.creates_new_application_instance,
                    "adds_to_recent_items": receipt.adds_to_recent_items,
                    "activates_new_instance": receipt.activates_new_instance,
                    "arguments": receipt.arguments,
                    "no_input_authority": receipt.no_input_authority
                ],
                "launcher_receipt_verified": true,
                "launcher_process_identity_before": launcherIdentityBefore as Any? ?? NSNull(),
                "launcher_process_identity_after": launcherIdentityAfter as Any? ?? NSNull(),
                "launcher_process_identity_revalidated": launcherUnchanged,
                "input_authorized": false,
                "input_posted": false,
                "mvp_complete": false,
                "ax_set_value_calls": 0,
                "ax_press_calls": 0,
                "set_focus_calls": 0,
                "cg_post_event_calls": 0,
                "window_activation_calls": request.activation_observation?.attempt_count ?? 0,
                "application_activation_calls": request.activation_observation?.attempt_count ?? 0,
                "activation_api_return_value": request.activation_observation?.return_value as Any? ?? NSNull(),
                "activation_request_status": request.activation_observation?.status as Any? ?? NSNull(),
                "activation_window_scope_verified": request.activation_observation?.window_verified as Any? ?? NSNull(),
                "system_focused_application_pid_before_activation": request.activation_observation?.system_focused_application_pid_before_activation as Any? ?? NSNull(),
                "system_focused_application_pid_after_activation": frontmostPID as Any? ?? NSNull(),
                "system_focused_application_error_after_activation": frontmostError,
                "activation_effect_matches_target_pid": request.activation_observation?.attempted == true && frontmostPID == request.target_pid,
                "activation_api_success_is_not_focus_effect_proof": true,
                "ax_process_trusted_before": trust,
                "ax_process_trusted_after": trustAfter,
                "process_identity_before": identityBefore,
                "process_identity_after": identityAfter as Any? ?? NSNull(),
                "process_identity_revalidated": processUnchanged && identityAfter != nil,
                "probe_parent_pid": request.probe_parent_pid,
                "target_kernel_parent_pid": identityBefore["ppid"] ?? NSNull(),
                "target_kernel_parent_path": identityBefore["parent_executable_path"] ?? NSNull(),
                "target_kernel_parent_kind": (identityBefore["ppid"] as? NSNumber)?.int32Value == 1 ? "launchd" : "recorded_in_private_launch_receipt",
                "target_kernel_parent_matches_receipt": (identityBefore["ppid"] as? NSNumber)?.int32Value == receipt.target_identity.parent_pid,
                "target_pid_was_preexisting": receipt.preexisting_textedit_pids.contains(request.target_pid),
                "cg_windows_before": cgBefore as Any? ?? NSNull(),
                "cg_windows_after": cgAfter as Any? ?? NSNull(),
                "unique_owned_window_count_before": candidates.count,
                "unique_owned_window_count_after": cgAfter?.count ?? -1,
                "original_cg_window": uniqueCGWindow as Any? ?? NSNull(),
                "original_cg_window_unchanged_after": windowUnchanged,
                "ax_windows_error": axWindowsError.rawValue,
                "ax_window_count": axWindows?.count as Any? ?? NSNull(),
                "ax_window_match_count_by_frame": matchingAXWindows.count,
                "expected_temp_document_title": expectedTitle,
                "cg_window_title": cgTitle as Any? ?? NSNull(),
                "ax_window_title": axTitle as Any? ?? NSNull(),
                "one_temp_document_window_verified": oneDocumentWindow,
                "original_ax_window": selectedWindow.map(reader.describe) as Any? ?? NSNull(),
                "focused_window_error": focusedWindowError.rawValue,
                "focused_window": focusedWindow.map(reader.describe) as Any? ?? NSNull(),
                "focused_window_error_after": focusedWindowAfterError.rawValue,
                "focused_window_after": focusedWindowAfter.map(reader.describe) as Any? ?? NSNull(),
                "focused_window_same_cf_after": axWindowUnchanged,
                "original_ax_window_same_cf_after": axWindowUnchanged,
                "system_focused_application_error": frontmostError,
                "system_focused_application_pid": frontmostPID as Any? ?? NSNull(),
                "system_focused_application_error_after": frontmostErrorAfter,
                "system_focused_application_pid_after": frontmostPIDAfter as Any? ?? NSNull(),
                "system_focused_application_pid_after_probe": frontmostPIDAfter as Any? ?? NSNull(),
                "receiver_error": receiverError.rawValue,
                "receiver_error_after": receiverAfterError.rawValue,
                "receiver": receiverDetails as Any? ?? NSNull(),
                "receiver_after": receiverAfter.map(reader.describe) as Any? ?? NSNull(),
                "receiver_is_current_system_receiver": receiverIsCurrent,
                "receiver_is_verified_for_temp_document_readback": currentReceiverForTempDocument,
                "receiver_same_cf_after": receiverUnchanged,
                "receiver_role": receiverRole as Any? ?? NSNull(),
                "receiver_in_bounded_ax_tree_count": receiverInTreeCount,
                "receiver_window_relation_same_cf": receiverBelongsToCurrentWindow as Any? ?? NSNull(),
                "text_areas": textAreaDetails,
                "text_fields": textFieldDetails,
                "field_editor_relation": fieldEditorReport,
                "tis_before": tisBefore,
                "tis_after": tisAfter,
                "tis_metadata_unchanged": NSDictionary(dictionary: tisBefore).isEqual(to: tisAfter),
                "composition_conclusion": "Unknown",
                "selection_range_is_not_composition_evidence": true,
                "input_source_name_is_not_composition_evidence": true,
                "deadline_exceeded": uptimeMs() >= reader.deadline,
                "probe_complete": trust && candidates.count == 1 && selectedWindow != nil && oneDocumentWindow && processUnchanged && windowUnchanged && launcherUnchanged,
                "text_edit_delivery_or_saved_mvp_claimed": false
            ]
            let data = try JSONSerialization.data(withJSONObject: jsonValue(result), options: [.sortedKeys])
            FileHandle.standardOutput.write(data + Data([10]))
        } catch {
            let observedIdentity = observedTargetPID.flatMap(processIdentity)
            let identityIsOwned = (observedIdentity?["ppid"] as? NSNumber)?.int32Value == getppid() &&
                observedIdentity?["executable_path"] as? String == "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
            let result: [String: Any] = [
                "kind": "unicode_receiver_readonly_probe",
                "run_id": runID,
                "error": String(describing: error),
                "ax_process_trusted": AXIsProcessTrusted(),
                "process_identity_observed": observedIdentity as Any? ?? NSNull(),
                "cg_windows_observed": identityIsOwned ? observedTargetPID.flatMap(cgWindows) as Any? ?? NSNull() : NSNull(),
                "input_authorized": false,
                "input_posted": false,
                "mvp_complete": false,
                "text_edit_delivery_or_saved_mvp_claimed": false
            ]
            if let data = try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]) {
                FileHandle.standardOutput.write(data + Data([10]))
            }
            exit(2)
        }
    }
}
