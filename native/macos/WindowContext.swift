import Foundation
import ApplicationServices

/// Readonly root facts. This carries no focused-control or input permission.
struct WindowContext: Encodable, Sendable {
    let source_session_id: String
    let session_epoch: UInt64
    let process: KernelProcessIdentity
    let origin_window_id: String
    let origin_window_frame: Rect
    var verified_source_sequence: UInt64
    var verified_observed_ms: UInt64
}

struct WindowReadStart {
    let application: MenuRetainedToken
    let window: MenuRetainedToken
    let frame: Rect
    let process: ProcessStamp
}

struct WindowContextProjection {
    let context: WindowContext
    let selectedRoot: MenuRetainedToken?
}

func sameWindowBoundary(_ before: WindowReadStart, _ after: WindowReadStart) -> Bool {
    before.process == after.process && before.frame == after.frame &&
        before.application.matches(after.application.element) && before.window.matches(after.window.element)
}

func windowReadEligible(_ structure: Structure, origin: String) -> Bool {
    focusReadEligible(structure, origin: origin) && !structure.objects.contains(where: { $0.secure })
}

func windowContextReadEligible(_ read: StructureRead, origin: String, window: WindowInfo,
                               process: ProcessStamp) -> Bool {
    let structure = read.structure
    if structure.complete {
        return windowReadEligible(structure, origin: origin)
    }
    guard read.visiblePrivacy?.processStamp == process,
          captureReadPrivacyEligible(read, window: window),
          structure.active_layer == origin, structure.menu_context == nil,
          !structure.objects.contains(where: { $0.secure }),
          !structure.objects.contains(where: {
              [kAXMenuRole, kAXMenuItemRole, kAXMenuBarRole, kAXSheetRole, "AXDialog", "AXPopover"].contains($0.role)
          }) else { return false }
    return true
}

/// Optional queries do not mutate the traversal's completeness or repair an
/// absent fact. All references and RPCs stay on the AXSession worker thread.
private struct WindowAXQueries {
    let deadline: UInt64
    func configure(_ element: AXUIElement) -> Bool {
        let now = uptimeNs()
        guard now < deadline else { return false }
        return AXUIElementSetMessagingTimeout(element, Float(min(0.04, Double(deadline - now) / 1_000_000_000))) == .success && uptimeNs() < deadline
    }
    func value(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        guard configure(element) else { return nil }
        var raw: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &raw) == .success,
              uptimeNs() < deadline else { return nil }
        return raw
    }
    func element(_ owner: AXUIElement, _ name: String) -> AXUIElement? {
        guard let raw = value(owner, name), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        return unsafeDowncast(raw, to: AXUIElement.self)
    }
    func samePID(_ element: AXUIElement, _ expected: UInt32) -> Bool {
        guard configure(element) else { return false }
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success && pid > 0 && UInt32(pid) == expected && uptimeNs() < deadline
    }
    func text(_ element: AXUIElement, _ name: String) -> String? {
        guard let raw = value(element, name), CFGetTypeID(raw) == CFStringGetTypeID() else { return nil }
        return raw as? String
    }
    func frame(_ element: AXUIElement) -> Rect? {
        guard let p = value(element, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = value(element, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(p, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(s, to: AXValue.self), .cgSize, &size) else { return nil }
        let result = Rect(CGRect(origin: point, size: size))
        return result.valid && result.width > 0 && result.height > 0 ? result : nil
    }
    func uniqueWindow(_ window: AXUIElement, application: AXUIElement) -> Bool {
        guard let parent = element(window, kAXParentAttribute), CFEqual(parent, application), configure(application) else { return false }
        var count = 0
        guard AXUIElementGetAttributeValueCount(application, kAXWindowsAttribute as CFString, &count) == .success,
              count > 0, count <= 32, configure(application) else { return false }
        var raw: CFArray?
        guard AXUIElementCopyAttributeValues(application, kAXWindowsAttribute as CFString, 0, count, &raw) == .success,
              let windows = raw as? [AXUIElement], windows.count == count, uptimeNs() < deadline else { return false }
        return windows.filter { CFEqual($0, window) }.count == 1
    }
}

final class WindowContextReader {
    let sessionID: String
    init(sessionID: String) { self.sessionID = sessionID }

    func begin(app: AppIdentity, request: NativeRequest, process: ProcessStamp, deadline: UInt64) -> WindowReadStart? {
        guard focusOriginWindow(request: request, app: app) != nil,
              process.seconds > 0, process.micros < 1_000_000, ProcessStamp.read(pid: app.pid) == process else { return nil }
        let query = WindowAXQueries(deadline: deadline)
        let system = AXUIElementCreateSystemWide(), application = AXUIElementCreateApplication(pid_t(app.pid))
        guard let active = query.element(system, kAXFocusedApplicationAttribute), CFEqual(active, application),
              query.samePID(active, app.pid), query.text(active, kAXRoleAttribute) == kAXApplicationRole,
              let window = query.element(active, kAXFocusedWindowAttribute), query.samePID(window, app.pid),
              query.text(window, kAXRoleAttribute) == kAXWindowRole,
              query.text(window, kAXSubroleAttribute) == kAXStandardWindowSubrole,
              strictFocusBoolean(query.value(window, kAXModalAttribute)) == false,
              query.uniqueWindow(window, application: active), let frame = query.frame(window),
              ProcessStamp.read(pid: app.pid) == process, uptimeNs() < deadline else { return nil }
        return WindowReadStart(application: MenuRetainedToken(active), window: MenuRetainedToken(window), frame: frame, process: process)
    }

    func project(before: WindowReadStart?, reader: AXReader, read: StructureRead,
                 process: ProcessStamp) -> WindowContextProjection? {
        let structure = read.structure
        guard let before, before.process == process,
              structure.app.pid == reader.app.pid, structure.app.bundle_id == reader.app.bundle_id,
              let origin = focusOriginWindow(request: reader.request, app: reader.app),
              structure.windows.filter({ $0.window_id == origin }).count == 1,
              let window = structure.windows.first(where: { $0.window_id == origin }),
              windowContextReadEligible(read, origin: origin, window: window, process: process),
              reader.secureFrames.isEmpty, let epoch = reader.request.session_epoch, epoch > 0,
              !window.modal, window.kind == "window",
              window.frame == before.frame, window.owner_bundle_id == reader.app.bundle_id,
              let rootID = uniqueFocusProjection(before.window.element, observed: reader.observedElements.map { ($0.element, $0.id) }),
              structure.objects.filter({ $0.window_id == origin && $0.parent == nil }).count == 1,
              structure.objects.filter({ $0.instance_ref == rootID }).count == 1,
              let root = structure.objects.first(where: { $0.instance_ref == rootID }), root.parent == nil,
              root.role == kAXWindowRole, root.subrole == kAXStandardWindowSubrole,
              root.window_id == origin, root.frame == window.frame, root.in_view, !root.secure,
              let after = begin(app: reader.app, request: reader.request, process: process, deadline: reader.deadline),
              sameWindowBoundary(before, after) else { return nil }
        let matchingActualRoots = reader.actualAXWindowRoots.filter { $0.matches(after.window.element) }
        let selectedRoot = matchingActualRoots.count == 1 ? after.window : nil
        return WindowContextProjection(context: WindowContext(source_session_id: sessionID, session_epoch: epoch,
            process: KernelProcessIdentity(pid: reader.app.pid, start_seconds: process.seconds, start_micros: process.micros),
            origin_window_id: origin, origin_window_frame: window.frame,
            verified_source_sequence: 0, verified_observed_ms: 0), selectedRoot: selectedRoot)
    }
}

func runWindowContextTests() throws -> [String] {
    let frame = Rect(CGRect(x: 20, y: 30, width: 160, height: 100))
    let process = ProcessStamp(seconds: 100, micros: 200)
    let start = WindowReadStart(application: MenuRetainedToken("app" as CFString),
        window: MenuRetainedToken("root" as CFString), frame: frame, process: process)
    try requireTest(sameWindowBoundary(start, start), "stable actual normal-window boundary matches")
    for change in ["application", "window", "frame", "kernel"] {
        let after = WindowReadStart(application: MenuRetainedToken((change == "application" ? "other-app" : "app") as CFString),
            window: MenuRetainedToken((change == "window" ? "same-frame-replacement" : "root") as CFString),
            frame: change == "frame" ? Rect(CGRect(x: 21, y: 30, width: 160, height: 100)) : frame,
            process: change == "kernel" ? ProcessStamp(seconds: 101, micros: 200) : process)
        try requireTest(!sameWindowBoundary(start, after), "normal-window read rejects changed \(change)")
    }
    let app = AppIdentity(bundle_id: "com.lingxi.computer-user.guard-fixture", name: "Offline", pid: 42)
    var structure = Structure(app: app)
    structure.complete = true; structure.incomplete_reason = nil; structure.active_layer = "window-1"
    structure.windows = [WindowInfo(window_id: "window-1", title: "Offline", frame: frame, z_index: 0,
        kind: "window", modal: false, owner_bundle_id: app.bundle_id)]
    structure.objects = [UiObject(instance_ref: "root", parent: nil, window_id: "window-1", role: kAXWindowRole,
        subrole: kAXStandardWindowSubrole, title: "Offline", description: nil, value: nil,
        enabled: true, focused: false, frame: frame, actions: [], in_view: true, group: nil, secure: false)]
    try requireTest(windowReadEligible(structure, origin: "window-1") && structure.focus_context == nil,
        "normal window facts do not require a focused element")
    for change in ["partial", "background", "secure", "menu", "sheet"] {
        var other = structure
        switch change {
        case "partial": other.complete = false
        case "background": other.active_layer = nil
        default:
            other.objects.append(UiObject(instance_ref: "transient", parent: "root", window_id: "window-1",
                role: change == "menu" ? kAXMenuRole : change == "sheet" ? kAXSheetRole : kAXTextFieldRole,
                subrole: nil, title: nil, description: nil, value: nil, enabled: true, focused: false,
                frame: frame, actions: [], in_view: true, group: nil, secure: change == "secure"))
        }
        try requireTest(!windowReadEligible(other, origin: "window-1"), "normal-window facts cannot repair \(change)")
    }
    structure.window_context = WindowContext(source_session_id: "offline-window-session", session_epoch: 7,
        process: KernelProcessIdentity(pid: 42, start_seconds: 100, start_micros: 200),
        origin_window_id: "window-1", origin_window_frame: frame, verified_source_sequence: 0, verified_observed_ms: 0)
    structure.normalizeSourceProvenance(sequence: 9, observedMS: 10)
    try requireTest(structure.window_context?.verified_source_sequence == 9 &&
        structure.window_context?.verified_observed_ms == 10 && structure.window_context?.session_epoch == 7 &&
        structure.focus_context == nil, "window publication stamps actual root facts without inventing focus or epoch")

    let windowID = "window-1"
    let appIdentity = AppIdentity(bundle_id: app.bundle_id, name: app.name, pid: app.pid)
    let privacyWindow = WindowInfo(window_id: windowID, title: "Offline", frame: frame,
        z_index: 0, kind: "window", modal: false, owner_bundle_id: app.bundle_id)
    let privacyScope = TableScopeSearchRequest(
        scroll_path: [TableScopeSearchStep(role: "ax_scroll_area", label: "Workspace")],
        column_headers: ["Name"])
    let privacyScrollFrame = Rect(CGRect(x: frame.x + 10, y: frame.y + 10,
        width: frame.width - 20, height: frame.height - 20))
    func privacyObject(_ ref: String, _ parent: String?, _ role: String,
                       _ subrole: String?, _ objectFrame: Rect) -> UiObject {
        UiObject(instance_ref: ref, parent: parent, window_id: windowID, role: role,
            subrole: subrole, title: nil, description: nil, value: nil,
            enabled: true, focused: false, frame: objectFrame, actions: [],
            in_view: true, group: nil, secure: false, identity_continuity: .notificationCovered)
    }
    let privacyPath = TableScopeSearchPathItem(instance_ref: "privacy-scroll", label: "Workspace", frame: privacyScrollFrame)
    let privacyCandidate = TableScopeSearchCandidate(table_instance_ref: "privacy-table",
        scroll_container_instance_ref: "privacy-scroll", scroll_path: [privacyPath], column_headers: ["Name"])
    var partialStructure = Structure(app: appIdentity)
    partialStructure.windows = [privacyWindow]
    partialStructure.active_layer = windowID
    partialStructure.complete = false
    partialStructure.incomplete_reason = "AX table row subtree omitted for scoped census"
    partialStructure.observer_coverage = ObserverCoverage(state: .degraded, covered_objects: 4,
        read_scoped_objects: 0, failed_registrations: 0, failures: [])
    partialStructure.objects = [
        privacyObject("privacy-root", nil, kAXWindowRole, kAXStandardWindowSubrole, frame),
        privacyObject("privacy-scroll", "privacy-root", kAXScrollAreaRole, nil, privacyScrollFrame),
        privacyObject("privacy-table", "privacy-scroll", kAXTableRole, nil, privacyScrollFrame),
        privacyObject("privacy-column", "privacy-table", kAXColumnRole, nil, privacyScrollFrame),
    ]
    partialStructure.table_scope_search = TableScopeSearchResult(requested_scope: privacyScope,
        process_id: app.pid, window_id: windowID, state: .complete, candidates: [privacyCandidate])
    partialStructure.table_viewports = [TableViewport(table_instance_ref: "privacy-table",
        scroll_container_instance_ref: "privacy-scroll", process_id: app.pid, table_frame: privacyScrollFrame,
        viewport_frame: privacyScrollFrame, ax_enabled: .enabled, visible_rows_complete: true,
        visible_rows: [], columns_complete: true,
        columns: [TableViewportColumn(instance_ref: "privacy-column", parent_instance_ref: "privacy-table",
            frame: privacyScrollFrame, ax_index: 0, header_instance_ref: nil, header: "Name")])]
    let privacyFingerprint = [
        VisiblePrivacyNodeFact(parentIndex: nil, childIndex: nil, role: kAXWindowRole,
            subrole: kAXStandardWindowSubrole, frame: frame, childrenSource: .visibleChildren, childCount: 1),
        VisiblePrivacyNodeFact(parentIndex: 0, childIndex: 0, role: kAXScrollAreaRole,
            subrole: nil, frame: privacyScrollFrame, childrenSource: .visibleChildren, childCount: 1),
        VisiblePrivacyNodeFact(parentIndex: 1, childIndex: 0, role: kAXTableRole,
            subrole: nil, frame: privacyScrollFrame, childrenSource: .visibleChildren, childCount: 1),
        VisiblePrivacyNodeFact(parentIndex: 2, childIndex: 0, role: kAXColumnRole,
            subrole: nil, frame: privacyScrollFrame, childrenSource: .visibleChildren, childCount: 0),
    ]
    let privacyCoverage = VisiblePrivacyCoverage(bundleID: app.bundle_id, processID: app.pid,
        processStamp: process, windowID: windowID, windowFrame: frame, tableScope: privacyScope,
        readStartedNS: 120, readFinishedNS: 200, visibleNodeCount: 4,
        visibleTreeFingerprint: privacyFingerprint,
        tableViewport: VisiblePrivacyTableViewport(viewport: partialStructure.table_viewports[0]))
    let partialRead = StructureRead(structure: partialStructure, started: 100, finished: 300,
        secureFrames: [], visiblePrivacy: privacyCoverage)
    try requireTest(windowContextReadEligible(partialRead, origin: windowID, window: privacyWindow,
                                               process: process),
        "partial row census may carry normal-window metadata only with same-read visible privacy coverage")
    try requireTest(!windowReadEligible(partialStructure, origin: windowID),
        "scoped privacy coverage does not upgrade generic complete-tree eligibility")

    var noPrivacyProof = partialRead
    noPrivacyProof.visiblePrivacy = nil
    try requireTest(!windowContextReadEligible(noPrivacyProof, origin: windowID, window: privacyWindow,
                                                process: process),
        "partial window context rejects a missing visible-tree privacy proof")
    let otherScope = TableScopeSearchRequest(
        scroll_path: [TableScopeSearchStep(role: "ax_scroll_area", label: "Other")],
        column_headers: ["Name"])
    var wrongScope = partialRead
    wrongScope.structure.table_scope_search = TableScopeSearchResult(requested_scope: otherScope,
        process_id: app.pid, window_id: windowID, state: .complete, candidates: [privacyCandidate])
    try requireTest(!windowContextReadEligible(wrongScope, origin: windowID, window: privacyWindow,
                                                process: process),
        "partial window context rejects privacy coverage for a different frozen row scope")
    let staleRead = StructureRead(structure: partialStructure, started: 100, finished: 150,
        secureFrames: [], visiblePrivacy: privacyCoverage)
    try requireTest(!windowContextReadEligible(staleRead, origin: windowID, window: privacyWindow,
                                                process: process),
        "partial window context rejects privacy coverage finished after the Source read")
    let secureRead = StructureRead(structure: partialStructure, started: 100, finished: 300,
        secureFrames: [frame], visiblePrivacy: privacyCoverage)
    try requireTest(!windowContextReadEligible(secureRead, origin: windowID, window: privacyWindow,
                                                process: process),
        "partial window context rejects secure content from the same read")
    var overlayRead = partialRead
    overlayRead.structure.objects = [UiObject(instance_ref: "sheet", parent: nil, window_id: windowID,
        role: kAXSheetRole, subrole: nil, title: nil, description: nil, value: nil,
        enabled: false, focused: false, frame: frame, actions: [], in_view: true, group: nil, secure: false)]
    try requireTest(!windowContextReadEligible(overlayRead, origin: windowID, window: privacyWindow,
                                                process: process),
        "partial window context does not widen sheet or overlay eligibility")
    let wrongGeometry = WindowInfo(window_id: windowID, title: "Offline",
        frame: Rect(CGRect(x: 21, y: 30, width: 160, height: 100)), z_index: 0,
        kind: "window", modal: false, owner_bundle_id: app.bundle_id)
    try requireTest(!windowContextReadEligible(partialRead, origin: windowID, window: wrongGeometry,
                                                process: process),
        "partial window context rejects a changed source window geometry")
    try requireTest(!windowContextReadEligible(partialRead, origin: windowID, window: privacyWindow,
                                                process: ProcessStamp(seconds: 101, micros: 200)),
        "partial window context rejects a proof from another process instance")

    let observe = try JSONDecoder().decode(NativeRequest.self, from: Data("{\"protocol_version\":1,\"command\":\"observe\",\"pid\":42,\"session_epoch\":7,\"text_policy\":{\"kind\":\"registered_editor\",\"window_id\":\"window-1\"}}".utf8))
    try requireTest(focusOriginWindow(request: observe, app: app) == nil, "private one-shot observation has no root context")
    return ["window_context_actual_cf_kernel_root_boundary", "window_context_without_focused_element",
        "window_context_partial_background_menu_secure_rejected", "window_context_nested_provenance_without_focus",
        "window_context_partial_requires_same_read_visible_privacy_scope",
        "window_context_partial_keeps_generic_complete_gate_closed",
        "window_context_private_observe_no_positive_facts"]
}
