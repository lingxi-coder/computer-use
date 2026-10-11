import Foundation
import Darwin

private func requireWatchControlTest(_ condition: @autoclosure () -> Bool, _ name: String) throws {
    guard condition() else { throw NativeFailure("watch-control self-test failed: \(name)") }
}

private func watchCaptureRecord(scope: NativeWatchScope, requestID: String = "computer-capture-1",
                               afterMS: Any = NSNumber(value: 100), deadlineMS: Any = NSNumber(value: 300),
                               extra: [String: Any] = [:], omit: Set<String> = []) throws -> Data {
    var fields: [String: Any] = [
        "protocol_version": 1,
        "command": "capture_now",
        "request_id": requestID,
        "watch_id": scope.watchID,
        "session_epoch": scope.sessionEpoch,
        "surface": ["id": scope.surface.id, "epoch": scope.surface.epoch],
        "after_ms": afterMS,
        "deadline_ms": deadlineMS,
    ]
    for key in omit { fields.removeValue(forKey: key) }
    for (key, value) in extra { fields[key] = value }
    return try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
}

private func watchCancelRecord(scope: NativeWatchScope, requestID: String = "computer-capture-1",
                               extra: [String: Any] = [:], omit: Set<String> = []) throws -> Data {
    var fields: [String: Any] = [
        "protocol_version": 1,
        "command": "cancel_capture",
        "request_id": requestID,
        "watch_id": scope.watchID,
        "session_epoch": scope.sessionEpoch,
        "surface": ["id": scope.surface.id, "epoch": scope.surface.epoch],
    ]
    for key in omit { fields.removeValue(forKey: key) }
    for (key, value) in extra { fields[key] = value }
    return try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
}

/// Exercise Dispatch callbacks created from MainActor, rather than just the
/// decoder and mailbox. These bytes stay inside a local anonymous pipe.
@MainActor
func runWatchControlReaderTests() async throws -> [String] {
    var descriptors: [Int32] = [-1, -1]
    guard pipe(&descriptors) == 0 else { throw NativeFailure("watch-control test pipe unavailable") }
    let readFD = descriptors[0]
    var writeFD = descriptors[1]
    let originalFlags = fcntl(readFD, F_GETFL)
    let scope = NativeWatchScope(watchID: "reader-watch", sessionEpoch: 7,
                                 surface: SurfaceRef(id: "main", epoch: 7))
    let reader: WatchControlReader
    do { reader = try WatchControlReader(fileDescriptor: readFD, scope: scope) }
    catch {
        _ = Darwin.close(readFD)
        _ = Darwin.close(writeFD)
        throw error
    }
    defer {
        reader.stop()
        _ = Darwin.close(readFD)
        if writeFD >= 0 { _ = Darwin.close(writeFD) }
    }

    func send(_ data: Data) throws {
        let written = data.withUnsafeBytes { bytes in
            Darwin.write(writeFD, bytes.baseAddress, bytes.count)
        }
        guard written == data.count else { throw NativeFailure("watch-control test pipe write incomplete") }
    }
    func nextDelivery() async throws -> WatchControlDelivery {
        let deadline = uptimeNs() + 2_000_000_000
        while uptimeNs() < deadline {
            if let delivery = reader.mailbox.next() { return delivery }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        throw NativeFailure("watch-control test callback did not deliver before deadline")
    }

    var capture = try watchCaptureRecord(scope: scope)
    capture.append(0x0a)
    try send(capture)
    let first = try await nextDelivery()
    guard case .capture(let request) = first else {
        throw NativeFailure("watch-control test capture was not delivered")
    }
    try requireWatchControlTest(request.scope == scope && request.requestID == "computer-capture-1",
                                "background Dispatch reader preserves the capture scope")

    var cancel = try watchCancelRecord(scope: scope)
    cancel.append(0x0a)
    try send(cancel)
    let second = try await nextDelivery()
    try requireWatchControlTest(second == .cancel(requestID: request.requestID),
                                "background Dispatch reader delivers cancellation")
    reader.mailbox.finish(request.requestID)

    _ = Darwin.close(writeFD)
    writeFD = -1
    let third = try await nextDelivery()
    try requireWatchControlTest(third == .inputClosed, "background Dispatch reader observes EOF")
    reader.stop()
    let cancellationDeadline = uptimeNs() + 2_000_000_000
    while fcntl(readFD, F_GETFL) != originalFlags && uptimeNs() < cancellationDeadline {
        try await Task.sleep(nanoseconds: 1_000_000)
    }
    try requireWatchControlTest(fcntl(readFD, F_GETFL) == originalFlags,
                                "background cancel handler restores original descriptor flags")
    return ["control_reader_dispatch_capture_cancel_eof_and_flags"]
}

func runWatchControlTests() throws -> [String] {
    let scope = NativeWatchScope(watchID: "watch-1", sessionEpoch: 7,
                                 surface: SurfaceRef(id: "main", epoch: 7))
    let first = NativeCaptureNowControl(requestID: "computer-capture-1", watchID: scope.watchID,
        sessionEpoch: scope.sessionEpoch, surface: scope.surface, afterMS: 100, deadlineMS: 300)
    var tests: [String] = []

    let exactCapture = try watchCaptureRecord(scope: scope)
    let decodedCapture = try decodeNativeWatchControl(exactCapture, expected: scope)
    try requireWatchControlTest(decodedCapture == .capture(first),
                                "capture control exact startup scope")
    let exactCancel = try watchCancelRecord(scope: scope)
    let cancelExpected = NativeCancelCaptureControl(requestID: first.requestID, watchID: scope.watchID,
        sessionEpoch: scope.sessionEpoch, surface: scope.surface)
    let decodedCancel = try decodeNativeWatchControl(exactCancel, expected: scope)
    try requireWatchControlTest(decodedCancel == .cancel(cancelExpected),
                                "cancel control carries exact private startup scope")
    tests.append("control_records_require_exact_startup_scope")

    let foreignSurface = NativeWatchScope(watchID: scope.watchID, sessionEpoch: scope.sessionEpoch,
                                          surface: SurfaceRef(id: "other", epoch: scope.surface.epoch))
    var scopeMismatch = false
    do { _ = try decodeNativeWatchControl(exactCapture, expected: foreignSurface) }
    catch NativeWatchControlDecodeError.scopeMismatch { scopeMismatch = true }
    try requireWatchControlTest(scopeMismatch, "foreign surface is a typed scope mismatch")
    tests.append("foreign_watch_surface_fails_closed")

    let unsafeInteger = try watchCaptureRecord(scope: scope,
        afterMS: NSNumber(value: maxNativeControlSafeInteger + 1))
    let booleanInteger = try watchCaptureRecord(scope: scope, afterMS: true)
    let integerSpelling = try watchCaptureRecord(scope: scope)
    guard let integerJSON = String(data: integerSpelling, encoding: .utf8) else {
        throw NativeFailure("watch-control self-test failed: JSON UTF-8 fixture")
    }
    let floatingInteger = Data(integerJSON.replacingOccurrences(of: "\"after_ms\":100",
        with: "\"after_ms\":100.0").utf8)
    try requireWatchControlTest((try? decodeNativeWatchControl(unsafeInteger, expected: scope)) == nil
        && (try? decodeNativeWatchControl(booleanInteger, expected: scope)) == nil
        && (try? decodeNativeWatchControl(floatingInteger, expected: scope)) == nil,
        "capture times reject unsafe integers, booleans and floating JSON numbers")
    tests.append("control_times_require_safe_json_integers")

    let missing = try watchCaptureRecord(scope: scope, omit: ["surface"])
    let unknown = try watchCaptureRecord(scope: scope, extra: ["extra": "rejected"])
    let missingCancelScope = try watchCancelRecord(scope: scope, omit: ["session_epoch"])
    let unknownCancelField = try watchCancelRecord(scope: scope, extra: ["surface_extra": 1])
    let badOrder = try watchCaptureRecord(scope: scope, afterMS: 300, deadlineMS: 300)
    let longSpan = try watchCaptureRecord(scope: scope, afterMS: 0, deadlineMS: 40_000)
    try requireWatchControlTest((try? decodeNativeWatchControl(missing, expected: scope)) == nil
        && (try? decodeNativeWatchControl(unknown, expected: scope)) == nil
        && (try? decodeNativeWatchControl(missingCancelScope, expected: scope)) == nil
        && (try? decodeNativeWatchControl(unknownCancelField, expected: scope)) == nil
        && (try? decodeNativeWatchControl(badOrder, expected: scope)) == nil,
        "control schema rejects missing, unknown and reversed deadline fields")
    try requireWatchControlTest((try? decodeNativeWatchControl(longSpan, expected: scope)) != nil,
                                "control preserves original deadlines beyond a native timeout")
    tests.append("control_schema_is_strict_without_shortening_deadline")

    let utf8WithinLimit = String(repeating: "界", count: 85)
    let utf8OverLimit = String(repeating: "界", count: 86)
    let withinBytes = try watchCaptureRecord(scope: scope, requestID: utf8WithinLimit)
    let overBytes = try watchCaptureRecord(scope: scope, requestID: utf8OverLimit)
    try requireWatchControlTest((try? decodeNativeWatchControl(withinBytes, expected: scope)) != nil
        && (try? decodeNativeWatchControl(overBytes, expected: scope)) == nil,
        "opaque request IDs are bounded by UTF-8 bytes")
    tests.append("request_id_uses_utf8_byte_bound")

    let mailbox = WatchControlMailbox(scope: scope)
    try requireWatchControlTest(mailbox.offer(.capture(first)) == .accepted,
                                "first capture occupies one pending slot")
    let active = mailbox.next()
    guard active == .capture(first) else { throw NativeFailure("watch-control self-test failed: capture slot delivery") }
    let second = NativeCaptureNowControl(requestID: "computer-capture-2", watchID: scope.watchID,
        sessionEpoch: scope.sessionEpoch, surface: scope.surface, afterMS: 110, deadlineMS: 300)
    try requireWatchControlTest(mailbox.offer(.capture(second)) == .busy,
                                "active capture rejects a concurrent request")
    try requireWatchControlTest(mailbox.next() == .rejected(control: second, code: .captureUnavailable),
                                "busy rejection preserves request scope and barrier")
    mailbox.finish(first.requestID)
    tests.append("mailbox_capacity_one_reports_bounded_busy_request")

    let cancelMailbox = WatchControlMailbox(scope: scope)
    try requireWatchControlTest(cancelMailbox.offer(.capture(first)) == .accepted,
                                "pending request accepts cancellation test")
    try requireWatchControlTest(cancelMailbox.offer(.cancel(cancelExpected)) == .cancelled,
                                "matching cancellation wins before pending capture")
    try requireWatchControlTest(cancelMailbox.isCancelled(first.requestID)
        && cancelMailbox.next() == .cancel(requestID: first.requestID)
        && cancelMailbox.next() == nil,
        "pending cancellation is prioritized and never delivered as capture")
    cancelMailbox.finish(first.requestID)
    tests.append("cancel_priority_does_not_deliver_capture")

    let activeCancelMailbox = WatchControlMailbox(scope: scope)
    _ = activeCancelMailbox.offer(.capture(first))
    _ = activeCancelMailbox.next()
    _ = activeCancelMailbox.offer(.cancel(cancelExpected))
    activeCancelMailbox.closeInput()
    try requireWatchControlTest(activeCancelMailbox.isCancelled(first.requestID)
        && activeCancelMailbox.next() == .cancel(requestID: first.requestID)
        && activeCancelMailbox.next() == .inputClosed,
        "EOF cancels active capture before closing control stream")
    activeCancelMailbox.finish(first.requestID)
    tests.append("control_eof_cancels_active_capture")

    let bounded = try watchCaptureRecord(scope: scope)
    var maxPayload = bounded
    maxPayload.append(contentsOf: repeatElement(UInt8(0x20), count: maxNativeWatchControlRecordBytes - 1 - maxPayload.count))
    let maxLineBuffer = NativeWatchControlLineBuffer()
    var exactLine = maxPayload
    exactLine.append(0x0a)
    let maxLines = maxLineBuffer.append(exactLine)
    guard maxLines.count == 1, case .record(let retained) = maxLines[0], retained.count == maxNativeWatchControlRecordBytes - 1 else {
        throw NativeFailure("watch-control self-test failed: maximum line accepted")
    }
    let oversizeBuffer = NativeWatchControlLineBuffer()
    var oversize = Data(repeating: 0x20, count: maxNativeWatchControlRecordBytes)
    oversize.append(0x0a)
    let oversizeLines = oversizeBuffer.append(oversize)
    try requireWatchControlTest(oversizeLines.count == 1 && oversizeLines[0] == .invalid,
                                "NDJSON reader rejects records above 8 KiB")
    let unfinishedBuffer = NativeWatchControlLineBuffer()
    try requireWatchControlTest(unfinishedBuffer.append(bounded).isEmpty && !unfinishedBuffer.finish(),
                                "control EOF rejects an unterminated record")
    tests.append("bounded_ndjson_accepts_exact_limit_and_rejects_overflow_or_partial")

    return tests
}
