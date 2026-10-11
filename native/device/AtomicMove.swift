import ApplicationServices
import Darwin
import Dispatch
import Foundation

/// One absolute mouseMoved event. No button transition, modifier event, drag,
/// cursor warp or follow-up correction belongs to this profile.
enum AtomicMoveProfile {
    static let id = "macos-atomic-pointer-move@1"
    static let deliveryAckMS: UInt64 = 150

    static func confirmationDeadline(originalDeadline: UInt64, postedMS: UInt64) -> UInt64 {
        let (ackDeadline, overflow) = postedMS.addingReportingOverflow(deliveryAckMS)
        return min(originalDeadline, overflow ? UInt64.max : ackDeadline)
    }
}

func atomicMoveConfirmed(observed: Bool, cursorMatched: Bool, readStartedMS: UInt64,
                         readFinishedMS: UInt64, deadlineMS: UInt64) -> Bool {
    observed && cursorMatched && readStartedMS <= readFinishedMS && readFinishedMS < deadlineMS
}

struct AtomicMoveEvent {
    let source: CGEventSource
    let event: CGEvent

    init?(request: NativeProbe, marker: Int64) {
        guard request.valid, request.action == "pointer_move", request.mouse == nil,
              request.value == nil, request.scroll == nil else { return nil }
        self.init(x: request.x, y: request.y, marker: marker)
    }

    init?(x: Double, y: Double, marker: Int64) {
        guard marker != 0, x.isFinite, y.isFinite, abs(x) <= 100_000, abs(y) <= 100_000,
              let source = CGEventSource(stateID: .privateState) else { return nil }
        source.userData = marker; source.localEventsSuppressionInterval = 0
        guard let event = CGEvent(mouseEventSource: source, mouseType: .mouseMoved,
                                  mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left) else { return nil }
        event.flags = []
        event.setIntegerValueField(.eventSourceUserData, value: marker)
        event.setIntegerValueField(.mouseEventClickState, value: 0)
        self.source = source; self.event = event
    }

    func post() {
        withExtendedLifetime(source) { event.post(tap: .cghidEventTap) }
    }
}

func movedCursorMatches(_ request: NativeProbe) -> Bool {
    guard let cursor = CGEvent(source: nil)?.location else { return false }
    return cursor.x.isFinite && cursor.y.isFinite && cursor.x == request.x && cursor.y == request.y &&
        request.original_proof.display_frame.contains(x: cursor.x, y: cursor.y)
}

func atomicMove(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: "native pointer move prerequisites unavailable")
    guard CGPreflightPostEventAccess(), mouseButtonsClear(), let hit = ticket.request.hit,
          let move = AtomicMoveEvent(request: ticket.request, marker: state.eventMarker) else { return response }
    guard let stamp = state.beginPointerDelivery(proposedStamp: DispatchTime.now().uptimeNanoseconds,
                                                 upType: CGEventType.mouseMoved.rawValue) else { return response }
    move.event.timestamp = stamp
    var postingStarted = false
    var postedMS: UInt64 = 0
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true,
                                  deadlineMS: ticket.deadline, action: "pointer_move") { _ in
        guard CGPreflightPostEventAccess(), mouseButtonsClear() else { return }
        response = state.commitPress(ticket) {
            // The stop/authority gate orders the single posting call. Recheck
            // button state here so a move never intentionally becomes a drag.
            guard CGPreflightPostEventAccess(), mouseButtonsClear() else { return false }
            postingStarted = true
            postedMS = monotonicMS()
            state.pointerPostsStarted(stamp: stamp)
            move.post()
            return true
        }
    }
    if !response.submitted && response.reason == "native pointer move prerequisites unavailable" {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "pointer move posting prerequisites changed" : result.reason
    } else if response.submitted && !postingStarted {
        response.outcome = "unknown"
        response.reason = "pointer move prerequisites changed at commit; no event posted; do not replay"
    } else if response.outcome == "applied" {
        // No lock or AX ref is held while observing the already-posted event.
        // The same bounded drain keeps the helper's tap alive on shutdown.
        let end = AtomicMoveProfile.confirmationDeadline(originalDeadline: ticket.deadline, postedMS: postedMS)
        var confirmed = false
        while monotonicMS() < end {
            let readStarted = monotonicMS()
            let observed = state.pointerUpObserved(stamp: stamp)
            let cursorMatched = observed && movedCursorMatches(ticket.request)
            let readFinished = monotonicMS()
            if atomicMoveConfirmed(observed: observed, cursorMatched: cursorMatched, readStartedMS: readStarted,
                                   readFinishedMS: readFinished, deadlineMS: end) {
                confirmed = true; break
            }
            if readFinished >= end { break }
            usleep(1000)
        }
        if !confirmed {
            response.outcome = "unknown"
            response.reason = "pointer move delivery and actual cursor were not confirmed within the original deadline; do not replay"
        } else {
            response.reason = "pointer move observed at session tap and actual cursor matched; UI effect unverified"
        }
    }
    return state.finalizeInputReceipt(ticket, response: response)
}

enum AtomicMoveConstructorFailure: Error { case failed(Int) }

/// Read-only constructor checks; constructing an event never posts it or moves
/// the cursor. Called only by the helper's explicitly requested self-test path.
func runAtomicMoveConstructorTests() throws -> Int {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw AtomicMoveConstructorFailure.failed(passed + 1) }
        passed += 1
    }
    try check(AtomicMoveProfile.id == "macos-atomic-pointer-move@1" && AtomicMoveProfile.deliveryAckMS == 150)
    try check(AtomicMoveProfile.confirmationDeadline(originalDeadline: 220, postedMS: 100) == 220)
    try check(AtomicMoveProfile.confirmationDeadline(originalDeadline: 400, postedMS: 100) == 250)
    try check(AtomicMoveProfile.confirmationDeadline(originalDeadline: 99, postedMS: 100) == 99)
    try check(AtomicMoveProfile.confirmationDeadline(originalDeadline: UInt64.max - 1, postedMS: UInt64.max - 10) == UInt64.max - 1)
    try check(atomicMoveConfirmed(observed: true, cursorMatched: true, readStartedMS: 199, readFinishedMS: 199, deadlineMS: 200))
    try check(!atomicMoveConfirmed(observed: true, cursorMatched: true, readStartedMS: 199, readFinishedMS: 200, deadlineMS: 200))
    try check(!atomicMoveConfirmed(observed: true, cursorMatched: true, readStartedMS: 200, readFinishedMS: 201, deadlineMS: 200))
    try check(!atomicMoveConfirmed(observed: true, cursorMatched: true, readStartedMS: 201, readFinishedMS: 199, deadlineMS: 200))
    try check(!atomicMoveConfirmed(observed: false, cursorMatched: true, readStartedMS: 199, readFinishedMS: 199, deadlineMS: 200))
    try check(!atomicMoveConfirmed(observed: true, cursorMatched: false, readStartedMS: 199, readFinishedMS: 199, deadlineMS: 200))
    for point in [CGPoint.zero, CGPoint(x: -1200.5, y: 320.25), CGPoint(x: 100_000, y: -100_000)] {
        guard let move = AtomicMoveEvent(x: point.x, y: point.y, marker: 123) else {
            throw AtomicMoveConstructorFailure.failed(passed + 1)
        }
        try check(move.event.type == .mouseMoved)
        try check(move.event.location == point)
        try check(move.event.flags.isEmpty)
        try check(move.event.getIntegerValueField(.eventSourceUserData) == 123)
        try check(move.event.getIntegerValueField(.mouseEventClickState) == 0)
        try check(move.source.localEventsSuppressionInterval == 0)
        move.event.timestamp = 100
        try check(move.event.timestamp == 100)
    }
    try check(AtomicMoveEvent(x: 1, y: 1, marker: 0) == nil)
    for invalid in [Double.nan, .infinity, -.infinity, -100_000.01, 100_000.01] {
        try check(AtomicMoveEvent(x: invalid, y: 0, marker: 123) == nil)
        try check(AtomicMoveEvent(x: 0, y: invalid, marker: 123) == nil)
    }
    return passed
}
