import ApplicationServices
import CoreGraphics
import Foundation

enum BackgroundAXWindowStatus: String, Sendable, Equatable {
    case visible
    case hidden
    case minimized
    case unavailable
}

enum BackgroundAXWindowFailure: String, Error, Sendable, Equatable {
    case invalidRequest
    case deadlineExceeded
    case timeoutSetupFailed
    case processUnavailable
    case processChanged
    case axApplicationUnavailable
    case axWindowInventoryUnavailable
    case axWindowInventoryInvalid
    case axWindowBoundaryUnsupported
    case axWindowMetadataUnavailable
    case axWindowMetadataChanged
    case axParentMismatch
    case cgInventoryUnavailable
    case cgWindowBoundaryUnsupported
    case cgWindowIdentityMismatch
    case cgWindowGeometryChanged
    case rootChangedDuringRead
    case rootReplacementLatched
    case visibilityUnknown
    case visibilityChanged
    case modalOrUnknownBoundary
    case stateNeedsVisibleBaseline
    case contradictoryVisibility
}

struct BackgroundAXWindowContext: Sendable, Equatable {
    let source_session_id: String
    let session_epoch: UInt64
    let process_id: UInt32
    let process: ProcessStamp
    let window_id: String
    let title: String
    let frame: Rect
}

struct BackgroundAXWindowReadResult: Sendable, Equatable {
    let status: BackgroundAXWindowStatus
    let context: BackgroundAXWindowContext?
    let failure: BackgroundAXWindowFailure?
    let started_ns: UInt64
    let finished_ns: UInt64
}

struct BackgroundAXWindowSnapshotFacts: Sendable, Equatable {
    let root_count: Int
    let registered_root_matches: Bool
    let parent_is_application: Bool
    let role: String?
    let subrole: String?
    let title: String?
    let frame: Rect?
    let modal: Bool?
    let minimized: Bool?
    let application_hidden: Bool?
    let cg_windows: [NativeWindow]?
}

struct BackgroundAXWindowPolicyFacts: Sendable, Equatable {
    let app_bundle_id: String
    let app_pid: UInt32
    let source_session_id: String
    let session_epoch: UInt64
    let expected_process: ProcessStamp
    let process_before: ProcessStamp?
    let process_after: ProcessStamp?
    let target_window_id: String
    let before: BackgroundAXWindowSnapshotFacts
    let after: BackgroundAXWindowSnapshotFacts
    let root_continuous: Bool
    /// Nil denotes the first visible read, before this watch has a retained AX root.
    let retained_root_matches: Bool?
}

struct BackgroundAXWindowDecision: Sendable, Equatable {
    let status: BackgroundAXWindowStatus
    let context: BackgroundAXWindowContext?
    let failure: BackgroundAXWindowFailure?
}

func decideBackgroundAXWindowContext(_ facts: BackgroundAXWindowPolicyFacts) -> BackgroundAXWindowDecision {
    func rejected(_ reason: BackgroundAXWindowFailure) -> BackgroundAXWindowDecision {
        BackgroundAXWindowDecision(status: .unavailable, context: nil, failure: reason)
    }

    guard !facts.app_bundle_id.isEmpty, facts.app_pid > 0,
          !facts.source_session_id.isEmpty, facts.session_epoch > 0,
          backgroundWindowNumber(facts.target_window_id) != nil else {
        return rejected(.invalidRequest)
    }
    guard facts.process_before == facts.expected_process,
          facts.process_after == facts.expected_process else {
        return rejected(facts.process_before == nil || facts.process_after == nil
            ? .processUnavailable : .processChanged)
    }
    guard facts.root_continuous, facts.retained_root_matches != false else {
        return rejected(facts.retained_root_matches == false ? .rootReplacementLatched : .rootChangedDuringRead)
    }
    guard facts.before.root_count == 1, facts.after.root_count == 1,
          facts.before.registered_root_matches, facts.after.registered_root_matches else {
        return rejected(.axWindowInventoryInvalid)
    }
    guard facts.before.parent_is_application, facts.after.parent_is_application else {
        return rejected(.axParentMismatch)
    }
    guard facts.before.role == kAXWindowRole, facts.after.role == kAXWindowRole,
          facts.before.subrole == kAXStandardWindowSubrole,
          facts.after.subrole == kAXStandardWindowSubrole,
          let beforeTitle = facts.before.title, let afterTitle = facts.after.title,
          let beforeFrame = facts.before.frame, let afterFrame = facts.after.frame,
          beforeFrame.valid, afterFrame.valid, beforeFrame.width > 0, beforeFrame.height > 0,
          beforeFrame == afterFrame, beforeTitle == afterTitle else {
        return rejected(.axWindowMetadataChanged)
    }
    guard facts.before.modal == false, facts.after.modal == false else {
        return rejected(.modalOrUnknownBoundary)
    }
    guard let hiddenBefore = facts.before.application_hidden,
          let hiddenAfter = facts.after.application_hidden,
          let minimizedBefore = facts.before.minimized,
          let minimizedAfter = facts.after.minimized else {
        return rejected(.visibilityUnknown)
    }
    guard hiddenBefore == hiddenAfter, minimizedBefore == minimizedAfter else {
        return rejected(.visibilityChanged)
    }
    guard facts.before.cg_windows != nil, facts.after.cg_windows != nil else {
        return rejected(.cgInventoryUnavailable)
    }
    if hiddenBefore && minimizedBefore {
        return rejected(.contradictoryVisibility)
    }
    if hiddenBefore || minimizedBefore {
        guard facts.retained_root_matches == true else { return rejected(.stateNeedsVisibleBaseline) }
        guard facts.before.cg_windows?.isEmpty == true, facts.after.cg_windows?.isEmpty == true else {
            return rejected(.cgWindowIdentityMismatch)
        }
        return BackgroundAXWindowDecision(status: hiddenBefore ? .hidden : .minimized,
                                          context: nil, failure: nil)
    }

    guard let beforeCG = facts.before.cg_windows, let afterCG = facts.after.cg_windows,
          beforeCG.count == 1, afterCG.count == 1,
          let targetNumber = backgroundWindowNumber(facts.target_window_id) else {
        return rejected(.cgWindowIdentityMismatch)
    }
    let cgBefore = beforeCG[0]
    let cgAfter = afterCG[0]
    guard cgBefore.id == targetNumber, cgAfter.id == targetNumber,
          cgBefore.pid == facts.app_pid, cgAfter.pid == facts.app_pid else {
        return rejected(.cgWindowIdentityMismatch)
    }
    guard cgBefore.frame == cgAfter.frame,
          cgBefore.frame.approximatelyEquals(beforeFrame),
          cgAfter.frame.approximatelyEquals(afterFrame) else {
        return rejected(.cgWindowGeometryChanged)
    }
    let context = BackgroundAXWindowContext(source_session_id: facts.source_session_id,
        session_epoch: facts.session_epoch, process_id: facts.app_pid,
        process: facts.expected_process, window_id: facts.target_window_id,
        title: beforeTitle, frame: beforeFrame)
    return BackgroundAXWindowDecision(status: .visible, context: context, failure: nil)
}

private func backgroundWindowNumber(_ windowID: String) -> UInt32? {
    guard windowID.hasPrefix("window-") else { return nil }
    let suffix = String(windowID.dropFirst("window-".count))
    guard let value = UInt32(suffix), value > 0, String(value) == suffix else { return nil }
    return value
}

/// Owns one fixed target root on the AX session worker. It never queries focus
/// or creates an active layer. Only a visible, uniquely CG-bound first read can
/// establish its retained AX root; a later root replacement latches closed.
final class BackgroundWindowContextReader {
    private let app: AppIdentity
    private let sourceSessionID: String
    private let sessionEpoch: UInt64
    private let targetWindowID: String
    private let expectedProcess: ProcessStamp
    private let rootLimit: Int
    private var retainedRoot: AXUIElement?
    private var replacementLatched = false

    func matchesRetainedRoot(_ candidate: CFTypeRef) -> Bool {
        !replacementLatched && retainedRoot.map { CFEqual($0, candidate) } == true
    }

    init?(app: AppIdentity, sourceSessionID: String, sessionEpoch: UInt64,
          targetWindowID: String, expectedProcess: ProcessStamp,
          nodeLimit: Int, depthLimit: Int) {
        guard app.pid > 0, !sourceSessionID.isEmpty, sessionEpoch > 0,
              backgroundWindowNumber(targetWindowID) != nil,
              expectedProcess.seconds > 0, expectedProcess.micros < 1_000_000,
              nodeLimit > 0, nodeLimit <= 512, depthLimit > 0, depthLimit <= 12 else { return nil }
        self.app = app
        self.sourceSessionID = sourceSessionID
        self.sessionEpoch = sessionEpoch
        self.targetWindowID = targetWindowID
        self.expectedProcess = expectedProcess
        self.rootLimit = min(32, min(nodeLimit, 512))
    }

    func read(deadline: UInt64) -> BackgroundAXWindowReadResult {
        let started = uptimeNs()
        func result(_ status: BackgroundAXWindowStatus, _ failure: BackgroundAXWindowFailure?,
                    _ context: BackgroundAXWindowContext? = nil) -> BackgroundAXWindowReadResult {
            BackgroundAXWindowReadResult(status: status, context: context, failure: failure,
                                         started_ns: started, finished_ns: uptimeNs())
        }
        func failed(_ reason: BackgroundAXWindowFailure) -> BackgroundAXWindowReadResult {
            result(.unavailable, reason)
        }

        guard !replacementLatched else { return failed(.rootReplacementLatched) }
        guard uptimeNs() < deadline else { return failed(.deadlineExceeded) }
        do {
            guard let beforeProcess = ProcessStamp.read(pid: app.pid) else { throw BackgroundAXWindowFailure.processUnavailable }
            guard beforeProcess == expectedProcess else { throw BackgroundAXWindowFailure.processChanged }
            let application = AXUIElementCreateApplication(pid_t(app.pid))
            guard try elementPID(application, deadline: deadline) == app.pid else {
                throw BackgroundAXWindowFailure.axApplicationUnavailable
            }
            let beforeRoots = try windowRoots(application, deadline: deadline)
            guard beforeRoots.count == 1, let beforeRoot = beforeRoots.first else {
                throw BackgroundAXWindowFailure.axWindowBoundaryUnsupported
            }
            let cgBefore = try onScreenWindows(deadline: deadline)
            let before = try snapshot(application: application, root: beforeRoot,
                                      rootCount: beforeRoots.count, cgWindows: cgBefore, deadline: deadline)

            let afterRoots = try windowRoots(application, deadline: deadline)
            guard afterRoots.count == 1, let afterRoot = afterRoots.first else {
                throw BackgroundAXWindowFailure.axWindowBoundaryUnsupported
            }
            let cgAfter = try onScreenWindows(deadline: deadline)
            let after = try snapshot(application: application, root: afterRoot,
                                     rootCount: afterRoots.count, cgWindows: cgAfter, deadline: deadline)
            guard let afterProcess = ProcessStamp.read(pid: app.pid) else {
                throw BackgroundAXWindowFailure.processUnavailable
            }
            guard afterProcess == expectedProcess else { throw BackgroundAXWindowFailure.processChanged }
            guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }

            let rootContinuous = CFEqual(beforeRoot, afterRoot)
            let retainedMatchesBefore = retainedRoot.map { CFEqual($0, beforeRoot) }
            let retainedMatchesAfter = retainedRoot.map { CFEqual($0, afterRoot) }
            if retainedRoot != nil && (retainedMatchesBefore != true || retainedMatchesAfter != true || !rootContinuous) {
                replacementLatched = true
                throw BackgroundAXWindowFailure.rootReplacementLatched
            }
            let facts = BackgroundAXWindowPolicyFacts(app_bundle_id: app.bundle_id, app_pid: app.pid,
                source_session_id: sourceSessionID, session_epoch: sessionEpoch,
                expected_process: expectedProcess, process_before: beforeProcess, process_after: afterProcess,
                target_window_id: targetWindowID,
                before: before, after: after, root_continuous: rootContinuous,
                retained_root_matches: retainedRoot == nil ? nil : retainedMatchesBefore)
            let decision = decideBackgroundAXWindowContext(facts)
            guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
            if decision.status == .visible, retainedRoot == nil {
                retainedRoot = afterRoot
            }
            return result(decision.status, decision.failure, decision.context)
        } catch let failure as BackgroundAXWindowFailure {
            return failed(failure)
        } catch {
            return failed(.axWindowMetadataUnavailable)
        }
    }

    private func remainingTimeoutSeconds(deadline: UInt64) throws -> Float {
        let now = uptimeNs()
        guard now < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        guard let timeout = backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: deadline - now) else {
            throw BackgroundAXWindowFailure.deadlineExceeded
        }
        return timeout
    }

    private func configure(_ element: AXUIElement, deadline: UInt64) throws {
        let timeout = try remainingTimeoutSeconds(deadline: deadline)
        guard AXUIElementSetMessagingTimeout(element, timeout) == .success else {
            throw BackgroundAXWindowFailure.timeoutSetupFailed
        }
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
    }

    private func elementPID(_ element: AXUIElement, deadline: UInt64) throws -> UInt32 {
        try configure(element, deadline: deadline)
        var owner: pid_t = 0
        guard AXUIElementGetPid(element, &owner) == .success, owner > 0,
              UInt32(owner) == app.pid, uptimeNs() < deadline else {
            throw BackgroundAXWindowFailure.axApplicationUnavailable
        }
        return UInt32(owner)
    }

    private func raw(_ element: AXUIElement, _ attribute: String, deadline: UInt64) throws -> CFTypeRef {
        try configure(element, deadline: deadline)
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        guard error == .success, let value else { throw BackgroundAXWindowFailure.axWindowMetadataUnavailable }
        return value
    }

    private func typedString(_ element: AXUIElement, _ attribute: String, deadline: UInt64) throws -> String {
        let value = try raw(element, attribute, deadline: deadline)
        guard CFGetTypeID(value) == CFStringGetTypeID(), let string = value as? String,
              string.utf8.count <= 512 else {
            throw BackgroundAXWindowFailure.axWindowMetadataUnavailable
        }
        return string
    }

    private func typedBoolean(_ element: AXUIElement, _ attribute: String, deadline: UInt64,
                              unknown: BackgroundAXWindowFailure) throws -> Bool {
        let value: CFTypeRef
        do {
            value = try raw(element, attribute, deadline: deadline)
        } catch let failure as BackgroundAXWindowFailure {
            if failure == .deadlineExceeded || failure == .timeoutSetupFailed { throw failure }
            throw unknown
        }
        guard CFGetTypeID(value) == CFBooleanGetTypeID(), let number = value as? NSNumber else {
            throw unknown
        }
        return number.boolValue
    }

    private func typedFrame(_ element: AXUIElement, deadline: UInt64) throws -> Rect {
        let position = try raw(element, kAXPositionAttribute, deadline: deadline)
        let size = try raw(element, kAXSizeAttribute, deadline: deadline)
        guard CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else {
            throw BackgroundAXWindowFailure.axWindowMetadataUnavailable
        }
        let positionValue = unsafeDowncast(position, to: AXValue.self)
        let sizeValue = unsafeDowncast(size, to: AXValue.self)
        guard AXValueGetType(positionValue) == .cgPoint, AXValueGetType(sizeValue) == .cgSize else {
            throw BackgroundAXWindowFailure.axWindowMetadataUnavailable
        }
        var point = CGPoint.zero
        var dimensions = CGSize.zero
        guard AXValueGetValue(positionValue, .cgPoint, &point),
              AXValueGetValue(sizeValue, .cgSize, &dimensions) else {
            throw BackgroundAXWindowFailure.axWindowMetadataUnavailable
        }
        let frame = Rect(CGRect(origin: point, size: dimensions))
        guard frame.valid, frame.width > 0, frame.height > 0 else {
            throw BackgroundAXWindowFailure.axWindowMetadataUnavailable
        }
        return frame
    }

    private func parentIsApplication(_ window: AXUIElement, application: AXUIElement,
                                     deadline: UInt64) throws -> Bool {
        let value = try raw(window, kAXParentAttribute, deadline: deadline)
        guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
            throw BackgroundAXWindowFailure.axParentMismatch
        }
        let parent = unsafeDowncast(value, to: AXUIElement.self)
        return try elementPID(parent, deadline: deadline) == app.pid && CFEqual(parent, application)
    }

    private func windowRoots(_ application: AXUIElement, deadline: UInt64) throws -> [AXUIElement] {
        try configure(application, deadline: deadline)
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(application, kAXWindowsAttribute as CFString, &count)
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        guard countError == .success, count > 0, count <= rootLimit else {
            throw BackgroundAXWindowFailure.axWindowInventoryUnavailable
        }
        try configure(application, deadline: deadline)
        var rawValues: CFArray?
        let copyError = AXUIElementCopyAttributeValues(application, kAXWindowsAttribute as CFString,
                                                       0, count, &rawValues)
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        guard copyError == .success, let rawValues,
              let values = rawValues as? [AXUIElement], values.count == count,
              values.count <= rootLimit else {
            throw BackgroundAXWindowFailure.axWindowInventoryUnavailable
        }
        for (index, window) in values.enumerated() {
            guard try elementPID(window, deadline: deadline) == app.pid,
                  !values[..<index].contains(where: { CFEqual($0, window) }) else {
                throw BackgroundAXWindowFailure.axWindowInventoryInvalid
            }
        }
        return values
    }

    private func snapshot(application: AXUIElement, root: AXUIElement, rootCount: Int,
                          cgWindows: [NativeWindow], deadline: UInt64) throws -> BackgroundAXWindowSnapshotFacts {
        guard rootCount == 1 else { throw BackgroundAXWindowFailure.axWindowBoundaryUnsupported }
        let pid = try elementPID(root, deadline: deadline)
        guard pid == app.pid else { throw BackgroundAXWindowFailure.processChanged }
        let role = try typedString(root, kAXRoleAttribute, deadline: deadline)
        let subrole = try typedString(root, kAXSubroleAttribute, deadline: deadline)
        let title = try typedString(root, kAXTitleAttribute, deadline: deadline)
        let frame = try typedFrame(root, deadline: deadline)
        let modal = try typedBoolean(root, kAXModalAttribute, deadline: deadline,
                                     unknown: .modalOrUnknownBoundary)
        let minimized = try typedBoolean(root, kAXMinimizedAttribute, deadline: deadline,
                                         unknown: .visibilityUnknown)
        let hidden = try typedBoolean(application, kAXHiddenAttribute, deadline: deadline,
                                      unknown: .visibilityUnknown)
        let parentMatches = try parentIsApplication(root, application: application, deadline: deadline)
        let rootMatches: Bool
        if let retainedRoot {
            rootMatches = CFEqual(retainedRoot, root)
        } else {
            let targetNumber = backgroundWindowNumber(targetWindowID)
            let matching = cgWindows.filter { $0.id == targetNumber }
            rootMatches = matching.count == 1 && matching[0].pid == app.pid
                && matching[0].frame.approximatelyEquals(frame)
        }
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        return BackgroundAXWindowSnapshotFacts(root_count: rootCount,
            registered_root_matches: rootMatches, parent_is_application: parentMatches,
            role: role, subrole: subrole, title: title, frame: frame, modal: modal,
            minimized: minimized, application_hidden: hidden, cg_windows: cgWindows)
    }

    private func onScreenWindows(deadline: UInt64) throws -> [NativeWindow] {
        guard uptimeNs() < deadline,
              let values = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                                       kCGNullWindowID) as? [[String: Any]],
              uptimeNs() < deadline else {
            throw BackgroundAXWindowFailure.cgInventoryUnavailable
        }
        var result: [NativeWindow] = []
        for (index, info) in values.enumerated() {
            guard let owner = info[kCGWindowOwnerPID as String] as? NSNumber,
                  owner.uint32Value == app.pid else { continue }
            guard let layer = info[kCGWindowLayer as String] as? NSNumber else {
                throw BackgroundAXWindowFailure.cgInventoryUnavailable
            }
            guard layer.intValue == 0,
                  let id = info[kCGWindowNumber as String] as? NSNumber,
                  let bounds = info[kCGWindowBounds as String] as? [String: Any],
                  let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary) else {
                throw BackgroundAXWindowFailure.cgWindowBoundaryUnsupported
            }
            let frame = Rect(cgFrame)
            guard frame.valid, frame.width > 0, frame.height > 0,
                  index <= Int(UInt32.max), result.count < rootLimit else {
                throw BackgroundAXWindowFailure.cgWindowBoundaryUnsupported
            }
            let native = NativeWindow(id: id.uint32Value, pid: app.pid, frame: frame,
                                      zIndex: UInt32(index))
            guard !result.contains(where: { $0.id == native.id }) else {
                throw BackgroundAXWindowFailure.cgWindowBoundaryUnsupported
            }
            result.append(native)
        }
        guard uptimeNs() < deadline else { throw BackgroundAXWindowFailure.deadlineExceeded }
        return result
    }
}

func runBackgroundWindowContextTests() throws -> [String] {
    let rect = Rect(CGRect(x: 20, y: 30, width: 500, height: 400))
    let cgAfter = NativeWindow(id: 19, pid: 700, frame: rect, zIndex: 22)

    func snapshot(hidden: Bool? = false, minimized: Bool? = false, modal: Bool? = false,
                  role: String? = kAXWindowRole, subrole: String? = kAXStandardWindowSubrole,
                  title: String? = "Editor",
                  frame: Rect? = Rect(CGRect(x: 20, y: 30, width: 500, height: 400)), rootCount: Int = 1,
                  registered: Bool = true, parent: Bool = true,
                  cg: [NativeWindow]? = nil, cgUnavailable: Bool = false) -> BackgroundAXWindowSnapshotFacts {
        BackgroundAXWindowSnapshotFacts(root_count: rootCount, registered_root_matches: registered,
            parent_is_application: parent, role: role, subrole: subrole, title: title, frame: frame,
            modal: modal, minimized: minimized, application_hidden: hidden,
            cg_windows: cgUnavailable ? nil : (cg ?? [NativeWindow(id: 19, pid: 700,
                frame: Rect(CGRect(x: 20, y: 30, width: 500, height: 400)), zIndex: 2)]))
    }
    func facts(before: BackgroundAXWindowSnapshotFacts? = nil,
               after: BackgroundAXWindowSnapshotFacts? = nil,
               expected: ProcessStamp = ProcessStamp(seconds: 42, micros: 17),
               processBefore: ProcessStamp? = ProcessStamp(seconds: 42, micros: 17),
               processAfter: ProcessStamp? = ProcessStamp(seconds: 42, micros: 17), rootContinuous: Bool = true,
               retainedRootMatches: Bool? = nil, windowID: String = "window-19") -> BackgroundAXWindowPolicyFacts {
        BackgroundAXWindowPolicyFacts(app_bundle_id: "example.editor", app_pid: 700,
            source_session_id: "watch-one", session_epoch: 4, expected_process: expected,
            process_before: processBefore, process_after: processAfter, target_window_id: windowID,
            before: before ?? snapshot(), after: after ?? snapshot(cg: [cgAfter]),
            root_continuous: rootContinuous, retained_root_matches: retainedRootMatches)
    }
    func accepted(_ result: BackgroundAXWindowDecision, _ status: BackgroundAXWindowStatus) -> Bool {
        result.status == status && result.failure == nil
    }
    var passed: [String] = []

    let visible = decideBackgroundAXWindowContext(facts())
    try requireTest(accepted(visible, .visible) && visible.context?.window_id == "window-19",
                    "background visible root requires exact target and complete current metadata")
    passed.append("background_visible_root_requires_exact_process_window_and_cg_match")
    try requireTest(visible.context?.frame == rect,
                    "a global z-index change does not invalidate the unchanged target window")
    passed.append("background_target_identity_ignores_other_app_focus_z_order")

    let hidden = decideBackgroundAXWindowContext(facts(
        before: snapshot(hidden: true, cg: []), after: snapshot(hidden: true, cg: []),
        retainedRootMatches: true))
    try requireTest(accepted(hidden, .hidden) && hidden.context == nil,
                    "known hidden state is typed but grants no visible context")
    passed.append("background_hidden_state_is_typed_without_context")

    let minimized = decideBackgroundAXWindowContext(facts(
        before: snapshot(minimized: true, cg: []), after: snapshot(minimized: true, cg: []),
        retainedRootMatches: true))
    try requireTest(accepted(minimized, .minimized) && minimized.context == nil,
                    "known minimized state is typed but grants no visible context")
    passed.append("background_minimized_state_is_typed_without_context")

    let firstHidden = decideBackgroundAXWindowContext(facts(
        before: snapshot(hidden: true, cg: []), after: snapshot(hidden: true, cg: [])))
    try requireTest(firstHidden.status == .unavailable && firstHidden.failure == .stateNeedsVisibleBaseline,
                    "hidden target without a prior exact visible root remains unavailable")
    passed.append("background_hidden_without_visible_baseline_is_unavailable")

    let unknownVisibility = decideBackgroundAXWindowContext(facts(
        before: snapshot(hidden: nil), after: snapshot(hidden: nil)))
    try requireTest(unknownVisibility.failure == .visibilityUnknown,
                    "unknown hidden state cannot be inferred from absent CG pixels")
    passed.append("background_unknown_visibility_fails_closed")

    let unknownMinimized = decideBackgroundAXWindowContext(facts(
        before: snapshot(minimized: nil), after: snapshot(minimized: nil)))
    try requireTest(unknownMinimized.failure == .visibilityUnknown,
                    "unknown minimized state cannot be inferred from geometry")
    passed.append("background_unknown_minimized_fails_closed")

    let modal = decideBackgroundAXWindowContext(facts(
        before: snapshot(modal: true), after: snapshot(modal: true)))
    try requireTest(modal.failure == .modalOrUnknownBoundary,
                    "modal background root remains unavailable")
    passed.append("background_modal_root_fails_closed")
    let unknownModal = decideBackgroundAXWindowContext(facts(
        before: snapshot(modal: nil), after: snapshot(modal: nil)))
    try requireTest(unknownModal.failure == .modalOrUnknownBoundary,
                    "unknown modal state cannot be treated as nonmodal")
    passed.append("background_unknown_modal_state_fails_closed")

    let wrongParent = decideBackgroundAXWindowContext(facts(
        before: snapshot(parent: false), after: snapshot(parent: false)))
    try requireTest(wrongParent.failure == .axParentMismatch,
                    "a frame match cannot replace the actual AX application parent")
    passed.append("background_actual_ax_parent_is_required")

    let extraRoot = decideBackgroundAXWindowContext(facts(
        before: snapshot(rootCount: 2), after: snapshot(rootCount: 2)))
    try requireTest(extraRoot.failure == .axWindowInventoryInvalid,
                    "an extra AX window remains an unknown boundary")
    passed.append("background_extra_ax_root_fails_closed")

    let changedBirth = decideBackgroundAXWindowContext(facts(processAfter: ProcessStamp(seconds: 43, micros: 17)))
    try requireTest(changedBirth.failure == .processChanged,
                    "process birth replacement rejects background context")
    passed.append("background_process_birth_change_fails_closed")

    let replacedRoot = decideBackgroundAXWindowContext(facts(rootContinuous: false))
    try requireTest(replacedRoot.failure == .rootChangedDuringRead,
                    "same-read actual AX root replacement rejects context")
    passed.append("background_same_read_root_replacement_fails_closed")

    let latchedRoot = decideBackgroundAXWindowContext(facts(retainedRootMatches: false))
    try requireTest(latchedRoot.failure == .rootReplacementLatched,
                    "cross-read root replacement stays latched for the watch")
    passed.append("background_cross_read_root_replacement_latches")

    let wrongWindow = NativeWindow(id: 20, pid: 700, frame: rect, zIndex: 2)
    let wrongID = decideBackgroundAXWindowContext(facts(
        before: snapshot(cg: [wrongWindow]), after: snapshot(cg: [wrongWindow])))
    try requireTest(wrongID.failure == .cgWindowIdentityMismatch,
                    "frame coincidence cannot substitute the registered CG window ID")
    passed.append("background_cg_identity_is_not_inferred_from_frame")

    let cgMoved = decideBackgroundAXWindowContext(facts(
        before: snapshot(), after: snapshot(cg: [NativeWindow(id: 19, pid: 700,
            frame: Rect(CGRect(x: 22, y: 30, width: 500, height: 400)), zIndex: 22)])))
    try requireTest(cgMoved.failure == .cgWindowGeometryChanged,
                    "stable AX metadata cannot hide a moved native window")
    passed.append("background_cg_geometry_must_match_across_read")

    let moved = Rect(CGRect(x: 21, y: 30, width: 500, height: 400))
    let changedGeometry = decideBackgroundAXWindowContext(facts(
        before: snapshot(), after: snapshot(frame: moved, cg: [NativeWindow(id: 19, pid: 700, frame: moved, zIndex: 22)])))
    try requireTest(changedGeometry.failure == .axWindowMetadataChanged,
                    "target AX geometry change within the read rejects context")
    passed.append("background_target_geometry_change_fails_closed")

    let unknownCG = decideBackgroundAXWindowContext(facts(
        before: snapshot(cgUnavailable: true), after: snapshot(cgUnavailable: true)))
    try requireTest(unknownCG.failure == .cgInventoryUnavailable,
                    "unavailable CG inventory is distinct from a known empty hidden window")
    passed.append("background_cg_inventory_unknown_is_not_empty")

    let invalidID = decideBackgroundAXWindowContext(facts(windowID: "window-019"))
    try requireTest(invalidID.failure == .invalidRequest,
                    "noncanonical target window IDs are rejected")
    passed.append("background_target_window_id_is_canonical")

    let app = AppIdentity(bundle_id: "example.editor", name: "Editor", pid: 700)
    let invalidBudget = BackgroundWindowContextReader(app: app, sourceSessionID: "watch-one",
        sessionEpoch: 4, targetWindowID: "window-19", expectedProcess: ProcessStamp(seconds: 42, micros: 17),
        nodeLimit: 513, depthLimit: 12)
    try requireTest(invalidBudget == nil,
                    "background reader cannot increase the shared node budget")
    passed.append("background_reader_preserves_node_budget_cap")

    return passed
}
