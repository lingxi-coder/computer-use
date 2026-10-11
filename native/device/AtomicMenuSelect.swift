import ApplicationServices

func atomicAnchoredShowMenu(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let unavailable = "native anchored menu opening unavailable"
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
                                        outcome: "context_unresolved", reason: unavailable)
    guard ticket.request.action == "ax_show_menu", ticket.request.valid,
          case .opener(let anchor, _) = ticket.request.current_proof.menu_scope, let hit = ticket.request.hit else { return response }
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true, deadlineMS: ticket.deadline,
                                  action: "ax_show_menu", menuAnchor: anchor) { opener in
        let now = monotonicMS()
        guard now < ticket.deadline,
              AXUIElementSetMessagingTimeout(opener, Float(min(25, ticket.deadline - now)) / 1000) == .success else { return }
        response = state.commitPress(ticket) { AXUIElementPerformAction(opener, kAXShowMenuAction as CFString) == .success }
    }
    if !response.submitted && response.reason == unavailable {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "menu opener action timeout unavailable" : result.reason
    }
    return response
}

func atomicMenuPress(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let unavailable = "native menu selection unavailable"
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
                                        outcome: "context_unresolved", reason: unavailable)
    guard ticket.request.action == "ax_menu_press", ticket.request.valid else { response.outcome = "rejected"; return response }
    if let invalid = state.probeValidity(ticket) { response.outcome = invalid; return response }
    let result = diagnosticMenuHitTest(ticket, epoch: state.epoch) { item in
        let now = monotonicMS()
        guard now < ticket.deadline,
              AXUIElementSetMessagingTimeout(item, Float(min(25, ticket.deadline - now)) / 1000) == .success else { return }
        response = state.commitPress(ticket) { AXUIElementPerformAction(item, kAXPressAction as CFString) == .success }
    }
    if !response.submitted && response.reason == unavailable {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "menu action timeout unavailable" : result.reason
    }
    return response
}
