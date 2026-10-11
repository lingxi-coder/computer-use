import ApplicationServices
import Darwin
import Dispatch
import Foundation

#if CU_OWNED_POINTER_DEATH_POINTS
// Separately built acceptance helper only. There is no runtime IPC switch.
// The helper's executable directory is owned by the fixture runner.
enum OwnedPointerDeathPoint {
    static func pause(_ point: String, closed: (() -> Bool)? = nil) {
        let executable = URL(fileURLWithPath: CommandLine.arguments[0])
        guard executable.lastPathComponent == "owned-" + point else { return }
        let parent = getppid()
        let value: [String: Any] = ["point": point, "helper_pid": getpid(), "device_pid": parent]
        guard let data = try? JSONSerialization.data(withJSONObject: value),
              (try? data.write(to: executable.deletingLastPathComponent().appendingPathComponent("death-point.json"), options: .atomic)) != nil else { _exit(126) }
        // Stay at the named boundary until this exact parent has died. A
        // timeout is a failed injection, never a successful boundary test.
        let deadline = monotonicMS() + 250
        while monotonicMS() < deadline {
            if getppid() != parent && (closed?() ?? true) { return }
            usleep(1000)
        }
        _exit(126)
    }
}
#endif

/// Frozen control profile macos-atomic-click@1: a single point, one click,
/// zero delay between posts, no modifier down/up and no hardware suppression.
enum AtomicClickProfile {
    static let id = "macos-atomic-click@1"
    static let releaseGapMS: UInt64 = 0
    static let releaseAckMS: UInt64 = 150
}

func mouseButtonsClear() -> Bool {
    [CGMouseButton.left, .right, .center].allSatisfy { button in
        !CGEventSource.buttonState(.combinedSessionState, button: button)
            && !CGEventSource.buttonState(.hidSystemState, button: button)
    }
}

struct AtomicMousePair {
    let source: CGEventSource
    let down: CGEvent, up: CGEvent
    init?(request: NativeProbe, marker: Int64) {
        guard request.valid, request.action == "click", marker != 0, let mouse = request.mouse,
              let source = CGEventSource(stateID: .privateState) else { return nil }
        source.userData = marker
        source.localEventsSuppressionInterval = 0
        let button: CGMouseButton, downType: CGEventType, upType: CGEventType
        switch mouse.button {
        case "left": button = .left; downType = .leftMouseDown; upType = .leftMouseUp
        case "right": button = .right; downType = .rightMouseDown; upType = .rightMouseUp
        case "middle": button = .center; downType = .otherMouseDown; upType = .otherMouseUp
        default: return nil
        }
        let point = CGPoint(x: request.x, y: request.y)
        guard let down = CGEvent(mouseEventSource: source, mouseType: downType, mouseCursorPosition: point, mouseButton: button),
              let up = CGEvent(mouseEventSource: source, mouseType: upType, mouseCursorPosition: point, mouseButton: button) else { return nil }
        for event in [down, up] {
            event.flags = mouse.modifiers.flags
            event.setIntegerValueField(.eventSourceUserData, value: marker)
            event.setIntegerValueField(.mouseEventClickState, value: 1)
        }
        self.source = source; self.down = down; self.up = up
    }
    func post() {
        // Both calls run under the same ordered commit gate. Once down starts,
        // always post up; stop/expiry cannot split this atomic pair.
        withExtendedLifetime(source) {
            down.post(tap: .cghidEventTap)
            #if CU_OWNED_POINTER_DEATH_POINTS
            OwnedPointerDeathPoint.pause("between-down-up")
            #endif
            up.post(tap: .cghidEventTap)
            #if CU_OWNED_POINTER_DEATH_POINTS
            OwnedPointerDeathPoint.pause("after-up")
            #endif
        }
    }
}

func atomicClick(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: "native pointer click unavailable")
    guard CGPreflightPostEventAccess(), mouseButtonsClear(), let hit = ticket.request.hit,
          let pair = AtomicMousePair(request: ticket.request, marker: state.eventMarker) else { return response }
    guard let stamp = state.beginPointerDelivery(proposedStamp: DispatchTime.now().uptimeNanoseconds, upType: pair.up.type.rawValue) else { return response }
    pair.down.timestamp = stamp; pair.up.timestamp = stamp
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true, deadlineMS: ticket.deadline, action: "click") { _ in
        guard CGPreflightPostEventAccess(), mouseButtonsClear() else { return }
        #if CU_OWNED_POINTER_DEATH_POINTS
        OwnedPointerDeathPoint.pause("before-down", closed: { state.inputClosed })
        #endif
        response = state.commitPress(ticket) {
            state.pointerPostsStarted(stamp: stamp)
            pair.post()
            // CGEventPost has no delivery receipt. Applied means both calls
            // returned; subsequent source evidence must prove the UI effect.
            return true
        }
    }
    if !response.submitted && response.reason == "native pointer click unavailable" {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "pointer posting prerequisites unavailable" : result.reason
    }
    if response.submitted && response.outcome == "applied" {
        // The OS post calls enqueue input. Keep this helper and its tap alive
        // until its matching release has passed through the session event tap.
        // No AX query, control lock or extra input is held while waiting.
        let end = min(ticket.deadline, monotonicMS() + AtomicClickProfile.releaseAckMS)
        while !state.pointerUpObserved(stamp: stamp), monotonicMS() < end { usleep(1000) }
        if !state.pointerUpObserved(stamp: stamp) {
            response.outcome = "unknown"; response.reason = "posted pointer release was not observed; do not replay"
        } else {
            response.reason = "pointer release observed at session tap; UI effect unverified"
        }
    }
    return state.finalizeInputReceipt(ticket, response: response)
}
