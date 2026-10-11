import ApplicationServices
import Foundation

/// Startup authority and Source-owned identity facts for one BackgroundAX action.
/// AX references are deliberately absent; they are resolved afresh on the worker.
struct BackgroundAXAuthorization {
    let pid: Int32
    let bundleID: String
    let birth: DeviceProcessStamp
    let windowID: UInt32
    let semanticScopeID: String
    let target: NativeBindingFingerprint
    let semanticContext: NativeDeviceContextBackgroundAXV1
    let sourceSequence: UInt64
    let sourceObservedMS: UInt64

    var windowFrame: HitRect { semanticContext.window.frame }
    var windowTitle: String? { semanticContext.window.title }
    var targetEnabled: Bool { semanticContext.target_enabled }
    var targetActionNames: [String] { semanticContext.target_action_names }
}

enum BackgroundAXPolicy {
    static let maximumNodes = 128
    static let maximumDepth = 12
    static let maximumAuxiliaryGroupNodes = 4
    static let maximumAuxiliaryGroupDepth = 2
    static let maximumPerCallMilliseconds: UInt64 = 25
    static let maximumWindowCount = 32
    static let maximumActions = 32

    private static let supportedTargetRoles: Set<String> = [
        "AXButton", "AXCheckBox", "AXRadioButton", "AXSwitch",
    ]
    private static let registeredBundles: Set<String> = [
        "com.lingxi.computer-user.guard-fixture",
        "com.lingxi.computer-user.mvp-fixture",
        "com.apple.TextEdit",
    ]
    private static let blockedAncestorRoles: Set<String> = [
        "AXSheet", "AXDialog", "AXPopover", "AXMenu", "AXMenuBar", "AXMenuItem",
    ]
    private static let supportedTreeRoles: Set<String> = [
        "AXWindow", "AXGroup", "AXScrollArea", "AXScrollBar", "AXSplitGroup",
        "AXTable", "AXRow", "AXCell", "AXColumn", "AXColumnHeader", "AXHeader",
        "AXStaticText", "AXButton", "AXCheckBox", "AXRadioButton", "AXSwitch", "AXTextField",
        "AXTextArea", "AXComboBox", "AXPopUpButton", "AXMenuButton", "AXSlider",
        "AXValueIndicator", "AXImage", "AXLink", "AXTab", "AXTabGroup", "AXList",
        "AXOutline", "AXDisclosureTriangle", "AXProgressIndicator", "AXToolbar",
        "AXWebArea", "AXHeading", "AXSplitter",
    ]

    static func authorizationIsValid(_ value: BackgroundAXAuthorization) -> Bool {
        guard value.pid > 0, value.birth.seconds > 0, value.birth.micros < 1_000_000,
              value.windowID > 0, value.windowFrame.valid,
              validSemanticScopeID(value.semanticScopeID), registeredBundles.contains(value.bundleID),
              value.sourceSequence > 0,
              value.target.valid, value.target.app == value.bundleID,
              value.target.app_pid == UInt32(value.pid),
              value.target.window_id == "window-\(value.windowID)",
              value.semanticContext.app.bundle_id == value.bundleID,
              value.semanticContext.app.pid == UInt32(value.pid),
              value.semanticContext.window.window_id == value.target.window_id,
              value.semanticContext.window.kind == .window,
              !value.semanticContext.window.modal,
              value.semanticContext.window.owner_bundle_id == value.bundleID,
              value.semanticContext.window.frame.matches(value.windowFrame),
              value.semanticContext.source.window.process.pid == UInt32(value.pid),
              value.semanticContext.source.window.process.stamp == value.birth,
              value.semanticContext.source.matchesSourceProvenance(
                  sequence: value.sourceSequence, observedMS: value.sourceObservedMS),
              value.semanticContext.source.valid(bundleID: value.bundleID, pid: UInt32(value.pid),
                  windowID: value.target.window_id, frame: value.windowFrame),
              NativeDeviceContextWitness.backgroundAxV1(value.semanticContext)
                  .matchesBinding(value.target, displayFrame: value.semanticContext.target.object.frame),
              value.targetEnabled,
              supportedTargetRoles.contains(value.target.role),
              value.target.ancestor_roles.first == "AXWindow",
              !value.target.ancestor_roles.contains(where: blockedAncestorRoles.contains),
              validOptionalText(value.windowTitle, maximum: 512),
              actionNamesMatch(expected: value.targetActionNames,
                               actual: value.targetActionNames) else { return false }
        return true
    }

    static func validSemanticScopeID(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        guard !bytes.isEmpty, bytes.count <= 128,
              ((48...57).contains(bytes[0]) || (65...90).contains(bytes[0]) || (97...122).contains(bytes[0])) else {
            return false
        }
        return bytes.dropFirst().allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0)
                || [45, 46, 58, 95].contains($0)
        }
    }

    static func uniqueTargetIndex(
        _ candidates: [NativeBindingFingerprint],
        expected: NativeBindingFingerprint
    ) -> Int? {
        let matches = candidates.indices.filter { candidates[$0] == expected }
        return matches.count == 1 ? matches[0] : nil
    }

    static func actionNamesMatch(expected: [String], actual: [String]) -> Bool {
        func valid(_ names: [String]) -> Bool {
            guard !names.isEmpty, names.count <= maximumActions else { return false }
            var seen = Set<String>()
            return names.allSatisfy { name in
                !name.isEmpty && name.utf8.count <= 64
                    && !name.utf8.contains(where: { $0 < 0x20 || $0 == 0x7f })
                    && seen.insert(name).inserted
            }
        }
        guard valid(expected), valid(actual), expected.contains(kAXPressAction as String) else { return false }
        return Set(expected) == Set(actual)
    }

    static func visibleStandardWindow(
        role: String,
        subrole: String,
        modal: Bool?,
        applicationHidden: Bool?,
        minimized: Bool?,
        actualFrame: HitRect,
        expectedFrame: HitRect
    ) -> Bool {
        role == "AXWindow" && subrole == "AXStandardWindow"
            && modal == false && applicationHidden == false && minimized == false
            && actualFrame.valid && expectedFrame.valid && actualFrame.matches(expectedFrame)
    }

    static func traversalIsWithinBounds(nodeCount: Int, depth: Int) -> Bool {
        nodeCount > 0 && nodeCount <= maximumNodes && depth >= 0 && depth <= maximumDepth
    }

    static func isSupportedTreeRole(_ role: String) -> Bool {
        !role.isEmpty && role.utf8.count <= 64 && supportedTreeRoles.contains(role)
    }

    static func auxiliaryZoomGroupRootIsAllowed(
        bundleID: String,
        decorationRole: String,
        decorationSubrole: String,
        rootRole: String,
        actualParentMatchesWindow: Bool,
        targetRole: String
    ) -> Bool {
        bundleID == "com.lingxi.computer-user.guard-fixture"
            && decorationRole == kAXButtonRole && decorationSubrole == (kAXZoomButtonSubrole as String)
            && rootRole == kAXGroupRole && actualParentMatchesWindow
            && rootRole != targetRole && !supportedTargetRoles.contains(rootRole)
    }

    static func auxiliaryDecorationHasOnlyRoot(childCount: Int, rootReferenceMatches: Bool) -> Bool {
        childCount == 1 && rootReferenceMatches
    }

    static func auxiliaryWindowMembershipIsAllowed(
        disposition: BackgroundAXWindowMembershipDisposition,
        rawValueIsNil: Bool,
        reportedPIDMatches: Bool?,
        selectedWindowMatches: Bool?,
        parentChainProven: Bool
    ) -> Bool {
        switch disposition {
        case .known:
            return !rawValueIsNil && reportedPIDMatches == true && selectedWindowMatches == true
        case .unsupported, .noValue:
            return rawValueIsNil && reportedPIDMatches == nil && selectedWindowMatches == nil
                && parentChainProven
        case .timeout, .readFailed, .malformed, .foreign, .mismatch, .parentUnproven:
            return false
        }
    }

    static func auxiliaryGroupTreeIsWithinBounds(groupCount: Int, groupDepth: Int, overallDepth: Int) -> Bool {
        groupCount > 0 && groupCount <= maximumAuxiliaryGroupNodes
            && groupDepth > 0 && groupDepth <= maximumAuxiliaryGroupDepth
            && overallDepth >= 0 && overallDepth <= maximumDepth
    }

    static func auxiliaryGroupNodeIsAllowed(
        role: String,
        actualParentIsWindowOrProvenGroup: Bool,
        windowMembershipProven: Bool,
        childrenArrayIsTyped: Bool,
        childCountBeforeCopy: Int,
        copiedChildCount: Int,
        childCountAfterCopy: Int,
        childrenAreGroups: Bool,
        childrenAreUnique: Bool,
        groupCount: Int,
        groupDepth: Int,
        overallDepth: Int
    ) -> Bool {
        role == kAXGroupRole && actualParentIsWindowOrProvenGroup
            && windowMembershipProven && childrenArrayIsTyped
            && childCountBeforeCopy >= 0 && copiedChildCount == childCountBeforeCopy
            && childCountAfterCopy == childCountBeforeCopy && childrenAreGroups && childrenAreUnique
            && auxiliaryGroupTreeIsWithinBounds(groupCount: groupCount,
                groupDepth: groupDepth, overallDepth: overallDepth)
    }

    private static func validOptionalText(_ value: String?, maximum: Int) -> Bool {
        guard let value else { return true }
        return value.utf8.count <= maximum
            && !value.unicodeScalars.contains { $0.properties.generalCategory == .control }
    }
}

private enum BackgroundAXPolicyTestFailure: Error {
    case assertion(String)
}

private func requireBackgroundAXPolicy(_ condition: Bool, _ message: String) throws {
    guard condition else { throw BackgroundAXPolicyTestFailure.assertion(message) }
}

private func backgroundAXTestFingerprint(
    ancestors: [String] = ["AXWindow", "AXGroup"],
    siblingIndex: UInt32 = 2
) -> NativeBindingFingerprint {
    NativeBindingFingerprint(
        app: "com.lingxi.computer-user.guard-fixture",
        app_pid: 700,
        window_id: "window-42",
        ancestor_roles: ancestors,
        group: "primary controls",
        role: "AXButton",
        subrole: nil,
        label: "apply",
        sibling_index: siblingIndex,
        frame: [20, 30, 80, 24],
        scroll_container: nil
    )
}

private func backgroundAXTestContext(
    hidden: NativeDeviceBooleanObservationFact = .known(false),
    minimized: NativeDeviceBooleanObservationFact = .known(false),
    enabled: Bool = true,
    actions: [String] = ["AXPress", "AXShowMenu"],
    targetIsAtDeclaredSibling: Bool = true,
    targetContextHashOverride: String? = nil
) -> NativeDeviceContextBackgroundAXV1 {
    let frame = HitRect(CGRect(x: 0, y: 0, width: 500, height: 400))
    let targetFrame = HitRect(CGRect(x: 20, y: 30, width: 80, height: 24))
    let app = NativeDeviceContextApp(bundle_id: "com.lingxi.computer-user.guard-fixture", pid: 700)
    let window = NativeDeviceContextWindow(window_id: "window-42", title: "fixture", frame: frame,
        kind: .window, modal: false, owner_bundle_id: app.bundle_id)
    let root = NativeDeviceContextObject(role: "AXWindow", subrole: "AXStandardWindow",
        label: "fixture", frame: frame)
    let group = NativeDeviceContextObject(role: "AXGroup", subrole: nil,
        label: "primary controls", frame: HitRect(CGRect(x: 10, y: 20, width: 200, height: 160)))
    let target = NativeDeviceContextObject(role: "AXButton", subrole: nil,
        label: "apply", frame: targetFrame)
    let process = NativeMenuProcessIdentity(pid: 700, start_seconds: 1_800_000_000, start_micros: 123_456)
    let sourceWindow = NativeDeviceSourceWindowContext(source_session_id: "source-session-1",
        session_epoch: 3, process: process, origin_window_id: "window-42",
        origin_window_frame: frame, verified_source_sequence: 7, verified_observed_ms: 10)
    let unhashedSource = NativeDeviceSourceBackgroundWindowContext(window: sourceWindow,
        hidden: hidden, minimized: minimized, target_context_hash: "tc1:" + String(repeating: "0", count: 64))
    let hash = targetContextHashOverride
        ?? unhashedSource.computedTargetContextHash(bundleID: app.bundle_id, pid: app.pid)
    let source = NativeDeviceSourceBackgroundWindowContext(window: sourceWindow,
        hidden: hidden, minimized: minimized, target_context_hash: hash)
    let siblings = [
        NativeDeviceContextObject(role: "AXButton", subrole: nil, label: "cancel",
            frame: HitRect(CGRect(x: 20, y: 60, width: 80, height: 24))),
        NativeDeviceContextObject(role: "AXButton", subrole: nil, label: "help",
            frame: HitRect(CGRect(x: 110, y: 60, width: 80, height: 24))),
        targetIsAtDeclaredSibling ? target : group,
    ]
    return NativeDeviceContextBackgroundAXV1(app: app, window: window, root: root,
        target: NativeDeviceContextTarget(object: target, group: "primary controls", scroll: nil),
        ancestry: [root, group], sibling_parent_ancestry_index: 1, siblings: siblings,
        target_sibling_index: 2, nearest_labelled_group_ancestry_index: 1, source: source,
        target_context_hash: hash, target_enabled: enabled, target_action_names: actions)
}

/// Pure regression checks for BackgroundAX selection and fail-closed policy.
/// The native self-test runner registers this without creating an AX reference.
func runBackgroundAXPolicyTests() throws -> [String] {
    let expected = backgroundAXTestFingerprint()
    let refinedStages = Set([
        NativeBackgroundAXTrace.Stage.targetNodeIdentity.rawValue,
        NativeBackgroundAXTrace.Stage.targetChildrenCount.rawValue,
        NativeBackgroundAXTrace.Stage.targetChildrenRead.rawValue,
        NativeBackgroundAXTrace.Stage.targetChildrenConfirm.rawValue,
        NativeBackgroundAXTrace.Stage.targetDeadline.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryRelation.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryNonempty.rawValue,
        NativeBackgroundAXTrace.Stage.childrenType.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryDecorationChildren.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupIdentity.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupRole.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupParent.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupWindow.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupChildren.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryGroupBounds.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowUnsupported.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowNoValue.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowTimeout.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowReadFailed.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowMalformed.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowForeign.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowMismatch.rawValue,
        NativeBackgroundAXTrace.Stage.auxiliaryWindowParentUnproven.rawValue,
    ])
    let encodedStages = NativeBackgroundAXTrace.Stage.allCases.map(\.rawValue)
    try requireBackgroundAXPolicy(refinedStages.count == 23
        && refinedStages.isSubset(of: Set(encodedStages))
        && encodedStages.count == Set(encodedStages).count,
        "target traversal diagnostics must use distinct closed stage values")
    let sameLabelAndFrameWrongPath = backgroundAXTestFingerprint(ancestors: ["AXWindow", "AXToolbar"])
    let sameLabelAndPathWrongSibling = backgroundAXTestFingerprint(siblingIndex: 3)
    let selected = BackgroundAXPolicy.uniqueTargetIndex(
        [sameLabelAndFrameWrongPath, sameLabelAndPathWrongSibling, expected], expected: expected)
    try requireBackgroundAXPolicy(selected == 2,
        "semantic selection must use ancestry and sibling identity beyond label/frame")
    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.uniqueTargetIndex([expected, expected], expected: expected) == nil,
        "duplicate exact semantic targets must be ambiguous")

    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.actionNamesMatch(expected: ["AXPress"], actual: ["AXPress"]),
        "the exact advertised AXPress action should pass")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.actionNamesMatch(expected: ["AXPress"], actual: ["AXShowMenu"]),
        "an unadvertised or changed action must fail")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.actionNamesMatch(expected: ["AXPress", "AXPress"], actual: ["AXPress"]),
        "duplicate action names must fail")
    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.actionNamesMatch(expected: ["AXPress", "AXShowMenu"], actual: ["AXShowMenu", "AXPress"])
            && !BackgroundAXPolicy.actionNamesMatch(expected: ["AXPress", "AXShowMenu"], actual: ["AXPress"]),
        "the complete action set must remain identical while allowing stable set ordering")

    let frame = HitRect(CGRect(x: 0, y: 0, width: 500, height: 400))
    let semanticContext = backgroundAXTestContext()
    let birth = semanticContext.source.window.process.stamp
    let authorization = BackgroundAXAuthorization(
        pid: 700,
        bundleID: "com.lingxi.computer-user.guard-fixture",
        birth: birth,
        windowID: 42,
        semanticScopeID: "fixture.button.apply",
        target: expected,
        semanticContext: semanticContext,
        sourceSequence: 7,
        sourceObservedMS: 10
    )
    let backgroundWitness = NativeDeviceContextWitness.backgroundAxV1(semanticContext)
    let witnessData = try JSONEncoder().encode(backgroundWitness)
    let witnessRaw = try JSONSerialization.jsonObject(with: witnessData)
    let decodedWitness = try JSONDecoder().decode(NativeDeviceContextWitness.self, from: witnessData)
    try requireBackgroundAXPolicy(semanticContext.valid && strictNativeDeviceContextWitnessFields(witnessRaw)
        && decodedWitness == backgroundWitness
        && decodedWitness.supportsNativeInput(action: "ax_press")
        && !decodedWitness.supportsNativeInput
        && decodedWitness.supportsAction("ax_press")
        && !decodedWitness.supportsAction("click"),
        "BackgroundAxV1 must round-trip under its own tag and grant only AXPress")
    try requireBackgroundAXPolicy(!decodedWitness.supportsAction("ax_set_selected_true")
        && !decodedWitness.supportsAction("activate_window"),
        "BackgroundAxV1 must not inherit foreground selection or window activation actions")
    let backgroundScope = NativeBindingProofScope.backgroundAX(
        targetContextHash: semanticContext.target_context_hash)
    try requireBackgroundAXPolicy(nativeRequestExecutionModeIsValid(
        executionDomain: "background_ax", purpose: "background_ax_action", action: "ax_press",
        coordinateSpace: "target_object", contextWitness: backgroundWitness,
        originalScope: backgroundScope, currentScope: backgroundScope),
        "the exact BackgroundAX domain, purpose, witness, scope hash and TargetObject coordinate must pair")
    try requireBackgroundAXPolicy(!nativeRequestExecutionModeIsValid(
        executionDomain: "foreground_native", purpose: "background_ax_action", action: "ax_press",
        coordinateSpace: "target_object", contextWitness: backgroundWitness,
        originalScope: backgroundScope, currentScope: backgroundScope)
        && !nativeRequestExecutionModeIsValid(
            executionDomain: nil, purpose: nil, action: "click", coordinateSpace: "target_object",
            contextWitness: .wholeStructureV1(semanticContext.wholeStructureFacts),
            originalScope: .wholeStructure, currentScope: .wholeStructure)
        && !nativeRequestExecutionModeIsValid(
            executionDomain: "background_ax", purpose: "background_ax_action", action: "ax_press",
            coordinateSpace: "display_points", contextWitness: backgroundWitness,
            originalScope: backgroundScope, currentScope: backgroundScope)
        && !nativeRequestExecutionModeIsValid(
            executionDomain: "background_ax", purpose: "background_ax_action", action: "ax_press",
            coordinateSpace: "target_object", contextWitness: backgroundWitness,
            originalScope: .wholeStructure, currentScope: backgroundScope),
        "cross-domain packets and TargetObject outside BackgroundAX must fail")
    try requireBackgroundAXPolicy(nativeRequestExecutionModeIsValid(
        executionDomain: nil, purpose: nil, action: "ax_press", coordinateSpace: nil,
        contextWitness: .wholeStructureV1(semanticContext.wholeStructureFacts),
        originalScope: .wholeStructure, currentScope: .wholeStructure),
        "legacy foreground packets without optional mode fields must remain valid")
    var malformedWitness = witnessRaw as! [String: Any]
    malformedWitness["active_layer"] = NSNull()
    try requireBackgroundAXPolicy(!strictNativeDeviceContextWitnessFields(malformedWitness),
        "BackgroundAxV1 must reject foreground WholeStructure fields")
    try requireBackgroundAXPolicy(BackgroundAXPolicy.authorizationIsValid(authorization),
        "a fully fixed process/window/semantic scope should pass")
    let wrongBirth = BackgroundAXAuthorization(pid: authorization.pid, bundleID: authorization.bundleID,
        birth: DeviceProcessStamp(seconds: authorization.birth.seconds + 1, micros: authorization.birth.micros),
        windowID: authorization.windowID, semanticScopeID: authorization.semanticScopeID,
        target: expected, semanticContext: semanticContext, sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(wrongBirth),
        "a Source birth stamp different from the trusted launch stamp must fail")
    let staleSource = BackgroundAXAuthorization(pid: authorization.pid, bundleID: authorization.bundleID,
        birth: authorization.birth, windowID: authorization.windowID,
        semanticScopeID: authorization.semanticScopeID, target: expected,
        semanticContext: semanticContext, sourceSequence: 8, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(staleSource),
        "the Source sequence and observed time must match the request")
    let wrongWindow = BackgroundAXAuthorization(
        pid: authorization.pid, bundleID: authorization.bundleID, birth: authorization.birth,
        windowID: 43, semanticScopeID: authorization.semanticScopeID, target: expected,
        semanticContext: semanticContext, sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(wrongWindow),
        "a semantic target from another window must not authorize")
    let unregisteredBundle = BackgroundAXAuthorization(
        pid: authorization.pid, bundleID: "example.editor", birth: authorization.birth,
        windowID: authorization.windowID, semanticScopeID: authorization.semanticScopeID,
        target: expected, semanticContext: semanticContext, sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(unregisteredBundle),
        "an unregistered application must not enter the purpose")
    let unknownSourceVisibility = BackgroundAXAuthorization(
        pid: authorization.pid, bundleID: authorization.bundleID, birth: authorization.birth,
        windowID: authorization.windowID, semanticScopeID: authorization.semanticScopeID,
        target: expected, semanticContext: backgroundAXTestContext(hidden: .unavailable),
        sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(unknownSourceVisibility),
        "unknown Source visibility facts must not authorize a background AX action")
    let wrongSiblingContext = backgroundAXTestContext(targetIsAtDeclaredSibling: false)
    let wrongSiblingAuthorization = BackgroundAXAuthorization(
        pid: authorization.pid, bundleID: authorization.bundleID, birth: authorization.birth,
        windowID: authorization.windowID, semanticScopeID: authorization.semanticScopeID,
        target: expected, semanticContext: wrongSiblingContext, sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(wrongSiblingAuthorization),
        "the declared source sibling index must name the exact target object")
    let disabledTarget = BackgroundAXAuthorization(pid: authorization.pid, bundleID: authorization.bundleID,
        birth: authorization.birth, windowID: authorization.windowID,
        semanticScopeID: authorization.semanticScopeID, target: expected,
        semanticContext: backgroundAXTestContext(enabled: false), sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(disabledTarget),
        "a Source-disabled target must not authorize AXPress")
    let sourceActionsMissingPress = BackgroundAXAuthorization(pid: authorization.pid, bundleID: authorization.bundleID,
        birth: authorization.birth, windowID: authorization.windowID,
        semanticScopeID: authorization.semanticScopeID, target: expected,
        semanticContext: backgroundAXTestContext(actions: ["AXShowMenu"]), sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(sourceActionsMissingPress),
        "the complete Source action set must include AXPress")
    let changedSourceHash = BackgroundAXAuthorization(pid: authorization.pid, bundleID: authorization.bundleID,
        birth: authorization.birth, windowID: authorization.windowID,
        semanticScopeID: authorization.semanticScopeID, target: expected,
        semanticContext: backgroundAXTestContext(targetContextHashOverride: "tc1:" + String(repeating: "f", count: 64)),
        sourceSequence: 7, sourceObservedMS: 10)
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.authorizationIsValid(changedSourceHash),
        "the Source target-context hash must bind its process/window facts")
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.validSemanticScopeID("fixture button"),
        "semantic scope identifiers must be canonical and bounded")

    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.visibleStandardWindow(role: "AXWindow", subrole: "AXStandardWindow",
            modal: false, applicationHidden: false, minimized: false,
            actualFrame: frame, expectedFrame: frame),
        "a nonmodal standard window in a known visible application should pass")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.visibleStandardWindow(role: "AXWindow", subrole: "AXStandardWindow",
            modal: false, applicationHidden: nil, minimized: false,
            actualFrame: frame, expectedFrame: frame),
        "unknown application hidden state must fail closed")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.visibleStandardWindow(role: "AXWindow", subrole: "AXStandardWindow",
            modal: false, applicationHidden: true, minimized: false,
            actualFrame: frame, expectedFrame: frame),
        "a hidden application must fail closed")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.visibleStandardWindow(role: "AXWindow", subrole: "AXStandardWindow",
            modal: false, applicationHidden: false, minimized: true,
            actualFrame: frame, expectedFrame: frame),
        "minimized windows are outside the initial purpose")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.visibleStandardWindow(role: "AXWindow", subrole: "AXStandardWindow",
            modal: nil, applicationHidden: false, minimized: false,
            actualFrame: frame, expectedFrame: frame),
        "unknown modal state must fail closed")
    let fixtureZoomRoot = BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
        bundleID: "com.lingxi.computer-user.guard-fixture", decorationRole: kAXButtonRole,
        decorationSubrole: kAXZoomButtonSubrole as String, rootRole: kAXGroupRole,
        actualParentMatchesWindow: true, targetRole: kAXCheckBoxRole)
    try requireBackgroundAXPolicy(fixtureZoomRoot,
        "the exact fixture zoom decoration may open a separate AXGroup-only auxiliary closure")
    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.auxiliaryDecorationHasOnlyRoot(childCount: 1, rootReferenceMatches: true),
        "the zoom decoration must expose exactly the root group reference")
    try requireBackgroundAXPolicy(!BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
        bundleID: "com.apple.TextEdit", decorationRole: kAXButtonRole,
        decorationSubrole: kAXZoomButtonSubrole as String, rootRole: kAXGroupRole,
        actualParentMatchesWindow: true, targetRole: kAXCheckBoxRole)
        && !BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
            bundleID: "com.lingxi.computer-user.guard-fixture", decorationRole: kAXButtonRole,
            decorationSubrole: kAXCloseButtonSubrole as String, rootRole: kAXGroupRole,
            actualParentMatchesWindow: true, targetRole: kAXCheckBoxRole)
        && !BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
            bundleID: "com.lingxi.computer-user.guard-fixture", decorationRole: kAXButtonRole,
            decorationSubrole: kAXZoomButtonSubrole as String, rootRole: kAXGroupRole,
            actualParentMatchesWindow: false, targetRole: kAXCheckBoxRole)
        && !BackgroundAXPolicy.auxiliaryZoomGroupRootIsAllowed(
            bundleID: "com.lingxi.computer-user.guard-fixture", decorationRole: kAXButtonRole,
            decorationSubrole: kAXZoomButtonSubrole as String, rootRole: kAXCheckBoxRole,
            actualParentMatchesWindow: true, targetRole: kAXCheckBoxRole),
        "the auxiliary root must reject other bundles, decoration roles, parents, and semantic roles")
    try requireBackgroundAXPolicy(
        !BackgroundAXPolicy.auxiliaryDecorationHasOnlyRoot(childCount: 2, rootReferenceMatches: true)
            && !BackgroundAXPolicy.auxiliaryDecorationHasOnlyRoot(childCount: 1, rootReferenceMatches: false),
        "the auxiliary edge must reject other children and changed root references")

    func windowMembershipAllowed(
        _ disposition: BackgroundAXWindowMembershipDisposition,
        rawNil: Bool,
        pidMatches: Bool?,
        windowMatches: Bool?,
        parentProven: Bool
    ) -> Bool {
        BackgroundAXPolicy.auxiliaryWindowMembershipIsAllowed(disposition: disposition,
            rawValueIsNil: rawNil, reportedPIDMatches: pidMatches,
            selectedWindowMatches: windowMatches, parentChainProven: parentProven)
    }
    try requireBackgroundAXPolicy(
        windowMembershipAllowed(.known, rawNil: false, pidMatches: true,
            windowMatches: true, parentProven: false)
            && windowMembershipAllowed(.unsupported, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: true)
            && windowMembershipAllowed(.noValue, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: true),
        "known AXWindow and explicit absent statuses may prove membership only under their respective facts")
    try requireBackgroundAXPolicy(
        !windowMembershipAllowed(.known, rawNil: false, pidMatches: false,
            windowMatches: true, parentProven: true)
            && !windowMembershipAllowed(.known, rawNil: false, pidMatches: true,
                windowMatches: false, parentProven: true)
            && !windowMembershipAllowed(.known, rawNil: true, pidMatches: true,
                windowMatches: true, parentProven: true)
            && !windowMembershipAllowed(.unsupported, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: false)
            && !windowMembershipAllowed(.noValue, rawNil: false, pidMatches: nil,
                windowMatches: nil, parentProven: true)
            && !windowMembershipAllowed(.timeout, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: true)
            && !windowMembershipAllowed(.readFailed, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: true)
            && !windowMembershipAllowed(.malformed, rawNil: false, pidMatches: true,
                windowMatches: true, parentProven: true)
            && !windowMembershipAllowed(.foreign, rawNil: false, pidMatches: false,
                windowMatches: true, parentProven: true)
            && !windowMembershipAllowed(.mismatch, rawNil: false, pidMatches: true,
                windowMatches: false, parentProven: true)
            && !windowMembershipAllowed(.parentUnproven, rawNil: true, pidMatches: nil,
                windowMatches: nil, parentProven: false),
        "mismatched, foreign, malformed, failed, timeout, and unproven AXWindow outcomes must reject")

    func auxiliaryNodeAllowed(
        role: String = kAXGroupRole,
        parent: Bool = true,
        window: Bool = true,
        typed: Bool = true,
        before: Int = 1,
        copied: Int = 1,
        after: Int = 1,
        childrenAreGroups: Bool = true,
        unique: Bool = true,
        count: Int = 2,
        depth: Int = 2,
        overallDepth: Int = 3
    ) -> Bool {
        BackgroundAXPolicy.auxiliaryGroupNodeIsAllowed(role: role,
            actualParentIsWindowOrProvenGroup: parent,
            windowMembershipProven: window,
            childrenArrayIsTyped: typed,
            childCountBeforeCopy: before, copiedChildCount: copied,
            childCountAfterCopy: after, childrenAreGroups: childrenAreGroups,
            childrenAreUnique: unique, groupCount: count, groupDepth: depth,
            overallDepth: overallDepth)
    }
    try requireBackgroundAXPolicy(auxiliaryNodeAllowed(before: 0, copied: 0, after: 0, count: 1, depth: 1)
        && auxiliaryNodeAllowed(before: 2, copied: 2, after: 2),
        "complete empty and nested AXGroup child lists may pass the bounded fixture closure")
    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.auxiliaryGroupTreeIsWithinBounds(groupCount: 4, groupDepth: 2, overallDepth: 3)
            && !BackgroundAXPolicy.auxiliaryGroupTreeIsWithinBounds(groupCount: 5, groupDepth: 2, overallDepth: 3)
            && !BackgroundAXPolicy.auxiliaryGroupTreeIsWithinBounds(groupCount: 4, groupDepth: 3, overallDepth: 3),
        "the fixture auxiliary closure must cap the complete graph at four groups and two group levels")
    try requireBackgroundAXPolicy(!auxiliaryNodeAllowed(role: kAXButtonRole)
        && !auxiliaryNodeAllowed(role: "AXMenu")
        && !auxiliaryNodeAllowed(role: "AXMenuItem")
        && !auxiliaryNodeAllowed(role: "AXDialog")
        && !auxiliaryNodeAllowed(parent: false)
        && !auxiliaryNodeAllowed(window: false)
        && !auxiliaryNodeAllowed(typed: false)
        && !auxiliaryNodeAllowed(before: 2, copied: 1, after: 2)
        && !auxiliaryNodeAllowed(before: 2, copied: 2, after: 1)
        && !auxiliaryNodeAllowed(childrenAreGroups: false)
        && !auxiliaryNodeAllowed(unique: false)
        && !auxiliaryNodeAllowed(count: 5)
        && !auxiliaryNodeAllowed(depth: 3)
        && !auxiliaryNodeAllowed(overallDepth: 13),
        "auxiliary traversal must reject wrong roles/parents/windows, malformed or duplicate child lists, and bounds")
    try requireBackgroundAXPolicy(
        BackgroundAXPolicy.traversalIsWithinBounds(nodeCount: 128, depth: 12)
            && !BackgroundAXPolicy.traversalIsWithinBounds(nodeCount: 129, depth: 12)
            && !BackgroundAXPolicy.traversalIsWithinBounds(nodeCount: 1, depth: 13),
        "AX traversal must enforce both fixed bounds")

    return ["fixed BackgroundAX scope", "unique semantic target", "exact AXPress action",
            "dedicated BackgroundAxV1 wire tag", "complete source action set",
            "visible standard window", "bounded AX traversal",
            "bounded fixture auxiliary group closure", "auxiliary closure negative facts",
            "typed auxiliary window membership", "auxiliary window membership negative facts"]
}
