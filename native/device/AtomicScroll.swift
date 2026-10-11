import ApplicationServices
import Darwin
import Dispatch
import Foundation

/// One bounded, pixel-based wheel event. No cursor movement, modifier event,
/// phase stream, momentum stream, held input or automatic replay is emitted.
enum AtomicScrollProfile {
    static let id = "macos-atomic-scroll@1"
    static let deliveryAckMS: UInt64 = 150
    static let maximumDelta: Int32 = 120
}

struct AtomicScrollEvent {
    let source: CGEventSource
    let event: CGEvent

    init?(request: NativeProbe, marker: Int64) {
        guard request.valid, request.action == "scroll", request.mouse == nil, request.value == nil,
              let scroll = request.scroll else { return nil }
        self.init(dx: scroll.dx, dy: scroll.dy, marker: marker)
    }

    init?(dx: Int32, dy: Int32, marker: Int64) {
        guard marker != 0,
              (-AtomicScrollProfile.maximumDelta...AtomicScrollProfile.maximumDelta).contains(dx),
              (-AtomicScrollProfile.maximumDelta...AtomicScrollProfile.maximumDelta).contains(dy),
              dx != 0 || dy != 0,
              let source = CGEventSource(stateID: .privateState) else { return nil }
        source.userData = marker
        source.localEventsSuppressionInterval = 0
        guard let event = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                                  wheel1: dy, wheel2: dx, wheel3: 0) else { return nil }
        event.flags = []
        event.setIntegerValueField(.eventSourceUserData, value: marker)
        event.setIntegerValueField(.scrollWheelEventScrollPhase, value: 0)
        event.setIntegerValueField(.scrollWheelEventMomentumPhase, value: 0)
        self.source = source; self.event = event
    }

    func post() {
        withExtendedLifetime(source) { event.post(tap: .cghidEventTap) }
    }
}

/// A scroll-wheel constructor has no cursor-position argument. The existing
/// cursor must already occupy the authority's point, so both the actual cursor
/// and the unmodified event location are checked. This reads state only: it
/// never warps the cursor, posts a move, or substitutes a fabricated location.
func scrollCursorMatches(_ request: NativeProbe, event: CGEvent) -> Bool {
    guard let cursor = CGEvent(source: nil)?.location else { return false }
    return [cursor, event.location].allSatisfy {
        $0.x.isFinite && $0.y.isFinite && $0.x == request.x && $0.y == request.y &&
            request.original_proof.display_frame.contains(x: $0.x, y: $0.y)
    }
}

func atomicScroll(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: "native scroll prerequisites unavailable")
    guard CGPreflightPostEventAccess(), mouseButtonsClear(), let hit = ticket.request.hit,
          let scroll = AtomicScrollEvent(request: ticket.request, marker: state.eventMarker) else { return response }
    guard scrollCursorMatches(ticket.request, event: scroll.event) else {
        response.reason = "current cursor does not match the authorized scroll point"; return response
    }
    // The existing delivery tracker accepts a type and timestamp. For this
    // indivisible event, scrollWheel is the final event rather than mouse-up.
    guard let stamp = state.beginPointerDelivery(proposedStamp: DispatchTime.now().uptimeNanoseconds,
                                                 upType: CGEventType.scrollWheel.rawValue) else { return response }
    scroll.event.timestamp = stamp
    var postingStarted = false
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true,
                                  deadlineMS: ticket.deadline, action: "scroll") { _ in
        guard CGPreflightPostEventAccess(), mouseButtonsClear(),
              scrollCursorMatches(ticket.request, event: scroll.event) else { return }
        response = state.commitPress(ticket) {
            // Cursor changes are independent of the stop/control lock. Close
            // the final race conservatively before the only posting call.
            guard CGPreflightPostEventAccess(), mouseButtonsClear(),
                  scrollCursorMatches(ticket.request, event: scroll.event) else { return false }
            postingStarted = true
            state.pointerPostsStarted(stamp: stamp)
            scroll.post()
            return true
        }
    }
    if !response.submitted && response.reason == "native scroll prerequisites unavailable" {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "scroll posting prerequisites changed" : result.reason
    } else if response.submitted && !postingStarted {
        // The intent was consumed by the ordered gate but no input was posted.
        // This conservative terminal result still must never be replayed.
        response.outcome = "unknown"; response.reason = "scroll prerequisites changed at commit; no event posted; do not replay"
    } else if response.outcome == "applied" {
        let end = min(ticket.deadline, monotonicMS() + AtomicScrollProfile.deliveryAckMS)
        while !state.pointerUpObserved(stamp: stamp), monotonicMS() < end { usleep(1000) }
        if state.pointerUpObserved(stamp: stamp) {
            response.reason = "scroll event observed at session tap; UI effect unverified"
        } else {
            response.outcome = "unknown"; response.reason = "posted scroll event was not observed; do not replay"
        }
    }
    return state.finalizeInputReceipt(ticket, response: response)
}
