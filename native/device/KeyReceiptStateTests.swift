import ApplicationServices
import Foundation

private struct KeyReceiptStateTestFailure: Error { let check: Int }

func runKeyReceiptStateTests() throws -> Int {
    var passed = 0
    func check(_ value: Bool) throws {
        guard value else { throw KeyReceiptStateTestFailure(check: passed + 1) }
        passed += 1
    }
    func started(nonce: Int64 = 9001, deadline: UInt64 = 500, now: UInt64 = 10) throws -> (KeyReceiptState, UInt64) {
        var receipt = KeyReceiptState()
        guard let stamp = receipt.begin(proposedStamp: 100, code: 49, flags: 0,
                nonce: nonce, eventMarker: 123),
              receipt.postsStarted(stamp: stamp, nonce: nonce, originalDeadline: deadline, now: now) else {
            throw KeyReceiptStateTestFailure(check: passed + 1)
        }
        return (receipt, stamp)
    }
    func observe(_ receipt: inout KeyReceiptState, nonce: Int64 = 9001,
                 type: CGEventType, code: Int64 = 49, flags: UInt64 = 0,
                 stamp: UInt64, repeatCount: Int64 = 0, now: UInt64 = 11) -> Bool {
        receipt.observe(marker: nonce, type: type.rawValue, code: code, flags: flags,
            stamp: stamp, autorepeat: repeatCount, now: now, eventMarker: 123)
    }

    // Nonces and expected named flags are validated before a delivery opens.
    var invalid = KeyReceiptState()
    try check(invalid.begin(proposedStamp: 100, code: 49, flags: 0, nonce: 0, eventMarker: 123) == nil)
    try check(invalid.begin(proposedStamp: 100, code: 49, flags: 0, nonce: 123, eventMarker: 123) == nil)
    try check(invalid.begin(proposedStamp: 100, code: 49, flags: CGEventFlags.maskShift.rawValue,
        nonce: 9001, eventMarker: 123) == nil)
    try check(invalid.begin(proposedStamp: 0, code: 49, flags: 0, nonce: 9001, eventMarker: 123) == nil)

    // A valid pair uses the actual ordered tap timestamps, then expires at the
    // earlier of the 150 ms receipt window and the original action deadline.
    var (pair, pairStamp) = try started(deadline: 120, now: 10)
    try check(pair.acknowledgementCutoff == 120)
    try check(observe(&pair, type: .keyDown, stamp: 400, now: 10))
    try check(pair.phase == .awaitingUp && pair.downEvent?.stamp == 400)
    try check(observe(&pair, type: .keyUp, stamp: 401, now: 11))
    try check(pair.phase == .acknowledged && pair.upEvent?.stamp == 401)
    try check(pair.acknowledged(stamp: pairStamp, cutoff: 120, now: 119))
    try check(!pair.acknowledged(stamp: pairStamp, cutoff: 120, now: 120))
    try check(!pair.acknowledged(stamp: pairStamp + 1, cutoff: 120, now: 119))

    var releaseBeforeDown = try started()
    try check(!observe(&releaseBeforeDown.0, type: .keyUp, stamp: 501))
    try check(releaseBeforeDown.0.phase == .poisoned)
    var duplicateDown = try started()
    try check(observe(&duplicateDown.0, type: .keyDown, stamp: 510))
    try check(!observe(&duplicateDown.0, type: .keyDown, stamp: 511))
    try check(duplicateDown.0.phase == .drainOnly)
    try check(observe(&duplicateDown.0, type: .keyUp, stamp: 512, now: 12))
    try check(!duplicateDown.0.isAcknowledged)
    var duplicateUp = try started()
    try check(observe(&duplicateUp.0, type: .keyDown, stamp: 520))
    try check(observe(&duplicateUp.0, type: .keyUp, stamp: 521))
    try check(!observe(&duplicateUp.0, type: .keyUp, stamp: 522))
    try check(duplicateUp.0.phase == .poisoned)
    var nonIncreasingTimestamp = try started()
    try check(observe(&nonIncreasingTimestamp.0, type: .keyDown, stamp: 530))
    try check(!observe(&nonIncreasingTimestamp.0, type: .keyUp, stamp: 530))
    try check(nonIncreasingTimestamp.0.phase == .drainOnly)
    try check(observe(&nonIncreasingTimestamp.0, type: .keyUp, stamp: 531, now: 12))
    try check(!nonIncreasingTimestamp.0.isAcknowledged)

    // Only named SDK flags are interpreted. An opaque raw bit is accepted as
    // evidence when both tap events carry it unchanged, and rejected if it
    // changes between down and up.
    let opaqueFlag = UInt64(1) << 63
    var stableRawFlags = try started()
    try check(observe(&stableRawFlags.0, type: .keyDown, flags: opaqueFlag, stamp: 600))
    try check(observe(&stableRawFlags.0, type: .keyUp, flags: opaqueFlag, stamp: 601))
    var unstableRawFlags = try started()
    try check(observe(&unstableRawFlags.0, type: .keyDown, flags: opaqueFlag, stamp: 610))
    try check(!observe(&unstableRawFlags.0, type: .keyUp, flags: 0, stamp: 611))
    try check(unstableRawFlags.0.phase == .drainOnly)
    try check(observe(&unstableRawFlags.0, type: .keyUp, flags: opaqueFlag, stamp: 612, now: 12))
    try check(!unstableRawFlags.0.isAcknowledged)
    var namedFlags = try started()
    try check(!observe(&namedFlags.0, type: .keyDown, flags: CGEventFlags.maskCommand.rawValue, stamp: 620))
    try check(namedFlags.0.phase == .poisoned)

    // Receipt events at or after the strict cutoff cannot confirm delivery.
    var lateDown = try started(deadline: 1000, now: 10)
    try check(!observe(&lateDown.0, type: .keyDown, stamp: 700, now: 160))
    try check(lateDown.0.phase == .poisoned)
    var lateUp = try started(deadline: 1000, now: 10)
    try check(observe(&lateUp.0, type: .keyDown, stamp: 710, now: 11))
    try check(observe(&lateUp.0, type: .keyUp, stamp: 711, now: 160))
    try check(!lateUp.0.isAcknowledged)
    try check(lateUp.0.phase == .poisoned)

    // Foreign, modifier-transition, and tap-disabled events invalidate an
    // active receipt before the ordinary classifier reports human/disabled.
    var foreign = try started()
    try check(observe(&foreign.0, type: .keyDown, stamp: 800))
    try check(!observe(&foreign.0, nonce: 9002, type: .keyUp, stamp: 801))
    try check(foreign.0.phase == .drainOnly)
    try check(observe(&foreign.0, type: .keyUp, stamp: 802, now: 12))
    try check(!foreign.0.isAcknowledged)
    var foreignBeforeDown = try started()
    try check(!observe(&foreignBeforeDown.0, nonce: 9002, type: .keyUp, stamp: 805))
    try check(foreignBeforeDown.0.phase == .poisoned)
    var modifierTransition = try started()
    try check(observe(&modifierTransition.0, type: .keyDown, stamp: 810))
    try check(!observe(&modifierTransition.0, type: .flagsChanged, flags: CGEventFlags.maskShift.rawValue,
        stamp: 811))
    try check(modifierTransition.0.phase == .drainOnly)
    try check(observe(&modifierTransition.0, type: .keyUp, stamp: 812, now: 12))
    try check(!modifierTransition.0.isAcknowledged)
    var disabledTap = try started()
    try check(observe(&disabledTap.0, type: .keyDown, stamp: 820))
    try check(!observe(&disabledTap.0, type: .tapDisabledByTimeout, stamp: 821))
    try check(disabledTap.0.phase == .drainOnly)
    try check(observe(&disabledTap.0, type: .keyUp, stamp: 822, now: 12))
    try check(!disabledTap.0.isAcknowledged)
    var userDisabledTap = try started()
    try check(observe(&userDisabledTap.0, type: .keyDown, stamp: 830))
    try check(!observe(&userDisabledTap.0, type: .tapDisabledByUserInput, stamp: 831))
    try check(userDisabledTap.0.phase == .drainOnly)
    try check(observe(&userDisabledTap.0, type: .keyUp, stamp: 832, now: 12))
    try check(!userDisabledTap.0.isAcknowledged)

    // An unknown result may drain one exact late up after a recorded down,
    // but it never becomes an acknowledgement and is bounded by post+250 ms.
    var drainOnly = try started()
    let drainStamp = drainOnly.1
    try check(observe(&drainOnly.0, type: .keyDown, stamp: 900))
    drainOnly.0.markDrainOnly(stamp: drainStamp)
    try check(drainOnly.0.phase == .drainOnly && drainOnly.0.drainUntil == 260)
    try check(observe(&drainOnly.0, type: .keyUp, stamp: 901, now: 12))
    try check(!drainOnly.0.isAcknowledged)
    try check(!observe(&drainOnly.0, type: .keyUp, stamp: 901, now: 13))
    var lateDrain = try started()
    try check(observe(&lateDrain.0, type: .keyDown, stamp: 910))
    lateDrain.0.markDrainOnly(stamp: lateDrain.1)
    try check(!observe(&lateDrain.0, type: .keyUp, stamp: 911, now: 260))

    // A later UNKNOWN result also revokes an already observed up, retaining
    // only one bounded duplicate-up mapping and no finalizable receipt.
    var afterUpUnknown = try started(nonce: 9013)
    try check(observe(&afterUpUnknown.0, nonce: 9013, type: .keyDown, stamp: 920))
    try check(observe(&afterUpUnknown.0, nonce: 9013, type: .keyUp, stamp: 921, now: 12))
    afterUpUnknown.0.markDrainOnly(stamp: afterUpUnknown.1)
    try check(afterUpUnknown.0.phase == .idle)
    try check(!afterUpUnknown.0.acknowledged(stamp: afterUpUnknown.1,
        cutoff: afterUpUnknown.0.acknowledgementCutoff, now: 13))
    try check(observe(&afterUpUnknown.0, nonce: 9013, type: .keyUp, stamp: 921, now: 13))
    try check(!observe(&afterUpUnknown.0, nonce: 9013, type: .keyUp, stamp: 921, now: 14))

    // A completed receipt retains one exact duplicate-up mapping for the
    // bounded drain interval. Replays and malformed duplicates fail closed.
    var retired = try started(nonce: 9003)
    try check(observe(&retired.0, nonce: 9003, type: .keyDown, stamp: 1000))
    try check(observe(&retired.0, nonce: 9003, type: .keyUp, stamp: 1001, now: 12))
    retired.0.finish(stamp: retired.1)
    try check(retired.0.phase == .idle)
    try check(observe(&retired.0, nonce: 9003, type: .keyUp, stamp: 1001, now: 13))
    try check(!observe(&retired.0, nonce: 9003, type: .keyUp, stamp: 1001, now: 14))
    var retiredDuringNext = try started(nonce: 9010)
    try check(observe(&retiredDuringNext.0, nonce: 9010, type: .keyDown, stamp: 1020))
    try check(observe(&retiredDuringNext.0, nonce: 9010, type: .keyUp, stamp: 1021, now: 12))
    retiredDuringNext.0.finish(stamp: retiredDuringNext.1)
    guard let nextStamp = retiredDuringNext.0.begin(proposedStamp: 200, code: 49, flags: 0,
            nonce: 9011, eventMarker: 123),
          retiredDuringNext.0.postsStarted(stamp: nextStamp, nonce: 9011, originalDeadline: 500, now: 30) else {
        throw KeyReceiptStateTestFailure(check: passed + 1)
    }
    try check(observe(&retiredDuringNext.0, nonce: 9010, type: .keyUp, stamp: 1021, now: 31))
    try check(retiredDuringNext.0.phase == .awaitingDown)
    try check(!retiredDuringNext.0.isAcknowledged)
    try check(!observe(&retiredDuringNext.0, nonce: 9010, type: .keyUp, stamp: 1021, now: 32))
    try check(retiredDuringNext.0.phase == .poisoned)
    try check(!retiredDuringNext.0.isAcknowledged)
    var retiredNonceReuse = try started(nonce: 9012)
    try check(observe(&retiredNonceReuse.0, nonce: 9012, type: .keyDown, stamp: 1030))
    try check(observe(&retiredNonceReuse.0, nonce: 9012, type: .keyUp, stamp: 1031, now: 12))
    retiredNonceReuse.0.finish(stamp: retiredNonceReuse.1)
    try check(retiredNonceReuse.0.begin(proposedStamp: 200, code: 49, flags: 0,
        nonce: 9012, eventMarker: 123) == nil)
    var retiredExpired = try started(nonce: 9004)
    try check(observe(&retiredExpired.0, nonce: 9004, type: .keyDown, stamp: 1010))
    try check(observe(&retiredExpired.0, nonce: 9004, type: .keyUp, stamp: 1011, now: 12))
    retiredExpired.0.finish(stamp: retiredExpired.1)
    try check(!observe(&retiredExpired.0, nonce: 9004, type: .keyUp, stamp: 1011, now: 260))

    // Overflow or an expired original deadline poisons the receipt instead of
    // wrapping either bounded window.
    var overflow = KeyReceiptState()
    guard let overflowStamp = overflow.begin(proposedStamp: 100, code: 49, flags: 0,
            nonce: 9005, eventMarker: 123) else { throw KeyReceiptStateTestFailure(check: passed + 1) }
    try check(!overflow.postsStarted(stamp: overflowStamp, nonce: 9005,
        originalDeadline: UInt64.max, now: UInt64.max - 100))
    try check(overflow.phase == .poisoned)
    var elapsed = KeyReceiptState()
    guard let elapsedStamp = elapsed.begin(proposedStamp: 100, code: 49, flags: 0,
            nonce: 9006, eventMarker: 123) else { throw KeyReceiptStateTestFailure(check: passed + 1) }
    try check(!elapsed.postsStarted(stamp: elapsedStamp, nonce: 9006, originalDeadline: 10, now: 10))
    try check(elapsed.phase == .poisoned)
    var exhausted = KeyReceiptState()
    guard exhausted.begin(proposedStamp: UInt64.max, code: 49, flags: 0,
            nonce: 9007, eventMarker: 123) == UInt64.max else { throw KeyReceiptStateTestFailure(check: passed + 1) }
    try check(exhausted.begin(proposedStamp: 1, code: 49, flags: 0, nonce: 9008, eventMarker: 123) == nil)

    // The ordered final gate independently rejects a stop revision change,
    // lost coverage, closure, isolation and every strict deadline boundary.
    struct FinalGateCase {
        var receipt = true
        var closing = false
        var isolated = false
        var accessibility = true
        var humanInput = true
        var ticketRevision: UInt64 = 7
        var stopRevision: UInt64 = 7
        var now: UInt64 = 99
        var cutoff: UInt64 = 100
        var ticketDeadline: UInt64 = 100
        var hostDeadline: UInt64 = 100
        var supervisorDeadline: UInt64 = 100
    }
    func finalGate(_ value: FinalGateCase = FinalGateCase()) -> Bool {
        keyReceiptFinalizationAllowed(receiptAcknowledged: value.receipt,
            closing: value.closing, isolated: value.isolated,
            accessibility: value.accessibility, humanInput: value.humanInput,
            ticketRevision: value.ticketRevision, stopRevision: value.stopRevision,
            now: value.now, cutoff: value.cutoff, ticketDeadline: value.ticketDeadline,
            hostDeadline: value.hostDeadline, supervisorDeadline: value.supervisorDeadline)
    }
    try check(finalGate())
    try check(!finalGate(FinalGateCase(receipt: false)))
    try check(!finalGate(FinalGateCase(closing: true)))
    try check(!finalGate(FinalGateCase(isolated: true)))
    try check(!finalGate(FinalGateCase(accessibility: false)))
    try check(!finalGate(FinalGateCase(humanInput: false)))
    try check(!finalGate(FinalGateCase(ticketRevision: 6)))
    try check(!finalGate(FinalGateCase(now: 100)))
    try check(!finalGate(FinalGateCase(cutoff: 98)))
    try check(!finalGate(FinalGateCase(ticketDeadline: 98)))
    try check(!finalGate(FinalGateCase(hostDeadline: 98)))
    try check(!finalGate(FinalGateCase(supervisorDeadline: 98)))
    try check(!keyReceiptVisibleAcknowledgement(true, droppingReceipt: true))
    try check(keyReceiptVisibleAcknowledgement(true, droppingReceipt: false))
    try check(keyReceiptDrainPending(until: 250, upObserved: false, now: 249))
    try check(!keyReceiptDrainPending(until: 250, upObserved: false, now: 250))
    try check(!keyReceiptDrainPending(until: 250, upObserved: true, now: 249))
    return passed
}
