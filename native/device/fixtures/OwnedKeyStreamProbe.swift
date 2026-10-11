// A bounded, listen-only comparison tap for one owned fixture and one owned key helper.
import ApplicationServices
import CoreGraphics
import Darwin
import Foundation
import os

private struct KernelIdentity: Sendable, Equatable {
    let pid: Int32
    let parentPID: Int32
    let startSeconds: Int64
    let startMicroseconds: Int64
    let executablePath: String
    let parentExecutablePath: String?

    var json: [String: Any] {
        [
            "pid": pid,
            "parent_pid": parentPID,
            "start_seconds": startSeconds,
            "start_microseconds": startMicroseconds,
            "executable_path": executablePath,
            "parent_executable_path": parentExecutablePath as Any? ?? NSNull()
        ]
    }
}

private struct StreamEvent: Sendable {
    let sequence: Int
    let receivedUptimeNanoseconds: UInt64
    let eventType: UInt32
    let keyCode: Int64
    let sourcePID: Int64
    let sourceUserData: Int64
    let eventTimestamp: UInt64

    var json: [String: Any] {
        [
            "sequence": sequence,
            "received_uptime_ns": String(receivedUptimeNanoseconds),
            "event_type": eventType,
            "key_code": keyCode,
            "event_source_unix_process_id": sourcePID,
            "event_source_user_data": String(sourceUserData),
            "cg_timestamp": String(eventTimestamp)
        ]
    }
}

private struct CaptureState: Sendable {
    var events: [StreamEvent] = []
    var overflow = false
    var unresolvedSourcePIDCount = 0
    var tapDisabledType: UInt32?
    var captureWindowExpired = false
}

private final class CaptureBuffer: Sendable {
    private let helperPID: Int32
    private let deadlineUptimeNanoseconds: UInt64
    private let state = OSAllocatedUnfairLock(initialState: CaptureState())

    init(helperPID: Int32, durationNanoseconds: UInt64 = 30_000_000_000) {
        self.helperPID = helperPID
        self.deadlineUptimeNanoseconds = DispatchTime.now().uptimeNanoseconds &+ durationNanoseconds
    }

    func markTapDisabled(_ type: CGEventType) {
        state.withLock { $0.tapDisabledType = type.rawValue }
    }

    func record(_ type: CGEventType, event: CGEvent) {
        guard type == .keyDown || type == .keyUp,
              event.getIntegerValueField(.keyboardEventKeycode) == 49 else { return }
        let userData = event.getIntegerValueField(.eventSourceUserData)
        guard userData != 0 else { return }
        let sourcePID = event.getIntegerValueField(.eventSourceUnixProcessID)
        let received = DispatchTime.now().uptimeNanoseconds
        let eventTimestamp = event.timestamp
        state.withLock { current in
            guard received <= deadlineUptimeNanoseconds else {
                current.captureWindowExpired = true
                return
            }
            guard sourcePID > 0 else {
                current.unresolvedSourcePIDCount = min(16, current.unresolvedSourcePIDCount + 1)
                return
            }
            guard sourcePID == Int64(helperPID) else { return }
            guard current.events.count < 16 else {
                current.overflow = true
                return
            }
            current.events.append(StreamEvent(
                sequence: current.events.count + 1,
                receivedUptimeNanoseconds: received,
                eventType: type.rawValue,
                keyCode: 49,
                sourcePID: sourcePID,
                sourceUserData: userData,
                eventTimestamp: eventTimestamp
            ))
        }
    }

    func snapshot() -> CaptureState { state.withLock { $0 } }
}

private let ownedKeyStreamTapCallback: CGEventTapCallBack = { _, type, event, userInfo in
    guard let userInfo else { return Unmanaged.passUnretained(event) }
    let capture = Unmanaged<CaptureBuffer>.fromOpaque(userInfo).takeUnretainedValue()
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
        capture.markTapDisabled(type)
    } else {
        capture.record(type, event: event)
    }
    return Unmanaged.passUnretained(event)
}

private func normalizedPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
}

private func pathForPID(_ pid: Int32) -> String? {
    guard pid > 0 else { return nil }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    let path = String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    return normalizedPath(path)
}

private func processIdentity(_ pid: Int32) -> KernelIdentity? {
    guard pid > 0 else { return nil }
    var stamp = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
    guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size,
          let path = pathForPID(pid) else { return nil }
    let parentPID = Int32(stamp.pbi_ppid)
    return KernelIdentity(
        pid: pid,
        parentPID: parentPID,
        startSeconds: Int64(stamp.pbi_start_tvsec),
        startMicroseconds: Int64(stamp.pbi_start_tvusec),
        executablePath: path,
        parentExecutablePath: pathForPID(parentPID)
    )
}

private func allProcessPIDs() -> [Int32]? {
    let initialBytes = proc_listpids(UInt32(PROC_ALL_PIDS), 0, nil, 0)
    guard initialBytes > 0 else { return nil }
    var capacity = max(4096, Int(initialBytes) / MemoryLayout<pid_t>.stride + 128)
    for _ in 0..<4 {
        var pids = [pid_t](repeating: 0, count: capacity)
        let copied = pids.withUnsafeMutableBufferPointer { buffer in
            proc_listpids(UInt32(PROC_ALL_PIDS), 0, buffer.baseAddress,
                          Int32(buffer.count * MemoryLayout<pid_t>.stride))
        }
        guard copied > 0 else { return nil }
        if Int(copied) < capacity * MemoryLayout<pid_t>.stride {
            let count = Int(copied) / MemoryLayout<pid_t>.stride
            var result: [Int32] = []
            result.reserveCapacity(count)
            for index in 0..<count where pids[index] > 0 {
                result.append(Int32(pids[index]))
            }
            return result
        }
        capacity *= 2
    }
    return nil
}

private func uniqueOwnedFixtureWindowCount(pid: Int32, expectedWindowID: UInt32) -> Int {
    guard let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]] else { return -1 }
    let owned = windows.filter {
        ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid &&
            ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0
    }
    guard owned.count == 1,
          (owned[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value == expectedWindowID else { return owned.count }
    return 1
}

// Controller state and tap/runloop operations are accessed only on the
// main runloop; the stdin worker sends immutable request strings to it.
private final class OwnedKeyStreamController: @unchecked Sendable {
    private struct Configuration {
        let fixture: KernelIdentity
        let fixtureWindowID: UInt32
        let helperParent: KernelIdentity
        let helper: KernelIdentity
        let ownerDirectory: String
    }

    private var configuration: Configuration?
    private var capture: CaptureBuffer?
    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var stopped = false

    func emitReady() {
        emit(["kind": "ready", "probe_pid": getpid(),
              "probe_executable_path": pathForPID(getpid()) as Any? ?? NSNull(),
              "input_authorized": false, "input_posted": false,
              "accessibility_used": false, "appkit_used": false])
    }

    func handle(line: String, mainLoop: MainRunLoop) {
        guard line.utf8.count <= 16 * 1024,
              let raw = try? JSONSerialization.jsonObject(with: Data(line.utf8)),
              let request = raw as? [String: Any],
              let op = request["op"] as? String else {
            emit(["kind": "probe_error", "error": "invalid or oversized control JSON"])
            return
        }
        switch op {
        case "configure": configure(request)
        case "snapshot": snapshot(request)
        case "quit":
            shutdown()
            emit(["kind": "quit_result", "tap_removed": tap == nil,
                  "input_authorized": false, "input_posted": false])
            stopped = true
            mainLoop.stop()
        default:
            emit(["kind": "probe_error", "error": "unsupported control operation"])
        }
    }

    func inputClosed(mainLoop: MainRunLoop) {
        shutdown()
        stopped = true
        mainLoop.stop()
    }

    private func configure(_ request: [String: Any]) {
        guard configuration == nil, tap == nil else {
            emit(["kind": "probe_error", "error": "configure is only accepted once"])
            return
        }
        guard let fixturePID = int32(request["fixture_pid"]),
              let fixtureParentPID = int32(request["fixture_parent_pid"]),
              let fixturePathValue = request["fixture_executable_path"] as? String,
              let windowID = uint32(request["window_id"]),
              let ownerDirectoryValue = request["owner_directory"] as? String,
              let helperParentPID = int32(request["helper_parent_pid"]),
              let helperParentPathValue = request["helper_parent_executable_path"] as? String,
              let helperPathValue = request["helper_executable_path"] as? String else {
            emit(["kind": "probe_error", "error": "configure fields missing or invalid"])
            return
        }
        let ownerDirectory = normalizedPath(ownerDirectoryValue)
        let fixturePath = normalizedPath(fixturePathValue)
        let helperParentPath = normalizedPath(helperParentPathValue)
        let helperPath = normalizedPath(helperPathValue)
        let tempRoot = normalizedPath(NSTemporaryDirectory())
        let expectedFixturePath = normalizedPath(ownerDirectory + "/KeyFixture.app/Contents/MacOS/KeyFixture")
        let expectedHelpersRoot = normalizedPath(ownerDirectory + "/helpers")
        guard ownerDirectory.hasPrefix(tempRoot + "/cu-owned-key-death-"),
              URL(fileURLWithPath: ownerDirectory).lastPathComponent.hasPrefix("cu-owned-key-death-"),
              fixturePath == expectedFixturePath,
              helperPath.hasPrefix(expectedHelpersRoot + "/owned-key-"),
              fixturePID > 0, fixtureParentPID == getppid(), helperParentPID > 0, windowID > 0 else {
            emit(["kind": "probe_error", "error": "owned fixture/helper scope does not match this probe parent"])
            return
        }
        guard let fixture = processIdentity(fixturePID),
              fixture.parentPID == fixtureParentPID,
              fixture.executablePath == fixturePath,
              uniqueOwnedFixtureWindowCount(pid: fixturePID, expectedWindowID: windowID) == 1,
              let helperParent = processIdentity(helperParentPID),
              helperParent.executablePath == helperParentPath,
              helperParent.executablePath.hasSuffix("/cu-device"),
              let pids = allProcessPIDs() else {
            emit(["kind": "probe_error", "error": "fixture/window/helper-parent kernel identity unavailable"])
            return
        }
        let helpers = pids.compactMap(processIdentity).filter {
            $0.parentPID == helperParentPID && $0.executablePath == helperPath
        }
        guard helpers.count == 1, let helper = helpers.first else {
            emit(["kind": "probe_error", "error": "expected exactly one helper child with the owned executable path",
                  "matching_helper_count": helpers.count])
            return
        }
        guard CGPreflightListenEventAccess() else {
            emit(["kind": "probe_error", "error": "listen-only event access is unavailable; no permission request was made"])
            return
        }
        let capture = CaptureBuffer(helperPID: helper.pid)
        let userInfo = Unmanaged.passUnretained(capture).toOpaque()
        let mask = CGEventMask(1 << CGEventType.keyDown.rawValue) | CGEventMask(1 << CGEventType.keyUp.rawValue)
        guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap,
                                         options: .listenOnly, eventsOfInterest: mask,
                                         callback: ownedKeyStreamTapCallback, userInfo: userInfo),
              let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0) else {
            emit(["kind": "probe_error", "error": "listen-only CG session tap could not be created"])
            return
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .defaultMode)
        guard CGEvent.tapIsEnabled(tap: tap) else {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .defaultMode)
            CFMachPortInvalidate(tap)
            emit(["kind": "probe_error", "error": "listen-only CG session tap was created but is not enabled",
                  "tap_enabled": false])
            return
        }
        self.configuration = Configuration(fixture: fixture, fixtureWindowID: windowID,
                                            helperParent: helperParent, helper: helper,
                                            ownerDirectory: ownerDirectory)
        self.capture = capture
        self.tap = tap
        self.runLoopSource = source
        emit([
            "kind": "configured",
            "capture_scope": "exact_helper_source_pid_space_key_nonzero_user_data",
            "tap_location": "cg_session",
            "tap_option": "listen_only",
            "tap_enabled": true,
            "fixture_identity": fixture.json,
            "fixture_window_id": windowID,
            "fixture_window_count": 1,
            "helper_parent_identity": helperParent.json,
            "helper_identity": helper.json,
            "helper_child_identity_unique": true,
            "owner_directory": ownerDirectory,
            "capture_limit": 16,
            "capture_window_limit_ms": 30000,
            "source_pid_zero_policy": "capture_unresolved_no_fallback",
            "input_authorized": false,
            "input_posted": false,
            "accessibility_used": false,
            "appkit_used": false
        ])
    }

    private func snapshot(_ request: [String: Any]) {
        guard let configuration, let capture else {
            emit(["kind": "probe_error", "error": "snapshot requested before successful configure"])
            return
        }
        guard let label = request["label"] as? String, !label.isEmpty, label.utf8.count <= 64 else {
            emit(["kind": "probe_error", "error": "snapshot label missing or exceeds 64 UTF-8 bytes"])
            return
        }
        let state = capture.snapshot()
        let currentFixture = processIdentity(configuration.fixture.pid)
        let currentHelperParent = processIdentity(configuration.helperParent.pid)
        let currentHelper = processIdentity(configuration.helper.pid)
        let windowCount = uniqueOwnedFixtureWindowCount(pid: configuration.fixture.pid,
                                                        expectedWindowID: configuration.fixtureWindowID)
        let fixtureStable = currentFixture == configuration.fixture && windowCount == 1
        let helperParentStable = currentHelperParent == configuration.helperParent
        let helperStillSameIdentity = currentHelper == configuration.helper
        let tapEnabled = tap.map { CGEvent.tapIsEnabled(tap: $0) } ?? false
        let targetScopeRevalidated = fixtureStable && helperParentStable
        let eventStreamCaptureComplete = tapEnabled && state.unresolvedSourcePIDCount == 0 && !state.overflow &&
            !state.captureWindowExpired && state.tapDisabledType == nil
        let hasResolutionFailure = !eventStreamCaptureComplete || !targetScopeRevalidated
        emit([
            "kind": "snapshot",
            "label": label,
            "fixture_identity_revalidated": fixtureStable,
            "target_scope_revalidated": targetScopeRevalidated,
            "fixture_identity": currentFixture?.json as Any? ?? NSNull(),
            "fixture_window_id": configuration.fixtureWindowID,
            "fixture_window_count": windowCount,
            "helper_parent_identity_revalidated": helperParentStable,
            "helper_parent_identity": currentHelperParent?.json as Any? ?? NSNull(),
            "helper_identity_live_after_snapshot": helperStillSameIdentity,
            "helper_identity_after_snapshot": currentHelper?.json as Any? ?? NSNull(),
            "helper_identity_at_configure": configuration.helper.json,
            "tap_enabled": tapEnabled,
            "event_stream_capture_complete": eventStreamCaptureComplete,
            "tap_disabled_type": state.tapDisabledType as Any? ?? NSNull(),
            "source_pid_unresolved_count": state.unresolvedSourcePIDCount,
            "capture_overflow": state.overflow,
            "capture_window_expired": state.captureWindowExpired,
            "capture_resolution": hasResolutionFailure ? "capture_unresolved" : "scoped_capture_complete",
            "events": state.events.map(\.json),
            "event_count": state.events.count,
            "input_authorized": false,
            "input_posted": false,
            "accessibility_used": false,
            "appkit_used": false
        ])
    }

    private func shutdown() {
        if let source = runLoopSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .defaultMode) }
        if let tap { CFMachPortInvalidate(tap) }
        runLoopSource = nil
        tap = nil
    }
}

private func int32(_ value: Any?) -> Int32? {
    guard let number = value as? NSNumber else { return nil }
    let raw = number.int64Value
    guard raw > 0, raw <= Int64(Int32.max) else { return nil }
    return Int32(raw)
}

private func uint32(_ value: Any?) -> UInt32? {
    guard let number = value as? NSNumber else { return nil }
    let raw = number.int64Value
    guard raw > 0, raw <= Int64(UInt32.max) else { return nil }
    return UInt32(raw)
}

private func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
    FileHandle.standardOutput.write(data + Data([10]))
}

// This wrapper only queues work onto the main runloop and wakes/stops it.
private final class MainRunLoop: @unchecked Sendable {
    private let loop = CFRunLoopGetMain()
    private var keepAliveSource: CFRunLoopSource?

    func installKeepAlive() -> Bool {
        var context = CFRunLoopSourceContext(
            version: 0, info: nil, retain: nil, release: nil, copyDescription: nil,
            equal: nil, hash: nil, schedule: nil, cancel: nil, perform: { _ in }
        )
        guard let source = CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &context) else { return false }
        CFRunLoopAddSource(loop, source, CFRunLoopMode.defaultMode!)
        keepAliveSource = source
        return true
    }

    func enqueue(_ work: @escaping @Sendable () -> Void) {
        DispatchQueue.main.async(execute: work)
        CFRunLoopWakeUp(loop)
    }

    func stop() {
        if let keepAliveSource {
            CFRunLoopRemoveSource(loop, keepAliveSource, CFRunLoopMode.defaultMode!)
            CFRunLoopSourceInvalidate(keepAliveSource)
            self.keepAliveSource = nil
        }
        CFRunLoopStop(loop)
        CFRunLoopWakeUp(loop)
    }
}

@main
struct OwnedKeyStreamProbeMain {
    static func main() {
        let controller = OwnedKeyStreamController()
        let mainRunLoop = MainRunLoop()
        guard mainRunLoop.installKeepAlive() else {
            emit(["kind": "probe_error", "error": "could not install stdin-control runloop keepalive"])
            return
        }
        controller.emitReady()
        Thread.detachNewThread {
            while let line = readLine() {
                mainRunLoop.enqueue { controller.handle(line: line, mainLoop: mainRunLoop) }
            }
            mainRunLoop.enqueue { controller.inputClosed(mainLoop: mainRunLoop) }
        }
        CFRunLoopRun()
    }
}
