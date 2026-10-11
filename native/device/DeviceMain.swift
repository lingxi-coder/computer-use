import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import os

private struct Control: Decodable, Sendable {
    let protocol_version: Int
    let session_epoch: UInt64
    let command: String
    let lane: String?
    let duration_ms: UInt64?
    let hit: HitRequest?
    let probe: NativeProbe?
    let permit: NativePermit?
    let clock_origin_ms: UInt64?
    let surface: NativeSurface?
    let clock_domain: String?
    let dependencies: [String: UInt64]?
    let dependency_validity: [String: Bool?]?
    let name: String?
    let revision: UInt64?
    let valid: Bool?
}

private let controlAllowedFields: Set<String> = [
    "protocol_version", "session_epoch", "command", "lane", "duration_ms", "hit", "probe", "permit",
    "clock_origin_ms", "surface", "clock_domain", "dependencies", "dependency_validity",
    "name", "revision", "valid", "owned_value",
]

private func controlFields(_ line: Data) -> [String: Any]? {
    guard let fields = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
          Set(fields.keys).isSubset(of: controlAllowedFields) else { return nil }
    return fields
}

private func ownedAuthorityBootstrapControlSelfTest() throws {
    let payload: [String: Any] = [
        "protocol_version": 1, "session_epoch": 7, "command": "configure_authority",
        "clock_origin_ms": 1_000, "surface": ["id": "main", "epoch": 7],
        "clock_domain": "macos/7/wall",
        "dependencies": ["authorization": 1, "focus": 1, "window": 1, "owned_scope": 1],
        "dependency_validity": ["authorization": true, "focus": NSNull(), "window": false, "owned_scope": true],
    ]
    let line = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    guard let fields = controlFields(line), Set(fields.keys) == Set(payload.keys) else {
        throw NSError(domain: "OwnedAuthorityBootstrapControlSelfTest", code: 1)
    }
    let control = try JSONDecoder().decode(Control.self, from: line)
    guard let validity = control.dependency_validity, validity.count == 4,
          validity["authorization"] == true, validity["window"] == false,
          validity.keys.contains("focus"), validity["focus"]! == nil else {
        throw NSError(domain: "OwnedAuthorityBootstrapControlSelfTest", code: 2)
    }
    var extra = payload
    extra["unexpected"] = true
    guard controlFields(try JSONSerialization.data(withJSONObject: extra)) == nil else {
        throw NSError(domain: "OwnedAuthorityBootstrapControlSelfTest", code: 3)
    }
}

private enum WorkerRequest: Sendable {
    case diagnostic(HitRequest), probe(NativeProbeTicket), probeRejected(String, String)
    case submit(NativeProbeTicket), submitRejected(String, String)
    case ownedValue(NativeOwnedValueTicket), ownedValueRejected(String, String, String)
}
private enum WorkerResponse: Sendable {
    case diagnostic(HitResponse), probe(NativeProbeResponse, NativeProbeTicket?), submit(NativeSubmitResponse)
    case ownedValue(NativeOwnedValueSubmitResponse)
}

private final class HitMailbox: Sendable {
    private struct State: Sendable {
        var pending: WorkerRequest?
        var active = false
        var response: WorkerResponse?
    }
    private let state = OSAllocatedUnfairLock(initialState: State())
    func offer(_ request: WorkerRequest) -> Bool {
        state.withLock {
            guard !$0.active, $0.pending == nil, $0.response == nil else { return false }
            $0.pending = request; return true
        }
    }
    func take() -> WorkerRequest? {
        state.withLock { let item = $0.pending; $0.pending = nil; $0.active = item != nil; return item }
    }
    func complete(_ result: WorkerResponse) { state.withLock { $0.active = false; $0.response = result } }
    func response() -> WorkerResponse? { state.withLock { let result = $0.response; $0.response = nil; return result } }
}

private final class TapContext {
    #if CU_OWNED_KEY_ACCEPTANCE
    private var ownedKeyTraceBudget = OSAllocatedUnfairLock(initialState: OwnedKeyTraceBudget())
    #endif
    let state: GuardState
    var classifier: InputClassifier
    init(state: GuardState, marker: Int64) { self.state = state; classifier = InputClassifier(marker: marker) }
    #if CU_OWNED_KEY_ACCEPTANCE
    func claimOwnedKeyTrace(now: UInt64, drainUntil: UInt64) -> Bool {
        ownedKeyTraceBudget.withLock { $0.claim(now: now, drainUntil: drainUntil) }
    }
    #endif
}

#if CU_OWNED_KEY_ACCEPTANCE
private func shouldWriteOwnedKeyTapTrace(type: UInt32, mapped: Bool) -> Bool {
    type == CGEventType.keyUp.rawValue || !mapped
}
#endif

private func strictOwnedValueControlFields(_ line: Data) -> [String: Any]? {
    guard ownedJSONHasNoDuplicateKeys(line),
          let fields = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
          Set(fields.keys) == ["protocol_version", "session_epoch", "command", "owned_value"],
          fields["command"] as? String == "owned_value_submit",
          let value = fields["owned_value"],
          JSONSerialization.isValidJSONObject(value) else { return nil }
    return fields
}

private func ownedValueControlLineSelfTest() throws {
    let valid = Data(#"{"protocol_version":1,"session_epoch":9,"command":"owned_value_submit","owned_value":{"request_id":"fixture","value":"x"}}"#.utf8)
    let duplicateOuter = Data(#"{"protocol_version":1,"session_epoch":9,"command":"owned_value_submit","command":"owned_value_submit","owned_value":{"request_id":"fixture"}}"#.utf8)
    let duplicateNested = Data(#"{"protocol_version":1,"session_epoch":9,"command":"owned_value_submit","owned_value":{"request_id":"fixture","request_id":"other"}}"#.utf8)
    guard strictOwnedValueControlFields(valid) != nil,
          strictOwnedValueControlFields(duplicateOuter) == nil,
          strictOwnedValueControlFields(duplicateNested) == nil else {
        throw NSError(domain: "OwnedValueControlLineSelfTest", code: 1)
    }
}

private func ownedSaveSelectorSelfTest() throws {
    let ordinary = OwnedSaveMenuItemFacts(
        role: kAXMenuItemRole,
        commandCharacter: nil,
        commandModifiers: nil,
        title: nil,
        enabled: nil,
        pressSupported: false,
        identifier: nil
    )
    let save = OwnedSaveMenuItemFacts(
        role: kAXMenuItemRole,
        commandCharacter: "s",
        commandModifiers: 0,
        title: "Save",
        enabled: true,
        pressSupported: true,
        identifier: "TextEdit.Save"
    )
    guard ownedSaveMenuItemIndex([ordinary, save]) == 1,
          ownedSaveMenuItemIndex([save, OwnedSaveMenuItemFacts(
              role: kAXMenuItemRole,
              commandCharacter: "S",
              commandModifiers: 1,
              title: "Save As",
              enabled: true,
              pressSupported: true,
              identifier: "TextEdit.SaveAs"
          )]) == 0,
          ownedDocumentSaveValueIsValid("existing\u{0001}text\n"),
          !ownedDocumentSetterValueIsValid("existing\u{0001}text\n"),
          !ownedDocumentSaveValueIsValid("nul\0text"),
          !ownedDocumentSaveValueIsValid(String(repeating: "中", count: 683)),
          ownedSaveMenuItemIndex([save, save]) == nil,
          ownedSaveMenuItemIndex([ordinary, OwnedSaveMenuItemFacts(
              role: kAXMenuItemRole,
              commandCharacter: "S",
              commandModifiers: nil,
              title: "保存",
              enabled: true,
              pressSupported: true,
              identifier: "save"
          )]) == nil,
          ownedSaveMenuItemIndex([ordinary, OwnedSaveMenuItemFacts(
              role: kAXMenuItemRole,
              commandCharacter: "s",
              commandModifiers: 0,
              title: "Save",
              enabled: false,
              pressSupported: true,
              identifier: "save"
          )]) == nil,
          ownedSaveMenuItemIndex([ordinary, OwnedSaveMenuItemFacts(
              role: kAXMenuItemRole,
              commandCharacter: "s",
              commandModifiers: 0,
              title: "Save",
              enabled: true,
              pressSupported: false,
              identifier: "save"
          )]) == nil,
          ownedSaveMenuItemIndex([ordinary, OwnedSaveMenuItemFacts(
              role: kAXMenuItemRole,
              commandCharacter: "s",
              commandModifiers: 0,
              title: "Save",
              enabled: true,
              pressSupported: true,
              identifier: ""
          )]) == nil else {
        throw NSError(domain: "OwnedSaveSelectorSelfTest", code: 1)
    }
}

private func ownedSaveRequestDecoderSelfTest() throws {
    let surface = NativeSurface(id: "main", epoch: 7)
    let domain = "macos/7/wall"
    let registration = try NativeOwnedDocumentRegistration.decodeForPureValidation(
        ownedValueFixtureRegistration(), nowUnixMS: 10_000
    )
    let proof = try JSONSerialization.jsonObject(with: ownedValueFixtureProof(clockDomain: domain))
    func decode(action: String, allowSave: Bool, value: String = "Expected private test contents") -> NativeOwnedValueRequest? {
        let body: [String: Any] = [
            "kind": "owned_value_submit",
            "request_id": "save-decoder-request",
            "intent_id": "save-decoder-intent",
            "attempt_id": "save-decoder-attempt",
            "task_id": "save-decoder-task",
            "task_revision": 1,
            "root_epoch": 1,
            "permit": ["id": "save-decoder-permit", "generation": 1],
            "stop_seq": 0,
            "guard_revision": 0,
            "clock_domain": domain,
            "surface": ["id": surface.id, "epoch": surface.epoch],
            "evidence_id": "save-decoder-evidence",
            "intent_evidence_id": "save-decoder-intent-evidence",
            "source_sequence": 2,
            "observed_ms": 150,
            "valid_until_ms": 350,
            "command_end_ms": 400,
            "started_ms": 200,
            "duration_ms": 200,
            "latest_start_ms": 250,
            "original_proof": proof,
            "current_proof": proof,
            "dependencies": ["authorization": 1, "focus": 2, "window": 3, "owned_scope": 4],
            "action": action,
            "value": value,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]) else {
            return nil
        }
        return NativeOwnedValueRequest.decode(
            data,
            registration: registration,
            expectedSessionEpoch: 7,
            expectedSurface: surface,
            expectedClockDomain: domain,
            nowSourceMS: 200,
            allowSave: allowSave
        )
    }
    guard decode(action: ownedValueSetterAction, allowSave: false) != nil,
          decode(action: ownedDocumentSaveAction, allowSave: false) == nil,
          decode(action: ownedDocumentSaveAction, allowSave: true)?.action == ownedDocumentSaveAction,
          decode(action: ownedDocumentSaveAction, allowSave: true, value: "existing\u{0001}text") != nil,
          decode(action: ownedValueSetterAction, allowSave: true, value: "existing\u{0001}text") == nil,
          decode(action: ownedDocumentSaveAction, allowSave: true, value: "bad\0value") == nil,
          decode(action: "ax_press_arbitrary_element", allowSave: true) == nil else {
        throw NSError(domain: "OwnedSaveRequestDecoderSelfTest", code: 1)
    }
}

private func ownedUnknownGateSelfTest() throws {
    let state = GuardState(epoch: 9)
    let permit = NativePermit(id: "owned-unknown-fixture", generation: 1)
    let reason = "owned AX action result unknown; input isolated; do not replay"
    let before = state.snapshot(now: 100)
    guard state.failOwnedInput(permit, reason: reason) else {
        throw NSError(domain: "OwnedUnknownGateSelfTest", code: 1)
    }
    let after = state.snapshot(now: 101)
    guard after.stop_revision == before.stop_revision + 1,
          after.fault_sequence == before.fault_sequence + 1,
          after.reason == reason else {
        throw NSError(domain: "OwnedUnknownGateSelfTest", code: 2)
    }
}

private func ownedSetterTimeoutSelfTest() throws {
    guard ownedValueSetterTimeoutMS(now: 100, deadline: 200) == 25,
          ownedValueSetterTimeoutMS(now: 190, deadline: 200) == 10,
          ownedValueSetterTimeoutMS(now: 200, deadline: 200) == nil else {
        throw NSError(domain: "OwnedSetterTimeoutSelfTest", code: 1)
    }
}

private func ownedReturnClockSelfTest() throws {
    let surface = NativeSurface(id: "surface-fixture", epoch: 1)
    guard let clock = NativeAuthorityClock(
        origin: 1_000,
        surface: surface,
        domain: "macos/7/wall",
        epoch: 7,
        wall: 1_100,
        uptime: 1_000
    ) else {
        throw NSError(domain: "OwnedReturnClockSelfTest", code: 1)
    }
    let trusted = inputReturnSourceMSIfTrusted(
        clock: clock,
        committedWallMS: 1_200,
        committedUptimeMS: 1_100,
        returnedWallMS: 1_210,
        returnedUptimeMS: 1_110,
        validUntilMS: 300
    )
    guard trusted == 210,
          ownedValueSetterOutcome(callSucceeded: true, returnedSourceMS: trusted) == "applied" else {
        throw NSError(domain: "OwnedReturnClockSelfTest", code: 2)
    }
    let wallRollback = inputReturnSourceMSIfTrusted(
        clock: clock,
        committedWallMS: 1_200,
        committedUptimeMS: 1_100,
        returnedWallMS: 1_199,
        returnedUptimeMS: 1_110,
        validUntilMS: 300
    )
    let inconsistentAdvance = inputReturnSourceMSIfTrusted(
        clock: clock,
        committedWallMS: 1_200,
        committedUptimeMS: 1_100,
        returnedWallMS: 1_401,
        returnedUptimeMS: 1_110,
        validUntilMS: 500
    )
    guard wallRollback == nil, inconsistentAdvance == nil,
          ownedValueSetterOutcome(callSucceeded: true, returnedSourceMS: wallRollback) == "unknown",
          ownedValueSetterOutcome(callSucceeded: true, returnedSourceMS: inconsistentAdvance) == "unknown" else {
        throw NSError(domain: "OwnedReturnClockSelfTest", code: 3)
    }
}

private func ownedJournalRecordSelfTest() throws {
    let previous = String(repeating: "0", count: 64)
    let prepared: [String: Any] = [
        "action": ownedValueSetterAction,
        "attempt_id": "attempt-fixture",
        "clock_domain": "macos/7/wall",
        "command_end_ms": 1_250,
        "dependencies": ["authorization": 1, "focus": 2, "window": 3, "owned_scope": 4],
        "duration_ms": 200,
        "evidence_id": "evidence-fixture",
        "guard_revision": 5,
        "intent_evidence_id": "evidence-fixture-original",
        "intent_id": "intent-fixture",
        "kind": "prepared",
        "latest_start_ms": 1_100,
        "native_deadline_uptime_ms": 9_999,
        "observed_ms": 1_080,
        "permit_generation": 1,
        "permit_id": "permit-fixture",
        "registration_id": "registration-fixture",
        "request_id": "request-fixture",
        "root_epoch": 1,
        "scope_digest": String(repeating: "a", count: 64),
        "session_epoch": 7,
        "source_sequence": 8,
        "started_ms": 1_050,
        "stop_seq": 1,
        "surface_epoch": 1,
        "surface_id": "surface-fixture",
        "task_id": "task-fixture",
        "task_revision": 1,
        "valid_until_ms": 1_200,
    ]
    guard let first = ownedJournalEncodedLine(prepared, sequence: 1, previousChecksum: previous),
          first.line.count <= 4 * 1024,
          first.line.last == 0x0a,
          let envelope = try? JSONSerialization.jsonObject(with: Data(first.line.dropLast())) as? [String: Any],
          let payload = envelope["payload"] as? [String: Any],
          envelope["checksum"] as? String == first.checksum,
          payload["sequence"] as? Int == 1,
          payload["previous_checksum"] as? String == previous,
          payload["kind"] as? String == "prepared",
          payload["action"] as? String == ownedValueSetterAction,
          payload["value"] == nil, payload["document_path"] == nil else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 1)
    }
    let payloadData = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys, .withoutEscapingSlashes])
    let expectedChecksum = SHA256.hash(data: payloadData).map { String(format: "%02x", $0) }.joined()
    guard expectedChecksum == first.checksum else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 2)
    }
    var terminal = prepared
    terminal["kind"] = "terminal"
    terminal["outcome"] = "applied"
    terminal["submitted"] = true
    terminal["committed_source_ms"] = 1_150
    terminal["returned_source_ms"] = 1_151
    guard let second = ownedJournalEncodedLine(terminal, sequence: 2, previousChecksum: first.checksum),
          let secondEnvelope = try? JSONSerialization.jsonObject(with: Data(second.line.dropLast())) as? [String: Any],
          let secondPayload = secondEnvelope["payload"] as? [String: Any],
          secondPayload["sequence"] as? Int == 2,
          secondPayload["previous_checksum"] as? String == first.checksum,
          secondPayload["kind"] as? String == "terminal",
          secondPayload["action"] as? String == ownedValueSetterAction,
          secondPayload["outcome"] as? String == "applied" else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 3)
    }
    let oversized = ownedJournalEncodedLine(
        ["padding": String(repeating: "x", count: 4 * 1024)],
        sequence: 3,
        previousChecksum: second.checksum
    )
    guard case nil = oversized else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 4)
    }
    var descriptors: [Int32] = [-1, -1]
    guard pipe(&descriptors) == 0 else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 5)
    }
    defer { _ = close(descriptors[0]); _ = close(descriptors[1]) }
    guard NativeOwnedInputJournal(fd: descriptors[1]) == nil else {
        throw NSError(domain: "OwnedJournalRecordSelfTest", code: 6)
    }
}

private func eventCallback(_ proxy: CGEventTapProxy, _ type: CGEventType, _ event: CGEvent,
                           _ context: UnsafeMutableRawPointer?) -> Unmanaged<CGEvent>? {
    if let context {
        let owner = Unmanaged<TapContext>.fromOpaque(context).takeUnretainedValue()
        let rawMarker = event.getIntegerValueField(.eventSourceUserData)
        if rawMarker == owner.state.eventMarker {
            owner.state.observeOwnedPointer(type: type.rawValue, stamp: event.timestamp)
        }
        let ownKey: Bool
        if type == .keyDown || type == .keyUp || type == .flagsChanged ||
            type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            let received = monotonicMS()
            let autorepeat = event.getIntegerValueField(.keyboardEventAutorepeat)
            ownKey = owner.state.observeOwnedKey(marker: rawMarker, type: type.rawValue,
                code: event.getIntegerValueField(.keyboardEventKeycode), flags: event.flags.rawValue,
                stamp: event.timestamp, autorepeat: autorepeat, now: received)
            #if CU_OWNED_KEY_ACCEPTANCE
            if shouldWriteOwnedKeyTapTrace(type: type.rawValue, mapped: ownKey),
               owner.state.keyboardInputAvailable {
                let facts = owner.state.ownedKeyDeliveryFacts()
                if let posted = facts.posted_ms, received >= posted, received < facts.drain_until_ms,
                   owner.claimOwnedKeyTrace(now: received, drainUntil: facts.drain_until_ms) {
                    writeOwnedKeyTapTrace(nonce: rawMarker,
                        code: event.getIntegerValueField(.keyboardEventKeycode), type: type.rawValue,
                        flags: event.flags.rawValue, stamp: event.timestamp, receivedMS: received,
                        autorepeat: autorepeat, mapped: ownKey, facts: facts)
                }
            }
            #endif
        } else {
            ownKey = false
        }
        let classified = owner.classifier.classify(type: type, marker: rawMarker,
            key: event.getIntegerValueField(.keyboardEventKeycode), flags: event.flags, point: event.location)
        // Preserve disabled-tap and local stop handling. Only an exact event
        // mapped to the current key-pair receipt avoids the generic human path.
        let notice: InputNotice = ownKey && classified == .human ? .ignored : classified
        owner.state.notice(notice, type: type.rawValue)
    }
    // This listen-only tap cannot change or suppress the original event.
    return Unmanaged.passUnretained(event)
}

#if CU_OWNED_KEY_ACCEPTANCE
private struct OwnedKeyEventDiagnostic: Encodable {
    struct Expected: Encodable {
        let delivery_base_cg_timestamp: String
        let cg_timestamp_rule: String
        let flags: String
        let code: Int
        let type: UInt32
        let autorepeat: Int64
    }
    struct Actual: Encodable {
        let cg_timestamp: String
        let flags: String
        let code: Int64
        let type: UInt32
        let autorepeat: Int64
        let observed_monotonic_ms: String
    }
    struct Matches: Encodable {
        let cg_timestamp_rule_satisfied: Bool
        let flags: Bool
        let code: Bool
        let type: Bool
        let autorepeat: Bool
    }
    let expected: Expected
    let actual: Actual
    let matches: Matches

    init(_ fact: OwnedKeyTapFact, expectedStamp: UInt64, expectedFlags: UInt64,
         expectedCode: UInt16, expectedType: UInt32, expectedAutorepeat: Int64) {
        let isUp = expectedType == CGEventType.keyUp.rawValue
        expected = Expected(delivery_base_cg_timestamp: String(expectedStamp),
            cg_timestamp_rule: isUp ? "strictly_after_delivery_base" : "equals_delivery_base",
            flags: String(expectedFlags),
            code: Int(expectedCode), type: expectedType, autorepeat: expectedAutorepeat)
        actual = Actual(cg_timestamp: String(fact.stamp), flags: String(fact.flags), code: fact.code,
            type: fact.type, autorepeat: fact.autorepeat, observed_monotonic_ms: String(fact.observed_ms))
        matches = Matches(cg_timestamp_rule_satisfied: isUp ? fact.stamp > expectedStamp : fact.stamp == expectedStamp,
            flags: fact.flags == expectedFlags,
            code: fact.code == Int64(expectedCode), type: fact.type == expectedType,
            autorepeat: fact.autorepeat == expectedAutorepeat)
    }
}

private struct OwnedKeyTraceBudget: Sendable {
    var emitted = 0

    mutating func claim(now: UInt64, drainUntil: UInt64) -> Bool {
        guard now < drainUntil else { return false }
        guard emitted < 2 else { return false }
        emitted += 1
        return true
    }
}

private struct OwnedKeyTapTraceFact: Encodable {
    let cg_timestamp: String
    let raw_flags: String
    let code: Int64
    let type: UInt32
    let autorepeat: Int64
    let received_monotonic_ms: String

    init(type: UInt32, stamp: UInt64, flags: UInt64, code: Int64, autorepeat: Int64, receivedMS: UInt64) {
        cg_timestamp = String(stamp); raw_flags = String(flags); self.code = code
        self.type = type; self.autorepeat = autorepeat; received_monotonic_ms = String(receivedMS)
    }

    init(_ fact: OwnedKeyTapFact) {
        self.init(type: fact.type, stamp: fact.stamp, flags: fact.flags, code: fact.code,
            autorepeat: fact.autorepeat, receivedMS: fact.observed_ms)
    }
}

private struct OwnedKeyTapTrace: Encodable {
    let kind = "owned_key_tap_trace"
    let nonce: String
    let current: OwnedKeyTapTraceFact
    let observer_mapped: Bool
    let rejection_reason: String?
    let posted_monotonic_ms: String
    let acknowledgement_cutoff_upper_bound_ms: String
    let confirmation_cutoff_monotonic_ms: String?
    let drain_until_monotonic_ms: String
    let up_observed: Bool
    let first: OwnedKeyTapTraceFact?
    let second: OwnedKeyTapTraceFact?

    init(nonce: Int64, code: Int64, type: UInt32, flags: UInt64, stamp: UInt64,
         receivedMS: UInt64, autorepeat: Int64, mapped: Bool, facts: OwnedKeyDeliveryFacts) {
        self.nonce = String(nonce)
        current = OwnedKeyTapTraceFact(type: type, stamp: stamp, flags: flags,
            code: code, autorepeat: autorepeat, receivedMS: receivedMS)
        observer_mapped = mapped
        rejection_reason = mapped ? nil : "receipt_observer_rejected_event"
        let posted = facts.posted_ms ?? 0
        let (ackUpperBound, overflow) = posted.addingReportingOverflow(AtomicKeyProfile.releaseAckMS)
        posted_monotonic_ms = String(posted)
        acknowledgement_cutoff_upper_bound_ms = String(overflow ? UInt64.max : ackUpperBound)
        confirmation_cutoff_monotonic_ms = facts.confirmation.map { String($0.cutoff_ms) }
        drain_until_monotonic_ms = String(facts.drain_until_ms)
        up_observed = facts.up_observed
        first = facts.first.map(OwnedKeyTapTraceFact.init)
        second = facts.second.map(OwnedKeyTapTraceFact.init)
    }
}

/// Writes one compact owned-fixture receipt line before the event reaches the
/// human-input classifier. This has no authority effect and never waits.
private func writeOwnedKeyTapTrace(nonce: Int64, code: Int64, type: UInt32, flags: UInt64,
                                   stamp: UInt64, receivedMS: UInt64, autorepeat: Int64, mapped: Bool,
                                   facts: OwnedKeyDeliveryFacts) {
    guard var data = try? JSONEncoder().encode(OwnedKeyTapTrace(nonce: nonce, code: code,
        type: type, flags: flags, stamp: stamp, receivedMS: receivedMS, autorepeat: autorepeat,
        mapped: mapped, facts: facts)),
        data.count < 1024 else { return }
    data.append(10)
    let originalFlags = fcntl(STDERR_FILENO, F_GETFL)
    guard originalFlags >= 0,
          fcntl(STDERR_FILENO, F_SETFL, originalFlags | O_NONBLOCK) == 0 else { return }
    data.withUnsafeBytes { bytes in _ = Darwin.write(STDERR_FILENO, bytes.baseAddress!, bytes.count) }
    _ = fcntl(STDERR_FILENO, F_SETFL, originalFlags)
}

func runOwnedKeyTapTraceTests() throws -> Int {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw DeviceTestFailure(check: passed + 1) }
        passed += 1
    }
    var budget = OwnedKeyTraceBudget()
    let mappedDownNeedsTrace = shouldWriteOwnedKeyTapTrace(type: CGEventType.keyDown.rawValue, mapped: true)
    try check(!mappedDownNeedsTrace)
    if mappedDownNeedsTrace { _ = budget.claim(now: 10, drainUntil: 20) }
    try check(budget.emitted == 0)
    let mappedUpNeedsTrace = shouldWriteOwnedKeyTapTrace(type: CGEventType.keyUp.rawValue, mapped: true)
    let foreignDownNeedsTrace = shouldWriteOwnedKeyTapTrace(type: CGEventType.keyDown.rawValue, mapped: false)
    try check(mappedUpNeedsTrace && foreignDownNeedsTrace)
    if mappedUpNeedsTrace { try check(budget.claim(now: 11, drainUntil: 20)) }
    if foreignDownNeedsTrace { try check(budget.claim(now: 12, drainUntil: 20)) }
    try check(!budget.claim(now: 12, drainUntil: 20))
    var expiredBudget = OwnedKeyTraceBudget()
    try check(!expiredBudget.claim(now: 20, drainUntil: 20))
    let downRuleFact = OwnedKeyTapFact(type: CGEventType.keyDown.rawValue, stamp: 100,
        flags: 0, code: 49, autorepeat: 0, observed_ms: 10)
    let upSameStampFact = OwnedKeyTapFact(type: CGEventType.keyUp.rawValue, stamp: 100,
        flags: 0, code: 49, autorepeat: 0, observed_ms: 10)
    let upLaterStampFact = OwnedKeyTapFact(type: CGEventType.keyUp.rawValue, stamp: 101,
        flags: 0, code: 49, autorepeat: 0, observed_ms: 10)
    try check(OwnedKeyEventDiagnostic(downRuleFact, expectedStamp: 100, expectedFlags: 0,
        expectedCode: 49, expectedType: CGEventType.keyDown.rawValue,
        expectedAutorepeat: 0).matches.cg_timestamp_rule_satisfied)
    try check(!OwnedKeyEventDiagnostic(upSameStampFact, expectedStamp: 100, expectedFlags: 0,
        expectedCode: 49, expectedType: CGEventType.keyUp.rawValue,
        expectedAutorepeat: 0).matches.cg_timestamp_rule_satisfied)
    try check(OwnedKeyEventDiagnostic(upLaterStampFact, expectedStamp: 100, expectedFlags: 0,
        expectedCode: 49, expectedType: CGEventType.keyUp.rawValue,
        expectedAutorepeat: 0).matches.cg_timestamp_rule_satisfied)
    let fact = OwnedKeyTapFact(type: CGEventType.keyUp.rawValue, stamp: UInt64.max,
        flags: UInt64.max, code: Int64.max, autorepeat: 0, observed_ms: UInt64.max)
    let confirmation = OwnedKeyConfirmationFact(observed: false, clear_checked: false,
        clear: false, coverage: false, returned_ms: nil, began_ms: UInt64.max - 2,
        finished_ms: UInt64.max - 1, cutoff_ms: UInt64.max)
    let facts = OwnedKeyDeliveryFacts(expected_stamp: UInt64.max, expected_flags: UInt64.max,
        expected_code: UInt16.max, expected_down_type: CGEventType.keyDown.rawValue,
        expected_up_type: CGEventType.keyUp.rawValue, expected_autorepeat: 0,
        posted_ms: UInt64.max - 150, drain_until_ms: UInt64.max,
        up_observed: false, first: fact, second: fact, confirmation: confirmation)
    let encoder = JSONEncoder()
    let downLine = try encoder.encode(OwnedKeyTapTrace(nonce: Int64.max, code: Int64.min,
        type: CGEventType.keyDown.rawValue, flags: UInt64.max, stamp: UInt64.max,
        receivedMS: UInt64.max, autorepeat: 1, mapped: false, facts: facts))
    let upLine = try encoder.encode(OwnedKeyTapTrace(nonce: Int64.max, code: Int64.max,
        type: CGEventType.keyUp.rawValue, flags: UInt64.max, stamp: UInt64.max,
        receivedMS: UInt64.max, autorepeat: 0, mapped: true, facts: facts))
    try check(downLine.count < 1024 && upLine.count < 1024)
    let upJSON = String(decoding: upLine, as: UTF8.self)
    try check(upJSON.contains("\"observer_mapped\":true"))
    try check(upJSON.contains("\"first\"") && upJSON.contains("\"second\""))
    try check(upJSON.contains("\"cg_timestamp\":\"\(UInt64.max)\""))
    try check(upJSON.contains("\"raw_flags\":\"\(UInt64.max)\""))
    return passed
}

private struct OwnedKeyConfirmationDiagnostic: Encodable {
    let observed: Bool
    let clear_checked: Bool
    let clear: Bool
    let coverage: Bool
    let returned_ms: String?
    let began_monotonic_ms: String
    let finished_monotonic_ms: String
    let cutoff_monotonic_ms: String

    init(_ fact: OwnedKeyConfirmationFact) {
        observed = fact.observed; clear_checked = fact.clear_checked; clear = fact.clear
        coverage = fact.coverage
        returned_ms = fact.returned_ms.map { String($0) }
        began_monotonic_ms = String(fact.began_ms); finished_monotonic_ms = String(fact.finished_ms)
        cutoff_monotonic_ms = String(fact.cutoff_ms)
    }
}

private struct OwnedKeyDeliveryDiagnostic: Encodable {
    let kind = "owned_key_delivery_diagnostic"
    let expected_posted_monotonic_ms: String?
    let expected_drain_until_monotonic_ms: String
    let expected_up_observed: Bool
    let events: [OwnedKeyEventDiagnostic]
    let last_confirmation: OwnedKeyConfirmationDiagnostic?

    init(_ facts: OwnedKeyDeliveryFacts) {
        expected_posted_monotonic_ms = facts.posted_ms.map { String($0) }
        expected_drain_until_monotonic_ms = String(facts.drain_until_ms)
        expected_up_observed = facts.up_observed
        var events: [OwnedKeyEventDiagnostic] = []
        if let first = facts.first {
            events.append(OwnedKeyEventDiagnostic(first, expectedStamp: facts.expected_stamp,
                expectedFlags: facts.expected_flags, expectedCode: facts.expected_code,
                expectedType: facts.expected_down_type, expectedAutorepeat: facts.expected_autorepeat))
        }
        if let second = facts.second {
            events.append(OwnedKeyEventDiagnostic(second, expectedStamp: facts.expected_stamp,
                expectedFlags: facts.expected_flags, expectedCode: facts.expected_code,
                expectedType: facts.expected_up_type, expectedAutorepeat: facts.expected_autorepeat))
        }
        self.events = events
        last_confirmation = facts.confirmation.map(OwnedKeyConfirmationDiagnostic.init)
    }
}

/// Best-effort acceptance diagnostics only: encode first, switch stderr to
/// nonblocking, attempt one bounded write, and drop any failure without
/// waiting or changing the native authority result.
func writeOwnedKeyDeliveryDiagnostic(_ facts: OwnedKeyDeliveryFacts) {
    guard var data = try? JSONEncoder().encode(OwnedKeyDeliveryDiagnostic(facts)), data.count < 2048 else { return }
    data.append(10)
    let originalFlags = fcntl(STDERR_FILENO, F_GETFL)
    guard originalFlags >= 0, fcntl(STDERR_FILENO, F_SETFL, originalFlags | O_NONBLOCK) == 0 else { return }
    data.withUnsafeBytes { bytes in
        _ = Darwin.write(STDERR_FILENO, bytes.baseAddress!, bytes.count)
    }
    _ = fcntl(STDERR_FILENO, F_SETFL, originalFlags)
}
#endif

private func boundedWrite<T: Encodable>(_ value: T, state: GuardState? = nil) {
    guard var data = try? JSONEncoder().encode(value), data.count <= 8192 else {
        if let state { state.close(); return }
        _exit(125)
    }
    data.append(10)
    let deadline = monotonicMS() + 20
    data.withUnsafeBytes { bytes in
        var offset = 0
        while offset < bytes.count {
            let count = Darwin.write(STDOUT_FILENO, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
            if count > 0 { offset += count; continue }
            if count < 0 && errno == EINTR { continue }
            guard count < 0, errno == EAGAIN, monotonicMS() < deadline else {
                // Parent death may race the control reader's EOF. Fence input
                // here too, then let the tap owner drain an already-posted up.
                if let state { state.close(); return }
                _exit(125)
            }
            usleep(1000)
        }
    }
}

private func readControl(state: GuardState, mailbox: HitMailbox) {
    var pending = Data()
    var buffer = [UInt8](repeating: 0, count: 512)
    while !state.closing {
        // Foundation's read(upToCount:) can fill its requested length on a
        // pipe. A short stop must be processed without waiting for more bytes.
        let count = buffer.withUnsafeMutableBytes { Darwin.read(STDIN_FILENO, $0.baseAddress, $0.count) }
        if count < 0 && errno == EINTR { continue }
        guard count > 0 else { state.close(); return }
        for byte in buffer.prefix(count) {
            if byte != 10 {
                if pending.count >= 8192 {
                    guard pending.count < 32 * 1024,
                          pending.starts(with: Data("{\"command\":\"owned_value_submit\"".utf8)) else {
                        state.close(); return
                    }
                }
                pending.append(byte); continue
            }
            let line = pending
            pending.removeAll(keepingCapacity: true)
            guard let fields = controlFields(line),
                  let control = try? JSONDecoder().decode(Control.self, from: line),
                  control.protocol_version == 1, control.session_epoch == state.epoch else { state.close(); return }
            switch control.command {
            case "lease":
                guard let lane = control.lane, let duration = control.duration_ms,
                      state.renew(lane: lane, duration: duration) else { state.close(); return }
            case "stop": state.stop()
            case "hit_test":
                let keys: Set<String> = ["request_id", "pid", "bundle_id", "window_id", "x", "y", "frame", "binding", "binding_digest"]
                guard let fields = fields["hit"] as? [String: Any], Set(fields.keys) == keys,
                      let frame = fields["frame"] as? [String: Any], Set(frame.keys) == ["x", "y", "width", "height"],
                      let binding = fields["binding"] as? [String: Any],
                      Set(binding.keys) == NativeBindingFingerprint.keys,
                      let hit = control.hit, hit.valid, mailbox.offer(.diagnostic(hit)) else { state.close(); return }
            case "configure_authority":
                let expectedKeys: Set<String> = state.ownedInputJournal == nil
                    ? ["protocol_version", "session_epoch", "command", "clock_origin_ms", "surface", "clock_domain"]
                    : ["protocol_version", "session_epoch", "command", "clock_origin_ms", "surface", "clock_domain", "dependencies", "dependency_validity"]
                guard Set(fields.keys) == expectedKeys,
                      let raw = fields["surface"] as? [String: Any], Set(raw.keys) == ["id", "epoch"],
                      let origin = control.clock_origin_ms, let surface = control.surface, let domain = control.clock_domain,
                      state.configureAuthority(origin: origin, surface: surface, domain: domain,
                          dependencies: control.dependencies, dependencyValidity: control.dependency_validity) else { state.close(); return }
            case "dependency_update":
                guard state.ownedInputJournal != nil,
                      Set(fields.keys) == ["protocol_version", "session_epoch", "command", "name", "revision", "valid"],
                      let name = control.name, let revision = control.revision,
                      state.updateDependency(name: name, revision: revision, valid: control.valid) else { state.close(); return }
            case "revoke_authority":
                guard Set(fields.keys) == ["protocol_version", "session_epoch", "command", "permit"],
                      let raw = fields["permit"] as? [String: Any], Set(raw.keys) == ["id", "generation"],
                      let permit = control.permit, state.revokeAuthority(permit) else { state.close(); return }
            case "authority_probe", "atomic_submit":
                guard Set(fields.keys) == ["protocol_version", "session_epoch", "command", "probe"],
                      let raw = fields["probe"] as? [String: Any], strictProbeFields(raw), let probe = control.probe else { state.close(); return }
                let execute = control.command == "atomic_submit"
                let (ticket, outcome) = execute ? state.admitSubmission(probe) : state.admitProbe(probe)
                if let ticket {
                    guard mailbox.offer(execute ? .submit(ticket) : .probe(ticket)) else { state.close(); return }
                } else {
                    // Rejections still occupy the same single response mailbox.
                    guard mailbox.offer(execute ? .submitRejected(probe.request_id, outcome) : .probeRejected(probe.request_id, outcome)) else { state.close(); return }
                }
            case "owned_value_submit":
                guard state.ownedInputJournal != nil,
                      let ownedFields = strictOwnedValueControlFields(line),
                      let raw = ownedFields["owned_value"],
                      let requestData = try? JSONSerialization.data(withJSONObject: raw, options: [.sortedKeys, .withoutEscapingSlashes]),
                      let request = state.decodeOwnedValueRequest(requestData) else { state.close(); return }
                let (ticket, outcome, reason) = state.admitOwnedValue(request)
                if let ticket {
                    guard mailbox.offer(.ownedValue(ticket)) else { state.close(); return }
                } else {
                    guard mailbox.offer(.ownedValueRejected(request.request_id, outcome, reason)) else { state.close(); return }
                }
            case "shutdown": state.close()
            default: state.close()
            }
        }
    }
}

/// Create the legacy DispatchSource handler outside the executable entry's
/// implicit MainActor context. Only the Sendable lock-backed state is captured.
private func startWatchdog(_ state: GuardState) -> any DispatchSourceTimer {
    let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "cu.device.watchdog"))
    timer.schedule(deadline: .now(), repeating: .milliseconds(10))
    timer.setEventHandler {
        if state.expire() { state.stop("native guard worker or run loop exceeded hard bound"); _exit(124) }
        // The run-loop owner exits after its pending tap callback returns.
        // Only the independent hard watchdog forces an unresponsive exit.
    }
    timer.resume()
    return timer
}

private func startHardWatchdog(_ state: GuardState) -> any DispatchSourceTimer {
    let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "cu.device.hard-watchdog"))
    timer.schedule(deadline: .now(), repeating: .milliseconds(10))
    // Never acquire the ordered input lock here, including on the exit path.
    timer.setEventHandler { if state.hardExpired() { _exit(124) } }
    timer.resume(); return timer
}

/// The same default-stack serial worker serves live packets and the bounded
/// zero-input background regression. AX witnesses never leave this thread.
private func makeAXWorker(state: GuardState, mailbox: HitMailbox, epoch: UInt64) -> Thread {
    Thread {
        let keyWitnesses = KeyFocusWitnessStore()
        while !state.closing {
            keyWitnesses.prune { ticket in
                state.probeValidity(ticket) == nil &&
                    ticket.request.current_proof.focus_scope.map { DeviceProcessStamp.read(Int32($0.context.process.pid)) == $0.context.process.stamp } == true
            }
            guard let request = mailbox.take() else { usleep(2000); continue }
            switch request {
            case .diagnostic(let hit):
                let revision = state.beginQuery()
                var result = diagnosticHitTest(hit, epoch: epoch)
                if !state.endQuery(revision: revision) {
                    result.outcome = "stop_latched"; result.reason = "guard changed during hit test"; result.binding_digest = nil
                }
                mailbox.complete(.diagnostic(result))
            case .probeRejected(let id, let outcome):
                mailbox.complete(.probe(NativeProbeResponse(session_epoch: epoch, request_id: id,
                    outcome: outcome, reason: "native authority admission rejected"), nil))
            case .probe(let ticket):
                let revision = state.beginQuery()
                var keyWitness: KeyFocusWitness?
                var response = NativeProbeResponse(session_epoch: epoch, request_id: ticket.request.request_id,
                    outcome: "context_unresolved", reason: "native probe unavailable")
                if let invalid = state.probeValidity(ticket) { response.outcome = invalid }
                else if ticket.request.claimsBackgroundAX {
                    response = diagnosticBackgroundAX(ticket, state: state, epoch: epoch)
                }
                else if ticket.request.action == "key_press" {
                    response = diagnosticKeyFocus(ticket, epoch: epoch) { receiver, root in
                        keyWitness = KeyFocusWitness(ticket: ticket, epoch: epoch, receiver: receiver, root: root)
                    }
                }
                else if ticket.request.action == "ax_menu_press" {
                    let result = diagnosticMenuHitTest(ticket, epoch: epoch)
                    response.outcome = result.outcome == "matched" ? "eligible" : result.outcome
                    response.reason = result.reason; response.binding_digest = result.binding_digest
                }
                else if let hit = ticket.request.hit {
                    let anchor: NativeMenuOpenerAnchor?
                    if case .opener(let value, _) = ticket.request.current_proof.menu_scope { anchor = value } else { anchor = nil }
                    let result = diagnosticHitTest(hit, epoch: epoch, requireActive: true, deadlineMS: ticket.deadline, action: ticket.request.action, menuAnchor: anchor)
                    response.outcome = result.outcome == "matched" ? "eligible" : result.outcome
                    response.reason = result.reason; response.binding_digest = result.binding_digest
                }
                _ = state.endQuery(revision: revision)
                if let invalid = state.probeValidity(ticket) { response.outcome = invalid; response.binding_digest = nil }
                if ticket.request.action == "key_press", response.outcome == "eligible" {
                    guard let keyWitness, keyWitnesses.arm(keyWitness) else {
                        response.outcome = "rejected"; response.reason = "one outstanding focused binding witness required"; response.binding_digest = nil
                        mailbox.complete(.probe(response, ticket)); continue
                    }
                }
                mailbox.complete(.probe(response, ticket))
            case .submitRejected(let id, let outcome):
                mailbox.complete(.submit(NativeSubmitResponse(session_epoch: epoch, request_id: id,
                    outcome: outcome, reason: "atomic submission admission rejected")))
            case .submit(let ticket):
                let revision = state.beginQuery()
                let keyWitness = ticket.request.action == "key_press" ? keyWitnesses.take(ticket, epoch: epoch) : nil
                let response = atomicPress(ticket, state: state, keyWitness: keyWitness)
                _ = state.endQuery(revision: revision)
                mailbox.complete(.submit(response))
            case .ownedValueRejected(let id, let outcome, let reason):
                mailbox.complete(.ownedValue(NativeOwnedValueSubmitResponse(
                    sessionEpoch: epoch,
                    requestID: id,
                    outcome: outcome,
                    reason: reason
                )))
            case .ownedValue(let ticket):
                let revision = state.beginQuery()
                let response = atomicOwnedValueAction(ticket, state: state)
                _ = state.endQuery(revision: revision)
                mailbox.complete(.ownedValue(response))
            }
        }
        keyWitnesses.invalidate()
    }
}

/// An expired ticket, a false immutable startup fact and no binding witness
/// fence this regression before CGEvent construction, AX querying or posting.
func nativeKeyWorkerRejectsWithoutInput(_ request: NativeProbe) -> Bool {
    guard request.valid, request.action == "key_press" else { return false }
    let state = GuardState(epoch: request.current_proof.focus_scope?.context.session_epoch ?? 1, startupKeyboardClear: false)
    let mailbox = HitMailbox()
    guard mailbox.offer(.submit(NativeProbeTicket(request: request, deadline: 0))) else { return false }
    let worker = makeAXWorker(state: state, mailbox: mailbox, epoch: state.epoch)
    guard worker.stackSize <= 524288 else { return false }
    worker.start()
    defer { state.close() }
    let deadline = monotonicMS() + 1000
    while monotonicMS() < deadline {
        if let result = mailbox.response() {
            guard case .submit(let response) = result else { return false }
            return response.outcome == "context_unresolved" && !response.submitted &&
                response.committed_ms == nil && response.returned_ms == nil
        }
        usleep(1000)
    }
    return false
}

private func readExactStartupBytes(fd: Int32, length: Int) -> Data? {
    guard length > 0, length <= NativeOwnedDocumentRegistration.maximumBytes else { return nil }
    var data = Data(count: length)
    var offset = 0
    let complete = data.withUnsafeMutableBytes { bytes -> Bool in
        guard let base = bytes.baseAddress else { return false }
        while offset < length {
            let count = Darwin.read(fd, base.advanced(by: offset), length - offset)
            if count > 0 { offset += count; continue }
            if count < 0 && errno == EINTR { continue }
            return false
        }
        return true
    }
    return complete ? data : nil
}

/// Only the explicit owned startup mode consumes this private length prefix;
/// the same stdin then continues with the ordinary newline-delimited control.
private func readOwnedStartupRegistration() -> NativeOwnedDocumentRegistration? {
    guard let header = readExactStartupBytes(fd: STDIN_FILENO, length: 4) else { return nil }
    let length = header.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
    guard length > 0, length <= NativeOwnedDocumentRegistration.maximumBytes,
          let payload = readExactStartupBytes(fd: STDIN_FILENO, length: Int(length)) else { return nil }
    return try? NativeOwnedDocumentRegistration.decode(payload, nowUnixMS: unixMS())
}

@main
struct DeviceMain {
    // The entry owns the CFRunLoop. Background legacy callbacks are created by
    // nonisolated helpers rather than inheriting this entry's actor context.
    static func main() {
        #if CU_STOP_GATE_SELF_TEST
        // This separately built binary has no live control/input entry point.
        // Synthetic scope facts exist only in this zero-input test profile.
        guard CommandLine.arguments.count == 2,
              CommandLine.arguments[1] == "--self-test-stop-gate" else { exit(2) }
        do { boundedWrite(try runStopGateSelfTests()); return }
        catch { FileHandle.standardError.write(Data("stop gate self-test failed: \(error)\n".utf8)); exit(2) }
        #else
        if (2...3).contains(CommandLine.arguments.count), CommandLine.arguments[1] == "--self-test" {
            do {
                try ownedValueControlLineSelfTest()
                try ownedAuthorityBootstrapControlSelfTest()
                try ownedSaveSelectorSelfTest()
                try ownedSaveRequestDecoderSelfTest()
                try ownedUnknownGateSelfTest()
                try ownedSetterTimeoutSelfTest()
                try ownedReturnClockSelfTest()
                try ownedJournalRecordSelfTest()
                boundedWrite(try runDeviceTests(goldenPath: CommandLine.arguments.count == 3 ? CommandLine.arguments[2] : "contracts/desktop-v1/native-binding/golden.json")); return
            }
            catch { FileHandle.standardError.write(Data("device self-test failed: \(error)\n".utf8)); exit(2) }
        }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--self-test-key-wire" {
            do { boundedWrite(try runNativeKeyWireTests(path: CommandLine.arguments[2])); return }
            catch { FileHandle.standardError.write(Data("key wire self-test failed: \(error)\n".utf8)); exit(2) }
        }
        let args = CommandLine.arguments
        let ownedMode = (args.count == 10 || args.count == 11)
            && args[9] == "--execute-owned-value"
            && (args.count == 10 || args[10] == "--allow-owned-save")
        let ownedSaveMode = ownedMode && args.count == 11
        guard ([5, 9, 11, 15, 16].contains(args.count) || ownedMode), args[1] == "--epoch", args[3] == "--marker",
              let epoch = UInt64(args[2]), epoch > 0, let marker = Int64(args[4]), marker != 0 else { exit(2) }
        var executionScope: NativeExecutionScope?
        var ownedRegistration: NativeOwnedDocumentRegistration?
        var ownedJournal: NativeOwnedInputJournal?
        if ownedMode {
            guard args[5] == "--execute-pid", args[7] == "--execute-bundle",
                  let pid = Int32(args[6]), args[8] == NativeOwnedDocumentRegistration.textEditBundleID,
                  let scope = NativeExecutionScope(pid: pid, bundle: args[8]),
                  let registration = readOwnedStartupRegistration(),
                  registration.pid == UInt32(pid), registration.bundle_id == args[8],
                  let journal = NativeOwnedInputJournal(fd: STDERR_FILENO) else { exit(2) }
            executionScope = scope
            ownedRegistration = registration
            ownedJournal = journal
        } else if args.count >= 9 {
            guard args[5] == "--execute-pid", args[7] == "--execute-bundle", let pid = Int32(args[6]) else { exit(2) }
            let window: String?
            let expectedStamp: DeviceProcessStamp?
            let backgroundAX: Bool
            switch args.count {
            case 9:
                window = nil
                expectedStamp = nil
                backgroundAX = false
            case 11:
                guard args[9] == "--execute-window" else { exit(2) }
                window = args[10]
                expectedStamp = nil
                backgroundAX = false
            case 15:
                guard args[9] == "--execute-window",
                      case .expected(let parsedStamp) = NativeExecutionBirthStampArguments.parse(Array(args.dropFirst(11))) else { exit(2) }
                window = args[10]
                expectedStamp = parsedStamp
                backgroundAX = false
            case 16:
                guard args[9] == "--execute-window", args[15] == "--execute-background-ax",
                      let parsedStamp = NativeExecutionBirthStampArguments.parseBackgroundAX(Array(args[11..<16])) else { exit(2) }
                window = args[10]
                expectedStamp = parsedStamp
                backgroundAX = true
            default:
                exit(2)
            }
            guard let scope = NativeExecutionScope(pid: pid, bundle: args[8], window: window,
                                                   expectedStamp: expectedStamp,
                                                   backgroundAX: backgroundAX) else { exit(2) }
            executionScope = scope
        }
        _ = signal(SIGPIPE, SIG_IGN)
        guard fcntl(STDOUT_FILENO, F_SETFL, O_NONBLOCK) == 0 else { exit(2) }
        let state = GuardState(epoch: epoch, executionScope: executionScope, eventMarker: marker,
            startupMouseButtonsClear: mouseButtonsClear(), startupKeyboardClear: systemKeyboardClear(),
            ownedRegistration: ownedRegistration, ownedInputJournal: ownedJournal,
            allowOwnedSave: ownedSaveMode), mailbox = HitMailbox()
        state.loopAlive()
        let hardWatchdog = startHardWatchdog(state)
        defer { hardWatchdog.cancel() }
        Thread { readControl(state: state, mailbox: mailbox) }.start()
        makeAXWorker(state: state, mailbox: mailbox, epoch: epoch).start()
        // Independent of the AX worker and of the run loop that owns the tap.
        let watchdog = startWatchdog(state)
        defer { watchdog.cancel() }
        let context = TapContext(state: state, marker: marker)
        let eventTypes: [CGEventType] = [.keyDown, .keyUp, .flagsChanged, .leftMouseDown, .leftMouseUp,
            .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .mouseMoved,
            .leftMouseDragged, .rightMouseDragged, .otherMouseDragged, .scrollWheel]
        let mask = eventTypes.reduce(CGEventMask(0)) { $0 | (CGEventMask(1) << $1.rawValue) }
        let permission = CGPreflightListenEventAccess()
        let tap = permission ? CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap,
            options: .listenOnly, eventsOfInterest: mask, callback: eventCallback,
            userInfo: Unmanaged.passUnretained(context).toOpaque()) : nil
        let source = tap.flatMap { CFMachPortCreateRunLoopSource(kCFAllocatorDefault, $0, 0) }
        if let source { CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .defaultMode) }
        if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
        state.permissions(ax: AXIsProcessTrusted(), human: source != nil && tap != nil)
        // Disabled taps stay unavailable. Restart and explicit authorization are
        // required; an automatic tapEnable must not clear a loss of coverage.
        var nextPermissions = monotonicMS(), nextOutput: UInt64 = 0
        withExtendedLifetime(context) {
            while !state.closing {
                // An unavailable tap leaves no run-loop sources. That mode
                // returns immediately; do not spin a CPU while read-only.
                if CFRunLoopRunInMode(.defaultMode, 0.02, false) == .finished { usleep(20_000) }
                if state.inputClosed { state.loopAlive(); continue }
                if monotonicMS() >= nextPermissions {
                    let available = state.snapshot().human_input && CGPreflightListenEventAccess() &&
                        tap.map { CFMachPortIsValid($0) && CGEvent.tapIsEnabled(tap: $0) } == true
                    state.permissions(ax: AXIsProcessTrusted(), human: available)
                    nextPermissions = monotonicMS() + 100
                }
                if let result = mailbox.response() {
                    switch result {
                    case .diagnostic(let response): boundedWrite(response, state: state)
                    case .probe(var response, let ticket):
                        if let ticket, let invalid = state.probeValidity(ticket) { response.outcome = invalid; response.binding_digest = nil }
                        boundedWrite(response, state: state)
                    case .submit(let response): boundedWrite(response, state: state)
                    case .ownedValue(let response): boundedWrite(response, state: state)
                    }
                }
                if !state.inputClosed && monotonicMS() >= nextOutput {
                    boundedWrite(state.snapshot(), state: state); state.loopAlive(); nextOutput = monotonicMS() + 100
                }
            }
        }
        if let source { CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, .defaultMode) }
        if let tap { CFMachPortInvalidate(tap) }
        // A closed parent pipe cannot receive a final snapshot. Do not let an
        // EPIPE exit race an already-posted release's tap callback.
        if !state.inputClosed { boundedWrite(state.snapshot(), state: state) }
        // Threads may be in a system call; process exit is the final lifetime
        // bound. Held-class primitives remain unavailable. An already-started
        // atomic pointer pair has the bounded release drain above.
        exit(0)
        #endif
    }
}
