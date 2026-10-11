import Foundation
import ApplicationServices
import CoreGraphics

struct KernelProcessIdentity: Encodable, Sendable, Equatable {
    let pid: UInt32
    let start_seconds: UInt64
    let start_micros: UInt64
}

struct MenuDrawingWindow: Encodable, Sendable, Equatable {
    let window_id: UInt32
    let owner_pid: UInt32
    let layer: Int32
    let frame: Rect
}

enum MenuAnchorContinuity: String, Encodable, Sendable { case directRead = "direct_read", observedLifetime = "observed_lifetime" }

/// Same field projection and root-first ancestry as Rust instance_fingerprint.
/// The digest is still calculated by the Rust contracts from this typed value.
struct MenuFingerprint: Encodable, Sendable, Equatable {
    let app: String
    let app_pid: UInt32
    let window_id: String
    let ancestor_roles: [String]
    let group: String?
    let role: String
    let subrole: String?
    let label: String
    let sibling_index: UInt32
    let frame: [Int64]
    let scroll_container: String? = nil
}

struct MenuOpenerAnchor: Encodable, Sendable {
    let anchor_id: String
    let source_session_id: String
    let anchor_revision: UInt64
    let process: KernelProcessIdentity
    let origin_window_id: String
    let origin_window_frame: Rect
    let opener_instance: String
    let opener_fingerprint: MenuFingerprint
    let opener_frame: Rect
    var verified_source_sequence: UInt64
    var verified_observed_ms: UInt64
    let continuity: MenuAnchorContinuity
}

struct MenuContext: Encodable, Sendable {
    let context_id: String
    let source_session_id: String
    var opener: MenuOpenerAnchor
    let menu_instance: String
    let menu_fingerprint: MenuFingerprint
    let menu_frame: Rect
    let drawing_window: MenuDrawingWindow
}

func menuNormalizeLabel(_ text: String) -> String {
    text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ").lowercased()
}

func menuFingerprint(object: UiObject, structure: Structure) -> MenuFingerprint? {
    var ancestors: [String] = []
    var seen = Set<String>()
    var cursor = object.parent
    while let id = cursor {
        guard seen.insert(id).inserted, ancestors.count < 32,
              let parent = structure.objects.first(where: { $0.instance_ref == id }) else { return nil }
        ancestors.append(parent.role); cursor = parent.parent
    }
    let siblings = structure.objects.filter { $0.parent == object.parent && $0.window_id == object.window_id }
    guard let index = siblings.firstIndex(where: { $0.instance_ref == object.instance_ref }), index < 256,
          object.frame.valid, object.frame.width > 0, object.frame.height > 0 else { return nil }
    let frame = [object.frame.x, object.frame.y, object.frame.width, object.frame.height]
    guard frame.allSatisfy({ (-100_000...100_000).contains($0) }) else { return nil }
    return MenuFingerprint(app: structure.app.bundle_id, app_pid: structure.app.pid, window_id: object.window_id,
        ancestor_roles: ancestors.reversed(), group: object.group.map(menuNormalizeLabel), role: object.role,
        subrole: object.subrole, label: menuNormalizeLabel(object.title ?? object.description ?? ""),
        sibling_index: UInt32(index), frame: frame.map { Int64($0.rounded()) })
}

/// Only trusted, previously registered fixed PID/window policy enables this
/// profile. A one-shot observe never retains an opener across an input.
func menuOriginWindow(request: NativeRequest, app: AppIdentity) -> String? {
    guard request.command == "watch", request.pid == app.pid,
          let policy = request.text_policy, policy.valid(bundle: app.bundle_id, pid: app.pid) else { return nil }
    return policy.window_id
}

func nativeMenuDrawingWindows(pid: UInt32) -> [MenuDrawingWindow] {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] else { return [] }
    return list.compactMap { info in
        guard let owner = info[kCGWindowOwnerPID as String] as? NSNumber, owner.uint32Value == pid,
              let layer = info[kCGWindowLayer as String] as? NSNumber, layer.int32Value == 101,
              let id = info[kCGWindowNumber as String] as? NSNumber, id.uint32Value > 0,
              let alpha = info[kCGWindowAlpha as String] as? NSNumber, alpha.doubleValue > 0,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let cgRect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return nil }
        let frame = Rect(cgRect)
        guard frame.valid, frame.width > 0, frame.height > 0 else { return nil }
        return MenuDrawingWindow(window_id: id.uint32Value, owner_pid: owner.uint32Value, layer: 101, frame: frame)
    }
}

func uniqueMenuDrawingWindow(menuFrame: Rect, itemFrames: [Rect], windows: [MenuDrawingWindow], pid: UInt32) -> MenuDrawingWindow? {
    let matches = windows.filter { window in
        window.owner_pid == pid && window.layer == 101 && window.frame == menuFrame &&
        !itemFrames.isEmpty && itemFrames.allSatisfy { item in
            item.valid && item.width > 0 && item.height > 0 &&
            window.frame.cgRect.contains(CGPoint(x: item.x + item.width / 2, y: item.y + item.height / 2))
        }
    }
    return matches.count == 1 ? matches.first : nil
}

/// CF references and direct-query helpers are worker-local, never Sendable.
/// Retention prevents address reuse from being treated as platform continuity.
final class MenuRetainedToken {
    let element: CFTypeRef
    init(_ element: CFTypeRef) { self.element = element }
    func matches(_ candidate: CFTypeRef) -> Bool { CFEqual(element, candidate) }
}

final class MenuPresentationIdentity {
    private(set) var menu: MenuRetainedToken?
    private(set) var contextID: String?
    func observe(_ element: CFTypeRef) -> String {
        if let menu, let contextID, menu.matches(element) { return contextID }
        menu = MenuRetainedToken(element)
        let next = "menu-context-\(UUID().uuidString.lowercased())"
        contextID = next
        return next
    }
    func close() { menu = nil; contextID = nil }
}

/// Notification retirement is irreversible, even if a later direct read sees
/// the original frame again. A new anchor must get a new ID and revision.
final class MenuAnchorEventState {
    let id: String
    let revision: UInt64
    let path: [MenuRetainedToken]
    let root: MenuRetainedToken
    let presentation = MenuPresentationIdentity()
    private(set) var retired = false
    init(id: String, revision: UInt64, path: [MenuRetainedToken], root: MenuRetainedToken) {
        self.id = id; self.revision = revision; self.path = path; self.root = root
    }
    func retire() { retired = true; presentation.close() }
}

/// Pure CF identity routing shared by the real callback and offline tests.
func applyMenuAnchorNotification(_ name: String, element: CFTypeRef,
                                 focusedWindow: CFTypeRef?, states: [MenuAnchorEventState]) {
    if name == kAXMenuClosedNotification {
        for state in states { state.presentation.close() }
    } else if [kAXUIElementDestroyedNotification, kAXWindowMovedNotification, kAXWindowResizedNotification].contains(name) {
        let affected = states.filter { state in
            state.path.contains { $0.matches(element) } ||
                name == kAXUIElementDestroyedNotification && state.presentation.menu?.matches(element) == true
        }
        // An unattributed mutation may belong to any protected retained path.
        for state in affected.isEmpty ? states : affected { state.retire() }
    } else if name == kAXFocusedWindowChangedNotification {
        for state in states {
            guard let focusedWindow, state.root.matches(focusedWindow) else { state.retire(); continue }
        }
    } else if ![kAXValueChangedNotification, kAXTitleChangedNotification, kAXLayoutChangedNotification,
                kAXFocusedUIElementChangedNotification, kAXWindowCreatedNotification,
                kAXApplicationActivatedNotification, kAXApplicationDeactivatedNotification,
                kAXSheetCreatedNotification, kAXMenuOpenedNotification].contains(name) {
        for state in states { state.retire() }
    }
}

/// Private Source evidence, separate from the diagnostic history/category.
struct OtherWindowDestructionProof {
    let sessionID: String
    let pid: UInt32
    let process: ProcessStamp
    let window: MenuRetainedToken
    let originRoot: MenuRetainedToken
    let verifiedNs: UInt64
}

enum OtherWindowProofDecision: String, Encodable {
    case consumed, missing, ambiguous, session, process, clock, expired, origin, protectedScope = "protected_scope"
}
enum OtherWindowRelationFailure: String, Encodable {
    case budget, process, application, membership, overflow, parent, role, modal, subtree
    case postcheck, incomplete
}

/// No fingerprint, window size or label can substitute for the retained CF.
final class OtherWindowDestructionProofRegistry {
    static let lifetimeNs: UInt64 = 2_000_000_000
    private struct Slot {
        let token: MenuRetainedToken
        var proof: OtherWindowDestructionProof?
        var ambiguous: Bool
    }
    private var slots: [Slot] = []
    var count: Int { slots.count }
    func commit(_ candidates: [OtherWindowDestructionProof], complete: Bool) {
        guard complete else { return }
        for proof in candidates {
            if let index = slots.firstIndex(where: { $0.token.matches(proof.window.element) }) {
                guard !slots[index].ambiguous,
                      proof.verifiedNs >= (slots[index].proof?.verifiedNs ?? 0) else { continue }
                slots[index].proof = proof
            } else if slots.count < 8 {
                slots.append(Slot(token: proof.window, proof: proof, ambiguous: false))
            }
        }
    }
    func markConflict(_ element: CFTypeRef) {
        if let index = slots.firstIndex(where: { $0.token.matches(element) }) {
            slots[index].ambiguous = true; slots[index].proof = nil
        }
    }
    func remove(_ element: CFTypeRef) {
        if let index = slots.firstIndex(where: { $0.token.matches(element) }) { slots[index].proof = nil }
    }
    func clear() { slots.removeAll() }
    func consume(_ element: CFTypeRef, sessionID: String, pid: UInt32, process: ProcessStamp?,
                 now: UInt64, originRoots: [MenuRetainedToken], protectedPaths: [[MenuRetainedToken]],
                 liveMenu: [MenuRetainedToken]) -> Bool {
        consumeDecision(element, sessionID: sessionID, pid: pid, process: process, now: now,
            originRoots: originRoots, protectedPaths: protectedPaths, liveMenu: liveMenu) == .consumed
    }
    func consumeDecision(_ element: CFTypeRef, sessionID: String, pid: UInt32, process: ProcessStamp?,
                         now: UInt64, originRoots: [MenuRetainedToken], protectedPaths: [[MenuRetainedToken]],
                         liveMenu: [MenuRetainedToken]) -> OtherWindowProofDecision {
        guard let index = slots.firstIndex(where: { $0.token.matches(element) }) else { return .missing }
        let slot = slots.remove(at: index) // One event only, including failed checks.
        guard !slot.ambiguous else { return .ambiguous }
        guard let proof = slot.proof else { return .missing }
        guard proof.sessionID == sessionID else { return .session }
        guard proof.pid == pid, proof.process == process else { return .process }
        guard proof.verifiedNs > 0, now >= proof.verifiedNs else { return .clock }
        guard now - proof.verifiedNs <= Self.lifetimeNs else { return .expired }
        guard !originRoots.isEmpty, originRoots.allSatisfy({ proof.originRoot.matches($0.element) }) else { return .origin }
        guard !originRoots.contains(where: { $0.matches(element) }),
              !protectedPaths.contains(where: { $0.contains { $0.matches(element) } }),
              !liveMenu.contains(where: { $0.matches(element) }) else { return .protectedScope }
        return .consumed
    }
}

func otherWindowModalBoolean(_ value: CFTypeRef?) -> Bool? {
    guard let value, CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
    return CFBooleanGetValue(unsafeDowncast(value, to: CFBoolean.self))
}

/// The actual role matters; AXWindow with AXDialog subrole is permitted only
/// with explicit nonmodal status and the independently proved application parent.
func otherWindowRelationsEligible(role: String?, modal: Bool?, parentRole: String?,
                                  parentMatches: Bool, uniqueMembership: Bool,
                                  processMatches: Bool, subtreeDisjoint: Bool) -> Bool {
    role == kAXWindowRole && modal == false && parentRole == kAXApplicationRole &&
        parentMatches && uniqueMembership && processMatches && subtreeDisjoint
}

/// Fixed metadata only: never serialize AX content or a CF address.
enum MenuAnchorDiagnosticCondition: String, Encodable {
    case notification, explicitInvalidation = "explicit_invalidation"
    case readUnavailable = "read_unavailable", processUnavailable = "process_unavailable"
    case processChanged = "process_changed", traversalWindowsChanged = "traversal_windows_changed"
    case sessionClosed = "session_closed", projectGate = "project_gate"
    case projectReaderPartial = "project_reader_partial", projectUnownedDrawing = "project_unowned_drawing"
    case projectMenuRelation = "project_menu_relation", projectFocusedRelation = "project_focused_relation"
    case projectItems = "project_items", projectDrawing = "project_drawing"
    case retired, processIdentity = "process_identity", originWindow = "origin_window"
    case originFrame = "origin_frame", originModal = "origin_modal", activeLayer = "active_layer"
    case openerMissing = "opener_missing", openerEnabled = "opener_enabled", openerInView = "opener_in_view"
    case pathRevalidation = "path_revalidation", pathCount = "path_count", retainedCF = "retained_cf"
    case fingerprint, pathFrame = "path_frame", rootCF = "root_cf"
    case directQuery = "direct_query", parentMembership = "parent_membership"
    case independentWindowDestruction = "independent_window_destruction"
    case otherWindowProofRegistered = "other_window_proof_registered"
    case otherWindowProofUnavailable = "other_window_proof_unavailable"
    case otherWindowProofModal = "other_window_proof_modal"
    case otherWindowProofDisjoint = "other_window_proof_disjoint"
}

enum MenuAnchorDiagnosticNotification: String, Encodable {
    case destroyed, moved, resized, focusedWindow = "focused_window", menuClosed = "menu_closed"
    case value, title, layout, focusedElement = "focused_element", windowCreated = "window_created"
    case activated, deactivated, sheetCreated = "sheet_created", menuOpened = "menu_opened", unknown
    init(_ name: String) {
        switch name {
        case kAXUIElementDestroyedNotification: self = .destroyed
        case kAXWindowMovedNotification: self = .moved
        case kAXWindowResizedNotification: self = .resized
        case kAXFocusedWindowChangedNotification: self = .focusedWindow
        case kAXMenuClosedNotification: self = .menuClosed
        case kAXValueChangedNotification: self = .value
        case kAXTitleChangedNotification: self = .title
        case kAXLayoutChangedNotification: self = .layout
        case kAXFocusedUIElementChangedNotification: self = .focusedElement
        case kAXWindowCreatedNotification: self = .windowCreated
        case kAXApplicationActivatedNotification: self = .activated
        case kAXApplicationDeactivatedNotification: self = .deactivated
        case kAXSheetCreatedNotification: self = .sheetCreated
        case kAXMenuOpenedNotification: self = .menuOpened
        default: self = .unknown
        }
    }
}

enum MenuHistoryIdentityCategory: String, Encodable {
    case closedMenu = "closed_menu", closedDirectItem = "closed_direct_item"
    case knownOtherWindow = "known_other_window", unknown
}

/// Diagnostic retention only. Nothing in anchor validation reads this catalog.
/// The first 32 actual references stay retained until watch close; overflow
/// simply leaves an event unknown. Ordinary registry resets never clear it.
final class MenuHistoryDiagnostics {
    private enum Kind { case menu, item, window }
    private struct Candidate {
        let token: MenuRetainedToken
        let kind: Kind
        var contextID: String?
        var closed: Bool
        var ambiguous: Bool
    }
    private var candidates: [Candidate] = []
    var count: Int { candidates.count }
    var windowCount: Int { candidates.filter { $0.kind == .window }.count }

    private func retain(_ element: CFTypeRef, kind: Kind, contextID: String? = nil) {
        if let index = candidates.firstIndex(where: { $0.token.matches(element) }) {
            // A conflicting observed role can never yield a known category.
            guard candidates[index].kind == kind else { candidates[index].ambiguous = true; return }
            if kind != .window {
                candidates[index].contextID = contextID; candidates[index].closed = false
            }
            return
        }
        guard candidates.count < 32, kind != .window || windowCount < 8 else { return }
        candidates.append(Candidate(token: MenuRetainedToken(element), kind: kind, contextID: contextID, closed: false, ambiguous: false))
    }

    func observeMenu(_ element: CFTypeRef, directItems: [CFTypeRef], contextID: String) {
        retain(element, kind: .menu, contextID: contextID)
        // Never grow one context's item history beyond eight, even across reads.
        for item in directItems.prefix(8) {
            let matching = candidates.contains { $0.kind == .item && $0.contextID == contextID && $0.token.matches(item) }
            let itemCount = candidates.filter { $0.kind == .item && $0.contextID == contextID }.count
            if matching || itemCount < 8 { retain(item, kind: .item, contextID: contextID) }
        }
    }
    func observeOtherWindow(_ element: CFTypeRef) { retain(element, kind: .window) }
    func closeMenus() {
        for index in candidates.indices where candidates[index].kind != .window { candidates[index].closed = true }
    }
    func classify(_ element: CFTypeRef, protectedPaths: [[MenuRetainedToken]]) -> MenuHistoryIdentityCategory {
        // Classification cannot override a protected path match, even when the
        // same reference also appears in old diagnostic history.
        guard !protectedPaths.contains(where: { $0.contains { $0.matches(element) } }),
              let candidate = candidates.first(where: { $0.token.matches(element) }), !candidate.ambiguous else { return .unknown }
        switch candidate.kind {
        case .window: return .knownOtherWindow
        case .menu: return candidate.closed ? .closedMenu : .unknown
        case .item: return candidate.closed ? .closedDirectItem : .unknown
        }
    }
    func clear() { candidates.removeAll() }
}

struct MenuAnchorDiagnosticRecord: Encodable {
    let kind = "menu_anchor_diagnostic"
    let source_session_id: String
    let ordinal: Int
    let uptime_ns: UInt64
    let condition: MenuAnchorDiagnosticCondition
    let notification: MenuAnchorDiagnosticNotification?
    let anchor_count: Int
    let affected_count: Int?
    let retired_count: Int?
    let known_root_cf_match: Bool?
    let path_index: Int?
    let destroyed_identity: MenuHistoryIdentityCategory?
    let other_window_proof_decision: OtherWindowProofDecision?
    let other_window_relation_failure: OtherWindowRelationFailure?
}

/// Worker-local and bounded for the lifetime of the watch, including resets.
final class MenuAnchorDiagnostics {
    private(set) var count = 0
    private let sessionID: String
    private let write: (Data) -> Void
    init(sessionID: String, write: @escaping (Data) -> Void = { try? FileHandle.standardError.write(contentsOf: $0) }) {
        self.sessionID = sessionID; self.write = write
    }
    func record(_ condition: MenuAnchorDiagnosticCondition, anchors: Int,
                notification: MenuAnchorDiagnosticNotification? = nil, affected: Int? = nil,
                retired: Int? = nil, rootMatch: Bool? = nil, pathIndex: Int? = nil,
                destroyedIdentity: MenuHistoryIdentityCategory? = nil,
                proofDecision: OtherWindowProofDecision? = nil, relationFailure: OtherWindowRelationFailure? = nil) {
        guard anchors > 0, count < 32 else { return }
        count += 1
        let record = MenuAnchorDiagnosticRecord(source_session_id: sessionID, ordinal: count, uptime_ns: uptimeNs(),
            condition: condition, notification: notification, anchor_count: anchors, affected_count: affected,
            retired_count: retired, known_root_cf_match: rootMatch, path_index: pathIndex, destroyed_identity: destroyedIdentity, other_window_proof_decision: proofDecision, other_window_relation_failure: relationFailure)
        guard var data = try? JSONEncoder().encode(record) else { return }
        data.append(0x0a); write(data)
    }
}

final class MenuAnchorRegistry {
    private struct ProtectedNode {
        let retained: MenuRetainedToken
        let fingerprint: MenuFingerprint
        let frame: Rect
    }
    private final class Entry {
        let events: MenuAnchorEventState
        var id: String { events.id }
        var revision: UInt64 { events.revision }
        let process: ProcessStamp
        let root: MenuRetainedToken
        let window: WindowInfo
        let path: [ProtectedNode] // opener -> ... -> actual standard root
        var presentation: MenuPresentationIdentity { events.presentation }
        init(id: String, revision: UInt64, process: ProcessStamp, root: AXUIElement,
             window: WindowInfo, path: [ProtectedNode]) {
            events = MenuAnchorEventState(id: id, revision: revision, path: path.map(\.retained), root: MenuRetainedToken(root))
            self.process = process
            self.root = MenuRetainedToken(root); self.window = window; self.path = path
        }
    }
    let sessionID = UUID().uuidString.lowercased()
    private lazy var diagnostics = MenuAnchorDiagnostics(sessionID: sessionID)
    private var revision: UInt64 = 0
    private var entries: [Entry] = []
    private let historyDiagnostics = MenuHistoryDiagnostics()
    private let otherWindowProofs = OtherWindowDestructionProofRegistry()
    // Current proved menu scope, bounded by the Source traversal's node limit.
    private var liveMenuScope: [MenuRetainedToken] = []
    var count: Int { entries.count }

    func invalidateAll(reason: MenuAnchorDiagnosticCondition = .explicitInvalidation) {
        diagnostics.record(reason, anchors: entries.count, retired: entries.count)
        for entry in entries { entry.events.retire() }
        entries.removeAll()
        otherWindowProofs.clear(); liveMenuScope.removeAll()
        if [.sessionClosed, .processUnavailable, .processChanged].contains(reason) { historyDiagnostics.clear() }
    }

    func notified(element: AXUIElement, name: String) {
        let focused = name == kAXFocusedWindowChangedNotification ? focusedWindowAtNotification(element) : nil
        let destroyedIdentity = name == kAXUIElementDestroyedNotification
            ? historyDiagnostics.classify(element, protectedPaths: entries.map { $0.events.path }) : nil
        if name == kAXMenuClosedNotification { historyDiagnostics.closeMenus(); liveMenuScope.removeAll() }
        let affected = entries.filter { entry in
            entry.events.path.contains { $0.matches(element) } ||
                name == kAXUIElementDestroyedNotification && entry.presentation.menu?.matches(element) == true
        }.count
        let rootMatch = focused.map { candidate in entries.contains { $0.root.matches(candidate) } }
        var proofDecision: OtherWindowProofDecision? = name == kAXUIElementDestroyedNotification ? .protectedScope : nil
        var independentlyProvedDestruction = false
        if name == kAXUIElementDestroyedNotification, affected == 0,
           let first = entries.first, let opener = first.path.first,
           entries.allSatisfy({ $0.process == first.process }) {
            let pid = opener.fingerprint.app_pid
            proofDecision = otherWindowProofs.consumeDecision(element, sessionID: sessionID, pid: pid,
                process: ProcessStamp.read(pid: pid), now: uptimeNs(), originRoots: entries.map(\.root),
                protectedPaths: entries.map { $0.events.path }, liveMenu: liveMenuScope + entries.compactMap { $0.presentation.menu })
            independentlyProvedDestruction = proofDecision == .consumed
        }
        if !independentlyProvedDestruction {
            applyMenuAnchorNotification(name, element: element, focusedWindow: focused, states: entries.map(\.events))
        }
        diagnostics.record(independentlyProvedDestruction ? .independentWindowDestruction : .notification, anchors: entries.count, notification: MenuAnchorDiagnosticNotification(name),
            affected: affected, retired: entries.filter { $0.events.retired }.count, rootMatch: rootMatch, destroyedIdentity: destroyedIdentity, proofDecision: proofDecision)
        if entries.contains(where: { $0.events.retired }) { otherWindowProofs.clear(); liveMenuScope.removeAll() }
        entries.removeAll { $0.events.retired }
    }

    /// Optional evidence queries share the traversal deadline. Failure only
    /// disables this narrow exception, never repairs an incomplete Source read.
    func prepareOtherWindowProofs(reader: AXReader, structure: Structure, process: ProcessStamp) -> [OtherWindowDestructionProof] {
        func rejected(_ reason: OtherWindowRelationFailure, clear: Bool = false) -> [OtherWindowDestructionProof] {
            diagnostics.record(.otherWindowProofUnavailable, anchors: entries.count, relationFailure: reason)
            if clear { otherWindowProofs.clear() }
            return []
        }
        guard structure.complete else { return rejected(.incomplete) }
        guard let origin = menuOriginWindow(request: reader.request, app: reader.app),
              structure.active_layer == origin, let entry = entries.first,
              entries.allSatisfy({ $0.process == process && $0.root.matches(entry.root.element) }),
              ProcessStamp.read(pid: reader.app.pid) == process else { return rejected(.process, clear: true) }
        let application = AXUIElementCreateApplication(pid_t(reader.app.pid))
        func configure(_ element: AXUIElement) -> Bool {
            let now = uptimeNs()
            guard now < reader.deadline else { return false }
            let seconds = min(0.04, Double(reader.deadline - now) / 1_000_000_000)
            return AXUIElementSetMessagingTimeout(element, Float(seconds)) == .success && uptimeNs() < reader.deadline
        }
        func value(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
            guard configure(element) else { return nil }
            var result: CFTypeRef?
            guard AXUIElementCopyAttributeValue(element, name as CFString, &result) == .success,
                  uptimeNs() < reader.deadline else { return nil }
            return result
        }
        func samePID(_ element: AXUIElement) -> Bool {
            guard configure(element) else { return false }
            var pid: pid_t = 0
            return AXUIElementGetPid(element, &pid) == .success && pid > 0 && UInt32(pid) == reader.app.pid && uptimeNs() < reader.deadline
        }
        guard samePID(application), value(application, kAXRoleAttribute) as? String == kAXApplicationRole else {
            return rejected(uptimeNs() >= reader.deadline ? .budget : .application, clear: true)
        }
        var windowCount = 0
        guard configure(application),
              AXUIElementGetAttributeValueCount(application, kAXWindowsAttribute as CFString, &windowCount) == .success,
              windowCount > 0, windowCount <= 32, uptimeNs() < reader.deadline else {
            return rejected(uptimeNs() >= reader.deadline ? .budget : windowCount > 32 ? .overflow : .membership, clear: true)
        }
        var windowValues: CFArray?
        guard configure(application), AXUIElementCopyAttributeValues(application, kAXWindowsAttribute as CFString, 0, windowCount, &windowValues) == .success,
              let actualWindows = windowValues as? [AXUIElement], actualWindows.count == windowCount,
              uptimeNs() < reader.deadline else {
            return rejected(uptimeNs() >= reader.deadline ? .budget : .membership, clear: true)
        }
        let protected = entries.flatMap { $0.events.path } + entries.compactMap { $0.presentation.menu }
        let menuScope = structure.objects.filter { $0.role == kAXMenuRole || $0.role == kAXMenuItemRole }
            .compactMap { element($0.instance_ref, reader: reader) }
        var pending: [OtherWindowDestructionProof] = []
        for object in structure.objects where object.role == kAXWindowRole && object.parent == nil && object.window_id != origin {
            guard let actual = element(object.instance_ref, reader: reader) else { continue }
            // Invalidate stale evidence if the actual CF is observed with a
            // conflicting role anywhere in the current complete traversal.
            let subtree = structure.objects.filter { $0.window_id == object.window_id }
            let refs = subtree.compactMap { element($0.instance_ref, reader: reader) }
            let disjoint = refs.count == subtree.count && !refs.isEmpty &&
                !subtree.contains(where: { [kAXMenuRole, kAXMenuItemRole, kAXSheetRole, "AXDialog", "AXPopover"].contains($0.role) }) &&
                refs.allSatisfy { ref in !protected.contains { $0.matches(ref) } && !menuScope.contains { CFEqual($0, ref) } }
            guard pending.count < 8, samePID(actual),
                  let parent = value(actual, kAXParentAttribute), CFGetTypeID(parent) == AXUIElementGetTypeID() else {
                diagnostics.record(.otherWindowProofUnavailable, anchors: entries.count,
                    relationFailure: uptimeNs() >= reader.deadline ? .budget : pending.count >= 8 ? .overflow : .parent)
                otherWindowProofs.remove(actual); continue
            }
            let parentAX = unsafeDowncast(parent, to: AXUIElement.self)
            let actualRole = value(actual, kAXRoleAttribute) as? String
            let actualModal = otherWindowModalBoolean(value(actual, kAXModalAttribute))
            let parentRole = value(parentAX, kAXRoleAttribute) as? String
            let parentMatches = CFEqual(parent, application)
            let uniqueMember = actualWindows.filter { CFEqual($0, actual) }.count == 1
            let processMatches = samePID(parentAX) && ProcessStamp.read(pid: reader.app.pid) == process
            let eligible = otherWindowRelationsEligible(role: actualRole,
                modal: actualModal, parentRole: parentRole, parentMatches: parentMatches,
                uniqueMembership: uniqueMember, processMatches: processMatches, subtreeDisjoint: disjoint)
            guard eligible, uptimeNs() < reader.deadline else {
                let reason: OtherWindowRelationFailure = uptimeNs() >= reader.deadline ? .budget :
                    actualRole != kAXWindowRole ? .role : actualModal != false ? .modal :
                    parentRole != kAXApplicationRole || !parentMatches ? .parent : !uniqueMember ? .membership :
                    !processMatches ? .process : .subtree
                diagnostics.record(.otherWindowProofUnavailable, anchors: entries.count, relationFailure: reason)
                otherWindowProofs.remove(actual); continue
            }
            pending.append(OtherWindowDestructionProof(sessionID: sessionID, pid: reader.app.pid, process: process,
                window: MenuRetainedToken(actual), originRoot: entry.root, verifiedNs: uptimeNs()))
        }
        for object in structure.objects where object.role != kAXWindowRole {
            if let ref = element(object.instance_ref, reader: reader) { otherWindowProofs.markConflict(ref) }
        }
        guard ProcessStamp.read(pid: reader.app.pid) == process else { return rejected(.process, clear: true) }
        guard uptimeNs() < reader.deadline else { return rejected(.budget, clear: true) }
        return pending
    }

    func rejectOtherWindowProofPostcheck(_ pending: [OtherWindowDestructionProof]) {
        if !pending.isEmpty { diagnostics.record(.otherWindowProofUnavailable, anchors: entries.count, relationFailure: .postcheck) }
    }

    func commitOtherWindowProofs(_ pending: [OtherWindowDestructionProof], reader: AXReader, structure: Structure) {
        guard structure.complete else { return }
        // Diagnostic history is never a condition for this independent proof.
        otherWindowProofs.commit(pending, complete: true)
        if !pending.isEmpty { diagnostics.record(.otherWindowProofRegistered, anchors: entries.count, affected: pending.count) }
        liveMenuScope = structure.objects.filter { $0.role == kAXMenuRole || $0.role == kAXMenuItemRole }
            .compactMap { element($0.instance_ref, reader: reader).map(MenuRetainedToken.init) }
    }

    /// Called only after the complete traversal and post-read checks succeed.
    /// It reuses actual read references and never queries a destroyed element.
    func retainCompleteReadDiagnostics(reader: AXReader, structure: Structure) {
        guard structure.complete, let origin = menuOriginWindow(request: reader.request, app: reader.app) else { return }
        if let context = structure.menu_context,
           let menu = element(context.menu_instance, reader: reader) {
            let items: [CFTypeRef] = structure.objects.filter {
                $0.parent == context.menu_instance && $0.role == kAXMenuItemRole
            }.prefix(8).compactMap { element($0.instance_ref, reader: reader) }
            historyDiagnostics.observeMenu(menu, directItems: items, contextID: context.context_id)
        } else if !structure.objects.contains(where: { $0.role == kAXMenuRole }) {
            historyDiagnostics.closeMenus()
        }
        for object in structure.objects where object.role == kAXWindowRole && object.parent == nil && object.window_id != origin {
            guard let actual = element(object.instance_ref, reader: reader),
                  !entries.contains(where: { $0.events.path.contains { $0.matches(actual) } }) else { continue }
            historyDiagnostics.observeOtherWindow(actual)
        }
    }

    private func focusedWindowAtNotification(_ element: AXUIElement) -> CFTypeRef? {
        guard let entry = entries.first, let opener = entry.path.first else { return nil }
        let expectedPID = opener.fingerprint.app_pid
        var actualPID: pid_t = 0
        guard AXUIElementGetPid(element, &actualPID) == .success, actualPID > 0, UInt32(actualPID) == expectedPID,
              ProcessStamp.read(pid: expectedPID) == entry.process else { return nil }
        var role: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &role) == .success,
              let role = role as? String else { return nil }
        // Prefer the actual window supplied by the notification: a subsequent
        // restore must not turn a different window event into the old root.
        if role == kAXWindowRole { return ProcessStamp.read(pid: expectedPID) == entry.process ? element : nil }
        guard role == kAXApplicationRole else { return nil }
        let application = AXUIElementCreateApplication(pid_t(expectedPID))
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(application, kAXFocusedWindowAttribute as CFString, &focused) == .success,
              let focused, CFGetTypeID(focused) == AXUIElementGetTypeID(),
              ProcessStamp.read(pid: expectedPID) == entry.process else { return nil }
        return focused
    }

    private func object(_ ref: CFTypeRef, reader: AXReader, structure: Structure) -> UiObject? {
        guard let pair = reader.observedElements.first(where: { CFEqual($0.element, ref) }) else { return nil }
        return structure.objects.first { $0.instance_ref == pair.id }
    }

    private func element(_ id: String, reader: AXReader) -> AXUIElement? {
        reader.observedElements.first { $0.id == id }?.element
    }

    private func directMatches(_ retained: CFTypeRef, object: UiObject, reader: AXReader) -> Bool {
        guard CFGetTypeID(retained) == AXUIElementGetTypeID(), reader.budget() else { return false }
        let ax = unsafeDowncast(retained, to: AXUIElement.self)
        var pid: pid_t = 0
        guard AXUIElementGetPid(ax, &pid) == .success, pid > 0, UInt32(pid) == reader.app.pid,
              reader.string(ax, kAXRoleAttribute, required: true) == object.role,
              reader.string(ax, kAXSubroleAttribute) == object.subrole,
              reader.frame(ax) == object.frame,
              reader.string(ax, kAXTitleAttribute) == object.title,
              reader.string(ax, kAXDescriptionAttribute) == object.description else {
            diagnostics.record(.directQuery, anchors: entries.count); return false
        }
        return reader.budget()
    }

    private func membership(child: AXUIElement, parent: AXUIElement, reader: AXReader) -> Bool {
        guard let rawParent = reader.attribute(child, kAXParentAttribute, required: true),
              CFGetTypeID(rawParent) == AXUIElementGetTypeID(), CFEqual(rawParent, parent) else {
            diagnostics.record(.parentMembership, anchors: entries.count); return false
        }
        let children = reader.children(parent, attribute: kAXChildrenAttribute, maximum: 512, required: true)
        let matches = children.filter { CFEqual($0, child) }.count
        if matches != 1 { diagnostics.record(.parentMembership, anchors: entries.count) }
        return matches == 1 && reader.budget()
    }

    private func menuWindowRelation(_ element: AXUIElement, root: AXUIElement, reader: AXReader) -> Bool {
        guard reader.budget() else { return false }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, kAXWindowAttribute as CFString, &value)
        if error == .noValue { return true }
        guard error == .success, let value, CFGetTypeID(value) == AXUIElementGetTypeID(), CFEqual(value, root) else {
            reader.mark("menu AXWindow relation unavailable or inconsistent"); return false
        }
        return true
    }

    private func path(opener: UiObject, reader: AXReader, structure: Structure) -> [ProtectedNode]? {
        var result: [ProtectedNode] = []
        var cursor: UiObject? = opener
        var seen = Set<String>()
        while let node = cursor {
            guard result.count < 12, seen.insert(node.instance_ref).inserted,
                  node.window_id == opener.window_id, !node.secure,
                  let ax = element(node.instance_ref, reader: reader),
                  let fingerprint = menuFingerprint(object: node, structure: structure),
                  directMatches(ax, object: node, reader: reader) else { return nil }
            result.append(ProtectedNode(retained: MenuRetainedToken(ax), fingerprint: fingerprint, frame: node.frame))
            if node.parent == nil {
                guard node.role == kAXWindowRole, node.subrole == kAXStandardWindowSubrole else { return nil }
                return result
            }
            guard ![kAXMenuRole, kAXMenuBarRole, kAXMenuItemRole, kAXSheetRole, "AXDialog"].contains(node.role),
                  let parent = structure.objects.first(where: { $0.instance_ref == node.parent }),
                  let parentAX = element(parent.instance_ref, reader: reader),
                  membership(child: ax, parent: parentAX, reader: reader) else { return nil }
            cursor = parent
        }
        return nil
    }

    private func verified(_ entry: Entry, process: ProcessStamp, reader: AXReader, structure: Structure) -> UiObject? {
        func failed(_ condition: MenuAnchorDiagnosticCondition, index: Int? = nil) -> UiObject? {
            diagnostics.record(condition, anchors: entries.count, pathIndex: index)
            return nil
        }
        guard !entry.events.retired else { return failed(.retired) }
        guard process == entry.process else { return failed(.processIdentity) }
        guard let window = structure.windows.first(where: { $0.window_id == entry.window.window_id }) else { return failed(.originWindow) }
        guard window.frame == entry.window.frame else { return failed(.originFrame) }
        guard !window.modal else { return failed(.originModal) }
        guard structure.active_layer == window.window_id else { return failed(.activeLayer) }
        guard let first = entry.path.first, let opener = object(first.retained.element, reader: reader, structure: structure) else { return failed(.openerMissing) }
        guard opener.enabled else { return failed(.openerEnabled) }
        guard opener.in_view else { return failed(.openerInView) }
        guard let fresh = path(opener: opener, reader: reader, structure: structure) else { return failed(.pathRevalidation) }
        guard fresh.count == entry.path.count else { return failed(.pathCount) }
        for (index, pair) in zip(entry.path, fresh).enumerated() {
            let (a, b) = pair
            guard a.retained.matches(b.retained.element) else { return failed(.retainedCF, index: index) }
            guard a.fingerprint == b.fingerprint else { return failed(.fingerprint, index: index) }
            guard a.frame == b.frame else { return failed(.pathFrame, index: index) }
        }
        guard let root = fresh.last, entry.root.matches(root.retained.element) else { return failed(.rootCF) }
        return opener
    }

    private func anchor(_ entry: Entry, opener: UiObject, structure: Structure) -> MenuOpenerAnchor? {
        guard let fingerprint = menuFingerprint(object: opener, structure: structure) else { return nil }
        return MenuOpenerAnchor(anchor_id: entry.id, source_session_id: sessionID, anchor_revision: entry.revision,
            process: KernelProcessIdentity(pid: structure.app.pid, start_seconds: entry.process.seconds, start_micros: entry.process.micros),
            origin_window_id: entry.window.window_id, origin_window_frame: entry.window.frame,
            opener_instance: opener.instance_ref, opener_fingerprint: fingerprint, opener_frame: opener.frame,
            verified_source_sequence: 0, verified_observed_ms: 0, continuity: .directRead)
    }

    /// All mutations below are retention/provenance bookkeeping, never AX input.
    func project(reader: AXReader, structure: inout Structure, process: ProcessStamp) -> Bool {
        var successful = false
        var failure: MenuAnchorDiagnosticCondition = .projectReaderPartial
        defer { if !successful { invalidateAll(reason: failure) } }
        guard let origin = menuOriginWindow(request: reader.request, app: reader.app),
              structure.active_layer == origin, let window = structure.windows.first(where: { $0.window_id == origin }),
              !window.modal, reader.reasons.isEmpty, !reader.modalFound else {
            invalidateAll(reason: reader.reasons.isEmpty ? .projectGate : .projectReaderPartial); return false
        }
        let menus = structure.objects.filter { $0.role == kAXMenuRole }
        entries.removeAll { verified($0, process: process, reader: reader, structure: structure) == nil }
        if menus.isEmpty {
            for entry in entries { entry.presentation.close() }
            for opener in structure.objects where opener.window_id == origin &&
                [kAXMenuButtonRole, kAXPopUpButtonRole].contains(opener.role) && opener.enabled && opener.in_view &&
                !opener.secure && opener.actions.contains("show_menu") {
                guard window.frame.cgRect.contains(opener.frame.cgRect) else { continue }
                guard let ax = element(opener.instance_ref, reader: reader) else { continue }
                if entries.contains(where: { $0.path.first?.retained.matches(ax) == true }) { continue }
                guard entries.count < 8 else { reader.mark("Source menu anchor capacity exceeded"); break }
                guard let retainedPath = path(opener: opener, reader: reader, structure: structure),
                      retainedPath.last?.frame == window.frame,
                      let root = retainedPath.last?.retained.element else { continue }
                revision += 1
                entries.append(Entry(id: "menu-anchor-\(UUID().uuidString.lowercased())", revision: revision,
                    process: process, root: unsafeDowncast(root, to: AXUIElement.self), window: window, path: retainedPath))
            }
        }
        structure.menu_anchors = entries.compactMap { entry in
            verified(entry, process: process, reader: reader, structure: structure).flatMap { anchor(entry, opener: $0, structure: structure) }
        }
        if menus.isEmpty {
            guard nativeMenuDrawingWindows(pid: reader.app.pid).isEmpty else {
                failure = .projectUnownedDrawing
                reader.mark("unowned native menu drawing layer is unresolved"); return false
            }
            successful = reader.reasons.isEmpty; return successful
        }
        guard menus.count == 1, let menuObject = menus.first, menuObject.window_id == origin,
              let menuAX = element(menuObject.instance_ref, reader: reader),
              let openerObject = structure.objects.first(where: { $0.instance_ref == menuObject.parent }),
              [kAXMenuButtonRole, kAXPopUpButtonRole].contains(openerObject.role),
              let openerAX = element(openerObject.instance_ref, reader: reader),
              let entry = entries.first(where: { $0.path.first?.retained.matches(openerAX) == true }),
              let currentAnchor = structure.menu_anchors.first(where: { $0.anchor_id == entry.id }),
              directMatches(menuAX, object: menuObject, reader: reader),
              membership(child: menuAX, parent: openerAX, reader: reader) else {
            failure = .projectMenuRelation
            reader.mark("Source retained level-one menu opener relation unavailable"); return false
        }
        let rootRaw = unsafeDowncast(entry.root.element, to: AXUIElement.self)
        let application = AXUIElementCreateApplication(pid_t(reader.app.pid))
        guard let focused = reader.attribute(application, kAXFocusedWindowAttribute, required: true),
              CFEqual(focused, rootRaw),
              let openerWindow = reader.attribute(openerAX, kAXWindowAttribute, required: true),
              CFEqual(openerWindow, rootRaw),
              let menuTop = reader.attribute(menuAX, kAXTopLevelUIElementAttribute, required: true),
              CFEqual(menuTop, rootRaw), menuWindowRelation(menuAX, root: rootRaw, reader: reader) else {
            failure = .projectFocusedRelation
            reader.mark("Source menu focused root or top-level relationship unavailable"); return false
        }
        let items = structure.objects.filter { $0.parent == menuObject.instance_ref }
        guard !items.isEmpty, items.allSatisfy({ item in
            guard item.role == kAXMenuItemRole, item.window_id == origin, !item.secure,
                  let ax = element(item.instance_ref, reader: reader), directMatches(ax, object: item, reader: reader),
                  membership(child: ax, parent: menuAX, reader: reader),
                  let top = reader.attribute(ax, kAXTopLevelUIElementAttribute, required: true), CFEqual(top, rootRaw) else { return false }
            return menuWindowRelation(ax, root: rootRaw, reader: reader)
        }), let fingerprint = menuFingerprint(object: menuObject, structure: structure) else {
            failure = .projectItems
            reader.mark("Source menu item parent membership or top-level relationship unavailable"); return false
        }
        let before = nativeMenuDrawingWindows(pid: reader.app.pid)
        guard before.count == 1,
              let drawing = uniqueMenuDrawingWindow(menuFrame: menuObject.frame, itemFrames: items.map(\.frame), windows: before, pid: reader.app.pid),
              nativeMenuDrawingWindows(pid: reader.app.pid) == before, reader.reasons.isEmpty, reader.budget() else {
            failure = .projectDrawing
            reader.mark("Source menu drawing window profile is ambiguous or changed"); return false
        }
        let contextID = entry.presentation.observe(menuAX)
        for index in structure.objects.indices where structure.objects[index].instance_ref == menuObject.instance_ref ||
            structure.objects[index].parent == menuObject.instance_ref {
            structure.objects[index].in_view = drawing.frame.cgRect.contains(structure.objects[index].frame.cgRect)
        }
        structure.menu_context = MenuContext(context_id: contextID, source_session_id: sessionID, opener: currentAnchor,
            menu_instance: menuObject.instance_ref, menu_fingerprint: fingerprint, menu_frame: menuObject.frame, drawing_window: drawing)
        successful = true
        return true
    }
}

func runMenuHistoryDiagnosticTests() throws -> [String] {
    let history = MenuHistoryDiagnostics()
    let menu = "offline-history-menu" as CFString
    let item = "offline-history-item" as CFString
    let root = "offline-history-root" as CFString
    history.observeMenu(menu, directItems: [item], contextID: "closed-context")
    try requireTest(history.classify(menu, protectedPaths: []) == .unknown && history.classify(item, protectedPaths: []) == .unknown,
                    "live menu history cannot claim closed identity")
    history.closeMenus()
    try requireTest(history.classify(menu, protectedPaths: []) == .closedMenu && history.classify(item, protectedPaths: []) == .closedDirectItem,
                    "closed diagnostic identity uses actual retained CF menu and direct item")
    history.observeOtherWindow(root)
    try requireTest(history.classify(root, protectedPaths: []) == .knownOtherWindow &&
                    history.classify("offline-unknown" as CFString, protectedPaths: []) == .unknown,
                    "known other window and unknown use CF identity only")
    try requireTest(history.classify(menu, protectedPaths: [[MenuRetainedToken(menu)]]) == .unknown &&
                    history.classify(root, protectedPaths: [[MenuRetainedToken(root)]]) == .unknown,
                    "protected CF paths take precedence over diagnostic candidates")
    history.observeMenu(menu, directItems: [item], contextID: "new-context")
    try requireTest(history.classify(menu, protectedPaths: []) == .unknown && history.classify(item, protectedPaths: []) == .unknown,
                    "reopen never revives closed context classification")

    let conflict = MenuHistoryDiagnostics()
    conflict.observeMenu(menu, directItems: [item], contextID: "conflicting-context")
    conflict.closeMenus()
    conflict.observeOtherWindow(menu)
    conflict.observeMenu(menu, directItems: [item], contextID: "later-context")
    conflict.closeMenus()
    try requireTest(conflict.classify(menu, protectedPaths: []) == .unknown,
                    "same CF observed with conflicting role remains permanently unknown")

    let bounded = MenuHistoryDiagnostics()
    for index in 0..<12 { bounded.observeOtherWindow("window-\(index)" as CFString) }
    try requireTest(bounded.windowCount == 8, "window diagnostic capacity is eight actual references")
    for context in 0..<10 {
        bounded.observeMenu("menu-\(context)" as CFString,
            directItems: (0..<16).map { "item-\(context)-\($0)" as CFString }, contextID: "context-\(context)")
    }
    try requireTest(bounded.count == 32, "diagnostic history has fixed total capacity 32")
    bounded.observeMenu("menu-0" as CFString, directItems: (16..<32).map { "item-0-\($0)" as CFString }, contextID: "context-0")
    bounded.closeMenus()
    try requireTest(bounded.classify("item-0-7" as CFString, protectedPaths: []) == .closedDirectItem &&
                    bounded.classify("item-0-8" as CFString, protectedPaths: []) == .unknown &&
                    bounded.classify("item-0-16" as CFString, protectedPaths: []) == .unknown &&
                    bounded.classify("window-8" as CFString, protectedPaths: []) == .unknown,
                    "candidate overflow stays unknown without authority")
    bounded.clear()
    try requireTest(bounded.count == 0, "watch close releases diagnostic CF references")

    history.closeMenus()
    let state = MenuAnchorEventState(id: "offline-anchor", revision: 1,
        path: [MenuRetainedToken(root)], root: MenuRetainedToken(root))
    let context = state.presentation.observe(menu)
    applyMenuAnchorNotification(kAXMenuClosedNotification, element: menu, focusedWindow: nil, states: [state])
    try requireTest(state.presentation.contextID == nil && !state.retired, "menu close ends active context")
    try requireTest(history.classify(menu, protectedPaths: [state.path]) == .closedMenu, "closed history supplies diagnostic category")
    applyMenuAnchorNotification(kAXUIElementDestroyedNotification, element: menu, focusedWindow: nil, states: [state])
    try requireTest(state.retired, "diagnostic known category does not relax unassigned destruction retirement")
    try requireTest(state.presentation.observe(menu) != context, "historical diagnostic context cannot revive authority")
    return ["menu_history_diagnostic_cf_categories", "menu_history_diagnostic_protected_precedence",
            "menu_history_diagnostic_fixed_capacity", "menu_history_diagnostic_has_no_authority"]
}

func runOtherWindowDestructionProofTests() throws -> [String] {
    let process = ProcessStamp(seconds: 10, micros: 20)
    let root = MenuRetainedToken("proof-origin" as CFString)
    let window = MenuRetainedToken("proof-other-window" as CFString)
    let scope = MenuRetainedToken("proof-opener" as CFString)
    let now: UInt64 = 10_000_000_000
    func proof(_ token: MenuRetainedToken = window, time: UInt64 = now) -> OtherWindowDestructionProof {
        OtherWindowDestructionProof(sessionID: "proof-session", pid: 42, process: process,
            window: token, originRoot: root, verifiedNs: time)
    }
    func consume(_ registry: OtherWindowDestructionProofRegistry, element: CFTypeRef = window.element,
                 session: String = "proof-session", stamp: ProcessStamp? = process, at: UInt64 = now + 1,
                 roots: [MenuRetainedToken]? = nil, paths: [[MenuRetainedToken]]? = nil,
                 menu: [MenuRetainedToken] = [], unambiguous: Bool = true) -> Bool {
        if !unambiguous { registry.markConflict(element) }
        return registry.consume(element, sessionID: session, pid: 42, process: stamp, now: at,
            originRoots: roots ?? [root], protectedPaths: paths ?? [[scope, root]], liveMenu: menu)
    }
    func eligible(role: String? = kAXWindowRole, modal: Bool? = false, parentRole: String? = kAXApplicationRole,
                  parent: Bool = true, member: Bool = true, pid: Bool = true, disjoint: Bool = true) -> Bool {
        otherWindowRelationsEligible(role: role, modal: modal, parentRole: parentRole,
            parentMatches: parent, uniqueMembership: member, processMatches: pid, subtreeDisjoint: disjoint)
    }
    try requireTest(eligible(), "actual nonmodal independent AXWindow relation is eligible")
    try requireTest(otherWindowModalBoolean(kCFBooleanFalse) == false && otherWindowModalBoolean(kCFBooleanTrue) == true &&
                    otherWindowModalBoolean(NSNumber(value: Int32(0))) == nil && otherWindowModalBoolean(nil) == nil,
                    "only real CFBoolean false can prove nonmodal status")
    try requireTest(!eligible(role: "AXDialog") && !eligible(role: kAXSheetRole) && !eligible(role: kAXMenuRole) &&
                    !eligible(role: "AXPopover") && !eligible(modal: nil) && !eligible(modal: true) &&
                    !eligible(parentRole: nil) && !eligible(parent: false) && !eligible(member: false) &&
                    !eligible(pid: false) && !eligible(disjoint: false),
                    "unknown modal, unsupported parent, duplicate member and scope overlap cannot prove other window")
    let partial = OtherWindowDestructionProofRegistry()
    partial.commit([proof()], complete: false)
    try requireTest(partial.count == 0 && !consume(partial), "partial traversal cannot register independent window proof")
    let once = OtherWindowDestructionProofRegistry()
    once.commit([proof()], complete: true)
    try requireTest(consume(once) && !consume(once), "actual independent CF proof consumes exactly once")
    let foreign = OtherWindowDestructionProofRegistry()
    foreign.commit([proof()], complete: true)
    try requireTest(!consume(foreign, element: "same-label-new-actual-window" as CFString) && foreign.count == 1,
                    "equal appearance never substitutes a different actual CF")
    try requireTest(!consume(foreign, session: "different-session") && foreign.count == 0,
                    "wrong session fails closed and consumes candidate")
    for badStamp in [nil, ProcessStamp(seconds: 11, micros: 20)] as [ProcessStamp?] {
        let registry = OtherWindowDestructionProofRegistry(); registry.commit([proof()], complete: true)
        try requireTest(!consume(registry, stamp: badStamp), "missing process or PID reuse cannot consume successful proof")
    }
    for time in [now - 1, now + OtherWindowDestructionProofRegistry.lifetimeNs + 1] {
        let registry = OtherWindowDestructionProofRegistry(); registry.commit([proof()], complete: true)
        try requireTest(!consume(registry, at: time), "regressed clock or expired proof cannot preserve anchor")
    }
    let boundary = OtherWindowDestructionProofRegistry(); boundary.commit([proof()], complete: true)
    try requireTest(consume(boundary, at: now + OtherWindowDestructionProofRegistry.lifetimeNs), "freshness boundary is bounded to two seconds")
    for (roots, paths, menu, unambiguous) in [
        ([root], [[window, root]], [], true),
        ([window], [[scope]], [], true),
        ([MenuRetainedToken("replacement-origin" as CFString)], [[scope]], [], true),
        ([], [[scope]], [], true),
        ([root], [[scope]], [window], true),
        ([root], [[scope]], [], false)
    ] as [([MenuRetainedToken], [[MenuRetainedToken]], [MenuRetainedToken], Bool)] {
        let registry = OtherWindowDestructionProofRegistry(); registry.commit([proof()], complete: true)
        try requireTest(!consume(registry, roots: roots, paths: paths, menu: menu, unambiguous: unambiguous),
                        "protected root/path/live menu or ambiguity takes priority over other window proof")
    }
    let bounded = OtherWindowDestructionProofRegistry()
    bounded.commit((0..<16).map { proof(MenuRetainedToken("bounded-other-\($0)" as CFString)) }, complete: true)
    try requireTest(bounded.count == 8 && !consume(bounded, element: "bounded-other-8" as CFString), "independent proofs remain bounded to eight")
    bounded.clear(); try requireTest(bounded.count == 0, "anchor/session retirement clears window authority")
    let conflict = OtherWindowDestructionProofRegistry(); conflict.commit([proof()], complete: true)
    conflict.markConflict(window.element); conflict.commit([proof(time: now + 1)], complete: true)
    try requireTest(!consume(conflict), "independent ambiguity stays sticky despite a later proposed proof")
    let invalidated = OtherWindowDestructionProofRegistry(); invalidated.commit([proof()], complete: true)
    invalidated.remove(window.element)
    try requireTest(!consume(invalidated), "failed live relation or observed role conflict invalidates candidate")
    for name in [kAXWindowMovedNotification, kAXWindowResizedNotification, kAXUIElementDestroyedNotification] {
        let state = MenuAnchorEventState(id: "protected-anchor", revision: 1, path: [scope, root], root: root)
        applyMenuAnchorNotification(name, element: scope.element, focusedWindow: nil, states: [state])
        try requireTest(state.retired, "protected geometry and destruction notifications always retire anchor")
    }
    let independent = MenuAnchorEventState(id: "independent-anchor", revision: 1, path: [scope, root], root: root)
    let registry = OtherWindowDestructionProofRegistry(); registry.commit([proof()], complete: true)
    if !consume(registry) { applyMenuAnchorNotification(kAXUIElementDestroyedNotification, element: window.element, focusedWindow: nil, states: [independent]) }
    try requireTest(!independent.retired, "only proved independent window destruction preserves protected opener")
    applyMenuAnchorNotification(kAXUIElementDestroyedNotification, element: "unknown-window" as CFString, focusedWindow: nil, states: [independent])
    try requireTest(independent.retired, "unknown destruction retains original fail-closed retirement")
    return ["other_window_actual_relation_proof", "other_window_complete_commit_and_one_use",
            "other_window_actual_cf_and_session", "other_window_kernel_process_identity",
            "other_window_bounded_freshness", "other_window_protected_and_live_scope_priority",
            "other_window_bounded_capacity_and_invalidation", "other_window_unknown_and_geometry_remain_fail_closed"]
}
