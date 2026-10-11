import Foundation
import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import os

struct AXSessionRead: Sendable {
    var read: StructureRead
    let tree_revision: UInt64
    let observer_revision: UInt64
    let observer_root_available: Bool
    var observer_available: Bool { observer_root_available }
    let observer_reason: String?
    let started_unix_ms: UInt64
    let finished_unix_ms: UInt64
    let identity_boundary: SourceIdentityBoundary?
    let owned_document_identity_boundary: SourceIdentityBoundary?
    let owned_document_evidence: OwnedDocumentReceiverEvidence?
    let backgroundWindowContext: BackgroundAXWindowContext?
    /// Reader-local actual-CF continuity for the first capture baseline only.
    /// This is never serialized and does not grant input or general authority.
    let captureRootContinuous: Bool?

    init(read: StructureRead, tree_revision: UInt64, observer_revision: UInt64,
         observer_available: Bool, observer_reason: String?, started_unix_ms: UInt64,
         finished_unix_ms: UInt64, identity_boundary: SourceIdentityBoundary? = nil,
         owned_document_identity_boundary: SourceIdentityBoundary? = nil,
         owned_document_evidence: OwnedDocumentReceiverEvidence? = nil,
         captureRootContinuous: Bool? = nil,
         backgroundWindowContext: BackgroundAXWindowContext? = nil) {
        self.read = read
        self.tree_revision = tree_revision
        self.observer_revision = observer_revision
        self.observer_root_available = observer_available
        self.observer_reason = observer_reason
        self.started_unix_ms = started_unix_ms
        self.finished_unix_ms = finished_unix_ms
        self.identity_boundary = identity_boundary
        self.owned_document_identity_boundary = owned_document_identity_boundary
        self.owned_document_evidence = owned_document_evidence
        self.backgroundWindowContext = backgroundWindowContext
        self.captureRootContinuous = captureRootContinuous
    }
}

private func unixMilliseconds() -> UInt64 {
    UInt64(max(0, Date().timeIntervalSince1970 * 1000))
}

func validBackgroundAXActionNames(_ names: [String]) -> Bool {
    names.count <= 32
        && names.allSatisfy { !$0.isEmpty && $0.utf8.count <= 64
            && !$0.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) }
        && Set(names).count == names.count
}

private func closedRead(app: AppIdentity, reason: String) -> AXSessionRead {
    var structure = Structure(app: app)
    structure.incomplete_reason = reason
    let now = uptimeNs()
    let wall = unixMilliseconds()
    return AXSessionRead(read: StructureRead(structure: structure, started: now, finished: now, secureFrames: [],
                                            privacyRejection: .readUnavailable),
                         tree_revision: 0, observer_revision: 0, observer_available: false,
                         observer_reason: reason, started_unix_ms: wall, finished_unix_ms: wall)
}

private struct AXReadCommand: Sendable {
    let frontmostPID: UInt32?
    let ownedDocumentScope: OwnedDocumentReadScope?
    let backgroundAXOnly: Bool
    let reply: CheckedContinuation<AXSessionRead, Never>
}

private enum AXCommand: Sendable {
    case read(AXReadCommand), close, idle
}

/// Only Sendable requests and continuations cross the thread boundary. AX refs
/// are created inside workerMain and never enter this lock's state.
private final class AXMailbox: Sendable {
    private struct State: Sendable {
        var reads: [AXReadCommand] = []
        var closing = false
        var finished = false
        var closeReply: CheckedContinuation<Void, Never>?
    }
    private let state = OSAllocatedUnfairLock(initialState: State())
    let app: AppIdentity

    init(app: AppIdentity) { self.app = app }

    func enqueue(_ command: AXReadCommand) -> Bool {
        state.withLock {
            guard !$0.closing, $0.reads.count < 2 else { return false }
            $0.reads.append(command)
            return true
        }
    }

    func next() -> AXCommand {
        state.withLock {
            if $0.closing { return .close }
            if !$0.reads.isEmpty { return .read($0.reads.removeFirst()) }
            return .idle
        }
    }

    func requestClose(_ reply: CheckedContinuation<Void, Never>? = nil) {
        let result = state.withLock { value -> (Bool, [AXReadCommand]) in
            if value.finished { return (true, []) }
            value.closing = true
            if let reply { value.closeReply = reply }
            let abandoned = value.reads
            value.reads.removeAll()
            return (false, abandoned)
        }
        for command in result.1 {
            command.reply.resume(returning: closedRead(app: app, reason: "AX session closed"))
        }
        if result.0 { reply?.resume() }
    }

    func finishClose() {
        let reply = state.withLock { value in
            value.finished = true
            let reply = value.closeReply
            value.closeReply = nil
            return reply
        }
        reply?.resume()
    }
}

/// One resident session per explicitly selected target process. The actor only
/// owns a Sendable mailbox; its dedicated Thread owns every AX reference and
/// the AXObserver run loop. No unchecked Sendable conformance is needed.
actor AXSession {
    private let app: AppIdentity
    private let mailbox: AXMailbox
    private var closing = false
    private var closed = false
    private var closeWaiters: [CheckedContinuation<Void, Never>] = []

    init(app: AppIdentity, request: NativeRequest,
         ownedDocumentRegistration: OwnedDocumentReceiverRegistration? = nil) {
        self.app = app
        let mailbox = AXMailbox(app: app)
        self.mailbox = mailbox
        let expectedProcess = ProcessStamp.read(pid: app.pid)
        let thread = Thread {
            let worker = AXSessionWorker(app: app, request: request, expectedProcess: expectedProcess,
                ownedDocumentRegistration: ownedDocumentRegistration)
            while true {
                switch mailbox.next() {
                case .read(let command):
                    let result = autoreleasepool {
                        worker.read(frontmostPID: command.frontmostPID,
                            ownedDocumentScope: command.ownedDocumentScope,
                            backgroundAXOnly: command.backgroundAXOnly)
                    }
                    command.reply.resume(returning: result)
                case .close:
                    worker.close()
                    mailbox.finishClose()
                    return
                case .idle:
                    worker.pumpNotifications()
                }
            }
        }
        thread.name = "cu.source.ax-session"
        thread.qualityOfService = .userInitiated
        thread.start()
    }

    func read(frontmostPID: UInt32?, ownedDocumentScope: OwnedDocumentReadScope? = nil) async -> AXSessionRead {
        guard !closing else { return closedRead(app: app, reason: "AX session closed") }
        return await withCheckedContinuation { reply in
            if !mailbox.enqueue(AXReadCommand(frontmostPID: frontmostPID,
                                               ownedDocumentScope: ownedDocumentScope,
                                               backgroundAXOnly: false, reply: reply)) {
                reply.resume(returning: closedRead(app: app, reason: "AX session request queue full or closed"))
            }
        }
    }

    func readBackgroundAXOnly() async -> AXSessionRead {
        guard !closing else { return closedRead(app: app, reason: "AX session closed") }
        return await withCheckedContinuation { reply in
            if !mailbox.enqueue(AXReadCommand(frontmostPID: nil, ownedDocumentScope: nil,
                                               backgroundAXOnly: true, reply: reply)) {
                reply.resume(returning: closedRead(app: app, reason: "AX session request queue full or closed"))
            }
        }
    }

    func close() async {
        if closed { return }
        if closing {
            await withCheckedContinuation { closeWaiters.append($0) }
            return
        }
        closing = true
        await withCheckedContinuation { mailbox.requestClose($0) }
        closed = true
        for waiter in closeWaiters { waiter.resume() }
        closeWaiters.removeAll()
    }

    deinit { mailbox.requestClose() }
}

struct AXElementContext: Equatable {
    let role: String
    let subrole: String?
    let frame: Rect
    let window: String
}

struct AXRootWindowProof: Equatable {
    let sourceSessionID: String
    let sessionEpoch: UInt64
    let process: KernelProcessIdentity
    let windowID: String
    let frame: Rect

    init?(_ context: WindowContext?) {
        guard let context,
              !context.source_session_id.isEmpty,
              context.session_epoch > 0,
              context.process.pid > 0,
              context.origin_window_id.hasPrefix("window-"),
              context.origin_window_frame.valid else { return nil }
        sourceSessionID = context.source_session_id
        sessionEpoch = context.session_epoch
        process = context.process
        windowID = context.origin_window_id
        frame = context.origin_window_frame
    }
}

func sameSelectedRoot(before: AXRootWindowProof?, after: AXRootWindowProof?, activeLayer: String?) -> Bool {
    guard let before, let after else { return false }
    return before == after && activeLayer == before.windowID
}

/// A read-local value can report whether its selected AX root matches the
/// actual retained root that started this AXSession. Unknown projections do not
/// initialize, clear, or recover the baseline; a witnessed replacement is
/// terminal for the lifetime of the session.
func observeCaptureRootContinuity(selectedRoot: MenuRetainedToken?, qualifies: Bool,
                                  baseline: inout MenuRetainedToken?,
                                  replacementLatched: inout Bool) -> Bool? {
    guard qualifies, let selectedRoot else { return nil }
    if replacementLatched { return false }
    guard let existingBaseline = baseline else {
        baseline = selectedRoot
        return true
    }
    guard existingBaseline.matches(selectedRoot.element) else {
        replacementLatched = true
        return false
    }
    return true
}

func canBeginWindowInventoryIdentityRevalidation(
    inventoryChanged: Bool,
    continuityVerified: Bool,
    dirty: Bool,
    processStable: Bool,
    frontmostStable: Bool,
    observerHealthy: Bool,
    bindingBoundaryInvalidationAvailable: Bool,
    hasSelectedRootProof: Bool
) -> Bool {
    inventoryChanged && continuityVerified && !dirty && processStable && frontmostStable
        && observerHealthy && bindingBoundaryInvalidationAvailable && hasSelectedRootProof
}

func canAcceptWindowInventoryIdentityRevalidationRead(
    rootStable: Bool,
    observerHealthy: Bool,
    observerRevisionStable: Bool,
    dirty: Bool,
    contentDirty: Bool,
    complete: Bool,
    treeStable: Bool,
    allCandidatesReused: Bool
) -> Bool {
    rootStable && observerHealthy && observerRevisionStable && !dirty && !contentDirty && complete
        && treeStable && allCandidatesReused
}

@discardableResult
func establishFreshContentReadBaseline(_ contentDirty: inout Bool) -> Bool {
    let changedBeforeRead = contentDirty
    contentDirty = false
    return changedBeforeRead
}

/// Retains only notification-covered identities across reads. Read-scoped
/// references are separate and disappear at the next read boundary.
final class AXIdentityRegistry {
    enum Assignment {
        case existing(String, UiObjectIdentityContinuity)
        case inserted(String, UiObjectIdentityContinuity)
        case changed, full
    }
    private struct Entry {
        let element: CFTypeRef
        var context: AXElementContext
        let id: String
        var ownerRoot: CFTypeRef?
        var candidate: Bool
        var candidateReused: Bool
    }
    struct CandidateSummary {
        let count: Int
        let reused: Int
        var discarded: Int { count - reused }
    }
    let capacity: Int
    let sessionID: String
    private(set) var generation: UInt64 = 1
    private var sequence: UInt64 = 0
    private var entries: [Entry] = []
    private struct ReadEntry {
        let element: CFTypeRef
        let context: AXElementContext
        let id: String
        let continuity: UiObjectIdentityContinuity
    }
    private var readEntries: [ReadEntry] = []
    private var readSequence: UInt64 = 0
    private var candidateRevalidation = false
    var count: Int { entries.count }
    var readCount: Int { readEntries.count }

    init(capacity: Int, sessionID: String = UUID().uuidString.lowercased()) {
        self.capacity = max(1, min(capacity, 512))
        self.sessionID = sessionID
    }

    func beginRead() {
        readEntries.removeAll(keepingCapacity: true)
        readSequence &+= 1
    }

    func assign(_ element: CFTypeRef, context: AXElementContext,
                ownerRoot: CFTypeRef? = nil, continuityCovered: Bool = true) -> Assignment {
        if let index = readEntries.firstIndex(where: { CFEqual($0.element, element) }) {
            guard readEntries[index].context == context else { return .changed }
            return .existing(readEntries[index].id, readEntries[index].continuity)
        }

        if let index = entries.firstIndex(where: { CFEqual($0.element, element) }) {
            if continuityCovered {
                entries[index].context = context
                entries[index].ownerRoot = ownerRoot
                if entries[index].candidate { entries[index].candidateReused = true }
                guard readEntries.count < capacity else { return .full }
                readEntries.append(ReadEntry(element: element, context: context,
                    id: entries[index].id, continuity: .notificationCovered))
                return .existing(entries[index].id, .notificationCovered)
            }
            entries.remove(at: index)
        }

        guard readEntries.count < capacity else { return .full }
        sequence += 1
        let id: String
        let continuity: UiObjectIdentityContinuity
        if continuityCovered, entries.count < capacity {
            id = "ax-\(sessionID)-g\(generation)-e\(sequence)"
            continuity = .notificationCovered
            entries.append(Entry(element: element, context: context, id: id, ownerRoot: ownerRoot,
                                 candidate: false, candidateReused: false))
        } else {
            id = "ax-\(sessionID)-g\(generation)-r\(readSequence)-e\(sequence)"
            continuity = .readScoped
        }
        readEntries.append(ReadEntry(element: element, context: context, id: id, continuity: continuity))
        return .inserted(id, continuity)
    }

    func invalidateObjects(_ elements: [CFTypeRef]) {
        guard !elements.isEmpty else { return }
        entries.removeAll { entry in elements.contains(where: { CFEqual(entry.element, $0) }) }
        readEntries.removeAll { entry in elements.contains(where: { CFEqual(entry.element, $0) }) }
    }

    func invalidateCurrentReadObjects() {
        invalidateObjects(readEntries.map(\.element))
        readEntries.removeAll(keepingCapacity: true)
    }

    func prune(to observed: [CFTypeRef]) {
        entries.removeAll { entry in !observed.contains(where: { CFEqual(entry.element, $0) }) }
    }

    func hasUniqueIdentityMatch(_ element: CFTypeRef) -> Bool {
        let retained = entries.filter { CFEqual($0.element, element) }
        let current = readEntries.filter { CFEqual($0.element, element) }
        guard retained.count <= 1, current.count <= 1 else { return false }
        if let retained = retained.first, let current = current.first {
            return retained.id == current.id
        }
        return !retained.isEmpty || !current.isEmpty
    }

    @discardableResult
    func beginOneReadCandidateRevalidation() -> Int {
        precondition(!candidateRevalidation)
        candidateRevalidation = true
        generation += 1
        sequence = 0
        for index in entries.indices {
            entries[index].candidate = true
            entries[index].candidateReused = false
        }
        return entries.count
    }

    func finishOneReadCandidateRevalidation() -> CandidateSummary {
        precondition(candidateRevalidation)
        let candidates = entries.filter(\.candidate)
        let summary = CandidateSummary(count: candidates.count, reused: candidates.filter(\.candidateReused).count)
        entries.removeAll { $0.candidate && !$0.candidateReused }
        for index in entries.indices {
            entries[index].candidate = false
            entries[index].candidateReused = false
        }
        candidateRevalidation = false
        return summary
    }

    var candidateReuseCount: Int {
        entries.filter { $0.candidate && $0.candidateReused }.count
    }

    func contains(_ element: CFTypeRef) -> Bool {
        entries.contains { CFEqual($0.element, element) }
            || readEntries.contains { CFEqual($0.element, element) }
    }

    func uniqueObjectRef(for element: CFTypeRef) -> String? {
        if let readMatches = readEntries.filter({ CFEqual($0.element, element) }).first,
           readEntries.filter({ CFEqual($0.element, element) }).count == 1 {
            return readMatches.continuity == .notificationCovered ? readMatches.id : nil
        }
        var id: String?
        var count = 0
        for entry in entries where CFEqual(entry.element, element) {
            count += 1
            id = entry.id
            if count > 1 { return nil }
        }
        return count == 1 ? id : nil
    }

    func diagnosticRoleCategory(for element: CFTypeRef) -> AXRetainedRoleCategory {
        var matchCount = 0
        var role: String?
        for entry in entries where CFEqual(entry.element, element) {
            matchCount += 1
            role = entry.context.role
            if matchCount > 1 { break }
        }
        return axRetainedRoleCategory(matchCount: matchCount, role: role)
    }

    func diagnosticOwnerRootMatch(for element: CFTypeRef, selectedRoot: CFTypeRef?) -> Bool? {
        var matchCount = 0
        var ownerRoot: CFTypeRef?
        for entry in entries where CFEqual(entry.element, element) {
            matchCount += 1
            ownerRoot = entry.ownerRoot
            if matchCount > 1 { break }
        }
        return axOwnerRootMatchesSelectedRoot(
            matchCount: matchCount,
            cachedOwnerRoot: ownerRoot,
            selectedRoot: selectedRoot)
    }

    func diagnosticSelectedRootWindowMatch(for element: CFTypeRef, selectedRootWindowID: String?) -> Bool? {
        var matchCount = 0
        var cachedWindow: String?
        for entry in entries where CFEqual(entry.element, element) {
            matchCount += 1
            cachedWindow = entry.context.window
            if matchCount > 1 { break }
        }
        return axRetainedWindowMatchesSelectedRoot(
            matchCount: matchCount,
            cachedWindow: cachedWindow,
            selectedRootWindowID: selectedRootWindowID)
    }

    func identityBoundary(globalRootVerified: Bool) -> SourceIdentityBoundary? {
        guard globalRootVerified, generation > 0,
              let session = UUID(uuidString: sessionID) else { return nil }
        return SourceIdentityBoundary(namespace: session.uuidString.lowercased(), generation: generation)
    }

    /// The same registry namespace and generation, admitted only by a complete
    /// dedicated owned-scope read rather than by generic Structure completeness.
    func ownedScopeIdentityBoundary(complete: Bool) -> SourceIdentityBoundary? {
        guard complete, generation > 0,
              let session = UUID(uuidString: sessionID) else { return nil }
        return SourceIdentityBoundary(namespace: session.uuidString.lowercased(), generation: generation)
    }

    func invalidate() {
        entries.removeAll()
        readEntries.removeAll()
        candidateRevalidation = false
        generation += 1
        sequence = 0
    }
}

struct ProcessStamp: Equatable, Sendable {
    let seconds: UInt64
    let micros: UInt64
    static func read(pid: UInt32) -> ProcessStamp? {
        guard pid > 0, pid <= UInt32(Int32.max) else { return nil }
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard proc_pidinfo(Int32(pid), PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return ProcessStamp(seconds: info.pbi_start_tvsec, micros: info.pbi_start_tvusec)
    }
}

/// Extend the same non-atomic read interval through optional proof queries and
/// final checks, preserving the original AX start used for freshness mapping.
func finishAXSessionStructureRead(_ read: StructureRead, finishedNs: UInt64) -> StructureRead {
    StructureRead(structure: read.structure, started: read.started, finished: finishedNs,
                  secureFrames: read.secureFrames, visiblePrivacy: read.visiblePrivacy,
                  privacyRejection: read.privacyRejection)
}

struct AXWindowContext: Equatable, Sendable {
    let id: UInt32
    let frame: Rect
    let zIndex: UInt32
}

/// An AX traversal is a series of RPCs, not an atomic snapshot. Revalidate the
/// target lifetime and window mapping after them even when notifications were
/// unsupported. On mismatch the caller discards every returned AX field.
func axPostReadContextMatches(expectedProcess: ProcessStamp, actualProcess: ProcessStamp?,
                              before: [AXWindowContext], after: [AXWindowContext]) -> Bool {
    actualProcess == expectedProcess && before == after
}

struct TreeContext: Equatable {
    let element: CFTypeRef
    let parentElement: CFTypeRef?
    let siblingIndex: Int?
    let window: String
    let role: String
    let subrole: String?
    let frame: Rect
    let group: String?

    init(_ object: UiObject, observed: ObservedAXElement) {
        self.init(object, element: observed.element,
                  parentElement: observed.parentElement,
                  siblingIndex: observed.siblingIndex)
    }

    init(_ object: UiObject, element: CFTypeRef, parentElement: CFTypeRef?, siblingIndex: Int?) {
        self.element = element
        self.parentElement = parentElement
        self.siblingIndex = siblingIndex
        window = object.window_id; role = object.role; subrole = object.subrole
        frame = object.frame; group = object.group
    }

    /// Test helper for pure topology checks; the string values stand in for
    /// CFEqual references and are deliberately independent from projected IDs.
    init(_ object: UiObject) {
        element = object.instance_ref as CFString
        parentElement = object.parent.map { $0 as CFString }
        siblingIndex = 0
        window = object.window_id; role = object.role; subrole = object.subrole
        frame = object.frame; group = object.group
    }

    static func == (lhs: TreeContext, rhs: TreeContext) -> Bool {
        let sameParent: Bool
        switch (lhs.parentElement, rhs.parentElement) {
        case (nil, nil): sameParent = true
        case let (left?, right?): sameParent = CFEqual(left, right)
        default: sameParent = false
        }
        return CFEqual(lhs.element, rhs.element) && sameParent
            && lhs.siblingIndex == rhs.siblingIndex && lhs.window == rhs.window
            && lhs.role == rhs.role && lhs.subrole == rhs.subrole
            && lhs.frame == rhs.frame && lhs.group == rhs.group
    }
}

func unchangedAXTreeBaseline(previous: [TreeContext]?, previousLayer: String?,
                             current: [TreeContext], currentLayer: String?) -> Bool {
    guard let previous,
          previous.allSatisfy({ $0.siblingIndex != nil }),
          current.allSatisfy({ $0.siblingIndex != nil }) else { return false }
    return previous == current && previousLayer == currentLayer
}

struct ContentContext: Equatable {
    let title: String?
    let description: String?
    let value: UiValue?
    let enabled: Bool
    let actions: [String]
    let secure: Bool
    let focused: Bool
    let inView: Bool
    let scroll: ScrollState?
    init(_ object: UiObject) {
        title = object.title; description = object.description; value = object.value
        enabled = object.enabled; actions = object.actions; secure = object.secure
        focused = object.focused; inView = object.in_view; scroll = object.scroll
    }
}

/// The C callback's refcon is an unretained worker pointer. Its run-loop source
/// is installed only on this worker's thread and removed before worker release;
/// AXObserver never invokes this callback on an actor or unrelated thread.
private func sessionAXCallback(_ observer: AXObserver, _ element: AXUIElement,
                               _ notification: CFString, _ context: UnsafeMutableRawPointer?) {
    guard let context else { return }
    Unmanaged<AXSessionWorker>.fromOpaque(context).takeUnretainedValue().notified(notification, element: element)
}

private struct AXObserverRegistrationFailureDiagnostic: Encodable {
    let kind = "source_ax_observer_registration_failure"
    let notification: String
    let error_code: Int32
    let role: String
    let subrole: String?
    let frame: Rect?
    let window_id: String?
    let instance_ref: String?
    let identity_generation: UInt64
}

private struct AXObjectNotificationRegistration {
    let element: AXUIElement
    let notification: ObserverNotification
    let succeeded: Bool
    let errorCode: Int32?
}

private func typedAXNotification(_ name: String) -> ObserverNotification? {
    switch name {
    case kAXUIElementDestroyedNotification: return .uiElementDestroyed
    case "AXCreated": return .created
    case kAXWindowMovedNotification: return .moved
    case kAXWindowResizedNotification: return .resized
    case kAXLayoutChangedNotification: return .layoutChanged
    case kAXValueChangedNotification: return .valueChanged
    case kAXTitleChangedNotification: return .titleChanged
    case "AXRowCountChanged": return .rowCountChanged
    default: return nil
    }
}

private struct AXIdentityContinuityDiagnostic: Encodable {
    let kind = "source_ax_identity_continuity"
    let reason: String
    let inventory_changed: Bool
    let candidate_started: Bool
    let process_stable: Bool
    let root_proof_available: Bool
    let root_stable: Bool
    let observer_healthy: Bool
    let observer_stable_during_read: Bool
    let content_dirty_before_read: Bool
    let content_stable_during_read: Bool
    let candidate_count: Int
    let candidate_reused: Int
}

enum AXInvalidatingNotificationKind: String, Encodable {
    case elementDestroyed = "element_destroyed"
    case windowMoved = "window_moved"
    case windowResized = "window_resized"
    case windowCreated = "window_created"
    case focusedWindowChanged = "focused_window_changed"
    case focusedElementChanged = "focused_element_changed"
    case applicationActivated = "application_activated"
    case applicationDeactivated = "application_deactivated"
    case layoutChanged = "layout_changed"
    case sheetCreated = "sheet_created"
    case menuOpened = "menu_opened"
    case menuClosed = "menu_closed"
    case unknown
}

enum AXRetainedRoleCategory: String, Encodable {
    case windowRoot = "window_root"
    case textReceiver = "text_receiver"
    case button
    case group
    case staticText = "static_text"
    case other
    case none
    case unknown
}

func axRetainedRoleCategory(matchCount: Int, role: String?) -> AXRetainedRoleCategory {
    guard matchCount > 0 else { return .none }
    guard matchCount == 1, let role else { return .unknown }
    switch role {
    case kAXWindowRole: return .windowRoot
    case kAXTextFieldRole, kAXTextAreaRole: return .textReceiver
    case kAXButtonRole, kAXPopUpButtonRole, kAXMenuButtonRole, kAXCheckBoxRole, kAXRadioButtonRole:
        return .button
    case kAXGroupRole: return .group
    case kAXStaticTextRole: return .staticText
    default: return .other
    }
}

/// False means no retained CFEqual entry exists; nil means root proof or unique
/// retained identity is unavailable, so no window comparison is justified.
func axRetainedWindowMatchesSelectedRoot(matchCount: Int, cachedWindow: String?,
                                         selectedRootWindowID: String?) -> Bool? {
    guard let selectedRootWindowID else { return nil }
    if matchCount == 0 { return false }
    guard matchCount == 1, let cachedWindow else { return nil }
    return cachedWindow == selectedRootWindowID
}

func axOwnerRootMatchesSelectedRoot(matchCount: Int, cachedOwnerRoot: CFTypeRef?,
                                    selectedRoot: CFTypeRef?) -> Bool? {
    guard matchCount == 1, let cachedOwnerRoot, let selectedRoot else { return nil }
    return CFEqual(cachedOwnerRoot, selectedRoot)
}

func axWindowCreatedRootMatchCount(callbackElement: CFTypeRef,
                                   actualWindowRoots: [MenuRetainedToken]) -> Int {
    actualWindowRoots.reduce(into: 0) { count, root in
        if root.matches(callbackElement) { count += 1 }
    }
}

func axWindowCreatedMatchesVerifiedSelectedRoot(matchCount: Int,
                                                callbackElement: CFTypeRef,
                                                selectedRoot: CFTypeRef?) -> Bool? {
    guard matchCount == 1, let selectedRoot else { return nil }
    return CFEqual(callbackElement, selectedRoot)
}

func axSelectedRootsMatch(previous: CFTypeRef?, currentVerified: CFTypeRef?) -> Bool? {
    guard let previous, let currentVerified else { return nil }
    return CFEqual(previous, currentVerified)
}

func axInvalidatingNotificationKind(_ notification: String) -> AXInvalidatingNotificationKind {
    switch notification {
    case kAXUIElementDestroyedNotification: return .elementDestroyed
    case kAXWindowMovedNotification: return .windowMoved
    case kAXWindowResizedNotification: return .windowResized
    case kAXWindowCreatedNotification: return .windowCreated
    case kAXFocusedWindowChangedNotification: return .focusedWindowChanged
    case kAXFocusedUIElementChangedNotification: return .focusedElementChanged
    case kAXApplicationActivatedNotification: return .applicationActivated
    case kAXApplicationDeactivatedNotification: return .applicationDeactivated
    case kAXLayoutChangedNotification: return .layoutChanged
    case kAXSheetCreatedNotification: return .sheetCreated
    case kAXMenuOpenedNotification: return .menuOpened
    case kAXMenuClosedNotification: return .menuClosed
    default: return .unknown
    }
}

struct AXInvalidatingNotificationDiagnostic: Encodable {
    let kind = "source_ax_invalidating_notification"
    let notification_kind: AXInvalidatingNotificationKind
    let registry_generation: UInt64
    let observer_revision: UInt64
    let matches_retained_registry: Bool
    let matches_application: Bool
    let retained_role_category: AXRetainedRoleCategory
    let retained_window_matches_selected_root: Bool?
    let retained_owner_root_matches_selected_cf: Bool?
}

private struct PendingWindowCreatedDiagnostic {
    let callbackElement: MenuRetainedToken
    let previousSelectedRoot: MenuRetainedToken?
    let registryGeneration: UInt64
    let observerRevision: UInt64
}

struct AXWindowCreatedTraversalDiagnostic: Encodable {
    let event_registry_generation: UInt64
    let event_observer_revision: UInt64
    let traversal_registry_generation: UInt64
    let traversal_observer_revision_before: UInt64
    let traversal_observer_revision_after: UInt64
    let actual_ax_window_root_match_count: Int
    let callback_root_matches_current_selected_cf: Bool?
    let previous_selected_root_matches_current_verified_cf: Bool?
    let raw_traversal_complete: Bool
    let observer_healthy_at_traversal_start: Bool
    let observer_revision_stable: Bool
    let observer_available_after_traversal: Bool
    let observer_continuity_verified_after_traversal: Bool

    private enum CodingKeys: String, CodingKey {
        case kind, event_registry_generation, event_observer_revision
        case traversal_registry_generation, traversal_observer_revision_before
        case traversal_observer_revision_after, actual_ax_window_root_match_count
        case callback_root_matches_current_selected_cf
        case previous_selected_root_matches_current_verified_cf
        case raw_traversal_complete, observer_healthy_at_traversal_start
        case observer_revision_stable, observer_available_after_traversal
        case observer_continuity_verified_after_traversal
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode("source_ax_window_created_traversal", forKey: .kind)
        try container.encode(event_registry_generation, forKey: .event_registry_generation)
        try container.encode(event_observer_revision, forKey: .event_observer_revision)
        try container.encode(traversal_registry_generation, forKey: .traversal_registry_generation)
        try container.encode(traversal_observer_revision_before, forKey: .traversal_observer_revision_before)
        try container.encode(traversal_observer_revision_after, forKey: .traversal_observer_revision_after)
        try container.encode(actual_ax_window_root_match_count, forKey: .actual_ax_window_root_match_count)
        if let callback_root_matches_current_selected_cf {
            try container.encode(callback_root_matches_current_selected_cf, forKey: .callback_root_matches_current_selected_cf)
        } else {
            try container.encodeNil(forKey: .callback_root_matches_current_selected_cf)
        }
        if let previous_selected_root_matches_current_verified_cf {
            try container.encode(previous_selected_root_matches_current_verified_cf,
                                 forKey: .previous_selected_root_matches_current_verified_cf)
        } else {
            try container.encodeNil(forKey: .previous_selected_root_matches_current_verified_cf)
        }
        try container.encode(raw_traversal_complete, forKey: .raw_traversal_complete)
        try container.encode(observer_healthy_at_traversal_start, forKey: .observer_healthy_at_traversal_start)
        try container.encode(observer_revision_stable, forKey: .observer_revision_stable)
        try container.encode(observer_available_after_traversal, forKey: .observer_available_after_traversal)
        try container.encode(observer_continuity_verified_after_traversal,
                             forKey: .observer_continuity_verified_after_traversal)
    }
}

private func boundedAXDiagnosticString(_ value: String?, maximum: Int = 96) -> String? {
    guard let value else { return nil }
    return String(value.prefix(maximum))
}

private func writeBoundedAXDiagnostic<T: Encodable>(_ diagnostic: T, maximumBytes: Int) {
    guard var data = try? JSONEncoder().encode(diagnostic), data.count < maximumBytes else { return }
    data.append(10)
    let originalFlags = fcntl(STDERR_FILENO, F_GETFL)
    guard originalFlags >= 0,
          fcntl(STDERR_FILENO, F_SETFL, originalFlags | O_NONBLOCK) == 0 else { return }
    data.withUnsafeBytes { bytes in _ = Darwin.write(STDERR_FILENO, bytes.baseAddress!, bytes.count) }
    _ = fcntl(STDERR_FILENO, F_SETFL, originalFlags)
}

/// Best-effort private diagnostic: a single bounded nonblocking write, after
/// the AX registration call has returned and with no observer lock held.
private func writeAXObserverRegistrationFailure(_ diagnostic: AXObserverRegistrationFailureDiagnostic) {
    writeBoundedAXDiagnostic(diagnostic, maximumBytes: 1024)
}

/// Fixed facts only; never writes object values, IDs, labels, or fingerprints.
private func writeAXIdentityContinuityDiagnostic(_ diagnostic: AXIdentityContinuityDiagnostic) {
    writeBoundedAXDiagnostic(diagnostic, maximumBytes: 768)
}

private func writeAXInvalidatingNotificationDiagnostic(_ diagnostic: AXInvalidatingNotificationDiagnostic) {
    writeBoundedAXDiagnostic(diagnostic, maximumBytes: 768)
}

private func writeAXWindowCreatedTraversalDiagnostic(_ diagnostic: AXWindowCreatedTraversalDiagnostic) {
    writeBoundedAXDiagnostic(diagnostic, maximumBytes: 768)
}

/// The generic window inventory is capped at 32. The owned proof must know
/// whether that cap hid a second frame match, so it reads a separate bounded
/// inventory and declines the sidecar if more entries exist.
private func ownedDocumentTargetWindows(pid: UInt32) -> [NativeWindow]? {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] else { return nil }
    var result: [NativeWindow] = []
    for (index, info) in list.enumerated() {
        guard let owner = info[kCGWindowOwnerPID as String] as? NSNumber,
              owner.uint32Value == pid,
              let id = info[kCGWindowNumber as String] as? NSNumber,
              let layer = info[kCGWindowLayer as String] as? NSNumber,
              layer.intValue == 0,
              let bounds = info[kCGWindowBounds as String] as? [String: Any],
              let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { continue }
        let frame = Rect(cgFrame)
        guard frame.valid, frame.width > 0, frame.height > 0 else { continue }
        guard result.count < 512 else { return nil }
        result.append(NativeWindow(id: id.uint32Value, pid: pid, frame: frame, zIndex: UInt32(index)))
    }
    return result
}

private struct OwnedDocumentScopeAXNode {
    let element: MenuRetainedToken
    let parent: MenuRetainedToken?
    let role: OwnedDocumentAXRead<String>
    let subrole: OwnedDocumentAXRead<String>
    let modal: OwnedDocumentAXRead<Bool>
    let pid: UInt32?
    let children: OwnedDocumentAXElementArrayRead
}

private func canonicalOwnedDocumentPath(_ path: String) -> String? {
    var buffer = [CChar](repeating: 0, count: 4 * 1024)
    let resolved = path.withCString { realpath($0, &buffer) }
    guard resolved != nil else { return nil }
    let terminator = buffer.firstIndex(of: 0) ?? buffer.endIndex
    return String(bytes: buffer[..<terminator].map { UInt8(bitPattern: $0) }, encoding: .utf8)
}

private struct OwnedDocumentPathBindingDiagnostic: Encodable {
    let kind = "owned_document_binding"
    let before: OwnedDocumentPathReadEvidence
    let after: OwnedDocumentPathReadEvidence
    let verified: Bool
}

struct OwnedDocumentProjectionGateDiagnostic: Encodable {
    let kind = "owned_document_projection_gate"
    let sequence: UInt64
    let result: OwnedDocumentProjectionGate
    let observer_state: OwnedDocumentObserverState?
    let scope_complete: Bool?
    let profile_eligible: Bool?
    let gate_summary: OwnedDocumentProjectionGateSummary

    init(sequence: UInt64, result: OwnedDocumentProjectionGate,
         evidence: OwnedDocumentReceiverEvidence?,
         gateSummary: OwnedDocumentProjectionGateSummary) {
        self.sequence = sequence
        self.result = result
        self.observer_state = evidence?.observer_state
        self.scope_complete = evidence?.scope_complete
        self.profile_eligible = evidence?.profile_eligible
        self.gate_summary = gateSummary
    }
}

private final class AXSessionWorker {
    let app: AppIdentity
    let request: NativeRequest
    let registry: AXIdentityRegistry
    let menuRegistry = MenuAnchorRegistry()
    private lazy var focusReader = FocusContextReader(sessionID: menuRegistry.sessionID)
    private lazy var windowReader = WindowContextReader(sessionID: menuRegistry.sessionID)
    let application: AXUIElement
    let runLoop = CFRunLoopGetCurrent()
    var observer: AXObserver?
    var observerRevision: UInt64 = 0
    private var observerWatchGeneration: UInt64 = 0
    var treeRevision: UInt64 = 1
    var dirty = false
    var contentDirty = false
    var observerReason: String?
    var applicationObserved = false
    var previousProcess: ProcessStamp?
    let boundProcess: ProcessStamp?
    let ownedDocumentRegistration: OwnedDocumentReceiverRegistration?
    private var ownedObserverRegistrations: [OwnedDocumentObserverRegistrationRead] = []
    var previousWindows: [AXWindowContext]?
    var previousFrontmost: UInt32?
    var previousTree: [TreeContext]?
    var previousContent: [ContentContext]?
    var previousLayer: String?
    var previousRootProof: AXRootWindowProof?
    var previousSelectedRoot: MenuRetainedToken?
    private var captureRootBaseline: MenuRetainedToken?
    private var captureRootReplacementLatched = false
    private lazy var backgroundWindowReader: BackgroundWindowContextReader? = {
        guard let target = request.background_target, let process = boundProcess,
              let watchScope = request.watchScope else { return nil }
        let actual = NativeBackgroundProcessIdentity(pid: app.pid, start_seconds: process.seconds,
                                                      start_micros: process.micros)
        guard target.process == nil || target.process == actual else { return nil }
        return BackgroundWindowContextReader(app: app, sourceSessionID: watchScope.watchID,
            sessionEpoch: watchScope.sessionEpoch, targetWindowID: target.window_id,
            expectedProcess: process, nodeLimit: request.nodeLimit, depthLimit: request.depthLimit)
    }()
    private var pendingWindowCreatedDiagnostics: [PendingWindowCreatedDiagnostic] = []
    var readDeadline: UInt64 = 0
    private var objectRegistrations: [AXObjectNotificationRegistration] = []
    private var traversalActive = false
    private var localNotificationDuringRead = false
    private var globalNotificationDuringRead = false
    var registrationFailed = false
    var continuityVerified = false
    private var hasWrittenRequiredRegistrationFailureDiagnostic = false
    private var privateAXDiagnosticCount = 0
    let messagingTimeoutInstalled: Bool

    var observerRootAvailable: Bool {
        observer != nil && applicationObserved && !registrationFailed
    }

    init(app: AppIdentity, request: NativeRequest, expectedProcess: ProcessStamp?,
         ownedDocumentRegistration: OwnedDocumentReceiverRegistration?) {
        self.app = app; self.request = request
        boundProcess = expectedProcess
        self.ownedDocumentRegistration = ownedDocumentRegistration
        registry = AXIdentityRegistry(capacity: request.nodeLimit)
        application = AXUIElementCreateApplication(pid_t(app.pid))
        let globalTimeout = AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), 0.04)
        let applicationTimeout = AXUIElementSetMessagingTimeout(application, 0.04)
        messagingTimeoutInstalled = globalTimeout == .success && applicationTimeout == .success
    }

    func notified(_ name: CFString, element: AXUIElement) {
        let notification = name as String
        if request.background_target != nil
            && (notification == kAXApplicationActivatedNotification
                || notification == kAXApplicationDeactivatedNotification) {
            // These report OS focus transitions, not a mutation to the fixed
            // target tree. Global focus is sampled independently at publish.
            return
        }
        observerRevision += 1
        menuRegistry.notified(element: element, name: notification)
        if notification == kAXWindowCreatedNotification {
            // This is an application-root event. Keep the modal/window
            // diagnostic, then fail closed at global scope.
            if privateAXDiagnosticCount < 8, pendingWindowCreatedDiagnostics.count < 8 {
                privateAXDiagnosticCount += 1
                pendingWindowCreatedDiagnostics.append(PendingWindowCreatedDiagnostic(
                    callbackElement: MenuRetainedToken(element),
                    previousSelectedRoot: previousSelectedRoot,
                    registryGeneration: registry.generation,
                    observerRevision: observerRevision))
            }
            markGlobalUncertainty("application window inventory notification")
            return
        }

        guard !CFEqual(application, element),
              let typed = typedAXNotification(notification),
              registry.hasUniqueIdentityMatch(element) else {
            recordInvalidatingNotification(notification, element: element)
            markGlobalUncertainty("unmapped or global AX notification")
            return
        }

        if typed == .valueChanged || typed == .titleChanged { contentDirty = true }
        if [.uiElementDestroyed, .moved, .resized].contains(typed) {
            registry.invalidateObjects([element])
        }
        if traversalActive { localNotificationDuringRead = true }
        if privateAXDiagnosticCount < 8 {
            privateAXDiagnosticCount += 1
            writeAXInvalidatingNotificationDiagnostic(AXInvalidatingNotificationDiagnostic(
                notification_kind: axInvalidatingNotificationKind(notification),
                registry_generation: registry.generation,
                observer_revision: observerRevision,
                matches_retained_registry: true,
                matches_application: false,
                retained_role_category: registry.diagnosticRoleCategory(for: element),
                retained_window_matches_selected_root: registry.diagnosticSelectedRootWindowMatch(
                    for: element, selectedRootWindowID: previousRootProof?.windowID),
                retained_owner_root_matches_selected_cf: registry.diagnosticOwnerRootMatch(
                    for: element, selectedRoot: previousSelectedRoot?.element)))
        }
    }

    private func markGlobalUncertainty(_ reason: String) {
        dirty = true
        observerReason = reason
        if traversalActive { globalNotificationDuringRead = true }
    }

    private func recordInvalidatingNotification(_ notification: String, element: AXUIElement) {
        guard privateAXDiagnosticCount < 8 else { return }
        privateAXDiagnosticCount += 1
        writeAXInvalidatingNotificationDiagnostic(AXInvalidatingNotificationDiagnostic(
            notification_kind: axInvalidatingNotificationKind(notification),
            registry_generation: registry.generation,
            observer_revision: observerRevision,
            matches_retained_registry: registry.contains(element),
            matches_application: CFEqual(application, element),
            retained_role_category: registry.diagnosticRoleCategory(for: element),
            retained_window_matches_selected_root: registry.diagnosticSelectedRootWindowMatch(
                for: element, selectedRootWindowID: previousRootProof?.windowID),
            retained_owner_root_matches_selected_cf: registry.diagnosticOwnerRootMatch(
                for: element, selectedRoot: previousSelectedRoot?.element)))
    }

    func disposeObserver() {
        if let observer {
            CFRunLoopRemoveSource(runLoop, AXObserverGetRunLoopSource(observer), .defaultMode)
        }
        // Releasing the observer tears down its registrations as one bounded
        // lifecycle operation; do not make hundreds of synchronous remove RPCs.
        observer = nil
        applicationObserved = false
        ownedObserverRegistrations.removeAll(keepingCapacity: false)
        objectRegistrations.removeAll(keepingCapacity: false)
    }

    func invalidate() {
        registry.invalidate()
        treeRevision += 1
        previousTree = nil; previousContent = nil; previousLayer = nil
        previousRootProof = nil; previousSelectedRoot = nil
        disposeObserver()
        dirty = false; contentDirty = false
    }

    func register(_ element: AXUIElement, _ name: String, required: Bool,
                  context: AXElementContext? = nil, instanceRef: String? = nil,
                  objectScoped: Bool = false) -> Bool {
        let scoped = objectScoped || context != nil || instanceRef != nil
        let typedNotification = typedAXNotification(name)
        if scoped, let typedNotification,
           let existing = objectRegistrations.first(where: {
               $0.notification == typedNotification && CFEqual($0.element, element)
           }) {
            return existing.succeeded
        }
        guard let observer, uptimeNs() < readDeadline else {
            if required && !scoped {
                registrationFailed = true
                observerReason = "AX observer registration budget exceeded"
            }
            return false
        }
        guard configureBackgroundAXQuery(element) else { return false }
        let result = AXObserverAddNotification(observer, element, name as CFString,
            Unmanaged.passUnretained(self).toOpaque())
        let timely = request.background_target == nil || uptimeNs() < readDeadline
        let ok = timely && (result == .success || result == .notificationAlreadyRegistered)
        if scoped, let typedNotification, objectRegistrations.count < request.nodeLimit * 8 {
            objectRegistrations.append(AXObjectNotificationRegistration(element: element,
                notification: typedNotification, succeeded: ok,
                errorCode: ok ? nil : Int32(result.rawValue)))
        }
        if let notification = OwnedDocumentObserverNotification(axName: name as String) {
            let read = OwnedDocumentObserverRegistrationRead(element: MenuRetainedToken(element),
                notification: notification, attempted: true, errorCode: Int32(result.rawValue),
                returnedLate: uptimeNs() >= readDeadline)
            if let existing = ownedObserverRegistrations.firstIndex(where: {
                $0.notification == notification && $0.element.matches(element)
            }) {
                ownedObserverRegistrations[existing] = read
            } else {
                ownedObserverRegistrations.append(read)
            }
        }
        if !ok, required {
            if !scoped {
                registrationFailed = true
                observerReason = "AX notification coverage unavailable: \(name) (\(result.rawValue))"
            }
            if !hasWrittenRequiredRegistrationFailureDiagnostic {
                hasWrittenRequiredRegistrationFailureDiagnostic = true
                let diagnostic = AXObserverRegistrationFailureDiagnostic(
                    notification: boundedAXDiagnosticString(name) ?? "unknown",
                    error_code: Int32(result.rawValue),
                    role: boundedAXDiagnosticString(context?.role) ?? "AXApplication",
                    subrole: boundedAXDiagnosticString(context?.subrole),
                    frame: context?.frame.valid == true ? context?.frame : nil,
                    window_id: boundedAXDiagnosticString(context?.window),
                    instance_ref: boundedAXDiagnosticString(instanceRef),
                    identity_generation: registry.generation)
                writeAXObserverRegistrationFailure(diagnostic)
            }
        }
        return ok
    }

    private func configureBackgroundAXQuery(_ element: AXUIElement) -> Bool {
        guard request.background_target != nil else { return true }
        let now = uptimeNs()
        guard now < readDeadline,
              let timeout = backgroundAXMessagingTimeoutSeconds(remainingNanoseconds: readDeadline - now),
              AXUIElementSetMessagingTimeout(element, timeout) == .success,
              uptimeNs() < readDeadline else { return false }
        return true
    }

    func ensureObserver() {
        guard observer == nil else { return }
        guard messagingTimeoutInstalled, uptimeNs() < readDeadline else {
            observerReason = "AX observer messaging timeout unavailable"
            registrationFailed = true
            return
        }
        var created: AXObserver?
        let result = AXObserverCreate(pid_t(app.pid), sessionAXCallback, &created)
        guard result == .success, let created else {
            observerReason = "AX observer unavailable (\(result.rawValue))"
            registrationFailed = true
            return
        }
        observer = created
        observerWatchGeneration &+= 1
        CFRunLoopAddSource(runLoop, AXObserverGetRunLoopSource(created), .defaultMode)
        let required = [kAXFocusedWindowChangedNotification, kAXFocusedUIElementChangedNotification,
                        kAXWindowCreatedNotification, kAXApplicationActivatedNotification,
                        kAXApplicationDeactivatedNotification]
        applicationObserved = true
        for name in required {
            if !register(application, name, required: true) { applicationObserved = false }
        }
        for name in [kAXLayoutChangedNotification, kAXValueChangedNotification,
                     kAXTitleChangedNotification, kAXSheetCreatedNotification,
                     kAXMenuOpenedNotification, kAXMenuClosedNotification] {
            _ = register(application, name, required: false)
        }
    }

    private func elementBelongsToTarget(_ element: AXUIElement) -> Bool {
        guard uptimeNs() < readDeadline, configureBackgroundAXQuery(element) else { return false }
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success && pid > 0
            && UInt32(pid) == app.pid && uptimeNs() < readDeadline
    }

    private func elementLifetimeCovered(_ element: AXUIElement, role: String?,
                                        context: AXElementContext? = nil) -> Bool {
        guard observerRootAvailable, elementBelongsToTarget(element) else { return false }
        let destroyed = register(element, kAXUIElementDestroyedNotification, required: true,
            context: context, objectScoped: true)
        guard destroyed else { return false }
        guard let role else { return false }
        guard role == kAXWindowRole || role == kAXSheetRole else { return true }
        let moved = register(element, kAXWindowMovedNotification, required: true,
            context: context, objectScoped: true)
        let resized = register(element, kAXWindowResizedNotification, required: true,
            context: context, objectScoped: true)
        return moved && resized
    }

    func identityAssignment(_ element: AXUIElement, role: String, subrole: String?, frame: Rect,
                            window: String, ownerWindowRoot: MenuRetainedToken) -> AXIdentityAssignment? {
        guard uptimeNs() < readDeadline else {
            observerReason = "AX identity registration exceeded read budget"
            return nil
        }
        let context = AXElementContext(role: role, subrole: subrole, frame: frame, window: window)
        let lifetimeCovered = elementLifetimeCovered(element, role: role, context: context)
        switch registry.assign(element, context: context, ownerRoot: ownerWindowRoot.element,
                               continuityCovered: lifetimeCovered) {
        case .existing(let id, let continuity), .inserted(let id, let continuity):
            return AXIdentityAssignment(instance_ref: id, identity_continuity: continuity)
        case .changed:
            markGlobalUncertainty("AX proxy changed context within one traversal")
            return nil
        case .full:
            observerReason = "AX per-read identity token capacity exceeded"
            return nil
        }
    }

    func identity(_ element: AXUIElement, role: String, subrole: String?, frame: Rect,
                  window: String, ownerWindowRoot: MenuRetainedToken) -> String? {
        identityAssignment(element, role: role, subrole: subrole, frame: frame,
            window: window, ownerWindowRoot: ownerWindowRoot)?.instance_ref
    }

    private func observerCoverageFailures(for reader: AXReader,
                                          objects: [UiObject]) -> [ObserverRegistrationFailure] {
        var failures: [ObserverRegistrationFailure] = []
        var seen = Set<String>()
        for registration in objectRegistrations {
            guard let errorCode = registration.errorCode, errorCode != 0,
                  let observed = reader.observedElements.first(where: {
                      CFEqual($0.element, registration.element)
                  }),
                  let object = objects.first(where: { $0.instance_ref == observed.id }),
                  object.identity_continuity == .readScoped else { continue }
            let key = "\(object.instance_ref)\u{0}\(registration.notification.rawValue)"
            guard seen.insert(key).inserted else { continue }
            failures.append(ObserverRegistrationFailure(instance_ref: object.instance_ref,
                notification: registration.notification, error_code: errorCode))
        }
        return failures
    }

    func pumpNotifications() {
        let result = CFRunLoopRunInMode(.defaultMode, 0.02, true)
        if result == .finished { Thread.sleep(forTimeInterval: 0.02) }
    }

    private func takePendingWindowCreatedDiagnostics(through observerRevision: UInt64)
        -> [PendingWindowCreatedDiagnostic]
    {
        let count = pendingWindowCreatedDiagnostics.prefix {
            $0.observerRevision <= observerRevision
        }.count
        guard count > 0 else { return [] }
        let pending = Array(pendingWindowCreatedDiagnostics.prefix(count))
        pendingWindowCreatedDiagnostics.removeFirst(count)
        return pending
    }

    private func recordWindowCreatedDiagnostics(
        _ events: [PendingWindowCreatedDiagnostic],
        reader: AXReader,
        rawTraversalComplete: Bool,
        observerHealthyAtTraversalStart: Bool,
        observerRevisionBefore: UInt64,
        observerRevisionStable: Bool,
        observerAvailableAfterTraversal: Bool,
        observerContinuityVerifiedAfterTraversal: Bool,
        currentVerifiedSelectedRoot: MenuRetainedToken?
    ) {
        for event in events {
            let rootMatchCount = axWindowCreatedRootMatchCount(
                callbackElement: event.callbackElement.element,
                actualWindowRoots: reader.actualAXWindowRoots)
            let diagnostic = AXWindowCreatedTraversalDiagnostic(
                event_registry_generation: event.registryGeneration,
                event_observer_revision: event.observerRevision,
                traversal_registry_generation: registry.generation,
                traversal_observer_revision_before: observerRevisionBefore,
                traversal_observer_revision_after: observerRevision,
                actual_ax_window_root_match_count: rootMatchCount,
                callback_root_matches_current_selected_cf: axWindowCreatedMatchesVerifiedSelectedRoot(
                    matchCount: rootMatchCount,
                    callbackElement: event.callbackElement.element,
                    selectedRoot: currentVerifiedSelectedRoot?.element),
                previous_selected_root_matches_current_verified_cf: axSelectedRootsMatch(
                    previous: event.previousSelectedRoot?.element,
                    currentVerified: currentVerifiedSelectedRoot?.element),
                raw_traversal_complete: rawTraversalComplete,
                observer_healthy_at_traversal_start: observerHealthyAtTraversalStart,
                observer_revision_stable: observerRevisionStable,
                observer_available_after_traversal: observerAvailableAfterTraversal,
                observer_continuity_verified_after_traversal: observerContinuityVerifiedAfterTraversal)
            writeAXWindowCreatedTraversalDiagnostic(diagnostic)
        }
    }

    private func readOwnedDocumentScopeOnly(scope: OwnedDocumentReadScope,
                                            registration: OwnedDocumentReceiverRegistration,
                                            frontmostPID: UInt32?,
                                            wallStarted: UInt64,
                                            start: UInt64) -> AXSessionRead {
        func finish(evidence: OwnedDocumentReceiverEvidence?, observerAvailable: Bool,
                    observerReason: String?) -> AXSessionRead {
            continuityVerified = false
            var finalEvidence = evidence
            var finalObserverAvailable = observerAvailable
            var finalObserverReason = observerReason
            var finalOwnedBoundary: SourceIdentityBoundary?
            let finishedMS = unixMilliseconds()
            if let evidence {
                let registryBoundary = registry.ownedScopeIdentityBoundary(complete: evidence.scope_complete)
                let finalProvenance = registryBoundary.flatMap {
                    scope.provenance(identityBoundary: $0,
                        readStartedMS: wallStarted, readFinishedMS: finishedMS)
                }
                if let returnedProof = ownedDocumentProofForReturn(evidence: evidence,
                        actualRegistryBoundary: registryBoundary, finalProvenance: finalProvenance) {
                    finalEvidence = returnedProof.evidence
                    finalOwnedBoundary = returnedProof.identityBoundary
                } else {
                    registry.invalidateCurrentReadObjects()
                    finalEvidence = nil
                    finalObserverAvailable = observerRootAvailable
                    finalObserverReason = "owned scope provenance expired before return"
                }
            }
            var structure = Structure(app: app)
            structure.complete = false
            structure.incomplete_reason = "generic AX Structure was not observed in owned-scope mode"
            structure.owned_document_evidence = finalEvidence
            let finishedNs = uptimeNs()
            return AXSessionRead(read: StructureRead(structure: structure, started: start,
                finished: finishedNs, secureFrames: [], privacyRejection: .readUnavailable), tree_revision: treeRevision,
                observer_revision: observerRevision, observer_available: finalObserverAvailable,
                observer_reason: finalObserverReason, started_unix_ms: wallStarted,
                finished_unix_ms: finishedMS, identity_boundary: nil,
                owned_document_identity_boundary: finalOwnedBoundary,
                owned_document_evidence: finalEvidence)
        }

        func fail(_ reason: String, resetGlobal: Bool = false) -> AXSessionRead {
            let globalReset = resetGlobal || dirty || registrationFailed || !observerRootAvailable
            if globalReset { invalidate() }
            else { registry.invalidateCurrentReadObjects() }
            return finish(evidence: nil, observerAvailable: !globalReset && observerRootAvailable,
                          observerReason: reason)
        }

        _ = CFRunLoopRunInMode(.defaultMode, 0, true)

        func pid(_ token: MenuRetainedToken?) -> UInt32? {
            guard let token else { return nil }
            var value: pid_t = 0
            guard AXUIElementGetPid(unsafeDowncast(token.element, to: AXUIElement.self), &value) == .success,
                  value > 0 else { return nil }
            return UInt32(value)
        }

        func executablePath(_ pid: UInt32) -> String? {
            guard pid > 0, pid <= UInt32(Int32.max) else { return nil }
            var buffer = [CChar](repeating: 0, count: 4 * 1024)
            let count = buffer.withUnsafeMutableBufferPointer {
                proc_pidpath(pid_t(pid), $0.baseAddress, UInt32($0.count))
            }
            guard count > 0 else { return nil }
            let terminator = buffer.firstIndex(of: 0) ?? buffer.endIndex
            return String(bytes: buffer[..<terminator].map { UInt8(bitPattern: $0) }, encoding: .utf8)
        }

        func documentPathMatch(_ read: OwnedDocumentAXRead<URL>) -> Bool {
            guard read.observation.status == .value, read.timely,
                  let url = read.value, isLocalOwnedDocumentFileURL(url),
                  let expected = canonicalOwnedDocumentPath(registration.documentCanonicalPath),
                  let actual = canonicalOwnedDocumentPath(url.path) else { return false }
            return expected == actual
        }

        func documentReadEvidence(_ read: OwnedDocumentAXRead<URL>, observedMS: UInt64,
                                  pathMatches: Bool) -> OwnedDocumentPathReadEvidence {
            OwnedDocumentPathReadEvidence(status: read.observation.status,
                api_code: read.observation.status == .value ? AXError.success.rawValue : read.observation.code,
                cf_type_id: read.cfTypeID.map(UInt64.init), observed_ms: observedMS,
                timely: read.timely, registered_path_matches: pathMatches)
        }

        guard registration.pid == app.pid, boundProcess == registration.process,
              ProcessStamp.read(pid: app.pid) == registration.process,
              frontmostPID == nil || frontmostPID == app.pid else {
            return fail("owned scope registration or process identity mismatch", resetGlobal: true)
        }
        if dirty { invalidate() }
        observerReason = nil
        registrationFailed = false
        registry.beginRead()
        ensureObserver()
        let generationBefore = registry.generation
        let revisionBefore = observerRevision
        let watchGenerationBefore = observerWatchGeneration
        let observerAvailableBefore = observerRootAvailable
        guard observerAvailableBefore else { return fail("owned scope application observer unavailable", resetGlobal: true) }

        let processBefore = ProcessStamp.read(pid: app.pid)
        guard let cgWindowsBefore = ownedDocumentTargetWindows(pid: app.pid) else {
            return fail("owned scope CG window inventory unavailable", resetGlobal: true)
        }
        let ax = OwnedDocumentAXReader(deadline: readDeadline, maximumArrayElements: request.nodeLimit)
        let applicationToken = MenuRetainedToken(application)
        let system = AXUIElementCreateSystemWide()
        let focusedApplicationBefore = ax.axElement(system, attribute: kAXFocusedApplicationAttribute)
        let focusedApplicationBeforePID = pid(focusedApplicationBefore.value)
        let applicationPID = pid(applicationToken)
        let windowsBefore = ax.elements(application, attribute: kAXWindowsAttribute,
                                        maximum: request.nodeLimit)
        let focusedWindowBefore = ax.axElement(application, attribute: kAXFocusedWindowAttribute)
        let systemReceiverBefore = ax.axElement(system, attribute: kAXFocusedUIElementAttribute)
        let applicationReceiverBefore = ax.axElement(application, attribute: kAXFocusedUIElementAttribute)
        guard windowsBefore.complete, windowsBefore.elements.count == 1,
              focusedApplicationBefore.value?.matches(applicationToken.element) == true,
              applicationReceiverBefore.value != nil,
              systemReceiverBefore.value?.matches(applicationReceiverBefore.value!.element) == true,
              focusedWindowBefore.value?.matches(windowsBefore.elements[0].element) == true else {
            return fail("owned scope app focus or complete AX window inventory unavailable", resetGlobal: true)
        }

        let windowToken = windowsBefore.elements[0]
        let receiverToken = systemReceiverBefore.value!
        let windowAX = unsafeDowncast(windowToken.element, to: AXUIElement.self)
        let receiverAX = unsafeDowncast(receiverToken.element, to: AXUIElement.self)
        let windowRole = ax.string(windowAX, attribute: kAXRoleAttribute)
        let windowSubrole = ax.string(windowAX, attribute: kAXSubroleAttribute)
        let windowModal = ax.boolean(windowAX, attribute: kAXModalAttribute)
        let windowFrameBefore = ax.frame(windowAX)
        let documentBefore = ax.documentURL(windowAX)
        let documentBeforeMS = unixMilliseconds()
        guard let windowRoleValue = windowRole.value, windowRole.timely,
              let windowSubroleValue = windowSubrole.value, windowSubrole.timely,
              windowRoleValue == kAXWindowRole,
              windowSubroleValue == kAXStandardWindowSubrole,
              windowModal.observation.status == .value, windowModal.timely,
              windowModal.value == false,
              let windowFrameValue = windowFrameBefore.value, windowFrameBefore.timely,
              let cgWindowBefore = matchAXWindow(windowFrameValue, windows: cgWindowsBefore),
              cgWindowBefore.pid == app.pid,
              let rootID = identity(windowAX, role: windowRoleValue, subrole: windowSubroleValue,
                  frame: windowFrameValue, window: cgWindowBefore.ref, ownerWindowRoot: windowToken),
              registry.uniqueObjectRef(for: windowToken.element) == rootID,
              register(windowAX, kAXLayoutChangedNotification, required: true,
                  context: AXElementContext(role: windowRoleValue, subrole: windowSubroleValue,
                      frame: windowFrameValue, window: cgWindowBefore.ref), instanceRef: rootID) else {
            return fail("owned scope selected window identity or observer registration failed")
        }

        let nodeLimit = max(1, min(request.nodeLimit, 512))
        let depthLimit = max(0, min(request.depthLimit, 64))
        var nodes: [OwnedDocumentScopeAXNode] = []
        var transientFound = false
        var secureNodes: [MenuRetainedToken] = []
        var unknownTextRoleNodes: [MenuRetainedToken] = []
        var treeComplete = true
        func visit(_ token: MenuRetainedToken, parent: MenuRetainedToken?, depth: Int) {
            guard treeComplete else { return }
            guard depth <= depthLimit, nodes.count < nodeLimit,
                  !nodes.contains(where: { $0.element.matches(token.element) }),
                  let nodePID = pid(token), nodePID == app.pid else {
                treeComplete = false
                return
            }
            let element = unsafeDowncast(token.element, to: AXUIElement.self)
            let role = ax.string(element, attribute: kAXRoleAttribute)
            let subrole = ax.string(element, attribute: kAXSubroleAttribute)
            let modal = ax.boolean(element, attribute: kAXModalAttribute)
            guard role.observation.status == .value, role.timely,
                  let roleValue = role.value, !roleValue.isEmpty,
                  subrole.timely else {
                treeComplete = false
                return
            }
            if modal.observation.status == .value && modal.value == true
                || [kAXSheetRole, "AXDialog", "AXPopover", kAXMenuRole].contains(roleValue) {
                transientFound = true
            }
            if roleValue == "AXSecureTextField"
                || subrole.observation.status == .value && subrole.value == kAXSecureTextFieldSubrole {
                secureNodes.append(token)
            }
            if [kAXTextFieldRole, kAXTextAreaRole, kAXComboBoxRole].contains(roleValue) {
                let subroleKnownPlain = ownedTextSubroleIsKnownPlain(subrole,
                    isCurrentReceiver: token.matches(receiverToken.element))
                if !subroleKnownPlain { unknownTextRoleNodes.append(token) }
            }
            let remaining = nodeLimit - nodes.count
            let children = ax.elements(element, attribute: kAXChildrenAttribute,
                                        maximum: max(1, remaining))
            let childTokens: [MenuRetainedToken]
            if children.complete {
                childTokens = children.elements
            } else if children.observation.status == .noValue
                        && children.observation.code == AXError.noValue.rawValue
                        && !children.returnedLate && children.elements.isEmpty {
                childTokens = []
            } else {
                treeComplete = false
                return
            }
            nodes.append(OwnedDocumentScopeAXNode(element: token, parent: parent,
                role: role, subrole: subrole, modal: modal, pid: nodePID, children: children))
            for child in childTokens { visit(child, parent: token, depth: depth + 1) }
        }
        visit(windowToken, parent: nil, depth: 0)
        guard treeComplete, let rootNode = nodes.first(where: { $0.element.matches(windowToken.element) }),
              rootNode.children.complete else {
            return fail("owned scope selected window subtree incomplete or truncated")
        }
        let receiverOccurrences = nodes.filter { $0.element.matches(receiverToken.element) }
        guard receiverOccurrences.count == 1 else {
            return fail("owned scope focused receiver is not unique in selected window")
        }

        func node(_ token: MenuRetainedToken) -> OwnedDocumentScopeAXNode? {
            let matches = nodes.filter { $0.element.matches(token.element) }
            return matches.count == 1 ? matches[0] : nil
        }

        var ancestorRelations: [OwnedDocumentAncestorRelationRead] = []
        var childToken = receiverToken
        var visitedChain: [MenuRetainedToken] = [receiverToken]
        var ancestorChainComplete = false
        for _ in 0..<8 {
            guard let childNode = node(childToken), let expectedParent = childNode.parent else { break }
            let childAX = unsafeDowncast(childToken.element, to: AXUIElement.self)
            let parentAXRead = ax.axElement(childAX, attribute: kAXParentAttribute)
            guard let actualParent = parentAXRead.value,
                  actualParent.matches(expectedParent.element),
                  let parentNode = node(actualParent),
                  parentNode.children.complete,
                  parentNode.children.membershipCount(of: childToken.element) == 1,
                  !visitedChain.contains(where: { $0.matches(actualParent.element) }) else {
                return fail("owned scope focused receiver parent chain incomplete or changed")
            }
            let parentAX = unsafeDowncast(actualParent.element, to: AXUIElement.self)
            let parentFrame = actualParent.matches(windowToken.element)
                ? windowFrameBefore : ax.frame(parentAX)
            guard parentNode.role.timely, let parentRole = parentNode.role.value,
                  parentNode.subrole.timely,
                  let parentRect = parentFrame.value, parentFrame.timely,
                  let parentID = identity(parentAX, role: parentRole,
                      subrole: parentNode.subrole.value, frame: parentRect,
                      window: cgWindowBefore.ref, ownerWindowRoot: windowToken),
                  registry.uniqueObjectRef(for: actualParent.element) == parentID else {
                return fail("owned scope ancestor identity could not be assigned in registry")
            }
            ancestorRelations.append(OwnedDocumentAncestorRelationRead(child: childToken,
                parent: actualParent, parentAttribute: parentAXRead,
                parentChildren: parentNode.children, parentPID: parentNode.pid,
                parentObjectRef: parentID, parentRole: parentNode.role))
            if actualParent.matches(windowToken.element) {
                ancestorChainComplete = true
                break
            }
            childToken = actualParent
            visitedChain.append(actualParent)
        }
        guard ancestorChainComplete else { return fail("owned scope receiver ancestry exceeded bound") }

        let chainContains: (MenuRetainedToken) -> Bool = { token in
            visitedChain.contains(where: { $0.matches(token.element) })
        }
        guard !secureNodes.contains(where: { !chainContains($0) }),
              !unknownTextRoleNodes.contains(where: { !chainContains($0) }),
              !secureNodes.contains(where: { !$0.matches(receiverToken.element) && chainContains($0) }),
              !unknownTextRoleNodes.contains(where: { !$0.matches(receiverToken.element) && chainContains($0) }),
              !transientFound else {
            return fail("owned scope window contains secure, unknown text, modal, or transient content")
        }

        let receiverNode = receiverOccurrences[0]
        let receiverFrameBefore = ax.frame(receiverAX)
        guard let receiverFrameValue = receiverFrameBefore.value, receiverFrameBefore.timely,
              let receiverRoleValue = receiverNode.role.value, receiverNode.role.timely,
              receiverNode.subrole.timely,
              let receiverID = identity(receiverAX, role: receiverRoleValue,
                  subrole: receiverNode.subrole.value, frame: receiverFrameValue,
                  window: cgWindowBefore.ref, ownerWindowRoot: windowToken),
              registry.uniqueObjectRef(for: receiverToken.element) == receiverID else {
            return fail("owned scope receiver identity could not be assigned in registry")
        }
        let receiverEnabled = ax.boolean(receiverAX, attribute: kAXEnabledAttribute)
        let receiverFocused = ax.boolean(receiverAX, attribute: kAXFocusedAttribute)
        let receiverSettable = ax.valueIsSettable(receiverAX)
        let receiverTitle = ax.string(receiverAX, attribute: kAXTitleAttribute)
        let receiverDescription = ax.string(receiverAX, attribute: kAXDescriptionAttribute)
        let markedBeforeRead = ax.markedRange(receiverAX)
        let markedBefore = OwnedDocumentMarkedRangeRead(receiver: receiverToken,
            observedMS: unixMilliseconds(), read: markedBeforeRead)

        let receiverFrameAfter = ax.frame(receiverAX)
        let windowFrameAfter = ax.frame(windowAX)
        let documentAfter = ax.documentURL(windowAX)
        let documentAfterMS = unixMilliseconds()
        let windowRoleAfter = ax.string(windowAX, attribute: kAXRoleAttribute)
        let windowSubroleAfter = ax.string(windowAX, attribute: kAXSubroleAttribute)
        let windowModalAfter = ax.boolean(windowAX, attribute: kAXModalAttribute)
        let focusedApplicationAfter = ax.axElement(system, attribute: kAXFocusedApplicationAttribute)
        let focusedWindowAfter = ax.axElement(application, attribute: kAXFocusedWindowAttribute)
        let systemReceiverAfter = ax.axElement(system, attribute: kAXFocusedUIElementAttribute)
        let applicationReceiverAfter = ax.axElement(application, attribute: kAXFocusedUIElementAttribute)
        let windowsAfter = ax.elements(application, attribute: kAXWindowsAttribute,
                                       maximum: request.nodeLimit)
        let cgWindowsAfter = ownedDocumentTargetWindows(pid: app.pid)
        let markedAfterRead = ax.markedRange(receiverAX)
        let markedAfter = OwnedDocumentMarkedRangeRead(receiver: receiverToken,
            observedMS: unixMilliseconds(), read: markedAfterRead)
        _ = CFRunLoopRunInMode(.defaultMode, 0, true)
        let observedExecutablePath = executablePath(app.pid)
        let observedBundleID = NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier
        let processAfter = ProcessStamp.read(pid: app.pid)
        let revisionAfter = observerRevision
        let generationAfter = registry.generation
        let watchGenerationAfter = observerWatchGeneration
        let observerAvailableAfter = observer != nil && applicationObserved && !registrationFailed
        let documentBeforeMatches = documentPathMatch(documentBefore)
        let documentAfterMatches = documentPathMatch(documentAfter)
        let documentBinding = OwnedDocumentPathBindingEvidence(
            before: documentReadEvidence(documentBefore, observedMS: documentBeforeMS,
                                         pathMatches: documentBeforeMatches),
            after: documentReadEvidence(documentAfter, observedMS: documentAfterMS,
                                         pathMatches: documentAfterMatches))
        let documentBindingVerified = documentBeforeMatches && documentAfterMatches
            && documentBefore.value?.isFileURL == true && documentAfter.value?.isFileURL == true
        if privateAXDiagnosticCount < 8 {
            privateAXDiagnosticCount += 1
            writeBoundedAXDiagnostic(OwnedDocumentPathBindingDiagnostic(before: documentBinding.before,
                after: documentBinding.after, verified: documentBindingVerified), maximumBytes: 768)
        }

        let cgWindowAfter = cgWindowsAfter.flatMap { matchAXWindow(windowFrameAfter.value ?? windowFrameValue, windows: $0) }
        let windowsStable = windowsAfter.complete && windowsAfter.elements.count == 1
            && windowsAfter.elements[0].matches(windowToken.element)
        let windowAttributesStable = windowRoleAfter.observation.status == .value
            && windowRoleAfter.timely && windowRoleAfter.value == windowRoleValue
            && windowSubroleAfter.observation.status == .value && windowSubroleAfter.timely
            && windowSubroleAfter.value == windowSubroleValue
            && windowModalAfter.observation.status == .value && windowModalAfter.timely
            && windowModalAfter.value == false
        guard processBefore == registration.process, processAfter == registration.process,
              applicationPID == registration.pid, focusedApplicationBeforePID == registration.pid,
              focusedApplicationAfter.value?.matches(applicationToken.element) == true,
              focusedWindowAfter.value?.matches(windowToken.element) == true,
              systemReceiverAfter.value?.matches(receiverToken.element) == true,
              applicationReceiverAfter.value?.matches(receiverToken.element) == true,
              windowsStable, windowAttributesStable,
              windowFrameBefore.value == windowFrameAfter.value,
              cgWindowAfter?.id == cgWindowBefore.id, cgWindowAfter?.pid == cgWindowBefore.pid,
              cgWindowAfter?.frame == cgWindowBefore.frame,
              documentBindingVerified,
              revisionBefore == revisionAfter, generationBefore == generationAfter,
              watchGenerationBefore == watchGenerationAfter,
              observerAvailableBefore && observerAvailableAfter, !dirty,
              let scopeBoundary = registry.ownedScopeIdentityBoundary(complete: treeComplete),
              let finishedProvenance = scope.provenance(identityBoundary: scopeBoundary,
                  readStartedMS: wallStarted, readFinishedMS: unixMilliseconds()) else {
            return fail("owned scope identity, document, focus, observer, or provenance changed")
        }

        let windowRead = OwnedDocumentReceiverWindowRead(element: windowToken,
            objectRef: rootID, role: windowRole, subrole: windowSubrole, modal: windowModal,
            frameBefore: windowFrameBefore, frameAfter: windowFrameAfter,
            cgWindowsBefore: cgWindowsBefore, cgWindowsAfter: cgWindowsAfter ?? [])
        let receiverRead = OwnedDocumentReceiverElementRead(element: receiverToken,
            objectRef: receiverID, pid: pid(receiverToken), frameBefore: receiverFrameBefore,
            frameAfter: receiverFrameAfter, role: receiverNode.role, subrole: receiverNode.subrole,
            enabled: receiverEnabled, focused: receiverFocused, valueSettable: receiverSettable,
            title: receiverTitle, description: receiverDescription)
        let facts = OwnedDocumentReceiverReadFacts(registration: registration,
            documentBindingVerified: documentBindingVerified, documentBinding: documentBinding,
            provenance: finishedProvenance, nowMS: unixMilliseconds(), ownedScopeReadComplete: treeComplete,
            noModalOrTransient: !transientFound, application: applicationToken,
            applicationPID: applicationPID, systemFocusedApplication: focusedApplicationBefore,
            systemFocusedApplicationPID: focusedApplicationBeforePID,
            applicationFocusedWindowBefore: focusedWindowBefore,
            applicationFocusedWindowAfter: focusedWindowAfter,
            systemFocusedReceiverBefore: systemReceiverBefore,
            systemFocusedReceiverAfter: systemReceiverAfter,
            applicationFocusedReceiverBefore: applicationReceiverBefore,
            applicationFocusedReceiverAfter: applicationReceiverAfter,
            systemFocusedReceiverObjectRef: registry.uniqueObjectRef(for: systemReceiverBefore.value!.element),
            applicationFocusedReceiverObjectRef: registry.uniqueObjectRef(for: applicationReceiverBefore.value!.element),
            actualWindows: windowsBefore, window: windowRead, receiver: receiverRead,
            ancestorRelations: ancestorRelations, ancestorChainComplete: ancestorChainComplete,
            ancestorChainTruncated: false, processBefore: processBefore, processAfter: processAfter,
            observedExecutablePath: observedExecutablePath, observedBundleID: observedBundleID,
            markedRangeBefore: markedBefore, markedRangeAfter: markedAfter,
            observer: OwnedDocumentObserverScopeRead(sameWatch: observerAvailableBefore && observerAvailableAfter
                    && watchGenerationBefore == watchGenerationAfter,
                observerAvailableBefore: observerAvailableBefore, observerAvailableAfter: observerAvailableAfter,
                generationBefore: generationBefore, generationAfter: generationAfter,
                revisionBefore: revisionBefore, revisionAfter: revisionAfter,
                registrations: ownedObserverRegistrations))
        let gateSummary = ownedDocumentProjectionGateSummary(facts)
        let projectedEvidence = projectOwnedDocumentReceiver(facts)
        let projectionGate = ownedDocumentProjectionGate(projectedEvidence)
        if privateAXDiagnosticCount < 8 {
            privateAXDiagnosticCount += 1
            writeBoundedAXDiagnostic(OwnedDocumentProjectionGateDiagnostic(
                sequence: scope.sourceSequence, result: projectionGate, evidence: projectedEvidence,
                gateSummary: gateSummary),
                maximumBytes: 768)
        }
        guard projectionGate == .accepted, var evidence = projectedEvidence else {
            return fail("owned scope projection gate: \(projectionGate.rawValue)")
        }
        // AXValue is read only after the existing source-owned profile gate
        // proves this exact receiver is an eligible plain-text setter.
        if evidence.profile_eligible && uptimeNs() < readDeadline {
            guard receiverToken.matches(receiverAX),
                  registry.uniqueObjectRef(for: receiverToken.element) == receiverID else {
                return fail("owned receiver identity changed before AXValue readback")
            }
            let valueRead = ax.boundedValue(receiverAX)
            let valueReadMS = unixMilliseconds()
            let valueReadback = projectOwnedDocumentValueReadback(
                valueRead,
                receiverObjectRef: receiverID,
                receiverSameCFAsActualObjectRef: receiverToken.matches(receiverAX),
                observedMS: valueReadMS,
                validUntilMS: evidence.provenance.validUntilMS)

            // Recheck the owned scope after the read. A digest from a receiver
            // that moved, changed document, process, focus or observer lifetime
            // is discarded with the rest of this evidence.
            let documentAfterValueRead = ax.documentURL(windowAX)
            let documentAfterValueReadMS = unixMilliseconds()
            let documentAfterValueReadMatches = documentPathMatch(documentAfterValueRead)
            let windowRoleAfterValueRead = ax.string(windowAX, attribute: kAXRoleAttribute)
            let windowSubroleAfterValueRead = ax.string(windowAX, attribute: kAXSubroleAttribute)
            let windowModalAfterValueRead = ax.boolean(windowAX, attribute: kAXModalAttribute)
            let windowFrameAfterValueRead = ax.frame(windowAX)
            let receiverFrameAfterValueRead = ax.frame(receiverAX)
            let receiverRoleAfterValueRead = ax.string(receiverAX, attribute: kAXRoleAttribute)
            let receiverSubroleAfterValueRead = ax.string(receiverAX, attribute: kAXSubroleAttribute)
            let receiverFocusedAfterValueRead = ax.boolean(receiverAX, attribute: kAXFocusedAttribute)
            let receiverEnabledAfterValueRead = ax.boolean(receiverAX, attribute: kAXEnabledAttribute)
            let receiverSettableAfterValueRead = ax.valueIsSettable(receiverAX)
            let markedRangeAfterValueRead = ax.markedRange(receiverAX)
            let markedRangeAfterValueReadMS = unixMilliseconds()
            let focusedApplicationAfterValueRead = ax.axElement(system, attribute: kAXFocusedApplicationAttribute)
            let focusedWindowAfterValueRead = ax.axElement(application, attribute: kAXFocusedWindowAttribute)
            let systemReceiverAfterValueRead = ax.axElement(system, attribute: kAXFocusedUIElementAttribute)
            let applicationReceiverAfterValueRead = ax.axElement(application, attribute: kAXFocusedUIElementAttribute)
            let windowsAfterValueRead = ax.elements(application, attribute: kAXWindowsAttribute,
                                                     maximum: request.nodeLimit)
            let cgWindowsAfterValueRead = ownedDocumentTargetWindows(pid: app.pid)
            _ = CFRunLoopRunInMode(.defaultMode, 0, true)
            let processAfterValueRead = ProcessStamp.read(pid: app.pid)
            let applicationAfterValueRead = NSRunningApplication(processIdentifier: pid_t(app.pid))
            let executableAfterValueRead = executablePath(app.pid)
            let bundleAfterValueRead = applicationAfterValueRead?.bundleIdentifier
            let windowFrameValueAfterRead = windowFrameAfterValueRead.value
            let cgWindowAfterValueRead = cgWindowsAfterValueRead.flatMap {
                matchAXWindow(windowFrameValueAfterRead ?? windowFrameValue, windows: $0)
            }
            let readbackFinishedMS = unixMilliseconds()
            let identityBoundaryAfterValueRead = registry.ownedScopeIdentityBoundary(complete: treeComplete)
            let provenanceAfterValueRead = identityBoundaryAfterValueRead.flatMap {
                scope.provenance(identityBoundary: $0,
                    readStartedMS: wallStarted, readFinishedMS: readbackFinishedMS)
            }
            let receiverRangeBeforeValueRead = evidence.marked_range_before.observation.value
            let receiverRangeAfterValueRead = markedRangeAfterValueRead.value
            let markedRangeStillUnchanged = markedRangeAfterValueRead.observation.status == .value
                && markedRangeAfterValueRead.timely
                && receiverToken.matches(receiverAX)
                && receiverRangeBeforeValueRead?.length == 0
                && receiverRangeAfterValueRead == receiverRangeBeforeValueRead
                && markedRangeAfterValueReadMS >= valueReadMS
                && markedRangeAfterValueReadMS < evidence.provenance.validUntilMS
            let receiverStillEnabled = receiverEnabledAfterValueRead.timely &&
                ((receiverEnabledAfterValueRead.observation.status == .value &&
                  receiverEnabledAfterValueRead.value == true) ||
                 (receiverEnabledAfterValueRead.observation.status == .unsupported &&
                  receiverEnabledAfterValueRead.observation.code == AXError.attributeUnsupported.rawValue))

            guard uptimeNs() < readDeadline,
                  receiverToken.matches(receiverAX),
                  documentAfterValueReadMatches,
                  documentAfterValueRead.value?.isFileURL == true,
                  documentAfterValueReadMS >= valueReadMS,
                  documentAfterValueReadMS < evidence.provenance.validUntilMS,
                  processAfterValueRead == registration.process,
                  applicationAfterValueRead?.isTerminated == false,
                  bundleAfterValueRead == registration.bundleID,
                  executableAfterValueRead == registration.executablePath,
                  windowRoleAfterValueRead.observation.status == .value,
                  windowRoleAfterValueRead.timely, windowRoleAfterValueRead.value == windowRoleValue,
                  windowSubroleAfterValueRead.observation.status == .value,
                  windowSubroleAfterValueRead.timely, windowSubroleAfterValueRead.value == windowSubroleValue,
                  windowModalAfterValueRead.observation.status == .value,
                  windowModalAfterValueRead.timely, windowModalAfterValueRead.value == false,
                  windowFrameAfterValueRead.timely,
                  windowFrameValueAfterRead == windowFrameValue,
                  receiverFrameAfterValueRead.timely,
                  receiverFrameAfterValueRead.value == receiverFrameAfter.value,
                  receiverRoleAfterValueRead.observation.status == .value,
                  receiverRoleAfterValueRead.timely, receiverRoleAfterValueRead.value == kAXTextAreaRole,
                  receiverSubroleAfterValueRead.timely,
                  ownedTextSubroleIsKnownPlain(receiverSubroleAfterValueRead, isCurrentReceiver: true),
                  receiverFocusedAfterValueRead.observation.status == .value,
                  receiverFocusedAfterValueRead.timely, receiverFocusedAfterValueRead.value == true,
                  receiverStillEnabled,
                  receiverSettableAfterValueRead.observation.status == .value,
                  receiverSettableAfterValueRead.timely, receiverSettableAfterValueRead.value == true,
                  markedRangeStillUnchanged,
                  focusedApplicationAfterValueRead.value?.matches(applicationToken.element) == true,
                  focusedApplicationAfterValueRead.timely,
                  focusedWindowAfterValueRead.value?.matches(windowToken.element) == true,
                  focusedWindowAfterValueRead.timely,
                  systemReceiverAfterValueRead.value?.matches(receiverToken.element) == true,
                  systemReceiverAfterValueRead.timely,
                  applicationReceiverAfterValueRead.value?.matches(receiverToken.element) == true,
                  applicationReceiverAfterValueRead.timely,
                  windowsAfterValueRead.complete, windowsAfterValueRead.elements.count == 1,
                  windowsAfterValueRead.elements[0].matches(windowToken.element),
                  cgWindowAfterValueRead?.id == cgWindowBefore.id,
                  cgWindowAfterValueRead?.pid == cgWindowBefore.pid,
                  cgWindowAfterValueRead?.frame == cgWindowBefore.frame,
                  registry.uniqueObjectRef(for: windowToken.element) == rootID,
                  registry.uniqueObjectRef(for: receiverToken.element) == receiverID,
                  registry.generation == generationAfter,
                  identityBoundaryAfterValueRead == scopeBoundary,
                  provenanceAfterValueRead == evidence.provenance,
                  observerRevision == revisionAfter,
                  observerWatchGeneration == watchGenerationAfter,
                  observer != nil, applicationObserved, !registrationFailed, !dirty else {
                return fail("owned scope changed after AXValue readback")
            }
            evidence.value_readback = valueReadback
        }
        _ = CFRunLoopRunInMode(.defaultMode, 0, true)
        guard observerRevision == revisionAfter, registry.generation == generationAfter,
              observerWatchGeneration == watchGenerationAfter,
              observer != nil, applicationObserved, !registrationFailed, !dirty else {
            return fail("owned scope observer or registry changed before return")
        }
        return finish(evidence: evidence, observerAvailable: true, observerReason: nil)
    }


    func read(frontmostPID: UInt32?, ownedDocumentScope: OwnedDocumentReadScope? = nil,
              backgroundAXOnly: Bool = false) -> AXSessionRead {
        let wallStarted = unixMilliseconds()
        let start = uptimeNs()
        readDeadline = start + UInt64(min(request.timeout / 2, 750)) * 1_000_000
        if backgroundAXOnly {
            return readBackgroundAXOnly(wallStarted: wallStarted, start: start)
        }
        if let ownedDocumentScope, let ownedDocumentRegistration {
            return readOwnedDocumentScopeOnly(scope: ownedDocumentScope,
                registration: ownedDocumentRegistration, frontmostPID: frontmostPID,
                wallStarted: wallStarted, start: start)
        }
        _ = CFRunLoopRunInMode(.defaultMode, 0, true)
        guard let process = ProcessStamp.read(pid: app.pid) else {
            return unavailable(reason: "AX target process identity unavailable", menuReason: .processUnavailable,
                privacyRejection: .processUnavailable, wallStarted: wallStarted, start: start)
        }
        guard let boundProcess, process == boundProcess else {
            return unavailable(reason: "AX target process changed; a new session is required", menuReason: .processChanged,
                privacyRejection: .processUnavailable, wallStarted: wallStarted, start: start)
        }
        let windows = targetWindows(pid: app.pid).map { AXWindowContext(id: $0.id, frame: $0.frame, zIndex: $0.zIndex) }
        let windowInventoryChanged = previousWindows != nil && windows != previousWindows
        let frontmostChanged = previousWindows != nil && previousFrontmost != frontmostPID
        // Only global uncertainty resets the registry generation. Window
        // inventory and active-layer changes are local evidence.
        if dirty || registrationFailed || previousProcess != nil && !continuityVerified
            && (observer != nil || registry.count > 0) {
            invalidate()
        }
        previousProcess = process
        observerReason = nil
        registrationFailed = false
        registry.beginRead()
        ensureObserver()
        guard observerRootAvailable else {
            return unavailable(reason: observerReason ?? "AX application observer root unavailable",
                privacyRejection: .observerRootUnavailable, wallStarted: wallStarted, start: start)
        }
        let beforeNotifications = observerRevision
        let observerHealthyAtTraversalStart = observerRootAvailable
        let createdEventsForTraversal = takePendingWindowCreatedDiagnostics(through: beforeNotifications)
        localNotificationDuringRead = false
        globalNotificationDuringRead = false
        traversalActive = true
        let windowBefore = windowReader.begin(app: app, request: request, process: process, deadline: readDeadline)
        let focusBefore = focusReader.begin(app: app, request: request, process: process, deadline: readDeadline)
        let reader = AXReader(app: app, request: request, identityProvider: { [self] in
            self.identityAssignment($0, role: $1, subrole: $2, frame: $3, window: $4,
                ownerWindowRoot: $5)
        }, deadline: readDeadline, menuRegistry: menuRegistry)
        var read = reader.read(frontmostPID: frontmostPID)
        let rawTraversalComplete = read.structure.complete
        // The initial focus sample is earlier than AXReader construction.
        read = StructureRead(structure: read.structure, started: start, finished: read.finished,
                             secureFrames: read.secureFrames, visiblePrivacy: read.visiblePrivacy,
                             privacyRejection: read.privacyRejection)
        let otherWindowProofs = menuRegistry.prepareOtherWindowProofs(reader: reader, structure: read.structure, process: process)
        let focusContext = focusReader.project(before: focusBefore, reader: reader, structure: read.structure, process: process)
        let windowProjection = windowReader.project(before: windowBefore, reader: reader,
                                                     read: read, process: process)
        let windowContext = windowProjection?.context
        _ = CFRunLoopRunInMode(.defaultMode, 0, true)
        let finalProcess = ProcessStamp.read(pid: app.pid)
        let finalWindows = targetWindows(pid: app.pid).map {
            AXWindowContext(id: $0.id, frame: $0.frame, zIndex: $0.zIndex)
        }
        traversalActive = false
        guard axPostReadContextMatches(expectedProcess: boundProcess, actualProcess: finalProcess,
                                       before: windows, after: finalWindows) else {
            menuRegistry.rejectOtherWindowProofPostcheck(otherWindowProofs)
            recordWindowCreatedDiagnostics(
                createdEventsForTraversal,
                reader: reader,
                rawTraversalComplete: rawTraversalComplete,
                observerHealthyAtTraversalStart: observerHealthyAtTraversalStart,
                observerRevisionBefore: beforeNotifications,
                observerRevisionStable: beforeNotifications == observerRevision,
                observerAvailableAfterTraversal: observerRootAvailable,
                observerContinuityVerifiedAfterTraversal: false,
                currentVerifiedSelectedRoot: nil)
            let reason = finalProcess == boundProcess
                ? "AX target window identity or geometry changed during traversal"
                : "AX target process changed or became unavailable during traversal"
            let rejection: ReadPrivacyRejection = finalProcess == nil || finalProcess != boundProcess
                ? .processUnavailable : .scopeBoundaryChanged
            return unavailable(reason: reason,
                menuReason: finalProcess == boundProcess ? .traversalWindowsChanged : .processChanged,
                privacyRejection: rejection, wallStarted: wallStarted, start: start)
        }
        let observerAvailable = observerRootAvailable
        let observerStableDuringRead = beforeNotifications == observerRevision
        guard observerAvailable, reader.rootIdentityVerified, !dirty,
              !globalNotificationDuringRead else {
            menuRegistry.rejectOtherWindowProofPostcheck(otherWindowProofs)
            recordWindowCreatedDiagnostics(
                createdEventsForTraversal, reader: reader,
                rawTraversalComplete: rawTraversalComplete,
                observerHealthyAtTraversalStart: observerHealthyAtTraversalStart,
                observerRevisionBefore: beforeNotifications,
                observerRevisionStable: observerStableDuringRead,
                observerAvailableAfterTraversal: observerAvailable,
                observerContinuityVerifiedAfterTraversal: false,
                currentVerifiedSelectedRoot: nil)
            let reason = !observerAvailable
                ? observerReason ?? "AX application observer root became unavailable"
                : "AX target root inventory continuity could not be established"
            let rejection: ReadPrivacyRejection = !observerAvailable
                ? .observerRootUnavailable
                : globalNotificationDuringRead ? .notificationDuringRead : .rootContinuityChanged
            return unavailable(reason: reason, privacyRejection: rejection, wallStarted: wallStarted, start: start)
        }
        let tree = zip(read.structure.objects, reader.observedElements).map {
            TreeContext($0.0, observed: $0.1)
        }
        let content = read.structure.objects.map(ContentContext.init)
        let treeStable = unchangedAXTreeBaseline(previous: previousTree, previousLayer: previousLayer,
                                                 current: tree, currentLayer: read.structure.active_layer)
        let contentStable = previousContent == nil || previousContent == content
        let localEventDuringRead = localNotificationDuringRead || !observerStableDuringRead
        if localEventDuringRead {
            read.visiblePrivacy = nil
            read.privacyRejection = .notificationDuringRead
            read.structure.complete = false
            read.structure.incomplete_reason = read.structure.incomplete_reason
                ?? "a mapped AX object changed during traversal"
            read.structure.objects.removeAll(keepingCapacity: false)
            read.structure.table_viewports.removeAll(keepingCapacity: false)
            read.structure.menu_anchors = []; read.structure.menu_context = nil
            read.structure.focus_context = nil; read.structure.window_context = nil
        }
        if !read.structure.complete {
            menuRegistry.rejectOtherWindowProofPostcheck(otherWindowProofs)
            read.structure.menu_anchors = []; read.structure.menu_context = nil
            read.structure.focus_context = nil; read.structure.window_context = nil
            if let windowContext,
               let window = read.structure.windows.first(where: { $0.window_id == windowContext.origin_window_id }),
               captureReadPrivacyEligible(read, window: window) {
                read.structure.window_context = windowContext
            }
        } else {
            if previousTree != nil && (!treeStable || !contentStable || windowInventoryChanged || frontmostChanged) {
                treeRevision += 1
            }
            previousTree = tree
            previousContent = content
            previousLayer = read.structure.active_layer
            previousWindows = windows
            previousFrontmost = frontmostPID
            registry.prune(to: reader.observedElements.map(\.element))
            contentDirty = false
            read.structure.focus_context = focusContext
            read.structure.window_context = windowContext
            menuRegistry.retainCompleteReadDiagnostics(reader: reader, structure: read.structure)
            menuRegistry.commitOtherWindowProofs(otherWindowProofs, reader: reader, structure: read.structure)
            previousRootProof = AXRootWindowProof(windowContext)
            previousSelectedRoot = windowProjection?.selectedRoot
        }
        continuityVerified = true
        read.structure.observer_coverage = ObserverCoverage.project(
            objects: read.structure.objects, rootAvailable: observerAvailable,
            failures: observerCoverageFailures(for: reader, objects: read.structure.objects))
        recordWindowCreatedDiagnostics(
            createdEventsForTraversal,
            reader: reader,
            rawTraversalComplete: rawTraversalComplete,
            observerHealthyAtTraversalStart: observerHealthyAtTraversalStart,
            observerRevisionBefore: beforeNotifications,
            observerRevisionStable: observerStableDuringRead,
            observerAvailableAfterTraversal: observerAvailable,
            observerContinuityVerifiedAfterTraversal: continuityVerified,
            currentVerifiedSelectedRoot: read.structure.complete ? windowProjection?.selectedRoot : nil)
        let finishedNs = uptimeNs()
        let wallFinished = unixMilliseconds()
        let identityBoundary = registry.identityBoundary(globalRootVerified: continuityVerified)
        let captureRootContinuous = observeCaptureRootContinuity(
            selectedRoot: windowProjection?.selectedRoot,
            qualifies: read.structure.window_context != nil,
            baseline: &captureRootBaseline,
            replacementLatched: &captureRootReplacementLatched)
        read = finishAXSessionStructureRead(read, finishedNs: finishedNs)
        return AXSessionRead(read: read, tree_revision: treeRevision, observer_revision: observerRevision,
                             observer_available: observerAvailable, observer_reason: observerReason,
                             started_unix_ms: wallStarted, finished_unix_ms: wallFinished,
                             identity_boundary: identityBoundary,
                             owned_document_evidence: nil,
                             captureRootContinuous: captureRootContinuous)
    }

    private func readBackgroundAXOnly(wallStarted: UInt64, start: UInt64) -> AXSessionRead {
        guard request.background_target != nil, ownedDocumentRegistration == nil else {
            return unavailable(reason: "background AX-only scope is unavailable",
                privacyRejection: .readUnavailable, wallStarted: wallStarted, start: start)
        }
        _ = CFRunLoopRunInMode(.defaultMode, 0, true)
        guard let process = ProcessStamp.read(pid: app.pid) else {
            return unavailable(reason: "background AX target process identity unavailable",
                menuReason: .processUnavailable, privacyRejection: .processUnavailable,
                wallStarted: wallStarted, start: start)
        }
        guard let boundProcess, process == boundProcess else {
            return unavailable(reason: "background AX target process changed; a new watch is required",
                menuReason: .processChanged, privacyRejection: .processUnavailable,
                wallStarted: wallStarted, start: start)
        }
        guard let targetReader = backgroundWindowReader else {
            return unavailable(reason: "background AX target process/window registration is invalid",
                privacyRejection: .scopeBoundaryChanged, wallStarted: wallStarted, start: start)
        }
        let targetBefore = targetReader.read(deadline: readDeadline)
        guard targetBefore.status == .visible, let beforeContext = targetBefore.context else {
            return unavailable(reason: "background AX target context unavailable: \(targetBefore.failure?.rawValue ?? targetBefore.status.rawValue)",
                privacyRejection: .scopeBoundaryChanged, wallStarted: wallStarted, start: start)
        }
        if dirty || registrationFailed || previousProcess != nil && !continuityVerified
            && (observer != nil || registry.count > 0) {
            invalidate()
        }
        previousProcess = process
        observerReason = nil
        registrationFailed = false
        registry.beginRead()
        ensureObserver()
        guard observerRootAvailable else {
            return unavailable(reason: observerReason ?? "background AX application observer root unavailable",
                privacyRejection: .observerRootUnavailable, wallStarted: wallStarted, start: start)
        }
        let beforeNotifications = observerRevision
        localNotificationDuringRead = false
        globalNotificationDuringRead = false
        traversalActive = true
        let reader = AXReader(app: app, request: request, identityProvider: { [self] in
            self.identityAssignment($0, role: $1, subrole: $2, frame: $3, window: $4,
                ownerWindowRoot: $5)
        }, deadline: readDeadline, menuRegistry: menuRegistry)
        var read = reader.read(frontmostPID: nil)
        read = StructureRead(structure: read.structure, started: start, finished: read.finished,
            secureFrames: read.secureFrames, visiblePrivacy: read.visiblePrivacy,
            privacyRejection: read.privacyRejection)
        let actionNamesComplete = captureActualAXActionNames(reader: reader, structure: &read.structure)
        let targetAfter = targetReader.read(deadline: readDeadline)
        let finalProcess = ProcessStamp.read(pid: app.pid)
        traversalActive = false
        let notificationsStable = beforeNotifications == observerRevision
        let observerAvailable = observerRootAvailable
        guard finalProcess == boundProcess,
              targetAfter.status == .visible, let afterContext = targetAfter.context,
              beforeContext == afterContext,
              reader.actualAXWindowRoots.count == 1,
              targetReader.matchesRetainedRoot(reader.actualAXWindowRoots[0].element),
              reader.rootIdentityVerified,
              observerAvailable, !dirty, !globalNotificationDuringRead,
              !localNotificationDuringRead, notificationsStable,
              uptimeNs() < readDeadline else {
            let reason = finalProcess != boundProcess
                ? "background AX target process changed during traversal"
                : targetAfter.failure.map { "background AX target changed during traversal: \($0.rawValue)" }
                    ?? "background AX root, observer or read boundary changed during traversal"
            return unavailable(reason: reason, privacyRejection: .scopeBoundaryChanged,
                wallStarted: wallStarted, start: start)
        }
        guard !reader.modalFound, !reader.unsupportedModalFound,
              !reader.transientMenuFound, read.structure.menu_context == nil,
              read.structure.menu_anchors.isEmpty,
              read.secureFrames.isEmpty,
              !read.structure.objects.contains(where: { $0.secure }) else {
            return unavailable(reason: "background AX tree contains a modal, menu or sensitive boundary",
                privacyRejection: .modalOrMenuUnresolved, wallStarted: wallStarted, start: start)
        }
        guard actionNamesComplete else {
            return unavailable(reason: "background AX action-name inventory is incomplete",
                privacyRejection: .scopeBoundaryChanged, wallStarted: wallStarted, start: start)
        }
        let targetWindowMatches = read.structure.windows.count == 1
            && read.structure.windows[0].window_id == beforeContext.window_id
            && read.structure.windows[0].frame == beforeContext.frame
            && read.structure.windows[0].kind == "window"
            && !read.structure.windows[0].modal
            && read.structure.windows[0].owner_bundle_id == app.bundle_id
        let targetContext = read.structure.complete && targetWindowMatches
            && read.structure.active_layer == nil ? afterContext : nil
        if targetContext == nil {
            read.structure.complete = false
            read.structure.incomplete_reason = read.structure.incomplete_reason
                ?? "background AX target window did not match the complete tree"
            read.structure.active_layer = nil
        }
        let tree = zip(read.structure.objects, reader.observedElements).map {
            TreeContext($0.0, observed: $0.1)
        }
        let content = read.structure.objects.map(ContentContext.init)
        let treeStable = unchangedAXTreeBaseline(previous: previousTree, previousLayer: nil,
            current: tree, currentLayer: nil)
        let contentStable = previousContent == nil || previousContent == content
        if read.structure.complete {
            if previousTree != nil && (!treeStable || !contentStable) { treeRevision += 1 }
            previousTree = tree
            previousContent = content
            previousLayer = nil
            registry.prune(to: reader.observedElements.map(\.element))
            contentDirty = false
        }
        continuityVerified = true
        read.structure.observer_coverage = ObserverCoverage.project(objects: read.structure.objects,
            rootAvailable: observerAvailable,
            failures: observerCoverageFailures(for: reader, objects: read.structure.objects))
        let finishedNs = uptimeNs()
        let finishedWall = unixMilliseconds()
        let identityBoundary = registry.identityBoundary(globalRootVerified: true)
        read = finishAXSessionStructureRead(read, finishedNs: finishedNs)
        return AXSessionRead(read: read, tree_revision: treeRevision,
            observer_revision: observerRevision, observer_available: observerAvailable,
            observer_reason: observerReason, started_unix_ms: wallStarted,
            finished_unix_ms: finishedWall, identity_boundary: identityBoundary,
            backgroundWindowContext: targetContext)
    }

    private func captureActualAXActionNames(reader: AXReader, structure: inout Structure) -> Bool {
        guard reader.observedElements.count == structure.objects.count else { return false }
        for index in reader.observedElements.indices {
            let element = reader.observedElements[index].element
            guard uptimeNs() < readDeadline, reader.beforeQuery(element),
                  uptimeNs() < readDeadline else { return false }
            var rawNames: CFArray?
            let result = AXUIElementCopyActionNames(element, &rawNames)
            guard uptimeNs() < readDeadline, result == .success, let rawNames,
                  CFGetTypeID(rawNames) == CFArrayGetTypeID(),
                  let names = rawNames as? [String], validBackgroundAXActionNames(names) else { return false }
            structure.objects[index].ax_action_names = names
        }
        return uptimeNs() < readDeadline
    }

    func unavailable(reason: String, menuReason: MenuAnchorDiagnosticCondition = .readUnavailable,
                     privacyRejection: ReadPrivacyRejection = .readUnavailable,
                     wallStarted: UInt64, start: UInt64) -> AXSessionRead {
        menuRegistry.invalidateAll(reason: menuReason)
        invalidate()
        continuityVerified = false
        var structure = Structure(app: app)
        structure.incomplete_reason = reason
        return AXSessionRead(read: StructureRead(structure: structure, started: start, finished: uptimeNs(), secureFrames: [],
                                                privacyRejection: privacyRejection),
                             tree_revision: treeRevision, observer_revision: observerRevision,
                             observer_available: false, observer_reason: reason,
                             started_unix_ms: wallStarted, finished_unix_ms: unixMilliseconds())
    }

    func close() {
        menuRegistry.invalidateAll(reason: .sessionClosed)
        disposeObserver()
        registry.invalidate()
        previousSelectedRoot = nil
        captureRootBaseline = nil
        captureRootReplacementLatched = false
        pendingWindowCreatedDiagnostics.removeAll(keepingCapacity: false)
    }
}
