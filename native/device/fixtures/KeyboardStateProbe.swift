import CoreGraphics
import Darwin
import Dispatch
import Foundation

/// Read-only identity and keyboard-state probe for the owned death matrix.
/// This executable deliberately has no AppKit or Accessibility dependency.
private struct ProbeRequest: Decodable {
    let target_kind: String
    let pid: Int32
    let expected_parent_pid: Int32
    let expected_executable_path: String
    let owner_directory: String
    let bundle_id: String?
    let window_id: UInt32?
}

private struct ProcessIdentity {
    let stamp: proc_bsdinfo
    let executablePath: String
}

private func canonicalPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
}

private func processIdentity(_ pid: Int32) throws -> ProcessIdentity {
    guard pid > 0 else { throw NSError(domain: "invalid process id", code: 1) }
    var stamp = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
    guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size else {
        throw NSError(domain: "process identity unavailable", code: 2)
    }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else {
        throw NSError(domain: "process executable unavailable", code: 3)
    }
    let path = String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    return ProcessIdentity(stamp: stamp, executablePath: canonicalPath(path))
}

private func bundleIdentifier(atExecutable executablePath: String) throws -> String {
    let bundle = URL(fileURLWithPath: executablePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    let infoURL = bundle.appendingPathComponent("Info.plist")
    let data = try Data(contentsOf: infoURL, options: [.mappedIfSafe])
    guard let info = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
          let identifier = info["CFBundleIdentifier"] as? String else {
        throw NSError(domain: "fixture bundle identifier unavailable", code: 4)
    }
    return identifier
}

private func ownedWindow(pid: Int32, windowID: UInt32) -> [String: Any]? {
    let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]] ?? []
    return windows.first { item in
        (item[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid
            && (item[kCGWindowNumber as String] as? NSNumber)?.uint32Value == windowID
    }
}

private func windowMetadata(_ window: [String: Any]) -> [String: Any] {
    [
        "window_id": (window[kCGWindowNumber as String] as? NSNumber) ?? 0,
        "owner_pid": (window[kCGWindowOwnerPID as String] as? NSNumber) ?? 0,
        "layer": (window[kCGWindowLayer as String] as? NSNumber) ?? -1
    ]
}

private func keyboardSource(_ state: CGEventSourceStateID, name: String) -> [String: Any] {
    let started = DispatchTime.now().uptimeNanoseconds / 1_000
    let flagsBefore = CGEventSource.flagsState(state)
    var pressed: [UInt16] = []
    var count = 0
    for code in UInt16(0)...UInt16(127) {
        if CGEventSource.keyState(state, key: code) { pressed.append(code) }
        count += 1
    }
    let flagsAfter = CGEventSource.flagsState(state)
    let ended = DispatchTime.now().uptimeNanoseconds / 1_000
    return [
        "source": name,
        "source_id": state.rawValue,
        "capture_started_uptime_microseconds": String(started),
        "capture_ended_uptime_microseconds": String(ended),
        "capture_complete": count == 128,
        "atomic_snapshot": false,
        "queried_virtual_code_min": 0,
        "queried_virtual_code_max": 127,
        "key_query_count": count,
        "pressed_virtual_codes": pressed,
        "flags_before": flagsBefore.rawValue,
        "flags_after": flagsAfter.rawValue,
        "flags_unchanged": flagsBefore == flagsAfter,
        "key_query_error_reporting_available": false,
        "unobservable": [
            "CGEventSource.keyState exposes no per-key query error.",
            "The 128 key queries are not an atomic snapshot."
        ]
    ]
}

private func keyboardSnapshot() -> [String: Any] {
    [
        "combinedSessionState": keyboardSource(.combinedSessionState, name: "combinedSessionState"),
        "hidSystemState": keyboardSource(.hidSystemState, name: "hidSystemState")
    ]
}

private func sameStart(_ lhs: proc_bsdinfo, _ rhs: proc_bsdinfo) -> Bool {
    lhs.pbi_pid == rhs.pbi_pid
        && lhs.pbi_ppid == rhs.pbi_ppid
        && lhs.pbi_start_tvsec == rhs.pbi_start_tvsec
        && lhs.pbi_start_tvusec == rhs.pbi_start_tvusec
}

private func run(_ request: ProbeRequest) throws -> [String: Any] {
    let owner = canonicalPath(request.owner_directory)
    let expected = canonicalPath(request.expected_executable_path)
    guard expected.hasPrefix(owner + "/") else {
        throw NSError(domain: "executable escaped owned directory", code: 5)
    }

    let first = try processIdentity(request.pid)
    guard first.executablePath == expected,
          first.stamp.pbi_pid == UInt32(request.pid),
          first.stamp.pbi_ppid == UInt32(request.expected_parent_pid) else {
        throw NSError(domain: "process path or parent mismatch", code: 6)
    }

    var result: [String: Any] = [
        "kind": "keyboard_state_probe",
        "target_kind": request.target_kind,
        "pid": request.pid,
        "kernel_pid": first.stamp.pbi_pid,
        "parent_pid": first.stamp.pbi_ppid,
        "expected_parent_pid": request.expected_parent_pid,
        "executable_path": first.executablePath,
        "process_start_seconds": first.stamp.pbi_start_tvsec,
        "process_start_microseconds": first.stamp.pbi_start_tvusec,
        "process_identity_revalidated": false,
        "input_authorized": false,
        "accessibility_used": false,
        "appkit_used": false
    ]

    if request.target_kind == "fixture" {
        guard let bundleID = request.bundle_id, let windowID = request.window_id,
              try bundleIdentifier(atExecutable: expected) == bundleID,
              bundleID == "com.lingxi.computer-user.guard-fixture",
              let window = ownedWindow(pid: request.pid, windowID: windowID) else {
            throw NSError(domain: "fixed fixture bundle or Core Graphics window mismatch", code: 7)
        }
        result["bundle_id"] = bundleID
        result["window_id"] = windowID
        result["window_cg_before"] = windowMetadata(window)
        result["system_keyboard_before"] = keyboardSnapshot()
        result["system_keyboard_after"] = keyboardSnapshot()
    } else if request.target_kind == "helper" {
        guard URL(fileURLWithPath: expected).lastPathComponent.hasPrefix("owned-key-") else {
            throw NSError(domain: "owned helper executable name mismatch", code: 8)
        }
    } else if request.target_kind == "device" {
        guard URL(fileURLWithPath: expected).lastPathComponent == "cu-device" else {
            throw NSError(domain: "cu-device executable name mismatch", code: 9)
        }
    } else {
        throw NSError(domain: "unknown process target kind", code: 10)
    }

    let last = try processIdentity(request.pid)
    guard sameStart(first.stamp, last.stamp), last.executablePath == expected,
          last.stamp.pbi_ppid == UInt32(request.expected_parent_pid) else {
        throw NSError(domain: "process identity changed during read", code: 11)
    }
    if request.target_kind == "fixture" {
        guard let window = ownedWindow(pid: request.pid, windowID: request.window_id!) else {
            throw NSError(domain: "fixed fixture window changed during read", code: 12)
        }
        result["window_cg_after"] = windowMetadata(window)
    }
    result["process_identity_revalidated"] = true
    return result
}

@main
struct KeyboardStateProbeMain {
    static func main() {
        do {
            let input = FileHandle.standardInput.readDataToEndOfFile()
            guard input.count <= 8192 else { throw NSError(domain: "probe request budget", code: 13) }
            let request = try JSONDecoder().decode(ProbeRequest.self, from: input)
            let result = try run(request)
            let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
            FileHandle.standardOutput.write(data + Data([10]))
        } catch {
            let result: [String: Any] = [
                "kind": "keyboard_state_probe",
                "error": String(describing: error),
                "input_authorized": false,
                "accessibility_used": false,
                "appkit_used": false
            ]
            if let data = try? JSONSerialization.data(withJSONObject: result) {
                FileHandle.standardOutput.write(data + Data([10]))
            }
            exit(2)
        }
    }
}
