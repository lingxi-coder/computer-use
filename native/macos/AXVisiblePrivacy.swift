import ApplicationServices
import CoreGraphics
import Foundation

enum VisiblePrivacyChildrenSource: String, Sendable, Equatable {
    case visibleChildren = "ax_visible_children"
    case childrenFallback = "ax_children_complete_fallback"
    case leafChildrenUnsupported = "leaf_ax_children_unsupported"
    case emptyGroupAXChildrenSuccessCountZero = "empty_group_ax_children_success_count_zero"
    /// A root-owned AXGroup whose complete AXChildren list is separately audited.
    case windowOwnedGroupAXChildren = "window_owned_group_ax_children_complete"
    case unknown
    case noValue
    case truncated
    case failed
}

struct VisiblePrivacyNodeFact: Sendable, Equatable {
    let parentIndex: Int?
    /// Nil means the ordinary expected parent. A root-owned AXGroup records its
    /// verified AXWindow parent separately from the returned visible-child slot.
    let actualParentIndex: Int?
    let childIndex: Int?
    let role: String
    let subrole: String?
    let frame: Rect
    let childrenSource: VisiblePrivacyChildrenSource
    let childCount: Int

    init(parentIndex: Int?, childIndex: Int?, role: String, subrole: String?, frame: Rect,
         childrenSource: VisiblePrivacyChildrenSource, childCount: Int,
         actualParentIndex: Int? = nil) {
        self.parentIndex = parentIndex
        self.actualParentIndex = actualParentIndex
        self.childIndex = childIndex
        self.role = role
        self.subrole = subrole
        self.frame = frame
        self.childrenSource = childrenSource
        self.childCount = childCount
    }
}

enum VisiblePrivacyAXChildrenCountRead: Sendable, Equatable {
    case success(Int)
    case unsupported
    case noValue
    case failed
    case late
}

struct VisiblePrivacyGroupParentMismatchCandidate: Sendable, Equatable {
    let expectedParentIndex: Int?
    let expectedParentRole: String?
    let expectedParentWasAudited: Bool
    let expectedChildIndex: Int?
    let expectedParentChildrenSource: VisiblePrivacyChildrenSource
    let actualParentIndex: Int?
    let actualParentMatchesCapturedWindow: Bool
    let processID: UInt32
    let role: String?
    let subrole: String?
    let frame: Rect?
    let roleTypeValid: Bool
    let subroleTypeValid: Bool
    let frameTypeValid: Bool
    let modal: Bool?
    let sensitive: Bool
    let forbiddenLayer: Bool
    let axChildrenCountRead: VisiblePrivacyAXChildrenCountRead
    let copiedAXChildrenCount: Int?
    let axChildrenCopyComplete: Bool
}

private struct VisiblePrivacyWindowOwnedGroupAudit {
    let observationIndex: Int
    let element: AXUIElement
    let children: [AXUIElement]
}

private struct VisiblePrivacyChildListAudit {
    let observationIndex: Int
    let element: AXUIElement
    let rootKind: VisiblePrivacyRootKind
    let source: VisiblePrivacyChildrenSource
    let children: [AXUIElement]
}

enum VisiblePrivacyRootKind: Sendable, Equatable {
    case standardWindow
    case registeredAuxiliaryDialog
}

/// Content-only AX projection for the exact auxiliary root. Read-local refs
/// are deliberately omitted; tree identity/order is covered separately.
struct VisiblePrivacyAuxiliaryContentSignature: Sendable, Equatable {
    let role: String
    let subrole: String?
    let frame: Rect
    let title: String?
    let description: String?
    let value: UiValue?
    let enabled: Bool
    let focused: Bool
    let actions: [String]
    let inView: Bool
    let group: String?
    let secure: Bool
    let scroll: ScrollState?

    init(_ object: UiObject) {
        role = object.role
        subrole = object.subrole
        frame = object.frame
        title = object.title
        description = object.description
        value = object.value
        enabled = object.enabled
        focused = object.focused
        actions = object.actions
        inView = object.in_view
        group = object.group
        secure = object.secure
        scroll = object.scroll
    }
}

/// Private, same-read audit result for one registered auxiliary AXDialog.
/// It is an Equatable fingerprint, not a new active/capture/input layer.
struct VisiblePrivacyAuxiliaryFingerprint: Sendable, Equatable {
    let windowID: String
    let processID: UInt32
    let cgWindowID: UInt32
    let frame: Rect
    let role: String
    let subrole: String?
    let modal: Bool
    let focused: Bool
    let treeFingerprint: [VisiblePrivacyNodeFact]
    let contentSignatures: [VisiblePrivacyAuxiliaryContentSignature]
}

struct VisiblePrivacyCoverage: Sendable, Equatable {
    let bundleID: String
    let processID: UInt32
    let processStamp: ProcessStamp
    let windowID: String
    let windowFrame: Rect
    let tableScope: TableScopeSearchRequest
    let readStartedNS: UInt64
    let readFinishedNS: UInt64
    let visibleNodeCount: UInt32
    let visibleTreeFingerprint: [VisiblePrivacyNodeFact]
    let tableViewport: VisiblePrivacyTableViewport
    var auxiliaryWindows: [VisiblePrivacyAuxiliaryFingerprint] = []
}

/// Cross-read table state without read-local AX identity references. Array order
/// is the observed AX order; every displayed value and selector-relevant fact is kept.
struct VisiblePrivacyTableViewport: Sendable, Equatable {
    struct Row: Sendable, Equatable {
        let frame: Rect
        let axIndex: UInt32?
        let accessibilityIdentifier: String?
        let axEnabled: AxEnabledObservation
        let selected: Bool?
        let selectedSettable: Bool?
        let cellsComplete: Bool
        let cells: [Cell]
    }
    struct Cell: Sendable, Equatable {
        let frame: Rect
        let rowAXIndex: UInt32?
        let columnAXIndex: UInt32?
        let columnHeader: String?
        let value: UiValue?
    }
    struct Column: Sendable, Equatable {
        let frame: Rect
        let axIndex: Int?
        let header: String?
    }
    let tableFrame: Rect
    let viewportFrame: Rect
    let axEnabled: AxEnabledObservation
    let visibleRowsComplete: Bool
    let rows: [Row]
    let columnsComplete: Bool
    let columns: [Column]

    init(viewport: TableViewport) {
        tableFrame = viewport.table_frame
        viewportFrame = viewport.viewport_frame
        axEnabled = viewport.ax_enabled
        visibleRowsComplete = viewport.visible_rows_complete
        rows = viewport.visible_rows.map { row in
            Row(frame: row.frame, axIndex: row.ax_index,
                accessibilityIdentifier: row.accessibility_identifier,
                axEnabled: row.ax_enabled, selected: row.selected,
                selectedSettable: row.ax_selected_settable, cellsComplete: row.cells_complete,
                cells: row.cells.map { cell in
                    Cell(frame: cell.frame, rowAXIndex: cell.row_ax_index,
                        columnAXIndex: cell.column_ax_index, columnHeader: cell.column_header,
                        value: cell.value)
                })
        }
        columnsComplete = viewport.columns_complete
        columns = viewport.columns.map { column in
            Column(frame: column.frame, axIndex: column.ax_index, header: column.header)
        }
    }
}

/// Reader-local facts supplied to the pure validator. `identityOrdinal` is an
/// index assigned only after CFEqual checks; it is never serialized as identity.
struct VisiblePrivacyAuditObservation: Sendable, Equatable {
    let identityOrdinal: Int
    let parentIdentityOrdinal: Int?
    let processID: UInt32
    let actualParentMatches: Bool
    let roleTypeValid: Bool
    let subroleTypeValid: Bool
    let frameTypeValid: Bool
    let modalTypeValid: Bool
    let sensitive: Bool
    let forbiddenLayer: Bool
    let modal: Bool?
    let fact: VisiblePrivacyNodeFact
}

enum VisiblePrivacyGraph {
    private static let allowedRoles: Set<String> = [
        "AXWindow", "AXGroup", "AXScrollArea", "AXScrollBar", "AXSplitGroup", "AXTable", "AXRow", "AXCell", "AXColumn",
        "AXColumnHeader", "AXHeader", "AXStaticText", "AXButton", "AXCheckBox", "AXRadioButton",
        "AXTextField", "AXTextArea", "AXComboBox", "AXPopUpButton", "AXMenuButton", "AXMenuItem",
        "AXSlider", "AXSwitch", "AXValueIndicator", "AXImage", "AXLink", "AXTab", "AXTabGroup", "AXList", "AXOutline",
        "AXDisclosureTriangle", "AXProgressIndicator", "AXToolbar", "AXWebArea", "AXHeading", "AXSplitter",
    ]
    private static let blockedRoles: Set<String> = [
        "AXMenu", "AXMenuBar", "AXMenuItem", "AXSheet", "AXDialog", "AXPopover", "AXApplication",
    ]
    private static let allowedSubroles: Set<String> = [
        "AXCloseButton", "AXMinimizeButton", "AXZoomButton", "AXToolbarButton", "AXHelpButton",
        "AXPushButton", "AXDisclosureTriangle", "AXIncrementArrow", "AXDecrementArrow", "AXSortButton",
    ]
    /// Newly observed standard subroles are accepted only on their documented
    /// parent role; a familiar subrole on an unrelated/custom role stays unknown.
    private static let pairedSubroleRoles: [String: String] = [
        "AXTableRow": "AXRow",
        "AXFullScreenButton": "AXButton",
        "AXIncrementPage": "AXButton",
        "AXDecrementPage": "AXButton",
    ]
    /// These standard control roles are documented leaves when AXChildren is
    /// unsupported. Containers and custom roles never inherit a leaf default.
    private static let leafRoles: Set<String> = [
        "AXButton", "AXCheckBox", "AXRadioButton", "AXStaticText", "AXSlider", "AXSwitch",
        "AXImage", "AXLink", "AXProgressIndicator", "AXDisclosureTriangle",
    ]

    static func fingerprint(
        observations: [VisiblePrivacyAuditObservation],
        expectedProcessID: UInt32,
        windowFrame: Rect,
        nodeLimit: Int,
        rootKind: VisiblePrivacyRootKind = .standardWindow
    ) -> [VisiblePrivacyNodeFact]? {
        guard expectedProcessID > 0, nodeLimit > 0, nodeLimit <= 512,
              !observations.isEmpty, observations.count <= nodeLimit,
              validFrame(windowFrame) else { return nil }

        var identities = Set<Int>()
        var directChildren = Array(repeating: 0, count: observations.count)
        for (index, observed) in observations.enumerated() {
            let fact = observed.fact
            let expectedParentIndex = fact.parentIndex
            let actualParentIndex = fact.actualParentIndex ?? expectedParentIndex
            let parentRole = fact.parentIndex.flatMap { parentIndex in
                observations.indices.contains(parentIndex) ? observations[parentIndex].fact.role : nil
            }
            let emptyGroupParentMismatch = isEmptyGroupParentMismatchFact(
                fact, observation: observed, nodeIndex: index, observations: observations)
            let windowOwnedGroupParentMismatch = isWindowOwnedGroupParentMismatchFact(
                fact, observation: observed, nodeIndex: index, observations: observations)
            let specialGroupParentMismatch = emptyGroupParentMismatch || windowOwnedGroupParentMismatch
            let emptyScrollBarArrowLeaf = isEmptyScrollBarArrowLeaf(
                role: fact.role, subrole: fact.subrole, parentRole: parentRole,
                frame: fact.frame, childrenSource: fact.childrenSource, childCount: fact.childCount)
            guard identities.insert(observed.identityOrdinal).inserted,
                  observed.processID == expectedProcessID,
                  observed.roleTypeValid, observed.subroleTypeValid, observed.frameTypeValid,
                  observed.modalTypeValid,
                  (observed.actualParentMatches || specialGroupParentMismatch),
                  !observed.sensitive, !observed.forbiddenLayer, observed.modal != true,
                  allowedRoles.contains(fact.role), !blockedRoles.contains(fact.role),
                  !isSensitiveField(role: fact.role, subrole: fact.subrole),
                  subroleIsSafe(fact.subrole, root: index == 0, role: fact.role,
                                rootKind: rootKind),
                  validNodeFrame(fact.frame, allowEmptyScrollBarArrowLeaf: emptyScrollBarArrowLeaf),
                  (fact.childrenSource != .windowOwnedGroupAXChildren
                      || windowOwnedGroupParentMismatch),
                  fact.childCount >= 0,
                  fact.childCount <= nodeLimit,
                  (sourceIsComplete(fact.childrenSource, role: fact.role, childCount: fact.childCount)
                      || specialGroupParentMismatch)
            else { return nil }

            if index == 0 {
                guard fact.parentIndex == nil, fact.actualParentIndex == nil, fact.childIndex == nil,
                      observed.parentIdentityOrdinal == nil,
                      fact.role == "AXWindow",
                      (rootKind == .standardWindow && fact.subrole == "AXStandardWindow"
                          || rootKind == .registeredAuxiliaryDialog && fact.subrole == "AXDialog"),
                      fact.frame == windowFrame,
                      fact.childrenSource != .leafChildrenUnsupported,
                      observed.modal == false else { return nil }
            } else {
                guard fact.role != "AXWindow",
                      let parentIndex = expectedParentIndex, parentIndex >= 0, parentIndex < index,
                      let actualParentIndex, actualParentIndex >= 0, actualParentIndex < index,
                      let childIndex = fact.childIndex, childIndex >= 0,
                      observed.parentIdentityOrdinal == observations[actualParentIndex].identityOrdinal,
                      (observed.actualParentMatches && actualParentIndex == parentIndex
                          || specialGroupParentMismatch),
                      childIndex == directChildren[parentIndex] else { return nil }
                directChildren[parentIndex] += 1
                if observations[parentIndex].fact.childrenSource == .visibleChildren
                    || observations[parentIndex].fact.childrenSource == .windowOwnedGroupAXChildren,
                   !emptyScrollBarArrowLeaf, !fact.frame.intersects(windowFrame) { return nil }
            }
        }
        for (index, observed) in observations.enumerated()
        where directChildren[index] != observed.fact.childCount {
            return nil
        }
        return observations.map(\.fact)
    }

    static func validFingerprint(_ facts: [VisiblePrivacyNodeFact], windowFrame: Rect,
                                 nodeLimit: Int = 512,
                                 rootKind: VisiblePrivacyRootKind = .standardWindow) -> Bool {
        let observations = facts.enumerated().map { index, fact in
            let actualParentIndex = fact.actualParentIndex ?? fact.parentIndex
            return VisiblePrivacyAuditObservation(
                identityOrdinal: index,
                parentIdentityOrdinal: actualParentIndex,
                processID: 1,
                actualParentMatches: actualParentIndex == fact.parentIndex,
                roleTypeValid: true,
                subroleTypeValid: true,
                frameTypeValid: true,
                modalTypeValid: true,
                sensitive: false,
                forbiddenLayer: false,
                modal: index == 0 ? false : nil,
                fact: fact)
        }
        return fingerprint(observations: observations, expectedProcessID: 1,
                           windowFrame: windowFrame, nodeLimit: nodeLimit,
                           rootKind: rootKind) != nil
    }

    static func validAuxiliaryFingerprint(_ auxiliary: VisiblePrivacyAuxiliaryFingerprint,
                                          expectedProcessID: UInt32,
                                          nodeLimit: Int = 512) -> Bool {
        guard auxiliary.processID == expectedProcessID, expectedProcessID > 0,
              auxiliary.cgWindowID > 0,
              auxiliary.windowID == "window-\(auxiliary.cgWindowID)",
              auxiliary.role == "AXWindow", auxiliary.subrole == "AXDialog",
              auxiliary.modal == false, auxiliary.focused == false,
              auxiliary.frame.valid, auxiliary.frame.width > 0, auxiliary.frame.height > 0,
              !auxiliary.treeFingerprint.isEmpty,
              validFingerprint(auxiliary.treeFingerprint, windowFrame: auxiliary.frame,
                  nodeLimit: nodeLimit, rootKind: .registeredAuxiliaryDialog),
              !auxiliary.contentSignatures.isEmpty,
              auxiliary.contentSignatures[0].role == "AXWindow",
              auxiliary.contentSignatures[0].subrole == "AXDialog",
              auxiliary.contentSignatures[0].frame == auxiliary.frame,
              auxiliary.contentSignatures.filter({ $0.role == "AXWindow" }).count == 1,
              auxiliary.contentSignatures.allSatisfy({
                  !$0.secure && !isSensitiveField(role: $0.role, subrole: $0.subrole)
              }) else { return false }
        var unmatched = Array(auxiliary.contentSignatures.dropFirst())
        for fact in auxiliary.treeFingerprint.dropFirst() {
            guard let index = unmatched.firstIndex(where: {
                $0.role == fact.role && $0.subrole == fact.subrole && $0.frame == fact.frame
            }) else { return false }
            unmatched.remove(at: index)
        }
        return true
    }

    static func fitsSharedNodeLimit(_ treeNodeCounts: [Int], nodeLimit: Int) -> Bool {
        guard nodeLimit > 0, nodeLimit <= 512, !treeNodeCounts.isEmpty,
              treeNodeCounts.allSatisfy({ $0 > 0 }) else { return false }
        return treeNodeCounts.reduce(0, +) <= nodeLimit
    }

    static func identityOrdinalsAreDisjoint(_ trees: [[VisiblePrivacyAuditObservation]]) -> Bool {
        var seen = Set<Int>()
        for tree in trees {
            for observation in tree {
                guard seen.insert(observation.identityOrdinal).inserted else { return false }
            }
        }
        return true
    }

    static func childListPostcheckMatches(expectedSource: VisiblePrivacyChildrenSource,
                                          currentSource: VisiblePrivacyChildrenSource,
                                          expectedCount: Int, currentCount: Int,
                                          referencesMatchInOrder: [Bool]) -> Bool {
        guard expectedSource == currentSource, expectedCount >= 0,
              expectedCount == currentCount, referencesMatchInOrder.count == expectedCount,
              referencesMatchInOrder.allSatisfy({ $0 }) else { return false }
        switch expectedSource {
        case .visibleChildren, .childrenFallback:
            return true
        case .leafChildrenUnsupported:
            return expectedCount == 0
        case .emptyGroupAXChildrenSuccessCountZero, .windowOwnedGroupAXChildren,
             .unknown, .noValue, .truncated, .failed:
            return false
        }
    }

    private static func sourceIsComplete(_ source: VisiblePrivacyChildrenSource,
                                         role: String, childCount: Int) -> Bool {
        switch source {
        case .visibleChildren, .childrenFallback:
            return true
        case .leafChildrenUnsupported:
            return childCount == 0 && leafRoles.contains(role)
        case .emptyGroupAXChildrenSuccessCountZero:
            return false
        case .windowOwnedGroupAXChildren:
            return role == "AXGroup" && childCount > 0
        case .unknown, .noValue, .truncated, .failed:
            return false
        }
    }

    static func classifyEmptyGroupParentMismatch(
        _ candidate: VisiblePrivacyGroupParentMismatchCandidate,
        expectedProcessID: UInt32
    ) -> VisiblePrivacyNodeFact? {
        guard let fields = validGroupParentMismatchFields(candidate, expectedProcessID: expectedProcessID),
              case .success(let count) = candidate.axChildrenCountRead, count == 0 else { return nil }
        return VisiblePrivacyNodeFact(
            parentIndex: fields.parentIndex, childIndex: fields.childIndex,
            role: fields.role, subrole: candidate.subrole, frame: fields.frame,
            childrenSource: .emptyGroupAXChildrenSuccessCountZero, childCount: 0,
            actualParentIndex: 0)
    }

    static func classifyWindowOwnedGroupParentMismatch(
        _ candidate: VisiblePrivacyGroupParentMismatchCandidate,
        expectedProcessID: UInt32
    ) -> VisiblePrivacyNodeFact? {
        guard let fields = validGroupParentMismatchFields(candidate, expectedProcessID: expectedProcessID),
              case .success(let count) = candidate.axChildrenCountRead,
              count > 0, count <= 512,
              candidate.copiedAXChildrenCount == count,
              candidate.axChildrenCopyComplete else { return nil }
        return VisiblePrivacyNodeFact(
            parentIndex: fields.parentIndex, childIndex: fields.childIndex,
            role: fields.role, subrole: candidate.subrole, frame: fields.frame,
            childrenSource: .windowOwnedGroupAXChildren, childCount: count,
            actualParentIndex: 0)
    }

    static func windowOwnedGroupAXChildrenPostcheckMatches(
        expectedCount: Int,
        countRead: VisiblePrivacyAXChildrenCountRead,
        copiedCount: Int?,
        referencesMatchInOrder: [Bool]
    ) -> Bool {
        guard expectedCount > 0,
              case .success(let count) = countRead,
              count == expectedCount,
              copiedCount == expectedCount,
              referencesMatchInOrder.count == expectedCount else { return false }
        return referencesMatchInOrder.allSatisfy { $0 }
    }

    private static func validGroupParentMismatchFields(
        _ candidate: VisiblePrivacyGroupParentMismatchCandidate,
        expectedProcessID: UInt32
    ) -> (parentIndex: Int, childIndex: Int, role: String, frame: Rect)? {
        guard expectedProcessID > 0,
              candidate.processID == expectedProcessID,
              let expectedParentIndex = candidate.expectedParentIndex,
              expectedParentIndex > 0,
              candidate.expectedParentWasAudited,
              candidate.expectedParentRole == "AXButton",
              let childIndex = candidate.expectedChildIndex, childIndex >= 0,
              candidate.expectedParentChildrenSource == .visibleChildren
                  || candidate.expectedParentChildrenSource == .childrenFallback,
              candidate.actualParentIndex == 0,
              candidate.actualParentMatchesCapturedWindow,
              candidate.roleTypeValid, candidate.subroleTypeValid, candidate.frameTypeValid,
              let role = candidate.role, role == "AXGroup",
              candidate.subrole == nil || candidate.subrole == "",
              allowedRoles.contains(role), !blockedRoles.contains(role),
              let frame = candidate.frame, validFrame(frame),
              !candidate.sensitive, !candidate.forbiddenLayer, candidate.modal != true,
              !isSensitiveField(role: role, subrole: candidate.subrole),
              !["AXDialog", "AXPopover", "AXSheet", "AXSecureTextField"].contains(candidate.subrole ?? ""),
              subroleIsSafe(candidate.subrole, root: false, role: role) else { return nil }
        return (expectedParentIndex, childIndex, role, frame)
    }

    private static func isEmptyGroupParentMismatchFact(
        _ fact: VisiblePrivacyNodeFact,
        observation: VisiblePrivacyAuditObservation,
        nodeIndex: Int,
        observations: [VisiblePrivacyAuditObservation]
    ) -> Bool {
        guard fact.childrenSource == .emptyGroupAXChildrenSuccessCountZero,
              fact.childCount == 0, fact.role == "AXGroup",
              let expectedParentIndex = fact.parentIndex,
              expectedParentIndex > 0, expectedParentIndex < nodeIndex,
              fact.actualParentIndex == 0,
              !observation.actualParentMatches,
              observations.indices.contains(0),
              observation.parentIdentityOrdinal == observations[0].identityOrdinal,
              observations[0].fact.role == "AXWindow",
              observations[0].fact.subrole == "AXStandardWindow",
              observations[0].fact.parentIndex == nil,
              observations[expectedParentIndex].fact.role == "AXButton",
              fact.subrole == nil || fact.subrole == "",
              observations[expectedParentIndex].fact.childrenSource == .visibleChildren
                  || observations[expectedParentIndex].fact.childrenSource == .childrenFallback,
              fact.childIndex.map({ $0 >= 0 }) == true else { return false }
        return true
    }

    private static func isWindowOwnedGroupParentMismatchFact(
        _ fact: VisiblePrivacyNodeFact,
        observation: VisiblePrivacyAuditObservation,
        nodeIndex: Int,
        observations: [VisiblePrivacyAuditObservation]
    ) -> Bool {
        guard fact.childrenSource == .windowOwnedGroupAXChildren,
              fact.childCount > 0, fact.role == "AXGroup",
              let expectedParentIndex = fact.parentIndex,
              expectedParentIndex > 0, expectedParentIndex < nodeIndex,
              fact.actualParentIndex == 0,
              !observation.actualParentMatches,
              observations.indices.contains(0),
              observation.parentIdentityOrdinal == observations[0].identityOrdinal,
              observations[0].fact.role == "AXWindow",
              observations[0].fact.subrole == "AXStandardWindow",
              observations[0].fact.parentIndex == nil,
              observations[expectedParentIndex].fact.role == "AXButton",
              fact.subrole == nil || fact.subrole == "",
              observations[expectedParentIndex].fact.childrenSource == .visibleChildren
                  || observations[expectedParentIndex].fact.childrenSource == .childrenFallback,
              fact.childIndex.map({ $0 >= 0 }) == true else { return false }
        return true
    }

    static func isEmptyScrollBarArrowLeaf(
        role: String,
        subrole: String?,
        parentRole: String?,
        frame: Rect,
        childrenSource: VisiblePrivacyChildrenSource,
        childCount: Int
    ) -> Bool {
        // AppKit may expose an undrawn scrollbar arrow as a real, finite 0x0
        // AXButton. Its audited scrollbar parent and complete empty child list
        // establish an empty leaf; the frame is still retained verbatim.
        role == "AXButton"
            && ["AXIncrementArrow", "AXDecrementArrow"].contains(subrole ?? "")
            && parentRole == "AXScrollBar"
            && validZeroSizeFrame(frame)
            && childCount == 0
            && sourceIsComplete(childrenSource, role: role, childCount: childCount)
    }

    static func validNodeFrame(_ frame: Rect, allowEmptyScrollBarArrowLeaf: Bool) -> Bool {
        validFrame(frame) || (allowEmptyScrollBarArrowLeaf && validZeroSizeFrame(frame))
    }

    private static func subroleIsSafe(_ subrole: String?, root: Bool, role: String,
                                      rootKind: VisiblePrivacyRootKind = .standardWindow) -> Bool {
        guard let subrole, !subrole.isEmpty else { return !root }
        if root {
            switch rootKind {
            case .standardWindow:
                return role == "AXWindow" && subrole == "AXStandardWindow"
            case .registeredAuxiliaryDialog:
                return role == "AXWindow" && subrole == "AXDialog"
            }
        }
        if let expectedRole = pairedSubroleRoles[subrole] {
            return role == expectedRole
        }
        return allowedSubroles.contains(subrole)
            && subrole != "AXSecureTextField"
            && subrole != "AXDialog"
            && subrole != "AXPopover"
            && subrole != "AXSheet"
    }

    private static func validFrame(_ frame: Rect) -> Bool {
        frame.valid && frame.width > 0 && frame.height > 0
            && [frame.x, frame.y, frame.width, frame.height].allSatisfy { abs($0) <= 100_000 }
    }

    private static func validZeroSizeFrame(_ frame: Rect) -> Bool {
        frame.valid && frame.width == 0 && frame.height == 0
            && [frame.x, frame.y, frame.width, frame.height].allSatisfy { abs($0) <= 100_000 }
    }
}

struct VisiblePrivacyFailure: Error {
    let reason: String
}

private struct VisiblePrivacyChildren {
    let elements: [AXUIElement]
    let source: VisiblePrivacyChildrenSource
}

private func visiblePrivacyNativeWindowInventoryIsClear(pid: UInt32, targetWindowID: UInt32,
                                                        frame: Rect, deadline: UInt64,
                                                        auditedAuxiliaryWindow: NativeWindow? = nil) -> Bool {
    guard uptimeNs() < deadline,
          let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]], uptimeNs() < deadline else { return false }
    let owned = list.filter {
        ($0[kCGWindowOwnerPID as String] as? NSNumber)?.uint32Value == pid
    }
    guard !owned.isEmpty, owned.count <= 32 else { return false }
    var targetMatches = 0
    var auxiliaryMatches = 0
    for info in owned {
        guard uptimeNs() < deadline,
              let id = info[kCGWindowNumber as String] as? NSNumber,
              let layer = info[kCGWindowLayer as String] as? NSNumber,
              let alpha = info[kCGWindowAlpha as String] as? NSNumber,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
        let observedFrame = Rect(cgFrame)
        guard observedFrame.valid, observedFrame.width > 0, observedFrame.height > 0,
              uptimeNs() < deadline else { return false }
        if id.uint32Value == targetWindowID, layer.intValue == 0, observedFrame == frame {
            targetMatches += 1
        } else if let auditedAuxiliaryWindow,
                  id.uint32Value == auditedAuxiliaryWindow.id {
            guard layer.intValue == 0, observedFrame == auditedAuxiliaryWindow.frame,
                  alpha.doubleValue > 0 else { return false }
            auxiliaryMatches += 1
        } else if alpha.doubleValue > 0, observedFrame.intersects(frame) {
            // Only the exact independently audited AXDialog root can overlap
            // the capture surface. Other same-process windows remain unknown.
            return false
        }
    }
    let expectedAuxiliaryMatches = auditedAuxiliaryWindow == nil ? 0 : 1
    return targetMatches == 1 && auxiliaryMatches == expectedAuxiliaryMatches
        && uptimeNs() < deadline
}

private func auxiliaryPrivacyAuditRootAppearsOnce(_ token: MenuRetainedToken,
                                                   in roots: [AXUIElement]) -> Bool {
    roots.filter { token.matches($0) }.count == 1
}

extension AXReader {
    func captureVisiblePrivacyCoverage(
        structure: Structure,
        pending: PendingTableScopeSearch,
        application: AXUIElement,
        preReadWindows: [AXUIElement],
        postReadWindows: [AXUIElement],
        scopeBoundaryStable: Bool,
        menuDrawingFound: Bool,
        menuProven: Bool
    ) -> Result<VisiblePrivacyCoverage, VisiblePrivacyFailure> {
        do {
            let coverage = try makeVisiblePrivacyCoverage(
                structure: structure, pending: pending, application: application,
                preReadWindows: preReadWindows, postReadWindows: postReadWindows,
                scopeBoundaryStable: scopeBoundaryStable,
                menuDrawingFound: menuDrawingFound, menuProven: menuProven)
            return .success(coverage)
        } catch let failure as VisiblePrivacyFailure {
            return .failure(failure)
        } catch {
            return .failure(VisiblePrivacyFailure(reason: "unexpected_reader_failure"))
        }
    }

    private func makeVisiblePrivacyCoverage(
        structure: Structure,
        pending: PendingTableScopeSearch,
        application: AXUIElement,
        preReadWindows: [AXUIElement],
        postReadWindows: [AXUIElement],
        scopeBoundaryStable: Bool,
        menuDrawingFound: Bool,
        menuProven: Bool
    ) throws -> VisiblePrivacyCoverage {
        guard let requested = request.table_scope, requested == pending.request,
              let result = structure.table_scope_search,
              result.requested_scope == requested,
              result.process_id == app.pid, result.window_id == pending.windowID,
              case .complete = result.state, result.candidates.count == 1 else {
            throw VisiblePrivacyFailure(reason: "scope_census_not_complete_unique")
        }
        let auxiliaryInput = auxiliaryPrivacyAuditInput
        let auxiliaryRoots = auxiliaryInput.flatMap { auxiliaryPrivacyAuditRootElements($0) }
        let hasAuxiliaryInput = auxiliaryInput.map { _ in true } ?? false
        let allowedAuxiliaryCGID = auxiliaryInput?.dialogRoot.facts.exactCGMatches.only?.id
        if let auxiliaryInput {
            guard let auxiliaryRoots,
                  auxiliaryInput.bundleID == app.bundle_id,
                  auxiliaryInput.processID == app.pid,
                  auxiliaryInput.processStamp == pending.processStamp,
                  auxiliaryInput.cgInventory == nativeWindows,
                  auxiliaryPrivacyAuditRootAppearsOnce(auxiliaryInput.standardRoot.token,
                                                       in: preReadWindows),
                  auxiliaryPrivacyAuditRootAppearsOnce(auxiliaryInput.dialogRoot.token,
                                                       in: preReadWindows),
                  auxiliaryPrivacyAuditRootAppearsOnce(auxiliaryInput.standardRoot.token,
                                                       in: postReadWindows),
                  auxiliaryPrivacyAuditRootAppearsOnce(auxiliaryInput.dialogRoot.token,
                                                       in: postReadWindows),
                  revalidateAuxiliaryPrivacyAuditInput(auxiliaryInput, application: application,
                                                       expectedStandard: auxiliaryRoots.standard),
                  CFEqual(auxiliaryRoots.standard, pending.window) else {
                throw VisiblePrivacyFailure(reason: "auxiliary_window_input_unverified")
            }
        }
        guard !structure.complete,
              structure.incomplete_reason == scopedTableRowsOmittedReason,
              structure.active_layer == pending.windowID,
              structure.app.pid == app.pid, pending.processID == app.pid,
              scopeBoundaryStable, rootIdentityVerified, frontmost,
              axWindowRootCount == (hasAuxiliaryInput ? 2 : 1),
              preReadWindows.count == (hasAuxiliaryInput ? 2 : 1),
              postReadWindows.count == (hasAuxiliaryInput ? 2 : 1),
              preReadWindows.contains(where: { CFEqual($0, pending.window) }),
              postReadWindows.contains(where: { CFEqual($0, pending.window) }),
              actualAXWindowRoots.count == (hasAuxiliaryInput ? 2 : 1),
              actualAXWindowRoots.filter({ $0.matches(pending.window) }).count == 1,
              auxiliaryRoots.map({ roots in
                  actualAXWindowRoots.filter({ $0.matches(roots.dialog) }).count == 1
              }) ?? true,
              modalFound == hasAuxiliaryInput, !unsupportedModalFound, !transientMenuFound,
              !menuDrawingFound, !menuProven else {
            throw VisiblePrivacyFailure(reason: "window_or_overlay_boundary_unproved")
        }
        guard let window = structure.windows.filter({ $0.window_id == pending.windowID }).only,
              window.kind == "window", !window.modal,
              window.owner_bundle_id == app.bundle_id,
              let nativeWindow = nativeWindows.filter({ $0.ref == pending.windowID && $0.pid == app.pid }).only,
              nativeWindows.count < 32,
              structure.windows.count == (hasAuxiliaryInput ? 2 : 1),
              !nativeWindows.contains(where: { candidate in
                  candidate.id != nativeWindow.id
                      && !(allowedAuxiliaryCGID.map { candidate.id == $0 } ?? false)
                      && candidate.frame.intersects(nativeWindow.frame)
              }),
              visiblePrivacyNativeWindowInventoryIsClear(pid: app.pid, targetWindowID: nativeWindow.id,
                  frame: window.frame, deadline: deadline,
                  auditedAuxiliaryWindow: auxiliaryInput?.dialogRoot.facts.exactCGMatches.only),
              window.frame == nativeWindow.frame else {
            throw VisiblePrivacyFailure(reason: "captured_window_geometry_or_overlay_unproved")
        }
        let auxiliaryWindowInfo: WindowInfo?
        if let auxiliaryInput, let cgWindow = auxiliaryInput.dialogRoot.facts.exactCGMatches.only {
            guard auxiliaryInput.dialogRoot.facts.pid == app.pid,
                  auxiliaryInput.dialogRoot.facts.role == "AXWindow",
                  auxiliaryInput.dialogRoot.facts.subrole == "AXDialog",
                  auxiliaryInput.dialogRoot.facts.modal == false,
                  auxiliaryInput.dialogRoot.facts.focused == false,
                  auxiliaryInput.dialogRoot.facts.frame == cgWindow.frame,
                  cgWindow.pid == app.pid, cgWindow.id != nativeWindow.id,
                  let info = structure.windows.filter({ $0.window_id == cgWindow.ref }).only,
                  info.kind == "window", !info.modal,
                  info.owner_bundle_id == app.bundle_id, info.frame == cgWindow.frame else {
                throw VisiblePrivacyFailure(reason: "auxiliary_window_metadata_unverified")
            }
            auxiliaryWindowInfo = info
        } else {
            guard !hasAuxiliaryInput else {
                throw VisiblePrivacyFailure(reason: "auxiliary_window_metadata_unverified")
            }
            auxiliaryWindowInfo = nil
        }
        guard let candidate = result.candidates.only,
              candidate.scroll_path.count == requested.scroll_path.count,
              candidate.scroll_path.map(\.label) == requested.labels,
              candidate.scroll_path.last?.instance_ref == candidate.scroll_container_instance_ref,
              TableScopeSearchValidation.headersMatch(
                  candidate.column_headers, requested: requested.column_headers),
              let viewport = structure.table_viewports.filter({
                  $0.table_instance_ref == candidate.table_instance_ref
              }).only,
              viewport.process_id == app.pid,
              viewport.scroll_container_instance_ref == candidate.scroll_container_instance_ref,
              viewport.visible_rows_complete, viewport.columns_complete,
              viewport.visible_rows.allSatisfy(\.cells_complete),
              !viewport.columns.isEmpty else {
            throw VisiblePrivacyFailure(reason: "table_viewport_not_complete")
        }
        guard let processStamp = pending.processStamp,
              ProcessStamp.read(pid: app.pid) == processStamp,
              uptimeNs() < deadline else {
            throw VisiblePrivacyFailure(reason: "process_or_deadline_unverified")
        }
        let scopeStart = uptimeNs()
        guard scopeStart < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_visible_walk") }

        let rootElement = pending.window
        guard let ownerRoot = actualAXWindowRoots.first(where: { $0.matches(rootElement) }) else {
            throw VisiblePrivacyFailure(reason: "window_root_identity_missing")
        }
        var currentRootElement = rootElement
        var currentRootParent = application
        var currentRootOwner = ownerRoot
        var currentRootKind = VisiblePrivacyRootKind.standardWindow
        var currentRootFrame = window.frame
        var currentRootWindowID = pending.windowID
        var elements: [AXUIElement] = []
        var observations: [VisiblePrivacyAuditObservation] = []
        var childListAudits: [VisiblePrivacyChildListAudit] = []
        var windowOwnedGroupAudits: [VisiblePrivacyWindowOwnedGroupAudit] = []

        func owned(_ element: AXUIElement) throws -> UInt32 {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_during_pid_read") }
            var pid: pid_t = 0
            let error = AXUIElementGetPid(element, &pid)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_pid_read") }
            guard error == .success, pid > 0, UInt32(pid) == app.pid else {
                throw VisiblePrivacyFailure(reason: "visible_child_pid_mismatch")
            }
            return UInt32(pid)
        }

        func readRole(_ element: AXUIElement) throws -> String {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_role_read") }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_role_read") }
            guard error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID(),
                  let role = raw as? String, !role.isEmpty, role.utf8.count <= 128,
                  !role.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
                throw VisiblePrivacyFailure(reason: "visible_child_role_invalid")
            }
            return role
        }

        func readSubrole(_ element: AXUIElement) throws -> String? {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_subrole_read") }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, kAXSubroleAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_subrole_read") }
            if error == .attributeUnsupported || error == .noValue { return nil }
            guard error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID(),
                  let subrole = raw as? String, subrole.utf8.count <= 128,
                  !subrole.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
                throw VisiblePrivacyFailure(reason: "visible_child_subrole_invalid")
            }
            return subrole
        }

        func readModal(_ element: AXUIElement, required: Bool) throws -> Bool? {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_modal_read") }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, kAXModalAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_modal_read") }
            if error == .attributeUnsupported || error == .noValue {
                guard !required else { throw VisiblePrivacyFailure(reason: "window_modal_state_unknown") }
                return nil
            }
            guard error == .success, let raw, CFGetTypeID(raw) == CFBooleanGetTypeID(),
                  let value = raw as? NSNumber else {
                throw VisiblePrivacyFailure(reason: "visible_child_modal_type_invalid")
            }
            return value.boolValue
        }

        func readFocused(_ element: AXUIElement, required: Bool) throws -> Bool? {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_focused_read") }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(element, kAXFocusedAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_focused_read") }
            if error == .attributeUnsupported || error == .noValue {
                guard !required else { throw VisiblePrivacyFailure(reason: "window_focus_state_unknown") }
                return nil
            }
            guard error == .success, let raw, CFGetTypeID(raw) == CFBooleanGetTypeID(),
                  let value = raw as? NSNumber else {
                throw VisiblePrivacyFailure(reason: "visible_child_focused_type_invalid")
            }
            return value.boolValue
        }

        func actualParent(_ child: AXUIElement) throws -> AXUIElement {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_parent_read") }
            var raw: CFTypeRef?
            let error = AXUIElementCopyAttributeValue(child, kAXParentAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_parent_read") }
            guard error == .success, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                throw VisiblePrivacyFailure(reason: "visible_child_parent_unavailable")
            }
            let actual = unsafeDowncast(raw, to: AXUIElement.self)
            _ = try owned(actual)
            return actual
        }

        func rootParentIsApplication(_ element: AXUIElement) -> Bool {
            guard uptimeNs() < deadline, let parent = try? actualParent(element) else { return false }
            return CFEqual(parent, application)
        }

        func parentMismatchFailure(_ child: AXUIElement, actual: AXUIElement,
                                   expected: AXUIElement,
                                   auditDiagnostic: String) throws -> VisiblePrivacyFailure {
            func category(_ element: AXUIElement) throws -> String {
                let role = try readRole(element)
                return ["AXWindow", "AXGroup", "AXScrollArea", "AXTable", "AXColumn",
                        "AXHeader", "AXRow", "AXCell", "AXButton", "AXStaticText"].contains(role)
                    ? role : "other"
            }
            let childRole = try category(child)
            let actualRole = try category(actual)
            let expectedRole = try category(expected)
            return VisiblePrivacyFailure(reason:
                "visible_child_parent_mismatch:\(childRole)/actual_\(actualRole)/expected_\(expectedRole);\(auditDiagnostic)")
        }

        func actualAXChildrenCountRead(_ element: AXUIElement) -> VisiblePrivacyAXChildrenCountRead {
            guard uptimeNs() < deadline else { return .late }
            var count = 0
            let error = AXUIElementGetAttributeValueCount(
                element, kAXChildrenAttribute as CFString, &count)
            guard uptimeNs() < deadline else { return .late }
            switch error {
            case .success: return .success(count)
            case .attributeUnsupported: return .unsupported
            case .noValue: return .noValue
            default: return .failed
            }
        }

        func copyCompleteAXChildren(_ element: AXUIElement, count: Int) throws -> [AXUIElement] {
            guard count >= 0, count <= request.nodeLimit else {
                throw VisiblePrivacyFailure(reason: "window_owned_group_children_count_over_limit")
            }
            if count == 0 { return [] }
            guard uptimeNs() < deadline else {
                throw VisiblePrivacyFailure(reason: "deadline_before_window_owned_group_children_copy")
            }
            var raw: CFArray?
            let copyStatus = AXUIElementCopyAttributeValues(
                element, kAXChildrenAttribute as CFString, 0, count, &raw)
            guard uptimeNs() < deadline else {
                throw VisiblePrivacyFailure(reason: "deadline_after_window_owned_group_children_copy")
            }
            guard copyStatus == .success, let values = raw as? [AXUIElement],
                  values.count == count,
                  values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
                throw VisiblePrivacyFailure(reason: "window_owned_group_children_copy_incomplete")
            }
            return values
        }

        func childElements(_ element: AXUIElement, role: String) throws -> VisiblePrivacyChildren {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_visible_children") }
            var count = 0
            let visibleStatus = AXUIElementGetAttributeValueCount(
                element, kAXVisibleChildrenAttribute as CFString, &count)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_visible_children_count") }
            if visibleStatus == .success {
                guard count >= 0, count <= request.nodeLimit else {
                    throw VisiblePrivacyFailure(reason: "visible_children_count_over_limit")
                }
                if count == 0 { return VisiblePrivacyChildren(elements: [], source: .visibleChildren) }
                var raw: CFArray?
                guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_visible_children_copy") }
                let copyStatus = AXUIElementCopyAttributeValues(
                    element, kAXVisibleChildrenAttribute as CFString, 0, count, &raw)
                guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_visible_children_copy") }
                guard copyStatus == .success, let values = raw as? [AXUIElement], values.count == count,
                      values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
                    throw VisiblePrivacyFailure(reason: "visible_children_copy_incomplete")
                }
                return VisiblePrivacyChildren(elements: values, source: .visibleChildren)
            }
            guard visibleStatus == .attributeUnsupported || visibleStatus == .noValue else {
                throw VisiblePrivacyFailure(reason: "visible_children_status_unknown_\(visibleStatus.rawValue)")
            }

            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_children_fallback") }
            var fullCount = 0
            let fullStatus = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &fullCount)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_children_fallback_count") }
            if fullStatus == .attributeUnsupported {
                guard VisiblePrivacyGraph.leafRoleCanOmitAXChildren(role) else {
                    throw VisiblePrivacyFailure(reason: "container_ax_children_unsupported")
                }
                return VisiblePrivacyChildren(elements: [], source: .leafChildrenUnsupported)
            }
            guard fullStatus == .success, fullCount >= 0, fullCount <= request.nodeLimit else {
                let suffix = fullStatus == .success ? "over_limit" : "unavailable_\(fullStatus.rawValue)"
                throw VisiblePrivacyFailure(reason: "ax_children_fallback_\(suffix)")
            }
            if fullCount == 0 { return VisiblePrivacyChildren(elements: [], source: .childrenFallback) }
            var raw: CFArray?
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_children_fallback_copy") }
            let copyStatus = AXUIElementCopyAttributeValues(
                element, kAXChildrenAttribute as CFString, 0, fullCount, &raw)
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_children_fallback_copy") }
            guard copyStatus == .success, let values = raw as? [AXUIElement], values.count == fullCount,
                  values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
                throw VisiblePrivacyFailure(reason: "ax_children_fallback_incomplete")
            }
            return VisiblePrivacyChildren(elements: values, source: .childrenFallback)
        }

        func walk(_ element: AXUIElement, parentElement: AXUIElement?,
                  parentIndex: Int?, childIndex: Int?, depth: Int) throws {
            guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_during_visible_walk") }
            guard depth <= request.depthLimit else { throw VisiblePrivacyFailure(reason: "visible_depth_over_limit") }
            guard CFGetTypeID(element) == AXUIElementGetTypeID() else {
                throw VisiblePrivacyFailure(reason: "visible_child_ax_type_invalid")
            }
            guard !elements.contains(where: { CFEqual($0, element) }) else {
                throw VisiblePrivacyFailure(reason: "visible_child_alias_or_cycle")
            }
            guard reserveTableScopeNode(element) else {
                throw VisiblePrivacyFailure(reason: "shared_node_budget_exceeded")
            }
            let processID = try owned(element)
            let actualParentElement = try actualParent(element)
            let role = try readRole(element)
            let subrole = try readSubrole(element)
            guard VisiblePrivacyGraph.roleIsSupported(role, subrole: subrole,
                    root: parentIndex == nil, rootKind: currentRootKind) else {
                throw VisiblePrivacyFailure(reason: "visible_child_role_or_subrole_unclassified")
            }
            guard let rect = frame(element) else {
                // Role/subrole have already passed the fixed classification;
                // this diagnostic contains no element label or value.
                throw VisiblePrivacyFailure(reason: "visible_child_frame_unavailable:\(role)/\(subrole ?? "none")")
            }
            let modal = try readModal(element, required: parentIndex == nil)
            guard modal != true else { throw VisiblePrivacyFailure(reason: "visible_modal_layer_present") }
            let sensitive = isSensitiveField(role: role, subrole: subrole)
            guard !sensitive else { throw VisiblePrivacyFailure(reason: "visible_sensitive_control_present") }

            let parentOK: Bool
            if let expectedParent = parentElement, let expectedParentIndex = parentIndex {
                if CFEqual(actualParentElement, expectedParent) {
                    parentOK = true
                } else {
                    let roleIsAXGroup = role == "AXGroup"
                    let subroleIsEmpty = subrole == nil || subrole == ""
                    let expectedParentWasAudited = observations.indices.contains(expectedParentIndex)
                        && elements.indices.contains(expectedParentIndex)
                        && observations[expectedParentIndex].fact.role == "AXButton"
                        && CFEqual(elements[expectedParentIndex], expectedParent)
                    let capturedWindowIndexes = elements.indices.filter {
                        CFEqual(elements[$0], pending.window)
                    }
                    let parentChildrenSource = observations.indices.contains(expectedParentIndex)
                        ? observations[expectedParentIndex].fact.childrenSource : .unknown
                    let expectedParentChildrenComplete =
                        parentChildrenSource == .visibleChildren || parentChildrenSource == .childrenFallback
                    let capturedWindowIsUniqueRoot = capturedWindowIndexes == [0]
                    let actualParentIsCapturedWindow = CFEqual(actualParentElement, pending.window)
                    let groupFrameIsValid = VisiblePrivacyGraph.validNodeFrame(
                        rect, allowEmptyScrollBarArrowLeaf: false)
                    let canAuditWindowOwnedGroup = currentRootKind == .standardWindow
                        && roleIsAXGroup
                        && subroleIsEmpty
                        && expectedParentWasAudited
                        && expectedParentChildrenComplete
                        && capturedWindowIsUniqueRoot
                        && actualParentIsCapturedWindow
                        && groupFrameIsValid
                    let childrenRead: VisiblePrivacyAXChildrenCountRead? = canAuditWindowOwnedGroup
                        ? actualAXChildrenCountRead(element) : nil
                    if canAuditWindowOwnedGroup {
                        if case .success(0)? = childrenRead {
                            let candidate = VisiblePrivacyGroupParentMismatchCandidate(
                                expectedParentIndex: expectedParentIndex,
                                expectedParentRole: observations[expectedParentIndex].fact.role,
                                expectedParentWasAudited: expectedParentWasAudited,
                                expectedChildIndex: childIndex,
                                expectedParentChildrenSource: parentChildrenSource,
                                actualParentIndex: capturedWindowIndexes.only == 0 ? 0 : nil,
                                actualParentMatchesCapturedWindow: actualParentIsCapturedWindow,
                                processID: processID, role: role, subrole: subrole, frame: rect,
                                roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
                                modal: modal, sensitive: sensitive,
                                forbiddenLayer: VisiblePrivacyGraph.isForbiddenLayer(role, subrole: subrole, root: false),
                                axChildrenCountRead: .success(0),
                                copiedAXChildrenCount: nil, axChildrenCopyComplete: false)
                            if let fact = VisiblePrivacyGraph.classifyEmptyGroupParentMismatch(
                                candidate, expectedProcessID: app.pid) {
                                _ = identityProvider?(element, role, subrole, rect, currentRootWindowID, currentRootOwner)
                                let index = observations.count
                                observations.append(VisiblePrivacyAuditObservation(
                                    identityOrdinal: index,
                                    parentIdentityOrdinal: observations[0].identityOrdinal,
                                    processID: processID, actualParentMatches: false,
                                    roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
                                    modalTypeValid: true, sensitive: false,
                                    forbiddenLayer: false, modal: modal, fact: fact))
                                elements.append(element)
                                return
                            }
                        } else if case .success(let childCount)? = childrenRead, childCount > 0 {
                            guard childCount <= request.nodeLimit,
                                  childCount <= remainingTableScopeNodeBudget else {
                                throw VisiblePrivacyFailure(reason: "window_owned_group_children_over_node_budget")
                            }
                            let groupChildren = try copyCompleteAXChildren(element, count: childCount)
                            let candidate = VisiblePrivacyGroupParentMismatchCandidate(
                                expectedParentIndex: expectedParentIndex,
                                expectedParentRole: observations[expectedParentIndex].fact.role,
                                expectedParentWasAudited: expectedParentWasAudited,
                                expectedChildIndex: childIndex,
                                expectedParentChildrenSource: parentChildrenSource,
                                actualParentIndex: capturedWindowIndexes.only == 0 ? 0 : nil,
                                actualParentMatchesCapturedWindow: actualParentIsCapturedWindow,
                                processID: processID, role: role, subrole: subrole, frame: rect,
                                roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
                                modal: modal, sensitive: sensitive,
                                forbiddenLayer: VisiblePrivacyGraph.isForbiddenLayer(role, subrole: subrole, root: false),
                                axChildrenCountRead: .success(childCount),
                                copiedAXChildrenCount: groupChildren.count,
                                axChildrenCopyComplete: true)
                            guard let fact = VisiblePrivacyGraph.classifyWindowOwnedGroupParentMismatch(
                                candidate, expectedProcessID: app.pid) else {
                                throw VisiblePrivacyFailure(reason: "window_owned_group_children_unclassified")
                            }
                            _ = identityProvider?(element, role, subrole, rect, currentRootWindowID, currentRootOwner)
                            let index = observations.count
                            observations.append(VisiblePrivacyAuditObservation(
                                identityOrdinal: index,
                                parentIdentityOrdinal: observations[0].identityOrdinal,
                                processID: processID, actualParentMatches: false,
                                roleTypeValid: true, subroleTypeValid: true, frameTypeValid: true,
                                modalTypeValid: true, sensitive: sensitive,
                                forbiddenLayer: VisiblePrivacyGraph.isForbiddenLayer(role, subrole: subrole, root: false),
                                modal: modal, fact: fact))
                            elements.append(element)
                            windowOwnedGroupAudits.append(VisiblePrivacyWindowOwnedGroupAudit(
                                observationIndex: index, element: element, children: groupChildren))
                            for (position, child) in groupChildren.enumerated() {
                                try walk(child, parentElement: element, parentIndex: index,
                                         childIndex: position, depth: depth + 1)
                            }
                            return
                        }
                    }
                    let childCountReadDiagnostic: String
                    switch childrenRead {
                    case .success(let count): childCountReadDiagnostic = "success_\(count)"
                    case .unsupported: childCountReadDiagnostic = "unsupported"
                    case .noValue: childCountReadDiagnostic = "no_value"
                    case .failed: childCountReadDiagnostic = "failed"
                    case .late: childCountReadDiagnostic = "late"
                    case nil: childCountReadDiagnostic = "not_attempted"
                    }
                    let auditDiagnostic = "window_owned_group_audit(" + [
                        "eligible=\(canAuditWindowOwnedGroup)",
                        "role_is_group=\(roleIsAXGroup)",
                        "subrole_is_empty=\(subroleIsEmpty)",
                        "expected_parent_is_audited_button=\(expectedParentWasAudited)",
                        "expected_parent_children_complete=\(expectedParentChildrenComplete)",
                        "captured_window_is_unique_root=\(capturedWindowIsUniqueRoot)",
                        "actual_parent_is_captured_window=\(actualParentIsCapturedWindow)",
                        "frame_is_valid=\(groupFrameIsValid)",
                        "ax_children_count=\(childCountReadDiagnostic)",
                    ].joined(separator: ",") + ")"
                    throw try parentMismatchFailure(element, actual: actualParentElement,
                                                    expected: expectedParent,
                                                    auditDiagnostic: auditDiagnostic)
                }
            } else if parentElement == nil, parentIndex == nil {
                parentOK = CFEqual(element, currentRootElement)
                    && CFEqual(actualParentElement, currentRootParent)
                guard parentOK else {
                    throw VisiblePrivacyFailure(reason: "visible_root_identity_mismatch")
                }
                let expectedSubrole = currentRootKind == .standardWindow ? "AXStandardWindow" : "AXDialog"
                guard role == "AXWindow", subrole == expectedSubrole, rect == currentRootFrame else {
                    throw VisiblePrivacyFailure(reason: "visible_root_facts_changed")
                }
                if currentRootKind == .registeredAuxiliaryDialog,
                   try readFocused(element, required: true) != false {
                    throw VisiblePrivacyFailure(reason: "visible_auxiliary_root_focused")
                }
            } else {
                throw VisiblePrivacyFailure(reason: "visible_child_parent_unavailable")
            }
            let children = try childElements(element, role: role)
            let parentRole = parentIndex.map { observations[$0].fact.role }
            let emptyScrollBarArrowLeaf = VisiblePrivacyGraph.isEmptyScrollBarArrowLeaf(
                role: role, subrole: subrole, parentRole: parentRole, frame: rect,
                childrenSource: children.source, childCount: children.elements.count)
            guard VisiblePrivacyGraph.validNodeFrame(
                rect, allowEmptyScrollBarArrowLeaf: emptyScrollBarArrowLeaf) else {
                throw VisiblePrivacyFailure(reason: "visible_child_frame_unavailable:\(role)/\(subrole ?? "none")")
            }
            // Optional observer registration supplements this read-local proof;
            // read-scoped identities are not mistaken for cross-read identity.
            _ = identityProvider?(element, role, subrole, rect, currentRootWindowID, currentRootOwner)
            let index = observations.count
            let parentIdentity = parentIndex.map { observations[$0].identityOrdinal }
            observations.append(VisiblePrivacyAuditObservation(
                identityOrdinal: index, parentIdentityOrdinal: parentIdentity, processID: processID,
                actualParentMatches: parentOK, roleTypeValid: true, subroleTypeValid: true,
                frameTypeValid: true,
                modalTypeValid: parentIndex != nil || modal == false,
                sensitive: sensitive,
                forbiddenLayer: VisiblePrivacyGraph.isForbiddenLayer(role, subrole: subrole,
                    root: parentIndex == nil, rootKind: currentRootKind),
                modal: modal,
                fact: VisiblePrivacyNodeFact(parentIndex: parentIndex, childIndex: childIndex,
                    role: role, subrole: subrole, frame: rect, childrenSource: children.source,
                    childCount: children.elements.count)))
            elements.append(element)
            childListAudits.append(VisiblePrivacyChildListAudit(
                observationIndex: index, element: element, rootKind: currentRootKind,
                source: children.source,
                children: children.elements))
            for (position, child) in children.elements.enumerated() {
                try walk(child, parentElement: element, parentIndex: index,
                         childIndex: position, depth: depth + 1)
            }
        }

        func treeObservations(start: Int, end: Int) throws -> [VisiblePrivacyAuditObservation] {
            guard start >= 0, start < end, end <= observations.count else {
                throw VisiblePrivacyFailure(reason: "visible_tree_observation_range_invalid")
            }
            return try observations[start..<end].map { observed in
                let fact = observed.fact
                func localIndex(_ value: Int?) throws -> Int? {
                    guard let value else { return nil }
                    guard value >= start, value < end else {
                        throw VisiblePrivacyFailure(reason: "visible_tree_parent_crossed_root")
                    }
                    return value - start
                }
                let localFact = VisiblePrivacyNodeFact(
                    parentIndex: try localIndex(fact.parentIndex), childIndex: fact.childIndex,
                    role: fact.role, subrole: fact.subrole, frame: fact.frame,
                    childrenSource: fact.childrenSource, childCount: fact.childCount,
                    actualParentIndex: try localIndex(fact.actualParentIndex))
                return VisiblePrivacyAuditObservation(
                    identityOrdinal: observed.identityOrdinal,
                    parentIdentityOrdinal: observed.parentIdentityOrdinal,
                    processID: observed.processID, actualParentMatches: observed.actualParentMatches,
                    roleTypeValid: observed.roleTypeValid, subroleTypeValid: observed.subroleTypeValid,
                    frameTypeValid: observed.frameTypeValid, modalTypeValid: observed.modalTypeValid,
                    sensitive: observed.sensitive, forbiddenLayer: observed.forbiddenLayer,
                    modal: observed.modal, fact: localFact)
            }
        }

        func auxiliaryContentSignatures(windowInfo: WindowInfo, treeStart: Int,
                                        treeEnd: Int) throws -> [VisiblePrivacyAuxiliaryContentSignature] {
            let objectsForWindow = structure.objects.filter { $0.window_id == windowInfo.window_id }
            let roots = objectsForWindow.filter { $0.role == "AXWindow" }
            guard !objectsForWindow.isEmpty,
                  Set(objectsForWindow.map(\.instance_ref)).count == objectsForWindow.count,
                  roots.count == 1, let sourceRoot = roots.first,
                  sourceRoot.parent == nil, sourceRoot.subrole == "AXDialog",
                  sourceRoot.frame == windowInfo.frame, sourceRoot.title == windowInfo.title else {
                throw VisiblePrivacyFailure(reason: "auxiliary_source_objects_incomplete")
            }
            let ids = Set(objectsForWindow.map(\.instance_ref))
            guard objectsForWindow.allSatisfy({ object in
                !object.secure && !isSensitiveField(role: object.role, subrole: object.subrole)
                    && (object.role == "AXWindow"
                        ? object.instance_ref == sourceRoot.instance_ref
                        : VisiblePrivacyGraph.roleIsSupported(object.role, subrole: object.subrole,
                            root: false))
                    && (object.parent == nil || ids.contains(object.parent!))
            }) else {
                throw VisiblePrivacyFailure(reason: "auxiliary_source_content_unclassified")
            }
            for index in treeStart..<treeEnd {
                guard elements.indices.contains(index), observations.indices.contains(index),
                      let observed = observedElements.filter({ CFEqual($0.element, elements[index]) }).only,
                      let sourceObject = objectsForWindow.filter({ $0.instance_ref == observed.id }).only,
                      sourceObject.role == observations[index].fact.role,
                      sourceObject.subrole == observations[index].fact.subrole,
                      sourceObject.frame == observations[index].fact.frame,
                      sourceObject.window_id == windowInfo.window_id,
                      !sourceObject.secure,
                      !isSensitiveField(role: sourceObject.role, subrole: sourceObject.subrole) else {
                    throw VisiblePrivacyFailure(reason: "auxiliary_content_does_not_match_audited_tree")
                }
                if let parent = observed.parentElement {
                    guard let sourceParent = observedElements.filter({ CFEqual($0.element, parent) }).only,
                          sourceObject.parent == sourceParent.id else {
                        throw VisiblePrivacyFailure(reason: "auxiliary_content_parent_mismatch")
                    }
                } else if sourceObject.instance_ref != sourceRoot.instance_ref {
                    throw VisiblePrivacyFailure(reason: "auxiliary_content_root_mismatch")
                }
            }
            return objectsForWindow.map(VisiblePrivacyAuxiliaryContentSignature.init)
        }

        func revalidateVisibleChildLists() throws {
            let expectedIndices = observations.indices.filter {
                let source = observations[$0].fact.childrenSource
                return source != .emptyGroupAXChildrenSuccessCountZero
                    && source != .windowOwnedGroupAXChildren
            }
            guard childListAudits.map(\.observationIndex) == expectedIndices else {
                throw VisiblePrivacyFailure(reason: "visible_child_list_audit_record_mismatch")
            }
            for audit in childListAudits {
                let index = audit.observationIndex
                guard uptimeNs() < deadline,
                      observations.indices.contains(index), elements.indices.contains(index),
                      CFEqual(elements[index], audit.element) else {
                    throw VisiblePrivacyFailure(reason: "visible_child_list_audit_identity_changed")
                }
                let observed = observations[index]
                let fact = observed.fact
                guard fact.childrenSource == audit.source,
                      fact.childCount == audit.children.count,
                      try owned(elements[index]) == app.pid,
                      try readRole(elements[index]) == fact.role,
                      try readSubrole(elements[index]) == fact.subrole,
                      VisiblePrivacyGraph.roleIsSupported(fact.role, subrole: fact.subrole,
                          root: fact.parentIndex == nil, rootKind: audit.rootKind),
                      frame(elements[index]) == fact.frame,
                      try readModal(elements[index], required: fact.parentIndex == nil) == observed.modal,
                      !isSensitiveField(role: fact.role, subrole: fact.subrole) else {
                    throw VisiblePrivacyFailure(reason: "visible_child_list_parent_facts_changed")
                }
                let expectedParentElement: AXUIElement
                if let parentIndex = fact.actualParentIndex ?? fact.parentIndex {
                    guard elements.indices.contains(parentIndex) else {
                        throw VisiblePrivacyFailure(reason: "visible_child_list_parent_index_invalid")
                    }
                    expectedParentElement = elements[parentIndex]
                } else {
                    expectedParentElement = application
                }
                let actualParentElement = try actualParent(elements[index])
                guard CFEqual(actualParentElement, expectedParentElement) else {
                    throw VisiblePrivacyFailure(reason: "visible_child_list_actual_parent_changed")
                }
                if audit.rootKind == .registeredAuxiliaryDialog, fact.parentIndex == nil,
                   try readFocused(elements[index], required: true) != false {
                    throw VisiblePrivacyFailure(reason: "visible_child_list_auxiliary_focus_changed")
                }
                let current = try childElements(elements[index], role: fact.role)
                let referencesMatch = zip(audit.children, current.elements).map {
                    CFEqual($0.0, $0.1)
                }
                guard VisiblePrivacyGraph.childListPostcheckMatches(
                    expectedSource: audit.source, currentSource: current.source,
                    expectedCount: audit.children.count, currentCount: current.elements.count,
                    referencesMatchInOrder: referencesMatch),
                      uptimeNs() < deadline else {
                    throw VisiblePrivacyFailure(reason: "visible_child_list_changed_or_incomplete")
                }
            }
        }

        func revalidateEmptyGroupParentMismatches() throws {
            for index in observations.indices where
                observations[index].fact.childrenSource == .emptyGroupAXChildrenSuccessCountZero {
                guard uptimeNs() < deadline, index < elements.count,
                      try owned(elements[index]) == app.pid,
                      let expectedParentIndex = observations[index].fact.parentIndex,
                      elements.indices.contains(expectedParentIndex),
                      try owned(elements[expectedParentIndex]) == app.pid,
                      try readRole(elements[expectedParentIndex]) == "AXButton",
                      try readRole(elements[index]) == "AXGroup",
                      (try readSubrole(elements[index])).map({ $0.isEmpty }) ?? true,
                      frame(elements[index]) == observations[index].fact.frame,
                      try readModal(elements[index], required: false) != true,
                      let parentBeforeCount = try? actualParent(elements[index]),
                      CFEqual(parentBeforeCount, pending.window),
                      case .success(0) = actualAXChildrenCountRead(elements[index]),
                      let parentAfterCount = try? actualParent(elements[index]),
                      CFEqual(parentAfterCount, pending.window) else {
                    throw VisiblePrivacyFailure(reason: "empty_group_parent_mismatch_changed_or_unproved")
                }
            }
        }

        func revalidateWindowOwnedGroupChildren() throws {
            let markedIndices = observations.indices.filter {
                observations[$0].fact.childrenSource == .windowOwnedGroupAXChildren
            }
            guard markedIndices == windowOwnedGroupAudits.map(\.observationIndex) else {
                throw VisiblePrivacyFailure(reason: "window_owned_group_audit_record_mismatch")
            }
            for audit in windowOwnedGroupAudits {
                let index = audit.observationIndex
                guard uptimeNs() < deadline,
                      observations.indices.contains(index), elements.indices.contains(index),
                      CFEqual(elements[index], audit.element),
                      observations[index].fact.childrenSource == .windowOwnedGroupAXChildren,
                      observations[index].fact.childCount == audit.children.count,
                      let expectedParentIndex = observations[index].fact.parentIndex,
                      elements.indices.contains(expectedParentIndex),
                      observations[expectedParentIndex].fact.role == "AXButton",
                      observations[expectedParentIndex].fact.childrenSource == .visibleChildren
                          || observations[expectedParentIndex].fact.childrenSource == .childrenFallback,
                      try owned(elements[index]) == app.pid,
                      try owned(elements[expectedParentIndex]) == app.pid,
                      try readRole(elements[index]) == "AXGroup",
                      (try readSubrole(elements[index])).map({ $0.isEmpty }) ?? true,
                      frame(elements[index]) == observations[index].fact.frame,
                      try readModal(elements[index], required: false) != true,
                      let parentBeforeCopy = try? actualParent(elements[index]),
                      CFEqual(parentBeforeCopy, pending.window),
                      case .success(let count) = actualAXChildrenCountRead(elements[index]),
                      count == audit.children.count else {
                    throw VisiblePrivacyFailure(reason: "window_owned_group_before_copy_changed_or_unproved")
                }
                let childrenAfter = try copyCompleteAXChildren(elements[index], count: audit.children.count)
                let referencesMatch = zip(audit.children, childrenAfter).map {
                    CFEqual($0.0, $0.1)
                }
                guard VisiblePrivacyGraph.windowOwnedGroupAXChildrenPostcheckMatches(
                    expectedCount: audit.children.count,
                    countRead: .success(count), copiedCount: childrenAfter.count,
                    referencesMatchInOrder: referencesMatch),
                      uptimeNs() < deadline,
                      let parentAfterCopy = try? actualParent(elements[index]),
                      CFEqual(parentAfterCopy, pending.window) else {
                    throw VisiblePrivacyFailure(reason: "window_owned_group_children_changed_or_unproved")
                }
            }
        }

        try walk(rootElement, parentElement: nil, parentIndex: nil, childIndex: nil, depth: 0)
        let standardTreeEnd = observations.count
        let standardObservations = try treeObservations(start: 0, end: standardTreeEnd)
        guard let standardFingerprint = VisiblePrivacyGraph.fingerprint(
            observations: standardObservations, expectedProcessID: app.pid,
            windowFrame: window.frame, nodeLimit: request.nodeLimit),
              !standardFingerprint.isEmpty, standardFingerprint.count == standardTreeEnd else {
            throw VisiblePrivacyFailure(reason: "visible_tree_validation_failed")
        }
        var auditedObservationTrees = [standardObservations]
        var auxiliaryFingerprints: [VisiblePrivacyAuxiliaryFingerprint] = []
        var auditedNodeCount = standardFingerprint.count
        if let auxiliaryInput, let auxiliaryRoots, let auxiliaryWindowInfo,
           let dialogCGWindow = auxiliaryInput.dialogRoot.facts.exactCGMatches.only,
           let dialogFrame = auxiliaryInput.dialogRoot.facts.frame {
            guard revalidateAuxiliaryPrivacyAuditInput(auxiliaryInput, application: application,
                                                       expectedStandard: rootElement),
                  dialogCGWindow.pid == app.pid,
                  auxiliaryInput.dialogRoot.facts.role == "AXWindow",
                  auxiliaryInput.dialogRoot.facts.subrole == "AXDialog",
                  auxiliaryInput.dialogRoot.facts.modal == false,
                  auxiliaryInput.dialogRoot.facts.focused == false,
                  dialogFrame == dialogCGWindow.frame else {
                throw VisiblePrivacyFailure(reason: "auxiliary_window_changed_before_walk")
            }
            currentRootElement = auxiliaryRoots.dialog
            currentRootParent = application
            currentRootOwner = auxiliaryInput.dialogRoot.token
            currentRootKind = .registeredAuxiliaryDialog
            currentRootFrame = dialogFrame
            currentRootWindowID = dialogCGWindow.ref
            let auxiliaryTreeStart = observations.count
            try walk(currentRootElement, parentElement: nil, parentIndex: nil,
                     childIndex: nil, depth: 0)
            let auxiliaryTreeEnd = observations.count
            let auxiliaryObservations = try treeObservations(
                start: auxiliaryTreeStart, end: auxiliaryTreeEnd)
            auditedObservationTrees.append(auxiliaryObservations)
            guard let auxiliaryTree = VisiblePrivacyGraph.fingerprint(
                observations: auxiliaryObservations, expectedProcessID: app.pid,
                windowFrame: dialogFrame, nodeLimit: request.nodeLimit,
                rootKind: .registeredAuxiliaryDialog),
                  !auxiliaryTree.isEmpty, auxiliaryTree.count == auxiliaryTreeEnd - auxiliaryTreeStart,
                  VisiblePrivacyGraph.fitsSharedNodeLimit(
                      [standardFingerprint.count, auxiliaryTree.count], nodeLimit: request.nodeLimit),
                  let content = try? auxiliaryContentSignatures(windowInfo: auxiliaryWindowInfo,
                      treeStart: auxiliaryTreeStart, treeEnd: auxiliaryTreeEnd) else {
                throw VisiblePrivacyFailure(reason: "auxiliary_visible_tree_validation_failed")
            }
            auxiliaryFingerprints.append(VisiblePrivacyAuxiliaryFingerprint(
                windowID: auxiliaryWindowInfo.window_id, processID: app.pid,
                cgWindowID: dialogCGWindow.id, frame: dialogFrame,
                role: "AXWindow", subrole: "AXDialog", modal: false, focused: false,
                treeFingerprint: auxiliaryTree, contentSignatures: content))
            auditedNodeCount += auxiliaryTree.count
        }
        guard auxiliaryFingerprints.count == (hasAuxiliaryInput ? 1 : 0),
              VisiblePrivacyGraph.identityOrdinalsAreDisjoint(auditedObservationTrees),
              VisiblePrivacyGraph.fitsSharedNodeLimit(
                  [standardFingerprint.count] + auxiliaryFingerprints.map({ $0.treeFingerprint.count }),
                  nodeLimit: request.nodeLimit) else {
            throw VisiblePrivacyFailure(reason: "visible_tree_shared_budget_exceeded")
        }
        try revalidateVisibleChildLists()
        try revalidateEmptyGroupParentMismatches()
        try revalidateWindowOwnedGroupChildren()

        // Recheck the real root, focus, window inventory and process after the
        // full visible-descendant walk. These CF refs stay on this AX worker.
        guard uptimeNs() < deadline else { throw VisiblePrivacyFailure(reason: "deadline_before_privacy_postcheck") }
        let system = AXUIElementCreateSystemWide()
        let expectedWindowCount = hasAuxiliaryInput ? 2 : 1
        let auxiliaryPostcheck: Bool
        if let auxiliaryInput {
            auxiliaryPostcheck = revalidateAuxiliaryPrivacyAuditInput(
                auxiliaryInput, application: application, expectedStandard: rootElement)
        } else {
            auxiliaryPostcheck = true
        }
        guard AXUIElementSetMessagingTimeout(system, 0.04) == .success,
              auxiliaryPostcheck,
              let finalWindows = boundedRootWindows(application), finalWindows.count == expectedWindowCount,
              auxiliaryPrivacyAuditRootAppearsOnce(ownerRoot, in: finalWindows),
              (auxiliaryInput.map {
                  auxiliaryPrivacyAuditRootAppearsOnce($0.dialogRoot.token, in: finalWindows)
              } ?? true),
              try owned(rootElement) == app.pid,
              try readRole(rootElement) == "AXWindow",
              try readSubrole(rootElement) == "AXStandardWindow",
              frame(rootElement) == window.frame,
              try readModal(rootElement, required: true) == false,
              rootParentIsApplication(rootElement),
              (auxiliaryRoots.map { rootParentIsApplication($0.dialog) } ?? true),
              let focused = attribute(application, kAXFocusedWindowAttribute),
              CFGetTypeID(focused) == AXUIElementGetTypeID(), CFEqual(focused, rootElement),
              auxiliaryPrivacyAuditRootAppearsOnce(ownerRoot, in: preReadWindows),
              (auxiliaryInput.map {
                  auxiliaryPrivacyAuditRootAppearsOnce($0.dialogRoot.token, in: preReadWindows)
              } ?? true),
              let focusedApplication = attribute(system, kAXFocusedApplicationAttribute),
              CFGetTypeID(focusedApplication) == AXUIElementGetTypeID(),
              try owned(unsafeDowncast(focusedApplication, to: AXUIElement.self)) == app.pid,
              targetWindows(pid: app.pid) == nativeWindows,
              nativeWindows.count < 32,
              !nativeWindows.contains(where: { candidate in
                  candidate.id != nativeWindow.id
                      && !(allowedAuxiliaryCGID.map { candidate.id == $0 } ?? false)
                      && candidate.frame.intersects(nativeWindow.frame)
              }),
              visiblePrivacyNativeWindowInventoryIsClear(pid: app.pid, targetWindowID: nativeWindow.id,
                  frame: window.frame, deadline: deadline,
                  auditedAuxiliaryWindow: auxiliaryInput?.dialogRoot.facts.exactCGMatches.only),
              nativeMenuDrawingWindows(pid: app.pid).isEmpty,
              ProcessStamp.read(pid: app.pid) == processStamp,
              uptimeNs() < deadline else {
            throw VisiblePrivacyFailure(reason: "privacy_postcheck_changed_or_incomplete")
        }
        let finished = uptimeNs()
        guard finished <= deadline else { throw VisiblePrivacyFailure(reason: "deadline_after_privacy_postcheck") }
        return VisiblePrivacyCoverage(bundleID: app.bundle_id, processID: app.pid,
            processStamp: processStamp, windowID: pending.windowID, windowFrame: window.frame,
            tableScope: requested, readStartedNS: scopeStart, readFinishedNS: finished,
            visibleNodeCount: UInt32(auditedNodeCount), visibleTreeFingerprint: standardFingerprint,
            tableViewport: VisiblePrivacyTableViewport(viewport: viewport),
            auxiliaryWindows: auxiliaryFingerprints)
    }
}

enum CaptureReadPrivacyStatus: String, Sendable, Equatable {
    case eligible
    case sensitiveFrames = "sensitive_frames"
    case missingCoverage = "missing_coverage"
    case identityMismatch = "identity_mismatch"
    case windowMismatch = "window_mismatch"
    case activeLayerMismatch = "active_layer_mismatch"
    case readBoundaryInvalid = "read_boundary_invalid"
    case fingerprintInvalid = "fingerprint_invalid"
    case censusInvalid = "census_invalid"
    case viewportInvalid = "viewport_invalid"
    case objectMembershipInvalid = "object_membership_invalid"
    case windowMetadataMismatch = "window_metadata_mismatch"
}

/// Return a fixed, content-free reason tag for the existing privacy gate.
/// These tags are diagnostic only; callers must accept only `.eligible`.
func captureReadPrivacyStatus(_ read: StructureRead, window: WindowInfo) -> CaptureReadPrivacyStatus {
    // Preserve the established full-tree rule exactly: only secure frames
    // affect eligibility, with no dependency on a scoped certificate.
    if read.structure.complete {
        return read.secureFrames.isEmpty ? .eligible : .sensitiveFrames
    }

    guard let coverage = read.visiblePrivacy else { return .missingCoverage }
    guard coverage.bundleID == read.structure.app.bundle_id,
          coverage.processID == read.structure.app.pid,
          coverage.processID > 0,
          coverage.processStamp.seconds > 0 else { return .identityMismatch }
    guard coverage.windowID == window.window_id,
          coverage.windowFrame == window.frame else { return .windowMismatch }
    guard window.kind == "window", !window.modal,
          window.owner_bundle_id == coverage.bundleID else { return .windowMetadataMismatch }
    guard read.structure.active_layer == coverage.windowID else { return .activeLayerMismatch }
    guard !read.structure.complete,
          read.structure.incomplete_reason == scopedTableRowsOmittedReason else { return .censusInvalid }
    guard read.secureFrames.isEmpty else { return .sensitiveFrames }
    guard read.started <= coverage.readStartedNS,
          coverage.readStartedNS < coverage.readFinishedNS,
          coverage.readFinishedNS <= read.finished else { return .readBoundaryInvalid }
    let auxiliaryNodeCount = coverage.auxiliaryWindows.reduce(0) { $0 + $1.treeFingerprint.count }
    guard coverage.auxiliaryWindows.count <= 1,
          Int(coverage.visibleNodeCount) == coverage.visibleTreeFingerprint.count + auxiliaryNodeCount,
          coverage.visibleNodeCount > 0, coverage.visibleNodeCount <= 512,
          VisiblePrivacyGraph.validFingerprint(coverage.visibleTreeFingerprint,
              windowFrame: coverage.windowFrame) else { return .fingerprintInvalid }
    guard coverage.auxiliaryWindows.allSatisfy({
              VisiblePrivacyGraph.validAuxiliaryFingerprint($0,
                  expectedProcessID: coverage.processID,
                  nodeLimit: 512 - coverage.visibleTreeFingerprint.count)
          }) else { return .fingerprintInvalid }

    guard let scoped = read.structure.table_scope_search,
          scoped.requested_scope == coverage.tableScope,
          scoped.process_id == coverage.processID,
          scoped.window_id == coverage.windowID,
          case .complete = scoped.state,
          scoped.candidates.count == 1,
          let candidate = scoped.candidates.first,
          candidate.scroll_path.count == coverage.tableScope.scroll_path.count,
          candidate.scroll_path.map(\.label) == coverage.tableScope.labels,
          candidate.scroll_path.last?.instance_ref == candidate.scroll_container_instance_ref,
          TableScopeSearchValidation.headersMatch(
              candidate.column_headers, requested: coverage.tableScope.column_headers) else {
        return .censusInvalid
    }
    guard let viewport = read.structure.table_viewports.filter({
              $0.table_instance_ref == candidate.table_instance_ref
          }).only,
          viewport.process_id == coverage.processID,
          viewport.scroll_container_instance_ref == candidate.scroll_container_instance_ref,
          viewport.visible_rows_complete, viewport.columns_complete,
          viewport.visible_rows.allSatisfy(\.cells_complete),
          coverage.tableViewport == VisiblePrivacyTableViewport(viewport: viewport),
          TableScopeSearchValidation.headersMatch(
              viewport.columns.compactMap(\.header), requested: coverage.tableScope.column_headers) else {
        return .viewportInvalid
    }
    guard read.structure.objects.filter({ $0.instance_ref == candidate.table_instance_ref
              && $0.role == "AXTable" && $0.window_id == coverage.windowID
              && $0.parent == candidate.scroll_container_instance_ref }).count == 1,
          read.structure.objects.filter({ $0.instance_ref == candidate.scroll_container_instance_ref
              && $0.role == "AXScrollArea" && $0.window_id == coverage.windowID }).count == 1 else {
        return .objectMembershipInvalid
    }
    guard let sourceWindow = read.structure.windows.filter({
              $0.window_id == coverage.windowID
          }).only,
          sourceWindow.window_id == window.window_id,
          sourceWindow.title == window.title,
          sourceWindow.frame == window.frame,
          sourceWindow.z_index == window.z_index,
          sourceWindow.kind == window.kind,
          sourceWindow.modal == window.modal,
          sourceWindow.owner_bundle_id == window.owner_bundle_id else {
        return .windowMetadataMismatch
    }
    let auxiliaryStructureWindows = read.structure.windows.filter { $0.window_id != coverage.windowID }
    guard auxiliaryStructureWindows.count == coverage.auxiliaryWindows.count,
          read.structure.windows.count == coverage.auxiliaryWindows.count + 1 else {
        return .windowMetadataMismatch
    }
    let rootObjects = read.structure.objects.filter { $0.role == "AXWindow" }
    guard rootObjects.count == coverage.auxiliaryWindows.count + 1,
          let standardRoot = rootObjects.filter({ $0.window_id == coverage.windowID }).only,
          standardRoot.subrole == "AXStandardWindow", standardRoot.frame == coverage.windowFrame,
          standardRoot.parent == nil else { return .objectMembershipInvalid }
    for auxiliary in coverage.auxiliaryWindows {
        let auxiliaryObjects = read.structure.objects.filter({ $0.window_id == auxiliary.windowID })
        guard let auxiliaryWindow = auxiliaryStructureWindows.filter({
                  $0.window_id == auxiliary.windowID
              }).only,
              auxiliaryWindow.kind == "window", !auxiliaryWindow.modal,
              auxiliaryWindow.owner_bundle_id == coverage.bundleID,
              auxiliaryWindow.frame == auxiliary.frame,
              let auxiliaryRoot = rootObjects.filter({ $0.window_id == auxiliary.windowID }).only,
              auxiliaryRoot.subrole == "AXDialog", auxiliaryRoot.frame == auxiliary.frame,
              auxiliaryRoot.title == auxiliaryWindow.title, auxiliaryRoot.parent == nil,
              !auxiliaryObjects.isEmpty, auxiliaryObjects.count <= 512,
              Set(auxiliaryObjects.map(\.instance_ref)).count == auxiliaryObjects.count,
              auxiliary.contentSignatures == auxiliaryObjects.map(VisiblePrivacyAuxiliaryContentSignature.init) else {
            return .windowMetadataMismatch
        }
        let auxiliaryIDs = Set(auxiliaryObjects.map(\.instance_ref))
        for object in auxiliaryObjects where object.instance_ref != auxiliaryRoot.instance_ref {
            var current = object
            var visited = Set<String>()
            while current.instance_ref != auxiliaryRoot.instance_ref {
                guard visited.insert(current.instance_ref).inserted,
                      let parentID = current.parent, auxiliaryIDs.contains(parentID),
                      let parent = auxiliaryObjects.first(where: { $0.instance_ref == parentID }) else {
                    return .objectMembershipInvalid
                }
                current = parent
            }
        }
    }
    return .eligible
}

func captureReadPrivacyEligible(_ read: StructureRead, window: WindowInfo) -> Bool {
    captureReadPrivacyStatus(read, window: window) == .eligible
}

private extension Array {
    var only: Element? { count == 1 ? first : nil }
}

extension VisiblePrivacyGraph {
    static func isForbiddenLayer(_ role: String, subrole: String?, root: Bool,
                                 rootKind: VisiblePrivacyRootKind = .standardWindow) -> Bool {
        if blockedLayerRoleNames.contains(role) { return true }
        if !root && role == "AXWindow" { return true }
        if root, rootKind == .registeredAuxiliaryDialog,
           role == "AXWindow", subrole == "AXDialog" { return false }
        return ["AXDialog", "AXPopover", "AXSheet", "AXSecureTextField"].contains(subrole ?? "")
    }

    static func roleIsSupported(_ role: String, subrole: String?, root: Bool,
                                rootKind: VisiblePrivacyRootKind = .standardWindow) -> Bool {
        allowedRoles.contains(role) && !isForbiddenLayer(role, subrole: subrole,
            root: root, rootKind: rootKind)
            && subroleIsSafe(subrole, root: root, role: role, rootKind: rootKind)
    }

    static func leafRoleCanOmitAXChildren(_ role: String) -> Bool {
        leafRoles.contains(role)
    }

    private static let blockedLayerRoleNames: Set<String> = [
        "AXMenu", "AXMenuBar", "AXMenuItem", "AXSheet", "AXDialog", "AXPopover", "AXApplication",
    ]
}
