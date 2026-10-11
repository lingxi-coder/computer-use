import CoreGraphics
import Foundation
import os

#if CU_OWNED_KEY_ACCEPTANCE
struct OwnedKeyTapFact: Sendable {
    let type: UInt32, stamp: UInt64, flags: UInt64
    let code: Int64, autorepeat: Int64
    let observed_ms: UInt64
}
struct OwnedKeyConfirmationFact: Sendable {
    let observed: Bool, clear_checked: Bool, clear: Bool, coverage: Bool
    let returned_ms: UInt64?
    let began_ms: UInt64, finished_ms: UInt64, cutoff_ms: UInt64
}
struct OwnedKeyDeliveryFacts: Sendable {
    let expected_stamp: UInt64, expected_flags: UInt64
    let expected_code: UInt16
    let expected_down_type: UInt32, expected_up_type: UInt32
    let expected_autorepeat: Int64
    let posted_ms: UInt64?, drain_until_ms: UInt64
    let up_observed: Bool
    let first: OwnedKeyTapFact?, second: OwnedKeyTapFact?
    let confirmation: OwnedKeyConfirmationFact?
}
#endif

enum InputNotice: Sendable, Equatable { case ignored, human, stop, disabled }

/// No characters, mouse positions or key codes leave the callback. Movement
/// uses an accumulated threshold, so a series of small moves is not lost.
struct InputClassifier {
    let marker: Int64
    var anchor: CGPoint?
    mutating func classify(type: CGEventType, marker eventMarker: Int64,
                           key: Int64, flags: CGEventFlags, point: CGPoint) -> InputNotice {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput { return .disabled }
        if eventMarker == marker { return .ignored }
        if type == .keyDown, key == 53, flags.contains(.maskShift),
           flags.contains(.maskCommand) || flags.contains(.maskControl) { return .stop }
        if type == .mouseMoved {
            guard point.x.isFinite, point.y.isFinite else { return .disabled }
            guard let previous = anchor else { anchor = point; return .ignored }
            if hypot(point.x - previous.x, point.y - previous.y) < 3 { return .ignored }
            anchor = point
        }
        return .human
    }
}

struct GuardSnapshot: Codable, Sendable, Equatable {
    let protocol_version: Int
    let kind: String
    let session_epoch: UInt64
    let sequence: UInt64
    let uptime_ms: UInt64
    let accessibility: Bool
    let human_input: Bool
    let host_alive: Bool
    let supervisor_alive: Bool
    let stop_revision: UInt64
    let human_sequence: UInt64
    let hotkey_sequence: UInt64
    let lease_loss_sequence: UInt64
    let fault_sequence: UInt64
    let reason: String
    /// The production facade remains closed until Core/backend integration.
    let input_available: Bool
    let authority_clock_ready: Bool
    let startup_mouse_buttons_clear: Bool
    let startup_keyboard_clear: Bool
    let keyboard_input_available: Bool
}

func monotonicMS() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

func inputReturnSourceMSIfTrusted(
    clock: NativeAuthorityClock,
    committedWallMS: UInt64,
    committedUptimeMS: UInt64,
    returnedWallMS: UInt64,
    returnedUptimeMS: UInt64,
    validUntilMS: UInt64
) -> UInt64? {
    let (committedSourceMS, commitUnderflow) = committedWallMS.subtractingReportingOverflow(clock.origin)
    let (returnedSourceMS, returnUnderflow) = returnedWallMS.subtractingReportingOverflow(clock.origin)
    let returnLimit = validUntilMS > UInt64.max - 500 ? UInt64.max : validUntilMS + 500
    guard !commitUnderflow, !returnUnderflow,
          committedUptimeMS >= clock.uptime,
          returnedWallMS >= committedWallMS,
          returnedUptimeMS >= committedUptimeMS,
          clock.healthy(wall: committedWallMS, uptime: committedUptimeMS),
          clock.healthy(wall: returnedWallMS, uptime: returnedUptimeMS),
          returnedSourceMS >= committedSourceMS,
          returnedSourceMS <= returnLimit else { return nil }
    return returnedSourceMS
}

func ownedValueSetterOutcome(callSucceeded: Bool, returnedSourceMS: UInt64?) -> String {
    callSucceeded && returnedSourceMS != nil ? "applied" : "unknown"
}

func keyReceiptVisibleAcknowledgement(_ acknowledged: Bool, droppingReceipt: Bool) -> Bool {
    acknowledged && !droppingReceipt
}

func keyReceiptDrainPending(until: UInt64, upObserved: Bool, now: UInt64) -> Bool {
    until > now && !upObserved
}

func keyReceiptFinalizationAllowed(receiptAcknowledged: Bool, closing: Bool, isolated: Bool,
                                   accessibility: Bool, humanInput: Bool,
                                   ticketRevision: UInt64, stopRevision: UInt64,
                                   now: UInt64, cutoff: UInt64, ticketDeadline: UInt64,
                                   hostDeadline: UInt64, supervisorDeadline: UInt64) -> Bool {
    receiptAcknowledged && !closing && !isolated && accessibility && humanInput &&
        ticketRevision == stopRevision && now < cutoff && now < ticketDeadline &&
        now < hostDeadline && now < supervisorDeadline
}

/// Fixed-size receipt matcher for the private owned-key acceptance lane.
/// Unknown event flag bits are retained as equality evidence; only named SDK
/// flags are interpreted, and this profile expects all of them to be clear.
struct KeyReceiptState: Sendable {
    enum Phase: Sendable, Equatable { case idle, awaitingDown, awaitingUp, acknowledged, poisoned, drainOnly }
    struct Event: Sendable, Equatable {
        let type: UInt32, stamp: UInt64, flags: UInt64
        let code: Int64, autorepeat: Int64, receivedMS: UInt64
    }
    private struct Active: Sendable {
        let deliveryStamp: UInt64, nonce: Int64, code: UInt16, expectedNamedFlags: UInt64
        var phase: Phase = .awaitingDown
        var postedAt: UInt64?
        var acknowledgementCutoff: UInt64 = 0
        var drainUntil: UInt64 = 0
        var down: Event?
        var up: Event?
        var lastReceivedMS: UInt64?
        var drainOnlyUpConsumed = false
    }
    private struct Retired: Sendable {
        let nonce: Int64, code: Int64, stamp: UInt64, flags: UInt64, receivedAt: UInt64, expiresAt: UInt64
        var available = true
    }
    private(set) var deliveryStamp: UInt64 = 0
    private var active: Active?
    private var retired: Retired?

    private static let namedFlagsMask: UInt64 = {
        let flags: CGEventFlags = [.maskAlphaShift, .maskShift, .maskControl, .maskAlternate,
            .maskCommand, .maskNumericPad, .maskHelp, .maskSecondaryFn, .maskNonCoalesced]
        return flags.rawValue
    }()

    var phase: Phase { active?.phase ?? .idle }
    var postedAt: UInt64? { active?.postedAt }
    var acknowledgementCutoff: UInt64 { active?.acknowledgementCutoff ?? 0 }
    var drainUntil: UInt64 { active?.drainUntil ?? 0 }
    var currentNonce: Int64? { active?.nonce }
    var downEvent: Event? { active?.down }
    var upEvent: Event? { active?.up }
    var isAcknowledged: Bool { active?.phase == .acknowledged }
    var hasActiveReceipt: Bool { active != nil }

    mutating func begin(proposedStamp: UInt64, code: UInt16, flags: UInt64, nonce: Int64,
                        eventMarker: Int64) -> UInt64? {
        guard proposedStamp > 0, active == nil, deliveryStamp < UInt64.max,
              nonce > 0, nonce != eventMarker, nonce != active?.nonce, nonce != retired?.nonce,
              flags & Self.namedFlagsMask == 0 else { return nil }
        let next = max(proposedStamp, deliveryStamp + 1)
        deliveryStamp = next
        active = Active(deliveryStamp: next, nonce: nonce, code: code, expectedNamedFlags: flags & Self.namedFlagsMask)
        return next
    }

    @discardableResult
    mutating func postsStarted(stamp: UInt64, nonce: Int64, originalDeadline: UInt64, now: UInt64) -> Bool {
        guard var item = active, item.deliveryStamp == stamp, item.nonce == nonce,
              item.phase == .awaitingDown, item.postedAt == nil else { return false }
        let (ackEnd, ackOverflow) = now.addingReportingOverflow(150)
        let (drainEnd, drainOverflow) = now.addingReportingOverflow(250)
        guard !ackOverflow, !drainOverflow, now < originalDeadline else {
            item.phase = .poisoned; active = item; return false
        }
        item.postedAt = now
        item.acknowledgementCutoff = min(ackEnd, originalDeadline)
        item.drainUntil = drainEnd
        active = item
        return true
    }

    /// Returns true only when this exact event belongs to the active receipt,
    /// or when it consumes the one bounded duplicate-up allowance for a
    /// completed receipt. It never means ACK by itself.
    @discardableResult
    mutating func observe(marker nonce: Int64, type: UInt32, code: Int64, flags: UInt64,
                          stamp: UInt64, autorepeat: Int64, now: UInt64,
                          eventMarker: Int64) -> Bool {
        if var old = retired, nonce == old.nonce {
            let knownUp = type == CGEventType.keyUp.rawValue && code == old.code && stamp == old.stamp &&
                flags == old.flags && autorepeat == 0 && now >= old.receivedAt &&
                now < old.expiresAt && old.available
            if knownUp {
                old.available = false; retired = old
                return true
            }
            poisonActive(now: now)
            return false
        }
        guard var item = active else { return false }
        if item.phase == .drainOnly {
            guard !item.drainOnlyUpConsumed, let down = item.down,
                  let posted = item.postedAt, now >= posted, now < item.drainUntil,
                  now >= down.receivedMS, item.lastReceivedMS.map({ now >= $0 }) ?? true,
                  nonce == item.nonce, nonce > 0, nonce != eventMarker,
                  type == CGEventType.keyUp.rawValue, code == Int64(item.code), autorepeat == 0,
                  stamp > 0, stamp > down.stamp, flags == down.flags,
                  flags & Self.namedFlagsMask == item.expectedNamedFlags else { return false }
            item.drainOnlyUpConsumed = true; item.lastReceivedMS = now; item.phase = .poisoned; active = item
            return true
        }

        // A structurally exact up after the ACK cutoff is still known-owned
        // during bounded drain, but can never become an ACK.
        if item.phase == .awaitingUp, type == CGEventType.keyUp.rawValue,
           let down = item.down, let posted = item.postedAt,
           now >= item.acknowledgementCutoff, now < item.drainUntil,
           now >= posted, now >= down.receivedMS,
           item.lastReceivedMS.map({ now >= $0 }) ?? true,
           nonce == item.nonce, nonce > 0, nonce != eventMarker,
           code == Int64(item.code), autorepeat == 0, stamp > 0, stamp > down.stamp,
           flags == down.flags, flags & Self.namedFlagsMask == item.expectedNamedFlags {
            item.drainOnlyUpConsumed = true; item.lastReceivedMS = now; item.phase = .poisoned; active = item
            return true
        }
        guard nonce == item.nonce, nonce > 0, nonce != eventMarker,
              type == CGEventType.keyDown.rawValue || type == CGEventType.keyUp.rawValue else {
            poison(&item, now: now); active = item
            return false
        }

        let event = Event(type: type, stamp: stamp, flags: flags, code: code,
                          autorepeat: autorepeat, receivedMS: now)
        guard let posted = item.postedAt, now >= posted, now < item.acknowledgementCutoff,
              item.lastReceivedMS.map({ now >= $0 }) ?? true,
              stamp > 0, code == Int64(item.code), autorepeat == 0,
              flags & Self.namedFlagsMask == item.expectedNamedFlags else {
            poison(&item, now: now); active = item
            return false
        }
        item.lastReceivedMS = now
        switch (item.phase, type) {
        case (.awaitingDown, let keyDown) where keyDown == CGEventType.keyDown.rawValue:
            item.down = event; item.phase = .awaitingUp; active = item
            return true
        case (.awaitingUp, let keyUp) where keyUp == CGEventType.keyUp.rawValue:
            guard let down = item.down, stamp > down.stamp, now >= down.receivedMS,
                  flags == down.flags else {
                poison(&item, now: now); active = item; return false
            }
            item.up = event; item.phase = .acknowledged; active = item
            return true
        default:
            poison(&item, now: now)
            active = item
            return false
        }
    }

    func acknowledged(stamp: UInt64, cutoff: UInt64, now: UInt64) -> Bool {
        guard let item = active else { return false }
        return item.deliveryStamp == stamp && item.phase == .acknowledged &&
            item.acknowledgementCutoff == cutoff && now < cutoff
    }

    mutating func cancelUnposted(stamp: UInt64) {
        if let item = active, item.deliveryStamp == stamp, item.postedAt == nil { active = nil }
    }

    mutating func finish(stamp: UInt64) {
        guard let item = active, item.deliveryStamp == stamp, item.phase == .acknowledged,
              let up = item.up, item.drainUntil > 0 else { return }
        retired = Retired(nonce: item.nonce, code: up.code, stamp: up.stamp,
                          flags: up.flags, receivedAt: up.receivedMS, expiresAt: item.drainUntil)
        active = nil
    }

    mutating func markDrainOnly(stamp: UInt64) {
        guard let item = active, item.deliveryStamp == stamp else { return }
        if let up = item.up, item.drainUntil > 0 {
            retired = Retired(nonce: item.nonce, code: up.code, stamp: up.stamp,
                              flags: up.flags, receivedAt: up.receivedMS, expiresAt: item.drainUntil)
            active = nil
        } else if item.down != nil, item.postedAt != nil, item.drainUntil > 0,
                  !item.drainOnlyUpConsumed {
            var draining = item; draining.phase = .drainOnly; active = draining
        } else if item.postedAt != nil, item.down == nil {
            var poisoned = item; poisoned.phase = .poisoned; active = poisoned
        }
    }

    private mutating func poison(_ item: inout Active, now: UInt64) {
        item.lastReceivedMS = max(item.lastReceivedMS ?? now, now)
        if item.down != nil, item.up == nil, item.postedAt != nil,
           item.drainUntil > 0, !item.drainOnlyUpConsumed {
            item.phase = .drainOnly
        } else {
            item.phase = .poisoned
        }
    }

    private mutating func poisonActive(now: UInt64) {
        guard var item = active, item.phase != .drainOnly else { return }
        poison(&item, now: now); active = item
    }
}

/// The tap, control reader, watchdog and AX worker share only typed values.
/// No AX references or run-loop objects are stored in this lock.
func ownedDependencyBootstrapValidity(
    revisions: [String: UInt64], validity: [String: Bool?]
) -> [String: Bool]? {
    guard Set(revisions.keys) == ownedValueDependencyNames,
          revisions.values.allSatisfy({ $0 > 0 }),
          Set(validity.keys) == ownedValueDependencyNames else { return nil }
    return validity.compactMapValues { $0 }
}

enum NativeSubmissionScopeProfile: Sendable {
    case production
    #if CU_STOP_GATE_SELF_TEST
    case simulated
    #endif
}

final class GuardState: Sendable {
    private struct State: Sendable {
        var accessibility = false
        var humanInput = false
        var hostDeadline: UInt64 = 0
        var supervisorDeadline: UInt64 = 0
        var hostExpired = true
        var supervisorExpired = true
        var stopRevision: UInt64 = 0
        var humanSequence: UInt64 = 0
        var hotkeySequence: UInt64 = 0
        var leaseLossSequence: UInt64 = 0
        var faultSequence: UInt64 = 0
        var reason = "native input unavailable"
        var sequence: UInt64 = 0
        var closing = false
        var inputIsolated = false
        var dependencyRevisions: [String: UInt64] = [:]
        var dependencyValidity: [String: Bool] = [:]
        var workerDeadline: UInt64 = 0
        var loopDeadline: UInt64 = 0
        var authority = NativeAuthorityLedger()

        mutating func isolateInput(permit: NativePermit, reason: String) -> Bool {
            if !inputIsolated {
                inputIsolated = true
                if stopRevision < UInt64.max { stopRevision += 1 }
                if faultSequence < UInt64.max { faultSequence += 1 }
                self.reason = reason
            }
            return authority.revoke(permit)
        }
    }
    private let state = OSAllocatedUnfairLock(initialState: State())
    // One already-committed input fragment runs at a time. Contention rejects
    // without consuming intent, rather than ageing an AX witness in a queue.
    // Stop, authority
    // revocation, leases and snapshots never acquire this lock or wait for OS
    // input to return. AX handles remain on the invoking worker's stack.
    private let submission = OSAllocatedUnfairLock(initialState: ())
    // Independent hard bound: a foreign AX action must not block the watchdog
    // by holding the ordered commit lock. This lock contains no input state.
    private struct Liveness: Sendable { var worker: UInt64 = 0; var loop: UInt64 = 0 }
    private let liveness = OSAllocatedUnfairLock(initialState: Liveness())
    private struct PointerDelivery: Sendable {
        var stamp: UInt64 = 0
        var upType: UInt32 = 0
        var upObserved = false
        var postedUntil: UInt64 = 0
    }
    private let pointerDelivery = OSAllocatedUnfairLock(initialState: PointerDelivery())
    private struct KeyDelivery: Sendable {
        var stamp: UInt64 = 0
        var code: UInt16 = 0
        var flags: UInt64 = 0
        var nonce: Int64 = 0
        var active = false
        var postedAt: UInt64?
        var postedUntil: UInt64 = 0
        var upObserved = false
        var receipt = KeyReceiptState()
        #if CU_OWNED_KEY_ACCEPTANCE
        var firstFact: OwnedKeyTapFact?, secondFact: OwnedKeyTapFact?
        var confirmation: OwnedKeyConfirmationFact?
        var diagnosticEmitted = false
        #endif
    }
    private let keyDelivery = OSAllocatedUnfairLock(initialState: KeyDelivery())
    private let executionScope: NativeExecutionScope?
    private let submissionScopeProfile: NativeSubmissionScopeProfile
    let ownedRegistration: NativeOwnedDocumentRegistration?
    let ownedInputJournal: NativeOwnedInputJournal?
    private let ownedValueExecutionEnabled: Bool
    let ownedSaveExecutionEnabled: Bool
    let epoch: UInt64
    let eventMarker: Int64
    let startupMouseButtonsClear: Bool
    let startupKeyboardClear: Bool
    let keyboardInputAvailable: Bool
    init(
        epoch: UInt64,
        executionScope: NativeExecutionScope? = nil,
        eventMarker: Int64 = 0,
        startupMouseButtonsClear: Bool = true,
        startupKeyboardClear: Bool = true,
        ownedRegistration: NativeOwnedDocumentRegistration? = nil,
        ownedInputJournal: NativeOwnedInputJournal? = nil,
        allowOwnedSave: Bool = false,
        submissionScopeProfile: NativeSubmissionScopeProfile = .production
    ) {
        self.epoch = epoch; self.executionScope = executionScope; self.eventMarker = eventMarker
        self.submissionScopeProfile = submissionScopeProfile
        self.ownedRegistration = ownedRegistration
        self.ownedInputJournal = ownedInputJournal
        ownedValueExecutionEnabled = ownedRegistration != nil && ownedInputJournal != nil
            && executionScope?.pid == Int32(ownedRegistration?.pid ?? 0)
            && executionScope?.bundle == NativeOwnedDocumentRegistration.textEditBundleID
            && executionScope?.window == nil
        ownedSaveExecutionEnabled = allowOwnedSave && ownedRegistration != nil
            && ownedInputJournal != nil
            && executionScope?.pid == Int32(ownedRegistration?.pid ?? 0)
            && executionScope?.bundle == NativeOwnedDocumentRegistration.textEditBundleID
            && executionScope?.window == nil
        self.startupMouseButtonsClear = startupMouseButtonsClear
        self.startupKeyboardClear = startupKeyboardClear
        self.keyboardInputAvailable = AtomicKeyProfile.acceptanceEnabled && startupKeyboardClear && startupMouseButtonsClear &&
            executionScope?.bundle == "com.lingxi.computer-user.guard-fixture" && executionScope?.window != nil
            && executionScope?.backgroundAX != true
    }
    private func submissionScopeAllows(_ request: NativeProbe) -> Bool {
        #if CU_STOP_GATE_SELF_TEST
        if case .simulated = submissionScopeProfile {
            return request.valid && !request.claimsBackgroundAX && request.action == "ax_press" &&
                ["com.lingxi.computer-user.guard-fixture", "com.apple.TextEdit"].contains(request.original_proof.fingerprint.app)
        }
        #endif
        return executionScope?.allows(request) == true
    }
    func backgroundAXAuthorization(for request: NativeProbe) -> BackgroundAXAuthorization? {
        executionScope?.backgroundAXAuthorization(for: request)
    }
    var ownedBackgroundAXDiagnosticsEnabled: Bool {
        executionScope?.ownedBackgroundAXDiagnostics == true
    }
    func beginKeyDelivery(proposedStamp: UInt64, code: UInt16, flags: UInt64, nonce: Int64) -> UInt64? {
        guard !pointerDrainPending() else { return nil }
        return keyDelivery.withLock {
            guard !$0.active,
                  let stamp = $0.receipt.begin(proposedStamp: proposedStamp, code: code, flags: flags,
                                               nonce: nonce, eventMarker: eventMarker) else { return nil }
            $0.stamp = stamp; $0.code = code; $0.flags = flags; $0.nonce = nonce
            $0.active = true; $0.postedAt = nil; $0.postedUntil = 0; $0.upObserved = false
            #if CU_OWNED_KEY_ACCEPTANCE
            $0.firstFact = nil; $0.secondFact = nil; $0.confirmation = nil
            $0.diagnosticEmitted = false
            #endif
            return $0.stamp
        }
    }
    func keyPostsStarted(stamp: UInt64, nonce: Int64, originalDeadline: UInt64, now: UInt64 = monotonicMS()) {
        keyDelivery.withLock {
            guard $0.active && $0.stamp == stamp && $0.nonce == nonce else { return }
            _ = $0.receipt.postsStarted(stamp: stamp, nonce: nonce, originalDeadline: originalDeadline, now: now)
            $0.postedAt = now
            $0.postedUntil = now > UInt64.max - 250 ? UInt64.max : now + 250
        }
    }
    @discardableResult
    func observeOwnedKey(marker: Int64, type: UInt32, code: Int64, flags: UInt64, stamp: UInt64,
                         autorepeat: Int64, now: UInt64 = monotonicMS()) -> Bool {
        keyDelivery.withLock {
            let known = $0.receipt.observe(marker: marker, type: type, code: code, flags: flags,
                stamp: stamp, autorepeat: autorepeat, now: now, eventMarker: eventMarker)
            #if CU_OWNED_KEY_ACCEPTANCE
            // Fixed-size facts only: no allocation, characters, IO or matching
            // relaxation in the callback. Capture only the active nonce's facts.
            if let activeNonce = $0.receipt.currentNonce, marker == activeNonce, $0.active,
               type == CGEventType.keyDown.rawValue || type == CGEventType.keyUp.rawValue {
                let fact = OwnedKeyTapFact(type: type, stamp: stamp, flags: flags, code: code,
                    autorepeat: autorepeat, observed_ms: now)
                if $0.firstFact == nil { $0.firstFact = fact } else if $0.secondFact == nil { $0.secondFact = fact }
            }
            #endif
            if !$0.receipt.isAcknowledged { $0.upObserved = false }
            else {
                #if CU_OWNED_KEY_DROP_ACK
                // The sequence remains mapped as owned, while this acceptance
                // build deliberately loses the local acknowledgement bit.
                $0.upObserved = keyReceiptVisibleAcknowledgement($0.receipt.isAcknowledged, droppingReceipt: true)
                #else
                $0.upObserved = keyReceiptVisibleAcknowledgement($0.receipt.isAcknowledged, droppingReceipt: false)
                #endif
            }
            return known
        }
    }
    #if CU_OWNED_KEY_ACCEPTANCE
    func recordOwnedKeyConfirmation(_ fact: OwnedKeyConfirmationFact, stamp: UInt64) {
        keyDelivery.withLock { if $0.stamp == stamp { $0.confirmation = fact } }
    }
    func ownedKeyDeliveryFacts() -> OwnedKeyDeliveryFacts {
        keyDelivery.withLock {
            OwnedKeyDeliveryFacts(expected_stamp: $0.stamp, expected_flags: $0.flags, expected_code: $0.code,
                expected_down_type: CGEventType.keyDown.rawValue, expected_up_type: CGEventType.keyUp.rawValue,
                expected_autorepeat: 0,
                posted_ms: $0.postedAt, drain_until_ms: $0.postedUntil, up_observed: $0.upObserved,
                first: $0.firstFact, second: $0.secondFact, confirmation: $0.confirmation)
        }
    }
    func takeOwnedKeyDeliveryFactsForDiagnostic() -> OwnedKeyDeliveryFacts? {
        guard keyboardInputAvailable else { return nil }
        return keyDelivery.withLock {
            guard !$0.diagnosticEmitted, $0.active || $0.postedAt != nil else { return nil }
            $0.diagnosticEmitted = true
            return OwnedKeyDeliveryFacts(expected_stamp: $0.stamp, expected_flags: $0.flags, expected_code: $0.code,
                expected_down_type: CGEventType.keyDown.rawValue, expected_up_type: CGEventType.keyUp.rawValue,
                expected_autorepeat: 0,
                posted_ms: $0.postedAt, drain_until_ms: $0.postedUntil, up_observed: $0.upObserved,
                first: $0.firstFact, second: $0.secondFact, confirmation: $0.confirmation)
        }
    }
    #endif
    func keyUpObserved(stamp: UInt64) -> Bool {
        keyDelivery.withLock { $0.stamp == stamp && $0.upObserved && $0.receipt.isAcknowledged }
    }
    func cancelUnpostedKey(stamp: UInt64) {
        keyDelivery.withLock {
            if $0.stamp == stamp && $0.postedAt == nil {
                $0.receipt.cancelUnposted(stamp: stamp); $0.active = false
            }
        }
    }
    func markKeyDeliveryDrainOnly(stamp: UInt64) {
        keyDelivery.withLock { $0.receipt.markDrainOnly(stamp: stamp) }
    }
    func finishKeyDelivery(stamp: UInt64) {
        keyDelivery.withLock {
            if $0.stamp == stamp && $0.upObserved && $0.receipt.isAcknowledged {
                $0.receipt.finish(stamp: stamp); $0.active = false
            }
        }
    }
    func keyDrainPending(now: UInt64 = monotonicMS()) -> Bool {
        keyDelivery.withLock { keyReceiptDrainPending(until: $0.postedUntil, upObserved: $0.upObserved, now: now) }
    }
    func keyConfirmationCoverage(_ ticket: NativeProbeTicket, now: UInt64 = monotonicMS()) -> Bool {
        state.withLock { !$0.closing && !$0.inputIsolated && $0.accessibility && $0.humanInput &&
            ticket.request.guard_revision == $0.stopRevision && now < $0.hostDeadline && now < $0.supervisorDeadline }
    }
    func confirmedKeyReturnMS(_ ticket: NativeProbeTicket, cutoff: UInt64) -> UInt64? {
        return state.withLock {
            let now = monotonicMS(), wall = unixMS()
            let receiptAcknowledged = keyDelivery.withLock {
                $0.active && $0.receipt.acknowledged(stamp: $0.stamp, cutoff: cutoff, now: now)
            }
            guard keyReceiptFinalizationAllowed(receiptAcknowledged: receiptAcknowledged,
                    closing: $0.closing, isolated: $0.inputIsolated, accessibility: $0.accessibility,
                    humanInput: $0.humanInput, ticketRevision: ticket.request.guard_revision,
                    stopRevision: $0.stopRevision, now: now, cutoff: cutoff,
                    ticketDeadline: ticket.deadline, hostDeadline: $0.hostDeadline,
                    supervisorDeadline: $0.supervisorDeadline),
                  $0.authority.checkClock(wall: wall, uptime: now),
                  let clock = $0.authority.clock, wall >= clock.origin else { return nil }
            let returned = wall - clock.origin
            return returned < ticket.request.valid_until_ms ? returned : nil
        }
    }
    func isolateInputAfterKeyUnknown(stamp: UInt64? = nil) {
        state.withLock {
            if !$0.inputIsolated { $0.inputIsolated = true; $0.stopRevision += 1; $0.reason = "native key delivery unknown; input isolated; do not replay" }
            if let stamp {
                keyDelivery.withLock {
                    $0.receipt.markDrainOnly(stamp: stamp)
                    $0.active = $0.receipt.hasActiveReceipt
                }
            }
        }
    }
    func beginPointerDelivery(proposedStamp: UInt64, upType: UInt32) -> UInt64? {
        pointerDelivery.withLock {
            guard proposedStamp > 0, $0.stamp < UInt64.max else { return nil }
            $0.stamp = max(proposedStamp, $0.stamp + 1); $0.upType = upType; $0.upObserved = false; $0.postedUntil = 0
            return $0.stamp
        }
    }
    func observeOwnedPointer(type: UInt32, stamp: UInt64) {
        #if CU_OWNED_POINTER_DROP_ACK
        // A separately built owned-fixture acceptance helper only.
        return
        #else
        pointerDelivery.withLock {
            if $0.stamp == stamp && $0.upType == type { $0.upObserved = true }
        }
        #endif
    }
    func pointerUpObserved(stamp: UInt64) -> Bool {
        pointerDelivery.withLock { $0.stamp == stamp && $0.upObserved }
    }
    func pointerPostsStarted(stamp: UInt64) {
        pointerDelivery.withLock { if $0.stamp == stamp { $0.postedUntil = monotonicMS() + 250 } }
    }
    func pointerDrainPending(now: UInt64 = monotonicMS()) -> Bool {
        pointerDelivery.withLock { $0.postedUntil > now && !$0.upObserved }
    }
    /// Delivery confirmation may fail or the clock may fault after perform
    /// returned. Recheck its original span without upgrading Unknown or
    /// inventing an end time. Post-call Unknown also closes new input permits.
    func finalizeInputReceipt(_ ticket: NativeProbeTicket, response: NativeSubmitResponse) -> NativeSubmitResponse {
        guard response.submitted else { return response }
        return state.withLock { value in
            var result = response
            let wall = unixMS(), uptime = monotonicMS()
            let limit = ticket.request.valid_until_ms > UInt64.max - 500
                ? UInt64.max : ticket.request.valid_until_ms + 500
            var trustedReturn: UInt64?
            if let clock = value.authority.clock,
               value.authority.checkClock(wall: wall, uptime: uptime), wall >= clock.origin,
               let committed = result.committed_ms, let originalReturn = result.returned_ms {
                let current = wall - clock.origin
                if committed >= ticket.request.started_ms, committed < ticket.request.valid_until_ms,
                   originalReturn >= committed, current >= originalReturn, current <= limit {
                    trustedReturn = current
                }
            }
            result.returned_ms = trustedReturn
            if trustedReturn == nil {
                value.authority.failedClock = true
                result.outcome = "unknown"
                result.reason = "native input final return clock untrusted; do not replay"
            }
            if result.outcome == "unknown" {
                _ = value.isolateInput(permit: ticket.request.permit,
                    reason: "native input final receipt unknown; input isolated; do not replay")
            }
            return result
        }
    }

    func permissions(ax: Bool, human: Bool) {
        state.withLock {
            if ($0.accessibility && !ax) || ($0.humanInput && !human) {
                $0.stopRevision += 1; $0.faultSequence += 1; $0.reason = "device permission or event tap unavailable"
            }
            $0.accessibility = ax; $0.humanInput = human
        }
    }
    func notice(_ notice: InputNotice, type: UInt32? = nil) {
        state.withLock {
            switch notice {
            case .ignored: break
            case .human:
                $0.humanSequence += 1; $0.stopRevision += 1; $0.reason = type.map { "human input type=\($0)" } ?? "human input"
            case .stop:
                $0.hotkeySequence += 1; $0.stopRevision += 1; $0.reason = "local stop hotkey"
            case .disabled:
                $0.humanInput = false; $0.stopRevision += 1; $0.faultSequence += 1; $0.reason = "event tap disabled"
            }
        }
    }
    func stop(_ reason: String = "operator stop") {
        state.withLock { $0.stopRevision += 1; $0.reason = reason }
    }
    func renew(lane: String, duration: UInt64, now: UInt64 = monotonicMS()) -> Bool {
        guard duration > 0, duration <= 1500, now <= UInt64.max - duration else { return false }
        return state.withLock {
            if lane == "host" {
                if !$0.hostExpired && now >= $0.hostDeadline {
                    $0.stopRevision += 1; $0.leaseLossSequence += 1; $0.reason = "host lease expired before renewal"
                }
                $0.hostDeadline = now + duration; $0.hostExpired = false
            } else if lane == "supervisor" {
                if !$0.supervisorExpired && now >= $0.supervisorDeadline {
                    $0.stopRevision += 1; $0.leaseLossSequence += 1; $0.reason = "supervisor lease expired before renewal"
                }
                $0.supervisorDeadline = now + duration; $0.supervisorExpired = false
            }
            else { return false }
            // Lease renewal never clears a stop or restores an old generation.
            return true
        }
    }
    func expire(now: UInt64 = monotonicMS()) -> Bool {
        let wall = unixMS()
        return state.withLock {
            if $0.authority.clock != nil && !$0.authority.failedClock && !$0.authority.checkClock(wall: wall, uptime: now) {
                $0.stopRevision += 1; $0.faultSequence += 1; $0.reason = "native authority clock diverged"
            }
            if !$0.hostExpired && now >= $0.hostDeadline {
                $0.hostExpired = true; $0.stopRevision += 1; $0.leaseLossSequence += 1; $0.reason = "host lease expired"
            }
            if !$0.supervisorExpired && now >= $0.supervisorDeadline {
                $0.supervisorExpired = true; $0.stopRevision += 1; $0.leaseLossSequence += 1; $0.reason = "supervisor lease expired"
            }
            return ($0.workerDeadline != 0 && now >= $0.workerDeadline) ||
                ($0.loopDeadline != 0 && now >= $0.loopDeadline)
        }
    }
    func loopAlive(now: UInt64 = monotonicMS()) {
        liveness.withLock { $0.loop = now + 750 }
        state.withLock { $0.loopDeadline = now + 750 }
    }
    func beginQuery(now: UInt64 = monotonicMS()) -> UInt64 {
        liveness.withLock { $0.worker = now + 500 }
        return state.withLock { $0.workerDeadline = now + 500; return $0.stopRevision }
    }
    func endQuery(revision: UInt64) -> Bool {
        let valid = state.withLock { $0.workerDeadline = 0; return !$0.closing && $0.stopRevision == revision }
        liveness.withLock { $0.worker = 0 }
        return valid
    }
    func hardExpired(now: UInt64 = monotonicMS()) -> Bool {
        liveness.withLock { ($0.worker != 0 && now >= $0.worker) || ($0.loop != 0 && now >= $0.loop) }
    }
    func close() { state.withLock { $0.closing = true; $0.stopRevision += 1; $0.reason = "control channel closed" } }
    func configureAuthority(
        origin: UInt64,
        surface: NativeSurface,
        domain: String,
        dependencies: [String: UInt64]? = nil,
        dependencyValidity: [String: Bool?]? = nil
    ) -> Bool {
        let now = monotonicMS(), wall = unixMS()
        guard let clock = NativeAuthorityClock(origin: origin, surface: surface, domain: domain, epoch: epoch, wall: wall, uptime: now) else { return false }
        let initialValidity: [String: Bool]?
        if ownedValueExecutionEnabled {
            guard let dependencies,
                  let dependencyValidity,
                  let validated = ownedDependencyBootstrapValidity(
                      revisions: dependencies, validity: dependencyValidity) else { return false }
            initialValidity = validated
        } else if dependencies != nil || dependencyValidity != nil {
            return false
        } else { initialValidity = nil }
        return state.withLock {
            if let dependencies, let initialValidity, !$0.dependencyRevisions.isEmpty {
                guard $0.dependencyRevisions == dependencies,
                      $0.dependencyValidity == initialValidity else { return false }
            }
            guard !$0.closing, $0.authority.configure(clock) else { return false }
            if let dependencies, let initialValidity {
                $0.dependencyRevisions = dependencies
                $0.dependencyValidity = initialValidity
            }
            return true
        }
    }
    func updateDependency(name: String, revision: UInt64, valid: Bool?) -> Bool {
        guard ownedValueExecutionEnabled, ownedValueDependencyNames.contains(name), revision > 0 else { return false }
        return state.withLock {
            guard !$0.closing, let current = $0.dependencyRevisions[name],
                  current < UInt64.max, revision == current + 1 else {
                $0.closing = true
                $0.inputIsolated = true
                $0.stopRevision += 1
                $0.reason = "owned dependency revision sequence invalid"
                $0.faultSequence += 1
                return false
            }
            $0.dependencyRevisions[name] = revision
            if let valid { $0.dependencyValidity[name] = valid }
            else { $0.dependencyValidity.removeValue(forKey: name) }
            $0.stopRevision += 1
            $0.reason = "owned dependency changed"
            return true
        }
    }
    func ownedSourceNowMS() -> UInt64? {
        state.withLock { $0.authority.clock?.sourceNow(uptime: monotonicMS()) }
    }
    func decodeOwnedValueRequest(_ data: Data) -> NativeOwnedValueRequest? {
        guard ownedValueExecutionEnabled, let registration = ownedRegistration else { return nil }
        guard let context = state.withLock({ value -> (NativeSurface, String, UInt64)? in
            guard let clock = value.authority.clock,
                  let sourceNow = clock.sourceNow(uptime: monotonicMS()) else { return nil }
            return (clock.surface, clock.domain, sourceNow)
        }) else { return nil }
        return NativeOwnedValueRequest.decode(
            data,
            registration: registration,
            expectedSessionEpoch: epoch,
            expectedSurface: context.0,
            expectedClockDomain: context.1,
            nowSourceMS: context.2,
            allowSave: ownedSaveExecutionEnabled
        )
    }
    func admitOwnedValue(_ request: NativeOwnedValueRequest) -> (NativeOwnedValueTicket?, String, String) {
        guard ownedValueExecutionEnabled,
              request.action == ownedValueSetterAction
                || ownedSaveExecutionEnabled && request.action == ownedDocumentSaveAction,
              startupMouseButtonsClear,
              let scope = executionScope,
              let registration = ownedRegistration,
              scope.pid == Int32(registration.pid),
              scope.bundle == registration.bundle_id,
              scope.window == nil,
              request.validIdentity else { return (nil, "rejected", "owned_scope_unavailable") }
        let now = monotonicMS(), wall = unixMS()
        return state.withLock { guardState in
            guard !guardState.closing, !guardState.inputIsolated else { return (nil, "stop_latched", "owned_channel_closed") }
            guard guardState.accessibility, guardState.humanInput else { return (nil, "stop_latched", "owned_permissions_unavailable") }
            guard now < guardState.hostDeadline, now < guardState.supervisorDeadline else { return (nil, "stop_latched", "owned_lease_expired") }
            guard request.guard_revision == guardState.stopRevision else { return (nil, "stop_latched", "owned_guard_revision_changed") }
            guard request.dependencies == guardState.dependencyRevisions else { return (nil, "stop_latched", "owned_dependency_revision_changed") }
            guard ownedValueDependencyNames.allSatisfy({ name in guardState.dependencyValidity[name] == true }) else {
                return (nil, "stop_latched", "owned_dependency_not_valid")
            }
            let result = guardState.authority.admitOwnedValue(request, now: now, wall: wall)
            return (result.0, result.1, "owned_authority_" + result.1)
        }
    }
    func refuseOwnedCommit(
        _ ticket: NativeOwnedValueTicket,
        outcome: String,
        reason: String
    ) -> NativeOwnedValueSubmitResponse {
        NativeOwnedValueSubmitResponse(
            sessionEpoch: epoch,
            requestID: ticket.request.request_id,
            outcome: outcome,
            reason: reason
        )
    }
    /// The journal is persisted before this gate. The short state check orders
    /// Stop/revoke with consuming intent. The one AX call uses the shared
    /// submission gate; Stop and snapshots never wait for it to return.
    func commitOwnedValue(
        _ ticket: NativeOwnedValueTicket,
        perform: () -> Bool
    ) -> NativeOwnedValueSubmitResponse {
        return submission.withLockIfAvailableUnchecked { _ in
            let request = ticket.request
            let prepared: (NativeOwnedValueSubmitResponse, NativeAuthorityClock?, UInt64, UInt64) = state.withLock { guardState in
                var response = NativeOwnedValueSubmitResponse(
                    sessionEpoch: epoch, requestID: request.request_id,
                    outcome: "rejected", reason: "owned atomic commit gate closed")
                let now = monotonicMS(), wall = unixMS()
                guard ownedValueExecutionEnabled,
                      request.action == ownedValueSetterAction
                        || ownedSaveExecutionEnabled && request.action == ownedDocumentSaveAction,
                      let scope = executionScope,
                      let registration = ownedRegistration,
                      scope.pid == Int32(registration.pid),
                      scope.bundle == registration.bundle_id,
                      scope.window == nil,
                      startupMouseButtonsClear,
                      !guardState.closing, !guardState.inputIsolated, guardState.accessibility, guardState.humanInput,
                      now < guardState.hostDeadline, now < guardState.supervisorDeadline,
                      request.guard_revision == guardState.stopRevision,
                      request.dependencies == guardState.dependencyRevisions,
                      ownedValueDependencyNames.allSatisfy({ name in guardState.dependencyValidity[name] == true }) else {
                    response.outcome = "stop_latched"
                    response.reason = "owned input guard, lease or dependency changed before setter"
                    return (response, nil, 0, 0)
                }
                guard let clock = guardState.authority.clock, wall >= clock.origin else {
                    response.outcome = "context_unresolved"
                    response.reason = "owned authority clock unavailable"
                    return (response, nil, 0, 0)
                }
                guard now < ticket.deadline, wall - clock.origin < request.valid_until_ms else {
                    response.outcome = "expired"
                    response.reason = "owned source or native deadline expired before setter"
                    return (response, nil, 0, 0)
                }
                if let invalid = guardState.authority.commitOwnedValue(ticket, now: now, wall: wall) {
                    response.outcome = invalid
                    response.reason = "owned request was revoked or already consumed"
                    return (response, nil, 0, 0)
                }
                response.submitted = true; response.committed_ms = wall - clock.origin
                return (response, clock, wall, now)
            }
            let initialResponse = prepared.0
            guard initialResponse.submitted, let clock = prepared.1 else { return initialResponse }
            let setterSucceeded = perform()
            let returnedWallMS = unixMS()
            let returnedUptimeMS = monotonicMS()
            return state.withLock { guardState in
                var response = initialResponse
                let clockTrusted = guardState.authority.checkClock(wall: returnedWallMS, uptime: returnedUptimeMS)
                let returnedSourceMS = clockTrusted ? inputReturnSourceMSIfTrusted(
                    clock: clock,
                    committedWallMS: prepared.2,
                    committedUptimeMS: prepared.3,
                    returnedWallMS: returnedWallMS,
                    returnedUptimeMS: returnedUptimeMS,
                    validUntilMS: request.valid_until_ms
                ) : nil
                response.outcome = ownedValueSetterOutcome(
                    callSucceeded: setterSucceeded, returnedSourceMS: returnedSourceMS
                )
                response.returned_ms = returnedSourceMS
                if returnedSourceMS == nil { guardState.authority.failedClock = true }
                if response.outcome == "unknown" {
                    let reason = returnedSourceMS == nil
                        ? "owned AX action return clock was inconsistent; input isolated; do not replay"
                        : "owned AX action result unknown; input isolated; do not replay"
                    _ = guardState.isolateInput(permit: request.permit, reason: reason)
                }
                response.reason = response.outcome == "applied"
                    ? "owned AX action returned; verify effect with a new source observation"
                    : returnedSourceMS == nil
                        ? "owned AX action return clock was inconsistent; do not replay"
                        : "owned AX action result unknown; do not replay"
                return response
            }
        } ?? NativeOwnedValueSubmitResponse(sessionEpoch: epoch, requestID: ticket.request.request_id,
            outcome: "rejected", reason: "native submission occupied; intent not consumed")
    }
    @discardableResult
    func failOwnedInput(_ permit: NativePermit, reason: String) -> Bool {
        state.withLock { $0.isolateInput(permit: permit, reason: reason) }
    }
    func failOwnedJournal(_ permit: NativePermit) {
        _ = failOwnedInput(permit, reason: "owned input journal failed; input isolated")
    }
    func revokeAuthority(_ permit: NativePermit) -> Bool { state.withLock { $0.authority.revoke(permit) } }
    func admitProbe(_ request: NativeProbe, now: UInt64 = monotonicMS(), wall: UInt64 = unixMS()) -> (NativeProbeTicket?, String) {
        guard !ownedValueExecutionEnabled else { return (nil, "rejected") }
        if request.action == "key_press", (!startupKeyboardClear || executionScope?.allows(request) != true) { return (nil, "rejected") }
        return state.withLock {
            guard !$0.closing, !$0.inputIsolated, $0.accessibility, $0.humanInput, now < $0.hostDeadline, now < $0.supervisorDeadline,
                  request.guard_revision == $0.stopRevision else { return (nil, "stop_latched") }
            return $0.authority.admit(request, now: now, wall: wall)
        }
    }
    func probeValidity(_ ticket: NativeProbeTicket, now: UInt64 = monotonicMS(), wall: UInt64 = unixMS()) -> String? {
        state.withLock {
            guard !$0.closing, !$0.inputIsolated, $0.accessibility, $0.humanInput, now < $0.hostDeadline, now < $0.supervisorDeadline,
                  ticket.request.guard_revision == $0.stopRevision else { return "stop_latched" }
            return $0.authority.validity(ticket, now: now, wall: wall)
        }
    }
    func admitSubmission(_ request: NativeProbe) -> (NativeProbeTicket?, String) {
        guard !ownedValueExecutionEnabled else { return (nil, "rejected") }
        guard startupMouseButtonsClear, submissionScopeAllows(request) else { return (nil, "rejected") }
        if request.action == "key_press" {
            let now = monotonicMS(), wall = unixMS()
            return state.withLock {
                guard startupKeyboardClear, !$0.closing, !$0.inputIsolated, $0.accessibility, $0.humanInput,
                      now < $0.hostDeadline, now < $0.supervisorDeadline, request.guard_revision == $0.stopRevision else { return (nil, "stop_latched") }
                return $0.authority.stagedKeySubmission(request, now: now, wall: wall)
            }
        }
        return admitProbe(request)
    }
    func recordMenuOpening(_ ticket: NativeProbeTicket, response: NativeSubmitResponse) -> Bool {
        state.withLock {
            guard !$0.closing, $0.accessibility, $0.humanInput, ticket.request.guard_revision == $0.stopRevision,
                  monotonicMS() < $0.hostDeadline, monotonicMS() < $0.supervisorDeadline else { return false }
            return $0.authority.recordMenuOpening(ticket, response: response)
        }
    }
    /// The short state gate orders Stop/revoke with consuming the input intent.
    /// The straight-line OS invocation then runs under the separate serial
    /// submission lock. An already-committed fragment cannot be undone by Stop;
    /// a concurrent fragment is refused without waiting or consuming intent.
    /// The optional final read runs without State held; its deadline is sampled
    /// with Stop state before intent consumption. Non-success remains unknown.
    func commitPress(
        _ ticket: NativeProbeTicket,
        deadlineMS: UInt64? = nil,
        preCommitRevalidate: (() -> String?)? = nil,
        perform: () -> Bool
    ) -> NativeSubmitResponse {
        // These synchronous closures may invoke worker-local AX refs; no ref is
        // stored in State or transferred to a different thread.
        return submission.withLockIfAvailableUnchecked { _ in
            // These platform reads can wait on the OS. Stop never waits for
            // them; the read-only final revalidation also runs before the
            // short state gate, which samples revisions and time afterwards.
            let scopeAllowed = submissionScopeAllows(ticket.request)
            let keyPrerequisites = ticket.request.action != "key_press" ||
                startupKeyboardClear && CGPreflightPostEventAccess() && mouseButtonsClear() && systemKeyboardClear()
            let preCommitFailure: String?
            if scopeAllowed, let preCommitRevalidate { preCommitFailure = preCommitRevalidate() }
            else { preCommitFailure = nil }
            let prepared: (NativeSubmitResponse, NativeAuthorityClock?, UInt64, UInt64) = state.withLock { value in
                var result = NativeSubmitResponse(session_epoch: epoch, request_id: ticket.request.request_id,
                    outcome: "rejected", reason: "atomic commit gate closed")
                let now = monotonicMS(), wall = unixMS()
                guard !ownedValueExecutionEnabled, startupMouseButtonsClear, scopeAllowed,
                      !value.closing, !value.inputIsolated, value.accessibility, value.humanInput,
                      now < value.hostDeadline, now < value.supervisorDeadline,
                      ticket.request.guard_revision == value.stopRevision else { return (result, nil, 0, 0) }
                if let deadlineMS, now >= deadlineMS {
                    result.outcome = "expired"; result.reason = "native action deadline expired before commit"
                    return (result, nil, 0, 0)
                }
                if let preCommitFailure {
                    result.outcome = "context_unresolved"; result.reason = preCommitFailure
                    return (result, nil, 0, 0)
                }
                if ticket.request.action == "key_press" {
                    guard keyPrerequisites else {
                        result.outcome = "context_unresolved"; result.reason = "system keyboard or posting prerequisites changed before first post"; return (result, nil, 0, 0)
                    }
                    guard let clock = value.authority.clock,
                          atomicKeyCommitTimeAllowed(now: now, deadline: ticket.deadline, hostDeadline: value.hostDeadline,
                            supervisorDeadline: value.supervisorDeadline, wall: wall, origin: clock.origin, validUntil: ticket.request.valid_until_ms) else {
                        result.outcome = "expired"; result.reason = "key deadline or lease expired during system-state verification"; return (result, nil, 0, 0)
                    }
                }
                guard let clock = value.authority.clock, wall >= clock.origin,
                      wall - clock.origin < ticket.request.valid_until_ms else { result.outcome = "expired"; return (result, nil, 0, 0) }
                if let invalid = value.authority.commit(ticket, now: now, wall: wall) { result.outcome = invalid; return (result, nil, 0, 0) }
                result.submitted = true; result.committed_ms = wall - clock.origin
                result.binding_digest = ticket.request.binding_digest
                return (result, clock, wall, now)
            }
            let initialResult = prepared.0
            guard initialResult.submitted, let clock = prepared.1 else { return initialResult }
            let callSucceeded = perform()
            let returnedWall = unixMS(), returnedUptime = monotonicMS()
            return state.withLock { value in
                var result = initialResult
                let clockTrusted = value.authority.checkClock(wall: returnedWall, uptime: returnedUptime)
                let returnedSource = clockTrusted ? inputReturnSourceMSIfTrusted(
                    clock: clock, committedWallMS: prepared.2, committedUptimeMS: prepared.3,
                    returnedWallMS: returnedWall, returnedUptimeMS: returnedUptime,
                    validUntilMS: ticket.request.valid_until_ms) : nil
                if let returnedSource {
                    result.returned_ms = returnedSource
                    result.outcome = callSucceeded ? "applied" : "unknown"
                } else {
                    result.returned_ms = nil
                    result.outcome = "unknown"
                    value.authority.failedClock = true
                }
                result.reason = result.outcome == "applied" ? "atomic input call returned; effect unverified"
                    : result.returned_ms == nil ? "native input return clock untrusted; do not replay"
                    : "atomic input result unknown; do not replay"
                if result.outcome == "unknown" {
                    _ = value.isolateInput(permit: ticket.request.permit,
                        reason: "native input call or return clock unknown; input isolated; do not replay")
                }
                return result
            }
        } ?? NativeSubmitResponse(session_epoch: epoch, request_id: ticket.request.request_id,
            outcome: "rejected", reason: "native submission occupied; intent not consumed")
    }
    // Closing fences admission immediately. The run loop retains the event
    // tap for a bounded already-posted release; it never emits another down.
    var closing: Bool { state.withLock { $0.closing } && !pointerDrainPending() && !keyDrainPending() }
    var inputClosed: Bool { state.withLock { $0.closing } }
    func snapshot(now: UInt64 = monotonicMS()) -> GuardSnapshot {
        return state.withLock {
            $0.sequence += 1
            return GuardSnapshot(protocol_version: 1, kind: "guard", session_epoch: epoch,
                sequence: $0.sequence, uptime_ms: now, accessibility: $0.accessibility,
                human_input: $0.humanInput, host_alive: !$0.hostExpired,
                supervisor_alive: !$0.supervisorExpired, stop_revision: $0.stopRevision,
                human_sequence: $0.humanSequence, hotkey_sequence: $0.hotkeySequence,
                lease_loss_sequence: $0.leaseLossSequence, fault_sequence: $0.faultSequence,
                reason: startupMouseButtonsClear ? $0.reason : "mouse input is not clear at session start; restart after reset",
                input_available: false, authority_clock_ready: $0.authority.clock != nil && !$0.authority.failedClock,
                startup_mouse_buttons_clear: startupMouseButtonsClear,
                startup_keyboard_clear: startupKeyboardClear,
                keyboard_input_available: keyboardInputAvailable)
        }
    }
}
