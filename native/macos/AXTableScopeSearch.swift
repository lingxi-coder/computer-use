import Foundation
import ApplicationServices

/// A request may name a public table scope, but never a row value, row index,
/// or Source object identity.
struct TableScopeSearchRequest: Codable, Sendable, Equatable {
    let scroll_path: [TableScopeSearchStep]
    let column_headers: [String]

    private enum CodingKeys: String, CodingKey { case scroll_path, column_headers }

    init(scroll_path: [TableScopeSearchStep], column_headers: [String]) {
        self.scroll_path = scroll_path
        self.column_headers = column_headers
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: TableScopeSearchDynamicKey.self)
        let keys = Set(container.allKeys.map(\.stringValue))
        guard keys == Set(["scroll_path", "column_headers"]) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "table_scope accepts only scroll_path and column_headers"))
        }
        scroll_path = try container.decode([TableScopeSearchStep].self,
            forKey: TableScopeSearchDynamicKey("scroll_path"))
        column_headers = try container.decode([String].self,
            forKey: TableScopeSearchDynamicKey("column_headers"))
        guard valid else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "table_scope is outside its bounded public shape"))
        }
    }

    var valid: Bool {
        guard !scroll_path.isEmpty, scroll_path.count <= 8,
              !column_headers.isEmpty, column_headers.count <= 64,
              scroll_path.allSatisfy(\.valid),
              column_headers.allSatisfy({ validScopeText($0, maximum: 256) }) else { return false }
        return Set(column_headers.map(scopeHeaderKey)).count == column_headers.count
    }

    var labels: [String] { scroll_path.map(\.label) }
}

struct TableScopeSearchStep: Codable, Sendable, Equatable {
    let role: String
    let label: String

    private enum CodingKeys: String, CodingKey { case role, label }

    init(role: String, label: String) {
        self.role = role
        self.label = label
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: TableScopeSearchDynamicKey.self)
        let keys = Set(container.allKeys.map(\.stringValue))
        guard keys == Set(["role", "label"]) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "table scope path items accept only role and label"))
        }
        role = try container.decode(String.self, forKey: TableScopeSearchDynamicKey("role"))
        label = try container.decode(String.self, forKey: TableScopeSearchDynamicKey("label"))
        guard valid else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "table scope path item is invalid"))
        }
    }

    var valid: Bool { role == "ax_scroll_area" && validScopeText(label, maximum: 256) }
}

private struct TableScopeSearchDynamicKey: CodingKey, Hashable {
    let stringValue: String
    let intValue: Int? = nil

    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

private func validScopeText(_ value: String, maximum: Int) -> Bool {
    !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && value.utf8.count <= maximum
        && !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
}

private func scopeHeaderKey(_ value: String) -> String {
    value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
}

struct TableScopeSearchPathItem: Encodable, Sendable, Equatable {
    let instance_ref: String
    let label: String
    let frame: Rect
}

struct TableScopeSearchCandidate: Encodable, Sendable, Equatable {
    let table_instance_ref: String
    let scroll_container_instance_ref: String
    let scroll_path: [TableScopeSearchPathItem]
    let column_headers: [String]
}

enum TableScopeSearchState: Encodable, Sendable, Equatable {
    case complete
    case incomplete(reason: String)

    private enum CodingKeys: String, CodingKey { case state, reason }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .complete:
            try container.encode("complete", forKey: .state)
        case .incomplete(let reason):
            try container.encode("incomplete", forKey: .state)
            try container.encode(reason, forKey: .reason)
        }
    }
}

struct TableScopeSearchResult: Encodable, Sendable, Equatable {
    let requested_scope: TableScopeSearchRequest
    let process_id: UInt32
    let window_id: String
    let state: TableScopeSearchState
    let candidates: [TableScopeSearchCandidate]
}

struct PendingTableScopeCandidate {
    let table: AXUIElement
    let scrollContainer: AXUIElement
    let path: [(element: AXUIElement, label: String, frame: Rect)]
    let columnHeaders: [String]
}

struct PendingTableScopeSearch {
    let request: TableScopeSearchRequest
    let processID: UInt32
    let windowID: String
    let window: AXUIElement
    let candidates: [PendingTableScopeCandidate]
    /// Exact-path/direct-parent tables with completely read headers. The
    /// global walk may omit their row descendants only after it registers the
    /// corresponding independent viewport projection.
    let prunableTables: [AXUIElement]
    let incompleteReason: String?
    let processStamp: ProcessStamp?
}

enum TableScopePathRelation: Equatable {
    case prefix
    case exact
    case impossible
}

enum TableScopeSubroleRead: Equatable {
    case absent
    case value(String)
    case invalid

    var isAbsentOrEmpty: Bool {
        switch self {
        case .absent: return true
        case .value(let value): return value.isEmpty
        case .invalid: return false
        }
    }
}

private struct PendingWindowOwnedGroupAudit {
    let element: AXUIElement
    let expectedButton: AXUIElement
    let rawChildIndex: Int
    let frame: Rect
    let subrole: TableScopeSubroleRead
    var actualChildren: [AXUIElement]?
    var childWalkCompleted = false
}

/// A table with a shorter matching scroll path may contain the remaining
/// nested scroll ancestry, so its children must be searched. A table at the
/// exact path is an eligible candidate only when its direct parent is the
/// nearest scoped AXScrollArea; that row subtree can then be pruned safely.
enum TableScopeSearchValidation {
    /// A fully enumerated empty AXGroup cannot contain a scoped scroller or
    /// table, so its inconsistent chrome parent cannot affect table coverage.
    static func excludesEmptyGroup(role: String?, childCountStatus: AXError,
                                   childCount: Int, timely: Bool) -> Bool {
        role == kAXGroupRole && childCountStatus == .success && childCount == 0 && timely
    }

    /// Allows traversal of one narrowly identified AXChildren branch when AXParent
    /// reports the captured window instead of the already visited button slot.
    /// It grants no table, path, or input authority; the caller still recursively
    /// enumerates the group before the census can complete.
    static func allowsWindowOwnedGroupParentMismatch(
        expectedParentRole: String?, expectedParentWasVisited: Bool,
        expectedParentProcessMatches: Bool,
        capturedWindowIsUnique: Bool, actualParentRole: String?,
        actualParentIsCapturedWindow: Bool, childProcessMatches: Bool,
        childRole: String?, childSubrole: TableScopeSubroleRead,
        childFrameIsPositive: Bool
    ) -> Bool {
        expectedParentRole == kAXButtonRole
            && expectedParentWasVisited
            && expectedParentProcessMatches
            && capturedWindowIsUnique
            && actualParentRole == kAXWindowRole
            && actualParentIsCapturedWindow
            && childProcessMatches
            && childRole == kAXGroupRole
            && childSubrole.isAbsentOrEmpty
            && childFrameIsPositive
    }

    static func windowOwnedGroupChildrenPostcheckMatches(
        parentBeforeIsCapturedWindow: Bool, parentAfterIsCapturedWindow: Bool,
        countStatus: AXError, expectedCount: Int, observedCount: Int,
        copyStatus: AXError, copiedCount: Int, referencesMatchInOrder: [Bool]
    ) -> Bool {
        guard parentBeforeIsCapturedWindow, parentAfterIsCapturedWindow,
              expectedCount >= 0,
              countStatus == .success, observedCount == expectedCount,
              copyStatus == .success, copiedCount == expectedCount,
              referencesMatchInOrder.count == expectedCount else { return false }
        return referencesMatchInOrder.allSatisfy { $0 }
    }

    static func windowOwnedGroupTraversalComplete(
        mismatchWasAuthorized: Bool, rawChildrenCount: Int?,
        recursivelyWalkedEveryRelevantChild: Bool
    ) -> Bool {
        guard mismatchWasAuthorized,
              let rawChildrenCount, rawChildrenCount >= 0, rawChildrenCount <= 512,
              recursivelyWalkedEveryRelevantChild else { return false }
        return true
    }

    static func childScrollPath<PathElement>(
        isWindowOwnedGroup: Bool, expectedParentPath: [PathElement]
    ) -> [PathElement] {
        isWindowOwnedGroup ? [] : expectedParentPath
    }

    static func windowOwnedGroupIdentityPostcheckMatches(
        expectedParentWasVisited: Bool, expectedParentRole: String?,
        expectedParentProcessMatches: Bool, childProcessMatches: Bool,
        childRole: String?, childSubroleBefore: TableScopeSubroleRead,
        childSubroleAfter: TableScopeSubroleRead,
        childFrameBefore: Rect?, childFrameAfter: Rect?
    ) -> Bool {
        guard expectedParentWasVisited,
              expectedParentRole == kAXButtonRole,
              expectedParentProcessMatches,
              childProcessMatches,
              childRole == kAXGroupRole,
              childSubroleBefore.isAbsentOrEmpty,
              childSubroleAfter == childSubroleBefore,
              let childFrameBefore, let childFrameAfter,
              childFrameAfter == childFrameBefore,
              childFrameAfter.valid, childFrameAfter.width > 0, childFrameAfter.height > 0 else {
            return false
        }
        return true
    }

    static func pathRelation(_ actual: [String], requested: [String]) -> TableScopePathRelation {
        guard actual.count <= requested.count,
              zip(actual, requested).allSatisfy({ $0 == $1 }) else { return .impossible }
        return actual.count == requested.count ? .exact : .prefix
    }

    static func tableDisposition(actualPath: [String], requestedPath: [String],
                                 directParentIsNearestScroll: Bool) -> TableScopeTableDisposition {
        switch pathRelation(actualPath, requested: requestedPath) {
        case .impossible: return .prune
        case .prefix: return .inspectChildren
        case .exact:
            return directParentIsNearestScroll ? .candidateAndPrune : .prune
        }
    }

    static func headersMatch(_ actual: [String], requested: [String]) -> Bool {
        actual.count == requested.count && actual.sorted() == requested.sorted()
    }

    static func globalWalkDecision(scopeRequested: Bool, censusComplete: Bool,
                                   roleIsTable: Bool, viewportRegistered: Bool,
                                   exactScopeReference: Bool) -> TableScopeGlobalWalkDecision {
        guard scopeRequested, censusComplete, roleIsTable, viewportRegistered, exactScopeReference else {
            return .traverseChildren
        }
        return .pruneTableRowsAndMarkGlobalPartial
    }

    static func result(request: TableScopeSearchRequest, processID: UInt32, windowID: String,
                       candidates: [TableScopeSearchCandidate], traversalComplete: Bool,
                       viewportCrossLinksComplete: Bool, boundaryStable: Bool,
                       incompleteReason: String?) -> TableScopeSearchResult {
        let reason: String?
        if !request.valid { reason = "invalid_request" }
        else if !traversalComplete { reason = incompleteReason ?? "scope_traversal_incomplete" }
        else if !boundaryStable { reason = "source_window_or_process_changed" }
        else if !viewportCrossLinksComplete { reason = "table_viewport_cross_link_incomplete" }
        else if candidates.count > 32 { reason = "table_candidate_limit_exceeded" }
        else { reason = nil }
        let state: TableScopeSearchState
        if let reason { state = .incomplete(reason: reason) }
        else { state = .complete }
        return TableScopeSearchResult(requested_scope: request, process_id: processID,
            window_id: windowID, state: state, candidates: reason == nil ? candidates : [])
    }
}

enum TableScopeTableDisposition: Equatable {
    case inspectChildren
    case candidateAndPrune
    case prune
}

enum TableScopeGlobalWalkDecision: Equatable {
    case traverseChildren
    case pruneTableRowsAndMarkGlobalPartial
}

extension AXReader {
    func captureTableScopeSearch(request: TableScopeSearchRequest, frontmostPID: UInt32?,
                                 axWindows: [AXUIElement], focusedWindow: CFTypeRef?,
                                 nativeWindows: [NativeWindow]) -> PendingTableScopeSearch? {
        guard let fixedWindowID = self.request.text_policy?.window_id,
              self.request.text_policy?.valid(bundle: app.bundle_id, pid: app.pid) == true else {
            return nil
        }
        func failed(_ reason: String) -> PendingTableScopeSearch {
            PendingTableScopeSearch(request: request, processID: app.pid, windowID: fixedWindowID,
                window: axWindows.first ?? AXUIElementCreateApplication(pid_t(app.pid)),
                candidates: [], prunableTables: [], incompleteReason: reason, processStamp: nil)
        }
        guard request.valid else { return failed("invalid_request") }
        guard frontmostPID == app.pid else { return failed("target_not_frontmost") }
        guard let focusedWindow, CFGetTypeID(focusedWindow) == AXUIElementGetTypeID() else {
            return failed("focused_window_unavailable")
        }
        let focusedAXWindow = unsafeDowncast(focusedWindow, to: AXUIElement.self)
        let focusedMatches = axWindows.filter { CFEqual($0, focusedAXWindow) }
        guard focusedMatches.count == 1 else { return failed("focused_window_not_in_root_inventory") }
        guard let windowFrame = scopeFrame(focusedAXWindow) else { return failed("focused_window_geometry_unavailable") }
        guard scopeString(focusedAXWindow, kAXRoleAttribute) == kAXWindowRole,
              scopeString(focusedAXWindow, kAXSubroleAttribute) == kAXStandardWindowSubrole else {
            return failed("focused_window_not_standard")
        }
        guard let modal = scopeBoolean(focusedAXWindow, kAXModalAttribute) else { return failed("focused_window_modal_unknown") }
        guard !modal else { return failed("focused_window_is_modal") }
        guard let nativeWindow = matchAXWindow(windowFrame, windows: nativeWindows) else { return failed("focused_window_native_inventory_mismatch") }
        guard nativeWindow.ref == fixedWindowID else { return failed("focused_window_changed") }

        let initialProcess = ProcessStamp.read(pid: app.pid)
        guard initialProcess != nil, reserveTableScopeNode(focusedAXWindow) else {
            return failed("scope_node_budget_exceeded")
        }
        var visited: [AXUIElement] = []
        var candidates: [PendingTableScopeCandidate] = []
        var prunableTables: [AXUIElement] = []
        var windowOwnedGroupAudits: [PendingWindowOwnedGroupAudit] = []
        var failure: String?
        let requestedLabels = request.labels

        func owned(_ element: AXUIElement) -> Bool {
            var pid: pid_t = 0
            return AXUIElementGetPid(element, &pid) == .success && pid > 0 && UInt32(pid) == app.pid
        }

        func readSubroleEvidence(_ element: AXUIElement) -> TableScopeSubroleRead {
            guard uptimeNs() < deadline else { return .invalid }
            var raw: CFTypeRef?
            let status = AXUIElementCopyAttributeValue(element, kAXSubroleAttribute as CFString, &raw)
            guard uptimeNs() < deadline else { return .invalid }
            if status == .attributeUnsupported || status == .noValue { return .absent }
            guard status == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID(),
                  let subrole = raw as? String, subrole.utf8.count <= 2_048,
                  !subrole.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
                return .invalid
            }
            return .value(subrole)
        }

        func saveRawChildrenForWindowOwnedGroup(_ parent: AXUIElement,
                                                children: [AXUIElement]) -> Bool {
            let auditIndices = windowOwnedGroupAudits.indices.filter {
                CFEqual(windowOwnedGroupAudits[$0].element, parent)
            }
            guard auditIndices.count <= 1 else {
                failure = "scope_window_owned_group_audit_duplicated"
                return false
            }
            guard let auditIndex = auditIndices.first else { return true }
            guard case nil = windowOwnedGroupAudits[auditIndex].actualChildren else {
                failure = "scope_window_owned_group_children_read_twice"
                return false
            }
            windowOwnedGroupAudits[auditIndex].actualChildren = children
            return true
        }

        func scopedChildren(_ parent: AXUIElement, leafRole: String?) -> [AXUIElement]? {
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            var count = 0
            let countError = AXUIElementGetAttributeValueCount(parent, kAXChildrenAttribute as CFString, &count)
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            if countError == .attributeUnsupported || countError == .noValue {
                if let leafRole, Self.scopeChildrenMayBeEmpty(for: leafRole) { return [] }
                failure = "scope_children_unavailable"
                return nil
            }
            guard countError == .success, count >= 0,
                  count <= remainingTableScopeNodeBudget else {
                failure = countError == .success ? "scope_node_budget_exceeded" : "scope_children_unavailable"
                return nil
            }
            if count == 0 {
                guard saveRawChildrenForWindowOwnedGroup(parent, children: []) else { return nil }
                return []
            }
            var raw: CFArray?
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            let copyStatus = AXUIElementCopyAttributeValues(parent, kAXChildrenAttribute as CFString,
                0, count, &raw)
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            guard copyStatus == .success, let children = raw as? [AXUIElement],
                  children.count == count, children.allSatisfy(owned) else {
                failure = "scope_children_incomplete"
                return nil
            }
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            var relevantChildren: [AXUIElement] = []
            for (childIndex, child) in children.enumerated() {
                guard reserveTableScopeNode(child) else {
                    failure = "scope_node_budget_exceeded"
                    return nil
                }
                guard let actualParent = tableElementForScope(child, attribute: kAXParentAttribute),
                      uptimeNs() < deadline else {
                    failure = uptimeNs() >= deadline ? "scope_time_budget_exceeded" : "scope_child_parent_unavailable"
                    return nil
                }
                guard CFEqual(actualParent, parent) else {
                    let roles: Set<String> = ["AXWindow", "AXScrollArea", "AXGroup", "AXApplication", "AXTable", "AXRow", "AXColumn", "AXCell", "AXButton"]
                    let childRole = scopeString(child, kAXRoleAttribute).flatMap { roles.contains($0) ? $0 : nil } ?? "other"
                    let actualParentRole = scopeString(actualParent, kAXRoleAttribute)
                        .flatMap { roles.contains($0) ? $0 : nil } ?? "other"
                    let actualParentIsCapturedWindow = CFEqual(actualParent, focusedAXWindow)
                    let expectedParentRole = leafRole == kAXButtonRole
                        && visited.contains(where: { CFEqual($0, parent) })
                        ? scopeString(parent, kAXRoleAttribute) : nil
                    let expectedParentWasVisited = visited.contains(where: { CFEqual($0, parent) })
                        && expectedParentRole == kAXButtonRole
                    let expectedParentProcessMatches = expectedParentWasVisited && owned(parent)
                    if childRole == kAXGroupRole && expectedParentWasVisited
                        && actualParentIsCapturedWindow && focusedMatches.count == 1
                        && expectedParentProcessMatches {
                        let childSubrole = readSubroleEvidence(child)
                        let childFrame = scopeFrame(child)
                        let childProcessMatches = owned(child)
                        let childFrameIsPositive = childFrame.map {
                            $0.valid && $0.width > 0 && $0.height > 0
                        } ?? false
                        if TableScopeSearchValidation.allowsWindowOwnedGroupParentMismatch(
                            expectedParentRole: expectedParentRole,
                            expectedParentWasVisited: expectedParentWasVisited,
                            expectedParentProcessMatches: expectedParentProcessMatches,
                            capturedWindowIsUnique: focusedMatches.count == 1,
                            actualParentRole: actualParentIsCapturedWindow ? kAXWindowRole : actualParentRole,
                            actualParentIsCapturedWindow: actualParentIsCapturedWindow,
                            childProcessMatches: childProcessMatches,
                            childRole: childRole, childSubrole: childSubrole,
                            childFrameIsPositive: childFrameIsPositive),
                           let childFrame {
                            guard !windowOwnedGroupAudits.contains(where: {
                                CFEqual($0.element, child)
                            }) else {
                                failure = "scope_alias_or_identity_invalid"
                                return nil
                            }
                            windowOwnedGroupAudits.append(PendingWindowOwnedGroupAudit(
                                element: child, expectedButton: parent, rawChildIndex: childIndex,
                                frame: childFrame, subrole: childSubrole,
                                actualChildren: nil, childWalkCompleted: false))
                            relevantChildren.append(child)
                            continue
                        }
                    }
                    var childCount = 0
                    let childCountStatus = uptimeNs() < deadline
                        ? AXUIElementGetAttributeValueCount(child, kAXChildrenAttribute as CFString, &childCount)
                        : .cannotComplete
                    if TableScopeSearchValidation.excludesEmptyGroup(role: childRole,
                        childCountStatus: childCountStatus, childCount: childCount,
                        timely: uptimeNs() < deadline) {
                        continue
                    }
                    failure = "scope_child_parent_mismatch_\(childRole)_\(actualParentRole)_children_\(childCountStatus.rawValue)_\(childCount)"
                    return nil
                }
                relevantChildren.append(child)
            }
            guard uptimeNs() < deadline,
                  saveRawChildrenForWindowOwnedGroup(parent, children: children) else {
                if failure == nil { failure = "scope_time_budget_exceeded" }
                return nil
            }
            return relevantChildren
        }

        func isRepeated(_ element: AXUIElement) -> Bool {
            if visited.contains(where: { CFEqual($0, element) }) { return true }
            visited.append(element)
            return false
        }

        func readHeaders(_ table: AXUIElement) -> [String]? {
            guard uptimeNs() < deadline else { failure = "scope_time_budget_exceeded"; return nil }
            var count = 0
            guard AXUIElementGetAttributeValueCount(table, kAXColumnsAttribute as CFString, &count) == .success,
                  count > 0, count <= 64, count <= remainingTableScopeNodeBudget else {
                failure = "scope_table_columns_incomplete"
                return nil
            }
            var raw: CFArray?
            guard AXUIElementCopyAttributeValues(table, kAXColumnsAttribute as CFString, 0, count, &raw) == .success,
                  let columns = raw as? [AXUIElement], columns.count == count, columns.allSatisfy(owned) else {
                failure = "scope_table_columns_incomplete"
                return nil
            }
            var headers: [String] = []
            for column in columns {
                guard reserveTableScopeNode(column),
                      let role = scopeString(column, kAXRoleAttribute), role == kAXColumnRole,
                      let parent = tableElementForScope(column, attribute: kAXParentAttribute), CFEqual(parent, table),
                      let header = tableElementForScope(column, attribute: kAXHeaderAttribute),
                      reserveTableScopeNode(header),
                      let title = scopeString(header, kAXTitleAttribute), validScopeText(title, maximum: 2_048) else {
                    failure = "scope_table_headers_incomplete"
                    return nil
                }
                headers.append(title)
            }
            guard Set(headers.map(scopeHeaderKey)).count == headers.count else {
                failure = "scope_table_headers_ambiguous"
                return nil
            }
            return headers.sorted()
        }

        func walk(_ element: AXUIElement, parent: AXUIElement?,
                  scrollPath: [(element: AXUIElement, label: String, frame: Rect)], depth: Int) {
            guard failure == nil else { return }
            defer {
                if let auditIndex = windowOwnedGroupAudits.firstIndex(where: {
                    CFEqual($0.element, element)
                }) {
                    let childWalkCompleted = failure == nil
                        && windowOwnedGroupAudits[auditIndex].actualChildren != nil
                    windowOwnedGroupAudits[auditIndex].childWalkCompleted = childWalkCompleted
                }
            }
            guard depth <= self.request.depthLimit, uptimeNs() < deadline, owned(element), !isRepeated(element) else {
                failure = depth > self.request.depthLimit ? "scope_depth_budget_exceeded"
                    : uptimeNs() >= deadline ? "scope_time_budget_exceeded" : "scope_alias_or_identity_invalid"
                return
            }
            guard let role = scopeString(element, kAXRoleAttribute) else {
                failure = "scope_role_unavailable"
                return
            }
            var path = scrollPath
            if role == kAXScrollAreaRole {
                if path.count >= requestedLabels.count { return }
                guard let label = scopeString(element, kAXDescriptionAttribute), validScopeText(label, maximum: 256),
                      let areaFrame = scopeFrame(element) else {
                    failure = "scope_scroll_label_or_geometry_unavailable"
                    return
                }
                path.append((element, label, areaFrame))
            }
            let relation = TableScopeSearchValidation.pathRelation(path.map(\.label), requested: requestedLabels)
            if relation == .impossible { return }

            if role == kAXTableRole {
                let directParentIsNearestScroll: Bool
                if let parent, let nearest = path.last?.element {
                    directParentIsNearestScroll = CFEqual(parent, nearest)
                } else {
                    directParentIsNearestScroll = false
                }
                switch TableScopeSearchValidation.tableDisposition(actualPath: path.map(\.label),
                    requestedPath: requestedLabels, directParentIsNearestScroll: directParentIsNearestScroll) {
                case .candidateAndPrune:
                    guard let parent, let nearest = path.last?.element,
                          let actualParent = tableElementForScope(element, attribute: kAXParentAttribute),
                          CFEqual(actualParent, parent), CFEqual(parent, nearest) else {
                        failure = "scope_table_parent_invalid"
                        return
                    }
                    guard let headers = readHeaders(element) else { return }
                    prunableTables.append(element)
                    if TableScopeSearchValidation.headersMatch(headers, requested: request.column_headers) {
                        candidates.append(PendingTableScopeCandidate(table: element,
                            scrollContainer: nearest, path: path, columnHeaders: headers))
                        if candidates.count > 32 { failure = "table_candidate_limit_exceeded" }
                    }
                    return
                case .prune:
                    return
                case .inspectChildren:
                    break
                }
            }

            // At the final scoped AXScrollArea only direct AXTable children
            // can produce a viewport for this exact RowTableScope. Nested
            // scrollers would add a path component, and non-table direct
            // children cannot satisfy the producer's direct-parent rule.
            if role == kAXScrollAreaRole, relation == .exact {
                guard path.count == requestedLabels.count else { return }
                guard let children = scopedChildren(element, leafRole: role) else { return }
                for child in children {
                    guard failure == nil else { return }
                    guard let childRole = scopeString(child, kAXRoleAttribute) else {
                        failure = "scope_role_unavailable"
                        return
                    }
                    if childRole == kAXTableRole {
                        walk(child, parent: element, scrollPath: path, depth: depth + 1)
                    }
                }
                return
            }

            guard let children = scopedChildren(element, leafRole: role) else { return }
            for child in children {
                guard failure == nil else { return }
                // The button is only the raw enumeration slot for this group;
                // its scroll ancestry cannot authorize the root-owned group.
                let childIsWindowOwnedGroup = windowOwnedGroupAudits.contains(where: {
                    CFEqual($0.element, child)
                })
                let childPath = TableScopeSearchValidation.childScrollPath(
                    isWindowOwnedGroup: childIsWindowOwnedGroup, expectedParentPath: path)
                walk(child, parent: element, scrollPath: childPath, depth: depth + 1)
            }
        }

        func revalidateWindowOwnedGroupAudits() -> Bool {
            for audit in windowOwnedGroupAudits {
                guard uptimeNs() < deadline,
                      audit.rawChildIndex >= 0,
                      TableScopeSearchValidation.windowOwnedGroupTraversalComplete(
                        mismatchWasAuthorized: true,
                        rawChildrenCount: audit.actualChildren?.count,
                        recursivelyWalkedEveryRelevantChild: audit.childWalkCompleted),
                      let expectedChildren = audit.actualChildren else { return false }
                let expectedParentWasVisited = visited.contains(where: {
                    CFEqual($0, audit.expectedButton)
                })
                let expectedParentRole = scopeString(audit.expectedButton, kAXRoleAttribute)
                let expectedParentProcessMatches = owned(audit.expectedButton)
                let childProcessMatches = owned(audit.element)
                let childRole = scopeString(audit.element, kAXRoleAttribute)
                let childSubrole = readSubroleEvidence(audit.element)
                let childFrame = scopeFrame(audit.element)
                guard uptimeNs() < deadline,
                      TableScopeSearchValidation.windowOwnedGroupIdentityPostcheckMatches(
                        expectedParentWasVisited: expectedParentWasVisited,
                        expectedParentRole: expectedParentRole,
                        expectedParentProcessMatches: expectedParentProcessMatches,
                        childProcessMatches: childProcessMatches,
                        childRole: childRole, childSubroleBefore: audit.subrole,
                        childSubroleAfter: childSubrole,
                        childFrameBefore: audit.frame, childFrameAfter: childFrame) else { return false }

                guard let parentBefore = tableElementForScope(audit.element, attribute: kAXParentAttribute),
                      uptimeNs() < deadline,
                      CFEqual(parentBefore, focusedAXWindow) else { return false }

                var postCount = 0
                guard uptimeNs() < deadline else { return false }
                let countStatus = AXUIElementGetAttributeValueCount(
                    audit.element, kAXChildrenAttribute as CFString, &postCount)
                guard uptimeNs() < deadline else { return false }
                guard countStatus == .success, postCount == expectedChildren.count,
                      postCount <= self.request.nodeLimit else { return false }

                var childrenAfter: [AXUIElement] = []
                var copyStatus: AXError = .success
                if postCount > 0 {
                    var raw: CFArray?
                    guard uptimeNs() < deadline else { return false }
                    copyStatus = AXUIElementCopyAttributeValues(
                        audit.element, kAXChildrenAttribute as CFString, 0, postCount, &raw)
                    guard uptimeNs() < deadline,
                          copyStatus == .success,
                          let values = raw as? [AXUIElement], values.count == postCount,
                          values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
                        return false
                    }
                    childrenAfter = values
                }
                let referencesMatch = zip(expectedChildren, childrenAfter).map {
                    CFEqual($0.0, $0.1)
                }

                guard let parentAfter = tableElementForScope(audit.element, attribute: kAXParentAttribute),
                      uptimeNs() < deadline,
                      TableScopeSearchValidation.windowOwnedGroupChildrenPostcheckMatches(
                        parentBeforeIsCapturedWindow: CFEqual(parentBefore, focusedAXWindow),
                        parentAfterIsCapturedWindow: CFEqual(parentAfter, focusedAXWindow),
                        countStatus: countStatus, expectedCount: expectedChildren.count,
                        observedCount: postCount, copyStatus: copyStatus,
                        copiedCount: childrenAfter.count,
                        referencesMatchInOrder: referencesMatch) else { return false }
            }
            return uptimeNs() < deadline
        }

        walk(focusedAXWindow, parent: nil, scrollPath: [], depth: 0)
        if failure == nil, !revalidateWindowOwnedGroupAudits() {
            failure = "scope_window_owned_group_postcheck_changed_or_incomplete"
        }
        guard failure == nil else { return failed(failure ?? "scope_traversal_incomplete") }
        guard uptimeNs() < deadline else { return failed("scope_time_budget_exceeded") }
        guard ProcessStamp.read(pid: app.pid) == initialProcess else { return failed("scope_process_changed") }
        guard uptimeNs() < deadline else { return failed("scope_time_budget_exceeded") }
        return PendingTableScopeSearch(request: request, processID: app.pid, windowID: fixedWindowID,
            window: focusedAXWindow, candidates: candidates, prunableTables: prunableTables,
            incompleteReason: nil,
            processStamp: initialProcess)
    }

    func finalizeTableScopeSearch(_ pending: PendingTableScopeSearch?, structure: Structure,
                                  boundaryStable: Bool) -> TableScopeSearchResult? {
        guard let pending else { return nil }
        guard pending.incompleteReason == nil, boundaryStable else {
            return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                windowID: pending.windowID, candidates: [], traversalComplete: false,
                viewportCrossLinksComplete: false, boundaryStable: boundaryStable,
                incompleteReason: pending.incompleteReason ?? "source_window_or_process_changed")
        }

        func ref(_ element: AXUIElement) -> String? {
            observedElements.first(where: { CFEqual($0.element, element) })?.id
        }
        var objectsByRef: [String: UiObject] = [:]
        for object in objects {
            guard objectsByRef[object.instance_ref] == nil else {
                return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                    windowID: pending.windowID, candidates: [], traversalComplete: false,
                    viewportCrossLinksComplete: false, boundaryStable: true,
                    incompleteReason: "duplicate_source_instance_reference")
            }
            objectsByRef[object.instance_ref] = object
        }
        guard let activeWindowRef = ref(pending.window),
              structure.app.pid == pending.processID,
              structure.active_layer == pending.windowID,
              structure.windows.contains(where: { $0.window_id == pending.windowID }),
              objectsByRef[activeWindowRef].map({ $0.role == kAXWindowRole
                  && $0.window_id == pending.windowID }) == true else {
            return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                windowID: pending.windowID, candidates: [], traversalComplete: false,
                viewportCrossLinksComplete: false, boundaryStable: false,
                incompleteReason: "scope_window_reference_missing")
        }

        var projected: [TableScopeSearchCandidate] = []
        for pendingCandidate in pending.candidates {
            guard let tableRef = ref(pendingCandidate.table),
                  let scrollRef = ref(pendingCandidate.scrollContainer),
                  let tableObject = objectsByRef[tableRef], tableObject.role == kAXTableRole,
                  tableObject.window_id == pending.windowID,
                  tableObject.parent == scrollRef,
                  let viewport = structure.table_viewports.first(where: { $0.table_instance_ref == tableRef }),
                  viewport.scroll_container_instance_ref == scrollRef,
                  viewport.visible_rows_complete, viewport.columns_complete,
                  viewport.visible_rows.allSatisfy(\.cells_complete),
                  TableScopeSearchValidation.headersMatch(
                    viewport.columns.map(\.header).compactMap({ $0 }),
                    requested: pending.request.column_headers),
                  TableScopeSearchValidation.headersMatch(
                    pendingCandidate.columnHeaders, requested: pending.request.column_headers) else {
                return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                    windowID: pending.windowID, candidates: [], traversalComplete: false,
                    viewportCrossLinksComplete: false, boundaryStable: true,
                    incompleteReason: "table_viewport_cross_link_incomplete")
            }
            let path = pendingCandidate.path.compactMap { step -> TableScopeSearchPathItem? in
                guard let id = ref(step.element), let object = objectsByRef[id],
                      object.role == kAXScrollAreaRole, object.window_id == pending.windowID,
                      object.description == step.label, object.frame == step.frame else { return nil }
                return TableScopeSearchPathItem(instance_ref: id, label: step.label, frame: step.frame)
            }
            guard path.count == pending.request.scroll_path.count,
                  path.map(\.label) == pending.request.labels,
                  path.last?.instance_ref == scrollRef else {
                return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                    windowID: pending.windowID, candidates: [], traversalComplete: false,
                    viewportCrossLinksComplete: false, boundaryStable: true,
                    incompleteReason: "table_scroll_path_cross_link_incomplete")
            }
            var actualAncestorPath: [String] = []
            var current = tableObject
            var visited = Set<String>()
            while current.instance_ref != activeWindowRef {
                guard visited.insert(current.instance_ref).inserted,
                      current.window_id == pending.windowID,
                      let parentRef = current.parent,
                      let parent = objectsByRef[parentRef] else {
                    return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                        windowID: pending.windowID, candidates: [], traversalComplete: false,
                        viewportCrossLinksComplete: false, boundaryStable: true,
                        incompleteReason: "table_parent_path_cross_link_incomplete")
                }
                if current.role == kAXScrollAreaRole { actualAncestorPath.append(current.instance_ref) }
                current = parent
            }
            guard current.window_id == pending.windowID,
                  actualAncestorPath.reversed().elementsEqual(path.map(\.instance_ref)) else {
                return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
                    windowID: pending.windowID, candidates: [], traversalComplete: false,
                    viewportCrossLinksComplete: false, boundaryStable: true,
                    incompleteReason: "table_parent_path_cross_link_incomplete")
            }
            projected.append(TableScopeSearchCandidate(table_instance_ref: tableRef,
                scroll_container_instance_ref: scrollRef, scroll_path: path,
                column_headers: pendingCandidate.columnHeaders))
        }
        return TableScopeSearchValidation.result(request: pending.request, processID: pending.processID,
            windowID: pending.windowID, candidates: projected, traversalComplete: true,
            viewportCrossLinksComplete: true, boundaryStable: true, incompleteReason: nil)
    }

    private func tableElementForScope(_ element: AXUIElement, attribute name: String) -> AXUIElement? {
        guard uptimeNs() < deadline else { return nil }
        var raw: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &raw) == .success,
              let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        let result = unsafeDowncast(raw, to: AXUIElement.self)
        var pid: pid_t = 0
        guard AXUIElementGetPid(result, &pid) == .success, pid > 0, UInt32(pid) == app.pid else { return nil }
        return result
    }

    private static func scopeChildrenMayBeEmpty(for role: String) -> Bool {
        ["AXButton", "AXCheckBox", "AXRadioButton", "AXStaticText", "AXTextField",
         "AXTextArea", "AXPopUpButton", "AXMenuButton", "AXMenuItem", "AXSlider", "AXSwitch"]
            .contains(role)
    }

    private func scopeString(_ element: AXUIElement, _ name: String) -> String? {
        guard uptimeNs() < deadline else { return nil }
        var raw: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &raw) == .success,
              let value = raw as? String, value.utf8.count <= 2_048,
              !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            return nil
        }
        return value
    }

    private func scopeBoolean(_ element: AXUIElement, _ name: String) -> Bool? {
        guard uptimeNs() < deadline else { return nil }
        var raw: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &raw) == .success,
              let raw, CFGetTypeID(raw) == CFBooleanGetTypeID() else { return nil }
        return (raw as? NSNumber)?.boolValue
    }

    private func scopeFrame(_ element: AXUIElement) -> Rect? {
        guard uptimeNs() < deadline else { return nil }
        var position: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXPositionAttribute as CFString, &position) == .success,
              let position, CFGetTypeID(position) == AXValueGetTypeID(), uptimeNs() < deadline else { return nil }
        var size: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXSizeAttribute as CFString, &size) == .success,
              let size, CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        var extent = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(position, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(size, to: AXValue.self), .cgSize, &extent) else { return nil }
        let result = Rect(CGRect(origin: point, size: extent))
        guard result.valid, result.width > 0, result.height > 0 else { return nil }
        return result
    }
}
