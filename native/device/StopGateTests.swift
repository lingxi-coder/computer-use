#if CU_STOP_GATE_SELF_TEST
import CoreGraphics
import ApplicationServices
import Darwin
import Foundation
import os

extension NativeExecutionScope {
    init(stopGateSelfTestPID pid: Int32, bundle: String) {
        self.pid = pid
        self.bundle = bundle
        self.window = nil
        self.stamp = DeviceProcessStamp(seconds: 1_700_000_000, micros: 123_456)
        self.backgroundAX = false
        self.ownedBackgroundAXDiagnostics = false
    }
}

struct StopGateSelfTestReport: Encodable, Sendable {
    let kind = "stop_gate_self_test"
    let passed: Int
    let mock_calls: Int
    let physical_inputs: Int
    let provider_calls: Int
    let native_stop_acceptance_verified: Bool
}

struct StopGateSelfTestFailure: Error, Sendable {
    let check: Int
    let sourceLine: UInt
    init(check: Int, sourceLine: UInt = #line) {
        self.check = check
        self.sourceLine = sourceLine
    }
}

private final class StopGateCell<Value: Sendable>: Sendable {
    private struct State: Sendable { var value: Value? }
    private let state = OSAllocatedUnfairLock(initialState: State())

    func store(_ value: Value) { state.withLock { $0.value = value } }
    func load() -> Value? { state.withLock { $0.value } }
}

private final class StopGateCounter: Sendable {
    private let count = OSAllocatedUnfairLock(initialState: 0)

    func increment() { count.withLock { $0 += 1 } }
    var value: Int { count.withLock { $0 } }
}

private final class StopGateRelease: Sendable {
    let semaphore = DispatchSemaphore(value: 0)
    private let state = OSAllocatedUnfairLock(initialState: false)

    func open() {
        let shouldSignal = state.withLock { released -> Bool in
            guard !released else { return false }
            released = true
            return true
        }
        if shouldSignal { semaphore.signal() }
    }
}

private struct StopGateAttempt: Sendable {
    let started = DispatchSemaphore(value: 0)
    let completed = DispatchSemaphore(value: 0)
    let response = StopGateCell<NativeSubmitResponse>()
}

private struct StopGateOperationResult: Sendable {
    let completed: Bool
    let elapsed_ms: UInt64
    let stop_revision: UInt64
    let hotkey_sequence: UInt64
    let human_sequence: UInt64
    let host_alive: Bool
    let lease_loss_sequence: UInt64
    let validity: String?
    let authority_clock_ready: Bool
}

private final class StopGateOperation: Sendable {
    let completed = DispatchSemaphore(value: 0)
    let result = StopGateCell<StopGateOperationResult>()
}

private struct StopGateFixture: Sendable {
    let state: GuardState
    let first: NativeProbeTicket
    let second: NativeProbeTicket
    let source_now_ms: UInt64
    let host_deadline: UInt64
    let query_started_uptime: UInt64
    let query_revision: UInt64
}

private struct StopGateOwnedFixture: Sendable {
    let state: GuardState
    let first: NativeOwnedValueTicket
    let second: NativeOwnedValueTicket
    let registration: NativeOwnedDocumentRegistration
    let proof_data: Data
    let source_now_ms: UInt64
    let journal_fd: Int32
    let journal_path: String

    func cleanup() {
        _ = Darwin.close(journal_fd)
        _ = Darwin.unlink(journal_path)
    }
}

private struct StopGateOwnedAttempt: Sendable {
    let started = DispatchSemaphore(value: 0)
    let completed = DispatchSemaphore(value: 0)
    let response = StopGateCell<NativeOwnedValueSubmitResponse>()
}

private func stopGateProbe(requestID: String, intentID: String, guardRevision: UInt64,
                           sourceNowMS: UInt64, permitID: String = "stop-gate-permit") throws -> NativeProbe {
    let frame = HitRect(CGRect(x: -11, y: 21, width: 40, height: 20))
    let fingerprint = NativeBindingFingerprint(
        app: "com.lingxi.computer-user.guard-fixture", app_pid: 4242,
        window_id: "window-1", ancestor_roles: [kAXWindowRole], group: nil,
        role: kAXButtonRole, subrole: nil, label: "stop gate target",
        sibling_index: 0, frame: frame.rounded, scroll_container: nil
    )
    let witness = try deviceContextWitnessJSON(fingerprint: fingerprint, displayFrame: frame)
    let fingerprintJSON = try JSONSerialization.jsonObject(with: fingerprint.canonical)
    let sourceProof: [String: Any] = [
        "binding": "stop-gate-binding",
        "instance": "stop-gate-instance",
        "snapshot_id": "stop-gate-snapshot",
        "environment_revision": 1,
        "layer_revision": 1,
        "fingerprint": fingerprintJSON,
        "display_frame": ["x": -11, "y": 21, "width": 40, "height": 20],
        "continuity": [
            "kind": "notification_covered",
            "source_sequence": 1,
            "context_digest": "bc1:" + String(repeating: "a", count: 64),
            "scope": ["kind": "whole_structure"]
        ]
    ]
    let body: [String: Any] = [
        "request_id": requestID,
        "intent_id": intentID,
        "task_id": "stop-gate-task",
        "task_revision": 1,
        "root_epoch": 1,
        "permit": ["id": permitID, "generation": 1],
        "stop_seq": 0,
        "guard_revision": guardRevision,
        "clock_domain": "macos/7/wall",
        "surface": ["id": "stop-gate-surface", "epoch": 7],
        "evidence_id": "stop-gate-evidence",
        "intent_evidence_id": "stop-gate-intent-evidence",
        "source_sequence": 1,
        "observed_ms": sourceNowMS - 1,
        "valid_until_ms": sourceNowMS + 1_400,
        "command_end_ms": sourceNowMS + 1_500,
        "started_ms": sourceNowMS,
        "duration_ms": 1_500,
        "latest_start_ms": sourceNowMS + 50,
        "original_proof": sourceProof,
        "current_proof": sourceProof,
        "context_witness": witness,
        "binding_digest": fingerprint.digest,
        "dependencies": ["focus": 1, "layer": 1, "binding": 1, "authorization": 1, "geometry": 1],
        "action": "ax_press",
        "x": -10,
        "y": 22
    ]
    let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys, .withoutEscapingSlashes])
    return try JSONDecoder().decode(NativeProbe.self, from: data)
}

private func makeStopGateFixture(hostLeaseMS: UInt64) throws -> StopGateFixture {
    let epoch: UInt64 = 7
    let surface = NativeSurface(id: "stop-gate-surface", epoch: epoch)
    let uptime = monotonicMS(), wall = unixMS(), sourceNow: UInt64 = 10_000
    guard wall > sourceNow else { throw StopGateSelfTestFailure(check: 1) }
    let origin = wall - sourceNow
    let state = GuardState(epoch: epoch, submissionScopeProfile: .simulated)
    state.permissions(ax: true, human: true)
    guard state.configureAuthority(origin: origin, surface: surface, domain: "macos/7/wall"),
          state.renew(lane: "host", duration: hostLeaseMS, now: uptime),
          state.renew(lane: "supervisor", duration: 1_400, now: uptime) else {
        throw StopGateSelfTestFailure(check: 1)
    }
    let firstRequest = try stopGateProbe(requestID: "stop-gate-request-1", intentID: "stop-gate-intent-1",
        guardRevision: 0, sourceNowMS: sourceNow)
    let secondRequest = try stopGateProbe(requestID: "stop-gate-request-2", intentID: "stop-gate-intent-2",
        guardRevision: 0, sourceNowMS: sourceNow)
    guard firstRequest.valid, secondRequest.valid else { throw StopGateSelfTestFailure(check: 1) }
    let first = state.admitProbe(firstRequest).0
    let second = state.admitProbe(secondRequest).0
    guard let first, let second else { throw StopGateSelfTestFailure(check: 1) }
    let queryRevision = state.beginQuery(now: uptime)
    return StopGateFixture(state: state, first: first, second: second, source_now_ms: sourceNow,
        host_deadline: uptime + hostLeaseMS, query_started_uptime: uptime, query_revision: queryRevision)
}

private func stopGateOwnedRequest(
    registration: NativeOwnedDocumentRegistration,
    proof: [String: Any],
    surface: NativeSurface,
    requestID: String,
    intentID: String,
    permitID: String,
    guardRevision: UInt64 = 0
) throws -> NativeOwnedValueRequest {
    let domain = "macos/7/wall"
    let body: [String: Any] = [
        "kind": "owned_value_submit",
        "request_id": requestID,
        "intent_id": intentID,
        "attempt_id": "owned-stop-gate-attempt-\(requestID)",
        "task_id": "owned-stop-gate-task",
        "task_revision": 1,
        "root_epoch": 1,
        "permit": ["id": permitID, "generation": 1],
        "stop_seq": 0,
        "guard_revision": guardRevision,
        "clock_domain": domain,
        "surface": ["id": surface.id, "epoch": surface.epoch],
        "evidence_id": "owned-stop-gate-evidence",
        "intent_evidence_id": "owned-stop-gate-intent-evidence",
        "source_sequence": 2,
        "observed_ms": 150,
        "valid_until_ms": 900,
        "command_end_ms": 1_000,
        "started_ms": 200,
        "duration_ms": 800,
        "latest_start_ms": 250,
        "original_proof": proof,
        "current_proof": proof,
        "dependencies": ["authorization": 1, "focus": 1, "window": 1, "owned_scope": 1],
        "action": ownedValueSetterAction,
        "value": "stop gate synthetic value"
    ]
    let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys, .withoutEscapingSlashes])
    guard let request = NativeOwnedValueRequest.decode(data, registration: registration,
              expectedSessionEpoch: 7, expectedSurface: surface, expectedClockDomain: domain,
              nowSourceMS: 200) else {
        throw StopGateSelfTestFailure(check: 1)
    }
    return request
}

private func makeStopGateOwnedFixture(hostLeaseMS: UInt64) throws -> StopGateOwnedFixture {
    let surface = NativeSurface(id: "main", epoch: 7)
    let domain = "macos/7/wall"
    let registration = try NativeOwnedDocumentRegistration.decodeForPureValidation(
        ownedValueFixtureRegistration(), nowUnixMS: 10_000)
    let proofData = try ownedValueFixtureProof(clockDomain: domain)
    guard let proof = try JSONSerialization.jsonObject(with: proofData) as? [String: Any] else {
        throw StopGateSelfTestFailure(check: 1)
    }

    var pathTemplate = Array("/private/tmp/cu-stop-gate-owned.XXXXXX".utf8CString)
    let journalFD = mkstemp(&pathTemplate)
    guard journalFD >= 0 else { throw StopGateSelfTestFailure(check: 1) }
    let journalPath = String(decoding: pathTemplate.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    var keepJournal = false
    defer {
        if !keepJournal {
            _ = Darwin.close(journalFD)
            _ = Darwin.unlink(journalPath)
        }
    }
    let originalFlags = fcntl(journalFD, F_GETFL)
    guard fchmod(journalFD, mode_t(0o600)) == 0,
          originalFlags >= 0,
          fcntl(journalFD, F_SETFL, originalFlags | O_APPEND) == 0,
          let journal = NativeOwnedInputJournal(fd: journalFD) else {
        throw StopGateSelfTestFailure(check: 1)
    }

    let dependencies: [String: UInt64] = [
        "authorization": 1, "focus": 1, "window": 1, "owned_scope": 1,
    ]
    let dependencyValidity: [String: Bool?] = [
        "authorization": true, "focus": true, "window": true, "owned_scope": true,
    ]
    let scope = NativeExecutionScope(stopGateSelfTestPID: Int32(registration.pid), bundle: registration.bundle_id)
    let state = GuardState(epoch: 7, executionScope: scope, ownedRegistration: registration,
        ownedInputJournal: journal)
    state.permissions(ax: true, human: true)
    let uptime = monotonicMS(), wall = unixMS(), sourceNow: UInt64 = 200
    guard wall > sourceNow,
          state.configureAuthority(origin: wall - sourceNow, surface: surface, domain: domain,
              dependencies: dependencies, dependencyValidity: dependencyValidity),
          state.renew(lane: "host", duration: hostLeaseMS, now: uptime),
          state.renew(lane: "supervisor", duration: 1_400, now: uptime) else {
        throw StopGateSelfTestFailure(check: 1)
    }
    let firstRequest = try stopGateOwnedRequest(registration: registration, proof: proof,
        surface: surface, requestID: "owned-stop-gate-request-1",
        intentID: "owned-stop-gate-intent-1", permitID: "owned-stop-gate-permit")
    let secondRequest = try stopGateOwnedRequest(registration: registration, proof: proof,
        surface: surface, requestID: "owned-stop-gate-request-2",
        intentID: "owned-stop-gate-intent-2", permitID: "owned-stop-gate-permit-2")
    guard firstRequest.validIdentity, secondRequest.validIdentity else {
        throw StopGateSelfTestFailure(check: 1)
    }
    let first = state.admitOwnedValue(firstRequest).0
    let second = state.admitOwnedValue(secondRequest).0
    guard let first, let second else { throw StopGateSelfTestFailure(check: 1) }
    guard journal.appendPrepared(first), journal.appendPrepared(second) else {
        throw StopGateSelfTestFailure(check: 1)
    }
    keepJournal = true
    return StopGateOwnedFixture(state: state, first: first, second: second,
        registration: registration, proof_data: proofData, source_now_ms: sourceNow,
        journal_fd: journalFD, journal_path: journalPath)
}

private func waitForStopGateSignal(_ semaphore: DispatchSemaphore, milliseconds: Int) -> Bool {
    semaphore.wait(timeout: .now() + .milliseconds(milliseconds)) == .success
}

private func startBlockedStopGateAttempt(
    state: GuardState,
    ticket: NativeProbeTicket,
    calls: StopGateCounter,
    release: StopGateRelease
) -> StopGateAttempt {
    let attempt = StopGateAttempt()
    DispatchQueue.global(qos: .userInitiated).async {
        let response = state.commitPress(ticket) {
            calls.increment()
            attempt.started.signal()
            return release.semaphore.wait(timeout: .now() + .seconds(2)) == .success
        }
        attempt.response.store(response)
        attempt.completed.signal()
    }
    return attempt
}

private func startCompetingStopGateAttempt(
    state: GuardState,
    ticket: NativeProbeTicket,
    calls: StopGateCounter
) -> StopGateAttempt {
    let attempt = StopGateAttempt()
    DispatchQueue.global(qos: .userInitiated).async {
        attempt.started.signal()
        let response = state.commitPress(ticket) {
            calls.increment()
            return true
        }
        attempt.response.store(response)
        attempt.completed.signal()
    }
    return attempt
}

private func startBlockedOwnedStopGateAttempt(
    state: GuardState,
    ticket: NativeOwnedValueTicket,
    calls: StopGateCounter,
    release: StopGateRelease,
    performResult: Bool = true
) -> StopGateOwnedAttempt {
    let attempt = StopGateOwnedAttempt()
    DispatchQueue.global(qos: .userInitiated).async {
        let response = state.commitOwnedValue(ticket) {
            calls.increment()
            attempt.started.signal()
            let released = release.semaphore.wait(timeout: .now() + .seconds(2)) == .success
            return released && performResult
        }
        attempt.response.store(response)
        attempt.completed.signal()
    }
    return attempt
}

private func startCompetingOwnedStopGateAttempt(
    state: GuardState,
    ticket: NativeOwnedValueTicket,
    calls: StopGateCounter
) -> StopGateOwnedAttempt {
    let attempt = StopGateOwnedAttempt()
    DispatchQueue.global(qos: .userInitiated).async {
        attempt.started.signal()
        let response = state.commitOwnedValue(ticket) {
            calls.increment()
            return true
        }
        attempt.response.store(response)
        attempt.completed.signal()
    }
    return attempt
}

private func startStopGateMutation(
    state: GuardState,
    action: InputNotice?,
    shouldStop: Bool,
    shouldClose: Bool,
    permit: NativePermit?
) -> StopGateOperation {
    let operation = StopGateOperation()
    DispatchQueue.global(qos: .userInitiated).async {
        let started = monotonicMS()
        if let action { state.notice(action) }
        if shouldStop { state.stop() }
        if shouldClose { state.close() }
        let permitRevoked = permit.map { state.revokeAuthority($0) }
        let snapshot = state.snapshot()
        let accepted: Bool
        if let permitRevoked { accepted = permitRevoked }
        else if shouldClose { accepted = state.inputClosed }
        else if shouldStop { accepted = snapshot.stop_revision > 0 }
        else if let action {
            switch action {
            case .stop: accepted = snapshot.hotkey_sequence > 0
            case .human: accepted = snapshot.human_sequence > 0
            case .ignored, .disabled: accepted = false
            }
        } else { accepted = false }
        let result = StopGateOperationResult(
            completed: accepted,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: nil,
            authority_clock_ready: snapshot.authority_clock_ready
        )
        operation.result.store(result)
        operation.completed.signal()
    }
    return operation
}

private func startStopGateObservation(state: GuardState, ticket: NativeProbeTicket,
                                     queryStartedUptime: UInt64) -> StopGateOperation {
    let operation = StopGateOperation()
    DispatchQueue.global(qos: .userInitiated).async {
        let started = monotonicMS()
        let watchdogExpired = state.hardExpired(now: queryStartedUptime + 500)
        let validity = state.probeValidity(ticket)
        let snapshot = state.snapshot()
        operation.result.store(StopGateOperationResult(
            completed: watchdogExpired && validity != nil && snapshot.stop_revision >= 4
                && snapshot.hotkey_sequence == 1 && snapshot.human_sequence == 1,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: validity,
            authority_clock_ready: snapshot.authority_clock_ready
        ))
        operation.completed.signal()
    }
    return operation
}

private func startOwnedStopGateStop(state: GuardState, ticket: NativeOwnedValueTicket) -> StopGateOperation {
    let operation = StopGateOperation()
    DispatchQueue.global(qos: .userInitiated).async {
        let started = monotonicMS()
        state.stop("owned stop gate self-test")
        let snapshot = state.snapshot()
        operation.result.store(StopGateOperationResult(
            completed: snapshot.stop_revision > ticket.request.guard_revision,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: nil,
            authority_clock_ready: snapshot.authority_clock_ready
        ))
        operation.completed.signal()
    }
    return operation
}

private func startOwnedClockSlip(state: GuardState) -> StopGateOperation {
    let operation = StopGateOperation()
    DispatchQueue.global(qos: .userInitiated).async {
        let started = monotonicMS()
        let watchdogExpired = state.expire(now: monotonicMS() + 200)
        let snapshot = state.snapshot()
        operation.result.store(StopGateOperationResult(
            completed: !watchdogExpired && !snapshot.authority_clock_ready,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: nil,
            authority_clock_ready: snapshot.authority_clock_ready
        ))
        operation.completed.signal()
    }
    return operation
}

func runStopGateSelfTests() throws -> StopGateSelfTestReport {
    var passed = 0
    func check(_ condition: @autoclosure () -> Bool, sourceLine: UInt = #line) throws {
        guard condition() else { throw StopGateSelfTestFailure(check: passed + 1, sourceLine: sourceLine) }
        passed += 1
    }

    let fixture = try makeStopGateFixture(hostLeaseMS: 400)
    let calls = StopGateCounter()
    let release = StopGateRelease()
    let firstAttempt = startBlockedStopGateAttempt(state: fixture.state, ticket: fixture.first,
        calls: calls, release: release)
    var operations: [StopGateOperation] = []
    var competingAttempt: StopGateAttempt?
    defer {
        release.open()
        if firstAttempt.response.load() == nil { _ = waitForStopGateSignal(firstAttempt.completed, milliseconds: 2_500) }
        if let competingAttempt, competingAttempt.response.load() == nil {
            _ = waitForStopGateSignal(competingAttempt.completed, milliseconds: 1_000)
        }
        for operation in operations where operation.result.load() == nil {
            _ = waitForStopGateSignal(operation.completed, milliseconds: 1_000)
        }
    }

    try check(waitForStopGateSignal(firstAttempt.started, milliseconds: 1_000))
    let firstCompetingAttempt = startCompetingStopGateAttempt(state: fixture.state, ticket: fixture.second, calls: calls)
    competingAttempt = firstCompetingAttempt
    try check(waitForStopGateSignal(firstCompetingAttempt.started, milliseconds: 1_000))
    try check(waitForStopGateSignal(firstCompetingAttempt.completed, milliseconds: 150))
    guard let busyResponse = firstCompetingAttempt.response.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(!busyResponse.submitted && busyResponse.outcome == "rejected")
    try check(calls.value == 1)

    let mutations: [(InputNotice?, Bool, Bool, NativePermit?)] = [
        (nil, true, false, nil),
        (.stop, false, false, nil),
        (.human, false, false, nil),
        (nil, false, true, nil),
        (nil, false, false, fixture.first.request.permit)
    ]
    for (notice, stop, close, permit) in mutations {
        let operation = startStopGateMutation(state: fixture.state, action: notice,
            shouldStop: stop, shouldClose: close, permit: permit)
        operations.append(operation)
        try check(waitForStopGateSignal(operation.completed, milliseconds: 150))
        guard let result = operation.result.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
        try check(result.completed && result.elapsed_ms <= 150)
    }

    let leaseExpiry = StopGateOperation()
    operations.append(leaseExpiry)
    DispatchQueue.global(qos: .utility).async {
        while monotonicMS() < fixture.host_deadline { Thread.sleep(forTimeInterval: 0.001) }
        let started = monotonicMS()
        _ = fixture.state.expire()
        let snapshot = fixture.state.snapshot()
        leaseExpiry.result.store(StopGateOperationResult(
            completed: !snapshot.host_alive && snapshot.lease_loss_sequence > 0,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: nil,
            authority_clock_ready: snapshot.authority_clock_ready
        ))
        leaseExpiry.completed.signal()
    }

    let observation = startStopGateObservation(state: fixture.state, ticket: fixture.first,
        queryStartedUptime: fixture.query_started_uptime)
    operations.append(observation)
    try check(waitForStopGateSignal(observation.completed, milliseconds: 150))
    guard let concurrentState = observation.result.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(concurrentState.completed && concurrentState.elapsed_ms <= 150)
    try check(concurrentState.validity != nil && concurrentState.stop_revision >= 4)
    try check(concurrentState.hotkey_sequence == 1 && concurrentState.human_sequence == 1)
    let now = monotonicMS()
    let remainingLeaseMS = fixture.host_deadline > now ? fixture.host_deadline - now : 0
    let untilLeaseDeadline = Int(remainingLeaseMS) + 150
    try check(waitForStopGateSignal(leaseExpiry.completed, milliseconds: untilLeaseDeadline))
    guard let leaseResult = leaseExpiry.result.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(leaseResult.completed && leaseResult.elapsed_ms <= 150)

    let revisionBeforeReturn = leaseResult.stop_revision
    release.open()
    try check(waitForStopGateSignal(firstAttempt.completed, milliseconds: 1_000))
    guard let firstResponse = firstAttempt.response.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(firstResponse.submitted && ["applied", "unknown"].contains(firstResponse.outcome))
    try check(fixture.state.snapshot().stop_revision == revisionBeforeReturn)
    let oldFirst = fixture.state.commitPress(fixture.first) { calls.increment(); return true }
    let oldSecond = fixture.state.commitPress(fixture.second) { calls.increment(); return true }
    try check(!oldFirst.submitted && !oldSecond.submitted)
    try check(calls.value == 1)
    try check(!fixture.state.endQuery(revision: fixture.query_revision))

    let revisionFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let oldTicket = revisionFixture.first
    revisionFixture.state.stop()
    let nextRevision = revisionFixture.state.snapshot().stop_revision
    let restampedSameIntent = try stopGateProbe(requestID: oldTicket.request.request_id,
        intentID: oldTicket.request.intent_id, guardRevision: nextRevision,
        sourceNowMS: revisionFixture.source_now_ms)
    try check(restampedSameIntent.valid)
    let revisionAdmission = revisionFixture.state.admitProbe(restampedSameIntent)
    try check(revisionAdmission.0 == nil && revisionAdmission.1 == "rejected")
    let revisionCalls = StopGateCounter()
    let oldRevisionReplay = revisionFixture.state.commitPress(oldTicket) { revisionCalls.increment(); return true }
    try check(!oldRevisionReplay.submitted && revisionCalls.value == 0)

    let finalBoundaryFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let finalBoundaryCalls = StopGateCounter()
    let finalBoundaryRejected = finalBoundaryFixture.state.commitPress(finalBoundaryFixture.first,
        deadlineMS: finalBoundaryFixture.first.deadline,
        preCommitRevalidate: { "window child boundary changed" }) {
            finalBoundaryCalls.increment(); return true
        }
    try check(!finalBoundaryRejected.submitted && finalBoundaryRejected.outcome == "context_unresolved"
        && finalBoundaryCalls.value == 0)
    let finalBoundaryRetry = finalBoundaryFixture.state.commitPress(finalBoundaryFixture.first,
        deadlineMS: finalBoundaryFixture.first.deadline,
        preCommitRevalidate: { nil }) {
            finalBoundaryCalls.increment(); return true
        }
    try check(finalBoundaryRetry.submitted && finalBoundaryRetry.outcome == "applied"
        && finalBoundaryCalls.value == 1)

    let deadlineFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let deadlineCalls = StopGateCounter()
    let expiredGate = deadlineFixture.state.commitPress(deadlineFixture.first,
        deadlineMS: monotonicMS() - 1, preCommitRevalidate: { nil }) {
            deadlineCalls.increment(); return true
        }
    try check(!expiredGate.submitted && expiredGate.outcome == "expired" && deadlineCalls.value == 0)
    let unconsumedDeadlineRetry = deadlineFixture.state.commitPress(deadlineFixture.first,
        deadlineMS: deadlineFixture.first.deadline, preCommitRevalidate: { nil }) {
            deadlineCalls.increment(); return true
        }
    try check(unconsumedDeadlineRetry.submitted && unconsumedDeadlineRetry.outcome == "applied"
        && deadlineCalls.value == 1)

    let finalStopFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let finalStopCalls = StopGateCounter()
    let finalStopRelease = StopGateRelease()
    let finalStopAttempt = StopGateAttempt()
    defer {
        finalStopRelease.open()
        if finalStopAttempt.response.load() == nil {
            _ = waitForStopGateSignal(finalStopAttempt.completed, milliseconds: 2_500)
        }
    }
    DispatchQueue.global(qos: .userInitiated).async {
        let response = finalStopFixture.state.commitPress(finalStopFixture.first,
            deadlineMS: finalStopFixture.first.deadline, preCommitRevalidate: {
                finalStopAttempt.started.signal()
                guard finalStopRelease.semaphore.wait(timeout: .now() + .seconds(2)) == .success else {
                    return "test final revalidation timed out"
                }
                return nil
            }) {
                finalStopCalls.increment(); return true
            }
        finalStopAttempt.response.store(response)
        finalStopAttempt.completed.signal()
    }
    try check(waitForStopGateSignal(finalStopAttempt.started, milliseconds: 1_000))
    let stopFinished = DispatchSemaphore(value: 0)
    let stoppedRevision = StopGateCell<UInt64>()
    DispatchQueue.global(qos: .userInitiated).async {
        finalStopFixture.state.stop("stop during final revalidation")
        stoppedRevision.store(finalStopFixture.state.snapshot().stop_revision)
        stopFinished.signal()
    }
    try check(waitForStopGateSignal(stopFinished, milliseconds: 150))
    finalStopRelease.open()
    try check(waitForStopGateSignal(finalStopAttempt.completed, milliseconds: 1_000))
    guard let finalStopResponse = finalStopAttempt.response.load() else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    try check(!finalStopResponse.submitted && finalStopResponse.outcome == "rejected"
        && finalStopCalls.value == 0)
    try check((stoppedRevision.load() ?? 0) > finalStopFixture.first.request.guard_revision)

    let lateCallFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let lateCallCount = StopGateCounter()
    let inputDeadline = monotonicMS() + 40
    let unknownAfterDeadline = lateCallFixture.state.commitPress(lateCallFixture.first,
        deadlineMS: inputDeadline, preCommitRevalidate: { nil }) {
            lateCallCount.increment()
            while monotonicMS() <= inputDeadline { Thread.sleep(forTimeInterval: 0.001) }
            return false
        }
    try check(unknownAfterDeadline.submitted && unknownAfterDeadline.outcome == "unknown"
        && lateCallCount.value == 1)
    try check(lateCallFixture.state.snapshot().stop_revision > lateCallFixture.first.request.guard_revision)

    let busyFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let busyCalls = StopGateCounter()
    let busyRelease = StopGateRelease()
    let busyFirst = startBlockedStopGateAttempt(state: busyFixture.state, ticket: busyFixture.first,
        calls: busyCalls, release: busyRelease)
    var busySecond: StopGateAttempt?
    defer {
        busyRelease.open()
        if busyFirst.response.load() == nil { _ = waitForStopGateSignal(busyFirst.completed, milliseconds: 2_500) }
        if let busySecond, busySecond.response.load() == nil {
            _ = waitForStopGateSignal(busySecond.completed, milliseconds: 1_000)
        }
    }
    try check(waitForStopGateSignal(busyFirst.started, milliseconds: 1_000))
    let competingRetry = startCompetingStopGateAttempt(state: busyFixture.state, ticket: busyFixture.second,
        calls: busyCalls)
    busySecond = competingRetry
    try check(waitForStopGateSignal(competingRetry.completed, milliseconds: 150))
    guard let secondWhileBusy = competingRetry.response.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(!secondWhileBusy.submitted && secondWhileBusy.outcome == "rejected" && busyCalls.value == 1)
    busyRelease.open()
    try check(waitForStopGateSignal(busyFirst.completed, milliseconds: 1_000))
    let secondRetry = busyFixture.state.commitPress(busyFixture.second) { busyCalls.increment(); return true }
    try check(secondRetry.submitted && secondRetry.outcome == "applied" && busyCalls.value == 2)

    let unknownFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let unknownCalls = StopGateCounter()
    let unknownResponse = unknownFixture.state.commitPress(unknownFixture.first) {
        unknownCalls.increment()
        return false
    }
    try check(unknownResponse.submitted && unknownResponse.outcome == "unknown" && unknownCalls.value == 1)
    let unknownSnapshot = unknownFixture.state.snapshot()
    try check(unknownSnapshot.stop_revision > unknownFixture.first.request.guard_revision)
    let freshPermitRequest = try stopGateProbe(requestID: "after-unknown-request",
        intentID: "after-unknown-intent", guardRevision: unknownSnapshot.stop_revision,
        sourceNowMS: unknownFixture.source_now_ms, permitID: "after-unknown-permit")
    try check(freshPermitRequest.valid)
    let afterUnknownAdmission = unknownFixture.state.admitSubmission(freshPermitRequest)
    try check(afterUnknownAdmission.0 == nil && afterUnknownAdmission.1 == "stop_latched")
    let unadmittedFreshTicket = NativeProbeTicket(request: freshPermitRequest, deadline: monotonicMS() + 1_000)
    let freshPermitAttempt = unknownFixture.state.commitPress(unadmittedFreshTicket) {
        unknownCalls.increment()
        return true
    }
    try check(!freshPermitAttempt.submitted && unknownCalls.value == 1)
    let unknownOldReplay = unknownFixture.state.commitPress(unknownFixture.first) {
        unknownCalls.increment()
        return true
    }
    try check(!unknownOldReplay.submitted && unknownCalls.value == 1)

    // Only the simulated uptime is advanced here; no wall clock is changed.
    // The failed anchor must remain sticky through the blocked call's return.
    let clockFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let clockCalls = StopGateCounter()
    let clockRelease = StopGateRelease()
    let clockAttempt = startBlockedStopGateAttempt(state: clockFixture.state,
        ticket: clockFixture.first, calls: clockCalls, release: clockRelease)
    let clockFault = StopGateOperation()
    defer {
        clockRelease.open()
        if clockAttempt.response.load() == nil { _ = waitForStopGateSignal(clockAttempt.completed, milliseconds: 2_500) }
        if clockFault.result.load() == nil { _ = waitForStopGateSignal(clockFault.completed, milliseconds: 1_000) }
    }
    try check(waitForStopGateSignal(clockAttempt.started, milliseconds: 1_000))
    DispatchQueue.global(qos: .userInitiated).async {
        let started = monotonicMS()
        let watchdogExpired = clockFixture.state.expire(now: monotonicMS() + 200)
        let snapshot = clockFixture.state.snapshot()
        clockFault.result.store(StopGateOperationResult(
            completed: !watchdogExpired && !snapshot.authority_clock_ready,
            elapsed_ms: monotonicMS() - started,
            stop_revision: snapshot.stop_revision,
            hotkey_sequence: snapshot.hotkey_sequence,
            human_sequence: snapshot.human_sequence,
            host_alive: snapshot.host_alive,
            lease_loss_sequence: snapshot.lease_loss_sequence,
            validity: nil,
            authority_clock_ready: snapshot.authority_clock_ready
        ))
        clockFault.completed.signal()
    }
    try check(waitForStopGateSignal(clockFault.completed, milliseconds: 150))
    guard let clockFaultState = clockFault.result.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(clockFaultState.completed && clockFaultState.elapsed_ms <= 150)
    try check(!clockFaultState.authority_clock_ready)
    clockRelease.open()
    try check(waitForStopGateSignal(clockAttempt.completed, milliseconds: 1_000))
    guard let clockResponse = clockAttempt.response.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(clockResponse.submitted && clockResponse.outcome == "unknown")
    try check(clockResponse.committed_ms != nil && clockResponse.returned_ms == nil)
    try check(!clockFixture.state.snapshot().authority_clock_ready)
    let clockReplay = clockFixture.state.commitPress(clockFixture.first) {
        clockCalls.increment()
        return true
    }
    try check(!clockReplay.submitted && clockCalls.value == 1)

    let receiptUnknownFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let receiptUnknownCalls = StopGateCounter()
    let receiptApplied = receiptUnknownFixture.state.commitPress(receiptUnknownFixture.first) {
        receiptUnknownCalls.increment()
        return true
    }
    try check(receiptApplied.submitted && receiptApplied.outcome == "applied")
    var receiptMutatedToUnknown = receiptApplied
    receiptMutatedToUnknown.outcome = "unknown"
    receiptMutatedToUnknown.reason = "synthetic post-commit acknowledgement uncertainty"
    let finalizedUnknown = receiptUnknownFixture.state.finalizeInputReceipt(
        receiptUnknownFixture.first, response: receiptMutatedToUnknown)
    try check(finalizedUnknown.submitted && finalizedUnknown.outcome == "unknown")
    try check(finalizedUnknown.committed_ms != nil && finalizedUnknown.returned_ms != nil)
    let beforeRepeatedUnknown = receiptUnknownFixture.state.snapshot()
    let repeatedUnknown = receiptUnknownFixture.state.finalizeInputReceipt(
        receiptUnknownFixture.first, response: finalizedUnknown)
    let afterRepeatedUnknown = receiptUnknownFixture.state.snapshot()
    try check(repeatedUnknown.submitted && repeatedUnknown.outcome == "unknown")
    try check(afterRepeatedUnknown.stop_revision == beforeRepeatedUnknown.stop_revision
        && afterRepeatedUnknown.fault_sequence == beforeRepeatedUnknown.fault_sequence)
    let receiptUnknownSnapshot = receiptUnknownFixture.state.snapshot()
    try check(receiptUnknownSnapshot.stop_revision > receiptUnknownFixture.first.request.guard_revision)
    let afterReceiptUnknownRequest = try stopGateProbe(requestID: "after-receipt-unknown-request",
        intentID: "after-receipt-unknown-intent", guardRevision: receiptUnknownSnapshot.stop_revision,
        sourceNowMS: receiptUnknownFixture.source_now_ms, permitID: "after-receipt-unknown-permit")
    let afterReceiptUnknownAdmission = receiptUnknownFixture.state.admitSubmission(afterReceiptUnknownRequest)
    try check(afterReceiptUnknownAdmission.0 == nil && afterReceiptUnknownAdmission.1 == "stop_latched")
    let afterReceiptUnknownTicket = NativeProbeTicket(request: afterReceiptUnknownRequest,
        deadline: monotonicMS() + 1_000)
    let newPermitAfterReceiptUnknown = receiptUnknownFixture.state.commitPress(afterReceiptUnknownTicket) {
        receiptUnknownCalls.increment()
        return true
    }
    let oldTicketAfterReceiptUnknown = receiptUnknownFixture.state.commitPress(receiptUnknownFixture.first) {
        receiptUnknownCalls.increment()
        return true
    }
    try check(!newPermitAfterReceiptUnknown.submitted && !oldTicketAfterReceiptUnknown.submitted)
    try check(receiptUnknownCalls.value == 1)

    let receiptClockFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let receiptClockCalls = StopGateCounter()
    let receiptClockApplied = receiptClockFixture.state.commitPress(receiptClockFixture.first) {
        receiptClockCalls.increment()
        return true
    }
    try check(receiptClockApplied.submitted && receiptClockApplied.outcome == "applied")
    try check(receiptClockApplied.committed_ms != nil && receiptClockApplied.returned_ms != nil)
    _ = receiptClockFixture.state.expire(now: monotonicMS() + 200)
    let finalizedClockFault = receiptClockFixture.state.finalizeInputReceipt(
        receiptClockFixture.first, response: receiptClockApplied)
    try check(finalizedClockFault.submitted && finalizedClockFault.outcome == "unknown")
    try check(finalizedClockFault.committed_ms != nil && finalizedClockFault.returned_ms == nil)
    let receiptClockSnapshot = receiptClockFixture.state.snapshot()
    try check(!receiptClockSnapshot.authority_clock_ready)
    let afterReceiptClockRequest = try stopGateProbe(requestID: "after-receipt-clock-request",
        intentID: "after-receipt-clock-intent", guardRevision: receiptClockSnapshot.stop_revision,
        sourceNowMS: receiptClockFixture.source_now_ms, permitID: "after-receipt-clock-permit")
    let afterReceiptClockAdmission = receiptClockFixture.state.admitSubmission(afterReceiptClockRequest)
    try check(afterReceiptClockAdmission.0 == nil && afterReceiptClockAdmission.1 == "stop_latched")
    let afterReceiptClockTicket = NativeProbeTicket(request: afterReceiptClockRequest,
        deadline: monotonicMS() + 1_000)
    let oldTicketAfterReceiptClock = receiptClockFixture.state.commitPress(receiptClockFixture.first) {
        receiptClockCalls.increment()
        return true
    }
    let newPermitAfterReceiptClock = receiptClockFixture.state.commitPress(afterReceiptClockTicket) {
        receiptClockCalls.increment()
        return true
    }
    try check(!oldTicketAfterReceiptClock.submitted && !newPermitAfterReceiptClock.submitted)
    try check(receiptClockCalls.value == 1)

    let receiptAppliedFixture = try makeStopGateFixture(hostLeaseMS: 1_400)
    let receiptAppliedCalls = StopGateCounter()
    let receiptAppliedResponse = receiptAppliedFixture.state.commitPress(receiptAppliedFixture.first) {
        receiptAppliedCalls.increment()
        return true
    }
    let finalizedApplied = receiptAppliedFixture.state.finalizeInputReceipt(
        receiptAppliedFixture.first, response: receiptAppliedResponse)
    try check(finalizedApplied.submitted && finalizedApplied.outcome == "applied")
    if let committed = finalizedApplied.committed_ms, let returned = finalizedApplied.returned_ms {
        try check(returned >= committed)
    } else {
        try check(false)
    }
    let afterAppliedRequest = try stopGateProbe(requestID: "after-finalized-applied-request",
        intentID: "after-finalized-applied-intent",
        guardRevision: receiptAppliedFixture.state.snapshot().stop_revision,
        sourceNowMS: receiptAppliedFixture.source_now_ms, permitID: "after-finalized-applied-permit")
    let afterAppliedAdmission = receiptAppliedFixture.state.admitSubmission(afterAppliedRequest)
    try check(afterAppliedAdmission.0 != nil)
    try check(receiptAppliedCalls.value == 1)

    let ownedStopFixture = try makeStopGateOwnedFixture(hostLeaseMS: 1_400)
    defer { ownedStopFixture.cleanup() }
    let ownedStopCalls = StopGateCounter()
    let ownedStopRelease = StopGateRelease()
    let ownedFirstAttempt = startBlockedOwnedStopGateAttempt(state: ownedStopFixture.state,
        ticket: ownedStopFixture.first, calls: ownedStopCalls, release: ownedStopRelease)
    var ownedStopOperation: StopGateOperation?
    defer {
        ownedStopRelease.open()
        if ownedFirstAttempt.response.load() == nil {
            _ = waitForStopGateSignal(ownedFirstAttempt.completed, milliseconds: 2_500)
        }
        if let ownedStopOperation, ownedStopOperation.result.load() == nil {
            _ = waitForStopGateSignal(ownedStopOperation.completed, milliseconds: 1_000)
        }
    }
    try check(waitForStopGateSignal(ownedFirstAttempt.started, milliseconds: 1_000))
    let stopOperation = startOwnedStopGateStop(state: ownedStopFixture.state, ticket: ownedStopFixture.first)
    ownedStopOperation = stopOperation
    try check(waitForStopGateSignal(stopOperation.completed, milliseconds: 150))
    guard let stopState = stopOperation.result.load() else { throw StopGateSelfTestFailure(check: passed + 1) }
    try check(stopState.completed && stopState.elapsed_ms <= 150)
    ownedStopRelease.open()
    try check(waitForStopGateSignal(ownedFirstAttempt.completed, milliseconds: 1_000))
    guard let ownedStopResponse = ownedFirstAttempt.response.load() else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    try check(ownedStopResponse.submitted && ownedStopResponse.outcome == "applied")
    let ownedStopReplay = ownedStopFixture.state.commitOwnedValue(ownedStopFixture.first) {
        ownedStopCalls.increment()
        return true
    }
    let ownedStaleSecond = ownedStopFixture.state.commitOwnedValue(ownedStopFixture.second) {
        ownedStopCalls.increment()
        return true
    }
    try check(!ownedStopReplay.submitted && !ownedStaleSecond.submitted && ownedStopCalls.value == 1)

    let ownedBusyFixture = try makeStopGateOwnedFixture(hostLeaseMS: 1_400)
    defer { ownedBusyFixture.cleanup() }
    let ownedBusyCalls = StopGateCounter()
    let ownedBusyRelease = StopGateRelease()
    let ownedBusyFirst = startBlockedOwnedStopGateAttempt(state: ownedBusyFixture.state,
        ticket: ownedBusyFixture.first, calls: ownedBusyCalls, release: ownedBusyRelease)
    var ownedBusySecond: StopGateOwnedAttempt?
    defer {
        ownedBusyRelease.open()
        if ownedBusyFirst.response.load() == nil {
            _ = waitForStopGateSignal(ownedBusyFirst.completed, milliseconds: 2_500)
        }
        if let ownedBusySecond, ownedBusySecond.response.load() == nil {
            _ = waitForStopGateSignal(ownedBusySecond.completed, milliseconds: 1_000)
        }
    }
    try check(waitForStopGateSignal(ownedBusyFirst.started, milliseconds: 1_000))
    let busyOwnedAttempt = startCompetingOwnedStopGateAttempt(state: ownedBusyFixture.state,
        ticket: ownedBusyFixture.second, calls: ownedBusyCalls)
    ownedBusySecond = busyOwnedAttempt
    try check(waitForStopGateSignal(busyOwnedAttempt.completed, milliseconds: 150))
    guard let busyOwnedResponse = busyOwnedAttempt.response.load() else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    try check(!busyOwnedResponse.submitted && busyOwnedResponse.outcome == "rejected"
        && ownedBusyCalls.value == 1)
    ownedBusyRelease.open()
    try check(waitForStopGateSignal(ownedBusyFirst.completed, milliseconds: 1_000))
    let ownedSecondRetry = ownedBusyFixture.state.commitOwnedValue(ownedBusyFixture.second) {
        ownedBusyCalls.increment()
        return true
    }
    try check(ownedSecondRetry.submitted && ownedSecondRetry.outcome == "applied"
        && ownedBusyCalls.value == 2)

    let ownedUnknownFixture = try makeStopGateOwnedFixture(hostLeaseMS: 1_400)
    defer { ownedUnknownFixture.cleanup() }
    let ownedUnknownCalls = StopGateCounter()
    let ownedUnknownResponse = ownedUnknownFixture.state.commitOwnedValue(ownedUnknownFixture.first) {
        ownedUnknownCalls.increment()
        return false
    }
    try check(ownedUnknownResponse.submitted && ownedUnknownResponse.outcome == "unknown")
    let ownedUnknownSnapshot = ownedUnknownFixture.state.snapshot()
    try check(ownedUnknownSnapshot.stop_revision > ownedUnknownFixture.first.request.guard_revision)
    guard let ownedUnknownProof = try JSONSerialization.jsonObject(with: ownedUnknownFixture.proof_data) as? [String: Any] else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    let freshOwnedRequest = try stopGateOwnedRequest(registration: ownedUnknownFixture.registration,
        proof: ownedUnknownProof,
        surface: ownedUnknownFixture.first.request.surface, requestID: "owned-after-unknown-request",
        intentID: "owned-after-unknown-intent", permitID: "owned-after-unknown-permit",
        guardRevision: ownedUnknownSnapshot.stop_revision)
    let freshOwnedAdmission = ownedUnknownFixture.state.admitOwnedValue(freshOwnedRequest)
    try check(freshOwnedAdmission.0 == nil && freshOwnedAdmission.1 == "stop_latched")
    let freshOwnedTicket = NativeOwnedValueTicket(request: freshOwnedRequest, deadline: monotonicMS() + 1_000)
    let freshOwnedReplay = ownedUnknownFixture.state.commitOwnedValue(freshOwnedTicket) {
        ownedUnknownCalls.increment()
        return true
    }
    let ownedUnknownReplay = ownedUnknownFixture.state.commitOwnedValue(ownedUnknownFixture.first) {
        ownedUnknownCalls.increment()
        return true
    }
    try check(!freshOwnedReplay.submitted && !ownedUnknownReplay.submitted && ownedUnknownCalls.value == 1)

    let ownedClockFixture = try makeStopGateOwnedFixture(hostLeaseMS: 1_400)
    defer { ownedClockFixture.cleanup() }
    let ownedClockCalls = StopGateCounter()
    let ownedClockRelease = StopGateRelease()
    let ownedClockAttempt = startBlockedOwnedStopGateAttempt(state: ownedClockFixture.state,
        ticket: ownedClockFixture.first, calls: ownedClockCalls, release: ownedClockRelease)
    var ownedClockSlip: StopGateOperation?
    defer {
        ownedClockRelease.open()
        if ownedClockAttempt.response.load() == nil {
            _ = waitForStopGateSignal(ownedClockAttempt.completed, milliseconds: 2_500)
        }
        if let ownedClockSlip, ownedClockSlip.result.load() == nil {
            _ = waitForStopGateSignal(ownedClockSlip.completed, milliseconds: 1_000)
        }
    }
    try check(waitForStopGateSignal(ownedClockAttempt.started, milliseconds: 1_000))
    let clockSlip = startOwnedClockSlip(state: ownedClockFixture.state)
    ownedClockSlip = clockSlip
    try check(waitForStopGateSignal(clockSlip.completed, milliseconds: 150))
    guard let ownedClockState = clockSlip.result.load() else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    try check(ownedClockState.completed && !ownedClockState.authority_clock_ready
        && ownedClockState.elapsed_ms <= 150)
    ownedClockRelease.open()
    try check(waitForStopGateSignal(ownedClockAttempt.completed, milliseconds: 1_000))
    guard let ownedClockResponse = ownedClockAttempt.response.load() else {
        throw StopGateSelfTestFailure(check: passed + 1)
    }
    try check(ownedClockResponse.submitted && ownedClockResponse.outcome == "unknown")
    try check(ownedClockResponse.committed_ms != nil && ownedClockResponse.returned_ms == nil)
    try check(!ownedClockFixture.state.snapshot().authority_clock_ready)
    let ownedClockReplay = ownedClockFixture.state.commitOwnedValue(ownedClockFixture.first) {
        ownedClockCalls.increment()
        return true
    }
    try check(!ownedClockReplay.submitted && ownedClockCalls.value == 1)

    return StopGateSelfTestReport(passed: passed,
        mock_calls: calls.value + revisionCalls.value + busyCalls.value + unknownCalls.value + clockCalls.value
            + receiptUnknownCalls.value + receiptClockCalls.value + receiptAppliedCalls.value
            + ownedStopCalls.value + ownedBusyCalls.value + ownedUnknownCalls.value + ownedClockCalls.value,
        physical_inputs: 0, provider_calls: 0, native_stop_acceptance_verified: false)
}
#endif
