import AppKit
import ApplicationServices

/// The only Native input primitive for the local TableViewportV1 proof. The
/// validated AXRow remains on this worker; the commit gate encloses exactly
/// one AXSelected=true setter call and the result cannot be replayed.
func atomicSetSelectedTrue(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let request = ticket.request
    var response = NativeSubmitResponse(session_epoch: state.epoch,
        request_id: request.request_id, outcome: "context_unresolved",
        reason: "row selection context unavailable")
    guard request.action == "ax_set_selected_true", request.valid,
          request.original_proof == request.current_proof,
          request.original_proof.menu_scope == nil, request.original_proof.focus_scope == nil,
          let hit = request.hit, request.context_witness.tableViewport != nil else {
        response.outcome = "rejected"
        response.reason = "row selection proof or action shape is invalid"
        return response
    }

    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true,
        deadlineMS: ticket.deadline, action: request.action) { row in
        let now = monotonicMS()
        guard now < ticket.deadline,
              AXUIElementSetMessagingTimeout(row,
                  Float(min(25, ticket.deadline - now)) / 1000) == .success else { return }
        // The preflight has already made two independent worker-local reads.
        // No AX query or second mutation occurs inside this submit fragment.
        var rowTrace: NativeRowTrace?
        response = state.commitPress(ticket) {
            var axCallCount: UInt8 = 0
            let callStarted = DispatchTime.now().uptimeNanoseconds
            axCallCount += 1
            let error = AXUIElementSetAttributeValue(row,
                kAXSelectedAttribute as CFString, kCFBooleanTrue)
            let callFinished = DispatchTime.now().uptimeNanoseconds
            rowTrace = NativeRowTrace.called(error: error, started: callStarted,
                finished: callFinished)
            return axCallCount == 1 && error == .success
        }
        // commitPress can refuse before entering the closure when Stop wins.
        // In that case the absent trace records no claim about an AX call.
        response.row_trace = rowTrace
    }

    if !response.submitted && response.reason == "row selection context unavailable" {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched"
            ? "row setter timeout could not be set" : result.reason
    }
    // A post-commit Unknown is final. Never infer selection or retry it here.
    return state.finalizeInputReceipt(ticket, response: response)
}
