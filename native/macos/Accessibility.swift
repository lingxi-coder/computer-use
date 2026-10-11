import Foundation
import ApplicationServices
import CoreGraphics

let scopedTableRowsOmittedReason = "AX table row subtree omitted for scoped census"

struct NativeWindow: Sendable, Equatable {
    let id: UInt32
    let pid: UInt32
    let frame: Rect
    let zIndex: UInt32
    var ref: String { "window-\(id)" }

    func structureInfo(title: String?, modal: Bool, ownerBundleID: String,
                       kind: String = "window") -> WindowInfo {
        // This is the CG capture binding. AX object geometry remains the
        // original AX reading even when the initial association needs tolerance.
        WindowInfo(window_id: ref, title: title, frame: frame, z_index: zIndex,
                   kind: kind, modal: modal, owner_bundle_id: ownerBundleID)
    }
}

func matchAXWindow(_ axFrame: Rect, windows: [NativeWindow]) -> NativeWindow? {
    let matches = windows.filter { $0.frame.approximatelyEquals(axFrame) }
    return matches.count == 1 ? matches.first : nil
}

func backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: UInt64) -> Float? {
    guard remainingNanoseconds > 0 else { return nil }
    let boundedNanoseconds = min(UInt64(25_000_000), remainingNanoseconds)
    var seconds = Float(Double(boundedNanoseconds) / 1_000_000_000)
    guard seconds.isFinite, seconds > 0 else { return nil }
    // Float rounding must not turn a remaining-deadline cap into a slightly
    // longer timeout. Step down only when conversion rounded upward.
    if Double(seconds) > Double(boundedNanoseconds) / 1_000_000_000 {
        seconds = seconds.nextDown
    }
    guard seconds > 0,
          Double(seconds) <= Double(boundedNanoseconds) / 1_000_000_000 else { return nil }
    return seconds
}

let mvpFixtureBundleID = "com.lingxi.computer-user.mvp-fixture"
let registeredAuxiliaryDialogBundleIDs: Set<String> = [
    "com.lingxi.computer-user.guard-fixture",
    mvpFixtureBundleID,
    "com.apple.TextEdit"
]

func supportsRegisteredAuxiliaryAXDialogMetadata(bundleID: String) -> Bool {
    registeredAuxiliaryDialogBundleIDs.contains(bundleID)
}

func isRegisteredAuxiliaryAXDialogRoot(bundleID: String, role: String, subrole: String?, topLevel: Bool) -> Bool {
    supportsRegisteredAuxiliaryAXDialogMetadata(bundleID: bundleID)
        && topLevel && role == kAXWindowRole && subrole == "AXDialog"
}

/// A sheet is a separate input layer only for the fixture topology that has
/// been observed and audited. AXModal is intentionally not required: AppKit's
/// sheet reports AXFocused and is returned as AXFocusedWindow in this profile.
func canProjectMvpAXSheet(bundleID: String, frontmost: Bool,
                          focusedWindowMatchesSheet: Bool, sheetRole: String,
                          sheetFocused: Bool?, sheetModal: Bool?, sheetSubrole: String?,
                          parentRole: String?, parentSubrole: String?, depth: Int,
                          axWindowCount: Int, cgWindowCount: Int) -> Bool {
    bundleID == mvpFixtureBundleID && frontmost && focusedWindowMatchesSheet
        && sheetRole == kAXSheetRole && sheetFocused == true && sheetModal != true
        && (sheetSubrole == nil || sheetSubrole == "")
        && parentRole == kAXWindowRole && parentSubrole == kAXStandardWindowSubrole && depth == 1
        && axWindowCount == 1 && cgWindowCount == 2
}

/// Match a sheet's exact AX frame to one distinct, frontmost CG window owned
/// by the same process as its AXWindow parent. Approximate window matching is
/// not enough to create a separate input layer.
func matchAXSheetWindow(_ sheetFrame: Rect, attachedTo parent: NativeWindow,
                        windows: [NativeWindow]) -> NativeWindow? {
    let parents = windows.filter { $0.id == parent.id && $0.pid == parent.pid }
    guard parents.count == 1, parents[0].frame == parent.frame else { return nil }
    let matches = windows.filter {
        $0.pid == parent.pid && $0.id != parent.id
            && $0.frame == sheetFrame && $0.zIndex < parent.zIndex
    }
    return matches.count == 1 ? matches.first : nil
}

/// Typed facts used only to recognize a registered app's inactive auxiliary
/// AXWindow as window metadata. This never creates a capture/input layer.
struct RegisteredAuxiliaryAXWindowFacts: Equatable {
    let pid: UInt32?
    let role: String?
    let subrole: String?
    let frame: Rect?
    let modal: Bool?
    let focused: Bool?
    let exactCGMatches: [NativeWindow]
}

/// One exact AX root plus its strict, same-read metadata facts. The retained
/// token keeps the actual AX object alive so a same-frame replacement cannot
/// inherit the proof.
struct RegisteredAuxiliaryPrivacyAuditRoot {
    let token: MenuRetainedToken
    let facts: RegisteredAuxiliaryAXWindowFacts
}

/// Reader-local input for the two-root visible-privacy audit. It deliberately
/// contains live AX references and is neither Sendable nor part of the wire.
struct RegisteredAuxiliaryPrivacyAuditInput {
    let bundleID: String
    let processID: UInt32
    let standardRoot: RegisteredAuxiliaryPrivacyAuditRoot
    let dialogRoot: RegisteredAuxiliaryPrivacyAuditRoot
    let processStamp: ProcessStamp
    let cgInventory: [NativeWindow]
}

struct RegisteredAuxiliaryAXDialogProof {
    let bundleID: String
    let targetPID: UInt32
    let frontmost: Bool
    let processStable: Bool
    let focusStable: Bool
    let focusedParentMatchesBefore: Bool
    let focusedParentMatchesAfter: Bool
    let rootsStable: Bool
    let cgInventoryStable: Bool
    let auxiliaryDialogVisitedOnce: Bool
    let beforeRoots: [RegisteredAuxiliaryAXWindowFacts]
    let afterRoots: [RegisteredAuxiliaryAXWindowFacts]
}

/// Permit only the exact, observed two-root topology. Unknown AX
/// booleans, ambiguous CG matches, another window root, or any pre/post change
/// fail closed. The AXDialog remains ordinary metadata; it is not active.
func canProjectRegisteredAuxiliaryAXDialog(_ proof: RegisteredAuxiliaryAXDialogProof) -> Bool {
    guard supportsRegisteredAuxiliaryAXDialogMetadata(bundleID: proof.bundleID), proof.targetPID > 0,
          proof.frontmost, proof.processStable, proof.focusStable,
          proof.focusedParentMatchesBefore, proof.focusedParentMatchesAfter,
          proof.rootsStable, proof.cgInventoryStable,
          proof.auxiliaryDialogVisitedOnce,
          proof.beforeRoots.count == 2, proof.afterRoots == proof.beforeRoots else { return false }

    let roots = proof.beforeRoots
    guard roots.allSatisfy({ $0.pid == proof.targetPID }),
          roots.filter({ $0.role == kAXWindowRole && $0.subrole == kAXStandardWindowSubrole }).count == 1,
          roots.filter({ $0.role == kAXWindowRole && $0.subrole == "AXDialog" }).count == 1 else { return false }
    guard let standard = roots.first(where: { $0.role == kAXWindowRole && $0.subrole == kAXStandardWindowSubrole }),
          let dialog = roots.first(where: { $0.role == kAXWindowRole && $0.subrole == "AXDialog" }),
          standard.modal == false, dialog.modal == false, dialog.focused == false,
          let standardFrame = standard.frame, let dialogFrame = dialog.frame,
          standard.exactCGMatches.count == 1, dialog.exactCGMatches.count == 1 else { return false }

    let standardCG = standard.exactCGMatches[0]
    let dialogCG = dialog.exactCGMatches[0]
    return standardCG.pid == proof.targetPID && dialogCG.pid == proof.targetPID
        && standardCG.frame == standardFrame && dialogCG.frame == dialogFrame
        && standardCG.id != dialogCG.id
}

/// Validate a fresh reader-local observation against the exact actual CF roots
/// and facts admitted at the start of the read. Geometry or metadata equality
/// alone never stands in for CF instance continuity.
func registeredAuxiliaryPrivacyAuditRevalidationMatches(
    input: RegisteredAuxiliaryPrivacyAuditInput,
    currentRoots: [RegisteredAuxiliaryPrivacyAuditRoot],
    expectedStandard: CFTypeRef,
    focusedWindow: CFTypeRef?,
    currentProcessStamp: ProcessStamp?,
    currentCGInventory: [NativeWindow],
    frontmost: Bool,
    transientMenuFound: Bool,
    menuDrawingFound: Bool,
    menuProven: Bool
) -> Bool {
    let processStable = currentProcessStamp.map { $0 == input.processStamp } ?? false
    guard currentRoots.count == 2,
          input.standardRoot.token.matches(expectedStandard),
          let focusedWindow,
          input.standardRoot.token.matches(focusedWindow),
          input.processID > 0,
          processStable,
          input.cgInventory.count < 32,
          input.cgInventory == currentCGInventory,
          !transientMenuFound, !menuDrawingFound, !menuProven else { return false }

    let expectedObservations = [input.standardRoot, input.dialogRoot].map {
        RegisteredAuxiliaryAXWindowRootObservation(element: $0.token, facts: $0.facts)
    }
    let currentObservations = currentRoots.map {
        RegisteredAuxiliaryAXWindowRootObservation(element: $0.token, facts: $0.facts)
    }
    let matchingStandards = currentRoots.filter({ $0.token.matches(input.standardRoot.token.element) })
    let matchingDialogs = currentRoots.filter({ $0.token.matches(input.dialogRoot.token.element) })
    guard sameRegisteredAXWindowRoots(expectedObservations, currentObservations),
          matchingStandards.count == 1, matchingDialogs.count == 1 else {
        return false
    }
    let currentStandard = matchingStandards[0]
    let currentDialog = matchingDialogs[0]

    let proof = RegisteredAuxiliaryAXDialogProof(
        bundleID: input.bundleID, targetPID: input.processID, frontmost: frontmost,
        processStable: processStable,
        focusStable: input.standardRoot.token.matches(expectedStandard)
            && input.standardRoot.token.matches(focusedWindow),
        focusedParentMatchesBefore: input.standardRoot.token.matches(expectedStandard),
        focusedParentMatchesAfter: currentStandard.token.matches(focusedWindow),
        rootsStable: true,
        cgInventoryStable: input.cgInventory.count < 32 && input.cgInventory == currentCGInventory,
        auxiliaryDialogVisitedOnce: true,
        beforeRoots: [input.standardRoot.facts, input.dialogRoot.facts],
        afterRoots: [currentStandard.facts, currentDialog.facts])
    return canProjectRegisteredAuxiliaryAXDialog(proof)
}

/// Allow the proven auxiliary root to coexist with the one intentional partial
/// AX projection used by scoped table census. This gate grants only root/window
/// metadata continuity; the exact auxiliary proof and pixel/privacy checks stay
/// separate and unchanged.
func registeredAuxiliaryAXDialogMetadataReadiness(reasons: [String],
                                                  hasValidTableScope: Bool,
                                                  hasPendingTableScopeSearch: Bool,
                                                  tableScopeBoundaryStable: Bool) -> Bool {
    guard !reasons.isEmpty else { return true }
    return reasons == [scopedTableRowsOmittedReason]
        && hasValidTableScope
        && hasPendingTableScopeSearch
        && tableScopeBoundaryStable
}

func scopedTableMetadataBoundaryStable(tableScopeBoundaryStable: Bool,
                                       scopeWindowRemainedActive: Bool,
                                       modalFound: Bool,
                                       supportedAuxiliaryDialog: Bool,
                                       unsupportedModalFound: Bool,
                                       transientMenuFound: Bool,
                                       menuDrawingFound: Bool) -> Bool {
    tableScopeBoundaryStable && scopeWindowRemainedActive
        && (!modalFound || supportedAuxiliaryDialog)
        && !unsupportedModalFound && !transientMenuFound && !menuDrawingFound
}

private struct RegisteredAuxiliaryAXWindowRootObservation {
    let element: MenuRetainedToken
    let facts: RegisteredAuxiliaryAXWindowFacts
}

private func sameRegisteredAXWindowRoots(_ before: [RegisteredAuxiliaryAXWindowRootObservation],
                                         _ after: [RegisteredAuxiliaryAXWindowRootObservation]) -> Bool {
    guard before.count == after.count else { return false }
    var unmatched = after
    for old in before {
        let matches = unmatched.indices.filter { old.element.matches(unmatched[$0].element.element) }
        guard matches.count == 1 else { return false }
        let current = unmatched.remove(at: matches[0])
        guard old.facts == current.facts else { return false }
    }
    return unmatched.isEmpty
}

private func focusedRegisteredStandardRoot(_ focused: CFTypeRef?,
                                           roots: [RegisteredAuxiliaryAXWindowRootObservation]) -> Bool {
    guard let focused, CFGetTypeID(focused) == AXUIElementGetTypeID() else { return false }
    let standards = roots.filter {
        $0.facts.role == kAXWindowRole && $0.facts.subrole == kAXStandardWindowSubrole
    }
    return standards.count == 1 && standards[0].element.matches(focused)
}

enum ReadPrivacyRejection: String, Sendable {
    case notFrontmost = "not_frontmost"
    case focusedWindowChanged = "focused_window_changed"
    case scopeBoundaryChanged = "scope_boundary_changed"
    case modalOrMenuUnresolved = "modal_or_menu_unresolved"
    case visibleCoverageRejected = "visible_coverage_rejected"
    case rootContinuityChanged = "root_continuity_changed"
    case processUnavailable = "process_unavailable"
    case observerRootUnavailable = "observer_root_unavailable"
    case notificationDuringRead = "notification_during_read"
    case readUnavailable = "read_unavailable"
}

struct StructureRead: Sendable {
    var structure: Structure
    let started: UInt64
    let finished: UInt64
    let secureFrames: [Rect]
    var visiblePrivacy: VisiblePrivacyCoverage? = nil
    var privacyRejection: ReadPrivacyRejection? = nil
}

/// A read-local reference and its observed parent/order. These AX refs stay on
/// the synchronous AX worker and never cross the Sendable Structure boundary.
struct ObservedAXElement {
    let element: AXUIElement
    let id: String
    let parentElement: AXUIElement?
    let siblingIndex: Int?
    let identity_continuity: UiObjectIdentityContinuity
}

private struct ScopedTableVisitPlan {
    let element: AXUIElement
    let parent: AXUIElement
    let role: String
    let subrole: String?
    let frame: Rect
    let parentRole: String
    let parentSubrole: String?
    let depth: Int
    let siblingIndex: Int?
}

private struct AXRootIdentityFact {
    let element: AXUIElement
    let role: String
    let subrole: String?
    let frame: Rect
}

func isSensitiveField(role: String, subrole: String?, registeredEditor: Bool = false) -> Bool {
    let textEntry = [kAXTextFieldRole, kAXTextAreaRole, kAXComboBoxRole].contains(role)
    // A registered normal editor may expose text. Secure and unknown subroles
    // remain private even within the registered process/window.
    if role == "AXSecureTextField" || subrole == kAXSecureTextFieldSubrole { return true }
    if registeredEditor && [kAXTextFieldRole, kAXTextAreaRole].contains(role) && (subrole == nil || subrole == "") { return false }
    return textEntry
}

func axNumericValue(_ number: NSNumber, role: String) -> UiValue? {
    if CFGetTypeID(number) == CFBooleanGetTypeID() { return .bool(number.boolValue) }
    let value = number.doubleValue
    guard value.isFinite else { return nil }
    // AX toggle values may be 2 (mixed). Preserve that state rather than
    // treating any nonzero number as the desired checked value.
    if [kAXCheckBoxRole, kAXRadioButtonRole].contains(role), value == 0 || value == 1 {
        return .bool(value == 1)
    }
    return .number(value)
}

func sourceGroupContext(
    for role: String,
    title: String?,
    description: String?,
    inherited: String?
) -> String? {
    guard role == kAXGroupRole || role == "AXRadioGroup" else { return inherited }
    return title ?? description ?? inherited
}

func axScrollAxisState(value: AnyObject?, minimum: AnyObject?, maximum: AnyObject?) -> ScrollAxisState? {
    guard let value = value as? NSNumber, CFGetTypeID(value) == CFNumberGetTypeID() else { return nil }
    var lower: Double?, upper: Double?
    if let minimum {
        guard let number = minimum as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID() else { return nil }
        lower = number.doubleValue
    }
    if let maximum {
        guard let number = maximum as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID() else { return nil }
        upper = number.doubleValue
    }
    return ScrollAxisState(value: value.doubleValue, min: lower, max: upper)
}

private struct PrivacySignature: Equatable {
    let window: String
    let role: String
    let subrole: String?
    let frame: Rect
    let title: String?
    let description: String?
    let value: UiValue?
    let secure: Bool
    let scroll: ScrollState?
}

/// Compare observed geometry and content separately from instance continuity,
/// solely to reject a changed privacy context.
func unchangedPrivacyContext(before: StructureRead, after: StructureRead) -> Bool {
    guard before.secureFrames.isEmpty, after.secureFrames.isEmpty,
          before.structure.active_layer == after.structure.active_layer else { return false }
    if before.structure.complete || after.structure.complete {
        guard before.structure.complete, after.structure.complete else { return false }
    } else {
        guard let beforeLayer = before.structure.active_layer,
              let afterLayer = after.structure.active_layer,
              beforeLayer == afterLayer,
              let beforeWindow = before.structure.windows.filter({ $0.window_id == beforeLayer }).first,
              before.structure.windows.filter({ $0.window_id == beforeLayer }).count == 1,
              let afterWindow = after.structure.windows.filter({ $0.window_id == afterLayer }).first,
              after.structure.windows.filter({ $0.window_id == afterLayer }).count == 1,
              captureReadPrivacyEligible(before, window: beforeWindow),
              captureReadPrivacyEligible(after, window: afterWindow),
              let beforeCoverage = before.visiblePrivacy,
              let afterCoverage = after.visiblePrivacy,
              beforeCoverage.bundleID == afterCoverage.bundleID,
              beforeCoverage.processID == afterCoverage.processID,
              beforeCoverage.processStamp == afterCoverage.processStamp,
              beforeCoverage.windowID == afterCoverage.windowID,
              beforeCoverage.windowFrame == afterCoverage.windowFrame,
              beforeCoverage.tableScope == afterCoverage.tableScope,
              beforeCoverage.tableViewport == afterCoverage.tableViewport,
              beforeCoverage.auxiliaryWindows == afterCoverage.auxiliaryWindows,
              beforeCoverage.visibleTreeFingerprint == afterCoverage.visibleTreeFingerprint else { return false }
    }
    func signatures(_ read: StructureRead) -> [PrivacySignature] {
        // SCStream excludes child drawing windows. A fully proved menu is its
        // own AX evidence domain, not a change to the captured root's privacy
        // coverage. Secure frames and every nonmenu root object remain checked.
        let context = read.structure.menu_context
        return read.structure.objects.filter { object in
            guard let context else { return true }
            if object.instance_ref == context.menu_instance && object.role == kAXMenuRole && !object.secure { return false }
            if object.parent == context.menu_instance && object.role == kAXMenuItemRole && !object.secure { return false }
            return true
        }.map {
            PrivacySignature(window: $0.window_id, role: $0.role, subrole: $0.subrole, frame: $0.frame,
                             title: $0.title, description: $0.description, value: $0.value, secure: $0.secure, scroll: $0.scroll)
        }
    }
    return signatures(before) == signatures(after)
}

/// The window list is metadata only; this never captures another application.
func targetWindows(pid: UInt32) -> [NativeWindow] {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] else { return [] }
    return list.enumerated().compactMap { z, info in
        guard let owner = info[kCGWindowOwnerPID as String] as? NSNumber, owner.uint32Value == pid,
              let id = info[kCGWindowNumber as String] as? NSNumber,
              let layer = info[kCGWindowLayer as String] as? NSNumber, layer.intValue == 0,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return nil }
        let frame = Rect(cgFrame)
        guard frame.valid, frame.width > 0, frame.height > 0 else { return nil }
        return NativeWindow(id: id.uint32Value, pid: pid, frame: frame, zIndex: UInt32(z))
    }.prefix(32).map { $0 }
}

/// All AX references remain inside this dedicated synchronous worker. Only typed
/// Sendable values leave it. No AX call runs on the main actor or device process.
func readStructure(app: AppIdentity, frontmostPID: UInt32?, request: NativeRequest) async -> StructureRead {
    await withCheckedContinuation { continuation in
        DispatchQueue(label: "cu.source.ax", qos: .userInitiated).async {
            let reader = AXReader(app: app, request: request)
            continuation.resume(returning: reader.read(frontmostPID: frontmostPID))
        }
    }
}

/// Constructed and consumed on one AX worker. The optional identity provider
/// never escapes that worker and must not retain text or perform input.
final class AXReader {
    let app: AppIdentity
    let request: NativeRequest
    let started = uptimeNs()
    let runID = UUID().uuidString.lowercased()
    var deadline: UInt64
    var reasons: [String] = []
    var objects: [UiObject] = []
    var secureFrames: [Rect] = []
    var seen: [AXUIElement] = []
    var modalFound = false
    var unsupportedModalFound = false
    var modalSheetCount = 0
    var modalSheetWindows: [(window: NativeWindow, title: String?)] = []
    var auxiliaryDialogVisitCount = 0
    var axWindowRootCount = 0
    var frontmost = false
    var focusedWindowElement: CFTypeRef?
    var nativeWindows: [NativeWindow] = []
    var transientMenuFound = false
    var observedElements: [ObservedAXElement] = []
    var observedTables: [ObservedAXTable] = []
    var actualAXWindowRoots: [MenuRetainedToken] = []
    private(set) var auxiliaryPrivacyAuditInput: RegisteredAuxiliaryPrivacyAuditInput?
    private var auxiliaryPrivacyMenuProven = false
    var tableScopeNodeRefs: [AXUIElement] = []
    var pendingTableScopeSearch: PendingTableScopeSearch?
    private(set) var rootIdentityVerified = false
    let menuRegistry: MenuAnchorRegistry?
    let identityProvider: ((AXUIElement, String, String?, Rect, String, MenuRetainedToken)
        -> AXIdentityAssignment?)?

    init(app: AppIdentity, request: NativeRequest,
         identityProvider: ((AXUIElement, String, String?, Rect, String, MenuRetainedToken)
            -> AXIdentityAssignment?)? = nil,
         deadline: UInt64? = nil, menuRegistry: MenuAnchorRegistry? = nil) {
        self.app = app; self.request = request
        self.identityProvider = identityProvider
        self.menuRegistry = menuRegistry
        // Leave part of the process budget for capture and OCR.
        self.deadline = deadline ?? started + UInt64(min(request.timeout / 2, 750)) * 1_000_000
    }

    func mark(_ reason: String) {
        if reasons.count < 8, !reasons.contains(reason) { reasons.append(reason) }
    }

    func budget() -> Bool {
        if uptimeNs() >= deadline { mark("AX traversal time budget exceeded"); return false }
        return true
    }

    /// Background AX requests install a per-call timeout bounded by both the
    /// common 25 ms ceiling and the original traversal deadline. Foreground
    /// reads retain their existing inherited 40 ms timeout behavior.
    func beforeQuery(_ element: AXUIElement) -> Bool {
        guard request.background_target != nil else { return true }
        let now = uptimeNs()
        guard now < deadline else { mark("AX traversal time budget exceeded"); return false }
        guard let timeout = backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: deadline - now),
              AXUIElementSetMessagingTimeout(element, timeout) == .success,
              uptimeNs() < deadline else {
            mark("background AX query timeout could not be bounded")
            return false
        }
        return true
    }

    var remainingTableScopeNodeBudget: Int {
        guard request.table_scope != nil else { return request.nodeLimit }
        return max(request.nodeLimit - tableScopeNodeRefs.count, 0)
    }

    func reserveTableScopeNode(_ element: AXUIElement) -> Bool {
        guard request.table_scope != nil else { return true }
        if tableScopeNodeRefs.contains(where: { CFEqual($0, element) }) { return true }
        guard tableScopeNodeRefs.count < request.nodeLimit else { return false }
        tableScopeNodeRefs.append(element)
        return true
    }

    func attribute(_ element: AXUIElement, _ name: String, required: Bool = false) -> CFTypeRef? {
        guard budget(), beforeQuery(element) else { return nil }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        if request.background_target != nil, !budget() { return nil }
        if error == .success { return value }
        if error != .attributeUnsupported && error != .noValue || required {
            mark("AX attribute unavailable: \(name) (\(error.rawValue))")
        }
        return nil
    }

    func semanticText(_ text: String, attribute name: String) -> String {
        // The bounded projection cannot establish a complete label or value:
        // a discarded suffix may distinguish two execution targets.
        if text.utf8.prefix(513).count > 512 { mark("AX text budget exceeded: \(name)") }
        return boundedText(text)
    }

    func semanticValue(_ text: String, registeredEditor: Bool) -> String {
        let bytes = registeredEditor ? 2048 : 512
        if text.utf8.prefix(bytes + 1).count > bytes { mark("AX text budget exceeded: \(kAXValueAttribute)") }
        // Values participate in exact predicates. Do not strip Unicode or
        // control scalars and turn a different UI value into a matching one.
        var result = String(decoding: text.utf8.prefix(bytes), as: UTF8.self)
        while result.utf8.count > bytes { result.removeLast() }
        return result
    }

    func string(_ element: AXUIElement, _ name: String, required: Bool = false) -> String? {
        attribute(element, name, required: required).flatMap { $0 as? String }
            .map { semanticText($0, attribute: name) }
    }

    func boolean(_ element: AXUIElement, _ name: String) -> Bool? {
        (attribute(element, name) as? NSNumber)?.boolValue
    }

    func strictBoolean(_ element: AXUIElement, _ name: String) -> Bool? {
        guard let raw = attribute(element, name, required: true) else { return nil }
        guard CFGetTypeID(raw) == CFBooleanGetTypeID(), let number = raw as? NSNumber else {
            mark("AX Boolean type unavailable: \(name)")
            return nil
        }
        return number.boolValue
    }

    func proofString(_ element: AXUIElement, _ name: String, required: Bool) -> String? {
        guard let raw = attribute(element, name, required: required) else { return nil }
        guard let value = raw as? String else {
            mark("AX string type unavailable: \(name)")
            return nil
        }
        return semanticText(value, attribute: name)
    }

    private func registeredWindowRootObservations(_ elements: [AXUIElement],
                                                  nativeWindows: [NativeWindow]) -> [RegisteredAuxiliaryAXWindowRootObservation] {
        var observations = elements.map { element in
            var pid: pid_t = 0
            let processPID: UInt32? = beforeQuery(element)
                && AXUIElementGetPid(element, &pid) == .success && pid > 0
                ? UInt32(pid) : nil
            if processPID != app.pid { mark("AX window process identity unavailable") }
            let role = proofString(element, kAXRoleAttribute, required: true)
            let subrole = proofString(element, kAXSubroleAttribute, required: elements.count > 1)
            let rootFrame = frame(element)
            let matches = rootFrame.map { frame in
                nativeWindows.filter { $0.pid == app.pid && $0.frame == frame }
            } ?? []
            return RegisteredAuxiliaryAXWindowRootObservation(
                element: MenuRetainedToken(element),
                facts: RegisteredAuxiliaryAXWindowFacts(pid: processPID, role: role, subrole: subrole,
                    frame: rootFrame, modal: nil, focused: nil, exactCGMatches: matches))
        }
        let standardIndices = observations.indices.filter {
            observations[$0].facts.role == kAXWindowRole
                && observations[$0].facts.subrole == kAXStandardWindowSubrole
        }
        let dialogIndices = observations.indices.filter {
            observations[$0].facts.role == kAXWindowRole && observations[$0].facts.subrole == "AXDialog"
        }
        if observations.count == 2, standardIndices.count == 1, dialogIndices.count == 1 {
            for index in observations.indices {
                let element = elements[index]
                let modal = strictBoolean(element, kAXModalAttribute)
                let focused = index == dialogIndices[0] ? strictBoolean(element, kAXFocusedAttribute) : nil
                let old = observations[index]
                observations[index] = RegisteredAuxiliaryAXWindowRootObservation(
                    element: old.element,
                    facts: RegisteredAuxiliaryAXWindowFacts(pid: old.facts.pid, role: old.facts.role,
                        subrole: old.facts.subrole, frame: old.facts.frame, modal: modal,
                        focused: focused, exactCGMatches: old.facts.exactCGMatches))
            }
        }
        return observations
    }

    private func ownsAuxiliaryPrivacyAuditInput(_ input: RegisteredAuxiliaryPrivacyAuditInput) -> Bool {
        guard let current = auxiliaryPrivacyAuditInput else { return false }
        return current.bundleID == input.bundleID && current.processID == input.processID
            && current.standardRoot.token === input.standardRoot.token
            && current.standardRoot.facts == input.standardRoot.facts
            && current.dialogRoot.token === input.dialogRoot.token
            && current.dialogRoot.facts == input.dialogRoot.facts
            && current.processStamp == input.processStamp
            && current.cgInventory == input.cgInventory
    }

    /// Return the retained actual roots only to code running synchronously on
    /// this reader's AX worker. The caller must revalidate before and after its
    /// descendant audit; these references never enter Structure or Source wire.
    func auxiliaryPrivacyAuditRootElements(
        _ input: RegisteredAuxiliaryPrivacyAuditInput
    ) -> (standard: AXUIElement, dialog: AXUIElement)? {
        guard budget(), ownsAuxiliaryPrivacyAuditInput(input),
              CFGetTypeID(input.standardRoot.token.element) == AXUIElementGetTypeID(),
              CFGetTypeID(input.dialogRoot.token.element) == AXUIElementGetTypeID() else { return nil }
        return (unsafeDowncast(input.standardRoot.token.element, to: AXUIElement.self),
                unsafeDowncast(input.dialogRoot.token.element, to: AXUIElement.self))
    }

    /// Re-read the exact root/focus/process/CG boundary under the original AX
    /// deadline. An equal frame with a replacement AX object is rejected.
    func revalidateAuxiliaryPrivacyAuditInput(
        _ input: RegisteredAuxiliaryPrivacyAuditInput,
        application: AXUIElement,
        expectedStandard: AXUIElement
    ) -> Bool {
        guard budget(), ownsAuxiliaryPrivacyAuditInput(input),
              input.bundleID == app.bundle_id, input.processID == app.pid,
              frontmost, !unsupportedModalFound, !transientMenuFound,
              !auxiliaryPrivacyMenuProven,
              CFGetTypeID(application) == AXUIElementGetTypeID(),
              CFGetTypeID(expectedStandard) == AXUIElementGetTypeID() else { return false }

        let system = AXUIElementCreateSystemWide()
        guard AXUIElementSetMessagingTimeout(system, 0.04) == .success,
              let liveAXWindows = boundedRootWindows(application), liveAXWindows.count == 2,
              budget(),
              let focusedRaw = attribute(application, kAXFocusedWindowAttribute, required: true),
              CFGetTypeID(focusedRaw) == AXUIElementGetTypeID(),
              CFEqual(focusedRaw, expectedStandard),
              let focusedApplicationRaw = attribute(system, kAXFocusedApplicationAttribute, required: true),
              CFGetTypeID(focusedApplicationRaw) == AXUIElementGetTypeID() else { return false }

        let focusedApplication = unsafeDowncast(focusedApplicationRaw, to: AXUIElement.self)
        var focusedPID: pid_t = 0
        guard AXUIElementGetPid(focusedApplication, &focusedPID) == .success,
              focusedPID > 0, UInt32(focusedPID) == input.processID,
              budget() else { return false }

        let liveCGInventory = targetWindows(pid: input.processID)
        let liveRoots = registeredWindowRootObservations(liveAXWindows, nativeWindows: liveCGInventory)
            .map { RegisteredAuxiliaryPrivacyAuditRoot(token: $0.element, facts: $0.facts) }
        let currentProcessStamp = ProcessStamp.read(pid: input.processID)
        let menuDrawingFound = !nativeMenuDrawingWindows(pid: input.processID).isEmpty
        guard budget() else { return false }
        return registeredAuxiliaryPrivacyAuditRevalidationMatches(
            input: input, currentRoots: liveRoots, expectedStandard: expectedStandard,
            focusedWindow: focusedRaw, currentProcessStamp: currentProcessStamp,
            currentCGInventory: liveCGInventory, frontmost: true,
            transientMenuFound: transientMenuFound, menuDrawingFound: menuDrawingFound,
            menuProven: auxiliaryPrivacyMenuProven)
    }

    func frame(_ element: AXUIElement) -> Rect? {
        guard let position = attribute(element, kAXPositionAttribute, required: true),
              CFGetTypeID(position) == AXValueGetTypeID(),
              let size = attribute(element, kAXSizeAttribute, required: true),
              CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var p = CGPoint.zero; var s = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(position, to: AXValue.self), .cgPoint, &p),
              AXValueGetValue(unsafeDowncast(size, to: AXValue.self), .cgSize, &s) else {
            mark("AX geometry type unavailable"); return nil
        }
        let result = Rect(CGRect(origin: p, size: s))
        guard result.valid else { mark("AX geometry invalid"); return nil }
        return result
    }

    func children(_ element: AXUIElement, attribute name: String, maximum: Int, required: Bool) -> [AXUIElement] {
        guard budget(), beforeQuery(element) else { return [] }
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(element, name as CFString, &count)
        if request.background_target != nil, !budget() { return [] }
        if countError == .attributeUnsupported || countError == .noValue {
            if required { mark("AX child list unavailable: \(name)") }
            return []
        }
        guard countError == .success else { mark("AX child count failed: \(countError.rawValue)"); return [] }
        guard count > 0 else { return [] }
        let remaining = min(maximum, remainingTableScopeNodeBudget)
        if count > remaining { mark("AX child list truncated") }
        guard remaining > 0, budget(), beforeQuery(element) else { return [] }
        var result: CFArray?
        let error = AXUIElementCopyAttributeValues(element, name as CFString, 0, min(count, remaining), &result)
        if request.background_target != nil, !budget() { return [] }
        guard error == .success, let values = result as? [AXUIElement] else {
            mark("AX child read failed: \(error.rawValue)"); return []
        }
        return values
    }

    func boundedRootWindows(_ application: AXUIElement) -> [AXUIElement]? {
        guard budget(), beforeQuery(application) else { return nil }
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(application, kAXWindowsAttribute as CFString, &count)
        if request.background_target != nil, !budget() { return nil }
        guard countError == .success, count > 0, count <= 32 else {
            mark("AX root window inventory unavailable or over budget")
            return nil
        }
        let elements = children(application, attribute: kAXWindowsAttribute, maximum: 32, required: true)
        guard elements.count == count, elements.count <= 32 else { return nil }
        return elements
    }

    private func rootFacts(_ elements: [AXUIElement]) -> [AXRootIdentityFact]? {
        guard !elements.isEmpty, elements.count <= 32 else { return nil }
        var facts: [AXRootIdentityFact] = []
        for element in elements {
            guard budget(), beforeQuery(element) else { return nil }
            var pid: pid_t = 0
            let pidError = AXUIElementGetPid(element, &pid)
            if request.background_target != nil, !budget() { return nil }
            guard pidError == .success, pid > 0,
                  UInt32(pid) == app.pid,
                  let role = string(element, kAXRoleAttribute, required: true),
                  role == kAXWindowRole,
                  let rect = frame(element) else { return nil }
            facts.append(AXRootIdentityFact(element: element, role: role,
                subrole: string(element, kAXSubroleAttribute), frame: rect))
        }
        return facts
    }

    private func sameRootFacts(_ before: [AXRootIdentityFact]?,
                               _ after: [AXRootIdentityFact]?) -> Bool {
        guard let before, let after, before.count == after.count,
              before.count > 0, before.count <= 32 else { return false }
        var unmatched = after
        for old in before {
            let matches = unmatched.indices.filter { CFEqual(old.element, unmatched[$0].element) }
            guard matches.count == 1 else { return false }
            let current = unmatched.remove(at: matches[0])
            guard old.role == current.role, old.subrole == current.subrole,
                  old.frame == current.frame else { return false }
        }
        return unmatched.isEmpty
    }

    func actions(_ element: AXUIElement, secure: Bool) -> [String] {
        guard !secure, budget(), beforeQuery(element) else { return [] }
        var names: CFArray?
        let error = AXUIElementCopyActionNames(element, &names)
        var result: [String] = []
        if error == .success, let actions = names as? [String] {
            for (ax, value) in [(kAXPressAction, "press"), (kAXShowMenuAction, "show_menu")] {
                if actions.contains(ax) { result.append(value) }
            }
        } else if error != .notImplemented && error != .attributeUnsupported {
            mark("AX actions unavailable: \(error.rawValue)")
        }
        if budget(), beforeQuery(element) {
            var settable = DarwinBoolean(false)
            let setError = AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable)
            if setError == .success, settable.boolValue { result.append("set_value") }
            else if setError != .attributeUnsupported && setError != .noValue && setError != .success {
                mark("AX value capability unavailable: \(setError.rawValue)")
            }
        }
        return result
    }

    func scrollAxis(_ element: AXUIElement, attribute name: String) -> ScrollAxisState? {
        guard budget(), objects.count < request.nodeLimit,
              let raw = attribute(element, name), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        let bar = unsafeDowncast(raw, to: AXUIElement.self)
        var owner: pid_t = 0
        // Do not read any scrollbar content from a foreign or reused process.
        guard budget(), beforeQuery(bar), AXUIElementGetPid(bar, &owner) == .success, budget(),
              owner > 0, UInt32(owner) == app.pid else {
            mark("AX scrollbar process identity unavailable"); return nil
        }
        guard string(bar, kAXRoleAttribute) == kAXScrollBarRole,
              let value = attribute(bar, kAXValueAttribute) else { return nil }
        // Stock NSScroller supplies AXValue but does not expose AXMin/MaxValue.
        // Preserve that observed value; missing limits remain unknown rather
        // than importing a guessed numeric domain from the scrollbar's role.
        let minimum = attribute(bar, kAXMinValueAttribute)
        let maximum = attribute(bar, kAXMaxValueAttribute)
        guard budget() else { return nil }
        return axScrollAxisState(value: value, minimum: minimum, maximum: maximum)
    }

    func scrollState(_ element: AXUIElement, role: String, secure: Bool) -> ScrollState? {
        guard !secure, role == kAXScrollAreaRole else { return nil }
        let horizontal = scrollAxis(element, attribute: kAXHorizontalScrollBarAttribute)
        let vertical = scrollAxis(element, attribute: kAXVerticalScrollBarAttribute)
        guard horizontal != nil || vertical != nil, budget() else { return nil }
        return ScrollState(horizontal: horizontal, vertical: vertical)
    }

    /// Materialize only objects already enumerated by the same table viewport
    /// read. Header labels are followed upward through their real AXParent chain
    /// and never cause AXColumn children to be enumerated.
    private func materializeScopedTableViewport(
        _ table: ObservedAXTable,
        tableElement scopeTable: AXUIElement,
        tableRole: String,
        tableSubrole: String?,
        tableDepth: Int,
        window: NativeWindow,
        viewport: Rect,
        group: String?,
        sheetRoot: AXUIElement?,
        ownerWindowRoot: MenuRetainedToken
    ) -> Set<String>? {
        guard table.rowsComplete, table.columnsComplete,
              table.rows.allSatisfy(\.complete), budget() else {
            mark("AX scoped table viewport is incomplete")
            return nil
        }

        var plans: [ScopedTableVisitPlan] = []

        func same(_ lhs: AXUIElement, _ rhs: AXUIElement) -> Bool { CFEqual(lhs, rhs) }

        func parentOf(_ element: AXUIElement) -> AXUIElement? {
            guard budget(), let raw = attribute(element, kAXParentAttribute, required: true),
                  CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                mark("AX scoped table parent is unavailable")
                return nil
            }
            let parent = unsafeDowncast(raw, to: AXUIElement.self)
            var pid: pid_t = 0
            guard budget(), AXUIElementGetPid(parent, &pid) == .success,
                  budget(), pid > 0, UInt32(pid) == app.pid else {
                mark("AX scoped table parent process identity is unavailable")
                return nil
            }
            return parent
        }

        func roleInfo(_ element: AXUIElement) -> (String, String?)? {
            if same(element, scopeTable) { return (tableRole, tableSubrole) }
            if let plan = plans.first(where: { same($0.element, element) }) {
                return (plan.role, plan.subrole)
            }
            guard let role = string(element, kAXRoleAttribute, required: true) else { return nil }
            return (role, string(element, kAXSubroleAttribute))
        }

        func add(
            _ element: AXUIElement,
            expectedRole: String?,
            expectedParent: AXUIElement,
            expectedFrame: Rect?,
            depth: Int,
            siblingIndex: Int?,
            allowExactDuplicate: Bool = false
        ) -> Bool {
            guard budget(), depth <= request.depthLimit else {
                mark("AX scoped table materialization exceeded its depth or time budget")
                return false
            }
            var pid: pid_t = 0
            guard AXUIElementGetPid(element, &pid) == .success,
                  pid > 0, UInt32(pid) == app.pid,
                  let role = string(element, kAXRoleAttribute, required: true),
                  (expectedRole.map({ role == $0 }) ?? true),
                  let actualParent = parentOf(element), same(actualParent, expectedParent),
                  let rect = frame(element), (expectedFrame.map({ rect == $0 }) ?? true) else {
                mark("AX scoped table object role, parent, process, or frame changed")
                return false
            }
            if let existing = plans.first(where: { same($0.element, element) }) {
                let exact = same(existing.parent, expectedParent) && existing.role == role
                    && existing.frame == rect
                guard allowExactDuplicate && exact else {
                    mark("AX scoped table object was repeated")
                    return false
                }
                return true
            }
            guard !seen.contains(where: { same($0, element) }),
                  !observedElements.contains(where: { same($0.element, element) }) else {
                mark("AX scoped table object is already present in the global walk")
                return false
            }
            guard objects.count + plans.count < request.nodeLimit,
                  reserveTableScopeNode(element),
                  let parentInfo = roleInfo(expectedParent) else {
                mark("AX scoped table materialization exceeded its node budget or lost its parent")
                return false
            }
            plans.append(ScopedTableVisitPlan(element: element, parent: expectedParent,
                role: role, subrole: string(element, kAXSubroleAttribute), frame: rect,
                parentRole: parentInfo.0, parentSubrole: parentInfo.1,
                depth: depth, siblingIndex: siblingIndex))
            return true
        }

        for row in table.rows {
            guard add(row.element, expectedRole: kAXRowRole, expectedParent: scopeTable,
                      expectedFrame: row.frame, depth: tableDepth + 1,
                      siblingIndex: nil) else { return nil }
            for cell in row.cells {
                guard add(cell.element, expectedRole: kAXCellRole, expectedParent: row.element,
                          expectedFrame: cell.frame, depth: tableDepth + 2,
                          siblingIndex: nil) else { return nil }
            }
        }

        for column in table.columns {
            guard add(column.element, expectedRole: kAXColumnRole, expectedParent: scopeTable,
                      expectedFrame: column.frame, depth: tableDepth + 1,
                      siblingIndex: nil) else { return nil }
            guard let header = column.header else { continue }
            guard !same(header, column.element), !same(header, scopeTable) else {
                mark("AX scoped table header aliases its column or table")
                return nil
            }

            var chain: [AXUIElement] = []
            var chainVisited: [AXUIElement] = []
            var current = header
            while !same(current, column.element) && !same(current, scopeTable) {
                guard chain.count < request.depthLimit,
                      !chainVisited.contains(where: { same($0, current) }),
                      !seen.contains(where: { same($0, current) }) else {
                    mark("AX scoped table header parent chain is repeated or too deep")
                    return nil
                }
                let currentRole = string(current, kAXRoleAttribute, required: true)
                guard let currentRole,
                      scopedTableHeaderRoleAllowed(currentRole, isHeaderLeaf: same(current, header)) else {
                    let knownRoles = [kAXButtonRole, kAXScrollAreaRole, kAXWindowRole,
                                      kAXColumnRole, kAXTableRole, kAXCellRole, kAXRowRole]
                    let diagnosticRole = currentRole.flatMap { knownRoles.contains($0) ? $0 : nil } ?? "unknown"
                    mark("AX scoped table header parent chain has an unsupported role \(diagnosticRole)")
                    return nil
                }
                chainVisited.append(current)
                chain.append(current)
                guard let parent = parentOf(current) else { return nil }
                current = parent
            }
            guard same(current, column.element) || same(current, scopeTable) else {
                mark("AX scoped table header is detached from its actual column or table")
                return nil
            }
            let parentDepth = same(current, scopeTable) ? tableDepth : tableDepth + 1
            for (offset, element) in chain.reversed().enumerated() {
                guard let expectedRole = string(element, kAXRoleAttribute, required: true) else {
                    mark("AX scoped table header role is unavailable")
                    return nil
                }
                guard let actualParent = parentOf(element),
                      add(element, expectedRole: expectedRole, expectedParent: actualParent,
                          expectedFrame: nil, depth: parentDepth + offset + 1,
                          siblingIndex: nil, allowExactDuplicate: true),
                      let node = plans.first(where: { same($0.element, element) }),
                      scopedTableHeaderRoleAllowed(node.role, isHeaderLeaf: same(element, header)) else {
                    mark("AX scoped table header object has an unsupported role or parent")
                    return nil
                }
            }
        }

        var refs = Set<String>()
        var childGroups: [(element: AXUIElement, value: String?)] = []
        for plan in plans {
            let inheritedGroup: String?
            if same(plan.parent, scopeTable) {
                inheritedGroup = group
            } else if let parentGroup = childGroups.first(where: { same($0.element, plan.parent) }) {
                inheritedGroup = parentGroup.value
            } else {
                mark("AX scoped table object could not link to its parent context")
                return nil
            }
            guard let parentObserved = observedElements.first(where: { same($0.element, plan.parent) }) else {
                mark("AX scoped table object could not link to a materialized parent")
                return nil
            }
            let objectsBefore = objects.count
            let elementsBefore = observedElements.count
            let unsupportedModalBefore = unsupportedModalFound
            visit(plan.element, parent: parentObserved.id, parentRole: plan.parentRole,
                  parentSubrole: plan.parentSubrole, window: window, depth: plan.depth,
                  viewport: viewport, group: inheritedGroup, sheetRoot: sheetRoot,
                  ownerWindowRoot: ownerWindowRoot, parentElement: plan.parent,
                  siblingIndex: plan.siblingIndex, traverseChildren: false)
            if let emitted = observedElements.last, same(emitted.element, plan.element),
               objects.count == objectsBefore + 1,
               objects[..<objectsBefore].contains(where: { $0.instance_ref == emitted.id }) {
                objects.removeLast()
                observedElements.removeLast()
                if let last = seen.last, same(last, plan.element) { seen.removeLast() }
                mark("AX scoped table projection produced a duplicate instance reference")
                return nil
            }
            guard objects.count == objectsBefore + 1,
                  observedElements.count == elementsBefore + 1,
                  let observed = observedElements.last, same(observed.element, plan.element),
                  let projected = objects.last, projected.role == plan.role,
                  projected.subrole == plan.subrole, projected.frame == plan.frame,
                  projected.parent == parentObserved.id, projected.window_id == window.ref,
                  !unsupportedModalFound || unsupportedModalBefore,
                  refs.insert(observed.id).inserted else {
                mark("AX scoped table object could not be safely projected")
                return nil
            }
            let childGroup = sourceGroupContext(
                for: projected.role, title: projected.title,
                description: projected.description, inherited: inheritedGroup)
            childGroups.append((plan.element, childGroup))
        }
        return refs
    }

    func visit(_ element: AXUIElement, parent: String?, parentRole: String?, parentSubrole: String?,
               window: NativeWindow, depth: Int, viewport: Rect, group: String?,
               sheetRoot: AXUIElement?, ownerWindowRoot: MenuRetainedToken,
               parentElement: AXUIElement?, siblingIndex: Int?, traverseChildren: Bool = true) {
        guard budget() else { return }
        guard objects.count < request.nodeLimit, reserveTableScopeNode(element) else {
            mark("AX node budget exceeded"); return
        }
        guard depth <= request.depthLimit else { mark("AX depth budget exceeded"); return }
        guard !seen.contains(where: { CFEqual($0, element) }) else { mark("AX cyclic or repeated element"); return }
        seen.append(element)
        var owner: pid_t = 0
        guard beforeQuery(element), AXUIElementGetPid(element, &owner) == .success,
              owner > 0, UInt32(owner) == app.pid else {
            mark("AX child process identity unavailable"); return
        }
        let role = string(element, kAXRoleAttribute, required: true)
        let subrole = string(element, kAXSubroleAttribute)
        guard let role else { return }
        guard let rect = frame(element) else { return }
        let modal = boolean(element, kAXModalAttribute)
        let elementFocused = boolean(element, kAXFocusedAttribute)
        let isSheetRole = role == kAXSheetRole
        let hasSheetRole = isSheetRole || subrole == kAXSheetRole
        let isRegisteredAuxiliaryDialogRoot = isRegisteredAuxiliaryAXDialogRoot(
            bundleID: app.bundle_id, role: role, subrole: subrole,
            topLevel: parent == nil && depth == 0)
        let hasUnsupportedLayerRole = ["AXDialog", "AXPopover"].contains(role)
            || subrole == "AXPopover"
            || (subrole == "AXDialog" && !isRegisteredAuxiliaryDialogRoot)
        if modal == true {
            modalFound = true
            unsupportedModalFound = true
        }
        if hasSheetRole || hasUnsupportedLayerRole || isRegisteredAuxiliaryDialogRoot { modalFound = true }
        if hasUnsupportedLayerRole || (subrole == kAXSheetRole && !isSheetRole) {
            unsupportedModalFound = true
        }
        if isRegisteredAuxiliaryDialogRoot { auxiliaryDialogVisitCount += 1 }
        if isSheetRole { modalSheetCount += 1 }

        var projectedWindow = window
        var projectedParent = parent
        var projectedSheet = false
        let nextSheetRoot: AXUIElement?
        if isSheetRole {
            let focusedWindowMatchesSheet = focusedWindowElement.map { CFEqual($0, element) } ?? false
            if canProjectMvpAXSheet(
                bundleID: app.bundle_id, frontmost: frontmost,
                focusedWindowMatchesSheet: focusedWindowMatchesSheet,
                sheetRole: role, sheetFocused: elementFocused, sheetModal: modal,
                sheetSubrole: subrole, parentRole: parentRole, parentSubrole: parentSubrole,
                depth: depth,
                axWindowCount: axWindowRootCount, cgWindowCount: nativeWindows.count),
               let sheetWindow = matchAXSheetWindow(rect, attachedTo: window, windows: nativeWindows) {
                projectedWindow = sheetWindow
                projectedParent = nil
                projectedSheet = true
                modalSheetWindows.append((window: sheetWindow, title: nil))
            }
        }
        nextSheetRoot = projectedSheet ? element : sheetRoot
        if let sheetRoot, !CFEqual(element, sheetRoot) {
            guard let topLevel = attribute(element, kAXTopLevelUIElementAttribute, required: true),
                  CFGetTypeID(topLevel) == AXUIElementGetTypeID(), CFEqual(topLevel, sheetRoot) else {
                mark("AX sheet descendant is outside its certified top-level root")
                return
            }
        }
        // Editable text is private unless the registered policy matches this
        // exact process and CG-matched window; secure/unknown subroles stay private.
        let registered = request.text_policy?.valid(bundle: app.bundle_id, pid: request.pid) == true &&
            request.pid == app.pid && request.text_policy?.window_id == projectedWindow.ref
        let secure = isSensitiveField(role: role, subrole: subrole, registeredEditor: registered)
        if secure { secureFrames.append(rect) }
        let title = secure ? nil : string(element, kAXTitleAttribute)
        let description = secure ? nil : string(element, kAXDescriptionAttribute)
        var value: UiValue?
        if !secure, let raw = attribute(element, kAXValueAttribute) {
            if let number = raw as? NSNumber {
                value = axNumericValue(number, role: role)
            } else if let text = raw as? String { value = .text(semanticValue(text, registeredEditor: registered && [kAXTextFieldRole, kAXTextAreaRole].contains(role))) }
        }
        let assignment: AXIdentityAssignment
        if let identityProvider {
            guard let provided = identityProvider(element, role, subrole, rect, projectedWindow.ref,
                    ownerWindowRoot) else {
                mark("AX instance continuity could not be established")
                return
            }
            assignment = provided
        } else {
            assignment = AXIdentityAssignment(instance_ref: "ax-\(app.pid)-\(runID)-\(objects.count)",
                identity_continuity: .readScoped)
        }
        let id = assignment.instance_ref
        if role == kAXMenuRole { transientMenuFound = true }
        let nextGroup = sourceGroupContext(
            for: role, title: title, description: description, inherited: group)
        let objectViewport = projectedSheet ? rect : viewport
        objects.append(UiObject(instance_ref: id, parent: projectedParent, window_id: projectedWindow.ref,
                                role: role, subrole: subrole, title: title, description: description,
                                value: value, enabled: boolean(element, kAXEnabledAttribute) ?? false,
                                focused: elementFocused ?? false, frame: rect,
                                actions: request.background_target == nil
                                    ? actions(element, secure: secure) : [],
                                in_view: rect.intersects(objectViewport),
                                group: group, secure: secure, identity_continuity: assignment.identity_continuity,
                                scroll: scrollState(element, role: role, secure: secure)))
        observedElements.append(ObservedAXElement(element: element, id: id, parentElement: parentElement,
            siblingIndex: siblingIndex, identity_continuity: assignment.identity_continuity))
        let clip = projectedSheet || role == kAXMenuRole ? rect :
            role == kAXScrollAreaRole ? Rect(rect.cgRect.intersection(viewport.cgRect)) : viewport
        var observedTableRegistered = false
        var observedTable: ObservedAXTable?
        if role == kAXTableRole, let table = observeTableViewport(element, tableID: id,
            parentID: projectedParent, viewport: viewport) {
            observedTable = table
            observedTableRegistered = true
        }
        let scopeCensusComplete = pendingTableScopeSearch.map { $0.incompleteReason == nil } ?? false
        let prunableScopeTable = pendingTableScopeSearch.map {
            $0.incompleteReason == nil
                && $0.prunableTables.contains(where: { CFEqual($0, element) })
        } ?? false
        let materializeThisTable = pendingTableScopeSearch.map {
            $0.incompleteReason == nil && $0.candidates.count == 1
                && CFEqual($0.candidates[0].table, element)
        } ?? false
        let scopeWalkDecision = TableScopeSearchValidation.globalWalkDecision(
            scopeRequested: request.table_scope != nil, censusComplete: scopeCensusComplete,
            roleIsTable: role == kAXTableRole, viewportRegistered: observedTableRegistered,
            exactScopeReference: prunableScopeTable)
        switch scopeWalkDecision {
        case .pruneTableRowsAndMarkGlobalPartial:
            mark(scopedTableRowsOmittedReason)
            if var table = observedTable {
                if materializeThisTable {
                    table.scopedMaterializedRefs = materializeScopedTableViewport(
                        table, tableElement: element, tableRole: role, tableSubrole: subrole,
                        tableDepth: depth, window: projectedWindow, viewport: clip,
                        group: nextGroup, sheetRoot: nextSheetRoot,
                        ownerWindowRoot: ownerWindowRoot) ?? []
                    if table.scopedMaterializedRefs?.isEmpty == true,
                       !table.rows.isEmpty || !table.columns.isEmpty {
                        mark("AX scoped table visible-object materialization is incomplete")
                    }
                }
                observedTables.append(table)
            } else {
                mark("AX scoped table viewport disappeared before materialization")
            }
            return
        case .traverseChildren:
            if let observedTable { observedTables.append(observedTable) }
            break
        }
        // The viewport becomes eligible only if the completed read proves the
        // menu context below; otherwise the whole transient read stays partial.
        guard traverseChildren else { return }
        let childElements = children(element, attribute: kAXChildrenAttribute,
                                     maximum: min(request.nodeLimit - objects.count,
                                                  remainingTableScopeNodeBudget), required: projectedSheet)
        if projectedSheet && childElements.isEmpty { mark("AX sheet child list unavailable or empty") }
        for (index, child) in childElements.enumerated() {
            visit(child, parent: id, parentRole: role, parentSubrole: subrole,
                  window: projectedWindow, depth: depth + 1,
                  viewport: clip, group: nextGroup, sheetRoot: nextSheetRoot,
                  ownerWindowRoot: ownerWindowRoot, parentElement: element, siblingIndex: index)
        }
    }

    func read(frontmostPID: UInt32?) -> StructureRead {
        var structure = Structure(app: app)
        auxiliaryPrivacyAuditInput = nil
        auxiliaryPrivacyMenuProven = false
        let system = AXUIElementCreateSystemWide()
        // Applying the timeout to the system-wide element sets the default for
        // every AX element in this helper, including descendants.
        let inheritedTimeout: Float = request.background_target == nil ? 0.04 : 0.025
        let systemTimeoutReady = request.background_target == nil
            ? AXUIElementSetMessagingTimeout(system, inheritedTimeout) == .success
            : beforeQuery(system)
        guard systemTimeoutReady else {
            structure.incomplete_reason = "AX global messaging timeout could not be installed"
            return StructureRead(structure: structure, started: started, finished: uptimeNs(), secureFrames: [],
                                 privacyRejection: .readUnavailable)
        }
        let application = AXUIElementCreateApplication(pid_t(app.pid))
        let applicationTimeoutReady = request.background_target == nil
            ? AXUIElementSetMessagingTimeout(application, inheritedTimeout) == .success
            : beforeQuery(application)
        guard applicationTimeoutReady else {
            structure.incomplete_reason = "AX application timeout could not be installed"
            return StructureRead(structure: structure, started: started, finished: uptimeNs(), secureFrames: [],
                                 privacyRejection: .readUnavailable)
        }
        let registeredAuxiliaryProofScope = request.background_target == nil
            && supportsRegisteredAuxiliaryAXDialogMetadata(bundleID: app.bundle_id)
        let processBefore = registeredAuxiliaryProofScope ? ProcessStamp.read(pid: app.pid) : nil
        let nativeWindows = targetWindows(pid: app.pid)
        let axWindows = boundedRootWindows(application) ?? []
        let rootFactsBefore = rootFacts(axWindows)
        let focused = request.background_target == nil
            ? attribute(application, kAXFocusedWindowAttribute,
                required: registeredAuxiliaryProofScope && axWindows.count > 1)
            : nil
        self.nativeWindows = nativeWindows
        self.axWindowRootCount = axWindows.count
        self.frontmost = frontmostPID == app.pid
        self.focusedWindowElement = focused
        let auxiliaryRootsBefore = registeredAuxiliaryProofScope
            ? registeredWindowRootObservations(axWindows, nativeWindows: nativeWindows) : []
        var focusedNativeWindowMatches: [NativeWindow] = []
        if let tableScope = request.table_scope {
            pendingTableScopeSearch = captureTableScopeSearch(request: tableScope,
                frontmostPID: frontmostPID, axWindows: axWindows, focusedWindow: focused,
                nativeWindows: nativeWindows)
        }
        for (index, axWindow) in axWindows.enumerated() {
            guard budget(), let rect = frame(axWindow) else { continue }
            guard let window = matchAXWindow(rect, windows: nativeWindows) else {
                mark("AX window could not be uniquely matched to an on-screen window"); continue
            }
            let modal = boolean(axWindow, kAXModalAttribute) ?? false
            structure.windows.append(window.structureInfo(title: string(axWindow, kAXTitleAttribute),
                                                          modal: modal, ownerBundleID: app.bundle_id))
            if frontmostPID == app.pid, let focused,
               CFGetTypeID(focused) == AXUIElementGetTypeID(), CFEqual(focused, axWindow) {
                structure.active_layer = window.ref
            }
            if request.background_target == nil, frontmostPID == app.pid, let focused,
               CFGetTypeID(focused) == AXUIElementGetTypeID(), CFEqual(focused, axWindow) {
                focusedNativeWindowMatches.append(window)
            }
            let ownerWindowRoot = MenuRetainedToken(axWindow)
            actualAXWindowRoots.append(ownerWindowRoot)
            visit(axWindow, parent: nil, parentRole: nil, parentSubrole: nil,
                  window: window, depth: 0,
                  viewport: rect, group: nil, sheetRoot: nil,
                  ownerWindowRoot: ownerWindowRoot, parentElement: nil, siblingIndex: index)
        }
        if axWindows.isEmpty { mark("target has no readable AX windows") }

        let postAXWindows = boundedRootWindows(application) ?? []
        let postNativeWindows = targetWindows(pid: app.pid)
        rootIdentityVerified = sameRootFacts(rootFactsBefore, rootFacts(postAXWindows))
        let shouldVerifyFocusedWindow = request.table_scope != nil
            || (request.background_target == nil && frontmostPID == app.pid)
        let postFocusedWindow = shouldVerifyFocusedWindow
            ? attribute(application, kAXFocusedWindowAttribute) : nil
        let focusedRootMatchCount: Int
        if let focused, CFGetTypeID(focused) == AXUIElementGetTypeID() {
            focusedRootMatchCount = axWindows.filter { CFEqual($0, focused) }.count
        } else {
            focusedRootMatchCount = 0
        }
        let focusedNativeWindowRemained: Bool
        if let focusedWindow = focusedNativeWindowMatches.first {
            focusedNativeWindowRemained = postNativeWindows.filter {
                $0.id == focusedWindow.id && $0.pid == app.pid && $0.frame == focusedWindow.frame
            }.count == 1
        } else {
            focusedNativeWindowRemained = false
        }
        if request.background_target == nil, frontmostPID == app.pid,
           focusedRootMatchCount == 1, focusedNativeWindowMatches.count == 1,
           let focused, let postFocusedWindow,
           CFGetTypeID(focused) == AXUIElementGetTypeID(),
           CFGetTypeID(postFocusedWindow) == AXUIElementGetTypeID(),
           CFEqual(focused, postFocusedWindow), rootIdentityVerified, focusedNativeWindowRemained,
           let focusedWindow = focusedNativeWindowMatches.first {
            structure.global_focus = GlobalFocusObservation(
                app: .known(app), window_id: .known(focusedWindow.ref))
        }
        let tableScopeBoundaryStable: Bool
        var tableScopeFocusStable: Bool?
        var tableScopeProcessStable: Bool?
        var tableScopeWindowInventoryStable: Bool?
        if let pending = pendingTableScopeSearch {
            let focusStable: Bool
            if let before = focused, let after = postFocusedWindow,
               CFGetTypeID(before) == AXUIElementGetTypeID(),
               CFGetTypeID(after) == AXUIElementGetTypeID() {
                focusStable = CFEqual(before, after) && CFEqual(before, pending.window)
            } else {
                focusStable = false
            }
            let windowStillPresent = postAXWindows.filter { CFEqual($0, pending.window) }.count == 1
            let windowInventoryStable = nativeWindows == postNativeWindows
            let earlierBoundaryStable = frontmostPID == app.pid && focusStable && windowStillPresent
                && rootIdentityVerified && windowInventoryStable
            let processStable: Bool? = earlierBoundaryStable
                ? ProcessStamp.read(pid: app.pid) == pending.processStamp : nil
            tableScopeFocusStable = focusStable
            tableScopeProcessStable = processStable
            tableScopeWindowInventoryStable = windowInventoryStable
            tableScopeBoundaryStable = earlierBoundaryStable && processStable == true
        } else {
            tableScopeBoundaryStable = false
        }

        let menuDrawingFound = !nativeMenuDrawingWindows(pid: app.pid).isEmpty
        var supportedAuxiliaryDialog = false
        var provenAuxiliaryPrivacyAuditInput: RegisteredAuxiliaryPrivacyAuditInput?
        if registeredAuxiliaryProofScope {
            let postFocused = attribute(application, kAXFocusedWindowAttribute,
                                        required: axWindows.count > 1 || postAXWindows.count > 1
                                            || auxiliaryDialogVisitCount > 0)
            let auxiliaryRootsAfter = registeredWindowRootObservations(postAXWindows, nativeWindows: postNativeWindows)
            let candidateSeen = auxiliaryDialogVisitCount > 0
                || auxiliaryRootsBefore.contains(where: { $0.facts.role == kAXWindowRole && $0.facts.subrole == "AXDialog" })
                || auxiliaryRootsAfter.contains(where: { $0.facts.role == kAXWindowRole && $0.facts.subrole == "AXDialog" })
            if candidateSeen {
                let rootsStable = sameRegisteredAXWindowRoots(auxiliaryRootsBefore, auxiliaryRootsAfter)
                let focusedWindowIsStable: Bool
                if let focused, let postFocused,
                   CFGetTypeID(focused) == AXUIElementGetTypeID(),
                   CFGetTypeID(postFocused) == AXUIElementGetTypeID() {
                    focusedWindowIsStable = CFEqual(focused, postFocused)
                } else { focusedWindowIsStable = false }
                let proof = RegisteredAuxiliaryAXDialogProof(
                    bundleID: app.bundle_id, targetPID: app.pid, frontmost: frontmost,
                    processStable: processBefore != nil && processBefore == ProcessStamp.read(pid: app.pid),
                    focusStable: focusedWindowIsStable,
                    focusedParentMatchesBefore: focusedRegisteredStandardRoot(focused, roots: auxiliaryRootsBefore),
                    focusedParentMatchesAfter: focusedRegisteredStandardRoot(postFocused, roots: auxiliaryRootsAfter),
                    rootsStable: rootsStable,
                    cgInventoryStable: nativeWindows.count < 32 && postNativeWindows.count < 32
                        && nativeWindows == postNativeWindows,
                    auxiliaryDialogVisitedOnce: auxiliaryDialogVisitCount == 1,
                    beforeRoots: auxiliaryRootsBefore.map(\.facts), afterRoots: auxiliaryRootsAfter.map(\.facts))
                let metadataReadiness = registeredAuxiliaryAXDialogMetadataReadiness(
                    reasons: reasons, hasValidTableScope: request.table_scope?.valid == true,
                    hasPendingTableScopeSearch: pendingTableScopeSearch != nil,
                    tableScopeBoundaryStable: tableScopeBoundaryStable)
                supportedAuxiliaryDialog = metadataReadiness && !unsupportedModalFound
                    && modalSheetCount == 0 && !transientMenuFound && !menuDrawingFound
                    && canProjectRegisteredAuxiliaryAXDialog(proof)
                let standardRoots = auxiliaryRootsBefore.filter({
                    $0.facts.role == kAXWindowRole && $0.facts.subrole == kAXStandardWindowSubrole
                })
                let dialogRoots = auxiliaryRootsBefore.filter({
                    $0.facts.role == kAXWindowRole && $0.facts.subrole == "AXDialog"
                })
                if supportedAuxiliaryDialog,
                   standardRoots.count == 1, dialogRoots.count == 1,
                   let processBefore {
                    let standardRoot = standardRoots[0]
                    let dialogRoot = dialogRoots[0]
                    provenAuxiliaryPrivacyAuditInput = RegisteredAuxiliaryPrivacyAuditInput(
                        bundleID: app.bundle_id, processID: app.pid,
                        standardRoot: RegisteredAuxiliaryPrivacyAuditRoot(
                            token: standardRoot.element, facts: standardRoot.facts),
                        dialogRoot: RegisteredAuxiliaryPrivacyAuditRoot(
                            token: dialogRoot.element, facts: dialogRoot.facts),
                        processStamp: processBefore, cgInventory: nativeWindows)
                }
                if supportedAuxiliaryDialog && provenAuxiliaryPrivacyAuditInput == nil {
                    supportedAuxiliaryDialog = false
                }
                if !supportedAuxiliaryDialog {
                    modalFound = true
                    unsupportedModalFound = true
                    mark("registered auxiliary AXDialog proof is incomplete or changed")
                }
            }
        }
        for sheet in modalSheetWindows where !structure.windows.contains(where: { $0.window_id == sheet.window.ref }) {
            structure.windows.append(sheet.window.structureInfo(title: sheet.title, modal: true,
                ownerBundleID: app.bundle_id, kind: "sheet"))
        }
        structure.objects = objects
        structure.observer_coverage = ObserverCoverage.project(objects: objects,
            rootAvailable: false, failures: [])
        structure.table_viewports = observedTables.compactMap { projectTableViewport($0) }
        let menuProven: Bool
        if request.background_target == nil, let menuRegistry, let process = ProcessStamp.read(pid: app.pid) {
            menuProven = menuRegistry.project(reader: self, structure: &structure, process: process)
        } else { menuProven = false }
        auxiliaryPrivacyMenuProven = menuProven
        if supportedAuxiliaryDialog && menuProven {
            supportedAuxiliaryDialog = false
            provenAuxiliaryPrivacyAuditInput = nil
            unsupportedModalFound = true
            modalFound = true
            mark("registered auxiliary AXDialog cannot overlap a menu context")
        }
        let supportedFixtureSheet = reasons.isEmpty && app.bundle_id == mvpFixtureBundleID
            && frontmost && axWindowRootCount == 1 && modalSheetCount == 1
            && modalSheetWindows.count == 1 && !unsupportedModalFound
            && !transientMenuFound && !menuDrawingFound && !menuProven
            && structure.windows.filter({ $0.window_id == modalSheetWindows[0].window.ref }).count == 1
        if supportedFixtureSheet {
            structure.active_layer = modalSheetWindows[0].window.ref
        } else if supportedAuxiliaryDialog {
            // Keep the proved focused standard root active. The auxiliary
            // AXDialog is separate window metadata and never becomes a layer.
        } else if modalFound {
            structure.active_layer = nil; mark("modal or transient active layer is unresolved")
        }
        if (transientMenuFound || menuDrawingFound) && !menuProven {
            structure.active_layer = nil; mark("modal or transient active layer is unresolved")
        }
        structure.complete = reasons.isEmpty
        if !structure.complete { structure.menu_anchors = []; structure.menu_context = nil }
        structure.incomplete_reason = reasons.isEmpty ? nil : reasons.joined(separator: "; ")
        let scopeWindowRemainedActive = pendingTableScopeSearch.map {
            structure.active_layer == $0.windowID
        } ?? false
        let scopeContextStable = scopedTableMetadataBoundaryStable(
            tableScopeBoundaryStable: tableScopeBoundaryStable,
            scopeWindowRemainedActive: scopeWindowRemainedActive,
            modalFound: modalFound, supportedAuxiliaryDialog: supportedAuxiliaryDialog,
            unsupportedModalFound: unsupportedModalFound,
            transientMenuFound: transientMenuFound, menuDrawingFound: menuDrawingFound)
        auxiliaryPrivacyAuditInput = supportedAuxiliaryDialog && scopeContextStable
            ? provenAuxiliaryPrivacyAuditInput : nil
        structure.table_scope_search = finalizeTableScopeSearch(pendingTableScopeSearch,
            structure: structure, boundaryStable: scopeContextStable)
        var visiblePrivacy: VisiblePrivacyCoverage?
        var visibleCoverageRejected = false
        if !structure.complete, let pending = pendingTableScopeSearch {
            switch captureVisiblePrivacyCoverage(structure: structure, pending: pending,
                application: application, preReadWindows: axWindows, postReadWindows: postAXWindows,
                scopeBoundaryStable: scopeContextStable, menuDrawingFound: menuDrawingFound,
                menuProven: menuProven) {
            case .success(let coverage):
                visiblePrivacy = coverage
            case .failure(let failure):
                visibleCoverageRejected = true
                mark("visible privacy coverage unavailable: \(failure.reason)")
                structure.complete = false
                structure.incomplete_reason = reasons.joined(separator: "; ")
            }
        }
        let privacyRejection: ReadPrivacyRejection?
        if !frontmost {
            privacyRejection = .notFrontmost
        } else if tableScopeProcessStable == false {
            privacyRejection = .processUnavailable
        } else if !rootIdentityVerified {
            privacyRejection = .rootContinuityChanged
        } else if tableScopeFocusStable == false {
            privacyRejection = .focusedWindowChanged
        } else if unsupportedModalFound
                    || (request.table_scope != nil && (modalFound || transientMenuFound || menuDrawingFound))
                    || (modalFound && !supportedFixtureSheet && !supportedAuxiliaryDialog)
                    || ((transientMenuFound || menuDrawingFound) && !menuProven) {
            privacyRejection = .modalOrMenuUnresolved
        } else if request.table_scope != nil
                    && (!tableScopeBoundaryStable || !scopeWindowRemainedActive
                        || tableScopeWindowInventoryStable == false) {
            privacyRejection = .scopeBoundaryChanged
        } else if visibleCoverageRejected {
            privacyRejection = .visibleCoverageRejected
        } else if !structure.complete && visiblePrivacy == nil {
            privacyRejection = .readUnavailable
        } else {
            privacyRejection = nil
        }
        return StructureRead(structure: structure, started: started, finished: uptimeNs(),
                             secureFrames: secureFrames, visiblePrivacy: visiblePrivacy,
                             privacyRejection: privacyRejection)
    }
}
