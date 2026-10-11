import ApplicationServices
import Carbon
import Darwin
import Dispatch
import Foundation
import Security

#if CU_OWNED_KEY_DEATH_POINTS
/// Counts actual calls to CGEvent.post for one constructed pair. This
/// reference is owned by AtomicKeyPair, so a second post() call cannot reset
/// the counters. The key submit worker is the only mutator/snapshotter.
fileprivate final class OwnedKeyPostCounts {
    private(set) var down: UInt64 = 0
    private(set) var up: UInt64 = 0

    func returnedFromDownPost() {
        if down < UInt64.max { down += 1 }
    }

    func returnedFromUpPost() {
        if up < UInt64.max { up += 1 }
    }
}

enum OwnedKeyDeathPoint {
    static func pause(_ phase: String, pair: AtomicKeyPair) {
        guard AtomicKeyProfile.acceptanceEnabled,
              URL(fileURLWithPath: CommandLine.arguments[0]).lastPathComponent == "owned-key-" + phase else { return }
        let controller = getppid()
        let diagnostic: [String: Any] = ["kind": "owned_key_death_point", "phase": phase,
            "helper_pid": getpid(), "actor_pid": getpid(), "controller_pid": controller,
            "nonce": String(pair.nonce), "key_code": Int(pair.code),
            "down_post_calls": pair.postCounts.down, "up_post_calls": pair.postCounts.up,
            "down_timestamp_ns": String(pair.down.timestamp), "up_timestamp_ns": String(pair.up.timestamp)]
        guard var data = try? JSONSerialization.data(withJSONObject: diagnostic), data.count <= 1024 else { _exit(126) }
        data.append(10)
        let count = data.withUnsafeBytes { Darwin.write(STDERR_FILENO, $0.baseAddress!, $0.count) }
        guard count == data.count else { _exit(126) }
        let end = monotonicMS() + 250
        while monotonicMS() < end {
            if getppid() != controller { return }
            usleep(1000)
        }
        // An uninjected timeout fails acceptance; it is never a successful
        // physical death experiment or a reason to synthesize another up.
        _exit(126)
    }
}
#endif

/// Construction does not advertise production availability. The separately
/// compiled owned helper exists to validate delivery and partial-post death;
/// production remains closed until that release lifecycle is verified.
enum AtomicKeyProfile {
    static let wireID = "macos_atomic_special_key_v1"
    static let id = "macos-atomic-special-key@1"
    static let receiverRoles: Set<String> = [kAXCheckBoxRole]
    static let releaseAckMS: UInt64 = 150
    static var acceptanceEnabled: Bool {
        #if CU_OWNED_KEY_ACCEPTANCE
        true
        #else
        false
        #endif
    }
    static func keyCode(_ name: String) -> CGKeyCode? {
        // The public canonical vocabulary is broader. Only this actual
        // nontext receiver operation enters the initial native profile.
        name == "Space" ? CGKeyCode(kVK_Space) : nil
    }
    static func confirmationDeadline(original: UInt64, posted: UInt64) -> UInt64? {
        let (end, overflow) = posted.addingReportingOverflow(releaseAckMS)
        return overflow ? nil : min(original, end)
    }
}

struct NativeKey: Decodable, Sendable, Equatable {
    let name: String
    let modifiers: NativeMouseModifiers
    var valid: Bool { AtomicKeyProfile.keyCode(name) != nil && modifiers.flags.isEmpty }
}

/// A tap receipt is correlated with a per-pair random value. It is only an
/// event tag; GuardState still validates the complete down/up receipt.
func makeAtomicKeyPairNonce(sessionMarker: Int64) -> Int64? {
    var bits: UInt64 = 0
    guard SecRandomCopyBytes(kSecRandomDefault, MemoryLayout<UInt64>.size, &bits) == errSecSuccess else { return nil }
    let nonce = Int64(bits & UInt64(Int64.max))
    guard nonce > 0, nonce != sessionMarker else { return nil }
    return nonce
}

func systemKeyboardClear() -> Bool {
    // Check every conventional Mac virtual keycode, not merely the requested
    // key. Synthetic-session state and actual HID state are independent.
    for state: CGEventSourceStateID in [.combinedSessionState, .hidSystemState] {
        let flags = CGEventSource.flagsState(state)
        let forbidden: CGEventFlags = [.maskShift, .maskControl, .maskAlternate, .maskCommand, .maskSecondaryFn, .maskHelp, .maskAlphaShift]
        guard flags.intersection(forbidden).isEmpty else { return false }
        for code in UInt16(0)...UInt16(127) {
            if CGEventSource.keyState(state, key: code) { return false }
        }
    }
    return true
}

func atomicKeyConfirmed(observed: Bool, clear: Bool, coverage: Bool, began: UInt64, finished: UInt64, cutoff: UInt64) -> Bool {
    observed && clear && coverage && began <= finished && finished < cutoff
}

func atomicKeyCommitTimeAllowed(now: UInt64, deadline: UInt64, hostDeadline: UInt64, supervisorDeadline: UInt64,
                                wall: UInt64, origin: UInt64, validUntil: UInt64) -> Bool {
    now < deadline && now < hostDeadline && now < supervisorDeadline && wall >= origin && wall - origin < validUntil
}

struct AtomicKeyTimestamps: Equatable {
    let down: UInt64
    let up: UInt64
}

func atomicKeyTimestamps(down: UInt64, upCandidate: UInt64) -> AtomicKeyTimestamps? {
    guard down > 0, upCandidate > down else { return nil }
    return AtomicKeyTimestamps(down: down, up: upCandidate)
}

struct AtomicKeyPair {
    let source: CGEventSource
    let down: CGEvent, up: CGEvent
    let code: CGKeyCode
    let nonce: Int64
    #if CU_OWNED_KEY_DEATH_POINTS
    fileprivate let postCounts = OwnedKeyPostCounts()
    #endif
    init?(key: NativeKey, marker: Int64) {
        guard key.valid, marker != 0, let code = AtomicKeyProfile.keyCode(key.name),
              let nonce = makeAtomicKeyPairNonce(sessionMarker: marker),
              let source = CGEventSource(stateID: .privateState) else { return nil }
        source.userData = nonce; source.localEventsSuppressionInterval = 0
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false) else { return nil }
        for event in [down, up] {
            event.flags = key.modifiers.flags
            event.setIntegerValueField(.eventSourceUserData, value: nonce)
            event.setIntegerValueField(.keyboardEventAutorepeat, value: 0)
        }
        self.source = source; self.down = down; self.up = up; self.code = code; self.nonce = nonce
    }
    /// Assign both timestamps after focus validation and before authority
    /// commit. The up uses a later real monotonic reading, never a guessed
    /// clock quantum. Failure therefore occurs before either event is posted.
    func setTimestamps(base: UInt64) -> Bool {
        guard let timestamps = atomicKeyTimestamps(down: base,
                upCandidate: DispatchTime.now().uptimeNanoseconds) else { return false }
        down.timestamp = timestamps.down
        up.timestamp = timestamps.up
        return down.timestamp == timestamps.down && up.timestamp == timestamps.up &&
            up.timestamp > down.timestamp
    }
    func post() {
        withExtendedLifetime(source) {
            down.post(tap: .cghidEventTap)
            #if CU_OWNED_KEY_DEATH_POINTS
            postCounts.returnedFromDownPost()
            OwnedKeyDeathPoint.pause("between-down-up", pair: self)
            #endif
            // No delay, modifier transition, second gate or cancellation
            // check can separate this already-committed down/up pair.
            up.post(tap: .cghidEventTap)
            #if CU_OWNED_KEY_DEATH_POINTS
            postCounts.returnedFromUpPost()
            OwnedKeyDeathPoint.pause("after-up", pair: self)
            #endif
        }
    }
}

func atomicKey(_ ticket: NativeProbeTicket, state: GuardState, witness: KeyFocusWitness?) -> NativeSubmitResponse {
    let unavailable = "native focused key prerequisites unavailable"
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: unavailable)
    guard AtomicKeyProfile.acceptanceEnabled, ticket.request.action == "key_press", ticket.request.valid,
          let witness, witness.epoch == state.epoch, witness.ticket == ticket,
          state.startupKeyboardClear, CGPreflightPostEventAccess(), mouseButtonsClear(), systemKeyboardClear(),
          let key = ticket.request.key, let pair = AtomicKeyPair(key: key, marker: state.eventMarker) else { return response }
    if let invalid = state.probeValidity(ticket) { response.outcome = invalid; return response }
    guard let stamp = state.beginKeyDelivery(proposedStamp: DispatchTime.now().uptimeNanoseconds,
        code: pair.code, flags: pair.up.flags.rawValue, nonce: pair.nonce) else { return response }
    var posted: UInt64?
    let focus = diagnosticKeyFocus(ticket, epoch: state.epoch, witness: witness) { _, _ in
        #if CU_OWNED_KEY_DEATH_POINTS
        OwnedKeyDeathPoint.pause("before-down", pair: pair)
        #endif
        guard pair.setTimestamps(base: stamp) else { return }
        response = state.commitPress(ticket) {
            let began = monotonicMS()
            posted = began
            state.keyPostsStarted(stamp: stamp, nonce: pair.nonce,
                originalDeadline: ticket.deadline, now: began)
            pair.post()
            return true
        }
    }
    guard response.submitted else {
        state.cancelUnpostedKey(stamp: stamp)
        if response.reason == unavailable { response.outcome = focus.outcome == "eligible" ? "context_unresolved" : focus.outcome; response.reason = focus.reason }
        return response
    }
    #if CU_OWNED_KEY_ACCEPTANCE
    // This record is emitted once by the owned-fixture helper after the
    // confirmation gate completes. It is out-of-band and cannot affect the
    // submission response or delivery decision.
    defer {
        if let facts = state.takeOwnedKeyDeliveryFactsForDiagnostic() {
            writeOwnedKeyDeliveryDiagnostic(facts)
        }
    }
    #endif
    var confirmed = false
    var confirmedReturned: UInt64?
    #if CU_OWNED_KEY_DEATH_POINTS
    OwnedKeyDeathPoint.pause("waiting-ack", pair: pair)
    #endif
    if response.outcome == "applied", let posted,
       let cutoff = AtomicKeyProfile.confirmationDeadline(original: ticket.deadline, posted: posted) {
        while monotonicMS() < cutoff {
            let began = monotonicMS()
            let observed = state.keyUpObserved(stamp: stamp)
            let clear = observed && systemKeyboardClear() && mouseButtonsClear()
            // Final receipt, lease coverage and clock validation are checked
            // together under GuardState's single confirmation linearization.
            let returned = observed && clear ? state.confirmedKeyReturnMS(ticket, cutoff: cutoff) : nil
            let coverage = returned != nil
            let finished = monotonicMS()
            #if CU_OWNED_KEY_ACCEPTANCE
            state.recordOwnedKeyConfirmation(OwnedKeyConfirmationFact(observed: observed,
                clear_checked: observed, clear: clear, coverage: coverage, returned_ms: returned,
                began_ms: began, finished_ms: finished, cutoff_ms: cutoff), stamp: stamp)
            #endif
            if let returned, atomicKeyConfirmed(observed: observed, clear: clear, coverage: coverage, began: began, finished: finished, cutoff: cutoff) {
                confirmed = true; confirmedReturned = returned; break
            }
            if finished >= cutoff { break }
            usleep(1000)
        }
    }
    // A late ACK may drain the tap, but it can never rewrite this decision.
    if confirmed, let returned = confirmedReturned, let committed = response.committed_ms {
        guard returned >= committed else {
            response.outcome = "unknown"; response.reason = "key confirmation clock predates posting; do not replay"
            let finalized = state.finalizeInputReceipt(ticket, response: response)
            state.isolateInputAfterKeyUnknown(stamp: stamp)
            return finalized
        }
        response.returned_ms = returned
        response.reason = "matching key release observed and system input clear; UI effect unverified"
        state.finishKeyDelivery(stamp: stamp)
    } else {
        response.outcome = "unknown"
        response.reason = "key release, system state or original deadline not confirmed; do not replay"
    }
    let finalized = state.finalizeInputReceipt(ticket, response: response)
    if finalized.outcome == "unknown" { state.isolateInputAfterKeyUnknown(stamp: stamp) }
    return finalized
}

enum AtomicKeyConstructorFailure: Error { case failed(Int) }
func runAtomicKeyConstructorTests() throws -> Int {
    var passed = 0
    func check(_ value: Bool) throws { guard value else { throw AtomicKeyConstructorFailure.failed(passed + 1) }; passed += 1 }
    let noModifiers = NativeMouseModifiers(shift: false, control: false, option: false, command: false)
    let key = NativeKey(name: "Space", modifiers: noModifiers)
    guard let pair = AtomicKeyPair(key: key, marker: 123) else { throw AtomicKeyConstructorFailure.failed(1) }
    try check(pair.nonce > 0 && pair.nonce != 123)
    try check(pair.code == 49 && pair.down.type == .keyDown && pair.up.type == .keyUp)
    try check(atomicKeyTimestamps(down: 0, upCandidate: 1) == nil)
    try check(atomicKeyTimestamps(down: 12345, upCandidate: 12345) == nil)
    try check(atomicKeyTimestamps(down: 12345, upCandidate: 12344) == nil)
    try check(atomicKeyTimestamps(down: UInt64.max, upCandidate: UInt64.max) == nil)
    try check(atomicKeyTimestamps(down: 12345, upCandidate: 12346) == AtomicKeyTimestamps(down: 12345, up: 12346))
    try check(pair.setTimestamps(base: 12345))
    try check(pair.down.timestamp == 12345 && pair.up.timestamp > pair.down.timestamp)
    for event in [pair.down, pair.up] {
        try check(event.getIntegerValueField(.keyboardEventKeycode) == 49)
        try check(event.getIntegerValueField(.keyboardEventAutorepeat) == 0)
        try check(event.getIntegerValueField(.eventSourceUserData) == pair.nonce)
        try check(event.flags.isEmpty)
    }
    let savedUpTimestamp = pair.up.timestamp
    try check(!pair.setTimestamps(base: UInt64.max))
    try check(pair.up.timestamp == savedUpTimestamp)
    try check(pair.source.localEventsSuppressionInterval == 0)
#if CU_OWNED_KEY_DEATH_POINTS
    let originalCounts = pair.postCounts
    try check(originalCounts === pair.postCounts)
    originalCounts.returnedFromDownPost()
    originalCounts.returnedFromUpPost()
    originalCounts.returnedFromDownPost()
    try check(pair.postCounts.down == 2 && pair.postCounts.up == 1)
#endif
    try check(AtomicKeyPair(key: key, marker: 0) == nil)
    for name in ["space", " ", "A", "49", "Shift", "Tab", "Return", "Escape", "ArrowDown"] { try check(AtomicKeyProfile.keyCode(name) == nil) }
    for modifiers in [NativeMouseModifiers(shift: true, control: false, option: false, command: false),
        NativeMouseModifiers(shift: false, control: true, option: false, command: false),
        NativeMouseModifiers(shift: false, control: false, option: true, command: false),
        NativeMouseModifiers(shift: false, control: false, option: false, command: true)] {
        try check(AtomicKeyPair(key: NativeKey(name: "Space", modifiers: modifiers), marker: 123) == nil)
    }
    try check(AtomicKeyProfile.confirmationDeadline(original: 200, posted: 100) == 200)
    try check(AtomicKeyProfile.confirmationDeadline(original: 400, posted: 100) == 250)
    try check(AtomicKeyProfile.confirmationDeadline(original: UInt64.max, posted: UInt64.max - 100) == nil)
    try check(atomicKeyConfirmed(observed: true, clear: true, coverage: true, began: 198, finished: 199, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: true, clear: true, coverage: true, began: 199, finished: 200, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: true, clear: true, coverage: true, began: 200, finished: 201, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: true, clear: true, coverage: true, began: 199, finished: 198, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: false, clear: true, coverage: true, began: 198, finished: 199, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: true, clear: false, coverage: true, began: 198, finished: 199, cutoff: 200))
    try check(!atomicKeyConfirmed(observed: true, clear: true, coverage: false, began: 198, finished: 199, cutoff: 200))
    try check(atomicKeyCommitTimeAllowed(now: 199, deadline: 200, hostDeadline: 300, supervisorDeadline: 300, wall: 1199, origin: 1000, validUntil: 200))
    try check(!atomicKeyCommitTimeAllowed(now: 200, deadline: 200, hostDeadline: 300, supervisorDeadline: 300, wall: 1199, origin: 1000, validUntil: 200))
    try check(!atomicKeyCommitTimeAllowed(now: 199, deadline: 200, hostDeadline: 199, supervisorDeadline: 300, wall: 1199, origin: 1000, validUntil: 200))
    try check(!atomicKeyCommitTimeAllowed(now: 199, deadline: 200, hostDeadline: 300, supervisorDeadline: 199, wall: 1199, origin: 1000, validUntil: 200))
    try check(!atomicKeyCommitTimeAllowed(now: 199, deadline: 200, hostDeadline: 300, supervisorDeadline: 300, wall: 1200, origin: 1000, validUntil: 200))
    try check(!atomicKeyCommitTimeAllowed(now: 199, deadline: 200, hostDeadline: 300, supervisorDeadline: 300, wall: 999, origin: 1000, validUntil: 200))
    return passed
}
