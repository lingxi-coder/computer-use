import AppKit
import ApplicationServices

/// ShowMenu invokes one declared native action on the worker-local matched
/// element. A successful AX return proves neither that a menu became visible
/// nor that its auxiliary menu window belongs to the original window scope.
/// Those facts require a new source proof before any subsequent menu input.
func atomicShowMenu(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let unavailable = "native AX show menu unavailable"
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: unavailable)
    guard ticket.request.action == "ax_show_menu", let hit = ticket.request.hit else {
        response.outcome = "rejected"; response.reason = "unsupported atomic menu primitive"; return response
    }
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true,
        deadlineMS: ticket.deadline, action: "ax_show_menu") { element in
        func boundTimeout() -> Bool {
            let now = monotonicMS()
            return now < ticket.deadline &&
                AXUIElementSetMessagingTimeout(element, Float(min(25, ticket.deadline - now)) / 1000) == .success
        }
        // The final hit/fp1, focused app/window, enabled state and declared
        // action are checked outside the ordered gate. No AX ref escapes this
        // closure, and no AX query is performed while holding the guard lock.
        guard boundTimeout() else { response.reason = "show menu timeout could not be set"; return }
        var actions: CFArray?
        guard AXUIElementCopyActionNames(element, &actions) == .success,
              let names = actions as? [String], names.count <= 16, names.contains(kAXShowMenuAction),
              AXIsProcessTrusted() else { response.reason = "declared show menu action unavailable"; return }
        guard boundTimeout() else { response.reason = "show menu deadline expired before commit"; return }
        response = state.commitPress(ticket) {
            // The gate rechecks scope, original deadline, stop revision and
            // both leases. Any non-success after this single call starts is
            // unknown. AppKit may track synchronously until menu dismissal;
            // the independent worker watchdog remains the final hard bound.
            AXUIElementPerformAction(element, kAXShowMenuAction as CFString) == .success
        }
    }
    if !response.submitted && response.reason == unavailable {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "show menu prerequisites unavailable" : result.reason
    }
    return response
}
