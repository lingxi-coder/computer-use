import AppKit
import ApplicationServices

/// The optional process-birth arguments are accepted only as one canonical
/// pair appended after a fixed-window execution scope.
enum NativeExecutionBirthStampArguments {
    enum ParseResult: Equatable {
        case absent
        case expected(DeviceProcessStamp)
        case invalid
    }

    static let maximumSafeSeconds: UInt64 = 9_007_199_254_740_991

    static func parse(_ trailingArguments: [String]) -> ParseResult {
        guard !trailingArguments.isEmpty else { return .absent }
        guard trailingArguments.count == 4,
              trailingArguments[0] == "--execute-start-seconds",
              trailingArguments[2] == "--execute-start-micros",
              let stamp = parseExpected(seconds: trailingArguments[1], micros: trailingArguments[3]) else {
            return .invalid
        }
        return .expected(stamp)
    }

    static func parseExpected(seconds: String, micros: String) -> DeviceProcessStamp? {
        guard let seconds = canonicalUnsigned(seconds), seconds > 0, seconds <= maximumSafeSeconds,
              let micros = canonicalUnsigned(micros), micros < 1_000_000 else { return nil }
        return DeviceProcessStamp(seconds: seconds, micros: micros)
    }

    static func matches(expected: DeviceProcessStamp?, actual: DeviceProcessStamp) -> Bool {
        guard let expected else { return true }
        return expected == actual
    }

    static func parseBackgroundAX(_ trailingArguments: [String]) -> DeviceProcessStamp? {
        guard trailingArguments.count == 5,
              trailingArguments[4] == "--execute-background-ax",
              case .expected(let stamp) = parse(Array(trailingArguments.prefix(4))) else { return nil }
        return stamp
    }

    private static func canonicalUnsigned(_ value: String) -> UInt64? {
        let bytes = Array(value.utf8)
        guard !bytes.isEmpty,
              (bytes.count == 1 || bytes[0] != 48),
              bytes.allSatisfy({ $0 >= 48 && $0 <= 57 }) else { return nil }
        return UInt64(value)
    }
}

func ownedBackgroundAXDiagnosticModeEnabled(
    executablePath: String,
    bundleID: String,
    pid: Int32,
    windowID: String?,
    expectedStamp: DeviceProcessStamp?,
    actualStamp: DeviceProcessStamp,
    backgroundAX: Bool
) -> Bool {
    let basename = URL(fileURLWithPath: executablePath).lastPathComponent
    guard basename == "owned-background-guard",
          backgroundAX, bundleID == "com.lingxi.computer-user.guard-fixture", pid > 0,
          let windowID, windowID.hasPrefix("window-"),
          let windowNumber = UInt32(windowID.dropFirst("window-".count)), windowNumber > 0,
          windowID == "window-\(windowNumber)",
          let expectedStamp, expectedStamp == actualStamp,
          expectedStamp.seconds > 0,
          expectedStamp.seconds <= NativeExecutionBirthStampArguments.maximumSafeSeconds,
          expectedStamp.micros < 1_000_000 else { return false }
    return true
}

/// Fixed by the trusted launcher. A readonly session cannot arm itself over
/// the control pipe. PID reuse invalidates the scope even with the same bundle.
struct NativeExecutionScope: Sendable {
    let pid: Int32, bundle: String
    let window: String?
    let stamp: DeviceProcessStamp
    let backgroundAX: Bool
    let ownedBackgroundAXDiagnostics: Bool
    init?(pid: Int32, bundle: String, window: String? = nil, expectedStamp: DeviceProcessStamp? = nil,
          backgroundAX: Bool = false) {
        #if CU_OWNED_POINTER_DROP_ACK || CU_OWNED_POINTER_DEATH_POINTS
        guard bundle == "com.lingxi.computer-user.guard-fixture" else { return nil }
        #endif
        guard pid > 0, ["com.lingxi.computer-user.guard-fixture", "com.lingxi.computer-user.mvp-fixture", "com.apple.TextEdit"].contains(bundle), nativeID(bundle),
              (!backgroundAX || (window != nil && expectedStamp != nil)),
              let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.bundleIdentifier == bundle, let stamp = DeviceProcessStamp.read(pid) else { return nil }
        guard NativeExecutionBirthStampArguments.matches(expected: expectedStamp, actual: stamp) else { return nil }
        if let window {
            guard window.hasPrefix("window-"), let number = UInt32(window.dropFirst(7)), number > 0,
                  let list = CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
                  list.contains(where: { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid && ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == number }) else { return nil }
        }
        self.pid = pid; self.bundle = bundle; self.stamp = stamp; self.window = window
        self.backgroundAX = backgroundAX
        self.ownedBackgroundAXDiagnostics = ownedBackgroundAXDiagnosticModeEnabled(
            executablePath: CommandLine.arguments.first ?? "", bundleID: bundle, pid: pid,
            windowID: window, expectedStamp: expectedStamp, actualStamp: stamp,
            backgroundAX: backgroundAX)
    }
    func allows(_ request: NativeProbe) -> Bool {
        if backgroundAX {
            guard request.valid, request.execution_domain == "background_ax",
                  request.purpose == "background_ax_action", request.action == "ax_press",
                  let context = request.context_witness.backgroundAXV1,
                  let window,
                  context.app.pid == UInt32(pid), context.app.bundle_id == bundle,
                  context.window.window_id == window,
                  context.source.window.process.pid == UInt32(pid),
                  context.source.window.process.stamp == stamp,
                  context.source.valid(bundleID: bundle, pid: UInt32(pid),
                      windowID: window, frame: context.window.frame),
                  DeviceProcessStamp.read(pid) == stamp else { return false }
            return true
        }
        guard !request.claimsBackgroundAX else { return false }
        if request.action == "ax_set_selected_true" {
            guard request.valid, request.original_proof == request.current_proof,
                  request.original_proof.menu_scope == nil, request.original_proof.focus_scope == nil,
                  request.context_witness.tableViewport != nil,
                  request.context_witness.supportsAction(request.action),
                  request.original_proof.continuity.scope.supportsRowSelection(witness: request.context_witness),
                  request.current_proof.continuity.scope.supportsRowSelection(witness: request.context_witness) else {
                return false
            }
        }
        if request.action == "key_press" {
            guard AtomicKeyProfile.acceptanceEnabled, bundle == "com.lingxi.computer-user.guard-fixture", window != nil,
                  request.original_proof.focus_scope != nil, request.original_proof.menu_scope == nil else { return false }
        }
        return ["ax_press", "ax_set_value", "ax_set_selected_true", "ax_show_menu", "ax_menu_press", "activate_window", "click", "scroll", "pointer_move", "key_press"].contains(request.action) &&
            (request.original_proof.menu_scope == nil || window != nil) &&
            (request.action != "activate_window" || window != nil) && request.original_proof.fingerprint.app_pid == UInt32(pid) &&
            request.original_proof.fingerprint.app == bundle && (window == nil || request.original_proof.fingerprint.window_id == window) && DeviceProcessStamp.read(pid) == stamp
    }

    func backgroundAXAuthorization(for request: NativeProbe) -> BackgroundAXAuthorization? {
        guard backgroundAX, allows(request), let context = request.context_witness.backgroundAXV1,
              let windowID = UInt32(context.window.window_id.dropFirst(7)) else { return nil }
        return BackgroundAXAuthorization(pid: pid, bundleID: bundle, birth: stamp,
            windowID: windowID, semanticScopeID: request.original_proof.binding,
            target: request.current_proof.fingerprint, semanticContext: context,
            sourceSequence: request.source_sequence, sourceObservedMS: request.observed_ms)
    }
}

struct ScopedWindowChildBoundaryEntry {
    let isAXElement: Bool
    let sameProcess: Bool
    let parentMatchesWindow: Bool
    let role: String?
}

struct ScopedWindowChildBoundaryFacts {
    var beforeCountError: Int32
    var beforeCount: Int?
    var copyError: Int32
    var rawIsArray: Bool
    var arrayCount: Int?
    var typedArrayCount: Int?
    var afterCountError: Int32
    var afterCount: Int?
    var referencesUnique: Bool
    var timely: Bool
    var children: [ScopedWindowChildBoundaryEntry]
}

private let scopedWindowKnownAXRoles: Set<String> = [
    "AXApplication", "AXSystemWide", "AXWindow", "AXSheet", "AXDrawer", "AXGrowArea",
    "AXImage", "AXButton", "AXRadioButton", "AXCheckBox", "AXPopUpButton", "AXMenuButton",
    "AXTabGroup", "AXTable", "AXColumn", "AXRow", "AXOutline", "AXBrowser", "AXScrollArea",
    "AXScrollBar", "AXRadioGroup", "AXList", "AXGroup", "AXValueIndicator", "AXComboBox",
    "AXSlider", "AXIncrementor", "AXBusyIndicator", "AXProgressIndicator", "AXRelevanceIndicator",
    "AXToolbar", "AXDisclosureTriangle", "AXTextField", "AXTextArea", "AXStaticText", "AXHeading",
    "AXMenuBar", "AXMenuBarItem", "AXMenu", "AXMenuItem", "AXSplitGroup", "AXSplitter",
    "AXColorWell", "AXTimeField", "AXDateField", "AXHelpTag", "AXMatte", "AXDockItem", "AXRuler",
    "AXRulerMarker", "AXGrid", "AXLevelIndicator", "AXCell", "AXLayoutArea", "AXLayoutItem",
    "AXHandle", "AXPopover",
]

func scopedWindowChildRoleIsAllowed(_ role: String?) -> Bool {
    guard let role, !role.isEmpty, role.utf8.count <= 128,
          scopedWindowKnownAXRoles.contains(role) else { return false }
    return !["AXSheet", "AXWindow", "AXDrawer"].contains(role)
}

func scopedWindowChildrenBoundaryIsValid(_ facts: ScopedWindowChildBoundaryFacts) -> Bool {
    let success = AXError.success.rawValue
    guard facts.timely, facts.beforeCountError == success,
          facts.copyError == success, facts.afterCountError == success,
          facts.rawIsArray, facts.referencesUnique,
          let before = facts.beforeCount, let array = facts.arrayCount,
          let typed = facts.typedArrayCount, let after = facts.afterCount,
          [before, array, typed, after].allSatisfy({ 0...64 ~= $0 }),
          before == array, array == typed, typed == after,
          facts.children.count == typed else { return false }
    return facts.children.allSatisfy { child in
        child.isAXElement && child.sameProcess && child.parentMatchesWindow
            && scopedWindowChildRoleIsAllowed(child.role)
    }
}

func scopedReferencesAreUnique<Element>(_ values: [Element], equal: (Element, Element) -> Bool) -> Bool {
    guard values.count > 1 else { return true }
    for left in 0..<(values.count - 1) {
        for right in (left + 1)..<values.count where equal(values[left], values[right]) { return false }
    }
    return true
}

func scopedReferenceSetsMatch<Element>(_ lhs: [Element], _ rhs: [Element], equal: (Element, Element) -> Bool) -> Bool {
    guard lhs.count == rhs.count else { return false }
    var unmatched = rhs
    for element in lhs {
        guard let index = unmatched.firstIndex(where: { equal(element, $0) }) else { return false }
        unmatched.remove(at: index)
    }
    return unmatched.isEmpty
}

private struct ScopedActivationWindowSample {
    let element: AXUIElement
    let children: [AXUIElement]
    let parent: AXUIElement?
    let parentRole: String?
    let parentSubrole: String?
    let role: String
    let subrole: String
    let title: String
    let frame: HitRect
    let modal: Bool
    let cgWindowID: UInt32?

    func sameLiveFact(as other: Self) -> Bool {
        let sameParent: Bool
        switch (parent, other.parent) {
        case (nil, nil): sameParent = true
        case let (lhs?, rhs?): sameParent = CFEqual(lhs, rhs)
        default: sameParent = false
        }
        return CFEqual(element, other.element) && sameParent
            && scopedReferenceSetsMatch(children, other.children, equal: { CFEqual($0, $1) })
            && parentRole == other.parentRole && parentSubrole == other.parentSubrole
            && role == other.role && subrole == other.subrole && title == other.title
            && frame == other.frame && modal == other.modal && cgWindowID == other.cgWindowID
    }
}

struct NativeSubmitResponse: Encodable, Sendable {
    let protocol_version = 1, kind = "atomic_submit"
    let session_epoch: UInt64, request_id: String
    var outcome: String, reason: String
    var submitted = false
    var committed_ms: UInt64? = nil
    var returned_ms: UInt64? = nil
    var binding_digest: String? = nil
    var row_trace: NativeRowTrace? = nil
    var ax_trace: NativeAXTrace? = nil
    var background_ax_trace: NativeBackgroundAXTrace? = nil
}

/// A private measurement of the one AXPress call. Uptime values use the
/// DispatchTime domain and do not imply a verified business effect.
struct NativeAXTrace: Codable, Sendable, Equatable {
    enum Primitive: String, Codable, Sendable {
        case axPress = "ax_press"
    }

    enum ClockDomain: String, Codable, Sendable {
        case dispatchTimeUptime = "dispatch_time_uptime"
    }

    let primitive: Primitive
    let ax_call_count: UInt8
    let ax_error_code: Int32?
    let clock_domain: ClockDomain
    let call_started_uptime_ns: UInt64?
    let call_finished_uptime_ns: UInt64?

    static func notCalled() -> Self {
        Self(primitive: .axPress, ax_call_count: 0,
            ax_error_code: nil, clock_domain: .dispatchTimeUptime,
            call_started_uptime_ns: nil, call_finished_uptime_ns: nil)
    }

    static func called(error: AXError, started: UInt64, finished: UInt64) -> Self {
        Self(primitive: .axPress, ax_call_count: 1,
            ax_error_code: error.rawValue, clock_domain: .dispatchTimeUptime,
            call_started_uptime_ns: started, call_finished_uptime_ns: finished)
    }

    var valid: Bool {
        switch (ax_call_count, ax_error_code, call_started_uptime_ns, call_finished_uptime_ns) {
        case (0, nil, nil, nil): true
        case (1, .some(_), let started?, let finished?):
            started > 0 && finished >= started && finished - started <= 1_000_000_000
        default: false
        }
    }
}

/// A private measurement of the one row-selection AX call. The uptime values
/// use DispatchTime's native uptime domain and are never mixed with Core time.
struct NativeRowTrace: Codable, Sendable, Equatable {
    enum Primitive: String, Codable, Sendable {
        case axSetSelectedTrue = "ax_set_selected_true"
    }

    enum ClockDomain: String, Codable, Sendable {
        case dispatchTimeUptime = "dispatch_time_uptime"
    }

    let primitive: Primitive
    let ax_call_count: UInt8
    let ax_error_code: Int32?
    let clock_domain: ClockDomain
    let call_started_uptime_ns: UInt64?
    let call_finished_uptime_ns: UInt64?

    static func notCalled() -> Self {
        Self(primitive: .axSetSelectedTrue, ax_call_count: 0,
            ax_error_code: nil, clock_domain: .dispatchTimeUptime,
            call_started_uptime_ns: nil, call_finished_uptime_ns: nil)
    }

    static func called(error: AXError, started: UInt64, finished: UInt64) -> Self {
        Self(primitive: .axSetSelectedTrue, ax_call_count: 1,
            ax_error_code: error.rawValue, clock_domain: .dispatchTimeUptime,
            call_started_uptime_ns: started, call_finished_uptime_ns: finished)
    }
}

/// The validated AX handle remains on this worker's stack. No matched/eligible
/// reply or serialized handle can be used as a later input token.
func atomicPress(_ ticket: NativeProbeTicket, state: GuardState, keyWitness: KeyFocusWitness? = nil) -> NativeSubmitResponse {
    if ticket.request.claimsBackgroundAX {
        return atomicBackgroundAXPress(ticket, state: state)
    }
    if ticket.request.action == "ax_set_selected_true" { return atomicSetSelectedTrue(ticket, state: state) }
    if ticket.request.action == "key_press" { return atomicKey(ticket, state: state, witness: keyWitness) }
    if ticket.request.action == "activate_window" { return atomicActivateWindow(ticket, state: state) }
    if ticket.request.action == "pointer_move" { return atomicMove(ticket, state: state) }
    if ticket.request.action == "click" { return atomicClick(ticket, state: state) }
    if ticket.request.action == "scroll" { return atomicScroll(ticket, state: state) }
    if ticket.request.action == "ax_menu_press" { return atomicMenuPress(ticket, state: state) }
    if ticket.request.action == "ax_show_menu" {
        var response = ticket.request.current_proof.menu_scope == nil ? atomicShowMenu(ticket, state: state) : atomicAnchoredShowMenu(ticket, state: state)
        if response.outcome == "applied", ticket.request.current_proof.menu_scope != nil,
           !state.recordMenuOpening(ticket, response: response) {
            response.outcome = "unknown"; response.reason = "menu opening audit unavailable after input; do not replay"
        }
        return state.finalizeInputReceipt(ticket, response: response)
    }
    var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: ticket.request.request_id,
        outcome: "context_unresolved", reason: "native AX press unavailable")
    guard ["ax_press", "ax_set_value"].contains(ticket.request.action), let hit = ticket.request.hit else {
        response.outcome = "rejected"; response.reason = "unsupported atomic primitive"; return response
    }
    var axPressTrace: NativeAXTrace?
    var commitAttempted = false
    let result = diagnosticHitTest(hit, epoch: state.epoch, requireActive: true, deadlineMS: ticket.deadline, action: ticket.request.action) { element in
        // Set the timeout before entering the ordered submit gate. No AX query
        // happens under that gate; it encloses only the one bounded action call.
        let now = monotonicMS()
        guard now < ticket.deadline,
              AXUIElementSetMessagingTimeout(element, Float(min(25, ticket.deadline - now)) / 1000) == .success else { return }
        commitAttempted = true
        response = state.commitPress(ticket) {
            if ticket.request.action == "ax_set_value", let value = ticket.request.value {
                let nativeValue: CFTypeRef = [kAXCheckBoxRole, kAXRadioButtonRole].contains(hit.binding.role) ? NSNumber(value: value == "on") : value as CFString
                return AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, nativeValue) == .success
            }
            let started = DispatchTime.now().uptimeNanoseconds
            let error = AXUIElementPerformAction(element, kAXPressAction as CFString)
            let finished = DispatchTime.now().uptimeNanoseconds
            axPressTrace = .called(error: error, started: started, finished: finished)
            return error == .success
        }
    }
    if ticket.request.action == "ax_press", commitAttempted {
        // A zero-call trace is known only when commitPress was attempted but
        // Stop prevented its closure from starting. Preflight failures remain
        // unobservable and therefore omit the optional trace.
        response.ax_trace = axPressTrace ?? .notCalled()
    }
    if !response.submitted && response.reason == "native AX press unavailable" {
        response.outcome = result.outcome == "matched" ? "context_unresolved" : result.outcome
        response.reason = result.outcome == "matched" ? "action timeout could not be set" : result.reason
    }
    // Once the call started, later stop/expiry cannot turn applied/unknown into
    // definitely rejected. Business effect must be verified by a new source.
    return response
}

/// BackgroundAX resolves a fixed semantic target from the authorized app and
/// window tree. This path never enters hit testing, frontmost checks, focus
/// reads, Raise, or any CGEvent fallback.
private func atomicBackgroundAXPress(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let request = ticket.request
    let diagnosticMode = state.ownedBackgroundAXDiagnosticsEnabled
    var trace = NativeBackgroundAXTrace()
    func refusal(_ reason: String, outcome: String = "context_unresolved") -> NativeSubmitResponse {
        var response = NativeSubmitResponse(session_epoch: state.epoch, request_id: request.request_id,
            outcome: outcome, reason: reason)
        if diagnosticMode { response.background_ax_trace = trace }
        return response
    }
    guard request.valid, request.execution_domain == "background_ax",
          request.purpose == "background_ax_action", request.action == "ax_press",
          AXIsProcessTrusted() else {
        return refusal("background AX purpose, fixed process grant, or accessibility permission unavailable",
                       outcome: request.valid ? "context_unresolved" : "rejected")
    }
    trace.stage = .processIdentity
    guard let authorization = state.backgroundAXAuthorization(for: request) else {
        return refusal("background AX purpose, fixed process grant, or accessibility permission unavailable")
    }
    let started = monotonicMS()
    guard started < ticket.deadline else {
        trace.stage = .preflight
        return refusal("background AX action deadline expired")
    }
    let (localEnd, overflow) = started.addingReportingOverflow(150)
    let deadline = overflow ? ticket.deadline : min(ticket.deadline, localEnd)
    let reader = BackgroundAXReader(authorization: authorization, deadlineMS: deadline)
    guard let initial = reader.readStableTarget(), monotonicMS() < deadline else {
        trace = reader.diagnosticTrace
        return refusal(reader.refusalReason)
    }
    trace = reader.diagnosticTrace

    var finalTarget: BackgroundAXCurrentTarget?
    var axTrace: NativeAXTrace?
    let response = state.commitPress(ticket, deadlineMS: deadline, preCommitRevalidate: {
        guard let latest = reader.revalidate(initial) else {
            trace = reader.diagnosticTrace
            return reader.refusalReason
        }
        trace = reader.diagnosticTrace
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(latest.target,
                  Float(min(BackgroundAXPolicy.maximumPerCallMilliseconds, deadline - now)) / 1000) == .success,
              monotonicMS() < deadline else { return "background AX action timeout could not be set before commit" }
        finalTarget = latest
        reader.markCommitReady()
        trace = reader.diagnosticTrace
        return nil
    }) {
        guard let latest = finalTarget else { return false }
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(latest.target,
                  Float(min(BackgroundAXPolicy.maximumPerCallMilliseconds, deadline - now)) / 1000) == .success,
              monotonicMS() < deadline else { return false }
        let callStarted = DispatchTime.now().uptimeNanoseconds
        let error = AXUIElementPerformAction(latest.target, kAXPressAction as CFString)
        let callFinished = DispatchTime.now().uptimeNanoseconds
        axTrace = .called(error: error, started: callStarted, finished: callFinished)
        return error == .success
    }
    var result = response
    trace = reader.diagnosticTrace
    trace.submitted = result.submitted
    if result.submitted { trace.stage = .submitted }
    if diagnosticMode { result.background_ax_trace = trace }
    if result.submitted { result.ax_trace = axTrace ?? .notCalled() }
    return result
}

/// Read-only BackgroundAX eligibility uses the same fixed semantic reader but
/// never consumes the request's intent or produces an input receipt.
func diagnosticBackgroundAX(_ ticket: NativeProbeTicket, state: GuardState, epoch: UInt64) -> NativeProbeResponse {
    let request = ticket.request
    func response(_ outcome: String, _ reason: String) -> NativeProbeResponse {
        NativeProbeResponse(session_epoch: epoch, request_id: request.request_id,
            outcome: outcome, reason: reason)
    }
    guard request.valid, request.claimsBackgroundAX, request.action == "ax_press",
          let authorization = state.backgroundAXAuthorization(for: request) else {
        return response(request.valid ? "context_unresolved" : "rejected",
            "background AX purpose, fixed process grant, or semantic source proof unavailable")
    }
    let started = monotonicMS()
    guard started < ticket.deadline else { return response("expired", "background AX probe deadline expired") }
    let (localEnd, overflow) = started.addingReportingOverflow(150)
    let deadline = overflow ? ticket.deadline : min(ticket.deadline, localEnd)
    let reader = BackgroundAXReader(authorization: authorization, deadlineMS: deadline)
    guard reader.readStableTarget() != nil, monotonicMS() < deadline else {
        return response("context_unresolved", reader.refusalReason)
    }
    var matched = response("eligible", "current BackgroundAX target matched; no input was sent")
    matched.binding_digest = request.binding_digest
    return matched
}

/// Focus changes use AX window identity, never a point in an occluding app.
/// Only the fixed authorized standard window can be raised. Its business
/// predicate (actually receiving focus) is established by subsequent AX data.
func atomicActivateWindow(_ ticket: NativeProbeTicket, state: GuardState) -> NativeSubmitResponse {
    let request = ticket.request, fp = request.original_proof.fingerprint
    func refusal(_ reason: String) -> NativeSubmitResponse {
        NativeSubmitResponse(session_epoch: state.epoch, request_id: request.request_id, outcome: "context_unresolved", reason: reason)
    }
    guard request.valid, request.action == "activate_window", AXIsProcessTrusted(),
          let app = NSRunningApplication(processIdentifier: Int32(fp.app_pid)), !app.isTerminated,
          app.bundleIdentifier == fp.app, let stamp = DeviceProcessStamp.read(Int32(fp.app_pid)),
          let number = UInt32(fp.window_id.dropFirst(7)) else { return refusal("window process unavailable") }
    let deadline = min(ticket.deadline, monotonicMS() + 150)
    let application = AXUIElementCreateApplication(Int32(fp.app_pid))
    guard AXUIElementSetMessagingTimeout(application, 0.025) == .success else { return refusal("window timeout unavailable") }
    func attribute(_ item: AXUIElement, _ key: String) -> CFTypeRef? {
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(item, Float(min(25, deadline - now)) / 1000) == .success else { return nil }
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(item, key as CFString, &value) == .success,
              let value, monotonicMS() < deadline else { return nil }
        return value
    }
    func rect(_ item: AXUIElement) -> HitRect? {
        guard let p = attribute(item, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = attribute(item, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
        return HitRect(CGRect(origin: point, size: size))
    }
    func nativeWindowID(_ frame: HitRect) -> UInt32? {
        guard monotonicMS() < deadline,
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        let matching = windows.filter {
            guard ($0[kCGWindowOwnerPID as String] as? NSNumber)?.uint32Value == fp.app_pid,
                  ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
                  let bounds = $0[kCGWindowBounds as String] as? [String: Any], let box = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
            return HitRect(box).matches(frame)
        }
        guard matching.count == 1 else { return nil }
        return (matching[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value
    }
    func selected(_ frame: HitRect) -> Bool {
        nativeWindowID(frame) == number
    }
    func currentActiveLayer() -> (known: Bool, value: String?) {
        guard app.isActive else { return (true, nil) }
        guard let rawFocused = attribute(application, kAXFocusedWindowAttribute),
              CFGetTypeID(rawFocused) == AXUIElementGetTypeID() else { return (false, nil) }
        let focused = rawFocused as! AXUIElement
        var owner: pid_t = 0
        guard AXUIElementSetMessagingTimeout(focused, 0.025) == .success,
              AXUIElementGetPid(focused, &owner) == .success, UInt32(owner) == fp.app_pid,
              let focusedFrame = rect(focused), let focusedID = nativeWindowID(focusedFrame) else { return (false, nil) }
        return (true, "window-\(focusedID)")
    }
    let scopedRoot = request.context_witness.windowRootV1
    func prepareScopedChildrenQuery(_ element: AXUIElement) -> Bool {
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(element, Float(min(25, deadline - now)) / 1000) == .success else { return false }
        return monotonicMS() < deadline
    }
    func readScopedWindowChildren(_ candidate: AXUIElement) -> [AXUIElement]? {
        guard prepareScopedChildrenQuery(candidate) else { return nil }
        var beforeCount = 0
        let beforeError = AXUIElementGetAttributeValueCount(candidate, kAXChildrenAttribute as CFString, &beforeCount)
        guard monotonicMS() < deadline, beforeError == .success, 0...64 ~= beforeCount else { return nil }

        guard prepareScopedChildrenQuery(candidate) else { return nil }
        var rawChildren: CFTypeRef?
        let copyError = AXUIElementCopyAttributeValue(candidate, kAXChildrenAttribute as CFString, &rawChildren)
        guard monotonicMS() < deadline, copyError == .success,
              let rawChildren, CFGetTypeID(rawChildren) == CFArrayGetTypeID() else { return nil }
        let childArray = rawChildren as! CFArray
        let arrayCount = CFArrayGetCount(childArray)
        guard 0...64 ~= arrayCount else { return nil }
        let rawValues = childArray as [AnyObject]
        guard rawValues.count <= 64, rawValues.count == arrayCount else { return nil }

        var children: [AXUIElement] = []
        var childFacts: [ScopedWindowChildBoundaryEntry] = []
        children.reserveCapacity(rawValues.count)
        childFacts.reserveCapacity(rawValues.count)
        for rawValue in rawValues {
            guard CFGetTypeID(rawValue) == AXUIElementGetTypeID() else { return nil }
            let child = unsafeDowncast(rawValue, to: AXUIElement.self)

            guard prepareScopedChildrenQuery(child) else { return nil }
            var owner: pid_t = 0
            guard AXUIElementGetPid(child, &owner) == .success,
                  monotonicMS() < deadline, owner > 0, UInt32(owner) == fp.app_pid else { return nil }

            guard prepareScopedChildrenQuery(child) else { return nil }
            var rawParent: CFTypeRef?
            guard AXUIElementCopyAttributeValue(child, kAXParentAttribute as CFString, &rawParent) == .success,
                  monotonicMS() < deadline, let rawParent,
                  CFGetTypeID(rawParent) == AXUIElementGetTypeID(), CFEqual(rawParent, candidate) else { return nil }

            guard prepareScopedChildrenQuery(child) else { return nil }
            var rawRole: CFTypeRef?
            guard AXUIElementCopyAttributeValue(child, kAXRoleAttribute as CFString, &rawRole) == .success,
                  monotonicMS() < deadline, let rawRole,
                  CFGetTypeID(rawRole) == CFStringGetTypeID(),
                  let role = rawRole as? String, scopedWindowChildRoleIsAllowed(role) else { return nil }

            children.append(child)
            childFacts.append(ScopedWindowChildBoundaryEntry(isAXElement: true,
                sameProcess: true, parentMatchesWindow: true, role: role))
        }

        guard prepareScopedChildrenQuery(candidate) else { return nil }
        var afterCount = 0
        let afterError = AXUIElementGetAttributeValueCount(candidate, kAXChildrenAttribute as CFString, &afterCount)
        let timely = monotonicMS() < deadline
        let facts = ScopedWindowChildBoundaryFacts(beforeCountError: beforeError.rawValue,
            beforeCount: beforeCount, copyError: copyError.rawValue, rawIsArray: true,
            arrayCount: arrayCount, typedArrayCount: children.count,
            afterCountError: afterError.rawValue, afterCount: afterCount,
            referencesUnique: scopedReferencesAreUnique(children, equal: { CFEqual($0, $1) }),
            timely: timely, children: childFacts)
        guard scopedWindowChildrenBoundaryIsValid(facts) else { return nil }
        return children
    }
    func hasUniqueWindowReferences(_ values: [AXUIElement]) -> Bool {
        guard values.count > 1 else { return true }
        for left in 0..<(values.count - 1) {
            for right in (left + 1)..<values.count where CFEqual(values[left], values[right]) {
                return false
            }
        }
        return true
    }
    guard let windows = attribute(application, kAXWindowsAttribute) as? [AXUIElement], !windows.isEmpty, windows.count <= 32 else { return refusal("window enumeration unavailable") }
    if scopedRoot != nil, !hasUniqueWindowReferences(windows) {
        return refusal("window enumeration contains duplicate roots")
    }
    let initialWindows = windows
    var initialWindowChildren: [(window: AXUIElement, children: [AXUIElement])] = []
    var matches: [AXUIElement] = []
    for window in windows {
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(window, Float(min(25, deadline - now)) / 1000) == .success,
              let rawModal = attribute(window, kAXModalAttribute),
              let modal = rawModal as? NSNumber, !modal.boolValue else { return refusal("modal or unknown window context") }
        if scopedRoot != nil, CFGetTypeID(rawModal) != CFBooleanGetTypeID() {
            return refusal("modal or unknown window context")
        }
        let role = attribute(window, kAXRoleAttribute) as? String
        let subrole = attribute(window, kAXSubroleAttribute) as? String
        if scopedRoot != nil {
            var owner: pid_t = 0
            guard monotonicMS() < deadline,
                  AXUIElementGetPid(window, &owner) == .success, UInt32(owner) == fp.app_pid,
                  monotonicMS() < deadline,
                  role == kAXWindowRole else { return refusal("window layer or sheet boundary unavailable") }
            if subrole == kAXStandardWindowSubrole {
                guard let children = readScopedWindowChildren(window) else { return refusal("window sheet boundary unavailable") }
                initialWindowChildren.append((window, children))
            } else if subrole == "AXDialog" {
                guard let children = readScopedWindowChildren(window) else { return refusal("dialog sheet boundary unavailable") }
                initialWindowChildren.append((window, children))
            } else {
                return refusal("unknown auxiliary window boundary")
            }
        } else if let sheets = attribute(window, "AXSheets") as? [AXUIElement], !sheets.isEmpty {
            return refusal("window has a sheet")
        }
        if role == kAXWindowRole, subrole == kAXStandardWindowSubrole,
           let frame = rect(window), frame.matches(request.original_proof.display_frame), selected(frame) { matches.append(window) }
    }
    guard matches.count == 1 else { return refusal("fixed window unavailable or ambiguous") }
    let window = matches[0]
    let contextReader = NativeBindingReader(pid: Int32(fp.app_pid), bundle: fp.app,
        windowID: number, window: window, deadline: deadline, allowedRoles: [kAXWindowRole])
    func sameWindowReferenceSet(_ lhs: [AXUIElement], _ rhs: [AXUIElement]) -> Bool {
        scopedReferenceSetsMatch(lhs, rhs, equal: { CFEqual($0, $1) })
    }
    func currentWindowChildrenMatchingInitial(_ candidate: AXUIElement) -> [AXUIElement]? {
        guard let initial = initialWindowChildren.first(where: { CFEqual($0.window, candidate) }),
              let children = readScopedWindowChildren(candidate),
              scopedReferenceSetsMatch(initial.children, children, equal: { CFEqual($0, $1) }) else { return nil }
        return children
    }
    func readScopedWindowInventory() -> [ScopedActivationWindowSample]? {
        guard let currentWindows = attribute(application, kAXWindowsAttribute) as? [AXUIElement],
              !currentWindows.isEmpty, currentWindows.count <= 32,
              hasUniqueWindowReferences(currentWindows),
              sameWindowReferenceSet(initialWindows, currentWindows) else { return nil }
        var samples: [ScopedActivationWindowSample] = []
        var matchedTarget = 0
        var auxiliaryDialogCount = 0
        var seenCGWindowIDs = Set<UInt32>()
        for candidate in currentWindows {
            let now = monotonicMS()
            var owner: pid_t = 0
            guard now < deadline,
                  AXUIElementSetMessagingTimeout(candidate, Float(min(25, deadline - now)) / 1000) == .success,
                  AXUIElementGetPid(candidate, &owner) == .success, UInt32(owner) == fp.app_pid,
                  monotonicMS() < deadline,
                  let rawModal = attribute(candidate, kAXModalAttribute),
                  CFGetTypeID(rawModal) == CFBooleanGetTypeID(),
                  let modal = rawModal as? NSNumber, !modal.boolValue,
                  let role = attribute(candidate, kAXRoleAttribute) as? String,
                  let subrole = attribute(candidate, kAXSubroleAttribute) as? String,
                  role == kAXWindowRole,
                  let title = attribute(candidate, kAXTitleAttribute) as? String,
                  title.utf8.count <= 512,
                  !title.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }),
                  let frame = rect(candidate), frame.valid else { return nil }

            if subrole == kAXStandardWindowSubrole {
                guard let children = currentWindowChildrenMatchingInitial(candidate) else { return nil }
                let cgID = nativeWindowID(frame)
                if CFEqual(candidate, window) {
                    guard bindingLabel(title) == fp.label,
                          frame.matches(request.original_proof.display_frame),
                          cgID == number, number > 0 else { return nil }
                    matchedTarget += 1
                }
                if let cgID, cgID == 0 || !seenCGWindowIDs.insert(cgID).inserted { return nil }
                samples.append(ScopedActivationWindowSample(element: candidate, children: children, parent: nil,
                    parentRole: nil, parentSubrole: nil,
                    role: role, subrole: subrole, title: title, frame: frame, modal: modal.boolValue,
                    cgWindowID: cgID))
            } else if subrole == "AXDialog" {
                auxiliaryDialogCount += 1
                guard auxiliaryDialogCount == 1,
                      let children = currentWindowChildrenMatchingInitial(candidate),
                      let rawParent = attribute(candidate, kAXParentAttribute),
                      CFGetTypeID(rawParent) == AXUIElementGetTypeID() else { return nil }
                let parent = rawParent as! AXUIElement
                guard let parentRole = attribute(parent, kAXRoleAttribute) as? String else { return nil }
                let parentSubrole: String?
                if CFEqual(parent, window) {
                    guard parentRole == kAXWindowRole,
                          let subrole = attribute(parent, kAXSubroleAttribute) as? String,
                          subrole == kAXStandardWindowSubrole else { return nil }
                    parentSubrole = subrole
                } else if CFEqual(parent, application) {
                    var parentPID: pid_t = 0
                    guard parentRole == kAXApplicationRole,
                          AXUIElementGetPid(parent, &parentPID) == .success,
                          UInt32(parentPID) == fp.app_pid, monotonicMS() < deadline else { return nil }
                    parentSubrole = nil
                } else {
                    return nil
                }
                guard
                      let cgID = nativeWindowID(frame), cgID > 0, cgID != number,
                      seenCGWindowIDs.insert(cgID).inserted else { return nil }
                samples.append(ScopedActivationWindowSample(element: candidate, children: children, parent: parent,
                    parentRole: parentRole, parentSubrole: parentSubrole,
                    role: role, subrole: subrole, title: title, frame: frame, modal: modal.boolValue,
                    cgWindowID: cgID))
            } else {
                return nil
            }
        }
        guard matchedTarget == 1, monotonicMS() < deadline else { return nil }
        return samples
    }
    var initialWindowInventory: [ScopedActivationWindowSample]?
    func selectedRootStillMember() -> Bool {
        if scopedRoot != nil {
            guard let initialWindowInventory,
                  let current = readScopedWindowInventory(),
                  current.count == initialWindowInventory.count else { return false }
            return zip(current, initialWindowInventory).allSatisfy { $0.sameLiveFact(as: $1) }
        }
        guard let currentWindows = attribute(application, kAXWindowsAttribute) as? [AXUIElement],
              !currentWindows.isEmpty, currentWindows.count <= 32 else { return false }
        return currentWindows.filter({ CFEqual($0, window) }).count == 1
    }
    var initialActiveLayer: (known: Bool, value: String?) = (true, nil)
    var initialRootFacts: NativeDeviceContextWindowRootFacts?
    var initialWholeContext: NativeBindingContextSnapshot?
    if let scopedRoot {
        guard let inventory = readScopedWindowInventory(),
              let facts = contextReader.readWindowRoot(scopedRoot,
                  binding: fp, displayFrame: request.original_proof.display_frame) else {
            return refusal("scoped normal-window root unavailable")
        }
        initialWindowInventory = inventory
        initialRootFacts = facts
    } else {
        initialActiveLayer = currentActiveLayer()
        guard let context = contextReader.readContext(window), initialActiveLayer.known,
              context.matches(request.context_witness,
                  binding: fp, displayFrame: request.original_proof.display_frame,
                  activeLayer: initialActiveLayer.value, verifyActiveLayer: true) else {
            return refusal(contextReader.failureReason)
        }
        initialWholeContext = context
    }
    func sameWindow() -> Bool {
        guard selectedRootStillMember(), DeviceProcessStamp.read(Int32(fp.app_pid)) == stamp,
              !app.isTerminated, app.bundleIdentifier == fp.app, AXIsProcessTrusted() else { return false }
        if let scopedRoot {
            guard let initialRootFacts,
                  let current = contextReader.readWindowRoot(scopedRoot,
                      binding: fp, displayFrame: request.original_proof.display_frame),
                  current == initialRootFacts,
                  nativeWindowID(current.window.frame) == number else { return false }
            return true
        }
        let currentLayer = currentActiveLayer()
        guard currentLayer.known,
              let title = attribute(window, kAXTitleAttribute) as? String, title.utf8.count <= 512,
              bindingLabel(title) == fp.label, let frame = rect(window), frame.matches(request.original_proof.display_frame), selected(frame),
              let modal = attribute(window, kAXModalAttribute) as? NSNumber, !modal.boolValue,
              let initialWholeContext,
              let currentContext = contextReader.readContext(window),
              currentContext.matches(request.context_witness,
                  binding: fp, displayFrame: request.original_proof.display_frame,
                  activeLayer: currentLayer.value, verifyActiveLayer: true),
              initialWholeContext.sameWorkerGraph(as: currentContext) else { return false }
        return true
    }
    func activeMenuBlockReason() -> String? {
        if let reason = nativeMenuInputBlockReason(root: window, pid: Int32(fp.app_pid), deadline: deadline) {
            return reason
        }
        guard scopedRoot != nil else { return nil }
        for sample in initialWindowInventory ?? [] where sample.subrole == "AXDialog" {
            if let reason = nativeMenuInputBlockReason(root: sample.element,
                pid: Int32(fp.app_pid), deadline: deadline) { return reason }
        }
        return nil
    }
    if let reason = activeMenuBlockReason() { return refusal(reason) }
    var actions: CFArray?
    var settable = DarwinBoolean(false)
    guard sameWindow(), AXUIElementCopyActionNames(window, &actions) == .success,
          let names = actions as? [String], names.count <= 16, names.contains(kAXRaiseAction),
          AXUIElementIsAttributeSettable(application, kAXFrontmostAttribute as CFString, &settable) == .success, settable.boolValue,
          monotonicMS() < deadline else { return refusal("window identity or raise action changed") }
    return state.commitPress(ticket, deadlineMS: deadline, preCommitRevalidate: {
        if let reason = activeMenuBlockReason() { return reason }
        return sameWindow() ? nil : "window identity or sheet boundary changed before raise"
    }) {
        func prepareInputCall(_ element: AXUIElement) -> Bool {
            let now = monotonicMS()
            guard now < deadline,
                  AXUIElementSetMessagingTimeout(element, Float(min(25, deadline - now)) / 1000) == .success else { return false }
            return monotonicMS() < deadline
        }
        // Once the intent is consumed, either late or failed OS call remains
        // unknown and is isolated by commitPress; it must never look unsubmitted.
        guard prepareInputCall(application), monotonicMS() < deadline,
              AXUIElementSetAttributeValue(application, kAXFrontmostAttribute as CFString,
                  kCFBooleanTrue) == .success else { return false }
        guard prepareInputCall(window), monotonicMS() < deadline,
              AXUIElementPerformAction(window, kAXRaiseAction as CFString) == .success else { return false }
        return true
    }
}
