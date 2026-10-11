import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import Foundation
import os

let ownedValueDependencyNames: Set<String> = [
    "authorization", "focus", "window", "owned_scope",
]
let ownedValueSetterAction = "ax_set_owned_document_value"
let ownedDocumentSaveAction = "ax_save_document"
private let ownedValueMaxAgeMS: UInt64 = 2_000
private let ownedJournalMaxBytes: UInt64 = 256 * 1024
private let ownedJournalMaxRecords = 256
private let ownedJournalMaxRecordBytes = 4 * 1024

func ownedValueSetterTimeoutMS(now: UInt64, deadline: UInt64) -> UInt64? {
    guard now < deadline else { return nil }
    return min(25, deadline - now)
}

func ownedJournalEncodedLine(
    _ supplied: [String: Any],
    sequence: UInt64,
    previousChecksum: String
) -> (line: Data, checksum: String)? {
    guard sequence > 0, previousChecksum.count == 64,
          previousChecksum.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
        return nil
    }
    var payload = supplied
    payload["sequence"] = sequence
    payload["previous_checksum"] = previousChecksum
    guard let canonicalPayload = try? JSONSerialization.data(
        withJSONObject: payload,
        options: [.sortedKeys, .withoutEscapingSlashes]
    ) else { return nil }
    let checksum = SHA256.hash(data: canonicalPayload)
        .map { String(format: "%02x", $0) }.joined()
    var line = Data("{\"checksum\":\"\(checksum)\",\"payload\":".utf8)
    line.append(canonicalPayload)
    line.append(contentsOf: [0x7d, 0x0a])
    guard line.count <= ownedJournalMaxRecordBytes else { return nil }
    return (line, checksum)
}

private struct NativeOwnedValueWire: Decodable, Sendable {
    let kind: String
    let request_id: String
    let intent_id: String
    let attempt_id: String?
    let task_id: String
    let task_revision: UInt64
    let root_epoch: UInt64
    let permit: NativePermit
    let stop_seq: UInt64
    let guard_revision: UInt64
    let clock_domain: String
    let surface: NativeSurface
    let evidence_id: String
    let intent_evidence_id: String
    let source_sequence: UInt64
    let observed_ms: UInt64
    let valid_until_ms: UInt64
    let command_end_ms: UInt64
    let started_ms: UInt64
    let duration_ms: UInt64
    let latest_start_ms: UInt64
    let original_proof: NativeOwnedDocumentBindingProof
    let current_proof: NativeOwnedDocumentBindingProof
    let dependencies: [String: UInt64]
    let action: String
    let value: String
}

struct NativeOwnedValueRequest: Sendable, Equatable {
    let request_id: String
    let intent_id: String
    let attempt_id: String
    let task_id: String
    let task_revision: UInt64
    let root_epoch: UInt64
    let permit: NativePermit
    let stop_seq: UInt64
    let guard_revision: UInt64
    let clock_domain: String
    let surface: NativeSurface
    let evidence_id: String
    let intent_evidence_id: String
    let source_sequence: UInt64
    let observed_ms: UInt64
    let valid_until_ms: UInt64
    let command_end_ms: UInt64
    let started_ms: UInt64
    let duration_ms: UInt64
    let latest_start_ms: UInt64
    let original_proof: NativeOwnedDocumentBindingProof
    let current_proof: NativeOwnedDocumentBindingProof
    let dependencies: [String: UInt64]
    let action: String
    let value: String
    let scopeDigest: String
    let registrationID: String

    static func decode(
        _ data: Data,
        registration: NativeOwnedDocumentRegistration,
        expectedSessionEpoch: UInt64,
        expectedSurface: NativeSurface,
        expectedClockDomain: String,
        nowSourceMS: UInt64,
        allowSave: Bool = false
    ) -> NativeOwnedValueRequest? {
        guard !data.isEmpty, data.count <= 32 * 1024,
              ownedJSONHasNoDuplicateKeys(data),
              let raw = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return nil
        }
        let required: Set<String> = [
            "kind", "request_id", "intent_id", "attempt_id", "task_id", "task_revision",
            "root_epoch", "permit", "stop_seq", "guard_revision", "clock_domain", "surface",
            "evidence_id", "intent_evidence_id", "source_sequence", "observed_ms",
            "valid_until_ms", "command_end_ms", "started_ms", "duration_ms", "latest_start_ms",
            "original_proof", "current_proof", "dependencies", "action", "value",
        ]
        guard Set(raw.keys) == required,
              let permit = raw["permit"] as? [String: Any], Set(permit.keys) == ["id", "generation"],
              let surface = raw["surface"] as? [String: Any], Set(surface.keys) == ["id", "epoch"],
              let originalRaw = raw["original_proof"] as? [String: Any],
              let currentRaw = raw["current_proof"] as? [String: Any],
              let originalIdentity = proofIdentityBytes(originalRaw),
              let currentIdentity = proofIdentityBytes(currentRaw),
              originalIdentity == currentIdentity,
              let currentReceiver = currentRaw["receiver"] as? [String: Any],
              let currentProvenance = currentReceiver["provenance"] as? [String: Any],
              let sourceSequence = raw["source_sequence"] as? UInt64,
              let observedMS = raw["observed_ms"] as? UInt64,
              let validUntilMS = raw["valid_until_ms"] as? UInt64,
              let currentSourceSequence = currentProvenance["source_sequence"] as? UInt64,
              let currentObservedMS = currentProvenance["observed_ms"] as? UInt64,
              let currentValidUntilMS = currentProvenance["valid_until_ms"] as? UInt64,
              let currentClockDomain = currentProvenance["clock_domain"] as? String,
              currentSourceSequence == sourceSequence,
              currentObservedMS <= observedMS,
              validUntilMS <= currentValidUntilMS,
              currentClockDomain == expectedClockDomain else { return nil }

        do {
            let wire = try JSONDecoder().decode(NativeOwnedValueWire.self, from: data)
            let originalData = try JSONSerialization.data(
                withJSONObject: originalRaw,
                options: [.sortedKeys, .withoutEscapingSlashes]
            )
            let currentData = try JSONSerialization.data(
                withJSONObject: currentRaw,
                options: [.sortedKeys, .withoutEscapingSlashes]
            )
            guard originalData.count <= 8 * 1024, currentData.count <= 8 * 1024 else {
                return nil
            }
            let original = try NativeOwnedDocumentBindingProof.decode(
                originalData,
                registration: registration,
                expectedSessionEpoch: expectedSessionEpoch,
                expectedSurface: expectedSurface,
                expectedClockDomain: expectedClockDomain,
                nowSourceMS: nowSourceMS,
                maxAgeMS: ownedValueMaxAgeMS
            )
            let current = try NativeOwnedDocumentBindingProof.decode(
                currentData,
                registration: registration,
                expectedSessionEpoch: expectedSessionEpoch,
                expectedSurface: expectedSurface,
                expectedClockDomain: expectedClockDomain,
                nowSourceMS: nowSourceMS,
                maxAgeMS: ownedValueMaxAgeMS
            )
            guard wire.original_proof == original, wire.current_proof == current,
                  wire.kind == "owned_value_submit",
                  wire.action == ownedValueSetterAction
                    || allowSave && wire.action == ownedDocumentSaveAction,
                  nativeID(wire.request_id), wire.request_id.utf8.count <= 128,
                  nativeID(wire.intent_id), nativeID(wire.attempt_id ?? ""),
                  nativeID(wire.task_id), wire.task_revision > 0, wire.root_epoch > 0,
                  nativeID(wire.permit.id), wire.permit.generation > 0,
                  nativeID(wire.evidence_id), nativeID(wire.intent_evidence_id),
                  wire.source_sequence > 0, wire.observed_ms <= nowSourceMS,
                  wire.valid_until_ms > nowSourceMS,
                  wire.valid_until_ms - nowSourceMS <= 1_500,
                  wire.valid_until_ms <= wire.command_end_ms,
                  wire.duration_ms > 0, wire.duration_ms <= 1_500,
                  wire.started_ms <= wire.latest_start_ms,
                  wire.started_ms <= nowSourceMS,
                  wire.started_ms <= UInt64.max - wire.duration_ms,
                  wire.command_end_ms == wire.started_ms + wire.duration_ms,
                  wire.clock_domain == expectedClockDomain,
                  wire.surface == expectedSurface,
                  wire.dependencies.count == ownedValueDependencyNames.count,
                  Set(wire.dependencies.keys) == ownedValueDependencyNames,
                  wire.dependencies.values.allSatisfy({ $0 > 0 }),
                  original.environment_revision > 0,
                  current.environment_revision >= original.environment_revision,
                  original.binding == current.binding,
                  original.semantic_target == current.semantic_target,
                  original.session_epoch == expectedSessionEpoch,
                  current.session_epoch == expectedSessionEpoch,
                  current.snapshot_observed_ms == wire.observed_ms,
                  (wire.action == ownedDocumentSaveAction
                      ? ownedDocumentSaveValueIsValid(wire.value)
                      : ownedDocumentSetterValueIsValid(wire.value)) else { return nil }
            let scopeDigest = SHA256.hash(data: currentIdentity)
                .map { String(format: "%02x", $0) }.joined()
            return NativeOwnedValueRequest(
                request_id: wire.request_id,
                intent_id: wire.intent_id,
                attempt_id: wire.attempt_id!,
                task_id: wire.task_id,
                task_revision: wire.task_revision,
                root_epoch: wire.root_epoch,
                permit: wire.permit,
                stop_seq: wire.stop_seq,
                guard_revision: wire.guard_revision,
                clock_domain: wire.clock_domain,
                surface: wire.surface,
                evidence_id: wire.evidence_id,
                intent_evidence_id: wire.intent_evidence_id,
                source_sequence: wire.source_sequence,
                observed_ms: wire.observed_ms,
                valid_until_ms: wire.valid_until_ms,
                command_end_ms: wire.command_end_ms,
                started_ms: wire.started_ms,
                duration_ms: wire.duration_ms,
                latest_start_ms: wire.latest_start_ms,
                original_proof: original,
                current_proof: current,
                dependencies: wire.dependencies,
                action: wire.action,
                value: wire.value,
                scopeDigest: scopeDigest,
                registrationID: registration.registration_id
            )
        } catch {
            return nil
        }
    }

    var validIdentity: Bool {
        !request_id.isEmpty && !intent_id.isEmpty && !attempt_id.isEmpty && !task_id.isEmpty
            && root_epoch > 0 && task_revision > 0 && permit.generation > 0
            && dependencies.count == 4 && Set(dependencies.keys) == ownedValueDependencyNames
            && dependencies.values.allSatisfy { $0 > 0 }
            && [ownedValueSetterAction, ownedDocumentSaveAction].contains(action)
    }
}

private func proofIdentityBytes(_ proof: [String: Any]) -> Data? {
    guard let receiver = proof["receiver"] as? [String: Any],
          let provenance = receiver["provenance"] as? [String: Any],
          let identityBoundary = provenance["identity_boundary"] else { return nil }
    let proofIdentityKeys = ["binding", "semantic_target", "session_epoch", "surface"]
    let receiverIdentityKeys = [
        "registration_id", "profile", "process", "window_id", "window_object_ref",
        "window_frame", "actual_object_ref", "receiver_frame", "ancestry",
    ]
    var projection: [String: Any] = [:]
    for key in proofIdentityKeys {
        guard let value = proof[key] else { return nil }
        projection[key] = value
    }
    var receiverProjection: [String: Any] = [:]
    for key in receiverIdentityKeys {
        guard let value = receiver[key] else { return nil }
        receiverProjection[key] = value
    }
    receiverProjection["identity_boundary"] = identityBoundary
    guard let clockDomain = provenance["clock_domain"] else { return nil }
    receiverProjection["clock_domain"] = clockDomain
    projection["receiver"] = receiverProjection
    return try? JSONSerialization.data(
        withJSONObject: projection,
        options: [.sortedKeys, .withoutEscapingSlashes]
    )
}

struct NativeOwnedValueTicket: Sendable, Equatable {
    let request: NativeOwnedValueRequest
    let deadline: UInt64
}

struct NativeOwnedValueSubmitResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "owned_value_submit"
    let session_epoch: UInt64
    let request_id: String
    var outcome: String
    var reason: String
    var submitted = false
    var committed_ms: UInt64? = nil
    var returned_ms: UInt64? = nil

    init(sessionEpoch: UInt64, requestID: String, outcome: String = "rejected", reason: String) {
        session_epoch = sessionEpoch
        request_id = requestID
        self.outcome = outcome
        self.reason = reason
    }
}

private struct NativeOwnedJournalCursor: Sendable {
    var byteCount: UInt64 = 0
    var recordCount = 0
    var sequence: UInt64 = 0
    var previousChecksum = String(repeating: "0", count: 64)
    var writing = false
    var failed = false
}

/// FD 2 is the helper's dedicated journal sink in owned mode. It is opened by
/// Device with O_APPEND|O_EXCL|O_NOFOLLOW and is never used for diagnostics.
final class NativeOwnedInputJournal: Sendable {
    private let fd: Int32
    private let device: UInt64
    private let inode: UInt64
    private let owner: uid_t
    private let cursor = OSAllocatedUnfairLock(initialState: NativeOwnedJournalCursor())

    init?(fd: Int32) {
        var status = stat()
        let flags = fcntl(fd, F_GETFL)
        guard fstat(fd, &status) == 0,
              (status.st_mode & S_IFMT) == S_IFREG,
              status.st_uid == getuid(),
              status.st_mode & 0o777 == 0o600,
              status.st_nlink == 1,
              status.st_size == 0,
              flags >= 0, flags & O_APPEND != 0 else { return nil }
        self.fd = fd
        device = UInt64(status.st_dev)
        inode = UInt64(status.st_ino)
        owner = getuid()
    }

    func appendPrepared(_ ticket: NativeOwnedValueTicket) -> Bool {
        var payload = commonPayload(ticket)
        payload["kind"] = "prepared"
        return append(payload)
    }

    func appendTerminal(_ ticket: NativeOwnedValueTicket, response: NativeOwnedValueSubmitResponse) -> Bool {
        var payload = commonPayload(ticket)
        payload["kind"] = "terminal"
        payload["outcome"] = response.outcome
        payload["submitted"] = response.submitted
        if let committed = response.committed_ms { payload["committed_source_ms"] = committed }
        else { payload["committed_source_ms"] = NSNull() }
        if let returned = response.returned_ms { payload["returned_source_ms"] = returned }
        else { payload["returned_source_ms"] = NSNull() }
        return append(payload)
    }

    private func commonPayload(_ ticket: NativeOwnedValueTicket) -> [String: Any] {
        let request = ticket.request
        return [
            "attempt_id": request.attempt_id,
            "action": request.action,
            "clock_domain": request.clock_domain,
            "command_end_ms": request.command_end_ms,
            "dependencies": request.dependencies,
            "duration_ms": request.duration_ms,
            "evidence_id": request.evidence_id,
            "guard_revision": request.guard_revision,
            "intent_evidence_id": request.intent_evidence_id,
            "intent_id": request.intent_id,
            "latest_start_ms": request.latest_start_ms,
            "native_deadline_uptime_ms": ticket.deadline,
            "observed_ms": request.observed_ms,
            "permit_generation": request.permit.generation,
            "permit_id": request.permit.id,
            "registration_id": request.registrationID,
            "request_id": request.request_id,
            "root_epoch": request.root_epoch,
            "scope_digest": request.scopeDigest,
            "session_epoch": request.current_proof.session_epoch,
            "source_sequence": request.source_sequence,
            "started_ms": request.started_ms,
            "stop_seq": request.stop_seq,
            "surface_epoch": request.surface.epoch,
            "surface_id": request.surface.id,
            "task_id": request.task_id,
            "task_revision": request.task_revision,
            "valid_until_ms": request.valid_until_ms,
        ]
    }

    private func append(_ supplied: [String: Any]) -> Bool {
        let reservation = cursor.withLock { value -> (UInt64, Int, UInt64, String)? in
            guard !value.failed, !value.writing,
                  value.recordCount < ownedJournalMaxRecords,
                  value.sequence < UInt64.max else { return nil }
            value.writing = true
            return (value.byteCount, value.recordCount, value.sequence + 1, value.previousChecksum)
        }
        guard let (oldByteCount, oldRecordCount, nextSequence, previousChecksum) = reservation else {
            markFailed()
            return false
        }
        guard let encoded = ownedJournalEncodedLine(
            supplied, sequence: nextSequence, previousChecksum: previousChecksum
        ) else {
            markFailed()
            return false
        }
        let line = encoded.line
        let checksum = encoded.checksum
        guard
              oldByteCount <= ownedJournalMaxBytes,
              UInt64(line.count) <= ownedJournalMaxBytes - oldByteCount,
              oldRecordCount + 1 <= ownedJournalMaxRecords,
              verifyFileSize(oldByteCount) else {
            markFailed()
            return false
        }

        var offset = 0
        let writeSucceeded = line.withUnsafeBytes { bytes -> Bool in
            guard let base = bytes.baseAddress else { return false }
            while offset < bytes.count {
                let result = Darwin.write(fd, base.advanced(by: offset), bytes.count - offset)
                if result > 0 { offset += result; continue }
                if result < 0 && errno == EINTR { continue }
                return false
            }
            return true
        }
        guard writeSucceeded, fsync(fd) == 0,
              verifyFileSize(oldByteCount + UInt64(line.count)) else {
            markFailed()
            return false
        }
        let lineCount = UInt64(line.count)
        cursor.withLock {
            guard $0.writing, !$0.failed, $0.byteCount == oldByteCount,
                  $0.sequence + 1 == nextSequence else {
                $0.failed = true
                $0.writing = false
                return
            }
            $0.byteCount += lineCount
            $0.recordCount += 1
            $0.sequence = nextSequence
            $0.previousChecksum = checksum
            $0.writing = false
        }
        return !cursor.withLock { $0.failed }
    }

    private func verifyFileSize(_ expected: UInt64) -> Bool {
        var status = stat()
        return fstat(fd, &status) == 0
            && UInt64(status.st_dev) == device
            && UInt64(status.st_ino) == inode
            && status.st_uid == owner
            && (status.st_mode & S_IFMT) == S_IFREG
            && status.st_mode & 0o777 == 0o600
            && status.st_nlink == 1
            && status.st_size >= 0
            && UInt64(status.st_size) == expected
    }

    private func markFailed() {
        cursor.withLock {
            $0.failed = true
            $0.writing = false
        }
    }
}

func atomicOwnedValueAction(_ ticket: NativeOwnedValueTicket, state: GuardState) -> NativeOwnedValueSubmitResponse {
    let request = ticket.request
    var response = NativeOwnedValueSubmitResponse(
        sessionEpoch: state.epoch,
        requestID: request.request_id,
        outcome: "context_unresolved",
        reason: "owned document receiver is unavailable"
    )
    guard let journal = state.ownedInputJournal,
          journal.appendPrepared(ticket) else {
        state.failOwnedJournal(request.permit)
        response.outcome = "rejected"
        response.reason = "owned input journal could not durably prepare this request"
        return response
    }
    let isSave = request.action == ownedDocumentSaveAction
    guard request.action == ownedValueSetterAction
            || isSave && state.ownedSaveExecutionEnabled else {
        response.outcome = "rejected"
        response.reason = "owned action is outside the startup grant"
        if !journal.appendTerminal(ticket, response: response) {
            state.failOwnedJournal(request.permit)
            response.reason = "owned action was refused before input; journal terminal record failed"
        }
        return response
    }

    let sourceNow: () -> UInt64 = { state.ownedSourceNowMS() ?? 0 }
    let beforeVerification = monotonicMS()
    let actionTimeoutMS = min(UInt64(150), ticket.deadline > beforeVerification
        ? ticket.deadline - beforeVerification
        : 0)
    let result: NativeOwnedValueSubmitResponse?
    var probeStage = OwnedReceiverProbeStage.initial
    if actionTimeoutMS == 0 {
        response.outcome = "expired"
        response.reason = "owned value deadline expired before AX verification"
        result = response
    } else {
        result = withVerifiedOwnedReceiver(
            registration: state.ownedRegistration!,
            proof: request.current_proof,
            setterValue: request.value,
            valuePolicy: isSave ? .save : .setter,
            expectedSessionEpoch: state.epoch,
            expectedSurface: request.surface,
            expectedClockDomain: request.clock_domain,
            maxAgeMS: ownedValueMaxAgeMS,
            sourceNowMS: sourceNow,
            deadlineMS: ticket.deadline,
            onStage: { probeStage = $0 }
        ) { element in
            if isSave {
                guard let saveItem = verifiedOwnedSaveTarget(
                    registration: state.ownedRegistration!,
                    receiver: element,
                    expectedValue: request.value,
                    deadlineMS: ticket.deadline
                ) else {
                    return state.refuseOwnedCommit(
                        ticket,
                        outcome: "context_unresolved",
                        reason: "owned Save target, current value or focus changed before AXPress"
                    )
                }
                return state.commitOwnedValue(ticket) {
                    AXUIElementPerformAction(saveItem, kAXPressAction as CFString) == .success
                }
            }
            guard let setterTimeoutMS = ownedValueSetterTimeoutMS(
                now: monotonicMS(), deadline: ticket.deadline
            ) else {
                return state.refuseOwnedCommit(ticket, outcome: "expired", reason: "owned value deadline expired before setter")
            }
            guard AXUIElementSetMessagingTimeout(element, Float(setterTimeoutMS) / 1000) == .success else {
                return state.refuseOwnedCommit(ticket, outcome: "context_unresolved", reason: "AX setter timeout could not be established")
            }
            return state.commitOwnedValue(ticket) {
                AXUIElementSetAttributeValue(
                    element,
                    kAXValueAttribute as CFString,
                    request.value as CFString
                ) == .success
            }
        }
    }
    if var final = result {
        if !final.submitted && final.outcome == "context_unresolved"
            && final.reason == "owned document receiver is unavailable" {
            final.outcome = "context_unresolved"
            final.reason = "independent owned receiver verification failed"
        }
        if !journal.appendTerminal(ticket, response: final) {
            if final.submitted {
                state.failOwnedJournal(request.permit)
                final.outcome = "unknown"
                final.reason = "native owned action ran but its terminal journal record was not durable; do not replay"
            } else {
                state.failOwnedJournal(request.permit)
                final.reason = "owned action was refused before input; journal terminal record failed"
            }
        }
        return final
    }
    response.reason = "owned_receiver_" + probeStage.rawValue
    if !journal.appendTerminal(ticket, response: response) {
        state.failOwnedJournal(request.permit)
        response.reason = "owned action was refused before input; journal terminal record failed"
    }
    return response
}
