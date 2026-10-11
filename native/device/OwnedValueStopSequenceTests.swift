import Foundation

func runOwnedValueStopSequenceTests() throws -> Int {
    var passed = 0
    func check(_ condition: @autoclosure () -> Bool) throws {
        guard condition() else { throw DeviceTestFailure(check: passed + 1) }
        passed += 1
    }
    let surface = NativeSurface(id: "main", epoch: 7)
    let domain = "macos/7/wall"
    let registration = try NativeOwnedDocumentRegistration.decodeForPureValidation(
        ownedValueFixtureRegistration(), nowUnixMS: 10_000)
    let proof = try JSONSerialization.jsonObject(
        with: ownedValueFixtureProof(clockDomain: domain))
    func request(_ changes: [String: Any] = [:], nowSourceMS: UInt64 = 200) throws -> NativeOwnedValueRequest? {
        let body: [String: Any] = [
            "kind": "owned_value_submit", "request_id": "owned-zero-request",
            "intent_id": "owned-zero-intent", "attempt_id": "owned-zero-attempt",
            "task_id": "owned-zero-root", "task_revision": 1, "root_epoch": 1,
            "permit": ["id": "owned-zero-permit", "generation": 1],
            "stop_seq": 0, "guard_revision": 0, "clock_domain": domain,
            "surface": ["id": surface.id, "epoch": surface.epoch],
            "evidence_id": "owned-zero-evidence", "intent_evidence_id": "owned-zero-evidence",
            "source_sequence": 2, "observed_ms": 150, "valid_until_ms": 350,
            "command_end_ms": 400, "started_ms": 200, "duration_ms": 200,
            "latest_start_ms": 250, "original_proof": proof, "current_proof": proof,
            "dependencies": ["authorization": 1, "focus": 1, "window": 1, "owned_scope": 1],
            "action": "ax_set_owned_document_value", "value": "zero-stop fixture",
        ]
        return NativeOwnedValueRequest.decode(
            try ownedValueFixtureData(body.merging(changes) { _, new in new }),
            registration: registration, expectedSessionEpoch: 7,
            expectedSurface: surface, expectedClockDomain: domain, nowSourceMS: nowSourceMS)
    }
    guard let zero = try request(),
          let clock = NativeAuthorityClock(origin: 1_000, surface: surface,
              domain: domain, epoch: 7, wall: 1_200, uptime: 500) else {
        throw DeviceTestFailure(check: passed + 1)
    }
    try check(zero.stop_seq == 0 && zero.guard_revision == 0)
    let negative = try request(["stop_seq": -1])
    try check(negative == nil)
    var ledger = NativeAuthorityLedger()
    try check(ledger.configure(clock))
    let admitted = ledger.admitOwnedValue(zero, now: 500, wall: 1_200)
    try check(admitted.0 != nil && admitted.1 == "eligible")
    guard let ticket = admitted.0 else { throw DeviceTestFailure(check: passed + 1) }
    try check(ledger.ownedValueValidity(ticket, now: 501, wall: 1_201) == nil)
    try check(ledger.revoke(zero.permit))
    try check(ledger.commitOwnedValue(ticket, now: 502, wall: 1_202) == "revoked")

    var raisedFloor = NativeAuthorityLedger()
    try check(raisedFloor.configure(clock))
    guard let one = try request(["stop_seq": 1]) else { throw DeviceTestFailure(check: passed + 1) }
    try check(raisedFloor.admitOwnedValue(one, now: 500, wall: 1_200).0 != nil)
    guard let stale = try request([
        "request_id": "owned-stale-request", "intent_id": "owned-stale-intent",
        "permit": ["id": "owned-stale-permit", "generation": 1],
    ]) else { throw DeviceTestFailure(check: passed + 1) }
    let rejected = raisedFloor.admitOwnedValue(stale, now: 501, wall: 1_201)
    try check(rejected.0 == nil && rejected.1 == "stop_latched")

    guard let later = try request([
        "started_ms": 400, "latest_start_ms": 450, "command_end_ms": 600, "valid_until_ms": 550,
    ], nowSourceMS: 400) else { throw DeviceTestFailure(check: passed + 1) }
    var advancing = NativeAuthorityLedger()
    try check(advancing.configure(clock))
    try check(advancing.admitOwnedValue(later, now: 700, wall: 1_400).0 != nil)
    var future = NativeAuthorityLedger()
    try check(future.configure(clock))
    try check(future.admitOwnedValue(later, now: 650, wall: 1_350).1 == "expired")
    var expired = NativeAuthorityLedger()
    try check(expired.configure(clock))
    try check(expired.admitOwnedValue(later, now: 850, wall: 1_550).1 == "expired")

    // Local Stop advances the guard revision rather than the ledger floor.
    let state = GuardState(epoch: 7)
    let before = state.snapshot(now: 500)
    state.stop("owned zero-stop fixture")
    let after = state.snapshot(now: 501)
    try check(after.stop_revision == before.stop_revision + 1
        && zero.guard_revision != after.stop_revision)
    let revisions: [String: UInt64] = ["authorization": 1, "focus": 1, "window": 1, "owned_scope": 1]
    let valid: [String: Bool?] = ["authorization": true, "focus": true, "window": true, "owned_scope": true]
    try check(ownedDependencyBootstrapValidity(revisions: revisions, validity: valid)?.count == 4)
    var uncertain = valid
    uncertain.updateValue(nil, forKey: "focus")
    uncertain["window"] = false
    let mapped = ownedDependencyBootstrapValidity(revisions: revisions, validity: uncertain)
    try check(mapped?["focus"] == nil && mapped?["window"] == false)
    try check(ownedDependencyBootstrapValidity(revisions: revisions, validity: [:]) == nil)
    var invalidRevisions = revisions
    invalidRevisions["owned_scope"] = 0
    try check(ownedDependencyBootstrapValidity(revisions: invalidRevisions, validity: valid) == nil)
    return passed
}
