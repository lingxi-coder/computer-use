import Foundation
import CoreGraphics
import ApplicationServices

/// Pure lifetime-registry checks. CFStrings substitute for identity tokens;
/// this does not message a target process or read anyone's AX tree.
func runAXSessionTests() throws -> [String] {
    let localizedRadioContexts: [(language: String, preferences: String, grouping: String, option: String)] = [
        ("en-US", "Preferences", "Summary grouping", "By week"),
        ("zh-CN", "偏好设置", "摘要分组", "按周"),
    ]
    for context in localizedRadioContexts {
        let preferences = sourceGroupContext(for: kAXGroupRole, title: nil,
            description: context.preferences, inherited: nil)
        let grouping = sourceGroupContext(for: "AXRadioGroup", title: nil,
            description: context.grouping, inherited: preferences)
        let option = sourceGroupContext(for: kAXRadioButtonRole, title: context.option,
            description: context.option, inherited: grouping)
        try requireTest(preferences == context.preferences && grouping == context.grouping
                        && option == context.grouping,
                        "Source inherits nearest localized radio-group context for \(context.language)")
    }
    let outerGroup = sourceGroupContext(for: kAXGroupRole, title: nil,
        description: "Preferences", inherited: "Window")
    let nestedGroup = sourceGroupContext(for: kAXGroupRole, title: "Account",
        description: "Account details", inherited: outerGroup)
    let unchangedChild = sourceGroupContext(for: kAXCheckBoxRole, title: "Alerts",
        description: "Alerts", inherited: nestedGroup)
    let unrelatedGroup = sourceGroupContext(for: "AXCustomGroup", title: "Unrelated",
        description: "Unrelated", inherited: outerGroup)
    let suffixedRadioRole = sourceGroupContext(for: "AXRadioGroupCustom", title: "Unrelated",
        description: "Unrelated", inherited: outerGroup)
    try requireTest(outerGroup == "Preferences" && nestedGroup == "Account"
                    && unchangedChild == "Account" && unrelatedGroup == "Preferences"
                    && suffixedRadioRole == "Preferences",
                    "Source preserves AXGroup precedence and rejects unrelated group-like roles")

    let registry = AXIdentityRegistry(capacity: 2, sessionID: "offline-session")
    let context = AXElementContext(role: "AXButton", subrole: nil,
                                  frame: Rect(CGRect(x: 1, y: 2, width: 3, height: 4)), window: "window-1")
    let privateNotificationName = "PrivateDynamicNotification-DoNotSerialize"
    let noScopeApp = AppIdentity(bundle_id: "offline.test", name: "Offline", pid: 1)
    let noScopeRead = AXSessionRead(read: StructureRead(
        structure: Structure(app: noScopeApp), started: 1, finished: 2, secureFrames: []),
        tree_revision: 0, observer_revision: 0, observer_available: false,
        observer_reason: nil, started_unix_ms: 1, finished_unix_ms: 2)
    try requireTest((noScopeRead.owned_document_evidence.map { _ in false } ?? true)
                    && noScopeRead.owned_document_identity_boundary == nil
                    && noScopeRead.identity_boundary == nil
                    && !noScopeRead.observer_root_available
                    && noScopeRead.read.structure.observer_coverage.state == .unavailable
                    && noScopeRead.read.structure.observer_coverage.covered_objects == 0
                    && noScopeRead.read.structure.owned_document_evidence == nil,
                    "omitted trusted registration and read scope leave the sidecar unavailable")
    let notificationRecord = AXInvalidatingNotificationDiagnostic(
        notification_kind: axInvalidatingNotificationKind(privateNotificationName),
        registry_generation: UInt64.max,
        observer_revision: UInt64.max,
        matches_retained_registry: true,
        matches_application: false,
        retained_role_category: .textReceiver,
        retained_window_matches_selected_root: true,
        retained_owner_root_matches_selected_cf: true)
    let notificationData = try JSONEncoder().encode(notificationRecord)
    let notificationJSON = String(decoding: notificationData, as: UTF8.self)
    try requireTest(axInvalidatingNotificationKind(kAXLayoutChangedNotification) == .layoutChanged
                    && notificationJSON.contains("\"notification_kind\":\"unknown\"")
                    && notificationJSON.contains("\"retained_role_category\":\"text_receiver\"")
                    && !notificationJSON.contains(privateNotificationName)
                    && notificationJSON.contains("\"registry_generation\":18446744073709551615")
                    && notificationJSON.contains("\"observer_revision\":18446744073709551615")
                    && notificationJSON.contains("\"matches_retained_registry\":true")
                    && notificationJSON.contains("\"matches_application\":false")
                    && notificationJSON.contains("\"retained_window_matches_selected_root\":true")
                    && notificationJSON.contains("\"retained_owner_root_matches_selected_cf\":true")
                    && notificationData.count + 1 <= 768,
                    "invalidating AX diagnostics use fixed kinds and stay bounded without raw names")
    try requireTest(axRetainedRoleCategory(matchCount: 1, role: kAXWindowRole) == .windowRoot
                    && axRetainedRoleCategory(matchCount: 1, role: kAXTextFieldRole) == .textReceiver
                    && axRetainedRoleCategory(matchCount: 1, role: kAXTextAreaRole) == .textReceiver
                    && axRetainedRoleCategory(matchCount: 1, role: kAXButtonRole) == .button
                    && axRetainedRoleCategory(matchCount: 1, role: kAXGroupRole) == .group
                    && axRetainedRoleCategory(matchCount: 1, role: kAXStaticTextRole) == .staticText
                    && axRetainedRoleCategory(matchCount: 1, role: "PrivateCustomRole") == .other
                    && axRetainedRoleCategory(matchCount: 0, role: nil) == .none
                    && axRetainedRoleCategory(matchCount: 2, role: kAXTextFieldRole) == .unknown,
                    "retained AX roles map to fixed categories and duplicate matches stay unknown")
    let privateWindowID = "window-private-id-do-not-serialize"
    try requireTest(axRetainedWindowMatchesSelectedRoot(
                        matchCount: 1, cachedWindow: privateWindowID, selectedRootWindowID: privateWindowID) == true
                    && axRetainedWindowMatchesSelectedRoot(
                        matchCount: 1, cachedWindow: privateWindowID, selectedRootWindowID: "window-other") == false
                    && axRetainedWindowMatchesSelectedRoot(
                        matchCount: 0, cachedWindow: nil, selectedRootWindowID: privateWindowID) == false
                    && axRetainedWindowMatchesSelectedRoot(
                        matchCount: 1, cachedWindow: privateWindowID, selectedRootWindowID: nil) == nil
                    && axRetainedWindowMatchesSelectedRoot(
                        matchCount: 2, cachedWindow: privateWindowID, selectedRootWindowID: privateWindowID) == nil
                    && axRetainedWindowMatchesSelectedRoot(
                        matchCount: 1, cachedWindow: nil, selectedRootWindowID: privateWindowID) == nil
                    && !notificationJSON.contains(privateWindowID),
                    "selected-root diagnostic is nil without unique proof and never serializes window IDs")
    let actualOwnerRoot = "actual-ax-owner-root-do-not-serialize" as CFString
    let anotherOwnerRoot = "different-actual-ax-root" as CFString
    let ownerRegistry = AXIdentityRegistry(capacity: 2, sessionID: "owner-root-session")
    let ownerElement = "owned-text-receiver" as CFString
    let ownerContext = AXElementContext(role: kAXTextFieldRole, subrole: nil,
        frame: context.frame, window: "cg-window-string-is-not-owner-proof")
    _ = ownerRegistry.assign(ownerElement, context: ownerContext,
                             ownerRoot: MenuRetainedToken(actualOwnerRoot).element)
    try requireTest(ownerRegistry.diagnosticOwnerRootMatch(
                        for: ownerElement, selectedRoot: actualOwnerRoot) == true
                    && ownerRegistry.diagnosticOwnerRootMatch(
                        for: ownerElement, selectedRoot: anotherOwnerRoot) == false
                    && ownerRegistry.diagnosticOwnerRootMatch(
                        for: "unretained-element" as CFString, selectedRoot: actualOwnerRoot) == nil
                    && ownerRegistry.diagnosticOwnerRootMatch(
                        for: ownerElement, selectedRoot: nil) == nil
                    && axOwnerRootMatchesSelectedRoot(matchCount: 1, cachedOwnerRoot: actualOwnerRoot,
                                                     selectedRoot: actualOwnerRoot) == true
                    && axOwnerRootMatchesSelectedRoot(matchCount: 1, cachedOwnerRoot: actualOwnerRoot,
                                                     selectedRoot: anotherOwnerRoot) == false
                    && axOwnerRootMatchesSelectedRoot(matchCount: 0, cachedOwnerRoot: nil,
                                                     selectedRoot: actualOwnerRoot) == nil
                    && axOwnerRootMatchesSelectedRoot(matchCount: 2, cachedOwnerRoot: actualOwnerRoot,
                                                     selectedRoot: actualOwnerRoot) == nil
                    && axOwnerRootMatchesSelectedRoot(matchCount: 1, cachedOwnerRoot: nil,
                                                     selectedRoot: actualOwnerRoot) == nil
                    && !notificationJSON.contains("actual-ax-owner-root-do-not-serialize"),
                    "owner-root proof uses unique actual CF ownership, never window strings or serialized IDs")

    let createdCallbackRoot = "created-window-callback-private-token" as CFString
    let matchingCreatedRoot = MenuRetainedToken(createdCallbackRoot)
    let otherCreatedRoot = MenuRetainedToken("different-actual-window-root" as CFString)
    let uniqueCreatedMatchCount = axWindowCreatedRootMatchCount(
        callbackElement: createdCallbackRoot,
        actualWindowRoots: [matchingCreatedRoot, otherCreatedRoot])
    let createdDiagnostic = AXWindowCreatedTraversalDiagnostic(
        event_registry_generation: 9,
        event_observer_revision: 21,
        traversal_registry_generation: 10,
        traversal_observer_revision_before: 22,
        traversal_observer_revision_after: 22,
        actual_ax_window_root_match_count: uniqueCreatedMatchCount,
        callback_root_matches_current_selected_cf: axWindowCreatedMatchesVerifiedSelectedRoot(
            matchCount: uniqueCreatedMatchCount, callbackElement: createdCallbackRoot,
            selectedRoot: createdCallbackRoot),
        previous_selected_root_matches_current_verified_cf: axSelectedRootsMatch(
            previous: createdCallbackRoot, currentVerified: createdCallbackRoot),
        raw_traversal_complete: true,
        observer_healthy_at_traversal_start: true,
        observer_revision_stable: true,
        observer_available_after_traversal: false,
        observer_continuity_verified_after_traversal: false)
    let createdDiagnosticData = try JSONEncoder().encode(createdDiagnostic)
    let createdDiagnosticJSON = String(decoding: createdDiagnosticData, as: UTF8.self)
    let ambiguousCreatedMatchCount = axWindowCreatedRootMatchCount(
        callbackElement: createdCallbackRoot,
        actualWindowRoots: [matchingCreatedRoot, MenuRetainedToken(createdCallbackRoot)])
    let noCreatedMatchCount = axWindowCreatedRootMatchCount(
        callbackElement: createdCallbackRoot, actualWindowRoots: [otherCreatedRoot])
    try requireTest(uniqueCreatedMatchCount == 1
                    && axWindowCreatedMatchesVerifiedSelectedRoot(
                        matchCount: uniqueCreatedMatchCount, callbackElement: createdCallbackRoot,
                        selectedRoot: createdCallbackRoot) == true
                    && axWindowCreatedMatchesVerifiedSelectedRoot(
                        matchCount: uniqueCreatedMatchCount, callbackElement: createdCallbackRoot,
                        selectedRoot: otherCreatedRoot.element) == false
                    && axWindowCreatedMatchesVerifiedSelectedRoot(
                        matchCount: noCreatedMatchCount, callbackElement: createdCallbackRoot,
                        selectedRoot: createdCallbackRoot) == nil
                    && axWindowCreatedMatchesVerifiedSelectedRoot(
                        matchCount: ambiguousCreatedMatchCount, callbackElement: createdCallbackRoot,
                        selectedRoot: createdCallbackRoot) == nil
                    && axWindowCreatedMatchesVerifiedSelectedRoot(
                        matchCount: uniqueCreatedMatchCount, callbackElement: createdCallbackRoot,
                        selectedRoot: nil) == nil
                    && axSelectedRootsMatch(previous: createdCallbackRoot,
                                            currentVerified: createdCallbackRoot) == true
                    && axSelectedRootsMatch(previous: createdCallbackRoot,
                                            currentVerified: otherCreatedRoot.element) == false
                    && axSelectedRootsMatch(previous: nil, currentVerified: createdCallbackRoot) == nil
                    && createdDiagnosticJSON.contains("\"actual_ax_window_root_match_count\":1")
                    && createdDiagnosticJSON.contains("\"observer_revision_stable\":true")
                    && createdDiagnosticJSON.contains("\"observer_continuity_verified_after_traversal\":false")
                    && createdDiagnosticJSON.contains("\"previous_selected_root_matches_current_verified_cf\":true")
                    && !createdDiagnosticJSON.contains("created-window-callback-private-token")
                    && !createdDiagnosticJSON.contains("different-actual-window-root")
                    && createdDiagnosticData.count + 1 <= 768,
                    "WindowCreated correlation requires unique actual AX CF roots and emits bounded fixed facts")

    let unknownCreatedDiagnostic = AXWindowCreatedTraversalDiagnostic(
        event_registry_generation: 9,
        event_observer_revision: 21,
        traversal_registry_generation: 10,
        traversal_observer_revision_before: 22,
        traversal_observer_revision_after: 23,
        actual_ax_window_root_match_count: ambiguousCreatedMatchCount,
        callback_root_matches_current_selected_cf: axWindowCreatedMatchesVerifiedSelectedRoot(
            matchCount: ambiguousCreatedMatchCount, callbackElement: createdCallbackRoot,
            selectedRoot: createdCallbackRoot),
        previous_selected_root_matches_current_verified_cf: axSelectedRootsMatch(
            previous: createdCallbackRoot, currentVerified: nil),
        raw_traversal_complete: false,
        observer_healthy_at_traversal_start: true,
        observer_revision_stable: false,
        observer_available_after_traversal: false,
        observer_continuity_verified_after_traversal: false)
    let unknownCreatedJSON = String(decoding: try JSONEncoder().encode(unknownCreatedDiagnostic), as: UTF8.self)
    try requireTest(unknownCreatedJSON.contains("\"callback_root_matches_current_selected_cf\":null")
                    && unknownCreatedJSON.contains("\"previous_selected_root_matches_current_verified_cf\":null"),
                    "zero, duplicate or missing-root evidence serializes as unknown, never a guessed match")

    func treeObject(_ id: String, parent: String? = nil, role: String = "AXButton",
                    frame: Rect? = nil, focused: Bool = false) -> UiObject {
        UiObject(instance_ref: id, parent: parent, window_id: "window-1", role: role,
            subrole: nil, title: nil, description: nil, value: nil, enabled: true,
            focused: focused, frame: frame ?? context.frame, actions: [], in_view: true,
            group: nil, secure: false)
    }
    func actualTreeNode(_ id: String, element: String, parent: String?, index: Int,
                        role: String = "AXButton", title: String? = nil,
                        group: String? = nil, frame: Rect? = nil) -> (UiObject, TreeContext) {
        let object = UiObject(instance_ref: id, parent: parent, window_id: "window-1", role: role,
            subrole: role == kAXWindowRole ? kAXStandardWindowSubrole : nil,
            title: title, description: nil, value: nil, enabled: true, focused: false,
            frame: frame ?? context.frame, actions: [], in_view: true, group: group, secure: false,
            identity_continuity: .readScoped)
        let treeContext = TreeContext(object, element: element as CFString,
            parentElement: parent.map { $0 as CFString }, siblingIndex: index)
        return (object, treeContext)
    }
    func id(_ result: AXIdentityRegistry.Assignment) -> String? {
        switch result {
        case .existing(let id, _), .inserted(let id, _): return id
        case .changed, .full: return nil
        }
    }
    func assignment(_ result: AXIdentityRegistry.Assignment) -> AXIdentityAssignment? {
        switch result {
        case .existing(let id, let continuity), .inserted(let id, let continuity):
            return AXIdentityAssignment(instance_ref: id, identity_continuity: continuity)
        case .changed, .full:
            return nil
        }
    }
    let original = "element-one" as CFString
    let first = id(registry.assign(original, context: context))
    try requireTest(first != nil, "AX identity initially allocated")
    try requireTest(registry.uniqueObjectRef(for: original) == first
                    && registry.uniqueObjectRef(for: "unknown-retained-element" as CFString) == nil,
                    "owned worker resolves only a unique retained CF reference to its current object ID")
    try requireTest(registry.contains(original)
                    && !registry.contains("unknown-retained-element" as CFString),
                    "diagnostic registry membership uses retained CFEqual references")
    try requireTest(registry.diagnosticSelectedRootWindowMatch(
                        for: original, selectedRootWindowID: "window-1") == true
                    && registry.diagnosticSelectedRootWindowMatch(
                        for: "unknown-retained-element" as CFString,
                        selectedRootWindowID: "window-1") == false
                    && registry.diagnosticSelectedRootWindowMatch(
                        for: original, selectedRootWindowID: nil) == nil,
                    "registry root-window diagnostic compares only cached context and supplied proof")
    try requireTest(id(registry.assign("element-one" as CFString, context: context)) == first,
                    "CFEqual identity stable across reads")
    try requireTest(id(registry.assign("element-two" as CFString, context: context)) != first,
                    "equal labels and geometry cannot merge different identities")
    try requireTest(id(registry.assign("element-three" as CFString, context: context)) == nil && registry.count == 2,
                    "AX reference registry remains bounded")
    registry.invalidate()
    try requireTest(id(registry.assign(original, context: context)) != first,
                    "destroyed or unknown generations never reuse instance IDs")
    let changed = AXElementContext(role: "AXCheckBox", subrole: nil, frame: context.frame, window: context.window)
    try requireTest(id(registry.assign(original, context: changed)) == nil,
                    "role mutation invalidates instance continuity")
    let moved = AXElementContext(role: context.role, subrole: context.subrole,
                                frame: Rect(CGRect(x: 2, y: 2, width: 3, height: 4)), window: context.window)
    try requireTest(id(registry.assign(original, context: moved)) == nil,
                    "geometry mutation invalidates instance continuity")
    let reparented = AXElementContext(role: context.role, subrole: context.subrole,
                                     frame: context.frame, window: "window-2")
    try requireTest(id(registry.assign(original, context: reparented)) == nil,
                    "window change invalidates instance continuity")
    let contextChangeRegistry = AXIdentityRegistry(capacity: 2, sessionID: "context-change-session")
    let contextChangeElement = "same-lifetime-different-context" as CFString
    contextChangeRegistry.beginRead()
    let contextStableID = id(contextChangeRegistry.assign(contextChangeElement, context: context))
    contextChangeRegistry.beginRead()
    let changedRoleID = id(contextChangeRegistry.assign(contextChangeElement, context: changed))
    contextChangeRegistry.beginRead()
    let changedFrameID = id(contextChangeRegistry.assign(contextChangeElement, context: moved))
    try requireTest(contextStableID != nil && changedRoleID == contextStableID
                    && changedFrameID == contextStableID,
                    "CFEqual lifetime identity survives role and geometry updates while semantic baselines change")
    let nextSession = AXIdentityRegistry(capacity: 2)
    try requireTest(id(nextSession.assign(original, context: context)) != first,
                    "another resident helper cannot reuse instance IDs")

    let mixedRegistry = AXIdentityRegistry(capacity: 4,
        sessionID: "550e8400-e29b-41d4-a716-446655440001")
    let coveredElement = "covered-object" as CFString
    let unsupportedElement = "unsupported-object" as CFString
    mixedRegistry.beginRead()
    guard let coveredAssignment = assignment(mixedRegistry.assign(coveredElement, context: context,
            continuityCovered: true)),
          let unsupportedFirst = assignment(mixedRegistry.assign(unsupportedElement, context: context,
            continuityCovered: false)) else {
        throw NativeFailure("mixed AX identity assignments unavailable")
    }
    let unchangedGeneration = mixedRegistry.generation
    let initialMixedBoundary = mixedRegistry.identityBoundary(globalRootVerified: true)
    mixedRegistry.beginRead()
    guard let unsupportedSecond = assignment(mixedRegistry.assign("unsupported-object" as CFString,
            context: context, continuityCovered: false)) else {
        throw NativeFailure("read-scoped AX identity assignment unavailable")
    }
    try requireTest(coveredAssignment.identity_continuity == .notificationCovered
                    && unsupportedFirst.identity_continuity == .readScoped
                    && unsupportedSecond.identity_continuity == .readScoped
                    && unsupportedFirst.instance_ref != unsupportedSecond.instance_ref
                    && mixedRegistry.generation == unchangedGeneration
                    && mixedRegistry.identityBoundary(globalRootVerified: true) == initialMixedBoundary,
                    "unsupported CFEqual objects get fresh read tokens without moving the global boundary")

    var coveredObject = treeObject(coveredAssignment.instance_ref)
    coveredObject.identity_continuity = .notificationCovered
    var unsupportedObject = treeObject(unsupportedSecond.instance_ref)
    unsupportedObject.identity_continuity = .readScoped
    let projectedCoverage = ObserverCoverage.project(objects: [coveredObject, unsupportedObject],
        rootAvailable: true, failures: [
            ObserverRegistrationFailure(instance_ref: unsupportedObject.instance_ref,
                notification: .uiElementDestroyed, error_code: -25202),
            ObserverRegistrationFailure(instance_ref: unsupportedObject.instance_ref,
                notification: .uiElementDestroyed, error_code: -25202),
            ObserverRegistrationFailure(instance_ref: coveredObject.instance_ref,
                notification: .uiElementDestroyed, error_code: -25202),
            ObserverRegistrationFailure(instance_ref: "unprojected-object",
                notification: .layoutChanged, error_code: -25202),
            ObserverRegistrationFailure(instance_ref: unsupportedObject.instance_ref,
                notification: .layoutChanged, error_code: 0)
        ])
    try requireTest(projectedCoverage.state == .degraded
                    && projectedCoverage.covered_objects == 1
                    && projectedCoverage.read_scoped_objects == 1
                    && projectedCoverage.failed_registrations == 1
                    && projectedCoverage.failures == [ObserverRegistrationFailure(
                        instance_ref: unsupportedObject.instance_ref,
                        notification: .uiElementDestroyed, error_code: -25202)],
                    "mixed observer coverage counts only final read-scoped objects and real unique failures")
    let readScopedOnly = ObserverCoverage.project(objects: [unsupportedObject],
        rootAvailable: false, failures: [])
    try requireTest(readScopedOnly.state == .unavailable && readScopedOnly.covered_objects == 0
                    && readScopedOnly.read_scoped_objects == 1,
                    "unavailable global root channel never upgrades read-scoped objects")
    let manyScopedObjects = (0..<20).map { index -> UiObject in
        treeObject("scoped-\(index)")
    }
    let manyFailures = manyScopedObjects.map {
        ObserverRegistrationFailure(instance_ref: $0.instance_ref,
            notification: .uiElementDestroyed, error_code: -25202)
    }
    let boundedCoverage = ObserverCoverage.project(objects: manyScopedObjects,
        rootAvailable: true, failures: manyFailures)
    try requireTest(boundedCoverage.state == .degraded
                    && boundedCoverage.failed_registrations == 20
                    && boundedCoverage.failures.count == 16,
                    "observer failure details cap at 16 while retaining the exact bounded count")

    let selectiveRegistry = AXIdentityRegistry(capacity: 4, sessionID: "selective-invalidation-session")
    let doomedElement = "known-destroyed-object" as CFString
    let retainedElement = "unrelated-covered-object" as CFString
    selectiveRegistry.beginRead()
    guard let doomedID = id(selectiveRegistry.assign(doomedElement, context: context)),
          let retainedID = id(selectiveRegistry.assign(retainedElement, context: context)) else {
        throw NativeFailure("selective invalidation baseline unavailable")
    }
    let selectiveGeneration = selectiveRegistry.generation
    selectiveRegistry.invalidateObjects(["known-destroyed-object" as CFString])
    try requireTest(selectiveRegistry.generation == selectiveGeneration
                    && selectiveRegistry.uniqueObjectRef(for: doomedElement) == nil
                    && selectiveRegistry.uniqueObjectRef(for: retainedElement) == retainedID,
                    "known destruction removes its object while preserving unrelated IDs and generation")
    selectiveRegistry.beginRead()
    let recreatedID = id(selectiveRegistry.assign("known-destroyed-object" as CFString, context: context))
    let retainedAgainID = id(selectiveRegistry.assign(retainedElement, context: context))
    try requireTest(recreatedID != doomedID && retainedAgainID == retainedID,
                    "selective invalidation mints a new local ID without disturbing its sibling")
    let beforeGlobalReset = selectiveRegistry.generation
    selectiveRegistry.invalidate()
    selectiveRegistry.beginRead()
    let afterGlobalResetID = id(selectiveRegistry.assign(retainedElement, context: context))
    try requireTest(selectiveRegistry.generation == beforeGlobalReset + 1
                    && afterGlobalResetID != retainedID,
                    "global uncertainty advances the boundary and resets every retained identity")

    let boundedReadRegistry = AXIdentityRegistry(capacity: 4096, sessionID: "bounded-read-session")
    boundedReadRegistry.beginRead()
    for index in 0..<512 {
        _ = boundedReadRegistry.assign("read-local-\(index)" as CFString, context: context,
                                       continuityCovered: false)
    }
    let overReadCapacity = boundedReadRegistry.assign("read-local-overflow" as CFString,
        context: context, continuityCovered: false)
    try requireTest(boundedReadRegistry.capacity == 512 && boundedReadRegistry.readCount == 512
                    && id(overReadCapacity) == nil,
                    "persistent registry and per-read identity tokens both remain bounded at 512")

    let candidateRegistry = AXIdentityRegistry(capacity: 4, sessionID: "inventory-session")
    let candidateOriginal = "inventory-element" as CFString
    let candidateID = id(candidateRegistry.assign(candidateOriginal, context: context))
    let generationBeforeCandidate = candidateRegistry.generation
    let candidateCount = candidateRegistry.beginOneReadCandidateRevalidation()
    try requireTest(candidateCount == 1 && candidateRegistry.generation == generationBeforeCandidate + 1,
                    "inventory revalidation advances generation while retaining one bounded candidate")
    candidateRegistry.beginRead()
    try requireTest(id(candidateRegistry.assign("inventory-element" as CFString, context: context)) == candidateID,
                    "inventory candidate reuses only the same CFEqual reference and exact context")
    let newReferenceID = id(candidateRegistry.assign("same descriptors new CF reference" as CFString, context: context))
    try requireTest(newReferenceID != nil && newReferenceID != candidateID,
                    "matching descriptors without CFEqual never reuse an inventory candidate")
    let reused = candidateRegistry.finishOneReadCandidateRevalidation()
    try requireTest(reused.count == 1 && reused.reused == 1 && reused.discarded == 0,
                    "candidate retention ends after one verified read")

    let rejectedCandidateRegistry = AXIdentityRegistry(capacity: 2, sessionID: "rejected-inventory-session")
    let rejectedCandidateID = id(rejectedCandidateRegistry.assign(candidateOriginal, context: context))
    rejectedCandidateRegistry.beginOneReadCandidateRevalidation()
    rejectedCandidateRegistry.beginRead()
    let rejectedNewReferenceID = id(rejectedCandidateRegistry.assign("different CF reference" as CFString, context: context))
    let rejectedSummary = rejectedCandidateRegistry.finishOneReadCandidateRevalidation()
    try requireTest(rejectedNewReferenceID != rejectedCandidateID && rejectedSummary.reused == 0
                    && rejectedSummary.discarded == 1,
                    "a changed CF reference discards the old candidate instead of descriptor rebinding")
    let baselineTree = [TreeContext(treeObject("root", role: kAXWindowRole)),
                        TreeContext(treeObject("child", parent: "root"))]
    let addedTree = baselineTree + [TreeContext(treeObject("new-child", parent: "root"))]
    let removedTree = [baselineTree[0]]
    let replacedTree = [baselineTree[0], TreeContext(treeObject("replacement-child", parent: "root"))]
    try requireTest(unchangedAXTreeBaseline(previous: baselineTree, previousLayer: "window-1",
                                             current: baselineTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: nil, previousLayer: nil,
                                                current: baselineTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: baselineTree, previousLayer: "window-1",
                                                current: addedTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: baselineTree, previousLayer: "window-1",
                                                current: removedTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: baselineTree, previousLayer: "window-1",
                                                current: replacedTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: baselineTree, previousLayer: "window-1",
                                                current: baselineTree, currentLayer: "other-window"),
                    "candidate read requires exact retained tree and selected layer baseline")

    let topologyRoot = actualTreeNode("root-id-1", element: "raw-root", parent: nil, index: 0,
        role: kAXWindowRole, title: "Root")
    let unknownSibling = TreeContext(topologyRoot.0, element: "raw-root" as CFString,
        parentElement: nil, siblingIndex: nil)
    try requireTest(!unchangedAXTreeBaseline(previous: [unknownSibling], previousLayer: "window-1",
                                               current: [unknownSibling], currentLayer: "window-1"),
                    "unknown scoped sibling order cannot establish an unchanged whole-tree baseline")
    let actualFirst = actualTreeNode("first-id-1", element: "raw-first", parent: "raw-root", index: 0,
        title: "Save", group: "File")
    let actualSecond = actualTreeNode("second-id-1", element: "raw-second", parent: "raw-root", index: 1,
        title: "Open", group: "File")
    let actualBaseline = [topologyRoot.1, actualFirst.1, actualSecond.1]
    let rotatedRoot = actualTreeNode("root-id-2", element: "raw-root", parent: nil, index: 0,
        role: kAXWindowRole, title: "Root")
    let rotatedFirst = actualTreeNode("first-id-2", element: "raw-first", parent: "raw-root", index: 0,
        title: "Save", group: "File")
    let rotatedSecond = actualTreeNode("second-id-2", element: "raw-second", parent: "raw-root", index: 1,
        title: "Open", group: "File")
    let rotatedTree = [rotatedRoot.1, rotatedFirst.1, rotatedSecond.1]
    let addedActual = actualTreeNode("third-id", element: "raw-third", parent: "raw-root", index: 2)
    let reparentedActual = actualTreeNode("first-id-3", element: "raw-first", parent: "raw-other-parent", index: 0,
        title: "Save", group: "File")
    let reorderedFirst = actualTreeNode("first-id-4", element: "raw-first", parent: "raw-root", index: 1,
        title: "Save", group: "File")
    let reorderedSecond = actualTreeNode("second-id-4", element: "raw-second", parent: "raw-root", index: 0,
        title: "Open", group: "File")
    let regroupedFirst = actualTreeNode("first-id-5", element: "raw-first", parent: "raw-root", index: 0,
        title: "Save", group: "Edit")
    let movedFirst = actualTreeNode("first-id-6", element: "raw-first", parent: "raw-root", index: 0,
        title: "Save", group: "File", frame: Rect(CGRect(x: 2, y: 2, width: 3, height: 4)))
    let titleChanged = actualTreeNode("first-id-7", element: "raw-first", parent: "raw-root", index: 0,
        title: "Save As", group: "File")
    try requireTest(unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                    current: rotatedTree, currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: actualBaseline + [addedActual.1],
                        currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: [topologyRoot.1, reparentedActual.1, actualSecond.1], currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: [topologyRoot.1, reorderedFirst.1, reorderedSecond.1], currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: [topologyRoot.1, regroupedFirst.1, actualSecond.1], currentLayer: "window-1")
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: [topologyRoot.1, movedFirst.1, actualSecond.1], currentLayer: "window-1")
                    && ContentContext(actualFirst.0) != ContentContext(titleChanged.0)
                    && !unchangedAXTreeBaseline(previous: actualBaseline, previousLayer: "window-1",
                        current: actualBaseline, currentLayer: "window-2"),
                    "actual CF topology ignores read token rotation but tracks add, reparent, order, group, frame, title and active layer")
    try requireTest(!canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: true,
                        dirty: false, contentDirty: false, complete: true,
                        treeStable: false, allCandidatesReused: true)
                    && !canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: true,
                        dirty: false, contentDirty: false, complete: true,
                        treeStable: true, allCandidatesReused: false)
                    && !canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: true,
                        dirty: false, contentDirty: false, complete: false,
                        treeStable: true, allCandidatesReused: true),
                    "tree changes, partial candidates and incomplete reads cannot promote mixed identities")
    rejectedCandidateRegistry.invalidate()
    try requireTest(id(rejectedCandidateRegistry.assign(candidateOriginal, context: context)) != rejectedCandidateID,
                    "hard invalidation still prevents candidate reuse after uncertain continuity")

    let boundarySessionID = "550e8400-e29b-41d4-a716-446655440000"
    let boundaryRegistry = AXIdentityRegistry(capacity: 2, sessionID: boundarySessionID)
    guard let initialBoundary = boundaryRegistry.identityBoundary(globalRootVerified: true) else {
        throw NativeFailure("valid AX registry boundary unavailable")
    }
    try requireTest(initialBoundary.namespace == boundarySessionID && initialBoundary.generation > 0
                    && boundaryRegistry.identityBoundary(globalRootVerified: false) == nil
                    && boundaryRegistry.identityBoundary(globalRootVerified: true) == initialBoundary
                    && AXIdentityRegistry(capacity: 2, sessionID: "unknown-session")
                        .identityBoundary(globalRootVerified: true) == nil,
                    "only independently verified roots with a real UUID registry session expose a source boundary")
    let ownedScopeBoundary = boundaryRegistry.ownedScopeIdentityBoundary(complete: true)
    try requireTest(ownedScopeBoundary == initialBoundary
                    && boundaryRegistry.ownedScopeIdentityBoundary(complete: false) == nil,
                    "dedicated scope uses the same registry namespace and generation without promoting generic completeness")
    boundaryRegistry.beginOneReadCandidateRevalidation()
    _ = boundaryRegistry.finishOneReadCandidateRevalidation()
    guard let candidateBoundary = boundaryRegistry.identityBoundary(globalRootVerified: true) else {
        throw NativeFailure("candidate AX registry boundary unavailable")
    }
    boundaryRegistry.invalidate()
    guard let invalidatedBoundary = boundaryRegistry.identityBoundary(globalRootVerified: true) else {
        throw NativeFailure("invalidated AX registry boundary unavailable")
    }
    try requireTest(candidateBoundary.namespace == initialBoundary.namespace
                    && candidateBoundary.generation > initialBoundary.generation
                    && invalidatedBoundary.generation > candidateBoundary.generation,
                    "one-read revalidation and hard invalidation advance the persistent identity boundary")

    let processProof = KernelProcessIdentity(pid: 42, start_seconds: 10, start_micros: 20)
    func windowProof(_ session: String, _ process: KernelProcessIdentity, _ window: String, _ frame: Rect) -> AXRootWindowProof? {
        AXRootWindowProof(WindowContext(source_session_id: session, session_epoch: 7, process: process,
            origin_window_id: window, origin_window_frame: frame,
            verified_source_sequence: 1, verified_observed_ms: 1))
    }
    let stableRoot = windowProof("same-session", processProof, "window-1", context.frame)
    try requireTest(sameSelectedRoot(before: stableRoot, after: stableRoot, activeLayer: "window-1"),
                    "same session process selected root and frame authorize candidate continuity")
    try requireTest(!sameSelectedRoot(before: stableRoot, after: nil, activeLayer: "window-1")
                    && !sameSelectedRoot(before: stableRoot,
                        after: windowProof("other-session", processProof, "window-1", context.frame),
                        activeLayer: "window-1")
                    && !sameSelectedRoot(before: stableRoot,
                        after: windowProof("same-session", KernelProcessIdentity(pid: 42, start_seconds: 11, start_micros: 20), "window-1", context.frame),
                        activeLayer: "window-1")
                    && !sameSelectedRoot(before: stableRoot,
                        after: windowProof("same-session", processProof, "window-2", context.frame),
                        activeLayer: "window-2")
                    && !sameSelectedRoot(before: stableRoot,
                        after: windowProof("same-session", processProof, "window-1", moved.frame),
                        activeLayer: "window-1"),
                    "missing session process root or geometry proof refuses continuity")
    let originalCaptureRoot = MenuRetainedToken("capture-root-original" as CFString)
    let sameCaptureRoot = MenuRetainedToken(originalCaptureRoot.element)
    let replacedCaptureRoot = MenuRetainedToken("capture-root-replaced" as CFString)
    var captureRootBaseline: MenuRetainedToken?
    var captureRootReplacementLatched = false
    try requireTest(observeCaptureRootContinuity(selectedRoot: nil, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == nil
                    && captureRootBaseline == nil && !captureRootReplacementLatched
                    && observeCaptureRootContinuity(selectedRoot: originalCaptureRoot, qualifies: false,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == nil
                    && captureRootBaseline == nil && !captureRootReplacementLatched,
                    "unknown or unqualified root projection does not create capture continuity")
    try requireTest(observeCaptureRootContinuity(selectedRoot: originalCaptureRoot, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == true
                    && captureRootBaseline.map({ $0 === originalCaptureRoot }) == true
                    && observeCaptureRootContinuity(selectedRoot: sameCaptureRoot, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == true
                    && observeCaptureRootContinuity(selectedRoot: nil, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == nil
                    && captureRootBaseline.map({ $0 === originalCaptureRoot }) == true
                    && !captureRootReplacementLatched,
                    "actual CF-equal roots continue while an unknown projection preserves the original baseline")
    try requireTest(sameSelectedRoot(before: stableRoot, after: stableRoot, activeLayer: "window-1")
                    && observeCaptureRootContinuity(selectedRoot: replacedCaptureRoot, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == false
                    && captureRootBaseline.map({ $0 === originalCaptureRoot }) == true
                    && captureRootReplacementLatched
                    && observeCaptureRootContinuity(selectedRoot: originalCaptureRoot, qualifies: true,
                        baseline: &captureRootBaseline,
                        replacementLatched: &captureRootReplacementLatched) == false
                    && captureRootBaseline.map({ $0 === originalCaptureRoot }) == true
                    && captureRootReplacementLatched,
                    "same session, process, window ID and frame cannot mask a same-frame CF root replacement or clear its session latch")
    try requireTest(canBeginWindowInventoryIdentityRevalidation(
                        inventoryChanged: true, continuityVerified: true, dirty: false,
                        processStable: true, frontmostStable: true, observerHealthy: true,
                        bindingBoundaryInvalidationAvailable: true,
                        hasSelectedRootProof: true)
                    && !canBeginWindowInventoryIdentityRevalidation(
                        inventoryChanged: true, continuityVerified: true, dirty: true,
                        processStable: true, frontmostStable: true, observerHealthy: true,
                        bindingBoundaryInvalidationAvailable: true,
                        hasSelectedRootProof: true)
                    && !canBeginWindowInventoryIdentityRevalidation(
                        inventoryChanged: true, continuityVerified: true, dirty: false,
                        processStable: true, frontmostStable: true, observerHealthy: false,
                        bindingBoundaryInvalidationAvailable: true,
                        hasSelectedRootProof: true)
                    && !canBeginWindowInventoryIdentityRevalidation(
                        inventoryChanged: true, continuityVerified: true, dirty: false,
                        processStable: true, frontmostStable: true, observerHealthy: true,
                        bindingBoundaryInvalidationAvailable: false,
                        hasSelectedRootProof: true),
                    "only clean inventory-only changes with binding boundary invalidation and root proof may retain candidates")

    var preexistingContentDirty = true
    let observedPriorContentChange = establishFreshContentReadBaseline(&preexistingContentDirty)
    try requireTest(observedPriorContentChange && !preexistingContentDirty
                    && canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: true,
                        dirty: false, contentDirty: preexistingContentDirty, complete: true,
                        treeStable: true, allCandidatesReused: true),
                    "a pre-read value/title change becomes the fresh content baseline")
    try requireTest(!canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: true,
                        dirty: false, contentDirty: true, complete: true,
                        treeStable: true, allCandidatesReused: true)
                    && !canAcceptWindowInventoryIdentityRevalidationRead(
                        rootStable: true, observerHealthy: true, observerRevisionStable: false,
                        dirty: false, contentDirty: false, complete: true,
                        treeStable: true, allCandidatesReused: true),
                    "content or observer notifications during the fresh read reject candidates")

    let process = ProcessStamp(seconds: 10, micros: 20)
    let window = AXWindowContext(id: 1, frame: context.frame, zIndex: 0)
    try requireTest(axPostReadContextMatches(expectedProcess: process, actualProcess: process,
                                            before: [window], after: [window]),
                    "unchanged post-read context remains admissible")
    try requireTest(!axPostReadContextMatches(expectedProcess: process, actualProcess: nil,
                                             before: [window], after: [window]),
                    "process loss discards an otherwise complete traversal")
    try requireTest(!axPostReadContextMatches(expectedProcess: process,
                                             actualProcess: ProcessStamp(seconds: 11, micros: 20),
                                             before: [window], after: [window]),
                    "PID reuse discards an otherwise complete traversal")
    let changedFrame = AXWindowContext(id: 1, frame: moved.frame, zIndex: 0)
    try requireTest(!axPostReadContextMatches(expectedProcess: process, actualProcess: process,
                                             before: [window], after: [changedFrame]),
                    "post-read geometry change invalidates notification fallback")
    let changedWindow = AXWindowContext(id: 2, frame: window.frame, zIndex: 0)
    try requireTest(!axPostReadContextMatches(expectedProcess: process, actualProcess: process,
                                             before: [window], after: [changedWindow]),
                    "same geometry cannot hide a replaced window")
    let request = try JSONDecoder().decode(NativeRequest.self,
        from: Data(#"{"protocol_version":1,"command":"observe"}"#.utf8))
    let app = AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 1)
    let textReader = AXReader(app: app, request: request)
    let exact = String(repeating: "x", count: 512)
    try requireTest(textReader.semanticText(exact, attribute: kAXTitleAttribute) == exact
                    && textReader.reasons.isEmpty, "exact AX text budget remains complete")
    for attribute in [kAXRoleAttribute, kAXSubroleAttribute, kAXTitleAttribute,
                      kAXDescriptionAttribute, kAXValueAttribute] {
        let reader = AXReader(app: app, request: request)
        let projection = reader.semanticText(exact + "distinct-target-suffix", attribute: attribute)
        try requireTest(projection == exact && reader.reasons.contains("AX text budget exceeded: \(attribute)"),
                        "truncated AX semantic fields cannot claim a complete target")
    }
    let unicodeReader = AXReader(app: app, request: request)
    let unicode = String(repeating: "é", count: 256)
    try requireTest(unicodeReader.semanticText(unicode, attribute: kAXTitleAttribute) == unicode
                    && unicodeReader.reasons.isEmpty, "AX text budget counts UTF8 bytes")
    let unicodeProjection = unicodeReader.semanticText(unicode + "a", attribute: kAXTitleAttribute)
    try requireTest(unicodeProjection.utf8.count <= 512 && !unicodeReader.reasons.isEmpty,
                    "over-budget Unicode AX text remains bounded and incomplete")
    for role in [kAXCheckBoxRole, kAXRadioButtonRole] {
        try requireTest(axNumericValue(NSNumber(value: 0), role: role) == .bool(false)
                        && axNumericValue(NSNumber(value: 1), role: role) == .bool(true),
                        "binary AX toggle values retain Boolean semantics")
        try requireTest(axNumericValue(NSNumber(value: 2), role: role) == .number(2)
                        && axNumericValue(NSNumber(value: -1), role: role) == .number(-1),
                        "mixed and unknown AX toggle states retain their numeric value")
    }
    try requireTest(axNumericValue(NSNumber(value: true), role: kAXButtonRole) == .bool(true)
                    && axNumericValue(NSNumber(value: 1), role: kAXSliderRole) == .number(1)
                    && axNumericValue(NSNumber(value: Double.nan), role: kAXCheckBoxRole) == nil,
                    "AX numbers preserve their actual type and reject nonfinite values")
    let cgFrame = Rect(CGRect(x: 100, y: 100, width: 800, height: 600))
    let axFrame = Rect(CGRect(x: 100.25, y: 100, width: 800, height: 600))
    let nativeWindow = NativeWindow(id: 42, pid: app.pid, frame: cgFrame, zIndex: 0)
    let match = matchAXWindow(axFrame, windows: [nativeWindow])
    let binding = match?.structureInfo(title: nil, modal: false, ownerBundleID: app.bundle_id)
    let axNode = AXElementContext(role: kAXWindowRole, subrole: kAXStandardWindowSubrole,
                                  frame: axFrame, window: nativeWindow.ref)
    try requireTest(binding?.frame == cgFrame && axNode.frame == axFrame && binding?.frame != axNode.frame,
                    "AX association preserves canonical CG capture and original AX object geometry")
    let ambiguousWindow = NativeWindow(id: 43, pid: app.pid, frame: axFrame, zIndex: 1)
    try requireTest(matchAXWindow(axFrame, windows: [nativeWindow, ambiguousWindow]) == nil,
                    "fractional geometry cannot resolve an ambiguous window association")
    let sheetFrame = Rect(CGRect(x: 260, y: 250, width: 320, height: 180))
    let parentFrame = Rect(CGRect(x: 100, y: 80, width: 800, height: 600))
    let sheetParent = NativeWindow(id: 50, pid: app.pid, frame: parentFrame, zIndex: 15)
    let sheetWindow = NativeWindow(id: 51, pid: app.pid, frame: sheetFrame, zIndex: 14)
    try requireTest(matchAXSheetWindow(sheetFrame, attachedTo: sheetParent,
                                       windows: [sheetWindow, sheetParent])?.id == sheetWindow.id,
                    "a sheet requires one exact, separate, front CG window attached to its AXWindow")
    let duplicateSheetWindow = NativeWindow(id: 52, pid: app.pid, frame: sheetFrame, zIndex: 13)
    let approximateSheetFrame = Rect(CGRect(x: 260.25, y: 250, width: 320, height: 180))
    let behindSheetWindow = NativeWindow(id: 53, pid: app.pid, frame: sheetFrame, zIndex: 16)
    try requireTest(matchAXSheetWindow(sheetFrame, attachedTo: sheetParent,
                                       windows: [sheetParent, sheetWindow, duplicateSheetWindow]) == nil
                    && matchAXSheetWindow(approximateSheetFrame, attachedTo: sheetParent,
                                          windows: [sheetParent, sheetWindow]) == nil
                    && matchAXSheetWindow(sheetFrame, attachedTo: sheetParent,
                                          windows: [sheetParent, behindSheetWindow]) == nil,
                    "duplicate, approximate, or behind-parent CG sheets remain unresolved")
    func eligibleFixtureSheet(bundleID: String = mvpFixtureBundleID, frontmost: Bool = true,
                              focusedWindowMatchesSheet: Bool = true, sheetRole: String = kAXSheetRole,
                              sheetFocused: Bool? = true, sheetModal: Bool? = nil,
                              sheetSubrole: String? = nil, parentRole: String? = kAXWindowRole,
                              parentSubrole: String? = kAXStandardWindowSubrole, depth: Int = 1,
                              axWindowCount: Int = 1, cgWindowCount: Int = 2) -> Bool {
        canProjectMvpAXSheet(bundleID: bundleID, frontmost: frontmost,
            focusedWindowMatchesSheet: focusedWindowMatchesSheet, sheetRole: sheetRole,
            sheetFocused: sheetFocused, sheetModal: sheetModal, sheetSubrole: sheetSubrole,
            parentRole: parentRole, parentSubrole: parentSubrole, depth: depth,
            axWindowCount: axWindowCount, cgWindowCount: cgWindowCount)
    }
    try requireTest(eligibleFixtureSheet() && eligibleFixtureSheet(sheetModal: false),
                    "the observed focused fixture sheet is eligible even when AXModal is absent")
    try requireTest(!eligibleFixtureSheet(bundleID: "other.application")
                    && !eligibleFixtureSheet(frontmost: false)
                    && !eligibleFixtureSheet(focusedWindowMatchesSheet: false)
                    && !eligibleFixtureSheet(sheetFocused: false)
                    && !eligibleFixtureSheet(sheetModal: true)
                    && !eligibleFixtureSheet(sheetRole: "AXPopover")
                    && !eligibleFixtureSheet(sheetSubrole: "AXDialog")
                    && !eligibleFixtureSheet(parentRole: "AXGroup")
                    && !eligibleFixtureSheet(parentSubrole: "AXDialog")
                    && !eligibleFixtureSheet(depth: 2)
                    && !eligibleFixtureSheet(axWindowCount: 2)
                    && !eligibleFixtureSheet(cgWindowCount: 3),
                    "unregistered, unfocused, modal-flagged, transient, nested, or ambiguous sheets fail closed")

    let auxiliaryParentFrame = Rect(CGRect(x: 80, y: 60, width: 900, height: 700))
    let auxiliaryDialogFrame = Rect(CGRect(x: 82, y: 35, width: 52, height: 20))
    let auxiliaryParentCG = NativeWindow(id: 70, pid: app.pid, frame: auxiliaryParentFrame, zIndex: 4)
    let auxiliaryDialogCG = NativeWindow(id: 71, pid: app.pid, frame: auxiliaryDialogFrame, zIndex: 3)
    let standardAuxFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryParentFrame, modal: false, focused: nil,
        exactCGMatches: [auxiliaryParentCG])
    let unfocusedStandardAuxFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryParentFrame, modal: false, focused: false,
        exactCGMatches: [auxiliaryParentCG])
    let unrelatedWindowFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryDialogFrame, modal: false, focused: nil,
        exactCGMatches: [auxiliaryDialogCG])
    func auxiliaryDialogFacts(pid: UInt32?? = nil, role: String?? = nil,
                              subrole: String?? = nil, frame: Rect?? = nil,
                              modal: Bool?? = nil, focused: Bool?? = nil,
                              exactCGMatches: [NativeWindow]? = nil) -> RegisteredAuxiliaryAXWindowFacts {
        let base = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
            subrole: "AXDialog", frame: auxiliaryDialogFrame, modal: false, focused: false,
            exactCGMatches: [auxiliaryDialogCG])
        return RegisteredAuxiliaryAXWindowFacts(pid: pid ?? base.pid, role: role ?? base.role,
            subrole: subrole ?? base.subrole, frame: frame ?? base.frame,
            modal: modal ?? base.modal, focused: focused ?? base.focused,
            exactCGMatches: exactCGMatches ?? base.exactCGMatches)
    }
    func eligibleRegisteredAuxiliaryDialog(bundleID: String = mvpFixtureBundleID,
                                    targetPID: UInt32? = nil, frontmost: Bool = true,
                                    processStable: Bool = true, focusStable: Bool = true,
                                    focusedParentBefore: Bool = true, focusedParentAfter: Bool = true,
                                    rootsStable: Bool = true, cgInventoryStable: Bool = true,
                                    visitedOnce: Bool = true,
                                    standard: RegisteredAuxiliaryAXWindowFacts? = nil,
                                    dialog: RegisteredAuxiliaryAXWindowFacts? = nil,
                                    afterRoots: [RegisteredAuxiliaryAXWindowFacts]? = nil) -> Bool {
        let roots = [standard ?? standardAuxFacts, dialog ?? auxiliaryDialogFacts()]
        return canProjectRegisteredAuxiliaryAXDialog(RegisteredAuxiliaryAXDialogProof(
            bundleID: bundleID, targetPID: targetPID ?? app.pid, frontmost: frontmost,
            processStable: processStable, focusStable: focusStable,
            focusedParentMatchesBefore: focusedParentBefore,
            focusedParentMatchesAfter: focusedParentAfter,
            rootsStable: rootsStable, cgInventoryStable: cgInventoryStable,
            auxiliaryDialogVisitedOnce: visitedOnce,
            beforeRoots: roots, afterRoots: afterRoots ?? roots))
    }
    try requireTest(eligibleRegisteredAuxiliaryDialog()
                    && eligibleRegisteredAuxiliaryDialog(standard: unfocusedStandardAuxFacts)
                    && eligibleRegisteredAuxiliaryDialog(bundleID: "com.lingxi.computer-user.guard-fixture")
                    && eligibleRegisteredAuxiliaryDialog(bundleID: "com.apple.TextEdit"),
                    "the registered app allowlist uses the same exact inactive-dialog metadata proof")
    let scopedAuxiliaryReadiness = registeredAuxiliaryAXDialogMetadataReadiness(
        reasons: [scopedTableRowsOmittedReason], hasValidTableScope: true,
        hasPendingTableScopeSearch: true, tableScopeBoundaryStable: true)
    try requireTest(registeredAuxiliaryAXDialogMetadataReadiness(reasons: [], hasValidTableScope: false,
                        hasPendingTableScopeSearch: false, tableScopeBoundaryStable: false)
                    && scopedAuxiliaryReadiness
                    && eligibleRegisteredAuxiliaryDialog(),
                    "an exact stable scoped-row omission permits only the unchanged auxiliary root proof as metadata")
    try requireTest(!registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: ["AX child list truncated"], hasValidTableScope: true,
                        hasPendingTableScopeSearch: true, tableScopeBoundaryStable: true)
                    && !registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: [scopedTableRowsOmittedReason, "AX child list truncated"],
                        hasValidTableScope: true, hasPendingTableScopeSearch: true, tableScopeBoundaryStable: true)
                    && !registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: [scopedTableRowsOmittedReason, scopedTableRowsOmittedReason],
                        hasValidTableScope: true, hasPendingTableScopeSearch: true, tableScopeBoundaryStable: true)
                    && !registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: [scopedTableRowsOmittedReason], hasValidTableScope: false,
                        hasPendingTableScopeSearch: true, tableScopeBoundaryStable: true)
                    && !registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: [scopedTableRowsOmittedReason], hasValidTableScope: true,
                        hasPendingTableScopeSearch: false, tableScopeBoundaryStable: true)
                    && !registeredAuxiliaryAXDialogMetadataReadiness(
                        reasons: [scopedTableRowsOmittedReason], hasValidTableScope: true,
                        hasPendingTableScopeSearch: true, tableScopeBoundaryStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(modal: .some(nil)))
                    && !eligibleRegisteredAuxiliaryDialog(focusStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(afterRoots: [standardAuxFacts,
                        auxiliaryDialogFacts(), unrelatedWindowFacts]),
                    "other partial causes and unstable boundaries fail, while unknown modal, changed focus, or an extra root still fail the exact proof")
    try requireTest(scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: false,
                        transientMenuFound: false, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: false,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: false,
                        transientMenuFound: false, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: false, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: false,
                        transientMenuFound: false, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: false, unsupportedModalFound: false,
                        transientMenuFound: false, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: true,
                        transientMenuFound: false, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: false,
                        transientMenuFound: true, menuDrawingFound: false)
                    && !scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: true,
                        scopeWindowRemainedActive: true, modalFound: true,
                        supportedAuxiliaryDialog: true, unsupportedModalFound: false,
                        transientMenuFound: false, menuDrawingFound: true),
                    "the scoped metadata exception preserves every boundary, unsupported modal, transient menu, and drawing-window exclusion")

    let auditStandardToken = MenuRetainedToken("audit-standard-root" as CFString)
    let auditDialogToken = MenuRetainedToken("audit-auxiliary-root" as CFString)
    let auditProcessStamp = ProcessStamp(seconds: 101, micros: 23)
    let auditCGInventory = [auxiliaryParentCG, auxiliaryDialogCG]
    let auditInput = RegisteredAuxiliaryPrivacyAuditInput(
        bundleID: mvpFixtureBundleID, processID: app.pid,
        standardRoot: RegisteredAuxiliaryPrivacyAuditRoot(token: auditStandardToken, facts: standardAuxFacts),
        dialogRoot: RegisteredAuxiliaryPrivacyAuditRoot(token: auditDialogToken, facts: auxiliaryDialogFacts()),
        processStamp: auditProcessStamp, cgInventory: auditCGInventory)
    let auditRoots = [auditInput.standardRoot, auditInput.dialogRoot]
    func auxiliaryAuditRevalidates(
        currentRoots: [RegisteredAuxiliaryPrivacyAuditRoot]? = nil,
        expectedStandard: CFTypeRef? = nil,
        focusedWindow: CFTypeRef? = nil,
        hasFocusedWindow: Bool = true,
        processStable: Bool = true,
        cgInventory: [NativeWindow]? = nil,
        frontmost: Bool = true,
        transientMenuFound: Bool = false,
        menuDrawingFound: Bool = false,
        menuProven: Bool = false
    ) -> Bool {
        registeredAuxiliaryPrivacyAuditRevalidationMatches(
            input: auditInput, currentRoots: currentRoots ?? auditRoots,
            expectedStandard: expectedStandard ?? auditStandardToken.element,
            focusedWindow: hasFocusedWindow ? (focusedWindow ?? auditStandardToken.element) : nil,
            currentProcessStamp: processStable ? auditProcessStamp : ProcessStamp(seconds: 102, micros: 23),
            currentCGInventory: cgInventory ?? auditCGInventory, frontmost: frontmost,
            transientMenuFound: transientMenuFound, menuDrawingFound: menuDrawingFound,
            menuProven: menuProven)
    }
    let sameFrameReplacement = RegisteredAuxiliaryPrivacyAuditRoot(
        token: MenuRetainedToken("replacement-standard-root" as CFString), facts: standardAuxFacts)
    let sameFrameDialogReplacement = RegisteredAuxiliaryPrivacyAuditRoot(
        token: MenuRetainedToken("replacement-auxiliary-root" as CFString), facts: auxiliaryDialogFacts())
    let unknownModalAuditRoot = RegisteredAuxiliaryPrivacyAuditRoot(
        token: auditDialogToken, facts: auxiliaryDialogFacts(modal: .some(nil)))
    let unknownFocusAuditRoot = RegisteredAuxiliaryPrivacyAuditRoot(
        token: auditDialogToken, facts: auxiliaryDialogFacts(focused: .some(nil)))
    let activeAuditRoot = RegisteredAuxiliaryPrivacyAuditRoot(
        token: auditDialogToken, facts: auxiliaryDialogFacts(focused: .some(.some(true))))
    let extraAuditRoot = RegisteredAuxiliaryPrivacyAuditRoot(
        token: MenuRetainedToken("extra-ax-window-root" as CFString), facts: unrelatedWindowFacts)
    try requireTest(auxiliaryAuditRevalidates()
                    && !auxiliaryAuditRevalidates(currentRoots: [sameFrameReplacement, auditInput.dialogRoot])
                    && !auxiliaryAuditRevalidates(currentRoots: [auditInput.standardRoot, sameFrameDialogReplacement])
                    && !auxiliaryAuditRevalidates(expectedStandard: sameFrameReplacement.token.element)
                    && !auxiliaryAuditRevalidates(focusedWindow: sameFrameReplacement.token.element)
                    && !auxiliaryAuditRevalidates(hasFocusedWindow: false)
                    && !auxiliaryAuditRevalidates(currentRoots: [auditInput.standardRoot, unknownModalAuditRoot])
                    && !auxiliaryAuditRevalidates(currentRoots: [auditInput.standardRoot, unknownFocusAuditRoot])
                    && !auxiliaryAuditRevalidates(currentRoots: [auditInput.standardRoot, activeAuditRoot])
                    && !auxiliaryAuditRevalidates(currentRoots: [auditInput.standardRoot,
                        auditInput.dialogRoot, extraAuditRoot])
                    && !auxiliaryAuditRevalidates(processStable: false)
                    && !auxiliaryAuditRevalidates(cgInventory: [auxiliaryParentCG])
                    && !auxiliaryAuditRevalidates(frontmost: false)
                    && !auxiliaryAuditRevalidates(transientMenuFound: true)
                    && !auxiliaryAuditRevalidates(menuDrawingFound: true)
                    && !auxiliaryAuditRevalidates(menuProven: true),
                    "the two-root privacy recheck requires the original CF objects, known modal/focus facts, exact CG/process inventory, and a clear menu boundary")
    try requireTest(isRegisteredAuxiliaryAXDialogRoot(bundleID: mvpFixtureBundleID,
                        role: kAXWindowRole, subrole: "AXDialog", topLevel: true)
                    && !isRegisteredAuxiliaryAXDialogRoot(bundleID: "other.application",
                        role: kAXWindowRole, subrole: "AXDialog", topLevel: true)
                    && !isRegisteredAuxiliaryAXDialogRoot(bundleID: mvpFixtureBundleID,
                        role: kAXWindowRole, subrole: kAXStandardWindowSubrole, topLevel: true)
                    && !isRegisteredAuxiliaryAXDialogRoot(bundleID: mvpFixtureBundleID,
                        role: "AXDialog", subrole: nil, topLevel: true)
                    && !isRegisteredAuxiliaryAXDialogRoot(bundleID: mvpFixtureBundleID,
                        role: kAXWindowRole, subrole: "AXDialog", topLevel: false),
                    "ordinary standard windows, actual AXDialog roles, nested roots, and unknown apps are not auxiliary metadata roots")
    let changedAuxDialog = auxiliaryDialogFacts(frame: .some(.some(Rect(CGRect(x: 83, y: 35, width: 52, height: 20)))))
    let ambiguousAuxDialog = auxiliaryDialogFacts(exactCGMatches: .some([auxiliaryDialogCG,
        NativeWindow(id: 72, pid: app.pid, frame: auxiliaryDialogFrame, zIndex: 2)]))
    let sameWindowAuxDialog = auxiliaryDialogFacts(exactCGMatches: .some([auxiliaryParentCG]))
    let modalStandardAuxFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryParentFrame, modal: true, focused: nil,
        exactCGMatches: [auxiliaryParentCG])
    let unknownModalStandardAuxFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryParentFrame, modal: nil, focused: nil,
        exactCGMatches: [auxiliaryParentCG])
    let unmappedStandardAuxFacts = RegisteredAuxiliaryAXWindowFacts(pid: app.pid, role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, frame: auxiliaryParentFrame, modal: false, focused: nil,
        exactCGMatches: [])
    let wrongCGFrameAuxDialog = auxiliaryDialogFacts(exactCGMatches: .some([
        NativeWindow(id: 71, pid: app.pid,
            frame: Rect(CGRect(x: 82, y: 35, width: 53, height: 20)), zIndex: 3)]))
    try requireTest(!eligibleRegisteredAuxiliaryDialog(bundleID: "other.application")
                    && !eligibleRegisteredAuxiliaryDialog(frontmost: false)
                    && !eligibleRegisteredAuxiliaryDialog(processStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(focusStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(focusedParentBefore: false)
                    && !eligibleRegisteredAuxiliaryDialog(focusedParentAfter: false)
                    && !eligibleRegisteredAuxiliaryDialog(rootsStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(cgInventoryStable: false)
                    && !eligibleRegisteredAuxiliaryDialog(visitedOnce: false),
                    "auxiliary dialog proof rejects app, process, focus, AX-root, CG, and traversal changes")
    try requireTest(!eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(modal: .some(nil)))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(modal: .some(.some(true))))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(focused: .some(nil)))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(focused: .some(.some(true))))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(pid: .some(.some(app.pid + 1))))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(subrole: .some(nil)))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: auxiliaryDialogFacts(frame: .some(nil)))
                    && !eligibleRegisteredAuxiliaryDialog(dialog: changedAuxDialog,
                        afterRoots: [standardAuxFacts, auxiliaryDialogFacts()])
                    && !eligibleRegisteredAuxiliaryDialog(afterRoots: [standardAuxFacts,
                        auxiliaryDialogFacts(), unrelatedWindowFacts])
                    && !eligibleRegisteredAuxiliaryDialog(dialog: ambiguousAuxDialog)
                    && !eligibleRegisteredAuxiliaryDialog(dialog: sameWindowAuxDialog)
                    && !eligibleRegisteredAuxiliaryDialog(dialog: wrongCGFrameAuxDialog)
                    && !eligibleRegisteredAuxiliaryDialog(standard: unknownModalStandardAuxFacts)
                    && !eligibleRegisteredAuxiliaryDialog(standard: unmappedStandardAuxFacts)
                    && !eligibleRegisteredAuxiliaryDialog(standard: modalStandardAuxFacts)
                    && !eligibleRegisteredAuxiliaryDialog(standard: unrelatedWindowFacts),
                    "unknown, active, modal, moved, cross-process, ambiguous, or ordinary windows never qualify as inactive AXDialog metadata")
    try requireTest(!axPostReadContextMatches(expectedProcess: process, actualProcess: process,
                    before: [AXWindowContext(id: nativeWindow.id, frame: cgFrame, zIndex: nativeWindow.zIndex)],
                    after: [AXWindowContext(id: nativeWindow.id, frame: axFrame, zIndex: nativeWindow.zIndex)]),
                    "actual fractional CG movement still discards the traversal")
    let retained = MenuRetainedToken("actual-opener" as CFString)
    try requireTest(retained.matches("actual-opener" as CFString) && !retained.matches("replacement-opener" as CFString),
                    "menu retention requires actual CF equality")
    let presentation = MenuPresentationIdentity()
    let menuContext = presentation.observe("actual-menu" as CFString)
    try requireTest(presentation.observe("actual-menu" as CFString) == menuContext,
                    "same retained menu keeps one context")
    presentation.close()
    try requireTest(presentation.observe("actual-menu" as CFString) != menuContext,
                    "closing and reopening retained menu allocates new context")
    let nextPresentation = presentation.observe("replacement-menu" as CFString)
    try requireTest(nextPresentation != menuContext && presentation.menu?.matches("actual-menu" as CFString) == false,
                    "replaced menu cannot continue a context")
    let actualRoot = "actual-root" as CFString
    let actualOpener = "actual-opener" as CFString
    func eventState(id: String = "retained-anchor", revision: UInt64 = 1) -> MenuAnchorEventState {
        MenuAnchorEventState(id: id, revision: revision,
            path: [MenuRetainedToken(actualOpener), MenuRetainedToken(actualRoot)], root: MenuRetainedToken(actualRoot))
    }
    for notification in [kAXWindowMovedNotification, kAXWindowResizedNotification] {
        let affected = eventState()
        let oldContext = affected.presentation.observe("actual-menu" as CFString)
        applyMenuAnchorNotification(notification, element: actualRoot, focusedWindow: nil, states: [affected])
        // Restoring the same original CF root and all original descriptor
        // values is not allowed to resurrect the retired event state.
        applyMenuAnchorNotification(kAXLayoutChangedNotification, element: actualRoot, focusedWindow: nil, states: [affected])
        try requireTest(affected.retired && affected.presentation.contextID == nil,
                        "observed geometry mutation cannot resume after restoring the frame")
        let fresh = eventState(id: "fresh-anchor", revision: 2)
        try requireTest(fresh.id != affected.id && fresh.revision > affected.revision
                        && fresh.presentation.observe("actual-menu" as CFString) != oldContext,
                        "geometry mutation requires new anchor and opening context identity")
    }
    let unknownGeometry = [eventState(id: "one"), eventState(id: "two")]
    applyMenuAnchorNotification(kAXWindowMovedNotification, element: "unknown-element" as CFString,
                                focusedWindow: nil, states: unknownGeometry)
    try requireTest(unknownGeometry.allSatisfy(\.retired), "unattributed geometry notification retires every anchor")
    let stableFocus = eventState()
    let stableContext = stableFocus.presentation.observe("actual-menu" as CFString)
    for notification in [kAXFocusedWindowChangedNotification, kAXFocusedUIElementChangedNotification,
                         kAXLayoutChangedNotification, kAXMenuOpenedNotification] {
        applyMenuAnchorNotification(notification, element: actualRoot, focusedWindow: actualRoot, states: [stableFocus])
    }
    try requireTest(!stableFocus.retired && stableFocus.presentation.contextID == stableContext,
                    "proved same-root menu focus and ordinary topology can be directly rechecked")
    for root: CFTypeRef? in ["different-root" as CFString, nil] {
        let changedFocus = eventState()
        applyMenuAnchorNotification(kAXFocusedWindowChangedNotification, element: actualRoot,
                                    focusedWindow: root, states: [changedFocus])
        try requireTest(changedFocus.retired, "different or unreadable focused root retires anchors")
    }
    let unknownNotification = eventState()
    applyMenuAnchorNotification("UnattributedUnknownNotification", element: actualRoot,
                                focusedWindow: actualRoot, states: [unknownNotification])
    try requireTest(unknownNotification.retired, "unknown notification has no menu continuity exemption")
    var fingerprintStructure = Structure(app: app)
    fingerprintStructure.objects = [
        UiObject(instance_ref: "root", parent: nil, window_id: "window-1", role: kAXWindowRole,
                 subrole: kAXStandardWindowSubrole, title: "Root", description: nil, value: nil,
                 enabled: true, focused: true, frame: cgFrame, actions: [], in_view: true, group: nil, secure: false),
        UiObject(instance_ref: "group", parent: "root", window_id: "window-1", role: kAXGroupRole,
                 subrole: nil, title: "Menu Group", description: nil, value: nil,
                 enabled: true, focused: false, frame: cgFrame, actions: [], in_view: true, group: nil, secure: false),
        UiObject(instance_ref: "opener", parent: "group", window_id: "window-1", role: kAXMenuButtonRole,
                 subrole: nil, title: "  FIXTURE\tNative Menu  ", description: nil, value: nil,
                 enabled: true, focused: false, frame: Rect(CGRect(x: 120.5, y: -2.5, width: 40, height: 20)),
                 actions: ["show_menu"], in_view: true, group: " Menu GROUP ", secure: false)
    ]
    let fingerprint = menuFingerprint(object: fingerprintStructure.objects[2], structure: fingerprintStructure)
    try requireTest(fingerprint?.ancestor_roles == [kAXWindowRole, kAXGroupRole] && fingerprint?.label == "fixture native menu"
                    && fingerprint?.group == "menu group" && fingerprint?.frame == [121, -3, 40, 20],
                    "menu fingerprint matches Rust root-first ancestry normalization and rounding")
    let drawing = MenuDrawingWindow(window_id: 7, owner_pid: 1, layer: 101, frame: cgFrame)
    let item = Rect(CGRect(x: 120, y: 120, width: 40, height: 20))
    try requireTest(uniqueMenuDrawingWindow(menuFrame: cgFrame, itemFrames: [item], windows: [drawing], pid: 1) == drawing,
                    "one exact registered drawing window associates the real menu")
    try requireTest(uniqueMenuDrawingWindow(menuFrame: cgFrame, itemFrames: [item], windows: [drawing, drawing], pid: 1) == nil
                    && uniqueMenuDrawingWindow(menuFrame: axFrame, itemFrames: [item], windows: [drawing], pid: 1) == nil
                    && uniqueMenuDrawingWindow(menuFrame: cgFrame, itemFrames: [item], windows: [drawing], pid: 2) == nil,
                    "ambiguous moved or foreign menu drawing windows fail closed")
    let trustedApp = AppIdentity(bundle_id: "com.lingxi.computer-user.guard-fixture", name: "Fixture", pid: 1)
    func menuRequest(command: String, pid: UInt32 = 1) throws -> NativeRequest {
        try JSONDecoder().decode(NativeRequest.self, from: Data("{\"protocol_version\":1,\"command\":\"\(command)\",\"pid\":\(pid),\"text_policy\":{\"kind\":\"registered_editor\",\"window_id\":\"window-1\"}}".utf8))
    }
    let trustedWatch = try menuRequest(command: "watch")
    let oneShotMenu = try menuRequest(command: "observe")
    let differentPID = try menuRequest(command: "watch", pid: 2)
    try requireTest(menuOriginWindow(request: trustedWatch, app: trustedApp) == "window-1"
                    && menuOriginWindow(request: oneShotMenu, app: trustedApp) == nil
                    && menuOriginWindow(request: differentPID, app: trustedApp) == nil,
                    "only trusted fixed PID watch scope can retain menu anchors")
    let originalRead = StructureRead(structure: Structure(app: app), started: 10_000_000_000,
        finished: 10_020_000_000, secureFrames: [])
    let delayedRead = finishAXSessionStructureRead(originalRead, finishedNs: 10_220_000_000)
    let originalWall: UInt64 = 1_000_020
    let delayedWall: UInt64 = 1_000_220
    let originalObserved = originalWall - (originalRead.finished - originalRead.started) / 1_000_000
    let delayedObserved = delayedWall - (delayedRead.finished - delayedRead.started) / 1_000_000
    try requireTest(delayedRead.started == originalRead.started && delayedRead.finished == 10_220_000_000 &&
                    delayedObserved == originalObserved &&
                    delayedWall - (originalRead.finished - originalRead.started) / 1_000_000 > originalObserved,
                    "paired final timestamps include optional queries without freshening old AX facts")
    let privateAXText = "Private AX title and element text must never enter rejection tags"
    let taggedApp = AppIdentity(bundle_id: "offline.test", name: privateAXText, pid: 1)
    let rejectedRead = StructureRead(structure: Structure(app: taggedApp), started: 20, finished: 30,
        secureFrames: [], privacyRejection: .visibleCoverageRejected)
    let reconstructedRejectedRead = finishAXSessionStructureRead(rejectedRead, finishedNs: 40)
    let privacyLabels = [ReadPrivacyRejection.notFrontmost.rawValue,
        ReadPrivacyRejection.focusedWindowChanged.rawValue,
        ReadPrivacyRejection.scopeBoundaryChanged.rawValue,
        ReadPrivacyRejection.modalOrMenuUnresolved.rawValue,
        ReadPrivacyRejection.visibleCoverageRejected.rawValue,
        ReadPrivacyRejection.rootContinuityChanged.rawValue,
        ReadPrivacyRejection.processUnavailable.rawValue,
        ReadPrivacyRejection.observerRootUnavailable.rawValue,
        ReadPrivacyRejection.notificationDuringRead.rawValue,
        ReadPrivacyRejection.readUnavailable.rawValue]
    let labelJSON = String(decoding: try JSONEncoder().encode(privacyLabels), as: UTF8.self)
    let readJSON = String(decoding: try JSONEncoder().encode(reconstructedRejectedRead.structure), as: UTF8.self)
    try requireTest(reconstructedRejectedRead.privacyRejection == .visibleCoverageRejected
                    && privacyLabels == ["not_frontmost", "focused_window_changed", "scope_boundary_changed",
                        "modal_or_menu_unresolved", "visible_coverage_rejected", "root_continuity_changed",
                        "process_unavailable", "observer_root_unavailable", "notification_during_read",
                        "read_unavailable"]
                    && !labelJSON.contains(privateAXText) && !labelJSON.contains("window-")
                    && readJSON.contains(privateAXText)
                    && !readJSON.contains("privacyRejection") && !readJSON.contains("visible_coverage_rejected"),
                    "fixed privacy rejection labels survive read reconstruction and stay outside Source structure output")
    return ["ax_optional_proof_queries_preserve_observation_time", "ax_invalidating_notification_diagnostic_is_bounded_and_redacted",
            "ax_group_context_inheritance_includes_exact_radio_group_role",
            "ax_group_context_inheritance_preserves_standard_groups_and_rejects_other_roles",
            "ax_privacy_rejection_tags_are_fixed_and_survive_reconstruction",
            "ax_capture_root_actual_cf_continuity_latches_same_frame_replacement",
            "ax_auxiliary_dialog_metadata_allows_only_stable_scoped_row_omission",
            "ax_auxiliary_dialog_metadata_keeps_exact_modal_focus_and_root_proof",
            "ax_auxiliary_dialog_metadata_preserves_modal_menu_and_boundary_exclusions",
            "ax_auxiliary_privacy_audit_revalidation_preserves_actual_cf_roots",
            "ax_cfequal_instance_continuity", "ax_diagnostic_registry_membership_uses_cfequal",
            "ax_destroyed_retained_role_diagnostic_is_fixed_and_ambiguous_safe",
            "ax_destroyed_retained_window_matches_selected_root_without_emitting_ids",
            "ax_destroyed_owner_root_cf_diagnostic_is_unique_and_redacted",
            "ax_window_created_callback_matches_only_unique_reader_roots",
            "ax_window_created_diagnostic_is_bounded_and_redacted",
            "ax_same_labels_do_not_merge_instances",
            "ax_registry_bounded", "ax_generation_never_reuses_ids", "ax_role_change_invalidates",
            "ax_notification_covered_identity_survives_semantic_context_changes",
            "ax_unsupported_cfequal_identity_is_fresh_per_read",
            "ax_observer_coverage_mixed_counts_and_failure_projection",
            "ax_observer_failure_detail_cap_preserves_total_count",
            "ax_known_destruction_selective_invalidation_preserves_boundary",
            "ax_global_uncertainty_resets_registry_generation",
            "ax_read_scoped_token_registry_bounded_512",
            "ax_tree_baseline_actual_cf_topology_ignores_token_rotation",
            "ax_inventory_candidate_requires_cfequal_context", "ax_inventory_candidate_is_one_read_bounded",
            "ax_changed_cf_candidate_does_not_rebind", "ax_inventory_candidate_requires_fixed_root_process_window",
            "ax_inventory_candidate_requires_exact_tree_baseline", "ax_partial_candidate_cannot_publish_mixed_tree",
            "ax_source_identity_boundary_is_persistent_and_generation_scoped",
            "ax_owned_scope_boundary_uses_same_registry_namespace_and_generation",
            "ax_inventory_candidate_content_baseline_is_fresh", "ax_inventory_candidate_rejects_in_read_changes",
            "ax_geometry_change_invalidates", "ax_window_change_invalidates", "ax_session_uuid_never_reuses_ids",
            "ax_postread_process_loss_discards", "ax_postread_pid_reuse_discards",
            "ax_postread_geometry_change_discards", "ax_postread_window_replacement_discards",
            "ax_exact_text_budget_remains_complete", "ax_truncated_semantic_fields_are_incomplete",
            "ax_utf8_text_budget_boundary", "ax_binary_toggle_values_remain_boolean",
            "ax_mixed_toggle_values_remain_numeric", "ax_numeric_value_preserves_type",
            "ax_cg_capture_binding_preserves_ax_geometry", "ax_fractional_window_match_remains_unique",
            "ax_fractional_cg_movement_discards_read", "menu_retention_requires_cfequal", "menu_context_retained_while_open",
            "menu_close_reopen_changes_context", "menu_replacement_changes_context", "menu_fingerprint_matches_rust_projection",
            "menu_drawing_unique_exact_profile", "menu_drawing_ambiguity_fails_closed", "menu_only_trusted_watch_scope",
            "menu_move_restore_retires_anchor", "menu_resize_restore_retires_anchor", "menu_geometry_requires_fresh_identity",
            "menu_unattributed_geometry_retires_all", "menu_same_root_focus_preserves_direct_read",
            "menu_different_or_unknown_focus_retires_anchor", "menu_unknown_event_retires_anchor"] + (try runMenuPrivacyTests())
}

/// A proven AX menu is an excluded drawing surface, never purported menu pixels.
/// These pure reads exercise that boundary without constructing platform AX refs.
private func runMenuPrivacyTests() throws -> [String] {
    let app = AppIdentity(bundle_id: "test.offline", name: "Offline", pid: 1)
    let rootFrame = Rect(CGRect(x: 100, y: 100, width: 800, height: 600))
    let openerFrame = Rect(CGRect(x: 120, y: 120, width: 80, height: 25))
    let menuFrame = Rect(CGRect(x: 120, y: 760, width: 100, height: 54))
    func node(_ id: String, parent: String?, role: String, frame: Rect,
              title: String? = nil, value: UiValue? = nil, secure: Bool = false) -> UiObject {
        UiObject(instance_ref: id, parent: parent, window_id: "window-1", role: role,
            subrole: role == kAXWindowRole ? kAXStandardWindowSubrole : nil, title: title, description: nil, value: value,
            enabled: true, focused: false, frame: frame, actions: role == kAXMenuButtonRole ? ["show_menu"] : [],
            in_view: true, group: role == kAXGroupRole || role == kAXWindowRole ? nil : "Menu Group", secure: secure)
    }
    var baseline = Structure(app: app)
    baseline.complete = true; baseline.incomplete_reason = nil; baseline.active_layer = "window-1"
    baseline.windows = [WindowInfo(window_id: "window-1", title: "Root", frame: rootFrame, z_index: 0,
        kind: "window", modal: false, owner_bundle_id: app.bundle_id)]
    baseline.objects = [node("root", parent: nil, role: kAXWindowRole, frame: rootFrame, title: "Root"),
        node("group", parent: "root", role: kAXGroupRole, frame: rootFrame, title: "Menu Group"),
        node("opener", parent: "group", role: kAXMenuButtonRole, frame: openerFrame, title: "Menu", value: .text("before"))]
    var opened = baseline
    opened.objects += [node("menu", parent: "opener", role: kAXMenuRole, frame: menuFrame),
        node("item", parent: "menu", role: kAXMenuItemRole, frame: Rect(CGRect(x: 120, y: 765, width: 100, height: 22)), title: "Choice")]
    guard let openerFP = menuFingerprint(object: opened.objects[2], structure: opened),
          let menuFP = menuFingerprint(object: opened.objects[3], structure: opened) else {
        throw NativeFailure("menu privacy fixture fingerprint unavailable")
    }
    let anchor = MenuOpenerAnchor(anchor_id: "anchor", source_session_id: "session", anchor_revision: 1,
        process: KernelProcessIdentity(pid: 1, start_seconds: 10, start_micros: 20), origin_window_id: "window-1",
        origin_window_frame: rootFrame, opener_instance: "opener", opener_fingerprint: openerFP, opener_frame: openerFrame,
        verified_source_sequence: 1, verified_observed_ms: 1, continuity: .directRead)
    opened.menu_anchors = [anchor]
    opened.menu_context = MenuContext(context_id: "context", source_session_id: "session", opener: anchor,
        menu_instance: "menu", menu_fingerprint: menuFP, menu_frame: menuFrame,
        drawing_window: MenuDrawingWindow(window_id: 7, owner_pid: 1, layer: 101, frame: menuFrame))
    func reading(_ structure: Structure, secure: [Rect] = []) -> StructureRead {
        StructureRead(structure: structure, started: 1, finished: 2, secureFrames: secure)
    }
    try requireTest(unchangedPrivacyContext(before: reading(baseline), after: reading(opened))
                    && unchangedPrivacyContext(before: reading(opened), after: reading(baseline)),
                    "proved menu appearance and disappearance preserve only root capture privacy")
    for mutation in ["value", "frame", "nonmenu", "sensitive", "partial", "unknown-menu", "layer", "unknown-child"] {
        var changed = opened
        var secureFrames: [Rect] = []
        switch mutation {
        case "value": changed.objects[2] = node("opener", parent: "group", role: kAXMenuButtonRole,
            frame: openerFrame, title: "Menu", value: .text("after"))
        case "frame": changed.objects[2] = node("opener", parent: "group", role: kAXMenuButtonRole,
            frame: Rect(CGRect(x: 121, y: 120, width: 80, height: 25)), title: "Menu", value: .text("before"))
        case "nonmenu": changed.objects.append(node("new-root-field", parent: "group", role: kAXStaticTextRole,
            frame: openerFrame, title: "Changed actual root content"))
        case "sensitive": secureFrames = [menuFrame]
        case "partial": changed.complete = false; changed.incomplete_reason = "truncated AX read"
        case "unknown-menu": changed.menu_context = nil
        case "layer": changed.active_layer = nil
        case "unknown-child": changed.objects.append(node("unexpected-field", parent: "menu", role: kAXTextFieldRole,
            frame: menuFrame, value: .text("unexpected"), secure: true))
        default: break
        }
        try requireTest(!unchangedPrivacyContext(before: reading(baseline), after: reading(changed, secure: secureFrames)),
                        "proved menu does not exempt \(mutation) privacy change")
    }
    var normalized = opened
    normalized.normalizeSourceProvenance(sequence: 9, observedMS: 10)
    try requireTest(normalized.menu_anchors[0].verified_source_sequence == 9 && normalized.menu_anchors[0].verified_observed_ms == 10
                    && normalized.menu_context?.opener.verified_source_sequence == 9 && normalized.menu_context?.opener.verified_observed_ms == 10,
                    "helper menu anchors and context opener share one publication stamp")
    return ["menu_capture_privacy_only_omits_proven_menu", "menu_capture_privacy_preserves_nonmenu_value",
            "menu_capture_privacy_preserves_nonmenu_frame", "menu_capture_privacy_preserves_root_content",
            "menu_capture_privacy_preserves_sensitive_frames", "menu_capture_privacy_rejects_partial_reads",
            "menu_capture_privacy_rejects_unproved_menus", "menu_capture_privacy_preserves_original_layer",
            "menu_capture_privacy_preserves_unknown_children", "menu_helper_provenance_updates_both_projections"]
}
