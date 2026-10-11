import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Fixed, non-authorizing progress facts for the owned BackgroundAX smoke.
/// This contains no Source payload or free-form refusal text.
struct NativeBackgroundAXTrace: Codable, Sendable, Equatable {
    enum Stage: String, Codable, Sendable, CaseIterable {
        case preflight
        case processIdentity = "process_identity"
        case applicationBoundary = "application_boundary"
        case windowBoundary = "window_boundary"
        case targetTraversal = "target_traversal"
        case targetNodeIdentity = "target_node_identity"
        case targetChildrenCount = "target_children_count"
        case targetChildrenRead = "target_children_read"
        case targetChildrenConfirm = "target_children_confirm"
        case targetDeadline = "target_deadline"
        case auxiliaryRelation = "auxiliary_relation"
        case auxiliaryNonempty = "auxiliary_nonempty"
        case childrenType = "children_type"
        case auxiliaryDecorationChildren = "auxiliary_decoration_children"
        case auxiliaryGroupIdentity = "auxiliary_group_identity"
        case auxiliaryGroupRole = "auxiliary_group_role"
        case auxiliaryGroupParent = "auxiliary_group_parent"
        case auxiliaryGroupWindow = "auxiliary_group_window"
        case auxiliaryGroupChildren = "auxiliary_group_children"
        case auxiliaryGroupBounds = "auxiliary_group_bounds"
        case auxiliaryWindowUnsupported = "auxiliary_window_unsupported"
        case auxiliaryWindowNoValue = "auxiliary_window_no_value"
        case auxiliaryWindowTimeout = "auxiliary_window_timeout"
        case auxiliaryWindowReadFailed = "auxiliary_window_read_failed"
        case auxiliaryWindowMalformed = "auxiliary_window_malformed"
        case auxiliaryWindowForeign = "auxiliary_window_foreign"
        case auxiliaryWindowMismatch = "auxiliary_window_mismatch"
        case auxiliaryWindowParentUnproven = "auxiliary_window_parent_unproven"
        case targetMatch = "target_match"
        case initialStability = "initial_stability"
        case finalRevalidation = "final_revalidation"
        case commitReady = "commit_ready"
        case submitted
    }

    var stage: Stage = .preflight
    var process_identity_matches = false
    var application_boundary_matches = false
    var window_boundary_matches = false
    var target_traversal_complete = false
    var target_matches = false
    var initial_stability_passed = false
    var final_revalidation_passed = false
    var commit_ready = false
    var submitted = false
}

/// Worker-local AX evidence for one registered BackgroundAX target. These
/// references must stay on the worker and are never encoded or sent to State.
struct BackgroundAXCurrentTarget {
    let application: AXUIElement
    let window: AXUIElement
    let target: AXUIElement
    let context: NativeBindingContextSnapshot
    let fingerprint: NativeBindingFingerprint
    let actionNames: [String]
    let processStamp: DeviceProcessStamp
    let windowFrame: HitRect
    let windowTitle: String?
    let auxiliaryChromeClosures: [BackgroundAXAuxiliaryChromeClosure]

    func sameWorkerGraph(as other: BackgroundAXCurrentTarget) -> Bool {
        CFEqual(application, other.application)
            && CFEqual(window, other.window)
            && CFEqual(target, other.target)
            && context.sameWorkerGraph(as: other.context)
            && fingerprint == other.fingerprint
            && actionNames == other.actionNames
            && processStamp == other.processStamp
            && windowFrame == other.windowFrame
            && windowTitle == other.windowTitle
            && auxiliaryChromeClosures.count == other.auxiliaryChromeClosures.count
            && zip(auxiliaryChromeClosures, other.auxiliaryChromeClosures).allSatisfy {
                $0.sameWorkerClosure(as: $1)
            }
    }
}

/// A group in the tightly bounded guard-fixture zoom decoration tree. The
/// actual owner/child list, AXParent, and AXWindow observation (including an
/// explicitly absent attribute) are retained for later graph comparison.
struct BackgroundAXAuxiliaryChromeGroup {
    let owner: AXUIElement
    let element: AXUIElement
    let actualParent: AXUIElement
    let window: AXUIElement
    let windowMembership: BackgroundAXAuxiliaryWindowMembership
    let children: [AXUIElement]

    func sameWorkerGroup(as other: Self) -> Bool {
        CFEqual(owner, other.owner) && CFEqual(element, other.element)
            && CFEqual(actualParent, other.actualParent) && CFEqual(window, other.window)
            && windowMembership.sameWorkerMembership(as: other.windowMembership)
            && children.count == other.children.count
            && zip(children, other.children).allSatisfy { CFEqual($0, $1) }
    }
}

enum BackgroundAXWindowMembershipDisposition: String, Equatable {
    case known
    case unsupported
    case noValue = "no_value"
    case timeout
    case readFailed = "read_failed"
    case malformed
    case foreign
    case mismatch
    case parentUnproven = "parent_unproven"
}

struct BackgroundAXAuxiliaryWindowMembership {
    let disposition: BackgroundAXWindowMembershipDisposition
    let knownWindow: AXUIElement?

    func sameWorkerMembership(as other: Self) -> Bool {
        guard disposition == other.disposition else { return false }
        switch (knownWindow, other.knownWindow) {
        case let (left?, right?): return CFEqual(left, right)
        case (nil, nil): return disposition == .unsupported || disposition == .noValue
        default: return false
        }
    }
}

/// The sole permitted AXChildren/AXParent discrepancy: a guard-fixture
/// AXZoomButton exposes a closed, bounded tree of non-semantic AXGroup nodes
/// through AXChildren, while each group reports the selected window or an
/// already-proven auxiliary group as AXParent. This graph stays outside target
/// matching and is compared by live CF references on each revalidation.
struct BackgroundAXAuxiliaryChromeClosure {
    let decoration: AXUIElement
    let decorationParent: AXUIElement
    let window: AXUIElement
    let groups: [BackgroundAXAuxiliaryChromeGroup]

    func sameWorkerClosure(as other: Self) -> Bool {
        guard CFEqual(decoration, other.decoration),
              CFEqual(decorationParent, other.decorationParent), CFEqual(window, other.window),
              groups.count == other.groups.count else { return false }
        return zip(groups, other.groups).allSatisfy { $0.sameWorkerGroup(as: $1) }
    }
}

private struct BackgroundAXStableChildren {
    let elements: [AXUIElement]
    let countBefore: Int
    let countAfter: Int
}

/// Resolves a public semantic scope directly from the authorized app/window AX
/// tree. It performs no system-wide point hit-test, focus query, raise, or input.
/// Construct and use it on the serial native AX worker only.
final class BackgroundAXReader {
    let authorization: BackgroundAXAuthorization
    let deadlineMS: UInt64
    private(set) var refusalReason = "background AX scope unavailable"
    var diagnosticTrace = NativeBackgroundAXTrace()

    init(authorization: BackgroundAXAuthorization, deadlineMS: UInt64) {
        self.authorization = authorization
        self.deadlineMS = deadlineMS
    }

    /// Two complete reads establish initial current identity and worker-local
    /// CF continuity. A later submit gate should call `revalidate` immediately
    /// before consuming the intent and use that returned target for AXPress.
    func readStableTarget() -> BackgroundAXCurrentTarget? {
        diagnosticTrace.stage = .preflight
        diagnosticTrace.initial_stability_passed = false
        guard BackgroundAXPolicy.authorizationIsValid(authorization),
              monotonicMS() < deadlineMS else {
            refusalReason = "background AX authorization or deadline unavailable"
            return nil
        }
        guard let first = readOnce(), let second = readOnce(),
              first.sameWorkerGraph(as: second), monotonicMS() < deadlineMS else {
            markTargetDeadlineIfExpired()
            if refusalReason == "background AX scope unavailable" {
                refusalReason = "background AX identity changed or could not be revalidated"
            }
            return nil
        }
        diagnosticTrace.stage = .initialStability
        diagnosticTrace.initial_stability_passed = true
        return second
    }

    /// Performs a fresh complete resolution and requires the same window,
    /// target, parent/sibling graph, properties, and advertised action.
    func revalidate(_ expected: BackgroundAXCurrentTarget) -> BackgroundAXCurrentTarget? {
        diagnosticTrace.stage = .finalRevalidation
        diagnosticTrace.process_identity_matches = false
        diagnosticTrace.final_revalidation_passed = false
        diagnosticTrace.commit_ready = false
        guard monotonicMS() < deadlineMS, let current = readOnce(),
              expected.sameWorkerGraph(as: current), monotonicMS() < deadlineMS else {
            markTargetDeadlineIfExpired()
            if refusalReason == "background AX scope unavailable" {
                refusalReason = "background AX target changed before commit"
            }
            return nil
        }
        diagnosticTrace.stage = .finalRevalidation
        diagnosticTrace.final_revalidation_passed = true
        return current
    }

    func markCommitReady() {
        diagnosticTrace.stage = .commitReady
        diagnosticTrace.commit_ready = true
    }

    private func markTargetDeadlineIfExpired() {
        if monotonicMS() >= deadlineMS { diagnosticTrace.stage = .targetDeadline }
    }

    private struct WalkEntry {
        let element: AXUIElement
        let parent: AXUIElement
        let depth: Int
    }

    private struct Candidate {
        let element: AXUIElement
        let context: NativeBindingContextSnapshot
        let actions: [String]
    }

    private func readOnce() -> BackgroundAXCurrentTarget? {
        let pid = authorization.pid
        diagnosticTrace.stage = .processIdentity
        diagnosticTrace.process_identity_matches = false
        diagnosticTrace.application_boundary_matches = false
        diagnosticTrace.window_boundary_matches = false
        diagnosticTrace.target_traversal_complete = false
        diagnosticTrace.target_matches = false
        guard monotonicMS() < deadlineMS,
              let app = NSRunningApplication(processIdentifier: pid),
              !app.isTerminated, app.bundleIdentifier == authorization.bundleID,
              DeviceProcessStamp.read(pid) == authorization.birth else {
            refusalReason = "target process, bundle, AX permission, or birth stamp changed"
            return nil
        }
        diagnosticTrace.process_identity_matches = true
        guard AXIsProcessTrusted(), monotonicMS() < deadlineMS else {
            refusalReason = "target process, bundle, AX permission, or birth stamp changed"
            return nil
        }

        let application = AXUIElementCreateApplication(pid)
        diagnosticTrace.stage = .applicationBoundary
        // AXHidden is application-specific; AXMinimized is window-specific.
        guard sameProcess(application),
              requiredText(application, kAXRoleAttribute) == "AXApplication",
              let applicationHidden = boolean(application, kAXHiddenAttribute),
              applicationHidden == false,
              let windows = elementList(application, attribute: kAXWindowsAttribute,
                                        maximum: BackgroundAXPolicy.maximumWindowCount),
              !windows.isEmpty,
              let drawingFrame = authorizedDrawingWindowFrame() else {
            markTargetDeadlineIfExpired()
            refusalReason = "the authorized application window boundary is incomplete"
            return nil
        }
        diagnosticTrace.application_boundary_matches = true

        diagnosticTrace.stage = .windowBoundary
        var windowCandidates: [(element: AXUIElement, frame: HitRect, title: String?)] = []
        for candidate in windows {
            guard sameProcess(candidate),
                  elementAttribute(candidate, kAXParentAttribute).map({ CFEqual($0, application) }) == true,
                  let role = requiredText(candidate, kAXRoleAttribute),
                  let subrole = requiredText(candidate, kAXSubroleAttribute),
                  let candidateFrame = frame(candidate) else {
                markTargetDeadlineIfExpired()
                refusalReason = "an AX application window has incomplete identity facts"
                return nil
            }
            let titleRead = optionalText(candidate, kAXTitleAttribute)
            guard titleRead.valid else {
                markTargetDeadlineIfExpired()
                refusalReason = "an AX application window title is malformed or unavailable"
                return nil
            }
            let titleMatches = authorization.windowTitle.map {
                bindingLabel($0) == bindingLabel(titleRead.value ?? "")
            } ?? true
            guard candidateFrame.matches(authorization.windowFrame),
                  candidateFrame.matches(drawingFrame), titleMatches else { continue }
            guard let modal = boolean(candidate, kAXModalAttribute),
                  let minimized = boolean(candidate, kAXMinimizedAttribute),
                  BackgroundAXPolicy.visibleStandardWindow(role: role, subrole: subrole,
                      modal: modal, applicationHidden: applicationHidden, minimized: minimized,
                      actualFrame: candidateFrame, expectedFrame: authorization.windowFrame) else {
                markTargetDeadlineIfExpired()
                refusalReason = "the matching AX window is modal, its application is hidden, it is minimized, or it is not standard"
                return nil
            }
            windowCandidates.append((candidate, candidateFrame, titleRead.value))
        }
        guard windowCandidates.count == 1, let selectedWindow = windowCandidates.first,
              drawingFrame.matches(authorization.windowFrame),
              DeviceProcessStamp.read(pid) == authorization.birth else {
            markTargetDeadlineIfExpired()
            refusalReason = "the authorized visible standard window is absent or ambiguous"
            return nil
        }
        diagnosticTrace.window_boundary_matches = true
        let window = selectedWindow.element
        let windowFrame = selectedWindow.frame

        diagnosticTrace.stage = .targetTraversal
        guard let resolved = resolveUniqueTarget(application: application, window: window),
              CFEqual(resolved.candidate.context.window, window),
              DeviceProcessStamp.read(pid) == authorization.birth,
              monotonicMS() < deadlineMS else {
            markTargetDeadlineIfExpired()
            if refusalReason == "background AX scope unavailable" {
                refusalReason = "the public semantic scope is absent or ambiguous"
            }
            return nil
        }
        diagnosticTrace.target_matches = true
        diagnosticTrace.stage = .targetMatch
        return BackgroundAXCurrentTarget(application: application, window: window,
            target: resolved.candidate.element, context: resolved.candidate.context,
            fingerprint: resolved.candidate.context.fingerprint, actionNames: resolved.candidate.actions,
            processStamp: authorization.birth, windowFrame: windowFrame,
            windowTitle: selectedWindow.title, auxiliaryChromeClosures: resolved.auxiliaryChromeClosures)
    }

    private struct ResolvedTarget {
        let candidate: Candidate
        let auxiliaryChromeClosures: [BackgroundAXAuxiliaryChromeClosure]
    }

    private struct AuxiliaryWalkEntry {
        let element: AXUIElement
        let owner: AXUIElement
        let depth: Int
    }

    private func resolveUniqueTarget(application: AXUIElement, window: AXUIElement) -> ResolvedTarget? {
        diagnosticTrace.stage = .targetTraversal
        var stack = [WalkEntry(element: window, parent: application, depth: 0)]
        var visited: [AXUIElement] = []
        var candidates: [Candidate] = []
        var auxiliaryChromeClosures: [BackgroundAXAuxiliaryChromeClosure] = []
        var nodeCount = 0
        let bindingReader = NativeBindingReader(pid: authorization.pid,
            bundle: authorization.bundleID, windowID: authorization.windowID,
            window: window, deadline: deadlineMS,
            allowedRoles: [authorization.target.role])

        while let entry = stack.popLast() {
            diagnosticTrace.stage = .targetNodeIdentity
            nodeCount += 1
            guard BackgroundAXPolicy.traversalIsWithinBounds(nodeCount: nodeCount,
                                                              depth: entry.depth),
                  monotonicMS() < deadlineMS, sameProcess(entry.element),
                  !visited.contains(where: { CFEqual($0, entry.element) }),
                  let actualParent = elementAttribute(entry.element, kAXParentAttribute),
                  let role = requiredText(entry.element, kAXRoleAttribute),
                  BackgroundAXPolicy.isSupportedTreeRole(role) else {
                markTargetDeadlineIfExpired()
                refusalReason = "AX tree is incomplete, repeated, foreign, or outside the fixed traversal bounds"
                return nil
            }
            var auxiliaryChildren: [AXUIElement]?
            let isAuxiliaryChromeGroup: Bool
            if CFEqual(actualParent, entry.parent) {
                isAuxiliaryChromeGroup = false
            } else {
                diagnosticTrace.stage = .auxiliaryGroupRole
                guard role == kAXGroupRole else {
                    refusalReason = "the fixture zoom child is not an AXGroup"
                    return nil
                }
                diagnosticTrace.stage = .auxiliaryGroupParent
                guard CFEqual(actualParent, window) else {
                    refusalReason = "the fixture zoom root group parent is not the selected window"
                    return nil
                }
                diagnosticTrace.stage = .auxiliaryRelation
                guard
                      let decorationRole = requiredText(entry.parent, kAXRoleAttribute),
                      let decorationSubrole = requiredText(entry.parent, kAXSubroleAttribute),
                      decorationRole == kAXButtonRole,
                      let decorationParent = elementAttribute(entry.parent, kAXParentAttribute),
                      CFEqual(decorationParent, window),
                      BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
                          bundleID: authorization.bundleID,
                          decorationRole: decorationRole,
                          decorationSubrole: decorationSubrole,
                          rootRole: role,
                          actualParentMatchesWindow: true,
                          targetRole: authorization.target.role) else {
                    markTargetDeadlineIfExpired()
                    refusalReason = "AX parent relation did not match the fixed window tree"
                    return nil
                }
                guard let closure = readAuxiliaryZoomClosure(
                    decoration: entry.parent, decorationParent: decorationParent,
                    root: entry.element, window: window, existingVisited: visited,
                    rootDepth: entry.depth) else {
                    markTargetDeadlineIfExpired()
                    if refusalReason == "background AX scope unavailable" {
                        refusalReason = "the fixture zoom auxiliary group graph is incomplete"
                    }
                    return nil
                }
                let additionalNodes = closure.groups.count - 1
                diagnosticTrace.stage = .auxiliaryGroupBounds
                guard additionalNodes >= 0,
                      nodeCount + stack.count + additionalNodes <= BackgroundAXPolicy.maximumNodes else {
                    refusalReason = "the fixture zoom auxiliary group graph exceeds the fixed traversal bound"
                    return nil
                }
                nodeCount += additionalNodes
                visited.append(contentsOf: closure.groups.map(\.element))
                auxiliaryChromeClosures.append(closure)
                auxiliaryChildren = []
                isAuxiliaryChromeGroup = true
                diagnosticTrace.stage = .targetTraversal
            }
            if !isAuxiliaryChromeGroup { visited.append(entry.element) }

            if !isAuxiliaryChromeGroup, !CFEqual(entry.element, window), role == authorization.target.role {
                diagnosticTrace.stage = .targetMatch
                let subroleRead = optionalText(entry.element, kAXSubroleAttribute)
                let titleRead = optionalText(entry.element, kAXTitleAttribute)
                let descriptionRead = optionalText(entry.element, kAXDescriptionAttribute)
                guard subroleRead.valid, titleRead.valid, descriptionRead.valid else {
                    markTargetDeadlineIfExpired()
                    refusalReason = "candidate AX properties are malformed or unavailable"
                    return nil
                }
                let label = titleRead.value ?? descriptionRead.value ?? ""
                if subroleRead.value == authorization.target.subrole,
                   bindingLabel(label) == authorization.target.label {
                    guard let context = bindingReader.readContext(entry.element),
                          context.fingerprint.valid,
                          context.matches(.wholeStructureV1(authorization.semanticContext.wholeStructureFacts),
                              binding: authorization.target,
                              displayFrame: authorization.semanticContext.target.object.frame,
                              activeLayer: nil, verifyActiveLayer: false) else {
                        markTargetDeadlineIfExpired()
                        refusalReason = "candidate AX ancestry, sibling list, or window path is incomplete"
                        return nil
                    }
                    if context.fingerprint == authorization.target {
                        guard let enabled = boolean(entry.element, kAXEnabledAttribute), enabled,
                              authorization.targetEnabled,
                              let actions = actionNames(entry.element),
                              BackgroundAXPolicy.actionNamesMatch(
                                  expected: authorization.targetActionNames, actual: actions) else {
                            markTargetDeadlineIfExpired()
                            refusalReason = "the unique target is disabled or its advertised AXPress actions changed"
                            return nil
                        }
                        candidates.append(Candidate(element: entry.element, context: context,
                                                     actions: actions))
                    }
                }
            }

            guard let children = auxiliaryChildren ?? elementList(entry.element, attribute: kAXChildrenAttribute,
                                              maximum: BackgroundAXPolicy.maximumNodes,
                                              targetTraversalChildren: true),
                  nodeCount + stack.count + children.count <= BackgroundAXPolicy.maximumNodes else {
                markTargetDeadlineIfExpired()
                refusalReason = "AX children could not be read completely within the node limit"
                return nil
            }
            for child in children.reversed() {
                stack.append(WalkEntry(element: child, parent: entry.element,
                                       depth: entry.depth + 1))
            }
        }

        diagnosticTrace.target_traversal_complete = true
        diagnosticTrace.stage = .targetMatch
        let fingerprints = candidates.map(\.context.fingerprint)
        guard let selected = BackgroundAXPolicy.uniqueTargetIndex(
            fingerprints, expected: authorization.target) else {
            refusalReason = "the public semantic scope did not resolve to exactly one current AX entity"
            return nil
        }
        return ResolvedTarget(candidate: candidates[selected], auxiliaryChromeClosures: auxiliaryChromeClosures)
    }

    private func readAuxiliaryZoomClosure(
        decoration: AXUIElement,
        decorationParent: AXUIElement,
        root: AXUIElement,
        window: AXUIElement,
        existingVisited: [AXUIElement],
        rootDepth: Int
    ) -> BackgroundAXAuxiliaryChromeClosure? {
        diagnosticTrace.stage = .auxiliaryRelation
        guard CFEqual(decorationParent, window), sameProcess(decoration), sameProcess(decorationParent) else {
            refusalReason = "the fixture zoom decoration parent changed"
            return nil
        }
        diagnosticTrace.stage = .auxiliaryDecorationChildren
        guard let decorationChildren = stableAuxiliaryChildren(decoration,
                  stage: .auxiliaryDecorationChildren, maximumCount: 1,
                  overflowStage: .auxiliaryDecorationChildren),
              BackgroundAXPolicy.auxiliaryDecorationHasOnlyRoot(
                  childCount: decorationChildren.elements.count,
                  rootReferenceMatches: decorationChildren.elements.count == 1
                      && CFEqual(decorationChildren.elements[0], root)) else {
            refusalReason = "the fixture zoom decoration child edge is incomplete"
            return nil
        }

        var stack = [AuxiliaryWalkEntry(element: root, owner: decoration, depth: 1)]
        var groups: [BackgroundAXAuxiliaryChromeGroup] = []
        var allSeen = existingVisited
        while let entry = stack.popLast() {
            diagnosticTrace.stage = .auxiliaryGroupBounds
            let projectedCount = groups.count + 1 + stack.count
            let overallDepth = rootDepth + entry.depth - 1
            guard BackgroundAXPolicy.auxiliaryGroupTreeIsWithinBounds(
                      groupCount: projectedCount, groupDepth: entry.depth, overallDepth: overallDepth) else {
                refusalReason = "the fixture auxiliary graph exceeds its fixed node or depth bound"
                return nil
            }
            guard monotonicMS() < deadlineMS else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary graph exceeded its deadline"
                return nil
            }

            diagnosticTrace.stage = .auxiliaryGroupIdentity
            guard sameProcess(entry.owner), sameProcess(entry.element),
                  !allSeen.contains(where: { CFEqual($0, entry.element) }) else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary group identity is foreign or repeated"
                return nil
            }

            diagnosticTrace.stage = .auxiliaryGroupRole
            guard let role = requiredText(entry.element, kAXRoleAttribute), role == kAXGroupRole else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary node role is not AXGroup"
                return nil
            }

            diagnosticTrace.stage = .auxiliaryGroupParent
            guard let actualParent = elementAttribute(entry.element, kAXParentAttribute),
                  sameProcess(actualParent) else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary group parent is unavailable"
                return nil
            }
            let parentIsBounded = CFEqual(actualParent, window)
                || groups.contains(where: { CFEqual(actualParent, $0.element) })
            guard parentIsBounded else {
                refusalReason = "the fixture auxiliary group parent is outside the proven closure"
                return nil
            }

            guard let windowMembership = readAuxiliaryWindowMembership(
                entry.element, selectedWindow: window, parentChainProven: parentIsBounded) else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary group window membership is unavailable or mismatched"
                return nil
            }

            diagnosticTrace.stage = .auxiliaryGroupChildren
            guard let children = stableAuxiliaryChildren(entry.element,
                      stage: .auxiliaryGroupChildren,
                      maximumCount: BackgroundAXPolicy.maximumAuxiliaryGroupNodes,
                      overflowStage: .auxiliaryGroupBounds) else {
                markTargetDeadlineIfExpired()
                refusalReason = "the fixture auxiliary group child list is incomplete"
                return nil
            }
            if !children.elements.isEmpty { diagnosticTrace.stage = .auxiliaryNonempty }

            diagnosticTrace.stage = .auxiliaryGroupIdentity
            let childRefsAreUniqueAndNew = !children.elements.indices.contains { index in
                children.elements[..<index].contains(where: { CFEqual($0, children.elements[index]) })
                    || allSeen.contains(where: { CFEqual($0, children.elements[index]) })
                    || groups.contains(where: { CFEqual($0.element, children.elements[index]) })
            }
            guard childRefsAreUniqueAndNew else {
                refusalReason = "the fixture auxiliary group child references are repeated"
                return nil
            }
            for child in children.elements {
                diagnosticTrace.stage = .auxiliaryGroupIdentity
                guard sameProcess(child) else {
                    markTargetDeadlineIfExpired()
                    refusalReason = "the fixture auxiliary child belongs to a different process"
                    return nil
                }
                diagnosticTrace.stage = .auxiliaryGroupRole
                guard let childRole = requiredText(child, kAXRoleAttribute), childRole == kAXGroupRole else {
                    markTargetDeadlineIfExpired()
                    refusalReason = "the fixture auxiliary child role is not AXGroup"
                    return nil
                }
            }

            diagnosticTrace.stage = .auxiliaryGroupBounds
            let nextGroupCount = groups.count + 1 + stack.count + children.elements.count
            guard BackgroundAXPolicy.auxiliaryGroupNodeIsAllowed(
                      role: role,
                      actualParentIsWindowOrProvenGroup: parentIsBounded,
                      windowMembershipProven: true,
                      childrenArrayIsTyped: true,
                      childCountBeforeCopy: children.countBefore,
                      copiedChildCount: children.elements.count,
                      childCountAfterCopy: children.countAfter,
                      childrenAreGroups: true,
                      childrenAreUnique: childRefsAreUniqueAndNew,
                      groupCount: nextGroupCount,
                      groupDepth: entry.depth,
                      overallDepth: overallDepth) else {
                refusalReason = "the fixture auxiliary group closure contains an unsupported or incomplete child"
                return nil
            }

            let snapshot = BackgroundAXAuxiliaryChromeGroup(owner: entry.owner,
                element: entry.element, actualParent: actualParent,
                window: window, windowMembership: windowMembership,
                children: children.elements)
            groups.append(snapshot)
            allSeen.append(entry.element)
            for child in children.elements.reversed() {
                stack.append(AuxiliaryWalkEntry(element: child, owner: entry.element,
                                                depth: entry.depth + 1))
            }
        }

        guard !groups.isEmpty else {
            refusalReason = "the fixture zoom decoration has no auxiliary group closure"
            return nil
        }
        diagnosticTrace.stage = .targetTraversal
        return BackgroundAXAuxiliaryChromeClosure(decoration: decoration,
            decorationParent: decorationParent, window: window, groups: groups)
    }

    private func readAuxiliaryWindowMembership(
        _ group: AXUIElement,
        selectedWindow: AXUIElement,
        parentChainProven: Bool
    ) -> BackgroundAXAuxiliaryWindowMembership? {
        diagnosticTrace.stage = .auxiliaryGroupWindow
        guard configure(group) else {
            diagnosticTrace.stage = monotonicMS() >= deadlineMS
                ? .auxiliaryWindowTimeout : .auxiliaryWindowReadFailed
            return nil
        }
        var raw: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(group, kAXWindowAttribute as CFString, &raw)
        guard monotonicMS() < deadlineMS else {
            diagnosticTrace.stage = .auxiliaryWindowTimeout
            return nil
        }
        if error == .cannotComplete {
            diagnosticTrace.stage = .auxiliaryWindowTimeout
            return nil
        }

        switch error {
        case .success:
            guard let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                diagnosticTrace.stage = .auxiliaryWindowMalformed
                return nil
            }
            let reportedWindow = raw as! AXUIElement
            diagnosticTrace.stage = .auxiliaryWindowForeign
            guard configure(reportedWindow) else {
                diagnosticTrace.stage = monotonicMS() >= deadlineMS
                    ? .auxiliaryWindowTimeout : .auxiliaryWindowReadFailed
                return nil
            }
            var reportedPID: pid_t = 0
            let pidError = AXUIElementGetPid(reportedWindow, &reportedPID)
            guard monotonicMS() < deadlineMS else {
                diagnosticTrace.stage = .auxiliaryWindowTimeout
                return nil
            }
            if pidError == .cannotComplete {
                diagnosticTrace.stage = .auxiliaryWindowTimeout
                return nil
            }
            guard pidError == .success else {
                diagnosticTrace.stage = .auxiliaryWindowReadFailed
                return nil
            }
            guard reportedPID == authorization.pid else {
                diagnosticTrace.stage = .auxiliaryWindowForeign
                return nil
            }
            let selectedWindowMatches = CFEqual(reportedWindow, selectedWindow)
            guard selectedWindowMatches,
                  BackgroundAXPolicy.auxiliaryWindowMembershipIsAllowed(
                      disposition: .known, rawValueIsNil: false,
                      reportedPIDMatches: true, selectedWindowMatches: selectedWindowMatches,
                      parentChainProven: parentChainProven) else {
                diagnosticTrace.stage = .auxiliaryWindowMismatch
                return nil
            }
            return BackgroundAXAuxiliaryWindowMembership(disposition: .known,
                                                         knownWindow: reportedWindow)

        case .attributeUnsupported:
            guard raw == nil else {
                diagnosticTrace.stage = .auxiliaryWindowMalformed
                return nil
            }
            guard parentChainProven,
                  BackgroundAXPolicy.auxiliaryWindowMembershipIsAllowed(
                      disposition: .unsupported, rawValueIsNil: true,
                      reportedPIDMatches: nil, selectedWindowMatches: nil,
                      parentChainProven: parentChainProven) else {
                diagnosticTrace.stage = .auxiliaryWindowParentUnproven
                return nil
            }
            diagnosticTrace.stage = .auxiliaryWindowUnsupported
            return BackgroundAXAuxiliaryWindowMembership(disposition: .unsupported,
                                                         knownWindow: nil)

        case .noValue:
            guard raw == nil else {
                diagnosticTrace.stage = .auxiliaryWindowMalformed
                return nil
            }
            guard parentChainProven,
                  BackgroundAXPolicy.auxiliaryWindowMembershipIsAllowed(
                      disposition: .noValue, rawValueIsNil: true,
                      reportedPIDMatches: nil, selectedWindowMatches: nil,
                      parentChainProven: parentChainProven) else {
                diagnosticTrace.stage = .auxiliaryWindowParentUnproven
                return nil
            }
            diagnosticTrace.stage = .auxiliaryWindowNoValue
            return BackgroundAXAuxiliaryWindowMembership(disposition: .noValue,
                                                         knownWindow: nil)

        default:
            diagnosticTrace.stage = raw == nil
                ? .auxiliaryWindowReadFailed : .auxiliaryWindowMalformed
            return nil
        }
    }

    /// Each auxiliary node is read with count-copy-count, including a known
    /// empty terminal list. This rejects races and unsupported/no-value cases.
    private func stableAuxiliaryChildren(
        _ owner: AXUIElement,
        stage: NativeBackgroundAXTrace.Stage,
        maximumCount: Int,
        overflowStage: NativeBackgroundAXTrace.Stage
    ) -> BackgroundAXStableChildren? {
        diagnosticTrace.stage = stage
        guard configure(owner) else {
            markTargetDeadlineIfExpired()
            return nil
        }
        var firstCount: CFIndex = 0
        let firstError = AXUIElementGetAttributeValueCount(owner, kAXChildrenAttribute as CFString,
                                                            &firstCount)
        guard monotonicMS() < deadlineMS, firstError == .success,
              firstCount >= 0 else {
            markTargetDeadlineIfExpired()
            return nil
        }
        guard firstCount <= maximumCount else {
            diagnosticTrace.stage = overflowStage
            return nil
        }

        diagnosticTrace.stage = stage
        guard configure(owner) else {
            markTargetDeadlineIfExpired()
            return nil
        }
        var raw: CFTypeRef?
        let copyError = AXUIElementCopyAttributeValue(owner, kAXChildrenAttribute as CFString, &raw)
        guard monotonicMS() < deadlineMS else {
            diagnosticTrace.stage = .targetDeadline
            return nil
        }
        guard copyError == .success, let raw,
              CFGetTypeID(raw) == CFArrayGetTypeID(),
              let values = raw as? [AnyObject], values.count == firstCount,
              values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else { return nil }
        let elements = values.map { $0 as! AXUIElement }
        diagnosticTrace.stage = stage
        guard configure(owner) else {
            markTargetDeadlineIfExpired()
            return nil
        }
        var secondCount: CFIndex = 0
        let secondError = AXUIElementGetAttributeValueCount(owner, kAXChildrenAttribute as CFString,
                                                             &secondCount)
        guard monotonicMS() < deadlineMS, secondError == .success,
              secondCount == firstCount, elements.count == secondCount else {
            markTargetDeadlineIfExpired()
            return nil
        }
        return BackgroundAXStableChildren(elements: elements, countBefore: Int(firstCount),
                                          countAfter: Int(secondCount))
    }

    private func authorizedDrawingWindowFrame() -> HitRect? {
        guard monotonicMS() < deadlineMS,
              let windows = CGWindowListCopyWindowInfo(
                [.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        let matching = windows.filter {
            ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == authorization.pid
                && ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == authorization.windowID
        }
        guard matching.count == 1, let info = matching.first,
              (info[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
              (info[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary),
              monotonicMS() < deadlineMS else { return nil }
        let result = HitRect(rect)
        return result.valid ? result : nil
    }

    private func elementList(_ owner: AXUIElement, attribute name: String,
                             maximum: Int, targetTraversalChildren: Bool = false) -> [AXUIElement]? {
        if targetTraversalChildren { diagnosticTrace.stage = .targetChildrenCount }
        guard sameProcess(owner), configure(owner) else {
            markTargetDeadlineIfExpired()
            return nil
        }
        var count: CFIndex = 0
        let countError = AXUIElementGetAttributeValueCount(owner, name as CFString, &count)
        guard monotonicMS() < deadlineMS else {
            if targetTraversalChildren { diagnosticTrace.stage = .targetDeadline }
            return nil
        }
        guard countError == .success, count >= 0, count <= maximum else { return nil }
        if count == 0 {
            if targetTraversalChildren { diagnosticTrace.stage = .targetChildrenConfirm }
            return []
        }

        let now = monotonicMS()
        guard now < deadlineMS else {
            if targetTraversalChildren { diagnosticTrace.stage = .targetDeadline }
            return nil
        }
        if targetTraversalChildren { diagnosticTrace.stage = .targetChildrenRead }
        guard AXUIElementSetMessagingTimeout(owner,
                  Float(min(BackgroundAXPolicy.maximumPerCallMilliseconds, deadlineMS - now)) / 1000) == .success else {
            return nil
        }
        var raw: CFArray?
        let error = AXUIElementCopyAttributeValues(owner, name as CFString, 0, count, &raw)
        guard monotonicMS() < deadlineMS else {
            if targetTraversalChildren { diagnosticTrace.stage = .targetDeadline }
            return nil
        }
        if targetTraversalChildren { diagnosticTrace.stage = .childrenType }
        guard error == .success, let values = raw as? [AnyObject], values.count == count,
              values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else { return nil }
        if targetTraversalChildren { diagnosticTrace.stage = .targetChildrenConfirm }
        let elements = values.map { $0 as! AXUIElement }
        for index in elements.indices {
            guard !elements[..<index].contains(where: { CFEqual($0, elements[index]) }) else { return nil }
        }
        if targetTraversalChildren, monotonicMS() >= deadlineMS {
            diagnosticTrace.stage = .targetDeadline
            return nil
        }
        return elements
    }

    private func configure(_ element: AXUIElement) -> Bool {
        let now = monotonicMS()
        return now < deadlineMS
            && AXUIElementSetMessagingTimeout(element,
                Float(min(BackgroundAXPolicy.maximumPerCallMilliseconds, deadlineMS - now)) / 1000) == .success
            && monotonicMS() < deadlineMS
    }

    private func sameProcess(_ element: AXUIElement) -> Bool {
        guard configure(element) else { return false }
        var owner: pid_t = 0
        return AXUIElementGetPid(element, &owner) == .success
            && owner == authorization.pid && monotonicMS() < deadlineMS
    }

    private func copy(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard sameProcess(element), configure(element) else { return nil }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        return error == .success && value != nil && monotonicMS() < deadlineMS ? value : nil
    }

    private func elementAttribute(_ element: AXUIElement, _ name: String) -> AXUIElement? {
        guard let value = copy(element, name), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return (value as! AXUIElement)
    }

    private func requiredText(_ element: AXUIElement, _ name: String) -> String? {
        guard let value = copy(element, name), CFGetTypeID(value) == CFStringGetTypeID(),
              let text = value as? String, !text.isEmpty, text.utf8.count <= 512,
              !text.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return nil }
        return text
    }

    private func optionalText(_ element: AXUIElement, _ name: String) -> (valid: Bool, value: String?) {
        guard sameProcess(element), configure(element) else { return (false, nil) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        guard monotonicMS() < deadlineMS else { return (false, nil) }
        if error == .attributeUnsupported || error == .noValue { return (true, nil) }
        guard error == .success, let value, CFGetTypeID(value) == CFStringGetTypeID(),
              let text = value as? String, text.utf8.count <= 512,
              !text.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else {
            return (false, nil)
        }
        return (true, text)
    }

    private func boolean(_ element: AXUIElement, _ name: String) -> Bool? {
        guard let value = copy(element, name), CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
        return CFBooleanGetValue((value as! CFBoolean))
    }

    private func frame(_ element: AXUIElement) -> HitRect? {
        guard let position = copy(element, kAXPositionAttribute), CFGetTypeID(position) == AXValueGetTypeID(),
              let size = copy(element, kAXSizeAttribute), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, dimensions = CGSize.zero
        guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
              AXValueGetValue(size as! AXValue, .cgSize, &dimensions),
              monotonicMS() < deadlineMS else { return nil }
        let result = HitRect(CGRect(origin: point, size: dimensions))
        return result.valid ? result : nil
    }

    private func actionNames(_ element: AXUIElement) -> [String]? {
        guard sameProcess(element), configure(element) else { return nil }
        var values: CFArray?
        let error = AXUIElementCopyActionNames(element, &values)
        guard error == .success, monotonicMS() < deadlineMS,
              let raw = values as? [AnyObject], !raw.isEmpty,
              raw.count <= BackgroundAXPolicy.maximumActions,
              raw.allSatisfy({ CFGetTypeID($0) == CFStringGetTypeID() }) else { return nil }
        let names = raw.compactMap { $0 as? String }
        return names.count == raw.count ? names.sorted() : nil
    }
}
