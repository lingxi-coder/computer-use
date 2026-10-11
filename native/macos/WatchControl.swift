import Foundation
import Darwin
import CoreFoundation
import os

let maxNativeWatchControlRecordBytes = 8_192
let maxNativeControlSafeInteger: UInt64 = 9_007_199_254_740_991

enum NativeCaptureResultCode: String, Sendable, CaseIterable {
    case deadlineExpired = "deadline_expired"
    case cancelled
    case scopeMismatch = "scope_mismatch"
    case permissionDenied = "permission_denied"
    case captureUnavailable = "capture_unavailable"
    case pairIncomplete = "pair_incomplete"
    case internalError = "internal_error"
}

struct NativeCaptureNowControl: Sendable, Equatable {
    let requestID: String
    let watchID: String
    let sessionEpoch: UInt64
    let surface: SurfaceRef
    let afterMS: UInt64
    let deadlineMS: UInt64

    var scope: NativeWatchScope {
        NativeWatchScope(watchID: watchID, sessionEpoch: sessionEpoch, surface: surface)
    }
}

struct NativeCancelCaptureControl: Sendable, Equatable {
    let requestID: String
    let watchID: String
    let sessionEpoch: UInt64
    let surface: SurfaceRef

    var scope: NativeWatchScope {
        NativeWatchScope(watchID: watchID, sessionEpoch: sessionEpoch, surface: surface)
    }
}

enum NativeWatchControl: Sendable, Equatable {
    case capture(NativeCaptureNowControl)
    case cancel(NativeCancelCaptureControl)
}

enum NativeWatchControlDecodeError: Error, Sendable {
    case invalidRecord
    case scopeMismatch
}

enum WatchControlDelivery: Sendable, Equatable {
    case capture(NativeCaptureNowControl)
    case cancel(requestID: String)
    case rejected(control: NativeCaptureNowControl, code: NativeCaptureResultCode)
    case failed(NativeCaptureResultCode)
    case inputClosed
}

enum WatchControlOffer: Sendable, Equatable {
    case accepted
    case cancelled
    case ignored
    case busy
    case closed
    case failed(NativeCaptureResultCode)
}

private struct CaptureNowControlRecord: Decodable, Sendable {
    let protocol_version: Int
    let command: String
    let request_id: String
    let watch_id: String
    let session_epoch: UInt64
    let surface: SurfaceRef
    let after_ms: UInt64
    let deadline_ms: UInt64
}

private struct CancelCaptureControlRecord: Decodable, Sendable {
    let protocol_version: Int
    let command: String
    let request_id: String
    let watch_id: String
    let session_epoch: UInt64
    let surface: SurfaceRef
}

func safeJSONInteger(_ value: Any?) -> Bool {
    guard let number = value as? NSNumber,
          CFGetTypeID(number) != CFBooleanGetTypeID() else { return false }
    let integerTypes: Set<String> = ["c", "s", "i", "l", "q", "C", "S", "I", "L", "Q"]
    guard integerTypes.contains(String(cString: number.objCType)) else { return false }
    let double = number.doubleValue
    return double.isFinite && double >= 0 && double.rounded(.towardZero) == double
        && double <= Double(maxNativeControlSafeInteger)
}

func validOpaqueNativeID(_ value: String) -> Bool {
    !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && value.utf8.count <= 256
        && !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
}

func safeNativeWatchScope(_ scope: NativeWatchScope) -> Bool {
    scope.valid && scope.sessionEpoch <= maxNativeControlSafeInteger
        && scope.surface.epoch <= maxNativeControlSafeInteger
}

func decodeNativeWatchControl(_ data: Data, expected: NativeWatchScope) throws -> NativeWatchControl {
    guard !data.isEmpty, data.count < maxNativeWatchControlRecordBytes, safeNativeWatchScope(expected) else {
        throw NativeWatchControlDecodeError.invalidRecord
    }
    let raw: Any
    do { raw = try JSONSerialization.jsonObject(with: data) }
    catch { throw NativeWatchControlDecodeError.invalidRecord }
    guard let fields = raw as? [String: Any], let command = fields["command"] as? String else {
        throw NativeWatchControlDecodeError.invalidRecord
    }
    let decoder = JSONDecoder()
    switch command {
    case "capture_now":
        let keys: Set<String> = ["protocol_version", "command", "request_id", "watch_id", "session_epoch",
                                 "surface", "after_ms", "deadline_ms"]
        guard Set(fields.keys) == keys,
              let surface = fields["surface"] as? [String: Any], Set(surface.keys) == ["id", "epoch"],
              safeJSONInteger(fields["protocol_version"]), safeJSONInteger(fields["session_epoch"]),
              safeJSONInteger(fields["after_ms"]), safeJSONInteger(fields["deadline_ms"]),
              safeJSONInteger(surface["epoch"]) else {
            throw NativeWatchControlDecodeError.invalidRecord
        }
        let record: CaptureNowControlRecord
        do { record = try decoder.decode(CaptureNowControlRecord.self, from: data) }
        catch { throw NativeWatchControlDecodeError.invalidRecord }
        guard record.protocol_version == 1, record.command == "capture_now",
              validOpaqueNativeID(record.request_id), validOpaqueNativeID(record.watch_id),
              record.after_ms <= maxNativeControlSafeInteger, record.deadline_ms <= maxNativeControlSafeInteger,
              record.deadline_ms > record.after_ms else {
            throw NativeWatchControlDecodeError.invalidRecord
        }
        guard record.watch_id == expected.watchID, record.session_epoch == expected.sessionEpoch,
              record.surface == expected.surface, record.surface.valid else {
            throw NativeWatchControlDecodeError.scopeMismatch
        }
        return .capture(NativeCaptureNowControl(requestID: record.request_id, watchID: record.watch_id,
            sessionEpoch: record.session_epoch, surface: record.surface, afterMS: record.after_ms,
            deadlineMS: record.deadline_ms))
    case "cancel_capture":
        let keys: Set<String> = ["protocol_version", "command", "request_id", "watch_id",
                                 "session_epoch", "surface"]
        guard Set(fields.keys) == keys,
              let surface = fields["surface"] as? [String: Any], Set(surface.keys) == ["id", "epoch"],
              safeJSONInteger(fields["protocol_version"]), safeJSONInteger(fields["session_epoch"]),
              safeJSONInteger(surface["epoch"]) else {
            throw NativeWatchControlDecodeError.invalidRecord
        }
        let record: CancelCaptureControlRecord
        do { record = try decoder.decode(CancelCaptureControlRecord.self, from: data) }
        catch { throw NativeWatchControlDecodeError.invalidRecord }
        guard record.protocol_version == 1, record.command == "cancel_capture",
              validOpaqueNativeID(record.request_id), validOpaqueNativeID(record.watch_id),
              record.surface.valid else {
            throw NativeWatchControlDecodeError.invalidRecord
        }
        guard record.watch_id == expected.watchID, record.session_epoch == expected.sessionEpoch,
              record.surface == expected.surface else {
            throw NativeWatchControlDecodeError.scopeMismatch
        }
        return .cancel(NativeCancelCaptureControl(requestID: record.request_id, watchID: record.watch_id,
            sessionEpoch: record.session_epoch, surface: record.surface))
    default:
        throw NativeWatchControlDecodeError.invalidRecord
    }
}

/// Thread-safe one-pending-capture mailbox. Cancellation is an independent
/// priority slot so it can revoke an active platform request while that API is
/// suspended; a second CaptureNow is converted to one bounded busy result.
final class WatchControlMailbox: Sendable {
    private struct State: Sendable {
        let expected: NativeWatchScope
        var pending: NativeCaptureNowControl?
        var activeRequestID: String?
        var cancelPriorityID: String?
        var cancellationDelivered = false
        var rejectedCapture: NativeCaptureNowControl?
        var failure: NativeCaptureResultCode?
        var inputClosed = false
        var inputClosedDelivered = false
        var stopped = false
    }

    private let state: OSAllocatedUnfairLock<State>

    init(scope: NativeWatchScope) {
        state = OSAllocatedUnfairLock(initialState: State(expected: scope))
    }

    func offer(_ control: NativeWatchControl) -> WatchControlOffer {
        state.withLock { state in
            guard !state.stopped, !state.inputClosed, state.failure == nil else { return .closed }
            switch control {
            case .capture(let request):
                guard request.scope == state.expected else {
                    state.failure = .scopeMismatch
                    return .failed(.scopeMismatch)
                }
                guard state.pending == nil, state.activeRequestID == nil,
                      state.cancelPriorityID == nil else {
                    guard state.rejectedCapture == nil else {
                        state.failure = .internalError
                        return .failed(.internalError)
                    }
                    state.rejectedCapture = request
                    return .busy
                }
                state.pending = request
                return .accepted
            case .cancel(let request):
                guard request.scope == state.expected else {
                    state.failure = .scopeMismatch
                    return .failed(.scopeMismatch)
                }
                guard state.pending?.requestID == request.requestID
                        || state.activeRequestID == request.requestID else { return .ignored }
                state.pending = nil
                state.cancelPriorityID = request.requestID
                state.cancellationDelivered = false
                return .cancelled
            }
        }
    }

    func next() -> WatchControlDelivery? {
        state.withLock { state in
            if let failure = state.failure { return .failed(failure) }
            if let requestID = state.cancelPriorityID, !state.cancellationDelivered {
                state.cancellationDelivered = true
                return .cancel(requestID: requestID)
            }
            if let request = state.rejectedCapture {
                state.rejectedCapture = nil
                return .rejected(control: request, code: .captureUnavailable)
            }
            if let request = state.pending {
                state.pending = nil
                state.activeRequestID = request.requestID
                return .capture(request)
            }
            if state.inputClosed, !state.inputClosedDelivered {
                state.inputClosedDelivered = true
                return .inputClosed
            }
            return nil
        }
    }

    func isCancelled(_ requestID: String) -> Bool {
        state.withLock { state in
            state.stopped || state.inputClosed || state.failure != nil
                || state.cancelPriorityID == requestID
        }
    }

    func finish(_ requestID: String) {
        state.withLock { state in
            // The caller must retain an active ID until the native capture task
            // has returned and either published or discarded its late result.
            if state.activeRequestID == requestID { state.activeRequestID = nil }
            if state.cancelPriorityID == requestID {
                state.cancelPriorityID = nil
                state.cancellationDelivered = false
            }
        }
    }

    func failInput(_ code: NativeCaptureResultCode) {
        state.withLock { state in
            guard state.failure == nil else { return }
            state.failure = code
            state.pending = nil
        }
    }

    func closeInput() {
        state.withLock { state in
            state.inputClosed = true
            if let pending = state.pending {
                state.pending = nil
                state.cancelPriorityID = pending.requestID
                state.cancellationDelivered = false
            } else if let active = state.activeRequestID {
                state.cancelPriorityID = active
                state.cancellationDelivered = false
            }
        }
    }

    func stop() {
        state.withLock { state in
            state.stopped = true
            state.pending = nil
            if let active = state.activeRequestID {
                state.cancelPriorityID = active
                state.cancellationDelivered = false
            }
        }
    }

    var isStopped: Bool { state.withLock { $0.stopped } }
}

enum NativeWatchControlLine: Sendable, Equatable {
    case record(Data)
    case invalid
}

private struct ControlLineState: Sendable {
    var partial = Data()
    var discardingOversize = false
    var closed = false
}

final class NativeWatchControlLineBuffer: Sendable {
    private let state = OSAllocatedUnfairLock(initialState: ControlLineState())

    func append(_ bytes: Data) -> [NativeWatchControlLine] {
        state.withLock { state in
            guard !state.closed else { return [.invalid] }
            var lines: [NativeWatchControlLine] = []
            for byte in bytes {
                if byte == 0x0a {
                    if state.discardingOversize {
                        lines.append(.invalid)
                    } else {
                        var record = state.partial
                        if record.last == 0x0d { record.removeLast() }
                        lines.append(record.isEmpty ? .invalid : .record(record))
                    }
                    state.partial.removeAll(keepingCapacity: true)
                    state.discardingOversize = false
                } else if !state.discardingOversize {
                    if state.partial.count >= maxNativeWatchControlRecordBytes - 1 {
                        state.partial.removeAll(keepingCapacity: false)
                        state.discardingOversize = true
                    } else {
                        state.partial.append(byte)
                    }
                }
            }
            return lines
        }
    }

    func finish() -> Bool {
        state.withLock { state in
            let clean = state.partial.isEmpty && !state.discardingOversize
            state.closed = true
            state.partial.removeAll(keepingCapacity: false)
            state.discardingOversize = false
            return clean
        }
    }
}

/// A nonblocking reader for the watch-only NDJSON control stream. Its handler
/// sees only bounded Data and Sendable value DTOs; the existing AX worker and
/// MainActor StreamWriter remain owned by the watch loop.
@MainActor
final class WatchControlReader {
    let mailbox: WatchControlMailbox
    private let source: DispatchSourceRead

    init(fileDescriptor: Int32 = STDIN_FILENO, scope: NativeWatchScope) throws {
        guard safeNativeWatchScope(scope) else { throw NativeFailure("watch control scope invalid") }
        let originalFlags = fcntl(fileDescriptor, F_GETFL)
        guard originalFlags >= 0,
              fcntl(fileDescriptor, F_SETFL, originalFlags | O_NONBLOCK) == 0 else {
            throw NativeFailure("cannot initialize nonblocking watch control input")
        }
        let mailbox = WatchControlMailbox(scope: scope)
        let lineBuffer = NativeWatchControlLineBuffer()
        let queue = DispatchQueue(label: "cu.source.watch-control", qos: .userInitiated)
        let source = DispatchSource.makeReadSource(fileDescriptor: fileDescriptor, queue: queue)
        self.mailbox = mailbox
        self.source = source
        // Dispatch invokes these callbacks on the dedicated control queue.
        // Explicit Sendable closures do not inherit this initializer's
        // MainActor isolation; only the locked mailbox crosses the boundary.
        source.setEventHandler { @Sendable [weak source] in
            let shouldStop = readWatchControl(fileDescriptor: fileDescriptor, expected: scope,
                mailbox: mailbox, lineBuffer: lineBuffer)
            if shouldStop { source?.cancel() }
        }
        source.setCancelHandler { @Sendable in
            _ = fcntl(fileDescriptor, F_SETFL, originalFlags)
        }
        source.resume()
    }

    func stop() {
        mailbox.stop()
        source.cancel()
    }
}

private func readWatchControl(fileDescriptor: Int32, expected: NativeWatchScope,
                              mailbox: WatchControlMailbox, lineBuffer: NativeWatchControlLineBuffer) -> Bool {
    guard !mailbox.isStopped else { return true }
    var bytes = [UInt8](repeating: 0, count: 4096)
    while true {
        let count = Darwin.read(fileDescriptor, &bytes, bytes.count)
        if count > 0 {
            let lines = lineBuffer.append(Data(bytes.prefix(count)))
            for line in lines {
                switch line {
                case .record(let data):
                    do {
                        let control = try decodeNativeWatchControl(data, expected: expected)
                        if case .failed(let code) = mailbox.offer(control) {
                            mailbox.failInput(code)
                            return true
                        }
                    } catch NativeWatchControlDecodeError.scopeMismatch {
                        mailbox.failInput(.scopeMismatch)
                        return true
                    } catch {
                        mailbox.failInput(.internalError)
                        return true
                    }
                case .invalid:
                    mailbox.failInput(.internalError)
                    return true
                }
            }
            if mailbox.isStopped { return true }
            continue
        }
        if count == 0 {
            if lineBuffer.finish() { mailbox.closeInput() }
            else { mailbox.failInput(.internalError) }
            return true
        }
        if errno == EINTR { continue }
        if errno == EAGAIN || errno == EWOULDBLOCK { return false }
        mailbox.failInput(.internalError)
        return true
    }
}
